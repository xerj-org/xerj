//! Engine: manages multiple named indices.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;
use tracing::{info, warn};
use xerj_common::config::Config;
use xerj_common::types::{IndexName, Schema};

use crate::index::{Index, IndexStats};
use crate::{EngineError, Result};

// ── Clustering types ─────────────────────────────────────────────────────────

/// Node identity and cluster membership configuration.
///
/// # Clustering Roadmap
///
/// xerj is currently single-node (single-shard per index).  The fields below
/// are the first step toward a distributed cluster:
///
/// 1. **Node identity** — `ClusterConfig` establishes a stable node ID, human
///    name, and cluster name.  This is the minimum required to join a cluster.
///
/// 2. **Shard routing** (next)
///    - Each index will support N primary shards, distributed across nodes via
///      consistent hashing of the document ID.
///    - The routing table will be propagated via a Raft-backed cluster state.
///
/// 3. **Leader election** (next)
///    - One node is designated the "master" (cluster coordinator).
///    - `GET /_cluster/health` will report `relocating_shards` / `unassigned_shards`.
///
/// 4. **Replication** (future)
///    - Each primary shard will have M replica shards on different nodes.
///    - Writes propagate via async WAL shipping to replica nodes.
///    - Reads can be served from any replica.
///
/// 5. **Allocation** (future)
///    - `GET /_cluster/allocation/explain` will return real allocation decisions.
///    - Shard rebalancing will be triggered automatically when nodes join/leave.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfig {
    /// Stable unique identifier for this node (UUID recommended).
    pub node_id: String,
    /// Human-readable node name shown in `_cat/nodes`.
    pub node_name: String,
    /// Logical cluster name; nodes with different names will not join each other.
    pub cluster_name: String,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            node_id: uuid::Uuid::new_v4().to_string(),
            node_name: "xerj-node-1".to_string(),
            cluster_name: "xerj".to_string(),
        }
    }
}

// ── Public types ─────────────────────────────────────────────────────────────

/// Summary information about one index (for the list endpoint).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexInfo {
    pub name: String,
    pub doc_count: u64,
    pub segment_count: usize,
    pub schema_version: u64,
}

/// Overall engine health.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub status: String,
    pub index_count: usize,
    pub total_docs: u64,
    pub version: String,
}

/// Index template — applied when a new index matching the pattern is created.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexTemplate {
    pub index_patterns: Vec<String>,
    pub settings: Value,
    pub mappings: Value,
    pub priority: i32,
}

/// Active scroll context holding all matching hits.
pub struct ScrollContext {
    pub index: String,
    pub hits: Vec<xerj_query::executor::Hit>,
    pub position: usize,
    pub page_size: usize,
    pub created: Instant,
}

/// Point-in-time search context — snapshots the set of indices and the
/// max seq_no visible at open time so later searches against the PIT
/// ignore docs that arrived after the snapshot.
#[derive(Debug, Clone)]
pub struct PitContext {
    /// Indices the PIT was opened against (resolved from wildcard).
    pub indices: Vec<String>,
    /// Optional index_filter query AST (applied on each participating
    /// index — matching it narrows the snapshot).
    pub index_filter: Option<Value>,
    /// Per-index snapshot of max visible seq_no at open time.
    pub index_max_seq: std::collections::HashMap<String, u64>,
    pub created: Instant,
    /// Wall-clock deadline after which the background sweeper drops
    /// this PIT. Computed at open time as `created + keep_alive`.
    /// Pre-v0.6.2 PITs had no TTL and accumulated forever — trivial
    /// memory leak vector. ES requires `keep_alive`; we default to
    /// `Config.pit.default_keep_alive` (5 min) when missing and
    /// silently cap at `Config.pit.max_keep_alive` (24 h).
    pub expires_at: Instant,
}

/// A data stream backed by one or more time-series indices.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataStream {
    pub name: String,
    pub backing_indices: Vec<String>,
    pub timestamp_field: String,
    pub generation: u64,
}

/// A created API key, kept in memory so the key can be re-authenticated by
/// the auth middleware. Lost on restart (no persistence yet) and not yet
/// revocable via the DELETE/query endpoints — those are follow-ups.
#[derive(Debug, Clone)]
pub struct ApiKeyRecord {
    /// Caller-supplied key name (informational).
    pub name: String,
    /// The secret half of the credential — the `api_key` value returned to
    /// the caller, i.e. the part after `id:` in the decoded `ApiKey` header.
    pub secret: String,
    /// Creation time in epoch milliseconds.
    pub creation_ms: u64,
    /// Absolute expiration in epoch milliseconds, or `None` if the key never
    /// expires.
    pub expiration_ms: Option<u64>,
    /// Set once the key has been invalidated (revoked).
    pub invalidated: bool,
}

// ── Engine ────────────────────────────────────────────────────────────────────

