//! FTS inverted index: writer and reader for one segment.
//!
//! ## On-disk layout
//!
//! For each indexed field `<field>` a segment produces two files:
//!
//! ```text
//! seg-<id>.<field>.fst       — FST term dictionary
//!                              value = byte offset into .post file
//! seg-<id>.<field>.post      — concatenated posting lists
//! seg-<id>.<field>.meta      — JSON: FieldStats + per-term TermPostings headers
//! seg-<id>.<field>.norms     — u16 per doc (field length, capped at 65535)
//! ```
//!
//! The FST key is the term text (UTF-8 bytes, lexicographically sorted by construction).
//! The FST output value is the byte offset in the `.post` file.
//! `TermPostings` metadata (doc_freq, ttf) is stored in the `.meta` JSON.

use crate::{
    analyzer::AnalyzerRegistry,
    bm25::FieldStats,
    postings::{PostingsWriter, TermPostings},
};
use anyhow::{Context, Result};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use fst::{Map, MapBuilder};
use memmap2::Mmap;
use serde::Deserialize;
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

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
const ZSTD_DURABLE_LEVEL: i32 = 3;
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
        zstd::bulk::compress(&records, ZSTD_DURABLE_LEVEL).with_context(|| "ZFM4 zstd compress")?;
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

// ── FtsIndexWriter ────────────────────────────────────────────────────────────

/// Builds the FTS inverted index for one segment.
///
/// Usage:
/// ```text
/// let mut writer = FtsIndexWriter::new(dir, segment_id, registry);
/// for doc in docs { writer.add_document(doc_id, &fields); }
/// writer.finish()?;
/// ```
pub struct FtsIndexWriter {
    segment_dir: PathBuf,
    segment_id: String,
    registry: Arc<AnalyzerRegistry>,
    /// Per-field: (config, postings_writer, field_stats, norms)
    fields: HashMap<String, FieldData>,
}

