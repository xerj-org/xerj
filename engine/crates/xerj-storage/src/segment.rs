//! Segment file format: `.seg` (data) + `.sidx` (skip index)
//!
//! ## `.seg` file layout
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │  Header  (64 bytes)                                              │
//! │    magic           [u8;4]   = "XAYA"                            │
//! │    format_version  u16      (currently 1)                       │
//! │    schema_version  u32                                           │
//! │    doc_count       u64                                           │
//! │    min_seq_no      u64                                           │
//! │    max_seq_no      u64                                           │
//! │    created_at_ms   u64      (unix epoch milliseconds)           │
//! │    flags           u16      (bit 0 = has_tombstones)            │
//! │    checksum_algo   u8       (0 = crc32c)                        │
//! │    _pad            [u8;9]                                        │
//! ├─────────────────────────────────────────────────────────────────┤
//! │  Section table length  u32                                       │
//! │  Section table entries (repeating, 25 bytes each)               │
//! │    section_type    u8                                            │
//! │    offset          u64                                           │
//! │    length          u64                                           │
//! │    crc32c          u32                                           │
//! │    _reserved       u4                                            │
//! ├─────────────────────────────────────────────────────────────────┤
//! │  Section data (arbitrary order, addressed by section table)      │
//! ├─────────────────────────────────────────────────────────────────┤
//! │  Footer  (32 bytes)                                              │
//! │    section_table_offset  u64                                     │
//! │    total_crc32c          u32  (over everything except footer)   │
//! │    _pad                  [u8;16]                                 │
//! │    magic_end             [u8;4]   = "BEEZ"                      │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## `.sidx` file layout
//!
//! The skip index is *intended* to map logical doc-position ranges to file
//! offsets for fast random access.  Its on-disk form is a sorted list of
//! (doc_ordinal, offset) pairs written as little-endian u64 pairs.
//!
//! **Status: written but not yet consumed on the read path.**  Every
//! flush/merge emits a `.sidx` side-car (see [`SegmentWriter::finish`]), but
//! no reader opens it — [`SegmentReader`] addresses sections purely through
//! the in-file section table, and random access to stored fields is already
//! served from an in-memory offset cache (the engine's `stored_slices_cache`).
//! The on-disk skip index is therefore currently redundant; it is retained
//! for format completeness and forward compatibility.  Wiring it into the
//! read path (so cold random access need not rebuild offsets in memory) is
//! future work.

use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use crc32fast::Hasher as Crc32Hasher;
use memmap2::Mmap;
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};
use uuid::Uuid;

use crate::{Result, SeqNo, StorageError};

// ── Constants ─────────────────────────────────────────────────────────────────

const SEG_MAGIC: &[u8; 4] = b"XAYA";
const SEG_MAGIC_END: &[u8; 4] = b"BEEZ";
const FORMAT_VERSION: u16 = 1;
const HEADER_LEN: u64 = 64;
const FOOTER_LEN: u64 = 32;
const SECTION_ENTRY_LEN: u64 = 25; // type(1)+offset(8)+length(8)+crc(4)+reserved(4)

// ── SegmentId ─────────────────────────────────────────────────────────────────

/// Unique identifier for a segment — a UUIDv4 string.
pub type SegmentId = String;

pub fn new_segment_id() -> SegmentId {
    Uuid::new_v4().to_string()
}

// ── SectionType ───────────────────────────────────────────────────────────────

/// Identifies the kind of data stored in a segment section.
///
/// Unknown section types are **skipped** (forward compatibility: a reader built
/// against an older version of the format will silently skip sections it does
/// not understand).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SectionType {
    /// Full-text search postings lists.
    Fts = 0x01,
    /// Dense float32 vectors for ANN search.
    Vectors = 0x02,
    /// Column-store (doc-values) for sorting / aggregations.
    Columns = 0x03,
    /// Stored fields: original source JSON.
    Stored = 0x04,
    /// Tombstone bitset (roaring bitmap of deleted doc ordinals).
    Tombstones = 0x05,
    /// Schema snapshot used when writing the segment.
    Schema = 0x06,
    /// Numeric point values (1-D BKD trees) for fast range queries.
    /// One BKD tree per indexed numeric field.
    Points = 0x07,
    /// Unknown — read by older versions; will be skipped.
    #[serde(other)]
    Unknown = 0xFF,
}

