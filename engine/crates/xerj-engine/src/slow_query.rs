//! Slow query log — v0.8 8-P6.
//!
//! When a search exceeds a configurable wall-clock threshold (default
//! 1 second), record a structured entry into a bounded ring buffer.
//! The most-recent N entries can be retrieved via the admin endpoint
//! `GET /v1/admin/slow_queries` so an operator can debug "why is this
//! cluster suddenly slow?" without enabling a full debug log.
//!
//! Design notes:
//! - Bounded by capacity (default 256) — we never grow unbounded under
//!   a runaway slow-query workload.
//! - Lock-free reads: the buffer is a single `parking_lot::RwLock` and
//!   readers take a shared lock to clone the snapshot.
//! - Append is O(1): grow until capacity, then overwrite oldest.
//! - Threshold is per-process global; per-index overrides can be added
//!   later if needed (most cluster-debug scenarios want a single knob).
//! - Each entry carries the query body, bounded to [`SLOW_QUERY_MAX_BODY`]
//!   chars so an operator can see *what* was slow — not just that something
//!   was. The cap keeps the ring safe to serialize over the admin endpoint
//!   even at full capacity (256 × 2 KB ≈ 512 KB worst case).
//! - This is the single source of truth for slow-query observability: the
//!   `tracing` warn/error line is emitted from inside [`maybe_record`] using
//!   the SAME runtime threshold as the ring buffer, so the log and the buffer
//!   can never disagree and both track the hot-reloadable knob.
//!
//! A future enhancement will export this as a Prometheus histogram + counter
//! (`xerj_search_slow_total`) so the operator can graph slow-query rates per
//! index in Grafana.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Default threshold: 1 second.
pub const DEFAULT_SLOW_QUERY_MS: u64 = 1_000;
/// Default ring buffer capacity.
pub const DEFAULT_SLOW_QUERY_CAPACITY: usize = 256;
/// Max chars of the query body retained per entry (and emitted on the tracing
/// line). Keeps the bounded ring small and safe to serialize even at full
/// capacity, and stops a pathological multi-megabyte query DSL from bloating
/// either the buffer or a log line.
pub const SLOW_QUERY_MAX_BODY: usize = 2_048;

