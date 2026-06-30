//! `GET /me` — caller identity, plus passkey listing/revocation.
//!
//! These need the smallest possible auth surface to be usable: as soon
//! as a session cookie exists, the SPA wants `/me` so it can render the
//! "logged in as …" header. Listing/deleting passkeys is here too
//! because a freshly-enrolled user immediately sees their credential
//! list on the settings screen.

use axum::{extract::State, response::Response};
use serde::Serialize;
use serde_json::json;

use crate::auth::{audit, sessions::AuthSession, store};
use crate::error::{ConsoleApiError, ConsoleResult};
use crate::indices;
use crate::response::{no_content, ok};
use crate::state::ConsoleState;
use crate::time::now_iso;

#[derive(Serialize)]
pub struct MeResponse {
    pub user: store::User,
}

pub async fn me(sess: AuthSession) -> ConsoleResult<Response> {
    Ok(ok(MeResponse { user: sess.user }, None))
}

#[derive(Serialize)]
pub struct PasskeyListItem {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

pub async fn list_passkeys(
    State(state): State<ConsoleState>,
    sess: AuthSession,
) -> ConsoleResult<Response> {
    let pks = store::list_passkeys_for_user(&state.engine, &sess.user.id).await?;
    let items: Vec<PasskeyListItem> = pks
        .into_iter()
        .map(|p| PasskeyListItem {
            id: p.id,
            name: p.name,
            created_at: p.created_at,
            last_used_at: p.last_used_at,
        })
        .collect();
    Ok(ok(json!({ "passkeys": items }), None))
}

pub async fn delete_passkey(
    State(state): State<ConsoleState>,
    sess: AuthSession,
    axum::extract::Path(credential_id): axum::extract::Path<String>,
) -> ConsoleResult<Response> {
    // Caller can only delete their own passkeys.
    let pks = store::list_passkeys_for_user(&state.engine, &sess.user.id).await?;
    let owned = pks.iter().any(|p| p.id == credential_id);
    if !owned {
        return Err(ConsoleApiError::NotFound("passkey".into()));
    }

    // Owner cannot revoke their last passkey if no SSO is configured —
    // would lock themselves out. Edge case in v1.0 since SSO ships in
    // v1.1; for now just block the last-passkey deletion outright.
    if pks.len() == 1 {
        return Err(ConsoleApiError::Conflict(
            "cannot revoke your last passkey — enrol another first".into(),
        ));
    }

    store::delete_passkey(&state.engine, &credential_id).await?;
    audit::record(
        &state.engine,
        &sess.user.id,
        "passkey-revoked",
        indices::PASSKEYS,
        Some(&credential_id),
        None,
        None,
    )
    .await;

    // If this was the user's last passkey we'd cascade-revoke API
    // tokens here. Phase 3 wires the cascade once /auth/api-tokens
    // exists; for now the only protection is the `len() == 1` check
    // above.
    let _ = now_iso();
    Ok(no_content())
}
