//! Response envelope used by every Xerj Console endpoint.
//!
//! Every successful response wraps the payload in
//!
//! ```json
//! { "data": <T>, "meta": { "etag": "…", "page": { "next": "…" }, "request_id": "…" } }
//! ```
//!
//! `meta` keys are all optional — the SPA reads them when it needs to
//! pin an `If-Match` etag, follow a cursor, or grep server logs by
//! request id.

use axum::{
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, Serialize)]
pub struct PageMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}

/// Build a JSON `{ data: …, meta: … }` envelope. Pass `meta = None` for
/// endpoints that don't need any meta keys; the field is omitted from the
/// body in that case so wire size stays small.
pub fn envelope<T: Serialize>(data: T, meta: Option<Value>) -> Value {
    match meta {
        Some(m) => json!({ "data": data, "meta": m }),
        None => json!({ "data": data }),
    }
}

/// Convenience: 200 OK with envelope and an optional `ETag` header.
///
/// The etag string is wrapped in `W/"…"` per RFC 7232 §2.3 — Xerj Console etags
/// are always weak (they reflect the resource version, not byte-level
/// equality, and therefore can't satisfy strong-comparison rules).
pub fn ok<T: Serialize>(data: T, etag: Option<&str>) -> Response {
    let mut headers = HeaderMap::new();
    if let Some(e) = etag {
        if let Ok(v) = HeaderValue::from_str(&format!("W/\"{e}\"")) {
            headers.insert(axum::http::header::ETAG, v);
        }
    }
    let body = if let Some(e) = etag {
        envelope(data, Some(json!({ "etag": format!("W/\"{e}\"") })))
    } else {
        envelope(data, None)
    };
    (StatusCode::OK, headers, Json(body)).into_response()
}

/// 201 Created with a `Location` header. Used by `POST` collection endpoints.
pub fn created<T: Serialize>(data: T, location: &str, etag: Option<&str>) -> Response {
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(location) {
        headers.insert(axum::http::header::LOCATION, v);
    }
    if let Some(e) = etag {
        if let Ok(v) = HeaderValue::from_str(&format!("W/\"{e}\"")) {
            headers.insert(axum::http::header::ETAG, v);
        }
    }
    let body = if let Some(e) = etag {
        envelope(data, Some(json!({ "etag": format!("W/\"{e}\"") })))
    } else {
        envelope(data, None)
    };
    (StatusCode::CREATED, headers, Json(body)).into_response()
}

/// 204 No Content — for `DELETE` and idempotent updates with no body.
pub fn no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}