impl SectionType {
    fn from_u8(v: u8) -> Self {
        match v {
            0x01 => Self::Fts,
            0x02 => Self::Vectors,
            0x03 => Self::Columns,
            0x04 => Self::Stored,
            0x05 => Self::Tombstones,
            0x06 => Self::Schema,
            0x07 => Self::Points,
            _ => Self::Unknown,
        }
    }
}

// ── Seq-aware tombstone section codec (RC4 W2 #14) ───────────────────────────
//
// `SectionType::Tombstones` payload, version 2:
//
//   "ZTB2"   4 bytes magic
//   u32      pair count
//   lz4_flex::compress_prepend_size(body) where body repeats:
//     u64  delete seq_no (LE)
//     u16  id_len (LE)
//     id_len bytes UTF-8 doc_id
//
// V2 makes acked deletes SEGMENT-DURABLE: reopen applies these pairs
// max-seq-wins against the doc entries rebuilt from `.ids`/stored, so a
// delete survives restart without its WAL entry — which is what finally
// lets WAL maintenance unpin delete-bearing shards (the pre-fix pinning
// retained one WAL generation per shard FOREVER after a single plain
// delete).  The legacy payload (a bare JSON array of doc_id strings,
// written since the 2026-07 delete-durability fix but never read) has no
// seq information and is IGNORED by `decode_tombstones_v2` — those
// deletes remain protected by WAL pinning exactly as before.

/// Encode `(delete_seq_no, doc_id)` pairs as a ZTB2 tombstone section.
pub fn encode_tombstones_v2(pairs: &[(u64, &str)]) -> Vec<u8> {
    let mut body: Vec<u8> =
        Vec::with_capacity(pairs.iter().map(|(_, id)| 8 + 2 + id.len()).sum::<usize>());
    for (seq, id) in pairs {
        body.extend_from_slice(&seq.to_le_bytes());
        body.extend_from_slice(&(id.len() as u16).to_le_bytes());
        body.extend_from_slice(id.as_bytes());
    }
    let compressed = lz4_flex::compress_prepend_size(&body);
    let mut out = Vec::with_capacity(8 + compressed.len());
    out.extend_from_slice(b"ZTB2");
    out.extend_from_slice(&(pairs.len() as u32).to_le_bytes());
    out.extend_from_slice(&compressed);
    out
}

/// Decode a ZTB2 tombstone section.  Returns `None` for the legacy
/// id-only JSON payload (no seq info — cannot be applied safely) or any
/// malformed input.
pub fn decode_tombstones_v2(bytes: &[u8]) -> Option<Vec<(u64, String)>> {
    if bytes.len() < 8 || &bytes[..4] != b"ZTB2" {
        return None;
    }
    let count = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
    let body = lz4_flex::decompress_size_prepended(&bytes[8..]).ok()?;
    let mut out = Vec::with_capacity(count);
    let mut pos = 0usize;
    for _ in 0..count {
        if pos + 10 > body.len() {
            return None;
        }
        let seq = u64::from_le_bytes(body[pos..pos + 8].try_into().ok()?);
        pos += 8;
        let id_len = u16::from_le_bytes(body[pos..pos + 2].try_into().ok()?) as usize;
        pos += 2;
        if pos + id_len > body.len() {
            return None;
        }
        let id = std::str::from_utf8(&body[pos..pos + id_len])
            .ok()?
            .to_string();
        pos += id_len;
        out.push((seq, id));
    }
    Some(out)
}

// ── SegmentHeader ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SegmentHeader {
    pub format_version: u16,
    pub schema_version: u32,
    pub doc_count: u64,
    pub min_seq_no: SeqNo,
    pub max_seq_no: SeqNo,
    pub created_at_ms: u64,
    pub flags: u16,
    pub checksum_algo: u8,
}

