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
    /// WAL `seq_no` of the buffered document (`u64::MAX` when it can't be
    /// resolved).  #191 — the memtable's bounded FTS materialisation has to
    /// truncate under the SAME total order the final page sort uses
    /// (`score DESC, seq_no ASC, _id ASC`); score alone leaves the survivors
    /// of a tie decided by shard-walk order, which is not arrival order.
    pub seq_no: u64,
}

/// Order `hits` by the page key XERJ's final sort uses: `score DESC`, then
/// `seq_no ASC` (arrival order — the `_doc` analogue), then `_id ASC`
/// (`index.rs:16222-16228`).
///
/// #191 — any bounded materialisation must truncate under the SAME total order
/// the page is finally sorted by, otherwise the documents that survive a tie
/// are decided by whatever order the walk happened to visit them in.  Lucene
/// makes the same comparator total for the same reason
/// (`HitQueue.lessThan` breaks an equal score by the smaller doc id,
/// `lucene/core/src/java/org/apache/lucene/search/HitQueue.java:76-82`);
/// approach only, no code taken.
fn sort_hits_by_page_key(hits: &mut [MemtableHit]) {
    hits.sort_unstable_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.seq_no.cmp(&b.seq_no))
            .then_with(|| a.doc_id.cmp(&b.doc_id))
    });
}

#[cfg(test)]
mod ingest_memory_drain_tests {
    use super::*;
    use crate::ingest_memory::{Category, Ledger, Measurement};
    use std::sync::{Arc, Barrier};

    #[test]
    fn authoritative_active_moves_to_drained_through_blocked_finalize_success_and_error() {
        for inject_error in [false, true] {
            let mem = ShardedFtsMemtable::new();
            mem.insert(
                "doc-1".to_string(),
                &serde_json::json!({"body": "retained"}),
                &Schema::default(),
                1,
            );
            let shard = mem
                .shard_loads()
                .into_iter()
                .find(|(_, docs, _)| *docs != 0)
                .unwrap()
                .0;
            let ledger = Ledger::new();
            ledger.observe(Category::MemtableActive, mem.size_bytes());
            assert!(
                ledger
                    .gauge(Category::MemtableActive, Measurement::Estimated)
                    .current
                    > 0
            );

            let ledger: &'static Ledger = Box::leak(Box::new(ledger));
            let (_entries, guard) = mem.drain_shard_accounted_for_test(shard, false, ledger);
            ledger.observe(Category::MemtableActive, mem.size_bytes());
            let entered = Arc::new(Barrier::new(2));
            let release = Arc::new(Barrier::new(2));
            std::thread::scope(|scope| {
                let entered_worker = Arc::clone(&entered);
                let release_worker = Arc::clone(&release);
                let handle = scope.spawn(move || -> Result<(), &'static str> {
                    let _guard = guard;
                    entered_worker.wait();
                    release_worker.wait();
                    if inject_error {
                        Err("injected finalizer error")
                    } else {
                        Ok(())
                    }
                });
                entered.wait();
                assert_eq!(
                    ledger
                        .gauge(Category::MemtableActive, Measurement::Estimated)
                        .current,
                    0
                );
                assert!(
                    ledger
                        .gauge(Category::FlushDrained, Measurement::Estimated)
                        .current
                        > 0
                );
                release.wait();
                assert_eq!(handle.join().unwrap().is_err(), inject_error);
            });
            assert_eq!(
                ledger
                    .gauge(Category::FlushDrained, Measurement::Estimated)
                    .current,
                0
            );
        }
    }
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
/// the legacy `drain_with_sources` / `drain` / `get_source` paths, which
/// are NOT the segment-FTS input path — use
/// [`extract_field_values_excluding`] for anything that will be indexed.
pub fn extract_text_fields_from(source: &Value) -> HashMap<String, String> {
    extract_text_fields_from_excluding(source, &std::collections::HashSet::new())
}

pub fn extract_text_fields_from_excluding(
    source: &Value,
    excluded: &std::collections::HashSet<String>,
) -> HashMap<String, String> {
    extract_field_values_excluding(source, excluded)
        .into_iter()
        .map(|(k, v)| (k, v.iter().collect::<Vec<_>>().join(" ")))
        .collect()
}

/// Extract the top-level source fields as the segment-FTS writer wants them:
/// one [`FieldValues`] per field, with a JSON **array preserved as N separate
/// values** rather than joined into one string.
///
/// This is the single walker feeding both segment-build paths — the flush path
/// (`do_flush_shard` → `add_documents_parallel`) and the merge path
/// (`run_merge_once`).  Before #332 each path had its own copy
/// (`memtable::extract_text_value` and `index::extract_field_text`), and both
/// copies did `arr.join(" ")`, so `{"tags":["red","blue"]}` was indexed by the
/// keyword analyzer as the single term `"red blue"` — matching neither `red`
/// nor `blue`.
///
/// It also carries #328's nested-exclusion pruning, which the merge path's own
/// copy used to do inline: a field whose value is an OBJECT with an excluded
/// descendant (a `dense_vector` mapped at `passages.vec`) is flattened from a
/// PRUNED copy of that object, so the segment builder never resurrects the
/// vector's decimal components as a term under the parent's name. Unifying the
/// two paths onto this walker would otherwise have dropped that pruning on the
/// merge path (`.passages.fst` back to ~398 KB); doing it here gives it to the
/// flush path too, matching the live-insert path (`collect_text_fields`). It
/// only fires for objects, so #332's array handling is untouched.
pub fn extract_field_values_excluding(
    source: &Value,
    excluded: &std::collections::HashSet<String>,
) -> HashMap<String, xerj_fts::index::FieldValues> {
    use xerj_fts::index::FieldValues;
    let mut out = HashMap::new();
    if let Some(obj) = source.as_object() {
        for (key, val) in obj {
            if excluded.contains(key) {
                continue;
            }
            // #328 — object field with an excluded descendant: flatten a pruned
            // copy so a nested `dense_vector` cannot leak into the parent
            // object's term dictionary. Arrays never enter this arm, so the
            // #332 split below still owns every multi-valued keyword field.
            if val.is_object() && has_excluded_descendant(key, excluded) {
                let text = extract_text_value_excluding(val, key, excluded);
                if !text.is_empty() {
                    out.insert(key.clone(), FieldValues::One(text));
                }
                continue;
            }
            if let Some(values) = extract_field_values(val) {
                out.insert(key.clone(), values);
            }
        }
    }
    out
}

/// Convert one JSON value into the values the FTS writer should index for it.
///
/// * scalars → `One`
/// * arrays → one entry per element, recursively flattened (a nested array is
///   itself multi-valued in ES; an object element is JSON-encoded as before)
/// * objects → `One(json)`, unchanged: the root-level JSON blob that
///   flattened-style whole-object queries rely on
/// * `null` / empty → `None`, i.e. the field is not indexed for this doc
///
/// Empty strings are dropped INSIDE an array too — the pre-#332 code produced
/// them only as a by-product of `join(" ")` on `[null, "x"]`, and an empty
/// keyword token would otherwise become a real, matchable term.
fn extract_field_values(val: &Value) -> Option<xerj_fts::index::FieldValues> {
    use xerj_fts::index::FieldValues;
    match val {
        Value::Array(arr) => {
            let mut values: Vec<String> = Vec::with_capacity(arr.len());
            collect_array_values(arr, &mut values);
            if values.is_empty() {
                None
            } else {
                Some(FieldValues::from_values(values))
            }
        }
        _ => {
            let text = extract_text_value(val);
            if text.is_empty() {
                None
            } else {
                Some(FieldValues::One(text))
            }
        }
    }
}

