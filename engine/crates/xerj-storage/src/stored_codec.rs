//! Stored-section codec.
//!
//! Supports two on-disk formats, selected per-segment:
//!
//! | Magic    | Version | Description                                        |
//! |----------|---------|----------------------------------------------------|
//! | `LZ4\0`  | v1      | LZ4 over a flat JSON array `[{_id,_seq_no,_source}]`|
//! | `ZBS2`   | v2      | Columnar block — per-column codec, cross-col dep, dict+bitpack |
//!
//! The decoder detects the magic and returns a canonical JSON-array payload
//! for both, so callers upstream do not need to change.
//!
//! V2 format exists because v1 LZ4 over flat JSON leaves structural
//! redundancy on the table: every row repeats the schema keys, and numeric
//! columns determined by keyword columns (URL → status, URL → bytes on
//! nginx logs) can be collapsed to mode+exceptions.  `~/cz/src/v7_column_*`
//! demonstrates 397× on this corpus; V2 is the random-access-friendly
//! subset of those techniques — the ones that survive when you still need
//! to materialise an individual document by `doc_ord`.
//!
//! V2 payload layout:
//!
//! ```text
//! "ZBS2"                           4 bytes
//! u32  num_docs
//! u32  num_columns
//! per column (num_columns times):
//!     u16  name_len; name_len bytes of UTF-8 name
//!     u8   codec_id
//!     u32  payload_size
//!     payload_size bytes           // see `col_codec` module below
//! ```
//!
//! Column codecs (u8 id in the header):
//!
//! * 0 — `RAW_JSON`: fallback for complex shapes (objects, arrays).  Payload
//!   is zstd-compressed JSON array of the column values.  Also the codec
//!   used when row count < `V2_MIN_DOCS`.
//! * 1 — `LZ4_JSON`: same as RAW but LZ4 instead of zstd.  Picked when
//!   content is compressible but CPU budget is tight.
//! * 2 — `DICT_BITPACK`: dictionary-encoded column.  Payload =
//!   `u32 dict_count; varint[dict_count] dict_entry_lens; dict_entries; u32 zstd_len; zstd(bitpacked_ids)`.
//!   Rows with `null` use dict id `dict_count` (reserved).
//! * 3 — `CROSS_DEP`: numeric column determined by another dict-encoded
//!   column's dict ids, ≥ 90 % deterministic.  Payload =
//!   `u32 src_col_ix; u32 mode_table_len; i64[mode_table_len] mode_values;
//!   u32 exc_count; (varint delta doc_ord, zigzag_varint target_value)[exc_count]`.
//!   Any source dict id that was never present in the block is represented
//!   by `i64::MIN` in `mode_values` (sentinel).
//! * 4 — `CONSTANT`: a single repeated value.  Payload = zstd(json_of_value).
//!
//! Column names beginning with `__` are reserved by the format: `__id` and
//! `__seq_no` carry the top-level document fields, and `__nulls`
//! ([`V2_EXPLICIT_NULLS_COLUMN`]) carries per-document field *presence* — see
//! that constant for why a columnar block cannot derive it from the values.
//!
//! Decoder always returns the canonical v1 JSON-array shape so the rest of
//! the engine is oblivious to which codec was used.

use crate::{Result, StorageError};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::collections::HashMap;
use std::io::{Cursor, Read, Write};

mod row_hydration;
pub use row_hydration::{
    decode_stored_v2_rows, decode_stored_v2_rows_controlled, decode_stored_v2_rows_projected,
    decode_stored_v2_rows_projected_controlled, StoredV2HydratedRow, StoredV2RowHydrationResult,
    StoredV2RowHydrationStats,
};

pub const STORED_DECODE_CHECK_INTERVAL: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoredDecodePhase {
    BeforeColumn,
    Rows,
    AfterColumn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoredDecodeCheckpoint {
    pub phase: StoredDecodePhase,
    pub column_index: usize,
    pub rows_processed: usize,
}

#[derive(Debug, PartialEq)]
pub enum StoredDecodeRun<T> {
    Complete(T),
    Cancelled,
}

pub(crate) fn decode_cancelled<F>(
    checkpoint: &mut F,
    phase: StoredDecodePhase,
    column_index: usize,
    rows_processed: usize,
) -> bool
where
    F: FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()> + ?Sized,
{
    checkpoint(StoredDecodeCheckpoint {
        phase,
        column_index,
        rows_processed,
    })
    .is_break()
}

// ── V1: flat LZ4 over JSON (legacy) ───────────────────────────────────────

/// Magic prefix for LZ4-compressed stored sections (v1, pre-columnar).
pub const STORED_LZ4_MAGIC: &[u8; 4] = b"LZ4\0";

/// Compress a stored-section blob using LZ4 and prepend the magic header.
/// Kept for tests and for the slow-path fallback.
pub fn encode_stored_lz4(uncompressed: &[u8]) -> Vec<u8> {
    let compressed = lz4_flex::compress_prepend_size(uncompressed);
    let mut out = Vec::with_capacity(4 + compressed.len());
    out.extend_from_slice(STORED_LZ4_MAGIC);
    out.extend_from_slice(&compressed);
    out
}

// ── V2: columnar block with per-column codec ─────────────────────────────

/// Magic prefix for the V2 columnar format.
pub const STORED_V2_MAGIC: &[u8; 4] = b"ZBS2";

/// Minimum document count to bother with the columnar path.  Below this,
/// the per-column header overhead dominates and flat LZ4 wins.
const V2_MIN_DOCS: usize = 128;

/// Reserved column carrying, per document, the `_source` keys that were
/// stored as an explicit JSON `null`.
///
/// A column cell holds `NULL` both when the document omitted the field and
/// when it stored `null` there, so the value alone cannot say which — and the
/// decoder used to resolve that ambiguity by dropping every null, silently
/// rewriting `{"reason": null}` to `{}` on any segment of ≥ `V2_MIN_DOCS`
/// docs.  Neither reference implementation infers presence from the value:
/// Lucene's stored-fields reader replays exactly the field ids that were
/// written (`Lucene90CompressingStoredFieldsReader.document`,
/// lucene/core/src/java/org/apache/lucene/codecs/lucene90/compressing/Lucene90CompressingStoredFieldsReader.java:662),
/// and its sparse doc-values keep "which documents have a value" in a
/// dedicated structure beside the values (`IndexedDISI`,
/// lucene/core/src/java/org/apache/lucene/codecs/lucene90/IndexedDISI.java:96).
/// This column is that structure, in the shape this codec can carry.
///
/// Written only when at least one document in the block stores an explicit
/// null, so segments without any pay nothing and stay byte-identical to what
/// earlier XERJ releases produced.  Absent on every pre-fix segment, which
/// decodes exactly as before.
pub const V2_EXPLICIT_NULLS_COLUMN: &str = "__nulls";

/// Column names the format owns.  A `_source` field using one of these names
/// gets no column of its own — `__id` and `__seq_no` were already dropped this
/// way (they are hydrated from the top-level document), and
/// [`V2_EXPLICIT_NULLS_COLUMN`] joins them so the synthetic column's name is
/// never ambiguous.
pub(crate) fn is_reserved_column(name: &str) -> bool {
    matches!(name, "__id" | "__seq_no" | V2_EXPLICIT_NULLS_COLUMN)
}

/// The `_source` keys `source` stores as an explicit JSON `null`, in source
/// order.
///
/// `Value::Null` when there are none — the overwhelmingly common case, which
/// dict-encodes the whole column to one reserved id per row.
fn explicit_null_names(source: Option<&serde_json::Value>) -> serde_json::Value {
    let Some(obj) = source.and_then(|s| s.as_object()) else {
        return serde_json::Value::Null;
    };
    let names: Vec<serde_json::Value> = obj
        .iter()
        .filter(|(key, value)| value.is_null() && !is_reserved_column(key))
        .map(|(key, _)| serde_json::Value::String(key.clone()))
        .collect();
    if names.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::Array(names)
    }
}

/// Determinism threshold for `CROSS_DEP` — a numeric column is stored as
/// cross-dependent on a keyword column iff at least this fraction of rows
/// match the mode value for their source-key value.
const CROSS_DEP_MIN_DETERMINISM: f64 = 0.90;

/// Cardinality cap for `DICT_BITPACK`.  Above this, dict+bitpack loses to
/// zstd over LZ4-JSON.
const DICT_MAX_CARDINALITY: usize = 16_384;

/// Cardinality cap for CROSS_DEP candidate SOURCES.  A mode table with
/// thousands of entries (e.g. a `@timestamp` keyword column) is never a
/// good cross-dep source in practice, and probing one costs a full
/// O(rows) tally per (target, source) pair — flush-finalize profiling
/// showed this scan dominating the stored-encode time.  Skipping
/// high-cardinality sources only changes which codec a column picks
/// (all codecs decode identically); it never affects correctness.
const CROSS_DEP_MAX_SRC_CARDINALITY: usize = 256;

/// Zstd level used for every per-column / dict / cross-dep / fallback
/// payload in V2.
///
/// History: 3 → 9 → 19 → 3.  The level-19 pass (commit 73c6367) was
/// reverted because it ran on the **flush** path, not just merge.
/// Per-shard flush throughput is the back-pressure-critical bound on
/// sustained ingest.  Level-19 zstd at ~25 MB/s/core stacks behind two
/// other level-19 encoders (.dv per-column and .post / .meta in
/// xerj-fts), so a ~50 MB segment flush took 5-10 s instead of
/// ~250 ms — the memtable hit its 3× cap, back-pressure triggered, and
/// 32 parallel ingest workers blew through the retry budget.  Result:
/// 75 % rejected docs at 21 K docs/s sustained vs the previous 1.55 M
/// docs/s peak.
///
/// Level 3 puts ingest back at ~250 MB/s/core encode, restores the
/// 1 M+ docs/s ingest path, and grows steady-state segments by ~5 %
/// (merges are infrequent and dominated by long-term-stored data, so
/// the on-flush overhead is only paid by the freshest tier-0 segments
/// before they merge anyway).  See
/// `engine/reports/2026-04-25T21-50-00_ingest_perf_regression_zstd19.md`.
///
/// This is now the level used **on flush**, which the operator cannot raise —
/// that is the whole content of the paragraph above.  Merge re-encodes at
/// whatever `compression.level` resolves to (#318); see
/// [`encode_stored_v2_at_level`].
pub const STORED_ZSTD_LEVEL: i32 = 3;

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq)]
enum ColCodec {
    RawJson = 0,
    Lz4Json = 1,
    DictBitpack = 2,
    CrossDep = 3,
    Constant = 4,
}

impl ColCodec {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::RawJson),
            1 => Some(Self::Lz4Json),
            2 => Some(Self::DictBitpack),
            3 => Some(Self::CrossDep),
            4 => Some(Self::Constant),
            _ => None,
        }
    }
}

/// Encode a stored section written as a JSON array of
/// `{"_id", "_seq_no", "_source"}` objects using the v2 columnar format.
///
/// On tiny segments (< `V2_MIN_DOCS` docs) or on any shape that doesn't
/// fit the column assumptions, the function silently falls back to the
/// v1 LZ4 encoder — the caller never has to care which was used.
///
/// Encodes at [`STORED_ZSTD_LEVEL`]; merge calls
/// [`encode_stored_v2_at_level`] instead to honour `compression.level`.
pub fn encode_stored_v2(stored_docs_json: &[u8]) -> Vec<u8> {
    encode_stored_v2_at_level(stored_docs_json, STORED_ZSTD_LEVEL)
}

/// [`encode_stored_v2`] at a caller-chosen zstd effort level.
///
/// Only the merge path passes anything but [`STORED_ZSTD_LEVEL`] — flush is
/// back-pressure-critical and stays pinned (see that constant). The output is
/// the same ZBS2 format at any level: zstd decode does not need to know the
/// level a payload was written at, so segments written before and after an
/// operator changes `compression.level` interleave with no migration and no
/// format flag.
pub fn encode_stored_v2_at_level(stored_docs_json: &[u8], level: i32) -> Vec<u8> {
    // Parse the JSON array of documents.  Failure → v1 fallback.
    let docs: Vec<serde_json::Value> = match serde_json::from_slice(stored_docs_json) {
        Ok(v) => v,
        Err(_) => return encode_stored_lz4(stored_docs_json),
    };
    if docs.len() < V2_MIN_DOCS {
        return encode_stored_lz4(stored_docs_json);
    }

    // Flatten each doc into (id, seq_no, source-fields).  Anything non-JSON
    // at the top level degrades to the RAW column codec.  Keys are
    // collected from _source; the top-level _id and _seq_no become
    // synthetic columns `__id` and `__seq_no` (names starting with `__`
    // are reserved).
    let num_docs = docs.len();

    // First pass: discover the union of _source field names preserving
    // insertion order, plus dtype hints.
    let mut col_order: Vec<String> = vec!["__id".into(), "__seq_no".into()];
    let mut col_seen: HashMap<String, usize> = HashMap::new();
    col_seen.insert("__id".into(), 0);
    col_seen.insert("__seq_no".into(), 1);

    for doc in &docs {
        if let Some(src) = doc.get("_source").and_then(|s| s.as_object()) {
            for key in src.keys() {
                if is_reserved_column(key) {
                    continue;
                }
                if !col_seen.contains_key(key) {
                    col_seen.insert(key.clone(), col_order.len());
                    col_order.push(key.clone());
                }
            }
        }
    }

    // Second pass: materialise each column as Vec<&serde_json::Value>
    // (borrowed out of the parsed docs — no per-field clones).
    // `__id` and `__seq_no` are hydrated from the top-level doc object.
    static NULL: serde_json::Value = serde_json::Value::Null;
    let mut columns: Vec<Vec<&serde_json::Value>> =
        vec![Vec::with_capacity(num_docs); col_order.len()];

    for doc in &docs {
        // __id and __seq_no
        columns[0].push(doc.get("_id").unwrap_or(&NULL));
        columns[1].push(doc.get("_seq_no").unwrap_or(&NULL));

        let src_obj = doc.get("_source").and_then(|s| s.as_object());
        for (cix, cname) in col_order.iter().enumerate().skip(2) {
            let val = src_obj.and_then(|m| m.get(cname)).unwrap_or(&NULL);
            columns[cix].push(val);
        }
    }

    let explicit_nulls: Vec<serde_json::Value> = docs
        .iter()
        .map(|doc| explicit_null_names(doc.get("_source")))
        .collect();
    push_explicit_nulls_column(&mut col_order, &mut columns, &explicit_nulls);

    encode_v2_columns(
        num_docs,
        &col_order,
        &columns,
        Some(stored_docs_json),
        level,
    )
}

/// Append [`V2_EXPLICIT_NULLS_COLUMN`] iff any document in the block stores an
/// explicit null.  Appended last so the column order of every block without one
/// is exactly what it was before this column existed.
fn push_explicit_nulls_column<'a>(
    col_order: &mut Vec<String>,
    columns: &mut Vec<Vec<&'a serde_json::Value>>,
    explicit_nulls: &'a [serde_json::Value],
) {
    if explicit_nulls.iter().all(serde_json::Value::is_null) {
        return;
    }
    col_order.push(V2_EXPLICIT_NULLS_COLUMN.to_string());
    columns.push(explicit_nulls.iter().collect());
}

/// P2.2 — columnar V2 encoder fed directly from already-parsed values.
///
/// `encode_stored_v2` re-parses the (potentially multi-MB) JSON array the
/// flush path just serialised and clones every field into columns — ~10s
/// of background-flush CPU per 1M docs.  When the flush already holds the
/// parsed `_source` trees (the HTTP `_bulk` turbo path), this entry point
/// builds the columns straight from them.
///
/// Contract: `stored_docs_json` MUST be the canonical serialisation of
/// `docs` (`[{"_id":…,"_seq_no":…,"_source":…}, …]`, as built by
/// `IndexStore::finalize_flush_with_publisher`).  It is used for the v1
/// (LZ4) fallback on tiny segments and the final "never make things
/// worse" size comparison, so the output is byte-identical to
/// `encode_stored_v2(stored_docs_json)` — asserted by unit tests here and
/// by the `XERJ_FLUSH_PARITY=1` runtime gate in the flush path.
pub fn encode_stored_v2_from_values(
    stored_docs_json: &[u8],
    docs: &[(&str, u64, &serde_json::Value)],
) -> Vec<u8> {
    encode_stored_v2_from_values_inner(Some(stored_docs_json), docs)
}

/// Flush fast path: encode straight from parsed Values with NO canonical
/// JSON-array serialisation at all.  Skips the v1-LZ4 "never make things
/// worse" size net (the columnar form wins on every real segment above a
/// few thousand docs; serialising a ~10 MB JSON array per flush purely to
/// run that comparison was ~3 s of background CPU per 1M ingested docs).
/// On sub-`V2_MIN_DOCS` inputs the canonical JSON array is built here so
/// the v1 fallback output stays byte-identical to the legacy path.
pub fn encode_stored_v2_from_values_nojson(docs: &[(&str, u64, &serde_json::Value)]) -> Vec<u8> {
    encode_stored_v2_from_values_inner(None, docs)
}

