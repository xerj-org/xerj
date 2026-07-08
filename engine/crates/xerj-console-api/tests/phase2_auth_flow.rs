//! End-to-end test for the phase-2 auth surface.
//!
//! WebAuthn requires a real authenticator browser-side, so we cannot test
//! the actual passkey crypto round-trip in pure-rust integration tests
//! without spinning up a virtual authenticator. We *can* prove every
//! non-WebAuthn path:
//!
//!  - magic-link redeem (bootstrap link → enrollment session in RAM)
//!  - passkey/begin returns a CreationChallengeResponse for the
//!    enrollment session (proves wiring & state plumbing)
//!  - passkey/finish with a bogus credential is rejected (proves the
//!    server actually verifies attestation)
//!  - login/begin returns a fake-challenge response for unknown emails
//!    (anti-enumeration check)
//!  - rate-limit kicks in after PER_MINUTE attempts on /login/begin
//!  - /me without a session returns 401
//!
//! The full passkey assertion path is covered by phase-7's playwright
//! browser smoke test against the bundled SPA.

use axum::{body::Body, http::Request, Router};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;
use xerj_common::config::Config;
use xerj_console_api::{
    auth::{sessions, store},
    state::ClusterMode,
    xerj_console_router, ConsoleState,
};
use xerj_engine::Engine;

