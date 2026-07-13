//! Response types for both the native xerj API and the ES-compatible API.
//!
//! The ES-compatible types mirror the exact JSON structure produced by
//! Elasticsearch 8.x so that official language clients (Python, Java, Go, JS)
//! work without modification.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Helper: true when a `matched_queries` value is Null/absent/empty.
fn matched_queries_is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Shared primitives
// ═════════════════════════════════════════════════════════════════════════════

/// The `_shards` sub-object present in most ES write/search responses.
///
/// xerj always responds as a single-shard cluster in v0.1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EsShards {
    pub total: u32,
    pub successful: u32,
    /// Skipped shards (search responses only). Declared BEFORE `failed`
    /// because serde emits fields in declaration order and ES 8.x renders
    /// search `_shards` as {total, successful, skipped, failed} — write
    /// responses omit `skipped` (None) and keep ES's {total, successful,
    /// failed}. Live-verified against ES 8.13.4 (byte-parity sweep).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<u32>,
    pub failed: u32,
}

impl EsShards {
    /// Standard single-shard success (write operations).
    pub fn single_success() -> Self {
        Self {
            total: 1,
            successful: 1,
            failed: 0,
            skipped: None,
        }
    }

    /// Standard single-shard search success.
    pub fn search_success() -> Self {
        Self {
            total: 1,
            successful: 1,
            failed: 0,
            skipped: Some(0),
        }
    }

    /// Multi-index search success: report one "shard" per participating
    /// index so `_shards.total` lines up with ES's per-shard numbers.
    /// xerj runs a single primary per index in v0.1 so total == successful.
    pub fn search_success_n(n: u32) -> Self {
        Self {
            total: n.max(1),
            successful: n.max(1),
            failed: 0,
            skipped: Some(0),
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Index management
// ═════════════════════════════════════════════════════════════════════════════

/// Response to `PUT /{index}` (create index).
#[derive(Debug, Serialize, Deserialize)]
pub struct EsIndexResponse {
    pub acknowledged: bool,
    pub shards_acknowledged: bool,
    pub index: String,
}

impl EsIndexResponse {
    pub fn ok(index: impl Into<String>) -> Self {
        Self {
            acknowledged: true,
            shards_acknowledged: true,
            index: index.into(),
        }
    }
}

/// Response to `DELETE /{index}`.
#[derive(Debug, Serialize, Deserialize)]
pub struct EsDeleteIndexResponse {
    pub acknowledged: bool,
}

impl EsDeleteIndexResponse {
    pub fn ok() -> Self {
        Self { acknowledged: true }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Document operations
// ═════════════════════════════════════════════════════════════════════════════

/// Response to `PUT /{index}/_doc/{id}` and `POST /{index}/_doc`.
#[derive(Debug, Serialize, Deserialize)]
pub struct EsDocResponse {
    #[serde(rename = "_index")]
    pub index: String,
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "_version")]
    pub version: u64,
    pub result: String, // "created" | "updated"
    #[serde(rename = "_shards")]
    pub shards: EsShards,
    #[serde(rename = "_seq_no")]
    pub seq_no: u64,
    #[serde(rename = "_primary_term")]
    pub primary_term: u64,
}

impl EsDocResponse {
    /// `result: "created"` — `version` is NOT always 1: a re-index after a
    /// delete reports `created` while `_version` continues from the
    /// tombstone (ES 8.13.4 live-verified).
    pub fn created(
        index: impl Into<String>,
        id: impl Into<String>,
        version: u64,
        seq_no: u64,
    ) -> Self {
        Self {
            index: index.into(),
            id: id.into(),
            version,
            result: "created".to_string(),
            shards: EsShards::single_success(),
            seq_no,
            primary_term: 1,
        }
    }

    pub fn updated(
        index: impl Into<String>,
        id: impl Into<String>,
        version: u64,
        seq_no: u64,
    ) -> Self {
        Self {
            index: index.into(),
            id: id.into(),
            version,
            result: "updated".to_string(),
            shards: EsShards::single_success(),
            seq_no,
            primary_term: 1,
        }
    }
}

/// Response to `GET /{index}/_doc/{id}`.
///
/// The metadata fields are `Option` because ES omits `_version` /
/// `_seq_no` / `_primary_term` entirely on a not-found GET — the 404 body
/// is exactly `{"_index": ..., "_id": ..., "found": false}` (live-verified
/// against ES 8.13.4).
#[derive(Debug, Serialize, Deserialize)]
pub struct EsGetResponse {
    #[serde(rename = "_index")]
    pub index: String,
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "_version", skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    #[serde(rename = "_seq_no", skip_serializing_if = "Option::is_none")]
    pub seq_no: Option<u64>,
    #[serde(rename = "_primary_term", skip_serializing_if = "Option::is_none")]
    pub primary_term: Option<u64>,
    pub found: bool,
    #[serde(rename = "_source", skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
}

impl EsGetResponse {
    pub fn found(
        index: impl Into<String>,
        id: impl Into<String>,
        version: u64,
        seq_no: u64,
        source: Value,
    ) -> Self {
        Self {
            index: index.into(),
            id: id.into(),
            version: Some(version),
            seq_no: Some(seq_no),
            primary_term: Some(1),
            found: true,
            source: Some(source),
        }
    }

    pub fn not_found(index: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            index: index.into(),
            id: id.into(),
            version: None,
            seq_no: None,
            primary_term: None,
            found: false,
            source: None,
        }
    }
}

/// Response to `DELETE /{index}/_doc/{id}`.
#[derive(Debug, Serialize, Deserialize)]
pub struct EsDeleteDocResponse {
    #[serde(rename = "_index")]
    pub index: String,
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "_version")]
    pub version: u64,
    pub result: String, // "deleted" | "not_found"
    #[serde(rename = "_shards")]
    pub shards: EsShards,
    #[serde(rename = "_seq_no")]
    pub seq_no: u64,
    #[serde(rename = "_primary_term")]
    pub primary_term: u64,
}

