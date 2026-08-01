//! Second-Brain Graph REST API — a relationship layer over documents that
//! already exist.
//!
//! This is not a graph database. There is no query language, no shortest-path,
//! no PageRank, and no unbounded traversal — by design. Edges are ordinary
//! documents; expansion is a bounded, batched read (≤2 hops per call).
//!
//! Each brain `B` keeps its edges in one reserved index,
//! `.xerj-memory-{B}-edges` (schema: SECOND_BRAIN_SPEC §2), so edges ride the
//! existing WAL → memtable → segment → doc-values path with zero
//! storage-format change. Traversal is [`xerj_engine::graph::Index::graph_expand`]
//! — a columnar expansion, never a per-node query fan-out. Like
//! `memory_api.rs`, this module is a thin adapter that composes the proven
//! ES-compat handlers (`create_index`, `index_doc`, `get_doc`, `search`); it
//! does NOT re-implement search.
//!
//! Endpoints (mounted on the ES-compat router):
//! ```text
//! POST   /_graph/{brain}/link            assert an edge (lazy index create)
//! DELETE /_graph/{brain}/link/{edge_id}  soft-invalidate (bi-temporal; never removes)
//! GET    /_graph/{brain}/ego             neighborhood of one node — or up to
//!                                        64 via `nodes=` (+ hydration)
//! GET    /_graph/{brain}/overview        brain-level stats (dashboard feed)
//! ```
//!
//! Bi-temporality: an edge is never physically removed by this API.
//! Invalidation re-indexes the same document (same `_id`) with
//! `invalid_at`/`expired_at` added, so "what did it believe last Tuesday"
//! stays answerable via `as_of` on every read endpoint.
//!
//! Honesty in-band: every read response carries a `not_shown` object counting
//! what was withheld (clipped edges/frontiers, bi-temporally excluded edges,
//! segments skipped, dangling node refs, agg tails).
//!
//! ## Authorization model — a brain IS a boundary (issue #79)
//!
//! Every endpoint below authorizes the caller for **this brain** before it
//! does anything else, including before it checks whether the brain exists —
//! so a 403 is returned identically for a brain that is not yours and a brain
//! that does not exist, and the response code cannot be used to enumerate.
//!
//! The resource a brain authorizes against is its edges index,
//! `.xerj-memory-{brain}-edges`. Who holds what:
//!
//! | Principal | Reach |
//! |---|---|
//! | open mode (`--insecure`, point-at-a-folder) | every brain — one user, no config, unchanged |
//! | the configured admin key | every brain |
//! | a key minted **with** `role_descriptors` naming the edges index | that brain, at the granted privilege |
//! | a key minted **without** `role_descriptors` | **no** brain |
//! | no/invalid credential | nothing |
//!
//! To grant brain `alice` to a key, name its indices at mint time:
//!
//! ```json
//! POST /_security/api_key
//! { "name": "alice-agent", "role_descriptors": { "alice": { "indices": [
//!     { "names": [".xerj-memory-alice-edges", ".xerj-memory-alice"],
//!       "privileges": ["read", "write"] } ] } } }
//! ```
//!
//! `ego`/`overview` need `read`; `link`/`unlink` need `write` (creating the
//! edges index lazily is part of writing a brain, not a separate `manage`
//! step). The second name is the brain's *nodes* index — needed for `ego`'s
//! node hydration and `overview`'s note count, and it is also the
//! agent-memory namespace of the same name.
//!
//! ⚠️ An access check *here alone* would not have created a boundary. A
//! brain's edges live in an ordinary index and `IndexName::validate` admits a
//! leading `.`, so `.xerj-memory-{brain}-edges` is reachable through the
//! generic ES-compat and native index routes: `_search` reads it around this
//! module's `as_of`/`not_shown` semantics, `_doc/{id}` **forges** edges around
//! the derived-`edge_id` invariant (§2.3) that only [`link`] enforces, `DELETE`
//! destroys the brain, and `GET /_mapping` lists every brain name. All of
//! those doors are closed in [`crate::authz`], which owns the rule and
//! enumerates the enforcement points; the checks in this module are the
//! in-handler half of the same decision, so these handlers stay safe even if
//! mounted without that middleware.
//!
//! `tests/brain_is_a_security_boundary.rs` executes every door.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

use crate::auth::Principal;
use crate::authz;
use crate::es_compat::{
    self, EsSearchBody, EsSearchJson, EsSearchQueryParams, GetDocParams, IndexDocParams,
};
use crate::extract::OptionalJson;
use crate::state::AppState;
use xerj_engine::graph::{
    GraphDirection, GraphEdgeLite, GraphExpandRequest, GRAPH_HOPS_CAP_REASON,
};
use xerj_engine::rbac::Privilege;

/// Contract version string, returned by every read endpoint.
pub const GRAPH_CONTRACT: &str = "xerj-second-brain/1";

/// Reserved `_id` of the per-brain meta document (SECOND_BRAIN_SPEC §2.5). It
/// carries no `src`/`dst`, so the hop path and every edge count (all filter on
/// `exists src`) never see it.
const BRAIN_META_ID: &str = "__xerj-brain-meta";

/// Max returned edges for `ego` (`limit` clamp ceiling).
const MAX_EGO_LIMIT: usize = 1000;

/// Max seed ids accepted by `ego`'s `nodes=` param (multi-seed expansion).
/// Excess seeds are dropped and counted into `not_shown.frontier_clipped` —
/// the same honesty channel the engine uses for its own frontier clip. Deeper
/// exploration composes by iterating: expand again from a slice of the
/// previous response's `reachable` ids.
const EGO_SEEDS_CAP: usize = 64;

/// Max dangling node ids listed verbatim in `not_shown.dangling_ids`.
const MAX_DANGLING_LISTED: usize = 50;

/// Edges index name for a brain (SECOND_BRAIN_SPEC §1).
///
/// The leading dot passes `IndexName::validate` and keeps the index out of the
/// *conventional* listing surfaces, the same way `.kibana` is conventionally
/// hidden. It does **not** by itself make the index unreachable — an earlier
/// version of this comment claimed that, and it was wrong; `.`-prefixed names
/// are accepted by the generic index routes. What makes it unreachable is that
/// it sits in the reserved [`authz::RESERVED_INDEX_PREFIX`] namespace, which
/// [`authz`] gates on every surface. This name is also the resource a
/// `role_descriptors` grant must name to unlock the brain.
fn edges_index(brain: &str) -> String {
    authz::brain_edges_index(brain)
}

/// Default nodes index for a brain: the agent-memory namespace of the same
/// name. Autoindex brains override this via the §2.5 meta doc.
fn default_nodes_index(brain: &str) -> String {
    authz::memory_namespace_index(brain)
}

/// Brain-name validation (SECOND_BRAIN_SPEC §1): identical rules to
/// `memory_api::validate_namespace` (lowercase/digit start, `[a-z0-9._-]`,
/// ≤200 chars, no `..`) plus the `-edges` suffix rejection — without it a
/// brain named `kb-edges` would collide with brain `kb`'s edge index.
/// xerj-autoindex carries a matching copy (`detect::validate_brain`).
fn validate_brain(brain: &str) -> Result<(), String> {
    if brain.is_empty() {
        return Err("brain name must not be empty".into());
    }
    if brain.len() > 200 {
        return Err("brain name too long (max 200 chars)".into());
    }
    let first = brain.chars().next().unwrap();
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err("brain name must start with a lowercase letter or digit".into());
    }
    for c in brain.chars() {
        let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.');
        if !ok {
            return Err(format!(
                "brain name contains illegal character '{c}' (allowed: a-z 0-9 _ - .)"
            ));
        }
    }
    if brain.contains("..") {
        return Err("brain name must not contain '..'".into());
    }
    if brain.ends_with("-edges") {
        return Err("namespace suffix '-edges' is reserved for graph edge indices".into());
    }
    Ok(())
}

/// edge_id = xxh3_128("xg1\0" src "\0" type "\0" dst "\0" decimal(valid_at_ms)),
/// rendered as 32 lowercase hex chars ({:032x}). Byte-identical copy of
/// `xerj_autoindex::detect::edge_id` (SECOND_BRAIN_SPEC §2.3) — both crates pin
/// the same test vector so the two writers can never drift apart. Same
/// (src, type, dst, valid_at) → same `_id` → re-asserts overwrite instead of
/// duplicating; a DIFFERENT `valid_at` creates a distinct edge (bi-temporal
/// design, not a bug).
pub fn edge_id(src: &str, edge_type: &str, dst: &str, valid_at_ms: i64) -> String {
    use xxhash_rust::xxh3::xxh3_128;
    let mut input = Vec::with_capacity(16 + src.len() + edge_type.len() + dst.len());
    input.extend_from_slice(b"xg1\x00");
    input.extend_from_slice(src.as_bytes());
    input.push(0);
    input.extend_from_slice(edge_type.as_bytes());
    input.push(0);
    input.extend_from_slice(dst.as_bytes());
    input.push(0);
    input.extend_from_slice(valid_at_ms.to_string().as_bytes());
    format!("{:032x}", xxh3_128(&input))
}

