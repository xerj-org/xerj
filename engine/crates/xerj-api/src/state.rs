//! Shared application state threaded through all request handlers.
//!
//! [`AppState`] is cloned cheaply into every handler via Axum's `State`
//! extractor. All mutable fields are wrapped in `Arc<DashMap<…>>` or
//! `Arc<RwLock<…>>` so concurrent requests never block each other on a
//! global lock.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
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
// Tasks API — real in-flight long-running operation registry
// ─────────────────────────────────────────────────────────────────────────────

/// One in-flight long-running operation (reindex, delete/update_by_query, …).
/// Cloned cheaply out of the registry for read-only ES responses; `cancelled`
/// is shared (Arc) with the live [`TaskHandle`] so a `_cancel` is observable.
#[derive(Clone)]
pub struct TaskEntry {
    pub id: u64,
    pub action: String,
    pub start_time_ms: i64,
    pub start_instant: Instant,
    pub node: Arc<String>,
    pub cancelled: Arc<AtomicBool>,
}

impl TaskEntry {
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
    pub fn running_nanos(&self) -> u64 {
        self.start_instant.elapsed().as_nanos() as u64
    }
    /// ES task key (`{node}:{id}`), used as the registry map key.
    pub fn key(&self) -> String {
        format!("{}:{}", self.node, self.id)
    }
}

/// RAII handle returned by [`TaskRegistry::register`]. Dropping it (at the end
/// of the long-running handler) removes the task from the registry, so the
/// Tasks API only ever reflects genuinely in-flight operations.
pub struct TaskHandle {
    inner: Arc<DashMap<String, TaskEntry>>,
    key: String,
    pub cancelled: Arc<AtomicBool>,
}

impl TaskHandle {
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

impl Drop for TaskHandle {
    fn drop(&mut self) {
        self.inner.remove(&self.key);
    }
}

/// Registry of in-flight long-running tasks. Cheaply cloneable (`Arc` inside).
#[derive(Clone)]
pub struct TaskRegistry {
    inner: Arc<DashMap<String, TaskEntry>>,
    next_id: Arc<AtomicU64>,
    node_id: Arc<String>,
}

impl TaskRegistry {
    pub fn new(node_id: Arc<String>) -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            next_id: Arc::new(AtomicU64::new(1)),
            node_id,
        }
    }
    /// Register a new in-flight task; the returned handle removes it on Drop.
    pub fn register(&self, action: &str) -> TaskHandle {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let cancelled = Arc::new(AtomicBool::new(false));
        let entry = TaskEntry {
            id,
            action: action.to_string(),
            start_time_ms: Utc::now().timestamp_millis(),
            start_instant: Instant::now(),
            node: self.node_id.clone(),
            cancelled: cancelled.clone(),
        };
        let key = entry.key();
        self.inner.insert(key.clone(), entry);
        TaskHandle { inner: self.inner.clone(), key, cancelled }
    }
    pub fn cancel(&self, id: &str) -> bool {
        match self.inner.get(id) {
            Some(e) => { e.cancelled.store(true, Ordering::Relaxed); true }
            None => false,
        }
    }
    pub fn get(&self, id: &str) -> Option<TaskEntry> {
        self.inner.get(id).map(|e| e.value().clone())
    }
    pub fn list(&self) -> Vec<TaskEntry> {
        self.inner.iter().map(|e| e.value().clone()).collect()
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
    /// In-memory registry of in-flight long-running tasks (reindex,
    /// delete/update_by_query) backing the ES Tasks API.
    pub tasks: TaskRegistry,
}

impl AppState {
    /// Construct state from a config, engine, and metrics instance.
    pub fn new(config: Config, engine: Engine, metrics: Metrics) -> Self {
        let tasks = TaskRegistry::new(engine.node_id.clone());
        Self {
            config: Arc::new(config),
            engine,
            metrics: Arc::new(metrics),
            tasks,
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
