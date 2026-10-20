//! DOC — legacy Word 97-2003 documents (`.doc`, an OLE2 compound file).
//!
//! Word 97 and later store the document's characters in the `WordDocument`
//! stream and describe where they live with a *piece table* (the CLX) kept in
//! the `0Table` or `1Table` stream. We read the File Information Block (FIB) at
//! the head of `WordDocument` to locate the CLX, walk the piece table, and
//! concatenate each piece — decoding it as little-endian UTF-16 or, when the
//! piece is flagged "compressed", as the legacy 8-bit Windows-1252 code page
//! (MS-DOC §2.3.1, §2.8.35, §2.9.177). The recovered text is emitted as section
//! records, with the title taken from the `\x05SummaryInformation` property set
//! when present.
//!
//! LIMITS — read honestly. This recovers the main document text of Word 97+
//! files, which is the overwhelming majority of surviving `.doc` attachments.
//! It does NOT recover:
//!   * formatting, styles, or heading structure — the output is plain prose, so
//!     unlike the DOCX extractor there is no `headings` list;
//!   * tables as tables — cell and row marks become line breaks, so a table
//!     reads as text, not as rows;
//!   * headers/footers, footnotes, endnotes, comments and textboxes, which live
//!     in separate ranges past the main `fcMac` and are not followed here;
//!   * Word 6.0/95 (and earlier) documents, which predate this piece-table
//!     layout — those fall back to reading the raw main stream as Windows-1252,
//!     which is lossy, and if even that yields nothing the file is counted as
//!     junk rather than mis-indexed.
//!
//! Field instruction codes (hidden `{ }` field source between the field-begin
//! and field-separator marks) are dropped; the field RESULT text is kept.

use super::{emit_document, ExtractStats, Sink, MAX_RECORDS_PER_FILE};
use anyhow::Result;
use std::io::Read;
use std::path::Path;

/// Largest `WordDocument` / table stream we read. A `.doc`'s text stream is
/// far below this; the cap only bounds what a crafted compound file can cost.
const MAX_STREAM_BYTES: u64 = 256 << 20;

/// Largest property-set stream (`SummaryInformation`) we read for the title.
const MAX_SUMMARY_BYTES: u64 = 1 << 20;

/// Body text cap: past this the document is already far larger than any real
/// `.doc`, and section emission is bounded by [`MAX_RECORDS_PER_FILE`] anyway.
const MAX_BODY_CHARS: usize = 64 << 20;

/// Defensive cap on piece-table entries, against a crafted `lcbClx`.
const MAX_PIECES: usize = 1 << 20;

// --- little-endian field reads, all bounds-checked (never panic on content) --

fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    b.get(off..off + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

pub fn extract(path: &Path, name: &Path, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let f = std::fs::File::open(path)?;
    // A malformed or truncated compound file is reported by `cfb` as an error,
    // never a panic — so every failure below degrades to "junk", and a single
    // unreadable legacy file never aborts a folder scan.
    let mut comp = match cfb::CompoundFile::open(f) {
        Ok(c) => c,
        Err(_) => {
            stats.junk += 1;
            return Ok(stats);
        }
    };

    let Some(word) = read_stream(&mut comp, "WordDocument", MAX_STREAM_BYTES) else {
        stats.junk += 1;
        return Ok(stats);
    };

    let body = recover_text(&mut comp, &word).unwrap_or_default();
    let body = body.trim();
    if body.is_empty() {
        stats.junk += 1;
        return Ok(stats);
    }

    let title = summary_title(&mut comp)
        .filter(|t| !t.trim().is_empty())
        .or_else(|| {
            body.lines()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim().chars().take(200).collect())
        })
        .unwrap_or_else(|| {
            name.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "untitled".into())
        });

    emit_document(&title, &[], body, MAX_RECORDS_PER_FILE, sink, &mut stats);
    Ok(stats)
}

