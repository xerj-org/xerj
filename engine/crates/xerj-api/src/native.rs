//! Native xerj REST API handlers (port 8080).

use std::time::Instant;

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{Datelike as _, Utc};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;
use xerj_common::types::{FieldConfig, IndexName, Schema};
use xerj_query::parse_request;

use xerj_engine::{EnrichTable, FieldEncodingInfo};

use crate::{
    error::{native_error, ApiError},
    responses::{
        NativeHealthResponse, NativeIndexInfo, NativeIngestResponse, NativeResponse,
        NativeSchemaResponse,
    },
    state::{AppState, IndexSettings},
};

// ─────────────────────────────────────────────────────────────────────────────
// Request bodies
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateIndexRequest {
    pub name: String,
    #[serde(default)]
    pub settings: IndexSettings,
    #[serde(default)]
    pub fields: Vec<FieldConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum IngestRequest {
    Single(Value),
    Batch(Vec<Value>),
}

#[derive(Debug, Deserialize)]
pub struct NativeSearchRequest {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default = "default_size")]
    pub size: usize,
    #[serde(default)]
    pub from: usize,
    #[serde(default)]
    pub query: Option<Value>,
    #[serde(default)]
    pub sort: Option<Value>,
    #[serde(default)]
    pub fields: Option<Vec<String>>,
}

fn default_size() -> usize {
    10
}

#[derive(Debug, Deserialize)]
pub struct EvolveSchemaRequest {
    pub fields: Vec<FieldConfig>,
}

