//! Issue #1022: the `significant_text` profiler debug fast path truncates its
//! foreground fetch at 10 000 documents per index.
//!
//! When a `profile: true` request carries a `significant_text` agg, the
//! handler pre-fetches the query-matched (foreground) docs with ONE search —
//! `{"query": …, "size": 10000, "from": 0}` — and folds their sources into
//! ES's profiler debug block (total_buckets / values_fetched / chars_fetched
//! / extract_count / collect_analyzed_count). With more than 10 000 matched
//! docs every counter silently under-reports: the profiler debug block is an
//! exact-statistics surface, and it reports made-up numbers.
//!
//! The counters are order-independent folds (sums and per-group unions), so
//! the fix feeds every matched doc through an incremental accumulator in one
//! engine walk — no materialisation, no paging order requirement.
//!
//! Fails before the fix at extract_count/values_fetched == 10 000 for a
//! 10 500-doc corpus (chars_fetched 110 000 vs 115 500,
//! collect_analyzed_count 20 000 vs 21 000).

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

/// Seed `n` identical docs `{"t": "hello world"}` — 11 chars, 2 analyzed
/// tokens per doc, token union of 2 — so every counter is a closed-form
/// function of `n`.
async fn seed_bulk(app: &axum::Router, index: &str, n: u64) {
    let (st, _) = call(
        app,
        "PUT",
        &format!("/{index}"),
        json!({"mappings": {"properties": {"t": {"type": "text"}}}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create {index}");

    let width = n.to_string().len();
    let chunk = 2_500u64;
    let mut start = 0u64;
    while start < n {
        let end = (start + chunk).min(n);
        let mut ndjson = String::new();
        for i in start..end {
            ndjson.push_str(&format!(
                "{{\"index\":{{\"_id\":\"d{i:0width$}\"}}}}\n{{\"t\":\"hello world\"}}\n",
                width = width
            ));
        }
        let response = app
            .clone()
            .oneshot(
                Request::post(format!("/{index}/_bulk"))
                    .header("content-type", "application/x-ndjson")
                    .body(Body::from(ndjson))
                    .unwrap(),
            )
            .await
            .expect("bulk request");
        assert_eq!(response.status(), StatusCode::OK, "bulk {start}..{end}");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).expect("bulk body");
        assert_eq!(body["errors"], json!(false), "bulk {start}..{end}: {body}");
        start = end;
    }
    let (st, _) = call(app, "POST", &format!("/{index}/_refresh"), Value::Null).await;
    assert_eq!(st, StatusCode::OK, "refresh {index}");
}

fn sig_debug(body: &Value) -> &Value {
    &body["profile"]["shards"][0]["aggregations"][0]["debug"]
}

/// Profiling a `significant_text` agg over 10 500 matched docs must count
/// ALL of them: the debug block is an exact-statistics surface. Every doc is
/// `{"t": "hello world"}` (1 string value, 11 chars, 2 distinct tokens), so
/// the exact answers are closed-form in N.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn significant_text_profile_counts_past_ten_thousand() {
    let (app, _dir) = app().await;
    const N: u64 = 10_500;
    seed_bulk(&app, "sig-big", N).await;

    let (st, body) = call(
        &app,
        "POST",
        "/sig-big/_search",
        json!({
            "size": 0,
            "profile": true,
            "query": {"match_all": {}},
            "aggs": {"sig": {"significant_text": {"field": "t"}}},
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let debug = sig_debug(&body);
    assert_eq!(
        debug.get("collection_strategy").and_then(Value::as_str),
        Some("analyze text from _source"),
        "{debug}"
    );
    assert_eq!(
        debug["extract_count"],
        json!(N),
        "extract_count must count every matched doc: {debug}"
    );
    assert_eq!(
        debug["values_fetched"],
        json!(N),
        "one string value per doc: {debug}"
    );
    assert_eq!(
        debug["chars_fetched"],
        json!(11 * N),
        "11 chars per doc: {debug}"
    );
    assert_eq!(
        debug["collect_analyzed_count"],
        json!(2 * N),
        "2 distinct tokens per doc: {debug}"
    );
    assert_eq!(
        debug["total_buckets"],
        json!(2),
        "token union across the single (no parent bucket) group: {debug}"
    );
}

/// The same walk must stay byte-identical on the small corpus the YAML suite
/// pins (7-doc shape): 7 docs of 11 chars → the historical exact answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn significant_text_profile_small_corpus_unchanged() {
    let (app, _dir) = app().await;
    seed_bulk(&app, "sig-small", 7).await;

    let (st, body) = call(
        &app,
        "POST",
        "/sig-small/_search",
        json!({
            "size": 0,
            "profile": true,
            "query": {"match_all": {}},
            "aggs": {"sig": {"significant_text": {"field": "t"}}},
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let debug = sig_debug(&body);
    assert_eq!(debug["extract_count"], json!(7), "{debug}");
    assert_eq!(debug["values_fetched"], json!(7), "{debug}");
    assert_eq!(debug["chars_fetched"], json!(77), "{debug}");
    assert_eq!(debug["collect_analyzed_count"], json!(14), "{debug}");
    assert_eq!(debug["total_buckets"], json!(2), "{debug}");
}

/// A selective foreground query folds only the matched docs — the fold's
/// membership must agree with the search path's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn significant_text_profile_honors_foreground_query() {
    let (app, _dir) = app().await;
    // 20 docs; the query matches only ids d00xx with an even prefix digit —
    // use a range on a numeric field for a portable selective query.
    let (st, _) = call(
        &app,
        "PUT",
        "/sig-sel",
        json!({"mappings": {"properties": {
            "t": {"type": "text"},
            "v": {"type": "long"},
        }}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let mut ndjson = String::new();
    for i in 0..20u64 {
        ndjson.push_str(&format!(
            "{{\"index\":{{\"_id\":\"d{i:02}\"}}}}\n{{\"t\":\"hello world\",\"v\":{i}}}\n"
        ));
    }
    let response = app
        .clone()
        .oneshot(
            Request::post("/sig-sel/_bulk")
                .header("content-type", "application/x-ndjson")
                .body(Body::from(ndjson))
                .unwrap(),
        )
        .await
        .expect("bulk");
    assert_eq!(response.status(), StatusCode::OK);
    call(&app, "POST", "/sig-sel/_refresh", Value::Null).await;

    let (st, body) = call(
        &app,
        "POST",
        "/sig-sel/_search",
        json!({
            "size": 0,
            "profile": true,
            "query": {"range": {"v": {"gte": 10}}},
            "aggs": {"sig": {"significant_text": {"field": "t"}}},
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let debug = sig_debug(&body);
    assert_eq!(
        debug["extract_count"],
        json!(10),
        "only the matched 10: {debug}"
    );
    assert_eq!(debug["values_fetched"], json!(10), "{debug}");
    assert_eq!(debug["chars_fetched"], json!(110), "{debug}");
    assert_eq!(debug["collect_analyzed_count"], json!(20), "{debug}");
}