/// Read a named stream (bounded) into memory, or `None` if it is absent or
/// unreadable.
fn read_stream<F: Read + std::io::Seek>(
    comp: &mut cfb::CompoundFile<F>,
    name: &str,
    cap: u64,
) -> Option<Vec<u8>> {
    if !comp.is_stream(name) {
        return None;
    }
    let stream = comp.open_stream(name).ok()?;
    let mut buf = Vec::new();
    stream.take(cap).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Recover the main document text via the FIB piece table. Returns `None` only
/// when the FIB is too short to parse; a FIB with no usable CLX falls back to
/// the raw-range reader (`fallback_text`).
fn recover_text<F: Read + std::io::Seek>(
    comp: &mut cfb::CompoundFile<F>,
    word: &[u8],
) -> Option<String> {
    // FIB.base.flags1 (offset 0x0A): bit 0x0200 (fWhichTblStm) selects the
    // table stream. fcClx / lcbClx sit at the fixed Word97 offsets 0x01A2 /
    // 0x01A6 within the FIB (MS-DOC §2.5.6 FibRgFcLcb97 — the 67th/68th FC/LCB
    // slot).
    let flags1 = u16_at(word, 0x0A)?;
    let table_name = if flags1 & 0x0200 != 0 {
        "1Table"
    } else {
        "0Table"
    };
    let fc_clx = u32_at(word, 0x01A2)? as usize;
    let lcb_clx = u32_at(word, 0x01A6)? as usize;

    if lcb_clx > 0 {
        // The table stream named by the flag, falling back to the other (some
        // writers disagree with the flag).
        let table = read_stream(comp, table_name, MAX_STREAM_BYTES)
            .or_else(|| {
                let other = if table_name == "1Table" {
                    "0Table"
                } else {
                    "1Table"
                };
                read_stream(comp, other, MAX_STREAM_BYTES)
            })
            .unwrap_or_default();
        if let Some(text) = text_from_piece_table(word, &table, fc_clx, lcb_clx) {
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }

    // No piece table (older Word, or an unreadable CLX): read the raw main
    // range as Windows-1252. fcMin/fcMac are at FIB offsets 0x0018 / 0x001C.
    Some(fallback_text(word))
}

/// Walk the CLX in `table[fc_clx .. fc_clx+lcb_clx]`, then materialise each
/// piece's characters out of `word`.
fn text_from_piece_table(
    word: &[u8],
    table: &[u8],
    fc_clx: usize,
    lcb_clx: usize,
) -> Option<String> {
    let clx = table.get(fc_clx..fc_clx.checked_add(lcb_clx)?)?;

    // The CLX is zero or more Prc blocks (clxt 0x01: a u16 byte count then that
    // many bytes) followed by exactly one Pcdt (clxt 0x02: a u32 byte count then
    // the PlcPcd). Walk to the Pcdt.
    let mut i = 0usize;
    let plc = loop {
        match clx.get(i)? {
            0x01 => {
                let cb = u16_at(clx, i + 1)? as usize;
                i = i.checked_add(3)?.checked_add(cb)?;
            }
            0x02 => {
                let lcb = u32_at(clx, i + 1)? as usize;
                let start = i.checked_add(5)?;
                break clx.get(start..start.checked_add(lcb)?)?;
            }
            _ => return None,
        }
    };

    // PlcPcd: (n+1) CPs (u32, 4 bytes) then n PCDs (8 bytes), so the total is
    // 4·(n+1) + 8·n = 4 + 12·n bytes, i.e. n = (len - 4) / 12.
    if plc.len() < 4 {
        return None;
    }
    let n = (plc.len() - 4) / 12;
    if n == 0 || n > MAX_PIECES {
        return None;
    }
    let pcd_base = (n + 1) * 4;

    let mut out = String::new();
    for p in 0..n {
        let cp_start = u32_at(plc, p * 4)? as usize;
        let cp_end = u32_at(plc, (p + 1) * 4)? as usize;
        let char_count = cp_end.checked_sub(cp_start)?;
        if char_count == 0 {
            continue;
        }
        // PCD: [0..2] flags, [2..6] FcCompressed, [6..8] prm.
        let fc_field = u32_at(plc, pcd_base + p * 8 + 2)?;
        let compressed = fc_field & 0x4000_0000 != 0;
        let fc = (fc_field & 0x3FFF_FFFF) as usize;

        if compressed {
            // 8-bit: the real byte offset is fc/2, one byte per character.
            let start = fc / 2;
            let bytes = word.get(start..start.checked_add(char_count)?)?;
            let (decoded, _, _) = encoding_rs::WINDOWS_1252.decode(bytes);
            out.push_str(&decoded);
        } else {
            // 16-bit little-endian UTF-16, two bytes per character.
            let start = fc;
            let byte_len = char_count.checked_mul(2)?;
            let bytes = word.get(start..start.checked_add(byte_len)?)?;
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            out.push_str(&String::from_utf16_lossy(&units));
        }
        if out.len() >= MAX_BODY_CHARS {
            break;
        }
    }
    Some(clean_text(&out))
}

/// Last-resort reader for pre-piece-table documents: the main text range
/// `[fcMin, fcMac)` of `WordDocument`, decoded as Windows-1252. Lossy by
/// nature; returns an empty string when the range is unusable.
fn fallback_text(word: &[u8]) -> String {
    let (Some(fc_min), Some(fc_mac)) = (u32_at(word, 0x0018), u32_at(word, 0x001C)) else {
        return String::new();
    };
    let (min, mac) = (fc_min as usize, fc_mac as usize);
    if mac <= min {
        return String::new();
    }
    let end = mac.min(min.saturating_add(MAX_BODY_CHARS));
    let Some(bytes) = word.get(min..end.min(word.len())) else {
        return String::new();
    };
    let (decoded, _, _) = encoding_rs::WINDOWS_1252.decode(bytes);
    clean_text(&decoded)
}

/// Turn Word's in-stream control characters into clean prose: paragraph, line,
/// page, cell and row marks become line breaks; field INSTRUCTION text (between
/// field-begin 0x13 and field-separator 0x14) is dropped while the field RESULT
/// is kept; and the remaining sub-0x20 control codes are discarded.
fn clean_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    // Depth of nested field instruction regions we are currently inside.
    let mut field_instr = 0u32;
    for c in s.chars() {
        match c {
            '\u{13}' => field_instr += 1, // field begin → enter instruction
            '\u{14}' => field_instr = field_instr.saturating_sub(1), // separator → result
            '\u{15}' => {}                // field end
            _ if field_instr > 0 => {}    // inside instruction: drop
            // \r (0x0D) is the paragraph mark; 0x0B line break, 0x0C page
            // break, 0x07 cell/row mark — all become line breaks.
            '\r' | '\u{0B}' | '\u{0C}' | '\u{07}' => out.push('\n'),
            '\u{1E}' => out.push('-'), // non-breaking hyphen
            '\u{A0}' => out.push(' '), // non-breaking space
            '\t' | '\n' => out.push(c),
            _ if (c as u32) < 0x20 => {} // other control codes: drop
            _ => out.push(c),
        }
    }
    // Collapse the runs of blank lines the mark-to-newline mapping produces
    // into paragraph breaks, and trim trailing whitespace on each line.
    let mut collapsed = String::with_capacity(out.len());
    let mut blank_run = 0u32;
    for line in out.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank_run += 1;
            continue;
        }
        if !collapsed.is_empty() {
            collapsed.push_str(if blank_run > 0 { "\n\n" } else { "\n" });
        }
        blank_run = 0;
        collapsed.push_str(line);
    }
    collapsed
}