/// Truncate a query body to [`SLOW_QUERY_MAX_BODY`] chars on a UTF-8 char
/// boundary, appending an ellipsis when clipped.
fn truncate_body(s: &str) -> String {
    if s.len() <= SLOW_QUERY_MAX_BODY {
        return s.to_string();
    }
    let mut end = SLOW_QUERY_MAX_BODY;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SlowQueryEntry {
    /// Wall-clock time of the request, milliseconds since epoch.
    pub at_ms: u64,
    /// Index name (or "_all", "_search").
    pub index: String,
    /// Query type / route (e.g. "search", "search/agg", "knn").
    pub op: String,
    /// Total wall-clock duration in milliseconds.
    pub took_ms: u64,
    /// Number of hits (or 0 for size:0 / aggs-only).
    pub hits: u64,
    /// Optional short reason (e.g. "many segments scanned", "agg fan-out").
    pub note: String,
    /// The query body (bounded to [`SLOW_QUERY_MAX_BODY`] chars). Empty when
    /// the caller had no structured body (e.g. a URI-only `q=` request or a
    /// `match_all`). This is what makes an entry *actionable* — the operator
    /// sees the exact DSL that ran slow.
    pub query: String,
}

/// Bounded ring buffer of slow query entries.
pub struct SlowQueryLog {
    buf: RwLock<Vec<SlowQueryEntry>>,
    capacity: usize,
    threshold_ms: AtomicU64,
    /// Total slow events ever observed (for metrics / sanity).
    total: AtomicU64,
}

impl SlowQueryLog {
    pub fn new(capacity: usize, threshold_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            buf: RwLock::new(Vec::with_capacity(capacity)),
            capacity,
            threshold_ms: AtomicU64::new(threshold_ms),
            total: AtomicU64::new(0),
        })
    }

    /// Threshold getter (atomic).
    pub fn threshold_ms(&self) -> u64 {
        self.threshold_ms.load(Ordering::Relaxed)
    }

    /// Threshold setter (atomic) — can be hot-reloaded from config.
    pub fn set_threshold_ms(&self, ms: u64) {
        self.threshold_ms.store(ms, Ordering::Relaxed);
    }

    /// Total slow events observed since process start.
    pub fn total_slow(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    /// If `took >= threshold`, record an entry AND emit the slow-query
    /// tracing line. No-op otherwise. Returns `true` if the entry was
    /// recorded.
    ///
    /// This is the ONE place slow queries are surfaced. Both the ring buffer
    /// and the `tracing` warn/error line are gated on the same runtime
    /// `threshold_ms` knob (hot-reloadable via `set_threshold_ms`), so they
    /// can never drift apart — previously the ring buffer respected the knob
    /// while `Index::search` logged against hardcoded 1s/5s thresholds.
    /// `error!` fires at 5× the knob, `warn!` at 1×.
    pub fn maybe_record(
        &self,
        index: &str,
        op: &str,
        took: Duration,
        hits: u64,
        note: &str,
        query: &str,
    ) -> bool {
        let took_ms = took.as_millis() as u64;
        let threshold = self.threshold_ms.load(Ordering::Relaxed);
        if took_ms < threshold {
            return false;
        }
        self.total.fetch_add(1, Ordering::Relaxed);
        let query = truncate_body(query);
        // Unified tracing — same threshold as the ring buffer above.
        if took_ms >= threshold.saturating_mul(5) {
            tracing::error!(took_ms, index, op, hits, query = %query, "slow query");
        } else {
            tracing::warn!(took_ms, index, op, hits, query = %query, "slow query");
        }
        let entry = SlowQueryEntry {
            at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            index: index.to_string(),
            op: op.to_string(),
            took_ms,
            hits,
            note: note.to_string(),
            query,
        };
        let mut buf = self.buf.write();
        if buf.len() < self.capacity {
            buf.push(entry);
        } else {
            // Rotate: drop oldest, push newest.  Over-large capacities
            // are rare so the O(N) shift is acceptable; if this ever
            // matters we can switch to a true ring with head index.
            buf.remove(0);
            buf.push(entry);
        }
        true
    }

    /// Snapshot the current buffer (cheap clone of N small structs).
    pub fn snapshot(&self) -> Vec<SlowQueryEntry> {
        self.buf.read().clone()
    }

    /// Clear the buffer (e.g. after operator inspection).
    pub fn clear(&self) {
        self.buf.write().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn under_threshold_no_record() {
        let log = SlowQueryLog::new(8, 1000);
        let recorded = log.maybe_record("idx", "search", Duration::from_millis(500), 10, "", "");
        assert!(!recorded);
        assert!(log.snapshot().is_empty());
        assert_eq!(log.total_slow(), 0);
    }

    #[test]
    fn over_threshold_records() {
        let log = SlowQueryLog::new(8, 1000);
        let recorded = log.maybe_record(
            "idx",
            "search",
            Duration::from_millis(1500),
            10,
            "test",
            r#"{"match_all":{}}"#,
        );
        assert!(recorded);
        let snap = log.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].took_ms, 1500);
        assert_eq!(snap[0].index, "idx");
        // The query body is captured so the entry is actionable.
        assert_eq!(snap[0].query, r#"{"match_all":{}}"#);
        assert_eq!(log.total_slow(), 1);
    }

    #[test]
    fn ring_rotates_at_capacity() {
        let log = SlowQueryLog::new(2, 100);
        for i in 0..5 {
            log.maybe_record("idx", "search", Duration::from_millis(200 + i), i, "", "");
        }
        let snap = log.snapshot();
        assert_eq!(snap.len(), 2);
        // Oldest two were dropped; we should see the last two.
        assert_eq!(snap[0].took_ms, 203);
        assert_eq!(snap[1].took_ms, 204);
        assert_eq!(log.total_slow(), 5);
    }

    #[test]
    fn threshold_can_be_updated() {
        let log = SlowQueryLog::new(8, 1000);
        log.set_threshold_ms(50);
        assert_eq!(log.threshold_ms(), 50);
        let recorded = log.maybe_record("idx", "search", Duration::from_millis(100), 0, "", "");
        assert!(recorded);
    }

    #[test]
    fn long_query_body_is_truncated_on_char_boundary() {
        let log = SlowQueryLog::new(4, 100);
        // A multibyte body larger than the cap must be clipped without
        // panicking on a char boundary, and marked with an ellipsis.
        let big = "é".repeat(SLOW_QUERY_MAX_BODY); // 2 bytes each ⇒ well over the cap
        let recorded = log.maybe_record("idx", "search", Duration::from_millis(200), 1, "", &big);
        assert!(recorded);
        let snap = log.snapshot();
        assert_eq!(snap.len(), 1);
        let stored = &snap[0].query;
        assert!(
            stored.ends_with('…'),
            "clipped body should end with an ellipsis"
        );
        // Body (minus the ellipsis) never exceeds the byte cap.
        assert!(stored.len() <= SLOW_QUERY_MAX_BODY + '…'.len_utf8());
    }
}
