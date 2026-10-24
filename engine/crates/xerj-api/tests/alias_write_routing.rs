//! Writes through a multi-index alias must follow ES's write-index rule.
//!
//! `Engine::get_index` resolves an alias to its FIRST backing index (fine for
//! reads by name), and every single-document write used to reach the store
//! through it. A two-index alias therefore accepted `PUT /alias/_doc/1` with
//! 201 and wrote the document to whichever member happened to be listed
//! first, even when the other member carried `is_write_index: true`. ES
//! refuses the ambiguous shape and routes the designated one.
//!
//! All document writes now resolve through `Engine::write_target`: one backing
//! index, or exactly one `is_write_index: true`; otherwise ES's 400. The bulk
//! path uses the same helper and rewrites the item's target so its result and
//! storage agree.
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

async fn ndjson(app: &axum::Router, path: &str, body: String) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/x-ndjson")
                .body(Body::from(body))
                .unwrap(),
        )
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

async fn docs_in(app: &axum::Router, index: &str) -> u64 {
    let (status, body) = call(app, "POST", &format!("/{index}/_refresh"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(app, "GET", &format!("/{index}/_count"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["count"].as_u64().unwrap_or(0)
}

fn refusal(body: &Value) -> &str {
    body.pointer("/error/root_cause/0/reason")
        .and_then(Value::as_str)
        .or_else(|| body.get("error").and_then(Value::as_str))
        .unwrap_or_default()
}

#[tokio::test]
async fn writes_follow_the_alias_write_index_and_refuse_ambiguity() {
    let (app, _dir) = app().await;
    for ix in ["i1", "i2"] {
        let (status, body) = call(&app, "PUT", &format!("/{ix}"), json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, body) = call(
        &app,
        "POST",
        "/_aliases",
        json!({"actions": [
            {"add": {"index": "i1", "alias": "multi"}},
            {"add": {"index": "i2", "alias": "multi"}},
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Ambiguous: more than one backing index, none designated. ES refuses and
    // nothing is written.
    let (status, body) = call(&app, "PUT", "/multi/_doc/1", json!({"t": "one"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body.pointer("/error/type").and_then(Value::as_str),
        Some("illegal_argument_exception"),
        "{body}"
    );
    assert!(
        refusal(&body).contains("no write index is defined for alias [multi]"),
        "{body}"
    );
    assert_eq!(
        docs_in(&app, "i1").await,
        0,
        "no index received the refused write"
    );
    assert_eq!(docs_in(&app, "i2").await, 0);

    // Designate i2: the write must land there, not on the first member.
    let (status, body) = call(
        &app,
        "PUT",
        "/i2/_alias/multi",
        json!({"is_write_index": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(&app, "PUT", "/multi/_doc/2", json!({"t": "two"})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["_index"], "i2",
        "response names the concrete index: {body}"
    );
    let (status, body) = call(&app, "GET", "/i2/_doc/2", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(&app, "GET", "/multi/_doc/2", Value::Null).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "get via the alias uses the write index: {body}"
    );
    assert_eq!(body["_index"], "i2", "{body}");
    let (status, _) = call(&app, "HEAD", "/multi/_doc/2", Value::Null).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "head via the alias uses the write index"
    );
    let (status, _) = call(&app, "GET", "/i1/_doc/2", Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(docs_in(&app, "i1").await, 0);
    assert_eq!(docs_in(&app, "i2").await, 1);

    // Every document-write entry point takes the same route.
    let (status, body) = call(&app, "POST", "/multi/_doc", json!({"t": "auto"})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["_index"], "i2", "{body}");

    let (status, body) = call(&app, "PUT", "/multi/_create/3", json!({"t": "created"})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["_index"], "i2", "{body}");

    let (status, body) = call(
        &app,
        "POST",
        "/multi/_update/2",
        json!({"doc": {"t": "updated"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["_index"], "i2", "{body}");
    let (_, doc) = call(&app, "GET", "/i2/_doc/2", Value::Null).await;
    assert_eq!(doc["_source"]["t"], "updated", "{doc}");

    let (status, body) = call(&app, "DELETE", "/multi/_doc/2", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["_index"], "i2", "{body}");

    // Bulk resolves and rewrites the target too, including the item result.
    let (status, body) = ndjson(
        &app,
        "/_bulk",
        "{\"index\":{\"_index\":\"multi\",\"_id\":\"9\"}}\n{\"t\":\"bulk\"}\n".to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["index"]["_index"], "i2", "{body}");
    assert_eq!(body["items"][0]["index"]["status"], 201, "{body}");

    // A one-index alias needs no designation.
    let (status, body) = call(
        &app,
        "POST",
        "/_aliases",
        json!({"actions": [{"add": {"index": "i1", "alias": "solo"}}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(&app, "PUT", "/solo/_doc/7", json!({"t": "solo"})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["_index"], "i1", "{body}");

    // Two designated write indices are refused like the ambiguous shape.
    let (status, body) = call(
        &app,
        "PUT",
        "/i1/_alias/multi",
        json!({"is_write_index": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(&app, "PUT", "/multi/_doc/10", json!({"t": "ten"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        refusal(&body).contains("has more than one write index"),
        "{body}"
    );
}