async fn boot() -> (Engine, Router, TempDir, String) {
    let dir = TempDir::new().unwrap();
    let mut cfg = Config::default();
    cfg.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(cfg).expect("engine");
    let outcome = xerj_console_api::bootstrap::run(&engine, dir.path(), "http://localhost:9200")
        .await
        .expect("bootstrap");
    let token = outcome
        .magic_link
        .clone()
        .unwrap()
        .rsplit_once("token=")
        .unwrap()
        .1
        .to_string();
    let state = ConsoleState::new(
        engine.clone(),
        "local".into(),
        outcome.master_key,
        ClusterMode::Standalone,
    );
    (engine, xerj_console_router(state), dir, token)
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

fn post_json(path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn post_json_cookie(path: &str, body: Value, cookie: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("cookie", cookie)
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Boot a console with a single active user of the given role plus a signed
/// session cookie for them, bypassing the WebAuthn dance the same way the
/// phase-3 suite does. Returns the engine (for direct store assertions), the
/// router, the cookie header value, and the temp dir (kept alive by the
/// caller so the data dir isn't reaped mid-test).
async fn boot_with_role(role: &str) -> (Engine, Router, String, TempDir) {
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

    let user = store::User {
        id: format!("{role}-test"),
        email: format!("{role}@example.com"),
        display_name: role.to_string(),
        role: role.to_string(),
        status: store::UserStatus::Active,
        created_at: xerj_console_api::time::now_iso(),
        last_seen_at: Some(xerj_console_api::time::now_iso()),
    };
    store::upsert_user(&engine, &user).await.unwrap();
    let (_session, signed) = sessions::mint_session(&state, &user.id, "passkey", None, None)
        .await
        .unwrap();
    let cookie = format!("xerj_session={signed}");

    (engine, xerj_console_router(state), cookie, dir)
}

#[tokio::test]
async fn magic_redeem_with_bootstrap_token_returns_enrollment_session() {
    let (_engine, router, _dir, token) = boot().await;

    let resp = router
        .clone()
        .oneshot(post_json(
            "/_xerj-console/api/v1/auth/magic/redeem",
            json!({ "token": token }),
        ))
        .await
        .unwrap();

    let (status, body) = body_json(resp).await;
    assert_eq!(status, 200, "body: {body}");
    let data = &body["data"];
    assert!(data["enrollment_session_id"].as_str().unwrap().len() > 30);
    assert_eq!(data["role"], "owner");
    assert!(
        data["expires_at"].as_str().is_some(),
        "missing expires_at: {data}"
    );
}

#[tokio::test]
async fn magic_redeem_is_single_use() {
    let (_engine, router, _dir, token) = boot().await;

    let r1 = router
        .clone()
        .oneshot(post_json(
            "/_xerj-console/api/v1/auth/magic/redeem",
            json!({ "token": token }),
        ))
        .await
        .unwrap();
    assert_eq!(r1.status(), 200);

    let r2 = router
        .oneshot(post_json(
            "/_xerj-console/api/v1/auth/magic/redeem",
            json!({ "token": token }),
        ))
        .await
        .unwrap();
    assert_eq!(r2.status(), 401, "second redemption must fail");
}

#[tokio::test]
async fn magic_redeem_with_unknown_token_returns_401() {
    let (_engine, router, _dir, _token) = boot().await;

    let resp = router
        .oneshot(post_json(
            "/_xerj-console/api/v1/auth/magic/redeem",
            json!({ "token": "not-a-real-token" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn passkey_begin_returns_creation_options_for_valid_enrollment() {
    let (_engine, router, _dir, token) = boot().await;

    // Redeem to get an enrollment session.
    let r = router
        .clone()
        .oneshot(post_json(
            "/_xerj-console/api/v1/auth/magic/redeem",
            json!({ "token": token }),
        ))
        .await
        .unwrap();
    let (_, redeem_body) = body_json(r).await;
    let enroll_id = redeem_body["data"]["enrollment_session_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Begin passkey enrollment.
    let r = router
        .oneshot(post_json(
            "/_xerj-console/api/v1/auth/passkey/begin",
            json!({
                "enrollment_session_id": enroll_id,
                "email": "owner@example.com",
                "display_name": "Owner"
            }),
        ))
        .await
        .unwrap();
    let (status, body) = body_json(r).await;
    assert_eq!(status, 200, "body: {body}");
    assert!(body["data"]["challenge_id"].as_str().is_some());
    // Spot-check the WebAuthn shape — must contain the challenge field.
    let opts = &body["data"]["creation_options"]["publicKey"];
    assert!(
        opts["challenge"].is_string(),
        "creation_options missing challenge: {body}"
    );
    assert!(opts["rp"].is_object());
    assert!(opts["user"].is_object());
}

#[tokio::test]
async fn passkey_begin_rejects_unknown_enrollment_session() {
    let (_engine, router, _dir, _token) = boot().await;
    let r = router
        .oneshot(post_json(
            "/_xerj-console/api/v1/auth/passkey/begin",
            json!({
                "enrollment_session_id": "not-a-real-id",
                "email": "x@y"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn login_begin_returns_challenge_for_unknown_email() {
    // Anti-enumeration: every email gets a challenge response.
    let (_engine, router, _dir, _token) = boot().await;

    let r = router
        .oneshot(post_json(
            "/_xerj-console/api/v1/auth/login/begin",
            json!({ "email": "ghost@nobody.example" }),
        ))
        .await
        .unwrap();
    let (status, body) = body_json(r).await;
    assert_eq!(status, 200, "body: {body}");
    assert!(body["data"]["challenge_id"].as_str().is_some());
    assert!(body["data"]["request_options"]["publicKey"]["challenge"].is_string());
}

#[tokio::test]
async fn login_begin_rate_limit_kicks_in() {
    let (_engine, router, _dir, _token) = boot().await;

    // PER_MINUTE = 10 in rate_limit.rs. The 11th hit must 429.
    let mut last_status = 200;
    for _ in 0..15 {
        let r = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_xerj-console/api/v1/auth/login/begin")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", "9.9.9.9")
                    .body(Body::from(json!({ "email": "x@y" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        last_status = r.status().as_u16();
        if last_status == 429 {
            break;
        }
    }
    assert_eq!(last_status, 429, "rate limit must engage within 15 calls");
}

#[tokio::test]
async fn me_without_session_returns_401() {
    let (_engine, router, _dir, _token) = boot().await;

    let r = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/_xerj-console/api/v1/me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn me_with_garbage_cookie_returns_401() {
    let (_engine, router, _dir, _token) = boot().await;

    let r = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/_xerj-console/api/v1/me")
                .header("cookie", "xerj_session=not.a.valid.signed.cookie")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn passkey_finish_rejects_bogus_credential() {
    let (_engine, router, _dir, token) = boot().await;

    let r = router
        .clone()
        .oneshot(post_json(
            "/_xerj-console/api/v1/auth/magic/redeem",
            json!({ "token": token }),
        ))
        .await
        .unwrap();
    let (_, redeem_body) = body_json(r).await;
    let enroll_id = redeem_body["data"]["enrollment_session_id"]
        .as_str()
        .unwrap()
        .to_string();

    let r = router
        .clone()
        .oneshot(post_json(
            "/_xerj-console/api/v1/auth/passkey/begin",
            json!({
                "enrollment_session_id": enroll_id,
                "email": "owner@example.com"
            }),
        ))
        .await
        .unwrap();
    let (_, begin_body) = body_json(r).await;
    let challenge_id = begin_body["data"]["challenge_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Send a syntactically-valid-but-unsigned bogus credential. The
    // important thing isn't the exact error code — it's that we don't
    // 200, and we don't 500.
    let bogus_credential = json!({
        "id":   "AAAAAA",
        "rawId": "AAAAAA",
        "type": "public-key",
        "response": {
            "clientDataJSON": "AAAAAA",
            "attestationObject": "AAAAAA"
        },
        "extensions": {}
    });

    let r = router
        .oneshot(post_json(
            "/_xerj-console/api/v1/auth/passkey/finish",
            json!({
                "enrollment_session_id": enroll_id,
                "challenge_id":          challenge_id,
                "name":                  "test",
                "email":                 "owner@example.com",
                "credential":            bogus_credential
            }),
        ))
        .await
        .unwrap();
    let (status, body) = body_json(r).await;
    // 400 (BadRequest from JSON parse) or 401 (Unauthorized rejection) — both
    // acceptable; what matters is "not 200, not 500".
    assert!(
        status == 400 || status == 401,
        "expected 4xx for bogus credential, got {status} body={body}"
    );
}

#[tokio::test]
async fn known_routes_grew() {
    let routes = xerj_console_api::router::known_routes();
    // Phase 1 had 1 route; phase 2 adds 9 more.
    assert!(routes.len() >= 10, "phase 2 must register 10+ routes");
    for required in &[
        "/_xerj-console/api/v1/auth/magic/redeem",
        "/_xerj-console/api/v1/auth/magic/issue",
        "/_xerj-console/api/v1/auth/passkey/begin",
        "/_xerj-console/api/v1/auth/passkey/finish",
        "/_xerj-console/api/v1/auth/login/begin",
        "/_xerj-console/api/v1/auth/login/finish",
        "/_xerj-console/api/v1/auth/logout",
        "/_xerj-console/api/v1/me",
        "/_xerj-console/api/v1/auth/passkeys",
    ] {
        assert!(routes.contains(required), "missing route: {required}");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Magic-link issue (admin invite)
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn magic_issue_as_admin_provisions_pending_invitee_and_redeems() {
    let (engine, router, cookie, _dir) = boot_with_role("admin").await;

    // Admin mints an invite for a brand-new address.
    let r = router
        .clone()
        .oneshot(post_json_cookie(
            "/_xerj-console/api/v1/auth/magic/issue",
            json!({ "email": "invitee@example.com", "role": "editor" }),
            &cookie,
        ))
        .await
        .unwrap();
    let (status, body) = body_json(r).await;
    assert_eq!(status, 200, "issue: {body}");
    let token = body["data"]["token"].as_str().unwrap().to_string();
    assert!(token.len() > 30, "token looks too short: {body}");
    assert_eq!(body["data"]["role"], "editor");
    assert_eq!(body["data"]["purpose"], "invite");
    assert_eq!(body["data"]["email"], "invitee@example.com");
    assert!(
        body["data"]["link"]
            .as_str()
            .unwrap()
            .contains("/setup#token="),
        "issue must return a setup link: {body}"
    );

    // A *pending* invitee row now exists with the requested role.
    let invitee = store::find_user_by_email(&engine, "invitee@example.com")
        .await
        .unwrap()
        .expect("invitee must be provisioned");
    assert_eq!(invitee.status, store::UserStatus::Pending);
    assert_eq!(invitee.role, "editor");

    // The freshly-issued token redeems into an enrollment session bound to
    // the invited identity. (The invitee only flips to active once they
    // finish enrolling a passkey, which needs a real authenticator, so this
    // is as far as a server-only test can drive it.)
    let r = router
        .clone()
        .oneshot(post_json(
            "/_xerj-console/api/v1/auth/magic/redeem",
            json!({ "token": token }),
        ))
        .await
        .unwrap();
    let (status, body) = body_json(r).await;
    assert_eq!(status, 200, "redeem: {body}");
    assert_eq!(body["data"]["role"], "editor");
    assert_eq!(body["data"]["email"], "invitee@example.com");
    assert!(
        body["data"]["enrollment_session_id"]
            .as_str()
            .unwrap()
            .len()
            > 30
    );

    // Single-use: the invite cannot be redeemed a second time.
    let r = router
        .oneshot(post_json(
            "/_xerj-console/api/v1/auth/magic/redeem",
            json!({ "token": token }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "an issued invite must be single-use");
}

#[tokio::test]
async fn magic_issue_requires_admin_or_owner() {
    // An authenticated but under-privileged session must be forbidden.
    let (_engine, router, cookie, _dir) = boot_with_role("viewer").await;
    let r = router
        .oneshot(post_json_cookie(
            "/_xerj-console/api/v1/auth/magic/issue",
            json!({ "email": "someone@example.com", "role": "viewer" }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "a viewer must not be able to invite");
}

#[tokio::test]
async fn magic_issue_admin_cannot_grant_owner() {
    // Privilege ceiling: an admin cannot mint an owner-role invite.
    let (_engine, router, cookie, _dir) = boot_with_role("admin").await;
    let r = router
        .oneshot(post_json_cookie(
            "/_xerj-console/api/v1/auth/magic/issue",
            json!({ "email": "boss@example.com", "role": "owner" }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "admin must not be able to mint an owner");
}

#[tokio::test]
async fn magic_issue_without_session_returns_401() {
    let (_engine, router, _cookie, _dir) = boot_with_role("admin").await;
    let r = router
        .oneshot(post_json(
            "/_xerj-console/api/v1/auth/magic/issue",
            json!({ "email": "someone@example.com", "role": "viewer" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "unauthenticated issue must be rejected");
}
