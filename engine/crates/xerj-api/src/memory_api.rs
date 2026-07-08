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
//! POST   /_memory/{namespace}            store   — {text, metadata?, vector?, id?} → {id, created}
//! POST   /_memory/{namespace}/_recall    recall  — {query?, vector?, semantic?, k?, filter?} → {hits:[…]}
//! GET    /_memory/{namespace}            list    — {count, entries:[…]} (bounded, recent first)
//! DELETE /_memory/{namespace}/{id}       forget  — delete one entry
//! DELETE /_memory/{namespace}            drop    — drop the whole namespace
//! ```
//!
//! Recall has three modes, tried in order: an explicit query `vector` (kNN over
//! a caller-supplied embedding) → `semantic: true` (the server embeds `query`
//! with the same embedder used at store time and recalls by vector similarity)
//! → plain `query` text (BM25 relevance). All three are offline-testable with
//! NO external embedding service — memories are stored in a `semantic_text`
//! field, so the built-in deterministic embedder vectorises both the stored
//! text and the recall query. Namespaces isolate — a recall in namespace A
//! never sees namespace B's entries because they live in physically distinct
//! backing indices.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::es_compat::{
    self, DeleteDocParams, DeleteIndexParams, EsSearchBody, EsSearchQueryParams, IndexDocParams,
};
use crate::extract::OptionalJson;
use crate::state::AppState;

/// Reserved backing-index prefix. Real (user) indices can never collide: the
/// ES-compat `create_index` handler rejects any index name that starts with
/// `_`, `-`, or `+`, and a caller cannot address a `.`-leading system index
/// through `/_memory/*`.
const MEMORY_PREFIX: &str = ".xerj-memory-";

/// Maximum number of entries returned by the introspection (`GET`) endpoint.
const LIST_LIMIT: usize = 100;

/// Default number of memories returned by `_recall` when `k` is omitted.
const DEFAULT_K: usize = 10;

/// Resolve a namespace to its backing index name.
fn backing_index(namespace: &str) -> String {
    format!("{MEMORY_PREFIX}{namespace}")
}