/// Serialise the canonical `[{"_id":…,"_seq_no":…,"_source":…}, …]` array
/// exactly as `IndexStore::finalize_flush_with_publisher` builds it.
fn serialize_canonical_stored_json(docs: &[(&str, u64, &serde_json::Value)]) -> Vec<u8> {
    let mut stored_bytes: Vec<u8> = Vec::with_capacity(docs.len() * 512);
    stored_bytes.push(b'[');
    let mut first = true;
    for (id, seq_no, src) in docs {
        if !first {
            stored_bytes.push(b',');
        }
        first = false;
        stored_bytes.extend_from_slice(br#"{"_id":"#);
        let _ = serde_json::to_writer(&mut stored_bytes, id);
        stored_bytes.extend_from_slice(br#","_seq_no":"#);
        let _ = write!(stored_bytes, "{}", seq_no);
        stored_bytes.extend_from_slice(br#","_source":"#);
        let _ = serde_json::to_writer(&mut stored_bytes, src);
        stored_bytes.push(b'}');
    }
    stored_bytes.push(b']');
    stored_bytes
}

fn encode_stored_v2_from_values_inner(
    stored_docs_json: Option<&[u8]>,
    docs: &[(&str, u64, &serde_json::Value)],
) -> Vec<u8> {
    if docs.len() < V2_MIN_DOCS {
        return match stored_docs_json {
            Some(json) => encode_stored_lz4(json),
            None => encode_stored_lz4(&serialize_canonical_stored_json(docs)),
        };
    }
    let num_docs = docs.len();
    let prof = std::env::var_os("XERJ_PROF").is_some();
    let t_build = std::time::Instant::now();

    // Synthetic __id / __seq_no columns need owned Values to borrow from.
    let ids: Vec<serde_json::Value> = docs
        .iter()
        .map(|(id, _, _)| serde_json::Value::String((*id).to_string()))
        .collect();
    let seqs: Vec<serde_json::Value> = docs
        .iter()
        .map(|(_, seq, _)| serde_json::Value::from(*seq))
        .collect();

    let mut col_order: Vec<String> = vec!["__id".into(), "__seq_no".into()];
    let mut col_seen: rustc_hash::FxHashMap<&str, usize> = rustc_hash::FxHashMap::default();
    col_seen.insert("__id", 0);
    col_seen.insert("__seq_no", 1);

    static NULL: serde_json::Value = serde_json::Value::Null;

    // Single pass over the docs: discover columns on first sight
    // (backfilling NULLs for rows already consumed) and route each
    // field value straight to its column.  The previous two-pass build
    // (discover, then `obj.get(cname)` per column per doc) performed
    // ~2× the map lookups — measured at ~115 ms per 31k-doc flush.
    let mut columns: Vec<Vec<&serde_json::Value>> =
        vec![Vec::with_capacity(num_docs), Vec::with_capacity(num_docs)];
    for (i, (_, _, src)) in docs.iter().enumerate() {
        columns[0].push(&ids[i]);
        columns[1].push(&seqs[i]);
        if let Some(obj) = src.as_object() {
            for (key, val) in obj {
                // A source field named like a reserved column must NOT route
                // into the synthetic columns (the legacy fill loop skipped
                // `__id` / `__seq_no` too).
                if is_reserved_column(key) {
                    continue;
                }
                let cix = match col_seen.get(key.as_str()) {
                    Some(&cix) if cix < 2 => continue,
                    Some(&cix) => cix,
                    None => {
                        let cix = col_order.len();
                        // `col_seen` borrows the key from the doc's
                        // source object (outlives this fn's locals);
                        // `col_order` owns an independent clone.
                        col_seen.insert(key.as_str(), cix);
                        col_order.push(key.clone());
                        let mut fresh: Vec<&serde_json::Value> = Vec::with_capacity(num_docs);
                        fresh.resize(i, &NULL); // rows before this doc lack the field
                        columns.push(fresh);
                        cix
                    }
                };
                let col = &mut columns[cix];
                // A duplicate key inside one object is impossible
                // (serde Map), so len == i here means every earlier pad
                // is in place — pad any gap, then push this doc's value.
                col.resize(i, &NULL);
                col.push(val);
            }
        }
        // Columns absent from this doc get padded lazily by the resize
        // above on their next occurrence, and by the final pass below.
    }
    for col in columns.iter_mut() {
        col.resize(num_docs, &NULL);
    }

    let explicit_nulls: Vec<serde_json::Value> = docs
        .iter()
        .map(|(_, _, src)| explicit_null_names(Some(src)))
        .collect();
    push_explicit_nulls_column(&mut col_order, &mut columns, &explicit_nulls);

    if prof {
        eprintln!(
            "XERJ_PROF encode-build docs={} build_us={}",
            num_docs,
            t_build.elapsed().as_micros()
        );
    }

    // Flush-only entry point — pinned to the flush level, never the
    // operator's (#318): raising the level here is the ingest regression
    // documented on `STORED_ZSTD_LEVEL`.
    encode_v2_columns(
        num_docs,
        &col_order,
        &columns,
        stored_docs_json,
        STORED_ZSTD_LEVEL,
    )
}

/// Shared tail of the two V2 entry points: per-column codec selection +
/// payload assembly + the v1-LZ4 "never make things worse" size net.
///
/// `stored_docs_json` is the canonical JSON-array serialisation used for
/// the LZ4 size net; pass `None` to skip the net (used by the flush
/// fast path on large segments where the columnar form always wins and
/// serialising a multi-MB JSON array per flush is measurable CPU).
fn encode_v2_columns(
    num_docs: usize,
    col_order: &[String],
    columns: &[Vec<&serde_json::Value>],
    stored_docs_json: Option<&[u8]>,
    level: i32,
) -> Vec<u8> {
    // THROWAWAY prof (XERJ_PROF): encode-phase breakdown.
    let prof = std::env::var_os("XERJ_PROF").is_some();
    let t_dict = std::time::Instant::now();

    // Build dict-encoded form for each column as a stable side representation.
    // `dict_encode(col)` returns Some((dict_entries, ids)) where ids[i] is the
    // 1-based id (0 reserved for null, we add 1 here).  Returns None when
    // the column has non-scalar values or unique-count exceeds DICT_MAX_CARDINALITY.
    let dict_encoded: Vec<Option<(Vec<serde_json::Value>, Vec<u32>)>> =
        columns.iter().map(|c| dict_encode_column(c)).collect();
    let dict_us = t_dict.elapsed().as_micros();
    let t_cross = std::time::Instant::now();

    // For each numeric column, try to find a dict-encoded keyword-like
    // source column that determines it.  First match wins.
    let cross_dep_src: Vec<Option<usize>> = columns
        .iter()
        .enumerate()
        .map(|(cix, col)| {
            // Only integer columns may use the lossy i64 cross-dep path;
            // float columns fall through to the lossless dict / raw path so
            // fractional values (e.g. `0.010127`) survive intact.
            if !col_is_all_integer(col) {
                return None;
            }
            // Provable upper bound: with a source of ≤S dict entries, the
            // mode table yields at most one hit-value per source id, so
            // misses ≥ distinct(target) − S and det ≤ 1 − (D − S)/N.
            // If D > N/10 + S the 0.90 determinism gate CANNOT pass for
            // any admissible source — skip the whole O(sources × rows)
            // probe.  This is what stops per-row-unique columns like
            // `__seq_no` from paying a full cross-dep scan every flush.
            let target_distinct = dict_encoded[cix]
                .as_ref()
                .map(|(e, _)| e.len())
                .unwrap_or(DICT_MAX_CARDINALITY + 1);
            if target_distinct > num_docs / 10 + CROSS_DEP_MAX_SRC_CARDINALITY {
                return None;
            }
            best_cross_dep_source(&dict_encoded, cix, col)
        })
        .collect();

    // CYCLE BREAKER (2026-07, S1 root cause): `best_cross_dep_source`
    // admits ANY dict-encodable column as a source — including integer
    // columns that are themselves about to be CrossDep-encoded.  Two
    // mutually-deterministic low-cardinality integer columns (e.g.
    // `a = i%5`, `b = (i%5)*7`) each picked the other as source, writing
    // a dependency CYCLE to disk that `decode_stored_v2` can never
    // resolve ("cross_dep src not resolved") — the whole stored section
    // became undecodable and the merge loss-amplifier destroyed every
    // doc in the segment.  Fix: any column chosen as a CrossDep SOURCE
    // must materialise in decode pass 1, so clear its OWN cross_dep_src
    // (it falls back to DictBitpack — it is dict-encodable and
    // ≤ CROSS_DEP_MAX_SRC_CARDINALITY ≤ DICT_MAX_CARDINALITY by
    // admission, so the compression give-up is negligible).  This
    // structurally rules out cycles AND multi-hop forward chains: every
    // surviving CrossDep target's source is a pass-1 codec.
    let cross_dep_src: Vec<Option<usize>> = {
        let chosen_sources: rustc_hash::FxHashSet<usize> =
            cross_dep_src.iter().flatten().copied().collect();
        cross_dep_src
            .iter()
            .enumerate()
            .map(|(cix, src)| {
                if chosen_sources.contains(&cix) {
                    None
                } else {
                    *src
                }
            })
            .collect()
    };
    let cross_us = t_cross.elapsed().as_micros();
    let t_cols = std::time::Instant::now();

    // Pick a codec per column and build its payload.
    let mut col_payloads: Vec<(String, u8, Vec<u8>)> = Vec::with_capacity(col_order.len());
    for (cix, cname) in col_order.iter().enumerate() {
        let col = &columns[cix];

        // CONSTANT → CROSS_DEP → DICT_BITPACK → LZ4_JSON → RAW_JSON fallback.
        if let Some(payload) = try_encode_constant(col) {
            col_payloads.push((cname.clone(), ColCodec::Constant as u8, payload));
            continue;
        }
        if let Some(src_ix) = cross_dep_src[cix] {
            let (payload, ok) = encode_cross_dep(col, src_ix, &dict_encoded, level);
            if ok {
                col_payloads.push((cname.clone(), ColCodec::CrossDep as u8, payload));
                continue;
            }
        }
        if let Some((entries, ids)) = &dict_encoded[cix] {
            if entries.len() <= DICT_MAX_CARDINALITY && all_scalar_dict_entries(entries) {
                let payload = encode_dict_bitpack(entries, ids, level);
                col_payloads.push((cname.clone(), ColCodec::DictBitpack as u8, payload));
                continue;
            }
        }
        // Fallback: zstd over JSON-array of the column's values.
        let col_json = serde_json::to_vec(col).unwrap_or_default();
        let zstd_payload =
            zstd::encode_all(Cursor::new(&col_json), level).unwrap_or_else(|_| col_json.clone());
        // Choose RAW_JSON vs LZ4_JSON by size.
        let lz4_payload = lz4_flex::compress_prepend_size(&col_json);
        if lz4_payload.len() + 1 < zstd_payload.len() {
            col_payloads.push((cname.clone(), ColCodec::Lz4Json as u8, lz4_payload));
        } else {
            col_payloads.push((cname.clone(), ColCodec::RawJson as u8, zstd_payload));
        }
    }

    // Assemble the V2 payload.
    let mut out: Vec<u8> = Vec::with_capacity(
        4 + 8
            + col_payloads
                .iter()
                .map(|(n, _, p)| 2 + n.len() + 5 + p.len())
                .sum::<usize>(),
    );
    out.extend_from_slice(STORED_V2_MAGIC);
    out.write_u32::<LittleEndian>(num_docs as u32).unwrap();
    out.write_u32::<LittleEndian>(col_payloads.len() as u32)
        .unwrap();
    for (name, codec_id, payload) in &col_payloads {
        out.write_u16::<LittleEndian>(name.len() as u16).unwrap();
        out.extend_from_slice(name.as_bytes());
        out.push(*codec_id);
        out.write_u32::<LittleEndian>(payload.len() as u32).unwrap();
        out.extend_from_slice(payload);
    }

    if prof {
        eprintln!(
            "XERJ_PROF encode-cols docs={} cols={} dict_us={} cross_us={} colpay_us={}",
            num_docs,
            col_order.len(),
            dict_us,
            cross_us,
            t_cols.elapsed().as_micros()
        );
    }

    // If v1 LZ4 would have been smaller, use it instead.  This is the
    // "never make things worse" safety net that mirrors cz's per-column
    // best-of-codec picker.
    if let Some(json) = stored_docs_json {
        let v1 = encode_stored_lz4(json);
        if v1.len() < out.len() {
            return v1;
        }
    }
    out
}

/// Decode a stored section written by any supported codec version.
///
/// Returns the canonical JSON-array payload that every upstream caller
/// expects (`[{"_id", "_seq_no", "_source": {...}}, ...]`).
pub fn decode_stored(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() >= 4 && &bytes[..4] == STORED_V2_MAGIC {
        return decode_stored_v2(&bytes[4..]);
    }
    if bytes.len() >= 4 && &bytes[..4] == STORED_LZ4_MAGIC {
        return lz4_flex::decompress_size_prepended(&bytes[4..])
            .map_err(|e| StorageError::Other(anyhow::anyhow!("LZ4 decompress failed: {e}")));
    }
    Ok(bytes.to_vec())
}

/// Conservative retained-size bound for the canonical stored bytes plus the
/// `(start, end)` row-offset table used by the engine's `StoredSlices`.
///
/// This reads only framing metadata. It never decompresses a column or
/// materialises a `serde_json::Value`, so publish-time cache admission can
/// fail closed *before* paying the full ZBS2 expansion. `Ok(None)` means a
/// zstd frame omitted its content-size field and therefore has no trustworthy
/// finite bound in the current format.
pub fn stored_slices_retained_upper_bound(bytes: &[u8], expected_docs: u64) -> Result<Option<u64>> {
    const STORED_SLICES_STRUCT_OVERHEAD: u64 = 64;
    const OFFSET_BYTES: u64 = 8;

    let expected_offsets = expected_docs
        .checked_mul(OFFSET_BYTES)
        .and_then(|size| size.checked_add(STORED_SLICES_STRUCT_OVERHEAD))
        .ok_or_else(|| StorageError::Other(anyhow::anyhow!("stored retained bound overflow")))?;

    let (decoded_bound, offsets_bound) = if bytes.len() >= 4 && &bytes[..4] == STORED_LZ4_MAGIC {
        let payload = bytes
            .get(4..8)
            .ok_or_else(|| StorageError::Other(anyhow::anyhow!("LZ4 stored size truncated")))?;
        let decoded = u32::from_le_bytes(payload.try_into().unwrap()) as u64;
        let offsets = decoded
            .checked_mul(OFFSET_BYTES)
            .and_then(|size| size.checked_add(STORED_SLICES_STRUCT_OVERHEAD))
            .ok_or_else(|| {
                StorageError::Other(anyhow::anyhow!("LZ4 stored offset bound overflow"))
            })?;
        (decoded, offsets)
    } else if bytes.len() >= 4 && &bytes[..4] == STORED_V2_MAGIC {
        let directory = parse_v2_directory(&bytes[4..])?;
        if directory.num_docs as u64 != expected_docs {
            return Err(StorageError::Other(anyhow::anyhow!(
                "v2 num_docs {} != expected {expected_docs}",
                directory.num_docs
            )));
        }
        let rows = expected_docs;
        // Canonical wrapper punctuation, `_id`, `_seq_no`, `_source`, commas,
        // colons and braces fit well below 64 bytes per row. This deliberately
        // over-counts empty/null fields.
        let mut bound = rows
            .checked_mul(64)
            .and_then(|size| size.checked_add(2))
            .ok_or_else(|| {
                StorageError::Other(anyhow::anyhow!("v2 decoded-size bound overflow"))
            })?;
        for column in &directory.columns {
            let value_bound = match column.codec {
                ColCodec::RawJson => {
                    let Some(size) = zstd_frame_content_size(column.payload, "raw json")? else {
                        return Ok(None);
                    };
                    size
                }
                ColCodec::Lz4Json => lz4_uncompressed_size(column.payload, "lz4 json")?,
                ColCodec::Constant => {
                    (column.payload.len() as u64)
                        .checked_mul(rows)
                        .ok_or_else(|| {
                            StorageError::Other(anyhow::anyhow!(
                                "constant decoded-size bound overflow"
                            ))
                        })?
                }
                ColCodec::DictBitpack => {
                    let Some(dictionary_bytes) =
                        dict_json_content_size(column.payload, directory.num_docs)?
                    else {
                        return Ok(None);
                    };
                    // Every row is either null (4 bytes) or one dictionary
                    // entry. No entry can be longer than the complete
                    // dictionary JSON payload.
                    dictionary_bytes.max(4).checked_mul(rows).ok_or_else(|| {
                        StorageError::Other(anyhow::anyhow!("dict decoded-size bound overflow"))
                    })?
                }
                // CROSS_DEP materialises only JSON i64 or null cells. The
                // longest i64 decimal representation is 20 bytes.
                ColCodec::CrossDep => rows.checked_mul(20).ok_or_else(|| {
                    StorageError::Other(anyhow::anyhow!("cross-dep decoded-size bound overflow"))
                })?,
            };
            bound = bound.checked_add(value_bound).ok_or_else(|| {
                StorageError::Other(anyhow::anyhow!("v2 decoded-size bound overflow"))
            })?;
            if column.name != "__id" && column.name != "__seq_no" {
                let encoded_name_len = serde_json::to_vec(column.name)
                    .map_err(|error| {
                        StorageError::Other(anyhow::anyhow!(
                            "v2 column-name size estimate: {error}"
                        ))
                    })?
                    .len() as u64;
                let key_overhead = rows
                    .checked_mul(encoded_name_len.saturating_add(2))
                    .ok_or_else(|| {
                        StorageError::Other(anyhow::anyhow!("v2 key-size bound overflow"))
                    })?;
                bound = bound.checked_add(key_overhead).ok_or_else(|| {
                    StorageError::Other(anyhow::anyhow!("v2 decoded-size bound overflow"))
                })?;
            }
        }
        (bound, expected_offsets)
    } else {
        let decoded = bytes.len() as u64;
        let offsets = decoded
            .checked_mul(OFFSET_BYTES)
            .and_then(|size| size.checked_add(STORED_SLICES_STRUCT_OVERHEAD))
            .ok_or_else(|| {
                StorageError::Other(anyhow::anyhow!("plain stored offset bound overflow"))
            })?;
        (decoded, offsets)
    };
    decoded_bound
        .checked_add(offsets_bound)
        .map(Some)
        .ok_or_else(|| StorageError::Other(anyhow::anyhow!("stored retained bound overflow")))
}

fn zstd_frame_content_size(payload: &[u8], context: &str) -> Result<Option<u64>> {
    let frame_size = zstd::zstd_safe::find_frame_compressed_size(payload).map_err(|_| {
        StorageError::Other(anyhow::anyhow!("{context} zstd frame header is invalid"))
    })?;
    if frame_size != payload.len() {
        return Err(StorageError::Other(anyhow::anyhow!(
            "{context} contains concatenated/trailing zstd data"
        )));
    }
    zstd::zstd_safe::get_frame_content_size(payload)
        .map_err(|_| StorageError::Other(anyhow::anyhow!("{context} zstd frame header is invalid")))
}

fn decode_zstd_exact(payload: &[u8], expected: usize, context: &str) -> Result<Vec<u8>> {
    if zstd_frame_content_size(payload, context)?.is_some_and(|size| size != expected as u64) {
        return Err(StorageError::Other(anyhow::anyhow!(
            "{context} content size does not match expected length"
        )));
    }
    let decoder = zstd::stream::read::Decoder::new(payload)
        .map_err(|error| StorageError::Other(anyhow::anyhow!("{context}: {error}")))?;
    let mut bounded = decoder.take(expected as u64 + 1);
    let mut decoded = Vec::with_capacity(expected);
    bounded
        .read_to_end(&mut decoded)
        .map_err(|error| StorageError::Other(anyhow::anyhow!("{context}: {error}")))?;
    if decoded.len() != expected {
        return Err(StorageError::Other(anyhow::anyhow!(
            "{context} decoded length {} != expected {expected}",
            decoded.len()
        )));
    }
    Ok(decoded)
}

fn lz4_uncompressed_size(payload: &[u8], context: &str) -> Result<u64> {
    let size = payload
        .get(..4)
        .ok_or_else(|| StorageError::Other(anyhow::anyhow!("{context} size is truncated")))?;
    Ok(u32::from_le_bytes(size.try_into().unwrap()) as u64)
}

fn dict_json_content_size(payload: &[u8], num_docs: usize) -> Result<Option<u64>> {
    let mut cursor = Cursor::new(payload);
    let _dict_count = cursor
        .read_u32::<LittleEndian>()
        .map_err(|error| StorageError::Other(anyhow::anyhow!("dict_count: {error}")))?;
    let bit_width = cursor
        .read_u8()
        .map_err(|error| StorageError::Other(anyhow::anyhow!("bit_width: {error}")))?;
    if !(1..=32).contains(&bit_width) {
        return Err(StorageError::Other(anyhow::anyhow!(
            "dict invalid bit_width {bit_width}"
        )));
    }
    let dict_len = cursor
        .read_u32::<LittleEndian>()
        .map_err(|error| StorageError::Other(anyhow::anyhow!("dict_zstd_len: {error}")))?
        as usize;
    let dict_start = cursor.position() as usize;
    let dict_end = dict_start
        .checked_add(dict_len)
        .filter(|end| *end <= payload.len())
        .ok_or_else(|| StorageError::Other(anyhow::anyhow!("dict payload truncated")))?;
    let content_size = zstd_frame_content_size(&payload[dict_start..dict_end], "dict json")?;
    cursor.set_position(dict_end as u64);
    let ids_len = cursor
        .read_u32::<LittleEndian>()
        .map_err(|error| StorageError::Other(anyhow::anyhow!("ids_len: {error}")))?
        as usize;
    if ids_len != num_docs {
        return Err(StorageError::Other(anyhow::anyhow!(
            "dict ids_len {ids_len} != num_docs {num_docs}"
        )));
    }
    let packed_len = cursor
        .read_u32::<LittleEndian>()
        .map_err(|error| StorageError::Other(anyhow::anyhow!("packed_zstd_len: {error}")))?
        as usize;
    let packed_start = cursor.position() as usize;
    let packed_end = packed_start
        .checked_add(packed_len)
        .filter(|end| *end == payload.len())
        .ok_or_else(|| {
            StorageError::Other(anyhow::anyhow!("dict packed payload truncated/trailing"))
        })?;
    let packed_size = zstd_frame_content_size(&payload[packed_start..packed_end], "dict ids")?;
    let expected_packed_size = num_docs
        .checked_mul(bit_width as usize)
        .ok_or_else(|| StorageError::Other(anyhow::anyhow!("dict packed-size overflow")))?
        .div_ceil(8) as u64;
    if packed_size.is_some_and(|size| size != expected_packed_size) {
        return Err(StorageError::Other(anyhow::anyhow!(
            "dict packed content size does not match ids_len"
        )));
    }
    if packed_size.is_none() {
        return Ok(None);
    }
    Ok(content_size)
}

/// Values decoded from a requested subset of a ZBS2 stored section.
///
/// Column names use their on-disk names: `_id` and `_seq_no` are `__id` and
/// `__seq_no`; source fields retain their original top-level names.
#[derive(Debug, PartialEq)]
pub struct StoredV2Projection {
    pub num_docs: usize,
    pub columns: HashMap<String, Vec<serde_json::Value>>,
}

/// Result of attempting a selective V2 decode.
#[derive(Debug, PartialEq)]
pub enum StoredV2ProjectionResult {
    /// The section is a valid legacy/non-V2 shape. Callers may use the full
    /// compatibility decoder.
    NotV2,
    /// The requested columns were decoded. Requested names absent from the
    /// segment are absent from `columns`.
    Projected(StoredV2Projection),
}

struct V2ColumnRef<'a> {
    name: &'a str,
    codec: ColCodec,
    payload: &'a [u8],
}

struct V2Directory<'a> {
    num_docs: usize,
    columns: Vec<V2ColumnRef<'a>>,
}

/// Typed columns needed by an exact kNN scan.
pub type StoredVectorRows = Vec<Option<Vec<f32>>>;
pub type StoredVectorChunkRows = Vec<Option<Vec<Vec<f32>>>>;

#[derive(Debug, PartialEq)]
pub struct StoredV2KnnProjection {
    pub num_docs: usize,
    /// Invalid or missing identity cells are represented as `None`; the
    /// engine's version/tombstone rules decide whether to skip those rows.
    pub ids: Vec<Option<String>>,
    pub seq_nos: Vec<Option<u64>>,
    pub vectors: StoredVectorRows,
    /// `None` means the optional chunks role was not requested or its named
    /// column is absent. It is not a corruption signal.
    pub vector_chunks: Option<StoredVectorChunkRows>,
}

/// Result of attempting the typed kNN projection.
#[derive(Debug, PartialEq)]
pub enum StoredV2KnnProjectionResult {
    /// Legacy LZ4 or headerless JSON. The caller must use the compatibility
    /// decoder.
    NotV2,
    /// A valid V2 section does not contain the requested vector column.
    MissingVectorColumn,
    /// The requested column exists but is not a finite numeric vector shape.
    /// Callers may preserve compatibility by using the generic decoder.
    UnsupportedVectorShape,
    Projected(StoredV2KnnProjection),
}

/// Parse and validate the complete ZBS2 directory without decoding payloads.
///
/// Keeping framing validation separate from payload decoding allows callers
/// to skip large unrelated columns while retaining the full decoder's
/// truncation, UTF-8 and codec-id checks.
fn parse_v2_directory(body: &[u8]) -> Result<V2Directory<'_>> {
    let mut cur = Cursor::new(body);
    let num_docs = cur
        .read_u32::<LittleEndian>()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 num_docs: {e}")))?
        as usize;
    let num_cols = cur
        .read_u32::<LittleEndian>()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 num_cols: {e}")))?
        as usize;
    const MIN_COLUMN_FRAMING_BYTES: usize =
        std::mem::size_of::<u16>() + std::mem::size_of::<u8>() + std::mem::size_of::<u32>();
    let remaining = body.len().saturating_sub(cur.position() as usize);
    if num_cols > remaining / MIN_COLUMN_FRAMING_BYTES {
        return Err(StorageError::Other(anyhow::anyhow!(
            "v2 num_cols {num_cols} exceeds the remaining directory framing"
        )));
    }
    let mut columns = Vec::new();
    columns.try_reserve_exact(num_cols).map_err(|error| {
        StorageError::Other(anyhow::anyhow!(
            "v2 cannot reserve directory for {num_cols} columns: {error}"
        ))
    })?;
    let mut seen = std::collections::HashSet::new();
    seen.try_reserve(num_cols).map_err(|error| {
        StorageError::Other(anyhow::anyhow!(
            "v2 cannot reserve duplicate-name set for {num_cols} columns: {error}"
        ))
    })?;
    for _ in 0..num_cols {
        let name_len = cur
            .read_u16::<LittleEndian>()
            .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 name_len: {e}")))?
            as usize;
        let name_start = cur.position() as usize;
        let name_end = name_start
            .checked_add(name_len)
            .filter(|end| *end <= body.len())
            .ok_or_else(|| StorageError::Other(anyhow::anyhow!("v2 truncated name")))?;
        let name = std::str::from_utf8(&body[name_start..name_end])
            .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 bad name utf8: {e}")))?;
        if !seen.insert(name) {
            return Err(StorageError::Other(anyhow::anyhow!(
                "v2 duplicate column name: {name}"
            )));
        }
        cur.set_position(name_end as u64);
        let codec_id = cur
            .read_u8()
            .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 codec_id: {e}")))?;
        let codec = ColCodec::from_u8(codec_id)
            .ok_or_else(|| StorageError::Other(anyhow::anyhow!("v2 unknown codec {codec_id}")))?;
        let payload_len = cur
            .read_u32::<LittleEndian>()
            .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 payload_len: {e}")))?
            as usize;
        let payload_start = cur.position() as usize;
        let payload_end = payload_start
            .checked_add(payload_len)
            .filter(|end| *end <= body.len())
            .ok_or_else(|| StorageError::Other(anyhow::anyhow!("v2 truncated payload")))?;
        columns.push(V2ColumnRef {
            name,
            codec,
            payload: &body[payload_start..payload_end],
        });
        cur.set_position(payload_end as u64);
    }
    if cur.position() as usize != body.len() {
        return Err(StorageError::Other(anyhow::anyhow!(
            "v2 trailing bytes after column payloads"
        )));
    }
    Ok(V2Directory { num_docs, columns })
}

