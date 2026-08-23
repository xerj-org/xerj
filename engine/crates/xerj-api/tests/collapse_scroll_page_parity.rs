//! Issue #623: the collapse `inner_hits` a client sees for a given group must be
//! byte-identical whether that group's leader is rendered on the FIRST page (by
//! `search_impl`, plain `_search`) or on a scroll CONTINUATION page (by
//! `scroll_page_response`). Both now route through the shared
//! `render_collapse_inner_hits` (#621/#622), so this pins them to each other —
//! the #622 verification recommended committing exactly this guard, since the
//! PR's own test only checked the continuation page in isolation.
//!
//! Elasticsearch is referenced for wire semantics only; no ES code is here.

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

/// Extract the `inner_hits` object of the hit whose `_source.grp == grp`.
fn inner_for_group(page: &Value, grp: &str) -> Value {
    page["hits"]["hits"]
        .as_array()
        .expect("hits array")
        .iter()
        .find(|h| h["_source"]["grp"] == grp)
        .unwrap_or_else(|| panic!("group {grp} leader not present: {page}"))
        .get("inner_hits")
        .cloned()
        .unwrap_or_else(|| panic!("group {grp} leader has no inner_hits: {page}"))
}

/// The `inner_hits` for collapse group "B" must be identical whether B's leader
/// is rendered by `search_impl` (first page) or `scroll_page_response`
/// (continuation) — a rich spec: inner sort, size/from, `fields`, and
/// `seq_no_primary_term`/`version` (#623).
#[tokio::test]
async fn collapse_inner_hits_are_identical_on_first_and_continuation_pages() {
    let (app, _dir) = app().await;
    let (st, body) = call(
        &app,
        "PUT",
        "/docs",
        json!({"mappings": {"properties": {
            "grp": { "type": "keyword" },
            "v": { "type": "long" }
        }}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create index: {body}");

    // Group A (2 members) sorts before group B (3 members) under `grp asc`.
    for (id, grp, v) in [
        ("a0", "A", 10),
        ("a1", "A", 11),
        ("b0", "B", 20),
        ("b1", "B", 21),
        ("b2", "B", 22),
    ] {
        let (st, _) = call(
            &app,
            "POST",
            &format!("/docs/_doc/{id}"),
            json!({ "grp": grp, "v": v }),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "index {id}");
    }
    let _ = call(&app, "POST", "/docs/_refresh", Value::Null).await;

    // A rich collapse spec, reused verbatim on both paths.
    let collapse = json!({
        "field": "grp",
        "inner_hits": {
            "name": "members",
            "size": 2,
            "from": 1,
            "sort": [{ "v": "desc" }],
            "fields": ["v"],
            "seq_no_primary_term": true,
            "version": true
        }
    });

    // Path A — first page: plain `_search`, size 10 → both leaders (A then B).
    let (st, plain) = call(
        &app,
        "POST",
        "/docs/_search",
        json!({
            "query": { "match_all": {} },
            "size": 10,
            "sort": [{ "grp": "asc" }],
            "collapse": collapse
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "plain collapse search: {plain}");
    let first_page_b = inner_for_group(&plain, "B");

    // Path B — continuation page: collapse scroll, size 1 → page 1 = A, page 2 = B.
    let (st, first) = call(
        &app,
        "POST",
        "/docs/_search?scroll=5m",
        json!({
            "query": { "match_all": {} },
            "size": 1,
            "sort": [{ "grp": "asc" }],
            "collapse": collapse
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "collapse scroll start: {first}");
    let sid = first["_scroll_id"].as_str().expect("scroll id").to_string();
    assert_eq!(
        first["hits"]["hits"][0]["_source"]["grp"], "A",
        "scroll page 1 must be group A: {first}"
    );
    let (st, page2) = call(
        &app,
        "POST",
        "/_search/scroll",
        json!({ "scroll": "5m", "scroll_id": sid }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "scroll continuation: {page2}");
    let continuation_b = inner_for_group(&page2, "B");

    // The heart of #623: the two renders of group B's inner_hits must match.
    assert_eq!(
        first_page_b, continuation_b,
        "collapse inner_hits for group B diverge between the first page (search_impl) \
         and the scroll continuation (scroll_page_response):\n first_page={first_page_b}\n continuation={continuation_b}"
    );
    // And they must be non-empty (guards against a vacuous match of two empties).
    assert!(
        continuation_b.pointer("/members/hits/hits/0").is_some(),
        "group B inner_hits must render members: {continuation_b}"
    );
}
