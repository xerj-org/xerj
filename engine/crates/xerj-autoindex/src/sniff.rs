//! Content-based format detection. NEVER trusts file extensions.
//!
//! Order: magic bytes → binary check → text heuristics
//! (json/jsonl → html/xml → logs → sql dump → csv → yaml → txt).

use anyhow::Result;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Jsonl,
    Json,
    Csv,
    Logs,
    Xml,
    Html,
    Yaml,
    TxtProse,
    TxtLines,
    Pdf,
    Docx,
    Sqlite,
    SqlDump,
    /// Source code — AST-parsed by the matching tree-sitter grammar.
    Code,
    /// Unity text-serialized asset (scene/prefab/.asset/.mat/.anim/…):
    /// a `%YAML` + `%TAG !u! tag:unity3d.com` multi-document stream.
    UnityYaml,
    /// Unity `.meta` sidecar: plain YAML opening with `fileFormatVersion:`
    /// and carrying the asset `guid` — the join key for everything Unity.
    UnityMeta,
    /// Biovision motion capture: a skeleton HIERARCHY header followed by a
    /// large numeric MOTION block. Indexed as ONE metadata record per file
    /// (joints, frame count, duration) — the motion numbers are never read.
    Bvh,
    /// User-designated existence-only file (`--stub <glob>`): ONE name-card
    /// record, contents never opened. For corpus-specific data blobs the
    /// owner wants referenceable but not parsed — never assigned by
    /// sniffing, only by the CLI flag.
    Stub,
    Binary,
}

