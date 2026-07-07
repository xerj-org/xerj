//! Verify route precedence inside `xerj_console_router`: API routes
//! (`/_xerj-console/api/v1/...`) win over the SPA wildcard `/_xerj-console/*rest`.
//!
//! The SPA wildcard is registered first (it's the most general route);
//! axum's matchit picks the more-specific API routes for paths that
//! match both. This test guards against accidental route-table changes
//! that would let the SPA handler swallow API requests.

use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use tempfile::TempDir;
use tower::ServiceExt;
use xerj_common::config::Config;
use xerj_console_api::{state::ClusterMode, xerj_console_router, ConsoleState};
use xerj_engine::Engine;

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

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).expect("response was valid JSON")
}

#[tokio::test]
async fn cluster_info_wins_over_spa_wildcard() {
    let (router, _dir) = boot();
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/_xerj-console/api/v1/cluster/info")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "API route must win over /_xerj-console/*rest — got {}",
        resp.status()
    );
    let body = body_json(resp).await;
    assert_eq!(body["data"]["mode"], "standalone");
}

#[tokio::test]
async fn spa_wildcard_still_owns_non_api_paths() {
    let (router, _dir) = boot();
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/_xerj-console/this-asset-does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // SPA handler returns 404 with its own error body for unknown
    // assets — proves the wildcard is still wired up for non-API
    // paths under /_xerj-console/.
    assert_eq!(resp.status(), 404);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8_lossy(&bytes);
    assert!(
        body.contains("xerj-console asset not found"),
        "expected SPA-handler 404 body, got: {body}"
    );
}
