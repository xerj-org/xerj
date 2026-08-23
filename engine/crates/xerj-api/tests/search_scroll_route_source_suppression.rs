//! Issue #659 (follow-up to #637): the `/:index/_search_scroll` route
//! (`search_with_scroll`) emits `_source` UNCONDITIONALLY on its first page — it
//! ignores request `_source: false`, mapping `_source.enabled: false`, and
//! `stored_fields`-implied suppression that `search_impl` (`_search?scroll=`)
//! honours. #637 already made the SHARED continuation renderer suppress these
//! per route; this issue brings THIS route's first page into the same parity so
//! it is both ES-correct and self-consistent (first page and continuation agree).

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

/// mapping `_source.enabled: false` via the `_search_scroll` route: the first
/// page must OMIT `_source` (matching `search_impl`). Fails before the fix — the
/// route emits `_source` unconditionally.
#[tokio::test]
async fn search_scroll_firstpage_omits_source_under_mapping_disabled() {
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

    let (st, first) = call(
        &app,
        "POST",
        "/docs/_search_scroll?scroll=5m",
        json!({"query": {"match_all": {}}, "size": 1, "sort": [{"v": "asc"}]}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "search_scroll open: {first}");
    assert!(
        first["hits"]["hits"][0].get("_source").is_none(),
        "#659: _search_scroll first page must omit _source when the mapping sets \
         _source.enabled:false (search_impl parity), got: {first}"
    );
}

/// request `_source: false` via the `_search_scroll` route: the first page must
/// OMIT `_source`. Fails before the fix (the route emits it, then the
/// continuation omits it — a pre-existing first-page/continuation split).
#[tokio::test]
async fn search_scroll_firstpage_omits_source_under_source_false() {
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
        "/docs/_search_scroll?scroll=5m",
        json!({"query": {"match_all": {}}, "size": 1, "_source": false, "sort": [{"v": "asc"}]}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "search_scroll open: {first}");
    assert!(
        first["hits"]["hits"][0].get("_source").is_none(),
        "#659: _search_scroll first page must omit _source under _source:false, got: {first}"
    );
}

/// After the fix, the route stays SELF-CONSISTENT: first page and continuation
/// agree (both omit) for a mapping-disabled index. Guards the parity #637 cared
/// about while #659 makes both pages correct.
#[tokio::test]
async fn search_scroll_firstpage_and_continuation_both_omit_under_mapping_disabled() {
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

    let (st, first) = call(
        &app,
        "POST",
        "/docs/_search_scroll?scroll=5m",
        json!({"query": {"match_all": {}}, "size": 1, "sort": [{"v": "asc"}]}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "open: {first}");
    let first_has = first["hits"]["hits"][0].get("_source").is_some();
    let sid = first["_scroll_id"].as_str().expect("scroll id").to_string();

    let (st, page2) = call(
        &app,
        "POST",
        "/_search/scroll",
        json!({"scroll": "5m", "scroll_id": sid}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "continuation: {page2}");
    let cont_has = page2["hits"]["hits"][0].get("_source").is_some();

    assert!(
        !first_has && !cont_has,
        "#659: _search_scroll first page AND continuation must both omit _source for a \
         mapping-disabled index (first={first_has}, continuation={cont_has}); \
         page1={first} page2={page2}"
    );
}
