//! Issue #1281: field names in `highlight.fields` may be patterns (`"*"`,
//! `"mess*"`, `"obj.*"`). XERJ looked each key up as a literal `_source` key,
//! so a Kibana-Discover-style `"fields": {"*": {}}` returned no highlight at
//! all. Expected results below were each verified against ES 8.13.4 on the
//! same mapping and document.
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
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
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

async fn seeded() -> (axum::Router, tempfile::TempDir) {
    let (app, dir) = app().await;
    let (st, body) = call(
        &app,
        "PUT",
        "/hl",
        json!({"mappings": {"properties": {
            "message": {"type": "text"},
            "title": {"type": "text"},
            "host": {"type": "keyword"},
            "n": {"type": "long"},
            "obj": {"properties": {"inner": {"type": "text"}}}
        }}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create index: {body}");
    let (st, body) = call(
        &app,
        "PUT",
        "/hl/_doc/1?refresh=true",
        json!({
            "message": "upstream timeout contacting backend",
            "title": "timeout report",
            "host": "timeout",
            "n": 5,
            "obj": {"inner": "a timeout here"}
        }),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "index doc: {body}");
    (app, dir)
}

/// The highlighted field names of the first hit, sorted.
async fn highlighted(app: &axum::Router, body: Value) -> Vec<String> {
    let (st, resp) = call(app, "POST", "/hl/_search", body.clone()).await;
    assert_eq!(st, StatusCode::OK, "{body}: {resp}");
    let mut names: Vec<String> = resp["hits"]["hits"][0]["highlight"]
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();
    names
}

#[tokio::test]
async fn star_highlights_the_queried_field() {
    let (app, _dir) = seeded().await;
    let body = json!({
        "_source": false,
        "query": {"match": {"message": "timeout"}},
        "highlight": {"fields": {"*": {}}}
    });
    let (st, resp) = call(&app, "POST", "/hl/_search", body).await;
    assert_eq!(st, StatusCode::OK, "{resp}");
    assert_eq!(
        resp["hits"]["hits"][0]["highlight"],
        json!({"message": ["upstream <em>timeout</em> contacting backend"]}),
        "{resp}"
    );
    for pattern in ["mess*", "*age", "m*e"] {
        let names = highlighted(
            &app,
            json!({
                "query": {"match": {"message": "timeout"}},
                "highlight": {"fields": {pattern: {}}}
            }),
        )
        .await;
        assert_eq!(names, ["message"], "{pattern}");
    }
}

#[tokio::test]
async fn require_field_match_false_highlights_every_text_and_keyword_field() {
    let (app, _dir) = seeded().await;
    let names = highlighted(
        &app,
        json!({
            "query": {"match": {"message": "timeout"}},
            "highlight": {"require_field_match": false, "fields": {"*": {}}}
        }),
    )
    .await;
    // `n` (long) is never a highlight target.
    assert_eq!(names, ["host", "message", "obj.inner", "title"]);

    let names = highlighted(
        &app,
        json!({
            "query": {"match": {"message": "timeout"}},
            "highlight": {"require_field_match": false, "fields": {"t*": {}}}
        }),
    )
    .await;
    assert_eq!(names, ["title"]);
}

#[tokio::test]
async fn star_follows_the_fields_the_query_targets() {
    let (app, _dir) = seeded().await;
    for (query, expected) in [
        (
            json!({"multi_match": {"query": "timeout", "fields": ["message", "title"]}}),
            vec!["message", "title"],
        ),
        (
            json!({"query_string": {"query": "timeout"}}),
            vec!["host", "message", "obj.inner", "title"],
        ),
        (json!({"term": {"host": "timeout"}}), vec!["host"]),
    ] {
        let names = highlighted(
            &app,
            json!({"query": query, "highlight": {"fields": {"*": {}}}}),
        )
        .await;
        assert_eq!(names, expected, "{query}");
    }
    let names = highlighted(
        &app,
        json!({
            "query": {"match": {"obj.inner": "timeout"}},
            "highlight": {"fields": {"obj.*": {}}}
        }),
    )
    .await;
    assert_eq!(names, ["obj.inner"]);
}

#[tokio::test]
async fn pattern_options_apply_to_each_expanded_field() {
    let (app, _dir) = seeded().await;
    let (st, resp) = call(
        &app,
        "POST",
        "/hl/_search",
        json!({
            "query": {"match": {"message": "timeout"}},
            "highlight": {"fields": {"mess*": {"pre_tags": ["<b>"], "post_tags": ["</b>"]}}}
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{resp}");
    assert_eq!(
        resp["hits"]["hits"][0]["highlight"],
        json!({"message": ["upstream <b>timeout</b> contacting backend"]}),
        "{resp}"
    );
}