impl Family {
    pub fn as_str(&self) -> &'static str {
        match self {
            Family::Jsonl => "jsonl",
            Family::Json => "json",
            Family::Csv => "csv",
            Family::Logs => "logs",
            Family::Xml => "xml",
            Family::Html => "html",
            Family::Yaml => "yaml",
            Family::TxtProse => "txt-prose",
            Family::TxtLines => "txt-lines",
            Family::Pdf => "pdf",
            Family::Docx => "docx",
            Family::Sqlite => "sqlite",
            Family::SqlDump => "sqldump",
            Family::Code => "code",
            Family::UnityYaml => "unity",
            Family::UnityMeta => "unity-meta",
            Family::Bvh => "bvh",
            Family::Stub => "stub",
            Family::Binary => "binary",
        }
    }
    /// Document-family formats produce one record per document/section.
    pub fn is_document(&self) -> bool {
        matches!(self, Family::Pdf | Family::Docx | Family::TxtProse)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CsvDialect {
    pub delim: u8,
    pub has_header: bool,
    pub decimal_comma: bool,
}

#[derive(Debug, Clone)]
pub struct Sniffed {
    pub family: Family,
    pub gzip: bool,
    /// e.g. "png", "zip", "elf", "unknown" — set when family == Binary.
    pub binary_kind: Option<String>,
    pub csv: Option<CsvDialect>,
    /// "utf-8" or "windows-1252 (lossy)"
    pub encoding: &'static str,
    /// File name of the LOGICAL source (`app.py`), even when the sniffed
    /// content lives elsewhere. Durable preparation reads content-addressed
    /// snapshot blobs (`blobs/00000000`), so an extractor that recovers a
    /// parameter from its content path silently loses the name — that is how
    /// #294 turned every code file into junk. Name-derived decisions after
    /// sniffing must use this, never the content path.
    pub logical_name: Option<std::path::PathBuf>,
}

fn read_prefix(path: &Path, gzip: bool, n: usize) -> Result<Vec<u8>> {
    let f = std::fs::File::open(path)?;
    let mut buf = Vec::with_capacity(n.min(1 << 20));
    if gzip {
        let mut r = flate2::read::MultiGzDecoder::new(f).take(n as u64);
        r.read_to_end(&mut buf).ok(); // truncated gz prefix is fine for sniffing
    } else {
        let mut r = f.take(n as u64);
        r.read_to_end(&mut buf)?;
    }
    Ok(buf)
}

pub fn sniff(path: &Path) -> Result<Sniffed> {
    sniff_with_name(path, path)
}

/// Classify bytes from `content_path` while retaining the logical filename
/// signals (currently source-code extensions) from `logical_path`.
///
/// Durable preparation uses this to classify an immutable snapshot blob
/// without losing the original name merely because the blob itself is named
/// by an ordinal.
pub fn sniff_with_name(content_path: &Path, logical_path: &Path) -> Result<Sniffed> {
    let head = read_prefix(content_path, false, 8)?;
    let gzip = head.len() >= 2 && head[0] == 0x1f && head[1] == 0x8b;
    let prefix = read_prefix(content_path, gzip, 8192)?;
    let mut s = sniff_bytes(&prefix, content_path, logical_path, gzip)?;
    s.gzip = gzip;
    s.logical_name = logical_path.file_name().map(std::path::PathBuf::from);
    Ok(s)
}

fn sniff_bytes(
    prefix: &[u8],
    content_path: &Path,
    logical_path: &Path,
    gzip: bool,
) -> Result<Sniffed> {
    let mk = |family: Family| Sniffed {
        family,
        gzip: false,
        binary_kind: None,
        csv: None,
        encoding: "utf-8",
        logical_name: None,
    };
    if prefix.is_empty() {
        let mut s = mk(Family::Binary);
        s.binary_kind = Some("empty".into());
        return Ok(s);
    }

    // 1. Magic bytes.
    if prefix.starts_with(b"%PDF-") {
        return Ok(mk(Family::Pdf));
    }
    if prefix.starts_with(b"SQLite format 3\0") {
        return Ok(mk(Family::Sqlite));
    }
    if prefix.starts_with(b"PK\x03\x04") {
        // zip container: DOCX iff it holds word/document.xml
        if !gzip {
            if let Ok(f) = std::fs::File::open(content_path) {
                if let Ok(mut z) = zip::ZipArchive::new(f) {
                    let is_docx = (0..z.len()).any(|i| {
                        z.by_index_raw(i)
                            .map(|e| e.name() == "word/document.xml")
                            .unwrap_or(false)
                    });
                    if is_docx {
                        return Ok(mk(Family::Docx));
                    }
                }
            }
        }
        let mut s = mk(Family::Binary);
        s.binary_kind = Some("zip".into());
        return Ok(s);
    }
    // Compressed image/audio/model payloads routinely pass the NUL and
    // control-char heuristics below (windows-1252 decodes almost every byte
    // to something printable), and a multi-MB PSD misread as prose turns into
    // thousands of junk records — measured: a 4,194,360-byte printable blob
    // yields 2,048 sections. `for_each_section` streams, so the cost is the
    // record COUNT, not resident bytes. Magic bytes are the reliable signal.
    // `MAGIC_TABLE` and its invariant are documented at the table itself.
    for &(magic, kind, qualify) in MAGIC_TABLE {
        if prefix.starts_with(magic) && qualify(prefix) {
            let mut s = mk(Family::Binary);
            s.binary_kind = Some(kind.into());
            return Ok(s);
        }
    }
    // Truevision TGA has NO magic number — the file opens straight into its
    // 18-byte header — so a raw texture is the one image format that reaches
    // the text heuristics on its own bytes. Its header is nevertheless highly
    // constrained, which is what makes this safe: byte 1 must be 0 or 1 and
    // byte 2 one of six small values, i.e. the second and third bytes of the
    // file must BOTH be control characters, which text is not.
    if looks_like_tga_header(prefix) {
        let mut s = mk(Family::Binary);
        s.binary_kind = Some("tga".into());
        return Ok(s);
    }

    // 2. Binary vs text: decode UTF-8, fall back windows-1252.
    let (text, encoding) = decode(prefix);
    let nul = prefix.iter().filter(|&&b| b == 0).count();
    if nul * 10 > prefix.len() {
        let mut s = mk(Family::Binary);
        s.binary_kind = Some("unknown".into());
        return Ok(s);
    }
    // High ratio of control chars (excluding \t \n \r) → binary.
    let ctrl = text
        .chars()
        .filter(|c| (*c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r'))
        .count();
    if ctrl * 10 > text.chars().count().max(1) {
        let mut s = mk(Family::Binary);
        s.binary_kind = Some("unknown".into());
        return Ok(s);
    }
    // NOTE — a "decoded via lossy windows-1252 AND over 30% non-ASCII is
    // pixel soup" guard was here, to catch a raw TGA. It is gone, and must
    // not come back in that form: the windows-1252 fallback is what EVERY
    // legacy 8-bit codepage and every legacy CJK encoding decodes through, so
    // the test was not "is this an image", it was "is this written in a script
    // that is not Latin". Measured through `sniff()` against `ca4d75a` with
    // identical fixtures, it changed windows-1251 Russian, KOI8-R Russian,
    // windows-1253 Greek, windows-1255 Hebrew and windows-1256 Arabic prose
    // from `TxtProse` to `Binary`, and `scan_file` turns `Binary` into
    // "junk: binary content (unknown)" — the file is never indexed and the
    // report says only that something unreadable was skipped.
    //
    // A `looks_like_legacy_cjk` escape hatch was tried and could not carry the
    // weight, for two reasons worth recording so it is not retried:
    //   * single-byte codepages never form valid SHIFT_JIS/GBK/BIG5/EUC_KR
    //     double-byte pairs, so Cyrillic/Greek/Hebrew/Arabic were never
    //     rescued by it at all;
    //   * it required a LOSSLESS trial decode, and `sniff()` only ever sees
    //     `read_prefix(path, gzip, 8192)`. A double-byte character straddling
    //     that cut makes every trial decode report `had_errors`, so the same
    //     Shift-JIS document was text or binary depending on its byte length
    //     — measured `TxtProse` at pad 0 and 2, `Binary` at pad 1 and 3.
    //
    // TGA is now caught by its header above, which is evidence of pixel data
    // rather than evidence of a non-Latin script. Headerless raw payloads
    // (`.raw`, `.bytes`, uncompressed PCM) still classify as text, exactly as
    // they do on `main`. Bounding what a magic-less binary costs is the right
    // fix for that (issue #381 — sectioning already streams, so the unbounded
    // quantity is the record count), not byte statistics that cannot tell a
    // texture from Cyrillic.

    // 2b. Unity text serialization, detected by content (never extension):
    // scenes/prefabs/assets open with `%YAML` and declare the Unity tag
    // namespace; `.meta` sidecars open with `fileFormatVersion:` and carry a
    // `guid:`. Binary-serialized Unity assets fail both checks and fall
    // through to the binary/text heuristics as before.
    {
        let body = text.trim_start_matches('\u{feff}');
        if body.starts_with("%YAML") && body.contains("%TAG !u! tag:unity3d.com") {
            let mut s = mk(Family::UnityYaml);
            s.encoding = encoding;
            return Ok(s);
        }
        let first_line = body.lines().next().unwrap_or("");
        if first_line.starts_with("fileFormatVersion:")
            && body.lines().any(|l| l.starts_with("guid:"))
        {
            let mut s = mk(Family::UnityMeta);
            s.encoding = encoding;
            return Ok(s);
        }
        // BVH motion capture: `HIERARCHY` opener with a `ROOT <name>` next.
        // Without this the numeric MOTION block classified as txt-lines and
        // indexed millions of meaningless number rows.
        if first_line.trim() == "HIERARCHY"
            && body
                .lines()
                .nth(1)
                .is_some_and(|l| l.trim_start().starts_with("ROOT "))
        {
            let mut s = mk(Family::Bvh);
            s.encoding = encoding;
            return Ok(s);
        }
    }

    // 2c. Source code: a known code extension whose content is text. We only
    // reach here after the binary guards above, so a text `.py`/`.rs`/`.go`/…
    // routes to the tree-sitter AST extractor (crate::extract::code). Extension
    // is the right signal — code vs prose is not reliably content-sniffable.
    if let Some(ext) = logical_path.extension().and_then(|e| e.to_str()) {
        if crate::extract::code::is_code_ext(ext) {
            let mut s = mk(Family::Code);
            s.encoding = encoding;
            return Ok(s);
        }
    }

    // 3. Text heuristics — complete lines only (last line may be truncated).
    let mut lines: Vec<&str> = text.lines().collect();
    if !text.ends_with('\n') && lines.len() > 1 {
        lines.pop();
    }
    let nonblank: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| !l.trim().is_empty())
        .collect();

    let mut out = mk(classify_text(&text, &nonblank));
    out.encoding = encoding;
    if out.family == Family::Csv {
        out.csv = sniff_csv_dialect(&nonblank);
        if out.csv.is_none() {
            out.family = txt_kind(&nonblank);
        }
    }
    Ok(out)
}

/// The label for the last-resort decode. Every byte maps to *something* in
/// windows-1252, so this never fails and therefore carries no evidence that
/// the bytes are text — the binary heuristics key off this exact string.
pub const WINDOWS_1252_LOSSY: &str = "windows-1252 (lossy)";

/// A magic-byte row: the signature, the `binary_kind` it reports, and the
/// structural check that has to pass before the signature is believed.
type MagicRow = (&'static [u8], &'static str, fn(&[u8]) -> bool);

/// Media/container signatures, tried in order before any text heuristic.
///
/// THE INVARIANT, and the reason this table is a named constant rather than a
/// literal inside `sniff_bytes`: **a signature whose bytes are all printable
/// ASCII MUST carry a qualifier**, because "starts with these letters" is also
/// true of ordinary text. A CSV whose first column header is `ID3` or `BMW`,
/// prose whose first word is `RIFF`, `OggS`, `fLaC`, `8BPS` or `Kaydara FBX
/// Binary`, and a note opening `GIF89a is the version string…` were each junked
/// as media by an unqualified row (measured — see the tests below). The rows
/// that use `accept` are exactly the ones carrying a byte text cannot contain:
/// `\x89PNG`, `\xff\xd8\xff`, `\x7fELF`, `\x00\x00\x01\x00`, the NUL in
/// `II*\x00` / `MM\x00*`, and the `\x01` in the EXR magic.
///
/// `GIF8` and `BM` were the last two exceptions — printable ASCII taken on
/// faith, so a `cars.csv` opening `BMW,model,year` was reported as an image and
/// never indexed (#379/#380, the same defect filed twice). The invariant is now
/// enforced by walking these rows in
/// `printable_magic_tests::every_printable_signature_carries_a_qualifier`, so
/// the next row added in violation of it fails a test instead of a code review.
///
/// Qualifying a magic number is the rule Lucene applies to its own containers:
/// `CodecUtil.checkHeader`
/// (`lucene/core/src/java/org/apache/lucene/codecs/CodecUtil.java:183-201`,
/// Apache-2.0) matches `CODEC_MAGIC` and then hands straight to
/// `checkHeaderNoMagic` (`CodecUtil.java:202-246`), which refuses the file
/// unless the codec name AND a version inside an accepted range follow — the
/// magic alone is never taken as proof. Adapted, not copied: Lucene is
/// validating files it wrote itself, this is declining to delete somebody
/// else's prose.
const MAGIC_TABLE: &[MagicRow] = &[
    (b"\x89PNG", "png", accept),
    (b"GIF8", "gif", gif_screen_descriptor),
    (b"\xff\xd8\xff", "jpeg", accept),
    (b"\x7fELF", "elf", accept),
    (b"BM", "bmp", bmp_file_header),
    (b"\x00\x00\x01\x00", "ico", accept),
    (b"8BPS", "psd", psd_version),
    (b"II*\x00", "tiff", accept),
    (b"MM\x00*", "tiff", accept),
    (b"RIFF", "riff", riff_form),
    (b"OggS", "ogg", ogg_page),
    (b"fLaC", "flac", flac_metadata_block),
    (b"ID3", "mp3", id3v2_header),
    (b"Kaydara FBX Binary", "fbx", fbx_header),
    (b"\x76\x2f\x31\x01", "exr", accept),
];

/// Magic-byte signature taken as sufficient on its own.
///
/// Every caller is a signature containing a byte text cannot contain
/// (`\x89PNG`, `\xff\xd8\xff`, `\x7fELF`, `\x00\x00\x01\x00`, the NUL in
/// `II*\x00` / `MM\x00*`, the `\x01` in the EXR magic) — that byte, not this
/// function, is what makes the match evidence. A signature made only of
/// printable ASCII must NOT be routed here; give it a structural qualifier, as
/// `GIF8` and `BM` now have (#379/#380). Do not read this function as a proof
/// that its callers are safe; it is only a statement that no *further* check is
/// performed.
fn accept(_prefix: &[u8]) -> bool {
    true
}

/// GIF: `GIF8` on its own is four printable letters, so a note opening "GIF89a
/// is the header format used by the GIF image standard" was classified as an
/// image and junked by `scan_file` as "binary content (gif)" (#379/#380).
///
/// Widening the signature to the full six-byte version stamp is NOT enough on
/// its own — that same sentence supplies `GIF89a` — and neither is adding the
/// canvas dimensions, because " i"/"s " are perfectly good non-zero u16s. The
/// qualifier has to reach the fields the spec pins to a fixed value.
///
/// GIF89a spec §17-19: the 6-byte Header (`GIF87a` or `GIF89a`) is followed by
/// the 7-byte Logical Screen Descriptor — canvas width and height
/// (little-endian, and an image has both), a packed field whose top bit is the
/// Global Colour Table flag and whose low three bits size that table, a
/// Background Colour Index, and a Pixel Aspect Ratio. Then comes the Global
/// Colour Table itself, `3 * 2^(N+1)` bytes, and after it the first block of
/// the data stream. The table is at most 768 bytes, so that first block starts
/// at offset 781 at the very latest and is always inside the 8 KiB
/// `read_prefix` buffer.
///
/// That first block is the discriminator. The spec admits exactly three
/// introducers there — `0x21` Extension, `0x2C` Image Descriptor, `0x3B`
/// Trailer — but the introducer ALONE is not enough, and this is worth
/// spelling out because it is what the first attempt at this fix used: all
/// three bytes are printable (`!`, `,`, `;`), so "GIF89a header, the six bytes
/// at the front of every GIF file" puts a comma exactly there and is junked all
/// over again. Measured, along with "GIF89a images! …" and "GIF87a format; …".
/// So the block is decoded one step further, into fields prose cannot supply:
///
///   * after `0x21`, the extension label. The spec defines four — Plain Text
///     `0x01`, Graphic Control `0xF9`, Comment `0xFE`, Application `0xFF` —
///     three unprintable and the fourth a control character.
///   * after `0x2C`, a 9-byte Image Descriptor. The frame must be non-empty and
///     must fit inside the canvas declared two fields earlier, which four
///     little-endian u16s of prose do not.
///
/// A bare `0x3B` is refused: that is a GIF containing no image at all, it
/// appears in no corpus measured, and it carries no pixel data to junk.
///
/// The fields this check does NOT use are as important, because round 1 of
/// #379 used them and they were wrong. Requiring the Pixel Aspect Ratio and
/// the Background Colour Index to be zero looks safe — satisfying them takes
/// NUL bytes, and prose has no NULs — but it refuses REAL GIFs. Swept over
/// every distinct GIF on the build machine (147 files deduped by content hash,
/// 51 B to 17.6 MB): the rule below accepts 147/147; the aspect/background rule
/// refused 4, among them this repository's own `docs/media/demo.gif`, whose
/// aspect byte is 49 — the spec's encoding of a 1:1 pixel ratio, since
/// `(49 + 15) / 64 = 1.0`.
///
/// Falling through to the text heuristics is not a safety net, and the honest
/// version of that claim is narrower than "it still classifies binary". All 4
/// refused GIFs do still reach `Family::Binary` here, but as `unknown` rather
/// than `gif`, and only because their first 8 KiB decodes above the 10%
/// control-character threshold — by 0.14 to 1.5 percentage points. Two other
/// GIFs in the same 147 sit BELOW it, at 9.39% and 9.78%, and would be
/// sectioned into prose records outright if they were refused. Whether a real
/// image survives therefore turns on the entropy of its first 8 KiB, which no
/// qualifier controls. Pinned by
/// `real_gif_shapes_the_aspect_ratio_rule_refused`.
fn gif_screen_descriptor(prefix: &[u8]) -> bool {
    if prefix.len() < 13 || !matches!(&prefix[..6], b"GIF87a" | b"GIF89a") {
        return false;
    }
    let canvas_width = u16::from_le_bytes([prefix[6], prefix[7]]);
    let canvas_height = u16::from_le_bytes([prefix[8], prefix[9]]);
    if canvas_width == 0 || canvas_height == 0 {
        return false;
    }
    let packed = prefix[10];
    // `3 * 2^(N+1)`, N being the low three bits: 6 bytes at N=0, 768 at N=7.
    let color_table = if packed & 0x80 != 0 {
        3 * (1usize << ((packed & 0x07) + 1))
    } else {
        0
    };
    // Background Colour Index (11) and Pixel Aspect Ratio (12) are skipped on
    // purpose — see above, real encoders put non-zero bytes in both.
    let block = 13 + color_table;
    match prefix.get(block) {
        Some(0x21) => matches!(prefix.get(block + 1), Some(0x01 | 0xf9 | 0xfe | 0xff)),
        Some(0x2c) => {
            let Some(desc) = prefix.get(block + 1..block + 9) else {
                return false;
            };
            let left = u32::from(u16::from_le_bytes([desc[0], desc[1]]));
            let top = u32::from(u16::from_le_bytes([desc[2], desc[3]]));
            let width = u32::from(u16::from_le_bytes([desc[4], desc[5]]));
            let height = u32::from(u16::from_le_bytes([desc[6], desc[7]]));
            width != 0
                && height != 0
                && left + width <= u32::from(canvas_width)
                && top + height <= u32::from(canvas_height)
        }
        _ => false,
    }
}

/// Windows bitmap: `BM`, then a 14-byte BITMAPFILEHEADER, then a DIB header
/// that opens with its own size.
///
/// The discriminator is the DIB header size and the two fields immediately
/// behind it. Every DIB header ever defined is between 12 (BITMAPCOREHEADER)
/// and 124 (BITMAPV5HEADER) bytes, so its little-endian u32 puts three NUL
/// bytes at offsets 15..18, where `BMW,model,year` puts `ar\n`. Colour planes
/// is then fixed at 1 by every variant of the header, which pins a fourth NUL,
/// and bit depth behind it is a closed set. Those are the checks text cannot
/// pass; the pixel-array offset must additionally clear both headers.
///
/// Two tighter rules were tried first, in round 1 of #379, and both refuse real
/// bitmaps — which is the failure this table exists to prevent, so neither is
/// used:
///
///   * `bfReserved1/2` at offsets 6..10 are specified as zero, but a bitmap
///     converted from a CUR or ICO carries the cursor hot-spot coordinates
///     there.
///   * a closed set of DIB sizes `{12, 16, 40, 52, 56, 64, 108, 124}` admits
///     five of the OS/2 2.x BITMAPCOREHEADER2 sizes, which are legally anything
///     in `16..=64`, and refuses the other 44.
///
/// Only 3 real BMPs exist on the build machine, all BITMAPINFOHEADER with zero
/// reserved words, so unlike the GIF sweep this is reasoned from the format
/// definitions rather than measured against a corpus — but the mechanism is the
/// one that demonstrably broke GIF, and a synthetic hot-spot / OS/2 bitmap does
/// sniff `txt-prose` under the old rule. Pinned by
/// `real_bmp_shapes_the_closed_set_refused`.
///
/// `bfSize` (offsets 2..6) is deliberately NOT compared against the file
/// length, which is what #379 proposed. Two reasons, both structural: this
/// function classifies from the `read_prefix(path, gzip, 8192)` buffer and has
/// no file length to compare against, and for a gzipped member the on-disk
/// length is the *compressed* length, so the comparison would be wrong exactly
/// where it was applied.
fn bmp_file_header(prefix: &[u8]) -> bool {
    if prefix.len() < 18 {
        return false;
    }
    let pixel_offset = u32::from_le_bytes([prefix[10], prefix[11], prefix[12], prefix[13]]);
    let dib_header_size = u32::from_le_bytes([prefix[14], prefix[15], prefix[16], prefix[17]]);
    if !(12..=124).contains(&dib_header_size) || pixel_offset < 14 + dib_header_size {
        return false;
    }
    // Colour planes then bit depth. BITMAPCOREHEADER declares u16 width and
    // height, every later header i32, which moves the pair from 22 to 26.
    let planes_at = if dib_header_size == 12 { 22 } else { 26 };
    let Some(f) = prefix.get(planes_at..planes_at + 4) else {
        return false;
    };
    u16::from_le_bytes([f[0], f[1]]) == 1
        && matches!(
            u16::from_le_bytes([f[2], f[3]]),
            // 0 is BI_JPEG / BI_PNG, where the DIB carries an embedded image.
            0 | 1 | 2 | 4 | 8 | 16 | 24 | 32
        )
}

/// Adobe Photoshop: `8BPS` is followed by a big-endian version, 1 for PSD and
/// 2 for PSB. Nothing else is defined, so a text file whose first word is
/// `8BPS` fails here on its fifth byte.
fn psd_version(prefix: &[u8]) -> bool {
    prefix.len() >= 6 && prefix[4] == 0 && matches!(prefix[5], 1 | 2)
}

/// RIFF container: `RIFF`, a little-endian chunk size, then a four-character
/// FORM type naming what the container actually holds. Requiring a known FORM
/// is what separates a WAV from a sentence that opens with the word "RIFF".
///
/// `ACON` is the animated-cursor FORM. It is listed because `ANI ` — which was
/// here on its own — is the file EXTENSION, not the FORM type, and no encoder
/// writes it: swept over every RIFF file on the build machine (195 files;
/// FORM types `WAVE` 158, `WEBP` 25, `ACON` 12), the closed set without `ACON`
/// refused all 12 real `.ani` files. Same defect shape as #379 one row up, so
/// it is corrected here rather than left for the closed set to be widened a
/// third time.
fn riff_form(prefix: &[u8]) -> bool {
    prefix.len() >= 12
        && matches!(
            &prefix[8..12],
            b"WAVE" | b"AVI " | b"WEBP" | b"RMID" | b"ACON" | b"CDDA" | b"PAL " | b"RDIB"
        )
}

/// Ogg page header: `OggS` then the stream structure version, which has been
/// 0 for the life of the format (RFC 3533 §6.1).
fn ogg_page(prefix: &[u8]) -> bool {
    prefix.len() >= 5 && prefix[4] == 0
}

/// FLAC stream: `fLaC` then a METADATA_BLOCK_HEADER whose block type must be
/// STREAMINFO (0) for the first block; the top bit is the last-block flag.
fn flac_metadata_block(prefix: &[u8]) -> bool {
    prefix.len() >= 5 && prefix[4] & 0x7f == 0
}

/// Autodesk FBX, binary flavour: the header is the 18 letters `Kaydara FBX
/// Binary`, two spaces, a NUL, then `0x1A 0x00` — 23 bytes before the uint32
/// version field. The three unprintable bytes are the entire discriminator;
/// the letters on their own are ordinary English, and they are *more* likely
/// to appear as prose inside this PR's own target corpus, a Unity/3D-asset
/// tree, than anywhere else. Measured through `sniff()`: a `.md` note opening
/// "Kaydara FBX Binary is the 20-byte magic that opens every binary FBX file
/// exported by Autodesk tools. " was `TxtProse` on `ca4d75a` and
/// `Binary`/`fbx` on this branch until this check existed, and `scan_file`
/// turns `Family::Binary` into "junk: binary content (fbx)" — never indexed.
fn fbx_header(prefix: &[u8]) -> bool {
    prefix.starts_with(b"Kaydara FBX Binary  \x00\x1a\x00")
}

/// ID3v2 tag header: `ID3`, a major version (2, 3 and 4 are the versions that
/// exist), a revision that is not the reserved 0xFF, a flags byte, and a
/// four-byte SYNCHSAFE size whose bytes each have the top bit clear.
///
/// Unqualified, this signature is three ASCII letters: a CSV whose first
/// column header is `ID3` classified `Csv` on `ca4d75a` and `Binary` on this
/// branch until the version byte was checked.
fn id3v2_header(prefix: &[u8]) -> bool {
    prefix.len() >= 10
        && matches!(prefix[3], 2..=4)
        && prefix[4] != 0xff
        && prefix[6..10].iter().all(|b| *b < 0x80)
}

/// Truevision TGA, which has no magic number at all — every byte of its
/// 18-byte header is data. Detection is therefore by CONSTRAINT: an image type
/// from the defined set, a pixel depth from the defined set, a colour-map
/// descriptor that must be all-zero when there is no colour map, and non-zero
/// dimensions.
///
/// The reason this is safe on text is byte 1 and byte 2: `color_map_type` is 0
/// or 1 and `image_type` is one of six values below 12, so both the second and
/// the third byte of the file have to be control characters. Prose, CSV, JSON
/// and source code fail on byte 1.
fn looks_like_tga_header(prefix: &[u8]) -> bool {
    if prefix.len() < 18 {
        return false;
    }
    let color_map_type = prefix[1];
    if color_map_type > 1 {
        return false;
    }
    // 1/2/3 uncompressed colour-mapped/true-colour/greyscale, 9/10/11 the
    // run-length encoded counterparts. Type 0 ("no image data") is excluded
    // deliberately: it carries no pixels, so nothing is lost by letting such a
    // file fall through to the text heuristics, and admitting it would make an
    // all-zero prefix a TGA.
    if !matches!(prefix[2], 1 | 2 | 3 | 9 | 10 | 11) {
        return false;
    }
    if !matches!(prefix[16], 8 | 15 | 16 | 24 | 32) {
        return false;
    }
    if color_map_type == 0 && prefix[3..8].iter().any(|b| *b != 0) {
        return false;
    }
    let width = u16::from_le_bytes([prefix[12], prefix[13]]);
    let height = u16::from_le_bytes([prefix[14], prefix[15]]);
    width > 0 && height > 0
}

fn decode(bytes: &[u8]) -> (String, &'static str) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), "utf-8"),
        Err(e) => {
            // Tolerate a multi-byte char cut at the prefix boundary.
            if e.valid_up_to() + 4 >= bytes.len() {
                (
                    String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned(),
                    "utf-8",
                )
            } else {
                let (s, _, _) = encoding_rs::WINDOWS_1252.decode(bytes);
                (s.into_owned(), WINDOWS_1252_LOSSY)
            }
        }
    }
}

/// Decode a whole byte buffer for extraction (same policy as sniffing).
pub fn decode_text(bytes: &[u8]) -> (String, &'static str) {
    decode(bytes)
}

fn classify_text(text: &str, nonblank: &[&str]) -> Family {
    let trimmed = text.trim_start();
    // A lone `[section]` opening line that does not parse as JSON is a
    // TOML/INI table header (`[package]` in Cargo.toml, `[Unit]` in a
    // systemd unit), not a JSON array. Without this guard every such file
    // fell into the JSON branch below (any '['-opener passes
    // `looks_like_json_start`), reached the JSON extractor, and was junked
    // — which, for Cargo.toml, also silently disabled the cratecite
    // detector on every real repository (its crate table only holds
    // indexed files). Live-verified 2026-07-30: three Cargo.tomls junked
    // as "json candidate family" before this guard, indexed after.
    let ini_table_header = nonblank.first().is_some_and(|l| {
        let t = l.trim();
        t.starts_with('[')
            && t.ends_with(']')
            && serde_json::from_str::<serde_json::Value>(t).is_err()
    });
    // JSON / JSONL
    if !ini_table_header && (trimmed.starts_with('{') || trimmed.starts_with('[')) {
        if nonblank.len() >= 2 {
            let parse_ok = nonblank
                .iter()
                .filter(|l| serde_json::from_str::<serde_json::Value>(l).is_ok())
                .count();
            if parse_ok * 10 >= nonblank.len() * 9 {
                return Family::Jsonl;
            }
        } else if nonblank.len() == 1
            && serde_json::from_str::<serde_json::Value>(nonblank[0]).is_ok()
        {
            // single complete JSON line — treat as JSON value file
            return Family::Json;
        }
        // Pretty-printed or multi-line JSON value.
        if looks_like_json_start(trimmed) {
            return Family::Json;
        }
    }

    // HTML / XML — declaration within the first 256 bytes.
    let head_lc: String = text.chars().take(256).collect::<String>().to_lowercase();
    if head_lc.contains("<!doctype html") || head_lc.contains("<html") {
        return Family::Html;
    }
    if head_lc.contains("<?xml") || (trimmed.starts_with('<') && text.contains("</")) {
        // xhtml disguised as xml?
        let lc: String = text.to_lowercase();
        if lc.contains("<html") || lc.contains("<body") {
            return Family::Html;
        }
        return Family::Xml;
    }

    // Log lines
    if nonblank.len() >= 3 {
        let hits = nonblank
            .iter()
            .filter(|l| crate::extract::logs::probe_line(l))
            .count();
        if hits * 10 >= nonblank.len() * 6 {
            return Family::Logs;
        }
    }

    // SQL dump
    let upper: String = text.chars().take(4096).collect::<String>().to_uppercase();
    if (upper.contains("CREATE TABLE") || upper.contains("INSERT INTO")) && text.contains(';') {
        return Family::SqlDump;
    }

    // CSV — dialect probe happens in caller; here just a cheap plausibility test.
    if nonblank.len() >= 2 && sniff_csv_dialect(nonblank).is_some() {
        return Family::Csv;
    }

    // YAML
    if nonblank.first().map(|l| l.trim() == "---").unwrap_or(false)
        || yaml_line_ratio(nonblank) >= 0.6
    {
        return Family::Yaml;
    }

    txt_kind(nonblank)
}

fn looks_like_json_start(t: &str) -> bool {
    // starts with { or [ and the first ~200 chars look like JSON tokens
    let head: String = t.chars().take(200).collect();
    head.contains(':') || head.contains('[') || head.contains('{')
}

fn yaml_line_ratio(nonblank: &[&str]) -> f64 {
    if nonblank.len() < 3 {
        return 0.0;
    }
    let re = regex::Regex::new(r"^\s*(- )?[\w.@/-]+:(\s|$)").unwrap();
    let hits = nonblank
        .iter()
        .filter(|l| {
            let t = l.trim_start();
            // Markdown task-list items (`- [ ]` / `- [x]`) are NOT YAML
            // evidence: `- [ ] text` is invalid YAML (flow sequence followed
            // by a scalar), while checklists are everywhere in real notes.
            // Counting them here routed whole checklist files into the YAML
            // extractor, which can only junk-file them.
            let checkbox =
                t.starts_with("- [ ] ") || t.starts_with("- [x] ") || t.starts_with("- [X] ");
            re.is_match(l) || (t.starts_with("- ") && !checkbox)
        })
        .count();
    hits as f64 / nonblank.len() as f64
}

fn txt_kind(nonblank: &[&str]) -> Family {
    if nonblank.is_empty() {
        return Family::TxtLines;
    }
    // NOTE — a whitespace-density guard ("text over 4 KiB with under 5%
    // whitespace is pixel soup, junk it") was proposed here to catch raw TGA
    // and `.bytes` payloads that decode into printable characters. It is not
    // present, deliberately, because low whitespace density does not mean
    // "not text":
    //
    //   * Chinese, Japanese, Korean, Thai, Lao, Khmer and Burmese prose is
    //     scriptio continua — `nonblank` comes from `text.lines()`, so the
    //     newlines are already stripped and only INTRA-LINE whitespace counts,
    //     which those scripts do not have. Every such document over ~4 KiB
    //     would be junked, in every corpus, worldwide.
    //   * base64 blobs, hex dumps, FASTA/genomic sequences, single-line
    //     minified payloads and long-token files are all legitimate text with
    //     near-zero whitespace.
    //   * `failure_resume_http_tests::legacy_key_collision_fails_before_
    //     visibility_with_scoped_guidance` builds a 65,537-byte fixture of one
    //     repeated ASCII letter. With the guard in place that file sniffs as
    //     binary and the run exits 3 instead of 0 — the false positive is
    //     reachable from this repo's own suite.
    //
    // A real TGA is caught by `looks_like_tga_header` in `sniff_bytes`, on the
    // structure of its 18-byte header. The byte-statistics version of that
    // check ("mostly non-ASCII under the windows-1252 fallback") was tried and
    // removed in the same breath as this one and for the same reason: it
    // junked Cyrillic, Greek, Hebrew, Arabic and mixed Japanese prose.
    // Bounding what a magic-less binary costs is the right fix for the concern
    // those guards existed for; silently deleting files that look unusual is
    // not. Tracked as #381, which measures the gap this NOTE leaves open: a
    // 4,194,495-byte printable NUL-free blob sniffs `txt-prose` and expands
    // into 2048 indexed records.
    let avg_len = nonblank.iter().map(|l| l.len()).sum::<usize>() as f64 / nonblank.len() as f64;
    if avg_len > 60.0 {
        return Family::TxtProse;
    }
    // A handful of short lines in a note-like file is still prose.
    if nonblank.len() <= 5 {
        return Family::TxtProse;
    }

    // Line LENGTH alone splits documents of the same kind.  A markdown
    // postmortem with `## Headings` averages ~50 chars over 7 lines and used
    // to land in TxtLines, while a 5-line runbook averaging 59 chars landed in
    // TxtProse — same content type, two different families, therefore two
    // different datasets with two different field names (`text` vs `body`).
    // Cross-index BM25 statistics are then incomparable and a caller has to
    // query both fields.
    //
    // Sentence density is the property that actually distinguishes a document
    // from a record stream: prose lines end in terminal punctuation, whereas
    // log lines, CSV rows and source code do not.  Measured on a mixed corpus:
    // markdown 0.43-0.57, nginx access logs 0.00, syslog 0.20, Rust/Python/JS
    // source 0.00-0.10.
    let sentences = nonblank
        .iter()
        .filter(|l| {
            let t = l.trim_end();
            t.ends_with('.') || t.ends_with('!') || t.ends_with('?')
        })
        .count();
    let sentence_ratio = sentences as f64 / nonblank.len() as f64;
    if sentence_ratio >= 0.40 {
        return Family::TxtProse;
    }

    // Hard-wrapped markdown rescue. Sentence density per LINE undercounts
    // prose whose author wraps at ~70-80 columns: sentences end mid-line, so
    // a five-paragraph note with a `# Title` scores ~0.30 and landed in
    // TxtLines — which silently cost it its title, its section anchors, and
    // (second brain) its wikilink detection. Content evidence, not the
    // extension: a file that OPENS with an ATX heading and shows either
    // markdown link syntax or some terminal punctuation is a markdown
    // document. The heading-opener requirement keeps shebang'd scripts
    // (`#!…`) and most code/config out; the second signal keeps out comment
    // banners over pure record streams.
    let md_link = nonblank
        .iter()
        .any(|l| l.contains("[[") || l.contains("]("));
    if md_heading(nonblank[0]) && (md_link || sentence_ratio >= 0.20) {
        return Family::TxtProse;
    }
    Family::TxtLines
}

/// `# Title` … `###### Title` — an ATX markdown heading (1-6 `#`, a space,
/// then text). `#!/bin/sh` and bare `#` fail the space-then-text rule.
fn md_heading(line: &str) -> bool {
    let t = line.trim_start();
    let hashes = t.bytes().take_while(|&b| b == b'#').count();
    (1..=6).contains(&hashes) && t.as_bytes().get(hashes) == Some(&b' ') && t.len() > hashes + 1
}

/// Quote-aware field split (supports " and ' quoting).
fn split_quoted(line: &str, delim: u8) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                } else if c as u32 == delim as u32 {
                    fields.push(std::mem::take(&mut cur));
                } else {
                    cur.push(c);
                }
            }
        }
    }
    fields.push(cur);
    fields
}

