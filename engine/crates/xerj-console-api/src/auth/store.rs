//! Persistence helpers for the auth subsystem.
//!
//! The Xerj Console auth surface keeps every persistent fact (users, passkeys,
//! magic links, sessions, API tokens) in `.xerj_*` system indices.
//! This module is the **only** place that talks to the engine for those
//! reads and writes — the handler modules call typed helpers here so
//! schema and shape decisions are localised.
//!
//! Document conventions:
//! - All timestamp fields are ISO-8601 RFC 3339 millis with trailing `Z`.
//! - Every doc has `_id` chosen by the caller (we never let the engine
//!   auto-assign), so reads are by-id and idempotent.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use xerj_engine::Engine;

use crate::error::{ConsoleApiError, ConsoleResult};
use crate::indices;

// ─────────────────────────────────────────────────────────────────────────────
// User
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub email: String,
    #[serde(default)]
    pub display_name: String,
    pub role: String,
    pub status: UserStatus,
    pub created_at: String,
    #[serde(default)]
    pub last_seen_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserStatus {
    Pending,
    Active,
    Disabled,
}

impl UserStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Disabled => "disabled",
        }
    }
}

pub async fn get_user(engine: &Engine, user_id: &str) -> ConsoleResult<Option<User>> {
    let idx = engine.get_index(indices::USERS)?;
    let body = json!({
        "query": { "ids": { "values": [user_id] } },
        "size": 1
    });
    search_one(&idx, &body).await
}

pub async fn find_user_by_email(
    engine: &Engine,
    email: &str,
) -> ConsoleResult<Option<User>> {
    let idx = engine.get_index(indices::USERS)?;
    let body = json!({
        "query": { "term": { "email": email } },
        "size": 1
    });
    search_one(&idx, &body).await
}

pub async fn upsert_user(engine: &Engine, user: &User) -> ConsoleResult<()> {
    let idx = engine.get_index(indices::USERS)?;
    let doc = serde_json::to_value(user)?;
    // delete-then-create gives us upsert semantics that work for both
    // first-write and re-write. Engine's `index_document` would also
    // work but we want to be explicit about idempotency on retries.
    let _ = idx.delete_document(&user.id).await;
    idx.create_document(user.id.clone(), doc).await?;
    Ok(())
}

pub async fn count_active_users(engine: &Engine) -> ConsoleResult<u64> {
    let idx = engine.get_index(indices::USERS)?;
    let body = json!({
        "query": { "term": { "status": "active" } },
        "size": 0,
        "track_total_hits": true
    });
    let req = xerj_query::parser::parse_request(&body)
        .map_err(|e| ConsoleApiError::Internal(e.to_string()))?;
    let r = idx.search(&req).await?;
    Ok(r.total.value)
}

// ─────────────────────────────────────────────────────────────────────────────
// Magic links
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MagicLink {
    pub id: String, // = sha256(token) hex
    pub purpose: String,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub role: String,
    pub created_by: String,
    pub created_at: String,
    pub expires_at: String,
    pub used_at: Option<String>,
}

pub async fn get_magic_link(
    engine: &Engine,
    token_hash: &str,
) -> ConsoleResult<Option<MagicLink>> {
    let idx = engine.get_index(indices::MAGIC_LINKS)?;
    let body = json!({
        "query": { "ids": { "values": [token_hash] } },
        "size": 1
    });
    let mut hit: Option<Value> = search_one_raw(&idx, &body).await?;
    Ok(match hit.take() {
        None => None,
        Some(mut v) => {
            // _id round-trip: the engine returns the doc inside `_source`;
            // we don't always have it inside the body so attach the id
            // back from the search hit metadata if needed.
            if v.get("id").is_none() {
                v["id"] = Value::String(token_hash.to_string());
            }
            Some(serde_json::from_value(v)?)
        }
    })
}

pub async fn mark_magic_link_used(
    engine: &Engine,
    token_hash: &str,
    used_at_iso: &str,
) -> ConsoleResult<()> {
    let idx = engine.get_index(indices::MAGIC_LINKS)?;
    let existing = get_magic_link(engine, token_hash).await?;
    let mut link = existing.ok_or_else(|| {
        ConsoleApiError::NotFound("magic link not found at consume time".into())
    })?;
    link.used_at = Some(used_at_iso.to_string());
    let doc = serde_json::to_value(&link)?;
    let _ = idx.delete_document(token_hash).await;
    idx.create_document(token_hash.to_string(), doc).await?;
    Ok(())
}

