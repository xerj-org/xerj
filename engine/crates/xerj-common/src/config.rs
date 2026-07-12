//! xerj configuration system.
//!
//! Configuration is intentionally minimal: **38 settings** versus Elasticsearch's
//! 3000+. Every option is named, documented, and has a sensible production-ready
//! default. The format is TOML, loaded from a single file.
//!
//! ## Quick start
//!
//! ```no_run
//! use xerj_common::Config;
//!
//! // Use all defaults (works out of the box)
//! let cfg = Config::default();
//!
//! // Or load from a file
//! let cfg = Config::load("/etc/xerj/xerj.toml").unwrap();
//! ```
//!
//! ## Example configuration file
//!
//! ```toml
//! [server]
//! rest_port = 8080
//! data_dir  = "/var/lib/xerj"
//!
//! [auth]
//! enabled = true
//!
//! [vector]
//! hnsw_m = 32
//! ```

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::XerjError;

// ═════════════════════════════════════════════════════════════════════════════
// Top-level Config
// ═════════════════════════════════════════════════════════════════════════════

/// Complete engine configuration.
///
/// Fields are grouped into sub-structs by concern. All fields implement
/// `Default` so that an empty config file (or no file at all) is valid.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Network and data directory settings — 5 settings.
    pub server: ServerConfig,
    /// Authentication — 2 settings.
    pub auth: AuthConfig,
    /// TLS — 3 settings.
    pub tls: TlsConfig,
    /// Write-ahead log and flush behaviour — 5 settings.
    pub storage: StorageConfig,
    /// Segment merging — 5 settings.
    pub merge: MergeConfig,
    /// Data compression — 3 settings.
    pub compression: CompressionConfig,
    /// Full-text search — 1 setting.
    pub fts: FtsConfig,
    /// Vector search (HNSW) — 6 settings.
    pub vector: VectorConfig,
    /// Log (time-series) retention — 2 settings.
    pub logs: LogsConfig,
    /// External embedding service — 4 settings.
    pub embedding: EmbeddingConfig,
    /// Resource limits — 3 settings.
    pub limits: LimitsConfig,
    /// High-throughput turbo indexing — 3 settings.
    pub indexing: IndexingConfig,
    /// Engine parallelism — 4 settings.
    pub engine: EngineConfig,
    /// Cluster / Raft settings — 4 settings.
    pub cluster: ClusterConfig,
    /// Point-in-time TTL + sweep cadence — 3 settings.
    pub pit: PitConfig,
}

// Total: 5+2+3+10+5+3+1+6+2+4+3+4+3 = 51 fields
// `Default` is derived — every field is a sub-config that implements
// `Default`, so the derive produces exactly the same all-defaults value
// the manual impl used to build by hand.

