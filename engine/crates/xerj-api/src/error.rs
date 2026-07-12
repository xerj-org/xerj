//! API error handling — converts internal [`XerjError`] values into HTTP
//! responses with the correct status code and an Elasticsearch-compatible
//! JSON body.
//!
//! # ES error format
//!
//! ```json
//! {
//!   "error": {
//!     "root_cause": [{ "type": "index_not_found_exception", "reason": "…" }],
//!     "type":   "index_not_found_exception",
//!     "reason": "no such index [my-index]"
//!   },
//!   "status": 404
//! }
//! ```

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use xerj_common::XerjError;

// ─────────────────────────────────────────────────────────────────────────────
// ES error body
// ─────────────────────────────────────────────────────────────────────────────

/// A single cause entry inside the ES `root_cause` array.
#[derive(Debug, Serialize)]
pub struct EsRootCause {
    #[serde(rename = "type")]
    pub error_type: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
}

/// The inner `error` object in an ES error response.
#[derive(Debug, Serialize)]
pub struct EsErrorBody {
    pub root_cause: Vec<EsRootCause>,
    #[serde(rename = "type")]
    pub error_type: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// Top-level ES-compatible error response shape.
#[derive(Debug, Serialize)]
pub struct EsErrorResponse {
    pub error: EsErrorBody,
    pub status: u16,
}

// ─────────────────────────────────────────────────────────────────────────────
// ApiError wrapper
// ─────────────────────────────────────────────────────────────────────────────

/// An error that can be returned from any handler.
///
/// Implements [`IntoResponse`] so it can be used directly as a handler return
/// type via `Result<T, ApiError>`.
#[derive(Debug)]
pub struct ApiError {
    pub inner: XerjError,
    pub request_id: Option<String>,
}

impl ApiError {
    pub fn new(inner: XerjError) -> Self {
        Self {
            inner,
            request_id: None,
        }
    }

