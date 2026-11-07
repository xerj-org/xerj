//! Issue #3005: `_search` with `profile: true` returned placeholder timings.
//!
//! Every timer was `took_ms * 1_000_000` (millisecond-rounded, so 0 for a
//! fast search) or a literal `1`, floored to 1 ns: tools reading the
//! ES-shaped profile got fake numbers with no warning. The fix measures the
//! query and fetch phases in nanoseconds, feeds those measured values to every
//! timer that used to be a placeholder, and discloses in
//! `profile.xerj_timing` that breakdown timers repeat their enclosing phase
//! (an upper bound) because XERJ has no per-sub-phase timers.
//!
//! Fails before the fix: the query `time_in_nanos` is 0 for a sub-millisecond
//! search, the fetch and aggregation `build_aggregation` timers are 1, and
//! `profile.xerj_timing` does not exist.

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_reports_measured_phase_timings_and_says_what_they_are() {
    let (app, _dir) = app().await;
    let (st, _) = call(
        &app,
        "PUT",
        "/prof",
        json!({"mappings": {"properties": {"t": {"type": "text"}, "k": {"type": "keyword"}}}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let mut ndjson = String::new();
    for i in 0..200 {
        ndjson.push_str(&format!(
            "{{\"index\":{{\"_id\":\"d{i}\"}}}}\n{{\"t\":\"hello world {i}\",\"k\":\"k{}\"}}\n",
            i % 7
        ));
    }
    let response = app
        .clone()
        .oneshot(
            Request::post("/prof/_bulk?refresh=true")
                .header("content-type", "application/x-ndjson")
                .body(Body::from(ndjson))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let (st, body) = call(
        &app,
        "POST",
        "/prof/_search",
        json!({
            "profile": true,
            "query": {"match": {"t": "hello"}},
            "aggs": {
                "by_k": {"terms": {"field": "k"}},
                "sig": {"significant_text": {"field": "t"}}
            }
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let profile = &body["profile"];
    let note = &profile["xerj_timing"];
    let query_ns = note["query_phase_nanos"]
        .as_u64()
        .expect("query_phase_nanos");
    let fetch_ns = note["fetch_phase_nanos"]
        .as_u64()
        .expect("fetch_phase_nanos");
    assert_eq!(note["breakdown"], json!("not_instrumented"), "{note}");
    assert!(
        note["warning"]
            .as_str()
            .is_some_and(|w| w.contains("upper bound")),
        "the disclosure must say what the breakdown timers are: {note}"
    );
    // A search that parsed, matched 200 docs and merged a page takes far more
    // than a microsecond; the placeholder was 0 (sub-ms took) or 1.
    assert!(
        query_ns > 1_000,
        "query phase must be measured, got {query_ns} ns"
    );
    assert!(
        fetch_ns > 1,
        "fetch phase must be measured, got {fetch_ns} ns"
    );

    let shard = &profile["shards"][0];
    let q = &shard["searches"][0]["query"][0];
    assert_eq!(q["time_in_nanos"].as_u64(), Some(query_ns), "{q}");
    assert_eq!(
        shard["searches"][0]["collector"][0]["time_in_nanos"].as_u64(),
        Some(query_ns)
    );
    assert_eq!(shard["fetch"]["time_in_nanos"].as_u64(), Some(fetch_ns));
    assert_eq!(
        shard["fetch"]["breakdown"]["next_reader"].as_u64(),
        Some(fetch_ns),
        "the fetch next_reader timer was a literal 1"
    );

    let aggs = shard["aggregations"]
        .as_array()
        .expect("aggregations profile");
    assert!(!aggs.is_empty());
    for agg in aggs {
        assert_eq!(agg["time_in_nanos"].as_u64(), Some(query_ns), "{agg}");
        for timer in [
            "initialize",
            "build_aggregation",
            "build_leaf_collector",
            "collect",
            "post_collection",
        ] {
            assert_eq!(
                agg["breakdown"][timer].as_u64(),
                Some(query_ns),
                "aggregation timer {timer} must carry the measured phase, not a placeholder: {agg}"
            );
        }
    }
    let sig = aggs
        .iter()
        .find(|a| a["description"] == json!("sig"))
        .expect("significant_text profile node");
    for timer in ["extract_ns", "collect_analyzed_ns"] {
        let ns = sig["debug"][timer].as_u64().unwrap_or(0);
        assert!(
            ns > 1,
            "significant_text {timer} must be measured, got {ns}: {sig}"
        );
    }
}
