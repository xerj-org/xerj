//! An alias may designate at most one write index.
//!
//! `POST /_aliases` and `PUT /{index}/_alias/{alias}` used to accept two
//! members with `is_write_index: true` and answer 200. The resulting alias
//! was unusable: every document write through it answered 400 "alias [a] has
//! more than one write index", so the configuration error surfaced only at
//! the next write, possibly long after the alias update.
//!
//! The invariant is checked against the FINAL state of the action batch:
//! actions run in order, so an atomic swap (add the new write index, then
//! clear the old one) is valid even though an intermediate step names two.
//! A refused batch changes nothing, including when the second write index is
//! added through a wildcard that expands to several indices.
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

async fn create_index(app: &axum::Router, index: &str) {
    let (status, body) = call(app, "PUT", &format!("/{index}"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn post_actions(app: &axum::Router, actions: Value) -> (StatusCode, Value) {
    call(app, "POST", "/_aliases", json!({"actions": actions})).await
}

async fn put_alias(
    app: &axum::Router,
    index: &str,
    alias: &str,
    body: Value,
) -> (StatusCode, Value) {
    call(app, "PUT", &format!("/{index}/_alias/{alias}"), body).await
}

fn is_write(entry: &Value) -> bool {
    entry["is_write_index"] == json!(true) || entry["is_write_index"] == json!("true")
}

/// `(index, is_write_index)` for one alias from `_cat/aliases?format=json`.
async fn alias_members(app: &axum::Router, alias: &str) -> Vec<(String, bool)> {
    let (status, body) = call(app, "GET", "/_cat/aliases?format=json", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut members: Vec<(String, bool)> = body
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["alias"] == alias)
        .map(|entry| {
            (
                entry["index"].as_str().unwrap_or_default().to_string(),
                is_write(entry),
            )
        })
        .collect();
    members.sort_by(|a, b| a.0.cmp(&b.0));
    members
}

async fn write_through_alias(app: &axum::Router, alias: &str) -> (StatusCode, Value) {
    call(app, "PUT", &format!("/{alias}/_doc/1"), json!({"v": "x"})).await
}

fn reason(body: &Value) -> &str {
    body.pointer("/error/root_cause/0/reason")
        .and_then(Value::as_str)
        .or_else(|| body.get("error").and_then(Value::as_str))
        .unwrap_or_default()
}

#[tokio::test]
async fn post_aliases_rejects_two_write_indices_atomically() {
    let (app, _dir) = app().await;
    create_index(&app, "i1").await;
    create_index(&app, "i2").await;

    let (status, body) = post_actions(
        &app,
        json!([
            {"add": {"index": "i1", "alias": "a", "is_write_index": true}},
            {"add": {"index": "i2", "alias": "a", "is_write_index": true}}
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body.pointer("/error/type").and_then(Value::as_str),
        Some("illegal_argument_exception"),
        "{body}"
    );
    assert!(
        reason(&body).contains("alias [a] has more than one write index"),
        "{body}"
    );
    assert!(
        alias_members(&app, "a").await.is_empty(),
        "a refused batch must leave no alias behind"
    );
}

#[tokio::test]
async fn atomic_swap_in_one_request_is_allowed() {
    let (app, _dir) = app().await;
    create_index(&app, "i1").await;
    create_index(&app, "i2").await;
    let (status, body) = post_actions(
        &app,
        json!([{"add": {"index": "i1", "alias": "swap", "is_write_index": true}}]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Intermediate state has two writers; the final state has one.
    let (status, body) = post_actions(
        &app,
        json!([
            {"add": {"index": "i2", "alias": "swap", "is_write_index": true}},
            {"add": {"index": "i1", "alias": "swap", "is_write_index": false}}
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let members = alias_members(&app, "swap").await;
    assert_eq!(
        members,
        vec![("i1".to_string(), false), ("i2".to_string(), true)],
        "the final write index must be i2"
    );
    let (status, body) = write_through_alias(&app, "swap").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["_index"], "i2", "{body}");
}

#[tokio::test]
async fn put_alias_rejects_second_write_index_and_can_switch() {
    let (app, _dir) = app().await;
    create_index(&app, "i1").await;
    create_index(&app, "i2").await;
    let (status, body) = put_alias(&app, "i1", "a", json!({"is_write_index": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = put_alias(&app, "i2", "a", json!({"is_write_index": true})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        reason(&body).contains("alias [a] has more than one write index"),
        "{body}"
    );
    let (status, body) = write_through_alias(&app, "a").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["_index"], "i1",
        "the rejected PUT must not move the write index"
    );

    // The supported switch is two updates: clear the old flag, then set the
    // new one. Each step leaves at most one write index.
    let (status, body) = put_alias(&app, "i1", "a", json!({"is_write_index": false})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = put_alias(&app, "i2", "a", json!({"is_write_index": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = write_through_alias(&app, "a").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["_index"], "i2", "{body}");
}

#[tokio::test]
async fn wildcard_add_with_write_index_is_refused() {
    let (app, _dir) = app().await;
    create_index(&app, "i1").await;
    create_index(&app, "i2").await;

    let (status, body) = post_actions(
        &app,
        json!([{"add": {"index": "i*", "alias": "w", "is_write_index": true}}]),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        reason(&body).contains("alias [w] has more than one write index"),
        "{body}"
    );
    assert!(alias_members(&app, "w").await.is_empty());
}

#[tokio::test]
async fn implicit_single_member_plus_explicit_true_is_allowed() {
    let (app, _dir) = app().await;
    create_index(&app, "i1").await;
    create_index(&app, "i2").await;
    let (status, body) = put_alias(&app, "i1", "a", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // i1 is an implicit write target while alone; adding i2 with an explicit
    // flag leaves exactly one designated writer, so this is valid.
    let (status, body) = post_actions(
        &app,
        json!([{"add": {"index": "i2", "alias": "a", "is_write_index": true}}]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = write_through_alias(&app, "a").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["_index"], "i2", "{body}");
}