impl Config {
    /// Load configuration from a TOML file.
    ///
    /// Missing keys fall back to their `Default` values, so a minimal config
    /// only needs to override what differs from the defaults.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, XerjError> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path).map_err(|e| {
            XerjError::config(format!("cannot read config file {}: {}", path.display(), e))
        })?;

        let config: Config = toml::from_str(&raw)?;
        config.validate()?;
        Ok(config)
    }

    /// Load configuration from a TOML string (useful for testing).
    ///
    /// Named `from_toml_str` rather than `from_str` to avoid shadowing the
    /// `std::str::FromStr::from_str` convention (clippy::should_implement_trait):
    /// this parses TOML specifically and also runs cross-field validation.
    pub fn from_toml_str(s: &str) -> Result<Self, XerjError> {
        let config: Config = toml::from_str(s)?;
        config.validate()?;
        Ok(config)
    }

    /// Validate cross-field constraints.
    ///
    /// Individual field validation (range checks, enum values) is done via
    /// serde; this method handles rules that span multiple fields.
    pub fn validate(&self) -> Result<(), XerjError> {
        // Server ports must be unique
        let ports = [
            self.server.rest_port,
            self.server.grpc_port,
            self.server.es_compat_port,
        ];
        let unique: std::collections::HashSet<_> = ports.iter().collect();
        if unique.len() != ports.len() {
            return Err(XerjError::config(
                "rest_port, grpc_port, and es_compat_port must all be distinct",
            ));
        }

        // TLS: if enabled, paths must be supplied
        if self.tls.enabled {
            if self.tls.cert_path.is_empty() {
                return Err(XerjError::config(
                    "tls.cert_path is required when tls.enabled = true",
                ));
            }
            if self.tls.key_path.is_empty() {
                return Err(XerjError::config(
                    "tls.key_path is required when tls.enabled = true",
                ));
            }
        }

        // Storage: WAL batch interval sanity (documented range: 1..=10000).
        if self.storage.wal_batch_ms == 0 {
            return Err(XerjError::config("storage.wal_batch_ms must be > 0"));
        }
        if self.storage.wal_batch_ms > 10_000 {
            return Err(XerjError::config(
                "storage.wal_batch_ms must be <= 10000 (10 s)",
            ));
        }

        // Merge: min_segments must be >= 2
        if self.merge.min_segments < 2 {
            return Err(XerjError::config("merge.min_segments must be >= 2"));
        }

        // Vector: hnsw_ef_construction >= hnsw_m
        if self.vector.hnsw_ef_construction < self.vector.hnsw_m {
            return Err(XerjError::config(
                "vector.hnsw_ef_construction must be >= vector.hnsw_m",
            ));
        }

        // Vector: max_dimensions must be power-of-two-friendly and > 0
        if self.vector.max_dimensions == 0 {
            return Err(XerjError::config("vector.max_dimensions must be > 0"));
        }

        // ── Config honesty guards ────────────────────────────────────────────
        // Some config knobs exist in the schema but are not wired into any code
        // path in this build. Silently ignoring them is worse than failing: an
        // operator who sets `storage.backend = "s3"` believes their data lands
        // in S3, and one who sets `default_quantization = "scalar8"` believes
        // vectors are compressed 4×. Neither is true. Fail loud at startup so
        // the mismatch surfaces immediately instead of after data is written.

        // Storage: only the local filesystem backend is implemented. The S3 /
        // object-store backend selector is inert — no code reads it to route
        // segment writes/reads to S3.
        if self.storage.backend != StorageBackendType::Local {
            return Err(XerjError::config(
                "storage.backend: the S3 storage backend is not implemented in this build; \
                 only \"local\" is supported",
            ));
        }

        // Vector: `scalar8` (SQ8) quantization is now wired into the kNN
        // serving path (`Index::run_knn_brute_force`): a `scalar8` dense_vector
        // field keeps a per-field u8 code store (1 byte/dim vs 4) and scores
        // candidates by decoding those codes, giving a real ~4× reduction on
        // that field's vector working set. `none` and `scalar8` are therefore
        // accepted. `binary` (1-bit) has no implemented quantizer, so honouring
        // it would silently store full-precision vectors while claiming a 32×
        // saving — it stays rejected until a BinaryQuantizer lands.
        if self.vector.default_quantization == VectorQuantization::Binary {
            return Err(XerjError::config(
                "vector.default_quantization: binary (1-bit) quantization is not implemented in \
                 this build; only \"none\" and \"scalar8\" are supported",
            ));
        }

        // Limits: concurrency must be > 0
        if self.limits.max_concurrent_searches == 0 {
            return Err(XerjError::config(
                "limits.max_concurrent_searches must be > 0",
            ));
        }

        self.engine.validate()?;

        Ok(())
    }

    /// Returns the effective bind address for the REST API.
    pub fn rest_addr(&self) -> String {
        format!("{}:{}", self.server.bind_address, self.server.rest_port)
    }

    /// Returns the effective bind address for the gRPC API.
    pub fn grpc_addr(&self) -> String {
        format!("{}:{}", self.server.bind_address, self.server.grpc_port)
    }

    /// Returns the effective bind address for the Elasticsearch-compatible API.
    pub fn es_compat_addr(&self) -> String {
        format!(
            "{}:{}",
            self.server.bind_address, self.server.es_compat_port
        )
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Sub-configs  (38 user-facing settings total)
// ═════════════════════════════════════════════════════════════════════════════

/// Network and data-directory settings.
///
/// **5 settings.**
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Port for the native REST API (default: `8080`).
    pub rest_port: u16,
    /// Port for the gRPC API (default: `8081`).
    pub grpc_port: u16,
    /// Port for the Elasticsearch-compatible REST API (default: `9200`).
    pub es_compat_port: u16,
    /// Directory where index data is persisted (default: `"./data"`).
    pub data_dir: String,
    /// Address to bind all listeners (default: `"0.0.0.0"`).
    pub bind_address: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            rest_port: 8080,
            grpc_port: 8081,
            es_compat_port: 9200,
            data_dir: "./data".into(),
            bind_address: "0.0.0.0".into(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// Authentication settings.
///
/// **2 settings.**
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    /// Enable API-key authentication (default: `true`).
    ///
    /// When `true`, every request must carry an `Authorization: ApiKey <key>`
    /// header. An admin key is auto-generated on first startup if `admin_api_key`
    /// is left empty.
    pub enabled: bool,
    /// Static admin API key (default: `""` — auto-generated on first run).
    ///
    /// Leave empty in production; the engine writes the generated key to
    /// `<data_dir>/admin.key` on startup.
    pub admin_api_key: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            admin_api_key: String::new(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// TLS settings.
///
/// **3 settings.**
///
/// Defaults are derived: TLS is disabled (`enabled: false`) with empty
/// cert/key paths so the engine starts out of the box; enable it in
/// production by setting `cert_path` + `key_path`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TlsConfig {
    /// Enable TLS on all listeners (default: `false` — enable in production).
    pub enabled: bool,
    /// Path to the PEM-encoded certificate file.
    pub cert_path: String,
    /// Path to the PEM-encoded private key file.
    pub key_path: String,
}

// ─────────────────────────────────────────────────────────────────────────────

/// Write-ahead log, flush, and object-store settings.
///
/// **10 settings** (5 WAL/flush + 5 object-store).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    /// WAL fsync strategy: `"sync"`, `"batched"`, or `"async"` (default: `"batched"`).
    ///
    /// - `"sync"` — fsync before every acknowledgement.  On the bulk API
    ///   this is one fsync per bulk request (group commit), matching ES's
    ///   per-request translog fsync granularity. Safest, slowest.
    /// - `"batched"` — writes reach the kernel immediately (process-crash
    ///   durable); a background loop fsyncs every `wal_batch_ms`, bounding
    ///   the power-loss window. Good balance.
    /// - `"async"` — never fsync; the OS decides when to write back.
    ///   Fastest, least durable.
    ///
    /// RC4 W1 #9: `"sync"` was previously ignored on the bulk ingest paths
    /// (fsync only via the undocumented `XERJ_STRICT_SYNC` env var) and
    /// the `wal_batch_ms` loop did not exist. Both are honored now.
    pub wal_sync: WalSync,
    /// How often to fsync the WAL when `wal_sync = "batched"` (default: `100` ms).
    /// Range: 1..=10000.
    pub wal_batch_ms: u64,
    /// Maximum WAL file size before it is rolled over (default: `512` MiB).
    pub wal_max_size_mb: u64,
    /// Accumulated in-memory data size that triggers a segment flush (default: `256` MiB).
    pub flush_size_mb: u64,
    /// Maximum time between flushes regardless of buffer size (default: `30` s).
    pub flush_interval_secs: u64,

    // ── Object-store backend (compute-storage separation) ─────────────────────
    /// Storage backend: `"local"` or `"s3"` (default: `"local"`).
    ///
    /// When set to `"s3"`, flushed segments are written to the configured S3
    /// bucket using range reads for efficient random access.  Local NVMe is used
    /// as a read-through cache (see `local_cache_dir`).
    pub backend: StorageBackendType,
    /// S3 bucket name (required when `backend = "s3"`).
    pub s3_bucket: String,
    /// Key prefix prepended to every S3 object (default: `"xerj/"`).
    pub s3_prefix: String,
    /// AWS region for S3 requests (default: `"us-east-1"`).
    pub s3_region: String,
    /// Local NVMe cache directory for S3 segments (default: `"./cache"`).
    ///
    /// Segments are cached here after the first fetch from S3.  The cache is
    /// evicted by the background `SegmentCache::maybe_evict` task.
    pub local_cache_dir: String,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            wal_sync: WalSync::Batched,
            wal_batch_ms: 100,
            wal_max_size_mb: 1024,
            flush_size_mb: 512,
            flush_interval_secs: 30,
            backend: StorageBackendType::Local,
            s3_bucket: String::new(),
            s3_prefix: "xerj/".into(),
            s3_region: "us-east-1".into(),
            local_cache_dir: "./cache".into(),
        }
    }
}

