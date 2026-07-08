//! API tokens — issue / list / revoke.
//!
//! `POST /auth/api-tokens { name, scopes? }` mints a random 32-byte
//! URL-safe secret, persists only `sha256(secret)` (as the doc `_id`) plus
//! metadata, and returns the plaintext **once**. The secret is never logged
//! and cannot be recovered afterward — the same store convention as magic
//! links (`bootstrap.rs`).
//!
//! `GET /auth/api-tokens` lists the caller's tokens as metadata only
//! (id = the hash, name, scopes, timestamps); it never returns a plaintext.
//!
//! `DELETE /auth/api-tokens/:id` revokes one of the caller's tokens. It is
//! idempotent and returns 404 when the id is not the caller's / does not
//! exist, so we never confirm the existence of another user's token id.
//!
//! Bearer authentication lives in `sessions.rs`: it hashes a presented
//! `Authorization: Bearer <secret>` and looks up the non-revoked row minted
//! here, then authenticates as the token's owner.

use axum::{extract::State, response::Response, Json};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{audit, sessions::AuthSession, store};
use crate::bootstrap::sha256_hex;
use crate::error::{ConsoleApiError, ConsoleResult};
use crate::indices;
use crate::response::{no_content, ok};
use crate::state::ConsoleState;
use crate::time::now_iso;

/// Secret length in bytes. 32 bytes of CSPRNG output → 43-char URL-safe
/// base64 (no padding), matching the magic-link token shape.
const TOKEN_BYTES: usize = 32;

// ─────────────────────────────────────────────────────────────────────────────
// Issue
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateBody {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub scopes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct CreatedToken {
    /// The plaintext secret — returned exactly once, never persisted or
    /// logged. Treat like a password.
    pub token: String,
    /// The token's stable identifier (= `sha256(secret)` hex). Safe to
    /// show/store; this is what `DELETE /auth/api-tokens/:id` takes.
    pub id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub created_at: String,
}

pub async fn create(
    State(state): State<ConsoleState>,
    sess: AuthSession,
    Json(body): Json<CreateBody>,
) -> ConsoleResult<Response> {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(ConsoleApiError::BadRequest("missing token name".into()));
    }

    // Random secret. URL-safe base64, no padding — same shape as magic
    // links so it survives a copy-paste into an `Authorization` header.
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::thread_rng().fill_bytes(&mut bytes);
    let secret = URL_SAFE_NO_PAD.encode(bytes);
    let hash = sha256_hex(secret.as_bytes());

    let now = now_iso();
    let token = store::ApiToken {
        id: hash.clone(),
        user_id: sess.user.id.clone(),
        name: name.clone(),
        scopes: body.scopes.clone(),
        created_at: now.clone(),
        last_used_at: None,
        revoked_at: None,
    };
    store::put_api_token(&state.engine, &token).await?;

    audit::record(
        &state.engine,
        &sess.user.id,
        "api-token-issued",
        indices::API_TOKENS,
        Some(&hash),
        None,
        Some(json!({ "name": name.clone() })),
    )
    .await;

    Ok(ok(
        CreatedToken {
            token: secret,
            id: hash,
            name,
            scopes: token.scopes,
            created_at: now,
        },
        None,
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// List
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct TokenListItem {
    /// Stable id (= `sha256(secret)` hex). Never the plaintext secret.
    pub id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

pub async fn list(State(state): State<ConsoleState>, sess: AuthSession) -> ConsoleResult<Response> {
    let tokens = store::list_api_tokens_for_user(&state.engine, &sess.user.id).await?;
    let items: Vec<TokenListItem> = tokens
        .into_iter()
        .map(|t| TokenListItem {
            id: t.id,
            name: t.name,
            scopes: t.scopes,
            created_at: t.created_at,
            last_used_at: t.last_used_at,
            revoked_at: t.revoked_at,
        })
        .collect();
    Ok(ok(json!({ "tokens": items }), None))
}

// ─────────────────────────────────────────────────────────────────────────────
// Revoke
// ─────────────────────────────────────────────────────────────────────────────

pub async fn revoke(
    State(state): State<ConsoleState>,
    sess: AuthSession,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ConsoleResult<Response> {
    // Look the token up by id (= hash). A token that does not exist, or one
    // owned by a different user, both return 404 — we never confirm the
    // existence of another user's token id.
    let mut token = match store::get_api_token(&state.engine, &id).await? {
        Some(t) if t.user_id == sess.user.id => t,
        _ => return Err(ConsoleApiError::NotFound("api token".into())),
    };

    // Idempotent: revoking an already-revoked token is a no-op success.
    if token.revoked_at.is_none() {
        token.revoked_at = Some(now_iso());
        store::put_api_token(&state.engine, &token).await?;
        audit::record(
            &state.engine,
            &sess.user.id,
            "api-token-revoked",
            indices::API_TOKENS,
            Some(&token.id),
            None,
            None,
        )
        .await;
    }

    Ok(no_content())
}
