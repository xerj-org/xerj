//! FTS inverted index: writer and reader for one segment.
//!
//! ## On-disk layout
//!
//! For each indexed field `<field-component>` a segment produces four files:
//!
//! ```text
//! seg-<id>.<field-component>.fst       — FST term dictionary
//!                              value = byte offset into .post file
//! seg-<id>.<field-component>.post      — concatenated posting lists
//! seg-<id>.<field-component>.meta      — FieldStats + per-term metadata
//! seg-<id>.<field-component>.norms     — encoded per-doc field lengths
//! seg-<id>.fts-layout-v2               — exact marker when components are encoded
//! ```
//!
//! Short portable field names are used literally for on-disk compatibility;
//! all other logical field names map to a bounded digest component.
//!
//! The FST key is the term text (UTF-8 bytes, lexicographically sorted by construction).
//! The FST output value is the byte offset in the `.post` file.
//! `TermPostings` metadata (doc_freq, ttf) is stored in the `.meta` JSON.

use crate::{
    analyzer::AnalyzerRegistry,
    bm25::FieldStats,
    postings::{PostingsWriter, TermPostings},
};
use anyhow::{bail, Context, Result};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use fst::{Map, MapBuilder};
use memmap2::Mmap;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::HashMap,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

/// Longest field name that can remain literal inside an FTS side-car filename.
///
/// The complete filename also carries a segment UUID, separators, an extension,
/// and sometimes `.tmp`. Keeping the user-controlled component at 128 bytes
/// leaves comfortable room below the common 255-byte component limit.
const MAX_LITERAL_FIELD_COMPONENT_BYTES: usize = 128;

/// Namespace for field names that cannot safely be used as portable filesystem
/// components. Literal names in this namespace are encoded too, so a user field
/// cannot alias another field's digest-derived component.
const ENCODED_FIELD_COMPONENT_PREFIX: &str = "__xerj_fts_field_sha256_";

/// Immutable per-segment discriminator for encoded field filename components.
///
/// This name has no FTS side-car extension, so it cannot equal any v1 file:
/// every historical file is `<segment>.<raw-field>.(fst|post|meta|norms)`.
/// It retains the segment prefix so publication manifests, rollback, orphan
/// recovery, and retirement treat it as part of the same artifact family.
const FTS_FILENAME_LAYOUT_V2_MARKER_SUFFIX: &str = "fts-layout-v2";
const FTS_FILENAME_LAYOUT_V2_MARKER_BYTES: &[u8] = b"XERJ_FTS_FILENAME_LAYOUT_V2\n";

fn segment_filename_layout_v2_marker_path(segment_dir: &Path, segment_id: &str) -> PathBuf {
    segment_dir.join(format!(
        "{segment_id}.{FTS_FILENAME_LAYOUT_V2_MARKER_SUFFIX}"
    ))
}

/// Absence means v1/raw. Exact magic means v2/encoded. Any other visible
/// marker is corruption. File-family existence is never layout evidence.
fn segment_uses_encoded_filename_layout(segment_dir: &Path, segment_id: &str) -> Result<bool> {
    let marker = segment_filename_layout_v2_marker_path(segment_dir, segment_id);
    match fs::read(&marker) {
        Ok(bytes) if bytes == FTS_FILENAME_LAYOUT_V2_MARKER_BYTES => Ok(true),
        Ok(_) => bail!("corrupt FTS filename-layout discriminator {:?}", marker),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error)
            .with_context(|| format!("reading FTS filename-layout discriminator {:?}", marker)),
    }
}

fn is_windows_reserved_device_name(field: &str) -> bool {
    let stem = field.split('.').next().unwrap_or(field);
    if ["CON", "PRN", "AUX", "NUL"]
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    {
        return true;
    }
    let bytes = stem.as_bytes();
    bytes.len() == 4
        && (bytes[..3].eq_ignore_ascii_case(b"COM") || bytes[..3].eq_ignore_ascii_case(b"LPT"))
        && matches!(bytes[3], b'1'..=b'9')
}

/// Return the bounded, portable component used for every side-car of `field`.
///
/// Existing short field names that are portable across supported filesystems
/// remain byte-for-byte compatible with the historical on-disk layout.
/// Everything else is represented by a SHA-256 digest. In particular, path
/// separators, traversal-like names, platform-reserved punctuation, controls,
/// and overlong names can never create a child path or exceed a normal
/// filesystem's component limit. Unicode, `@`, and internal spaces are valid
/// portable literals; trailing spaces and dots are not portable to Windows.
/// Preserving literals also preserves v1's pre-existing limitation: distinct
/// portable names can alias on a filesystem that case-folds or normalizes
/// Unicode. Digest-derived names remain distinct by exact UTF-8 bytes modulo a
/// SHA-256 collision; eliminating literal aliases would require a new all-name
/// encoding or name table and is outside filename layout v2.
fn field_file_component(field: &str) -> Cow<'_, str> {
    let portable_literal = !field.is_empty()
        && field.len() <= MAX_LITERAL_FIELD_COMPONENT_BYTES
        && field != "."
        && field != ".."
        && !field.starts_with(ENCODED_FIELD_COMPONENT_PREFIX)
        && !is_windows_reserved_device_name(field)
        && !field.ends_with(' ')
        && !field.ends_with('.')
        && field.chars().all(|character| {
            !character.is_control()
                && !matches!(
                    character,
                    '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*'
                )
        });

    if portable_literal {
        return Cow::Borrowed(field);
    }

    let digest = Sha256::digest(field.as_bytes());
    Cow::Owned(format!("{ENCODED_FIELD_COMPONENT_PREFIX}{digest:x}"))
}

/// Derive one FTS side-car path from controlled components only.
fn field_sidecar_path(
    segment_dir: &Path,
    segment_id: &str,
    field_name: &str,
    extension: &str,
) -> PathBuf {
    let field_component = field_file_component(field_name);
    segment_dir.join(format!("{segment_id}.{field_component}.{extension}"))
}

/// Return the exact pre-fix raw path only when it remains one child of the
/// segment directory on this host.
///
/// This reader-only bridge keeps already-published Linux fields such as a raw
/// backslash, a reserved digest-prefix literal, or a formerly accepted long
/// component readable after upgrade. It never creates a path and it refuses
/// slash/backslash-as-separator shapes through the parent equality check.
fn legacy_field_sidecar_path(
    segment_dir: &Path,
    segment_id: &str,
    field_name: &str,
    extension: &str,
) -> Option<PathBuf> {
    let candidate = segment_dir.join(format!("{segment_id}.{field_name}.{extension}"));
    (candidate.parent() == Some(segment_dir)).then_some(candidate)
}

// ── FieldIndexConfig ──────────────────────────────────────────────────────────

/// Per-field indexing configuration.
#[derive(Debug, Clone)]
pub struct FieldIndexConfig {
    /// Name of the analyzer to use for this field.
    pub analyzer: String,
    /// Whether to store positions (required for phrase queries).
    pub store_positions: bool,
    /// Whether to store term vectors (for highlight / more-like-this).
    pub store_term_vectors: bool,
}

impl Default for FieldIndexConfig {
    fn default() -> Self {
        Self {
            analyzer: "standard".to_owned(),
            store_positions: true,
            store_term_vectors: false,
        }
    }
}

// ── Meta file structures ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct FieldMeta {
    stats: FieldStats,
    terms: HashMap<String, SerialTermPostings>,
    /// When `false` the posting lists for this field omit both freq
    /// blocks and position blocks — each posting is just the doc_id.
    /// Readers must treat term_freq as 1 for every match.  Stored on
    /// disk as part of the `ZFM2` header.  Legacy `ZFM1` segments are
    /// implicitly `has_positions = true`.
    has_positions: bool,
    /// Format version used to read the field's FST values.
    ///
    /// * `FstValueFormat::PostingsOffset` — legacy ZFM1/ZFM2.  The FST
    ///   value is the byte offset into the `.post` file where this
    ///   term's postings start.  All of `{doc_freq, ttf, length}` are
    ///   looked up in `terms` keyed by the term string.
    /// * `FstValueFormat::MetaByteOffset` — ZFM3.  The FST value is the
    ///   byte offset of this term's 24-byte `{df, ttf, offset, length}`
    ///   record inside the `.meta` binary array.  The `.meta` file no
    ///   longer stores the term string — a 22 B/term saving on
    ///   high-cardinality keyword fields.
    fst_value_format: FstValueFormat,
    /// Raw bytes of the ZFM3 flat 24-byte-per-term records (in the same
    /// sorted-by-term order the FST enumerates).  Empty for ZFM1/ZFM2
    /// where metadata lives inside `terms`.
    flat_records: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FstValueFormat {
    /// Legacy: FST value is `postings_offset`.
    PostingsOffset,
    /// ZFM3: FST value is the byte offset of this term's record in the
    /// `.meta` flat array.  `meta[value..value+24]` holds
    /// `{doc_frequency: u32, total_term_frequency: u64,
    /// postings_offset: u64, postings_length: u32}`.
    MetaByteOffset,
}

/// Serializable version of TermPostings (all fields stored).
#[derive(Debug, Clone, Copy)]
struct SerialTermPostings {
    doc_frequency: u32,
    total_term_frequency: u64,
    postings_offset: u64,
    postings_length: u32,
}

impl From<&TermPostings> for SerialTermPostings {
    fn from(tp: &TermPostings) -> Self {
        Self {
            doc_frequency: tp.doc_frequency,
            total_term_frequency: tp.total_term_frequency,
            postings_offset: tp.postings_offset,
            postings_length: tp.postings_length,
        }
    }
}

impl From<SerialTermPostings> for TermPostings {
    fn from(s: SerialTermPostings) -> Self {
        Self {
            doc_frequency: s.doc_frequency,
            total_term_frequency: s.total_term_frequency,
            postings_offset: s.postings_offset,
            postings_length: s.postings_length,
        }
    }
}

// Binary `.meta` file format (V4 M4.7 — replaces pretty-JSON serde).
//
// Pretty-JSON was ~120 B/term (field names, quotes, indentation, newlines)
// on a dataset where the actual data per term is 24 B.  On the 66.5 M
// nginx battle that ballooned `.meta` to 8.37 GB = 126 B/doc across 2253
// segments × ~10 fields.  The binary format below is 4-20 B/term
// depending on term length — a 6-20× reduction.
//
// Layout:
//
// ```
// "ZFM1"  4 bytes magic  (legacy — implies has_positions = true)
// "ZFM2"  4 bytes magic  (adds 1-byte has_positions flag after num_terms)
// "ZFM3"  4 bytes magic  (drops term names — FST value = meta byte offset)
// u64    total_docs             (FieldStats.total_docs)
// u64    total_field_length     (FieldStats.total_field_length)
// u32    num_terms
// [ZFM2 + ZFM3] u8   has_positions
// ZFM1/ZFM2 per term (num_terms times):
//     u16  term_len
//     term_len bytes             (UTF-8 term)
//     u32  doc_frequency
//     u64  total_term_frequency
//     u64  postings_offset
//     u32  postings_length
// ZFM3 per term (fixed 24 bytes each, no term string):
//     u32  doc_frequency
//     u64  total_term_frequency
//     u64  postings_offset
//     u32  postings_length
// ```
const META_MAGIC_V1: &[u8; 4] = b"ZFM1";
const META_MAGIC_V2: &[u8; 4] = b"ZFM2";
const META_MAGIC_V3: &[u8; 4] = b"ZFM3";
/// ZFM4 = ZFM3 records section wrapped in a Zstd-19 envelope.  The
/// header bytes (magic + total_docs + total_field_length + num_terms +
/// has_positions) stay uncompressed so they're cheap to read; only
/// the per-term fixed-24-byte records (which dominate the file at
/// high-cardinality fields — 60 % of segment bytes on the bench)
/// get compressed.  Records are highly redundant (small u32/u64s,
/// monotonic offsets) so Zstd-19 typically squeezes 6-10× on them.
/// On read we decompress once into the same in-memory `flat_records`
/// `Vec<u8>` ZFM3 already populates, so the lookup hot path is
/// completely unchanged.
const META_MAGIC_V4: &[u8; 4] = b"ZFM4";
/// Postings file wrapped in a whole-file LZ4 envelope — magic prefix
/// lets the reader auto-detect and decompress while legacy `.post`
/// files (no prefix) continue to work via the raw mmap path.
const POST_MAGIC_LZ4: &[u8; 4] = b"ZPL1";
/// Postings file wrapped in a Zstd-19 envelope.  Same idea as ZPL1
/// but trades ~3× more CPU at flush time for a ~1.4× tighter file
/// — flush is already CPU-light per segment and is the right place
/// to spend CPU on durable artifacts.  Reader auto-detects which
/// envelope was used; old segments stay readable.
const POST_MAGIC_ZSTD: &[u8; 4] = b"ZPS1";
/// Zstd compression level used for the durable segment artifacts
/// (`.meta` ZFM4, `.post` ZPS1).  Reverted from 19 to 3: this constant
/// is invoked at **flush** time, not just merge, so the ~25 MB/s/core
/// throughput of level 19 is not "out of band" — it stalls the back-
/// pressure-critical flush path.  Level 3 (~250 MB/s/core) restores
/// 1 M+ docs/s sustained ingest at a ~5 % steady-state disk cost
/// (merge-dominated long-term storage barely changes; only the
/// freshest tier-0 segments are larger before they merge).  See
/// `engine/reports/2026-04-25T21-50-00_ingest_perf_regression_zstd19.md`.
///
/// This is the **flush** level and the writer default. A merge-time caller
/// raises it via [`FtsIndexWriter::with_zstd_level`] to honour the operator's
/// `compression.level` (#318) — merge is off the ingest critical path, which
/// is exactly the distinction the paragraph above is about.
pub const ZSTD_DURABLE_LEVEL: i32 = 3;
const ZFM3_RECORD_LEN: usize = 4 + 8 + 8 + 4; // 24 bytes: df, ttf, off, len

