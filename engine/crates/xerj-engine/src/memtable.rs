//! In-memory inverted index for unflushed documents.
//!
//! The FTS memtable provides fast search over recently ingested documents
//! that have not yet been flushed to a segment on disk.  It uses a simple
//! HashMap-based inverted index with BM25 scoring, plus a columnar DocValues
//! store for O(N) term/range/agg queries without JSON parsing per document.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::Value;
use xerj_common::types::{FieldType, Schema};
use xerj_compress::field_codec::{FieldAnalyzer, FieldEncoding};
use xerj_fts::analyzer::AnalyzerRegistry;
use xerj_fts::bm25::Bm25Scorer;

// ── Types ─────────────────────────────────────────────────────────────────────

/// A scored hit from the memtable.
#[derive(Debug, Clone)]
pub struct MemtableHit {
    pub doc_id: String,
    pub score: f32,
}

/// Entry stored in the memtable.
///
/// Post-M4.9 the `fields: HashMap<String, String>` was removed — it was
/// a per-doc allocation built at ingest time that duplicated data
/// already present in `source: Arc<Value>`.  Flush-time consumers
/// reconstruct it lazily from `source` via `extract_text_fields_from`,
/// so the hot ingest path does nothing more than a `Vec::push` of this
/// struct plus one `Arc::clone` of the pre-parsed source.
///
/// Post-M5.0 the struct carries `seq_no` so that `drain_with_sources`
/// can SORT the drained docs by global sequence number — this lets us
/// lift the WAL append OUT of the engine memtable write lock.  Pre-M5.0
/// the lock spanned WAL append + memtable push so both memtables saw
/// docs in identical order; now the WAL and engine memtable can be
/// advanced independently under different locks, and flush drains
/// recover the global order by seq_no sort.
#[derive(Debug, Clone)]
struct MemEntry {
    /// Global WAL sequence number assigned by the storage layer.
    /// Zero when unknown (e.g. test shims and single-doc `insert`).
    seq_no: u64,
    doc_id: String,
    /// Original source document as ingested (preserves arrays, nested objects, etc.).
    /// Stored as `Arc<Value>` so that turbo-ingest batches can share the same
    /// allocation across WAL serialisation and this memtable entry — no deep copy.
    ///
    /// M5.11 — in the ultra-turbo hot path this can be `Value::Null` as a
    /// placeholder.  When the caller supplies `source_bytes` (non-empty) we
    /// defer the JSON parse all the way until drain-for-flush time so the
    /// ingest path doesn't burn ~5 µs/doc on `serde_json::from_str`.
    source: Arc<Value>,
    /// M5.11 — raw NDJSON bytes for the document, kept alongside `source` so
    /// that drain can lazily parse them into `Value` if `source` is still
    /// `Value::Null`.  This lets the bulk hot path SKIP the doc-body parse
    /// entirely and push work to the background flush thread pool.
    source_bytes: Arc<[u8]>,
    /// Approximate byte size for flush threshold tracking.
    size_bytes: usize,
}

/// Reconstruct the (field_name → flattened text) map that pre-M4.9
/// `MemEntry` used to cache eagerly at ingest time.  Called only by
/// flush (`drain_with_sources`, `drain`) and by the rare
/// `get_source` query path — neither is on the hot ingest loop.
pub fn extract_text_fields_from(source: &Value) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(obj) = source.as_object() {
        for (key, val) in obj {
            let text = extract_text_value(val);
            if !text.is_empty() {
                out.insert(key.clone(), text);
            }
        }
    }
    out
}

/// Posting list entry: doc_id → term frequency.
type PostingList = HashMap<String, u32>; // doc_id → tf

// ── DocValues ─────────────────────────────────────────────────────────────────

/// Number of raw string samples to collect per field before running
/// smart encoding analysis.
const ANALYSIS_THRESHOLD: usize = 1000;

/// Columnar doc-values store, one column per field, one row per document
/// (in insertion order, parallel to `FtsMemtable::docs`).
///
/// This enables O(N * simple_compare) term/range queries and O(N) aggregations
/// instead of O(N * JSON_parse) full-document scans.
///
/// After `ANALYSIS_THRESHOLD` values are collected per field, `FieldAnalyzer`
/// is run to determine the optimal encoding.  Subsequent writes use the
/// chosen encoding.
#[derive(Default)]
pub struct DocValues {
    /// field → per-doc numeric value (for Long, Double, Date fields and any
    /// field whose value can be parsed as f64).
    pub numeric: HashMap<String, Vec<Option<f64>>>,
    /// field → per-doc keyword value (for Keyword, IP, and any string field).
    pub keyword: HashMap<String, Vec<Option<String>>>,
    /// field → set of all distinct values seen (for fast cardinality / terms aggs).
    ///
    /// V4 M4: rebuilt lazily from `keyword` via `ensure_counts_built`.
    pub keyword_set: HashMap<String, HashSet<String>>,
    /// field → value → live doc count.
    ///
    /// V4 M4: lazily rebuilt from `keyword` when `counts_dirty == true`.
    /// Old design incrementally maintained this on every push; that cost
    /// 5-8 HashMap::entry chains per field per doc on the ingest-path
    /// hot loop and capped xerj at ~10 k docs/s.  Deferring the rebuild
    /// to query time (or to flush time) trades a one-time O(n) scan for
    /// a 5× ingest-rate improvement.
    pub keyword_counts: HashMap<String, HashMap<String, u32>>,
    /// field → numeric value → live doc count.  Same lazy contract.
    pub numeric_counts: HashMap<String, HashMap<u64, u32>>,
    /// V4 M4: set to `true` on every `push_field`; cleared by
    /// `ensure_counts_built` once the count/set maps are in sync
    /// with the column data again.
    pub counts_dirty: bool,
    /// Analyzed field encodings — built lazily after sufficient samples.
    pub analyzed_encodings: HashMap<String, FieldEncoding>,
    /// Raw string samples per field for deferred analysis (cleared after analysis).
    pub samples: HashMap<String, Vec<String>>,
    /// Whether analysis has been performed for each field.
    pub analyzed: HashMap<String, bool>,
}

impl DocValues {
    /// V4 M4 — lazy rebuild of the keyword/numeric count + set maps.
    ///
    /// Called by the query path (term shortcut, terms-agg, cardinality)
    /// and by the flush path right before the column snapshot is taken.
    /// O(sum(column_lengths)) once, then free until the next `push_field`.
    pub fn ensure_counts_built(&mut self) {
        if !self.counts_dirty {
            return;
        }
        self.keyword_counts.clear();
        self.numeric_counts.clear();
        self.keyword_set.clear();

        for (field, col) in &self.keyword {
            let counts = self.keyword_counts.entry(field.clone()).or_default();
            let set = self.keyword_set.entry(field.clone()).or_default();
            for slot in col {
                if let Some(s) = slot {
                    *counts.entry(s.clone()).or_insert(0) += 1;
                    if !set.contains(s) {
                        set.insert(s.clone());
                    }
                }
            }
        }
        for (field, col) in &self.numeric {
            let counts = self.numeric_counts.entry(field.clone()).or_default();
            for slot in col {
                if let Some(f) = slot {
                    *counts.entry(f.to_bits()).or_insert(0) += 1;
                }
            }
        }
        self.counts_dirty = false;
    }
}

impl DocValues {
    /// Append values for all fields in `source` to the columnar store.
    ///
    /// `doc_index` is the index that this document will occupy in `docs`.
    /// All existing columns are padded to length `doc_index` with `None`
    /// before appending so that every column stays aligned with `docs`.
    fn push(&mut self, source: &Value, doc_index: usize) {
        if let Some(obj) = source.as_object() {
            for (key, val) in obj {
                self.push_field(key, val, doc_index);
            }
        }
    }

    /// Collect a string sample for a field and trigger analysis when threshold is reached.
    fn collect_sample(&mut self, field: &str, value: &str) {
        if self.analyzed.get(field).copied().unwrap_or(false) {
            // Already analyzed — no more samples needed.
            return;
        }
        let samples = self.samples.entry(field.to_string()).or_default();
        samples.push(value.to_string());

        if samples.len() >= ANALYSIS_THRESHOLD {
            // Run the smart analyzer on all collected samples.
            let analyzer = FieldAnalyzer::default();
            let refs: Vec<&str> = samples.iter().map(|s| s.as_str()).collect();
            let encoding = analyzer.analyze(field, &refs);
            self.analyzed_encodings.insert(field.to_string(), encoding);
            self.analyzed.insert(field.to_string(), true);
            // Free samples memory now that we have an encoding.
            self.samples.remove(field);
        }
    }

