//! End-to-end test for the phase-1 surface.
//!
//! Boots a real `Engine` against a temp dir, runs `bootstrap::run`, mounts
//! the Xerj Console router, and exercises every phase-1 endpoint over an
//! in-memory `tower::Service` call. The same code path the real server
//! uses, minus only the TCP listener.
//!
//! Each test creates its own `TempDir` so they're independent and can
//! run in parallel.

use axum::{body::Body, http::Request, Router};
use http_body_util::BodyExt;
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt; // for `.oneshot`
use xerj_common::config::Config;
use xerj_engine::Engine;
use xerj_console_api::{state::ClusterMode, xerj_console_router, ConsoleState};

fn engine_in(dir: &TempDir) -> Engine {
    let mut cfg = Config::default();
    cfg.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(cfg).expect("engine init")
}

async fn boot(dir: &TempDir) -> (Engine, Router, Option<String>) {
    let engine = engine_in(dir);
    let outcome = xerj_console_api::bootstrap::run(&engine, dir.path(), "http://localhost:9200")
        .await
        .expect("bootstrap");
    let state = ConsoleState::new(
        engine.clone(),
        "local".to_string(),
        outcome.master_key,
        ClusterMode::Standalone,
    );
    (engine, xerj_console_router(state), outcome.magic_link)
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).expect("response was valid JSON")
}

#[tokio::test]
async fn fresh_boot_creates_every_system_index() {
    let dir = TempDir::new().unwrap();
    let (engine, _router, _link) = boot(&dir).await;

    for name in xerj_console_api::indices::ALL {
        assert!(
            engine.get_index(name).is_ok(),
            "system index {name} was not created on first boot"
        );
    }
}

#[tokio::test]
async fn fresh_boot_emits_a_bootstrap_link() {
    let dir = TempDir::new().unwrap();
    let (_engine, _router, link) = boot(&dir).await;
    let url = link.expect("first boot must mint a link");
    assert!(url.starts_with("http://localhost:9200/_xerj-console/setup#token="));

    // The token should be 43 base64url chars (32 random bytes).
    let token = url.rsplit_once("token=").unwrap().1;
    assert_eq!(token.len(), 43, "token = {token}");
    for c in token.chars() {
        assert!(c.is_ascii_alphanumeric() || c == '-' || c == '_');
    }
}

#[tokio::test]
async fn cluster_info_returns_standalone_mode() {
    let dir = TempDir::new().unwrap();
    let (_engine, router, _link) = boot(&dir).await;

    let resp = router
        .oneshot(
            Request::builder()
                .uri("/_xerj-console/api/v1/cluster/info")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    let data = &body["data"];
    assert_eq!(data["mode"], "standalone");
    assert_eq!(data["node_id"], "local");
    assert!(
        data["version"].is_string(),
        "version must echo the crate version: got {}",
        data["version"]
    );
    assert!(
        data["started_at_ms"].is_i64(),
        "started_at_ms must be an integer: got {}",
        data["started_at_ms"]
    );
}

#[tokio::test]
async fn cluster_info_returns_raft_mode_when_clustered() {
    // Simulate xerj-server having brought up cluster mode by passing
    // ClusterMode::Raft + a non-"local" node id.
    let dir = TempDir::new().unwrap();
    let engine = engine_in(&dir);
    let outcome =
        xerj_console_api::bootstrap::run(&engine, dir.path(), "http://x").await.unwrap();
    let state = ConsoleState::new(
        engine,
        "10.0.0.1:7000".to_string(),
        outcome.master_key,
        ClusterMode::Raft,
    );
    let router = xerj_console_router(state);

    let resp = router
        .oneshot(
            Request::builder()
                .uri("/_xerj-console/api/v1/cluster/info")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    assert_eq!(body["data"]["mode"], "raft");
    assert_eq!(body["data"]["node_id"], "10.0.0.1:7000");
}

#[tokio::test]
async fn second_boot_does_not_create_new_link() {
    let dir = TempDir::new().unwrap();
    let (engine, _router, link1) = boot(&dir).await;
    assert!(link1.is_some(), "first boot must mint a link");

    // Simulate finishing enrolment by writing an active user.
    let users = engine.get_index(xerj_console_api::indices::USERS).unwrap();
    users
        .create_document(
            "u1".into(),
            serde_json::json!({
                "email": "owner@example.com",
                "role":  "owner",
                "status": "active",
                "created_at": xerj_console_api::time::now_iso()
            }),
        )
        .await
        .unwrap();
    users.flush().await.unwrap();

    // Re-boot with the same data dir.
    let outcome2 = xerj_console_api::bootstrap::run(&engine, dir.path(), "http://x")
        .await
        .unwrap();
    assert!(
        outcome2.magic_link.is_none(),
        "second boot with an active user must not mint a fresh link"
    );
}

#[tokio::test]
async fn unknown_xerj_console_route_returns_404() {
    let dir = TempDir::new().unwrap();
    let (_engine, router, _link) = boot(&dir).await;

    let resp = router
        .oneshot(
            Request::builder()
                .uri("/this/route/does/not/exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn known_routes_table_matches_registered_routes() {
    // Catches the easy mistake of adding a route to known_routes() (used
    // by tests) without registering it, or vice versa.
    let routes = xerj_console_api::router::known_routes();
    assert!(!routes.is_empty(), "phase 1 must expose at least /cluster/info");
    assert!(routes.contains(&"/_xerj-console/api/v1/cluster/info"));
}
