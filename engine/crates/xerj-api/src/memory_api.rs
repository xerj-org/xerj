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
//! POST   /_memory/{namespace}            store   — {text, metadata?, vector?, id?, dedup?, dedup_threshold?} → {id, created} | {id, created:false, deduplicated:true}
//! POST   /_memory/{namespace}/_recall    recall  — {query?, vector?, semantic?, k?, filter?, recency_weight?} → {hits:[…]}
//! GET    /_memory/{namespace}?from&size  list    — {count, entries:[…], next} (paged, recent first)
//! DELETE /_memory/{namespace}/{id}       forget  — delete one entry
//! DELETE /_memory/{namespace}            drop    — drop the whole namespace
//! ```
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
    // Second-brain collision guard (SECOND_BRAIN_SPEC §1): brain `B` keeps its
    // graph edges in `.xerj-memory-{B}-edges`, so a namespace ending in
    // `-edges` would silently write memories into another brain's edge index.
    // Deliberate breaking change — record in the release notes.
    if ns.ends_with("-edges") {
        return Err("namespace suffix '-edges' is reserved for graph edge indices".into());
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

    let index = backing_index(&namespace);
    let dims = body.vector.as_ref().map(|v| v.len());
    if let Err(resp) = ensure_backing_index(&state, &index, dims).await {
        return resp;
    }

    // Opt-in semantic dedup: skip the write when a near-identical memory already
    // exists. Off by default; only runs when the caller sets `dedup: true`.
    if body.dedup == Some(true) {
        let threshold = body.dedup_threshold.unwrap_or(DEFAULT_DEDUP_THRESHOLD);
        if let Some((existing_id, score)) =
            dedup_probe(&state, &index, &body.text, body.vector.as_deref()).await
        {
            if score >= threshold {
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
    let k = body.k.unwrap_or(DEFAULT_K).max(1);
    // Recency blending needs a wider candidate pool than the final k so the
    // re-rank can actually promote recent-but-slightly-less-relevant memories.
    let recency_weight = body.recency_weight.map(|w| w.clamp(0.0, 1.0));
    // Optional graph coupling (SECOND_BRAIN_SPEC §5): validate up front so a
    // bad mode / out-of-bounds hops fails fast (with the §4.6 wording), before
    // any search runs.
    let graph_ctx = match parse_graph_opts(body.graph.as_ref()) {
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
    let filter_opt: Option<Value> = match &graph_ctx {
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
        let mut semantic = json!({ "field": "text", "query": q, "k": fetch });
        if let Some(filter) = filter_opt {
            semantic["filter"] = filter;
        }
        search_body.query = Some(json!({ "semantic": semantic }));
    } else {
        // (3) Text recall (BM25). Empty/absent query → match_all (recent memories).
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
fn parse_graph_opts(opts: Option<&GraphRecallOpts>) -> Result<Option<GraphCtx>, Box<Response>> {
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
        None => chrono::Utc::now().timestamp_millis(),
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
    json!({
        "id": h.get("_id").cloned().unwrap_or(Value::Null),
        "text": src.and_then(|s| s.get("text")).cloned().unwrap_or(Value::Null),
        "metadata": src
            .and_then(|s| s.get("metadata"))
            .cloned()
            .unwrap_or(Value::Null),
        "score": h.get("_score").cloned().unwrap_or(Value::Null),
    })
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

// ─────────────────────────────────────────────────────────────────────────────
// Forget
// ─────────────────────────────────────────────────────────────────────────────

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
        return error_response(status, format!("forget failed: {body}"));
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
}
