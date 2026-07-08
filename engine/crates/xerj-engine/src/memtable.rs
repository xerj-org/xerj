//! In-memory inverted index for unflushed documents.
//!
//! The FTS memtable provides fast search over recently ingested documents
//! that have not yet been flushed to a segment on disk.  It uses a simple
//! HashMap-based inverted index with BM25 scoring, plus a columnar DocValues
//! store for O(N) term/range/agg queries without JSON parsing per document.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;
use xerj_common::types::{FieldType, Schema};
use xerj_compress::field_codec::{FieldAnalyzer, FieldEncoding};
use xerj_fts::analyzer::{AnalyzerPipeline, AnalyzerRegistry, Token};
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
    /// M5.18 — read-side memo for the M5.11 deferred parse.  When `source`
    /// is `Null` and `source_bytes` is non-empty, the FIRST read that needs
    /// the parsed `Value` (`resolve_source*`, `get_doc_source_arc`) parses
    /// the bytes once and caches the resulting `Arc<Value>` here; every
    /// subsequent read/scan is an `Arc::clone` instead of a per-doc
    /// `serde_json::from_slice`.  Under the mixed read-under-write cluster a
    /// range/bool scan walks the whole buffered memtable, so without this
    /// memo each query re-parsed ~10⁵–10⁶ raw-bytes docs (live WARN:
    /// mem_admit=2151 ms spike on one range read).  Inline `OnceLock` →
    /// `OnceLock::new()` is a const, so the ingest hot path pays NO extra
    /// heap allocation; the cell only allocates a `Value` when actually
    /// read.  Empty (never initialised) for eager entries where `source`
    /// already holds the parsed tree.
    parsed_memo: OnceLock<Arc<Value>>,
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

/// Interned document identifier.
///
/// One `Arc<str>` is allocated per document at insert time and its
/// pointer is cloned (a relaxed refcount bump, no `malloc`) into every
/// doc-id-keyed map — the postings lists, `field_lengths`, and
/// `doc_id_index`.  Pre-interning, `insert_analyzed` paid a fresh
/// `String` heap allocation of the doc id for EVERY (doc × token) it
/// pushed into a posting list plus one per (doc × field) length entry —
/// profiled as the dominant remaining under-write-lock cost on the
/// 1M×c8 bulk ceiling (~40 ms / 10k-doc batch).  Interning collapses
/// that to a single allocation per doc; the per-token work becomes a
/// pointer bump inside the shard critical section.
///
/// `Arc<str>: Borrow<str>` and its `Hash`/`Eq` forward to the pointed-to
/// `str`, so every existing lookup by `&str` (`remove`, `contains_key`,
/// BM25 `field_lengths` probe) is unchanged and byte-for-byte
/// identical to the `String`-keyed maps it replaces.
type DocId = Arc<str>;

/// Posting list entry: doc_id → term frequency.
///
/// FxHashMap (rustc-hash) instead of the SipHash-keyed std HashMap:
/// perf profiling of the c8 bulk-ingest ceiling showed >21% of on-CPU
/// time inside `core::hash::sip` + `hash_one` for the nested posting
/// merges — all executed under the memtable shard write locks.  These
/// maps are never exposed to untrusted-key HashDoS (doc ids and field
/// names of a single tenant's own documents), so the DoS-resistant
/// hasher buys nothing here.
type PostingList = FxHashMap<DocId, u32>; // doc_id → tf

// ── DocValues ─────────────────────────────────────────────────────────────────

/// Number of raw string samples to collect per field before running
/// smart encoding analysis.
// Intentional scaffolding for the deferred field-encoding-analysis subsystem
// (`collect_sample` + the `pub` samples/analyzed/analyzed_encodings columns); the
// bounded-delta read-side fold superseded its ingest-time call site, but the writer
// half and its public columns are retained, so silence dead_code.
#[allow(dead_code)]
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
    pub numeric: FxHashMap<String, Vec<Option<f64>>>,
    /// field → per-doc keyword value (for Keyword, IP, and any string field).
    pub keyword: FxHashMap<String, Vec<Option<String>>>,
    /// Bounded-delta maintained per-value counts, distinct-value sets, and
    /// sorted-numeric range indexes for the columns above.  Kept behind a
    /// `Mutex` (interior mutability) so the query path folds only the
    /// column positions appended since the last query — O(delta), not
    /// O(memtable) — under the shard READ lock, instead of the old
    /// `counts_dirty` full-rebuild under an all-shard WRITE lock.  That
    /// full-rebuild + write-lock serialisation WAS the residual
    /// terms/cardinality/range/bool read-under-write p99 term.  See
    /// [`CountState`] and [`DocValues::with_keyword_field`].
    ///
    /// The ingest hot path (`push_field`) does NOT touch this — it only
    /// appends to the raw columns, so per-doc ingest cost is unchanged;
    /// the fold happens lazily on the read side over just the delta.
    pub counts: parking_lot::Mutex<CountState>,
    /// Analyzed field encodings — built lazily after sufficient samples.
    pub analyzed_encodings: HashMap<String, FieldEncoding>,
    /// Raw string samples per field for deferred analysis (cleared after analysis).
    pub samples: HashMap<String, Vec<String>>,
    /// Whether analysis has been performed for each field.
    pub analyzed: HashMap<String, bool>,
    /// Fields that have EVER carried an array value in this memtable.
    /// The keyword column stores only an array's FIRST scalar, so any
    /// columnar fast path whose brute twin fans out over every element
    /// (terms/cardinality aggs) must bail for these fields.  Never
    /// removed on delete — conservative (a stale flag only costs the
    /// fast path, not correctness).
    pub array_fields: FxHashSet<String>,
    /// Fields whose keyword column has EVER contained a whitespace-bearing
    /// value.  Such a column stores an analyzed-text field's full source
    /// string verbatim; the `term` / `terms` / fused-`bool` doc-values fast
    /// paths compare against a single token, which can never equal a
    /// multi-token value, so they MUST bail to the scan path for these
    /// fields.  Computed ONCE at insert time (see `push_field`) instead of
    /// the old O(N) `col.iter().any(is_whitespace)` prescan every query ran.
    /// Never cleared on delete — conservative (a stale flag only sends the
    /// query down the always-correct scan path, it can't change results),
    /// mirroring `array_fields`.
    pub keyword_has_whitespace: FxHashSet<String>,
    /// Incremental per-(field, order) sorted-candidate cache for the
    /// field-sorted match_all memtable arm — see
    /// `FtsMemtable::sort_candidates_cached`.  Keyed
    /// `"{field}\u{1}{asc|desc}"`.  Reset with the rest of `DocValues`
    /// at drain; cleared by `remove_at` (positions shift).
    ///
    /// Step 4: behind a `Mutex` so the sorted-`match_all` fast path can
    /// extend the cache under the shard's READ lock instead of forcing a
    /// write lock across all 16 shards (which serialised every sorted read
    /// against concurrent ingest — the residual `match_all` read-under-write
    /// p99 term).  The shard write lock is exclusive, so `push_field` /
    /// `remove_at` mutate the columns with no concurrent reader; two
    /// concurrent readers under the shard read lock serialise briefly on
    /// this leaf mutex while one folds in the new docs.
    pub sort_cand_cache: parking_lot::Mutex<FxHashMap<String, SortCandCache>>,
}

/// State of one incremental per-shard sorted-candidate extraction: the
/// shard's top-`cap` (+boundary ties) by sort key plus up to `cap`
/// missing-key positions, extended by O(new docs) per query instead of
/// re-walking the whole shard (the O(memtable) epoch-memo walk was
/// 15-30 ms/query at a drain-lagged 300-500 k-doc memtable — the
/// residual match_all read-under-write p99 term).
pub struct SortCandCache {
    /// Docs positions `[0, seen_docs)` are already folded in.
    pub seen_docs: usize,
    /// The cap this cache was built for; a larger request rebuilds.
    pub cap: usize,
    /// Candidate pool: (key, docs index).  Kept cut to ~2×cap between
    /// queries (page-order sort + boundary-tie truncate on overflow).
    pub top: Vec<(f64, usize)>,
    /// Up to `cap` positions missing the sort key.
    pub missing: Vec<usize>,
    /// A value failed to normalise (or the field carried arrays) — the
    /// shard is ineligible until the next drain resets the memtable.
    pub poisoned: bool,
}

/// Amortised sorted numeric index for O(log n + tail) memtable range
/// COUNTs.  A sorted run plus a bounded unsorted `tail`; the tail is
/// merged into the run once it grows past `TAIL_MERGE`, so a range count
/// is `partition_point` bisects over the run plus a linear scan of the
/// (≤ `TAIL_MERGE`) tail — never an O(memtable) column walk.  The merge
/// is O(run+tail) but amortised across `TAIL_MERGE` appends, and it runs
/// on the READ side (bounded-delta fold), never on the ingest hot path.
/// Bounded-delta maintained aggregation state for a [`DocValues`] store:
/// per-value keyword counts, distinct-value sets (exact cardinality), and
/// numeric-value counts.  Folded incrementally + **PER FIELD** on the read
/// side — `kw_built`/`num_built` record how many leading positions of each
/// field's column are already reflected, so a query folds only the newly-
/// appended tail of the ONE field it touches (O(delta)), never the whole
/// memtable and never other fields' columns.
///
/// Per-field folding matters: a `terms`/`cardinality` query on a low-
/// cardinality field (`model`, `status`) must NOT pay to fold a high-
/// cardinality sibling column (`cost_usd` doubles stringified into the
/// keyword store) — folding everything on every query regressed the very
/// cells this exists to serve.
#[derive(Default)]
pub struct CountState {
    /// per keyword-field: column positions already folded into the maps.
    kw_built: FxHashMap<String, usize>,
    /// per numeric-field: column positions already folded into numeric_counts.
    num_built: FxHashMap<String, usize>,
    /// field → distinct keyword value → live doc count.
    pub keyword_counts: FxHashMap<String, FxHashMap<String, u32>>,
    /// field → numeric-bits → live doc count.
    pub numeric_counts: FxHashMap<String, FxHashMap<u64, u32>>,
    /// field → set of distinct keyword values (exact cardinality).
    pub keyword_set: FxHashMap<String, FxHashSet<String>>,
}

