//! Live database sources (`xerj connect`), the node side.
//!
//! ```text
//! GET    /_xerj/sources          every source with its status
//! GET    /_xerj/sources/{name}   one source: spec (no secret) + status
//! PUT    /_xerj/sources/{name}   create or replace; starts streaming
//! DELETE /_xerj/sources/{name}   stop, drop the replication slot (and the
//!                                publication XERJ created);
//!                                ?delete_indices=true also deletes the indices
//! ```
//!
//! Superuser only, by the `/_xerj/*` rule in [`crate::authz`]: a source reads
//! a whole database with credentials the node holds, the same class of power
//! as the WAL tap and snapshots.
//!
//! The worker runs on its own thread (`xerj_sources::Worker`) and writes
//! through [`ApiSink`], which calls the same in-process `_bulk` path an HTTP
//! client would, so mapping, pipelines and validation are the ones every
//! other write gets. Specs persist in `<data_dir>/_sources/`; [`start`]
//! resumes them at boot.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use xerj_sources::{Op, Sink, SourceSpec, Store, Worker};

use crate::state::AppState;

struct Manager {
    data_dir: std::path::PathBuf,
    sink: Arc<ApiSink>,
    workers: Mutex<BTreeMap<String, Worker>>,
}

static MANAGER: OnceLock<Manager> = OnceLock::new();

struct ApiSink {
    state: AppState,
    rt: tokio::runtime::Handle,
}

impl Sink for ApiSink {
    fn apply(&self, ops: &[Op]) -> anyhow::Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        let body = bytes::Bytes::from(xerj_sources::change::bulk_body(ops));
        let state = self.state.clone();
        let resp = self.rt.block_on(async move {
            let r = crate::es_compat::process_bulk_body(
                &state,
                None,
                &body,
                Instant::now(),
                Default::default(),
                false,
            )
            .await;
            let status = r.status();
            let b = axum::body::to_bytes(r.into_body(), 512 * 1024 * 1024)
                .await
                .unwrap_or_default();
            (status, b)
        });
        let (status, b) = resp;
        let v: Value = serde_json::from_slice(&b).unwrap_or(Value::Null);
        if !status.is_success() {
            anyhow::bail!("bulk write refused ({status}): {v}");
        }
        if let Some(f) = xerj_sources::change::bulk_failure(&v) {
            anyhow::bail!("bulk write failed: {f}");
        }
        Ok(())
    }

    fn clear_index(&self, index: &str) -> anyhow::Result<()> {
        let engine = self.state.engine.clone();
        let name = index.to_string();
        let exists = engine.get_index(&name).is_ok();
        if exists {
            self.rt
                .block_on(async move { engine.delete_index(&name).await })?;
        }
        Ok(())
    }
}

fn data_dir(state: &AppState) -> std::path::PathBuf {
    std::path::PathBuf::from(&state.config.server.data_dir)
}

fn manager(state: &AppState) -> &'static Manager {
    MANAGER.get_or_init(|| Manager {
        data_dir: data_dir(state),
        sink: Arc::new(ApiSink {
            state: state.clone(),
            rt: tokio::runtime::Handle::current(),
        }),
        workers: Mutex::new(BTreeMap::new()),
    })
}

/// Resume every stored source. Called once at boot, inside the runtime.
pub fn start(state: &AppState) {
    let m = manager(state);
    for name in Store::list(&m.data_dir) {
        let store = Store::new(&m.data_dir, &name);
        match store.load_spec().and_then(|s| s.validate().map(|_| s)) {
            Ok(spec) => {
                tracing::info!("source {name}: resuming ({})", spec.display_url());
                let w = Worker::spawn(spec, store, m.sink.clone(), None);
                m.workers.lock().unwrap().insert(name, w);
            }
            Err(e) => tracing::warn!("source {name}: not started: {e:#}"),
        }
    }
}

fn err(status: StatusCode, kind: &str, reason: impl Into<String>) -> Response {
    (
        status,
        Json(json!({"error": {"type": kind, "reason": reason.into()}, "status": status.as_u16()})),
    )
        .into_response()
}

fn body_for(spec: &SourceSpec, status: Option<xerj_sources::Status>) -> Value {
    json!({
        "name": spec.name,
        "url": spec.display_url(),
        "password": if spec.password_env.is_some() { json!({"env": spec.password_env}) }
                    else if spec.password_file.is_some() { json!({"file": spec.password_file}) }
                    else { Value::Null },
        "tables": spec.tables,
        "exclude_tables": spec.exclude_tables,
        "index_prefix": spec.index_prefix(),
        "mode": spec.mode,
        "publication": spec.publication_name(),
        "slot": spec.slot_name(),
        "watermark_column": spec.watermark_column,
        "facts_namespace": spec.facts_namespace,
        "status": status.map(|s| serde_json::to_value(s).unwrap_or(Value::Null)),
    })
}

