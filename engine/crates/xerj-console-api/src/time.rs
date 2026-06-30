//! Time helpers used across handlers.
//!
//! Centralises ISO-8601 parse/format and "epoch ms now" so individual
//! handlers don't rebuild the same chrono import / format-string knowledge.

use chrono::{DateTime, TimeZone, Utc};

/// Current wall-clock time as an ISO-8601 RFC 3339 string with millisecond
/// precision and a trailing `Z`. Stored as the canonical format in every
/// `.xerj_*` system index.
pub fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Current wall-clock time as Unix milliseconds. Useful for `expires_at`
/// fields the SPA compares to `Date.now()`.
pub fn now_epoch_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Convert epoch milliseconds back to an ISO-8601 string. Returns
/// `1970-01-01T00:00:00.000Z` on overflow rather than panicking.
pub fn epoch_ms_to_iso(ms: i64) -> String {
    Utc.timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(|| Utc.timestamp_millis_opt(0).unwrap())
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Parse an ISO-8601 string back into a `DateTime<Utc>`. Tolerant of
/// trailing `Z` vs `+00:00` and of microsecond precision; returns `None`
/// on anything not parseable.
pub fn parse_iso(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}
