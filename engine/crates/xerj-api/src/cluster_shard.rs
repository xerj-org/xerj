//! Shard routing for the ES-compat document and search handlers.
//!
//! With a cluster handle attached, a replicated index carries a primary plan
//! in the same Raft entry as the create. [`dispatch_index`] forwards a
//! document to the owner [`Engine::route_write`] names. [`dispatch_update`]
//! and [`bulk_on_primaries`] send `_update` and `_bulk` to that same owner.
//! [`update_by_query_on_primaries`] runs `_update_by_query` on every primary
//! and each primary copies the documents it changed.
//! [`delete_by_query_on_primaries`] runs `_delete_by_query` on every primary
//! and each primary copies the deletes.
//! The primary copies an index, delete, or the document an update stored
//! onto the in-sync replicas and acknowledges the caller when a quorum of
//! the copies has confirmed. A replica that does not
//! confirm is dropped from the in-sync set when the rest already make a
//! quorum. That replica copies the primary's live documents when it can
//! answer again, and the primary then publishes it back into the in-sync
//! set. [`fanout_search`] asks every
//! primary and merges hits. If a primary does not answer, a GET or a search
//! of that shard reads a replica copy. Single-node mode (`has_cluster`
//! false) returns before any of this.
//!
//! Replica bytes live in a hidden per-shard index, not in the user index, so
//! a primary search does not count them twice. The owner's handler does not
//! route a `replica` apply again.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use xerj_cluster::{ApplyEffect, ClusterCommand, ShardOp, ShardResult};

use crate::es_compat::{
    aggregate_by_query_results, apply_get_doc_source_filter, apply_local_update,
    pipeline_rejected_body, search_shard_local, unrunnable_pipeline_reason, EsSearchBody,
    EsSearchJson, EsSearchQueryParams, EsUpdateBody, GetDocParams, UpdateDocParams,
};
use crate::responses::{EsDeleteDocResponse, EsDocResponse, EsGetResponse};
use crate::state::AppState;
use crate::ApiError;

/// Per-shard `from + size` cap when a search is merged across owners.
const MAX_FANOUT_WINDOW: usize = 10_000;

/// How long a forward waits for the owner to apply the index create.
const OWNER_READY_WAIT: Duration = Duration::from_secs(2);

pub(crate) enum Dispatch {
    /// This node holds the shard (or the index has no plan). The caller
    /// uses its existing local path.
    Local,
    /// The response is complete: forwarded, or refused.
    Done(Response),
}

pub(crate) fn shard_not_ready_body(index: &str) -> String {
    json!({
        "error": {
            "type": "shard_not_ready",
            "reason": format!("index [{index}] is not on this shard yet")
        },
        "status": 503
    })
    .to_string()
}

pub(crate) fn shard_not_ready_response(index: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": {
                "type": "shard_not_ready",
                "reason": format!("index [{index}] is not on this shard yet")
            },
            "status": 503
        })),
    )
        .into_response()
}

fn http_result(result: ShardResult) -> Response {
    let status = StatusCode::from_u16(result.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let value: Value = serde_json::from_str(&result.body).unwrap_or(Value::Null);
    (status, Json(value)).into_response()
}

fn is_shard_not_ready(result: &ShardResult) -> bool {
    result.status == 503 && result.body.contains("shard_not_ready")
}

fn engine_error_response(err: xerj_engine::EngineError) -> Response {
    ApiError::new(xerj_common::XerjError::from(err)).into_response()
}

async fn forward_retrying(state: &AppState, owner: &str, op: ShardOp) -> Response {
    let deadline = Instant::now() + OWNER_READY_WAIT;
    loop {
        match state.engine.forward_shard(owner, op.clone()).await {
            Ok(result) if is_shard_not_ready(&result) && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok(result) => return http_result(result),
            Err(err) => return engine_error_response(err),
        }
    }
}

/// Route a prepared document. `Ok`-shaped [`Dispatch::Local`] means index here.
///
/// `op` is `"index"` or `"create"`. Sequence numbers are the engine's
/// internal (1-based) values.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn dispatch_index(
    state: &AppState,
    index: &str,
    id: &str,
    doc: &Value,
    op: &str,
    if_seq_no: Option<u64>,
    if_primary_term: Option<u64>,
    version: Option<u64>,
    version_type: Option<String>,
) -> Dispatch {
    if !state.engine.has_cluster() {
        return Dispatch::Local;
    }
    if state.engine.get_index(index).is_err() {
        if let Err(err) = state.engine.ensure_replicated_index(index).await {
            return Dispatch::Done(engine_error_response(err));
        }
    }
    let source_json = doc.to_string();
    if state.engine.owns_shard(index, id) {
        let result = local_index(
            state,
            index,
            id,
            &source_json,
            op,
            if_seq_no,
            if_primary_term,
            version,
            version_type.as_deref(),
            false,
            0,
        )
        .await;
        return Dispatch::Done(http_result(result));
    }
    let Some(owner) = state.engine.route_write(index, id) else {
        return Dispatch::Local;
    };
    let shard_op = ShardOp::IndexDoc {
        index: index.to_string(),
        id: id.to_string(),
        source_json,
        op: op.to_string(),
        if_seq_no,
        if_primary_term,
        version,
        version_type,
        replica: false,
        shard: 0,
    };
    Dispatch::Done(forward_retrying(state, &owner, shard_op).await)
}

pub(crate) async fn dispatch_get(
    state: &AppState,
    index: &str,
    id: &str,
    params: &GetDocParams,
) -> Dispatch {
    if !state.engine.has_cluster() {
        return Dispatch::Local;
    }
    let Some((shard, owner)) = state.engine.primary_of(index, id) else {
        return Dispatch::Local;
    };
    if owner == state.engine.routing_node_id() {
        return Dispatch::Local;
    }
    let op = ShardOp::GetDoc {
        index: index.to_string(),
        id: id.to_string(),
        source: params.source.clone(),
        source_includes: params.source_includes.clone(),
        source_excludes: params.source_excludes.clone(),
        replica: false,
        shard: 0,
    };
    if let Some(response) = forward_primary(state, &owner, op).await {
        return Dispatch::Done(response);
    }
    for replica in state.engine.replica_nodes(index, shard) {
        let op = ShardOp::GetDoc {
            index: index.to_string(),
            id: id.to_string(),
            source: params.source.clone(),
            source_includes: params.source_includes.clone(),
            source_excludes: params.source_excludes.clone(),
            replica: true,
            shard,
        };
        match state.engine.forward_shard(&replica, op).await {
            Ok(result) if !is_shard_not_ready(&result) => {
                return Dispatch::Done(http_result(result));
            }
            _ => continue,
        }
    }
    Dispatch::Done(shard_not_ready_response(index))
}

/// `Some` when the primary answered (including a 404). `None` when the
/// primary could not be reached, so the caller may read a replica.
async fn forward_primary(state: &AppState, owner: &str, op: ShardOp) -> Option<Response> {
    let deadline = Instant::now() + OWNER_READY_WAIT;
    loop {
        match state.engine.forward_shard(owner, op.clone()).await {
            Ok(result) if is_shard_not_ready(&result) && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok(result) => return Some(http_result(result)),
            Err(_) => return None,
        }
    }
}

pub(crate) async fn dispatch_delete(
    state: &AppState,
    index: &str,
    id: &str,
    if_seq_no: Option<u64>,
    if_primary_term: Option<u64>,
) -> Dispatch {
    if !state.engine.has_cluster() {
        return Dispatch::Local;
    }
    if state.engine.owns_shard(index, id) {
        let result = local_delete(state, index, id, if_seq_no, if_primary_term, false, 0).await;
        return Dispatch::Done(http_result(result));
    }
    let Some(owner) = state.engine.route_write(index, id) else {
        return Dispatch::Local;
    };
    let op = ShardOp::DeleteDoc {
        index: index.to_string(),
        id: id.to_string(),
        if_seq_no,
        if_primary_term,
        replica: false,
        shard: 0,
    };
    Dispatch::Done(forward_retrying(state, &owner, op).await)
}

/// Route one `_update` to the shard primary. The primary applies the partial
/// document or the script and copies the stored source onto the in-sync replicas.
///
/// [`Dispatch::Local`] is only an index with no shard plan. Single-node mode
/// does not call this.
pub(crate) async fn dispatch_update(
    state: &AppState,
    index: &str,
    id: &str,
    body: &EsUpdateBody,
    params: &UpdateDocParams,
) -> Dispatch {
    if !state.engine.has_cluster() {
        return Dispatch::Local;
    }
    let concrete = match ensure_concrete_index(state, index).await {
        Ok(name) => name,
        Err(err) => return Dispatch::Done(ensure_fail_response(index, err)),
    };
    let body_json = match serde_json::to_string(body) {
        Ok(json) => json,
        Err(err) => {
            return Dispatch::Done(
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": {
                            "type": "parsing_exception",
                            "reason": format!("update body could not be forwarded: {err}")
                        },
                        "status": 400
                    })),
                )
                    .into_response(),
            );
        }
    };
    let params_json = serde_json::to_string(params).unwrap_or_else(|_| "{}".to_string());
    if state.engine.owns_shard(&concrete, id) {
        let result = local_update(state, index, id, &body_json, &params_json).await;
        return Dispatch::Done(http_result(result));
    }
    let Some(owner) = state.engine.route_write(&concrete, id) else {
        return Dispatch::Local;
    };
    let op = ShardOp::UpdateDoc {
        index: index.to_string(),
        id: id.to_string(),
        body_json,
        params_json,
    };
    Dispatch::Done(forward_retrying(state, &owner, op).await)
}

fn cluster_search_unsupported(feature: &str) -> Response {
    let reason = format!(
        "cluster search does not merge {feature} across shards yet; the request was not executed"
    );
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "error": {
                "type": "illegal_argument_exception",
                "reason": reason
            },
            "status": 400
        })),
    )
        .into_response()
}