/// Decode only named columns from a V2 stored section.
///
/// This API does not change `decode_stored` or any engine behavior. It is the
/// storage primitive for consumers that can operate on selected columns
/// without reconstructing every `_source` object.
pub fn decode_stored_v2_projection(
    bytes: &[u8],
    requested: &[&str],
) -> Result<StoredV2ProjectionResult> {
    if bytes.len() < 4 || &bytes[..4] != STORED_V2_MAGIC {
        return Ok(StoredV2ProjectionResult::NotV2);
    }
    let directory = parse_v2_directory(&bytes[4..])?;
    let requested: std::collections::HashSet<&str> = requested.iter().copied().collect();
    let mut decoded = vec![None; directory.columns.len()];
    let mut visiting = vec![false; directory.columns.len()];
    let mut columns = HashMap::with_capacity(requested.len());
    for index in 0..directory.columns.len() {
        let column = &directory.columns[index];
        if requested.contains(column.name) {
            let values =
                decode_projected_column(index, &directory, &mut decoded, &mut visiting, 0)?.clone();
            columns.insert(column.name.to_string(), values);
        }
    }
    Ok(StoredV2ProjectionResult::Projected(StoredV2Projection {
        num_docs: directory.num_docs,
        columns,
    }))
}

fn decode_cross_dep_body(payload: &[u8], num_docs: usize) -> Result<Vec<u8>> {
    if payload.is_empty() {
        return Err(StorageError::Other(anyhow::anyhow!("cross_dep empty")));
    }
    // A source dictionary cannot contain more entries than source rows.
    // Each mode costs 8 bytes and each row contributes at most one
    // (u32-delta, i64-zigzag) exception of at most 15 bytes.
    let max_body = num_docs
        .checked_mul(23)
        .and_then(|row_bytes| 12usize.checked_add(row_bytes))
        .ok_or_else(|| StorageError::Other(anyhow::anyhow!("cross_dep size overflow")))?;
    let body = if payload[0] == 1 {
        if zstd_frame_content_size(&payload[1..], "cross_dep")?
            .is_some_and(|size| size > max_body as u64)
        {
            return Err(StorageError::Other(anyhow::anyhow!(
                "cross_dep frame exceeds maximum decoded size"
            )));
        }
        let decoder = zstd::stream::read::Decoder::new(&payload[1..])
            .map_err(|e| StorageError::Other(anyhow::anyhow!("cross_dep zstd decode: {e}")))?;
        let mut bounded = decoder.take(max_body as u64 + 1);
        let mut body = Vec::new();
        bounded
            .read_to_end(&mut body)
            .map_err(|e| StorageError::Other(anyhow::anyhow!("cross_dep zstd decode: {e}")))?;
        if body.len() > max_body {
            return Err(StorageError::Other(anyhow::anyhow!(
                "cross_dep decoded body exceeds limit"
            )));
        }
        body
    } else if payload[0] == 0 {
        if payload.len() - 1 > max_body {
            return Err(StorageError::Other(anyhow::anyhow!(
                "cross_dep raw body exceeds limit"
            )));
        }
        payload[1..].to_vec()
    } else {
        Err(StorageError::Other(anyhow::anyhow!(
            "cross_dep invalid compression flag {}",
            payload[0]
        )))?
    };
    Ok(body)
}

fn cross_dep_source_index(payload: &[u8], num_docs: usize) -> Result<usize> {
    let body = decode_cross_dep_body(payload, num_docs)?;
    Cursor::new(body)
        .read_u32::<LittleEndian>()
        .map(|value| value as usize)
        .map_err(|e| StorageError::Other(anyhow::anyhow!("cross_dep src_ix: {e}")))
}

fn decode_projected_column<'a>(
    index: usize,
    directory: &'a V2Directory<'a>,
    decoded: &'a mut [Option<Vec<serde_json::Value>>],
    visiting: &mut [bool],
    depth: usize,
) -> Result<&'a Vec<serde_json::Value>> {
    const MAX_CROSS_DEP_DEPTH: usize = 256;
    if depth > MAX_CROSS_DEP_DEPTH {
        return Err(StorageError::Other(anyhow::anyhow!(
            "cross_dep dependency depth exceeds {MAX_CROSS_DEP_DEPTH}"
        )));
    }
    if decoded.get(index).and_then(Option::as_ref).is_some() {
        return Ok(decoded[index].as_ref().unwrap());
    }
    if index >= directory.columns.len() || index >= visiting.len() {
        return Err(StorageError::Other(anyhow::anyhow!(
            "cross_dep source index {index} out of range"
        )));
    }
    if visiting[index] {
        return Err(StorageError::Other(anyhow::anyhow!(
            "cross_dep dependency cycle at column {index}"
        )));
    }
    visiting[index] = true;
    let column = &directory.columns[index];
    let values = match column.codec {
        ColCodec::RawJson => decode_raw_json(column.payload)?,
        ColCodec::Lz4Json => decode_lz4_json(column.payload)?,
        ColCodec::Constant => decode_constant(column.payload, directory.num_docs)?,
        ColCodec::DictBitpack => decode_dict_bitpack(column.payload, directory.num_docs)?,
        ColCodec::CrossDep => {
            let source_index = cross_dep_source_index(column.payload, directory.num_docs)?;
            let source =
                decode_projected_column(source_index, directory, decoded, visiting, depth + 1)?
                    .clone();
            let mut dependencies = vec![Vec::new(); directory.columns.len()];
            dependencies[source_index] = source;
            decode_cross_dep(
                column.payload,
                directory.num_docs,
                &dependencies,
                &HashMap::new(),
            )?
            .ok_or_else(|| {
                StorageError::Other(anyhow::anyhow!(
                    "cross_dep source column {source_index} was not resolved"
                ))
            })?
        }
    };
    visiting[index] = false;
    if values.len() != directory.num_docs {
        return Err(StorageError::Other(anyhow::anyhow!(
            "v2 column {} row count {} != header {}",
            column.name,
            values.len(),
            directory.num_docs
        )));
    }
    decoded[index] = Some(values);
    Ok(decoded[index].as_ref().unwrap())
}

fn finite_f32_vector(values: Vec<f64>) -> Option<Vec<f32>> {
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let value = value as f32;
        if !value.is_finite() {
            return None;
        }
        out.push(value);
    }
    Some(out)
}

struct TypedVectorRowsVisitor<'a> {
    expected_rows: usize,
    column_index: usize,
    checkpoint: &'a mut dyn FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()>,
    cancelled: &'a std::cell::Cell<bool>,
}

impl<'de> serde::de::Visitor<'de> for TypedVectorRowsVisitor<'_> {
    type Value = Option<StoredVectorRows>;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a vector column JSON array")
    }

    fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut out = Vec::new();
        out.try_reserve_exact(self.expected_rows)
            .map_err(serde::de::Error::custom)?;
        let mut supported = true;
        for row in 0..self.expected_rows {
            let Some(value) = seq.next_element::<serde_json::Value>()? else {
                return Err(serde::de::Error::custom("vector column ended early"));
            };
            let typed = match value {
                serde_json::Value::Null => None,
                serde_json::Value::Array(values) => {
                    let values = values
                        .into_iter()
                        .map(|value| value.as_f64())
                        .collect::<Option<Vec<_>>>()
                        .and_then(finite_f32_vector);
                    let Some(values) = values else {
                        supported = false;
                        out.push(None);
                        if checkpoint_decoded_row(
                            row,
                            self.column_index,
                            self.checkpoint,
                            self.cancelled,
                        ) {
                            return Err(serde::de::Error::custom("stored decode cancelled"));
                        }
                        continue;
                    };
                    Some(values)
                }
                _ => {
                    supported = false;
                    None
                }
            };
            out.push(typed);
            if checkpoint_decoded_row(row, self.column_index, self.checkpoint, self.cancelled) {
                return Err(serde::de::Error::custom("stored decode cancelled"));
            }
        }
        if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::custom("vector column has extra rows"));
        }
        Ok(supported.then_some(out))
    }
}

fn checkpoint_decoded_row(
    zero_based_row: usize,
    column_index: usize,
    checkpoint: &mut dyn FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()>,
    cancelled: &std::cell::Cell<bool>,
) -> bool {
    let processed = zero_based_row + 1;
    if processed.is_multiple_of(STORED_DECODE_CHECK_INTERVAL)
        && checkpoint(StoredDecodeCheckpoint {
            phase: StoredDecodePhase::Rows,
            column_index,
            rows_processed: processed,
        })
        .is_break()
    {
        cancelled.set(true);
        return true;
    }
    false
}

fn repeat_constant_rows_controlled<T: Clone>(
    row: T,
    num_docs: usize,
    column_index: usize,
    checkpoint: &mut dyn FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()>,
) -> Result<std::result::Result<Vec<T>, ()>> {
    let mut out = Vec::new();
    out.try_reserve_exact(num_docs).map_err(|error| {
        StorageError::Other(anyhow::anyhow!(
            "cannot reserve {num_docs} constant stored rows: {error}"
        ))
    })?;
    for zero_based_row in 0..num_docs {
        out.push(row.clone());
        let processed = zero_based_row + 1;
        if processed.is_multiple_of(STORED_DECODE_CHECK_INTERVAL)
            && checkpoint(StoredDecodeCheckpoint {
                phase: StoredDecodePhase::Rows,
                column_index,
                rows_processed: processed,
            })
            .is_break()
        {
            return Ok(Err(()));
        }
    }
    Ok(Ok(out))
}

fn decode_typed_vector_rows_controlled(
    column: &V2ColumnRef<'_>,
    num_docs: usize,
    column_index: usize,
    checkpoint: &mut dyn FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()>,
) -> Result<std::result::Result<Option<StoredVectorRows>, ()>> {
    if column.codec == ColCodec::Constant {
        let row = match serde_json::from_slice::<serde_json::Value>(column.payload)
            .map_err(|error| StorageError::Other(anyhow::anyhow!("constant decode: {error}")))?
        {
            serde_json::Value::Null => None,
            serde_json::Value::Array(values) => {
                let Some(values) = values
                    .into_iter()
                    .map(|value| value.as_f64())
                    .collect::<Option<Vec<_>>>()
                    .and_then(finite_f32_vector)
                else {
                    return Ok(Ok(None));
                };
                Some(values)
            }
            _ => return Ok(Ok(None)),
        };
        return repeat_constant_rows_controlled(row, num_docs, column_index, checkpoint)
            .map(|result| result.map(Some));
    }
    match column.codec {
        ColCodec::RawJson => {
            let decoder = zstd::stream::read::Decoder::new(column.payload).map_err(|error| {
                StorageError::Other(anyhow::anyhow!("raw zstd decode: {error}"))
            })?;
            parse_typed_vector_reader(decoder, num_docs, column_index, checkpoint)
        }
        ColCodec::Lz4Json => {
            let raw = lz4_flex::decompress_size_prepended(column.payload)
                .map_err(|error| StorageError::Other(anyhow::anyhow!("lz4 decode: {error}")))?;
            parse_typed_vector_reader(raw.as_slice(), num_docs, column_index, checkpoint)
        }
        _ => Ok(Ok(None)),
    }
}