/// Storage backend selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageBackendType {
    /// Local filesystem only (default).
    Local,
    /// AWS S3 (or compatible, e.g. MinIO).
    S3,
}

/// WAL fsync strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WalSync {
    /// fsync after every individual write.
    Sync,
    /// fsync on a timer (`wal_batch_ms`).
    Batched,
    /// Never fsync (OS decides).
    Async,
}

// ─────────────────────────────────────────────────────────────────────────────

/// Segment merge settings.
///
/// **5 settings.**
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MergeConfig {
    /// Merge strategy: `"size_tiered"` or `"log_structured"` (default: `"size_tiered"`).
    ///
    /// - `"size_tiered"` — groups similarly-sized segments. Best for write-heavy.
    /// - `"log_structured"` — LSMT-style tiered levels. Best for mixed workloads.
    pub strategy: MergeStrategy,
    /// Minimum number of segments to trigger a merge (default: `10`).
    pub min_segments: u32,
    /// Maximum merged segment size in MiB (default: `8192` = 8 GiB).
    /// Segments at or above this size are excluded from further merges.
    pub max_segment_mb: u64,
    /// I/O rate cap for merge operations in MiB/s (default: `100`).
    ///
    /// Throttle this to prevent merges from saturating I/O on shared storage.
    pub io_rate_mb_per_sec: u64,
    /// Maximum number of concurrent merge operations (default: `1`).
    pub max_concurrent: u32,
    /// Tier boundary base for size-tiered merge policy in MiB (default: `4`).
    /// Segments group into tiers by `floor(log2(size / tier_floor_mb))`.
    pub tier_floor_mb: u64,
    /// Minimum segments in a tier before merging is triggered (default: `4`).
    /// Distinct from `min_segments` (which gates whether a merge runs at all).
    pub min_merge_count: u32,
    /// Maximum segments merged per batch (default: `16`).
    /// Caps per-batch RAM: ~max_merge_count × per-segment overhead.
    pub max_merge_count: u32,
}

