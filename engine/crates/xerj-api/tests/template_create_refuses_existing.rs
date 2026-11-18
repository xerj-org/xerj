//! `?create=true` on the three template PUTs refuses a name that is taken.
//!
//! Elasticsearch reads `create` on `PUT /_template/{name}`,
//! `PUT /_index_template/{name}` and `PUT /_component_template/{name}`: when it
//! is true and the name already exists, the request fails with a 400
//! `illegal_argument_exception` and the stored template is left alone. XERJ
//! used to drop the flag, answer 200 and overwrite. Without the flag a PUT
//! still creates or replaces, as before.
//!
//! The reasons are ES 8.13's own strings (`MetadataIndexTemplateService`):
//! `index_template [n] already exists` for legacy templates, `index template
//! [n] already exists` for composable ones and `component template [n]
//! already exists` for component templates.

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

async fn put(app: &axum::Router, path: &str, body: &Value) -> (StatusCode, Value) {
    send(
        app,
        Request::put(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request"),
    )
    .await
}

async fn get(app: &axum::Router, path: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::get(path).body(Body::empty()).expect("request"),
    )
    .await
}

/// One template API: where it lives, two distinguishable bodies, how to read
/// back which of the two is stored, and ES's "already exists" reason.
struct Kind {
    base: &'static str,
    first: Value,
    second: Value,
    marker: fn(&Value) -> Value,
    exists_reason: &'static str,
}

fn kinds() -> Vec<Kind> {
    vec![
        Kind {
            base: "/_template/t",
            first: json!({ "index_patterns": ["first-*"] }),
            second: json!({ "index_patterns": ["second-*"] }),
            marker: |got| got["t"]["index_patterns"][0].clone(),
            exists_reason: "index_template [t] already exists",
        },
        Kind {
            base: "/_index_template/t",
            first: json!({ "index_patterns": ["first-*"], "priority": 1 }),
            second: json!({ "index_patterns": ["second-*"], "priority": 2 }),
            marker: |got| got["index_templates"][0]["index_template"]["index_patterns"][0].clone(),
            exists_reason: "index template [t] already exists",
        },
        Kind {
            base: "/_component_template/t",
            first: json!({ "template": {}, "_meta": { "v": "first-*" } }),
            second: json!({ "template": {}, "_meta": { "v": "second-*" } }),
            marker: |got| got["component_templates"][0]["component_template"]["_meta"]["v"].clone(),
            exists_reason: "component template [t] already exists",
        },
    ]
}

async fn stored(app: &axum::Router, kind: &Kind) -> Value {
    let (status, got) = get(app, kind.base).await;
    assert_eq!(status, StatusCode::OK, "GET {}: {got}", kind.base);
    (kind.marker)(&got)
}

fn assert_already_exists(kind: &Kind, status: StatusCode, got: &Value) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{}: {got}", kind.base);
    assert_eq!(got["status"], 400, "{}: {got}", kind.base);
    assert_eq!(
        got["error"]["type"], "illegal_argument_exception",
        "{}: {got}",
        kind.base
    );
    assert_eq!(
        got["error"]["reason"], kind.exists_reason,
        "{}: {got}",
        kind.base
    );
    assert_eq!(
        got["error"]["root_cause"][0]["reason"], kind.exists_reason,
        "{}: {got}",
        kind.base
    );
}

#[tokio::test]
async fn create_true_on_a_free_name_creates() {
    for kind in kinds() {
        let (app, _dir) = app().await;
        let (status, got) = put(&app, &format!("{}?create=true", kind.base), &kind.first).await;
        assert_eq!(status, StatusCode::OK, "{}: {got}", kind.base);
        assert_eq!(got["acknowledged"], true, "{}: {got}", kind.base);
        assert_eq!(stored(&app, &kind).await, "first-*", "{}", kind.base);
    }
}

#[tokio::test]
async fn create_true_on_a_taken_name_is_refused_and_keeps_the_original() {
    for kind in kinds() {
        let (app, _dir) = app().await;
        let (status, got) = put(&app, &format!("{}?create=true", kind.base), &kind.first).await;
        assert_eq!(status, StatusCode::OK, "{}: {got}", kind.base);

        let (status, got) = put(&app, &format!("{}?create=true", kind.base), &kind.second).await;
        assert_already_exists(&kind, status, &got);
        assert_eq!(stored(&app, &kind).await, "first-*", "{}", kind.base);
    }
}

/// ES checks `create` before comparing bodies, so resending the identical
/// template is refused too. This is the shape of the ES-YAML case
/// `indices.put_template/10_basic.yml · Put template create`.
#[tokio::test]
async fn create_true_refuses_an_identical_resend() {
    for kind in kinds() {
        let (app, _dir) = app().await;
        let (status, got) = put(&app, &format!("{}?create=true", kind.base), &kind.first).await;
        assert_eq!(status, StatusCode::OK, "{}: {got}", kind.base);
        let (status, got) = put(&app, &format!("{}?create=true", kind.base), &kind.first).await;
        assert_already_exists(&kind, status, &got);
    }
}

/// A bare `?create` means true, as `RestRequest#paramAsBoolean` reads it.
#[tokio::test]
async fn bare_create_means_true() {
    for kind in kinds() {
        let (app, _dir) = app().await;
        let (status, got) = put(&app, kind.base, &kind.first).await;
        assert_eq!(status, StatusCode::OK, "{}: {got}", kind.base);
        let (status, got) = put(&app, &format!("{}?create", kind.base), &kind.second).await;
        assert_already_exists(&kind, status, &got);
        assert_eq!(stored(&app, &kind).await, "first-*", "{}", kind.base);
    }
}

/// No flag, or `create=false`, still creates or replaces: the default does
/// not move.
#[tokio::test]
async fn without_create_a_put_still_replaces() {
    for kind in kinds() {
        for suffix in ["", "?create=false"] {
            let (app, _dir) = app().await;
            let path = format!("{}{suffix}", kind.base);
            let (status, got) = put(&app, &path, &kind.first).await;
            assert_eq!(status, StatusCode::OK, "{path}: {got}");
            let (status, got) = put(&app, &path, &kind.second).await;
            assert_eq!(status, StatusCode::OK, "{path}: {got}");
            assert_eq!(stored(&app, &kind).await, "second-*", "{path}");
        }
    }
}

/// Any other value is a 400 with ES's `Booleans#parseBoolean` message, and
/// nothing is stored.
#[tokio::test]
async fn a_non_boolean_create_is_refused_and_stores_nothing() {
    for kind in kinds() {
        let (app, _dir) = app().await;
        let (status, got) = put(&app, &format!("{}?create=yes", kind.base), &kind.first).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{}: {got}", kind.base);
        assert_eq!(
            got["error"]["type"], "illegal_argument_exception",
            "{}: {got}",
            kind.base
        );
        assert_eq!(
            got["error"]["reason"],
            "Failed to parse value [yes] as only [true] or [false] are allowed.",
            "{}: {got}",
            kind.base
        );
        let (status, got) = get(&app, kind.base).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{}: {got}", kind.base);
    }
}
