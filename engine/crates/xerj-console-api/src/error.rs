//! Error types for the Xerj Console API surface.
//!
//! Every handler returns `ConsoleResult<T>`, which `axum` serialises into a
//! JSON body with the same envelope as the rest of xerj's HTTP error
//! responses (so a Xerj Console 404 looks the same as a `/v1/...` 404 to the SPA).

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ConsoleApiError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("unauthorized: {0}")]
    Unauthorized(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("not implemented: {0}")]
    NotImplemented(String),

    #[error("rate limited")]
    RateLimited,

    /// Wraps an underlying engine / I/O failure. The full error message is
    /// emitted to tracing; the HTTP body shows a generic 500 to avoid
    /// leaking internal paths.
    #[error("internal: {0}")]
    Internal(String),
}

pub type ConsoleResult<T> = Result<T, ConsoleApiError>;

impl ConsoleApiError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::Forbidden(_) => "forbidden",
            Self::Unauthorized(_) => "unauthorized",
            Self::BadRequest(_) => "bad_request",
            Self::NotImplemented(_) => "not_implemented",
            Self::RateLimited => "rate_limited",
            Self::Internal(_) => "internal_error",
        }
    }
}

impl IntoResponse for ConsoleApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        // Internal errors never leak the message to the client. Everything
        // the SPA can act on is in `kind`; the human-readable line is for
        // logs only.
        let display = if matches!(self, Self::Internal(_)) {
            "internal server error".to_string()
        } else {
            self.to_string()
        };

        if matches!(self, Self::Internal(ref msg) if !msg.is_empty()) {
            tracing::error!(error = %self, "xerj-console internal error");
        }

        // Emit the API-contract keys (`code`/`message`) alongside the legacy
        // (`type`/`reason`) pair. `code` uses the contract vocabulary
        // (`conflict|forbidden|not_found|bad_request`, …) via `kind`, so the
        // SPA can read the documented shape while older readers still find
        // `type`/`reason`. Purely additive — no existing consumer breaks.
        let body = json!({
            "error": {
                "code":    self.kind(),
                "message": display,
                "type":    self.kind(),
                "reason":  display,
            }
        });

        (status, Json(body)).into_response()
    }
}

// Common conversions so handlers can use `?` on engine and serde failures.

impl From<xerj_common::XerjError> for ConsoleApiError {
    fn from(e: xerj_common::XerjError) -> Self {
        // Map the few error categories the Xerj Console surface is likely to surface.
        let msg = e.to_string();
        let lower = msg.to_lowercase();
        if lower.contains("not found") {
            Self::NotFound(msg)
        } else if lower.contains("already exists") {
            Self::Conflict(msg)
        } else {
            Self::Internal(msg)
        }
    }
}

impl From<xerj_engine::EngineError> for ConsoleApiError {
    fn from(e: xerj_engine::EngineError) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<serde_json::Error> for ConsoleApiError {
    fn from(e: serde_json::Error) -> Self {
        Self::BadRequest(format!("invalid json: {e}"))
    }
}

impl From<std::io::Error> for ConsoleApiError {
    fn from(e: std::io::Error) -> Self {
        Self::Internal(format!("io: {e}"))
    }
}