    fn push_field(&mut self, field: &str, val: &Value, doc_index: usize) {
        // V4 M4: ingest-path push_field is reduced to the minimum —
        // raw column storage only.  The `keyword_counts`,
        // `numeric_counts`, `keyword_set`, and `samples` maps are all
        // **populated lazily** at query time via `ensure_counts_built`
        // (see `memtable.rs` → `counts_dirty` flag).  This drops per-
        // doc HashMap churn from 5-8 entry chains per field to 1 Vec
        // push — measured at ~5× faster ingest on log workloads.
        self.counts_dirty = true;
        match val {
            Value::Number(n) => {
                let col = self.numeric.entry(field.to_string()).or_default();
                pad_to(col, doc_index);
                col.push(n.as_f64());
                // Keep the keyword column populated too for mixed-type
                // access but defer the set/count maps.
                let kcol = self.keyword.entry(field.to_string()).or_default();
                pad_to(kcol, doc_index);
                kcol.push(Some(n.to_string()));
            }
            Value::String(s) => {
                let kcol = self.keyword.entry(field.to_string()).or_default();
                pad_to(kcol, doc_index);
                kcol.push(Some(s.clone()));
                // If the string looks numeric, also index it in the numeric column.
                if let Ok(f) = s.parse::<f64>() {
                    let ncol = self.numeric.entry(field.to_string()).or_default();
                    pad_to(ncol, doc_index);
                    ncol.push(Some(f));
                }
            }
            Value::Bool(b) => {
                let kcol = self.keyword.entry(field.to_string()).or_default();
                pad_to(kcol, doc_index);
                kcol.push(Some(b.to_string()));
            }
            Value::Array(arr) => {
                // Flatten: store the first element (or None for empty arrays).
                // Each element is treated as a separate value for aggregations,
                // but for term/range queries we store the first scalar found.
                let first_num = arr.iter().find_map(|v| v.as_f64());
                let first_str: Option<String> = arr.iter().find_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    Value::Bool(b) => Some(b.to_string()),
                    _ => None,
                });

                let ncol = self.numeric.entry(field.to_string()).or_default();
                pad_to(ncol, doc_index);
                ncol.push(first_num);

                let kcol = self.keyword.entry(field.to_string()).or_default();
                pad_to(kcol, doc_index);
                if let Some(ref s) = first_str {
                    self.keyword_set
                        .entry(field.to_string())
                        .or_default()
                        .insert(s.clone());
                }
                kcol.push(first_str);
            }
            Value::Null | Value::Object(_) => {
                // Push None for null/object fields so columns stay aligned.
                let ncol = self.numeric.entry(field.to_string()).or_default();
                pad_to(ncol, doc_index);
                ncol.push(None);

                let kcol = self.keyword.entry(field.to_string()).or_default();
                pad_to(kcol, doc_index);
                kcol.push(None);
            }
        }
    }

    /// Remove the entry at `doc_index` from all columns (called on delete).
    fn remove_at(&mut self, doc_index: usize) {
        for col in self.numeric.values_mut() {
            if doc_index < col.len() {
                col.remove(doc_index);
            }
        }
        for col in self.keyword.values_mut() {
            if doc_index < col.len() {
                col.remove(doc_index);
            }
        }
        // keyword_set is rebuilt lazily; we don't update it on remove for
        // simplicity (it's used for cardinality estimates, not exact counts).
        // analyzed_encodings, samples, analyzed are not updated on remove —
        // they are statistical summaries, not per-document state.
    }

    /// Return a reference to the analyzed field encodings map.
    pub fn field_encodings(&self) -> &HashMap<String, FieldEncoding> {
        &self.analyzed_encodings
    }
}

// ── ShardedFtsMemtable ───────────────────────────────────────────────────────
//
// A 16-shard wrapper over FtsMemtable.  The ingest hot path takes only
// one shard's mutex for the entire batch it's pushing, so N concurrent
// bulk clients pushing to N different shards run truly in parallel on
// the write side.  Query path acquires every shard sequentially and
// aggregates — the 16 lock acquisitions × ~100 ns each are negligible
// compared to actual query work and keep the public API point-in-time
// consistent.

/// Default number of shards in the engine memtable when callers don't
/// supply one. Must be a power of two so `hash & (N-1)` picks the shard.
/// 16 overlaps cleanly with up to 16 concurrent bulk clients without
/// significant collision (doc_id hash distribution is uniform).
///
/// Production callers do NOT use this default — they pass the operator-
/// tuned value via `with_registry_and_shards(registry, n)` from
/// `Config.engine.ingest_shards`. This constant survives only to keep
/// `ShardedFtsMemtable::new()` and `with_registry()` ergonomic for
/// tests and tools that don't care.
const DEFAULT_ENGINE_MEMTABLE_SHARDS: usize = 16;

/// Sharded engine FTS memtable.
///
/// Internally holds `num_shards` (set at construction) independent
/// `parking_lot::RwLock<FtsMemtable>` instances.  Batches in the turbo
/// ingest path route to exactly one shard (by hash of the first
/// doc_id) and take the **write** lock; query paths iterate all shards
/// under **read** locks, so 32 concurrent queries scale with core
/// count instead of serialising through one exclusive mutex per shard.
///
/// M5.19 — pre-M5.19 this was `Mutex<FtsMemtable>`, which made the
/// concurrent QPS bench collapse from ~5 k QPS (ES) / ~5 k QPS
/// (single-threaded xerj) down to 1 QPS (xerj under load) because
/// every query serialised through the same mutex sweep across 16
/// shards.  With RwLock reader-writer separation, 32 clients can
/// run search_inner concurrently.
///
/// Drain methods re-order the per-shard outputs by global WAL seq_no
/// so the downstream segment writer observes docs in canonical order
/// even though different shards saw pushes at different wall-clock
/// instants.
pub struct ShardedFtsMemtable {
    shards: Vec<parking_lot::RwLock<FtsMemtable>>,
    shard_mask: usize,
}

impl Default for ShardedFtsMemtable {
    fn default() -> Self {
        Self::new()
    }
}

impl ShardedFtsMemtable {
    pub fn new() -> Self {
        let registry = Arc::new(AnalyzerRegistry::default());
        Self::with_registry(registry)
    }

    pub fn with_registry(registry: Arc<AnalyzerRegistry>) -> Self {
        Self::with_registry_and_shards(registry, DEFAULT_ENGINE_MEMTABLE_SHARDS)
    }

    pub fn with_registry_and_shards(registry: Arc<AnalyzerRegistry>, num_shards: usize) -> Self {
        let n = num_shards.max(1).next_power_of_two();
        let shards = (0..n)
            .map(|_| parking_lot::RwLock::new(FtsMemtable::with_registry(Arc::clone(&registry))))
            .collect();
        Self { shards, shard_mask: n - 1 }
    }

    #[inline]
    pub fn shard_for_dynamic(&self, doc_id: &str) -> usize {
        let h = xxhash_rust::xxh3::xxh3_64(doc_id.as_bytes());
        (h as usize) & self.shard_mask
    }

    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    /// Run `f` with exclusive access to a specific shard's
    /// `FtsMemtable`.  Used by the turbo ingest hot path to hold one
    /// shard's lock for the entire bulk batch so all docs of a batch
    /// see each other's state consistently.
    pub fn with_shard_mut<R>(&self, shard: usize, f: impl FnOnce(&mut FtsMemtable) -> R) -> R {
        let mut g = self.shards[shard].write();
        f(&mut *g)
    }

    /// Run `f` with shared (read-only) access to a specific shard.
    pub fn with_shard<R>(&self, shard: usize, f: impl FnOnce(&FtsMemtable) -> R) -> R {
        let g = self.shards[shard].read();
        f(&*g)
    }

    /// Total document count across all shards.
    pub fn doc_count(&self) -> usize {
        self.shards.iter().map(|s| s.read().doc_count()).sum()
    }

    /// Total approximate byte size across all shards.
    pub fn size_bytes(&self) -> usize {
        self.shards.iter().map(|s| s.read().size_bytes()).sum()
    }

    /// Drop a doc from whichever shard owns it.
    pub fn remove(&self, doc_id: &str) {
        let s = self.shard_for_dynamic(doc_id);
        self.shards[s].write().remove(doc_id);
    }

    /// Return `true` if any shard holds the doc.  Picks the
    /// correct shard directly — no need to scan others.
    pub fn contains(&self, doc_id: &str) -> bool {
        let s = self.shard_for_dynamic(doc_id);
        self.shards[s].read().contains(doc_id)
    }

    pub fn get_doc_source_as_value(&self, doc_id: &str) -> Option<Value> {
        let s = self.shard_for_dynamic(doc_id);
        self.shards[s].read().get_doc_source_as_value(doc_id)
    }

    pub fn get_doc_source_arc(&self, doc_id: &str) -> Option<Arc<Value>> {
        let s = self.shard_for_dynamic(doc_id);
        self.shards[s].read().get_doc_source_arc(doc_id)
    }

    /// Return every doc id in every shard (unordered).
    pub fn all_doc_ids(&self) -> Vec<String> {
        let mut out = Vec::new();
        for s in &self.shards {
            out.extend(s.read().all_doc_ids());
        }
        out
    }