/// Header byte length for a ZFM3 file (magic + total_docs + total_field_length
/// + num_terms + has_positions).
const ZFM3_HEADER_LEN: usize = 4 + 8 + 8 + 4 + 1;

/// Encode a `.meta` file in the ZFM3 flat format.
///
/// `sorted_terms` must be the same term-sorted order used to insert
/// into the FST; the i-th term's record lives at byte offset
/// `ZFM3_HEADER_LEN + i * ZFM3_RECORD_LEN`, which is what the FST
/// value stores for that term.  Drops the ~20 B/term term-string
/// duplication that ZFM1/ZFM2 carried, since the `.fst` already owns
/// the authoritative sorted dictionary.
/// Encode a `.meta` file in the ZFM4 format = ZFM3 header + Zstd
/// envelope around the records section.  See `META_MAGIC_V4` for the
/// motivation; on a 100 k-doc XERJ bench segment this drops the two
/// largest meta files (`.k.meta`, `.name.meta`) from 2.25 MB each to
/// ~250 KB — the single biggest disk-efficiency win.
///
/// Layout:
/// ```text
///   "ZFM4" 4
///   total_docs           u64
///   total_field_length   u64
///   num_terms            u32
///   has_positions        u8
///   uncompressed_len     u32  (= num_terms * 24, sanity check)
///   compressed_len       u32  (= len(zstd_payload), tail follows)
///   zstd_payload         compressed_len bytes
/// ```
fn encode_field_meta_v4(
    stats: &FieldStats,
    has_positions: bool,
    sorted_terms: &[String],
    term_postings: &HashMap<String, TermPostings>,
    zstd_level: i32,
) -> Result<Vec<u8>> {
    let num_terms = sorted_terms.len();
    // Build the records section in the same byte layout that ZFM3
    // would write — that way `flat_records` in memory is identical to
    // the ZFM3 path and `lookup_term` works unchanged.
    let mut records: Vec<u8> = Vec::with_capacity(num_terms * ZFM3_RECORD_LEN);
    for term in sorted_terms {
        let tp = term_postings
            .get(term)
            .expect("sorted_terms must match term_postings keys");
        records.write_u32::<LittleEndian>(tp.doc_frequency).unwrap();
        records
            .write_u64::<LittleEndian>(tp.total_term_frequency)
            .unwrap();
        records
            .write_u64::<LittleEndian>(tp.postings_offset)
            .unwrap();
        records
            .write_u32::<LittleEndian>(tp.postings_length)
            .unwrap();
    }
    let uncompressed_len = records.len() as u32;
    let compressed =
        zstd::bulk::compress(&records, zstd_level).with_context(|| "ZFM4 zstd compress")?;
    let mut out: Vec<u8> = Vec::with_capacity(ZFM3_HEADER_LEN + 4 + 4 + compressed.len());
    out.extend_from_slice(META_MAGIC_V4);
    out.write_u64::<LittleEndian>(stats.total_docs).unwrap();
    out.write_u64::<LittleEndian>(stats.total_field_length)
        .unwrap();
    out.write_u32::<LittleEndian>(num_terms as u32).unwrap();
    out.push(if has_positions { 1u8 } else { 0u8 });
    out.write_u32::<LittleEndian>(uncompressed_len).unwrap();
    out.write_u32::<LittleEndian>(compressed.len() as u32)
        .unwrap();
    out.extend_from_slice(&compressed);
    Ok(out)
}

fn decode_field_meta_binary(bytes: &[u8]) -> Result<FieldMeta> {
    use std::io::Cursor;
    if bytes.len() < 4 {
        return Err(anyhow::anyhow!("field meta: too short"));
    }
    // ZFM4 path — Zstd-compressed records section.  Decompresses
    // once at open time into the same `flat_records` Vec<u8> ZFM3
    // populates, so the per-query lookup path is unchanged.
    if &bytes[..4] == META_MAGIC_V4 {
        // Header + the two trailing length u32s = 4 + 8 + 8 + 4 + 1 + 4 + 4
        let zfm4_prefix = ZFM3_HEADER_LEN + 4 + 4;
        if bytes.len() < zfm4_prefix {
            return Err(anyhow::anyhow!("field meta: ZFM4 truncated header"));
        }
        let mut cur = Cursor::new(&bytes[4..zfm4_prefix]);
        let total_docs = cur.read_u64::<LittleEndian>()?;
        let total_field_length = cur.read_u64::<LittleEndian>()?;
        let num_terms = cur.read_u32::<LittleEndian>()? as usize;
        let has_positions = cur.read_u8()? != 0;
        let uncompressed_len = cur.read_u32::<LittleEndian>()? as usize;
        let compressed_len = cur.read_u32::<LittleEndian>()? as usize;
        let expected_uncompressed = num_terms * ZFM3_RECORD_LEN;
        if uncompressed_len != expected_uncompressed {
            return Err(anyhow::anyhow!(
                "field meta: ZFM4 length mismatch (uncompressed_len={uncompressed_len}, num_terms*24={expected_uncompressed})"
            ));
        }
        if bytes.len() < zfm4_prefix + compressed_len {
            return Err(anyhow::anyhow!(
                "field meta: ZFM4 payload truncated (expected {compressed_len} bytes, got {})",
                bytes.len() - zfm4_prefix
            ));
        }
        let payload = &bytes[zfm4_prefix..zfm4_prefix + compressed_len];
        let flat_records = zstd::bulk::decompress(payload, uncompressed_len)
            .with_context(|| "ZFM4 zstd decompress")?;
        if flat_records.len() != uncompressed_len {
            return Err(anyhow::anyhow!(
                "field meta: ZFM4 decompressed size mismatch (got {}, expected {uncompressed_len})",
                flat_records.len()
            ));
        }
        return Ok(FieldMeta {
            stats: FieldStats {
                total_docs,
                total_field_length,
            },
            terms: HashMap::new(),
            has_positions,
            fst_value_format: FstValueFormat::MetaByteOffset,
            flat_records,
        });
    }
    // ZFM3 path — flat 24-byte records, no term strings stored.
    if &bytes[..4] == META_MAGIC_V3 {
        if bytes.len() < ZFM3_HEADER_LEN {
            return Err(anyhow::anyhow!("field meta: ZFM3 truncated header"));
        }
        let mut cur = Cursor::new(&bytes[4..ZFM3_HEADER_LEN]);
        let total_docs = cur.read_u64::<LittleEndian>()?;
        let total_field_length = cur.read_u64::<LittleEndian>()?;
        let num_terms = cur.read_u32::<LittleEndian>()? as usize;
        let has_positions = cur.read_u8()? != 0;
        let expected = ZFM3_HEADER_LEN + num_terms * ZFM3_RECORD_LEN;
        if bytes.len() < expected {
            return Err(anyhow::anyhow!(
                "field meta: ZFM3 body truncated (expected {expected}, got {})",
                bytes.len()
            ));
        }
        return Ok(FieldMeta {
            stats: FieldStats {
                total_docs,
                total_field_length,
            },
            // ZFM3 doesn't populate `terms` — lookups go through
            // `flat_records` via the FST byte-offset value instead.
            terms: HashMap::new(),
            has_positions,
            fst_value_format: FstValueFormat::MetaByteOffset,
            flat_records: bytes[ZFM3_HEADER_LEN..expected].to_vec(),
        });
    }

    // ZFM1 / ZFM2 path — term names + metadata interleaved.
    let is_v2 = if &bytes[..4] == META_MAGIC_V2 {
        true
    } else if &bytes[..4] == META_MAGIC_V1 {
        false
    } else {
        return Err(anyhow::anyhow!("field meta: missing ZFM1/ZFM2/ZFM3 magic"));
    };
    let mut cur = Cursor::new(&bytes[4..]);
    let total_docs = cur.read_u64::<LittleEndian>()?;
    let total_field_length = cur.read_u64::<LittleEndian>()?;
    let num_terms = cur.read_u32::<LittleEndian>()? as usize;
    let has_positions = if is_v2 {
        cur.read_u8()? != 0
    } else {
        true // legacy default
    };
    let mut terms: HashMap<String, SerialTermPostings> = HashMap::with_capacity(num_terms);
    for _ in 0..num_terms {
        let term_len = cur.read_u16::<LittleEndian>()? as usize;
        let pos = cur.position() as usize + 4;
        if bytes.len() < pos + term_len {
            return Err(anyhow::anyhow!("field meta: truncated term"));
        }
        let term_bytes = &bytes[pos..pos + term_len];
        let term = std::str::from_utf8(term_bytes)
            .map_err(|e| anyhow::anyhow!("field meta: bad utf8: {e}"))?
            .to_owned();
        cur.set_position((pos + term_len - 4) as u64);
        let doc_frequency = cur.read_u32::<LittleEndian>()?;
        let total_term_frequency = cur.read_u64::<LittleEndian>()?;
        let postings_offset = cur.read_u64::<LittleEndian>()?;
        let postings_length = cur.read_u32::<LittleEndian>()?;
        terms.insert(
            term,
            SerialTermPostings {
                doc_frequency,
                total_term_frequency,
                postings_offset,
                postings_length,
            },
        );
    }
    Ok(FieldMeta {
        stats: FieldStats {
            total_docs,
            total_field_length,
        },
        terms,
        has_positions,
        fst_value_format: FstValueFormat::PostingsOffset,
        flat_records: Vec::new(),
    })
}

// ── Multi-valued field input ──────────────────────────────────────────────────

/// Position gap inserted between two values of the same multi-valued field.
///
/// Mirrors Lucene/Elasticsearch's default `position_increment_gap` of 100.
/// Without it, `["alpha bravo", "charlie"]` would put `bravo` at position 1 and
/// `charlie` at position 2, so `match_phrase: "bravo charlie"` would match a
/// phrase that exists in NEITHER value.  100 is large enough that no realistic
/// `slop` bridges the boundary, and small enough to stay in the VByte fast path.
pub const POSITION_INCREMENT_GAP: u32 = 100;

/// The values a single document supplies for a single field.
///
/// A JSON document field is either a scalar or an array, and Elasticsearch
/// treats the array as N independent values of the field — not as one
/// concatenated value.  That distinction is invisible for an analyzed `text`
/// field only by accident (the standard tokenizer re-splits on the joining
/// space); for a `keyword` field, which is indexed with the whole input as one
/// token, joining `["red","blue"]` into `"red blue"` produces a term that
/// matches neither `red` nor `blue`.  See issue #332.
///
/// `One` exists so the overwhelmingly common single-valued case costs no extra
/// allocation on the flush/merge path — the `String` is moved in as-is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValues {
    /// Exactly one value (a JSON scalar, or a one-element array).
    One(String),
    /// Two or more values (a JSON array). Never constructed with < 2 elements
    /// by [`FieldValues::from_values`], but tolerated if built directly.
    Many(Vec<String>),
}

impl FieldValues {
    /// Build from an iterator, collapsing the single-value case to `One`.
    pub fn from_values<I: IntoIterator<Item = String>>(values: I) -> Self {
        let mut v: Vec<String> = values.into_iter().collect();
        if v.len() == 1 {
            FieldValues::One(v.pop().unwrap())
        } else {
            FieldValues::Many(v)
        }
    }

    /// Iterate every value in document order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        match self {
            FieldValues::One(s) => std::slice::from_ref(s).iter(),
            FieldValues::Many(v) => v.iter(),
        }
        .map(|s| s.as_str())
    }

    /// Number of values.
    pub fn len(&self) -> usize {
        match self {
            FieldValues::One(_) => 1,
            FieldValues::Many(v) => v.len(),
        }
    }

    /// True when the field carries no value at all (only reachable via a
    /// directly-constructed `Many(vec![])`).
    pub fn is_empty(&self) -> bool {
        match self {
            FieldValues::One(s) => s.is_empty(),
            FieldValues::Many(v) => v.iter().all(|s| s.is_empty()),
        }
    }
}

impl From<String> for FieldValues {
    fn from(s: String) -> Self {
        FieldValues::One(s)
    }
}

impl From<&str> for FieldValues {
    fn from(s: &str) -> Self {
        FieldValues::One(s.to_owned())
    }
}

impl From<Vec<String>> for FieldValues {
    fn from(v: Vec<String>) -> Self {
        FieldValues::from_values(v)
    }
}

/// Analyze every value of one field for one document and fold the tokens into
/// `field_data`.
///
/// Shared by [`FtsIndexWriter::add_document`] and
/// [`FtsIndexWriter::add_documents_parallel`] so the serial and parallel paths
/// cannot drift — the two used to hold independent copies of this loop.
///
/// Semantics per value:
/// * positions restart from `base`, which advances past the previous value's
///   last position plus [`POSITION_INCREMENT_GAP`];
/// * the norm (field length) is the SUM of every value's token count, matching
///   Lucene, so BM25 length-normalisation sees the whole field;
/// * exactly one norm entry and one `total_docs` increment per (doc, field),
///   however many values the field has.
fn index_field_values<'a>(
    field_data: &mut FieldData,
    analyzer: &crate::analyzer::AnalyzerPipeline,
    doc_ord: u32,
    values: impl Iterator<Item = &'a str>,
) {
    let mut field_len: u64 = 0;
    let mut base: u32 = 0;
    let mut first = true;
    for value in values {
        if !first {
            base = base.saturating_add(POSITION_INCREMENT_GAP);
        }
        first = false;
        let tokens = analyzer.analyze(value);
        let mut max_position = 0u32;
        for token in &tokens {
            field_data.postings.add_occurrence(
                &token.text,
                doc_ord,
                base.saturating_add(token.position),
            );
            max_position = max_position.max(token.position);
        }
        if !tokens.is_empty() {
            base = base.saturating_add(max_position).saturating_add(1);
        }
        field_len += tokens.len() as u64;
    }

    field_data.norms.push((
        doc_ord,
        norm_u16_to_u8(field_len.min(u16::MAX as u64) as u16),
    ));
    field_data.stats.total_docs += 1;
    field_data.stats.total_field_length += field_len;
}

// ── FtsIndexWriter ────────────────────────────────────────────────────────────