impl EsDeleteDocResponse {
    pub fn deleted(
        index: impl Into<String>,
        id: impl Into<String>,
        version: u64,
        seq_no: u64,
    ) -> Self {
        Self {
            index: index.into(),
            id: id.into(),
            version,
            result: "deleted".to_string(),
            shards: EsShards::single_success(),
            seq_no,
            primary_term: 1,
        }
    }

    /// ES 404 body for a delete that found nothing: same shape as a
    /// successful delete but `result: "not_found"` (live-verified against
    /// ES 8.13.4 — the version bookkeeping still advances).
    pub fn not_found(
        index: impl Into<String>,
        id: impl Into<String>,
        version: u64,
        seq_no: u64,
    ) -> Self {
        Self {
            index: index.into(),
            id: id.into(),
            version,
            result: "not_found".to_string(),
            shards: EsShards::single_success(),
            seq_no,
            primary_term: 1,
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Search
// ═════════════════════════════════════════════════════════════════════════════

/// A single hit in a search response.
#[derive(Debug, Serialize, Deserialize)]
pub struct EsHit {
    #[serde(rename = "_index")]
    pub index: String,
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "_score")]
    pub score: Option<f64>,
    /// Document version (incremented on each update).
    #[serde(rename = "_version", skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    /// Sequence number for optimistic concurrency control.
    #[serde(rename = "_seq_no", skip_serializing_if = "Option::is_none")]
    pub seq_no: Option<u64>,
    /// Primary term for optimistic concurrency control.
    #[serde(rename = "_primary_term", skip_serializing_if = "Option::is_none")]
    pub primary_term: Option<u64>,
    /// Source fields (omitted entirely when `_source: false`).
    #[serde(rename = "_source", skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
    /// Stored/doc-value fields (returned when `fields` is specified in the request).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<std::collections::HashMap<String, Value>>,
    /// Sort values (present when `sort` was specified in the query).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<Vec<Value>>,
    /// Highlight fragments per field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highlight: Option<std::collections::HashMap<String, Vec<String>>>,
    /// Score explanation (present when `explain: true` was requested).
    #[serde(rename = "_explanation", skip_serializing_if = "Option::is_none")]
    pub explanation: Option<Value>,
    /// Inner hits for nested / parent-child queries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inner_hits: Option<Value>,
    /// Names of named queries that matched this document. Skipped when
    /// empty/null. Can be an array (default) or a map when
    /// `include_named_queries_score=true`.
    #[serde(skip_serializing_if = "matched_queries_is_empty")]
    pub matched_queries: Value,
    /// `_ignored` meta field: names of doc fields whose values failed
    /// validation (e.g. ignore_malformed). ES emits this at hit top level.
    #[serde(rename = "_ignored", skip_serializing_if = "Option::is_none")]
    pub ignored: Option<Vec<String>>,
    /// `ignored_field_values` meta field (ES 8+): per-field array of the
    /// original malformed values that triggered `_ignored`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignored_field_values: Option<Value>,
}

/// The `total` sub-object in `hits`.
#[derive(Debug, Serialize, Deserialize)]
pub struct EsHitsTotal {
    pub value: u64,
    /// `"eq"` when the count is exact; `"gte"` when the count was capped.
    pub relation: String,
}

/// The outer `hits` object.
#[derive(Debug, Serialize, Deserialize)]
pub struct EsHits {
    pub total: EsHitsTotal,
    pub max_score: Option<f64>,
    pub hits: Vec<EsHit>,
}

/// Top-level search response — matches ES `_search` JSON exactly.
#[derive(Debug, Serialize, Deserialize)]
pub struct EsSearchResponse {
    /// Query wall-clock time in milliseconds.
    pub took: u64,
    pub timed_out: bool,
    #[serde(rename = "_shards")]
    pub shards: EsShards,
    pub hits: EsHits,
    /// Aggregation results (present when `aggs` was in the request).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregations: Option<Value>,
}

impl EsSearchResponse {
    pub fn empty(took_ms: u64) -> Self {
        Self {
            took: took_ms,
            timed_out: false,
            shards: EsShards::search_success(),
            hits: EsHits {
                total: EsHitsTotal {
                    value: 0,
                    relation: "eq".to_string(),
                },
                max_score: None,
                hits: vec![],
            },
            aggregations: None,
        }
    }

