//! Agent-Memory REST API — namespaced semantic memory for AI agents.
//!
//! Each namespace is backed by a regular XERJ index under a reserved name
//! (`.xerj-memory-{namespace}`), so store / recall reuse the exact same
//! document, `dense_vector`, BM25, and metadata-filter code paths that already
//! serve the ES-compatible surface. This module is a thin, self-contained
//! adapter over those handlers — it does NOT re-implement search.
//!
//! Endpoints (mounted on the ES-compat router):
//! ```text
//! GET    /_memory?from&size              list_ns — {count, namespaces:[{namespace, count, graph}], from, size, next}
//! POST   /_memory/{namespace}            store   — {text, metadata?, vector?, id?, dedup?, dedup_threshold?, valid_from?, valid_to?} → {id, created} | {id, created:false, deduplicated:true}
//! POST   /_memory/{namespace}/_recall    recall  — {query?, vector?, semantic?, hybrid?, fusion?, k?, filter?, recency_weight?, graph?, as_of?} → {hits:[…], as_of?}
//! GET    /_memory/{namespace}?from&size  list    — {count, entries:[…], next} (paged, recent first)
//! DELETE /_memory/{namespace}/{id}       forget  — delete one entry
//! DELETE /_memory/{namespace}            drop    — drop the whole namespace
//! ```
//!
//! `POST /_memory/{namespace}/_doc` and `PUT /_memory/{namespace}/_doc/{id}`
//! are the Elasticsearch document routes, not this API. They answer `405`
//! with a JSON body that names the store and recall calls, instead of an
//! empty method-not-allowed.
//!
//! ## Pagination (item 12)
//!
//! `GET /_memory/{namespace}` pages the namespace with `from` (offset, default
//! 0; `after` is accepted as an alias) and `size` (page size, default 100,
//! capped at [`MAX_LIST_SIZE`]). Before this the endpoint silently truncated at
//! 100 entries with no way to see the rest — an agent introspecting a large
//! namespace got a partial, unmarked view. The response now carries `count`
//! (the true total) and a `next` cursor (`{from, size}` for the next page, or
//! `null` on the last page). Deep offsets are bounded by the backing index's
//! `max_result_window`; for exhaustive retrieval prefer `_recall`.
//!
//! ## Validity windows and as-of recall (#1792)
//!
//! A memory may carry a valid-time window `[valid_from, valid_to)` (epoch-ms
//! or RFC3339 on input, epoch-ms on output) next to its transaction time:
//! `stored_at` (when this version was written) and `recorded_at` (when the
//! fact was first recorded; kept across re-stores of the same `id`).
//! `_recall` takes a top-level `as_of` (default now) and returns only
//! memories valid at that instant; a memory with no bounds is always valid.
//! Retract a fact by storing the same `id` again with `valid_to`: bounds the
//! request omits are carried over, so the fact stays visible to an `as_of`
//! before the retraction. `graph.as_of` defaults to the top-level `as_of`.
//! A namespace that never stored a bound runs recall with no extra filter.
//!
//! ## Authorization model (item 12)
//!
//! ⚠️ The `/_memory/*` endpoints are guarded ONLY by the process-wide API-key
//! auth middleware (the admin key or any key minted via
//! `POST /_security/api_key`). There is **no per-namespace authorization**: any
//! credential that can reach the node can read, write, and drop **every**
//! namespace. Namespaces isolate data *by name* (a recall in `agent-a` never
//! returns `agent-b`'s memories), NOT by credential — do not treat a namespace
//! as a security boundary between mutually-distrusting tenants. Per-namespace
//! access control depends on the deferred RBAC enforcement (see
//! `xerj_engine::rbac`).
//!
//! Recall has three single modes, tried in order: an explicit query `vector`
//! (kNN over a caller-supplied embedding) → `semantic: true` (the server embeds
//! `query` with the same embedder used at store time and recalls by vector
//! similarity) → plain `query` text (BM25 relevance). `hybrid: true` (#918) is
//! the opt-in fourth: the BM25 and the server-side semantic leg run over the
//! same `query` and are fused by the engine's `hybrid` query type (reciprocal
//! rank fusion by default, `fusion: "linear"` on request), so an agent gets
//! fused recall without leaving `/_memory` for a raw `_search` on the backing
//! index. All of them are offline-testable with NO external embedding service
//! — memories are stored in a `semantic_text` field, so the built-in
//! deterministic embedder vectorises both the stored text and the recall
//! query. Namespaces isolate — a recall in namespace A
//! never sees namespace B's entries because they live in physically distinct
//! backing indices.

use axum::{
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Principal;
use crate::authz;
use crate::es_compat::{
    self, DeleteDocParams, DeleteIndexParams, EsSearchBody, EsSearchJson, EsSearchQueryParams,
    IndexDocParams,
};
use crate::extract::OptionalJson;
use crate::state::AppState;
use xerj_engine::rbac::Privilege;

/// Reserved backing-index prefix. Real (user) indices can never collide: the
/// ES-compat `create_index` handler rejects any index name that starts with
/// `_`, `-`, or `+`, and a caller cannot address a `.`-leading system index
/// through `/_memory/*`.
const MEMORY_PREFIX: &str = crate::authz::RESERVED_INDEX_PREFIX;

/// Default page size for the introspection (`GET`) endpoint when `size` is
/// omitted. Callers page further with `from`/`size` (see [`ListParams`]).
const LIST_LIMIT: usize = 100;

/// Hard cap on a single `list` page `size`, so one call can't ask the backing
/// index for an unbounded result set (memory-safety). Deep pagination uses
/// `from` across multiple calls.
const MAX_LIST_SIZE: usize = 1000;

/// Default number of memories returned by `_recall` when `k` is omitted.
const DEFAULT_K: usize = 10;

/// Default cosine-similarity threshold for opt-in semantic dedup in `store`.
/// XERJ's cosine `_score` is the cosine similarity itself (0..1), so a stored
/// memory whose nearest existing neighbour scores at or above this is treated
/// as a near-duplicate and skipped.
const DEFAULT_DEDUP_THRESHOLD: f32 = 0.95;

/// Resolve a namespace to its backing index name.
fn backing_index(namespace: &str) -> String {
    format!("{MEMORY_PREFIX}{namespace}")
}

/// Validate a namespace. We keep this deliberately strict so the derived index
/// name is always a legal, lowercase XERJ/ES index name and cannot be used to
/// smuggle characters (`/`, `,`, `*`, whitespace, uppercase, `..`) into the
/// backing-index routing layer.
pub(crate) fn validate_namespace(ns: &str) -> Result<(), String> {
    if ns.is_empty() {
        return Err("namespace must not be empty".into());
    }
    if ns.len() > 200 {
        return Err("namespace too long (max 200 chars)".into());
    }
    let first = ns.chars().next().unwrap();
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err("namespace must start with a lowercase letter or digit".into());
    }
    for c in ns.chars() {
        let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.');
        if !ok {
            return Err(format!(
                "namespace contains illegal character '{c}' (allowed: a-z 0-9 _ - .)"
            ));
        }
    }
    if ns.contains("..") {
        return Err("namespace must not contain '..'".into());
    }
    // Second-brain collision guard (SECOND_BRAIN_SPEC §1): brain `B` keeps its
    // graph edges in `.xerj-memory-{B}-edges`, so a namespace ending in
    // `-edges` would silently write memories into another brain's edge index.
    // Deliberate breaking change — record in the release notes.
    if ns.ends_with("-edges") {
        return Err("namespace suffix '-edges' is reserved for graph edge indices".into());
    }
    Ok(())
}

/// A memory write that failed because the data-dir disk is at the flood-stage
/// watermark (#2284).
///
/// The document API answers that with a `cluster_block_exception`, and this
/// module used to stringify that JSON into `error.reason` — the nested blob
/// the CLI and MCP showed. The replacement is the same sentence autoindex
/// prints for the watermark (#2077): free space, used percent, threshold,
/// and the `limits.disk_flood_stage_percent` override — plus the data-dir
/// path and the `--data-dir` move. Any other failure keeps `wrapped`.
fn memory_failure(status: StatusCode, body: &Value, data_dir: &str, wrapped: String) -> Response {
    error_response(status, memory_failure_reason(body, data_dir, &wrapped))
}

fn memory_failure_reason(body: &Value, data_dir: &str, wrapped: &str) -> String {
    let (free, used_live) = disk_sample(data_dir);
    let flood_live = xerj_engine::governor::global().and_then(|governor| {
        let pct = governor.disk_flood_admission().flood_pct;
        (pct > 0).then_some(pct)
    });
    let facts = xerj_autoindex::flood::DiskFacts {
        data_dir: data_dir.to_string(),
        free_bytes: free,
        used_pct: used_live,
        flood_pct: flood_live,
    };
    xerj_autoindex::flood::explain_memory_flood(body, Some(&facts))
        .unwrap_or_else(|| wrapped.to_string())
}

/// `(free bytes, used percent)` for the filesystem holding `data_dir`.
/// `None` when the probe fails — the message then says unknown, it does not
/// invent a zero.
fn disk_sample(data_dir: &str) -> (Option<u64>, Option<u64>) {
    match xerj_engine::governor::disk_stats(data_dir) {
        Some((total, avail)) if total > 0 => {
            let used = ((total.saturating_sub(avail) as u128 * 100) / total as u128) as u64;
            (Some(avail), Some(used))
        }
        _ => (None, None),
    }
}

/// Emit a uniform ES-shaped error response.
fn error_response(status: StatusCode, reason: impl Into<String>) -> Response {
    (
        status,
        Json(json!({
            "error": { "type": "memory_error", "reason": reason.into() },
            "status": status.as_u16(),
        })),
    )
        .into_response()
}

/// Drain an inner handler [`Response`] into `(status, json_body)` so this
/// module can inspect the reused ES-compat handler's result and re-shape it
/// into the agent-memory contract.
async fn drain_json(resp: Response) -> (StatusCode, Value) {
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap_or_default();
    let value = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// Whether the backing index for a namespace currently exists.
fn index_exists(state: &AppState, index: &str) -> bool {
    state.engine.get_index(index).is_ok()
}

/// Ensure the backing index exists with an appropriate mapping. Created lazily
/// on first store. When the first stored memory carries a vector, the index is
/// created with a `dense_vector` field sized to that vector so kNN recall works
/// out of the box. `text` is always explicitly mapped; `metadata` deliberately
/// is not -- see the comment where `properties` is built below.
async fn ensure_backing_index(
    state: &AppState,
    index: &str,
    vector_dims: Option<usize>,
) -> Result<(), Response> {
    if index_exists(state, index) {
        return Ok(());
    }

    // `text` is a `semantic_text` field: the engine auto-embeds it at ingest
    // with the built-in (or configured) embedder into a companion `text_vector`,
    // so a memory is BM25-searchable *and* kNN-searchable with zero client-side
    // embedding. That is what powers `_recall {"semantic": true}` — the query
    // text is embedded server-side the same way and matched by vector
    // similarity. 384 mirrors the built-in embedder width (xerj_ai DEFAULT_DIMS);
    // kept as a literal to avoid a build-graph edge from xerj-api onto xerj-ai
    // (same convention as es_compat's semantic_text mapper).
    //
    // `metadata` is deliberately NOT pre-declared here (unlike `text`/
    // `stored_at`, which are always present and same-shaped). Its own shape
    // is caller-defined and varies per memory, so pinning it to a bare
    // `{"type": "object"}` up front would permanently prevent its keys from
    // ever being dynamically mapped -- once a field has ANY explicit
    // mapping entry, later documents no longer go through dynamic
    // discovery for it, even a shallow one with no `properties`. Leaving it
    // out lets the first stored memory's `metadata` keys flow through the
    // engine's normal dynamic-mapping path instead, which now recurses
    // into nested objects (see xerj-engine's `dynamic_field_config`), so
    // `metadata.kind`/`metadata.topic`/etc. become real, `_mapping`-visible
    // fields instead of being permanently opaque.
    //
    // KNOWN LIMITATION, deliberately not addressed here: this only helps
    // namespaces whose backing index is created AFTER this change ships. A
    // `.xerj-memory-*` index created before it already has `metadata`
    // declared as a bare `{"type": "object"}` in ITS OWN stored mapping --
    // that declaration was written once, at that index's creation time,
    // and is not retroactively edited by a code change. No migration for
    // already-existing namespaces is included in this change; one would
    // need to either re-create the index or explicitly `PUT _mapping` a
    // corrected `metadata.properties` shape onto it by hand.
    let mut properties = json!({
        "text": { "type": "semantic_text", "dimensions": 384 },
        "stored_at": { "type": "long" }
    });
    if let Some(dims) = vector_dims {
        if dims == 0 {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "vector must not be empty",
            ));
        }
        properties["vector"] = json!({
            "type": "dense_vector",
            "dims": dims,
            "index": true,
            "similarity": "cosine"
        });
    }

    let mapping = json!({ "mappings": { "properties": properties } });
    let resp = es_compat::create_index(
        State(state.clone()),
        Path(index.to_string()),
        OptionalJson(Some(mapping)),
    )
    .await
    .into_response();

    let (status, body) = drain_json(resp).await;
    // Tolerate a concurrent creator winning the race (resource_already_exists).
    if status.is_success() || index_exists(state, index) {
        return Ok(());
    }
    Err(memory_failure(
        status,
        &body,
        &state.config.server.data_dir,
        format!("failed to create memory namespace backing index: {body}"),
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// Store
// ─────────────────────────────────────────────────────────────────────────────

/// Request body for `POST /_memory/{namespace}`.
#[derive(Debug, Deserialize)]
pub struct StoreBody {
    /// Free text of the memory. Always stored and BM25-indexed.
    pub text: String,
    /// Optional arbitrary metadata object (tags, source, …).
    #[serde(default)]
    pub metadata: Option<Value>,
    /// Optional caller-supplied dense embedding. When present, it is stored as
    /// a `dense_vector` and enables kNN recall.
    #[serde(default)]
    pub vector: Option<Vec<f32>>,
    /// Optional explicit ID. Auto-generated (UUID) when omitted.
    #[serde(default)]
    pub id: Option<String>,
    /// Opt-in semantic dedup. When `true`, before indexing we probe the backing
    /// index for the single nearest existing memory — kNN when a `vector` is
    /// supplied, server-side `semantic` recall over `text` otherwise — and, if
    /// its cosine `_score` meets `dedup_threshold`, skip the write and return
    /// the existing entry with `created:false, deduplicated:true`. Defaults to
    /// `false` so existing callers keep storing every entry (backward-compatible).
    #[serde(default)]
    pub dedup: Option<bool>,
    /// Cosine-similarity threshold in `[0, 1]` for `dedup`. Defaults to 0.95.
    /// Ignored unless `dedup` is `true`.
    #[serde(default)]
    pub dedup_threshold: Option<f32>,
    /// Start of the record's validity window (valid time, #1792), inclusive.
    /// Epoch-ms number or RFC3339 string, same parser as `graph.as_of`.
    /// Absent → valid since forever. A record with neither bound is always
    /// valid, so callers that never send these see no change.
    #[serde(default)]
    pub valid_from: Option<Value>,
    /// End of the record's validity window, exclusive (`[valid_from,
    /// valid_to)`). Absent → still valid. Retract a fact by storing the same
    /// `id` again with `valid_to` set: bounds the new request omits are
    /// carried over from the stored version, and so is `recorded_at`.
    #[serde(default)]
    pub valid_to: Option<Value>,
}

/// Parse one optional validity bound (`valid_from` / `valid_to` / `as_of`).
/// JSON `null` counts as absent; anything else must be an epoch-ms number or
/// an RFC3339 string, or the request is a 400 naming the field.
fn parse_time_bound(v: Option<&Value>, field: &str) -> Result<Option<i64>, Box<Response>> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(v) => parse_epoch_ms(v).map(Some).ok_or_else(|| {
            Box::new(error_response(
                StatusCode::BAD_REQUEST,
                format!("`{field}` must be an epoch-ms number or an RFC3339 string"),
            ))
        }),
    }
}

