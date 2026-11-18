//! `/_cat/allocation?format=json` carries the node's flood-stage gate (#2077).
//!
//! Autoindex reads this before it walks. The numbers are the data directory's
//! filesystem (the same `statvfs` the governor admits on), not a client-side
//! guess, and the threshold is the governor's effective percent — config
//! default 95, after any live cluster-settings override. The plain-text cat
//! table does not grow a column.

use axum::body::Body;
use axum::http::Request;
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

#[tokio::test]
async fn allocation_json_reports_the_flood_stage_gate_and_text_does_not() {
    let (app, _dir) = app().await;

    let response = app
        .clone()
        .oneshot(
            Request::get("/_cat/allocation?format=json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("json allocation");
    assert!(response.status().is_success());
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    let row = body
        .as_array()
        .and_then(|rows| rows.first())
        .expect("one allocation row");
    assert_eq!(row["disk.flood_stage"], "95");
    assert_eq!(row["disk.threshold_enabled"], "true");
    assert!(
        row["disk.flood_blocked"] == "true" || row["disk.flood_blocked"] == "false",
        "blocked is a cat-style boolean string: {row}"
    );
    let avail = row["disk.avail"].as_str().expect("disk.avail");
    assert!(
        avail.ends_with('b') && avail[..avail.len() - 1].chars().all(|c| c.is_ascii_digit()),
        "free space is a raw byte count, which is what autoindex parses: {avail}"
    );
    let percent: u64 = row["disk.percent"]
        .as_str()
        .expect("disk.percent")
        .parse()
        .expect("integer percent");
    assert!(percent <= 100, "{percent}");
    let data_dir = _dir.path().to_string_lossy().into_owned();
    assert_eq!(
        row["path.data"].as_str(),
        Some(data_dir.as_str()),
        "memory writes name this directory when the flood-stage block fires: {row}"
    );

    let response = app
        .oneshot(
            Request::get("/_cat/allocation")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("text allocation");
    assert!(response.status().is_success());
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        text.contains("xerj-node-1"),
        "the text table is still the cat line: {text}"
    );
    assert!(
        !text.contains("flood"),
        "flood-stage fields are not text columns: {text}"
    );
}
