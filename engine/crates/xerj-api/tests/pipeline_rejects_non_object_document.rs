//! Issue #3174: an ingest pipeline containing `set` (or `add_field`)
//! panicked on a non-object document root (`serde_json` `IndexMut`
//! panics), and the release profile aborts on panic, so one malformed
//! document killed the whole node instead of returning a 400.
//!
//! The guard lives in `Engine::process_through_pipeline`, so every route
//! that runs a pipeline is covered: `POST /{index}/_doc`,
//! `PUT /{index}/_doc/{id}`, `PUT /{index}/_create/{id}`, and the `_bulk`
//! create branch (per-item 400). Each test also indexes a valid object
//! through the same pipeline afterwards, proving the node stayed alive.

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

async fn send(app: &axum::Router, method: &str, path: &str, body: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .expect("request");
    let res = app.clone().oneshot(req).await.expect("response");
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn send_ndjson(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: &str,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/x-ndjson")
        .body(Body::from(body.to_owned()))
        .expect("request");
    let res = app.clone().oneshot(req).await.expect("response");
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn define_set_pipeline(app: &axum::Router) {
    let (status, body) = send(
        app,
        "PUT",
        "/_ingest/pipeline/set-pipe",
        &json!({ "processors": [{ "set": { "field": "tagged", "value": true } }] }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "pipeline stores: {body}");
}

async fn get_doc(app: &axum::Router, index: &str, id: &str) -> (StatusCode, Value) {
    send(app, "GET", &format!("/{index}/_doc/{id}"), "").await
}

#[tokio::test]
async fn post_doc_with_array_body_and_pipeline_returns_400() {
    let (app, _dir) = app().await;
    define_set_pipeline(&app).await;

    let (status, body) = send(&app, "POST", "/t/_doc?pipeline=set-pipe", "[1, 2, 3]").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("JSON object"),
        "the refusal must name the required shape: {body}"
    );

    // The node is still alive: the same pipeline accepts an object doc.
    let (status, body) = send(&app, "POST", "/t/_doc?pipeline=set-pipe", r#"{"f": "v"}"#).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["_id"].as_str().expect("created doc has an id");
    let (status, got) = get_doc(&app, "t", id).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["_source"]["tagged"], true, "the set stage ran: {got}");
}

#[tokio::test]
async fn put_doc_with_array_body_and_pipeline_returns_400() {
    let (app, _dir) = app().await;
    define_set_pipeline(&app).await;

    let (status, body) = send(&app, "PUT", "/t/_doc/1?pipeline=set-pipe", "[1, 2, 3]").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = send(&app, "PUT", "/t/_doc/1?pipeline=set-pipe", r#"{"f": "v"}"#).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, got) = get_doc(&app, "t", "1").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["_source"]["tagged"], true, "the set stage ran: {got}");
}

#[tokio::test]
async fn create_doc_with_array_body_and_pipeline_returns_400() {
    let (app, _dir) = app().await;
    define_set_pipeline(&app).await;

    let (status, body) = send(
        &app,
        "PUT",
        "/t/_create/1?pipeline=set-pipe",
        r#""just a string""#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = send(
        &app,
        "PUT",
        "/t/_create/1?pipeline=set-pipe",
        r#"{"f": "v"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, got) = get_doc(&app, "t", "1").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["_source"]["tagged"], true, "the set stage ran: {got}");
}

#[tokio::test]
async fn bulk_create_with_array_body_returns_per_item_400() {
    let (app, _dir) = app().await;
    define_set_pipeline(&app).await;

    let body = concat!(
        "{\"create\": {\"_index\": \"t\", \"_id\": \"bad\", \"pipeline\": \"set-pipe\"}}\n",
        "[1, 2, 3]\n",
        "{\"create\": {\"_index\": \"t\", \"_id\": \"good\", \"pipeline\": \"set-pipe\"}}\n",
        "{\"f\": \"v\"}\n",
    );
    let (status, got) = send_ndjson(&app, "POST", "/_bulk", body).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["errors"], true, "{got}");
    assert_eq!(got["items"][0]["create"]["status"], 400, "{got}");
    assert!(
        got["items"][0]["create"]["error"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("JSON object"),
        "the per-item error must name the required shape: {got}"
    );
    assert_eq!(got["items"][1]["create"]["status"], 201, "{got}");

    // The sibling still indexed, through the same pipeline.
    let (status, doc) = get_doc(&app, "t", "good").await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["_source"]["tagged"], true, "the set stage ran: {doc}");
    let (status, _) = get_doc(&app, "t", "bad").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the bad item must not be indexed"
    );
}