/// Builds the FTS inverted index for one segment.
///
/// Usage:
/// ```text
/// let mut writer = FtsIndexWriter::new(dir, segment_id, registry);
/// for doc in docs { writer.add_document(doc_id, &fields); }
/// if writer.uses_encoded_field_filename_components() {
///     // The engine advances its data-directory format before this step.
///     writer.publish_encoded_filename_layout()?;
/// }
/// writer.finish()?;
/// ```
pub struct FtsIndexWriter {
    segment_dir: PathBuf,
    segment_id: String,
    registry: Arc<AnalyzerRegistry>,
    /// Per-field: (config, postings_writer, field_stats, norms)
    fields: HashMap<String, FieldData>,
    encoded_filename_layout_published: bool,
    /// Zstd effort for the `.meta` (ZFM4) and `.post` (ZPS1) envelopes.
    /// [`ZSTD_DURABLE_LEVEL`] unless [`Self::with_zstd_level`] raised it.
    zstd_level: i32,
}

struct FieldData {
    config: FieldIndexConfig,
    postings: PostingsWriter,
    stats: FieldStats,
    /// `(doc_id, quantised norm byte)` in insertion order.
    ///
    /// The quantisation happens HERE rather than in `write_field_static` so
    /// that the merge path can carry a source segment's norm byte through
    /// verbatim.  [`norm_u8_to_u16`] is NOT the inverse of
    /// [`norm_u16_to_u8`] — byte 9 dequantises to length 8, which
    /// re-quantises to byte 8 — so a merge that read lengths back out of a
    /// source segment and re-quantised them would silently change BM25
    /// length normalisation for every merged document.
    norms: Vec<(u32, u8)>,
}

impl FtsIndexWriter {
    /// Create a new writer that will output files to `segment_dir`.
    pub fn new(
        segment_dir: impl AsRef<Path>,
        segment_id: impl Into<String>,
        registry: Arc<AnalyzerRegistry>,
    ) -> Self {
        Self {
            segment_dir: segment_dir.as_ref().to_path_buf(),
            segment_id: segment_id.into(),
            registry,
            fields: HashMap::new(),
            encoded_filename_layout_published: false,
            zstd_level: ZSTD_DURABLE_LEVEL,
        }
    }

    /// Re-encode this segment's `.meta` / `.post` at `level` instead of the
    /// flush default [`ZSTD_DURABLE_LEVEL`].
    ///
    /// For merge callers only — flush must not raise the level, and
    /// [`ZSTD_DURABLE_LEVEL`] records the measured reason. Both envelopes
    /// stay byte-format-identical across levels (zstd decode does not consult
    /// the level a payload was written at), so this changes only how hard the
    /// encoder works and how large the result is.
    pub fn with_zstd_level(mut self, level: i32) -> Self {
        self.zstd_level = level;
        self
    }

    /// Register a field with its indexing configuration.
    /// Must be called before `add_document` uses this field.
    pub fn configure_field(&mut self, field: impl Into<String>, config: FieldIndexConfig) {
        let postings = if config.store_positions {
            PostingsWriter::new()
        } else {
            PostingsWriter::new_no_positions()
        };
        self.fields.insert(
            field.into(),
            FieldData {
                config,
                postings,
                stats: FieldStats::default(),
                norms: Vec::new(),
            },
        );
    }

    /// Whether finishing this writer can create a v2 digest-derived field
    /// filename.
    ///
    /// Engine callers use this after configuration/document ingestion and
    /// before [`Self::finish`] to durably advance the data-directory marker.
    pub fn uses_encoded_field_filename_components(&self) -> bool {
        self.fields
            .keys()
            .any(|field| matches!(field_file_component(field), Cow::Owned(_)))
    }

