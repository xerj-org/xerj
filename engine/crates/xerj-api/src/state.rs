//! Shared application state threaded through all request handlers.
//!
//! [`AppState`] is cloned cheaply into every handler via Axum's `State`
//! extractor. All mutable fields are wrapped in `Arc<DashMap<…>>` or
//! `Arc<RwLock<…>>` so concurrent requests never block each other on a
//! global lock.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use xerj_common::{config::Config, metrics::Metrics, types::Schema};
use xerj_engine::Engine;

// ─────────────────────────────────────────────────────────────────────────────
// Index settings
// ─────────────────────────────────────────────────────────────────────────────

/// Per-index settings that can be configured at creation time.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IndexSettings {
    /// Number of primary shards (informational for ES compat — xerj is
    /// effectively single-shard per node in v0.1).
    pub number_of_shards: u32,
    /// Number of replica shards (informational — replication not yet impl).
    pub number_of_replicas: u32,
    /// Maximum number of result documents returned in a single search
    /// (per-index override; falls back to global config default 10,000).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_result_window: Option<u32>,
    /// Whether new fields are accepted dynamically or rejected (true by default).
    #[serde(default = "crate::state::bool_true")]
    pub dynamic_mapping: bool,
}

pub fn bool_true() -> bool {
    true
}

impl Default for IndexSettings {
    fn default() -> Self {
        Self {
            number_of_shards: 1,
            number_of_replicas: 0,
            max_result_window: None,
            dynamic_mapping: true,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// IndexHandle
// ─────────────────────────────────────────────────────────────────────────────

/// API-layer view of an index (for ES-compat endpoints that need settings).
#[derive(Debug, Clone)]
pub struct IndexHandle {
    /// Current mapping schema, wrapped for reader-writer concurrent access.
    pub schema: Arc<RwLock<Schema>>,
    /// Per-index configuration (shards, replicas, etc.).
    pub settings: IndexSettings,
    /// When this index was first created.
    pub created_at: DateTime<Utc>,
    /// When this index was last modified (document write or schema evolution).
    pub updated_at: DateTime<Utc>,
    /// Approximate document count (updated on ingest, decremented on delete).
    pub doc_count: Arc<std::sync::atomic::AtomicU64>,
}

impl IndexHandle {
    /// Create a new handle with the given schema and settings.
    pub fn new(schema: Schema, settings: IndexSettings) -> Self {
        let now = Utc::now();
        Self {
            schema: Arc::new(RwLock::new(schema)),
            settings,
            created_at: now,
            updated_at: now,
            doc_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Approximate document count (relaxed load — fine for stats).
    pub fn doc_count(&self) -> u64 {
        self.doc_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Increment the document counter by `n`.
    pub fn increment_docs(&self, n: u64) {
        self.doc_count
            .fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    }

    /// Decrement the document counter by `n` (saturating at 0).
    pub fn decrement_docs(&self, n: u64) {
        let prev = self
            .doc_count
            .fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |v| Some(v.saturating_sub(n)),
            )
            .unwrap_or(0);
        let _ = prev;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// AppState
// ─────────────────────────────────────────────────────────────────────────────

/// Shared state injected into every request handler via Axum's `State`
/// extractor.
#[derive(Clone)]
pub struct AppState {
    /// Engine-wide configuration (immutable after startup).
    pub config: Arc<Config>,
    /// The search engine (manages all indices).
    pub engine: Engine,
    /// Prometheus metrics.
    pub metrics: Arc<Metrics>,
}

impl AppState {
    /// Construct state from a config, engine, and metrics instance.
    pub fn new(config: Config, engine: Engine, metrics: Metrics) -> Self {
        Self {
            config: Arc::new(config),
            engine,
            metrics: Arc::new(metrics),
        }
    }

    /// Create state with all defaults — useful in tests.
    pub fn default_for_tests() -> Self {
        let config = Config::default();
        let metrics = Metrics::new().expect("metrics init");
        let engine = Engine::new(config.clone()).expect("engine init");
        Self::new(config, engine, metrics)
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .finish_non_exhaustive()
    }
}