struct FieldData {
    config: FieldIndexConfig,
    postings: PostingsWriter,
    stats: FieldStats,
    /// (doc_id, field_length) in insertion order
    norms: Vec<(u32, u16)>,
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
        }
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

    /// Index all text fields of one document.
    ///
    /// `fields` is a map of field name → field text value.
    /// Fields not previously registered via `configure_field` are indexed
    /// with the default configuration (standard analyzer, positions on).
    pub fn add_document(&mut self, doc_id: u32, fields: &HashMap<String, String>) {
        for (field_name, text) in fields {
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

            let tokens = analyzer.analyze(text);
            let field_len = tokens.len() as u64;

            // Record norms (capped at u16::MAX)
            field_data
                .norms
                .push((doc_id, field_len.min(u16::MAX as u64) as u16));

            // Update field stats
            field_data.stats.total_docs += 1;
            field_data.stats.total_field_length += field_len;

            // Accumulate postings
            for token in &tokens {
                field_data
                    .postings
                    .add_occurrence(&token.text, doc_id, token.position);
            }
        }
    }

    /// V4 M4 — **parallel batch** add for flush time.
    ///
    /// Reshapes `(doc_id, field, text)` from row-major (per-doc) into
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
        docs: &[(String, HashMap<String, String>, S)],
    ) {
        use rayon::prelude::*;
        use std::collections::HashMap as StdHashMap;

        // Column-major reshape: field_name → Vec<(doc_ordinal, text)>.
        // Lookup-first so the common case (field already seen) skips the
        // per-doc-field `field_name.clone()` the `entry()` API forced.
        let mut per_field: StdHashMap<String, Vec<(u32, &str)>> = StdHashMap::new();
        for (ord, (_id, fields, _src)) in docs.iter().enumerate() {
            for (field_name, text) in fields {
                if let Some(v) = per_field.get_mut(field_name) {
                    v.push((ord as u32, text.as_str()));
                } else {
                    per_field
                        .entry(field_name.clone())
                        .or_default()
                        .push((ord as u32, text.as_str()));
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

        let per_field_vec: Vec<(String, Vec<(u32, &str)>)> = per_field.into_iter().collect();

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

                for (doc_ord, text) in entries {
                    let tokens = analyzer.analyze(text);
                    let field_len = tokens.len() as u64;
                    fd.norms
                        .push((doc_ord, field_len.min(u16::MAX as u64) as u16));
                    fd.stats.total_docs += 1;
                    fd.stats.total_field_length += field_len;
                    for token in &tokens {
                        fd.postings
                            .add_occurrence(&token.text, doc_ord, token.position);
                    }
                }
                (field_name, fd)
            })
            .collect();

        // Swap the built field data back in.
        for (name, fd) in built {
            self.fields.insert(name, fd);
        }
    }

    /// Flush all data to disk and return field stats for the segment manifest.
    ///
    /// Field writes run in parallel via Rayon — each thread owns one field's
    /// FST + postings + meta + norms build and writes its four side-car
    /// files independently.  On a 2-text-field nginx log this halves the
    /// flush stall; on a 10-text-field catalog index it's closer to 5×.
    pub fn finish(self) -> Result<HashMap<String, FieldStats>> {
        use rayon::prelude::*;

        fs::create_dir_all(&self.segment_dir)
            .with_context(|| format!("creating segment dir {:?}", self.segment_dir))?;

        let segment_dir = self.segment_dir.clone();
        let segment_id = self.segment_id.clone();

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
                Self::write_field_static(&segment_dir, &segment_id, &field_name, field_data)
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
    ) -> Result<()> {
        // NOTE: `PathBuf::with_extension` replaces the final `.ext` in the path,
        // so using `segment_dir.join("segment_id.field_name")` followed by
        // `with_extension("fst")` would strip `.field_name` and collapse every
        // field to the same file.  Build filenames by hand to avoid that.
        let filename =
            |ext: &str| segment_dir.join(format!("{}.{}.{}", segment_id, field_name, ext));
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
            let compressed = zstd::bulk::compress(&post_data, ZSTD_DURABLE_LEVEL)
                .with_context(|| "ZPS1 zstd compress")?;
            let mut out = Vec::with_capacity(4 + 4 + compressed.len());
            out.extend_from_slice(POST_MAGIC_ZSTD);
            out.write_u32::<LittleEndian>(uncompressed_len).unwrap();
            out.extend_from_slice(&compressed);
            out
        };
        // RC4 W1 #10 — every FTS side-car write below goes through the
        // durable tmp+fsync+rename+dir-fsync pattern.  These files are
        // part of the segment publish chain: the WAL entries they cover
        // are pruned ~1 s after the flush, so side-cars sitting only in
        // the volatile page cache meant a power loss could leave a
        // registered segment with missing/torn FTS data (silently wrong
        // query results or unreadable fields) with no WAL to recover from.
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
        // dir-fsync publishes it durably (RC4 W1 #10 — see the postings
        // note above).
        let fst_tmp = segment_dir.join(format!("{}.{}.fst.tmp", segment_id, field_name));
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
        let mut fst_out = fst_builder
            .into_inner()
            .with_context(|| "finishing FST")?;
        fst_out.flush().with_context(|| "flushing FST")?;
        fst_out
            .get_ref()
            .sync_all()
            .with_context(|| "fsyncing FST")?;
        drop(fst_out);
        fs::rename(&fst_tmp, &fst_path)
            .with_context(|| format!("publishing FST {:?}", fst_path))?;
        xerj_common::fsio::fsync_dir(segment_dir).with_context(|| "fsyncing segment dir (fst)")?;

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
        for (doc_id, norm) in &norms {
            dense[*doc_id as usize] = norm_u16_to_u8(*norm);
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
#[allow(dead_code)]
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
    /// doc_id → field_length lookup (sorted by doc_id).
    norms: Vec<(u32, u16)>,
}

impl FtsIndexReader {
    /// Load an existing segment's FTS data from disk.
    pub fn open(
        segment_dir: impl AsRef<Path>,
        segment_id: impl Into<String>,
        field_names: &[&str],
    ) -> Result<Self> {
        let segment_dir = segment_dir.as_ref().to_path_buf();
        let segment_id = segment_id.into();
        let mut fields = HashMap::new();

        for &field_name in field_names {
            // Build filenames explicitly — see note in `write_field_static`
            // for why `with_extension()` would be wrong here.
            let filename =
                |ext: &str| segment_dir.join(format!("{}.{}.{}", segment_id, field_name, ext));
            let fst_path = filename("fst");
            let post_path = filename("post");
            let meta_path = filename("meta");
            let norms_path = filename("norms");

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
            let raw_post = fs::read(&post_path)
                .with_context(|| format!("reading postings {:?}", post_path))?;
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

            let norms = Self::load_norms(&norms_path)?;

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

    fn load_norms(path: &Path) -> Result<Vec<(u32, u16)>> {
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
                    norms.push((doc_id as u32, norm_u8_to_u16(b)));
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
                norms.push((doc_id, norm));
            }
            Ok(norms)
        }
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
    pub fn postings_data<'a>(&'a self, field: &str, tp: &TermPostings) -> Option<&'a [u8]> {
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
    /// Returns `None` if the document has no data for this field.
    pub fn field_length(&self, field: &str, doc_id: u32) -> Option<u16> {
        let loaded = self.fields.get(field)?;
        // Binary search by doc_id
        loaded
            .norms
            .binary_search_by_key(&doc_id, |(id, _)| *id)
            .ok()
            .map(|idx| loaded.norms[idx].1)
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
    use tempfile::TempDir;

    fn make_registry() -> Arc<AnalyzerRegistry> {
        Arc::new(AnalyzerRegistry::default())
    }

    #[test]
    fn write_and_read_single_field() {
        let dir = TempDir::new().unwrap();
        let registry = make_registry();

        let mut writer = FtsIndexWriter::new(dir.path(), "seg0", registry);

        let docs: Vec<HashMap<String, String>> = vec![
            [("body".to_owned(), "the quick brown fox".to_owned())]
                .into_iter()
                .collect(),
            [("body".to_owned(), "the lazy dog".to_owned())]
                .into_iter()
                .collect(),
            [("body".to_owned(), "quick fox lazy dog".to_owned())]
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
            [("title".to_owned(), "hello world".to_owned())]
                .into_iter()
                .collect(),
            [("title".to_owned(), "hello rust".to_owned())]
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
}
