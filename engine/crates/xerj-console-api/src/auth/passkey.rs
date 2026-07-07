//! Passkey (WebAuthn) enrollment.
//!
//! Two endpoints:
//!
//! - `POST /auth/passkey/begin   { enrollment_session_id, email?, display_name? }`
//!   → returns the WebAuthn `CreationChallengeResponse` JSON the SPA
//!   hands to `navigator.credentials.create({ publicKey })`.
//! - `POST /auth/passkey/finish  { enrollment_session_id, name, credential }`
//!   → validates the attestation, persists the credential, marks the
//!   user `active`, and mints a session cookie.
//!
//! The enrollment session itself lives only in
//! `ConsoleState.enrollment_sessions` — it is consumed once at finish-time
//! and never re-readable.

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

use crate::auth::{audit, sessions, store, webauthn_setup};
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
    pub enrollment_session_id: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BeginResponse {
    pub challenge_id: String,
    pub creation_options: CreationChallengeResponse,
}

pub async fn begin(
    State(state): State<ConsoleState>,
    Json(body): Json<BeginBody>,
) -> ConsoleResult<Response> {
    // Look up the enrollment session.
    let enroll = state
        .enrollment_sessions
        .get(&body.enrollment_session_id)
        .map(|e| e.clone())
        .ok_or_else(|| ConsoleApiError::Unauthorized("unknown enrollment session".into()))?;

    if now_epoch_ms() > enroll.expires_at_ms {
        state
            .enrollment_sessions
            .remove(&body.enrollment_session_id);
        return Err(ConsoleApiError::Unauthorized(
            "enrollment session expired".into(),
        ));
    }

    // Email: prefer the body (operator may be entering it for the first
    // time on a bootstrap claim) and fall back to whatever was on the
    // magic link.
    let email = body
        .email
        .clone()
        .or_else(|| Some(enroll.email.clone()).filter(|s| !s.is_empty()))
        .unwrap_or_default();
    if email.is_empty() {
        return Err(ConsoleApiError::BadRequest(
            "email is required to enrol the first credential".into(),
        ));
    }

    // Email-uniqueness check: a different user with this email already
    // exists. Skip when this is the same user we're enrolling for
    // (re-running begin is fine).
    if let Some(existing) = store::find_user_by_email(&state.engine, &email).await? {
        if existing.id != enroll.user_id {
            return Err(ConsoleApiError::Conflict(format!(
                "email {email} is already registered"
            )));
        }
    }

    // Existing credentials to exclude (avoids a single physical key
    // being enrolled twice for the same user).
    let exclude: Vec<CredentialID> = store::list_passkeys_for_user(&state.engine, &enroll.user_id)
        .await?
        .into_iter()
        .filter_map(|p| {
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&p.id)
                .ok()?;
            Some(CredentialID::from(bytes))
        })
        .collect();

    let webauthn = webauthn_setup::build(&state)?;
    let user_unique_id = uuid::Uuid::parse_str(&enroll.user_id).unwrap_or_else(|_| Uuid::new_v4()); // synthetic placeholders are uuids; legacy ids fall back.

    let display_name = body.display_name.clone().unwrap_or_else(|| email.clone());
    let (creation_options, reg_state) = webauthn
        .start_passkey_registration(
            user_unique_id,
            &email,
            &display_name,
            if exclude.is_empty() {
                None
            } else {
                Some(exclude)
            },
        )
        .map_err(|e| ConsoleApiError::Internal(format!("start registration: {e}")))?;

    // Stash the registration state so /finish can verify the response.
    let challenge_id = random_id();
    state.pending_challenges.insert(
        challenge_id.clone(),
        PendingChallenge {
            kind: ChallengeKind::PasskeyEnroll {
                user_id: enroll.user_id.clone(),
                state: reg_state,
            },
            created_at_ms: now_epoch_ms(),
        },
    );
    prune_pending(&state);

    Ok(ok(
        BeginResponse {
            challenge_id,
            creation_options,
        },
        None,
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// Finish
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct FinishBody {
    pub enrollment_session_id: String,
    pub challenge_id: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub credential: RegisterPublicKeyCredential,
}

pub async fn finish(
    State(state): State<ConsoleState>,
    headers: HeaderMap,
    Json(body): Json<FinishBody>,
) -> ConsoleResult<Response> {
    // Pull and consume both the enrollment session and the challenge —
    // either error fails the request without a partial commit.
    let enroll = state
        .enrollment_sessions
        .remove(&body.enrollment_session_id)
        .map(|(_, v)| v)
        .ok_or_else(|| ConsoleApiError::Unauthorized("unknown enrollment session".into()))?;
    if now_epoch_ms() > enroll.expires_at_ms {
        return Err(ConsoleApiError::Unauthorized(
            "enrollment session expired".into(),
        ));
    }

    let pending = state
        .pending_challenges
        .remove(&body.challenge_id)
        .map(|(_, v)| v)
        .ok_or_else(|| ConsoleApiError::Unauthorized("unknown challenge".into()))?;
    let reg_state = match pending.kind {
        ChallengeKind::PasskeyEnroll { user_id, state } if user_id == enroll.user_id => state,
        _ => {
            return Err(ConsoleApiError::Unauthorized(
                "challenge does not belong to this enrollment".into(),
            ))
        }
    };

    let webauthn = webauthn_setup::build(&state)?;
    let passkey: Passkey = webauthn
        .finish_passkey_registration(&body.credential, &reg_state)
        .map_err(|e| ConsoleApiError::Unauthorized(format!("attestation rejected: {e}")))?;

    // Persist the credential.
    let cred_id_bytes: &[u8] = passkey.cred_id().as_ref();
    let credential_id_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(cred_id_bytes);
    let pk_blob = serde_json::to_value(&passkey)?;
    let pk = store::StoredPasskey {
        id: credential_id_b64.clone(),
        user_id: enroll.user_id.clone(),
        name: body.name.unwrap_or_else(|| "default".into()),
        created_at: now_iso(),
        last_used_at: None,
        blob: pk_blob,
    };
    store::put_passkey(&state.engine, &pk).await?;

    // Upsert the user — flip pending → active. Bootstrap claims also
    // land here (placeholder user gets its first concrete data here).
    let now = now_iso();
    let email = body
        .email
        .clone()
        .or_else(|| Some(enroll.email.clone()).filter(|s| !s.is_empty()))
        .unwrap_or_default();
    let display_name = body.display_name.clone().unwrap_or_else(|| email.clone());

    let existing = store::get_user(&state.engine, &enroll.user_id).await?;
    let user = match existing {
        Some(mut u) => {
            u.email = email;
            u.display_name = display_name;
            u.status = store::UserStatus::Active;
            u.last_seen_at = Some(now.clone());
            u
        }
        None => store::User {
            id: enroll.user_id.clone(),
            email,
            display_name,
            role: enroll.role.clone(),
            status: store::UserStatus::Active,
            created_at: now.clone(),
            last_seen_at: Some(now.clone()),
        },
    };
    store::upsert_user(&state.engine, &user).await?;

    // Audit + session cookie.
    let ip = sessions_request_ip(&headers);
    let ua = headers
        .get("user-agent")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    audit::record(
        &state.engine,
        &user.id,
        "passkey-enrolled",
        indices::PASSKEYS,
        Some(&credential_id_b64),
        Some(&ip),
        None,
    )
    .await;

    let (_session, signed) =
        sessions::mint_session(&state, &user.id, "passkey", Some(ip.clone()), ua).await?;

    audit::record(
        &state.engine,
        &user.id,
        "session-minted",
        indices::SESSIONS,
        None,
        Some(&ip),
        Some(json!({ "via": "passkey-enroll" })),
    )
    .await;

    let cookie = sessions::make_set_cookie(signed, false);
    let body = ok(
        json!({
            "user": {
                "id":   user.id,
                "email": user.email,
                "role":  user.role,
            },
            "passkey": {
                "id":   pk.id,
                "name": pk.name,
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
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

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

fn sessions_request_ip(headers: &HeaderMap) -> String {
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