/// The exact edges-index mapping body (SECOND_BRAIN_SPEC §2.1), byte-identical
/// to `xerj_autoindex::detect::edge_index_mapping` — the two writers must
/// agree or the first one to touch a brain decides the column types for
/// everyone. Timestamps are `epoch_millis` NUMBERS (never ISO strings — a
/// string would make the doc-values column a KeywordColumn and kill the as-of
/// compare on the hop path).
fn edge_index_mapping() -> Value {
    json!({
        "mappings": {
            "properties": {
                "edge_id":        { "type": "keyword" },
                "src":            { "type": "keyword" },
                "dst":            { "type": "keyword" },
                "type":           { "type": "keyword" },
                "weight":         { "type": "float" },
                "valid_at":       { "type": "date", "format": "epoch_millis" },
                "invalid_at":     { "type": "date", "format": "epoch_millis" },
                "created_at":     { "type": "date", "format": "epoch_millis" },
                "expired_at":     { "type": "date", "format": "epoch_millis" },
                "detector":       { "type": "keyword" },
                "confidence":     { "type": "float" },
                "schema_version": { "type": "integer" },
                "src_file":       { "type": "keyword" },
                "src_format":     { "type": "keyword" },
                "dst_format":     { "type": "keyword" },
                "evidence": {
                    "properties": {
                        "quote":  { "type": "text" },
                        "source": { "type": "keyword" },
                        "offset": { "type": "long" }
                    }
                }
            }
        }
    })
}

/// Emit a uniform graph error response (same shape as memory_api, typed
/// `graph_error`).
fn error_response(status: StatusCode, reason: impl Into<String>) -> Response {
    (
        status,
        Json(json!({
            "error": { "type": "graph_error", "reason": reason.into() },
            "status": status.as_u16(),
        })),
    )
        .into_response()
}

/// Drain an inner handler [`Response`] into `(status, json_body)` so this
/// module can inspect the reused ES-compat handler's result and re-shape it
/// into the graph contract. (Private copy of memory_api's helper, per the
/// adapter discipline: compose, don't widen visibility.)
async fn drain_json(resp: Response) -> (StatusCode, Value) {
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap_or_default();
    let value = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// Whether an index currently exists.
fn index_exists(state: &AppState, index: &str) -> bool {
    state.engine.get_index(index).is_ok()
}

/// Parse a timestamp that may be an epoch-ms JSON number or an RFC3339 string
/// (inputs accept both; outputs are always numbers).
fn parse_ms_value(v: &Value) -> Option<i64> {
    match v {
        Value::Number(_) => v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)),
        Value::String(s) => parse_ms_str(s),
        _ => None,
    }
}

/// Parse a query-string timestamp: decimal epoch-ms first, RFC3339 second.
fn parse_ms_str(s: &str) -> Option<i64> {
    if let Ok(n) = s.parse::<i64>() {
        return Some(n);
    }
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

/// Server "now" in epoch milliseconds.
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Ensure the edges index for a brain exists with the §2.1 mapping and its
/// §2.5 meta doc. Lazy — called on the write path (`link`) only; read
/// endpoints 404 on an unknown brain instead.
async fn ensure_edges_index(state: &AppState, brain: &str, index: &str) -> Result<(), Response> {
    if index_exists(state, index) {
        return Ok(());
    }
    let resp = es_compat::create_index(
        State(state.clone()),
        Path(index.to_string()),
        OptionalJson(Some(edge_index_mapping())),
    )
    .await
    .into_response();
    let (status, body) = drain_json(resp).await;
    // Tolerate a concurrent creator winning the race.
    if !status.is_success() && !index_exists(state, index) {
        return Err(error_response(
            status,
            format!("failed to create edges index for brain '{brain}': {body}"),
        ));
    }

    // §2.5 meta doc, create-if-absent (`op_type=create`): a racing writer's
    // meta doc must never be clobbered — it may carry a `nodes_index` some
    // reader already resolved through. A 409 conflict is exactly the state we
    // want; any other failure is surfaced.
    let meta = json!({
        "meta_version": 1,
        "brain": brain,
        "nodes_index": default_nodes_index(brain),
        "created_at": now_ms(),
    });
    let params = IndexDocParams {
        op_type: Some("create".into()),
        ..Default::default()
    };
    let resp = es_compat::index_doc(
        State(state.clone()),
        Path((index.to_string(), BRAIN_META_ID.to_string())),
        Query(params),
        Json(meta),
    )
    .await
    .into_response();
    let (status, body) = drain_json(resp).await;
    if !status.is_success() && status != StatusCode::CONFLICT {
        return Err(error_response(
            status,
            format!("failed to write brain meta doc for '{brain}': {body}"),
        ));
    }
    Ok(())
}

/// Resolve the nodes index for a brain: explicit request param → meta doc's
/// `nodes_index` → the `.xerj-memory-{brain}` default.
async fn resolve_nodes_index(state: &AppState, brain: &str, index: &str) -> String {
    let resp = es_compat::get_doc(
        State(state.clone()),
        Path((index.to_string(), BRAIN_META_ID.to_string())),
        Query(GetDocParams::default()),
    )
    .await
    .into_response();
    let (status, body) = drain_json(resp).await;
    if status.is_success() {
        if let Some(ni) = body.pointer("/_source/nodes_index").and_then(Value::as_str) {
            if !ni.is_empty() {
                return ni.to_string();
            }
        }
    }
    default_nodes_index(brain)
}

// ─────────────────────────────────────────────────────────────────────────────
// POST /_graph/{brain}/link — assert an edge
// ─────────────────────────────────────────────────────────────────────────────

/// Request body for `POST /_graph/{brain}/link` (SECOND_BRAIN_SPEC §4.1).
/// Unknown fields are rejected with a 400 — a typo'd key must never silently
/// drop an assertion detail.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkBody {
    pub src: String,
    pub dst: String,
    #[serde(rename = "type")]
    pub edge_type: String,
    /// Optional, default 1.0, clamped to [0, 1].
    #[serde(default)]
    pub weight: Option<f64>,
    /// Optional epoch-ms number or RFC3339 string; default = server now.
    #[serde(default)]
    pub valid_at: Option<Value>,
    /// Optional epoch-ms number or RFC3339 string; default = server now
    /// (fixtures/imports).
    #[serde(default)]
    pub created_at: Option<Value>,
    /// Optional, default 1.0, clamped to [0, 1].
    #[serde(default)]
    pub confidence: Option<f64>,
    /// Optional, default "manual@1".
    #[serde(default)]
    pub detector: Option<String>,
    /// Optional `{quote, source, offset}` object.
    #[serde(default)]
    pub evidence: Option<Value>,
}

/// `POST /_graph/{brain}/link` — assert an edge. Creates the edges index
/// lazily (§2.1 mapping + §2.5 meta doc). 201 on first assertion, 200 when
/// the same (src, type, dst, valid_at) is re-asserted (idempotent overwrite).
pub async fn link(
    State(state): State<AppState>,
    Path(brain): Path<String>,
    principal: Principal,
    body: OptionalJson<LinkBody>,
) -> Response {
    if let Err(reason) = validate_brain(&brain) {
        return error_response(StatusCode::BAD_REQUEST, reason);
    }
    // Before anything that could touch or create the brain (issue #79).
    if let Err(denied) = authz::authorize_brain(&principal, &brain, Privilege::WriteIndex) {
        return denied;
    }
    let Some(body) = body.0 else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "link requires a JSON body with `src`, `dst`, and `type`",
        );
    };
    if body.src.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "`src` must be a non-empty string");
    }
    if body.dst.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "`dst` must be a non-empty string");
    }
    if body.src == body.dst {
        return error_response(
            StatusCode::BAD_REQUEST,
            "self-edges are not allowed (src == dst)",
        );
    }
    if body.edge_type.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "`type` must be a non-empty string");
    }
    let weight = body.weight.unwrap_or(1.0);
    if !weight.is_finite() {
        return error_response(StatusCode::BAD_REQUEST, "`weight` must be a finite number");
    }
    let weight = weight.clamp(0.0, 1.0);
    let confidence = body.confidence.unwrap_or(1.0);
    if !confidence.is_finite() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "`confidence` must be a finite number",
        );
    }
    let confidence = confidence.clamp(0.0, 1.0);
    let detector = body.detector.unwrap_or_else(|| "manual@1".into());
    let now = now_ms();
    let valid_at = match &body.valid_at {
        None => now,
        Some(v) => match parse_ms_value(v) {
            Some(ms) => ms,
            None => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "`valid_at` must be an epoch-ms number or an RFC3339 string",
                );
            }
        },
    };
    let created_at = match &body.created_at {
        None => now,
        Some(v) => match parse_ms_value(v) {
            Some(ms) => ms,
            None => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "`created_at` must be an epoch-ms number or an RFC3339 string",
                );
            }
        },
    };
    if let Some(ev) = &body.evidence {
        if !ev.is_object() {
            return error_response(
                StatusCode::BAD_REQUEST,
                "`evidence` must be an object like {quote, source, offset}",
            );
        }
    }

    let index = edges_index(&brain);
    if let Err(resp) = ensure_edges_index(&state, &brain, &index).await {
        return resp;
    }

    // §2.4 stored document. §2.2 type discipline: scalars are plain
    // strings/numbers; `invalid_at`/`expired_at`/`src_file` are OMITTED when
    // unset (never null) — omission lands the row in the doc-values
    // null_bitmap, which is exactly the "still valid" signal the hop reads.
    let id = edge_id(&body.src, &body.edge_type, &body.dst, valid_at);
    let mut doc = json!({
        "edge_id": id,
        "src": body.src,
        "dst": body.dst,
        "type": body.edge_type,
        "weight": weight,
        "valid_at": valid_at,
        "created_at": created_at,
        "detector": detector,
        "confidence": confidence,
        "schema_version": 1,
    });
    if let Some(ev) = &body.evidence {
        // Top-level `src_file` mirrors evidence.source so invalidation-style
        // queries ride the keyword doc-values prefilter, matching the
        // autoindex writer's shape.
        if let Some(src_file) = ev.get("source").and_then(Value::as_str) {
            doc["src_file"] = json!(src_file);
        }
        doc["evidence"] = ev.clone();
    }

    let resp = es_compat::index_doc(
        State(state.clone()),
        Path((index.clone(), id.clone())),
        Query(IndexDocParams::default()),
        Json(doc.clone()),
    )
    .await
    .into_response();
    let (status, result_body) = drain_json(resp).await;
    if !status.is_success() {
        return error_response(status, format!("failed to store edge: {result_body}"));
    }
    let created = result_body.get("result").and_then(Value::as_str) == Some("created");
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    (
        status,
        Json(json!({
            "brain": brain,
            "edge_id": id,
            "created": created,
            "edge": doc,
        })),
    )
        .into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// DELETE /_graph/{brain}/link/{edge_id} — soft invalidate