/// `Some` when this search was answered by fan-out (including a refusal).
/// `None` when the caller should run the ordinary local search.
pub(crate) async fn fanout_search(
    state: &AppState,
    index: &str,
    params: &EsSearchQueryParams,
    principal: &crate::auth::Principal,
    body: &EsSearchJson,
) -> Option<Response> {
    if !state.engine.has_cluster() {
        return None;
    }
    if index.contains(',') || index.contains('*') || index == "_all" {
        return None;
    }
    let targets = state.engine.shard_search_targets(index);
    let me = state.engine.routing_node_id();
    if targets.is_empty() || (targets.len() == 1 && targets[0] == me) {
        return None;
    }
    let parsed = body.0.clone().unwrap_or_default();
    if parsed.aggs.is_some() || parsed.aggregations.is_some() {
        return Some(cluster_search_unsupported("aggregations"));
    }
    if parsed.suggest.is_some() {
        return Some(cluster_search_unsupported("suggest"));
    }
    if parsed.collapse.is_some() {
        return Some(cluster_search_unsupported("collapse"));
    }
    if parsed.rescore.is_some() {
        return Some(cluster_search_unsupported("rescore"));
    }
    if parsed.profile {
        return Some(cluster_search_unsupported("profile"));
    }
    if parsed.search_after.is_some() {
        return Some(cluster_search_unsupported("search_after"));
    }
    if params.scroll.is_some() {
        return Some(cluster_search_unsupported("scroll"));
    }

    let from = parsed.from;
    let size = parsed.size;
    let window = from.saturating_add(size);
    if window > MAX_FANOUT_WINDOW {
        return Some(cluster_search_unsupported("a from+size window above 10000"));
    }
    let mut shard_body = parsed;
    shard_body.from = 0;
    shard_body.size = window;
    let mut shard_params = params.clone();
    shard_params.from = Some("0".to_string());
    shard_params.size = Some(window.to_string());

    let started = Instant::now();
    let mut parts: Vec<Value> = Vec::with_capacity(targets.len());
    for target in &targets {
        let (status, value) = if target == &me {
            let response = Box::pin(search_shard_local(
                state.clone(),
                index.to_string(),
                shard_params.clone(),
                principal.clone(),
                shard_body.clone(),
            ))
            .await;
            match response_value(response).await {
                Ok(pair) => pair,
                Err(err) => {
                    return Some(shard_failure(target, &err));
                }
            }
        } else {
            let op = match (
                serde_json::to_string(&shard_body),
                serde_json::to_string(&shard_params),
            ) {
                (Ok(body_json), Ok(params_json)) => ShardOp::Search {
                    index: index.to_string(),
                    body_json,
                    params_json,
                },
                (Err(err), _) | (_, Err(err)) => {
                    return Some(shard_failure(target, &err.to_string()));
                }
            };
            match state.engine.forward_shard(target, op).await {
                Ok(result) => {
                    let value = serde_json::from_str(&result.body).unwrap_or(Value::Null);
                    (result.status, value)
                }
                Err(err) => match replica_search_for_primary(
                    state,
                    index,
                    target,
                    &shard_body,
                    &shard_params,
                    principal,
                )
                .await
                {
                    Some(value) => (200, value),
                    None => return Some(engine_error_response(err)),
                },
            }
        };
        if !(200..300).contains(&status) {
            return Some(
                (
                    StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                    Json(value),
                )
                    .into_response(),
            );
        }
        parts.push(value);
    }

    let took_ms = started.elapsed().as_millis() as u64;
    let as_int = matches!(
        params.rest_total_hits_as_int.as_deref(),
        Some("true") | Some("1")
    );
    let merged = merge_hits(&parts, from, size, as_int, took_ms);
    Some(Json(merged).into_response())
}

fn shard_failure(target: &str, err: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": {
                "type": "shard_not_ready",
                "reason": format!("search on shard owner [{target}] failed: {err}")
            },
            "status": 503
        })),
    )
        .into_response()
}

async fn response_value(response: Response) -> Result<(u16, Value), String> {
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .map_err(|err| err.to_string())?;
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Ok((status, value))
}