impl SegmentHeader {
    fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(SEG_MAGIC)?;
        w.write_u16::<LittleEndian>(self.format_version)?;
        w.write_u32::<LittleEndian>(self.schema_version)?;
        w.write_u64::<LittleEndian>(self.doc_count)?;
        w.write_u64::<LittleEndian>(self.min_seq_no)?;
        w.write_u64::<LittleEndian>(self.max_seq_no)?;
        w.write_u64::<LittleEndian>(self.created_at_ms)?;
        w.write_u16::<LittleEndian>(self.flags)?;
        w.write_u8(self.checksum_algo)?;
        // Padding: magic(4)+ver(2)+schema_ver(4)+doc_count(8)+min_seq(8)+max_seq(8)
        //          +created_at(8)+flags(2)+checksum_algo(1) = 45 bytes → need 19 more for 64
        w.write_all(&[0u8; 19])?;
        Ok(())
    }

    fn read_from(data: &[u8]) -> Result<Self> {
        use std::io::Cursor;
        let mut r = Cursor::new(data);

        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        if &magic != SEG_MAGIC {
            return Err(StorageError::InvalidMagic {
                expected: SEG_MAGIC,
                actual: magic.to_vec(),
            });
        }

        let format_version = r.read_u16::<LittleEndian>()?;
        if format_version != FORMAT_VERSION {
            return Err(StorageError::UnsupportedVersion(format_version));
        }

        let schema_version = r.read_u32::<LittleEndian>()?;
        let doc_count = r.read_u64::<LittleEndian>()?;
        let min_seq_no = r.read_u64::<LittleEndian>()?;
        let max_seq_no = r.read_u64::<LittleEndian>()?;
        let created_at_ms = r.read_u64::<LittleEndian>()?;
        let flags = r.read_u16::<LittleEndian>()?;
        let checksum_algo = r.read_u8()?;

        Ok(Self {
            format_version,
            schema_version,
            doc_count,
            min_seq_no,
            max_seq_no,
            created_at_ms,
            flags,
            checksum_algo,
        })
    }
}

// ── SectionEntry ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct SectionEntry {
    section_type: SectionType,
    offset: u64,
    length: u64,
    crc32c: u32,
}

impl SectionEntry {
    fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_u8(self.section_type as u8)?;
        w.write_u64::<LittleEndian>(self.offset)?;
        w.write_u64::<LittleEndian>(self.length)?;
        w.write_u32::<LittleEndian>(self.crc32c)?;
        w.write_u32::<LittleEndian>(0)?; // reserved
        Ok(())
    }

    fn read_from(r: &mut impl io::Read) -> io::Result<Self> {
        let t = r.read_u8()?;
        let offset = r.read_u64::<LittleEndian>()?;
        let length = r.read_u64::<LittleEndian>()?;
        let crc32c = r.read_u32::<LittleEndian>()?;
        let _reserved = r.read_u32::<LittleEndian>()?;
        Ok(Self {
            section_type: SectionType::from_u8(t),
            offset,
            length,
            crc32c,
        })
    }
}

// ── SegmentMeta ───────────────────────────────────────────────────────────────

/// Lightweight metadata kept in memory for a segment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentMeta {
    pub id: SegmentId,
    pub doc_count: u64,
    pub size_bytes: u64,
    pub min_seq_no: SeqNo,
    pub max_seq_no: SeqNo,
    pub created_at_ms: u64,
    pub has_tombstones: bool,
    /// Relative path of the `.seg` file from the index root.
    pub seg_path: String,
    /// Relative path of the `.sidx` file from the index root.
    pub sidx_path: String,
}

// ── SegmentWriter ─────────────────────────────────────────────────────────────

/// Builds a segment incrementally and writes `.seg` + `.sidx` atomically.
///
/// ## Usage
///
/// ```rust,no_run
/// # use xerj_storage::segment::{SegmentWriter, SectionType};
/// # let dir: std::path::PathBuf = "/tmp/idx".into();
/// let mut w = SegmentWriter::new(&dir, 0, 0, 1).unwrap();
/// w.add_section(SectionType::Stored, b"{\"title\":\"hello\"}").unwrap();
/// let meta = w.finish(1 /* doc_count */, 1 /* min_seq */, 1 /* max_seq */).unwrap();
/// ```
pub struct SegmentWriter {
    dir: PathBuf,
    id: SegmentId,
    schema_version: u32,
    sections: Vec<(SectionType, Vec<u8>)>,
    created_at_ms: u64,
}

