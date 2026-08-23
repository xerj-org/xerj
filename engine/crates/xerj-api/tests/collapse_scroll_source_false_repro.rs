//! Issue #624: a collapse `_search?scroll=` continuation under `_source: false`.
//!
//! Two facts, established by reproduction:
//!   1. `inner_hits` DO render (the scroll snapshot is `merged_hits.clone()` taken
//!      BEFORE `_source` suppression, so the context keeps the sentinel-laden
//!      source) — #622's cautious "no inner_hits" scoping note was wrong.
//!   2. The real bug: the continuation used to EMIT `_source` even though the
//!      opening request set `_source: false` — a divergence from ES and from the
//!      `_search?scroll=` first page (search_impl). Fixed by capturing
//!      `source_disabled` once at scroll-open (`ScrollContext`) and omitting
//!      `_source` on continuation pages, while still rendering `inner_hits`.

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

#[tokio::test]
async fn collapse_scroll_source_false_still_renders_inner_hits() {
    let (app, _dir) = app().await;
    let (st, _) = call(
        &app,
        "PUT",
        "/docs",
        json!({"mappings": {"properties": {"grp": {"type": "keyword"}, "v": {"type": "long"}}}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    for (id, grp, v) in [
        ("a0", "A", 10),
        ("a1", "A", 11),
        ("b0", "B", 20),
        ("b1", "B", 21),
    ] {
        let (st, _) = call(
            &app,
            "POST",
            &format!("/docs/_doc/{id}"),
            json!({"grp": grp, "v": v}),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "index {id}");
    }
    let _ = call(&app, "POST", "/docs/_refresh", Value::Null).await;

    // Collapse scroll, size 1, sorted grp asc, WITH `_source: false`.
    let (st, first) = call(
        &app,
        "POST",
        "/docs/_search?scroll=5m",
        json!({
            "query": {"match_all": {}},
            "size": 1,
            "_source": false,
            "sort": [{"grp": "asc"}],
            "collapse": {"field": "grp", "inner_hits": {"name": "members", "size": 10, "sort": [{"v": "asc"}]}}
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "collapse scroll start: {first}");
    let sid = first["_scroll_id"].as_str().expect("scroll id").to_string();

    // Continue to page 2 (group B).
    let (st, page2) = call(
        &app,
        "POST",
        "/_search/scroll",
        json!({"scroll": "5m", "scroll_id": sid}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "continuation: {page2}");
    let leader = &page2["hits"]["hits"][0];

    // #624 finding: inner_hits DO render under _source:false (the scroll snapshot
    // retains the sentinel-laden source), so the filed "no inner_hits" premise is
    // wrong — assert they render (a guard for that good behavior).
    assert!(
        leader.pointer("/inner_hits/members/hits/hits").is_some(),
        "#624: collapse scroll continuation renders inner_hits even under _source:false: {leader}"
    );
    // The REAL bug: under `_source: false` the continuation must OMIT `_source`
    // entirely (ES does; the `_search?scroll=` first page does via search_impl).
    // The scroll continuation currently emits it — this assertion fails before
    // the fix.
    assert!(
        leader.get("_source").is_none(),
        "#624: scroll continuation must omit _source under _source:false (ES parity), got: {leader}"
    );
}
