//! Custom axum extractors that close pitfalls in axum's defaults.
//!
//! ## Why
//!
//! axum's stock `Option<Json<T>>` extractor has a behaviour that is
//! easy to overlook: if the request body is present but malformed
//! (invalid JSON, hits the deserializer's recursion limit, exceeds
//! the body limit, etc.) the extractor **silently returns `None`**
//! instead of producing a `JsonRejection`. The handler then sees
//! `body.is_none()` and typically falls back to defaults.
//!
//! The 2026-04-25 OSS code review verified this empirically: a
//! 70-level deep nested-bool POST against `/idx/_search` arrived at
//! `parse_request` as `{"query": {"match_all": {}}}` — the deep query
//! had been silently replaced with the default. The user got back
//! 200 OK with empty hits, never knowing the query they sent had
//! been thrown away.
//!
//! `OptionalJson<T>` distinguishes the three real cases:
//!
//! | Case               | Behaviour                            |
//! |--------------------|--------------------------------------|
//! | Body absent/empty  | `Ok(OptionalJson(None))`             |
//! | Body parses to `T` | `Ok(OptionalJson(Some(T)))`          |
//! | Body is malformed  | `Err(JsonRejection)` → 400 response  |
//!
//! Use this on every POST/PUT handler that accepts a body but
//! tolerates an absent one (e.g. `/_search` works with no body —
//! defaults to match-all). For mandatory-body endpoints, use
//! `Json<T>` directly.
//!
//! ## Migration
//!
//! ```ignore
//! // Before:
//! pub async fn handler(body: Option<Json<EsSearchBody>>) {
//!     let body = body.map(|j| j.0).unwrap_or_default();
//! }
//!
//! // After:
//! use crate::extract::OptionalJson;
//! pub async fn handler(body: OptionalJson<EsSearchBody>) {
//!     let body = body.0.unwrap_or_default();
//! }
//! ```

use axum::{
    async_trait,
    body::Bytes,
    extract::{rejection::BytesRejection, FromRequest, Request},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::de::DeserializeOwned;

/// JSON-body extractor that returns `None` when the body is absent
/// but rejects (rather than silently defaulting) when the body is
/// present and invalid. See module docs for rationale.
pub struct OptionalJson<T>(pub Option<T>);

/// Why an `OptionalJson<T>` extraction failed. Always converts to a
/// 400 with an ES-shaped error body so clients see the real reason
/// instead of getting a misleading 200 with default behaviour.
#[derive(Debug)]
pub enum OptionalJsonRejection {
    /// Reading the body bytes failed (Content-Length oversize,
    /// connection reset, etc.). Forwarded from axum's `Bytes`
    /// extractor.
    BodyRead(BytesRejection),
    /// The body bytes did not parse as JSON for `T`.
    JsonParse(serde_json::Error),
    /// `Content-Type` header was set to something other than a JSON
    /// flavour. Most clients send `application/json`; we also accept
    /// `application/x-ndjson` for the bulk endpoints (those use
    /// their own extractor anyway, so this only fires for misuse).
    UnsupportedMediaType(String),
}

impl IntoResponse for OptionalJsonRejection {
    fn into_response(self) -> Response {
        let (status, reason) = match self {
            OptionalJsonRejection::BodyRead(r) => {
                (r.status(), format!("failed to read request body: {r}"))
            }
            OptionalJsonRejection::JsonParse(e) => (
                StatusCode::BAD_REQUEST,
                format!("malformed JSON in request body: {e}"),
            ),
            OptionalJsonRejection::UnsupportedMediaType(ct) => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                format!("unsupported Content-Type: {ct}"),
            ),
        };
        let body = serde_json::json!({
            "error": {
                "type": "parse_exception",
                "reason": reason,
            },
            "status": status.as_u16(),
        });
        (status, Json(body)).into_response()
    }
}

#[async_trait]
impl<S, T> FromRequest<S> for OptionalJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = OptionalJsonRejection;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        // Tolerate a missing or generic Content-Type for empty bodies;
        // only reject if the caller explicitly sets a non-JSON one
        // alongside a payload. This matches what `Json` does internally
        // but is more permissive about absent headers (which is the
        // common case for GET-with-body and empty POST bodies).
        let ct = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(OptionalJsonRejection::BodyRead)?;

        if bytes.is_empty() {
            return Ok(OptionalJson(None));
        }

        if let Some(ct_str) = ct {
            // application/json, application/json; charset=utf-8,
            // application/vnd.elasticsearch+json, application/x-ndjson, …
            // are all acceptable. Fall back to leniency on missing CT.
            let lower = ct_str.to_ascii_lowercase();
            let is_json = lower.contains("json") || lower.starts_with("text/plain");
            if !is_json {
                return Err(OptionalJsonRejection::UnsupportedMediaType(ct_str));
            }
        }

        let value: T = serde_json::from_slice(&bytes).map_err(OptionalJsonRejection::JsonParse)?;
        Ok(OptionalJson(Some(value)))
    }
}

impl<T: Default> OptionalJson<T> {
    /// Convenience for the very common pattern
    /// `body.0.unwrap_or_default()`.
    #[inline]
    pub fn into_or_default(self) -> T {
        self.0.unwrap_or_default()
    }
}

// Deref into the inner `Option<T>` so handlers that previously called
// `body.as_ref()`, `body.iter()`, `body.is_some()`, `body.as_mut()` etc.
// against the stock `Option<Json<T>>` keep working with one mechanical
// change to the type signature only.
impl<T> std::ops::Deref for OptionalJson<T> {
    type Target = Option<T>;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> std::ops::DerefMut for OptionalJson<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