impl DocValues {
    /// Fold the keyword column for ONE `field` up to date (counts + set).
    /// O(positions appended to that field's column since the last fold).
    fn fold_keyword_field(&self, cs: &mut CountState, field: &str) {
        let Some(col) = self.keyword.get(field) else {
            return;
        };
        let built = cs.kw_built.get(field).copied().unwrap_or(0);
        if built >= col.len() {
            return;
        }
        let counts = cs.keyword_counts.entry(field.to_string()).or_default();
        let set = cs.keyword_set.entry(field.to_string()).or_default();
        for s in col[built..].iter().flatten() {
            *counts.entry(s.clone()).or_insert(0) += 1;
            if !set.contains(s) {
                set.insert(s.clone());
            }
        }
        cs.kw_built.insert(field.to_string(), col.len());
    }

    /// Fold the numeric column for ONE `field` up to date (numeric_counts).
    fn fold_numeric_field(&self, cs: &mut CountState, field: &str) {
        let Some(col) = self.numeric.get(field) else {
            return;
        };
        let built = cs.num_built.get(field).copied().unwrap_or(0);
        if built >= col.len() {
            return;
        }
        let counts = cs.numeric_counts.entry(field.to_string()).or_default();
        for f in col[built..].iter().flatten() {
            *counts.entry(f.to_bits()).or_insert(0) += 1;
        }
        cs.num_built.insert(field.to_string(), col.len());
    }

    /// Fold ONE keyword field's maps up to date, then run `f` with a
    /// read-only borrow of the shared `CountState`.  Takes only `&self`
    /// (interior mutability via the `counts` Mutex), so the query path
    /// holds the shard READ lock — never the all-shard WRITE lock the old
    /// full-rebuild `&mut self` count path forced.
    pub fn with_keyword_field<R>(&self, field: &str, f: impl FnOnce(&CountState) -> R) -> R {
        let mut cs = self.counts.lock();
        self.fold_keyword_field(&mut cs, field);
        f(&cs)
    }

    /// Fold ONE numeric field's counts up to date, then run `f`.
    pub fn with_numeric_field<R>(&self, field: &str, f: impl FnOnce(&CountState) -> R) -> R {
        let mut cs = self.counts.lock();
        self.fold_numeric_field(&mut cs, field);
        f(&cs)
    }