/// True when any record in the backing index has carried a validity bound
/// (the dynamically mapped `valid_from` / `valid_to` fields exist). Namespaces
/// that never used validity skip the as-of filter entirely, so their recall
/// query is unchanged.
async fn schema_has_validity(state: &AppState, index: &str) -> bool {
    match state.engine.get_index(index) {
        Ok(idx) => {
            let schema = idx.schema().await;
            schema.has_field("valid_from") || schema.has_field("valid_to")
        }
        Err(_) => false,
    }
}

/// `_source` of the stored version of `id`, or `None` when absent.
async fn existing_source(state: &AppState, index: &str, id: &str) -> Option<Value> {
    let resp = es_compat::get_doc(
        State(state.clone()),
        Path((index.to_string(), id.to_string())),
        axum::extract::Query(es_compat::GetDocParams::default()),
    )
    .await
    .into_response();
    let (status, body) = drain_json(resp).await;
    if !status.is_success() || body.get("found") != Some(&Value::Bool(true)) {
        return None;
    }
    body.get("_source").cloned()
}

/// Filter clause matching records valid at instant `t`: `valid_from` absent
/// or `<= t`, and `valid_to` absent or `> t` (half-open window). The
/// "missing bound = open-ended" shape follows the empty-or-range pattern of
/// qdrant (`IsEmptyCondition`, lib/segment/src/types.rs:3780) and
/// meilisearch's `NOT EXISTS` filter (crates/filter-parser/src/lib.rs:53),
/// written here as plain bool/exists/range clauses.
fn validity_filter(t: i64) -> Value {
    json!({ "bool": { "filter": [
        { "bool": { "should": [
            { "bool": { "must_not": [ { "exists": { "field": "valid_from" } } ] } },
            { "range": { "valid_from": { "lte": t } } }
        ], "minimum_should_match": 1 } },
        { "bool": { "should": [
            { "bool": { "must_not": [ { "exists": { "field": "valid_to" } } ] } },
            { "range": { "valid_to": { "gt": t } } }
        ], "minimum_should_match": 1 } }
    ] } })
}

/// Best-effort semantic-dedup probe. Returns the single nearest existing memory
/// as `(id, cosine_score)`, or `None` when the namespace is empty / the probe
/// cannot run. Uses kNN over the stored `vector` when a caller embedding is
/// supplied, otherwise server-side `semantic` recall over the `text`
/// semantic_text field (the same embedder used at store time). Any probe error
/// (empty index, schema mismatch) is treated as "no duplicate" so dedup is
/// strictly best-effort and never blocks a legitimate write.
async fn dedup_probe(
    state: &AppState,
    index: &str,
    principal: &Principal,
    text: &str,
    vector: Option<&[f32]>,
) -> Option<(String, f32)> {
    let mut search_body = EsSearchBody {
        size: 1,
        ..Default::default()
    };
    match vector {
        Some(vec) if !vec.is_empty() => {
            search_body.knn = Some(json!({ "field": "vector", "query_vector": vec, "k": 1 }));
        }
        _ => {
            if text.is_empty() {
                return None;
            }
            search_body.query =
                Some(json!({ "semantic": { "field": "text", "query": text, "k": 1 } }));
        }
    }

    let resp = es_compat::search(
        State(state.clone()),
        Path(index.to_string()),
        axum::extract::Query(EsSearchQueryParams::default()),
        principal.clone(),
        EsSearchJson(Some(search_body)),
    )
    .await
    .into_response();

    let (status, body) = drain_json(resp).await;
    if !status.is_success() {
        return None;
    }
    let hit = body.pointer("/hits/hits/0")?;
    let id = hit.get("_id").and_then(Value::as_str)?.to_string();
    let score = hit.get("_score").and_then(Value::as_f64)? as f32;
    Some((id, score))
}

/// `POST /_memory/{namespace}` — store a memory entry.
pub async fn store(
    State(state): State<AppState>,
    Path(namespace): Path<String>,
    principal: Principal,
    body: OptionalJson<StoreBody>,
) -> Response {
    if let Err(reason) = validate_namespace(&namespace) {
        return error_response(StatusCode::BAD_REQUEST, reason);
    }
    if let Err(denied) =
        authz::authorize_memory_namespace(&principal, &namespace, Privilege::WriteIndex)
    {
        return denied;
    }
    let body = match body.0 {
        Some(b) => b,
        None => return error_response(StatusCode::BAD_REQUEST, "missing request body"),
    };
    if body.text.is_empty() && body.vector.is_none() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "a memory must have non-empty `text` or a `vector`",
        );
    }

    let mut valid_from = match parse_time_bound(body.valid_from.as_ref(), "valid_from") {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let mut valid_to = match parse_time_bound(body.valid_to.as_ref(), "valid_to") {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let sent_validity = valid_from.is_some() || valid_to.is_some();
    if let (Some(f), Some(t)) = (valid_from, valid_to) {
        if t <= f {
            return error_response(
                StatusCode::BAD_REQUEST,
                "empty validity window: `valid_to` must be later than `valid_from`",
            );
        }
    }

    let index = backing_index(&namespace);
    let dims = body.vector.as_ref().map(|v| v.len());
    if let Err(resp) = ensure_backing_index(&state, &index, dims).await {
        return resp;
    }

    // #1792: re-storing an existing id keeps the record's validity history.
    // Bounds this request omits come from the stored version, and
    // `recorded_at` (first time the fact was recorded) is never reset, so a
    // retraction is just `{id, text, valid_to}`. Only runs when the request
    // or the namespace uses validity, so plain stores do no extra read.
    let mut recorded_at: Option<i64> = None;
    if let Some(explicit) = body.id.as_deref() {
        if sent_validity || schema_has_validity(&state, &index).await {
            if let Some(prev) = existing_source(&state, &index, explicit).await {
                if valid_from.is_none() {
                    valid_from = prev.get("valid_from").and_then(Value::as_i64);
                }
                if valid_to.is_none() {
                    valid_to = prev.get("valid_to").and_then(Value::as_i64);
                }
                recorded_at = prev
                    .get("recorded_at")
                    .and_then(Value::as_i64)
                    .or_else(|| prev.get("stored_at").and_then(Value::as_i64));
            }
        }
    }
    if let (Some(f), Some(t)) = (valid_from, valid_to) {
        if t <= f {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!(
                    "empty validity window: `valid_to` ({t}) must be later than \
                     `valid_from` ({f}) of the stored version"
                ),
            );
        }
    }
    let has_validity = valid_from.is_some() || valid_to.is_some();

    // Opt-in semantic dedup: skip the write when a near-identical memory already
    // exists. Off by default; only runs when the caller sets `dedup: true`.
    if body.dedup == Some(true) {
        let threshold = body.dedup_threshold.unwrap_or(DEFAULT_DEDUP_THRESHOLD);
        if let Some((existing_id, score)) = dedup_probe(
            &state,
            &index,
            &principal,
            &body.text,
            body.vector.as_deref(),
        )
        .await
        {
            // A versioned re-store of the same id (a retraction) is an
            // update, not a duplicate of itself.
            let self_update = has_validity && body.id.as_deref() == Some(existing_id.as_str());
            if score >= threshold && !self_update {
                return (
                    StatusCode::OK,
                    Json(json!({
                        "id": existing_id,
                        "namespace": namespace,
                        "created": false,
                        "deduplicated": true,
                        "score": score,
                    })),
                )
                    .into_response();
            }
        }
    }

    let id = body.id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut doc = json!({
        "text": body.text,
        "stored_at": now_ms,
    });
    if has_validity {
        if let Some(f) = valid_from {
            doc["valid_from"] = json!(f);
        }
        if let Some(t) = valid_to {
            doc["valid_to"] = json!(t);
        }
        doc["recorded_at"] = json!(recorded_at.unwrap_or(now_ms));
    }
    if let Some(md) = body.metadata {
        doc["metadata"] = md;
    }
    if let Some(vec) = body.vector {
        doc["vector"] = json!(vec);
    }

    let resp = es_compat::index_doc(
        State(state.clone()),
        Path((index.clone(), id.clone())),
        axum::extract::Query(IndexDocParams::default()),
        es_compat::document_body(&doc),
    )
    .await
    .into_response();

    let (status, body) = drain_json(resp).await;
    if !status.is_success() {
        // #2284: a flood-stage block arrives as a nested
        // `cluster_block_exception`. Say the disk is nearly full instead.
        return memory_failure(
            status,
            &body,
            &state.config.server.data_dir,
            format!("failed to store memory: {body}"),
        );
    }

    (
        StatusCode::CREATED,
        Json(json!({
            "id": id,
            "namespace": namespace,
            "created": true,
        })),
    )
        .into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Recall
// ─────────────────────────────────────────────────────────────────────────────

/// Request body for `POST /_memory/{namespace}/_recall`.
///
/// Unknown fields are rejected with a 400 (`deny_unknown_fields`): a typo'd
/// key used to be silently ignored, degrading the request to a match-all
/// that returned arbitrary memories at score 1.0 — the worst possible
/// failure mode for an agent that trusts its recall results.  Exactly one
/// of `vector` or `query` must be supplied (enforced in [`recall`]).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallBody {
    /// Free-text query → BM25 recall over stored text.
    #[serde(default)]
    pub query: Option<String>,
    /// Query embedding → kNN recall over stored vectors. Takes precedence over
    /// `query` and `semantic` when supplied.
    #[serde(default)]
    pub vector: Option<Vec<f32>>,
    /// When `true`, embed `query` server-side with the same embedder used at
    /// store time and recall by vector similarity — no client-side embedding
    /// needed. Requires a non-empty `query`. Default (`false`/absent) keeps the
    /// BM25 text-relevance recall. Ignored when an explicit `vector` is given.
    #[serde(default)]
    pub semantic: Option<bool>,
    /// When `true`, run BM25 and server-side semantic recall over `query`
    /// together and fuse the two rankings — the engine's `hybrid` query type
    /// lifted into `_recall` (#918). Requires a non-empty `query`; a
    /// caller-supplied `vector` or `semantic: true` beside it is a 400, so a
    /// request never silently picks one leg. Default (`false`/absent) keeps
    /// the single-mode behaviour bit-for-bit.
    #[serde(default)]
    pub hybrid: Option<bool>,
    /// Fusion strategy for `hybrid: true`: `"rrf"` (default; reciprocal rank
    /// fusion, k=60) or `"linear"` (min-max-normalised score sum). Anything
    /// else — including `"learned"`, which the engine does not implement — is
    /// a 400. Only meaningful with `hybrid: true`; a 400 otherwise.
    #[serde(default)]
    pub fusion: Option<String>,
    /// Number of memories to return. Defaults to 10.
    #[serde(default)]
    pub k: Option<usize>,
    /// Optional metadata pre-filter, expressed as a standard ES query clause
    /// (e.g. `{"term": {"metadata.topic": "cats"}}`). Applied as a `bool`
    /// `filter` so it narrows — but does not score — the recalled set.
    #[serde(default)]
    pub filter: Option<Value>,
    /// Optional recency blend weight in `[0, 1]`. When present, recall
    /// over-fetches candidates and re-ranks by
    /// `blended = (1 - w) * norm_score + w * norm_recency`, where both terms are
    /// min-max normalized across the candidate set and `norm_recency` is derived
    /// from each memory's `stored_at`. `0` = pure relevance, `1` = pure recency.
    /// Absent → existing relevance-only behavior is preserved exactly.
    #[serde(default)]
    pub recency_weight: Option<f32>,
    /// Optional graph coupling for recall (SECOND_BRAIN_SPEC §5). The
    /// namespace's brain is the namespace itself: edges live in
    /// `.xerj-memory-{ns}-edges`. Absent → behavior is bit-identical to a
    /// graph-less recall.
    #[serde(default)]
    pub graph: Option<GraphRecallOpts>,
    /// Valid-time cut (#1792): return only memories whose `[valid_from,
    /// valid_to)` window contains this instant. Epoch-ms number or RFC3339
    /// string; default now. Memories stored without bounds are always valid.
    /// Also the default for `graph.as_of` when that is absent.
    #[serde(default)]
    pub as_of: Option<Value>,
}

