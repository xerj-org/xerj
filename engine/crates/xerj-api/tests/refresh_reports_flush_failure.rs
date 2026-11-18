//! A refresh is a synchronous visibility barrier: its HTTP response must not
//! claim success when the engine could not publish the requested segment.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn request(app: &axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let body = serde_json::from_slice(&bytes).expect("JSON response");
    (status, body)
}

fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("data dir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let app = xerj_api::router::build_es_compat_router(xerj_api::state::AppState::new(
        config, engine, metrics,
    ));
    (app, dir)
}

async fn create_text_index_with_document(app: &axum::Router, name: &str) {
    let (status, body) = request(
        app,
        "PUT",
        &format!("/{name}"),
        json!({"mappings": {"properties": {"body": {"type": "text"}}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create {name}: {body}");

    let (status, body) = request(
        app,
        "PUT",
        &format!("/{name}/_doc/1"),
        json!({"body": "must remain searchable after a successful refresh"}),
    )
    .await;
    assert!(status.is_success(), "index document: {status} {body}");
}

fn block_segment_publication(dir: &std::path::Path, name: &str) {
    let segments = dir.join(name).join("segments");
    std::fs::remove_dir(&segments).expect("empty segments directory");
    std::fs::write(&segments, b"not a directory").expect("segments blocker");
}

#[tokio::test]
async fn refresh_reports_a_segment_publication_failure() {
    let (app, dir) = app();
    create_text_index_with_document(&app, "refresh-failure").await;

    // Make segment publication fail independently of any particular encoder:
    // replacing the still-empty segments directory with a regular file makes
    // the real storage/FTS finalizer return an I/O error. This is deterministic
    // even as individual side-car filename bugs are fixed.
    block_segment_publication(dir.path(), "refresh-failure");

    let (status, body) = request(&app, "POST", "/refresh-failure/_refresh", json!({})).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "an all-shards refresh failure must not be HTTP 200: {body}"
    );
    assert_eq!(body.pointer("/_shards/total"), Some(&json!(1)), "{body}");
    assert_eq!(
        body.pointer("/_shards/successful"),
        Some(&json!(0)),
        "{body}"
    );
    assert_eq!(body.pointer("/_shards/failed"), Some(&json!(1)), "{body}");
    assert_eq!(
        body.pointer("/_shards/failures/0/index"),
        Some(&json!("refresh-failure")),
        "{body}"
    );
    assert_eq!(
        body.pointer("/_shards/failures/0/status"),
        Some(&json!("INTERNAL_SERVER_ERROR")),
        "{body}"
    );
    let failure = body
        .pointer("/_shards/failures/0")
        .and_then(Value::as_object)
        .expect("failure object");
    let mut failure_fields: Vec<&str> = failure.keys().map(String::as_str).collect();
    failure_fields.sort_unstable();
    assert_eq!(
        failure_fields,
        ["index", "reason", "shard", "status"],
        "ES 8.13.4 DefaultShardOperationFailedException shape: {body}"
    );
    assert_eq!(
        body.pointer("/_shards/failures/0/reason/type"),
        Some(&json!("store_exception")),
        "{body}"
    );
    let reason = body
        .pointer("/_shards/failures/0/reason/reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        !reason.is_empty() && reason != "refresh failed" && reason.contains("I/O error:"),
        "segment publication must preserve the real storage I/O reason: {body}"
    );
}

#[tokio::test]
async fn multi_index_refresh_attempts_every_index_after_a_failure() {
    let (app, dir) = app();
    create_text_index_with_document(&app, "refresh-good").await;
    create_text_index_with_document(&app, "refresh-bad").await;
    block_segment_publication(dir.path(), "refresh-bad");

    // The broken index is explicitly first: `successful == 1` proves the
    // handler did not abort before attempting the second resolved index.
    let (status, body) = request(
        &app,
        "POST",
        "/refresh-bad,refresh-good/_refresh",
        json!({}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "ES 8.13.4 refresh uses the first failed shard's status: {body}"
    );
    assert_eq!(body.pointer("/_shards/total"), Some(&json!(2)), "{body}");
    assert_eq!(
        body.pointer("/_shards/successful"),
        Some(&json!(1)),
        "{body}"
    );
    assert_eq!(body.pointer("/_shards/failed"), Some(&json!(1)), "{body}");
    assert_eq!(
        body.pointer("/_shards/failures/0/index"),
        Some(&json!("refresh-bad")),
        "{body}"
    );
}

/// #2234 — an already-searchable index must refresh without the durability
/// tail. `Index::flush` (what `_refresh` used to call) always runs verified
/// WAL maintenance, which renames a new `snapshot.json` into place. Replacing
/// that file with a directory makes the rename fail. `_refresh` of an empty
/// memtable must still succeed; `_flush` must still report the failure.
#[tokio::test]
async fn refresh_does_not_wait_on_wal_maintenance() {
    let (app, dir) = app();
    create_text_index_with_document(&app, "refresh-ready").await;

    let (status, body) = request(&app, "POST", "/refresh-ready/_refresh", json!({})).await;
    assert!(
        status.is_success(),
        "the first refresh publishes the document: {status} {body}"
    );
    let (status, body) = request(&app, "GET", "/refresh-ready/_count", json!({})).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body.get("count"), Some(&json!(1)), "{body}");

    let snap = dir.path().join("refresh-ready").join("snapshot.json");
    assert!(
        snap.is_file(),
        "the publishing refresh should have written snapshot.json"
    );
    std::fs::remove_file(&snap).expect("remove snapshot");
    std::fs::create_dir(&snap).expect("wedge snapshot path");

    let (status, body) = request(&app, "POST", "/refresh-ready/_refresh", json!({})).await;
    assert!(
        status.is_success(),
        "_refresh must not run WAL maintenance: {status} {body}"
    );
    let (status, body) = request(&app, "POST", "/refresh-ready/_flush", json!({})).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "_flush must still persist the snapshot: {status} {body}"
    );
}

/// #2234 — a multi-index refresh must not be serialized behind one index.
/// `refresh-busy` is held inside `Index::refresh`. `refresh-healthy` has to
/// finish (its completion counter moves) before that hold is released.
#[tokio::test]
async fn multi_index_refresh_is_not_serialized_behind_one_index() {
    let dir = tempfile::tempdir().expect("data dir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let probe = engine.clone();
    let app = xerj_api::router::build_es_compat_router(xerj_api::state::AppState::new(
        config, engine, metrics,
    ));
    create_text_index_with_document(&app, "refresh-busy").await;
    create_text_index_with_document(&app, "refresh-healthy").await;

    let busy = probe.get_index("refresh-busy").expect("busy index");
    let healthy = probe.get_index("refresh-healthy").expect("healthy index");
    busy.test_set_pause_refresh(true);

    let app_for_request = app.clone();
    let request_task = tokio::spawn(async move {
        request(
            &app_for_request,
            "POST",
            "/refresh-busy,refresh-healthy/_refresh",
            json!({}),
        )
        .await
    });

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !busy.test_refresh_is_paused() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("busy index never entered refresh");

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while healthy.test_refresh_finished_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("healthy index did not finish refresh while the busy index was held");
    assert_eq!(
        busy.test_refresh_finished_count(),
        0,
        "the held index must not look finished"
    );

    busy.test_set_pause_refresh(false);
    let (status, body) = tokio::time::timeout(std::time::Duration::from_secs(10), request_task)
        .await
        .expect("refresh request did not finish after the hold was released")
        .expect("refresh task");
    assert!(status.is_success(), "{status} {body}");
    assert_eq!(body.pointer("/_shards/total"), Some(&json!(2)), "{body}");
    assert_eq!(
        body.pointer("/_shards/successful"),
        Some(&json!(2)),
        "{body}"
    );
}
