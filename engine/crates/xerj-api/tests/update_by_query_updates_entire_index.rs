//! Issue #1022: `_update_by_query` truncates at 10 000 documents per call.
//!
//! The handler built ONE search body — `{"query": …, "size": 10000, "from":
//! 0}` — transformed whatever came back, and reported `total` as the page
//! length with `batches` hardcoded to 1. Under the two real uses of the
//! endpoint (mass scripted migration, mapping-change pickup via the no-script
//! re-index-in-place) exactly the documents past the first page kept their
//! stale values while the call reported success with `total == 10 000`.
//!
//! ES semantics (8.18 docs-update-by-query): no default document cap — the
//! whole match set is processed in `scroll_size` batches (default 1 000)
//! unless `max_docs` limits it; `total` is the exact match count (the
//! snapshot the operation started from), `batches` the real number of scroll
//! responses pulled back.
//!
//! These tests go through the real HTTP route. They fail before the fix at
//! `total`/`updated` == 10 000 with docs 10 000..N left untouched.
//!
//! Elasticsearch is referenced for wire semantics only (approach-only per the
//! licence rules); no ES code is reproduced here.

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

/// Seed `n` docs `{"v": i, "n": 0}` into `index` via `_bulk` (chunked so no
/// single request body is enormous). Ids are zero-padded so lexicographic
/// `_id` order is deterministic — the paginated runner pages by `_id` keyset.
async fn seed_bulk(app: &axum::Router, index: &str, n: u64) {
    let (st, _) = call(
        app,
        "PUT",
        &format!("/{index}"),
        json!({"mappings": {"properties": {
            "v": {"type": "long"},
            "n": {"type": "long"},
            "touched": {"type": "long"},
        }}}),
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
                "{{\"index\":{{\"_id\":\"d{i:0width$}\"}}}}\n{{\"v\":{i},\"n\":0}}\n",
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
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "bulk chunk {start}..{end}"
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).expect("bulk body");
        assert_eq!(
            body["errors"],
            json!(false),
            "bulk chunk {start}..{end}: {body}"
        );
        start = end;
    }
    let (st, _) = call(app, "POST", &format!("/{index}/_refresh"), Value::Null).await;
    assert_eq!(st, StatusCode::OK, "refresh {index}");
}

