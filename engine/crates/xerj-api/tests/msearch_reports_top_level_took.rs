//! Issue #1288: an ES 8.x `_msearch` response carries a top-level `took`
//! (wall time of the whole multi-search, in ms) next to `responses`. XERJ
//! returned only `responses`, so a dashboard reading request time from it got
//! `undefined`. `_msearch/template` already emitted it; this pins `_msearch`
//! on both its routes.
//!
//! Elasticsearch is referenced for wire semantics only; no ES code is
//! reproduced here.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    ctype: &str,
    body: String,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if !body.is_empty() {
        req = req.header("content-type", ctype);
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn msearch_response_has_top_level_took() {
    let (app, _dir) = app().await;
    let (st, _) = send(
        &app,
        "PUT",
        "/repro",
        "application/json",
        json!({"mappings": {"properties": {"host": {"type": "keyword"}}}}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create index");
    let (st, _) = send(
        &app,
        "PUT",
        "/repro/_doc/1?refresh=true",
        "application/json",
        json!({"host": "a"}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "index doc");

    // One good item and one per-item error: `took` belongs to the envelope
    // whatever the items carry.
    let body = "{\"index\":\"repro\"}\n{\"size\":0,\"query\":{\"match_all\":{}}}\n\
                {\"index\":\"missing-index\"}\n{\"size\":0}\n"
        .to_string();
    for path in ["/_msearch", "/repro/_msearch"] {
        let (st, resp) = send(&app, "POST", path, "application/x-ndjson", body.clone()).await;
        assert_eq!(st, StatusCode::OK, "{path}: {resp}");
        assert!(
            resp.get("took").map(Value::is_u64).unwrap_or(false),
            "{path}: top-level `took` must be a non-negative integer, got: {resp}"
        );
        let items = resp["responses"].as_array().expect("responses array");
        assert_eq!(items.len(), 2, "{path}: {resp}");
        assert_eq!(items[0]["status"], 200, "{path}: {resp}");
        assert!(
            items[0]["took"].is_u64(),
            "{path}: per-item took kept: {resp}"
        );
    }
}