impl SegmentWriter {
    /// Create a new writer that will produce files in `dir`.
    pub fn new(
        dir: impl AsRef<Path>,
        schema_version: u32,
        _generation: u64,
        _shard: u32,
    ) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let created_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Ok(Self {
            dir,
            id: new_segment_id(),
            schema_version,
            sections: Vec::new(),
            created_at_ms,
        })
    }

    /// Add a named section with raw bytes.
    pub fn add_section(&mut self, kind: SectionType, data: impl AsRef<[u8]>) -> Result<()> {
        self.sections.push((kind, data.as_ref().to_vec()));
        Ok(())
    }

    /// Finalise, write, and return metadata.
    ///
    /// Files are written to tmp paths then `rename`d so crashes leave no
    /// partial state.
    #[instrument(skip(self), fields(id = %self.id, doc_count, min_seq_no, max_seq_no))]
    pub fn finish(
        &self,
        doc_count: u64,
        min_seq_no: SeqNo,
        max_seq_no: SeqNo,
    ) -> Result<SegmentMeta> {
        let has_tombstones = self
            .sections
            .iter()
            .any(|(t, _)| *t == SectionType::Tombstones);
        let flags: u16 = if has_tombstones { 0x0001 } else { 0x0000 };

        let header = SegmentHeader {
            format_version: FORMAT_VERSION,
            schema_version: self.schema_version,
            doc_count,
            min_seq_no,
            max_seq_no,
            created_at_ms: self.created_at_ms,
            flags,
            checksum_algo: 0, // crc32c
        };

        // Build the section data buffer and section table
        let mut section_data: Vec<u8> = Vec::new();
        let mut section_entries: Vec<SectionEntry> = Vec::with_capacity(self.sections.len());

        // Section data starts after: header(64) + section_table_len(4) + N*25
        // We'll fix up the offset after we know N
        let section_table_len_bytes = 4u64 + self.sections.len() as u64 * SECTION_ENTRY_LEN;
        let data_start = HEADER_LEN + section_table_len_bytes;

        let mut cursor = data_start;
        for (kind, data) in &self.sections {
            let mut hasher = Crc32Hasher::new();
            hasher.update(data);
            let crc = hasher.finalize();
            section_entries.push(SectionEntry {
                section_type: *kind,
                offset: cursor,
                length: data.len() as u64,
                crc32c: crc,
            });
            section_data.extend_from_slice(data);
            cursor += data.len() as u64;
        }

        // Compute where the section table itself will sit (right after header)
        let section_table_offset = HEADER_LEN;
        let section_data_start = data_start;
        let _ = section_data_start; // used in offset calculation above

        // Compute footer position
        let _footer_offset = data_start + section_data.len() as u64;

        // Now assemble the full file in memory for CRC
        let mut buf: Vec<u8> = Vec::new();
        header.write_to(&mut buf)?;

        // Section table: count(u32) + entries
        let mut section_table_buf: Vec<u8> = Vec::new();
        section_table_buf.write_u32::<LittleEndian>(self.sections.len() as u32)?;
        for entry in &section_entries {
            entry.write_to(&mut section_table_buf)?;
        }
        buf.extend_from_slice(&section_table_buf);
        buf.extend_from_slice(&section_data);

        // Total CRC32C over everything except footer
        let mut total_hasher = Crc32Hasher::new();
        total_hasher.update(&buf);
        let total_crc = total_hasher.finalize();

        // Write footer (32 bytes)
        buf.write_u64::<LittleEndian>(section_table_offset)?;
        buf.write_u32::<LittleEndian>(total_crc)?;
        buf.write_all(&[0u8; 16])?; // padding
        buf.write_all(SEG_MAGIC_END)?;

        // Write .seg file
        let seg_filename = format!("{}.seg", self.id);
        let seg_path = self.dir.join(&seg_filename);
        let seg_tmp = seg_path.with_extension("tmp");
        {
            let mut f = File::create(&seg_tmp)?;
            f.write_all(&buf)?;
            f.sync_all()?;
        }
        std::fs::rename(&seg_tmp, &seg_path)?;

        // Write .sidx side-car (simple: N*(doc_ordinal:u64, offset:u64) pairs).
        // NOTE: emitted for format completeness only — the read path does NOT
        // consume it (see the module-level ".sidx file layout" note).
        // `SegmentReader` addresses sections via the in-file section table, and
        // random access to stored fields is served from the engine's in-memory
        // `stored_slices_cache`, so no reader ever opens this file.
        let sidx_filename = format!("{}.sidx", self.id);
        let sidx_path = self.dir.join(&sidx_filename);
        let sidx_tmp = sidx_path.with_extension("tmp");
        {
            let mut f = BufWriter::new(File::create(&sidx_tmp)?);
            // Minimal skip index: one entry pointing to the stored section
            if let Some(stored) = section_entries
                .iter()
                .find(|e| e.section_type == SectionType::Stored)
            {
                f.write_u64::<LittleEndian>(0)?; // first doc ordinal
                f.write_u64::<LittleEndian>(stored.offset)?; // file offset
            }
            f.flush()?;
            f.get_ref().sync_all()?;
        }
        std::fs::rename(&sidx_tmp, &sidx_path)?;

        // RC4 W1 #10 — make the renames themselves power-loss durable.
        // The file CONTENTS were fsynced above, but a rename only becomes
        // durable when the parent directory is fsynced; without this a
        // power loss could leave a fully-written segment invisible in the
        // directory while the WAL entries it covers were already pruned.
        xerj_common::fsio::fsync_dir(&self.dir)?;

        debug!(id = %self.id, size = buf.len(), doc_count, "segment written");

        Ok(SegmentMeta {
            id: self.id.clone(),
            doc_count,
            size_bytes: buf.len() as u64,
            min_seq_no,
            max_seq_no,
            created_at_ms: self.created_at_ms,
            has_tombstones,
            seg_path: seg_filename,
            sidx_path: sidx_filename,
        })
    }
}