async fn count_query(app: &axum::Router, index: &str, query: Value) -> u64 {
    let (st, body) = call(
        app,
        "POST",
        &format!("/{index}/_count"),
        json!({"query": query}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "_count {index}: {body}");
    body["count"].as_u64().unwrap_or(u64::MAX)
}

/// One scripted `_update_by_query` over 25 000 matching docs must update ALL
/// of them — no 10k truncation, `total` the exact match count, `batches` the
/// real page count, and every document carrying the scripted mutation.
/// Fails before the fix at total/updated == 10 000 with 15 000 docs left
/// `touched`-less.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_by_query_scripts_past_ten_thousand_in_one_call() {
    let (app, _dir) = app().await;
    const N: u64 = 25_000;
    seed_bulk(&app, "ubq-big", N).await;
    assert_eq!(
        count_query(&app, "ubq-big", json!({"match_all": {}})).await,
        N,
        "seeded"
    );

    let (st, body) = call(
        &app,
        "POST",
        "/ubq-big/_update_by_query",
        json!({
            "query": {"match_all": {}},
            "script": {"source": "ctx._source.touched = 1"},
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(
        body["total"],
        json!(N),
        "total must be the exact match count, not the page length: {body}"
    );
    assert_eq!(
        body["updated"],
        json!(N),
        "every matching doc updated by ONE call: {body}"
    );
    let batches = body["batches"].as_u64().expect("batches");
    assert!(
        batches >= 3,
        "a 25k run at the default scroll_size of 1000 must pull real batches, got {batches}: {body}"
    );
    assert_eq!(body["failures"], json!([]), "{body}");

    assert_eq!(
        count_query(&app, "ubq-big", json!({"term": {"touched": 1}})).await,
        N,
        "every document must carry the scripted mutation"
    );
}

/// The no-script form (re-index-in-place, the pick-up-mapping-changes use)
/// must equally cover the whole match set — it pages with per-page source
/// hydration instead of the ids-only projection, and the exactness contract
/// is the same. Fails before the fix at total/updated == 10 000.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_by_query_reindexes_past_ten_thousand_in_one_call() {
    let (app, _dir) = app().await;
    const N: u64 = 25_000;
    seed_bulk(&app, "ubq-noscript", N).await;

    let (st, body) = call(
        &app,
        "POST",
        "/ubq-noscript/_update_by_query",
        json!({"query": {"match_all": {}}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["total"], json!(N), "{body}");
    assert_eq!(body["updated"], json!(N), "{body}");
    let batches = body["batches"].as_u64().expect("batches");
    assert!(batches >= 3, "real batch count, got {batches}: {body}");
    assert_eq!(body["failures"], json!([]), "{body}");

    assert_eq!(
        count_query(&app, "ubq-noscript", json!({"match_all": {}})).await,
        N,
        "re-index-in-place must not lose documents"
    );
    // The re-index preserves the source: a term from the original docs still
    // matches exactly N documents.
    assert_eq!(
        count_query(&app, "ubq-noscript", json!({"range": {"v": {"gte": 0}}})).await,
        N,
        "re-index-in-place must preserve field values"
    );
}

/// ES `max_docs`: "Maximum number of documents to process. Defaults to all
/// documents." Exactly `max_docs` documents are updated, `total` reports the
/// processed count, and a `max_docs <= scroll_size` run is a single batch
/// (the documented no-scroll fast path).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_by_query_honors_max_docs() {
    let (app, _dir) = app().await;
    seed_bulk(&app, "ubq-max", 20).await;

    let (st, body) = call(
        &app,
        "POST",
        "/ubq-max/_update_by_query",
        json!({
            "query": {"match_all": {}},
            "max_docs": 7,
            "script": {"source": "ctx._source.touched = 1"},
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(
        body["total"],
        json!(7),
        "with max_docs set, total is the processed count: {body}"
    );
    assert_eq!(body["updated"], json!(7), "{body}");
    assert_eq!(
        body["batches"],
        json!(1),
        "max_docs <= scroll_size is a single batch: {body}"
    );

    assert_eq!(
        count_query(&app, "ubq-max", json!({"term": {"touched": 1}})).await,
        7,
        "exactly max_docs documents updated"
    );
}

/// ES `scroll_size` (default 1 000): the size of the batch that powers the
/// operation — `batches` is the number of batches actually pulled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_by_query_reports_real_batch_count() {
    let (app, _dir) = app().await;
    seed_bulk(&app, "ubq-scroll", 10).await;

    let (st, body) = call(
        &app,
        "POST",
        "/ubq-scroll/_update_by_query",
        json!({
            "query": {"match_all": {}},
            "scroll_size": 3,
            "script": {"source": "ctx._source.touched = 1"},
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["total"], json!(10), "{body}");
    assert_eq!(body["updated"], json!(10), "{body}");
    assert_eq!(
        body["batches"],
        json!(4),
        "10 docs at scroll_size 3 = 4 batches: {body}"
    );
    assert_eq!(
        count_query(&app, "ubq-scroll", json!({"term": {"touched": 1}})).await,
        10,
    );
}

/// A selective query must page exactly like match_all: every matching doc
/// updated, non-matching untouched, `total` the exact match count.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_by_query_paginates_selective_queries() {
    let (app, _dir) = app().await;
    seed_bulk(&app, "ubq-sel", 30).await;

    let (st, body) = call(
        &app,
        "POST",
        "/ubq-sel/_update_by_query",
        json!({
            "query": {"range": {"v": {"gte": 10}}},
            "scroll_size": 4,
            "script": {"source": "ctx._source.touched = 1"},
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["total"], json!(20), "docs with v >= 10: {body}");
    assert_eq!(body["updated"], json!(20), "{body}");
    assert_eq!(
        body["batches"],
        json!(5),
        "20 matches at scroll_size 4 = 5 batches: {body}"
    );

    assert_eq!(
        count_query(&app, "ubq-sel", json!({"term": {"touched": 1}})).await,
        20,
        "exactly the matched docs updated"
    );
}

/// MID-RUN REWRITE HAZARD (the one thing `_update_by_query` has that
/// `_delete_by_query` did not): the updates themselves repopulate the
/// memtable, and a later page's search must not page over unflushed state —
/// a rewritten doc with `_id <= cursor` resurfacing on a later page would
/// apply the script TWICE (silent corruption, versus delete's idempotent
/// duplicate). A counter script over a paged run must increment each doc
/// exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_by_query_applies_script_exactly_once_per_doc_when_paged() {
    let (app, _dir) = app().await;
    const N: u64 = 2_500;
    seed_bulk(&app, "ubq-once", N).await;

    let (st, body) = call(
        &app,
        "POST",
        "/ubq-once/_update_by_query",
        json!({
            "query": {"match_all": {}},
            "scroll_size": 100,
            "script": {"source": "ctx._source.n += 1"},
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["total"], json!(N), "{body}");
    assert_eq!(body["updated"], json!(N), "{body}");
    assert_eq!(
        body["batches"],
        json!(25),
        "2500 docs at scroll_size 100 = 25 batches: {body}"
    );
    assert_eq!(body["failures"], json!([]), "{body}");

    assert_eq!(
        count_query(&app, "ubq-once", json!({"term": {"n": 1}})).await,
        N,
        "every doc incremented exactly once"
    );
    assert_eq!(
        count_query(&app, "ubq-once", json!({"term": {"n": 0}})).await,
        0,
        "no doc left un-incremented"
    );
    assert_eq!(
        count_query(&app, "ubq-once", json!({"range": {"n": {"gte": 2}}})).await,
        0,
        "no doc double-applied"
    );
}
