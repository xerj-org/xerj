//! Cookie-backed sessions.
//!
//! After a successful passkey assertion (enrollment-finish or login-finish)
//! the server mints a session id, persists a `.xerj_sessions` doc, and
//! returns a `Set-Cookie: xerj_session=<id>.<sig>` header. The signature
//! is HMAC-SHA256 of the session id keyed by `ConsoleState.master_key`.
//!
//! Cookie attributes:
//! - `HttpOnly`: blocks JS from reading the cookie (mitigates XSS theft).
//! - `Secure`: not set on http/localhost (so dev works) — set on https
//!   when the request was over TLS.
//! - `SameSite=Lax`: protects against cross-site POST CSRF; `Lax` (not
//!   `Strict`) lets the SPA bookmark links work cross-origin top-nav.
//! - `Path=/_xerj-console`: scoped tightly so xerj's other surfaces never see
//!   the cookie even when proxied alongside.
//!
//! Idle expiry: a session is considered fresh if `now < expires_at AND
//! now < last_seen_at + idle_seconds`. `last_seen_at` is bumped on
//! every authenticated request via the extractor.

use axum::{extract::FromRequestParts, http::request::Parts};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::auth::store;
use crate::bootstrap::sha256_hex;
use crate::error::{ConsoleApiError, ConsoleResult};
use crate::state::ConsoleState;
use crate::time::{epoch_ms_to_iso, now_epoch_ms, now_iso, parse_iso};
use xerj_engine::Engine;

type HmacSha256 = Hmac<Sha256>;

pub const COOKIE_NAME: &str = "xerj_session";
pub const SESSION_TTL_MS: i64 = 12 * 60 * 60 * 1000; // 12 hours hard expiry
pub const SESSION_IDLE_MS: i64 = 30 * 60 * 1000; //  30 minutes idle

// ─────────────────────────────────────────────────────────────────────────────
// Mint
// ─────────────────────────────────────────────────────────────────────────────

/// Create a fresh session for `user_id`, persist it, and return both the
/// row and the cookie value the SPA should echo back.
pub async fn mint_session(
    state: &ConsoleState,
    user_id: &str,
    idp: &str,
    ip: Option<String>,
    ua: Option<String>,
) -> ConsoleResult<(store::Session, String)> {
    let mut id_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut id_bytes);
    let session_id = URL_SAFE_NO_PAD.encode(id_bytes);

    let now_ms = now_epoch_ms();
    let now_iso_s = now_iso();
    let session = store::Session {
        id: session_id.clone(),
        user_id: user_id.to_string(),
        created_at: now_iso_s.clone(),
        expires_at: epoch_ms_to_iso(now_ms + SESSION_TTL_MS),
        last_seen_at: now_iso_s,
        ip,
        ua,
        idp: idp.to_string(),
        revoked_at: None,
    };
    store::put_session(&state.engine, &session).await?;

    let signed = sign_session_id(&session_id, &state.master_key);
    Ok((session, signed))
}

/// Sign `session_id` with the master key. Output is `<id>.<sig_b64u>`.
pub fn sign_session_id(session_id: &str, master_key: &[u8; 32]) -> String {
    let mut mac = HmacSha256::new_from_slice(master_key).expect("hmac key");
    mac.update(session_id.as_bytes());
    let sig = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    format!("{session_id}.{sig}")
}

/// Verify and split a `<id>.<sig>` cookie. Constant-time comparison.
pub fn verify_session_cookie(value: &str, master_key: &[u8; 32]) -> Option<String> {
    let (id, sig) = value.split_once('.')?;
    let expected = sign_session_id(id, master_key);
    let (_id_part, expected_sig) = expected.rsplit_once('.')?;
    if expected_sig.as_bytes().ct_eq(sig.as_bytes()).into() {
        Some(id.to_string())
    } else {
        None
    }
}

/// Build the `Set-Cookie` value for a freshly minted session.
pub fn make_set_cookie(value: String, secure: bool) -> Cookie<'static> {
    let mut c = Cookie::new(COOKIE_NAME.to_string(), value);
    c.set_http_only(true);
    c.set_secure(secure);
    c.set_same_site(SameSite::Lax);
    c.set_path("/_xerj-console");
    c
}

/// Build a `Set-Cookie` that immediately deletes the session cookie.
pub fn make_clear_cookie() -> Cookie<'static> {
    let mut c = Cookie::new(COOKIE_NAME.to_string(), String::new());
    c.set_http_only(true);
    c.set_same_site(SameSite::Lax);
    c.set_path("/_xerj-console");
    c.set_max_age(time::Duration::seconds(0));
    c
}

