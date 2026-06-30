//! Smoke tests for the bundled Xerj Console SPA assets.
//!
//! `build.rs` walks the playground/ source tree at compile time and
//! emits a static `(url_path, bytes, content_type)` slice. These tests
//! confirm the runtime serves the files the demo flow depends on:
//! setup.html, login.html, the auth + sync JS modules, and the
//! patched index.html with its auth guard.

use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use tempfile::TempDir;
use tower::ServiceExt;
use xerj_common::config::Config;
use xerj_engine::Engine;
use xerj_console_api::{state::ClusterMode, xerj_console_router, ConsoleState};

fn boot() -> (axum::Router, TempDir) {
    let dir = TempDir::new().unwrap();
    let mut cfg = Config::default();
    cfg.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(cfg).expect("engine");
    xerj_console_api::indices::ensure_all(&engine).unwrap();
    let state = ConsoleState::new(
        engine,
        "local".to_string(),
        [0u8; 32],
        ClusterMode::Standalone,
    );
    (xerj_console_router(state), dir)
}

async fn fetch(router: axum::Router, path: &str) -> (u16, String) {
    let resp = router
        .oneshot(
            Request::builder()
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

#[tokio::test]
async fn setup_html_is_bundled_and_served() {
    let (router, _dir) = boot();
    // /_xerj-console/setup (no extension) should fall back to setup.html.
    let (status, body) = fetch(router, "/_xerj-console/setup").await;
    assert_eq!(status, 200, "expected 200 for /_xerj-console/setup, body={body}");
    assert!(
        body.contains("XERJ CONSOLE") && body.contains("setup"),
        "setup page must contain XERJ CONSOLE banner — got: {}",
        &body[..body.len().min(200)]
    );
    assert!(
        body.contains("token") && body.contains("redeemMagic"),
        "setup page must include the magic-link consumption logic"
    );
}

#[tokio::test]
async fn login_html_is_bundled_and_served() {
    let (router, _dir) = boot();
    let (status, body) = fetch(router, "/_xerj-console/login").await;
    assert_eq!(status, 200);
    assert!(
        body.contains("XERJ CONSOLE") && body.contains("Sign in"),
        "login page must contain XERJ CONSOLE + sign-in copy"
    );
    assert!(
        body.contains("beginLogin"),
        "login page must include the WebAuthn login logic"
    );
}

#[tokio::test]
async fn xerj_console_auth_module_is_bundled() {
    let (router, _dir) = boot();
    let (status, body) = fetch(router, "/_xerj-console/src/xerj-console-auth.js").await;
    assert_eq!(status, 200);
    assert!(body.contains("redeemMagic"));
    assert!(body.contains("beginEnrol"));
    assert!(body.contains("finishEnrol"));
    assert!(body.contains("beginLogin"));
    assert!(body.contains("finishLogin"));
    assert!(body.contains("b64uToBuf"));
    assert!(body.contains("bufToB64u"));
}

#[tokio::test]
async fn xerj_console_sync_module_is_bundled() {
    let (router, _dir) = boot();
    let (status, body) = fetch(router, "/_xerj-console/src/xerj-console-sync.js").await;
    assert_eq!(status, 200);
    assert!(body.contains("pullAll") && body.contains("startPush"));
}

#[tokio::test]
async fn index_html_has_auth_guard() {
    let (router, _dir) = boot();
    let (status, body) = fetch(router, "/_xerj-console/").await;
    assert_eq!(status, 200);
    assert!(
        body.contains("/_xerj-console/api/v1/me"),
        "index.html must call /me as the auth guard"
    );
    assert!(
        body.contains("/_xerj-console/login"),
        "index.html must redirect to /login on 401"
    );
}

#[tokio::test]
async fn root_redirect_works() {
    // /_xerj-console (no trailing slash) → redirect to /_xerj-console/.
    let (router, _dir) = boot();
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/_xerj-console")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection(),
        "/_xerj-console must redirect, got {}",
        resp.status()
    );
}