/// Best-effort document title from the `\x05SummaryInformation` property set
/// (MS-OLEPS): property id 2 (PIDSI_TITLE). Returns `None` on any shortfall.
fn summary_title<F: Read + std::io::Seek>(comp: &mut cfb::CompoundFile<F>) -> Option<String> {
    let s = read_stream(comp, "\u{5}SummaryInformation", MAX_SUMMARY_BYTES)?;
    property_string(&s, 2)
}

/// Read a single string property (`pid`) out of the first section of a
/// property-set stream. Handles VT_LPSTR (code-page bytes) and VT_LPWSTR
/// (UTF-16). Everything is bounds-checked; `None` on any malformation.
fn property_string(s: &[u8], pid: u32) -> Option<String> {
    // PropertySetStream header: byteOrder(2) version(2) sysId(4) clsid(16)
    // cSections(4) = 28 bytes, then the first FMTID(16) + section offset(4).
    let section_off = u32_at(s, 44)? as usize;
    let cb_section = u32_at(s, section_off)? as usize;
    let num_props = u32_at(s, section_off + 4)? as usize;
    // The section must fit, and the property index table must fit inside it.
    let section_end = section_off.checked_add(cb_section)?;
    if section_end > s.len() || num_props > 1 << 16 {
        return None;
    }

    // Find the code page (pid 1, VT_I2) for VT_LPSTR decoding; default 1252.
    let mut code_page: u16 = 1252;
    let mut target: Option<usize> = None;
    for i in 0..num_props {
        let entry = section_off + 8 + i * 8;
        let this_pid = u32_at(s, entry)?;
        let value_off = section_off + u32_at(s, entry + 4)? as usize;
        if this_pid == 1 {
            // VT_I2 code page: type(4) then i16.
            if u32_at(s, value_off) == Some(2) {
                if let Some(cp) = u16_at(s, value_off + 4) {
                    code_page = cp;
                }
            }
        }
        if this_pid == pid {
            target = Some(value_off);
        }
    }

    let value_off = target?;
    let vt = u32_at(s, value_off)?;
    let len = u32_at(s, value_off + 4)? as usize;
    let data_off = value_off + 8;
    match vt {
        0x1E => {
            // VT_LPSTR: `len` bytes incl terminating NUL, code-page encoded.
            let bytes = s.get(data_off..data_off.checked_add(len)?)?;
            let bytes = bytes.split(|&b| b == 0).next().unwrap_or(bytes);
            let enc = encoding_rs::Encoding::for_label(format!("windows-{code_page}").as_bytes())
                .unwrap_or(encoding_rs::WINDOWS_1252);
            let (decoded, _, _) = enc.decode(bytes);
            Some(decoded.trim().to_string())
        }
        0x1F => {
            // VT_LPWSTR: `len` UTF-16 code units incl NUL.
            let byte_len = len.checked_mul(2)?;
            let bytes = s.get(data_off..data_off.checked_add(byte_len)?)?;
            let mut units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            if let Some(pos) = units.iter().position(|&u| u == 0) {
                units.truncate(pos);
            }
            Some(String::from_utf16_lossy(&units).trim().to_string())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_maps_marks_and_drops_field_instructions() {
        // A paragraph mark, then an inline field `{ HYPERLINK "x" }result`: the
        // field RESULT ("click here") is kept inline where it sits and the
        // instruction source is dropped.
        let raw = "Hello\u{0D}See \u{13}HYPERLINK \"http://x\"\u{14}click here\u{15}\u{0D}End";
        let cleaned = clean_text(raw);
        assert_eq!(cleaned, "Hello\nSee click here\nEnd");
        assert!(
            !cleaned.contains("HYPERLINK"),
            "field source must be dropped"
        );
    }

    #[test]
    fn clean_text_collapses_blank_runs_and_trims() {
        let raw = "A\u{0D}\u{0D}\u{0D}B  \u{0D}C";
        assert_eq!(clean_text(raw), "A\n\nB\nC");
    }

    #[test]
    fn field_reads_are_bounds_checked() {
        assert_eq!(u16_at(&[1], 0), None);
        assert_eq!(u32_at(&[1, 2, 3], 0), None);
        assert_eq!(u32_at(&[1, 2, 3, 4], 0), Some(0x04030201));
    }

    /// A piece table with one compressed (Windows-1252) and one uncompressed
    /// (UTF-16) piece, assembled by hand, round-trips to the expected text.
    /// This is the core of the extractor and is exercised without needing a
    /// real BIFF file on disk.
    #[test]
    fn piece_table_decodes_compressed_and_uncompressed_pieces() {
        // WordDocument: FIB header padded to 0x0200, then the text region.
        // Piece 0 (compressed, 5 chars "Hello") at byte 0x200.
        // Piece 1 (uncompressed UTF-16, " wörld") right after.
        let mut word = vec![0u8; 0x200];
        let p0_bytes = b"Hello"; // CP1252, 1 byte/char
        let p0_off = 0x200usize;
        word.extend_from_slice(p0_bytes);
        let p1_text = " wörld";
        let p1_off = word.len();
        for u in p1_text.encode_utf16() {
            word.extend_from_slice(&u.to_le_bytes());
        }

        // Build the table stream: CLX = Pcdt (0x02) + lcb + PlcPcd.
        // CPs: [0, 5, 5+6] ; PCDs: piece0 fc=p0_off*2 | compressed, piece1 fc=p1_off.
        let p0_chars = p0_bytes.len() as u32; // 5
        let p1_chars = p1_text.chars().count() as u32; // 6
        let mut plc = Vec::new();
        for cp in [0u32, p0_chars, p0_chars + p1_chars] {
            plc.extend_from_slice(&cp.to_le_bytes());
        }
        // PCD 0: flags(2)=0, fc(4)= (p0_off*2) | 0x40000000, prm(2)=0
        let fc0 = (p0_off as u32 * 2) | 0x4000_0000;
        plc.extend_from_slice(&[0, 0]);
        plc.extend_from_slice(&fc0.to_le_bytes());
        plc.extend_from_slice(&[0, 0]);
        // PCD 1: fc = p1_off (uncompressed)
        plc.extend_from_slice(&[0, 0]);
        plc.extend_from_slice(&(p1_off as u32).to_le_bytes());
        plc.extend_from_slice(&[0, 0]);

        let mut clx = vec![0x02u8];
        clx.extend_from_slice(&(plc.len() as u32).to_le_bytes());
        clx.extend_from_slice(&plc);

        // Table stream: put the CLX at offset 0 so fc_clx = 0.
        let table = clx.clone();
        let text = text_from_piece_table(&word, &table, 0, clx.len()).unwrap();
        assert_eq!(text, "Hello wörld");
    }

    #[test]
    fn property_string_reads_a_vt_lpstr_title() {
        // Minimal SummaryInformation with one section holding pid 2 = "Budget".
        // Layout: 28-byte header, FMTID(16)+sectionOffset(4) at 28..48.
        let title = b"Budget\0";
        let mut s = vec![0u8; 48];
        // byteOrder 0xFFFE, version 0, cSections 1
        s[0] = 0xFE;
        s[1] = 0xFF;
        let section_off = 48u32;
        s[44..48].copy_from_slice(&section_off.to_le_bytes());
        // Section: cbSection, cProperties=1, then (pid=2, off=16), then value.
        // Property table entry sits at section_off+8; value at section_off+16.
        let value_rel = 16u32;
        let mut section = Vec::new();
        // cbSection filled in after; cProperties
        section.extend_from_slice(&[0, 0, 0, 0]); // placeholder cbSection
        section.extend_from_slice(&1u32.to_le_bytes()); // cProperties
        section.extend_from_slice(&2u32.to_le_bytes()); // pid 2
        section.extend_from_slice(&value_rel.to_le_bytes()); // value offset
                                                             // value: VT_LPSTR (0x1E), len, bytes
        section.extend_from_slice(&0x1Eu32.to_le_bytes());
        section.extend_from_slice(&(title.len() as u32).to_le_bytes());
        section.extend_from_slice(title);
        let cb = section.len() as u32;
        section[0..4].copy_from_slice(&cb.to_le_bytes());
        s.extend_from_slice(&section);

        assert_eq!(property_string(&s, 2).as_deref(), Some("Budget"));
        assert_eq!(property_string(&s, 99), None, "absent pid yields None");
    }

    #[test]
    fn malformed_clx_never_panics() {
        // Truncated / nonsense CLX bytes must return None, not panic.
        let word = vec![0u8; 0x400];
        for clx in [
            vec![0x02, 0xFF, 0xFF, 0xFF, 0xFF], // lcb far past end
            vec![0x01, 0x10, 0x00],             // Prc with no body
            vec![0x09],                         // unknown clxt
            vec![],                             // empty
        ] {
            let _ = text_from_piece_table(&word, &clx, 0, clx.len());
        }
    }
}