    /// Durably publish this segment's immutable v2 filename discriminator.
    ///
    /// The engine must durably advance the data-directory format first, call
    /// this method second, and call [`Self::finish`] last. `finish` refuses to
    /// emit encoded side-cars without this proof. An existing discriminator is
    /// accepted only when it contains the exact v2 magic.
    pub fn publish_encoded_filename_layout(&mut self) -> Result<()> {
        if !self.uses_encoded_field_filename_components() {
            return Ok(());
        }
        fs::create_dir_all(&self.segment_dir)
            .with_context(|| format!("creating segment dir {:?}", self.segment_dir))?;
        let marker = segment_filename_layout_v2_marker_path(&self.segment_dir, &self.segment_id);
        match fs::read(&marker) {
            Ok(bytes) if bytes == FTS_FILENAME_LAYOUT_V2_MARKER_BYTES => {
                // A previous attempt can have made the rename visible and
                // then failed its durability confirmation. A fresh writer has
                // no process-local proof, so confirm the directory entry
                // before authorizing encoded side-cars.
                #[cfg(not(windows))]
                xerj_common::fsio::fsync_dir(&self.segment_dir).with_context(|| {
                    format!("confirming FTS filename-layout discriminator {:?}", marker)
                })?;
                // Windows has no parent-directory fsync contract. Replacing
                // the exact marker through MoveFileExW WRITE_THROUGH also
                // repairs a visible discriminator produced by an older build
                // whose Windows directory-sync shim was a no-op.
                #[cfg(windows)]
                xerj_common::fsio::write_file_durable(&marker, FTS_FILENAME_LAYOUT_V2_MARKER_BYTES)
                    .with_context(|| {
                        format!("confirming FTS filename-layout discriminator {:?}", marker)
                    })?;
            }
            Ok(_) => bail!("corrupt FTS filename-layout discriminator {:?}", marker),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                xerj_common::fsio::write_file_durable(&marker, FTS_FILENAME_LAYOUT_V2_MARKER_BYTES)
                    .with_context(|| {
                        format!("publishing FTS filename-layout discriminator {:?}", marker)
                    })?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("reading FTS filename-layout discriminator {:?}", marker)
                });
            }
        }
        self.encoded_filename_layout_published = true;
        Ok(())
    }

    /// Index all text fields of one document.
    ///
    /// `fields` is a map of field name → that field's [`FieldValues`].  Each
    /// value is analyzed independently and separated from the next by
    /// [`POSITION_INCREMENT_GAP`] — a JSON array is N values of the field, not
    /// one joined value (issue #332).
    ///
    /// Fields not previously registered via `configure_field` are indexed
    /// with the default configuration (standard analyzer, positions on).
    pub fn add_document(&mut self, doc_id: u32, fields: &HashMap<String, FieldValues>) {
        for (field_name, values) in fields {
            let registry = Arc::clone(&self.registry);

            // Ensure field entry exists
            if !self.fields.contains_key(field_name) {
                self.fields.insert(
                    field_name.clone(),
                    FieldData {
                        config: FieldIndexConfig::default(),
                        postings: PostingsWriter::new(),
                        stats: FieldStats::default(),
                        norms: Vec::new(),
                    },
                );
            }

            let field_data = self.fields.get_mut(field_name).unwrap();

            // Resolve analyzer
            let analyzer_name = &field_data.config.analyzer;
            let analyzer = registry
                .get_analyzer(analyzer_name)
                .or_else(|| registry.get_analyzer("standard"))
                .unwrap();

            index_field_values(field_data, &analyzer, doc_id, values.iter());
        }
    }

    /// V4 M4 — **parallel batch** add for flush time.
    ///
    /// Reshapes `(doc_id, field, values)` from row-major (per-doc) into
    /// column-major (per-field) then tokenises + builds per-field
    /// postings in parallel via rayon.  The underlying PostingsWriter
    /// state is still single-threaded per field, but fields run in
    /// parallel, which is the biggest win since nginx logs have ~10
    /// fields and the machine has 32 cores.
    ///
    /// Correctness notes:
    /// - Every field in any doc pre-registers the same `FieldIndexConfig`
    ///   via `configure_field`, so the analyzer resolution is identical
    ///   across threads.
    /// - Doc ordinals are assigned by position in the input `docs` vec,
    ///   matching the row index the caller used with
    ///   `add_document(ordinal, ...)`.
    ///
    /// Generic over the third tuple element (a source payload the caller
    /// keeps alongside for its own use — `serde_json::Value`,
    /// `Arc<serde_json::Value>`, …): this method never reads it.
    pub fn add_documents_parallel<S: Sync>(
        &mut self,
        docs: &[(String, HashMap<String, FieldValues>, S)],
    ) {
        use rayon::prelude::*;
        use std::collections::HashMap as StdHashMap;

        // Column-major reshape: field_name → Vec<(doc_ordinal, values)>.
        // Lookup-first so the common case (field already seen) skips the
        // per-doc-field `field_name.clone()` the `entry()` API forced.
        let mut per_field: StdHashMap<String, Vec<(u32, &FieldValues)>> = StdHashMap::new();
        for (ord, (_id, fields, _src)) in docs.iter().enumerate() {
            for (field_name, values) in fields {
                if let Some(v) = per_field.get_mut(field_name) {
                    v.push((ord as u32, values));
                } else {
                    per_field
                        .entry(field_name.clone())
                        .or_default()
                        .push((ord as u32, values));
                }
            }
        }

        // Pre-register every field so the parallel build picks up the
        // right config.
        for name in per_field.keys() {
            if !self.fields.contains_key(name) {
                self.fields.insert(
                    name.clone(),
                    FieldData {
                        config: FieldIndexConfig::default(),
                        postings: PostingsWriter::new(),
                        stats: FieldStats::default(),
                        norms: Vec::new(),
                    },
                );
            }
        }

        // Process fields in parallel.  Each task owns its own
        // `FieldData` — we swap them back in once the parallel work
        // finishes.
        let registry = Arc::clone(&self.registry);
        let field_configs: StdHashMap<String, FieldIndexConfig> = self
            .fields
            .iter()
            .map(|(k, v)| (k.clone(), v.config.clone()))
            .collect();

        let per_field_vec: Vec<(String, Vec<(u32, &FieldValues)>)> =
            per_field.into_iter().collect();

        let built: Vec<(String, FieldData)> = per_field_vec
            .into_par_iter()
            .map(|(field_name, entries)| {
                let cfg = field_configs.get(&field_name).cloned().unwrap_or_default();
                let analyzer = registry
                    .get_analyzer(&cfg.analyzer)
                    .or_else(|| registry.get_analyzer("standard"))
                    .unwrap();

                let postings = if cfg.store_positions {
                    PostingsWriter::new()
                } else {
                    PostingsWriter::new_no_positions()
                };
                let mut fd = FieldData {
                    config: cfg,
                    postings,
                    stats: FieldStats::default(),
                    norms: Vec::with_capacity(entries.len()),
                };

                for (doc_ord, values) in entries {
                    index_field_values(&mut fd, &analyzer, doc_ord, values.iter());
                }
                (field_name, fd)
            })
            .collect();

        // Swap the built field data back in.
        for (name, fd) in built {
            self.fields.insert(name, fd);
        }
    }

    /// Build every field of this segment by REPLAYING the source segments'
    /// postings, instead of re-analysing their documents.
    ///
    /// This is the merge path.  `write_field_static` has always taken a
    /// `PostingsWriter`, and `PostingsWriter`'s ingest unit has always been a
    /// (term, doc, position) occurrence — never a document — so nothing in
    /// the segment writer needs an analyzer.  The engine's merge nevertheless
    /// walked back to each surviving document's stored JSON, re-extracted its
    /// field values and re-ran the whole analyzer chain, recomputing postings
    /// that were already on disk and already correct.  Under size-tiered
    /// levelling that re-analyses every document O(log N) times (#876).
    ///
    /// What this reproduces exactly, and what it does not:
    ///
    /// * **postings** — doc ids (remapped), term frequencies and positions
    ///   are replayed verbatim from the source `.post` blobs, so `doc_freq`
    ///   and every posting list are identical to a re-analysed merge.
    /// * **norms** — the source's *quantised* byte is carried through
    ///   untouched.  It cannot be recomputed from `field_length`, because
    ///   `norm_u16_to_u8` is lossy and is not invertible.
    /// * **field statistics** — `total_docs` / `total_field_length` are
    ///   carried from the source `.meta` and reduced by the dropped
    ///   documents' contribution.  With no dropped documents (the ordinary
    ///   merge) they are exact.  Two blind spots when documents ARE dropped:
    ///   a dropped document whose value analysed to zero tokens leaves no
    ///   trace in either the postings or the norms, so its `total_docs` seat
    ///   is not reclaimed; and for a docs-only field a dropped document's
    ///   length is read back from the quantised norm byte, which is exact
    ///   only up to length 7.
    /// * **`total_term_frequency`** of a docs-only field becomes its doc
    ///   frequency, because the docs-only `.post` format never stored a
    ///   per-document frequency — its reader synthesises `term_freq = 1`.
    ///   That is the value every reader of the source segment already sees,
    ///   and nothing outside this crate reads `total_term_frequency`.
    ///
    /// Returns `Err` (leaving the writer untouched) when the inputs cannot be
    /// merged this way — a field whose sources disagree about positions, or
    /// whose on-disk shape contradicts the merge-time configuration.  The
    /// caller is expected to fall back to re-analysis rather than to fail the
    /// merge.
    pub fn merge_from_segments(&mut self, sources: &[FtsMergeSource<'_>]) -> Result<()> {
        use rayon::prelude::*;

        let mut field_names: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for source in sources {
            for field in source.reader.indexed_fields() {
                if seen.insert(field) {
                    field_names.push(field.to_owned());
                }
            }
        }
        drop(seen);
        // Deterministic field order keeps a merge reproducible run to run.
        field_names.sort_unstable();

        // Snapshot the configs before the parallel pass so the closure never
        // borrows `self.fields` while the results are being inserted.
        let configs: Vec<FieldIndexConfig> = field_names
            .iter()
            .map(|field| {
                self.fields
                    .get(field)
                    .map(|data| data.config.clone())
                    .unwrap_or_default()
            })
            .collect();

        let built: Vec<Result<FieldData>> = field_names
            .par_iter()
            .zip(configs.par_iter())
            .map(|(field, config)| merge_field_from_segments(field, config, sources))
            .collect();

        // Collect every field before mutating anything: a failure must leave
        // the writer exactly as the caller configured it, so the fallback
        // path starts from a clean slate.
        let mut merged: Vec<(String, FieldData)> = Vec::with_capacity(field_names.len());
        for (field, data) in field_names.into_iter().zip(built) {
            merged.push((field, data?));
        }
        for (field, data) in merged {
            self.fields.insert(field, data);
        }
        Ok(())
    }

    /// Flush all data to disk and return field stats for the segment manifest.
    ///
    /// Field writes run in parallel via Rayon — each thread owns one field's
    /// FST + postings + meta + norms build and writes its four side-car
    /// files independently.  On a 2-text-field nginx log this halves the
    /// flush stall; on a 10-text-field catalog index it's closer to 5×.
    pub fn finish(self) -> Result<HashMap<String, FieldStats>> {
        use rayon::prelude::*;

        if self.uses_encoded_field_filename_components() && !self.encoded_filename_layout_published
        {
            bail!("encoded FTS field filenames require a durable per-segment layout discriminator");
        }

        fs::create_dir_all(&self.segment_dir)
            .with_context(|| format!("creating segment dir {:?}", self.segment_dir))?;

        let segment_dir = self.segment_dir.clone();
        let segment_id = self.segment_id.clone();
        let zstd_level = self.zstd_level;

        // Drain fields into a Vec so we can parallelise the iterator.
        // Cloning `stats` before consuming `field_data` — `stats` goes into
        // the returned map, `field_data` goes into the writer.
        let fields: Vec<(String, FieldStats, FieldData)> = self
            .fields
            .into_iter()
            .map(|(name, fd)| {
                let stats = fd.stats.clone();
                (name, stats, fd)
            })
            .collect();

        // Parallel field writes.  `write_field_static` is a pure function
        // of its inputs and touches only files named after the field, so
        // there's no cross-thread contention.
        let results: Vec<Result<(String, FieldStats)>> = fields
            .into_par_iter()
            .map(|(field_name, stats, field_data)| {
                Self::write_field_static(
                    &segment_dir,
                    &segment_id,
                    &field_name,
                    field_data,
                    zstd_level,
                )
                .with_context(|| format!("writing field '{}'", field_name))?;
                Ok((field_name, stats))
            })
            .collect();

        // Surface any error; otherwise build the stats map.
        let mut all_stats = HashMap::new();
        for r in results {
            let (name, stats) = r?;
            all_stats.insert(name, stats);
        }

        Ok(all_stats)
    }

    fn write_field_static(
        segment_dir: &Path,
        segment_id: &str,
        field_name: &str,
        field_data: FieldData,
        zstd_level: i32,
    ) -> Result<()> {
        // NOTE: `PathBuf::with_extension` replaces the final `.ext` in the path,
        // so using `segment_dir.join("segment_id.field_name")` followed by
        // `with_extension("fst")` would strip `.field_name` and collapse every
        // field to the same file. The shared helper also keeps untrusted field
        // names from becoming paths.
        let filename = |ext: &str| field_sidecar_path(segment_dir, segment_id, field_name, ext);
        let fst_path = filename("fst");
        let post_path = filename("post");
        let meta_path = filename("meta");
        let norms_path = filename("norms");

        // 1. Build posting data and collect (term → TermPostings) in sorted order
        let mut post_data: Vec<u8> = Vec::new();
        let mut term_postings: HashMap<String, TermPostings> = HashMap::new();

        // Collect sorted terms (FST requires lexicographic order)
        let mut sorted_terms: Vec<String> =
            field_data.postings.terms().map(|s| s.to_owned()).collect();
        sorted_terms.sort_unstable();

        // Pre-compute per-term (doc_freq, ttf) ONCE.  The previous code
        // called `term_stats().find(..)` INSIDE the per-term loop — an
        // O(T²) scan that also re-summed every candidate's ttf on each
        // probe.  On numeric-heavy segments (thousands of distinct terms
        // per field, e.g. `latency_ms`/`@timestamp` string tokens) this
        // was the dominant flush-finalize CPU cost.
        let stats_by_term: HashMap<&str, (u32, u64)> = field_data
            .postings
            .term_stats()
            .map(|(t, df, ttf)| (t, (df, ttf)))
            .collect();

        for term in &sorted_terms {
            if let Some((offset, _skip)) = field_data.postings.encode_term(term, &mut post_data) {
                // Calculate doc_freq and ttf from the writer's internal stats
                let (doc_freq, ttf) = stats_by_term.get(term.as_str()).copied().unwrap_or((0, 0));

                let end_offset = post_data.len() as u64;
                let length = (end_offset - offset) as u32;

                term_postings.insert(
                    term.clone(),
                    TermPostings {
                        doc_frequency: doc_freq,
                        total_term_frequency: ttf,
                        postings_offset: offset,
                        postings_length: length,
                    },
                );
            }
        }

        // 2. Write postings file, wrapped in the `ZPS1` Zstd envelope.
        //
        // Bit-packed doc-id blocks look high-entropy to casual eyes
        // but the residual sections, vbyte run-lengths, and block-
        // level `num_bits` headers carry enough repetition that the
        // wrapper is worth it.  Zstd-19 squeezes ~1.4× tighter than
        // LZ4 on the XERJ bench's keyword-heavy postings (`name`
        // and `k` fields dominate).  Old segments using the ZPL1
        // (LZ4) envelope and the pre-magic raw mmap path are still
        // readable — see the open path's auto-detect block.
        //
        // Layout:
        //   "ZPS1"             4 bytes magic
        //   uncompressed_len   u32 little-endian
        //   payload            compressed_len bytes (zstd)
        let post_bytes_wrapped: Vec<u8> = if post_data.is_empty() {
            Vec::new()
        } else {
            let uncompressed_len = post_data.len() as u32;
            let compressed = zstd::bulk::compress(&post_data, zstd_level)
                .with_context(|| "ZPS1 zstd compress")?;
            let mut out = Vec::with_capacity(4 + 4 + compressed.len());
            out.extend_from_slice(POST_MAGIC_ZSTD);
            out.write_u32::<LittleEndian>(uncompressed_len).unwrap();
            out.extend_from_slice(&compressed);
            out
        };
        // RC4 W1 #10 — every FTS side-car write below goes through durable
        // temporary-file sync plus platform-appropriate publication: rename
        // and parent-directory fsync on Unix, write-through replacement on
        // Windows. These files are part of the segment publish chain: the WAL
        // entries they cover are pruned ~1 s after the flush, so side-cars
        // sitting only in the volatile page cache meant a power loss could
        // leave a registered segment with missing/torn FTS data (silently
        // wrong query results or unreadable fields) with no WAL to recover from.
        xerj_common::fsio::write_file_durable(&post_path, &post_bytes_wrapped)
            .with_context(|| format!("writing postings to {:?}", post_path))?;

        // 3. Build and write FST (term → meta byte offset).
        //
        // Pre-ZFM3 the FST value was `postings_offset` and the reader
        // had to look up `{df, ttf, length}` via a second HashMap<String,
        // TermPostings> lookup in the meta file.  Storing term strings
        // twice (once in the FST, once in the meta) was pure overhead on
        // high-cardinality keyword fields (~20 B/term).  Now the FST
        // value is the byte offset of the term's fixed-24-byte record
        // inside the meta's flat array — `meta[offset..offset + 24]`
        // holds df + ttf + postings_offset + length, so one FST hit →
        // one bounded mmap read.
        // FST streams into a same-directory temp file; fsync + rename +
        // platform-specific durable replacement publishes it (RC4 W1 #10 —
        // see the postings note above).
        let fst_tmp = filename("fst.tmp");
        let fst_file = BufWriter::new(
            File::create(&fst_tmp).with_context(|| format!("creating FST file {:?}", fst_tmp))?,
        );
        let mut fst_builder = MapBuilder::new(fst_file).with_context(|| "creating FST builder")?;

        // The i-th sorted term gets record slot `i`, whose byte offset
        // inside the flat meta array is `ZFM3_HEADER_LEN + i * 24`.  We
        // iterate the same sorted_terms list that built the meta body,
        // so ordering is consistent.
        for (i, term) in sorted_terms.iter().enumerate() {
            if term_postings.contains_key(term) {
                let meta_offset = (ZFM3_HEADER_LEN + i * ZFM3_RECORD_LEN) as u64;
                fst_builder
                    .insert(term.as_bytes(), meta_offset)
                    .with_context(|| format!("inserting term '{}' into FST", term))?;
            }
        }
        let mut fst_out = fst_builder.into_inner().with_context(|| "finishing FST")?;
        fst_out.flush().with_context(|| "flushing FST")?;
        fst_out
            .get_ref()
            .sync_all()
            .with_context(|| "fsyncing FST")?;
        drop(fst_out);
        xerj_common::fsio::replace_file_durable(&fst_tmp, &fst_path)
            .with_context(|| format!("publishing FST {:?}", fst_path))?;

        // 4. Write meta in the ZFM4 format — Zstd-19 envelope around
        //    the per-term records section; ZFM3-compatible header so
        //    `lookup_term` can decompress once at open time and use
        //    the in-memory `flat_records` Vec exactly as before.
        let has_positions = field_data.config.store_positions;
        let meta_bytes = encode_field_meta_v4(
            &field_data.stats,
            has_positions,
            &sorted_terms,
            &term_postings,
            zstd_level,
        )?;
        xerj_common::fsio::write_file_durable(&meta_path, &meta_bytes)
            .with_context(|| format!("writing meta to {:?}", meta_path))?;

        // 5. Write norms file — V4 M4.7 compact format.
        //
        // Old format was `(u32 doc_id, u16 norm)` pairs = 6 B per live
        // doc per field.  On 66.5 M nginx × 10 fields that was 3.99 GB
        // for norms alone (60 B/doc).  The new format stores ONE byte
        // per doc at the implicit index `doc_id`, using Lucene's
        // logarithmic quantisation: `byte ≈ norm_to_byte(field_len)`.
        // Missing docs get byte 0 (norm = 0).  Sparse fields still benefit
        // because the file is LZ4-compressed when > 1 KB of runs-of-zeros
        // make it worthwhile.
        //
        // Layout:
        //   "ZNM1"     4 bytes magic
        //   u8         encoding: 0 = dense u8, 1 = dense u8 + LZ4
        //   u32        max_doc_id + 1   (size of implicit array)
        //   u32        payload_len
        //   payload    dense array (u8 × max_doc_id+1) or LZ4(dense)
        let mut norms = field_data.norms;
        norms.sort_unstable_by_key(|(doc_id, _)| *doc_id);
        let max_doc_id: u32 = norms.last().map(|(d, _)| *d).unwrap_or(0);
        let dense_len = (max_doc_id as usize).saturating_add(1);
        let mut dense: Vec<u8> = vec![0u8; dense_len];
        for (doc_id, norm_byte) in &norms {
            dense[*doc_id as usize] = *norm_byte;
        }

        // Try LZ4 when the dense array is big enough for compression
        // to pay off (long runs of identical norms on low-entropy fields
        // like nginx `method` compress ~8×).
        let lz4_try = lz4_flex::compress_prepend_size(&dense);
        let (encoding, payload): (u8, &[u8]) = if dense.len() > 1024 && lz4_try.len() < dense.len()
        {
            (1, &lz4_try[..])
        } else {
            (0, &dense[..])
        };

        let mut norms_bytes: Vec<u8> = Vec::with_capacity(4 + 1 + 4 + 4 + payload.len());
        norms_bytes.extend_from_slice(NORMS_MAGIC);
        norms_bytes.push(encoding);
        norms_bytes.extend_from_slice(&(dense_len as u32).to_le_bytes());
        norms_bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        norms_bytes.extend_from_slice(payload);
        xerj_common::fsio::write_file_durable(&norms_path, &norms_bytes)
            .with_context(|| format!("writing norms to {:?}", norms_path))?;

        Ok(())
    }
}

/// One input segment of a postings-level merge.
///
/// See [`FtsIndexWriter::merge_from_segments`].
pub struct FtsMergeSource<'a> {
    /// A full (NOT `open_stats_only`) reader over the source segment.
    pub reader: &'a FtsIndexReader,
    /// Source document ordinal → merged document ordinal, `None` for a
    /// document the merge drops.  Exactly one entry per document in the
    /// source segment, in that segment's own ordinal order — which is the
    /// order its stored section holds, the same alignment the doc-values
    /// side-car already relies on.
    pub doc_map: &'a [Option<u32>],
}