fn merge_hits(
    parts: &[Value],
    from: usize,
    size: usize,
    total_as_int: bool,
    took_ms: u64,
) -> Value {
    let mut hits: Vec<Value> = Vec::new();
    let mut total: u64 = 0;
    let mut relation = "eq";
    let mut max_score: Option<f64> = None;
    let mut timed_out = false;
    for part in parts {
        if part.get("timed_out").and_then(Value::as_bool) == Some(true) {
            timed_out = true;
        }
        match part.pointer("/hits/total") {
            Some(Value::Number(n)) => total = total.saturating_add(n.as_u64().unwrap_or(0)),
            Some(Value::Object(_)) => {
                total = total.saturating_add(
                    part.pointer("/hits/total/value")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
                if part.pointer("/hits/total/relation").and_then(Value::as_str) == Some("gte") {
                    relation = "gte";
                }
            }
            _ => {}
        }
        if let Some(score) = part.pointer("/hits/max_score").and_then(Value::as_f64) {
            max_score = Some(max_score.map(|current| current.max(score)).unwrap_or(score));
        }
        if let Some(arr) = part.pointer("/hits/hits").and_then(Value::as_array) {
            hits.extend(arr.iter().cloned());
        }
    }
    hits.sort_by(|left, right| {
        let left_score = left.get("_score").and_then(Value::as_f64).unwrap_or(0.0);
        let right_score = right.get("_score").and_then(Value::as_f64).unwrap_or(0.0);
        right_score
            .partial_cmp(&left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                left.get("_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .cmp(right.get("_id").and_then(Value::as_str).unwrap_or(""))
            })
    });
    if size == 0 {
        hits.clear();
    } else {
        hits = hits.into_iter().skip(from).take(size).collect();
    }
    let total_value = if total_as_int {
        json!(total)
    } else {
        json!({ "value": total, "relation": relation })
    };
    json!({
        "took": took_ms,
        "timed_out": timed_out,
        "_shards": {
            "total": parts.len(),
            "successful": parts.len(),
            "skipped": 0,
            "failed": 0
        },
        "hits": {
            "total": total_value,
            "max_score": max_score,
            "hits": hits
        }
    })
}

/// Handler installed on the cluster handle. Runs the operation locally.
pub struct ApiShardHandler {
    state: AppState,
}

impl ApiShardHandler {
    pub fn new(state: AppState) -> Self {
        ApiShardHandler { state }
    }
}

#[async_trait::async_trait]
impl xerj_cluster::ShardHandler for ApiShardHandler {
    async fn handle(&self, op: ShardOp) -> ShardResult {
        match op {
            ShardOp::IndexDoc {
                index,
                id,
                source_json,
                op,
                if_seq_no,
                if_primary_term,
                version,
                version_type,
                replica,
                shard,
            } => {
                local_index(
                    &self.state,
                    &index,
                    &id,
                    &source_json,
                    &op,
                    if_seq_no,
                    if_primary_term,
                    version,
                    version_type.as_deref(),
                    replica,
                    shard,
                )
                .await
            }
            ShardOp::GetDoc {
                index,
                id,
                source,
                source_includes,
                source_excludes,
                replica,
                shard,
            } => {
                local_get(
                    &self.state,
                    &index,
                    &id,
                    source,
                    source_includes,
                    source_excludes,
                    replica,
                    shard,
                )
                .await
            }
            ShardOp::DeleteDoc {
                index,
                id,
                if_seq_no,
                if_primary_term,
                replica,
                shard,
            } => {
                local_delete(
                    &self.state,
                    &index,
                    &id,
                    if_seq_no,
                    if_primary_term,
                    replica,
                    shard,
                )
                .await
            }
            ShardOp::UpdateDoc {
                index,
                id,
                body_json,
                params_json,
            } => {
                let concrete = concrete_index_name(&self.state.engine, &index);
                if self.state.engine.get_index(&concrete).is_err() {
                    return ShardResult::http(503, shard_not_ready_body(&index));
                }
                if !self.state.engine.owns_shard(&concrete, &id) {
                    if let Some(owner) = self.state.engine.route_write(&concrete, &id) {
                        return not_this_primary(&index, &owner);
                    }
                }
                local_update(&self.state, &index, &id, &body_json, &params_json).await
            }
            ShardOp::Bulk {
                body,
                default_index,
                require_alias,
                default_pipeline,
                default_source_req_json,
            } => {
                handle_bulk(
                    &self.state,
                    &body,
                    default_index.as_deref(),
                    require_alias,
                    default_pipeline,
                    default_source_req_json,
                )
                .await
            }
            ShardOp::UpdateByQuery {
                index,
                query_json,
                script_source,
                script_params_json,
                pipeline,
                max_docs,
                scroll_size,
                check_only,
            } => {
                handle_update_by_query(
                    &self.state,
                    &index,
                    &query_json,
                    script_source,
                    script_params_json,
                    pipeline,
                    max_docs,
                    scroll_size,
                    check_only,
                )
                .await
            }
            ShardOp::DeleteByQuery {
                index,
                query_json,
                max_docs,
                scroll_size,
                check_only,
            } => {
                handle_delete_by_query(
                    &self.state,
                    &index,
                    &query_json,
                    max_docs,
                    scroll_size,
                    check_only,
                )
                .await
            }
            ShardOp::Search {
                index,
                body_json,
                params_json,
            } => local_search(&self.state, &index, &body_json, &params_json).await,
            ShardOp::BackfillReplica {
                index,
                shard,
                replica_id,
            } => backfill_replica(&self.state, &index, shard, &replica_id).await,
            ShardOp::RetainReplicaDocs { index, shard, ids } => {
                retain_replica_docs(&self.state, &index, shard, &ids).await
            }
        }
    }
}

async fn response_to_result(response: Response) -> ShardResult {
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap_or_default();
    let body = String::from_utf8(bytes.to_vec()).unwrap_or_else(|_| "{}".to_string());
    ShardResult::http(status, body)
}

#[allow(clippy::too_many_arguments)]
async fn local_index(
    state: &AppState,
    index: &str,
    id: &str,
    source_json: &str,
    op: &str,
    if_seq_no: Option<u64>,
    if_primary_term: Option<u64>,
    version: Option<u64>,
    version_type: Option<&str>,
    replica: bool,
    shard: u32,
) -> ShardResult {
    let store = match doc_store(index, replica, shard) {
        Ok(store) => store,
        Err(result) => return result,
    };
    if open_store(state, &store).await.is_err() {
        return ShardResult::http(503, shard_not_ready_body(index));
    }
    let doc: Value = match serde_json::from_str(source_json) {
        Ok(doc) => doc,
        Err(err) => {
            return ShardResult::http(
                400,
                json!({
                    "error": {
                        "type": "document_parsing_exception",
                        "reason": format!("forwarded document is not JSON: {err}")
                    },
                    "status": 400
                })
                .to_string(),
            );
        }
    };
    let idx = match open_store(state, &store).await {
        Ok(idx) => idx,
        Err(_) => return ShardResult::http(503, shard_not_ready_body(index)),
    };
    let result = if op == "create" {
        idx.create_document(id.to_string(), doc).await
    } else if let (Some(version), Some(version_type)) = (version, version_type) {
        if version_type == "external" || version_type == "external_gte" {
            idx.index_document_external(Some(id.to_string()), doc, version, version_type)
                .await
        } else {
            idx.index_document_with_version(Some(id.to_string()), doc, if_seq_no, if_primary_term)
                .await
        }
    } else {
        idx.index_document_with_version(Some(id.to_string()), doc, if_seq_no, if_primary_term)
            .await
    };
    match result {
        Ok(resp) => {
            state.metrics.record_doc_indexed(index);
            let wire_seq = resp.seq_no.saturating_sub(1);
            let (body, status) = if resp.result == "updated" {
                (
                    EsDocResponse::updated(index, &resp.id, resp.version, wire_seq),
                    200,
                )
            } else {
                (
                    EsDocResponse::created(index, &resp.id, resp.version, wire_seq),
                    201,
                )
            };
            let result = ShardResult::http(
                status,
                serde_json::to_string(&body).unwrap_or_else(|_| "{}".into()),
            );
            if replica {
                return result;
            }
            if let Err(nack) = replicate_write(
                state,
                index,
                id,
                ShardOp::IndexDoc {
                    index: index.to_string(),
                    id: id.to_string(),
                    source_json: source_json.to_string(),
                    op: op.to_string(),
                    if_seq_no,
                    if_primary_term,
                    version,
                    version_type: version_type.map(str::to_string),
                    replica: false,
                    shard: 0,
                },
            )
            .await
            {
                return nack;
            }
            result
        }
        Err(err) => response_to_result(engine_error_response(err)).await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn local_get(
    state: &AppState,
    index: &str,
    id: &str,
    source: Option<String>,
    source_includes: Option<String>,
    source_excludes: Option<String>,
    replica: bool,
    shard: u32,
) -> ShardResult {
    if !replica && state.engine.closed_indices.contains_key(index) {
        return response_to_result(crate::es_compat::closed_index_error(index)).await;
    }
    let store = match doc_store(index, replica, shard) {
        Ok(store) => store,
        Err(result) => return result,
    };
    let idx = match open_store(state, &store).await {
        Ok(idx) => idx,
        Err(_) => return ShardResult::http(503, shard_not_ready_body(index)),
    };
    match idx.get_document(id).await {
        Ok(Some(mut doc)) => {
            xerj_query::executor::strip_internal_passage_metadata(&mut doc);
            let params = GetDocParams {
                source,
                source_includes,
                source_excludes,
            };
            let filtered = apply_get_doc_source_filter(doc, &params);
            let version = idx.lookup_version(id).unwrap_or(1);
            let seq_no = idx.lookup_seq_no(id).unwrap_or(0);
            let resp = EsGetResponse::found(index, id, version, seq_no, filtered);
            ShardResult::http(
                200,
                serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into()),
            )
        }
        Ok(None) => {
            let resp = EsGetResponse::not_found(index, id);
            ShardResult::http(
                404,
                serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into()),
            )
        }
        Err(err) => response_to_result(engine_error_response(err)).await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn local_delete(
    state: &AppState,
    index: &str,
    id: &str,
    if_seq_no: Option<u64>,
    if_primary_term: Option<u64>,
    replica: bool,
    shard: u32,
) -> ShardResult {
    let store = match doc_store(index, replica, shard) {
        Ok(store) => store,
        Err(result) => return result,
    };
    let idx = match open_store(state, &store).await {
        Ok(idx) => idx,
        Err(_) => return ShardResult::http(503, shard_not_ready_body(index)),
    };
    match idx
        .delete_document_versioned(id, if_seq_no, if_primary_term)
        .await
    {
        Ok(outcome) if outcome.found => {
            let resp = EsDeleteDocResponse::deleted(
                index,
                id,
                outcome.version,
                outcome.seq_no.saturating_sub(1),
            );
            finish_delete(state, index, id, replica, 200, &resp).await
        }
        Ok(outcome) => {
            let resp = EsDeleteDocResponse::not_found(
                index,
                id,
                outcome.version,
                outcome.seq_no.saturating_sub(1),
            );
            finish_delete(state, index, id, replica, 404, &resp).await
        }
        Err(err) => response_to_result(engine_error_response(err)).await,
    }
}

async fn finish_delete(
    state: &AppState,
    index: &str,
    id: &str,
    replica: bool,
    status: u16,
    resp: &EsDeleteDocResponse,
) -> ShardResult {
    if !replica {
        if let Err(nack) = replicate_write(
            state,
            index,
            id,
            ShardOp::DeleteDoc {
                index: index.to_string(),
                id: id.to_string(),
                if_seq_no: None,
                if_primary_term: None,
                replica: false,
                shard: 0,
            },
        )
        .await
        {
            return nack;
        }
    }
    ShardResult::http(
        status,
        serde_json::to_string(resp).unwrap_or_else(|_| "{}".into()),
    )
}

/// Replica stores are cluster-internal. A caller's index ACL must not hide
/// them. A failover search of a replica that lives on this node runs on the
/// caller's task, and that task's visibility guard would otherwise 404 the
/// copy. The in-process shard thread has no request guard (it stands in for
/// a TCP handler); this still covers the local path.
struct OpenReplicaStore(String);

impl xerj_engine::index_guard::IndexVisibility for OpenReplicaStore {
    fn visible(&self, index: &str) -> bool {
        index == self.0
    }
}

async fn open_store(
    state: &AppState,
    name: &str,
) -> Result<Arc<xerj_engine::Index>, xerj_engine::EngineError> {
    if name.starts_with(".xerj_replica_") {
        let engine = Arc::clone(&state.engine);
        let owned = name.to_string();
        let guard = Arc::new(OpenReplicaStore(owned.clone()));
        return xerj_engine::index_guard::scoped(guard, async move { engine.get_index(&owned) })
            .await;
    }
    state.engine.get_index(name)
}

fn doc_store(index: &str, replica: bool, shard: u32) -> Result<String, ShardResult> {
    if !replica {
        return Ok(index.to_string());
    }
    xerj_cluster::replica_index_name(index, shard).ok_or_else(|| {
        ShardResult::http(
            400,
            json!({
                "error": {
                    "type": "invalid_index_name_exception",
                    "reason": format!("replica store name for [{index}] shard {shard} is not valid")
                },
                "status": 400
            })
            .to_string(),
        )
    })
}

fn quorum_nack(index: &str, needed: usize, got: usize, detail: &str) -> ShardResult {
    ShardResult::http(
        503,
        json!({
            "error": {
                "type": "replica_not_acknowledged",
                "reason": format!(
                    "primary applied the write on [{index}] but only {got} of {needed} replica acknowledgements are in sync: {detail}. The primary has the write; retry the index so an in-sync replica can take the copy"
                )
            },
            "status": 503
        })
        .to_string(),
    )
}

fn replica_nack(index: &str, replica: &str, detail: &str) -> ShardResult {
    ShardResult::http(
        503,
        json!({
            "error": {
                "type": "replica_not_acknowledged",
                "reason": format!(
                    "primary applied the write on [{index}] but replica [{replica}] did not acknowledge it: {detail}. The primary has the write; retry the index so the copy can catch up"
                )
            },
            "status": 503
        })
        .to_string(),
    )
}

/// Copy `op` onto the in-sync replicas and wait for a quorum.
///
/// The quorum is a majority of the copies placed at create (the primary,
/// which already has the write, plus [`xerj_cluster::replica_quorum_acks`]
/// replica confirmations). An empty placement returns immediately. The copy
/// is an upsert or a delete without the caller's sequence check: the
/// replica's own sequence numbers are not the primary's.
///
/// Every in-sync replica is asked. When enough confirm and some do not, the
/// ones that confirmed are published through the Raft log before the caller
/// is acknowledged, and later reads skip the rest until
/// [`backfill_replica`] copies the primary's live documents and publishes
/// the replica again. Two writes of the same shard do not publish different
/// sets; the primary holds `replica_sync_lock` across the copy and the
/// publish. Backfill takes that same lock.
async fn replicate_write(
    state: &AppState,
    index: &str,
    id: &str,
    op: ShardOp,
) -> Result<(), ShardResult> {
    let Some((shard, _)) = state.engine.primary_of(index, id) else {
        return Ok(());
    };
    let sync = state.engine.replica_sync_lock(index, shard);
    let _sync = sync.lock().await;
    let placed = state.engine.placed_replica_nodes(index, shard);
    if placed.is_empty() {
        return Ok(());
    }
    let targets = state.engine.replica_nodes(index, shard);
    let needed = xerj_cluster::replica_quorum_acks(placed.len());
    if targets.len() < needed {
        return Err(quorum_nack(
            index,
            needed,
            targets.len(),
            "fewer replicas are in sync than the quorum requires",
        ));
    }
    let op = replica_copy(op, shard);
    let mut acked: Vec<String> = Vec::new();
    let mut failed: Option<(String, String)> = None;
    for replica in &targets {
        match confirm_replica(state, replica, &op).await {
            Ok(()) => acked.push(replica.clone()),
            Err(detail) => {
                if failed.is_none() {
                    failed = Some((replica.clone(), detail));
                }
            }
        }
    }
    if acked.len() < needed {
        let (replica, detail) = failed.unwrap_or_else(|| {
            (
                "none".to_string(),
                "no in-sync replica acknowledged the write".to_string(),
            )
        });
        return Err(replica_nack(index, &replica, &detail));
    }
    if acked.len() == targets.len() {
        return Ok(());
    }
    acked.sort();
    acked.dedup();
    let Some(handle) = state.engine.cluster_handle() else {
        return Err(quorum_nack(
            index,
            needed,
            acked.len(),
            "no cluster handle to publish the in-sync set",
        ));
    };
    match handle
        .propose(ClusterCommand::SetShardInSync {
            index: index.to_string(),
            shard,
            nodes: acked.clone(),
        })
        .await
    {
        Ok(ApplyEffect::Applied) => {
            let published = state.engine.replica_nodes(index, shard);
            if acked.iter().all(|id| published.iter().any(|got| got == id)) {
                Ok(())
            } else {
                Err(quorum_nack(
                    index,
                    needed,
                    acked.len(),
                    "the in-sync set committed without the replicas that confirmed this write",
                ))
            }
        }
        Ok(other) => Err(quorum_nack(
            index,
            needed,
            acked.len(),
            &format!("in-sync update applied as {other:?}"),
        )),
        Err(err) => Err(quorum_nack(index, needed, acked.len(), &err.to_string())),
    }
}

async fn confirm_replica(state: &AppState, replica: &str, op: &ShardOp) -> Result<(), String> {
    let deadline = Instant::now() + OWNER_READY_WAIT;
    loop {
        match state.engine.forward_shard(replica, op.clone()).await {
            Ok(result) if is_shard_not_ready(&result) && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok(result) if replica_confirmed(op, &result) => return Ok(()),
            Ok(result) => return Err(result.body),
            Err(err) => return Err(err.to_string()),
        }
    }
}

fn replica_copy(op: ShardOp, shard: u32) -> ShardOp {
    match op {
        ShardOp::IndexDoc {
            index,
            id,
            source_json,
            ..
        } => ShardOp::IndexDoc {
            index,
            id,
            source_json,
            op: "index".to_string(),
            if_seq_no: None,
            if_primary_term: None,
            version: None,
            version_type: None,
            replica: true,
            shard,
        },
        ShardOp::DeleteDoc { index, id, .. } => ShardOp::DeleteDoc {
            index,
            id,
            if_seq_no: None,
            if_primary_term: None,
            replica: true,
            shard,
        },
        other => other,
    }
}

fn replica_confirmed(op: &ShardOp, result: &ShardResult) -> bool {
    match op {
        // A delete of a copy that is already gone is still in sync.
        ShardOp::DeleteDoc { .. } => result.status == 200 || result.status == 404,
        _ => (200..300).contains(&result.status),
    }
}

fn backfill_body(status: u16, type_name: &str, reason: impl Into<String>) -> ShardResult {
    let reason = reason.into();
    ShardResult::http(
        status,
        json!({
            "error": { "type": type_name, "reason": reason },
            "status": status
        })
        .to_string(),
    )
}

fn already_in_sync() -> ShardResult {
    ShardResult::http(
        200,
        json!({ "already_in_sync": true, "copied": 0 }).to_string(),
    )
}

/// Live documents on this primary for `shard`.
///
/// The user index on the primary holds the documents it owns. Ids that
/// route elsewhere are skipped. A tombstone is not in the list. `Err` is
/// the reason the copy must not publish an in-sync set.
async fn primary_live_docs(
    state: &AppState,
    index: &str,
    shard: u32,
) -> Result<Vec<(String, Value)>, String> {
    let idx = state
        .engine
        .get_index(index)
        .map_err(|err| format!("primary index [{index}] is not open: {err}"))?;
    let mut docs = Vec::new();
    for id in idx.live_document_ids() {
        let Some((doc_shard, _)) = state.engine.primary_of(index, &id) else {
            continue;
        };
        if doc_shard != shard {
            continue;
        }
        match idx.get_document(&id).await {
            Ok(Some(source)) => docs.push((id, source)),
            Ok(None) => {}
            Err(err) => return Err(format!("reading [{index}] document [{id}]: {err}")),
        }
    }
    Ok(docs)
}

/// Copy this primary's live documents onto `replica_id` and publish it.
///
/// The replica lock is held from the snapshot through the in-sync publish.
/// A document write applies on the primary first and then waits for this
/// lock before it copies, so a write that missed the snapshot is replicated
/// to the set that includes `replica_id`. A failed copy leaves the in-sync
/// set unchanged. The replica retries.
async fn backfill_replica(
    state: &AppState,
    index: &str,
    shard: u32,
    replica_id: &str,
) -> ShardResult {
    if replica_id.is_empty() {
        return backfill_body(400, "illegal_argument_exception", "replica id is empty");
    }
    let me = state.engine.routing_node_id();
    if replica_id == me {
        return backfill_body(
            400,
            "illegal_argument_exception",
            "a primary does not backfill itself",
        );
    }
    let Some(owner) = state.engine.shard_primary(index, shard) else {
        return backfill_body(
            409,
            "shard_not_ready",
            format!("[{index}] shard {shard} has no primary on this node"),
        );
    };
    if owner != me {
        return backfill_body(
            409,
            "shard_not_primary",
            format!("[{index}] shard {shard} is owned by [{owner}], not [{me}]"),
        );
    }
    let placed = state.engine.placed_replica_nodes(index, shard);
    if !placed.iter().any(|id| id == replica_id) {
        return backfill_body(
            400,
            "illegal_argument_exception",
            format!("[{replica_id}] is not a replica of [{index}] shard {shard}"),
        );
    }
    if state
        .engine
        .replica_nodes(index, shard)
        .iter()
        .any(|id| id == replica_id)
    {
        return already_in_sync();
    }

    let sync = state.engine.replica_sync_lock(index, shard);
    let _guard = sync.lock().await;
    if state
        .engine
        .replica_nodes(index, shard)
        .iter()
        .any(|id| id == replica_id)
    {
        return already_in_sync();
    }

    let docs = match primary_live_docs(state, index, shard).await {
        Ok(docs) => docs,
        Err(reason) => return backfill_body(503, "replica_backfill_failed", reason),
    };
    for (id, source) in &docs {
        let source_json = match serde_json::to_string(source) {
            Ok(json) => json,
            Err(err) => {
                return backfill_body(
                    503,
                    "replica_backfill_failed",
                    format!("document [{id}] is not JSON: {err}"),
                );
            }
        };
        let op = ShardOp::IndexDoc {
            index: index.to_string(),
            id: id.clone(),
            source_json,
            op: "index".to_string(),
            if_seq_no: None,
            if_primary_term: None,
            version: None,
            version_type: None,
            replica: true,
            shard,
        };
        if let Err(detail) = confirm_replica(state, replica_id, &op).await {
            return backfill_body(
                503,
                "replica_backfill_failed",
                format!("replica [{replica_id}] did not take document [{id}]: {detail}"),
            );
        }
    }
    let ids: Vec<String> = docs.iter().map(|(id, _)| id.clone()).collect();
    let retain = ShardOp::RetainReplicaDocs {
        index: index.to_string(),
        shard,
        ids,
    };
    if let Err(detail) = confirm_replica(state, replica_id, &retain).await {
        return backfill_body(
            503,
            "replica_backfill_failed",
            format!(
                "replica [{replica_id}] did not drop documents the primary no longer has: {detail}"
            ),
        );
    }

    let mut nodes = state.engine.replica_nodes(index, shard);
    if !nodes.iter().any(|id| id == replica_id) {
        nodes.push(replica_id.to_string());
    }
    nodes.sort();
    nodes.dedup();
    let Some(handle) = state.engine.cluster_handle() else {
        return backfill_body(
            503,
            "replica_backfill_failed",
            "no cluster handle to publish the in-sync set",
        );
    };
    let copied = docs.len();
    match handle
        .propose(ClusterCommand::SetShardInSync {
            index: index.to_string(),
            shard,
            nodes,
        })
        .await
    {
        Ok(ApplyEffect::Applied) => {
            let published = state.engine.replica_nodes(index, shard);
            if !published.iter().any(|id| id == replica_id) {
                return backfill_body(
                    503,
                    "replica_backfill_failed",
                    "the in-sync set committed without the replica that was filled",
                );
            }
            tracing::info!(
                index = %index,
                shard,
                replica = %replica_id,
                copied,
                "copied primary documents onto a replica and published the in-sync set"
            );
            ShardResult::http(
                200,
                json!({ "already_in_sync": false, "copied": copied }).to_string(),
            )
        }
        Ok(other) => backfill_body(
            503,
            "replica_backfill_failed",
            format!("in-sync update applied as {other:?}"),
        ),
        Err(err) => backfill_body(503, "replica_backfill_failed", err.to_string()),
    }
}

/// Drop replica-store documents that are not in `ids`.
async fn retain_replica_docs(
    state: &AppState,
    index: &str,
    shard: u32,
    ids: &[String],
) -> ShardResult {
    let store = match doc_store(index, true, shard) {
        Ok(store) => store,
        Err(result) => return result,
    };
    let idx = match open_store(state, &store).await {
        Ok(idx) => idx,
        Err(_) => return ShardResult::http(503, shard_not_ready_body(index)),
    };
    let keep: HashSet<&str> = ids.iter().map(String::as_str).collect();
    for id in idx.live_document_ids() {
        if keep.contains(id.as_str()) {
            continue;
        }
        if let Err(err) = idx.delete_document(&id).await {
            return backfill_body(
                503,
                "replica_backfill_failed",
                format!("dropping [{id}] from [{store}]: {err}"),
            );
        }
    }
    ShardResult::http(200, json!({ "retained": ids.len() }).to_string())
}

/// Search the shards `primary` owns by reading one replica copy of each.
///
/// `None` when this index has no replica for that primary, or none of them
/// answered. The caller then fails the search the way it did before replicas
/// existed.
async fn replica_search_for_primary(
    state: &AppState,
    index: &str,
    primary: &str,
    body: &EsSearchBody,
    params: &EsSearchQueryParams,
    principal: &crate::auth::Principal,
) -> Option<Value> {
    let shards = state.engine.shards_owned_by(index, primary);
    if shards.is_empty() {
        return None;
    }
    let window = body.size;
    let mut parts = Vec::with_capacity(shards.len());
    for shard in shards {
        let store = xerj_cluster::replica_index_name(index, shard)?;
        let mut found = None;
        for replica in state.engine.replica_nodes(index, shard) {
            if let Some(mut value) =
                search_replica(state, &replica, &store, body, params, principal).await
            {
                rewrite_hit_index(&mut value, &store, index);
                found = Some(value);
                break;
            }
        }
        parts.push(found?);
    }
    Some(merge_hits(&parts, 0, window, false, 0))
}

async fn search_replica(
    state: &AppState,
    replica: &str,
    store: &str,
    body: &EsSearchBody,
    params: &EsSearchQueryParams,
    principal: &crate::auth::Principal,
) -> Option<Value> {
    if replica == state.engine.routing_node_id() {
        let engine_store = store.to_string();
        let guard = Arc::new(OpenReplicaStore(engine_store.clone()));
        let response = xerj_engine::index_guard::scoped(guard, async move {
            search_shard_local(
                state.clone(),
                engine_store,
                params.clone(),
                principal.clone(),
                body.clone(),
            )
            .await
        })
        .await;
        let (status, value) = response_value(response).await.ok()?;
        return (200..300).contains(&status).then_some(value);
    }
    let op = match (serde_json::to_string(body), serde_json::to_string(params)) {
        (Ok(body_json), Ok(params_json)) => ShardOp::Search {
            index: store.to_string(),
            body_json,
            params_json,
        },
        _ => return None,
    };
    match state.engine.forward_shard(replica, op).await {
        Ok(result) if (200..300).contains(&result.status) => {
            serde_json::from_str(&result.body).ok()
        }
        _ => None,
    }
}

fn rewrite_hit_index(value: &mut Value, from: &str, to: &str) {
    let Some(hits) = value
        .pointer_mut("/hits/hits")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for hit in hits {
        if hit.get("_index").and_then(Value::as_str) == Some(from) {
            hit["_index"] = json!(to);
        }
    }
}

async fn local_search(
    state: &AppState,
    index: &str,
    body_json: &str,
    params_json: &str,
) -> ShardResult {
    let params: EsSearchQueryParams = serde_json::from_str(params_json).unwrap_or_default();
    let body: EsSearchBody = match serde_json::from_str(body_json) {
        Ok(body) => body,
        Err(err) => {
            return ShardResult::http(
                400,
                json!({
                    "error": {
                        "type": "parsing_exception",
                        "reason": format!("forwarded search body is not JSON: {err}")
                    },
                    "status": 400
                })
                .to_string(),
            );
        }
    };
    // The accepting node already passed its auth middleware. This RPC is the
    // cluster transport, not a second client. The local shard of the
    // coordinator still runs under the caller's principal.
    let response = if index.starts_with(".xerj_replica_") {
        let guard = Arc::new(OpenReplicaStore(index.to_string()));
        xerj_engine::index_guard::scoped(guard, async move {
            search_shard_local(
                state.clone(),
                index.to_string(),
                params,
                crate::auth::Principal::Superuser,
                body,
            )
            .await
        })
        .await
    } else {
        Box::pin(search_shard_local(
            state.clone(),
            index.to_string(),
            params,
            crate::auth::Principal::Superuser,
            body,
        ))
        .await
    };
    response_to_result(response).await
}

// ── `_update` and `_bulk` on the primary ────────────────────────────────────

/// Backing index when `name` is an alias, otherwise `name`.
///
/// `Engine::get_index` writes an alias through its first backing index. Routing
/// has to use that same name, or the shard plan (which lives on the concrete
/// index) is invisible and the write stays on the node that received it.
fn concrete_index_name(engine: &xerj_engine::Engine, name: &str) -> String {
    engine
        .aliases
        .get(name)
        .and_then(|names| names.first().cloned())
        .filter(|backing| !backing.is_empty())
        .unwrap_or_else(|| name.to_string())
}

enum EnsureFail {
    Engine(xerj_engine::EngineError),
    Unavailable(String),
}

fn ensure_fail_response(index: &str, err: EnsureFail) -> Response {
    match err {
        EnsureFail::Engine(err) => engine_error_response(err),
        EnsureFail::Unavailable(reason) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": {
                    "type": "shard_not_ready",
                    "reason": format!("index [{index}] is not on this shard yet: {reason}")
                },
                "status": 503
            })),
        )
            .into_response(),
    }
}

