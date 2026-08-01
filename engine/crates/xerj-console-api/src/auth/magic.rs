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

use axum::{extract::State, response::Response, Json};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{audit, rate_limit, store, AuthSession};
use crate::bootstrap::sha256_hex;
use crate::client_ip::ClientIp;
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
    ClientIp(ip): ClientIp,
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
    ClientIp(ip): ClientIp,
    Json(body): Json<RedeemBody>,
) -> ConsoleResult<Response> {
    // Rate-limit by source IP. `ClientIp` resolves that from the TCP peer,
    // consulting `x-forwarded-for` only when the peer is a configured
    // trusted proxy (#76 S5-4) — this endpoint is unauthenticated, so a
    // header-derived key would let anyone mint a fresh quota per request.
    rate_limit::charge(&state, &ip, "magic-redeem")?;

    if body.token.is_empty() {
        return Err(ConsoleApiError::BadRequest("missing token".into()));
    }
    let token_hash = sha256_hex(body.token.as_bytes());

    // #76 S5-3: serialize the single-use check and the mark-used commit. The
    // used-check (below) and `mark_magic_link_used` (get→delete→create) are
    // separated by several awaits with no exclusive lock, so two concurrent
    // redeems of one token could both pass the check and both mint a session.
    // Hold the gate for the rest of this function so check→consume is atomic.
    // `lock_owned` avoids tying the guard's lifetime to `state` across awaits.
    let _redeem_guard = state.redeem_gate.clone().lock_owned().await;

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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::TempDir;
    use tokio::sync::Barrier;

    use super::*;
    use crate::state::ClusterMode;

    /// Console state with the `.xerj_*` system indices in place. We skip
    /// `bootstrap::run` — redeem only touches magic links, users and audit,
    /// and dashboard seeding would dominate the runtime of a test that races
    /// hundreds of redemptions.
    fn test_state() -> (ConsoleState, TempDir) {
        let dir = TempDir::new().unwrap();
        let mut cfg = xerj_common::config::Config::default();
        cfg.server.data_dir = dir.path().to_str().unwrap().to_string();
        let engine = xerj_engine::Engine::new(cfg).expect("engine");
        indices::ensure_all(&engine).expect("system indices");
        let state = ConsoleState::new(engine, "local".into(), [0u8; 32], ClusterMode::Standalone);
        (state, dir)
    }

    /// Write an invite link for `token` plus the pending invitee row it
    /// points at — the same pair `issue` persists. `ttl_ms` may be negative
    /// to mint an already-expired link.
    async fn mint_invite(state: &ConsoleState, token: &str, ttl_ms: i64) {
        let user_id = format!("invitee-{token}");
        let user = store::User {
            id: user_id.clone(),
            email: format!("{token}@example.com"),
            display_name: String::new(),
            role: "editor".to_string(),
            status: store::UserStatus::Pending,
            created_at: now_iso(),
            last_seen_at: None,
        };
        store::upsert_user(&state.engine, &user).await.unwrap();

        let link = store::MagicLink {
            id: sha256_hex(token.as_bytes()),
            purpose: "invite".to_string(),
            user_id: Some(user_id),
            email: Some(user.email),
            role: "editor".to_string(),
            created_by: "admin-test".to_string(),
            created_at: now_iso(),
            expires_at: crate::time::epoch_ms_to_iso(now_epoch_ms() + ttl_ms),
            used_at: None,
        };
        store::put_magic_link(&state.engine, &link).await.unwrap();
    }

    /// Drive the handler exactly as axum would. Every call declares its own
    /// resolved source IP so the per-IP limiter (10/min) never fires — a 429
    /// would silently stand in for a rejected redemption and hide the
    /// invariant under test. Note this is the *resolved* address (post trust
    /// check), not a header a caller could set: see `client_ip`.
    async fn redeem_as(state: &ConsoleState, token: &str, ip: &str) -> ConsoleResult<Response> {
        redeem(
            State(state.clone()),
            ClientIp(ip.to_string()),
            Json(RedeemBody {
                token: token.to_string(),
            }),
        )
        .await
    }

    async fn used_at_of(state: &ConsoleState, token: &str) -> Option<String> {
        store::get_magic_link(&state.engine, &sha256_hex(token.as_bytes()))
            .await
            .unwrap()
            .expect("link row must survive redemption")
            .used_at
    }

    /// #76 S5-3: the used-check and the `mark_magic_link_used` commit are
    /// separated by several awaits, so without the redeem gate every racer
    /// passes the used-check on one token and they all go on to consume it —
    /// the single-use rule then rests on whatever the store does with the
    /// colliding writes, not on this handler. Race a fresh token per round,
    /// many rounds, and pin both halves of the guarantee: one winner, and
    /// losers rejected as *used* (the generic 401) rather than surfacing a
    /// write collision from underneath.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn racing_redemptions_of_one_token_mint_exactly_one_session() {
        const ROUNDS: usize = 25;
        const RACERS: usize = 6;

        let (state, _dir) = test_state();

        for round in 0..ROUNDS {
            let token = format!("race-token-{round}");
            mint_invite(&state, &token, INVITE_TTL_MS).await;

            let start = Arc::new(Barrier::new(RACERS));
            let mut racers = Vec::with_capacity(RACERS);
            for racer in 0..RACERS {
                let state = state.clone();
                let token = token.clone();
                let start = start.clone();
                racers.push(tokio::spawn(async move {
                    start.wait().await;
                    redeem_as(&state, &token, &format!("10.0.{round}.{racer}")).await
                }));
            }

            let mut winners = 0usize;
            let mut losses = Vec::new();
            for racer in racers {
                match racer.await.expect("redeem must not panic") {
                    Ok(_) => winners += 1,
                    Err(e) => losses.push(e),
                }
            }

            assert_eq!(
                winners, 1,
                "round {round}: exactly one of {RACERS} concurrent redemptions may mint a session"
            );
            for loss in &losses {
                assert!(
                    matches!(loss, ConsoleApiError::Unauthorized(_)),
                    "round {round}: a losing racer must get the generic 401, got {loss}"
                );
            }
            assert_eq!(
                state.enrollment_sessions.len(),
                round + 1,
                "round {round}: one enrollment session per token, never two"
            );
            assert!(
                used_at_of(&state, &token).await.is_some(),
                "round {round}: the winning redemption must leave the link consumed"
            );
        }
    }

    /// The gate serializes redemptions but must not reject them: two invitees
    /// redeeming their own links at the same instant both get a session. This
    /// is also the control for the test above — it proves the racing harness
    /// can produce more than one winner, so `winners == 1` there is the
    /// single-use rule holding, not the harness serializing the calls.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn racing_redemptions_of_distinct_tokens_all_succeed() {
        const RACERS: usize = 6;

        let (state, _dir) = test_state();
        for racer in 0..RACERS {
            mint_invite(&state, &format!("distinct-token-{racer}"), INVITE_TTL_MS).await;
        }

        let start = Arc::new(Barrier::new(RACERS));
        let mut racers = Vec::with_capacity(RACERS);
        for racer in 0..RACERS {
            let state = state.clone();
            let start = start.clone();
            racers.push(tokio::spawn(async move {
                start.wait().await;
                redeem_as(
                    &state,
                    &format!("distinct-token-{racer}"),
                    &format!("10.1.0.{racer}"),
                )
                .await
            }));
        }
        for racer in racers {
            let outcome = racer.await.expect("redeem must not panic");
            assert!(
                outcome.is_ok(),
                "a distinct token must still redeem under contention: {:?}",
                outcome.err()
            );
        }
        assert_eq!(state.enrollment_sessions.len(), RACERS);
    }

    /// An invite past its `expires_at` is refused, and refusing it neither
    /// mints a session nor consumes the link.
    #[tokio::test]
    async fn an_expired_token_is_refused_and_never_mints_a_session() {
        let (state, _dir) = test_state();
        mint_invite(&state, "stale-token", -60_000).await;

        let outcome = redeem_as(&state, "stale-token", "10.2.0.1").await;
        assert!(
            matches!(outcome, Err(ConsoleApiError::Unauthorized(_))),
            "an expired link must be refused with the generic 401"
        );
        assert!(state.enrollment_sessions.is_empty());
        assert!(
            used_at_of(&state, "stale-token").await.is_none(),
            "a refused redemption must not consume the link"
        );
    }

    /// Replay of a consumed token is refused and leaves the original
    /// consumption record intact — a second attempt must not re-stamp
    /// `used_at` and so blur who redeemed the link and when.
    #[tokio::test]
    async fn an_already_redeemed_token_cannot_be_replayed() {
        let (state, _dir) = test_state();
        mint_invite(&state, "replay-token", INVITE_TTL_MS).await;

        redeem_as(&state, "replay-token", "10.3.0.1")
            .await
            .expect("first redemption");
        let consumed_at = used_at_of(&state, "replay-token").await;
        assert!(consumed_at.is_some());

        let outcome = redeem_as(&state, "replay-token", "10.3.0.2").await;
        assert!(
            matches!(outcome, Err(ConsoleApiError::Unauthorized(_))),
            "a redeemed link must be refused with the generic 401"
        );
        assert_eq!(
            state.enrollment_sessions.len(),
            1,
            "the replay must not mint a second enrollment session"
        );
        assert_eq!(
            used_at_of(&state, "replay-token").await,
            consumed_at,
            "the replay must not re-stamp used_at"
        );
    }
}