// ─────────────────────────────────────────────────────────────────────────────

/// Query params for `DELETE /_graph/{brain}/link/{edge_id}`.
#[derive(Debug, Default, Deserialize)]
pub struct UnlinkParams {
    /// When the fact stopped being true (epoch-ms or RFC3339; default =
    /// server now). `expired_at` — when the system recorded that — is always
    /// server now; the two form the bi-temporal pair.
    #[serde(default)]
    pub invalid_at: Option<String>,
}

/// `DELETE /_graph/{brain}/link/{edge_id}` — soft-invalidate an edge. NEVER
/// removes the document: the same doc is re-indexed under the same `_id` with
/// `invalid_at`/`expired_at` added, keeping `as_of` time travel answerable.
pub async fn unlink(
    State(state): State<AppState>,
    Path((brain, edge_id)): Path<(String, String)>,
    principal: Principal,
    Query(params): Query<UnlinkParams>,
) -> Response {
    if let Err(reason) = validate_brain(&brain) {
        return error_response(StatusCode::BAD_REQUEST, reason);
    }
    // Ordered before the existence probe below so an unauthorized caller
    // cannot tell "not yours" from "not there" (issue #79).
    if let Err(denied) = authz::authorize_brain(&principal, &brain, Privilege::WriteIndex) {
        return denied;
    }
    let not_found = || {
        error_response(
            StatusCode::NOT_FOUND,
            format!("edge '{edge_id}' does not exist in brain '{brain}'"),
        )
    };
    let index = edges_index(&brain);
    if !index_exists(&state, &index) {
        return not_found();
    }
    let resp = es_compat::get_doc(
        State(state.clone()),
        Path((index.clone(), edge_id.clone())),
        Query(GetDocParams::default()),
    )
    .await
    .into_response();
    let (status, body) = drain_json(resp).await;
    if status == StatusCode::NOT_FOUND {
        return not_found();
    }
    if !status.is_success() {
        return error_response(status, format!("failed to read edge: {body}"));
    }
    let Some(source) = body.get("_source").cloned() else {
        return not_found();
    };

    // Already invalidated → idempotent no-op that reports the standing fact.
    if let Some(already) = source.get("invalid_at").and_then(parse_ms_value) {
        return Json(json!({
            "brain": brain,
            "edge_id": edge_id,
            "invalidated": false,
            "already_invalid_at": already,
        }))
        .into_response();
    }

    let now = now_ms();
    let invalid_at = match &params.invalid_at {
        None => now,
        Some(s) => match parse_ms_str(s) {
            Some(ms) => ms,
            None => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "`invalid_at` must be an epoch-ms number or an RFC3339 string",
                );
            }
        },
    };
    let mut doc = source;
    doc["invalid_at"] = json!(invalid_at);
    doc["expired_at"] = json!(now);

    let resp = es_compat::index_doc(
        State(state.clone()),
        Path((index, edge_id.clone())),
        Query(IndexDocParams::default()),
        Json(doc),
    )
    .await
    .into_response();
    let (status, body) = drain_json(resp).await;
    if !status.is_success() {
        return error_response(status, format!("failed to invalidate edge: {body}"));
    }
    Json(json!({
        "brain": brain,
        "edge_id": edge_id,
        "invalidated": true,
        "invalid_at": invalid_at,
        "expired_at": now,
    }))
    .into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// GET /_graph/{brain}/ego — neighborhood of one node
// ─────────────────────────────────────────────────────────────────────────────

/// Query params for `GET /_graph/{brain}/ego` (SECOND_BRAIN_SPEC §4.3).
#[derive(Debug, Default, Deserialize)]
pub struct EgoParams {
    /// The node to expand from. Exactly one of `node`/`nodes` is required.
    #[serde(default)]
    pub node: Option<String>,
    /// Comma-separated seed ids for multi-seed expansion (mutually exclusive
    /// with `node`; `node` is the 1-element case). Deduped; clamped to
    /// [`EGO_SEEDS_CAP`] with the clip counted in
    /// `not_shown.frontier_clipped`. This is the sanctioned drill-down
    /// iterator: feed a slice of the previous response's `reachable` back in.
    #[serde(default)]
    pub nodes: Option<String>,
    /// 1 (default) or 2; anything else is a 400 with the not-a-graph-database
    /// wording.
    #[serde(default)]
    pub hops: Option<u64>,
    /// `out` | `in` | `both` (default).
    #[serde(default)]
    pub direction: Option<String>,
    /// Comma-separated edge-type allowlist; absent = all types.
    #[serde(default)]
    pub types: Option<String>,
    /// Max returned edges, clamped 1..=1000 (default 100).
    #[serde(default)]
    pub limit: Option<u64>,
    /// Bi-temporal cut (epoch-ms or RFC3339; default now).
    #[serde(default)]
    pub as_of: Option<String>,
    /// Return soft-invalidated edges too (default false).
    #[serde(default)]
    pub include_expired: Option<bool>,
    /// Hydrate node summaries from the nodes index (default false).
    #[serde(default)]
    pub include_nodes: Option<bool>,
    /// Nodes index override; default = meta doc's `nodes_index`, else
    /// `.xerj-memory-{brain}`.
    #[serde(default)]
    pub nodes_index: Option<String>,
    /// Hydrate evidence/envelope for RETURNED edges (default true).
    #[serde(default)]
    pub include_evidence: Option<bool>,
}