fn ensure_fail_status(err: EnsureFail) -> (u16, String) {
    match err {
        EnsureFail::Engine(err) => {
            let common = xerj_common::XerjError::from(err);
            let status = common.http_status();
            (status, common.to_string())
        }
        EnsureFail::Unavailable(reason) => (503, reason),
    }
}

async fn wait_index_open(state: &AppState, name: &str) -> bool {
    let deadline = Instant::now() + OWNER_READY_WAIT;
    loop {
        if state.engine.get_index(name).is_ok() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Open `name`, creating it through the Raft log when it is not an alias and
/// this node does not have it yet. Does not write a document.
async fn ensure_concrete_index(state: &AppState, name: &str) -> Result<String, EnsureFail> {
    let concrete = concrete_index_name(&state.engine, name);
    if concrete != name {
        if state.engine.get_index(&concrete).is_err() {
            return Err(EnsureFail::Unavailable(format!(
                "alias [{name}] does not point at an open index"
            )));
        }
        return Ok(concrete);
    }
    if state.engine.get_index(&concrete).is_err() {
        state
            .engine
            .ensure_replicated_index(&concrete)
            .await
            .map_err(EnsureFail::Engine)?;
    }
    if !wait_index_open(state, &concrete).await {
        return Err(EnsureFail::Unavailable(format!(
            "index [{concrete}] was not open after the create committed"
        )));
    }
    Ok(concrete)
}

fn not_this_primary(index: &str, owner: &str) -> ShardResult {
    ShardResult::http(
        409,
        json!({
            "error": {
                "type": "shard_not_primary",
                "reason": format!("[{index}] is owned by [{owner}], not this node")
            },
            "status": 409
        })
        .to_string(),
    )
}

async fn local_update(
    state: &AppState,
    index: &str,
    id: &str,
    body_json: &str,
    params_json: &str,
) -> ShardResult {
    let concrete = concrete_index_name(&state.engine, index);
    if state.engine.get_index(&concrete).is_err() {
        return ShardResult::http(503, shard_not_ready_body(index));
    }
    let body: EsUpdateBody = match serde_json::from_str(body_json) {
        Ok(body) => body,
        Err(err) => {
            return ShardResult::http(
                400,
                json!({
                    "error": {
                        "type": "parsing_exception",
                        "reason": format!("forwarded update body is not JSON: {err}")
                    },
                    "status": 400
                })
                .to_string(),
            );
        }
    };
    let params: UpdateDocParams = serde_json::from_str(params_json).unwrap_or_default();
    let applied = apply_local_update(state, index, id, body, &params).await;
    if !applied.replicate {
        return response_to_result(applied.response).await;
    }
    let copy = match state.engine.get_index(&concrete) {
        Ok(idx) => match idx.get_document(id).await {
            Ok(Some(doc)) => match serde_json::to_string(&doc) {
                Ok(source_json) => ShardOp::IndexDoc {
                    index: concrete.clone(),
                    id: id.to_string(),
                    source_json,
                    op: "index".to_string(),
                    if_seq_no: None,
                    if_primary_term: None,
                    version: None,
                    version_type: None,
                    replica: false,
                    shard: 0,
                },
                Err(err) => {
                    return ShardResult::http(
                        503,
                        json!({
                            "error": {
                                "type": "replica_not_acknowledged",
                                "reason": format!(
                                    "primary applied the update on [{index}] but document [{id}] could not be copied: {err}"
                                )
                            },
                            "status": 503
                        })
                        .to_string(),
                    );
                }
            },
            Ok(None) => ShardOp::DeleteDoc {
                index: concrete.clone(),
                id: id.to_string(),
                if_seq_no: None,
                if_primary_term: None,
                replica: false,
                shard: 0,
            },
            Err(err) => {
                return ShardResult::http(
                    503,
                    json!({
                        "error": {
                            "type": "replica_not_acknowledged",
                            "reason": format!(
                                "primary applied the update on [{index}] but document [{id}] could not be read for the replica copy: {err}"
                            )
                        },
                        "status": 503
                    })
                    .to_string(),
                );
            }
        },
        Err(err) => {
            return ShardResult::http(
                503,
                json!({
                    "error": {
                        "type": "replica_not_acknowledged",
                        "reason": format!(
                            "primary applied the update on [{index}] but [{concrete}] is not open for the replica copy: {err}"
                        )
                    },
                    "status": 503
                })
                .to_string(),
            );
        }
    };
    if let Err(nack) = replicate_write(state, &concrete, id, copy).await {
        return nack;
    }
    response_to_result(applied.response).await
}

/// One parsed bulk action, in request order.
struct BulkPiece {
    action: String,
    /// Name the caller used. The bulk pipeline reports this on the item.
    index: String,
    id: String,
    action_line: String,
    source_line: Option<String>,
    /// Set when this action is already an error and must not be written.
    preset: Option<xerj_engine::bulk::BulkItemResult>,
    concrete: String,
}

fn bulk_item_error(
    action: &str,
    index: &str,
    id: &str,
    status: u16,
    error: impl Into<String>,
) -> xerj_engine::bulk::BulkItemResult {
    xerj_engine::bulk::BulkItemResult {
        action: action.to_string(),
        index: index.to_string(),
        id: id.to_string(),
        status,
        result: None,
        error: Some(error.into()),
        get_source: None,
    }
}

/// Same pairing rule as the bulk engine: a delete action has no source line.
/// The two parsers have to agree, or a delete swallows the next action.
fn bulk_action_line_is_delete(line: &str) -> bool {
    let b = line.as_bytes();
    let mut p = 0usize;
    while p < b.len() && (b[p] == b' ' || b[p] == b'\t') {
        p += 1;
    }
    if p >= b.len() || b[p] != b'{' {
        return false;
    }
    p += 1;
    while p < b.len() && (b[p] == b' ' || b[p] == b'\t') {
        p += 1;
    }
    if p >= b.len() || b[p] != b'"' {
        return false;
    }
    p += 1;
    let start = p;
    while p < b.len() && b[p] != b'"' {
        p += 1;
    }
    p < b.len() && &b[start..p] == b"delete"
}

fn json_meta_id(meta: &Value) -> Option<String> {
    match meta.get("_id")? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn split_bulk_pieces(body: &str, default_index: Option<&str>) -> Vec<BulkPiece> {
    let lines: Vec<&str> = body
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let mut pieces = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let raw_action = lines[i];
        let is_delete = bulk_action_line_is_delete(raw_action);
        let source_line = if !is_delete && i + 1 < lines.len() {
            let source = lines[i + 1];
            i += 2;
            Some(source.to_string())
        } else {
            i += 1;
            None
        };
        let action_val: Value = match serde_json::from_str(raw_action) {
            Ok(value) => value,
            Err(err) => {
                pieces.push(BulkPiece {
                    action: "unknown".to_string(),
                    index: default_index.unwrap_or("_unknown").to_string(),
                    id: String::new(),
                    action_line: String::new(),
                    source_line: None,
                    preset: Some(bulk_item_error(
                        "unknown",
                        default_index.unwrap_or("_unknown"),
                        "",
                        400,
                        format!("failed to parse action line: {err}"),
                    )),
                    concrete: String::new(),
                });
                continue;
            }
        };
        let (action, meta) = if let Some(meta) = action_val.get("index") {
            ("index", meta)
        } else if let Some(meta) = action_val.get("create") {
            ("create", meta)
        } else if let Some(meta) = action_val.get("update") {
            ("update", meta)
        } else if let Some(meta) = action_val.get("delete") {
            ("delete", meta)
        } else {
            pieces.push(BulkPiece {
                action: "unknown".to_string(),
                index: default_index.unwrap_or("_unknown").to_string(),
                id: String::new(),
                action_line: String::new(),
                source_line: None,
                preset: Some(bulk_item_error(
                    "unknown",
                    default_index.unwrap_or("_unknown"),
                    "",
                    400,
                    "unknown action type",
                )),
                concrete: String::new(),
            });
            continue;
        };
        let mut meta = if meta.is_object() {
            meta.clone()
        } else {
            json!({})
        };
        let index = meta
            .get("_index")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| default_index.map(str::to_string))
            .unwrap_or_else(|| "_unknown".to_string());
        if meta.get("_index").and_then(Value::as_str).is_none() {
            meta["_index"] = json!(index);
        }
        if let Some(id) = json_meta_id(&meta) {
            if id.is_empty() {
                pieces.push(BulkPiece {
                    action: action.to_string(),
                    index: index.clone(),
                    id: String::new(),
                    action_line: String::new(),
                    source_line: None,
                    preset: Some(bulk_item_error(
                        action,
                        &index,
                        "",
                        400,
                        "if _id is specified it must not be empty",
                    )),
                    concrete: String::new(),
                });
                continue;
            }
        }
        let id = match json_meta_id(&meta) {
            Some(id) => id,
            None if action == "index" || action == "create" => {
                let id = uuid::Uuid::new_v4().to_string();
                meta["_id"] = json!(id);
                id
            }
            None => {
                let reason = if action == "update" {
                    "update action requires _id"
                } else {
                    "delete action requires _id"
                };
                pieces.push(BulkPiece {
                    action: action.to_string(),
                    index: index.clone(),
                    id: String::new(),
                    action_line: String::new(),
                    source_line: None,
                    preset: Some(bulk_item_error(action, &index, "", 400, reason)),
                    concrete: String::new(),
                });
                continue;
            }
        };
        if action != "delete" && source_line.is_none() {
            pieces.push(BulkPiece {
                action: action.to_string(),
                index: index.clone(),
                id: id.clone(),
                action_line: String::new(),
                source_line: None,
                preset: Some(bulk_item_error(
                    action,
                    &index,
                    &id,
                    400,
                    "missing document body",
                )),
                concrete: String::new(),
            });
            continue;
        }
        let mut action_obj = serde_json::Map::new();
        action_obj.insert(action.to_string(), meta);
        let action_line = serde_json::to_string(&Value::Object(action_obj))
            .unwrap_or_else(|_| raw_action.to_string());
        pieces.push(BulkPiece {
            action: action.to_string(),
            index,
            id,
            action_line,
            source_line: if action == "delete" {
                None
            } else {
                source_line
            },
            preset: None,
            concrete: String::new(),
        });
    }
    pieces
}

fn group_ndjson(pieces: &[BulkPiece], which: &[usize]) -> String {
    let mut body = String::new();
    for &at in which {
        body.push_str(&pieces[at].action_line);
        body.push('\n');
        if let Some(source) = &pieces[at].source_line {
            body.push_str(source);
            body.push('\n');
        }
    }
    body
}

#[derive(serde::Serialize, serde::Deserialize)]
struct BulkWire {
    version: u64,
    seq_no: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PrimaryBulkReply {
    took_ms: u64,
    errors: bool,
    items: Vec<xerj_engine::bulk::BulkItemResult>,
    wire: Vec<Option<BulkWire>>,
}

fn nack_reason(result: &ShardResult) -> String {
    serde_json::from_str::<Value>(&result.body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/reason")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| result.body.clone())
}

fn mark_bulk_nack(item: &mut xerj_engine::bulk::BulkItemResult, nack: &ShardResult) {
    item.status = nack.status;
    item.result = None;
    item.get_source = None;
    item.error = Some(nack_reason(nack));
}

fn wire_of(engine: &xerj_engine::Engine, index: &str, id: &str) -> Option<(u64, u64)> {
    let idx = engine.get_index(index).ok()?;
    Some((
        idx.lookup_version(id).unwrap_or(1),
        idx.lookup_seq_no(id).unwrap_or(0),
    ))
}

/// Run the local bulk pipeline, then copy each success onto the in-sync replicas.
///
/// A noop update is not copied: the source did not change. A failed item is
/// not copied. A copy that misses quorum replaces that item's success with
/// the same `replica_not_acknowledged` a single-document write returns. The
/// primary already has the write.
async fn replicate_bulk_result(
    state: &AppState,
    mut result: xerj_engine::bulk::BulkResult,
) -> (xerj_engine::bulk::BulkResult, Vec<Option<(u64, u64)>>) {
    let mut wire = Vec::with_capacity(result.items.len());
    // `item` borrows `result.items`, so the flag is applied after the loop.
    let mut copy_failed = false;
    for item in &mut result.items {
        let failed = item.error.is_some() || !(200..300).contains(&item.status);
        if failed || item.id.is_empty() {
            if !failed && item.id.is_empty() {
                item.status = 503;
                item.result = None;
                item.error = Some(
                    "bulk item was acknowledged without an id, so it was not copied".to_string(),
                );
                copy_failed = true;
            }
            wire.push(None);
            continue;
        }
        let concrete = concrete_index_name(&state.engine, &item.index);
        if item.action == "delete" {
            let index = concrete.clone();
            let id = item.id.clone();
            let op = ShardOp::DeleteDoc {
                index: index.clone(),
                id: id.clone(),
                if_seq_no: None,
                if_primary_term: None,
                replica: false,
                shard: 0,
            };
            if let Err(nack) = replicate_write(state, &index, &id, op).await {
                mark_bulk_nack(item, &nack);
                copy_failed = true;
            }
            wire.push(None);
            continue;
        }
        if item.result.as_deref() == Some("noop") {
            wire.push(wire_of(&state.engine, &concrete, &item.id));
            continue;
        }
        let op = match state.engine.get_index(&concrete) {
            Ok(idx) => match idx.get_document(&item.id).await {
                Ok(Some(doc)) => match serde_json::to_string(&doc) {
                    Ok(source_json) => ShardOp::IndexDoc {
                        index: concrete.clone(),
                        id: item.id.clone(),
                        source_json,
                        op: "index".to_string(),
                        if_seq_no: None,
                        if_primary_term: None,
                        version: None,
                        version_type: None,
                        replica: false,
                        shard: 0,
                    },
                    Err(err) => {
                        mark_bulk_nack(
                            item,
                            &ShardResult::http(
                                503,
                                json!({
                                    "error": {
                                        "type": "replica_not_acknowledged",
                                        "reason": format!(
                                            "primary applied the bulk item on [{concrete}] but document [{}] is not JSON: {err}",
                                            item.id
                                        )
                                    },
                                    "status": 503
                                })
                                .to_string(),
                            ),
                        );
                        copy_failed = true;
                        wire.push(None);
                        continue;
                    }
                },
                Ok(None) => ShardOp::DeleteDoc {
                    index: concrete.clone(),
                    id: item.id.clone(),
                    if_seq_no: None,
                    if_primary_term: None,
                    replica: false,
                    shard: 0,
                },
                Err(err) => {
                    mark_bulk_nack(
                        item,
                        &ShardResult::http(
                            503,
                            json!({
                                "error": {
                                    "type": "replica_not_acknowledged",
                                    "reason": format!(
                                        "primary applied the bulk item on [{concrete}] but document [{}] could not be read: {err}",
                                        item.id
                                    )
                                },
                                "status": 503
                            })
                            .to_string(),
                        ),
                    );
                    copy_failed = true;
                    wire.push(None);
                    continue;
                }
            },
            Err(err) => {
                mark_bulk_nack(
                    item,
                    &ShardResult::http(
                        503,
                        json!({
                            "error": {
                                "type": "replica_not_acknowledged",
                                "reason": format!(
                                    "primary applied the bulk item but [{concrete}] is not open: {err}"
                                )
                            },
                            "status": 503
                        })
                        .to_string(),
                    ),
                );
                copy_failed = true;
                wire.push(None);
                continue;
            }
        };
        let index = op_index(&op).to_string();
        if let Err(nack) = replicate_write(state, &index, &item.id, op).await {
            mark_bulk_nack(item, &nack);
            copy_failed = true;
            wire.push(None);
            continue;
        }
        wire.push(wire_of(&state.engine, &concrete, &item.id));
    }
    if copy_failed {
        result.errors = true;
    }
    (result, wire)
}

fn op_index(op: &ShardOp) -> &str {
    match op {
        ShardOp::IndexDoc { index, .. }
        | ShardOp::DeleteDoc { index, .. }
        | ShardOp::UpdateDoc { index, .. } => index,
        _ => "",
    }
}

fn bulk_opts_from_parts(
    require_alias: Option<bool>,
    default_pipeline: Option<String>,
    default_source_req_json: Option<String>,
) -> xerj_engine::bulk::BulkOpts {
    xerj_engine::bulk::BulkOpts {
        require_alias,
        default_pipeline,
        default_source_req: default_source_req_json
            .and_then(|json| serde_json::from_str(&json).ok()),
    }
}

async fn run_primary_bulk(
    state: &AppState,
    body: &str,
    default_index: Option<&str>,
    opts: xerj_engine::bulk::BulkOpts,
) -> (xerj_engine::bulk::BulkResult, Vec<Option<(u64, u64)>>) {
    let result =
        xerj_engine::bulk::process_bulk_with_opts(&state.engine, default_index, body, opts).await;
    replicate_bulk_result(state, result).await
}

/// Refuse a forwarded bulk that names a shard this node does not own, and
/// wait until every named index is open. Either check happens before any
/// document is written.
fn bulk_refused_here(state: &AppState, pieces: &[BulkPiece]) -> Option<ShardResult> {
    let me = state.engine.routing_node_id();
    for piece in pieces {
        if piece.preset.is_some() {
            continue;
        }
        let concrete = concrete_index_name(&state.engine, &piece.index);
        if state.engine.get_index(&concrete).is_err() {
            return Some(ShardResult::http(503, shard_not_ready_body(&piece.index)));
        }
        if state.engine.owns_shard(&concrete, &piece.id) {
            continue;
        }
        if let Some(owner) = state.engine.route_write(&concrete, &piece.id) {
            if owner != me {
                return Some(not_this_primary(&piece.index, &owner));
            }
        }
    }
    None
}

async fn handle_bulk(
    state: &AppState,
    body: &str,
    default_index: Option<&str>,
    require_alias: Option<bool>,
    default_pipeline: Option<String>,
    default_source_req_json: Option<String>,
) -> ShardResult {
    let pieces = split_bulk_pieces(body, default_index);
    if let Some(refused) = bulk_refused_here(state, &pieces) {
        return refused;
    }
    let opts = bulk_opts_from_parts(require_alias, default_pipeline, default_source_req_json);
    let (result, wire) = run_primary_bulk(state, body, default_index, opts).await;
    let reply = PrimaryBulkReply {
        took_ms: result.took_ms,
        errors: result.errors,
        items: result.items,
        wire: wire
            .into_iter()
            .map(|item| item.map(|(version, seq_no)| BulkWire { version, seq_no }))
            .collect(),
    };
    let body = serde_json::to_string(&reply).unwrap_or_else(|_| {
        json!({
            "took_ms": 0,
            "errors": true,
            "items": [],
            "wire": []
        })
        .to_string()
    });
    ShardResult::http(200, body)
}

struct OwnerGroup {
    remote: bool,
    owner: String,
    pieces: Vec<usize>,
}

fn spread_group_failure(
    pieces: &[BulkPiece],
    which: &[usize],
    status: u16,
    error: String,
) -> (
    Vec<xerj_engine::bulk::BulkItemResult>,
    Vec<Option<(u64, u64)>>,
) {
    let items = which
        .iter()
        .map(|&at| {
            bulk_item_error(
                &pieces[at].action,
                &pieces[at].index,
                &pieces[at].id,
                status,
                error.clone(),
            )
        })
        .collect();
    (items, vec![None; which.len()])
}

async fn execute_owner_group(
    state: &AppState,
    pieces: &[BulkPiece],
    group: &OwnerGroup,
    opts: &xerj_engine::bulk::BulkOpts,
    default_index: Option<&str>,
) -> (
    Vec<xerj_engine::bulk::BulkItemResult>,
    Vec<Option<(u64, u64)>>,
) {
    let body = group_ndjson(pieces, &group.pieces);
    if !group.remote {
        let (result, wire) = run_primary_bulk(state, &body, default_index, opts.clone()).await;
        return align_group(pieces, &group.pieces, result, wire);
    }
    let op = ShardOp::Bulk {
        body,
        default_index: default_index.map(str::to_string),
        require_alias: opts.require_alias,
        default_pipeline: opts.default_pipeline.clone(),
        default_source_req_json: opts
            .default_source_req
            .as_ref()
            .and_then(|value| serde_json::to_string(value).ok()),
    };
    let deadline = Instant::now() + OWNER_READY_WAIT;
    loop {
        match state.engine.forward_shard(&group.owner, op.clone()).await {
            Ok(result) if is_shard_not_ready(&result) && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok(result) if (200..300).contains(&result.status) => {
                match serde_json::from_str::<PrimaryBulkReply>(&result.body) {
                    Ok(reply) => {
                        let wire = reply
                            .wire
                            .into_iter()
                            .map(|item| item.map(|wire| (wire.version, wire.seq_no)))
                            .collect();
                        let result = xerj_engine::bulk::BulkResult {
                            took_ms: reply.took_ms,
                            errors: reply.errors,
                            items: reply.items,
                        };
                        return align_group(pieces, &group.pieces, result, wire);
                    }
                    Err(err) => {
                        return spread_group_failure(
                            pieces,
                            &group.pieces,
                            503,
                            format!("primary bulk response was not usable: {err}"),
                        );
                    }
                }
            }
            Ok(result) => {
                return spread_group_failure(
                    pieces,
                    &group.pieces,
                    result.status,
                    nack_reason(&result),
                );
            }
            Err(err) => {
                return spread_group_failure(pieces, &group.pieces, 503, err.to_string());
            }
        }
    }
}

fn align_group(
    pieces: &[BulkPiece],
    which: &[usize],
    mut result: xerj_engine::bulk::BulkResult,
    mut wire: Vec<Option<(u64, u64)>>,
) -> (
    Vec<xerj_engine::bulk::BulkItemResult>,
    Vec<Option<(u64, u64)>>,
) {
    if result.items.len() == which.len() {
        if wire.len() < which.len() {
            wire.resize(which.len(), None);
        }
        wire.truncate(which.len());
        return (result.items, wire);
    }
    if result.items.len() == 1 && result.errors {
        let err = result.items.pop().expect("length checked");
        let reason = err
            .error
            .unwrap_or_else(|| "bulk request failed".to_string());
        return spread_group_failure(pieces, which, err.status, reason);
    }
    spread_group_failure(
        pieces,
        which,
        500,
        format!(
            "primary returned {} bulk items for {} actions",
            result.items.len(),
            which.len()
        ),
    )
}

/// Route a bulk body to each action's primary and copy the successes.
///
/// [`crate::es_compat`] calls this only when a cluster handle is attached.
/// The returned wire versions are the primary's. The accepting node does not
/// have the document, so a local lookup would report a fresh id.
pub(crate) async fn bulk_on_primaries(
    state: &AppState,
    default_index: Option<&str>,
    body: &str,
    opts: xerj_engine::bulk::BulkOpts,
) -> (xerj_engine::bulk::BulkResult, Vec<Option<(u64, u64)>>) {
    let started = Instant::now();
    let line_count = body.lines().filter(|line| !line.trim().is_empty()).count();
    let max_actions = state.engine.config().limits.max_actions_per_bulk;
    if line_count == 0 || (max_actions > 0 && line_count > max_actions.saturating_mul(2)) {
        let result =
            xerj_engine::bulk::process_bulk_with_opts(&state.engine, default_index, body, opts)
                .await;
        let wire = vec![None; result.items.len()];
        return (result, wire);
    }

    let mut pieces = split_bulk_pieces(body, default_index);
    let mut ensured: HashMap<String, Result<String, (u16, String)>> = HashMap::new();
    for piece in &mut pieces {
        if piece.preset.is_some() {
            continue;
        }
        let outcome = if let Some(cached) = ensured.get(&piece.index) {
            cached.clone()
        } else {
            let outcome = match ensure_concrete_index(state, &piece.index).await {
                Ok(name) => Ok(name),
                Err(err) => Err(ensure_fail_status(err)),
            };
            ensured.insert(piece.index.clone(), outcome.clone());
            outcome
        };
        match outcome {
            Ok(name) => piece.concrete = name,
            Err((status, reason)) => {
                piece.preset = Some(bulk_item_error(
                    &piece.action,
                    &piece.index,
                    &piece.id,
                    status,
                    reason,
                ));
            }
        }
    }

    let mut groups: Vec<OwnerGroup> = Vec::new();
    for (at, piece) in pieces.iter().enumerate() {
        if piece.preset.is_some() {
            continue;
        }
        let remote = if state.engine.owns_shard(&piece.concrete, &piece.id) {
            None
        } else {
            state.engine.route_write(&piece.concrete, &piece.id)
        };
        let owner = remote.clone().unwrap_or_default();
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.remote == remote.is_some() && group.owner == owner)
        {
            group.pieces.push(at);
        } else {
            groups.push(OwnerGroup {
                remote: remote.is_some(),
                owner,
                pieces: vec![at],
            });
        }
    }

    let mut items: Vec<Option<xerj_engine::bulk::BulkItemResult>> =
        pieces.iter().map(|piece| piece.preset.clone()).collect();
    let mut wire = vec![None; pieces.len()];
    for group in &groups {
        let (group_items, group_wire) =
            execute_owner_group(state, &pieces, group, &opts, default_index).await;
        for (slot, (item, item_wire)) in group
            .pieces
            .iter()
            .zip(group_items.into_iter().zip(group_wire))
        {
            items[*slot] = Some(item);
            wire[*slot] = item_wire;
        }
    }
    let items: Vec<_> = items
        .into_iter()
        .enumerate()
        .map(|(at, item)| {
            item.unwrap_or_else(|| {
                bulk_item_error(
                    pieces
                        .get(at)
                        .map(|piece| piece.action.as_str())
                        .unwrap_or("index"),
                    pieces
                        .get(at)
                        .map(|piece| piece.index.as_str())
                        .unwrap_or(""),
                    pieces.get(at).map(|piece| piece.id.as_str()).unwrap_or(""),
                    500,
                    "bulk item was not executed",
                )
            })
        })
        .collect();
    let errors = items
        .iter()
        .any(|item| item.error.is_some() || !(200..300).contains(&item.status));
    (
        xerj_engine::bulk::BulkResult {
            took_ms: started.elapsed().as_millis() as u64,
            errors,
            items,
        },
        wire,
    )
}

// ── `_update_by_query` on every primary ─────────────────────────────────────

/// One `_update_by_query` after the accepting node has validated the query.
///
/// [`update_by_query_on_primaries`] borrows this. `max_docs` is the caller's
/// cap for the whole index; the fan-out spends it across primaries.
#[derive(Clone)]
pub(crate) struct UpdateByQueryParts {
    pub query: Value,
    pub script: Option<(String, Value)>,
    pub pipeline: Option<String>,
    pub max_docs: Option<u64>,
    pub scroll_size: usize,
}

/// Box the per-index fan-out so it is not inlined into `update_by_query`.
///
/// See `begin_cluster_update_by_query` for why the future has to live on
/// the heap: a debug worker stack overflows when the handler and the runner
/// are both built on it.
pub(crate) fn pin_update_by_query_on_primaries(
    state: AppState,
    index: String,
    parts: UpdateByQueryParts,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send>> {
    Box::pin(async move { update_by_query_on_primaries(&state, &index, &parts).await })
}

fn update_by_query_op(
    index: &str,
    parts: &UpdateByQueryParts,
    max_docs: Option<u64>,
    check_only: bool,
) -> ShardOp {
    ShardOp::UpdateByQuery {
        index: index.to_string(),
        query_json: parts.query.to_string(),
        script_source: parts.script.as_ref().map(|(source, _)| source.clone()),
        script_params_json: parts.script.as_ref().map(|(_, params)| params.to_string()),
        pipeline: parts.pipeline.clone(),
        max_docs,
        scroll_size: parts.scroll_size as u64,
        check_only,
    }
}

fn update_by_query_unavailable(index: &str, detail: &str) -> Value {
    json!({
        "error": {
            "type": "shard_not_ready",
            "reason": format!("update_by_query on [{index}] failed: {detail}")
        },
        "status": 503
    })
}

fn empty_update_by_query() -> Value {
    json!({
        "took": 0,
        "timed_out": false,
        "total": 0,
        "updated": 0,
        "deleted": 0,
        "batches": 0,
        "version_conflicts": 0,
        "noops": 0,
        "failures": [],
        "throttled_millis": 0,
        "requests_per_second": -1,
        "throttled_until_millis": 0,
    })
}

fn value_from_shard(result: &ShardResult) -> Value {
    serde_json::from_str(&result.body).unwrap_or_else(|_| {
        json!({
            "error": {
                "type": "shard_not_ready",
                "reason": result.body,
            },
            "status": result.status
        })
    })
}

fn is_not_ready_value(value: &Value) -> bool {
    value.get("status").and_then(Value::as_u64) == Some(503)
        && value.pointer("/error/type").and_then(Value::as_str) == Some("shard_not_ready")
}

fn ensure_error_status(mut value: Value) -> Value {
    if value.get("status").is_some() {
        return value;
    }
    if let Some(obj) = value.as_object_mut() {
        obj.insert("status".to_string(), json!(503));
    }
    value
}

fn has_replica_nack(body: &Value) -> bool {
    body.get("failures")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.pointer("/cause/type").and_then(Value::as_str)
                    == Some("replica_not_acknowledged")
            })
        })
}