// ─────────────────────────────────────────────────────────────────────────────
// Extractor
// ─────────────────────────────────────────────────────────────────────────────

/// How this request authenticated.
///
/// A cookie session and an API token both become an [`AuthSession`], so
/// ordinary console reads and writes can accept either. They are not the
/// same credential. [`PasskeySession`] is the passkey-backed browser session
/// a handler must hold before it mints or changes credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionOrigin {
    /// `xerj_session` cookie. `idp` is the `.xerj_sessions` row's identity
    /// provider. Only `"passkey"` satisfies [`AuthSession::require_passkey_session`].
    BrowserSession { idp: String },
    /// `Authorization: Bearer <api token>`. `token_id` is `sha256(secret)`.
    ApiToken { token_id: String },
}

/// Stable text of the 403 returned when a credential-minting or
/// identity-changing handler is reached without a passkey-backed browser
/// session. The console tests and `docs/SECURITY_MODEL.md` quote it.
pub const PASSKEY_SESSION_REQUIRED: &str = concat!(
    "this operation requires a passkey-backed browser session; ",
    "API tokens cannot mint or change credentials"
);

/// Authenticated caller, from the `xerj_session` cookie or from
/// `Authorization: Bearer <api token>`.
///
/// Use as a handler parameter to require authentication. Returns 401 on a
/// missing credential, a bad signature, an expired or revoked session, or
/// an unknown or revoked token.
///
/// This type does **not** mean the caller presented a passkey. Credential
/// minting and identity changes take [`PasskeySession`] instead, which
/// returns 403 for a valid bearer token (CVE-2026-97406 / CERT VU#645818).
#[derive(Debug, Clone)]
pub struct AuthSession {
    pub session_id: String,
    pub user: store::User,
    pub origin: SessionOrigin,
}

impl AuthSession {
    /// Refuse unless this request presented a passkey-backed browser session.
    ///
    /// A valid API token is a 403, not a 401: the caller is authenticated
    /// and the operation is forbidden. Cookie sessions and bearer tokens
    /// used to resolve to the same `AuthSession`, so `POST /auth/api-tokens`
    /// would mint a successor that survived revocation of the parent.
    pub fn require_passkey_session(&self) -> ConsoleResult<()> {
        match &self.origin {
            SessionOrigin::BrowserSession { idp } if idp == "passkey" => Ok(()),
            other => Err(ConsoleApiError::Forbidden(refusal_message(other))),
        }
    }
}

fn refusal_message(origin: &SessionOrigin) -> String {
    match origin {
        SessionOrigin::ApiToken { .. } => PASSKEY_SESSION_REQUIRED.to_string(),
        SessionOrigin::BrowserSession { idp } => {
            format!("{PASSKEY_SESSION_REQUIRED} (this session's idp is {idp}, not passkey)")
        }
    }
}

#[axum::async_trait]
impl FromRequestParts<ConsoleState> for AuthSession {
    type Rejection = ConsoleApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ConsoleState,
    ) -> Result<Self, Self::Rejection> {
        // API clients authenticate with `Authorization: Bearer <token>`;
        // browsers with a signed session cookie. Bearer wins when the header
        // is present, including when the token is invalid: a token-carrying
        // request is never upgraded onto a cookie that happens to be
        // attached. `PasskeySession` then refuses a *valid* bearer with 403.
        // Preferring the cookie whenever both are present would let that
        // request pass the passkey gate (CVE-2026-97406).
        if let Some(token) = bearer_token(&parts.headers) {
            return authenticate_bearer(state, &token).await;
        }

        let jar = CookieJar::from_headers(&parts.headers);
        let cookie = jar
            .get(COOKIE_NAME)
            .ok_or_else(|| ConsoleApiError::Unauthorized("no session".into()))?;
        let session_id = verify_session_cookie(cookie.value(), &state.master_key)
            .ok_or_else(|| ConsoleApiError::Unauthorized("bad session signature".into()))?;

        let session = store::get_session(&state.engine, &session_id)
            .await?
            .ok_or_else(|| ConsoleApiError::Unauthorized("unknown session".into()))?;

        if session.revoked_at.is_some() {
            return Err(ConsoleApiError::Unauthorized("session revoked".into()));
        }
        let now_ms = now_epoch_ms();
        if let Some(exp) = parse_iso(&session.expires_at) {
            if now_ms > exp.timestamp_millis() {
                return Err(ConsoleApiError::Unauthorized("session expired".into()));
            }
        }
        if let Some(seen) = parse_iso(&session.last_seen_at) {
            if now_ms - seen.timestamp_millis() > SESSION_IDLE_MS {
                return Err(ConsoleApiError::Unauthorized("session idle".into()));
            }
        }

        let user = store::get_user(&state.engine, &session.user_id)
            .await?
            .ok_or_else(|| ConsoleApiError::Unauthorized("session user gone".into()))?;
        if user.status != store::UserStatus::Active {
            return Err(ConsoleApiError::Unauthorized("user disabled".into()));
        }

        // Bump last_seen so the session stays fresh while the user is active.
        // Best-effort — we don't fail the request if the write races with another
        // tab. SECURITY (S5-1): do NOT blind-write the session we read at the top
        // of this function. A concurrent revoke_session() between that read and
        // this write would be clobbered by the stale copy (revoked_at: None),
        // resurrecting a revoked session — a revocation bypass. Re-read and bump
        // only the fresh record, skipping entirely if it is now revoked or gone.
        bump_last_seen(&state.engine, &session_id).await;

        Ok(AuthSession {
            session_id,
            user,
            origin: SessionOrigin::BrowserSession { idp: session.idp },
        })
    }
}

