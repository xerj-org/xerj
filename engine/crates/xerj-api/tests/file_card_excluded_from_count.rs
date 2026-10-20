//! A file-card anchor (`ax_locator: "file"`) lives in the dataset index so a
//! graph edge can resolve it. It is not a content record. A bare `_count` —
//! what `xerj autoindex map` reads, and what an agent curls — must not count
//! it. An explicit query still does, and `_search` can still return the card.

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

async fn json_req(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn bare_count_skips_file_cards_and_a_term_query_still_sees_them() {
    let (app, _dir) = app().await;
    let (status, body) = json_req(
        &app,
        "PUT",
        "/docs",
        json!({
            "mappings": { "properties": {
                "body": { "type": "text" },
                "ax_locator": { "type": "keyword" }
            }}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create index: {body}");

    for (id, doc) in [
        ("s0", json!({"body": "the design", "ax_locator": "s0"})),
        ("s1", json!({"body": "the rest", "ax_locator": "s1"})),
        ("card", json!({"body": "README.md", "ax_locator": "file"})),
    ] {
        let (status, body) = json_req(&app, "PUT", &format!("/docs/_doc/{id}"), doc).await;
        assert!(status.is_success(), "index {id}: {status} {body}");
    }
    let (status, body) = json_req(&app, "POST", "/docs/_refresh", json!({})).await;
    assert!(status.is_success(), "refresh: {status} {body}");

    let (status, count) = json_req(&app, "GET", "/docs/_count", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{count}");
    assert_eq!(
        count["count"].as_u64(),
        Some(2),
        "the file card must not inflate a bare count: {count}"
    );

    let (status, glob) = json_req(&app, "GET", "/docs*/_count", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{glob}");
    assert_eq!(glob["count"].as_u64(), Some(2), "{glob}");

    let (status, cards) = json_req(
        &app,
        "POST",
        "/docs/_count",
        json!({"query": {"term": {"ax_locator": "file"}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cards}");
    assert_eq!(
        cards["count"].as_u64(),
        Some(1),
        "an explicit card query still counts the anchor: {cards}"
    );

    let (status, search) = json_req(
        &app,
        "POST",
        "/docs/_search",
        json!({"size": 0, "track_total_hits": true, "query": {"match_all": {}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{search}");
    assert_eq!(
        search.pointer("/hits/total/value").and_then(Value::as_u64),
        Some(3),
        "the card stays in the index: {search}"
    );
}