fn parse_typed_vector_reader<R: std::io::Read>(
    reader: R,
    num_docs: usize,
    column_index: usize,
    checkpoint: &mut dyn FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()>,
) -> Result<std::result::Result<Option<StoredVectorRows>, ()>> {
    use serde::de::Deserializer as _;
    let cancelled = std::cell::Cell::new(false);
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    let value = match deserializer.deserialize_seq(TypedVectorRowsVisitor {
        expected_rows: num_docs,
        column_index,
        checkpoint,
        cancelled: &cancelled,
    }) {
        Ok(value) => value,
        Err(_) if cancelled.get() => return Ok(Err(())),
        Err(error) => {
            return Err(StorageError::Other(anyhow::anyhow!(
                "vector json decode: {error}"
            )));
        }
    };
    if value.is_some() {
        deserializer
            .end()
            .map_err(|error| StorageError::Other(anyhow::anyhow!("vector json decode: {error}")))?;
    }
    Ok(Ok(value))
}

fn decode_typed_vector_chunk_rows_controlled(
    column: &V2ColumnRef<'_>,
    num_docs: usize,
    column_index: usize,
    checkpoint: &mut dyn FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()>,
) -> Result<std::result::Result<Option<StoredVectorChunkRows>, ()>> {
    if column.codec == ColCodec::Constant {
        let row = match serde_json::from_slice::<serde_json::Value>(column.payload)
            .map_err(|error| StorageError::Other(anyhow::anyhow!("constant decode: {error}")))?
        {
            serde_json::Value::Null => None,
            serde_json::Value::Array(chunks) => {
                let mut typed = Vec::new();
                typed.try_reserve_exact(chunks.len()).map_err(|error| {
                    StorageError::Other(anyhow::anyhow!(
                        "cannot reserve {} constant vector chunks: {error}",
                        chunks.len()
                    ))
                })?;
                for chunk in chunks {
                    let serde_json::Value::Array(values) = chunk else {
                        return Ok(Ok(None));
                    };
                    let Some(vector) = values
                        .into_iter()
                        .map(|value| value.as_f64())
                        .collect::<Option<Vec<_>>>()
                        .and_then(finite_f32_vector)
                    else {
                        return Ok(Ok(None));
                    };
                    typed.push(vector);
                }
                Some(typed)
            }
            _ => return Ok(Ok(None)),
        };
        return repeat_constant_rows_controlled(row, num_docs, column_index, checkpoint)
            .map(|result| result.map(Some));
    }
    match column.codec {
        ColCodec::RawJson => {
            let decoder = zstd::stream::read::Decoder::new(column.payload).map_err(|error| {
                StorageError::Other(anyhow::anyhow!("raw zstd decode: {error}"))
            })?;
            parse_typed_chunk_reader(decoder, num_docs, column_index, checkpoint)
        }
        ColCodec::Lz4Json => {
            let raw = lz4_flex::decompress_size_prepended(column.payload)
                .map_err(|error| StorageError::Other(anyhow::anyhow!("lz4 decode: {error}")))?;
            parse_typed_chunk_reader(raw.as_slice(), num_docs, column_index, checkpoint)
        }
        _ => Ok(Ok(None)),
    }
}

fn parse_typed_chunk_reader<R: std::io::Read>(
    reader: R,
    num_docs: usize,
    column_index: usize,
    checkpoint: &mut dyn FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()>,
) -> Result<std::result::Result<Option<StoredVectorChunkRows>, ()>> {
    use serde::de::Deserializer as _;
    struct Visitor<'a> {
        rows: usize,
        column: usize,
        checkpoint: &'a mut dyn FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()>,
        cancelled: &'a std::cell::Cell<bool>,
    }
    impl<'de> serde::de::Visitor<'de> for Visitor<'_> {
        type Value = Option<StoredVectorChunkRows>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a vector chunks column JSON array")
        }
        fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut out = Vec::new();
            out.try_reserve_exact(self.rows)
                .map_err(serde::de::Error::custom)?;
            let mut supported = true;
            for row in 0..self.rows {
                let Some(value) = seq.next_element::<serde_json::Value>()? else {
                    return Err(serde::de::Error::custom("vector chunks ended early"));
                };
                let typed = match value {
                    serde_json::Value::Null => None,
                    serde_json::Value::Array(chunks) => {
                        let mut result = Vec::with_capacity(chunks.len());
                        for chunk in chunks {
                            let serde_json::Value::Array(values) = chunk else {
                                supported = false;
                                continue;
                            };
                            let vector = values
                                .into_iter()
                                .map(|value| value.as_f64())
                                .collect::<Option<Vec<_>>>()
                                .and_then(finite_f32_vector);
                            let Some(vector) = vector else {
                                supported = false;
                                continue;
                            };
                            result.push(vector);
                        }
                        Some(result)
                    }
                    _ => {
                        supported = false;
                        None
                    }
                };
                out.push(typed);
                let processed = row + 1;
                if processed.is_multiple_of(STORED_DECODE_CHECK_INTERVAL)
                    && (self.checkpoint)(StoredDecodeCheckpoint {
                        phase: StoredDecodePhase::Rows,
                        column_index: self.column,
                        rows_processed: processed,
                    })
                    .is_break()
                {
                    self.cancelled.set(true);
                    return Err(serde::de::Error::custom("stored decode cancelled"));
                }
            }
            if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                return Err(serde::de::Error::custom("vector chunks has extra rows"));
            }
            Ok(supported.then_some(out))
        }
    }
    let cancelled = std::cell::Cell::new(false);
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    let value = match deserializer.deserialize_seq(Visitor {
        rows: num_docs,
        column: column_index,
        checkpoint,
        cancelled: &cancelled,
    }) {
        Ok(value) => value,
        Err(_) if cancelled.get() => return Ok(Err(())),
        Err(error) => {
            return Err(StorageError::Other(anyhow::anyhow!(
                "vector chunks json decode: {error}"
            )));
        }
    };
    if value.is_some() {
        deserializer.end().map_err(|error| {
            StorageError::Other(anyhow::anyhow!("vector chunks json decode: {error}"))
        })?;
    }
    Ok(Ok(value))
}

/// Decode the identity and vector columns needed by exact kNN without
/// reconstructing complete source documents.
pub fn decode_stored_v2_knn_projection(
    bytes: &[u8],
    vector_field: &str,
    vector_chunks_field: Option<&str>,
) -> Result<StoredV2KnnProjectionResult> {
    match decode_stored_v2_knn_projection_controlled(
        bytes,
        vector_field,
        vector_chunks_field,
        |_| std::ops::ControlFlow::Continue(()),
    )? {
        StoredDecodeRun::Complete(result) => Ok(result),
        StoredDecodeRun::Cancelled => unreachable!("non-cancelling compatibility wrapper"),
    }
}

pub fn decode_stored_v2_knn_projection_controlled<F>(
    bytes: &[u8],
    vector_field: &str,
    vector_chunks_field: Option<&str>,
    mut checkpoint: F,
) -> Result<StoredDecodeRun<StoredV2KnnProjectionResult>>
where
    F: FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()>,
{
    if bytes.len() < 4 || &bytes[..4] != STORED_V2_MAGIC {
        return Ok(StoredDecodeRun::Complete(
            StoredV2KnnProjectionResult::NotV2,
        ));
    }
    if matches!(vector_field, "__id" | "__seq_no")
        || vector_chunks_field
            .is_some_and(|field| matches!(field, "__id" | "__seq_no") || field == vector_field)
    {
        return Ok(StoredDecodeRun::Complete(
            StoredV2KnnProjectionResult::UnsupportedVectorShape,
        ));
    }
    let directory = parse_v2_directory(&bytes[4..])?;
    let Some(vector_column) = directory
        .columns
        .iter()
        .find(|column| column.name == vector_field)
    else {
        return Ok(StoredDecodeRun::Complete(
            StoredV2KnnProjectionResult::MissingVectorColumn,
        ));
    };
    let vector_index = directory
        .columns
        .iter()
        .position(|column| std::ptr::eq(column, vector_column))
        .unwrap();
    if decode_cancelled(
        &mut checkpoint,
        StoredDecodePhase::BeforeColumn,
        vector_index,
        0,
    ) {
        return Ok(StoredDecodeRun::Cancelled);
    }
    let Some(vectors) = (match decode_typed_vector_rows_controlled(
        vector_column,
        directory.num_docs,
        vector_index,
        &mut checkpoint,
    )? {
        Ok(value) => value,
        Err(()) => return Ok(StoredDecodeRun::Cancelled),
    }) else {
        return Ok(StoredDecodeRun::Complete(
            StoredV2KnnProjectionResult::UnsupportedVectorShape,
        ));
    };
    if decode_cancelled(
        &mut checkpoint,
        StoredDecodePhase::AfterColumn,
        vector_index,
        directory.num_docs,
    ) {
        return Ok(StoredDecodeRun::Cancelled);
    }
    let vector_chunks = match vector_chunks_field {
        Some(field) => match directory.columns.iter().find(|column| column.name == field) {
            Some(column) => {
                let index = directory
                    .columns
                    .iter()
                    .position(|candidate| std::ptr::eq(candidate, column))
                    .unwrap();
                if decode_cancelled(&mut checkpoint, StoredDecodePhase::BeforeColumn, index, 0) {
                    return Ok(StoredDecodeRun::Cancelled);
                }
                let Some(chunks) = (match decode_typed_vector_chunk_rows_controlled(
                    column,
                    directory.num_docs,
                    index,
                    &mut checkpoint,
                )? {
                    Ok(value) => value,
                    Err(()) => return Ok(StoredDecodeRun::Cancelled),
                }) else {
                    return Ok(StoredDecodeRun::Complete(
                        StoredV2KnnProjectionResult::UnsupportedVectorShape,
                    ));
                };
                if decode_cancelled(
                    &mut checkpoint,
                    StoredDecodePhase::AfterColumn,
                    index,
                    directory.num_docs,
                ) {
                    return Ok(StoredDecodeRun::Cancelled);
                }
                Some(chunks)
            }
            None => None,
        },
        None => None,
    };
    let (identity_num_docs, id_values, seq_values) =
        match row_hydration::decode_v2_identity_columns_controlled(bytes, &mut checkpoint)? {
            StoredDecodeRun::Complete(Some(values)) => values,
            StoredDecodeRun::Complete(None) => {
                return Ok(StoredDecodeRun::Complete(
                    StoredV2KnnProjectionResult::UnsupportedVectorShape,
                ));
            }
            StoredDecodeRun::Cancelled => return Ok(StoredDecodeRun::Cancelled),
        };
    let ids = id_values
        .into_iter()
        .map(|value| value.as_str().map(ToOwned::to_owned))
        .collect();
    let seq_nos = seq_values.into_iter().map(|value| value.as_u64()).collect();
    Ok(StoredDecodeRun::Complete(
        StoredV2KnnProjectionResult::Projected(StoredV2KnnProjection {
            num_docs: identity_num_docs,
            ids,
            seq_nos,
            vectors,
            vector_chunks,
        }),
    ))
}

fn decode_stored_v2(body: &[u8]) -> Result<Vec<u8>> {
    let mut cur = Cursor::new(body);
    let num_docs = cur
        .read_u32::<LittleEndian>()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 num_docs: {e}")))?
        as usize;
    let num_cols = cur
        .read_u32::<LittleEndian>()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 num_cols: {e}")))?
        as usize;

    // First pass: decode each column into Vec<Value>.  Store the name
    // alongside so we can re-assemble the docs.
    let mut col_names: Vec<String> = Vec::with_capacity(num_cols);
    let mut col_data: Vec<Vec<serde_json::Value>> = Vec::with_capacity(num_cols);

    for _ in 0..num_cols {
        let name_len = cur
            .read_u16::<LittleEndian>()
            .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 name_len: {e}")))?
            as usize;
        let pos = cur.position() as usize;
        if body.len() < pos + name_len {
            return Err(StorageError::Other(anyhow::anyhow!("v2 truncated name")));
        }
        let name = std::str::from_utf8(&body[pos..pos + name_len])
            .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 bad name utf8: {e}")))?
            .to_string();
        cur.set_position((pos + name_len) as u64);

        let codec_id = cur
            .read_u8()
            .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 codec_id: {e}")))?;
        let codec = ColCodec::from_u8(codec_id)
            .ok_or_else(|| StorageError::Other(anyhow::anyhow!("v2 unknown codec {}", codec_id)))?;
        let payload_len = cur
            .read_u32::<LittleEndian>()
            .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 payload_len: {e}")))?
            as usize;
        let pos = cur.position() as usize;
        if body.len() < pos + payload_len {
            return Err(StorageError::Other(anyhow::anyhow!("v2 truncated payload")));
        }
        let payload = &body[pos..pos + payload_len];
        cur.set_position((pos + payload_len) as u64);

        let values = match codec {
            ColCodec::RawJson => decode_raw_json(payload)?,
            ColCodec::Lz4Json => decode_lz4_json(payload)?,
            ColCodec::Constant => decode_constant(payload, num_docs)?,
            ColCodec::DictBitpack => decode_dict_bitpack(payload, num_docs)?,
            ColCodec::CrossDep => {
                // CROSS_DEP requires the source column to already be
                // decoded.  We decode it in a second pass below — here we
                // emit a placeholder and revisit it after.
                col_names.push(name);
                col_data.push(Vec::new());
                // Stash the raw payload in col_data[i] as a single
                // "deferred" Value.  Second pass recognises and replaces.
                col_data
                    .last_mut()
                    .unwrap()
                    .push(serde_json::Value::Object({
                        let mut m = serde_json::Map::new();
                        m.insert(
                            "__deferred_cross_dep__".into(),
                            serde_json::Value::Array(
                                payload
                                    .iter()
                                    .map(|b| serde_json::Value::Number((*b).into()))
                                    .collect(),
                            ),
                        );
                        m
                    }));
                continue;
            }
        };
        col_names.push(name);
        col_data.push(values);
    }

    // Second pass: resolve deferred CROSS_DEP columns now that the pass-1
    // source columns are materialised.
    //
    // FIXPOINT (2026-07, S1 decode hardening): the old resolver was a
    // SINGLE sweep in column-index order — a CrossDep column whose source
    // was a LATER CrossDep column (forward reference) failed with
    // "cross_dep src not resolved" and the entire stored section became
    // undecodable, which the merge path then amplified into wholesale doc
    // loss.  Now we sweep repeatedly, resolving every column whose source
    // has materialised, until either all are resolved or a sweep makes no
    // progress.  No-progress means a true dependency CYCLE on disk
    // (mathematically unrecoverable — the mode tables of both columns
    // reference each other's values); we return a hard error so callers
    // (e.g. the merge loss-firewall) PRESERVE the segment instead of
    // dropping its docs.  The encode-side cycle breaker stops new cycles
    // from being written; this loop recovers forward-chain segments
    // already on disk.
    let col_name_to_ix: HashMap<String, usize> = col_names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), i))
        .collect();
    let is_deferred = |col: &Vec<serde_json::Value>| -> Option<Vec<u8>> {
        if col.len() == 1 {
            if let serde_json::Value::Object(ref m) = col[0] {
                if let Some(serde_json::Value::Array(bytes_arr)) = m.get("__deferred_cross_dep__") {
                    return Some(
                        bytes_arr
                            .iter()
                            .filter_map(|v| v.as_u64().map(|u| u as u8))
                            .collect(),
                    );
                }
            }
        }
        None
    };
    loop {
        let mut progressed = false;
        let mut still_deferred = 0usize;
        for cix in 0..col_data.len() {
            let Some(payload) = is_deferred(&col_data[cix]) else {
                continue;
            };
            match decode_cross_dep(&payload, num_docs, &col_data, &col_name_to_ix)? {
                Some(resolved) => {
                    col_data[cix] = resolved;
                    progressed = true;
                }
                None => still_deferred += 1,
            }
        }
        if still_deferred == 0 {
            break;
        }
        if !progressed {
            return Err(StorageError::Other(anyhow::anyhow!(
                "cross_dep dependency cycle: {still_deferred} column(s) unresolvable"
            )));
        }
    }

    // Re-assemble the JSON-array payload.  col_data[0] = __id, col_data[1] = __seq_no.
    let id_col_ix = col_name_to_ix.get("__id").copied().unwrap_or(0);
    let seq_col_ix = col_name_to_ix.get("__seq_no").copied().unwrap_or(1);
    // Absent on every segment written before this column existed, and on every
    // block whose documents store no explicit null — both decode exactly as
    // they did before.
    let nulls_col_ix = col_name_to_ix.get(V2_EXPLICIT_NULLS_COLUMN).copied();

    let mut out_docs: Vec<serde_json::Value> = Vec::new();
    out_docs.try_reserve_exact(num_docs).map_err(|error| {
        StorageError::Other(anyhow::anyhow!(
            "cannot reserve {num_docs} decoded stored rows: {error}"
        ))
    })?;
    for d in 0..num_docs {
        let mut source_map = serde_json::Map::new();
        let stored_null = nulls_col_ix
            .and_then(|cix| col_data[cix].get(d))
            .and_then(serde_json::Value::as_array);
        for (cix, name) in col_names.iter().enumerate() {
            if cix == id_col_ix || cix == seq_col_ix || Some(cix) == nulls_col_ix {
                continue;
            }
            let v = col_data[cix]
                .get(d)
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            if !v.is_null() {
                source_map.insert(name.clone(), v);
            } else if column_stored_null(stored_null, name) {
                // Present in the document and `null` — not absent.  Restored
                // in column order so the key lands where a non-null value of
                // the same field would have.
                source_map.insert(name.clone(), serde_json::Value::Null);
            }
        }
        let mut doc = serde_json::Map::new();
        doc.insert(
            "_id".into(),
            col_data[id_col_ix]
                .get(d)
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        );
        doc.insert(
            "_seq_no".into(),
            col_data[seq_col_ix]
                .get(d)
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        );
        doc.insert("_source".into(), serde_json::Value::Object(source_map));
        out_docs.push(serde_json::Value::Object(doc));
    }

    serde_json::to_vec(&out_docs)
        .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 reassemble: {e}")))
}

/// Did this row store `name` as an explicit `null`?
///
/// Defensive about the payload shape: a legacy segment carrying a *user* field
/// called `__nulls` decodes to whatever it held, and anything that is not an
/// array of strings restores nothing — i.e. it degrades to the pre-fix
/// behaviour rather than inventing keys.
pub(crate) fn column_stored_null(stored_null: Option<&Vec<serde_json::Value>>, name: &str) -> bool {
    stored_null.is_some_and(|names| names.iter().any(|stored| stored.as_str() == Some(name)))
}