/// Passkey-backed browser session.
///
/// Extract this instead of [`AuthSession`] for any handler that mints a
/// credential or changes identity. A missing credential is 401. A valid
/// `Authorization: Bearer` API token, and a browser session whose `idp` is
/// not `"passkey"`, are 403 with [`PASSKEY_SESSION_REQUIRED`]. The check
/// runs in the extractor, before the handler, so a refusal writes nothing.
///
/// # Handler classification
///
/// **Requires this type** (credential minting or identity change):
/// - `POST /auth/api-tokens` — mint an API token (`tokens::create`)
/// - `DELETE /auth/api-tokens/:id` — revoke an API token (`tokens::revoke`)
/// - `POST /auth/magic/issue` — mint an invitation (`magic::issue`)
/// - `DELETE /auth/passkeys/:id` — delete a passkey (`me::delete_passkey`)
/// - `POST /auth/logout` — revoke the current browser session (`login::logout`)
///
/// **[`AuthSession`] is enough** (ordinary console data; bearer tokens work):
/// - `GET /me`, `GET /auth/passkeys`, `GET /auth/api-tokens` (metadata only)
/// - `GET`/`PUT /prefs`
/// - dashboard and saved-view CRUD, including `_bulk`
/// - data-source connection, index, field, and search routes
/// - `GET /knowledge` and the `/graph/*` reads
///
/// **Not an [`AuthSession`] route.** These authorize with a different
/// credential, and a bearer token does not unlock them:
/// - `POST /auth/magic/redeem` — the magic link is the credential. The
///   invitee has no session yet; requiring one would break bootstrap.
///   A stolen API token cannot issue the link once `magic/issue` is gated.
/// - `POST /auth/passkey/begin` and `/finish` — the in-memory enrollment
///   session from redeem. There is no "enroll a passkey while holding an
///   API token" path.
/// - `POST /auth/login/begin` and `/finish` — WebAuthn, unauthenticated.
///
/// **No handler exists** for user/role management (`/users`) or for
/// revoking a session other than the current cookie via `logout`. A future
/// SSO cookie (`idp` other than `"passkey"`) fails this extractor, so it
/// cannot mint credentials either.
///
/// No API-token row records a parent. `tokens::create` is the only insert
/// of a new token, and it takes this type, so a bearer token cannot mint a
/// descendant for revocation to miss.
#[derive(Debug, Clone)]
pub struct PasskeySession {
    pub session_id: String,
    pub user: store::User,
}