/// Graph coupling options for `_recall` (SECOND_BRAIN_SPEC §5).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphRecallOpts {
    /// "restrict" | "blend" (required). `restrict` = recall only within reach
    /// of the seeds; `blend` = let graph proximity pull related memories up.
    pub mode: String,
    /// Seed node ids. REQUIRED for restrict (400 if absent/empty).
    /// Optional for blend (default: ids of the top-5 base-recall hits).
    #[serde(default)]
    pub seeds: Option<Vec<String>>,
    /// 1 (default) or 2.
    #[serde(default)]
    pub hops: Option<u8>,
    /// Edge-type allowlist.
    #[serde(default)]
    pub types: Option<Vec<String>>,
    /// Blend weight in [0,1], default 0.3. Ignored for restrict.
    #[serde(default)]
    pub weight: Option<f32>,
    /// Bi-temporal cut, default now. Epoch-ms number or RFC3339 string.
    #[serde(default)]
    pub as_of: Option<Value>,
}

/// Hard ceiling on the reachable-id set folded into a `restrict` filter (the
/// `ids` query stays a bounded doc-values prefilter, not an unbounded OR).
const MAX_RESTRICT_IDS: usize = 10_000;

/// `POST /_memory/{namespace}/_recall` — recall the top-k relevant memories.
pub async fn recall(
    State(state): State<AppState>,
    Path(namespace): Path<String>,
    principal: Principal,
    body: OptionalJson<RecallBody>,
) -> Response {
    if let Err(reason) = validate_namespace(&namespace) {
        return error_response(StatusCode::BAD_REQUEST, reason);
    }
    if let Err(denied) =
        authz::authorize_memory_namespace(&principal, &namespace, Privilege::ReadIndex)
    {
        return denied;
    }
    // Recall must say what to recall BY: exactly one of `vector` (a query
    // embedding) or `query` (text).  A missing body, both-at-once, or an
    // empty `query` are hard 400s — the old lenient fallback degraded every
    // malformed request to match-all and handed back arbitrary memories at
    // score 1.0.  (Unknown keys are already rejected at deserialization via
    // `deny_unknown_fields`.)  Recent-memory listing lives at
    // `GET /_memory/{namespace}`, which needs no query.
    let Some(mut body) = body.0 else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "recall requires a JSON body with exactly one of `vector` (query \
             embedding) or `query` (text); list recent memories with GET \
             /_memory/{namespace} instead",
        );
    };
    match (body.vector.is_some(), body.query.is_some()) {
        (true, true) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "supply exactly one of `vector` or `query`, not both",
            );
        }
        (false, false) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "recall requires exactly one of `vector` (query embedding) or \
                 `query` (text); list recent memories with GET \
                 /_memory/{namespace} instead",
            );
        }
        _ => {}
    }
    if body.vector.is_none() && body.query.as_deref().is_some_and(str::is_empty) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "`query` must be a non-empty string",
        );
    }
    if body.vector.as_deref().is_some_and(<[f32]>::is_empty) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "`vector` must be a non-empty array of numbers",
        );
    }
    // Hybrid recall (#918) fuses the two `query`-driven legs; it cannot take a
    // caller-supplied `vector` (nothing lexical to fuse it with) and it already
    // contains the semantic leg, so both combinations are refused rather than
    // silently resolved to one mode.
    let hybrid = body.hybrid == Some(true);
    if hybrid && body.vector.is_some() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "hybrid recall fuses BM25 and server-side semantic recall over \
             `query`; it cannot take a caller-supplied `vector`",
        );
    }
    if hybrid && body.semantic == Some(true) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "`hybrid: true` already runs the semantic leg; drop `semantic`",
        );
    }
    let fusion = match body.fusion.as_deref() {
        None => "rrf",
        Some(_) if !hybrid => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "`fusion` only applies with `hybrid: true`",
            );
        }
        Some("rrf") => "rrf",
        Some("linear") => "linear",
        // Mirror the `hybrid` query type: learned fusion is not implemented,
        // and substituting RRF would misrepresent the ranking asked for.
        Some("learned") => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "hybrid fusion learned is not yet supported; use rrf or linear",
            );
        }
        Some(other) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!("unknown hybrid fusion strategy `{other}`; use rrf or linear"),
            );
        }
    };
    let k = body.k.unwrap_or(DEFAULT_K).max(1);
    // Recency blending needs a wider candidate pool than the final k so the
    // re-rank can actually promote recent-but-slightly-less-relevant memories.
    let recency_weight = body.recency_weight.map(|w| w.clamp(0.0, 1.0));
    // Optional graph coupling (SECOND_BRAIN_SPEC §5): validate up front so a
    // bad mode / out-of-bounds hops fails fast (with the §4.6 wording), before
    // any search runs.
    let as_of_explicit = match parse_time_bound(body.as_of.as_ref(), "as_of") {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let as_of_ms = as_of_explicit.unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let graph_ctx = match parse_graph_opts(body.graph.as_ref(), as_of_explicit) {
        Ok(g) => g,
        Err(resp) => return *resp,
    };
    let fetch = match recency_weight {
        Some(_) => (k * 4).max(50),
        None => k,
    };
    // Graph blend also needs a wider pool than k: proximity can only promote
    // candidates that were fetched (same over-fetch convention as recency).
    let fetch = match &graph_ctx {
        Some(g) if !g.restrict => fetch.max((k * 4).max(50)),
        _ => fetch,
    };
    let index = backing_index(&namespace);
    let edges_index_name = format!("{MEMORY_PREFIX}{namespace}-edges");
    // Graph coupling reads the brain's EDGES, a different resource from the
    // namespace itself — a credential granted only the notes must not read the
    // link graph through `graph:` (issue #79). Authorized only when coupling is
    // actually requested, so ungrouped recall is unaffected.
    if graph_ctx.is_some() {
        if let Err(denied) =
            authz::authorize_index(&principal, &edges_index_name, Privilege::ReadIndex)
        {
            return denied;
        }
    }
    let caller_filter = body.filter.take();

    // Graph restrict: expand the seeds FIRST and fold the reachable set into
    // the metadata filter as an `ids` clause — recall itself is unchanged, it
    // just runs over a graph-bounded universe. A missing edges index is NOT
    // an error: recall proceeds ungated with the honesty flagged in-band (an
    // agent must be able to opt into graph recall before its first link
    // exists).
    let mut restrict_expansion: Option<GraphExpansion> = None;
    let mut filter_opt: Option<Value> = match &graph_ctx {
        Some(g) if g.restrict => {
            let seeds = g.seeds.clone().unwrap_or_default(); // validated non-empty
            match expand_graph(&state, &edges_index_name, &seeds, g) {
                Err(resp) => return *resp,
                Ok(None) => caller_filter,
                Ok(Some(exp)) => {
                    let ids = json!({ "ids": { "values": &exp.reachable } });
                    let folded = match caller_filter {
                        Some(f) => json!({ "bool": { "filter": [f, ids] } }),
                        None => json!({ "bool": { "filter": [ids] } }),
                    };
                    restrict_expansion = Some(exp);
                    Some(folded)
                }
            }
        }
        _ => caller_filter,
    };

    // Unknown namespace → no memories (also enforces isolation cleanly). The
    // graph object stays honest even here: the edges index can exist without
    // the memory index (a brain linked before its first memory was stored),
    // so `no_edges_index` is only claimed when the edges index truly is
    // absent — for blend with explicit seeds that means actually expanding.
    if !index_exists(&state, &index) {
        let mut resp = json!({ "hits": [], "namespace": namespace });
        if as_of_explicit.is_some() {
            resp["as_of"] = json!(as_of_ms);
        }
        if let Some(g) = &graph_ctx {
            let seeds = g.seeds.clone().unwrap_or_default();
            let expansion = if g.restrict {
                restrict_expansion
            } else if seeds.is_empty() {
                if index_exists(&state, &edges_index_name) {
                    Some(GraphExpansion::default())
                } else {
                    None
                }
            } else {
                match expand_graph(&state, &edges_index_name, &seeds, g) {
                    Err(resp) => return *resp,
                    Ok(e) => e,
                }
            };
            resp["graph"] = graph_obj(g, &seeds, expansion.as_ref());
        }
        return Json(resp).into_response();
    }

    // #1792: valid-time cut. Folded in as one more `filter` clause so every
    // recall mode (kNN, hybrid, semantic, BM25) narrows the same way.
    let validity_applied = schema_has_validity(&state, &index).await;
    if validity_applied {
        let vf = validity_filter(as_of_ms);
        filter_opt = Some(match filter_opt {
            Some(f) => json!({ "bool": { "filter": [f, vf] } }),
            None => vf,
        });
    }

    // Build the search body, reusing the proven kNN / BM25 / filter paths.
    let mut search_body = EsSearchBody {
        size: fetch,
        ..Default::default()
    };

    if let Some(vec) = body.vector {
        // (1) Caller-supplied embedding → pure kNN over the stored `vector`.
        let knn = json!({ "field": "vector", "query_vector": vec, "k": fetch });
        match filter_opt {
            Some(filter) => {
                // kNN with a metadata pre-filter: express kNN as a bool `must`
                // clause so the `filter` narrows the candidate set.
                search_body.query = Some(json!({
                    "bool": { "must": [ { "knn": knn } ], "filter": [ filter ] }
                }));
            }
            None => {
                // Pure kNN: use the top-level `knn` (ES 8.x) path.
                search_body.knn = Some(knn);
            }
        }
    } else if hybrid {
        // (2) Hybrid recall (#918): BM25 and server-side semantic recall over
        // the same `query`, run as the two legs of the engine's `hybrid` query
        // and fused by rank (RRF, k=60) or by min-max-normalised score
        // (`linear`). Each leg keeps the filter shape its single-mode
        // counterpart uses: the BM25 leg wraps in `bool.must` + `bool.filter`,
        // the semantic leg carries `filter` inside the `semantic` node —
        // a `semantic` clause nested under a `bool` is not dispatched to the
        // vector path (#395), whereas `hybrid.queries` are dispatched leg by
        // leg (`dispatching_vector_fields`). Both legs fetch `fetch` (the
        // recency/graph over-fetch width, not the caller's `k`), so the fused
        // pool the re-rankers see is as deep as it is in single mode.
        let q = body.query.as_deref().unwrap_or_default(); // validated non-empty above
        let lexical = match &filter_opt {
            Some(filter) => json!({
                "bool": { "must": [ { "match": { "text": q } } ], "filter": [ filter ] }
            }),
            None => json!({ "match": { "text": q } }),
        };
        let mut semantic = json!({ "field": "text", "query": q, "k": fetch });
        if let Some(filter) = filter_opt {
            semantic["filter"] = filter;
        }
        search_body.query = Some(json!({
            "hybrid": {
                "queries": [ { "query": lexical }, { "query": { "semantic": semantic } } ],
                "fusion": fusion
            }
        }));
    } else if body.semantic == Some(true) {
        // (3) Server-side semantic recall: embed `query` with the same embedder
        // used at store time (via the `semantic` query over the `text`
        // semantic_text field) and recall by vector similarity. No client-side
        // embedding required.
        let q = match body.query.as_deref() {
            Some(q) if !q.is_empty() => q,
            _ => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "semantic recall requires a non-empty `query` to embed",
                );
            }
        };
        // The `semantic` query carries its own `filter` (applied as a kNN
        // pre-filter inside the engine), so pass the metadata filter there
        // rather than wrapping in a `bool` — a `semantic` node nested in a
        // `bool` is not dispatched to the vector path.
        let mut semantic = json!({ "field": "text", "query": q, "k": fetch });
        if let Some(filter) = filter_opt {
            semantic["filter"] = filter;
        }
        search_body.query = Some(json!({ "semantic": semantic }));
    } else {
        // (4) Text recall (BM25). Empty/absent query → match_all (recent memories).
        let inner = match body.query.as_deref() {
            Some(q) if !q.is_empty() => json!({ "match": { "text": q } }),
            _ => json!({ "match_all": {} }),
        };
        search_body.query = Some(match filter_opt {
            Some(filter) => json!({ "bool": { "must": [ inner ], "filter": [ filter ] } }),
            None => inner,
        });
    }

    let resp = es_compat::search(
        State(state.clone()),
        Path(index.clone()),
        axum::extract::Query(EsSearchQueryParams::default()),
        principal.clone(),
        EsSearchJson(Some(search_body)),
    )
    .await
    .into_response();

    let (status, response) = drain_json(resp).await;
    if !status.is_success() {
        return error_response(status, format!("recall failed: {response}"));
    }

    let (hits, graph_out) = match &graph_ctx {
        // No graph coupling → bit-identical to the graph-less recall.
        None => {
            let hits = match recency_weight {
                Some(w) => blend_recency(&response, w, k),
                None => extract_hits(&response),
            };
            (hits, None)
        }
        // restrict: the search already ran over the graph-bounded universe;
        // hit shaping is unchanged, the response just gains the graph object.
        Some(g) if g.restrict => {
            let hits = match recency_weight {
                Some(w) => blend_recency(&response, w, k),
                None => extract_hits(&response),
            };
            let seeds = g.seeds.clone().unwrap_or_default();
            (
                hits,
                Some(graph_obj(g, &seeds, restrict_expansion.as_ref())),
            )
        }
        // blend: expand (seeds given, or the top-5 base hits), then re-rank
        // the fetched candidates by (1-w)·norm_score + w·proximity. The graph
        // blend applies FIRST, then the existing recency blend over the
        // graph-blended ordering — each re-rank is order+truncate only, so
        // they compose.
        Some(g) => {
            let raw: Vec<&Value> = response
                .pointer("/hits/hits")
                .and_then(Value::as_array)
                .map(|a| a.iter().collect())
                .unwrap_or_default();
            let seeds: Vec<String> = match &g.seeds {
                Some(s) if !s.is_empty() => s.clone(),
                _ => raw
                    .iter()
                    .take(5)
                    .filter_map(|h| h.get("_id").and_then(Value::as_str))
                    .map(String::from)
                    .collect(),
            };
            let expansion = if seeds.is_empty() {
                // No seeds derivable (empty base recall): nothing to expand.
                // Distinguish "no edges index" (flagged) from "no seeds".
                if index_exists(&state, &edges_index_name) {
                    Some(GraphExpansion::default())
                } else {
                    None
                }
            } else {
                match expand_graph(&state, &edges_index_name, &seeds, g) {
                    Err(resp) => return *resp,
                    Ok(e) => e,
                }
            };
            let graph_ranked: Vec<&Value> = match &expansion {
                Some(exp) => rank_by_proximity(&raw, &exp.proximity, g.weight, k),
                // No edges index → recall proceeds ungated (base order).
                None => raw.clone(),
            };
            let final_ranked: Vec<&Value> = match recency_weight {
                Some(w) => rank_by_recency(&graph_ranked, f64::from(w), k),
                None => graph_ranked.into_iter().take(k).collect(),
            };
            let hits: Vec<Value> = final_ranked
                .into_iter()
                .map(|h| {
                    let mut m = map_hit(h);
                    let id = h.get("_id").and_then(Value::as_str).unwrap_or("");
                    let prox = expansion
                        .as_ref()
                        .and_then(|e| e.proximity.get(id))
                        .copied()
                        .unwrap_or(0.0);
                    m["graph_proximity"] = json!(prox);
                    m
                })
                .collect();
            (hits, Some(graph_obj(g, &seeds, expansion.as_ref())))
        }
    };
    let mut resp = json!({ "hits": hits, "namespace": namespace });
    if validity_applied || as_of_explicit.is_some() {
        resp["as_of"] = json!(as_of_ms);
    }
    if let Some(gobj) = graph_out {
        resp["graph"] = gobj;
    }
    Json(resp).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Graph coupling (SECOND_BRAIN_SPEC §5)
