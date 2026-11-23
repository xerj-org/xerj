//! A field mapping that names a `normalizer` is refused (#3513).
//!
//! XERJ builds no normalizers: a custom one under `analysis.normalizer` was
//! already refused at create (#204), but naming a built-in one (`lowercase`)
//! or an undeclared one on the field itself was accepted, echoed back by
//! `GET /_mapping`, and then ignored, so `match tag: "fruit"` found nothing
//! for "Fruit". Refuse it on create and put-mapping, at any depth.
//!
//! Without a normalizer, `match` on a keyword multi-field stays a
//! case-sensitive whole-value term, as in ES (#3495).

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

async fn req(app: &axum::Router, method: &str, path: &str, body: String) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body))
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

fn assert_refused(status: StatusCode, body: &Value, what: &str) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{what}: {body}");
    assert_eq!(
        body.pointer("/error/type"),
        Some(&json!("mapper_parsing_exception")),
        "{what}: {body}"
    );
    let reason = body
        .pointer("/error/reason")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(reason.contains("normalizer"), "{what}: {body}");
}

#[tokio::test]
async fn create_with_a_field_normalizer_is_refused_and_creates_nothing() {
    let (app, _dir) = app().await;
    let kw = |n: &str| json!({"type": "keyword", "normalizer": n});
    let cases = [
        ("top_level", json!({"tag": kw("lowercase")})),
        (
            "multi_field",
            json!({"title": {"type": "text", "fields": {"raw": kw("lowercase")}}}),
        ),
        (
            "object_leaf",
            json!({"obj": {"properties": {"name": {"type": "text", "fields": {"raw": kw("lowercase")}}}}}),
        ),
        ("undeclared", json!({"tag": kw("nope")})),
    ];
    for (name, props) in cases {
        let index = format!("norm_{name}");
        let (status, body) = req(
            &app,
            "PUT",
            &format!("/{index}"),
            json!({"mappings": {"properties": props}}).to_string(),
        )
        .await;
        assert_refused(status, &body, &index);
        let (status, body) = req(&app, "GET", &format!("/{index}"), String::new()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{index} exists: {body}");
    }
}

#[tokio::test]
async fn put_mapping_with_a_field_normalizer_is_refused() {
    let (app, _dir) = app().await;
    let (status, body) = req(
        &app,
        "PUT",
        "/norm_put",
        json!({"mappings": {"properties": {"title": {"type": "text"}}}}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create: {body}");
    let (status, body) = req(
        &app,
        "PUT",
        "/norm_put/_mapping",
        json!({"properties": {"tag": {"type": "keyword", "normalizer": "lowercase"}}}).to_string(),
    )
    .await;
    assert_refused(status, &body, "put_mapping");
    let (status, body) = req(&app, "GET", "/norm_put/_mapping", String::new()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.pointer("/norm_put/mappings/properties/tag").is_none(),
        "refused field was mapped: {body}"
    );
}

#[tokio::test]
async fn keyword_multi_field_without_normalizer_is_case_sensitive() {
    let (app, _dir) = app().await;
    let raw = json!({"type": "text", "fields": {"raw": {"type": "keyword"}}});
    let (status, body) = req(
        &app,
        "PUT",
        "/cs",
        json!({"mappings": {"properties": {"title": raw}}}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create: {body}");
    let bulk = concat!(
        "{\"index\":{\"_id\":\"1\"}}\n{\"title\":\"Fruit\"}\n",
        "{\"index\":{\"_id\":\"2\"}}\n{\"title\":\"fruit\"}\n",
        "{\"index\":{\"_id\":\"3\"}}\n{\"title\":\"fruit salad\"}\n",
    );
    let (status, body) = req(&app, "POST", "/cs/_bulk", bulk.to_string()).await;
    assert_eq!(status, StatusCode::OK, "bulk: {body}");
    assert_eq!(body["errors"], json!(false), "bulk: {body}");
    for phase in ["memtable", "flushed"] {
        if phase == "flushed" {
            let (status, body) = req(&app, "POST", "/cs/_flush", String::new()).await;
            assert_eq!(status, StatusCode::OK, "flush: {body}");
        }
        for (q, want) in [
            ("Fruit", vec!["1"]),
            ("fruit", vec!["2"]),
            ("FRUIT", vec![]),
            ("salad", vec![]),
        ] {
            let query = json!({"match": {"title.raw": q}});
            let (status, body) = req(
                &app,
                "POST",
                "/cs/_search",
                json!({"query": query}).to_string(),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{query}: {body}");
            let mut ids: Vec<&str> = body["hits"]["hits"]
                .as_array()
                .expect("hits")
                .iter()
                .map(|h| h["_id"].as_str().expect("_id"))
                .collect();
            ids.sort_unstable();
            assert_eq!(ids, want, "{phase} {query}");
        }
    }
}
