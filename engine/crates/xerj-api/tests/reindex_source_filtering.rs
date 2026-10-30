//! `_reindex` must copy only the fields the caller asked for, and must not
//! silently ignore a transformation it cannot run.
//!
//! `source._source` was not in `ReindexSource` and `script` was not in
//! `ReindexBody`, so serde dropped both. A caller excluding a secret or large
//! field still copied it verbatim, and a caller asking for a redaction/rename
//! script got the untransformed document copied under a 200. Both are
//! data-minimization failures, not a missing convenience.
//!
//! Elasticsearch semantics pinned here:
//! - `_source: [fields]` and `_source: {excludes: [...]}` limit the bytes that
//!   reach the destination;
//! - `_source: false` is refused by the reindex context;
//! - `script` is part of the copy contract. This build has no reindex script
//!   context, so the request is refused instead of copying raw documents.
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
    let (status, body) = call(
        app,
        "PUT",
        &format!("/{index}"),
        json!({"mappings": {"properties": {
            "text": {"type": "keyword"},
            "secret": {"type": "keyword"}
        }}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn source_of(app: &axum::Router, index: &str, id: &str) -> Option<Value> {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/{index}/_doc/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    if response.status() != StatusCode::OK {
        return None;
    }
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Some(body["_source"].clone())
}

async fn reindex(app: &axum::Router, body: Value) -> (StatusCode, Value) {
    call(app, "POST", "/_reindex", body).await
}

#[tokio::test]
async fn source_include_list_limits_copied_fields() {
    let (app, _dir) = app().await;
    create_index(&app, "s").await;
    call(
        &app,
        "PUT",
        "/s/_doc/1",
        json!({"text": "keep", "secret": "drop"}),
    )
    .await;

    let (status, body) = reindex(
        &app,
        json!({
            "source": {"index": "s", "_source": ["text"]},
            "dest": {"index": "d1"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["created"], 1, "{body}");
    assert_eq!(
        source_of(&app, "d1", "1").await,
        Some(json!({"text": "keep"})),
        "the excluded field must not reach the destination"
    );
}

#[tokio::test]
async fn source_exclude_object_limits_copied_fields() {
    let (app, _dir) = app().await;
    create_index(&app, "s").await;
    call(
        &app,
        "PUT",
        "/s/_doc/1",
        json!({"text": "keep", "secret": "drop"}),
    )
    .await;

    let (status, body) = reindex(
        &app,
        json!({
            "source": {"index": "s", "_source": {"excludes": ["secret"]}},
            "dest": {"index": "d1"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["created"], 1, "{body}");
    assert_eq!(
        source_of(&app, "d1", "1").await,
        Some(json!({"text": "keep"})),
        "the excluded field must not reach the destination"
    );
}

#[tokio::test]
async fn source_false_is_refused_before_any_work() {
    let (app, _dir) = app().await;
    create_index(&app, "s").await;
    call(&app, "PUT", "/s/_doc/1", json!({"text": "keep"})).await;

    let (status, body) = reindex(
        &app,
        json!({
            "source": {"index": "s", "_source": false},
            "dest": {"index": "d"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body.pointer("/error/type").and_then(Value::as_str),
        Some("illegal_argument_exception"),
        "{body}"
    );
    let reason = body
        .pointer("/error/root_cause/0/reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(reason.contains("_source:false"), "{body}");

    let (status, _) = call(&app, "GET", "/d", Value::Null).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "no destination may be created"
    );
}

#[tokio::test]
async fn script_is_refused_instead_of_copying_untransformed() {
    let (app, _dir) = app().await;
    create_index(&app, "s").await;
    call(
        &app,
        "PUT",
        "/s/_doc/1",
        json!({"text": "keep", "secret": "drop"}),
    )
    .await;

    let (status, body) = reindex(
        &app,
        json!({
            "source": {"index": "s"},
            "dest": {"index": "d"},
            "script": {"source": "ctx._source.remove('secret')", "lang": "painless"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body.pointer("/error/type").and_then(Value::as_str),
        Some("illegal_argument_exception"),
        "{body}"
    );
    let reason = body
        .pointer("/error/root_cause/0/reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(reason.contains("scripts are not supported"), "{body}");

    let (status, _) = call(&app, "GET", "/d", Value::Null).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "no destination may be created"
    );
}

#[tokio::test]
async fn default_copies_the_whole_source() {
    let (app, _dir) = app().await;
    create_index(&app, "s").await;
    call(
        &app,
        "PUT",
        "/s/_doc/1",
        json!({"text": "keep", "secret": "also-kept"}),
    )
    .await;

    let (status, body) = reindex(
        &app,
        json!({"source": {"index": "s"}, "dest": {"index": "d"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        source_of(&app, "d", "1").await,
        Some(json!({"text": "keep", "secret": "also-kept"})),
        "with no filter the whole source is copied"
    );
}
