//! `PUT /_bulk` and `PUT /{index}/_bulk` write, as in Elasticsearch 8 (#3036).
//!
//! The rest-api-spec lists both POST and PUT for bulk, and the official Python
//! client 8.x sends PUT from `helpers.bulk`. XERJ registered POST only, so that
//! client got 405 Method Not Allowed and wrote nothing.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

async fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn send(app: &axum::Router, method: &str, path: &str, body: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/x-ndjson")
        .body(Body::from(body.to_owned()))
        .expect("request");
    let response = app.clone().oneshot(req).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn doc_exists(app: &axum::Router, index: &str, id: &str) -> bool {
    let (status, _) = send(app, "GET", &format!("/{index}/_doc/{id}"), "").await;
    status == StatusCode::OK
}

fn assert_written(status: StatusCode, got: &Value) {
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["errors"], false, "{got}");
}

#[tokio::test]
async fn put_global_bulk_writes() {
    let (app, _dir) = app().await;
    let body = "{\"index\":{\"_index\":\"t\",\"_id\":\"1\"}}\n{\"f\":\"v\"}\n";
    let (status, got) = send(&app, "PUT", "/_bulk?refresh=true", body).await;
    assert_written(status, &got);
    assert!(doc_exists(&app, "t", "1").await);
}

#[tokio::test]
async fn put_index_bulk_writes() {
    let (app, _dir) = app().await;
    let body = "{\"index\":{\"_id\":\"1\"}}\n{\"f\":\"v\"}\n";
    let (status, got) = send(&app, "PUT", "/t/_bulk?refresh=true", body).await;
    assert_written(status, &got);
    assert!(doc_exists(&app, "t", "1").await);
}

#[tokio::test]
async fn post_bulk_still_writes() {
    let (app, _dir) = app().await;
    let body = "{\"index\":{\"_index\":\"t\",\"_id\":\"1\"}}\n{\"f\":\"v\"}\n";
    let (status, got) = send(&app, "POST", "/_bulk?refresh=true", body).await;
    assert_written(status, &got);
    let body = "{\"index\":{\"_id\":\"2\"}}\n{\"f\":\"v\"}\n";
    let (status, got) = send(&app, "POST", "/t/_bulk?refresh=true", body).await;
    assert_written(status, &got);
    assert!(doc_exists(&app, "t", "1").await);
    assert!(doc_exists(&app, "t", "2").await);
}
