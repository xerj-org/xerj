//! Issue #630: the `POST /{index}/_search_scroll` (alias-route scroll) FIRST page
//! must carry `_seq_no`/`_version`/`_primary_term` when the request asks for them.
//!
//! The continuation path (`scroll_page_response`) emits them (#428), and the
//! `_search?scroll=` first page (rendered by `search_impl`) emits them too, but
//! `search_with_scroll`'s first-page response is hand-built and only serialized
//! `_index`/`_id`/`_score`/`_source`(+`fields`) — the `EsHit` carried the
//! versioning pair, but the map never read it out. So a client that opens a
//! scroll via this route with `seq_no_primary_term`/`version` got them on every
//! CONTINUATION page but not the first — a wire divergence from ES.
//!
//! ES is referenced for wire semantics only; no ES code is here.

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

/// A scroll opened via `_search_scroll` must carry the requested versioning pair
/// on its FIRST page, matching the continuation page and the `_search?scroll=`
/// first page (#630).
#[tokio::test]
async fn search_scroll_first_page_carries_seq_no_and_version_when_requested() {
    let (app, _dir) = app().await;
    let (st, _) = call(&app, "POST", "/docs/_doc/d1", json!({ "v": 1 })).await;
    assert_eq!(st, StatusCode::CREATED, "index d1");
    let _ = call(&app, "POST", "/docs/_refresh", Value::Null).await;

    // Open a scroll via the alias route, requesting the versioning pair.
    let (st, first) = call(
        &app,
        "POST",
        "/docs/_search_scroll?scroll=1m",
        json!({
            "query": { "match_all": {} },
            "size": 10,
            "seq_no_primary_term": true,
            "version": true
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "search_scroll first page: {first}");
    let hit = &first["hits"]["hits"][0];
    assert_eq!(hit["_id"], "d1", "first page returns d1: {first}");

    // #630: the first page must carry the requested versioning pair — and the
    // REAL engine values, not fabricated ones. Cross-check against a plain GET,
    // the ground truth (the same read the engine resolves `_source` from). This
    // catches both the omission bug AND any future value-fabrication regression.
    assert!(
        hit.get("_seq_no").is_some()
            && hit.get("_version").is_some()
            && hit.get("_primary_term").is_some(),
        "first page must carry the full versioning triple when requested (#630): {hit}"
    );
    let (st, got) = call(&app, "GET", "/docs/_doc/d1", Value::Null).await;
    assert_eq!(st, StatusCode::OK, "GET d1 ground truth: {got}");
    assert_eq!(
        hit["_seq_no"], got["_seq_no"],
        "first-page _seq_no must be the real value, not fabricated (#630): first={hit} got={got}"
    );
    assert_eq!(
        hit["_version"], got["_version"],
        "first-page _version must be the real value, not fabricated (#630): first={hit} got={got}"
    );
    // `_primary_term` is the engine's placeholder alongside a real `_seq_no`.
    assert_eq!(
        hit["_primary_term"],
        json!(1),
        "first-page _primary_term must be 1 alongside a real _seq_no (#630): {hit}"
    );
}
