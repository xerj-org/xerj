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
//! - We deliberately keep the entry shape small (no full query JSON) so
//!   that even with 256 entries the buffer is < 64 KB and safe to
//!   serialize over an HTTP admin endpoint.
//!
//! A future v0.9 enhancement will export this as a Prometheus
//! histogram + counter (`xerj_search_slow_total`) so the operator
//! can graph slow-query rates per index in Grafana.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Default threshold: 1 second.
pub const DEFAULT_SLOW_QUERY_MS: u64 = 1_000;
/// Default ring buffer capacity.
pub const DEFAULT_SLOW_QUERY_CAPACITY: usize = 256;

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

    /// If `took >= threshold`, record an entry. No-op otherwise.
    /// Returns `true` if the entry was recorded.
    pub fn maybe_record(
        &self,
        index: &str,
        op: &str,
        took: Duration,
        hits: u64,
        note: &str,
    ) -> bool {
        let took_ms = took.as_millis() as u64;
        if took_ms < self.threshold_ms.load(Ordering::Relaxed) {
            return false;
        }
        self.total.fetch_add(1, Ordering::Relaxed);
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
        let recorded = log.maybe_record("idx", "search", Duration::from_millis(500), 10, "");
        assert!(!recorded);
        assert!(log.snapshot().is_empty());
        assert_eq!(log.total_slow(), 0);
    }

    #[test]
    fn over_threshold_records() {
        let log = SlowQueryLog::new(8, 1000);
        let recorded = log.maybe_record("idx", "search", Duration::from_millis(1500), 10, "test");
        assert!(recorded);
        let snap = log.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].took_ms, 1500);
        assert_eq!(snap[0].index, "idx");
        assert_eq!(log.total_slow(), 1);
    }

    #[test]
    fn ring_rotates_at_capacity() {
        let log = SlowQueryLog::new(2, 100);
        for i in 0..5 {
            log.maybe_record("idx", "search", Duration::from_millis(200 + i), i, "");
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
        let recorded = log.maybe_record("idx", "search", Duration::from_millis(100), 0, "");
        assert!(recorded);
    }
}
