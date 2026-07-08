//! End-to-end test for the API-token surface (issue / list / revoke) and
//! `Authorization: Bearer` authentication.
//!
//! WebAuthn can't be driven from a pure-Rust test (no browser
//! authenticator), so instead of walking the passkey enrolment flow we
//! seed an active user directly and mint a signed session cookie with the
//! same `sessions::mint_session` the login handler uses. That cookie
//! authenticates the issue/list/revoke calls; the minted token then
//! authenticates a protected endpoint over Bearer.
//!
//! Proven here:
//!   - `POST /auth/api-tokens` returns the plaintext secret exactly once.
//!   - `GET  /auth/api-tokens` lists only the hash/metadata — no plaintext.
//!   - the plaintext used as `Authorization: Bearer` authenticates `/me`.
//!   - `DELETE /auth/api-tokens/:id` revokes it → the same Bearer call 401s.
//!   - issuing a token without a session is rejected (401).

use axum::{body::Body, http::Request, Router};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;
use xerj_common::config::Config;
use xerj_console_api::auth::{sessions, store};
use xerj_console_api::{state::ClusterMode, xerj_console_router, ConsoleState};
use xerj_engine::Engine;

async fn boot() -> (Engine, ConsoleState, Router, TempDir) {
    let dir = TempDir::new().unwrap();
    let mut cfg = Config::default();
    cfg.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(cfg).expect("engine");
    let outcome = xerj_console_api::bootstrap::run(&engine, dir.path(), "http://localhost:9200")
        .await
        .expect("bootstrap");
    let state = ConsoleState::new(
        engine.clone(),
        "local".into(),
        outcome.master_key,
        ClusterMode::Standalone,
    );
    let router = xerj_console_router(state.clone());
    (engine, state, router, dir)
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

/// Seed an active user + a signed session cookie for it. Returns the
/// `Cookie:` header value the session-protected endpoints accept.
async fn seed_user_and_cookie(engine: &Engine, state: &ConsoleState) -> (String, String) {
    let user = store::User {
        id: "u-token-test".into(),
        email: "owner@example.com".into(),
        display_name: "Owner".into(),
        role: "owner".into(),
        status: store::UserStatus::Active,
        created_at: "2026-01-01T00:00:00.000Z".into(),
        last_seen_at: None,
    };
    store::upsert_user(engine, &user).await.unwrap();

    let (_sess, signed) = sessions::mint_session(state, &user.id, "passkey", None, None)
        .await
        .unwrap();
    (user.id, format!("{}={}", sessions::COOKIE_NAME, signed))
}

#[tokio::test]
async fn api_token_issue_list_bearer_and_revoke() {
    let (engine, state, router, _dir) = boot().await;
    let (user_id, cookie) = seed_user_and_cookie(&engine, &state).await;

    // ── Issue ──────────────────────────────────────────────────────────
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/_xerj-console/api/v1/auth/api-tokens")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(
                    json!({ "name": "ci-token", "scopes": ["read"] }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = body_json(resp).await;
    assert_eq!(status, 200, "issue must succeed: {body}");
    let plaintext = body["data"]["token"].as_str().unwrap().to_string();
    let token_id = body["data"]["id"].as_str().unwrap().to_string();
    assert!(
        plaintext.len() >= 40,
        "secret should be 32 bytes url-safe b64"
    );
    assert_ne!(plaintext, token_id, "id must be the hash, not the secret");
    assert_eq!(body["data"]["name"], "ci-token");

    // ── List: metadata only, never the plaintext ───────────────────────
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/_xerj-console/api/v1/auth/api-tokens")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = body_json(resp).await;
    assert_eq!(status, 200, "list must succeed: {body}");
    let tokens = body["data"]["tokens"].as_array().unwrap();
    assert_eq!(tokens.len(), 1, "exactly one token: {body}");
    assert_eq!(tokens[0]["id"], token_id);
    assert!(
        !body.to_string().contains(&plaintext),
        "plaintext secret leaked into the list response"
    );

    // ── Bearer auth on a protected endpoint → 200 ──────────────────────
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/_xerj-console/api/v1/me")
                .header("authorization", format!("Bearer {plaintext}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = body_json(resp).await;
    assert_eq!(status, 200, "bearer auth must authenticate: {body}");
    assert_eq!(body["data"]["user"]["id"], user_id);

    // ── Revoke ─────────────────────────────────────────────────────────
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/_xerj-console/api/v1/auth/api-tokens/{token_id}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 204, "revoke must return 204");

    // ── Same Bearer call now 401s ──────────────────────────────────────
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/_xerj-console/api/v1/me")
                .header("authorization", format!("Bearer {plaintext}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        401,
        "a revoked token must no longer authenticate"
    );
}

#[tokio::test]
async fn api_token_issue_requires_a_session() {
    let (_engine, _state, router, _dir) = boot().await;

    // No cookie, no bearer → the AuthSession extractor rejects with 401.
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/_xerj-console/api/v1/auth/api-tokens")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "name": "x" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "issuing a token needs authentication");
}

#[tokio::test]
async fn bogus_bearer_token_is_rejected() {
    let (_engine, _state, router, _dir) = boot().await;

    let resp = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/_xerj-console/api/v1/me")
                .header("authorization", "Bearer not-a-real-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        401,
        "unknown bearer token must not authenticate"
    );
}
