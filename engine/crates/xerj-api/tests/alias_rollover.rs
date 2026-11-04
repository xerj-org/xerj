//! `POST /{index_alias}/_rollover` must create the next index, copy the
//! write index's mapping/settings and alias metadata, and move the write
//! target. The same route already served data streams; index aliases used to
//! fall through to "data stream not found" and returned 404.
//!
//! Pinned here:
//! - generated names increment the trailing `-NNNNNN`; a name without that
//!   suffix is refused with ES's `^.*-\d+$` sentence;
//! - `new_index` overrides the generated name;
//! - `dry_run` and `max_docs` block the creation/move;
//! - the new index carries the old mapping and the alias's filter, and the old
//!   member becomes non-write while remaining readable;
//! - a multi-member alias without a write index is refused, not silently
//!   rolled from its first member.
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

async fn create_logs_index(app: &axum::Router, index: &str, alias: &str, alias_body: Value) {
    let (status, body) = call(
        app,
        "PUT",
        &format!("/{index}"),
        json!({
            "settings": {"number_of_shards": 1},
            "mappings": {"properties": {"v": {"type": "keyword"}}},
            "aliases": {alias: alias_body}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn write_doc(app: &axum::Router, index_or_alias: &str, id: &str, v: &str) {
    let (status, body) = call(
        app,
        "PUT",
        &format!("/{index_or_alias}/_doc/{id}"),
        json!({"v": v}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

async fn refresh(app: &axum::Router, index_or_alias: &str) {
    let (status, body) = call(
        app,
        "POST",
        &format!("/{index_or_alias}/_refresh"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// `(index, is_write_index)` for one alias.
async fn alias_members(app: &axum::Router, alias: &str) -> Vec<(String, bool)> {
    let (status, body) = call(app, "GET", "/_cat/aliases?format=json", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut members: Vec<(String, bool)> = body
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["alias"] == alias)
        .map(|entry| {
            let write =
                entry["is_write_index"] == json!(true) || entry["is_write_index"] == json!("true");
            (
                entry["index"].as_str().unwrap_or_default().to_string(),
                write,
            )
        })
        .collect();
    members.sort_by(|a, b| a.0.cmp(&b.0));
    members
}

fn reason(body: &Value) -> &str {
    body.pointer("/error/root_cause/0/reason")
        .and_then(Value::as_str)
        .or_else(|| body.get("error").and_then(Value::as_str))
        .unwrap_or_default()
}

async fn rollover(app: &axum::Router, alias: &str, body: Value) -> (StatusCode, Value) {
    call(app, "POST", &format!("/{alias}/_rollover"), body).await
}

#[tokio::test]
async fn rollover_creates_next_index_copies_mapping_and_moves_write_target() {
    let (app, _dir) = app().await;
    create_logs_index(&app, "logs-000001", "logs", json!({"is_write_index": true})).await;
    write_doc(&app, "logs-000001", "1", "one").await;
    refresh(&app, "logs-000001").await;

    let (status, body) = rollover(&app, "logs", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["old_index"], "logs-000001", "{body}");
    assert_eq!(body["new_index"], "logs-000002", "{body}");
    assert_eq!(body["rolled_over"], true, "{body}");
    assert_eq!(body["dry_run"], false, "{body}");

    let (status, body) = call(&app, "GET", "/logs-000002/_mapping", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["logs-000002"]["mappings"]["properties"]["v"]["type"], "keyword",
        "{body}"
    );

    write_doc(&app, "logs", "2", "two").await;
    refresh(&app, "logs").await;
    let (status, body) = call(&app, "GET", "/logs-000002/_doc/2", Value::Null).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the write must land in the new index: {body}"
    );
    let (status, body) = call(&app, "POST", "/logs/_search", json!({"size": 10})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["hits"]["total"]["value"], 2, "{body}");
    assert_eq!(
        alias_members(&app, "logs").await,
        vec![
            ("logs-000001".to_string(), false),
            ("logs-000002".to_string(), true),
        ]
    );
}

#[tokio::test]
async fn dry_run_and_max_docs_do_not_create_or_move() {
    let (app, _dir) = app().await;
    create_logs_index(&app, "logs-000001", "logs", json!({"is_write_index": true})).await;
    write_doc(&app, "logs-000001", "1", "one").await;
    refresh(&app, "logs").await;

    let (status, body) = rollover(&app, "logs", json!({"dry_run": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rolled_over"], false, "{body}");
    assert_eq!(body["dry_run"], true, "{body}");
    assert_eq!(body["old_index"], "logs-000001", "{body}");
    assert_eq!(body["new_index"], "logs-000002", "{body}");
    let (status, _) = call(&app, "GET", "/logs-000002", Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = rollover(&app, "logs", json!({"conditions": {"max_docs": 2}})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rolled_over"], false, "{body}");
    assert_eq!(body["conditions"]["[max_docs: 2]"], false, "{body}");
    let (status, _) = call(&app, "GET", "/logs-000002", Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    write_doc(&app, "logs-000001", "2", "two").await;
    refresh(&app, "logs").await;
    let (status, body) = rollover(&app, "logs", json!({"conditions": {"max_docs": 2}})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rolled_over"], true, "{body}");
    assert_eq!(body["conditions"]["[max_docs: 2]"], true, "{body}");
}

#[tokio::test]
async fn explicit_new_index_name_is_honoured() {
    let (app, _dir) = app().await;
    create_logs_index(&app, "logs-000001", "logs", json!({"is_write_index": true})).await;

    let (status, body) = rollover(&app, "logs", json!({"new_index": "logs-next"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["new_index"], "logs-next", "{body}");
    assert_eq!(body["rolled_over"], true, "{body}");
    write_doc(&app, "logs", "1", "one").await;
    let (status, _) = call(&app, "GET", "/logs-next/_doc/1", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn name_without_numeric_suffix_is_refused() {
    let (app, _dir) = app().await;
    let (status, body) = call(&app, "PUT", "/plain", json!({"aliases": {"p": {}}})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = rollover(&app, "p", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        reason(&body).contains("does not match pattern '^.*-\\d+$'"),
        "{body}"
    );
}

#[tokio::test]
async fn ambiguous_alias_is_refused() {
    let (app, _dir) = app().await;
    for index in ["i1", "i2"] {
        let (status, body) = call(&app, "PUT", &format!("/{index}"), json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, body) = call(
        &app,
        "POST",
        "/_aliases",
        json!({"actions": [
            {"add": {"index": "i1", "alias": "amb"}},
            {"add": {"index": "i2", "alias": "amb"}}
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = rollover(&app, "amb", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(reason(&body).contains("no write index"), "{body}");
}

#[tokio::test]
async fn filter_is_copied_to_the_new_member() {
    let (app, _dir) = app().await;
    create_logs_index(
        &app,
        "logs-000001",
        "logs",
        json!({
            "is_write_index": true,
            "filter": {"term": {"v": "keep"}}
        }),
    )
    .await;
    write_doc(&app, "logs-000001", "1", "keep").await;
    write_doc(&app, "logs-000001", "2", "drop").await;
    refresh(&app, "logs").await;

    let (status, body) = rollover(&app, "logs", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = call(&app, "GET", "/_alias/logs", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["logs-000002"]["aliases"]["logs"]["filter"],
        json!({"term": {"v": "keep"}}),
        "{body}"
    );
    assert_eq!(
        body["logs-000001"]["aliases"]["logs"]["is_write_index"], false,
        "{body}"
    );
    let (status, body) = call(&app, "POST", "/logs/_search", json!({"size": 10})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["hits"]["total"]["value"], 1,
        "the filtered alias must keep exposing only the keep document: {body}"
    );
}

#[tokio::test]
async fn unsupported_alias_rollover_fields_are_refused() {
    let (app, _dir) = app().await;
    create_logs_index(&app, "logs-000001", "logs", json!({"is_write_index": true})).await;

    let (status, body) = rollover(&app, "logs", json!({"settings": {"number_of_shards": 2}})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(reason(&body).contains("settings"), "{body}");
    assert!(
        reason(&body).contains("index aliases"),
        "the refusal must identify alias rollover, not the data-stream path: {body}"
    );
}