/// `GET /_graph/{brain}/ego` — the bounded neighborhood of one node, or of up
/// to [`EGO_SEEDS_CAP`] seeds via `nodes=` (the engine expands a whole
/// frontier at frontier-size-independent cost, so multi-seed is the same one
/// bounded read). Traversal reads doc-values columns only; evidence and node
/// summaries are hydrated AFTER traversal, on the bounded result set, via
/// `ids` queries.
pub async fn ego(
    State(state): State<AppState>,
    Path(brain): Path<String>,
    principal: Principal,
    Query(params): Query<EgoParams>,
) -> Response {
    if let Err(reason) = validate_brain(&brain) {
        return error_response(StatusCode::BAD_REQUEST, reason);
    }
    if let Err(denied) = authz::authorize_brain(&principal, &brain, Privilege::ReadIndex) {
        return denied;
    }
    // `nodes_index` lets the caller redirect hydration at an arbitrary index,
    // so it is authorized separately — otherwise a grant on one brain would be
    // a read primitive for any index on the node.
    if let Some(nodes_index) = params.nodes_index.as_deref() {
        if let Err(denied) = authz::authorize_index(&principal, nodes_index, Privilege::ReadIndex) {
            return denied;
        }
    }
    if params.node.is_some() && params.nodes.is_some() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "`node` and `nodes` are mutually exclusive — `nodes` is the multi-seed form",
        );
    }
    // Seed list: `node` is the 1-element case of `nodes`. Deduped preserving
    // order (the order is part of the `reachable` contract), then clamped to
    // EGO_SEEDS_CAP with the clip counted — never silent.
    let raw_seeds: Vec<String> = match (params.node.as_deref(), params.nodes.as_deref()) {
        (Some(n), None) if !n.is_empty() => vec![n.to_string()],
        (None, Some(ns)) => ns
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        _ => Vec::new(),
    };
    let mut seeds: Vec<String> = Vec::with_capacity(raw_seeds.len());
    {
        let mut seen: HashSet<&str> = HashSet::with_capacity(raw_seeds.len());
        for id in &raw_seeds {
            if seen.insert(id.as_str()) {
                seeds.push(id.clone());
            }
        }
    }
    if seeds.is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "`node` (or comma-separated `nodes`) is required: the node id(s) to expand from",
        );
    }
    let mut seeds_clipped = 0u64;
    if seeds.len() > EGO_SEEDS_CAP {
        seeds_clipped = (seeds.len() - EGO_SEEDS_CAP) as u64;
        seeds.truncate(EGO_SEEDS_CAP);
    }
    let hops = params.hops.unwrap_or(1);
    if hops == 0 || hops > 2 {
        return error_response(StatusCode::BAD_REQUEST, GRAPH_HOPS_CAP_REASON);
    }
    let direction_str = params.direction.as_deref().unwrap_or("both");
    let direction = match direction_str {
        "out" => GraphDirection::Out,
        "in" => GraphDirection::In,
        "both" => GraphDirection::Both,
        other => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!("`direction` must be 'out', 'in', or 'both' (got '{other}')"),
            );
        }
    };
    let types: Option<Vec<String>> = params.types.as_deref().map(|t| {
        t.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    });
    let types = types.filter(|t| !t.is_empty());
    let limit = (params.limit.unwrap_or(100) as usize).clamp(1, MAX_EGO_LIMIT);
    let as_of = match params.as_of.as_deref() {
        None => now_ms(),
        Some(s) => match parse_ms_str(s) {
            Some(ms) => ms,
            None => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "`as_of` must be an epoch-ms number or an RFC3339 string",
                );
            }
        },
    };
    let include_expired = params.include_expired.unwrap_or(false);
    let include_nodes = params.include_nodes.unwrap_or(false);
    let include_evidence = params.include_evidence.unwrap_or(true);

    let index = edges_index(&brain);
    let Ok(idx) = state.engine.get_index(&index) else {
        return error_response(
            StatusCode::NOT_FOUND,
            format!("brain '{brain}' does not exist (no edges index)"),
        );
    };

    let req = GraphExpandRequest {
        frontier: seeds.clone(),
        hops: hops as u8,
        direction,
        types,
        as_of_ms: as_of,
        include_expired,
        max_result_edges: limit,
    };
    let result = match idx.graph_expand(&req) {
        Ok(r) => r,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, e.to_string()),
    };

    // Per-edge direction relative to the expansion: an edge discovered at hop
    // h is "out" iff its src was in hop h's frontier (an edge admitted by both
    // scans reports "out"). The hop-1 frontier is the seed set; the hop-2
    // frontier is every endpoint hop 1 discovered.
    let frontier1: HashSet<&str> = seeds.iter().map(String::as_str).collect();
    let mut frontier2: HashSet<&str> = HashSet::new();
    for e in result.edges.iter().filter(|e| e.hop == 1) {
        for id in [e.src.as_str(), e.dst.as_str()] {
            if !frontier1.contains(id) {
                frontier2.insert(id);
            }
        }
    }
    let edge_direction = |e: &GraphEdgeLite| -> &'static str {
        let frontier = if e.hop == 1 { &frontier1 } else { &frontier2 };
        if frontier.contains(e.src.as_str()) {
            "out"
        } else {
            "in"
        }
    };

    // Post-traversal hydration (bounded ≤ limit ≤ 1000): ONE `ids` search on
    // the edges index for evidence/envelope fields — rides the ids-prefilter
    // fast path; the hop itself never touched `_source`.
    let mut hydrated: HashMap<String, Value> = HashMap::new();
    if include_evidence && !result.edges.is_empty() {
        let ids: Vec<&str> = result.edges.iter().map(|e| e.edge_id.as_str()).collect();
        let search_body = EsSearchBody {
            query: Some(json!({ "ids": { "values": ids } })),
            size: ids.len(),
            ..Default::default()
        };
        let resp = es_compat::search(
            State(state.clone()),
            Path(index.clone()),
            Query(EsSearchQueryParams::default()),
            EsSearchJson(Some(search_body)),
        )
        .await
        .into_response();
        let (status, body) = drain_json(resp).await;
        if status.is_success() {
            if let Some(hits) = body.pointer("/hits/hits").and_then(Value::as_array) {
                for h in hits {
                    if let (Some(id), Some(src)) =
                        (h.get("_id").and_then(Value::as_str), h.get("_source"))
                    {
                        hydrated.insert(id.to_string(), src.clone());
                    }
                }
            }
        }
    }

    // Node summaries (`include_nodes=true`): ONE `ids` search on the nodes
    // index for every reachable id. Ids that resolve nowhere are DANGLING —
    // the edge is kept, the honesty is counted, and up to 50 ids are listed.
    let mut nodes_obj = Map::new();
    let mut dangling_nodes = 0u64;
    let mut dangling_ids: Vec<String> = Vec::new();
    if include_nodes {
        let nodes_index = match params.nodes_index.as_deref() {
            Some(ni) if !ni.is_empty() => ni.to_string(),
            _ => resolve_nodes_index(&state, &brain, &index).await,
        };
        // The meta doc's `nodes_index` is caller-controlled data (anyone with
        // write on the brain can point it anywhere), so hydration is
        // authorized against the RESOLVED index, not just the brain. Without
        // this, write on one brain would be a read primitive for every index
        // on the node.
        if let Err(denied) = authz::authorize_index(&principal, &nodes_index, Privilege::ReadIndex)
        {
            return denied;
        }
        let mut found: HashSet<String> = HashSet::new();
        let search_body = EsSearchBody {
            query: Some(json!({ "ids": { "values": result.reachable } })),
            size: result.reachable.len(),
            ..Default::default()
        };
        let resp = es_compat::search(
            State(state.clone()),
            Path(nodes_index.clone()),
            Query(EsSearchQueryParams::default()),
            EsSearchJson(Some(search_body)),
        )
        .await
        .into_response();
        let (status, body) = drain_json(resp).await;
        if status.is_success() {
            if let Some(hits) = body.pointer("/hits/hits").and_then(Value::as_array) {
                for h in hits {
                    let Some(id) = h.get("_id").and_then(Value::as_str) else {
                        continue;
                    };
                    let src = h.get("_source");
                    let title = src
                        .and_then(|s| s.get("title"))
                        .and_then(Value::as_str)
                        .map(Value::from)
                        .unwrap_or(Value::Null);
                    let preview = src
                        .and_then(|s| {
                            s.get("text")
                                .and_then(Value::as_str)
                                .or_else(|| s.get("body").and_then(Value::as_str))
                        })
                        .map(|t| Value::from(t.chars().take(160).collect::<String>()))
                        .unwrap_or(Value::Null);
                    // Truthful file label for the map/ledger: the autoindex
                    // writer stamps `ax_path` on every note; null when absent
                    // (manually-written nodes) — never fabricated.
                    let path = src
                        .and_then(|s| s.get("ax_path"))
                        .and_then(Value::as_str)
                        .map(Value::from)
                        .unwrap_or(Value::Null);
                    let hit_index = h
                        .get("_index")
                        .cloned()
                        .unwrap_or_else(|| json!(nodes_index));
                    found.insert(id.to_string());
                    nodes_obj.insert(
                        id.to_string(),
                        json!({ "title": title, "preview": preview, "path": path, "index": hit_index }),
                    );
                }
            }
        }
        // A missing/unsearchable nodes index means every reachable id is
        // dangling — reported, never fabricated.
        let mut dangling: Vec<&String> = result
            .reachable
            .iter()
            .filter(|id| !found.contains(*id))
            .collect();
        dangling.sort();
        dangling_nodes = dangling.len() as u64;
        dangling_ids = dangling
            .into_iter()
            .take(MAX_DANGLING_LISTED)
            .cloned()
            .collect();
    }

    // Edges in the §3.2 stable order (the engine already sorted), each
    // annotated with its expansion direction; `invalid_at` is JSON null when
    // unset (response-side null is fine — the omit-rule binds stored docs).
    let edges_json: Vec<Value> = result
        .edges
        .iter()
        .map(|e| {
            let mut obj = json!({
                "edge_id": e.edge_id,
                "src": e.src,
                "dst": e.dst,
                "type": e.edge_type,
                "weight": e.weight,
                "hop": e.hop,
                "direction": edge_direction(e),
                "valid_at": e.valid_at_ms,
                "invalid_at": e.invalid_at_ms.map(Value::from).unwrap_or(Value::Null),
            });
            if let Some(src) = hydrated.get(&e.edge_id) {
                for field in [
                    "created_at",
                    "detector",
                    "confidence",
                    "evidence",
                    "expired_at",
                ] {
                    if let Some(v) = src.get(field) {
                        obj[field] = v.clone();
                    }
                }
            }
            obj
        })
        .collect();

    // Neighbors: first-discovery order following the sorted edge list,
    // excluding the seed nodes; `via_edge` is the first sorted edge that
    // reached each one.
    let mut neighbors: Vec<Value> = Vec::new();
    let mut seen: HashSet<&str> = seeds.iter().map(String::as_str).collect();
    for e in &result.edges {
        for id in [e.src.as_str(), e.dst.as_str()] {
            if seen.insert(id) {
                neighbors.push(json!({ "id": id, "hop": e.hop, "via_edge": e.edge_id }));
            }
        }
    }

    // `seeds` echoes the ADMITTED seed list (post-dedupe, post-clamp) so the
    // caller can bookkeep exactly what was expanded; `node` stays on the
    // response whenever there is exactly one seed (the 1-element case keeps
    // its historical shape). Handler-clipped seeds fold into the same
    // `frontier_clipped` counter the engine uses — one honesty channel.
    let mut resp = json!({
        "brain": brain,
        "contract": GRAPH_CONTRACT,
        "seeds": seeds,
        "as_of": as_of,
        "hops": hops,
        "direction": direction_str,
        "edges": edges_json,
        "neighbors": neighbors,
        "not_shown": {
            "edges_clipped": result.stats.edges_clipped,
            "frontier_clipped": result.stats.frontier_clipped + seeds_clipped,
            "expired_excluded": result.stats.expired_excluded,
            "type_filtered": result.stats.type_filtered,
            "segments_without_columns": result.stats.segments_without_columns,
            "memtable_docs_scanned": result.stats.memtable_docs_scanned,
            "dangling_nodes": dangling_nodes,
            "dangling_ids": dangling_ids,
        }
    });
    if let [only] = resp["seeds"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        // §8.5 is a normative instance including key order: `node` sits
        // between `contract` and `seeds`, so rebuild in place rather than
        // appending (which would serialize `node` last).
        let only = only.clone();
        let old = std::mem::take(resp.as_object_mut().expect("ego response is an object"));
        let mut ordered = serde_json::Map::with_capacity(old.len() + 1);
        for (k, v) in old {
            if k == "seeds" {
                ordered.insert("node".to_string(), only.clone());
            }
            ordered.insert(k, v);
        }
        resp = Value::Object(ordered);
    }
    if include_nodes {
        resp["nodes"] = Value::Object(nodes_obj);
    }
    Json(resp).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// GET /_graph/{brain}/overview — brain-level stats (dashboard feed)