/// Validate a namespace. We keep this deliberately strict so the derived index
/// name is always a legal, lowercase XERJ/ES index name and cannot be used to
/// smuggle characters (`/`, `,`, `*`, whitespace, uppercase, `..`) into the
/// backing-index routing layer.
fn validate_namespace(ns: &str) -> Result<(), String> {
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
    Ok(())
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
/// out of the box; text and metadata are always mapped.
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
    let mut properties = json!({
        "text": { "type": "semantic_text", "dimensions": 384 },
        "stored_at": { "type": "long" },
        "metadata": { "type": "object" }
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
    Err(error_response(
        status,
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
}

/// `POST /_memory/{namespace}` — store a memory entry.
pub async fn store(
    State(state): State<AppState>,
    Path(namespace): Path<String>,
    body: OptionalJson<StoreBody>,
) -> Response {
    if let Err(reason) = validate_namespace(&namespace) {
        return error_response(StatusCode::BAD_REQUEST, reason);
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

    let index = backing_index(&namespace);
    let dims = body.vector.as_ref().map(|v| v.len());
    if let Err(resp) = ensure_backing_index(&state, &index, dims).await {
        return resp;
    }

    let id = body.id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let mut doc = json!({
        "text": body.text,
        "stored_at": chrono::Utc::now().timestamp_millis(),
    });
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
        Json(doc),
    )
    .await
    .into_response();

    let (status, body) = drain_json(resp).await;
    if !status.is_success() {
        return error_response(status, format!("failed to store memory: {body}"));
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
#[derive(Debug, Deserialize)]
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
    /// Number of memories to return. Defaults to 10.
    #[serde(default)]
    pub k: Option<usize>,
    /// Optional metadata pre-filter, expressed as a standard ES query clause
    /// (e.g. `{"term": {"metadata.topic": "cats"}}`). Applied as a `bool`
    /// `filter` so it narrows — but does not score — the recalled set.
    #[serde(default)]
    pub filter: Option<Value>,
}

/// `POST /_memory/{namespace}/_recall` — recall the top-k relevant memories.
pub async fn recall(
    State(state): State<AppState>,
    Path(namespace): Path<String>,
    body: OptionalJson<RecallBody>,
) -> Response {
    if let Err(reason) = validate_namespace(&namespace) {
        return error_response(StatusCode::BAD_REQUEST, reason);
    }
    let body = body.0.unwrap_or(RecallBody {
        query: None,
        vector: None,
        semantic: None,
        k: None,
        filter: None,
    });
    let k = body.k.unwrap_or(DEFAULT_K).max(1);
    let index = backing_index(&namespace);

    // Unknown namespace → no memories (also enforces isolation cleanly).
    if !index_exists(&state, &index) {
        return Json(json!({ "hits": [], "namespace": namespace })).into_response();
    }

    // Build the search body, reusing the proven kNN / BM25 / filter paths.
    let mut search_body = EsSearchBody {
        size: k,
        ..Default::default()
    };

    if let Some(vec) = body.vector {
        // (1) Caller-supplied embedding → pure kNN over the stored `vector`.
        let knn = json!({ "field": "vector", "query_vector": vec, "k": k });
        match body.filter {
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
    } else if body.semantic == Some(true) {
        // (2) Server-side semantic recall: embed `query` with the same embedder
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
        let mut semantic = json!({ "field": "text", "query": q, "k": k });
        if let Some(filter) = body.filter {
            semantic["filter"] = filter;
        }
        search_body.query = Some(json!({ "semantic": semantic }));
    } else {
        // (3) Text recall (BM25). Empty/absent query → match_all (recent memories).
        let inner = match body.query.as_deref() {
            Some(q) if !q.is_empty() => json!({ "match": { "text": q } }),
            _ => json!({ "match_all": {} }),
        };
        search_body.query = Some(match body.filter {
            Some(filter) => json!({ "bool": { "must": [ inner ], "filter": [ filter ] } }),
            None => inner,
        });
    }

    let resp = es_compat::search(
        State(state.clone()),
        Path(index.clone()),
        axum::extract::Query(EsSearchQueryParams::default()),
        OptionalJson(Some(search_body)),
    )
    .await
    .into_response();

    let (status, body) = drain_json(resp).await;
    if !status.is_success() {
        return error_response(status, format!("recall failed: {body}"));
    }

    let hits = extract_hits(&body);
    Json(json!({ "hits": hits, "namespace": namespace })).into_response()
}

/// Map an ES search response into the agent-memory hit shape.
fn extract_hits(search_response: &Value) -> Vec<Value> {
    search_response
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|h| {
                    let src = h.get("_source");
                    json!({
                        "id": h.get("_id").cloned().unwrap_or(Value::Null),
                        "text": src.and_then(|s| s.get("text")).cloned().unwrap_or(Value::Null),
                        "metadata": src
                            .and_then(|s| s.get("metadata"))
                            .cloned()
                            .unwrap_or(Value::Null),
                        "score": h.get("_score").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ─────────────────────────────────────────────────────────────────────────────
// List / introspect
// ─────────────────────────────────────────────────────────────────────────────

/// `GET /_memory/{namespace}` — count + most-recent entries (bounded).
pub async fn list(State(state): State<AppState>, Path(namespace): Path<String>) -> Response {
    if let Err(reason) = validate_namespace(&namespace) {
        return error_response(StatusCode::BAD_REQUEST, reason);
    }
    let index = backing_index(&namespace);
    if !index_exists(&state, &index) {
        return Json(json!({
            "namespace": namespace,
            "exists": false,
            "count": 0,
            "entries": [],
        }))
        .into_response();
    }

    let search_body = EsSearchBody {
        query: Some(json!({ "match_all": {} })),
        size: LIST_LIMIT,
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
        OptionalJson(Some(search_body)),
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
    Json(json!({
        "namespace": namespace,
        "exists": true,
        "count": count,
        "entries": entries,
    }))
    .into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Forget
// ─────────────────────────────────────────────────────────────────────────────

/// `DELETE /_memory/{namespace}/{id}` — forget a single memory entry.
pub async fn forget_one(
    State(state): State<AppState>,
    Path((namespace, id)): Path<(String, String)>,
) -> Response {
    if let Err(reason) = validate_namespace(&namespace) {
        return error_response(StatusCode::BAD_REQUEST, reason);
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
        return error_response(status, format!("forget failed: {body}"));
    }
    Json(json!({ "id": id, "namespace": namespace, "forgotten": true })).into_response()
}

/// `DELETE /_memory/{namespace}` — drop an entire namespace.
pub async fn forget_namespace(
    State(state): State<AppState>,
    Path(namespace): Path<String>,
) -> Response {
    if let Err(reason) = validate_namespace(&namespace) {
        return error_response(StatusCode::BAD_REQUEST, reason);
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
}
