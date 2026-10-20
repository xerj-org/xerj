//! `PUT /_cluster/settings` merges into the stored settings, the way
//! Elasticsearch does: a later request for one setting must not drop the
//! others, the nested object form is stored under the same dotted keys as the
//! flat form, and `null` removes a setting. The runtime readers (disk
//! flood-stage and memory-breaker overrides) look settings up by flat key, so
//! a replaced or nested setting silently stopped applying. `GET` renders the
//! nested form unless `flat_settings=true` is passed.
//!
//! Elasticsearch is referenced for semantics only; no code from it is
//! reproduced here.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(req).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn put_settings(app: &axum::Router, body: Value) {
    let (status, _) = send(
        app,
        Request::put("/_cluster/settings")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

async fn get_settings(app: &axum::Router) -> Value {
    get_settings_at(app, "/_cluster/settings?flat_settings=true").await
}

async fn get_settings_at(app: &axum::Router, uri: &str) -> Value {
    let (status, body) = send(app, Request::get(uri).body(Body::empty()).expect("request")).await;
    assert_eq!(status, StatusCode::OK);
    body
}

const FLOOD: &str = "cluster.routing.allocation.disk.watermark.flood_stage";
const BREAKER: &str = "indices.breaker.total.limit";

#[tokio::test]
async fn a_second_put_keeps_earlier_settings() {
    let (app, _dir) = app().await;
    put_settings(&app, json!({ "persistent": { FLOOD: "99%" } })).await;
    put_settings(&app, json!({ "persistent": { BREAKER: "70%" } })).await;
    put_settings(&app, json!({ "transient": { "search.max_buckets": 100 } })).await;

    let got = get_settings(&app).await;
    assert_eq!(got["persistent"][FLOOD], "99%", "{got}");
    assert_eq!(got["persistent"][BREAKER], "70%", "{got}");
    assert_eq!(got["transient"]["search.max_buckets"], 100, "{got}");
}

#[tokio::test]
async fn nested_form_is_stored_under_dotted_keys() {
    let (app, _dir) = app().await;
    put_settings(
        &app,
        json!({ "persistent": { "cluster": { "routing": { "allocation": { "disk": {
            "watermark": { "flood_stage": "99%" }
        } } } } } }),
    )
    .await;

    let got = get_settings(&app).await;
    assert_eq!(got["persistent"][FLOOD], "99%", "{got}");
}

#[tokio::test]
async fn null_removes_a_setting() {
    let (app, _dir) = app().await;
    put_settings(
        &app,
        json!({ "persistent": { FLOOD: "99%", BREAKER: "70%" } }),
    )
    .await;
    put_settings(&app, json!({ "persistent": { FLOOD: null } })).await;

    let got = get_settings(&app).await;
    assert!(got["persistent"].get(FLOOD).is_none(), "{got}");
    assert_eq!(got["persistent"][BREAKER], "70%", "{got}");
}

#[tokio::test]
async fn get_is_nested_unless_flat_settings() {
    let (app, _dir) = app().await;
    put_settings(&app, json!({ "persistent": { BREAKER: "70%" } })).await;

    let nested = get_settings_at(&app, "/_cluster/settings").await;
    assert_eq!(
        nested["persistent"]["indices"]["breaker"]["total"]["limit"], "70%",
        "{nested}"
    );
    let flat = get_settings_at(&app, "/_cluster/settings?flat_settings=true").await;
    assert_eq!(flat["persistent"][BREAKER], "70%", "{flat}");
}

#[tokio::test]
async fn put_response_lists_only_this_requests_settings() {
    let (app, _dir) = app().await;
    put_settings(&app, json!({ "persistent": { FLOOD: "99%" } })).await;

    let (status, body) = send(
        &app,
        Request::put("/_cluster/settings?flat_settings=true")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "persistent": { BREAKER: "70%" } }).to_string(),
            ))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["persistent"], json!({ BREAKER: "70%" }), "{body}");
    assert_eq!(body["transient"], json!({}), "{body}");
}
