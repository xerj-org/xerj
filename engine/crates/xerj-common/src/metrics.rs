//! Prometheus metrics for the xerj engine.
//!
//! Metrics are registered in a dedicated [`prometheus::Registry`] (not the
//! global default) so that tests can create isolated registries and crates
//! can embed the registry in their own state without racing on global
//! singletons.
//!
//! ## Usage
//!
//! ```no_run
//! use xerj_common::metrics::Metrics;
//!
//! let metrics = Metrics::new().expect("metrics init failed");
//!
//! // Record a document being indexed
//! metrics.docs_indexed.inc();
//! metrics.index_latency.observe(0.003); // 3 ms
//!
//! // Expose via HTTP scrape endpoint:
//! // let body = metrics.gather_text();
//! ```

use prometheus::{
    exponential_buckets, histogram_opts, Histogram, HistogramOpts, HistogramVec, IntCounter,
    IntCounterVec, IntGauge, Opts, Registry,
};

use crate::error::XerjError;

// ═════════════════════════════════════════════════════════════════════════════
// Metrics collection
// ═════════════════════════════════════════════════════════════════════════════

/// All Prometheus metrics for the engine, grouped by type.
///
/// A single `Metrics` instance is created at startup and shared (via `Arc`)
/// across all subsystems.
#[derive(Clone)]
pub struct Metrics {
    /// The Prometheus registry that owns all metrics in this struct.
    pub registry: Registry,

    // ── Counters ─────────────────────────────────────────────────────────────
    /// Total number of documents successfully indexed.
    pub docs_indexed: IntCounter,
    /// Total number of search queries executed.
    pub queries_executed: IntCounter,
    /// Total bytes written to segment files (uncompressed).
    pub bytes_written: IntCounter,
    /// Total bytes read from segment files (uncompressed).
    pub bytes_read: IntCounter,

    // ── Per-index counters ────────────────────────────────────────────────────
    /// Documents indexed, labelled by index name.
    pub docs_indexed_by_index: IntCounterVec,
    /// Queries executed, labelled by index name.
    pub queries_by_index: IntCounterVec,

    // ── Histograms ────────────────────────────────────────────────────────────
    /// End-to-end query latency in seconds.
    pub query_latency: Histogram,
    /// End-to-end index (ingest) latency in seconds.
    pub index_latency: Histogram,
    /// Time taken to flush an in-memory buffer to a segment in seconds.
    pub flush_duration: Histogram,
    /// Time taken to complete a segment merge in seconds.
    pub merge_duration: Histogram,
    /// WAL write latency in seconds.
    pub wal_write_latency: Histogram,

    // ── Per-operation histograms ──────────────────────────────────────────────
    /// Query latency in seconds, labelled by query type.
    pub query_latency_by_type: HistogramVec,

    // ── Gauges ────────────────────────────────────────────────────────────────
    /// Number of open segments across all indices.
    pub segment_count: IntGauge,
    /// Total number of live (non-deleted) documents across all indices.
    pub doc_count: IntGauge,
    /// Approximate engine memory usage in bytes.
    pub memory_usage: IntGauge,
    /// Number of searches currently in flight.
    pub active_searches: IntGauge,
    /// Size of the WAL on disk in bytes.
    pub wal_size_bytes: IntGauge,

    // ── Query cache (RC4-W4 item 4) ───────────────────────────────────────────
    /// Cumulative internal query-result cache hits across all indices. Exposed
    /// as a gauge because it is reconciled from the engine's own atomic counters
    /// at scrape time (Prometheus `IntCounter` has no set()); the value is
    /// monotonic across a process lifetime, so `hits / (hits + misses)` gives
    /// the cache hit ratio directly.
    pub query_cache_hits: IntGauge,
    /// Cumulative internal query-result cache misses across all indices.
    pub query_cache_misses: IntGauge,
}

impl Metrics {
    /// Create a new `Metrics` instance with its own isolated registry.
    pub fn new() -> Result<Self, XerjError> {
        let registry = Registry::new();
        Self::with_registry(registry)
    }