pub async fn put_magic_link(engine: &Engine, link: &MagicLink) -> ConsoleResult<()> {
    let idx = engine.get_index(indices::MAGIC_LINKS)?;
    let doc = serde_json::to_value(link)?;
    let _ = idx.delete_document(&link.id).await;
    idx.create_document(link.id.clone(), doc).await?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Passkeys
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredPasskey {
    pub id: String, // credential_id (base64url)
    pub user_id: String,
    pub name: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    /// The full webauthn-rs `Passkey` blob, serialised to JSON. We need
    /// the entire object back at login-time to verify assertions.
    /// (`danger-allow-state-serialisation` feature is enabled in Cargo.toml.)
    pub blob: Value,
}

pub async fn list_passkeys_for_user(
    engine: &Engine,
    user_id: &str,
) -> ConsoleResult<Vec<StoredPasskey>> {
    let idx = engine.get_index(indices::PASSKEYS)?;
    let body = json!({
        "query": { "term": { "user_id": user_id } },
        "size": 100
    });
    search_all(&idx, &body).await
}

pub async fn put_passkey(engine: &Engine, pk: &StoredPasskey) -> ConsoleResult<()> {
    let idx = engine.get_index(indices::PASSKEYS)?;
    let doc = serde_json::to_value(pk)?;
    let _ = idx.delete_document(&pk.id).await;
    idx.create_document(pk.id.clone(), doc).await?;
    Ok(())
}

pub async fn count_passkeys_for_user(engine: &Engine, user_id: &str) -> ConsoleResult<u64> {
    let idx = engine.get_index(indices::PASSKEYS)?;
    let body = json!({
        "query": { "term": { "user_id": user_id } },
        "size": 0,
        "track_total_hits": true
    });
    let req = xerj_query::parser::parse_request(&body)
        .map_err(|e| ConsoleApiError::Internal(e.to_string()))?;
    let r = idx.search(&req).await?;
    Ok(r.total.value)
}

pub async fn delete_passkey(engine: &Engine, credential_id: &str) -> ConsoleResult<()> {
    let idx = engine.get_index(indices::PASSKEYS)?;
    let _ = idx.delete_document(credential_id).await;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Sessions
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub user_id: String,
    pub created_at: String,
    pub expires_at: String,
    pub last_seen_at: String,
    pub ip: Option<String>,
    pub ua: Option<String>,
    pub idp: String, // "passkey" | "oidc" | "saml"
    pub revoked_at: Option<String>,
}

pub async fn put_session(engine: &Engine, sess: &Session) -> ConsoleResult<()> {
    let idx = engine.get_index(indices::SESSIONS)?;
    let doc = serde_json::to_value(sess)?;
    let _ = idx.delete_document(&sess.id).await;
    idx.create_document(sess.id.clone(), doc).await?;
    Ok(())
}

pub async fn get_session(
    engine: &Engine,
    session_id: &str,
) -> ConsoleResult<Option<Session>> {
    let idx = engine.get_index(indices::SESSIONS)?;
    let body = json!({
        "query": { "ids": { "values": [session_id] } },
        "size": 1
    });
    search_one(&idx, &body).await
}

pub async fn revoke_session(
    engine: &Engine,
    session_id: &str,
    revoked_at_iso: &str,
) -> ConsoleResult<()> {
    let mut sess = match get_session(engine, session_id).await? {
        Some(s) => s,
        None => return Ok(()), // already gone — idempotent
    };
    sess.revoked_at = Some(revoked_at_iso.to_string());
    put_session(engine, &sess).await
}

// ─────────────────────────────────────────────────────────────────────────────
// API tokens
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiToken {
    pub id: String, // sha256(secret) hex
    pub user_id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

pub async fn put_api_token(engine: &Engine, token: &ApiToken) -> ConsoleResult<()> {
    let idx = engine.get_index(indices::API_TOKENS)?;
    let doc = serde_json::to_value(token)?;
    let _ = idx.delete_document(&token.id).await;
    idx.create_document(token.id.clone(), doc).await?;
    Ok(())
}

pub async fn get_api_token(
    engine: &Engine,
    token_hash: &str,
) -> ConsoleResult<Option<ApiToken>> {
    let idx = engine.get_index(indices::API_TOKENS)?;
    let body = json!({
        "query": { "ids": { "values": [token_hash] } },
        "size": 1
    });
    search_one(&idx, &body).await
}

pub async fn list_api_tokens_for_user(
    engine: &Engine,
    user_id: &str,
) -> ConsoleResult<Vec<ApiToken>> {
    let idx = engine.get_index(indices::API_TOKENS)?;
    let body = json!({
        "query": { "term": { "user_id": user_id } },
        "size": 100
    });
    search_all(&idx, &body).await
}

pub async fn revoke_api_tokens_for_user(
    engine: &Engine,
    user_id: &str,
    revoked_at_iso: &str,
) -> ConsoleResult<()> {
    let tokens = list_api_tokens_for_user(engine, user_id).await?;
    for mut t in tokens {
        if t.revoked_at.is_some() {
            continue;
        }
        t.revoked_at = Some(revoked_at_iso.to_string());
        put_api_token(engine, &t).await?;
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Search helpers
// ─────────────────────────────────────────────────────────────────────────────

async fn search_one<T: serde::de::DeserializeOwned>(
    idx: &std::sync::Arc<xerj_engine::Index>,
    body: &Value,
) -> ConsoleResult<Option<T>> {
    Ok(match search_one_raw(idx, body).await? {
        None => None,
        Some(v) => Some(serde_json::from_value(v)?),
    })
}

async fn search_one_raw(
    idx: &std::sync::Arc<xerj_engine::Index>,
    body: &Value,
) -> ConsoleResult<Option<Value>> {
    let req = xerj_query::parser::parse_request(body)
        .map_err(|e| ConsoleApiError::Internal(e.to_string()))?;
    let result = idx.search(&req).await?;
    let hit = result.hits.into_iter().next();
    Ok(hit.map(|h| h.source))
}

async fn search_all<T: serde::de::DeserializeOwned>(
    idx: &std::sync::Arc<xerj_engine::Index>,
    body: &Value,
) -> ConsoleResult<Vec<T>> {
    let req = xerj_query::parser::parse_request(body)
        .map_err(|e| ConsoleApiError::Internal(e.to_string()))?;
    let result = idx.search(&req).await?;
    let mut out = Vec::with_capacity(result.hits.len());
    for h in result.hits {
        out.push(serde_json::from_value(h.source)?);
    }
    Ok(out)
}