fn sniff_csv_dialect(nonblank: &[&str]) -> Option<CsvDialect> {
    if nonblank.len() < 2 {
        return None;
    }
    let sample: Vec<&str> = nonblank.iter().take(64).copied().collect();
    let mut best: Option<(u8, usize)> = None; // (delim, field count)
    for delim in *b",;\t|" {
        let counts: Vec<usize> = sample
            .iter()
            .map(|l| split_quoted(l, delim).len())
            .collect();
        let first = counts[0];
        if first < 2 {
            continue;
        }
        let consistent = counts.iter().filter(|&&c| c == first).count();
        // ≥90% of lines share the same field count
        if consistent * 10 >= counts.len() * 9 {
            match best {
                Some((_, bc)) if bc >= first => {}
                _ => best = Some((delim, first)),
            }
        }
    }
    let (delim, _) = best?;
    let head_fields = split_quoted(sample[0], delim);
    let numericish = |s: &str| {
        let t = s.trim();
        !t.is_empty()
            && t.chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | '+'))
            && t.chars().any(|c| c.is_ascii_digit())
    };
    let has_header = {
        let mut distinct = std::collections::HashSet::new();
        let all_nonnum = head_fields.iter().all(|f| !numericish(f));
        let all_distinct = head_fields
            .iter()
            .all(|f| distinct.insert(f.trim().to_string()));
        let body_has_num = sample
            .iter()
            .skip(1)
            .any(|l| split_quoted(l, delim).iter().any(|f| numericish(f)));
        all_nonnum && all_distinct && body_has_num
    };
    // decimal comma: with ';' delimiter, a meaningful share of fields look like 12,3
    let decimal_comma = if delim == b';' {
        let re = regex::Regex::new(r"^-?\d{1,9},\d+$").unwrap();
        let (mut num, mut hits) = (0usize, 0usize);
        for l in sample.iter().skip(if has_header { 1 } else { 0 }) {
            for f in split_quoted(l, delim) {
                let t = f.trim().to_string();
                if numericish(&t) {
                    num += 1;
                    if re.is_match(&t) {
                        hits += 1;
                    }
                }
            }
        }
        num > 0 && hits * 10 >= num * 3
    } else {
        false
    };
    Some(CsvDialect {
        delim,
        has_header,
        decimal_comma,
    })
}