fn stamp_replica_nack(mut body: Value, reason: &str) -> Value {
    if !has_replica_nack(&body) {
        return body;
    }
    if let Some(obj) = body.as_object_mut() {
        obj.insert("status".to_string(), json!(503));
        obj.insert(
            "error".to_string(),
            json!({
                "type": "replica_not_acknowledged",
                "reason": reason,
            }),
        );
    }
    body
}

/// Index open, this node is a primary (when a plan exists), pipeline runnable.
///
/// `Err` is a body the HTTP layer can return. Nothing has been written.
fn prepare_update_by_query(
    state: &AppState,
    index: &str,
    pipeline: Option<&str>,
) -> Result<(), Value> {
    if state.engine.get_index(index).is_err() {
        return Err(serde_json::from_str(&shard_not_ready_body(index))
            .unwrap_or_else(|_| update_by_query_unavailable(index, "index is not open")));
    }
    let me = state.engine.routing_node_id();
    let targets = state.engine.shard_search_targets(index);
    if !targets.is_empty() && !targets.iter().any(|id| id == &me) {
        let owner = targets.first().map(String::as_str).unwrap_or("unknown");
        let body = not_this_primary(index, owner);
        return Err(value_from_shard(&body));
    }
    if let Some(name) = pipeline {
        if let Some(reason) = unrunnable_pipeline_reason(&state.engine, name) {
            return Err(pipeline_rejected_body(&reason));
        }
    }
    Ok(())
}