// ─────────────────────────────────────────────────────────────────────────────

/// Query params for `GET /_graph/{brain}/overview` (SECOND_BRAIN_SPEC §4.4).
#[derive(Debug, Default, Deserialize)]
pub struct OverviewParams {
    /// Bi-temporal cut for the live slice (epoch-ms or RFC3339; default now).
    #[serde(default)]
    pub as_of: Option<String>,
    /// Size of every top-N list, clamped 1..=50 (default 10).
    #[serde(default)]
    pub top: Option<u64>,
    /// `day` (default) | `hour`.
    #[serde(default)]
    pub histogram_interval: Option<String>,
}

/// Run one size-0 search on the edges index and return the drained JSON body
/// (or an error response). Composes `es_compat::search` — the aggregations are
/// the same terms/date_histogram machinery every dashboard already uses.
async fn overview_search(
    state: &AppState,
    index: &str,
    body: EsSearchBody,
) -> Result<Value, Response> {
    let resp = es_compat::search(
        State(state.clone()),
        Path(index.to_string()),
        Query(EsSearchQueryParams::default()),
        EsSearchJson(Some(body)),
    )
    .await
    .into_response();
    let (status, body) = drain_json(resp).await;
    if !status.is_success() {
        return Err(error_response(
            status,
            format!("overview search failed: {body}"),
        ));
    }
    Ok(body)
}