// ── Column-level helpers ─────────────────────────────────────────────────

/// True iff every non-null value in the column is a numeric *integer*
/// (`as_i64`/`as_u64`), i.e. nothing would be lost by routing it through
/// the i64 mode-table / cross-dependency path.
///
/// The cross-dep codec models the column as `i64` mode values plus i64
/// exceptions, so any value carrying a fractional component (`0.010127`)
/// would be truncated to its integer part and silently corrupt the stored
/// `_source`.  A column that contains even one non-integer float must skip
/// the numeric optimization entirely and fall back to the lossless dict /
/// raw-JSON path, which preserves the exact `serde_json::Value`.
fn col_is_all_integer<B: std::borrow::Borrow<serde_json::Value>>(col: &[B]) -> bool {
    let mut saw_num = false;
    for v in col {
        let v = v.borrow();
        if v.is_null() {
            continue;
        }
        // Only true JSON integers qualify; floats (even integer-valued
        // ones like `10.0`) are left for the lossless path so their exact
        // representation round-trips.
        if v.as_i64().is_none() && v.as_u64().is_none() {
            return false;
        }
        saw_num = true;
    }
    saw_num
}

fn all_scalar_dict_entries(entries: &[serde_json::Value]) -> bool {
    entries
        .iter()
        .all(|v| v.is_null() || v.is_string() || v.is_number() || v.is_boolean())
}

/// Build a dictionary representation `(entries, ids)` where:
/// * `entries[0..entries.len()]` are unique values in first-seen order
/// * `ids[i] = index_of_value(col[i])`; `null` values get id = `entries.len()` (reserved)
///
/// Returns `None` if the column contains any non-scalar (object/array) value
/// or if the unique-count explodes beyond the cap.
/// Typed dictionary key — replaces the old `v.to_string()` keying, which
/// allocated a fresh String per CELL (13 columns × 31k rows ≈ 400k allocs
/// per flush segment).  Strings are borrowed straight from the column;
/// numbers are keyed by their exact serde variant so `1`, `1.0` and
/// `"1"` stay distinct exactly as their serialized forms did.
#[derive(PartialEq, Eq, Hash)]
enum DictKey<'a> {
    Str(&'a str),
    PosInt(u64),
    NegInt(i64),
    Float(u64), // f64 bit pattern
    Bool(bool),
}

fn dict_key(v: &serde_json::Value) -> Option<DictKey<'_>> {
    match v {
        serde_json::Value::String(s) => Some(DictKey::Str(s.as_str())),
        serde_json::Value::Bool(b) => Some(DictKey::Bool(*b)),
        serde_json::Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                Some(DictKey::PosInt(u))
            } else if let Some(i) = n.as_i64() {
                Some(DictKey::NegInt(i))
            } else {
                n.as_f64().map(|f| DictKey::Float(f.to_bits()))
            }
        }
        _ => None,
    }
}

fn dict_encode_column<'a, B: std::borrow::Borrow<serde_json::Value>>(
    col: &'a [B],
) -> Option<(Vec<serde_json::Value>, Vec<u32>)> {
    // FxHashMap: this map sees one lookup per row per column at flush
    // time (~400k per 31k-doc segment); SipHash was measurable there and
    // the keys are the tenant's own field values (no HashDoS surface).
    let mut map: rustc_hash::FxHashMap<DictKey<'a>, u32> = rustc_hash::FxHashMap::default();
    let mut entries: Vec<serde_json::Value> = Vec::new();
    let mut ids: Vec<u32> = Vec::with_capacity(col.len());
    // Early abort for id-like columns: if the first `UNIQUE_PREFIX_ABORT`
    // non-null values are ALL distinct (`__id` on auto-id bulk, UUIDs…),
    // no dictionary ≤ DICT_MAX_CARDINALITY can emerge from ≤ 65k rows
    // worth scanning — bail before cloning thousands of entries.  A
    // column that merely starts diverse (e.g. timestamps) repeats within
    // this window and is unaffected.
    const UNIQUE_PREFIX_ABORT: usize = 8_192;
    let mut non_null = 0usize;
    for v in col {
        let v = v.borrow();
        if v.is_null() {
            ids.push(u32::MAX); // resolved to "null id" later
            continue;
        }
        non_null += 1;
        let k = dict_key(v)?; // object / array
        if let Some(&id) = map.get(&k) {
            ids.push(id);
        } else {
            let id = entries.len() as u32;
            entries.push(v.clone());
            map.insert(k, id);
            ids.push(id);
        }
        if entries.len() >= UNIQUE_PREFIX_ABORT && entries.len() == non_null {
            return None;
        }
        if entries.len() > DICT_MAX_CARDINALITY {
            // Above the payload cap: both the DICT_BITPACK pick in
            // `encode_v2_columns` and the cross-dep source filter reject
            // dictionaries larger than DICT_MAX_CARDINALITY, so building
            // one bigger than that (the old cap was 4×) was pure wasted
            // work — e.g. the per-doc-unique `__id` column cloned ~31k
            // Values per flush segment only to be discarded.
            return None;
        }
    }
    // Replace u32::MAX with the reserved-null id.
    let null_id = entries.len() as u32;
    for id in ids.iter_mut() {
        if *id == u32::MAX {
            *id = null_id;
        }
    }
    Some((entries, ids))
}

/// For a numeric target column, find the index of an earlier dict-encoded
/// column (if any) whose dict ids deterministically predict the target at
/// ≥ `CROSS_DEP_MIN_DETERMINISM` fraction of rows.
fn best_cross_dep_source<B: std::borrow::Borrow<serde_json::Value>>(
    dict_encoded: &[Option<(Vec<serde_json::Value>, Vec<u32>)>],
    target_ix: usize,
    target_col: &[B],
) -> Option<usize> {
    for (src_ix, de) in dict_encoded.iter().enumerate() {
        if src_ix == target_ix {
            continue;
        }
        let (entries, ids) = match de {
            Some(e) => e,
            None => continue,
        };
        if !all_scalar_dict_entries(entries) {
            continue;
        }
        if entries.len() < 2 {
            continue;
        } // constant source: useless
        if entries.len() > CROSS_DEP_MAX_SRC_CARDINALITY {
            continue;
        }

        // Sampled prefilter: tally only the first `SAMPLE` rows first.
        // Cross-dep is a codec CHOICE — a false negative here just means
        // the column keeps its dict/zstd codec (identical decode), so a
        // cheap sample with a slightly relaxed threshold is safe.  The
        // full O(rows) tally (below) runs only for sources that pass,
        // which cuts the per-flush cross-dep probe from ~110 ms to ~5 ms
        // on non-deterministic corpora (the common case).
        const SAMPLE: usize = 2048;
        const SAMPLE_SLACK: f64 = 0.05;
        if target_col.len() > SAMPLE * 2 {
            if let Some(det) = cross_dep_determinism(&target_col[..SAMPLE], ids) {
                if det < CROSS_DEP_MIN_DETERMINISM - SAMPLE_SLACK {
                    continue;
                }
            } else {
                continue;
            }
        }

        // Full single-pass tally.  The number of rows matching their
        // source-id's mode equals Σ_sid max_count(tally[sid]) — no second
        // row scan needed (the old two-pass version re-walked every row
        // against `mode_pick`, doubling the cost for an identical result).
        match cross_dep_determinism(target_col, ids) {
            Some(det) if det >= CROSS_DEP_MIN_DETERMINISM => return Some(src_ix),
            _ => {}
        }
    }
    None
}

/// Fraction of numeric rows whose value equals the mode of their source
/// dict id (`None` when no numeric rows).  Shared by the sampled
/// prefilter and the full pass in [`best_cross_dep_source`].
fn cross_dep_determinism<B: std::borrow::Borrow<serde_json::Value>>(
    target_col: &[B],
    ids: &[u32],
) -> Option<f64> {
    let mut mode_tally: HashMap<u32, HashMap<i64, usize>> = HashMap::new();
    let mut total_numeric = 0usize;
    for (row, t_val) in target_col.iter().enumerate() {
        let t_val = t_val.borrow();
        let Some(t) = t_val.as_i64().or_else(|| t_val.as_f64().map(|f| f as i64)) else {
            continue;
        };
        total_numeric += 1;
        let sid = ids[row];
        *mode_tally.entry(sid).or_default().entry(t).or_insert(0) += 1;
    }
    if total_numeric == 0 {
        return None;
    }
    let hits: usize = mode_tally
        .values()
        .map(|tally| tally.values().copied().max().unwrap_or(0))
        .sum();
    Some(hits as f64 / total_numeric as f64)
}

fn try_encode_constant<B: std::borrow::Borrow<serde_json::Value>>(col: &[B]) -> Option<Vec<u8>> {
    let first = col.iter().map(|v| v.borrow()).find(|v| !v.is_null())?;
    if col.iter().all(|v| {
        let v = v.borrow();
        v == first || v.is_null()
    }) && !col.iter().any(|v| v.borrow().is_null())
    {
        // Only encode as constant when there are no nulls (keep the codec simple).
        return serde_json::to_vec(first).ok();
    }
    None
}

fn decode_constant(payload: &[u8], num_docs: usize) -> Result<Vec<serde_json::Value>> {
    let v: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|e| StorageError::Other(anyhow::anyhow!("constant decode: {e}")))?;
    let mut values = Vec::new();
    values.try_reserve_exact(num_docs).map_err(|error| {
        StorageError::Other(anyhow::anyhow!(
            "cannot reserve {num_docs} constant stored rows: {error}"
        ))
    })?;
    values.resize(num_docs, v);
    Ok(values)
}

fn encode_dict_bitpack(entries: &[serde_json::Value], ids: &[u32], level: i32) -> Vec<u8> {
    let dict_count = entries.len();
    // Reserve one extra id for null → dict_count (which might be past the
    // packed range, so we allocate `bit_width` large enough for dict_count
    // inclusive).
    let max_id = dict_count as u32;
    let bit_width = if max_id == 0 {
        1
    } else {
        32 - max_id.leading_zeros() as u8
    };

    let mut out = Vec::new();
    out.write_u32::<LittleEndian>(dict_count as u32).unwrap();
    out.push(bit_width);

    // Dict entries as zstd(json array).
    let dict_json = serde_json::to_vec(entries).unwrap_or_default();
    let dict_zstd = zstd::encode_all(Cursor::new(&dict_json), level).unwrap_or(dict_json.clone());
    out.write_u32::<LittleEndian>(dict_zstd.len() as u32)
        .unwrap();
    out.extend_from_slice(&dict_zstd);

    // Bit-packed ids.
    let packed = bitpack_u32(ids, bit_width);
    // zstd over the bit-packed stream — gives another 20-40 % on log
    // data because repeated ids cluster.
    let packed_zstd = zstd::encode_all(Cursor::new(&packed), level).unwrap_or(packed.clone());
    out.write_u32::<LittleEndian>(ids.len() as u32).unwrap();
    out.write_u32::<LittleEndian>(packed_zstd.len() as u32)
        .unwrap();
    out.extend_from_slice(&packed_zstd);
    out
}

fn decode_dict_bitpack(payload: &[u8], num_docs: usize) -> Result<Vec<serde_json::Value>> {
    let mut cur = Cursor::new(payload);
    let dict_count =
        cur.read_u32::<LittleEndian>()
            .map_err(|e| StorageError::Other(anyhow::anyhow!("dict_count: {e}")))? as usize;
    if dict_count > num_docs {
        return Err(StorageError::Other(anyhow::anyhow!(
            "dict_count {dict_count} exceeds num_docs {num_docs}"
        )));
    }
    let bit_width = cur
        .read_u8()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("bit_width: {e}")))?;
    if !(1..=32).contains(&bit_width) {
        return Err(StorageError::Other(anyhow::anyhow!(
            "dict invalid bit_width {bit_width}"
        )));
    }

    let dict_zstd_len = cur
        .read_u32::<LittleEndian>()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("dict_zstd_len: {e}")))?
        as usize;
    let pos = cur.position() as usize;
    if payload.len() < pos + dict_zstd_len {
        return Err(StorageError::Other(anyhow::anyhow!(
            "dict bitpack truncated"
        )));
    }
    let dict_json = zstd::decode_all(&payload[pos..pos + dict_zstd_len])
        .map_err(|e| StorageError::Other(anyhow::anyhow!("dict zstd decode: {e}")))?;
    let entries: Vec<serde_json::Value> = serde_json::from_slice(&dict_json)
        .map_err(|e| StorageError::Other(anyhow::anyhow!("dict json decode: {e}")))?;
    cur.set_position((pos + dict_zstd_len) as u64);

    let ids_len =
        cur.read_u32::<LittleEndian>()
            .map_err(|e| StorageError::Other(anyhow::anyhow!("ids_len: {e}")))? as usize;
    if ids_len != num_docs {
        return Err(StorageError::Other(anyhow::anyhow!(
            "dict bitpack ids_len {} != num_docs {}",
            ids_len,
            num_docs
        )));
    }
    let packed_zstd_len = cur
        .read_u32::<LittleEndian>()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("packed_zstd_len: {e}")))?
        as usize;
    let pos = cur.position() as usize;
    let packed_end = pos
        .checked_add(packed_zstd_len)
        .filter(|end| *end == payload.len())
        .ok_or_else(|| {
            StorageError::Other(anyhow::anyhow!("dict bitpack packed truncated/trailing"))
        })?;
    let expected_packed_len = num_docs
        .checked_mul(bit_width as usize)
        .ok_or_else(|| StorageError::Other(anyhow::anyhow!("dict packed length overflow")))?
        .div_ceil(8);
    let packed = decode_zstd_exact(
        &payload[pos..packed_end],
        expected_packed_len,
        "packed zstd decode",
    )?;

    let ids = bitunpack_u32(&packed, bit_width, num_docs);
    let null_id = dict_count as u32;
    let values: Vec<serde_json::Value> = ids
        .into_iter()
        .map(|id| {
            if id == null_id {
                serde_json::Value::Null
            } else {
                entries
                    .get(id as usize)
                    .cloned()
                    .unwrap_or(serde_json::Value::Null)
            }
        })
        .collect();
    Ok(values)
}

fn encode_cross_dep<B: std::borrow::Borrow<serde_json::Value>>(
    target_col: &[B],
    src_ix: usize,
    dict_encoded: &[Option<(Vec<serde_json::Value>, Vec<u32>)>],
    level: i32,
) -> (Vec<u8>, bool) {
    let (_src_entries, src_ids) = match &dict_encoded[src_ix] {
        Some(e) => e,
        None => return (Vec::new(), false),
    };

    // Recompute the mode table for just the subset.  We need:
    //   mode[src_id] = most frequent target i64
    //   exceptions  = list of (doc_ord, target_i64) where row deviates
    let dict_count = match &dict_encoded[src_ix] {
        Some((e, _)) => e.len(),
        None => return (Vec::new(), false),
    };
    let mut tally: Vec<HashMap<i64, u32>> = vec![HashMap::new(); dict_count + 1];
    for (row, t) in target_col.iter().enumerate() {
        let t = t.borrow();
        let Some(tv) = t.as_i64().or_else(|| t.as_f64().map(|f| f as i64)) else {
            continue;
        };
        let sid = src_ids[row] as usize;
        *tally[sid.min(dict_count)].entry(tv).or_insert(0) += 1;
    }
    let mut mode_values: Vec<i64> = vec![i64::MIN; dict_count];
    for (sid, t) in tally.iter().enumerate().take(dict_count) {
        if let Some((v, _)) = t.iter().max_by_key(|(_, c)| *c) {
            mode_values[sid] = *v;
        }
    }

    let mut exceptions: Vec<(u32, i64)> = Vec::new();
    for (row, t) in target_col.iter().enumerate() {
        let t = t.borrow();
        let Some(tv) = t.as_i64().or_else(|| t.as_f64().map(|f| f as i64)) else {
            // Null / non-numeric — emit as exception with sentinel i64::MIN+1 ? No,
            // simpler: mark with i64::MIN via sentinel tuple, but we cannot
            // distinguish, so encode using i64::MIN+1 meaning "null" in the decoder.
            exceptions.push((row as u32, i64::MIN + 1));
            continue;
        };
        let sid = src_ids[row] as usize;
        let expected = if sid < dict_count {
            mode_values[sid]
        } else {
            i64::MIN
        };
        if expected != tv {
            exceptions.push((row as u32, tv));
        }
    }

    // Serialise.
    let mut out = Vec::new();
    out.write_u32::<LittleEndian>(src_ix as u32).unwrap();
    out.write_u32::<LittleEndian>(dict_count as u32).unwrap();
    for &v in &mode_values {
        out.write_i64::<LittleEndian>(v).unwrap();
    }
    out.write_u32::<LittleEndian>(exceptions.len() as u32)
        .unwrap();
    // delta-encode doc_ords
    let mut prev_ord = 0u32;
    for (ord, val) in &exceptions {
        let d = ord.wrapping_sub(prev_ord);
        write_varint(&mut out, d as u64);
        write_zigzag_i64(&mut out, *val);
        prev_ord = *ord;
    }
    // zstd the whole thing.
    let compressed = zstd::encode_all(Cursor::new(&out), level).unwrap_or(out.clone());
    // Save the smaller of raw vs zstd.
    let mut final_out = Vec::with_capacity(1 + compressed.len().max(out.len()));
    if compressed.len() + 1 < out.len() {
        final_out.push(1);
        final_out.extend_from_slice(&compressed);
    } else {
        final_out.push(0);
        final_out.extend_from_slice(&out);
    }
    (final_out, true)
}

