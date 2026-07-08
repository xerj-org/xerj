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

use axum::{
    extract::FromRequestParts,
    http::{request::Parts, HeaderValue, StatusCode},
};
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

/// Authenticated session, extracted from the `xerj_session` cookie.
///
/// Use as a handler parameter to require authentication. Returns 401 on
/// missing cookie, bad signature, expired session, or revoked session.
#[derive(Debug, Clone)]
pub struct AuthSession {
    pub session_id: String,
    pub user: store::User,
}

#[axum::async_trait]
impl FromRequestParts<ConsoleState> for AuthSession {
    type Rejection = ConsoleApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ConsoleState,
    ) -> Result<Self, Self::Rejection> {
        // API clients authenticate with `Authorization: Bearer <token>`;
        // browsers with a signed session cookie. Bearer wins when present so
        // a token-carrying request is never silently downgraded to the
        // (absent) cookie path.
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

        // Bump last_seen so the session stays fresh while the user is
        // active. Best-effort — we don't fail the request if the write
        // races with another tab.
        let mut bumped = session.clone();
        bumped.last_seen_at = now_iso();
        let _ = store::put_session(&state.engine, &bumped).await;

        Ok(AuthSession { session_id, user })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Bearer (API token) authentication
// ─────────────────────────────────────────────────────────────────────────────

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
    // `session_id` is opaque to callers; for a token-authenticated request
    // it is the token id (= hash). The cookie-only `logout` handler would
    // just no-op against `.xerj_sessions` for such an id.
    Ok(AuthSession {
        session_id: token.id,
        user,
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

#[allow(dead_code)]
fn _silence_unused_imports(_v: HeaderValue, _s: StatusCode) {}

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
}