/// Terms-agg buckets → `[{"<key_name>": key, "<count_name>": doc_count}]`,
/// plus the agg's `sum_other_doc_count` (the not-listed tail, reported
/// in-band).
fn terms_list(aggs: &Value, agg: &str, key_name: &str, count_name: &str) -> (Vec<Value>, u64) {
    let buckets = aggs
        .pointer(&format!("/{agg}/buckets"))
        .and_then(Value::as_array);
    let list = buckets
        .map(|bs| {
            bs.iter()
                .map(|b| {
                    json!({
                        key_name: b.get("key").cloned().unwrap_or(Value::Null),
                        count_name: b.get("doc_count").cloned().unwrap_or(json!(0)),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let other = aggs
        .pointer(&format!("/{agg}/sum_other_doc_count"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    (list, other)
}

/// The honesty marker for the default embedder (§0 invariant 8): the built-in
/// embedder is LEXICAL feature-hashing — no surface may imply neural
/// semantics. When the node backing store is configured with a real external/
/// neural embedder, its configured id is reported verbatim instead.
fn embedder_id(state: &AppState) -> String {
    let emb = &state.config.embedding;
    match emb.mode.as_str() {
        "neural" => emb.neural_model.clone(),
        "proxy" if !emb.default_model.is_empty() => emb.default_model.clone(),
        "proxy" => "proxy".into(),
        "auto" if !emb.default_endpoint.is_empty() && !emb.default_model.is_empty() => {
            emb.default_model.clone()
        }
        "auto" if !emb.default_endpoint.is_empty() => "proxy".into(),
        _ => "lexical-feature-hash".into(),
    }
}

/// `GET /_graph/{brain}/overview` — totals, live slice (types/detectors/
/// hubs), the created-over-time histogram, and the notes total. Exactly three
/// composed searches on the edges index plus one size-0 count on the nodes
/// index; every top-N tail is reported in `not_shown`.
pub async fn overview(
    State(state): State<AppState>,
    Path(brain): Path<String>,
    principal: Principal,
    Query(params): Query<OverviewParams>,
) -> Response {
    if let Err(reason) = validate_brain(&brain) {
        return error_response(StatusCode::BAD_REQUEST, reason);
    }
    // Before the `exists: false` probe below, so a caller cannot map the
    // node's brains by watching 404 vs 403 (issue #79).
    if let Err(denied) = authz::authorize_brain(&principal, &brain, Privilege::ReadIndex) {
        return denied;
    }
    let as_of = match params.as_of.as_deref() {
        None => now_ms(),
        Some(s) => match parse_ms_str(s) {
            Some(ms) => ms,
            None => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "`as_of` must be an epoch-ms number or an RFC3339 string",
                );
            }
        },
    };
    let top = (params.top.unwrap_or(10) as usize).clamp(1, 50);
    let interval = params.histogram_interval.as_deref().unwrap_or("day");
    if !matches!(interval, "day" | "hour") {
        return error_response(
            StatusCode::BAD_REQUEST,
            "`histogram_interval` must be 'day' or 'hour'",
        );
    }

    let index = edges_index(&brain);
    if !index_exists(&state, &index) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "brain": brain, "contract": GRAPH_CONTRACT, "exists": false })),
        )
            .into_response();
    }

    // 1. Totals: every edge ever asserted (`exists src` excludes the meta doc).
    let totals = EsSearchBody {
        query: Some(json!({ "exists": { "field": "src" } })),
        size: 0,
        track_total_hits: Some(json!(true)),
        ..Default::default()
    };
    let totals = match overview_search(&state, &index, totals).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let total = totals
        .pointer("/hits/total/value")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    // 2. Live slice at as_of, with the type/detector/hub breakdowns.
    let live_body = EsSearchBody {
        query: Some(json!({
            "bool": {
                "filter": [
                    { "exists": { "field": "src" } },
                    { "range": { "valid_at": { "lte": as_of } } }
                ],
                "must_not": [
                    { "range": { "invalid_at": { "lte": as_of } } }
                ]
            }
        })),
        size: 0,
        track_total_hits: Some(json!(true)),
        aggs: Some(json!({
            "by_type":     { "terms": { "field": "type",     "size": top } },
            "by_detector": { "terms": { "field": "detector", "size": top } },
            "top_src":     { "terms": { "field": "src",      "size": top } },
            "top_dst":     { "terms": { "field": "dst",      "size": top } },
        })),
        ..Default::default()
    };
    let live_resp = match overview_search(&state, &index, live_body).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let live = live_resp
        .pointer("/hits/total/value")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let aggs = live_resp
        .get("aggregations")
        .cloned()
        .unwrap_or(Value::Null);
    let (types, types_other) = terms_list(&aggs, "by_type", "type", "live");
    let (detectors, detectors_other) = terms_list(&aggs, "by_detector", "detector", "live");
    let (hubs_out, hubs_out_other) = terms_list(&aggs, "top_src", "id", "live_edges");
    let (hubs_in, hubs_in_other) = terms_list(&aggs, "top_dst", "id", "live_edges");

    // 3. Created-over-time histogram (all asserted edges, not just live —
    // the timeline shows assertion activity, invalidation does not erase it).
    let timeline_body = EsSearchBody {
        query: Some(json!({ "exists": { "field": "src" } })),
        size: 0,
        aggs: Some(json!({
            "created": {
                "date_histogram": { "field": "created_at", "calendar_interval": interval }
            }
        })),
        ..Default::default()
    };
    let timeline = match overview_search(&state, &index, timeline_body).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let created_over_time: Vec<Value> = timeline
        .pointer("/aggregations/created/buckets")
        .and_then(Value::as_array)
        .map(|bs| {
            bs.iter()
                .map(|b| {
                    json!({
                        "t": b.get("key").cloned().unwrap_or(Value::Null),
                        "count": b.get("doc_count").cloned().unwrap_or(json!(0)),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let nodes_index = resolve_nodes_index(&state, &brain, &index).await;

    // 4. Notes total: one size-0 count on the nodes index. A brain whose
    // nodes index was never created (edges asserted through the API alone)
    // truthfully has 0 stored notes — reported as such, not a 404 and never
    // fabricated from edge endpoints.
    //
    // The nodes index comes from the brain's meta doc, which is writable by
    // anyone with write on the brain — so it is authorized in its own right
    // (see the matching check in `ego`).
    if let Err(denied) = authz::authorize_index(&principal, &nodes_index, Privilege::ReadIndex) {
        return denied;
    }
    let nodes_total = if index_exists(&state, &nodes_index) {
        let count_body = EsSearchBody {
            query: Some(json!({ "match_all": {} })),
            size: 0,
            track_total_hits: Some(json!(true)),
            ..Default::default()
        };
        match overview_search(&state, &nodes_index, count_body).await {
            Ok(v) => v
                .pointer("/hits/total/value")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            Err(r) => return r,
        }
    } else {
        0
    };

    Json(json!({
        "brain": brain,
        "contract": GRAPH_CONTRACT,
        "exists": true,
        "as_of": as_of,
        "nodes_index": nodes_index,
        "nodes": { "total": nodes_total },
        "embedder": embedder_id(&state),
        "edges": {
            "total": total,
            "live": live,
            "invalidated": total.saturating_sub(live),
        },
        "types": types,
        "detectors": detectors,
        "hubs": { "out": hubs_out, "in": hubs_in },
        "created_over_time": created_over_time,
        "not_shown": {
            "types_not_listed": types_other,
            "detectors_not_listed": detectors_other,
            "hubs_out_not_listed": hubs_out_other,
            "hubs_in_not_listed": hubs_in_other,
        }
    }))
    .into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests — the SECOND_BRAIN_SPEC §8 fixture, end to end through the handlers
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use xerj_common::{config::Config, metrics::Metrics};
    use xerj_engine::Engine;

    const T0: i64 = 1_753_600_000_000; // fixture valid_at / created_at
    const AS_OF: i64 = 1_753_700_000_000; // fixture as-of instant
    const INVALID_AT: i64 = 1_753_650_000_000; // §8.6 invalidation instant

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

    async fn do_link(state: &AppState, brain: &str, body: Value) -> (StatusCode, Value) {
        let b: LinkBody = serde_json::from_value(body).unwrap();
        let resp = link(
            State(state.clone()),
            Path(brain.to_string()),
            Principal::Superuser,
            OptionalJson(Some(b)),
        )
        .await;
        drain_json(resp).await
    }

    async fn do_unlink(
        state: &AppState,
        brain: &str,
        edge_id: &str,
        invalid_at: Option<&str>,
    ) -> (StatusCode, Value) {
        let resp = unlink(
            State(state.clone()),
            Path((brain.to_string(), edge_id.to_string())),
            Principal::Superuser,
            Query(UnlinkParams {
                invalid_at: invalid_at.map(String::from),
            }),
        )
        .await;
        drain_json(resp).await
    }

    async fn do_ego(state: &AppState, brain: &str, params: EgoParams) -> (StatusCode, Value) {
        let resp = ego(
            State(state.clone()),
            Path(brain.to_string()),
            Principal::Superuser,
            Query(params),
        )
        .await;
        drain_json(resp).await
    }

    fn ego_params(node: &str, as_of: i64) -> EgoParams {
        EgoParams {
            node: Some(node.to_string()),
            as_of: Some(as_of.to_string()),
            ..Default::default()
        }
    }

    /// The eight §8.3 fixture edges as `link` bodies (explicit timestamps so
    /// the fixture is reproducible; src_file = evidence.source throughout).
    fn fixture_links() -> Vec<Value> {
        let alpha_line = "Alpha is the hub note. It links to [[beta]] and [[gamma]].";
        let wiki = |src: &str, dst: &str, quote: &str, source: &str, offset: u64| {
            json!({
                "src": src, "dst": dst, "type": "wikilink",
                "weight": 1.0, "confidence": 0.95, "detector": "wikilink@1",
                "valid_at": T0, "created_at": T0,
                "evidence": { "quote": quote, "source": source, "offset": offset }
            })
        };
        let dir = |src: &str, dst: &str, quote: &str, source: &str| {
            json!({
                "src": src, "dst": dst, "type": "same_dir",
                "weight": 0.3, "confidence": 0.4, "detector": "samedir@1",
                "valid_at": T0, "created_at": T0,
                "evidence": { "quote": quote, "source": source, "offset": 0 }
            })
        };
        vec![
            wiki("note-alpha", "note-beta", alpha_line, "alpha.md", 35),
            wiki("note-alpha", "note-gamma", alpha_line, "alpha.md", 48),
            wiki(
                "note-beta",
                "note-gamma",
                "Beta continues the thread and references [[gamma]].",
                "beta.md",
                41,
            ),
            wiki(
                "note-delta",
                "note-alpha",
                "Delta cites [[alpha]] as its source.",
                "delta.md",
                12,
            ),
            dir(
                "note-alpha",
                "note-beta",
                "alpha.md and beta.md share directory .",
                "alpha.md",
            ),
            dir(
                "note-beta",
                "note-delta",
                "beta.md and delta.md share directory .",
                "beta.md",
            ),
            dir(
                "note-delta",
                "note-epsilon",
                "delta.md and epsilon.md share directory .",
                "delta.md",
            ),
            dir(
                "note-epsilon",
                "note-gamma",
                "epsilon.md and gamma.md share directory .",
                "epsilon.md",
            ),
        ]
    }

    /// §8.3 edge ids, in table order.
    const FIXTURE_IDS: [&str; 8] = [
        "bef814a75bd3d914c3e561f610154304",
        "11c2d0ef216cd6e99a3907a0b53c1452",
        "9bbf7d2068321ac0fa71d95e21fae2fd",
        "cead55986c364ad5ff6f0894daf61f77",
        "63b747655365aa16d38188aa49966f40",
        "a61e6caacb5e485baf6d45184f23ec67",
        "3efff61b58c978943e6fd2a1e4eeaee8",
        "7c07cdc441f0a3faa29be8946df3e7a4",
    ];

    async fn link_fixture(state: &AppState) {
        for (i, body) in fixture_links().into_iter().enumerate() {
            let (s, b) = do_link(state, "notes", body).await;
            assert_eq!(s, StatusCode::CREATED, "fixture edge {i} must create: {b}");
            assert_eq!(
                b["edge_id"].as_str().unwrap(),
                FIXTURE_IDS[i],
                "fixture edge {i} id must match the §8.3 pin"
            );
        }
    }

    /// §2.3 pin vector — keeps this copy of `edge_id` in lockstep with the
    /// autoindex copy.
    #[test]
    fn edge_id_pin_vector() {
        assert_eq!(
            edge_id("note-alpha", "wikilink", "note-beta", 1753600000000),
            "bef814a75bd3d914c3e561f610154304"
        );
    }

    #[test]
    fn brain_validation_rejects_the_edges_suffix() {
        assert!(validate_brain("notes").is_ok());
        assert!(validate_brain("kb-edges").is_err());
        assert!(validate_brain("").is_err());
        assert!(validate_brain("Notes").is_err());
        assert!(validate_brain("a..b").is_err());
    }

    /// §4.1: link creates (201) with the pinned edge_id; a re-assert of the
    /// same (src, type, dst, valid_at) overwrites (200, created=false);
    /// self-edges are a 400 with the normative reason.
    #[tokio::test]
    async fn link_pins_edge_ids_and_is_idempotent() {
        let state = test_state();
        link_fixture(&state).await;

        // Re-assert edge #1 → overwrite, not duplicate.
        let (s, b) = do_link(&state, "notes", fixture_links()[0].clone()).await;
        assert_eq!(s, StatusCode::OK, "re-assert is an overwrite: {b}");
        assert_eq!(b["created"], json!(false));
        assert_eq!(b["edge_id"].as_str().unwrap(), FIXTURE_IDS[0]);
        // The stored doc carries the §2.4 envelope, with src_file mirrored
        // from evidence.source and NO invalid_at/expired_at keys.
        let edge = &b["edge"];
        assert_eq!(edge["src_file"], json!("alpha.md"));
        assert_eq!(edge["schema_version"], json!(1));
        assert!(edge.get("invalid_at").is_none());
        assert!(edge.get("expired_at").is_none());

        let (s, b) = do_link(
            &state,
            "notes",
            json!({ "src": "n1", "dst": "n1", "type": "t" }),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(
            b["error"]["reason"],
            json!("self-edges are not allowed (src == dst)")
        );

        // The brain named like an edges index is rejected up front.
        let (s, _) = do_link(
            &state,
            "kb-edges",
            json!({ "src": "a", "dst": "b", "type": "t" }),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    /// §8.5: the ego response is byte-normative (only `memtable_docs_scanned`
    /// is flush-state-dependent, asserted ≥ 0 and then normalized).
    #[tokio::test]
    async fn ego_matches_the_fixture_response() {
        let state = test_state();
        link_fixture(&state).await;

        let (s, mut got) = do_ego(&state, "notes", ego_params("note-alpha", AS_OF)).await;
        assert_eq!(s, StatusCode::OK, "{got}");
        assert!(
            got["not_shown"]["memtable_docs_scanned"].as_u64().is_some(),
            "memtable_docs_scanned must be present (≥0)"
        );
        got["not_shown"]["memtable_docs_scanned"] = json!(0);

        // §8.5 is normative INCLUDING top-level key order: `node` sits between
        // `contract` and `seeds` on the wire (Value equality alone can't see
        // an order regression).
        let keys: Vec<&str> = got
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "brain",
                "contract",
                "node",
                "seeds",
                "as_of",
                "hops",
                "direction",
                "edges",
                "neighbors",
                "not_shown"
            ]
        );

        let expected = json!({
            "brain": "notes",
            "contract": "xerj-second-brain/1",
            "node": "note-alpha",
            "seeds": ["note-alpha"],
            "as_of": 1753700000000i64,
            "hops": 1,
            "direction": "both",
            "edges": [
                { "edge_id": "11c2d0ef216cd6e99a3907a0b53c1452", "src": "note-alpha", "dst": "note-gamma",
                  "type": "wikilink", "weight": 1.0, "hop": 1, "direction": "out",
                  "valid_at": 1753600000000i64, "invalid_at": null, "created_at": 1753600000000i64,
                  "detector": "wikilink@1", "confidence": 0.95,
                  "evidence": { "quote": "Alpha is the hub note. It links to [[beta]] and [[gamma]].", "source": "alpha.md", "offset": 48 } },
                { "edge_id": "bef814a75bd3d914c3e561f610154304", "src": "note-alpha", "dst": "note-beta",
                  "type": "wikilink", "weight": 1.0, "hop": 1, "direction": "out",
                  "valid_at": 1753600000000i64, "invalid_at": null, "created_at": 1753600000000i64,
                  "detector": "wikilink@1", "confidence": 0.95,
                  "evidence": { "quote": "Alpha is the hub note. It links to [[beta]] and [[gamma]].", "source": "alpha.md", "offset": 35 } },
                { "edge_id": "cead55986c364ad5ff6f0894daf61f77", "src": "note-delta", "dst": "note-alpha",
                  "type": "wikilink", "weight": 1.0, "hop": 1, "direction": "in",
                  "valid_at": 1753600000000i64, "invalid_at": null, "created_at": 1753600000000i64,
                  "detector": "wikilink@1", "confidence": 0.95,
                  "evidence": { "quote": "Delta cites [[alpha]] as its source.", "source": "delta.md", "offset": 12 } },
                { "edge_id": "63b747655365aa16d38188aa49966f40", "src": "note-alpha", "dst": "note-beta",
                  "type": "same_dir", "weight": 0.3, "hop": 1, "direction": "out",
                  "valid_at": 1753600000000i64, "invalid_at": null, "created_at": 1753600000000i64,
                  "detector": "samedir@1", "confidence": 0.4,
                  "evidence": { "quote": "alpha.md and beta.md share directory .", "source": "alpha.md", "offset": 0 } }
            ],
            "neighbors": [
                { "id": "note-gamma", "hop": 1, "via_edge": "11c2d0ef216cd6e99a3907a0b53c1452" },
                { "id": "note-beta",  "hop": 1, "via_edge": "bef814a75bd3d914c3e561f610154304" },
                { "id": "note-delta", "hop": 1, "via_edge": "cead55986c364ad5ff6f0894daf61f77" }
            ],
            "not_shown": {
                "edges_clipped": 0, "frontier_clipped": 0, "expired_excluded": 0,
                "type_filtered": 0, "segments_without_columns": 0,
                "memtable_docs_scanned": 0, "dangling_nodes": 0, "dangling_ids": []
            }
        });
        assert_eq!(got, expected, "ego response must match §8.5 exactly");

        // Dangling honesty (§8.5 tail): with no node docs stored,
        // include_nodes=true returns an empty `nodes` object and lists every
        // reachable id as dangling, sorted.
        let mut p = ego_params("note-alpha", AS_OF);
        p.include_nodes = Some(true);
        let (s, got) = do_ego(&state, "notes", p).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(got["nodes"], json!({}));
        assert_eq!(got["not_shown"]["dangling_nodes"], json!(4));
        assert_eq!(
            got["not_shown"]["dangling_ids"],
            json!(["note-alpha", "note-beta", "note-delta", "note-gamma"])
        );
    }

    /// §8.6: soft invalidation + as_of time travel + include_expired, and the
    /// idempotent second DELETE.
    #[tokio::test]
    async fn soft_invalidate_time_travel() {
        let state = test_state();
        link_fixture(&state).await;

        let (s, b) = do_unlink(
            &state,
            "notes",
            FIXTURE_IDS[0],
            Some(&INVALID_AT.to_string()),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["invalidated"], json!(true));
        assert_eq!(b["invalid_at"], json!(INVALID_AT));
        assert!(b["expired_at"].as_i64().unwrap() > 0);

        // Second DELETE: reports the standing invalidation, changes nothing.
        let (s, b) = do_unlink(&state, "notes", FIXTURE_IDS[0], None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(b["invalidated"], json!(false));
        assert_eq!(b["already_invalid_at"], json!(INVALID_AT));

        // Belief BEFORE the invalidation instant: all four §8.5 edges.
        let (_, before) =
            do_ego(&state, "notes", ego_params("note-alpha", 1_753_640_000_000)).await;
        assert_eq!(before["edges"].as_array().unwrap().len(), 4);
        assert_eq!(before["not_shown"]["expired_excluded"], json!(0));

        // Belief AFTER: three edges, the exclusion counted.
        let (_, after) = do_ego(&state, "notes", ego_params("note-alpha", AS_OF)).await;
        let ids: Vec<&str> = after["edges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["edge_id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            vec![FIXTURE_IDS[1], FIXTURE_IDS[3], FIXTURE_IDS[4]],
            "invalidated edge must disappear at as_of >= invalid_at"
        );
        assert_eq!(after["not_shown"]["expired_excluded"], json!(1));

        // include_expired=true: the edge returns, carrying its bi-temporal pair.
        let mut p = ego_params("note-alpha", AS_OF);
        p.include_expired = Some(true);
        let (_, all) = do_ego(&state, "notes", p).await;
        let edges = all["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 4);
        let inv = edges
            .iter()
            .find(|e| e["edge_id"] == json!(FIXTURE_IDS[0]))
            .expect("include_expired returns the invalidated edge");
        assert_eq!(inv["invalid_at"], json!(INVALID_AT));
        assert!(inv["expired_at"].as_i64().unwrap() > 0);

        // Unknown edge → 404 with the normative message.
        let (s, b) = do_unlink(&state, "notes", "deadbeef", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        assert!(b["error"]["reason"]
            .as_str()
            .unwrap()
            .contains("does not exist in brain 'notes'"));
    }

    /// §3.5/§4.6: a 3-hop request is a 400 carrying the not-a-graph-database
    /// sentence; hops=0 likewise; an unknown brain is a 404.
    #[tokio::test]
    async fn ego_bounds_and_unknown_brain() {
        let state = test_state();
        link_fixture(&state).await;

        for bad_hops in [0u64, 3] {
            let mut p = ego_params("note-alpha", AS_OF);
            p.hops = Some(bad_hops);
            let (s, b) = do_ego(&state, "notes", p).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "hops={bad_hops}");
            assert!(
                b["error"]["reason"]
                    .as_str()
                    .unwrap()
                    .contains("not a graph database"),
                "hops={bad_hops} must carry the §4.6 sentence: {b}"
            );
        }

        let (s, _) = do_ego(&state, "nope", ego_params("x", AS_OF)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);

        // hops=2 composes: the fixture's full 8-edge closure from note-alpha.
        let mut p = ego_params("note-alpha", AS_OF);
        p.hops = Some(2);
        let (s, b) = do_ego(&state, "notes", p).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(b["edges"].as_array().unwrap().len(), 8);
    }

    /// §4.4: overview counts for the fixture, before and after one
    /// invalidation; unknown brain → exists:false 404.
    #[tokio::test]
    async fn overview_counts_the_fixture() {
        let state = test_state();
        link_fixture(&state).await;

        let ov = |as_of: i64| {
            let state = state.clone();
            async move {
                let resp = overview(
                    State(state.clone()),
                    Path("notes".to_string()),
                    Principal::Superuser,
                    Query(OverviewParams {
                        as_of: Some(as_of.to_string()),
                        ..Default::default()
                    }),
                )
                .await;
                drain_json(resp).await
            }
        };

        let (s, b) = ov(AS_OF).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["contract"], json!(GRAPH_CONTRACT));
        assert_eq!(b["exists"], json!(true));
        assert_eq!(b["nodes_index"], json!(".xerj-memory-notes"));
        assert_eq!(b["embedder"], json!("lexical-feature-hash"));
        // No nodes index exists for this brain yet → 0 stored notes, honestly.
        assert_eq!(b["nodes"], json!({ "total": 0 }));
        assert_eq!(
            b["edges"],
            json!({ "total": 8, "live": 8, "invalidated": 0 })
        );
        let type_counts: BTreeMap<&str, u64> = b["types"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["type"].as_str().unwrap(), t["live"].as_u64().unwrap()))
            .collect();
        assert_eq!(
            type_counts,
            BTreeMap::from([("wikilink", 4), ("same_dir", 4)])
        );
        let det_counts: BTreeMap<&str, u64> = b["detectors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| (d["detector"].as_str().unwrap(), d["live"].as_u64().unwrap()))
            .collect();
        assert_eq!(
            det_counts,
            BTreeMap::from([("wikilink@1", 4), ("samedir@1", 4)])
        );
        // Hubs: note-alpha teaches 3 outgoing edges; note-gamma receives 3.
        assert_eq!(
            b["hubs"]["out"][0],
            json!({ "id": "note-alpha", "live_edges": 3 })
        );
        assert_eq!(
            b["hubs"]["in"][0],
            json!({ "id": "note-gamma", "live_edges": 3 })
        );
        // One day bucket holds all 8 assertions (T0 floored to its UTC day).
        assert_eq!(
            b["created_over_time"],
            json!([{ "t": 1753574400000i64, "count": 8 }])
        );
        assert_eq!(
            b["not_shown"],
            json!({
                "types_not_listed": 0, "detectors_not_listed": 0,
                "hubs_out_not_listed": 0, "hubs_in_not_listed": 0
            })
        );

        // Invalidate one edge: total stays 8 (nothing is deleted), live drops.
        let (_, r) = do_unlink(
            &state,
            "notes",
            FIXTURE_IDS[0],
            Some(&INVALID_AT.to_string()),
        )
        .await;
        assert_eq!(r["invalidated"], json!(true));
        let (_, b) = ov(AS_OF).await;
        assert_eq!(
            b["edges"],
            json!({ "total": 8, "live": 7, "invalidated": 1 })
        );
        // …but the belief BEFORE the invalidation still counts all 8 live.
        let (_, b) = ov(1_753_640_000_000).await;
        assert_eq!(
            b["edges"],
            json!({ "total": 8, "live": 8, "invalidated": 0 })
        );

        let resp = overview(
            State(state.clone()),
            Path("nope".to_string()),
            Principal::Superuser,
            Query(OverviewParams::default()),
        )
        .await;
        let (s, b) = drain_json(resp).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        assert_eq!(
            b,
            json!({ "brain": "nope", "contract": GRAPH_CONTRACT, "exists": false })
        );
    }

    /// The wire-level contract for multi-seed: `?nodes=a,b` arrives through
    /// the real axum `Query` extractor as ONE comma-separated string (the
    /// urlencoded parser does not split on commas) — this pins that.
    #[test]
    fn ego_params_parse_from_a_real_query_string() {
        let uri: axum::http::Uri =
            "/_graph/notes/ego?nodes=note-beta,note-epsilon&hops=2&as_of=1753700000000"
                .parse()
                .unwrap();
        let Query(p) = Query::<EgoParams>::try_from_uri(&uri).unwrap();
        assert_eq!(p.nodes.as_deref(), Some("note-beta,note-epsilon"));
        assert_eq!(p.node, None);
        assert_eq!(p.hops, Some(2));
        assert_eq!(p.as_of.as_deref(), Some("1753700000000"));
    }

    /// Multi-seed ego (`nodes=`): the union of the seed neighborhoods in the
    /// §3.2 stable order, directions relative to the seed SET, neighbors
    /// excluding every seed, `seeds` echoed, `node` absent (>1 seed).
    #[tokio::test]
    async fn ego_multi_seed_unions_neighborhoods() {
        let state = test_state();
        link_fixture(&state).await;

        let p = EgoParams {
            nodes: Some("note-beta,note-epsilon".into()),
            as_of: Some(AS_OF.to_string()),
            ..Default::default()
        };
        let (s, b) = do_ego(&state, "notes", p).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["seeds"], json!(["note-beta", "note-epsilon"]));
        assert!(
            b.get("node").is_none(),
            "multi-seed response must not carry a single `node`: {b}"
        );

        // beta touches fixture edges 0/2/4/5; epsilon touches 6/7 — six edges
        // in (hop asc, weight desc, edge_id asc) order, direction "out" iff
        // the edge's src is one of the seeds.
        let got: Vec<(&str, &str)> = b["edges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["edge_id"].as_str().unwrap(),
                    e["direction"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (FIXTURE_IDS[2], "out"), // beta → gamma (wikilink)
                (FIXTURE_IDS[0], "in"),  // alpha → beta (wikilink)
                (FIXTURE_IDS[6], "in"),  // delta → epsilon (same_dir)
                (FIXTURE_IDS[4], "in"),  // alpha → beta (same_dir)
                (FIXTURE_IDS[7], "out"), // epsilon → gamma (same_dir)
                (FIXTURE_IDS[5], "out"), // beta → delta (same_dir)
            ]
        );
        let neighbor_ids: Vec<&str> = b["neighbors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap())
            .collect();
        assert_eq!(neighbor_ids, vec!["note-gamma", "note-alpha", "note-delta"]);
        assert_eq!(b["not_shown"]["frontier_clipped"], json!(0));

        // node+nodes together → 400 (mutually exclusive).
        let p = EgoParams {
            node: Some("note-alpha".into()),
            nodes: Some("note-beta".into()),
            as_of: Some(AS_OF.to_string()),
            ..Default::default()
        };
        let (s, b) = do_ego(&state, "notes", p).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(b["error"]["reason"]
            .as_str()
            .unwrap()
            .contains("mutually exclusive"));

        // An all-empty seed list → the same 400 as a missing `node`.
        let p = EgoParams {
            nodes: Some(", ,".into()),
            ..Default::default()
        };
        let (s, _) = do_ego(&state, "notes", p).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);

        // A 1-element `nodes` IS the `node` case: `node` stays on the shape.
        let p = EgoParams {
            nodes: Some("note-alpha".into()),
            as_of: Some(AS_OF.to_string()),
            ..Default::default()
        };
        let (s, b) = do_ego(&state, "notes", p).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(b["node"], json!("note-alpha"));
        assert_eq!(b["seeds"], json!(["note-alpha"]));
    }

    /// A seed list beyond EGO_SEEDS_CAP is clipped — never silently — into
    /// the same `frontier_clipped` counter the engine uses; duplicates dedupe
    /// before the cap so they cost nothing.
    #[tokio::test]
    async fn ego_seed_cap_is_counted() {
        let state = test_state();
        link_fixture(&state).await;

        // 70 distinct seeds (note-alpha first + 69 unknowns) → 64 kept, 6
        // clipped; input order preserved so note-alpha survives the clamp.
        let mut ids: Vec<String> = vec!["note-alpha".into()];
        ids.extend((0..69).map(|i| format!("ghost-{i:02}")));
        let p = EgoParams {
            nodes: Some(ids.join(",")),
            as_of: Some(AS_OF.to_string()),
            ..Default::default()
        };
        let (s, b) = do_ego(&state, "notes", p).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["seeds"].as_array().unwrap().len(), 64);
        assert_eq!(b["seeds"][0], json!("note-alpha"));
        assert_eq!(b["not_shown"]["frontier_clipped"], json!(6));
        assert_eq!(b["edges"].as_array().unwrap().len(), 4);

        // 100 copies of one id are one seed — dedupe precedes the cap.
        let p = EgoParams {
            nodes: Some(vec!["note-alpha"; 100].join(",")),
            as_of: Some(AS_OF.to_string()),
            ..Default::default()
        };
        let (s, b) = do_ego(&state, "notes", p).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(b["seeds"], json!(["note-alpha"]));
        assert_eq!(b["not_shown"]["frontier_clipped"], json!(0));
    }

    /// A2 + A3: `overview.nodes.total` counts stored notes once the nodes
    /// index exists (0, honestly, before that), and ego node hydration
    /// carries `path` from `ax_path` (null when the note has none).
    #[tokio::test]
    async fn overview_nodes_total_and_hydrated_path() {
        let state = test_state();
        link_fixture(&state).await;

        async fn put_node(state: &AppState, id: &str, doc: Value) {
            let resp = es_compat::index_doc(
                State(state.clone()),
                Path((".xerj-memory-notes".to_string(), id.to_string())),
                Query(IndexDocParams::default()),
                Json(doc),
            )
            .await
            .into_response();
            let (s, b) = drain_json(resp).await;
            assert!(s.is_success(), "storing node {id} failed: {b}");
        }
        async fn ov(state: &AppState) -> Value {
            let resp = overview(
                State(state.clone()),
                Path("notes".to_string()),
                Principal::Superuser,
                Query(OverviewParams {
                    as_of: Some(AS_OF.to_string()),
                    ..Default::default()
                }),
            )
            .await;
            drain_json(resp).await.1
        }

        // Before any node doc exists: total 0 (no nodes index — not a 404).
        assert_eq!(ov(&state).await["nodes"], json!({ "total": 0 }));

        put_node(
            &state,
            "note-alpha",
            json!({
                "title": "Alpha", "text": "Alpha is the hub note.",
                "ax_path": "notes/alpha.md", "ax_format": "md"
            }),
        )
        .await;
        put_node(
            &state,
            "note-beta",
            json!({ "title": "Beta", "body": "Beta continues the thread." }),
        )
        .await;

        assert_eq!(ov(&state).await["nodes"], json!({ "total": 2 }));

        let mut p = ego_params("note-alpha", AS_OF);
        p.include_nodes = Some(true);
        let (s, b) = do_ego(&state, "notes", p).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(
            b["nodes"]["note-alpha"],
            json!({
                "title": "Alpha",
                "preview": "Alpha is the hub note.",
                "path": "notes/alpha.md",
                "index": ".xerj-memory-notes"
            })
        );
        // No ax_path on beta → null, never guessed.
        assert_eq!(b["nodes"]["note-beta"]["path"], json!(null));
        // gamma + delta still dangle — counted and listed.
        assert_eq!(b["not_shown"]["dangling_nodes"], json!(2));
        assert_eq!(
            b["not_shown"]["dangling_ids"],
            json!(["note-delta", "note-gamma"])
        );
    }
}
