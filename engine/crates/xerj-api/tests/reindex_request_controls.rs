//! `_reindex` must honour the request controls that bound or guard a copy.
//!
//! `dest.op_type=create`, top-level `conflicts`, and `max_docs` were not in the
//! request structs, so serde dropped them. A caller asking `create` got an
//! overwrite of the existing destination document; a caller asking
//! `max_docs: 1` got the whole source. Both answered 200, so the wrong result
//! was silent.
//!
//! Elasticsearch semantics pinned here:
//! - `create` conflicts are version conflicts; `conflicts: abort` (default)
//!   stops on the first one and answers 409 with the normal reindex summary,
//!   `version_conflicts`, and one `failures[]` entry whose item `status` is 409;
//! - `conflicts: proceed` skips the conflict, counts it, and continues;
//! - `max_docs` caps SUCCESSFUL writes, so with `proceed` the source may be
//!   read past the cap until that many documents have been written.
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
        json!({"mappings": {"properties": {"v": {"type": "keyword"}}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn doc(app: &axum::Router, index: &str, id: &str, v: &str) {
    let (status, body) = call(app, "PUT", &format!("/{index}/_doc/{id}"), json!({"v": v})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

async fn value_of(app: &axum::Router, index: &str, id: &str) -> Option<String> {
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
    body["_source"]["v"].as_str().map(str::to_string)
}

async fn count(app: &axum::Router, index: &str) -> u64 {
    let (status, body) = call(app, "POST", &format!("/{index}/_refresh"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(app, "GET", &format!("/{index}/_count"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["count"].as_u64().unwrap_or(0)
}

async fn reindex(app: &axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    call(app, "POST", path, body).await
}

#[tokio::test]
async fn op_type_create_aborts_without_overwriting() {
    let (app, _dir) = app().await;
    create_index(&app, "s").await;
    create_index(&app, "d").await;
    doc(&app, "s", "1", "source").await;
    doc(&app, "d", "1", "existing").await;

    let (status, body) = reindex(
        &app,
        "/_reindex",
        json!({"source": {"index": "s"}, "dest": {"index": "d", "op_type": "create"}}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["created"], 0, "{body}");
    assert_eq!(body["updated"], 0, "{body}");
    assert_eq!(body["version_conflicts"], 1, "{body}");
    assert_eq!(body["total"], 0, "{body}");
    assert_eq!(
        body.pointer("/failures/0/status").and_then(Value::as_u64),
        Some(409),
        "{body}"
    );
    assert_eq!(
        body.pointer("/failures/0/cause/type")
            .and_then(Value::as_str),
        Some("version_conflict_engine_exception"),
        "{body}"
    );
    assert_eq!(
        value_of(&app, "d", "1").await.as_deref(),
        Some("existing"),
        "the existing destination document must not be overwritten"
    );
}

#[tokio::test]
async fn op_type_create_with_conflicts_proceed_skips_existing() {
    let (app, _dir) = app().await;
    create_index(&app, "s").await;
    create_index(&app, "d").await;
    doc(&app, "s", "1", "source-one").await;
    doc(&app, "s", "2", "source-two").await;
    doc(&app, "d", "1", "existing").await;

    let (status, body) = reindex(
        &app,
        "/_reindex",
        json!({
            "conflicts": "proceed",
            "source": {"index": "s"},
            "dest": {"index": "d", "op_type": "create"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["created"], 1, "{body}");
    assert_eq!(body["updated"], 0, "{body}");
    assert_eq!(body["version_conflicts"], 1, "{body}");
    assert_eq!(body["total"], 1, "{body}");
    assert_eq!(body["failures"], json!([]), "{body}");
    assert_eq!(value_of(&app, "d", "1").await.as_deref(), Some("existing"));
    assert_eq!(
        value_of(&app, "d", "2").await.as_deref(),
        Some("source-two")
    );
}

#[tokio::test]
async fn max_docs_caps_successful_writes() {
    let (app, _dir) = app().await;
    create_index(&app, "s").await;
    for (id, v) in [("1", "one"), ("2", "two"), ("3", "three")] {
        doc(&app, "s", id, v).await;
    }

    let (status, body) = reindex(
        &app,
        "/_reindex",
        json!({"max_docs": 1, "source": {"index": "s"}, "dest": {"index": "d1"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["created"], 1, "{body}");
    assert_eq!(body["total"], 1, "{body}");
    assert_eq!(count(&app, "d1").await, 1);

    let (status, body) = reindex(
        &app,
        "/_reindex",
        json!({"max_docs": 2, "source": {"index": "s"}, "dest": {"index": "d2"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["created"], 2, "{body}");
    assert_eq!(body["total"], 2, "{body}");
    assert_eq!(count(&app, "d2").await, 2);

    // The query-parameter form is part of the API too.
    let (status, body) = reindex(
        &app,
        "/_reindex?max_docs=1",
        json!({"source": {"index": "s"}, "dest": {"index": "d3"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["created"], 1, "{body}");
    assert_eq!(count(&app, "d3").await, 1);

    // -1 explicitly means "no cap".
    let (status, body) = reindex(
        &app,
        "/_reindex",
        json!({"max_docs": -1, "source": {"index": "s"}, "dest": {"index": "d4"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["created"], 3, "{body}");
    assert_eq!(count(&app, "d4").await, 3);
}

#[tokio::test]
async fn max_docs_counts_successful_writes_when_conflicts_proceed() {
    let (app, _dir) = app().await;
    create_index(&app, "s").await;
    create_index(&app, "d").await;
    for (id, v) in [("1", "one"), ("2", "two"), ("3", "three")] {
        doc(&app, "s", id, v).await;
    }
    doc(&app, "d", "1", "existing").await;

    let (status, body) = reindex(
        &app,
        "/_reindex",
        json!({
            "conflicts": "proceed",
            "max_docs": 2,
            "source": {"index": "s"},
            "dest": {"index": "d", "op_type": "create"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version_conflicts"], 1, "{body}");
    assert_eq!(body["created"], 2, "{body}");
    assert_eq!(body["total"], 2, "{body}");
    assert_eq!(body["failures"], json!([]), "{body}");
    assert_eq!(value_of(&app, "d", "1").await.as_deref(), Some("existing"));
    assert_eq!(value_of(&app, "d", "2").await.as_deref(), Some("two"));
    assert_eq!(value_of(&app, "d", "3").await.as_deref(), Some("three"));
}

#[tokio::test]
async fn invalid_request_controls_are_refused_before_any_copy() {
    let (app, _dir) = app().await;
    create_index(&app, "s").await;
    doc(&app, "s", "1", "one").await;

    for body in [
        json!({"source": {"index": "s"}, "dest": {"index": "d", "op_type": "upsert"}}),
        json!({"conflicts": "ignore", "source": {"index": "s"}, "dest": {"index": "d"}}),
        json!({"max_docs": 0, "source": {"index": "s"}, "dest": {"index": "d"}}),
        json!({"max_docs": -2, "source": {"index": "s"}, "dest": {"index": "d"}}),
    ] {
        let (status, response) = reindex(&app, "/_reindex", body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} -> {response}");
        assert_eq!(
            response.pointer("/error/type").and_then(Value::as_str),
            Some("illegal_argument_exception"),
            "{body} -> {response}"
        );
    }

    // None of the refused requests may leave a destination index behind.
    let (status, body) = call(&app, "GET", "/d", Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}