    /// Return every (doc_id, source) pair.  Aggregates across all
    /// shards; insertion order is preserved within a shard but not
    /// across shards.  Callers that need global order should sort
    /// downstream.
    pub fn all_docs_with_sources(&self) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        for s in &self.shards {
            out.extend(s.read().all_docs_with_sources());
        }
        out
    }

    /// Drain every shard, merge-sort by seq_no, and return the
    /// combined (doc_id, text_fields, source) stream.  This is the
    /// single entry point used by the flush path — the sort
    /// canonicalises global WAL order across the independent shards.
    pub fn drain_with_sources(&self) -> Vec<(String, HashMap<String, String>, Value)> {
        let mut all: Vec<(u64, (String, HashMap<String, String>, Value))> = Vec::new();
        for s in &self.shards {
            let mut g = s.write();
            let rows = g.drain_with_sources_raw();
            all.extend(rows);
        }
        all.sort_by_key(|(seq, _)| *seq);
        all.into_iter().map(|(_, t)| t).collect()
    }

    /// Drain every shard (deletes tombstones only), merge by seq_no.
    pub fn drain(&self) -> Vec<(String, HashMap<String, String>)> {
        let mut all: Vec<(u64, (String, HashMap<String, String>))> = Vec::new();
        for s in &self.shards {
            let mut g = s.write();
            let rows = g.drain_raw();
            all.extend(rows);
        }
        all.sort_by_key(|(seq, _)| *seq);
        all.into_iter().map(|(_, t)| t).collect()
    }

    /// Per-doc field-length getter used by memtable BM25 scoring.
    pub fn get_source(&self, doc_id: &str) -> Option<HashMap<String, String>> {
        let s = self.shard_for_dynamic(doc_id);
        self.shards[s].read().get_source(doc_id)
    }

    /// Combined numeric doc-values column — concatenates per-shard
    /// columns.  Note: column indexes are per-shard; callers that use
    /// absolute doc indices must account for shard offsets.  For
    /// aggregations this is sufficient because the values are what
    /// matter, not the indices.
    pub fn doc_values_numeric_values(&self, field: &str) -> Vec<Option<f64>> {
        let mut out = Vec::new();
        for s in &self.shards {
            let g = s.read();
            if let Some(col) = g.doc_values_numeric_column(field) {
                out.extend_from_slice(col);
            }
        }
        out
    }

    pub fn doc_values_keyword_values(&self, field: &str) -> Vec<Option<String>> {
        let mut out = Vec::new();
        for s in &self.shards {
            let g = s.read();
            if let Some(col) = g.doc_values_keyword_column(field) {
                out.extend(col.iter().cloned());
            }
        }
        out
    }

    /// Fan out a term count query across shards and sum.
    ///
    /// NOTE: FtsMemtable::doc_values_keyword_count takes `&mut self`
    /// because it lazily builds the counts map on first access via
    /// `ensure_counts_built()`.  Until that's moved behind interior
    /// mutability, we must take the shard's **write** lock here, which
    /// serialises concurrent term-count queries.  The fast `term`
    /// path goes through `doc_values_term_query` which IS read-locked.
    pub fn doc_values_keyword_count(&self, field: &str, value: &str) -> u32 {
        self.shards
            .iter()
            .map(|s| s.write().doc_values_keyword_count(field, value).unwrap_or(0))
            .sum()
    }

    pub fn doc_values_numeric_count(&self, field: &str, value: f64) -> u32 {
        self.shards
            .iter()
            .map(|s| s.write().doc_values_numeric_count(field, value).unwrap_or(0))
            .sum()
    }

    /// Aggregate all terms + counts for a field across shards.
    pub fn all_terms_for_field(&self, field: &str) -> Vec<(String, usize)> {
        let mut acc: HashMap<String, usize> = HashMap::new();
        for s in &self.shards {
            let g = s.read();
            for (term, count) in g.all_terms_for_field(field) {
                *acc.entry(term).or_insert(0) += count;
            }
        }
        acc.into_iter().collect()
    }

    pub fn all_keyword_values_for_field(&self, field: &str) -> Vec<(String, usize)> {
        let mut acc: HashMap<String, usize> = HashMap::new();
        for s in &self.shards {
            let g = s.read();
            for (v, c) in g.all_keyword_values_for_field(field) {
                *acc.entry(v).or_insert(0) += c;
            }
        }
        acc.into_iter().collect()
    }

    /// Full-text search across all shards, top-`limit` hits.
    ///
    /// BM25 is computed with GLOBAL doc_count + global per-term doc_freq +
    /// global avg_field_length so the score is segment-invariant. Without
    /// this, sharded memtable runs would give each shard its own (smaller)
    /// statistics — flat IDFs and dropped length normalisation, since
    /// many shards would have N=1, doc_freq=1.
    pub fn search_text(&self, query: &str, fields: &[&str], limit: usize) -> Vec<MemtableHit> {
        // Pre-pass: tokenise the query (use any shard's analyzer — they're
        // all the same registry-provided one) and aggregate per-term
        // global doc_freq + per-field global stats.
        let analyzer = self.shards.iter()
            .find_map(|s| {
                let g = s.read();
                g.registry.get_analyzer("default")
                    .or_else(|| g.registry.get_analyzer("standard"))
            });
        let analyzer = match analyzer { Some(a) => a, None => return Vec::new() };
        let q_tokens = analyzer.analyze(query);
        if q_tokens.is_empty() { return Vec::new(); }

        // Delete-aware BM25 collection statistics (Lucene/ES parity): the
        // scoring N counts both live docs AND tombstoned/superseded versions
        // that have not yet been merged away.  NOTE: this is *only* the N fed
        // to the BM25 IDF — hits.total and pagination still use the live
        // `doc_count()`, so a search over an index that has never had an
        // update/delete scores bit-for-bit identically to before.
        let mut global_doc_count: u64 = self.doc_count() as u64;
        // Aggregate (per-field global avg_field_len, per-(field,term) doc_freq).
        let mut field_total_len: std::collections::HashMap<String, (f64, u64)> =
            std::collections::HashMap::new();
        let mut term_global_df: std::collections::HashMap<(String, String), u64> =
            std::collections::HashMap::new();
        for shard in &self.shards {
            let g = shard.read();
            // Live N is already in `global_doc_count`; add tombstoned versions.
            global_doc_count += g.ghost_docs;
            // Field length sums (live).
            for (fname, (sum, n)) in &g.avg_field_lengths {
                let entry = field_total_len.entry(fname.clone()).or_insert((0.0, 0));
                entry.0 += sum;
                entry.1 += n;
            }
            // Field length sums (tombstoned versions retained for avgdl).
            for (fname, (sum, n)) in &g.ghost_field_len {
                let entry = field_total_len.entry(fname.clone()).or_insert((0.0, 0));
                entry.0 += sum;
                entry.1 += n;
            }
            // Per-term doc_freq across shards (live postings).
            for (fname, postings) in &g.index {
                if !fields.is_empty() && !fields.iter().any(|f| f == fname) {
                    continue;
                }
                for token in &q_tokens {
                    if let Some(pl) = postings.get(&token.text) {
                        *term_global_df.entry((fname.clone(), token.text.clone()))
                            .or_insert(0) += pl.len() as u64;
                    }
                }
            }
            // Per-term doc_freq from tombstoned versions (delete-aware df).
            for (fname, terms) in &g.ghost_doc_freq {
                if !fields.is_empty() && !fields.iter().any(|f| f == fname) {
                    continue;
                }
                for token in &q_tokens {
                    if let Some(df) = terms.get(&token.text) {
                        *term_global_df.entry((fname.clone(), token.text.clone()))
                            .or_insert(0) += *df;
                    }
                }
            }
        }
        let global_avg_field_len: std::collections::HashMap<String, f32> =
            field_total_len.into_iter()
                .map(|(k, (sum, n))| (k, if n == 0 { 0.0 } else { (sum / n as f64) as f32 }))
                .collect();

        let mut all: Vec<MemtableHit> = Vec::new();
        for s in &self.shards {
            all.extend(s.read().search_text_with_global_stats(
                query, fields, limit,
                global_doc_count, &global_avg_field_len, &term_global_df,
            ));
        }
        all.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        all.truncate(limit);
        all
    }

    /// Drop-in replacement for the old single-memtable `insert` used by
    /// the `IndexStore::index()` single-doc path.
    pub fn insert(&self, doc_id: String, source: &Value, schema: &Schema, seq_no: u64) {
        let s = self.shard_for_dynamic(&doc_id);
        self.shards[s].write().insert(doc_id, source, schema, seq_no);
    }

    /// Drop-in for `insert_pretokenized_with_seq` — picks a shard by
    /// doc_id so delete / re-insert on the same doc collide under
    /// the same shard lock.
    pub fn insert_pretokenized_with_seq(
        &self,
        seq_no: u64,
        doc_id: String,
        source: Arc<Value>,
        tokens: &[String],
    ) {
        let s = self.shard_for_dynamic(&doc_id);
        self.shards[s]
            .write()
            .insert_pretokenized_with_seq(seq_no, doc_id, source, tokens);
    }

    /// M5.11 — raw-bytes ultra-turbo insert (shard-routed).
    pub fn insert_raw_bytes_with_seq(
        &self,
        seq_no: u64,
        doc_id: String,
        source_bytes: Arc<[u8]>,
    ) {
        let s = self.shard_for_dynamic(&doc_id);
        self.shards[s]
            .write()
            .insert_raw_bytes_with_seq(seq_no, doc_id, source_bytes);
    }

    /// Iterate every document in every shard as `(doc_id, Value)`.
    /// Clones each `Arc<Value>`'s inner so callers that expect an
    /// owned `Value` keep working.
    pub fn all_docs_cloned(&self) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        for s in &self.shards {
            let g = s.read();
            out.extend(g.all_docs());
        }
        out
    }

    /// Fold `f` over every document's `(doc_id, Value)` pair under
    /// the owning shard's lock.  Used by query paths that want a
    /// streaming view without materialising a full Vec.
    pub fn for_each_doc<F: FnMut(&str, &Value)>(&self, mut f: F) {
        for s in &self.shards {
            let g = s.read();
            for (doc_id, val) in g.all_docs() {
                f(&doc_id, &val);
            }
        }
    }

    /// Fold `f` over every numeric value (from doc-values) for
    /// `field` across all shards.  Used by stats/sum/avg/min/max
    /// agg runners — replaces the `mem.doc_values.numeric.get(field)`
    /// single-shard borrow that the old code did.  The closure may
    /// return `false` to stop iteration early.
    pub fn for_each_numeric_value<F: FnMut(f64)>(&self, field: &str, mut f: F) {
        for s in &self.shards {
            let g = s.read();
            if let Some(col) = g.doc_values_numeric_column(field) {
                for v in col.iter().flatten() {
                    f(*v);
                }
            }
        }
    }

    /// Fold `f` over every (keyword_value, count) pair for a field
    /// across shards.
    pub fn for_each_keyword_value<F: FnMut(&str)>(&self, field: &str, mut f: F) {
        for s in &self.shards {
            let g = s.read();
            if let Some(col) = g.doc_values_keyword_column(field) {
                for v in col.iter().flatten() {
                    f(v);
                }
            }
        }
    }

    /// Total live numeric entries for `field` (sum of per-shard
    /// non-null counts).
    pub fn numeric_count(&self, field: &str) -> u64 {
        let mut n: u64 = 0;
        for s in &self.shards {
            let g = s.read();
            if let Some(col) = g.doc_values_numeric_column(field) {
                n += col.iter().filter(|v| v.is_some()).count() as u64;
            }
        }
        n
    }

    /// Total live keyword entries for `field`.
    pub fn keyword_count(&self, field: &str) -> u64 {
        let mut n: u64 = 0;
        for s in &self.shards {
            let g = s.read();
            if let Some(col) = g.doc_values_keyword_column(field) {
                n += col.iter().filter(|v| v.is_some()).count() as u64;
            }
        }
        n
    }

    /// DocValues term query — aggregates hits across all shards.
    /// Returns `Some(Vec<(doc_id, local_idx)>)` if any shard matched.
    /// The `local_idx` is shard-local; callers use the doc_id to
    /// resolve the source via `get_doc_source_*`.
    pub fn doc_values_term_query(&self, field: &str, value: &str) -> Option<Vec<(String, usize)>> {
        let mut out: Vec<(String, usize)> = Vec::new();
        let mut any_hit = false;
        for s in &self.shards {
            let g = s.read();
            if let Some(mut hits) = g.doc_values_term_query(field, value) {
                any_hit = true;
                out.append(&mut hits);
            }
        }
        if any_hit {
            Some(out)
        } else {
            None
        }
    }

    pub fn doc_values_terms_query(
        &self,
        field: &str,
        values: &[String],
    ) -> Option<Vec<(String, usize)>> {
        let mut out: Vec<(String, usize)> = Vec::new();
        let mut any_hit = false;
        for s in &self.shards {
            let g = s.read();
            if let Some(mut hits) = g.doc_values_terms_query(field, values) {
                any_hit = true;
                out.append(&mut hits);
            }
        }
        if any_hit {
            Some(out)
        } else {
            None
        }
    }

    /// Aggregated smart-field-encoding map across all shards.
    pub fn aggregated_field_encodings(&self) -> HashMap<String, FieldEncoding> {
        let mut out: HashMap<String, FieldEncoding> = HashMap::new();
        for s in &self.shards {
            let g = s.read();
            for (k, v) in g.analyzed_field_encodings().iter() {
                out.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        out
    }

    pub fn doc_values_range_query(
        &self,
        field: &str,
        gte: Option<f64>,
        gt: Option<f64>,
        lte: Option<f64>,
        lt: Option<f64>,
    ) -> Option<Vec<(String, usize)>> {
        let mut out: Vec<(String, usize)> = Vec::new();
        let mut any_hit = false;
        for s in &self.shards {
            let g = s.read();
            if let Some(mut hits) = g.doc_values_range_query(field, gte, gt, lte, lt) {
                any_hit = true;
                out.append(&mut hits);
            }
        }
        if any_hit {
            Some(out)
        } else {
            None
        }
    }

    /// Drain every shard and return raw `(seq_no, doc_id, source_arc)`
    /// triples in WAL-sequence order.  Used by the flush path to
    /// construct a `DrainedMemtable` for the storage finalizer.
    ///
    /// M5.11 — entries with `source = Value::Null` but non-empty
    /// `source_bytes` are lazily parsed here on the flush thread,
    /// keeping the ingest hot path free of `serde_json::from_str`.
    pub fn drain_for_flush(&self) -> Vec<(u64, String, Arc<Value>, Arc<[u8]>)> {
        let mut all: Vec<(u64, String, Arc<Value>, Arc<[u8]>)> = Vec::new();
        for shard_idx in 0..self.shards.len() {
            let mut shard_entries = self.drain_shard(shard_idx);
            all.append(&mut shard_entries);
        }
        all.sort_by_key(|(seq, _, _, _)| *seq);
        all
    }

    /// Drain ONE shard and return `(seq_no, doc_id, source_arc, raw_bytes)`
    /// tuples in WAL-sequence order.  Raw bytes are passed through to the
    /// segment writer so it can skip re-serializing the Value.
    pub fn drain_shard(&self, shard_idx: usize) -> Vec<(u64, String, Arc<Value>, Arc<[u8]>)> {
        self.drain_shard_inner(shard_idx, false)
    }

    /// Drain without parsing raw-bytes entries. Returns Value::Null for
    /// entries that came from insert_raw_bytes_with_seq. Use when neither
    /// FTS nor DV sidecars will be built (turbo/CLI ingest path).
    pub fn drain_shard_raw(&self, shard_idx: usize) -> Vec<(u64, String, Arc<Value>, Arc<[u8]>)> {
        self.drain_shard_inner(shard_idx, true)
    }

    fn drain_shard_inner(&self, shard_idx: usize, skip_parse: bool) -> Vec<(u64, String, Arc<Value>, Arc<[u8]>)> {
        let drained: Vec<MemEntry> = {
            let mut g = self.shards[shard_idx].write();
            let d: Vec<MemEntry> = g.docs.drain(..).collect();
            g.index = HashMap::new();
            g.doc_values = DocValues::default();
            g.total_bytes = 0;
            g.field_lengths = HashMap::new();
            g.avg_field_lengths = HashMap::new();
            g.doc_id_index = HashMap::new();
            // Flush == merge: purge delete-aware ghost collection stats.
            g.ghost_docs = 0;
            g.ghost_field_len = HashMap::new();
            g.ghost_doc_freq = HashMap::new();
            g.docs.shrink_to_fit();
            d
        };
        let mut out: Vec<(u64, String, Arc<Value>, Arc<[u8]>)> = drained
            .into_iter()
            .map(|e| {
                let raw = e.source_bytes.clone();
                let source = if skip_parse {
                    e.source
                } else if e.source.is_null() && !raw.is_empty() {
                    match serde_json::from_slice::<Value>(&raw) {
                        Ok(v) => Arc::new(v),
                        Err(_) => Arc::new(Value::Null),
                    }
                } else {
                    e.source
                };
                (e.seq_no, e.doc_id, source, raw)
            })
            .collect();
        out.sort_by_key(|(seq, _, _, _)| *seq);
        out
    }

    /// Check if a shard's first entry was inserted via the raw-bytes
    /// path (`insert_raw_bytes_with_seq`).  Used by `do_flush_shard`
    /// to decide whether to build FTS sidecars at flush time.
    pub fn peek_shard_has_raw_bytes(&self, shard_idx: usize) -> bool {
        let g = self.shards[shard_idx].read();
        g.docs.first().map(|e| !e.source_bytes.is_empty()).unwrap_or(false)
    }

    /// Return `(shard_idx, doc_count, size_bytes)` triples so the
    /// flush scheduler can pick which shard to drain next.  The
    /// tuple is sampled under each shard's own lock so it's
    /// point-in-time accurate.
    pub fn shard_loads(&self) -> Vec<(usize, usize, usize)> {
        self.shards
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let g = s.read();
                (i, g.doc_count(), g.size_bytes())
            })
            .collect()
    }

    /// Return `(doc_count, size_bytes)` for a single shard.  Used by
    /// the sync ingest path to decide whether to raise the flush
    /// signal bit for the shard it just wrote to — cheaper than
    /// iterating all shards via `shard_loads()`.
    pub fn shard_load(&self, shard_idx: usize) -> (usize, usize) {
        let g = self.shards[shard_idx].read();
        (g.doc_count(), g.size_bytes())
    }
}

