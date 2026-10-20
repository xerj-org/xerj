//! Issue #1573: `GET /{index}` reported `settings.index.creation_date` as the
//! current time on every request, even though the engine persists each
//! index's real creation time (`created_at.json`, `Engine::index_created_at`).
//!
//! Two effects were user-visible: the value changed on every call, and a
//! restart re-synthesized it. Clients that read `creation_date` (index age,
//! ILM-style tooling) saw "now" forever.
//!
//! This test pins the three surfaces that read the value: `GET /{index}`,
//! `GET /{index}?human=true` (`creation_date_string` must be derived from the
//! SAME millisecond), and `GET /{index}/_settings` — plus survival across a
//! restart on the same data dir.
//!
//! Elasticsearch is referenced for wire semantics only; no ES code is
//! reproduced here.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

fn config_for(dir: &std::path::Path) -> xerj_common::config::Config {
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config
}

/// Build a fresh app over `dir`. Calling this after the previous app has been
/// dropped is a restart: nothing is handed over in memory.
fn app_over(dir: &std::path::Path) -> axum::Router {
    let config = config_for(dir);
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    xerj_api::router::build_es_compat_router(state)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(req).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

fn put_json(path: &str, body: Value) -> Request<Body> {
    Request::put(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

fn get(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).expect("request")
}

fn creation_date(body: &Value, index: &str) -> i64 {
    body.pointer(&format!("/{index}/settings/index/creation_date"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no settings.index.creation_date in {body}"))
        .parse()
        .unwrap_or_else(|_| panic!("creation_date is not a millisecond integer in {body}"))
}

#[tokio::test]
async fn creation_date_is_the_persisted_creation_time() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = "born";

    let before_create = chrono::Utc::now().timestamp_millis();

    // ── Session 1 ──
    let first;
    {
        let app = app_over(dir.path());
        let (st, body) = send(&app, put_json(&format!("/{index}"), json!({}))).await;
        assert_eq!(st, StatusCode::OK, "create index: {body}");

        let (st, body) = send(&app, get(&format!("/{index}"))).await;
        assert_eq!(st, StatusCode::OK, "get index: {body}");
        first = creation_date(&body, index);

        // The recorded time is the real creation time, not a far-future or
        // far-past placeholder.
        let now_after = chrono::Utc::now().timestamp_millis();
        assert!(
            first >= before_create && first <= now_after,
            "creation_date {first} must fall between {before_create} and {now_after}"
        );

        // A later request must return the SAME millisecond. Before #1573 this
        // was `Utc::now()` per request, so the sleep makes the two differ.
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let (st, body) = send(&app, get(&format!("/{index}"))).await;
        assert_eq!(st, StatusCode::OK, "second get index: {body}");
        assert_eq!(
            creation_date(&body, index),
            first,
            "creation_date must not change between requests: {body}"
        );

        // `human=true` renders `creation_date_string` from the same value.
        let (st, body) = send(&app, get(&format!("/{index}?human=true"))).await;
        assert_eq!(st, StatusCode::OK, "human get index: {body}");
        assert_eq!(creation_date(&body, index), first);
        let rendered = body
            .pointer(&format!("/{index}/settings/index/creation_date_string"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("no creation_date_string in {body}"));
        let rendered_ms = chrono::DateTime::parse_from_rfc3339(rendered)
            .unwrap_or_else(|e| panic!("creation_date_string {rendered:?} is not RFC3339: {e}"))
            .timestamp_millis();
        assert_eq!(
            rendered_ms, first,
            "creation_date_string must render the same millisecond"
        );

        // The `_settings` surface reads the same engine value.
        let (st, body) = send(&app, get(&format!("/{index}/_settings"))).await;
        assert_eq!(st, StatusCode::OK, "get settings: {body}");
        assert_eq!(creation_date(&body, index), first);
    }

    // ── Session 2: a fresh engine sees only what reached disk. ──
    let app = app_over(dir.path());
    let (st, body) = send(&app, get(&format!("/{index}"))).await;
    assert_eq!(st, StatusCode::OK, "get index after restart: {body}");
    assert_eq!(
        creation_date(&body, index),
        first,
        "creation_date must survive a restart, not be re-synthesized: {body}"
    );
}