// ─────────────────────────────────────────────────────────────────────────────

/// Parsed + validated `graph` recall options.
struct GraphCtx {
    /// true = restrict, false = blend.
    restrict: bool,
    seeds: Option<Vec<String>>,
    hops: u8,
    types: Option<Vec<String>>,
    /// Blend weight in [0,1] (ignored for restrict).
    weight: f64,
    as_of_ms: i64,
}

/// One completed expansion over the namespace's edges index.
#[derive(Default)]
struct GraphExpansion {
    /// Reachable node ids (seeds first), clipped to [`MAX_RESTRICT_IDS`].
    reachable: Vec<String>,
    /// node id → graph proximity in [0,1]: seeds = 1.0, a node reached at hop
    /// h via edge e = max over paths of `0.5^h * clamp(e.weight, 0, 1)`.
    proximity: std::collections::HashMap<String, f64>,
    stats: xerj_engine::graph::GraphExpandStats,
    reachable_clipped: u64,
}

/// Validate the `graph` options (400s carry the memory_error shape — this
/// endpoint keeps its existing error type; the hops bound carries the §4.6
/// not-a-graph-database wording).
fn parse_graph_opts(
    opts: Option<&GraphRecallOpts>,
    default_as_of: Option<i64>,
) -> Result<Option<GraphCtx>, Box<Response>> {
    let Some(o) = opts else { return Ok(None) };
    let restrict = match o.mode.as_str() {
        "restrict" => true,
        "blend" => false,
        other => {
            return Err(Box::new(error_response(
                StatusCode::BAD_REQUEST,
                format!("graph.mode must be 'restrict' or 'blend' (got '{other}')"),
            )));
        }
    };
    if restrict && o.seeds.as_deref().is_none_or(<[String]>::is_empty) {
        return Err(Box::new(error_response(
            StatusCode::BAD_REQUEST,
            "graph.mode 'restrict' requires non-empty graph.seeds",
        )));
    }
    let hops = o.hops.unwrap_or(1);
    if hops == 0 || hops > 2 {
        return Err(Box::new(error_response(
            StatusCode::BAD_REQUEST,
            xerj_engine::graph::GRAPH_HOPS_CAP_REASON,
        )));
    }
    let as_of_ms = match &o.as_of {
        None => default_as_of.unwrap_or_else(|| chrono::Utc::now().timestamp_millis()),
        Some(v) => match parse_epoch_ms(v) {
            Some(ms) => ms,
            None => {
                return Err(Box::new(error_response(
                    StatusCode::BAD_REQUEST,
                    "graph.as_of must be an epoch-ms number or an RFC3339 string",
                )));
            }
        },
    };
    Ok(Some(GraphCtx {
        restrict,
        seeds: o.seeds.clone(),
        hops,
        types: o.types.clone(),
        weight: f64::from(o.weight.unwrap_or(0.3)).clamp(0.0, 1.0),
        as_of_ms,
    }))
}

/// Epoch-ms from a JSON number or an RFC3339 string (inputs accept both;
/// outputs are always numbers).
fn parse_epoch_ms(v: &Value) -> Option<i64> {
    match v {
        Value::Number(_) => v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)),
        Value::String(s) => {
            if let Ok(n) = s.parse::<i64>() {
                return Some(n);
            }
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|dt| dt.timestamp_millis())
        }
        _ => None,
    }
}

/// Expand `seeds` over the namespace's edges index (direction Both, no
/// expired edges). `Ok(None)` = the edges index does not exist — the caller
/// proceeds ungated with the honesty flagged, per §5.
fn expand_graph(
    state: &AppState,
    edges_index: &str,
    seeds: &[String],
    g: &GraphCtx,
) -> Result<Option<GraphExpansion>, Box<Response>> {
    use xerj_engine::graph::{GraphDirection, GraphExpandRequest};
    let Ok(idx) = state.engine.get_index(edges_index) else {
        return Ok(None);
    };
    let req = GraphExpandRequest {
        frontier: seeds.to_vec(),
        hops: g.hops,
        direction: GraphDirection::Both,
        types: g.types.clone(),
        as_of_ms: g.as_of_ms,
        include_expired: false,
        max_result_edges: MAX_RESTRICT_IDS,
    };
    let res = match idx.graph_expand(&req) {
        Ok(r) => r,
        Err(e) => {
            return Err(Box::new(error_response(
                StatusCode::BAD_REQUEST,
                e.to_string(),
            )))
        }
    };
    // Proximity: seeds anchor at 1.0; every edge contributes
    // `0.5^hop * clamp(weight, 0, 1)` to both endpoints, max over paths
    // (deterministic; a seed can never be demoted since 0.5^h·w ≤ 0.5 < 1).
    let mut proximity: std::collections::HashMap<String, f64> =
        seeds.iter().map(|s| (s.clone(), 1.0)).collect();
    for e in &res.edges {
        let p = 0.5f64.powi(i32::from(e.hop)) * e.weight.clamp(0.0, 1.0);
        for id in [&e.src, &e.dst] {
            let entry = proximity.entry(id.clone()).or_insert(0.0);
            if p > *entry {
                *entry = p;
            }
        }
    }
    let mut reachable = res.reachable;
    let mut reachable_clipped = 0u64;
    if reachable.len() > MAX_RESTRICT_IDS {
        reachable_clipped = (reachable.len() - MAX_RESTRICT_IDS) as u64;
        reachable.truncate(MAX_RESTRICT_IDS);
    }
    Ok(Some(GraphExpansion {
        reachable,
        proximity,
        stats: res.stats,
        reachable_clipped,
    }))
}

/// The top-level `graph` response object (§5). `exp = None` means the edges
/// index did not exist — reported in-band as `no_edges_index`, never hidden.
fn graph_obj(g: &GraphCtx, seeds: &[String], exp: Option<&GraphExpansion>) -> Value {
    let mode = if g.restrict { "restrict" } else { "blend" };
    let not_shown = match exp {
        Some(e) => json!({
            "frontier_clipped": e.stats.frontier_clipped,
            "edges_clipped": e.stats.edges_clipped,
            "expired_excluded": e.stats.expired_excluded,
            "type_filtered": e.stats.type_filtered,
            "segments_without_columns": e.stats.segments_without_columns,
            "reachable_clipped": e.reachable_clipped,
        }),
        None => json!({
            "frontier_clipped": 0,
            "edges_clipped": 0,
            "expired_excluded": 0,
            "type_filtered": 0,
            "segments_without_columns": 0,
            "reachable_clipped": 0,
            "no_edges_index": true,
        }),
    };
    json!({
        "mode": mode,
        "seeds": seeds,
        "hops": g.hops,
        "as_of": g.as_of_ms,
        "reachable": exp.map(|e| e.reachable.len()).unwrap_or(0),
        "not_shown": not_shown,
    })
}

/// Map a single ES search hit into the agent-memory hit shape.
fn map_hit(h: &Value) -> Value {
    let src = h.get("_source");
    let mut out = json!({
        "id": h.get("_id").cloned().unwrap_or(Value::Null),
        "text": src.and_then(|s| s.get("text")).cloned().unwrap_or(Value::Null),
        "metadata": src
            .and_then(|s| s.get("metadata"))
            .cloned()
            .unwrap_or(Value::Null),
        "score": h.get("_score").cloned().unwrap_or(Value::Null),
    });
    // #1792: validity fields only appear on records that carry them, so
    // hits from validity-free namespaces keep their old shape.
    for key in ["valid_from", "valid_to", "recorded_at"] {
        if let Some(v) = src.and_then(|s| s.get(key)) {
            out[key] = v.clone();
        }
    }
    out
}

/// Map an ES search response into the agent-memory hit shape.
fn extract_hits(search_response: &Value) -> Vec<Value> {
    search_response
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(map_hit).collect())
        .unwrap_or_default()
}

/// Re-rank recall hits by a recency-blended score and truncate to `k`.
///
/// For each candidate, `blended = (1 - w) * norm_score + w * norm_recency`,
/// where `norm_score` and `norm_recency` are min-max normalized across the
/// candidate set (relevance `_score` and `stored_at` respectively). `w = 0`
/// reduces to pure relevance order; `w = 1` to pure recency. The returned hits
/// keep the original relevance `_score` in their `score` field — only the order
/// (and truncation to `k`) reflects the blend.
fn blend_recency(search_response: &Value, w: f32, k: usize) -> Vec<Value> {
    let hits = match search_response
        .pointer("/hits/hits")
        .and_then(Value::as_array)
    {
        Some(h) if !h.is_empty() => h,
        _ => return Vec::new(),
    };
    let refs: Vec<&Value> = hits.iter().collect();
    rank_by_recency(&refs, f64::from(w), k)
        .into_iter()
        .map(map_hit)
        .collect()
}

/// Min-max normalize a value against a range; a uniform range (hi == lo)
/// normalizes to 0.0 so that dimension contributes nothing to a blend rather
/// than dividing by zero.
fn min_max_norm(v: f64, lo: f64, hi: f64) -> f64 {
    if hi > lo {
        (v - lo) / (hi - lo)
    } else {
        0.0
    }
}