/// `GET /_xerj/sources`
pub async fn list_sources(State(state): State<AppState>) -> Response {
    let m = manager(&state);
    let w = m.workers.lock().unwrap();
    let sources: Vec<Value> = w
        .values()
        .map(|w| body_for(&w.spec, Some(w.status())))
        .collect();
    Json(json!({ "sources": sources })).into_response()
}

/// `GET /_xerj/sources/{name}`
pub async fn get_source(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let m = manager(&state);
    let w = m.workers.lock().unwrap();
    match w.get(&name) {
        Some(w) => Json(body_for(&w.spec, Some(w.status()))).into_response(),
        None => err(
            StatusCode::NOT_FOUND,
            "resource_not_found_exception",
            format!("no source [{name}]. `xerj sources` lists them; `xerj connect` adds one"),
        ),
    }
}

/// `PUT /_xerj/sources/{name}`
pub async fn put_source(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(mut body): Json<Value>,
) -> Response {
    if let Some(o) = body.as_object_mut() {
        match o.get("name").and_then(Value::as_str) {
            Some(n) if n != name => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "illegal_argument_exception",
                    format!("body name {n:?} differs from the path name {name:?}"),
                )
            }
            _ => {
                o.insert("name".into(), json!(name));
            }
        }
    }
    let spec: SourceSpec = match serde_json::from_value(body) {
        Ok(s) => s,
        Err(e) => return err(StatusCode::BAD_REQUEST, "parse_exception", e.to_string()),
    };
    if let Err(e) = spec.validate() {
        return err(
            StatusCode::BAD_REQUEST,
            "illegal_argument_exception",
            format!("{e:#}"),
        );
    }
    let m = manager(&state);
    let store = Store::new(&m.data_dir, &name);
    let previous = m.workers.lock().unwrap().remove(&name);
    let replaced = previous.is_some();
    if let Some(prev) = previous {
        let changed_source = prev.spec.url != spec.url
            || prev.spec.tables != spec.tables
            || prev.spec.exclude_tables != spec.exclude_tables
            || prev.spec.index_prefix != spec.index_prefix
            || prev.spec.mode != spec.mode;
        tokio::task::spawn_blocking(move || prev.stop(Duration::from_secs(15)))
            .await
            .ok();
        if changed_source {
            // A different database, table set or index layout: start over
            // with a fresh snapshot rather than resuming a stream that no
            // longer describes the same data.
            let mut st = store.load_state();
            st.phase = xerj_sources::Phase::Snapshot;
            st.lsn = 0;
            st.watermarks.clear();
            let _ = store.save_state(&st);
        }
    }
    if let Err(e) = store.save_spec(&spec) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "exception",
            format!("cannot store the source: {e:#}"),
        );
    }
    let w = Worker::spawn(spec.clone(), store, m.sink.clone(), None);
    let status = w.status();
    m.workers.lock().unwrap().insert(name.clone(), w);
    let code = if replaced {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    let mut b = body_for(&spec, Some(status));
    b["result"] = json!(if replaced { "updated" } else { "created" });
    (code, Json(b)).into_response()
}

/// `DELETE /_xerj/sources/{name}`
pub async fn delete_source(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let m = manager(&state);
    let Some(w) = m.workers.lock().unwrap().remove(&name) else {
        return err(
            StatusCode::NOT_FOUND,
            "resource_not_found_exception",
            format!("no source [{name}]"),
        );
    };
    let spec = w.spec.clone();
    let tables: Vec<String> = w.status().tables.iter().map(|t| t.index.clone()).collect();
    let store = Store::new(&m.data_dir, &name);
    let stopped = tokio::task::spawn_blocking(move || w.stop(Duration::from_secs(15)))
        .await
        .unwrap_or(false);
    let st = store.load_state();
    let spec2 = spec.clone();
    let cleanup =
        tokio::task::spawn_blocking(move || xerj_sources::pg::connector::cleanup(&spec2, &st))
            .await;
    let mut done: Vec<String> = Vec::new();
    let mut warning: Option<String> = None;
    match cleanup {
        Ok(Ok(d)) => done = d,
        Ok(Err(e)) => {
            warning = Some(format!(
                "could not drop the replication slot {} on the database ({e:#}). An abandoned slot keeps WAL on the server forever: drop it with SELECT pg_drop_replication_slot('{}');",
                spec.slot_name(),
                spec.slot_name()
            ))
        }
        Err(e) => warning = Some(format!("cleanup did not run: {e}")),
    }
    if q.get("delete_indices")
        .map(|v| v == "true")
        .unwrap_or(false)
    {
        for idx in tables {
            if state.engine.get_index(&idx).is_ok() && state.engine.delete_index(&idx).await.is_ok()
            {
                done.push(format!("deleted index {idx}"));
            }
        }
    }
    let _ = store.remove();
    Json(json!({
        "acknowledged": true,
        "stopped_cleanly": stopped,
        "cleanup": done,
        "warning": warning,
    }))
    .into_response()
}