#[cfg(test)]
mod unity_sniff_tests {
    use super::*;
    use std::path::Path;

    fn sniff_str(s: &str, name: &str) -> Family {
        sniff_bytes(s.as_bytes(), Path::new(name), Path::new(name), false)
            .unwrap()
            .family
    }

    #[test]
    fn unity_tagged_yaml_is_detected_by_header_not_extension() {
        let scene =
            "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!1 &123\nGameObject:\n  m_Name: X\n";
        assert_eq!(sniff_str(scene, "Main.unity"), Family::UnityYaml);
        assert_eq!(
            sniff_str(scene, "renamed.txt"),
            Family::UnityYaml,
            "content decides, never the extension"
        );
    }

    #[test]
    fn a_bom_before_the_yaml_directive_is_tolerated() {
        let scene =
            "\u{feff}%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!1 &1\nGameObject:\n  m_Name: X\n";
        assert_eq!(sniff_str(scene, "Main.unity"), Family::UnityYaml);
    }

    #[test]
    fn meta_needs_the_first_line_rule_and_a_guid() {
        let meta =
            "fileFormatVersion: 2\nguid: 9f1c4d0ab2e34f6\nMonoImporter:\n  serializedVersion: 2\n";
        assert_eq!(sniff_str(meta, "Player.cs.meta"), Family::UnityMeta);
        let stray = "config: true\nfileFormatVersion: 2\nguid: abc\n";
        assert_ne!(
            sniff_str(stray, "some.yaml"),
            Family::UnityMeta,
            "guid keys inside ordinary YAML must not reclassify it"
        );
        let no_guid = "fileFormatVersion: 2\nsettings:\n  a: 1\n";
        assert_ne!(sniff_str(no_guid, "x.meta"), Family::UnityMeta);
    }

    #[test]
    fn bvh_is_detected_by_hierarchy_root_header() {
        let bvh = "HIERARCHY\nROOT Hips\n{\n  OFFSET 0 90 0\n  CHANNELS 6 Xposition Yposition Zposition Zrotation Xrotation Yrotation\n}\nMOTION\nFrames: 2\nFrame Time: 0.033\n1 2 3\n4 5 6\n";
        assert_eq!(sniff_str(bvh, "clip.bvh"), Family::Bvh);
        assert_eq!(
            sniff_str(bvh, "clip.txt"),
            Family::Bvh,
            "content decides, never the extension"
        );
        let not_bvh = "HIERARCHY\nof needs (Maslow):\n- physiological\n- safety\n";
        assert_ne!(sniff_str(not_bvh, "notes.txt"), Family::Bvh);
    }

    #[test]
    fn plain_yaml_without_the_unity_tag_stays_yaml() {
        let plain = "%YAML 1.2\n---\nkey: value\nother: 1\nnested:\n  a: 2\n";
        assert_ne!(sniff_str(plain, "doc.yaml"), Family::UnityYaml);
    }

