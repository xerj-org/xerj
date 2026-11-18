//! `DELETE /{index}` refuses `*`, `_all` and any wildcard unless
//! `action.destructive_requires_name` is false.
//!
//! The default is true, matching Elasticsearch 8. A concrete name, and a
//! comma list of concrete names, still delete. Transient overrides persistent.
//! Closing an index is unchanged: it refuses wildcards even when this setting
//! is false.

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
    let (status, got) = send(
        app,
        Request::put("/_cluster/settings")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{got}");
}

async fn create(app: &axum::Router, name: &str) {
    let (status, body) = send(
        app,
        Request::put(format!("/{name}"))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .expect("request"),
    )
    .await;
    assert!(status.is_success(), "create {name}: {status} {body}");
}

async fn exists(app: &axum::Router, name: &str) -> bool {
    let (status, _) = send(
        app,
        Request::get(format!("/{name}"))
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    status.is_success()
}

async fn delete(app: &axum::Router, path: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::delete(path).body(Body::empty()).expect("request"),
    )
    .await
}

fn refused(status: StatusCode, body: &Value) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let reason = body["error"]["reason"].as_str().unwrap_or("");
    assert!(
        reason.contains("Wildcard expressions or all indices are not allowed"),
        "{body}"
    );
}

#[tokio::test]
async fn a_wildcard_delete_is_refused_until_the_setting_is_turned_off() {
    let (app, _dir) = app().await;
    create(&app, "ax-docs").await;
    create(&app, "kept").await;

    let (status, body) = delete(&app, "/ax-*").await;
    refused(status, &body);
    assert!(
        exists(&app, "ax-docs").await,
        "a refused wildcard must delete nothing"
    );
    assert!(exists(&app, "kept").await);

    let (status, body) = delete(&app, "/_all").await;
    refused(status, &body);
    assert!(exists(&app, "ax-docs").await);
    assert!(exists(&app, "kept").await);

    let (status, body) = delete(&app, "/*").await;
    refused(status, &body);

    let (status, body) = delete(&app, "/kept,ax-*").await;
    refused(status, &body);
    assert!(
        exists(&app, "kept").await,
        "a mixed list must not delete the concrete name either"
    );

    // `?` is a query-string marker on the wire, so the wildcard is percent-encoded.
    // The router decodes it back to a selector that contains `?`.
    let (status, body) = delete(&app, "/ke%3F").await;
    refused(status, &body);

    let (status, body) = delete(&app, "/ax-docs").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!exists(&app, "ax-docs").await);
    assert!(
        exists(&app, "kept").await,
        "a concrete delete leaves the other index"
    );

    put_settings(
        &app,
        json!({ "persistent": { "action.destructive_requires_name": false } }),
    )
    .await;
    create(&app, "ax-docs").await;
    let (status, body) = delete(&app, "/ax-*").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!exists(&app, "ax-docs").await);
    assert!(
        exists(&app, "kept").await,
        "the pattern must not widen past ax-*"
    );
}

#[tokio::test]
async fn a_comma_list_of_concrete_names_still_deletes() {
    let (app, _dir) = app().await;
    create(&app, "ax-docs").await;
    create(&app, "ax-code").await;
    let (status, body) = delete(&app, "/ax-docs,ax-code").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!exists(&app, "ax-docs").await);
    assert!(!exists(&app, "ax-code").await);
}

#[tokio::test]
async fn transient_true_overrides_persistent_false() {
    let (app, _dir) = app().await;
    create(&app, "ax-docs").await;
    put_settings(
        &app,
        json!({ "persistent": { "action.destructive_requires_name": "false" } }),
    )
    .await;
    put_settings(
        &app,
        json!({ "transient": { "action.destructive_requires_name": true } }),
    )
    .await;
    let (status, body) = delete(&app, "/ax-*").await;
    refused(status, &body);
    assert!(exists(&app, "ax-docs").await);
}
