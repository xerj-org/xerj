//! `search.max_buckets` set through `PUT /_cluster/settings` caps the
//! buckets a search's aggregations may return, as in Elasticsearch 8.
//!
//! ES counts every bucket the final reduce keeps, across the whole
//! aggregation tree (nested `terms` buckets add to their parent's), and
//! answers 400 `too_many_buckets_exception` once the count is over the
//! limit. Transient overrides persistent, and `null` restores the default.
//! Before this, XERJ stored the setting and kept answering 200.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    content_type: &str,
    body: String,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", content_type)
        .body(Body::from(body))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn put_settings(app: &axum::Router, body: Value) {
    let (status, got) = send(
        app,
        "PUT",
        "/_cluster/settings",
        "application/json",
        body.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{got}");
}

/// The eight documents of `aggregations/max_buckets.yml`: eight distinct
/// `keyword` values, each with its own `date`.
async fn seeded() -> (axum::Router, tempfile::TempDir) {
    let (app, dir) = app().await;
    let (status, got) = send(
        &app,
        "PUT",
        "/test",
        "application/json",
        json!({"mappings": {"properties": {
            "keyword": {"type": "keyword"},
            "date": {"type": "date"}
        }}})
        .to_string(),
    )
    .await;
    assert!(status.is_success(), "create: {status} {got}");
    let keywords = ["dgx", "dfs", "foobar", "foo", "bar", "baz", "qux", "quux"];
    let mut bulk = String::new();
    for (i, keyword) in keywords.iter().enumerate() {
        bulk.push_str(&format!("{{\"index\":{{\"_id\":\"{}\"}}}}\n", i + 1));
        bulk.push_str(&format!(
            "{{\"date\":\"{}-03-03T00:00:00\",\"keyword\":\"{keyword}\"}}\n",
            2014 + i
        ));
    }
    let (status, got) = send(
        &app,
        "POST",
        "/test/_bulk?refresh=true",
        "application/x-ndjson",
        bulk,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["errors"], false, "{got}");
    (app, dir)
}

/// Eight top-level buckets.
fn terms() -> Value {
    json!({"size": 0, "aggregations": {"test": {"terms": {"field": "keyword"}}}})
}

/// Eight top-level buckets, each holding one `date` bucket: sixteen in all.
fn nested_terms() -> Value {
    json!({"size": 0, "aggregations": {"test": {
        "terms": {"field": "keyword"},
        "aggs": {"2": {"terms": {"field": "date"}}}
    }}})
}

async fn search(app: &axum::Router, body: Value) -> (StatusCode, Value) {
    send(
        app,
        "POST",
        "/test/_search",
        "application/json",
        body.to_string(),
    )
    .await
}

fn too_many_buckets(status: StatusCode, body: &Value, limit: u64) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let cause = &body["error"]["caused_by"];
    assert_eq!(cause["type"], "too_many_buckets_exception", "{body}");
    assert_eq!(cause["max_buckets"], limit, "{body}");
    let reason = cause["reason"].as_str().unwrap_or("");
    assert!(
        reason.contains(&format!(
            "Trying to create too many buckets. Must be less than or equal to: [{limit}]"
        )),
        "{body}"
    );
}

fn answered(status: StatusCode, body: &Value, top_buckets: usize) {
    assert_eq!(status, StatusCode::OK, "{body}");
    let buckets = body["aggregations"]["test"]["buckets"]
        .as_array()
        .unwrap_or_else(|| panic!("no buckets: {body}"));
    assert_eq!(buckets.len(), top_buckets, "{body}");
}

/// The vendored YAML case, step for step.
#[tokio::test]
async fn a_persistent_limit_refuses_aggregations_over_it() {
    let (app, _dir) = seeded().await;

    put_settings(&app, json!({"persistent": {"search.max_buckets": 3}})).await;
    let (status, body) = search(&app, terms()).await;
    too_many_buckets(status, &body, 3);

    put_settings(&app, json!({"persistent": {"search.max_buckets": 6}})).await;
    let (status, body) = search(&app, nested_terms()).await;
    too_many_buckets(status, &body, 6);
}

/// ES fails only when the count is over the limit, and the count includes
/// the nested buckets: sixteen fit under 16 and not under 15.
#[tokio::test]
async fn the_count_is_strict_and_includes_nested_buckets() {
    let (app, _dir) = seeded().await;

    put_settings(&app, json!({"persistent": {"search.max_buckets": 8}})).await;
    let (status, body) = search(&app, terms()).await;
    answered(status, &body, 8);
    let (status, body) = search(&app, nested_terms()).await;
    too_many_buckets(status, &body, 8);

    put_settings(&app, json!({"persistent": {"search.max_buckets": 15}})).await;
    let (status, body) = search(&app, nested_terms()).await;
    too_many_buckets(status, &body, 15);

    put_settings(&app, json!({"persistent": {"search.max_buckets": 16}})).await;
    let (status, body) = search(&app, nested_terms()).await;
    answered(status, &body, 8);
}

/// Transient wins over persistent; clearing with `null` falls back to the
/// other scope, then to the default.
#[tokio::test]
async fn transient_wins_and_null_restores_the_default() {
    let (app, _dir) = seeded().await;

    put_settings(&app, json!({"persistent": {"search.max_buckets": 3}})).await;
    put_settings(&app, json!({"transient": {"search.max_buckets": "100"}})).await;
    let (status, body) = search(&app, terms()).await;
    answered(status, &body, 8);

    put_settings(&app, json!({"transient": {"search.max_buckets": null}})).await;
    let (status, body) = search(&app, terms()).await;
    too_many_buckets(status, &body, 3);

    put_settings(&app, json!({"persistent": {"search.max_buckets": null}})).await;
    let (status, body) = search(&app, nested_terms()).await;
    answered(status, &body, 8);
}
