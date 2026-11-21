//! Search parameters Elasticsearch 8 refuses are refused here too, with ES's
//! own reasons (`RestSearchAction`, `SearchSourceBuilder` in 8.13.4).
//!
//! XERJ used to run three of these searches with a 200: a negative
//! `terminate_after` was ignored, a `track_total_hits` below -1 was read as
//! `true`, and `rest_total_hits_as_int` rendered a capped count as if it were
//! exact. A negative `size` was refused with a generic parse message. Each is
//! checked in the URL and, where ES reads it there too, in the body.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;
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

async fn send(app: &axum::Router, method: &str, path: &str, body: &str) -> (StatusCode, Value) {
    let content_type = if path.contains("_bulk") {
        "application/x-ndjson"
    } else {
        "application/json"
    };
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", content_type)
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

/// An index with three documents, so a capped total differs from the exact one.
async fn seeded() -> (axum::Router, tempfile::TempDir) {
    let (app, dir) = app().await;
    let bulk = "{\"index\":{\"_index\":\"spv\",\"_id\":\"1\"}}\n{\"foo\":\"bar\"}\n\
                {\"index\":{\"_index\":\"spv\",\"_id\":\"2\"}}\n{\"foo\":\"bar\"}\n\
                {\"index\":{\"_index\":\"spv\",\"_id\":\"3\"}}\n{\"foo\":\"bar\"}\n";
    let (status, got) = send(&app, "POST", "/_bulk?refresh=true", bulk).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["errors"], false, "{got}");
    (app, dir)
}

fn assert_refused(status: StatusCode, got: &Value, reason: &str) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{got}");
    assert_eq!(got["status"], 400, "{got}");
    assert_eq!(got["error"]["type"], "illegal_argument_exception", "{got}");
    assert_eq!(got["error"]["reason"], reason, "{got}");
    assert_eq!(got["error"]["root_cause"][0]["reason"], reason, "{got}");
}