// ── SegmentReader ─────────────────────────────────────────────────────────────

/// Memory-mapped segment reader.
///
/// The OS page cache backs the reads — no heap copies for the common path.
/// Section data is validated on first access (CRC32C).
pub struct SegmentReader {
    mmap: Arc<Mmap>,
    header: SegmentHeader,
    sections: Vec<SectionEntry>,
}

impl SegmentReader {
    /// Open the `.seg` file at `path` and validate the header + footer.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(path.as_ref())?;
        let mmap = unsafe { Mmap::map(&file) }?;
        let mmap = Arc::new(mmap);
        Self::from_mmap(mmap)
    }

    /// Return the underlying Arc<Mmap>.  Used by the cached
    /// `open_segment_arc` path to clone SegmentReaders without
    /// reopening the file.
    pub fn mmap_arc(&self) -> &Arc<Mmap> {
        &self.mmap
    }

    /// Construct a SegmentReader from an already-validated Arc<Mmap>.
    /// Bypasses the full-file CRC check — only used from the
    /// `open_segment_arc` cache path where the same mmap has already
    /// been validated once at cache insertion time.
    pub fn from_mmap_arc(mmap: Arc<Mmap>) -> Result<Self> {
        // Re-parse the section table without redoing the total CRC.
        // The mmap came from a reader that already validated at open.
        let data = &mmap[..];
        let footer_start = data.len() - FOOTER_LEN as usize;
        let mut footer_cur = std::io::Cursor::new(&data[footer_start..]);
        let section_table_offset = footer_cur.read_u64::<LittleEndian>()?;
        let _stored_total_crc = footer_cur.read_u32::<LittleEndian>()?;
        let header = SegmentHeader::read_from(&data[..HEADER_LEN as usize])?;
        let st_start = section_table_offset as usize;
        let mut st_cur = std::io::Cursor::new(&data[st_start..]);
        let section_count = st_cur.read_u32::<LittleEndian>()? as usize;
        let mut sections = Vec::with_capacity(section_count);
        for _ in 0..section_count {
            sections.push(SectionEntry::read_from(&mut st_cur)?);
        }
        Ok(Self {
            mmap,
            header,
            sections,
        })
    }

    fn from_mmap(mmap: Arc<Mmap>) -> Result<Self> {
        let data = &mmap[..];
        if data.len() < (HEADER_LEN + FOOTER_LEN) as usize {
            return Err(StorageError::WalCorrupt(0, "segment file too small".into()));
        }

        // Validate footer magic
        let footer_start = data.len() - FOOTER_LEN as usize;
        let magic_end_offset = data.len() - 4;
        if &data[magic_end_offset..] != SEG_MAGIC_END {
            return Err(StorageError::InvalidMagic {
                expected: SEG_MAGIC_END,
                actual: data[magic_end_offset..].to_vec(),
            });
        }

        // Read footer
        let mut footer_cur = std::io::Cursor::new(&data[footer_start..]);
        let section_table_offset = footer_cur.read_u64::<LittleEndian>()?;
        let stored_total_crc = footer_cur.read_u32::<LittleEndian>()?;

        // Verify total CRC (everything except footer)
        let mut hasher = Crc32Hasher::new();
        hasher.update(&data[..footer_start]);
        let computed_crc = hasher.finalize();
        if stored_total_crc != computed_crc {
            return Err(StorageError::ChecksumMismatch {
                expected: stored_total_crc,
                actual: computed_crc,
            });
        }

        // Parse header
        let header = SegmentHeader::read_from(&data[..HEADER_LEN as usize])?;

        // Parse section table
        let st_start = section_table_offset as usize;
        let mut st_cur = std::io::Cursor::new(&data[st_start..]);
        let section_count = st_cur.read_u32::<LittleEndian>()? as usize;
        let mut sections = Vec::with_capacity(section_count);
        for _ in 0..section_count {
            sections.push(SectionEntry::read_from(&mut st_cur)?);
        }

        Ok(Self {
            mmap,
            header,
            sections,
        })
    }

    /// Return the segment header.
    pub fn header(&self) -> &SegmentHeader {
        &self.header
    }

    /// Return the raw bytes of a section, or `None` if that section type is
    /// not present in this segment.
    ///
    /// M5.20 — section CRC is NO LONGER re-validated on every call.
    /// The whole-file CRC is validated once at open time (in
    /// `from_mmap`); individual section CRCs were verifying the same
    /// immutable bytes on every query, which was a significant share
    /// of CPU time on concurrent-search benchmarks (197 segments ×
    /// ~500 KB stored = ~100 MB of CRC32 per query, 50 ms per query
    /// just in checksum work).  If you want strict per-section CRC
    /// checking, use `section_checked` below.
    pub fn section(&self, kind: SectionType) -> Result<Option<&[u8]>> {
        let entry = match self.sections.iter().find(|e| e.section_type == kind) {
            Some(e) => e,
            None => return Ok(None),
        };
        let start = entry.offset as usize;
        let end = start + entry.length as usize;
        Ok(Some(&self.mmap[start..end]))
    }

    /// Same as `section()` but re-validates the per-section CRC on
    /// every call.  Useful for `fsck`-style integrity checks; the
    /// fast path uses `section()` which relies on the open-time
    /// whole-file CRC.
    pub fn section_checked(&self, kind: SectionType) -> Result<Option<&[u8]>> {
        let entry = match self.sections.iter().find(|e| e.section_type == kind) {
            Some(e) => e,
            None => return Ok(None),
        };
        let start = entry.offset as usize;
        let end = start + entry.length as usize;
        let slice = &self.mmap[start..end];
        let mut hasher = Crc32Hasher::new();
        hasher.update(slice);
        let crc = hasher.finalize();
        if crc != entry.crc32c {
            return Err(StorageError::ChecksumMismatch {
                expected: entry.crc32c,
                actual: crc,
            });
        }
        Ok(Some(slice))
    }

    /// List all section types present in this segment.
    pub fn section_types(&self) -> Vec<SectionType> {
        self.sections.iter().map(|e| e.section_type).collect()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_read_segment() {
        let dir = tempfile::tempdir().unwrap();

        let mut writer = SegmentWriter::new(dir.path(), 1, 0, 0).unwrap();
        let stored_data = b"{\"title\":\"hello xerj\"}";
        writer
            .add_section(SectionType::Stored, stored_data)
            .unwrap();
        writer
            .add_section(SectionType::Schema, b"{\"fields\":{}}")
            .unwrap();

        let meta = writer.finish(1, 10, 10).unwrap();
        assert_eq!(meta.doc_count, 1);
        assert!(!meta.has_tombstones);

        let seg_path = dir.path().join(&meta.seg_path);
        let reader = SegmentReader::open(&seg_path).unwrap();

        assert_eq!(reader.header().doc_count, 1);
        assert_eq!(reader.header().min_seq_no, 10);

        let stored = reader.section(SectionType::Stored).unwrap().unwrap();
        assert_eq!(stored, stored_data);

        let schema = reader.section(SectionType::Schema).unwrap().unwrap();
        assert_eq!(schema, b"{\"fields\":{}}");

        // Missing section returns None
        assert!(reader.section(SectionType::Fts).unwrap().is_none());
    }

    #[test]
    fn segment_with_tombstones_sets_flag() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = SegmentWriter::new(dir.path(), 1, 0, 0).unwrap();
        writer
            .add_section(SectionType::Tombstones, [0xFFu8; 4])
            .unwrap();
        let meta = writer.finish(10, 1, 10).unwrap();
        assert!(meta.has_tombstones);

        let reader = SegmentReader::open(dir.path().join(&meta.seg_path)).unwrap();
        assert_ne!(reader.header().flags & 0x0001, 0);
    }

    #[test]
    fn crc_mismatch_detected() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = SegmentWriter::new(dir.path(), 1, 0, 0).unwrap();
        writer.add_section(SectionType::Stored, b"data").unwrap();
        let meta = writer.finish(1, 1, 1).unwrap();

        // Corrupt the file
        let seg_path = dir.path().join(&meta.seg_path);
        let mut content = std::fs::read(&seg_path).unwrap();
        let mid = content.len() / 2;
        content[mid] ^= 0xFF;
        std::fs::write(&seg_path, &content).unwrap();

        let result = SegmentReader::open(&seg_path);
        assert!(matches!(result, Err(StorageError::ChecksumMismatch { .. })));
    }

    #[test]
    fn section_types_listed() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = SegmentWriter::new(dir.path(), 1, 0, 0).unwrap();
        writer.add_section(SectionType::Stored, b"s").unwrap();
        writer.add_section(SectionType::Fts, b"f").unwrap();
        let meta = writer.finish(1, 1, 1).unwrap();

        let reader = SegmentReader::open(dir.path().join(&meta.seg_path)).unwrap();
        let types = reader.section_types();
        assert!(types.contains(&SectionType::Stored));
        assert!(types.contains(&SectionType::Fts));
        assert!(!types.contains(&SectionType::Vectors));
    }

    /// Locks the `.sidx` disclosure: `finish()` emits the skip-index
    /// side-car, but the read path does NOT consume it.  `SegmentReader`
    /// addresses every section through the in-file section table, so a
    /// segment reads back fully even after its `.sidx` file is deleted.
    /// If a future change starts reading `.sidx`, this test will fail and
    /// force the module doc-comment to be updated in step.
    #[test]
    fn sidx_is_written_but_read_path_ignores_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = SegmentWriter::new(dir.path(), 1, 0, 0).unwrap();
        let stored_data = b"{\"title\":\"sidx disclosure\"}";
        writer
            .add_section(SectionType::Stored, stored_data)
            .unwrap();
        let meta = writer.finish(1, 1, 1).unwrap();

        // The side-car is written on finish.
        let sidx_path = dir.path().join(&meta.sidx_path);
        assert!(
            sidx_path.exists(),
            "finish() must emit the .sidx side-car for format completeness"
        );

        // Reads must not depend on it: remove the .sidx and re-open.
        std::fs::remove_file(&sidx_path).unwrap();
        let reader = SegmentReader::open(dir.path().join(&meta.seg_path)).unwrap();
        let stored = reader.section(SectionType::Stored).unwrap().unwrap();
        assert_eq!(
            stored, stored_data,
            "the read path must serve stored fields via the section table, \
             not the (absent) .sidx skip index"
        );
    }
}