/// Build one merged field by replaying every source segment's postings.
fn merge_field_from_segments(
    field: &str,
    config: &FieldIndexConfig,
    sources: &[FtsMergeSource<'_>],
) -> Result<FieldData> {
    let inputs: Vec<&FtsMergeSource<'_>> = sources
        .iter()
        .filter(|source| source.reader.field_stats(field).is_some())
        .collect();

    let Some(first) = inputs.first() else {
        return Ok(FieldData {
            config: config.clone(),
            postings: if config.store_positions {
                PostingsWriter::new()
            } else {
                PostingsWriter::new_no_positions()
            },
            stats: FieldStats::default(),
            norms: Vec::new(),
        });
    };

    // A posting list's on-disk shape decides how it decodes, so every input
    // must agree with every other AND with what this writer would emit.
    // Anything else would be a re-encode, not a merge — the caller
    // re-analyses instead.
    let has_positions = first.reader.field_has_positions(field);
    for source in &inputs {
        if source.reader.field_has_positions(field) != has_positions {
            bail!("FTS merge: sources disagree on positions for field '{field}'");
        }
    }
    if has_positions != config.store_positions {
        bail!(
            "FTS merge: field '{field}' is stored with store_positions={has_positions} but the \
             merge configuration asks for {}",
            config.store_positions
        );
    }

    let mut postings = if has_positions {
        PostingsWriter::new()
    } else {
        PostingsWriter::new_no_positions()
    };
    let mut norms: Vec<(u32, u8)> = Vec::new();
    let mut stats = FieldStats::default();

    for source in &inputs {
        let source_stats = source
            .reader
            .field_stats(field)
            .expect("filtered to the sources that carry this field")
            .clone();
        let doc_count = source.doc_map.len();
        // One flag per source document: which documents the postings mention.
        // A dropped document that no posting mentions had zero tokens, and
        // its `total_docs` seat is unreclaimable — see the method docs.
        let mut had_posting = vec![false; doc_count];
        let mut dropped_token_count: u64 = 0;
        let mut failure: Option<String> = None;

        source.reader.for_each_term(field, |term| {
            let Some(term_postings) = source.reader.lookup_term(field, term) else {
                failure = Some(format!(
                    "term '{term}' of field '{field}' is in the FST but not in the .meta"
                ));
                return false;
            };
            let Some(data) = source.reader.postings_data(field, &term_postings) else {
                failure = Some(format!(
                    "field '{field}' has no postings bytes for term '{term}' (a stats-only \
                     reader cannot be merged)"
                ));
                return false;
            };
            let mut decoded = crate::postings::PostingsReader::new_with_positions(
                data,
                term_postings.doc_frequency,
                has_positions,
            );
            // One run per (source, term), handed to the writer in one call:
            // a per-occurrence hand-off would re-descend the term BTreeMap
            // for every token in the corpus.
            let mut run: Vec<crate::postings::DecodedPosting> =
                Vec::with_capacity(term_postings.doc_frequency as usize);
            while let Some(mut posting) = decoded.next() {
                let ordinal = posting.doc_id as usize;
                if ordinal >= doc_count {
                    failure = Some(format!(
                        "field '{field}' has a posting for doc {ordinal}, past the {doc_count} \
                         documents the caller mapped"
                    ));
                    return false;
                }
                had_posting[ordinal] = true;
                match source.doc_map[ordinal] {
                    Some(merged) => {
                        posting.doc_id = merged;
                        run.push(posting);
                    }
                    None => dropped_token_count += posting.term_freq as u64,
                }
            }
            postings.extend_postings(term, run);
            true
        });
        if let Some(message) = failure {
            bail!("FTS merge: {message}");
        }
        // An input that maps no documents puts nothing into the output, so it
        // contributes no statistics either.  Any input that DOES have
        // postings while mapping no documents was already refused above.
        if doc_count == 0 {
            continue;
        }

        let mut norm_bytes = vec![0u8; doc_count];
        for (doc_id, byte) in source.reader.field_norm_bytes(field) {
            let ordinal = doc_id as usize;
            if ordinal >= doc_count {
                bail!(
                    "FTS merge: field '{field}' has a norm for doc {ordinal}, past the \
                     {doc_count} documents the caller mapped"
                );
            }
            norm_bytes[ordinal] = byte;
        }

        let mut dropped_docs: u64 = 0;
        let mut dropped_quantised_length: u64 = 0;
        for (ordinal, &byte) in norm_bytes.iter().enumerate() {
            match source.doc_map[ordinal] {
                // Byte 0 is how the dense norms array spells BOTH "field
                // absent" and "field length 1", so skipping it reproduces the
                // array a re-analysing writer would have built.
                Some(merged) => {
                    if byte != 0 {
                        norms.push((merged, byte));
                    }
                }
                None => {
                    if had_posting[ordinal] {
                        dropped_docs += 1;
                        dropped_quantised_length += norm_u8_to_u16(byte) as u64;
                    }
                }
            }
        }

        stats.total_docs += source_stats.total_docs.saturating_sub(dropped_docs);
        // A positioned field knows each dropped document's exact token count
        // (the sum of its term frequencies).  A docs-only field does not —
        // its `.post` never stored one — so its dropped length comes from the
        // quantised norm byte, which is exact up to length 7.
        let dropped_length = if has_positions {
            dropped_token_count
        } else {
            dropped_quantised_length
        };
        stats.total_field_length += source_stats
            .total_field_length
            .saturating_sub(dropped_length);
    }

    // The inputs' surviving documents interleave in the merged ordinal space,
    // so a term shared by several of them arrives as several ascending runs.
    postings.sort_postings_by_doc();

    Ok(FieldData {
        config: config.clone(),
        postings,
        stats,
        norms,
    })
}

/// Every field name each of `segment_ids` holds FTS side-cars for, in the same
/// order as `segment_ids`.
///
/// A segment's field set is whatever its documents happened to carry, so it is
/// not derivable from the mapping; the merge path needs it to know which
/// readers to open.  A portable field name IS its own filename component and
/// reads straight back; a digest-encoded component is resolved against
/// `known_fields`, and an unresolvable one is an error rather than a silent
/// omission — dropping a field here would drop its postings from the merged
/// segment and silently stop its terms matching.
///
/// Takes the whole batch at once because it costs ONE directory scan. A
/// converging index keeps thousands of segments in this directory (tens of
/// thousands of side-car files), and a merge batch has up to sixteen inputs:
/// scanning per input would re-walk that directory sixteen times per batch.
pub fn segments_indexed_field_names(
    segment_dir: &Path,
    segment_ids: &[&str],
    known_fields: &[String],
) -> Result<Vec<Vec<String>>> {
    let mut encoded: HashMap<String, &str> = HashMap::new();
    for name in known_fields {
        if let Cow::Owned(component) = field_file_component(name) {
            encoded.insert(component, name.as_str());
        }
    }

    let prefixes: Vec<String> = segment_ids.iter().map(|id| format!("{id}.")).collect();
    let mut per_segment: Vec<Vec<String>> = vec![Vec::new(); segment_ids.len()];
    let mut seen: Vec<std::collections::HashSet<String>> =
        vec![std::collections::HashSet::new(); segment_ids.len()];

    let entries = fs::read_dir(segment_dir)
        .with_context(|| format!("listing segment dir {:?}", segment_dir))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading segment dir {:?}", segment_dir))?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        for (index, prefix) in prefixes.iter().enumerate() {
            let Some(rest) = file_name.strip_prefix(prefix.as_str()) else {
                continue;
            };
            // Only the FST is enumerated: it is the file `FtsIndexReader::open`
            // gates a field's existence on, and taking one extension keeps a
            // four-file family from being counted four times.  `.fst.tmp`
            // leftovers fall out here too — their final component is `tmp`.
            let Some((component, extension)) = rest.rsplit_once('.') else {
                break;
            };
            if extension != "fst" || component.is_empty() {
                break;
            }
            let name = if component.starts_with(ENCODED_FIELD_COMPONENT_PREFIX) {
                match encoded.get(component) {
                    Some(name) => (*name).to_owned(),
                    None => bail!(
                        "segment {} has an FTS side-car component that no known field name \
                         digests to: {component}",
                        segment_ids[index]
                    ),
                }
            } else {
                component.to_owned()
            };
            if seen[index].insert(name.clone()) {
                per_segment[index].push(name);
            }
            break;
        }
    }

    for names in &mut per_segment {
        names.sort_unstable();
    }
    Ok(per_segment)
}

/// [`segments_indexed_field_names`] for a single segment.
pub fn segment_indexed_field_names(
    segment_dir: &Path,
    segment_id: &str,
    known_fields: &[String],
) -> Result<Vec<String>> {
    let mut names = segments_indexed_field_names(segment_dir, &[segment_id], known_fields)?;
    Ok(names.pop().unwrap_or_default())
}

const NORMS_MAGIC: &[u8; 4] = b"ZNM1";

/// Lucene-style logarithmic norm quantisation: maps a u16 field length
/// onto a u8.  Exactly 256 values with finer resolution at short lengths
/// (where BM25 is most sensitive).  Inverse loses precision at long
/// lengths, same as Lucene's `SmallFloat`.
#[inline]
fn norm_u16_to_u8(len: u16) -> u8 {
    if len == 0 {
        return 0;
    }
    // Clamp short lengths [1..8] to direct encoding (0..7).
    if len < 8 {
        return (len - 1) as u8 & 0x07;
    }
    // Logarithmic scale beyond 8.
    let l = (len as f64).log2();
    let v = ((l - 3.0) * 32.0 + 8.0) as i32;
    v.clamp(0, 255) as u8
}

#[inline]
fn norm_u8_to_u16(b: u8) -> u16 {
    if b < 8 {
        return (b + 1) as u16;
    }
    let l = ((b - 8) as f64) / 32.0 + 3.0;
    let v = l.exp2();
    v.min(u16::MAX as f64) as u16
}

// ── FtsIndexReader ────────────────────────────────────────────────────────────

/// Provides term lookup into a segment's FTS data.
///
/// Designed for mmap-friendly usage: the FST and postings data can be backed
/// by `memmap2::Mmap` buffers; only the meta JSON and norms are read eagerly.
pub struct FtsIndexReader {
    #[allow(dead_code)]
    segment_dir: PathBuf,
    #[allow(dead_code)]
    segment_id: String,
    /// Loaded per-field data
    fields: HashMap<String, LoadedField>,
    /// Set by [`FtsIndexReader::open_stats_only`]: the postings envelope and
    /// the norms table were NOT loaded, so this reader can answer
    /// `field_stats` / `term_doc_freq` (FST + `.meta` only) and nothing else.
    /// `postings_data` and `field_length` fail closed (`None`) rather than
    /// silently returning empty payloads — a stats-only reader handed to the
    /// searcher must produce zero hits, never wrong ones.
    stats_only: bool,
}

/// Backing storage for a field's postings bytes.
///
/// We prefer `Mmap` so `FtsIndexReader::open` allocates almost nothing per
/// segment and the OS page cache serves hot byte ranges.  Falls back to an
/// owned `Vec<u8>` when the file is in a read-only filesystem / tmpfs that
/// refuses to mmap (rare in practice but safer than panicking).
enum PostData {
    Mmap(Mmap),
    Owned(Vec<u8>),
}

impl PostData {
    #[inline]
    fn as_bytes(&self) -> &[u8] {
        match self {
            PostData::Mmap(m) => &m[..],
            PostData::Owned(v) => v.as_slice(),
        }
    }
}

/// Backing storage for a field's FST (term dictionary).
///
/// `fst::Map` is generic over `T: AsRef<[u8]>`.  `Mmap` implements
/// `AsRef<[u8]>` so `Map<Mmap>` is the mmap-backed fast path;
/// `Map<Vec<u8>>` is the owned fallback for filesystems that refuse mmap.
enum FstData {
    Mmap(Map<Mmap>),
    Owned(Map<Vec<u8>>),
}

impl FstData {
    #[inline]
    fn get(&self, key: &[u8]) -> Option<u64> {
        match self {
            FstData::Mmap(m) => m.get(key),
            FstData::Owned(m) => m.get(key),
        }
    }
}

struct LoadedField {
    /// FST term dictionary — mmap'd where possible.
    fst: FstData,
    /// Raw postings bytes — mmap'd where possible.
    post_data: PostData,
    /// Pre-parsed metadata (term postings, field stats) — small, stays owned.
    meta: FieldMeta,
    /// `(doc_id, field_length, quantised norm byte)`, sorted by doc_id.
    ///
    /// The raw byte rides along beside the dequantised length because the
    /// merge path must reproduce the source segment's norms file byte for
    /// byte: `norm_u16_to_u8(norm_u8_to_u16(b)) != b` for most `b`, so
    /// re-quantising the length would move BM25's length normalisation on
    /// every merged document.  `(u32, u16, u8)` still occupies 8 bytes
    /// after padding, so this costs no memory over the old `(u32, u16)`.
    norms: Vec<(u32, u16, u8)>,
}

impl FtsIndexReader {
    /// Load an existing segment's FTS data from disk.
    pub fn open(
        segment_dir: impl AsRef<Path>,
        segment_id: impl Into<String>,
        field_names: &[&str],
    ) -> Result<Self> {
        Self::open_inner(segment_dir, segment_id, field_names, false)
    }

    /// Open ONLY what BM25 collection statistics need: the FST term
    /// dictionary (mmap'd) and the small `.meta` side-car.
    ///
    /// Skips the two expensive parts of a full [`Self::open`]:
    ///
    ///  * the postings envelope — a whole-file `fs::read` plus a zstd
    ///    decompress of every posting list in the field, O(index bytes);
    ///  * the norms table — a whole-file `fs::read` plus decode, O(docs).
    ///
    /// That makes the per-segment `field_stats` + `term_doc_freq` pre-pass
    /// the index-wide scorer needs (#188) cost, per field, one mmap plus one
    /// `.meta` read-and-decode — for the current ZFM4 format that decode is
    /// a zstd decompress of the `num_terms × 24`-byte records section, i.e.
    /// O(field vocabulary), cheap but not free (#193) — instead of re-paying
    /// the full open a second time.
    ///
    /// The returned reader can answer [`Self::field_stats`],
    /// [`Self::term_doc_freq`] and [`Self::lookup_term`]; [`Self::postings_data`]
    /// and [`Self::field_length`] return `None` on it by construction, so
    /// handing one to a searcher yields zero hits rather than wrong ones.
    pub fn open_stats_only(
        segment_dir: impl AsRef<Path>,
        segment_id: impl Into<String>,
        field_names: &[&str],
    ) -> Result<Self> {
        Self::open_inner(segment_dir, segment_id, field_names, true)
    }