fn collect_array_values(arr: &[Value], out: &mut Vec<String>) {
    for element in arr {
        match element {
            // A nested array contributes its own elements as separate values —
            // ES flattens arrays of arrays for indexing purposes.
            Value::Array(inner) => collect_array_values(inner, out),
            _ => {
                let text = extract_text_value(element);
                if !text.is_empty() {
                    out.push(text);
                }
            }
        }
    }
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
                // Numeric column FIRST, matching the on-disk segment
                // encoding (`build_doc_value_columns` in index.rs: a
                // boolean is always the f64 bit-pattern of 1.0/0.0, never
                // a keyword-only value) — the disk-segment builder reads
                // straight from the same `Value` tree and has always
                // gotten this right, but the memtable's own in-memory
                // representation never populated `numeric` for booleans at
                // all, only `keyword`. Any query still hitting a
                // memtable-resident boolean doc through a numeric-column
                // path (`ScoreEval::Bool`, `doc_values_numeric_count`,
                // range/terms-agg fast paths) saw the field as absent —
                // found empirically: a real OpenSearch Dashboards filter
                // pill on a boolean field returned 0 hits against
                // freshly-bulk-imported data (matches every
                // real-world/OSD import shape — natural background flush,
                // no explicit `_flush` — while the aggregation reading
                // the SAME field correctly bucketed true/false, since
                // aggregations use a different, disk-segment-only read
                // path that was never affected).
                let ncol = entry_no_clone(&mut self.numeric, field, Default::default);
                pad_to(ncol, doc_index);
                ncol.push(Some(if *b { 1.0 } else { 0.0 }));
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
    /// #191 — reduce per-shard bounded candidates to the globally best `limit`
    /// under the order the final page sort uses.
    ///
    /// Every hit these paths produce carries the SAME score (`1.0` for the
    /// id-only and doc-values collectors; a top-level `constant_score` rewrites
    /// them all to the wrapper's boost), so the page order reduces to
    /// `(seq_no ASC, _id ASC)` — the tail of `(score DESC, seq_no ASC, _id ASC)`
    /// at `index.rs:16222-16228`.
    ///
    /// Returns the surviving cap-th `seq_no` as the next shard's cutoff, so the
    /// walk can stop cloning ids that can no longer reach the page.
    fn narrow_to_page<T>(cands: &mut Vec<(u64, String, T)>, limit: usize) -> u64 {
        if cands.len() <= limit {
            return u64::MAX;
        }
        cands.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        cands.truncate(limit);
        cands.last().map_or(u64::MAX, |c| c.0)
    }

    pub fn doc_ids_bounded(&self, limit: usize) -> (Vec<String>, u64) {
        // #191 — this used to fill greedily from shard 0 (`limit - out.len()`
        // of room for each later shard), so an index whose memtable held more
        // than `limit` documents returned "shard 0's first `limit` ids", not
        // "the `limit` earliest-arriving ids".  Shard assignment is by doc-id
        // hash (and by BATCH for bulk), so that set is neither arrival order
        // nor stable, and `size:5` stopped being a prefix of `size:1000` on an
        // all-tied corpus.  Bound each shard independently and keep the global
        // best `limit` by `(seq_no, _id)` instead.
        let mut cands: Vec<(u64, String, ())> = Vec::with_capacity(limit.min(4096));
        let mut total: u64 = 0;
        let mut cutoff = u64::MAX;
        for s in &self.shards {
            let g = s.read();
            total += g.doc_count() as u64;
            cands.extend(
                g.ranked_ids_up_to(limit, cutoff)
                    .into_iter()
                    .map(|(q, id)| (q, id, ())),
            );
            cutoff = Self::narrow_to_page(&mut cands, limit);
        }
        (cands.into_iter().map(|(_, id, ())| id).collect(), total)
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

    /// `(seq_no, doc_id, source_arc)` for every buffered document.
    ///
    /// #191 — the brute per-doc scan's bounded collector has to decide whether
    /// a document it reaches AFTER filling the cap still belongs on the page.
    /// Shards are concatenated in index order, which is not arrival order, so
    /// that decision needs the document's arrival `seq_no`.  Carrying it in the
    /// snapshot costs 8 bytes per entry and saves a `VersionMap` hash lookup
    /// per match on the hottest memtable read path.
    pub fn all_docs_with_seq_arc(&self) -> Vec<(u64, String, Arc<Value>)> {
        let mut out = Vec::new();
        for s in &self.shards {
            out.extend(s.read().all_docs_with_seq_arc());
        }
        out
    }

    /// Columnar-filtered twin of `all_docs_with_sources_arc`: materialise ONLY
    /// the `(doc_id, source_arc)` pairs matching `preds` (a pure conjunction of
    /// Term/Range predicates), so a filtered `size:0` aggregation folds
    /// O(matching) docs instead of hydrating the whole memtable under the shard
    /// read lock.  Returns `None` (caller must fall back to
    /// `all_docs_with_sources_arc`) when any shard can't prove column↔source
    /// equivalence for a predicate field — see `filtered_docs_arc_into`.
    ///
    /// Each shard's enumeration + source materialisation happens under a single
    /// `s.read()` held only for that shard's walk (the same brief per-shard hold
    /// `all_docs_with_sources_arc` takes), so this both shrinks the work and the
    /// lock hold that the mixed read-under-write contention is bound on.
    pub fn filtered_docs_arc(&self, preds: &[MemBoolPred]) -> Option<Vec<(String, Arc<Value>)>> {
        if preds.is_empty() {
            return None;
        }
        let mut out: Vec<(String, Arc<Value>)> = Vec::new();
        for s in &self.shards {
            let g = s.read();
            if g.doc_count() == 0 {
                continue;
            }
            g.filtered_docs_arc_into(preds, &mut out)?;
        }
        Some(out)
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
        self.search_text_boosted_inner(query, fields, limit, limit, field_boosts, None)
            .0
    }

    /// `search_text_boosted` with counting DECOUPLED from materialisation:
    /// returns the top-`limit` hits plus the EXACT number of matching docs
    /// across all shards, independent of `limit`.  Shards are searched
    /// uncapped — the per-match score-map entry is paid either way; only
    /// the `MemtableHit` materialisation grows — which is exactly the cost
    /// the `count_only` path already paid via `usize::MAX`.  This is the
    /// memtable twin of the segment `search_bounded` (hits, seg_total)
    /// contract: pre-fix, size>0 match totals capped the memtable
    /// contribution at the ~256 fetch limit (b7 DEFECT 1b).
    pub fn search_text_boosted_with_total(
        &self,
        query: &str,
        fields: &[&str],
        limit: usize,
        field_boosts: &std::collections::HashMap<String, f32>,
    ) -> (Vec<MemtableHit>, u64) {
        self.search_text_boosted_inner(query, fields, limit, usize::MAX, field_boosts, None)
    }

    /// `search_text_boosted_with_total` scored against INDEX-WIDE BM25
    /// statistics (#188).
    ///
    /// `stats` is the union over every live arm (all segments + this
    /// memtable), computed once per search by the engine.  Without it the
    /// memtable's "global" statistics are global only across its own shards —
    /// so a document whose live copy has just been moved here by an overwrite
    /// scores against `N = 1`, `df = 1`, `dl/avgdl = 1`, and outranks every
    /// correctly-normalised segment hit.
    ///
    /// `None` reproduces `search_text_boosted_with_total` bit-for-bit; the
    /// engine passes `None` when the index has at most one live scoring arm.
    pub fn search_text_boosted_with_total_using(
        &self,
        query: &str,
        fields: &[&str],
        limit: usize,
        field_boosts: &std::collections::HashMap<String, f32>,
        stats: Option<&xerj_fts::CollectionStats>,
    ) -> (Vec<MemtableHit>, u64) {
        self.search_text_boosted_inner(query, fields, limit, usize::MAX, field_boosts, stats)
    }

    /// This memtable's contribution to the index-wide BM25 collection
    /// statistics (#188): per-field `FieldStats` and per-(field, term)
    /// doc_freq for the analysed `query` tokens, restricted to `fields`
    /// (empty ⇒ every indexed field).
    ///
    /// Ghost-inclusive, exactly like the in-search aggregation it was hoisted
    /// out of: tombstoned and superseded versions count until a flush/merge
    /// purges them, which is what Lucene does and what keeps a delete from
    /// silently shifting every score.
    ///
    /// Returns `None` when the memtable can't analyse the query (no analyzer,
    /// or the query analyses to zero tokens) — the caller must then not build
    /// index-wide stats at all rather than build partial ones.
    pub fn collection_stats(
        &self,
        query: &str,
        fields: &[&str],
    ) -> Option<xerj_fts::CollectionStats> {
        let analyzer = self.shards.iter().find_map(|s| {
            let g = s.read();
            g.registry
                .get_analyzer("default")
                .or_else(|| g.registry.get_analyzer("standard"))
        })?;
        let q_tokens = analyzer.analyze(query);
        if q_tokens.is_empty() {
            return None;
        }
        let (field_total_len, term_df) = self.aggregate_shard_stats(&q_tokens, fields);
        let mut out = xerj_fts::CollectionStats::new();
        for (fname, (sum, n)) in field_total_len {
            out.add_field(
                &fname,
                &xerj_fts::FieldStats {
                    total_docs: n,
                    total_field_length: sum.round() as u64,
                },
            );
        }
        for ((fname, term), df) in term_df {
            out.add_doc_freq(&fname, &term, df);
        }
        Some(out)
    }

    /// Shared cross-shard fold behind both [`Self::collection_stats`] and the
    /// in-search aggregation: `(per-field (Σ field_len, docs-with-field),
    /// per-(field, term) doc_freq)`, ghosts included.
    fn aggregate_shard_stats(
        &self,
        q_tokens: &[xerj_fts::analyzer::Token],
        fields: &[&str],
    ) -> (
        std::collections::HashMap<String, (f64, u64)>,
        std::collections::HashMap<(String, String), u64>,
    ) {
        let mut field_total_len: std::collections::HashMap<String, (f64, u64)> =
            std::collections::HashMap::new();
        let mut term_df: std::collections::HashMap<(String, String), u64> =
            std::collections::HashMap::new();
        for shard in &self.shards {
            let g = shard.read();
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
                for token in q_tokens {
                    if let Some(pl) = postings.get(&token.text) {
                        *term_df
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
                for token in q_tokens {
                    if let Some(df) = terms.get(&token.text) {
                        *term_df
                            .entry((fname.clone(), token.text.clone()))
                            .or_insert(0) += *df;
                    }
                }
            }
        }
        (field_total_len, term_df)
    }

    /// Shared body: `shard_limit` caps each shard's hit materialisation
    /// (`usize::MAX` ⇒ the returned total is the exact global match count);
    /// `limit` caps the merged, score-sorted result.
    fn search_text_boosted_inner(
        &self,
        query: &str,
        fields: &[&str],
        limit: usize,
        shard_limit: usize,
        field_boosts: &std::collections::HashMap<String, f32>,
        external: Option<&xerj_fts::CollectionStats>,
    ) -> (Vec<MemtableHit>, u64) {
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
            None => return (Vec::new(), 0),
        };
        let q_tokens = analyzer.analyze(query);
        if q_tokens.is_empty() {
            return (Vec::new(), 0);
        }

        // ── Statistics fed to the per-shard scorers ──────────────────────
        //
        // TWO modes, and the difference is deliberate:
        //
        //  * `external = Some(cs)` (#188) — the engine computed the INDEX-WIDE
        //    union (all segments + this memtable) and every arm scores against
        //    it, so a document's score no longer depends on which arm holds it.
        //    `N` is then per-field docs-with-field, matching Lucene's pairing
        //    of `FieldStats.docCount()` with `TermStats.docFreq()`.
        //  * `external = None` — the historical memtable-only aggregation,
        //    global across SHARDS with a single scalar `N`.  Kept byte-exact
        //    because the engine takes this path whenever the index has at most
        //    one live scoring arm, where the union is the identity, and any
        //    drift there would move every memtable-only score.
        //
        // Delete-aware in both modes (Lucene/ES parity): the scoring `N`
        // counts live docs AND tombstoned/superseded versions not yet merged
        // away — tantivy's scalar N is likewise physical, Σ `max_doc()` over
        // segments (`Bm25StatisticsProvider for Searcher::total_num_docs`,
        // tantivy/src/query/bm25.rs:38-45, MIT).  This is *only* the BM25
        // IDF `N` — hits.total and pagination still use the live
        // `doc_count()`.
        //
        // The ghost-inclusive scalar is computed ONCE here because BOTH
        // modes need the same value (#193): the external mode falls back to
        // it for any scored field the supplied union does not carry (or
        // carries with `total_docs == 0`), and using the live-only count
        // there silently raised IDF on indices holding tombstoned or
        // superseded versions — an unintended divergence from the `None`
        // path that the previous comment ("scalar N is unused in this
        // mode") wrongly waved off.
        let ghost_inclusive_n: u64 = {
            let mut n: u64 = self.doc_count() as u64;
            for shard in &self.shards {
                n += shard.read().ghost_docs;
            }
            n
        };
        let mut global_field_doc_count: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        let global_doc_count: u64;
        let global_avg_field_len: std::collections::HashMap<String, f32>;
        let term_global_df: std::collections::HashMap<(String, String), u64>;
        match external {
            Some(cs) => {
                // Scalar N backs the per-field fallback (field absent from
                // the union) — keep it ghost-inclusive, exactly like the
                // `None` branch, so the two modes agree on IDF wherever
                // the fallback engages.
                global_doc_count = ghost_inclusive_n;
                let mut avg = std::collections::HashMap::new();
                for (fname, fs) in cs.field_iter() {
                    avg.insert(fname.clone(), fs.avg_field_length());
                    global_field_doc_count.insert(fname.clone(), fs.total_docs);
                }
                global_avg_field_len = avg;
                let mut df = std::collections::HashMap::new();
                for token in &q_tokens {
                    for fname in global_avg_field_len.keys() {
                        if !fields.is_empty() && !fields.iter().any(|f| f == fname) {
                            continue;
                        }
                        if let Some(v) = cs.df(fname, &token.text) {
                            df.insert((fname.clone(), token.text.clone()), v);
                        }
                    }
                }
                term_global_df = df;
            }
            None => {
                global_doc_count = ghost_inclusive_n;
                let (field_total_len, df) = self.aggregate_shard_stats(&q_tokens, fields);
                global_avg_field_len = field_total_len
                    .into_iter()
                    .map(|(k, (sum, n))| (k, if n == 0 { 0.0 } else { (sum / n as f64) as f32 }))
                    .collect();
                term_global_df = df;
            }
        }

        let mut all: Vec<MemtableHit> = Vec::new();
        for s in &self.shards {
            all.extend(s.read().search_text_with_global_stats(
                query,
                fields,
                shard_limit,
                global_doc_count,
                &global_field_doc_count,
                &global_avg_field_len,
                &term_global_df,
                field_boosts,
            ));
        }
        // #191 — the cross-shard merge sorts by the FULL page key, not by score
        // alone.  Score alone leaves ties in shard-concatenation order, so
        // `truncate(limit)` kept "the tied documents that happen to live in the
        // lowest-numbered shards" rather than the earliest-arriving ones, and a
        // `size:5` page stopped being a prefix of a `size:1000` one.
        sort_hits_by_page_key(&mut all);
        // A doc_id lives in exactly ONE shard, so `all` has no duplicates:
        // its pre-truncation length IS the exact global match count when
        // `shard_limit == usize::MAX` (a lower bound otherwise).
        let total = all.len() as u64;
        all.truncate(limit);
        (all, total)
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

    /// Restore documents drained by a flush whose pre-publication finalizer
    /// failed. The caller supplies the exact text fields captured during the
    /// original drain, avoiding schema re-inference on this rare recovery
    /// path. Newer writes must be filtered by the caller before restoration.
    pub(crate) fn restore_failed_flush(
        &self,
        entries: Vec<(
            u64,
            String,
            HashMap<String, xerj_fts::index::FieldValues>,
            Arc<Value>,
        )>,
        version_map: &xerj_storage::version_map::VersionMap,
    ) {
        for (seq_no, doc_id, fields, source) in entries {
            let shard_idx = self.shard_for_dynamic(&doc_id);
            let mut shard = self.shards[shard_idx].write();
            // This check deliberately happens while holding the owning shard
            // lock. A concurrent PUT/DELETE publishes its version before it
            // mutates this shard; checking outside the lock would allow a
            // stale drained copy to overwrite the newer doc_id_index entry.
            let Some(current) = version_map.get(&doc_id) else {
                continue;
            };
            if current.seq_no != seq_no
                || current.segment_id.as_ref() != xerj_storage::version_map::IN_MEMORY_SEGMENT_ID
                || current.deleted
            {
                continue;
            }
            let analyzer = shard
                .registry
                .get_analyzer("default")
                .or_else(|| shard.registry.get_analyzer("standard"))
                .expect("standard analyzer always present");
            // Each value of a multi-valued field is analyzed on its own and the
            // token streams concatenated (#332). The memtable's postings carry
            // term frequencies only — no positions — so no position gap is
            // needed here; the gap lives in the segment writer, which is where
            // positions exist.
            let analyzed: Vec<(String, Vec<Token>)> = fields
                .into_iter()
                .map(|(field, values)| {
                    let tokens = values
                        .iter()
                        .flat_map(|value| analyzer.analyze(value))
                        .collect();
                    (field, tokens)
                })
                .collect();
            let size = (source.to_string().len() + doc_id.len()) * 3 + 64;
            shard.insert_analyzed(seq_no, doc_id, source, &analyzed, size);
        }
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
        // #191 — bound each shard independently and keep the global best
        // `limit` by `(seq_no, _id)`; `limit - out.len()` starved every shard
        // after the first, so the page was "shard 0's first `limit` matches".
        let mut cands: Vec<(u64, String, usize)> = Vec::new();
        let mut total: u64 = 0;
        for s in &self.shards {
            let g = s.read();
            if g.doc_count() == 0 {
                continue;
            }
            let (hits, t) = g.doc_values_bool_hits(preds, limit)?;
            cands.extend(
                hits.into_iter()
                    .map(|(id, pos)| (g.seq_no_at(pos), id, pos)),
            );
            total += t;
            Self::narrow_to_page(&mut cands, limit);
        }
        Some((
            cands.into_iter().map(|(_, id, pos)| (id, pos)).collect(),
            total,
        ))
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

    /// True when ANY shard's single-valued doc-values column for `field` is
    /// lossy, so no shard's column may be trusted for whole-value matching.
    ///
    /// #332 — the per-shard `doc_values_term_query` / `doc_values_terms_query`
    /// return `None` for two very different reasons: "this shard holds no
    /// column for the field" (a true zero-match answer) and "this shard's
    /// column cannot answer" (array-valued → first element only;
    /// analyzed-text → whitespace values). The cross-shard wrappers below
    /// could not tell them apart and folded a REFUSAL into "zero hits here",
    /// so a two-shard memtable with `{"tags":["red","blue"]}` in shard A and
    /// `{"tags":"red"}` in shard B answered `term tags:red` with shard B's
    /// doc only. `doc_values_bool_query` already propagates a per-shard
    /// refusal globally (`?` inside its loop); this makes the single-leaf
    /// wrappers behave the same way.
    fn dv_column_unusable(&self, field: &str) -> bool {
        self.shards.iter().any(|s| {
            let g = s.read();
            g.doc_values.array_fields.contains(field)
                || g.doc_values.keyword_has_whitespace.contains(field)
        })
    }

    /// True when the memtable's single-valued doc-values column for `field`
    /// cannot authoritatively answer a `term`/count query: the field has
    /// carried an ARRAY (only the first element is stored — see `push_field`)
    /// or a whitespace keyword (the column holds analyzed-text source). Callers
    /// that count via the column MUST fall back to the stored-source scan,
    /// which matches array membership. This is the same guard `doc_values_term_query`
    /// and `doc_values_bool_hits` already apply; exposed so the `try_shortcut_count`
    /// bare-`term` count path can bail identically (#423/#408: a `term` on a
    /// keyword array counted 0 while buffered but N after flush).
    pub fn term_count_needs_source_scan(&self, field: &str) -> bool {
        self.dv_column_unusable(field)
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
        if self.dv_column_unusable(field) {
            return None;
        }
        let mut cands: Vec<(u64, String, usize)> = Vec::new();
        let mut total: u64 = 0;
        let mut any_hit = false;
        for s in &self.shards {
            let g = s.read();
            // Step 1: bound the id clone per shard; the total is still exact
            // per shard.  Mirrors `doc_values_bool_query`.  #191 — the bound
            // used to be the REMAINING global window, which handed the whole
            // page to shard 0.
            if let Some((hits, t)) = g.doc_values_term_query(field, value, limit) {
                any_hit = true;
                cands.extend(
                    hits.into_iter()
                        .map(|(id, pos)| (g.seq_no_at(pos), id, pos)),
                );
                total += t;
                Self::narrow_to_page(&mut cands, limit);
            }
        }
        if any_hit {
            Some((
                cands.into_iter().map(|(_, id, pos)| (id, pos)).collect(),
                total,
            ))
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
        // #191 — per-shard bound + global `(seq_no, _id)` narrowing, as in
        // `doc_values_term_query`.
        // #332 — one lossy shard poisons the whole field; see
        // `dv_column_unusable`.
        if self.dv_column_unusable(field) {
            return None;
        }
        let mut cands: Vec<(u64, String, usize)> = Vec::new();
        let mut total: u64 = 0;
        let mut any_hit = false;
        for s in &self.shards {
            let g = s.read();
            if let Some((hits, t)) = g.doc_values_terms_query(field, values, limit) {
                any_hit = true;
                cands.extend(
                    hits.into_iter()
                        .map(|(id, pos)| (g.seq_no_at(pos), id, pos)),
                );
                total += t;
                Self::narrow_to_page(&mut cands, limit);
            }
        }
        if any_hit {
            Some((
                cands.into_iter().map(|(_, id, pos)| (id, pos)).collect(),
                total,
            ))
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
        // #191 — per-shard bound + global `(seq_no, _id)` narrowing, as in
        // `doc_values_term_query`.
        let mut cands: Vec<(u64, String, usize)> = Vec::new();
        let mut total: u64 = 0;
        let mut any_hit = false;
        for s in &self.shards {
            let g = s.read();
            if let Some((hits, t)) = g.doc_values_range_query(field, gte, gt, lte, lt, limit) {
                any_hit = true;
                cands.extend(
                    hits.into_iter()
                        .map(|(id, pos)| (g.seq_no_at(pos), id, pos)),
                );
                total += t;
                Self::narrow_to_page(&mut cands, limit);
            }
        }
        if any_hit {
            Some((
                cands.into_iter().map(|(_, id, pos)| (id, pos)).collect(),
                total,
            ))
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
        self.drain_shard_inner(shard_idx, false, None).0
    }

    /// Drain without parsing raw-bytes entries. Returns Value::Null for
    /// entries that came from insert_raw_bytes_with_seq. Use when neither
    /// FTS nor DV sidecars will be built (turbo/CLI ingest path).
    pub fn drain_shard_raw(&self, shard_idx: usize) -> Vec<(u64, String, Arc<Value>, Arc<[u8]>)> {
        self.drain_shard_inner(shard_idx, true, None).0
    }

    pub fn drain_shard_accounted(
        &self,
        shard_idx: usize,
        skip_parse: bool,
    ) -> (
        Vec<(u64, String, Arc<Value>, Arc<[u8]>)>,
        crate::ingest_memory::Retained<'static>,
    ) {
        let ledger = crate::ingest_memory::active_ledger();
        let (entries, guard) = self.drain_shard_inner(shard_idx, skip_parse, ledger);
        (
            entries,
            guard.unwrap_or_else(crate::ingest_memory::Retained::disabled),
        )
    }

    #[cfg(test)]
    pub(crate) fn drain_shard_accounted_for_test(
        &self,
        shard_idx: usize,
        skip_parse: bool,
        ledger: &'static crate::ingest_memory::Ledger,
    ) -> (
        Vec<(u64, String, Arc<Value>, Arc<[u8]>)>,
        crate::ingest_memory::Retained<'static>,
    ) {
        let (entries, guard) = self.drain_shard_inner(shard_idx, skip_parse, Some(ledger));
        (entries, guard.expect("test ledger always creates guard"))
    }

    fn drain_shard_inner<'a>(
        &self,
        shard_idx: usize,
        skip_parse: bool,
        ledger: Option<&'a crate::ingest_memory::Ledger>,
    ) -> (
        Vec<(u64, String, Arc<Value>, Arc<[u8]>)>,
        Option<crate::ingest_memory::Retained<'a>>,
    ) {
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
        let (drained, dead, drain_guard) = {
            let mut g = self.shards[shard_idx].write();
            let removed_bytes = g.total_bytes;
            let d: Vec<MemEntry> = std::mem::take(&mut g.docs);
            let dead_index = std::mem::take(&mut g.index);
            let dead_dv = std::mem::take(&mut g.doc_values);
            let dead_fl = std::mem::take(&mut g.field_lengths);
            let dead_afl = std::mem::take(&mut g.avg_field_lengths);
            let dead_dii = std::mem::take(&mut g.doc_id_index);
            g.total_bytes = 0;
            // Created while the authoritative shard lock is still held so
            // the drained lifetime includes detached-map handoff and parsing.
            // Periodic active-vs-drained snapshots remain best-effort because
            // the two authorities are sampled separately.
            let drain_guard = ledger.map(|ledger| {
                crate::ingest_memory::Retained::for_ledger(
                    ledger,
                    crate::ingest_memory::Category::FlushDrained,
                    removed_bytes,
                )
            });
            // Flush == merge: purge delete-aware ghost collection stats.
            g.ghost_docs = 0;
            let dead_gfl = std::mem::take(&mut g.ghost_field_len);
            let dead_gdf = std::mem::take(&mut g.ghost_doc_freq);
            (
                d,
                (
                    dead_index, dead_dv, dead_fl, dead_afl, dead_dii, dead_gfl, dead_gdf,
                ),
                drain_guard,
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
        (out, drain_guard)
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
        let mut drained: Vec<MemEntry> = std::mem::take(&mut self.docs);
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
        let mut drained: Vec<MemEntry> = std::mem::take(&mut self.docs);
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
            &std::collections::HashMap::new(),
        )
    }

    /// search_text variant that uses caller-supplied GLOBAL doc_count,
    /// per-field avg lengths, and per-(field,term) doc frequencies.
    /// Falls back to local stats when the global maps are empty.
    ///
    /// `global_field_doc_count` is the per-field `N` (docs WITH the field).
    /// When it carries the field, it wins over the scalar `global_doc_count` —
    /// that is the Lucene pairing (`FieldStats.docCount()` with
    /// `TermStats.docFreq()`, `BM25Similarity.idfExplain`) and the shape the
    /// index-wide statistics of #188 arrive in.  Empty ⇒ the historical
    /// single-scalar behaviour, so memtable-only searches score unchanged.
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
        global_field_doc_count: &std::collections::HashMap<String, u64>,
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
                // Per-field N (docs WITH the field) when the caller supplied
                // index-wide statistics; the scalar otherwise.
                let field_doc_count = global_field_doc_count
                    .get(*field_name)
                    .copied()
                    .filter(|n| *n > 0)
                    .unwrap_or(doc_count);

                let scorer = Bm25Scorer::new(avg_field_len, field_doc_count);
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
            .map(|(doc_id, score)| {
                // #191 — carry the arrival order with the hit.  `scores` is a
                // HashMap, so without it the pre-truncation order of an
                // all-tied hit set is the hash iteration order: the surviving
                // documents changed from run to run.
                let seq_no = self
                    .doc_id_index
                    .get(&doc_id)
                    .map_or(u64::MAX, |&pos| self.seq_no_at(pos));
                MemtableHit {
                    doc_id: doc_id.to_string(),
                    score,
                    seq_no,
                }
            })
            .collect();

        // Sort by the FULL page key (score DESC, seq_no ASC, `_id` ASC) — the
        // comparator `index.rs:16222-16228` applies to the final page — so a
        // truncation at `limit` keeps exactly the documents that page would.
        sort_hits_by_page_key(&mut hits);
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

    /// The WAL `seq_no` of the document at shard-local position `pos`
    /// (`u64::MAX` when the position is out of range).
    ///
    /// #191 — the bounded memtable paths must rank candidates ACROSS shards
    /// by the same key the final page sort uses, and `seq_no` is that key's
    /// tie-break.  Positions are only comparable within a shard; `seq_no` is
    /// global.
    pub fn seq_no_at(&self, pos: usize) -> u64 {
        self.docs.get(pos).map_or(u64::MAX, |e| e.seq_no)
    }

    /// `(seq_no, doc_id)` for the first `n` buffered documents, stopping early
    /// once an entry's `seq_no` exceeds `cutoff`.
    ///
    /// #191 — `docs` is append-ordered and `remove` preserves that order, so a
    /// shard's entries ascend by `seq_no`: once one entry is past the caller's
    /// running cap-th key, so is every entry after it, and neither needs its
    /// `doc_id` cloned.  That early break is what keeps the per-shard bound
    /// (which replaced a greedy "fill from shard 0" walk) from multiplying the
    /// id clones by the shard count.
    pub fn ranked_ids_up_to(&self, n: usize, cutoff: u64) -> Vec<(u64, String)> {
        let mut out = Vec::new();
        for e in self.docs.iter().take(n) {
            if e.seq_no > cutoff {
                break;
            }
            out.push((e.seq_no, e.doc_id.clone()));
        }
        out
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

    /// `seq_no`-carrying twin of [`Self::all_docs_with_sources_arc`] (#191).
    pub fn all_docs_with_seq_arc(&self) -> Vec<(u64, String, Arc<Value>)> {
        self.docs
            .iter()
            .map(|e| (e.seq_no, e.doc_id.clone(), Self::resolve_source_arc(e)))
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
        let mut drained: Vec<MemEntry> = std::mem::take(&mut self.docs);
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
        let mut drained: Vec<MemEntry> = std::mem::take(&mut self.docs);
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
    ///
    /// #332 — the array bailout is part of "mirror the standalone paths
    /// exactly" and was MISSING here: `doc_values_term_query`,
    /// `doc_values_terms_query` and `filtered_docs_arc` all refuse a field
    /// that has ever carried an array, because `push_field` stores only the
    /// array's FIRST element in these single-valued columns. This walk did
    /// not, so it answered a bare `term` on a multi-valued keyword field
    /// from the lossy column and silently dropped every doc whose match was
    /// in a non-first element (`{"tags":["red","blue"]}` → `term tags:blue`
    /// = 0 hits) for as long as the doc stayed memtable-resident. It is the
    /// arm a plain `term` search actually takes (`is_doc_scan_query` →
    /// fused columnar, ahead of `try_doc_values_query`), so the standalone
    /// path's bailout never got a chance to fire.
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
                    // Array-valued field: the column is lossy (first element
                    // only) → bail so a later matching element can't be
                    // dropped (#332; mirrors `filtered_docs_arc`).
                    if self.doc_values.array_fields.contains(field.as_str()) {
                        return None;
                    }
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
                    } else {
                        // Numeric term predicate: exact f64 equality (the
                        // predicate value was lowered to its string form).
                        let col = self.doc_values.numeric.get(field.as_str())?;
                        let needle = value.parse::<f64>().ok()?;
                        cols.push(Col::NumEq(col, needle));
                    }
                }
                MemBoolPred::Range {
                    field,
                    gte,
                    gt,
                    lte,
                    lt,
                } => {
                    if self.doc_values.array_fields.contains(field.as_str()) {
                        return None;
                    }
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

    /// Push `(doc_id, source_arc)` for every buffered position whose
    /// doc-values columns satisfy EVERY predicate (a pure conjunction of
    /// Term/Range).  Column resolution + per-predicate match semantics are the
    /// exact ones `doc_values_bool_hits` uses, so this enumerates the same doc
    /// set the fused-bool search walk would — but UNBOUNDED (all matches, the
    /// aggregation contract) and materialising the matching sources instead of
    /// just ids.
    ///
    /// Returns `None` (caller must fall back to the full-corpus walk) whenever
    /// column↔source equivalence with the JSON matcher isn't provable for a
    /// predicate field:
    ///   * the field has EVER carried an ARRAY value in this shard — the column
    ///     keeps only the first element, so a later element that matches under
    ///     `doc_matches_filter` would be silently dropped (a false negative the
    ///     aggregation could NOT recover);
    ///   * a keyword predicate field holds analyzed-text (whitespace) values;
    ///   * a predicate field has no column at all.
    ///
    /// For the shapes it DOES accept (scalar numeric/keyword columns) the
    /// columnar match set equals the `doc_matches_filter` set — the fast-agg
    /// caller still re-applies the JSON matcher as a safety net, so any residual
    /// false POSITIVE is removed and only the provable no-false-negative
    /// guarantee is load-bearing.
    pub fn filtered_docs_arc_into(
        &self,
        preds: &[MemBoolPred],
        out: &mut Vec<(String, Arc<Value>)>,
    ) -> Option<()> {
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
                    // Array-valued field: the column is lossy (first element
                    // only) → bail so a later matching element can't be dropped.
                    if self.doc_values.array_fields.contains(field.as_str()) {
                        return None;
                    }
                    if let Some(col) = self.doc_values.keyword.get(field.as_str()) {
                        if self
                            .doc_values
                            .keyword_has_whitespace
                            .contains(field.as_str())
                        {
                            return None;
                        }
                        cols.push(Col::Kw(col, value.as_str()));
                    } else {
                        let col = self.doc_values.numeric.get(field.as_str())?;
                        let needle = value.parse::<f64>().ok()?;
                        cols.push(Col::NumEq(col, needle));
                    }
                }
                MemBoolPred::Range {
                    field,
                    gte,
                    gt,
                    lte,
                    lt,
                } => {
                    if self.doc_values.array_fields.contains(field.as_str()) {
                        return None;
                    }
                    let col = self.doc_values.numeric.get(field.as_str())?;
                    cols.push(Col::Num(col, *gte, *gt, *lte, *lt));
                }
            }
        }
        let n = self.docs.len();
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
            let e = &self.docs[idx];
            out.push((e.doc_id.clone(), Self::resolve_source_arc(e)));
        }
        Some(())
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
            //
            // Array/multi-valued bailout: a keyword ARRAY stores only its FIRST
            // element in this single-valued column (see `push_field`), so a
            // `term` on any non-first element would be silently missed. Bail to
            // the array-aware source scan (`json_values_equal` matches on array
            // membership). This is the fix for ES-incompatible term-on-array.
            if self.doc_values.keyword_has_whitespace.contains(field)
                || self.doc_values.array_fields.contains(field)
            {
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
        // Array/multi-valued keyword fields keep only their first element in the
        // single-valued column; term matching must fall back to the array-aware
        // source scan (mirrors the bail in `doc_values_term_query`).
        if self.doc_values.array_fields.contains(field) {
            return None;
        }
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
        // Array/multi-valued keyword fields (only the first element is stored
        // in the single-valued column) likewise bail to the array-aware scan.
        if self.doc_values.keyword_has_whitespace.contains(field)
            || self.doc_values.array_fields.contains(field)
        {
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
fn collect_text_fields(
    v: &Value,
    prefix: &str,
    out: &mut HashMap<String, String>,
    excluded: &std::collections::HashSet<String>,
) {
    if let Value::Object(obj) = v {
        for (k, val) in obj {
            let path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{}.{}", prefix, k)
            };
            // Matched on the FULL dotted path, not just the root key — #328.
            // Every pre-#328 member of the set is a root-level name, for which
            // `path == k` and this is the check that was here before; a
            // `dense_vector` nested under an object mapping is the first member
            // that can arrive as `passages.vec`.
            if excluded.contains(&path) {
                continue;
            }
            match val {
                Value::Object(_) => {
                    // Root-level JSON-blob for flattened-style
                    // whole-object queries.
                    //
                    // Built from a PRUNED copy of the object (#328): the blob
                    // is where a nested `dense_vector` actually landed. The
                    // recursion below skips `passages.vec`, but this line
                    // serialised the whole `passages` object — vector included
                    // — into one text field, so excluding the leaf alone moved
                    // no bytes at all. Pinned by
                    // `nested_dense_vector_is_excluded_from_its_parent_objects_term_dictionary`,
                    // which reports `<seg>.passages.fst` and requires it under
                    // 4 KiB — the fixture's 19,200 vector components cannot fit
                    // in that. Both mapping shapes are covered there: the
                    // dotted top-level name `"passages.vec"` and a `vec`
                    // sub-mapping under a `passages` object.
                    if prefix.is_empty() {
                        let t = extract_text_value_excluding(val, &path, excluded);
                        if !t.is_empty() && !out.contains_key(&path) {
                            out.insert(path.clone(), t);
                        }
                    }
                    collect_text_fields(val, &path, out, excluded);
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
    let excluded = fts_excluded_fields(schema);
    analyze_doc_excluding(source, schema, analyzer, &excluded)
}

pub fn analyze_doc_excluding(
    source: &Value,
    schema: &Schema,
    analyzer: &AnalyzerPipeline,
    excluded: &std::collections::HashSet<String>,
) -> Vec<(String, Vec<Token>)> {
    let mut text_fields: HashMap<String, String> = HashMap::new();

    // Index fields that are defined as Text in the schema.
    for field_cfg in &schema.fields {
        // `excluded` is authoritative for schema fields too, not only for the
        // dynamic-path sweep below. Before `index: false` was honoured the only
        // members were semantic-derived vector fields, which are never
        // `FieldType::Text`, so this loop could skip the check; a declared
        // `"index": false` Text field is exactly the case that needs it.
        if excluded.contains(&field_cfg.name) {
            continue;
        }
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
        collect_text_fields(source, "", &mut text_fields, excluded);
    }

    text_fields
        .into_iter()
        .map(|(field_name, text)| {
            let tokens = analyzer.analyze(&text);
            (field_name, tokens)
        })
        .collect()
}

/// Exact internal fields generated from semantic embedding mappings.
///
/// This is schema-derived: similarly named user fields are unaffected unless
/// an embedding config explicitly designates them as its target.
pub fn semantic_derived_vector_fields(schema: &Schema) -> std::collections::HashSet<String> {
    let mut excluded = std::collections::HashSet::new();
    for field in &schema.fields {
        let Some(embedding) = &field.embedding else {
            continue;
        };
        let target = embedding
            .target_field
            .clone()
            .unwrap_or_else(|| format!("{}_vector", field.name));
        excluded.insert(target.clone());
        excluded.insert(format!("{target}_chunks"));
        excluded.insert(format!(
            "{}{}",
            xerj_query::executor::PASSAGE_METADATA_PREFIX,
            target
        ));
    }
    excluded
}

/// Every field that must be kept OUT of the full-text inverted index.
///
/// Three disjoint reasons, unioned into one set so flush, merge, the memtable
/// analyser and `build_fts_field_configs` cannot disagree:
///
///  * semantic-derived vector fields (see above) — engine-internal payloads
///    that were never user text;
///  * fields the user MAPPED as `dense_vector` (`FieldType::Vector`) — #328.
///    See "Why a declared vector has no lexical form" below;
///  * fields whose mapping declares `"index": false` — #204's last open
///    accepted-and-ignored instance. The option was echoed by `GET _mapping`
///    and then ignored, so the field kept a full inverted index and stayed
///    searchable.
///
/// Derived from the SCHEMA rather than from the documents, exactly like
/// `doc_values_skip_set` — a merge must not resurrect postings that flush
/// correctly skipped. Dynamic (unmapped) fields are absent from the schema
/// and are unaffected: there is no declared `index: false` to honour.
///
/// ## Why `index: false` alone is not enough to drop the postings
///
/// Dropping a field's postings does not make it unmatchable here: the
/// per-segment `fts_has_field` gate routes the query onto the stored-doc
/// scan instead. That fallback is only usable where it is *equivalent* —
/// #204's second rule, "degrade loudly or not at all", applies to us as much
/// as to anyone.
///
///  * A **Text** field's scan comparison is analysed, which is exactly what
///    the postings did. Equivalent → the postings go.
///  * An **exact-typed** field (keyword / ip / date / numeric / boolean) is
///    FTS-indexed with the `keyword` analyzer: one whole-value token. The
///    scan's `match` arm splits on non-alphanumerics, so `192.168.0.1` would
///    start matching `192.168.0.2`. Measured, not theorised — it is the one
///    case `search/390_doc_values_search.yml` fails on. Weaker → the postings
///    stay, and they are what answers ES 8.1's "doc values search" exactly.
///    Nothing observable changes for those fields; the footprint saving is
///    knowingly forgone rather than bought with wrong answers.
///  * A field with **no doc values either** is unsearchable in ES's sense
///    (`MappedFieldType.isSearchable()` = has terms OR has doc values), and
///    `unsearchable_query_field` in `index.rs` rejects every query naming it
///    before execution — so no fallback of any strength ever runs, and the
///    postings are pure waste whatever the type.
///
/// ## Why a declared vector has no lexical form (#328)
///
/// `#12` removed the term dictionary for the vectors XERJ *generates* from a
/// semantic mapping. A field the user maps as `dense_vector` took a different
/// route through the same code and kept its own: kNN needs the HNSW graph, so
/// the field is `"index": true`, so the `index: false` arm above never fires.
/// The field then flowed through `extract_fts_fields_excluding` into
/// `extract_field_text`, whose `Value::Array` arm joins the elements with
/// spaces — turning a 128-dim vector into one enormous decimal-string term per
/// document. kNN is served by `hnsw/graph.bin`; `exists` is resolved from
/// `_source` / doc values; `_field_caps` and highlighting never open the
/// field's FST — so every one of those bytes is unreadable by construction.
///
/// Lucene draws the type line in exactly the same place and gets the same
/// consequence for free: `KnnFloatVectorField.createFieldType` sets vector
/// attributes and then `freeze()`s, and sets nothing else
/// (`lucene/core/src/java/org/apache/lucene/document/KnnFloatVectorField.java:70`;
/// `KnnByteVectorField.java:82` is identical), so `IndexOptions` stays `NONE`
/// (`lucene/core/src/java/org/apache/lucene/index/FieldInfo.java:565`) and a
/// term dictionary for the field cannot exist. `ColumnValidation` enumerates
/// "vectors" as an indexing feature DISJOINT from "index options"
/// (`lucene/core/src/java/org/apache/lucene/document/column/ColumnValidation.java:104`)
/// — the same type-level separation this is.
///
/// Measured — one harness, one corpus, built twice and diffed file by file
/// after `force_merge(1)`. 5,000 docs × 128 dim: `text` + `keyword` + `long` +
/// a `dense_vector` + its `_chunks` companion (the corpus carries the companion
/// because that is the shape RFC #148 reports). Every byte below is a
/// `std::fs::metadata().len()` sum over the data dir, not an estimate:
///
/// | file, 5,000 × 128 | `main` | this branch |
/// |---|---:|---:|
/// | `<seg>.emb.fst` | 13,256,659 B | 0 |
/// | `<seg>.emb.post` | 10,834 B | 0 |
/// | `<seg>.emb.norms` | 48 B | 0 |
/// | `<seg>.emb_chunks.fst` | 26,518,005 B | 0 |
/// | `<seg>.emb_chunks.post` | 10,834 B | 0 |
/// | `<seg>.body.fst` | 278 B | 278 B |
/// | `<seg>.cat.fst` | 53 B | 53 B |
/// | `<seg>.n.fst` | 209 B | 209 B |
/// | **durable index** (all files bar `.wal`) | **54,068,549 B** | **14,251,975 B** (−73.6%, 3.79×) |
///
/// Two observations the table is here to make. First, the term dictionary of a
/// vector-carrying index essentially *is* the vector field: the three lexical
/// `.fst` files total 540 B against 39.8 MB of vector postings. Second — and
/// this is why the exclusion cannot stop at the base name — the `_chunks`
/// companion is the BIGGER half. At 26,518,005 B it is twice `emb.fst` and 49%
/// of `main`'s whole index on its own, so a base-name-only exclusion would
/// leave more behind than it removes.
///
/// The row is the DURABLE index — every file in the data directory except
/// `.wal` — because the write-ahead log's tail is reclaimed asynchronously and
/// the whole-directory total is therefore not reproducible: runs of the 300-doc
/// regression test have put it anywhere between 858,147 B and 1,105,001 B while
/// the durable bytes moved by ~500 B. `.seg` (the stored documents, 11.2 MB
/// here) is counted on both sides and is why the ratio is 3.79× rather than the
/// 39.8 MB the term dictionaries cost outright.
///
/// The 300-doc × 128-dim fixture the regression tests actually run is the same
/// shape one order of magnitude down: `emb.fst` 796,274 B, `emb.post` 591 B,
/// `emb.norms` 313 B, `emb_chunks.fst` 1,592,118 B, `emb_chunks.post` 591 B,
/// durable index 3,249,323 B → 857,265–857,769 B (eight runs on this revision
/// printed 857,265 / 857,529 / 857,533 / 857,545 / 857,549 / 857,657 /
/// 857,657 / 857,769, a 504 B spread). That test prints both totals and asserts
/// a ceiling on the durable one, so this figure is enforced rather than merely
/// published. The same eight runs are quoted verbatim in `CHANGELOG.md` and in
/// the comment above the `eprintln!` in
/// `user_mapped_dense_vector_builds_no_fts_term_dictionary`; all three move
/// together or none of them do.
///
/// TWO RULES DECIDE THIS SET, and they are stated separately because they do
/// not have the same strength. [`lexically_typeless_fields`] has the full
/// statement; the summary is:
///
///  * BY DECLARED TYPE, for the field itself: `FieldType::Vector` and nothing
///    else. Not by value shape — an unmapped numeric array stays lexical
///    exactly as before (the same line HNSW draws in
///    `load_hnsw_artifacts_sync`, which refuses a graph pinned to a
///    non-`dense_vector` field). `FieldType::Chunk` is deliberately NOT
///    excluded: its declared payload is chunk *text* plus a vector, and only
///    the vector half is droppable — that needs a value-shape-aware rule, not
///    a type rule.
///  * BY NAME, for the two companions `<field>_chunks` and
///    `__xerj_passage_meta__<field>`, which arrive in `_source` with no
///    `FieldConfig` behind them, so there is no declared type to read. This
///    rule is the weaker of the two and YIELDS to ANY declaration: a field
///    mapped as anything other than `dense_vector` under one of those names —
///    `text`, `keyword`, `long`, `double`, `date`, `boolean`, `ip`,
///    `geo_point`, `binary`, `object`, `nested`, `chunk`, or an ES type string
///    `es_type_to_field_type` does not recognise — keeps its full lexical
///    surface. That is only sound because the companion is prevented from ever
///    ACQUIRING a declaration: `index::is_undeclared_multi_vector_companion`
///    stops dynamic mapping registering a `<vector>_chunks` key whose value is
///    a real multi-vector, so an "absent" answer here means absent.
///
/// The bytes in the table above are almost all the second rule's: `emb_chunks`
/// in that fixture is absent from the user's mapping — and, since this change,
/// absent from the evolved schema too — and it is 26,518,005 B of the
/// 39,796,380 B removed.
///
/// Removing the postings is only half of it, and the other half is NOT the
/// `*`-expansion filter. Every other member of this set is protected from the
/// stored-doc scan by `unsearchable_query_field`, which rejects the query
/// before execution; a `dense_vector` is not, because it IS indexed. So two
/// things happen on the query side:
///
///  * `text_fields` / `exact_fields` in `search_inner` drop vector fields, so a
///    `*` expansion stops projecting a clause onto them;
///  * and `index::lower_lexically_typeless_clauses` lowers any clause that
///    NAMES one to `match_none` at plan time. The expansion filter alone does
///    nothing for `{"term":{"emb":…}}` or `{"multi_match":{"fields":["emb"]}}`,
///    which name the field outright — those fall through the per-segment
///    `fts_has_field` gate onto the stored-doc scan, whose `Term` arm matches
///    ANY ELEMENT of a JSON array (`json_values_equal`) and whose `match` arm
///    splits on non-alphanumerics.
///
/// Measured on the corpus above, post-`force_merge`, three source states —
/// `main` / write-side exclusion only (postings gone, no plan-time lowering) /
/// this branch. Timings are the FIRST call for each shape, before the result
/// cache has seen it; a repeat of any row here is ~0.002 ms on all three and
/// measures the cache, not the plan:
///
/// | query | `main` | postings gone, no lowering | this branch |
/// |---|---|---|---|
/// | `term {emb: "<component>"}` | 0 hits, 89.2 ms | 0 hits, 186.8 ms | 0 hits, 0.061 ms |
/// | `term {emb: <component>}` numeric | **1 hit**, 87.6 ms | **1 hit**, 187.6 ms | **0 hits**, 0.011 ms |
/// | `terms {emb: [<component>]}` numeric | **1 hit**, 85.6 ms | **1 hit**, 112.0 ms | **0 hits**, 0.012 ms |
/// | `prefix {emb: "0"}` | **2,500 hits**, 231.8 ms | 0 hits, 95.0 ms | **0 hits**, 0.008 ms |
/// | `wildcard {emb: "0*"}` | **5,000 hits**, 316.6 ms | 0 hits, 95.1 ms | **0 hits**, 0.008 ms |
/// | `fuzzy {emb: "0"~2}` | **5,000 hits**, 798.5 ms | 0 hits, 97.7 ms | **0 hits**, 0.011 ms |
/// | `match_phrase_prefix {emb: "0"}` | **50 hits**, 84.6 ms | 0 hits, 89.3 ms | **0 hits**, 0.009 ms |
/// | `range {emb: {gte:-2, lte:2}}` | 0 hits, 119.8 ms | 0 hits, 134.5 ms | 0 hits, 0.009 ms |
/// | `multi_match {fields:["emb"], query:"0"}` | 0 hits, 0.409 ms | **5,000 hits**, 123.5 ms | 0 hits, 0.012 ms |
/// | `simple_query_string {fields:["emb"]}` | 0 hits, 0.033 ms | 0 hits, 114.5 ms | 0 hits, 0.015 ms |
/// | `term {emb_chunks: "<component>"}` | 0 hits, 86.0 ms | 0 hits, 116.6 ms | 0 hits, 0.008 ms |
/// | `wildcard {emb_chunks: "0*"}` | **5,000 hits**, 626.9 ms | 0 hits, 89.9 ms | **0 hits**, 0.007 ms |
/// | `multi_match {fields:["emb_chunks"], query:"0"}` | 0 hits, 2.24 ms | **5,000 hits**, 138.5 ms | 0 hits, 0.014 ms |
/// | `bool{should:[term emb], must_not:[term cat]}` | 0 hits, 81.4 ms | 0 hits, 92.4 ms | 0 hits, 0.011 ms |
/// | `bool{must_not:[term emb]}` | 5,000 hits, 84.9 ms | 5,000 hits, 98.9 ms | 5,000 hits, 2.31 ms |
/// | `constant_score{filter: term emb}` | 0 hits, 83.0 ms | 0 hits, 110.3 ms | 0 hits, 0.012 ms |
/// | `exists {field: emb}` | 5,000 hits, 83.0 ms | 5,000 hits, 108.0 ms | 5,000 hits, 88.5 ms |
/// | `match {body: "liquidity"}` | 5,000 hits, 5.18 ms | 5,000 hits, 5.96 ms | 5,000 hits, 4.95 ms |
/// | `term {cat: "even"}` | 2,500 hits, 2.32 ms | 2,500 hits, 2.87 ms | 2,500 hits, 2.27 ms |
///
/// The two `multi_match` rows are the ones that are not merely slow. Without the
/// lowering they answer EVERY document, because the scan renders the float array
/// to text and every component contains a `0` — a 0-hit query silently becomes
/// a match-all. Those rows are the reason the write-side change cannot ship on
/// its own.
///
/// A WHOLE CLASS OF ANSWERS CHANGES, and it is stated rather than buried. SEVEN
/// rows above differ between `main` and this branch (nine shapes on the 300-doc
/// regression fixture, which probes both companions), and they are all the same
/// thing: a `dense_vector` and its UNDECLARED `_chunks` companion no longer
/// have any lexical surface, so every lexical leaf naming one OUTRIGHT answers
/// 0. Two exclusions from that sentence, both measured and both stated where
/// they live: a `fields: ["emb.*"]` PATTERN is not lowered and still answers
/// 300 of 300 for `"0"` on the fixture, exactly as on the merge base
/// (`index::lower_lexically_typeless_clauses`'s `is_typeless_spec`), and a
/// `<field>_chunks` the user mapped `text`/`keyword`/`date`/`boolean`/`ip` is
/// not in the set at all, so nothing naming it changes
/// ([`lexically_typeless_fields`]). On `main` those
/// clauses reached the stored-doc scan, which renders the float array back to
/// decimal text and matches against THAT — its `Term` arm matches any ELEMENT of
/// a JSON array (`json_values_equal`) and its `match` arm splits on
/// non-alphanumerics — so a shape answered 0 or answered most of the corpus
/// purely according to whether the rendered decimals happened to contain the
/// probe. `wildcard {emb:"0*"}` and `fuzzy {emb:"0"~2}` matched EVERY document.
/// None of that is a retrieval semantic anyone asked for: ES rejects these
/// queries on a `dense_vector` outright (`DenseVectorFieldType` has no term
/// query) and Lucene gives a field with no terms an empty scorer
/// (`lucene/core/src/java/org/apache/lucene/index/Terms.java:40`). 0 is the
/// Lucene answer. It is still a behaviour change and the CHANGELOG describes the
/// class; `user_mapped_dense_vector_builds_no_fts_term_dictionary`'s (d) loop
/// pins it shape by shape.
///
/// `_score` MOVES TOO, for bools whose hit count does not. Dropping the dead
/// clause out of `should` makes the surviving bool projectable through
/// `query_node_to_fts_with_keyword_fields`, so BM25 scores it where the
/// stored-doc scan used to. On the 300-doc fixture,
/// `bool{should:[term emb, term cat:"even"]}` keeps its 150 hits but goes
/// `_score` 0.008402659 → 0.6931471 and `max_score` 1.6931472 → 0.6931471, and
/// `bool{must:[match body], must_not:[term emb]}` keeps its 300 hits and its
/// `_score` but goes `max_score` 1.6931472 → 0.008402659. Both moves are toward
/// consistency: on `main` these reported a `max_score` no returned hit carried.
/// Isolated to the plan-time fold — with the write-side exclusion alone and the
/// lowering disabled, both are byte-identical to `main`. Pinned in that test's
/// (d2c) block.
///
/// Not claimed and not changed: XERJ still answers 200 rather than 400 for
/// these shapes. Adding ES's rejection is a separate, breaking change.
pub fn fts_excluded_fields(schema: &Schema) -> std::collections::HashSet<String> {
    let mut excluded = semantic_derived_vector_fields(schema);
    excluded.extend(lexically_typeless_fields(schema));
    for field in &schema.fields {
        if field.options.indexed {
            continue;
        }
        let fallback_is_equivalent =
            matches!(field.field_type, FieldType::Text) || !field.options.doc_values;
        if fallback_is_equivalent {
            excluded.insert(field.name.clone());
        }
    }
    excluded
}

pub const MULTI_VECTOR_COMPANION_SUFFIX: &str = "_chunks";

/// Is `val` the exact `_source` shape RFC #148 gives a per-document
/// MULTI-VECTOR — a non-empty array of equal-length, non-empty, all-numeric
/// arrays?
///
/// This is the value-shape half of the companion rule. The schema alone cannot
/// tell a multi-vector from a user's own numeric field (see
/// [`declares_non_vector_shaped_field`]); the VALUE can, and it is available at
/// exactly the moment that matters — when dynamic mapping is about to invent a
/// `FieldConfig` for an unmapped `<vector>_chunks` key
/// (`index::evolve_schema_from_doc`/`_docs`). Refusing to invent one there is
/// what keeps the schema honest, so the plan-time side of the rule can then
/// defer to ANY declaration it does find.
///
/// The equal-length requirement is not decoration: every chunk of one document
/// embeds with the same model, so a real multi-vector is rectangular. A ragged
/// array of numeric arrays is therefore NOT one, is left to dynamic mapping,
/// and keeps its term dictionary — the fail-safe direction.
///
/// Lucene models the same object as its own field type rather than inferring a
/// numeric one: `LateInteractionField extends BinaryDocValuesField`
/// (`lucene/core/src/java/org/apache/lucene/document/LateInteractionField.java:36`),
/// constructed from a `float[][]` and encoded straight to a `BytesRef`
/// (`:44`, `:45`) — it never reaches an analyzer or a term dictionary at all.
pub(crate) fn looks_like_multi_vector(val: &serde_json::Value) -> bool {
    let Some(rows) = val.as_array() else {
        return false;
    };
    let mut dims: Option<usize> = None;
    if rows.is_empty() {
        return false;
    }
    for row in rows {
        let Some(components) = row.as_array() else {
            return false;
        };
        if components.is_empty() || !components.iter().all(serde_json::Value::is_number) {
            return false;
        }
        match dims {
            None => dims = Some(components.len()),
            Some(d) if d == components.len() => {}
            Some(_) => return false,
        }
    }
    true
}

/// Fields whose DECLARED type has no lexical representation at all — #328.
///
/// Today that is exactly `FieldType::Vector` (`dense_vector`), so the base list
/// is deliberately the SAME list, from the same walk, that decides which fields
/// get an HNSW graph: [`crate::index::collect_dense_vector_fields`]. Two
/// independent notions of "which fields are vectors" is how you end up with a
/// field that has both a graph and a term dictionary, or neither — so there is
/// one walk and this is a view of it.
///
/// Each vector contributes THREE names, exactly the shape
/// [`semantic_derived_vector_fields`] already emits per target, because a
/// `dense_vector` never arrives alone: `passage_scored_vector_fields` unions
/// user-mapped vectors with semantic targets and both grow a `<field>_chunks`
/// companion (the per-document MULTI-vector) and a
/// `__xerj_passage_meta__<field>` sidecar. Excluding only the base name leaves
/// the companion behind, and the companion is the BIGGER half: on the 5,000-doc
/// × 128-dim corpus measured in [`fts_excluded_fields`], `<seg>.emb_chunks.fst`
/// is 26,518,005 B against `<seg>.emb.fst`'s 13,256,659 B — twice the size, and
/// 49% of `main`'s whole 54,068,549 B durable index on its own.
///
/// Lucene draws the same line and never lets a multi-vector reach an analyzer:
/// its per-document float[][] shape is `LateInteractionField`, which `extends
/// BinaryDocValuesField`
/// (`lucene/core/src/java/org/apache/lucene/document/LateInteractionField.java:36`)
/// and hands its constructor straight to `super(name, encode(value))` (`:45`;
/// the constructor signature is `:44`)
/// — doc values only, no index options, so no term dictionary can exist for it
/// any more than for the single-vector `KnnFloatVectorField`
/// (`KnnFloatVectorField.java:70`: vector attributes, then `freeze()`).
///
/// TWO RULES, and they are not the same rule. The first is BY DECLARED TYPE:
/// `FieldType::Vector` has no lexical form, so the base name is excluded
/// unconditionally. The second is BY NAME: the two companions are synthesised
/// from the base name because no USER mapping declares them — they arrive in
/// `_source` from the passage pipeline, and whatever `FieldConfig` they end up
/// with is one dynamic mapping inferred from their values. A name-based rule
/// can collide with a field the user actually mapped, so it yields to ANY
/// declaration it finds:
///
///  * declared as anything other than `dense_vector` — `text`, `keyword`,
///    `long`, `double`, `date`, `boolean`, `ip`, `geo_point`, `binary`,
///    `object`, `nested`, `chunk`, or a mapping type
///    `es_type_to_field_type` did not recognise — the declaration wins and the
///    field keeps its term dictionary and its whole lexical surface. Lucene
///    draws exactly this line: whether a field gets a term dictionary is read
///    off the field's own `IndexableFieldType` (`IndexingChain.invertAndStore`,
///    `lucene/core/src/java/org/apache/lucene/index/IndexingChain.java:1416`),
///    never off its name, and one name meaning two different things is an
///    outright error rather than a silent downgrade
///    (`IndexingChain.FieldSchema.setIndexOptions` → `assertSame` →
///    `raiseNotSame`, `IndexingChain.java:2213`, `:2187` and `:2193`);
///  * absent → the name rule applies, and this is the companion's own case.
///
/// THAT SECOND BULLET IS ONLY SAFE BECAUSE THE COMPANION IS KEPT UNDECLARED.
/// `<field>_chunks` is user-supplied `_source` (an array of float arrays), so
/// dynamic mapping used to register it like any other unmapped key —
/// `infer_field_type` walks to the first scalar and lands on
/// `FieldType::Double`, a `FieldConfig` byte-identical to a user's own
/// `"type": "double"`. That is what forced an earlier revision to carve
/// `double`/`long` out of the yield set above, and with them a real user field
/// under that name. `index::is_undeclared_multi_vector_companion` removes the
/// ambiguity at its source instead: dynamic mapping refuses to invent the
/// config when the value [`looks_like_multi_vector`], so the only `double` or
/// `long` under this name is one somebody wrote.
///
/// BOTH HALVES ARE LOAD-BEARING, measured by disabling each on its own against
/// the committed 300-doc × 128-dim fixture and the 50-doc collision fixture:
///
/// | | `.emb_chunks.fst` (300-doc) | durable index | declared `long` sibling |
/// |---|---|---|---|
/// | here | **0 B** | 857,265–857,769 B | 44 B `.fst`, answers 25 |
/// | yield-to-numeric restored | 0 B | 857,445 B | **0 B `.fst`, answers 0** |
/// | refusal disabled | **1,592,118 B** | **2,451,336 B** | 44 B, answers 25 |
///
/// The middle row is the residual this replaces; the bottom row is the 67% of
/// the saving an unconditional "any declaration wins" gives up. Neither is
/// paid for here.
///
/// This is deliberately NOT the contract `semantic_derived_vector_fields`
/// carries: that one synthesises `<target>_chunks` unconditionally, and a
/// declared field of that name has always lost its postings to it. That
/// behaviour is untouched here — it is `main`'s, on a mapping shape this PR
/// does not introduce — but it is not a precedent this rule reuses, because it
/// fires only when an embedding config designates the target, whereas this rule
/// fires for every user-mapped `dense_vector`. Nor is
/// `observe_passage_chunks_in_source` a precedent: it reserves `<field>_chunks`
/// only when `_source` holds a NON-EMPTY ARRAY there
/// (`index.rs`'s `Some(Value::Array(chunks)) if !chunks.is_empty()`), so a
/// string or keyword value was never captured by it. Measured on
/// `{text body, keyword emb_chunks, dense_vector emb}`, 50 docs × 16 dim, half
/// `tenant-a` — three source states, all three run in one worktree: the merge
/// base `ca4d75a` (`main` when this branch was cut), the unconditional
/// name-only rule this replaces (`e629a58`), and here.
///
/// | | `main` | name-only | here |
/// |---|---|---|---|
/// | `<seg>.emb_chunks.fst` | 54 B | **0** | 54 B |
/// | `<seg>.emb.fst` | 16,713 B | 0 | 0 |
/// | `term {emb_chunks:"tenant-a"}` | 25 | **0** | 25 |
/// | `terms {emb_chunks:["tenant-a"]}` | 25 | **0** | 25 |
/// | `bool{filter:[term emb_chunks]}` | 25 | **0** | 25 |
/// | `bool{must:[match body],filter:[term …]}` | 25 | **0** | 25 |
/// | `constant_score{filter:term emb_chunks}` | 25 | **0** | 25 |
/// | `bool{must_not:[term emb_chunks]}` | 25 | **50** | 25 |
/// | `bool{must:[match_all],must_not:[term …]}` | 25 | **50** | 25 |
/// | `terms` agg on `emb_chunks` | 25/25 | 25/25 | 25/25 |
///
/// The same schema with `emb_chunks` mapped `text` (values `tenant-a widgets` /
/// `tenant-b gadgets`, so one token is unique to each half):
///
/// | | `main` | name-only | here |
/// |---|---|---|---|
/// | `<seg>.emb_chunks.fst` | 71 B | **0** | 71 B |
/// | `match {emb_chunks:"widgets"}` | 25 | **0** | 25 |
/// | `match_phrase {emb_chunks:"tenant-a widgets"}` | 25 | **0** | 25 |
/// | `multi_match {fields:["emb_chunks"]}` | 25 | **0** | 25 |
/// | `simple_query_string {fields:["emb_chunks"]}` | 25 | **0** | 25 |
/// | `prefix {emb_chunks:"widget"}` | 25 | **0** | 25 |
/// | `bool{must:[match_all],must_not:[match …]}` | 25 | **50** | 25 |
/// | unfielded `query_string {"query":"widgets"}` | 25 | 25 | 25 |
///
/// Every ANSWER row is back to `main`'s value while `emb.fst` stays gone, which
/// is the whole claim: the vector loses its term dictionary, the declared field
/// beside it loses nothing.
///
/// The `must_not` rows are why this is a correctness rule and not a footprint
/// one: under the name-only rule the clause lowered to `match_none`, the Bool
/// fold dropped it as "excludes nothing", and the query RETURNED the 25
/// documents the exclusion was written to remove — with the aggregation proving
/// the data was present and correctly populated the whole time. The unfielded
/// `query_string` row is the self-inconsistency it produced: 25 through the
/// stored-doc scan while every fielded shape said 0.
///
/// One shape is NOT in these tables and deliberately not asserted anywhere:
/// bare `bool{must_not:[match …]}` with no `must`/`filter` answers 0 on
/// `ca4d75a` (and 50 under the name-only rule). That is a PRE-EXISTING
/// XERJ quirk with nothing to do with #328 — a control index with no vector
/// field at all, `{text body, text tags, keyword kw}`, answers
/// `bool{must_not:[match tags:"widgets"]}` = 0 and
/// `bool{must:[match_all],must_not:[match tags:"widgets"]}` = 25 there too.
/// The `match_all`-anchored form is the one the tests pin, because it is the
/// one that is correct on `main` to begin with.
///
/// THE NUMERIC ARM, which an earlier revision of this branch shipped as an
/// accepted residual and which is now closed. On that revision a field the user
/// declared `long` or `double` under one of these two names was still excluded
/// by name and the plan-time lowering still zeroed every lexical clause on it.
/// Measured on `{text body, long emb_chunks, dense_vector emb}`, 50 docs,
/// values 7 and 9 — merge base `ca4d75a` → that revision → here:
///
///   `term {emb_chunks:7}`                      25 → **0** → 25
///   `terms {emb_chunks:[7]}`                   25 → **0** → 25
///   `range {emb_chunks:{gte:8}}`               25 → **0** → 25
///   `bool{filter:[term emb_chunks 7]}`         25 → **0** → 25
///   `bool{must:[match_all],must_not:[term 7]}` 25 → **50** → 25
///   `terms` agg / `stats` on `emb_chunks`      25/25 on all three
///
/// The `double` arm and the DYNAMICALLY mapped numeric case (a scalar `7`/`9`
/// under this name with no mapping at all, which dynamic mapping registers as
/// `long`) behave identically and are pinned alongside it. It was NOT closed by
/// per-field schema provenance and NOT by giving up the by-name rule — the two
/// options an earlier revision of this comment called exhaustive, wrongly. It
/// is closed by not manufacturing the ambiguous declaration in the first place
/// (`index::is_undeclared_multi_vector_companion`), which costs nothing: the
/// 300-doc fixture still reports `emb_chunks.fst=0` and durable 857,265–857,769 B.
///
/// THE LIMIT THAT REMAINS, and it fails safe: the refusal needs the
/// `dense_vector` declared BEFORE the documents arrive. Index first and map
/// afterwards and `emb_chunks` already carries a dynamic `double` config, which
/// this function then yields to — measured `.emb_chunks.fst` = 9,467 B on a
/// 50-doc × 16-dim fixture, with `exists {field: emb_chunks}` still answering
/// 50. That costs bytes, not answers, and is pinned by
/// `a_companion_mapped_before_its_vector_keeps_its_dictionary_and_its_answers`
/// in `tests/integration.rs`. The nested-under-an-object spelling has the same
/// shape of limit and the same direction.
///
/// Pinned by `a_declared_sibling_keeps_its_lexical_surface` here and by
/// `declared_chunks_sibling_of_a_dense_vector_stays_queryable_unless_it_looks_like_one` in
/// `tests/integration.rs`.
///
/// ALLOCATION: this runs once per SEARCH (`index::search_inner` calls it to
/// decide whether the plan-time lowering has anything to do), and the vast
/// majority of indices have no vector field at all. `collect_dense_vector_fields`
/// clones the name of EVERY field it walks past, not just the vector ones, so
/// the no-vector case is short-circuited by the allocation-free predicate
/// below and returns an empty `HashSet`, which does not allocate either. The
/// two must agree by construction, and
/// `the_fast_path_predicate_agrees_with_the_walk` pins that they do.
pub fn lexically_typeless_fields(schema: &Schema) -> std::collections::HashSet<String> {
    let mut excluded = std::collections::HashSet::new();
    if !has_vector_field(&schema.fields) {
        return excluded;
    }
    for field in crate::index::collect_dense_vector_fields(schema) {
        // BY NAME, and only where no declaration contradicts it.
        for synthesised in [
            format!("{field}{MULTI_VECTOR_COMPANION_SUFFIX}"),
            format!("{}{}", xerj_query::executor::PASSAGE_METADATA_PREFIX, field),
        ] {
            if !declares_non_vector_shaped_field(schema, &synthesised) {
                excluded.insert(synthesised);
            }
        }
        // BY DECLARED TYPE: unconditional.
        excluded.insert(field);
    }
    excluded
}

/// Does `schema` declare `path` as a field whose type a MULTI-VECTOR could not
/// have produced — i.e. does the name-based companion rule have to yield to it?
///
/// `path` is dotted exactly as [`crate::index::collect_dense_vector_fields`]
/// emits it, so the walk descends `FieldConfig::fields` segment by segment
/// rather than using `Schema::has_field`, which only sees root names and would
/// answer `false` for every nested companion.
///
/// THE RULE IS ITS COMPLEMENT, NOT A LIST: this yields for EVERY declared type
/// except `dense_vector`. It is stated that way on purpose — an earlier
/// revision enumerated `Text`/`Keyword`/`Date`/`Boolean`/`Ip`/`Chunk`/`Object`
/// and read as exhaustive when it was not (`GeoPoint`, `Binary` and `Nested`
/// also yielded, and `es_type_to_field_type` maps every UNRECOGNISED ES type
/// string to `Object`, so unknown mapping types yielded too). So:
///
///  * `Vector` — already in the set by declared type; `false` is a no-op, and
///    the companion of a companion is not a thing.
///  * anything else — a declaration is a declaration. The user mapped a field
///    under this name, so the name rule yields and the field keeps its term
///    dictionary and its whole lexical surface: `text`, `keyword`, `long`,
///    `double`, `date`, `boolean`, `ip`, `geo_point`, `binary`, `object`,
///    `nested`, `chunk`, and anything `es_type_to_field_type` did not
///    recognise.
///  * absent — nothing to defer to; the name rule applies. This is the
///    companion's own case, and it is the ONLY one, because
///    `index::evolve_schema_from_doc`/`_docs` refuse to invent a `FieldConfig`
///    for an unmapped `<vector>_chunks` key whose value
///    [`looks_like_multi_vector`]. Without that refusal this predicate would be
///    answering about a declaration DYNAMIC MAPPING wrote: `infer_field_type`
///    walks an array of float arrays to its first scalar and lands on
///    `FieldType::Double`, indistinguishable from a user's own `double`. That
///    is why an earlier revision had to carve `Double`/`Long` out of the yield
///    set, and why a user's own numeric field under this name silently lost its
///    lexical surface. The carve-out is gone; the ambiguity is prevented at the
///    only place it was ever manufactured.
///
/// Lucene draws exactly this line: whether a field gets a term dictionary is
/// read off the field's own `IndexableFieldType`
/// (`IndexingChain.invertAndStore`,
/// `lucene/core/src/java/org/apache/lucene/index/IndexingChain.java:1416`),
/// never off its name, and one name meaning two different things is an outright
/// error rather than a silent downgrade (`FieldSchema.setIndexOptions` →
/// `assertSame` → `raiseNotSame`, `IndexingChain.java:2213`, `:2187`, `:2193`).
///
/// BOTH SPELLINGS of a nested name are resolved, because both reach the schema.
/// `put_mapping` stores a dotted path as ONE top-level `FieldConfig` named
/// `"passages.vec_chunks"`, while `es_properties_to_fields` stores it as a
/// `vec_chunks` child under a `passages` object — the same two shapes
/// `nested_dense_vector_is_excluded_from_its_parent_objects_term_dictionary`
/// exercises for the vector itself. The literal lookup runs first so the flat
/// spelling cannot fall through the segmented walk and be treated as undeclared.
fn declares_non_vector_shaped_field(schema: &Schema, path: &str) -> bool {
    let is_lexical =
        |fc: &xerj_common::types::FieldConfig| !matches!(fc.field_type, FieldType::Vector);
    if let Some(field) = schema.fields.iter().find(|fc| fc.name == path) {
        return is_lexical(field);
    }
    let mut fields: &[xerj_common::types::FieldConfig] = &schema.fields;
    let mut segments = path.split('.').peekable();
    while let Some(segment) = segments.next() {
        let Some(field) = fields.iter().find(|fc| fc.name == segment) else {
            return false;
        };
        if segments.peek().is_none() {
            return is_lexical(field);
        }
        fields = &field.fields;
    }
    false
}

/// Does this field list contain a `dense_vector` anywhere? Allocation-free.
///
/// The recursion condition is `collect_dense_vector_fields`'s, deliberately: it
/// stops at a `Vector` rather than descending into its sub-fields, so the two
/// walks visit exactly the same nodes and cannot disagree about whether the set
/// would come back empty.
fn has_vector_field(fields: &[xerj_common::types::FieldConfig]) -> bool {
    fields.iter().any(|fc| {
        if matches!(fc.field_type, FieldType::Vector) {
            true
        } else {
            !fc.fields.is_empty() && has_vector_field(&fc.fields)
        }
    })
}

#[cfg(test)]
mod semantic_derived_vector_exclusion_tests {
    use super::*;
    use serde_json::json;
    use xerj_common::types::{EmbeddingConfig, FieldConfig, FieldType};

    #[test]
    fn derives_default_and_custom_targets_without_suffix_guessing() {
        let mut schema = Schema::empty();
        schema
            .add_field(
                FieldConfig::new("body", FieldType::Text).with_embedding(EmbeddingConfig {
                    endpoint: None,
                    model: None,
                    target_field: None,
                }),
            )
            .unwrap();
        schema
            .add_field(FieldConfig::new("summary", FieldType::Text).with_embedding(
                EmbeddingConfig {
                    endpoint: None,
                    model: None,
                    target_field: Some("summary_model_output".to_string()),
                },
            ))
            .unwrap();
        schema
            .add_field(FieldConfig::new("user_vector", FieldType::Text))
            .unwrap();

        let excluded = semantic_derived_vector_fields(&schema);
        assert_eq!(
            excluded,
            std::collections::HashSet::from([
                "body_vector".to_string(),
                "body_vector_chunks".to_string(),
                "__xerj_passage_meta__body_vector".to_string(),
                "summary_model_output".to_string(),
                "summary_model_output_chunks".to_string(),
                "__xerj_passage_meta__summary_model_output".to_string(),
            ])
        );

        let source = json!({
            "body": "cash and equivalents",
            "page": 7,
            "user_vector": [1.0, 2.0],
            "body_vector_backup": [3.0, 4.0],
            "body_vector": [0.1, 0.2],
            "body_vector_chunks": [[0.1, 0.2], [0.3, 0.4]],
            "__xerj_passage_meta__body_vector": {"field": "body", "chunks": [[0, 20]]},
            "summary_model_output": [0.5, 0.6],
            "summary_model_output_chunks": [[0.5, 0.6], [0.7, 0.8]],
            "__xerj_passage_meta__summary_model_output": {
                "field": "summary",
                "chunks": [[0, 7]]
            }
        });
        let fields = extract_text_fields_from_excluding(&source, &excluded);
        assert_eq!(fields.get("page").map(String::as_str), Some("7"));
        assert_eq!(
            fields.get("user_vector").map(String::as_str),
            Some("1.0 2.0")
        );
        assert_eq!(
            fields.get("body_vector_backup").map(String::as_str),
            Some("3.0 4.0")
        );
        for derived in &excluded {
            assert!(!fields.contains_key(derived));
        }
    }
}

#[cfg(test)]
mod index_false_exclusion_tests {
    //! `"index": false` must remove the field from the full-text index —
    //! the storage half of #204's last accepted-and-ignored instance. Before
    //! the fix `FieldOptions::indexed` was never read here, so a field the
    //! mapping declared non-indexed got a full set of postings anyway.
    use super::*;
    use serde_json::json;
    use xerj_common::types::{FieldConfig, FieldType};

    fn schema_with_a_non_indexed_field() -> Schema {
        let mut schema = Schema::empty();
        let mut note = FieldConfig::new("note", FieldType::Text);
        note.options.indexed = false;
        note.options.doc_values = false;
        schema.add_field(note).unwrap();
        let mut code = FieldConfig::new("code", FieldType::Keyword);
        code.options.indexed = false; // keeps doc values — still queryable
        schema.add_field(code).unwrap();
        schema
            .add_field(FieldConfig::new("body", FieldType::Text))
            .unwrap();
        schema
    }

    #[test]
    fn non_indexed_fields_join_the_fts_exclusion_set() {
        let schema = schema_with_a_non_indexed_field();
        let excluded = fts_excluded_fields(&schema);
        assert!(
            excluded.contains("note"),
            "a non-indexed Text field: the stored-doc scan analyses it the \
             same way the postings did, so dropping them is equivalent: \
             {excluded:?}"
        );
        assert!(
            !excluded.contains("code"),
            "a non-indexed KEYWORD that kept its doc values is still \
             searchable in ES's sense, and the scan fallback is weaker than \
             its whole-value postings (it would split `192.168.0.1` into \
             tokens) — the postings stay: {excluded:?}"
        );
        assert!(!excluded.contains("body"), "{excluded:?}");

        // …but with no doc values either, nothing can answer it, the query is
        // rejected outright, and the postings are pure waste whatever the type.
        let mut schema = Schema::empty();
        let mut opaque = FieldConfig::new("opaque", FieldType::Keyword);
        opaque.options.indexed = false;
        opaque.options.doc_values = false;
        schema.add_field(opaque).unwrap();
        assert!(fts_excluded_fields(&schema).contains("opaque"));
    }

    #[test]
    fn a_non_indexed_schema_text_field_is_never_analysed() {
        // The schema-Text loop in `analyze_doc_excluding` used to ignore the
        // exclusion set entirely (its only members were semantic-derived
        // vector fields, which are never Text). A declared `index: false`
        // Text field is exactly the case that made the omission visible.
        let schema = schema_with_a_non_indexed_field();
        let source = json!({ "note": "zzquagga", "code": "AB-1234", "body": "ordinary text" });
        let analyzer = xerj_fts::analyzer::AnalyzerPipeline::new(
            vec![],
            std::sync::Arc::new(xerj_fts::analyzer::StandardTokenizer),
            vec![std::sync::Arc::new(xerj_fts::analyzer::LowercaseFilter)
                as std::sync::Arc<dyn xerj_fts::analyzer::TokenFilter>],
        );
        let analysed: std::collections::HashMap<String, Vec<Token>> =
            analyze_doc(&source, &schema, &analyzer)
                .into_iter()
                .collect();
        let names: Vec<&str> = analysed.keys().map(String::as_str).collect();
        assert!(
            !analysed.contains_key("note"),
            "a non-indexed Text field must contribute no tokens: {names:?}"
        );
        assert!(
            analysed.contains_key("code"),
            "`code` kept its doc values, so it kept its whole-value postings \
             too — see `fts_excluded_fields`: {names:?}"
        );
        assert!(
            analysed.contains_key("body"),
            "the indexed control field must still be analysed: {names:?}"
        );
    }
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

/// Does `excluded` hold anything strictly BELOW `path`?
///
/// Allocation-free on the hot path: the set is schema-derived and normally
/// holds zero dotted entries, so this is a handful of length compares per
/// object-valued field per document.
pub(crate) fn has_excluded_descendant(
    path: &str,
    excluded: &std::collections::HashSet<String>,
) -> bool {
    excluded.iter().any(|entry| {
        entry.len() > path.len() && entry.as_bytes()[path.len()] == b'.' && entry.starts_with(path)
    })
}

/// [`extract_text_value`], with every descendant named in `excluded` removed
/// from the flattened object first — #328.
///
/// `path` is the dotted path of `val` itself. Only objects can have
/// descendants, so every other shape short-circuits to `extract_text_value`,
/// and an object with no excluded descendant does too — the clone is paid only
/// by the documents that actually carry an excluded sub-field.
pub(crate) fn extract_text_value_excluding(
    val: &Value,
    path: &str,
    excluded: &std::collections::HashSet<String>,
) -> String {
    fn prune(val: &Value, path: &str, excluded: &std::collections::HashSet<String>) -> Value {
        match val {
            Value::Object(map) => {
                let mut kept = serde_json::Map::with_capacity(map.len());
                for (key, child) in map {
                    let child_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    if excluded.contains(&child_path) {
                        continue;
                    }
                    kept.insert(key.clone(), prune(child, &child_path, excluded));
                }
                Value::Object(kept)
            }
            other => other.clone(),
        }
    }
    if !matches!(val, Value::Object(_)) || !has_excluded_descendant(path, excluded) {
        return extract_text_value(val);
    }
    extract_text_value(&prune(val, path, excluded))
}

/// Unit coverage for the three helpers #328 adds — the widest new surface in
/// this file is [`lexically_typeless_fields`], which is `pub`.
#[cfg(test)]
mod lexically_typeless_fields_tests {
    use super::*;
    use serde_json::json;
    use xerj_common::types::{FieldConfig, FieldType, Schema};

    fn vector(name: &str, dims: usize) -> FieldConfig {
        let mut f = FieldConfig::new(name, FieldType::Vector);
        f.options.dimensions = Some(dims);
        f.options.similarity = Some("cosine".to_string());
        f
    }

    /// Every `dense_vector` contributes THREE names, because a vector never
    /// arrives alone: the base field, the `_chunks` multi-vector companion and
    /// the passage-metadata sidecar. Excluding only the base name is what left
    /// `<seg>.emb_chunks.fst` — the bigger of the two artifacts — behind.
    #[test]
    fn every_vector_contributes_its_whole_family() {
        let mut schema = Schema::empty();
        schema
            .add_field(FieldConfig::new("body", FieldType::Text))
            .unwrap();
        schema.add_field(vector("emb", 8)).unwrap();

        let typeless = lexically_typeless_fields(&schema);
        assert_eq!(
            typeless,
            std::collections::HashSet::from([
                "emb".to_string(),
                "emb_chunks".to_string(),
                "__xerj_passage_meta__emb".to_string(),
            ])
        );
        assert!(!typeless.contains("body"), "a text field is not typeless");
    }

    /// The name-based half YIELDS to a declaration. A user who maps a field
    /// called `emb_chunks` beside a `dense_vector emb` gets a real keyword
    /// field, not a silently unqueryable one — the previous revision of this
    /// function excluded the name unconditionally, which took the postings away
    /// from a field the schema declares and (through the plan-time lowering)
    /// made `bool{must_not:[term emb_chunks]}` RETURN the documents it excludes.
    ///
    /// Lucene reads "does this field get a term dictionary" off the field's own
    /// `IndexableFieldType` and never off its name
    /// (`IndexingChain.java:1416`), and treats one name carrying two different
    /// index options as an error rather than a silent downgrade (`:2213`).
    #[test]
    fn a_declared_sibling_keeps_its_lexical_surface() {
        for declared in [
            FieldType::Keyword,
            FieldType::Text,
            FieldType::Date,
            FieldType::Boolean,
            FieldType::Ip,
        ] {
            let mut schema = Schema::empty();
            schema
                .add_field(FieldConfig::new("body", FieldType::Text))
                .unwrap();
            schema
                .add_field(FieldConfig::new("emb_chunks", declared))
                .unwrap();
            schema
                .add_field(FieldConfig::new("__xerj_passage_meta__emb", declared))
                .unwrap();
            schema.add_field(vector("emb", 8)).unwrap();

            let typeless = lexically_typeless_fields(&schema);
            assert_eq!(
                typeless,
                std::collections::HashSet::from(["emb".to_string()]),
                "a declared `{declared:?}` sibling must keep its term dictionary"
            );
            // And the union the write path actually reads agrees.
            let excluded = fts_excluded_fields(&schema);
            assert!(excluded.contains("emb"));
            assert!(
                !excluded.contains("emb_chunks"),
                "declared sibling excluded from the inverted index: {excluded:?}"
            );
        }
    }

    /// THE NUMERIC ARM, which used to be the accepted residual: a `Double` or
    /// `Long` declaration is exactly what dynamic mapping USED to produce from
    /// a real multi-vector, so the name rule could not yield to it without
    /// losing the whole companion saving. It yields now, because
    /// `index::is_undeclared_multi_vector_companion` stops that config from
    /// being invented — so a `long`/`double` under this name is necessarily
    /// somebody's own field. This assertion is the inverse of the one it
    /// replaces; if the carve-out ever comes back it fails here first.
    #[test]
    fn a_numerically_declared_sibling_keeps_its_lexical_surface() {
        for declared in [FieldType::Double, FieldType::Long] {
            let mut schema = Schema::empty();
            schema
                .add_field(FieldConfig::new("emb_chunks", declared))
                .unwrap();
            schema.add_field(vector("emb", 8)).unwrap();
            assert!(
                !lexically_typeless_fields(&schema).contains("emb_chunks"),
                "a `{declared:?}` declaration is somebody's own field and the \
                 name rule must yield to it"
            );
            assert!(declares_non_vector_shaped_field(&schema, "emb_chunks"));
        }
    }

    /// The yield set is the COMPLEMENT of `Vector`, not the enumerated list an
    /// earlier doc comment gave. `GeoPoint`, `Binary` and `Nested` were never
    /// in that list and always yielded, and `es_type_to_field_type` maps every
    /// unrecognised ES type string to `Object`, so an unknown mapping type
    /// yields too. Pinned across every variant so the doc and the code cannot
    /// drift apart again.
    #[test]
    fn every_declared_type_except_dense_vector_keeps_its_lexical_surface() {
        for declared in [
            FieldType::Text,
            FieldType::Keyword,
            FieldType::Long,
            FieldType::Double,
            FieldType::Boolean,
            FieldType::Date,
            FieldType::Ip,
            FieldType::GeoPoint,
            FieldType::Binary,
            FieldType::Object,
            FieldType::Nested,
        ] {
            let mut schema = Schema::empty();
            schema
                .add_field(FieldConfig::new("emb_chunks", declared))
                .unwrap();
            schema.add_field(vector("emb", 8)).unwrap();
            assert!(
                declares_non_vector_shaped_field(&schema, "emb_chunks"),
                "`{declared:?}` is a declaration and must be yielded to"
            );
            let typeless = lexically_typeless_fields(&schema);
            assert!(
                !typeless.contains("emb_chunks"),
                "`{declared:?}` sibling excluded anyway: {typeless:?}"
            );
            // …while the vector itself is excluded on every one of them.
            assert!(typeless.contains("emb"));
        }
    }

    /// The value-shape half of the rule, exhaustively. Only a rectangular,
    /// non-empty array of non-empty all-numeric arrays is a multi-vector;
    /// everything else is somebody's field and keeps its mapping.
    #[test]
    fn looks_like_multi_vector_accepts_only_the_rfc_148_shape() {
        for yes in [
            serde_json::json!([[1.0, 2.0], [3.0, 4.0]]),
            serde_json::json!([[1, 2, 3]]),
            serde_json::json!([[0.5]]),
        ] {
            assert!(looks_like_multi_vector(&yes), "should accept {yes}");
        }
        for no in [
            // scalars and strings — a user's own field
            serde_json::json!(7),
            serde_json::json!(7.5),
            serde_json::json!("tenant-a"),
            serde_json::json!(true),
            serde_json::json!(null),
            serde_json::json!({"a": 1}),
            // a FLAT numeric array is a single vector, not a multi-vector, and
            // is not what this rule is about
            serde_json::json!([1.0, 2.0, 3.0]),
            // empty outer, empty inner
            serde_json::json!([]),
            serde_json::json!([[]]),
            // RAGGED — cannot have come from one embedding model, so it is left
            // to dynamic mapping (the fail-safe direction)
            serde_json::json!([[1.0, 2.0], [3.0]]),
            // non-numeric elements
            serde_json::json!([["a", "b"]]),
            serde_json::json!([[1.0, "b"]]),
            serde_json::json!([[1.0], null]),
            serde_json::json!([[1.0], 2.0]),
            // array of objects
            serde_json::json!([{"a": 1}]),
        ] {
            assert!(!looks_like_multi_vector(&no), "should reject {no}");
        }
    }

    /// A sibling declared as a `dense_vector` is excluded — by the TYPE rule,
    /// which is unconditional — so yielding to the declaration must not
    /// accidentally rescue it.
    #[test]
    fn a_sibling_declared_as_a_vector_is_still_typeless() {
        let mut schema = Schema::empty();
        schema.add_field(vector("emb_chunks", 8)).unwrap();
        schema.add_field(vector("emb", 8)).unwrap();

        let typeless = lexically_typeless_fields(&schema);
        for name in [
            "emb",
            "emb_chunks",
            "emb_chunks_chunks",
            "__xerj_passage_meta__emb",
        ] {
            assert!(typeless.contains(name), "{name} missing from {typeless:?}");
        }
    }

    /// The declaration check walks DOTTED paths, because a nested vector's
    /// companion is nested too. `Schema::has_field` only sees root names and
    /// would answer "not declared" for every one of these.
    #[test]
    fn a_declared_nested_sibling_keeps_its_lexical_surface() {
        let mut schema = Schema::empty();
        let mut parent = FieldConfig::new("passages", FieldType::Object);
        parent.fields.push(vector("vec", 4));
        parent
            .fields
            .push(FieldConfig::new("vec_chunks", FieldType::Keyword));
        schema.add_field(parent).unwrap();

        let typeless = lexically_typeless_fields(&schema);
        assert!(typeless.contains("passages.vec"), "got {typeless:?}");
        assert!(
            !typeless.contains("passages.vec_chunks"),
            "declared nested sibling excluded: {typeless:?}"
        );
        assert!(declares_non_vector_shaped_field(
            &schema,
            "passages.vec_chunks"
        ));
        assert!(
            !declares_non_vector_shaped_field(&schema, "passages.vec"),
            "a declared vector is not a lexical declaration"
        );
        assert!(
            !declares_non_vector_shaped_field(&schema, "passages.absent"),
            "an undeclared name has no declaration to defer to"
        );
        assert!(
            !declares_non_vector_shaped_field(&schema, "passages.vec_chunks.deeper"),
            "walking past a leaf must not resolve"
        );
    }

    /// The FLAT spelling of a nested name — one top-level `FieldConfig` called
    /// `"passages.vec_chunks"`, which is what `put_mapping` produces — must
    /// resolve too. Without the literal lookup the segmented walk looks for a
    /// root `passages` object, does not find one, and treats a declared field
    /// as undeclared.
    #[test]
    fn the_flat_dotted_spelling_of_a_declared_sibling_resolves() {
        let mut schema = Schema::empty();
        schema.add_field(vector("passages.vec", 4)).unwrap();
        schema
            .add_field(FieldConfig::new("passages.vec_chunks", FieldType::Keyword))
            .unwrap();

        assert!(declares_non_vector_shaped_field(
            &schema,
            "passages.vec_chunks"
        ));
        let typeless = lexically_typeless_fields(&schema);
        assert!(typeless.contains("passages.vec"), "got {typeless:?}");
        assert!(
            !typeless.contains("passages.vec_chunks"),
            "the flat spelling was treated as undeclared: {typeless:?}"
        );
    }

    /// The exclusion is by DECLARED type, not by value shape: a numeric array
    /// under a non-vector mapping stays lexical exactly as before.
    #[test]
    fn a_non_vector_field_is_never_typeless() {
        let mut schema = Schema::empty();
        schema
            .add_field(FieldConfig::new("body", FieldType::Text))
            .unwrap();
        schema
            .add_field(FieldConfig::new("cat", FieldType::Keyword))
            .unwrap();
        schema
            .add_field(FieldConfig::new("n", FieldType::Long))
            .unwrap();
        assert!(lexically_typeless_fields(&schema).is_empty());
    }

    /// A vector nested under an object mapping arrives as a DOTTED path — the
    /// first member of this set that is not a root-level name, and the reason
    /// `collect_text_fields` had to start matching the full path.
    #[test]
    fn a_nested_vector_is_collected_by_its_dotted_path() {
        let mut schema = Schema::empty();
        let mut parent = FieldConfig::new("passages", FieldType::Object);
        parent.fields.push(vector("vec", 4));
        schema.add_field(parent).unwrap();

        let typeless = lexically_typeless_fields(&schema);
        assert!(typeless.contains("passages.vec"), "got {typeless:?}");
        assert!(typeless.contains("passages.vec_chunks"));
        assert!(
            !typeless.contains("vec"),
            "the leaf name alone must not match"
        );
    }

    /// `fts_excluded_fields` is the union, so the vector family must appear
    /// there too — that union is what flush, merge and `build_fts_field_configs`
    /// all read.
    #[test]
    fn fts_excluded_fields_unions_the_vector_family_in() {
        let mut schema = Schema::empty();
        schema
            .add_field(FieldConfig::new("body", FieldType::Text))
            .unwrap();
        schema.add_field(vector("emb", 8)).unwrap();

        let excluded = fts_excluded_fields(&schema);
        for name in ["emb", "emb_chunks", "__xerj_passage_meta__emb"] {
            assert!(excluded.contains(name), "{name} missing from {excluded:?}");
        }
        assert!(!excluded.contains("body"));
    }

    /// The allocation-free short-circuit and the allocating walk must agree on
    /// "is this set empty", for every shape either of them can see — otherwise
    /// the fast path would silently disable the whole #328 query half on some
    /// schema.
    #[test]
    fn the_fast_path_predicate_agrees_with_the_walk() {
        let mut nested = FieldConfig::new("passages", FieldType::Object);
        nested.fields.push(vector("vec", 4));
        let mut deep = FieldConfig::new("a", FieldType::Object);
        let mut mid = FieldConfig::new("b", FieldType::Object);
        mid.fields.push(vector("c", 2));
        deep.fields.push(mid);
        let mut object_without_a_vector = FieldConfig::new("meta", FieldType::Object);
        object_without_a_vector
            .fields
            .push(FieldConfig::new("note", FieldType::Text));

        for (label, fields) in [
            ("empty", vec![]),
            ("text only", vec![FieldConfig::new("body", FieldType::Text)]),
            ("object with no vector", vec![object_without_a_vector]),
            ("root vector", vec![vector("emb", 8)]),
            ("nested vector", vec![nested]),
            ("two levels deep", vec![deep]),
            (
                "vector beside text",
                vec![FieldConfig::new("body", FieldType::Text), vector("emb", 8)],
            ),
        ] {
            let mut schema = Schema::empty();
            for f in fields {
                schema.add_field(f).unwrap();
            }
            assert_eq!(
                has_vector_field(&schema.fields),
                !crate::index::collect_dense_vector_fields(&schema).is_empty(),
                "predicate and walk disagree on `{label}`"
            );
            assert_eq!(
                has_vector_field(&schema.fields),
                !lexically_typeless_fields(&schema).is_empty(),
                "short-circuit changed the answer on `{label}`"
            );
        }
    }

    #[test]
    fn has_excluded_descendant_matches_only_strictly_below_the_path() {
        let set = std::collections::HashSet::from(["passages.vec".to_string(), "emb".to_string()]);
        assert!(has_excluded_descendant("passages", &set));
        // The path ITSELF is not a descendant of itself — the caller checks
        // `excluded.contains(&path)` separately, and double-counting it here
        // would make the prune fire for a leaf that was already skipped.
        assert!(!has_excluded_descendant("passages.vec", &set));
        assert!(!has_excluded_descendant("emb", &set));
        // A shared PREFIX is not a path prefix: `passages_extra` must not match
        // `passages.vec`, which is why the check tests for the `.` separator
        // rather than calling `starts_with` alone.
        assert!(!has_excluded_descendant("passages_extra", &set));
        assert!(!has_excluded_descendant("other", &set));
    }

    /// The flattened root-level blob is where a nested vector actually lands,
    /// so the prune has to happen there and not only in the recursion.
    #[test]
    fn extract_text_value_excluding_drops_the_nested_vector_from_the_blob() {
        let set = std::collections::HashSet::from(["passages.vec".to_string()]);
        let val = json!({
            "text": "quarterly liquidity",
            "vec": [0.125, 0.25, 0.5],
            "nested": {"keep": "yes"}
        });
        let pruned = extract_text_value_excluding(&val, "passages", &set);
        assert!(
            pruned.contains("quarterly liquidity"),
            "text kept: {pruned}"
        );
        assert!(
            pruned.contains("yes"),
            "unrelated sub-object kept: {pruned}"
        );
        assert!(
            !pruned.contains("0.125") && !pruned.contains("0.25"),
            "vector components must be gone: {pruned}"
        );
    }

    /// No excluded descendant → identical to `extract_text_value`, and no clone
    /// is paid. This is the path every document without a nested vector takes.
    #[test]
    fn extract_text_value_excluding_is_a_no_op_without_a_descendant() {
        let set = std::collections::HashSet::from(["other.vec".to_string()]);
        let val = json!({"text": "quarterly liquidity", "n": 7});
        assert_eq!(
            extract_text_value_excluding(&val, "passages", &set),
            extract_text_value(&val)
        );
        // A non-object short-circuits regardless of what the set holds.
        let scalar = json!("plain string");
        assert_eq!(
            extract_text_value_excluding(&scalar, "passages", &set),
            extract_text_value(&scalar)
        );
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

#[cfg(test)]
mod filtered_docs_arc_tests {
    use super::*;
    use crate::aggs::doc_matches_filter;
    use serde_json::json;

    /// Build a sharded memtable of `n_docs` docs with scalar fields:
    ///   n (long = i), status (keyword, 5-card = i%5), lat (double).
    fn build_mem(n_docs: usize) -> ShardedFtsMemtable {
        let mem = ShardedFtsMemtable::new();
        let schema = Schema::default();
        for i in 0..n_docs {
            let doc = json!({
                "n": i as i64,
                "status": format!("s{}", i % 5),
                "lat": (i % 1000) as f64 * 0.01,
            });
            mem.insert(format!("d{i}"), &doc, &schema, i as u64);
        }
        mem
    }

    /// Brute reference: every buffered doc whose source matches `filter_json`
    /// under the exact JSON matcher, as a sorted `doc_id` vec.
    fn brute_ids(mem: &ShardedFtsMemtable, filter_json: &Value) -> Vec<String> {
        let mut ids: Vec<String> = mem
            .all_docs_with_sources_arc()
            .into_iter()
            .filter(|(_id, src)| doc_matches_filter(src, filter_json))
            .map(|(id, _)| id)
            .collect();
        ids.sort();
        ids
    }

    fn columnar_ids(mem: &ShardedFtsMemtable, preds: &[MemBoolPred]) -> Vec<String> {
        let mut ids: Vec<String> = mem
            .filtered_docs_arc(preds)
            .expect("columnar path should apply for scalar fields")
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        ids.sort();
        ids
    }

    /// The columnar candidate set must EQUAL the `doc_matches_filter` set —
    /// including on a filter that matches FAR more than the historical
    /// `materialisation_limit` (256) so a bounded/truncated fold would be
    /// caught (the all-matches aggregation contract).
    #[test]
    fn columnar_range_matches_brute_over_256() {
        let mem = build_mem(4000);
        // Range n ∈ [500, 3500) → 3000 matches, an order of magnitude past 256.
        let preds = vec![MemBoolPred::Range {
            field: "n".to_string(),
            gte: Some(500.0),
            gt: None,
            lte: None,
            lt: Some(3500.0),
        }];
        let filter = json!({ "range": { "n": { "gte": 500, "lt": 3500 } } });
        let brute = brute_ids(&mem, &filter);
        let columnar = columnar_ids(&mem, &preds);
        assert_eq!(brute.len(), 3000, "sanity: 3000 docs in [500,3500)");
        assert_eq!(columnar, brute, "columnar range set diverged from brute");
        // Value fold parity: sum over the columnar candidate set (metric-agg
        // fold input) equals the brute sum over the matcher set.
        let sum_of = |ids: &[String]| -> f64 {
            ids.iter()
                .map(|id| id.trim_start_matches('d').parse::<f64>().unwrap())
                .sum()
        };
        assert_eq!(sum_of(&columnar), sum_of(&brute));
    }

    /// Term + Range conjunction (a `bool{filter}`), also > 256 matches.
    #[test]
    fn columnar_bool_conjunction_matches_brute() {
        let mem = build_mem(4000);
        // status == "s2" (800 docs) AND n ∈ [0,4000) → the 800 status-s2 docs.
        let preds = vec![
            MemBoolPred::Term {
                field: "status".to_string(),
                value: "s2".to_string(),
            },
            MemBoolPred::Range {
                field: "n".to_string(),
                gte: Some(0.0),
                gt: None,
                lte: None,
                lt: Some(4000.0),
            },
        ];
        let filter = json!({
            "bool": { "filter": [
                { "term": { "status": "s2" } },
                { "range": { "n": { "gte": 0, "lt": 4000 } } },
            ]}
        });
        let brute = brute_ids(&mem, &filter);
        assert_eq!(brute.len(), 800);
        assert_eq!(columnar_ids(&mem, &preds), brute);
    }

    /// Bare keyword term.
    #[test]
    fn columnar_term_matches_brute() {
        let mem = build_mem(2000);
        let preds = vec![MemBoolPred::Term {
            field: "status".to_string(),
            value: "s3".to_string(),
        }];
        let filter = json!({ "term": { "status": "s3" } });
        let brute = brute_ids(&mem, &filter);
        assert_eq!(brute.len(), 400);
        assert_eq!(columnar_ids(&mem, &preds), brute);
    }

    /// A predicate field that ever carried an ARRAY value makes the column
    /// lossy (first element only), so the columnar path MUST bail (`None`) so
    /// the caller keeps the exact full-corpus walk — otherwise a later matching
    /// array element would be a silent false negative.
    #[test]
    fn columnar_bails_on_array_predicate_field() {
        let mem = ShardedFtsMemtable::new();
        let schema = Schema::default();
        for i in 0..2000usize {
            // Every 500th doc stores `n` as an array — poisons the shard's
            // `array_fields` for whichever shard owns it.
            let n_val = if i % 500 == 0 {
                json!([i as i64, (i as i64) + 1])
            } else {
                json!(i as i64)
            };
            mem.insert(format!("d{i}"), &json!({ "n": n_val }), &schema, i as u64);
        }
        let preds = vec![MemBoolPred::Range {
            field: "n".to_string(),
            gte: Some(0.0),
            gt: None,
            lte: None,
            lt: Some(2000.0),
        }];
        assert!(
            mem.filtered_docs_arc(&preds).is_none(),
            "array-valued predicate field must force the full-corpus fallback"
        );
    }
}

#[cfg(test)]
mod external_scalar_n_ghost_tests {
    use super::*;
    use serde_json::json;

    /// #193 item 2 — the external-mode (`Some(cs)`) scalar `N` must be
    /// ghost-inclusive, exactly like the historical `None` branch.
    ///
    /// The scalar only engages as the per-field fallback: a scored field the
    /// supplied union does not carry.  This test pins the seam directly with
    /// ONE shard, so the local per-shard fallbacks for `df` and `avgdl`
    /// coincide with the `None` branch's cross-shard aggregation by
    /// construction, and the ghost carries NO occurrence of the queried
    /// field — leaving the scalar `N` as the ONLY quantity the two modes
    /// could disagree on.  Before the fix the external branch used the
    /// live-only `doc_count()` (2) where the `None` branch used
    /// live + ghosts (3), so the same query on the same memtable scored
    /// differently depending on which mode the engine happened to take.
    #[test]
    fn external_mode_scalar_n_matches_none_mode_under_ghosts() {
        let registry = Arc::new(AnalyzerRegistry::default());
        let mem = ShardedFtsMemtable::with_registry_and_shards(registry, 1);
        let schema = Schema::empty();

        // Two live docs carrying the queried term in `body`, one doomed doc
        // that never mentions `body` at all.
        mem.insert("a".into(), &json!({"body": "quicklist alpha"}), &schema, 1);
        mem.insert(
            "b".into(),
            &json!({"body": "quicklist beta gamma delta"}),
            &schema,
            2,
        );
        mem.insert(
            "ghost".into(),
            &json!({"other": "unrelated text"}),
            &schema,
            3,
        );
        mem.remove("ghost");

        // Union that does NOT carry `body` — the scored field falls back to
        // the scalar N (plus local df/avgdl, identical across modes here).
        let mut cs = xerj_fts::CollectionStats::new();
        cs.add_field(
            "other",
            &xerj_fts::FieldStats {
                total_docs: 1,
                total_field_length: 2,
            },
        );

        let boosts = std::collections::HashMap::new();
        let (none_hits, none_total) =
            mem.search_text_boosted_with_total("quicklist", &["body"], 10, &boosts);
        let (ext_hits, ext_total) = mem.search_text_boosted_with_total_using(
            "quicklist",
            &["body"],
            10,
            &boosts,
            Some(&cs),
        );

        assert_eq!(none_total, 2, "sanity: two live matches");
        assert_eq!(ext_total, none_total, "totals must agree between modes");
        assert_eq!(none_hits.len(), 2);
        assert_eq!(ext_hits.len(), 2);
        for (n, e) in none_hits.iter().zip(ext_hits.iter()) {
            assert_eq!(n.doc_id, e.doc_id, "hit order diverged between modes");
            assert_eq!(
                n.score.to_bits(),
                e.score.to_bits(),
                "{}: external-mode fallback scored {} vs None-mode {} — the \
                 scalar N dropped the ghost from IDF",
                n.doc_id,
                e.score,
                n.score
            );
        }
    }
}