async fn prepare_update_by_query_wait(
    state: &AppState,
    index: &str,
    pipeline: Option<&str>,
) -> Result<(), Value> {
    let deadline = Instant::now() + OWNER_READY_WAIT;
    loop {
        match prepare_update_by_query(state, index, pipeline) {
            Err(body) if is_not_ready_value(&body) && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => return other,
        }
    }
}

async fn forward_update_by_query(state: &AppState, index: &str, owner: &str, op: ShardOp) -> Value {
    let deadline = Instant::now() + OWNER_READY_WAIT;
    loop {
        match state.engine.forward_shard(owner, op.clone()).await {
            Ok(result) if is_shard_not_ready(&result) && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok(result) => return value_from_shard(&result),
            Err(err) => return update_by_query_unavailable(index, &err.to_string()),
        }
    }
}

async fn copy_updated_document(state: &AppState, index: &str, id: &str) -> Result<(), String> {
    if state.engine.primary_of(index, id).is_none() {
        return Ok(());
    }
    if !state.engine.owns_shard(index, id) {
        return Err(format!(
            "primary applied the update on [{index}] but document [{id}] is not owned by this node"
        ));
    }
    let op = match state.engine.get_index(index) {
        Ok(idx) => match idx.get_document(id).await {
            Ok(Some(doc)) => match serde_json::to_string(&doc) {
                Ok(source_json) => ShardOp::IndexDoc {
                    index: index.to_string(),
                    id: id.to_string(),
                    source_json,
                    op: "index".to_string(),
                    if_seq_no: None,
                    if_primary_term: None,
                    version: None,
                    version_type: None,
                    replica: false,
                    shard: 0,
                },
                Err(err) => {
                    return Err(format!(
                        "primary applied the update on [{index}] but document [{id}] is not JSON: {err}"
                    ));
                }
            },
            Ok(None) => ShardOp::DeleteDoc {
                index: index.to_string(),
                id: id.to_string(),
                if_seq_no: None,
                if_primary_term: None,
                replica: false,
                shard: 0,
            },
            Err(err) => {
                return Err(format!(
                    "primary applied the update on [{index}] but document [{id}] could not be read: {err}"
                ));
            }
        },
        Err(err) => {
            return Err(format!(
                "primary applied the update but [{index}] is not open: {err}"
            ));
        }
    };
    replicate_write(state, index, id, op)
        .await
        .map_err(|nack| nack_reason(&nack))
}