    /// Regression: PSD image data decoded via lossy windows-1252 passed the
    /// NUL/control-char heuristics and classified as txt-prose, shredding a
    /// multi-MB texture into thousands of junk prose records (a 4 MB blob
    /// yields ~2,048 sections). Sectioning streams, so the cost is record
    /// count, not RAM. Media magic bytes must win before any text heuristic.
    #[test]
    fn media_containers_are_binary_by_magic_not_heuristics() {
        for (name, head, kind) in [
            ("t.psd", &b"8BPS\x00\x01"[..], "psd"),
            ("t.tif", &b"II*\x00\x08\x00"[..], "tiff"),
            ("t.tif2", &b"MM\x00*\x00\x08"[..], "tiff"),
            ("t.wav", &b"RIFF\x24\x08\x00\x00WAVE"[..], "riff"),
            ("t.ogg", &b"OggS\x00\x02"[..], "ogg"),
            ("t.flac", &b"fLaC\x00\x00\x00\x22"[..], "flac"),
            ("t.mp3", &b"ID3\x03\x00\x00\x00\x00\x0f\x76"[..], "mp3"),
            // The REAL 23-byte binary-FBX header: the 18 letters, two spaces,
            // NUL, 0x1A, 0x00. The previous fixture stopped at the NUL, which
            // is not a header any Autodesk tool emits and which no longer
            // satisfies `fbx_header`.
            ("t.fbx", &b"Kaydara FBX Binary  \x00\x1a\x00"[..], "fbx"),
        ] {
            let mut bytes = head.to_vec();
            // A printable tail that WOULD pass the prose heuristics.
            bytes.extend(b"lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(20));
            let sn = sniff_bytes(&bytes, Path::new(name), Path::new(name), false).unwrap();
            assert_eq!(sn.family, Family::Binary, "{name} must be binary");
            assert_eq!(
                sn.binary_kind.as_deref(),
                Some(kind),
                "{name} must be recognised as {kind}, not fall through to the \
                 heuristics and land on `unknown`"
            );
        }
    }

    /// Regression: the printable-ASCII signatures added by this branch matched
    /// TEXT. Measured through `sniff()` on `ca4d75a` vs this branch with
    /// identical fixtures, a CSV whose first column header is `ID3` went `Csv`
    /// -> `Binary`, and prose whose first word is `RIFF`, `OggS`, `fLaC`,
    /// `8BPS` or `Kaydara FBX Binary` went `TxtProse` -> `Binary`.
    /// `scan_file` turns `Family::Binary` into "junk: binary content (...)",
    /// so each of those files stopped being indexed at all.
    ///
    /// The FBX case was missed in round 2 and found in review: it was the one
    /// signature in the table still using `accept`, and at 18 printable
    /// characters it is the likeliest of all of them to open a real sentence
    /// — in a Unity/3D-asset corpus above all, which is this PR's target.
    ///
    /// The true positives above must keep passing, which is the whole point:
    /// the signature is necessary, it is just not sufficient.
    #[test]
    fn a_printable_ascii_signature_alone_does_not_make_a_file_binary() {
        let csv = b"ID3,name,value\n1,alpha,2\n3,beta,4\n5,gamma,6\n7,delta,8\n";
        assert_eq!(
            sniff_bytes(csv, Path::new("t.csv"), Path::new("t.csv"), false)
                .unwrap()
                .family,
            Family::Csv,
            "a column header may legitimately be the three letters ID3"
        );
        for (label, opener) in [
            (
                "riff",
                "RIFF is a container format used by WAV files. It stores chunks. ",
            ),
            (
                "oggs",
                "OggS pages carry the packets of an Ogg stream in order. ",
            ),
            (
                "flac",
                "fLaC is the four byte magic of a FLAC audio stream header. ",
            ),
            (
                "psd",
                "8BPS is the magic of an Adobe Photoshop document header. ",
            ),
            (
                "fbx",
                "Kaydara FBX Binary is the 20-byte magic that opens every binary \
                 FBX file exported by Autodesk tools. ",
            ),
            // The DOUBLE-space variant is the dangerous one and was previously
            // only measured by hand, never pinned. `fbx_header` requires the
            // real 23-byte header `Kaydara FBX Binary  \x00\x1a\x00`, and the
            // two spaces ARE part of it — so this sentence satisfies 20 of
            // those 23 bytes and is refused only at offset 20, where the
            // header needs `\x00` and prose has a letter. The one-space
            // fixture above is refused one byte earlier, at offset 19. Both
            // match the 18-byte table entry itself in full; the qualifier is
            // the only thing that keeps either of them out of `Family::Binary`.
            (
                "fbx-two-space",
                "Kaydara FBX Binary  is followed by a NUL and 0x1a in the real \
                 header, which is what this note is about. ",
            ),
        ] {
            let text = opener.repeat(30);
            let sn = sniff_bytes(
                text.as_bytes(),
                Path::new("notes.txt"),
                Path::new("notes.txt"),
                false,
            )
            .unwrap();
            assert_ne!(
                sn.family,
                Family::Binary,
                "{label}: prose that opens with the signature word is still prose"
            );
        }
    }

    /// TGA is the format the removed byte-statistics guard existed for: it has
    /// no magic number, so a raw texture reaches the text heuristics on its own
    /// bytes. It is caught on the structure of its 18-byte header instead.
    #[test]
    fn a_raw_tga_texture_is_binary_by_header_structure() {
        // id_len=0, no colour map, uncompressed true-colour, 64x64, 24bpp.
        let mut tga: Vec<u8> = vec![0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 64, 0, 64, 0, 24, 0];
        tga.extend((0..8192u32).map(|i| (0x80 + (i * 37) % 0x7f) as u8));
        let sn = sniff_bytes(
            &tga,
            Path::new("texture.tga"),
            Path::new("texture.tga"),
            false,
        )
        .unwrap();
        assert_eq!(sn.family, Family::Binary, "a real TGA header must be seen");
        assert_eq!(sn.binary_kind.as_deref(), Some("tga"));

        // And the constraint must be tight enough that text never satisfies
        // it. Every one of these is >= 18 bytes, so length is not what saves
        // them.
        for (label, text) in [
            (
                "prose",
                "The quick brown fox jumps over the lazy dog again.",
            ),
            ("csv", "id,name,value\n1,alpha,2\n3,beta,4\n"),
            ("json", "{\"id\": 1, \"name\": \"alpha\", \"value\": 2}"),
            ("code", "fn main() { println!(\"hello, world\"); }"),
            ("yaml", "name: alpha\nvalue: 2\nnested:\n  a: 1\n"),
        ] {
            assert!(text.len() >= 18, "{label}: fixture too short to prove it");
            assert!(
                !looks_like_tga_header(text.as_bytes()),
                "{label}: text must not satisfy the TGA header constraints"
            );
        }
    }

    /// Whitespace density must NOT be used to junk text. This pins the
    /// counter-examples that make the rejected guard unsafe: a long run of one
    /// ASCII letter (the shape of this repo's own 65,537-byte resume-key
    /// fixture), and a base64 blob. Both are >4 KiB with zero whitespace, and
    /// both are text.
    #[test]
    fn whitespace_free_ascii_text_is_not_junked_as_binary() {
        let mut repeated = "x".repeat(65_536);
        repeated.push('b');
        let b64: String = std::iter::repeat_n("QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo", 300)
            .collect::<Vec<_>>()
            .join("");
        for (label, text) in [("repeated-letter", &repeated), ("base64", &b64)] {
            assert!(text.chars().filter(|c| c.is_whitespace()).count() == 0);
            let sn = sniff_bytes(
                text.as_bytes(),
                Path::new("blob.txt"),
                Path::new("blob.txt"),
                false,
            )
            .unwrap();
            assert_ne!(
                sn.family,
                Family::Binary,
                "{label}: whitespace-free ASCII is still text"
            );
        }
        // Real prose is unaffected either way.
        let prose = "The quick brown fox jumps over the lazy dog. ".repeat(200);
        let sn = sniff_bytes(
            prose.as_bytes(),
            Path::new("note.txt"),
            Path::new("note.txt"),
            false,
        )
        .unwrap();
        assert_ne!(sn.family, Family::Binary, "real prose must stay text");
    }

    /// Regression: a whitespace-density guard judged every script by a rule
    /// that only holds for space-delimited ones. `nonblank` is built from
    /// `text.lines()`, so newlines are already gone and only INTRA-LINE
    /// whitespace counts — which CJK/Thai/Lao/Khmer/Burmese prose does not
    /// have. Every such document over ~4 KB became `Family::Binary` and was
    /// junked as "binary content (unknown)", in a Unity PR, for users with no
    /// Unity project. Both original tests used Latin prose and ASCII soup, so
    /// neither could see it. The guard is gone; this pins the outcome so it
    /// cannot come back in another form.
    #[test]
    fn scriptio_continua_prose_is_not_mistaken_for_binary_soup() {
        // Each sample is >= 4096 chars (the old guard's gate) and has NO
        // spaces, exactly like the real documents that were being junked.
        for (label, unit) in [
            ("chinese", "本文档描述了系统的架构设计与实现细节。"),
            (
                "japanese",
                "この文書はシステムの設計と実装について説明します。",
            ),
            ("korean", "이문서는시스템설계와구현에대해설명합니다"),
            ("thai", "เอกสารนี้อธิบายการออกแบบและการใช้งานของระบบ"),
        ] {
            let text = unit.repeat(4096 / unit.chars().count() + 2);
            assert!(
                text.chars().count() >= 4096,
                "{label}: sample must clear the 4096-char gate"
            );
            let ws = text.chars().filter(|c| c.is_whitespace()).count();
            assert!(
                ws * 20 < text.chars().count(),
                "{label}: sample must be below the 5% whitespace ratio, else \
                 it would have passed the old guard for the wrong reason"
            );
            let sn = sniff_bytes(
                text.as_bytes(),
                Path::new("notes.txt"),
                Path::new("notes.txt"),
                false,
            )
            .unwrap();
            assert_ne!(
                sn.family,
                Family::Binary,
                "{label}: scriptio-continua prose must not be junked as binary"
            );
        }
    }

    /// Classify a byte buffer through the REAL read path: a file on disk, read
    /// by `sniff()`, which sees only `read_prefix(path, gzip, 8192)`.
    ///
    /// Every legacy-encoding regression below has to go through this and not
    /// through `sniff_bytes` with the whole buffer. The escape hatch that was
    /// supposed to protect legacy CJK required a LOSSLESS trial decode, and a
    /// double-byte character straddling the 8192-byte cut makes every trial
    /// decode fail — so a test that hands over the complete buffer cannot
    /// observe half of the bug it is written for.
    fn sniff_file(bytes: &[u8], name: &str) -> Family {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        sniff(&path).unwrap().family
    }

    /// Regression (round 2): a guard on "lossy windows-1252 AND over 30%
    /// non-ASCII" is not a test for pixel data, it is a test for "not written
    /// in Latin script". windows-1252 is the fallback EVERY legacy 8-bit
    /// codepage decodes through.
    ///
    /// Measured through `sniff()` with these exact fixtures: on `ca4d75a` all
    /// five are `TxtProse`; with the guard present all five are `Binary`, and
    /// `scan_file` turns that into "junk: binary content (unknown)".
    ///
    /// The assertion is deliberately only "not binary" — this crate still
    /// decodes the bytes as windows-1252 and still produces mojibake, because
    /// telling legacy codepages apart needs a statistical language model it
    /// does not have. Mojibake that is INDEXED beats prose that is deleted.
    #[test]
    fn legacy_single_byte_prose_is_not_junked_by_the_real_read_path() {
        for (label, enc, unit) in [
            (
                "windows-1251-ru",
                encoding_rs::WINDOWS_1251,
                "Этот документ описывает архитектуру системы и детали реализации. ",
            ),
            (
                "koi8-r-ru",
                encoding_rs::KOI8_R,
                "Этот документ описывает архитектуру системы и детали реализации. ",
            ),
            (
                "windows-1253-el",
                encoding_rs::WINDOWS_1253,
                "Το έγγραφο αυτό περιγράφει την αρχιτεκτονική του συστήματος. ",
            ),
            (
                "windows-1255-he",
                encoding_rs::WINDOWS_1255,
                "מסמך זה מתאר את ארכיטקטורת המערכת ואת פרטי היישום שלה. ",
            ),
            (
                "windows-1256-ar",
                encoding_rs::WINDOWS_1256,
                "تصف هذه الوثيقة بنية النظام وتفاصيل تنفيذه بشكل كامل. ",
            ),
        ] {
            let text = unit.repeat(200);
            let (bytes, _, had_errors) = enc.encode(&text);
            assert!(!had_errors, "{label}: fixture must encode cleanly");
            assert!(
                std::str::from_utf8(&bytes).is_err(),
                "{label}: fixture must not be valid UTF-8, or it proves nothing"
            );
            assert!(
                bytes.len() > 8192,
                "{label}: fixture must exceed the sniff prefix"
            );
            assert_ne!(
                sniff_file(&bytes, "doc.txt"),
                Family::Binary,
                "{label}: legacy-encoded prose must not be junked as binary"
            );
        }
    }

    /// Regression (round 2): the legacy-CJK escape hatch was alignment
    /// dependent. `sniff()` classifies from the first 8192 bytes only, so a
    /// double-byte character split by that cut made the trial decode report
    /// `had_errors` and the rescue declined. Measured on the branch that
    /// carried it: the SAME Shift-JIS document was `TxtProse` at byte pads 0
    /// and 2 and `Binary` at pads 1 and 3 — a coin flip per file.
    ///
    /// Sweeping the alignment is the point of this test; a single-length
    /// fixture passes half the time by luck.
    #[test]
    fn legacy_cjk_prose_survives_every_prefix_alignment() {
        for (label, enc, unit) in [
            (
                "shift_jis",
                encoding_rs::SHIFT_JIS,
                "この文書はシステムの設計と実装について詳しく説明します。",
            ),
            (
                "gbk",
                encoding_rs::GBK,
                "本文档描述了系统的架构设计与实现。",
            ),
            (
                "big5",
                encoding_rs::BIG5,
                "本文件描述系統的架構設計與實作。",
            ),
            (
                "euc-kr",
                encoding_rs::EUC_KR,
                "이문서는시스템의설계와구현을설명합니다",
            ),
        ] {
            let text = unit.repeat(400);
            let (body, _, had_errors) = enc.encode(&text);
            assert!(!had_errors, "{label}: fixture must encode cleanly");
            assert!(
                body.len() > 8192,
                "{label}: fixture must exceed the sniff prefix, or alignment \
                 cannot matter"
            );
            for pad in 0..4usize {
                let mut bytes = vec![b'a'; pad];
                bytes.extend_from_slice(&body);
                assert_ne!(
                    sniff_file(&bytes, "doc.txt"),
                    Family::Binary,
                    "{label}: junked at byte-offset pad {pad}"
                );
            }
        }
    }

    /// Regression (round 2): a realistic Japanese technical document is not
    /// 100% ideographs — it is prose around ASCII code fences, identifiers and
    /// URLs. That shape sat under the 30% scriptio-continua threshold the
    /// rescue needed while sitting over the 30% non-ASCII threshold the guard
    /// junked on, so the ordinary shape of real CJK documentation was the case
    /// that lost. Measured: `TxtLines` on `ca4d75a`, `Binary` on the branch,
    /// at both 13 KB and 67 KB.
    #[test]
    fn mixed_script_documentation_is_not_junked() {
        let unit = "この関数は入力データを検証してから処理を実行し、結果を呼び出し元に返します。\n\
                    エラーが発生した場合は、詳細な理由を含む診断情報を記録します。\n\
                    ```\nfn process(d: &[u8]) -> Result<Vec<u8>, ProcessError> {\n    \
                    let checked = validate(d)?;\n    Ok(checked.to_vec())\n}\n```\n\
                    See docs/process.md and the API reference for the full parameter list.\n";
        let ideographic =
            unit.chars().filter(|c| !c.is_ascii()).count() * 100 / unit.chars().count().max(1);
        assert!(
            (20..30).contains(&ideographic),
            "fixture must sit in the band that loses — between the rescue's \
             30% ideograph floor and the guard's 30% non-ASCII ceiling; got \
             {ideographic}%"
        );
        for reps in [40usize, 200] {
            let doc = unit.repeat(reps);
            let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode(&doc);
            assert_ne!(
                sniff_file(&bytes, "doc.txt"),
                Family::Binary,
                "mixed Japanese/ASCII documentation ({} bytes) must not be junked",
                bytes.len()
            );
        }
    }

    /// Accented European prose is the case the removed guard claimed to be
    /// safe for. It must stay safe now that the guard is gone.
    #[test]
    fn european_windows_1252_prose_is_still_prose() {
        let text = "Le système décrit ici gère les données réservées à \
                    l'opérateur, prêtes à être exportées. ";
        let long = text.repeat(200);
        let (bytes, _, _) = encoding_rs::WINDOWS_1252.encode(&long);
        assert!(std::str::from_utf8(&bytes).is_err());
        let (_, enc) = decode(&bytes);
        assert_eq!(enc, WINDOWS_1252_LOSSY, "must stay windows-1252");
        assert_ne!(
            sniff_file(&bytes, "note.txt"),
            Family::Binary,
            "accented prose is still prose"
        );
    }

    #[test]
    fn binary_serialized_unity_assets_stay_binary() {
        let mut bytes = b"UnityFS\x00\x00\x00\x00\x08".to_vec();
        bytes.extend(std::iter::repeat_n(0u8, 64));
        let sn = sniff_bytes(
            &bytes,
            Path::new("scene.unity"),
            Path::new("scene.unity"),
            false,
        )
        .unwrap();
        assert_eq!(sn.family, Family::Binary);
    }
}

/// Regression for #379/#380: `GIF8` and `BM` were the last two printable-ASCII
/// signatures in `MAGIC_TABLE` taken on faith, so text beginning with those
/// characters was classified `Family::Binary` and `scan_file` junked it as
/// "binary content (gif)" / "(bmp)" — never indexed, with a wrong reason. The
/// reported case is a CSV whose first column header is `BMW`.
///
/// Before this module's fix, `gif8_and_bm_are_still_taken_on_faith` pinned the
/// wrong answers here on purpose. These tests are its replacement: they assert
/// the right answer for the same fixtures, keep the true positives, and add a
/// walk of `MAGIC_TABLE` so the *class* of defect cannot come back by way of a
/// new row.
#[cfg(test)]
mod printable_magic_tests {
    use super::*;

