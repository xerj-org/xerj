//! End-to-end tests for the user-state surface (prefs, dashboards, views,
//! data-sources) — phase 3.
//!
//! These are the endpoints the Xerj Console SPA depends on for v1.0 to drop
//! `localStorage` writes.  Every endpoint requires an authenticated
//! session, so the suite uses a test-only helper that bypasses the
//! WebAuthn dance by minting a session cookie directly. (Production
//! flows always go through the real passkey path; the helper exists
//! only inside #[cfg(test)] code paths.)

use axum::{body::Body, http::Request, Router};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;
use xerj_common::config::Config;
use xerj_engine::Engine;
use xerj_console_api::{
    auth::{sessions, store},
    state::ClusterMode,
    xerj_console_router, ConsoleState,
};

struct TestApp {
    router: Router,
    cookie: String,
    _dir: TempDir,
}

async fn boot_with_session() -> TestApp {
    let dir = TempDir::new().unwrap();
    let mut cfg = Config::default();
    cfg.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(cfg).expect("engine");
    let outcome =
        xerj_console_api::bootstrap::run(&engine, dir.path(), "http://localhost:9200")
            .await
            .unwrap();
    let state = ConsoleState::new(
        engine.clone(),
        "local".into(),
        outcome.master_key,
        ClusterMode::Standalone,
    );

    // Synthesize an active owner user + a session for them, so the
    // tests can hit the protected routes without WebAuthn round-trips.
    let user = store::User {
        id: "owner-test".to_string(),
        email: "owner@example.com".to_string(),
        display_name: "Owner".to_string(),
        role: "owner".to_string(),
        status: store::UserStatus::Active,
        created_at: xerj_console_api::time::now_iso(),
        last_seen_at: Some(xerj_console_api::time::now_iso()),
    };
    store::upsert_user(&engine, &user).await.unwrap();
    let (_session, signed) = sessions::mint_session(
        &state, &user.id, "passkey", None, None,
    ).await.unwrap();
    let cookie = format!("xerj_session={signed}");

    let router = xerj_console_router(state);
    TestApp { router, cookie, _dir: dir }
}

async fn body_json(resp: axum::response::Response) -> (axum::http::StatusCode, Value) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, v)
}