async fn apply_update_by_query_here(
    state: &AppState,
    index: &str,
    parts: &UpdateByQueryParts,
    max_docs: Option<u64>,
) -> Value {
    let idx = match state.engine.get_index(index) {
        Ok(idx) => idx,
        Err(err) => return update_by_query_unavailable(index, &err.to_string()),
    };
    let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let pipeline = parts
        .pipeline
        .as_ref()
        .map(|name| (std::sync::Arc::clone(&state.engine), name.clone()));
    let mut body = crate::es_compat::run_update_by_query_recorded_boxed(
        idx,
        parts.query.clone(),
        max_docs,
        parts.scroll_size,
        parts.script.clone(),
        pipeline,
        Some(std::sync::Arc::clone(&recorded)),
    )
    .await;
    let ids = recorded
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone();
    // A failed run may already have updated some documents. Copy those before
    // returning the error so a replica does not keep the pre-update source.
    let mut nacks = Vec::new();
    for id in ids {
        if let Err(reason) = copy_updated_document(state, index, &id).await {
            nacks.push((id, reason));
        }
    }
    if body.get("status").is_some() {
        return body;
    }
    if nacks.is_empty() {
        return body;
    }
    let Some(obj) = body.as_object_mut() else {
        return body;
    };
    let failures = obj
        .entry("failures".to_string())
        .or_insert_with(|| json!([]));
    if let Some(failures) = failures.as_array_mut() {
        for (id, reason) in nacks {
            failures.push(json!({
                "id": id,
                "cause": {
                    "type": "replica_not_acknowledged",
                    "reason": reason,
                }
            }));
        }
    }
    body
}

#[allow(clippy::too_many_arguments)]
async fn handle_update_by_query(
    state: &AppState,
    index: &str,
    query_json: &str,
    script_source: Option<String>,
    script_params_json: Option<String>,
    pipeline: Option<String>,
    max_docs: Option<u64>,
    scroll_size: u64,
    check_only: bool,
) -> ShardResult {
    if let Err(body) = prepare_update_by_query_wait(state, index, pipeline.as_deref()).await {
        let status = body
            .get("status")
            .and_then(Value::as_u64)
            .and_then(|status| u16::try_from(status).ok())
            .unwrap_or(503);
        return ShardResult::http(status, body.to_string());
    }
    if check_only {
        return ShardResult::http(200, json!({ "ok": true }).to_string());
    }
    let query = match serde_json::from_str::<Value>(query_json) {
        Ok(query) => query,
        Err(err) => {
            return ShardResult::http(
                400,
                json!({
                    "error": {
                        "type": "parsing_exception",
                        "reason": format!("update_by_query query is not JSON: {err}")
                    },
                    "status": 400
                })
                .to_string(),
            );
        }
    };
    let script = script_source.map(|source| {
        let params = script_params_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_else(|| json!({}));
        (source, params)
    });
    let parts = UpdateByQueryParts {
        query,
        script,
        pipeline,
        max_docs,
        scroll_size: scroll_size.max(1) as usize,
    };
    let value = apply_update_by_query_here(state, index, &parts, max_docs).await;
    let status = value
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .unwrap_or(200);
    ShardResult::http(status, value.to_string())
}

