//! Magic-link issue + redemption.
//!
//! `POST /auth/magic/issue   { email, role }`
//!
//! An owner or admin mints a single-use invite link for `email`. If no
//! user with that email exists yet a `pending` row is provisioned so the
//! invitee flips to `active` once they enrol a passkey. The raw token is
//! returned exactly once; only its `sha256` is persisted.
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

use crate::auth::{audit, rate_limit, store, AuthSession};
use crate::bootstrap::sha256_hex;
use crate::error::{ConsoleApiError, ConsoleResult};
use crate::indices;
use crate::response::ok;
use crate::state::{ConsoleState, EnrollmentSession};
use crate::time::{now_epoch_ms, now_iso, parse_iso};

const ENROLL_TTL_MS: i64 = 30 * 60 * 1000;
/// How long an admin-issued invite link stays valid before it must be
/// re-minted. Invites travel out-of-band (email/chat) so they get a much
/// longer window than the in-session enrollment handoff.
const INVITE_TTL_MS: i64 = 72 * 60 * 60 * 1000; // 72 hours
/// Roles an operator is allowed to grant when issuing an invite. Anything
/// outside this set is rejected rather than written verbatim into a user
/// row, so a typo can't mint an unrecognised (and therefore unchecked)
/// privilege string.
const INVITE_ROLES: &[&str] = &["owner", "admin", "editor", "viewer"];

// ─────────────────────────────────────────────────────────────────────────────
// Issue (admin-only invite minting)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct IssueBody {
    pub email: String,
    pub role: String,
}

#[derive(Debug, Serialize)]
pub struct IssueResponse {
    /// The raw token — returned exactly once. Only its `sha256` is stored,
    /// so this is the operator's single chance to hand it to the invitee.
    pub token: String,
    /// Host-relative setup link the operator can forward; the origin is
    /// supplied by whatever server rendered the console.
    pub link: String,
    pub user_id: String,
    pub email: String,
    pub role: String,
    pub purpose: String,
    pub expires_at: String,
}

/// `POST /auth/magic/issue { email, role }` — an owner or admin mints a
/// single-use invite link for `email`. When no user with that email exists
/// we provision a `pending` row so the invitee flips to `active` the moment
/// they redeem the link and enrol a passkey (`redeem` → `passkey/finish`).
pub async fn issue(
    State(state): State<ConsoleState>,
    session: AuthSession,
    headers: HeaderMap,
    Json(body): Json<IssueBody>,
) -> ConsoleResult<Response> {
    // Only owners and admins may invite.
    match session.user.role.as_str() {
        "owner" | "admin" => {}
        _ => {
            return Err(ConsoleApiError::Forbidden(
                "only an owner or admin may issue invites".into(),
            ));
        }
    }

    let email = body.email.trim().to_ascii_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err(ConsoleApiError::BadRequest(
            "a valid email is required".into(),
        ));
    }
    let role = body.role.trim();
    if !INVITE_ROLES.contains(&role) {
        return Err(ConsoleApiError::BadRequest(format!("unknown role: {role}")));
    }
    // Privilege ceiling: only an owner may grant the owner role, so an admin
    // can't quietly mint a peer with more authority than themselves.
    if role == "owner" && session.user.role != "owner" {
        return Err(ConsoleApiError::Forbidden(
            "only an owner may invite another owner".into(),
        ));
    }

    // Provision or look up the invitee. Re-inviting an existing address reuses
    // that row (and its current role) rather than duplicating the user or
    // silently escalating them via the invite.
    let now = now_iso();
    let user = match store::find_user_by_email(&state.engine, &email).await? {
        Some(u) => u,
        None => {
            let u = store::User {
                id: uuid::Uuid::new_v4().to_string(),
                email: email.clone(),
                display_name: String::new(),
                role: role.to_string(),
                status: store::UserStatus::Pending,
                created_at: now.clone(),
                last_seen_at: None,
            };
            store::upsert_user(&state.engine, &u).await?;
            u
        }
    };
    let user_id = user.id;

    // Mint a random 32-byte URL-safe token; persist only its sha256.
    let mut token_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut token_bytes);
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token_bytes);
    let token_hash = sha256_hex(token.as_bytes());

    let now_ms = now_epoch_ms();
    let expires_at = crate::time::epoch_ms_to_iso(now_ms + INVITE_TTL_MS);

    let link = store::MagicLink {
        id: token_hash.clone(),
        purpose: "invite".to_string(),
        user_id: Some(user_id.clone()),
        email: Some(email.clone()),
        role: role.to_string(),
        created_by: session.user.id.clone(),
        created_at: now,
        expires_at: expires_at.clone(),
        used_at: None,
    };
    store::put_magic_link(&state.engine, &link).await?;

    let ip = ip_from_headers(&headers);
    audit::record(
        &state.engine,
        &session.user.id,
        "magic-issued",
        indices::MAGIC_LINKS,
        Some(&token_hash),
        Some(&ip),
        Some(json!({ "purpose": "invite", "invitee": user_id.clone(), "role": role })),
    )
    .await;

    let setup_link = format!("/_xerj-console/setup#token={token}");
    Ok(ok(
        IssueResponse {
            token,
            link: setup_link,
            user_id,
            email,
            role: role.to_string(),
            purpose: "invite".to_string(),
            expires_at,
        },
        None,
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// Redeem
// ─────────────────────────────────────────────────────────────────────────────

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
                // Same shape as invite: the account was provisioned when the
                // link was minted. Harden the redemption identically — refuse
                // if the target row vanished between mint and redeem so a
                // leaked recovery link can never resurrect a deleted account
                // (and thereby hand an attacker a fresh enrollment session for
                // a user the admin already removed).
                let uid = link.user_id.clone().ok_or_else(|| {
                    ConsoleApiError::Internal("recovery link without user_id".into())
                })?;
                if store::get_user(&state.engine, &uid).await?.is_none() {
                    audit_redeem_failed(&state, "recovery-user-gone", &ip);
                    return Err(ConsoleApiError::Unauthorized(
                        "invalid or expired link".into(),
                    ));
                }
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