// Expose raw drain helpers on FtsMemtable that include seq_no so the
// sharded wrapper can merge by global WAL order.
impl FtsMemtable {
    pub fn drain_with_sources_raw(
        &mut self,
    ) -> Vec<(u64, (String, HashMap<String, String>, Value))> {
        let mut drained: Vec<MemEntry> = self.docs.drain(..).collect();
        drained.sort_by_key(|e| e.seq_no);
        let result: Vec<_> = drained
            .into_iter()
            .map(|e| {
                let seq = e.seq_no;
                let val = Arc::try_unwrap(e.source).unwrap_or_else(|arc| (*arc).clone());
                let fields = extract_text_fields_from(&val);
                (seq, (e.doc_id, fields, val))
            })
            .collect();
        self.index = HashMap::new();
        self.doc_values = DocValues::default();
        self.total_bytes = 0;
        self.field_lengths = HashMap::new();
        self.avg_field_lengths = HashMap::new();
        self.doc_id_index = HashMap::new();
        // Flush is the equivalent of a Lucene merge: tombstone contributions
        // are purged, so delete-aware ghost statistics reset to empty.
        self.ghost_docs = 0;
        self.ghost_field_len = HashMap::new();
        self.ghost_doc_freq = HashMap::new();
        self.docs.shrink_to_fit();
        result
    }