    pub fn with_request_id(mut self, id: impl Into<String>) -> Self {
        self.request_id = Some(id.into());
        self
    }
}

impl From<XerjError> for ApiError {
    fn from(e: XerjError) -> Self {
        Self::new(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut status_code = self.inner.http_status();

        let mut error_type = xerj_error_type(&self.inner);
        // ES wraps mapping/query errors with a specific phrasing in `reason`;
        // our Display impl adds a "invalid mapping:" / "invalid query:" prefix
        // that doesn't match the published shape. Strip it for compat.
        let reason = match &self.inner {
            XerjError::InvalidMapping { reason } => reason.clone(),
            XerjError::InvalidQuery { reason } => reason.clone(),
            other => other.to_string(),
        };

        // Try to extract index name for root_cause annotation.
        let index_name = extract_index_name(&self.inner);

        // ES date-resolution errors from the range parser (invalid `format`
        // pattern, value that fails an explicit format, malformed date
        // math).  The query crate's Display wraps them as
        // `parse error: <msg>`; strip that so the reason matches ES
        // byte-for-byte, and surface ES's exception types
        // (`illegal_argument_exception` for a bad format string,
        // `parse_exception` in root_cause for unparseable values/math).
        let mut reason = reason;
        let mut date_root: Option<&'static str> = None;
        {
            let stripped = reason.strip_prefix("parse error: ").unwrap_or(&reason);
            if stripped.starts_with("Invalid format: [") {
                reason = stripped.to_string();
                error_type = "illegal_argument_exception".into();
                status_code = 400;
                date_root = Some("illegal_argument_exception");
            } else if stripped.starts_with("failed to parse date field [")
                || stripped.starts_with("operator not supported for date math [")
            {
                reason = stripped.to_string();
                status_code = 400;
                date_root = Some("parse_exception");
            }
        }

        // ES specific-case: `failed to create query:` comes from shard-
        // level query builders and reports `query_shard_exception` in
        // root_cause while keeping the outer wrapper as its usual type.
        let root_type = if let Some(rt) = date_root {
            rt.to_string()
        } else if reason.starts_with("failed to create query:") {
            "query_shard_exception".to_string()
        } else if reason.starts_with("function score query returned an invalid score:") {
            // ES validates function_score results and raises
            // `illegal_argument_exception` with a 400 status when a
            // function produces a non-finite or negative score
            // (e.g. ln1p on qty=-1 → ln(0) = -∞).
            error_type = "illegal_argument_exception".into();
            status_code = 400;
            "illegal_argument_exception".to_string()
        } else {
            error_type.clone()
        };

        let http_status =
            StatusCode::from_u16(status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let root_cause = vec![EsRootCause {
            error_type: root_type,
            reason: reason.clone(),
            index: index_name,
        }];

        let body = EsErrorResponse {
            error: EsErrorBody {
                root_cause,
                error_type,
                reason,
                request_id: self.request_id,
            },
            status: status_code,
        };

        (http_status, Json(body)).into_response()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Map a [`XerjError`] to an ES exception type string.
///
/// The returned strings match the `type` field in real ES error responses so
/// that existing ES clients, dashboards, and alerting rules can handle xerj
/// errors without modification.
///
/// Mapping table:
///
/// | `XerjError` variant | ES exception type | HTTP status |
/// |---|---|---|
/// | `IndexNotFound` | `index_not_found_exception` | 404 |
/// | `IndexAlreadyExists` | `resource_already_exists_exception` | 409 |
/// | `DocumentNotFound` | `document_missing_exception` | 404 |
/// | `InvalidMapping` | `mapper_parsing_exception` | 400 |
/// | `InvalidQuery` | `search_phase_execution_exception` | 400 |
/// | `StorageError` / `WalError` | `store_exception` | 500 |
/// | `SerializationError` | `json_parse_exception` | 500 |
/// | `ConfigError` | `action_request_validation_exception` | 400 |
/// | `AuthError` | `security_exception` | 401 |
/// | `TlsError` | `connect_exception` | 500 |
/// | `EmbeddingError` | `circuit_breaking_exception` | 500 |
/// | `ResourceExhausted` | `es_rejected_execution_exception` | 429 |
/// | `VersionConflict` | `version_conflict_engine_exception` | 409 |
/// | `ResultWindowTooLarge` | `illegal_argument_exception` | 400 |
/// | `IndexBlocked` | `cluster_block_exception` | 403 |
/// | `Internal` | `internal_server_error_exception` | 500 |
fn xerj_error_type(e: &XerjError) -> String {
    match e {
        // ── Index lifecycle ───────────────────────────────────────────────
        XerjError::IndexNotFound { .. } => "index_not_found_exception",
        XerjError::IndexAlreadyExists { .. } => "resource_already_exists_exception",
        // ── Document operations ───────────────────────────────────────────
        XerjError::DocumentNotFound { .. } => "document_missing_exception",
        // ── Mapping / schema ──────────────────────────────────────────────
        XerjError::InvalidMapping { .. } => "mapper_parsing_exception",
        // ── Query parsing / execution ─────────────────────────────────────
        XerjError::InvalidQuery { .. } => "search_phase_execution_exception",
        // ── Storage layer ─────────────────────────────────────────────────
        XerjError::StorageError { .. } | XerjError::WalError { .. } => "store_exception",
        // ── Serialization ─────────────────────────────────────────────────
        XerjError::SerializationError { .. } => "json_parse_exception",
        // ── Configuration ─────────────────────────────────────────────────
        XerjError::ConfigError { .. } => "action_request_validation_exception",
        // ── Auth ──────────────────────────────────────────────────────────
        XerjError::AuthError { .. } => "security_exception",
        // ── TLS ───────────────────────────────────────────────────────────
        XerjError::TlsError { .. } => "connect_exception",
        // ── Embedding / AI ────────────────────────────────────────────────
        XerjError::EmbeddingError { .. } => "circuit_breaking_exception",
        // ── Resource limits ───────────────────────────────────────────────
        XerjError::ResourceExhausted { .. } => "es_rejected_execution_exception",
        // ── Optimistic concurrency ────────────────────────────────────────
        XerjError::VersionConflict { .. } => "version_conflict_engine_exception",
        // ── Result window ─────────────────────────────────────────────────
        XerjError::ResultWindowTooLarge { .. } => "illegal_argument_exception",
        // ── Index blocks ──────────────────────────────────────────────────
        XerjError::IndexBlocked { .. } => "cluster_block_exception",
        // ── Catch-all ─────────────────────────────────────────────────────
        XerjError::Internal { .. } => "internal_server_error_exception",
    }
    .to_string()
}

/// Extract the index name from an error, when available, for the ES
/// `root_cause[].index` field. This lets clients identify which index
/// triggered the error without parsing the human-readable reason string.
fn extract_index_name(e: &XerjError) -> Option<String> {
    match e {
        XerjError::IndexNotFound { name } | XerjError::IndexAlreadyExists { name } => {
            Some(name.clone())
        }
        XerjError::DocumentNotFound { index, .. } => Some(index.clone()),
        XerjError::IndexBlocked { index, .. } => Some(index.clone()),
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Native API error response (with request_id, timing)
// ─────────────────────────────────────────────────────────────────────────────

/// A native xerj error response body (richer than the ES format).
#[derive(Debug, Serialize)]
pub struct NativeErrorResponse {
    pub error: String,
    pub reason: String,
    pub status: u16,
    pub request_id: Option<String>,
    pub took_ms: u64,
}

/// Convenience to build a native error response.
pub fn native_error(
    e: XerjError,
    request_id: Option<&str>,
    took_ms: u64,
) -> (StatusCode, axum::Json<NativeErrorResponse>) {
    let status_code = e.http_status();
    let http_status =
        StatusCode::from_u16(status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = NativeErrorResponse {
        error: xerj_error_type(&e),
        reason: e.to_string(),
        status: status_code,
        request_id: request_id.map(str::to_owned),
        took_ms,
    };
    (http_status, axum::Json(body))
}