    fn sniff_str(text: &str, name: &str) -> Sniffed {
        let p = Path::new(name);
        sniff_bytes(text.as_bytes(), p, p, false).unwrap()
    }

    /// The issue's own three fixtures, plus the CSV shape it calls out. `ID3`
    /// is included because it was already correct and must stay correct.
    #[test]
    fn text_that_opens_with_gif8_or_bm_is_text() {
        for (label, text, name, want) in [
            (
                "cars-csv",
                "BMW,model,year\nX5,SUV,2024\n330i,sedan,2023\nM3,coupe,2022\niX,SUV,2025\n"
                    .to_string(),
                "cars.csv",
                Family::Csv,
            ),
            (
                "bmi-csv",
                "BMI,height_cm,weight_kg\n22.1,180,71\n25.6,165,70\n19.8,171,58\n27.3,178,86\n"
                    .to_string(),
                "health.csv",
                Family::Csv,
            ),
            (
                "gif8-csv",
                "GIF8,name,value\n1,alpha,2\n3,beta,4\n5,gamma,6\n7,delta,8\n".to_string(),
                "t.csv",
                Family::Csv,
            ),
            (
                "gif-notes",
                "GIF89a is the header format used by the GIF image standard. ".repeat(30),
                "gif-notes.txt",
                Family::TxtProse,
            ),
            (
                "gif87a-notes",
                "GIF87a was the first version of the format, published in 1987. ".repeat(30),
                "gif87.txt",
                Family::TxtProse,
            ),
            (
                "gif8-prose",
                "GIF8 is the four byte magic that opens a GIF image file. ".repeat(30),
                "notes.txt",
                Family::TxtProse,
            ),
            (
                "bm-prose",
                "BM is the two byte magic that opens a Windows bitmap file. ".repeat(30),
                "notes.txt",
                Family::TxtProse,
            ),
            (
                "id3-notes",
                "ID3 tags are metadata containers embedded in MP3 files. ".repeat(30),
                "id3-notes.txt",
                Family::TxtProse,
            ),
        ] {
            let sn = sniff_str(&text, name);
            assert_ne!(
                sn.family,
                Family::Binary,
                "{label}: text opening with a printable signature was junked as \
                 {:?}",
                sn.binary_kind
            );
            assert_eq!(sn.family, want, "{label}: wrong family");
        }
    }