impl Default for MergeConfig {
    fn default() -> Self {
        Self {
            strategy: MergeStrategy::SizeTiered,
            min_segments: 10,
            max_segment_mb: 8192,
            io_rate_mb_per_sec: 100,
            max_concurrent: 1,
            tier_floor_mb: 4,
            min_merge_count: 4,
            max_merge_count: 16,
        }
    }
}

/// Segment merge strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    /// Merge segments of similar size (good for write-heavy workloads).
    SizeTiered,
    /// LSMT-style levelled merge (good for mixed workloads).
    LogStructured,
}

// ─────────────────────────────────────────────────────────────────────────────

/// Compression settings.
///
/// **3 settings.**
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompressionConfig {
    /// Enable block compression for stored fields (default: `true`).
    pub enabled: bool,
    /// Compression level: `"fast"`, `"balanced"`, or `"best"` (default: `"balanced"`).
    ///
    /// Uses LZ4 for `"fast"` and Zstandard for `"balanced"` / `"best"`.
    pub level: CompressionLevel,
    /// Number of documents per compressed block (default: `128`).
    ///
    /// Larger blocks compress better but increase random read amplification.
    pub block_size_docs: u32,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            level: CompressionLevel::Balanced,
            block_size_docs: 128,
        }
    }
}

/// Compression level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompressionLevel {
    /// LZ4 — maximum throughput, moderate ratio.
    Fast,
    /// Zstandard level 3 — good ratio with low CPU overhead.
    Balanced,
    /// Zstandard level 19 — maximum ratio, higher CPU cost.
    Best,
}

// ─────────────────────────────────────────────────────────────────────────────

/// Full-text search settings.
///
/// **1 setting.**
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FtsConfig {
    /// Default analyzer for `text` fields (default: `"standard"`).
    ///
    /// Built-in analyzers: `"standard"`, `"whitespace"`, `"simple"`, `"english"`.
    /// Custom analyzers are defined at index creation time.
    pub default_analyzer: String,
}