    /// Create `Metrics` registered against the provided registry.
    ///
    /// Use this when you need to merge xerj metrics into an existing
    /// Prometheus registry (e.g., alongside application-level metrics).
    pub fn with_registry(registry: Registry) -> Result<Self, XerjError> {
        // ── Counters ──────────────────────────────────────────────────────────
        let docs_indexed = IntCounter::with_opts(Opts::new(
            "xerj_docs_indexed_total",
            "Total number of documents successfully indexed",
        ))
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        let queries_executed = IntCounter::with_opts(Opts::new(
            "xerj_queries_executed_total",
            "Total number of search queries executed",
        ))
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        let bytes_written = IntCounter::with_opts(Opts::new(
            "xerj_bytes_written_total",
            "Total bytes written to segment files (uncompressed)",
        ))
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        let bytes_read = IntCounter::with_opts(Opts::new(
            "xerj_bytes_read_total",
            "Total bytes read from segment files (uncompressed)",
        ))
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        let docs_indexed_by_index = IntCounterVec::new(
            Opts::new(
                "xerj_docs_indexed_by_index_total",
                "Documents indexed, labelled by index name",
            ),
            &["index"],
        )
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        let queries_by_index = IntCounterVec::new(
            Opts::new(
                "xerj_queries_by_index_total",
                "Queries executed, labelled by index name",
            ),
            &["index"],
        )
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        // ── Histograms ────────────────────────────────────────────────────────
        // Latency buckets: 500µs → 30s (15 buckets)
        let latency_buckets =
            exponential_buckets(0.0005, 2.0, 15).expect("latency bucket config is valid");

        let query_latency = Histogram::with_opts(
            HistogramOpts::new("xerj_query_latency_seconds", "End-to-end query latency")
                .buckets(latency_buckets.clone()),
        )
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        let index_latency = Histogram::with_opts(
            HistogramOpts::new(
                "xerj_index_latency_seconds",
                "End-to-end document indexing latency",
            )
            .buckets(latency_buckets.clone()),
        )
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        // Flush/merge duration buckets: 10ms → ~5min
        let duration_buckets =
            exponential_buckets(0.01, 2.0, 15).expect("duration bucket config is valid");

        let flush_duration = Histogram::with_opts(
            HistogramOpts::new(
                "xerj_flush_duration_seconds",
                "Time to flush an in-memory buffer to a segment",
            )
            .buckets(duration_buckets.clone()),
        )
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        let merge_duration = Histogram::with_opts(
            HistogramOpts::new(
                "xerj_merge_duration_seconds",
                "Time to complete a segment merge operation",
            )
            .buckets(duration_buckets),
        )
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        let wal_write_latency = Histogram::with_opts(
            HistogramOpts::new("xerj_wal_write_latency_seconds", "WAL write latency")
                .buckets(latency_buckets),
        )
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        let query_latency_by_type = HistogramVec::new(
            histogram_opts!(
                "xerj_query_latency_by_type_seconds",
                "Query latency, labelled by query type",
                exponential_buckets(0.0005, 2.0, 15).expect("valid")
            ),
            &["query_type"],
        )
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        // ── Gauges ────────────────────────────────────────────────────────────
        let segment_count = IntGauge::with_opts(Opts::new(
            "xerj_segment_count",
            "Number of open segments across all indices",
        ))
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        let doc_count = IntGauge::with_opts(Opts::new(
            "xerj_doc_count",
            "Total live documents across all indices",
        ))
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        let memory_usage = IntGauge::with_opts(Opts::new(
            "xerj_memory_usage_bytes",
            "Approximate engine memory usage in bytes",
        ))
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        let active_searches = IntGauge::with_opts(Opts::new(
            "xerj_active_searches",
            "Number of searches currently in flight",
        ))
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        let wal_size_bytes = IntGauge::with_opts(Opts::new(
            "xerj_wal_size_bytes",
            "Current WAL size on disk in bytes",
        ))
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        let query_cache_hits = IntGauge::with_opts(Opts::new(
            "xerj_query_cache_hits",
            "Cumulative internal query-result cache hits across all indices",
        ))
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        let query_cache_misses = IntGauge::with_opts(Opts::new(
            "xerj_query_cache_misses",
            "Cumulative internal query-result cache misses across all indices",
        ))
        .map_err(|e| XerjError::internal(format!("metrics: {e}")))?;

        // ── Register everything ───────────────────────────────────────────────
        macro_rules! reg {
            ($metric:expr) => {
                registry
                    .register(Box::new($metric.clone()))
                    .map_err(|e| XerjError::internal(format!("metrics register: {e}")))?;
            };
        }

        reg!(docs_indexed);
        reg!(queries_executed);
        reg!(bytes_written);
        reg!(bytes_read);
        reg!(docs_indexed_by_index);
        reg!(queries_by_index);
        reg!(query_latency);
        reg!(index_latency);
        reg!(flush_duration);
        reg!(merge_duration);
        reg!(wal_write_latency);
        reg!(query_latency_by_type);
        reg!(segment_count);
        reg!(doc_count);
        reg!(memory_usage);
        reg!(active_searches);
        reg!(wal_size_bytes);
        reg!(query_cache_hits);
        reg!(query_cache_misses);

        Ok(Self {
            registry,
            docs_indexed,
            queries_executed,
            bytes_written,
            bytes_read,
            docs_indexed_by_index,
            queries_by_index,
            query_latency,
            index_latency,
            flush_duration,
            merge_duration,
            wal_write_latency,
            query_latency_by_type,
            segment_count,
            doc_count,
            memory_usage,
            active_searches,
            wal_size_bytes,
            query_cache_hits,
            query_cache_misses,
        })
    }