#[derive(Debug, Deserialize)]
pub struct BulkIngestRequest {
    pub docs: Vec<Value>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: POST /v1/indices — create index
// ─────────────────────────────────────────────────────────────────────────────

pub async fn create_index(
    State(state): State<AppState>,
    Json(req): Json<CreateIndexRequest>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let index_name = match IndexName::new(&req.name) {
        Ok(n) => n,
        Err(e) => {
            return native_error(e, Some(&request_id), started.elapsed().as_millis() as u64)
                .into_response();
        }
    };

    let name_str = index_name.as_str().to_string();

    // Build schema from provided fields.
    let mut schema = Schema::empty();
    for field in req.fields {
        if let Err(e) = schema.add_field(field) {
            return native_error(e, Some(&request_id), started.elapsed().as_millis() as u64)
                .into_response();
        }
    }

    if let Err(e) = state.engine.create_index(&name_str, schema) {
        let xerj_err: xerj_common::XerjError = e.into();
        return native_error(
            xerj_err,
            Some(&request_id),
            started.elapsed().as_millis() as u64,
        )
        .into_response();
    }

    let took_ms = started.elapsed().as_millis() as u64;
    let resp = NativeResponse::new(
        serde_json::json!({ "index": name_str, "acknowledged": true }),
        took_ms,
        &request_id,
    );
    (StatusCode::CREATED, Json(resp)).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: GET /v1/indices/{name}
// ─────────────────────────────────────────────────────────────────────────────

pub async fn get_index(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let idx = match state.engine.get_index(&name) {
        Ok(i) => i,
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            return native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response();
        }
    };

    let stats = idx.stats().await;
    let schema = idx.schema().await;

    let info = NativeIndexInfo {
        name: name.clone(),
        doc_count: stats.doc_count,
        schema_version: stats.schema_version,
        field_count: schema.field_count(),
        settings: IndexSettings::default(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    let took_ms = started.elapsed().as_millis() as u64;
    let resp = NativeResponse::new(info, took_ms, &request_id);
    Json(resp).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: DELETE /v1/indices/{name}
// ─────────────────────────────────────────────────────────────────────────────

pub async fn delete_index(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    if let Err(e) = state.engine.delete_index(&name).await {
        let xerj_err: xerj_common::XerjError = e.into();
        return native_error(
            xerj_err,
            Some(&request_id),
            started.elapsed().as_millis() as u64,
        )
        .into_response();
    }

    let took_ms = started.elapsed().as_millis() as u64;
    let resp = NativeResponse::new(
        serde_json::json!({ "index": name, "acknowledged": true }),
        took_ms,
        &request_id,
    );
    Json(resp).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: POST /v1/indices/{name}/docs — ingest doc(s)
// ─────────────────────────────────────────────────────────────────────────────

pub async fn ingest_docs(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<IngestRequest>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let idx = match state.engine.get_index(&name) {
        Ok(i) => i,
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            return native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response();
        }
    };

    match req {
        IngestRequest::Single(doc) => {
            let id = extract_or_gen_id(&doc);
            match idx.index_document(Some(id.clone()), doc).await {
                Ok(resp) => {
                    let took_ms = started.elapsed().as_millis() as u64;
                    state.metrics.record_doc_indexed(&name);
                    let response = NativeResponse::new(
                        NativeIngestResponse {
                            index: name,
                            id: resp.id,
                            result: resp.result,
                            seq_no: resp.seq_no,
                        },
                        took_ms,
                        &request_id,
                    );
                    (StatusCode::CREATED, Json(response)).into_response()
                }
                Err(e) => {
                    let xerj_err: xerj_common::XerjError = e.into();
                    native_error(
                        xerj_err,
                        Some(&request_id),
                        started.elapsed().as_millis() as u64,
                    )
                    .into_response()
                }
            }
        }
        IngestRequest::Batch(docs) => {
            let count = docs.len() as u64;
            let mut ids = Vec::with_capacity(docs.len());
            let mut last_seq_no = 0u64;

            for doc in docs {
                let id = extract_or_gen_id(&doc);
                match idx.index_document(Some(id.clone()), doc).await {
                    Ok(r) => {
                        last_seq_no = r.seq_no;
                        ids.push(r.id);
                        state.metrics.record_doc_indexed(&name);
                    }
                    Err(e) => {
                        let xerj_err: xerj_common::XerjError = e.into();
                        return native_error(
                            xerj_err,
                            Some(&request_id),
                            started.elapsed().as_millis() as u64,
                        )
                        .into_response();
                    }
                }
            }

            let took_ms = started.elapsed().as_millis() as u64;
            let resp = NativeResponse::new(
                serde_json::json!({
                    "index": name,
                    "indexed": count,
                    "ids": ids,
                    "last_seq_no": last_seq_no,
                }),
                took_ms,
                &request_id,
            );
            (StatusCode::CREATED, Json(resp)).into_response()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: GET /v1/indices/{name}/docs/{id}
// ─────────────────────────────────────────────────────────────────────────────

pub async fn get_doc(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, String)>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let idx = match state.engine.get_index(&name) {
        Ok(i) => i,
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            return native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response();
        }
    };

    match idx.get_document(&id).await {
        Ok(Some(source)) => {
            let took_ms = started.elapsed().as_millis() as u64;
            let resp = NativeResponse::new(
                serde_json::json!({ "index": name, "id": id, "_source": source }),
                took_ms,
                &request_id,
            );
            Json(resp).into_response()
        }
        Ok(None) => {
            let e = xerj_common::XerjError::document_not_found(&id, &name);
            native_error(e, Some(&request_id), started.elapsed().as_millis() as u64).into_response()
        }
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: DELETE /v1/indices/{name}/docs/{id}
// ─────────────────────────────────────────────────────────────────────────────

pub async fn delete_doc(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, String)>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let idx = match state.engine.get_index(&name) {
        Ok(i) => i,
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            return native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response();
        }
    };

    match idx.delete_document(&id).await {
        Ok(_) => {
            let took_ms = started.elapsed().as_millis() as u64;
            let resp = NativeResponse::new(
                serde_json::json!({ "index": name, "id": id, "result": "deleted" }),
                took_ms,
                &request_id,
            );
            Json(resp).into_response()
        }
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: POST /v1/indices/{name}/search
// ─────────────────────────────────────────────────────────────────────────────

pub async fn search(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<NativeSearchRequest>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let idx = match state.engine.get_index(&name) {
        Ok(i) => i,
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            return native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response();
        }
    };

    state.metrics.queries_executed.inc();
    state
        .metrics
        .queries_by_index
        .with_label_values(&[&name])
        .inc();

    // Build a query body for the parser.
    let query_body = if let Some(q_str) = &req.q {
        serde_json::json!({
            "query": {
                "multi_match": {
                    "query": q_str,
                    "fields": ["*"]
                }
            },
            "from": req.from,
            "size": req.size
        })
    } else if let Some(query) = &req.query {
        serde_json::json!({
            "query": query,
            "from": req.from,
            "size": req.size
        })
    } else {
        serde_json::json!({
            "query": { "match_all": {} },
            "from": req.from,
            "size": req.size
        })
    };

    let search_req = match parse_request(&query_body) {
        Ok(r) => r,
        Err(e) => {
            let ze = xerj_common::XerjError::invalid_query(e.to_string());
            return native_error(ze, Some(&request_id), started.elapsed().as_millis() as u64)
                .into_response();
        }
    };

    match idx.search(&search_req).await {
        Ok(result) => {
            let took_ms = started.elapsed().as_millis() as u64;
            state.metrics.query_latency.observe(took_ms as f64 / 1000.0);

            let hits: Vec<Value> = result
                .hits
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "_id": h.id,
                        "_score": h.score,
                        "_source": h.source,
                    })
                })
                .collect();

            let resp = NativeResponse::new(
                serde_json::json!({
                    "total": result.total.value,
                    "hits": hits,
                }),
                took_ms,
                &request_id,
            );
            Json(resp).into_response()
        }
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: POST /v1/indices/{name}/_flush
// ─────────────────────────────────────────────────────────────────────────────

/// Flush the in-memory memtable for an index to a durable on-disk segment.
///
/// After a flush the WAL checkpoint advances, old WAL generations are pruned,
/// and the data is guaranteed to survive a restart without WAL replay overhead.
pub async fn flush_index(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    if let Err(e) = state.engine.flush_index(&name).await {
        let xerj_err: xerj_common::XerjError = e.into();
        return native_error(
            xerj_err,
            Some(&request_id),
            started.elapsed().as_millis() as u64,
        )
        .into_response();
    }

    let took_ms = started.elapsed().as_millis() as u64;
    let resp = NativeResponse::new(
        serde_json::json!({ "index": name, "flushed": true }),
        took_ms,
        &request_id,
    );
    Json(resp).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: GET /v1/health
// ─────────────────────────────────────────────────────────────────────────────

pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let health = state.engine.health().await;

    let took_ms = started.elapsed().as_millis() as u64;
    let resp = NativeResponse::new(
        NativeHealthResponse {
            status: health.status,
            index_count: health.index_count,
            total_docs: health.total_docs,
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        took_ms,
        &request_id,
    );
    Json(resp).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// k8s probes — v0.8 8-P4
//
// `/health/live` and `/health/ready` are the conventional probe endpoints
// kubelet hits.  We expose them as cheap, dependency-free handlers:
//
// - **liveness**: returns 200 if the process can serve HTTP at all.  No
//   engine call, no allocations.  kubelet uses this to decide whether to
//   restart the pod.  A liveness 5xx triggers a pod restart, so this MUST
//   be a no-op — checking the engine here would create a feedback loop
//   where a slow engine flapping flush causes constant pod restarts.
//
// - **readiness**: returns 200 only when the engine reports a non-`red`
//   cluster status (i.e. at least one index is queryable).  kubelet uses
//   this to decide whether to send traffic.  Until ready, the Service
//   removes the pod from rotation but doesn't restart it — appropriate
//   for "still replaying WAL" or "still loading 50 K segments" startup
//   states that are transient but visible.
// ─────────────────────────────────────────────────────────────────────────────

pub async fn liveness() -> impl IntoResponse {
    (axum::http::StatusCode::OK, "live").into_response()
}

pub async fn readiness(State(state): State<AppState>) -> impl IntoResponse {
    let h = state.engine.health().await;
    if h.status == "red" {
        (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            format!("not ready: cluster status = {}", h.status),
        )
            .into_response()
    } else {
        (axum::http::StatusCode::OK, "ready").into_response()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: GET /v1/cluster/health — native cluster health summary
//
// Mirrors the data the ES-compat `_cluster/health` exposes, in the native
// `{ data, took_ms, request_id }` envelope. Single-node by design (Xerj is one
// binary), so node counts are 1.
// ─────────────────────────────────────────────────────────────────────────────

pub async fn cluster_health(State(state): State<AppState>) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let h = state.engine.health().await;
    let took_ms = started.elapsed().as_millis() as u64;
    let resp = NativeResponse::new(
        serde_json::json!({
            "cluster_name": "xerj",
            "status": h.status,
            "number_of_nodes": 1,
            "number_of_data_nodes": 1,
            "index_count": h.index_count,
            "total_docs": h.total_docs,
            "version": h.version,
        }),
        took_ms,
        &request_id,
    );
    Json(resp).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: POST /v1/admin/flush — flush every index's memtable to disk
//
// Cluster-wide counterpart of `POST /v1/indices/:name/_flush`. Walks every
// index, flushes its memtable to a durable segment, and reports per-index
// success so an operator can force a checkpoint before a backup or upgrade.
// ─────────────────────────────────────────────────────────────────────────────

pub async fn admin_flush(State(state): State<AppState>) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let indices = state.engine.list_indices().await;
    let mut flushed: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    for info in &indices {
        match state.engine.flush_index(&info.name).await {
            Ok(()) => flushed.push(info.name.clone()),
            Err(_) => failed.push(info.name.clone()),
        }
    }

    let took_ms = started.elapsed().as_millis() as u64;
    let resp = NativeResponse::new(
        serde_json::json!({
            "flushed": flushed.len(),
            "indices": flushed,
            "failed": failed,
        }),
        took_ms,
        &request_id,
    );
    Json(resp).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: POST /v1/admin/backup — snapshot the whole cluster to disk
//
// Optional JSON body: { "repo_path"?: string, "name"?: string,
// "indices"?: [string] }. Defaults: repo_path = "<data_dir>/_backups",
// name = "backup-<uuid>", indices = all. Flushes each index then copies its
// WAL + segments + schema and writes a manifest (engine::create_snapshot).
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct BackupRequest {
    #[serde(default)]
    pub repo_path: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub indices: Option<Vec<String>>,
}

pub async fn admin_backup(State(state): State<AppState>, body: Bytes) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    // Body is optional and lenient — `{}` or empty both mean "defaults".
    let req: BackupRequest = if body.is_empty() {
        BackupRequest::default()
    } else {
        serde_json::from_slice(&body).unwrap_or_default()
    };

    let data_dir = state.engine.config().server.data_dir.clone();
    let repo_path = req
        .repo_path
        .unwrap_or_else(|| format!("{data_dir}/_backups"));
    let name = req
        .name
        .unwrap_or_else(|| format!("backup-{}", Uuid::new_v4()));

    match state
        .engine
        .create_snapshot(&repo_path, &name, req.indices)
        .await
    {
        Ok(manifest) => {
            let took_ms = started.elapsed().as_millis() as u64;
            let resp = NativeResponse::new(
                serde_json::json!({
                    "backup": name,
                    "repo_path": repo_path,
                    "manifest": manifest,
                }),
                took_ms,
                &request_id,
            );
            (StatusCode::CREATED, Json(resp)).into_response()
        }
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Admin: slow query log — v0.8 8-P6
//
// `GET /v1/admin/slow_queries` returns the last N slow queries.
// `DELETE /v1/admin/slow_queries` clears the buffer.
// `PUT /v1/admin/slow_queries/threshold/:ms` sets the threshold at runtime
// (also exposed via SIGHUP config reload in 8-P5).
// ─────────────────────────────────────────────────────────────────────────────

pub async fn admin_slow_queries(State(state): State<AppState>) -> impl IntoResponse {
    let log = &state.engine.slow_query;
    let body = serde_json::json!({
        "threshold_ms": log.threshold_ms(),
        "total_slow":   log.total_slow(),
        "entries":      log.snapshot(),
    });
    Json(body).into_response()
}

pub async fn admin_slow_queries_clear(State(state): State<AppState>) -> impl IntoResponse {
    state.engine.slow_query.clear();
    (axum::http::StatusCode::OK, "cleared").into_response()
}

pub async fn admin_slow_queries_set_threshold(
    State(state): State<AppState>,
    axum::extract::Path(ms): axum::extract::Path<u64>,
) -> impl IntoResponse {
    state.engine.slow_query.set_threshold_ms(ms);
    Json(serde_json::json!({ "threshold_ms": ms })).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// v0.9 9-P4 — Audit log admin
// ─────────────────────────────────────────────────────────────────────────────

pub async fn audit_search(State(state): State<AppState>) -> impl IntoResponse {
    let snap = state.engine.audit.snapshot();
    let body = serde_json::json!({
        "next_seq": state.engine.audit.next_seq(),
        "entries":  snap,
    });
    Json(body).into_response()
}

pub async fn audit_verify(State(state): State<AppState>) -> impl IntoResponse {
    match state.engine.audit.verify() {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err((seq, expected, actual)) => {
            let body = serde_json::json!({
                "ok": false,
                "broken_at_seq": seq,
                "expected_hash": expected,
                "actual_hash": actual,
            });
            (axum::http::StatusCode::CONFLICT, Json(body)).into_response()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// v0.9 9-P2 — Role store admin — data model only, NOT ENFORCED (RC4 item 6)
// ─────────────────────────────────────────────────────────────────────────────

/// Honest-surface banner stamped on every `/_security/role*` response. Roles
/// are stored and round-trip, but the auth path never consults them — every
/// authenticated caller is superuser. See `xerj_engine::rbac` module docs.
const RBAC_NOT_ENFORCED_WARNING: &str =
    "roles are stored but NOT enforced: every authenticated caller has full \
     superuser access regardless of any role assignment. Full RBAC enforcement \
     is deferred.";

pub async fn rbac_list_roles(State(state): State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "roles": state.engine.roles.list(),
        "enforced": false,
        "warning": RBAC_NOT_ENFORCED_WARNING,
    }))
    .into_response()
}

pub async fn rbac_get_role(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> impl IntoResponse {
    match state.engine.roles.get(&name) {
        Some(r) => Json(serde_json::json!({
            "role": r,
            "enforced": false,
            "warning": RBAC_NOT_ENFORCED_WARNING,
        }))
        .into_response(),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            format!("role '{name}' not found"),
        )
            .into_response(),
    }
}

pub async fn rbac_put_role(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Json(body): Json<xerj_engine::rbac::Role>,
) -> impl IntoResponse {
    let mut r = body;
    // Caller may omit the name field; the URL is authoritative.
    r.name = name;
    state.engine.roles.put(r.clone());
    // Loud signal that this PUT grants/restricts nothing (item 6).
    tracing::warn!(
        role = %r.name,
        "PUT /_security/role: role stored but NOT enforced — caller remains superuser"
    );
    Json(serde_json::json!({
        "role": r,
        "created": true,
        "enforced": false,
        "warning": RBAC_NOT_ENFORCED_WARNING,
    }))
    .into_response()
}

pub async fn rbac_delete_role(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> impl IntoResponse {
    match state.engine.roles.delete(&name) {
        Some(_) => Json(serde_json::json!({
            "deleted": true,
            "enforced": false,
            "warning": RBAC_NOT_ENFORCED_WARNING,
        }))
        .into_response(),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            format!("role '{name}' not found"),
        )
            .into_response(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: GET /v1/metrics
// ─────────────────────────────────────────────────────────────────────────────

pub async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    match state.metrics.gather_text() {
        Ok(text) => (
            StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            )],
            text,
        )
            .into_response(),
        Err(e) => {
            let api_err = ApiError::new(e);
            api_err.into_response()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: GET /v1/schema/{name}
// ─────────────────────────────────────────────────────────────────────────────

pub async fn get_schema(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let idx = match state.engine.get_index(&name) {
        Ok(i) => i,
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            return native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response();
        }
    };

    let schema = idx.schema().await;
    let took_ms = started.elapsed().as_millis() as u64;
    let resp = NativeResponse::new(
        NativeSchemaResponse {
            index: name,
            schema,
        },
        took_ms,
        &request_id,
    );
    Json(resp).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: POST /v1/schema/{name}/evolve
// ─────────────────────────────────────────────────────────────────────────────

pub async fn evolve_schema(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<EvolveSchemaRequest>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let idx = match state.engine.get_index(&name) {
        Ok(i) => i,
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            return native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response();
        }
    };

    let mut added = 0usize;
    for field in req.fields {
        match idx.add_field(field).await {
            Ok(()) => added += 1,
            Err(e) => {
                let xerj_err: xerj_common::XerjError = e.into();
                return native_error(
                    xerj_err,
                    Some(&request_id),
                    started.elapsed().as_millis() as u64,
                )
                .into_response();
            }
        }
    }

    let schema = idx.schema().await;
    let took_ms = started.elapsed().as_millis() as u64;
    let resp = NativeResponse::new(
        serde_json::json!({
            "index": name,
            "fields_added": added,
            "schema_version": schema.version,
        }),
        took_ms,
        &request_id,
    );
    Json(resp).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: POST /v1/indices/{name}/docs/_bulk
// ─────────────────────────────────────────────────────────────────────────────

pub async fn bulk_ingest(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<BulkIngestRequest>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let idx = match state.engine.get_index(&name) {
        Ok(i) => i,
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            return native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response();
        }
    };

    let count = req.docs.len() as u64;
    let mut indexed = 0u64;

    for doc in req.docs {
        let id = extract_or_gen_id(&doc);
        if idx.index_document(Some(id), doc).await.is_ok() {
            indexed += 1;
            state.metrics.record_doc_indexed(&name);
        }
    }

    let took_ms = started.elapsed().as_millis() as u64;
    let resp = NativeResponse::new(
        serde_json::json!({
            "index": name,
            "indexed": indexed,
            "errors": indexed < count,
        }),
        took_ms,
        &request_id,
    );
    (StatusCode::OK, Json(resp)).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: POST /v1/indices/{name}/logs — batch log ingestion
// ─────────────────────────────────────────────────────────────────────────────

/// Request body for the log ingest endpoint.
///
/// Accepts a JSON array of log records.  Each record is any JSON object;
/// `@timestamp`, `level`, and `message` fields are conventionally expected
/// but not required.  A unique ID is auto-generated for every record.
#[derive(Debug, Deserialize)]
pub struct LogIngestRequest(pub Vec<Value>);

pub async fn ingest_logs(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(LogIngestRequest(records)): Json<LogIngestRequest>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    // Auto-create the index if it does not exist yet.
    let idx = match state.engine.get_or_create_index(&name) {
        Ok(i) => i,
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            return native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response();
        }
    };

    let total = records.len() as u64;
    let mut indexed = 0u64;
    let mut ids: Vec<String> = Vec::with_capacity(records.len());

    for record in records {
        // Auto-generate a unique ID for every log record.
        let id = Uuid::new_v4().to_string();
        match idx.index_document(Some(id.clone()), record).await {
            Ok(_) => {
                indexed += 1;
                ids.push(id);
                state.metrics.record_doc_indexed(&name);
            }
            Err(e) => {
                tracing::warn!(
                    index = name.as_str(),
                    error = %e,
                    "failed to index log record"
                );
            }
        }
    }

    let took_ms = started.elapsed().as_millis() as u64;
    let resp = NativeResponse::new(
        serde_json::json!({
            "index": name,
            "indexed": indexed,
            "total": total,
            "errors": indexed < total,
            "ids": ids,
        }),
        took_ms,
        &request_id,
    );
    (StatusCode::CREATED, Json(resp)).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: POST /v1/indices/{name}/otlp — OpenTelemetry Log ingest
// ─────────────────────────────────────────────────────────────────────────────

/// Ingest OpenTelemetry logs in OTLP JSON format.
///
/// Expected body shape (OTLP `ExportLogsServiceRequest`):
/// ```json
/// {
///   "resourceLogs": [{
///     "resource": { "attributes": [{ "key": "service.name", "value": { "stringValue": "my-svc" } }] },
///     "scopeLogs": [{
///       "logRecords": [{
///         "timeUnixNano": "1712000000000000000",
///         "severityText": "INFO",
///         "body": { "stringValue": "hello world" },
///         "attributes": [{ "key": "http.status_code", "value": { "intValue": "200" } }]
///       }]
///     }]
///   }]
/// }
/// ```
///
/// Each log record is flattened into an indexed document with the fields:
/// `timestamp`, `severity`, `body`, `trace_id`, `span_id`, and one field per
/// OTLP attribute.  Resource attributes are merged at the top level with a
/// `resource.` prefix.
pub async fn ingest_otlp(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let idx = match state.engine.get_or_create_index(&name) {
        Ok(i) => i,
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            return native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response();
        }
    };

    let mut total = 0u64;
    let mut indexed = 0u64;

    // Parse the OTLP resource-log tree.
    if let Some(resource_logs) = body.get("resourceLogs").and_then(Value::as_array) {
        for resource_log in resource_logs {
            // Collect resource-level attributes (e.g. service.name).
            let mut resource_attrs = serde_json::Map::new();
            if let Some(res) = resource_log.get("resource") {
                if let Some(attrs) = res.get("attributes").and_then(Value::as_array) {
                    for attr in attrs {
                        if let (Some(key), Some(val)) =
                            (attr.get("key").and_then(Value::as_str), attr.get("value"))
                        {
                            let field = format!("resource.{key}");
                            resource_attrs.insert(field, extract_otlp_any_value(val));
                        }
                    }
                }
            }

            if let Some(scope_logs) = resource_log.get("scopeLogs").and_then(Value::as_array) {
                for scope_log in scope_logs {
                    // Scope name (instrumentation library).
                    let scope_name = scope_log
                        .get("scope")
                        .and_then(|s| s.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();

                    if let Some(records) = scope_log.get("logRecords").and_then(Value::as_array) {
                        for record in records {
                            total += 1;
                            let doc = otlp_record_to_doc(record, &resource_attrs, &scope_name);
                            let id = Uuid::new_v4().to_string();
                            match idx.index_document(Some(id), doc).await {
                                Ok(_) => {
                                    indexed += 1;
                                    state.metrics.record_doc_indexed(&name);
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        index = name.as_str(),
                                        error = %e,
                                        "otlp: failed to index log record"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let took_ms = started.elapsed().as_millis() as u64;
    let resp = NativeResponse::new(
        serde_json::json!({
            "index": name,
            "indexed": indexed,
            "total": total,
            "errors": indexed < total,
        }),
        took_ms,
        &request_id,
    );
    (StatusCode::CREATED, Json(resp)).into_response()
}

/// Flatten a single OTLP `LogRecord` into a flat JSON document ready for
/// indexing.
fn otlp_record_to_doc(
    record: &Value,
    resource_attrs: &serde_json::Map<String, Value>,
    scope_name: &str,
) -> Value {
    let mut doc = serde_json::Map::new();

    // Merge resource attributes.
    for (k, v) in resource_attrs {
        doc.insert(k.clone(), v.clone());
    }

    // Instrumentation scope name.
    if !scope_name.is_empty() {
        doc.insert("scope".to_string(), Value::String(scope_name.to_string()));
    }

    // Timestamp (nanoseconds → ISO-8601 string for indexing convenience).
    if let Some(ts_nano) = record
        .get("timeUnixNano")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<u64>().ok())
    {
        let secs = (ts_nano / 1_000_000_000) as i64;
        let nanos = (ts_nano % 1_000_000_000) as u32;
        if let Some(dt) = chrono::DateTime::<chrono::Utc>::from_timestamp(secs, nanos) {
            doc.insert("@timestamp".to_string(), Value::String(dt.to_rfc3339()));
            doc.insert("timestamp_nanos".to_string(), Value::Number(ts_nano.into()));
        }
    } else if let Some(ts_nano) = record.get("timeUnixNano").and_then(Value::as_u64) {
        let secs = (ts_nano / 1_000_000_000) as i64;
        let nanos = (ts_nano % 1_000_000_000) as u32;
        if let Some(dt) = chrono::DateTime::<chrono::Utc>::from_timestamp(secs, nanos) {
            doc.insert("@timestamp".to_string(), Value::String(dt.to_rfc3339()));
            doc.insert("timestamp_nanos".to_string(), Value::Number(ts_nano.into()));
        }
    }

    // Severity.
    if let Some(sev) = record.get("severityText").and_then(Value::as_str) {
        doc.insert("severity".to_string(), Value::String(sev.to_string()));
        // Also store numeric severity number when present.
    }
    if let Some(sev_num) = record.get("severityNumber") {
        doc.insert("severity_number".to_string(), sev_num.clone());
    }

    // Body.
    if let Some(body) = record.get("body") {
        doc.insert("message".to_string(), extract_otlp_any_value(body));
    }

    // Trace / span context.
    if let Some(tid) = record.get("traceId").and_then(Value::as_str) {
        doc.insert("trace_id".to_string(), Value::String(tid.to_string()));
    }
    if let Some(sid) = record.get("spanId").and_then(Value::as_str) {
        doc.insert("span_id".to_string(), Value::String(sid.to_string()));
    }

    // Record-level attributes.
    if let Some(attrs) = record.get("attributes").and_then(Value::as_array) {
        for attr in attrs {
            if let (Some(key), Some(val)) =
                (attr.get("key").and_then(Value::as_str), attr.get("value"))
            {
                doc.insert(key.to_string(), extract_otlp_any_value(val));
            }
        }
    }

    Value::Object(doc)
}

/// Extract a scalar Rust `Value` from an OTLP `AnyValue` object.
///
/// OTLP encodes values as `{"stringValue":"..."}`, `{"intValue":"123"}`,
/// `{"doubleValue":1.5}`, `{"boolValue":true}`, etc.
fn extract_otlp_any_value(any_val: &Value) -> Value {
    if let Some(s) = any_val.get("stringValue").and_then(Value::as_str) {
        return Value::String(s.to_string());
    }
    if let Some(i) = any_val.get("intValue") {
        // OTLP encodes int64 as a JSON string to avoid precision loss.
        if let Some(s) = i.as_str() {
            if let Ok(n) = s.parse::<i64>() {
                return Value::Number(n.into());
            }
        }
        if let Some(n) = i.as_i64() {
            return Value::Number(n.into());
        }
    }
    if let Some(d) = any_val.get("doubleValue").and_then(Value::as_f64) {
        if let Some(n) = serde_json::Number::from_f64(d) {
            return Value::Number(n);
        }
    }
    if let Some(b) = any_val.get("boolValue").and_then(Value::as_bool) {
        return Value::Bool(b);
    }
    if let Some(arr) = any_val
        .get("arrayValue")
        .and_then(|a| a.get("values"))
        .and_then(Value::as_array)
    {
        let items: Vec<Value> = arr.iter().map(extract_otlp_any_value).collect();
        return Value::Array(items);
    }
    // Fallback: return as-is.
    any_val.clone()
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: POST /v1/indices/{name}/syslog — Syslog line ingest
// ─────────────────────────────────────────────────────────────────────────────

/// Ingest syslog lines.
///
/// Accepts a plain-text body where each line is a syslog entry.  Supports:
///
/// * **RFC 5424** — `<priority>version timestamp hostname app-name procid msgid msg`
/// * **RFC 3164 (BSD syslog)** — `<priority>Mon DD HH:MM:SS hostname tag: message`
/// * **Plain text** — lines without a PRI are stored as bare messages.
///
/// Parsed fields: `priority`, `facility`, `severity`, `timestamp`, `hostname`,
/// `app`, `pid`, `message`.
pub async fn ingest_syslog(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let body_bytes = body;

    let idx = match state.engine.get_or_create_index(&name) {
        Ok(i) => i,
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            return native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response();
        }
    };

    let text = String::from_utf8_lossy(&body_bytes);
    let mut total = 0u64;
    let mut indexed = 0u64;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        total += 1;
        let doc = parse_syslog_line(line);
        let id = Uuid::new_v4().to_string();
        match idx.index_document(Some(id), doc).await {
            Ok(_) => {
                indexed += 1;
                state.metrics.record_doc_indexed(&name);
            }
            Err(e) => {
                tracing::warn!(
                    index = name.as_str(),
                    error = %e,
                    "syslog: failed to index line"
                );
            }
        }
    }

    let took_ms = started.elapsed().as_millis() as u64;
    let resp = NativeResponse::new(
        serde_json::json!({
            "index": name,
            "indexed": indexed,
            "total": total,
            "errors": indexed < total,
        }),
        took_ms,
        &request_id,
    );
    (StatusCode::CREATED, Json(resp)).into_response()
}

/// Parse a single syslog line into a flat JSON document.
///
/// Handles RFC 5424, RFC 3164, and bare message fallback.
fn parse_syslog_line(line: &str) -> Value {
    let mut doc = serde_json::Map::new();
    doc.insert("raw".to_string(), Value::String(line.to_string()));
    doc.insert(
        "@timestamp".to_string(),
        Value::String(chrono::Utc::now().to_rfc3339()),
    );

    // Try to strip the PRI field `<NNN>`.
    let (pri_opt, rest) = if line.starts_with('<') {
        if let Some(close) = line.find('>') {
            let pri_str = &line[1..close];
            let rest = &line[close + 1..];
            if let Ok(pri) = pri_str.parse::<u32>() {
                (Some(pri), rest)
            } else {
                (None, line)
            }
        } else {
            (None, line)
        }
    } else {
        (None, line)
    };

    if let Some(pri) = pri_opt {
        let facility = pri >> 3;
        let severity = pri & 0x07;
        doc.insert("priority".to_string(), Value::Number(pri.into()));
        doc.insert("facility".to_string(), Value::Number(facility.into()));
        doc.insert("severity_code".to_string(), Value::Number(severity.into()));
        doc.insert(
            "severity".to_string(),
            Value::String(syslog_severity_name(severity).to_string()),
        );
        doc.insert(
            "facility_name".to_string(),
            Value::String(syslog_facility_name(facility).to_string()),
        );
    }

    // Detect RFC 5424 vs RFC 3164 by checking whether the next field is a
    // version digit (RFC 5424) or a month abbreviation / timestamp (RFC 3164).
    let trimmed = rest.trim();

    // RFC 5424: `<VERSION> <TIMESTAMP> <HOSTNAME> <APP-NAME> <PROCID> <MSGID> <MSG>`
    // Version is a single digit (usually "1").
    if let Some(rest5424) = try_parse_rfc5424(trimmed, &mut doc) {
        doc.insert("message".to_string(), Value::String(rest5424.to_string()));
    } else if let Some(rest3164) = try_parse_rfc3164(trimmed, &mut doc) {
        doc.insert("message".to_string(), Value::String(rest3164.to_string()));
    } else {
        // Plain text fallback.
        doc.insert("message".to_string(), Value::String(trimmed.to_string()));
    }

    Value::Object(doc)
}

/// Attempt RFC 5424 parse: `1 TIMESTAMP HOSTNAME APP PROCID MSGID MSG`.
///
/// Returns the message portion on success, `None` if not RFC 5424.
fn try_parse_rfc5424<'a>(s: &'a str, doc: &mut serde_json::Map<String, Value>) -> Option<&'a str> {
    let mut parts = s.splitn(7, ' ');
    let version = parts.next()?;
    // RFC 5424 version is a small integer (typically "1").
    if version.parse::<u32>().is_err() {
        return None;
    }
    let timestamp = parts.next().unwrap_or("-");
    let hostname = parts.next().unwrap_or("-");
    let app = parts.next().unwrap_or("-");
    let procid = parts.next().unwrap_or("-");
    let _msgid = parts.next().unwrap_or("-");
    let msg = parts.next().unwrap_or("");

    if timestamp != "-" {
        doc.insert(
            "@timestamp".to_string(),
            Value::String(timestamp.to_string()),
        );
    }
    if hostname != "-" {
        doc.insert("hostname".to_string(), Value::String(hostname.to_string()));
    }
    if app != "-" {
        doc.insert("app".to_string(), Value::String(app.to_string()));
    }
    if procid != "-" {
        doc.insert("pid".to_string(), Value::String(procid.to_string()));
    }
    doc.insert(
        "syslog_format".to_string(),
        Value::String("rfc5424".to_string()),
    );

    // Strip optional structured data block `[...]` from the start of msg.
    let msg = msg.trim_start();
    let msg = if msg.starts_with('[') {
        if let Some(end) = msg.find(']') {
            msg[end + 1..].trim()
        } else {
            msg
        }
    } else if let Some(rest) = msg.strip_prefix('-') {
        rest.trim()
    } else {
        msg
    };

    Some(msg)
}

/// Attempt RFC 3164 parse: `Mon DD HH:MM:SS HOSTNAME TAG: MESSAGE`.
///
/// Returns the message portion on success, `None` if not RFC 3164.
fn try_parse_rfc3164<'a>(s: &'a str, doc: &mut serde_json::Map<String, Value>) -> Option<&'a str> {
    // Month abbreviations used in BSD syslog.
    const MONTHS: &[&str] = &[
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    // The first token should be a month abbreviation.
    let first_space = s.find(' ')?;
    let month_str = &s[..first_space];
    if !MONTHS.contains(&month_str) {
        return None;
    }

    let rest = s[first_space..].trim_start();

    // Day (1 or 2 digits, possibly space-padded).
    let day_end = rest.find(' ')?;
    let day_str = rest[..day_end].trim();
    let rest = rest[day_end..].trim_start();

    // HH:MM:SS
    let time_end = rest.find(' ').unwrap_or(rest.len());
    let time_str = &rest[..time_end];
    let rest = if time_end < rest.len() {
        rest[time_end..].trim_start()
    } else {
        ""
    };

    // Build a timestamp string.
    let year = chrono::Utc::now().year();
    let ts = format!("{month_str} {day_str} {time_str} {year}");
    doc.insert("@timestamp".to_string(), Value::String(ts));
    doc.insert(
        "syslog_format".to_string(),
        Value::String("rfc3164".to_string()),
    );

    // HOSTNAME
    let hostname_end = rest.find(' ').unwrap_or(rest.len());
    let hostname = &rest[..hostname_end];
    let rest = if hostname_end < rest.len() {
        rest[hostname_end..].trim_start()
    } else {
        ""
    };
    if !hostname.is_empty() {
        doc.insert("hostname".to_string(), Value::String(hostname.to_string()));
    }

    // TAG (optional, ends at ':' or '[').
    let tag_end = rest.find([':', '[']).unwrap_or(rest.len());
    let tag = &rest[..tag_end];
    let rest = &rest[tag_end..];

    // Extract optional PID from tag[pid].
    if let Some(pid_start) = tag.find('[') {
        let app = &tag[..pid_start];
        if !app.is_empty() {
            doc.insert("app".to_string(), Value::String(app.to_string()));
        }
        if let Some(pid_end) = tag.find(']') {
            let pid = &tag[pid_start + 1..pid_end];
            doc.insert("pid".to_string(), Value::String(pid.to_string()));
        }
    } else if !tag.is_empty() {
        doc.insert("app".to_string(), Value::String(tag.to_string()));
    }

    // Message follows the ':' separator.
    let msg = rest.trim_start_matches(':').trim();
    Some(msg)
}

/// Map a syslog severity code (0–7) to a human-readable name.
fn syslog_severity_name(sev: u32) -> &'static str {
    match sev {
        0 => "emergency",
        1 => "alert",
        2 => "critical",
        3 => "error",
        4 => "warning",
        5 => "notice",
        6 => "informational",
        7 => "debug",
        _ => "unknown",
    }
}

/// Map a syslog facility code (0–23) to a human-readable name.
fn syslog_facility_name(fac: u32) -> &'static str {
    match fac {
        0 => "kern",
        1 => "user",
        2 => "mail",
        3 => "daemon",
        4 => "auth",
        5 => "syslog",
        6 => "lpr",
        7 => "news",
        8 => "uucp",
        9 => "cron",
        10 => "authpriv",
        11 => "ftp",
        16 => "local0",
        17 => "local1",
        18 => "local2",
        19 => "local3",
        20 => "local4",
        21 => "local5",
        22 => "local6",
        23 => "local7",
        _ => "unknown",
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn extract_or_gen_id(doc: &Value) -> String {
    doc.get("_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: POST /v1/indices/{name}/turbo-ingest
// ─────────────────────────────────────────────────────────────────────────────

/// High-throughput batch ingest using the turbo pipeline.
///
/// Accepts a JSON array of documents and processes them using parallel
/// tokenisation and a single batched WAL write.  The turbo pipeline is
/// significantly faster than calling the single-document ingest endpoint
/// in a loop for large batches.
///
/// # Opt-in methods
///
/// 1. **Dedicated endpoint** — `POST /v1/indices/{name}/turbo-ingest`
///    with a JSON array body.
/// 2. **Header on `_bulk`** — add `X-Turbo: true` to a standard
///    `POST /v1/indices/{name}/docs/_bulk` request.
///
/// # Request body
///
/// ```json
/// [
///   { "title": "Document one", "body": "..." },
///   { "_id": "custom-id", "title": "Document two" }
/// ]
/// ```
///
/// # Response
///
/// ```json
/// {
///   "data": {
///     "index": "my-index",
///     "indexed": 1000,
///     "errors": 0,
///     "ids": ["uuid-1", "uuid-2", "..."]
///   },
///   "took_ms": 42,
///   "request_id": "..."
/// }
/// ```
pub async fn turbo_ingest(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(docs): Json<Vec<Value>>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let idx = match state.engine.get_index(&name) {
        Ok(i) => i,
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            return native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response();
        }
    };

    if docs.is_empty() {
        let took_ms = started.elapsed().as_millis() as u64;
        let resp = NativeResponse::new(
            serde_json::json!({
                "index": name,
                "indexed": 0,
                "errors": 0,
                "ids": [],
            }),
            took_ms,
            &request_id,
        );
        return (StatusCode::OK, Json(resp)).into_response();
    }

    // Pull turbo settings from engine config.
    let cfg = state.engine.config();
    let parallel = cfg.indexing.turbo_parallel;
    let fast_analyzer = cfg.indexing.turbo_fast_analyzer;

    // Build `(id, source, source_bytes)` triples, auto-generating IDs
    // where not supplied.  The native API accepts already-parsed
    // `Value` objects so we serialize once here (`to_vec`) to produce
    // the WAL bytes — still one serialize total versus two in the
    // pre-v14 path (one parse + one re-serialize).
    let pairs: Vec<(String, Value, std::sync::Arc<[u8]>)> = docs
        .into_iter()
        .map(|doc| {
            let id = extract_or_gen_id(&doc);
            let bytes: std::sync::Arc<[u8]> = serde_json::to_vec(&doc)
                .map(std::sync::Arc::from)
                .unwrap_or_else(|_| std::sync::Arc::from(&[][..]));
            (id, doc, bytes)
        })
        .collect();

    let total = pairs.len();

    match idx.index_batch_turbo(pairs, parallel, fast_analyzer).await {
        Ok(responses) => {
            let indexed = responses.len();
            let errors = total - indexed;
            let ids: Vec<String> = responses.into_iter().map(|r| r.id).collect();

            for _ in 0..indexed {
                state.metrics.record_doc_indexed(&name);
            }

            let took_ms = started.elapsed().as_millis() as u64;
            let resp = NativeResponse::new(
                serde_json::json!({
                    "index": name,
                    "indexed": indexed,
                    "errors": errors,
                    "ids": ids,
                }),
                took_ms,
                &request_id,
            );
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: GET /v1/indices/{name}/encodings
// ─────────────────────────────────────────────────────────────────────────────

/// Return per-field smart encoding analysis for an index.
///
/// Shows which encoding was automatically selected for each field after
/// 1 000+ samples were observed, along with the estimated compression ratio.
///
/// Fields that have not yet accumulated enough samples are omitted.
pub async fn get_index_encodings(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let idx = match state.engine.get_index(&name) {
        Ok(i) => i,
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            return native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response();
        }
    };

    let stats = idx.stats().await;

    // Sort by compression_ratio descending so the most efficiently encoded
    // fields appear first.
    let mut encodings: Vec<FieldEncodingInfo> = stats.field_encodings;
    encodings.sort_by(|a, b| {
        b.compression_ratio
            .partial_cmp(&a.compression_ratio)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let total_fields = encodings.len();
    let took_ms = started.elapsed().as_millis() as u64;
    let resp = NativeResponse::new(
        serde_json::json!({
            "index": name,
            "analyzed_fields": total_fields,
            "encodings": encodings,
        }),
        took_ms,
        &request_id,
    );
    Json(resp).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: GET /v1/dashboard/summary
//
// First step toward a built-in UI — returns an overview of all indices with
// doc counts, measured on-disk + memtable sizes, per-field encodings, and
// health status.
// This is intentionally designed as a "one-shot" endpoint so a lightweight
// dashboard can render a complete picture with a single HTTP request.
// ─────────────────────────────────────────────────────────────────────────────

/// Per-index summary entry in the dashboard response.
#[derive(Debug, serde::Serialize)]
pub struct DashboardIndexSummary {
    pub name: String,
    pub doc_count: u64,
    /// Real measured size in bytes: sum of on-disk segment file sizes plus the
    /// in-memory memtable byte size (the same figures reported by the
    /// `_segments` API and `IndexStats`).
    pub size_bytes: u64,
    /// Number of distinct fields.
    pub field_count: usize,
    /// Top field encodings (by compression ratio), up to 5.
    pub top_encodings: Vec<FieldEncodingInfo>,
    /// Health colour: "green" (fully flushed), "yellow" (memtable data present).
    pub health: &'static str,
}

pub async fn dashboard_summary(State(state): State<AppState>) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let mut summaries: Vec<DashboardIndexSummary> = Vec::new();
    let mut total_docs = 0u64;
    let mut overall_health = "green";

    let index_list = state.engine.list_indices().await;
    for info in &index_list {
        let idx = match state.engine.get_index(&info.name) {
            Ok(i) => i,
            Err(_) => continue,
        };
        let stats = idx.stats().await;

        let health = if stats.segment_count == 0 && stats.memtable_doc_count > 0 {
            overall_health = "yellow";
            "yellow"
        } else {
            "green"
        };

        // Real measured size: sum of on-disk segment file sizes (the same
        // per-segment `size_bytes` reported by the `_segments` API) plus the
        // in-memory memtable byte size (as reported by `IndexStats`).
        let segment_bytes: u64 = idx
            .store_snapshot()
            .segments
            .iter()
            .map(|s| s.size_bytes)
            .sum();
        let size_bytes = segment_bytes + stats.memtable_size_bytes as u64;

        let mut encodings = stats.field_encodings;
        encodings.sort_by(|a, b| {
            b.compression_ratio
                .partial_cmp(&a.compression_ratio)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        encodings.truncate(5);

        total_docs += stats.doc_count;
        summaries.push(DashboardIndexSummary {
            name: stats.name,
            doc_count: stats.doc_count,
            size_bytes,
            field_count: stats.field_count,
            top_encodings: encodings,
            health,
        });
    }

    // Sort alphabetically so the UI renders consistently.
    summaries.sort_by(|a, b| a.name.cmp(&b.name));

    let took_ms = started.elapsed().as_millis() as u64;
    let resp = NativeResponse::new(
        serde_json::json!({
            "index_count": summaries.len(),
            "total_doc_count": total_docs,
            "overall_health": overall_health,
            "indices": summaries,
        }),
        took_ms,
        &request_id,
    );
    Json(resp).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: POST /v1/indices/{name}/enrich — register an enrich table
// ─────────────────────────────────────────────────────────────────────────────

/// Register (or replace) an enrichment lookup table on an index.
///
/// # Request body
///
/// ```json
/// {
///   "name": "product_meta",
///   "match_field": "sku",
///   "lookup": {
///     "SKU-001": { "category": "electronics", "brand": "Acme" },
///     "SKU-002": { "category": "clothing",    "brand": "Zephyr" }
///   }
/// }
/// ```
///
/// After registration, any document indexed into this index that contains
/// `"sku": "SKU-001"` will automatically have `"category"` and `"brand"` merged
/// in before the document is written to the WAL/FTS memtable.
///
/// # Duplicate keys
/// If a table with the same `name` already exists it is replaced atomically.
pub async fn enrich_index(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let idx = match state.engine.get_index(&name) {
        Ok(i) => i,
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            return native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response();
        }
    };

    // Parse the table name, match_field, and lookup map.
    let table_name = match body.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => {
            return native_error(
                xerj_common::XerjError::invalid_query("enrich request must include a 'name' field"),
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response();
        }
    };

    let match_field = match body.get("match_field").and_then(|v| v.as_str()) {
        Some(f) => f.to_string(),
        None => {
            return native_error(
                xerj_common::XerjError::invalid_query(
                    "enrich request must include a 'match_field' field",
                ),
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response();
        }
    };

    let lookup_raw = match body.get("lookup").and_then(|v| v.as_object()) {
        Some(o) => o.clone(),
        None => {
            return native_error(
                xerj_common::XerjError::invalid_query(
                    "enrich request must include a 'lookup' object",
                ),
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response();
        }
    };

    let lookup: std::collections::HashMap<String, serde_json::Value> =
        lookup_raw.into_iter().collect();

    let entry_count = lookup.len();
    let table = EnrichTable {
        name: table_name.clone(),
        match_field,
        lookup,
    };

    {
        let mut enrichments = idx.enrichments.write().await;
        enrichments.insert(table_name.clone(), table);
    }

    let took_ms = started.elapsed().as_millis() as u64;
    let resp = NativeResponse::new(
        serde_json::json!({
            "index": name,
            "enrich_table": table_name,
            "entries": entry_count,
            "acknowledged": true,
        }),
        took_ms,
        &request_id,
    );
    (StatusCode::CREATED, Json(resp)).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: GET /v1/indices/{name}/explain-plan — return query execution plan
// ─────────────────────────────────────────────────────────────────────────────

/// Return the query execution plan for a query WITHOUT executing it.
///
/// Shows:
/// - Which engine will be used: FTS, vector, or doc-scan
/// - Estimated cost (relative)
/// - Fields accessed
///
/// # Request body
/// Same as `_search` query body.
pub async fn explain_plan(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    // Ensure the index exists.
    let idx = match state.engine.get_index(&name) {
        Ok(i) => i,
        Err(e) => {
            let xerj_err: xerj_common::XerjError = e.into();
            return native_error(
                xerj_err,
                Some(&request_id),
                started.elapsed().as_millis() as u64,
            )
            .into_response();
        }
    };

    let body_val: serde_json::Value = if body.is_empty() {
        serde_json::json!({ "query": { "match_all": {} } })
    } else {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                return native_error(
                    xerj_common::XerjError::invalid_query(format!("invalid JSON: {e}")),
                    Some(&request_id),
                    started.elapsed().as_millis() as u64,
                )
                .into_response();
            }
        }
    };

    // Parse the query node.
    let search_req = match xerj_query::parse_request(&body_val)
        .map_err(|e| xerj_common::XerjError::invalid_query(e.to_string()))
    {
        Ok(r) => r,
        Err(e) => {
            return native_error(e, Some(&request_id), started.elapsed().as_millis() as u64)
                .into_response();
        }
    };

    let stats = idx.stats().await;
    let doc_count = stats.doc_count;

    // Analyse the query to produce an explain plan.
    let plan = build_explain_plan(&search_req.query, doc_count);

    let took_ms = started.elapsed().as_millis() as u64;
    let resp = NativeResponse::new(
        serde_json::json!({
            "index": name,
            "doc_count": doc_count,
            "plan": plan,
        }),
        took_ms,
        &request_id,
    );
    Json(resp).into_response()
}

/// Build a human-readable explain plan for a query node.
fn build_explain_plan(query: &xerj_query::ast::QueryNode, doc_count: u64) -> serde_json::Value {
    use xerj_query::ast::QueryNode;

    match query {
        QueryNode::MatchAll => serde_json::json!({
            "engine": "doc-scan",
            "description": "match_all — full collection scan",
            "estimated_docs_scanned": doc_count,
            "estimated_cost": "O(N)",
            "fields_accessed": [],
        }),
        QueryNode::MatchNone => serde_json::json!({
            "engine": "none",
            "description": "match_none — returns immediately with zero results",
            "estimated_docs_scanned": 0,
            "estimated_cost": "O(1)",
            "fields_accessed": [],
        }),
        QueryNode::Match {
            field, query: q, ..
        } => serde_json::json!({
            "engine": "fts",
            "description": format!("match query on field '{field}' for '{q}'"),
            "estimated_docs_scanned": doc_count / 10,
            "estimated_cost": "O(log N + k)",
            "fields_accessed": [field],
        }),
        QueryNode::Term { field, .. } => serde_json::json!({
            "engine": "fts",
            "description": format!("term filter on field '{field}'"),
            "estimated_docs_scanned": doc_count / 20,
            "estimated_cost": "O(log N + k)",
            "fields_accessed": [field],
        }),
        QueryNode::Range { field, .. } => serde_json::json!({
            "engine": "doc-scan",
            "description": format!("range filter on field '{field}'"),
            "estimated_docs_scanned": doc_count / 4,
            "estimated_cost": "O(N)",
            "fields_accessed": [field],
        }),
        QueryNode::Knn { field, k, .. } => serde_json::json!({
            "engine": "vector",
            "description": format!("k-NN vector search on field '{field}', k={k}"),
            "estimated_docs_scanned": k,
            "estimated_cost": "O(log N)",
            "fields_accessed": [field],
        }),
        QueryNode::Bool {
            must,
            should,
            filter,
            must_not,
            ..
        } => {
            let sub_plans: Vec<_> = must
                .iter()
                .chain(should.iter())
                .chain(filter.iter())
                .chain(must_not.iter())
                .map(|c| build_explain_plan(c, doc_count))
                .collect();
            serde_json::json!({
                "engine": "bool",
                "description": format!(
                    "bool query: {} must, {} should, {} filter, {} must_not",
                    must.len(), should.len(), filter.len(), must_not.len()
                ),
                "estimated_docs_scanned": doc_count / 5,
                "estimated_cost": "O(log N + k)",
                "clauses": sub_plans,
            })
        }
        _ => serde_json::json!({
            "engine": "doc-scan",
            "description": "generic query — doc-scan with post-filter",
            "estimated_docs_scanned": doc_count,
            "estimated_cost": "O(N)",
            "fields_accessed": [],
        }),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: PUT /v1/pipelines/{name}  — create / replace a transform pipeline
// ─────────────────────────────────────────────────────────────────────────────

/// Create (or replace) a named transform pipeline from a JSON
/// [`PipelineConfig`](xerj_wasm::pipeline::PipelineConfig) body.
///
/// Example request body:
/// ```json
/// {
///   "description": "Enrich nginx logs",
///   "stages": [
///     { "type": "grok",        "config": { "field": "message", "pattern": "NGINX_COMBINED" } },
///     { "type": "timestamp_parse", "config": { "field": "time_local" } },
///     { "type": "drop_field",  "config": { "fields": ["message"] } },
///     { "type": "add_field",   "config": { "field": "log_type", "value": "nginx" } }
///   ]
/// }
/// ```
pub async fn put_pipeline(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    match state.engine.create_pipeline(&name, body) {
        Ok(()) => {
            let took_ms = started.elapsed().as_millis() as u64;
            let resp = NativeResponse::new(
                serde_json::json!({ "pipeline": name, "acknowledged": true }),
                took_ms,
                &request_id,
            );
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => {
            let ze = xerj_common::XerjError::internal(e.to_string());
            native_error(ze, Some(&request_id), started.elapsed().as_millis() as u64)
                .into_response()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler: POST /v1/indices/{name}/ingest?pipeline={pipeline}
// ─────────────────────────────────────────────────────────────────────────────

/// Query parameters for the pipeline-ingest endpoint.
#[derive(Debug, Deserialize)]
pub struct IngestWithPipelineParams {
    pub pipeline: String,
}

/// Ingest one or more documents through a named transform pipeline before
/// indexing.
///
/// Documents with [`ProcessAction::Drop`] are silently discarded.
/// Documents with [`ProcessAction::Route(target)`] are indexed into `target`
/// instead of `{name}`.
pub async fn ingest_with_pipeline(
    State(state): State<AppState>,
    Path(name): Path<String>,
    axum::extract::Query(params): axum::extract::Query<IngestWithPipelineParams>,
    Json(req): Json<IngestRequest>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    let docs: Vec<Value> = match req {
        IngestRequest::Single(doc) => vec![doc],
        IngestRequest::Batch(docs) => docs,
    };

    let processed = match state
        .engine
        .process_through_pipeline(&params.pipeline, docs)
    {
        Ok(p) => p,
        Err(e) => {
            let ze = xerj_common::XerjError::internal(e.to_string());
            return native_error(ze, Some(&request_id), started.elapsed().as_millis() as u64)
                .into_response();
        }
    };

    let mut indexed = 0u64;
    let mut dropped = 0u64;
    let mut routed = 0u64;
    let mut errors = 0u64;

    for (action, doc) in processed {
        use xerj_wasm::pipeline::ProcessAction;
        let target_name = match action {
            ProcessAction::Drop => {
                dropped += 1;
                continue;
            }
            ProcessAction::Route(ref target) => {
                routed += 1;
                target.clone()
            }
            ProcessAction::Pass => name.clone(),
        };

        let idx = match state.engine.get_or_create_index(&target_name) {
            Ok(i) => i,
            Err(_) => {
                errors += 1;
                continue;
            }
        };

        let doc_id = extract_or_gen_id(&doc);
        match idx.index_document(Some(doc_id), doc).await {
            Ok(_) => {
                indexed += 1;
                state.metrics.record_doc_indexed(&target_name);
            }
            Err(_) => {
                errors += 1;
            }
        }
    }

    let took_ms = started.elapsed().as_millis() as u64;
    let resp = NativeResponse::new(
        serde_json::json!({
            "index": name,
            "pipeline": params.pipeline,
            "indexed": indexed,
            "dropped": dropped,
            "routed": routed,
            "errors": errors,
        }),
        took_ms,
        &request_id,
    );
    (StatusCode::CREATED, Json(resp)).into_response()
}