#[tokio::test]
async fn a_negative_size_gets_es_reason() {
    let (app, _dir) = seeded().await;
    let (status, got) = send(&app, "GET", "/spv/_search?size=-5", "").await;
    assert_refused(
        status,
        &got,
        "[size] parameter cannot be negative, found [-5]",
    );
    let (status, got) = send(&app, "POST", "/spv/_search", r#"{"size":-5}"#).await;
    assert_refused(
        status,
        &got,
        "[size] parameter cannot be negative, found [-5]",
    );
    // An earlier bad field keeps its own reason.
    let (status, got) = send(&app, "POST", "/spv/_search", r#"{"from":-1,"size":-5}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{got}");
    let reason = got["error"]["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("[from] parameter cannot be negative"),
        "{got}"
    );
}

#[tokio::test]
async fn a_negative_terminate_after_is_refused() {
    let (app, _dir) = seeded().await;
    let (status, got) = send(&app, "GET", "/spv/_search?terminate_after=-1", "").await;
    assert_refused(status, &got, "terminateAfter must be > 0");
    let (status, got) = send(&app, "POST", "/spv/_search", r#"{"terminate_after":-1}"#).await;
    assert_refused(status, &got, "terminateAfter must be > 0");
    let (status, got) = send(&app, "GET", "/spv/_search?terminate_after=abc", "").await;
    assert_refused(
        status,
        &got,
        "Failed to parse int parameter [terminate_after] with value [abc]",
    );
    // ES reads it as a Java int.
    let (status, got) = send(&app, "GET", "/spv/_search?terminate_after=3000000000", "").await;
    assert_refused(
        status,
        &got,
        "Failed to parse int parameter [terminate_after] with value [3000000000]",
    );
}

#[tokio::test]
async fn a_track_total_hits_below_minus_one_is_refused() {
    let (app, _dir) = seeded().await;
    let reason = "[track_total_hits] parameter must be positive or equals to -1, got -2";
    let (status, got) = send(&app, "GET", "/spv/_search?track_total_hits=-2", "").await;
    assert_refused(status, &got, reason);
    let (status, got) = send(&app, "POST", "/spv/_search", r#"{"track_total_hits":-2}"#).await;
    assert_refused(status, &got, reason);
    let (status, got) = send(&app, "GET", "/spv/_search?track_total_hits=lots", "").await;
    assert_refused(
        status,
        &got,
        "Failed to parse int parameter [track_total_hits] with value [lots]",
    );
    let (status, got) = send(&app, "GET", "/spv/_search?track_total_hits=3000000000", "").await;
    assert_refused(
        status,
        &got,
        "Failed to parse int parameter [track_total_hits] with value [3000000000]",
    );
}

#[tokio::test]
async fn minus_one_disables_total_tracking_as_false_does() {
    let (app, _dir) = seeded().await;
    let (status, got) = send(&app, "GET", "/spv/_search?track_total_hits=-1", "").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert!(got["hits"].get("total").is_none(), "{got}");
    let (status, got) = send(&app, "POST", "/spv/_search", r#"{"track_total_hits":-1}"#).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert!(got["hits"].get("total").is_none(), "{got}");
}

#[tokio::test]
async fn an_int_total_needs_an_exact_count() {
    let (app, _dir) = seeded().await;
    let reason = "[rest_total_hits_as_int] cannot be used if the tracking of total hits is not \
                  accurate, got 2";
    let (status, got) = send(
        &app,
        "GET",
        "/spv/_search?rest_total_hits_as_int=true&track_total_hits=2",
        "",
    )
    .await;
    assert_refused(status, &got, reason);
    let (status, got) = send(
        &app,
        "POST",
        "/spv/_search?rest_total_hits_as_int=true",
        r#"{"track_total_hits":2}"#,
    )
    .await;
    assert_refused(status, &got, reason);
}

#[tokio::test]
async fn every_value_es_accepts_still_runs() {
    let (app, _dir) = seeded().await;
    for path in [
        "/spv/_search?size=0",
        "/spv/_search?terminate_after=0",
        "/spv/_search?terminate_after=5",
        "/spv/_search?track_total_hits=true",
        "/spv/_search?track_total_hits=false",
        "/spv/_search?track_total_hits=2",
        "/spv/_search?rest_total_hits_as_int=true",
        "/spv/_search?rest_total_hits_as_int=true&track_total_hits=true",
        "/spv/_search?rest_total_hits_as_int=true&track_total_hits=false",
        "/spv/_search?rest_total_hits_as_int=true&track_total_hits=-1",
        "/spv/_search?rest_total_hits_as_int=false&track_total_hits=2",
    ] {
        let (status, got) = send(&app, "GET", path, "").await;
        assert_eq!(status, StatusCode::OK, "{path}: {got}");
    }
    for body in [
        r#"{"size":0}"#,
        r#"{"terminate_after":0}"#,
        r#"{"terminate_after":5}"#,
    ] {
        let (status, got) = send(&app, "POST", "/spv/_search", body).await;
        assert_eq!(status, StatusCode::OK, "{body}: {got}");
    }
    let (_, got) = send(&app, "GET", "/spv/_search?rest_total_hits_as_int=true", "").await;
    assert_eq!(got["hits"]["total"], 3, "{got}");
    let (_, got) = send(&app, "GET", "/spv/_search?track_total_hits=2", "").await;
    assert_eq!(got["hits"]["total"]["value"], 2, "{got}");
    assert_eq!(got["hits"]["total"]["relation"], "gte", "{got}");
}

/// With no limit set, an integer total is counted exactly, as ES's
/// `checkRestTotalHits` does, instead of stopping at the 10,000 default.
#[tokio::test]
async fn an_int_total_without_a_limit_counts_past_ten_thousand() {
    let (app, _dir) = app().await;
    let mut bulk = String::new();
    for i in 0..10_001 {
        bulk.push_str(&format!(
            "{{\"index\":{{\"_index\":\"big\",\"_id\":\"{i}\"}}}}\n{{\"n\":{i}}}\n"
        ));
    }
    let (status, got) = send(&app, "POST", "/_bulk?refresh=true", &bulk).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["errors"], false);
    let (_, got) = send(&app, "GET", "/big/_search?size=0", "").await;
    assert_eq!(got["hits"]["total"]["value"], 10_000, "{got}");
    assert_eq!(got["hits"]["total"]["relation"], "gte", "{got}");
    let (status, got) = send(
        &app,
        "GET",
        "/big/_search?size=0&rest_total_hits_as_int=true",
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["hits"]["total"], 10_001, "{got}");
}