    fn open_inner(
        segment_dir: impl AsRef<Path>,
        segment_id: impl Into<String>,
        field_names: &[&str],
        stats_only: bool,
    ) -> Result<Self> {
        let segment_dir = segment_dir.as_ref().to_path_buf();
        let segment_id = segment_id.into();
        let encoded_layout = segment_uses_encoded_filename_layout(&segment_dir, &segment_id)?;
        let mut fields = HashMap::new();

        for &field_name in field_names {
            // The immutable segment discriminator is the sole authority.
            // Existence is deliberately irrelevant: otherwise a v1 field
            // whose literal name equals another field's digest component can
            // alias, and partial or stray files can alter layout selection.
            let filename = |ext: &str| -> Option<PathBuf> {
                if encoded_layout {
                    Some(field_sidecar_path(
                        &segment_dir,
                        &segment_id,
                        field_name,
                        ext,
                    ))
                } else {
                    legacy_field_sidecar_path(&segment_dir, &segment_id, field_name, ext)
                }
            };
            let Some(fst_path) = filename("fst") else {
                continue;
            };
            let post_path =
                filename("post").expect("FTS family path containment is extension-invariant");
            let meta_path =
                filename("meta").expect("FTS family path containment is extension-invariant");
            let norms_path =
                filename("norms").expect("FTS family path containment is extension-invariant");

            // Skip fields that haven't been indexed yet
            if !fst_path.exists() {
                continue;
            }

            // ── FST ──────────────────────────────────────────────────
            // Prefer mmap; fall back to fs::read if mmap fails (tmpfs etc.).
            let fst = match Self::mmap_file(&fst_path) {
                Ok(mmap) => {
                    FstData::Mmap(Map::new(mmap).with_context(|| "parsing FST map (mmap)")?)
                }
                Err(_) => {
                    let fst_bytes = fs::read(&fst_path)
                        .with_context(|| format!("reading FST {:?}", fst_path))?;
                    FstData::Owned(Map::new(fst_bytes).with_context(|| "parsing FST map (owned)")?)
                }
            };

            // ── Postings ─────────────────────────────────────────────
            //
            // Format detection (in priority order):
            //   * `ZPS1` magic → Zstd-19 envelope; decompress into an
            //     owned `Vec<u8>` once at open time.  Current writer.
            //   * `ZPL1` magic → legacy LZ4 envelope; same path,
            //     different codec.  Old segments stay readable.
            //   * No magic → pre-envelope raw bytes; mmap if the FS
            //     allows it, otherwise read into an owned buffer.
            //
            // The query path references `post_data` by slice in all
            // three cases, so there's no per-query decompress cost.
            //
            // STATS-ONLY: skip the read + decompress entirely — the caller
            // only wants `field_stats`/`term_doc_freq`, both of which live
            // in `.meta`/`.fst`.
            let raw_post = if stats_only {
                Vec::new()
            } else {
                fs::read(&post_path).with_context(|| format!("reading postings {:?}", post_path))?
            };
            let post_data = if raw_post.len() >= 8 && &raw_post[..4] == POST_MAGIC_ZSTD {
                let mut len_buf = [0u8; 4];
                len_buf.copy_from_slice(&raw_post[4..8]);
                let uncompressed_len = u32::from_le_bytes(len_buf) as usize;
                let decompressed = zstd::bulk::decompress(&raw_post[8..], uncompressed_len)
                    .map_err(|e| anyhow::anyhow!("ZPS1 postings decompress failed: {e}"))?;
                PostData::Owned(decompressed)
            } else if raw_post.len() >= 4 && &raw_post[..4] == POST_MAGIC_LZ4 {
                let decompressed = lz4_flex::decompress_size_prepended(&raw_post[4..])
                    .map_err(|e| anyhow::anyhow!("ZPL1 postings decompress failed: {e}"))?;
                PostData::Owned(decompressed)
            } else if !raw_post.is_empty() {
                // Legacy raw path — prefer mmap, fall back to owned.
                match Self::mmap_file(&post_path) {
                    Ok(mmap) => PostData::Mmap(mmap),
                    Err(_) => PostData::Owned(raw_post),
                }
            } else {
                PostData::Owned(Vec::new())
            };

            // ── Meta + norms — small, read eagerly ───────────────────
            // Auto-detect the on-disk format: ZFM1 binary (new) vs
            // legacy pretty-JSON (from pre-M4.7 segments).  We keep the
            // JSON fallback so upgrades don't require a reindex.
            let meta_bytes =
                fs::read(&meta_path).with_context(|| format!("reading meta {:?}", meta_path))?;
            let is_binary = meta_bytes.len() >= 4
                && (&meta_bytes[..4] == META_MAGIC_V1
                    || &meta_bytes[..4] == META_MAGIC_V2
                    || &meta_bytes[..4] == META_MAGIC_V3
                    || &meta_bytes[..4] == META_MAGIC_V4);
            let meta: FieldMeta = if is_binary {
                decode_field_meta_binary(&meta_bytes)
                    .with_context(|| "parsing binary field meta")?
            } else {
                // Legacy JSON path — same shape as before the refactor.
                #[derive(Deserialize)]
                struct LegacyFieldMeta {
                    stats: FieldStats,
                    terms: HashMap<String, LegacyTermPostings>,
                }
                #[derive(Deserialize)]
                struct LegacyTermPostings {
                    doc_frequency: u32,
                    total_term_frequency: u64,
                    postings_offset: u64,
                    postings_length: u32,
                }
                let legacy: LegacyFieldMeta = serde_json::from_slice(&meta_bytes)
                    .with_context(|| "parsing legacy field meta JSON")?;
                FieldMeta {
                    stats: legacy.stats,
                    terms: legacy
                        .terms
                        .into_iter()
                        .map(|(k, v)| {
                            (
                                k,
                                SerialTermPostings {
                                    doc_frequency: v.doc_frequency,
                                    total_term_frequency: v.total_term_frequency,
                                    postings_offset: v.postings_offset,
                                    postings_length: v.postings_length,
                                },
                            )
                        })
                        .collect(),
                    has_positions: true, // legacy segments always had positions
                    fst_value_format: FstValueFormat::PostingsOffset,
                    flat_records: Vec::new(),
                }
            };

            // STATS-ONLY: the norms table is a whole-file read + decode that
            // is O(docs); the statistics pre-pass never asks for a doc length.
            let norms = if stats_only {
                Vec::new()
            } else {
                Self::load_norms(&norms_path)?
            };

            fields.insert(
                field_name.to_owned(),
                LoadedField {
                    fst,
                    post_data,
                    meta,
                    norms,
                },
            );
        }

        Ok(Self {
            segment_dir,
            segment_id,
            fields,
            stats_only,
        })
    }

    /// Memory-map a file.
    ///
    /// SAFETY: mmap is unsafe in Rust because another process could mutate
    /// the file under us.  In xerj segment files are written once to a
    /// staging path and `rename`d atomically; after rename they're
    /// immutable until merged away.  We only mmap these stable files, so
    /// this is safe in practice.
    fn mmap_file(path: &Path) -> Result<Mmap> {
        let file = File::open(path).with_context(|| format!("opening {:?} for mmap", path))?;
        // Zero-length files would panic — short-circuit.
        let len = file.metadata().with_context(|| "stat for mmap")?.len();
        if len == 0 {
            return Err(anyhow::anyhow!("empty file"));
        }
        let mmap = unsafe { Mmap::map(&file) }.with_context(|| format!("mmap {:?}", path))?;
        Ok(mmap)
    }

    fn load_norms(path: &Path) -> Result<Vec<(u32, u16, u8)>> {
        let bytes = fs::read(path).with_context(|| format!("opening norms {:?}", path))?;
        // V4 M4.7 compact format starts with `NORMS_MAGIC`; legacy starts
        // with a raw u32 count (whose first byte almost never matches 'Z').
        if bytes.len() >= 4 && &bytes[..4] == NORMS_MAGIC {
            if bytes.len() < 4 + 1 + 4 + 4 {
                return Err(anyhow::anyhow!("norms: truncated ZNM1 header"));
            }
            let encoding = bytes[4];
            let dense_len = u32::from_le_bytes(bytes[5..9].try_into().unwrap()) as usize;
            let payload_len = u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;
            let payload = &bytes[13..13 + payload_len];
            let dense: Vec<u8> = match encoding {
                0 => payload.to_vec(),
                1 => lz4_flex::decompress_size_prepended(payload)
                    .map_err(|e| anyhow::anyhow!("norms: lz4 decompress: {e}"))?,
                _ => return Err(anyhow::anyhow!("norms: unknown encoding {}", encoding)),
            };
            if dense.len() != dense_len {
                return Err(anyhow::anyhow!(
                    "norms: dense length mismatch {} != {}",
                    dense.len(),
                    dense_len
                ));
            }
            // Materialise (doc_id, norm) pairs only for live docs so the
            // rest of the engine (which expects `Vec<(u32, u16)>`) is
            // unchanged.  Zero bytes → missing.
            let mut norms = Vec::new();
            for (doc_id, &b) in dense.iter().enumerate() {
                if b != 0 {
                    norms.push((doc_id as u32, norm_u8_to_u16(b), b));
                }
            }
            Ok(norms)
        } else {
            // Legacy path (pre-M4.7): u32 count + count × (u32 doc_id, u16 norm).
            let mut cur = std::io::Cursor::new(&bytes[..]);
            let count = cur.read_u32::<LittleEndian>()? as usize;
            let mut norms = Vec::with_capacity(count);
            for _ in 0..count {
                let doc_id = cur.read_u32::<LittleEndian>()?;
                let norm = cur.read_u16::<LittleEndian>()?;
                // The pre-M4.7 format stored an unquantised length; the byte a
                // writer would emit for it is what a merge must carry forward.
                norms.push((doc_id, norm, norm_u16_to_u8(norm)));
            }
            Ok(norms)
        }
    }

    /// Heap bytes this reader retains, for the segment-cache budget.
    ///
    /// Only *owned* allocations are counted. `Mmap` backings are deliberately
    /// excluded: they are page-cache pages the kernel can reclaim, not heap the
    /// process is responsible for, and charging them would make the budget
    /// refuse to cache readers that cost almost nothing to hold.
    ///
    /// The dominant terms are the two decompressed blobs. A compressed `.post`
    /// (`ZPS1`) or `.meta` (`ZFM4`) cannot be mmap'd — it must be inflated into
    /// an owned `Vec` at open time — so on a modern segment this is essentially
    /// `post_uncompressed + meta_uncompressed`.
    pub fn retained_bytes(&self) -> u64 {
        let mut total: u64 = 0;
        for (name, f) in &self.fields {
            total += name.len() as u64;
            if let PostData::Owned(v) = &f.post_data {
                total += v.len() as u64;
            }
            if let FstData::Owned(m) = &f.fst {
                total += m.as_fst().as_bytes().len() as u64;
            }
            total += f.meta.flat_records.len() as u64;
            // `norms` is Vec<(u32, u16)>, which pads to 8 bytes per element.
            total += (f.norms.len() as u64) * 8;
            // Legacy ZFM1/ZFM2 keep per-term metadata in a HashMap instead of
            // the flat array; approximate it rather than walking every entry.
            total += (f.meta.terms.len() as u64) * 64;
        }
        total
    }

    /// Look up a term in a field.
    ///
    /// Returns `Some(TermPostings)` if the term exists, `None` otherwise.
    pub fn lookup_term(&self, field: &str, term: &str) -> Option<TermPostings> {
        let loaded = self.fields.get(field)?;
        let fst_value = loaded.fst.get(term.as_bytes())?;
        match loaded.meta.fst_value_format {
            FstValueFormat::MetaByteOffset => {
                // ZFM3: the FST value is the byte offset of a 24-byte
                // record inside the flat `.meta` array.
                let off = fst_value as usize;
                let end = off.checked_add(ZFM3_RECORD_LEN)?;
                let rec = loaded
                    .meta
                    .flat_records
                    .get(off.checked_sub(ZFM3_HEADER_LEN)?..end.checked_sub(ZFM3_HEADER_LEN)?)?;
                // rec is exactly 24 bytes: df(4) ttf(8) off(8) len(4).
                let mut cur = std::io::Cursor::new(rec);
                let doc_frequency = cur.read_u32::<LittleEndian>().ok()?;
                let total_term_frequency = cur.read_u64::<LittleEndian>().ok()?;
                let postings_offset = cur.read_u64::<LittleEndian>().ok()?;
                let postings_length = cur.read_u32::<LittleEndian>().ok()?;
                Some(TermPostings {
                    doc_frequency,
                    total_term_frequency,
                    postings_offset,
                    postings_length,
                })
            }
            FstValueFormat::PostingsOffset => {
                // Legacy ZFM1/ZFM2: term metadata lives in `terms`,
                // keyed by term string.
                let _ = fst_value;
                let serial = loaded.meta.terms.get(term)?;
                Some(TermPostings {
                    doc_frequency: serial.doc_frequency,
                    total_term_frequency: serial.total_term_frequency,
                    postings_offset: serial.postings_offset,
                    postings_length: serial.postings_length,
                })
            }
        }
    }

