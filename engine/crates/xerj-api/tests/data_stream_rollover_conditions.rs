//! `POST /{data_stream}/_rollover` must honour its request body.
//!
//! The handler used to ignore the body completely: `dry_run: true` created a
//! backing index, `conditions` never blocked a rollover, and `old_index`
//! reported the data-stream name instead of the current backing index. A caller
//! asking "would this roll, and why?" got a real mutation and a misleading
//! answer.
//!
//! This pins the ES behavior for the conditions this build can evaluate:
//! `max_docs` and `max_age` are ORed and reported in a `conditions` map; the
//! candidate old/new backing index names are returned even when no roll
//! happens; a dry run and an unmet condition never create an index; and
//! unsupported conditions (`max_size`, ...) or alias-rollover body fields are
//! refused instead of silently ignored.
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

async fn create_stream(app: &axum::Router, name: &str) {
    let (status, body) = call(app, "PUT", &format!("/_data_stream/{name}"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn write_doc(app: &axum::Router, stream: &str, id: &str) {
    let (status, body) = call(
        app,
        "PUT",
        &format!("/{stream}/_doc/{id}"),
        json!({"@timestamp": "2026-11-03T00:00:00Z", "v": id}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

async fn refresh(app: &axum::Router, stream: &str) {
    let (status, body) = call(app, "POST", &format!("/{stream}/_refresh"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// `(generation, backing index names)`.
async fn stream_state(app: &axum::Router, stream: &str) -> (u64, Vec<String>) {
    let (status, body) = call(app, "GET", &format!("/_data_stream/{stream}"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ds = &body["data_streams"][0];
    (
        ds["generation"].as_u64().unwrap_or(0),
        ds["indices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["index_name"].as_str().unwrap_or_default().to_string())
            .collect(),
    )
}

async fn rollover(app: &axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    call(app, "POST", path, body).await
}

fn reason(body: &Value) -> &str {
    body.pointer("/error/root_cause/0/reason")
        .and_then(Value::as_str)
        .or_else(|| body.get("error").and_then(Value::as_str))
        .unwrap_or_default()
}

#[tokio::test]
async fn dry_run_reports_candidate_without_creating() {
    let (app, _dir) = app().await;
    create_stream(&app, "ds").await;
    write_doc(&app, "ds", "1").await;
    refresh(&app, "ds").await;

    let (status, body) = rollover(&app, "/ds/_rollover", json!({"dry_run": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["old_index"], ".ds-ds-000001", "{body}");
    assert_eq!(body["new_index"], ".ds-ds-000002", "{body}");
    assert_eq!(body["rolled_over"], false, "{body}");
    assert_eq!(body["dry_run"], true, "{body}");

    let (generation, indices) = stream_state(&app, "ds").await;
    assert_eq!(generation, 1);
    assert_eq!(indices, vec![".ds-ds-000001"]);
    let (status, _) = call(&app, "GET", "/.ds-ds-000002", Value::Null).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a dry run must not create the candidate backing index"
    );
}

#[tokio::test]
async fn dry_run_query_parameter_is_honoured() {
    let (app, _dir) = app().await;
    create_stream(&app, "ds").await;
    write_doc(&app, "ds", "1").await;
    refresh(&app, "ds").await;

    let (status, body) = rollover(&app, "/ds/_rollover?dry_run=true", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rolled_over"], false, "{body}");
    assert_eq!(body["dry_run"], true, "{body}");
    assert_eq!(stream_state(&app, "ds").await.0, 1);
}

#[tokio::test]
async fn max_docs_condition_blocks_then_rolls() {
    let (app, _dir) = app().await;
    create_stream(&app, "ds").await;
    write_doc(&app, "ds", "1").await;
    refresh(&app, "ds").await;

    let (status, body) = rollover(
        &app,
        "/ds/_rollover",
        json!({"conditions": {"max_docs": 2}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rolled_over"], false, "{body}");
    assert_eq!(body["conditions"]["[max_docs: 2]"], false, "{body}");
    assert_eq!(body["old_index"], ".ds-ds-000001", "{body}");
    assert_eq!(body["new_index"], ".ds-ds-000002", "{body}");
    assert_eq!(stream_state(&app, "ds").await.0, 1);

    write_doc(&app, "ds", "2").await;
    refresh(&app, "ds").await;
    let (status, body) = rollover(
        &app,
        "/ds/_rollover",
        json!({"conditions": {"max_docs": 2}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rolled_over"], true, "{body}");
    assert_eq!(body["conditions"]["[max_docs: 2]"], true, "{body}");
    assert_eq!(body["old_index"], ".ds-ds-000001", "{body}");
    assert_eq!(body["new_index"], ".ds-ds-000002", "{body}");

    let (generation, indices) = stream_state(&app, "ds").await;
    assert_eq!(generation, 2);
    assert_eq!(indices, vec![".ds-ds-000001", ".ds-ds-000002"]);

    // The data stream now writes to the new backing index.
    let (status, body) = call(
        &app,
        "PUT",
        "/ds/_doc/3",
        json!({"@timestamp": "2026-11-03T00:00:01Z"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["_index"], ".ds-ds-000002", "{body}");
}

#[tokio::test]
async fn max_age_condition() {
    let (app, _dir) = app().await;
    create_stream(&app, "ds").await;
    write_doc(&app, "ds", "1").await;
    refresh(&app, "ds").await;

    let (status, body) = rollover(
        &app,
        "/ds/_rollover",
        json!({"conditions": {"max_age": "1h"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rolled_over"], false, "{body}");
    assert_eq!(body["conditions"]["[max_age: 1h]"], false, "{body}");
    assert_eq!(stream_state(&app, "ds").await.0, 1);

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let (status, body) = rollover(
        &app,
        "/ds/_rollover",
        json!({"conditions": {"max_age": "1ms"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rolled_over"], true, "{body}");
    assert_eq!(body["conditions"]["[max_age: 1ms]"], true, "{body}");
    assert_eq!(stream_state(&app, "ds").await.0, 2);
}

#[tokio::test]
async fn conditions_are_ored() {
    let (app, _dir) = app().await;
    create_stream(&app, "ds").await;
    write_doc(&app, "ds", "1").await;
    refresh(&app, "ds").await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let (status, body) = rollover(
        &app,
        "/ds/_rollover",
        json!({"conditions": {"max_docs": 100, "max_age": "1ms"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rolled_over"], true, "{body}");
    assert_eq!(body["conditions"]["[max_docs: 100]"], false, "{body}");
    assert_eq!(body["conditions"]["[max_age: 1ms]"], true, "{body}");
}

#[tokio::test]
async fn unsupported_conditions_and_body_fields_are_refused() {
    let (app, _dir) = app().await;
    create_stream(&app, "ds").await;
    write_doc(&app, "ds", "1").await;
    refresh(&app, "ds").await;

    let (status, body) = rollover(
        &app,
        "/ds/_rollover",
        json!({"conditions": {"max_size": "1gb"}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(reason(&body).contains("max_size"), "{body}");
    assert_eq!(stream_state(&app, "ds").await.0, 1);

    let (status, body) = rollover(&app, "/ds/_rollover", json!({"new_index": "custom-name"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(reason(&body).contains("new_index"), "{body}");
    assert_eq!(stream_state(&app, "ds").await.0, 1);
}

#[tokio::test]
async fn plain_rollover_reports_backing_names() {
    let (app, _dir) = app().await;
    create_stream(&app, "ds").await;
    write_doc(&app, "ds", "1").await;
    refresh(&app, "ds").await;

    let (status, body) = rollover(&app, "/ds/_rollover", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["old_index"], ".ds-ds-000001", "{body}");
    assert_eq!(body["new_index"], ".ds-ds-000002", "{body}");
    assert_eq!(body["rolled_over"], true, "{body}");
    assert_eq!(body["dry_run"], false, "{body}");
    let (generation, indices) = stream_state(&app, "ds").await;
    assert_eq!(generation, 2);
    assert_eq!(indices, vec![".ds-ds-000001", ".ds-ds-000002"]);
}