    pub fn drain_raw(&mut self) -> Vec<(u64, (String, HashMap<String, String>))> {
        let mut drained: Vec<MemEntry> = self.docs.drain(..).collect();
        drained.sort_by_key(|e| e.seq_no);
        let result: Vec<_> = drained
            .into_iter()
            .map(|e| {
                let seq = e.seq_no;
                let fields = extract_text_fields_from(e.source.as_ref());
                (seq, (e.doc_id, fields))
            })
            .collect();
        self.index = HashMap::new();
        self.doc_values = DocValues::default();
        self.total_bytes = 0;
        self.field_lengths = HashMap::new();
        self.avg_field_lengths = HashMap::new();
        self.doc_id_index = HashMap::new();
        // Flush is the equivalent of a Lucene merge: tombstone contributions
        // are purged, so delete-aware ghost statistics reset to empty.
        self.ghost_docs = 0;
        self.ghost_field_len = HashMap::new();
        self.ghost_doc_freq = HashMap::new();
        self.docs.shrink_to_fit();
        result
    }
}

/// Pad a column with `None` up to (but not including) `target_len`.
fn pad_to<T>(col: &mut Vec<Option<T>>, target_len: usize) {
    while col.len() < target_len {
        col.push(None);
    }
}

// ── FtsMemtable ──────────────────────────────────────────────────────────────

/// In-memory inverted index for documents awaiting flush.
pub struct FtsMemtable {
    /// Documents in insertion order.
    docs: Vec<MemEntry>,
    /// Inverted index: field → term → posting list (doc_id → tf).
    index: HashMap<String, HashMap<String, PostingList>>,
    /// Columnar doc-values store for fast term/range/agg queries.
    pub doc_values: DocValues,
    /// Total accumulated byte size.
    total_bytes: usize,
    /// Analyzer registry.
    registry: Arc<AnalyzerRegistry>,
    /// Precomputed field lengths for BM25 scoring: field → {doc_id → token_count}
    field_lengths: HashMap<String, HashMap<String, u32>>,
    /// Running average field length per field: field → (total_tokens, doc_count)
    avg_field_lengths: HashMap<String, (f64, u64)>,
    /// doc_id → position in self.docs for O(1) lookup
    doc_id_index: HashMap<String, usize>,
    /// Delete-aware BM25 collection statistics (Lucene/ES parity).
    ///
    /// When a document is superseded by an update (remove + re-insert) or
    /// explicitly deleted, Lucene keeps that document's contribution to the
    /// collection statistics (N, total field length, per-term doc_freq)
    /// until the segment is actually merged.  We mirror that: `remove()`
    /// strips the old version from the LIVE structures (so it no longer
    /// matches or counts toward hits.total) but folds its contribution into
    /// these "ghost" accumulators, which are added back ONLY for BM25
    /// scoring.  They reset to empty on every drain/flush — the moral
    /// equivalent of a Lucene merge purging tombstones.
    ///
    /// A document that was never removed contributes nothing here, so its
    /// BM25 score is bit-for-bit identical to before this feature existed.
    ghost_docs: u64,
    ghost_field_len: HashMap<String, (f64, u64)>,
    ghost_doc_freq: HashMap<String, HashMap<String, u64>>,
}

impl FtsMemtable {
    /// Create a new empty memtable with the default analyzer registry.
    pub fn new() -> Self {
        Self {
            docs: Vec::new(),
            index: HashMap::new(),
            doc_values: DocValues::default(),
            total_bytes: 0,
            registry: Arc::new(AnalyzerRegistry::default()),
            field_lengths: HashMap::new(),
            avg_field_lengths: HashMap::new(),
            doc_id_index: HashMap::new(),
            ghost_docs: 0,
            ghost_field_len: HashMap::new(),
            ghost_doc_freq: HashMap::new(),
        }
    }

    /// Create a memtable using a shared custom analyzer registry.
    ///
    /// Use this when custom analyzers (synonyms, edge-ngrams, etc.) have been
    /// configured in the index settings so that indexing and query expansion
    /// use the same pipeline.
    pub fn with_registry(registry: Arc<AnalyzerRegistry>) -> Self {
        Self {
            docs: Vec::new(),
            index: HashMap::new(),
            doc_values: DocValues::default(),
            total_bytes: 0,
            registry,
            field_lengths: HashMap::new(),
            avg_field_lengths: HashMap::new(),
            doc_id_index: HashMap::new(),
            ghost_docs: 0,
            ghost_field_len: HashMap::new(),
            ghost_doc_freq: HashMap::new(),
        }
    }