/// Run `_update_by_query` on every primary of `index` and copy the updates.
///
/// Primaries are visited in node-id order so `max_docs` is one budget for the
/// index. A primary that fails the preflight (missing pipeline, or not
/// answering) stops the call before any document is written. A later primary
/// that fails after an earlier one has already applied is returned as that
/// error; the earlier updates stay. `[cluster] enabled = false` does not call
/// this.
pub(crate) async fn update_by_query_on_primaries(
    state: &AppState,
    index: &str,
    parts: &UpdateByQueryParts,
) -> Value {
    let targets = state.engine.shard_search_targets(index);
    if targets.is_empty() {
        // No plan: this node holds the whole index. There is no primary to
        // call, so the run stays here. `replicate_write` does nothing when
        // `primary_of` is `None`.
        return apply_update_by_query_here(state, index, parts, parts.max_docs).await;
    }
    // Every primary, including this node, is a shard call. The runner builds
    // `search_inner` on the caller's stack, and that overflows a debug worker
    // (2 MiB) once the fan-out is above it. `MemoryShardRpc` already runs a
    // handler on an 8 MiB thread — the same thread a remote primary uses — so
    // this node's own shard takes that path too. A primary that is not
    // answering fails here, before any document is written.
    for target in &targets {
        let value = forward_update_by_query(
            state,
            index,
            target,
            update_by_query_op(index, parts, parts.max_docs, true),
        )
        .await;
        if value.get("ok").and_then(Value::as_bool) != Some(true) {
            return ensure_error_status(value);
        }
    }
    if parts.max_docs == Some(0) {
        return empty_update_by_query();
    }

    let mut remaining = parts.max_docs;
    let mut parts_out = Vec::with_capacity(targets.len());
    for target in &targets {
        if remaining == Some(0) {
            break;
        }
        let cap = remaining;
        let value = forward_update_by_query(
            state,
            index,
            target,
            update_by_query_op(index, parts, cap, false),
        )
        .await;
        // A shard that failed its own run (flush, script limit, or a primary
        // that stopped answering) has no `updated` count. Replica nacks keep
        // `updated` and sit in `failures` until the aggregate below.
        if value.get("updated").is_none() {
            return ensure_error_status(value);
        }
        if let Some(left) = remaining.as_mut() {
            let used = value.get("total").and_then(Value::as_u64).unwrap_or(0);
            *left = left.saturating_sub(used);
        }
        parts_out.push(value);
    }
    if parts_out.is_empty() {
        return empty_update_by_query();
    }
    stamp_replica_nack(
        aggregate_by_query_results(parts_out, "updated"),
        "primary applied the update but a replica copy missed quorum. The primary has the update; retry the update so an in-sync replica can take the copy",
    )
}

// ── `_delete_by_query` on every primary ─────────────────────────────────────

/// One `_delete_by_query` after the accepting node has validated the query.
///
/// [`delete_by_query_on_primaries`] borrows this. `max_docs` is the caller's
/// cap for the whole index; the fan-out spends it across primaries.
#[derive(Clone)]
pub(crate) struct DeleteByQueryParts {
    pub query: Value,
    pub max_docs: Option<u64>,
    pub scroll_size: usize,
}

/// Box the per-index fan-out so it is not inlined into `delete_by_query`.
///
/// Same split as [`pin_update_by_query_on_primaries`]: a debug worker stack
/// overflows when the handler and the runner are both built on it.
#[inline(never)]
pub(crate) fn pin_delete_by_query_on_primaries(
    state: AppState,
    index: String,
    parts: DeleteByQueryParts,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send>> {
    Box::pin(async move { delete_by_query_on_primaries(&state, &index, &parts).await })
}

fn delete_by_query_op(
    index: &str,
    parts: &DeleteByQueryParts,
    max_docs: Option<u64>,
    check_only: bool,
) -> ShardOp {
    ShardOp::DeleteByQuery {
        index: index.to_string(),
        query_json: parts.query.to_string(),
        max_docs,
        scroll_size: parts.scroll_size as u64,
        check_only,
    }
}

fn delete_by_query_unavailable(index: &str, detail: &str) -> Value {
    json!({
        "error": {
            "type": "shard_not_ready",
            "reason": format!("delete_by_query on [{index}] failed: {detail}")
        },
        "status": 503
    })
}

fn empty_delete_by_query() -> Value {
    json!({
        "took": 0,
        "timed_out": false,
        "total": 0,
        "deleted": 0,
        "batches": 0,
        "version_conflicts": 0,
        "noops": 0,
        "failures": [],
        "throttled_millis": 0,
        "requests_per_second": -1,
        "throttled_until_millis": 0,
    })
}

/// Index open, and this node is a primary when a plan exists.
///
/// `Err` is a body the HTTP layer can return. Nothing has been written.
fn prepare_delete_by_query(state: &AppState, index: &str) -> Result<(), Value> {
    if state.engine.get_index(index).is_err() {
        return Err(serde_json::from_str(&shard_not_ready_body(index))
            .unwrap_or_else(|_| delete_by_query_unavailable(index, "index is not open")));
    }
    let me = state.engine.routing_node_id();
    let targets = state.engine.shard_search_targets(index);
    if !targets.is_empty() && !targets.iter().any(|id| id == &me) {
        let owner = targets.first().map(String::as_str).unwrap_or("unknown");
        let body = not_this_primary(index, owner);
        return Err(value_from_shard(&body));
    }
    Ok(())
}

async fn prepare_delete_by_query_wait(state: &AppState, index: &str) -> Result<(), Value> {
    let deadline = Instant::now() + OWNER_READY_WAIT;
    loop {
        match prepare_delete_by_query(state, index) {
            Err(body) if is_not_ready_value(&body) && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => return other,
        }
    }
}

async fn forward_delete_by_query(state: &AppState, index: &str, owner: &str, op: ShardOp) -> Value {
    let deadline = Instant::now() + OWNER_READY_WAIT;
    loop {
        match state.engine.forward_shard(owner, op.clone()).await {
            Ok(result) if is_shard_not_ready(&result) && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok(result) => return value_from_shard(&result),
            Err(err) => return delete_by_query_unavailable(index, &err.to_string()),
        }
    }
}

async fn copy_deleted_document(state: &AppState, index: &str, id: &str) -> Result<(), String> {
    if state.engine.primary_of(index, id).is_none() {
        return Ok(());
    }
    if !state.engine.owns_shard(index, id) {
        return Err(format!(
            "primary applied the delete on [{index}] but document [{id}] is not owned by this node"
        ));
    }
    let op = ShardOp::DeleteDoc {
        index: index.to_string(),
        id: id.to_string(),
        if_seq_no: None,
        if_primary_term: None,
        replica: false,
        shard: 0,
    };
    replicate_write(state, index, id, op)
        .await
        .map_err(|nack| nack_reason(&nack))
}

async fn apply_delete_by_query_here(
    state: &AppState,
    index: &str,
    parts: &DeleteByQueryParts,
    max_docs: Option<u64>,
) -> Value {
    let idx = match state.engine.get_index(index) {
        Ok(idx) => idx,
        Err(err) => return delete_by_query_unavailable(index, &err.to_string()),
    };
    let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut body = crate::es_compat::run_delete_by_query_recorded_boxed(
        idx,
        parts.query.clone(),
        max_docs,
        parts.scroll_size,
        Some(std::sync::Arc::clone(&recorded)),
    )
    .await;
    let ids = recorded
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone();
    // A failed run may already have deleted some documents. Copy those before
    // returning the error so a replica does not keep them.
    let mut nacks = Vec::new();
    for id in ids {
        if let Err(reason) = copy_deleted_document(state, index, &id).await {
            nacks.push((id, reason));
        }
    }
    if body.get("status").is_some() {
        return body;
    }
    if nacks.is_empty() {
        return body;
    }
    let Some(obj) = body.as_object_mut() else {
        return body;
    };
    let failures = obj
        .entry("failures".to_string())
        .or_insert_with(|| json!([]));
    if let Some(failures) = failures.as_array_mut() {
        for (id, reason) in nacks {
            failures.push(json!({
                "id": id,
                "cause": {
                    "type": "replica_not_acknowledged",
                    "reason": reason,
                }
            }));
        }
    }
    body
}

async fn handle_delete_by_query(
    state: &AppState,
    index: &str,
    query_json: &str,
    max_docs: Option<u64>,
    scroll_size: u64,
    check_only: bool,
) -> ShardResult {
    if let Err(body) = prepare_delete_by_query_wait(state, index).await {
        let status = body
            .get("status")
            .and_then(Value::as_u64)
            .and_then(|status| u16::try_from(status).ok())
            .unwrap_or(503);
        return ShardResult::http(status, body.to_string());
    }
    if check_only {
        return ShardResult::http(200, json!({ "ok": true }).to_string());
    }
    let query = match serde_json::from_str::<Value>(query_json) {
        Ok(query) => query,
        Err(err) => {
            return ShardResult::http(
                400,
                json!({
                    "error": {
                        "type": "parsing_exception",
                        "reason": format!("delete_by_query query is not JSON: {err}")
                    },
                    "status": 400
                })
                .to_string(),
            );
        }
    };
    let parts = DeleteByQueryParts {
        query,
        max_docs,
        scroll_size: scroll_size.max(1) as usize,
    };
    let value = apply_delete_by_query_here(state, index, &parts, max_docs).await;
    let status = value
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .unwrap_or(200);
    ShardResult::http(status, value.to_string())
}

/// Run `_delete_by_query` on every primary of `index` and copy the deletes.
///
/// Primaries are visited in node-id order so `max_docs` is one budget for the
/// index. A primary that does not answer stops the call before any document
/// is written. A later primary that fails after an earlier one has already
/// applied is returned as that error; the earlier deletes stay.
/// `[cluster] enabled = false` does not call this.
pub(crate) async fn delete_by_query_on_primaries(
    state: &AppState,
    index: &str,
    parts: &DeleteByQueryParts,
) -> Value {
    let targets = state.engine.shard_search_targets(index);
    if targets.is_empty() {
        // No plan: this node holds the whole index. There is no primary to
        // call, so the run stays here. `replicate_write` does nothing when
        // `primary_of` is `None`.
        return apply_delete_by_query_here(state, index, parts, parts.max_docs).await;
    }
    // Every primary, including this node, is a shard call. The runner builds
    // a search on the caller's stack, and that overflows a debug worker
    // (2 MiB) once the fan-out is above it. `MemoryShardRpc` already runs a
    // handler on an 8 MiB thread, so this node's own shard takes that path
    // too. A primary that is not answering fails here, before any document
    // is written.
    for target in &targets {
        let value = forward_delete_by_query(
            state,
            index,
            target,
            delete_by_query_op(index, parts, parts.max_docs, true),
        )
        .await;
        if value.get("ok").and_then(Value::as_bool) != Some(true) {
            return ensure_error_status(value);
        }
    }
    if parts.max_docs == Some(0) {
        return empty_delete_by_query();
    }

    let mut remaining = parts.max_docs;
    let mut parts_out = Vec::with_capacity(targets.len());
    for target in &targets {
        if remaining == Some(0) {
            break;
        }
        let cap = remaining;
        let value = forward_delete_by_query(
            state,
            index,
            target,
            delete_by_query_op(index, parts, cap, false),
        )
        .await;
        // A shard that failed its own run (flush, script limit, or a primary
        // that stopped answering) has no `deleted` count. Replica nacks keep
        // `deleted` and sit in `failures` until the aggregate below.
        if value.get("deleted").is_none() {
            return ensure_error_status(value);
        }
        if let Some(left) = remaining.as_mut() {
            let used = value.get("total").and_then(Value::as_u64).unwrap_or(0);
            *left = left.saturating_sub(used);
        }
        parts_out.push(value);
    }
    if parts_out.is_empty() {
        return empty_delete_by_query();
    }
    stamp_replica_nack(
        aggregate_by_query_results(parts_out, "deleted"),
        "primary applied the delete but a replica copy missed quorum. The primary has the delete; retry the delete so an in-sync replica can take the copy",
    )
}
