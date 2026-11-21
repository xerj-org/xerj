//! Slow-hold reporting for the locks and brackets a search or a write can
//! wait behind (#3324). A hold or wait longer than the threshold is logged at
//! WARN with its call site, so a multi-second stall under write load names
//! what was blocking instead of only showing up as a slow request.
//!
//! The threshold is 1 s, or `XERJ_STALL_TRACE_MS` when set.

use std::sync::OnceLock;
use std::time::Duration;

/// Holds and waits at or above this duration are reported.
pub fn threshold() -> Duration {
    static THRESHOLD: OnceLock<Duration> = OnceLock::new();
    *THRESHOLD.get_or_init(|| {
        std::env::var("XERJ_STALL_TRACE_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_secs(1))
    })
}

/// True when `elapsed` should be reported.
pub fn is_slow(elapsed: Duration) -> bool {
    elapsed >= threshold()
}
