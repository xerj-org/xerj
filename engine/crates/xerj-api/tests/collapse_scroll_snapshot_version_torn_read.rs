//! Issue #566 (items 2 & 3): the collapse `inner_hits` render must carry each
//! member's SNAPSHOT `_seq_no`/`_version`, not a live lookup at render time.
//! The #506 repro (`collapse_inner_hits_seqno_is_real.rs`) is a plain
//! `POST /_search`, where snapshot == live (docs are versioned once at search
//! time), so it cannot distinguish the two. This test holds a collapse scroll
//! across an update: a member is bumped AFTER the scroll snapshot but BEFORE the
//! continuation page that renders its inner hit, so a live lookup would surface
//! the post-update `_version`/`_seq_no` beside the snapshot `_source` — the torn
//! read #499/#506 describe, on the collapse path.
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

/// A collapse scroll whose continuation page renders a member that was updated
/// after the snapshot must report the member's SNAPSHOT `_version`/`_seq_no`,
/// not the live (bumped) one (#566 items 2 & 3).
///
/// `#[ignore]`d reproduction of #621: `scroll_page_response` never renders the
/// collapse group (unlike `search_impl`), so it leaks `__xy_collapse_group__` /
/// `__xy_collapse_spec__` into the wire `_source` and emits no `inner_hits`.
/// The snapshot values ARE carried correctly (the sentinel shows the pre-update
/// `_version`), so the fix is purely to port the collapse render into the scroll
/// continuation path. Un-ignore when #621 lands.
#[ignore = "#621: scroll_page_response does not render the collapse group; leaks sentinels"]
#[tokio::test]
async fn collapse_scroll_inner_hit_reports_snapshot_version_not_live() {
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

    // Two collapse groups. Group "A" sorts before group "B" (grp asc), so with
    // page_size 1 the scroll returns A's leader on page 1 and B's leader on
    // page 2 — the page rendered AFTER the update below.
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
            json!({ "grp": grp, "v": v }),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "index {id}");
    }
    let _ = call(&app, "POST", "/docs/_refresh", Value::Null).await;

    // Open a collapse scroll, one group per page, sorted by grp asc. Inner hits
    // request version + seq_no so the render carries the snapshot pair.
    let (st, first) = call(
        &app,
        "POST",
        "/docs/_search?scroll=5m",
        json!({
            "query": { "match_all": {} },
            "size": 1,
            "sort": [{ "grp": "asc" }],
            "collapse": { "field": "grp", "inner_hits": {
                "name": "members", "size": 10,
                "sort": [{ "v": "asc" }],
                "seq_no_primary_term": true,
                "version": true
            } }
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "collapse scroll start: {first}");
    let sid = first["_scroll_id"].as_str().expect("scroll id").to_string();
    assert_eq!(
        first["hits"]["hits"][0]["_source"]["grp"], "A",
        "page 1 must be group A: {first}"
    );

    // Concurrent update AFTER the snapshot: bump b1's `v`, seq_no and version.
    let (st, upd) = call(
        &app,
        "PUT",
        "/docs/_doc/b1",
        json!({ "grp": "B", "v": 999 }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "concurrent update b1: {upd}");
    let live_version_after = upd["_version"].as_i64().expect("_version on update");
    assert!(live_version_after >= 2, "update must bump version: {upd}");

    // Continue to page 2 (group B), rendered AFTER the update.
    let (st, page2) = call(
        &app,
        "POST",
        "/_search/scroll",
        json!({ "scroll": "5m", "scroll_id": sid }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "scroll continuation: {page2}");
    let leader = &page2["hits"]["hits"][0];
    assert_eq!(
        leader["_source"]["grp"], "B",
        "page 2 must be group B: {page2}"
    );
    // The internal collapse sentinels must NOT leak into the wire `_source`:
    // `search_impl` strips them when it renders inner_hits; the scroll
    // continuation path (`scroll_page_response`) must do the same (#566).
    assert!(
        leader["_source"].get("__xy_collapse_group__").is_none()
            && leader["_source"].get("__xy_collapse_spec__").is_none(),
        "collapse scroll continuation leaked internal sentinels into _source — \
         scroll_page_response does not render the collapse group like search_impl: {leader}"
    );

    // Find b1's inner hit on the group-B leader.
    let inner = page2
        .pointer("/hits/hits/0/inner_hits/members/hits/hits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let b1 = inner
        .iter()
        .find(|h| h["_id"] == "b1")
        .unwrap_or_else(|| panic!("b1 must be a group-B inner hit: {page2}"));

    // The snapshot `_source` (v=21, pre-update) must be paired with the snapshot
    // `_version`/`_seq_no`, NOT the live post-update values.
    assert_eq!(
        b1["_source"]["v"].as_i64(),
        Some(21),
        "b1 inner-hit _source must be the SNAPSHOT body (v=21), got {b1}"
    );
    assert_eq!(
        b1["_version"].as_i64(),
        Some(1),
        "b1 inner-hit _version must be the SNAPSHOT version (1), not the live post-update \
         value {live_version_after} — a torn read a client could replay into if_seq_no (#566): {b1}"
    );
}
