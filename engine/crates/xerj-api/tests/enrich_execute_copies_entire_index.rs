//! Issue #1022: `POST /_enrich/policy/{name}/_execute` truncates its source
//! copy at 10 000 documents per source index.
//!
//! The executor ran ONE search per source index — `{"query":{"match_all":{}},
//! "size":10000,"_source":true}` — and copied whatever came back into the
//! system index `.enrich-<name>`. With more than 10 000 docs in a source
//! index the documents past the first page were silently never materialised,
//! so every later enrich lookup against those keys silently misses (enrich
//! correctness is keyed on completeness of the copy), and `records`
//! under-reports.
//!
//! This endpoint is a reindex into a system index, so the fix adopts
//! reindex's exact `_id`-keyset `search_after` loop. ES `_execute` has no
//! `max_docs`/`scroll_size` wire parameters — none are invented here.
//!
//! Fails before the fix at `records` == 10 000 with the `.enrich-` index
//! holding 10 000 of 10 500 documents.

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

/// Seed `n` docs `{"k": "key-i", "payload": i}` with zero-padded ids so the
/// `_id`-keyset copy pages deterministically.
async fn seed_bulk(app: &axum::Router, index: &str, n: u64, offset: u64) {
    let (st, _) = call(
        app,
        "PUT",
        &format!("/{index}"),
        json!({"mappings": {"properties": {
            "k": {"type": "keyword"},
            "payload": {"type": "long"},
        }}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create {index}");

    let width = (offset + n).to_string().len();
    let chunk = 2_500u64;
    let mut start = 0u64;
    while start < n {
        let end = (start + chunk).min(n);
        let mut ndjson = String::new();
        for i in start..end {
            let gid = offset + i;
            ndjson.push_str(&format!(
                "{{\"index\":{{\"_id\":\"d{gid:0width$}\"}}}}\n{{\"k\":\"key-{gid}\",\"payload\":{gid}}}\n",
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

async fn count(app: &axum::Router, index: &str) -> u64 {
    let (st, body) = call(
        app,
        "POST",
        &format!("/{index}/_count"),
        json!({"query": {"match_all": {}}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "_count {index}: {body}");
    body["count"].as_u64().unwrap_or(u64::MAX)
}

/// Executing a policy over a 10 500-doc source must materialise ALL of them
/// into `.enrich-<name>` — the copy is a lookup table, and a key missing from
/// it is a lookup that silently misses forever. Fails before the fix at
/// `records`/count == 10 000.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enrich_execute_copies_past_ten_thousand() {
    let (app, _dir) = app().await;
    const N: u64 = 10_500;
    seed_bulk(&app, "enrich-src", N, 0).await;

    let (st, body) = call(
        &app,
        "PUT",
        "/_enrich/policy/pol-big",
        json!({"match": {
            "indices": "enrich-src",
            "match_field": "k",
            "enrich_fields": ["payload"],
        }}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");

    let (st, body) = call(
        &app,
        "POST",
        "/_enrich/policy/pol-big/_execute",
        Value::Null,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(
        body["records"],
        json!(N),
        "records must be the exact source count, not the page length: {body}"
    );

    assert_eq!(
        count(&app, ".enrich-pol-big").await,
        N,
        "the enrich index must hold every source document"
    );
    // Completeness is keyed: the LAST document of the _id order (the one the
    // truncation dropped) must be present with its payload.
    let (st, body) = call(
        &app,
        "POST",
        "/.enrich-pol-big/_search",
        json!({"query": {"term": {"k": "key-10499"}}, "size": 1}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(
        body["hits"]["hits"][0]["_source"]["payload"],
        json!(10_499),
        "the final source doc must be materialised with its fields"
    );
}

/// A policy naming multiple source indices copies each one completely; a
/// missing source index is skipped, not fatal (existing ES-tolerated
/// behavior, pinned here so the paged rewrite keeps it).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enrich_execute_copies_every_source_index_and_skips_missing() {
    let (app, _dir) = app().await;
    seed_bulk(&app, "enrich-a", 300, 0).await;
    seed_bulk(&app, "enrich-b", 200, 1_000).await;

    let (st, body) = call(
        &app,
        "PUT",
        "/_enrich/policy/pol-multi",
        json!({"match": {
            "indices": ["enrich-a", "enrich-b", "enrich-missing"],
            "match_field": "k",
            "enrich_fields": ["payload"],
        }}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");

    let (st, body) = call(
        &app,
        "POST",
        "/_enrich/policy/pol-multi/_execute",
        Value::Null,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["records"], json!(500), "{body}");
    assert_eq!(count(&app, ".enrich-pol-multi").await, 500);
}
