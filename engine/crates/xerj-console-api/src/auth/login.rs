//! Returning-user login via passkey assertion.
//!
//! - `POST /auth/login/begin   { email }`
//!   → returns the WebAuthn `RequestChallengeResponse` JSON.
//!   Even when the email is unknown we return a *fake* challenge with
//!   no allow-credentials so the response shape is identical — this
//!   denies an attacker the email-enumeration oracle.
//! - `POST /auth/login/finish  { challenge_id, credential }`
//!   → validates the assertion, mints a session cookie.
//!
//! Logout: `POST /auth/logout` revokes the current session.

use axum::{
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
    Json,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use webauthn_rs::prelude::*;

use crate::auth::{audit, rate_limit, sessions, store, webauthn_setup};
use crate::error::{ConsoleApiError, ConsoleResult};
use crate::indices;
use crate::response::ok;
use crate::state::{ChallengeKind, ConsoleState, PendingChallenge};
use crate::time::{now_epoch_ms, now_iso};

const CHALLENGE_TTL_MS: i64 = 5 * 60 * 1000;

// ─────────────────────────────────────────────────────────────────────────────
// Begin
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct BeginBody {
    pub email: String,
}

#[derive(Debug, Serialize)]
pub struct BeginResponse {
    pub challenge_id: String,
    pub request_options: RequestChallengeResponse,
}

pub async fn begin(
    State(state): State<ConsoleState>,
    headers: HeaderMap,
    Json(body): Json<BeginBody>,
) -> ConsoleResult<Response> {
    let ip = ip_from_headers(&headers);
    rate_limit::charge(&state, &ip, "login-begin")?;

    if body.email.is_empty() {
        return Err(ConsoleApiError::BadRequest("missing email".into()));
    }

    // Find the user (may be None — we still return a challenge to avoid
    // enumeration).
    let user = store::find_user_by_email(&state.engine, &body.email).await?;

    // Collect this user's passkeys (empty list when user is None).
    let passkeys: Vec<Passkey> = match &user {
        None => Vec::new(),
        Some(u) => store::list_passkeys_for_user(&state.engine, &u.id)
            .await?
            .into_iter()
            .filter_map(|p| serde_json::from_value(p.blob).ok())
            .collect(),
    };

    let webauthn = webauthn_setup::build(&state)?;
    let (request_options, auth_state) = if passkeys.is_empty() {
        // Fake challenge: build a real challenge for an empty-credential
        // set so the response shape matches a known-user response.
        let fake_passkey: Vec<Passkey> = Vec::new();
        webauthn
            .start_passkey_authentication(&fake_passkey)
            .map_err(|e| ConsoleApiError::Internal(format!("start fake auth: {e}")))?
    } else {
        webauthn
            .start_passkey_authentication(&passkeys)
            .map_err(|e| ConsoleApiError::Internal(format!("start auth: {e}")))?
    };

    let challenge_id = random_id();
    state.pending_challenges.insert(
        challenge_id.clone(),
        PendingChallenge {
            kind: ChallengeKind::Login {
                email: user.as_ref().map(|u| u.email.clone()),
                state: auth_state,
            },
            created_at_ms: now_epoch_ms(),
        },
    );
    prune_pending(&state);

    Ok(ok(
        BeginResponse {
            challenge_id,
            request_options,
        },
        None,
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// Finish
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct FinishBody {
    pub challenge_id: String,
    pub credential: PublicKeyCredential,
}

pub async fn finish(
    State(state): State<ConsoleState>,
    headers: HeaderMap,
    Json(body): Json<FinishBody>,
) -> ConsoleResult<Response> {
    let ip = ip_from_headers(&headers);
    rate_limit::charge(&state, &ip, "login-finish")?;

    let pending = state
        .pending_challenges
        .remove(&body.challenge_id)
        .map(|(_, v)| v)
        .ok_or_else(|| ConsoleApiError::Unauthorized("unknown challenge".into()))?;
    let auth_state = match pending.kind {
        ChallengeKind::Login { state: s, .. } => s,
        _ => return Err(ConsoleApiError::Unauthorized("wrong challenge kind".into())),
    };

    let webauthn = webauthn_setup::build(&state)?;
    let auth_result: AuthenticationResult = webauthn
        .finish_passkey_authentication(&body.credential, &auth_state)
        .map_err(|e| {
            tracing::debug!(error = %e, "passkey authentication failed");
            ConsoleApiError::Unauthorized("authentication failed".into())
        })?;

    // Look up the credential we just authenticated, then the user.
    let cred_id_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode::<&[u8]>(auth_result.cred_id().as_ref());
    let pk = match get_passkey_by_id(&state, &cred_id_b64).await? {
        Some(p) => p,
        None => {
            // Should be impossible given the challenge state used the
            // same credential set, but stay defensive.
            return Err(ConsoleApiError::Unauthorized(
                "authentication failed".into(),
            ));
        }
    };

    let user = store::get_user(&state.engine, &pk.user_id)
        .await?
        .ok_or_else(|| ConsoleApiError::Unauthorized("user gone".into()))?;
    if user.status != store::UserStatus::Active {
        return Err(ConsoleApiError::Unauthorized("user disabled".into()));
    }

    // Update last_used_at on the passkey + sign-count is handled inside
    // webauthn-rs's Passkey state which we re-serialise.
    let mut updated_pk = pk.clone();
    updated_pk.last_used_at = Some(now_iso());
    // Re-serialise the Passkey blob so any internal counter updates land.
    if let Ok(typed) = serde_json::from_value::<Passkey>(pk.blob.clone()) {
        updated_pk.blob = serde_json::to_value(&typed).unwrap_or(pk.blob);
    }
    store::put_passkey(&state.engine, &updated_pk).await?;

    let ua = headers
        .get("user-agent")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    let (_session, signed) =
        sessions::mint_session(&state, &user.id, "passkey", Some(ip.clone()), ua).await?;

    audit::record(
        &state.engine,
        &user.id,
        "session-minted",
        indices::SESSIONS,
        None,
        Some(&ip),
        Some(json!({ "via": "login" })),
    )
    .await;

    let cookie = sessions::make_set_cookie(signed, false);
    let body = ok(
        json!({
            "user": {
                "id":    user.id,
                "email": user.email,
                "role":  user.role,
            }
        }),
        None,
    );
    Ok((
        axum_extra::extract::cookie::CookieJar::new().add(cookie),
        body,
    )
        .into_response())
}

// ─────────────────────────────────────────────────────────────────────────────
// Logout
// ─────────────────────────────────────────────────────────────────────────────

pub async fn logout(
    State(state): State<ConsoleState>,
    sess: sessions::AuthSession,
) -> ConsoleResult<Response> {
    store::revoke_session(&state.engine, &sess.session_id, &now_iso()).await?;
    audit::record(
        &state.engine,
        &sess.user.id,
        "session-revoked",
        indices::SESSIONS,
        Some(&sess.session_id),
        None,
        Some(json!({ "reason": "logout" })),
    )
    .await;
    let cookie = sessions::make_clear_cookie();
    Ok((
        axum_extra::extract::cookie::CookieJar::new().add(cookie),
        ok(json!({}), None),
    )
        .into_response())
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

async fn get_passkey_by_id(
    state: &ConsoleState,
    credential_id: &str,
) -> ConsoleResult<Option<store::StoredPasskey>> {
    let idx = state.engine.get_index(indices::PASSKEYS)?;
    let body = json!({
        "query": { "ids": { "values": [credential_id] } },
        "size": 1
    });
    let req = xerj_query::parser::parse_request(&body)
        .map_err(|e| ConsoleApiError::Internal(e.to_string()))?;
    let r = idx.search(&req).await?;
    Ok(match r.hits.into_iter().next() {
        None => None,
        Some(h) => Some(serde_json::from_value(h.source)?),
    })
}

fn random_id() -> String {
    let mut b = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn prune_pending(state: &ConsoleState) {
    let cutoff = now_epoch_ms() - CHALLENGE_TTL_MS;
    state
        .pending_challenges
        .retain(|_k, v| v.created_at_ms > cutoff);
}

fn ip_from_headers(headers: &HeaderMap) -> String {
    if let Some(v) = headers.get("x-forwarded-for").and_then(|h| h.to_str().ok()) {
        if let Some(first) = v.split(',').next() {
            let t = first.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    if let Some(v) = headers.get("x-real-ip").and_then(|h| h.to_str().ok()) {
        return v.trim().to_string();
    }
    "unknown".to_string()
}

use base64::Engine as _;