/// Order+truncate re-rank by `blended = (1 - w)·norm_score + w·norm_recency`
/// (both min-max normalized across the candidate set). Returns raw hit refs
/// so re-ranks compose — the recall handler applies this AFTER the graph
/// blend when both are requested.
fn rank_by_recency<'a>(hits: &[&'a Value], w: f64, k: usize) -> Vec<&'a Value> {
    if hits.is_empty() {
        return Vec::new();
    }
    let scores: Vec<f64> = hits
        .iter()
        .map(|h| h.get("_score").and_then(Value::as_f64).unwrap_or(0.0))
        .collect();
    let times: Vec<f64> = hits
        .iter()
        .map(|h| {
            h.pointer("/_source/stored_at")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
        })
        .collect();
    let smin = scores.iter().copied().fold(f64::INFINITY, f64::min);
    let smax = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let tmin = times.iter().copied().fold(f64::INFINITY, f64::min);
    let tmax = times.iter().copied().fold(f64::NEG_INFINITY, f64::max);

    let mut ranked: Vec<(f64, &Value)> = hits
        .iter()
        .zip(scores.iter().zip(times.iter()))
        .map(|(h, (&sc, &tm))| {
            let blended =
                (1.0 - w) * min_max_norm(sc, smin, smax) + w * min_max_norm(tm, tmin, tmax);
            (blended, *h)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    ranked.into_iter().take(k).map(|(_, h)| h).collect()
}

/// Order+truncate re-rank by `blended = (1 - w)·norm_score + w·proximity`
/// (SECOND_BRAIN_SPEC §5 blend). `norm_score` uses the same min-max
/// normalization as [`rank_by_recency`]; proximity is already an absolute
/// [0,1] quantity (seed = 1.0, hop-decayed otherwise) and is deliberately NOT
/// re-normalized. Hits absent from the proximity map contribute 0.
fn rank_by_proximity<'a>(
    hits: &[&'a Value],
    proximity: &std::collections::HashMap<String, f64>,
    w: f64,
    k: usize,
) -> Vec<&'a Value> {
    if hits.is_empty() {
        return Vec::new();
    }
    let scores: Vec<f64> = hits
        .iter()
        .map(|h| h.get("_score").and_then(Value::as_f64).unwrap_or(0.0))
        .collect();
    let smin = scores.iter().copied().fold(f64::INFINITY, f64::min);
    let smax = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);

    let mut ranked: Vec<(f64, &Value)> = hits
        .iter()
        .zip(scores.iter())
        .map(|(h, &sc)| {
            let prox = h
                .get("_id")
                .and_then(Value::as_str)
                .and_then(|id| proximity.get(id))
                .copied()
                .unwrap_or(0.0);
            let blended = (1.0 - w) * min_max_norm(sc, smin, smax) + w * prox;
            (blended, *h)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    ranked.into_iter().take(k).map(|(_, h)| h).collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// List / introspect
// ─────────────────────────────────────────────────────────────────────────────

/// Query params for the paged `list` endpoint (item 12).
///
/// Deliberately lenient on unknown params (no `deny_unknown_fields`): the
/// previous handler took no query at all and ignored everything, so a client
/// appending a benign ES-ism like `?pretty` / `?human` / `?format=json` must
/// keep working. Only `from` / `after` / `size` are interpreted.
#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    /// Zero-based offset of the first entry to return. Default 0.
    #[serde(default)]
    pub from: Option<usize>,
    /// Alias for `from` (an `after`-style cursor). `from` wins if both given.
    #[serde(default)]
    pub after: Option<usize>,
    /// Page size. Default [`LIST_LIMIT`] (100), capped at [`MAX_LIST_SIZE`].
    #[serde(default)]
    pub size: Option<usize>,
}

/// `GET /_memory/{namespace}` — count + a page of most-recent entries.
///
/// Pages with `from`/`size` (item 12); see the module-level docs for the
/// pagination and (no-per-namespace) authorization model.
pub async fn list(
    State(state): State<AppState>,
    Path(namespace): Path<String>,
    principal: Principal,
    axum::extract::Query(params): axum::extract::Query<ListParams>,
) -> Response {
    if let Err(reason) = validate_namespace(&namespace) {
        return error_response(StatusCode::BAD_REQUEST, reason);
    }
    if let Err(denied) =
        authz::authorize_memory_namespace(&principal, &namespace, Privilege::ReadIndex)
    {
        return denied;
    }
    // `from`/`after` are aliases (offset cursor); `size` is clamped to
    // [1, MAX_LIST_SIZE] so a caller can neither page from a negative offset
    // nor pull an unbounded set in one call.
    let from = params.from.or(params.after).unwrap_or(0);
    let size = params.size.unwrap_or(LIST_LIMIT).clamp(1, MAX_LIST_SIZE);

    let index = backing_index(&namespace);
    if !index_exists(&state, &index) {
        return Json(json!({
            "namespace": namespace,
            "exists": false,
            "count": 0,
            "entries": [],
            "from": from,
            "size": size,
            "next": Value::Null,
        }))
        .into_response();
    }

    let search_body = EsSearchBody {
        query: Some(json!({ "match_all": {} })),
        from,
        size,
        // Recent-first. `unmapped_type` guards a namespace whose entries were
        // all created before `stored_at` existed (defensive; we always store it).
        sort: Some(json!([{ "stored_at": { "order": "desc", "unmapped_type": "long" } }])),
        track_total_hits: Some(json!(true)),
        ..Default::default()
    };

    let resp = es_compat::search(
        State(state.clone()),
        Path(index.clone()),
        axum::extract::Query(EsSearchQueryParams::default()),
        principal.clone(),
        EsSearchJson(Some(search_body)),
    )
    .await
    .into_response();

    let (status, body) = drain_json(resp).await;
    if !status.is_success() {
        return error_response(status, format!("list failed: {body}"));
    }

    let count = body
        .pointer("/hits/total/value")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let entries = extract_hits(&body);
    // `next` cursor: present only when more entries remain past this page.
    let next = if (from as u64) + (entries.len() as u64) < count {
        json!({ "from": from + entries.len(), "size": size })
    } else {
        Value::Null
    };
    Json(json!({
        "namespace": namespace,
        "exists": true,
        "count": count,
        "entries": entries,
        "from": from,
        "size": size,
        "next": next,
    }))
    .into_response()
}

/// `GET /_memory` — list agent-memory namespaces.
///
/// Enumerates all memory namespaces (indices named `.xerj-memory-{namespace}`),
/// returning their active document count and whether a corresponding graph
/// edges index (`.xerj-memory-{namespace}-edges`) exists.
///
/// Filtered to only the namespaces the calling principal holds
/// `Privilege::ReadIndex` on.
pub async fn list_namespaces(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Query(params): axum::extract::Query<ListParams>,
) -> Response {
    let from = params.from.or(params.after).unwrap_or(0);
    let size = params.size.unwrap_or(LIST_LIMIT).clamp(1, MAX_LIST_SIZE);

    let all_indices = state.engine.list_indices().await;

    // Collect all edges indices so we can flag `graph: true` in O(1).
    let mut edges = std::collections::HashSet::new();
    for idx in &all_indices {
        if let Some(rest) = idx.name.strip_prefix(MEMORY_PREFIX) {
            if let Some(brain) = rest.strip_suffix("-edges") {
                edges.insert(brain.to_string());
            }
        }
    }

    let mut namespaces = Vec::new();
    for idx in &all_indices {
        if let Some(ns) = idx.name.strip_prefix(MEMORY_PREFIX) {
            // Edges indices are graph metadata, not memory namespaces.
            if ns.ends_with("-edges") {
                continue;
            }
            if validate_namespace(ns).is_err() {
                continue;
            }
            if authz::authorize_memory_namespace(&principal, ns, Privilege::ReadIndex).is_err() {
                continue;
            }
            let has_graph = edges.contains(ns);
            namespaces.push(json!({
                "namespace": ns,
                "count": idx.doc_count,
                "graph": has_graph,
            }));
        }
    }

    // Sort deterministically by namespace name.
    namespaces.sort_by(|a, b| {
        let a_ns = a.get("namespace").and_then(Value::as_str).unwrap_or("");
        let b_ns = b.get("namespace").and_then(Value::as_str).unwrap_or("");
        a_ns.cmp(b_ns)
    });

    let total = namespaces.len();
    let paged: Vec<Value> = namespaces.into_iter().skip(from).take(size).collect();
    let next = if from + paged.len() < total {
        json!({ "from": from + paged.len(), "size": size })
    } else {
        Value::Null
    };

    Json(json!({
        "count": total,
        "namespaces": paged,
        "from": from,
        "size": size,
        "next": next,
    }))
    .into_response()
}

/// JSON `405` for unsupported mutating verbs on bare `/_memory`.
pub async fn unsupported_memory_bare() -> Response {
    let reason = "/_memory only supports GET to list namespaces. \
                  Specify a namespace in the path (e.g. POST /_memory/{namespace}) \
                  to store, recall, list, or drop memories.";
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, HeaderValue::from_static("GET"))],
        Json(json!({
            "error": { "type": "memory_error", "reason": reason },
            "status": 405,
        })),
    )
        .into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Forget
// ─────────────────────────────────────────────────────────────────────────────

/// JSON `405` for a memory path that is not a write or a read.
///
/// The Elasticsearch document routes (`POST /{index}/_doc`,
/// `PUT /{index}/_doc/{id}`, `POST /{index}/_search`) land here when an agent
/// copies them onto `/_memory`. An empty `405` costs a round trip; the body
/// names the four calls this API actually has.
///
/// `allow` is the `Allow` header for the path that was hit: `DELETE` on
/// `/_memory/{namespace}/{id}` (forget), empty on `/_memory/{namespace}/_doc/{id}`,
/// where no method is served.
fn memory_method_not_allowed(namespace: &str, allow: &'static str) -> Response {
    let reason = format!(
        "/_memory/{namespace} is the agent-memory API, not the document API. \
         Store with POST /_memory/{namespace} and a JSON body {{\"text\":\"...\"}}. \
         Recall with POST /_memory/{namespace}/_recall and {{\"query\":\"...\"}}. \
         List with GET /_memory/{namespace}. \
         Forget one entry with DELETE /_memory/{namespace}/{{id}}."
    );
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, HeaderValue::from_static(allow))],
        Json(json!({
            "error": { "type": "memory_error", "reason": reason },
            "status": 405,
        })),
    )
        .into_response()
}

/// `GET`/`POST`/`PUT /_memory/{namespace}/{id}` — that path only forgets.
pub async fn unsupported_memory_verb(Path((namespace, _id)): Path<(String, String)>) -> Response {
    memory_method_not_allowed(&namespace, "DELETE")
}

/// `PUT`/`POST`/`GET`/`DELETE /_memory/{namespace}/_doc/{id}` — the document
/// route with an id. No memory id lives at this path.
pub async fn unsupported_memory_doc_id(Path((namespace, _id)): Path<(String, String)>) -> Response {
    memory_method_not_allowed(&namespace, "")
}

/// `DELETE /_memory/{namespace}/{id}` — forget a single memory entry.
pub async fn forget_one(
    State(state): State<AppState>,
    Path((namespace, id)): Path<(String, String)>,
    principal: Principal,
) -> Response {
    if let Err(reason) = validate_namespace(&namespace) {
        return error_response(StatusCode::BAD_REQUEST, reason);
    }
    if let Err(denied) =
        authz::authorize_memory_namespace(&principal, &namespace, Privilege::WriteIndex)
    {
        return denied;
    }
    let index = backing_index(&namespace);
    if !index_exists(&state, &index) {
        return error_response(
            StatusCode::NOT_FOUND,
            format!("memory namespace '{namespace}' does not exist"),
        );
    }

    let resp = es_compat::delete_doc(
        State(state.clone()),
        Path((index.clone(), id.clone())),
        axum::extract::Query(DeleteDocParams::default()),
    )
    .await
    .into_response();

    let (status, body) = drain_json(resp).await;
    if status == StatusCode::NOT_FOUND {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "id": id, "namespace": namespace, "forgotten": false })),
        )
            .into_response();
    }
    if !status.is_success() {
        return memory_failure(
            status,
            &body,
            &state.config.server.data_dir,
            format!("forget failed: {body}"),
        );
    }
    Json(json!({ "id": id, "namespace": namespace, "forgotten": true })).into_response()
}

