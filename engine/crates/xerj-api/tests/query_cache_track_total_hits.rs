//! A search with total tracking on is never answered from the cached result of
//! the same search with tracking off.
//!
//! The engine's whole-result query cache keys on the serialized
//! `SearchRequest`. `TrackTotalHits` was an `untagged` enum, which writes both
//! unit variants, `True` and `False`, as `null`, so the two modes shared one
//! cache entry. A `track_total_hits: false` search caches a total capped at
//! `size`; the same search with `true` then printed that capped count.

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

async fn post(app: &axum::Router, path: &str, ctype: &str, body: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", ctype)
        .body(Body::from(body.to_owned()))
        .expect("request");
    let response = app.clone().oneshot(req).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Three documents, so a total capped at `size: 1` differs from the exact one.
async fn seeded() -> (axum::Router, tempfile::TempDir) {
    let (app, dir) = app().await;
    let bulk = "{\"index\":{\"_index\":\"qct\",\"_id\":\"1\"}}\n{\"f\":\"x\"}\n\
                {\"index\":{\"_index\":\"qct\",\"_id\":\"2\"}}\n{\"f\":\"x\"}\n\
                {\"index\":{\"_index\":\"qct\",\"_id\":\"3\"}}\n{\"f\":\"x\"}\n";
    let (status, got) = post(&app, "/_bulk?refresh=true", "application/x-ndjson", bulk).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["errors"], false, "{got}");
    (app, dir)
}

#[tokio::test]
async fn tracking_on_after_tracking_off_counts_every_match() {
    let (app, _dir) = seeded().await;
    let off = r#"{"size":1,"track_total_hits":false,"query":{"term":{"f":"x"}}}"#;
    let on = r#"{"size":1,"track_total_hits":true,"query":{"term":{"f":"x"}}}"#;

    let (status, got) = post(&app, "/qct/_search", "application/json", off).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert!(got["hits"].get("total").is_none(), "{got}");

    let (status, got) = post(&app, "/qct/_search", "application/json", on).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got["hits"]["total"],
        json!({"value": 3, "relation": "eq"}),
        "{got}"
    );
}

#[tokio::test]
async fn a_template_with_tracking_on_after_one_with_tracking_off_counts_every_match() {
    let (app, _dir) = seeded().await;
    let off = r#"{"source":{"size":1,"track_total_hits":false,"query":{"match_all":{}}}}"#;
    let on = r#"{"source":{"size":1,"query":{"match_all":{}}}}"#;

    let (status, got) = post(&app, "/qct/_search/template", "application/json", off).await;
    assert_eq!(status, StatusCode::OK, "{got}");

    let (status, got) = post(&app, "/qct/_search/template", "application/json", on).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got["hits"]["total"],
        json!({"value": 3, "relation": "eq"}),
        "{got}"
    );
}