fn req(method: &str, path: &str, cookie: &str, body: Option<Value>) -> Request<Body> {
    let body = match body {
        Some(b) => Body::from(b.to_string()),
        None => Body::empty(),
    };
    Request::builder()
        .method(method)
        .uri(path)
        .header("cookie", cookie)
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

// ─────────────────────────────────────────────────────────────────────────────
// PREFS
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn prefs_default_when_unset() {
    let app = boot_with_session().await;
    let r = app.router.oneshot(req("GET", "/_xerj-console/api/v1/prefs", &app.cookie, None)).await.unwrap();
    let (status, body) = body_json(r).await;
    assert_eq!(status, 200);
    assert!(body["data"]["theme"].is_string(), "got body: {body}");
}

#[tokio::test]
async fn prefs_round_trip() {
    let app = boot_with_session().await;

    let put = req("PUT", "/_xerj-console/api/v1/prefs", &app.cookie, Some(json!({
        "theme": "light",
        "time": "1H",
        "cluster": "REMOTE"
    })));
    let r = app.router.clone().oneshot(put).await.unwrap();
    assert_eq!(r.status(), 200);

    let r = app.router.oneshot(req("GET", "/_xerj-console/api/v1/prefs", &app.cookie, None)).await.unwrap();
    let (_, body) = body_json(r).await;
    assert_eq!(body["data"]["theme"], "light");
    assert_eq!(body["data"]["time"], "1H");
}

// ─────────────────────────────────────────────────────────────────────────────
// DASHBOARDS
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn dashboards_create_get_list_patch_delete() {
    let app = boot_with_session().await;

    // CREATE
    let r = app.router.clone().oneshot(req(
        "POST", "/_xerj-console/api/v1/dashboards", &app.cookie,
        Some(json!({ "name": "AI Overview", "visibility": "private" })),
    )).await.unwrap();
    let (status, body) = body_json(r).await;
    assert_eq!(status, 201, "create: {body}");
    let dash_id = body["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(body["data"]["name"], "AI Overview");
    assert_eq!(body["data"]["version"], 1);

    // GET ONE
    let r = app.router.clone().oneshot(req(
        "GET", &format!("/_xerj-console/api/v1/dashboards/{dash_id}"), &app.cookie, None,
    )).await.unwrap();
    let (status, body) = body_json(r).await;
    assert_eq!(status, 200);
    assert_eq!(body["data"]["id"], dash_id);

    // LIST
    let r = app.router.clone().oneshot(req(
        "GET", "/_xerj-console/api/v1/dashboards", &app.cookie, None,
    )).await.unwrap();
    let (_, body) = body_json(r).await;
    let arr = body["data"]["dashboards"].as_array().unwrap();
    assert!(arr.iter().any(|d| d["id"] == dash_id), "missing in list: {body}");

    // PATCH (rename) — exercise the etag check path with a correct etag
    let patch = Request::builder()
        .method("PATCH")
        .uri(&format!("/_xerj-console/api/v1/dashboards/{dash_id}"))
        .header("cookie", &app.cookie)
        .header("content-type", "application/json")
        .header("if-match", "W/\"1\"")
        .body(Body::from(json!({ "name": "Renamed" }).to_string()))
        .unwrap();
    let r = app.router.clone().oneshot(patch).await.unwrap();
    let (status, body) = body_json(r).await;
    assert_eq!(status, 200, "patch: {body}");
    assert_eq!(body["data"]["name"], "Renamed");
    assert_eq!(body["data"]["version"], 2);

    // PATCH with stale etag → 409
    let stale = Request::builder()
        .method("PATCH")
        .uri(&format!("/_xerj-console/api/v1/dashboards/{dash_id}"))
        .header("cookie", &app.cookie)
        .header("content-type", "application/json")
        .header("if-match", "W/\"1\"")
        .body(Body::from(json!({ "name": "Should fail" }).to_string()))
        .unwrap();
    let r = app.router.clone().oneshot(stale).await.unwrap();
    assert_eq!(r.status(), 409, "stale etag must 409");

    // DELETE
    let r = app.router.clone().oneshot(req(
        "DELETE", &format!("/_xerj-console/api/v1/dashboards/{dash_id}"), &app.cookie, None,
    )).await.unwrap();
    assert_eq!(r.status(), 204);

    // GET after delete → 404
    let r = app.router.oneshot(req(
        "GET", &format!("/_xerj-console/api/v1/dashboards/{dash_id}"), &app.cookie, None,
    )).await.unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn dashboards_unauth_returns_401() {
    let app = boot_with_session().await;
    let r = app.router.oneshot(
        Request::builder().method("GET").uri("/_xerj-console/api/v1/dashboards").body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn dashboard_create_rejects_unknown_visibility() {
    let app = boot_with_session().await;
    let r = app.router.oneshot(req(
        "POST", "/_xerj-console/api/v1/dashboards", &app.cookie,
        Some(json!({ "name": "x", "visibility": "totally-not-real" })),
    )).await.unwrap();
    assert_eq!(r.status(), 400);
}

// ─────────────────────────────────────────────────────────────────────────────
// VIEWS
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn views_round_trip() {
    let app = boot_with_session().await;

    // Need a dashboard first
    let r = app.router.clone().oneshot(req(
        "POST", "/_xerj-console/api/v1/dashboards", &app.cookie,
        Some(json!({ "name": "D", "visibility": "private" })),
    )).await.unwrap();
    let (_, body) = body_json(r).await;
    let dash_id = body["data"]["id"].as_str().unwrap().to_string();

    // Create view
    let r = app.router.clone().oneshot(req(
        "POST", "/_xerj-console/api/v1/views", &app.cookie,
        Some(json!({
            "dashboard_id": dash_id, "name": "Last 24h",
            "time": { "from": "now-24h", "to": "now" }
        })),
    )).await.unwrap();
    let (status, body) = body_json(r).await;
    assert_eq!(status, 201, "{body}");
    let view_id = body["data"]["id"].as_str().unwrap().to_string();

    // List filtered by dashboard
    let r = app.router.clone().oneshot(req(
        "GET", &format!("/_xerj-console/api/v1/views?dashboard={dash_id}"), &app.cookie, None,
    )).await.unwrap();
    let (_, body) = body_json(r).await;
    assert_eq!(body["data"]["total"], 1);
    assert_eq!(body["data"]["views"][0]["id"], view_id);

    // Delete
    let r = app.router.oneshot(req(
        "DELETE", &format!("/_xerj-console/api/v1/views/{view_id}"), &app.cookie, None,
    )).await.unwrap();
    assert_eq!(r.status(), 204);
}

// ─────────────────────────────────────────────────────────────────────────────
// DATA SOURCES
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn data_sources_lists_built_in_connection() {
    let app = boot_with_session().await;
    let r = app.router.oneshot(req(
        "GET", "/_xerj-console/api/v1/data-sources/connections", &app.cookie, None,
    )).await.unwrap();
    let (status, body) = body_json(r).await;
    assert_eq!(status, 200);
    let conns = body["data"]["connections"].as_array().unwrap();
    assert!(!conns.is_empty(), "must auto-provision built-in connection");
    assert_eq!(conns[0]["id"], "built-in");
    assert_eq!(conns[0]["kind"], "xerj-local");
    assert_eq!(conns[0]["managed"], true);
    assert_eq!(conns[0]["status"], "green");
}

#[tokio::test]
async fn data_sources_list_indices_for_built_in() {
    let app = boot_with_session().await;
    let r = app.router.oneshot(req(
        "GET", "/_xerj-console/api/v1/data-sources/connections/built-in/indices", &app.cookie, None,
    )).await.unwrap();
    let (status, body) = body_json(r).await;
    assert_eq!(status, 200);
    assert!(body["data"]["indices"].is_array());
    // System indices are skipped — fresh data dir must list nothing.
    assert_eq!(body["data"]["total"], 0);
}

#[tokio::test]
async fn data_sources_unknown_connection_returns_501() {
    let app = boot_with_session().await;
    let r = app.router.oneshot(req(
        "GET", "/_xerj-console/api/v1/data-sources/connections/elasticsearch-prod/indices", &app.cookie, None,
    )).await.unwrap();
    assert_eq!(r.status(), 501);
}

#[tokio::test]
async fn known_routes_include_phase3() {
    let routes = xerj_console_api::router::known_routes();
    for r in &[
        "/_xerj-console/api/v1/prefs",
        "/_xerj-console/api/v1/dashboards",
        "/_xerj-console/api/v1/dashboards/:id",
        "/_xerj-console/api/v1/views",
        "/_xerj-console/api/v1/views/:id",
        "/_xerj-console/api/v1/data-sources/connections",
        "/_xerj-console/api/v1/data-sources/connections/:id/indices",
        "/_xerj-console/api/v1/data-sources/connections/:id/indices/:name/fields",
    ] {
        assert!(routes.contains(r), "missing route: {r}");
    }
}
