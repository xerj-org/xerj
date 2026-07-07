//! Shared application state threaded through all request handlers.
//!
//! [`AppState`] is cloned cheaply into every handler via Axum's `State`
//! extractor. All mutable fields are wrapped in `Arc<DashMap<…>>` or
//! `Arc<RwLock<…>>` so concurrent requests never block each other on a
//! global lock.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
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
        self.doc_count.load(std::sync::atomic::Ordering::Relaxed)
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
        TaskHandle {
            inner: self.inner.clone(),
            key,
            cancelled,
        }
    }
    pub fn cancel(&self, id: &str) -> bool {
        match self.inner.get(id) {
            Some(e) => {
                e.cancelled.store(true, Ordering::Relaxed);
                true
            }
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
// ML anomaly detection — detector configs
// ─────────────────────────────────────────────────────────────────────────────

/// A persisted anomaly-detector configuration.
///
/// A detector describes how to turn a time-series source index into a set of
/// evenly-spaced buckets (via `date_histogram` over `time_field`), reduce each
/// bucket to a single number (via `function` over `field`), and then score each
/// bucket against a statistical baseline (moving mean + stddev) built from the
/// preceding *normal* buckets. It is a real, honest statistical detector — not
/// an opaque ML model — and is fully deterministic.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MlDetector {
    /// User-supplied detector id (path segment, unique per node).
    pub detector_id: String,
    /// Source index (or index pattern) to analyse.
    pub source_index: String,
    /// Time field used to bucket the data (default `@timestamp`).
    pub time_field: String,
    /// Metric function: `count | mean | min | max | sum`.
    pub function: String,
    /// Field the metric function operates on. Ignored (and optional) for `count`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub field: Option<String>,
    /// Bucket span, ES interval syntax (e.g. `"1h"`, `"5m"`, `"1d"`).
    pub bucket_span: String,
    /// Number of standard deviations a bucket must deviate from its baseline to
    /// be flagged as an anomaly (default `3.0`).
    #[serde(default = "default_anomaly_threshold")]
    pub anomaly_threshold: f64,
    /// Creation timestamp (epoch millis).
    pub create_time_ms: i64,
    /// Optional human description.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub description: Option<String>,
}

pub fn default_anomaly_threshold() -> f64 {
    3.0
}

impl MlDetector {
    /// Absolute path of the on-disk detector registry file.
    fn registry_path(data_dir: &str) -> std::path::PathBuf {
        std::path::Path::new(data_dir)
            .join("_ml")
            .join("detectors.json")
    }

    /// Load all persisted detectors from `<data_dir>/_ml/detectors.json`.
    /// Missing/corrupt files yield an empty registry (best effort).
    pub fn load_all(data_dir: &str) -> DashMap<String, MlDetector> {
        let map = DashMap::new();
        let path = Self::registry_path(data_dir);
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(list) = serde_json::from_slice::<Vec<MlDetector>>(&bytes) {
                for d in list {
                    map.insert(d.detector_id.clone(), d);
                }
            }
        }
        map
    }

    /// Persist the full registry to disk (best effort — swallows I/O errors).
    pub fn save_all(data_dir: &str, registry: &DashMap<String, MlDetector>) {
        let path = Self::registry_path(data_dir);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let list: Vec<MlDetector> = registry.iter().map(|e| e.value().clone()).collect();
        if let Ok(bytes) = serde_json::to_vec_pretty(&list) {
            let _ = std::fs::write(&path, bytes);
        }
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
    ///
    /// Wrapped in `Arc` so that cloning `AppState` — which Axum/tower does
    /// several times per request across the middleware stack (auth, request-id,
    /// trace, CORS) and once more for the handler — bumps a single refcount
    /// instead of deep-cloning `Engine`'s ~30 inner `Arc<DashMap<…>>` fields
    /// (plus a `PathBuf`). Under concurrent load those ~30 shared refcounts
    /// bounced between cores on every clone/drop; a saturated `perf` profile
    /// attributed ~42 % of server CPU to `AppState::clone` + its `drop`. All
    /// `Engine` methods take `&self`, so `state.engine.foo()` and `&state.engine`
    /// keep working unchanged via `Deref` coercion.
    pub engine: Arc<Engine>,
    /// Prometheus metrics.
    pub metrics: Arc<Metrics>,
    /// In-memory registry of in-flight long-running tasks (reindex,
    /// delete/update_by_query) backing the ES Tasks API.
    pub tasks: TaskRegistry,
    /// In-process Elasticsearch license document — mutated by `PUT /_license`,
    /// reflected by `GET /_license` and the `license` block of `GET /_xpack`.
    pub license: Arc<RwLock<serde_json::Value>>,
    /// Whether the Watcher service is "running" (toggled by
    /// `_watcher/_start` / `_watcher/_stop`; reported in `_xpack`).
    pub watcher_active: Arc<AtomicBool>,
    /// Registry of anomaly-detector configs, keyed by detector id. Loaded from
    /// `<data_dir>/_ml/detectors.json` at startup and re-persisted on every
    /// create/delete. Backs the `PUT/GET/DELETE /_ml/anomaly_detectors/{id}`,
    /// `POST /_ml/anomaly_detectors/{id}/_score`, and `_cat/ml` endpoints.
    pub ml_detectors: Arc<DashMap<String, MlDetector>>,
}

impl AppState {
    /// Construct state from a config, engine, and metrics instance.
    pub fn new(config: Config, engine: Engine, metrics: Metrics) -> Self {
        let tasks = TaskRegistry::new(engine.node_id.clone());
        let now = Utc::now();
        let license = Arc::new(RwLock::new(serde_json::json!({
            "uid": uuid::Uuid::new_v4().to_string(),
            "type": "basic",
            "mode": "basic",
            "status": "active",
            "issue_date": now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
            "issue_date_in_millis": now.timestamp_millis(),
            "start_date_in_millis": now.timestamp_millis(),
            "expiry_date_in_millis": i64::MAX,
            "max_nodes": 1000,
            "issued_to": "xerj",
            "issuer": "xerj"
        })));
        let watcher_active = Arc::new(AtomicBool::new(true));
        let ml_detectors = Arc::new(MlDetector::load_all(&config.server.data_dir));
        Self {
            config: Arc::new(config),
            engine: Arc::new(engine),
            metrics: Arc::new(metrics),
            tasks,
            license,
            watcher_active,
            ml_detectors,
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
        f.debug_struct("AppState").finish_non_exhaustive()
    }
}
