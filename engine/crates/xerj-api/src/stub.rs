//! 501 Not Implemented helpers for ES API endpoints that are
//! wire-compatible stubs in the current release.
//!
//! ## Why this module exists
//!
//! The 2026-04-25 fairness review found that several ES API
//! endpoints are wired into the router and return 200-OK with a
//! fake success body, even though no real work happens behind
//! them (EQL search returns empty hits, Reindex returns
//! `{"failures": []}` without copying anything, Watcher / Transform
//! / Rollup / CCR / ML jobs accept inputs and silently discard
//! them). That is a hostile experience: the client thinks the
//! operation succeeded.
//!
//! v0.6.1 replaces those handlers with `501 Not Implemented`,
//! returning a structured response that says exactly which
//! milestone the feature ships in (per
//! `engine/reports/PATH_TO_100_PCT_v0.6.0_to_v1.0.md`) and
//! includes a `Retry-After: 0` header so well-behaved clients
//! don't auto-retry on a clock.
//!
//! ## Usage
//!
//! ```ignore
//! pub async fn eql_search(...) -> impl IntoResponse {
//!     not_implemented_yet("EQL search", "v1.x", "EQL is not on the v1.0 roadmap")
//! }
//! ```

use axum::{
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// Build a `501 Not Implemented` response shaped like an ES error
/// envelope so existing clients log it predictably.
///
/// * `feature`  — short human label, e.g. `"EQL search"` or
///                `"Watcher (scheduled queries)"`.
/// * `milestone` — the version this is planned for, e.g. `"v0.7"`
///                 or `"v1.x"` for un-scheduled features.
/// * `note`      — one-sentence detail; printed in `error.reason`.
///
/// Returns a 501 with a JSON body and a `Retry-After: 0` header
/// so a client polling for completion does not back off forever.
pub fn not_implemented_yet(
    feature: &'static str,
    milestone: &'static str,
    note: &'static str,
) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::RETRY_AFTER, HeaderValue::from_static("0"));

    let body = json!({
        "error": {
            "type": "not_implemented_exception",
            "reason": format!("{feature} is not implemented in this xerj build. {note}"),
            "feature": feature,
            "planned_milestone": milestone,
            "roadmap": "https://github.com/xerj-ai/xerj/blob/main/engine/reports/PATH_TO_100_PCT_v0.6.0_to_v1.0.md",
        },
        "status": 501,
    });

    (StatusCode::NOT_IMPLEMENTED, headers, Json(body)).into_response()
}
