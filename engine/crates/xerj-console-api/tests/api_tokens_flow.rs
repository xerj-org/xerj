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
//!   - `POST /auth/api-tokens` returns the plaintext secret exactly once,
//!     and only for a passkey-backed session cookie.
//!   - `GET  /auth/api-tokens` lists only the hash/metadata — no plaintext.
//!   - the plaintext used as `Authorization: Bearer` authenticates `/me`
//!     and other ordinary console calls.
//!   - the same bearer token is 403 on credential minting and identity
//!     changes (token create/revoke, invite issue, passkey delete, logout)
//!     and those refusals write nothing (CVE-2026-97406 / CERT VU#645818).
//!   - `DELETE /auth/api-tokens/:id` with the passkey cookie revokes it →
//!     the same Bearer call 401s.
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

const TOKENS: &str = "/_xerj-console/api/v1/auth/api-tokens";

async fn call(
    router: &Router,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    bearer: Option<&str>,
    json_body: Option<Value>,
) -> (axum::http::StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    if let Some(bearer) = bearer {
        builder = builder.header("authorization", format!("Bearer {bearer}"));
    }
    let req = match json_body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    body_json(router.clone().oneshot(req).await.unwrap()).await
}

fn assert_passkey_required(status: axum::http::StatusCode, body: &Value, what: &str) {
    assert_eq!(
        status, 403,
        "{what} must refuse a non-passkey caller: {body}"
    );
    assert_eq!(body["error"]["code"], "forbidden", "{what}: {body}");
    let message = body["error"]["message"].as_str().unwrap_or("");
    assert!(
        message.contains(sessions::PASSKEY_SESSION_REQUIRED),
        "{what} 403 must name the passkey-session rule, got {body}"
    );
}