/// Top-level engine — manages multiple named indices.
///
/// `Engine` is cheaply clonable (`Arc`-backed) and safe to share across
/// async tasks.
#[derive(Clone)]
pub struct Engine {
    config: Arc<Config>,
    indices: Arc<DashMap<String, Arc<Index>>>,
    data_dir: PathBuf,
    /// alias_name → list of index names
    pub aliases: Arc<DashMap<String, Vec<String>>>,
    /// template_name → IndexTemplate
    pub templates: Arc<DashMap<String, IndexTemplate>>,
    /// scroll_id → ScrollContext
    pub scrolls: Arc<DashMap<String, ScrollContext>>,
    /// pipeline_id → pipeline definition JSON
    pub pipelines: Arc<DashMap<String, Value>>,
    /// index_name → open/closed state (true = closed)
    pub closed_indices: Arc<DashMap<String, bool>>,
    /// data stream name → DataStream
    pub data_streams: Arc<DashMap<String, DataStream>>,
    /// ILM policy name → policy JSON
    pub ilm_policies: Arc<DashMap<String, Value>>,
    /// component template name → template JSON
    pub component_templates: Arc<DashMap<String, Value>>,
    /// snapshot repository name → repo config JSON
    pub snapshot_repos: Arc<DashMap<String, Value>>,
    /// snapshot repo/snapshot_name → snapshot info JSON
    pub snapshots: Arc<DashMap<String, Value>>,
    /// cluster-level settings (persistent + transient)
    pub cluster_settings: Arc<RwLock<Value>>,
    /// enrich policy name → policy JSON
    pub enrich_policies: Arc<DashMap<String, Value>>,
    /// watcher id → watch definition JSON
    pub watches: Arc<DashMap<String, Value>>,
    /// script/template id → template source JSON (for _search/template)
    pub search_templates: Arc<DashMap<String, Value>>,
    /// async search id → stored result JSON
    pub async_searches: Arc<DashMap<String, Value>>,
    /// Names of index directories that failed to open on startup (health = red).
    pub failed_indices: Arc<DashMap<String, String>>,
    /// transform id → transform definition JSON
    pub transforms: Arc<DashMap<String, Value>>,
    /// index_name → frozen state (true = frozen / read-only)
    pub frozen_indices: Arc<DashMap<String, bool>>,
    /// rollup job id → job definition JSON
    pub rollup_jobs: Arc<DashMap<String, Value>>,
    /// CCR auto-follow pattern name → pattern JSON
    pub ccr_auto_follow: Arc<DashMap<String, Value>>,
    /// API key id → record. Populated by `POST /_security/api_key` so the
    /// auth middleware can re-authenticate `Authorization: ApiKey <encoded>`.
    /// In-memory only (lost on restart).
    pub api_keys: Arc<DashMap<String, ApiKeyRecord>>,
    /// legacy index template name (v1 /_template) → template JSON
    pub legacy_templates: Arc<DashMap<String, Value>>,
    /// pipeline_name → compiled, executable Pipeline (typed transform pipeline)
    pub transform_pipelines: Arc<DashMap<String, xerj_wasm::pipeline::Pipeline>>,
    /// PIT id → PitContext. Records the max seq_no per index at PIT
    /// open time so searches using `pit.id: ...` filter out any docs
    /// that appeared after the snapshot was taken.
    pub pits: Arc<DashMap<String, PitContext>>,
    /// index_name → opaque settings blob as last written by the user.
    /// Stored as-is so `GET /{index}/_settings` can round-trip what was
    /// provided on creation or updated by PUT /{index}/_settings. Keys
    /// include `number_of_replicas`, `refresh_interval`, `max_result_window`
    /// and anything else ES accepts.
    pub index_settings: Arc<DashMap<String, Value>>,
    /// index_name → mapping properties JSON, also stored as written so
    /// `GET /{index}/_mapping` and `indices.create` round-trip.
    pub index_mappings: Arc<DashMap<String, Value>>,
    /// index_name → aliases object as seen at create-time (so we can
    /// round-trip filter, routing, is_write_index etc. that would
    /// otherwise be collapsed to `{}` in the simple alias map).
    pub index_alias_metadata: Arc<DashMap<String, Value>>,

    /// Slow query log — v0.8 8-P6.  Per-process bounded ring buffer of
    /// queries that exceeded the configured wall-clock threshold.
    pub slow_query: Arc<crate::slow_query::SlowQueryLog>,

    /// Tamper-evident audit log — v0.9 9-P4.  Hash-chained append-only
    /// log of every search / index / delete / admin op.
    pub audit: Arc<crate::audit::AuditLog>,

    /// Role store — v0.9 9-P2.  In-memory map of role name → Role.
    /// Wired into auth middleware in v0.9.0-beta.1.
    pub roles: Arc<crate::rbac::RoleStore>,

    // ── V4 M5.2: cluster routing ────────────────────────────────────────────
    /// Local node id (matches `cluster.peers` entry name when clustering
    /// is enabled). Used by the write path to decide if a doc belongs
    /// to this node or must be forwarded to a peer.
    ///
    /// When clustering is disabled, this is set to `"local"` and every
    /// routing decision resolves to "this node" — so the write path is
    /// 100 % backward compatible with single-node deployments.
    pub node_id: Arc<String>,
    /// Shard router shared across all indices. For single-node clusters
    /// this has `num_shards = 1` and always routes to `node_id`, which
    /// is a no-op. In multi-node mode, populated via
    /// `ShardRouter::update_from_metadata` whenever the Raft log
    /// commits a new shard assignment.
    pub shard_router: Arc<parking_lot::RwLock<xerj_cluster::router::ShardRouter>>,

    /// Exclusive advisory lock on `<data_dir>/node.lock`, held for the
    /// engine's whole lifetime (RC4 blocker 13). Acquired in
    /// [`Engine::new`] BEFORE any index is opened (i.e. before any WAL
    /// replay or segment flush can touch the directory), so a second
    /// xerj process pointed at a live data dir fails fast instead of
    /// replaying the WAL and flushing duplicate segments into it — the
    /// classic systemd double-start corruption. The lock is an OS-level
    /// `flock`-style lock (`std::fs::File::try_lock`), so it dies with
    /// the process: a `kill -9` releases it automatically and a stale
    /// `node.lock` file never blocks the next boot.
    _node_lock: Arc<std::fs::File>,
}

