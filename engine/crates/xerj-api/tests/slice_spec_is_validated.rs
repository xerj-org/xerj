//! `slice` is validated before a search runs.
//!
//! The post-search slice filter did its shard arithmetic in `u32` on the raw
//! request values: `{"slice":{"id":0,"max":4294967296}}` truncated `max` to 0
//! and the `shard % max` aborted the whole node (release builds use
//! `panic = "abort"`), for any caller with read access to one non-empty index.
//! A stored `number_of_shards: 0` reached the same division through the shard
//! count. Elasticsearch refuses these with a 400: `id` and `max` are ints,
//! `id >= 0`, `max > 1`, `id < max`, and a scroll may not use more slices than
//! `index.max_slices_per_scroll` (default 1024).
//!
//! Elasticsearch is referenced for semantics only. It is AGPL-3.0/SSPL-1.0/
//! Elastic-2.0 licensed and no code from it is reproduced here.

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

async fn seed(app: &axum::Router, index: &str, settings: Value) {
    let (status, resp) = json_req(app, "PUT", &format!("/{index}"), settings).await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    for i in 0..8 {
        let (status, resp) = json_req(
            app,
            "PUT",
            &format!("/{index}/_doc/{i}?refresh=true"),
            json!({"n": i}),
        )
        .await;
        assert!(status.is_success(), "{resp}");
    }
}

fn reason(resp: &Value) -> String {
    resp["error"]["reason"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// FAIL-BEFORE: the first body panicked on `% 0` inside the handler; the
/// others returned 200 with an arbitrary (often empty) slice.
#[tokio::test]
async fn out_of_range_slice_specs_are_400_not_a_crash() {
    let (app, _dir) = app().await;
    seed(&app, "s", json!({})).await;

    for (slice, want) in [
        (
            json!({"id": 0, "max": 4294967296u64}),
            "out of range of int",
        ),
        (json!({"id": 0, "max": 1}), "max must be greater than 1"),
        (
            json!({"id": -1, "max": 2}),
            "id must be greater than or equal to 0",
        ),
        (json!({"id": 2, "max": 2}), "max must be greater than id"),
        (
            json!({"id": "x", "max": 2}),
            "[slice] failed to parse field [id]",
        ),
    ] {
        let (status, resp) = json_req(&app, "POST", "/s/_search", json!({"slice": slice})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{slice}: {resp}");
        assert!(reason(&resp).contains(want), "{slice}: {resp}");
    }
}

/// Valid slices still partition the result set: two slices of `max: 2`
/// are disjoint and together return every document.
#[tokio::test]
async fn valid_slices_partition_the_hits() {
    let (app, _dir) = app().await;
    seed(&app, "s", json!({})).await;

    let mut seen = Vec::new();
    for id in [json!(0), json!("1")] {
        let (status, resp) = json_req(
            &app,
            "POST",
            "/s/_search",
            json!({"size": 100, "slice": {"id": id, "max": 2}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        for h in resp["hits"]["hits"].as_array().unwrap() {
            seen.push(h["_id"].as_str().unwrap().to_string());
        }
    }
    seen.sort();
    let all: Vec<String> = (0..8).map(|i| i.to_string()).collect();
    assert_eq!(seen, all);
}

/// FAIL-BEFORE: a stored shard count of 0 reached `rem_euclid(0)` in the
/// shard routing hash and aborted the node.
#[tokio::test]
async fn zero_shard_setting_does_not_divide_by_zero() {
    let (app, _dir) = app().await;
    let (status, _) = json_req(
        &app,
        "PUT",
        "/z",
        json!({"settings": {"number_of_shards": 0}}),
    )
    .await;
    if !status.is_success() {
        // Refused at creation is also correct; nothing to slice then.
        return;
    }
    let (status, resp) = json_req(&app, "PUT", "/z/_doc/1?refresh=true", json!({"n": 1})).await;
    assert!(status.is_success(), "{resp}");
    let (status, resp) = json_req(
        &app,
        "POST",
        "/z/_search",
        json!({"slice": {"id": 0, "max": 2}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
}

/// `scroll/12_slices.yml · Sliced scroll with invalid arguments`.
#[tokio::test]
async fn scroll_slices_respect_max_slices_per_scroll() {
    let (app, _dir) = app().await;
    seed(&app, "s", json!({})).await;
    let body = json!({"size": 1, "slice": {"id": "0", "max": 1025}});

    let (status, resp) = json_req(&app, "POST", "/s/_search?scroll=1m", body.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{resp}");
    assert!(
        reason(&resp).contains("The number of slices [1025] is too large"),
        "{resp}"
    );

    // Without a scroll the cap does not apply.
    let (status, resp) = json_req(&app, "POST", "/s/_search", body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{resp}");

    let (status, resp) = json_req(
        &app,
        "PUT",
        "/s/_settings",
        json!({"index.max_slices_per_scroll": 1025}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    let (status, resp) = json_req(&app, "POST", "/s/_search?scroll=1m", body).await;
    assert_eq!(status, StatusCode::OK, "{resp}");
}
