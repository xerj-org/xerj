//! API key authentication middleware.
//!
//! Checks the `Authorization` header for either:
//! - `Authorization: ApiKey <key>`
//! - `Authorization: Bearer <key>`
//!
//! When `config.auth.enabled` is `false` (or `--insecure` mode was set by
//! clearing the key), the check is skipped entirely.

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};

use crate::error::{EsErrorBody, EsErrorResponse, EsRootCause};
use crate::state::AppState;

// ─────────────────────────────────────────────────────────────────────────────

/// Axum middleware that enforces API key authentication.
///
/// Call via `middleware::from_fn_with_state(state, auth_middleware)` in the
/// router builders.
pub async fn auth_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let cfg = &state.config.auth;

    // Skip auth when disabled or no admin key is configured.
    if !cfg.enabled || cfg.admin_api_key.is_empty() {
        return next.run(req).await;
    }
    // Xerj Console lives in a peer router (mounted by `xerj-server` via
    // `Router::merge`) and runs its own session-cookie auth, so this
    // middleware never sees `/_xerj-console/*` requests — no path-prefix
    // bypass needed.

    // Extract the Authorization header.
    let auth_header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let provided_key = match auth_header {
        Some(h) => extract_key(h),
        None => None,
    };

    match provided_key {
        Some(key) if key == cfg.admin_api_key => {
            // Valid key — allow the request through.
            next.run(req).await
        }
        _ => unauthorized_response(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Extract the raw key from an `Authorization` header value.
///
/// Accepts both `ApiKey <key>` and `Bearer <key>` schemes (case-insensitive
/// scheme prefix).
fn extract_key(header: &str) -> Option<&str> {
    let lower = header.to_ascii_lowercase();
    if lower.starts_with("apikey ") {
        Some(header["ApiKey ".len()..].trim())
    } else if lower.starts_with("bearer ") {
        Some(header["Bearer ".len()..].trim())
    } else {
        None
    }
}

/// Build an ES-compatible 401 Unauthorized response.
fn unauthorized_response() -> Response {
    let reason = "missing or invalid API key in Authorization header".to_string();
    let error_type = "security_exception".to_string();

    let body = EsErrorResponse {
        error: EsErrorBody {
            root_cause: vec![EsRootCause {
                error_type: error_type.clone(),
                reason: reason.clone(),
                index: None,
            }],
            error_type,
            reason,
            request_id: None,
        },
        status: 401,
    };

    (StatusCode::UNAUTHORIZED, Json(body)).into_response()
}
