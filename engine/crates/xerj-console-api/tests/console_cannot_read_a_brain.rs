//! The Console data-sources proxy must not cross the per-tenant brain
//! boundary (RC10 blocker B1).
//!
//! The data plane guards the reserved `.xerj-memory-*` namespace in
//! `xerj_api::authz` middleware, but the Console router is merged onto the
//! engine routers *after* their layers are applied (`xerj-server::main`), so
//! that middleware never runs on `/_xerj-console/*` and no index-visibility
//! scope is installed. The data-sources handlers talk to the engine
//! in-process, where an absent guard means "engine-internal work, allow" —
//! so any authenticated Console session of any role could read any tenant's
//! brain through the search/fields/indices proxy. These tests pin the fix:
//! the proxy refuses the reserved namespace itself, with the same NotFound
//! it already gives `.xerj_*` system indices, so existence doesn't leak.

use axum::{body::Body, http::Request, Router};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_console_api::{
    auth::{sessions, store},
    state::ClusterMode,
    xerj_console_router, ConsoleState,
};
use xerj_engine::Engine;

/// A secret that must never appear in any Console response body.
const ALICE_SECRET: &str = "alice-private-fact-9f83c1";

struct TestApp {
    router: Router,
    cookie: String,
    _dir: TempDir,
}

/// Boot the console with an authenticated owner session — the strongest
/// role there is. If even the owner cannot reach a brain through the
/// proxy, no session can.
async fn boot_with_brain() -> TestApp {
    let dir = TempDir::new().unwrap();
    let mut cfg = Config::default();
    cfg.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(cfg).expect("engine");
    let outcome = xerj_console_api::bootstrap::run(&engine, dir.path(), "http://localhost:9200")
        .await
        .unwrap();
    let state = ConsoleState::new(
        engine.clone(),
        "local".into(),
        outcome.master_key,
        ClusterMode::Standalone,
    );

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
    let (_session, signed) = sessions::mint_session(&state, &user.id, "passkey", None, None)
        .await
        .unwrap();
    let cookie = format!("xerj_session={signed}");

    // Another tenant's brain, exactly as memory_api provisions it: the
    // namespace index plus its -edges sibling, both in the reserved prefix.
    engine
        .create_index(".xerj-memory-alice", Schema::empty())
        .expect("create brain index");
    engine
        .create_index(".xerj-memory-alice-edges", Schema::empty())
        .expect("create brain edges index");
    let brain = engine.get_index(".xerj-memory-alice").unwrap();
    brain
        .create_document("m1".into(), json!({ "content": ALICE_SECRET }))
        .await
        .expect("write brain doc");

    let router = xerj_console_router(state);
    TestApp {
        router,
        cookie,
        _dir: dir,
    }
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

#[tokio::test]
async fn search_cannot_read_a_brain() {
    let app = boot_with_brain().await;

    let r = app
        .router
        .clone()
        .oneshot(req(
            "POST",
            "/_xerj-console/api/v1/data-sources/connections/built-in/indices/.xerj-memory-alice/search",
            &app.cookie,
            Some(json!({ "query": { "match_all": {} } })),
        ))
        .await
        .unwrap();
    let (status, body) = body_json(r).await;
    assert_eq!(status, 404, "brain search must 404, got: {body}");
    assert!(
        !body.to_string().contains(ALICE_SECRET),
        "brain content leaked: {body}"
    );

    // No existence oracle: a brain that exists and one that doesn't must be
    // indistinguishable through this endpoint.
    let r = app
        .router
        .oneshot(req(
            "POST",
            "/_xerj-console/api/v1/data-sources/connections/built-in/indices/.xerj-memory-nobody/search",
            &app.cookie,
            Some(json!({ "query": { "match_all": {} } })),
        ))
        .await
        .unwrap();
    let (ghost_status, ghost_body) = body_json(r).await;
    assert_eq!(status, ghost_status);
    assert_eq!(
        body.to_string().replace("alice", "nobody"),
        ghost_body.to_string(),
        "existing vs missing brain must be indistinguishable"
    );
}

#[tokio::test]
async fn fields_do_not_leak_brain_mappings() {
    let app = boot_with_brain().await;

    for index in [".xerj-memory-alice", ".xerj-memory-alice-edges"] {
        let r = app
            .router
            .clone()
            .oneshot(req(
                "GET",
                &format!(
                    "/_xerj-console/api/v1/data-sources/connections/built-in/indices/{index}/fields"
                ),
                &app.cookie,
                None,
            ))
            .await
            .unwrap();
        let (status, body) = body_json(r).await;
        assert_eq!(status, 404, "{index} fields must 404, got: {body}");
    }
}

#[tokio::test]
async fn index_listing_hides_brains() {
    let app = boot_with_brain().await;

    let r = app
        .router
        .oneshot(req(
            "GET",
            "/_xerj-console/api/v1/data-sources/connections/built-in/indices",
            &app.cookie,
            None,
        ))
        .await
        .unwrap();
    let (status, body) = body_json(r).await;
    assert_eq!(status, 200);
    let names: Vec<&str> = body["data"]["indices"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["name"].as_str())
        .collect();
    assert!(
        !names.iter().any(|n| n.starts_with(".xerj-memory-")),
        "brain names leaked into the listing: {names:?}"
    );
}