    /// Reset the maintained maps.  Called on delete (`remove_at`) because a
    /// positional shift invalidates the append-only watermarks; the next
    /// query re-folds the (now smaller) columns from scratch.  Deletes are
    /// rare (never on the append-only bulk path), so this is trivially
    /// correct at negligible amortised cost.
    fn reset_counts(&mut self) {
        *self.counts.get_mut() = CountState::default();
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
    // Not on any current call path: the bounded-delta read-side fold superseded the
    // ingest-time sampling, but this is the writer for the `pub` samples/analyzed/
    // analyzed_encodings scaffolding (and the sole user of `FieldAnalyzer`), so it is
    // retained intentionally rather than deleted.
    #[allow(dead_code)]
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
        // V4 M4 / bounded-delta: ingest-path push_field is reduced to the
        // minimum — raw column storage only.  The `keyword_counts`,
        // `numeric_counts`, `keyword_set`, and `samples` maps are all
        // **populated lazily** on the read side via `with_keyword_field` /
        // `with_numeric_field` (per-field bounded-delta fold over just the
        // appended tail of the touched field).  This keeps the
        // hot ingest path at ~1 Vec push per field — no per-doc HashMap
        // churn — so ingest throughput is unchanged.
        // Column keys (`field`) recur across every doc, so `entry_no_clone`
        // skips the per-doc `field.to_string()` allocation on the hot path.
        match val {
            Value::Number(n) => {
                let col = entry_no_clone(&mut self.numeric, field, Default::default);
                pad_to(col, doc_index);
                col.push(n.as_f64());
                // Keep the keyword column populated too for mixed-type
                // access but defer the set/count maps.
                let kcol = entry_no_clone(&mut self.keyword, field, Default::default);
                pad_to(kcol, doc_index);
                kcol.push(Some(n.to_string()));
            }
            Value::String(s) => {
                // Step 2: cache the analyzed-text eligibility flag at insert
                // time so the term/terms/bool fast paths skip the per-query
                // whole-column whitespace prescan.
                if s.contains(char::is_whitespace) && !self.keyword_has_whitespace.contains(field) {
                    self.keyword_has_whitespace.insert(field.to_string());
                }
                let kcol = entry_no_clone(&mut self.keyword, field, Default::default);
                pad_to(kcol, doc_index);
                kcol.push(Some(s.clone()));
                // If the string looks numeric, also index it in the numeric column.
                if let Ok(f) = s.parse::<f64>() {
                    let ncol = entry_no_clone(&mut self.numeric, field, Default::default);
                    pad_to(ncol, doc_index);
                    ncol.push(Some(f));
                }
            }
            Value::Bool(b) => {
                let kcol = entry_no_clone(&mut self.keyword, field, Default::default);
                pad_to(kcol, doc_index);
                kcol.push(Some(b.to_string()));
            }
            Value::Array(arr) => {
                // Flatten: store the first element (or None for empty arrays).
                // Each element is treated as a separate value for aggregations,
                // but for term/range queries we store the first scalar found.
                if !self.array_fields.contains(field) {
                    self.array_fields.insert(field.to_string());
                }
                let first_num = arr.iter().find_map(|v| v.as_f64());
                let first_str: Option<String> = arr.iter().find_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    Value::Bool(b) => Some(b.to_string()),
                    _ => None,
                });

                let ncol = entry_no_clone(&mut self.numeric, field, Default::default);
                pad_to(ncol, doc_index);
                ncol.push(first_num);

                if let Some(ref s) = first_str {
                    if s.contains(char::is_whitespace)
                        && !self.keyword_has_whitespace.contains(field)
                    {
                        self.keyword_has_whitespace.insert(field.to_string());
                    }
                    // keyword_set is maintained lazily on the read side
                    // (bounded-delta fold of the keyword column, which
                    // already receives `first_str` below).
                }
                let kcol = entry_no_clone(&mut self.keyword, field, Default::default);
                pad_to(kcol, doc_index);
                kcol.push(first_str);
            }
            Value::Null | Value::Object(_) => {
                // Push None for null/object fields so columns stay aligned.
                let ncol = entry_no_clone(&mut self.numeric, field, Default::default);
                pad_to(ncol, doc_index);
                ncol.push(None);

                let kcol = entry_no_clone(&mut self.keyword, field, Default::default);
                pad_to(kcol, doc_index);
                kcol.push(None);
            }
        }
    }

    /// Remove the entry at `doc_index` from all columns (called on delete).
    fn remove_at(&mut self, doc_index: usize) {
        // Positions shift left — every cached sorted-candidate pool's
        // `(key, docs-index)` pairs go stale.  Deletes are rare on the
        // hot ingest path; a wholesale clear is simplest-correct.
        self.sort_cand_cache.lock().clear();
        // The bounded-delta count/set/range maps track leading column
        // positions by count; a positional shift invalidates the
        // watermarks, so reset and let the next read re-fold the (now
        // smaller) columns.  Keeps counts/sets/range EXACTLY equal to a
        // full recount after a delete (not an estimate).
        self.reset_counts();
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
        Self {
            shards,
            shard_mask: n - 1,
        }
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
        f(&mut g)
    }

    /// Run `f` with shared (read-only) access to a specific shard.
    pub fn with_shard<R>(&self, shard: usize, f: impl FnOnce(&FtsMemtable) -> R) -> R {
        let g = self.shards[shard].read();
        f(&g)
    }

    /// The analyzer `FtsMemtable::insert` uses for text fields: the
    /// registry's "default" pipeline when one was configured via index
    /// settings, else "standard".  Every shard shares the same registry
    /// (see `with_registry_and_shards`), so shard 0 is representative.
    /// Used by the bulk turbo path to pre-analyze docs OUTSIDE the
    /// shard write locks via [`analyze_doc`].
    pub fn default_analyzer(&self) -> Option<Arc<AnalyzerPipeline>> {
        let g = self.shards[0].read();
        g.registry
            .get_analyzer("default")
            .or_else(|| g.registry.get_analyzer("standard"))
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

    /// Bounded sorted-candidate extraction for a single-field sort
    /// (the implicit `@timestamp desc` index sort in particular).
    ///
    /// Returns `(candidate_ids, total_buffered_docs)` where the candidates
    /// are every buffered doc that could possibly reach the global
    /// top-`cap` page: the per-shard top-`cap` by sort key (boundary
    /// ties included) plus up to `cap` docs missing the field (the top-N
    /// heap's full-key comparison places them per the sort's `missing`
    /// policy).  Served INCREMENTALLY per shard via `SortCandCache` —
    /// each query folds in only the docs inserted since the last one —
    /// under the shard write lock (brief: O(new docs)).
    ///
    /// Per-doc key resolution (mirrors how the heap ranks the hit later):
    /// numeric column value when present, else the keyword column value
    /// through `normalize` (the caller passes the memoised date→epoch
    /// normaliser `compute_sort_values` uses per hit — this is what lets
    /// date-STRING `@timestamp` columns take the bounded path), else
    /// missing.
    ///
    /// `None` when any value is un-normalisable or the field has carried
    /// array values (per-element `mode` semantics need the full walk) —
    /// caller falls back to the exact full walk.
    pub fn sort_candidates_numeric(
        &self,
        field: &str,
        desc: bool,
        cap: usize,
        normalize: &dyn Fn(&str) -> Option<f64>,
    ) -> Option<(Vec<String>, u64)> {
        let cap = cap.max(1);
        let mut total: u64 = 0;
        let mut cands: Vec<(f64, String)> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        for s in &self.shards {
            // Step 4: READ lock — sort_candidates_cached now folds new docs
            // into the interior-mutable cache without an exclusive shard lock.
            let g = s.read();
            let n = g.doc_count();
            total += n as u64;
            if n == 0 {
                continue;
            }
            let (mut top, miss, _) = g.sort_candidates_cached(field, desc, cap, normalize)?;
            cands.append(&mut top);
            for m in miss {
                if missing.len() < cap {
                    missing.push(m);
                } else {
                    break;
                }
            }
        }
        // Global cut to cap (+boundary ties).
        if desc {
            cands.sort_unstable_by(|a, b| {
                b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
            });
        } else {
            cands.sort_unstable_by(|a, b| {
                a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        let mut keep = cands.len().min(cap);
        while keep < cands.len() && keep > 0 && cands[keep].0 == cands[keep - 1].0 {
            keep += 1;
        }
        let mut ids: Vec<String> = cands.into_iter().take(keep).map(|(_, id)| id).collect();
        ids.extend(missing);
        Some((ids, total))
    }

    /// Bounded variant: clone at most `limit` doc ids and return the TOTAL
    /// buffered doc count alongside.  The unsorted match_all page only
    /// needs `from+size+ε` ids, but `all_doc_ids` cloned every buffered id
    /// on every request — ~1 s per query at a 300 k-doc memtable under
    /// sustained bulk load (the `mem_admit` phase of the read-under-write
    /// breakdown).
    pub fn doc_ids_bounded(&self, limit: usize) -> (Vec<String>, u64) {
        let mut out: Vec<String> = Vec::with_capacity(limit.min(4096));
        let mut total: u64 = 0;
        for s in &self.shards {
            let g = s.read();
            total += g.doc_count() as u64;
            if out.len() < limit {
                out.extend(g.doc_ids_up_to(limit - out.len()));
            }
        }
        (out, total)
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

    /// Arc-sharing twin of `all_docs_with_sources` — no deep clones.
    pub fn all_docs_with_sources_arc(&self) -> Vec<(String, Arc<Value>)> {
        let mut out = Vec::new();
        for s in &self.shards {
            out.extend(s.read().all_docs_with_sources_arc());
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
    /// Bounded-delta: `FtsMemtable::doc_values_keyword_count` now takes
    /// `&self` (the maintained maps are folded under interior mutability),
    /// so this holds only the shard **READ** lock — concurrent term-count
    /// queries no longer serialise against each other or the writer.
    pub fn doc_values_keyword_count(&self, field: &str, value: &str) -> u32 {
        self.shards
            .iter()
            .map(|s| s.read().doc_values_keyword_count(field, value).unwrap_or(0))
            .sum()
    }

    pub fn doc_values_numeric_count(&self, field: &str, value: f64) -> u32 {
        self.shards
            .iter()
            .map(|s| s.read().doc_values_numeric_count(field, value).unwrap_or(0))
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
        static NO_BOOSTS: std::sync::OnceLock<std::collections::HashMap<String, f32>> =
            std::sync::OnceLock::new();
        self.search_text_boosted(
            query,
            fields,
            limit,
            NO_BOOSTS.get_or_init(std::collections::HashMap::new),
        )
    }

    /// `search_text` with per-field boost multipliers from the query tree
    /// (ES `boost` on match clauses / `field^N` on multi_match). Fields
    /// absent from the map score with boost 1.0.
    pub fn search_text_boosted(
        &self,
        query: &str,
        fields: &[&str],
        limit: usize,
        field_boosts: &std::collections::HashMap<String, f32>,
    ) -> Vec<MemtableHit> {
        // Pre-pass: tokenise the query (use any shard's analyzer — they're
        // all the same registry-provided one) and aggregate per-term
        // global doc_freq + per-field global stats.
        let analyzer = self.shards.iter().find_map(|s| {
            let g = s.read();
            g.registry
                .get_analyzer("default")
                .or_else(|| g.registry.get_analyzer("standard"))
        });
        let analyzer = match analyzer {
            Some(a) => a,
            None => return Vec::new(),
        };
        let q_tokens = analyzer.analyze(query);
        if q_tokens.is_empty() {
            return Vec::new();
        }

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
                        *term_global_df
                            .entry((fname.clone(), token.text.clone()))
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
                        *term_global_df
                            .entry((fname.clone(), token.text.clone()))
                            .or_insert(0) += *df;
                    }
                }
            }
        }
        let global_avg_field_len: std::collections::HashMap<String, f32> = field_total_len
            .into_iter()
            .map(|(k, (sum, n))| (k, if n == 0 { 0.0 } else { (sum / n as f64) as f32 }))
            .collect();

        let mut all: Vec<MemtableHit> = Vec::new();
        for s in &self.shards {
            all.extend(s.read().search_text_with_global_stats(
                query,
                fields,
                limit,
                global_doc_count,
                &global_avg_field_len,
                &term_global_df,
                field_boosts,
            ));
        }
        all.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all.truncate(limit);
        all
    }

    /// Drop-in replacement for the old single-memtable `insert` used by
    /// the `IndexStore::index()` single-doc path.
    pub fn insert(&self, doc_id: String, source: &Value, schema: &Schema, seq_no: u64) {
        let s = self.shard_for_dynamic(&doc_id);
        self.shards[s]
            .write()
            .insert(doc_id, source, schema, seq_no);
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
    pub fn insert_raw_bytes_with_seq(&self, seq_no: u64, doc_id: String, source_bytes: Arc<[u8]>) {
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

    /// Fused columnar bool query across all shards — one position walk per
    /// shard applying every predicate, all under a single read lock per
    /// shard (so positions can't shift between per-child walks under a
    /// concurrent flush drain).
    ///
    /// Returns `(hits, total)` where `hits` holds at most `limit` cloned
    /// `(doc_id, local_idx)` pairs (page materialisation) and `total` is
    /// the EXACT matching-doc count across the whole memtable.  `None`
    /// when any predicate's column is missing in a non-empty shard or a
    /// keyword column looks analyzed-text (same conservative bailouts as
    /// `doc_values_term_query` / `doc_values_range_query`) — caller falls
    /// back to the stored-source scan.
    ///
    /// This replaces the per-query `DocsForScan` walk for composite bool
    /// filters, which paid a `doc_matches_query` JSON descent per buffered
    /// doc per query (~1-6 s at a 40-280 k-doc memtable under a sustained
    /// bulk writer — the read-under-write bool collapse).
    pub fn doc_values_bool_query(
        &self,
        preds: &[MemBoolPred],
        limit: usize,
    ) -> Option<(Vec<(String, usize)>, u64)> {
        if preds.is_empty() {
            return None;
        }
        let mut out: Vec<(String, usize)> = Vec::new();
        let mut total: u64 = 0;
        for s in &self.shards {
            let g = s.read();
            if g.doc_count() == 0 {
                continue;
            }
            let room = limit.saturating_sub(out.len());
            let (mut hits, t) = g.doc_values_bool_hits(preds, room)?;
            out.append(&mut hits);
            total += t;
        }
        Some((out, total))
    }

    /// Columnar per-value counts for one field across all shards, for the
    /// terms/cardinality agg fast paths.  Returns `(value → live doc
    /// count, missing_docs)` — value RENDERING matches the brute per-doc
    /// extractor exactly for the shapes this serves (the keyword column
    /// stores strings verbatim, numbers via `Number::to_string`, bools as
    /// "true"/"false" — the same strings `flatten_to_strings` emits).
    ///
    /// `None` (caller falls back to the per-doc JSON extraction walk,
    /// which cost 100-300 ms/query against a drain-lagged 300 k-doc
    /// memtable — the terms-agg read-under-write tail) when equivalence
    /// isn't provable:
    /// - the field has EVER carried an array value in a shard (column
    ///   keeps only the first element; brute fans out over all);
    /// - any doc carries `_doc_count` (brute weights buckets by it);
    /// - the caller must additionally gate on plain field names (no
    ///   dotted paths / `.keyword` fallbacks — those resolve through
    ///   `get_nested_field`, not the column).
    pub fn terms_counts_columnar(
        &self,
        field: &str,
    ) -> Option<(std::collections::HashMap<String, u64>, u64)> {
        use rayon::prelude::*;
        // Bounded-delta: instead of the old FULL O(memtable) keyword-column
        // walk on EVERY terms/cardinality agg request (25-32 ms/query at a
        // drain-lagged 100-270 k-doc memtable — the dominant `fast_aggs`
        // read-under-write p95/p99 term), each shard reads its maintained
        // `keyword_counts` map (folded on the read side over just the docs
        // appended since the last query).  Result is O(distinct values) per
        // shard, not O(docs).  Value RENDERING is identical: `keyword_counts`
        // is folded from the same keyword column the old walk read.  `present`
        // (sum of the per-value counts) == the old "non-None slots" count, so
        // `missing = n - present` is unchanged.  Any shard that hits a bail
        // condition (array-valued field, `_doc_count` weights) yields `None`,
        // and `collect::<Option<_>>` short-circuits the whole call to `None`.
        let per_shard: Option<Vec<(std::collections::HashMap<String, u64>, u64)>> = self
            .shards
            .par_iter()
            .map(|s| {
                let g = s.read();
                let n = g.doc_count();
                if n == 0 {
                    return Some((std::collections::HashMap::new(), 0u64));
                }
                if g.doc_values.array_fields.contains(field)
                    || g.doc_values.numeric.contains_key("_doc_count")
                    || g.doc_values.keyword.contains_key("_doc_count")
                {
                    return None;
                }
                Some(g.doc_values.with_keyword_field(field, |c| {
                    match c.keyword_counts.get(field) {
                        None => {
                            // No doc in this shard carries the field as a scalar.
                            (std::collections::HashMap::new(), n as u64)
                        }
                        Some(m) => {
                            let mut counts: std::collections::HashMap<String, u64> =
                                std::collections::HashMap::with_capacity(m.len());
                            let mut present: u64 = 0;
                            for (k, &cnt) in m.iter() {
                                counts.insert(k.clone(), cnt as u64);
                                present += cnt as u64;
                            }
                            (counts, n as u64 - present)
                        }
                    }
                }))
            })
            .collect();
        let per_shard = per_shard?;
        // Merge the per-shard partials (commutative — identical to the serial
        // single-map accumulation this replaces).
        let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        let mut missing: u64 = 0;
        for (shard_counts, shard_missing) in per_shard {
            missing += shard_missing;
            if counts.is_empty() {
                // First non-empty partial becomes the base map — avoids
                // re-hashing its keys on the common single-populated-shard
                // path.
                counts = shard_counts;
            } else {
                for (term, cnt) in shard_counts {
                    *counts.entry(term).or_insert(0) += cnt;
                }
            }
        }
        Some((counts, missing))
    }

    /// DocValues term query — aggregates hits across all shards.
    /// Returns `Some(Vec<(doc_id, local_idx)>)` if any shard matched.
    /// The `local_idx` is shard-local; callers use the doc_id to
    /// resolve the source via `get_doc_source_*`.
    pub fn doc_values_term_query(
        &self,
        field: &str,
        value: &str,
        limit: usize,
    ) -> Option<(Vec<(String, usize)>, u64)> {
        let mut out: Vec<(String, usize)> = Vec::new();
        let mut total: u64 = 0;
        let mut any_hit = false;
        for s in &self.shards {
            let g = s.read();
            // Step 1: bound the id clone to the remaining global window;
            // the total is still exact per shard.  Mirrors
            // `doc_values_bool_query`.
            let room = limit.saturating_sub(out.len());
            if let Some((mut hits, t)) = g.doc_values_term_query(field, value, room) {
                any_hit = true;
                out.append(&mut hits);
                total += t;
            }
        }
        if any_hit {
            Some((out, total))
        } else {
            None
        }
    }

    pub fn doc_values_terms_query(
        &self,
        field: &str,
        values: &[String],
        limit: usize,
    ) -> Option<(Vec<(String, usize)>, u64)> {
        let mut out: Vec<(String, usize)> = Vec::new();
        let mut total: u64 = 0;
        let mut any_hit = false;
        for s in &self.shards {
            let g = s.read();
            let room = limit.saturating_sub(out.len());
            if let Some((mut hits, t)) = g.doc_values_terms_query(field, values, room) {
                any_hit = true;
                out.append(&mut hits);
                total += t;
            }
        }
        if any_hit {
            Some((out, total))
        } else {
            None
        }
    }

    /// Aggregated smart-field-encoding map across all shards.
    ///
    /// Computed lazily on the READ side from the doc-values `keyword`
    /// columns already maintained per shard — the same bounded-read pattern
    /// used by `all_keyword_values_for_field` / the `keyword_counts` fold.
    /// The ingest hot path only appends raw column values (it no longer runs
    /// `FieldAnalyzer` — the ingest-time `collect_sample` was dropped during
    /// the M4 perf pass, which is why `analyzed_encodings` is always empty),
    /// so this adds ZERO per-doc cost: the analysis happens here, once, when
    /// a caller (the `/v1/indices/:name/encodings` stats endpoint) asks for
    /// it.
    ///
    /// For each field we merge every shard's non-`None` keyword values into a
    /// single sample vector (capped at `ENCODING_SAMPLE_CAP` to bound work on
    /// a large memtable), then — for each field carrying at least
    /// `ENCODING_MIN_SAMPLES` values — run `FieldAnalyzer` to pick its optimal
    /// encoding.  Read-only: it never touches the columns or the ingest path.
    pub fn aggregated_field_encodings(&self) -> HashMap<String, FieldEncoding> {
        /// Upper bound on values analyzed per field — keeps this read bounded
        /// on a multi-hundred-k-doc memtable while staying statistically ample.
        const ENCODING_SAMPLE_CAP: usize = 4096;
        /// Minimum values a field needs before we report an encoding for it;
        /// below this the cardinality tiers aren't statistically meaningful.
        const ENCODING_MIN_SAMPLES: usize = 16;

        // Merge each shard's keyword columns into one capped sample vector
        // per field, under the shard READ lock.
        let mut samples: HashMap<String, Vec<String>> = HashMap::new();
        for s in &self.shards {
            let g = s.read();
            for (field, col) in g.doc_values.keyword.iter() {
                let acc = samples.entry(field.clone()).or_default();
                if acc.len() >= ENCODING_SAMPLE_CAP {
                    continue;
                }
                for v in col.iter().flatten() {
                    acc.push(v.clone());
                    if acc.len() >= ENCODING_SAMPLE_CAP {
                        break;
                    }
                }
            }
        }

        let analyzer = FieldAnalyzer::default();
        let mut out: HashMap<String, FieldEncoding> = HashMap::new();
        for (field, vals) in samples {
            if vals.len() < ENCODING_MIN_SAMPLES {
                continue;
            }
            let refs: Vec<&str> = vals.iter().map(|s| s.as_str()).collect();
            out.insert(field.clone(), analyzer.analyze(&field, &refs));
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
        limit: usize,
    ) -> Option<(Vec<(String, usize)>, u64)> {
        let mut out: Vec<(String, usize)> = Vec::new();
        let mut total: u64 = 0;
        let mut any_hit = false;
        for s in &self.shards {
            let g = s.read();
            let room = limit.saturating_sub(out.len());
            if let Some((mut hits, t)) = g.doc_values_range_query(field, gte, gt, lte, lt, room) {
                any_hit = true;
                out.append(&mut hits);
                total += t;
            }
        }
        if any_hit {
            Some((out, total))
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

    fn drain_shard_inner(
        &self,
        shard_idx: usize,
        skip_parse: bool,
    ) -> Vec<(u64, String, Arc<Value>, Arc<[u8]>)> {
        // Swap the shard's maps out under the write lock (pointer moves,
        // O(1)) and deallocate them AFTER the lock is released, on a
        // detached thread.  Pre-fix the reset assignments freed the
        // shard's entire inverted index + doc-values (millions of String
        // entries, ~95 ms at ~30 k docs/shard) while holding the shard
        // write lock — and because the bulk path fans every request
        // across ALL shards and joins on the slowest one, each flush
        // drain stalled every in-flight bulk request.  32 flushes per
        // 1 M docs × ~95 ms = a fixed ~3 s Amdahl serial term that
        // capped 8-client ingest at ~3.1× single-client throughput.
        let (drained, dead) = {
            let mut g = self.shards[shard_idx].write();
            let d: Vec<MemEntry> = std::mem::take(&mut g.docs);
            let dead_index = std::mem::take(&mut g.index);
            let dead_dv = std::mem::take(&mut g.doc_values);
            let dead_fl = std::mem::take(&mut g.field_lengths);
            let dead_afl = std::mem::take(&mut g.avg_field_lengths);
            let dead_dii = std::mem::take(&mut g.doc_id_index);
            g.total_bytes = 0;
            // Flush == merge: purge delete-aware ghost collection stats.
            g.ghost_docs = 0;
            let dead_gfl = std::mem::take(&mut g.ghost_field_len);
            let dead_gdf = std::mem::take(&mut g.ghost_doc_freq);
            (
                d,
                (
                    dead_index, dead_dv, dead_fl, dead_afl, dead_dii, dead_gfl, dead_gdf,
                ),
            )
        };
        // Free the dead maps off the flush critical path too — the
        // drain result is needed synchronously by the segment writer,
        // but nobody waits for these deallocations.  If thread spawn
        // fails (resource exhaustion) the closure — and the bundle it
        // owns — is dropped right here, inline: same correctness, we
        // only lose the async-free optimisation.
        let _ = std::thread::Builder::new()
            .name("xerj-drain-free".to_string())
            .spawn(move || drop(dead));
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
        g.docs
            .first()
            .map(|e| !e.source_bytes.is_empty())
            .unwrap_or(false)
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
        self.index = FxHashMap::default();
        self.doc_values = DocValues::default();
        self.total_bytes = 0;
        self.field_lengths = FxHashMap::default();
        self.avg_field_lengths = FxHashMap::default();
        self.doc_id_index = FxHashMap::default();
        // Flush is the equivalent of a Lucene merge: tombstone contributions
        // are purged, so delete-aware ghost statistics reset to empty.
        self.ghost_docs = 0;
        self.ghost_field_len = FxHashMap::default();
        self.ghost_doc_freq = FxHashMap::default();
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
        self.index = FxHashMap::default();
        self.doc_values = DocValues::default();
        self.total_bytes = 0;
        self.field_lengths = FxHashMap::default();
        self.avg_field_lengths = FxHashMap::default();
        self.doc_id_index = FxHashMap::default();
        // Flush is the equivalent of a Lucene merge: tombstone contributions
        // are purged, so delete-aware ghost statistics reset to empty.
        self.ghost_docs = 0;
        self.ghost_field_len = FxHashMap::default();
        self.ghost_doc_freq = FxHashMap::default();
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

/// Borrow the value for `key`, inserting `make()` when absent — but only
/// allocate a fresh OWNED key on the miss path.
///
/// `map.entry(key.to_owned()).or_insert_with(make)` clones `key` into a
/// heap `String` on EVERY call, even when the entry already exists.  On
/// the bulk-ingest write-lock critical path the field name and each term
/// recur across every doc of a shard's sub-batch, so that unconditional
/// clone was a `malloc` per (doc × field) and per (doc × term) — profiled
/// as the dominant under-lock cost (~40 ms / 10 k-doc batch).  Here the
/// common HIT path pays a second `&str` hash/probe instead (a few ns with
/// FxHash) and allocates NOTHING; only a genuinely new field/term pays the
/// key clone.
///
/// Crucially this shrinks the critical section WITHOUT moving any work off
/// the lock, so the write lock still serialises writer CPU against reader
/// CPU (measured: an off-lock rebuild instead regressed the CPU-heaviest
/// read aggs by removing that serialisation).  The resulting map is
/// byte-identical to the `entry` form.
#[inline]
fn entry_no_clone<'a, V>(
    map: &'a mut FxHashMap<String, V>,
    key: &str,
    make: impl FnOnce() -> V,
) -> &'a mut V {
    if !map.contains_key(key) {
        map.insert(key.to_string(), make());
    }
    // Present after the branch above; the second lookup is a hash+probe,
    // strictly cheaper than the key `String` allocation it replaces.
    map.get_mut(key).expect("just inserted / already present")
}

/// One resolved predicate of a `bool { must/filter }` for the fused
/// columnar memtable walk (`doc_values_bool_query`).  Built by the engine
/// from Term / Range query nodes; semantics per predicate mirror
/// `doc_values_term_query` / `doc_values_range_query` exactly.
pub enum MemBoolPred {
    Term {
        field: String,
        value: String,
    },
    Range {
        field: String,
        gte: Option<f64>,
        gt: Option<f64>,
        lte: Option<f64>,
        lt: Option<f64>,
    },
}

// ── FtsMemtable ──────────────────────────────────────────────────────────────

/// In-memory inverted index for documents awaiting flush.
pub struct FtsMemtable {
    /// Documents in insertion order.
    docs: Vec<MemEntry>,
    /// Inverted index: field → term → posting list (doc_id → tf).
    /// FxHashMap — see `PostingList` for the hasher rationale.
    index: FxHashMap<String, FxHashMap<String, PostingList>>,
    /// Columnar doc-values store for fast term/range/agg queries.
    pub doc_values: DocValues,
    /// Total accumulated byte size.
    total_bytes: usize,
    /// Analyzer registry.
    registry: Arc<AnalyzerRegistry>,
    /// Precomputed field lengths for BM25 scoring: field → {doc_id → token_count}
    field_lengths: FxHashMap<String, FxHashMap<DocId, u32>>,
    /// Running average field length per field: field → (total_tokens, doc_count)
    avg_field_lengths: FxHashMap<String, (f64, u64)>,
    /// doc_id → position in self.docs for O(1) lookup
    doc_id_index: FxHashMap<DocId, usize>,
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
    ghost_field_len: FxHashMap<String, (f64, u64)>,
    ghost_doc_freq: FxHashMap<String, FxHashMap<String, u64>>,
}

impl FtsMemtable {
    /// Create a new empty memtable with the default analyzer registry.
    pub fn new() -> Self {
        Self {
            docs: Vec::new(),
            index: FxHashMap::default(),
            doc_values: DocValues::default(),
            total_bytes: 0,
            registry: Arc::new(AnalyzerRegistry::default()),
            field_lengths: FxHashMap::default(),
            avg_field_lengths: FxHashMap::default(),
            doc_id_index: FxHashMap::default(),
            ghost_docs: 0,
            ghost_field_len: FxHashMap::default(),
            ghost_doc_freq: FxHashMap::default(),
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
            index: FxHashMap::default(),
            doc_values: DocValues::default(),
            total_bytes: 0,
            registry,
            field_lengths: FxHashMap::default(),
            avg_field_lengths: FxHashMap::default(),
            doc_id_index: FxHashMap::default(),
            ghost_docs: 0,
            ghost_field_len: FxHashMap::default(),
            ghost_doc_freq: FxHashMap::default(),
        }
    }

    /// Insert a document into the memtable, indexing all text fields.
    pub fn insert(&mut self, doc_id: String, source: &Value, schema: &Schema, seq_no: u64) {
        // Prefer a "default" analyzer if one was registered via custom settings
        // (e.g. with synonym expansion), otherwise fall back to "standard".
        let analyzer = self
            .registry
            .get_analyzer("default")
            .or_else(|| self.registry.get_analyzer("standard"))
            .expect("standard analyzer always present");

        let analyzed = analyze_doc(source, schema, &analyzer);

        // See `insert_pretokenized` for sizing rationale.
        let raw_size = source.to_string().len() + doc_id.len();
        let size = raw_size * 3 + 64;

        self.insert_analyzed(seq_no, doc_id, Arc::new(source.clone()), &analyzed, size);
    }

    /// Back half of [`insert`] — everything that MUST run under the
    /// shard write lock: field-length/avg updates, posting merges,
    /// size accounting, doc-values push and the docs/doc_id_index push.
    ///
    /// The front half (schema text-field extraction + dynamic-mapping
    /// tree walk + analyzer tokenisation — see [`analyze_doc`]) is pure
    /// and can run OUTSIDE the lock; the ES `_bulk` turbo path runs it
    /// on the rayon pool per batch and then calls this under the shard
    /// lock.  Splitting insert this way cuts the lock-held cost from
    /// ~39 µs/doc to ~9 µs/doc — the single biggest lever on the
    /// 8-client bulk-ingest scaling ceiling.
    ///
    /// (A deeper split that also pre-aggregated tf and pre-extracted
    /// doc-values cells outside the lock was tried and REVERTED: the
    /// extra per-doc map/Vec allocations it added on the analyze side
    /// cost more total CPU than the in-lock time they saved — c8
    /// throughput dropped ~10% because the 8-client workload is CPU-
    /// saturated, not lock-saturated, after this split.)
    ///
    /// `size` is the pre-computed `size_bytes` accounting value
    /// (`(serialized_len + doc_id_len) * 3 + 64`), passed in so the
    /// `source.to_string()` re-serialisation also stays off the lock.
    /// The `source` Arc is stored directly — no deep clone.
    pub fn insert_analyzed(
        &mut self,
        seq_no: u64,
        doc_id: String,
        source: Arc<Value>,
        analyzed: &[(String, Vec<Token>)],
        size: usize,
    ) {
        // Intern the doc id ONCE (one `malloc`).  Every posting / field-
        // length / doc-id-index entry below then stores a pointer-bumped
        // `Arc<str>` clone instead of a fresh `String` heap allocation —
        // the per-token clone under the shard write lock becomes a
        // relaxed refcount increment.  See `DocId` for the rationale.
        let doc_key: DocId = Arc::from(doc_id.as_str());

        for (field_name, tokens) in analyzed {
            let token_count = tokens.len() as u32;

            // Cache the field length for BM25 scoring.  `field_name`
            // recurs across every doc of the batch, so `entry_no_clone`
            // skips the per-doc field-key `malloc` on the hot path.
            entry_no_clone(&mut self.field_lengths, field_name, Default::default)
                .insert(doc_key.clone(), token_count);

            // Update running average.
            let entry = entry_no_clone(&mut self.avg_field_lengths, field_name, || (0.0, 0));
            entry.0 += token_count as f64;
            entry.1 += 1;

            let field_index = entry_no_clone(&mut self.index, field_name, Default::default);
            for token in tokens {
                // `token.text` likewise recurs (low-cardinality keyword-ish
                // fields), so only a genuinely new term pays the key clone.
                let posting = entry_no_clone(field_index, &token.text, Default::default);
                // Interned doc id — a pointer bump, not a `String` malloc.
                *posting.entry(doc_key.clone()).or_insert(0) += 1;
            }
        }

        self.total_bytes += size;

        // Populate the columnar DocValues store BEFORE pushing to docs so that
        // the doc_index equals the current length (i.e. the slot we're about to fill).
        let doc_index = self.docs.len();
        self.doc_values.push(&source, doc_index);

        // Track doc_id → index for O(1) lookup.
        self.doc_id_index.insert(doc_key, doc_index);

        self.docs.push(MemEntry {
            seq_no,
            doc_id,
            source,
            source_bytes: Arc::from(&[][..]),
            parsed_memo: OnceLock::new(),
            size_bytes: size,
        });
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
        self.doc_id_index
            .insert(Arc::from(doc_id.as_str()), doc_index);

        self.docs.push(MemEntry {
            seq_no,
            doc_id,
            source: Arc::new(Value::Null),
            source_bytes,
            parsed_memo: OnceLock::new(),
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
    pub fn insert_raw_bytes_fresh(&mut self, seq_no: u64, doc_id: String, source_bytes: Arc<[u8]>) {
        let estimated = 800usize;
        self.total_bytes += estimated;

        let doc_index = self.docs.len();
        self.doc_id_index
            .insert(Arc::from(doc_id.as_str()), doc_index);
        self.docs.push(MemEntry {
            seq_no,
            doc_id,
            source: Arc::new(Value::Null),
            source_bytes,
            parsed_memo: OnceLock::new(),
            size_bytes: estimated,
        });
    }

    pub fn insert_pretokenized(&mut self, doc_id: String, source: Arc<Value>, tokens: &[String]) {
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
        self.doc_id_index
            .insert(Arc::from(doc_id.as_str()), doc_index);

        self.docs.push(MemEntry {
            seq_no,
            doc_id,
            source,
            source_bytes: Arc::from(&[][..]),
            parsed_memo: OnceLock::new(),
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

        let doc_key: DocId = Arc::from(doc_id.as_str());
        for (field_name, text) in &text_fields {
            let tokens = analyzer.analyze(text);
            let token_count = tokens.len() as u32;

            // Cache the field length for BM25 scoring.
            self.field_lengths
                .entry(field_name.clone())
                .or_default()
                .insert(doc_key.clone(), token_count);

            // Update running average.
            let entry = self
                .avg_field_lengths
                .entry(field_name.clone())
                .or_insert((0.0, 0));
            entry.0 += token_count as f64;
            entry.1 += 1;

            let field_index = self.index.entry(field_name.clone()).or_default();
            for token in &tokens {
                let posting = field_index.entry(token.text.clone()).or_default();
                *posting.entry(doc_key.clone()).or_insert(0) += 1;
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

        // Track doc_id → index for O(1) lookup (interned Arc<str> key).
        self.doc_id_index.insert(doc_key, doc_index);

        self.docs.push(MemEntry {
            seq_no: 0,
            doc_id,
            // Arc clone = atomic refcount increment (cheap pointer copy).
            source,
            source_bytes: Arc::from(&[][..]),
            parsed_memo: OnceLock::new(),
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
            query,
            fields,
            limit,
            0,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
        )
    }

    /// search_text variant that uses caller-supplied GLOBAL doc_count,
    /// per-field avg lengths, and per-(field,term) doc frequencies.
    /// Falls back to local stats when the global maps are empty.
    // The stats params mirror the cross-shard aggregation the orchestrator
    // computes once per query; bundling them into a struct would just move
    // the arity into a builder for a single internal call site.
    #[allow(clippy::too_many_arguments)]
    pub fn search_text_with_global_stats(
        &self,
        query: &str,
        fields: &[&str],
        limit: usize,
        global_doc_count: u64,
        global_avg_field_len: &std::collections::HashMap<String, f32>,
        global_term_df: &std::collections::HashMap<(String, String), u64>,
        field_boosts: &std::collections::HashMap<String, f32>,
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

        let doc_count = if global_doc_count > 0 {
            global_doc_count
        } else {
            self.docs.len() as u64
        };
        // Keyed by the interned `Arc<str>` doc id from the postings lists,
        // so accumulating a term's contribution is a pointer-bump clone
        // rather than a per-hit `String` allocation.
        let mut scores: HashMap<DocId, f32> = HashMap::new();

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
                    .unwrap_or(posting_list.len() as u64);
                let avg_field_len = global_avg_field_len
                    .get(*field_name)
                    .copied()
                    .unwrap_or_else(|| self.avg_field_length(field_name));

                let scorer = Bm25Scorer::new(avg_field_len, doc_count);
                // Per-field boost from the query tree (ES `boost` on match /
                // `field^N` on multi_match). 1.0 when unboosted, so scores
                // stay bit-identical for boost-free queries.
                let field_boost = field_boosts.get(*field_name).copied().unwrap_or(1.0);

                for (doc_id, &tf) in posting_list {
                    let field_len = self
                        .field_lengths
                        .get(*field_name)
                        .and_then(|m| m.get(doc_id))
                        .copied()
                        .unwrap_or(1);

                    let score = scorer.score_term(doc_freq, tf, field_len) * field_boost;
                    *scores.entry(doc_id.clone()).or_insert(0.0) += score;
                }
            }
        }

        let mut hits: Vec<MemtableHit> = scores
            .into_iter()
            .map(|(doc_id, score)| MemtableHit {
                doc_id: doc_id.to_string(),
                score,
            })
            .collect();

        // Sort by score descending.
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
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

    /// First `n` doc ids (insertion order) — see
    /// `ShardedFtsMemtable::doc_ids_bounded`.
    pub fn doc_ids_up_to(&self, n: usize) -> Vec<String> {
        self.docs.iter().take(n).map(|e| e.doc_id.clone()).collect()
    }

    /// Resolve a MemEntry's source Value — if `source` is Null but
    /// `source_bytes` is non-empty, lazily parse the bytes.  This is
    /// the M5.11 deferred-parse path used by `insert_raw_bytes_with_seq`.
    fn resolve_source(entry: &MemEntry) -> Value {
        if entry.source.is_null() && !entry.source_bytes.is_empty() {
            (**Self::memoized_parsed(entry)).clone()
        } else {
            (*entry.source).clone()
        }
    }

    /// Arc-sharing twin of `resolve_source`: no deep clone for the common
    /// HTTP-bulk path where the parsed source is already Arc-stored.
    fn resolve_source_arc(entry: &MemEntry) -> Arc<Value> {
        if entry.source.is_null() && !entry.source_bytes.is_empty() {
            Arc::clone(Self::memoized_parsed(entry))
        } else {
            Arc::clone(&entry.source)
        }
    }

    /// Parse `entry.source_bytes` at most once, caching the result in the
    /// entry's `parsed_memo`.  All read paths for M5.11 raw-bytes entries
    /// funnel through here so a scan of the buffered memtable parses each
    /// doc a single time for the memtable's lifetime instead of once per
    /// query.  The bytes are immutable for a given entry (a doc update
    /// creates a fresh `MemEntry` with a fresh memo), so the cache never
    /// goes stale.
    fn memoized_parsed(entry: &MemEntry) -> &Arc<Value> {
        entry.parsed_memo.get_or_init(|| {
            Arc::new(serde_json::from_slice(&entry.source_bytes).unwrap_or(Value::Null))
        })
    }

    /// All (doc_id, source) pairs WITHOUT deep-cloning the source trees.
    /// The fast-agg path was deep-cloning the entire memtable per agg
    /// request via `all_docs_with_sources` (~100-300 ms/query at 1e5
    /// buffered docs under a bulk writer).
    pub fn all_docs_with_sources_arc(&self) -> Vec<(String, Arc<Value>)> {
        self.docs
            .iter()
            .map(|e| (e.doc_id.clone(), Self::resolve_source_arc(e)))
            .collect()
    }

    /// Return the full original source JSON for a document by ID.
    /// O(1) doc lookup via `doc_id_index` (maintained by every insert /
    /// remove path), with a linear-scan fallback in case an entry is ever
    /// missing from the index.
    ///
    /// Pre-fix this was an unconditional `docs.iter().find(..)` — a full
    /// linear walk of the shard per call.  `fill_memtable_sources`
    /// hydrates up to `materialisation_limit` (256) hits per search, so
    /// with a bulk writer keeping the memtable at 10⁵ docs every search
    /// paid 256 × O(shard) string compares; `get_doc_source_as_value` was
    /// the single hottest frame in the read-under-write profile.
    fn entry_for(&self, doc_id: &str) -> Option<&MemEntry> {
        if let Some(&i) = self.doc_id_index.get(doc_id) {
            if let Some(e) = self.docs.get(i) {
                if e.doc_id == doc_id {
                    return Some(e);
                }
            }
        }
        self.docs.iter().find(|e| e.doc_id == doc_id)
    }

    pub fn get_doc_source_as_value(&self, doc_id: &str) -> Option<Value> {
        self.entry_for(doc_id).map(Self::resolve_source)
    }

    /// Return the Arc-wrapped source for a document by ID.
    pub fn get_doc_source_arc(&self, doc_id: &str) -> Option<Arc<Value>> {
        self.entry_for(doc_id).map(Self::resolve_source_arc)
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

    /// Check if a document exists in the memtable.  O(1) via
    /// `doc_id_index`, with linear fallback (see `entry_for`).
    pub fn contains(&self, doc_id: &str) -> bool {
        self.entry_for(doc_id).is_some()
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
        self.index = FxHashMap::default();
        self.doc_values = DocValues::default();
        self.total_bytes = 0;
        self.field_lengths = FxHashMap::default();
        self.avg_field_lengths = FxHashMap::default();
        self.doc_id_index = FxHashMap::default();
        // Flush is the equivalent of a Lucene merge: tombstone contributions
        // are purged, so delete-aware ghost statistics reset to empty.
        self.ghost_docs = 0;
        self.ghost_field_len = FxHashMap::default();
        self.ghost_doc_freq = FxHashMap::default();
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
        self.index = FxHashMap::default();
        self.doc_values = DocValues::default();
        self.total_bytes = 0;
        self.field_lengths = FxHashMap::default();
        self.avg_field_lengths = FxHashMap::default();
        self.doc_id_index = FxHashMap::default();
        // Flush is the equivalent of a Lucene merge: tombstone contributions
        // are purged, so delete-aware ghost statistics reset to empty.
        self.ghost_docs = 0;
        self.ghost_field_len = FxHashMap::default();
        self.ghost_doc_freq = FxHashMap::default();
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
    /// Backed by the bounded-delta per-field `keyword_counts` map (folds
    /// only this field's appended tail).  `&self` — the fold uses interior
    /// mutability, so the caller holds only the shard READ lock.
    pub fn doc_values_keyword_count(&self, field: &str, value: &str) -> Option<u32> {
        self.doc_values.with_keyword_field(field, |c| {
            c.keyword_counts
                .get(field)
                .map(|m| m.get(value).copied().unwrap_or(0))
        })
    }

    /// O(1) memtable count for `field == value` over the numeric column.
    pub fn doc_values_numeric_count(&self, field: &str, value: f64) -> Option<u32> {
        self.doc_values.with_numeric_field(field, |c| {
            c.numeric_counts
                .get(field)
                .map(|m| m.get(&value.to_bits()).copied().unwrap_or(0))
        })
    }

    // ── Fast DocValues queries ────────────────────────────────────────────────

    /// Incremental per-shard sorted-candidate extraction — see
    /// `ShardedFtsMemtable::sort_candidates_numeric` for the contract and
    /// `SortCandCache` for the state.  Returns `(top page-order candidates
    /// as (key, doc_id), missing doc_ids, shard doc count)`; `None` when
    /// the shard is ineligible (un-normalisable value or array-valued
    /// field — poisoned until the next drain resets the memtable, so the
    /// fallback decision itself is O(1) per query, not a re-walk).
    pub fn sort_candidates_cached(
        &self,
        field: &str,
        desc: bool,
        cap: usize,
        normalize: &dyn Fn(&str) -> Option<f64>,
    ) -> Option<(Vec<(f64, String)>, Vec<String>, u64)> {
        let n = self.docs.len();
        let cap = cap.max(1);
        if self.doc_values.array_fields.contains(field) {
            return None;
        }
        let key = if desc {
            format!("{field}\u{1}d")
        } else {
            format!("{field}\u{1}a")
        };
        // Step 4: interior-mutable cache — held under the shard READ lock.
        let mut cache = self.doc_values.sort_cand_cache.lock();
        let entry = cache.entry(key).or_insert_with(|| SortCandCache {
            seen_docs: 0,
            cap,
            top: Vec::new(),
            missing: Vec::new(),
            poisoned: false,
        });
        if entry.poisoned {
            return None;
        }
        if entry.cap < cap || entry.seen_docs > n {
            // Larger page than cached, or the docs vec shrank without a
            // cache clear (defensive) — rebuild from scratch.
            entry.cap = cap;
            entry.seen_docs = 0;
            entry.top.clear();
            entry.missing.clear();
        }
        // Fold in the docs inserted since the last query.
        let ncol = self.doc_values.numeric.get(field);
        let kcol = self.doc_values.keyword.get(field);
        for i in entry.seen_docs..n {
            let nv: Option<f64> = ncol.and_then(|c| c.get(i).copied().flatten());
            let v: Option<f64> = match nv {
                Some(x) => Some(x),
                None => match kcol.and_then(|c| c.get(i)).and_then(|o| o.as_deref()) {
                    Some(s) => match normalize(s) {
                        Some(x) => Some(x),
                        None => {
                            entry.poisoned = true;
                            return None;
                        }
                    },
                    None => None,
                },
            };
            match v {
                Some(x) => entry.top.push((x, i)),
                None => {
                    if entry.missing.len() < entry.cap {
                        entry.missing.push(i);
                    }
                }
            }
        }
        entry.seen_docs = n;
        // Keep the pool bounded: page-order sort + cut to cap (+boundary
        // ties) once it doubles.  Top-cap of (old top-cap ∪ new docs) ==
        // top-cap of all docs, so the incremental cut is exact.
        let cut = |top: &mut Vec<(f64, usize)>, cap: usize| {
            if desc {
                top.sort_unstable_by(|a, b| {
                    b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
                });
            } else {
                top.sort_unstable_by(|a, b| {
                    a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
                });
            }
            let mut keep = top.len().min(cap);
            while keep < top.len() && keep > 0 && top[keep].0 == top[keep - 1].0 {
                keep += 1;
            }
            top.truncate(keep);
        };
        if entry.top.len() > entry.cap.saturating_mul(2).max(64) {
            cut(&mut entry.top, entry.cap);
        }
        // Result view: an exact top-`cap` (+ties) cut of the pool.
        let mut pool = entry.top.clone();
        cut(&mut pool, cap);
        let out: Vec<(f64, String)> = pool
            .into_iter()
            .map(|(v, i)| (v, self.docs[i].doc_id.clone()))
            .collect();
        let miss: Vec<String> = entry
            .missing
            .iter()
            .take(cap)
            .map(|&i| self.docs[i].doc_id.clone())
            .collect();
        Some((out, miss, n as u64))
    }

    /// Per-shard fused bool walk — see `ShardedFtsMemtable::doc_values_bool_query`.
    ///
    /// Column resolution and per-predicate match semantics mirror the
    /// standalone `doc_values_term_query` (keyword column, case-sensitive
    /// whole value, analyzed-text whitespace bailout) and
    /// `doc_values_range_query` (numeric column, gte/gt/lte/lt) exactly, so
    /// a bool of term+range predicates matches the same doc set the
    /// per-child queries would intersect to — without intermediate
    /// per-child hit vectors.
    pub fn doc_values_bool_hits(
        &self,
        preds: &[MemBoolPred],
        limit: usize,
    ) -> Option<(Vec<(String, usize)>, u64)> {
        enum Col<'a> {
            Kw(&'a Vec<Option<String>>, &'a str),
            NumEq(&'a Vec<Option<f64>>, f64),
            Num(
                &'a Vec<Option<f64>>,
                Option<f64>,
                Option<f64>,
                Option<f64>,
                Option<f64>,
            ),
        }
        let mut cols: Vec<Col<'_>> = Vec::with_capacity(preds.len());
        for p in preds {
            match p {
                MemBoolPred::Term { field, value } => {
                    if let Some(col) = self.doc_values.keyword.get(field.as_str()) {
                        // Step 2: analyzed-text bailout via the insert-time
                        // cached flag instead of an O(N) per-query column
                        // prescan.
                        if self
                            .doc_values
                            .keyword_has_whitespace
                            .contains(field.as_str())
                        {
                            return None;
                        }
                        cols.push(Col::Kw(col, value.as_str()));
                    } else if let Some(col) = self.doc_values.numeric.get(field.as_str()) {
                        // Numeric term predicate: exact f64 equality (the
                        // predicate value was lowered to its string form).
                        let needle = value.parse::<f64>().ok()?;
                        cols.push(Col::NumEq(col, needle));
                    } else {
                        return None;
                    }
                }
                MemBoolPred::Range {
                    field,
                    gte,
                    gt,
                    lte,
                    lt,
                } => {
                    let col = self.doc_values.numeric.get(field.as_str())?;
                    cols.push(Col::Num(col, *gte, *gt, *lte, *lt));
                }
            }
        }
        let n = self.docs.len();
        let mut out: Vec<(String, usize)> = Vec::new();
        let mut total: u64 = 0;
        'doc: for idx in 0..n {
            for c in &cols {
                let ok = match c {
                    Col::Kw(col, want) => col.get(idx).and_then(|o| o.as_deref()) == Some(*want),
                    Col::NumEq(col, want) => col.get(idx).copied().flatten() == Some(*want),
                    Col::Num(col, gte, gt, lte, lt) => match col.get(idx).copied().flatten() {
                        None => false,
                        Some(v) => {
                            let pl = match (gte, gt) {
                                (Some(b), _) => v >= *b,
                                (None, Some(b)) => v > *b,
                                (None, None) => true,
                            };
                            let pu = match (lte, lt) {
                                (Some(b), _) => v <= *b,
                                (None, Some(b)) => v < *b,
                                (None, None) => true,
                            };
                            pl && pu
                        }
                    },
                };
                if !ok {
                    continue 'doc;
                }
            }
            total += 1;
            if out.len() < limit {
                out.push((self.docs[idx].doc_id.clone(), idx));
            }
        }
        Some((out, total))
    }

    /// Fast term query using the keyword column — O(N * string_compare).
    ///
    /// Returns `(doc_id, doc_index)` pairs for documents where the keyword
    /// column for `field` equals `value` (case-sensitive exact match).
    ///
    /// Returns `None` when the field has no keyword column (fall back to JSON scan).
    pub fn doc_values_term_query(
        &self,
        field: &str,
        value: &str,
        limit: usize,
    ) -> Option<(Vec<(String, usize)>, u64)> {
        if let Some(col) = self.doc_values.keyword.get(field) {
            // Step 2: text-field bailout via the insert-time cached flag — a
            // whitespace-containing keyword value means the column stores the
            // full analyzed-text source; a `term` query expects a token match
            // which only the FTS / scan path can serve.  Was an O(N) per-query
            // column prescan.
            if self.doc_values.keyword_has_whitespace.contains(field) {
                return None;
            }
            // Step 1: walk the whole column for an exact total but only clone
            // the doc_id String for the first `limit` matches — mirrors
            // `doc_values_bool_hits`.  The unbounded clone was ~all-matching-doc
            // String allocations per query at a drain-lagged memtable.
            let mut out: Vec<(String, usize)> = Vec::new();
            let mut total: u64 = 0;
            for (idx, opt) in col.iter().enumerate() {
                if opt.as_deref() == Some(value) {
                    total += 1;
                    if out.len() < limit {
                        out.push((self.docs[idx].doc_id.clone(), idx));
                    }
                }
            }
            return Some((out, total));
        }
        // Numeric column: `term` on a numeric field is lowered to its string
        // form by the caller; parse it back to f64 and match by exact equality
        // (mirrors the numeric matching in `doc_values_range_indices`). Without
        // this a `term` on a numeric field found no keyword column and returned
        // `None`, dropping the query onto the O(N) per-doc `_source` scan — a
        // selective numeric term (`code:500`, an id lookup) then paid a full
        // memtable walk. Bail (→ JSON scan) if the value isn't numeric.
        if let Some(col) = self.doc_values.numeric.get(field) {
            let needle = value.parse::<f64>().ok()?;
            let mut out: Vec<(String, usize)> = Vec::new();
            let mut total: u64 = 0;
            for (idx, opt) in col.iter().enumerate() {
                if *opt == Some(needle) {
                    total += 1;
                    if out.len() < limit {
                        out.push((self.docs[idx].doc_id.clone(), idx));
                    }
                }
            }
            return Some((out, total));
        }
        None
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
                if pl && pu {
                    Some(idx)
                } else {
                    None
                }
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
        limit: usize,
    ) -> Option<(Vec<(String, usize)>, u64)> {
        let Some(col) = self.doc_values.keyword.get(field) else {
            // Numeric column: a `terms` over a numeric field — parse every
            // query value to f64 and match by exact equality (mirrors the
            // single-value numeric path in `doc_values_term_query`). Without
            // this a numeric `terms` fell onto the O(N) `_source` scan.
            if let Some(col) = self.doc_values.numeric.get(field) {
                let needles: Vec<f64> = values
                    .iter()
                    .filter_map(|v| v.parse::<f64>().ok())
                    .collect();
                if needles.len() != values.len() {
                    return None; // some value isn't numeric — JSON scan
                }
                let mut out: Vec<(String, usize)> = Vec::new();
                let mut total: u64 = 0;
                for (idx, opt) in col.iter().enumerate() {
                    if let Some(v) = *opt {
                        if needles.contains(&v) {
                            total += 1;
                            if out.len() < limit {
                                out.push((self.docs[idx].doc_id.clone(), idx));
                            }
                        }
                    }
                }
                return Some((out, total));
            }
            return None;
        };
        // Step 2: analyzed-text bailout via the insert-time cached flag.
        // If any stored keyword value in this column contains whitespace
        // it's likely an analyzed text field whose doc-values were built
        // from the full source string (not the token stream). A `terms`
        // query compares against tokens in that case, which doc-values
        // can't serve — bail so callers fall through to the scan path.
        if self.doc_values.keyword_has_whitespace.contains(field) {
            return None;
        }
        // Step 1: exact total, bounded doc_id materialisation.
        let mut out: Vec<(String, usize)> = Vec::new();
        let mut total: u64 = 0;
        for (idx, opt) in col.iter().enumerate() {
            if let Some(v) = opt.as_deref() {
                if values.iter().any(|qv| qv == v) {
                    total += 1;
                    if out.len() < limit {
                        out.push((self.docs[idx].doc_id.clone(), idx));
                    }
                }
            }
        }
        Some((out, total))
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
        limit: usize,
    ) -> Option<(Vec<(String, usize)>, u64)> {
        let col = self.doc_values.numeric.get(field)?;
        // Step 1: exact total, bounded doc_id materialisation — the
        // unbounded id clone was the biggest read-under-write term for
        // broad range filters (~all-of-memtable String allocations/query).
        let mut out: Vec<(String, usize)> = Vec::new();
        let mut total: u64 = 0;
        for (idx, opt) in col.iter().enumerate() {
            let v = match *opt {
                Some(v) => v,
                None => continue,
            };
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
                total += 1;
                if out.len() < limit {
                    out.push((self.docs[idx].doc_id.clone(), idx));
                }
            }
        }
        Some((out, total))
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
        self.doc_values
            .with_keyword_field(field, |c| match c.keyword_set.get(field) {
                Some(set) => set.iter().map(|v| (v.clone(), 1)).collect(),
                None => Vec::new(),
            })
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

/// Collect any string-valued field not in the schema (dynamic mapping).
/// Walks nested objects with dotted paths so a doc `{a: {b: {c: "x"}}}`
/// indexes `a.b.c: "x"` — queries targeting the leaf-specific dotted
/// path match only docs that carry that leaf. Also keeps a root-level
/// JSON-blob entry for each top-level object key so types like
/// flattened (queried at the root path) still find their inner tokens.
///
/// Hoisted out of `FtsMemtable::insert` so [`analyze_doc`] can run the
/// identical walk outside the shard write lock.
fn collect_text_fields(v: &Value, prefix: &str, out: &mut HashMap<String, String>) {
    if let Value::Object(obj) = v {
        for (k, val) in obj {
            let path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{}.{}", prefix, k)
            };
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
                    let joined: String = arr
                        .iter()
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
}

/// Front half of `FtsMemtable::insert`, factored out as a pure function
/// so it can run OUTSIDE the memtable shard write locks (e.g. on the
/// rayon pool, one call per doc, before the bucketed per-shard insert).
///
/// Extracts the schema Text fields plus the dynamic-mapping dotted-path
/// text fields (exactly like `insert` did in-lock) and tokenises each
/// with `analyzer`.  Feed the result to [`FtsMemtable::insert_analyzed`].
pub fn analyze_doc(
    source: &Value,
    schema: &Schema,
    analyzer: &AnalyzerPipeline,
) -> Vec<(String, Vec<Token>)> {
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

    // Also index any string-valued field not in the schema (dynamic
    // mapping) — see `collect_text_fields`.
    if source.is_object() {
        collect_text_fields(source, "", &mut text_fields);
    }

    text_fields
        .into_iter()
        .map(|(field_name, text)| {
            let tokens = analyzer.analyze(&text);
            (field_name, tokens)
        })
        .collect()
}

/// Extract a string value from a JSON value for text indexing.
fn extract_text_value(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(arr) => arr
            .iter()
            .map(extract_text_value)
            .collect::<Vec<_>>()
            .join(" "),
        Value::Object(_) => serde_json::to_string(val).unwrap_or_default(),
        Value::Null => String::new(),
    }
}

#[cfg(test)]
mod bounded_delta_counts_tests {
    use super::*;
    use serde_json::json;

    /// Brute-force full recount of a DocValues store's columns, matching
    /// the OLD `ensure_counts_built` semantics exactly.
    fn recount(
        dv: &DocValues,
    ) -> (
        FxHashMap<String, FxHashMap<String, u32>>,
        FxHashMap<String, FxHashMap<u64, u32>>,
        FxHashMap<String, FxHashSet<String>>,
    ) {
        let mut kc: FxHashMap<String, FxHashMap<String, u32>> = FxHashMap::default();
        let mut nc: FxHashMap<String, FxHashMap<u64, u32>> = FxHashMap::default();
        let mut ks: FxHashMap<String, FxHashSet<String>> = FxHashMap::default();
        for (field, col) in &dv.keyword {
            let counts = kc.entry(field.clone()).or_default();
            let set = ks.entry(field.clone()).or_default();
            for s in col.iter().flatten() {
                *counts.entry(s.clone()).or_insert(0) += 1;
                set.insert(s.clone());
            }
        }
        for (field, col) in &dv.numeric {
            let counts = nc.entry(field.clone()).or_default();
            for f in col.iter().flatten() {
                *counts.entry(f.to_bits()).or_insert(0) += 1;
            }
        }
        (kc, nc, ks)
    }

    /// Fold every field per-field (as the query path does) and assert the
    /// maintained maps equal a full brute-force recount.
    fn assert_maintained_eq_recount(dv: &DocValues) {
        let (kc, nc, ks) = recount(dv);
        // Fold each keyword + numeric field via the per-field accessors,
        // exactly as the query path does, then compare the whole state.
        for field in dv.keyword.keys() {
            dv.with_keyword_field(field, |_| {});
        }
        for field in dv.numeric.keys() {
            dv.with_numeric_field(field, |_| {});
        }
        let cs = dv.counts.lock();
        assert_eq!(cs.keyword_counts, kc, "keyword_counts drift");
        assert_eq!(cs.numeric_counts, nc, "numeric_counts drift");
        assert_eq!(cs.keyword_set, ks, "keyword_set drift");
    }

    #[test]
    fn bounded_delta_matches_full_recount() {
        let mut dv = DocValues::default();
        // Push a first batch spanning keyword + numeric + mixed-number fields.
        let models = ["haiku", "sonnet", "opus", "haiku", "opus"];
        for i in 0..2500usize {
            let doc = json!({
                "model": models[i % models.len()],
                "status": if i % 3 == 0 { "ok" } else { "err" },
                "latency_ms": (i % 400) as i64,
                "cost_usd": (i as f64) * 0.001,
            });
            dv.push(&doc, i);
        }
        // First fold (folds the whole batch) must equal a full recount.
        assert_maintained_eq_recount(&dv);

        // Incremental: append MORE docs past the watermark, fold again — must
        // still equal a full recount (bounded-delta folds only the new tail).
        for i in 2500..6000usize {
            let doc = json!({
                "model": models[i % models.len()],
                "status": if i % 5 == 0 { "ok" } else { "err" },
                "latency_ms": (i % 400) as i64,
                "cost_usd": (i as f64) * 0.001,
            });
            dv.push(&doc, i);
        }
        assert_maintained_eq_recount(&dv);

        // Per-field isolation: folding ONE field must not populate a sibling's
        // maps (a `terms` on `model` must not build `cost_usd`'s high-card map).
        {
            let mut fresh = DocValues::default();
            for i in 0..1000usize {
                fresh.push(&json!({ "model": "x", "cost_usd": i as f64 }), i);
            }
            fresh.with_keyword_field("model", |c| {
                assert!(c.keyword_counts.contains_key("model"));
                assert!(!c.keyword_counts.contains_key("cost_usd"), "sibling folded");
                assert!(!c.numeric_counts.contains_key("cost_usd"), "sibling folded");
            });
        }

        // Delete resets the watermarks; the next fold re-derives from scratch
        // and must again equal a full recount over the (shifted) columns.
        dv.remove_at(1234);
        dv.remove_at(0);
        assert_maintained_eq_recount(&dv);
    }
}