    /// Get the raw postings bytes for a term (to hand to `PostingsReader`).
    ///
    /// Always `None` on a [`Self::open_stats_only`] reader — it never read the
    /// postings envelope, so a caller that got this far would otherwise decode
    /// an empty buffer and silently see zero postings.
    pub fn postings_data<'a>(&'a self, field: &str, tp: &TermPostings) -> Option<&'a [u8]> {
        if self.stats_only {
            return None;
        }
        let loaded = self.fields.get(field)?;
        let start = tp.postings_offset as usize;
        let end = start + tp.postings_length as usize;
        loaded.post_data.as_bytes().get(start..end)
    }

    /// Per-segment document frequency for a term (number of docs containing
    /// the term in this segment's postings).  Returns `None` if the term is
    /// not present in the segment's FST.
    ///
    /// This is the O(1) hook used by the `shortcut_total_hit_count` fast
    /// path: `sum across segments` → total doc count for a `term` query
    /// without touching a single posting list.  Mirrors Lucene's
    /// `TermsEnum.docFreq()` contract.
    pub fn term_doc_freq(&self, field: &str, term: &str) -> Option<u32> {
        // Re-use the full lookup path — it handles ZFM3's flat-record
        // decode and ZFM1/ZFM2's HashMap lookup uniformly.
        self.lookup_term(field, term).map(|tp| tp.doc_frequency)
    }

    /// Get the field stats (for BM25 scorer construction).
    pub fn field_stats(&self, field: &str) -> Option<&FieldStats> {
        self.fields.get(field).map(|f| &f.meta.stats)
    }

    /// `true` when the field's posting lists carry term freqs + positions.
    /// `false` for docs-only fields (keyword, numeric, ip), in which case
    /// the caller must construct a `PostingsReader` with `has_positions
    /// = false` so the decoder synthesises freq=1 per posting.
    ///
    /// Returns `true` for unknown fields (safe default matching legacy
    /// ZFM1 segments).
    pub fn field_has_positions(&self, field: &str) -> bool {
        self.fields
            .get(field)
            .map(|f| f.meta.has_positions)
            .unwrap_or(true)
    }

    /// Look up the field length (norm) for a specific document.
    /// Returns `None` if the document has no data for this field — which is
    /// every document on a [`Self::open_stats_only`] reader, whose norms
    /// table was deliberately not loaded.
    pub fn field_length(&self, field: &str, doc_id: u32) -> Option<u16> {
        let loaded = self.fields.get(field)?;
        // Binary search by doc_id
        loaded
            .norms
            .binary_search_by_key(&doc_id, |(id, _, _)| *id)
            .ok()
            .map(|idx| loaded.norms[idx].1)
    }

    /// Every `(doc_id, quantised norm byte)` this field records, ascending by
    /// doc id.  Docs whose norm byte is zero are absent — the dense on-disk
    /// array cannot tell "field missing" from "field length 1" (both encode
    /// as byte 0), and neither can this.
    ///
    /// Exists for the merge path: a merged segment must write the SAME norm
    /// byte the source segment holds, and [`Self::field_length`]'s
    /// dequantised `u16` cannot be re-quantised back to it.
    pub fn field_norm_bytes(&self, field: &str) -> impl Iterator<Item = (u32, u8)> + '_ {
        self.fields.get(field).into_iter().flat_map(|loaded| {
            loaded
                .norms
                .iter()
                .map(|(doc_id, _, byte)| (*doc_id, *byte))
        })
    }

    /// Stream every term of `field` through `f` in lexicographic order,
    /// stopping as soon as `f` returns `false`.
    ///
    /// Unlike [`Self::all_terms`] this never materialises the dictionary —
    /// multi-term expansion (prefix/wildcard/fuzzy) walks dictionaries with
    /// millions of entries, and building a `Vec<String>` first both doubles
    /// the walk cost and makes the whole enumeration an uninterruptible
    /// unit (RC4 blocker 12: search `timeout` could never fire during
    /// expansion). The callback's `bool` return is the cooperative
    /// cancellation hook.
    pub fn for_each_term<F: FnMut(&str) -> bool>(&self, field: &str, mut f: F) {
        let Some(loaded) = self.fields.get(field) else {
            return;
        };
        use fst::Streamer;
        match &loaded.fst {
            FstData::Mmap(m) => {
                let mut stream = m.stream();
                while let Some((key, _)) = stream.next() {
                    if let Ok(s) = std::str::from_utf8(key) {
                        if !f(s) {
                            return;
                        }
                    }
                }
            }
            FstData::Owned(m) => {
                let mut stream = m.stream();
                while let Some((key, _)) = stream.next() {
                    if let Ok(s) = std::str::from_utf8(key) {
                        if !f(s) {
                            return;
                        }
                    }
                }
            }
        }
    }

    /// Enumerate all terms in a field (lexicographic order, for debugging/admin).
    pub fn all_terms(&self, field: &str) -> Vec<String> {
        let Some(loaded) = self.fields.get(field) else {
            return Vec::new();
        };
        use fst::Streamer;
        let mut terms = Vec::new();
        match &loaded.fst {
            FstData::Mmap(m) => {
                let mut stream = m.stream();
                while let Some((key, _)) = stream.next() {
                    if let Ok(s) = std::str::from_utf8(key) {
                        terms.push(s.to_owned());
                    }
                }
            }
            FstData::Owned(m) => {
                let mut stream = m.stream();
                while let Some((key, _)) = stream.next() {
                    if let Ok(s) = std::str::from_utf8(key) {
                        terms.push(s.to_owned());
                    }
                }
            }
        }
        terms
    }

    /// Returns `true` if a term exists in the field's FST (O(m) where m = term length).
    pub fn term_exists(&self, field: &str, term: &str) -> bool {
        self.fields
            .get(field)
            .map(|f| f.fst.get(term.as_bytes()).is_some())
            .unwrap_or(false)
    }

    pub fn indexed_fields(&self) -> Vec<&str> {
        self.fields.keys().map(|s| s.as_str()).collect()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use tempfile::TempDir;

    fn make_registry() -> Arc<AnalyzerRegistry> {
        Arc::new(AnalyzerRegistry::default())
    }

    #[test]
    fn write_and_read_single_field() {
        let dir = TempDir::new().unwrap();
        let registry = make_registry();

        let mut writer = FtsIndexWriter::new(dir.path(), "seg0", registry);

        let docs: Vec<HashMap<String, FieldValues>> = vec![
            [("body".to_owned(), FieldValues::from("the quick brown fox"))]
                .into_iter()
                .collect(),
            [("body".to_owned(), FieldValues::from("the lazy dog"))]
                .into_iter()
                .collect(),
            [("body".to_owned(), FieldValues::from("quick fox lazy dog"))]
                .into_iter()
                .collect(),
        ];

        for (i, doc) in docs.iter().enumerate() {
            writer.add_document(i as u32, doc);
        }

        let stats = writer.finish().unwrap();
        assert!(stats.contains_key("body"));
        assert_eq!(stats["body"].total_docs, 3);

        // Read back
        let reader = FtsIndexReader::open(dir.path(), "seg0", &["body"]).unwrap();

        // "fox" should appear in docs 0 and 2 (after stemming by standard analyzer → "fox")
        // "quick" → "quick" (not stemmed significantly)
        let all_terms = reader.all_terms("body");
        assert!(!all_terms.is_empty(), "should have indexed terms");

        // "lazi" is the Snowball stem of "lazy"
        let lazi_exists = reader.term_exists("body", "lazi") || reader.term_exists("body", "lazy");
        assert!(
            lazi_exists,
            "lazy/lazi should be indexed; terms={:?}",
            all_terms
        );

        // Field stats round-trip
        let fs = reader.field_stats("body").unwrap();
        assert_eq!(fs.total_docs, 3);

        // Norms: each doc should have a norm entry
        for i in 0u32..3 {
            let norm = reader.field_length("body", i);
            assert!(norm.is_some(), "doc {} should have a norm", i);
            assert!(norm.unwrap() > 0, "norm should be non-zero for doc {}", i);
        }
    }

    /// #332 — a `keyword`-analyzed field with N values must produce N terms,
    /// and must NOT produce the space-joined concatenation of them.
    #[test]
    fn keyword_field_indexes_each_value_as_its_own_term() {
        let dir = TempDir::new().unwrap();
        let mut writer = FtsIndexWriter::new(dir.path(), "seg-mv-kw", make_registry());
        writer.configure_field(
            "tags",
            FieldIndexConfig {
                analyzer: "keyword".to_owned(),
                ..Default::default()
            },
        );
        writer.add_document(
            0,
            &[(
                "tags".to_owned(),
                FieldValues::from(vec!["red".to_owned(), "blue".to_owned()]),
            )]
            .into_iter()
            .collect(),
        );
        writer.finish().unwrap();

        let reader = FtsIndexReader::open(dir.path(), "seg-mv-kw", &["tags"]).unwrap();
        let mut terms = reader.all_terms("tags");
        terms.sort();
        assert_eq!(
            terms,
            vec!["blue".to_owned(), "red".to_owned()],
            "each array element is its own keyword term, and \"red blue\" is not a term"
        );
        // The norm is the SUM over values, as in Lucene: one token each.
        assert_eq!(reader.field_length("tags", 0), Some(2));
        // One (doc, field) pair, however many values it carried.
        assert_eq!(reader.field_stats("tags").unwrap().total_docs, 1);
    }

    /// #332 — consecutive values are separated by `POSITION_INCREMENT_GAP`, so
    /// a phrase cannot straddle the boundary between two array elements.
    #[test]
    fn position_increment_gap_separates_consecutive_values() {
        use crate::postings::PostingsReader;

        let dir = TempDir::new().unwrap();
        let mut writer = FtsIndexWriter::new(dir.path(), "seg-mv-pos", make_registry());
        writer.configure_field(
            "notes",
            FieldIndexConfig {
                analyzer: "whitespace".to_owned(),
                ..Default::default()
            },
        );
        writer.add_document(
            0,
            &[(
                "notes".to_owned(),
                FieldValues::from(vec!["alpha bravo".to_owned(), "charlie delta".to_owned()]),
            )]
            .into_iter()
            .collect(),
        );
        writer.finish().unwrap();

        let reader = FtsIndexReader::open(dir.path(), "seg-mv-pos", &["notes"]).unwrap();
        let position_of = |term: &str| -> u32 {
            let tp = reader.lookup_term("notes", term).expect("term present");
            let data = reader.postings_data("notes", &tp).expect("postings");
            let mut pr = PostingsReader::new_with_positions(data, tp.doc_frequency, true);
            let p = pr.next().expect("one posting");
            p.positions[0]
        };

        // Value 0 occupies positions 0 and 1.
        assert_eq!(position_of("alpha"), 0);
        assert_eq!(position_of("bravo"), 1);
        // Value 1 restarts one past the previous value, plus the gap.
        assert_eq!(position_of("charlie"), 2 + POSITION_INCREMENT_GAP);
        assert_eq!(position_of("delta"), 3 + POSITION_INCREMENT_GAP);
        // `bravo charlie` is therefore 100 positions apart, not adjacent —
        // no realistic `slop` bridges it.
        assert_eq!(position_of("charlie") - position_of("bravo"), 101);
    }

    #[test]
    fn term_lookup_returns_correct_metadata() {
        let dir = TempDir::new().unwrap();
        let registry = make_registry();

        let mut writer = FtsIndexWriter::new(dir.path(), "seg1", registry);

        // Use keyword analyzer to avoid stemming surprises
        let cfg = FieldIndexConfig {
            analyzer: "whitespace".to_owned(),
            ..Default::default()
        };
        writer.configure_field("title", cfg);

        let docs = [
            [("title".to_owned(), FieldValues::from("hello world"))]
                .into_iter()
                .collect(),
            [("title".to_owned(), FieldValues::from("hello rust"))]
                .into_iter()
                .collect(),
        ];
        for (i, doc) in docs.iter().enumerate() {
            writer.add_document(i as u32, doc);
        }
        writer.finish().unwrap();

        let reader = FtsIndexReader::open(dir.path(), "seg1", &["title"]).unwrap();

        // "hello" appears in both docs
        let tp = reader
            .lookup_term("title", "hello")
            .expect("'hello' should be in index");
        assert_eq!(tp.doc_frequency, 2);
        assert_eq!(tp.total_term_frequency, 2);

        // "world" appears in 1 doc
        let tp = reader
            .lookup_term("title", "world")
            .expect("'world' should be in index");
        assert_eq!(tp.doc_frequency, 1);

        // Postings data retrievable
        let tp = reader.lookup_term("title", "hello").unwrap();
        let data = reader.postings_data("title", &tp);
        assert!(data.is_some() && !data.unwrap().is_empty());
    }

    #[test]
    fn safe_field_component_preserves_existing_filename_layout() {
        let dir = TempDir::new().unwrap();
        let mut writer = FtsIndexWriter::new(dir.path(), "seg-safe", make_registry());
        let fields = ["title.body-2", "@timestamp", "space field", "配置.名字"];
        let document: HashMap<String, FieldValues> = fields
            .into_iter()
            .enumerate()
            .map(|(index, field)| (field.to_owned(), FieldValues::One(format!("needle{index}"))))
            .collect();
        writer.add_document(0, &document);
        writer.finish().unwrap();

        for field in fields {
            assert_eq!(field_file_component(field), Cow::Borrowed(field));
            for extension in ["fst", "post", "meta", "norms"] {
                assert!(dir
                    .path()
                    .join(format!("seg-safe.{field}.{extension}"))
                    .is_file());
            }
        }
    }

    fn write_legacy_raw_field_fixture(segment_dir: &Path, segment_id: &str, legacy_field: &str) {
        let source_field = "legacy_fixture_source";
        let mut writer = FtsIndexWriter::new(segment_dir, segment_id, make_registry());
        let document = [(source_field.to_owned(), FieldValues::from("legacyneedle"))]
            .into_iter()
            .collect();
        writer.add_document(0, &document);
        writer.finish().unwrap();
        for extension in ["fst", "post", "meta", "norms"] {
            let source = field_sidecar_path(segment_dir, segment_id, source_field, extension);
            let destination =
                legacy_field_sidecar_path(segment_dir, segment_id, legacy_field, extension)
                    .expect("test fixture must be one component on this host");
            fs::rename(source, destination).unwrap();
        }
    }

    #[test]
    fn new_reader_opens_existing_single_component_legacy_fields() {
        let root = TempDir::new().unwrap();
        let segment_dir = root.path().join("segments");
        fs::create_dir_all(&segment_dir).unwrap();
        let overlong = "a".repeat(MAX_LITERAL_FIELD_COMPONENT_BYTES + 1);
        let platform_fields: &[&str] = if cfg!(windows) {
            &[]
        } else {
            &[r"legacy\backslash", "legacy:colon*question?", "trailing "]
        };
        let fields: Vec<&str> = vec![
            "@timestamp",
            "space field",
            "配置.名字",
            "CON.txt",
            ENCODED_FIELD_COMPONENT_PREFIX,
            overlong.as_str(),
        ]
        .into_iter()
        .chain(platform_fields.iter().copied())
        .collect();

        for (index, field) in fields.iter().enumerate() {
            let segment_id = format!("legacy-{index}");
            write_legacy_raw_field_fixture(&segment_dir, &segment_id, field);
            let raw_fst = legacy_field_sidecar_path(&segment_dir, &segment_id, field, "fst")
                .expect("legacy field must remain contained");
            assert!(raw_fst.is_file());

            let reader = FtsIndexReader::open(&segment_dir, &segment_id, &[*field]).unwrap();
            assert!(reader.term_exists(field, "legacyneedle"));
        }
    }

    fn collision_fields() -> (String, String) {
        let unsafe_name = "CON".to_owned();
        let legacy_alias = format!(
            "{ENCODED_FIELD_COMPONENT_PREFIX}{:x}",
            Sha256::digest(unsafe_name.as_bytes())
        );
        (unsafe_name, legacy_alias)
    }

    fn write_v1_collision_fixture(segment_dir: &Path, segment_id: &str) -> (String, String) {
        let (unsafe_name, legacy_alias) = collision_fields();
        let source_fields = ["source_unsafe", "source_alias"];
        let mut writer = FtsIndexWriter::new(segment_dir, segment_id, make_registry());
        let document: HashMap<String, FieldValues> = [
            (
                source_fields[0].to_owned(),
                FieldValues::from("unsafeneedle"),
            ),
            (
                source_fields[1].to_owned(),
                FieldValues::from("aliasneedle"),
            ),
        ]
        .into_iter()
        .collect();
        writer.add_document(0, &document);
        writer.finish().unwrap();
        for (source, destination) in source_fields
            .into_iter()
            .zip([unsafe_name.as_str(), legacy_alias.as_str()])
        {
            for extension in ["fst", "post", "meta", "norms"] {
                fs::rename(
                    field_sidecar_path(segment_dir, segment_id, source, extension),
                    legacy_field_sidecar_path(segment_dir, segment_id, destination, extension)
                        .unwrap(),
                )
                .unwrap();
            }
        }
        (unsafe_name, legacy_alias)
    }

    fn assert_collision_fields_read(
        segment_dir: &Path,
        segment_id: &str,
        unsafe_name: &str,
        legacy_alias: &str,
    ) {
        for stats_only in [false, true] {
            let reader = if stats_only {
                FtsIndexReader::open_stats_only(
                    segment_dir,
                    segment_id,
                    &[unsafe_name, legacy_alias],
                )
            } else {
                FtsIndexReader::open(segment_dir, segment_id, &[unsafe_name, legacy_alias])
            }
            .unwrap();
            assert!(reader.term_exists(unsafe_name, "unsafeneedle"));
            assert!(!reader.term_exists(unsafe_name, "aliasneedle"));
            assert!(reader.term_exists(legacy_alias, "aliasneedle"));
            assert!(!reader.term_exists(legacy_alias, "unsafeneedle"));
        }
    }

    #[test]
    fn discriminator_prevents_same_segment_v1_digest_alias_collision_across_reader_reopens() {
        let root = TempDir::new().unwrap();
        let segment_dir = root.path().join("segments");
        fs::create_dir_all(&segment_dir).unwrap();
        let segment_id = "v1-collision";
        let (unsafe_name, legacy_alias) = write_v1_collision_fixture(&segment_dir, segment_id);
        assert!(!segment_filename_layout_v2_marker_path(&segment_dir, segment_id).exists());
        assert_collision_fields_read(&segment_dir, segment_id, &unsafe_name, &legacy_alias);
        // Reopening creates a fresh reader and must make the same selection.
        assert_collision_fields_read(&segment_dir, segment_id, &unsafe_name, &legacy_alias);
    }

    #[test]
    fn discriminator_keeps_same_segment_v2_digest_alias_fields_distinct_across_reader_reopens() {
        let root = TempDir::new().unwrap();
        let segment_dir = root.path().join("segments");
        let segment_id = "v2-collision";
        let (unsafe_name, legacy_alias) = collision_fields();
        let mut writer = FtsIndexWriter::new(&segment_dir, segment_id, make_registry());
        let document: HashMap<String, FieldValues> = [
            (unsafe_name.clone(), FieldValues::from("unsafeneedle")),
            (legacy_alias.clone(), FieldValues::from("aliasneedle")),
        ]
        .into_iter()
        .collect();
        writer.add_document(0, &document);
        writer.publish_encoded_filename_layout().unwrap();
        writer.finish().unwrap();
        assert!(segment_filename_layout_v2_marker_path(&segment_dir, segment_id).is_file());
        assert_ne!(
            field_sidecar_path(&segment_dir, segment_id, &unsafe_name, "fst"),
            field_sidecar_path(&segment_dir, segment_id, &legacy_alias, "fst")
        );
        assert_collision_fields_read(&segment_dir, segment_id, &unsafe_name, &legacy_alias);
        assert_collision_fields_read(&segment_dir, segment_id, &unsafe_name, &legacy_alias);
    }

    #[test]
    fn v1_discriminator_state_ignores_stray_and_partial_v2_families_and_fails_on_raw_corruption() {
        let root = TempDir::new().unwrap();
        let segment_dir = root.path().join("segments");
        fs::create_dir_all(&segment_dir).unwrap();
        let segment_id = "v1-stray-v2";
        let field = ".";
        write_legacy_raw_field_fixture(&segment_dir, segment_id, field);
        let raw_fst = legacy_field_sidecar_path(&segment_dir, segment_id, field, "fst").unwrap();
        let encoded_fst = field_sidecar_path(&segment_dir, segment_id, field, "fst");
        let encoded_meta = field_sidecar_path(&segment_dir, segment_id, field, "meta");
        fs::copy(&raw_fst, &encoded_fst).unwrap();
        fs::write(&encoded_meta, b"corrupt-stray-v2-meta").unwrap();

        let full = FtsIndexReader::open(&segment_dir, segment_id, &[field]).unwrap();
        assert!(full.term_exists(field, "legacyneedle"));
        let stats = FtsIndexReader::open_stats_only(&segment_dir, segment_id, &[field]).unwrap();
        assert!(stats.term_exists(field, "legacyneedle"));

        // The raw v1 family does not make an invalid visible discriminator
        // ignorable. Layout selection is discriminator-only and corruption
        // fails closed before either family is opened.
        let marker = segment_filename_layout_v2_marker_path(&segment_dir, segment_id);
        fs::write(&marker, b"corrupt-layout-marker-over-raw-v1-family").unwrap();
        assert!(FtsIndexReader::open(&segment_dir, segment_id, &[field]).is_err());
        assert!(FtsIndexReader::open_stats_only(&segment_dir, segment_id, &[field]).is_err());
        fs::remove_file(&marker).unwrap();

        let raw_post = legacy_field_sidecar_path(&segment_dir, segment_id, field, "post").unwrap();
        let raw_post_bytes = fs::read(&raw_post).unwrap();
        fs::remove_file(&raw_post).unwrap();
        assert!(FtsIndexReader::open(&segment_dir, segment_id, &[field]).is_err());
        assert!(
            FtsIndexReader::open_stats_only(&segment_dir, segment_id, &[field])
                .unwrap()
                .term_exists(field, "legacyneedle")
        );
        fs::write(&raw_post, raw_post_bytes).unwrap();

        let raw_meta = legacy_field_sidecar_path(&segment_dir, segment_id, field, "meta").unwrap();
        fs::write(&raw_meta, b"corrupt-selected-v1-meta").unwrap();
        assert!(FtsIndexReader::open(&segment_dir, segment_id, &[field]).is_err());
        assert!(FtsIndexReader::open_stats_only(&segment_dir, segment_id, &[field]).is_err());
    }

    #[test]
    fn v2_discriminator_state_ignores_raw_strays_and_fails_closed_on_partial_or_corrupt_state() {
        let root = TempDir::new().unwrap();
        let segment_dir = root.path().join("segments");
        let segment_id = "v2-partial";
        let field = ".";
        let mut writer = FtsIndexWriter::new(&segment_dir, segment_id, make_registry());
        writer.add_document(
            0,
            &[(field.to_owned(), FieldValues::from("encodedneedle"))]
                .into_iter()
                .collect(),
        );
        writer.publish_encoded_filename_layout().unwrap();
        writer.finish().unwrap();

        // A corrupt complete raw family is a stray in explicit v2 state.
        for extension in ["fst", "post", "meta", "norms"] {
            let raw =
                legacy_field_sidecar_path(&segment_dir, segment_id, field, extension).unwrap();
            fs::write(raw, b"corrupt-raw-stray").unwrap();
        }
        assert!(FtsIndexReader::open(&segment_dir, segment_id, &[field])
            .unwrap()
            .term_exists(field, "encodedneedle"));
        assert!(
            FtsIndexReader::open_stats_only(&segment_dir, segment_id, &[field])
                .unwrap()
                .term_exists(field, "encodedneedle")
        );

        // Full mode needs postings and norms; stats-only intentionally does not.
        let encoded_post = field_sidecar_path(&segment_dir, segment_id, field, "post");
        let encoded_post_bytes = fs::read(&encoded_post).unwrap();
        fs::remove_file(&encoded_post).unwrap();
        assert!(FtsIndexReader::open(&segment_dir, segment_id, &[field]).is_err());
        assert!(
            FtsIndexReader::open_stats_only(&segment_dir, segment_id, &[field])
                .unwrap()
                .term_exists(field, "encodedneedle")
        );
        fs::write(&encoded_post, encoded_post_bytes).unwrap();

        let encoded_meta = field_sidecar_path(&segment_dir, segment_id, field, "meta");
        let encoded_meta_bytes = fs::read(&encoded_meta).unwrap();
        fs::write(&encoded_meta, b"corrupt-selected-v2-meta").unwrap();
        assert!(FtsIndexReader::open(&segment_dir, segment_id, &[field]).is_err());
        assert!(FtsIndexReader::open_stats_only(&segment_dir, segment_id, &[field]).is_err());
        fs::write(&encoded_meta, encoded_meta_bytes).unwrap();

        fs::write(
            segment_filename_layout_v2_marker_path(&segment_dir, segment_id),
            b"corrupt-layout-marker",
        )
        .unwrap();
        assert!(FtsIndexReader::open(&segment_dir, segment_id, &[field]).is_err());
        assert!(FtsIndexReader::open_stats_only(&segment_dir, segment_id, &[field]).is_err());
    }

    #[test]
    fn writer_reports_when_filename_v2_preflight_is_required() {
        let dir = TempDir::new().unwrap();
        let mut writer = FtsIndexWriter::new(dir.path(), "seg", make_registry());
        writer.configure_field("title", FieldIndexConfig::default());
        assert!(!writer.uses_encoded_field_filename_components());
        writer.configure_field("bad/field", FieldIndexConfig::default());
        assert!(writer.uses_encoded_field_filename_components());
        assert!(writer.finish().is_err());
    }

    #[test]
    fn unsafe_field_components_are_bounded_portable_and_distinct() {
        let overlong = "a".repeat(MAX_LITERAL_FIELD_COMPONENT_BYTES + 1);
        let encoded_alias = format!(
            "{ENCODED_FIELD_COMPONENT_PREFIX}{:x}",
            Sha256::digest(b"bad/field")
        );
        let fields = vec![
            "bad/field",
            r"bad\field",
            "../traversal",
            ".",
            "..",
            "trailing ",
            "line\nfield",
            "nul\0field",
            "bad:windows*name?",
            "CON",
            "con.txt",
            "PRN",
            "aux.log",
            "NUL",
            "COM1",
            "com9.log",
            "LPT1",
            "lpt9.txt",
            ENCODED_FIELD_COMPONENT_PREFIX,
            encoded_alias.as_str(),
            overlong.as_str(),
        ];
        let components: Vec<String> = fields
            .iter()
            .map(|field| field_file_component(field).into_owned())
            .collect();

        assert_eq!(
            components.len(),
            components.iter().collect::<HashSet<_>>().len()
        );
        for (field, component) in fields.iter().zip(&components) {
            assert_eq!(component.as_str(), field_file_component(field).as_ref());
            assert!(component.starts_with(ENCODED_FIELD_COMPONENT_PREFIX));
            assert!(component.is_ascii());
            assert!(component
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'));
            assert!(component.len() < MAX_LITERAL_FIELD_COMPONENT_BYTES);
            assert!(!component.contains('/'));
            assert!(!component.contains('\\'));
        }
        assert_ne!(
            field_file_component("bad/field"),
            field_file_component(&encoded_alias),
            "an encoded component used as a literal field must enter the reserved namespace"
        );
        for field in ["COM0", "COM10", "LPT0", "CONSOLE"] {
            assert_eq!(field_file_component(field), Cow::Borrowed(field));
        }
    }

    #[test]
    fn unsafe_field_names_round_trip_without_creating_child_paths() {
        let root = TempDir::new().unwrap();
        let segment_dir = root.path().join("segments");
        let overlong = "long".repeat(80);
        let fields = [
            "bad/field",
            r"bad\field",
            "../../escape",
            "nul\0field",
            "bad:windows*name?",
            overlong.as_str(),
        ];
        for field in ["bad/field", "../../escape"] {
            assert!(legacy_field_sidecar_path(&segment_dir, "seg-unsafe", field, "fst").is_none());
        }
        let mut writer = FtsIndexWriter::new(&segment_dir, "seg-unsafe", make_registry());
        let document: HashMap<String, FieldValues> = fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                (
                    (*field).to_owned(),
                    FieldValues::One(format!("needle{index}")),
                )
            })
            .collect();
        writer.add_document(0, &document);
        writer.publish_encoded_filename_layout().unwrap();
        writer.finish().unwrap();

        let field_refs: Vec<&str> = fields.to_vec();
        let reader = FtsIndexReader::open(&segment_dir, "seg-unsafe", &field_refs).unwrap();
        for (index, field) in fields.iter().enumerate() {
            assert!(reader.term_exists(field, &format!("needle{index}")));
            for extension in ["fst", "post", "meta", "norms"] {
                let path = field_sidecar_path(&segment_dir, "seg-unsafe", field, extension);
                assert_eq!(path.parent(), Some(segment_dir.as_path()));
                assert!(path.is_file(), "missing {}", path.display());
            }
            assert!(!field_sidecar_path(&segment_dir, "seg-unsafe", field, "fst.tmp").exists());
        }

        let entries: Vec<_> = fs::read_dir(&segment_dir)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        assert_eq!(entries.len(), fields.len() * 4 + 1);
        assert!(entries
            .iter()
            .all(|entry| entry.file_type().unwrap().is_file()));
        assert!(!root.path().join("escape").exists());
    }
}
