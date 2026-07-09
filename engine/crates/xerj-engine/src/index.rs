//! Per-index coordinator: ties together storage, FTS memtable, and search.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{RwLock, Semaphore};
use tracing::{debug, error, info, warn};
use uuid::Uuid;
use xerj_common::config::Config;
use xerj_common::schema::ManagedSchema;
use xerj_common::types::{FieldConfig, FieldType, IndexName, Schema};
use xerj_fts::analyzer::AnalyzerRegistry;
use xerj_fts::index::FtsIndexReader;
use xerj_fts::search::{
    BoolQuery as FtsBool, DisMaxQuery as FtsDisMax, FtsSearcher, Query as FtsQuery,
    TermQuery as FtsTerm,
};
use xerj_query::ast::{
    BoostMode, FieldValueFactor, Fuzziness, HighlightRequest, MinShouldMatch, Modifier, QueryNode,
    RandomScore, RescoreQuery, ScoreFunction, ScoreMode, SearchRequest, SourceFilter,
    TrackTotalHits,
};
use xerj_query::executor::{Hit, SearchResult, TotalHits, TotalHitsRelation};
use xerj_storage::index_store::{IndexStore, IndexStoreConfig};
use xerj_storage::segment::SectionType;
use xerj_storage::wal::{SyncMode, WalEntry};
use xerj_vector::distance::DistanceMetric;
use xerj_vector::hnsw::{HnswIndex, HnswParams};
use xerj_vector::Sq8Params;

use crate::aggs::run_aggs_with_all;

/// Clears an index's `merge_in_progress` flag on every exit path of the
/// merge holder (background pass or forcemerge), including panics.
struct MergeFlagClear<'a>(&'a std::sync::atomic::AtomicBool);
impl<'a> Drop for MergeFlagClear<'a> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

// Doc-values (columnar) fast path for size:0 + match_all + aggs — child
// module so it can reach Index's private fields/methods via `super::`.
#[path = "fast_aggs.rs"]
mod fast_aggs;

/// Reconstruct the ES-JSON form of a query filter that the columnar fast-agg
/// path (`fast_aggs::compile_top_pred` + `aggs::doc_matches_filter`) can
/// evaluate over doc-values WITHOUT hydrating `_source`.  Returns `None` for
/// any query that isn't a plain conjunctive filter over exact/numeric fields
/// (FTS, wild/prefix, must_not/should bool, date-string ranges, numeric term
/// keys, …) — the caller then keeps the exact brute path.
///
/// Only the leaf/bool shapes that `doc_matches_filter` and `compile_pred`
/// jointly support are emitted, so the memtable matcher and the columnar
/// predicate stay byte-identical.
fn query_node_to_agg_filter(node: &QueryNode) -> Option<Value> {
    match node {
        QueryNode::MatchAll => Some(serde_json::json!({ "match_all": {} })),
        // Exact term — keyword (string) values only.  Numeric/bool term keys
        // have typed-coercion subtleties, so bail (brute path handles them).
        QueryNode::Term {
            field,
            value: Value::String(s),
            ..
        } => Some(serde_json::json!({ "term": { field: s } })),
        QueryNode::Terms { field, values, .. } => {
            let mut strs: Vec<Value> = Vec::with_capacity(values.len());
            for v in values {
                match v {
                    Value::String(s) => strs.push(Value::String(s.clone())),
                    _ => return None,
                }
            }
            Some(serde_json::json!({ "terms": { field: Value::Array(strs) } }))
        }
        // Numeric range only — every present bound must be a JSON number
        // (date-string bounds compile differently and are left to the brute
        // path).
        QueryNode::Range {
            field,
            gte,
            gt,
            lte,
            lt,
            ..
        } => {
            let mut bounds = serde_json::Map::new();
            for (k, opt) in [("gte", gte), ("gt", gt), ("lte", lte), ("lt", lt)] {
                if let Some(v) = opt {
                    match v {
                        Value::Number(_) => {
                            bounds.insert(k.to_string(), v.clone());
                        }
                        _ => return None,
                    }
                }
            }
            if bounds.is_empty() {
                return None;
            }
            Some(serde_json::json!({ "range": { field: Value::Object(bounds) } }))
        }
        // Pure conjunction: `must` + `filter` only.  `should`, `must_not` and
        // `minimum_should_match` change matching semantics that the columnar
        // AND / `doc_matches_filter` conjunction can't reproduce here → bail.
        QueryNode::Bool {
            must,
            should,
            must_not,
            filter,
            minimum_should_match,
        } => {
            if !should.is_empty() || !must_not.is_empty() || minimum_should_match.is_some() {
                return None;
            }
            let mut clauses: Vec<Value> = Vec::with_capacity(must.len() + filter.len());
            for c in must.iter().chain(filter.iter()) {
                clauses.push(query_node_to_agg_filter(c)?);
            }
            if clauses.is_empty() {
                // bool with no positive clauses == match_all
                return Some(serde_json::json!({ "match_all": {} }));
            }
            Some(serde_json::json!({ "bool": { "filter": Value::Array(clauses) } }))
        }
        _ => None,
    }
}
use crate::{EngineError, Result};

// ── Response types ────────────────────────────────────────────────────────────

/// Response returned after indexing a document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexResponse {
    pub id: String,
    pub seq_no: u64,
    pub version: u64,
    pub result: String,
}

/// Per-field encoding statistics, derived from `FieldAnalyzer`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldEncodingInfo {
    /// Field name.
    pub field: String,
    /// Encoding type string, e.g. `"bitset_enum"`, `"delta_timestamp"`.
    pub encoding: String,
    /// Estimated average bytes stored per value in this encoding.
    pub bytes_per_value: f64,
    /// Estimated average bytes if stored as raw UTF-8 strings.
    pub raw_bytes_per_value: f64,
    /// Compression ratio vs raw strings (higher = better).
    pub compression_ratio: f64,
    /// Number of distinct values observed in the sample.
    pub distinct_values: usize,
}

/// Statistics about an index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStats {
    pub name: String,
    pub doc_count: u64,
    pub segment_count: usize,
    pub memtable_doc_count: usize,
    pub memtable_size_bytes: usize,
    pub schema_version: u64,
    pub field_count: usize,
    /// Total number of search queries executed against this index.
    pub query_count: u64,
    /// Total search latency accumulated (milliseconds).
    pub query_total_ms: u64,
    /// Total number of documents indexed.
    pub index_count: u64,
    /// Total indexing latency accumulated (milliseconds).
    pub index_total_ms: u64,
    /// Per-field smart encoding analysis results (populated after 1000+ samples).
    pub field_encodings: Vec<FieldEncodingInfo>,
}

/// Auto-detected log format based on field names in ingested documents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogFormat {
    /// Apache/Nginx access log: `status`, `method`, `path`, `client_ip`, `bytes`, `response_time`.
    ApacheAccess,
    /// Nginx access log (same field set, different naming conventions).
    NginxAccess,
    /// Application / structured log: `level`, `message`, `service`, `timestamp`.
    AppLog,
    /// Syslog (RFC 3164 / RFC 5424): `priority`, `facility`, `hostname`, `message`.
    Syslog,
    /// Unknown / mixed format.
    Generic,
}

/// Detect the log format of a document by examining its field names.
///
/// Returns `None` when not enough evidence is present to make a reliable
/// determination, or `Some(LogFormat)` otherwise.
pub fn detect_log_format(source: &Value) -> Option<LogFormat> {
    let obj = source.as_object()?;
    let keys: std::collections::HashSet<&str> = obj.keys().map(|k| k.as_str()).collect();

    // Apache / Nginx access log: must have at least status + method + path.
    let apache_fields = ["status", "method", "path"];
    if apache_fields.iter().all(|f| keys.contains(f)) {
        // Differentiate Nginx by body_bytes_sent or upstream_response_time.
        if keys.contains("body_bytes_sent") || keys.contains("upstream_response_time") {
            return Some(LogFormat::NginxAccess);
        }
        return Some(LogFormat::ApacheAccess);
    }

    // App log: level + message is the minimum signal; service strengthens it.
    if keys.contains("level") && keys.contains("message") {
        return Some(LogFormat::AppLog);
    }

    // Syslog: priority or facility + hostname + message.
    if (keys.contains("priority") || keys.contains("facility"))
        && keys.contains("hostname")
        && keys.contains("message")
    {
        return Some(LogFormat::Syslog);
    }

    Some(LogFormat::Generic)
}

// ── Enrich types ─────────────────────────────────────────────────────────────

/// An enrichment lookup table stored in an [`Index`].
///
/// When a document contains the `match_field` key equal to an entry in
/// `lookup`, the corresponding `Value` (must be a JSON object) is merged into
/// the document before it is indexed.
///
/// # Example
///
/// ```json
/// {
///   "name": "product_category",
///   "match_field": "sku",
///   "lookup": {
///     "SKU-001": { "category": "electronics", "taxable": true },
///     "SKU-002": { "category": "clothing",    "taxable": false }
///   }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichTable {
    /// Human-readable name for this enrich table (matches the URL parameter).
    pub name: String,
    /// The document field whose value is used as the lookup key.
    pub match_field: String,
    /// key → JSON object of enrichment fields to merge.
    pub lookup: HashMap<String, Value>,
}

// ── Request-cache hit-tracking set ───────────────────────────────────────────

/// Bounded FIFO set of recently-seen query body hashes.
///
/// The previous implementation used an unbounded `HashSet<u64>`. On a
/// long-lived server that sees N million distinct queries, that's
/// `~24 * N MiB` of HashMap bookkeeping plus the u64 entries. Trivial
/// to OOM in production; trivial to fix.
///
/// Behaviour:
///   * `record(hash)` returns `true` on first sight (miss), `false` on
///     repeat (hit).
///   * Capacity is fixed at construction. Inserting at capacity evicts
///     the oldest entry (true FIFO, not LRU — LRU would need a per-hit
///     reorder which is overkill for a hit-count metric).
///   * `O(1)` for both insert and membership; bounded memory at
///     `cap * (size_of::<u64>() + small overhead)`.
///
/// 65 536 entries (the default) is ~1.5 MiB; comfortable headroom for
/// the working set of a typical query pattern, small enough to never
/// matter even on tiny VMs.
pub(crate) struct RequestCacheSeen {
    set: std::collections::HashSet<u64>,
    order: std::collections::VecDeque<u64>,
    cap: usize,
}

impl RequestCacheSeen {
    pub fn with_capacity(cap: usize) -> Self {
        let cap = cap.max(1);
        Self {
            set: std::collections::HashSet::with_capacity(cap),
            order: std::collections::VecDeque::with_capacity(cap),
            cap,
        }
    }
    /// Returns `true` if `hash` is new (insert; was-miss), `false` if
    /// already present (was-hit).
    pub fn record(&mut self, hash: u64) -> bool {
        if !self.set.insert(hash) {
            return false; // already seen → hit
        }
        // New entry; evict oldest if we just blew the cap.
        if self.order.len() >= self.cap {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        self.order.push_back(hash);
        true
    }
}

// ── SQ8 serving-path code store ───────────────────────────────────────────────

/// Per-field in-memory SQ8 (scalar8) code store used by the kNN serving path.
///
/// Holds one shared [`Sq8Params`] (fitted from the first ~1000 ingested
/// vectors for the field) plus a `doc_id -> Vec<u8>` code map. Each code vector
/// is 1 byte per dimension — a quarter of the 4 bytes/dim an f32 vector costs —
/// so this is the concrete artifact behind the ~4× memory claim. The store is
/// consulted by [`Index::run_knn_brute_force`] instead of reading the f32
/// vector from `_source` for scoring.
struct Sq8FieldStore {
    /// Shared per-dimension min/scale codec for this field.
    params: Sq8Params,
    /// Vector dimensionality the params were fitted for.
    dim: usize,
    /// Whether vectors were L2-normalised before quantising (cosine fields).
    normalize: bool,
    /// doc_id → SQ8 codes (1 byte/dim).
    codes: HashMap<String, Vec<u8>>,
}

impl Sq8FieldStore {
    /// Total bytes held by the u8 code map — the working set we shrank 4×.
    fn code_bytes(&self) -> usize {
        self.codes.values().map(|c| c.len()).sum()
    }
}

/// L2-normalise a vector in place (no-op for a zero vector). Used for cosine
/// fields so SQ8 fits over bounded per-dimension ranges (better recall).
fn l2_normalize_vec(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

// ── Index ────────────────────────────────────────────────────────────────────

/// Per-index coordinator.
///
/// Owns the storage layer (`IndexStore`), the FTS memtable, and the schema.
/// All public methods are `async` so they can be called from Axum handlers
/// without blocking the async runtime.
pub struct Index {
    name: IndexName,
    schema: Arc<RwLock<ManagedSchema>>,
    store: Arc<IndexStore>,
    /// 16-shard FTS memtable.  Replaces the pre-v16 single
    /// `Arc<RwLock<FtsMemtable>>` that serialized all concurrent bulk
    /// clients on one write lock.  Bulk batches pick one shard by
    /// `xxh3_64(first_doc_id) & 15` and hold only that shard's mutex
    /// for the WAL append + engine memtable push, so N concurrent
    /// clients on N different shards run truly in parallel on the
    /// write side.  Query paths iterate all shards.
    memtable: Arc<crate::memtable::ShardedFtsMemtable>,
    doc_count: Arc<AtomicU64>,
    /// Counter for `update` operations that detected no change to the
    /// existing source — surfaced via `indices.stats` as
    /// `indexing.noop_update_total`.
    noop_update_count: Arc<AtomicU64>,
    /// Request cache tracking for `request_cache=true` searches.
    /// `seen` stores query-body hashes that have been served before
    /// (bounded FIFO; see `RequestCacheSeen` for the eviction policy).
    /// miss_count increments on first sight, hit_count on repeat.
    request_cache_seen: Arc<RwLock<RequestCacheSeen>>,
    request_cache_hits: Arc<AtomicU64>,
    request_cache_misses: Arc<AtomicU64>,
    registry: Arc<AnalyzerRegistry>,
    data_dir: PathBuf,
    /// Doc count threshold for auto-flush (default: 10,000).
    flush_doc_threshold: usize,
    /// Byte threshold for auto-flush (from config flush_size_mb).
    flush_byte_threshold: usize,
    /// Index-level settings (e.g. number_of_shards, number_of_replicas).
    settings: Arc<RwLock<Value>>,
    /// Cached schema hash for skipping schema evolution on unchanged docs.
    /// Updated every 100 docs; avoids hashing the schema on every ingest.
    schema_hash_cache: Arc<std::sync::atomic::AtomicU64>,
    /// Doc count at which the cached schema hash was last computed.
    schema_hash_epoch: Arc<AtomicU64>,
    /// Optional HNSW index for dense-vector KNN search.
    /// Lazily created on first vector document insertion.
    pub hnsw: Arc<RwLock<Option<HnswIndex>>>,
    /// Maps string doc-id → u64 node-id used by HNSW.
    hnsw_id_map: Arc<RwLock<HashMap<String, u64>>>,
    /// Maps u64 node-id → string doc-id (reverse of hnsw_id_map).
    hnsw_id_rev: Arc<RwLock<HashMap<u64, String>>>,
    /// Monotonic counter for HNSW node IDs.
    hnsw_next_id: Arc<AtomicU64>,
    /// Per-field SQ8 (scalar8) code stores for the kNN serving path.
    ///
    /// Keyed by dense_vector field name. Populated only for fields whose
    /// mapping opts into `scalar8` quantization (`index_options.type:
    /// int8_hnsw`). Each store holds one shared [`Sq8Params`] plus a
    /// `doc_id -> Vec<u8>` code map (1 byte/dim, NOT 4), giving a real ~4×
    /// reduction on that field's vector working set. Built lazily on the first
    /// kNN over the field from the same (doc_id, source) candidates the
    /// brute-force scan already gathers, then refreshed incrementally as new
    /// docs appear. Default (none) fields never touch this map, so their exact
    /// f32 brute-force path is byte-identical to before.
    sq8_stores: Arc<RwLock<HashMap<String, Sq8FieldStore>>>,
    // ── Per-index concurrency control ─────────────────────────────────────────
    /// Semaphore that limits the number of queries executing concurrently
    /// against this index.  The default is 64 permits, matching the global
    /// `max_concurrent_searches` default.  This prevents a single hot index
    /// from starving other indices in a multi-tenant deployment — a noisy
    /// neighbour cannot exhaust all available query slots.
    max_concurrent_queries: Arc<Semaphore>,
    // ── Per-index enrich lookup tables ────────────────────────────────────────
    /// Named enrich tables: each table maps a key to a JSON object of extra
    /// fields to merge into matching documents at ingest time.
    ///
    /// Added via `POST /v1/indices/{name}/enrich`, keyed by table name.
    /// The lookup is keyed by the value of a nominated field; when a document
    /// has that field set and the value appears in the table, the enrichment
    /// fields are merged into the document before indexing.
    pub enrichments: Arc<RwLock<HashMap<String, EnrichTable>>>,

    /// Doc count at which the last automatic flush was triggered.
    last_flush_doc_count: Arc<AtomicU64>,

    /// V4 M4.5 — concurrent flush semaphore.  Replaces the old
    /// `flush_lock` + `flush_in_progress` pair that capped in-flight
    /// flushes at one and became the ingest ceiling on log-bursty
    /// workloads (60 k docs/s ingest, one flush takes ~5 s for 100 k
    /// docs).  Allowing up to `MAX_CONCURRENT_FLUSHES` writers to build
    /// segments in parallel lets ingest use all CPU cores instead of
    /// just the handful rayon uses inside a single flush.
    flush_sema: Arc<tokio::sync::Semaphore>,

    /// Set to `true` while a segment merge is in progress.  Prevents the
    /// background merge thread from queueing overlapping merges.
    merge_in_progress: Arc<std::sync::atomic::AtomicBool>,

    /// Snapshot of the operator-tunable merge policy parameters
    /// (tier_floor, min/max merge count, max segment size). Cloned from
    /// `Config.merge` at index construction; reads are cheap and merge
    /// runs hold the snapshot for the duration of one batch.
    merge_config: xerj_common::config::MergeConfig,

    /// Optional embedding proxy used by the `semantic` query type
    /// (v0.7-P2). None when `Config.embedding.default_endpoint` is
    /// empty — semantic queries then return a clear 400 telling the
    /// operator to configure an endpoint. Reused across queries
    /// (semaphore + retry budget are per-proxy).
    embedding_proxy: Arc<Option<xerj_ai::embed::EmbeddingProxy>>,

    // ── Per-index metrics ─────────────────────────────────────────────────────
    /// Total search queries executed.
    metric_query_count: Arc<AtomicU64>,
    /// Total search latency accumulated (milliseconds).
    metric_query_total_ms: Arc<AtomicU64>,
    /// Total documents indexed.
    metric_index_count: Arc<AtomicU64>,
    /// Total indexing latency accumulated (milliseconds).
    metric_index_total_ms: Arc<AtomicU64>,
    /// Total `_doc/{id}` GET operations — includes both the HTTP
    /// `GET /{index}/_doc/{id}` route and engine-internal lookups
    /// (e.g. terms-lookup in queries). Exposed via indices.stats.
    pub metric_get_count: Arc<AtomicU64>,
    pub metric_get_total_ms: Arc<AtomicU64>,
    pub metric_get_exists_count: Arc<AtomicU64>,
    pub metric_get_missing_count: Arc<AtomicU64>,

    /// M2 G2 — cache of decoded doc-values columns keyed by segment id.
    /// Segments are immutable once flushed, so the decoded columns stay
    /// valid for the lifetime of the segment.  First agg against a segment
    /// decodes the `.dv` sidecar; subsequent queries hit the cache and
    /// skip I/O + LZ4 decompress + per-column decode entirely.
    dv_cache: Arc<
        dashmap::DashMap<
            String,
            Arc<std::collections::BTreeMap<String, xerj_storage::doc_values::Column>>,
        >,
    >,

    /// Per-(segment, field) sorted sort-key shadow: `(f64-bits, doc_pos)`
    /// ascending by value, used by the field-sort candidate prefilter.
    /// For Numeric dv columns this is a cached copy of `NumericColumn::
    /// sorted`; for Keyword dv columns whose every term is a date-shaped
    /// string it holds the epoch keys produced by the SAME normalisation
    /// `compute_sort_values` applies per hit (`sort_date_normalize`), so
    /// candidate order == heap order.  `None` is cached for ineligible
    /// columns (nulls, non-date terms, count mismatch) to avoid re-probing
    /// per query.  Keyed by `"{segment_id}\u{1}{field}"`; same
    /// immutable-segment lifecycle as `dv_cache` (never invalidated,
    /// retired segment ids are simply never queried again).
    sort_shadow_cache: Arc<dashmap::DashMap<String, Option<Arc<Vec<(i64, u32)>>>>>,
    /// Fields that have EVER been requested through `sorted_shadow_for`
    /// on this index (bounded at 16) — the publish-time warm pre-builds
    /// these fields' shadows for every new segment so the first sorted
    /// query after a flush/merge doesn't pay the O(n log n) build inside
    /// its own latency.
    sort_shadow_fields: Arc<dashmap::DashMap<String, ()>>,
    /// Per-segment single-flight guard for the `stored_slices_for` miss
    /// arm: without it, every in-flight query racing the same cold
    /// segment (up to the 64-permit cap) ran its own full stored-section
    /// decompress — one 30 ms decode became a ~1 s, 64-query stall
    /// episode at every flush/merge publish the warm hadn't reached yet.
    stored_slices_build_locks: Arc<dashmap::DashMap<String, Arc<std::sync::Mutex<()>>>>,
    /// Per-(segment, field, bounds) Range pre-filter position sets.
    /// `build_range_prefilter_cached` was cached in name only — it re-ran
    /// the O(matches) `range_doc_ids` walk + HashSet build per query per
    /// segment (~30-80 ms against a freshly-merged multi-M-doc segment,
    /// multiplied by every query racing the same post-publish miss — the
    /// residual range p99 wobble).  Segments are immutable; bounded by a
    /// wholesale clear at 32 entries (worst-case tens of MB per merged-
    /// segment entry — a roaring bitmap representation is the durable
    /// follow-up) and evicted by segment id at merge retire.
    range_prefilter_cache: Arc<dashmap::DashMap<String, Arc<HashSet<u32>>>>,

    /// Per-segment `_id → stored-position` index, built lazily on the first
    /// `ids` query that touches a segment and reused thereafter.  An `ids`
    /// query used to full-scan every stored doc (O(N) brace-walk + parse)
    /// just to find a handful of primary keys — 128 ms to return 3 docs at
    /// 100k, scaling linearly with the corpus.  With this map an `ids` query
    /// resolves each requested id to its stored position (like GET
    /// `_doc/{id}` / `_mget`) and hydrates only those positions via the
    /// `StoredSlices` offset index, so it is O(#ids) and FLAT vs corpus
    /// size.  Segments are immutable → the map never goes stale; evicted by
    /// segment id at the merge-completion site alongside the other caches.
    id_pos_cache: Arc<dashmap::DashMap<String, Arc<std::collections::HashMap<String, u32>>>>,

    /// Per-segment cache of decoded `Vec<Value>` from the stored section.
    /// KNN search and segment-scan get-document paths used to call
    /// `decode_stored` + `simd_json::serde::from_slice` *every* time —
    /// for a 100-segment index with 100 MB stored per segment, every
    /// vector query re-paid ~10 GB of decompress + parse work. Segments
    /// are immutable once flushed, so this cache is correct without
    /// invalidation. Same `Left unbounded for now` caveat as `dv_cache`.
    stored_value_cache: Arc<dashmap::DashMap<String, Arc<Vec<Value>>>>,

    /// Per-segment cache of the DECOMPRESSED stored section plus per-doc
    /// `(start, end)` byte offsets, used by the sorted-DV candidate path
    /// (field-sorted match_all / search_after).  With it, hydrating the
    /// O(from+size) candidate positions of a segment is two vector
    /// lookups + a per-candidate `simd_json` parse — no per-query
    /// zstd/LZ4 decompress and no O(segment-bytes) brace re-scan.
    ///
    /// Unlike `stored_value_cache` (unbounded parsed `Value`s, ~3-6× raw
    /// bytes) this holds the raw bytes once per segment and is BUDGETED:
    /// `stored_slices_cache_bytes` tracks retained size and inserts stop
    /// at `STORED_SLICES_CACHE_BUDGET` (miss → per-query decompress path,
    /// exactly the pre-cache behaviour).  Segments are immutable, so
    /// entries stay valid until a merge drops the segment id (evicted at
    /// the merge-completion site alongside dv_cache/stored_value_cache).
    stored_slices_cache: Arc<dashmap::DashMap<String, Arc<StoredSlices>>>,
    stored_slices_cache_bytes: Arc<AtomicU64>,
    /// Per-segment cache of the DECOMPRESSED stored section bytes for the
    /// UNSORTED scan path (`scan_stored_section_into`).  The bounded
    /// collector already stops the scan O(from+size) docs in, but every
    /// query still paid the full zstd/LZ4 decompress of the FIRST segment
    /// it touched — for a merged multi-million-doc segment that is
    /// 0.5-1.5 s per query, and under a bulk writer (100 % query-cache
    /// misses) it WAS the match_all/bool/range read-under-write tail.
    /// Budgeted like `stored_slices_cache`; entries evicted by id at the
    /// merge-completion site.  Segments are immutable → no invalidation.
    decoded_stored_cache: Arc<dashmap::DashMap<String, Arc<Vec<u8>>>>,
    decoded_stored_cache_bytes: Arc<AtomicU64>,
    /// Per-(segment, query-shape) match-count cache for the
    /// `try_shortcut_count` Bool intersection arm.  The fused columnar
    /// walk is O(anchor-predicate matches) per segment PER QUERY — for a
    /// dashboard-style fixed bool filter under a bulk writer (query cache
    /// useless: every batch bumps `dataset_version`) that was ~0.6-1 s of
    /// re-counting per request at 1.5 M docs.  Segments are immutable, so
    /// a (segment_id, canonical-query-json) key never goes stale; bounded
    /// by wholesale clear at `SHORTCUT_COUNT_CACHE_MAX` since query
    /// shapes are client-supplied.
    shortcut_count_cache: Arc<dashmap::DashMap<(String, String), u64>>,
    /// Regexp term-dictionary expansion cache — keyed by
    /// `(segment_id, field, pattern)`, holds the exact union doc count
    /// plus the first `REGEXP_EXPANSION_POS_CAP` matching doc positions
    /// for that segment (ES keeps the equivalent in its per-segment
    /// query cache).  Segments are immutable, so entries stay valid for
    /// the segment's lifetime — same no-invalidation contract as
    /// `dv_cache` / `stored_value_cache` above.  Bounded: cleared
    /// wholesale when it exceeds `REGEXP_EXPANSION_CACHE_MAX` entries
    /// because patterns are query-supplied.  This is what makes a
    /// cache-busted (size-varied) regexp count O(1) after the first
    /// touch instead of re-opening FTS side-cars and re-merging postings
    /// on every request.
    regexp_expand_cache: Arc<dashmap::DashMap<(String, String, String), Arc<RegexpExpansion>>>,

    /// Fast-agg path: per-(segment, field) parsed date ordinals
    /// (ord → epoch-ms; i64::MIN = unparseable).  Same immutable-segment
    /// lifecycle as `dv_cache`; evicted alongside it on merge.
    fast_date_cache: Arc<dashmap::DashMap<(String, String), Arc<Vec<i64>>>>,

    /// Fast-agg path: per-(segment, field) sorted parsed dates + prefix doc
    /// counts (`ms[i]` ascending, `prefix[i+1]-prefix[lo]` = docs in a ms
    /// range).  Lets `date_range` answer each range with two binary searches
    /// instead of a full ord walk.  Same lifecycle as `fast_date_cache`.
    fast_date_sorted_cache: Arc<dashmap::DashMap<(String, String), Arc<(Vec<i64>, Vec<u64>)>>>,

    /// M3 framework — response cache for identical queries.  Keyed on
    /// `(query_body_hash, dataset_version)` and holds the pre-computed
    /// `SearchResult` ready to serialize.  Cache hit → ~10 µs total
    /// query latency (vs ~500 µs cache miss) because we skip query
    /// parsing, lock acquisition, segment iteration, and result
    /// construction entirely.  Invalidates on every doc write via
    /// `dataset_version.fetch_add(1)` in `index_doc`.
    pub query_cache: Arc<dashmap::DashMap<(u64, u64), Arc<SearchResult>>>,
    pub dataset_version: Arc<AtomicU64>,

    /// Single-flight coalescing map for identical in-flight reads, keyed by
    /// the SAME `(query_body_hash, dataset_version)` as `query_cache`. Under
    /// the mixed read-under-write workload the open-loop 300/s reader fires
    /// the IDENTICAL body thousands of times; a flush/merge stall makes the
    /// `query_cache` miss (the flush bumped `dataset_version`) and lets a
    /// burst of same-key requests pile up concurrently, each otherwise
    /// recomputing the whole scan/agg and stealing cores from the writer.
    /// The FIRST request of a key ("leader") computes and publishes on this
    /// watch channel; identical concurrent requests ("followers") await and
    /// clone the leader's result instead of recomputing. Because the key is
    /// identical to the `query_cache` key, a follower's served result is
    /// byte-identical to what a `query_cache` hit would have returned — no
    /// new staleness is introduced. A leader that errors / times out
    /// publishes nothing (drops the sender), so followers fall through and
    /// recompute independently — no poisoning.
    query_inflight:
        Arc<dashmap::DashMap<(u64, u64), tokio::sync::watch::Sender<Option<Arc<SearchResult>>>>>,
    /// Count of follower reads served by single-flight coalescing (i.e. the
    /// number of full search recomputes eliminated). Structural evidence for
    /// the read-under-write CPU-contention fix.
    metric_singleflight_coalesced: Arc<AtomicU64>,

    /// Sync-path flush coordinator — notified by `index_batch_sync_raw`
    /// when a shard crosses threshold; drained by a dedicated OS thread
    /// (`xerj-flusher-<idx>`).  Lets rayon scanners stay fully
    /// synchronous instead of crossing into tokio per batch.
    pub(crate) flush_signal: Arc<SyncFlushCoord>,

    /// Caller-supplied `_version` values for docs indexed with
    /// `version_type=external[_gte]`. Used to enforce monotone /
    /// strict CAS on reindex and to echo the exact version back in
    /// the response.
    pub external_versions: Arc<dashmap::DashMap<String, u64>>,

    /// Handle to the per-Index merge background task.  Held so the
    /// shutdown path (`Engine::flush_all_force` → SIGTERM exit) can
    /// abort the task explicitly — without it the tokio runtime stays
    /// alive after axum has stopped accepting connections, because
    /// `tokio::spawn` keeps the runtime non-empty while the task's
    /// 5-s `tokio::time::sleep` is pending.  The bench at 2026-04-25
    /// caught this as a SIGTERM hang regression introduced by B-2b.
    /// `parking_lot::Mutex` (sync, no async, no `.await`) is fine
    /// because we only ever take/replace under it, never hold across
    /// any awaits.
    pub(crate) merge_task: Arc<parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl Index {
    /// Create a new index at `data_dir/<name>`.
    pub fn create(
        name: IndexName,
        schema: Schema,
        config: &Config,
        data_dir: &Path,
    ) -> Result<Arc<Self>> {
        Self::create_with_settings(name, schema, Value::Null, config, data_dir)
    }

    /// Resolve the effective auto-flush thresholds `(docs, bytes)`.
    ///
    /// The byte budget is the PRIMARY driver (the memtable RAM ceiling a
    /// shard fills before draining to a segment); the doc count is a
    /// secondary sanity cap.  Both are runtime-overridable for cadence
    /// tuning / benchmarking, mirroring the existing `XERJ_MERGE_*` knobs:
    ///   `XERJ_FLUSH_SIZE_MB` — global in-memory byte budget (MiB).
    ///   `XERJ_FLUSH_DOCS`    — global doc-count sanity cap.
    /// Both are GLOBAL (across all ingest shards); the per-shard trigger
    /// is `value / shard_count` (see the flush scheduler).
    fn resolve_flush_thresholds(config_flush_size_mb: u64) -> (usize, usize) {
        let size_mb = std::env::var("XERJ_FLUSH_SIZE_MB")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(config_flush_size_mb);
        let byte_threshold = (size_mb as usize).saturating_mul(1024 * 1024);
        let doc_threshold = std::env::var("XERJ_FLUSH_DOCS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(FLUSH_DOC_THRESHOLD_DEFAULT);
        (doc_threshold, byte_threshold)
    }

    /// Create a new index with explicit settings at `data_dir/<name>`.
    pub fn create_with_settings(
        name: IndexName,
        schema: Schema,
        settings: Value,
        config: &Config,
        data_dir: &Path,
    ) -> Result<Arc<Self>> {
        let index_dir = data_dir.join(name.as_str());
        std::fs::create_dir_all(&index_dir)?;

        let store_config = store_config_from(config);
        let store = IndexStore::open(&index_dir, store_config)?;

        let managed = ManagedSchema {
            schema,
            dynamic: xerj_common::schema::DynamicMapping::Dynamic,
        };

        // Doc count is a sanity cap only — the byte threshold is the primary
        // driver.  Historically this was 10 000, which forced flushes every ~8 MB
        // regardless of `flush_size_mb`, producing thousands of tiny segments on
        // log workloads.  Both are env-overridable — see resolve_flush_thresholds.
        let (flush_doc_threshold, flush_byte_threshold) =
            Self::resolve_flush_thresholds(config.storage.flush_size_mb);

        // Warn if >1 shard requested (we only support single-shard).
        if let Some(n) = settings
            .pointer("/index/number_of_shards")
            .and_then(Value::as_u64)
        {
            if n > 1 {
                tracing::warn!(
                    shards = n,
                    index = name.as_str(),
                    "xerj is single-shard; number_of_shards={} is ignored",
                    n
                );
            }
        }

        // Persist settings alongside schema.
        let settings_path = index_dir.join("settings.json");
        if !settings.is_null() {
            let bytes = serde_json::to_vec_pretty(&settings)?;
            write_file_atomic(&settings_path, &bytes)?;
        }

        // Persist the schema at create time so an explicit mapping
        // (PUT /{index} with a mappings body) survives a restart.
        // Historically schema.json was only written by put_mapping /
        // dynamic-field evolution, so a create-time mapping silently
        // degraded to ManagedSchema::dynamic() (empty) after reboot —
        // keyword FST routing, analyzers, and doc-values typing all
        // reverted to inference until a manual _mapping PUT.
        {
            let bytes = serde_json::to_vec_pretty(&managed)?;
            write_file_atomic(&index_dir.join("schema.json"), &bytes)?;
        }

        // Build analyzer registry, applying any custom analysis settings.
        let registry = Arc::new(build_registry_from_settings(&settings));

        info!(name = name.as_str(), "index created");
        let index = Arc::new(Self {
            name,
            schema: Arc::new(RwLock::new(managed)),
            store,
            memtable: Arc::new(
                crate::memtable::ShardedFtsMemtable::with_registry_and_shards(
                    Arc::clone(&registry),
                    config.engine.ingest_shards,
                ),
            ),
            doc_count: Arc::new(AtomicU64::new(0)),
            noop_update_count: Arc::new(AtomicU64::new(0)),
            request_cache_seen: Arc::new(RwLock::new(RequestCacheSeen::with_capacity(65_536))),
            request_cache_hits: Arc::new(AtomicU64::new(0)),
            request_cache_misses: Arc::new(AtomicU64::new(0)),
            registry,
            data_dir: index_dir,
            flush_doc_threshold,
            flush_byte_threshold,
            settings: Arc::new(RwLock::new(settings)),
            schema_hash_cache: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            schema_hash_epoch: Arc::new(AtomicU64::new(0)),
            last_flush_doc_count: Arc::new(AtomicU64::new(0)),
            max_concurrent_queries: Arc::new(Semaphore::new(64)),
            enrichments: Arc::new(RwLock::new(HashMap::new())),
            hnsw: Arc::new(RwLock::new(None)),
            hnsw_id_map: Arc::new(RwLock::new(HashMap::new())),
            hnsw_id_rev: Arc::new(RwLock::new(HashMap::new())),
            hnsw_next_id: Arc::new(AtomicU64::new(1)),
            sq8_stores: Arc::new(RwLock::new(HashMap::new())),
            metric_query_count: Arc::new(AtomicU64::new(0)),
            metric_query_total_ms: Arc::new(AtomicU64::new(0)),
            metric_index_count: Arc::new(AtomicU64::new(0)),
            metric_index_total_ms: Arc::new(AtomicU64::new(0)),
            metric_get_count: Arc::new(AtomicU64::new(0)),
            metric_get_total_ms: Arc::new(AtomicU64::new(0)),
            metric_get_exists_count: Arc::new(AtomicU64::new(0)),
            metric_get_missing_count: Arc::new(AtomicU64::new(0)),
            flush_sema: Arc::new(tokio::sync::Semaphore::new(
                config.engine.ingest_shards.max(4),
            )),
            merge_in_progress: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            merge_config: config.merge.clone(),
            embedding_proxy: Arc::new(make_embedding_proxy(&config.embedding)),
            dv_cache: Arc::new(dashmap::DashMap::new()),
            sort_shadow_cache: Arc::new(dashmap::DashMap::new()),
            sort_shadow_fields: Arc::new(dashmap::DashMap::new()),
            stored_slices_build_locks: Arc::new(dashmap::DashMap::new()),
            range_prefilter_cache: Arc::new(dashmap::DashMap::new()),
            id_pos_cache: Arc::new(dashmap::DashMap::new()),
            stored_value_cache: Arc::new(dashmap::DashMap::new()),
            stored_slices_cache: Arc::new(dashmap::DashMap::new()),
            stored_slices_cache_bytes: Arc::new(AtomicU64::new(0)),
            decoded_stored_cache: Arc::new(dashmap::DashMap::new()),
            decoded_stored_cache_bytes: Arc::new(AtomicU64::new(0)),
            shortcut_count_cache: Arc::new(dashmap::DashMap::new()),
            regexp_expand_cache: Arc::new(dashmap::DashMap::new()),
            fast_date_cache: Arc::new(dashmap::DashMap::new()),
            fast_date_sorted_cache: Arc::new(dashmap::DashMap::new()),
            query_cache: Arc::new(dashmap::DashMap::new()),
            dataset_version: Arc::new(AtomicU64::new(0)),
            query_inflight: Arc::new(dashmap::DashMap::new()),
            metric_singleflight_coalesced: Arc::new(AtomicU64::new(0)),
            flush_signal: Arc::new(SyncFlushCoord::new()),
            external_versions: Arc::new(dashmap::DashMap::new()),
            merge_task: Arc::new(parking_lot::Mutex::new(None)),
        });
        // Kick off the background merge pass.  5 s cadence is aggressive
        // enough to collapse a burst of flushes quickly without burning a
        // core — every pass is cheap when there's nothing to merge.
        index.spawn_merge_task(5);
        Ok(index)
    }

    /// Open an existing index at `data_dir/<name>`.
    pub fn open(name: IndexName, config: &Config, data_dir: &Path) -> Result<Arc<Self>> {
        let index_dir = data_dir.join(name.as_str());

        if !index_dir.exists() {
            return Err(EngineError::Common(
                xerj_common::XerjError::index_not_found(name.as_str()),
            ));
        }

        let store_config = store_config_from(config);
        let store = IndexStore::open(&index_dir, store_config)?;

        // Estimate doc count from snapshot.
        let snap = store.snapshot();
        let segment_doc_count: u64 = snap.segments.iter().map(|s| s.doc_count).sum();
        drop(snap);

        // Load schema from disk if it exists.
        let schema = load_schema(&index_dir).unwrap_or_else(|_| ManagedSchema::dynamic());

        // Load persisted settings early so we can build the registry before WAL replay.
        let settings = load_settings(&index_dir).unwrap_or(Value::Null);

        // Build analyzer registry from persisted settings so WAL replay uses
        // the same custom analyzers (synonyms, ngrams, etc.) that were active
        // when the documents were originally indexed.
        let registry = Arc::new(build_registry_from_settings(&settings));

        // Replay WAL entries into the FTS memtable.  The IndexStore already
        // replays the WAL into its own storage memtable (for future flushes);
        // we do a second pass here to rebuild the BM25 in-memory index so that
        // queries against un-flushed documents work correctly after a restart.
        let memtable = {
            let mem = crate::memtable::ShardedFtsMemtable::with_registry_and_shards(
                Arc::clone(&registry),
                config.engine.ingest_shards,
            );
            let wal_dir = store.wal_dir();
            // Must use the sharded-aware discovery: with the 16-shard WAL
            // layout the entries live in wal/s{N}/, and a reader on the
            // root directory alone replays nothing — the FTS memtable then
            // reopens empty and every unflushed doc is unsearchable and
            // un-GETtable until the next flush (chaos_data_integrity).
            let mut replayed = 0usize;
            for replay_entry in xerj_storage::wal::replay_all_sorted(&wal_dir) {
                match replay_entry.entry {
                    WalEntry::Index { doc_id, source } => {
                        mem.remove(&doc_id);
                        mem.insert(doc_id, &source, &schema.schema, replay_entry.seq_no);
                        replayed += 1;
                    }
                    WalEntry::Delete { doc_id } => {
                        mem.remove(&doc_id);
                        replayed += 1;
                    }
                    WalEntry::UpdateMapping { .. } => {}
                }
            }
            if replayed > 0 {
                info!(
                    name = name.as_str(),
                    replayed,
                    memtable_docs = mem.doc_count(),
                    "FTS memtable rebuilt from WAL"
                );
            }
            mem
        };

        let memtable_doc_count = memtable.doc_count() as u64;
        let total_doc_count = segment_doc_count + memtable_doc_count;

        // Conservative ghost init: at rest (no in-flight flushes) a live
        // count below the physical count means superseded copies exist
        // from a pre-restart history the WAL replay above cannot see.
        // Flag them so the delete-aware slow paths stay on — the exact
        // per-event tracking (`VersionMap::ghost_events`) only observes
        // events from this process lifetime onward.
        if (store.version_map.live_count() as u64) < total_doc_count {
            store.version_map.force_ghost_event();
        }

        // See create_with_settings — doc count is a sanity cap, byte threshold drives flushes.
        let (flush_doc_threshold, flush_byte_threshold) =
            Self::resolve_flush_thresholds(config.storage.flush_size_mb);

        // Try to reload a previously-persisted HNSW snapshot. If both
        // graph.bin and ids.json exist and validate, we skip the
        // O(N log N) WAL-replay rebuild and load the byte-identical
        // graph that was running pre-restart. Any error => None and the
        // first kNN search after a vector ingest re-creates the graph.
        let loaded_hnsw = load_hnsw_artifacts_sync(&index_dir.join("hnsw"));
        let initial_hnsw = loaded_hnsw.as_ref().map(|_| ()).is_some();
        let (hnsw_init, id_map_init, id_rev_init, next_id_init) = match loaded_hnsw {
            Some(l) => (Some(l.graph), l.id_map, l.id_rev, l.next_id),
            None => (None, HashMap::new(), HashMap::new(), 1),
        };
        if initial_hnsw {
            info!(name = name.as_str(), "HNSW reloaded from disk");
        }

        info!(
            name = name.as_str(),
            doc_count = total_doc_count,
            "index opened"
        );
        let index = Arc::new(Self {
            name,
            schema: Arc::new(RwLock::new(schema)),
            max_concurrent_queries: Arc::new(Semaphore::new(64)),
            enrichments: Arc::new(RwLock::new(HashMap::new())),
            store,
            memtable: Arc::new(memtable),
            doc_count: Arc::new(AtomicU64::new(total_doc_count)),
            noop_update_count: Arc::new(AtomicU64::new(0)),
            request_cache_seen: Arc::new(RwLock::new(RequestCacheSeen::with_capacity(65_536))),
            request_cache_hits: Arc::new(AtomicU64::new(0)),
            request_cache_misses: Arc::new(AtomicU64::new(0)),
            registry,
            data_dir: index_dir,
            flush_doc_threshold,
            flush_byte_threshold,
            settings: Arc::new(RwLock::new(settings)),
            schema_hash_cache: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            schema_hash_epoch: Arc::new(AtomicU64::new(0)),
            last_flush_doc_count: Arc::new(AtomicU64::new(0)),
            hnsw: Arc::new(RwLock::new(hnsw_init)),
            hnsw_id_map: Arc::new(RwLock::new(id_map_init)),
            hnsw_id_rev: Arc::new(RwLock::new(id_rev_init)),
            hnsw_next_id: Arc::new(AtomicU64::new(next_id_init)),
            sq8_stores: Arc::new(RwLock::new(HashMap::new())),
            metric_query_count: Arc::new(AtomicU64::new(0)),
            metric_query_total_ms: Arc::new(AtomicU64::new(0)),
            metric_index_count: Arc::new(AtomicU64::new(0)),
            metric_index_total_ms: Arc::new(AtomicU64::new(0)),
            metric_get_count: Arc::new(AtomicU64::new(0)),
            metric_get_total_ms: Arc::new(AtomicU64::new(0)),
            metric_get_exists_count: Arc::new(AtomicU64::new(0)),
            metric_get_missing_count: Arc::new(AtomicU64::new(0)),
            flush_sema: Arc::new(tokio::sync::Semaphore::new(
                config.engine.ingest_shards.max(4),
            )),
            merge_in_progress: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            merge_config: config.merge.clone(),
            embedding_proxy: Arc::new(make_embedding_proxy(&config.embedding)),
            dv_cache: Arc::new(dashmap::DashMap::new()),
            sort_shadow_cache: Arc::new(dashmap::DashMap::new()),
            sort_shadow_fields: Arc::new(dashmap::DashMap::new()),
            stored_slices_build_locks: Arc::new(dashmap::DashMap::new()),
            range_prefilter_cache: Arc::new(dashmap::DashMap::new()),
            id_pos_cache: Arc::new(dashmap::DashMap::new()),
            stored_value_cache: Arc::new(dashmap::DashMap::new()),
            stored_slices_cache: Arc::new(dashmap::DashMap::new()),
            stored_slices_cache_bytes: Arc::new(AtomicU64::new(0)),
            decoded_stored_cache: Arc::new(dashmap::DashMap::new()),
            decoded_stored_cache_bytes: Arc::new(AtomicU64::new(0)),
            shortcut_count_cache: Arc::new(dashmap::DashMap::new()),
            regexp_expand_cache: Arc::new(dashmap::DashMap::new()),
            fast_date_cache: Arc::new(dashmap::DashMap::new()),
            fast_date_sorted_cache: Arc::new(dashmap::DashMap::new()),
            query_cache: Arc::new(dashmap::DashMap::new()),
            dataset_version: Arc::new(AtomicU64::new(0)),
            query_inflight: Arc::new(dashmap::DashMap::new()),
            metric_singleflight_coalesced: Arc::new(AtomicU64::new(0)),
            flush_signal: Arc::new(SyncFlushCoord::new()),
            external_versions: Arc::new(dashmap::DashMap::new()),
            merge_task: Arc::new(parking_lot::Mutex::new(None)),
        });
        index.spawn_merge_task(5);
        Ok(index)
    }

    // ── Document operations ───────────────────────────────────────────────────

    /// Index a document, assigning an ID if one is not provided.
    ///
    /// If `if_seq_no` and `if_primary_term` are both provided, this performs an
    /// optimistic concurrency check: the document's current `seq_no` must match
    /// `if_seq_no`, otherwise a `VersionConflict` error is returned.
    pub async fn index_document(&self, id: Option<String>, source: Value) -> Result<IndexResponse> {
        self.index_document_with_version(id, source, None, None)
            .await
    }

    /// Index with caller-supplied `version` / `version_type`. Supports
    /// `external` (caller version MUST be strictly greater than
    /// current) and `external_gte` (caller version >= current).
    pub async fn index_document_external(
        &self,
        id: Option<String>,
        source: Value,
        version: u64,
        version_type: &str,
    ) -> Result<IndexResponse> {
        let doc_id = id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let strict = version_type != "external_gte";
        // CAS against the caller's prior external version.
        if let Some(existing) = self.external_versions.get(&doc_id) {
            let cur = *existing;
            let ok = if strict {
                version > cur
            } else {
                version >= cur
            };
            if !ok {
                return Err(EngineError::Common(
                    xerj_common::XerjError::version_conflict(&doc_id, version, cur),
                ));
            }
        }
        self.external_versions.insert(doc_id.clone(), version);
        let mut resp = self
            .index_document_with_version(Some(doc_id), source, None, None)
            .await?;
        resp.version = version;
        Ok(resp)
    }

    /// Index a document with optional optimistic concurrency control.
    pub async fn index_document_with_version(
        &self,
        id: Option<String>,
        source: Value,
        if_seq_no: Option<u64>,
        if_primary_term: Option<u64>,
    ) -> Result<IndexResponse> {
        // Check write block.
        if self.is_write_blocked().await {
            return Err(EngineError::Common(xerj_common::XerjError::index_blocked(
                self.name.as_str(),
                "write",
            )));
        }
        // Invalidate the response cache: every doc write bumps the
        // dataset version, which is part of the cache key, so old
        // entries become invisible to lookups.  No clear() needed —
        // they age out on the next 1k-entry truncation.
        self.dataset_version.fetch_add(1, Ordering::Release);

        let index_start = std::time::Instant::now();
        let doc_id = id.unwrap_or_else(|| Uuid::new_v4().to_string());

        // Optimistic concurrency check: honour `if_seq_no` whenever it
        // is supplied. ES requires `if_primary_term` alongside it, but
        // xerj is single-shard with primary_term=1, so we treat a
        // missing `if_primary_term` as equivalent to `1` rather than
        // silently skipping the check. Clients that pass a garbled
        // `if_primary_term` (e.g. an unresolved YAML variable) still
        // get the expected seq_no comparison.
        let _ = if_primary_term;
        if let Some(expected_seq) = if_seq_no {
            if let Some(entry) = self.store.version_map.get(&doc_id) {
                if entry.seq_no != expected_seq {
                    return Err(EngineError::Common(
                        xerj_common::XerjError::version_conflict(
                            &doc_id,
                            expected_seq,
                            entry.seq_no,
                        ),
                    ));
                }
            } else {
                // Document doesn't exist but caller expects a specific seq_no.
                return Err(EngineError::Common(
                    xerj_common::XerjError::version_conflict(&doc_id, expected_seq, 0),
                ));
            }
        }

        // `index.default_pipeline` is resolved and executed at the API layer
        // (xerj-api es_compat::resolve_effective_pipeline) before the document
        // reaches the engine, so no pipeline handling is needed here.

        // Auto-embed `semantic_text` fields: vectorise the field's text into
        // its companion vector field (`<field>_vector`) so it becomes
        // kNN-searchable. Runs before copy_to / storage so the derived vector
        // is part of `_source` and gets picked up by HNSW indexing below.
        let source = self.apply_semantic_embeddings(source).await?;

        // Apply copy_to: expand the source by copying field values to their target fields.
        let source = {
            let schema_guard = self.schema.read().await;
            apply_copy_to(&source, &schema_guard.schema)
        };

        // Detect whether this is an overwrite (existing doc) vs a new
        // insert BEFORE writing to storage. Used to set the response
        // `result` field to "updated" / "created" per ES semantics so
        // HTTP-layer 200/201 bulk status mapping works.
        let existed_before = self.store.version_map.get(&doc_id).is_some();

        // Write to storage WAL.
        let seq_no = self.store.index(&doc_id, source.clone())?;

        // Write to FTS memtable.
        {
            let schema_guard = self.schema.read().await;
            let mem = &*self.memtable;
            mem.remove(&doc_id);
            mem.insert(doc_id.clone(), &source, &schema_guard.schema, seq_no);
        }

        // Bump counter.
        let version = self.doc_count.fetch_add(1, Ordering::Relaxed) + 1;

        // Auto-evolve schema for new fields.
        // Fast path: skip the schema read lock every doc when schema is stable.
        // We cache a hash of the document field keys and only re-check schema
        // evolution every 100 docs (or when field keys change).
        let should_evolve = {
            let current_count = self.doc_count.load(Ordering::Relaxed);
            let last_epoch = self.schema_hash_epoch.load(Ordering::Relaxed);
            // Compute a quick hash of the doc's field names.
            let mut h: u64 = 0xcbf29ce484222325;
            if let Some(obj) = source.as_object() {
                for k in obj.keys() {
                    for b in k.bytes() {
                        h ^= b as u64;
                        h = h.wrapping_mul(0x00000100000001b3);
                    }
                }
            }
            let cached = self.schema_hash_cache.load(Ordering::Relaxed);
            if cached != h {
                // Field set changed — must evolve and update cache.
                self.schema_hash_cache.store(h, Ordering::Relaxed);
                self.schema_hash_epoch
                    .store(current_count, Ordering::Relaxed);
                true
            } else {
                // Same field set — only re-check every 100 docs.
                current_count.saturating_sub(last_epoch) >= 100
            }
        };
        if should_evolve {
            self.evolve_schema_from_doc(&source).await;
        }

        // Auto-detect log format on the first few documents and log it.
        let current_count = self.doc_count.load(Ordering::Relaxed);
        if current_count <= 5 {
            if let Some(fmt) = detect_log_format(&source) {
                debug!(
                    index = self.name.as_str(),
                    doc_count = current_count,
                    format = ?fmt,
                    "auto-detected log format"
                );
            }
        }

        // Index any vector fields into the HNSW index.
        self.index_vectors(&doc_id, &source).await;

        // Trigger a background flush if memtable exceeds size or doc-count threshold.
        self.maybe_spawn_flush().await;

        // Record indexing metrics.
        let index_elapsed_ms = index_start.elapsed().as_millis() as u64;
        self.metric_index_count.fetch_add(1, Ordering::Relaxed);
        self.metric_index_total_ms
            .fetch_add(index_elapsed_ms, Ordering::Relaxed);

        debug!(doc_id = doc_id.as_str(), seq_no, "document indexed");

        Ok(IndexResponse {
            id: doc_id,
            seq_no,
            version,
            result: if existed_before {
                "updated".to_string()
            } else {
                "created".to_string()
            },
        })
    }

    /// Batch index multiple documents using the turbo pipeline.
    ///
    /// Turbo mode achieves higher throughput than calling [`index_document`]
    /// in a loop by:
    ///
    /// 1. **Parallel tokenisation** — all documents in the batch are tokenised
    ///    concurrently via Rayon before any lock is acquired.
    /// 2. **Single lock acquisition** — the FTS memtable write lock is held for
    ///    the entire batch rather than once per document.
    /// 3. **Amortised WAL overhead** — WAL entries are written sequentially
    ///    inside the lock, but the expensive `fsync` cycle is paid only once for
    ///    the batch (as the underlying WAL writer is in `Batched` or `Async` mode).
    ///
    /// # Opt-in
    ///
    /// This method is called by the `/v1/indices/{name}/turbo-ingest` endpoint
    /// and by the standard `_bulk` path when the `X-Turbo: true` request header
    /// is present.
    ///
    /// # Trade-offs
    ///
    /// - When `fast_analyzer` is `true`, stemming and stopword removal are
    ///   skipped, which reduces search recall slightly.
    /// - Documents within the same batch are processed together; partial
    ///   failures are collected and returned rather than aborting the batch.
    pub async fn index_batch_turbo(
        &self,
        docs: Vec<(String, Value, Arc<[u8]>)>,
        parallel: bool,
        _fast_analyzer: bool,
    ) -> Result<Vec<IndexResponse>> {
        use crate::turbo_ingest::TurboIngestPipeline;

        if docs.is_empty() {
            return Ok(Vec::new());
        }

        if self.is_write_blocked().await {
            return Err(EngineError::Common(xerj_common::XerjError::index_blocked(
                self.name.as_str(),
                "write",
            )));
        }

        // ── Back-pressure ─────────────────────────────────────────────────
        // Reject the batch with 429 ResourceExhausted if the memtable is
        // already larger than 2× the flush byte threshold.  The previous
        // behaviour — accept-then-buffer-in-memory — caused xerj to OOM
        // under sustained bulk load because flushes could not keep up with
        // incoming writes.  Returning 429 lets upstream clients slow down
        // before the process dies.  The threshold is 2× so transient spikes
        // above the flush target don't trip back-pressure immediately.
        //
        // The check is a quick read under the memtable read-lock; no heavy
        // work is done before we decide to reject.
        //
        // V4 M4: soft back-pressure with bounded yield.
        //
        // When the memtable crosses 1.5 × threshold, we DON'T 429 the
        // client immediately — we kick a flush and yield to the tokio
        // scheduler for a few short naps.  If the background flush
        // drains enough memory within ~250 ms, the write proceeds;
        // otherwise we finally return 429.
        //
        // This eliminates the "200 k-docs/s burst → 63 dropped batches"
        // observed in the SIEM battle — the client hits a brief stall
        // instead of a hard reject, and the existing ingest driver's
        // retry logic isn't needed.
        let hard_block = self.flush_byte_threshold.saturating_mul(3);
        let soft_block = self.flush_byte_threshold.saturating_mul(2);
        {
            let mem_bytes = self.memtable.size_bytes();
            if mem_bytes >= soft_block {
                self.maybe_spawn_flush().await;
                // Block up to 30 s wall-clock until the memtable drains.
                // Pre-fix: 10 × 5 ms then error out — too short for a
                // sustained bulk-ingest workload where a flush of a
                // 512 MB segment takes 250 ms-2 s.  See
                // `2026-04-25T21-50-00_ingest_perf_regression_zstd19.md`.
                let bp_start = std::time::Instant::now();
                let bp_deadline = std::time::Duration::from_secs(30);
                while self.memtable.size_bytes() >= hard_block {
                    if bp_start.elapsed() >= bp_deadline {
                        self.maybe_spawn_flush().await;
                        return Err(EngineError::Common(
                            xerj_common::XerjError::resource_exhausted(format!(
                                "indexing back-pressure: memtable={}MB exceeds 3×flush_threshold={}MB on index [{}] after {}s wait — flusher may be stuck",
                                self.memtable.size_bytes() / (1024 * 1024),
                                self.flush_byte_threshold / (1024 * 1024),
                                self.name.as_str(),
                                bp_deadline.as_secs(),
                            )),
                        ));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    if bp_start.elapsed().as_millis() % 200 < 25 {
                        self.maybe_spawn_flush().await;
                    }
                }
            }
        }

        let index_start = std::time::Instant::now();
        let batch_len = docs.len();

        // ── Step 1: parallel tokenisation (no locks held) ──────────────────
        //
        // Build the pipeline with a batch size equal to the full input so it
        // never auto-flushes mid-way through.
        let mut pipeline = TurboIngestPipeline::new(batch_len + 1, parallel);
        for (id, source, source_bytes) in docs {
            pipeline.push(id, source, source_bytes);
        }
        // Flush collects all IngestResult values (with pre-computed tokens).
        let results = pipeline.flush();

        // ── Step 2: apply copy_to on all docs ─────────────────────────────
        // apply_copy_to deep-clones the document Map — on a 10-field nginx
        // schema with zero copy_to entries that's ~60 allocs per doc for
        // nothing.  Check the schema ONCE per batch and skip the clone
        // entirely on the common (no copy_to) path, wrapping the original
        // source in an Arc directly.
        let schema_guard = self.schema.read().await;
        let has_copy_to = schema_has_copy_to(&schema_guard.schema);
        let processed: Vec<_> = if has_copy_to {
            results
                .into_iter()
                .map(|r| {
                    let source =
                        std::sync::Arc::new(apply_copy_to(&r.source, &schema_guard.schema));
                    crate::turbo_ingest::IngestResult {
                        id: r.id,
                        tokens: r.tokens,
                        source,
                        source_bytes: r.source_bytes,
                    }
                })
                .collect()
        } else {
            // Zero-copy path — source is already an `Arc<Value>` from the
            // pipeline, no deep clone.
            results
        };
        drop(schema_guard);

        // ── Step 3+4: WAL append + FTS memtable insert under one lock ─────
        //
        // The engine memtable write lock spans BOTH the WAL append and
        // the FTS memtable push so that (a) the engine memtable and
        // the storage memtable (inside `wal_append_batch`) see docs
        // in identical order — flush relies on that for FTS-ordinal /
        // stored-section alignment — and (b) no other concurrent bulk
        // batch can race the drain-vs-push interleaving.
        //
        // A previous M5.0 attempt lifted the WAL out of this lock to
        // let concurrent clients pipeline.  It regressed throughput
        // because the two memtables DESYNCED (storage mem had the
        // docs a batch just pushed, engine mem still had fewer) and
        // the periodic flush would pick up a tiny storage drain and
        // a mismatched engine drain — producing 20 k-doc flush
        // segments where 150 k were expected, thrashing the flush
        // path and triggering back-pressure 429s.  Rolled back.
        let t3 = std::time::Instant::now();
        let wal_refs: Vec<(String, Arc<Value>, Arc<[u8]>)> = processed
            .iter()
            .map(|r| {
                (
                    r.id.clone(),
                    Arc::clone(&r.source),
                    Arc::clone(&r.source_bytes),
                )
            })
            .collect();

        let seq_nos = self.store.wal_append_batch(&wal_refs)?;
        let t3_dur = t3.elapsed();

        // ── M5.1 HOT PATH: one shard lock per BATCH ───────────────────────
        //
        // Pre-M5.1 we called `mem.insert_pretokenized_with_seq(...)` per
        // doc, which acquired the shard mutex TWICE per doc (once for
        // `remove`, once for `insert`).  For a 5 000-doc batch that's
        // 10 000 mutex acquisitions — high contention across 4
        // concurrent clients caused a 4× throughput regression (99 k →
        // 23 k docs/s).
        //
        // Now the bulk batch is routed to exactly ONE shard (by the
        // first doc_id's hash) and the shard lock is held for the
        // entire batch loop — so 4 concurrent clients routed to 4
        // different shards run in parallel with zero per-batch lock
        // churn.
        let t4 = std::time::Instant::now();
        let mut responses = Vec::with_capacity(batch_len);
        let shard_idx = if processed.is_empty() {
            0
        } else {
            // Use the instance method so routing matches the actual
            // configured shard count. The static `shard_for` is hardcoded
            // to assume 16 shards and panics on machines configured for
            // fewer (e.g. ingest_shards=2 on a 4-core box).
            self.memtable.shard_for_dynamic(&processed[0].id)
        };
        // Chunked insert — same read-under-write rationale as the raw path
        // (`index_batch_turbo_raw`): release the shard write lock between
        // chunks so a concurrent search stalls on a writing shard for at
        // most one chunk, not the whole batch.  `remove()` still precedes
        // each `insert_pretokenized_with_seq`, so overwrite semantics are
        // preserved regardless of the chunk boundary.
        {
            let mut base = 0usize;
            while base < batch_len {
                let end = (base + MEMTABLE_INSERT_CHUNK).min(batch_len);
                self.memtable.with_shard_mut(shard_idx, |mem| {
                    for i in base..end {
                        let ingest = &processed[i];
                        mem.remove(&ingest.id);
                        mem.insert_pretokenized_with_seq(
                            seq_nos[i],
                            ingest.id.clone(),
                            Arc::clone(&ingest.source),
                            &ingest.tokens,
                        );

                        let version = self.doc_count.fetch_add(1, Ordering::Relaxed) + 1;

                        responses.push(IndexResponse {
                            id: ingest.id.clone(),
                            seq_no: seq_nos[i],
                            version,
                            result: "created".to_string(),
                        });
                    }
                });
                base = end;
            }
        }
        let t4_dur = t4.elapsed();

        if batch_len >= 1000 {
            tracing::debug!(
                batch_len,
                tokenize_ms = index_start.elapsed().as_millis() as u64
                    - t3_dur.as_millis() as u64
                    - t4_dur.as_millis() as u64,
                wal_ms = t3_dur.as_millis() as u64,
                memtable_ms = t4_dur.as_millis() as u64,
                "turbo batch complete"
            );
        }

        // ── Step 5: schema evolution + vector indexing (post-lock) ────────
        //
        // These were previously called PER DOC inside a `for ingest in
        // &processed` loop = 10 000 async state-machine steps + 10 000
        // `RwLock::read_acquire` ops per 5 000-doc batch.  For the
        // common log-ingest case (stable schema, no dense-vector
        // fields) both functions are no-ops but still paid the lock +
        // `.await` overhead, costing measurable wall time.
        //
        // Hoisted once-per-batch: we take the schema read lock EXACTLY
        // ONCE, scan all sources for unknown field names in one pass,
        // and only call the slow-path `evolve_schema_from_doc` when
        // something new is actually detected.  Similarly for vectors —
        // we detect presence-of-any-vector-field via a single pass over
        // the first doc's object keys (sufficient for our schema model
        // because vector fields are fixed per index, not per doc).
        {
            let schema_guard = self.schema.read().await;
            let is_dynamic = matches!(
                schema_guard.dynamic,
                xerj_common::schema::DynamicMapping::Dynamic
            );
            // Fast-path: for a stable, non-dynamic schema we can skip
            // the entire per-doc evolve pass.  Under dynamic mapping we
            // cheaply collect the union of unknown field names seen in
            // this batch under the read lock, then only upgrade to a
            // write lock if we actually found any.
            let mut unknown_field_sources: Vec<&Value> = Vec::new();
            if is_dynamic {
                for ingest in &processed {
                    if let Some(obj) = ingest.source.as_object() {
                        let has_unknown = obj.keys().any(|k| !schema_guard.schema.has_field(k));
                        if has_unknown {
                            unknown_field_sources.push(ingest.source.as_ref());
                        }
                    }
                }
            }
            drop(schema_guard);
            for src in unknown_field_sources {
                self.evolve_schema_from_doc(src).await;
            }
        }

        // Vector indexing is only meaningful when at least one document
        // in the batch carries an array of numbers.  Detecting that
        // costs a single pass over the first doc's top-level object;
        // for log workloads (no vector fields) this skips the async
        // HNSW lock acquire entirely.
        if processed
            .first()
            .and_then(|r| r.source.as_object())
            .map(|obj| {
                obj.values().any(|v| {
                    v.as_array()
                        .map(|arr| !arr.is_empty() && arr.iter().all(Value::is_number))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
        {
            for ingest in &processed {
                self.index_vectors(&ingest.id, &ingest.source).await;
            }
        }

        // ── Step 5: check flush threshold ─────────────────────────────────
        self.maybe_spawn_flush().await;

        // ── Metrics ───────────────────────────────────────────────────────
        let elapsed_ms = index_start.elapsed().as_millis() as u64;
        self.metric_index_count
            .fetch_add(batch_len as u64, Ordering::Relaxed);
        self.metric_index_total_ms
            .fetch_add(elapsed_ms, Ordering::Relaxed);

        debug!(batch = batch_len, elapsed_ms, "turbo batch indexed");

        Ok(responses)
    }

    /// M5.11 — ULTRA-turbo raw-bytes ingest.
    ///
    /// Takes already-serialized NDJSON document lines and pushes them
    /// through the WAL + memtable without ever running
    /// `serde_json::from_str` on the document body.  The JSON parse is
    /// deferred all the way to drain-for-flush time, where it runs on
    /// a background task without contending with HTTP workers.
    ///
    /// Trade-off: schema auto-evolution and vector HNSW indexing do
    /// NOT fire on this path — the parsed `Value` they need isn't
    /// built.  Callers must pre-register the index mapping and avoid
    /// vector fields on this endpoint.  Nginx-style log ingest is the
    /// canonical use case.
    pub async fn index_batch_turbo_raw(
        &self,
        docs: Vec<(String, Arc<[u8]>)>,
    ) -> Result<Vec<IndexResponse>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        if self.is_write_blocked().await {
            return Err(EngineError::Common(xerj_common::XerjError::index_blocked(
                self.name.as_str(),
                "write",
            )));
        }

        let hard_block = self.flush_byte_threshold.saturating_mul(3);
        let soft_block = self.flush_byte_threshold.saturating_mul(2);
        {
            let mem_bytes = self.memtable.size_bytes();
            if mem_bytes >= soft_block {
                self.maybe_spawn_flush().await;
                // Block up to 30 s wall-clock until the memtable drains.
                // Pre-fix: 10 × 5 ms then error out — too short for a
                // sustained bulk-ingest workload where a flush of a
                // 512 MB segment takes 250 ms-2 s.  See
                // `2026-04-25T21-50-00_ingest_perf_regression_zstd19.md`.
                let bp_start = std::time::Instant::now();
                let bp_deadline = std::time::Duration::from_secs(30);
                while self.memtable.size_bytes() >= hard_block {
                    if bp_start.elapsed() >= bp_deadline {
                        self.maybe_spawn_flush().await;
                        return Err(EngineError::Common(
                            xerj_common::XerjError::resource_exhausted(format!(
                                "indexing back-pressure: memtable={}MB exceeds 3×flush_threshold={}MB on index [{}] after {}s wait — flusher may be stuck",
                                self.memtable.size_bytes() / (1024 * 1024),
                                self.flush_byte_threshold / (1024 * 1024),
                                self.name.as_str(),
                                bp_deadline.as_secs(),
                            )),
                        ));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    if bp_start.elapsed().as_millis() % 200 < 25 {
                        self.maybe_spawn_flush().await;
                    }
                }
            }
        }

        let index_start = std::time::Instant::now();
        let batch_len = docs.len();
        // THROWAWAY prof (XERJ_PROF): per-phase attribution of the turbo path.
        let prof = std::env::var_os("XERJ_PROF").is_some();
        let mut p_wal_us = 0u128;
        let mut p_parse_us = 0u128;
        let mut p_evolve_us = 0u128;
        let mut p_analyze_us = 0u128;
        let mut p_insert_us = 0u128;
        let p_t = std::time::Instant::now();

        // Build WAL refs directly from bytes.  We pass `Arc<Value::Null>`
        // as the `source` parameter because `wal_append_batch` uses
        // `source_bytes` when it's non-empty and never touches the
        // Value tree on the fast path.
        let null_val: Arc<Value> = Arc::new(Value::Null);
        let wal_refs: Vec<(String, Arc<Value>, Arc<[u8]>)> = docs
            .iter()
            .map(|(id, bytes)| (id.clone(), Arc::clone(&null_val), Arc::clone(bytes)))
            .collect();

        let seq_nos = self.store.wal_append_batch(&wal_refs)?;
        if prof {
            p_wal_us = p_t.elapsed().as_micros();
        }

        // Insert each doc EXACTLY ONCE into the engine FTS memtable,
        // routed to the doc's OWN shard — bit-for-bit identical to the
        // per-doc `index_document` path.
        //
        // The earlier "ultra-turbo" shape pushed every doc TWICE: once
        // as a raw-bytes entry pinned to docs[0]'s shard (intended for
        // _source) and again via `insert` on the doc's own shard (for
        // search/aggs).  That double-represented each doc in
        // `memtable.docs` — often on two different shards — so the
        // brute-force agg corpus (`all_docs_with_sources`) and every
        // terms/stats/cardinality/date_histogram aggregation counted
        // each UNFLUSHED doc twice (140-test regression).
        //
        // A single `insert` discharges BOTH responsibilities: it stores
        // the parsed `source` so `_source` GET (`get_doc_source_as_value`)
        // resolves it, AND it builds the inverted index + columnar
        // doc-values for search + aggregations.  `wal_append_batch`
        // above already wrote the WAL frames and set the version_map
        // (so `_count`/`hits.total` via `version_map.live_count()` and
        // GET visibility are correct), exactly like `self.store.index`
        // does on the per-doc path.
        // ── P2.1: intra-request shard fan-out (the DWPT equivalent). ──
        //
        // The pre-P2.1 shape indexed serially, doc-by-doc, so a single
        // `_bulk` request used exactly one core regardless of the cores
        // available (measured: ~104% CPU on a 32-core box). We now:
        //   1. parse every doc's JSON in parallel (the dominant per-doc
        //      CPU cost) on the rayon pool;
        //   2. evolve the schema SERIALLY in doc order (rare, additive;
        //      a no-op after the first few docs — see
        //      `evolve_schema_from_doc`), so dynamic-mapping resolution
        //      is order-deterministic and identical to the serial path;
        //   3. bucket doc indices by their OWN shard and insert each
        //      shard's docs in parallel, holding that shard's lock
        //      exactly once. Because the buckets partition by shard, no
        //      two rayon workers ever contend on the same lock — one
        //      bulk request now fans across min(Ncore, Nshard) cores.
        //
        // Correctness invariants preserved bit-for-bit vs the serial
        // loop: (a) each doc is inserted EXACTLY ONCE on its own shard
        // with its own `seq_nos[i]`; (b) `version` = prior doc_count +
        // position + 1, assigned in doc order via one batch-level
        // `fetch_add`; (c) response order matches request order.
        use rayon::prelude::*;

        // 1. Parse in parallel — each doc exactly once.  Arc-wrapped so
        // the memtable entry can share the allocation (no deep clone at
        // insert time — pre-fix `insert` did `Arc::new(source.clone())`
        // per doc under the shard write lock).
        //
        // Runs on the dedicated ingest pool (`crate::ingest_pool`) so bulk
        // parse bursts never queue ahead of search/agg par_iters on the
        // global rayon pool — see the read-under-write collapse notes on
        // `ingest_pool()`.
        let p_t = std::time::Instant::now();
        let sources: Vec<Arc<Value>> = crate::ingest_pool().install(|| {
            docs.par_iter()
                .map(|(_, bytes)| {
                    Arc::new(
                        serde_json::from_slice::<Value>(bytes)
                            .unwrap_or_else(|_| Value::Object(serde_json::Map::new())),
                    )
                })
                .collect()
        });
        if prof {
            p_parse_us = p_t.elapsed().as_micros();
        }

        // 2. Dynamic mapping: evolve schema in doc order — batched (one
        // schema read-lock for the whole batch, not one per doc).
        let p_t = std::time::Instant::now();
        self.evolve_schema_from_docs(&sources).await;
        if prof {
            p_evolve_us = p_t.elapsed().as_micros();
        }

        // 3. Parallel shard-partitioned insert. Partition the batch by
        // each doc's own shard, then insert each shard's sub-batch on a
        // rayon worker holding that shard's lock exactly once — the
        // DocumentsWriterPerThread analogue.
        {
            let schema_guard = self.schema.read().await;
            let schema = &schema_guard.schema;
            let mem = &*self.memtable;

            // 3a. Pre-analyze every doc OUTSIDE the shard locks.  The
            // schema text-field extraction + dynamic-mapping tree walk
            // + analyzer tokenisation are the dominant per-doc CPU cost
            // of `insert` (~30 s of tokenize per 1 M docs) and are pure
            // — running them here on the rayon pool instead of inside
            // `with_shard_mut` cuts the lock-held work from ~39 µs/doc
            // to ~9 µs/doc, which is what lets 8 concurrent bulk
            // clients actually scale instead of convoying on the shard
            // write locks.  Uses the exact same analyzer lookup as
            // `FtsMemtable::insert`, so the postings are bit-identical.
            let analyzer = mem
                .default_analyzer()
                .expect("standard analyzer always present");
            let p_t = std::time::Instant::now();
            let analyzed: Vec<Vec<(String, Vec<xerj_fts::analyzer::Token>)>> = crate::ingest_pool()
                .install(|| {
                    sources
                        .par_iter()
                        .map(|source| {
                            crate::memtable::analyze_doc(source.as_ref(), schema, &analyzer)
                        })
                        .collect()
                });
            if prof {
                p_analyze_us = p_t.elapsed().as_micros();
            }
            let p_t = std::time::Instant::now();

            let n_shards = mem.shard_count().max(1);
            let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); n_shards];
            for (i, (id, _)) in docs.iter().enumerate() {
                buckets[mem.shard_for_dynamic(id)].push(i);
            }
            crate::ingest_pool().install(|| {
                buckets.par_iter().enumerate().for_each(|(shard, idxs)| {
                    if idxs.is_empty() {
                        return;
                    }
                    mem.with_shard_mut(shard, |m| {
                        for &i in idxs {
                            let (id, bytes) = &docs[i];
                            // `remove` first so a same-id overwrite within
                            // the batch leaves exactly one live entry
                            // (mirrors index_document).
                            m.remove(id);
                            // Same sizing formula as `insert` (`raw * 3 + 64`)
                            // with the raw NDJSON byte length standing in for
                            // `source.to_string().len()` — keeps the size
                            // re-serialisation off the lock too.
                            let size = (bytes.len() + id.len()) * 3 + 64;
                            m.insert_analyzed(
                                seq_nos[i],
                                id.clone(),
                                Arc::clone(&sources[i]),
                                &analyzed[i],
                                size,
                            );
                        }
                    });
                });
            });
            if prof {
                p_insert_us = p_t.elapsed().as_micros();
            }
        }

        if prof {
            eprintln!(
                "XERJ_PROF turbo-batch docs={} wal_us={} parse_us={} evolve_us={} analyze_us={} insert_us={} total_us={}",
                batch_len, p_wal_us, p_parse_us, p_evolve_us, p_analyze_us, p_insert_us,
                index_start.elapsed().as_micros()
            );
        }

        // 4. One batch-level version stamp, assigned in request order.
        let base = self
            .doc_count
            .fetch_add(batch_len as u64, Ordering::Relaxed);
        let responses: Vec<IndexResponse> = docs
            .iter()
            .enumerate()
            .map(|(i, (id, _))| IndexResponse {
                id: id.clone(),
                seq_no: seq_nos[i],
                version: base + i as u64 + 1,
                result: "created".to_string(),
            })
            .collect();

        self.maybe_spawn_flush().await;

        let elapsed_ms = index_start.elapsed().as_millis() as u64;
        self.metric_index_count
            .fetch_add(batch_len as u64, Ordering::Relaxed);
        self.metric_index_total_ms
            .fetch_add(elapsed_ms, Ordering::Relaxed);

        Ok(responses)
    }

    /// **Fully synchronous** raw-bytes ingest path.  Intended for bulk CLI
    /// indexing where the caller is a rayon worker — no `.await`, no
    /// `tokio::spawn`, no runtime crossing.  Returns the number of docs
    /// accepted (equal to `docs.len()` on success).
    ///
    /// This path is the sync twin of [`index_batch_turbo_raw`].  Both
    /// append to the same sharded WAL + memtable; the only difference is
    /// that flush scheduling and back-pressure are expressed via a
    /// `parking_lot::Condvar` (one futex pair per signal) instead of
    /// `tokio::time::sleep` + `tokio::spawn` (10-100 µs wake-up chain
    /// per call, futex-heavy at scale).
    ///
    /// **Write-block check is skipped** — the sync path is for bulk
    /// ingest harnesses (`xerj index`) which are not subject to per-
    /// index write blocks.  HTTP handlers should continue to use the
    /// async `index_batch_turbo_raw` which honours blocks.
    pub fn index_batch_sync_raw(&self, docs: Vec<(String, Arc<[u8]>)>) -> Result<usize> {
        if docs.is_empty() {
            return Ok(0);
        }

        let hard_block = self.flush_byte_threshold.saturating_mul(3);
        let soft_block = self.flush_byte_threshold.saturating_mul(2);
        {
            let mem_bytes = self.memtable.size_bytes();
            if mem_bytes >= soft_block {
                // Kick a flush on every shard — we're over the global
                // soft cap so they all need to drain.
                self.try_spawn_sync_flush_all();
                // Block on Condvar until the memtable drains below
                // soft_block.  Pre-fix this loop ran 10 × 5 ms = 50 ms
                // and then returned ResourceExhausted — that's far
                // shorter than a single flush takes (250 ms-2 s for a
                // 512 MB segment), so under sustained 16-32-worker
                // ingest the cap was reactive rather than preventive
                // and 75 % of batches errored out (see
                // `2026-04-25T21-50-00_ingest_perf_regression_zstd19.md`).
                //
                // Now: block up to 30 s wall-clock total, in 25 ms
                // Condvar-wake increments.  Returns error only if the
                // flusher is genuinely stuck (deadlock, OOM, disk
                // full).  At 30 s of unbroken back-pressure on a healthy
                // host, something else is wrong and the error is real.
                let bp_start = std::time::Instant::now();
                let bp_deadline = std::time::Duration::from_secs(30);
                while self.memtable.size_bytes() >= hard_block {
                    if bp_start.elapsed() >= bp_deadline {
                        self.try_spawn_sync_flush_all();
                        return Err(EngineError::Common(
                            xerj_common::XerjError::resource_exhausted(format!(
                                "indexing back-pressure: memtable={}MB exceeds 3×flush_threshold={}MB on index [{}] after {}s wait — flusher may be stuck",
                                self.memtable.size_bytes() / (1024 * 1024),
                                self.flush_byte_threshold / (1024 * 1024),
                                self.name.as_str(),
                                bp_deadline.as_secs(),
                            )),
                        ));
                    }
                    self.flush_signal
                        .wait_for_drain(std::time::Duration::from_millis(25));
                    // Re-kick periodically in case a flush completed and a
                    // new shard now needs draining.
                    if bp_start.elapsed().as_millis() % 200 < 25 {
                        self.try_spawn_sync_flush_all();
                    }
                }
            }
        }

        let index_start = std::time::Instant::now();
        let batch_len = docs.len();

        // Fast-path WAL append: no Arc<Value> wrapper, no per-batch Vec
        // allocation.  `wal_append_batch_raw` borrows docs directly.
        let seq_nos = self.store.wal_append_batch_raw(&docs)?;

        // Use the instance method so routing matches the actual configured
        // shard count. The previous `Self::shard_for` was hardcoded to a
        // 16-shard mask and panicked on machines configured for fewer.
        let shard_idx = self.memtable.shard_for_dynamic(&docs[0].0);
        // Consume `docs` by value — move each (String, Arc<[u8]>) into
        // the memtable without cloning.  The CLI bulk path generates
        // unique doc_ids so `insert_raw_bytes_fresh` is safe (no prior
        // entry with this id); we skip the `remove()` HashMap miss
        // lookup that the generic `insert_raw_bytes_with_seq` preceded.
        //
        // READ-UNDER-WRITE stall reduction: the insert loop is CHUNKED so
        // the shard write lock is released between chunks.  Previously a
        // whole bulk batch (mixval: 10 000 docs) was routed to ONE shard and
        // the write lock was held for the ENTIRE insert (~10-30 ms), so a
        // concurrent search that touched that shard (`doc_ids_bounded`,
        // `all_docs_with_sources_arc`, `terms_counts_columnar` all take
        // `s.read()` on every shard) blocked for the full batch.  Chunking
        // bounds a reader's *lock* stall on a writing shard to ONE chunk
        // (~0.5-1.5 ms).  NOTE: measured in isolation this removes the
        // shard-lock component of the read tail, but the mixed
        // read-under-write p99 is dominated by CPU/scheduler contention with
        // the full-speed ingest+flush+merge (already deprioritised via the
        // nice-pool ladder), which this does not address — see commit body.
        // The lock churn is trivial (batch_len/CHUNK acquisitions, e.g. 20
        // for a 10 k batch) — nothing like the per-DOC locking that once
        // caused the 4× ingest regression.  Correctness is unchanged:
        // seq_nos are pre-assigned, inserts stay in seq order, and a flush
        // drain that slips between chunks is safe (docs are independent and
        // `take_memtable_for_flush` re-sorts by seq_no).
        {
            let mut docs_iter = docs.into_iter();
            let mut i = 0usize;
            while i < batch_len {
                let end = (i + MEMTABLE_INSERT_CHUNK).min(batch_len);
                self.memtable.with_shard_mut(shard_idx, |mem| {
                    for &seq_no in &seq_nos[i..end] {
                        let (id, bytes) = docs_iter
                            .next()
                            .expect("docs_iter yields exactly batch_len items");
                        mem.insert_raw_bytes_fresh(seq_no, id, bytes);
                    }
                });
                i = end;
            }
        }
        // Single batch-level atomic instead of one per doc.  At 1.7 M/s
        // × 10 k batch = 170 batches/s this cuts ~17 M atomic ops/s of
        // cache-line bouncing on `doc_count`.
        self.doc_count
            .fetch_add(batch_len as u64, Ordering::Relaxed);

        // Cheap per-shard threshold check — one read-lock, one shard.
        let n_shards = self.memtable.shard_count().max(1);
        let per_shard_doc_t =
            staggered_per_shard_threshold(self.flush_doc_threshold, shard_idx, n_shards);
        let per_shard_byte_t =
            staggered_per_shard_threshold(self.flush_byte_threshold, shard_idx, n_shards);
        let (sd_docs, sd_bytes) = self.memtable.shard_load(shard_idx);
        if sd_docs >= per_shard_doc_t || sd_bytes >= per_shard_byte_t {
            self.try_spawn_sync_flush(shard_idx);
        }

        let elapsed_ms = index_start.elapsed().as_millis() as u64;
        self.metric_index_count
            .fetch_add(batch_len as u64, Ordering::Relaxed);
        self.metric_index_total_ms
            .fetch_add(elapsed_ms, Ordering::Relaxed);

        Ok(batch_len)
    }

    /// Spawn one `do_flush_shard` tokio task for `shard_idx`, bounded
    /// by `flush_sema` (non-blocking `try_acquire`).  Called from the
    /// **synchronous** ingest path — no `.await`, just a handful of
    /// atomics to queue the task onto the tokio runtime.  Drops the
    /// request if the sema is already at capacity; the next batch on
    /// this shard will re-request, so there's no lost flush.
    pub(crate) fn try_spawn_sync_flush(&self, shard_idx: usize) {
        let rt = match self.flush_signal.runtime() {
            Some(rt) => rt,
            None => return,
        };
        let permit = match Arc::clone(&self.flush_sema).try_acquire_owned() {
            Ok(p) => p,
            Err(_) => return,
        };
        let store = Arc::clone(&self.store);
        let memtable = Arc::clone(&self.memtable);
        let registry = Arc::clone(&self.registry);
        let data_dir = self.data_dir.clone();
        let field_configs = self.flush_signal.field_configs(&self.schema);
        let dataset_version = Arc::clone(&self.dataset_version);
        let query_cache = Arc::clone(&self.query_cache);
        let warm_caches = self.publish_warm_caches();
        // Permit is released by `on_drained` (fired after Phase 1 drain)
        // so the slow segment-write I/O in Phase 2 doesn't hold up the
        // per-shard flush permit pool.  The `Option` wrapper makes the
        // move into the FnOnce explicit — `take()` + `drop` releases it.
        let permit_cell: std::sync::Mutex<Option<tokio::sync::OwnedSemaphorePermit>> =
            std::sync::Mutex::new(Some(permit));
        let permit_cell = Arc::new(permit_cell);
        let permit_cell_cb = Arc::clone(&permit_cell);
        let flush_signal_cb = Arc::clone(&self.flush_signal);
        rt.spawn(async move {
            let on_drained = move || {
                if let Ok(mut guard) = permit_cell_cb.lock() {
                    let _ = guard.take();
                }
                // Wake any ingest thread blocked on back-pressure.
                flush_signal_cb.notify_drain();
            };
            let result = do_flush_shard(
                shard_idx,
                store,
                memtable,
                registry,
                data_dir,
                field_configs,
                on_drained,
                warm_caches,
            )
            .await;
            // Fallback: if do_flush_shard aborted before on_drained fired
            // (shouldn't happen with current structure but defensive),
            // the permit_cell still holds the permit — drop it here.
            if let Ok(mut guard) = permit_cell.lock() {
                let _ = guard.take();
            }
            if let Err(e) = result {
                tracing::error!(error = %e, shard_idx, "sync flush failed");
            }
            // P3.2 — additive invalidation. Bump the dataset version
            // (the query_cache is keyed by (query_hash, dataset_version)
            // so every cached result now misses) and clear the small
            // query_cache, but DO NOT clear the per-segment dv_cache: a
            // flush only CREATES new immutable segments and cannot
            // invalidate the doc-values of segments that already
            // existed. The old blanket clear forced a cold mmap+decode
            // on the next read after every flush — the flush-coincident
            // read-p99 spike. Segment-dropping merges evict the exact
            // dropped ids; see run_merge_once.
            dataset_version.fetch_add(1, Ordering::Release);
            query_cache.clear();
        });
    }

    fn try_spawn_sync_flush_all(&self) {
        let n = self.memtable.shard_count();
        for i in 0..n {
            self.try_spawn_sync_flush(i);
        }
    }

    /// Ultra-fast turbo ingest that **skips the WAL entirely**.
    ///
    /// Documents are written directly to the FTS memtable with zero disk I/O.
    /// Data durability is deferred until the next background flush, which
    /// writes a segment to disk.  In the event of a crash before flush,
    /// documents ingested via this method may be lost.
    ///
    /// # Trade-offs
    ///
    /// - **Speed**: WAL write is the dominant bottleneck for batches of
    ///   1 000+ docs.  Skipping it unlocks maximum CPU-bound throughput.
    /// - **Durability**: Documents are only durable after `flush()` completes.
    ///   For log pipelines where occasional re-ingestion is acceptable,
    ///   this is the preferred path.
    ///
    /// # When to use
    ///
    /// Enable with `X-Turbo-Realtime: true` on the bulk endpoint, or call
    /// this method directly when the caller can tolerate losing the most
    /// recent batch on crash.
    pub async fn index_batch_turbo_realtime(
        &self,
        docs: Vec<(String, Value, Arc<[u8]>)>,
        parallel: bool,
    ) -> Result<Vec<IndexResponse>> {
        use crate::turbo_ingest::TurboIngestPipeline;

        if docs.is_empty() {
            return Ok(Vec::new());
        }

        if self.is_write_blocked().await {
            return Err(EngineError::Common(xerj_common::XerjError::index_blocked(
                self.name.as_str(),
                "write",
            )));
        }

        let batch_len = docs.len();
        let index_start = std::time::Instant::now();

        // Step 1: Parallel tokenisation (no locks, no I/O).
        let mut pipeline = TurboIngestPipeline::new(batch_len + 1, parallel);
        for (id, source, source_bytes) in docs {
            pipeline.push(id, source, source_bytes);
        }
        let results = pipeline.flush();

        // Step 2: apply copy_to — wrap each source in Arc immediately.
        let schema_guard = self.schema.read().await;
        let processed: Vec<_> = results
            .into_iter()
            .map(|r| {
                let source = std::sync::Arc::new(apply_copy_to(&r.source, &schema_guard.schema));
                crate::turbo_ingest::IngestResult {
                    id: r.id,
                    tokens: r.tokens,
                    source,
                    source_bytes: r.source_bytes,
                }
            })
            .collect();
        drop(schema_guard);

        // Step 3: Update version map (lock-free atomic operations) — no WAL.
        // Assign monotonically increasing seq_nos from the engine's counter.
        use std::sync::atomic::Ordering;
        let seq_nos: Vec<u64> = processed
            .iter()
            .map(|_| self.doc_count.fetch_add(0, Ordering::Relaxed))
            .collect();

        // Step 4: FTS memtable insert — use standard insert() so the inverted
        // index is populated and match queries work correctly.
        let schema_guard2 = self.schema.read().await;
        let mem = &*self.memtable;
        let mut responses = Vec::with_capacity(batch_len);

        for (i, ingest) in processed.iter().enumerate() {
            mem.remove(&ingest.id);
            let seq_no = seq_nos[i];
            mem.insert(
                ingest.id.clone(),
                &ingest.source,
                &schema_guard2.schema,
                seq_no,
            );
            let version = self.doc_count.fetch_add(1, Ordering::Relaxed) + 1;
            responses.push(IndexResponse {
                id: ingest.id.clone(),
                seq_no,
                version,
                result: "created".to_string(),
            });
        }
        drop(schema_guard2);

        // Step 5: schema evolution + vector indexing (post-lock).
        for ingest in &processed {
            self.evolve_schema_from_doc(&ingest.source).await;
            self.index_vectors(&ingest.id, &ingest.source).await;
        }

        // Step 6: flush threshold check.
        self.maybe_spawn_flush().await;

        let elapsed_ms = index_start.elapsed().as_millis() as u64;
        self.metric_index_count
            .fetch_add(batch_len as u64, Ordering::Relaxed);
        self.metric_index_total_ms
            .fetch_add(elapsed_ms, Ordering::Relaxed);

        debug!(
            batch = batch_len,
            elapsed_ms, "turbo-realtime batch indexed (no WAL)"
        );

        Ok(responses)
    }

    /// Index a document only if it does NOT already exist (ES `op_type=create` semantics).
    ///
    /// Returns `Err(VersionConflict)` if a live document with the same ID exists.
    pub async fn create_document(&self, id: String, source: Value) -> Result<IndexResponse> {
        // Check write block first.
        if self.is_write_blocked().await {
            return Err(EngineError::Common(xerj_common::XerjError::index_blocked(
                self.name.as_str(),
                "write",
            )));
        }

        // Fail with 409 if a live document already exists with this id.
        let already_exists = {
            let in_version_map = self
                .store
                .version_map
                .get(&id)
                .map(|e| !e.deleted)
                .unwrap_or(false);
            let in_memtable = {
                let mem = &*self.memtable;
                mem.contains(&id)
            };
            in_version_map || in_memtable
        };

        if already_exists {
            return Err(EngineError::Common(
                xerj_common::XerjError::version_conflict(&id, 0, 1),
            ));
        }

        self.index_document(Some(id), source).await
    }

    /// Refresh: flush the current active memtable to a disk segment.
    ///
    /// After refresh, the memtable data is written to a new on-disk segment
    /// (FTS index + stored sources), the active memtable is replaced with a
    /// new empty one, and the old memtable is dropped entirely — freeing all
    /// RAM.  This keeps memory usage bounded regardless of dataset size.
    ///
    /// Search reads: active memtable (RAM) + disk segments (file I/O).
    pub async fn refresh(&self) -> Result<()> {
        {
            let mem = &*self.memtable;
            if mem.doc_count() == 0 {
                return Ok(());
            }
        }

        // Flush the memtable to a new segment on disk.  This drains the
        // memtable, writes the segment file + FTS index, swaps the snapshot,
        // and checkpoints the WAL.  The drained memtable is dropped, freeing
        // all RAM.
        let field_configs = {
            let schema = self.schema.read().await;
            build_fts_field_configs(&schema.schema)
        };
        // Explicit refresh: wait behind any in-flight concurrent flushes
        // (semaphore has 4 permits) so the user-visible flush sees a
        // fully-drained memtable once the await returns.
        let _permit = self.flush_sema.acquire().await.ok();
        // M5.2 — explicit refresh drains EVERY non-empty shard in
        // sequence (one shard per `do_flush_shard` call) so the
        // user-visible `refresh()` returns with zero unflushed
        // documents.  Sharded flushes run serially here because the
        // caller is waiting synchronously; the background scheduler
        // still fires shard flushes in parallel.
        let n_shards = self.memtable.shard_count();
        for shard_idx in 0..n_shards {
            do_flush_shard(
                shard_idx,
                Arc::clone(&self.store),
                Arc::clone(&self.memtable),
                Arc::clone(&self.registry),
                self.data_dir.clone(),
                field_configs.clone(),
                || {}, // serial refresh path — no permit to drop early
                self.publish_warm_caches(),
            )
            .await?;
        }

        // Update the doc counter so maybe_auto_flush uses a fresh baseline.
        self.last_flush_doc_count
            .store(self.doc_count.load(Ordering::Relaxed), Ordering::Relaxed);

        debug!(
            index = self.name.as_str(),
            "refresh complete — memtable flushed to disk segment"
        );
        Ok(())
    }

    /// Spawn a background flush task if the memtable has grown past the
    /// configured doc/byte thresholds.
    ///
    /// V4 M4.5 — CONCURRENT FLUSH PIPELINE.
    ///
    /// Previously a `flush_in_progress` bool limited us to one in-flight
    /// flush at a time.  With a single CPU-bound flush taking ~5-10 s for
    /// 100 k docs, ingest at 60 k docs/s fills the memtable faster than one
    /// flush can drain it, pushing back-pressure.  We now allow up to
    /// `max_concurrent_flushes` flushes to execute in parallel — each drain
    /// is atomic under the memtable write lock (so no two flushes see the
    /// same doc twice), segment ids are unique per flush, and snapshot
    /// publication is atomic via ArcSwap.  Multiple flushes writing
    /// distinct segments to disk in parallel is exactly what Lucene's
    /// IndexWriter does on sustained ingest.
    async fn maybe_spawn_flush(&self) {
        use std::sync::atomic::Ordering as AtomicOrdering;

        // M5.2 — per-shard flush scheduler.  Pre-M5.2 `do_flush`
        // drained ALL shards into one segment, making flush a global
        // serialisation point.  Now we walk per-shard loads and
        // spawn one flush task per shard whose size crosses a
        // per-shard threshold.  Because shards flush independently
        // into their own segments on their own tokio tasks, the
        // aggregate drain rate scales with the number of busy
        // shards instead of being pinned at the single-flush rate.
        //
        // Per-shard threshold: 1/N of the global threshold so the
        // cumulative memtable size across all shards is bounded at
        // the same `flush_byte_threshold` the user configured.
        let shards = self.memtable.shard_loads();
        // M5.3 — per-shard threshold is `global / shard_count` at
        // FULL size (no 15 % soft factor).  Pre-M5.3 the scheduler
        // used a 15 % soft trigger to smooth flush cadence on a
        // single-memtable design; with per-shard flushing the tiny
        // fixed cost per segment (FtsIndexWriter setup + FST build
        // + file I/O) dominates when flushes are only a few
        // thousand docs each, so we let shards fill closer to
        // their share of the global threshold before flushing.
        let n_shards_sched = shards.len().max(1);

        let field_configs_once: std::sync::OnceLock<
            HashMap<String, xerj_fts::index::FieldIndexConfig>,
        > = std::sync::OnceLock::new();

        for (shard_idx, docs, bytes) in shards {
            let per_shard_doc_t =
                staggered_per_shard_threshold(self.flush_doc_threshold, shard_idx, n_shards_sched);
            let per_shard_byte_t =
                staggered_per_shard_threshold(self.flush_byte_threshold, shard_idx, n_shards_sched);
            if docs < per_shard_doc_t && bytes < per_shard_byte_t {
                continue;
            }

            // Try to acquire a flush permit non-blockingly.  A global
            // semaphore caps total concurrent flushes so we don't OOM
            // trying to hold N concurrent drained memtable snapshots.
            let permit = match Arc::clone(&self.flush_sema).try_acquire_owned() {
                Ok(p) => p,
                Err(_) => continue,
            };

            let store = Arc::clone(&self.store);
            let memtable = Arc::clone(&self.memtable);
            let registry = Arc::clone(&self.registry);
            let data_dir = self.data_dir.clone();
            let dataset_version = Arc::clone(&self.dataset_version);
            let query_cache = Arc::clone(&self.query_cache);
            let warm_caches = self.publish_warm_caches();

            // Pre-build field configs ONCE for all shard flushes we
            // spawn this tick — avoids N schema read-lock acquires.
            let field_configs = if let Some(cfg) = field_configs_once.get() {
                cfg.clone()
            } else {
                let schema = self.schema.read().await;
                let cfg = build_fts_field_configs(&schema.schema);
                let _ = field_configs_once.set(cfg.clone());
                cfg
            };

            let flush_signal_cb = Arc::clone(&self.flush_signal);
            tokio::spawn(async move {
                // Release permit after drain (Phase 1), not after Phase 2
                // I/O — lets new flushes dispatch while segment writes
                // continue in parallel.
                let permit_cell = Arc::new(std::sync::Mutex::new(Some(permit)));
                let permit_cell_cb = Arc::clone(&permit_cell);
                let on_drained = move || {
                    if let Ok(mut guard) = permit_cell_cb.lock() {
                        let _ = guard.take();
                    }
                    // Wake any sync-ingest thread blocked on back-pressure.
                    flush_signal_cb.notify_drain();
                };
                let result = do_flush_shard(
                    shard_idx,
                    store,
                    memtable,
                    registry,
                    data_dir,
                    field_configs,
                    on_drained,
                    warm_caches,
                )
                .await;
                if let Ok(mut guard) = permit_cell.lock() {
                    let _ = guard.take();
                }
                if let Err(e) = result {
                    tracing::error!(error = %e, shard_idx, "background shard flush failed");
                }
                // P3.2 — additive invalidation. Old segment IDs are
                // still valid after a flush (segments are immutable), so
                // we keep their per-segment dv_cache / stored_value_cache
                // entries warm and only invalidate the query_cache via
                // the dataset-version bump. This removes the cold-read
                // p99 spike that the old blanket clear produced on every
                // flush. Merges evict their dropped segment ids exactly.
                dataset_version.fetch_add(1, AtomicOrdering::Release);
                query_cache.clear();
            });
        }
    }

    // ── Segment merging ────────────────────────────────────────────────────────

    /// Run one pass of the size-tiered merge policy.  Each selected batch is
    /// merged synchronously; FTS side-cars for the merged segment are built
    /// before the new segment is published, and the old segments' files are
    /// left in place (cleanup happens on next restart via snapshot GC).
    ///
    /// Returns the number of merge batches that succeeded.
    ///
    /// This is called from a background task (see `spawn_merge_task`) and may
    /// also be invoked explicitly by tests.
    pub async fn run_merge_once(&self) -> Result<usize> {
        use std::sync::atomic::Ordering as AtomicOrdering;

        // Serialise merges with ourselves (skip if another pass is running).
        if self
            .merge_in_progress
            .compare_exchange(false, true, AtomicOrdering::AcqRel, AtomicOrdering::Relaxed)
            .is_err()
        {
            return Ok(0);
        }
        // Ensure we clear the flag on every exit path.
        let _clear = MergeFlagClear(&self.merge_in_progress);
        self.merge_pass_locked(None).await
    }

    /// True while a merge pass (background or forced) holds the merge flag.
    /// Exposed through `_stats` so callers (benchmarks) can verify the
    /// index is merge-quiescent before starting a measurement phase.
    pub fn is_merge_in_progress(&self) -> bool {
        self.merge_in_progress
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// ES-style `_forcemerge`: run SYNCHRONOUSLY until the index has at
    /// most `max_num_segments` segments.
    ///
    /// Unlike `run_merge_once`, this does NOT bail out when a background
    /// pass currently holds the merge flag — it WAITS for the background
    /// pass to finish, takes the flag over, and then drives merge passes
    /// to convergence before returning.  The old behaviour (return
    /// `merged_batches: 0` immediately) meant a benchmark's
    /// "forcemerge + settle" step quiesced ES but left XERJ's background
    /// merge churning through the entire read phase, polluting every
    /// read-latency sample with merge CPU.
    ///
    /// On return the index is quiescent for the merged segment set: the
    /// memtable has been flushed, segments are merged to the target, and
    /// the size-tiered background policy selects no further work until
    /// new writes arrive.
    pub async fn force_merge(&self, max_num_segments: usize) -> Result<usize> {
        use std::sync::atomic::Ordering as AtomicOrdering;

        let target = max_num_segments.max(1);

        // NOTE: like ES, forcemerge operates on COMMITTED segments only —
        // it does not flush the memtable. (An earlier draft called
        // `self.refresh()` here; that exposed a pre-existing defect where
        // nested dense_vector kNN returns 0 hits once docs move from the
        // memtable into segments — see 135_knn_query_nested_search_ivf.yml.
        // Callers that want memtable docs included must refresh first,
        // exactly as with ES.)

        // Wait for any in-flight background pass instead of skipping.
        // 50 ms poll; background passes always terminate, and the HTTP
        // handler above us applies the request-level timeout.
        while self
            .merge_in_progress
            .compare_exchange(false, true, AtomicOrdering::AcqRel, AtomicOrdering::Relaxed)
            .is_err()
        {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let _clear = MergeFlagClear(&self.merge_in_progress);

        let mut total = 0usize;
        loop {
            let n = self.merge_pass_locked(Some(target)).await?;
            total += n;
            let seg_count = self.store.snapshot().segments.len();
            if n == 0 || seg_count <= target {
                break;
            }
        }
        Ok(total)
    }

    /// One merge pass.  Caller MUST hold `merge_in_progress` (and clear it
    /// after — see `MergeFlagClear`).
    ///
    /// `force_max_segments: None` → size-tiered policy (background path).
    /// `Some(target)` → forcemerge selection: EVERY segment (large ones
    /// included — ES ignores the max-merged-segment cap on an explicit
    /// forcemerge) is chunked into `max_merge_count`-sized batches; the
    /// caller loops passes until the index converges to `target` segments.
    async fn merge_pass_locked(&self, force_max_segments: Option<usize>) -> Result<usize> {
        use std::sync::atomic::Ordering as AtomicOrdering;
        use xerj_fts::index::FtsIndexWriter;
        use xerj_storage::merge::{MergePolicy, SizeTieredMergePolicy};
        use xerj_storage::segment::{SectionType, SegmentId, SegmentReader, SegmentWriter};

        // V4 M4.5 — larger merge batches so a bursty-ingest index with
        // thousands of small segments converges in far fewer passes.
        // Override via `XERJ_MAX_MERGE_COUNT` for battle tests that
        // want to collapse 2 k+ segments in a single pass.
        // M5.16 — merge memory bounds.
        //
        // Pre-M5.16 default max_merge_count was 64, which combined with
        // MERGE_PARALLELISM=4 meant up to 4 × 64 × ~30k = 7.6 M docs
        // held in per-batch `fts_input` / `dv_sources` Vec<Value>
        // simultaneously.  On the 20 M bq forcemerge that spiked RSS
        // to 93-117 GB and pushed the process to swap.
        //
        // Merge policy parameters from Config.merge — operators can tune
        // per-deployment without rebuilding. Defaults match the historical
        // hardcoded values (4 MiB tier floor, 8 GiB max merged segment,
        // min_merge_count=4, max_merge_count=16). The XERJ_MAX_MERGE_COUNT
        // env var was removed; use config.merge.max_merge_count instead.
        let mc = &self.merge_config;
        let policy = SizeTieredMergePolicy {
            min_merge_count: mc.min_merge_count as usize,
            max_merge_count: mc.max_merge_count as usize,
            tier_floor_bytes: mc.tier_floor_mb * 1024 * 1024,
            max_merged_segment_bytes: mc.max_segment_mb * 1024 * 1024,
        };

        let segments_snapshot_init = {
            let snap = self.store.snapshot();
            snap.segments.clone()
        };
        let batches: Vec<Vec<SegmentId>> = match force_max_segments {
            // Forcemerge: chunk every segment (smallest first) into
            // max_merge_count-sized batches, ignoring tier/size caps.
            Some(target) if segments_snapshot_init.len() > target => {
                let mut segs: Vec<&xerj_storage::segment::SegmentMeta> =
                    segments_snapshot_init.iter().collect();
                segs.sort_by_key(|s| s.size_bytes);
                let ids: Vec<SegmentId> = segs.into_iter().map(|s| s.id.clone()).collect();
                ids.chunks((mc.max_merge_count as usize).max(2))
                    .filter(|c| c.len() >= 2)
                    .map(|c| c.to_vec())
                    .collect()
            }
            Some(_) => Vec::new(),
            None => policy.select_merges(&segments_snapshot_init),
        };
        if batches.is_empty() {
            return Ok(0);
        }
        tracing::info!(
            num_segments_in = segments_snapshot_init.len(),
            num_batches = batches.len(),
            "run_merge_once: scheduling batches"
        );

        // Resolve per-field analyzer configs once for FTS rebuild.  See
        // `build_fts_field_configs` — keyword/numeric fields must use the
        // `keyword` analyzer or the FST drops their values to stop-words.
        let field_configs = {
            let schema = self.schema.read().await;
            build_fts_field_configs(&schema.schema)
        };
        // Kept for the legacy "if !text_fields.is_empty()" branch — empty
        // here because the merge path now indexes ALL source fields, not
        // just declared text fields.  See the merge FTS rebuild block.
        let _text_fields: Vec<String> = Vec::new();

        let segments_dir = self.data_dir.join("segments");
        let mut merged_batches = 0usize;
        // P3.2 — collect the exact segment ids a merge drops so we can
        // evict only their per-segment cache entries (vs clearing the
        // whole dv_cache / stored_value_cache).
        let mut dropped_seg_ids: Vec<String> = Vec::new();

        // V4 M4.7 — PARALLEL BATCHES.  Previously the for-loop awaited
        // each batch's spawn_blocking before starting the next, so only
        // one merge ever ran at a time.  On a 2253-segment index that
        // meant ~1 merge per 90 s = 3.5 HOURS to converge.  Spawning all
        // batches in one sweep lets the blocking thread pool fan out to
        // ~N CPU cores, limited only by the rayon pool rebuilding FTS
        // side-cars and by disk write bandwidth.  Observed gain: ~8×
        // on a 32-core host with 60 concurrent batches.
        //
        // Cap parallelism at `MERGE_PARALLELISM` to avoid RAM blowup
        // during the big initial convergence pass — each concurrent
        // merge holds its full drained doc set in RAM.
        // V4 M4.8 — back to 4 after the byte-copy merge refactor cut
        // per-batch peak RAM by ~5×.  Was: 2 (forced down after the
        // pre-M4.8 OOM); pre-M4.7: 8 (OOM'd at 70 GB RSS).  Now each
        // batch holds at most one segment's worth of decoded bytes
        // plus a compact `(id, fields, source)` stream — total ≈ 200 MB
        // per batch on this workload.  4 × 200 MB = 800 MB peak instead
        // of 16 GB.
        // M5.16 — reduce from 4 → 1.  See max_merge_count comment above.
        // 4 parallel × 16 segments × 30k docs × 5 KB = 10 GB peak — still
        // too high for a 20 M-doc index.  Serial merges at 16 segments
        // each give ~2.4 GB peak and let the index converge in
        // predictable time without swapping.  Override via env var
        // for machines with more RAM.
        let merge_parallelism: usize = std::env::var("XERJ_MERGE_PARALLELISM")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        use tokio::task::JoinHandle;
        type MergeOutput = (
            Vec<xerj_storage::segment::SegmentId>,
            xerj_storage::segment::SegmentMeta,
            usize,
            // (seq_no, doc_id) per surviving doc, in stored order — used to
            // write the merge output's `.ids` side-car after `apply_merge`
            // commits (flush already writes one; merge outputs previously
            // never got one, forcing the slow decode-stored fallback in
            // `rebuild_version_map_from_segments` on every reopen).
            Vec<(u64, String)>,
        );

        // Build the list of (batch, metas) pairs we'll launch.
        let mut launch_queue: Vec<(
            Vec<xerj_storage::segment::SegmentId>,
            Vec<xerj_storage::segment::SegmentMeta>,
        )> = Vec::new();
        for batch in batches {
            let snap = self.store.snapshot();
            let metas: Vec<xerj_storage::segment::SegmentMeta> = batch
                .iter()
                .filter_map(|id| snap.segments.iter().find(|s| &s.id == id).cloned())
                .collect();
            drop(snap);
            if metas.len() == batch.len() {
                launch_queue.push((batch, metas));
            }
        }

        // Launch up to MERGE_PARALLELISM tasks at a time, consuming the
        // queue as tasks complete.  Results are applied back to the
        // snapshot in the order they finish (apply_merge is atomic).
        let mut in_flight: Vec<JoinHandle<Option<MergeOutput>>> = Vec::new();
        let mut queue_iter = launch_queue.into_iter();
        let mut pending = queue_iter.next();

        let spawn_one = |batch: Vec<xerj_storage::segment::SegmentId>,
                         metas: Vec<xerj_storage::segment::SegmentMeta>|
         -> JoinHandle<Option<MergeOutput>> {
            let store_for_task = Arc::clone(&self.store);
            let registry_for_task = Arc::clone(&self.registry);
            let field_configs_for_task = field_configs.clone();
            let segments_dir_for_task = segments_dir.clone();
            let batch_for_task = batch;
            let metas_for_task = metas;

            tokio::task::spawn_blocking(move || -> Option<MergeOutput> {
                // Entire merge encode runs on the dedicated SMALL merge
                // pool (nice +15, ncores/8 threads): the decompress +
                // byte-copy + FTS/DV rebuild below saturates its workers
                // for seconds.  On the normal-priority blocking pool it
                // stole scheduler time from foreground searches (read-
                // under-write tail); on the shared ingest pool it queued
                // every bulk request's parse/analyze/insert behind it,
                // fully stalling 1 M×c8 ingest for ~13 s per merge pass
                // (see `crate::merge_pool`).
                crate::merge_pool().install(move || -> Option<MergeOutput> {
                    use serde_json::value::RawValue;

                    // V4 M4.8 — STREAMING BYTE-COPY MERGE.
                    //
                    // Pre-M4.8 path materialised every source segment as
                    // `Vec<serde_json::Value>` (~1-2 KB heap per doc), held all
                    // 16 × 30 k = 480 k Values in RAM, then `to_vec(&merged_docs)`
                    // → `encode_stored_v2` (which parses back to `Vec<Value>`).
                    // ~2 GB working set per batch; with PARALLELISM=8 that hit
                    // 70 GB total RSS during the 2 291-segment forcemerge and
                    // crawled at 16 k docs/s.
                    //
                    // New path:
                    //   1. Decode ONE segment at a time (RAM ≈ 1 segment).
                    //   2. Parse it as `Vec<Box<RawValue>>` — each element is a
                    //      byte slice, NOT a deep `Value`.
                    //   3. Borrow-extract `_id` + `_seq_no` per doc with a tiny
                    //      `IdSeq<'_>` helper (no String alloc on the id).
                    //   4. Concatenate surviving doc byte slices straight into
                    //      `merged_json_buf` — no Value round-trip.
                    //   5. Parse the surviving doc's source ONCE for FTS / DV
                    //      (these still need a Value), but we don't HOLD them.
                    //      The Value drops at the end of the inner loop body —
                    //      only `fts_input` and `dv_sources` (compact) survive.
                    //
                    // Net: ~5× lower RAM per batch and ~3× faster, allowing
                    // PARALLELISM to be raised back from 2 to 4 and forcemerge
                    // to run in minutes, not hours.
                    #[derive(serde::Deserialize)]
                    struct IdSeq<'a> {
                        #[serde(rename = "_id", borrow)]
                        id: &'a str,
                        #[serde(rename = "_seq_no")]
                        seq_no: u64,
                    }

                    let mut min_seq = u64::MAX;
                    let mut max_seq = 0u64;

                    let mut merged_json_buf: Vec<u8> = Vec::with_capacity(60 * 1024 * 1024);
                    merged_json_buf.push(b'[');
                    let mut first_doc = true;
                    let mut live_doc_count: u64 = 0;

                    // M5.22 — merge memory halving via Arc<Value> fan-out.
                    //
                    // Pre-M5.22 `run_merge_once` held BOTH `fts_input: Vec<(_,_,Value)>`
                    // and `dv_sources: Vec<Value>` — one full `Value` per doc
                    // in each, which meant every merged doc occupied ~10 KB
                    // twice (the Value tree is heap-allocated and `Value::Object`
                    // is a HashMap<String, Value> with its own String keys).
                    // For a 16-segment batch of ~30 k docs each = 480 k docs ×
                    // 2 × 10 KB = ~10 GB per merge batch.  M5.16 capped batches
                    // to 16 and parallelism to 1 to keep worst case at ~2.4 GB,
                    // but the 2× duplication still doubles the working set.
                    //
                    // M5.22: store Arc<Value> once in fts_input, iterate the
                    // same source via `fts_input.iter().map(|(_, _, v)| Some(v))`
                    // for the doc-values pass.  Drops `dv_sources` entirely.
                    // Halves merge working memory and frees the Arc<Value>
                    // immediately after both passes complete.
                    let mut fts_input: Vec<(String, HashMap<String, String>, Value)> = Vec::new();
                    // (seq_no, doc_id) for every surviving doc — kept exactly
                    // aligned with the stored-section byte copy (pushed right
                    // after `live_doc_count += 1`, BEFORE the per-doc Value
                    // parse that can `continue`), so the `.ids` side-car covers
                    // all stored docs even if a doc fails the FTS/DV re-parse.
                    let mut ids_pairs: Vec<(u64, String)> = Vec::new();

                    for meta in &metas_for_task {
                        min_seq = min_seq.min(meta.min_seq_no);
                        max_seq = max_seq.max(meta.max_seq_no);
                        let seg_path = segments_dir_for_task.join(&meta.seg_path);
                        let reader = match SegmentReader::open(&seg_path) {
                            Ok(r) => r,
                            Err(e) => {
                                tracing::warn!(?seg_path, "merge: failed to open segment: {e}");
                                continue;
                            }
                        };
                        let stored_bytes_raw = match reader.section(SectionType::Stored) {
                            Ok(Some(b)) => b,
                            _ => continue,
                        };
                        let stored_bytes =
                            match xerj_storage::stored_codec::decode_stored(stored_bytes_raw) {
                                Ok(b) => b,
                                Err(e) => {
                                    tracing::warn!("merge: failed to decode stored section: {e}");
                                    continue;
                                }
                            };
                        // `Box<RawValue>` uses a serde-private newtype tag that
                        // simd_json's serde adapter does not recognise — the
                        // deserialiser fails with "invalid type: newtype struct,
                        // expected any valid JSON value" on every segment, the
                        // `continue` below skips the entire segment, and merge
                        // silently loses every document in it.  Use serde_json
                        // here (which DOES handle RawValue correctly).  All other
                        // simd_json call sites in this file parse to `Vec<Value>`
                        // which doesn't trigger the RawValue private path.
                        //
                        // Bonus: serde_json takes `&[u8]` immutably so we don't
                        // need the `.to_vec()` the simd_json path required.
                        let raw_docs: Vec<Box<RawValue>> =
                            match serde_json::from_slice(&stored_bytes) {
                                Ok(d) => d,
                                Err(e) => {
                                    tracing::warn!(
                                        "merge: failed to parse stored as RawValue: {e}"
                                    );
                                    continue;
                                }
                            };

                        for raw in &raw_docs {
                            let raw_str = raw.get();
                            let id_seq: IdSeq = match serde_json::from_str(raw_str) {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                            if id_seq.id.is_empty() {
                                continue;
                            }
                            if let Some(entry) = store_for_task.version_map.get(id_seq.id) {
                                if entry.deleted {
                                    continue;
                                }
                                if entry.seq_no > id_seq.seq_no {
                                    continue;
                                }
                            }

                            // Survives — append raw bytes to the merged buffer.
                            if !first_doc {
                                merged_json_buf.push(b',');
                            }
                            first_doc = false;
                            merged_json_buf.extend_from_slice(raw_str.as_bytes());
                            live_doc_count += 1;
                            ids_pairs.push((id_seq.seq_no, id_seq.id.to_string()));

                            // Per-doc Value parse for FTS / DV builders.
                            let doc_value: Value = match serde_json::from_str(raw_str) {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                            let id_str = id_seq.id.to_string();
                            let source = doc_value.get("_source").cloned().unwrap_or(Value::Null);
                            let mut fields: HashMap<String, String> = HashMap::new();
                            if let Some(obj) = source.as_object() {
                                for (key, val) in obj {
                                    let text = extract_field_text(val);
                                    if !text.is_empty() {
                                        fields.insert(key.clone(), text);
                                    }
                                }
                            }
                            fts_input.push((id_str, fields, source));
                        }
                        // raw_docs + stored_bytes drop here — segment RAM reclaimed.
                    }

                    if live_doc_count == 0 {
                        return None;
                    }
                    merged_json_buf.push(b']');

                    // Write merged stored section using the columnar v2 codec.
                    let mut writer = match SegmentWriter::new(&segments_dir_for_task, 1, 0, 0) {
                        Ok(w) => w,
                        Err(e) => {
                            tracing::warn!("merge: failed to create writer: {e}");
                            return None;
                        }
                    };
                    let encoded = xerj_storage::stored_codec::encode_stored_v2(&merged_json_buf);
                    drop(merged_json_buf);
                    if let Err(e) = writer.add_section(SectionType::Stored, &encoded) {
                        tracing::warn!("merge: failed to add section: {e}");
                        return None;
                    }

                    let merged_meta = match writer.finish(live_doc_count, min_seq, max_seq) {
                        Ok(m) => m,
                        Err(e) => {
                            tracing::warn!("merge: failed to finish segment: {e}");
                            return None;
                        }
                    };

                    // Build FTS side-cars using the parallel per-field builder.
                    // We pre-built `fts_input` during the byte-copy pass above,
                    // so this is just one rayon `add_documents_parallel` call.
                    {
                        let mut fts_writer = FtsIndexWriter::new(
                            &segments_dir_for_task,
                            merged_meta.id.as_str(),
                            Arc::clone(&registry_for_task),
                        );
                        for (field_name, cfg) in &field_configs_for_task {
                            fts_writer.configure_field(field_name.clone(), cfg.clone());
                        }
                        if !fts_input.is_empty() {
                            // Dedicated merge pool: a merged-segment FTS build
                            // is the single longest rayon job in the system —
                            // on the global pool it queued every concurrent
                            // search's par_iter behind it (read-under-write
                            // collapse), and on the ingest pool it stalled
                            // every bulk request (see `crate::merge_pool`).
                            // Same-pool install here is a no-op re-entry —
                            // the whole batch already runs inside merge_pool.
                            crate::merge_pool().install(|| {
                                fts_writer.add_documents_parallel(&fts_input);
                                if let Err(e) = fts_writer.finish() {
                                    tracing::warn!("merge: FTS build failed: {e}");
                                }
                            });
                        }
                    }

                    // Update version_map so doc → segment_id points to the
                    // merged segment, using each doc's REAL seq_no from
                    // `ids_pairs` (collected during the byte-copy pass).
                    //
                    // CRITICAL correctness fix (2026-07): the previous code set
                    // EVERY surviving doc's entry to the segment's `max_seq`
                    // ("monotone upper bound").  That poisoned the version map:
                    // on the NEXT merge round the survivor filter compares
                    // `entry.seq_no > stored._seq_no` and, with entry.seq_no
                    // faked to max_seq, every doc except the one that actually
                    // owns max_seq looks stale and is dropped.  Any second-
                    // level merge (merged segment merged again — routine on a
                    // converging index) silently discarded ~all docs; live-
                    // verified as 200 k → 12 docs after one background merge
                    // cascade.  The loss was masked on restart only because
                    // merged-away input segments were never deleted from disk
                    // and `recover_orphaned_segments` resurrected them — a
                    // crutch removed by the merge-commit file deletion below.
                    //
                    // `set_if_latest` (>= guard) rather than unconditional
                    // `set`: a doc updated while the merge ran already has a
                    // newer entry that must not be clobbered by the merged
                    // (older) copy.
                    {
                        let merged_arc: Arc<str> = Arc::from(merged_meta.id.as_str());
                        for (seq_no, id) in &ids_pairs {
                            store_for_task.version_map.set_if_latest(
                                id.as_str(),
                                *seq_no,
                                Arc::clone(&merged_arc),
                                false,
                            );
                        }
                    }

                    // Doc-values side-car — reuse the same `Value`s we
                    // stashed in fts_input above (M5.22).
                    {
                        let columns =
                            build_doc_value_columns(fts_input.iter().map(|(_, _, v)| Some(v)));
                        if !columns.is_empty() {
                            if let Err(e) = write_doc_values_sidecar(
                                &segments_dir_for_task,
                                merged_meta.id.as_str(),
                                &columns,
                            ) {
                                tracing::warn!("merge: doc-values write failed: {e}");
                            }
                        }
                    }

                    Some((
                        batch_for_task,
                        merged_meta,
                        live_doc_count as usize,
                        ids_pairs,
                    ))
                })
            })
        };

        // Fill in_flight up to the cap, then drain one-by-one as they
        // complete, launching replacements from `queue_iter`.
        while in_flight.len() < merge_parallelism {
            match pending.take() {
                Some((batch, metas)) => {
                    in_flight.push(spawn_one(batch, metas));
                    pending = queue_iter.next();
                }
                None => break,
            }
        }
        while !in_flight.is_empty() {
            // Await the first handle in the queue.  tokio doesn't provide
            // a built-in "select any", but for small parallelism this FIFO
            // drain is fine — all spawn_blocking tasks run concurrently
            // anyway, we're just reaping their results in submission
            // order.
            let handle = in_flight.remove(0);
            match handle.await {
                Ok(Some((batch_slice, merged_meta, live_doc_count, ids_pairs))) => {
                    // Warm the merged segment's stored slices BEFORE the
                    // swap makes it visible: the first sorted-candidates
                    // queries after a merge otherwise each pay the fresh
                    // multi-100 MB stored-section decompress inside their
                    // own latency (dec=1.1-3.8 s p99 spikes at every merge
                    // completion under the mixed read/write bench).  On the
                    // blocking pool — the decompress is CPU-bound and must
                    // not stall the async merge driver.
                    {
                        let w_store = Arc::clone(&self.store);
                        let w_caches = self.publish_warm_caches();
                        let w_dir = self.data_dir.join("segments");
                        let w_id = merged_meta.id.clone();
                        let w_docs = merged_meta.doc_count;
                        let _ = tokio::task::spawn_blocking(move || {
                            warm_segment_at_publish(&w_store, &w_dir, &w_caches, &w_id, w_docs);
                        })
                        .await;
                    }
                    if let Err(e) = self.store.apply_merge(&batch_slice, merged_meta.clone()) {
                        tracing::warn!("merge: apply_merge failed: {e}");
                        // The pre-warmed slices belong to a segment that
                        // never became visible — release them.
                        if let Some((_, slices)) =
                            self.stored_slices_cache.remove(merged_meta.id.as_str())
                        {
                            self.stored_slices_cache_bytes
                                .fetch_sub(slices.retained_bytes(), Ordering::Relaxed);
                        }
                    } else {
                        // Write the merge output's `.ids` side-car AFTER the
                        // merge is committed: if we crashed before
                        // `apply_merge`, an output segment carrying a valid
                        // `.ids` would be resurrected by
                        // `recover_orphaned_segments` on restart alongside
                        // its still-live inputs (duplicate docs).  Written
                        // here, a crash in between merely loses the side-car
                        // and reopen falls back to the (slow but correct)
                        // decode-stored path — the pre-fix status quo.
                        {
                            let pairs: Vec<(u64, &str)> = ids_pairs
                                .iter()
                                .map(|(seq, id)| (*seq, id.as_str()))
                                .collect();
                            if let Err(e) = self
                                .store
                                .write_ids_sidecar(merged_meta.id.as_str(), &pairs)
                            {
                                tracing::warn!(
                                    merged_id = merged_meta.id.as_str(),
                                    "merge: failed to write .ids sidecar: {e}"
                                );
                            }
                        }
                        // Disk-space fix (2026-07): the input segments are
                        // now unreachable from the (persisted) snapshot —
                        // reclaim their files instead of leaving them for
                        // the next restart's orphan cleanup (~137 MB of
                        // dead segments after the 1 M-doc benchmark).
                        // Drop the cached SegmentReader mmaps first so the
                        // kernel can actually free the unlinked blocks once
                        // in-flight readers finish (in-flight queries that
                        // already hold an Arc<SegmentReader> keep working —
                        // unlink under a live mmap is safe on Linux).
                        for id in &batch_slice {
                            self.store.evict_segment_reader_cache(id.as_str());
                        }
                        // Merge-race fix (2026-07, follow-up to the disk
                        // fix): RETIRE, don't delete.  An immediate unlink
                        // raced in-flight searches that had snapshotted the
                        // pre-merge segment list but not yet opened every
                        // segment — the scan silently skipped the vanished
                        // segment and undercounted hits.total (observed
                        // live: 798,281 instead of 932,037).
                        // `retire_segment_files` deletes right away when no
                        // snapshot read lease is outstanding, otherwise it
                        // parks the ids in a graveyard swept by the last
                        // lease drop — so deferral is bounded by the
                        // longest-running in-flight query, not a restart.
                        let (rm_files, rm_bytes) = self.store.retire_segment_files(&batch_slice);
                        tracing::info!(
                            merged_id = merged_meta.id.as_str(),
                            inputs = batch_slice.len(),
                            live_docs = live_doc_count,
                            removed_files = rm_files,
                            removed_mb = rm_bytes / 1_000_000,
                            deferred = rm_files == 0,
                            "segment merge complete"
                        );
                        // These input segment ids are now unreachable —
                        // record them for precise per-segment cache
                        // eviction below.
                        dropped_seg_ids.extend(batch_slice.iter().cloned());
                        merged_batches += 1;
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("merge: spawn_blocking panicked: {e}"),
            }
            // Top up in_flight.
            if let Some((batch, metas)) = pending.take() {
                in_flight.push(spawn_one(batch, metas));
                pending = queue_iter.next();
            }
        }

        // If we applied any merges, bump the dataset version (invalidates
        // the query_cache) and evict ONLY the dropped segments' entries
        // from the per-segment dv_cache / stored_value_cache. P3.2: the
        // segments that survived the merge are immutable and their cached
        // doc-values / stored fields remain valid, so we no longer clear
        // the whole maps (which cold-started every post-merge read).
        if merged_batches > 0 {
            self.dataset_version.fetch_add(1, AtomicOrdering::Release);
            self.query_cache.clear();
            for id in &dropped_seg_ids {
                self.dv_cache.remove(id.as_str());
                self.id_pos_cache.remove(id.as_str());
                self.stored_value_cache.remove(id.as_str());
                if let Some((_, slices)) = self.stored_slices_cache.remove(id.as_str()) {
                    self.stored_slices_cache_bytes
                        .fetch_sub(slices.retained_bytes(), Ordering::Relaxed);
                }
                if let Some((_, bytes)) = self.decoded_stored_cache.remove(id.as_str()) {
                    self.decoded_stored_cache_bytes
                        .fetch_sub(bytes.len() as u64, Ordering::Relaxed);
                }
                self.fast_date_cache
                    .retain(|(seg, _), _| seg != id.as_str());
                self.fast_date_sorted_cache
                    .retain(|(seg, _), _| seg != id.as_str());
                // Sort-key shadows are keyed "{seg}\u{1}{field}" and hold
                // an O(doc_count) Vec each — without eviction every
                // merged-away segment leaked its shadows for the process
                // lifetime under a sustained writer.
                let shadow_prefix = format!("{}\u{1}", id.as_str());
                self.sort_shadow_cache
                    .retain(|k, _| !k.starts_with(&shadow_prefix));
                self.shortcut_count_cache
                    .retain(|(seg, _), _| seg != id.as_str());
                self.range_prefilter_cache
                    .retain(|k, _| !k.starts_with(&shadow_prefix));
                self.stored_slices_build_locks.remove(id.as_str());
            }
        }

        Ok(merged_batches)
    }

    /// Spawn a background tokio task that runs the merge pass every
    /// `interval` seconds until the index is dropped.
    ///
    /// Uses a Weak pointer so the task exits naturally when the last Arc to
    /// the index is released — no explicit shutdown plumbing needed for the
    /// happy path.  The returned `JoinHandle` is stored in `self.merge_task`
    /// so the SIGTERM path can call `abort_background_tasks` to break the
    /// 5-second `tokio::time::sleep` and let the runtime exit promptly
    /// (otherwise tokio waits for the sleep to wake before noticing the
    /// engine has been dropped — that is the bench-found shutdown hang).
    pub fn spawn_merge_task(self: &Arc<Self>, interval_secs: u64) {
        let weak = Arc::downgrade(self);
        // Allow override via env for benchmarks / battle tests so merges
        // don't interfere with ingest-rate measurements.
        let effective = std::env::var("XERJ_MERGE_INTERVAL_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(interval_secs);
        let interval = std::time::Duration::from_secs(effective.max(1));
        let handle = tokio::spawn(async move {
            tracing::info!(interval_secs, "merge background task started");
            loop {
                tokio::time::sleep(interval).await;
                let idx = match weak.upgrade() {
                    Some(a) => a,
                    None => {
                        tracing::info!("merge background task exiting (index dropped)");
                        return;
                    }
                };
                match idx.run_merge_once().await {
                    Ok(0) => {}
                    Ok(n) => tracing::debug!(batches = n, "merge pass ran"),
                    Err(e) => tracing::warn!("merge pass failed: {e}"),
                }
            }
        });
        // Replace any previous handle and abort it (defensive — should be
        // None at first construction; only matters if a caller ever calls
        // spawn_merge_task twice on the same Index).
        let mut slot = self.merge_task.lock();
        if let Some(prev) = slot.replace(handle) {
            prev.abort();
        }
    }

    /// Abort the merge background task spawned by `spawn_merge_task`.
    ///
    /// Idempotent and safe to call from a signal handler.  After this
    /// returns, the merge task is no longer holding the tokio runtime
    /// alive, so once axum's listeners stop and the final flush completes,
    /// the runtime exits cleanly instead of waiting up to 5 s for the
    /// merge sleep to wake on its own.  The aborted task itself is still
    /// being unwound, so ingest-side callers MUST NOT rely on the merge
    /// loop running after this.  Called from `Engine::flush_all_force`.
    pub fn abort_background_tasks(&self) {
        if let Some(handle) = self.merge_task.lock().take() {
            handle.abort();
        }
    }

    /// Update a document with upsert support.
    ///
    /// - If the document exists: merge `partial_doc` fields into existing source.
    /// - If the document does not exist and `upsert_doc` is `Some`: create it
    ///   using `upsert_doc` (then merge `partial_doc` on top if both provided).
    /// - If the document does not exist and `doc_as_upsert` is `true`: use
    ///   `partial_doc` itself as the creation body.
    /// - Otherwise (no doc, no upsert): return `None` (not found).
    pub async fn update_document_with_upsert(
        &self,
        id: &str,
        partial_doc: Option<Value>,
        upsert_doc: Option<Value>,
        doc_as_upsert: bool,
    ) -> Result<Option<IndexResponse>> {
        match self.get_document(id).await? {
            Some(existing) => {
                // Document exists — merge partial_doc on top.
                let merged = if let Some(patch) = partial_doc {
                    match &existing {
                        Value::Object(map) => {
                            let mut new_map = map.clone();
                            if let Some(patch_obj) = patch.as_object() {
                                for (k, v) in patch_obj {
                                    new_map.insert(k.clone(), v.clone());
                                }
                            }
                            Value::Object(new_map)
                        }
                        _ => {
                            if patch.is_object() {
                                patch.clone()
                            } else {
                                existing.clone()
                            }
                        }
                    }
                } else {
                    existing.clone()
                };
                // ES `update` returns `result: noop` when the merge would
                // not change the source. Detect by comparing the merged
                // object against the existing source.
                if merged == existing {
                    self.noop_update_count.fetch_add(1, Ordering::Relaxed);
                    return Ok(Some(IndexResponse {
                        id: id.to_string(),
                        seq_no: 0,
                        version: 1,
                        result: "noop".to_string(),
                    }));
                }
                let resp = self.index_document(Some(id.to_string()), merged).await?;
                Ok(Some(resp))
            }
            None => {
                // Document does not exist.
                if doc_as_upsert {
                    // Use partial_doc as the creation body.
                    let body = partial_doc.unwrap_or(Value::Object(serde_json::Map::new()));
                    let resp = self.index_document(Some(id.to_string()), body).await?;
                    Ok(Some(resp))
                } else if let Some(upsert_body) = upsert_doc {
                    // Create with upsert body, then merge partial_doc on top.
                    let body = if let Some(patch) = partial_doc {
                        match upsert_body {
                            Value::Object(mut map) => {
                                if let Some(patch_obj) = patch.as_object() {
                                    for (k, v) in patch_obj {
                                        map.insert(k.clone(), v.clone());
                                    }
                                }
                                Value::Object(map)
                            }
                            other => other,
                        }
                    } else {
                        upsert_body
                    };
                    let resp = self.index_document(Some(id.to_string()), body).await?;
                    Ok(Some(resp))
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// Scan a document for array-of-number fields and insert them into the HNSW index.
    async fn index_vectors(&self, doc_id: &str, source: &Value) {
        if let Some(obj) = source.as_object() {
            for (_field, val) in obj {
                if let Some(arr) = val.as_array() {
                    // Detect: all elements are numbers.
                    let all_numbers = !arr.is_empty() && arr.iter().all(|v| v.is_number());
                    if !all_numbers {
                        continue;
                    }

                    let vector: Vec<f32> = arr
                        .iter()
                        .filter_map(|v| v.as_f64().map(|f| f as f32))
                        .collect();

                    let dim = vector.len();
                    if dim == 0 {
                        continue;
                    }

                    // Ensure HNSW index exists with matching dim, or create it.
                    let node_id = self.hnsw_next_id.fetch_add(1, Ordering::Relaxed);
                    {
                        let mut hnsw_guard = self.hnsw.write().await;
                        if hnsw_guard.is_none() {
                            let params = HnswParams::new(dim, DistanceMetric::Cosine);
                            *hnsw_guard = Some(HnswIndex::new(params));
                        }
                        if let Some(ref hnsw) = *hnsw_guard {
                            if hnsw.params().dim != dim {
                                // Mismatched dimension — skip silently.
                                continue;
                            }
                            if let Err(e) = hnsw.insert(node_id, vector) {
                                warn!(doc_id, error = %e, "HNSW insert failed");
                                continue;
                            }
                        }
                    }

                    // Update id maps.
                    let mut id_map = self.hnsw_id_map.write().await;
                    let mut id_rev = self.hnsw_id_rev.write().await;
                    id_map.insert(doc_id.to_string(), node_id);
                    id_rev.insert(node_id, doc_id.to_string());
                    // Only use the first vector field found.
                    break;
                }
            }
        }
    }

    /// Perform KNN search over the HNSW index.
    ///
    /// Returns the top-`k` (doc_id, distance) pairs.
    pub async fn knn_search(&self, query_vector: &[f32], k: usize) -> Vec<(String, f32)> {
        let hnsw_guard = self.hnsw.read().await;
        let hnsw = match hnsw_guard.as_ref() {
            Some(h) => h,
            None => return vec![],
        };
        let ef = (k * 2).max(64);
        let results = match hnsw.search(query_vector, k, ef) {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "HNSW search failed");
                return vec![];
            }
        };
        let id_rev = self.hnsw_id_rev.read().await;
        results
            .into_iter()
            .filter_map(|(node_id, dist)| id_rev.get(&node_id).map(|doc_id| (doc_id.clone(), dist)))
            .collect()
    }

    /// Read the running counter of update operations that detected no
    /// change to the source. Surfaced via `indices.stats` as
    /// `indexing.noop_update_total`.
    pub fn noop_update_total(&self) -> u64 {
        self.noop_update_count.load(Ordering::Relaxed)
    }

    /// Brute-force exact KNN against every doc's stored source.
    ///
    /// We iterate (memtable ∪ segments.stored) once, extract the vector
    /// field, compute the configured similarity against the query
    /// vector, optionally apply a filter sub-query, and return the top-k
    /// by similarity as `Hit`s. `similarity` is one of
    /// `cosine | dot_product | max_inner_product | l2_norm` — matching
    /// Lucene's score conversion for each metric:
    ///
    /// * `cosine`:              `(1 + dot(a,b) / (|a|*|b|)) / 2`
    /// * `dot_product`:         `sigmoid(dot(a,b))` (≈ Lucene 9.x)
    /// * `max_inner_product`:   `if dot<0 { 1/(1-dot) } else { dot+1 }`
    /// * `l2_norm`:             `1 / (1 + sq_euclid(a,b))`
    ///
    /// Exact (brute force) results match the approximate HNSW path for
    /// small datasets (which is what the BBQ IVF YAML tests produce —
    /// ≤ a few hundred docs per index). Supersets of the tested
    /// behaviour (BBQ disk quantisation, IVF clustering) are not
    /// necessary for wire-correctness on these tests because the
    /// expected doc order is deterministic given exact scoring.
    pub async fn run_knn_brute_force(
        &self,
        request: &SearchRequest,
        field: &str,
        query_vec: &[f32],
        k: usize,
        filter: Option<Box<QueryNode>>,
        similarity: &str,
    ) -> Result<SearchResult> {
        let started = std::time::Instant::now();

        // ── Collect all candidate (doc_id, source) pairs ──────────────
        let mut candidates: Vec<(String, Value)> = Vec::new();
        // Memtable first (newest writes).
        {
            let mem = &*self.memtable;
            candidates.extend(mem.all_docs_with_sources());
        }
        // Then every flushed segment's stored section.
        let snap = self.store.snapshot();
        // Track seen IDs so later-segment copies don't duplicate memtable entries.
        let mut seen: HashSet<String> = candidates.iter().map(|(id, _)| id.clone()).collect();
        for meta in snap.segments.iter() {
            // Cache-backed: first KNN against this segment pays the
            // I/O + decompress + simd_json parse, every subsequent
            // query reads from `stored_value_cache`. For a 100-segment
            // index this turns repeated KNN from O(seg_count * 100MB)
            // copy work into Arc clone + iterate.
            let docs_arc = match self.stored_values_for(&meta.id) {
                Some(a) => a,
                None => continue,
            };
            // Cache-backed iteration: stored_values_for() handles the
            // I/O + decode + parse; subsequent KNN over the same segment
            // is an Arc clone. The underlying parse uses serde_json (not
            // simd_json) per ffd49ac — simd_json silently corrupts some
            // raw-bytes-flush payloads, the per-doc alloc cost is
            // irrelevant on the brute-force similarity scan.
            for doc in docs_arc.iter() {
                let id = match doc.get("_id").and_then(Value::as_str) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                if !seen.insert(id.clone()) {
                    continue;
                }
                // Skip tombstoned (deleted) docs.
                if self
                    .store
                    .version_map
                    .get(&id)
                    .map(|v| v.deleted)
                    .unwrap_or(false)
                {
                    continue;
                }
                // Reassembled segment docs have shape
                //   { "_id":..., "_seq_no":..., "_source": {...} }
                // but `get_field_value(src, "embedding")` further down
                // looks for the field at the top level (memtable path
                // shape). Unwrap to `_source` so either layout matches;
                // legacy pre-M7 segments without `_source` fall through
                // to the wrapper.
                let src = doc.get("_source").cloned().unwrap_or_else(|| {
                    let mut d = doc.clone();
                    if let Some(obj) = d.as_object_mut() {
                        obj.remove("_id");
                    }
                    d
                });
                candidates.push((id, src));
            }
        }

        // ── Determine whether this field opts into SQ8 (scalar8) ──────
        // Default fields keep the exact f32 brute-force scan below,
        // byte-identical to before. A `scalar8` field instead scores against
        // a per-field u8 code store — decoding each doc's SQ8 codes rather
        // than reading its f32 vector from `_source` for scoring.
        let use_sq8 = {
            let schema = self.schema.read().await;
            lookup_vector_quantization(&schema.schema, field).as_deref() == Some("scalar8")
        };

        // ── Score each candidate against the query vector ─────────────
        let mut scored: Vec<(String, f32, Value)> = Vec::with_capacity(candidates.len());
        if use_sq8 {
            // Cosine fields are L2-normalised before quantising so SQ8 fits
            // over bounded [-1,1] per-dim ranges (much better recall); cosine
            // is invariant to that scaling so ranking stays correct. Non-cosine
            // metrics must keep raw magnitudes, so they are quantised as-is.
            let normalize = !matches!(similarity, "l2_norm" | "dot_product" | "max_inner_product");
            let dim = query_vec.len();

            // Post-filter candidate vectors for this field: (id, src, doc_vec).
            let mut cand: Vec<(String, Value, Vec<f32>)> = Vec::with_capacity(candidates.len());
            for (id, src) in candidates {
                if let Some(ref f) = filter {
                    let mut src_with_id = src.clone();
                    if let Some(obj) = src_with_id.as_object_mut() {
                        obj.insert("_id".to_string(), Value::String(id.clone()));
                    }
                    if !doc_matches_query(f, &src_with_id) {
                        continue;
                    }
                }
                let vec_val = match get_field_value(&src, field) {
                    Some(v) => v,
                    None => continue,
                };
                let mut doc_vec: Vec<f32> = match &vec_val {
                    Value::Array(arr) => arr
                        .iter()
                        .filter_map(|v| v.as_f64().map(|f| f as f32))
                        .collect(),
                    _ => continue,
                };
                if doc_vec.len() != dim {
                    continue;
                }
                if normalize {
                    l2_normalize_vec(&mut doc_vec);
                }
                cand.push((id, src, doc_vec));
            }

            // Build/refresh the per-field SQ8 code store: fit params once from
            // the first ≤1000 vectors, then encode any doc not yet stored. The
            // store holds u8 codes (1 byte/dim), so its steady-state footprint
            // is ~4× smaller than the f32 vectors it replaces for scoring.
            {
                let mut stores = self.sq8_stores.write().await;
                let store = stores.entry(field.to_string()).or_insert_with(|| {
                    let sample: Vec<Vec<f32>> =
                        cand.iter().take(1000).map(|(_, _, v)| v.clone()).collect();
                    let params = Sq8Params::fit(&sample, dim);
                    Sq8FieldStore {
                        params,
                        dim,
                        normalize,
                        codes: HashMap::new(),
                    }
                });
                if store.dim == dim {
                    for (id, _src, v) in cand.iter() {
                        if !store.codes.contains_key(id) {
                            store.codes.insert(id.clone(), store.params.encode(v));
                        }
                    }
                    debug!(
                        field,
                        docs = store.codes.len(),
                        code_bytes = store.code_bytes(),
                        f32_bytes = store.codes.len() * store.dim * std::mem::size_of::<f32>(),
                        normalize = store.normalize,
                        "SQ8 code store refreshed"
                    );
                }
            }

            // Score by DECODING the stored SQ8 codes (never the raw f32).
            let stores = self.sq8_stores.read().await;
            if let Some(store) = stores.get(field) {
                let mut decoded = vec![0.0f32; store.dim];
                for (id, src, _v) in cand {
                    let codes = match store.codes.get(&id) {
                        Some(c) if c.len() == store.dim => c,
                        _ => continue,
                    };
                    store.params.decode_into(codes, &mut decoded);
                    let score = compute_vector_similarity(similarity, query_vec, &decoded);
                    scored.push((id, score, src));
                }
            }
        } else {
            // Per-chunk (passage) companion: multi-chunk `semantic_text` docs
            // persist their per-passage vectors under `<field>_chunks` at
            // ingest. When present we score by the BEST-matching passage
            // (max-sim) rather than the single pooled vector — a query that
            // matches one section of a long document ranks that document on
            // that section, not on the whole-doc average. Plain `dense_vector`
            // kNN and short single-chunk docs have no such companion, so they
            // fall through to the exact single-vector scan below (unchanged).
            let chunk_field = format!("{field}_chunks");
            for (id, src) in candidates {
                // Apply filter: if the filter doesn't match this doc, skip.
                if let Some(ref f) = filter {
                    let mut src_with_id = src.clone();
                    if let Some(obj) = src_with_id.as_object_mut() {
                        obj.insert("_id".to_string(), Value::String(id.clone()));
                    }
                    if !doc_matches_query(f, &src_with_id) {
                        continue;
                    }
                }
                let score = if let Some(Value::Array(chunks)) = get_field_value(&src, &chunk_field)
                {
                    // Best-matching passage over the stored chunk vectors.
                    let mut best: Option<f32> = None;
                    for cv in &chunks {
                        let dv: Vec<f32> = match cv {
                            Value::Array(a) => a
                                .iter()
                                .filter_map(|v| v.as_f64().map(|f| f as f32))
                                .collect(),
                            _ => continue,
                        };
                        if dv.len() != query_vec.len() {
                            continue;
                        }
                        let s = compute_vector_similarity(similarity, query_vec, &dv);
                        best = Some(best.map_or(s, |b| b.max(s)));
                    }
                    match best {
                        Some(s) => s,
                        None => continue,
                    }
                } else {
                    let vec_val = match get_field_value(&src, field) {
                        Some(v) => v,
                        None => continue,
                    };
                    let doc_vec: Vec<f32> = match &vec_val {
                        Value::Array(arr) => arr
                            .iter()
                            .filter_map(|v| v.as_f64().map(|f| f as f32))
                            .collect(),
                        _ => continue,
                    };
                    if doc_vec.len() != query_vec.len() {
                        continue;
                    }
                    compute_vector_similarity(similarity, query_vec, &doc_vec)
                };
                scored.push((id, score, src));
            }
        }

        // ── Rank, cap the candidate pool at k, then paginate ──────────
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        // `k` bounds the kNN candidate pool; `from`/`size` then window into
        // it, exactly like an ES top-level knn (returned hits are the
        // size-slice of the top-k, NOT k itself). Without this, a
        // `{"knn"/"semantic": {k: 5}}` with `"size": 3` returned 5 hits.
        // `size == 0` is a count-only request (total only, no hits).
        scored.truncate(k.max(1));
        // ES reports `hits.total.value` for a knn/semantic query as the size
        // of the retrieved neighbor pool (min(k, matches)), NOT the number of
        // docs that merely have a vector — so compute it AFTER the truncate.
        // (Hybrid/RRF ignores this total and recomputes its own from the fused
        // list, so bounding it here is safe for that path.)
        let total_value = scored.len() as u64;
        let hits: Vec<Hit> = if request.size == 0 {
            Vec::new()
        } else {
            scored
                .into_iter()
                .skip(request.from)
                .take(request.size)
                .collect::<Vec<_>>()
        }
        .into_iter()
        .map(|(id, score, source)| Hit {
            id,
            score,
            source,
            sort: Vec::new(),
            explain: None,
            highlight: None,
            matched_queries: Vec::new(),
        })
        .collect();

        Ok(SearchResult {
            hits,
            total: TotalHits {
                value: total_value,
                relation: TotalHitsRelation::Eq,
            },
            took_ms: started.elapsed().as_millis() as u64,
            aggs: None,
            timed_out: false,
            profile: None,
            max_score: None,
        })
    }

    /// Brute-force nested KNN: score each parent by the best (max)
    /// similarity among its `<nested_path>` array elements.
    ///
    /// Semantic search executor. Embeds the natural-language query
    /// text via the configured `EmbeddingProxy`, then runs filtered
    /// kNN against the same field.
    ///
    /// Failure modes:
    ///  * No proxy configured → 400 with a clear config hint.
    ///  * Proxy returns an empty / mismatched-dim vector → 400.
    ///  * Proxy network / 5xx error → propagate the proxy's error.
    ///
    /// We deliberately do NOT silently fall back to BM25 on embed
    /// failure — the semantic query type is opt-in by the caller and
    /// quietly degrading would mask deployment bugs (missing API
    /// key, throttled key, model name typo). Operators wanting that
    /// behaviour can wrap the call in a `dis_max` of `semantic` +
    /// `match` clauses.
    pub async fn run_semantic(
        &self,
        request: &SearchRequest,
        field: &str,
        text: &str,
        k: usize,
        filter: Option<Box<QueryNode>>,
    ) -> Result<SearchResult> {
        // Resolve whether `field` is a `semantic_text` field (has an embedding
        // config). If so, the vector lives in the companion `target_field` and
        // was produced by the same embedder used at ingest — so we must embed
        // the query with that same embedder and kNN against `target_field`.
        let (knn_field, dims, is_semantic_text, similarity) = {
            let schema = self.schema.read().await;
            match schema.schema.field(field) {
                Some(fc) if fc.embedding.is_some() => {
                    let emb = fc.embedding.as_ref().unwrap();
                    let target = emb
                        .target_field
                        .clone()
                        .unwrap_or_else(|| format!("{field}_vector"));
                    let dims = fc
                        .options
                        .dimensions
                        .unwrap_or(xerj_ai::local::DEFAULT_DIMS);
                    let sim = fc
                        .options
                        .similarity
                        .clone()
                        .unwrap_or_else(|| "cosine".to_string());
                    (target, dims, true, sim)
                }
                _ => (
                    field.to_string(),
                    xerj_ai::local::DEFAULT_DIMS,
                    false,
                    lookup_vector_similarity(&schema.schema, field),
                ),
            }
        };

        // Embed the query text with the effective embedder:
        //   * EmbeddingProxy when configured (high-quality, same as ingest),
        //   * else the built-in deterministic embedder — but ONLY for
        //     `semantic_text` fields, which were embedded that same way at
        //     ingest. A `semantic` query against a plain dense_vector field
        //     with no proxy configured still returns the original 400 (there
        //     is no comparable stored vector to match against).
        let query_vec = if let Some(proxy) = &*self.embedding_proxy {
            // `embed_batch` takes Vec<String>; we only have one text but the
            // proxy's batching keeps the wire format stable for callers.
            let mut vectors = proxy
                .embed_batch(vec![text.to_string()])
                .await
                .map_err(|e| {
                    EngineError::Common(xerj_common::XerjError::invalid_query(format!(
                        "semantic embed failed: {e}"
                    )))
                })?;
            match vectors.pop() {
                Some(v) if !v.is_empty() => v,
                _ => {
                    return Err(EngineError::Common(xerj_common::XerjError::invalid_query(
                        "embedding proxy returned no vector for semantic query",
                    )));
                }
            }
        } else if is_semantic_text {
            xerj_ai::local::local_embed(text, dims)
        } else {
            return Err(EngineError::Common(xerj_common::XerjError::invalid_query(
                "semantic query requires either a `semantic_text` field (auto-embedded \
                     with the built-in embedder) or `embedding.default_endpoint` configured \
                     to an OpenAI-compatible /v1/embeddings endpoint.",
            )));
        };

        self.run_knn_brute_force(request, &knn_field, &query_vec, k, filter, &similarity)
            .await
    }

    /// Auto-embed `semantic_text` fields on ingest.
    ///
    /// For each schema field carrying an [`EmbeddingConfig`], reads the field's
    /// text from `source` and writes an embedding into the config's
    /// `target_field` (default `<field>_vector`). Uses the configured
    /// [`EmbeddingProxy`] when present, else the zero-config built-in
    /// deterministic embedder — the *same* choice made at query time so the
    /// vectors are comparable. Fields where the target already holds a value
    /// (caller pre-embedded) or whose value is not a string are left untouched.
    async fn apply_semantic_embeddings(&self, mut source: Value) -> Result<Value> {
        // Collect (field, target_field, dims) specs without holding the schema
        // lock across the (possibly async) embedding calls.
        let specs: Vec<(String, String, usize)> = {
            let schema = self.schema.read().await;
            schema
                .schema
                .fields
                .iter()
                .filter_map(|fc| {
                    fc.embedding.as_ref().map(|emb| {
                        let target = emb
                            .target_field
                            .clone()
                            .unwrap_or_else(|| format!("{}_vector", fc.name));
                        let dims = fc
                            .options
                            .dimensions
                            .unwrap_or(xerj_ai::local::DEFAULT_DIMS);
                        (fc.name.clone(), target, dims)
                    })
                })
                .collect()
        };
        if specs.is_empty() {
            return Ok(source);
        }
        let Some(obj) = source.as_object_mut() else {
            return Ok(source);
        };
        for (field, target, dims) in specs {
            // Respect a caller-supplied vector (e.g. pre-computed offline).
            if obj.get(&target).map(|v| !v.is_null()).unwrap_or(false) {
                continue;
            }
            let Some(text) = obj.get(&field).and_then(Value::as_str) else {
                continue;
            };
            let text = text.to_string();
            // Chunk once and embed EACH overlapping chunk. `chunk_vecs` is one
            // embedding per passage (>= 1). Short text (<= chunk_size) yields a
            // single chunk == the exact pre-chunking behavior.
            let chunk_vecs: Vec<Vec<f32>> = if let Some(proxy) = &*self.embedding_proxy {
                let chunks = semantic_chunker().chunk(&text, None);
                let chunk_texts: Vec<String> = if chunks.len() <= 1 {
                    vec![text.clone()]
                } else {
                    chunks.iter().map(|c| c.text.clone()).collect()
                };
                let vs = proxy.embed_batch(chunk_texts).await.map_err(|e| {
                    EngineError::Common(xerj_common::XerjError::invalid_query(format!(
                        "semantic_text embed failed for field [{field}]: {e}"
                    )))
                })?;
                vs.into_iter().filter(|v| !v.is_empty()).collect()
            } else {
                local_chunk_vectors(&text, dims)
            };
            // Pooled vector for `target`: single chunk is stored as-is (no
            // re-normalization, matching pre-chunking behavior); multiple
            // chunks are L2-normalized, mean-pooled, then re-normalized. This
            // keeps `target` back-compatible for plain kNN and for callers
            // that read the companion vector directly.
            let pooled = match chunk_vecs.len() {
                0 => continue,
                1 => chunk_vecs[0].clone(),
                _ => mean_pool_normalize(&chunk_vecs),
            };
            let arr: Vec<Value> = pooled.into_iter().map(|f| Value::from(f as f64)).collect();
            obj.insert(target.clone(), Value::Array(arr));
            // Per-chunk passage vectors — persisted ONLY when the document
            // actually spans more than one chunk, under `<target>_chunks` as an
            // array of vectors. Semantic search prefers these and scores by the
            // best-matching passage (max-sim) instead of the blurred pooled
            // average. Single-chunk docs store nothing extra, so their `_source`
            // and single-vector scoring are byte-identical to before.
            if chunk_vecs.len() > 1 {
                let chunks_json: Vec<Value> = chunk_vecs
                    .iter()
                    .map(|cv| Value::Array(cv.iter().map(|f| Value::from(*f as f64)).collect()))
                    .collect();
                obj.insert(format!("{target}_chunks"), Value::Array(chunks_json));
            }
        }
        Ok(source)
    }

    /// Hybrid (multi-query + fusion) executor. Recursively runs each
    /// sub-query as its own search and combines the result lists via
    /// the chosen fusion strategy (RRF / Linear / Learned). Pagination
    /// is applied to the fused list, not to the sub-lists — each
    /// sub-list is fetched at `top_k = max(50, request.size + request.from)`
    /// to give the fusion enough material to produce stable top-N.
    ///
    /// Aggregations are NOT supported in hybrid queries today (the
    /// natural semantics — agg over the fused result set vs over each
    /// sub-query — is application-specific). If the request asks for
    /// aggs we reject it with an explicit 400 rather than silently
    /// dropping them; a bounded aggregation over the fused top-N would
    /// present under-counted `doc_count`s as if they were exact. Run
    /// the aggregation as a separate, non-hybrid search instead. The
    /// v0.7-P1 scope is hits + scores only.
    pub async fn run_hybrid(
        &self,
        request: &SearchRequest,
        sub_queries: Vec<xerj_query::ast::WeightedQuery>,
        fusion: xerj_query::ast::FusionStrategy,
    ) -> Result<SearchResult> {
        let started = std::time::Instant::now();

        // Aggregations over a fused (RRF/Linear/Learned) result set have
        // no well-defined exact semantics — a bounded agg over the fused
        // top-N would silently under-count. Fail loud (400) instead of
        // returning `aggs: null`, mirroring `result_window_too_large`.
        if request.aggs.is_some() {
            return Err(EngineError::Common(xerj_common::XerjError::invalid_query(
                "aggregations are not supported with hybrid/fusion queries; \
                 run the aggregation as a separate non-hybrid search",
            )));
        }

        // Each sub-list needs to be deeper than the final top-N so
        // fusion has enough material for stable rank-merge. ES uses
        // 50 by default; we mirror that, growing if the caller asked
        // for a deeper page.
        let per_query_topk = request.size.saturating_add(request.from).max(50);

        // Run each sub-query sequentially. (Parallel `join_all` is the
        // obvious next step but means cloning Self into each future;
        // sequential is correct, easy to reason about, and good enough
        // for v0.7-P1 — the per-query latency is dominated by the kNN
        // / FTS scan, not the await ordering.)
        let mut sub_results: Vec<(Vec<Hit>, f32)> = Vec::with_capacity(sub_queries.len());
        for wq in sub_queries {
            let sub_request = SearchRequest {
                query: wq.query.clone(),
                from: 0,
                size: per_query_topk,
                sort: Vec::new(),
                search_after: None,
                source: request.source.clone(),
                aggs: None,
                track_total_hits: request.track_total_hits,
                explain: false,
                highlight: None,
                collapse: None,
                rescore: Vec::new(),
                min_score: None,
                timeout_ms: request.timeout_ms,
                script_fields: None,
                fields: Vec::new(),
                profile: false,
            };
            // Box::pin to break the type-recursion (search_inner ↔
            // run_hybrid both async fn).
            let sub_result = Box::pin(self.search_inner(&sub_request)).await?;
            sub_results.push((sub_result.hits, wq.weight));
        }

        // Apply fusion.
        let fused = match fusion {
            xerj_query::ast::FusionStrategy::Rrf { k } => fuse_rrf(&sub_results, k),
            xerj_query::ast::FusionStrategy::Linear => fuse_linear(&sub_results),
            // Defense in depth: the parser already rejects `fusion: learned`
            // with a 400 (see xerj-query parser.rs::parse_hybrid), so this
            // arm is unreachable via the ES API. Fail loud rather than
            // silently substituting RRF, in case a future non-parser caller
            // constructs the AST directly.
            xerj_query::ast::FusionStrategy::Learned => {
                return Err(EngineError::Common(xerj_common::XerjError::invalid_query(
                    "hybrid fusion learned is not yet supported; use rrf or linear",
                )));
            }
        };

        // Apply pagination on the fused list.
        let total_value = fused.len() as u64;
        let page: Vec<Hit> = fused
            .into_iter()
            .skip(request.from)
            .take(request.size)
            .collect();

        let took_ms = started.elapsed().as_millis() as u64;
        Ok(SearchResult {
            hits: page,
            total: TotalHits {
                value: total_value,
                relation: TotalHitsRelation::Eq,
            },
            took_ms,
            aggs: None,
            timed_out: false,
            profile: None,
            max_score: None,
        })
    }

    /// `field` is the full dotted path as the ES query provides it
    /// (e.g. `nested.vector`). The element inside the nested array is
    /// the sub-field after the path prefix (`vector`).
    ///
    /// Parents whose nested array is empty or whose elements have a
    /// vector of the wrong dim are skipped. `extra_filter` is applied
    /// at parent-doc granularity.
    #[allow(clippy::too_many_arguments)] // ES nested-kNN request surface; 1:1 with query DSL
    pub async fn run_nested_knn_brute_force(
        &self,
        request: &SearchRequest,
        nested_path: &str,
        field: &str,
        query_vec: &[f32],
        k: usize,
        num_candidates: usize,
        pre_filter: Option<Box<QueryNode>>,
        post_filter: Option<Box<QueryNode>>,
        similarity: &str,
    ) -> Result<SearchResult> {
        let started = std::time::Instant::now();
        // The sub-field name inside each nested element.
        let subfield = field
            .strip_prefix(&format!("{}.", nested_path))
            .unwrap_or(field)
            .to_string();

        // ── Collect candidate parent docs ─────────────────────────────
        let mut candidates: Vec<(String, Value)> = Vec::new();
        {
            let mem = &*self.memtable;
            candidates.extend(mem.all_docs_with_sources());
        }
        let snap = self.store.snapshot();
        let mut seen: HashSet<String> = candidates.iter().map(|(id, _)| id.clone()).collect();
        for meta in snap.segments.iter() {
            // Cache-backed: see comment on the matching loop in
            // run_knn_brute_force.
            let docs_arc = match self.stored_values_for(&meta.id) {
                Some(a) => a,
                None => continue,
            };
            // Cache + serde_json under the hood (see stored_values_for).
            for doc in docs_arc.iter() {
                let id = match doc.get("_id").and_then(Value::as_str) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                if !seen.insert(id.clone()) {
                    continue;
                }
                if self
                    .store
                    .version_map
                    .get(&id)
                    .map(|v| v.deleted)
                    .unwrap_or(false)
                {
                    continue;
                }
                let mut doc = doc.clone();
                if let Some(obj) = doc.as_object_mut() {
                    obj.remove("_id");
                }
                candidates.push((id, doc));
            }
        }

        // ── Score each parent by best-matching nested element ─────────
        // Pre-filter is applied before scoring (alias filter, knn.filter).
        let mut scored: Vec<(String, f32, Value)> = Vec::new();
        for (id, src) in candidates {
            if let Some(ref f) = pre_filter {
                let mut src_with_id = src.clone();
                if let Some(obj) = src_with_id.as_object_mut() {
                    obj.insert("_id".to_string(), Value::String(id.clone()));
                }
                if !doc_matches_query(f, &src_with_id) {
                    continue;
                }
            }
            let nested_arr = match get_field_value(&src, nested_path) {
                Some(Value::Array(a)) => a,
                Some(single) => vec![single],
                None => continue,
            };
            let mut best: Option<f32> = None;
            for elem in &nested_arr {
                let vec_val = match elem.get(&subfield) {
                    Some(v) => v,
                    None => continue,
                };
                let doc_vec: Vec<f32> = match vec_val {
                    Value::Array(arr) => arr
                        .iter()
                        .filter_map(|v| v.as_f64().map(|f| f as f32))
                        .collect(),
                    _ => continue,
                };
                if doc_vec.len() != query_vec.len() {
                    continue;
                }
                let s = compute_vector_similarity(similarity, query_vec, &doc_vec);
                best = Some(match best {
                    Some(b) => b.max(s),
                    None => s,
                });
            }
            if let Some(score) = best {
                scored.push((id, score, src));
            }
        }

        // Truncate to num_candidates BEFORE post-filtering — this is
        // what makes "post-filter with num_candidates:1" return 0 when
        // the single top candidate doesn't match the outer filter.
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(num_candidates);

        // Apply post-filter (sibling bool.must clauses).
        if let Some(ref f) = post_filter {
            scored.retain(|(id, _, src)| {
                let mut src_with_id = src.clone();
                if let Some(obj) = src_with_id.as_object_mut() {
                    obj.insert("_id".to_string(), Value::String(id.clone()));
                }
                doc_matches_query(f, &src_with_id)
            });
        }

        let total_value = scored.len() as u64;
        let k_final = k.max(request.size).max(1);
        scored.truncate(k_final);

        let hits: Vec<Hit> = scored
            .into_iter()
            .map(|(id, score, source)| Hit {
                id,
                score,
                source,
                sort: Vec::new(),
                explain: None,
                highlight: None,
                matched_queries: Vec::new(),
            })
            .collect();

        Ok(SearchResult {
            hits,
            total: TotalHits {
                value: total_value,
                relation: TotalHitsRelation::Eq,
            },
            took_ms: started.elapsed().as_millis() as u64,
            aggs: None,
            timed_out: false,
            profile: None,
            max_score: None,
        })
    }

    /// Current max seq_no across this index's storage. Used to snapshot
    /// visibility at `open_point_in_time` time — docs with `_seq_no`
    /// greater than the snapshot are considered unborn relative to
    /// the PIT and filtered out at search time.
    pub fn current_seq_no(&self) -> u64 {
        self.store.current_seq_no()
    }

    // ── HNSW persistence ─────────────────────────────────────────────────
    //
    // Pre-v0.6.2 the HNSW graph lived only in memory; on every restart
    // we re-inserted every vector by walking the WAL — O(N log N)
    // startup cost on million-vector indices. v0.6.2 ships save/load
    // for the graph (xerj-vector::HnswIndex::save_to/load_from) and
    // a tiny sidecar for the doc_id ↔ node_id map.
    //
    // Layout under `<index_data_dir>/hnsw/`:
    //   graph.bin    HNSW graph (xerj-vector format, CRC32C-validated)
    //   ids.json     { next_id, map: { "doc_id": node_id, ... } }
    //
    // Caller invariants:
    //   * `save_hnsw_to_disk` writes both files atomically (via .tmp +
    //     rename). A crash leaves either both old files or no files;
    //     in the latter case startup falls back to WAL replay.
    //   * `load_hnsw_from_disk` is best-effort. Any error returns Ok(())
    //     and leaves `self.hnsw = None`; the next vector ingest re-
    //     creates the graph.

    fn hnsw_dir(&self) -> std::path::PathBuf {
        self.data_dir.join("hnsw")
    }

    fn hnsw_graph_path(&self) -> std::path::PathBuf {
        self.hnsw_dir().join("graph.bin")
    }

    fn hnsw_ids_path(&self) -> std::path::PathBuf {
        self.hnsw_dir().join("ids.json")
    }

    /// Persist the in-memory HNSW graph + id-map to disk. No-op when
    /// `self.hnsw` is None (no vector field has been indexed yet).
    /// Called on flush completion and graceful shutdown.
    pub async fn save_hnsw_to_disk(&self) -> Result<()> {
        let hnsw_guard = self.hnsw.read().await;
        let hnsw = match &*hnsw_guard {
            Some(h) => h,
            None => return Ok(()),
        };
        let dir = self.hnsw_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!(error = %e, "HNSW save: cannot create dir");
            return Ok(());
        }
        if let Err(e) = hnsw.save_to(&self.hnsw_graph_path()) {
            warn!(error = %e, "HNSW save: graph serialization failed");
            return Ok(());
        }
        // Save the id map alongside. JSON is fine — the map is at most
        // a few hundred MB even for the largest indices and writes
        // infrequently (once per flush). The shape is fixed so a v0.7
        // codec swap (e.g. to a binary keyed by doc_id length) won't
        // break loaders that read both fields.
        let id_map = self.hnsw_id_map.read().await;
        let snapshot = serde_json::json!({
            "next_id": self.hnsw_next_id.load(Ordering::Relaxed),
            "map": *id_map,
        });
        let tmp = self.hnsw_ids_path().with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp, serde_json::to_vec(&snapshot).unwrap_or_default()) {
            warn!(error = %e, "HNSW save: ids.json write failed");
            return Ok(());
        }
        if let Err(e) = std::fs::rename(&tmp, self.hnsw_ids_path()) {
            warn!(error = %e, "HNSW save: ids.json rename failed");
            return Ok(());
        }
        debug!(
            graph = %self.hnsw_graph_path().display(),
            nodes = hnsw.len(),
            "HNSW persisted"
        );
        Ok(())
    }

    /// Try to load a previously-persisted HNSW graph + id-map.
    /// Returns `Ok(true)` on a successful load (graph and ids both
    /// present and self-consistent), `Ok(false)` if no snapshot
    /// exists or load failed (caller falls back to WAL replay).
    pub async fn load_hnsw_from_disk(&self) -> Result<bool> {
        let loaded = match load_hnsw_artifacts_sync(&self.hnsw_dir()) {
            Some(l) => l,
            None => return Ok(false),
        };
        let mut id_map = self.hnsw_id_map.write().await;
        let mut id_rev = self.hnsw_id_rev.write().await;
        *id_map = loaded.id_map;
        *id_rev = loaded.id_rev;
        drop(id_map);
        drop(id_rev);
        self.hnsw_next_id.store(loaded.next_id, Ordering::Relaxed);
        let nodes = loaded.graph.len();
        *self.hnsw.write().await = Some(loaded.graph);
        info!(
            nodes,
            "HNSW reloaded from disk — skipping WAL replay rebuild"
        );
        Ok(true)
    }

    /// Re-validate every section's CRC32C across every segment in
    /// this index's current snapshot. Returns a structured report
    /// with `corrupt_sections > 0` indicating on-disk bit rot.
    ///
    /// Whole-file CRC is checked once at segment open; per-section
    /// CRC is normally skipped on the search hot path for perf
    /// (see `xerj_storage::SegmentReader::section` docs). This
    /// method goes back over every section to detect rot at rest.
    /// Cost is O(total stored bytes) — runs in seconds for typical
    /// indexes, minutes for hundreds of GB. Designed for periodic
    /// scheduling, not per-request use.
    pub fn fsck_segments(&self) -> xerj_storage::FsckReport {
        self.store.fsck_segments()
    }

    /// Track a request_cache-eligible search. Increments hit_count when
    /// the same query hash has been seen before, miss_count on first
    /// sight. Caller decides whether the search is cache-eligible
    /// (request_cache=true + size:0 + no aggs that mutate the result).
    pub fn track_request_cache(&self, body_hash: u64) {
        // RwLock here is tokio's async one; block_in_place isn't available
        // from a non-async caller. Use std::sync::Mutex semantics via
        // try_write spin since this is a small hot path — the dataset is
        // bounded so contention is negligible.
        let mut seen = loop {
            match self.request_cache_seen.try_write() {
                Ok(g) => break g,
                Err(_) => std::thread::yield_now(),
            }
        };
        if seen.record(body_hash) {
            self.request_cache_misses.fetch_add(1, Ordering::Relaxed);
        } else {
            self.request_cache_hits.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn request_cache_hit_count(&self) -> u64 {
        self.request_cache_hits.load(Ordering::Relaxed)
    }

    pub fn request_cache_miss_count(&self) -> u64 {
        self.request_cache_misses.load(Ordering::Relaxed)
    }

    /// Look up the latest `seq_no` for a document by id via the version
    /// map. Returns `None` when the doc is unknown or tombstoned.
    ///
    /// The internal WAL counter starts at 1 so the first WAL op has seq=1,
    /// but ES semantics number seq_no from 0 per shard. We subtract 1 to
    /// match the ES wire format.
    pub fn lookup_seq_no(&self, id: &str) -> Option<u64> {
        self.store.version_map.get(id).and_then(|entry| {
            if entry.deleted {
                None
            } else {
                Some(entry.seq_no.saturating_sub(1))
            }
        })
    }

    /// Retrieve a document by its string ID.
    ///
    /// Checks the memtable first, then searches on-disk segments.
    pub async fn get_document(&self, id: &str) -> Result<Option<Value>> {
        // Count every GET, regardless of the result, so indices.stats
        // reflects the true ES `get.total` counter (which includes
        // missing + exists).
        let get_started = std::time::Instant::now();
        self.metric_get_count.fetch_add(1, Ordering::Relaxed);
        // Check version map — if the document was deleted, return None.
        if let Some(entry) = self.store.version_map.get(id) {
            if entry.deleted {
                self.metric_get_missing_count
                    .fetch_add(1, Ordering::Relaxed);
                self.metric_get_total_ms
                    .fetch_add(get_started.elapsed().as_millis() as u64, Ordering::Relaxed);
                return Ok(None);
            }
        } else {
            // Document not in version map at all.
            self.metric_get_missing_count
                .fetch_add(1, Ordering::Relaxed);
            self.metric_get_total_ms
                .fetch_add(get_started.elapsed().as_millis() as u64, Ordering::Relaxed);
            return Ok(None);
        }

        // Check the memtable first — documents live here until a flush.
        {
            let mem = &*self.memtable;
            if mem.contains(id) {
                self.metric_get_exists_count.fetch_add(1, Ordering::Relaxed);
                self.metric_get_total_ms
                    .fetch_add(get_started.elapsed().as_millis() as u64, Ordering::Relaxed);
                return Ok(mem.get_doc_source_as_value(id));
            }
        }

        // Search segments for the stored document.
        let snap = self.store.snapshot();
        for meta in snap.segments.iter().rev() {
            // Check version map: is this doc in this segment?
            if let Some(ver) = self.store.version_map.get(id) {
                if &*ver.segment_id != meta.id.as_str() {
                    continue; // not this segment
                }
            }

            if let Ok(reader) = self.store.open_segment(&meta.id) {
                if let Ok(Some(stored_bytes_raw)) = reader.section(SectionType::Stored) {
                    let stored_bytes =
                        match xerj_storage::stored_codec::decode_stored(stored_bytes_raw) {
                            Ok(b) => b,
                            Err(_) => continue,
                        };
                    // serde_json (not simd_json): same root cause as the
                    // KNN cache — see ffd49ac. simd_json silently corrupts
                    // some stored payloads from the M7 raw-bytes flush path.
                    if let Ok(docs) = serde_json::from_slice::<Vec<Value>>(&stored_bytes) {
                        for doc in docs {
                            if doc.get("_id").and_then(Value::as_str) == Some(id) {
                                self.metric_get_exists_count.fetch_add(1, Ordering::Relaxed);
                                self.metric_get_total_ms.fetch_add(
                                    get_started.elapsed().as_millis() as u64,
                                    Ordering::Relaxed,
                                );
                                return Ok(doc.get("_source").cloned());
                            }
                        }
                    }
                }
            }
        }
        drop(snap);

        self.metric_get_missing_count
            .fetch_add(1, Ordering::Relaxed);
        self.metric_get_total_ms
            .fetch_add(get_started.elapsed().as_millis() as u64, Ordering::Relaxed);
        Ok(None)
    }

    /// Delete a document by ID.
    pub async fn delete_document(&self, id: &str) -> Result<bool> {
        // Check write block.
        if self.is_write_blocked().await {
            return Err(EngineError::Common(xerj_common::XerjError::index_blocked(
                self.name.as_str(),
                "write",
            )));
        }
        self.dataset_version.fetch_add(1, Ordering::Release);

        // Check whether the document actually exists and is not already deleted.
        let is_live = self
            .store
            .version_map
            .get(id)
            .map(|entry| !entry.deleted)
            .unwrap_or(false);

        // Also check the memtable (newly indexed docs may not yet be in the
        // version map or may have been put there after the version_map check).
        let in_memtable = {
            let mem = &*self.memtable;
            mem.contains(id)
        };

        let should_delete = is_live || in_memtable;

        if !should_delete {
            return Ok(false);
        }

        let existed = self.store.delete(id)?.is_some();

        // Remove from memtable.
        {
            let mem = &*self.memtable;
            mem.remove(id);
        }

        // v0.6.2 — propagate the delete into the HNSW graph. Pre-fix
        // a deleted doc was unfindable via _get / _search but the
        // vector still showed up in kNN results forever (the node
        // and its neighbour edges stayed in the graph). mark_deleted
        // tombstones the node so kNN skips it; the edges remain
        // until a graph compaction (planned v0.7+).
        let node_id_to_tomb = {
            let id_map = self.hnsw_id_map.read().await;
            id_map.get(id).copied()
        };
        if let Some(node_id) = node_id_to_tomb {
            let hnsw_guard = self.hnsw.read().await;
            if let Some(ref h) = *hnsw_guard {
                h.mark_deleted(node_id);
            }
            drop(hnsw_guard);
            // Remove from id maps so a re-index of the same doc_id
            // gets a fresh node_id (the old one stays tombstoned).
            self.hnsw_id_map.write().await.remove(id);
            self.hnsw_id_rev.write().await.remove(&node_id);
        }

        if existed {
            self.doc_count.fetch_sub(1, Ordering::Relaxed);
        }
        Ok(existed || in_memtable)
    }

    /// Update a document by merging a partial document into the existing source.
    ///
    /// Fetches the current document, merges the provided `partial_doc` fields
    /// (top-level keys) into the existing source, then re-indexes the merged result.
    /// Returns `None` if the document does not exist.
    pub async fn update_document(
        &self,
        id: &str,
        partial_doc: Value,
    ) -> Result<Option<IndexResponse>> {
        // Fetch existing document source.
        let existing = match self.get_document(id).await? {
            Some(src) => src,
            None => return Ok(None),
        };

        // Merge: start from existing, overlay partial_doc fields.
        let merged = match existing {
            Value::Object(mut map) => {
                if let Some(partial_obj) = partial_doc.as_object() {
                    for (k, v) in partial_obj {
                        map.insert(k.clone(), v.clone());
                    }
                }
                Value::Object(map)
            }
            other => {
                // Non-object source — just replace with the partial.
                if partial_doc.is_object() {
                    partial_doc
                } else {
                    other
                }
            }
        };

        // Re-index with the same ID (upsert-style).
        let resp = self.index_document(Some(id.to_string()), merged).await?;
        Ok(Some(resp))
    }

    /// Delete all documents matching the given query.
    ///
    /// Runs a search limited to 10 000 hits, then deletes each matched document.
    /// Returns `(total_matched, deleted_count)`.
    pub async fn delete_by_query(&self, query: QueryNode) -> Result<(u64, u64)> {
        let req = SearchRequest {
            query,
            from: 0,
            size: 10_000,
            ..SearchRequest::default()
        };
        let results = self.search(&req).await?;
        let total = results.total.value;
        let mut deleted = 0u64;
        for hit in results.hits {
            if self.delete_document(&hit.id).await? {
                deleted += 1;
            }
        }
        Ok((total, deleted))
    }

    // ── Search ────────────────────────────────────────────────────────────────

    /// Execute a search request against this index.
    pub async fn search(&self, request: &SearchRequest) -> Result<SearchResult> {
        // M3 framework: response cache.  Hash the request shape and the
        // current dataset version; on a hit return the cloned `Arc<SearchResult>`
        // immediately, skipping the semaphore, search_inner, locks, and
        // result construction entirely.  Cache invalidation is via the
        // `dataset_version` counter which bumps on every doc write.
        //
        // Skipped when:
        // - `request.profile == true` (profiling output should always
        //   reflect a fresh execution).
        // - `request.scroll.is_some()` / scroll cursor (stateful).
        // - `request.search_after.is_some()` (cursor-based pagination
        //   could collide with cached state).
        // XERJ_DISABLE_QUERY_CACHE=1 turns off the whole-result cache so a
        // benchmark measures true per-request execution on every call (no
        // took_ms=0 clone). Read once. Normal operation is unaffected.
        let cache_eligible = !request.profile && request.search_after.is_none() && {
            static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            !*DISABLED.get_or_init(|| {
                matches!(std::env::var("XERJ_DISABLE_QUERY_CACHE").as_deref(), Ok("1") | Ok("true"))
            })
        };
        let cache_key: Option<(u64, u64)> = if cache_eligible {
            // Hash the request via its serde_json representation,
            // streaming the serializer output STRAIGHT INTO the hasher.
            // The previous implementation built a full `String` with
            // `serde_json::to_string` on every request just to hash it —
            // a per-request allocation + copy proportional to the body
            // size that showed up as part of the fixed per-request tax
            // on trivial reads.
            use std::collections::hash_map::DefaultHasher;
            use std::hash::Hasher;
            struct HasherWriter<'a>(&'a mut DefaultHasher);
            impl std::io::Write for HasherWriter<'_> {
                #[inline]
                fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                    self.0.write(buf);
                    Ok(buf.len())
                }
                #[inline]
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            let mut h = DefaultHasher::new();
            let body_hash: Option<u64> = serde_json::to_writer(HasherWriter(&mut h), request)
                .ok()
                .map(|_| h.finish());
            let v = self.dataset_version.load(Ordering::Acquire);
            body_hash.map(|h| (h, v))
        } else {
            None
        };
        if let Some(key) = cache_key {
            if let Some(entry) = self.query_cache.get(&key) {
                let mut cloned = (**entry.value()).clone();
                cloned.took_ms = 0;
                self.metric_query_count.fetch_add(1, Ordering::Relaxed);
                return Ok(cloned);
            }
        }

        // ── Single-flight coalescing ────────────────────────────────────
        // On a `query_cache` miss, coalesce identical concurrent reads: the
        // first request of a `(hash, version)` key becomes the leader and
        // computes below; any identical request that arrives while the
        // leader is still in flight becomes a follower and awaits the
        // leader's published result instead of recomputing. See the
        // `query_inflight` field doc for the correctness argument (the key
        // is identical to the `query_cache` key, so the served result is
        // byte-identical to a cache hit). This removes the redundant
        // recompute CPU exactly in the flush/merge stall window where the
        // open-loop reader piles up same-key requests — the p99 tail.
        //
        // RAII guard: whatever return path the leader takes (success,
        // timed-out partial, or timeout error), the in-flight entry is
        // removed on drop. On the success path the leader also `send`s its
        // result so waiting followers wake immediately; on any other path it
        // publishes nothing, so followers observe the closed channel, re-check
        // the `query_cache`, and otherwise recompute independently.
        struct InflightGuard<'a> {
            map: &'a dashmap::DashMap<
                (u64, u64),
                tokio::sync::watch::Sender<Option<Arc<SearchResult>>>,
            >,
            key: (u64, u64),
        }
        impl Drop for InflightGuard<'_> {
            fn drop(&mut self) {
                self.map.remove(&self.key);
            }
        }
        fn singleflight_log() -> bool {
            static SF_LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            *SF_LOG.get_or_init(|| std::env::var("XERJ_SF_LOG").is_ok())
        }

        let mut sf_leader: Option<tokio::sync::watch::Sender<Option<Arc<SearchResult>>>> = None;
        let mut _sf_guard: Option<InflightGuard<'_>> = None;
        if let Some(key) = cache_key {
            use dashmap::mapref::entry::Entry;
            match self.query_inflight.entry(key) {
                Entry::Occupied(o) => {
                    // Follower: subscribe to the leader's result, then release
                    // the shard lock BEFORE awaiting (never hold a DashMap ref
                    // across an await).
                    let mut rx = o.get().subscribe();
                    drop(o);
                    loop {
                        if let Some(r) = rx.borrow_and_update().clone() {
                            let mut cloned = (*r).clone();
                            cloned.took_ms = 0;
                            self.metric_query_count.fetch_add(1, Ordering::Relaxed);
                            self.metric_singleflight_coalesced
                                .fetch_add(1, Ordering::Relaxed);
                            if singleflight_log() {
                                warn!(index = self.name.as_str(), "singleflight follower served");
                            }
                            return Ok(cloned);
                        }
                        if rx.changed().await.is_err() {
                            // Leader dropped its sender. If it succeeded it
                            // inserted the result into `query_cache` BEFORE
                            // dropping, so serve that; otherwise recompute.
                            if let Some(entry) = self.query_cache.get(&key) {
                                let mut cloned = (**entry.value()).clone();
                                cloned.took_ms = 0;
                                self.metric_query_count.fetch_add(1, Ordering::Relaxed);
                                self.metric_singleflight_coalesced
                                    .fetch_add(1, Ordering::Relaxed);
                                return Ok(cloned);
                            }
                            break;
                        }
                    }
                }
                Entry::Vacant(v) => {
                    // Leader: publish an empty channel now; identical requests
                    // that arrive during our computation become followers.
                    let (tx, _rx) = tokio::sync::watch::channel(None);
                    v.insert(tx.clone());
                    sf_leader = Some(tx);
                    _sf_guard = Some(InflightGuard {
                        map: &self.query_inflight,
                        key,
                    });
                }
            }
        }

        // Acquire a query permit before proceeding.  This bounds the number of
        // queries executing concurrently against this index to 64 (the semaphore
        // capacity), preventing a single hot index from starving others in a
        // multi-tenant deployment.  The permit is automatically released when
        // `_permit` is dropped at the end of this call.
        let _permit = self.max_concurrent_queries.acquire().await.map_err(|_| {
            EngineError::Common(xerj_common::XerjError::internal(
                "query semaphore closed — index is shutting down",
            ))
        })?;

        let search_start = std::time::Instant::now();

        // Determine the timeout: use the request-level timeout if set, otherwise
        // fall back to the default of 30 seconds.
        let timeout_ms = request.timeout_ms.unwrap_or(30_000);
        let timeout_duration = std::time::Duration::from_millis(timeout_ms);

        // M5.21 — run the CPU-heavy search body inside `block_in_place`
        // on multi-thread runtimes; fall back to plain await on
        // current-thread (used by `#[tokio::test]` default).
        //
        // Pre-M5.21 a concurrent QPS bench collapsed to <1 QPS.  See
        // the commit body for `perf/search-block-in-place` for the
        // full root-cause analysis.
        let is_multi_thread = tokio::runtime::Handle::current().runtime_flavor()
            == tokio::runtime::RuntimeFlavor::MultiThread;
        let search_fut = self.search_inner(request);
        let search_result = if is_multi_thread {
            let fut = async move {
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(search_fut)
                })
            };
            tokio::time::timeout(timeout_duration, fut).await
        } else {
            tokio::time::timeout(timeout_duration, search_fut).await
        };
        match search_result {
            Ok(result) => {
                let took_ms = search_start.elapsed().as_millis() as u64;
                // Record metrics.
                self.metric_query_count.fetch_add(1, Ordering::Relaxed);
                self.metric_query_total_ms
                    .fetch_add(took_ms, Ordering::Relaxed);
                // Slow query logging.
                let query_summary = summarize_query(&request.query);
                if took_ms >= 5_000 {
                    error!(
                        took_ms,
                        index = self.name.as_str(),
                        query = %query_summary,
                        "slow query"
                    );
                } else if took_ms >= 1_000 {
                    warn!(
                        took_ms,
                        index = self.name.as_str(),
                        query = %query_summary,
                        "slow query"
                    );
                }
                // Populate the response cache.  Bound the cache to ~1k
                // entries by truncating when it grows; for hot dashboards
                // the working set is far smaller and the cheap O(n)
                // truncate happens once.
                if let (Some(key), Ok(ref r)) = (cache_key, &result) {
                    // Never cache a timed-out partial result — the dataset
                    // version doesn't change on timeout, so a poisoned entry
                    // would be served until the next write. But DO hand it to
                    // any coalesced followers (identical query ⇒ identical
                    // timed-out response) so they return now instead of
                    // waiting for the closed channel and then recomputing an
                    // equally-slow query.
                    if r.timed_out {
                        if let Some(tx) = &sf_leader {
                            let _ = tx.send(Some(Arc::new(r.clone())));
                        }
                        return result;
                    }
                    if self.query_cache.len() > 1024 {
                        // Cheap eviction: clear the whole cache rather
                        // than implement LRU.  It rebuilds on the next
                        // few queries.
                        self.query_cache.clear();
                    }
                    let arc = Arc::new(r.clone());
                    self.query_cache.insert(key, Arc::clone(&arc));
                    // Single-flight: wake any coalesced followers with the
                    // freshly computed result (byte-identical to this cache
                    // entry). `send` returns Err if there are no followers —
                    // ignored. The `_sf_guard` removes the in-flight entry on
                    // drop. Ordering: cache insert precedes this send, which
                    // precedes the guard's entry removal, so a follower that
                    // races the drop still finds the result in `query_cache`.
                    if let Some(tx) = &sf_leader {
                        let _ = tx.send(Some(arc));
                    }
                }
                result
            }
            Err(_elapsed) => {
                let took_ms = search_start.elapsed().as_millis() as u64;
                self.metric_query_count.fetch_add(1, Ordering::Relaxed);
                self.metric_query_total_ms
                    .fetch_add(took_ms, Ordering::Relaxed);
                warn!(
                    took_ms,
                    timeout_ms,
                    index = self.name.as_str(),
                    "search timed out, returning partial results"
                );
                // Return empty partial results with timed_out=true signaled via
                // a special took_ms value (see es_compat handler).
                let to = SearchResult {
                    hits: vec![],
                    total: xerj_query::executor::TotalHits {
                        value: 0,
                        relation: TotalHitsRelation::Gte,
                    },
                    took_ms,
                    aggs: None,
                    timed_out: true,
                    profile: None,
                    max_score: None,
                };
                // Hand the timed-out response to any coalesced followers so
                // they return immediately rather than waiting for the closed
                // channel and recomputing an equally-slow query. Not cached.
                if let Some(tx) = &sf_leader {
                    let _ = tx.send(Some(Arc::new(to.clone())));
                }
                Ok(to)
            }
        }
    }

    /// Inner search implementation (without timeout wrapper).
    async fn search_inner(&self, request: &SearchRequest) -> Result<SearchResult> {
        // Check read block.
        if self.is_read_blocked().await {
            return Err(EngineError::Common(xerj_common::XerjError::index_blocked(
                self.name.as_str(),
                "read",
            )));
        }

        // ── Cooperative deadline ──────────────────────────────────────────
        // The `tokio::time::timeout` wrapper in `search()` is DEAD CODE on
        // the multi-thread runtime: the search body runs inside
        // `block_in_place(block_on(..))`, so the wrapper's first poll only
        // returns when the whole search is already finished — it can never
        // fire mid-flight.  Live-verified during the read-under-write
        // benchmark: 2 118 slow queries (p50 19 s, max 39 s) and ZERO
        // "search timed out" log lines; every piled-up search ran to
        // completion even after its client disconnected, which is what let
        // the backlog snowball keep the CPU pegged for minutes after load
        // stopped.  Instead we compute a wall-clock deadline here and have
        // the O(N) loops below check it cooperatively, returning partial
        // results with `timed_out: true` (ES semantics).
        let search_deadline: std::time::Instant = std::time::Instant::now()
            + std::time::Duration::from_millis(request.timeout_ms.unwrap_or(30_000));
        let mut deadline_exceeded = false;
        // Set when a field-sorted non-match_all scan was narrowed to the
        // per-segment top-cap sort candidates: the scan's own total tally is
        // partial by construction and MUST be replaced by `shortcut_count`
        // post-loop (mirrors `scan_hit_cap` for the unsorted bounded scan).
        let mut sort_candidates_narrowed = false;
        // Slow-query phase attribution (logged for >1 s queries).
        let phase_t0 = std::time::Instant::now();
        let mut phase_marks: Vec<(&'static str, u64)> = Vec::new();
        let mut dbg_segs = 0u32;
        let mut dbg_decode_ms = 0u64;
        let mut dbg_scan_ms = 0u64;
        let mut dbg_fts_ms = 0u64;
        let mut dbg_walked = 0u64;
        let mut dbg_admitted = 0u64;

        // size=0 is valid and means "return no hits but still run aggs / return total".
        let size = request.size;
        let from = request.from;

        // Resolve field aliases in the query: rewrite any alias field names to their targets.
        let resolved_query = {
            let schema = self.schema.read().await;
            rewrite_query_aliases(&request.query, &schema.schema)
        };
        let query = &resolved_query;

        // ── KNN short-circuit ──────────────────────────────────────────────────
        // Vector-only queries (top-level `knn`) bypass the FTS / doc-scan
        // path entirely: we iterate every doc, compute the configured
        // similarity against the query vector, and return the top-k by
        // similarity. This runs against both memtable and segments so
        // refreshed + flushed + merged indexes all work.
        //
        // The mapping's declared `similarity` (cosine | dot_product |
        // max_inner_product | l2_norm) is looked up via self.schema —
        // the es_compat mapper propagates it into FieldOptions.similarity
        // at index-create time. Default is cosine when unspecified.
        //
        // A Knn filter (sub-query) is applied as a doc_matches_query
        // pre-filter before the top-k extraction so filter semantics
        // match ES.
        // Top-level pure-knn is exact brute force with a pre-filter only, so
        // `num_candidates` (the ANN fan-out) has no effect and is ignored here.
        if let Some((field, query_vec, k, _num_candidates, filter_opt)) = peel_knn_query(query) {
            let similarity = {
                let schema = self.schema.read().await;
                lookup_vector_similarity(&schema.schema, &field)
            };
            return self
                .run_knn_brute_force(request, &field, &query_vec, k, filter_opt, &similarity)
                .await;
        }
        // Nested `knn` query: `nested { path: P, query: { knn { field: P.vec } } }`.
        // ES scores each parent by the best-matching nested element and
        // returns parents in similarity-descending order. We implement
        // the same by flattening the nested array per parent doc and
        // taking the max score across elements.
        if let Some((
            nested_path,
            field,
            query_vec,
            k,
            num_candidates_opt,
            pre_filter,
            post_filter,
        )) = peel_nested_knn_query(query)
        {
            let similarity = {
                let schema = self.schema.read().await;
                lookup_vector_similarity(&schema.schema, &field)
            };
            // ES semantics: `num_candidates` caps how many docs the vector
            // search returns before the outer bool.must clauses post-filter.
            // It defaults to `k` when omitted and is clamped to `>= k` (a
            // smaller fan-out than the requested top-k makes no sense).
            let num_candidates = num_candidates_opt.unwrap_or(k).max(k);
            return self
                .run_nested_knn_brute_force(
                    request,
                    &nested_path,
                    &field,
                    &query_vec,
                    k,
                    num_candidates,
                    pre_filter,
                    post_filter,
                    &similarity,
                )
                .await;
        }

        // ── Semantic search short-circuit ─────────────────────────────────────
        // v0.7-P2: pre-v0.7 the parser accepted `semantic` queries but
        // the executor had no arm for QueryNode::SemanticSearch — every
        // semantic request crashed at runtime. Fix: embed the query
        // text via the configured EmbeddingProxy, then run the existing
        // kNN brute-force path with the embedded vector.
        //
        // Failure modes (all surfaced as proper errors, not crashes):
        //  - no proxy configured → 400 with config hint
        //  - proxy timeout / 5xx → propagate the proxy's error
        //  - dim mismatch         → caught by run_knn_brute_force / HNSW
        if let Some((field, text, k, filter)) = peel_semantic_query(query) {
            return self.run_semantic(request, &field, &text, k, filter).await;
        }

        // ── Hybrid (RRF / Linear / Learned) short-circuit ─────────────────────
        // v0.7-P1: pre-v0.7 the parser accepted `hybrid` queries but the
        // executor had no arm for QueryNode::Hybrid — every hybrid request
        // crashed at runtime. Fix is to recursively run each sub-query as
        // its own search, then combine the result lists via the chosen
        // fusion strategy.
        //
        // RRF (default, k=60): each doc's combined score is
        //   Σ_i  weight_i / (k_rrf + rank_i_in_list_i)
        // where rank is 1-based. Docs missing from a list contribute 0
        // for that list. This is the same formula ES / OpenSearch use
        // and is robust to score-scale differences between BM25 and
        // vector similarity.
        //
        // Linear: each doc's combined score is
        //   Σ_i  weight_i * normalised_score_i
        // where normalised_score is min-max normalised to [0,1] within
        // each sub-list. Cheaper than RRF and lets callers bias toward
        // one signal, but sensitive to score-scale outliers.
        //
        // Learned: not yet implemented — falls back to RRF with a warn.
        if let Some((sub_queries, fusion)) = peel_hybrid_query(query) {
            return self.run_hybrid(request, sub_queries, fusion).await;
        }

        // ── Max result window enforcement ──────────────────────────────────────
        // Default max_result_window is 10,000 (matches ES default).
        // Allow override via index setting `index.max_result_window`.
        // The key may be stored either as nested object `{index: {max_result_window}}`
        // or as a literal-dotted key `{index: {"index.max_result_window"}}` depending
        // on the shape the caller provided at index creation. (Ids-count validation
        // happens at the HTTP layer where index_settings is accessible.)
        let max_result_window: usize = {
            let settings = self.settings.read().await;
            settings
                .pointer("/index/max_result_window")
                .and_then(|v| {
                    v.as_u64()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                })
                .or_else(|| {
                    settings
                        .get("index")
                        .and_then(|i| i.get("index.max_result_window"))
                        .and_then(|v| {
                            v.as_u64()
                                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                        })
                })
                .or_else(|| {
                    settings.get("index.max_result_window").and_then(|v| {
                        v.as_u64()
                            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                    })
                })
                .map(|v| v as usize)
                .unwrap_or(10_000)
        };
        if from + size > max_result_window {
            return Err(EngineError::Common(
                xerj_common::XerjError::result_window_too_large(from, size, max_result_window),
            ));
        }

        // Fetch limit is the per-sub-source materialisation cap.  We materialise
        // at most (from + size + 100) hits with their sources, and count the
        // rest as `total` without buffering them.  This keeps query memory
        // O(from + size) regardless of how many documents match.
        //
        // Historical behaviour with `track_total_hits=true` bumped this to 10M
        // and caused OOM kills on match_all queries over segment-heavy indices
        // — see CAPPED_RAM_BATTLE_2026-04-13.md.  The new contract is: count
        // accurately but **only materialise what pagination needs**.
        let materialisation_limit: usize = (from + size + 100).max(256);
        // For the memtable search that already knows how to rank a hit list,
        // we still need a hint for top-k; use the materialisation limit.
        let fetch_limit: usize = materialisation_limit;

        // Determine whether we need hit *sources* at all.
        // - size > 0          → sources for hits output
        // - rescore/highlight → mutate source after collection
        // - collapse          → needs source field
        // - sort on a field   → needs source
        // - aggs without DV fast path → needs sources
        // When none of these apply (classic `size:0 + match_all + track_total_hits`
        // counting query), we skip materialisation entirely and just count.
        let need_hits_output: bool = size > 0;
        let need_sources_for_post: bool = need_hits_output
            || !request.rescore.is_empty()
            || request.highlight.is_some()
            || request.collapse.is_some()
            || request
                .sort
                .iter()
                .any(|sf| !sf.is_score() && !sf.is_doc_order())
            || request.aggs.is_some();
        let count_only: bool = !need_sources_for_post;

        // --- Memtable search ---
        // `all_hits` is the bounded collector of fully-hydrated hits (source +
        // score).  Capacity is `materialisation_limit`; anything beyond that
        // only contributes to `total_count`.  `total_count` is the EXACT
        // number of matching documents — used for `track_total_hits=true`.
        let mut all_hits: Vec<Hit> = Vec::new();
        let mut total_count: u64 = 0;
        // O(1) dedup: tracks doc IDs already added to all_hits.
        let mut seen_ids: HashSet<String> = HashSet::new();

        // ── Global top-(from+size) sorted collector ───────────────────────
        // When the request sorts on a real field (not just `_score` / `_doc`)
        // and asks for hits (`size > 0`), the arrival-order `materialisation_limit`
        // cap on `all_hits` would truncate to the first ~256 docs *in scan
        // order* and then sort only those — returning a wrong subset once total
        // matches exceed the cap.  Instead we route every admitted hit through
        // a bounded max-heap (`SortTopK`) keyed by the final sort order, so the
        // survivors are the true GLOBAL top-(from+size).  `all_hits` is left
        // empty on this path; the heap is drained into `final_hits` after the
        // segment loop.  `total_count` is tallied independently and stays exact.
        let field_sort_active: bool = size > 0
            && request
                .sort
                .iter()
                .any(|sf| !sf.is_score() && !sf.is_doc_order());

        // ── search_after cursor, normalized ONCE up front ─────────────────
        // Date-shaped cursor strings are converted to epoch numbers here so
        // BOTH the bounded sorted collector (pre-heap rejection during
        // collection) and the legacy post-collection filter compare against
        // the same shape the hits' `sort` arrays use (`compute_sort_values`
        // emits epoch numbers for date-shaped strings).  Users may supply
        // either the raw epoch number (`search_after: [1571646604828]`) or a
        // formatted date string (`search_after: ["2019-10-21 08:30:04.828"]`);
        // ES accepts both.
        let after_norm: Option<Vec<Value>> = match &request.search_after {
            Some(vals) if !vals.is_empty() => Some(
                vals.iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let fmt = request.sort.get(i).and_then(|s| s.format.as_deref());
                        normalize_search_after_value(v, fmt)
                    })
                    .collect(),
            ),
            _ => None,
        };

        let mut sort_topk: Option<SortTopK> = if field_sort_active {
            Some(SortTopK::new(
                Arc::new(request.sort.clone()),
                materialisation_limit,
                // The cursor is applied DURING collection: the collector
                // admits only hits strictly after it, so deep cursors cost
                // the same as page 1 and are correct past the
                // materialisation cap.
                after_norm.clone(),
            ))
        } else {
            None
        };

        // Helper: admit a hit into the bounded `all_hits` collector.
        //
        // Contract:
        // - Every call increments `total_count` (exact match count).
        // - If `count_only` is true, we do NOT materialise the hit at all.
        // - Otherwise we admit up to `materialisation_limit` hits and then
        //   silently drop the rest.  `max_result_window` (default 10 000)
        //   already prevents pagination from asking for more than that, so
        //   dropping beyond the cap is safe for the user-visible window.
        //
        // Dedup: `seen_ids` guards against the same doc appearing in both the
        // memtable and a segment (possible during a flush race).  We prefer
        // the first appearance (which is usually the memtable, i.e. the
        // newest version).
        macro_rules! admit_hit {
            ($hit:expr) => {{
                total_count += 1;
                if !count_only {
                    let mut hit: Hit = $hit;
                    if let Some(topk) = sort_topk.as_mut() {
                        // Field-sorted: offer to the bounded top-N heap.  A hit
                        // admitted source-less (memtable FTS / id-only paths)
                        // gets its `_source` filled from the memtable so the
                        // sort key can be computed now — the heap must rank on
                        // the real key, not on a deferred/Null source.
                        if !seen_ids.contains(&hit.id) {
                            if hit.source.is_null() {
                                if let Some(src) = self.memtable.get_doc_source_as_value(&hit.id) {
                                    hit.source = src;
                                }
                            }
                            seen_ids.insert(hit.id.clone());
                            topk.offer(hit);
                        }
                    } else if all_hits.len() < materialisation_limit && !seen_ids.contains(&hit.id)
                    {
                        seen_ids.insert(hit.id.clone());
                        all_hits.push(hit);
                    }
                    // else: bound hit or duplicate — already counted.
                }
            }};
            // Count-only variant: increments total without allocating a Hit.
            (count $id:expr) => {{
                total_count += 1;
                if !count_only {
                    if let Some(topk) = sort_topk.as_mut() {
                        let id: String = $id;
                        if !seen_ids.contains(&id) {
                            // Pre-clone primary-key rejection: resolve the
                            // buffered source as a SHARED Arc (lookup only)
                            // and, once the heap is full, drop docs whose
                            // primary sort key is strictly worse than the
                            // heap's worst BEFORE paying the deep source
                            // clone + `seen_ids` insert + full `offer`.
                            // Ties / underivable keys are conservatively
                            // admitted; the doc was already counted above
                            // either way.  This bounds the O(memtable)
                            // field-sorted walk (match_all/range under the
                            // implicit @timestamp index sort) that was
                            // ~1 s per query against a ~1 M-doc memtable.
                            let src_arc = self.memtable.get_doc_source_arc(&id);
                            let rejected = src_arc
                                .as_deref()
                                .is_some_and(|s| memtable_primary_key_rejects(topk, s));
                            if !rejected {
                                let source = src_arc.map(|a| (*a).clone()).unwrap_or(Value::Null);
                                seen_ids.insert(id.clone());
                                topk.offer(Hit {
                                    id,
                                    score: 1.0,
                                    source,
                                    sort: Vec::new(),
                                    explain: None,
                                    highlight: None,
                                    matched_queries: Vec::new(),
                                });
                            }
                        }
                    } else if all_hits.len() < materialisation_limit {
                        let id: String = $id;
                        if !seen_ids.contains(&id) {
                            seen_ids.insert(id.clone());
                            all_hits.push(Hit {
                                id,
                                score: 1.0,
                                source: Value::Null,
                                sort: Vec::new(),
                                explain: None,
                                highlight: None,
                                matched_queries: Vec::new(),
                            });
                        }
                    }
                }
            }};
        }

        // When the query was served by the DocValues fast path, we retain the
        // internal doc indices so the aggregation path can use them to iterate
        // DocValues columns directly (Optimization 2: filtered stats agg).
        // `None` means MatchAll (use all docs) or a non-DocValues path.
        let mut mem_dv_doc_indices: Option<Vec<usize>> = None;

        // Take a snapshot of memtable data under a short read lock, then
        // release the lock before doing expensive search work.  This prevents
        // long-held read locks from blocking concurrent document indexing.
        enum MemSnapshot {
            Empty,
            FtsHits(Vec<(String, f32)>), // (doc_id, score) from inverted index
            AllDocIds(Vec<String>, u64), // MatchAll: bounded IDs + uncollected remainder count
            /// Fast DocValues hit: (doc_id, internal_index) pairs — source is NOT
            /// cloned here; it will be fetched only for the final page of hits.
            /// The `u64` is the UNCOLLECTED matching-doc remainder (bounded
            /// fused-bool path); it still counts toward `hits.total`.
            DocValuesHits(Vec<(String, usize)>, u64),
            DocsForScan(Vec<(String, Arc<Value>)>), // doc_id + shared source for term-level scan
        }

        // Search the memtable (unflushed docs only). Flushed docs are searched
        // via segment readers below. This is a known v0.1 limitation: flushed
        // docs are not yet searchable via the FTS segment reader for match
        // queries, but MatchAll and doc-scan queries do work via stored segments.
        // Step 4: fan out the 16-shard `doc_count()` sum ONCE per query and
        // reuse it at the mem_snapshot gate and the size=0 total-synthesis
        // arms below, instead of re-locking all 16 shards 3-4×.  The memtable
        // is written concurrently, so this is a point-in-time snapshot — every
        // consumer here either feeds a total that a later `live_doc_count()`
        // overwrites (match_all) or gates the empty-memtable fast path, both
        // tolerant of a one-instant-stale count.
        let mem_doc_count = self.memtable.doc_count();
        let mem_snapshot = {
            let mem = &*self.memtable;
            if mem_doc_count == 0 {
                MemSnapshot::Empty
            } else if matches!(query, QueryNode::MatchAll) && size == 0 && request.aggs.is_none() {
                // MatchAll + size=0 + NO aggs: pure count query.
                // Everything is served by try_shortcut_count via the
                // engine atomic doc_count.  Skipping the memtable
                // iteration saves the 200 k doc_id clone.
                //
                // When aggs ARE present we can NOT take this
                // short-circuit because an unsupported agg type (range,
                // filter, histogram, etc.) will fall back to
                // `run_aggs_with_all` which needs the materialised hit
                // sources — and skipping the memtable here leaves that
                // fallback with empty inputs.
                MemSnapshot::Empty
            } else if matches!(query, QueryNode::MatchAll) {
                // size > 0 match_all.  Field-sorted requests must offer
                // EVERY buffered doc to the top-N heap; unsorted requests
                // only ever admit the first `materialisation_limit` ids,
                // so clone just those and carry the remainder as a count.
                if sort_topk.is_some() {
                    // Bounded sorted candidates for the common shape
                    // (single default-flavour field sort, no cursor):
                    // columnar per-shard top-cap extraction instead of
                    // offering EVERY buffered doc to the heap.  Date-string
                    // columns (the implicit `@timestamp` index sort) resolve
                    // through the bounded process-wide epoch memo — the SAME
                    // normalisation `compute_sort_values` applies per hit —
                    // so the candidate cut ranks exactly like the heap would.
                    let bounded: Option<(Vec<String>, u64)> = (|| {
                        use xerj_query::sort::{SortMissing, SortMode, SortOrder};
                        let topk = sort_topk.as_ref()?;
                        if topk.after.is_some() || request.sort.len() != 1 {
                            return None;
                        }
                        let sf = &request.sort[0];
                        if sf.is_score()
                            || sf.is_doc_order()
                            || sf.mode != SortMode::default()
                            || sf.missing != SortMissing::default()
                        {
                            return None;
                        }
                        mem.sort_candidates_numeric(
                            &sf.field,
                            sf.order == SortOrder::Desc,
                            materialisation_limit,
                            &sort_epoch_memo,
                        )
                    })();
                    match bounded {
                        Some((ids, total)) => {
                            let extra = total.saturating_sub(ids.len() as u64);
                            MemSnapshot::AllDocIds(ids, extra)
                        }
                        // Ineligible shape (multi-field sort, cursor,
                        // non-default mode/missing, un-normalisable
                        // values): fall back to offering every buffered
                        // doc — via Arc-shared sources so the scan arm's
                        // pre-clone primary-key rejection bounds the
                        // per-doc cost to a memoised epoch lookup once
                        // the heap is full (NOT the id-only snapshot,
                        // whose per-doc deep clone cost ~1 s/query at a
                        // 1 M-doc memtable).
                        None => MemSnapshot::DocsForScan(mem.all_docs_with_sources_arc()),
                    }
                } else {
                    let (ids, total) = mem.doc_ids_bounded(materialisation_limit);
                    let extra = total.saturating_sub(ids.len() as u64);
                    MemSnapshot::AllDocIds(ids, extra)
                }
            } else if is_doc_scan_query(query) {
                // count_only + Term/Range short-circuits BEFORE
                // `try_doc_values_query` because the DV-term path walks
                // the whole memtable and clones the matching doc_id
                // Strings into a Vec — for a 170 k-hit term query that
                // was ~15 ms of pure String allocation, just to have the
                // segment `try_shortcut_count` path overwrite the count
                // a moment later.
                if count_only && matches!(query, QueryNode::Term { .. } | QueryNode::Bool { .. }) {
                    // For `size:0 + term/bool` the segment
                    // `try_shortcut_count` path handles the count from
                    // doc-values directly. We exclude `Range` here
                    // because non-indexed range fields don't have
                    // doc-values in the segment shortcut and would be
                    // silently undercounted.
                    MemSnapshot::Empty
                } else if let Some((hits, total)) = (|| {
                    // Fused columnar term/range/bool: ONE position walk
                    // per shard, bounded id materialisation, exact total.
                    // Gated off when a field sort is active — the top-N
                    // heap must see EVERY matching doc — and when aggs
                    // are present (the filtered-agg path needs the full
                    // `mem_dv_doc_indices` position set).  Placed BEFORE
                    // `try_doc_values_query`, whose unbounded walks clone
                    // every matching doc_id (~all-of-memtable for broad
                    // range filters) per request.
                    if sort_topk.is_some() || request.aggs.is_some() {
                        return None;
                    }
                    let preds = mem_bool_preds(query)?;
                    mem.doc_values_bool_query(&preds, materialisation_limit)
                })() {
                    let uncollected = total.saturating_sub(hits.len() as u64);
                    MemSnapshot::DocValuesHits(hits, uncollected)
                } else if let Some((hits, total)) = try_doc_values_query(
                    query,
                    mem,
                    // Field-sorted requests must offer EVERY matching doc to
                    // the top-N heap (sort correctness); unsorted requests
                    // only materialise the id for the first
                    // `materialisation_limit` matches and carry the rest as
                    // an uncollected count (mirrors the fused-bool path).
                    if sort_topk.is_some() {
                        usize::MAX
                    } else {
                        materialisation_limit
                    },
                ) {
                    // DocValues fast path for queries that actually need
                    // the hit list (size > 0 or sort-on-field).
                    let uncollected = total.saturating_sub(hits.len() as u64);
                    MemSnapshot::DocValuesHits(hits, uncollected)
                } else {
                    // Composite queries (e.g. bool) that the memtable DV
                    // path can't resolve: share the buffered sources via
                    // Arc — the old deep-clone of the ENTIRE memtable per
                    // query (plus a second per-doc clone for `_id`
                    // injection below) was the single hottest search-side
                    // cost in the read-under-write thread dumps.
                    let docs: Vec<(String, Arc<Value>)> = mem.all_docs_with_sources_arc();
                    MemSnapshot::DocsForScan(docs)
                }
            } else {
                let query_text = extract_query_text(query);
                if let Some(text) = query_text {
                    // FTS path: use the inverted index.  For count_only
                    // queries the default `fetch_limit` (= max(from+size+100,
                    // 256)) under-counts because the searcher truncates
                    // the hit list — request a much larger cap so the
                    // memtable contribution to total_count is accurate.
                    let mem_limit = if count_only { usize::MAX } else { fetch_limit };
                    // For a single-field Match, restrict FTS to that
                    // field — otherwise docs that store the query tokens
                    // in a *different* field (incl. dynamically-ignored
                    // ones under subobjects:false + dynamic:false) would
                    // spuriously match. QueryString / multi-field
                    // patterns still search all indexed fields.
                    let field_filter: Vec<&str> = match query {
                        QueryNode::Match { field, .. } if field != "*" && !field.ends_with('*') => {
                            vec![field.as_str()]
                        }
                        _ => Vec::new(),
                    };
                    // Per-field boosts (match `boost:` / multi_match `field^N`)
                    // must reach the memtable BM25 scorer too — the segment
                    // path applies them via query_node_to_fts, and without
                    // this the unflushed ranking disagrees with the flushed
                    // one (test_weighted_bool_boost_ranking).
                    let mut field_boosts: std::collections::HashMap<String, f32> =
                        std::collections::HashMap::new();
                    collect_field_boosts(query, &mut field_boosts);
                    let hits =
                        mem.search_text_boosted(&text, &field_filter, mem_limit, &field_boosts);
                    if !hits.is_empty() {
                        MemSnapshot::FtsHits(
                            hits.into_iter().map(|h| (h.doc_id, h.score)).collect(),
                        )
                    } else {
                        MemSnapshot::Empty
                    }
                } else if let Some((hits, total)) = try_doc_values_query(
                    query,
                    mem,
                    if sort_topk.is_some() {
                        usize::MAX
                    } else {
                        materialisation_limit
                    },
                ) {
                    let uncollected = total.saturating_sub(hits.len() as u64);
                    MemSnapshot::DocValuesHits(hits, uncollected)
                } else {
                    MemSnapshot::Empty
                }
            }
            // Lock dropped here
        };
        phase_marks.push(("mem_snapshot", phase_t0.elapsed().as_millis() as u64));

        let dbg_mem_arm: &'static str = match &mem_snapshot {
            MemSnapshot::Empty => "empty",
            MemSnapshot::FtsHits(_) => "fts",
            MemSnapshot::AllDocIds(..) => "allids",
            MemSnapshot::DocValuesHits(..) => "dv",
            MemSnapshot::DocsForScan(_) => "scan",
        };
        match mem_snapshot {
            MemSnapshot::Empty => {}
            MemSnapshot::FtsHits(hits) => {
                for (doc_id, score) in hits {
                    admit_hit!(Hit {
                        id: doc_id,
                        score,
                        source: Value::Null, // filled in below
                        sort: Vec::new(),
                        explain: None,
                        highlight: None,
                        matched_queries: Vec::new(),
                    });
                }
            }
            MemSnapshot::AllDocIds(ids, uncollected) => {
                // Bounded id page: the remainder still counts toward
                // hits.total (match_all's total is later overwritten by
                // the authoritative live count anyway).
                total_count += uncollected;
                for doc_id in ids {
                    admit_hit!(count doc_id);
                }
            }
            MemSnapshot::DocValuesHits(hits, uncollected) => {
                // DocValues fast path: source is NOT cloned here — we defer the
                // clone until after pagination so we only pay for from+size docs.
                // Retain the internal indices for filtered aggregation (Opt 2).
                let mut indices: Vec<usize> =
                    Vec::with_capacity(hits.len().min(materialisation_limit));
                for (doc_id, mem_idx) in hits {
                    if !count_only
                        && all_hits.len() < materialisation_limit
                        && !seen_ids.contains(&doc_id)
                    {
                        indices.push(mem_idx);
                    }
                    admit_hit!(count doc_id);
                }
                // Bounded fused-bool remainder: matched but un-materialised
                // docs still count toward hits.total.
                total_count += uncollected;
                mem_dv_doc_indices = Some(indices);
            }
            MemSnapshot::DocsForScan(docs) => {
                // Lock already released — scan docs without holding the memtable lock.
                //
                // `_id` injection (a full per-doc Object clone) is only
                // observable when the query tree can actually read `_id`
                // — a nested Ids query.  Detect that ONCE via the query's
                // serialised form (conservative: false positives merely
                // keep the legacy per-doc clone) instead of cloning every
                // buffered doc on every request.
                let query_may_read_id: bool = serde_json::to_string(query)
                    .map(|s| s.contains("_id") || s.contains("Ids") || s.contains("ids"))
                    .unwrap_or(true);
                for (doc_id, source) in docs {
                    let matched = if let QueryNode::Ids { values } = query {
                        values.iter().any(|v| v == doc_id.as_str())
                    } else if !query_may_read_id || source.get("_id").is_some() {
                        doc_matches_query(query, &source)
                    } else {
                        // Inject `_id` so deeply-nested Ids queries (e.g.
                        // function_score → ids) can resolve it from source.
                        let source_with_id = if let Some(obj) = source.as_object() {
                            let mut o = obj.clone();
                            o.insert("_id".to_string(), serde_json::Value::String(doc_id.clone()));
                            serde_json::Value::Object(o)
                        } else {
                            (*source).clone()
                        };
                        doc_matches_query(query, &source_with_id)
                    };
                    if matched {
                        // Materialise (score + owned source) ONLY when the
                        // hit can actually enter the bounded collector /
                        // top-N heap; everything else is a bare count.
                        // Identical outcome to unconditionally building the
                        // Hit: `admit_hit!` drops post-cap hits anyway.
                        if count_only {
                            total_count += 1;
                        } else if sort_topk.is_some()
                            || (all_hits.len() < materialisation_limit
                                && !seen_ids.contains(&doc_id))
                        {
                            // Field-sorted: pre-clone primary-key rejection.
                            // Once the heap is full, a doc whose PRIMARY
                            // sort key is strictly worse than the heap's
                            // worst can never enter the page — skip the
                            // `(*source).clone()`, the `seen_ids` insert
                            // and the full `offer` (ties and underivable
                            // keys are conservatively admitted; the doc
                            // still counts toward `total_count` exactly
                            // like `admit_hit!` would have counted it).
                            if let Some(topk) = sort_topk.as_ref() {
                                if !seen_ids.contains(&doc_id)
                                    && memtable_primary_key_rejects(topk, &source)
                                {
                                    total_count += 1;
                                    continue;
                                }
                            }
                            let score = score_query_against_doc(query, &source);
                            admit_hit!(Hit {
                                id: doc_id,
                                score,
                                source: (*source).clone(),
                                sort: Vec::new(),
                                explain: None,
                                highlight: None,
                                matched_queries: Vec::new(),
                            });
                        } else {
                            total_count += 1;
                        }
                    }
                }
            }
        }

        // --- Segment search ---
        let snap = self.store.snapshot();
        let segments_dir = self.data_dir.join("segments");

        // Determine text fields once outside the segment loop to avoid
        // re-acquiring the schema read lock on every segment iteration.
        //
        // `exact_fields` mirrors `build_fts_field_configs`: every non-Text
        // schema field (keyword / numeric / date / bool / ip) is FTS-indexed
        // with the `keyword` analyzer — the WHOLE value is one case-preserved
        // token.  The query side (query_node_to_fts) must therefore look
        // these fields up by whole value, never by standard-analyzer tokens:
        // a token like "claude" can never match the keyword term
        // "claude-haiku-4-5", which made multi_match / query_string /
        // simple_query_string return 0 hits on keyword-only mappings.
        let (text_fields, exact_fields): (Vec<String>, std::collections::HashSet<String>) = {
            let schema_guard = self.schema.read().await;
            let mut tf: Vec<String> = Vec::new();
            let mut ef: std::collections::HashSet<String> = std::collections::HashSet::new();
            for f in &schema_guard.schema.fields {
                if matches!(f.field_type, FieldType::Text) {
                    tf.push(f.name.clone());
                } else {
                    ef.insert(f.name.clone());
                }
            }
            (tf, ef)
        };

        // ── Fast path: count-only shortcut ────────────────────────────────
        //
        // Ported from Elasticsearch's
        // `QueryPhaseCollectorManager.shortcutTotalHitCount`
        // (server/src/main/java/org/elasticsearch/search/query/
        //  QueryPhaseCollectorManager.java:607-660).
        //
        // When the user only asks for `{"size":0, "query":X}` (and possibly
        // `track_total_hits=true`), we don't need to iterate a single
        // document — we can read the count straight out of the FST / segment
        // metadata for a handful of query shapes.  This turns `size:0 term`
        // queries from ~660 ms (full stored-doc scan) to ~1 ms (O(segments)
        // metadata reads).
        //
        // Shortcut-able query shapes:
        //   - MatchAll: sum of segment doc_counts + memtable doc_count.
        //   - Term { field, value }:
        //       - Keyword field with a memtable doc-values column — match
        //         against the column directly.
        //       - FTS term dictionary on disk — read the FST's `doc_freq`
        //         for the term (with a lowercased fallback because the
        //         segment analyzer lowercases).
        //       - If NEITHER path has data for the field, we conservatively
        //         return `None` and fall through to the regular scan.
        //
        // Anything more complex (Bool, Range, Match) currently returns
        // `None` — `Range` will be shortcut-able once G3 (BKD) lands.
        let is_match_all = matches!(query, QueryNode::MatchAll);

        // For MatchAll we now skip the memtable iteration entirely at
        // `MemSnapshot` time (it was cloning 200 k doc_ids just to tick a
        // counter).  Re-add the memtable's doc count here so downstream
        // branches that add segment counts on top arrive at the right
        // total.  `try_shortcut_count` + the precomputed agg fast path
        // both overwrite `total_count` anyway, so double-counting isn't a
        // concern there; this only matters for the fallback segment
        // stored-scan path.
        // For MatchAll+size=0 with NO aggs, `MemSnapshot::Empty` skipped
        // the memtable iteration, so `total_count` is missing the
        // memtable contribution — add it back here.  With aggs
        // present, `MemSnapshot::AllDocIds` already counted every
        // memtable doc via `admit_hit!(count ...)` — if we add
        // `mem_count` again we double-count (visible as the 40 vs 20
        // failure on `test_terms_agg_bucket_counts` under M5.1).
        if is_match_all && size == 0 && request.aggs.is_none() {
            let mem_count = mem_doc_count as u64;
            total_count = total_count.saturating_add(mem_count);
        }

        // Compute the authoritative match count via the fast doc-values /
        // FST metadata path.  Historically this was gated on `count_only`
        // (size:0), but F1 decouples the EXACT total from source
        // materialisation for size>0 too: when the count is known cheaply we
        // can bound the stored-doc scan to only the top (from+size) hits
        // instead of walking every match to tally `hits.total`.  For query
        // shapes the shortcut can't resolve it returns `None` (cheaply) and we
        // fall back to the full counting scan — so this is always safe.
        let shortcut_count: Option<u64> = self.try_shortcut_count(query, &snap, is_match_all).await;

        // Whether this query resolves to an FTS (inverted-index) search.  The
        // FTS scored path already counts authoritatively via `seg_hits.len()`
        // and materialises only the top prefix, so the F1 bounded-scan / count
        // overwrite must NOT touch FTS queries — it only applies to the
        // non-FTS stored-doc scan (match_all / term-on-keyword / range).
        let query_needs_fts: bool = query_node_to_fts(query, &text_fields, &exact_fields).is_some();

        // ── Precomputed segment agg fast path (M2 G2) ─────────────────────
        //
        // When the request is `size=0 + aggs + match_all`, every agg can
        // often be served directly from segment `.dv` sidecars.  If that
        // succeeds here — BEFORE the stored-section scan — we skip the
        // scan entirely and synthesize `total_count` from segment metadata
        // + memtable.  This is the M2/G2 "100× agg" win.
        //
        // We only try this when hits output isn't needed (`size==0`), so
        // there's no requirement to materialise source values.  The agg
        // result is stashed in `precomputed_aggs` and consumed at the
        // agg-section below.
        let mut precomputed_aggs: Option<Value> = None;
        phase_marks.push(("mem_admit", phase_t0.elapsed().as_millis() as u64));
        let dbg_sort_topk = sort_topk.is_some();
        let dbg_mem_hits = all_hits.len();
        let dbg_mem_seen = seen_ids.len();
        let dbg_mem_total = total_count;
        // Extend the columnar agg fast path to FILTERED `size:0` aggs: a
        // `{size:0, query:Q, aggs:…}` where Q is a term/terms/numeric-range/
        // conjunctive-bool over doc-values fields.  Without this the filtered
        // shape fell through to the full-corpus `_source` hydration path
        // (~7.8 s on 1 M docs) while the unfiltered shape was ~0.1 ms — a
        // ~78,000× cliff on any filtered-agg benchmark row.  The filter is
        // evaluated columnarly (matching-row gate) and the reduction runs over
        // only the matching docs' doc-values, byte-identical to the brute
        // path.  Non-columnarizable queries (FTS, must_not/should bool, …)
        // yield `agg_filter == None` here and keep the exact brute fallback.
        let agg_filter: Option<Value> = if is_match_all {
            None
        } else {
            query_node_to_agg_filter(query)
        };
        if size == 0
            && request.aggs.is_some()
            && request.min_score.is_none()
            && !query_needs_fts
            && (is_match_all || agg_filter.is_some())
        {
            if let Some(aggs_def) = &request.aggs {
                let r_opt = self
                    .try_aggs_fast_with_segments_cached(
                        aggs_def,
                        agg_filter.as_ref(),
                        &snap,
                        &segments_dir,
                    )
                    .await;
                if let Some((r, filtered_total)) = r_opt {
                    precomputed_aggs = Some(r);
                    match filtered_total {
                        // Filtered fast path already computed the matching-doc
                        // count (exact — no deletes on the fast path).
                        Some(t) => {
                            total_count = t;
                        }
                        // match_all: synthesize total_count from segments +
                        // memtable so we don't touch the stored section at all
                        // (later overwritten with the delete-aware live count).
                        None => {
                            let seg_docs: u64 = snap.segments.iter().map(|m| m.doc_count).sum();
                            let mem_count = mem_doc_count as u64;
                            total_count = seg_docs + mem_count;
                        }
                    }
                }
            }
        }

        // F1 correctness gate (hoisted so both the scan-early-stop decision and
        // the post-loop `total_count` overwrite can see it): the doc-values/FST
        // `shortcut_count` is DELETE-BLIND (counts physical postings, not live
        // docs), whereas the stored-doc scan it lets us skip filters tombstones.
        // The shortcut may only stand in for the EXACT `hits.total` when the
        // index has no deleted/superseded docs. Conservative O(segments) signal:
        // any flushed tombstone, OR live_count < physical (updates + deletes
        // inflate the per-segment/memtable physical tally above the version-map
        // live count). MatchAll is unaffected — overwritten with the
        // delete-aware live_doc_count() regardless of this flag.
        // EXACT ghost signal (see `VersionMap::ghost_events`): flushed
        // tombstones OR any overwrite/delete event ever recorded.  The old
        // `live_count() < seg_physical + mem_physical` arithmetic also
        // tripped on unrelated physical-count drift — live-verified: two
        // duplicate docs per merged segment turned the gate permanently ON
        // for a pure append-only corpus, disabling the F1 bounded scan and
        // forcing every size>0 term/range/bool into a full O(N) stored
        // scan.  That was the dominant per-query cost in the
        // read-under-write collapse.
        let deletes_present: bool = snap.segments.iter().any(|m| m.has_tombstones)
            || self.store.version_map.ghost_events() > 0;

        phase_marks.push(("fast_aggs+gates", phase_t0.elapsed().as_millis() as u64));
        if precomputed_aggs.is_some() {
            // Skip the entire segment loop — the fast agg path already
            // computed everything we need.  `total_count` is set above.
        } else if count_only && shortcut_count.is_some() {
            // Overwrite `total_count` — the memtable/bounded-collector scan
            // above has already looked at the memtable, but we want an
            // authoritative per-shape count, not the sum of two sources.
            // (The memtable path bails on `count_only` without double
            // counting, but to be safe we set the total directly.)
            //
            // NOTE: size>0 queries that ALSO have a shortcut count do NOT take
            // this branch — they fall through to the segment loop so their
            // top (from+size) hit *sources* still get materialised.  Their
            // `total_count` is overwritten with `shortcut_count` after the
            // loop, and the scan runs in bounded (`count_authoritative`) mode.
            total_count = shortcut_count.unwrap();
        } else if is_match_all && count_only {
            // Legacy MatchAll fast path (covers the `try_shortcut_count`
            // return as well, but keeps existing behaviour if the helper
            // ever returns None for MatchAll).
            let seg_docs: u64 = snap.segments.iter().map(|m| m.doc_count).sum();
            total_count += seg_docs;
        } else {
            // Decide up-front whether this query needs FTS side-cars at all.
            // For term/range/etc. queries we skip FtsIndexReader::open
            // entirely — that call eagerly reads .fst/.meta/.post/.norms
            // for every text field, which costs ~50 MB per segment and is
            // wasted work when the query is a stored-doc scan.
            let fts_query_probe = query_node_to_fts(query, &text_fields, &exact_fields);
            let needs_fts = fts_query_probe.is_some();
            // Open the FTS reader with every field the projected query
            // actually touches, ON TOP of the text fields.  Keyword-typed
            // fields have FTS side-cars too (whole-value keyword-analyzer
            // terms) but were never loaded because `field_refs` only listed
            // Text fields — so keyword projections silently searched an
            // empty reader.  Restricting the extension to the queried
            // fields keeps the eager `.fst/.meta/.post/.norms` read bounded.
            let fts_open_fields: Vec<String> = {
                let mut all = text_fields.clone();
                if let Some(fq) = &fts_query_probe {
                    let mut qf: Vec<String> = Vec::new();
                    collect_fts_query_fields(fq, &mut qf);
                    for f in qf {
                        if !all.contains(&f) {
                            all.push(f);
                        }
                    }
                }
                all
            };
            drop(fts_query_probe); // only needed for the decision + field list

            // F1: when the EXACT total is already known independently of the
            // scan — MatchAll (overwritten with `live_doc_count()` below) or a
            // resolved `shortcut_count` (doc-values / FST) — the stored-doc
            // scan no longer needs to walk every match just to tally
            // `hits.total`.  In that case we let the non-FTS scan STOP as soon
            // as it has materialised the top `materialisation_limit` sources,
            // turning size>0 match_all/term/range from O(total-matches) into
            // O(from+size).  When the count is NOT authoritative (no shortcut,
            // not match_all) we keep the full counting scan for correctness.
            // F1 correctness gate: the doc-values/FST `shortcut_count` is
            // DELETE-BLIND — it counts physical postings, not live docs —
            // whereas the stored-doc scan it lets us skip filters tombstones.
            // So the shortcut may only stand in for the EXACT `hits.total` when
            // the index has no deleted/superseded docs. Cheap, conservative
            // signal: any flushed tombstone, OR live_count < physical (updates
            // and deletes both inflate the per-segment/ memtable physical tally
            // above the version-map live count). MatchAll is unaffected: it is
            // overwritten with the delete-aware `live_doc_count()` regardless.
            // When deletes ARE present (and not match_all) we keep the full
            // delete-aware counting scan for a correct total, at the cost of the
            // pre-F1 scan time — correctness over speed until the shortcut
            // itself is made delete-aware. (`deletes_present` hoisted above.)
            let count_authoritative: bool = !query_needs_fts
                && (is_match_all || (shortcut_count.is_some() && !deletes_present));

            // Whether a Regexp query's field is keyword-typed — gates the
            // FST term-dictionary route of the regexp pre-filter (computed
            // here because the segment loop below is sync).  See
            // `build_regexp_prefilter_cached` for the exactness argument.
            let regexp_field_is_keyword: bool = if let QueryNode::Regexp { field, .. } = query {
                let schema_guard = self.schema.read().await;
                schema_guard
                    .schema
                    .field(field)
                    .map(|f| matches!(f.field_type, FieldType::Keyword))
                    .unwrap_or(false)
            } else {
                false
            };

            for meta in &snap.segments {
                dbg_segs += 1;
                // F1: once the exact total is authoritative AND the bounded
                // collector is full, remaining segments can neither add a
                // materialised hit (page is full) nor change `hits.total`
                // (overwritten by shortcut/live_doc_count).  Skip them WITHOUT
                // opening + decompressing their stored section — otherwise the
                // O(N) cost just moves from JSON-parsing to `decode_stored`
                // over every segment.  This is what turns the whole query into
                // O(from+size) rather than O(segments·docs_per_segment).
                if count_authoritative && all_hits.len() >= materialisation_limit {
                    break;
                }
                // Cooperative deadline: stop fanning into further segments
                // once the request timeout has elapsed — partial results
                // with `timed_out: true`, exactly like ES.
                if std::time::Instant::now() >= search_deadline {
                    deadline_exceeded = true;
                    break;
                }

                let seg_id = meta.id.clone();
                let fts_dir = segments_dir.clone();

                let field_refs: Vec<&str> = fts_open_fields.iter().map(|s| s.as_str()).collect();
                let scan_stored = matches!(query, QueryNode::MatchAll) || is_doc_scan_query(query);

                // Try FTS path first if we have an FTS query.
                let mut fts_handled = false;
                let t_fts = std::time::Instant::now();
                let reader_opt = if needs_fts {
                    FtsIndexReader::open(&fts_dir, &seg_id, &field_refs).ok()
                } else {
                    None
                };
                dbg_fts_ms += t_fts.elapsed().as_millis() as u64;
                if let Some(reader) = reader_opt {
                    let reader = Arc::new(reader);
                    let searcher =
                        FtsSearcher::new(Arc::clone(&reader), Arc::clone(&self.registry));
                    let fts_query = query_node_to_fts(query, &text_fields, &exact_fields);

                    // Count-only fast path for single-term FTS queries:
                    // call `term_doc_freq` directly on the segment reader
                    // so we never decode the postings list.  This is the
                    // bug fix that pulls `match field:term` from
                    // truncated-256-hits to exact total.
                    if count_only {
                        if let Some(FtsQuery::Term(t)) = fts_query.as_ref() {
                            if let Some(df) = reader.term_doc_freq(&t.field, &t.term) {
                                total_count += df as u64;
                                continue;
                            }
                        }
                    }

                    if let Some(fq) = fts_query {
                        // Check if the FTS reader actually has data for the
                        // queried field(s).  M5.4 skips FTS build at flush,
                        // so the reader may open successfully (empty sidecars)
                        // but contain no terms — in that case fall through to
                        // stored-doc scan instead of claiming "0 matches".
                        // Generalised from the old single-`Term` probe: a
                        // Bool projection (multi_match / lowered
                        // query_string / simple_query_string) over a field
                        // with no FTS data would otherwise "authoritatively"
                        // report 0 matches for the segment. Require every
                        // referenced field to be present; if any is missing,
                        // fall through to the stored-doc scan.
                        let fts_has_field = {
                            let mut qf: Vec<String> = Vec::new();
                            collect_fts_query_fields(&fq, &mut qf);
                            qf.iter().all(|f| reader.field_stats(f).is_some())
                        };

                        // Counting is DECOUPLED from materialisation on the
                        // scored FTS path.  `search_bounded` returns the EXACT
                        // segment match `seg_total` (counted over the full match
                        // set, independent of the page cap) alongside the best
                        // `cap` hits.  Previously the size>0 path passed
                        // `fetch_limit` and tallied `total_count` per materialised
                        // hit, so `hits.total` was capped at the ~256
                        // materialisation limit while size:0 (which used
                        // `usize::MAX`) was exact — DEFECT #3.  Now both paths add
                        // the full `seg_total` so the count agrees, and we
                        // materialise *sources* for only the top
                        // `materialisation_limit` hits.
                        //
                        // PERF (bounded top-N): the old code built + fully sorted
                        // the ENTIRE match set per segment even for size:10 —
                        // O(N log N) on tens of thousands of hits.  On the common
                        // path (no field sort, not count-only, no deletes) only
                        // the top `materialisation_limit` sources are ever
                        // materialised (the loop below breaks there), so we cap
                        // the collector heap at `materialisation_limit`: O(N log
                        // cap) work, O(cap) memory, byte-identical top-k prefix.
                        // The three cases that consume PAST the page cap keep the
                        // legacy full-set path (`usize::MAX`):
                        //   * `sort_topk.is_some()` — the field-sort arm narrows
                        //     *every* matching doc_id to sort candidates.
                        //   * `count_only` — wants the exact total with no page
                        //     cap (and never materialises).
                        //   * `deletes_present` — tombstones/superseded dups can
                        //     skip past `materialisation_limit` matches while
                        //     filling the page, so the page needs more than the
                        //     top-`cap` by score.
                        let fts_cap = if count_only || sort_topk.is_some() || deletes_present {
                            usize::MAX
                        } else {
                            materialisation_limit
                        };
                        if fts_has_field {
                            if let Ok((seg_hits, seg_total)) =
                                searcher.search_bounded(&fq, fts_cap, false)
                            {
                                // FTS is authoritative for this segment (field
                                // present + searcher ran) — mark handled the same
                                // way for count-only and size>0 so the fall-through
                                // decision (and thus the count) agrees.
                                fts_handled = true;
                                // Exact count — identical for count_only and size>0.
                                total_count += seg_total;
                                // Field-sorted FTS (e.g. bool/match under the
                                // implicit `@timestamp desc` index sort): the
                                // legacy arm below decodes + parses the WHOLE
                                // stored section per segment per query and offers
                                // every match to the heap.  Narrow the match set
                                // to the per-segment top-cap candidates by
                                // primary sort key (+ ties) and hydrate only
                                // those.  `hits.total` is already exact
                                // (`seg_hits.len()` tallied above), so hydrate's
                                // own partial tally is discarded.
                                let mut fts_sorted_handled = false;
                                #[allow(clippy::unnecessary_unwrap)]
                                // is_some() gate; as_mut() below needs the checked form
                                if !count_only && !seg_hits.is_empty() && sort_topk.is_some() {
                                    let cand_opt = {
                                        let topk_ref =
                                            sort_topk.as_ref().expect("gated on is_some");
                                        let matches: HashSet<u32> =
                                            seg_hits.iter().map(|sh| sh.doc_id).collect();
                                        self.narrow_matches_to_sort_candidates(
                                            &segments_dir,
                                            &seg_id,
                                            meta.doc_count,
                                            topk_ref,
                                            materialisation_limit,
                                            &matches,
                                        )
                                    };
                                    if let Some(cand) = cand_opt {
                                        let best_rejected = cand
                                            .ordered
                                            .first()
                                            .zip(sort_topk.as_ref())
                                            .is_some_and(|(&(kb, _), topk)| {
                                                topk.primary_f64_rejects(f64::from_bits(kb as u64))
                                            });
                                        if best_rejected {
                                            // No candidate can enter the page —
                                            // total is already exact from
                                            // seg_hits.len(); skip the stored
                                            // bytes entirely.
                                            fts_sorted_handled = true;
                                        } else if let Some(slices) =
                                            self.stored_slices_for(seg_id.as_str(), meta.doc_count)
                                        {
                                            let mut discard = 0u64;
                                            self.hydrate_sorted_candidates(
                                                &slices,
                                                &cand,
                                                query,
                                                false,
                                                sort_topk.as_mut().expect("gated on is_some"),
                                                &mut seen_ids,
                                                &mut discard,
                                            );
                                            fts_sorted_handled = true;
                                        }
                                    }
                                }
                                // Materialise sources for the top hits only.
                                // `seg_hits` is score-sorted descending, so taking
                                // the prefix that fits under `materialisation_limit`
                                // preserves top-k ordering/scoring.
                                if !fts_sorted_handled
                                    && !count_only
                                    && !seg_hits.is_empty()
                                    && all_hits.len() < materialisation_limit
                                {
                                    // F2 RANDOM-ACCESS source hydration.  The
                                    // scored FTS page needs `_source` for only
                                    // the top-(from+size) matched doc_ids, yet
                                    // the legacy path decoded the ENTIRE stored
                                    // section (`from_slice::<Vec<Value>>`,
                                    // ~10 MB / O(N) per 100k docs) just to index
                                    // them — the dominant size>0 scored-read cost
                                    // (~485 ms at size:10 vs ~1 ms at size:0).
                                    // Reuse the per-segment offset index
                                    // (`stored_slices_for`, the SAME
                                    // random-access mechanism the term /
                                    // doc-values page hydration uses) to parse
                                    // ONLY the page's doc slices: O(page) parses
                                    // on a warm segment.  Each slice is a
                                    // complete top-level `{...}` object (see
                                    // `brace_walk_offsets`), so parsing it with
                                    // the SAME `serde_json` parser the legacy
                                    // path used yields a byte-identical
                                    // `_source` — we deliberately do NOT switch
                                    // to simd_json here (the M7 raw-bytes
                                    // payload corruption noted below still
                                    // applies).
                                    let slices_opt =
                                        self.stored_slices_for(seg_id.as_str(), meta.doc_count);
                                    // Legacy whole-section fallback ONLY when the
                                    // offset index can't be built (open/decode
                                    // failure or a malformed/incomplete section).
                                    // Merge-race hardening (2026-07): a genuine
                                    // segment-open failure must FAIL the query
                                    // (`?`), not silently shrink its results —
                                    // under the read-lease fix the files are
                                    // guaranteed present, so an open error is
                                    // real corruption.
                                    let whole_fallback: Option<Vec<Value>> =
                                        if slices_opt.is_some() {
                                            None
                                        } else {
                                            let seg_reader =
                                                self.store.open_segment_arc(&seg_id)?;
                                            match seg_reader.section(SectionType::Stored) {
                                                Ok(Some(raw)) => {
                                                    match xerj_storage::stored_codec::decode_stored(
                                                        raw,
                                                    ) {
                                                        // serde_json — see ffd49ac,
                                                        // simd_json silently
                                                        // corrupts some M7
                                                        // raw-bytes payloads.
                                                        Ok(b) => serde_json::from_slice::<
                                                            Vec<Value>,
                                                        >(
                                                            &b
                                                        )
                                                        .ok(),
                                                        Err(_) => None,
                                                    }
                                                }
                                                _ => None,
                                            }
                                        };
                                    if slices_opt.is_some() || whole_fallback.is_some() {
                                        // Owned parsed doc for a stored position,
                                        // shared by both source layouts.  The
                                        // slice arm parses one object on demand;
                                        // the fallback clones from the fully
                                        // decoded Vec.
                                        let fetch_doc = |pos: u32| -> Option<Value> {
                                            if let Some(slices) = slices_opt.as_deref() {
                                                let &(start, end) =
                                                    slices.offsets.get(pos as usize)?;
                                                let slice = slices
                                                    .bytes
                                                    .get(start as usize..end as usize)?;
                                                serde_json::from_slice::<Value>(slice).ok()
                                            } else {
                                                whole_fallback
                                                    .as_ref()
                                                    .and_then(|d| d.get(pos as usize).cloned())
                                            }
                                        };
                                        for sh in &seg_hits {
                                            // Page full — the rest are already
                                            // counted in `total_count`, so stop
                                            // decoding sources.  When a field
                                            // sort is active we must NOT stop:
                                            // every match has to be offered to
                                            // the bounded top-N heap so the
                                            // survivors are the GLOBAL top-N by
                                            // the sort key, not the highest-
                                            // scoring prefix.
                                            if sort_topk.is_none()
                                                && all_hits.len() >= materialisation_limit
                                            {
                                                break;
                                            }
                                            let doc = match fetch_doc(sh.doc_id) {
                                                Some(d) => d,
                                                None => continue,
                                            };
                                            let id = doc
                                                .get("_id")
                                                .and_then(Value::as_str)
                                                .unwrap_or("")
                                                .to_string();
                                            if let Some(ver) = self.store.version_map.get(&id) {
                                                if ver.deleted {
                                                    continue;
                                                }
                                            }
                                            // Dedup against memtable/earlier
                                            // segments WITHOUT touching
                                            // `total_count` (already tallied
                                            // above via `seg_total`).
                                            if seen_ids.contains(&id) {
                                                continue;
                                            }
                                            let source = doc
                                                .get("_source")
                                                .cloned()
                                                .unwrap_or(Value::Null);
                                            seen_ids.insert(id.clone());
                                            let hit = Hit {
                                                id,
                                                score: sh.score,
                                                source,
                                                sort: Vec::new(),
                                                explain: None,
                                                highlight: None,
                                                matched_queries: Vec::new(),
                                            };
                                            if let Some(topk) = sort_topk.as_mut() {
                                                topk.offer(hit);
                                            } else {
                                                all_hits.push(hit);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } // close fts_has_field
                }

                // If FTS didn't handle it, fall through to stored-doc scan.
                // This covers MatchAll (no FTS query), term-level queries
                // that are routed through doc scan, AND segments missing
                // FTS sidecars (e.g. after M5.4 flush which skips FTS build).
                // When `needs_fts` was true but the reader failed to open,
                // we degrade to stored-section scan so the query still
                // returns correct results (just slower).
                let scan_stored = scan_stored || (needs_fts && !fts_handled);
                if !fts_handled && scan_stored {
                    // M2 G3: for Range queries, compute the matching doc-id
                    // set from the segment's on-disk sorted numeric index
                    // and pass it as a pre-filter so `scan_stored_section_into`
                    // only parses those specific positions.
                    // Field-sort candidate set for THIS segment (page-ordered
                    // positions + primary keys) — set when the bounded
                    // sorted-candidates path is provably safe; the plain
                    // position pre-filter below is derived from it.
                    let mut sort_cand: Option<SortCandidates> = None;
                    let pre_filter: Option<Arc<HashSet<u32>>> = if let QueryNode::Range {
                        field,
                        gte,
                        gt,
                        lte,
                        lt,
                        ..
                    } = query
                    {
                        let base = self.build_range_prefilter_cached(
                            &segments_dir,
                            &seg_id,
                            field,
                            gte.as_ref(),
                            gt.as_ref(),
                            lte.as_ref(),
                            lt.as_ref(),
                        );
                        // Field-sorted Range whose exact total comes from
                        // the dv shortcut: narrow the (possibly huge)
                        // match set to the per-segment top-cap by sort
                        // key (+ boundary ties) — every candidate is a
                        // genuine range match, so the bounded hydration
                        // path applies and the heap sees exactly the
                        // candidates that can reach the page.  The
                        // scan-tallied total is partial by design and is
                        // overwritten with `shortcut_count` post-loop
                        // (`sort_candidates_narrowed`).
                        if let (Some(topk), Some(base_set)) = (sort_topk.as_ref(), base.as_ref()) {
                            if shortcut_count.is_some() && !deletes_present && !count_only {
                                sort_cand = self.narrow_matches_to_sort_candidates(
                                    &segments_dir,
                                    &seg_id,
                                    meta.doc_count,
                                    topk,
                                    materialisation_limit,
                                    base_set,
                                );
                            }
                        }
                        match &sort_cand {
                            Some(sc) => Some(Arc::new(sc.set.clone())),
                            None => base,
                        }
                    } else if let QueryNode::Term { field, value, .. } = query {
                        // Selective `term`: parse only the matching stored
                        // positions instead of the whole section (id lookups,
                        // `code:500`). A field sort still gets a complete set
                        // (the builder never caps a set it returns), so the
                        // top-N heap sees every match.
                        self.build_term_prefilter_cached(
                            &segments_dir,
                            &seg_id,
                            field,
                            std::slice::from_ref(value),
                        )
                    } else if let QueryNode::Terms { field, values, .. } = query {
                        self.build_term_prefilter_cached(&segments_dir, &seg_id, field, values)
                    } else if let QueryNode::Ids { values } = query {
                        // Resolve primary keys to stored positions via the
                        // per-segment id index (the GET `_doc/{id}` / `_mget`
                        // lookup), so an `ids` query hydrates O(#ids) positions
                        // instead of scanning the whole collection.  Gated on
                        // `!deletes_present`: with overwrites/tombstones a live
                        // id can occupy positions in more than one segment, so
                        // the scan's delete-aware `_id` matching is used
                        // instead.
                        if deletes_present {
                            None
                        } else {
                            self.build_ids_prefilter_cached(&seg_id, meta.doc_count, values)
                        }
                    } else if matches!(query, QueryNode::Bool { .. }) {
                        // Pure-conjunction bool: parse only the most-selective
                        // conjunct's docs; `doc_matches_query` re-tests the full
                        // bool per admitted doc (superset filter).
                        self.build_bool_prefilter_cached(&segments_dir, &seg_id, query)
                    } else if is_match_all && !deletes_present {
                        // Sorted-DV candidate pruning for field-sorted
                        // match_all: the segment's sorted numeric
                        // doc-values index yields the only positions that
                        // can reach the global top-(from+size) page —
                        // including the `search_after` cursor bound — so
                        // the stored scan parses O(from+size) docs per
                        // segment instead of every doc.  `None` when the
                        // shape isn't provably safe (non-numeric field,
                        // nulls/arrays present, dv/stored misalignment,
                        // deletes) → full scan, still correct.
                        sort_cand = sort_topk.as_ref().and_then(|topk| {
                            self.build_sort_candidates_prefilter(
                                &segments_dir,
                                &seg_id,
                                meta.doc_count,
                                topk,
                                materialisation_limit,
                            )
                        });
                        sort_cand.as_ref().map(|sc| Arc::new(sc.set.clone()))
                    } else if let QueryNode::Regexp { field, pattern } = query {
                        // Dictionary-expansion pre-filter (mirrors the
                        // `try_shortcut_count` Regexp arm).  When the
                        // exact total comes from the shortcut
                        // (count_authoritative) AND the hits collector is
                        // the arrival-order bounded `all_hits` (no field
                        // sort), the scan early-breaks at
                        // `materialisation_limit` hits, so cap the
                        // position set there; otherwise the scan must
                        // visit EVERY match — to tally the exact total
                        // and/or to offer every match to the field-sort
                        // top-N heap — so only a COMPLETE position set is
                        // a valid filter.
                        //
                        // The `sort_topk.is_none()` gate is load-bearing:
                        // with a field sort active (incl. the implicit
                        // `@timestamp desc` index sort injected by the ES
                        // layer for time-series mappings) hits bypass
                        // `all_hits`, so the caller's early-break and the
                        // post-loop `scan_hit_cap` shortcut-count
                        // overwrite NEVER fire — a capped prefilter would
                        // leak `segments × materialisation_limit` into
                        // `hits.total` (the 512-vs-847306 bench
                        // regression) and starve the top-N heap of
                        // candidates.
                        let cap = if count_authoritative && sort_topk.is_none() {
                            Some(materialisation_limit)
                        } else {
                            None
                        };
                        self.build_regexp_prefilter_cached(
                            &segments_dir,
                            &seg_id,
                            field,
                            pattern,
                            cap,
                            regexp_field_is_keyword,
                        )
                        .map(Arc::new)
                    } else {
                        None
                    };

                    // A `Some(∅)` pre-filter proves no doc in this segment can
                    // contribute a hit or a count (Range: no value in range;
                    // sorted-DV: no candidate position): skip the open +
                    // decompress + brace-scan entirely — the scan would have
                    // skipped every position anyway.
                    // Sorted-DV candidate path (field-sorted match_all with a
                    // candidate pre-filter): serve the candidates from the
                    // per-segment `stored_slices_cache` when warm — two vec
                    // lookups + O(candidates) simd_json parses, no per-query
                    // decompress and no O(segment-bytes) brace re-scan.
                    let sorted_candidates_path = sort_topk.is_some() && sort_cand.is_some();
                    if sorted_candidates_path && !is_match_all {
                        // The scan/hydrate below visits only the narrowed
                        // candidates, so its total tally is partial — flag
                        // the post-loop `shortcut_count` overwrite.
                        sort_candidates_narrowed = true;
                    }

                    if pre_filter.as_ref().is_some_and(|pf| pf.is_empty()) {
                        // A `Some(∅)` pre-filter proves no doc in this segment
                        // can contribute a hit or a count (Range: no value in
                        // range; sorted-DV: no candidate position): skip the
                        // open + decompress + brace-scan entirely — the scan
                        // would have skipped every position anyway.
                    } else if sorted_candidates_path
                        && sort_cand
                            .as_ref()
                            .and_then(|sc| sc.ordered.first())
                            .zip(sort_topk.as_ref())
                            .is_some_and(|(&(kb, _), topk)| {
                                topk.primary_f64_rejects(f64::from_bits(kb as u64))
                            })
                    {
                        // This segment's BEST candidate already loses to the
                        // full heap on the primary key — every candidate does
                        // (page order).  Skip the segment without touching
                        // its stored bytes.  The partial tally is overwritten
                        // post-loop (live count / shortcut).
                    } else if sorted_candidates_path && {
                        // Warm-or-build slices, then hydrate ONLY the
                        // page-ordered candidates (pre-parse primary-key
                        // rejection inside).  `false` (slices unavailable —
                        // genuine decode failure) falls through to the
                        // legacy full-scan arm below.
                        let t_dec = std::time::Instant::now();
                        let slices_opt = self.stored_slices_for(seg_id.as_str(), meta.doc_count);
                        dbg_decode_ms += t_dec.elapsed().as_millis() as u64;
                        match slices_opt {
                            Some(slices) => {
                                let t_scan = std::time::Instant::now();
                                self.hydrate_sorted_candidates(
                                    &slices,
                                    sort_cand.as_ref().expect("gated on is_some"),
                                    query,
                                    is_match_all,
                                    sort_topk.as_mut().expect("gated on is_some"),
                                    &mut seen_ids,
                                    &mut total_count,
                                );
                                dbg_scan_ms += t_scan.elapsed().as_millis() as u64;
                                true
                            }
                            None => false,
                        }
                    } {
                        // handled above
                    } else if let Some(cached_bytes) = (!sorted_candidates_path)
                        .then(|| {
                            self.decoded_stored_cache
                                .get(seg_id.as_str())
                                .map(|e| Arc::clone(e.value()))
                        })
                        .flatten()
                    {
                        // Decoded-bytes cache hit: skip open + decompress
                        // entirely.  With the in-section early stop, the
                        // whole per-segment cost is now O(from+size) doc
                        // parses instead of O(section) decompress+parse.
                        let t_scan = std::time::Instant::now();
                        self.scan_stored_section_into(
                            &cached_bytes,
                            query,
                            is_match_all,
                            count_only,
                            materialisation_limit,
                            count_authoritative,
                            &mut total_count,
                            &mut all_hits,
                            &mut seen_ids,
                            pre_filter.as_deref(),
                            sort_topk.as_mut(),
                            None,
                            Some(search_deadline),
                            &mut deadline_exceeded,
                            &mut dbg_walked,
                            &mut dbg_admitted,
                        );
                        dbg_scan_ms += t_scan.elapsed().as_millis() as u64;
                    } else if let Some(warm_slices) = (!sorted_candidates_path)
                        .then(|| {
                            self.stored_slices_cache
                                .get(seg_id.as_str())
                                .map(|e| Arc::clone(e.value()))
                        })
                        .flatten()
                    {
                        // Unsorted scan (bool/range, no implicit sort) reusing
                        // the merge/flush-warmed `StoredSlices`.  `slices.bytes`
                        // IS the decoded stored section — bit-identical to what
                        // `decoded_stored_cache` would hold — so the FIRST
                        // bool/range query after a merge scans it directly
                        // instead of paying a fresh multi-100 MB decompress.
                        // `warm_segment_at_publish` pre-warms `stored_slices_cache`
                        // (the sorted-candidate source) but NOT
                        // `decoded_stored_cache`; without this arm the unsorted
                        // arm re-decompressed the merged segment on its next
                        // touch (live dec=1.1-3.8 s spikes at every merge
                        // completion under the mixed read/write bench).  No
                        // extra memory: the warmed bytes are shared, not copied.
                        let t_scan = std::time::Instant::now();
                        // F2: with warm slices we have the per-doc offset index,
                        // so an UNSORTED pre-filtered query (selective term /
                        // terms / conjunction bool) random-accesses only its
                        // matching positions instead of brace-walking the whole
                        // section — the O(section bytes)→O(|pre_filter|) fix for
                        // a term fanned across every shard-segment. Sorted or
                        // no-pre-filter queries keep the linear scan.
                        match pre_filter.as_deref() {
                            Some(pf) if sort_topk.is_none() => {
                                self.hydrate_prefiltered_unsorted(
                                    &warm_slices,
                                    pf,
                                    query,
                                    is_match_all,
                                    count_only,
                                    materialisation_limit,
                                    count_authoritative,
                                    &mut total_count,
                                    &mut all_hits,
                                    &mut seen_ids,
                                );
                            }
                            _ => {
                                self.scan_stored_section_into(
                                    &warm_slices.bytes,
                                    query,
                                    is_match_all,
                                    count_only,
                                    materialisation_limit,
                                    count_authoritative,
                                    &mut total_count,
                                    &mut all_hits,
                                    &mut seen_ids,
                                    pre_filter.as_deref(),
                                    sort_topk.as_mut(),
                                    None,
                                    Some(search_deadline),
                                    &mut deadline_exceeded,
                                    &mut dbg_walked,
                                    &mut dbg_admitted,
                                );
                            }
                        }
                        dbg_scan_ms += t_scan.elapsed().as_millis() as u64;
                    } else {
                        // Merge-race hardening (2026-07): a snapshot
                        // segment that fails to open is a query error, not
                        // a segment to skip — skipping silently undercounts
                        // hits.total (the observed 798,281-of-932,037 bug).
                        // With the read lease held by `snap`, retired
                        // segment files persist, so this only fires on
                        // genuine corruption.
                        let t_dec = std::time::Instant::now();
                        let seg_reader = self.store.open_segment_arc(&seg_id)?;
                        if let Ok(Some(stored_bytes_raw)) = seg_reader.section(SectionType::Stored)
                        {
                            if let Ok(stored_bytes) =
                                xerj_storage::stored_codec::decode_stored(stored_bytes_raw)
                            {
                                dbg_decode_ms += t_dec.elapsed().as_millis() as u64;
                                let t_scan = std::time::Instant::now();
                                // Record per-doc offsets on the sorted path so
                                // the decompressed section can be cached for
                                // the next query (subject to the budget).
                                let mut offsets: Vec<(u32, u32)> = Vec::new();
                                let want_cache = sorted_candidates_path
                                    && stored_bytes.len() <= u32::MAX as usize
                                    && !self.stored_slices_cache.contains_key(seg_id.as_str());
                                self.scan_stored_section_into(
                                    &stored_bytes,
                                    query,
                                    is_match_all,
                                    count_only,
                                    materialisation_limit,
                                    count_authoritative,
                                    &mut total_count,
                                    &mut all_hits,
                                    &mut seen_ids,
                                    pre_filter.as_deref(),
                                    sort_topk.as_mut(),
                                    if want_cache { Some(&mut offsets) } else { None },
                                    Some(search_deadline),
                                    &mut deadline_exceeded,
                                    &mut dbg_walked,
                                    &mut dbg_admitted,
                                );
                                dbg_scan_ms += t_scan.elapsed().as_millis() as u64;
                                // Cache only a COMPLETE offsets map (a
                                // malformed section bails early) and only
                                // within the retained-bytes budget.
                                if want_cache && offsets.len() as u64 == meta.doc_count {
                                    let slices = StoredSlices {
                                        bytes: stored_bytes,
                                        offsets,
                                    };
                                    let sz = slices.retained_bytes();
                                    if self
                                        .stored_slices_cache_bytes
                                        .load(Ordering::Relaxed)
                                        .saturating_add(sz)
                                        <= stored_slices_cache_budget()
                                        && self
                                            .stored_slices_cache
                                            .insert(seg_id.clone(), Arc::new(slices))
                                            .is_none()
                                    {
                                        self.stored_slices_cache_bytes
                                            .fetch_add(sz, Ordering::Relaxed);
                                    }
                                } else if !want_cache {
                                    // Unsorted path: retain the decompressed
                                    // section so the NEXT query on this
                                    // segment skips the decompress (the
                                    // read-under-write match_all/bool/range
                                    // tail fix — see `decoded_stored_cache`).
                                    let sz = stored_bytes.len() as u64;
                                    if self
                                        .decoded_stored_cache_bytes
                                        .load(Ordering::Relaxed)
                                        .saturating_add(sz)
                                        <= DECODED_STORED_CACHE_BUDGET
                                        && self
                                            .decoded_stored_cache
                                            .insert(seg_id.clone(), Arc::new(stored_bytes))
                                            .is_none()
                                    {
                                        self.decoded_stored_cache_bytes
                                            .fetch_add(sz, Ordering::Relaxed);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        drop(snap);

        // F1: did the bounded collector fill to the cap?  If so the stored-doc
        // scan may have STOPPED early (count_authoritative), so its
        // `total_count` is only a partial tally and must be replaced by the
        // authoritative shortcut count below.  If it did NOT fill to the cap,
        // every match was visited and `total_count` is already EXACT — and it
        // faithfully reflects full query semantics (flattened / subobjects:false
        // / passthrough field resolution the doc-values shortcut can't model),
        // so we keep it and never overwrite.
        phase_marks.push(("segment_loop", phase_t0.elapsed().as_millis() as u64));
        let scan_hit_cap: bool = all_hits.len() >= materialisation_limit;

        // De-duplication is already enforced by `seen_ids` inside the
        // bounded collector (`admit_hit!`), so `all_hits` is already unique.
        // Field-sorted queries collected into the bounded top-N heap instead
        // of `all_hits` (which stays empty on that path); drain it here.  The
        // heap already holds only the GLOBAL top-(from+size) by the sort key,
        // so the downstream `final_hits.sort_by` + `skip(from).take(size)`
        // produce the correct page.
        let final_hits: Vec<Hit> = match sort_topk.take() {
            Some(topk) => topk.into_hits(),
            None => all_hits,
        };

        // --- Fill memtable sources (for hits that don't yet have a source) ---
        let mut final_hits = self.fill_memtable_sources(final_hits).await;

        // --- Apply pinned query: boost pinned IDs to the top ---
        // Assign pinned IDs a score higher than any organic result so they sort first.
        if let QueryNode::Pinned { ids, .. } = query {
            let max_organic_score = final_hits
                .iter()
                .map(|h| h.score)
                .fold(f32::NEG_INFINITY, f32::max);
            let pin_score = (max_organic_score + 1.0).max(1.0);
            for hit in &mut final_hits {
                if ids.iter().any(|pid| pid == &hit.id) {
                    // Give pinned docs a very high score so they sort first.
                    // Use index in the ids list to preserve pinned ordering.
                    let rank = ids.iter().position(|pid| pid == &hit.id).unwrap_or(0);
                    hit.score = pin_score + (ids.len() - rank) as f32;
                }
            }
        }

        // --- Apply function score modifications ---
        // Unwrap `Named` / `Constant` / `Boosted` wrappers so a
        // function_score nested inside a named query still runs.
        // Also peels through a Bool whose ONLY scoring clause is a
        // function_score (filter[] clauses are unscored constraints
        // already enforced by the matcher).
        fn peel_function_score(q: &QueryNode) -> Option<&QueryNode> {
            match q {
                QueryNode::FunctionScore { .. } => Some(q),
                QueryNode::Named { query, .. }
                | QueryNode::Constant { query, .. }
                | QueryNode::Boosted { query, .. } => peel_function_score(query),
                QueryNode::Bool {
                    must,
                    should,
                    must_not,
                    ..
                } => {
                    if !must_not.is_empty() {
                        return None;
                    }
                    // Find the single scoring clause across must+should.
                    // bool.filter is unscored (no contribution) — it's a
                    // pure constraint already enforced by the matcher,
                    // so it doesn't disrupt FunctionScore peeling.
                    let scoring: Vec<&QueryNode> = must.iter().chain(should.iter()).collect();
                    if scoring.len() == 1 {
                        peel_function_score(scoring[0])
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
        if let Some(QueryNode::FunctionScore {
            functions,
            score_mode,
            boost_mode,
            max_boost,
            ..
        }) = peel_function_score(query)
        {
            for (doc_idx, hit) in final_hits.iter_mut().enumerate() {
                let query_score = hit.score;
                let fn_score =
                    apply_function_score(&hit.id, &hit.source, functions, *score_mode, query_score);
                let combined = combine_scores(query_score, fn_score, *boost_mode);
                // ES rejects non-finite or negative scores here with
                // `illegal_argument_exception`. Match the exact reason
                // text so the YAML test's root_cause.reason comparison
                // succeeds.
                if !combined.is_finite() || combined < 0.0 {
                    let label = if combined.is_nan() {
                        "NaN".to_string()
                    } else if combined.is_infinite() {
                        if combined > 0.0 {
                            "Infinity".into()
                        } else {
                            "-Infinity".into()
                        }
                    } else {
                        format!("{combined}")
                    };
                    return Err(EngineError::Common(
                        xerj_common::XerjError::invalid_query(format!(
                            "function score query returned an invalid score: {label} for doc: {doc_idx}; score must be a non-negative real number"
                        )),
                    ));
                }
                hit.score = if let Some(cap) = max_boost {
                    combined.min(*cap)
                } else {
                    combined
                };
            }
        }

        // --- Apply field-value sort (populate hit.sort) ---
        if !request.sort.is_empty() {
            let sort_fields = &request.sort;
            // Populate sort key arrays from source.  `compute_sort_values` is
            // the SAME routine the bounded `SortTopK` collector uses at
            // admission time, so the global top-N selection and this final
            // ordering agree on the exact total order (incl. date-string
            // normalisation).
            for hit in &mut final_hits {
                hit.sort = compute_sort_values(&hit.source, hit.score, &hit.id, sort_fields);
            }
            // Sort by the populated sort keys.
            final_hits.sort_by(|a, b| {
                xerj_query::sort::compare_sort_keys(&a.sort, &b.sort, sort_fields)
                    .then_with(|| a.id.cmp(&b.id))
            });
        } else {
            // Default: sort by score descending, with insertion-order
            // (seq_no ASC) as the secondary key so ties resolve to "doc
            // that arrived first comes first" — matching ES's `_doc`
            // tie-break. The previous tie-break was `_id` ascending
            // which is essentially random for UUID-shaped ids and
            // produced reverse-of-insertion order for unsorted match-all
            // results (see search/380_sort_segments_on_timestamp.yml's
            // "NOT sorted on timestamp" sub-test).
            let seq = |id: &str| self.lookup_seq_no(id).unwrap_or(u64::MAX);
            final_hits.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| seq(&a.id).cmp(&seq(&b.id)))
                    .then_with(|| a.id.cmp(&b.id))
            });
        }

        // --- IDF-weighted rescore for Bool queries with multiple terms ---
        // When the query is Bool(should: [Match(A), Match(B), ...]) or
        // query_string lowered to one, the per-hit memtable score is
        // uniform (sum of tf contributions without IDF). Re-weight using
        // per-term document frequency across the full hit set so rare
        // terms contribute more. This is the difference between "all
        // docs get 1.0" and "docs with rare terms rank higher" for
        // diversified_sampler / top_hits / 115_multi / interval_query
        // ordering.
        if !final_hits.is_empty() && request.sort.is_empty() && query_uses_bool_text(&request.query)
        {
            let field_term_pairs = collect_match_field_terms(&request.query);
            if !field_term_pairs.is_empty() {
                let n_docs = final_hits.len() as f32;
                // For each (field, term) clause, compute its doc frequency.
                let term_idf: Vec<f32> = field_term_pairs
                    .iter()
                    .map(|(field, term, _boost)| {
                        let df = final_hits
                            .iter()
                            .filter(|h| match_term_frequency(&h.source, field, term) > 0.0)
                            .count() as f32;
                        if df == 0.0 {
                            0.0
                        } else {
                            // BM25-style IDF: ln(1 + (N - df + 0.5) / (df + 0.5))
                            (1.0 + (n_docs - df + 0.5) / (df + 0.5)).ln()
                        }
                    })
                    .collect();
                // Rescore each hit: score = Σ boost(i) · idf(i) · (1 + ln(1 + tf(i)))
                for hit in final_hits.iter_mut() {
                    let mut score = 0.0f32;
                    for (i, (field, term, clause_boost)) in field_term_pairs.iter().enumerate() {
                        let tf = match_term_frequency(&hit.source, field, term);
                        if tf > 0.0 {
                            score += clause_boost * term_idf[i] * (1.0 + (1.0 + tf).ln());
                        }
                    }
                    if score > 0.0 {
                        hit.score = score;
                    }
                }
                // Re-sort by the new scores.
                final_hits.sort_by(|a, b| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.id.cmp(&b.id))
                });
            }
        }

        // --- Score normalization: normalize BM25 scores to [0, max_score] ---
        // When scores are extremely low (BM25 near-zero), fall back to simple TF-IDF scoring.
        if !final_hits.is_empty() && request.sort.is_empty() {
            let max_score = final_hits
                .iter()
                .map(|h| h.score)
                .fold(f32::NEG_INFINITY, f32::max);
            // TF-IDF fallback: when all BM25 scores are negligibly small, recompute
            // using a simpler term-frequency heuristic for more useful ranking.
            if max_score > 0.0 && max_score < 0.001 {
                let query_terms = extract_query_text(&request.query)
                    .map(|t| {
                        t.split_whitespace()
                            .map(|s| s.to_lowercase())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if !query_terms.is_empty() {
                    let n_docs = final_hits.len() as f32;
                    // Pre-compute lowercased source strings to avoid double-borrowing.
                    let src_strings: Vec<String> = final_hits
                        .iter()
                        .map(|h| h.source.to_string().to_lowercase())
                        .collect();
                    // Compute per-term document frequencies from the snapshot.
                    let new_scores: Vec<f32> = src_strings
                        .iter()
                        .map(|src_str| {
                            let mut tfidf = 0.0f32;
                            for term in &query_terms {
                                let tf = src_str.matches(term.as_str()).count() as f32;
                                let df = src_strings
                                    .iter()
                                    .filter(|s| s.contains(term.as_str()))
                                    .count() as f32;
                                if tf > 0.0 && df > 0.0 {
                                    tfidf += tf.sqrt() * (1.0 + (n_docs / df).ln());
                                }
                            }
                            tfidf
                        })
                        .collect();
                    for (hit, score) in final_hits.iter_mut().zip(new_scores) {
                        hit.score = score;
                    }
                    // Re-sort with new TF-IDF scores.
                    final_hits.sort_by(|a, b| {
                        b.score
                            .partial_cmp(&a.score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then_with(|| a.id.cmp(&b.id))
                    });
                }
            }
        }

        // --- Apply collapse BEFORE rescore (ES order) ---
        // ES applies field collapsing on the initial query results,
        // selecting the top-scoring representative per group, and only
        // THEN re-scores the survivors. Rescoring before collapse would
        // change which doc per group "wins" and produce different
        // results for tests that explicitly check this interaction
        // (search/112_field_collapsing_with_rescore).
        if request.collapse.is_some() && !request.rescore.is_empty() {
            if let Some(collapse) = &request.collapse {
                final_hits = apply_collapse_with_inner(
                    final_hits,
                    &collapse.field,
                    collapse.inner_hits.as_ref(),
                );
            }
        }

        // --- Apply rescore ---
        // For each rescore stage, re-score the top window_size hits using the secondary query.
        // Final score = original_score * query_weight + rescore_score * rescore_query_weight
        if !request.rescore.is_empty() {
            // Sort by score before applying rescore so we work on the correct top-N.
            final_hits.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.id.cmp(&b.id))
            });

            for rescore_stage in &request.rescore {
                apply_rescore(&mut final_hits, rescore_stage);
                // ES re-sorts between chained rescore stages so the
                // next stage's `window_size` applies to the top-N
                // of the just-rescored order, not the pre-rescore
                // BM25 order.
                final_hits.sort_by(|a, b| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.id.cmp(&b.id))
                });
            }

            // Re-sort after rescoring.
            final_hits.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.id.cmp(&b.id))
            });
        }

        // --- match_all total = authoritative live doc count ---
        // The size>0 segment scan tallies stored doc-versions (and the
        // count-shortcut/precomputed paths use the version-doubling atomic), so
        // an UPDATE would inflate hits.total. For an unfiltered match_all the
        // true total is the live doc count (one entry per `_id`). min_score
        // below still adjusts from this corrected base if present.
        if is_match_all {
            total_count = self.live_doc_count();
        } else if !count_only
            && !query_needs_fts
            && (scan_hit_cap || sort_candidates_narrowed)
            && !deletes_present
        {
            // `!count_only` (not `size > 0`): a `size:0 + aggs` request also
            // materialises hits (count_only=false) and therefore also runs
            // the scan in bounded early-break mode when the count is
            // authoritative — its partial tally needs the same overwrite.
            // Pure count-only requests never materialise, so scan_hit_cap
            // is always false for them and this branch can't misfire.
            // F1: the non-FTS stored scan filled its bounded collector and may
            // have stopped early, so its `total_count` is partial — replace it
            // with the authoritative doc-values / FST shortcut count.  We only
            // do this when the scan actually hit the cap AND there are no
            // deletes (the shortcut is delete-blind; see the hoisted
            // `deletes_present` gate — with deletes present the scan ran in full
            // delete-aware counting mode so `total_count` is already exact).
            // For small result sets
            // the scan already produced the EXACT, full-semantics count above
            // and must be trusted (the shortcut can under-count flattened /
            // subobjects:false / passthrough fields, returning 0 where the scan
            // finds matches).  The `sc >= final_hits.len()` guard is a final
            // sanity check — the true total can never be below the number of
            // distinct hits we actually materialised.
            if let Some(sc) = shortcut_count {
                if sc >= final_hits.len() as u64 {
                    total_count = sc;
                }
            }
        }

        // --- Apply min_score threshold ---
        // ES drops hits below min_score before pagination + aggregations
        // + total counting. Compute the drop count and subtract from
        // total_count so hits.total reflects the post-filter count.
        if let Some(min_score) = request.min_score {
            let before = final_hits.len();
            final_hits.retain(|h| (h.score as f64) >= min_score);
            let dropped = before - final_hits.len();
            total_count = total_count.saturating_sub(dropped as u64);
        }

        // --- Collect matched_queries for named queries ---
        for hit in &mut final_hits {
            // Inject `_id` so Ids queries nested inside named wrappers can
            // resolve their membership check from the hit's metadata.
            let source_with_id = if hit.source.get("_id").is_some() {
                hit.source.clone()
            } else if let Some(obj) = hit.source.as_object() {
                let mut o = obj.clone();
                o.insert("_id".to_string(), Value::String(hit.id.clone()));
                Value::Object(o)
            } else {
                hit.source.clone()
            };
            hit.matched_queries = collect_matched_queries(&request.query, &source_with_id);
        }

        // --- Apply search_after cursor (skip hits up to and including the cursor) ---
        // Field-sorted queries already rejected cursor-≤ hits INSIDE the
        // bounded collector (`SortTopK::would_admit`), so for that path this
        // block is a no-op safety net (position 0 matches immediately).  It
        // remains load-bearing for the score-sort cursor path, which has no
        // field collector.  The cursor was normalized ONCE into `after_norm`
        // before collection (date strings → epoch numbers).
        if let Some(ref after_values) = request.search_after {
            if !after_values.is_empty() {
                let after_values_norm: &[Value] = after_norm.as_deref().unwrap_or(&[]);
                let after_values_ref: &[Value] = after_values_norm;
                // Find the first hit whose sort values are strictly "after" the cursor.
                // For sorted results: skip until we find a hit where sort > after_values.
                // For score-only sort: after_values[0] is the score cursor.
                let pos = if !request.sort.is_empty() {
                    final_hits.iter().position(|hit| {
                        xerj_query::sort::compare_sort_keys(
                            &hit.sort,
                            after_values_ref,
                            &request.sort,
                        ) == std::cmp::Ordering::Greater
                    })
                } else {
                    // Default score sort: find where score < cursor_score (descending).
                    let cursor_score = after_values
                        .first()
                        .and_then(|v| v.as_f64())
                        .unwrap_or(f64::NEG_INFINITY);
                    // Tie-break on _id if provided (second element).
                    let cursor_id = after_values.get(1).and_then(|v| v.as_str()).unwrap_or("");
                    final_hits.iter().position(|hit| {
                        let hit_score = hit.score as f64;
                        if (hit_score - cursor_score).abs() < f64::EPSILON {
                            // Same score: tie-break by id lexicographically.
                            hit.id.as_str() > cursor_id
                        } else {
                            hit_score < cursor_score
                        }
                    })
                };
                if let Some(p) = pos {
                    final_hits = final_hits.split_off(p);
                } else {
                    // All hits are at or before the cursor — return nothing.
                    final_hits.clear();
                }
            }
        }

        // --- Compute total with track_total_hits semantics ---
        // `total_count` is the exact number of documents that matched, as
        // tallied by `admit_hit!` and the fast-paths — we do NOT rely on
        // `final_hits.len()` anymore because the collector is bounded.
        let (total, total_relation) = match request.track_total_hits {
            TrackTotalHits::True => (total_count, TotalHitsRelation::Eq),
            TrackTotalHits::False => {
                let cap = size as u64;
                if total_count > cap {
                    (cap, TotalHitsRelation::Gte)
                } else {
                    (total_count, TotalHitsRelation::Eq)
                }
            }
            TrackTotalHits::Limit(limit) => {
                if total_count > limit {
                    (limit, TotalHitsRelation::Gte)
                } else {
                    (total_count, TotalHitsRelation::Eq)
                }
            }
        };

        // --- Collect background docs for significant_terms (all memtable docs) ---
        // Used as the corpus for background frequency calculation.
        // Skip this when the fast path already served the aggs (precomputed_aggs
        // is Some), or when the request has no significant_terms agg.  Cloning
        // every memtable doc into a Vec<Value> is the dominant overhead for
        // `size:0 + agg` queries on a 200 k-doc memtable.
        // Build the full corpus (live memtable + every segment's stored
        // section) whenever we will need a JSON-scan aggregation fallback.
        //
        // CORRECTNESS: aggregations must see EVERY matching document, not just
        // the `materialisation_limit`-capped hit window (`final_hits`). The
        // segment doc-values fast paths (`try_aggs_fast*`) are disabled under
        // the sharded memtable (M5.1), so for `size:0 + aggs` the agg value is
        // computed by `run_aggs_with_all` — which previously ran over only the
        // first ~256 materialised hits, silently under-counting stats/terms on
        // any index larger than the cap. We hand it the full corpus instead.
        //
        // This corpus also serves as the `significant_terms` / `global` /
        // `min_doc_count:0` background, so it must exist after `_refresh` has
        // flushed the memtable to segments.
        let need_full_corpus = precomputed_aggs.is_none() && request.aggs.is_some();
        let all_docs: Vec<Value> = if need_full_corpus {
            // `_id` is injected onto each source so `top_hits` / `_id`-keyed
            // aggs over the corpus still work (the fast path never provided it).
            let mut docs: Vec<Value> = self
                .memtable
                .all_docs_with_sources()
                .into_iter()
                .map(|(id, mut v)| {
                    if let Some(o) = v.as_object_mut() {
                        o.entry("_id".to_string())
                            .or_insert_with(|| Value::String(id));
                    }
                    v
                })
                .collect();
            let snap_bg = self.store.snapshot();
            for seg in &snap_bg.segments {
                // Cooperative deadline: the full-corpus assembly is the
                // most expensive path in the engine (decompress + parse
                // EVERY stored doc).  Stop pulling further segments once
                // past the request timeout — aggs become partial and the
                // response carries `timed_out: true`.
                if std::time::Instant::now() >= search_deadline {
                    deadline_exceeded = true;
                    break;
                }
                // Merge-race hardening (2026-07): open failures are
                // errors, not skips — a skipped segment silently drops
                // its docs from every aggregation bucket.  The read
                // lease held by `snap_bg` keeps retired segment files
                // on disk, so this only fires on genuine corruption.
                let reader = self.store.open_segment_arc(&seg.id)?;
                let stored_bytes_raw = match reader.section(SectionType::Stored) {
                    Ok(Some(b)) => b,
                    _ => continue,
                };
                let stored_bytes = match xerj_storage::stored_codec::decode_stored(stored_bytes_raw)
                {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                // serde_json — see ffd49ac. simd_json silently corrupts
                // some M7 raw-bytes flush payloads.
                if let Ok(arr) = serde_json::from_slice::<Vec<Value>>(&stored_bytes) {
                    for d in arr {
                        // Skip tombstoned docs.
                        let id_ref = d.get("_id").and_then(Value::as_str).unwrap_or("");
                        if let Some(ver) = self.store.version_map.get(id_ref) {
                            if ver.deleted {
                                continue;
                            }
                        }
                        let id_owned = id_ref.to_string();
                        let mut src = d.get("_source").cloned().unwrap_or(d);
                        if let Some(o) = src.as_object_mut() {
                            if !id_owned.is_empty() {
                                o.entry("_id".to_string())
                                    .or_insert(Value::String(id_owned));
                            }
                        }
                        docs.push(src);
                    }
                }
            }
            // Enrich corpus docs with hit-level metadata so score/seq-sensitive
            // sub-aggs reproduce ES semantics: `top_hits` default-sorts by
            // `_score` desc then `_seq_no` asc and surfaces `_index`/`_score`/
            // `matched_queries`; `diversified_sampler` keeps the top
            // max_docs_per_value per field value BY SCORE. Raw stored sources
            // carry none of these, so without this top_hits returns docs in
            // arbitrary corpus order (and Null index/score/matched_queries) and
            // the sampler keeps the wrong doc. `_score`/`matched_queries` come
            // from the already-scored hit window (`final_hits`).
            {
                let hit_meta: std::collections::HashMap<&str, &Hit> =
                    final_hits.iter().map(|h| (h.id.as_str(), h)).collect();
                for d in docs.iter_mut() {
                    if let Some(o) = d.as_object_mut() {
                        o.entry("_index".to_string())
                            .or_insert_with(|| Value::String(self.name.to_string()));
                        let id = o.get("_id").and_then(Value::as_str).map(str::to_string);
                        if let Some(id) = id {
                            if let Some(seq) = self.lookup_seq_no(&id) {
                                o.entry("_seq_no".to_string())
                                    .or_insert_with(|| serde_json::json!(seq));
                            }
                            if let Some(h) = hit_meta.get(id.as_str()) {
                                o.insert("_score".to_string(), serde_json::json!(h.score));
                                if !h.matched_queries.is_empty() {
                                    o.insert(
                                        "_matched_queries".to_string(),
                                        Value::Array(
                                            h.matched_queries
                                                .iter()
                                                .cloned()
                                                .map(Value::String)
                                                .collect(),
                                        ),
                                    );
                                }
                            }
                        }
                    }
                }
            }
            docs
        } else {
            vec![]
        };

        phase_marks.push(("hydrate+corpus", phase_t0.elapsed().as_millis() as u64));
        // --- Run aggregations over the full matched set (before pagination) ---
        let agg_start = std::time::Instant::now();
        let agg_result = if let Some(aggs_def) = &request.aggs {
            // If the pre-segment-loop fast path already computed the agg
            // value, consume it here and skip every slower path.  This is
            // the hot path for `size:0 + match_all + aggs` queries.
            if let Some(r) = precomputed_aggs.take() {
                Some(r)
            } else {
                let is_match_all = matches!(query, QueryNode::MatchAll);

                // Memtable + segments DV fast path (non-size=0 MatchAll still
                // benefits if the stored scan hasn't materialized sources yet).
                let mut agg_result_opt: Option<Value> = None;
                if is_match_all {
                    let snap2 = self.store.snapshot();
                    agg_result_opt = try_aggs_fast_with_segments(
                        aggs_def,
                        &snap2,
                        &segments_dir,
                        &self.memtable,
                    )
                    .await;
                    drop(snap2);
                }

                // Propagate outer query terms into the aggs thread-local so
                // run_top_hits can build highlights without requiring the
                // aggs layer to know about QueryNode trees.
                let hl_terms = extract_highlight_terms(query);
                crate::aggs::set_outer_query_terms(hl_terms);
                // Also propagate (field, term) pairs for explain output.
                let ft_pairs: Vec<(String, String)> = collect_match_field_terms(query)
                    .into_iter()
                    .map(|(f, t, _boost)| (f, t))
                    .collect();
                crate::aggs::set_outer_query_field_terms(ft_pairs);

                let agg_result = if let Some(r) = agg_result_opt {
                    r
                } else {
                    // Memtable-only DV fast path (currently a no-op under the
                    // sharded memtable — returns None; kept wired for when the
                    // cross-shard column aggregator lands).
                    let can_use_dv_fast = is_match_all || mem_dv_doc_indices.is_some();
                    let dv_result = if can_use_dv_fast {
                        try_aggs_fast(aggs_def, mem_dv_doc_indices.as_deref(), &self.memtable).await
                    } else {
                        None
                    };
                    if let Some(r) = dv_result {
                        r
                    } else if need_full_corpus {
                        // JSON-scan over the FULL matching set (correctness fix).
                        // match_all -> the whole corpus; filtered query -> keep only
                        // docs the query matches (same matcher the stored-doc scan
                        // uses), so aggregations reflect every match rather than the
                        // `materialisation_limit`-capped hit window.
                        let fg_owned: Vec<Value>;
                        let fg: &[Value] = if is_match_all {
                            &all_docs[..]
                        } else {
                            fg_owned = all_docs
                                .iter()
                                .filter(|d| doc_matches_query(query, d))
                                .cloned()
                                .collect();
                            &fg_owned[..]
                        };
                        // `doc_matches_query` is a boolean matcher and ignores
                        // scoring, so when `min_score` is set the agg foreground
                        // must additionally drop docs whose score fell below it.
                        // `final_hits` is already min_score-filtered (above), so
                        // intersect by `_id`.
                        let fg_min: Vec<Value>;
                        let fg: &[Value] = if request.min_score.is_some() {
                            let keep: std::collections::HashSet<&str> =
                                final_hits.iter().map(|h| h.id.as_str()).collect();
                            fg_min = fg
                                .iter()
                                .filter(|d| {
                                    d.get("_id")
                                        .and_then(Value::as_str)
                                        .is_some_and(|id| keep.contains(id))
                                })
                                .cloned()
                                .collect();
                            &fg_min[..]
                        } else {
                            fg
                        };
                        let bg = if all_docs.is_empty() {
                            fg
                        } else {
                            &all_docs[..]
                        };
                        run_aggs_with_all(aggs_def, fg, bg)
                    } else {
                        // Safety net: corpus not built — aggregate the hit window.
                        let sources: Vec<Value> = final_hits
                            .iter()
                            .map(|h| {
                                let mut s = h.source.clone();
                                if let Some(obj) = s.as_object_mut() {
                                    obj.insert("_score".to_string(), serde_json::json!(h.score));
                                    obj.insert("_id".to_string(), Value::String(h.id.clone()));
                                    obj.insert(
                                        "_index".to_string(),
                                        Value::String(self.name.to_string()),
                                    );
                                    if let Some(seq) = self.lookup_seq_no(&h.id) {
                                        obj.insert("_seq_no".to_string(), serde_json::json!(seq));
                                    }
                                    if !h.matched_queries.is_empty() {
                                        obj.insert(
                                            "_matched_queries".to_string(),
                                            Value::Array(
                                                h.matched_queries
                                                    .iter()
                                                    .cloned()
                                                    .map(Value::String)
                                                    .collect(),
                                            ),
                                        );
                                    }
                                }
                                s
                            })
                            .collect();
                        let bg = if all_docs.is_empty() {
                            &sources[..]
                        } else {
                            &all_docs[..]
                        };
                        run_aggs_with_all(aggs_def, &sources, bg)
                    }
                };
                Some(agg_result)
            }
        } else {
            None
        };
        let agg_nanos = agg_start.elapsed().as_nanos() as u64;

        // Population max over ALL matched docs (pre-collapse, pre-page) so the
        // API can report ES `max_score` with collapse + track_scores correctly
        // (ES reports the max over the whole result set, not just the collapsed
        // top hit per group).
        let population_max_score: Option<f32> = final_hits
            .iter()
            .map(|h| h.score)
            .fold(None, |acc: Option<f32>, s| {
                Some(acc.map_or(s, |m| m.max(s)))
            });

        // --- Apply field collapsing (deduplicate by field value, keep top hit per value) ---
        // Skipped here when rescore was present (we already collapsed before rescore).
        let final_hits = if let Some(collapse) = &request.collapse {
            if request.rescore.is_empty() {
                apply_collapse_with_inner(final_hits, &collapse.field, collapse.inner_hits.as_ref())
            } else {
                final_hits
            }
        } else {
            final_hits
        };

        // --- Apply pagination (size=0 means no hits returned) ---
        let page: Vec<Hit> = if size == 0 {
            Vec::new()
        } else {
            final_hits.into_iter().skip(from).take(size).collect()
        };

        // --- Apply _source filtering ---
        let page = apply_source_filter(page, &request.source);

        // --- Apply highlighting ---
        let page = if let Some(hl_req) = &request.highlight {
            apply_highlight(page, hl_req, query)
        } else {
            page
        };

        // --- Build profile data if requested ---
        let profile = if request.profile {
            let query_desc = format!("{:?}", query).chars().take(120).collect::<String>();
            Some(serde_json::json!({
                "shards": [{
                    "id": "0",
                    "searches": [{
                        "query": [{
                            "type": "MatchQuery",
                            "description": query_desc,
                            "time_in_nanos": agg_nanos,
                            "children": []
                        }]
                    }],
                    "aggregations": [{
                        "type": "Aggregations",
                        "description": "aggs",
                        "time_in_nanos": agg_nanos
                    }]
                }]
            }))
        } else {
            None
        };

        let inner_ms = phase_t0.elapsed().as_millis() as u64;
        if inner_ms >= phase_log_threshold_ms() {
            phase_marks.push(("aggs+page", inner_ms));
            // Cumulative marks → per-phase deltas for the log line.
            let mut prev = 0u64;
            let breakdown: Vec<String> = phase_marks
                .iter()
                .map(|(name, at)| {
                    let d = at.saturating_sub(prev);
                    prev = *at;
                    format!("{name}={d}ms")
                })
                .collect();
            warn!(
                total_ms = inner_ms,
                index = self.name.as_str(),
                phases = %breakdown.join(" "),
                gates = %format!(
                    "needs_fts={} shortcut={:?} deletes={} ghosts={} total={} lim={} segs={} dec={}ms scan={}ms ftsopen={}ms walked={} admitted={} allhits_cap={} memhits={} memseen={} memtotal={} memarm={} sorttopk={}",
                    query_needs_fts,
                    shortcut_count,
                    deletes_present,
                    self.store.version_map.ghost_events(),
                    total,
                    materialisation_limit,
                    dbg_segs,
                    dbg_decode_ms,
                    dbg_scan_ms,
                    dbg_fts_ms,
                    dbg_walked,
                    dbg_admitted,
                    scan_hit_cap,
                    dbg_mem_hits,
                    dbg_mem_seen,
                    dbg_mem_total,
                    dbg_mem_arm,
                    dbg_sort_topk
                ),
                "slow search_inner phase breakdown"
            );
        }

        Ok(SearchResult {
            hits: page,
            total: TotalHits {
                value: total,
                relation: total_relation,
            },
            took_ms: 0,
            aggs: agg_result,
            // Set when any cooperative deadline check fired: the hits /
            // total / aggs above are partial, exactly like an ES shard
            // timeout.  `search()` refuses to cache timed-out results.
            timed_out: deadline_exceeded,
            profile,
            max_score: population_max_score,
        })
    }

    // ── Flush ─────────────────────────────────────────────────────────────────

    /// Flush the memtable to a new segment on disk, then build the FTS index.
    pub async fn flush(&self) -> Result<()> {
        let field_configs = {
            let schema = self.schema.read().await;
            build_fts_field_configs(&schema.schema)
        };
        // M5.15 — PARALLEL final flush.
        //
        // Pre-M5.15 the user-visible `flush()` walked every shard in
        // sequence (`for shard_idx in 0..N { do_flush_shard(...).await }`)
        // which made the end-of-file flush in the CLI indexer dominate
        // the wall time: 16 shards × ~1 s/shard = 16 s of serial drain
        // at EOF on a 2 GB memtable, dragging the 20 M benchmark down
        // from 1.0 M docs/s peak to as low as 480 k docs/s sustained.
        //
        // Each shard drains into an independent segment with no
        // cross-shard dependency, so we spawn one tokio task per
        // shard and join them concurrently.  The `flush_sema` still
        // bounds global flush concurrency against ingest-time
        // background flushes on other indices.
        let n_shards = self.memtable.shard_count();
        let mut shard_futures = Vec::with_capacity(n_shards);
        for shard_idx in 0..n_shards {
            let sema = Arc::clone(&self.flush_sema);
            let store = Arc::clone(&self.store);
            let memtable = Arc::clone(&self.memtable);
            let registry = Arc::clone(&self.registry);
            let data_dir = self.data_dir.clone();
            let field_configs = field_configs.clone();
            let warm_caches = self.publish_warm_caches();
            shard_futures.push(tokio::spawn(async move {
                let permit = sema.acquire_owned().await.ok();
                let permit_cell = Arc::new(std::sync::Mutex::new(permit));
                let permit_cell_cb = Arc::clone(&permit_cell);
                let on_drained = move || {
                    if let Ok(mut guard) = permit_cell_cb.lock() {
                        let _ = guard.take();
                    }
                };
                let result = do_flush_shard(
                    shard_idx,
                    store,
                    memtable,
                    registry,
                    data_dir,
                    field_configs,
                    on_drained,
                    warm_caches,
                )
                .await;
                // Defensive: in case on_drained didn't fire.
                if let Ok(mut guard) = permit_cell.lock() {
                    let _ = guard.take();
                }
                result
            }));
        }
        let mut result: Result<()> = Ok(());
        for fut in shard_futures {
            match fut.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    result = Err(e);
                }
                Err(e) => {
                    tracing::warn!("flush shard task panicked: {e}");
                }
            }
        }
        // Force a global WAL checkpoint + rotate + prune at the user-
        // visible flush boundary.  `finalize_flush_with_publisher` now
        // time-gates this work (1 s window) on the hot per-shard path;
        // the final flush must bypass the gate so the CLI session's
        // last segment is definitely checkpointed when the CLI exits.
        {
            let seq = self.store.current_seq_no();
            if seq > 0 {
                let _ = self.store.force_wal_maintenance(seq.saturating_sub(1));
            }
        }
        // Flushing moves data from memtable → segments, which changes
        // what the shortcut count paths compute.  Bump the dataset
        // version so the response cache invalidates. P3.2: the new
        // segments are immutable and don't invalidate any EXISTING
        // segment's cached doc-values / stored fields, so we no longer
        // clear those per-segment caches here — the version bump alone
        // makes the query_cache miss.
        self.dataset_version.fetch_add(1, Ordering::Release);
        // Persist the HNSW graph alongside the segment durability
        // event. v0.6.2 — pre-flush the graph could be reconstructed
        // from the WAL on restart (slow); post-flush the WAL has been
        // checkpointed past those vector inserts, so without this save
        // a crash would lose the graph entirely. Failures are logged
        // (warn!) inside save_hnsw_to_disk and do not fail the flush
        // — losing a graph snapshot is recoverable; failing the flush
        // is not.
        let _ = self.save_hnsw_to_disk().await;
        result
    }

    /// Check if the memtable exceeds the configured thresholds.
    pub async fn needs_flush(&self) -> bool {
        let mem = &*self.memtable;
        mem.doc_count() >= self.flush_doc_threshold || mem.size_bytes() >= self.flush_byte_threshold
    }

    pub fn memtable_bytes(&self) -> usize {
        self.memtable.size_bytes()
    }

    pub fn flush_threshold(&self) -> usize {
        self.flush_byte_threshold
    }

    // ── Stats ─────────────────────────────────────────────────────────────────

    /// Return statistics for this index.
    /// Authoritative live document count: one entry per `_id` in the version
    /// map, excluding tombstones. The `doc_count` atomic doubles as the version
    /// generator (bumped on every write, so an UPDATE inflates it by 1), and
    /// summing per-segment `doc_count` includes superseded versions until a
    /// merge — both over-count after updates. The version map holds exactly one
    /// live entry per `_id`, so this is the true count ES clients expect.
    #[inline]
    pub fn live_doc_count(&self) -> u64 {
        self.store.version_map.live_count() as u64
    }

    pub async fn stats(&self) -> IndexStats {
        let snap = self.store.snapshot();
        let segment_count = snap.segments.len();
        drop(snap);

        let _mem = &*self.memtable;
        let schema = self.schema.read().await;

        // Collect per-field encoding info from the memtable's smart analyzer.
        // M5.1 — aggregated across shards; take the union of encodings.
        let field_encodings_map = self.memtable.aggregated_field_encodings();
        let field_encodings = field_encodings_map
            .iter()
            .map(|(field, encoding)| {
                use xerj_compress::field_codec::FieldEncoding;
                let encoding_name = match encoding {
                    FieldEncoding::BitsetEnum { .. } => "bitset_enum",
                    FieldEncoding::DeltaTimestamp { .. } => "delta_timestamp",
                    FieldEncoding::PackedIp { .. } => "packed_ip",
                    FieldEncoding::UrlTemplate { .. } => "url_template",
                    FieldEncoding::Varint { .. } => "varint",
                    FieldEncoding::Dictionary { .. } => "dictionary",
                    FieldEncoding::RawString { .. } => "raw_string",
                    FieldEncoding::Bitpacked { .. } => "bitpacked",
                    FieldEncoding::FixedPrecision { .. } => "fixed_precision",
                };
                let bpv = encoding.bytes_per_value();
                let ratio = encoding.compression_ratio_vs_raw();
                let raw_bpv = bpv * ratio;
                let distinct = match encoding {
                    FieldEncoding::BitsetEnum { values, .. } => values.len(),
                    FieldEncoding::Dictionary { dict, .. } => dict.len(),
                    FieldEncoding::RawString { values } => {
                        let set: std::collections::HashSet<&String> = values.iter().collect();
                        set.len()
                    }
                    _ => 0,
                };
                FieldEncodingInfo {
                    field: field.clone(),
                    encoding: encoding_name.to_string(),
                    bytes_per_value: bpv,
                    raw_bytes_per_value: raw_bpv,
                    compression_ratio: ratio,
                    distinct_values: distinct,
                }
            })
            .collect();

        IndexStats {
            name: self.name.to_string(),
            doc_count: self.live_doc_count(),
            segment_count,
            memtable_doc_count: self.memtable.doc_count(),
            memtable_size_bytes: self.memtable.size_bytes(),
            schema_version: schema.schema.version,
            field_count: schema.schema.field_count(),
            query_count: self.metric_query_count.load(Ordering::Relaxed),
            query_total_ms: self.metric_query_total_ms.load(Ordering::Relaxed),
            index_count: self.metric_index_count.load(Ordering::Relaxed),
            index_total_ms: self.metric_index_total_ms.load(Ordering::Relaxed),
            field_encodings,
        }
    }

    /// Return a snapshot of the current index segments.
    ///
    /// Exposes segment metadata (id, doc_count, size_bytes, seq_no range) for
    /// use by the `_segments` ES-compat API.
    pub fn store_snapshot(&self) -> xerj_storage::index_store::IndexSnapshot {
        let guard = self.store.snapshot();
        (**guard).clone()
    }

    /// Estimate memory usage of this index in bytes.
    ///
    /// Includes:
    /// - Memtable: doc count × ~512 bytes estimated per entry (terms + source JSON overhead)
    /// - Version map entries: doc count × ~64 bytes estimated per entry
    /// - Schema: field count × ~128 bytes per field definition
    pub async fn memory_usage_bytes(&self) -> usize {
        const BYTES_PER_MEMTABLE_ENTRY: usize = 512;
        const BYTES_PER_VERSION_MAP_ENTRY: usize = 64;
        const BYTES_PER_SCHEMA_FIELD: usize = 128;

        let mem = &*self.memtable;
        let memtable_size = mem
            .size_bytes()
            .max(mem.doc_count() * BYTES_PER_MEMTABLE_ENTRY);

        let version_map_count = self.store.version_map.len();
        let version_map_size = version_map_count * BYTES_PER_VERSION_MAP_ENTRY;

        let schema = self.schema.read().await;
        let schema_size = schema.schema.field_count() * BYTES_PER_SCHEMA_FIELD;
        drop(schema);

        memtable_size + version_map_size + schema_size
    }

    // ── Settings ──────────────────────────────────────────────────────────────

    /// Get the index-level settings.
    pub async fn get_settings(&self) -> Value {
        self.settings.read().await.clone()
    }

    /// Update the index-level settings.
    pub async fn put_settings(&self, new_settings: Value) -> Result<()> {
        // Warn if >1 shard requested.
        if let Some(n) = new_settings
            .pointer("/index/number_of_shards")
            .and_then(Value::as_u64)
        {
            if n > 1 {
                tracing::warn!(
                    shards = n,
                    index = self.name.as_str(),
                    "xerj is single-shard; number_of_shards={} is ignored",
                    n
                );
            }
        }
        let path = self.data_dir.join("settings.json");
        let bytes = serde_json::to_vec_pretty(&new_settings)?;
        std::fs::write(&path, bytes).map_err(EngineError::Io)?;
        *self.settings.write().await = new_settings;
        Ok(())
    }

    // ── Index blocks ──────────────────────────────────────────────────────────

    /// Returns true if the write block is set on this index.
    pub async fn is_write_blocked(&self) -> bool {
        let settings = self.settings.read().await;
        settings
            .pointer("/index/blocks/write")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// Returns true if the read block is set on this index.
    pub async fn is_read_blocked(&self) -> bool {
        let settings = self.settings.read().await;
        settings
            .pointer("/index/blocks/read")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// Set a named block on the index (`read_only`, `read_only_allow_delete`, `write`, `metadata`, `read`).
    ///
    /// Stores as `index.blocks.<block_name> = true` in the settings.
    pub async fn set_block(&self, block_name: &str) -> Result<()> {
        let mut settings = {
            let guard = self.settings.read().await;
            guard.clone()
        };

        // Ensure nested structure: settings["index"]["blocks"][block_name] = true
        if settings.is_null() {
            settings = Value::Object(serde_json::Map::new());
        }
        let obj = settings.as_object_mut().unwrap();
        let index_obj = obj
            .entry("index")
            .or_insert_with(|| Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .unwrap();
        let blocks_obj = index_obj
            .entry("blocks")
            .or_insert_with(|| Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .unwrap();
        blocks_obj.insert(block_name.to_string(), Value::Bool(true));

        // Handle aliases: read_only also sets both read + write blocks.
        if block_name == "read_only" {
            blocks_obj.insert("read".to_string(), Value::Bool(true));
            blocks_obj.insert("write".to_string(), Value::Bool(true));
        }
        if block_name == "read_only_allow_delete" {
            blocks_obj.insert("read".to_string(), Value::Bool(true));
            // write is NOT blocked (only delete is allowed)
        }

        let path = self.data_dir.join("settings.json");
        let bytes = serde_json::to_vec_pretty(&settings)?;
        write_file_atomic(&path, &bytes).map_err(EngineError::Io)?;
        *self.settings.write().await = settings;
        Ok(())
    }

    // ── Schema ────────────────────────────────────────────────────────────────

    /// Get the current schema.
    pub async fn schema(&self) -> Schema {
        self.schema.read().await.schema.clone()
    }

    /// Add a field to the schema.
    pub async fn add_field(&self, field: FieldConfig) -> Result<()> {
        let mut schema = self.schema.write().await;
        schema.add_field(field)?;
        self.save_schema(&schema).await?;
        Ok(())
    }

    // ── Suggest helpers ───────────────────────────────────────────────────────

    /// Return all indexed (term, doc_frequency) pairs for a given field.
    ///
    /// Combines terms from:
    /// 1. The active memtable's inverted index.
    /// 2. All frozen memtables.
    /// 3. On-disk FTS segments (via FST scan).
    ///
    /// This is used by the term suggester to find candidates via Levenshtein
    /// distance, and by the completion suggester for prefix autocomplete.
    pub async fn get_all_terms_for_field(&self, field: &str) -> Vec<(String, usize)> {
        let mut term_counts: HashMap<String, usize> = HashMap::new();

        // 1. Active memtable.
        {
            let mem = &*self.memtable;
            for (term, freq) in mem.all_terms_for_field(field) {
                *term_counts.entry(term).or_insert(0) += freq;
            }
            // Also include keyword values for completion suggester.
            for (val, _) in mem.all_keyword_values_for_field(field) {
                term_counts.entry(val).or_insert(1);
            }
        }

        // 2. On-disk FTS segments.
        {
            let snap = self.store.snapshot();
            let segments_dir = self.data_dir.join("segments");
            for meta in &snap.segments {
                if let Ok(reader) = FtsIndexReader::open(&segments_dir, &meta.id, &[field]) {
                    for term in reader.all_terms(field) {
                        term_counts.entry(term).or_insert(1);
                    }
                }
            }
        }

        term_counts.into_iter().collect()
    }

    /// Return the data directory path for this index (used by snapshot/restore).
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    // ── Delete index ──────────────────────────────────────────────────────────

    /// Remove all data files for this index.
    pub async fn delete_all_data(&self) -> Result<()> {
        std::fs::remove_dir_all(&self.data_dir).map_err(EngineError::Io)?;
        Ok(())
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Batch variant of [`evolve_schema_from_doc`]: one schema read-lock
    /// acquisition for the WHOLE batch instead of one per doc.  The per-doc
    /// variant paid a tokio `RwLock::read().await` per document — measured
    /// ~22 ms per 10 k-doc bulk batch (≈ a quarter of the whole turbo exec
    /// phase) even though the schema stops evolving after the first docs.
    ///
    /// Semantics are identical to calling `evolve_schema_from_doc` serially
    /// in doc order: new fields are collected in FIRST-SEEN order (so the
    /// first doc that introduces a field decides its inferred type) and
    /// added under a single write lock, which re-checks `has_field` exactly
    /// like the per-doc slow path does.
    async fn evolve_schema_from_docs(&self, sources: &[Arc<Value>]) {
        let new_fields: Vec<(String, FieldConfig)> = {
            let schema = self.schema.read().await;
            if !matches!(schema.dynamic, xerj_common::schema::DynamicMapping::Dynamic) {
                return;
            }
            let mut out: Vec<(String, FieldConfig)> = Vec::new();
            for source in sources {
                let Some(obj) = source.as_object() else {
                    continue;
                };
                for (key, val) in obj {
                    if !schema.schema.has_field(key) && !out.iter().any(|(k, _)| k == key) {
                        let ft = infer_field_type(val);
                        out.push((key.clone(), FieldConfig::new(key.clone(), ft)));
                    }
                }
            }
            out
        };

        if new_fields.is_empty() {
            return;
        }

        let mut schema = self.schema.write().await;
        if !matches!(schema.dynamic, xerj_common::schema::DynamicMapping::Dynamic) {
            return;
        }
        let mut schema_changed = false;
        for (_, fc) in new_fields {
            if !schema.schema.has_field(&fc.name) {
                let _ = schema.schema.add_field(fc);
                schema_changed = true;
            }
        }
        if schema_changed {
            let _ = self.save_schema(&schema).await;
        }
    }

    async fn evolve_schema_from_doc(&self, source: &Value) {
        let obj = match source.as_object() {
            Some(o) => o,
            None => return,
        };

        // Fast path: check with a read lock first to avoid taking a write lock
        // on every document when all fields are already known.  This is the
        // common case after the first few documents and avoids the write lock
        // bottleneck that otherwise limits indexing throughput.
        let new_fields: Vec<(String, FieldConfig)> = {
            let schema = self.schema.read().await;
            if !matches!(schema.dynamic, xerj_common::schema::DynamicMapping::Dynamic) {
                return;
            }
            obj.iter()
                .filter(|(key, _)| !schema.schema.has_field(key))
                .map(|(key, val)| {
                    let ft = infer_field_type(val);
                    (key.clone(), FieldConfig::new(key.clone(), ft))
                })
                .collect()
        };

        if new_fields.is_empty() {
            // No new fields — skip the write lock entirely (hot path).
            return;
        }

        // Slow path: at least one new field found.  Upgrade to write lock and
        // persist the updated schema.
        let mut schema = self.schema.write().await;
        if !matches!(schema.dynamic, xerj_common::schema::DynamicMapping::Dynamic) {
            return;
        }
        let mut schema_changed = false;
        for (_, fc) in new_fields {
            if !schema.schema.has_field(&fc.name) {
                let _ = schema.schema.add_field(fc);
                schema_changed = true;
            }
        }
        if schema_changed {
            let _ = self.save_schema(&schema).await;
        }
    }

    async fn save_schema(&self, schema: &ManagedSchema) -> Result<()> {
        let path = self.data_dir.join("schema.json");
        let bytes = serde_json::to_vec_pretty(schema)?;
        write_file_atomic(&path, &bytes).map_err(EngineError::Io)?;
        Ok(())
    }

    async fn fill_memtable_sources(&self, hits: Vec<Hit>) -> Vec<Hit> {
        // Look up sources from the active memtable for unflushed docs.
        // Flushed docs already have their sources filled by the segment reader
        // path in search_inner().
        let mem = &*self.memtable;
        hits.into_iter()
            .map(|mut h| {
                if h.source.is_null() {
                    if let Some(source) = mem.get_doc_source_as_value(&h.id) {
                        h.source = source;
                    }
                    // If still null, try loading from disk segments.  This can
                    // happen for FTS hits from the active memtable that were
                    // found by score but whose source was not cloned inline.
                }
                h
            })
            .collect()
    }
}

// ── Doc-values side-car helpers ─────────────────────────────────────────────
//
// One side-car file per segment named `{segment_id}.dv` that holds an
// LZ4-compressed `BTreeMap<String, Column>` (numeric or keyword).  Writing
// happens at flush + merge time; reading happens from the agg / sort /
// term-count fast paths.  Mirrors the Lucene `.dvd`/`.dvm` design
// philosophy of "every field has a column store" without tying us to
// Lucene's particular block-bitpacking layout (we use whole-vec encoding
// in M2; M3 introduces 128-doc bitpacked blocks).
//
// Auto-typing: any source field whose values are all numeric becomes a
// numeric column; otherwise a keyword column with the values stringified.
// This mirrors what ES does when `dynamic` mapping infers field types
// without explicit schema declarations.

fn build_doc_value_columns<'a>(
    sources: impl Iterator<Item = Option<&'a Value>>,
) -> std::collections::BTreeMap<String, xerj_storage::doc_values::Column> {
    use std::collections::BTreeMap;
    use xerj_storage::doc_values::{Column, KeywordColumn, NumericColumn};

    // Collect per-field per-doc raw values.  We materialise into vectors
    // so we can decide column-type once we've seen all values.  This is
    // O(docs × fields) memory but much smaller than the segment itself.
    let mut numeric: BTreeMap<String, Vec<Option<i64>>> = BTreeMap::new();
    let mut keyword: BTreeMap<String, Vec<Option<String>>> = BTreeMap::new();

    let mut total_docs: usize = 0;
    for src in sources {
        let doc_idx = total_docs;
        total_docs += 1;
        let Some(obj) = src.and_then(|v| v.as_object()) else {
            continue;
        };
        for (field, val) in obj {
            // Skip the `_id`, `_seq_no`, `_source` envelope keys — they're
            // bookkeeping, not user data.
            if field.starts_with('_') {
                continue;
            }
            // Lookup-first (`get_mut` before `entry`) so the common case
            // (field already has a column) skips the per-doc-field
            // `field.clone()` String allocation `entry()` would force —
            // ~370k allocs per 31k-doc flush segment on a 12-field corpus.
            match val {
                Value::Number(n) => {
                    // Always store as f64 bit-pattern.  The agg reader uses
                    // `f64::from_bits(v as u64)` to recover the value, which
                    // is lossless for integers up to 2^53 and exact for
                    // doubles.  Mixing i64 and f64 storage in the same column
                    // is undefined-behaviour because we have no per-column
                    // kind flag yet, so commit to one representation.
                    let Some(f) = n.as_f64() else { continue };
                    let v = f.to_bits() as i64;
                    if !numeric.contains_key(field) {
                        numeric.insert(field.clone(), Vec::new());
                    }
                    let col = numeric.get_mut(field).expect("just inserted");
                    pad_with_none(col, doc_idx);
                    col.push(Some(v));
                }
                Value::String(s) => {
                    if !keyword.contains_key(field) {
                        keyword.insert(field.clone(), Vec::new());
                    }
                    let col = keyword.get_mut(field).expect("just inserted");
                    pad_with_none(col, doc_idx);
                    col.push(Some(s.clone()));
                }
                Value::Bool(b) => {
                    if !numeric.contains_key(field) {
                        numeric.insert(field.clone(), Vec::new());
                    }
                    let col = numeric.get_mut(field).expect("just inserted");
                    pad_with_none(col, doc_idx);
                    let v = (if *b { 1.0_f64 } else { 0.0_f64 }).to_bits() as i64;
                    col.push(Some(v));
                }
                _ => {}
            }
        }
    }

    // Pad every column to total_docs so position == doc_id.
    for col in numeric.values_mut() {
        pad_with_none(col, total_docs);
    }
    for col in keyword.values_mut() {
        pad_with_none(col, total_docs);
    }

    let mut out: BTreeMap<String, Column> = BTreeMap::new();
    for (name, col) in numeric {
        out.insert(name, Column::Numeric(NumericColumn::from_iter(col)));
    }
    for (name, col) in keyword {
        match KeywordColumn::from_iter(col) {
            Ok(k) => {
                out.insert(name, Column::Keyword(k));
            }
            Err(e) => {
                tracing::warn!("doc-values keyword build failed for one field: {e}");
            }
        }
    }
    out
}

fn pad_with_none<T>(v: &mut Vec<Option<T>>, target_len: usize) {
    while v.len() < target_len {
        v.push(None);
    }
}

/// Encode columns to bytes and write them to `{segment_id}.dv`.
fn write_doc_values_sidecar(
    segments_dir: &std::path::Path,
    segment_id: &str,
    columns: &std::collections::BTreeMap<String, xerj_storage::doc_values::Column>,
) -> std::io::Result<()> {
    let path = segments_dir.join(format!("{segment_id}.dv"));
    let bytes = xerj_storage::doc_values::encode_columns(columns);
    std::fs::write(&path, bytes)
}

/// Load `{segment_id}.dv`, returning an empty map if the file doesn't
/// exist (older segments don't have doc-values yet).
fn read_doc_values_sidecar(
    segments_dir: &std::path::Path,
    segment_id: &str,
) -> std::collections::BTreeMap<String, xerj_storage::doc_values::Column> {
    use std::collections::BTreeMap;
    let path = segments_dir.join(format!("{segment_id}.dv"));
    let Ok(bytes) = std::fs::read(&path) else {
        return BTreeMap::new();
    };
    xerj_storage::doc_values::decode_columns(&bytes).unwrap_or_default()
}

/// Build a per-field FTS analyzer config map from a schema.
///
/// This is the bridge between the user's mapping (keyword / integer /
/// date / text) and the analyzer the FTS writer applies to each field's
/// tokens at flush + merge time.  The G4 short-circuit count path
/// depends on this: if a keyword field is indexed with the standard
/// analyzer it gets stop-worded ("GET"/"PUT"/"DELETE" all dropped) and
/// the FST has no entry for the literal value, so `term_doc_freq` falls
/// through to the slow stored-doc scan.
///
/// Mapping rules (matches Lucene's behaviour):
/// - `Text` → `standard` analyzer (tokenise, lowercase, stop-words)
/// - `Keyword`, `Long`, `Integer` (alias), `Double`, `Float`, `Date`,
///   `Boolean`, `Ip` → `keyword` analyzer (whole input as one token, no
///   stop-words)
/// - Any unknown / unmapped field defaults to `standard` because the
///   memtable insert path passes every source field through and we must
///   not stop-word user data unexpectedly.
fn build_fts_field_configs(schema: &Schema) -> HashMap<String, xerj_fts::index::FieldIndexConfig> {
    use xerj_fts::index::FieldIndexConfig;
    let mut out = HashMap::new();
    for f in &schema.fields {
        let analyzer = match f.field_type {
            FieldType::Text => "standard",
            // Everything else is exact-match.  We use the registered
            // "keyword" analyzer (KeywordTokenizer) which emits the input
            // string as a single token.
            _ => "keyword",
        };
        out.insert(
            f.name.clone(),
            FieldIndexConfig {
                analyzer: analyzer.to_string(),
                store_positions: matches!(f.field_type, FieldType::Text),
                store_term_vectors: false,
            },
        );
    }
    out
}

/// Mirror of `FtsMemtable::extract_text_value` — converts any JSON value
/// into a flattened string suitable for tokenization, so the merge path
/// indexes fields the same way the memtable does.  Kept as a free function
/// here because the memtable's helper is private.
fn extract_field_text(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(arr) => arr
            .iter()
            .map(extract_field_text)
            .collect::<Vec<_>>()
            .join(" "),
        Value::Object(_) => serde_json::to_string(val).unwrap_or_default(),
        Value::Null => String::new(),
    }
}

// ── Short-circuit count helper ───────────────────────────────────────────────

impl Index {
    /// Compute the total hit count for a query without touching a single
    /// stored document, if the query shape allows it.  Returns `Some(total)`
    /// on success, `None` if the query needs a full scan.
    ///
    /// Ported from ES's
    /// `QueryPhaseCollectorManager.shortcutTotalHitCount` — the same method
    /// that makes `{"size":0, "query":{"term":{...}}}` return in ≤ 3 ms on
    /// 66 M-doc indices.
    ///
    /// Supported shapes:
    /// * `MatchAll` — sum of segment `doc_count` + memtable doc count.
    /// * `Term { field, value }` — for a keyword field:
    ///     - Counts matches in the memtable via `doc_values_keyword_column`.
    ///     - For each on-disk segment, calls `FtsIndexReader::term_doc_freq`
    ///       on the raw value; if the segment analyzer lowercased terms,
    ///       falls back to the lowercased form.
    ///     - If the segment has no FTS data for the field at all, the
    ///       shortcut is abandoned (return `None`) because the segment
    ///       would otherwise undercount.
    /// Exact match count for a `bool` that carries `should` and/or `must_not`
    /// clauses — the shapes the fused must/filter-intersection arm bails on.
    /// Resolves every leaf clause to its doc-values position set per segment
    /// and combines them with set algebra:
    ///   required = ∩(must, filter);  should = positions in ≥k should sets;
    ///   result   = (required ∩ should) − ∪(must_not).
    /// Only Term/Terms/Range leaves are resolvable; anything else → `None`
    /// (caller keeps the counting scan).  Conservative guards make a
    /// delete-blind / memtable-blind count impossible to mistake for
    /// authoritative: bails on any ghost (overwrite/tombstone) event and on a
    /// non-empty memtable.  `minimum_should_match` percentages/field/script →
    /// `None`.
    fn bool_should_mustnot_count(
        &self,
        must: &[QueryNode],
        should: &[QueryNode],
        must_not: &[QueryNode],
        filter: &[QueryNode],
        minimum_should_match: &Option<xerj_query::ast::MinShouldMatch>,
        snap: &xerj_storage::index_store::IndexSnapshot,
    ) -> Option<u64> {
        // Delete/memtable blindness guards — an authoritative count must be
        // exact against live docs, and these sets only see flushed physical
        // segment docs.
        if self.store.version_map.ghost_events() > 0 {
            return None;
        }
        if self.memtable.doc_count() > 0 {
            return None;
        }

        // Effective minimum_should_match required-count.
        let k: u32 = if should.is_empty() {
            0
        } else {
            let default = if must.is_empty() && filter.is_empty() {
                1
            } else {
                0
            };
            match minimum_should_match {
                None => default,
                Some(xerj_query::ast::MinShouldMatch::Fixed(n)) => *n,
                // Percentage / per-doc field / script — not resolvable here.
                Some(_) => return None,
            }
        };
        // `should` only filters when at least one clause is required (k >= 1).
        let should_active = !should.is_empty() && k >= 1;

        let resolve = |cols: &std::collections::BTreeMap<
            String,
            xerj_storage::doc_values::Column,
        >,
                       clause: &QueryNode|
         -> Option<HashSet<u32>> {
            let clause = match clause {
                QueryNode::Constant { query, .. } | QueryNode::Boosted { query, .. } => {
                    query.as_ref()
                }
                _ => clause,
            };
            match clause {
                QueryNode::Term { field, value, .. } => {
                    seg_term_positions(cols, field, std::slice::from_ref(value))
                }
                QueryNode::Terms { field, values, .. } => seg_term_positions(cols, field, values),
                QueryNode::Range {
                    field,
                    gte,
                    gt,
                    lte,
                    lt,
                    ..
                } => seg_range_positions(
                    cols,
                    field,
                    gte.as_ref(),
                    gt.as_ref(),
                    lte.as_ref(),
                    lt.as_ref(),
                ),
                _ => None,
            }
        };

        let segments_dir = self.data_dir.join("segments");
        // Per-(segment, shape) count cache so the warm/repeat query doesn't
        // rebuild the position sets each call (the whole-result cache may be
        // off).  Segments are immutable → the cached seg count never staleness.
        let cache_key: Option<String> =
            serde_json::to_string(&(must, should, must_not, filter, k)).ok();
        let mut total: u64 = 0;
        for meta in &snap.segments {
            if meta.doc_count == 0 {
                continue;
            }
            if let Some(ck) = &cache_key {
                if let Some(hit) = self
                    .shortcut_count_cache
                    .get(&(meta.id.to_string(), ck.clone()))
                {
                    total = total.saturating_add(*hit.value());
                    continue;
                }
            }
            let cols = self.dv_columns_for(&segments_dir, &meta.id)?;

            // Required conjunction: intersection of must + filter sets.
            let required: Option<HashSet<u32>> = if must.is_empty() && filter.is_empty() {
                None
            } else {
                let mut sets: Vec<HashSet<u32>> = Vec::new();
                for c in must.iter().chain(filter.iter()) {
                    sets.push(resolve(&cols, c)?);
                }
                sets.sort_by_key(|s| s.len());
                let mut it = sets.into_iter();
                let mut acc = it.next().unwrap_or_default();
                for s in it {
                    acc.retain(|p| s.contains(p));
                    if acc.is_empty() {
                        break;
                    }
                }
                Some(acc)
            };

            // must_not union.
            let mut mnot: HashSet<u32> = HashSet::new();
            for c in must_not {
                mnot.extend(resolve(&cols, c)?);
            }

            let seg_count: u64 = if should_active {
                // Positions matching >= k of the should sets, then filtered by
                // the required set and the must_not exclusion.
                let mut hits: std::collections::HashMap<u32, u32> =
                    std::collections::HashMap::new();
                for c in should {
                    for p in resolve(&cols, c)? {
                        *hits.entry(p).or_insert(0) += 1;
                    }
                }
                let mut cnt: u64 = 0;
                for (p, h) in hits.iter() {
                    if *h < k {
                        continue;
                    }
                    if let Some(req) = &required {
                        if !req.contains(p) {
                            continue;
                        }
                    }
                    if mnot.contains(p) {
                        continue;
                    }
                    cnt += 1;
                }
                cnt
            } else if let Some(req) = &required {
                req.iter().filter(|p| !mnot.contains(p)).count() as u64
            } else {
                // No required, no active should → all docs minus must_not.
                (meta.doc_count).saturating_sub(mnot.len() as u64)
            };
            if let Some(ck) = &cache_key {
                if self.shortcut_count_cache.len() >= SHORTCUT_COUNT_CACHE_MAX {
                    self.shortcut_count_cache.clear();
                }
                self.shortcut_count_cache
                    .insert((meta.id.to_string(), ck.clone()), seg_count);
            }
            total = total.saturating_add(seg_count);
        }
        Some(total)
    }

    async fn try_shortcut_count(
        &self,
        query: &QueryNode,
        snap: &xerj_storage::index_store::IndexSnapshot,
        is_match_all: bool,
    ) -> Option<u64> {
        // Unwrap wrapper queries so constant_score and boosted
        // queries benefit from the same fast paths.
        let query = match query {
            QueryNode::Constant { query, .. } | QueryNode::Boosted { query, .. } => query.as_ref(),
            _ => query,
        };
        // Bool intersection shortcut — for `bool { must: [Term, Range] }`
        // (the most common production filter shape) compute the matching
        // doc-id set per segment via doc-values intersection, no scan.
        // Counted via the same `dv_columns_for` cache as the other paths.
        if let QueryNode::Bool {
            must,
            should,
            must_not,
            filter,
            minimum_should_match,
        } = query
        {
            // `should`/`must_not` shapes: resolve via per-segment doc-values
            // set algebra (the fused must/filter anchor-walk below only does
            // pure conjunctions).  `None` from the helper (unresolvable leaf /
            // ghost / non-empty memtable) falls through to the scan.
            if !should.is_empty() || !must_not.is_empty() {
                return self.bool_should_mustnot_count(
                    must,
                    should,
                    must_not,
                    filter,
                    minimum_should_match,
                    snap,
                );
            }
            // Combine must + filter — both behave identically for counting.
            let mut all_must: Vec<&QueryNode> = must.iter().collect();
            for f in filter {
                all_must.push(f);
            }
            if all_must.is_empty() {
                return None;
            }
            // Every must child must be a Term or Range we can resolve via
            // doc-values; otherwise abandon.
            for child in &all_must {
                if !matches!(child, QueryNode::Term { .. } | QueryNode::Range { .. }) {
                    return None;
                }
            }

            let segments_dir = self.data_dir.join("segments");
            let mut seg_matches: u64 = 0;
            // Canonical query key for the per-segment count cache.
            let bool_cache_key: Option<String> = serde_json::to_string(query).ok();
            for meta in &snap.segments {
                if meta.doc_count == 0 {
                    continue;
                }
                if let Some(k) = &bool_cache_key {
                    if let Some(hit) = self.shortcut_count_cache.get(&(meta.id.clone(), k.clone()))
                    {
                        seg_matches = seg_matches.saturating_add(*hit.value());
                        continue;
                    }
                }
                let cols = self.dv_columns_for(&segments_dir, &meta.id)?;
                // Resolve every predicate to a closure ONCE, then run a
                // single fused walk over the segment's doc range applying
                // every predicate per doc.  Zero allocations in the inner
                // loop.
                // `bool` flag = null bitmap is empty (skip null check).
                enum SegPred<'a> {
                    Keyword(&'a xerj_storage::doc_values::KeywordColumn, u32, bool),
                    Numeric(
                        &'a xerj_storage::doc_values::NumericColumn,
                        f64,
                        bool,
                        f64,
                        bool,
                        bool,
                    ),
                }
                let mut preds: Vec<SegPred<'_>> = Vec::with_capacity(all_must.len());
                let mut abandoned = false;
                let mut empty_pred = false;
                for child in &all_must {
                    match child {
                        QueryNode::Term { field, value, .. } => match cols.get(field.as_str()) {
                            Some(xerj_storage::doc_values::Column::Keyword(k)) => {
                                let term = match value {
                                    Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                };
                                match k.ord_for_term(&term) {
                                    Some(ord) => preds.push(SegPred::Keyword(
                                        k,
                                        ord,
                                        k.null_bitmap.is_empty(),
                                    )),
                                    None => {
                                        empty_pred = true;
                                        break;
                                    }
                                }
                            }
                            Some(xerj_storage::doc_values::Column::Numeric(n)) => {
                                let Some(tgt) = value.as_f64() else {
                                    abandoned = true;
                                    break;
                                };
                                preds.push(SegPred::Numeric(
                                    n,
                                    tgt,
                                    true,
                                    tgt,
                                    true,
                                    n.null_bitmap.is_empty(),
                                ));
                            }
                            None => {
                                abandoned = true;
                                break;
                            }
                        },
                        QueryNode::Range {
                            field,
                            gte,
                            gt,
                            lte,
                            lt,
                            ..
                        } => {
                            let to_f64 = |v: &Option<Value>| -> Option<f64> {
                                v.as_ref().and_then(|x| x.as_f64())
                            };
                            let (lo, lo_incl) = match (to_f64(gte), to_f64(gt)) {
                                (Some(v), _) => (v, true),
                                (None, Some(v)) => (v, false),
                                (None, None) => (f64::NEG_INFINITY, true),
                            };
                            let (hi, hi_incl) = match (to_f64(lte), to_f64(lt)) {
                                (Some(v), _) => (v, true),
                                (None, Some(v)) => (v, false),
                                (None, None) => (f64::INFINITY, true),
                            };
                            let Some(xerj_storage::doc_values::Column::Numeric(n)) =
                                cols.get(field.as_str())
                            else {
                                abandoned = true;
                                break;
                            };
                            preds.push(SegPred::Numeric(
                                n,
                                lo,
                                lo_incl,
                                hi,
                                hi_incl,
                                n.null_bitmap.is_empty(),
                            ));
                        }
                        // Anything other than Term or Range is filtered out
                        // upstream. If a new QueryNode variant ever slips
                        // through, fall back to the slow path instead of
                        // panicking on a search request.
                        _ => {
                            abandoned = true;
                            break;
                        }
                    }
                }
                if abandoned {
                    return None;
                }
                if empty_pred {
                    // One predicate matches nothing in this segment →
                    // intersection is empty → contribute 0 hits.
                    continue;
                }

                // Anchor on the smallest-cardinality predicate so the
                // intersection visits ~min(matches) docs instead of all
                // doc_count.  For nginx `bool { GET, 4xx }`, this drops
                // ~300k iterations → ~1.6k per segment (the 4xx range).
                let n_docs = meta.doc_count as u32;

                // Estimate cardinality for each predicate to pick the
                // anchor.  Cheap: keyword uses per_ord_count, numeric
                // uses range_count via the sorted index bisect.
                let card = |p: &SegPred<'_>| -> u32 {
                    match p {
                        SegPred::Keyword(k, ord, _) => {
                            k.per_ord_count.get(*ord as usize).copied().unwrap_or(0)
                        }
                        SegPred::Numeric(n, lo, lo_incl, hi, hi_incl, _) => {
                            n.range_count(*lo, *hi, *lo_incl, *hi_incl) as u32
                        }
                    }
                };
                let anchor_idx = (0..preds.len())
                    .min_by_key(|i| card(&preds[*i]))
                    .unwrap_or(0);
                let anchor_doc_ids: Vec<u32> = match &preds[anchor_idx] {
                    SegPred::Keyword(k, ord, _) => {
                        // Walk ords once to collect matching positions.
                        let mut v = Vec::with_capacity(card(&preds[anchor_idx]) as usize);
                        for (i, &o) in k.ords.iter().enumerate() {
                            if o == *ord && !k.null_bitmap.contains(i as u32) {
                                v.push(i as u32);
                            }
                        }
                        v
                    }
                    SegPred::Numeric(n, lo, lo_incl, hi, hi_incl, _) => {
                        n.range_doc_ids(*lo, *hi, *lo_incl, *hi_incl)
                    }
                };

                let mut sc: u64 = 0;
                'doc: for &i in &anchor_doc_ids {
                    for (idx, p) in preds.iter().enumerate() {
                        if idx == anchor_idx {
                            continue;
                        }
                        let ok = match p {
                            SegPred::Keyword(k, target_ord, no_nulls) => {
                                if !*no_nulls && k.null_bitmap.contains(i) {
                                    false
                                } else {
                                    k.ords[i as usize] == *target_ord
                                }
                            }
                            SegPred::Numeric(n, lo, lo_incl, hi, hi_incl, no_nulls) => {
                                if !*no_nulls && n.null_bitmap.contains(i) {
                                    false
                                } else {
                                    let v = f64::from_bits(n.data[i as usize] as u64);
                                    let pl = if *lo_incl { v >= *lo } else { v > *lo };
                                    let pu = if *hi_incl { v <= *hi } else { v < *hi };
                                    pl && pu
                                }
                            }
                        };
                        if !ok {
                            continue 'doc;
                        }
                    }
                    sc += 1;
                }
                let _ = n_docs; // silence unused
                if let Some(k) = &bool_cache_key {
                    if self.shortcut_count_cache.len() >= SHORTCUT_COUNT_CACHE_MAX {
                        self.shortcut_count_cache.clear();
                    }
                    self.shortcut_count_cache
                        .insert((meta.id.clone(), k.clone()), sc);
                }
                seg_matches = seg_matches.saturating_add(sc);
            }

            // Memtable side — fused columnar walk (one position pass per
            // shard, count-only) via the same predicate lowering the
            // memtable hit path uses.  The old fallback DEEP-CLONED the
            // entire memtable per query (`all_docs_with_sources`) and ran
            // a `doc_matches_query` JSON descent per buffered doc — under
            // a sustained bulk writer that alone was 1-6 s per bool query
            // (the read-under-write bool collapse, WARN mem_admit term).
            // When the columns can't serve the predicates we keep the
            // per-doc scan but share sources via Arc instead of cloning.
            let mem_matches: u64 = if self.memtable.doc_count() == 0 {
                0
            } else if let Some((_, total)) = mem_bool_preds(query)
                .and_then(|preds| self.memtable.doc_values_bool_query(&preds, 0))
            {
                total
            } else {
                let docs = self.memtable.all_docs_with_sources_arc();
                docs.iter()
                    .filter(|(_, src)| doc_matches_query(query, src))
                    .count() as u64
            };

            return Some(seg_matches + mem_matches);
        }

        if is_match_all {
            // Live doc count from the version map (one non-deleted entry per
            // `_id`). The `doc_count` atomic doubles as the version generator
            // and is bumped on every write, so an UPDATE would inflate `_count`
            // by 1 per update — the version map is the authoritative source and
            // is stable across in-flight flushes (a flush re-points an `_id` to
            // its new segment without changing which ids are live).
            return Some(self.live_doc_count());
        }

        // Range query shortcut (M2 G4-ext): walk segment `.dv` sorted
        // indices in O(log n) per segment, plus the memtable numeric
        // column linearly.  Any segment without a dv sidecar or without
        // the field causes the shortcut to be abandoned — we must not
        // undercount.
        if let QueryNode::Range {
            field,
            gte,
            gt,
            lte,
            lt,
            ..
        } = query
        {
            // Accept date-shaped string bounds by resolving them via
            // `parse_date_ms` so a range on an ISO-8601 date field
            // doesn't abandon to the stored-scan path (which is correct
            // but much slower for large segments). Abandon on non-date,
            // non-numeric strings — the stored-scan path handles those
            // deterministically via `json_compare`.
            let to_f64 = |v: &Option<Value>| -> Option<Option<f64>> {
                match v.as_ref() {
                    None => Some(None),
                    Some(Value::Number(_)) => Some(v.as_ref().and_then(|x| x.as_f64())),
                    Some(Value::String(s)) => {
                        if let Ok(f) = s.parse::<f64>() {
                            return Some(Some(f));
                        }
                        crate::aggs::parse_date_ms(&Value::String(s.clone()))
                            .map(|ms| Some(ms as f64))
                    }
                    _ => None,
                }
            };
            let gte_opt = to_f64(gte)?;
            let gt_opt = to_f64(gt)?;
            let lte_opt = to_f64(lte)?;
            let lt_opt = to_f64(lt)?;
            // Determine [lo, hi] with inclusivity flags.
            let (lo, lo_incl) = match (gte_opt, gt_opt) {
                (Some(v), _) => (v, true),
                (None, Some(v)) => (v, false),
                (None, None) => (f64::NEG_INFINITY, true),
            };
            let (hi, hi_incl) = match (lte_opt, lt_opt) {
                (Some(v), _) => (v, true),
                (None, Some(v)) => (v, false),
                (None, None) => (f64::INFINITY, true),
            };
            let field_str = field.as_str();

            let segments_dir = self.data_dir.join("segments");
            let mut seg_matches: u64 = 0;
            for meta in &snap.segments {
                if meta.doc_count == 0 {
                    continue;
                }
                let cols = self.dv_columns_for(&segments_dir, &meta.id)?;
                let col = match cols.get(field_str) {
                    Some(xerj_storage::doc_values::Column::Numeric(n)) => n,
                    _ => return None, // abandon — no numeric column
                };
                seg_matches = seg_matches.saturating_add(col.range_count(lo, hi, lo_incl, hi_incl));
            }

            // Memtable side — abandon the shortcut if the memtable has
            // no numeric column for this field. Otherwise we'd silently
            // return 0 and hide stored-string-date range matches that
            // the stored-scan path would otherwise find. Use a probe
            // via `for_each_numeric_value` since `shards` is private.
            //
            // NOTE: a bounded-delta sorted numeric index was trialled here to
            // make this O(log n + tail).  At the hardcoded 500k-doc flush cap
            // (~31k docs/shard) the memtable numeric column is small enough
            // that this branch-free linear f64 scan (memory-bandwidth bound,
            // tens of µs) BEATS a maintained sorted structure — the structure's
            // per-flush rebuild + sort + high-cardinality (cost_usd double)
            // count-map churn regressed range p99.  Kept the linear scan.
            if self.memtable.doc_count() > 0 {
                let mut has_any = false;
                self.memtable.for_each_numeric_value(field_str, |_| {
                    has_any = true;
                });
                if !has_any {
                    // Confirm by also checking the keyword column — if
                    // the field was indexed as keyword/text the numeric
                    // DV will be absent but so will numeric matches; we
                    // fall back to the stored-scan path rather than
                    // returning a bogus 0 count.
                    return None;
                }
            }
            let mem_matches: u64 = {
                let mut m: u64 = 0;
                self.memtable.for_each_numeric_value(field_str, |v| {
                    let pl = if lo_incl { v >= lo } else { v > lo };
                    let pu = if hi_incl { v <= hi } else { v < hi };
                    if pl && pu {
                        m += 1;
                    }
                });
                m
            };

            return Some(seg_matches + mem_matches);
        }

        // Regexp shortcut — the ES-equivalent term-dictionary rewrite.
        // ES answers `regexp` on a keyword field by expanding the pattern
        // against the field's term dictionary and unioning postings; the
        // XERJ equivalent runs the (compile-once) anchored regex over each
        // segment's DISTINCT terms — the doc-values keyword dictionary
        // when present, else the FTS `.fst` term dictionary — and sums the
        // per-term doc counts, O(segments × dictionary_size) instead of
        // O(doc_count) full stored-doc scans.
        //
        // Exactness vs the doc-scan it replaces: the Regexp arm of
        // `doc_matches_query` matches ONLY `Value::String` sources.  The
        // keyword doc-values column stores exactly those raw strings
        // (numbers / bools / arrays / objects are null in the column and
        // can never regexp-match), so it is exact for any field.  The FTS
        // FST fallback is only exact when the field is KEYWORD-typed in
        // the schema (whole raw string as a single un-lowercased token —
        // see `schema_to_field_configs`); for text/numeric fields the FST
        // holds analyzed/stringified tokens that the doc-scan would never
        // match, so we bail to the scan.  Abandoned (→ full scan) when a
        // segment has neither source (dotted / nested fields, pre-FTS
        // segments) or when deletes are present (both dictionaries tally
        // physical docs, not live ones).
        if let QueryNode::Regexp { field, pattern } = query {
            // Delete-blind guard — same conservative signal as the F1
            // `deletes_present` gate in `search_inner`.
            let seg_physical: u64 = snap.segments.iter().map(|m| m.doc_count).sum();
            let mem_physical = self.memtable.doc_count() as u64;
            if snap.segments.iter().any(|m| m.has_tombstones)
                || self.live_doc_count() < seg_physical + mem_physical
            {
                return None;
            }
            let field_is_keyword = {
                let schema_guard = self.schema.read().await;
                schema_guard
                    .schema
                    .field(field)
                    .map(|f| matches!(f.field_type, FieldType::Keyword))
                    .unwrap_or(false)
            };
            let segments_dir = self.data_dir.join("segments");
            let mut seg_matches: u64 = 0;
            for meta in &snap.segments {
                if meta.doc_count == 0 {
                    continue;
                }
                // Cached per-(segment, field, pattern) expansion — dv
                // keyword dictionary or FST postings union; one dictionary
                // walk + postings merge per segment LIFETIME, O(1) map hit
                // per query after that.  `None` (no usable dictionary /
                // non-keyword field / pathological pattern) abandons to
                // the stored-doc scan.
                let exp = self.regexp_segment_expansion(
                    &segments_dir,
                    &meta.id,
                    field,
                    pattern,
                    field_is_keyword,
                )?;
                seg_matches = seg_matches.saturating_add(exp.count);
            }
            // Memtable side — exact per-doc scan, the same fallback the
            // Bool arm above uses.  Memtables are flush-bounded and the
            // compiled regex is cached, so this stays cheap.  The keyword
            // count maps can NOT stand in here: they stringify numbers /
            // bools / first-array-elements, all of which the Regexp
            // doc-scan arm would never match.
            let mem_matches: u64 = if self.memtable.doc_count() == 0 {
                0
            } else {
                let docs = self.memtable.all_docs_with_sources();
                docs.iter()
                    .filter(|(_, src)| doc_matches_query(query, src))
                    .count() as u64
            };
            return Some(seg_matches + mem_matches);
        }

        // `terms` — union of exact per-value doc-values counts.  Distinct
        // values on a single-valued keyword/numeric column are disjoint, so
        // the summed doc-freqs are exact.  Conservative guards (ghost /
        // non-empty memtable) keep the delete/memtable-blind count from ever
        // standing in as authoritative; the ordinary scan handles those cases.
        if let QueryNode::Terms { field, values, .. } = query {
            use xerj_storage::doc_values::Column;
            if self.store.version_map.ghost_events() > 0 || self.memtable.doc_count() > 0 {
                return None;
            }
            let field_str = field.as_str();
            let segments_dir = self.data_dir.join("segments");
            let mut seg_matches: u64 = 0;
            for meta in &snap.segments {
                if meta.doc_count == 0 {
                    continue;
                }
                let cols = self.dv_columns_for(&segments_dir, &meta.id)?;
                match cols.get(field_str) {
                    Some(Column::Keyword(k)) => {
                        let mut seen: HashSet<String> = HashSet::new();
                        for v in values {
                            let raw = match v {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            if seen.insert(raw.clone()) {
                                seg_matches = seg_matches.saturating_add(k.doc_freq(&raw) as u64);
                            }
                        }
                    }
                    Some(Column::Numeric(n)) => {
                        let mut seen: HashSet<u64> = HashSet::new();
                        for v in values {
                            let f = v.as_f64()?;
                            if seen.insert(f.to_bits()) {
                                seg_matches = seg_matches
                                    .saturating_add(n.range_count(f, f, true, true));
                            }
                        }
                    }
                    None => return None, // no dv column → can't count safely
                }
            }
            return Some(seg_matches);
        }

        // `exists` — provable only for the DENSE case: a field whose column
        // has no nulls and covers every doc means every doc has a scalar value
        // (arrays / objects / json-null leave the null bitmap non-empty), so
        // all docs match.  Any null / partial coverage / missing column, or a
        // non-empty memtable, abandons to the scan — which resolves
        // `get_field_value(...).is_some()` exactly (incl. arrays, dotted
        // paths).  Meta fields are always present.
        if let QueryNode::Exists { field } = query {
            use xerj_storage::doc_values::Column;
            if self.store.version_map.ghost_events() > 0 || self.memtable.doc_count() > 0 {
                return None;
            }
            match field.as_str() {
                "_id" | "_index" | "_seq_no" | "_version" | "_primary_term" => {
                    return Some(self.live_doc_count());
                }
                _ => {}
            }
            let segments_dir = self.data_dir.join("segments");
            let mut seg_matches: u64 = 0;
            for meta in &snap.segments {
                if meta.doc_count == 0 {
                    continue;
                }
                let cols = self.dv_columns_for(&segments_dir, &meta.id)?;
                let (null_empty, dcount) = match cols.get(field.as_str()) {
                    Some(Column::Keyword(k)) => (k.null_bitmap.is_empty(), k.doc_count as u64),
                    Some(Column::Numeric(n)) => (n.null_bitmap.is_empty(), n.doc_count as u64),
                    None => return None, // can't prove presence → scan
                };
                if !null_empty || dcount != meta.doc_count {
                    return None; // sparse / partial → scan (may still match arrays)
                }
                seg_matches = seg_matches.saturating_add(meta.doc_count);
            }
            return Some(seg_matches);
        }

        // Only handle single Term queries for now; Bool goes through
        // the regular path until a Bool-specific shortcut lands.
        let (field, value) = match query {
            QueryNode::Term { field, value, .. } => (field.as_str(), value),
            _ => return None,
        };

        // Memtable side — M5.1 sharded count via per-shard count maps.
        // Each shard lazily rebuilds its own count map on first query
        // after an ingest; the sharded `doc_values_*_count` helpers
        // fan out the call across shards and sum the results.
        let mem_matches: u64 = {
            if self.memtable.doc_count() == 0 {
                0
            } else {
                let target_str: Option<String> = value.as_str().map(String::from);
                let target_num: Option<f64> = value.as_f64();
                let mut m = 0u64;
                if let Some(tgt) = target_num {
                    m += self.memtable.doc_values_numeric_count(field, tgt) as u64;
                }
                if m == 0 {
                    if let Some(ref tgt) = target_str {
                        m += self.memtable.doc_values_keyword_count(field, tgt) as u64;
                    } else if let Some(tgt) = target_num {
                        let s = tgt.to_string();
                        m += self.memtable.doc_values_keyword_count(field, &s) as u64;
                    }
                }
                m
            }
        };

        // Segment side — prefer the on-disk doc-values fast path (G2)
        // over the FTS term dictionary.  DocValues reads are a single
        // DashMap cache hit after the first query; the FTS path has to
        // parse the 13 MB meta.json per segment which is where ~200 ms/
        // query was going.
        //
        // Strategy:
        //   1. For each segment, load the cached doc-values columns.
        //   2. Keyword column → `doc_freq(term)` is an O(n) ord scan.
        //   3. Numeric column → `range_count(v, v, true, true)` bisects
        //      the sorted index in O(log n).
        //   4. Fall back to FTS `term_doc_freq` if both columns are
        //      missing for this segment.
        //   5. If FTS is missing too, abandon — we can't answer safely.
        let segments_dir = self.data_dir.join("segments");
        let raw = match value {
            Value::String(s) => s.as_str().to_string(),
            other => other.to_string(),
        };
        let lowered = raw.to_ascii_lowercase();
        let target_num: Option<f64> = value.as_f64();

        let mut seg_matches: u64 = 0;
        for meta in &snap.segments {
            if meta.doc_count == 0 {
                continue;
            }
            let cols = self.dv_columns_for(&segments_dir, &meta.id);

            let mut served_by_dv = false;
            if let Some(cols) = cols {
                match cols.get(field) {
                    Some(xerj_storage::doc_values::Column::Keyword(k)) => {
                        let df = k.doc_freq(&raw);
                        seg_matches = seg_matches.saturating_add(df as u64);
                        served_by_dv = true;
                    }
                    Some(xerj_storage::doc_values::Column::Numeric(n)) => {
                        if let Some(tgt) = target_num {
                            seg_matches =
                                seg_matches.saturating_add(n.range_count(tgt, tgt, true, true));
                            served_by_dv = true;
                        }
                    }
                    None => {}
                }
            }

            if served_by_dv {
                continue;
            }

            // Fallback: FTS term dictionary.  This is the slow path that
            // parses meta.json; we only take it when the field isn't in
            // the segment's doc-values (e.g. text fields with positions).
            let reader = match FtsIndexReader::open(&segments_dir, &meta.id, &[field]) {
                Ok(r) => r,
                Err(_) => return None, // FST missing — abandon
            };
            reader.field_stats(field)?;
            let df = reader
                .term_doc_freq(field, &raw)
                .or_else(|| {
                    if raw == lowered {
                        None
                    } else {
                        reader.term_doc_freq(field, &lowered)
                    }
                })
                .unwrap_or(0);
            seg_matches = seg_matches.saturating_add(df as u64);
        }

        Some(mem_matches + seg_matches)
    }
}

// ── Cached segment doc-values agg fast path ─────────────────────────────────

impl Index {
    /// Cache-backed `.dv` column loader.
    ///
    /// Note: M5.23 (clear-on-overflow cap) and M5.24 (proper LRU) both
    /// failed to improve sustained concurrent QPS.  The fundamental
    /// issue is that with 195 small segments, queries sweep ALL of
    /// them every time, so the working set IS the full cache.  Any
    /// eviction loses state that's needed moments later.  The right
    /// fix is forcemerge → 1 segment (which blows up on a separate
    /// memory issue in run_merge_once, M5.16/22 capped but still
    /// converges slowly).  Left unbounded for now.
    /// Bundle the publish-time warm targets (see `warm_segment_at_publish`).
    fn publish_warm_caches(&self) -> PublishWarmCaches {
        PublishWarmCaches {
            slices: Arc::clone(&self.stored_slices_cache),
            slices_bytes: Arc::clone(&self.stored_slices_cache_bytes),
            dv: Arc::clone(&self.dv_cache),
            shadow: Arc::clone(&self.sort_shadow_cache),
            shadow_fields: Arc::clone(&self.sort_shadow_fields),
        }
    }

    fn dv_columns_for(
        &self,
        segments_dir: &std::path::Path,
        segment_id: &str,
    ) -> Option<Arc<std::collections::BTreeMap<String, xerj_storage::doc_values::Column>>> {
        if let Some(entry) = self.dv_cache.get(segment_id) {
            return Some(Arc::clone(entry.value()));
        }
        let cols = read_doc_values_sidecar(segments_dir, segment_id);
        if cols.is_empty() {
            return None;
        }
        let arc = Arc::new(cols);
        self.dv_cache
            .insert(segment_id.to_string(), Arc::clone(&arc));
        Some(arc)
    }

    /// Cache-backed reader for a segment's parsed stored section.
    ///
    /// First call for a segment opens the segment, decodes the Stored
    /// section, and parses the result. Subsequent calls return an
    /// `Arc<Vec<Value>>` clone — no I/O, no decompress, no parse.
    /// Segments are immutable post-flush so the cache value remains
    /// valid until the segment is removed by a merge (at which point
    /// `stored_value_cache.clear()` flushes the entire map; see the
    /// merge-completion site).
    ///
    /// Uses `serde_json::from_slice` not `simd_json::serde::from_slice`:
    /// per ffd49ac, simd_json silently corrupts some payloads produced
    /// by the raw-bytes flush path (M7) — the deserialiser succeeds but
    /// returns malformed values. The KNN scan is the worst place to
    /// catch that. serde_json handles every variant correctly and the
    /// per-doc parse cost is amortised once across the whole cache
    /// lifetime of a segment.
    fn stored_values_for(&self, segment_id: &str) -> Option<Arc<Vec<Value>>> {
        if let Some(entry) = self.stored_value_cache.get(segment_id) {
            return Some(Arc::clone(entry.value()));
        }
        let reader = self.store.open_segment(segment_id).ok()?;
        let stored_bytes_raw = reader.section(SectionType::Stored).ok()??;
        let stored_bytes = xerj_storage::stored_codec::decode_stored(stored_bytes_raw).ok()?;
        let docs: Vec<Value> = serde_json::from_slice(&stored_bytes).ok()?;
        let arc = Arc::new(docs);
        self.stored_value_cache
            .insert(segment_id.to_string(), Arc::clone(&arc));
        Some(arc)
    }

    /// Cache-backed range pre-filter — returns the set of internal doc
    /// positions matching a `Range` query, used to skip over non-matching
    /// docs in the stored-section scan.
    #[allow(clippy::too_many_arguments)] // ES range bounds (gte/gt/lte/lt) + segment identity
    fn build_range_prefilter_cached(
        &self,
        segments_dir: &std::path::Path,
        segment_id: &str,
        field: &str,
        gte: Option<&Value>,
        gt: Option<&Value>,
        lte: Option<&Value>,
        lt: Option<&Value>,
    ) -> Option<Arc<HashSet<u32>>> {
        use xerj_storage::doc_values::Column;

        let parse = |v: Option<&Value>| -> Option<Option<f64>> {
            match v {
                None => Some(None),
                Some(Value::Number(_)) => Some(v.and_then(|x| x.as_f64())),
                Some(Value::String(s)) => {
                    if let Ok(f) = s.parse::<f64>() {
                        return Some(Some(f));
                    }
                    crate::aggs::parse_date_ms(&Value::String(s.clone())).map(|ms| Some(ms as f64))
                }
                _ => None,
            }
        };
        let gte = parse(gte)?;
        let gt = parse(gt)?;
        let lte = parse(lte)?;
        let lt = parse(lt)?;

        let (lo, lo_incl) = match (gte, gt) {
            (Some(v), _) => (v, true),
            (None, Some(v)) => (v, false),
            (None, None) => (f64::NEG_INFINITY, true),
        };
        let (hi, hi_incl) = match (lte, lt) {
            (Some(v), _) => (v, true),
            (None, Some(v)) => (v, false),
            (None, None) => (f64::INFINITY, true),
        };

        // Segments are immutable → a (segment, field, bounds) key never
        // goes stale.  SINGLE-FLIGHT the build: post-merge-publish, every
        // in-flight range query raced the same miss and each rebuilt the
        // O(matches) set.
        let cache_key = format!(
            "{segment_id}\u{1}{field}\u{1}{:x}\u{1}{:x}\u{1}{}{}",
            lo.to_bits(),
            hi.to_bits(),
            lo_incl as u8,
            hi_incl as u8
        );
        if let Some(hit) = self.range_prefilter_cache.get(&cache_key) {
            return Some(Arc::clone(hit.value()));
        }
        let flight = {
            let e = self
                .stored_slices_build_locks
                .entry(format!("{segment_id}\u{2}rpf"))
                .or_default();
            Arc::clone(e.value())
        };
        let _g = flight.lock().ok()?;
        if let Some(hit) = self.range_prefilter_cache.get(&cache_key) {
            return Some(Arc::clone(hit.value()));
        }

        let cols = self.dv_columns_for(segments_dir, segment_id)?;
        let Some(Column::Numeric(n)) = cols.get(field) else {
            return None;
        };
        let matching = n.range_doc_ids(lo, hi, lo_incl, hi_incl);
        let built: Arc<HashSet<u32>> = Arc::new(matching.into_iter().collect());
        if self.range_prefilter_cache.len() >= 32 {
            self.range_prefilter_cache.clear();
        }
        self.range_prefilter_cache
            .insert(cache_key, Arc::clone(&built));
        Some(built)
    }

    /// Build a doc-position pre-filter for a `term` / `terms` query against a
    /// segment, so `scan_stored_section_into` parses ONLY the matching stored
    /// docs instead of the whole section. Without this a SELECTIVE term (an id
    /// lookup, `code:500`) walked + JSON-parsed every stored doc looking for
    /// the handful of matches — the size>0 F1 early-break never fired because
    /// the bounded hit collector never filled (few matches).
    ///
    /// Mirrors `build_range_prefilter_cached`: numeric-field terms reuse the
    /// sorted numeric index via a degenerate `[v, v]` range; keyword-field
    /// terms enumerate the positions carrying the term's ordinal.
    ///
    /// Returns:
    /// - `Some(∅)` when the term matches nothing in the segment (caller skips
    ///   the segment entirely — no decompress/scan);
    /// - `Some(set)` (COMPLETE match set) when the match cardinality is
    ///   selective (`<= TERM_PREFILTER_CAP`);
    /// - `None` for very-high-cardinality terms (the ordinary early-break scan
    ///   is already fast) and for shapes the doc-values can't resolve (analyzed
    ///   text, non-numeric value on a numeric column, missing column) → full
    ///   scan, still correct.
    fn build_term_prefilter_cached(
        &self,
        segments_dir: &std::path::Path,
        segment_id: &str,
        field: &str,
        values: &[Value],
    ) -> Option<Arc<HashSet<u32>>> {
        use xerj_storage::doc_values::Column;
        // Selectivity ceiling: above this the match set is large enough that
        // the ordinary scan's F1 early-break already bounds the parse to
        // O(from+size), so a big position set would only add build + memory
        // cost. Keeps the set to <= ~256 KB.
        const TERM_PREFILTER_CAP: usize = 65_536;

        let cols = self.dv_columns_for(segments_dir, segment_id)?;
        match cols.get(field)? {
            Column::Numeric(n) => {
                // Exact numeric equality per value = a degenerate inclusive
                // range; union across `terms` values. Bail if any value isn't
                // numeric (→ full scan preserves semantics).
                let mut set: HashSet<u32> = HashSet::new();
                for v in values {
                    let f = v.as_f64()?;
                    // Cheap cardinality guard before materialising positions.
                    if set.len() + n.range_count(f, f, true, true) as usize > TERM_PREFILTER_CAP {
                        return None;
                    }
                    set.extend(n.range_doc_ids(f, f, true, true));
                }
                Some(Arc::new(set))
            }
            Column::Keyword(k) => {
                let mut set: HashSet<u32> = HashSet::new();
                for v in values {
                    let term = match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    let Some(ord) = k.ord_for_term(&term) else {
                        continue; // term absent in this segment → contributes 0
                    };
                    let card = k.per_ord_count.get(ord as usize).copied().unwrap_or(0) as usize;
                    if set.len() + card > TERM_PREFILTER_CAP {
                        return None;
                    }
                    for (pos, &o) in k.ords.iter().enumerate() {
                        if o == ord && !k.null_bitmap.contains(pos as u32) {
                            set.insert(pos as u32);
                        }
                    }
                }
                Some(Arc::new(set))
            }
        }
    }

    /// Lazily-built, cached `_id → stored-position` index for a segment.
    ///
    /// Reuses the decompressed `StoredSlices` (offset index) and extracts each
    /// doc's `_id` from the leading bytes of its stored slice, so an `ids`
    /// query can resolve primary keys to positions instead of scanning the
    /// whole section.  Built once per segment (segments are immutable) and
    /// evicted by id at the merge-completion site.
    ///
    /// Returns `None` when the slices can't be decoded OR when the map does
    /// not cover EVERY doc (a stored doc that carries no `_id` — the raw-source
    /// flush shape — can't be indexed here); the caller then falls back to the
    /// stored-doc scan, which has the identical `_id` resolution semantics, so
    /// no match is ever lost.
    fn id_pos_map_for(
        &self,
        seg_id: &str,
        expect_docs: u64,
    ) -> Option<Arc<std::collections::HashMap<String, u32>>> {
        if let Some(entry) = self.id_pos_cache.get(seg_id) {
            return Some(Arc::clone(entry.value()));
        }
        let slices = self.stored_slices_for(seg_id, expect_docs)?;
        let mut map: std::collections::HashMap<String, u32> =
            std::collections::HashMap::with_capacity(slices.offsets.len());
        for (pos, &(start, end)) in slices.offsets.iter().enumerate() {
            let slice = slices.bytes.get(start as usize..end as usize)?;
            let id = match extract_stored_id(slice) {
                Some(id) => id,
                None => {
                    // Escape-bearing or unusual layout — fall back to a full
                    // parse for just this doc to recover its `_id`.
                    let mut buf = slice.to_vec();
                    match simd_json::serde::from_slice::<Value>(&mut buf) {
                        Ok(v) => match v.get("_id").and_then(Value::as_str) {
                            Some(s) => s.to_string(),
                            None => continue,
                        },
                        Err(_) => continue,
                    }
                }
            };
            map.insert(id, pos as u32);
        }
        // Only cache + serve a COMPLETE map (every stored doc has an `_id`);
        // otherwise defer to the scan so a doc without a stored `_id` is not
        // silently dropped from `ids` results.
        if map.len() as u64 != expect_docs {
            return None;
        }
        let arc = Arc::new(map);
        self.id_pos_cache
            .insert(seg_id.to_string(), Arc::clone(&arc));
        Some(arc)
    }

    /// Position pre-filter for an `ids` query: resolve each requested id to its
    /// stored position via the cached `_id → position` index.  Returns
    /// `Some(∅)` when none of the ids live in this segment (caller skips it),
    /// `Some(set)` with the resolved positions, or `None` when the segment has
    /// no complete id index (→ full scan, still correct).
    ///
    /// Callers gate this on `!deletes_present`: with no overwrites/tombstones
    /// each live id occupies exactly one segment position, so the resolved
    /// positions are the exact match set.
    fn build_ids_prefilter_cached(
        &self,
        seg_id: &str,
        expect_docs: u64,
        values: &[String],
    ) -> Option<Arc<HashSet<u32>>> {
        let map = self.id_pos_map_for(seg_id, expect_docs)?;
        let mut set: HashSet<u32> = HashSet::with_capacity(values.len());
        for id in values {
            if let Some(&pos) = map.get(id) {
                set.insert(pos);
            }
        }
        Some(Arc::new(set))
    }

    /// Superset pre-filter for a pure-conjunction `bool` (`must` + `filter`
    /// only — no `should` / `must_not`).
    ///
    /// `scan_stored_section_into` re-runs the FULL query via `doc_matches_query`
    /// on every admitted position, so the pre-filter only needs to be a
    /// SUPERSET of the true matches. A bool AND matches a SUBSET of every one of
    /// its conjuncts, so the COMPLETE position set of ANY single conjunct is a
    /// valid superset — and the SMALLEST such set is the tightest (fewest docs
    /// to parse + re-test). This is what makes the ubiquitous
    /// `bool { filter:[ term, range, … ] }` shape parse O(most-selective-
    /// conjunct) stored docs instead of the whole section.
    ///
    /// Returns `Some(∅)` if a conjunct matches nothing (→ the bool matches
    /// nothing → skip the segment), `Some(set)` (the smallest resolvable
    /// conjunct's complete set), or `None` when no conjunct resolves to a
    /// selective dv-backed Term/Terms/Range (→ full scan, still correct).
    fn build_bool_prefilter_cached(
        &self,
        segments_dir: &std::path::Path,
        segment_id: &str,
        query: &QueryNode,
    ) -> Option<Arc<HashSet<u32>>> {
        let QueryNode::Bool {
            must,
            should,
            // `must_not` is subtractive: it only REMOVES docs, so it never
            // widens the superset. The stored-scan re-runs the full bool per
            // admitted doc and applies it — no need to read it here.
            must_not: _,
            filter,
            ..
        } = query
        else {
            return None;
        };
        const BOOL_PREFILTER_CAP: usize = 65_536;
        // Resolve a single leaf clause to its COMPLETE match-position set for
        // this segment (term/terms/range only). `None` = not resolvable to a
        // complete set (e.g. a `match`/`wildcard` clause).
        let resolve = |child: &QueryNode| -> Option<Arc<HashSet<u32>>> {
            let child = match child {
                QueryNode::Constant { query, .. } | QueryNode::Boosted { query, .. } => {
                    query.as_ref()
                }
                _ => child,
            };
            match child {
                QueryNode::Term { field, value, .. } => self.build_term_prefilter_cached(
                    segments_dir,
                    segment_id,
                    field,
                    std::slice::from_ref(value),
                ),
                QueryNode::Terms { field, values, .. } => {
                    self.build_term_prefilter_cached(segments_dir, segment_id, field, values)
                }
                QueryNode::Range {
                    field,
                    gte,
                    gt,
                    lte,
                    lt,
                    ..
                } => self.build_range_prefilter_cached(
                    segments_dir,
                    segment_id,
                    field,
                    gte.as_ref(),
                    gt.as_ref(),
                    lte.as_ref(),
                    lt.as_ref(),
                ),
                _ => None,
            }
        };

        // CASE A — at least one REQUIRED conjunct (`must`/`filter`).
        // Every hit is a subset of the AND of all required conjuncts, hence a
        // subset of ANY single required conjunct's complete set. `should` is
        // optional/narrowing when a required clause is present (default
        // `minimum_should_match` is 0), and `must_not` is subtractive — both
        // only REMOVE docs, never add. So the smallest resolvable required
        // conjunct is a valid SUPERSET regardless of `should`/`must_not`; the
        // stored-scan re-runs the full bool per admitted doc, refining it to
        // the exact match set and applying `should` boosts + `must_not`.
        if !must.is_empty() || !filter.is_empty() {
            let mut best: Option<Arc<HashSet<u32>>> = None;
            for child in must.iter().chain(filter.iter()) {
                if let Some(set) = resolve(child) {
                    if set.is_empty() {
                        // A required conjunct matches nothing → AND is empty.
                        return Some(set);
                    }
                    best = match best {
                        Some(b) if b.len() <= set.len() => Some(b),
                        _ => Some(set),
                    };
                }
            }
            return best;
        }

        // CASE B — pure `should` (no `must`/`filter`). Here `minimum_should_match`
        // defaults to 1, so a hit matches AT LEAST one should clause: the match
        // set is a subset of the UNION of every should clause's complete set
        // (a larger `minimum_should_match` only shrinks it further, so the union
        // stays a valid superset). `must_not` is still subtractive and gets
        // re-tested by the scan. The union is only valid if EVERY should clause
        // resolves — an unresolvable clause could admit docs outside the union.
        if !should.is_empty() {
            let mut union: HashSet<u32> = HashSet::new();
            for child in should.iter() {
                let set = resolve(child)?;
                if union.len() + set.len() > BOOL_PREFILTER_CAP {
                    return None; // too broad to be worth prefiltering
                }
                union.extend(set.iter().copied());
            }
            return Some(Arc::new(union));
        }

        // Pure `must_not` (or nothing resolvable) → base set is ~all docs; no
        // useful superset. Fall back to the full scan.
        None
    }

    /// Sorted-DV candidate pruning for field-sorted `match_all` queries
    /// (the search_after hot-path fix).
    ///
    /// For a segment with a numeric doc-values column on the PRIMARY sort
    /// field, the column's sorted `(value, position)` index tells us exactly
    /// which positions can possibly reach the global top-`cap` page:
    /// - `asc`:  the first `cap` positions at/after the cursor's primary
    ///   value (cursor ties INCLUDED — the secondary sort key may still
    ///   qualify them), plus all positions tied with the boundary value so
    ///   the secondary key can arbitrate;
    /// - `desc`: symmetric from the top end.
    ///
    /// Every other position in the segment provably sorts outside the page
    /// (worse primary key than `cap` closer candidates) or at/before the
    /// cursor, so the stored scan can skip parsing it entirely.  This is
    /// what turns a 1M-doc `search_after` page from an O(N) parse-everything
    /// scan into O(from+size) per segment.
    ///
    /// Returns `None` (→ caller falls back to the full scan, still correct)
    /// unless the shape is provably safe:
    /// - primary sort is a plain field (not `_score`/`_doc`) with a
    ///   `Column::Numeric` sidecar in this segment;
    /// - the column has NO nulls (a null means a missing field, an array,
    ///   or a non-scalar — `missing`/`mode` semantics would need the full
    ///   scan) and covers exactly `seg_doc_count` docs (position alignment);
    /// - the `search_after` primary cursor value (when present) is numeric.
    ///
    /// The caller additionally gates on `is_match_all && !deletes_present`:
    /// deleted/superseded docs occupy positions in the sorted index, so with
    /// deletes present the candidate slice could under-fill the page.
    /// Per-(segment, field) sorted `(key_bits, pos)` source for the
    /// field-sort candidate prefilter.  Serves Numeric dv columns directly
    /// and date-shaped Keyword dv columns via an epoch shadow built with
    /// the SAME normalisation `compute_sort_values` applies per hit —
    /// this is what lets the implicit `@timestamp desc` index sort (a
    /// date STRING in `_source`, stored as a Keyword column) use the
    /// bounded candidate path instead of a full scan+heap walk.
    fn sorted_shadow_for(
        &self,
        segments_dir: &std::path::Path,
        segment_id: &str,
        field: &str,
        seg_doc_count: u64,
    ) -> Option<Arc<Vec<(i64, u32)>>> {
        // Register the field so the publish-time warm pre-builds this
        // shadow for every FUTURE segment (bounded registry).
        if self.sort_shadow_fields.len() < 16 && !self.sort_shadow_fields.contains_key(field) {
            self.sort_shadow_fields.insert(field.to_string(), ());
        }
        let key = format!("{segment_id}\u{1}{field}");
        if let Some(entry) = self.sort_shadow_cache.get(&key) {
            return entry.value().clone();
        }
        // A missing/unreadable dv sidecar is a TRANSIENT state (a segment can
        // become visible a beat before its sidecar read succeeds under heavy
        // flush churn) — return None WITHOUT caching so the next query
        // re-probes.  Only a successfully-read sidecar whose column SHAPE is
        // ineligible gets a durable `None` (that never changes for an
        // immutable segment).  Caching the transient miss permanently
        // disabled the bounded path for that segment: every subsequent
        // sorted read full-scanned it until a merge retired it.
        let cols = self.dv_columns_for(segments_dir, segment_id)?;
        let built = build_sort_shadow(&cols, field, seg_doc_count);
        self.sort_shadow_cache.insert(key, built.clone());
        built
    }

    fn build_sort_candidates_prefilter(
        &self,
        segments_dir: &std::path::Path,
        segment_id: &str,
        seg_doc_count: u64,
        topk: &SortTopK,
        cap: usize,
    ) -> Option<SortCandidates> {
        use xerj_query::sort::SortOrder;

        let sf = topk.fields.first()?;
        if sf.is_score() || sf.is_doc_order() {
            return None;
        }
        let shadow = self.sorted_shadow_for(segments_dir, segment_id, &sf.field, seg_doc_count)?;
        let sorted: &[(i64, u32)] = shadow.as_slice();
        if sorted.is_empty() {
            return None;
        }

        // Primary cursor bound (search_after). `None` cursor → page 1.
        let cursor_v: Option<f64> = match topk.after.as_deref() {
            Some(vals) => match vals.first() {
                Some(Value::Number(x)) => Some(x.as_f64()?),
                // Non-numeric cursor against a numeric column — don't
                // guess, take the full scan.
                Some(_) => return None,
                None => None,
            },
            None => None,
        };

        let len = sorted.len();
        let cap = cap.max(1);
        let (start, end) = if sf.order == SortOrder::Asc {
            let lo = match cursor_v {
                // Include cursor ties: `< v0` keeps every position whose
                // value equals the cursor's primary value; the collector's
                // full-key comparison decides via the secondary fields.
                Some(v0) => sorted.partition_point(|(b, _)| f64::from_bits(*b as u64) < v0),
                None => 0,
            };
            let mut end = (lo + cap).min(len);
            // Extend across boundary ties so the secondary sort key can
            // arbitrate between equal primary values.
            if end > lo && end < len {
                let boundary = sorted[end - 1].0;
                while end < len && sorted[end].0 == boundary {
                    end += 1;
                }
            }
            (lo, end)
        } else {
            let hi = match cursor_v {
                // Include cursor ties from the other side: `<= v0` keeps
                // positions with value == v0 inside the slice.
                Some(v0) => sorted.partition_point(|(b, _)| f64::from_bits(*b as u64) <= v0),
                None => len,
            };
            let mut start = hi.saturating_sub(cap);
            if start < hi && start > 0 {
                let boundary = sorted[start].0;
                while start > 0 && sorted[start - 1].0 == boundary {
                    start -= 1;
                }
            }
            (start, hi)
        };

        let sel = &sorted[start..end];
        let ordered: Vec<(i64, u32)> = if sf.order == SortOrder::Asc {
            sel.to_vec()
        } else {
            sel.iter().rev().copied().collect()
        };
        Some(SortCandidates {
            set: sel.iter().map(|(_, d)| *d).collect(),
            ordered,
        })
    }

    /// Field-sort candidate narrowing for a query with a KNOWN per-segment
    /// match position set (Range dv prefilter, FTS hit set): walk the
    /// segment's sort-key shadow in page order (cursor-bounded) and keep
    /// only positions that are in `matches`, stopping after `cap` plus
    /// boundary ties.  The result is EXACTLY the per-segment top-`cap`
    /// matching docs by the primary sort key (ties extended so the heap's
    /// full-key/_id comparison arbitrates) — a valid replacement for
    /// offering every match to the heap, PROVIDED the caller sources
    /// `hits.total` independently (shortcut count / seg_hits tally).
    fn narrow_matches_to_sort_candidates(
        &self,
        segments_dir: &std::path::Path,
        segment_id: &str,
        seg_doc_count: u64,
        topk: &SortTopK,
        cap: usize,
        matches: &HashSet<u32>,
    ) -> Option<SortCandidates> {
        use xerj_query::sort::SortOrder;
        let sf = topk.fields.first()?;
        if sf.is_score() || sf.is_doc_order() {
            return None;
        }
        let shadow = self.sorted_shadow_for(segments_dir, segment_id, &sf.field, seg_doc_count)?;
        let sorted: &[(i64, u32)] = shadow.as_slice();
        if sorted.is_empty() {
            return None;
        }
        // Cursor bound (same semantics as build_sort_candidates_prefilter).
        let cursor_v: Option<f64> = match topk.after.as_deref() {
            Some(vals) => match vals.first() {
                Some(Value::Number(x)) => Some(x.as_f64()?),
                Some(_) => return None,
                None => None,
            },
            None => None,
        };
        let cap = cap.max(1);
        let len = sorted.len();
        let mut ordered: Vec<(i64, u32)> = Vec::with_capacity(cap.min(matches.len()) + 8);
        let mut boundary: Option<i64> = None;
        let mut walk = |it: &mut dyn Iterator<Item = &(i64, u32)>| {
            for &(kb, pos) in it {
                if let Some(b) = boundary {
                    // Tie-extension phase: keep collecting equal-key matches.
                    if kb != b {
                        break;
                    }
                    if matches.contains(&pos) {
                        ordered.push((kb, pos));
                    }
                    continue;
                }
                if matches.contains(&pos) {
                    ordered.push((kb, pos));
                    if ordered.len() >= cap {
                        boundary = Some(kb);
                    }
                }
            }
        };
        if sf.order == SortOrder::Asc {
            let lo = match cursor_v {
                Some(v0) => sorted.partition_point(|(b, _)| f64::from_bits(*b as u64) < v0),
                None => 0,
            };
            walk(&mut sorted[lo..].iter());
        } else {
            let hi = match cursor_v {
                Some(v0) => sorted.partition_point(|(b, _)| f64::from_bits(*b as u64) <= v0),
                None => len,
            };
            walk(&mut sorted[..hi].iter().rev());
        }
        Some(SortCandidates {
            set: ordered.iter().map(|(_, d)| *d).collect(),
            ordered,
        })
    }

    /// Hydrate the sorted-DV candidate positions of one segment from the
    /// warm `stored_slices_cache` — the fast twin of the
    /// `scan_stored_section_into` sort-topk branch (same version-map delete
    /// check, same dedup, same pre-heap cursor/worst rejection, same
    /// `total_count` tally for parsed live candidates), minus the per-query
    /// decompress and the O(segment-bytes) brace scan.
    ///
    /// Only called on the `is_match_all` sorted-candidates path, so hits
    /// score 1.0 and every live candidate matches by construction.
    /// Warm-or-build access to a segment's `StoredSlices` (decompressed
    /// stored bytes + per-doc offsets).  Cache miss → decompress + one
    /// O(bytes) brace walk (NO per-doc parse) and insert under the budget;
    /// over budget the built value is still returned (per-query cost, but
    /// never a per-doc parse).  `None` on open/decode failure or a
    /// malformed section (offsets incomplete) — caller falls back to the
    /// legacy scan.
    fn stored_slices_for(&self, seg_id: &str, expect_docs: u64) -> Option<Arc<StoredSlices>> {
        if let Some(entry) = self.stored_slices_cache.get(seg_id) {
            return Some(Arc::clone(entry.value()));
        }
        // SINGLE-FLIGHT the miss: up to 64 in-flight queries race the
        // same cold segment; without this every one of them ran its own
        // full stored-section decompress (one ~30 ms decode became a ~1 s
        // 64-query stall episode at every publish the warm hadn't
        // reached).  Waiters block on the per-segment mutex and wake to a
        // cache hit.
        let flight = {
            let entry = self
                .stored_slices_build_locks
                .entry(seg_id.to_string())
                .or_default();
            Arc::clone(entry.value())
        };
        let _flight_guard = flight.lock().ok()?;
        if let Some(entry) = self.stored_slices_cache.get(seg_id) {
            return Some(Arc::clone(entry.value()));
        }
        // Reuse already-decompressed bytes from `decoded_stored_cache`
        // before paying a fresh open + decompress.
        let bytes: Vec<u8> = if let Some(entry) = self.decoded_stored_cache.get(seg_id) {
            entry.value().as_ref().clone()
        } else {
            let reader = self.store.open_segment_arc(seg_id).ok()?;
            let raw = reader.section(SectionType::Stored).ok()??;
            xerj_storage::stored_codec::decode_stored(raw).ok()?
        };
        if bytes.len() > u32::MAX as usize {
            return None;
        }
        let offsets = brace_walk_offsets(&bytes);
        if offsets.len() as u64 != expect_docs {
            return None;
        }
        let slices = Arc::new(StoredSlices { bytes, offsets });
        let sz = slices.retained_bytes();
        if self
            .stored_slices_cache_bytes
            .load(Ordering::Relaxed)
            .saturating_add(sz)
            <= stored_slices_cache_budget()
            && self
                .stored_slices_cache
                .insert(seg_id.to_string(), Arc::clone(&slices))
                .is_none()
        {
            self.stored_slices_cache_bytes
                .fetch_add(sz, Ordering::Relaxed);
        }
        Some(slices)
    }

    #[allow(clippy::too_many_arguments)]
    fn hydrate_sorted_candidates(
        &self,
        slices: &StoredSlices,
        candidates: &SortCandidates,
        query: &QueryNode,
        is_match_all: bool,
        topk: &mut SortTopK,
        seen_ids: &mut HashSet<String>,
        total_count: &mut u64,
    ) {
        // Walk candidates in page order (best-first).  Their primary sort
        // key is already known from the dv shadow, so once the heap is
        // full and a candidate's primary is STRICTLY worse than the
        // current worst, every remaining candidate (same or worse primary)
        // is also rejected — break WITHOUT parsing.  After the heap warms
        // up this makes each additional segment O(ties) parses instead of
        // O(cap).
        for &(key_bits, pos) in &candidates.ordered {
            if topk.primary_f64_rejects(f64::from_bits(key_bits as u64)) {
                break;
            }
            let Some(&(start, end)) = slices.offsets.get(pos as usize) else {
                continue;
            };
            let Some(slice) = slices.bytes.get(start as usize..end as usize) else {
                continue;
            };
            let mut doc_buf = slice.to_vec();
            let doc: Value = match simd_json::serde::from_slice(&mut doc_buf) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let id_ref = doc.get("_id").and_then(Value::as_str).unwrap_or("");
            if let Some(ver) = self.store.version_map.get(id_ref) {
                if ver.deleted {
                    continue;
                }
            }
            *total_count += 1;
            if seen_ids.contains(id_ref) {
                continue;
            }
            let source_ref = doc.get("_source").unwrap_or(&doc);
            // Candidates are guaranteed query matches by construction
            // (match_all, or positions drawn from the query's own dv/FTS
            // match set) — only the SCORE needs the per-query path.
            let score = if is_match_all {
                1.0
            } else {
                score_query_against_doc(query, source_ref)
            };
            let key = compute_sort_values(source_ref, score, id_ref, topk.fields.as_slice());
            if !topk.would_admit(&key) {
                continue;
            }
            let id = id_ref.to_string();
            seen_ids.insert(id.clone());
            topk.offer_keyed(Hit {
                id,
                score,
                source: source_ref.clone(),
                sort: key,
                explain: None,
                highlight: None,
                matched_queries: Vec::new(),
            });
        }
    }

    /// Hydrate an UNSORTED, pre-filtered query from a warm `StoredSlices` using
    /// the per-doc offset index for RANDOM ACCESS — parse only the pre-filter
    /// positions, never brace-walk the whole section.
    ///
    /// The warm-slices scan path otherwise walks every doc's bytes (balanced-
    /// brace split) to advance the position counter and test pre-filter
    /// membership, so a selective term whose matches fan across all N shard-
    /// segments paid O(total section bytes) per query even though it parses
    /// only a handful of docs. With the offsets already cached, we jump
    /// straight to each matching position instead: O(|pre_filter|) parses, no
    /// section walk.
    ///
    /// `pre_filter` is a SUPERSET of the true matches (bool picks one
    /// conjunct's complete set; term/range sets are exact) — every hydrated doc
    /// is re-tested with `doc_matches_query`, so false positives are dropped
    /// exactly as the brace-walk scan drops them. Admit order is positional
    /// (positions sorted ascending) to match the linear scan's `all_hits`
    /// order, and deletes are filtered via the version map. Early-break mirrors
    /// F1: stop once the bounded page is full and the exact total is supplied
    /// elsewhere (`count_authoritative`).
    #[allow(clippy::too_many_arguments)]
    fn hydrate_prefiltered_unsorted(
        &self,
        slices: &StoredSlices,
        pre_filter: &HashSet<u32>,
        query: &QueryNode,
        is_match_all: bool,
        count_only: bool,
        materialisation_limit: usize,
        count_authoritative: bool,
        total_count: &mut u64,
        all_hits: &mut Vec<Hit>,
        seen_ids: &mut HashSet<String>,
    ) {
        let mut positions: Vec<u32> = pre_filter.iter().copied().collect();
        positions.sort_unstable();
        for pos in positions {
            if count_authoritative && !count_only && all_hits.len() >= materialisation_limit {
                break;
            }
            let Some(&(start, end)) = slices.offsets.get(pos as usize) else {
                continue;
            };
            let Some(slice) = slices.bytes.get(start as usize..end as usize) else {
                continue;
            };
            let mut doc_buf = slice.to_vec();
            let doc: Value = match simd_json::serde::from_slice(&mut doc_buf) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let id_ref = doc.get("_id").and_then(Value::as_str).unwrap_or("");
            if let Some(ver) = self.store.version_map.get(id_ref) {
                if ver.deleted {
                    continue;
                }
            }
            // Re-test the full query — the pre-filter is only a superset.
            let matched = if is_match_all {
                true
            } else if let QueryNode::Ids { values } = query {
                values.iter().any(|v| v == id_ref)
            } else {
                let source_ref = doc.get("_source").unwrap_or(&doc);
                let source_with_id = if source_ref.get("_id").is_some() {
                    source_ref.clone()
                } else if let Some(obj) = source_ref.as_object() {
                    let mut o = obj.clone();
                    o.insert("_id".to_string(), Value::String(id_ref.to_string()));
                    Value::Object(o)
                } else {
                    source_ref.clone()
                };
                doc_matches_query(query, &source_with_id)
            };
            if !matched {
                continue;
            }
            *total_count += 1;
            if count_only {
                continue;
            }
            if all_hits.len() >= materialisation_limit {
                if count_authoritative {
                    break;
                }
                continue;
            }
            let id: String = id_ref.to_string();
            if seen_ids.contains(&id) {
                continue;
            }
            let source = doc.get("_source").cloned().unwrap_or_else(|| doc.clone());
            let score = if is_match_all {
                1.0
            } else {
                score_query_against_doc(query, &source)
            };
            seen_ids.insert(id.clone());
            all_hits.push(Hit {
                id,
                score,
                source,
                sort: Vec::new(),
                explain: None,
                highlight: None,
                matched_queries: Vec::new(),
            });
        }
    }

    /// Regexp pre-filter — the set of internal doc positions whose keyword
    /// term matches the (compile-once) anchored pattern, used to skip
    /// non-matching docs in the stored-section scan the same way the
    /// Range pre-filter does.
    ///
    /// The regex runs over the segment's DISTINCT terms (doc-values
    /// keyword dictionary when present, else the field's FTS `.fst`
    /// dictionary), not per doc: O(dictionary) matcher work + one ords
    /// walk / bounded postings merge.  Exactness mirrors the
    /// `try_shortcut_count` Regexp arm: the doc-scan Regexp arm matches
    /// only `Value::String` sources; the keyword column stores exactly
    /// those strings (non-strings are null), and the FST route is only
    /// taken when `fst_ok` (schema says keyword-typed → term == raw
    /// string, and FTS doc ids == stored positions, see the FTS hit
    /// materialisation which indexes stored docs by `doc_id`).  Fields
    /// with neither source return `None` → full scan, never a wrong skip.
    ///
    /// `cap`: when the caller's `hits.total` is authoritative (F1) the
    /// scan early-breaks after materialising `cap` hits anyway, so we stop
    /// collecting positions there instead of building an ~850 k-entry set
    /// for a broad pattern.  Pass `None` when the scan must tally the
    /// exact total itself — then the set must be complete.  The capped
    /// set holds the FIRST `cap` matching positions in doc order (ords
    /// walk / ascending postings merge), so the scan materialises the
    /// same leading hits the uncapped scan would.
    fn build_regexp_prefilter_cached(
        &self,
        segments_dir: &std::path::Path,
        segment_id: &str,
        field: &str,
        pattern: &str,
        cap: Option<usize>,
        fst_ok: bool,
    ) -> Option<HashSet<u32>> {
        let exp =
            self.regexp_segment_expansion(segments_dir, segment_id, field, pattern, fst_ok)?;
        if exp.count == 0 {
            // No dictionary term matches — whole segment skipped without
            // decompressing / parsing a single stored doc.
            return Some(HashSet::new());
        }
        match cap {
            // Bounded (F1 count-authoritative) mode: the scan stops after
            // materialising `c` hits, so the FIRST `c` matching positions
            // (ascending == scan order) reproduce exactly the hits the
            // unfiltered scan would emit.  The expansion retains
            // `REGEXP_EXPANSION_POS_CAP` ≥ any default page, so this only
            // falls back to a full scan for very deep pagination.
            Some(c) => {
                if exp.complete || exp.positions.len() >= c {
                    Some(exp.positions.iter().take(c).copied().collect())
                } else {
                    None
                }
            }
            // Unbounded mode (scan must tally the exact total itself):
            // only a COMPLETE position set is a valid filter.
            None => {
                if exp.complete {
                    Some(exp.positions.iter().copied().collect())
                } else {
                    None
                }
            }
        }
    }

    /// Expand a regexp against one segment's term dictionary, cached per
    /// `(segment_id, field, pattern)` — see `regexp_expand_cache`.
    ///
    /// Sources, in order:
    /// (a) the `.dv` keyword column (sorted distinct terms + ords) — exact
    ///     for any field type because it stores raw strings only;
    /// (b) the field's FTS `.fst` term dictionary + postings union —
    ///     taken only when `fst_ok` (schema says the field is
    ///     keyword-typed, so FST terms are the raw un-analyzed strings and
    ///     the expansion agrees with the stored-doc scan it stands in
    ///     for).  Flushed segments currently carry no `.dv` sidecar, so
    ///     (b) is the path that serves live traffic; merged segments have
    ///     both.
    ///
    /// Returns `None` when neither source can answer exactly (missing
    /// side-cars, non-keyword field, or a pathological pattern matching
    /// > 1024 dictionary terms) — callers fall back to the stored scan.
    fn regexp_segment_expansion(
        &self,
        segments_dir: &std::path::Path,
        segment_id: &str,
        field: &str,
        pattern: &str,
        fst_ok: bool,
    ) -> Option<Arc<RegexpExpansion>> {
        let key = (
            segment_id.to_string(),
            field.to_string(),
            pattern.to_string(),
        );
        if let Some(hit) = self.regexp_expand_cache.get(&key) {
            return Some(Arc::clone(hit.value()));
        }

        // An invalid pattern matches nothing (the doc-scan arm fails the
        // per-doc compile and returns false) — expansion is empty+complete.
        let re = compiled_anchored_regex(pattern);

        let mut exp: Option<RegexpExpansion> = None;
        if let Some(cols) = self.dv_columns_for(segments_dir, segment_id) {
            if let Some(xerj_storage::doc_values::Column::Keyword(k)) = cols.get(field) {
                let mut count: u64 = 0;
                let mut positions: Vec<u32> = Vec::new();
                if let Some(re) = re.as_ref() {
                    let matched_ords: Vec<bool> = k.terms.iter().map(|t| re.is_match(t)).collect();
                    if matched_ords.iter().any(|&m| m) {
                        for (i, &ord) in k.ords.iter().enumerate() {
                            let pos = i as u32;
                            if k.null_bitmap.contains(pos) {
                                continue;
                            }
                            if matched_ords.get(ord as usize).copied().unwrap_or(false) {
                                count += 1;
                                if positions.len() < REGEXP_EXPANSION_POS_CAP {
                                    positions.push(pos);
                                }
                            }
                        }
                    }
                }
                let complete = count == positions.len() as u64;
                exp = Some(RegexpExpansion {
                    count,
                    positions,
                    complete,
                });
            }
        }

        let exp = match exp {
            Some(e) => e,
            None => {
                if !fst_ok {
                    return None;
                }
                let reader = FtsIndexReader::open(segments_dir, segment_id, &[field]).ok()?;
                reader.field_stats(field)?;
                let matched = regexp_matched_fst_terms(&reader, field, re.as_ref())?;
                if matched.is_empty() {
                    RegexpExpansion {
                        count: 0,
                        positions: Vec::new(),
                        complete: true,
                    }
                } else {
                    let (count, positions) =
                        postings_union_expand(&reader, field, &matched, REGEXP_EXPANSION_POS_CAP);
                    let complete = count == positions.len() as u64;
                    RegexpExpansion {
                        count,
                        positions,
                        complete,
                    }
                }
            }
        };

        if self.regexp_expand_cache.len() >= REGEXP_EXPANSION_CACHE_MAX {
            self.regexp_expand_cache.clear();
        }
        let arc = Arc::new(exp);
        self.regexp_expand_cache.insert(key, Arc::clone(&arc));
        Some(arc)
    }

    /// Version of `try_aggs_fast_with_segments` that caches decoded columns
    /// per segment on `self.dv_cache`.  Segments are immutable once flushed,
    /// so a decoded `Arc<BTreeMap<String, Column>>` stays valid until the
    /// segment is merged away (at which point it gets evicted in bulk).
    ///
    /// The cache turns the first query over a segment into a file read +
    /// LZ4 decompress + decode, and every subsequent query into a pointer
    /// chase.  This is what makes `terms agg method` over millions of
    /// docs land in the single-digit ms range instead of 100ms+.
    async fn try_aggs_fast_with_segments_cached(
        &self,
        aggs_def: &Value,
        filter: Option<&Value>,
        snap: &xerj_storage::index_store::IndexSnapshot,
        segments_dir: &std::path::Path,
    ) -> Option<(Value, Option<u64>)> {
        // 2026-07 agg-hang fix: the columnar executor in `fast_aggs.rs`
        // serves `size:0 + match_all + aggs` straight from segment `.dv`
        // sidecars + the live memtable.  It bails (None) on any shape it
        // can't reproduce byte-identically, on small indices, and when
        // deletes/updates are present — the caller then falls back to the
        // brute `run_aggs_with_all` stored-doc path.
        //
        // Boolean-mapped fields are stored as 0/1 in numeric `.dv` columns,
        // indistinguishable from real integers at the column level. The
        // schema is the only source of truth for "render 0/1 as
        // false/true", so snapshot the boolean field names here (async
        // lock) and hand them to the sync executor.
        let bool_fields: std::collections::HashSet<String> = {
            let guard = self.schema.read().await;
            guard
                .schema
                .fields
                .iter()
                .filter(|f| matches!(f.field_type, xerj_common::FieldType::Boolean))
                .map(|f| f.name.clone())
                .collect()
        };
        self.try_fast_aggs(aggs_def, filter, snap, segments_dir, &bool_fields)
    }
}

// ── Helper for streaming stored-doc scan ─────────────────────────────────────

impl Index {
    /// Scan the stored section of a single segment and push matching docs
    /// into the caller's bounded collector.
    ///
    /// This uses a **streaming parser** that reads one JSON value at a time
    /// out of the `[v1, v2, ...]` array — a full `Vec<Value>` is never
    /// materialised for the segment, which was essential to stop the
    /// query-path OOM in CAPPED_RAM_BATTLE.  For a 50 k-doc segment the
    /// materialised Vec used ~150 MB of heap; the streaming version uses
    /// <1 MB because each doc is dropped before the next is parsed.
    ///
    /// `pre_filter` (M2 G3): when `Some`, restricts parsing to internal
    /// doc positions in the set.  Non-matching positions are skipped via
    /// brace counting only — we never allocate a `Value` tree for them.
    /// This makes Range queries O(matching_docs) instead of O(total_docs)
    /// in the worst case, because `range_doc_ids` on the segment's doc-
    /// values sorted index gives us the exact set in O(log n).
    #[allow(clippy::too_many_arguments)]
    fn scan_stored_section_into(
        &self,
        stored_bytes: &[u8],
        query: &QueryNode,
        is_match_all: bool,
        count_only: bool,
        materialisation_limit: usize,
        count_authoritative: bool,
        total_count: &mut u64,
        all_hits: &mut Vec<Hit>,
        seen_ids: &mut HashSet<String>,
        pre_filter: Option<&HashSet<u32>>,
        mut sort_topk: Option<&mut SortTopK>,
        // When `Some`, every doc's `(start, end)` byte range is appended as
        // the brace-scan walks the section, so the caller can cache the
        // decompressed bytes + offsets (`stored_slices_cache`) and hydrate
        // future sorted-candidate queries without re-scanning.  Callers must
        // verify `offsets_out.len() == segment doc_count` before caching —
        // a malformed section bails early and leaves the vec incomplete.
        mut offsets_out: Option<&mut Vec<(u32, u32)>>,
        // Cooperative timeout: when `Some`, the walk polls the wall clock
        // every 4 096 docs and aborts (setting `*timed_out`) once past the
        // deadline.  One giant merged segment is otherwise a multi-second
        // uninterruptible unit — the between-segment deadline check in
        // `search_inner` can't bound it.
        deadline: Option<std::time::Instant>,
        timed_out: &mut bool,
        // Diagnostics: docs brace-walked and hits admitted by THIS call.
        dbg_walked: &mut u64,
        dbg_admitted: &mut u64,
    ) {
        // Fast path: scan one doc at a time with a hand-rolled array splitter.
        // The stored section is always shaped as `[{...}, {...}, ...]` (a
        // JSON array of objects).  We walk the bytes, find each top-level
        // object's range by balanced-brace counting, and parse only that
        // slice — so the only allocation per doc is the `Value` tree for a
        // single doc, which is released before the next iteration.
        let bytes = stored_bytes;
        let n = bytes.len();
        if n < 2 {
            return;
        }
        // Skip leading whitespace/`[`.
        let mut i = 0usize;
        while i < n && (bytes[i].is_ascii_whitespace() || bytes[i] == b'[') {
            i += 1;
        }

        // Track the current document's internal index so we can match it
        // against `pre_filter`.  Position 0 = first doc in stored section.
        let mut doc_pos: u32 = 0;

        loop {
            // F1b — IN-SEGMENT early stop.  The between-segment F1 break in
            // `search_inner` skips segments once the page is full and the
            // total is authoritative, but a single giant merged segment
            // still paid a FULL walk here: brace-scan + simd_json parse of
            // every stored doc (~11 s for an uncached size=10 `match_all`
            // at 3.5 M docs — completely masked at steady state by the
            // query cache, catastrophic under a bulk writer where every
            // request is a cache miss).  Once the bounded collector is
            // full, nothing later in this section can change the page
            // (admit order is positional) and `total_count` is overwritten
            // by the shortcut/live count anyway — so stop scanning.
            //
            // Excluded, for the same reasons as the outer break:
            //   - `count_only`: the scan tally IS the total when no
            //     shortcut resolved (count_authoritative already implies a
            //     shortcut, but keep the guard belt-and-braces);
            //   - `sort_topk`: top-N needs every matching doc's sort key;
            //   - `offsets_out`: the offsets cache must cover the section.
            if count_authoritative
                && !count_only
                && sort_topk.is_none()
                && offsets_out.is_none()
                && all_hits.len() >= materialisation_limit
            {
                return;
            }
            // Skip whitespace/commas between docs.
            while i < n && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
                i += 1;
            }
            if i >= n || bytes[i] == b']' {
                break;
            }
            if bytes[i] != b'{' {
                // Unexpected char — bail gracefully.
                return;
            }
            // Scan for the matching closing brace, tracking strings + escapes.
            let start = i;
            let mut depth = 0i32;
            let mut in_str = false;
            let mut escape = false;
            while i < n {
                let b = bytes[i];
                if in_str {
                    if escape {
                        escape = false;
                    } else if b == b'\\' {
                        escape = true;
                    } else if b == b'"' {
                        in_str = false;
                    }
                } else {
                    match b {
                        b'"' => in_str = true,
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                i += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                i += 1;
            }
            let end = i;
            if depth != 0 {
                // Malformed — bail.
                return;
            }

            if let Some(out) = offsets_out.as_deref_mut() {
                out.push((start as u32, end as u32));
            }

            let cur_pos = doc_pos;
            doc_pos += 1;
            *dbg_walked += 1;

            // Cooperative timeout poll — every 4 096 docs (~1 µs of clock
            // reads per million docs; the parse below costs ~3 µs/doc).
            if doc_pos & 0xFFF == 0 {
                if let Some(dl) = deadline {
                    if std::time::Instant::now() >= dl {
                        *timed_out = true;
                        return;
                    }
                }
            }

            // Pre-filter: if the caller supplied a matching set and this
            // position isn't in it, skip the parse entirely.  We already
            // advanced `i` past the doc, so just loop.
            if let Some(filter) = pre_filter {
                if !filter.contains(&cur_pos) {
                    continue;
                }
            }

            let doc_slice = &bytes[start..end];

            let mut doc_buf = doc_slice.to_vec();
            let doc: Value = match simd_json::serde::from_slice(&mut doc_buf) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let id_ref = doc.get("_id").and_then(Value::as_str).unwrap_or("");
            if let Some(ver) = self.store.version_map.get(id_ref) {
                if ver.deleted {
                    continue;
                }
            }

            let matched = if is_match_all {
                true
            } else if let QueryNode::Ids { values } = query {
                values.iter().any(|v| v == id_ref)
            } else {
                // The stored doc can be either:
                //   {"_id":"..","_seq_no":..,"_source":{fields}}  (envelope format)
                //   {fields}                                       (raw source, e.g. from M5.11 raw-bytes path)
                // Try _source first; fall back to the doc itself. Inject
                // `_id` into source so deeply-nested Ids queries (e.g.
                // function_score → ids) can resolve it.
                let source_ref = doc.get("_source").unwrap_or(&doc);
                let source_with_id = if source_ref.get("_id").is_some() {
                    source_ref.clone()
                } else if let Some(obj) = source_ref.as_object() {
                    let mut o = obj.clone();
                    o.insert("_id".to_string(), Value::String(id_ref.to_string()));
                    Value::Object(o)
                } else {
                    source_ref.clone()
                };
                doc_matches_query(query, &source_with_id)
            };

            if !matched {
                continue;
            }

            *total_count += 1;
            *dbg_admitted += 1;

            if count_only {
                continue;
            }

            // Field-sorted queries route every match into the bounded top-N
            // heap: no early stop (we must visit ALL matches so the survivors
            // are the GLOBAL top-N by the sort key), and retention is bounded
            // by the heap capacity rather than by first-N scan order.
            //
            // Hot-path discipline (search_after OOM fix): compute the sort
            // key on the BORROWED source first and reject pre-heap — hits at
            // or before the `search_after` cursor, and hits that cannot beat
            // the current heap-worst, are dropped WITHOUT cloning `_source`
            // and WITHOUT an id insert.  Pre-fix this branch cloned every
            // matching doc's source (~1 KB × 1M docs ≈ 1 GB churn/query) and
            // grew `seen_ids` to one String per match; under piled-up
            // concurrent search_after queries that ballooned RSS to an OOM
            // kill.  `seen_ids` now only tracks OFFERED hits, which is
            // bounded by heap-admission traffic (≈ cap·ln(N) typical).
            if let Some(topk) = sort_topk.as_deref_mut() {
                if seen_ids.contains(id_ref) {
                    continue;
                }
                let source_ref = doc.get("_source").unwrap_or(&doc);
                let score = if is_match_all {
                    1.0
                } else {
                    score_query_against_doc(query, source_ref)
                };
                let key = compute_sort_values(source_ref, score, id_ref, topk.fields.as_slice());
                if !topk.would_admit(&key) {
                    continue;
                }
                let id: String = id_ref.to_string();
                seen_ids.insert(id.clone());
                topk.offer_keyed(Hit {
                    id,
                    score,
                    source: source_ref.clone(),
                    sort: key,
                    explain: None,
                    highlight: None,
                    matched_queries: Vec::new(),
                });
                continue;
            }

            if all_hits.len() >= materialisation_limit {
                // Bounded collector is full.
                if count_authoritative {
                    // The EXACT `hits.total` is supplied independently (MatchAll
                    // live_doc_count / doc-values shortcut) and is overwritten
                    // by the caller after this scan, so there is nothing left to
                    // do once we have the top (from+size) sources.  Stop the
                    // scan instead of walking every remaining match — this is
                    // the O(from+size) fetch that F1 buys us.  The `*total_count`
                    // tallied so far is discarded by the caller's overwrite.
                    break;
                }
                // Count not authoritative for this shape: keep scanning to
                // tally the exact `hits.total`, just don't materialise sources.
                continue;
            }

            let id: String = id_ref.to_string();
            if seen_ids.contains(&id) {
                continue;
            }

            let source = doc.get("_source").cloned().unwrap_or_else(|| doc.clone());
            let score = if is_match_all {
                1.0
            } else {
                score_query_against_doc(query, &source)
            };

            seen_ids.insert(id.clone());
            all_hits.push(Hit {
                id,
                score,
                source,
                sort: Vec::new(),
                explain: None,
                highlight: None,
                matched_queries: Vec::new(),
            });
        }
    }
}

// ── do_flush ──────────────────────────────────────────────────────────────────

/// Flush the FTS memtable to a new on-disk segment.
///
/// This is a free function (not a method) so it can be called from both
/// `Index::flush()` and background tokio tasks without holding `Arc<Index>`.
///
/// The algorithm:
/// 1. Drain the memtable under a write lock, releasing it immediately.
/// 2. Flush the storage layer (writes the `.seg` file, swaps the snapshot,
///    checkpoints the WAL).
/// 3. Build the FTS inverted index files for the new segment using ordinal
///    positions (0, 1, 2, …) so that the segment search path can look up
///    stored docs by their ordinal.
#[allow(clippy::too_many_arguments)] // free fn threading the full flush-pipeline dependency set
async fn do_flush_shard(
    shard_idx: usize,
    store: Arc<IndexStore>,
    memtable: Arc<crate::memtable::ShardedFtsMemtable>,
    registry: Arc<AnalyzerRegistry>,
    data_dir: PathBuf,
    field_configs: HashMap<String, xerj_fts::index::FieldIndexConfig>,
    on_drained: impl FnOnce() + Send + 'static,
    // Publish-time cache warm (see `warm_segment_at_publish`): the index's
    // read-path caches, threaded through because this is a free fn without
    // access to `Index`.
    warm_caches: PublishWarmCaches,
) -> Result<()> {
    // V4 M4.5: no outer flush_lock — concurrent flushes are allowed.  The
    // memtable write lock below is the only atomicity point we need for
    // correctness (each concurrent flush drains a disjoint set of docs
    // and writes an independent segment).

    // ── Phase 1: atomic drain of BOTH memtables under the FTS write lock ──
    //
    // Ingest blocks only for the duration of this drain — which is fast
    // (memory move, no I/O).  Once we drop `mem`, new ingests can proceed
    // immediately against a fresh memtable while we do the expensive
    // segment + FTS side-car write.
    //
    // Correctness: the ingest path takes the FTS RwLock write BEFORE
    // pushing to both the FTS memtable and the storage memtable (via
    // `wal_append_batch` → storage mutex).  So while we hold the FTS write
    // lock, neither memtable can receive new entries.  Draining them both
    // under this single lock guarantees the FTS drain set == storage drain
    // set — and therefore the ordinals we encode in FTS side-cars match
    // the row positions in the stored section.
    // Peek BEFORE drain to determine if docs came from raw-bytes or FTS insert.
    let is_raw_bytes_path = memtable.peek_shard_has_raw_bytes(shard_idx);

    // THROWAWAY prof: per-flush finalize breakdown, gated on XERJ_PROF.
    let prof = std::env::var_os("XERJ_PROF").is_some();
    // Run the Phase-1 prep `par_iter` (JIT JSON re-parse + text-field
    // extraction, ~140 ms/flush) on the deprioritised INGEST pool rather
    // than the global rayon pool.  Reason: the sidecar build already does
    // this (see `build_fts`), but the prep par_iter did NOT — so a 16-wide
    // flush storm floods the GLOBAL rayon pool that concurrent search/agg
    // par_iters run on, queuing reads behind ~140 ms of flush CPU and
    // spiking read-under-write p99.  Offloading it to `background_pool`
    // (nice-10, separate from the search pool) keeps reads off that queue.
    // Env-overridable (default ON) for A/B — set XERJ_PREP_OFFLOAD=0 to
    // reproduce the pre-fix global-pool behaviour.
    let prep_offload = std::env::var("XERJ_PREP_OFFLOAD")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true);
    let t_flush_start = std::time::Instant::now();
    let prof_drain_us: u128;
    let mut prof_prep_us: u128 = 0;

    let drained_opt: Option<(
        Vec<(
            String,
            std::collections::HashMap<String, String>,
            std::sync::Arc<serde_json::Value>,
        )>,
        xerj_storage::index_store::DrainedMemtable,
    )> = {
        let t_drain = std::time::Instant::now();
        let raw = if is_raw_bytes_path {
            memtable.drain_shard_raw(shard_idx)
        } else {
            memtable.drain_shard(shard_idx)
        };
        prof_drain_us = t_drain.elapsed().as_micros();
        let t_prep = std::time::Instant::now();
        let _ = &t_prep;
        if raw.is_empty() {
            None
        } else {
            // M5.4 was a pure-perf optimisation that skipped Value-parse
            // + extract_text_fields when the shard's first entry was the
            // turbo raw-bytes path (CLI bulk).  The cost: the segment FTS
            // sidecar got an empty input set, the per-segment match_all
            // enumeration uses `seg.ids` + segment-FTS as its source of
            // truth, and the docs in raw-bytes segments were invisible
            // to match_all/term/range and the brute-force kNN pre-filter.
            // POV battery (2026-04-25) caught this as a 5-9% missing-doc
            // rate after `?refresh=true` on every bulk.  The save was
            // ~700ns/doc, but at 100 K docs/s that's still <100 ms of
            // CPU per flush — irrelevant against the 50-100 ms segment
            // write.  Always build the drained_fts payload so the FTS
            // sidecar carries the full doc set.
            // Rayon: the per-doc text-field extraction (plus the JSON
            // parse on the raw-bytes path) is pure and was a ~140 ms
            // SERIAL stretch per 31k-doc flush on the flush task —
            // par_iter preserves order and spreads it across the pool.
            let build_drained_fts = || {
                use rayon::prelude::*;
                raw.par_iter()
                    .map(|(_seq, doc_id, arc, bytes)| {
                        // Raw-bytes path stores Value::Null in `arc` and
                        // the actual JSON in `bytes` (skip-parse drain).
                        // Re-parse just-in-time for FTS field extraction.
                        // P2.2: the parsed path SHARES the memtable's Arc
                        // instead of deep-cloning every Value (~3.7s CPU
                        // per 1M docs on the background flush thread).
                        let val: std::sync::Arc<serde_json::Value> = if !arc.is_null() {
                            std::sync::Arc::clone(arc)
                        } else if !bytes.is_empty() {
                            std::sync::Arc::new(
                                serde_json::from_slice::<serde_json::Value>(bytes)
                                    .unwrap_or(serde_json::Value::Null),
                            )
                        } else {
                            std::sync::Arc::new(serde_json::Value::Null)
                        };
                        let fields = crate::memtable::extract_text_fields_from(&val);
                        (doc_id.clone(), fields, val)
                    })
                    .collect()
            };
            // Off the global rayon pool (default) so the flush-storm prep
            // doesn't queue ahead of concurrent search/agg par_iters.
            let drained_fts: Vec<(
                String,
                std::collections::HashMap<String, String>,
                std::sync::Arc<serde_json::Value>,
            )> = if prep_offload {
                crate::background_pool().install(build_drained_fts)
            } else {
                build_drained_fts()
            };

            let storage_entries: Vec<xerj_storage::index_store::MemEntry> = raw
                .into_iter()
                .map(
                    |(seq_no, doc_id, arc, raw_bytes)| xerj_storage::index_store::MemEntry {
                        seq_no,
                        doc_id,
                        source: Some(arc),
                        source_bytes: raw_bytes,
                    },
                )
                .collect();
            let storage_drained = xerj_storage::index_store::DrainedMemtable {
                entries: storage_entries,
            };
            prof_prep_us = t_prep.elapsed().as_micros();
            Some((drained_fts, storage_drained))
        }
    };

    // CRITICAL: fire `on_drained` BEFORE the early-return and BEFORE
    // Phase 2's slow segment I/O.  For callers holding a flush_sema
    // permit, this drops it now — releasing memtable-size back-pressure
    // (memtable bytes are already down post-drain) AND freeing the
    // permit so the next batch's per-shard threshold check can dispatch
    // a fresh flush task in parallel with our own Phase 2 I/O.  Pre-fix,
    // the permit was held for the entire do_flush_shard call including
    // the ~50-100 ms segment write, which capped concurrent drain
    // parallelism at `flush_sema = 16` even though the expensive
    // segment-write I/O could comfortably run at higher concurrency on
    // tmpfs.
    on_drained();

    let (drained_fts, storage_drained) = match drained_opt {
        Some(pair) => pair,
        None => return Ok(()),
    };

    // ── Phase 2: build segment + FTS side-cars without holding any lock ───
    //
    // From this point on, new ingests are appending to a fresh memtable in
    // parallel with the I/O we're about to do.  The only remaining
    // serialisation point is `flush_lock` (outer mutex on this function).
    let segments_dir = data_dir.join("segments");
    let registry_for_build = Arc::clone(&registry);
    let fts_doc_count = drained_fts.len();
    let segments_dir_for_dv = segments_dir.clone();
    let store_for_warm = Arc::clone(&store);
    // POV battery (B-3) caught that gating FTS+DV on the raw-bytes path
    // hides docs from match_all/term/range and from the kNN brute-force
    // pre-filter — the per-segment enumeration uses these sidecars as
    // its source of truth.  The "skip if raw-bytes" optimisation was
    // only meant for the turbo CLI path where the search cycle never
    // runs against the same data; in any user-facing flow we MUST
    // build them.  The cost (~700 ns / doc) is irrelevant next to the
    // 50-100 ms segment write that follows, and 100K-doc/s ingest
    // bench numbers are unaffected.
    let build_fts = move |meta: &xerj_storage::segment::SegmentMeta| -> xerj_storage::Result<()> {
        // Whole side-car build runs on the dedicated ingest pool so a
        // flush burst can't queue search/agg par_iters behind it on the
        // global rayon pool (read-under-write; see `crate::ingest_pool`).
        crate::background_pool().install(|| {
            // ── Doc-values side-car (always built) ────────────────────
            let t_dv = std::time::Instant::now();
            {
                let columns = build_doc_value_columns(
                    drained_fts
                        .iter()
                        .map(|(_id, _fields, src)| Some(src.as_ref())),
                );
                if !columns.is_empty() {
                    if let Err(e) =
                        write_doc_values_sidecar(&segments_dir_for_dv, meta.id.as_str(), &columns)
                    {
                        tracing::warn!("doc-values side-car write failed: {e}");
                    }
                }
            }
            let dv_us = t_dv.elapsed().as_micros();
            let mut fts_add_us: u128 = 0;
            let mut fts_finish_us: u128 = 0;
            // Build FTS sidecars when the drained docs have inverted-index data.
            if fts_doc_count > 0 {
                let mut fts_writer = xerj_fts::index::FtsIndexWriter::new(
                    &segments_dir,
                    meta.id.as_str(),
                    Arc::clone(&registry_for_build),
                );
                for (field_name, cfg) in &field_configs {
                    fts_writer.configure_field(field_name.clone(), cfg.clone());
                }
                let t_add = std::time::Instant::now();
                fts_writer.add_documents_parallel(&drained_fts);
                fts_add_us = t_add.elapsed().as_micros();
                let t_fin = std::time::Instant::now();
                if let Err(e) = fts_writer.finish() {
                    tracing::warn!("flush: FTS build failed: {e}");
                }
                fts_finish_us = t_fin.elapsed().as_micros();
            }
            if prof {
                eprintln!(
                    "XERJ_PROF flush-sidecar docs={} dv_us={} fts_add_us={} fts_finish_us={}",
                    fts_doc_count, dv_us, fts_add_us, fts_finish_us
                );
            }
            // Publish-time cache warm — MUST run here, inside the finaliser's
            // pre-publish callback: the snapshot rcu that makes this segment
            // visible happens right after `post_finish` returns, so by the
            // time any query can see the segment its stored slices, dv
            // columns, and sort shadows are already hot.  (A post-finalize
            // warm left a ~250 ms visible-but-cold window per flush storm;
            // 64 racing queries each decompressed all 16 fresh segments
            // inside it — the dec≈900 ms stall episodes.)
            warm_segment_at_publish(
                &store_for_warm,
                &segments_dir_for_dv,
                &warm_caches,
                meta.id.as_str(),
                meta.doc_count,
            );
        });
        let _ = &segments_dir;
        let _ = &drained_fts;

        Ok(())
    };

    // Hand the pre-drained storage entries to the finaliser.
    //
    // The segment encode + side-car build (CPU-heavy, ~600 ms) runs on the
    // deprioritised background pool (build_fts's inner install() is a
    // same-pool no-op).  Two flush-storm mitigations wrap it:
    //   1. `flush_finalize_gate()` bounds how many shard finalizes run at
    //      once, staggering the 16-wide storm's core contention off the
    //      readers (see the gate's doc comment for the measured win).
    //   2. `spawn_blocking` runs the (blocking) finalize on a blocking
    //      thread instead of inline, so it never parks a tokio async worker
    //      — a 16-wide storm used to park up to 16 workers, starving the
    //      concurrent bulk clients' HTTP handling and the reader's search
    //      dispatch.
    let t_finalize = std::time::Instant::now();
    let _fin_permit = crate::flush_finalize_gate().acquire().await.ok();
    let finalize_join = tokio::task::spawn_blocking(move || {
        crate::background_pool()
            .install(|| store.finalize_flush_with_publisher(storage_drained, build_fts))
    })
    .await;
    let meta = match finalize_join {
        Ok(Ok(Some(m))) => m,
        Ok(Ok(None)) => {
            tracing::warn!("storage finalize returned None — unexpected");
            return Ok(());
        }
        Ok(Err(e)) => return Err(e.into()),
        Err(join_e) => {
            tracing::warn!("finalize spawn_blocking join failed: {join_e}");
            return Ok(());
        }
    };
    if prof {
        eprintln!(
            "XERJ_PROF flush-total shard={} docs={} drain_us={} prep_us={} finalize_us={} total_us={}",
            shard_idx,
            fts_doc_count,
            prof_drain_us,
            prof_prep_us,
            t_finalize.elapsed().as_micros(),
            t_flush_start.elapsed().as_micros()
        );
    }

    info!(
        segment_id = meta.id.as_str(),
        doc_count = fts_doc_count,
        "memtable flushed to segment with FTS index"
    );
    Ok(())
}

// ── Sync flush coordination ───────────────────────────────────────────────────
//
// Rayon-scanner ingest (`xerj index`) pre-refactor called
// `rt_handle.block_on(index_batch_turbo_raw(..))` for every batch — ~40 % of
// syscall time was futex from those crossings.  `SyncFlushCoord` removes the
// hot-path crossing.  The sync ingest method (`index_batch_sync_raw`) is fully
// synchronous; when a shard crosses its per-shard threshold it uses the
// coord's **cached tokio Handle** to `Handle::spawn` a `do_flush_shard` task.
// `Handle::spawn` is a handful of atomics plus an mpsc send — no block_on,
// no dedicated flusher thread, no per-batch runtime crossing.  Back-pressure
// is a plain `std::thread::sleep` (nanosleep syscall) instead of
// `tokio::time::sleep` (timer wheel + semaphore + task wake).

pub(crate) struct SyncFlushCoord {
    /// Cached runtime handle captured at Index construction (when we're
    /// guaranteed to be inside `#[tokio::main]`).  Ingest threads use it
    /// to dispatch flush tasks without crossing runtime boundaries on
    /// every batch.
    rt: Option<tokio::runtime::Handle>,
    /// Cached FTS field configs — built once on first flush (requires a
    /// single `schema.read().await` under block_on) and reused.
    field_configs_cache:
        parking_lot::RwLock<Option<HashMap<String, xerj_fts::index::FieldIndexConfig>>>,
    /// Drain-complete signal.  Back-pressure in `index_batch_sync_raw`
    /// used `std::thread::sleep(5 ms) × 10` loops while memtable was
    /// above soft-block — 50 ms of wasted wall-time per batch even if
    /// a drain completed 1 ms in.  Now ingest threads `wait_for(5 ms)`
    /// on this Condvar and `on_drained` callbacks `notify_all`, so a
    /// drain wakes every blocked ingest thread within microseconds.
    drain_mu: parking_lot::Mutex<()>,
    drain_cv: parking_lot::Condvar,
}

impl SyncFlushCoord {
    fn new() -> Self {
        Self {
            rt: tokio::runtime::Handle::try_current().ok(),
            field_configs_cache: parking_lot::RwLock::new(None),
            drain_mu: parking_lot::Mutex::new(()),
            drain_cv: parking_lot::Condvar::new(),
        }
    }

    pub(crate) fn runtime(&self) -> Option<&tokio::runtime::Handle> {
        self.rt.as_ref()
    }

    /// Wake every ingest thread currently blocked on back-pressure.
    /// Called from `on_drained` callbacks right after Phase 1 drain
    /// completes — at that moment memtable bytes have dropped, so any
    /// back-pressure gate is potentially clearable.
    pub(crate) fn notify_drain(&self) {
        let _g = self.drain_mu.lock();
        self.drain_cv.notify_all();
    }

    /// Block until a drain fires (any shard) or `timeout` elapses.
    /// Single futex wait — NOT a tokio timer; no runtime involvement.
    pub(crate) fn wait_for_drain(&self, timeout: std::time::Duration) {
        let mut g = self.drain_mu.lock();
        let _ = self.drain_cv.wait_for(&mut g, timeout);
    }

    /// Get the cached field_configs, or build + cache them on first call.
    fn field_configs(
        &self,
        schema: &Arc<RwLock<ManagedSchema>>,
    ) -> HashMap<String, xerj_fts::index::FieldIndexConfig> {
        if let Some(cfg) = self.field_configs_cache.read().as_ref() {
            return cfg.clone();
        }
        let rt = match self.rt.as_ref() {
            Some(rt) => rt,
            None => return HashMap::new(),
        };
        let cfg = rt.block_on(async {
            let guard = schema.read().await;
            build_fts_field_configs(&guard.schema)
        });
        *self.field_configs_cache.write() = Some(cfg.clone());
        cfg
    }
}

// ── Free helpers ──────────────────────────────────────────────────────────────

/// Collapse with optional inner_hits emission. When `inner_hits_name` is
/// provided, the representative hit (first per group) carries the rest of
/// its group under a sentinel source key `__xy_collapse_group__` so the
/// response layer can build the inner_hits block. Limits (size, sort) are
/// respected by the response layer.
fn apply_collapse_with_inner(
    hits: Vec<Hit>,
    field: &str,
    inner_hits_spec: Option<&serde_json::Value>,
) -> Vec<Hit> {
    let mut groups: std::collections::BTreeMap<String, Vec<Hit>> =
        std::collections::BTreeMap::new();
    let mut group_order: Vec<String> = Vec::new();
    for hit in hits {
        let key = match hit.source.get(field) {
            Some(v) => match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => "\0null".to_string(),
                other => other.to_string(),
            },
            None => "\0missing".to_string(),
        };
        if !groups.contains_key(&key) {
            group_order.push(key.clone());
        }
        groups.entry(key).or_default().push(hit);
    }

    let mut result = Vec::with_capacity(group_order.len());
    for k in group_order {
        let mut members = groups.remove(&k).unwrap_or_default();
        if members.is_empty() {
            continue;
        }
        let mut lead = members.remove(0);
        if inner_hits_spec.is_some() {
            // Stash every member (including lead) so es_compat.rs can
            // build the inner_hits block with size/sort applied.
            let all_members: Vec<serde_json::Value> = std::iter::once(&lead)
                .chain(members.iter())
                .map(|h| {
                    serde_json::json!({
                        "_id": h.id,
                        "_score": h.score,
                        "_source": h.source,
                        "sort": h.sort,
                    })
                })
                .collect();
            if let Some(obj) = lead.source.as_object_mut() {
                obj.insert(
                    "__xy_collapse_group__".to_string(),
                    Value::Array(all_members),
                );
                obj.insert(
                    "__xy_collapse_spec__".to_string(),
                    inner_hits_spec.cloned().unwrap_or(Value::Null),
                );
            }
        }
        result.push(lead);
    }
    result
}

/// Produce a short human-readable summary of a query for logging.
fn summarize_query(query: &QueryNode) -> String {
    match query {
        QueryNode::MatchAll => "match_all".to_string(),
        QueryNode::MatchNone => "match_none".to_string(),
        QueryNode::Match { field, query, .. } => {
            format!("match({}:{})", field, &query[..query.len().min(50)])
        }
        QueryNode::Term { field, value, .. } => format!(
            "term({}:{})",
            field,
            value.to_string().chars().take(50).collect::<String>()
        ),
        QueryNode::Terms { field, values, .. } => {
            format!("terms({}, {} values)", field, values.len())
        }
        QueryNode::Range { field, .. } => format!("range({})", field),
        QueryNode::Bool {
            must,
            should,
            filter,
            must_not,
            ..
        } => {
            format!(
                "bool(must={}, should={}, filter={}, must_not={})",
                must.len(),
                should.len(),
                filter.len(),
                must_not.len()
            )
        }
        QueryNode::MultiMatch { query, fields, .. } => {
            format!(
                "multi_match({}, {} fields)",
                &query[..query.len().min(50)],
                fields.len()
            )
        }
        QueryNode::QueryString { query, .. } => {
            format!("query_string({})", &query[..query.len().min(50)])
        }
        QueryNode::Ids { values } => format!("ids({} ids)", values.len()),
        QueryNode::Knn { field, .. } => format!("knn({})", field),
        QueryNode::Nested { path, .. } => format!("nested({})", path),
        QueryNode::MoreLikeThis { like, .. } => format!("more_like_this({} texts)", like.len()),
        QueryNode::Pinned { ids, .. } => format!("pinned({} ids)", ids.len()),
        other => format!("{:?}", other).chars().take(100).collect(),
    }
}

/// Apply `_source` filtering to all hits.
///
/// * `Enabled(true)`  → return source unchanged (default).
/// * `Enabled(false)` → strip source entirely (set to `Value::Null`; serialised
///   as absent via `skip_serializing_if`).
/// * `Includes(fields)` → keep only the listed top-level fields.
/// * `Fields { includes, excludes }` → keep includes, then remove excludes.
///
/// Parse `s` under an ES-pattern `fmt` and return the corresponding
/// epoch-ms (or epoch-ns when the fractional seconds have ≥4 digits).
fn es_format_to_epoch_ms(s: &str, fmt: &str) -> Option<i64> {
    // Passthrough for epoch numerics.
    if fmt.contains("epoch_millis") {
        return s.parse::<i64>().ok();
    }
    if fmt.contains("epoch_second") {
        return s.parse::<i64>().ok().map(|v| v * 1000);
    }
    // Strict_date_optional_time_nanos: ISO-8601 — fall through to the
    // general ISO detector path (not this helper).
    if fmt.contains("strict_date_optional_time") || fmt.contains("date_optional_time") {
        return None;
    }
    // Translate ES pattern to chrono strftime. Minimal subset — covers
    // the tokens the YAML sort-format tests use.
    fn es_to_strftime(es_fmt: &str) -> String {
        let mut out = String::with_capacity(es_fmt.len() + 8);
        let bytes = es_fmt.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let rest = &es_fmt[i..];
            if rest.starts_with("yyyy") {
                out.push_str("%Y");
                i += 4;
            } else if rest.starts_with("yy") {
                out.push_str("%y");
                i += 2;
            } else if rest.starts_with("MM") {
                out.push_str("%m");
                i += 2;
            } else if rest.starts_with("dd") {
                out.push_str("%d");
                i += 2;
            } else if rest.starts_with("HH") {
                out.push_str("%H");
                i += 2;
            } else if rest.starts_with("mm") {
                out.push_str("%M");
                i += 2;
            } else if rest.starts_with("ss") {
                out.push_str("%S");
                i += 2;
            } else if rest.starts_with("SSSSSSSSS") {
                out.push_str("%9f");
                i += 9;
            } else if rest.starts_with("SSSSSS") {
                out.push_str("%6f");
                i += 6;
            } else if rest.starts_with("SSS") {
                out.push_str("%3f");
                i += 3;
            } else if rest.starts_with("Z") {
                out.push_str("%z");
                i += 1;
            } else if rest.starts_with("XXX") {
                out.push_str("%:z");
                i += 3;
            } else if rest.starts_with("'") {
                i += 1;
                while i < bytes.len() && bytes[i] != b'\'' {
                    out.push(bytes[i] as char);
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
            } else {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
        out
    }
    let pat = es_to_strftime(fmt);
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, &pat) {
        return Some(dt.and_utc().timestamp_millis());
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, &pat) {
        return d
            .and_hms_opt(0, 0, 0)
            .map(|dt| dt.and_utc().timestamp_millis());
    }
    None
}

fn apply_source_filter(hits: Vec<Hit>, filter: &SourceFilter) -> Vec<Hit> {
    match filter {
        SourceFilter::Enabled(true) => hits,
        // `_source: false`: keep the raw source so the response layer can
        // still resolve `fields` / `_ignored` / `highlight` against it —
        // `_source` emission is suppressed in es_compat.rs.
        SourceFilter::Enabled(false) => hits,
        SourceFilter::Includes(fields) => hits
            .into_iter()
            .map(|mut h| {
                h.source = filter_object(&h.source, fields, &[]);
                h
            })
            .collect(),
        SourceFilter::Fields { includes, excludes } => hits
            .into_iter()
            .map(|mut h| {
                h.source = filter_object(&h.source, includes, excludes);
                h
            })
            .collect(),
    }
}

/// Return a filtered copy of a JSON object keeping `includes` and removing `excludes`.
///
/// Supports dotted field paths: `include.field1` matches the nested
/// `{"include": {"field1": ...}}` structure. If `includes` is empty,
/// all fields are kept before exclusions are applied.
fn filter_object(source: &Value, includes: &[String], excludes: &[String]) -> Value {
    let obj = match source.as_object() {
        Some(o) => o,
        None => return source.clone(),
    };

    // Check if any include/exclude path is dotted (nested).
    let has_dotted = includes
        .iter()
        .chain(excludes.iter())
        .any(|f| f.contains('.'));

    if !has_dotted {
        // Simple top-level filtering.
        let mut result = serde_json::Map::new();
        for (k, v) in obj {
            let keep = if includes.is_empty() {
                true
            } else {
                includes.iter().any(|inc| field_matches(k, inc))
            };
            let excluded = excludes.iter().any(|exc| field_matches(k, exc));
            if keep && !excluded {
                result.insert(k.clone(), v.clone());
            }
        }
        return Value::Object(result);
    }

    // Dotted path filtering: flatten, filter, reconstruct.
    // For dotted includes like "include.field1", we need to check the
    // full path of each leaf value. Glob semantics: `*` matches a single
    // path segment (i.e. anything except a literal `.`), mirroring ES's
    // `_source` filter behavior. A bare `*` also matches any single
    // segment, so e.g. `*.field2` strips `{include,other}.field2`.
    fn matches_path(path: &str, patterns: &[String]) -> bool {
        fn glob_match(pat: &str, path: &str) -> bool {
            let pat_bytes = pat.as_bytes();
            let path_bytes = path.as_bytes();
            // Recursive impl with memoization would be overkill — glob
            // patterns here are short. Match greedy per character.
            fn walk(pat: &[u8], path: &[u8]) -> bool {
                let (mut p, mut s) = (0, 0);
                while p < pat.len() {
                    if pat[p] == b'*' {
                        // Wildcard matches 0+ characters but stops at '.'
                        // in ES source filter semantics.
                        let rest_pat = &pat[p + 1..];
                        // Try each possible match length for '*'.
                        for end in s..=path.len() {
                            // '*' cannot swallow a '.' — stop there.
                            if end > s && path[end - 1] == b'.' {
                                break;
                            }
                            if walk(rest_pat, &path[end..]) {
                                return true;
                            }
                        }
                        return false;
                    } else {
                        if s >= path.len() || pat[p] != path[s] {
                            return false;
                        }
                        p += 1;
                        s += 1;
                    }
                }
                s == path.len()
            }
            walk(pat_bytes, path_bytes)
        }
        patterns.iter().any(|p| {
            if p.contains('*') {
                glob_match(p, path) || (p.ends_with(".*") && path.starts_with(&p[..p.len() - 2]))
            } else {
                path == p || path.starts_with(&format!("{}.", p))
            }
        })
    }

    fn collect_and_filter(
        source: &Value,
        prefix: &str,
        includes: &[String],
        excludes: &[String],
    ) -> Value {
        match source {
            Value::Object(obj) => {
                let mut result = serde_json::Map::new();
                for (k, v) in obj {
                    let path = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{}.{}", prefix, k)
                    };
                    let keep = if includes.is_empty() {
                        true
                    } else {
                        matches_path(&path, includes)
                            || includes
                                .iter()
                                .any(|i| i.starts_with(&format!("{}.", path)))
                    };
                    let excluded = matches_path(&path, excludes);
                    if keep && !excluded {
                        let filtered = collect_and_filter(v, &path, includes, excludes);
                        if !filtered.is_null() {
                            result.insert(k.clone(), filtered);
                        }
                    }
                }
                if result.is_empty() && !prefix.is_empty() {
                    Value::Null
                } else {
                    Value::Object(result)
                }
            }
            _ => source.clone(),
        }
    }

    collect_and_filter(source, "", includes, excludes)
}

/// Simple field pattern matching — supports trailing `*` wildcard.
fn field_matches(field: &str, pattern: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        field.starts_with(prefix)
    } else {
        field == pattern
    }
}

/// Apply highlight to hits, generating fragments per requested field.
///
/// For each field listed in `highlight.fields`, the stored field value is
/// searched for matching terms.  A short fragment surrounding the first match
/// is returned with terms wrapped in configurable pre/post tags.
fn apply_highlight(hits: Vec<Hit>, hl: &HighlightRequest, query: &QueryNode) -> Vec<Hit> {
    if hl.fields.is_empty() {
        return hits;
    }
    // Pre-compute query terms ONCE before iterating hits (Opt 3).
    let query_terms = extract_highlight_terms(query);
    if query_terms.is_empty() {
        return hits;
    }

    let pre_default = hl.pre_tag.as_deref().unwrap_or("<em>");
    let post_default = hl.post_tag.as_deref().unwrap_or("</em>");
    let frag_size_default = hl.fragment_size.unwrap_or(150);
    // Cap at 3 fragments by default to bound per-hit work (Opt 3).
    let num_frags_default = hl.number_of_fragments.unwrap_or(3).min(3);

    hits.into_iter()
        .map(|mut hit| {
            // Skip entirely if the hit has no source (e.g. _source: false).
            if hit.source.is_null() {
                return hit;
            }

            let mut hl_map: std::collections::HashMap<String, Vec<String>> =
                std::collections::HashMap::new();
            for (field_name, field_opts) in &hl.fields {
                let pre = field_opts.pre_tag.as_deref().unwrap_or(pre_default);
                let post = field_opts.post_tag.as_deref().unwrap_or(post_default);
                let frag_size = field_opts.fragment_size.unwrap_or(frag_size_default);
                let num_frags_raw = field_opts.number_of_fragments.unwrap_or(num_frags_default);

                if let Some(field_val) = get_field_value(&hit.source, field_name) {
                    let text = match &field_val {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    let text_lower = text.to_lowercase();
                    let has_any = query_terms.iter().any(|t| text_lower.contains(t.as_str()));
                    if !has_any {
                        continue;
                    }
                    let fragments = if num_frags_raw == 0 {
                        // ES semantics: `number_of_fragments: 0` disables
                        // fragmenting entirely and emits the full text
                        // with every occurrence of every query term
                        // wrapped in the pre/post tags. Whitespace at
                        // the head/tail is preserved verbatim.
                        vec![highlight_full_text(
                            &text,
                            &text_lower,
                            &query_terms,
                            pre,
                            post,
                        )]
                    } else {
                        let num_frags = num_frags_raw.min(3);
                        build_highlight_fragments(
                            &text,
                            &text_lower,
                            &query_terms,
                            pre,
                            post,
                            frag_size,
                            num_frags,
                        )
                        // ES's unified highlighter trims leading/trailing
                        // whitespace from every emitted fragment.
                        .into_iter()
                        .map(|f| f.trim().to_string())
                        .collect()
                    };
                    if !fragments.is_empty() {
                        hl_map.insert(field_name.clone(), fragments);
                    }
                }
            }
            if !hl_map.is_empty() {
                hit.highlight = Some(hl_map);
            }
            hit
        })
        .collect()
}

/// Extract query terms for highlighting.
///
/// Returns lowercase tokens that should be highlighted.
fn extract_highlight_terms(query: &QueryNode) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    collect_highlight_terms(query, &mut terms);
    // Deduplicate.
    terms.sort();
    terms.dedup();
    terms
}

fn collect_highlight_terms(query: &QueryNode, out: &mut Vec<String>) {
    match query {
        QueryNode::Match { query, .. }
        | QueryNode::MatchPhrase { query, .. }
        | QueryNode::MultiMatch { query, .. }
        | QueryNode::QueryString { query, .. } => {
            for tok in query.split_whitespace() {
                let tok = tok
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase();
                if !tok.is_empty() {
                    out.push(tok);
                }
            }
        }
        QueryNode::Term { value, .. } => {
            if let Some(s) = value.as_str() {
                out.push(s.to_lowercase());
            }
        }
        QueryNode::Bool {
            must,
            should,
            filter,
            ..
        } => {
            for q in must.iter().chain(should.iter()).chain(filter.iter()) {
                collect_highlight_terms(q, out);
            }
        }
        QueryNode::Boosted { query, .. }
        | QueryNode::Constant { query, .. }
        | QueryNode::FunctionScore { query, .. } => {
            collect_highlight_terms(query, out);
        }
        _ => {}
    }
}

/// Build highlighted fragments for a single field value.
///
/// `text_lower` must be `text.to_lowercase()` (pre-computed by the caller to
/// avoid a redundant allocation per field per hit).
///
/// Scans `text_lower` for byte-level occurrences of each query term, then
/// extracts a window of `frag_size` bytes centred on each match.  Windows are
/// snapped to UTF-8 character boundaries so the output is always valid UTF-8.
/// Returns up to `num_frags` non-overlapping fragments with `pre`/`post` tags.
/// Render the full source text with every match of every query term
/// wrapped in the pre/post tags. Used when the caller requests
/// `number_of_fragments: 0`, which disables fragmenting.
fn highlight_full_text(
    text: &str,
    text_lower: &str,
    terms: &[String],
    pre: &str,
    post: &str,
) -> String {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for term in terms {
        if term.is_empty() {
            continue;
        }
        let mut start = 0;
        while let Some(pos) = text_lower[start..].find(term.as_str()) {
            let abs = start + pos;
            spans.push((abs, abs + term.len()));
            start = abs + term.len();
        }
    }
    spans.sort_by_key(|&(s, _)| s);
    // Merge overlaps — a term that spans into another term should not
    // produce double tags.
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
    for (s, e) in spans {
        if let Some(last) = merged.last_mut() {
            if s <= last.1 {
                last.1 = last.1.max(e);
                continue;
            }
        }
        merged.push((s, e));
    }
    let mut out = String::with_capacity(text.len() + merged.len() * (pre.len() + post.len()));
    let mut cursor = 0;
    for (s, e) in merged {
        if cursor < s {
            out.push_str(&text[cursor..s]);
        }
        out.push_str(pre);
        out.push_str(&text[s..e]);
        out.push_str(post);
        cursor = e;
    }
    if cursor < text.len() {
        out.push_str(&text[cursor..]);
    }
    out
}

fn build_highlight_fragments(
    text: &str,
    text_lower: &str,
    terms: &[String],
    pre: &str,
    post: &str,
    frag_size: usize,
    num_frags: usize,
) -> Vec<String> {
    // Collect all match byte-offsets (start, end) across all terms.
    let mut match_positions: Vec<(usize, usize)> = Vec::new();
    for term in terms {
        let mut start = 0;
        while let Some(pos) = text_lower[start..].find(term.as_str()) {
            let abs = start + pos;
            match_positions.push((abs, abs + term.len()));
            start = abs + term.len();
            if match_positions.len() > num_frags * 4 {
                break; // enough candidates — stop scanning early
            }
        }
    }

    if match_positions.is_empty() {
        return Vec::new();
    }

    // Sort by position so we pick the earliest matches first.
    match_positions.sort_by_key(|&(s, _)| s);

    let text_bytes = text.as_bytes();
    let total_bytes = text_bytes.len();

    /// Snap `pos` forward to the next UTF-8 character boundary.
    fn snap_start(bytes: &[u8], pos: usize) -> usize {
        let mut p = pos.min(bytes.len());
        // Continuation bytes (10xxxxxx) are not char starts; advance past them.
        while p < bytes.len() && bytes[p] & 0xC0 == 0x80 {
            p += 1;
        }
        p
    }

    /// Snap `pos` backward to a valid UTF-8 char boundary (not mid-char).
    fn snap_end(bytes: &[u8], pos: usize) -> usize {
        let mut p = pos.min(bytes.len());
        // Step back while we're pointing at a continuation byte.
        while p > 0 && p < bytes.len() && bytes[p] & 0xC0 == 0x80 {
            p -= 1;
        }
        p
    }

    let mut fragments: Vec<String> = Vec::new();
    let mut last_win_end: usize = 0;

    for (byte_start, byte_end) in &match_positions {
        if fragments.len() >= num_frags {
            break;
        }

        // Centre a byte window around the match.
        let half = frag_size / 2;
        let win_start_raw = byte_start.saturating_sub(half);
        let win_end_raw = (win_start_raw + frag_size).min(total_bytes);
        let win_start_raw = win_end_raw.saturating_sub(frag_size);

        // Snap to valid UTF-8 boundaries.
        let win_start = snap_start(text_bytes, win_start_raw);
        let win_end = snap_end(text_bytes, win_end_raw);
        let win_end = win_end.max(win_start);

        // Skip if this window overlaps with the previous fragment (avoid duplicates).
        if win_start < last_win_end {
            continue;
        }
        last_win_end = win_end;

        // Clamp match offsets to the window.
        let match_start = (*byte_start).max(win_start).min(win_end);
        let match_end = (*byte_end).max(win_start).min(win_end);

        // Build the fragment: prefix + pre-tag + matched span + post-tag + suffix.
        let mut frag = String::with_capacity(frag_size + pre.len() + post.len() + 6);
        if win_start > 0 {
            frag.push_str("...");
        }
        // Text before the match (within the window).
        if match_start > win_start {
            // Use the original text casing for the context surrounding the match.
            frag.push_str(&text[win_start..match_start]);
        }
        frag.push_str(pre);
        // The matched span (from original text, not lowercased).
        frag.push_str(&text[match_start..match_end]);
        frag.push_str(post);
        // Text after the match (within the window).
        if match_end < win_end {
            frag.push_str(&text[match_end..win_end]);
        }
        if win_end < total_bytes {
            frag.push_str("...");
        }
        fragments.push(frag);
    }

    fragments
}

/// Build an `EmbeddingProxy` from `Config.embedding`. Returns `None`
/// when no endpoint is configured — semantic queries then return a
/// helpful 400 ("configure embedding.default_endpoint") instead of
/// a runtime crash.
fn make_embedding_proxy(
    cfg: &xerj_common::config::EmbeddingConfig,
) -> Option<xerj_ai::embed::EmbeddingProxy> {
    if cfg.default_endpoint.is_empty() {
        return None;
    }
    let proxy_cfg = xerj_ai::embed::EmbeddingProxyConfig {
        endpoint: cfg.default_endpoint.clone(),
        api_key: std::env::var("XERJ_EMBEDDING_API_KEY").ok(),
        model: cfg.default_model.clone(),
        timeout_secs: cfg.timeout_ms / 1000,
        max_concurrent: 4,
        max_retries: 3,
    };
    match xerj_ai::embed::EmbeddingProxy::new(proxy_cfg) {
        Ok(p) => Some(p),
        Err(e) => {
            warn!(error = %e, "embedding proxy init failed — semantic queries will 400");
            None
        }
    }
}

// ── HNSW persistence helpers (module-level, called from sync `Index::open`)
//
// Returns the parsed graph + id maps + next_id when both files load
// cleanly, or None on any error / missing-file. Logging is at warn!()
// level so a corrupt snapshot is never silent — operator sees the
// reason and can decide whether to run a manual rebuild.

struct LoadedHnsw {
    graph: xerj_vector::HnswIndex,
    id_map: HashMap<String, u64>,
    id_rev: HashMap<u64, String>,
    next_id: u64,
}

fn load_hnsw_artifacts_sync(hnsw_dir: &Path) -> Option<LoadedHnsw> {
    let graph_path = hnsw_dir.join("graph.bin");
    let ids_path = hnsw_dir.join("ids.json");
    if !graph_path.exists() || !ids_path.exists() {
        return None;
    }
    let graph = match xerj_vector::HnswIndex::load_from(&graph_path) {
        Ok(g) => g,
        Err(e) => {
            warn!(error = %e, "HNSW load: graph file rejected — falling back to WAL replay");
            return None;
        }
    };
    let ids_bytes = match std::fs::read(&ids_path) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "HNSW load: ids.json missing — graph dropped");
            return None;
        }
    };
    let ids: serde_json::Value = match serde_json::from_slice(&ids_bytes) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "HNSW load: ids.json corrupt — graph dropped");
            return None;
        }
    };
    let next_id = ids.get("next_id").and_then(|v| v.as_u64()).unwrap_or(1);
    let map_obj = match ids.get("map").and_then(|v| v.as_object()) {
        Some(m) => m,
        None => {
            warn!("HNSW load: ids.json missing `map` — graph dropped");
            return None;
        }
    };
    let mut id_map: HashMap<String, u64> = HashMap::with_capacity(map_obj.len());
    let mut id_rev: HashMap<u64, String> = HashMap::with_capacity(map_obj.len());
    for (doc_id, node_id_v) in map_obj.iter() {
        if let Some(nid) = node_id_v.as_u64() {
            id_map.insert(doc_id.clone(), nid);
            id_rev.insert(nid, doc_id.clone());
        }
    }
    Some(LoadedHnsw {
        graph,
        id_map,
        id_rev,
        next_id,
    })
}

fn store_config_from(config: &Config) -> IndexStoreConfig {
    let sync_mode = match config.storage.wal_sync {
        xerj_common::config::WalSync::Sync => SyncMode::Strict,
        xerj_common::config::WalSync::Batched | xerj_common::config::WalSync::Async => {
            SyncMode::Batched
        }
    };
    IndexStoreConfig {
        memtable_max_bytes: (config.storage.flush_size_mb * 1024 * 1024) as usize,
        wal_max_size_bytes: config.storage.wal_max_size_mb * 1024 * 1024,
        sync_mode,
        schema_version: 1,
        storage_mode: xerj_storage::StorageMode::Local,
        num_wal_shards: config.engine.ingest_shards,
    }
}

/// Write `bytes` to `path` atomically: write a same-directory temp file,
/// fsync it, then rename over the target.  A kill -9 mid-write leaves
/// either the old file or the new file on disk, never a truncated one.
/// This matters for `schema.json`: `load_schema` treats a torn file as
/// "no schema" and silently falls back to an empty dynamic mapping,
/// which is a mapping-loss corruption after crash.
pub fn write_file_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        use std::io::Write as _;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    // Make the rename itself durable across power loss.
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

fn load_schema(index_dir: &Path) -> std::result::Result<ManagedSchema, ()> {
    let path = index_dir.join("schema.json");
    let bytes = std::fs::read(path).map_err(|_| ())?;
    serde_json::from_slice(&bytes).map_err(|_| ())
}

fn load_settings(index_dir: &Path) -> std::result::Result<Value, ()> {
    let path = index_dir.join("settings.json");
    let bytes = std::fs::read(path).map_err(|_| ())?;
    serde_json::from_slice(&bytes).map_err(|_| ())
}

// ── Date math in index names ──────────────────────────────────────────────────

/// Resolve date math expressions in an index name.
///
/// ES syntax: `<log-{now/d}>` → `log-2026.04.11`
///
/// Supported expressions:
/// - `now`       → current UTC date-time (formatted as yyyy.MM.dd)
/// - `now/d`     → today rounded to day
/// - `now-1d/d`  → yesterday rounded to day
/// - `now+1d/d`  → tomorrow rounded to day
pub fn resolve_date_math(name: &str) -> String {
    if let (Some(start), Some(end)) = (name.find('<'), name.rfind('>')) {
        if start < end {
            let inner = &name[start + 1..end];
            let resolved = resolve_date_math_inner(inner);
            let mut result = name[..start].to_string();
            result.push_str(&resolved);
            result.push_str(&name[end + 1..]);
            return result;
        }
    }
    name.to_string()
}

fn resolve_date_math_inner(expr: &str) -> String {
    let brace_start = match expr.find('{') {
        Some(i) => i,
        None => return expr.to_string(),
    };
    let brace_end = match expr.rfind('}') {
        Some(i) => i,
        None => return expr.to_string(),
    };

    let prefix = &expr[..brace_start];
    let date_part = &expr[brace_start + 1..brace_end];

    let (math_expr, fmt) = if let Some(inner_brace) = date_part.find('{') {
        let inner_end = date_part.rfind('}').unwrap_or(date_part.len());
        (
            &date_part[..inner_brace],
            &date_part[inner_brace + 1..inner_end],
        )
    } else {
        (date_part, "yyyy.MM.dd")
    };

    let now = chrono::Utc::now();
    let date = resolve_now_date(math_expr, now);
    let formatted = format_date_expr(date, fmt);
    format!("{}{}", prefix, formatted)
}

fn resolve_now_date(
    expr: &str,
    base: chrono::DateTime<chrono::Utc>,
) -> chrono::DateTime<chrono::Utc> {
    use chrono::Duration;
    use chrono::{Datelike, TimeZone};

    let rest = match expr.strip_prefix("now") {
        Some(r) => r,
        None => return base,
    };

    let (mut dt, rest) = if rest.starts_with('+') || rest.starts_with('-') {
        let sign: i64 = if rest.starts_with('+') { 1 } else { -1 };
        let rest = &rest[1..];
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let n: i64 = rest[..end].parse().unwrap_or(0) * sign;
        let (unit, rest) = if end < rest.len() {
            (&rest[end..end + 1], &rest[end + 1..])
        } else {
            ("", &rest[end..])
        };
        let offset = match unit {
            "d" => Duration::days(n),
            "h" => Duration::hours(n),
            "w" => Duration::weeks(n),
            "M" => Duration::days(n * 30),
            "y" => Duration::days(n * 365),
            _ => Duration::zero(),
        };
        (base + offset, rest)
    } else {
        (base, rest)
    };

    if let Some(round_rest) = rest.strip_prefix('/') {
        let unit = round_rest.chars().next().unwrap_or('d');
        dt = match unit {
            'd' => chrono::Utc
                .with_ymd_and_hms(dt.year(), dt.month(), dt.day(), 0, 0, 0)
                .single()
                .unwrap_or(dt),
            'M' => chrono::Utc
                .with_ymd_and_hms(dt.year(), dt.month(), 1, 0, 0, 0)
                .single()
                .unwrap_or(dt),
            'y' => chrono::Utc
                .with_ymd_and_hms(dt.year(), 1, 1, 0, 0, 0)
                .single()
                .unwrap_or(dt),
            _ => dt,
        };
    }
    dt
}

fn format_date_expr(dt: chrono::DateTime<chrono::Utc>, fmt: &str) -> String {
    use chrono::Datelike;
    use chrono::Timelike;
    let mut s = fmt.to_string();
    s = s.replace("yyyy", &format!("{:04}", dt.year()));
    s = s.replace("MM", &format!("{:02}", dt.month()));
    s = s.replace("dd", &format!("{:02}", dt.day()));
    s = s.replace("HH", &format!("{:02}", dt.hour()));
    s = s.replace("mm", &format!("{:02}", dt.minute()));
    s = s.replace("ss", &format!("{:02}", dt.second()));
    s
}

// ── Field alias resolution ────────────────────────────────────────────────────

/// Resolve a field name through alias mappings.
///
/// If `field` is mapped as `{"type": "alias", "path": "target"}` in the schema,
/// returns the target field name. Otherwise returns the original field name.
pub fn resolve_field_alias(schema: &Schema, field: &str) -> String {
    for fc in &schema.fields {
        if fc.name == field && fc.field_type == FieldType::Object {
            // Alias fields are stored as Object type with the path in options.
            // The copy_to target is stored in the field name following convention.
            // We store alias path via a special naming convention in the schema.
        }
    }
    // Walk schema fields looking for an alias that matches.
    for fc in &schema.fields {
        if fc.name == field {
            // Check if this field has alias metadata stored in its null_value option
            // (we repurpose null_value to store the alias path for alias-type fields).
            if let Some(serde_json::Value::String(path)) = &fc.options.null_value {
                if let Some(rest) = path.strip_prefix("__alias__:") {
                    return rest.to_string();
                }
            }
        }
    }
    field.to_string()
}

/// Compute a Lucene-style similarity score between two vectors.
///
/// Mirrors Lucene's `VectorSimilarityFunction` conversions so the
/// resulting `_score` matches what ES returns: higher is better.
///
/// * `cosine`:              `(1 + cosine_sim(a,b)) / 2` in `[0, 1]`.
/// * `dot_product`:         `sigmoid(dot(a,b))` — assumes normalised vectors.
/// * `max_inner_product`:   `dot(a,b) + 1` when ≥ 0, else `1/(1-dot(a,b))`.
/// * `l2_norm`:             `1 / (1 + sq_euclid(a,b))`.
///
/// For any unknown similarity label we default to cosine-equivalent.
///
/// Sums in f64 to match Painless `dotProduct` precision — vector
/// scores stay byte-identical between the brute-force knn path and
/// the script_score-via-Painless path, which is what
/// 46_knn_search_bbq_ivf's "rescore vector consistency" tests assert.
fn compute_vector_similarity(sim: &str, a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let dot: f64 = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| (*x as f64) * (*y as f64))
        .sum();
    let result: f64 = match sim {
        "l2_norm" => {
            let sq: f64 = a
                .iter()
                .zip(b.iter())
                .map(|(x, y)| {
                    let d = (*x as f64) - (*y as f64);
                    d * d
                })
                .sum();
            1.0 / (1.0 + sq)
        }
        "dot_product" => (1.0 + dot) / 2.0,
        "max_inner_product" => {
            if dot < 0.0 {
                1.0 / (1.0 - dot)
            } else {
                dot + 1.0
            }
        }
        _ => {
            let na: f64 = a
                .iter()
                .map(|x| (*x as f64) * (*x as f64))
                .sum::<f64>()
                .sqrt();
            let nb: f64 = b
                .iter()
                .map(|x| (*x as f64) * (*x as f64))
                .sum::<f64>()
                .sqrt();
            let denom = na * nb;
            let cos = if denom > 0.0 { dot / denom } else { 0.0 };
            (1.0 + cos) / 2.0
        }
    };
    result as f32
}

/// Unwrap a top-level Knn query, optionally nested under a single transparent
/// wrapper (Boosted, Constant, Named, or a single-clause Bool must/should).
/// Returns `(field, query_vector, k, filter)` when the query is a KNN
/// short-circuit candidate, `None` otherwise.
///
/// Filters attached to the Knn node are preserved; filters attached to the
/// wrapping Bool are merged in via Bool::filter semantics.
#[allow(clippy::type_complexity)]
fn peel_knn_query(
    q: &QueryNode,
) -> Option<(
    String,
    Vec<f32>,
    usize,
    Option<usize>,
    Option<Box<QueryNode>>,
)> {
    match q {
        QueryNode::Knn {
            field,
            vector,
            k,
            num_candidates,
            filter,
            ..
        } => Some((
            field.clone(),
            vector.clone(),
            *k,
            *num_candidates,
            filter.clone(),
        )),
        QueryNode::Constant { query, .. }
        | QueryNode::Boosted { query, .. }
        | QueryNode::Named { query, .. } => peel_knn_query(query),
        QueryNode::Bool {
            must,
            should,
            filter,
            must_not,
            ..
        } => {
            // Only handle the simple case: exactly one clause containing a
            // KNN, possibly with extra filters to AND in.
            if !must_not.is_empty() {
                return None;
            }
            let candidates: Vec<&QueryNode> = must.iter().chain(should.iter()).collect();
            if candidates.len() != 1 {
                return None;
            }
            let (f, v, k, nc, inner_filter) = peel_knn_query(candidates[0])?;
            // Combine inner_filter + bool's filter clauses.
            let mut merged_filters: Vec<QueryNode> = filter.clone();
            if let Some(fi) = inner_filter {
                merged_filters.push(*fi);
            }
            let final_filter: Option<Box<QueryNode>> = match merged_filters.len() {
                0 => None,
                1 => Some(Box::new(merged_filters.into_iter().next().unwrap())),
                _ => Some(Box::new(QueryNode::Bool {
                    must: Vec::new(),
                    should: Vec::new(),
                    filter: merged_filters,
                    must_not: Vec::new(),
                    minimum_should_match: None,
                })),
            };
            Some((f, v, k, nc, final_filter))
        }
        _ => None,
    }
}

// Peel a nested-KNN query and partition surrounding clauses into
// pre-filters (applied BEFORE top-k ranking — align with ES `knn.filter`
// and `bool.filter`) and post-filters (applied AFTER top-k — align with
// sibling `bool.must` clauses).
//
// Returns `(nested_path, knn_field, query_vector, k, pre_filter, post_filter)`
// when matched.
// ── Hybrid fusion helpers ────────────────────────────────────────────────────
//
// Both fuse_rrf and fuse_linear take a slice of (hits, weight) pairs and
// return a single Vec<Hit> sorted by combined score descending. The same
// doc_id appearing across multiple lists collapses to one Hit; the source
// is taken from the first list that produced it (sub-lists may differ on
// what they materialise, e.g. a kNN list returns the full source while
// a BM25 list with `_source: false` returns null — first non-null wins).

/// RRF (reciprocal-rank-fusion) combiner. Each doc d in list i at
/// 1-based rank r_i contributes `weight_i / (k + r_i)` to its
/// combined score. The smoothing constant k defaults to 60 (ES /
/// OpenSearch / TREC convention) and is small enough that the top
/// few ranks still dominate.
fn fuse_rrf(sub_results: &[(Vec<Hit>, f32)], k: u32) -> Vec<Hit> {
    let kf = k as f32;
    // doc_id → (combined_score, picked_hit). Picked Hit is mutated to
    // carry the fused score on output.
    let mut accum: HashMap<String, (f32, Hit)> = HashMap::new();
    for (hits, weight) in sub_results {
        for (rank_zero_based, h) in hits.iter().enumerate() {
            let rank = (rank_zero_based + 1) as f32;
            let contrib = weight / (kf + rank);
            match accum.get_mut(&h.id) {
                Some(existing) => {
                    existing.0 += contrib;
                    // Take a non-null source from the second list if the
                    // first had _source: false.
                    if existing.1.source.is_null() && !h.source.is_null() {
                        existing.1.source = h.source.clone();
                    }
                }
                None => {
                    let mut hit = h.clone();
                    hit.score = 0.0; // will overwrite from combined
                    accum.insert(h.id.clone(), (contrib, hit));
                }
            }
        }
    }
    let mut combined: Vec<Hit> = accum
        .into_iter()
        .map(|(_, (score, mut h))| {
            h.score = score;
            h
        })
        .collect();
    combined.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    combined
}

/// Linear combiner. Within each sub-list, normalise scores to [0,1]
/// via min-max (constant-score lists collapse to all-zero), then sum
/// weight × normalised across lists. Cheaper than RRF; sensitive to
/// score outliers.
fn fuse_linear(sub_results: &[(Vec<Hit>, f32)]) -> Vec<Hit> {
    let mut accum: HashMap<String, (f32, Hit)> = HashMap::new();
    for (hits, weight) in sub_results {
        // Min-max normalise within this sub-list.
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for h in hits {
            if h.score < lo {
                lo = h.score;
            }
            if h.score > hi {
                hi = h.score;
            }
        }
        let span = hi - lo;
        for h in hits {
            let norm = if span > 0.0 {
                (h.score - lo) / span
            } else {
                0.0
            };
            let contrib = weight * norm;
            match accum.get_mut(&h.id) {
                Some(existing) => {
                    existing.0 += contrib;
                    if existing.1.source.is_null() && !h.source.is_null() {
                        existing.1.source = h.source.clone();
                    }
                }
                None => {
                    let mut hit = h.clone();
                    hit.score = 0.0;
                    accum.insert(h.id.clone(), (contrib, hit));
                }
            }
        }
    }
    let mut combined: Vec<Hit> = accum
        .into_iter()
        .map(|(_, (score, mut h))| {
            h.score = score;
            h
        })
        .collect();
    combined.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    combined
}

/// Detect a semantic query (parser node `QueryNode::SemanticSearch`)
/// possibly wrapped in the harmless `Constant`/`Boosted`/`Named`
/// decorators. Returns `(field, text, k, filter)` ready to feed
/// into `Index::run_semantic`.
fn peel_semantic_query(q: &QueryNode) -> Option<(String, String, usize, Option<Box<QueryNode>>)> {
    match q {
        QueryNode::SemanticSearch {
            field,
            text,
            k,
            filter,
            ..
        } => Some((field.clone(), text.clone(), *k, filter.clone())),
        QueryNode::Constant { query, .. }
        | QueryNode::Boosted { query, .. }
        | QueryNode::Named { query, .. } => peel_semantic_query(query),
        _ => None,
    }
}

/// Detect a hybrid query that we can fan out across sub-searches
/// and recombine via fusion. Allows Hybrid wrapped in the harmless
/// `Constant` / `Boosted` / `Named` decorators since the parser
/// emits those for query-level boost / `_name`.
fn peel_hybrid_query(
    q: &QueryNode,
) -> Option<(
    Vec<xerj_query::ast::WeightedQuery>,
    xerj_query::ast::FusionStrategy,
)> {
    match q {
        QueryNode::Hybrid { queries, fusion } => {
            // Empty hybrid is a programming error in the parser; treat
            // as not-hybrid so the doc-scan path returns no hits
            // gracefully.
            if queries.is_empty() {
                return None;
            }
            Some((queries.clone(), fusion.clone()))
        }
        QueryNode::Constant { query, .. }
        | QueryNode::Boosted { query, .. }
        | QueryNode::Named { query, .. } => peel_hybrid_query(query),
        _ => None,
    }
}

#[allow(clippy::type_complexity)]
fn peel_nested_knn_query(
    q: &QueryNode,
) -> Option<(
    String,
    String,
    Vec<f32>,
    usize,
    Option<usize>,
    Option<Box<QueryNode>>,
    Option<Box<QueryNode>>,
)> {
    // Walk transparent wrappers + a bool with exactly one Nested(Knn)
    // clause.  Return (inner_nested_node, pre_filters, post_filters).
    #[allow(clippy::type_complexity)]
    fn walk(q: &QueryNode) -> Option<(&QueryNode, Vec<QueryNode>, Vec<QueryNode>)> {
        match q {
            QueryNode::Nested { .. } => Some((q, Vec::new(), Vec::new())),
            QueryNode::Constant { query, .. }
            | QueryNode::Boosted { query, .. }
            | QueryNode::Named { query, .. } => walk(query),
            QueryNode::Bool {
                must,
                should,
                filter,
                must_not,
                ..
            } => {
                if !must_not.is_empty() {
                    return None;
                }
                // Find the single Nested(Knn) clause among must/should.
                let mut nested_from_must: Option<(usize, bool)> = None;
                for (i, c) in must.iter().enumerate() {
                    if walk(c).is_some() {
                        if nested_from_must.is_some() {
                            return None;
                        }
                        nested_from_must = Some((i, true));
                    }
                }
                let mut nested_from_should: Option<usize> = None;
                for (i, c) in should.iter().enumerate() {
                    if walk(c).is_some() {
                        if nested_from_must.is_some() || nested_from_should.is_some() {
                            return None;
                        }
                        nested_from_should = Some(i);
                    }
                }
                let (inner, mut pre, mut post) = if let Some((i, _)) = nested_from_must {
                    walk(&must[i])?
                } else if let Some(i) = nested_from_should {
                    walk(&should[i])?
                } else {
                    return None;
                };
                // Siblings in must → post-filters.
                for (i, c) in must.iter().enumerate() {
                    if let Some((ni, _)) = nested_from_must {
                        if i == ni {
                            continue;
                        }
                    }
                    post.push(c.clone());
                }
                for (i, c) in should.iter().enumerate() {
                    if Some(i) == nested_from_should {
                        continue;
                    }
                    post.push(c.clone());
                }
                // bool.filter[] → pre-filters.
                for f in filter {
                    pre.push(f.clone());
                }
                Some((inner, pre, post))
            }
            _ => None,
        }
    }
    let (nested_node, pre_filters, post_filters) = walk(q)?;
    let (path, inner_q) = match nested_node {
        QueryNode::Nested { path, query, .. } => (path.clone(), query.as_ref()),
        _ => return None,
    };
    let (field, vector, k, num_candidates, inner_filter) = peel_knn_query(inner_q)?;
    // Knn's own filter sub-query is a pre-filter.
    let mut pre_all: Vec<QueryNode> = pre_filters;
    if let Some(f) = inner_filter {
        pre_all.push(*f);
    }
    let to_box = |mut v: Vec<QueryNode>| -> Option<Box<QueryNode>> {
        match v.len() {
            0 => None,
            1 => Some(Box::new(v.pop().unwrap())),
            _ => Some(Box::new(QueryNode::Bool {
                must: Vec::new(),
                should: Vec::new(),
                filter: v,
                must_not: Vec::new(),
                minimum_should_match: None,
            })),
        }
    };
    Some((
        path,
        field,
        vector,
        k,
        num_candidates,
        to_box(pre_all),
        to_box(post_filters),
    ))
}

/// Look up the declared similarity for a dense_vector field.  Walks
/// nested `FieldConfig.fields` for dotted paths (`foo.vec`) so KNN
/// against sub-fields works.  Defaults to "cosine" — the ES default
/// for indexed dense_vector fields.
fn lookup_vector_similarity(schema: &Schema, field: &str) -> String {
    fn find<'a>(fields: &'a [FieldConfig], path: &[&str]) -> Option<&'a FieldConfig> {
        if path.is_empty() {
            return None;
        }
        let head = path[0];
        let tail = &path[1..];
        for fc in fields {
            if fc.name == head {
                if tail.is_empty() {
                    return Some(fc);
                }
                return find(&fc.fields, tail);
            }
        }
        None
    }
    let parts: Vec<&str> = field.split('.').collect();
    find(&schema.fields, &parts)
        .and_then(|fc| fc.options.similarity.clone())
        .unwrap_or_else(|| "cosine".to_string())
}

/// Look up the quantization scheme declared for a dense_vector `field`.
///
/// Returns `Some("scalar8")` only for fields that opted into SQ8 (via
/// `index_options.type: int8_hnsw` at mapping time); `None` for every default
/// full-precision field. Mirrors [`lookup_vector_similarity`]'s dotted-path
/// descent so nested vector fields resolve correctly.
fn lookup_vector_quantization(schema: &Schema, field: &str) -> Option<String> {
    fn find<'a>(fields: &'a [FieldConfig], path: &[&str]) -> Option<&'a FieldConfig> {
        if path.is_empty() {
            return None;
        }
        let head = path[0];
        let tail = &path[1..];
        for fc in fields {
            if fc.name == head {
                if tail.is_empty() {
                    return Some(fc);
                }
                return find(&fc.fields, tail);
            }
        }
        None
    }
    let parts: Vec<&str> = field.split('.').collect();
    find(&schema.fields, &parts).and_then(|fc| fc.options.quantization.clone())
}

/// Rewrite a query by resolving any field aliases to their canonical field names.
///
/// Traverses the query tree, replacing alias field names with their target paths.
fn rewrite_query_aliases(q: &QueryNode, schema: &Schema) -> QueryNode {
    match q {
        QueryNode::Term {
            field,
            value,
            boost,
        } => {
            let resolved = resolve_field_alias(schema, field);
            QueryNode::Term {
                field: resolved,
                value: value.clone(),
                boost: *boost,
            }
        }
        QueryNode::Terms {
            field,
            values,
            boost,
        } => {
            let resolved = resolve_field_alias(schema, field);
            QueryNode::Terms {
                field: resolved,
                values: values.clone(),
                boost: *boost,
            }
        }
        QueryNode::Range {
            field,
            gte,
            gt,
            lte,
            lt,
            boost,
        } => {
            let resolved = resolve_field_alias(schema, field);
            QueryNode::Range {
                field: resolved,
                gte: gte.clone(),
                gt: gt.clone(),
                lte: lte.clone(),
                lt: lt.clone(),
                boost: *boost,
            }
        }
        QueryNode::Exists { field } => {
            let resolved = resolve_field_alias(schema, field);
            QueryNode::Exists { field: resolved }
        }
        QueryNode::Match {
            field,
            query,
            boost,
            operator,
            analyzer,
            minimum_should_match,
        } => {
            let resolved = resolve_field_alias(schema, field);
            QueryNode::Match {
                field: resolved,
                query: query.clone(),
                boost: *boost,
                operator: *operator,
                analyzer: analyzer.clone(),
                minimum_should_match: minimum_should_match.clone(),
            }
        }
        QueryNode::Bool {
            must,
            should,
            must_not,
            filter,
            minimum_should_match,
        } => QueryNode::Bool {
            must: must
                .iter()
                .map(|c| rewrite_query_aliases(c, schema))
                .collect(),
            should: should
                .iter()
                .map(|c| rewrite_query_aliases(c, schema))
                .collect(),
            must_not: must_not
                .iter()
                .map(|c| rewrite_query_aliases(c, schema))
                .collect(),
            filter: filter
                .iter()
                .map(|c| rewrite_query_aliases(c, schema))
                .collect(),
            minimum_should_match: minimum_should_match.clone(),
        },
        // For all other query types, return as-is.
        other => other.clone(),
    }
}

/// Rewrite an inner-nested query by stripping the `<path>.` prefix from every
/// field reference in the tree.  ES allows field names inside a `nested` query
/// to be written in full (`nested.foo`) even though they resolve against the
/// element itself; XERJ matches via JSON source traversal of the element, so
/// the bare name (`foo`) is what `get_field_value` needs to see.
fn strip_nested_path_in_query(q: &QueryNode, path: &str) -> QueryNode {
    let pfx = format!("{}.", path);
    let strip = |f: &str| -> String {
        f.strip_prefix(&pfx)
            .map(|s| s.to_string())
            .unwrap_or_else(|| f.to_string())
    };
    match q {
        QueryNode::Term {
            field,
            value,
            boost,
        } => QueryNode::Term {
            field: strip(field),
            value: value.clone(),
            boost: *boost,
        },
        QueryNode::Terms {
            field,
            values,
            boost,
        } => QueryNode::Terms {
            field: strip(field),
            values: values.clone(),
            boost: *boost,
        },
        QueryNode::Range {
            field,
            gte,
            gt,
            lte,
            lt,
            boost,
        } => QueryNode::Range {
            field: strip(field),
            gte: gte.clone(),
            gt: gt.clone(),
            lte: lte.clone(),
            lt: lt.clone(),
            boost: *boost,
        },
        QueryNode::Prefix {
            field,
            value,
            boost,
        } => QueryNode::Prefix {
            field: strip(field),
            value: value.clone(),
            boost: *boost,
        },
        QueryNode::Wildcard {
            field,
            value,
            boost,
        } => QueryNode::Wildcard {
            field: strip(field),
            value: value.clone(),
            boost: *boost,
        },
        QueryNode::Exists { field } => QueryNode::Exists {
            field: strip(field),
        },
        QueryNode::Match {
            field,
            query,
            boost,
            operator,
            analyzer,
            minimum_should_match,
        } => QueryNode::Match {
            field: strip(field),
            query: query.clone(),
            boost: *boost,
            operator: *operator,
            analyzer: analyzer.clone(),
            minimum_should_match: minimum_should_match.clone(),
        },
        QueryNode::MatchPhrase {
            field,
            query,
            slop,
            analyzer,
            boost,
        } => QueryNode::MatchPhrase {
            field: strip(field),
            query: query.clone(),
            slop: *slop,
            analyzer: analyzer.clone(),
            boost: *boost,
        },
        QueryNode::MatchPhrasePrefix {
            field,
            query,
            max_expansions,
        } => QueryNode::MatchPhrasePrefix {
            field: strip(field),
            query: query.clone(),
            max_expansions: *max_expansions,
        },
        QueryNode::Fuzzy {
            field,
            value,
            fuzziness,
        } => QueryNode::Fuzzy {
            field: strip(field),
            value: value.clone(),
            fuzziness: *fuzziness,
        },
        QueryNode::Regexp { field, pattern } => QueryNode::Regexp {
            field: strip(field),
            pattern: pattern.clone(),
        },
        QueryNode::MultiMatch {
            fields,
            query,
            match_type,
            operator,
            analyzer,
            boost,
        } => QueryNode::MultiMatch {
            fields: fields.iter().map(|f| strip(f)).collect(),
            query: query.clone(),
            match_type: *match_type,
            operator: *operator,
            analyzer: analyzer.clone(),
            boost: *boost,
        },
        QueryNode::GeoDistance {
            field,
            lat,
            lon,
            distance_km,
        } => QueryNode::GeoDistance {
            field: strip(field),
            lat: *lat,
            lon: *lon,
            distance_km: *distance_km,
        },
        QueryNode::GeoBoundingBox {
            field,
            top_left,
            bottom_right,
        } => QueryNode::GeoBoundingBox {
            field: strip(field),
            top_left: *top_left,
            bottom_right: *bottom_right,
        },
        QueryNode::Bool {
            must,
            should,
            must_not,
            filter,
            minimum_should_match,
        } => QueryNode::Bool {
            must: must
                .iter()
                .map(|c| strip_nested_path_in_query(c, path))
                .collect(),
            should: should
                .iter()
                .map(|c| strip_nested_path_in_query(c, path))
                .collect(),
            must_not: must_not
                .iter()
                .map(|c| strip_nested_path_in_query(c, path))
                .collect(),
            filter: filter
                .iter()
                .map(|c| strip_nested_path_in_query(c, path))
                .collect(),
            minimum_should_match: minimum_should_match.clone(),
        },
        QueryNode::Boosting {
            positive,
            negative,
            negative_boost,
        } => QueryNode::Boosting {
            positive: Box::new(strip_nested_path_in_query(positive, path)),
            negative: Box::new(strip_nested_path_in_query(negative, path)),
            negative_boost: *negative_boost,
        },
        QueryNode::DisMax {
            queries,
            tie_breaker,
        } => QueryNode::DisMax {
            queries: queries
                .iter()
                .map(|c| strip_nested_path_in_query(c, path))
                .collect(),
            tie_breaker: *tie_breaker,
        },
        QueryNode::Named { name, query } => QueryNode::Named {
            name: name.clone(),
            query: Box::new(strip_nested_path_in_query(query, path)),
        },
        QueryNode::FunctionScore {
            query,
            functions,
            score_mode,
            boost_mode,
            max_boost,
        } => QueryNode::FunctionScore {
            query: Box::new(strip_nested_path_in_query(query, path)),
            functions: functions.clone(),
            score_mode: *score_mode,
            boost_mode: *boost_mode,
            max_boost: *max_boost,
        },
        QueryNode::Constant { score, query } => QueryNode::Constant {
            score: *score,
            query: Box::new(strip_nested_path_in_query(query, path)),
        },
        QueryNode::Boosted { boost, query } => QueryNode::Boosted {
            boost: *boost,
            query: Box::new(strip_nested_path_in_query(query, path)),
        },
        // An inner Nested has its own path scope; do not rewrite through it.
        other => other.clone(),
    }
}

// ── copy_to implementation ────────────────────────────────────────────────────

/// True when the schema has at least one `copy_to` mapping.  Lets the
/// ingest hot path skip the (deep-cloning) `apply_copy_to` call entirely
/// on schemas that don't use the feature — common for log workloads.
fn schema_has_copy_to(schema: &Schema) -> bool {
    schema.fields.iter().any(|fc| {
        matches!(&fc.options.null_value, Some(serde_json::Value::String(s)) if s.starts_with("__copy_to__:"))
    })
}

/// Apply copy_to field mappings: for each field with a copy_to option,
/// copy its value into the target field in the document source.
///
/// The copy_to target field name is stored in the FieldConfig as a special
/// null_value prefix `"__copy_to__:<target>"`.
fn apply_copy_to(source: &Value, schema: &Schema) -> Value {
    let obj = match source.as_object() {
        Some(o) => o,
        None => return source.clone(),
    };

    let mut result = obj.clone();
    let mut pristine: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut touched_targets: Vec<String> = Vec::new();

    /// Get a value at a dotted path in an object. Walks through nested
    /// objects but stops early if a segment is missing.
    fn path_get<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
        let mut cur = root;
        for seg in path.split('.') {
            cur = cur.as_object()?.get(seg)?;
        }
        Some(cur)
    }

    /// Set / append at a dotted path inside `result`, auto-creating
    /// intermediate object levels. Appends to arrays, upgrades scalars
    /// to arrays on second write. Mirrors apply_copy_to's single-key
    /// behaviour but walks dotted segments.
    fn path_append(target: &mut serde_json::Map<String, Value>, path: &str, val: Value) {
        let segs: Vec<&str> = path.split('.').collect();
        if segs.is_empty() {
            return;
        }
        let last_idx = segs.len() - 1;
        let mut cur: &mut serde_json::Map<String, Value> = target;
        for (i, seg) in segs.iter().enumerate() {
            if i == last_idx {
                let entry = cur.entry((*seg).to_string()).or_insert(Value::Null);
                match entry {
                    Value::Null => {
                        *entry = val;
                        return;
                    }
                    Value::Array(arr) => {
                        arr.push(val);
                        return;
                    }
                    existing => {
                        let prev = existing.clone();
                        *existing = Value::Array(vec![prev, val]);
                        return;
                    }
                }
            }
            // Ensure intermediate is an object.
            let entry = cur
                .entry((*seg).to_string())
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if !entry.is_object() {
                let old = entry.take();
                let mut m = serde_json::Map::new();
                m.insert("__xy_scalar__".to_string(), old);
                *entry = Value::Object(m);
            }
            cur = entry.as_object_mut().unwrap();
        }
    }

    /// Walk the schema tree and collect (source_path, copy_to_target)
    /// pairs. Each nested FieldConfig carries its leaf-name only, so we
    /// build the full dotted path as we recurse.
    fn collect_copy_to(
        fields: &[xerj_common::FieldConfig],
        prefix: &str,
        out: &mut Vec<(String, String)>,
    ) {
        for fc in fields {
            let path = if prefix.is_empty() {
                fc.name.clone()
            } else {
                format!("{}.{}", prefix, fc.name)
            };
            if let Some(serde_json::Value::String(meta)) = &fc.options.null_value {
                if let Some(target) = meta.strip_prefix("__copy_to__:") {
                    out.push((path.clone(), target.to_string()));
                }
            }
            if !fc.fields.is_empty() {
                collect_copy_to(&fc.fields, &path, out);
            }
        }
    }
    let mut copy_to_pairs: Vec<(String, String)> = Vec::new();
    collect_copy_to(&schema.fields, "", &mut copy_to_pairs);

    // Also look up values at src_path in __xy_ignored_values__ —
    // ignore_malformed may have moved the source value there before
    // apply_copy_to ran. ES's copy_to happens conceptually alongside
    // indexing so the malformed value still copies to the target (the
    // target is typically keyword/text which accepts any string shape).
    let ignored_lookup = source
        .get("__xy_ignored_values__")
        .and_then(Value::as_object)
        .cloned();
    for (src_path, target) in &copy_to_pairs {
        let mut field_val = path_get(source, src_path).cloned();
        if field_val.is_none() {
            if let Some(iv) = ignored_lookup.as_ref() {
                if let Some(v) = iv.get(src_path) {
                    // ignored_values are always arrays — unwrap single-element.
                    field_val = match v.clone() {
                        Value::Array(mut a) if a.len() == 1 => Some(a.remove(0)),
                        other => Some(other),
                    };
                }
            }
        }
        if let Some(field_val) = field_val {
            if !touched_targets.iter().any(|t| t == target) {
                if let Some(orig) = path_get(source, target) {
                    pristine.insert(target.clone(), orig.clone());
                } else {
                    pristine.insert(target.clone(), Value::Null);
                }
                touched_targets.push(target.clone());
            }
            match field_val {
                Value::Array(arr) => {
                    for v in arr {
                        path_append(&mut result, target, v);
                    }
                }
                other => path_append(&mut result, target, other),
            }
        }
    }
    if !touched_targets.is_empty() {
        // Serialize pristine target map — at synthetic-source emit time
        // each target is restored from this snapshot (explicit values
        // kept; copy-only targets dropped entirely).
        result.insert(
            "__xy_copy_to_pristine__".to_string(),
            Value::Object(pristine),
        );
    }

    Value::Object(result)
}

fn infer_field_type(val: &Value) -> FieldType {
    match val {
        Value::String(_) => FieldType::Text,
        Value::Number(n) => {
            if n.is_f64() {
                FieldType::Double
            } else {
                FieldType::Long
            }
        }
        Value::Bool(_) => FieldType::Boolean,
        Value::Array(arr) => {
            // Detect type from first non-null element in the array.
            arr.iter()
                .find(|v| !v.is_null())
                .map(infer_field_type)
                .unwrap_or(FieldType::Text)
        }
        Value::Object(_) => FieldType::Object,
        Value::Null => FieldType::Text,
    }
}

// ── DocValues fast-path helpers ───────────────────────────────────────────────

/// Try to resolve a Term or Range query directly from the memtable's DocValues
/// columnar store, bypassing full document JSON scan.
///
/// Returns `Some(hits)` when the fast path succeeds (the field has a column
/// and the query type is supported), or `None` to signal the caller to fall
/// back to the `DocsForScan` path.
/// Returns `(doc_id, internal_memtable_index)` pairs WITHOUT cloning source.
/// The caller defers source fetching until after scoring and pagination so only
/// the final `from+size` hits pay the clone cost.
/// Lower a `bool { must/filter: [Term|Range…] }` (no should / must_not) —
/// or a bare Term / Range — to fused columnar memtable predicates: the
/// memtable twin of the `try_shortcut_count` Bool segment arm.  `None` for
/// any other shape (caller falls back to the stored-source scan).  Wrapper
/// nodes (constant_score / boost) are peeled like `try_doc_values_query`
/// does.
fn mem_bool_preds(q: &QueryNode) -> Option<Vec<crate::memtable::MemBoolPred>> {
    use crate::memtable::MemBoolPred;
    let q = match q {
        QueryNode::Constant { query, .. } | QueryNode::Boosted { query, .. } => query.as_ref(),
        _ => q,
    };
    // Bare term/range: a single-predicate fused walk — bounds the id
    // materialisation the unbounded `doc_values_term_query` /
    // `doc_values_range_query` paths would pay (~all-matching doc_id
    // String clones per query at a 200 k-doc memtable).
    let children: Vec<&QueryNode> = match q {
        QueryNode::Term { .. } | QueryNode::Range { .. } => vec![q],
        QueryNode::Bool {
            must,
            should,
            must_not,
            filter,
            ..
        } => {
            if !should.is_empty() || !must_not.is_empty() {
                return None;
            }
            must.iter().chain(filter.iter()).collect()
        }
        _ => return None,
    };
    if children.is_empty() {
        return None;
    }
    let mut preds: Vec<MemBoolPred> = Vec::with_capacity(children.len());
    for child in children {
        let child = match child {
            QueryNode::Constant { query, .. } | QueryNode::Boosted { query, .. } => query.as_ref(),
            _ => child,
        };
        match child {
            QueryNode::Term { field, value, .. } => {
                // Same value lowering (and CIDR bailout) as the standalone
                // Term arm of `try_doc_values_query`.
                let val_str = match value {
                    Value::String(s) => {
                        if s.contains('/') {
                            return None;
                        }
                        s.clone()
                    }
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    _ => return None,
                };
                preds.push(MemBoolPred::Term {
                    field: field.clone(),
                    value: val_str,
                });
            }
            QueryNode::Range {
                field,
                gte,
                gt,
                lte,
                lt,
                ..
            } => {
                // Numeric bounds only — mirrors the Range arm of
                // `try_doc_values_query`.
                let to_f64 = |v: &Option<Value>| -> Option<Option<f64>> {
                    match v {
                        None => Some(None),
                        Some(Value::Number(n)) => Some(n.as_f64()),
                        Some(Value::String(s)) => s.parse::<f64>().ok().map(Some),
                        _ => None,
                    }
                };
                let gte_f = to_f64(gte)?;
                let gt_f = to_f64(gt)?;
                let lte_f = to_f64(lte)?;
                let lt_f = to_f64(lt)?;
                if (gte_f.is_none() && gte.is_some())
                    || (gt_f.is_none() && gt.is_some())
                    || (lte_f.is_none() && lte.is_some())
                    || (lt_f.is_none() && lt.is_some())
                {
                    return None;
                }
                preds.push(MemBoolPred::Range {
                    field: field.clone(),
                    gte: gte_f,
                    gt: gt_f,
                    lte: lte_f,
                    lt: lt_f,
                });
            }
            _ => return None,
        }
    }
    Some(preds)
}

fn try_doc_values_query(
    q: &QueryNode,
    mem: &crate::memtable::ShardedFtsMemtable,
    limit: usize,
) -> Option<(Vec<(String, usize)>, u64)> {
    match q {
        QueryNode::Term { field, value, .. } => {
            // Skip if the value is complex or the field uses CIDR notation.
            let val_str = match value {
                Value::String(s) => {
                    if s.contains('/') {
                        return None;
                    } // CIDR — fall back
                    s.clone()
                }
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => return None,
            };
            mem.doc_values_term_query(field, &val_str, limit)
        }

        QueryNode::Terms { field, values, .. } => {
            // Convert JSON values to strings for keyword column lookup.
            let strs: Vec<String> = values
                .iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    Value::Bool(b) => Some(b.to_string()),
                    _ => None,
                })
                .collect();
            if strs.len() != values.len() {
                return None; // complex values — fall back
            }
            mem.doc_values_terms_query(field, &strs, limit)
        }

        QueryNode::Range {
            field,
            gte,
            gt,
            lte,
            lt,
            ..
        } => {
            // Only handle purely numeric ranges (not date strings or IP ranges).
            let to_f64 = |v: &Option<Value>| -> Option<Option<f64>> {
                match v {
                    None => Some(None),
                    Some(Value::Number(n)) => Some(n.as_f64()),
                    Some(Value::String(s)) => {
                        // Could be a date string or IP — fall back for those.
                        s.parse::<f64>().ok().map(Some)
                    }
                    _ => None,
                }
            };
            let gte_f = to_f64(gte)?;
            let gt_f = to_f64(gt)?;
            let lte_f = to_f64(lte)?;
            let lt_f = to_f64(lt)?;

            // All bounds must be either absent or numeric.
            if gte_f.is_none() && gte.is_some() {
                return None;
            }
            if gt_f.is_none() && gt.is_some() {
                return None;
            }
            if lte_f.is_none() && lte.is_some() {
                return None;
            }
            if lt_f.is_none() && lt.is_some() {
                return None;
            }

            mem.doc_values_range_query(field, gte_f, gt_f, lte_f, lt_f, limit)
        }

        // Wrapper queries: delegate to inner query.
        QueryNode::Constant { query, .. } | QueryNode::Boosted { query, .. } => {
            try_doc_values_query(query, mem, limit)
        }

        _ => None,
    }
}

/// Try to run all top-level aggregations in `aggs_def` using the DocValues
/// fast path.  Returns `Some(result)` only when ALL aggregations can be served
/// by the fast path; otherwise returns `None` so the caller falls back to the
/// JSON-scan path for the entire `aggs_def`.
///
/// `doc_indices` — when `Some`, restricts the aggregation to only those memtable
/// document positions (from a DocValues query result).  When `None`, all memtable
/// docs are used (MatchAll case).
///
/// This enables fast filtered aggregation (e.g. stats on a term-filtered result
/// set) without constructing a Vec<Value> of full document sources.
async fn try_aggs_fast(
    aggs_def: &Value,
    doc_indices: Option<&[usize]>,
    memtable: &crate::memtable::ShardedFtsMemtable,
) -> Option<Value> {
    // M5.1 — with the sharded memtable, `run_agg_fast` (which expects
    // a single `&DocValues` borrow) cannot operate directly on the
    // per-shard columns without a bigger refactor of the agg runners.
    // For the sharded-path correctness pass we materialise a
    // temporary per-field aggregation from each shard and fold it
    // into the agg result.  Because memtables are small (≤ few
    // hundred thousand docs before a flush fires), the cost is
    // negligible vs the segment-level work that dominates real
    // queries — and the slow fallback (stored-doc scan in
    // `run_aggs_with_all`) is correct for everything this
    // function used to optimise.
    //
    // For now we return `None` so the caller falls back to the
    // stored-doc scan path.  A follow-up will expose a sharded
    // `DocValuesView` to `aggs::run_agg_fast` so the fast path can
    // resume.
    let _ = (aggs_def, doc_indices, memtable);
    None
}

// ── Segment-aware doc-values agg fast path (M2 G2) ───────────────────────────
//
// Walks each segment's `.dv` side-car file plus the live memtable's
// in-memory `DocValues` columns.  Returns `Some(result)` only when every
// agg in `aggs_def` can be served from a column; otherwise `None` so the
// caller falls back to the full stored-doc scan path.
//
// Currently restricted to MatchAll queries — i.e. global aggs over the
// whole index.  Filtered aggs against on-disk segments need either a
// per-segment match bitmap (G7 postings skip lists) or a doc-id intersection
// pass; both come in M3.
//
// Numeric values stored on disk are f64 bit-patterns (see
// `build_doc_value_columns`), so we recover them via `f64::from_bits`.

#[allow(clippy::needless_range_loop)]
async fn try_aggs_fast_with_segments(
    aggs_def: &Value,
    snap: &xerj_storage::index_store::IndexSnapshot,
    segments_dir: &std::path::Path,
    memtable: &crate::memtable::ShardedFtsMemtable,
) -> Option<Value> {
    // M5.1 — disabled for the sharded memtable pass; queries fall
    // back to the general `run_aggs_with_all` stored-doc path which
    // is correct for every supported agg type.  Re-enabling requires
    // teaching `run_numeric_agg_with_segments` & friends to iterate
    // a `ShardedDocValuesView` instead of borrowing a single
    // `&DocValues`.  See ARCHITECTURE_V5 §2.4 + §6.
    let _ = (aggs_def, snap, segments_dir, memtable);
    None
}

/// Extract a plain-text query string from a QueryNode (for memtable BM25 search).
///
/// Only full-text query types are handled here. Term-level queries (Term, Terms,
/// Range, Exists, Prefix, Wildcard) are handled by doc scanning instead.
fn extract_query_text(q: &QueryNode) -> Option<String> {
    match q {
        // Wildcard field match must use doc scanning, not BM25.
        QueryNode::Match { field, query, .. } if field != "*" && !field.ends_with('*') => {
            Some(query.clone())
        }
        QueryNode::QueryString { query, .. } => Some(query.clone()),
        // MultiMatch and MatchPhrase are handled by doc scanning (is_doc_scan_query).
        _ => None,
    }
}

/// Returns true for query types that should be evaluated by scanning stored documents.
fn is_doc_scan_query(q: &QueryNode) -> bool {
    // Wildcard field match queries must go through doc scanning.
    // Match with `operator: AND` also requires doc scanning because the
    // memtable/segment BM25 path is OR-over-tokens; only the doc-scan
    // substring path honours per-token AND semantics.
    // Match on a date-shaped query string also routes through doc-scan
    // so our date-equality short-circuit fires (FTS token search on
    // `2017/01/01` mis-matches `2017/01/02` because the "2017" and "01"
    // tokens overlap).
    if let QueryNode::Match {
        field,
        operator,
        query,
        ..
    } = q
    {
        if field == "*" || field.ends_with('*') {
            return true;
        }
        if matches!(operator, xerj_query::ast::BoolOperator::And) {
            return true;
        }
        if crate::aggs::parse_date_ms(&Value::String(query.clone())).is_some()
            && query.chars().any(|c| c == '-' || c == '/' || c == 'T')
        {
            return true;
        }
    }
    matches!(
        q,
        QueryNode::Term { .. }
            | QueryNode::Terms { .. }
            | QueryNode::Range { .. }
            | QueryNode::Exists { .. }
            | QueryNode::Prefix { .. }
            | QueryNode::Wildcard { .. }
            | QueryNode::Ids { .. }
            | QueryNode::Bool { .. }
            | QueryNode::Boosting { .. }
            | QueryNode::DisMax { .. }
            | QueryNode::Fuzzy { .. }
            | QueryNode::Regexp { .. }
            | QueryNode::MatchPhrasePrefix { .. }
            | QueryNode::SimpleQueryString { .. }
            | QueryNode::MultiMatch { .. }
            | QueryNode::MatchPhrase { .. }
            | QueryNode::GeoDistance { .. }
            | QueryNode::GeoBoundingBox { .. }
            | QueryNode::GeoPolygon { .. }
            | QueryNode::GeoShape { .. }
            | QueryNode::FunctionScore { .. }
            | QueryNode::Nested { .. }
            | QueryNode::MoreLikeThis { .. }
            | QueryNode::Pinned { .. }
            | QueryNode::Named { .. }
            | QueryNode::SpanTerm { .. }
            | QueryNode::SpanNear { .. }
            | QueryNode::SpanOr { .. }
            | QueryNode::SpanNot { .. }
            | QueryNode::SpanFirst { .. }
            | QueryNode::SpanContaining { .. }
            | QueryNode::SpanWithin { .. }
            | QueryNode::HasChild { .. }
            | QueryNode::HasParent { .. }
            | QueryNode::Constant { .. }
            | QueryNode::Boosted { .. }
            | QueryNode::Intervals { .. }
            | QueryNode::Percolate { .. }
    )
}

/// Compile-once cache for anchored `regexp` patterns.
///
/// `doc_matches_query` is a free function invoked once per candidate doc;
/// pre-fix, its Regexp arm called `Regex::new` for EVERY document
/// (~150 µs per compile), which turned a 1 M-doc regexp stored-scan into
/// minutes of pure recompilation (measured: 79 s at 500 k docs vs 2.6 s
/// for the identical scan with a wildcard matcher).  A thread-local cache
/// keyed by the raw pattern makes the compile a once-per-query cost;
/// `Regex` clones share the compiled program via an internal `Arc`, so
/// cache hits are pointer-cheap.  Invalid patterns cache `None` — the
/// doc-scan arm treats them as match-nothing, same as before.
///
/// Thread-local (not a global lock) because doc-scan runs inside
/// `block_in_place` across many worker threads; a shared map would just
/// re-serialise the hot loop.  Capped and cleared at 64 entries — the
/// pattern set is query-supplied and must not grow unbounded.
fn compiled_anchored_regex(pattern: &str) -> Option<Regex> {
    use std::cell::RefCell;
    thread_local! {
        static REGEX_CACHE: RefCell<std::collections::HashMap<String, Option<Regex>>> =
            RefCell::new(std::collections::HashMap::new());
    }
    REGEX_CACHE.with(|cache| {
        let mut map = cache.borrow_mut();
        if let Some(re) = map.get(pattern) {
            return re.clone();
        }
        // Anchor the pattern to a full-string match like ES does.
        let anchored = format!("^(?:{})$", pattern);
        let re = Regex::new(&anchored).ok();
        if map.len() >= 64 {
            map.clear();
        }
        map.insert(pattern.to_string(), re.clone());
        re
    })
}

/// Result of expanding a regexp pattern against one segment's term
/// dictionary: the exact number of matching docs plus the first
/// `REGEXP_EXPANSION_POS_CAP` matching doc positions (ascending stored-
/// section order).  `complete` marks whether `positions` holds EVERY
/// matching position (true when count ≤ the cap).
struct RegexpExpansion {
    count: u64,
    positions: Vec<u32>,
    complete: bool,
}

/// How many matching doc positions an expansion retains — covers any
/// `from + size` page the F1 bounded scan can ask a pre-filter for
/// (materialisation_limit = (from+size+100).max(256)) up to ~3.9k deep.
const REGEXP_EXPANSION_POS_CAP: usize = 4096;

/// Entry bound for `regexp_expand_cache` — patterns are query-supplied,
/// so the cache is cleared wholesale when it grows past this.
const REGEXP_EXPANSION_CACHE_MAX: usize = 256;

/// Enumerate the FST terms of `field` that match the anchored regex —
/// the ES-style "rewrite the pattern against the term dictionary" step.
/// O(dictionary size) regex probes.  Returns `None` when the expansion
/// is pathological (more matching terms than ES's default rewrite bound
/// of 1024) so callers fall back to the stored-doc scan instead of
/// k-way-merging thousands of postings lists.  `re = None` (invalid
/// pattern) matches nothing, mirroring the doc-scan arm.
fn regexp_matched_fst_terms(
    reader: &FtsIndexReader,
    field: &str,
    re: Option<&Regex>,
) -> Option<Vec<String>> {
    const MAX_TERM_EXPANSIONS: usize = 1024;
    let Some(re) = re else {
        return Some(Vec::new());
    };
    let mut matched: Vec<String> = Vec::new();
    for term in reader.all_terms(field) {
        if re.is_match(&term) {
            if matched.len() >= MAX_TERM_EXPANSIONS {
                return None;
            }
            matched.push(term);
        }
    }
    Some(matched)
}

/// Union the postings of `terms` into (exact distinct doc count, first
/// `pos_cap` ascending internal doc positions).  FTS doc ids are
/// assigned in stored-section order at flush/merge time (the FTS hit
/// path indexes stored docs by `doc_id`), so the positions feed the
/// stored-scan pre-filter directly.  K-way heap merge over the per-term
/// posting lists (each ascending, distinct doc ids within a list);
/// duplicates across lists (multi-valued docs matching several terms)
/// come out adjacent and are counted ONCE — a plain df-sum would
/// double-count them and disagree with the stored-doc scan.  One full
/// O(total postings) pass; callers cache the result per (segment,
/// field, pattern) so this runs once per segment lifetime, not per
/// query.
fn postings_union_expand(
    reader: &FtsIndexReader,
    field: &str,
    terms: &[String],
    pos_cap: usize,
) -> (u64, Vec<u32>) {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;
    use xerj_fts::postings::PostingsReader;

    let has_positions = reader.field_has_positions(field);
    let mut streams: Vec<PostingsReader<'_>> = Vec::with_capacity(terms.len());
    let mut heap: BinaryHeap<Reverse<(u32, usize)>> = BinaryHeap::new();
    for term in terms {
        let Some(tp) = reader.lookup_term(field, term) else {
            continue;
        };
        let Some(data) = reader.postings_data(field, &tp) else {
            continue;
        };
        let mut pr = PostingsReader::new_with_positions(data, tp.doc_frequency, has_positions);
        if let Some(first) = pr.next() {
            let idx = streams.len();
            streams.push(pr);
            heap.push(Reverse((first.doc_id, idx)));
        }
    }

    let mut count: u64 = 0;
    let mut last: Option<u32> = None;
    let mut positions: Vec<u32> = Vec::new();
    while let Some(Reverse((doc_id, idx))) = heap.pop() {
        if last != Some(doc_id) {
            last = Some(doc_id);
            count += 1;
            if positions.len() < pos_cap {
                positions.push(doc_id);
            }
        }
        if let Some(next) = streams[idx].next() {
            heap.push(Reverse((next.doc_id, idx)));
        }
    }
    (count, positions)
}

/// Evaluate a query against a single stored document source value.
///
/// Returns true if the document matches the query.
fn doc_matches_query(q: &QueryNode, source: &Value) -> bool {
    match q {
        QueryNode::MatchAll => true,
        QueryNode::MatchNone => false,

        QueryNode::Term { field, value, .. } => {
            let doc_val = get_field_value(source, field);
            // Support CIDR notation for IP fields: {"term": {"ip": "192.168.1.0/24"}}
            if let Some(Value::String(query_str)) = value.as_str().map(|_| value) {
                if query_str.contains('/') {
                    if let Some(doc_ip_str) = doc_val.as_ref().and_then(|v| v.as_str()) {
                        if let Some(matches) = ip_matches_cidr(doc_ip_str, query_str) {
                            return matches;
                        }
                    }
                }
            }
            json_values_equal(&doc_val, value)
        }

        QueryNode::Terms { field, values, .. } => {
            let doc_val = get_field_value(source, field);
            if values.iter().any(|v| json_values_equal(&doc_val, v)) {
                return true;
            }
            // Text-field fallback: if the doc has a string with whitespace
            // (i.e. looks tokenized), treat each space-separated token as a
            // candidate match. This mirrors ES's behavior of running `terms`
            // against the analyzed-token stream for text fields. Case folded
            // to match ES's standard analyzer.
            fn tokens_contain(doc_val: &Option<Value>, v: &Value) -> bool {
                let target = match v {
                    Value::String(s) => s.to_lowercase(),
                    Value::Number(n) => n.to_string(),
                    _ => return false,
                };
                let dv = match doc_val {
                    Some(Value::String(s)) => s,
                    _ => return false,
                };
                if !dv.contains(char::is_whitespace) {
                    return false;
                }
                dv.split_whitespace().any(|t| {
                    t.trim_matches(|c: char| !c.is_alphanumeric())
                        .eq_ignore_ascii_case(&target)
                })
            }
            values.iter().any(|v| tokens_contain(&doc_val, v))
        }

        QueryNode::Range {
            field,
            gte,
            gt,
            lte,
            lt,
            ..
        } => {
            let doc_val = get_field_value(source, field);
            match doc_val {
                Some(dv) => {
                    // Check if we're comparing IP addresses.
                    let doc_is_ip = dv
                        .as_str()
                        .map(|s| parse_ip_to_u128(s).is_some())
                        .unwrap_or(false);
                    let bound_is_ip = gte
                        .as_ref()
                        .or(gt.as_ref())
                        .or(lte.as_ref())
                        .or(lt.as_ref())
                        .and_then(|b| b.as_str())
                        .map(|s| parse_ip_to_u128(s).is_some())
                        .unwrap_or(false);

                    if doc_is_ip && bound_is_ip {
                        // IP address comparison using numeric ordering.
                        let doc_ip = parse_ip_to_u128(dv.as_str().unwrap_or("")).unwrap_or(0);
                        let passes_lower = match (gte, gt) {
                            (Some(b), _) => {
                                let bound = b.as_str().and_then(parse_ip_to_u128).unwrap_or(0);
                                doc_ip >= bound
                            }
                            (None, Some(b)) => {
                                let bound = b.as_str().and_then(parse_ip_to_u128).unwrap_or(0);
                                doc_ip > bound
                            }
                            (None, None) => true,
                        };
                        let passes_upper = match (lte, lt) {
                            (Some(b), _) => {
                                let bound =
                                    b.as_str().and_then(parse_ip_to_u128).unwrap_or(u128::MAX);
                                doc_ip <= bound
                            }
                            (None, Some(b)) => {
                                let bound =
                                    b.as_str().and_then(parse_ip_to_u128).unwrap_or(u128::MAX);
                                doc_ip < bound
                            }
                            (None, None) => true,
                        };
                        passes_lower && passes_upper
                    } else {
                        let passes_lower = match (gte, gt) {
                            (Some(b), _) => json_compare(&dv, b) >= 0,
                            (None, Some(b)) => json_compare(&dv, b) > 0,
                            (None, None) => true,
                        };
                        let passes_upper = match (lte, lt) {
                            (Some(b), _) => json_compare(&dv, b) <= 0,
                            (None, Some(b)) => json_compare(&dv, b) < 0,
                            (None, None) => true,
                        };
                        passes_lower && passes_upper
                    }
                }
                None => false,
            }
        }

        QueryNode::Exists { field } => match field.as_str() {
            "_id" | "_index" | "_seq_no" | "_version" | "_primary_term" => true,
            "_routing" => source.get("_routing").is_some(),
            _ => get_field_value(source, field).is_some(),
        },

        QueryNode::Prefix { field, value, .. } => {
            // ES Prefix matches against analyzed tokens for text fields and
            // against the raw value for keyword fields. Without schema info
            // at this layer we check both: either the whole value starts
            // with the pattern (keyword semantics) OR any whitespace-
            // separated token does (analyzed-text semantics).
            let value_lc = value.to_lowercase();
            let matches_str = |s: &str| -> bool {
                let lc = s.to_lowercase();
                if lc.starts_with(&value_lc) {
                    return true;
                }
                lc.split(|c: char| !c.is_alphanumeric())
                    .any(|tok| !tok.is_empty() && tok.starts_with(&value_lc))
            };
            get_field_value(source, field)
                .and_then(|v| match v {
                    Value::String(s) => Some(matches_str(&s)),
                    Value::Array(arr) => Some(arr.iter().any(|e| match e {
                        Value::String(s) => matches_str(s),
                        _ => false,
                    })),
                    _ => None,
                })
                .unwrap_or(false)
        }

        QueryNode::Wildcard {
            field,
            value: pattern,
            ..
        } => {
            // Match against the raw value AND against every whitespace/
            // non-alnum token — matches both keyword and text semantics.
            let pat_lc = pattern.to_lowercase();
            let matches_str = |s: &str| -> bool {
                let lc = s.to_lowercase();
                if wildcard_match(&lc, &pat_lc) {
                    return true;
                }
                lc.split(|c: char| !c.is_alphanumeric())
                    .any(|tok| !tok.is_empty() && wildcard_match(tok, &pat_lc))
            };
            get_field_value(source, field)
                .and_then(|v| match v {
                    Value::String(s) => Some(matches_str(&s)),
                    Value::Array(arr) => Some(arr.iter().any(|e| match e {
                        Value::String(s) => matches_str(s),
                        _ => false,
                    })),
                    _ => None,
                })
                .unwrap_or(false)
        }

        QueryNode::Ids { values } => {
            // _id is passed via the source's "_id" metadata field when present,
            // but for memtable doc-scan calls the source may not contain it.
            // We handle this by always returning true here and letting the caller
            // match on the doc_id directly.  For segment scans, the check is
            // performed inline.  For memtable scans, the caller passes the source
            // directly and we can check "_id" if it is embedded.
            if let Some(Value::String(id)) = source.get("_id") {
                values.iter().any(|v| v == id)
            } else {
                // Cannot determine _id from source alone — fall back to false
                // (the search layer handles _id matching separately for memtable).
                false
            }
        }

        QueryNode::Bool {
            must,
            should,
            must_not,
            filter,
            minimum_should_match,
            ..
        } => {
            // All must clauses must match.
            if must.iter().any(|q| !doc_matches_query(q, source)) {
                return false;
            }
            // No must_not clause may match.
            if must_not.iter().any(|q| doc_matches_query(q, source)) {
                return false;
            }
            // All filter clauses must match.
            if filter.iter().any(|q| !doc_matches_query(q, source)) {
                return false;
            }
            // Should clauses: if present, at least minimum_should_match must match.
            if !should.is_empty() {
                // `should.len() + 1` is the "unsatisfiable" sentinel: `matched`
                // can never reach it, so the doc is forced to not match.
                let unsatisfiable = should.len() + 1;
                let min = match minimum_should_match {
                    Some(MinShouldMatch::Fixed(n)) => *n as usize,
                    Some(MinShouldMatch::Percentage(pct)) => {
                        // Round down, minimum 1.
                        ((should.len() as f32 * (*pct as f32 / 100.0)).floor() as usize).max(1)
                    }
                    // terms_set: per-doc required count read from a numeric
                    // field. Missing / non-numeric => the doc cannot match
                    // (ES semantics).
                    Some(MinShouldMatch::Field(name)) => {
                        match get_field_value(source, name).and_then(|v| v.as_f64()) {
                            Some(n) if n >= 0.0 => n as usize,
                            _ => unsatisfiable,
                        }
                    }
                    // terms_set: per-doc required count computed by a Painless
                    // script, with `params.num_terms` injected. A script error
                    // makes the doc unsatisfiable (fail-closed).
                    Some(MinShouldMatch::Script {
                        source: script_src,
                        params,
                    }) => {
                        let mut p = params.clone().unwrap_or_else(|| serde_json::json!({}));
                        if let Value::Object(map) = &mut p {
                            map.insert("num_terms".to_string(), Value::from(should.len() as u64));
                        }
                        let ctx = crate::painless::PainlessCtx::new(source, &p, 0.0);
                        match crate::painless::eval_painless(script_src, &ctx)
                            .ok()
                            .and_then(|v| v.as_f64())
                        {
                            Some(n) if n >= 0.0 => n as usize,
                            _ => unsatisfiable,
                        }
                    }
                    None => {
                        // Default: if must/filter are empty, at least 1 should must match.
                        if must.is_empty() && filter.is_empty() {
                            1
                        } else {
                            0
                        }
                    }
                };
                if min > 0 {
                    let matched = should
                        .iter()
                        .filter(|q| doc_matches_query(q, source))
                        .count();
                    if matched < min {
                        return false;
                    }
                }
            }
            true
        }

        QueryNode::Constant { query, .. } | QueryNode::Boosted { query, .. } => {
            doc_matches_query(query, source)
        }

        QueryNode::Boosting {
            positive,
            negative: _,
            ..
        } => {
            // A doc must match the positive query to be returned.
            doc_matches_query(positive, source)
            // (score penalty for matching negative is handled in scoring, not filtering)
        }

        QueryNode::DisMax { queries, .. } => {
            // Matches if any sub-query matches.
            queries.iter().any(|q| doc_matches_query(q, source))
        }

        // Full-text queries via doc-scan: do simple substring match as fallback.
        QueryNode::Match {
            field,
            query,
            operator,
            analyzer,
            ..
        } => {
            // Support wildcard field names
            if field == "*" || field.ends_with('*') {
                let q_lower = query.to_lowercase();
                return match_any_field_wildcard(source, field, &q_lower);
            }
            // `_index` is a metadata field filtered at coordination time
            // (handler-side); by the time a doc reaches
            // doc_matches_query we're already inside the matching index,
            // so this clause is effectively a no-op pass.
            if field == "_index" {
                return true;
            }
            // Analyzer-aware case handling: `whitespace` / `keyword`
            // analyzers don't lowercase, so a match query for "BROWN"
            // should compare case-sensitively against field tokens. The
            // default (standard/simple/lowercase etc.) lowercases both
            // sides like the existing path.
            let preserve_case = matches!(analyzer.as_deref(), Some("whitespace") | Some("keyword"));
            // When the query string parses as a date AND the field value
            // is a date (or array of dates), switch to date-aware equality:
            // the field value is coerced to epoch ms so
            // `match: {date: "2021-04-01"}` against doc value
            // `"2021-04-01T00:00:00Z"` matches exactly rather than via
            // the generic text tokenizer (which would otherwise also match
            // any other date sharing the "2021" substring).
            let query_date_ms = crate::aggs::parse_date_ms(&Value::String(query.clone()));
            if let Some(qms) = query_date_ms {
                if let Some(v) = get_field_value(source, field) {
                    let check_str = |s: &str| -> Option<bool> {
                        crate::aggs::parse_date_ms(&Value::String(s.to_string()))
                            .map(|ms| ms == qms)
                    };
                    let (field_is_date, matched) = match v {
                        Value::String(s) => {
                            let parsed = check_str(s.as_str());
                            (parsed.is_some(), parsed.unwrap_or(false))
                        }
                        Value::Array(arr) => {
                            let parsed: Vec<Option<bool>> = arr
                                .iter()
                                .filter_map(|e| match e {
                                    Value::String(s) => Some(check_str(s.as_str())),
                                    _ => None,
                                })
                                .collect();
                            let any_date = parsed.iter().any(|o| o.is_some());
                            let any_match = parsed.iter().any(|o| o.unwrap_or(false));
                            (any_date, any_match)
                        }
                        _ => (false, false),
                    };
                    if field_is_date {
                        return matched;
                    }
                }
            }
            // Tokenize both query and field — honor the operator: AND requires
            // every query token to be present in the field; OR requires any.
            // Short single-character tokens are kept when numeric (to preserve
            // `match: {field: 1}` against a numeric field doc value 1); only
            // single-character alphabetic tokens (historic stopword noise) are
            // dropped.
            let fold = |s: &str| -> String {
                if preserve_case {
                    s.to_string()
                } else {
                    s.to_lowercase()
                }
            };
            let q_tokens: Vec<String> = if preserve_case {
                // `whitespace` analyzer splits only on ASCII whitespace.
                query.split_whitespace().map(str::to_string).collect()
            } else {
                query
                    .to_lowercase()
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| {
                        w.len() >= 2
                            || w.chars()
                                .next()
                                .map(|c| c.is_ascii_digit())
                                .unwrap_or(false)
                    })
                    .map(|w| w.to_string())
                    .collect()
            };
            if q_tokens.is_empty() {
                return false;
            }

            // Token-level match (ES standard-analyzer semantics): tokenize
            // the field value on non-alphanumeric boundaries, then check
            // token equality — NOT substring. Using substring would make
            // `match(jump)` match "jumparound" which ES does not.
            let tokenize = |s: &str| -> Vec<String> {
                fold(s)
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .collect()
            };
            get_field_value(source, field)
                .and_then(|v| match v {
                    Value::String(s) => {
                        let field_tokens = tokenize(&s);
                        let hit = match operator {
                            xerj_query::ast::BoolOperator::And => q_tokens
                                .iter()
                                .all(|qt| field_tokens.iter().any(|ft| ft == qt)),
                            xerj_query::ast::BoolOperator::Or => q_tokens
                                .iter()
                                .any(|qt| field_tokens.iter().any(|ft| ft == qt)),
                        };
                        Some(hit)
                    }
                    Value::Number(n) => {
                        let s = n.to_string();
                        let hit = match operator {
                            xerj_query::ast::BoolOperator::And => {
                                q_tokens.iter().all(|qt| qt == &s)
                            }
                            xerj_query::ast::BoolOperator::Or => q_tokens.iter().any(|qt| qt == &s),
                        };
                        Some(hit)
                    }
                    Value::Array(arr) => {
                        // Multi-valued field: a match on any element is a hit.
                        let hit = arr.iter().any(|elem| match elem {
                            Value::String(s) => {
                                let ft = tokenize(s);
                                q_tokens.iter().any(|qt| ft.iter().any(|t| t == qt))
                            }
                            Value::Number(n) => {
                                let s = n.to_string();
                                q_tokens.iter().any(|qt| qt == &s)
                            }
                            _ => false,
                        });
                        Some(hit)
                    }
                    Value::Object(_) => {
                        // Flattened-field match: the field maps to an
                        // object whose values are indexed as a single
                        // searchable text. Tokenize every leaf string +
                        // number and test against the query tokens,
                        // honoring the operator.
                        fn collect_leaf_tokens(
                            v: &Value,
                            fold: &impl Fn(&str) -> String,
                            out: &mut Vec<String>,
                        ) {
                            match v {
                                Value::String(s) => {
                                    let tokenized: Vec<String> = fold(s)
                                        .split(|c: char| !c.is_alphanumeric())
                                        .filter(|t| !t.is_empty())
                                        .map(String::from)
                                        .collect();
                                    out.extend(tokenized);
                                }
                                Value::Number(n) => {
                                    out.push(n.to_string());
                                }
                                Value::Array(arr) => {
                                    for e in arr {
                                        collect_leaf_tokens(e, fold, out);
                                    }
                                }
                                Value::Object(obj) => {
                                    for (_, val) in obj {
                                        collect_leaf_tokens(val, fold, out);
                                    }
                                }
                                _ => {}
                            }
                        }
                        let mut flat_tokens: Vec<String> = Vec::new();
                        collect_leaf_tokens(&v, &fold, &mut flat_tokens);
                        let hit = match operator {
                            xerj_query::ast::BoolOperator::And => q_tokens
                                .iter()
                                .all(|qt| flat_tokens.iter().any(|ft| ft == qt)),
                            xerj_query::ast::BoolOperator::Or => q_tokens
                                .iter()
                                .any(|qt| flat_tokens.iter().any(|ft| ft == qt)),
                        };
                        Some(hit)
                    }
                    _ => None,
                })
                .unwrap_or(false)
        }

        // MultiMatch: check all specified fields; semantics depend on
        // operator and match_type. `operator: and` + `cross_fields` (used
        // by `combined_fields`) requires every query token to appear in
        // at least one of the listed fields. The default (operator: or)
        // admits the hit when any query substring hits any field.
        //
        // Field specs may carry a boost factor (`"title^3"`); strip it for matching.
        QueryNode::MultiMatch {
            fields,
            query,
            match_type,
            operator,
            ..
        } => {
            let q_lower = query.to_lowercase();
            let tokens: Vec<String> = q_lower
                .split(|c: char| !c.is_alphanumeric())
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect();
            let is_cross = matches!(match_type, xerj_query::ast::MultiMatchType::CrossFields);
            let is_and = matches!(operator, Some(xerj_query::ast::BoolOperator::And));
            let field_texts: Vec<String> = fields
                .iter()
                .filter_map(|field_spec| {
                    let (field, _) = parse_field_boost(field_spec);
                    match get_field_value(source, field) {
                        Some(Value::String(s)) => Some(s.to_lowercase()),
                        Some(Value::Array(arr)) => {
                            let joined: Vec<String> = arr
                                .iter()
                                .map(|v| match v {
                                    Value::String(s) => s.to_lowercase(),
                                    other => other.to_string(),
                                })
                                .collect();
                            if joined.is_empty() {
                                None
                            } else {
                                Some(joined.join(" "))
                            }
                        }
                        Some(other) => Some(other.to_string().to_lowercase()),
                        None => None,
                    }
                })
                .collect();
            if is_cross && is_and && !tokens.is_empty() {
                // cross_fields + operator AND: every token must appear in
                // at least one listed field (combined perspective).
                let combined = field_texts.join(" ");
                let combined_tokens: std::collections::HashSet<&str> = combined
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|t| !t.is_empty())
                    .collect();
                tokens.iter().all(|t| combined_tokens.contains(t.as_str()))
            } else if is_cross && !tokens.is_empty() {
                // cross_fields + operator OR (the default, used by
                // `combined_fields`): admit the hit if ANY query token
                // appears in the pooled (combined) text of the listed
                // fields. Without this, a multi-token query whose tokens
                // are scattered across fields fell through to the
                // substring-of-whole-query branch below and returned 0
                // hits even though every token was present somewhere.
                let combined = field_texts.join(" ");
                let combined_tokens: std::collections::HashSet<&str> = combined
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|t| !t.is_empty())
                    .collect();
                tokens.iter().any(|t| combined_tokens.contains(t.as_str()))
            } else if is_and && !tokens.is_empty() {
                // best_fields + operator AND: a single field must hold
                // every token.
                field_texts.iter().any(|ft| {
                    let ft_tokens: std::collections::HashSet<&str> = ft
                        .split(|c: char| !c.is_alphanumeric())
                        .filter(|t| !t.is_empty())
                        .collect();
                    tokens.iter().all(|t| ft_tokens.contains(t.as_str()))
                })
            } else {
                field_texts.iter().any(|ft| ft.contains(&q_lower))
            }
        }

        // MatchPhrase: tokenize both query and field value, check tokens appear
        // in order (contiguous) within the field's token sequence.
        QueryNode::MatchPhrase {
            field, query, slop, ..
        } => {
            get_field_value(source, field)
                .and_then(|v| match v {
                    Value::String(s) => {
                        let field_tokens: Vec<String> = s
                            .to_lowercase()
                            .split(|c: char| !c.is_alphanumeric())
                            .filter(|t| !t.is_empty())
                            .map(str::to_string)
                            .collect();
                        let query_tokens: Vec<String> = query
                            .to_lowercase()
                            .split(|c: char| !c.is_alphanumeric())
                            .filter(|t| !t.is_empty())
                            .map(str::to_string)
                            .collect();
                        if query_tokens.is_empty() {
                            return Some(true);
                        }
                        if query_tokens.len() > field_tokens.len() {
                            return Some(false);
                        }
                        // slop=0: exact contiguous phrase match in order.
                        if *slop == 0 {
                            let found = field_tokens
                                .windows(query_tokens.len())
                                .any(|w| w == query_tokens.as_slice());
                            return Some(found);
                        }
                        // slop>0: ES enforces in-order positions with at
                        // most `slop` intervening tokens between adjacent
                        // query tokens. Find each query token AFTER the
                        // previous one and sum the gaps.
                        let mut last_pos: Option<usize> = None;
                        let mut total_gaps: i64 = 0;
                        let mut ordered_ok = true;
                        for qt in &query_tokens {
                            let search_start = last_pos.map(|p| p + 1).unwrap_or(0);
                            match field_tokens[search_start..]
                                .iter()
                                .position(|ft| ft == qt)
                                .map(|off| search_start + off)
                            {
                                Some(pos) => {
                                    if let Some(prev) = last_pos {
                                        total_gaps += (pos as i64 - prev as i64 - 1).max(0);
                                    }
                                    last_pos = Some(pos);
                                }
                                None => {
                                    ordered_ok = false;
                                    break;
                                }
                            }
                        }
                        Some(ordered_ok && total_gaps <= *slop as i64)
                    }
                    _ => None,
                })
                .unwrap_or(false)
        }

        // GeoDistance: compute haversine distance and compare against threshold.
        QueryNode::GeoDistance {
            field,
            lat,
            lon,
            distance_km,
        } => {
            get_field_value(source, field)
                .and_then(|v| {
                    // Accept {"lat": f64, "lon": f64} or [lon, lat] or "lat,lon".
                    let (doc_lat, doc_lon) = match &v {
                        Value::Object(obj) => {
                            let dlat = obj.get("lat").and_then(|x| x.as_f64())?;
                            let dlon = obj.get("lon").and_then(|x| x.as_f64())?;
                            (dlat, dlon)
                        }
                        Value::Array(arr) if arr.len() == 2 => {
                            let dlon = arr[0].as_f64()?;
                            let dlat = arr[1].as_f64()?;
                            (dlat, dlon)
                        }
                        Value::String(s) => {
                            let parts: Vec<&str> = s.splitn(2, ',').collect();
                            if parts.len() != 2 {
                                return None;
                            }
                            let dlat = parts[0].trim().parse::<f64>().ok()?;
                            let dlon = parts[1].trim().parse::<f64>().ok()?;
                            (dlat, dlon)
                        }
                        _ => return None,
                    };
                    let dist = haversine_distance(*lat, *lon, doc_lat, doc_lon);
                    Some(dist <= *distance_km)
                })
                .unwrap_or(false)
        }

        QueryNode::Fuzzy {
            field,
            value: query_value,
            fuzziness,
        } => {
            let max_edits = match fuzziness {
                Fuzziness::Auto => auto_fuzziness(query_value),
                Fuzziness::Fixed(n) => *n as usize,
            };
            let q_lower = query_value.to_lowercase();
            // ES's Fuzzy query matches at the TERM level with an UNANALYZED
            // query term. For keyword fields the indexed term is the whole
            // field value, so first compare the full (case-folded) string
            // against the query — e.g. doc "claude-haiku-4-5" vs query
            // "claude-haiku-4-6" is 1 edit and must match. Then fall back to
            // per-token comparison for text-field semantics, where each
            // analyzed token is an indexed term.
            let token_match = |s: &str| -> bool {
                let s_lower = s.to_lowercase();
                if levenshtein_distance(&s_lower, &q_lower) <= max_edits {
                    return true;
                }
                s_lower
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|t| !t.is_empty())
                    .any(|tok| levenshtein_distance(tok, &q_lower) <= max_edits)
            };
            get_field_value(source, field)
                .and_then(|v| match v {
                    Value::String(s) => Some(token_match(&s)),
                    Value::Array(arr) => Some(arr.iter().any(|e| match e {
                        Value::String(s) => token_match(s),
                        _ => false,
                    })),
                    _ => None,
                })
                .unwrap_or(false)
        }

        QueryNode::Regexp { field, pattern } => {
            get_field_value(source, field)
                .and_then(|v| match v {
                    Value::String(s) => {
                        // Compile-once (thread-local cache) — this arm runs
                        // per candidate doc and used to recompile every time.
                        compiled_anchored_regex(pattern).map(|re| re.is_match(&s))
                    }
                    _ => None,
                })
                .unwrap_or(false)
        }

        QueryNode::MatchPhrasePrefix { field, query, .. } => {
            get_field_value(source, field)
                .and_then(|v| match v {
                    Value::String(s) => {
                        let tokens: Vec<&str> = query.split_whitespace().collect();
                        if tokens.is_empty() {
                            return Some(true);
                        }
                        let s_lower = s.to_lowercase();
                        let (prefix, exact_tokens) = match tokens.split_last() {
                            Some(pair) => pair,
                            None => return Some(true),
                        };
                        // All tokens except the last must appear as an ordered substring.
                        // Last token is a prefix match.
                        let phrase_without_last = exact_tokens.join(" ").to_lowercase();
                        let last_lower = prefix.to_lowercase();
                        if exact_tokens.is_empty() {
                            // Only one token — prefix match on the whole query.
                            Some(
                                s_lower
                                    .split_whitespace()
                                    .any(|w| w.starts_with(last_lower.as_str())),
                            )
                        } else {
                            // Multi-token: check phrase prefix.
                            if let Some(pos) = s_lower.find(&phrase_without_last) {
                                let after =
                                    &s_lower[pos + phrase_without_last.len()..].trim_start();
                                Some(after.starts_with(last_lower.as_str()))
                            } else {
                                Some(false)
                            }
                        }
                    }
                    _ => None,
                })
                .unwrap_or(false)
        }

        QueryNode::SimpleQueryString { .. } => false, // Converted to Bool at parse time

        QueryNode::FunctionScore { query, .. } => doc_matches_query(query, source),

        // Nested: extract the array at `path`, run the inner query against each element.
        // The document matches if any element of the nested array matches.
        // ES lets the inner query reference fields in full (`path.field`) even
        // though they resolve against the element — strip the prefix first.
        QueryNode::Nested { path, query, .. } => {
            let inner = strip_nested_path_in_query(query, path);
            match get_field_value(source, path) {
                Some(Value::Array(arr)) => arr.iter().any(|elem| doc_matches_query(&inner, elem)),
                Some(elem @ Value::Object(_)) => doc_matches_query(&inner, &elem),
                _ => false,
            }
        }

        // MoreLikeThis: tokenize the `like` text, check if the document contains
        // any of the terms (simple bag-of-words similarity).
        QueryNode::MoreLikeThis { fields, like, .. } => {
            // Build a set of query terms from all `like` strings.
            let terms: Vec<String> = like
                .iter()
                .flat_map(|s| s.split_whitespace().map(|t| t.to_lowercase()))
                .filter(|t| t.len() >= 3) // skip very short terms
                .collect();

            if terms.is_empty() {
                return false;
            }

            // Determine which fields to check.
            let check_fields: Vec<String> = if fields.is_empty() {
                // No fields specified — scan all string fields in the document.
                source
                    .as_object()
                    .map(|obj| {
                        obj.iter()
                            .filter_map(|(k, v)| if v.is_string() { Some(k.clone()) } else { None })
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                fields.clone()
            };

            // A document matches if any term appears in any of the check fields.
            check_fields.iter().any(|field| {
                get_field_value(source, field)
                    .and_then(|v| match v {
                        Value::String(s) => {
                            let s_lower = s.to_lowercase();
                            Some(terms.iter().any(|t| s_lower.contains(t.as_str())))
                        }
                        _ => None,
                    })
                    .unwrap_or(false)
            })
        }

        // Pinned: a pinned query matches via the organic sub-query OR if the doc id
        // is in the pinned list.  Score boosting / ordering is handled in search_inner.
        QueryNode::Pinned { ids, organic } => {
            if let Some(Value::String(doc_id)) = source.get("_id") {
                if ids.iter().any(|pid| pid == doc_id) {
                    return true;
                }
            }
            doc_matches_query(organic, source)
        }

        // GeoBoundingBox: check if doc's lat/lon is within the bounding box.
        QueryNode::GeoBoundingBox {
            field,
            top_left,
            bottom_right,
        } => {
            get_field_value(source, field)
                .and_then(|v| {
                    let (doc_lat, doc_lon) = match &v {
                        Value::Object(obj) => {
                            let dlat = obj.get("lat").and_then(|x| x.as_f64())?;
                            let dlon = obj.get("lon").and_then(|x| x.as_f64())?;
                            (dlat, dlon)
                        }
                        Value::Array(arr) if arr.len() == 2 => {
                            let dlon = arr[0].as_f64()?;
                            let dlat = arr[1].as_f64()?;
                            (dlat, dlon)
                        }
                        Value::String(s) => {
                            let parts: Vec<&str> = s.splitn(2, ',').collect();
                            if parts.len() != 2 {
                                return None;
                            }
                            let dlat = parts[0].trim().parse::<f64>().ok()?;
                            let dlon = parts[1].trim().parse::<f64>().ok()?;
                            (dlat, dlon)
                        }
                        _ => return None,
                    };
                    // ES geo_bounding_box semantics:
                    //   - lat range is always [min(br.lat, tl.lat), max(..)]
                    //     so top/bottom ordering is lenient.
                    //   - lon range: if top_left.lon <= bottom_right.lon,
                    //     the doc lon must fall in that range. If
                    //     top_left.lon > bottom_right.lon, the box
                    //     crosses the antimeridian, so the doc matches
                    //     when its lon is `>= top_left.lon` OR
                    //     `<= bottom_right.lon` (the wraparound
                    //     complement).
                    let (tl_lat, tl_lon) = *top_left;
                    let (br_lat, br_lon) = *bottom_right;
                    let min_lat = tl_lat.min(br_lat);
                    let max_lat = tl_lat.max(br_lat);
                    let lat_ok = doc_lat >= min_lat && doc_lat <= max_lat;
                    let lon_ok = if tl_lon <= br_lon {
                        doc_lon >= tl_lon && doc_lon <= br_lon
                    } else {
                        doc_lon >= tl_lon || doc_lon <= br_lon
                    };
                    Some(lat_ok && lon_ok)
                })
                .unwrap_or(false)
        }

        QueryNode::Named { query, .. } => doc_matches_query(query, source),

        // ── Span queries ──────────────────────────────────────────────────────

        // SpanTerm: same as exact field match (Term query semantics).
        QueryNode::SpanTerm { field, value } => get_field_value(source, field)
            .map(|v| match v {
                Value::String(s) => s.to_lowercase() == value.to_lowercase(),
                _ => false,
            })
            .unwrap_or(false),

        // SpanNear: tokenize the field, check that each span_term clause appears
        // within `slop` positions.  Optionally enforces `in_order`.
        QueryNode::SpanNear {
            clauses,
            slop,
            in_order,
        } => {
            // Collect (field, value) pairs from SpanTerm / Term sub-clauses.
            let targets: Vec<(String, String)> = clauses
                .iter()
                .filter_map(|c| match c {
                    QueryNode::SpanTerm { field, value } => Some((field.clone(), value.clone())),
                    QueryNode::Term { field, value, .. } => {
                        value.as_str().map(|v| (field.clone(), v.to_string()))
                    }
                    _ => None,
                })
                .collect();

            if targets.is_empty() {
                return clauses.iter().all(|c| doc_matches_query(c, source));
            }

            // All targets must be on the same field for proximity logic.
            let first_field = &targets[0].0;
            if !targets.iter().all(|(f, _)| f == first_field) {
                // Cross-field span: fall back to all-must match.
                return clauses.iter().all(|c| doc_matches_query(c, source));
            }

            let field_val = match get_field_value(source, first_field) {
                Some(Value::String(s)) => s,
                _ => return false,
            };

            // Tokenize by splitting on non-alphanumeric characters.
            let tokens: Vec<String> = field_val
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect();

            // Find positions for each target term.
            let positions: Vec<Vec<usize>> = targets
                .iter()
                .map(|(_, val)| {
                    let v_lower = val.to_lowercase();
                    tokens
                        .iter()
                        .enumerate()
                        .filter_map(|(i, t)| if t == &v_lower { Some(i) } else { None })
                        .collect()
                })
                .collect();

            // Check if any combination of positions satisfies slop + in_order.
            fn check_positions(
                positions: &[Vec<usize>],
                idx: usize,
                last_pos: Option<usize>,
                last_idx: Option<usize>,
                slop: u32,
                in_order: bool,
            ) -> bool {
                if idx == positions.len() {
                    return true;
                }
                for &pos in &positions[idx] {
                    // in_order: each term must appear after the previous.
                    if in_order {
                        if let Some(lp) = last_pos {
                            if pos <= lp {
                                continue;
                            }
                        }
                    }
                    // slop check: distance from the first positioned term must be ≤ slop.
                    let ok = match last_idx {
                        None => true,
                        Some(lp) => {
                            let dist = pos.abs_diff(lp);
                            dist as u32 <= slop + 1
                        }
                    };
                    if ok
                        && check_positions(positions, idx + 1, Some(pos), Some(pos), slop, in_order)
                    {
                        return true;
                    }
                }
                false
            }

            check_positions(&positions, 0, None, None, *slop, *in_order)
        }

        // SpanOr: matches if any clause matches.
        QueryNode::SpanOr { clauses } => clauses.iter().any(|c| doc_matches_query(c, source)),

        // SpanNot: include matches AND exclude does not match.
        QueryNode::SpanNot { include, exclude } => {
            doc_matches_query(include, source) && !doc_matches_query(exclude, source)
        }

        // SpanFirst: match if the span_term appears in the first `end` tokens of the field.
        QueryNode::SpanFirst { match_query, end } => {
            // Determine field and value from the inner query.
            let (field, value) = match match_query.as_ref() {
                QueryNode::SpanTerm { field, value } => (field.as_str(), value.as_str()),
                QueryNode::Term { field, value, .. } => match value.as_str() {
                    Some(v) => (field.as_str(), v),
                    None => return doc_matches_query(match_query, source),
                },
                other => return doc_matches_query(other, source),
            };

            let field_val = match get_field_value(source, field) {
                Some(Value::String(s)) => s,
                _ => return false,
            };

            let tokens: Vec<String> = field_val
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect();

            let limit = (*end as usize).min(tokens.len());
            let v_lower = value.to_lowercase();
            tokens[..limit].iter().any(|t| t == &v_lower)
        }

        // SpanContaining / SpanWithin: both reduce to the same doc-level
        // predicate — there exists a `big` span that encloses a `little`
        // span. (span_containing returns the enclosing spans, span_within the
        // enclosed ones, but a document matches in either case iff such a pair
        // exists.) Tokenise the shared field, evaluate both spans into
        // position intervals, and test enclosure.
        QueryNode::SpanContaining { little, big } | QueryNode::SpanWithin { little, big } => {
            let field = match span_field(big).or_else(|| span_field(little)) {
                Some(f) => f,
                None => return false,
            };
            let text = match get_field_value(source, &field) {
                Some(Value::String(s)) => s,
                _ => return false,
            };
            let tokens = intervals_tokenise(&text);
            let bigs = span_intervals(big, &tokens);
            if bigs.is_empty() {
                return false;
            }
            let littles = span_intervals(little, &tokens);
            bigs.iter()
                .any(|b| littles.iter().any(|l| b.start <= l.start && l.end <= b.end))
        }

        // ── Join queries — UNREACHABLE ─────────────────────────────────────────
        // has_child/has_parent are rejected with a 400 at parse time
        // (see xerj-query parser.rs::parse_has_child), so these AST variants
        // are never built and these branches never execute. Previously they
        // silently ran the inner query on the flat doc set, which returned
        // wrong results (no parent/child join is materialized). Kept for
        // exhaustiveness / a future real join executor.
        QueryNode::HasChild { query, .. } => doc_matches_query(query, source),
        QueryNode::HasParent { query, .. } => doc_matches_query(query, source),

        // ── Geo shape queries ─────────────────────────────────────────────────

        // GeoPolygon: point-in-polygon using ray casting.
        QueryNode::GeoPolygon { field, points } => get_field_value(source, field)
            .and_then(|v| {
                let (doc_lat, doc_lon) = extract_lat_lon(&v)?;
                Some(point_in_polygon(doc_lat, doc_lon, points))
            })
            .unwrap_or(false),

        // GeoShape: dispatch by shape type.
        QueryNode::GeoShape { field, shape } => get_field_value(source, field)
            .and_then(|v| {
                let (doc_lat, doc_lon) = extract_lat_lon(&v)?;
                Some(match shape {
                    xerj_query::ast::GeoShapeType::Point { lat, lon } => {
                        (doc_lat - lat).abs() < 1e-9 && (doc_lon - lon).abs() < 1e-9
                    }
                    xerj_query::ast::GeoShapeType::Envelope {
                        top_left,
                        bottom_right,
                    } => {
                        let (tl_lat, tl_lon) = top_left;
                        let (br_lat, br_lon) = bottom_right;
                        doc_lat <= *tl_lat
                            && doc_lat >= *br_lat
                            && doc_lon >= *tl_lon
                            && doc_lon <= *br_lon
                    }
                    xerj_query::ast::GeoShapeType::Polygon { points } => {
                        point_in_polygon(doc_lat, doc_lon, points)
                    }
                    xerj_query::ast::GeoShapeType::Circle { center, radius_km } => {
                        let dist = haversine_distance(center.0, center.1, doc_lat, doc_lon);
                        dist <= *radius_km
                    }
                })
            })
            .unwrap_or(false),

        // Position-aware intervals query. Tokenise the field, evaluate
        // the rule tree into a set of matching intervals, and report
        // `true` if at least one interval survives after filters.
        QueryNode::Intervals { field, rule } => {
            let text = match get_field_value(source, field) {
                Some(Value::String(s)) => s,
                _ => return false,
            };
            let tokens: Vec<String> = intervals_tokenise(&text);
            let intervals = intervals_eval(rule, &tokens);
            !intervals.is_empty()
        }

        // Percolate (reverse search): this stored doc's `field` holds a
        // serialized ES query. Parse it and report a match iff that stored
        // query matches at least one of the supplied inline documents.
        // A missing field or an unparseable stored query => no match.
        QueryNode::Percolate { field, documents } => {
            let stored = match get_field_value(source, field) {
                Some(v) if v.is_object() => v,
                _ => return false,
            };
            let stored_q = match xerj_query::parse_query(&stored) {
                Ok(q) => q,
                Err(_) => return false,
            };
            documents.iter().any(|d| doc_matches_query(&stored_q, d))
        }

        _ => false,
    }
}

/// Standard-analyzer tokenisation for intervals: split on non-alphanum,
/// lowercase each token, preserve position indices so interval ops can
/// measure gaps.
fn intervals_tokenise(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

/// Closed-open token-position interval `[start, end]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TokenInterval {
    start: usize,
    end: usize,
}

impl TokenInterval {
    fn width(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
}

/// Find the target field of a span query subtree (the field all span clauses
/// operate on). ES span queries are single-field; we return the first
/// `span_term` / `term` field encountered.
fn span_field(node: &QueryNode) -> Option<String> {
    match node {
        QueryNode::SpanTerm { field, .. } => Some(field.clone()),
        QueryNode::Term { field, .. } => Some(field.clone()),
        QueryNode::SpanNear { clauses, .. } | QueryNode::SpanOr { clauses } => {
            clauses.iter().find_map(span_field)
        }
        QueryNode::SpanNot { include, .. } => span_field(include),
        QueryNode::SpanFirst { match_query, .. } => span_field(match_query),
        QueryNode::SpanContaining { big, little } | QueryNode::SpanWithin { big, little } => {
            span_field(big).or_else(|| span_field(little))
        }
        _ => None,
    }
}

/// Evaluate a span query subtree over an already-tokenised field, returning
/// the set of matching token-position intervals. Used by span_containing /
/// span_within to test enclosure between a `little` and a `big` span.
///
/// Supports SpanTerm/Term, SpanNear, SpanOr, SpanFirst and SpanNot — the same
/// family the doc-scan span matcher already understands.
fn span_intervals(node: &QueryNode, tokens: &[String]) -> Vec<TokenInterval> {
    match node {
        QueryNode::SpanTerm { value, .. } => {
            let target = value.to_lowercase();
            tokens
                .iter()
                .enumerate()
                .filter(|(_, t)| **t == target)
                .map(|(i, _)| TokenInterval { start: i, end: i })
                .collect()
        }
        QueryNode::Term { value, .. } => match value.as_str() {
            Some(v) => {
                let target = v.to_lowercase();
                tokens
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| **t == target)
                    .map(|(i, _)| TokenInterval { start: i, end: i })
                    .collect()
            }
            None => Vec::new(),
        },
        QueryNode::SpanOr { clauses } => clauses
            .iter()
            .flat_map(|c| span_intervals(c, tokens))
            .collect(),
        QueryNode::SpanNear {
            clauses,
            slop,
            in_order,
        } => {
            let per: Vec<Vec<TokenInterval>> =
                clauses.iter().map(|c| span_intervals(c, tokens)).collect();
            if per.iter().any(|p| p.is_empty()) {
                return Vec::new();
            }
            // Choose one interval per clause such that consecutive picks are
            // within `slop` (adjacency gap) and — when in_order — appear in
            // clause order. The produced interval encloses all picks.
            #[allow(clippy::too_many_arguments)]
            fn rec(
                per: &[Vec<TokenInterval>],
                idx: usize,
                prev: Option<TokenInterval>,
                min: usize,
                max: usize,
                slop: u32,
                in_order: bool,
                out: &mut Vec<TokenInterval>,
            ) {
                if idx == per.len() {
                    if prev.is_some() {
                        out.push(TokenInterval {
                            start: min,
                            end: max,
                        });
                    }
                    return;
                }
                for &iv in &per[idx] {
                    let (nmin, nmax, ok) = match prev {
                        None => (iv.start, iv.end, true),
                        Some(p) => {
                            let order_ok = !in_order || iv.start > p.end;
                            // Gap between the two intervals (0 when they
                            // overlap); one of the two subtractions is 0.
                            let dist = iv
                                .start
                                .saturating_sub(p.end)
                                .max(p.start.saturating_sub(iv.end));
                            let slop_ok = dist <= slop as usize + 1;
                            (min.min(iv.start), max.max(iv.end), order_ok && slop_ok)
                        }
                    };
                    if ok {
                        rec(per, idx + 1, Some(iv), nmin, nmax, slop, in_order, out);
                    }
                }
            }
            let mut out = Vec::new();
            rec(&per, 0, None, 0, 0, *slop, *in_order, &mut out);
            out
        }
        QueryNode::SpanFirst { match_query, end } => span_intervals(match_query, tokens)
            .into_iter()
            .filter(|iv| iv.end < *end as usize)
            .collect(),
        QueryNode::SpanNot { include, exclude } => {
            let exc = span_intervals(exclude, tokens);
            span_intervals(include, tokens)
                .into_iter()
                .filter(|iv| {
                    // Keep include spans that do not overlap any exclude span.
                    !exc.iter().any(|e| iv.start <= e.end && e.start <= iv.end)
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Evaluate an intervals rule against a tokenised doc. Returns the set of
/// surviving intervals (can be empty).
fn intervals_eval(rule: &Value, tokens: &[String]) -> Vec<TokenInterval> {
    let raw = match intervals_eval_inner(rule, tokens) {
        Some(v) => v,
        None => return Vec::new(),
    };
    // Apply top-level filter when present.
    if let Some(filter) = rule
        .get("match")
        .and_then(|m| m.get("filter"))
        .or_else(|| rule.get("all_of").and_then(|a| a.get("filter")))
        .or_else(|| rule.get("any_of").and_then(|a| a.get("filter")))
        .or_else(|| rule.get("filter"))
    {
        return intervals_apply_filter(&raw, filter, tokens);
    }
    raw
}

/// Core rule → intervals resolver (filter-agnostic — `intervals_eval`
/// wraps this and applies filters at each level).
fn intervals_eval_inner(rule: &Value, tokens: &[String]) -> Option<Vec<TokenInterval>> {
    if let Some(m) = rule.get("match") {
        return Some(intervals_match(m, tokens));
    }
    if let Some(p) = rule.get("prefix") {
        let pref = p
            .get("prefix")
            .and_then(Value::as_str)
            .or_else(|| p.as_str())
            .unwrap_or("")
            .to_lowercase();
        if pref.is_empty() {
            return Some(Vec::new());
        }
        return Some(
            tokens
                .iter()
                .enumerate()
                .filter(|(_, t)| t.starts_with(&pref))
                .map(|(i, _)| TokenInterval { start: i, end: i })
                .collect(),
        );
    }
    if let Some(w) = rule.get("wildcard") {
        let pat = w
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        if pat.is_empty() {
            return Some(Vec::new());
        }
        // Convert ES wildcard ('?'=single, '*'=any) to a regex.
        let mut re = String::from("^");
        for c in pat.chars() {
            match c {
                '?' => re.push('.'),
                '*' => re.push_str(".*"),
                c if c.is_alphanumeric() => re.push(c),
                _ => {
                    re.push('\\');
                    re.push(c);
                }
            }
        }
        re.push('$');
        let regex = match regex::Regex::new(&re) {
            Ok(r) => r,
            Err(_) => return Some(Vec::new()),
        };
        return Some(
            tokens
                .iter()
                .enumerate()
                .filter(|(_, t)| regex.is_match(t))
                .map(|(i, _)| TokenInterval { start: i, end: i })
                .collect(),
        );
    }
    if let Some(r) = rule.get("regexp") {
        let pat = r
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        if pat.is_empty() {
            return Some(Vec::new());
        }
        let anchored = format!("^(?:{})$", pat);
        let regex = match regex::Regex::new(&anchored) {
            Ok(r) => r,
            Err(_) => return Some(Vec::new()),
        };
        return Some(
            tokens
                .iter()
                .enumerate()
                .filter(|(_, t)| regex.is_match(t))
                .map(|(i, _)| TokenInterval { start: i, end: i })
                .collect(),
        );
    }
    if let Some(r) = rule.get("range") {
        let gte = r
            .get("gte")
            .and_then(Value::as_str)
            .map(|s| s.to_lowercase());
        let gt = r
            .get("gt")
            .and_then(Value::as_str)
            .map(|s| s.to_lowercase());
        let lte = r
            .get("lte")
            .and_then(Value::as_str)
            .map(|s| s.to_lowercase());
        let lt = r
            .get("lt")
            .and_then(Value::as_str)
            .map(|s| s.to_lowercase());
        return Some(
            tokens
                .iter()
                .enumerate()
                .filter(|(_, t)| {
                    if let Some(ref lo) = gte {
                        if t.as_str() < lo.as_str() {
                            return false;
                        }
                    }
                    if let Some(ref lo) = gt {
                        if t.as_str() <= lo.as_str() {
                            return false;
                        }
                    }
                    if let Some(ref hi) = lte {
                        if t.as_str() > hi.as_str() {
                            return false;
                        }
                    }
                    if let Some(ref hi) = lt {
                        if t.as_str() >= hi.as_str() {
                            return false;
                        }
                    }
                    true
                })
                .map(|(i, _)| TokenInterval { start: i, end: i })
                .collect(),
        );
    }
    if let Some(fz) = rule.get("fuzzy") {
        let t = fz
            .get("term")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        if t.is_empty() {
            return Some(Vec::new());
        }
        // Fuzziness AUTO: 0 for len<3, 1 for 3..=5, 2 for >5.
        let max_edits = if t.chars().count() < 3 {
            0
        } else if t.chars().count() <= 5 {
            1
        } else {
            2
        };
        let within = |a: &str, b: &str, n: usize| -> bool {
            if a == b {
                return true;
            }
            if n == 0 {
                return false;
            }
            // Simple Levenshtein distance up to bound `n`.
            let a: Vec<char> = a.chars().collect();
            let b: Vec<char> = b.chars().collect();
            let (m, nn) = (a.len(), b.len());
            if m.abs_diff(nn) > n {
                return false;
            }
            let mut prev: Vec<usize> = (0..=nn).collect();
            let mut curr = vec![0usize; nn + 1];
            for i in 1..=m {
                curr[0] = i;
                for j in 1..=nn {
                    let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
                    curr[j] = (curr[j - 1] + 1).min(prev[j] + 1).min(prev[j - 1] + cost);
                }
                std::mem::swap(&mut prev, &mut curr);
            }
            prev[nn] <= n
        };
        return Some(
            tokens
                .iter()
                .enumerate()
                .filter(|(_, dt)| within(dt, &t, max_edits))
                .map(|(i, _)| TokenInterval { start: i, end: i })
                .collect(),
        );
    }
    if let Some(ao) = rule.get("all_of") {
        let subs = ao.get("intervals").and_then(Value::as_array)?;
        let ordered = ao.get("ordered").and_then(Value::as_bool).unwrap_or(false);
        let max_gaps = ao.get("max_gaps").and_then(Value::as_i64);
        let per_sub: Vec<Vec<TokenInterval>> =
            subs.iter().map(|s| intervals_eval(s, tokens)).collect();
        return Some(intervals_combine_all(&per_sub, ordered, max_gaps));
    }
    if let Some(ao) = rule.get("any_of") {
        let subs = ao.get("intervals").and_then(Value::as_array)?;
        let mut out: Vec<TokenInterval> = Vec::new();
        for sub in subs {
            out.extend(intervals_eval(sub, tokens));
        }
        // Dedup and sort.
        out.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
        out.dedup();
        return Some(out);
    }
    None
}

/// Resolve a `match` rule — single-token leaf OR multi-token with
/// ordered/max_gaps constraints. Emits the minimal intervals spanning
/// every query-token occurrence in the doc.
fn intervals_match(m: &Value, tokens: &[String]) -> Vec<TokenInterval> {
    let q = m.get("query").and_then(Value::as_str).unwrap_or("");
    let ordered = m.get("ordered").and_then(Value::as_bool).unwrap_or(false);
    let max_gaps = m.get("max_gaps").and_then(Value::as_i64);
    let qtokens: Vec<String> = intervals_tokenise(q);
    if qtokens.is_empty() {
        return Vec::new();
    }
    // For each query token, compute its matching doc positions.
    let per_token: Vec<Vec<TokenInterval>> = qtokens
        .iter()
        .map(|qt| {
            tokens
                .iter()
                .enumerate()
                .filter(|(_, dt)| *dt == qt)
                .map(|(i, _)| TokenInterval { start: i, end: i })
                .collect()
        })
        .collect();
    intervals_combine_all(&per_token, ordered, max_gaps)
}

/// Combine a set of interval lists into the minimal enclosing intervals
/// that contain at least one interval from every list, respecting
/// `ordered` and `max_gaps` constraints.
fn intervals_combine_all(
    per_sub: &[Vec<TokenInterval>],
    ordered: bool,
    max_gaps: Option<i64>,
) -> Vec<TokenInterval> {
    if per_sub.is_empty() {
        return Vec::new();
    }
    if per_sub.iter().any(|v| v.is_empty()) {
        return Vec::new();
    }
    if per_sub.len() == 1 {
        return per_sub[0].clone();
    }
    // Enumerate all cross-products — tests have only a few intervals.
    fn rec(
        per: &[Vec<TokenInterval>],
        idx: usize,
        acc: &mut Vec<TokenInterval>,
        out: &mut Vec<Vec<TokenInterval>>,
    ) {
        if idx == per.len() {
            out.push(acc.clone());
            return;
        }
        for iv in &per[idx] {
            acc.push(*iv);
            rec(per, idx + 1, acc, out);
            acc.pop();
        }
    }
    let mut combos: Vec<Vec<TokenInterval>> = Vec::new();
    let mut acc = Vec::with_capacity(per_sub.len());
    rec(per_sub, 0, &mut acc, &mut combos);

    let mut out: Vec<TokenInterval> = Vec::new();
    for combo in combos {
        if ordered {
            // Ordered: intervals must appear in the listed order (starts
            // strictly-increasing, each start > previous end).
            let mut ok = true;
            for i in 1..combo.len() {
                if combo[i].start <= combo[i - 1].end {
                    ok = false;
                    break;
                }
            }
            if !ok {
                continue;
            }
        } else {
            // Unordered: intervals must be disjoint (no overlaps).
            let mut sorted = combo.clone();
            sorted.sort_by_key(|iv| iv.start);
            let mut ok = true;
            for i in 1..sorted.len() {
                if sorted[i].start <= sorted[i - 1].end {
                    ok = false;
                    break;
                }
            }
            if !ok {
                continue;
            }
        }
        let start = combo.iter().map(|iv| iv.start).min().unwrap();
        let end = combo.iter().map(|iv| iv.end).max().unwrap();
        let enclosing = TokenInterval { start, end };
        // max_gaps: Σ_i (sub_i+1.start - sub_i.end - 1) where subs are
        // sorted by position — the total number of non-covered tokens
        // between contiguous sub-intervals.
        if let Some(mg) = max_gaps {
            let mut sorted = combo.clone();
            sorted.sort_by_key(|iv| iv.start);
            let mut gap = 0i64;
            for i in 1..sorted.len() {
                let diff = sorted[i].start as i64 - sorted[i - 1].end as i64 - 1;
                if diff > 0 {
                    gap += diff;
                }
            }
            if gap > mg {
                continue;
            }
        }
        out.push(enclosing);
    }
    out.sort_by(|a, b| {
        a.start.cmp(&b.start).then(
            a.end
                .cmp(&b.end)
                .then((a.width() as i64).cmp(&(b.width() as i64))),
        )
    });
    out.dedup();
    out
}

/// Apply an intervals `filter:` clause — the filter contains one of:
/// containing / not_containing / contained_by / not_contained_by /
/// overlapping / not_overlapping / before / after.
fn intervals_apply_filter(
    intervals: &[TokenInterval],
    filter: &Value,
    tokens: &[String],
) -> Vec<TokenInterval> {
    let filter_obj = match filter.as_object() {
        Some(o) => o,
        None => return intervals.to_vec(),
    };
    if let Some((kind, rule)) = filter_obj.into_iter().next() {
        let others = intervals_eval(rule, tokens);
        let pred: Box<dyn Fn(&TokenInterval) -> bool> = match kind.as_str() {
            "containing" => Box::new(move |iv: &TokenInterval| {
                others
                    .iter()
                    .any(|o| iv.start <= o.start && iv.end >= o.end)
            }),
            "not_containing" => Box::new(move |iv: &TokenInterval| {
                !others
                    .iter()
                    .any(|o| iv.start <= o.start && iv.end >= o.end)
            }),
            "contained_by" => Box::new(move |iv: &TokenInterval| {
                others
                    .iter()
                    .any(|o| o.start <= iv.start && o.end >= iv.end)
            }),
            "not_contained_by" => Box::new(move |iv: &TokenInterval| {
                !others
                    .iter()
                    .any(|o| o.start <= iv.start && o.end >= iv.end)
            }),
            "overlapping" => Box::new(move |iv: &TokenInterval| {
                others
                    .iter()
                    .any(|o| !(o.end < iv.start || iv.end < o.start))
            }),
            "not_overlapping" => Box::new(move |iv: &TokenInterval| {
                !others
                    .iter()
                    .any(|o| !(o.end < iv.start || iv.end < o.start))
            }),
            "before" => Box::new(move |iv: &TokenInterval| {
                // iv ends before every `other` starts.
                others.iter().any(|o| iv.end < o.start)
            }),
            "after" => Box::new(move |iv: &TokenInterval| others.iter().any(|o| o.end < iv.start)),
            _ => Box::new(|_: &TokenInterval| true),
        };
        return intervals.iter().copied().filter(|iv| pred(iv)).collect();
    }
    intervals.to_vec()
}

/// Extract (lat, lon) from a geo_point field value.
///
/// Accepts `{"lat": f64, "lon": f64}`, `[lon, lat]`, or `"lat,lon"`.
fn extract_lat_lon(v: &Value) -> Option<(f64, f64)> {
    match v {
        Value::Object(obj) => {
            let lat = obj.get("lat").and_then(|x| x.as_f64())?;
            let lon = obj.get("lon").and_then(|x| x.as_f64())?;
            Some((lat, lon))
        }
        Value::Array(arr) if arr.len() == 2 => {
            let lon = arr[0].as_f64()?;
            let lat = arr[1].as_f64()?;
            Some((lat, lon))
        }
        Value::String(s) => {
            let parts: Vec<&str> = s.splitn(2, ',').collect();
            if parts.len() != 2 {
                return None;
            }
            let lat = parts[0].trim().parse::<f64>().ok()?;
            let lon = parts[1].trim().parse::<f64>().ok()?;
            Some((lat, lon))
        }
        _ => None,
    }
}

/// Ray-casting point-in-polygon algorithm.
///
/// Points are (lat, lon) pairs.  Returns `true` if (lat, lon) is inside the polygon.
fn point_in_polygon(lat: f64, lon: f64, polygon: &[(f64, f64)]) -> bool {
    let n = polygon.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (yi, xi) = polygon[i];
        let (yj, xj) = polygon[j];
        let intersects =
            ((yi > lat) != (yj > lat)) && (lon < (xj - xi) * (lat - yi) / (yj - yi) + xi);
        if intersects {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Collect the names of all named queries that match the given document source.
///
/// Recursively traverses the query tree, collecting the `name` of every `Named`
/// query whose inner query matches the document.
fn collect_matched_queries(q: &QueryNode, source: &Value) -> Vec<String> {
    let mut names = Vec::new();
    collect_matched_queries_inner(q, source, &mut names);
    names
}

fn collect_matched_queries_inner(q: &QueryNode, source: &Value, names: &mut Vec<String>) {
    match q {
        QueryNode::Named { name, query } => {
            if doc_matches_query(query, source) {
                names.push(name.clone());
            }
            // Also recurse into the wrapped query in case it contains further Named nodes.
            collect_matched_queries_inner(query, source, names);
        }
        QueryNode::Bool {
            must,
            should,
            must_not,
            filter,
            ..
        } => {
            for sub in must
                .iter()
                .chain(should.iter())
                .chain(must_not.iter())
                .chain(filter.iter())
            {
                collect_matched_queries_inner(sub, source, names);
            }
        }
        QueryNode::Boosted { query, .. } | QueryNode::Constant { query, .. } => {
            collect_matched_queries_inner(query, source, names);
        }
        QueryNode::DisMax { queries, .. } => {
            for sub in queries {
                collect_matched_queries_inner(sub, source, names);
            }
        }
        QueryNode::FunctionScore {
            query, functions, ..
        } => {
            collect_matched_queries_inner(query, source, names);
            for f in functions {
                // ES 8.9+: when the function entry itself carries
                // `_name`, that name OVERRIDES any `_name` inside the
                // filter — the filter's inner named queries are not
                // reported as matched. Otherwise we walk the filter to
                // surface its inner named clauses.
                let filter_has_fn_name = f.name.is_some();
                let filter_matches = match &f.filter {
                    Some(filter) => {
                        if !filter_has_fn_name {
                            collect_matched_queries_inner(filter, source, names);
                        }
                        doc_matches_query(filter, source)
                    }
                    None => true,
                };
                if let Some(name) = &f.name {
                    if filter_matches {
                        names.push(name.clone());
                    }
                }
            }
        }
        QueryNode::Boosting {
            positive, negative, ..
        } => {
            collect_matched_queries_inner(positive, source, names);
            collect_matched_queries_inner(negative, source, names);
        }
        QueryNode::Nested { query, .. } => {
            collect_matched_queries_inner(query, source, names);
        }
        QueryNode::Pinned { organic, .. } => {
            collect_matched_queries_inner(organic, source, names);
        }
        _ => {}
    }
}

/// Apply a single rescore stage to the hits list.
///
/// Re-scores the top `window_size` hits using the secondary query and blends
/// the scores: final_score = original * query_weight + rescore * rescore_query_weight.
fn apply_rescore(hits: &mut [Hit], stage: &RescoreQuery) {
    let window = stage.window_size.min(hits.len());

    // ── Query rescore ──────────────────────────────────────────────
    if let Some(ref qspec) = stage.query {
        let q_weight = qspec.query_weight;
        let rq_weight = qspec.rescore_query_weight;
        let rescore_query = &qspec.rescore_query;

        for (i, hit) in hits.iter_mut().enumerate() {
            if i < window {
                let rescore_score = score_query_against_doc(rescore_query, &hit.source);
                hit.score = hit.score * q_weight + rescore_score * rq_weight;
            } else {
                hit.score *= q_weight;
            }
        }
    }

    // ── Script rescore ─────────────────────────────────────────────
    // Compose with the query rescore by treating the script's returned
    // value as a second-pass score combined per `score_mode` (default
    // "total" → sum, weighted by query_weight + rescore_query_weight).
    if let Some(ref sspec) = stage.script {
        // ES script-rescore default: the script's returned value
        // REPLACES the score (the script can reference `_score` to
        // incorporate the original). When `score_mode` is set, combine
        // accordingly.
        let mode = sspec.score_mode.as_deref().unwrap_or("replace");
        let q_w = sspec.query_weight;
        let r_w = sspec.rescore_query_weight;
        let weights_explicit = q_w != 1.0 || r_w != 1.0;
        for (i, hit) in hits.iter_mut().enumerate() {
            if i >= window {
                continue;
            }
            let ctx = crate::painless::PainlessCtx::new(&hit.source, &sspec.params, hit.score);
            let script_score = match crate::painless::eval_painless(&sspec.source, &ctx) {
                Ok(v) => v.as_f64().unwrap_or(0.0) as f32,
                Err(_) => 0.0,
            };
            let original = hit.score;
            // Stash the pre-rescore score into the source under a
            // sentinel key so the explanation builder can surface it
            // in `details.0.value` alongside the `"_score: "` label
            // (ES's canonical rescore explanation shape). Preserves
            // the *first* pre-rescore score across chained rescore
            // stages — each stage sees the prior hit.score there.
            if let Value::Object(ref mut obj) = hit.source {
                if !obj.contains_key("__xy_pre_rescore_score__") {
                    obj.insert(
                        "__xy_pre_rescore_score__".to_string(),
                        serde_json::json!(original),
                    );
                }
            }
            hit.score = match mode {
                "multiply" => original * script_score,
                "min" => original.min(script_score),
                "max" => original.max(script_score),
                "avg" => (original + script_score) / 2.0,
                "total" => original * q_w + script_score * r_w,
                _ => {
                    // Default: replace, unless explicit non-default
                    // weights were configured — then sum weighted.
                    if weights_explicit {
                        original * q_w + script_score * r_w
                    } else {
                        script_score
                    }
                }
            };
        }
    }
}

/// Compute a simple relevance score for a document against a query.
///
/// This is a lightweight scoring function used by the rescore step.
/// It does not use BM25 — it returns 1.0 if the document matches, 0.0 if not,
/// multiplied by any boost factor on the query.
fn score_query_against_doc(q: &QueryNode, source: &Value) -> f32 {
    match q {
        QueryNode::MatchAll => 1.0,
        QueryNode::MatchNone => 0.0,
        QueryNode::Boosted { boost, query } => {
            if doc_matches_query(query, source) {
                *boost
            } else {
                0.0
            }
        }
        QueryNode::Constant { score, query } => {
            if doc_matches_query(query, source) {
                *score
            } else {
                0.0
            }
        }
        QueryNode::Match {
            boost,
            field,
            query,
            ..
        } => {
            if !doc_matches_query(q, source) {
                return 0.0;
            }
            let b = boost.unwrap_or(1.0);
            let tf = match_term_frequency(source, field, query);
            // IDF-less tf norm: the caller (Bool aggregator) will have
            // access to the total matched set later. Contribution here is
            // boost × (1 + ln(1 + tf)). Saturating at ~2.4× for tf=5.
            if tf == 0.0 {
                b
            } else {
                b * (1.0 + (1.0 + tf).ln())
            }
        }
        QueryNode::Term {
            boost,
            field,
            value,
        } => {
            if !doc_matches_query(q, source) {
                return 0.0;
            }
            let b = boost.unwrap_or(1.0);
            let q_str = match value {
                Value::String(s) => s.clone(),
                other => other.to_string().trim_matches('"').to_string(),
            };
            let tf = match_term_frequency(source, field, &q_str);
            if tf == 0.0 {
                b
            } else {
                b * (1.0 + (1.0 + tf).ln())
            }
        }
        QueryNode::Terms { boost, .. }
        | QueryNode::Range { boost, .. }
        | QueryNode::Prefix { boost, .. }
        | QueryNode::Wildcard { boost, .. } => {
            let b = boost.unwrap_or(1.0);
            if doc_matches_query(q, source) {
                b
            } else {
                0.0
            }
        }
        QueryNode::Bool {
            must,
            should,
            must_not,
            filter,
            minimum_should_match,
        } => {
            if !doc_matches_query(q, source) {
                return 0.0;
            }
            // Sum all contributing sub-query scores. DON'T clamp to 1.0
            // minimum — the score gradient is what lets downstream
            // ranking distinguish "doc matches 1 clause" from "doc
            // matches 3 clauses".
            let mut score = 0.0f32;
            for sub in must {
                score += score_query_against_doc(sub, source);
            }
            for sub in should {
                score += score_query_against_doc(sub, source);
            }
            let _ = (filter, must_not, minimum_should_match);
            if score == 0.0 {
                1.0
            } else {
                score
            }
        }
        QueryNode::Named { query, .. } => score_query_against_doc(query, source),
        // MultiMatch with field boosts. ES semantics per type:
        //   best_fields (default) → dis_max: MAX of the per-field scores.
        //   most_fields           → sum of the per-field scores.
        QueryNode::MultiMatch {
            fields,
            query,
            boost,
            match_type,
            ..
        } => {
            let q_lower = query.to_lowercase();
            let outer_boost = boost.unwrap_or(1.0);
            let is_cross = matches!(match_type, xerj_query::ast::MultiMatchType::CrossFields);
            let mut sum_score = 0.0f32;
            let mut max_score = 0.0f32;
            let mut matched = false;
            for field_spec in fields {
                let (field, field_boost) = parse_field_boost(field_spec);
                if let Some(Value::String(s)) = get_field_value(source, field) {
                    if s.to_lowercase().contains(&q_lower) {
                        sum_score += field_boost;
                        if field_boost > max_score {
                            max_score = field_boost;
                        }
                        matched = true;
                    }
                }
            }
            if !matched {
                // cross_fields (combined_fields) pools tokens across fields:
                // a multi-token query whose tokens are scattered across the
                // fields matches even though no single field contains the
                // whole query. Mirror the doc_matches_query pooling so such
                // hits score non-zero (was returning 0.0 → dropped by scored
                // paths / rescore). Score is the fraction of query tokens
                // present in the pooled text, times the outer boost.
                if is_cross {
                    let tokens: Vec<&str> = q_lower
                        .split(|c: char| !c.is_alphanumeric())
                        .filter(|t| !t.is_empty())
                        .collect();
                    if !tokens.is_empty() {
                        let combined = fields
                            .iter()
                            .filter_map(|fs| {
                                let (f, _) = parse_field_boost(fs);
                                match get_field_value(source, f) {
                                    Some(Value::String(s)) => Some(s.to_lowercase()),
                                    Some(Value::Array(arr)) => Some(
                                        arr.iter()
                                            .map(|v| match v {
                                                Value::String(s) => s.to_lowercase(),
                                                other => other.to_string(),
                                            })
                                            .collect::<Vec<_>>()
                                            .join(" "),
                                    ),
                                    Some(other) => Some(other.to_string().to_lowercase()),
                                    None => None,
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                        let combined_tokens: std::collections::HashSet<&str> = combined
                            .split(|c: char| !c.is_alphanumeric())
                            .filter(|t| !t.is_empty())
                            .collect();
                        let hits = tokens
                            .iter()
                            .filter(|t| combined_tokens.contains(**t))
                            .count();
                        if hits > 0 {
                            return (hits as f32 / tokens.len() as f32) * outer_boost;
                        }
                    }
                }
                return 0.0;
            }
            let combined = if matches!(match_type, xerj_query::ast::MultiMatchType::MostFields) {
                sum_score
            } else {
                max_score
            };
            combined * outer_boost
        }
        QueryNode::FunctionScore {
            query,
            functions,
            score_mode,
            boost_mode,
            ..
        } => {
            // Score the inner query, then run the function scores against
            // it — same flow as the main search path, but executed on
            // demand for rescore.
            if !doc_matches_query(query, source) {
                return 0.0;
            }
            let inner = score_query_against_doc(query, source);
            let doc_id = source.get("_id").and_then(Value::as_str).unwrap_or("");
            let fn_score = apply_function_score(doc_id, source, functions, *score_mode, inner);
            combine_scores(inner, fn_score, *boost_mode)
        }
        QueryNode::Intervals { field, .. } => {
            // Mirror Lucene's intervals scoring: each match contributes
            // BM25-like weight 1/sqrt(doc_length) so shorter docs rank
            // higher when both match. Falls back to 1.0 when the field
            // isn't a string we can tokenise.
            if !doc_matches_query(q, source) {
                return 0.0;
            }
            let text = match get_field_value(source, field) {
                Some(Value::String(s)) => s,
                _ => return 1.0,
            };
            let tokens = intervals_tokenise(&text);
            let dl = tokens.len().max(1) as f32;
            1.0 / dl.sqrt()
        }
        other => {
            if doc_matches_query(other, source) {
                1.0
            } else {
                0.0
            }
        }
    }
}

/// Returns true when the query tree is a Bool with only Match/MultiMatch/
/// Term children (no Prefix/Wildcard/etc.) and has two or more text
/// clauses. We only rescore this shape because mixed Bool trees (e.g.
/// match_bool_prefix → [Match, Prefix]) have per-clause scores that
/// would break if we zeroed out the non-Match contribution.
fn query_uses_bool_text(q: &QueryNode) -> bool {
    fn walk(q: &QueryNode) -> (u32, bool) {
        match q {
            QueryNode::Bool {
                must,
                should,
                filter,
                must_not,
                ..
            } => {
                let mut text_children = 0u32;
                let mut any_disqualifying = false;
                let mut any_sub_bool = false;
                for sub in must
                    .iter()
                    .chain(should.iter())
                    .chain(filter.iter())
                    .chain(must_not.iter())
                {
                    match sub {
                        QueryNode::Match { .. }
                        | QueryNode::MultiMatch { .. }
                        | QueryNode::Term { .. } => text_children += 1,
                        QueryNode::Bool { .. }
                        | QueryNode::Named { .. }
                        | QueryNode::Boosted { .. }
                        | QueryNode::Constant { .. } => {
                            let (c, b) = walk(sub);
                            text_children += c;
                            any_sub_bool = any_sub_bool || b;
                        }
                        QueryNode::Prefix { .. }
                        | QueryNode::Wildcard { .. }
                        | QueryNode::Fuzzy { .. }
                        | QueryNode::Regexp { .. }
                        | QueryNode::MatchPhrase { .. }
                        | QueryNode::MatchPhrasePrefix { .. }
                        | QueryNode::Range { .. } => {
                            any_disqualifying = true;
                        }
                        _ => {}
                    }
                }
                if any_disqualifying {
                    (0, false)
                } else {
                    (text_children, text_children >= 2 || any_sub_bool)
                }
            }
            QueryNode::Named { query, .. }
            | QueryNode::Boosted { query, .. }
            | QueryNode::Constant { query, .. } => walk(query),
            _ => (0, false),
        }
    }
    walk(q).1
}

/// Flatten Match/Term clauses out of a Bool query tree into
/// (field, term, boost) triples so the IDF rescore pass can compute
/// per-term document frequency AND weight each clause's contribution by
/// its ES `boost` (dropping the boost here made boosted and unboosted
/// clauses score identically — test_weighted_bool_boost_ranking).
fn collect_match_field_terms(q: &QueryNode) -> Vec<(String, String, f32)> {
    let mut out: Vec<(String, String, f32)> = Vec::new();
    fn walk(q: &QueryNode, mult: f32, out: &mut Vec<(String, String, f32)>) {
        match q {
            QueryNode::Match {
                field,
                query,
                boost,
                ..
            } => {
                out.push((field.clone(), query.clone(), mult * boost.unwrap_or(1.0)));
            }
            QueryNode::Term {
                field,
                value,
                boost,
            } => {
                let s = match value {
                    Value::String(s) => s.clone(),
                    other => other.to_string().trim_matches('"').to_string(),
                };
                out.push((field.clone(), s, mult * boost.unwrap_or(1.0)));
            }
            QueryNode::MultiMatch {
                fields,
                query,
                boost,
                ..
            } => {
                let qb = boost.unwrap_or(1.0);
                for f in fields {
                    let (f, fb) = parse_field_boost(f);
                    out.push((f.to_string(), query.clone(), mult * qb * fb));
                }
            }
            QueryNode::Bool {
                must,
                should,
                filter,
                ..
            } => {
                for sub in must.iter().chain(should.iter()).chain(filter.iter()) {
                    walk(sub, mult, out);
                }
            }
            QueryNode::Boosted { boost, query } => walk(query, mult * *boost, out),
            QueryNode::Named { query, .. } | QueryNode::Constant { query, .. } => {
                walk(query, mult, out)
            }
            _ => {}
        }
    }
    walk(q, 1.0, &mut out);
    out
}

/// Count whitespace-token-level occurrences of `query` in the document's
/// value at `field`. Used by score_query_against_doc to weight Match
/// clauses by how often the queried term appears in the doc — without
/// this, a Bool.should of two Match clauses returns score=1.0 for every
/// matching doc regardless of which clauses matched, which collapses
/// BM25-style ranking. Handles string + array-of-string field values.
fn match_term_frequency(source: &Value, field: &str, query: &str) -> f32 {
    let q = query.to_lowercase();
    let q_terms: Vec<&str> = q.split_whitespace().collect();
    if q_terms.is_empty() {
        return 0.0;
    }
    let count_in_str = |s: &str| -> f32 {
        let lc = s.to_lowercase();
        let mut total = 0f32;
        for qt in &q_terms {
            total += lc
                .split(|c: char| !c.is_alphanumeric())
                .filter(|t| *t == *qt)
                .count() as f32;
        }
        total
    };
    let v = get_field_value(source, field);
    match v {
        Some(Value::String(s)) => count_in_str(&s),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|e| match e {
                Value::String(s) => Some(count_in_str(s)),
                _ => None,
            })
            .sum(),
        _ => 0.0,
    }
}

/// Parse a field specifier that may carry a boost factor (`"title^3"` or `"body"`).
///
/// Returns `(field_name, boost)`.  The boost defaults to `1.0` when absent.
fn parse_field_boost(field_spec: &str) -> (&str, f32) {
    if let Some(pos) = field_spec.rfind('^') {
        let name = &field_spec[..pos];
        let boost = field_spec[pos + 1..].parse::<f32>().unwrap_or(1.0);
        (name, boost)
    } else {
        (field_spec, 1.0)
    }
}

/// Check if a query string matches any field matching a wildcard field pattern.
///
/// - `"*"` matches all string fields in the document.
/// - `"field*"` matches all fields whose name starts with "field".
fn match_any_field_wildcard(source: &Value, field_pattern: &str, query_lower: &str) -> bool {
    let prefix = if field_pattern == "*" {
        ""
    } else {
        field_pattern.strip_suffix('*').unwrap_or(field_pattern)
    };

    fn check_value(v: &Value, q: &str) -> bool {
        match v {
            Value::String(s) => s.to_lowercase().contains(q),
            Value::Array(arr) => arr.iter().any(|elem| check_value(elem, q)),
            _ => false,
        }
    }

    fn scan_object(obj: &serde_json::Map<String, Value>, prefix: &str, q: &str) -> bool {
        for (key, val) in obj {
            if (prefix.is_empty() || key.starts_with(prefix)) && check_value(val, q) {
                return true;
            }
            // Also recurse into nested objects.
            if let Value::Object(nested) = val {
                if scan_object(nested, prefix, q) {
                    return true;
                }
            }
        }
        false
    }

    match source.as_object() {
        Some(obj) => scan_object(obj, prefix, query_lower),
        None => false,
    }
}

/// Get a field value from a JSON object (supports dot-notation paths with array expansion).
///
/// - Nested objects: `user.name` in `{"user": {"name": "John"}}` → `"John"`
/// - Arrays of objects: `tags.key` in `{"tags": [{"key":"a"}, {"key":"b"}]}` → `["a","b"]`
/// - Deep nesting: `a.b.c` in `{"a": {"b": {"c": 42}}}` → `42`
/// - Dotted source keys (subobjects:false): `a.b.c` in `{"a.b.c": 42}` → `42`
///   Tries the literal full path first, then falls back to walking parts.
// ── Global top-(from+size) sorted collector ───────────────────────────────
//
// Bug fix (sort top-N correctness): a field-sorted query used to keep only the
// FIRST `materialisation_limit` (≈256) hits in *scan order* and then sort that
// truncated set — so once total matches exceeded the cap it returned a
// non-deterministic subset instead of the GLOBAL top-N by the sort key.
//
// `SortTopK` replaces that arrival-order cap with a bounded max-heap keyed by
// the SAME total order as the final `final_hits.sort_by` (ascending
// `(compare_sort_keys, _id)`).  Because `BinaryHeap` is a max-heap, its
// greatest element is the hit that sorts LAST — exactly the one to evict when
// the heap exceeds `from+size` capacity.  Every source (memtable shards + each
// segment, memtable-first for newest-wins dedup) funnels through `offer`, so
// the heap ends holding the true global top-(from+size).  `_source` is only
// materialised for hits that reach the heap; retained memory stays
// O(from+size), preserving F1's win.
struct SortHeapEntry {
    hit: Hit,
    fields: Arc<Vec<xerj_query::sort::SortField>>,
}
impl PartialEq for SortHeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}
impl Eq for SortHeapEntry {}
impl PartialOrd for SortHeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for SortHeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        xerj_query::sort::compare_sort_keys(&self.hit.sort, &other.hit.sort, &self.fields)
            .then_with(|| self.hit.id.cmp(&other.hit.id))
    }
}

/// Bounded top-(from+size) collector for field-sorted queries.
struct SortTopK {
    heap: std::collections::BinaryHeap<SortHeapEntry>,
    fields: Arc<Vec<xerj_query::sort::SortField>>,
    cap: usize,
    /// Normalized `search_after` cursor.  Hits whose sort key is NOT
    /// strictly greater than this key (in the request's sort order) are
    /// rejected BEFORE touching the heap — ES cursor semantics: a page
    /// starts strictly after the cursor.  Applying the cursor during
    /// collection (rather than post-hoc on the drained top-N) is what
    /// makes deep cursors both correct (a cursor past the materialisation
    /// cap used to yield an empty/wrong page, because the collector had
    /// already discarded everything past the top-(from+size)) and cheap
    /// (page N costs the same as page 1).
    after: Option<Vec<Value>>,
}
impl SortTopK {
    fn new(
        fields: Arc<Vec<xerj_query::sort::SortField>>,
        cap: usize,
        after: Option<Vec<Value>>,
    ) -> Self {
        Self {
            heap: std::collections::BinaryHeap::new(),
            fields,
            cap: cap.max(1),
            after,
        }
    }
    /// Cheap pre-admission check on a computed sort key.  Returns `false`
    /// when the hit provably cannot appear in the result page:
    /// - it sorts at or before the `search_after` cursor, or
    /// - the heap is full and the hit sorts strictly after the current
    ///   worst (key-only comparison; key ties are admitted and resolved
    ///   by the heap's `_id` tie-break, matching the pre-existing
    ///   push-then-evict semantics).
    ///
    /// Callers on the hot scan path use this BEFORE cloning `_source`, so
    /// rejected hits cost zero allocations.
    fn would_admit(&self, key: &[Value]) -> bool {
        if let Some(after) = &self.after {
            if xerj_query::sort::compare_sort_keys(key, after, &self.fields)
                != std::cmp::Ordering::Greater
            {
                return false;
            }
        }
        if self.heap.len() >= self.cap {
            if let Some(worst) = self.heap.peek() {
                if xerj_query::sort::compare_sort_keys(key, &worst.hit.sort, &self.fields)
                    == std::cmp::Ordering::Greater
                {
                    return false;
                }
            }
        }
        true
    }
    /// Offer a fully-sourced hit.  Computes its sort key from `_source`,
    /// then delegates to `offer_keyed`.
    fn offer(&mut self, mut hit: Hit) {
        hit.sort = compute_sort_values(&hit.source, hit.score, &hit.id, &self.fields);
        self.offer_keyed(hit);
    }
    /// Offer a hit whose `sort` key is already computed.  Applies the
    /// cursor + heap-worst rejection, inserts into the bounded heap, and
    /// evicts the current worst (the element that sorts last) when over
    /// capacity.
    fn offer_keyed(&mut self, hit: Hit) {
        if !self.would_admit(&hit.sort) {
            return;
        }
        self.heap.push(SortHeapEntry {
            hit,
            fields: Arc::clone(&self.fields),
        });
        if self.heap.len() > self.cap {
            self.heap.pop();
        }
    }
    fn into_hits(self) -> Vec<Hit> {
        self.heap.into_iter().map(|e| e.hit).collect()
    }
    /// Primary-key-only rejection for candidates whose FIRST sort value is
    /// known numerically BEFORE the doc is parsed (sorted-DV candidate
    /// hydration).  Returns `true` only when the heap is full AND `v` is
    /// STRICTLY worse than the current worst's primary value — primary
    /// ties are conservatively admitted (the full-key/_id comparison
    /// decides post-parse).  Never consults the cursor (a primary tie with
    /// the cursor may still be after it on a secondary key).
    fn primary_f64_rejects(&self, v: f64) -> bool {
        use xerj_query::sort::SortOrder;
        if self.heap.len() < self.cap {
            return false;
        }
        let Some(worst) = self.heap.peek() else {
            return false;
        };
        let Some(w) = worst.hit.sort.first().and_then(Value::as_f64) else {
            return false;
        };
        match self.fields.first().map(|f| f.order) {
            Some(SortOrder::Asc) => v > w,
            Some(SortOrder::Desc) => v < w,
            None => false,
        }
    }
}

/// Per-segment field-sort candidate set: `set` for the stored-scan
/// pre-filter (positional membership), `ordered` — the SAME positions in
/// page order (best-first) with their primary sort-key bits — for the
/// hydration path's pre-parse rejection / early break.
struct SortCandidates {
    set: HashSet<u32>,
    ordered: Vec<(i64, u32)>,
}

/// One balanced-brace pass over a `[{...}, {...}, ...]` stored section,
/// returning each top-level object's `(start, end)` byte range.  Identical
/// walk to `scan_stored_section_into`, minus every per-doc parse.  Returns
/// however many complete objects it found (caller compares against the
/// segment doc count to reject malformed sections).
/// Extract the `_id` string value from a stored-doc slice
/// (`{"_id":"<value>",...}`) without a full JSON parse.  Returns `None` when
/// the slice does not begin with an `_id` string key, or when the value
/// contains a JSON escape (`\`), in which case the caller falls back to a full
/// parse.  This is the hot path for building the per-segment id→position map:
/// the common append-only corpus has escape-free ids, so it stays a cheap
/// prefix scan.
fn extract_stored_id(slice: &[u8]) -> Option<String> {
    let n = slice.len();
    let mut i = 0usize;
    // Leading whitespace + opening brace.
    while i < n && slice[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= n || slice[i] != b'{' {
        return None;
    }
    i += 1;
    while i < n && slice[i].is_ascii_whitespace() {
        i += 1;
    }
    // Expect the `_id` key verbatim as the first field.
    const KEY: &[u8] = b"\"_id\"";
    if i + KEY.len() > n || &slice[i..i + KEY.len()] != KEY {
        return None;
    }
    i += KEY.len();
    while i < n && slice[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= n || slice[i] != b':' {
        return None;
    }
    i += 1;
    while i < n && slice[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= n || slice[i] != b'"' {
        return None; // non-string _id (unexpected) → fall back
    }
    i += 1;
    let val_start = i;
    while i < n {
        match slice[i] {
            b'\\' => return None, // escape present → let the caller full-parse
            b'"' => {
                return std::str::from_utf8(&slice[val_start..i])
                    .ok()
                    .map(str::to_string)
            }
            _ => i += 1,
        }
    }
    None
}

/// Exact set of stored positions matching a `term`/`terms` value list in one
/// segment, resolved from the segment's doc-values columns.  Keyword fields
/// enumerate positions carrying any of the terms' ordinals; numeric fields
/// union the degenerate `[v, v]` ranges.  `Some(∅)` = no value present in this
/// segment; `None` = the field has no dv column or a value isn't numeric on a
/// numeric column (→ caller abandons the shortcut).  Mirrors
/// `build_term_prefilter_cached` but operates on already-loaded columns.
fn seg_term_positions(
    cols: &std::collections::BTreeMap<String, xerj_storage::doc_values::Column>,
    field: &str,
    values: &[Value],
) -> Option<HashSet<u32>> {
    use xerj_storage::doc_values::Column;
    match cols.get(field)? {
        Column::Keyword(k) => {
            let mut ords: HashSet<u32> = HashSet::new();
            for v in values {
                let term = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                if let Some(ord) = k.ord_for_term(&term) {
                    ords.insert(ord);
                }
            }
            let mut set: HashSet<u32> = HashSet::new();
            if ords.is_empty() {
                return Some(set); // no listed value present in this segment
            }
            for (pos, o) in k.ords.iter().enumerate() {
                if ords.contains(o) && !k.null_bitmap.contains(pos as u32) {
                    set.insert(pos as u32);
                }
            }
            Some(set)
        }
        Column::Numeric(n) => {
            let mut set: HashSet<u32> = HashSet::new();
            for v in values {
                let f = v.as_f64()?;
                set.extend(n.range_doc_ids(f, f, true, true));
            }
            Some(set)
        }
    }
}

/// Exact set of stored positions matching a numeric `range` in one segment.
/// `None` when the field has no numeric dv column or a bound is a
/// non-numeric, non-date string (→ caller abandons the shortcut).
fn seg_range_positions(
    cols: &std::collections::BTreeMap<String, xerj_storage::doc_values::Column>,
    field: &str,
    gte: Option<&Value>,
    gt: Option<&Value>,
    lte: Option<&Value>,
    lt: Option<&Value>,
) -> Option<HashSet<u32>> {
    use xerj_storage::doc_values::Column;
    let parse = |v: Option<&Value>| -> Option<Option<f64>> {
        match v {
            None => Some(None),
            Some(Value::Number(_)) => Some(v.and_then(|x| x.as_f64())),
            Some(Value::String(s)) => {
                if let Ok(f) = s.parse::<f64>() {
                    return Some(Some(f));
                }
                crate::aggs::parse_date_ms(&Value::String(s.clone())).map(|ms| Some(ms as f64))
            }
            _ => None,
        }
    };
    let gte = parse(gte)?;
    let gt = parse(gt)?;
    let lte = parse(lte)?;
    let lt = parse(lt)?;
    let (lo, lo_incl) = match (gte, gt) {
        (Some(v), _) => (v, true),
        (None, Some(v)) => (v, false),
        (None, None) => (f64::NEG_INFINITY, true),
    };
    let (hi, hi_incl) = match (lte, lt) {
        (Some(v), _) => (v, true),
        (None, Some(v)) => (v, false),
        (None, None) => (f64::INFINITY, true),
    };
    let Column::Numeric(n) = cols.get(field)? else {
        return None;
    };
    Some(n.range_doc_ids(lo, hi, lo_incl, hi_incl).into_iter().collect())
}

fn brace_walk_offsets(bytes: &[u8]) -> Vec<(u32, u32)> {
    let n = bytes.len();
    let mut out: Vec<(u32, u32)> = Vec::new();
    let mut i = 0usize;
    while i < n && (bytes[i].is_ascii_whitespace() || bytes[i] == b'[') {
        i += 1;
    }
    loop {
        while i < n && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= n || bytes[i] == b']' || bytes[i] != b'{' {
            break;
        }
        let start = i;
        let mut depth = 0i32;
        let mut in_str = false;
        let mut escape = false;
        while i < n {
            let b = bytes[i];
            if in_str {
                if escape {
                    escape = false;
                } else if b == b'\\' {
                    escape = true;
                } else if b == b'"' {
                    in_str = false;
                }
            } else {
                match b {
                    b'"' => in_str = true,
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        }
        if depth != 0 {
            break;
        }
        out.push((start as u32, i as u32));
    }
    out
}

/// Decompressed stored section of one segment plus per-doc byte ranges,
/// cached so the sorted-DV candidate path can hydrate specific doc
/// positions without a per-query decompress + O(bytes) brace re-scan.
/// See `Index::stored_slices_cache`.
struct StoredSlices {
    bytes: Vec<u8>,
    /// `offsets[pos] = (start, end)` byte range of the doc at stored
    /// position `pos` inside `bytes`.
    offsets: Vec<(u32, u32)>,
}

impl StoredSlices {
    fn retained_bytes(&self) -> u64 {
        (self.bytes.len() + self.offsets.len() * 8 + 64) as u64
    }
}

/// Segment sort-key shadow builder — shared by the query-path
/// `Index::sorted_shadow_for` and the publish-time warm.  See
/// `sorted_shadow_for` for the eligibility contract (no nulls, full
/// coverage, all-date keyword terms).
fn build_sort_shadow(
    cols: &std::collections::BTreeMap<String, xerj_storage::doc_values::Column>,
    field: &str,
    seg_doc_count: u64,
) -> Option<Arc<Vec<(i64, u32)>>> {
    use xerj_storage::doc_values::Column;
    match cols.get(field) {
        Some(Column::Numeric(n)) => {
            if !n.null_bitmap.is_empty()
                || n.doc_count as u64 != seg_doc_count
                || n.sorted.is_empty()
            {
                return None;
            }
            Some(Arc::new(n.sorted.clone()))
        }
        Some(Column::Keyword(k)) => {
            if !k.null_bitmap.is_empty()
                || k.doc_count as u64 != seg_doc_count
                || k.ords.len() != k.doc_count as usize
                || k.terms.is_empty()
            {
                return None;
            }
            // Every distinct term must normalise to a NUMBER via the
            // exact per-hit date path; one miss → ineligible (the
            // heap would rank raw strings, not epochs).
            let mut keys: Vec<f64> = Vec::with_capacity(k.terms.len());
            for t in &k.terms {
                keys.push(sort_date_normalize(t)?.as_f64()?);
            }
            let mut sorted: Vec<(i64, u32)> = k
                .ords
                .iter()
                .enumerate()
                .map(|(pos, ord)| (keys[*ord as usize].to_bits() as i64, pos as u32))
                .collect();
            sorted.sort_unstable_by(|a, b| {
                f64::from_bits(a.0 as u64)
                    .partial_cmp(&f64::from_bits(b.0 as u64))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.1.cmp(&b.1))
            });
            Some(Arc::new(sorted))
        }
        None => None,
    }
}

/// The read-path caches a freshly-published segment must be warm in so
/// its first queries don't pay cold-start costs inside their own latency.
/// Bundled so the flush path (a free fn without `&Index`) can carry ONE
/// handle.  All fields are the Index's own Arcs.
#[derive(Clone)]
struct PublishWarmCaches {
    slices: Arc<dashmap::DashMap<String, Arc<StoredSlices>>>,
    slices_bytes: Arc<AtomicU64>,
    dv: Arc<
        dashmap::DashMap<
            String,
            Arc<std::collections::BTreeMap<String, xerj_storage::doc_values::Column>>,
        >,
    >,
    shadow: Arc<dashmap::DashMap<String, Option<Arc<Vec<(i64, u32)>>>>>,
    shadow_fields: Arc<dashmap::DashMap<String, ()>>,
}

/// Warm one just-written segment's read-path caches OUTSIDE the query
/// path, BEFORE the segment becomes visible:
///  1. `StoredSlices` (decompressed stored section + per-doc offsets) —
///     the sorted-candidates hydration source.  Cold cost the first
///     queries otherwise paid: a full multi-MB (flush) to multi-100 MB
///     (merge) decompress, multiplied by every concurrent query racing
///     the same miss (dec=0.9–3.8 s stall episodes at every publish).
///  2. dv columns — the aggs / shortcut-count / sort-shadow source
///     (terms/cardinality p99 spikes at every publish).
///  3. Sort-key shadows for every field the index has ever field-sorted
///     on (the implicit `@timestamp` in particular) — the O(n log n)
///     build on a merged segment was a ~200-500 ms first-query cost.
///
/// Called (a) by the merge driver right BEFORE `apply_merge` swaps the
/// merged segment in, and (b) inside `do_flush_shard`'s `build_fts`
/// callback, which the storage finaliser runs BEFORE the snapshot rcu
/// publish.  Budget-checked exactly like the query-path inserts; failure
/// or over-budget is non-fatal (queries fall back to the per-query
/// decompress — pre-warm status quo).
fn warm_segment_at_publish(
    store: &IndexStore,
    segments_dir: &std::path::Path,
    caches: &PublishWarmCaches,
    seg_id: &str,
    expect_docs: u64,
) {
    // 1. Stored slices.
    if !caches.slices.contains_key(seg_id) {
        let built: Option<Arc<StoredSlices>> = (|| {
            let reader = store.open_segment_arc(seg_id).ok()?;
            let raw = reader.section(SectionType::Stored).ok()??;
            let bytes = xerj_storage::stored_codec::decode_stored(raw).ok()?;
            if bytes.len() > u32::MAX as usize {
                return None;
            }
            let offsets = brace_walk_offsets(&bytes);
            if offsets.len() as u64 != expect_docs {
                return None;
            }
            Some(Arc::new(StoredSlices { bytes, offsets }))
        })();
        if let Some(slices) = built {
            let sz = slices.retained_bytes();
            if caches
                .slices_bytes
                .load(Ordering::Relaxed)
                .saturating_add(sz)
                <= stored_slices_cache_budget()
                && caches.slices.insert(seg_id.to_string(), slices).is_none()
            {
                caches.slices_bytes.fetch_add(sz, Ordering::Relaxed);
            }
        }
    }
    // 2. dv columns (mirrors `Index::dv_columns_for`'s miss arm).
    let cols: Option<Arc<std::collections::BTreeMap<String, xerj_storage::doc_values::Column>>> =
        if let Some(entry) = caches.dv.get(seg_id) {
            Some(Arc::clone(entry.value()))
        } else {
            let cols = read_doc_values_sidecar(segments_dir, seg_id);
            if cols.is_empty() {
                None
            } else {
                let arc = Arc::new(cols);
                caches.dv.insert(seg_id.to_string(), Arc::clone(&arc));
                Some(arc)
            }
        };
    // 3. Sort shadows for every historically-sorted field.
    if let Some(cols) = cols {
        for e in caches.shadow_fields.iter() {
            let field = e.key();
            let key = format!("{seg_id}\u{1}{field}");
            if !caches.shadow.contains_key(&key) {
                let built = build_sort_shadow(&cols, field, expect_docs);
                caches.shadow.insert(key, built);
            }
        }
    }
}

/// Phase-breakdown log threshold (ms).  `search_inner` logs a per-phase
/// timing breakdown for any query slower than this.  Default 1000ms; set
/// `XERJ_PHASE_LOG_MS` to lower it when profiling the read-under-write tail.
/// Read once (cached) so the hot query path pays no per-request env lookup.
fn phase_log_threshold_ms() -> u64 {
    static T: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
        std::env::var("XERJ_PHASE_LOG_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1_000)
    });
    *T
}

/// Chunk size for the turbo bulk-insert loops (`index_batch_turbo` /
/// `index_batch_turbo_raw`).  The shard write lock is released between chunks
/// so a concurrent search stalls on a writing shard for at most one chunk,
/// not the whole batch.  512 bounds a reader's worst-case stall to ~0.5-1.5 ms
/// while adding only batch_len/512 lock acquisitions per batch (trivial vs the
/// per-doc locking that once regressed ingest 4×).
const MEMTABLE_INSERT_CHUNK: usize = 512;

/// Default GLOBAL doc-count auto-flush sanity cap (across all ingest
/// shards; per-shard trigger is this / shard_count).  This is a
/// SECONDARY guard — the byte budget (`flush_size_mb`) is the primary
/// flush driver.  It exists only to bound the posting-map growth of a
/// pathological tiny-doc workload between byte checkpoints.  Sized so
/// that for realistic log docs the BYTE budget always trips first: at
/// 16 shards a 4 M cap is 250 k docs/shard, which for ~300-byte docs is
/// far more than a 512 MiB/16 = 32 MiB shard-buffer holds, so the
/// byte trigger dominates as intended.  See `resolve_flush_thresholds`.
const FLUSH_DOC_THRESHOLD_DEFAULT: usize = 4_000_000;

/// Cached per-shard flush-trigger STAGGER fraction (`XERJ_FLUSH_STAGGER`,
/// default 0 = off).  When >0, shard `i`'s per-shard threshold is scaled
/// by `1 + frac*(i/(N-1) - 0.5)` — a RAM-neutral centred ramp (mean over
/// shards == base) that phase-offsets when each shard crosses its
/// threshold, so the N ingest shards do NOT all flush in one synchronised
/// storm.  Investigated as a read-under-write p99 mitigation; ships
/// DEFAULT-OFF because measurement showed no material p99 win — the
/// per-storm stall cost is conserved when the storm is spread (see the
/// flush-cadence investigation).  Kept as a tunable for operators.
fn flush_stagger_frac() -> f64 {
    static F: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *F.get_or_init(|| {
        std::env::var("XERJ_FLUSH_STAGGER")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(0.0)
    })
}

/// Per-shard flush threshold = `base_total / n_shards`, optionally
/// phase-staggered by `flush_stagger_frac()` so the shards don't all
/// cross their threshold simultaneously.  The stagger is centred, so the
/// summed per-shard thresholds still equal `base_total` (RAM-neutral).
fn staggered_per_shard_threshold(base_total: usize, shard_idx: usize, n_shards: usize) -> usize {
    let n = n_shards.max(1);
    let base = base_total.div_ceil(n);
    let frac = flush_stagger_frac();
    if frac <= 0.0 || n <= 1 {
        return base;
    }
    let pos = shard_idx as f64 / (n - 1) as f64 - 0.5; // -0.5 ..= 0.5
    ((base as f64) * (1.0 + frac * pos)).max(1.0) as usize
}

/// Retained-memory budget for `stored_slices_cache`.  Inserts stop once the
/// budget is reached (queries then fall back to the per-query decompress
/// path); merge eviction returns the dropped segment's bytes to the budget.
/// 20% of host RAM, ≥2 GB floor (was a flat 3 GB, then a host-tuned 24 GB):
/// a sustained bulk writer grows the corpus past a small flat budget DURING
/// the read window; once the budget is full every over-budget segment is
/// decompressed PER QUERY (insert stops, nothing evicts) — measured
/// dec=28–30 s per match_all against a 21 M-doc corpus even fully quiesced,
/// because with corpus-wide primary-key ties (cycled telemetry timestamps)
/// the per-segment best-candidate rejection never strictly loses, so NO
/// segment is skipped.  Merge eviction returns merged-away segments' bytes.
/// The durable fix — hydrating only GLOBAL winners after a shadow-merge of
/// per-segment candidate keys — is tracked as follow-up.
fn stored_slices_cache_budget() -> u64 {
    static BUDGET: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
        const FLOOR: u64 = 2 * 1024 * 1024 * 1024;
        let ram_total: Option<u64> = std::fs::read_to_string("/proc/meminfo").ok().and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| {
                    l.split_whitespace()
                        .nth(1)
                        .and_then(|kb| kb.parse::<u64>().ok())
                        .map(|kb| kb * 1024)
                })
        });
        ram_total.map(|t| (t / 5).max(FLOOR)).unwrap_or(FLOOR)
    });
    *BUDGET
}

/// Budget for `decoded_stored_cache` (raw decompressed stored sections,
/// ~1× the corpus JSON size).  Overflow → inserts are refused and the
/// query falls back to the per-query decompress (pre-cache behaviour).
const DECODED_STORED_CACHE_BUDGET: u64 = 4 * 1024 * 1024 * 1024;

/// Entry cap for `shortcut_count_cache` (cleared wholesale when exceeded).
const SHORTCUT_COUNT_CACHE_MAX: usize = 65_536;

/// Normalize one `search_after` cursor value so it compares against the
/// normalized `hit.sort` values `compute_sort_values` produces (date-shaped
/// strings → epoch-ms / epoch-ns numbers).  `fmt_hint` is the SortField's
/// ES format pattern, tried first for custom formats like
/// `yyyy-MM-dd | HH:mm:ss.SSS` whose cursor strings the default date
/// detectors below don't recognize.  Non-date values pass through unchanged.
fn normalize_search_after_value(v: &Value, fmt_hint: Option<&str>) -> Value {
    if let Some(s) = v.as_str() {
        if let Some(fmt) = fmt_hint {
            if let Some(epoch) = es_format_to_epoch_ms(s, fmt) {
                return Value::Number(serde_json::Number::from(epoch));
            }
        }
        let bytes = s.as_bytes();
        let looks_date = bytes.len() >= 5
            && bytes[0].is_ascii_digit()
            && bytes[1].is_ascii_digit()
            && bytes[2].is_ascii_digit()
            && bytes[3].is_ascii_digit()
            && bytes[4] == b'-';
        if looks_date {
            let frac_digits = s
                .rsplit_once('.')
                .map(|(_, rest)| rest.chars().take_while(|c| c.is_ascii_digit()).count())
                .unwrap_or(0);
            let is_nanos = frac_digits >= 4;
            let s_utc = s.replace(' ', "T");
            let s_utc = if s_utc.ends_with('Z') || s_utc.contains('+') {
                s_utc
            } else {
                format!("{}Z", s_utc)
            };
            if is_nanos {
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&s_utc) {
                    let secs = dt.timestamp();
                    let nanos = dt.timestamp_subsec_nanos() as i64;
                    if let Some(ns) = secs
                        .checked_mul(1_000_000_000)
                        .and_then(|x| x.checked_add(nanos))
                    {
                        return Value::Number(serde_json::Number::from(ns));
                    }
                }
            }
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&s_utc) {
                return Value::Number(serde_json::Number::from(dt.timestamp_millis()));
            }
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(
                s_utc.trim_end_matches('Z'),
                "%Y-%m-%dT%H:%M:%S%.f",
            ) {
                return Value::Number(serde_json::Number::from(dt.and_utc().timestamp_millis()));
            }
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(
                s_utc.trim_end_matches('Z'),
                "%Y-%m-%dT%H:%M:%S",
            ) {
                return Value::Number(serde_json::Number::from(dt.and_utc().timestamp_millis()));
            }
        }
    }
    v.clone()
}

/// Compute the ES sort-value array for a hit from its `_source` (+ score / id
/// for the `_score` / `_doc` pseudo-fields).  Shared by the bounded
/// `SortTopK` collector (selection) and the final `final_hits.sort_by`
/// population (output ordering) so both agree on the exact total order.
// Date-normalisation helpers shared by `compute_sort_values` (per-hit sort
// keys) and the segment-level sorted-candidates shadow builder
// (`sorted_shadow_for`) — the two MUST agree exactly on how a date-shaped
// string maps to an epoch number, or candidate selection would diverge from
// the heap's ordering.
// When a field sort pulls a date-shaped string out of the source we emit
// epoch-ms (or epoch-ns for nanosecond-precision inputs) instead of the
// raw string, to match ES sort-value semantics.  Heuristic: the value must
// start with a 4-digit year followed by `-`.
fn looks_like_date(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 5
        && bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2].is_ascii_digit()
        && bytes[3].is_ascii_digit()
        && bytes[4] == b'-'
}
// Day-first slash dates (`dd/MM/yyyy[ HH:mm:ss.SSS]`) the year-first
// detector misses.
fn looks_like_slash_date(s: &str) -> bool {
    let head = s.split_whitespace().next().unwrap_or(s);
    let parts: Vec<&str> = head.split('/').collect();
    parts.len() == 3
        && (1..=2).contains(&parts[0].len())
        && parts[0].bytes().all(|b| b.is_ascii_digit())
        && (1..=2).contains(&parts[1].len())
        && parts[1].bytes().all(|b| b.is_ascii_digit())
        && parts[2].len() == 4
        && parts[2].bytes().all(|b| b.is_ascii_digit())
}
fn slash_date_to_epoch(s: &str) -> Option<Value> {
    for pat in [
        "%d/%m/%Y %H:%M:%S%.f",
        "%d/%m/%Y %H:%M:%S",
        "%m/%d/%Y %H:%M:%S%.f",
        "%m/%d/%Y %H:%M:%S",
    ] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, pat) {
            return Some(Value::Number(serde_json::Number::from(
                dt.and_utc().timestamp_millis(),
            )));
        }
    }
    for pat in ["%d/%m/%Y", "%m/%d/%Y"] {
        if let Ok(d) = chrono::NaiveDate::parse_from_str(s, pat) {
            return d.and_hms_opt(0, 0, 0).map(|dt| {
                Value::Number(serde_json::Number::from(dt.and_utc().timestamp_millis()))
            });
        }
    }
    None
}
fn date_string_to_epoch(s: &str) -> Option<Value> {
    let frac_digits = s
        .rsplit_once('.')
        .map(|(_, rest)| rest.chars().take_while(|c| c.is_ascii_digit()).count())
        .unwrap_or(0);
    let is_nanos = frac_digits >= 4;
    let s_utc = s.replace(' ', "T");
    let s_utc = if s_utc.ends_with('Z') || s_utc.contains('+') {
        s_utc
    } else {
        format!("{}Z", s_utc)
    };
    if is_nanos {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&s_utc) {
            let secs = dt.timestamp();
            let nanos = dt.timestamp_subsec_nanos() as i64;
            let epoch_ns = secs.checked_mul(1_000_000_000)?.checked_add(nanos)?;
            return Some(Value::Number(serde_json::Number::from(epoch_ns)));
        }
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(
            s_utc.trim_end_matches('Z'),
            "%Y-%m-%dT%H:%M:%S%.f",
        ) {
            let subsec_nanos = dt.and_utc().timestamp_subsec_nanos() as i64;
            let secs = dt.and_utc().timestamp();
            let epoch_ns = secs.checked_mul(1_000_000_000)?.checked_add(subsec_nanos)?;
            return Some(Value::Number(serde_json::Number::from(epoch_ns)));
        }
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&s_utc) {
        return Some(Value::Number(serde_json::Number::from(
            dt.timestamp_millis(),
        )));
    }
    if let Ok(dt) =
        chrono::NaiveDateTime::parse_from_str(s_utc.trim_end_matches('Z'), "%Y-%m-%dT%H:%M:%S%.f")
    {
        return Some(Value::Number(serde_json::Number::from(
            dt.and_utc().timestamp_millis(),
        )));
    }
    if let Ok(dt) =
        chrono::NaiveDateTime::parse_from_str(s_utc.trim_end_matches('Z'), "%Y-%m-%dT%H:%M:%S")
    {
        return Some(Value::Number(serde_json::Number::from(
            dt.and_utc().timestamp_millis(),
        )));
    }
    if let Ok(dt) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(Value::Number(serde_json::Number::from(
            dt.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis(),
        )));
    }
    None
}

/// Normalise a string sort value the way `compute_sort_values` does:
/// date-shaped strings become epoch-ms / epoch-ns numbers, everything else
/// returns `None` (caller keeps the raw string).
fn sort_date_normalize(s: &str) -> Option<Value> {
    if looks_like_date(s) {
        date_string_to_epoch(s)
    } else if looks_like_slash_date(s) {
        slash_date_to_epoch(s)
    } else {
        None
    }
}

/// Pre-clone rejection for the memtable scan arm: `true` only when the
/// doc's PRIMARY sort key can be derived from `source` withOUT cloning it
/// (root-level literal field; numbers directly, date-shaped strings via
/// the bounded process-wide epoch memo — the EXACT `sort_date_normalize`
/// value `compute_sort_values` would emit) AND the full heap proves it
/// strictly worse on that key (`SortTopK::primary_f64_rejects`: primary
/// ties conservatively admitted, cursor never consulted).  Any shape we
/// can't derive → `false` → the doc takes the identical admit path it
/// always took.
fn memtable_primary_key_rejects(topk: &SortTopK, source: &Value) -> bool {
    let Some(sf) = topk.fields.first() else {
        return false;
    };
    if sf.is_score() || sf.is_doc_order() {
        return false;
    }
    // Root-level literal key only — mirrors `get_field_value`'s fast path.
    // A dotted/nested primary field misses here and is admitted (correct,
    // just not accelerated).
    let Value::Object(obj) = source else {
        return false;
    };
    let Some(raw) = obj.get(&sf.field) else {
        return false;
    };
    let v = match raw {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => sort_epoch_memo(s),
        _ => None,
    };
    match v {
        Some(v) => topk.primary_f64_rejects(v),
        None => false,
    }
}

/// Bounded process-wide `date-string → epoch f64` memo for the memtable
/// pre-clone rejection: telemetry corpora repeat a small set of timestamp
/// strings millions of times, so the chrono parse (~200 ns) collapses to a
/// DashMap hit (~50 ns).  `None` is memoised too (non-date strings rank as
/// raw strings in the heap — `primary_f64_rejects` can never reject those,
/// so callers correctly admit).  Insertion stops at 65 536 entries; misses
/// beyond that just re-parse (pure function of the string, index-agnostic).
fn sort_epoch_memo(s: &str) -> Option<f64> {
    static MEMO: std::sync::LazyLock<dashmap::DashMap<String, Option<f64>>> =
        std::sync::LazyLock::new(dashmap::DashMap::new);
    if let Some(e) = MEMO.get(s) {
        return *e.value();
    }
    let v = sort_date_normalize(s).and_then(|x| x.as_f64());
    if MEMO.len() < 65_536 {
        MEMO.insert(s.to_string(), v);
    }
    v
}

fn compute_sort_values(
    source: &Value,
    score: f32,
    id: &str,
    sort_fields: &[xerj_query::sort::SortField],
) -> Vec<Value> {
    let mut sort_vals: Vec<Value> = Vec::with_capacity(sort_fields.len());
    for sf in sort_fields {
        let v = if sf.is_score() {
            Value::Number(
                serde_json::Number::from_f64(score as f64).unwrap_or(serde_json::Number::from(0)),
            )
        } else if sf.is_doc_order() || sf.field == "_id" {
            // `_id` (and `_doc`) is doc metadata, NOT part of `_source`, so a
            // plain `get_field_value` returns Null — which breaks `_id`-sorted
            // `search_after` cursors (the cursor `[id]` has nothing to compare
            // against). Project the doc id as the sort value so `sort: [_id]`
            // ordering and `search_after: [last_id]` keyset paging (used by
            // reindex) compare correctly. Matches ES, which echoes the id.
            Value::String(id.to_string())
        } else {
            let raw = get_field_value(source, &sf.field).unwrap_or(Value::Null);
            match raw {
                Value::String(ref s) if looks_like_date(s) => {
                    date_string_to_epoch(s).unwrap_or(raw)
                }
                Value::String(ref s) if looks_like_slash_date(s) => {
                    slash_date_to_epoch(s).unwrap_or(raw)
                }
                other => other,
            }
        };
        sort_vals.push(v);
    }
    sort_vals
}

fn get_field_value(source: &Value, field: &str) -> Option<Value> {
    // Fast path: literal dotted key at the source root (subobjects:false).
    if let Value::Object(obj) = source {
        if let Some(v) = obj.get(field) {
            return Some(v.clone());
        }
    }
    let parts: Vec<&str> = field.split('.').collect();
    get_field_value_parts(source, &parts)
}

fn get_field_value_parts(cur: &Value, parts: &[&str]) -> Option<Value> {
    if parts.is_empty() {
        return Some(cur.clone());
    }
    let part = parts[0];
    let rest = &parts[1..];
    match cur {
        Value::Object(map) => {
            // Prefer the longest dotted-key match starting at this level.
            // For `parts = [a, b, c]` over an object containing `"a.b.c"`
            // as a literal key, return that scalar directly.
            for take in (1..=parts.len()).rev() {
                let candidate = parts[..take].join(".");
                if let Some(next) = map.get(&candidate) {
                    let next_rest = &parts[take..];
                    if let Some(v) = get_field_value_parts(next, next_rest) {
                        return Some(v);
                    }
                }
            }
            let next = map.get(part)?;
            get_field_value_parts(next, rest)
        }
        Value::Array(arr) => {
            // When traversing through an array, collect values from each element.
            let collected: Vec<Value> = arr
                .iter()
                .filter_map(|elem| get_field_value_parts(elem, parts))
                .flat_map(|v| match v {
                    Value::Array(inner) => inner,
                    other => vec![other],
                })
                .collect();
            if collected.is_empty() {
                None
            } else if collected.len() == 1 {
                Some(collected.into_iter().next().unwrap())
            } else {
                Some(Value::Array(collected))
            }
        }
        _ => None,
    }
}

/// Compare two JSON values as numbers or strings for range queries.
/// Returns negative/zero/positive like cmp.
fn json_compare(a: &Value, b: &Value) -> i32 {
    // Try numeric comparison first.
    if let (Some(na), Some(nb)) = (a.as_f64(), b.as_f64()) {
        return na.partial_cmp(&nb).map(|o| o as i32).unwrap_or(0);
    }
    // Date-aware path: when both operands are strings and both parse
    // as dates (including French `lun./déc./…` abbreviations), use
    // timestamp ordering rather than lexicographic ordering.
    if let (Value::String(sa), Value::String(sb)) = (a, b) {
        if let (Some(ma), Some(mb)) = (parse_localized_date_ms(sa), parse_localized_date_ms(sb)) {
            return ma.cmp(&mb) as i32;
        }
    }
    // Fall back to string comparison.
    let sa = json_to_str(a);
    let sb = json_to_str(b);
    sa.cmp(&sb) as i32
}

/// Parse a date string via the standard `parse_date_ms` path *and*
/// a French-locale translator (day/month abbreviations mapped to
/// English). Returns `None` when neither the ISO nor the localized
/// form parses. Applies to formats like `E, d MMM yyyy HH:mm:ss Z`
/// with `locale: fr`, e.g. `"mer., 6 déc. 2000 02:55:00 -0800"`.
fn parse_localized_date_ms(s: &str) -> Option<i64> {
    if let Some(ms) = crate::aggs::parse_date_ms(&Value::String(s.to_string())) {
        return Some(ms);
    }
    let en = translate_french_date_tokens(s);
    if en != *s {
        if let Ok(dt) = chrono::DateTime::parse_from_str(&en, "%a, %-d %b %Y %H:%M:%S %z") {
            return Some(dt.timestamp_millis());
        }
        if let Ok(dt) = chrono::DateTime::parse_from_str(&en, "%a, %-d %b %Y %H:%M:%S %:z") {
            return Some(dt.timestamp_millis());
        }
        if let Ok(dt) = chrono::DateTime::parse_from_str(&en, "%a, %d %b %Y %H:%M:%S %z") {
            return Some(dt.timestamp_millis());
        }
    }
    None
}

/// Translate French weekday / month abbreviations (lun./mar./.../déc.)
/// into the three-letter English forms chrono's `%a` / `%b` parsers
/// accept. Non-matching tokens are left unchanged.
fn translate_french_date_tokens(s: &str) -> String {
    let replacements: [(&str, &str); 19] = [
        // Weekdays (with trailing dot).
        ("lun.", "Mon"),
        ("mar.", "Tue"),
        ("mer.", "Wed"),
        ("jeu.", "Thu"),
        ("ven.", "Fri"),
        ("sam.", "Sat"),
        ("dim.", "Sun"),
        // Months (with trailing dot where relevant).
        ("janv.", "Jan"),
        ("févr.", "Feb"),
        ("mars", "Mar"),
        ("avr.", "Apr"),
        ("mai", "May"),
        ("juin", "Jun"),
        ("juil.", "Jul"),
        ("août", "Aug"),
        ("sept.", "Sep"),
        ("oct.", "Oct"),
        ("nov.", "Nov"),
        ("déc.", "Dec"),
    ];
    let mut out = s.to_string();
    for (fr, en) in &replacements {
        out = out.replace(fr, en);
    }
    out
}

fn json_to_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Check equality between a stored document field value and a query value.
fn json_values_equal(doc_val: &Option<Value>, query_val: &Value) -> bool {
    match doc_val {
        None => false,
        Some(dv) => {
            // Direct equality check.
            if dv == query_val {
                return true;
            }
            // Cross-type: number stored as string or vice versa.
            match (dv, query_val) {
                (Value::String(s), Value::Number(n)) => s.parse::<f64>().ok() == n.as_f64(),
                (Value::Number(n), Value::String(s)) => n.as_f64() == s.parse::<f64>().ok(),
                // Array field: any element matches.
                (Value::Array(arr), _) => arr.iter().any(|elem| elem == query_val),
                _ => false,
            }
        }
    }
}

/// Simple wildcard pattern matching: `?` = any single char, `*` = zero or more chars.
fn wildcard_match(text: &str, pattern: &str) -> bool {
    let text: Vec<char> = text.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    wildcard_match_inner(&text, &pattern)
}

fn wildcard_match_inner(text: &[char], pattern: &[char]) -> bool {
    match (text, pattern) {
        (_, []) => text.is_empty(),
        (_, ['*', rest @ ..]) => {
            // * matches zero or more characters.
            wildcard_match_inner(text, rest)
                || (!text.is_empty() && wildcard_match_inner(&text[1..], pattern))
        }
        ([], _) => false,
        ([tc, trest @ ..], [pc, prest @ ..]) => {
            (*pc == '?' || tc == pc) && wildcard_match_inner(trest, prest)
        }
    }
}

/// Compute Levenshtein edit distance between two strings.
fn levenshtein_distance(a: &str, b: &str) -> usize {
    // Damerau-Levenshtein (with transposition) — ES `fuzzy` queries default
    // to `fuzzy_transpositions: true`, meaning an adjacent character swap
    // counts as ONE edit (not two). Without this, `xylophoen` ≠ `xylophone`
    // at fuzziness: 1 even though ES matches it.
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();

    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    #[allow(clippy::needless_range_loop)] // 2D dp: index is both the row and the assigned value
    for i in 0..=m {
        dp[i][0] = i;
    }
    #[allow(clippy::needless_range_loop)] // 2D dp: index is both the column and the assigned value
    for j in 0..=n {
        dp[0][j] = j;
    }

    for i in 1..=m {
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            let mut best = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
            // Transposition: a[i-1]==b[j-2] AND a[i-2]==b[j-1]
            if i >= 2
                && j >= 2
                && a_chars[i - 1] == b_chars[j - 2]
                && a_chars[i - 2] == b_chars[j - 1]
            {
                best = best.min(dp[i - 2][j - 2] + 1);
            }
            dp[i][j] = best;
        }
    }
    dp[m][n]
}

/// Compute the great-circle distance between two lat/lon points using the
/// Haversine formula.  Returns the distance in kilometres.
pub fn haversine_distance(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R: f64 = 6371.0; // Earth's mean radius in km
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    R * c
}

/// AUTO fuzziness: returns max allowed edits based on term length.
///
/// - 0–2 chars: 0 edits
/// - 3–5 chars: 1 edit
/// - 6+ chars: 2 edits
fn auto_fuzziness(term: &str) -> usize {
    let len = term.chars().count();
    if len <= 2 {
        0
    } else if len <= 5 {
        1
    } else {
        2
    }
}

// ── IP address helpers ────────────────────────────────────────────────────────

/// Parse an IPv4 or IPv6 address string to a u128 value for numeric comparison.
///
/// IPv4 addresses are mapped to the IPv4-in-IPv6 space (::ffff:a.b.c.d).
fn parse_ip_to_u128(s: &str) -> Option<u128> {
    // Try IPv4 first.
    if let Ok(addr) = s.parse::<std::net::Ipv4Addr>() {
        let octets = addr.octets();
        let v = u128::from_be_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, octets[0], octets[1], octets[2], octets[3],
        ]);
        return Some(v);
    }
    // Try IPv6.
    if let Ok(addr) = s.parse::<std::net::Ipv6Addr>() {
        return Some(u128::from_be_bytes(addr.octets()));
    }
    None
}

/// Check if an IP address matches a CIDR notation (e.g. "192.168.1.0/24").
///
/// Returns `None` if either address is invalid or `cidr` is not CIDR notation.
/// Returns `Some(true)` if the IP is within the subnet, `Some(false)` otherwise.
fn ip_matches_cidr(ip_str: &str, cidr: &str) -> Option<bool> {
    let slash = cidr.find('/')?;
    let network_str = &cidr[..slash];
    let prefix_len: u32 = cidr[slash + 1..].parse().ok()?;

    let ip = parse_ip_to_u128(ip_str)?;
    let network = parse_ip_to_u128(network_str)?;

    // For IPv4 CIDR, adjust prefix length to IPv6 mapped space (add 96 bits).
    let adjusted_prefix = if network_str.contains(':') {
        // IPv6
        prefix_len.min(128)
    } else {
        // IPv4 mapped to IPv6: add 96 for the IPv4-in-IPv6 prefix.
        (prefix_len + 96).min(128)
    };

    if adjusted_prefix == 0 {
        return Some(true);
    }

    let mask = !0u128 << (128 - adjusted_prefix);
    Some((ip & mask) == (network & mask))
}

/// Collect every field name referenced by an FTS query tree.
///
/// Used to (a) extend the set of side-car fields `FtsIndexReader::open`
/// loads beyond the Text-typed schema fields, and (b) verify that a segment
/// actually has FTS data for every queried field before trusting an FTS
/// result as authoritative.
fn collect_fts_query_fields(q: &FtsQuery, out: &mut Vec<String>) {
    match q {
        FtsQuery::Term(t) => {
            if !out.contains(&t.field) {
                out.push(t.field.clone());
            }
        }
        FtsQuery::Phrase(p) => {
            if !out.contains(&p.field) {
                out.push(p.field.clone());
            }
        }
        FtsQuery::PhrasePrefix(p) => {
            if !out.contains(&p.field) {
                out.push(p.field.clone());
            }
        }
        FtsQuery::Prefix(p) => {
            if !out.contains(&p.field) {
                out.push(p.field.clone());
            }
        }
        FtsQuery::Wildcard(w) => {
            if !out.contains(&w.field) {
                out.push(w.field.clone());
            }
        }
        FtsQuery::Fuzzy(f) => {
            if !out.contains(&f.field) {
                out.push(f.field.clone());
            }
        }
        FtsQuery::Bool(b) => {
            for sub in b
                .must
                .iter()
                .chain(b.should.iter())
                .chain(b.must_not.iter())
            {
                collect_fts_query_fields(sub, out);
            }
        }
        FtsQuery::DisMax(d) => {
            for sub in &d.queries {
                collect_fts_query_fields(sub, out);
            }
        }
        FtsQuery::MatchAll => {}
    }
}

/// Convert a QueryNode to an FTS Query for segment search.
///
/// `exact_fields` are the non-Text schema fields (keyword / numeric / date /
/// bool / ip).  `build_fts_field_configs` indexes those with the `keyword`
/// analyzer — the whole value becomes ONE case-preserved term — so the query
/// side must look them up by whole value too (ES semantics: `match` /
/// `multi_match` / query-string clauses on a keyword field are exact
/// whole-value comparisons, because the keyword analyzer is a no-op).
/// Tokenizing the query with the `standard` analyzer here would produce
/// terms (e.g. "claude" from "claude-haiku-4-5") that can never exist in a
/// keyword field's FST — the cause of the multi_match / query_string /
/// simple_query_string 0-hit bugs on keyword-only mappings.
/// Walk a query tree and collect per-field boost multipliers for the
/// flattened memtable text-search path (which sees only query text + a
/// field filter, not the clause structure). Only scoring-relevant branches
/// are walked (`must_not` never contributes to the score). When the same
/// field is boosted by more than one clause the largest boost wins — the
/// flattened path can't score per-clause, and the max preserves the
/// clause's ranking intent.
fn collect_field_boosts(q: &QueryNode, out: &mut HashMap<String, f32>) {
    fn add(out: &mut HashMap<String, f32>, field: &str, b: f32) {
        if (b - 1.0).abs() > f32::EPSILON {
            let e = out.entry(field.to_string()).or_insert(1.0);
            if b > *e {
                *e = b;
            }
        }
    }
    match q {
        QueryNode::Match { field, boost, .. } => {
            add(out, field, boost.unwrap_or(1.0));
        }
        QueryNode::MultiMatch { fields, boost, .. } => {
            let qb = boost.unwrap_or(1.0);
            for spec in fields {
                let (f, fb) = parse_field_boost(spec);
                add(out, f, fb * qb);
            }
        }
        QueryNode::Bool {
            must,
            should,
            filter,
            ..
        } => {
            for sub in must.iter().chain(should.iter()).chain(filter.iter()) {
                collect_field_boosts(sub, out);
            }
        }
        _ => {}
    }
}

fn query_node_to_fts(
    q: &QueryNode,
    text_fields: &[String],
    exact_fields: &std::collections::HashSet<String>,
) -> Option<FtsQuery> {
    match q {
        QueryNode::MatchAll => {
            // Return None; MatchAll is handled separately by reading stored docs.
            None
        }
        QueryNode::MatchNone => None,
        QueryNode::Match {
            field,
            query,
            operator,
            boost,
            ..
        } => {
            // Per-clause boost (ES `{"match": {"f": {"query": …, "boost": N}}}`)
            // must reach the BM25 scorer — dropping it here made boosted and
            // unboosted clauses score identically (test_weighted_bool_boost_ranking).
            let b = boost.unwrap_or(1.0);
            // Keyword-indexed field: single whole-value, case-sensitive term.
            if exact_fields.contains(field.as_str()) {
                return Some(FtsQuery::Term(FtsTerm::boosted(
                    field.as_str(),
                    query.as_str(),
                    b,
                )));
            }
            // Tokenize and produce a bool query over terms. Honour the
            // operator: AND → every token must match; OR → any token.
            let registry = AnalyzerRegistry::default();
            let analyzer = registry.get_analyzer("standard")?;
            let tokens = analyzer.analyze(query);
            if tokens.is_empty() {
                return None;
            }
            if tokens.len() == 1 {
                return Some(FtsQuery::Term(FtsTerm::boosted(
                    field.as_str(),
                    &tokens[0].text,
                    b,
                )));
            }
            let is_and = matches!(operator, xerj_query::ast::BoolOperator::And);
            let mut bool_q = FtsBool::new().boost(b);
            for token in &tokens {
                let term = FtsQuery::Term(FtsTerm::new(field.as_str(), &token.text));
                bool_q = if is_and {
                    bool_q.must(term)
                } else {
                    bool_q.should(term)
                };
            }
            Some(FtsQuery::Bool(Box::new(bool_q)))
        }
        QueryNode::MultiMatch {
            query,
            fields,
            match_type,
            boost,
            ..
        } => {
            let registry = AnalyzerRegistry::default();
            let analyzer = registry.get_analyzer("standard")?;
            let tokens = analyzer.analyze(query);
            // Split boost factors out of field specs (e.g. "title^3" → ("title", 3.0)).
            let field_specs: Vec<(String, f32)> = if fields.is_empty() {
                text_fields.iter().map(|s| (s.clone(), 1.0)).collect()
            } else {
                fields
                    .iter()
                    .map(|s| {
                        let (f, b) = parse_field_boost(s);
                        (f.to_string(), b)
                    })
                    .collect()
            };
            if field_specs.is_empty() {
                return None;
            }
            // Keyword-typed fields match by WHOLE value (their FST holds one
            // case-preserved keyword-analyzer token per value), so an empty
            // standard-token set only kills the projection when no keyword
            // field could still match the raw query string.
            if tokens.is_empty()
                && !field_specs
                    .iter()
                    .any(|(f, _)| exact_fields.contains(f.as_str()))
            {
                return None;
            }
            // One scored clause per field.  A single-token query lowers to a
            // bare Term so that `multi_match(q, [f])` scores IDENTICALLY to
            // `match {f: q}`; multi-token queries lower to an OR-bool over the
            // tokens (per-field score = Σ token BM25) — same as the Match
            // lowering above.  Per-field `^boost` multiplies that field's score.
            //
            // Keyword fields get ONE whole-value term (ES: match/multi_match
            // on a keyword field is exact whole-value equality — standard
            // tokens like "claude" can never match the keyword term
            // "claude-haiku-4-5").
            let mut per_field: Vec<FtsQuery> = Vec::with_capacity(field_specs.len());
            for (field, fb) in &field_specs {
                if exact_fields.contains(field.as_str()) {
                    per_field.push(FtsQuery::Term(FtsTerm::boosted(
                        field.as_str(),
                        query.as_str(),
                        *fb,
                    )));
                } else if tokens.is_empty() {
                    continue;
                } else if tokens.len() == 1 {
                    per_field.push(FtsQuery::Term(FtsTerm::boosted(
                        field.as_str(),
                        &tokens[0].text,
                        *fb,
                    )));
                } else {
                    let mut field_bool = FtsBool::new().boost(*fb);
                    for token in &tokens {
                        field_bool = field_bool
                            .should(FtsQuery::Term(FtsTerm::new(field.as_str(), &token.text)));
                    }
                    per_field.push(FtsQuery::Bool(Box::new(field_bool)));
                }
            }
            if per_field.is_empty() {
                return None;
            }
            // Combine per-field scores according to the multi_match type:
            //   best_fields (ES default) → dis_max: score = MAX field score.
            //   most_fields / everything else currently modeled → bool.should:
            //     score = Σ field scores (pre-existing behaviour, now non-zero).
            let combined = if per_field.len() == 1 {
                per_field.pop().unwrap()
            } else if matches!(match_type, xerj_query::ast::MultiMatchType::BestFields) {
                FtsQuery::DisMax(Box::new(FtsDisMax::new(per_field)))
            } else {
                let mut bool_q = FtsBool::new();
                for clause in per_field {
                    bool_q = bool_q.should(clause);
                }
                FtsQuery::Bool(Box::new(bool_q))
            };
            // Apply the outer `multi_match.boost` (if any) via a wrapping bool.
            let outer = boost.unwrap_or(1.0);
            if (outer - 1.0).abs() > f32::EPSILON {
                Some(FtsQuery::Bool(Box::new(
                    FtsBool::new().should(combined).boost(outer),
                )))
            } else {
                Some(combined)
            }
        }
        QueryNode::Term { .. } => {
            // Term queries are routed through stored-doc scanning
            // (json_values_equal) because the FTS index applies the text
            // analyzer at index time — so `term {method: "GET"}` against a
            // keyword/int/date field would miss (stopword for "GET", wrong
            // type for integer).  Doc scanning handles all field types
            // correctly.  Trade-off: slower on huge segments, but segments
            // are merged aggressively and most term queries are highly
            // selective.
            None
        }
        QueryNode::Bool {
            must,
            should,
            must_not,
            filter,
            ..
        } => {
            let mut bool_q = FtsBool::new();
            let mut projected_any = false;
            // CRITICAL: if a `must` child can't be projected to FTS we
            // CANNOT lift just the projectable subset — the bool would
            // become more permissive than the original query.  Return
            // None in that case so the caller falls back to stored-scan,
            // which handles all child shapes correctly.
            //
            // `filter` children constrain the hit set exactly like `must`
            // (they differ only in scoring). They MUST NOT be dropped —
            // with keyword-field projections now producing real matches, a
            // dropped filter would silently overcount. Project them as
            // `must`, or fall back to the stored scan when one can't
            // project (Term/Range/etc. all project to None, so classic
            // filters keep taking the doc-scan path as before).
            for sub in must.iter().chain(filter.iter()) {
                if let Some(fq) = query_node_to_fts(sub, text_fields, exact_fields) {
                    bool_q = bool_q.must(fq);
                    projected_any = true;
                } else {
                    return None;
                }
            }
            for sub in should {
                if let Some(fq) = query_node_to_fts(sub, text_fields, exact_fields) {
                    bool_q = bool_q.should(fq);
                    projected_any = true;
                } else {
                    // A `should` clause that can't be projected to FTS (e.g.
                    // MatchPhrase over indexed phrases) must NOT be silently
                    // dropped — dropping it makes the bool LESS permissive and
                    // loses docs that match only via that clause. Fall back to
                    // the stored-doc scan, which handles every child shape.
                    return None;
                }
            }
            // `must_not` children that don't project are similar: dropping
            // a must_not relaxes the filter, which is wrong.
            for sub in must_not {
                if let Some(fq) = query_node_to_fts(sub, text_fields, exact_fields) {
                    bool_q = bool_q.must_not(fq);
                    projected_any = true;
                } else {
                    return None;
                }
            }
            if !projected_any {
                return None;
            }
            Some(FtsQuery::Bool(Box::new(bool_q)))
        }
        QueryNode::QueryString {
            query,
            default_field,
            boost,
            ..
        } => {
            // Honor top-level `query_string.boost` like the Match arm above.
            let b = boost.unwrap_or(1.0);
            let field = default_field
                .as_deref()
                .or_else(|| text_fields.first().map(|s| s.as_str()))
                .unwrap_or("_all");
            // Keyword default field: whole-value term (keyword analyzer).
            if exact_fields.contains(field) {
                return Some(FtsQuery::Term(FtsTerm::boosted(field, query.as_str(), b)));
            }
            let registry = AnalyzerRegistry::default();
            let analyzer = registry.get_analyzer("standard")?;
            let tokens = analyzer.analyze(query);
            if tokens.is_empty() {
                return None;
            }
            let mut bool_q = FtsBool::new().boost(b);
            for token in &tokens {
                bool_q = bool_q.should(FtsQuery::Term(FtsTerm::new(field, &token.text)));
            }
            Some(FtsQuery::Bool(Box::new(bool_q)))
        }
        QueryNode::MatchPhrase {
            field,
            query,
            slop,
            analyzer,
            boost,
        } => {
            // match_phrase on a KEYWORD field: the field is not tokenized, so
            // the query analyzes (keyword analyzer) to a single whole-value
            // token — i.e. exact whole-value equality, identical to `match` on
            // a keyword field (which projects to Term above and is a proven,
            // tested path).  Route through the postings/term index instead of
            // the per-doc brute scan: `is_doc_scan_query` keeps MatchPhrase in
            // the doc-scan set, but returning `Some` here makes `needs_fts`
            // true so the FTS path runs FIRST and sets `fts_handled`, skipping
            // the O(N·field_len) stored scan that made keyword match_phrase
            // multi-second (5.6 s / 100k).
            let b = boost.unwrap_or(1.0);
            if exact_fields.contains(field.as_str()) {
                return Some(FtsQuery::Term(FtsTerm::boosted(
                    field.as_str(),
                    query.as_str(),
                    b,
                )));
            }
            // TEXT field, slop 0, default (standard) analyzer: the FTS segment
            // stores term POSITIONS for analyzed text (store_positions=true), so
            // route to a positional phrase intersection bounded to candidate
            // docs (`FtsQuery::Phrase`) instead of the O(N·field_len) stored
            // scan.  The query is analyzed with the SAME standard analyzer the
            // field was indexed with (tokenize + lowercase, no stemming), so the
            // phrase terms line up byte-for-byte with the indexed terms — and
            // lowercasing makes it case-insensitive exactly like ES's analyzed
            // phrase.  slop>0 and non-standard analyzers keep the stored scan
            // (None): the sloppy/analyzer semantics stay on the proven path.
            if *slop == 0
                && text_fields.iter().any(|f| f == field)
                && matches!(analyzer.as_deref(), None | Some("standard"))
            {
                let registry = AnalyzerRegistry::default();
                let analyzer = registry.get_analyzer("standard")?;
                let tokens = analyzer.analyze(query);
                if tokens.is_empty() {
                    // Empty analyzed phrase — fall back to the stored scan.
                    return None;
                }
                if tokens.len() == 1 {
                    return Some(FtsQuery::Term(FtsTerm::boosted(
                        field.as_str(),
                        &tokens[0].text,
                        b,
                    )));
                }
                return Some(FtsQuery::Phrase(xerj_fts::search::PhraseQuery {
                    field: field.clone(),
                    terms: tokens.iter().map(|t| t.text.clone()).collect(),
                    slop: 0,
                    boost: b,
                }));
            }
            None
        }
        QueryNode::MatchPhrasePrefix {
            field,
            query,
            max_expansions,
        } => {
            // match_phrase_prefix on a KEYWORD field: single whole-value token
            // whose last (only) term is a prefix → a prefix query over the
            // keyword value (ES default max_expansions 50).  Same O(N)
            // brute-scan cliff as match_phrase (5.0 s / 100k) until routed
            // through the FST term dictionary.
            if exact_fields.contains(field.as_str()) {
                return Some(FtsQuery::Prefix(xerj_fts::search::PrefixQuery {
                    field: field.clone(),
                    prefix: query.clone(),
                    boost: 1.0,
                    max_expansions: *max_expansions as usize,
                    constant_score: false,
                }));
            }
            // TEXT field: analyze the query with the standard analyzer (the
            // indexing analyzer) — the leading tokens form an ordered phrase and
            // the LAST token is a prefix expanded against the field's term
            // dictionary (bounded by `max_expansions`).  Positional, bounded to
            // candidate docs, instead of the O(N) stored scan.  The analyzer
            // lowercases every token, so the head phrase and the prefix are
            // case-insensitive exactly like ES (which analyzes the input).
            if text_fields.iter().any(|f| f == field) {
                let registry = AnalyzerRegistry::default();
                let analyzer = registry.get_analyzer("standard")?;
                let tokens = analyzer.analyze(query);
                if tokens.is_empty() {
                    return None;
                }
                return Some(FtsQuery::PhrasePrefix(xerj_fts::search::PhrasePrefixQuery {
                    field: field.clone(),
                    terms: tokens.iter().map(|t| t.text.clone()).collect(),
                    max_expansions: *max_expansions as usize,
                    boost: 1.0,
                }));
            }
            None
        }
        QueryNode::Prefix { field, value, boost } => {
            // `prefix` on a KEYWORD field: whole-value, case-sensitive prefix
            // match over the keyword FST term dictionary — byte-identical to ES
            // `prefix` on a keyword field (default case-sensitive, matches the
            // whole indexed value).  Mirrors the match_phrase_prefix keyword arm
            // above, routing through the postings/FST index instead of the O(N)
            // per-doc stored scan that made keyword `prefix` ~160 ms / 100k.  If
            // the keyword sidecar is absent for a segment, `fts_handled` stays
            // false and the query falls back to the (correct) brute scan.
            //
            // Unlike match_phrase_prefix, an ES `prefix` query has NO
            // `max_expansions` cap — it matches EVERY term sharing the prefix —
            // so we expand without bound (`usize::MAX`) to keep `hits.total`
            // exact.  TEXT fields keep the doc-scan (None): their prefix runs
            // against analyzed tokens, which the FST-whole-value route can't
            // reproduce.
            if exact_fields.contains(field.as_str()) {
                return Some(FtsQuery::Prefix(xerj_fts::search::PrefixQuery {
                    field: field.clone(),
                    prefix: value.clone(),
                    boost: boost.unwrap_or(1.0),
                    max_expansions: usize::MAX,
                    constant_score: false,
                }));
            }
            // TEXT field: expand the prefix against the analyzed term dictionary
            // (each indexed term is a lowercased token).  ES does NOT analyze
            // the prefix pattern — it is matched CASE-SENSITIVELY against the
            // already-lowercased terms, so an uppercase pattern matches nothing.
            // `expand_prefix` is a raw `starts_with` (no folding), reproducing
            // that exactly.  No `max_expansions` cap (ES `prefix` matches every
            // term sharing the prefix) so `hits.total` stays exact.  ES rewrites
            // `prefix` to a `constant_score` query, so every match scores `boost`
            // (`constant_score: true`).
            if text_fields.iter().any(|f| f == field) {
                return Some(FtsQuery::Prefix(xerj_fts::search::PrefixQuery {
                    field: field.clone(),
                    prefix: value.clone(),
                    boost: boost.unwrap_or(1.0),
                    max_expansions: usize::MAX,
                    constant_score: true,
                }));
            }
            None
        }
        QueryNode::Wildcard { field, value, boost } => {
            // `wildcard` on a KEYWORD field: expand the keyword FST term
            // dictionary to every term matching the pattern (`*`=0+ chars,
            // `?`=1 char) and score each as a term.  The searcher's expansion
            // predicate is byte-identical to `doc_matches_query`'s wildcard arm
            // (case-insensitive, whole-value OR sub-token), so the hit set is
            // exactly the doc-scan's — only sourced from the term dictionary
            // (≪ docs) instead of a per-doc O(N) scan (~150 ms/100k → ~1 ms).
            //
            // Case-insensitivity is REQUIRED, not a shortcut: XERJ's parser
            // drops `case_insensitive` and rewrites `term{case_insensitive:true}`
            // to a Wildcard, both relying on the matcher folding case — so a
            // case-sensitive FST route would break those (passing) paths.
            // TEXT fields keep the doc-scan (None): analyzed-token semantics.
            if exact_fields.contains(field.as_str()) {
                return Some(FtsQuery::Wildcard(xerj_fts::search::WildcardQuery {
                    field: field.clone(),
                    pattern: value.clone(),
                    boost: boost.unwrap_or(1.0),
                    case_insensitive: true,
                    constant_score: false,
                }));
            }
            // TEXT field: expand the pattern against the analyzed term
            // dictionary CASE-SENSITIVELY.  Indexed text terms are lowercased by
            // the standard analyzer; ES does not analyze a RAW `wildcard`
            // pattern, so it is matched literally against those terms (an
            // uppercase pattern matches nothing).  `case_insensitive: false`
            // gives exactly that.  (query_string lowercases its wildcard terms
            // at parse time, so `q=field:BA*` still matches — that lowering is
            // done in the parser, not here.)  ES rewrites `wildcard` to a
            // `constant_score` query, so every match scores `boost`.
            if text_fields.iter().any(|f| f == field) {
                return Some(FtsQuery::Wildcard(xerj_fts::search::WildcardQuery {
                    field: field.clone(),
                    pattern: value.clone(),
                    boost: boost.unwrap_or(1.0),
                    case_insensitive: false,
                    constant_score: true,
                }));
            }
            None
        }
        QueryNode::Fuzzy {
            field,
            value,
            fuzziness,
        } => {
            // `fuzzy` on a KEYWORD field: expand the keyword FST term dictionary
            // to every term within `max_edits` Damerau-Levenshtein distance and
            // score each as a term.  `max_edits` is resolved HERE (AUTO depends
            // on the query term length) so the searcher stays type-agnostic.
            // The searcher's distance predicate is byte-identical to
            // `doc_matches_query`'s fuzzy arm (case-insensitive, transpositions
            // on, whole-value OR sub-token) — same hit set as the doc scan,
            // sourced from the term dictionary (~260 ms/100k → ~1 ms).  TEXT
            // fields keep the doc-scan (None).
            if exact_fields.contains(field.as_str()) {
                let max_edits = match fuzziness {
                    Fuzziness::Auto => auto_fuzziness(value),
                    Fuzziness::Fixed(n) => *n as usize,
                };
                return Some(FtsQuery::Fuzzy(xerj_fts::search::FuzzyQuery {
                    field: field.clone(),
                    value: value.clone(),
                    max_edits,
                    boost: 1.0,
                    case_insensitive: true,
                }));
            }
            // TEXT field: expand the term dictionary within `max_edits`
            // Damerau-Levenshtein distance of the RAW query value.  ES does not
            // lowercase the fuzzy query term, and the indexed text terms are
            // lowercased — so distance is measured case-sensitively against the
            // lowercased terms (`case_insensitive: false`), matching ES (an
            // uppercase query term is `len` edits away and matches nothing).
            if text_fields.iter().any(|f| f == field) {
                let max_edits = match fuzziness {
                    Fuzziness::Auto => auto_fuzziness(value),
                    Fuzziness::Fixed(n) => *n as usize,
                };
                return Some(FtsQuery::Fuzzy(xerj_fts::search::FuzzyQuery {
                    field: field.clone(),
                    value: value.clone(),
                    max_edits,
                    boost: 1.0,
                    case_insensitive: false,
                }));
            }
            None
        }
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// FunctionScore helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Apply a modifier to a numeric value.
fn apply_modifier(value: f64, modifier: Modifier) -> f64 {
    match modifier {
        Modifier::None => value,
        Modifier::Log => value.log10().max(0.0),
        Modifier::Log1p => (value + 1.0).log10().max(0.0),
        Modifier::Log2p => (value + 2.0).log10().max(0.0),
        Modifier::Ln => value.ln().max(0.0),
        Modifier::Ln1p => (value + 1.0).ln().max(0.0),
        Modifier::Ln2p => (value + 2.0).ln().max(0.0),
        Modifier::Square => value * value,
        Modifier::Sqrt => value.sqrt(),
        Modifier::Reciprocal => {
            if value == 0.0 {
                0.0
            } else {
                1.0 / value
            }
        }
    }
}

/// Compute the score contribution from a field_value_factor function.
fn compute_field_value_factor(fvf: &FieldValueFactor, source: &Value) -> f32 {
    let raw_value = get_field_value(source, &fvf.field)
        .and_then(|v| match &v {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.parse::<f64>().ok(),
            _ => None,
        })
        .or(fvf.missing)
        .unwrap_or(0.0);

    // ES throws `illegal_argument_exception` when the combination of
    // value + modifier would produce a non-finite / NaN score (for
    // example ln1p on qty=-1 → ln(0) = -∞). Propagate NaN here so the
    // caller can surface the error with the exact ES reason string.
    let domain_violation = match fvf.modifier {
        Modifier::Ln | Modifier::Log => raw_value <= 0.0,
        Modifier::Ln1p | Modifier::Log1p => raw_value <= -1.0,
        Modifier::Ln2p | Modifier::Log2p => raw_value <= -2.0,
        Modifier::Sqrt => raw_value < 0.0,
        _ => false,
    };
    if domain_violation {
        return f32::NAN;
    }

    let modified = apply_modifier(raw_value, fvf.modifier);
    (modified * fvf.factor as f64) as f32
}

/// Default chunk size (characters) for auto-embed-on-ingest chunking.
/// `EmbeddingConfig` has no per-field chunk parameter yet, so this is a
/// sensible fixed default (roughly a paragraph).
const SEMANTIC_CHUNK_SIZE: usize = 512;
/// Default overlap (characters) between consecutive chunks.
const SEMANTIC_CHUNK_OVERLAP: usize = 64;

/// The chunker used by the auto-embed-on-ingest path.
fn semantic_chunker() -> xerj_ai::TextChunker {
    xerj_ai::TextChunker::new(SEMANTIC_CHUNK_SIZE, SEMANTIC_CHUNK_OVERLAP)
}

/// L2-normalize each input vector, mean-pool across all of them, then
/// re-normalize the pooled result. Produces a unit-norm vector of the same
/// dimensionality as the inputs (empty input => empty vec). Vectors whose
/// length differs from the first are skipped.
fn mean_pool_normalize(vectors: &[Vec<f32>]) -> Vec<f32> {
    let dims = match vectors.first() {
        Some(v) => v.len(),
        None => return Vec::new(),
    };
    let mut acc = vec![0.0f32; dims];
    let mut used = 0usize;
    for v in vectors {
        if v.len() != dims {
            continue;
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for (a, x) in acc.iter_mut().zip(v.iter()) {
                *a += x / norm;
            }
            used += 1;
        }
    }
    if used > 0 {
        let inv = 1.0 / used as f32;
        for a in acc.iter_mut() {
            *a *= inv;
        }
    }
    // Re-normalize the pooled vector to unit length.
    let norm = acc.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for a in acc.iter_mut() {
            *a /= norm;
        }
    }
    acc
}

/// Chunk `text` and embed each overlapping chunk with the built-in *local*
/// embedder, returning one unit-norm vector per passage. Short text
/// (<= chunk_size) produces a single chunk, so the result is
/// `vec![local_embed(text, dims)]`. This is the per-chunk primitive used by
/// the auto-embed-on-ingest path to persist passage vectors for max-sim
/// semantic scoring.
fn local_chunk_vectors(text: &str, dims: usize) -> Vec<Vec<f32>> {
    let chunks = semantic_chunker().chunk(text, None);
    if chunks.len() <= 1 {
        return vec![xerj_ai::local::local_embed(text, dims)];
    }
    chunks
        .iter()
        .map(|c| xerj_ai::local::local_embed(&c.text, dims))
        .collect()
}

/// Chunk `text` and mean-pool the per-chunk *local* embeddings into a single
/// unit-norm vector of length `dims`. Short text (<= chunk_size) produces a
/// single chunk, so the result is exactly `local_embed(text, dims)` — a no-op
/// relative to the pre-chunking behavior. Retained as the reference pooled
/// path the per-chunk tests assert against (the ingest path now stores the
/// per-chunk vectors directly via [`local_chunk_vectors`]).
#[cfg(test)]
fn local_chunk_embed(text: &str, dims: usize) -> Vec<f32> {
    let vecs = local_chunk_vectors(text, dims);
    if vecs.len() <= 1 {
        return vecs.into_iter().next().unwrap_or_default();
    }
    mean_pool_normalize(&vecs)
}

/// Compute the score contribution from a `rank_feature` function over a
/// numeric feature field. Implements the four ES functions exactly:
///
/// * saturation: `v / (v + pivot)`  (pivot defaults to 1.0 when omitted)
/// * log:        `log10(scaling_factor + v)`
/// * sigmoid:    `v^exponent / (v^exponent + pivot^exponent)`
/// * linear:     `v`
///
/// A missing or non-numeric field contributes 0 (the surrounding Exists
/// query already filters docs lacking the field).
fn compute_rank_feature_score(rf: &xerj_query::ast::RankFeature, source: &Value) -> f32 {
    use xerj_query::ast::RankFeatureFn;
    let Some(v) = get_field_value(source, &rf.field).and_then(|x| x.as_f64()) else {
        return 0.0;
    };
    let s = match &rf.function {
        RankFeatureFn::Saturation { pivot } => {
            let p = pivot.unwrap_or(1.0);
            if v + p == 0.0 {
                0.0
            } else {
                v / (v + p)
            }
        }
        RankFeatureFn::Log { scaling_factor } => (scaling_factor + v).log10(),
        RankFeatureFn::Sigmoid { pivot, exponent } => {
            let ve = v.powf(*exponent);
            let pe = pivot.powf(*exponent);
            if ve + pe == 0.0 {
                0.0
            } else {
                ve / (ve + pe)
            }
        }
        RankFeatureFn::Linear => v,
    };
    s as f32
}

/// Compute the score contribution from a `distance_feature` function.
/// Formula: `pivot / (pivot + distance(origin, field_value))`.
///
/// For a date or date_nanos field the distance is the absolute time
/// delta in the pivot's unit (e.g. `1h` → milliseconds; `100nanos` →
/// nanoseconds). For a `geo_point` field the distance is haversine
/// km (converted to the pivot's length unit).
fn compute_distance_feature_score(df: &xerj_query::ast::DistanceFeature, source: &Value) -> f32 {
    let Some(field_val) = get_field_value(source, &df.field) else {
        return 0.0;
    };

    // ── GEO branch ────────────────────────────────────────────────
    // A geo_point value is an `[lon, lat]` array or an object with
    // `lat`/`lon`. Origin is matched in the same shape.
    let as_lonlat = |v: &Value| -> Option<(f64, f64)> {
        if let Value::Array(arr) = v {
            if arr.len() >= 2 {
                let lon = arr[0].as_f64()?;
                let lat = arr[1].as_f64()?;
                return Some((lon, lat));
            }
        }
        if let Value::Object(o) = v {
            let lat = o.get("lat").and_then(Value::as_f64)?;
            let lon = o.get("lon").and_then(Value::as_f64)?;
            return Some((lon, lat));
        }
        None
    };
    if let (Some((o_lon, o_lat)), Some((d_lon, d_lat))) =
        (as_lonlat(&df.origin), as_lonlat(&field_val))
    {
        let dist_km = haversine_distance(o_lat, o_lon, d_lat, d_lon);
        let (pivot_val, unit_km_factor) = parse_distance_pivot(&df.pivot);
        let dist_in_unit = dist_km / unit_km_factor;
        return (pivot_val / (pivot_val + dist_in_unit)) as f32;
    }

    // ── DATE branch ───────────────────────────────────────────────
    // Parse both origin and field value as epoch-nanos so pivots like
    // `100nanos` behave correctly against `date_nanos` fields.
    let (pivot_val, unit_ns_factor) = parse_duration_pivot(&df.pivot);
    let origin_ns = df.origin.as_str().and_then(parse_date_to_epoch_ns);
    let doc_ns = field_val.as_str().and_then(parse_date_to_epoch_ns);
    if let (Some(o), Some(d)) = (origin_ns, doc_ns) {
        let delta_ns = (o - d).unsigned_abs() as f64;
        let delta_in_unit = delta_ns / unit_ns_factor;
        return (pivot_val / (pivot_val + delta_in_unit)) as f32;
    }

    0.0
}

/// Parse an ES duration pivot like `1h`, `100nanos`, `30s`.  Returns
/// `(pivot_numeric_in_unit, ns_per_unit)`.
fn parse_duration_pivot(p: &str) -> (f64, f64) {
    let trim = p.trim();
    // Longest-suffix match first so `nanos` doesn't collide with `s`.
    let table: &[(&str, f64)] = &[
        ("nanos", 1.0),
        ("micros", 1_000.0),
        ("ms", 1_000_000.0),
        ("s", 1_000_000_000.0),
        ("m", 60.0 * 1_000_000_000.0),
        ("h", 3_600.0 * 1_000_000_000.0),
        ("d", 86_400.0 * 1_000_000_000.0),
    ];
    for (suf, mult) in table {
        if let Some(rest) = trim.strip_suffix(suf) {
            if let Ok(n) = rest.trim().parse::<f64>() {
                return (n, *mult);
            }
        }
    }
    // Fallback: treat as milliseconds.
    (trim.parse::<f64>().unwrap_or(1.0), 1_000_000.0)
}

/// Parse an ES distance pivot like `1km`, `500m`, `1mi`. Returns
/// `(pivot_numeric_in_unit, km_per_unit)`.
fn parse_distance_pivot(p: &str) -> (f64, f64) {
    let trim = p.trim();
    let table: &[(&str, f64)] = &[
        ("km", 1.0),
        ("mi", 1.609_344),
        ("yd", 0.000_914_4),
        ("ft", 0.000_304_8),
        ("m", 0.001),
        ("cm", 0.000_01),
        ("mm", 0.000_001),
    ];
    for (suf, mult) in table {
        if let Some(rest) = trim.strip_suffix(suf) {
            if let Ok(n) = rest.trim().parse::<f64>() {
                return (n, *mult);
            }
        }
    }
    (trim.parse::<f64>().unwrap_or(1.0), 1.0)
}

/// Parse an ISO-8601 date (millisecond or nanosecond precision) to
/// epoch nanoseconds. Also accepts `now` and simple `now±<n><unit>`
/// date math — the `now` anchor is the wall-clock time at call site.
fn parse_date_to_epoch_ns(s: &str) -> Option<i128> {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix("now") {
        let base = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0) as i128;
        if rest.is_empty() || rest == "/" {
            return Some(base);
        }
        // `now-1d`, `now+3h`, `now-30s/d` etc. Take the numeric delta,
        // ignore any `/<unit>` rounding suffix.
        let (sign, rest) = if let Some(r) = rest.strip_prefix('+') {
            (1i128, r)
        } else if let Some(r) = rest.strip_prefix('-') {
            (-1i128, r)
        } else {
            return Some(base);
        };
        let delta_end = rest.find('/').unwrap_or(rest.len());
        let delta_expr = &rest[..delta_end];
        let (num_part, unit_part): (String, String) = delta_expr
            .chars()
            .partition(|c| c.is_ascii_digit() || *c == '.');
        if let Ok(n) = num_part.parse::<f64>() {
            let ns_per_unit: f64 = match unit_part.as_str() {
                "ms" => 1_000_000.0,
                "s" => 1_000_000_000.0,
                "m" => 60.0 * 1_000_000_000.0,
                "h" => 3_600.0 * 1_000_000_000.0,
                "d" => 86_400.0 * 1_000_000_000.0,
                "w" => 7.0 * 86_400.0 * 1_000_000_000.0,
                "M" => 30.0 * 86_400.0 * 1_000_000_000.0,
                "y" => 365.0 * 86_400.0 * 1_000_000_000.0,
                _ => 0.0,
            };
            return Some(base + sign * (n * ns_per_unit) as i128);
        }
        return Some(base);
    }
    // date_nanos: try RFC3339 first so `.123456789Z` precision is kept.
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        let secs = dt.timestamp() as i128;
        let ns = dt.timestamp_subsec_nanos() as i128;
        return Some(secs * 1_000_000_000 + ns);
    }
    // Naive date-time (no tz): assume UTC.
    if let Ok(dt) =
        chrono::NaiveDateTime::parse_from_str(trimmed.trim_end_matches('Z'), "%Y-%m-%dT%H:%M:%S%.f")
    {
        let secs = dt.and_utc().timestamp() as i128;
        let ns = dt.and_utc().timestamp_subsec_nanos() as i128;
        return Some(secs * 1_000_000_000 + ns);
    }
    if let Ok(dt) =
        chrono::NaiveDateTime::parse_from_str(trimmed.trim_end_matches('Z'), "%Y-%m-%dT%H:%M:%S")
    {
        let secs = dt.and_utc().timestamp() as i128;
        return Some(secs * 1_000_000_000);
    }
    // Date-only forms (no time), accepting the common mapping-format
    // shapes used by ES YAML tests: `yyyy-MM-dd`, `yyyy/MM/dd`.
    for fmt in &["%Y-%m-%d", "%Y/%m/%d"] {
        if let Ok(d) = chrono::NaiveDate::parse_from_str(trimmed, fmt) {
            if let Some(dt) = d.and_hms_opt(0, 0, 0) {
                let secs = dt.and_utc().timestamp() as i128;
                return Some(secs * 1_000_000_000);
            }
        }
    }
    None
}

/// Compute a deterministic random score for a document.
fn compute_random_score(doc_id: &str, rs: &RandomScore) -> f32 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    doc_id.hash(&mut hasher);
    if let Some(seed) = rs.seed {
        seed.hash(&mut hasher);
    }
    // Normalise to [0, 1).
    (hasher.finish() as f32) / (u64::MAX as f32)
}

/// Apply all score functions to a document and combine them according to score_mode.
fn apply_function_score(
    doc_id: &str,
    source: &Value,
    functions: &[ScoreFunction],
    score_mode: ScoreMode,
    query_score: f32,
) -> f32 {
    if functions.is_empty() {
        return 1.0;
    }

    let mut fn_scores: Vec<f32> = Vec::new();

    for func in functions {
        // Check filter: if present, doc must match.
        if let Some(filter) = &func.filter {
            if !doc_matches_query(filter, source) {
                continue;
            }
        }

        // Compute this function's contribution.
        let mut score: f32 = func.weight.unwrap_or(1.0);

        if let Some(fvf) = &func.field_value_factor {
            let fvf_score = compute_field_value_factor(fvf, source);
            score *= fvf_score;
        }

        if let Some(rs) = &func.random_score {
            let rs_score = compute_random_score(doc_id, rs);
            score *= rs_score;
        }

        if let Some(ss) = func.script_score {
            // Numeric-literal script_score (our parsed subset). A numeric
            // script replaces the function's score entirely — matching
            // ES semantics where `script_score` returns the new score
            // rather than multiplying.
            score = ss;
        }

        // Painless script_score: evaluate the source against the doc.
        if let Some(src) = &func.script_source {
            let empty = serde_json::Value::Object(serde_json::Map::new());
            let params = func.script_params.as_ref().unwrap_or(&empty);
            let ctx = crate::painless::PainlessCtx::new(source, params, query_score);
            score = match crate::painless::eval_painless(src, &ctx) {
                Ok(v) => v.as_f64().unwrap_or(0.0) as f32,
                Err(_) => 0.0,
            };
        }

        if let Some(df) = &func.distance_feature {
            // ES distance_feature: pivot / (pivot + distance(origin, field)).
            // Docs closer to `origin` get scores closer to 1.0; docs far
            // away approach 0. For missing fields the function contributes
            // 0 (ES excludes them from the score but the surrounding
            // Exists query already filters them out).
            let ds = compute_distance_feature_score(df, source);
            score = ds;
        }

        if let Some(rf) = &func.rank_feature {
            // rank_feature emits its own score; the query `boost` (carried in
            // `weight`, default 1.0) multiplies it, matching ES.
            score *= compute_rank_feature_score(rf, source);
        }

        fn_scores.push(score);
    }

    if fn_scores.is_empty() {
        return 1.0;
    }

    // Combine function scores using score_mode.
    match score_mode {
        ScoreMode::Multiply => fn_scores.iter().product(),
        ScoreMode::Sum => fn_scores.iter().sum(),
        ScoreMode::Avg => fn_scores.iter().sum::<f32>() / fn_scores.len() as f32,
        ScoreMode::First => fn_scores[0],
        ScoreMode::Max => fn_scores.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
        ScoreMode::Min => fn_scores.iter().cloned().fold(f32::INFINITY, f32::min),
    }
}

/// Combine a query score with a function score according to boost_mode.
fn combine_scores(query_score: f32, fn_score: f32, boost_mode: BoostMode) -> f32 {
    match boost_mode {
        BoostMode::Multiply => query_score * fn_score,
        BoostMode::Replace => fn_score,
        BoostMode::Sum => query_score + fn_score,
        BoostMode::Avg => (query_score + fn_score) / 2.0,
        BoostMode::Max => query_score.max(fn_score),
        BoostMode::Min => query_score.min(fn_score),
    }
}

// ── Analyzer registry builder ─────────────────────────────────────────────────

/// Build an `AnalyzerRegistry` pre-populated with built-in analyzers and any
/// custom analyzers declared in the index `settings` JSON blob.
///
/// The settings blob uses the ES-compatible `settings.analysis` structure:
///
/// ```json
/// {
///   "settings": {
///     "analysis": {
///       "analyzer": { ... },
///       "filter":   { ... },
///       "tokenizer": { ... }
///     }
///   }
/// }
/// ```
fn build_registry_from_settings(settings: &Value) -> AnalyzerRegistry {
    let mut registry = AnalyzerRegistry::default();

    // Settings may be stored as the full settings envelope or as the inner
    // analysis block itself.  Try "/settings" first, fall back to the value
    // as-is so that both shapes work.
    let analysis_root = settings.pointer("/settings").unwrap_or(settings);

    registry.apply_settings(analysis_root);
    registry
}

#[cfg(test)]
mod fts_projection_tests {
    use super::*;

    fn kw(names: &[&str]) -> std::collections::HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// Text-field behavior is UNCHANGED: a multi-token match query on a
    /// text field still projects to a standard-analyzed (tokenized +
    /// lowercased) OR-bool of per-token terms.
    #[test]
    fn match_on_text_field_still_tokenizes() {
        let q = QueryNode::Match {
            field: "body".into(),
            query: "Quick Fox".into(),
            operator: xerj_query::ast::BoolOperator::Or,
            analyzer: None,
            boost: None,
            minimum_should_match: None,
        };
        let text_fields = vec!["body".to_string()];
        let fq = query_node_to_fts(&q, &text_fields, &kw(&["status"])).expect("projects");
        match fq {
            FtsQuery::Bool(b) => {
                assert_eq!(b.should.len(), 2, "two analyzed tokens");
                let terms: Vec<&str> = b
                    .should
                    .iter()
                    .map(|s| match s {
                        FtsQuery::Term(t) => t.term.as_str(),
                        other => panic!("expected term, got {:?}", other),
                    })
                    .collect();
                assert_eq!(terms, vec!["quick", "fox"], "lowercased standard tokens");
            }
            other => panic!("expected bool, got {:?}", other),
        }
    }

    /// A single-token text match still projects to a single lowercased term.
    #[test]
    fn match_on_text_field_single_token() {
        let q = QueryNode::Match {
            field: "body".into(),
            query: "Fox".into(),
            operator: xerj_query::ast::BoolOperator::Or,
            analyzer: None,
            boost: None,
            minimum_should_match: None,
        };
        let fq = query_node_to_fts(&q, &["body".to_string()], &kw(&[])).expect("projects");
        match fq {
            FtsQuery::Term(t) => {
                assert_eq!(t.field, "body");
                assert_eq!(t.term, "fox");
            }
            other => panic!("expected term, got {:?}", other),
        }
    }

    /// Match on a KEYWORD field projects to ONE whole-value, case-preserved
    /// term — the FST stores keyword values as single keyword-analyzer
    /// tokens, so standard-analyzer tokens could never match.
    #[test]
    fn match_on_keyword_field_is_whole_value() {
        let q = QueryNode::Match {
            field: "model".into(),
            query: "claude-haiku-4-5".into(),
            operator: xerj_query::ast::BoolOperator::Or,
            analyzer: None,
            boost: None,
            minimum_should_match: None,
        };
        let fq = query_node_to_fts(&q, &[], &kw(&["model"])).expect("projects");
        match fq {
            FtsQuery::Term(t) => {
                assert_eq!(t.field, "model");
                assert_eq!(t.term, "claude-haiku-4-5", "whole value, not tokens");
            }
            other => panic!("expected single term, got {:?}", other),
        }
    }

    /// multi_match over keyword fields: one whole-value term per field
    /// (ES: match on keyword requires whole-value equality per field).
    #[test]
    fn multi_match_keyword_fields_whole_value_per_field() {
        let q = QueryNode::MultiMatch {
            fields: vec!["model".into(), "top_doc".into()],
            query: "claude-haiku-4-5".into(),
            match_type: xerj_query::ast::MultiMatchType::BestFields,
            operator: None,
            analyzer: None,
            boost: None,
        };
        let fq = query_node_to_fts(&q, &[], &kw(&["model", "top_doc"])).expect("projects");
        match fq {
            // best_fields → dis_max over the per-field clauses.
            FtsQuery::DisMax(d) => {
                assert_eq!(d.queries.len(), 2, "one whole-value term per keyword field");
                for s in &d.queries {
                    match s {
                        FtsQuery::Term(t) => assert_eq!(t.term, "claude-haiku-4-5"),
                        other => panic!("expected term, got {:?}", other),
                    }
                }
            }
            other => panic!("expected dis_max, got {:?}", other),
        }
    }

    /// multi_match over a MIX of text + keyword fields: the text field gets
    /// standard tokens, the keyword field gets the whole value.
    #[test]
    fn multi_match_mixed_text_and_keyword() {
        let q = QueryNode::MultiMatch {
            fields: vec!["title".into(), "model".into()],
            query: "claude-haiku-4-5".into(),
            match_type: xerj_query::ast::MultiMatchType::BestFields,
            operator: None,
            analyzer: None,
            boost: None,
        };
        let fq = query_node_to_fts(&q, &["title".to_string()], &kw(&["model"])).expect("projects");
        match fq {
            // best_fields → dis_max: one clause per field. The text field's
            // clause is an OR-bool over its standard-analyzed tokens; the
            // keyword field's clause is a single whole-value term.
            FtsQuery::DisMax(d) => {
                assert_eq!(d.queries.len(), 2);
                let mut title_tokens = 0;
                let mut model_whole = 0;
                for clause in &d.queries {
                    match clause {
                        FtsQuery::Bool(b) => {
                            for s in &b.should {
                                match s {
                                    FtsQuery::Term(t) if t.field == "title" => {
                                        assert!(
                                            ["claude", "haiku", "4", "5"]
                                                .contains(&t.term.as_str()),
                                            "text field analyzed with standard: {}",
                                            t.term
                                        );
                                        title_tokens += 1;
                                    }
                                    other => panic!("unexpected clause {:?}", other),
                                }
                            }
                        }
                        FtsQuery::Term(t) if t.field == "model" => {
                            assert_eq!(t.term, "claude-haiku-4-5");
                            model_whole += 1;
                        }
                        other => panic!("unexpected clause {:?}", other),
                    }
                }
                assert_eq!(title_tokens, 4);
                assert_eq!(model_whole, 1);
            }
            other => panic!("expected dis_max, got {:?}", other),
        }
    }

    /// match_phrase on a KEYWORD field projects to a single whole-value term
    /// (routed through the postings index instead of the O(N) brute scan that
    /// made keyword match_phrase multi-second). ES: match_phrase on keyword is
    /// exact whole-value equality, identical to `match` on keyword.
    #[test]
    fn match_phrase_keyword_projects_whole_value_term() {
        let q = QueryNode::MatchPhrase {
            field: "top_doc".into(),
            query: "runbook/oncall.md".into(),
            slop: 0,
            analyzer: None,
            boost: None,
        };
        let fq = query_node_to_fts(&q, &[], &kw(&["top_doc"])).expect("keyword phrase projects");
        match fq {
            FtsQuery::Term(t) => {
                assert_eq!(t.field, "top_doc");
                assert_eq!(t.term, "runbook/oncall.md", "whole value, not tokens");
            }
            other => panic!("expected single whole-value term, got {:?}", other),
        }
    }

    /// match_phrase on a TEXT field (slop 0, default analyzer) projects to a
    /// positional `FtsQuery::Phrase` over the STANDARD-analyzed query tokens —
    /// the segment stores term positions for analyzed text, so this routes
    /// through the bounded positional intersection instead of the O(N) stored
    /// scan.
    #[test]
    fn match_phrase_text_field_projects_positional_phrase() {
        let q = QueryNode::MatchPhrase {
            field: "body".into(),
            query: "Quick Brown Fox".into(),
            slop: 0,
            analyzer: None,
            boost: None,
        };
        let fq = query_node_to_fts(&q, &["body".to_string()], &kw(&[])).expect("text phrase projects");
        match fq {
            FtsQuery::Phrase(p) => {
                assert_eq!(p.field, "body");
                // standard analyzer lowercases + tokenizes on word boundaries.
                assert_eq!(p.terms, vec!["quick", "brown", "fox"]);
                assert_eq!(p.slop, 0);
            }
            other => panic!("expected positional phrase, got {:?}", other),
        }
    }

    /// match_phrase with slop > 0 on a TEXT field keeps the stored-doc scan
    /// (the proven sloppy-phrase semantics), so it must NOT project to FTS.
    #[test]
    fn match_phrase_text_slop_falls_back_to_scan() {
        let q = QueryNode::MatchPhrase {
            field: "body".into(),
            query: "quick fox".into(),
            slop: 2,
            analyzer: None,
            boost: None,
        };
        assert!(
            query_node_to_fts(&q, &["body".to_string()], &kw(&[])).is_none(),
            "slop>0 text phrase must fall back to the stored-doc scan"
        );
    }

    /// prefix / wildcard / fuzzy on a TEXT field expand against the analyzed
    /// term dictionary CASE-SENSITIVELY (indexed text terms are lowercased; ES
    /// does not analyze the pattern), and match_phrase_prefix routes to the
    /// positional phrase-prefix executor.
    #[test]
    fn text_multiterm_queries_project_case_sensitively() {
        let tf = ["body".to_string()];
        // prefix
        let q = QueryNode::Prefix {
            field: "body".into(),
            value: "run".into(),
            boost: None,
        };
        match query_node_to_fts(&q, &tf, &kw(&[])).expect("text prefix projects") {
            FtsQuery::Prefix(p) => {
                assert_eq!(p.prefix, "run", "pattern NOT lowercased/analyzed");
                assert_eq!(p.max_expansions, usize::MAX);
            }
            other => panic!("expected prefix, got {:?}", other),
        }
        // wildcard — case-sensitive (case_insensitive=false) for text
        let q = QueryNode::Wildcard {
            field: "body".into(),
            value: "run*".into(),
            boost: None,
        };
        match query_node_to_fts(&q, &tf, &kw(&[])).expect("text wildcard projects") {
            FtsQuery::Wildcard(w) => {
                assert_eq!(w.pattern, "run*");
                assert!(!w.case_insensitive, "text wildcard is case-sensitive");
            }
            other => panic!("expected wildcard, got {:?}", other),
        }
        // fuzzy — case-sensitive for text
        let q = QueryNode::Fuzzy {
            field: "body".into(),
            value: "runbok".into(),
            fuzziness: Fuzziness::Fixed(1),
        };
        match query_node_to_fts(&q, &tf, &kw(&[])).expect("text fuzzy projects") {
            FtsQuery::Fuzzy(f) => {
                assert_eq!(f.value, "runbok");
                assert_eq!(f.max_edits, 1);
                assert!(!f.case_insensitive, "text fuzzy is case-sensitive");
            }
            other => panic!("expected fuzzy, got {:?}", other),
        }
        // match_phrase_prefix — positional phrase-prefix over analyzed tokens
        let q = QueryNode::MatchPhrasePrefix {
            field: "body".into(),
            query: "status ok log".into(),
            max_expansions: 50,
        };
        match query_node_to_fts(&q, &tf, &kw(&[])).expect("text mpp projects") {
            FtsQuery::PhrasePrefix(p) => {
                assert_eq!(p.terms, vec!["status", "ok", "log"]);
                assert_eq!(p.max_expansions, 50);
            }
            other => panic!("expected phrase_prefix, got {:?}", other),
        }
    }

    /// match_phrase_prefix on a KEYWORD field projects to a prefix query over
    /// the whole value (FST term-dictionary route, not the O(N) brute scan).
    #[test]
    fn match_phrase_prefix_keyword_projects_prefix() {
        let q = QueryNode::MatchPhrasePrefix {
            field: "top_doc".into(),
            query: "runbook/on".into(),
            max_expansions: 50,
        };
        let fq = query_node_to_fts(&q, &[], &kw(&["top_doc"])).expect("keyword prefix projects");
        match fq {
            FtsQuery::Prefix(p) => {
                assert_eq!(p.field, "top_doc");
                assert_eq!(p.prefix, "runbook/on");
                assert_eq!(p.max_expansions, 50);
            }
            other => panic!("expected prefix query, got {:?}", other),
        }
    }

    /// A Bool of keyword Match clauses (the parse-time lowering of
    /// `query_string: "model:X AND status:ok"`) projects each clause to a
    /// whole-value term inside an FTS bool.
    #[test]
    fn lowered_query_string_bool_over_keyword_fields() {
        let q = QueryNode::Bool {
            must: vec![
                QueryNode::Match {
                    field: "model".into(),
                    query: "claude-haiku-4-5".into(),
                    operator: xerj_query::ast::BoolOperator::Or,
                    analyzer: None,
                    boost: None,
                    minimum_should_match: None,
                },
                QueryNode::Match {
                    field: "status".into(),
                    query: "ok".into(),
                    operator: xerj_query::ast::BoolOperator::Or,
                    analyzer: None,
                    boost: None,
                    minimum_should_match: None,
                },
            ],
            should: vec![],
            must_not: vec![],
            filter: vec![],
            minimum_should_match: None,
        };
        let fq = query_node_to_fts(&q, &[], &kw(&["model", "status"])).expect("projects");
        match fq {
            FtsQuery::Bool(b) => {
                assert_eq!(b.must.len(), 2);
                match (&b.must[0], &b.must[1]) {
                    (FtsQuery::Term(m), FtsQuery::Term(s)) => {
                        assert_eq!(
                            (m.field.as_str(), m.term.as_str()),
                            ("model", "claude-haiku-4-5")
                        );
                        assert_eq!((s.field.as_str(), s.term.as_str()), ("status", "ok"));
                    }
                    other => panic!("expected two terms, got {:?}", other),
                }
            }
            other => panic!("expected bool, got {:?}", other),
        }
    }

    /// Field collector walks every clause of a projected query.
    #[test]
    fn collect_fields_walks_bool_tree() {
        let q = FtsQuery::Bool(Box::new({
            let mut b = FtsBool::new();
            b = b.must(FtsQuery::Term(FtsTerm::new("model", "x")));
            b = b.should(FtsQuery::Term(FtsTerm::new("status", "ok")));
            b = b.must_not(FtsQuery::Term(FtsTerm::new("model", "y")));
            b
        }));
        let mut out = Vec::new();
        collect_fts_query_fields(&q, &mut out);
        out.sort();
        assert_eq!(out, vec!["model".to_string(), "status".to_string()]);
    }
}

#[cfg(test)]
mod percolate_tests {
    use super::*;
    use serde_json::json;
    use xerj_query::ast::QueryNode;

    /// A stored percolator doc holds a serialized query in `query`; the
    /// percolate query matches that stored doc iff the stored query matches
    /// one of the supplied inline documents.
    #[test]
    fn percolate_term_match_and_miss() {
        let stored = json!({ "query": { "term": { "message": "bonsai" } } });

        let q_hit = QueryNode::Percolate {
            field: "query".into(),
            documents: vec![json!({ "message": "bonsai" })],
        };
        assert!(
            doc_matches_query(&q_hit, &stored),
            "term matches inline doc"
        );

        let q_miss = QueryNode::Percolate {
            field: "query".into(),
            documents: vec![json!({ "message": "cactus" })],
        };
        assert!(
            !doc_matches_query(&q_miss, &stored),
            "term does not match a different value"
        );
    }

    #[test]
    fn percolate_range_and_bool() {
        let stored_range = json!({ "query": { "range": { "price": { "gte": 10, "lte": 20 } } } });
        let q = QueryNode::Percolate {
            field: "query".into(),
            documents: vec![json!({ "price": 15 })],
        };
        assert!(doc_matches_query(&q, &stored_range));
        let q_out = QueryNode::Percolate {
            field: "query".into(),
            documents: vec![json!({ "price": 25 })],
        };
        assert!(!doc_matches_query(&q_out, &stored_range));

        let stored_bool = json!({
            "query": {
                "bool": {
                    "must": [
                        { "term": { "message": "foo" } },
                        { "range": { "price": { "gte": 5 } } }
                    ]
                }
            }
        });
        let q_bool = QueryNode::Percolate {
            field: "query".into(),
            documents: vec![json!({ "message": "foo", "price": 9 })],
        };
        assert!(doc_matches_query(&q_bool, &stored_bool));
        let q_bool_miss = QueryNode::Percolate {
            field: "query".into(),
            documents: vec![json!({ "message": "foo", "price": 1 })],
        };
        assert!(!doc_matches_query(&q_bool_miss, &stored_bool));
    }

    #[test]
    fn percolate_missing_or_unparseable_field_is_no_match() {
        let stored = json!({ "message": "no query here" });
        let q = QueryNode::Percolate {
            field: "query".into(),
            documents: vec![json!({ "message": "anything" })],
        };
        assert!(!doc_matches_query(&q, &stored));

        let stored2 = json!({ "query": "not-an-object" });
        assert!(!doc_matches_query(&q, &stored2));
    }
}

#[cfg(test)]
mod knn_num_candidates_tests {
    use super::*;
    use xerj_query::ast::QueryNode;

    fn nested_knn(num_candidates: Option<usize>) -> QueryNode {
        QueryNode::Nested {
            path: "passages".into(),
            query: Box::new(QueryNode::Knn {
                field: "passages.vec".into(),
                vector: vec![1.0, 0.0, 0.0],
                k: 2,
                num_candidates,
                filter: None,
                boost: None,
            }),
            score_mode: None,
        }
    }

    /// `num_candidates` is peeled out of a nested knn query as its own value.
    #[test]
    fn peel_nested_carries_num_candidates() {
        let q = nested_knn(Some(10));
        let peeled = peel_nested_knn_query(&q).expect("nested knn peels");
        // Tuple: (path, field, vector, k, num_candidates, pre, post).
        assert_eq!(peeled.0, "passages");
        assert_eq!(peeled.3, 2, "k");
        assert_eq!(peeled.4, Some(10), "num_candidates");
    }

    /// When omitted, num_candidates peels to None (the caller then defaults
    /// it to k and clamps to >= k).
    #[test]
    fn peel_nested_num_candidates_none_when_omitted() {
        let q = nested_knn(None);
        let peeled = peel_nested_knn_query(&q).expect("nested knn peels");
        assert_eq!(peeled.4, None);
    }

    /// Top-level knn peels num_candidates too (ignored by exact brute force,
    /// but still surfaced).
    #[test]
    fn peel_top_level_carries_num_candidates() {
        let q = QueryNode::Knn {
            field: "vec".into(),
            vector: vec![0.1, 0.2],
            k: 5,
            num_candidates: Some(64),
            filter: None,
            boost: None,
        };
        let peeled = peel_knn_query(&q).expect("knn peels");
        // Tuple: (field, vector, k, num_candidates, filter).
        assert_eq!(peeled.2, 5, "k");
        assert_eq!(peeled.3, Some(64), "num_candidates");
    }
}

#[cfg(test)]
mod span_containment_tests {
    use super::*;
    use serde_json::json;
    use xerj_query::ast::QueryNode;

    fn span_term(field: &str, value: &str) -> QueryNode {
        QueryNode::SpanTerm {
            field: field.into(),
            value: value.into(),
        }
    }

    /// A `big` span_near over [quick, fox] with slop 3, and a `little`
    /// span_term(brown) that falls between them.
    fn near_quick_fox() -> QueryNode {
        QueryNode::SpanNear {
            clauses: vec![span_term("text", "quick"), span_term("text", "fox")],
            slop: 3,
            in_order: true,
        }
    }

    #[test]
    fn span_within_little_inside_big_matches() {
        // "the quick brown fox jumps": brown (pos 2) is inside the
        // quick..fox span (pos 1..3).
        let doc = json!({ "text": "the quick brown fox jumps" });
        let q = QueryNode::SpanWithin {
            little: Box::new(span_term("text", "brown")),
            big: Box::new(near_quick_fox()),
        };
        assert!(doc_matches_query(&q, &doc));
    }

    #[test]
    fn span_containing_big_contains_little_matches() {
        let doc = json!({ "text": "the quick brown fox jumps" });
        let q = QueryNode::SpanContaining {
            little: Box::new(span_term("text", "brown")),
            big: Box::new(near_quick_fox()),
        };
        assert!(doc_matches_query(&q, &doc));
    }

    #[test]
    fn span_within_non_enclosing_does_not_match() {
        // "jumps" (pos 4) is outside the quick..fox span (pos 1..3).
        let doc = json!({ "text": "the quick brown fox jumps" });
        let q = QueryNode::SpanWithin {
            little: Box::new(span_term("text", "jumps")),
            big: Box::new(near_quick_fox()),
        };
        assert!(!doc_matches_query(&q, &doc));

        // And when the big span does not even form (no "quick" near "fox").
        let doc2 = json!({ "text": "brown only here" });
        let q2 = QueryNode::SpanContaining {
            little: Box::new(span_term("text", "brown")),
            big: Box::new(near_quick_fox()),
        };
        assert!(!doc_matches_query(&q2, &doc2));
    }
}

#[cfg(test)]
mod terms_set_tests {
    use super::*;
    use serde_json::json;
    use xerj_query::ast::{MinShouldMatch, QueryNode};

    /// Build the Bool that `terms_set` lowers to: a `should` of Term clauses
    /// over `codes` with the given `minimum_should_match`.
    fn terms_set_bool(codes: &[&str], msm: MinShouldMatch) -> QueryNode {
        QueryNode::Bool {
            must: vec![],
            must_not: vec![],
            filter: vec![],
            should: codes
                .iter()
                .map(|c| QueryNode::Term {
                    field: "codes".into(),
                    value: json!(c),
                    boost: None,
                })
                .collect(),
            minimum_should_match: Some(msm),
        }
    }

    #[test]
    fn terms_set_field_required_count() {
        let q = terms_set_bool(&["a", "b", "c"], MinShouldMatch::Field("required".into()));

        // 2 of the codes present, required 2 => match.
        assert!(doc_matches_query(
            &q,
            &json!({ "codes": ["a", "b"], "required": 2 })
        ));
        // Only 1 present, required 2 => no match.
        assert!(!doc_matches_query(
            &q,
            &json!({ "codes": ["a"], "required": 2 })
        ));
        // required exceeds the number of matching terms => no match.
        assert!(!doc_matches_query(
            &q,
            &json!({ "codes": ["a", "b", "c"], "required": 4 })
        ));
        // required field missing => unsatisfiable, no match.
        assert!(!doc_matches_query(&q, &json!({ "codes": ["a", "b", "c"] })));
        // required field non-numeric => unsatisfiable, no match.
        assert!(!doc_matches_query(
            &q,
            &json!({ "codes": ["a", "b"], "required": "two" })
        ));
    }

    #[test]
    fn terms_set_script_num_terms() {
        // source `params.num_terms` => require ALL terms to match.
        let q = terms_set_bool(
            &["a", "b", "c"],
            MinShouldMatch::Script {
                source: "params.num_terms".into(),
                params: None,
            },
        );
        // All three present => match.
        assert!(doc_matches_query(&q, &json!({ "codes": ["a", "b", "c"] })));
        // Only two present => no match.
        assert!(!doc_matches_query(&q, &json!({ "codes": ["a", "b"] })));
    }
}

#[cfg(test)]
mod rank_feature_tests {
    use super::*;
    use serde_json::json;
    use xerj_query::ast::{RankFeature, RankFeatureFn};

    fn score(function: RankFeatureFn, v: f64) -> f32 {
        let rf = RankFeature {
            field: "pagerank".into(),
            function,
        };
        compute_rank_feature_score(&rf, &json!({ "pagerank": v }))
    }

    #[test]
    fn saturation_formula_and_ordering() {
        // v / (v + pivot); pivot = 8.
        let f = |v| score(RankFeatureFn::Saturation { pivot: Some(8.0) }, v);
        assert!((f(8.0) - 0.5).abs() < 1e-6, "8/(8+8) = 0.5");
        assert!((f(2.0) - 0.2).abs() < 1e-6, "2/(2+8) = 0.2");
        // Monotonic increasing.
        assert!(f(2.0) < f(8.0) && f(8.0) < f(100.0));
    }

    #[test]
    fn linear_is_identity() {
        assert!((score(RankFeatureFn::Linear, 42.0) - 42.0).abs() < 1e-4);
        assert!((score(RankFeatureFn::Linear, 3.5) - 3.5).abs() < 1e-4);
    }

    #[test]
    fn log_is_monotonic() {
        let f = |v| {
            score(
                RankFeatureFn::Log {
                    scaling_factor: 4.0,
                },
                v,
            )
        };
        // log10(4 + 6) = 1.0.
        assert!((f(6.0) - 1.0).abs() < 1e-6);
        assert!(f(1.0) < f(6.0) && f(6.0) < f(96.0));
    }

    #[test]
    fn missing_field_scores_zero() {
        let rf = RankFeature {
            field: "pagerank".into(),
            function: RankFeatureFn::Linear,
        };
        assert_eq!(compute_rank_feature_score(&rf, &json!({ "other": 1 })), 0.0);
    }
}

#[cfg(test)]
mod chunk_embed_tests {
    use super::*;
    use xerj_ai::local::{local_embed, DEFAULT_DIMS};

    fn l2_norm(v: &[f32]) -> f32 {
        v.iter().map(|x| x * x).sum::<f32>().sqrt()
    }

    /// A long, multi-sentence document is split into more than one chunk and
    /// pooled into a single unit-norm vector of exactly `dims` elements.
    #[test]
    fn long_text_chunks_and_pools_to_unit_vector() {
        let text =
            "The quick brown fox jumps over the lazy dog. Pack my box with five dozen liquor jugs. "
                .repeat(20);
        assert!(text.len() > SEMANTIC_CHUNK_SIZE);

        // (b) The chunker actually produced more than one chunk.
        let chunks = semantic_chunker().chunk(&text, None);
        assert!(
            chunks.len() > 1,
            "expected multiple chunks, got {}",
            chunks.len()
        );

        // (a) Output has exactly `dims` elements and is unit-norm.
        let v = local_chunk_embed(&text, DEFAULT_DIMS);
        assert_eq!(v.len(), DEFAULT_DIMS);
        assert!(
            (l2_norm(&v) - 1.0).abs() < 1e-4,
            "pooled vector is unit-norm"
        );
    }

    /// (c) A short string still embeds to a single unit-norm vector identical
    /// to the pre-change `local_embed` path.
    #[test]
    fn short_text_is_identical_to_pre_change_path() {
        let text = "Hello world, this is a short document.";
        assert!(text.len() <= SEMANTIC_CHUNK_SIZE);

        let got = local_chunk_embed(text, DEFAULT_DIMS);
        let expected = local_embed(text, DEFAULT_DIMS);
        assert_eq!(got, expected, "short text is a no-op vs local_embed");
        assert!(
            (l2_norm(&got) - 1.0).abs() < 1e-4,
            "single-chunk vector unit-norm"
        );
    }

    /// mean_pool_normalize on already-unit vectors returns a unit vector.
    #[test]
    fn mean_pool_of_two_vectors_is_unit_norm() {
        let a = local_embed("first chunk of text", DEFAULT_DIMS);
        let b = local_embed("a very different second chunk", DEFAULT_DIMS);
        let pooled = mean_pool_normalize(&[a, b]);
        assert_eq!(pooled.len(), DEFAULT_DIMS);
        assert!((l2_norm(&pooled) - 1.0).abs() < 1e-4);
    }

    /// `local_chunk_vectors` returns one unit-norm vector PER passage for a
    /// multi-chunk document, and pooling those equals `local_chunk_embed`.
    #[test]
    fn chunk_vectors_are_per_passage_and_pool_consistently() {
        let text =
            "The quick brown fox jumps over the lazy dog. Pack my box with five dozen liquor jugs. "
                .repeat(20);
        let n_chunks = semantic_chunker().chunk(&text, None).len();
        assert!(n_chunks > 1, "fixture must span multiple chunks");

        let vecs = local_chunk_vectors(&text, DEFAULT_DIMS);
        assert_eq!(vecs.len(), n_chunks, "one vector per chunk");
        for v in &vecs {
            assert_eq!(v.len(), DEFAULT_DIMS);
            assert!(
                (l2_norm(v) - 1.0).abs() < 1e-4,
                "each passage vector unit-norm"
            );
        }
        // Pooling the per-chunk vectors reproduces the pooled companion.
        assert_eq!(
            mean_pool_normalize(&vecs),
            local_chunk_embed(&text, DEFAULT_DIMS)
        );
    }

    /// The whole point of per-chunk storage: a query that matches ONE buried
    /// passage of a long document scores higher against the best chunk
    /// (max-sim) than against the blurred whole-document pooled vector. This
    /// is the vector-level invariant the ingest+query wiring relies on.
    #[test]
    fn max_sim_over_chunks_beats_pooled_for_a_buried_passage() {
        // A long document whose passages cover distinct topics; the topic of
        // interest ("photosynthesis chloroplast ...") is only ONE passage.
        let filler =
            "quarterly revenue guidance and operating margins for the fiscal year. ".repeat(12);
        let target_passage =
            "photosynthesis converts sunlight into chemical energy inside the chloroplast \
             of a plant cell using chlorophyll pigments. ";
        let doc = format!("{filler}{target_passage}{filler}");
        assert!(
            semantic_chunker().chunk(&doc, None).len() > 1,
            "doc must span multiple chunks"
        );

        let query = "how does chlorophyll drive photosynthesis in a chloroplast";
        let q = local_embed(query, DEFAULT_DIMS);

        // Pooled: the whole-doc average vector (what we stored before).
        let pooled = local_chunk_embed(&doc, DEFAULT_DIMS);
        let pooled_score = compute_vector_similarity("cosine", &q, &pooled);

        // Per-chunk: best-matching passage (what semantic search now uses).
        let chunk_vecs = local_chunk_vectors(&doc, DEFAULT_DIMS);
        let max_sim = chunk_vecs
            .iter()
            .map(|cv| compute_vector_similarity("cosine", &q, cv))
            .fold(f32::MIN, f32::max);

        assert!(
            max_sim > pooled_score,
            "best-passage max-sim ({max_sim}) should beat pooled ({pooled_score})"
        );
    }
}