/// Decode one deferred CROSS_DEP column.
///
/// Returns `Ok(None)` when the SOURCE column is itself a still-deferred
/// CROSS_DEP placeholder — the fixpoint loop in `decode_stored_v2` retries
/// it on the next round once the source has materialised.  (Pre-2026-07
/// this case was a hard error inside a single-pass resolver: any forward
/// reference or cycle made the whole stored section undecodable.)
fn decode_cross_dep(
    payload: &[u8],
    num_docs: usize,
    col_data: &[Vec<serde_json::Value>],
    col_name_to_ix: &HashMap<String, usize>,
) -> Result<Option<Vec<serde_json::Value>>> {
    if payload.is_empty() {
        return Err(StorageError::Other(anyhow::anyhow!("cross_dep empty")));
    }
    let body = decode_cross_dep_body(payload, num_docs)?;
    let mut cur = Cursor::new(&body[..]);
    let src_ix = cur
        .read_u32::<LittleEndian>()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("cross_dep src_ix: {e}")))?
        as usize;
    let _ = col_name_to_ix; // not strictly needed yet
    let dict_count = cur
        .read_u32::<LittleEndian>()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("cross_dep dict_count: {e}")))?
        as usize;
    if dict_count > num_docs {
        return Err(StorageError::Other(anyhow::anyhow!(
            "cross_dep dict_count {dict_count} exceeds num_docs {num_docs}"
        )));
    }
    let mut mode_values: Vec<i64> = Vec::with_capacity(dict_count);
    for _ in 0..dict_count {
        mode_values.push(
            cur.read_i64::<LittleEndian>()
                .map_err(|e| StorageError::Other(anyhow::anyhow!("cross_dep mode: {e}")))?,
        );
    }
    let exc_count = cur
        .read_u32::<LittleEndian>()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("cross_dep exc_count: {e}")))?
        as usize;
    if exc_count > num_docs {
        return Err(StorageError::Other(anyhow::anyhow!(
            "cross_dep exc_count {exc_count} exceeds num_docs {num_docs}"
        )));
    }

    // Rebuild the source column's dict ids by re-running `dict_encode_column`
    // on the already-decoded source column.
    let src_col = col_data
        .get(src_ix)
        .ok_or_else(|| StorageError::Other(anyhow::anyhow!("cross_dep src missing")))?;
    if src_col.len() != num_docs {
        // Source column is itself a CROSS_DEP that has not been resolved
        // yet — tell the fixpoint loop to retry this column next round.
        return Ok(None);
    }
    let (_src_entries, src_ids) = dict_encode_column(src_col)
        .ok_or_else(|| StorageError::Other(anyhow::anyhow!("cross_dep re-dict src failed")))?;

    // Exceptions.
    let mut exc: Vec<(u32, i64)> = Vec::new();
    exc.try_reserve_exact(exc_count).map_err(|error| {
        StorageError::Other(anyhow::anyhow!(
            "cannot reserve {exc_count} cross-dependency exceptions: {error}"
        ))
    })?;
    let mut pos = cur.position() as usize;
    let mut prev_ord = 0u32;
    for _ in 0..exc_count {
        let delta = u32::try_from(read_varint(&body, &mut pos)?)
            .map_err(|_| StorageError::Other(anyhow::anyhow!("cross_dep ordinal overflow")))?;
        let val = read_zigzag_i64(&body, &mut pos)?;
        let ord = prev_ord
            .checked_add(delta)
            .ok_or_else(|| StorageError::Other(anyhow::anyhow!("cross_dep ordinal overflow")))?;
        if ord as usize >= num_docs || (!exc.is_empty() && ord <= prev_ord) {
            return Err(StorageError::Other(anyhow::anyhow!(
                "cross_dep invalid exception ordinal {ord}"
            )));
        }
        exc.push((ord, val));
        prev_ord = ord;
    }
    if pos != body.len() {
        return Err(StorageError::Other(anyhow::anyhow!(
            "cross_dep trailing exception bytes"
        )));
    }

    // Materialise.
    let mut result: Vec<serde_json::Value> = Vec::new();
    result.try_reserve_exact(num_docs).map_err(|error| {
        StorageError::Other(anyhow::anyhow!(
            "cannot reserve {num_docs} cross-dependency rows: {error}"
        ))
    })?;
    let mut exc_ix = 0;
    for row in 0..num_docs {
        if exc_ix < exc.len() && exc[exc_ix].0 as usize == row {
            let v = exc[exc_ix].1;
            exc_ix += 1;
            if v == i64::MIN + 1 {
                result.push(serde_json::Value::Null);
            } else {
                result.push(serde_json::Value::Number(v.into()));
            }
            continue;
        }
        let sid = src_ids.get(row).copied().unwrap_or(0) as usize;
        let v = if sid < dict_count {
            mode_values[sid]
        } else {
            i64::MIN
        };
        if v == i64::MIN {
            result.push(serde_json::Value::Null);
        } else {
            result.push(serde_json::Value::Number(v.into()));
        }
    }
    Ok(Some(result))
}

fn decode_raw_json(payload: &[u8]) -> Result<Vec<serde_json::Value>> {
    let raw = zstd::decode_all(payload)
        .map_err(|e| StorageError::Other(anyhow::anyhow!("raw zstd decode: {e}")))?;
    serde_json::from_slice(&raw)
        .map_err(|e| StorageError::Other(anyhow::anyhow!("raw json decode: {e}")))
}

fn decode_lz4_json(payload: &[u8]) -> Result<Vec<serde_json::Value>> {
    let raw = lz4_flex::decompress_size_prepended(payload)
        .map_err(|e| StorageError::Other(anyhow::anyhow!("lz4 decode: {e}")))?;
    serde_json::from_slice(&raw)
        .map_err(|e| StorageError::Other(anyhow::anyhow!("lz4 json decode: {e}")))
}

// ── Bit-pack helpers ──────────────────────────────────────────────────────

fn bitpack_u32(ids: &[u32], bit_width: u8) -> Vec<u8> {
    if bit_width == 0 {
        return Vec::new();
    }
    let bw = bit_width as usize;
    let total_bits = ids.len() * bw;
    let total_bytes = total_bits.div_ceil(8);
    let mut out = vec![0u8; total_bytes];
    let mut bit_pos = 0usize;
    for &id in ids {
        let val = id as u64 & ((1u64 << bw) - 1);
        let byte_ix = bit_pos / 8;
        let shift = bit_pos % 8;
        let combined = val << shift;
        // Write up to 64 bits starting at byte_ix.
        let n_bytes = (bw + shift).div_ceil(8).min(total_bytes - byte_ix);
        for i in 0..n_bytes {
            out[byte_ix + i] |= ((combined >> (i * 8)) & 0xFF) as u8;
        }
        bit_pos += bw;
    }
    out
}

fn bitunpack_u32(packed: &[u8], bit_width: u8, count: usize) -> Vec<u32> {
    if bit_width == 0 {
        return vec![0; count];
    }
    let bw = bit_width as usize;
    let mask = (1u64 << bw) - 1;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let bit_pos = i * bw;
        let byte_ix = bit_pos / 8;
        let shift = bit_pos % 8;
        let mut combined: u64 = 0;
        let n_bytes = (bw + shift).div_ceil(8).min(packed.len() - byte_ix);
        for j in 0..n_bytes {
            combined |= (packed[byte_ix + j] as u64) << (j * 8);
        }
        let val = (combined >> shift) & mask;
        out.push(val as u32);
    }
    out
}

// ── Varint / zigzag helpers ───────────────────────────────────────────────

fn write_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
}

