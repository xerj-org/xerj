//! Magic-link redemption.
//!
//! `POST /auth/magic/redeem  { token }`
//!
//! Looks up `sha256(token)` in `.xerj_magic_links`, validates expiry
//! and single-use, marks `used_at = now`, and returns an enrollment
//! session id the SPA echoes back on `POST /auth/passkey/begin`.
//!
//! Enrollment session lives only in RAM (not persisted) for 30 minutes
//! and is consumed exactly once by `passkey/finish`.

use axum::{extract::State, http::HeaderMap, response::Response, Json};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{audit, rate_limit, store};
use crate::bootstrap::sha256_hex;
use crate::error::{ConsoleApiError, ConsoleResult};
use crate::indices;
use crate::response::ok;
use crate::state::{ConsoleState, EnrollmentSession};
use crate::time::{now_epoch_ms, now_iso, parse_iso};

const ENROLL_TTL_MS: i64 = 30 * 60 * 1000;

#[derive(Debug, Deserialize)]
pub struct RedeemBody {
    pub token: String,
}

#[derive(Debug, Serialize)]
pub struct RedeemResponse {
    pub enrollment_session_id: String,
    pub email: Option<String>,
    pub role: String,
    pub expires_at: String,
}

pub async fn redeem(
    State(state): State<ConsoleState>,
    headers: HeaderMap,
    Json(body): Json<RedeemBody>,
) -> ConsoleResult<Response> {
    // Rate-limit by source IP. We pull headers manually here because
    // FromRequestParts wiring would force every endpoint into the same
    // typed extractor.
    let ip = ip_from_headers(&headers);
    rate_limit::charge(&state, &ip, "magic-redeem")?;

    if body.token.is_empty() {
        return Err(ConsoleApiError::BadRequest("missing token".into()));
    }
    let token_hash = sha256_hex(body.token.as_bytes());

    // Look it up.
    let link = store::get_magic_link(&state.engine, &token_hash)
        .await?
        .ok_or_else(|| {
            // Single error message regardless of "doesn't exist" vs
            // "expired" vs "used" — never leak which one.
            audit_redeem_failed(&state, "not-found", &ip);
            ConsoleApiError::Unauthorized("invalid or expired link".into())
        })?;

    // Single-use.
    if link.used_at.is_some() {
        audit_redeem_failed(&state, "already-used", &ip);
        return Err(ConsoleApiError::Unauthorized(
            "invalid or expired link".into(),
        ));
    }
    // Expiry.
    if let Some(exp) = parse_iso(&link.expires_at) {
        if now_epoch_ms() > exp.timestamp_millis() {
            audit_redeem_failed(&state, "expired", &ip);
            return Err(ConsoleApiError::Unauthorized(
                "invalid or expired link".into(),
            ));
        }
    }

    // Resolve target user. Bootstrap links have no user_id yet — we
    // synthesise one. Invite links carry the user_id we already
    // provisioned in pending state.
    let (user_id, email) =
        match link.purpose.as_str() {
            "bootstrap" => {
                // Make sure no active user has snuck in between mint and
                // redeem (race window when two operators open the same
                // banner).
                let active = store::count_active_users(&state.engine).await?;
                if active > 0 {
                    audit_redeem_failed(&state, "bootstrap-already-claimed", &ip);
                    return Err(ConsoleApiError::Conflict(
                        "this server has already been claimed; ask your admin for an invite".into(),
                    ));
                }
                // Provision a placeholder user. The SPA fills in the email
                // and display name during the passkey enrollment flow.
                let synthetic_id = uuid::Uuid::new_v4().to_string();
                (synthetic_id, link.email.clone())
            }
            "invite" => {
                let uid = link.user_id.clone().ok_or_else(|| {
                    ConsoleApiError::Internal("invite link without user_id".into())
                })?;
                // Make sure the invitee row still exists (admin may have
                // deleted them between mint and redeem).
                let user = store::get_user(&state.engine, &uid).await?;
                if user.is_none() {
                    audit_redeem_failed(&state, "invitee-gone", &ip);
                    return Err(ConsoleApiError::Unauthorized(
                        "invalid or expired link".into(),
                    ));
                }
                (uid, link.email.clone())
            }
            "recovery" => {
                // Reserved for v1.1 — same shape as invite.
                let uid = link.user_id.clone().ok_or_else(|| {
                    ConsoleApiError::Internal("recovery link without user_id".into())
                })?;
                (uid, link.email.clone())
            }
            other => {
                return Err(ConsoleApiError::Internal(format!(
                    "unknown magic-link purpose: {other}"
                )));
            }
        };

    // Mark used.
    store::mark_magic_link_used(&state.engine, &token_hash, &now_iso()).await?;

    // Mint enrollment session.
    let mut id_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut id_bytes);
    let session_id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(id_bytes);
    let now_ms = now_epoch_ms();
    let expires_ms = now_ms + ENROLL_TTL_MS;

    let enroll = EnrollmentSession {
        session_id: session_id.clone(),
        email: email.clone().unwrap_or_default(),
        user_id: user_id.clone(),
        role: link.role.clone(),
        created_at_ms: now_ms,
        expires_at_ms: expires_ms,
    };
    state.enrollment_sessions.insert(session_id.clone(), enroll);

    audit::record(
        &state.engine,
        "system",
        "magic-redeemed",
        indices::MAGIC_LINKS,
        Some(&token_hash),
        Some(&ip),
        Some(json!({ "purpose": link.purpose, "user_id": user_id })),
    )
    .await;

    Ok(ok(
        RedeemResponse {
            enrollment_session_id: session_id,
            email,
            role: link.role,
            expires_at: crate::time::epoch_ms_to_iso(expires_ms),
        },
        None,
    ))
}

fn audit_redeem_failed(state: &ConsoleState, why: &str, ip: &str) {
    let engine = state.engine.clone();
    let why = why.to_string();
    let ip = ip.to_string();
    tokio::spawn(async move {
        audit::record(
            &engine,
            "system",
            "magic-redeem-failed",
            indices::MAGIC_LINKS,
            None,
            Some(&ip),
            Some(json!({ "reason": why })),
        )
        .await;
    });
}

use base64::Engine as _;

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