    /// Reconcile the query-cache gauges from the engine's cumulative counters.
    /// Called by the `/v1/metrics` handler at scrape time.
    pub fn set_query_cache(&self, hits: u64, misses: u64) {
        self.query_cache_hits.set(hits as i64);
        self.query_cache_misses.set(misses as i64);
    }

    // ── Scrape helpers ────────────────────────────────────────────────────────

    /// Gather all metrics from the registry as a `MetricFamily` vector.
    pub fn gather(&self) -> Vec<prometheus::proto::MetricFamily> {
        self.registry.gather()
    }

    /// Encode all metrics to the Prometheus text exposition format.
    ///
    /// The returned string can be served directly from `/metrics`.
    pub fn gather_text(&self) -> Result<String, XerjError> {
        use prometheus::Encoder;
        let encoder = prometheus::TextEncoder::new();
        let mut buf = Vec::new();
        encoder
            .encode(&self.gather(), &mut buf)
            .map_err(|e| XerjError::internal(format!("metrics encode: {e}")))?;
        String::from_utf8(buf).map_err(|e| XerjError::internal(format!("metrics utf8: {e}")))
    }

    // ── Ergonomic recording helpers ───────────────────────────────────────────

    /// Record a successful document indexing operation.
    ///
    /// Increments both the global counter and the per-index counter.
    pub fn record_doc_indexed(&self, index: &str) {
        self.docs_indexed.inc();
        self.docs_indexed_by_index.with_label_values(&[index]).inc();
    }

    /// Drop the per-index label series for `index` (RC4-W4 item 5).
    ///
    /// Called when an index is deleted so its `queries_by_index` /
    /// `docs_indexed_by_index` label series don't linger for the lifetime of
    /// the process (unbounded metric cardinality). Removing a label that was
    /// never created is a harmless no-op — the error is ignored.
    pub fn prune_index_labels(&self, index: &str) {
        let _ = self.queries_by_index.remove_label_values(&[index]);
        let _ = self.docs_indexed_by_index.remove_label_values(&[index]);
    }

    /// Record a completed query.
    ///
    /// Increments counters and records latency histograms.
    pub fn record_query(&self, index: &str, query_type: &str, latency_secs: f64) {
        self.queries_executed.inc();
        self.queries_by_index.with_label_values(&[index]).inc();
        self.query_latency.observe(latency_secs);
        self.query_latency_by_type
            .with_label_values(&[query_type])
            .observe(latency_secs);
    }

    /// Guard type that decrements `active_searches` when dropped.
    ///
    /// ```no_run
    /// # use xerj_common::metrics::Metrics;
    /// # let metrics = Metrics::new().unwrap();
    /// let _guard = metrics.active_search_guard();
    /// // active_searches is now incremented; decremented when `_guard` is dropped.
    /// ```
    pub fn active_search_guard(&self) -> ActiveSearchGuard {
        self.active_searches.inc();
        ActiveSearchGuard {
            gauge: self.active_searches.clone(),
        }
    }
}

/// RAII guard for the `active_searches` gauge.
pub struct ActiveSearchGuard {
    gauge: IntGauge,
}

impl Drop for ActiveSearchGuard {
    fn drop(&mut self) {
        self.gauge.dec();
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Tests
// ═════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_init_succeeds() {
        Metrics::new().expect("metrics should initialise without error");
    }

    #[test]
    fn counter_increments() {
        let m = Metrics::new().unwrap();
        assert_eq!(m.docs_indexed.get(), 0);
        m.record_doc_indexed("test-index");
        assert_eq!(m.docs_indexed.get(), 1);
        assert_eq!(
            m.docs_indexed_by_index
                .with_label_values(&["test-index"])
                .get(),
            1
        );
    }

    #[test]
    fn active_search_guard_decrements_on_drop() {
        let m = Metrics::new().unwrap();
        assert_eq!(m.active_searches.get(), 0);
        {
            let _g = m.active_search_guard();
            assert_eq!(m.active_searches.get(), 1);
        }
        assert_eq!(m.active_searches.get(), 0);
    }

    #[test]
    fn gather_text_is_valid_utf8() {
        let m = Metrics::new().unwrap();
        m.docs_indexed.inc();
        let text = m.gather_text().unwrap();
        assert!(text.contains("xerj_docs_indexed_total"));
    }

    #[test]
    fn isolated_registries_do_not_conflict() {
        // Two Metrics instances must not share state
        let m1 = Metrics::new().unwrap();
        let m2 = Metrics::new().unwrap();
        m1.docs_indexed.inc();
        assert_eq!(m1.docs_indexed.get(), 1);
        assert_eq!(m2.docs_indexed.get(), 0);
    }
}