fn read_varint(data: &[u8], pos: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    let mut shift = 0u32;
    while *pos < data.len() && shift < 64 {
        let b = data[*pos];
        *pos += 1;
        if shift == 63 && b & 0x7e != 0 {
            return Err(StorageError::Other(anyhow::anyhow!("varint overflow")));
        }
        v |= ((b & 0x7F) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
        shift += 7;
    }
    Err(StorageError::Other(anyhow::anyhow!(
        "truncated or overlong varint"
    )))
}

fn write_zigzag_i64(out: &mut Vec<u8>, v: i64) {
    let z = ((v << 1) ^ (v >> 63)) as u64;
    write_varint(out, z);
}

fn read_zigzag_i64(data: &[u8], pos: &mut usize) -> Result<i64> {
    let z = read_varint(data, pos)?;
    Ok(((z >> 1) as i64) ^ -((z & 1) as i64))
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lz4_roundtrip() {
        let raw = br#"[{"_id":"a","_source":{"msg":"hello world"}}]"#;
        let encoded = encode_stored_lz4(raw);
        assert_eq!(&encoded[..4], STORED_LZ4_MAGIC);
        let decoded = decode_stored(&encoded).unwrap();
        assert_eq!(&decoded, raw);
    }

    #[test]
    fn uncompressed_passthrough() {
        let raw = br#"[{"_id":"a"}]"#;
        let decoded = decode_stored(raw).unwrap();
        assert_eq!(&decoded, raw);
    }

    fn projection_fixture() -> (Vec<serde_json::Value>, Vec<u8>) {
        let docs: Vec<_> = (0..256)
            .map(|i| {
                json!({
                    "_id": format!("doc-{i}"),
                    "_seq_no": i,
                    "_source": {
                        "category": if i % 2 == 0 { "even" } else { "odd" },
                        "embedding": [i as f64 / 10.0, 0.25, -0.5],
                        "embedding_chunks": [
                            [i as f64 / 10.0, 0.25, -0.5],
                            [-0.0, 1.25e-10, -2]
                        ],
                        "large_unrequested": "the same payload repeated to compress well"
                    }
                })
            })
            .collect();
        let encoded = encode_stored_v2(&serde_json::to_vec(&docs).unwrap());
        assert_eq!(&encoded[..4], STORED_V2_MAGIC);
        (docs, encoded)
    }

    /// #367: a `_source` carrying an explicit `null` (plus the other "empty"
    /// shapes that are easy to confuse with absence) must come back exactly as
    /// it went in.
    ///
    /// Pre-fix this failed on the first assertion: the columnar path fills a
    /// cell with `NULL` both for an absent field and for a stored `null`, and
    /// the decoder dropped every null — so `{"reason": null}` came back as
    /// `{}` on any segment of ≥ `V2_MIN_DOCS` docs. Below that threshold the
    /// v1 LZ4 fallback keeps the bytes verbatim, which is exactly why the
    /// corruption stayed invisible on small corpora: a 209-file autoindex run
    /// flushes shard segments under 128 docs and round-trips, a 6 000-file one
    /// does not.
    fn explicit_null_fixture(docs: usize) -> Vec<serde_json::Value> {
        (0..docs)
            .map(|i| {
                json!({
                    "_id": format!("file:{i}"),
                    "_seq_no": i,
                    "_source": {
                        "doc_kind": "file",
                        "path": format!("p/{i}.java"),
                        "reason": serde_json::Value::Null,
                        "records": i,
                        "empty_array": [],
                        "empty_object": {},
                        "empty_string": "",
                        "zero": 0,
                        "untrue": false,
                    }
                })
            })
            .collect()
    }

    #[test]
    fn v2_round_trips_explicit_null_and_empty_values() {
        for docs in [V2_MIN_DOCS - 1, V2_MIN_DOCS, 2001] {
            let fixture = explicit_null_fixture(docs);
            let json = serde_json::to_vec(&fixture).unwrap();
            let decoded: Vec<serde_json::Value> =
                serde_json::from_slice(&decode_stored(&encode_stored_v2(&json)).unwrap()).unwrap();
            assert_eq!(decoded, fixture, "{docs}-doc block must round-trip");

            // The flush path encodes from parsed values and must agree.
            let sources: Vec<(&str, u64, &serde_json::Value)> = fixture
                .iter()
                .map(|doc| {
                    (
                        doc["_id"].as_str().unwrap(),
                        doc["_seq_no"].as_u64().unwrap(),
                        &doc["_source"],
                    )
                })
                .collect();
            let from_values = encode_stored_v2_from_values(&json, &sources);
            assert_eq!(
                from_values,
                encode_stored_v2(&json),
                "{docs}-doc from-values encode must stay byte-identical"
            );
            let decoded: Vec<serde_json::Value> =
                serde_json::from_slice(&decode_stored(&from_values).unwrap()).unwrap();
            assert_eq!(decoded, fixture);
            let decoded: Vec<serde_json::Value> = serde_json::from_slice(
                &decode_stored(&encode_stored_v2_from_values_nojson(&sources)).unwrap(),
            )
            .unwrap();
            assert_eq!(decoded, fixture);
        }
    }

    /// A stored `null` and an absent field are different documents, and the
    /// decoder has to keep telling them apart per document — not per column.
    #[test]
    fn v2_distinguishes_a_stored_null_from_an_absent_field() {
        let docs: Vec<_> = (0..V2_MIN_DOCS * 2)
            .map(|i| {
                let mut source = serde_json::Map::new();
                source.insert("always".into(), json!(i));
                match i % 3 {
                    0 => {
                        source.insert("sometimes".into(), serde_json::Value::Null);
                    }
                    1 => {
                        source.insert("sometimes".into(), json!(i));
                    }
                    _ => {}
                }
                json!({"_id": format!("d{i}"), "_seq_no": i, "_source": source})
            })
            .collect();
        let encoded = encode_stored_v2(&serde_json::to_vec(&docs).unwrap());
        assert_eq!(&encoded[..4], STORED_V2_MAGIC);
        let decoded: Vec<serde_json::Value> =
            serde_json::from_slice(&decode_stored(&encoded).unwrap()).unwrap();
        assert_eq!(decoded, docs);
        for (i, doc) in decoded.iter().enumerate() {
            let source = doc["_source"].as_object().unwrap();
            match i % 3 {
                0 => assert!(
                    source
                        .get("sometimes")
                        .is_some_and(serde_json::Value::is_null),
                    "doc {i} stored null and must read back as null: {source:?}"
                ),
                1 => assert_eq!(source["sometimes"], json!(i)),
                _ => assert!(
                    !source.contains_key("sometimes"),
                    "doc {i} never had the field and must not gain one: {source:?}"
                ),
            }
        }
    }

    /// A block with no explicit nulls must not grow a column, so segments that
    /// were byte-identical before this fix still are.
    #[test]
    fn v2_adds_no_column_when_no_document_stores_a_null() {
        let (_, encoded) = projection_fixture();
        let directory = parse_v2_directory(&encoded[4..]).unwrap();
        assert!(
            !directory
                .columns
                .iter()
                .any(|column| column.name == V2_EXPLICIT_NULLS_COLUMN),
            "null-free blocks must carry no explicit-null column"
        );
    }

    fn handcrafted_v2(num_docs: u32, columns: &[(&str, u8, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(STORED_V2_MAGIC);
        out.write_u32::<LittleEndian>(num_docs).unwrap();
        out.write_u32::<LittleEndian>(columns.len() as u32).unwrap();
        for (name, codec, payload) in columns {
            out.write_u16::<LittleEndian>(name.len() as u16).unwrap();
            out.extend_from_slice(name.as_bytes());
            out.push(*codec);
            out.write_u32::<LittleEndian>(payload.len() as u32).unwrap();
            out.extend_from_slice(payload);
        }
        out
    }

    fn raw_cross_dep(source_index: u32) -> Vec<u8> {
        let mut payload = vec![0];
        payload.write_u32::<LittleEndian>(source_index).unwrap();
        payload
    }

    fn valid_cross_dep(source_index: u32, mode: i64) -> Vec<u8> {
        let mut payload = raw_cross_dep(source_index);
        payload.write_u32::<LittleEndian>(1).unwrap();
        payload.write_i64::<LittleEndian>(mode).unwrap();
        payload.write_u32::<LittleEndian>(0).unwrap();
        payload
    }

    fn lz4_json(value: &serde_json::Value) -> Vec<u8> {
        lz4_flex::compress_prepend_size(&serde_json::to_vec(value).unwrap())
    }

    fn dict_bitpack_with_content_sizes(entries: &[serde_json::Value], ids: &[u32]) -> Vec<u8> {
        let bit_width = if entries.is_empty() {
            1
        } else {
            32 - (entries.len() as u32).leading_zeros() as u8
        };
        let dict =
            zstd::bulk::compress(&serde_json::to_vec(entries).unwrap(), STORED_ZSTD_LEVEL).unwrap();
        let packed = zstd::bulk::compress(&bitpack_u32(ids, bit_width), STORED_ZSTD_LEVEL).unwrap();
        let mut out = Vec::new();
        out.write_u32::<LittleEndian>(entries.len() as u32).unwrap();
        out.push(bit_width);
        out.write_u32::<LittleEndian>(dict.len() as u32).unwrap();
        out.extend_from_slice(&dict);
        out.write_u32::<LittleEndian>(ids.len() as u32).unwrap();
        out.write_u32::<LittleEndian>(packed.len() as u32).unwrap();
        out.extend_from_slice(&packed);
        out
    }

    fn typed_fixture_with_columns(
        num_docs: usize,
        vector_codec: ColCodec,
        vector_payload: Vec<u8>,
        chunks: Option<(ColCodec, Vec<u8>)>,
    ) -> Vec<u8> {
        let ids = json!((0..num_docs).map(|i| format!("id-{i}")).collect::<Vec<_>>());
        let seq = json!((0..num_docs).collect::<Vec<_>>());
        let mut columns = vec![
            ("__id", ColCodec::Lz4Json as u8, lz4_json(&ids)),
            ("__seq_no", ColCodec::Lz4Json as u8, lz4_json(&seq)),
            ("embedding", vector_codec as u8, vector_payload),
        ];
        if let Some((codec, payload)) = chunks {
            columns.push(("embedding_chunks", codec as u8, payload));
        }
        handcrafted_v2(num_docs as u32, &columns)
    }

    #[test]
    fn retained_upper_bound_covers_real_decode_and_all_v2_codecs() {
        let num_docs = 4usize;
        let ids = json!(["a", "b", "a", "b"]);
        let raw_values = json!([
            {"nested": [1, 2]},
            null,
            {"nested": [3]},
            {"text": "longer"}
        ]);
        let raw_payload =
            zstd::bulk::compress(&serde_json::to_vec(&raw_values).unwrap(), STORED_ZSTD_LEVEL)
                .unwrap();
        let dict_payload =
            dict_bitpack_with_content_sizes(&[json!("east"), json!("west")], &[0, 1, 0, 1]);
        let encoded = handcrafted_v2(
            num_docs as u32,
            &[
                ("__id", ColCodec::Lz4Json as u8, lz4_json(&ids)),
                (
                    "__seq_no",
                    ColCodec::Constant as u8,
                    serde_json::to_vec(&json!(7)).unwrap(),
                ),
                ("raw", ColCodec::RawJson as u8, raw_payload),
                ("region", ColCodec::DictBitpack as u8, dict_payload),
                (
                    "amount",
                    ColCodec::CrossDep as u8,
                    valid_cross_dep(3, i64::MIN),
                ),
            ],
        );
        let decoded = decode_stored(&encoded).unwrap();
        let bound = stored_slices_retained_upper_bound(&encoded, num_docs as u64)
            .unwrap()
            .unwrap();
        let actual_retained = decoded.len() as u64 + num_docs as u64 * 8 + 64;
        assert!(
            bound >= actual_retained,
            "bound {bound} < actual retained {actual_retained}"
        );

        let legacy = encode_stored_lz4(&decoded);
        assert!(
            stored_slices_retained_upper_bound(&legacy, num_docs as u64)
                .unwrap()
                .unwrap()
                >= actual_retained
        );
        assert!(stored_slices_retained_upper_bound(&encoded, 5).is_err());
    }

    #[test]
    fn retained_upper_bound_fails_closed_for_unknown_zstd_content_size() {
        let values = serde_json::to_vec(&json!(["a", "b"])).unwrap();
        let mut encoder = zstd::stream::Encoder::new(Vec::new(), STORED_ZSTD_LEVEL).unwrap();
        encoder.include_contentsize(false).unwrap();
        encoder.write_all(&values).unwrap();
        let payload = encoder.finish().unwrap();
        assert_eq!(
            zstd::zstd_safe::get_frame_content_size(&payload).unwrap(),
            None
        );
        let encoded = handcrafted_v2(
            2,
            &[
                ("__id", ColCodec::RawJson as u8, payload),
                (
                    "__seq_no",
                    ColCodec::Constant as u8,
                    serde_json::to_vec(&json!(1)).unwrap(),
                ),
            ],
        );
        assert_eq!(
            stored_slices_retained_upper_bound(&encoded, 2).unwrap(),
            None
        );

        let first = zstd::bulk::compress(&values, STORED_ZSTD_LEVEL).unwrap();
        let second = zstd::bulk::compress(b"[]", STORED_ZSTD_LEVEL).unwrap();
        let concatenated = handcrafted_v2(
            2,
            &[
                ("__id", ColCodec::RawJson as u8, [first, second].concat()),
                (
                    "__seq_no",
                    ColCodec::Constant as u8,
                    serde_json::to_vec(&json!(1)).unwrap(),
                ),
            ],
        );
        assert!(stored_slices_retained_upper_bound(&concatenated, 2).is_err());
    }

    #[test]
    fn v2_projection_decodes_only_requested_columns_with_full_decode_parity() {
        let (docs, encoded) = projection_fixture();
        let StoredV2ProjectionResult::Projected(projected) =
            decode_stored_v2_projection(&encoded, &["__id", "embedding"]).unwrap()
        else {
            panic!("fixture should support projection");
        };
        assert_eq!(projected.num_docs, docs.len());
        assert_eq!(projected.columns.len(), 2);
        assert!(!projected.columns.contains_key("large_unrequested"));
        for (row, expected) in docs.iter().enumerate() {
            assert_eq!(projected.columns["__id"][row], expected["_id"]);
            assert_eq!(
                projected.columns["embedding"][row],
                expected["_source"]["embedding"]
            );
        }
        let StoredV2ProjectionResult::Projected(seq_projection) =
            decode_stored_v2_projection(&encoded, &["__seq_no"]).unwrap()
        else {
            panic!("CROSS_DEP sequence column should resolve");
        };
        for (row, expected) in docs.iter().enumerate() {
            assert_eq!(seq_projection.columns["__seq_no"][row], expected["_seq_no"]);
        }
    }

    #[test]
    fn v2_projection_does_not_decode_unrequested_payload() {
        let (_, mut encoded) = projection_fixture();
        let (payload_offset, payload_len) = {
            let directory = parse_v2_directory(&encoded[4..]).unwrap();
            let column = directory
                .columns
                .iter()
                .find(|column| column.name == "large_unrequested")
                .unwrap();
            (
                column.payload.as_ptr() as usize - encoded.as_ptr() as usize,
                column.payload.len(),
            )
        };
        encoded[payload_offset + payload_len / 2] ^= 0xff;

        let StoredV2ProjectionResult::Projected(projected) =
            decode_stored_v2_projection(&encoded, &["__id"]).unwrap()
        else {
            panic!("an unrelated corrupt payload must not be decoded");
        };
        assert_eq!(projected.columns["__id"].len(), 256);
        assert!(
            decode_stored(&encoded).is_err(),
            "the full decoder must still observe corrupt unrequested payloads"
        );
    }

    #[test]
    fn v2_knn_projection_resolves_identity_and_typed_vectors() {
        let (docs, encoded) = projection_fixture();
        let StoredV2KnnProjectionResult::Projected(projected) =
            decode_stored_v2_knn_projection(&encoded, "embedding", Some("embedding_chunks"))
                .unwrap()
        else {
            panic!("fixture should support typed projection");
        };
        assert_eq!(projected.num_docs, 256);
        assert_eq!(projected.ids[42].as_deref(), Some("doc-42"));
        assert_eq!(projected.seq_nos[42], Some(42));
        for row in [0, 1, 42, 255] {
            let expected = docs[row]["_source"]["embedding"].as_array().unwrap();
            let actual = projected.vectors[row].as_ref().unwrap();
            for (actual, expected) in actual.iter().zip(expected) {
                assert_eq!(
                    actual.to_bits(),
                    (expected.as_f64().unwrap() as f32).to_bits()
                );
            }
        }
        let chunks = projected.vector_chunks.as_ref().unwrap()[7]
            .as_ref()
            .unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[1][0].to_bits(), (-0.0f32).to_bits());
        assert_eq!(chunks[1][1].to_bits(), (1.25e-10f32).to_bits());
        assert_eq!(chunks[1][2], -2.0);
    }

    #[test]
    fn controlled_knn_projection_cancels_without_changing_compatibility_outcomes() {
        let (_, encoded) = projection_fixture();
        let mut observed = Vec::new();
        let run = decode_stored_v2_knn_projection_controlled(
            &encoded,
            "embedding",
            Some("embedding_chunks"),
            |point| {
                observed.push(point);
                if point.phase == StoredDecodePhase::Rows && point.rows_processed == 128 {
                    std::ops::ControlFlow::Break(())
                } else {
                    std::ops::ControlFlow::Continue(())
                }
            },
        )
        .unwrap();
        assert_eq!(run, StoredDecodeRun::Cancelled);
        assert!(observed.iter().any(|point| {
            point.phase == StoredDecodePhase::BeforeColumn && point.rows_processed == 0
        }));

        let controlled = decode_stored_v2_knn_projection_controlled(
            &encoded,
            "embedding",
            Some("embedding_chunks"),
            |_| std::ops::ControlFlow::Continue(()),
        )
        .unwrap();
        assert_eq!(
            controlled,
            StoredDecodeRun::Complete(
                decode_stored_v2_knn_projection(&encoded, "embedding", Some("embedding_chunks"))
                    .unwrap()
            )
        );

        assert_eq!(
            decode_stored_v2_knn_projection_controlled(
                &encode_stored_lz4(b"[]"),
                "embedding",
                None,
                |_| std::ops::ControlFlow::Continue(())
            )
            .unwrap(),
            StoredDecodeRun::Complete(StoredV2KnnProjectionResult::NotV2)
        );
    }

    #[test]
    fn controlled_knn_projection_stops_before_bad_row_129() {
        let rows_with_bad_129 = |chunks: bool| {
            let values: Vec<_> = (0..256)
                .map(|row| {
                    if row == 128 {
                        json!("row-129-must-not-be-consumed")
                    } else if chunks {
                        json!([[1.0, 0.0]])
                    } else {
                        json!([1.0, 0.0])
                    }
                })
                .collect();
            zstd::encode_all(
                Cursor::new(serde_json::to_vec(&values).unwrap()),
                STORED_ZSTD_LEVEL,
            )
            .unwrap()
        };
        let constant_id = serde_json::to_vec(&json!("id")).unwrap();
        let constant_seq = serde_json::to_vec(&json!(1)).unwrap();

        for bad_column in ["embedding", "embedding_chunks"] {
            let embedding = if bad_column == "embedding" {
                rows_with_bad_129(false)
            } else {
                let values = vec![json!([1.0, 0.0]); 256];
                zstd::encode_all(
                    Cursor::new(serde_json::to_vec(&values).unwrap()),
                    STORED_ZSTD_LEVEL,
                )
                .unwrap()
            };
            let chunks = if bad_column == "embedding_chunks" {
                rows_with_bad_129(true)
            } else {
                let values = vec![json!([[1.0, 0.0]]); 256];
                zstd::encode_all(
                    Cursor::new(serde_json::to_vec(&values).unwrap()),
                    STORED_ZSTD_LEVEL,
                )
                .unwrap()
            };
            let encoded = handcrafted_v2(
                256,
                &[
                    ("__id", ColCodec::Constant as u8, constant_id.clone()),
                    ("__seq_no", ColCodec::Constant as u8, constant_seq.clone()),
                    ("embedding", ColCodec::RawJson as u8, embedding),
                    ("embedding_chunks", ColCodec::RawJson as u8, chunks),
                ],
            );
            let cancel_column = if bad_column == "embedding" { 2 } else { 3 };
            assert_eq!(
                decode_stored_v2_knn_projection_controlled(
                    &encoded,
                    "embedding",
                    Some("embedding_chunks"),
                    |point| {
                        if point.column_index == cancel_column
                            && point.phase == StoredDecodePhase::Rows
                            && point.rows_processed == 128
                        {
                            std::ops::ControlFlow::Break(())
                        } else {
                            std::ops::ControlFlow::Continue(())
                        }
                    }
                )
                .unwrap(),
                StoredDecodeRun::Cancelled,
                "{bad_column} must cancel before consuming row 129"
            );
            assert_eq!(
                decode_stored_v2_knn_projection_controlled(
                    &encoded,
                    "embedding",
                    Some("embedding_chunks"),
                    |_| std::ops::ControlFlow::Continue(())
                )
                .unwrap(),
                StoredDecodeRun::Complete(StoredV2KnnProjectionResult::UnsupportedVectorShape),
                "without cancellation {bad_column} must consume bad row 129"
            );
        }
    }

    #[test]
    fn controlled_vector_decode_checkpoints_after_invalid_row_for_raw_and_lz4() {
        let values: Vec<_> = (0..256)
            .map(|row| {
                if row == 0 {
                    json!([1.0, "invalid"])
                } else {
                    json!([1.0, 0.0])
                }
            })
            .collect();
        let json = serde_json::to_vec(&values).unwrap();
        let payloads = [
            (
                ColCodec::RawJson,
                zstd::encode_all(Cursor::new(&json), STORED_ZSTD_LEVEL).unwrap(),
            ),
            (ColCodec::Lz4Json, lz4_flex::compress_prepend_size(&json)),
        ];
        for (codec, embedding) in payloads {
            let encoded = handcrafted_v2(
                256,
                &[
                    (
                        "__id",
                        ColCodec::Constant as u8,
                        serde_json::to_vec(&json!("id")).unwrap(),
                    ),
                    (
                        "__seq_no",
                        ColCodec::Constant as u8,
                        serde_json::to_vec(&json!(1)).unwrap(),
                    ),
                    ("embedding", codec as u8, embedding),
                ],
            );
            assert_eq!(
                decode_stored_v2_knn_projection_controlled(&encoded, "embedding", None, |point| {
                    if point.column_index == 2
                        && point.phase == StoredDecodePhase::Rows
                        && point.rows_processed == 128
                    {
                        std::ops::ControlFlow::Break(())
                    } else {
                        std::ops::ControlFlow::Continue(())
                    }
                })
                .unwrap(),
                StoredDecodeRun::Cancelled,
                "{codec:?} must checkpoint even after an invalid first row"
            );
        }
    }

    #[test]
    fn controlled_constant_vector_and_chunk_decode_cancel_at_row_checkpoint() {
        for cancel_column in [2, 3] {
            let non_constant_embedding = zstd::encode_all(
                Cursor::new(serde_json::to_vec(&vec![json!([1.0, 0.0]); 256]).unwrap()),
                STORED_ZSTD_LEVEL,
            )
            .unwrap();
            let encoded = handcrafted_v2(
                256,
                &[
                    (
                        "__id",
                        ColCodec::Constant as u8,
                        serde_json::to_vec(&json!("id")).unwrap(),
                    ),
                    (
                        "__seq_no",
                        ColCodec::Constant as u8,
                        serde_json::to_vec(&json!(1)).unwrap(),
                    ),
                    (
                        "embedding",
                        if cancel_column == 2 {
                            ColCodec::Constant as u8
                        } else {
                            ColCodec::RawJson as u8
                        },
                        if cancel_column == 2 {
                            serde_json::to_vec(&json!([1.0, 0.0])).unwrap()
                        } else {
                            non_constant_embedding
                        },
                    ),
                    (
                        "embedding_chunks",
                        ColCodec::Constant as u8,
                        serde_json::to_vec(&json!([[1.0, 0.0], [0.0, 1.0]])).unwrap(),
                    ),
                ],
            );
            assert_eq!(
                decode_stored_v2_knn_projection_controlled(
                    &encoded,
                    "embedding",
                    Some("embedding_chunks"),
                    |point| {
                        if point.column_index == cancel_column
                            && point.phase == StoredDecodePhase::Rows
                            && point.rows_processed == 128
                        {
                            std::ops::ControlFlow::Break(())
                        } else {
                            std::ops::ControlFlow::Continue(())
                        }
                    }
                )
                .unwrap(),
                StoredDecodeRun::Cancelled
            );
        }
    }

    #[test]
    fn v2_directory_rejects_impossible_column_count_before_allocation() {
        let mut body = Vec::new();
        body.extend_from_slice(&0_u32.to_le_bytes());
        body.extend_from_slice(&u32::MAX.to_le_bytes());
        let error = match parse_v2_directory(&body) {
            Ok(_) => panic!("impossible column count must be rejected"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("exceeds the remaining directory framing"));
    }

    #[test]
    fn v2_knn_projection_has_explicit_legacy_missing_and_shape_outcomes() {
        let legacy = encode_stored_lz4(br#"[{"_id":"legacy"}]"#);
        assert_eq!(
            decode_stored_v2_knn_projection(&legacy, "embedding", None).unwrap(),
            StoredV2KnnProjectionResult::NotV2
        );

        let (_, encoded) = projection_fixture();
        assert_eq!(
            decode_stored_v2_knn_projection(&encoded, "missing", None).unwrap(),
            StoredV2KnnProjectionResult::MissingVectorColumn
        );

        let malformed_docs: Vec<_> = (0..256)
            .map(|i| {
                json!({
                    "_id": format!("bad-{i}"),
                    "_seq_no": i,
                    "_source": {
                        "embedding": if i == 128 {
                            json!([1.0, "not-a-number", 3.0])
                        } else {
                            json!([1.0, 2.0, 3.0])
                        }
                    }
                })
            })
            .collect();
        let malformed = encode_stored_v2(&serde_json::to_vec(&malformed_docs).unwrap());
        assert_eq!(&malformed[..4], STORED_V2_MAGIC);
        assert_eq!(
            decode_stored_v2_knn_projection(&malformed, "embedding", None).unwrap(),
            StoredV2KnnProjectionResult::UnsupportedVectorShape
        );
    }

    #[test]
    fn v2_projection_rejects_cross_dep_cycles_and_bad_source_indices() {
        let cycle = handcrafted_v2(
            1,
            &[
                ("a", ColCodec::CrossDep as u8, raw_cross_dep(1)),
                ("b", ColCodec::CrossDep as u8, raw_cross_dep(0)),
            ],
        );
        let error = decode_stored_v2_projection(&cycle, &["a"]).unwrap_err();
        assert!(error.to_string().contains("dependency cycle"), "{error}");

        let out_of_range = handcrafted_v2(1, &[("a", ColCodec::CrossDep as u8, raw_cross_dep(9))]);
        let error = decode_stored_v2_projection(&out_of_range, &["a"]).unwrap_err();
        assert!(error.to_string().contains("out of range"), "{error}");

        let bad_flag = handcrafted_v2(1, &[("a", ColCodec::CrossDep as u8, vec![7, 0, 0, 0, 0])]);
        let error = decode_stored_v2_projection(&bad_flag, &["a"]).unwrap_err();
        assert!(
            error.to_string().contains("invalid compression flag"),
            "{error}"
        );
        assert!(
            decode_stored(&bad_flag).is_err(),
            "full and selective decoders must reject the same bad flag"
        );
    }

    #[test]
    fn v2_projection_resolves_forward_multihop_and_shared_dependencies() {
        let id_payload = serde_json::to_vec(&vec!["doc"; 4]).unwrap();
        let seq_payload = serde_json::to_vec(&vec![0, 1, 2, 3]).unwrap();
        let encoded = handcrafted_v2(
            4,
            &[
                (
                    "__id",
                    ColCodec::Lz4Json as u8,
                    lz4_flex::compress_prepend_size(&id_payload),
                ),
                (
                    "__seq_no",
                    ColCodec::Lz4Json as u8,
                    lz4_flex::compress_prepend_size(&seq_payload),
                ),
                ("a", ColCodec::CrossDep as u8, valid_cross_dep(3, 10)),
                ("b", ColCodec::CrossDep as u8, valid_cross_dep(4, 5)),
                ("base", ColCodec::Constant as u8, br#""key""#.to_vec()),
                ("d", ColCodec::CrossDep as u8, valid_cross_dep(4, 20)),
            ],
        );
        let StoredV2ProjectionResult::Projected(projected) =
            decode_stored_v2_projection(&encoded, &["a", "d"]).unwrap()
        else {
            panic!("valid recursive dependencies should project");
        };
        assert_eq!(
            projected
                .columns
                .keys()
                .cloned()
                .collect::<std::collections::HashSet<_>>(),
            ["a".to_string(), "d".to_string()].into_iter().collect()
        );
        assert_eq!(projected.columns["a"], vec![json!(10); 4]);
        assert_eq!(projected.columns["d"], vec![json!(20); 4]);

        let full: Vec<serde_json::Value> =
            serde_json::from_slice(&decode_stored(&encoded).unwrap()).unwrap();
        for row in full {
            assert_eq!(row["_source"]["a"], 10);
            assert_eq!(row["_source"]["d"], 20);
        }
    }

    #[test]
    fn cross_dep_checked_exceptions_reject_truncation_overflow_and_trailing_bytes() {
        let dependencies = vec![Vec::new(), vec![json!("key"); 4]];
        let make = |exception_bytes: &[u8]| {
            let mut payload = valid_cross_dep(1, 7);
            let len = payload.len();
            payload[len - 4..].copy_from_slice(&1u32.to_le_bytes());
            payload.extend_from_slice(exception_bytes);
            payload
        };

        let truncated = make(&[0x80]);
        assert!(decode_cross_dep(&truncated, 4, &dependencies, &HashMap::new()).is_err());

        let overlong = make(&[0x80; 11]);
        assert!(decode_cross_dep(&overlong, 4, &dependencies, &HashMap::new()).is_err());

        let out_of_range = make(&[4, 0]);
        assert!(decode_cross_dep(&out_of_range, 4, &dependencies, &HashMap::new()).is_err());

        let mut trailing = valid_cross_dep(1, 7);
        trailing.push(0);
        assert!(decode_cross_dep(&trailing, 4, &dependencies, &HashMap::new()).is_err());
    }

    #[test]
    fn malformed_cardinality_and_cross_dep_expansion_are_bounded() {
        let oversized_dict = ((DICT_MAX_CARDINALITY + 1) as u32).to_le_bytes().to_vec();
        assert!(decode_dict_bitpack(&oversized_dict, 1).is_err());

        let dependencies = vec![vec![json!("key")]];
        let mut oversized_cross = vec![0];
        oversized_cross.extend_from_slice(&0u32.to_le_bytes());
        oversized_cross.extend_from_slice(&((DICT_MAX_CARDINALITY + 1) as u32).to_le_bytes());
        assert!(decode_cross_dep(&oversized_cross, 1, &dependencies, &HashMap::new()).is_err());

        let mut too_many_exceptions = valid_cross_dep(0, 7);
        let count_offset = 1 + 4 + 4 + 8;
        too_many_exceptions[count_offset..count_offset + 4].copy_from_slice(&2u32.to_le_bytes());
        assert!(decode_cross_dep(&too_many_exceptions, 1, &dependencies, &HashMap::new()).is_err());

        let decoded_limit = 12 + 23;
        let compressed =
            zstd::bulk::compress(&vec![0; decoded_limit + 1], STORED_ZSTD_LEVEL).unwrap();
        let mut bomb = vec![1];
        bomb.extend_from_slice(&compressed);
        assert!(decode_cross_dep(&bomb, 1, &dependencies, &HashMap::new()).is_err());
    }

    #[test]
    fn v2_knn_projection_preserves_384d_f32_bits_and_rejects_overflow() {
        let docs: Vec<_> = (0..256)
            .map(|row| {
                let vector: Vec<f64> = (0..384)
                    .map(|dim| (row as f64 - dim as f64) / 997.0)
                    .collect();
                json!({
                    "_id": format!("wide-{row}"),
                    "_seq_no": row,
                    "_source": {"embedding": vector}
                })
            })
            .collect();
        let encoded = encode_stored_v2(&serde_json::to_vec(&docs).unwrap());
        assert_eq!(&encoded[..4], STORED_V2_MAGIC);
        let StoredV2KnnProjectionResult::Projected(projected) =
            decode_stored_v2_knn_projection(&encoded, "embedding", None).unwrap()
        else {
            panic!("384d vectors should project");
        };
        for row in [0, 127, 255] {
            let actual = projected.vectors[row].as_ref().unwrap();
            assert_eq!(actual.len(), 384);
            for dim in [0, 1, 191, 383] {
                let expected = docs[row]["_source"]["embedding"][dim].as_f64().unwrap() as f32;
                assert_eq!(actual[dim].to_bits(), expected.to_bits());
            }
        }

        let overflow_docs: Vec<_> = (0..256)
            .map(|row| {
                json!({
                    "_id": format!("overflow-{row}"),
                    "_seq_no": row,
                    "_source": {"embedding": [1e39, 0.0]}
                })
            })
            .collect();
        let overflow = encode_stored_v2(&serde_json::to_vec(&overflow_docs).unwrap());
        assert_eq!(
            decode_stored_v2_knn_projection(&overflow, "embedding", None).unwrap(),
            StoredV2KnnProjectionResult::UnsupportedVectorShape
        );
    }

    #[test]
    fn v2_knn_projection_forced_lz4_nulls_and_optional_missing_chunks() {
        let vectors = json!([[1.0, -0.0], null, [], [3, 4]]);
        let chunks = json!([[[1.0, 2.0]], null, [], [[3.0], [4.0, 5.0]]]);
        let encoded = typed_fixture_with_columns(
            4,
            ColCodec::Lz4Json,
            lz4_json(&vectors),
            Some((ColCodec::Lz4Json, lz4_json(&chunks))),
        );
        let StoredV2KnnProjectionResult::Projected(projected) =
            decode_stored_v2_knn_projection(&encoded, "embedding", Some("embedding_chunks"))
                .unwrap()
        else {
            panic!("forced LZ4 vectors should project");
        };
        assert_eq!(
            projected.vectors[0].as_ref().unwrap()[1].to_bits(),
            (-0.0f32).to_bits()
        );
        assert_eq!(projected.vectors[1], None);
        assert_eq!(projected.vectors[2], Some(Vec::new()));
        assert_eq!(projected.vector_chunks.as_ref().unwrap()[1], None);
        assert_eq!(
            projected.vector_chunks.as_ref().unwrap()[3],
            Some(vec![vec![3.0], vec![4.0, 5.0]])
        );

        let StoredV2KnnProjectionResult::Projected(missing_chunks) =
            decode_stored_v2_knn_projection(&encoded, "embedding", Some("different_chunks"))
                .unwrap()
        else {
            panic!("missing optional chunks must not block pooled vectors");
        };
        assert_eq!(missing_chunks.vector_chunks, None);
    }

    #[test]
    fn v2_knn_projection_constant_vectors_and_identity_cell_semantics() {
        let encoded = typed_fixture_with_columns(
            4,
            ColCodec::Constant,
            br#"[1.5,-0.0]"#.to_vec(),
            Some((ColCodec::Constant, b"null".to_vec())),
        );
        let StoredV2KnnProjectionResult::Projected(projected) =
            decode_stored_v2_knn_projection(&encoded, "embedding", Some("embedding_chunks"))
                .unwrap()
        else {
            panic!("constant vectors should project");
        };
        assert_eq!(projected.vectors, vec![Some(vec![1.5, -0.0]); 4]);
        assert_eq!(projected.vector_chunks, Some(vec![None; 4]));

        let wrong_identity = handcrafted_v2(
            2,
            &[
                ("__id", ColCodec::Lz4Json as u8, lz4_json(&json!(["ok", 7]))),
                (
                    "__seq_no",
                    ColCodec::Lz4Json as u8,
                    lz4_json(&json!([0, -1])),
                ),
                (
                    "embedding",
                    ColCodec::Lz4Json as u8,
                    lz4_json(&json!([[1.0], [2.0]])),
                ),
            ],
        );
        let StoredV2KnnProjectionResult::Projected(projected) =
            decode_stored_v2_knn_projection(&wrong_identity, "embedding", None).unwrap()
        else {
            panic!("wrong identity cells are explicit None rows");
        };
        assert_eq!(projected.ids, vec![Some("ok".into()), None]);
        assert_eq!(projected.seq_nos, vec![Some(0), None]);
    }

    #[test]
    fn v2_knn_projection_rejects_reserved_or_colliding_role_names() {
        let (_, encoded) = projection_fixture();
        for (vector, chunks) in [
            ("__id", None),
            ("__seq_no", None),
            ("embedding", Some("__id")),
            ("embedding", Some("__seq_no")),
            ("embedding", Some("embedding")),
        ] {
            assert_eq!(
                decode_stored_v2_knn_projection(&encoded, vector, chunks).unwrap(),
                StoredV2KnnProjectionResult::UnsupportedVectorShape
            );
        }
    }

    #[test]
    fn v2_projection_enforces_dependency_depth_boundary() {
        let chain = |edges: usize| {
            let mut owned: Vec<(String, u8, Vec<u8>)> = (0..edges)
                .map(|index| {
                    (
                        format!("c{index}"),
                        ColCodec::CrossDep as u8,
                        valid_cross_dep((index + 1) as u32, index as i64),
                    )
                })
                .collect();
            owned.push((
                format!("c{edges}"),
                ColCodec::Constant as u8,
                br#""base""#.to_vec(),
            ));
            let borrowed: Vec<_> = owned
                .iter()
                .map(|(name, codec, payload)| (name.as_str(), *codec, payload.clone()))
                .collect();
            handcrafted_v2(1, &borrowed)
        };

        let at_limit = chain(256);
        assert!(
            decode_stored_v2_projection(&at_limit, &["c0"]).is_ok(),
            "256 dependency edges are supported"
        );
        let over_limit = chain(257);
        let error = decode_stored_v2_projection(&over_limit, &["c0"]).unwrap_err();
        assert!(error.to_string().contains("depth exceeds 256"), "{error}");
    }

    #[test]
    fn v2_projection_validates_framing_and_preserves_legacy_fallback() {
        let (_, encoded) = projection_fixture();
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(decode_stored_v2_projection(&trailing, &["__id"]).is_err());

        let legacy = encode_stored_lz4(br#"[{"_id":"legacy"}]"#);
        assert_eq!(
            decode_stored_v2_projection(&legacy, &["__id"]).unwrap(),
            StoredV2ProjectionResult::NotV2
        );
    }

    #[test]
    fn v2_roundtrip_nginx_like() {
        // Synthesise 256 nginx-like docs with strong determinism: URL → status, URL → bytes.
        let mut docs = Vec::new();
        let urls = ["/a.png", "/b.png", "/c.js", "/d.html", "/api/x"];
        let statuses = [200, 200, 200, 404, 500];
        let sizes = [1024, 2048, 4096, 0, 12345];
        for i in 0..256 {
            let u = i % urls.len();
            docs.push(json!({
                "_id": format!("doc-{}", i),
                "_seq_no": i as u64,
                "_source": {
                    "path": urls[u],
                    "status": statuses[u],
                    "bytes": sizes[u],
                    "method": if i % 7 == 0 { "POST" } else { "GET" },
                    "ip": format!("10.0.{}.{}", i / 256, i % 256),
                    "ua": "Mozilla/5.0 Chrome/120.0",
                    "@timestamp": 1_700_000_000u64 + i as u64,
                }
            }));
        }
        let raw = serde_json::to_vec(&docs).unwrap();
        let encoded = encode_stored_v2(&raw);
        // V2 should have meaningfully reduced the payload.
        assert!(
            encoded.len() < raw.len() / 2,
            "v2 encoded {} not < half of raw {}",
            encoded.len(),
            raw.len()
        );

        let decoded = decode_stored(&encoded).unwrap();
        let round: Vec<serde_json::Value> = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(round.len(), docs.len());
        // Spot-check a few docs.
        for i in [0, 1, 5, 42, 128, 255] {
            assert_eq!(round[i]["_id"], docs[i]["_id"], "id mismatch at {i}");
            assert_eq!(
                round[i]["_source"]["status"], docs[i]["_source"]["status"],
                "status mismatch at {i}"
            );
            assert_eq!(
                round[i]["_source"]["path"], docs[i]["_source"]["path"],
                "path mismatch at {i}"
            );
            assert_eq!(
                round[i]["_source"]["bytes"], docs[i]["_source"]["bytes"],
                "bytes mismatch at {i}"
            );
        }
    }

    #[test]
    fn v2_falls_back_on_tiny_input() {
        let raw = br#"[{"_id":"a","_seq_no":1,"_source":{"m":"x"}}]"#;
        let encoded = encode_stored_v2(raw);
        // Small input: should fall back to v1 LZ4 magic.
        assert_eq!(&encoded[..4], STORED_LZ4_MAGIC);
    }

    #[test]
    fn v2_preserves_float_column_at_scale() {
        // Regression for the silent-data-loss defect: a float column that is
        // strongly determined by a low-cardinality keyword column used to be
        // routed through the i64 cross-dep optimization, which truncated
        // every `f64` to its integer part (0.010127 -> 0) once the segment
        // was large enough for the optimization to kick in.  The float column
        // must now round-trip exactly while a sibling integer column stays a
        // genuine integer (cross-dep still allowed for integers).
        let categories = ["a", "b", "c", "d", "e"];
        let costs = [0.0017_f64, 0.019, 0.5, 0.010127, 12.3456];
        let counts = [1_i64, 2, 3, 4, 5];

        let mut docs = Vec::new();
        for i in 0..5000usize {
            let c = i % categories.len();
            docs.push(json!({
                "_id": format!("doc-{}", i),
                "_seq_no": i as u64,
                "_source": {
                    "category": categories[c],
                    "cost_usd": costs[c],
                    "count": counts[c],
                }
            }));
        }
        let raw = serde_json::to_vec(&docs).unwrap();
        let encoded = encode_stored_v2(&raw);
        let decoded = decode_stored(&encoded).unwrap();
        let round: Vec<serde_json::Value> = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(round.len(), docs.len());

        for (i, doc) in round.iter().enumerate() {
            let c = i % categories.len();
            // Exact f64 fidelity — no truncation to integer.
            let got = doc["_source"]["cost_usd"].as_f64().unwrap();
            assert_eq!(got, costs[c], "cost_usd corrupted at row {i}");
            // The integer sibling stays a genuine integer (not promoted to float).
            let cnt = &doc["_source"]["count"];
            assert!(
                cnt.is_i64() || cnt.is_u64(),
                "count became non-integer at row {i}: {cnt}"
            );
            assert_eq!(
                cnt.as_i64().unwrap(),
                counts[c],
                "count value wrong at row {i}"
            );
        }
    }

    #[test]
    fn bitpack_roundtrip() {
        let ids = vec![0u32, 1, 2, 3, 4, 5, 6, 7, 0, 1, 2, 3];
        let packed = bitpack_u32(&ids, 3);
        let unpacked = bitunpack_u32(&packed, 3, ids.len());
        assert_eq!(unpacked, ids);
    }

    /// Serialise (id, seq_no, source) triples with the EXACT byte
    /// construction `IndexStore::finalize_flush_with_publisher` uses for
    /// parsed-source entries, so the parity assertion below covers the
    /// real flush input.
    fn stored_json_like_flush(docs: &[(String, u64, serde_json::Value)]) -> Vec<u8> {
        use std::io::Write;
        let mut stored_bytes: Vec<u8> = Vec::with_capacity(docs.len() * 512);
        stored_bytes.push(b'[');
        let mut first = true;
        for (id, seq, src) in docs {
            if !first {
                stored_bytes.push(b',');
            }
            first = false;
            stored_bytes.extend_from_slice(br#"{"_id":"#);
            serde_json::to_writer(&mut stored_bytes, id).unwrap();
            stored_bytes.extend_from_slice(br#","_seq_no":"#);
            write!(stored_bytes, "{}", seq).unwrap();
            stored_bytes.extend_from_slice(br#","_source":"#);
            serde_json::to_writer(&mut stored_bytes, src).unwrap();
            stored_bytes.push(b'}');
        }
        stored_bytes.push(b']');
        stored_bytes
    }

    /// P2.2 parity gate — `encode_stored_v2_from_values` must be
    /// byte-identical to the legacy parse-based `encode_stored_v2` for
    /// the same flush input, across mixed field types (strings, ints,
    /// floats, dates, bools, explicit nulls, nested objects, arrays)
    /// and ragged shapes (missing fields).
    #[test]
    fn v2_from_values_byte_identical_to_legacy() {
        let mut docs: Vec<(String, u64, serde_json::Value)> = Vec::new();
        for i in 0..2000usize {
            let mut src = json!({
                "name": format!("doc-{}", i % 37),
                "count": i as i64,
                "big": u64::MAX - (i as u64 % 3),
                "neg": -(i as i64) * 7,
                "score": (i as f64) * 0.25 + 0.010127,
                "when": format!("2026-07-{:02}T12:00:{:02}Z", (i % 28) + 1, i % 60),
                "flag": i % 2 == 0,
                "maybe": if i % 5 == 0 { serde_json::Value::Null } else { json!("x") },
                "nested": { "a": i % 3, "b": [1, "two", serde_json::Value::Null, 3.5] },
                "tags": ["alpha", "beta"],
            });
            if i % 7 == 0 {
                src.as_object_mut().unwrap().remove("count");
            }
            if i % 11 == 0 {
                src.as_object_mut()
                    .unwrap()
                    .insert("rare".into(), json!({"deep": {"x": i}}));
            }
            docs.push((format!("id-{}", i), 1_000 + i as u64, src));
        }
        let stored = stored_json_like_flush(&docs);
        let refs: Vec<(&str, u64, &serde_json::Value)> = docs
            .iter()
            .map(|(id, seq, src)| (id.as_str(), *seq, src))
            .collect();

        let legacy = encode_stored_v2(&stored);
        let from_values = encode_stored_v2_from_values(&stored, &refs);
        assert_eq!(
            legacy, from_values,
            "from_values encoder diverged from legacy parse-based encoder"
        );

        // Decode round-trip sanity: every doc materialises with its id/seq.
        let decoded = decode_stored(&from_values).unwrap();
        let round: Vec<serde_json::Value> = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(round.len(), docs.len());
        for i in [0usize, 1, 6, 42, 1024, 1999] {
            assert_eq!(round[i]["_id"].as_str().unwrap(), format!("id-{}", i));
            assert_eq!(round[i]["_seq_no"].as_u64().unwrap(), 1_000 + i as u64);
            assert_eq!(round[i]["_source"]["score"], docs[i].2["score"]);
        }
    }

    /// The single-pass column build discovers columns lazily and must
    /// backfill NULLs for rows consumed before a field first appears —
    /// exercise a column whose first occurrence is deep into the batch,
    /// plus a source field named `__id` (must not clobber the synthetic
    /// id column), and assert byte parity with the legacy two-pass
    /// parse-based encoder.
    #[test]
    fn v2_from_values_late_discovered_column_parity() {
        let mut docs: Vec<(String, u64, serde_json::Value)> = Vec::new();
        for i in 0..1500usize {
            let mut src = json!({
                "kind": format!("k{}", i % 5),
                "n": i as i64,
            });
            if i >= 700 {
                // late-discovered column → backfill path
                src.as_object_mut()
                    .unwrap()
                    .insert("late".into(), json!(i % 9));
            }
            if i % 13 == 0 {
                // reserved-name source field → must be ignored
                src.as_object_mut()
                    .unwrap()
                    .insert("__id".into(), json!("evil"));
            }
            docs.push((format!("id-{}", i), 5_000 + i as u64, src));
        }
        let stored = stored_json_like_flush(&docs);
        let refs: Vec<(&str, u64, &serde_json::Value)> = docs
            .iter()
            .map(|(id, seq, src)| (id.as_str(), *seq, src))
            .collect();

        let legacy = encode_stored_v2(&stored);
        let from_values = encode_stored_v2_from_values(&stored, &refs);
        assert_eq!(
            legacy, from_values,
            "late-column build diverged from legacy"
        );

        // And the no-JSON fast path must produce the same v2 bytes when
        // v2 wins the size net (it does on this shape).
        let nojson = encode_stored_v2_from_values_nojson(&refs);
        assert_eq!(from_values, nojson, "nojson fast path diverged");

        let decoded = decode_stored(&from_values).unwrap();
        let round: Vec<serde_json::Value> = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(round.len(), docs.len());
        assert!(round[0]["_source"]
            .get("late")
            .map(|v| v.is_null())
            .unwrap_or(true));
        assert_eq!(round[0]["_id"].as_str().unwrap(), "id-0");
        assert_eq!(round[750]["_source"]["late"], json!(750 % 9));
    }

    /// Below `V2_MIN_DOCS` both entry points must produce the identical
    /// v1 LZ4 fallback bytes.
    #[test]
    fn v2_from_values_tiny_input_matches_legacy_fallback() {
        let docs: Vec<(String, u64, serde_json::Value)> = (0..10usize)
            .map(|i| (format!("t-{}", i), i as u64, json!({"m": i, "s": "x"})))
            .collect();
        let stored = stored_json_like_flush(&docs);
        let refs: Vec<(&str, u64, &serde_json::Value)> = docs
            .iter()
            .map(|(id, seq, src)| (id.as_str(), *seq, src))
            .collect();
        let legacy = encode_stored_v2(&stored);
        let from_values = encode_stored_v2_from_values(&stored, &refs);
        assert_eq!(legacy, from_values);
        assert_eq!(&from_values[..4], STORED_LZ4_MAGIC);
    }
}