impl Engine {
    /// Create a new engine, opening any existing indices from disk.
    pub fn new(config: Config) -> Result<Self> {
        let data_dir = PathBuf::from(&config.server.data_dir);
        std::fs::create_dir_all(&data_dir)?;

        // Data-dir exclusivity (RC4 blocker 13): take the node lock BEFORE
        // scanning/opening any index below — Index::open replays the WAL
        // and can flush segments, which must never happen while another
        // process serves the same directory.
        let node_lock = Arc::new(Self::acquire_node_lock(&data_dir)?);

        // Apply operator-tunable aggregation bucket cap. Stored in a static
        // AtomicUsize inside aggs.rs so all per-bucket-allocator hot loops
        // can read it with no plumbing through every agg signature.
        crate::aggs::set_max_buckets(config.limits.max_buckets);

        let engine = Self {
            config: Arc::new(config),
            indices: Arc::new(DashMap::new()),
            data_dir: data_dir.clone(),
            aliases: Arc::new(DashMap::new()),
            templates: Arc::new(DashMap::new()),
            scrolls: Arc::new(DashMap::new()),
            pipelines: Arc::new(DashMap::new()),
            closed_indices: Arc::new(DashMap::new()),
            data_streams: Arc::new(DashMap::new()),
            ilm_policies: Arc::new(DashMap::new()),
            component_templates: Arc::new(DashMap::new()),
            snapshot_repos: Arc::new(DashMap::new()),
            snapshots: Arc::new(DashMap::new()),
            cluster_settings: Arc::new(RwLock::new(serde_json::json!({
                "persistent": {},
                "transient": {}
            }))),
            enrich_policies: Arc::new(DashMap::new()),
            watches: Arc::new(DashMap::new()),
            search_templates: Arc::new(DashMap::new()),
            async_searches: Arc::new(DashMap::new()),
            failed_indices: Arc::new(DashMap::new()),
            transforms: Arc::new(DashMap::new()),
            frozen_indices: Arc::new(DashMap::new()),
            rollup_jobs: Arc::new(DashMap::new()),
            ccr_auto_follow: Arc::new(DashMap::new()),
            api_keys: Arc::new(DashMap::new()),
            legacy_templates: Arc::new(DashMap::new()),
            transform_pipelines: Arc::new(DashMap::new()),
            pits: Arc::new(DashMap::new()),
            index_settings: Arc::new(DashMap::new()),
            index_mappings: Arc::new(DashMap::new()),
            index_alias_metadata: Arc::new(DashMap::new()),
            slow_query: crate::slow_query::SlowQueryLog::new(
                crate::slow_query::DEFAULT_SLOW_QUERY_CAPACITY,
                crate::slow_query::DEFAULT_SLOW_QUERY_MS,
            ),
            audit: crate::audit::AuditLog::new(crate::audit::DEFAULT_AUDIT_CAPACITY),
            roles: crate::rbac::RoleStore::new(),
            // Single-node default: 1 shard, "local" owner. Writes never
            // forward; multi-node mode overrides these via the Raft
            // commit handler when shard assignments change.
            node_id: Arc::new("local".to_string()),
            shard_router: Arc::new(parking_lot::RwLock::new(
                xerj_cluster::router::ShardRouter::new(1),
            )),
            _node_lock: node_lock,
        };

        // Scan data_dir for existing index directories.
        if let Ok(read_dir) = std::fs::read_dir(&data_dir) {
            for entry in read_dir.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                // Check if this looks like an index directory (has a WAL subdirectory).
                if !path.join("wal").exists() {
                    continue;
                }
                let name_str = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };
                let index_name = match IndexName::new(&name_str) {
                    Ok(n) => n,
                    Err(_) => {
                        warn!("Skipping directory '{}': not a valid index name", name_str);
                        continue;
                    }
                };
                match Index::open(index_name.clone(), &engine.config, &data_dir) {
                    Ok(idx) => {
                        info!(name = name_str.as_str(), "opened existing index");
                        // Restore the raw ES mapping blob (analyzers, formats,
                        // dims — full fidelity) BEFORE any ingest/query can run,
                        // so GET /_mapping and mapping-dependent code paths see
                        // the same mapping as pre-restart.
                        engine.load_persisted_es_mapping(&name_str);
                        engine.indices.insert(name_str, idx);
                    }
                    Err(e) => {
                        warn!(name = name_str.as_str(), error = %e, "failed to open index");
                        engine.failed_indices.insert(name_str, e.to_string());
                    }
                }
            }
        }

        // Spawn the PIT sweeper. Pre-v0.6.2 PITs accumulated forever;
        // every open without close was a memory leak. The sweeper
        // walks `engine.pits` every `pit.sweep_interval_secs` and
        // drops any with `expires_at < now`. Cheap (DashMap iter +
        // Instant compare) and bounded by the live PIT count.
        engine.spawn_pit_sweeper();

        Ok(engine)
    }

    /// Acquire the exclusive `<data_dir>/node.lock` advisory lock (RC4
    /// blocker 13 — data-dir exclusivity).
    ///
    /// Uses `std::fs::File::try_lock` (flock-style, non-blocking): if
    /// another process already holds the lock we fail fast with the
    /// holder's pid instead of replaying its WAL and flushing duplicate
    /// segments into a live directory. On success our own pid is written
    /// into the file purely as a diagnostic for the *next* contender —
    /// exclusivity comes from the OS lock, never from the pid content,
    /// so a stale file left by `kill -9` (lock auto-released at process
    /// death) can never wedge a reboot.
    fn acquire_node_lock(data_dir: &std::path::Path) -> Result<std::fs::File> {
        use std::io::Write;
        let lock_path = data_dir.join("node.lock");
        // Never O_TRUNC here: truncation must only happen AFTER the lock
        // is ours, or a losing contender would erase the holder's pid.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        match file.try_lock() {
            Ok(()) => {
                let _ = file.set_len(0);
                let _ = writeln!(&file, "{}", std::process::id());
                let _ = file.sync_all();
                Ok(file)
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                let holder = std::fs::read_to_string(&lock_path)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "unknown".to_string());
                Err(EngineError::Common(xerj_common::XerjError::config(format!(
                    "data dir '{}' is already in use by another running xerj process \
                     (pid {holder}, lock file '{}') — refusing to start. Two processes \
                     serving one data dir would replay each other's WAL and corrupt \
                     segments; stop the other process or point this one at its own \
                     server.data_dir.",
                    data_dir.display(),
                    lock_path.display(),
                ))))
            }
            Err(std::fs::TryLockError::Error(e)) => Err(EngineError::Common(
                xerj_common::XerjError::config(format!(
                    "failed to acquire node lock '{}': {e}",
                    lock_path.display()
                )),
            )),
        }
    }

    /// Drop PIT contexts whose `expires_at` is in the past. Cheap
    /// O(N) walk; runs on the background sweeper task and is also
    /// invoked opportunistically inside `open_pit` so a tight
    /// open-without-close loop self-bounds without waiting for the
    /// next sweep tick.
    pub fn sweep_expired_pits(&self) -> usize {
        let now = Instant::now();
        let expired: Vec<String> = self
            .pits
            .iter()
            .filter(|e| e.value().expires_at <= now)
            .map(|e| e.key().clone())
            .collect();
        for id in &expired {
            self.pits.remove(id);
        }
        expired.len()
    }

    fn spawn_pit_sweeper(&self) {
        let pits = Arc::clone(&self.pits);
        let interval = std::time::Duration::from_secs(self.config.pit.sweep_interval_secs.max(1));
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            // Skip the immediate first tick — Engine::new just ran, so
            // the pits map is empty.
            tick.tick().await;
            loop {
                tick.tick().await;
                let now = Instant::now();
                let expired: Vec<String> = pits
                    .iter()
                    .filter(|e| e.value().expires_at <= now)
                    .map(|e| e.key().clone())
                    .collect();
                if expired.is_empty() {
                    continue;
                }
                for id in &expired {
                    pits.remove(id);
                }
                tracing::debug!(
                    swept = expired.len(),
                    remaining = pits.len(),
                    "PIT sweep dropped expired contexts",
                );
            }
        });
    }

    /// Create a new index, applying any matching index template.
    pub fn create_index(&self, name: &str, schema: Schema) -> Result<()> {
        let index_name = IndexName::new(name).map_err(EngineError::Common)?;

        if self.indices.contains_key(name) {
            return Err(EngineError::Common(
                xerj_common::XerjError::index_already_exists(name),
            ));
        }

        // Apply matching template (highest priority wins).
        let mut effective_schema = schema;
        if let Some(tmpl) = self.best_matching_template(name) {
            // Merge template mappings into schema.
            if let Some(props) = tmpl.mappings.get("properties") {
                // Parse template properties and add any missing fields.
                if let Some(obj) = props.as_object() {
                    for (field_name, field_def) in obj {
                        let es_type = field_def
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("object");
                        let native_type = es_type_to_field_type(es_type);
                        if !effective_schema
                            .fields
                            .iter()
                            .any(|f| &f.name == field_name)
                        {
                            let fc = xerj_common::types::FieldConfig::new(
                                field_name.clone(),
                                native_type,
                            );
                            let _ = effective_schema.add_field(fc);
                        }
                    }
                }
            }
        }

        let idx = Index::create(index_name, effective_schema, &self.config, &self.data_dir)?;
        self.indices.insert(name.to_string(), idx);
        info!(name, "index created");
        Ok(())
    }

    /// Create a new index with explicit settings (e.g. custom analysis configuration).
    ///
    /// The `settings` value is stored alongside the index and used to configure
    /// the analyzer registry (custom analyzers, synonym filters, ngram tokenizers, etc.).
    pub fn create_index_with_settings(
        &self,
        name: &str,
        schema: Schema,
        settings: serde_json::Value,
    ) -> Result<()> {
        let index_name = IndexName::new(name).map_err(EngineError::Common)?;

        if self.indices.contains_key(name) {
            return Err(EngineError::Common(
                xerj_common::XerjError::index_already_exists(name),
            ));
        }

        let idx = Index::create_with_settings(
            index_name,
            schema,
            settings,
            &self.config,
            &self.data_dir,
        )?;
        self.indices.insert(name.to_string(), idx);
        info!(name, "index created with custom settings");
        Ok(())
    }

    /// Register the raw ES mapping blob for `name` and persist it into the
    /// index data dir (atomic temp-file + rename) so `GET /{index}/_mapping`
    /// round-trips the exact user-provided mapping (analyzers, date formats,
    /// dense_vector dims/similarity, multi-fields) across restarts.
    ///
    /// This is the single write path for `engine.index_mappings` — both
    /// index-create-with-mappings and PUT /_mapping go through here.
    pub fn put_index_mapping(&self, name: &str, mapping: Value) {
        let index_dir = self.data_dir.join(name);
        if index_dir.is_dir() {
            match serde_json::to_vec_pretty(&mapping) {
                Ok(bytes) => {
                    if let Err(e) =
                        crate::index::write_file_atomic(&index_dir.join("es_mapping.json"), &bytes)
                    {
                        warn!(index = name, error = %e, "failed to persist es_mapping.json");
                    }
                }
                Err(e) => {
                    warn!(index = name, error = %e, "failed to serialize index mapping for persistence");
                }
            }
        }
        self.index_mappings.insert(name.to_string(), mapping);
    }

    /// Load a previously-persisted raw ES mapping blob for `name` (if any)
    /// into the in-memory `index_mappings` map.  Called whenever an index is
    /// (re)opened from disk — boot scan and snapshot restore.  A missing
    /// file is fine (pre-fix indices, dynamic-only indices): readers fall
    /// back to schema-derived properties from `schema.json`.
    fn load_persisted_es_mapping(&self, name: &str) {
        let path = self.data_dir.join(name).join("es_mapping.json");
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(mapping) => {
                self.index_mappings.insert(name.to_string(), mapping);
            }
            Err(e) => {
                warn!(index = name, error = %e, "ignoring corrupt es_mapping.json");
            }
        }
    }

    /// Find the highest-priority template matching `index_name`.
    fn best_matching_template(&self, index_name: &str) -> Option<IndexTemplate> {
        let mut best: Option<(i32, IndexTemplate)> = None;
        for entry in self.templates.iter() {
            let tmpl = entry.value();
            let matches = tmpl
                .index_patterns
                .iter()
                .any(|pat| glob_match(pat, index_name));
            if matches {
                let priority = tmpl.priority;
                if best.as_ref().map(|(p, _)| priority > *p).unwrap_or(true) {
                    best = Some((priority, tmpl.clone()));
                }
            }
        }
        best.map(|(_, t)| t)
    }

    // ── Alias methods ─────────────────────────────────────────────────────────

    /// Add an alias pointing to an index.
    pub fn add_alias(&self, alias: &str, index: &str) {
        let mut entry = self.aliases.entry(alias.to_string()).or_default();
        if !entry.contains(&index.to_string()) {
            entry.push(index.to_string());
        }
    }

    /// Remove an alias's association with an index.
    pub fn remove_alias(&self, alias: &str, index: &str) {
        if let Some(mut entry) = self.aliases.get_mut(alias) {
            entry.retain(|i| i != index);
        }
        // Clean up empty alias entries.
        self.aliases.retain(|_, v| !v.is_empty());
    }

    /// Resolve a name: if it's an alias, return the aliased index names;
    /// otherwise return the name itself (if the index exists).
    pub fn resolve_alias(&self, name: &str) -> Vec<String> {
        if let Some(indices) = self.aliases.get(name) {
            return indices.clone();
        }
        vec![name.to_string()]
    }

    /// Delete an index and all its data.
    ///
    /// Also drops any aliases that pointed only at this index (matching ES
    /// semantics) and clears the `closed_indices` flag so the name is
    /// truly gone when another test recreates it.
    pub async fn delete_index(&self, name: &str) -> Result<()> {
        let idx =
            self.indices.remove(name).map(|(_, v)| v).ok_or_else(|| {
                EngineError::Common(xerj_common::XerjError::index_not_found(name))
            })?;

        idx.delete_all_data().await?;

        // Remove this index from every alias that references it; drop the
        // alias entirely when its backing list becomes empty.
        let empty_aliases: Vec<String> = self
            .aliases
            .iter_mut()
            .filter_map(|mut entry| {
                entry.value_mut().retain(|n| n != name);
                if entry.value().is_empty() {
                    Some(entry.key().clone())
                } else {
                    None
                }
            })
            .collect();
        for a in empty_aliases {
            self.aliases.remove(&a);
        }

        self.closed_indices.remove(name);
        self.index_settings.remove(name);
        self.index_mappings.remove(name);
        self.index_alias_metadata.remove(name);

        info!(name, "index deleted");
        Ok(())
    }

    /// Get a reference to an index by name, resolving aliases first.
    /// If the name is an alias pointing to multiple indices, returns the first one.
    pub fn get_index(&self, name: &str) -> Result<Arc<Index>> {
        // Check if name is an alias — if so, resolve to the first backing index.
        if let Some(aliased) = self.aliases.get(name) {
            if let Some(real_name) = aliased.first() {
                return self
                    .indices
                    .get(real_name.as_str())
                    .map(|r| Arc::clone(r.value()))
                    .ok_or_else(|| {
                        EngineError::Common(xerj_common::XerjError::index_not_found(real_name))
                    });
            }
        }
        self.indices
            .get(name)
            .map(|r| Arc::clone(r.value()))
            .ok_or_else(|| EngineError::Common(xerj_common::XerjError::index_not_found(name)))
    }

    /// Return an index by name, creating it if it doesn't exist (ES behaviour).
    pub fn get_or_create_index(&self, name: &str) -> Result<Arc<Index>> {
        if let Ok(idx) = self.get_index(name) {
            return Ok(idx);
        }
        // Auto-create with empty schema.
        self.create_index(name, Schema::empty())?;
        self.get_index(name)
    }

    /// V4 M5.2 — route a document to the node that owns its shard.
    ///
    /// Returns `Ok(None)` when the doc belongs to this node (handle it
    /// locally via the existing `Index::index_document_with_version`
    /// path). Returns `Ok(Some(node_id))` when the doc belongs to a
    /// peer and must be forwarded via the cluster transport.
    ///
    /// In single-node mode (`num_shards == 1` and the router has no
    /// assignments) this always resolves to "local" and the caller
    /// takes the existing fast path — zero overhead for single-node
    /// deployments.
    pub fn route_write(&self, index: &str, doc_id: &str) -> Option<String> {
        let router = self.shard_router.read();
        let (_shard, owner) = router.route_doc(index, doc_id);
        match owner {
            // No assignment yet — treat as local. This is the
            // single-node default path.
            None => None,
            Some(owner) if owner == self.node_id.as_str() => None,
            Some(owner) => Some(owner.to_string()),
        }
    }

    /// Read-only view of the local node id.
    pub fn local_node_id(&self) -> &str {
        self.node_id.as_str()
    }

    /// List all indices with summary info.
    /// Cheap sync accessor for the set of currently-open index names.
    /// Used by PIT expansion and other handlers that need to iterate
    /// the live index list without paying for the `list_indices`
    /// snapshot or `get_settings()` call.
    pub fn index_name_list(&self) -> Vec<String> {
        self.indices.iter().map(|e| e.key().clone()).collect()
    }

    pub async fn list_indices(&self) -> Vec<IndexInfo> {
        let mut list = Vec::new();
        for entry in self.indices.iter() {
            let stats = entry.value().stats().await;
            list.push(IndexInfo {
                name: stats.name,
                doc_count: stats.doc_count,
                segment_count: stats.segment_count,
                schema_version: stats.schema_version,
            });
        }
        list
    }

    /// Get the stats for a single index.
    pub async fn index_stats(&self, name: &str) -> Result<IndexStats> {
        let idx = self.get_index(name)?;
        Ok(idx.stats().await)
    }

    /// Flush the in-memory memtable for an index to a durable segment on disk.
    ///
    /// After a flush the WAL checkpoint is advanced and old WAL generations are
    /// pruned, so the data survives future restarts without WAL replay.
    pub async fn flush_index(&self, name: &str) -> Result<()> {
        let idx = self.get_index(name)?;
        idx.flush().await
    }

    /// Flush all indices whose memtable exceeds the size threshold.
    ///
    /// Called periodically by the background flush timer.
    pub async fn flush_all_if_needed(&self) {
        for entry in self.indices.iter() {
            let idx = Arc::clone(entry.value());
            if idx.needs_flush().await {
                if let Err(e) = idx.flush().await {
                    tracing::warn!(
                        index = entry.key().as_str(),
                        error = %e,
                        "background flush failed"
                    );
                }
            }
        }
    }

    /// Force-flush every index regardless of memtable size.
    ///
    /// Called from the SIGTERM/SIGINT shutdown hook so that any data still
    /// in the memtable at the moment we stop accepting requests gets a
    /// chance to land on disk as a segment before the process exits.
    /// Without this, anything that was bulk-ingested after the last
    /// auto-flush threshold crossing lives only in the WAL until the next
    /// startup — and if startup index-discovery doesn't pick the index up
    /// (e.g. WAL-only indexes), the data is lost.
    ///
    /// First aborts every index's per-Index merge background task — those
    /// tasks are spawned via `tokio::spawn` and use a `tokio::time::sleep`
    /// loop, which keeps the tokio runtime alive even after axum has
    /// stopped accepting connections.  Without aborting them up-front,
    /// the process stays at 100% CPU until the next sleep wake notices
    /// the index is dropped (or a merge fires post-shutdown — either way
    /// SIGTERM hangs).  See bench `engine/reports/2026-04-25T03-30-00`
    /// for the captured regression introduced by B-2b (commit 605ac7b).
    pub async fn flush_all_force(&self) {
        // 1. Stop all background merges so the runtime can exit once the
        //    flush is done.  Aborts are non-blocking; the spawned task is
        //    unwound by tokio without us needing to await it.
        for entry in self.indices.iter() {
            entry.value().abort_background_tasks();
        }
        // 2. Final synchronous flush across every index.
        for entry in self.indices.iter() {
            let idx = Arc::clone(entry.value());
            if let Err(e) = idx.flush().await {
                tracing::warn!(
                    index = entry.key().as_str(),
                    error = %e,
                    "shutdown flush failed"
                );
            }
        }
    }

    /// Engine health status.
    ///
    /// Returns `"green"` when all indices are fully flushed to durable segments.
    /// Returns `"yellow"` when one or more indices have unflushed memtable data
    /// (data is safe in the WAL but not yet in a segment — a crash would require
    /// WAL replay).
    /// Returns `"red"` when one or more indices failed to open on startup
    /// (tracked in [`failed_indices`]).
    pub async fn health(&self) -> HealthStatus {
        let mut total_docs = 0u64;
        let mut has_memtable_only = false;

        for entry in self.indices.iter() {
            let stats = entry.value().stats().await;
            total_docs += stats.doc_count;
            // Yellow condition: any index has in-memory data that hasn't been
            // flushed to a segment yet.
            if stats.segment_count == 0 && stats.memtable_doc_count > 0 {
                has_memtable_only = true;
            }
        }

        // Red condition: any index directory could not be opened on startup.
        let has_failed = !self.failed_indices.is_empty();

        let status = if has_failed {
            "red"
        } else if has_memtable_only {
            "yellow"
        } else {
            "green"
        };

        HealthStatus {
            status: status.to_string(),
            index_count: self.indices.len(),
            total_docs,
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    // ── Data Stream methods ───────────────────────────────────────────────────

    /// Create a new data stream with its first backing index.
    pub fn create_data_stream(&self, name: &str) -> Result<()> {
        if self.data_streams.contains_key(name) {
            return Err(EngineError::Common(
                xerj_common::XerjError::index_already_exists(name),
            ));
        }
        let backing_name = format!(".ds-{}-000001", name);
        self.create_index(&backing_name, Schema::empty())?;
        // Alias: writing to the stream name → first backing index.
        self.add_alias(name, &backing_name);
        let ds = DataStream {
            name: name.to_string(),
            backing_indices: vec![backing_name],
            timestamp_field: "@timestamp".to_string(),
            generation: 1,
        };
        self.data_streams.insert(name.to_string(), ds);
        info!(name, "data stream created");
        Ok(())
    }

    /// Roll over a data stream: create the next backing index and update the alias.
    pub fn rollover_data_stream(&self, name: &str) -> Result<String> {
        let mut ds = self
            .data_streams
            .get_mut(name)
            .ok_or_else(|| EngineError::Common(xerj_common::XerjError::index_not_found(name)))?;

        ds.generation += 1;
        let new_backing = format!(".ds-{}-{:06}", name, ds.generation);
        drop(ds); // release borrow before calling create_index

        self.create_index(&new_backing, Schema::empty())?;
        // Update alias to point at the new (write) backing index.
        // Keep old backing indices accessible for reads via the alias list.
        if let Some(mut entry) = self.aliases.get_mut(name) {
            if !entry.contains(&new_backing) {
                entry.push(new_backing.clone());
            }
        } else {
            self.add_alias(name, &new_backing);
        }

        if let Some(mut ds) = self.data_streams.get_mut(name) {
            ds.backing_indices.push(new_backing.clone());
        }

        info!(
            name,
            new_backing = new_backing.as_str(),
            "data stream rolled over"
        );
        Ok(new_backing)
    }

    /// Delete a data stream and all its backing indices.
    pub async fn delete_data_stream(&self, name: &str) -> Result<()> {
        let ds = self
            .data_streams
            .remove(name)
            .map(|(_, v)| v)
            .ok_or_else(|| EngineError::Common(xerj_common::XerjError::index_not_found(name)))?;

        // Remove the alias.
        self.aliases.remove(name);

        // Delete every backing index.
        for backing in &ds.backing_indices {
            if let Ok(idx) = self.indices.remove(backing).map(|(_, v)| v).ok_or(()) {
                let _ = idx.delete_all_data().await;
            }
        }
        info!(name, "data stream deleted");
        Ok(())
    }

    /// Return a reference to the engine configuration.
    ///
    /// Useful for handlers that need to read turbo-mode settings without
    /// coupling to the full engine internals.
    pub fn config(&self) -> &Config {
        &self.config
    }

    // ── Transform pipeline methods ────────────────────────────────────────────

    /// Compile and register a typed transform pipeline from a JSON config.
    ///
    /// `config_json` must be a valid [`PipelineConfig`](xerj_wasm::pipeline::PipelineConfig)
    /// object.  The compiled pipeline is stored in `transform_pipelines` and
    /// can be retrieved by name for use at ingest time.
    ///
    /// The raw JSON is also stored in `pipelines` so it can be returned by the
    /// ES-compatible ingest pipeline API.
    pub fn create_pipeline(
        &self,
        name: &str,
        config_json: Value,
    ) -> std::result::Result<(), xerj_wasm::WasmError> {
        let cfg: xerj_wasm::pipeline::PipelineConfig =
            serde_json::from_value(config_json.clone()).map_err(xerj_wasm::WasmError::Json)?;
        let pipeline = xerj_wasm::pipeline::Pipeline::from_config(name, &cfg)?;
        self.pipelines.insert(name.to_string(), config_json);
        self.transform_pipelines.insert(name.to_string(), pipeline);
        info!(name, "transform pipeline created");
        Ok(())
    }

    /// Run `docs` through a named pipeline, returning `(action, doc)` pairs.
    ///
    /// Returns [`xerj_wasm::WasmError::PipelineNotFound`] when `pipeline_name`
    /// does not exist.  Documents with a [`ProcessAction::Drop`] action are
    /// still returned in the output — callers decide whether to skip indexing.
    pub fn process_through_pipeline(
        &self,
        pipeline_name: &str,
        mut docs: Vec<Value>,
    ) -> std::result::Result<Vec<(xerj_wasm::pipeline::ProcessAction, Value)>, xerj_wasm::WasmError>
    {
        let pipeline = self
            .transform_pipelines
            .get(pipeline_name)
            .ok_or_else(|| xerj_wasm::WasmError::PipelineNotFound(pipeline_name.to_string()))?;

        let actions = pipeline.process_batch(&mut docs);
        Ok(actions.into_iter().zip(docs).collect())
    }

    // ── Snapshot / Restore ────────────────────────────────────────────────────

    /// Create a filesystem snapshot of all (or named) indices.
    ///
    /// For each index this copies:
    /// - WAL files  (`<index>/wal/`)
    /// - Segment files (`<index>/segments/`)
    /// - Schema and settings JSON files
    ///
    /// A `manifest.json` is written at the snapshot root listing every index
    /// and its files so that `restore_snapshot` can replay them.
    pub async fn create_snapshot(
        &self,
        repo_path: &str,
        name: &str,
        indices: Option<Vec<String>>,
    ) -> Result<Value> {
        let snap_dir = std::path::Path::new(repo_path).join(name);
        std::fs::create_dir_all(&snap_dir).map_err(EngineError::Io)?;

        let target_indices: Vec<String> = match indices {
            Some(list) if !list.is_empty() => list,
            _ => self.indices.iter().map(|e| e.key().clone()).collect(),
        };

        let mut manifest_indices: Vec<Value> = Vec::new();

        for idx_name in &target_indices {
            let idx = match self.indices.get(idx_name.as_str()) {
                Some(i) => i,
                None => continue,
            };

            // Flush memtable so all data is on disk before copying.
            let _ = idx.flush().await;

            let src_dir = idx.data_dir().to_path_buf();
            let dst_dir = snap_dir.join(idx_name);
            std::fs::create_dir_all(&dst_dir).map_err(EngineError::Io)?;

            let mut files: Vec<String> = Vec::new();

            // Copy everything recursively (WAL + segments + schema).
            copy_dir_recursive(&src_dir, &dst_dir, &mut files).map_err(EngineError::Io)?;

            manifest_indices.push(serde_json::json!({
                "name": idx_name,
                "files": files,
            }));
        }

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let manifest = serde_json::json!({
            "snapshot": name,
            "uuid": uuid::Uuid::new_v4().to_string(),
            "version": "8.13.0",
            "indices": target_indices,
            "state": "SUCCESS",
            "start_time_in_millis": now_ms,
            "end_time_in_millis": now_ms,
            "duration_in_millis": 0,
            "failures": [],
            "shards": {
                "total": target_indices.len(),
                "failed": 0,
                "successful": target_indices.len(),
            },
            "index_files": manifest_indices,
        });

        let manifest_path = snap_dir.join("manifest.json");
        let bytes = serde_json::to_vec_pretty(&manifest).map_err(EngineError::Serde)?;
        std::fs::write(&manifest_path, bytes).map_err(EngineError::Io)?;

        info!(snapshot = name, repo = repo_path, "snapshot created");
        Ok(manifest)
    }

    /// Restore a snapshot: copies files back and reopens the indices.
    pub async fn restore_snapshot(&self, repo_path: &str, name: &str) -> Result<()> {
        let snap_dir = std::path::Path::new(repo_path).join(name);
        let manifest_path = snap_dir.join("manifest.json");

        let manifest_bytes = std::fs::read(&manifest_path).map_err(EngineError::Io)?;
        let manifest: Value =
            serde_json::from_slice(&manifest_bytes).map_err(EngineError::Serde)?;

        let index_names: Vec<String> = manifest
            .get("indices")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default();

        for idx_name in &index_names {
            let src_dir = snap_dir.join(idx_name.as_str());
            if !src_dir.exists() {
                warn!(index = idx_name, "snapshot directory missing, skipping");
                continue;
            }

            let dst_dir = self.data_dir.join(idx_name);

            // Remove existing index data (if any) and close it.
            if self.indices.contains_key(idx_name.as_str()) {
                self.indices.remove(idx_name.as_str());
            }
            if dst_dir.exists() {
                std::fs::remove_dir_all(&dst_dir).map_err(EngineError::Io)?;
            }
            std::fs::create_dir_all(&dst_dir).map_err(EngineError::Io)?;

            // Copy snapshot files back.
            let mut _files: Vec<String> = Vec::new();
            copy_dir_recursive(&src_dir, &dst_dir, &mut _files).map_err(EngineError::Io)?;

            // Reopen the index.
            let index_name = IndexName::new(idx_name).map_err(EngineError::Common)?;
            match Index::open(index_name, &self.config, &self.data_dir) {
                Ok(idx) => {
                    // Snapshot dirs carry es_mapping.json — reload it so the
                    // restored index serves the same mapping it was saved with.
                    self.load_persisted_es_mapping(idx_name);
                    self.indices.insert(idx_name.clone(), idx);
                    info!(index = idx_name, "index restored from snapshot");
                }
                Err(e) => {
                    warn!(index = idx_name, error = %e, "failed to reopen restored index");
                    self.failed_indices.insert(idx_name.clone(), e.to_string());
                }
            }
        }

        info!(snapshot = name, repo = repo_path, "snapshot restored");
        Ok(())
    }
} // end impl Engine

// ── Private helpers ───────────────────────────────────────────────────────────

/// Recursively copy all files from `src` to `dst`, recording relative paths in `files`.
fn copy_dir_recursive(
    src: &std::path::Path,
    dst: &std::path::Path,
    files: &mut Vec<String>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let file_name = entry.file_name();
        let dst_path = dst.join(&file_name);

        if src_path.is_dir() {
            std::fs::create_dir_all(&dst_path)?;
            copy_dir_recursive(&src_path, &dst_path, files)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
            files.push(file_name.to_string_lossy().to_string());
        }
    }
    Ok(())
}