/// `DELETE /_memory/{namespace}` — drop an entire namespace.
pub async fn forget_namespace(
    State(state): State<AppState>,
    Path(namespace): Path<String>,
    principal: Principal,
) -> Response {
    if let Err(reason) = validate_namespace(&namespace) {
        return error_response(StatusCode::BAD_REQUEST, reason);
    }
    if let Err(denied) =
        authz::authorize_memory_namespace(&principal, &namespace, Privilege::AdminIndex)
    {
        return denied;
    }
    let index = backing_index(&namespace);
    if !index_exists(&state, &index) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "namespace": namespace, "dropped": false })),
        )
            .into_response();
    }

    let resp = es_compat::delete_index(
        State(state.clone()),
        Path(index.clone()),
        axum::extract::Query(DeleteIndexParams::default()),
    )
    .await
    .into_response();

    let (status, body) = drain_json(resp).await;
    if !status.is_success() {
        return error_response(status, format!("drop namespace failed: {body}"));
    }
    Json(json!({ "namespace": namespace, "dropped": true })).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use xerj_common::{config::Config, metrics::Metrics};
    use xerj_engine::Engine;

    fn test_state() -> AppState {
        let dir = tempfile::tempdir().expect("tempdir");
        // Leak the tempdir so the data directory outlives the test body — the
        // Engine holds it open; cleanup is the OS's problem at process exit.
        let path = dir.keep();
        let mut config = Config::default();
        config.server.data_dir = path.to_str().unwrap().to_string();
        let metrics = Metrics::new().expect("metrics");
        let engine = Engine::new(config.clone()).expect("engine");
        AppState::new(config, engine, metrics)
    }

    async fn store_mem(state: &AppState, ns: &str, body: Value) -> (StatusCode, Value) {
        let b: StoreBody = serde_json::from_value(body).unwrap();
        let resp = store(
            State(state.clone()),
            Path(ns.to_string()),
            Principal::Superuser,
            OptionalJson(Some(b)),
        )
        .await;
        drain_json(resp).await
    }

    async fn recall_mem(state: &AppState, ns: &str, body: Value) -> (StatusCode, Value) {
        let b: RecallBody = serde_json::from_value(body).unwrap();
        let resp = recall(
            State(state.clone()),
            Path(ns.to_string()),
            Principal::Superuser,
            OptionalJson(Some(b)),
        )
        .await;
        drain_json(resp).await
    }

    #[test]
    fn namespace_validation() {
        assert!(validate_namespace("agent1").is_ok());
        assert!(validate_namespace("a.b-c_1").is_ok());
        assert!(validate_namespace("").is_err());
        assert!(validate_namespace("Agent").is_err()); // uppercase
        assert!(validate_namespace("_x").is_err()); // leading underscore
        assert!(validate_namespace("a/b").is_err()); // slash
        assert!(validate_namespace("a..b").is_err()); // path traversal
                                                      // Second-brain collision guard: `POST /_memory/kb-edges` would write
                                                      // memories into brain `kb`'s edge index.
        assert_eq!(
            validate_namespace("kb-edges").unwrap_err(),
            "namespace suffix '-edges' is reserved for graph edge indices"
        );
    }

    /// #2284: the flood-stage block is a nested `cluster_block_exception`
    /// inside `failed to store memory: {json}`. The reason the CLI and MCP
    /// print is the disk sentence, with this process's free space and the
    /// data-dir path. A mapping error stays the raw body.
    #[test]
    fn a_flood_stage_store_failure_names_the_disk_and_the_data_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_string_lossy();
        let inner = crate::error::ApiError::new(xerj_common::XerjError::index_blocked(
            ".xerj-memory-notes",
            "read_only_allow_delete (disk usage 96% exceeded flood-stage watermark [95%])",
        ))
        .into_value();
        let wrapped_body = json!({
            "error": {
                "type": "memory_error",
                "reason": format!("failed to store memory: {inner}")
            },
            "status": 429
        });
        let unused = "failed to store memory: {raw}".to_string();
        let reason = memory_failure_reason(&wrapped_body, &path, &unused);
        assert!(
            reason.contains("the disk holding the data dir is nearly full"),
            "{reason}"
        );
        assert!(reason.contains("96% used"), "{reason}");
        assert!(reason.contains("threshold 95%"), "{reason}");
        assert!(reason.contains(path.as_ref()), "{reason}");
        assert!(
            reason.contains("Free space on that drive, or restart the node with --data-dir"),
            "{reason}"
        );
        assert!(
            reason.contains("limits.disk_flood_stage_percent"),
            "{reason}"
        );
        assert!(!reason.contains("cluster_block_exception"), "{reason}");
        assert!(reason.contains("free "), "{reason}");
        assert!(!reason.contains("free unknown"), "{reason}");

        let other = json!({
            "error": { "type": "mapper_parsing_exception", "reason": "bad field" }
        });
        let wrapped = format!("failed to store memory: {other}");
        assert_eq!(memory_failure_reason(&other, &path, &wrapped), wrapped);
    }

    #[tokio::test]
    async fn store_recall_vector_topk_and_isolation() {
        let state = test_state();

        // Store 3 memories in agent1 with 3-dim vectors.
        let (s, _) = store_mem(
            &state,
            "agent1",
            json!({"text":"cat sat on mat","vector":[1.0,0.0,0.0],"metadata":{"topic":"animals"},"id":"m1"}),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED);
        store_mem(
            &state,
            "agent1",
            json!({"text":"dogs are loyal","vector":[0.0,1.0,0.0],"metadata":{"topic":"animals"},"id":"m2"}),
        )
        .await;
        store_mem(
            &state,
            "agent1",
            json!({"text":"stocks rallied","vector":[0.9,0.1,0.0],"metadata":{"topic":"finance"},"id":"m3"}),
        )
        .await;

        // kNN recall by vector, k=2 → nearest to [1,0,0] are m1 then m3.
        let (s, body) = recall_mem(&state, "agent1", json!({"vector":[1.0,0.0,0.0],"k":2})).await;
        assert_eq!(s, StatusCode::OK);
        let hits = body["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 2, "k=2 must return 2 hits");
        assert_eq!(hits[0]["id"], "m1");
        assert_eq!(hits[1]["id"], "m3");

        // BM25 text recall.
        let (_, body) = recall_mem(&state, "agent1", json!({"query":"cat"})).await;
        let hits = body["hits"].as_array().unwrap();
        assert!(hits.iter().any(|h| h["id"] == "m1"));

        // Metadata filter narrows results (exclude finance).
        let (_, body) = recall_mem(
            &state,
            "agent1",
            json!({"vector":[1.0,0.0,0.0],"k":10,"filter":{"term":{"metadata.topic":"animals"}}}),
        )
        .await;
        let hits = body["hits"].as_array().unwrap();
        assert!(!hits.is_empty());
        assert!(
            hits.iter().all(|h| h["id"] != "m3"),
            "finance memory must be filtered out"
        );

        // Isolation: agent2 recall is empty.
        let (_, body) = recall_mem(&state, "agent2", json!({"vector":[1.0,0.0,0.0],"k":5})).await;
        assert_eq!(body["hits"].as_array().unwrap().len(), 0);

        // Forget m1 → gone.
        let resp = forget_one(
            State(state.clone()),
            Path(("agent1".to_string(), "m1".to_string())),
            Principal::Superuser,
        )
        .await;
        let (s, _) = drain_json(resp).await;
        assert_eq!(s, StatusCode::OK);
        let (_, body) = recall_mem(&state, "agent1", json!({"vector":[1.0,0.0,0.0],"k":10})).await;
        let hits = body["hits"].as_array().unwrap();
        assert!(hits.iter().all(|h| h["id"] != "m1"), "m1 must be forgotten");
    }

    /// #17: server-side semantic recall. Memories are stored text-only (NO
    /// client-side vector); because `text` is a `semantic_text` field the
    /// engine auto-embeds each at store time. `_recall {"semantic": true}` then
    /// embeds the query text with the same built-in embedder and recalls by
    /// vector similarity — no external service, no client-side embedding.
    #[tokio::test]
    async fn store_recall_server_side_semantic() {
        let state = test_state();

        for (id, text) in [
            (
                "i1",
                "host 1.2.3.4 brute forced ssh with hundreds of failed passwords",
            ),
            ("i2", "nightly database backup completed without errors"),
            (
                "i3",
                "repeated ssh authentication failures from an unknown attacker",
            ),
        ] {
            let (s, _) = store_mem(&state, "soc", json!({"text": text, "id": id})).await;
            assert_eq!(
                s,
                StatusCode::CREATED,
                "text-only store must succeed (auto-embedded)"
            );
        }

        // Pass query TEXT + semantic:true, NO vector. The server embeds it.
        let (s, body) = recall_mem(
            &state,
            "soc",
            json!({"query": "ssh brute force attack", "semantic": true, "k": 2}),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let hits = body["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 2, "semantic k=2 → exactly 2 hits");
        let ids: Vec<&str> = hits.iter().map(|h| h["id"].as_str().unwrap()).collect();
        assert!(
            ids.iter().any(|id| *id == "i1" || *id == "i3"),
            "an ssh incident must be recalled semantically, got {ids:?}"
        );
        assert!(
            !ids.contains(&"i2"),
            "the unrelated backup memory must not rank in the top-2, got {ids:?}"
        );

        // semantic:true with no query text → 400 (nothing to embed).
        let (s, _) = recall_mem(&state, "soc", json!({"semantic": true})).await;
        assert_eq!(
            s,
            StatusCode::BAD_REQUEST,
            "semantic recall needs a `query` to embed"
        );

        // A metadata filter still narrows semantic recall.
        store_mem(
            &state,
            "soc",
            json!({"text":"ssh attack from host 9.9.9.9","id":"i4","metadata":{"sev":"high"}}),
        )
        .await;
        let (_, body) = recall_mem(
            &state,
            "soc",
            json!({"query":"ssh attack","semantic":true,"k":10,"filter":{"term":{"metadata.sev":"high"}}}),
        )
        .await;
        let hits = body["hits"].as_array().unwrap();
        assert!(
            !hits.is_empty(),
            "filtered semantic recall returns the high-sev memory"
        );
        assert!(
            hits.iter().all(|h| h["id"] == "i4"),
            "only the high-sev memory passes the filter"
        );
    }

    /// #918: `hybrid: true` runs the BM25 leg and the server-side semantic
    /// leg over the same `query` and fuses them inside `_recall` — no raw
    /// `_search` on the backing index. Memories are stored text-only (the
    /// `semantic_text` field auto-embeds them), exactly as for `semantic`.
    #[tokio::test]
    async fn store_recall_hybrid_fuses_lexical_and_semantic() {
        let state = test_state();
        for (id, text) in [
            (
                "i1",
                "host 1.2.3.4 brute forced ssh with hundreds of failed passwords",
            ),
            ("i2", "nightly database backup completed without errors"),
            (
                "i3",
                "repeated ssh authentication failures from an unknown attacker",
            ),
        ] {
            let (s, _) = store_mem(&state, "soc", json!({"text": text, "id": id})).await;
            assert_eq!(s, StatusCode::CREATED);
        }

        let (s, body) = recall_mem(
            &state,
            "soc",
            json!({"query": "ssh brute force attack", "hybrid": true, "k": 2}),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{body}");
        let hits = body["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 2, "hybrid k=2 → exactly 2 hits");
        let ids: Vec<&str> = hits.iter().map(|h| h["id"].as_str().unwrap()).collect();
        assert!(
            ids.contains(&"i1") && ids.contains(&"i3"),
            "both ssh incidents must surface, got {ids:?}"
        );
        assert!(
            !ids.contains(&"i2"),
            "the backup note must not, got {ids:?}"
        );
        // Teeth: the scores are reciprocal-rank-fusion sums, not BM25 or
        // cosine. With k=60 and two legs a document contributes at most
        // 1/61 per leg, so no fused score can exceed 2/61 — a BM25 or
        // cosine score for this query would be well above that.
        for h in hits {
            let score = h["score"].as_f64().unwrap();
            assert!(
                score > 0.0 && score <= 2.0 / 61.0 + 1e-6,
                "hybrid recall must return RRF-fused scores, got {score}"
            );
        }
        // The top memory is within the top-2 of BOTH legs, so its fused score
        // is at least 1/61 + 1/62 — a single-leg document never exceeds 1/61.
        let top = hits[0]["score"].as_f64().unwrap();
        assert!(
            top >= 1.0 / 61.0 + 1.0 / 62.0 - 1e-6,
            "the top memory must be fused from both legs, got {top}"
        );

        // `fusion: "linear"` is the other supported combiner: each leg is
        // min-max-normalised to [0, 1] and summed, so the top hit scores 2.0
        // when both legs agree on it and 1.0 when they disagree — either way
        // it is >= 1.0, which no RRF sum can reach.
        let (s, body) = recall_mem(
            &state,
            "soc",
            json!({"query": "ssh brute force attack", "hybrid": true, "fusion": "linear", "k": 2}),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{body}");
        let hits = body["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 2);
        let top = hits[0]["score"].as_f64().unwrap();
        assert!(
            (1.0 - 1e-6..=2.0 + 1e-6).contains(&top),
            "linear fusion sums normalised leg scores, got {top}"
        );

        // A metadata filter narrows BOTH legs (it is applied per leg, in the
        // shape each leg dispatches with).
        store_mem(
            &state,
            "soc",
            json!({"text":"ssh attack from host 9.9.9.9","id":"i4","metadata":{"sev":"high"}}),
        )
        .await;
        let (s, body) = recall_mem(
            &state,
            "soc",
            json!({"query":"ssh attack","hybrid":true,"k":10,"filter":{"term":{"metadata.sev":"high"}}}),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{body}");
        let hits = body["hits"].as_array().unwrap();
        assert!(
            !hits.is_empty(),
            "filtered hybrid recall returns the high-sev memory"
        );
        assert!(
            hits.iter().all(|h| h["id"] == "i4"),
            "only the high-sev memory passes the filter on both legs"
        );

        // Composes with the recency re-rank (over-fetch + blend + truncate).
        let (s, body) = recall_mem(
            &state,
            "soc",
            json!({"query":"ssh brute force attack","hybrid":true,"k":2,"recency_weight":0.5}),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{body}");
        assert_eq!(body["hits"].as_array().unwrap().len(), 2);
    }

    /// #918: hybrid recall keeps the fail-loud contract — every ambiguous or
    /// unsupported combination is a 400 with a specific reason, never a
    /// silent fallback to one leg.
    #[tokio::test]
    async fn recall_hybrid_rejects_ambiguous_requests() {
        let state = test_state();
        store_mem(
            &state,
            "soc",
            json!({"text": "ssh brute force", "id": "i1"}),
        )
        .await;

        let cases: [(Value, &str); 6] = [
            (json!({"hybrid": true}), "hybrid without a query"),
            (
                json!({"query": "", "hybrid": true}),
                "hybrid with an empty query",
            ),
            (
                json!({"vector": [1.0, 0.0, 0.0], "hybrid": true}),
                "hybrid with a caller-supplied vector",
            ),
            (
                json!({"query": "ssh", "semantic": true, "hybrid": true}),
                "hybrid beside semantic",
            ),
            (
                json!({"query": "ssh", "fusion": "rrf"}),
                "fusion without hybrid",
            ),
            (
                json!({"query": "ssh", "hybrid": true, "fusion": "learned"}),
                "learned fusion",
            ),
        ];
        for (body, what) in cases {
            let (s, resp) = recall_mem(&state, "soc", body).await;
            assert_eq!(
                s,
                StatusCode::BAD_REQUEST,
                "{what} must be a 400, got {resp}"
            );
        }
        let (s, resp) = recall_mem(
            &state,
            "soc",
            json!({"query": "ssh", "hybrid": true, "fusion": "bogus"}),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(
            resp.to_string()
                .contains("unknown hybrid fusion strategy `bogus`"),
            "{resp}"
        );

        // `hybrid: false` is the documented default — plain BM25, byte-identical.
        let (s, off) = recall_mem(&state, "soc", json!({"query": "ssh", "hybrid": false})).await;
        assert_eq!(s, StatusCode::OK);
        let (_, plain) = recall_mem(&state, "soc", json!({"query": "ssh"})).await;
        assert_eq!(off, plain, "hybrid:false must not change the BM25 path");
    }

    async fn count_of(state: &AppState, ns: &str) -> u64 {
        let resp = list(
            State(state.clone()),
            Path(ns.to_string()),
            Principal::Superuser,
            axum::extract::Query(ListParams::default()),
        )
        .await;
        let (_, body) = drain_json(resp).await;
        body["count"].as_u64().unwrap()
    }

    async fn list_page(state: &AppState, ns: &str, params: ListParams) -> Value {
        let resp = list(
            State(state.clone()),
            Path(ns.to_string()),
            Principal::Superuser,
            axum::extract::Query(params),
        )
        .await;
        let (s, body) = drain_json(resp).await;
        assert_eq!(s, StatusCode::OK, "list must 200");
        body
    }

    /// Item 12: `list` pages a namespace larger than the default page with
    /// `from`/`size` and exposes a `next` cursor — the old endpoint silently
    /// capped at 100 with no way to reach the rest.
    #[tokio::test]
    async fn list_paginates_beyond_default_limit() {
        let state = test_state();
        // 150 entries — more than the default page (LIST_LIMIT = 100).
        for i in 0..150 {
            let (s, _) = store_mem(
                &state,
                "big",
                json!({"text": format!("memory number {i}"), "id": format!("m{i:03}")}),
            )
            .await;
            assert_eq!(s, StatusCode::CREATED);
        }

        // Default page: true count is 150, exactly 100 entries returned, and a
        // `next` cursor points at offset 100.
        let p1 = list_page(&state, "big", ListParams::default()).await;
        assert_eq!(
            p1["count"].as_u64().unwrap(),
            150,
            "count is the true total"
        );
        assert_eq!(
            p1["entries"].as_array().unwrap().len(),
            100,
            "default page returns LIST_LIMIT entries"
        );
        assert_eq!(
            p1["next"]["from"].as_u64().unwrap(),
            100,
            "next cursor at 100"
        );
        assert_eq!(p1["next"]["size"].as_u64().unwrap(), 100);

        // Second page via the cursor: the remaining 50, and `next` is null.
        let p2 = list_page(
            &state,
            "big",
            ListParams {
                from: Some(100),
                after: None,
                size: Some(100),
            },
        )
        .await;
        assert_eq!(
            p2["entries"].as_array().unwrap().len(),
            50,
            "from=100 returns the final 50 entries"
        );
        assert!(p2["next"].is_null(), "last page has no next cursor");

        // `size` is clamped to MAX_LIST_SIZE (asking for more still succeeds).
        let big = list_page(
            &state,
            "big",
            ListParams {
                from: Some(0),
                after: None,
                size: Some(10_000),
            },
        )
        .await;
        assert_eq!(
            big["size"].as_u64().unwrap(),
            MAX_LIST_SIZE as u64,
            "size clamped"
        );
        assert_eq!(
            big["entries"].as_array().unwrap().len(),
            150,
            "a large (clamped) page still returns all 150"
        );
        assert!(big["next"].is_null());

        // `after` is accepted as an alias for `from`.
        let aliased = list_page(
            &state,
            "big",
            ListParams {
                from: None,
                after: Some(100),
                size: Some(100),
            },
        )
        .await;
        assert_eq!(
            aliased["entries"].as_array().unwrap().len(),
            50,
            "after=100 aliases from=100"
        );
    }

    /// Opt-in semantic dedup: storing the same text twice with `dedup:true`
    /// keeps the namespace at one entry (2nd store is deduplicated), a distinct
    /// text still stores, and omitting `dedup` keeps the backward-compatible
    /// store-everything default.
    #[tokio::test]
    async fn store_semantic_dedup() {
        let state = test_state();
        let dup = "the sprint planning meeting is at noon on friday";

        // First store → created.
        let (s, b) = store_mem(&state, "dd", json!({"text": dup, "dedup": true})).await;
        assert_eq!(s, StatusCode::CREATED);
        assert_eq!(b["created"], json!(true));
        let first_id = b["id"].as_str().unwrap().to_string();

        // Same text again with dedup:true → deduplicated, not created.
        let (s, b) = store_mem(&state, "dd", json!({"text": dup, "dedup": true})).await;
        assert_eq!(s, StatusCode::OK, "a deduplicated store is not a creation");
        assert_eq!(b["created"], json!(false));
        assert_eq!(b["deduplicated"], json!(true));
        assert_eq!(
            b["id"].as_str().unwrap(),
            first_id,
            "dedup returns the id of the existing near-duplicate"
        );
        assert_eq!(
            count_of(&state, "dd").await,
            1,
            "dedup keeps a single entry"
        );

        // A distinct memory still stores under dedup:true.
        let (s, b) = store_mem(
            &state,
            "dd",
            json!({"text": "quarterly revenue grew twelve percent", "dedup": true}),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED);
        assert_eq!(b["created"], json!(true));
        assert_eq!(
            count_of(&state, "dd").await,
            2,
            "distinct text is not a dup"
        );

        // dedup omitted → default OFF, the duplicate IS stored (backward-compat).
        let (s, b) = store_mem(&state, "dd", json!({"text": dup})).await;
        assert_eq!(s, StatusCode::CREATED);
        assert_eq!(b["created"], json!(true));
        assert_eq!(
            count_of(&state, "dd").await,
            3,
            "dedup defaults OFF: omitting it stores duplicates"
        );
    }

    /// Link one SECOND_BRAIN_SPEC §8.3 fixture edge through the graph API
    /// (same brain name as the memory namespace; explicit fixture timestamps
    /// so the pinned edge_ids and the §8.6 sort order reproduce exactly).
    async fn link_edge(state: &AppState, src: &str, dst: &str, ty: &str, weight: f64) {
        let b: crate::graph_api::LinkBody = serde_json::from_value(json!({
            "src": src, "dst": dst, "type": ty, "weight": weight,
            "valid_at": 1_753_600_000_000i64, "created_at": 1_753_600_000_000i64,
        }))
        .unwrap();
        let resp = crate::graph_api::link(
            State(state.clone()),
            Path("notes".to_string()),
            Principal::Superuser,
            OptionalJson(Some(b)),
        )
        .await;
        let (s, body) = drain_json(resp).await;
        assert!(s.is_success(), "fixture link {src}->{dst} failed: {body}");
    }

    /// The §8 fixture world: the five notes as memories (fixture node ids)
    /// plus the eight §8.3 edges in brain `notes`.
    async fn graph_fixture(state: &AppState) {
        for (id, text) in [
            (
                "note-alpha",
                "Alpha is the hub note. It links to [[beta]] and [[gamma]].",
            ),
            (
                "note-beta",
                "Beta continues the thread and references [[gamma]].",
            ),
            (
                "note-gamma",
                "Gamma is the sink note with no outgoing links.",
            ),
            ("note-delta", "Delta cites [[alpha]] as its source."),
            ("note-epsilon", "Epsilon stands alone."),
        ] {
            let (s, _) = store_mem(state, "notes", json!({"text": text, "id": id})).await;
            assert_eq!(s, StatusCode::CREATED);
        }
        for (src, dst, ty, w) in [
            ("note-alpha", "note-beta", "wikilink", 1.0),
            ("note-alpha", "note-gamma", "wikilink", 1.0),
            ("note-beta", "note-gamma", "wikilink", 1.0),
            ("note-delta", "note-alpha", "wikilink", 1.0),
            ("note-alpha", "note-beta", "same_dir", 0.3),
            ("note-beta", "note-delta", "same_dir", 0.3),
            ("note-delta", "note-epsilon", "same_dir", 0.3),
            ("note-epsilon", "note-gamma", "same_dir", 0.3),
        ] {
            link_edge(state, src, dst, ty, w).await;
        }
    }

    /// SECOND_BRAIN_SPEC §5 + §8.6: `graph.mode=restrict` narrows recall to
    /// the seeds' bounded reach; the graph object reports the expansion
    /// honestly.
    #[tokio::test]
    async fn recall_graph_restrict_bounds_the_universe() {
        let state = test_state();
        graph_fixture(&state).await;

        // Sanity: ungated recall for "note" surfaces alpha AND gamma.
        let (_, b) = recall_mem(&state, "notes", json!({"query": "note", "k": 10})).await;
        let ids: Vec<&str> = b["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"note-alpha") && ids.contains(&"note-gamma"));
        assert!(b.get("graph").is_none(), "no graph key without graph opts");

        // §8.6: restrict from seed note-delta, hops 1, at the fixture as-of.
        // 1-hop both-direction reach = {delta, alpha, epsilon, beta} — gamma
        // is out of reach and must vanish from the hits.
        let (s, b) = recall_mem(
            &state,
            "notes",
            json!({
                "query": "note", "k": 10,
                "graph": {"mode": "restrict", "seeds": ["note-delta"],
                          "hops": 1, "as_of": 1_753_700_000_000i64}
            }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{b}");
        let ids: Vec<&str> = b["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["note-alpha"], "gamma is outside the 1-hop reach");
        let graph = &b["graph"];
        assert_eq!(graph["mode"], json!("restrict"));
        assert_eq!(graph["seeds"], json!(["note-delta"]));
        assert_eq!(graph["hops"], json!(1));
        assert_eq!(graph["as_of"], json!(1_753_700_000_000i64));
        assert_eq!(graph["reachable"], json!(4), "§8.6: 'reachable': 4");
        assert_eq!(graph["not_shown"]["reachable_clipped"], json!(0));
        assert!(
            graph["not_shown"].get("no_edges_index").is_none(),
            "the edges index exists — no honesty flag"
        );
    }

    /// §5 blend: graph proximity re-ranks the fetched candidates; every hit
    /// carries `graph_proximity`; seeds derive from the base recall when
    /// absent.
    #[tokio::test]
    async fn recall_graph_blend_reranks_by_proximity() {
        let state = test_state();
        graph_fixture(&state).await;

        let (s, b) = recall_mem(
            &state,
            "notes",
            json!({
                "query": "note", "k": 10,
                "graph": {"mode": "blend", "seeds": ["note-alpha"],
                          "weight": 0.9, "as_of": 1_753_700_000_000i64}
            }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{b}");
        let hits = b["hits"].as_array().unwrap();
        assert!(!hits.is_empty());
        // The seed itself anchors at proximity 1.0 and, at w=0.9, must
        // outrank gamma (proximity 0.5 via the hop-1 wikilink).
        assert_eq!(hits[0]["id"], json!("note-alpha"));
        assert_eq!(hits[0]["graph_proximity"], json!(1.0));
        let gamma = hits
            .iter()
            .find(|h| h["id"] == json!("note-gamma"))
            .expect("gamma stays recalled (blend re-ranks, never filters)");
        assert_eq!(gamma["graph_proximity"], json!(0.5));
        assert_eq!(b["graph"]["mode"], json!("blend"));

        // Seeds absent → derived from the top base hits, echoed back.
        let (s, b) = recall_mem(
            &state,
            "notes",
            json!({"query": "note", "k": 10, "graph": {"mode": "blend"}}),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let seeds = b["graph"]["seeds"].as_array().unwrap();
        assert!(!seeds.is_empty(), "blend derives seeds from base recall");
        assert!(b["graph"]["reachable"].as_u64().unwrap() > 0);
    }

    /// §5: a namespace with no edges index is NOT an error — recall proceeds
    /// ungated with `no_edges_index` flagged; malformed graph opts are loud
    /// 400s (including the not-a-graph-database hops wording).
    #[tokio::test]
    async fn recall_graph_no_edges_index_and_errors() {
        let state = test_state();
        let (s, _) = store_mem(&state, "lonely", json!({"text": "solo memory", "id": "m1"})).await;
        assert_eq!(s, StatusCode::CREATED);

        let (s, b) = recall_mem(
            &state,
            "lonely",
            json!({"query": "solo", "graph": {"mode": "restrict", "seeds": ["m1"]}}),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{b}");
        let ids: Vec<&str> = b["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["m1"], "recall proceeds ungated");
        assert_eq!(b["graph"]["reachable"], json!(0));
        assert_eq!(b["graph"]["not_shown"]["no_edges_index"], json!(true));

        // Unknown mode.
        let (s, _) = recall_mem(
            &state,
            "lonely",
            json!({"query": "solo", "graph": {"mode": "pagerank"}}),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);

        // restrict without seeds.
        let (s, b) = recall_mem(
            &state,
            "lonely",
            json!({"query": "solo", "graph": {"mode": "restrict"}}),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(
            b["error"]["reason"],
            json!("graph.mode 'restrict' requires non-empty graph.seeds")
        );

        // hops beyond the cap → the §4.6 sentence.
        let (s, b) = recall_mem(
            &state,
            "lonely",
            json!({"query": "solo", "graph": {"mode": "blend", "hops": 3}}),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(b["error"]["reason"]
            .as_str()
            .unwrap()
            .contains("not a graph database"));

        // The inverse corner: an edges index CAN exist before the memory
        // index does (a brain linked before its first memory). The empty
        // recall must NOT claim no_edges_index — that flag is a statement
        // about the world, not a shrug.
        let b: crate::graph_api::LinkBody = serde_json::from_value(json!({
            "src": "g1", "dst": "g2", "type": "wikilink"
        }))
        .unwrap();
        let resp = crate::graph_api::link(
            State(state.clone()),
            Path("ghost".to_string()),
            Principal::Superuser,
            OptionalJson(Some(b)),
        )
        .await;
        let (s, _) = drain_json(resp).await;
        assert_eq!(s, StatusCode::CREATED);
        let (s, b) = recall_mem(
            &state,
            "ghost",
            json!({"query": "anything", "graph": {"mode": "blend", "seeds": ["g1"]}}),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(b["hits"], json!([]), "no memory index → no hits");
        assert!(
            b["graph"]["not_shown"].get("no_edges_index").is_none(),
            "the edges index exists — the flag would be a lie: {b}"
        );
        assert_eq!(
            b["graph"]["reachable"],
            json!(2),
            "expansion still reports the seeds' true reach"
        );
    }

    /// Recency-blended recall: given an older perfect-relevance memory and a
    /// newer near-relevance one, `recency_weight=0.9` surfaces the newer memory
    /// first, while `recency_weight=0` preserves pure relevance order.
    #[tokio::test]
    async fn recall_recency_blend_reranks() {
        let state = test_state();

        // Old memory: exact match to the query vector → highest relevance.
        let (s, _) = store_mem(
            &state,
            "rc",
            json!({"text": "old note", "vector": [1.0, 0.0, 0.0], "id": "old"}),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED);

        // Guarantee a strictly later `stored_at` for the newer memory.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // New memory: slightly lower relevance (cosine ~0.994) but more recent.
        let (s, _) = store_mem(
            &state,
            "rc",
            json!({"text": "new note", "vector": [0.9, 0.1, 0.0], "id": "new"}),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED);

        // recency_weight = 0 → pure relevance: the exact-match old note is first.
        let (_, b) = recall_mem(
            &state,
            "rc",
            json!({"vector": [1.0, 0.0, 0.0], "k": 2, "recency_weight": 0.0}),
        )
        .await;
        let hits = b["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0]["id"], "old",
            "recency_weight=0 must preserve pure relevance order"
        );

        // recency_weight = 0.9 → recency dominates: the newer note ranks first.
        let (_, b) = recall_mem(
            &state,
            "rc",
            json!({"vector": [1.0, 0.0, 0.0], "k": 2, "recency_weight": 0.9}),
        )
        .await;
        let hits = b["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0]["id"], "new",
            "recency_weight=0.9 must surface the most recent memory first"
        );
    }

    /// Copying the Elasticsearch document routes onto `/_memory` used to
    /// return an empty 405/404. The body has to name the real calls.
    #[tokio::test]
    async fn document_routes_on_memory_name_the_real_calls() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.keep();
        let mut config = Config::default();
        config.server.data_dir = path.to_str().unwrap().to_string();
        config.auth.enabled = false;
        let metrics = Metrics::new().expect("metrics");
        let engine = Engine::new(config.clone()).expect("engine");
        let app = crate::router::build_es_compat_router(AppState::new(config, engine, metrics));

        for (method, uri) in [
            ("POST", "/_memory/notes/_doc"),
            ("GET", "/_memory/notes/_search"),
            ("PUT", "/_memory/notes/_doc/abc"),
            ("POST", "/_memory/notes/some-id"),
        ] {
            let req = axum::http::Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    r#"{"text":"via the document route"}"#,
                ))
                .unwrap();
            let resp = tower::ServiceExt::oneshot(app.clone(), req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{method} {uri}"
            );
            // `/_memory/{ns}/{id}` serves DELETE (forget); the `_doc/{id}`
            // spelling serves nothing, so its `Allow` is empty.
            let expected_allow = if uri.starts_with("/_memory/notes/_doc/") {
                ""
            } else {
                "DELETE"
            };
            assert_eq!(
                resp.headers()
                    .get(header::ALLOW)
                    .and_then(|v| v.to_str().ok()),
                Some(expected_allow),
                "{method} {uri}"
            );
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            let reason = body["error"]["reason"].as_str().unwrap_or("");
            assert!(
                reason.contains("POST /_memory/notes") && reason.contains("\"text\""),
                "{method} {uri} reason must name the store call: {reason}"
            );
            assert!(
                reason.contains("POST /_memory/notes/_recall"),
                "{method} {uri} reason must name recall: {reason}"
            );
            assert!(!bytes.is_empty(), "{method} {uri} body was empty");
        }

        // The real store route is unchanged.
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/_memory/notes")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(r#"{"text":"kept"}"#))
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app.clone(), req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        // DELETE of one id is still forget, including an id spelled `_doc`.
        let req = axum::http::Request::builder()
            .method("DELETE")
            .uri("/_memory/notes/_doc")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["forgotten"], false);
        assert_eq!(body["id"], "_doc");
    }

    #[tokio::test]
    async fn list_namespaces_empty_and_populated() {
        let state = test_state();

        // 1. Initially empty: no memory namespaces exist.
        let resp = list_namespaces(
            State(state.clone()),
            Principal::Superuser,
            axum::extract::Query(ListParams::default()),
        )
        .await;
        let (s, body) = drain_json(resp).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(body["count"], 0);
        assert_eq!(body["namespaces"].as_array().unwrap().len(), 0);
        assert!(body["next"].is_null());

        // 2. Populate two namespaces: `agent-a` (2 docs) and `support-bot` (1 doc).
        store_mem(&state, "agent-a", json!({"text":"first memory"})).await;
        store_mem(&state, "agent-a", json!({"text":"second memory"})).await;
        store_mem(&state, "support-bot", json!({"text":"bot memory"})).await;

        // Add a graph edge for `support-bot` using graph_api::link.
        let link_body: crate::graph_api::LinkBody = serde_json::from_value(json!({
            "src": "m1",
            "dst": "m2",
            "type": "relates_to",
        }))
        .unwrap();
        let link_resp = crate::graph_api::link(
            State(state.clone()),
            Path("support-bot".to_string()),
            Principal::Superuser,
            OptionalJson(Some(link_body)),
        )
        .await;
        assert_eq!(link_resp.into_response().status(), StatusCode::CREATED);

        // 3. List namespaces: should return both sorted alphabetically.
        let resp = list_namespaces(
            State(state.clone()),
            Principal::Superuser,
            axum::extract::Query(ListParams::default()),
        )
        .await;
        let (s, body) = drain_json(resp).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(body["count"], 2);
        let list = body["namespaces"].as_array().unwrap();
        assert_eq!(list.len(), 2);

        // `agent-a`: count=2, graph=false
        assert_eq!(list[0]["namespace"], "agent-a");
        assert_eq!(list[0]["count"], 2);
        assert_eq!(list[0]["graph"], false);

        // `support-bot`: count=1, graph=true
        assert_eq!(list[1]["namespace"], "support-bot");
        assert_eq!(list[1]["count"], 1);
        assert_eq!(list[1]["graph"], true);

        // 4. Pagination: from=0, size=1 -> returns agent-a, next={from:1, size:1}
        let resp = list_namespaces(
            State(state.clone()),
            Principal::Superuser,
            axum::extract::Query(ListParams {
                from: Some(0),
                size: Some(1),
                ..Default::default()
            }),
        )
        .await;
        let (_, p1) = drain_json(resp).await;
        assert_eq!(p1["count"], 2);
        assert_eq!(p1["namespaces"].as_array().unwrap().len(), 1);
        assert_eq!(p1["namespaces"][0]["namespace"], "agent-a");
        assert_eq!(p1["next"], json!({ "from": 1, "size": 1 }));

        // Page 2: from=1, size=1 -> returns support-bot, next=null
        let resp = list_namespaces(
            State(state.clone()),
            Principal::Superuser,
            axum::extract::Query(ListParams {
                from: Some(1),
                size: Some(1),
                ..Default::default()
            }),
        )
        .await;
        let (_, p2) = drain_json(resp).await;
        assert_eq!(p2["count"], 2);
        assert_eq!(p2["namespaces"].as_array().unwrap().len(), 1);
        assert_eq!(p2["namespaces"][0]["namespace"], "support-bot");
        assert!(p2["next"].is_null());
    }

    #[tokio::test]
    async fn bare_memory_unsupported_verbs() {
        let resp = unsupported_memory_bare().await;
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            resp.headers().get("allow").and_then(|v| v.to_str().ok()),
            Some("GET")
        );
        let (_, body) = drain_json(resp).await;
        assert_eq!(body["status"], 405);
        assert_eq!(body["error"]["type"], "memory_error");
        assert!(body["error"]["reason"]
            .as_str()
            .unwrap()
            .contains("/_memory only supports GET"));
    }

    // ── #1792: validity windows + as-of recall ─────────────────────────────

    const T0: i64 = 1_700_000_000_000; // 2023-11-14T22:13:20Z
    const DAY: i64 = 86_400_000;

    fn ids_of(body: &Value) -> Vec<String> {
        let mut ids: Vec<String> = body["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    }

    async fn asof_ids(state: &AppState, ns: &str, as_of: Value) -> Vec<String> {
        let (s, body) = recall_mem(
            state,
            ns,
            json!({"query": "office", "k": 10, "as_of": as_of}),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{body}");
        ids_of(&body)
    }

    #[tokio::test]
    async fn recall_as_of_overlapping_windows() {
        let state = test_state();
        let ns = "asof-overlap";
        // Two facts whose windows overlap on [T0+5d, T0+10d), plus one
        // timeless fact that must be visible at every instant.
        for b in [
            json!({"id":"berlin","text":"the office is in berlin",
                   "valid_from": T0, "valid_to": T0 + 10 * DAY}),
            json!({"id":"paris","text":"the office is in paris",
                   "valid_from": T0 + 5 * DAY}),
            json!({"id":"always","text":"the office opens at nine"}),
        ] {
            let (s, body) = store_mem(&state, ns, b).await;
            assert_eq!(s, StatusCode::CREATED, "{body}");
        }

        // Before every window: only the timeless fact.
        assert_eq!(asof_ids(&state, ns, json!(T0 - 1)).await, vec!["always"]);
        // valid_from is inclusive.
        assert_eq!(
            asof_ids(&state, ns, json!(T0)).await,
            vec!["always", "berlin"]
        );
        // Overlap: both windows contain the instant.
        assert_eq!(
            asof_ids(&state, ns, json!(T0 + 7 * DAY)).await,
            vec!["always", "berlin", "paris"]
        );
        // valid_to is exclusive.
        assert_eq!(
            asof_ids(&state, ns, json!(T0 + 10 * DAY)).await,
            vec!["always", "paris"]
        );
        // RFC3339 input parses to the same instant as epoch-ms.
        assert_eq!(
            asof_ids(&state, ns, json!("2023-11-21T22:13:20Z")).await,
            vec!["always", "berlin", "paris"]
        );
        // Default as_of is now: berlin's window closed in 2023.
        let (_, body) = recall_mem(&state, ns, json!({"query": "office"})).await;
        assert_eq!(ids_of(&body), vec!["always", "paris"]);
        assert!(body["as_of"].as_i64().unwrap() > T0 + 10 * DAY);

        // Hits carry the window and the transaction time.
        let (_, body) = recall_mem(&state, ns, json!({"query": "berlin", "as_of": T0 + DAY})).await;
        let hit = &body["hits"][0];
        assert_eq!(hit["id"], "berlin");
        assert_eq!(hit["valid_from"], json!(T0));
        assert_eq!(hit["valid_to"], json!(T0 + 10 * DAY));
        assert!(hit["recorded_at"].as_i64().is_some());
        assert_eq!(body["as_of"], json!(T0 + DAY));

        // Every recall mode narrows the same way (semantic leg carries the
        // filter inside the `semantic` node; hybrid on both legs).
        for mode in [json!({"semantic": true}), json!({"hybrid": true})] {
            let mut b = json!({"query": "office", "k": 10, "as_of": T0 - 1});
            b.as_object_mut()
                .unwrap()
                .extend(mode.as_object().unwrap().clone());
            let (s, body) = recall_mem(&state, ns, b).await;
            assert_eq!(s, StatusCode::OK, "{body}");
            assert_eq!(ids_of(&body), vec!["always"], "mode {mode}");
        }
        // A caller filter composes with the validity cut.
        let (_, body) = recall_mem(
            &state,
            ns,
            json!({"query": "office", "as_of": T0 + 7 * DAY,
                   "filter": {"ids": {"values": ["berlin", "always"]}}}),
        )
        .await;
        assert_eq!(ids_of(&body), vec!["always", "berlin"]);
    }

    #[tokio::test]
    async fn recall_as_of_retracted_fact() {
        let state = test_state();
        let ns = "asof-retract";
        let (s, _) = store_mem(
            &state,
            ns,
            json!({"id":"ceo","text":"alice is the ceo of the office","valid_from": T0}),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED);
        let (_, first) = recall_mem(&state, ns, json!({"query": "ceo", "as_of": T0})).await;
        let recorded = first["hits"][0]["recorded_at"].as_i64().unwrap();

        // Retract later: same id, only valid_to. valid_from and recorded_at
        // are carried over from the stored version.
        let (s, body) = store_mem(
            &state,
            ns,
            json!({"id":"ceo","text":"alice is the ceo of the office",
                   "valid_to": T0 + 30 * DAY, "dedup": true}),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "retraction must write: {body}");

        // Before the retraction instant the fact is still true...
        let (_, body) =
            recall_mem(&state, ns, json!({"query": "ceo", "as_of": T0 + 29 * DAY})).await;
        assert_eq!(ids_of(&body), vec!["ceo"]);
        let hit = &body["hits"][0];
        assert_eq!(hit["valid_from"], json!(T0), "valid_from carried over");
        assert_eq!(hit["valid_to"], json!(T0 + 30 * DAY));
        assert_eq!(hit["recorded_at"], json!(recorded), "recorded_at kept");
        // ...not before it started, and not after it was retracted.
        assert!(asof_ids(&state, ns, json!(T0 - 1)).await.is_empty());
        let (_, body) =
            recall_mem(&state, ns, json!({"query": "ceo", "as_of": T0 + 30 * DAY})).await;
        assert!(ids_of(&body).is_empty());
        let (_, body) = recall_mem(&state, ns, json!({"query": "ceo"})).await;
        assert!(ids_of(&body).is_empty(), "retracted fact gone by default");

        // A retraction that would end before the carried-over start is a 400.
        let (s, body) = store_mem(
            &state,
            ns,
            json!({"id":"ceo","text":"alice is the ceo","valid_to": T0 - DAY}),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.to_string().contains("empty validity window"));
    }

    #[tokio::test]
    async fn validity_input_errors_and_validity_free_namespaces() {
        let state = test_state();
        let ns = "asof-errors";
        for (b, needle) in [
            (
                json!({"text":"x","valid_from":"yesterday"}),
                "`valid_from` must be",
            ),
            (json!({"text":"x","valid_to":true}), "`valid_to` must be"),
            (
                json!({"text":"x","valid_from": T0, "valid_to": T0}),
                "empty validity window",
            ),
        ] {
            let (s, body) = store_mem(&state, ns, b).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
            assert!(body.to_string().contains(needle), "{body}");
        }

        // A namespace that never stored a bound: as_of is accepted and
        // everything is valid; no `as_of` echo unless asked for.
        store_mem(&state, ns, json!({"id":"plain","text":"plain office note"})).await;
        let (_, body) = recall_mem(&state, ns, json!({"query": "office"})).await;
        assert_eq!(ids_of(&body), vec!["plain"]);
        assert!(body.get("as_of").is_none());
        assert!(body["hits"][0].get("valid_from").is_none());
        assert_eq!(asof_ids(&state, ns, json!(0)).await, vec!["plain"]);
        let (s, body) = recall_mem(&state, ns, json!({"query": "office", "as_of": "soon"})).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(body.to_string().contains("`as_of` must be"));
    }

    #[tokio::test]
    async fn validity_fields_do_not_bypass_namespace_authz() {
        let state = test_state();
        let unscoped = Principal::Unscoped {
            key_id: "k1".into(),
        };
        let b: StoreBody =
            serde_json::from_value(json!({"text":"x","valid_from": T0, "valid_to": T0 + DAY}))
                .unwrap();
        let resp = store(
            State(state.clone()),
            Path("asof-authz".to_string()),
            unscoped.clone(),
            OptionalJson(Some(b)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let b: RecallBody = serde_json::from_value(json!({"query":"x","as_of": T0})).unwrap();
        let resp = recall(
            State(state.clone()),
            Path("asof-authz".to_string()),
            unscoped,
            OptionalJson(Some(b)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn graph_as_of_defaults_to_top_level_as_of() {
        let g = GraphRecallOpts {
            mode: "blend".into(),
            seeds: None,
            hops: None,
            types: None,
            weight: None,
            as_of: None,
        };
        let ctx = parse_graph_opts(Some(&g), Some(T0)).ok().flatten().unwrap();
        assert_eq!(ctx.as_of_ms, T0);
        let g = GraphRecallOpts {
            as_of: Some(json!(T0 + DAY)),
            ..g
        };
        let ctx = parse_graph_opts(Some(&g), Some(T0)).ok().flatten().unwrap();
        assert_eq!(ctx.as_of_ms, T0 + DAY, "explicit graph.as_of wins");
    }
}