    /// The signature is necessary, it is just not sufficient — real bitmaps and
    /// real GIFs must still be caught by magic, before any text heuristic, or
    /// the fix has simply moved the damage (a multi-MB image sectioned into
    /// thousands of prose records, which is what the magic table exists to
    /// prevent).
    #[test]
    fn real_bitmaps_and_gifs_are_still_binary_by_magic() {
        // 2x2 24bpp BMP: BITMAPFILEHEADER (14) + BITMAPINFOHEADER (40).
        let mut bmp: Vec<u8> = b"BM".to_vec();
        bmp.extend_from_slice(&70u32.to_le_bytes()); // bfSize
        bmp.extend_from_slice(&[0, 0, 0, 0]); // bfReserved1/2
        bmp.extend_from_slice(&54u32.to_le_bytes()); // bfOffBits
        bmp.extend_from_slice(&40u32.to_le_bytes()); // biSize
        bmp.extend_from_slice(&2i32.to_le_bytes()); // biWidth
        bmp.extend_from_slice(&2i32.to_le_bytes()); // biHeight
        bmp.extend_from_slice(&[1, 0, 24, 0]); // planes, bpp
        bmp.extend(std::iter::repeat_n(0u8, 24));
        // A printable tail that WOULD pass the prose heuristics.
        bmp.extend(b"lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(20));

        // BITMAPCOREHEADER is the other end of the range that exists.
        let mut bmp_core: Vec<u8> = b"BM".to_vec();
        bmp_core.extend_from_slice(&38u32.to_le_bytes());
        bmp_core.extend_from_slice(&[0, 0, 0, 0]);
        bmp_core.extend_from_slice(&26u32.to_le_bytes()); // 14 + 12
        bmp_core.extend_from_slice(&12u32.to_le_bytes()); // BITMAPCOREHEADER
        bmp_core.extend_from_slice(&[2, 0, 2, 0, 1, 0, 24, 0]);
        bmp_core.extend(b"lorem ipsum dolor sit amet, consectetur adipiscing. ".repeat(20));

        // GIF89a, 4x4, global colour table of 2 entries, then a Graphic Control
        // extension — introducer 0x21, label 0xF9.
        let mut gif: Vec<u8> = b"GIF89a".to_vec();
        gif.extend_from_slice(&4u16.to_le_bytes()); // width
        gif.extend_from_slice(&4u16.to_le_bytes()); // height
        gif.push(0x80); // GCT present, 2 entries
        gif.push(0); // background colour index
        gif.push(0); // pixel aspect ratio
        gif.extend_from_slice(&[0, 0, 0, 0xff, 0xff, 0xff]); // GCT
        gif.extend_from_slice(&[0x21, 0xf9]);
        gif.extend(b"lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(20));

        // GIF87a with no global colour table, opening straight on an Image
        // Descriptor: introducer 0x2C, then left/top/width/height/packed for a
        // frame that fills the 16x16 canvas.
        let mut gif87: Vec<u8> = b"GIF87a".to_vec();
        gif87.extend_from_slice(&[16, 0, 16, 0, 0x07, 0, 0]);
        gif87.extend_from_slice(&[0x2c, 0, 0, 0, 0, 16, 0, 16, 0, 0]);
        gif87.extend(b"lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(20));

        for (name, bytes, kind) in [
            ("t.bmp", bmp, "bmp"),
            ("core.bmp", bmp_core, "bmp"),
            ("t.gif", gif, "gif"),
            ("t87.gif", gif87, "gif"),
        ] {
            let p = Path::new(name);
            let sn = sniff_bytes(&bytes, p, p, false).unwrap();
            assert_eq!(sn.family, Family::Binary, "{name} must be binary");
            assert_eq!(
                sn.binary_kind.as_deref(),
                Some(kind),
                "{name} must be recognised as {kind} by magic, not fall through \
                 to the heuristics"
            );
        }
    }

    /// A printable body the text heuristics WOULD accept: no NULs and no
    /// control characters, so a fixture carrying it reaches `TxtProse` the
    /// moment its qualifier declines. That is what makes the tests below true
    /// negatives rather than restatements of the NUL ratio check — a colour
    /// table or a pixel array full of zeroes would be caught by the heuristics
    /// even with the magic row deleted, and would prove nothing.
    fn printable_payload(n: usize) -> Vec<u8> {
        b"lorem ipsum dolor sit amet, consectetur adipiscing elit. "
            .repeat(n)
            .to_vec()
    }

    /// True negatives for the regression round 1 of #379 shipped: real GIF
    /// header shapes that the "Pixel Aspect Ratio and Background Colour Index
    /// must be zero" qualifier REFUSED, sending a real image down the text
    /// path to be sectioned into prose records.
    ///
    /// Both shapes are transcribed from real files on the build machine, not
    /// invented — which is the point, since
    /// `real_bitmaps_and_gifs_are_still_binary_by_magic` builds ideal headers
    /// and therefore could not see this class at all:
    ///
    ///   * `docs/media/demo.gif`, this repository's own 3.27 MB demo:
    ///     720x450, packed `0xF7`, background `0xFF`, **aspect `0x31`**, a
    ///     768-byte global colour table, then the `0x21 0xFF` NETSCAPE2.0
    ///     application extension that every animated GIF carries. `0x31` is
    ///     not corruption, it is the spec's encoding of a 1:1 pixel ratio:
    ///     `(49 + 15) / 64 = 1.0`.
    ///   * a 92 KB screen capture with the **GCT flag clear and background
    ///     index 255**, which the spec says should be 0 when no table is
    ///     declared and which real encoders write anyway.
    ///
    /// Two things about the assertion are deliberate. The bodies are printable
    /// (see `printable_payload`), so a refused fixture sniffs `TxtProse` — the
    /// cue for `extract` to turn an image into records — rather than being
    /// rescued by the control-character heuristic and quietly proving nothing.
    /// And the expectation is `Some("gif")`, not merely `Family::Binary`,
    /// because that heuristic is luck, not a fallback: across the 147 real
    /// GIFs measured it clears the 10% threshold by as little as 0.14 points,
    /// and two of them sit under it at 9.39% and 9.78%.
    #[test]
    fn real_gif_shapes_the_aspect_ratio_rule_refused() {
        // demo.gif: GCT present and full size, non-zero background AND aspect.
        let mut animated: Vec<u8> = b"GIF89a".to_vec();
        animated.extend_from_slice(&720u16.to_le_bytes());
        animated.extend_from_slice(&450u16.to_le_bytes());
        animated.push(0xf7); // GCT present, 256 entries -> 768 bytes
        animated.push(0xff); // background colour index
        animated.push(0x31); // pixel aspect ratio: 1:1, NOT "unspecified"
        animated.extend(printable_payload(14).into_iter().take(768));
        animated.extend_from_slice(&[0x21, 0xff]); // NETSCAPE2.0 loop extension
        animated.extend(printable_payload(20));
        assert_eq!(animated[781], 0x21, "the first block must land at 13 + 768");

        // screen capture: GCT flag clear, background index still 255.
        let mut no_table: Vec<u8> = b"GIF89a".to_vec();
        no_table.extend_from_slice(&2184u16.to_le_bytes());
        no_table.extend_from_slice(&1280u16.to_le_bytes());
        no_table.push(0x70); // GCT flag CLEAR
        no_table.push(0xff); // background colour index, spec says 0
        no_table.push(0x00);
        no_table.extend_from_slice(&[0x21, 0xff]);
        no_table.extend(printable_payload(20));

        for (label, bytes) in [("animated-loop", animated), ("no-global-table", no_table)] {
            let p = Path::new("capture.gif");
            let sn = sniff_bytes(&bytes, p, p, false).unwrap();
            assert_eq!(
                (sn.family, sn.binary_kind.as_deref()),
                (Family::Binary, Some("gif")),
                "{label}: a real GIF header was refused by its qualifier and \
                 fell through to the text heuristics as {:?}/{:?} — this is \
                 #379 inverted, an image turned into prose records",
                sn.family,
                sn.binary_kind
            );
        }
    }

    /// The same true negative for BMP. `bfReserved1/2` are specified as zero,
    /// but a bitmap converted from a CUR or ICO carries the cursor hot-spot
    /// there; OS/2 2.x BITMAPCOREHEADER2 is legally any size in `16..=64`,
    /// while the closed set round 1 used admitted only five of those 49.
    /// Bodies are printable, so a refusal here means `TxtProse`, not a lucky
    /// save by the control-character ratio.
    #[test]
    fn real_bmp_shapes_the_closed_set_refused() {
        fn bmp(dib: u32, reserved: [u8; 4], bits: u16) -> Vec<u8> {
            let mut v: Vec<u8> = b"BM".to_vec();
            v.extend_from_slice(&(14 + dib + 1024).to_le_bytes()); // bfSize
            v.extend_from_slice(&reserved);
            v.extend_from_slice(&(14 + dib + 1024).to_le_bytes()); // bfOffBits
            v.extend_from_slice(&dib.to_le_bytes());
            v.extend_from_slice(&256i32.to_le_bytes()); // width
            v.extend_from_slice(&256i32.to_le_bytes()); // height
            v.extend_from_slice(&1u16.to_le_bytes()); // colour planes
            v.extend_from_slice(&bits.to_le_bytes());
            v.extend(printable_payload(40).into_iter().take(dib as usize - 16));
            v.extend(printable_payload(40));
            v
        }

        for (label, bytes) in [
            // Converted from a cursor: hot-spot coordinates in bfReserved1/2.
            ("cur-hotspot", bmp(40, [16, 0, 16, 0], 8)),
            // OS/2 2.x BITMAPCOREHEADER2, truncated to 24 and 44 bytes — both
            // legal, neither in the closed set round 1 shipped.
            ("os2-dib-24", bmp(24, [0, 0, 0, 0], 8)),
            ("os2-dib-44", bmp(44, [0, 0, 0, 0], 24)),
        ] {
            let p = Path::new("logo.bmp");
            let sn = sniff_bytes(&bytes, p, p, false).unwrap();
            assert_eq!(
                (sn.family, sn.binary_kind.as_deref()),
                (Family::Binary, Some("bmp")),
                "{label}: a real BMP header was refused by its qualifier and \
                 fell through to the text heuristics as {:?}/{:?}",
                sn.family,
                sn.binary_kind
            );
        }
    }

    /// The third instance of the same shape, found by sweeping the build
    /// machine while checking the GIF rule: `riff_form`'s closed set of FORM
    /// types listed `ANI `, which is the animated-cursor file EXTENSION and
    /// not its FORM type. The FORM type is `ACON`, and all 12 real `.ani`
    /// files here were refused. They still reached `Family::Binary` through
    /// the heuristics — 13.68% and 15.72% control characters against a 10%
    /// threshold, and one at 90.99% NULs — so the measured cost was the
    /// specific `binary_kind`, not records; the margin is the same kind of
    /// luck the GIF rule was relying on.
    #[test]
    fn a_riff_form_type_is_the_form_not_the_extension() {
        let mut ani: Vec<u8> = b"RIFF".to_vec();
        ani.extend_from_slice(&4096u32.to_le_bytes());
        ani.extend_from_slice(b"ACON");
        ani.extend(printable_payload(20));
        let p = Path::new("wait.ani");
        let sn = sniff_bytes(&ani, p, p, false).unwrap();
        assert_eq!(
            (sn.family, sn.binary_kind.as_deref()),
            (Family::Binary, Some("riff")),
            "an animated cursor declares FORM type ACON; `ANI ` is the \
             extension and no encoder writes it into the container"
        );
    }

    /// The other half of the same trade: widening the qualifiers must not
    /// hand #379 back. The GIF block introducers are `0x21`, `0x2C` and
    /// `0x3B` — `!`, `,` and `;`, all printable — so "walk to the first block
    /// and check the introducer", which is the obvious rule and the one this
    /// fix was first asked for, junks ordinary sentences that happen to put
    /// punctuation at offset 13. Each fixture below does exactly that.
    ///
    /// This is the repository's "a fix that reintroduces the very class it
    /// fixes", caught in the same file it would have been reintroduced in.
    #[test]
    fn prose_that_lands_a_block_introducer_at_the_right_offset_is_still_text() {
        for (label, opener) in [
            // offset 13 is ',' -> 0x2C, an Image Descriptor introducer
            (
                "comma",
                "GIF89a header, the six bytes at the front of every GIF file. ",
            ),
            // offset 13 is '!' -> 0x21, an Extension introducer
            (
                "bang",
                "GIF89a images! They are still everywhere on the web today. ",
            ),
            // offset 13 is ';' -> 0x3B, the Trailer
            (
                "semicolon",
                "GIF87a format; the original release, superseded two years on. ",
            ),
        ] {
            let text = opener.repeat(30);
            assert_eq!(
                text.as_bytes()[13],
                match label {
                    "comma" => 0x2c,
                    "bang" => 0x21,
                    _ => 0x3b,
                },
                "{label}: fixture no longer lands an introducer at offset 13, \
                 so it stopped testing anything — fix the sentence"
            );
            let p = Path::new("notes.txt");
            let sn = sniff_bytes(text.as_bytes(), p, p, false).unwrap();
            assert_eq!(
                sn.family,
                Family::TxtProse,
                "{label}: prose was junked as {:?} — the block introducer is \
                 printable, so it cannot be the whole qualifier (#379)",
                sn.binary_kind
            );
        }
    }

    /// The invariant itself, walked over every row of `MAGIC_TABLE` rather than
    /// asserted about the two rows this issue happened to name: a signature
    /// made only of printable ASCII must REFUSE a prefix that is that signature
    /// followed by ordinary prose. A new row wired to `accept` fails here.
    ///
    /// The counter is not decoration — without it a table whose printable rows
    /// all disappeared would pass this test by iterating over nothing.
    ///
    /// SCOPE: `MAGIC_TABLE` only. Three signatures are matched earlier, as
    /// early returns in `sniff_bytes` — see
    /// `signatures_matched_before_the_table` below for what that leaves open.
    #[test]
    fn every_printable_signature_carries_a_qualifier() {
        let mut printable_rows = 0;
        for &(magic, kind, qualify) in MAGIC_TABLE {
            let printable = magic.iter().all(|b| b.is_ascii_graphic() || *b == b' ');
            if !printable {
                continue;
            }
            printable_rows += 1;
            let mut sentence = magic.to_vec();
            sentence.extend(
                " is a magic number that identifies a media file format, and this \
                 sentence is about it rather than an instance of it. "
                    .repeat(30)
                    .as_bytes(),
            );
            assert!(
                !qualify(&sentence),
                "{kind}: the printable signature {:?} accepts a prefix that is \
                 that signature followed by prose — it needs a structural \
                 qualifier (#379/#380)",
                String::from_utf8_lossy(magic)
            );
            let p = Path::new("notes.txt");
            let sn = sniff_bytes(&sentence, p, p, false).unwrap();
            assert_ne!(
                sn.family,
                Family::Binary,
                "{kind}: prose opening with {:?} was junked",
                String::from_utf8_lossy(magic)
            );
        }
        assert!(
            printable_rows >= 7,
            "expected the printable signatures (GIF8, BM, 8BPS, RIFF, OggS, \
             fLaC, ID3, Kaydara FBX Binary) to be walked; saw {printable_rows} \
             — did the table lose rows, or did this filter stop matching?"
        );
    }

    /// `%PDF-`, `SQLite format 3\0` and `PK\x03\x04` never reach `MAGIC_TABLE`
    /// — they are early returns in `sniff_bytes` — so the walk above says
    /// nothing about them. Two of the three are safe for the usual reason (a
    /// byte text cannot contain: the NUL after `3`, and `\x03\x04`), and this
    /// pins that.
    ///
    /// `%PDF-` is NOT safe and is the same class as #379/#380: prose opening
    /// `%PDF-` sniffs `Family::Pdf` and is handed to the PDF extractor.
    /// Measured on this branch and filed as #403 rather than widened into this
    /// fix — five characters including `%` and `-` is a far smaller collision
    /// surface than `BM`, and it does not produce #379's silent "binary
    /// content" junk reason. The wrong answer is asserted here on purpose so
    /// #403 has to come to this file and flip it.
    #[test]
    fn signatures_matched_before_the_table() {
        for (label, opener, want) in [
            (
                "sqlite",
                "SQLite format 3 is the string at the start of every database file. ",
                Family::TxtProse,
            ),
            (
                "zip",
                "PK is the local file header signature of the ZIP format. ",
                Family::TxtProse,
            ),
            // #403: this one is wrong. It must read TxtProse when #403 lands.
            (
                "pdf",
                "%PDF- is the five byte header every PDF file begins with. ",
                Family::Pdf,
            ),
        ] {
            let text = opener.repeat(30);
            let p = Path::new("notes.txt");
            let sn = sniff_bytes(text.as_bytes(), p, p, false).unwrap();
            assert_eq!(
                sn.family, want,
                "{label}: prose opening with a pre-table signature classified \
                 {:?}/{:?}",
                sn.family, sn.binary_kind
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(s: &str) -> Family {
        let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
        classify_text(s, &lines)
    }

    #[test]
    fn sniff_families() {
        assert_eq!(classify("{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n"), Family::Jsonl);
        assert_eq!(
            classify("{\n  \"a\": 1,\n  \"b\": [1,2]\n}\n"),
            Family::Json
        );
        assert_eq!(
            classify("<!DOCTYPE html>\n<html><head></head></html>"),
            Family::Html
        );
        assert_eq!(
            classify("<?xml version='1.0'?>\n<r><a>1</a></r>"),
            Family::Xml
        );
        assert_eq!(classify("a,b,c\n1,2,3\n4,5,6\n"), Family::Csv);
        assert_eq!(
            classify("CREATE TABLE `t` (\n `a` int\n);\nINSERT INTO `t` VALUES (1);\n"),
            Family::SqlDump
        );
        assert_eq!(
            classify("key: value\nother: 1\nnested:\n  a: 2\n"),
            Family::Yaml
        );
    }

    /// `[package]`-style openers are TOML/INI table headers, not JSON
    /// arrays. Before the guard, Cargo.toml sniffed as Json, the JSON
    /// extractor junked it, and cratecite's crate table stayed empty on
    /// every real repository (its dst must be an INDEXED Cargo.toml).
    #[test]
    fn toml_table_header_is_not_json() {
        let cargo = "[package]\nname = \"xerj-fts\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                     [dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\n\
                     serde_json = \"1\"\nanyhow = \"1\"\n";
        assert_eq!(classify(cargo), Family::TxtLines);
        let ini = "[Unit]\nDescription=demo\nAfter=network.target\n\n\
                   [Service]\nExecStart=/bin/true\nRestart=always\n\n\
                   [Install]\nWantedBy=multi-user.target\n";
        assert_eq!(classify(ini), Family::TxtLines);
        // …while real JSON arrays keep their family.
        assert_eq!(classify("[1, 2, 3]\n"), Family::Json);
        assert_eq!(
            classify("[\n  {\"a\": 1},\n  {\"a\": 2}\n]\n"),
            Family::Json
        );
    }

    #[test]
    fn csv_dialect_semicolon_decimal_comma() {
        let lines = vec![
            "geraet;zeitpunkt;temperatur_c",
            "dev-1;2026-03-09T02:09:26Z;50,6",
            "dev-2;2026-03-10T19:10:36Z;57,0",
        ];
        let d = sniff_csv_dialect(&lines).unwrap();
        assert_eq!(d.delim, b';');
        assert!(d.has_header);
        assert!(d.decimal_comma);
    }
}

#[cfg(test)]
mod text_family_tests {
    use super::*;

    fn kind(text: &str) -> Family {
        let nonblank: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        txt_kind(&nonblank)
    }

    /// Full text classifier — access logs and syslog are claimed by the `Logs`
    /// family before `txt_kind` is ever consulted, so they must be asserted
    /// through the real entry point rather than against `txt_kind` directly.
    fn classify_full(text: &str) -> Family {
        let nonblank: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        classify_text(text, &nonblank)
    }

    /// Regression: a markdown document with `## Headings` averages ~50 chars
    /// over 7 lines, which the length-only rule classified as TxtLines — the
    /// same corpus's 5-line runbook (avg 59) went to TxtProse. Same content
    /// type, two families, two field names, incomparable BM25 statistics.
    #[test]
    fn markdown_with_headings_is_prose() {
        let md = "# Postmortem: checkout outage, 14 June 2026\n\n\
                  ## Impact\n\
                  Checkout was unavailable for 51 minutes.\n\n\
                  ## Root cause\n\
                  The payment gateway TLS certificate expired.\n\n\
                  ## Resolution\n\
                  We reloaded the service and added an alert.\n";
        assert_eq!(kind(md), Family::TxtProse);
    }

    #[test]
    fn short_runbook_is_still_prose() {
        let md = "# Database runbook\n\n\
                  ## Failover\n\
                  Promote the standby with pg_ctl promote.\n\n\
                  ## Pool exhaustion\n\
                  Symptoms are rising p99 and pool errors in the logs.\n";
        assert_eq!(kind(md), Family::TxtProse);
    }

    /// The record-stream side must be unaffected — these are what TxtLines is for.
    #[test]
    fn access_logs_stay_line_records() {
        let log = (0..20)
            .map(|i| format!(
                "10.0.0.{i} - - [01/Jun/2026:10:00:00 +0000] \"GET /api/checkout HTTP/1.1\" 200 {i}00 \"-\" \"Mozilla/5.0\""
            ))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(classify_full(&log), Family::Logs);
    }

    #[test]
    fn syslog_stays_line_records() {
        // One message in five ends with a period — well under the threshold.
        let msgs = [
            "sshd[123]: Accepted publickey for deploy from 10.0.3.4 port 55212",
            "kernel: Out of memory: Killed process 8123 (java)",
            "cron[99]: session opened for user root by (uid=0)",
            "postfix[7]: connection timed out while talking to upstream",
            "systemd[1]: Started Daily apt download activities.",
        ];
        let log = (0..6)
            .flat_map(|_| msgs.iter().map(|m| format!("Jun  1 10:00:00 host1 {m}")))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(classify_full(&log), Family::Logs);
    }

    #[test]
    fn source_code_stays_line_records() {
        let code = "pub struct Pool { max: usize, in_use: usize }\n\
                    impl Pool {\n\
                    pub fn acquire(&mut self) -> Result<Conn, PoolError> {\n\
                    if self.in_use >= self.max { return Err(PoolError::Exhausted); }\n\
                    self.in_use += 1;\n\
                    Ok(Conn::new())\n\
                    }\n\
                    }\n\
                    fn helper() -> u32 { 42 }\n\
                    const LIMIT: usize = 10;\n";
        assert_eq!(kind(code), Family::TxtLines);
    }

    #[test]
    fn long_lines_are_prose_regardless_of_punctuation() {
        let t = (0..10)
            .map(|_| "x".repeat(120))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(kind(&t), Family::TxtProse);
    }

    /// Regression: a markdown checklist (`- [ ]` task items under a heading)
    /// used to sniff as YAML — 2 of 3 nonblank lines start with `- ` — and
    /// the YAML extractor then junk-filed it (and, before the yaml_x
    /// non-progress fix, hung on it). Checkbox items are invalid YAML and
    /// must not count as YAML evidence.
    #[test]
    fn markdown_checklist_is_prose_not_yaml() {
        let md = "# Launch checklist\n\n\
                  - [ ] Sign off the [business plan](01-business-plan.md)\n\
                  - [x] Close out permits\n\
                  - [ ] Dry run: two full batches back to back\n";
        assert_eq!(classify_full(md), Family::TxtProse);
        // A real YAML list is still YAML.
        let yaml = "- alpha\n- beta\n- gamma\n";
        assert_eq!(classify_full(yaml), Family::Yaml);
    }

    /// Regression (second-brain demo vault, live 2026-07-30): a markdown
    /// note hard-wrapped at ~75 columns averages < 60 chars/line and ends
    /// most lines mid-sentence, so it scored below the 0.40 sentence ratio
    /// and landed in TxtLines — silently losing its title, its `s0` section
    /// anchor, and its wikilink detection (8 of 39 vault files). A file that
    /// opens with an ATX heading and shows markdown link syntax (or some
    /// terminal punctuation) is markdown prose.
    #[test]
    fn hard_wrapped_markdown_note_is_prose_not_lines() {
        let md = "# Hydration\n\n\
                  Hydration is water as a percentage of flour weight, the core of\n\
                  [[baker-percentages]]. 65% is a tight sandwich loaf; 75-80% is where open\n\
                  [[crumb-structure]] lives; past 85% the dough demands real skill.\n\n\
                  Higher hydration = looser dough, longer bulk, bigger holes. It is the single\n\
                  most consequential number in the formula.\n";
        assert_eq!(classify_full(md), Family::TxtProse);
        // No heading opener → the rescue must not fire: a Python file whose
        // comment banner starts mid-file stays wherever the base heuristics
        // put it, and a shebang is not a heading.
        let sh = "#!/usr/bin/env bash\n\
                  set -euo pipefail\n\
                  for f in a b c; do\n\
                  echo one\n\
                  echo two\n\
                  echo three\n\
                  done\n";
        assert_eq!(classify_full(sh), Family::TxtLines);
    }
}
