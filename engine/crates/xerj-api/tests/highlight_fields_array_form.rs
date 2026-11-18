//! Issue #1946: `highlight.fields` given as an array of single-field objects
//! (the ES form that fixes field order) was silently ignored: no `highlight`
//! in the hits and no error. Only the object form worked.
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

async fn seeded() -> (axum::Router, tempfile::TempDir) {
    let (app, dir) = app().await;
    let (st, resp) = send(
        &app,
        "PUT",
        "/hl",
        "application/json",
        json!({"mappings": {"properties": {"body": {"type": "text"}}}}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create: {resp}");
    let (st, resp) = send(
        &app,
        "PUT",
        "/hl/_doc/1?refresh=true",
        "application/json",
        json!({"body": "Pricing is per-node for Professional."}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "index: {resp}");
    (app, dir)
}

#[tokio::test]
async fn highlight_fields_array_form_matches_the_object_form() {
    let (app, _dir) = seeded().await;
    let object_form = json!({
        "query": {"match": {"body": "pricing"}},
        "highlight": {"fields": {"body": {"pre_tags": ["["], "post_tags": ["]"]}}}
    });
    let array_form = json!({
        "query": {"match": {"body": "pricing"}},
        "highlight": {"fields": [{"body": {"pre_tags": ["["], "post_tags": ["]"]}}]}
    });
    let (st, want) = send(
        &app,
        "POST",
        "/hl/_search",
        "application/json",
        object_form.to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{want}");
    let (st, got) = send(
        &app,
        "POST",
        "/hl/_search",
        "application/json",
        array_form.to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{got}");
    assert_eq!(
        got["hits"]["hits"][0]["highlight"],
        json!({"body": ["[Pricing] is per-node for Professional."]}),
        "{got}"
    );
    assert_eq!(
        got["hits"]["hits"][0]["highlight"],
        want["hits"]["hits"][0]["highlight"]
    );
}