    /// Insert a document into the memtable, indexing all text fields.
    pub fn insert(&mut self, doc_id: String, source: &Value, schema: &Schema, seq_no: u64) {
        let mut text_fields: HashMap<String, String> = HashMap::new();

        // Index fields that are defined as Text in the schema.
        for field_cfg in &schema.fields {
            if matches!(field_cfg.field_type, FieldType::Text) {
                if let Some(val) = source.get(&field_cfg.name) {
                    let text = extract_text_value(val);
                    if !text.is_empty() {
                        text_fields.insert(field_cfg.name.clone(), text);
                    }
                }
            }
        }

        // Also index any string-valued field not in the schema
        // (dynamic mapping). Walk nested objects with dotted paths
        // so a doc `{a: {b: {c: "x"}}}` indexes `a.b.c: "x"` —
        // queries targeting the leaf-specific dotted path match only
        // docs that carry that leaf. Also keep a root-level JSON-blob
        // entry for each top-level object key so types like flattened
        // (queried at the root path) still find their inner tokens.
        fn collect_text_fields(v: &Value, prefix: &str, out: &mut HashMap<String, String>) {
            match v {
                Value::Object(obj) => {
                    for (k, val) in obj {
                        let path = if prefix.is_empty() { k.clone() } else { format!("{}.{}", prefix, k) };
                        match val {
                            Value::Object(_) => {
                                // Root-level JSON-blob for flattened-style
                                // whole-object queries.
                                if prefix.is_empty() {
                                    let t = extract_text_value(val);
                                    if !t.is_empty() && !out.contains_key(&path) {
                                        out.insert(path.clone(), t);
                                    }
                                }
                                collect_text_fields(val, &path, out);
                            }
                            Value::Array(arr) => {
                                let joined: String = arr.iter()
                                    .map(extract_text_value)
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                if !joined.is_empty() && !out.contains_key(&path) {
                                    out.insert(path, joined);
                                }
                            }
                            _ => {
                                let t = extract_text_value(val);
                                if !t.is_empty() && !out.contains_key(&path) {
                                    out.insert(path, t);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(_obj) = source.as_object() {
            collect_text_fields(source, "", &mut text_fields);
        }

        // Build the inverted index entries.
        // Prefer a "default" analyzer if one was registered via custom settings
        // (e.g. with synonym expansion), otherwise fall back to "standard".
        let analyzer = self
            .registry
            .get_analyzer("default")
            .or_else(|| self.registry.get_analyzer("standard"))
            .expect("standard analyzer always present");

        for (field_name, text) in &text_fields {
            let tokens = analyzer.analyze(text);
            let token_count = tokens.len() as u32;

            // Cache the field length for BM25 scoring.
            self.field_lengths
                .entry(field_name.clone())
                .or_default()
                .insert(doc_id.clone(), token_count);

            // Update running average.
            let entry = self.avg_field_lengths.entry(field_name.clone()).or_insert((0.0, 0));
            entry.0 += token_count as f64;
            entry.1 += 1;

            let field_index = self.index.entry(field_name.clone()).or_default();
            for token in &tokens {
                let posting = field_index.entry(token.text.clone()).or_default();
                *posting.entry(doc_id.clone()).or_insert(0) += 1;
            }
        }

        // See `insert_pretokenized` for sizing rationale.
        let raw_size = source.to_string().len() + doc_id.len();
        let size = raw_size * 3 + 64;
        self.total_bytes += size;

        // Populate the columnar DocValues store BEFORE pushing to docs so that
        // the doc_index equals the current length (i.e. the slot we're about to fill).
        let doc_index = self.docs.len();
        self.doc_values.push(source, doc_index);

        // Track doc_id → index for O(1) lookup.
        self.doc_id_index.insert(doc_id.clone(), doc_index);

        self.docs.push(MemEntry {
            seq_no,
            doc_id,
            source: Arc::new(source.clone()),
            source_bytes: Arc::from(&[][..]),
            size_bytes: size,
        });
        let _ = text_fields; // still used above for inverted-index build
    }

    /// Insert with pre-tokenized terms — builds the inverted index using
    /// the standard analyzer pipeline (same as `insert`).
    /// Hot-path insert called by the turbo bulk ingest loop.  The
    /// `seq_no` comes from the WAL append that just completed — the
    /// engine stores it on the entry so `drain_with_sources` can
    /// restore global order by sort at flush time (this is what makes
    /// it safe to drop the engine memtable lock while the WAL append
    /// runs, so multiple concurrent bulk requests can pipeline).
    pub fn insert_pretokenized_with_seq(
        &mut self,
        seq_no: u64,
        doc_id: String,
        source: Arc<Value>,
        tokens: &[String],
    ) {
        self.insert_pretokenized_inner(seq_no, doc_id, source, tokens);
    }

    /// M5.11 — raw-bytes ultra-turbo insert.
    ///
    /// Skips the JSON parse on the ingest hot path.  Stores the raw
    /// NDJSON bytes; drain-for-flush reconstructs the `Value` lazily on
    /// the background flush thread.  Used by the `_bulk` turbo path to
    /// take the per-doc `serde_json::from_str` (~5 µs/doc) off the
    /// critical section.
    pub fn insert_raw_bytes_with_seq(
        &mut self,
        seq_no: u64,
        doc_id: String,
        source_bytes: Arc<[u8]>,
    ) {
        // M5.6 flat-800-byte estimate retained — changing it to a
        // per-doc `source_bytes.len()` in M5.17 regressed ingest
        // throughput (varies 400-640 k vs 870 k baseline) because
        // the math interacts non-obviously with the shard flush
        // scheduler.  Revert.  The 800-byte number over-estimates
        // compressed log docs by ~7× but keeps the flush cadence
        // predictable.
        let estimated = 800usize;
        self.total_bytes += estimated;

        let doc_index = self.docs.len();
        self.doc_id_index.insert(doc_id.clone(), doc_index);

        self.docs.push(MemEntry {
            seq_no,
            doc_id,
            source: Arc::new(Value::Null),
            source_bytes,
            size_bytes: estimated,
        });
    }

    /// Fresh-insert fast path: the caller guarantees `doc_id` does not
    /// already exist in this shard (bulk CLI ingest makes this promise —
    /// each scanner+seq pair is unique).  Avoids cloning `doc_id` twice
    /// (once for the HashMap key, once for `MemEntry`) by inserting
    /// the HashMap entry using a borrowed key lookup via `RawEntry`.
    /// Currently we still clone since stable Rust HashMap requires an
    /// owned key; the gain is skipping the prior `remove()` miss lookup.
    pub fn insert_raw_bytes_fresh(
        &mut self,
        seq_no: u64,
        doc_id: String,
        source_bytes: Arc<[u8]>,
    ) {
        let estimated = 800usize;
        self.total_bytes += estimated;

        let doc_index = self.docs.len();
        self.doc_id_index.insert(doc_id.clone(), doc_index);
        self.docs.push(MemEntry {
            seq_no,
            doc_id,
            source: Arc::new(Value::Null),
            source_bytes,
            size_bytes: estimated,
        });
    }

    pub fn insert_pretokenized(
        &mut self,
        doc_id: String,
        source: Arc<Value>,
        tokens: &[String],
    ) {
        // `seq_no = 0` means "unknown" — drain_with_sources falls back
        // to insertion order when all entries share seq_no 0.  Tests
        // and the legacy single-doc path (which never carries a WAL
        // seq_no through) land here.
        self.insert_pretokenized_inner(0, doc_id, source, tokens);
    }

    fn insert_pretokenized_inner(
        &mut self,
        seq_no: u64,
        doc_id: String,
        source: Arc<Value>,
        _tokens: &[String],
    ) {
        // M5.6 — ULTRA-TURBO hot path.
        //
        // Pre-M5.6 this function did, for each doc:
        //   - Full tree walk to extract text_fields (HashMap<String,String>)
        //   - Per-field token-split for length counting
        //   - Per-field `entry(field.to_string())` × 2 HashMap inserts
        //     (field_lengths + avg_field_lengths)  → 2-4 allocs/field
        //   - `doc_values.push(source, idx)` → another tree walk + per
        //     field HashMap entry + String clones for keyword columns
        //   - `doc_id_index.insert(doc_id.clone(), ...)`
        //   - docs.push(MemEntry { .. })
        //
        // Measured: ~7 µs per doc → 35 ms per 5000-doc batch = ~145 k
        // docs/s per lock-holder.  Lock contention across 32 workers
        // capped us at ~380 k/s.
        //
        // The only work that MUST happen on the hot path is:
        //   1. Push to docs Vec (for drainability at flush)
        //   2. Update doc_id_index (for remove() support in next batch)
        //   3. Rough size accounting (for back-pressure)
        //
        // field_lengths / avg_field_lengths / doc_values are all
        // **reconstructable** from `docs[].source` at flush time.  We
        // skip them on the hot path entirely and rebuild them lazily
        // from the Arc<Value> in `drain_for_flush` / search-time
        // fallbacks.
        //
        // Rough size estimate: 800 bytes/doc is a reasonable proxy
        // for log data and keeps back-pressure within 2× of truth.
        let estimated = 800usize;
        self.total_bytes += estimated;

        let doc_index = self.docs.len();
        self.doc_id_index.insert(doc_id.clone(), doc_index);

        self.docs.push(MemEntry {
            seq_no,
            doc_id,
            source,
            source_bytes: Arc::from(&[][..]),
            size_bytes: estimated,
        });
    }

    /// Zero-clone turbo insert — takes an `Arc<Value>` already shared with the
    /// WAL serialisation step.  The Arc pointer is bumped (8 bytes), not the
    /// entire JSON tree.  Builds the inverted index so match queries work.
    pub fn insert_pretokenized_arc(
        &mut self,
        doc_id: String,
        source: Arc<Value>,
        _tokens: &[String],
    ) {
        // Build text_fields by reading through the Arc — read-only, no clone.
        let mut text_fields: HashMap<String, String> = HashMap::new();
        if let Some(obj) = source.as_object() {
            for (key, val) in obj {
                let text = extract_text_value(val);
                if !text.is_empty() {
                    text_fields.insert(key.clone(), text);
                }
            }
        }

        // Build the inverted index entries so match queries work correctly.
        let analyzer = self
            .registry
            .get_analyzer("default")
            .or_else(|| self.registry.get_analyzer("standard"))
            .expect("standard analyzer always present");

        for (field_name, text) in &text_fields {
            let tokens = analyzer.analyze(text);
            let token_count = tokens.len() as u32;

            // Cache the field length for BM25 scoring.
            self.field_lengths
                .entry(field_name.clone())
                .or_default()
                .insert(doc_id.clone(), token_count);

            // Update running average.
            let entry = self.avg_field_lengths.entry(field_name.clone()).or_insert((0.0, 0));
            entry.0 += token_count as f64;
            entry.1 += 1;

            let field_index = self.index.entry(field_name.clone()).or_default();
            for token in &tokens {
                let posting = field_index.entry(token.text.clone()).or_default();
                *posting.entry(doc_id.clone()).or_insert(0) += 1;
            }
        }

        // See `insert_pretokenized` for the sizing rationale — 3× the raw
        // text-field byte count covers inverted index + Arc + map overhead.
        let raw_size = source.to_string().len() + doc_id.len();
        let size = raw_size * 3 + 64;
        self.total_bytes += size;

        let doc_index = self.docs.len();
        // Pass a reference through the Arc — DocValues reads without cloning source.
        self.doc_values.push(&source, doc_index);

        // Track doc_id → index for O(1) lookup.
        self.doc_id_index.insert(doc_id.clone(), doc_index);

        self.docs.push(MemEntry {
            seq_no: 0,
            doc_id,
            // Arc clone = atomic refcount increment (cheap pointer copy).
            source,
            source_bytes: Arc::from(&[][..]),
            size_bytes: size,
        });
        let _ = text_fields; // consumed above by inverted-index build
    }

    /// Remove a document from the memtable (for deletes/updates).
    ///
    /// Fast exit when the doc isn't present — this is critical on the
    /// turbo ingest hot path, which calls `remove()` before every
    /// `insert_pretokenized` to guard against the "reindex existing doc"
    /// case.  For the much-more-common "append new doc" case the doc
    /// isn't in `doc_id_index`, so we can skip the O(fields × terms)
    /// inverted-index sweep that used to run unconditionally.  That
    /// sweep was the dominant single cost in the ingest hot path at
    /// 30 k+-doc memtables, turning 5 k-doc bulks into 2 500 ms of work.
    pub fn remove(&mut self, doc_id: &str) {
        // Cheap check first — if the doc isn't tracked, none of the
        // parallel structures contain it either (they're all updated
        // together inside `insert_pretokenized`).
        if !self.doc_id_index.contains_key(doc_id) {
            return;
        }
        // Remove from inverted index.  Capture every (field, term) this doc
        // actually contributed to so we can preserve its delete-aware
        // doc_freq contribution (Lucene keeps tombstoned postings counted
        // until a merge).  Two-phase to avoid borrowing `self.index` and
        // `self.ghost_doc_freq` mutably at once.
        let mut ghosted_terms: Vec<(String, String)> = Vec::new();
        for (field_name, field_index) in self.index.iter_mut() {
            for (term, posting_list) in field_index.iter_mut() {
                if posting_list.remove(doc_id).is_some() {
                    ghosted_terms.push((field_name.clone(), term.clone()));
                }
            }
        }
        for (field_name, term) in ghosted_terms {
            *self
                .ghost_doc_freq
                .entry(field_name)
                .or_default()
                .entry(term)
                .or_insert(0) += 1;
        }
        // Remove from field length caches and update running averages.
        // Retain the removed doc's field-length contribution as a ghost so
        // avgdl stays delete-aware (total_field_length / N counts tombstones).
        let mut ghosted_lengths: Vec<(String, u32)> = Vec::new();
        for (field_name, lengths) in &mut self.field_lengths {
            if let Some(token_count) = lengths.remove(doc_id) {
                if let Some(entry) = self.avg_field_lengths.get_mut(field_name) {
                    entry.0 -= token_count as f64;
                    entry.1 = entry.1.saturating_sub(1);
                }
                ghosted_lengths.push((field_name.clone(), token_count));
            }
        }
        for (field_name, token_count) in ghosted_lengths {
            let g = self.ghost_field_len.entry(field_name).or_insert((0.0, 0));
            g.0 += token_count as f64;
            g.1 += 1;
        }
        // One more document is now a tombstone for collection-stats purposes.
        self.ghost_docs += 1;
        // Remove from docs list AND the parallel DocValues columns.
        if let Some(pos) = self.doc_id_index.remove(doc_id) {
            let entry = self.docs.remove(pos);
            self.total_bytes = self.total_bytes.saturating_sub(entry.size_bytes);
            self.doc_values.remove_at(pos);
            // Shift all indices above pos down by 1.
            for idx in self.doc_id_index.values_mut() {
                if *idx > pos {
                    *idx -= 1;
                }
            }
        }
    }

    /// Search the memtable using a simple term-based BM25 query.
    ///
    /// Tokenizes the query string and scores documents using BM25.
    /// If a "default" analyzer is registered (e.g. with synonym expansion),
    /// it is used; otherwise falls back to "standard".
    pub fn search_text(&self, query: &str, fields: &[&str], limit: usize) -> Vec<MemtableHit> {
        // Local-stats variant — preserved for direct callers + tests
        // that don't care about cross-shard correctness. The
        // FtsMemtable orchestrator uses search_text_with_global_stats
        // instead so BM25 reflects the union.
        self.search_text_with_global_stats(
            query, fields, limit, 0,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
        )
    }

    /// search_text variant that uses caller-supplied GLOBAL doc_count,
    /// per-field avg lengths, and per-(field,term) doc frequencies.
    /// Falls back to local stats when the global maps are empty.
    pub fn search_text_with_global_stats(
        &self,
        query: &str,
        fields: &[&str],
        limit: usize,
        global_doc_count: u64,
        global_avg_field_len: &std::collections::HashMap<String, f32>,
        global_term_df: &std::collections::HashMap<(String, String), u64>,
    ) -> Vec<MemtableHit> {
        let analyzer = match self
            .registry
            .get_analyzer("default")
            .or_else(|| self.registry.get_analyzer("standard"))
        {
            Some(a) => a,
            None => return Vec::new(),
        };

        let tokens = analyzer.analyze(query);
        if tokens.is_empty() {
            return Vec::new();
        }

        let doc_count = if global_doc_count > 0 { global_doc_count } else { self.docs.len() as u64 };
        let mut scores: HashMap<String, f32> = HashMap::new();

        for token in &tokens {
            // Search across requested fields (or all indexed fields if none specified).
            let search_fields: Vec<&str> = if fields.is_empty() {
                self.index.keys().map(|s| s.as_str()).collect()
            } else {
                fields.to_vec()
            };

            for field_name in &search_fields {
                let field_index = match self.index.get(*field_name) {
                    Some(fi) => fi,
                    None => continue,
                };

                let posting_list = match field_index.get(&token.text) {
                    Some(pl) => pl,
                    None => continue,
                };

                // Prefer the caller-supplied global doc_freq; fall back
                // to local posting list size if absent.
                let doc_freq: u64 = global_term_df
                    .get(&(field_name.to_string(), token.text.clone()))
                    .copied()
                    .unwrap_or_else(|| posting_list.len() as u64);
                let avg_field_len = global_avg_field_len
                    .get(*field_name)
                    .copied()
                    .unwrap_or_else(|| self.avg_field_length(field_name));

                let scorer = Bm25Scorer::new(avg_field_len, doc_count);

                for (doc_id, &tf) in posting_list {
                    let field_len = self
                        .field_lengths
                        .get(*field_name)
                        .and_then(|m| m.get(doc_id))
                        .copied()
                        .unwrap_or(1);

                    let score =
                        scorer.score_term(doc_freq, tf, field_len);
                    *scores.entry(doc_id.clone()).or_insert(0.0) += score;
                }
            }
        }

        let mut hits: Vec<MemtableHit> = scores
            .into_iter()
            .map(|(doc_id, score)| MemtableHit { doc_id, score })
            .collect();

        // Sort by score descending.
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(limit);
        hits
    }

    /// Look up a document's indexed text fields by ID (used for BM25 scoring).
    ///
    /// Post-M4.9 this is computed lazily from `e.source` — we no
    /// longer eagerly cache a per-doc `HashMap<String, String>` at
    /// ingest time.  Query-time cost is one tree walk + ~10
    /// allocations, which is fine for the sub-second SLAs xerj
    /// targets on memtable-FTS fallback queries.
    pub fn get_source(&self, doc_id: &str) -> Option<HashMap<String, String>> {
        self.docs
            .iter()
            .find(|e| e.doc_id == doc_id)
            .map(|e| extract_text_fields_from(e.source.as_ref()))
    }

    /// Return all doc IDs stored in the memtable (in insertion order).
    pub fn all_doc_ids(&self) -> Vec<String> {
        self.docs.iter().map(|e| e.doc_id.clone()).collect()
    }

    /// Resolve a MemEntry's source Value — if `source` is Null but
    /// `source_bytes` is non-empty, lazily parse the bytes.  This is
    /// the M5.11 deferred-parse path used by `insert_raw_bytes_with_seq`.
    fn resolve_source(entry: &MemEntry) -> Value {
        if entry.source.is_null() && !entry.source_bytes.is_empty() {
            serde_json::from_slice(&entry.source_bytes).unwrap_or(Value::Null)
        } else {
            (*entry.source).clone()
        }
    }

    /// Return the full original source JSON for a document by ID.
    pub fn get_doc_source_as_value(&self, doc_id: &str) -> Option<Value> {
        self.docs
            .iter()
            .find(|e| e.doc_id == doc_id)
            .map(Self::resolve_source)
    }

    /// Return the Arc-wrapped source for a document by ID.
    pub fn get_doc_source_arc(&self, doc_id: &str) -> Option<Arc<Value>> {
        self.docs
            .iter()
            .find(|e| e.doc_id == doc_id)
            .map(|e| {
                if e.source.is_null() && !e.source_bytes.is_empty() {
                    Arc::new(serde_json::from_slice(&e.source_bytes).unwrap_or(Value::Null))
                } else {
                    Arc::clone(&e.source)
                }
            })
    }

    /// Iterate all stored documents as (doc_id, original_source) pairs.
    /// Lazily parses M5.11 raw-bytes entries.
    pub fn all_docs(&self) -> Vec<(String, Value)> {
        self.docs
            .iter()
            .map(|e| (e.doc_id.clone(), Self::resolve_source(e)))
            .collect()
    }

    /// Collect all docs as owned (doc_id, source) pairs for predicate-based scan.
    pub fn all_docs_with_sources(&self) -> Vec<(String, Value)> {
        self.docs
            .iter()
            .map(|e| (e.doc_id.clone(), Self::resolve_source(e)))
            .collect()
    }

    /// Check if a document exists in the memtable.
    pub fn contains(&self, doc_id: &str) -> bool {
        self.docs.iter().any(|e| e.doc_id == doc_id)
    }

    /// Number of documents in the memtable.
    pub fn doc_count(&self) -> usize {
        self.docs.len()
    }

    /// Approximate total byte size of buffered data.
    pub fn size_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Drain all documents (for flush).
    ///
    /// NOTE: uses `= HashMap::new()` (not `.clear()`) so the HashMap backing
    /// storage is actually released back to the allocator.  `HashMap::clear`
    /// keeps the internal bucket capacity around for reuse, which for a
    /// log-ingest workload means memory never shrinks after a flush —
    /// RSS grows monotonically until OOM.  See CAPPED_RAM_BATTLE for the bug.
    pub fn drain(&mut self) -> Vec<(String, HashMap<String, String>)> {
        // Sort by seq_no — see `drain_with_sources` for the rationale.
        let mut drained: Vec<MemEntry> = self.docs.drain(..).collect();
        drained.sort_by_key(|e| e.seq_no);
        let result = drained
            .into_iter()
            .map(|e| {
                let fields = extract_text_fields_from(e.source.as_ref());
                (e.doc_id, fields)
            })
            .collect();
        self.index = HashMap::new();
        self.doc_values = DocValues::default();
        self.total_bytes = 0;
        self.field_lengths = HashMap::new();
        self.avg_field_lengths = HashMap::new();
        self.doc_id_index = HashMap::new();
        // Flush is the equivalent of a Lucene merge: tombstone contributions
        // are purged, so delete-aware ghost statistics reset to empty.
        self.ghost_docs = 0;
        self.ghost_field_len = HashMap::new();
        self.ghost_doc_freq = HashMap::new();
        // `self.docs` was drained in place; its Vec backing is kept (cheap).
        self.docs.shrink_to_fit();
        result
    }

    /// Return a snapshot of the current analyzed field encodings.
    ///
    /// Each entry represents a field whose sample threshold was reached and
    /// whose optimal encoding was determined by `FieldAnalyzer`.
    pub fn analyzed_field_encodings(&self) -> &HashMap<String, FieldEncoding> {
        &self.doc_values.analyzed_encodings
    }

    /// Drain all documents with their original source values (for flush to segment).
    ///
    /// Returns `(doc_id, text_fields, source)` in insertion order.
    pub fn drain_with_sources(&mut self) -> Vec<(String, HashMap<String, String>, Value)> {
        // M5.0 — sort drained docs by global WAL seq_no before
        // returning so the downstream FTS / stored-section writers
        // produce matching ordinals.  Out-of-order in-memory pushes
        // (introduced when we lift the WAL append out of the engine
        // memtable write lock so concurrent bulk batches pipeline)
        // get canonicalised here.  For workloads where all entries
        // carry `seq_no = 0` (the legacy `insert` path), `sort_by_key`
        // is stable so existing insertion order is preserved.
        let mut drained: Vec<MemEntry> = self.docs.drain(..).collect();
        drained.sort_by_key(|e| e.seq_no);
        let result = drained
            .into_iter()
            .map(|e| {
                let val = Arc::try_unwrap(e.source).unwrap_or_else(|arc| (*arc).clone());
                let fields = extract_text_fields_from(&val);
                (e.doc_id, fields, val)
            })
            .collect();
        // See `drain()` for why we must reassign these (not `.clear()`):
        // `HashMap::clear` retains internal bucket capacity and the RSS
        // never shrinks between flushes.
        self.index = HashMap::new();
        self.doc_values = DocValues::default();
        self.total_bytes = 0;
        self.field_lengths = HashMap::new();
        self.avg_field_lengths = HashMap::new();
        self.doc_id_index = HashMap::new();
        // Flush is the equivalent of a Lucene merge: tombstone contributions
        // are purged, so delete-aware ghost statistics reset to empty.
        self.ghost_docs = 0;
        self.ghost_field_len = HashMap::new();
        self.ghost_doc_freq = HashMap::new();
        self.docs.shrink_to_fit();
        result
    }

    /// Return the source Value for a document by its internal index (O(1)).
    ///
    /// Unlike `get_doc_source_as_value` this uses the pre-known index position
    /// instead of scanning by doc_id, making it O(1) rather than O(N).
    pub fn get_doc_source_by_index(&self, idx: usize) -> Option<Value> {
        self.docs.get(idx).map(|e| (*e.source).clone())
    }

    /// Return a reference to the numeric DocValues column for `field`.
    ///
    /// Each slot corresponds to a document in insertion order (parallel to `docs`).
    /// `None` slots mean the document had no value for this field.
    pub fn doc_values_numeric_column(&self, field: &str) -> Option<&[Option<f64>]> {
        self.doc_values.numeric.get(field).map(|v| v.as_slice())
    }

    /// Return a reference to the keyword DocValues column for `field`.
    pub fn doc_values_keyword_column(&self, field: &str) -> Option<&[Option<String>]> {
        self.doc_values.keyword.get(field).map(|v| v.as_slice())
    }

    /// O(1) memtable count for `field == value` over the keyword column.
    /// Backed by a lazily-built `HashMap<String, u32>` per field —
    /// skips the linear column scan entirely once the count map is
    /// built (first query post-ingest triggers the one-time O(n)
    /// rebuild; subsequent queries are O(1)).
    pub fn doc_values_keyword_count(&mut self, field: &str, value: &str) -> Option<u32> {
        self.doc_values.ensure_counts_built();
        self.doc_values.keyword_counts.get(field).map(|m| m.get(value).copied().unwrap_or(0))
    }

    /// O(1) memtable count for `field == value` over the numeric column.
    pub fn doc_values_numeric_count(&mut self, field: &str, value: f64) -> Option<u32> {
        self.doc_values.ensure_counts_built();
        self.doc_values
            .numeric_counts
            .get(field)
            .map(|m| m.get(&value.to_bits()).copied().unwrap_or(0))
    }

    // ── Fast DocValues queries ────────────────────────────────────────────────

    /// Fast term query using the keyword column — O(N * string_compare).
    ///
    /// Returns `(doc_id, doc_index)` pairs for documents where the keyword
    /// column for `field` equals `value` (case-sensitive exact match).
    ///
    /// Returns `None` when the field has no keyword column (fall back to JSON scan).
    pub fn doc_values_term_query(&self, field: &str, value: &str) -> Option<Vec<(String, usize)>> {
        let col = self.doc_values.keyword.get(field)?;
        // Same text-field bailout as doc_values_terms_query: a whitespace-
        // containing keyword value means the column stores the full
        // analyzed-text source; a `term` query expects a token match which
        // only the FTS / scan path can serve.
        if col.iter().any(|v| v.as_deref().map(|s| s.contains(char::is_whitespace)).unwrap_or(false)) {
            return None;
        }
        let results = col
            .iter()
            .enumerate()
            .filter_map(|(idx, opt)| {
                if opt.as_deref() == Some(value) {
                    Some((self.docs[idx].doc_id.clone(), idx))
                } else {
                    None
                }
            })
            .collect();
        Some(results)
    }

    /// Index-only variant of `doc_values_term_query` that returns only
    /// the matching memtable positions, no cloned doc-id Strings. Used
    /// by the bool intersection shortcut to avoid 170k String allocations
    /// per query.
    pub fn doc_values_term_indices(&self, field: &str, value: &str) -> Option<Vec<usize>> {
        let col = self.doc_values.keyword.get(field)?;
        let results: Vec<usize> = col
            .iter()
            .enumerate()
            .filter_map(|(idx, opt)| {
                if opt.as_deref() == Some(value) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect();
        Some(results)
    }

    /// Index-only variant of `doc_values_range_query`.
    pub fn doc_values_range_indices(
        &self,
        field: &str,
        gte: Option<f64>,
        gt: Option<f64>,
        lte: Option<f64>,
        lt: Option<f64>,
    ) -> Option<Vec<usize>> {
        let col = self.doc_values.numeric.get(field)?;
        let results: Vec<usize> = col
            .iter()
            .enumerate()
            .filter_map(|(idx, opt)| {
                let v = (*opt)?;
                let pl = match (gte, gt) {
                    (Some(b), _) => v >= b,
                    (None, Some(b)) => v > b,
                    (None, None) => true,
                };
                let pu = match (lte, lt) {
                    (Some(b), _) => v <= b,
                    (None, Some(b)) => v < b,
                    (None, None) => true,
                };
                if pl && pu { Some(idx) } else { None }
            })
            .collect();
        Some(results)
    }

    /// Fast terms query (multi-value term filter) using the keyword column.
    ///
    /// Returns `(doc_id, doc_index)` pairs for documents where the keyword
    /// column for `field` matches any of the given `values`.
    ///
    /// Returns `None` when the field has no keyword column (fall back to JSON scan).
    pub fn doc_values_terms_query(
        &self,
        field: &str,
        values: &[String],
    ) -> Option<Vec<(String, usize)>> {
        let col = self.doc_values.keyword.get(field)?;
        // If any stored keyword value in this column contains whitespace
        // it's likely an analyzed text field whose doc-values were built
        // from the full source string (not the token stream). A `terms`
        // query compares against tokens in that case, which doc-values
        // can't serve — bail so callers fall through to the scan path.
        if col.iter().any(|v| v.as_deref().map(|s| s.contains(char::is_whitespace)).unwrap_or(false)) {
            return None;
        }
        let results: Vec<(String, usize)> = col
            .iter()
            .enumerate()
            .filter_map(|(idx, opt)| {
                if let Some(v) = opt.as_deref() {
                    if values.iter().any(|qv| qv == v) {
                        return Some((self.docs[idx].doc_id.clone(), idx));
                    }
                }
                None
            })
            .collect();
        Some(results)
    }

    /// Fast range query using the numeric column — O(N * f64_compare).
    ///
    /// Parameters mirror the ES range query: `gte` / `gt` / `lte` / `lt`.
    ///
    /// Returns `None` when the field has no numeric column (fall back to JSON scan).
    pub fn doc_values_range_query(
        &self,
        field: &str,
        gte: Option<f64>,
        gt: Option<f64>,
        lte: Option<f64>,
        lt: Option<f64>,
    ) -> Option<Vec<(String, usize)>> {
        let col = self.doc_values.numeric.get(field)?;
        let results = col
            .iter()
            .enumerate()
            .filter_map(|(idx, opt)| {
                let v = (*opt)?;
                let passes_lower = match (gte, gt) {
                    (Some(b), _) => v >= b,
                    (None, Some(b)) => v > b,
                    (None, None) => true,
                };
                let passes_upper = match (lte, lt) {
                    (Some(b), _) => v <= b,
                    (None, Some(b)) => v < b,
                    (None, None) => true,
                };
                if passes_lower && passes_upper {
                    Some((self.docs[idx].doc_id.clone(), idx))
                } else {
                    None
                }
            })
            .collect();
        Some(results)
    }

    /// Return all (term, frequency) pairs for a given field from the inverted index.
    ///
    /// Used by the term suggester to find candidate terms via edit distance, and
    /// by the completion suggester for prefix-based autocomplete over indexed terms.
    pub fn all_terms_for_field(&self, field: &str) -> Vec<(String, usize)> {
        match self.index.get(field) {
            Some(postings) => postings
                .iter()
                .map(|(term, docs)| (term.clone(), docs.len()))
                .collect(),
            None => Vec::new(),
        }
    }

    /// Return all distinct keyword values for a field (from the DocValues keyword column).
    ///
    /// Used by the completion suggester for fast prefix autocomplete on keyword fields.
    pub fn all_keyword_values_for_field(&self, field: &str) -> Vec<(String, usize)> {
        match self.doc_values.keyword_set.get(field) {
            Some(set) => set.iter().map(|v| (v.clone(), 1)).collect(),
            None => Vec::new(),
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn avg_field_length(&self, field_name: &str) -> f32 {
        match self.avg_field_lengths.get(field_name) {
            Some(&(total, count)) if count > 0 => (total / count as f64) as f32,
            _ => 1.0,
        }
    }
}

impl Default for FtsMemtable {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extract a string value from a JSON value for text indexing.
fn extract_text_value(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(arr) => arr
            .iter()
            .map(|v| extract_text_value(v))
            .collect::<Vec<_>>()
            .join(" "),
        Value::Object(_) => serde_json::to_string(val).unwrap_or_default(),
        Value::Null => String::new(),
    }
}