/// Simple glob pattern matching (supports `*` and `?`).
fn glob_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    let (m, n) = (pat.len(), txt.len());
    let mut dp = vec![vec![false; n + 1]; m + 1];
    dp[0][0] = true;
    for i in 1..=m {
        if pat[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }
    for i in 1..=m {
        for j in 1..=n {
            if pat[i - 1] == '*' {
                dp[i][j] = dp[i - 1][j] || dp[i][j - 1];
            } else if pat[i - 1] == '?' || pat[i - 1] == txt[j - 1] {
                dp[i][j] = dp[i - 1][j - 1];
            }
        }
    }
    dp[m][n]
}

/// Convert an ES field type string to a native FieldType.
fn es_type_to_field_type(es_type: &str) -> xerj_common::types::FieldType {
    use xerj_common::types::FieldType;
    match es_type {
        "text" => FieldType::Text,
        "keyword" | "constant_keyword" | "wildcard" => FieldType::Keyword,
        "long" | "integer" | "short" | "byte" | "unsigned_long" => FieldType::Long,
        "double" | "float" | "half_float" | "scaled_float" => FieldType::Double,
        "boolean" => FieldType::Boolean,
        "date" | "date_nanos" => FieldType::Date,
        "ip" => FieldType::Ip,
        "dense_vector" => FieldType::Vector,
        "geo_point" => FieldType::GeoPoint,
        "binary" => FieldType::Binary,
        "nested" => FieldType::Nested,
        _ => FieldType::Object,
    }
}
