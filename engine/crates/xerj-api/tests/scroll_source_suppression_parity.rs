//! Issue #637 (follow-up to #624): a `_search?scroll=` continuation page must
//! suppress `_source` in the SAME cases the first page (`search_impl`) does, not
//! only for a request-level `_source: false`.
//!
//! #624 added `ScrollContext::source_disabled`, but captured it from
//! `matches!(body.source, Some(Value::Bool(false)))` ONLY. The first page omits
//! `_source` in three more cases:
//!   - mapping `_source.enabled: false`,
//!   - `stored_fields` implying suppression when `_source` is unspecified,
//!   - a null/absent stored source (this one is already per-hit on the
//!     continuation, so it is not a divergence).
//!
//! For the first two, a scroll opened against them omitted `_source` on the
//! first page but EMITTED it on every continuation page — a first-page/
//! continuation divergence. These tests pin the parity.

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

async fn seed(app: &axum::Router, index: &str, mappings: Value) {
    let (st, body) = call(app, "PUT", &format!("/{index}"), mappings).await;
    assert_eq!(st, StatusCode::OK, "create {index}: {body}");
    for (id, v) in [("d0", 10), ("d1", 11), ("d2", 12), ("d3", 13)] {
        let (st, _) = call(
            app,
            "POST",
            &format!("/{index}/_doc/{id}"),
            json!({"grp": "g", "v": v}),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "index {id}");
    }
    let _ = call(app, "POST", &format!("/{index}/_refresh"), Value::Null).await;
}

/// mapping `_source.enabled: false`: the first page omits `_source`; the
/// continuation must too (fails before the fix — the continuation emitted it).
#[tokio::test]
async fn scroll_continuation_omits_source_under_mapping_source_disabled() {
    let (app, _dir) = app().await;
    seed(
        &app,
        "docs",
        json!({"mappings": {
            "_source": {"enabled": false},
            "properties": {"grp": {"type": "keyword"}, "v": {"type": "long"}}
        }}),
    )
    .await;

    // Scroll, size 1, sorted v asc → first page 1 hit, continuation pages follow.
    let (st, first) = call(
        &app,
        "POST",
        "/docs/_search?scroll=5m",
        json!({"query": {"match_all": {}}, "size": 1, "sort": [{"v": "asc"}]}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "scroll start: {first}");
    // Baseline: mapping `_source.enabled:false` already omits `_source` on the
    // FIRST page (search_impl). If this ever regresses the divergence premise is
    // moot — assert it so the test stays honest.
    assert!(
        first["hits"]["hits"][0].get("_source").is_none(),
        "#637 premise: mapping _source.enabled:false omits _source on the first page: {first}"
    );
    let sid = first["_scroll_id"].as_str().expect("scroll id").to_string();

    let (st, page2) = call(
        &app,
        "POST",
        "/_search/scroll",
        json!({"scroll": "5m", "scroll_id": sid}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "continuation: {page2}");
    let hit = &page2["hits"]["hits"][0];
    assert!(
        hit.get("_id").is_some(),
        "#637: continuation should still return a hit: {page2}"
    );
    assert!(
        hit.get("_source").is_none(),
        "#637: scroll continuation must omit _source when the mapping sets \
         _source.enabled:false (first-page parity), got: {hit}"
    );
}

/// `stored_fields` without an explicit `_source`: the first page omits `_source`;
/// the continuation must too (fails before the fix).
#[tokio::test]
async fn scroll_continuation_omits_source_under_stored_fields() {
    let (app, _dir) = app().await;
    seed(
        &app,
        "docs",
        json!({"mappings": {
            "properties": {"grp": {"type": "keyword"}, "v": {"type": "long"}}
        }}),
    )
    .await;

    let (st, first) = call(
        &app,
        "POST",
        "/docs/_search?scroll=5m",
        json!({
            "query": {"match_all": {}},
            "size": 1,
            "sort": [{"v": "asc"}],
            "stored_fields": ["grp"]
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "scroll start: {first}");
    assert!(
        first["hits"]["hits"][0].get("_source").is_none(),
        "#637 premise: stored_fields (no _source) omits _source on the first page: {first}"
    );
    let sid = first["_scroll_id"].as_str().expect("scroll id").to_string();

    let (st, page2) = call(
        &app,
        "POST",
        "/_search/scroll",
        json!({"scroll": "5m", "scroll_id": sid}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "continuation: {page2}");
    let hit = &page2["hits"]["hits"][0];
    assert!(
        hit.get("_source").is_none(),
        "#637: scroll continuation must omit _source when the opening request set \
         stored_fields without _source (first-page parity), got: {hit}"
    );
}