async fn seed_passkey(engine: &Engine, user_id: &str, id: &str) {
    store::put_passkey(
        engine,
        &store::StoredPasskey {
            id: id.into(),
            user_id: user_id.into(),
            name: id.into(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            last_used_at: None,
            blob: json!({}),
        },
    )
    .await
    .unwrap();
}

struct Issued {
    engine: Engine,
    router: Router,
    // Kept so the data directory outlives the engine.
    _dir: TempDir,
    user_id: String,
    cookie: String,
    plaintext: String,
    token_id: String,
}

/// Each `call` builds an axum `Router::oneshot` future. Several of those in
/// one async fn overflow the test thread's stack in a debug build, so the
/// CVE checks are separate tests that share this setup.
async fn issue_with_passkey_cookie() -> Issued {
    let (engine, state, router, dir) = boot().await;
    let (user_id, cookie) = seed_user_and_cookie(&engine, &state).await;
    seed_passkey(&engine, &user_id, "pk-laptop").await;
    seed_passkey(&engine, &user_id, "pk-phone").await;
    let (status, body) = call(
        &router,
        "POST",
        TOKENS,
        Some(&cookie),
        None,
        Some(json!({ "name": "ci-token", "scopes": ["read"] })),
    )
    .await;
    assert_eq!(status, 200, "passkey cookie must still mint: {body}");
    Issued {
        engine,
        router,
        _dir: dir,
        user_id,
        cookie,
        plaintext: body["data"]["token"].as_str().unwrap().to_string(),
        token_id: body["data"]["id"].as_str().unwrap().to_string(),
    }
}

/// CVE-2026-97406 / CERT VU#645818. A bearer token used to resolve to the
/// same `AuthSession` as a passkey cookie, so it could mint a successor
/// that survived revocation of the parent. Bearer alone, and bearer sent
/// together with the passkey cookie, are both 403 and insert nothing.
#[tokio::test]
async fn bearer_token_cannot_mint_a_successor() {
    // Boxed so this test's future does not embed the setup future. Several
    // `Router::oneshot` futures in one frame overflow the debug stack.
    let issued = Box::pin(issue_with_passkey_cookie()).await;

    for (label, cookie_hdr) in [
        ("bearer alone", None),
        (
            "bearer plus the passkey cookie",
            Some(issued.cookie.as_str()),
        ),
    ] {
        let name = format!("successor-{label}");
        let (status, body) = call(
            &issued.router,
            "POST",
            TOKENS,
            cookie_hdr,
            Some(&issued.plaintext),
            Some(json!({ "name": name })),
        )
        .await;
        assert_passkey_required(status, &body, label);
    }
    let stored = store::list_api_tokens_for_user(&issued.engine, &issued.user_id)
        .await
        .unwrap();
    assert_eq!(
        stored.len(),
        1,
        "a refused mint must not insert a descendant: {stored:?}"
    );
    assert_eq!(stored[0].id, issued.token_id);
    assert!(stored[0].revoked_at.is_none());

    // An unknown bearer is 401, not the passkey-session 403.
    let (status, body) = call(
        &issued.router,
        "POST",
        TOKENS,
        None,
        Some("not-a-real-token"),
        Some(json!({ "name": "nope" })),
    )
    .await;
    assert_eq!(status, 401, "an unknown bearer is unauthenticated: {body}");
    assert_eq!(body["error"]["code"], "unauthorized");
}

/// Revoking a token, issuing an invitation, and deleting a passkey are the
/// same class of operation. A bearer token is 403 and the rows stay put.
/// The passkey cookie can still delete a passkey.
#[tokio::test]
async fn bearer_token_cannot_revoke_invite_or_delete_a_passkey() {
    // Boxed so this test's future does not embed the setup future. Several
    // `Router::oneshot` futures in one frame overflow the debug stack.
    let issued = Box::pin(issue_with_passkey_cookie()).await;

    let (status, body) = call(
        &issued.router,
        "DELETE",
        &format!("{TOKENS}/{}", issued.token_id),
        None,
        Some(&issued.plaintext),
        None,
    )
    .await;
    assert_passkey_required(status, &body, "revoke");
    let (status, body) = call(
        &issued.router,
        "GET",
        "/_xerj-console/api/v1/me",
        None,
        Some(&issued.plaintext),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "refused revoke must leave the token working: {body}"
    );

    let (status, body) = call(
        &issued.router,
        "POST",
        "/_xerj-console/api/v1/auth/magic/issue",
        None,
        Some(&issued.plaintext),
        Some(json!({ "email": "attacker@example.com", "role": "owner" })),
    )
    .await;
    assert_passkey_required(status, &body, "invite issue");
    assert!(
        store::find_user_by_email(&issued.engine, "attacker@example.com")
            .await
            .unwrap()
            .is_none(),
        "a refused invite must not provision the invitee"
    );

    let (status, body) = call(
        &issued.router,
        "DELETE",
        "/_xerj-console/api/v1/auth/passkeys/pk-laptop",
        None,
        Some(&issued.plaintext),
        None,
    )
    .await;
    assert_passkey_required(status, &body, "passkey delete");
    let passkeys = store::list_passkeys_for_user(&issued.engine, &issued.user_id)
        .await
        .unwrap();
    assert_eq!(passkeys.len(), 2, "refused delete must keep both passkeys");

    // Two passkeys are enrolled, so the last-passkey guard does not fire.
    let (status, _) = call(
        &issued.router,
        "DELETE",
        "/_xerj-console/api/v1/auth/passkeys/pk-phone",
        Some(&issued.cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, 204, "passkey cookie must still delete a passkey");
}

/// Logout revokes the passkey session only. A bearer token cannot end that
/// session, and ending it does not revoke the token.
#[tokio::test]
async fn bearer_logout_does_not_revoke_the_passkey_session() {
    // Boxed so this test's future does not embed the setup future. Several
    // `Router::oneshot` futures in one frame overflow the debug stack.
    let issued = Box::pin(issue_with_passkey_cookie()).await;

    let (status, body) = call(
        &issued.router,
        "POST",
        "/_xerj-console/api/v1/auth/logout",
        None,
        Some(&issued.plaintext),
        None,
    )
    .await;
    assert_passkey_required(status, &body, "logout");
    let (status, body) = call(
        &issued.router,
        "GET",
        "/_xerj-console/api/v1/me",
        Some(&issued.cookie),
        None,
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "bearer logout must not revoke the passkey session: {body}"
    );

    let (status, _) = call(
        &issued.router,
        "POST",
        "/_xerj-console/api/v1/auth/logout",
        Some(&issued.cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "passkey cookie must still be able to log out");
    let (status, _) = call(
        &issued.router,
        "GET",
        "/_xerj-console/api/v1/me",
        Some(&issued.cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, 401, "logout must revoke the passkey session");
    let (status, _) = call(
        &issued.router,
        "GET",
        "/_xerj-console/api/v1/me",
        None,
        Some(&issued.plaintext),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "logging out the browser session must not revoke the API token"
    );
}

/// Listing credential metadata and writing ordinary console data stay
/// available to a bearer token.
#[tokio::test]
async fn bearer_token_still_serves_ordinary_console_calls() {
    // Boxed so this test's future does not embed the setup future. Several
    // `Router::oneshot` futures in one frame overflow the debug stack.
    let issued = Box::pin(issue_with_passkey_cookie()).await;

    let (status, body) = call(
        &issued.router,
        "GET",
        TOKENS,
        None,
        Some(&issued.plaintext),
        None,
    )
    .await;
    assert_eq!(status, 200, "listing tokens is a metadata read: {body}");
    assert_eq!(body["data"]["tokens"].as_array().unwrap().len(), 1);
    assert!(
        !body.to_string().contains(&issued.plaintext),
        "plaintext secret leaked into the list response"
    );

    let (status, body) = call(
        &issued.router,
        "GET",
        "/_xerj-console/api/v1/auth/passkeys",
        None,
        Some(&issued.plaintext),
        None,
    )
    .await;
    assert_eq!(status, 200, "listing passkeys is a metadata read: {body}");
    assert_eq!(body["data"]["passkeys"].as_array().unwrap().len(), 2);

    let (status, body) = call(
        &issued.router,
        "PUT",
        "/_xerj-console/api/v1/prefs",
        None,
        Some(&issued.plaintext),
        Some(json!({ "theme": "light" })),
    )
    .await;
    assert_eq!(
        status, 200,
        "a data write must still accept a bearer token: {body}"
    );
    assert_eq!(body["data"]["theme"], "light");
}

/// A browser session that was not minted by a passkey assertion is the
/// same class of refusal. SSO cookies are not issued today; the idp check
/// is what will refuse them if they are.
#[tokio::test]
async fn non_passkey_browser_session_cannot_mint_an_api_token() {
    let (engine, state, router, _dir) = boot().await;
    let user = store::User {
        id: "u-oidc".into(),
        email: "oidc@example.com".into(),
        display_name: "Oidc".into(),
        role: "owner".into(),
        status: store::UserStatus::Active,
        created_at: "2026-01-01T00:00:00.000Z".into(),
        last_seen_at: None,
    };
    store::upsert_user(&engine, &user).await.unwrap();
    let (_sess, signed) = sessions::mint_session(&state, &user.id, "oidc", None, None)
        .await
        .unwrap();
    let cookie = format!("{}={signed}", sessions::COOKIE_NAME);

    let (status, body) = call(
        &router,
        "POST",
        TOKENS,
        Some(&cookie),
        None,
        Some(json!({ "name": "from-oidc" })),
    )
    .await;
    assert_passkey_required(status, &body, "oidc session");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("oidc"),
        "the 403 must name the session idp: {body}"
    );
    assert!(
        store::list_api_tokens_for_user(&engine, &user.id)
            .await
            .unwrap()
            .is_empty(),
        "the refused mint must not insert a token"
    );

    // The same cookie still authenticates an ordinary read.
    let (status, body) = call(
        &router,
        "GET",
        "/_xerj-console/api/v1/me",
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "a non-passkey session is still a session: {body}"
    );
    assert_eq!(body["data"]["user"]["id"], user.id);
}