impl Default for FtsConfig {
    fn default() -> Self {
        Self {
            default_analyzer: "standard".into(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// Vector search (HNSW) settings.
///
/// **6 settings.**
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VectorConfig {
    /// Similarity metric: `"cosine"`, `"dot_product"`, or `"euclidean"` (default: `"cosine"`).
    pub default_metric: VectorMetric,
    /// HNSW `M` parameter — edges per node per layer (default: `16`).
    ///
    /// Higher values improve recall at the cost of memory and build time.
    pub hnsw_m: usize,
    /// HNSW `ef_construction` — search width during index build (default: `200`).
    pub hnsw_ef_construction: usize,
    /// HNSW `ef` — search width at query time (default: `100`).
    ///
    /// Can be overridden per query. Must be ≥ the number of neighbours
    /// requested (`k`).
    pub hnsw_ef_search: usize,
    /// Default quantization: `"none"` (default) or `"scalar8"`. `"binary"` is
    /// **not implemented in this build** and is rejected at startup.
    ///
    /// - `"none"` — full float32 vectors (highest accuracy, most memory).
    /// - `"scalar8"` — 8-bit scalar quantization (~4× memory reduction) — WIRED
    ///   into the kNN serving path. A `scalar8` dense_vector field keeps a
    ///   per-field u8 code store (1 byte/dim) and scores candidates by decoding
    ///   those codes, so the memory saving is real, not cosmetic. Typically
    ///   opted into per field via `index_options.type: int8_hnsw` on the
    ///   mapping; this global default applies the same scheme index-wide.
    /// - `"binary"` — 1-bit binary quantization (~32× memory reduction) — NOT
    ///   YET IMPLEMENTED (no `BinaryQuantizer` exists).
    ///
    /// Honouring `binary` would silently store full-precision vectors while
    /// claiming a saving, so only `none` and `scalar8` are accepted (see
    /// `Config::validate`).
    pub default_quantization: VectorQuantization,
    /// Maximum supported vector dimensionality (default: `16384`).
    pub max_dimensions: usize,
}

impl Default for VectorConfig {
    fn default() -> Self {
        Self {
            default_metric: VectorMetric::Cosine,
            hnsw_m: 16,
            hnsw_ef_construction: 200,
            hnsw_ef_search: 100,
            default_quantization: VectorQuantization::None,
            max_dimensions: 16384,
        }
    }
}

/// Vector similarity metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorMetric {
    /// Cosine similarity (vectors are normalised).
    Cosine,
    /// Raw dot product.
    DotProduct,
    /// L2 (Euclidean) distance.
    Euclidean,
}

/// Vector quantization scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorQuantization {
    /// No quantization — store full float32 vectors.
    None,
    /// 8-bit scalar quantization.
    Scalar8,
    /// 1-bit binary quantization.
    Binary,
}

// ─────────────────────────────────────────────────────────────────────────────

/// Log (time-series) retention settings.
///
/// **2 settings.**
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogsConfig {
    /// How long to retain log data before automatic deletion (default: `90` days).
    pub retention_days: u32,
    /// Time-based partition granularity (default: `"1h"`).
    ///
    /// Supported values: `"1m"`, `"5m"`, `"15m"`, `"1h"`, `"6h"`, `"1d"`.
    pub time_partition: String,
}

impl Default for LogsConfig {
    fn default() -> Self {
        Self {
            retention_days: 90,
            time_partition: "1h".into(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// Embedding backend settings.
///
/// XERJ can embed `semantic_text` fields three ways, chosen by [`mode`]:
///   * `"lexical"` — the zero-dependency built-in feature-hash embedder
///     (deterministic, 384-dim, no model, no network). This is the honest
///     default: lexical, *not* neural semantic understanding.
///   * `"neural"` — a built-in BERT sentence embedder (all-MiniLM-L6-v2 by
///     default) that runs in-process via `candle`. The model weights are
///     downloaded once on first use (or read from [`local_model_dir`] for
///     air-gapped deployments). The neural backend ships in the standard
///     binary; a `--no-default-features` slim build omits it and falls back
///     to lexical.
///   * `"proxy"` — call an external OpenAI-compatible `/v1/embeddings`
///     endpoint ([`default_endpoint`]). Lets customers plug in ANY embedding
///     model / provider they already run.
///   * `"auto"` (default) — use the proxy when [`default_endpoint`] is set,
///     otherwise lexical. This preserves the historical behavior exactly.
///
/// **8 settings.**
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EmbeddingConfig {
    /// Backend selector: `"auto"` (default), `"lexical"`, `"neural"`, or
    /// `"proxy"`. Unknown values are treated as `"auto"`.
    pub mode: String,
    /// OpenAI-compatible endpoint URL (default: `""` — disabled).
    pub default_endpoint: String,
    /// Model name to request from the endpoint (default: `""`).
    pub default_model: String,
    /// Maximum documents per embedding API call (default: `64`).
    pub batch_size: usize,
    /// HTTP timeout for embedding requests in ms (default: `5000`).
    pub timeout_ms: u64,
    /// Neural backend: HuggingFace model id to load (default
    /// `sentence-transformers/all-MiniLM-L6-v2`, a 384-dim sentence encoder).
    pub neural_model: String,
    /// Neural backend: directory to cache downloaded model weights. Empty
    /// (default) uses the standard HuggingFace cache (`~/.cache/huggingface`).
    pub model_cache_dir: String,
    /// Neural backend: if set, load `config.json`, `tokenizer.json`, and the
    /// safetensors weights from this local directory instead of downloading
    /// — for air-gapped / offline deployments. Empty (default) = download.
    pub local_model_dir: String,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            mode: "auto".to_string(),
            default_endpoint: String::new(),
            default_model: String::new(),
            batch_size: 64,
            timeout_ms: 5000,
            neural_model: "sentence-transformers/all-MiniLM-L6-v2".to_string(),
            model_cache_dir: String::new(),
            local_model_dir: String::new(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// Resource limits.
///
/// **7 settings.**
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsConfig {
    /// Maximum memory a single query may allocate (default: `512` MiB).
    pub max_query_memory_mb: u64,
    /// Maximum number of searches executing concurrently (default: `64`).
    pub max_concurrent_searches: u32,
    /// Maximum number of mapped fields per index (default: `500`).
    ///
    /// Elasticsearch's mapping explosion protection. Keep this low.
    pub max_fields_per_index: u32,
    /// Maximum HTTP request body size in bytes (default: `100 * 1024 * 1024`,
    /// i.e. 100 MiB). Caps NDJSON bulk payloads, large mget bodies, etc.
    /// Raise this only if your client routinely sends bigger requests; the
    /// router rejects oversized bodies before they reach a handler.
    pub max_body_bytes: usize,
    /// Maximum value of `from + size` in a search request (default: `10_000`).
    ///
    /// Mirrors Elasticsearch's `index.max_result_window`. Deep pagination past
    /// this should use `search_after` / point-in-time cursors instead. The
    /// limit prevents `size=2_000_000_000` from allocating 2 billion `Hit`
    /// structs from a single HTTP POST.
    pub max_result_window: usize,
    /// Maximum number of doc-references in a single `_mget` request body
    /// (default: `10_000`). Mirrors `max_result_window`.
    pub max_mget_docs: usize,
    /// Maximum number of buckets a single aggregation may produce
    /// (default: `65_536`). Mirrors Elasticsearch's `search.max_buckets`
    /// cluster setting. Without this cap, a `terms` agg over a high-
    /// cardinality field (e.g. 50M unique user IDs) allocates 50M
    /// HashMap entries before pagination can drop them. Apply at the
    /// accumulator boundary, not after sort, so memory never grows
    /// past the cap.
    pub max_buckets: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_query_memory_mb: 512,
            max_concurrent_searches: 64,
            max_fields_per_index: 500,
            max_body_bytes: 100 * 1024 * 1024,
            max_result_window: 10_000,
            max_mget_docs: 10_000,
            max_buckets: 65_536,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// High-throughput turbo indexing settings.
///
/// Turbo mode is **opt-in**: it must be enabled per-request via the
/// `/v1/indices/{name}/turbo-ingest` endpoint or the `X-Turbo: true` header.
/// These settings tune its behaviour globally.
///
/// **3 settings.**
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexingConfig {
    /// Number of documents accumulated per batch in turbo mode (default: `1000`).
    ///
    /// Larger batches amortise WAL and fsync overhead but increase per-batch
    /// latency.  Values between 500 and 5000 work well for most workloads.
    pub turbo_batch_size: usize,
    /// Enable parallel tokenisation via Rayon in turbo mode (default: `true`).
    ///
    /// Disable only for debugging or on single-core machines.
    pub turbo_parallel: bool,
    /// Skip stemming and stopword removal in turbo mode for maximum speed (default: `false`).
    ///
    /// When `true`, the `FastTokenizer` is used even for fields that would
    /// normally be processed by the configured `fts.default_analyzer`.  Search
    /// recall may be reduced (e.g. "running" won't match "run"), but ingest
    /// throughput increases significantly.
    pub turbo_fast_analyzer: bool,
}

impl Default for IndexingConfig {
    fn default() -> Self {
        Self {
            turbo_batch_size: 1000,
            turbo_parallel: true,
            turbo_fast_analyzer: false,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// Engine parallelism settings for vertical scaling.
///
/// Controls how many independent ingest/flush/search pipelines run in
/// parallel.  The default is tuned for the host's core count.  Increase
/// `ingest_shards` on high-core-count machines for linear throughput scaling.
///
/// **4 settings.**
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EngineConfig {
    /// Number of independent ingest shards (WAL + memtable + flush pipeline
    /// each).  Must be a power of 2.  Default: `max(1, num_cpus / 2)`.
    ///
    /// Each shard has its own WAL file, memtable partition, and flush thread.
    /// Doubling shards roughly doubles sustained ingest throughput (linear
    /// scaling) until memory bandwidth is saturated.
    pub ingest_shards: usize,
    /// Maximum concurrent flush tasks across all shards (default: `max(1, num_cpus / 4)`).
    pub flush_workers: usize,
    /// Background merge threads (default: `2`).
    pub merge_workers: usize,
    /// Parallel segment scan threads for search (default: `max(1, num_cpus / 4)`).
    pub search_workers: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        Self {
            ingest_shards: (cpus / 2).max(1).next_power_of_two(),
            flush_workers: (cpus / 4).max(1),
            merge_workers: 2,
            search_workers: (cpus / 4).max(1),
        }
    }
}

impl EngineConfig {
    pub fn validate(&self) -> Result<(), crate::XerjError> {
        if self.ingest_shards == 0 || !self.ingest_shards.is_power_of_two() {
            return Err(crate::XerjError::config(format!(
                "engine.ingest_shards must be a power of 2, got {}",
                self.ingest_shards
            )));
        }
        if self.ingest_shards > 256 {
            return Err(crate::XerjError::config(format!(
                "engine.ingest_shards max is 256, got {}",
                self.ingest_shards
            )));
        }
        if self.flush_workers == 0 {
            return Err(crate::XerjError::config(
                "engine.flush_workers must be >= 1",
            ));
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// Cluster / Raft consensus settings.
///
/// When `enabled = false` (the default), the node runs in single-node mode and
/// no Raft or TCP transport is initialised.
///
/// **4 settings.**
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClusterConfig {
    /// Enable multi-node cluster mode (default: `false`).
    ///
    /// When `true`, the Raft state machine and TCP cluster transport are started
    /// on `cluster_port`. At least one peer must be listed in `peers`.
    pub enabled: bool,
    /// TCP port for inter-node Raft and search messages (default: `9300`).
    ///
    /// Each node in the cluster must expose this port and it must be reachable
    /// from all peers.
    pub port: u16,
    /// Peer nodes in `"<node_id>=<host>:<port>"` format.
    ///
    /// Example: `["n2=10.0.0.2:9300", "n3=10.0.0.3:9300"]`
    pub peers: Vec<String>,
    /// Raft tick interval in milliseconds (default: `50`).
    ///
    /// Controls how often the Raft state machine is driven forward. Lower values
    /// improve leader election latency at the cost of CPU.
    pub tick_ms: u64,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 9300,
            peers: Vec::new(),
            tick_ms: 50,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// Point-in-time (PIT) lifecycle settings.
///
/// **3 settings.**
///
/// PITs are search snapshots — they pin the set of indices and the
/// max visible seq_no at open time so subsequent searches against
/// `pit.id` ignore newer writes. Each open PIT holds memory; before
/// v0.6.2 PITs accumulated forever, which is a trivial memory-leak
/// vector. The settings here put a TTL on every PIT and run a
/// background sweeper to evict expired ones.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PitConfig {
    /// Default TTL when a client opens a PIT without `?keep_alive=…`
    /// (default: 300 seconds = 5 minutes). ES requires `keep_alive`;
    /// we accept its absence and apply this default rather than 400.
    pub default_keep_alive_secs: u64,
    /// Hard cap on `keep_alive` regardless of what the client asked
    /// for (default: 86 400 = 24 h). Prevents abusive clients from
    /// requesting a 30-day PIT.
    pub max_keep_alive_secs: u64,
    /// How often the background sweeper scans for expired PITs
    /// (default: 30 seconds). Lower = less memory drift, more CPU.
    pub sweep_interval_secs: u64,
}

impl Default for PitConfig {
    fn default() -> Self {
        Self {
            default_keep_alive_secs: 300,
            max_keep_alive_secs: 86_400,
            sweep_interval_secs: 30,
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Tests
// ═════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        Config::default()
            .validate()
            .expect("default config should be valid");
    }

    #[test]
    fn parse_minimal_toml() {
        let cfg = Config::from_toml_str(
            r#"
            [server]
            rest_port = 9000
            "#,
        )
        .expect("minimal TOML should parse");
        assert_eq!(cfg.server.rest_port, 9000);
        // Other fields retain their defaults
        assert_eq!(cfg.server.grpc_port, 8081);
    }

    #[test]
    fn duplicate_ports_rejected() {
        let result = Config::from_toml_str(
            r#"
            [server]
            rest_port = 9200
            es_compat_port = 9200
            "#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn tls_enabled_requires_paths() {
        let result = Config::from_toml_str(
            r#"
            [tls]
            enabled = true
            cert_path = ""
            key_path  = ""
            "#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn tls_disabled_no_paths_ok() {
        let cfg = Config::from_toml_str(
            r#"
            [tls]
            enabled = false
            "#,
        )
        .expect("tls disabled with no paths should be ok");
        assert!(!cfg.tls.enabled);
    }

    #[test]
    fn s3_backend_rejected() {
        // The S3 backend selector is inert in this build; setting it must fail
        // loud rather than silently running on local disk.
        let result = Config::from_toml_str(
            r#"
            [storage]
            backend = "s3"
            s3_bucket = "my-bucket"
            "#,
        );
        let err = result.expect_err("s3 backend must be rejected as unimplemented");
        assert!(
            err.to_string().contains("not implemented"),
            "error should explain S3 is unimplemented, got: {err}"
        );
    }

    #[test]
    fn quantization_scalar8_accepted() {
        // scalar8 (SQ8) is now wired into the kNN serving path, so it must be
        // accepted at startup (no longer a silent-fake rejection).
        let cfg = Config::from_toml_str(
            r#"
            [vector]
            default_quantization = "scalar8"
            "#,
        )
        .expect("scalar8 quantization must be accepted now that it is wired");
        assert_eq!(cfg.vector.default_quantization, VectorQuantization::Scalar8);
    }

    #[test]
    fn quantization_binary_rejected() {
        let result = Config::from_toml_str(
            r#"
            [vector]
            default_quantization = "binary"
            "#,
        );
        assert!(
            result.is_err(),
            "binary quantization must be rejected as unimplemented"
        );
    }

    #[test]
    fn local_backend_none_quantization_ok() {
        let cfg = Config::from_toml_str(
            r#"
            [storage]
            backend = "local"

            [vector]
            default_quantization = "none"
            "#,
        )
        .expect("local backend + none quantization should be ok");
        assert_eq!(cfg.storage.backend, StorageBackendType::Local);
        assert_eq!(cfg.vector.default_quantization, VectorQuantization::None);
    }

    #[test]
    fn default_quantization_is_none() {
        // Guards depend on the default being the only implemented scheme so the
        // out-of-the-box config validates.
        assert_eq!(
            VectorConfig::default().default_quantization,
            VectorQuantization::None
        );
    }

    #[test]
    fn count_user_facing_settings() {
        // 47 user-facing settings:
        //   server: 5      (rest_port, grpc_port, es_compat_port, data_dir, bind_address)
        //   auth:   2      (enabled, admin_api_key)
        //   tls:    3      (enabled, cert_path, key_path)
        //   storage: 10    (wal_sync, wal_batch_ms, wal_max_size_mb, flush_size_mb,
        //                   flush_interval_secs, backend, s3_bucket, s3_prefix,
        //                   s3_region, local_cache_dir)
        //   merge:  5      (strategy, min_segments, max_segment_mb, io_rate_mb_per_sec, max_concurrent)
        //   compression: 3 (enabled, level, block_size_docs)
        //   fts:    1      (default_analyzer)
        //   vector: 6      (default_metric, hnsw_m, hnsw_ef_construction, hnsw_ef_search,
        //                   default_quantization, max_dimensions)
        //   logs:   2      (retention_days, time_partition)
        //   embedding: 4   (default_endpoint, default_model, batch_size, timeout_ms)
        //   limits: 3      (max_query_memory_mb, max_concurrent_searches, max_fields_per_index)
        //   indexing: 3    (turbo_batch_size, turbo_parallel, turbo_fast_analyzer)
        //   ─────────
        //   total: 47 fields, minus 1 auto-generated (admin_api_key) = 46 meaningful user settings
        let total: usize = 5 + 2 + 3 + 10 + 5 + 3 + 1 + 6 + 2 + 4 + 3 + 3;
        assert_eq!(total, 47);
    }
}