#[axum::async_trait]
impl FromRequestParts<ConsoleState> for PasskeySession {
    type Rejection = ConsoleApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ConsoleState,
    ) -> Result<Self, Self::Rejection> {
        let sess = AuthSession::from_request_parts(parts, state).await?;
        sess.require_passkey_session()?;
        Ok(PasskeySession {
            session_id: sess.session_id,
            user: sess.user,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Bearer (API token) authentication
// ─────────────────────────────────────────────────────────────────────────────

/// Refresh a session's `last_seen_at` without clobbering a concurrent revocation.
///
/// S5-1 fix: re-reads the session immediately before writing and skips the write
/// if it is now gone or revoked, so a `revoke_session()` that landed after the
/// request's initial auth read cannot be overwritten by a stale copy. The write
/// carries the *fresh* record's fields (including its `revoked_at`), so even the
/// tiny residual window between this re-read and the put cannot resurrect a
/// revoked session — it would only rewrite an already-revoked record unchanged.
/// Best-effort: any store error is swallowed, exactly as before.
pub(crate) async fn bump_last_seen(engine: &Engine, session_id: &str) {
    let fresh = match store::get_session(engine, session_id).await {
        Ok(Some(s)) => s,
        _ => return, // gone (revoked-by-delete or expired-out) → do not recreate
    };
    if fresh.revoked_at.is_some() {
        return; // revoked between auth read and now → must not refresh
    }
    let mut bumped = fresh;
    bumped.last_seen_at = now_iso();
    let _ = store::put_session(engine, &bumped).await;
}

/// Pull the secret out of an `Authorization: Bearer <secret>` header.
/// Returns `None` when the header is absent, is not a bearer scheme, or
/// carries an empty value — the caller then falls back to the cookie path.
fn bearer_token(headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let rest = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))?;
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Authenticate a presented API token: hash it, look up a non-revoked
/// `ApiToken` row (see `auth::tokens`), and resolve its owner. Failure
/// semantics mirror the cookie path — every mismatch is a flat 401 that
/// never reveals which check failed.
async fn authenticate_bearer(
    state: &ConsoleState,
    presented: &str,
) -> Result<AuthSession, ConsoleApiError> {
    let hash = sha256_hex(presented.as_bytes());
    let token = store::get_api_token(&state.engine, &hash)
        .await?
        .ok_or_else(|| ConsoleApiError::Unauthorized("invalid api token".into()))?;
    if token.revoked_at.is_some() {
        return Err(ConsoleApiError::Unauthorized("api token revoked".into()));
    }
    let user = store::get_user(&state.engine, &token.user_id)
        .await?
        .ok_or_else(|| ConsoleApiError::Unauthorized("api token user gone".into()))?;
    if user.status != store::UserStatus::Active {
        return Err(ConsoleApiError::Unauthorized("user disabled".into()));
    }
    // `session_id` is the token id (= hash), not a `.xerj_sessions` id.
    // `PasskeySession` refuses this origin before a credential handler runs,
    // so logout cannot treat the hash as a browser session to revoke.
    let token_id = token.id.clone();
    Ok(AuthSession {
        session_id: token_id.clone(),
        user,
        origin: SessionOrigin::ApiToken { token_id },
    })
}

/// Allow `Option<AuthSession>` extractors for endpoints that work
/// signed-in *or* signed-out (e.g. `/me` returns 401 explicitly rather
/// than relying on the rejection so the SPA can branch on it).
#[axum::async_trait]
impl FromRequestParts<ConsoleState> for OptionalAuthSession {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ConsoleState,
    ) -> Result<Self, Self::Rejection> {
        match AuthSession::from_request_parts(parts, state).await {
            Ok(s) => Ok(OptionalAuthSession(Some(s))),
            Err(_) => Ok(OptionalAuthSession(None)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct OptionalAuthSession(pub Option<AuthSession>);

/// Convert an extractor rejection into a JSON 401 (matches the rest of
/// the surface).
impl From<ConsoleApiError> for axum::response::Response {
    fn from(e: ConsoleApiError) -> Self {
        use axum::response::IntoResponse;
        e.into_response()
    }
}

/// Helper for handlers that need to know if the request hit us over
/// HTTPS. We don't have direct TLS info inside axum (that's at the
/// listener layer), so we approximate by checking the `forwarded` /
/// `x-forwarded-proto` headers a reverse proxy sets, defaulting to
/// false (= http) so the local dev cookie still works.
pub fn request_is_secure(parts: &Parts) -> bool {
    if let Some(v) = parts.headers.get("x-forwarded-proto") {
        if v.as_bytes().eq_ignore_ascii_case(b"https") {
            return true;
        }
    }
    if let Some(v) = parts.headers.get("forwarded") {
        if let Ok(s) = v.to_str() {
            for part in s.split(';') {
                if part.trim().to_ascii_lowercase().starts_with("proto=https") {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_round_trip() {
        let key = [7u8; 32];
        let signed = sign_session_id("abc123", &key);
        let id = verify_session_cookie(&signed, &key).unwrap();
        assert_eq!(id, "abc123");
    }

    #[test]
    fn cookie_rejects_tampered_sig() {
        let key = [7u8; 32];
        let signed = sign_session_id("abc123", &key);
        let mut chars: Vec<char> = signed.chars().collect();
        // flip one byte in the signature half
        let sig_start = signed.find('.').unwrap() + 1;
        chars[sig_start] = if chars[sig_start] == 'a' { 'b' } else { 'a' };
        let tampered: String = chars.into_iter().collect();
        assert!(verify_session_cookie(&tampered, &key).is_none());
    }

    #[test]
    fn cookie_rejects_wrong_key() {
        let signed = sign_session_id("abc123", &[7u8; 32]);
        assert!(verify_session_cookie(&signed, &[8u8; 32]).is_none());
    }

    async fn test_engine() -> (Engine, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let mut cfg = xerj_common::config::Config::default();
        cfg.server.data_dir = dir.path().to_str().unwrap().to_string();
        let engine = Engine::new(cfg).expect("engine");
        crate::bootstrap::run(&engine, dir.path(), "http://localhost:9200")
            .await
            .expect("bootstrap");
        (engine, dir)
    }

    fn session(id: &str, revoked: bool) -> store::Session {
        store::Session {
            id: id.to_string(),
            user_id: "u1".into(),
            created_at: now_iso(),
            expires_at: crate::time::epoch_ms_to_iso(now_epoch_ms() + 3_600_000),
            last_seen_at: crate::time::epoch_ms_to_iso(now_epoch_ms() - 600_000),
            ip: None,
            ua: None,
            idp: "passkey".into(),
            revoked_at: if revoked { Some(now_iso()) } else { None },
        }
    }

    /// S5-1: bump_last_seen must NOT resurrect a session that was revoked before
    /// the bump. Pre-fix, the extractor blind-wrote a stale (revoked_at: None)
    /// copy, un-revoking it — a revocation bypass.
    #[tokio::test]
    async fn bump_does_not_resurrect_a_revoked_session() {
        let (engine, _dir) = test_engine().await;
        // A session that is live, then revoked (the realistic race outcome:
        // revoke lands before the in-flight request's bump).
        store::put_session(&engine, &session("s-revoked", false))
            .await
            .unwrap();
        store::revoke_session(&engine, "s-revoked", &now_iso())
            .await
            .unwrap();

        bump_last_seen(&engine, "s-revoked").await;

        let after = store::get_session(&engine, "s-revoked").await.unwrap();
        assert!(
            after.is_some() && after.unwrap().revoked_at.is_some(),
            "bump must not clear revoked_at"
        );
    }

    /// A live session's bump still refreshes last_seen (the feature still works).
    #[tokio::test]
    async fn bump_refreshes_a_live_session() {
        let (engine, _dir) = test_engine().await;
        let mut s = session("s-live", false);
        s.last_seen_at = crate::time::epoch_ms_to_iso(now_epoch_ms() - 600_000);
        let before = s.last_seen_at.clone();
        store::put_session(&engine, &s).await.unwrap();

        bump_last_seen(&engine, "s-live").await;

        let after = store::get_session(&engine, "s-live")
            .await
            .unwrap()
            .unwrap();
        assert!(after.revoked_at.is_none());
        assert_ne!(after.last_seen_at, before, "last_seen should advance");
    }

    fn user() -> store::User {
        store::User {
            id: "u".into(),
            email: "u@example.com".into(),
            display_name: String::new(),
            role: "owner".into(),
            status: store::UserStatus::Active,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            last_seen_at: None,
        }
    }

    fn session_with(origin: SessionOrigin) -> AuthSession {
        AuthSession {
            session_id: "s".into(),
            user: user(),
            origin,
        }
    }

    #[test]
    fn a_passkey_browser_session_may_mint_credentials() {
        let sess = session_with(SessionOrigin::BrowserSession {
            idp: "passkey".into(),
        });
        assert!(sess.require_passkey_session().is_ok());
    }

    #[test]
    fn an_api_token_cannot_mint_credentials() {
        let sess = session_with(SessionOrigin::ApiToken {
            token_id: "hash".into(),
        });
        let err = sess.require_passkey_session().unwrap_err();
        assert!(matches!(err, ConsoleApiError::Forbidden(_)));
        assert!(
            err.to_string().contains(PASSKEY_SESSION_REQUIRED),
            "403 text must name the passkey-session rule, got {err}"
        );
    }

    #[test]
    fn a_non_passkey_browser_session_cannot_mint_credentials() {
        let sess = session_with(SessionOrigin::BrowserSession { idp: "oidc".into() });
        let err = sess.require_passkey_session().unwrap_err();
        assert!(matches!(err, ConsoleApiError::Forbidden(_)));
        let text = err.to_string();
        assert!(text.contains(PASSKEY_SESSION_REQUIRED), "{text}");
        assert!(text.contains("oidc"), "{text}");
    }
}