    pub fn with_hits(took_ms: u64, hits: Vec<EsHit>, total: u64, max_score: Option<f64>) -> Self {
        Self {
            took: took_ms,
            timed_out: false,
            shards: EsShards::search_success(),
            hits: EsHits {
                total: EsHitsTotal {
                    value: total,
                    relation: "eq".to_string(),
                },
                max_score,
                hits,
            },
            aggregations: None,
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Bulk
// ═════════════════════════════════════════════════════════════════════════════

/// A single item in a `_bulk` response.
#[derive(Debug, Serialize, Deserialize)]
pub struct EsBulkItem {
    /// The action: `"index"`, `"create"`, `"update"`, or `"delete"`.
    #[serde(flatten)]
    pub action: EsBulkItemAction,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EsBulkItemAction {
    Index(EsBulkItemResult),
    Create(EsBulkItemResult),
    Update(EsBulkItemResult),
    Delete(EsBulkItemResult),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EsBulkItemResult {
    #[serde(rename = "_index")]
    pub index: String,
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "_version")]
    pub version: u64,
    pub result: String,
    #[serde(rename = "_shards")]
    pub shards: EsShards,
    #[serde(rename = "_seq_no")]
    pub seq_no: u64,
    #[serde(rename = "_primary_term")]
    pub primary_term: u64,
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BulkItemError>,
    /// For update actions with `_source: true` / `_source: {...}` in the
    /// action metadata, this carries the post-update source so clients
    /// see `items[N].update.get._source.<field>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub get: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BulkItemError {
    #[serde(rename = "type")]
    pub error_type: String,
    pub reason: String,
    pub status: u16,
}

/// Top-level `_bulk` response.
#[derive(Debug, Serialize, Deserialize)]
pub struct EsBulkResponse {
    pub took: u64,
    pub errors: bool,
    pub items: Vec<EsBulkItem>,
}

// ═════════════════════════════════════════════════════════════════════════════
// Cluster health
// ═════════════════════════════════════════════════════════════════════════════

/// `GET /_cluster/health` response — matches ES exactly.
#[derive(Debug, Serialize, Deserialize)]
pub struct EsHealthResponse {
    pub cluster_name: String,
    pub status: String, // "green" | "yellow" | "red"
    pub timed_out: bool,
    pub number_of_nodes: u32,
    pub number_of_data_nodes: u32,
    pub active_primary_shards: u32,
    pub active_shards: u32,
    pub relocating_shards: u32,
    pub initializing_shards: u32,
    pub unassigned_shards: u32,
    pub unassigned_primary_shards: u32,
    pub delayed_unassigned_shards: u32,
    pub number_of_pending_tasks: u32,
    pub number_of_in_flight_fetch: u32,
    pub task_max_waiting_in_queue_millis: u64,
    pub active_shards_percent_as_number: f64,
}

impl EsHealthResponse {
    pub fn green(cluster_name: impl Into<String>, index_count: u32) -> Self {
        Self {
            cluster_name: cluster_name.into(),
            status: "green".to_string(),
            timed_out: false,
            number_of_nodes: 1,
            number_of_data_nodes: 1,
            active_primary_shards: index_count,
            active_shards: index_count,
            relocating_shards: 0,
            initializing_shards: 0,
            unassigned_shards: 0,
            unassigned_primary_shards: 0,
            delayed_unassigned_shards: 0,
            number_of_pending_tasks: 0,
            number_of_in_flight_fetch: 0,
            task_max_waiting_in_queue_millis: 0,
            active_shards_percent_as_number: 100.0,
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Root info (`GET /`)
// ═════════════════════════════════════════════════════════════════════════════

/// ES version block inside the info response.
#[derive(Debug, Serialize, Deserialize)]
pub struct EsVersion {
    /// Reported ES version — `"8.13.0"` is a common client baseline.
    pub number: String,
    pub build_flavor: String,
    pub build_type: String,
    pub build_hash: String,
    pub build_date: String,
    pub build_snapshot: bool,
    pub lucene_version: String,
    pub minimum_wire_compatibility_version: String,
    pub minimum_index_compatibility_version: String,
}

impl Default for EsVersion {
    fn default() -> Self {
        Self {
            // Advertise 8.13.0 — well within what all modern ES clients support.
            number: "8.13.0".to_string(),
            build_flavor: "default".to_string(),
            build_type: "tar".to_string(),
            build_hash: "00000000".to_string(),
            build_date: "2024-03-22T00:00:00.000Z".to_string(),
            build_snapshot: false,
            lucene_version: "9.10.0".to_string(),
            minimum_wire_compatibility_version: "7.17.0".to_string(),
            minimum_index_compatibility_version: "7.0.0".to_string(),
        }
    }
}

/// `GET /` response — the very first thing ES clients call to verify
/// connectivity and determine the server version.
#[derive(Debug, Serialize, Deserialize)]
pub struct EsInfoResponse {
    pub name: String,
    pub cluster_name: String,
    pub cluster_uuid: String,
    pub version: EsVersion,
    pub tagline: String,
}

impl EsInfoResponse {
    pub fn new(node_name: impl Into<String>, cluster_name: impl Into<String>) -> Self {
        Self {
            name: node_name.into(),
            cluster_name: cluster_name.into(),
            // Stable pseudo-UUID derived from cluster name — fine for compat.
            cluster_uuid: "xerj-cluster-0000-0000-0000-000000000000".to_string(),
            version: EsVersion::default(),
            // Must be exactly this string — many clients assert it.
            tagline: "You Know, for Search".to_string(),
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Mapping / settings
// ═════════════════════════════════════════════════════════════════════════════

/// `GET /{index}/_mapping` response wraps the mapping per index.
/// The outer key is the index name.
pub type EsMappingResponse = std::collections::HashMap<String, EsIndexMapping>;

#[derive(Debug, Serialize, Deserialize)]
pub struct EsIndexMapping {
    pub mappings: EsMappings,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EsMappings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic: Option<String>,
    pub properties: serde_json::Map<String, Value>,
}

/// `GET /{index}/_settings` response.
pub type EsSettingsResponse = std::collections::HashMap<String, EsIndexSettings>;

#[derive(Debug, Serialize, Deserialize)]
pub struct EsIndexSettings {
    pub settings: EsSettingsBlock,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EsSettingsBlock {
    pub index: EsIndexSettingsInner,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EsIndexSettingsInner {
    pub number_of_shards: String,
    pub number_of_replicas: String,
    pub creation_date: String,
    pub uuid: String,
    pub version: EsIndexVersion,
    pub provided_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EsIndexVersion {
    pub created: String,
}

// ═════════════════════════════════════════════════════════════════════════════
// Native API responses
// ═════════════════════════════════════════════════════════════════════════════

/// Standard native API envelope that wraps every successful response.
#[derive(Debug, Serialize)]
pub struct NativeResponse<T: Serialize> {
    pub data: T,
    pub took_ms: u64,
    pub request_id: String,
}

impl<T: Serialize> NativeResponse<T> {
    pub fn new(data: T, took_ms: u64, request_id: impl Into<String>) -> Self {
        Self {
            data,
            took_ms,
            request_id: request_id.into(),
        }
    }
}

/// Native index info response.
#[derive(Debug, Serialize)]
pub struct NativeIndexInfo {
    pub name: String,
    pub doc_count: u64,
    pub schema_version: u64,
    pub field_count: usize,
    pub settings: crate::state::IndexSettings,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Native document ingest response.
#[derive(Debug, Serialize)]
pub struct NativeIngestResponse {
    pub index: String,
    pub id: String,
    pub result: String,
    pub seq_no: u64,
}

/// Native schema response.
#[derive(Debug, Serialize)]
pub struct NativeSchemaResponse {
    pub index: String,
    pub schema: xerj_common::types::Schema,
}

/// Native health response.
#[derive(Debug, Serialize)]
pub struct NativeHealthResponse {
    pub status: String,
    pub index_count: usize,
    pub total_docs: u64,
    pub version: String,
}
