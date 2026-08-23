//! Issue #463: `POST /{index}/_close` marks an index unqueryable but frees ~no
//! memory — `close_index` only sets the `closed_indices` flag; the `Arc<Index>`
//! (memtable + per-segment caches + hydration budget) stays in `engine.indices`.
//!
//! This probe establishes the fail-before observable: after `_close`, the global
//! `segment_hydration.current_in_bytes` gauge does NOT drop (memory retained),
//! and a subsequent `_open` + query must still return every doc (the fix must
//! flush before releasing, so no data is lost).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;
use xerj_common::types::{FieldConfig, FieldType, Schema};

async fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);

    let mut schema = Schema::empty();
    schema
        .add_field(FieldConfig::new("body", FieldType::Text))
        .expect("body");
    schema
        .add_field(FieldConfig::new("n", FieldType::Long))
        .expect("n");
    state.engine.create_index("docs", schema).expect("create");
    let idx = state.engine.get_index("docs").expect("get");
    for i in 0..200 {
        idx.index_document(
            Some(format!("d{i}")),
            json!({"body": format!("document number {i} with some searchable prose content to hydrate"), "n": i}),
        )
        .await
        .expect("index");
    }
    idx.refresh().await.expect("refresh");
    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn call(app: &axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    let body = if body.is_null() {
        Body::empty()
    } else {
        req = req.header("content-type", "application/json");
        Body::from(body.to_string())
    };
    let response = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Recursively find the first `segment_hydration.current_in_bytes` in the stats.
fn hydration_current(stats: &Value) -> Option<u64> {
    fn walk(v: &Value) -> Option<u64> {
        if let Some(obj) = v.as_object() {
            if let Some(h) = obj.get("segment_hydration") {
                if let Some(c) = h.get("current_in_bytes").and_then(Value::as_u64) {
                    return Some(c);
                }
            }
            for (_, child) in obj {
                if let Some(c) = walk(child) {
                    return Some(c);
                }
            }
        } else if let Some(arr) = v.as_array() {
            for child in arr {
                if let Some(c) = walk(child) {
                    return Some(c);
                }
            }
        }
        None
    }
    walk(stats)
}

#[tokio::test]
async fn probe_close_hydration_and_reopen_roundtrip() {
    let (app, _dir) = app().await;

    // Hydrate: run a query that reads segments.
    let (st, _) = call(
        &app,
        "POST",
        "/docs/_search",
        json!({"query": {"match": {"body": "document"}}, "size": 50}),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (_, before) = call(&app, "GET", "/_nodes/stats", Value::Null).await;
    let hyd_before = hydration_current(&before);
    eprintln!("PROBE hydration current_in_bytes BEFORE close = {hyd_before:?}");

    // Close.
    let (st, cbody) = call(&app, "POST", "/docs/_close", Value::Null).await;
    eprintln!("PROBE close status={st} body={cbody}");
    assert_eq!(st, StatusCode::OK, "close: {cbody}");

    let (_, after) = call(&app, "GET", "/_nodes/stats", Value::Null).await;
    let hyd_after = hydration_current(&after);
    eprintln!("PROBE hydration current_in_bytes AFTER close = {hyd_after:?}");

    // Reopen + query must return docs (lossless) — this must hold before AND
    // after the fix (the fix must flush before releasing).
    let (st, _open) = call(&app, "POST", "/docs/_open", Value::Null).await;
    eprintln!("PROBE open status={st}");
    let (st, q) = call(
        &app,
        "POST",
        "/docs/_search",
        json!({"query": {"match_all": {}}, "size": 0}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "post-open query: {q}");
    let total = q.pointer("/hits/total/value").and_then(Value::as_u64);
    eprintln!("PROBE post-open total hits = {total:?}");
    assert_eq!(total, Some(200), "reopen must be lossless: {q}");
}

/// #463 (+#206 visibility guarantee): a closed index whose in-memory handle was
/// released must still appear in `_cat/indices` with status `close` — omitting
/// it is the exact invisibility regression #206 fixed for failed indices. This
/// is the fail-before for the release change (before the cat_indices fix the row
/// vanishes because the released index is absent from the loaded-only listing).
#[tokio::test]
async fn closed_index_stays_visible_in_cat_indices() {
    let (app, _dir) = app().await;

    let (st, before) = call(&app, "GET", "/_cat/indices?format=json", Value::Null).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        before
            .as_array()
            .is_some_and(|a| a.iter().any(|r| r["index"] == "docs")),
        "docs should be listed before close: {before}"
    );

    let (st, _) = call(&app, "POST", "/docs/_close", Value::Null).await;
    assert_eq!(st, StatusCode::OK);

    let (st, after) = call(&app, "GET", "/_cat/indices?format=json", Value::Null).await;
    assert_eq!(st, StatusCode::OK);
    let row = after
        .as_array()
        .and_then(|a| a.iter().find(|r| r["index"] == "docs"));
    assert!(
        row.is_some(),
        "#463/#206: a closed index must stay visible in _cat/indices, not vanish: {after}"
    );
    assert_eq!(
        row.unwrap()["status"],
        "close",
        "#463: the closed index must report status close: {after}"
    );

    // And a concrete-name lookup of the closed index must not 404.
    let (st, _) = call(&app, "GET", "/_cat/indices/docs?format=json", Value::Null).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "a concrete _cat lookup of a closed index must not 404"
    );
}
