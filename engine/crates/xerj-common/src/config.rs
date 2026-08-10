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
    /// Network and data directory settings — 6 settings.
    pub server: ServerConfig,
    /// Authentication — 2 settings.
    pub auth: AuthConfig,
    /// CORS (cross-origin browser access) — 2 settings. Default restrictive.
    pub cors: CorsConfig,
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
    /// Scroll + async-search context TTLs and open-context caps — 7 settings.
    pub search_context: SearchContextConfig,
    /// Index lifecycle management (retention) executor — 2 settings.
    pub ilm: IlmConfig,
    /// Structured logging + access log — 2 settings.
    pub logging: LoggingConfig,
    /// Elasticsearch/OpenSearch wire-compatibility identity — 2 settings.
    pub compat: CompatConfig,
}

// Total: 5+3+2+3+10+5+3+1+6+2+4+3+4+3+2 = 56 fields (incl. cors: 2, auth: 3,
// logging: 2). `Default` is derived — every field is a sub-config that
// implements `Default`, so the derive produces exactly the same all-defaults
// value the manual impl used to build by hand.

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

        // Trusted proxies: a typo in a trust boundary must fail startup, not
        // silently degrade. (A rejected entry that we merely skipped would
        // leave the operator believing forwarding headers are honoured when
        // they are not — or, worse, the reverse.)
        crate::net::TrustedProxies::parse(&self.server.trusted_proxies)
            .map_err(|why| XerjError::config(format!("server.trusted_proxies: {why}")))?;

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

        if !(1..=4096).contains(&self.embedding.onnx_scheduling_window) {
            return Err(XerjError::config(
                "embedding.onnx_scheduling_window must be in 1..=4096",
            ));
        }
        if !(1..=2).contains(&self.embedding.onnx_session_pool_size) {
            return Err(XerjError::config(
                "embedding.onnx_session_pool_size must be in 1..=2",
            ));
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

        // Cluster: fail closed rather than expose an unauthenticated Raft
        // control port (issue #75).
        self.cluster.validate()?;

        // Logging: format must be one of the two supported line formats.
        let fmt = self.logging.format.as_str();
        if !fmt.eq_ignore_ascii_case("text") && !fmt.eq_ignore_ascii_case("json") {
            return Err(XerjError::config(format!(
                "logging.format must be \"text\" or \"json\" (got {fmt:?})"
            )));
        }

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

    /// Is `server.bind_address` confined to the local host?
    ///
    /// Only a loopback literal counts. `0.0.0.0` and `::` are *unspecified*,
    /// not loopback — they bind every interface the host has, which is the
    /// exposure this predicate exists to detect, and they are also the
    /// shipped default. An address that does not parse is reported as not
    /// confined: it fails closed here, and the `SocketAddr` parse at bind
    /// time rejects it a moment later anyway.
    pub fn bind_address_is_loopback(&self) -> bool {
        self.server
            .bind_address
            .trim()
            .parse::<std::net::IpAddr>()
            .map(crate::net::canonical_ip)
            .is_ok_and(|ip| ip.is_loopback())
    }

    /// Would starting now put a cleartext gRPC listener on a network-reachable
    /// interface while the operator believes TLS covers the node? (issue #229)
    ///
    /// True only when all three hold: TLS is on, the bind address is not
    /// confined to loopback, and the operator has not declared the exposure
    /// intentional via `tls.allow_insecure_grpc_h2c`. The caller's job is to
    /// refuse to start — see `xerj-server/src/main.rs`.
    ///
    /// The shape is Elasticsearch's bootstrap-check idea (approach only, no
    /// code: `BootstrapChecks.java:58-70` gates enforcement on the transport
    /// being bound off-loopback and offers one explicit override). Two
    /// deliberate departures: ES also exempts link-local, which is still
    /// reachable by every other host on the link, so this does not; and the
    /// override lives in the config file the setting it relaxes lives in,
    /// rather than a JVM system property.
    pub fn grpc_h2c_exposed_off_loopback(&self) -> bool {
        self.tls.enabled && !self.tls.allow_insecure_grpc_h2c && !self.bind_address_is_loopback()
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Sub-configs  (38 user-facing settings total)
// ═════════════════════════════════════════════════════════════════════════════

/// Network and data-directory settings.
///
/// **6 settings.**
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
    /// Reverse proxies whose `X-Forwarded-For` / `X-Real-IP` headers may be
    /// believed (default: `[]` — **believe nobody**).
    ///
    /// Client identity (the per-IP auth rate-limit bucket and the audit-log
    /// source address) is normally taken from the TCP peer address, which a
    /// caller cannot forge. `X-Forwarded-For` is attacker-controlled and is
    /// therefore ignored unless the *socket peer* matches an entry here.
    ///
    /// Entries are single addresses (`"10.0.0.7"`, `"::1"`) or CIDR blocks
    /// (`"10.0.0.0/8"`, `"fd00::/8"`); invalid entries are rejected at
    /// startup rather than silently ignored. Set this **only** to the
    /// addresses of proxies you operate: anything listed here can claim to
    /// be any client and so bypass per-IP throttling.
    ///
    /// With this set, the forwarded chain is read right-to-left and the
    /// right-most address that is *not* itself a listed proxy is used — the
    /// left end of the chain is written by the caller and cannot be trusted.
    pub trusted_proxies: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            rest_port: 8080,
            grpc_port: 8081,
            es_compat_port: 9200,
            data_dir: "./data".into(),
            bind_address: "0.0.0.0".into(),
            trusted_proxies: Vec::new(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// Elasticsearch/OpenSearch wire-compatibility identity — controls what the
/// `version`/`distribution` block in `GET /` (and any other endpoint that
/// reports the same block) says about xerj to the calling client.
///
/// Three-tier resolution, most explicit wins:
/// 1. `distribution`/`version` set here (via `--compat-distribution` /
///    `XERJ_COMPAT_DISTRIBUTION`, `--compat-version` / `XERJ_COMPAT_VERSION`)
///    — always wins, no per-request inspection at all. For a client that
///    sends no `User-Agent`, or an unrecognized one, or an operator who just
///    wants deterministic behavior without relying on request sniffing.
/// 2. Left unset (default): xerj inspects the request's `User-Agent` header
///    and answers per-request — an OpenSearch client and an Elasticsearch
///    client hitting the SAME running instance each see the block shaped
///    for their own ecosystem.
/// 3. Neither of the above resolves anything (no override, no recognizable
///    `User-Agent`): unchanged pre-existing behavior — plain Elasticsearch.
///
/// **2 settings.**
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompatConfig {
    /// Force the reported client distribution: `"elasticsearch"` or
    /// `"opensearch"`. Empty (default) — resolve per-request from
    /// `User-Agent` instead (falls back to `elasticsearch`-shaped output,
    /// i.e. no `distribution` field, when nothing is detected either way).
    pub distribution: String,
    /// Force the reported `version.number`. Empty (default) — when the
    /// caller is auto-detected as OpenSearch (see `distribution` above),
    /// reports a fixed, empirically-verified compatible OpenSearch version
    /// instead (NOT the client's own self-reported library version — tried
    /// that first, a real OpenSearch Dashboards container rejected it, see
    /// `es_compat::FALLBACK_OPENSEARCH_VERSION`'s doc comment for why).
    pub version: String,
}

// ─────────────────────────────────────────────────────────────────────────────

/// Authentication settings.
///
/// **3 settings.**
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
    /// Optional read-only metrics scrape token (default: `""` — disabled).
    ///
    /// When set AND auth is enabled, a caller presenting this exact token
    /// (`Authorization: Bearer <token>` or `ApiKey <token>`) may scrape
    /// `GET /v1/metrics` — and ONLY that endpoint — without the admin key.
    /// This lets an operator hand Prometheus a low-privilege scrape credential
    /// that can read metrics but cannot touch index data. The admin key still
    /// works for `/v1/metrics`; when this is empty, metrics require the admin
    /// key like any other endpoint (unchanged behavior).
    pub metrics_token: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            admin_api_key: String::new(),
            metrics_token: String::new(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// Structured logging + access-log settings (RC4-W4 item 6).
///
/// **2 settings.**
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    /// Log line format:
    /// - `"text"` (default) — human-readable compact one-liners.
    /// - `"json"` — one JSON object per line, for structured log shippers
    ///   (Loki / Elastic / Datadog). Field names follow tracing's JSON schema.
    ///
    /// Validated at load time; any other value is rejected.
    pub format: String,
    /// Emit an INFO-level access log line per HTTP request (method, path,
    /// status, latency) via the tower-http request-tracing layer.
    ///
    /// Default `false`: request tracing stays at DEBUG (silent under the
    /// default `info` filter), preserving today's quiet startup. Turn on for
    /// request-level observability without dropping the global filter to debug.
    pub access_log: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            format: "text".into(),
            access_log: false,
        }
    }
}

impl LoggingConfig {
    /// True when JSON line format is requested (case-insensitive).
    pub fn is_json(&self) -> bool {
        self.format.eq_ignore_ascii_case("json")
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// Cross-Origin Resource Sharing (CORS) settings.
///
/// **2 settings.**
///
/// DEFAULT RESTRICTIVE (RC4 item 5): out of the box the server emits **no**
/// `Access-Control-Allow-Origin` header, so a browser blocks every cross-origin
/// read. The pre-RC4 build hard-coded `Access-Control-Allow-Origin: *` /
/// `-Methods: *` / `-Headers: *` with no knob — any web page on the internet
/// could script authenticated reads/writes against a XERJ node reachable from
/// the victim's browser.
///
/// This restrictive default does **not** affect:
/// - the bundled Xerj Console (served same-origin from the same listener — CORS
///   never applies to same-origin requests);
/// - non-browser clients: `curl`, the shipped recipes, SDKs, and Kibana (which
///   talks to ES server-side) all ignore CORS entirely.
///
/// To allow a browser SPA hosted on another origin, set
/// `allowed_origins = ["https://app.example.com"]`. Set
/// `allow_any_origin = true` **only** for local development to restore the old
/// wide-open behavior.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CorsConfig {
    /// Explicit allow-list of browser `Origin`s permitted to make cross-origin
    /// requests, e.g. `["https://app.example.com"]`. Empty by default
    /// (restrictive). Each entry must be a full origin (scheme + host + optional
    /// port); entries that don't parse as an HTTP header value are ignored.
    pub allowed_origins: Vec<String>,
    /// Escape hatch restoring the pre-RC4 wide-open policy
    /// (`Access-Control-Allow-Origin: *`, any method, any header). Default
    /// `false`. Enable **only** for local development — a public node with this
    /// on is scriptable by any web page.
    pub allow_any_origin: bool,
}

// ─────────────────────────────────────────────────────────────────────────────

/// TLS settings.
///
/// **4 settings.**
///
/// Defaults are derived: TLS is disabled (`enabled: false`) with empty
/// cert/key paths so the engine starts out of the box; enable it in
/// production by setting `cert_path` + `key_path`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TlsConfig {
    /// Enable in-process TLS termination on the **REST and ES-compat**
    /// listeners (default: `false` — enable in production).
    ///
    /// It does **not** cover the gRPC listener: `xerj-server/src/grpc.rs`
    /// builds tonic without its `tls` feature, so `server.grpc_port` speaks
    /// cleartext HTTP/2 (h2c) whatever this is set to (issue #229). Enabling
    /// TLS while binding off-loopback therefore refuses to start unless
    /// [`TlsConfig::allow_insecure_grpc_h2c`] says the exposure is intended.
    pub enabled: bool,
    /// Path to the PEM-encoded certificate file.
    pub cert_path: String,
    /// Path to the PEM-encoded private key file.
    pub key_path: String,
    /// Permit the cleartext h2c gRPC listener on a network-reachable
    /// interface while `enabled` is `true` (default: `false` — refuse).
    ///
    /// An operator who turns TLS on reasonably reads that as "every listener
    /// is encrypted", and nothing on the wire corrects them: gRPC clients keep
    /// working, so credentials and documents cross the network in the clear
    /// with no symptom. Rather than downgrade silently, startup fails closed
    /// and names this setting as the way to say the exposure is deliberate —
    /// for example when a sidecar or service mesh terminates TLS in front of
    /// `server.grpc_port`.
    ///
    /// Irrelevant when `enabled` is `false`: nothing then claims the gRPC
    /// port is encrypted, and the startup banner already says the listeners
    /// are plain TCP.
    pub allow_insecure_grpc_h2c: bool,
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
    /// **Not implemented — nothing reads this value.** (Issue #199, second
    /// comment.)
    ///
    /// It was documented as "how long to retain log data before automatic
    /// deletion (default: 90 days)", and no code path in any crate has ever
    /// read it: `grep -rn retention_days engine/crates` finds this
    /// declaration and `xerj-logs`'s unrelated `RetentionPolicy::retention_days`,
    /// which the server never constructs from here. An operator who set it got
    /// the same silent nothing the ES-compat ILM API used to give.
    ///
    /// Retention that actually runs is ILM: put a policy with a `delete` phase
    /// and attach it with `index.lifecycle.name` (see `xerj_engine::ilm`). The
    /// server warns at boot when this knob is set away from its default, so the
    /// setting can no longer be silently believed. Wiring it into the executor
    /// (or deleting it) is tracked in the accepted-and-ignored class issue #204
    /// — it is left in place here rather than removed because
    /// `deny_unknown_fields` would turn removal into a node that refuses to
    /// boot on an existing `xerj.toml`.
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
    /// Experimental ONNX backend: local FP32 all-MiniLM-L6-v2-compatible
    /// model with int64 BERT inputs and a width-384 token-embedding output.
    /// Required when `mode = "onnx-experimental"`; never auto-downloaded.
    pub onnx_model_path: String,
    /// Experimental ONNX backend: tokenizer.json from the same model/export.
    pub onnx_tokenizer_path: String,
    /// Maximum passages collected before one ONNX scheduling call
    /// (default: `64`, range: `1..=4096`). This is independent of the
    /// backend's internal `onnx_max_batch` microbatch cap.
    pub onnx_scheduling_window: usize,
    /// Experimental ONNX Runtime intra-op threads (default: available CPUs).
    pub onnx_intra_threads: usize,
    /// Number of independent ONNX Runtime sessions (default: `1`, range:
    /// `1..=2`). Two sessions permit two scheduling windows to run in
    /// parallel, at the cost of another model session's memory.
    pub onnx_session_pool_size: usize,
    /// Maximum texts accepted by one ONNX scheduling window.
    pub onnx_max_pending: usize,
    /// Maximum documents in one ONNX inference microbatch.
    pub onnx_max_batch: usize,
    /// Maximum batch × padded-token slots per ONNX inference microbatch.
    pub onnx_padded_token_budget: usize,
    /// Maximum ONNX calls admitted globally per shared model/session.
    pub onnx_max_inflight_calls: usize,
    /// Reject one ONNX call above this many UTF-8 input bytes before tokenizing.
    pub onnx_max_input_bytes_per_call: usize,
    /// Aggregate UTF-8 bytes admitted globally per shared ONNX model/session.
    pub onnx_max_inflight_input_bytes: usize,
    /// Engine-lifetime immutable ONNX assets. Cloned configurations share this
    /// cell, so identity reporting and lazy loading consume the same bytes.
    /// It is runtime state, never configuration input or serialized output.
    #[serde(skip)]
    pub runtime_onnx_assets:
        std::sync::Arc<std::sync::OnceLock<Result<EmbeddingAssetSnapshot, String>>>,
}

#[derive(Debug, Clone)]
pub struct EmbeddingAssetSnapshot {
    pub model_bytes: std::sync::Arc<[u8]>,
    pub tokenizer_bytes: std::sync::Arc<[u8]>,
    pub model_sha256: String,
    pub tokenizer_sha256: String,
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
            onnx_model_path: String::new(),
            onnx_tokenizer_path: String::new(),
            onnx_scheduling_window: 64,
            onnx_intra_threads: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
            onnx_session_pool_size: 1,
            onnx_max_pending: 4096,
            onnx_max_batch: 64,
            onnx_padded_token_budget: 4096,
            onnx_max_inflight_calls: 8,
            onnx_max_input_bytes_per_call: 8 * 1024 * 1024,
            onnx_max_inflight_input_bytes: 32 * 1024 * 1024,
            runtime_onnx_assets: std::sync::Arc::new(std::sync::OnceLock::new()),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// Resource limits.
///
/// **11 settings.**
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
    /// Maximum number of actions in a single `_bulk` request (default:
    /// `50_000`). Each action materializes several intermediate Vecs
    /// (line pairs, parse outcomes, parsed actions) totalling ~300 B/entry,
    /// so an unbounded bulk body of one-byte lines can amplify to ~7.5 GiB
    /// of heap before any memtable admission check fires. This cap rejects
    /// oversized bulks at the top of the parse phase with a 413-style per-item
    /// error instead of letting jemalloc abort the process on OOM.
    pub max_actions_per_bulk: usize,
    /// Filesystem locations under which a snapshot repository's `settings.location`
    /// is allowed to resolve (an ES `path.repo` equivalent). Empty (the default)
    /// means **only `data_dir`** is permitted. Without this bound a write-capable
    /// client could `PUT /_snapshot/<repo>` with an arbitrary absolute `location`
    /// and then create/restore snapshots that read or write index data outside
    /// `data_dir` (path-traversal via the repo root, F-PATH-02). Each entry is a
    /// base directory; a repo location is accepted only if it canonicalizes to a
    /// path inside `data_dir` or inside one of these bases.
    #[serde(default)]
    pub snapshot_repo_allowlist: Vec<String>,
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
    /// Process-wide ceiling on the total bytes buffered across ALL indices'
    /// memtables (default: `0` = auto-derive to 25% of the effective
    /// cgroup/system memory limit, floored at 2048 MiB, capped at 50% of
    /// that same limit). This is the parent circuit breaker for the ingest
    /// path:
    /// per-index back-pressure only bounds one index at `~3×flush_size_mb`,
    /// so `N` indices could buffer `N × 1.5 GiB` with no global ceiling —
    /// the structural cause of the 112 GiB OOM. When the summed memtable
    /// footprint crosses this budget, writes are rejected with HTTP 429
    /// `circuit_breaking_exception` instead of growing until the kernel
    /// OOM-kills the process.
    pub max_total_memtable_mb: u64,
    /// Process-wide retained-payload ceiling for all immutable segment
    /// hydration caches (default: `0` = 20% of the effective cgroup/system
    /// memory limit). This is not an RSS ceiling: query materialization,
    /// decode scratch, mmaps, allocator fragmentation, and unrelated caches
    /// are outside it.
    pub max_segment_hydration_cache_mb: u64,
    /// RSS admission watermark, as a percentage of the effective process
    /// memory limit (default: `95`). The effective limit is the cgroup
    /// memory limit when one is set (e.g. under `systemd-run -p MemoryMax=`
    /// or a container), else total system RAM. When resident set size
    /// crosses this fraction of the limit, writes are rejected with HTTP 429
    /// `circuit_breaking_exception` so a 429 beats the OOM-killer. Mirrors
    /// Elasticsearch's real-memory parent circuit breaker (default 95%).
    /// Set to `0` to disable the RSS admission check.
    pub memory_watermark_percent: u8,
    /// Disk flood-stage watermark, as a percentage of the data-dir
    /// filesystem that is *used* (default: `95`). A background `statvfs`
    /// poll auto-engages a write block when used space crosses this
    /// threshold, mirroring Elasticsearch's
    /// `cluster.routing.allocation.disk.watermark.flood_stage`. This
    /// prevents the engine from writing until `ENOSPC`, which poisons the
    /// WAL. The block clears automatically once usage drops back below the
    /// threshold. Set to `0` to disable the disk watermark.
    pub disk_flood_stage_percent: u8,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_query_memory_mb: 512,
            max_concurrent_searches: 64,
            max_fields_per_index: 500,
            max_actions_per_bulk: 50_000,
            snapshot_repo_allowlist: Vec::new(),
            max_body_bytes: 100 * 1024 * 1024,
            max_result_window: 10_000,
            max_mget_docs: 10_000,
            max_buckets: 65_536,
            max_total_memtable_mb: 0, // 0 = auto-derive (25% effective limit, floor 2 GiB, cap 50%)
            max_segment_hydration_cache_mb: 0, // 0 = 20% effective memory, no floor
            memory_watermark_percent: 95,
            disk_flood_stage_percent: 95,
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
/// **5 settings.**
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
    /// Cluster-wide shared secret authenticating every control frame on
    /// `port` (default: empty — which is only legal while `enabled = false`).
    ///
    /// Every node in the cluster must be configured with the **same** secret.
    /// Frames are signed with HMAC-SHA256 over a per-connection challenge, so
    /// a peer that does not hold this secret cannot inject Raft messages, and a
    /// captured frame cannot be replayed.
    ///
    /// May be left empty here and supplied via the `XERJ_CLUSTER_AUTH_SECRET`
    /// environment variable instead — preferable when the config file is baked
    /// into an image. An explicit non-empty value here wins over the
    /// environment.
    ///
    /// **Fail-closed:** with `enabled = true` and no secret from either source,
    /// the node refuses to start. There is no unauthenticated cluster mode.
    ///
    /// Minimum length [`ClusterConfig::MIN_AUTH_SECRET_LEN`]; generate one with
    /// `openssl rand -hex 32`.
    pub auth_secret: String,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 9300,
            peers: Vec::new(),
            tick_ms: 50,
            auth_secret: String::new(),
        }
    }
}

impl ClusterConfig {
    /// Environment variable consulted when `auth_secret` is empty.
    pub const AUTH_SECRET_ENV: &'static str = "XERJ_CLUSTER_AUTH_SECRET";

    /// Minimum accepted secret length, in characters.
    ///
    /// Kept in step with `xerj_cluster::auth::MIN_SECRET_LEN`; the transport
    /// enforces the same floor independently, so a mismatch fails closed.
    pub const MIN_AUTH_SECRET_LEN: usize = 16;

    /// The secret actually in force, or `None` if neither source supplied one.
    ///
    /// Precedence: an explicit non-empty `auth_secret` wins; otherwise
    /// [`Self::AUTH_SECRET_ENV`]. Explicit configuration beats the ambient
    /// environment so that a stray env var cannot silently re-key a cluster.
    pub fn effective_auth_secret(&self) -> Option<String> {
        Self::resolve_auth_secret(&self.auth_secret, std::env::var(Self::AUTH_SECRET_ENV).ok())
    }

    /// Pure resolution rule behind [`Self::effective_auth_secret`], split out so
    /// it can be tested without mutating process-wide environment state.
    pub fn resolve_auth_secret(field: &str, env: Option<String>) -> Option<String> {
        let field = field.trim();
        if !field.is_empty() {
            return Some(field.to_string());
        }
        env.filter(|v| !v.trim().is_empty())
            .map(|v| v.trim().to_string())
    }

    /// Fail-closed validation: cluster mode requires a usable shared secret.
    pub fn validate(&self) -> Result<(), crate::XerjError> {
        if !self.enabled {
            return Ok(());
        }
        match self.effective_auth_secret() {
            None => Err(crate::XerjError::config(format!(
                "cluster.enabled = true requires a shared secret: set cluster.auth_secret \
                 or the {} environment variable. Refusing to start an unauthenticated \
                 cluster transport — anyone able to reach cluster.port could otherwise \
                 inject Raft control messages.",
                Self::AUTH_SECRET_ENV
            ))),
            Some(s) if s.chars().count() < Self::MIN_AUTH_SECRET_LEN => {
                Err(crate::XerjError::config(format!(
                    "cluster auth secret is too short ({} chars, minimum {}); \
                     generate one with `openssl rand -hex 32`",
                    s.chars().count(),
                    Self::MIN_AUTH_SECRET_LEN
                )))
            }
            Some(_) => Ok(()),
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

// ─────────────────────────────────────────────────────────────────────────────

/// Scroll + async-search context lifecycle settings (RC4 blocker 11).
///
/// **7 settings.**
///
/// Every open scroll context pins a fully-hydrated `Vec<Hit>` snapshot and
/// every stored async-search result pins its response JSON. Before v1.0.0-rc.4
/// neither was ever expired or capped — an unauthenticated client that opened
/// scrolls in a loop (or simply never called DELETE /_search/scroll) grew the
/// process RSS without bound. These settings mirror the PIT lifecycle: a TTL
/// on every context (refreshed by the `scroll`/`keep_alive` parameters), a
/// background sweeper, and a hard cap on concurrently-open contexts (requests
/// beyond the cap get 429, like ES's `search.max_open_scroll_context`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchContextConfig {
    /// Default scroll TTL when `?scroll=…` carries no parsable duration
    /// (default: 60 seconds). ES requires the duration; we degrade to this
    /// default rather than 400.
    pub scroll_default_keep_alive_secs: u64,
    /// Hard cap on any scroll keep-alive (default: 86 400 = 24 h — mirrors
    /// ES `search.max_keep_alive`). Larger requests are capped silently.
    pub scroll_max_keep_alive_secs: u64,
    /// Maximum concurrently-open scroll contexts (default: 500 — mirrors
    /// ES `search.max_open_scroll_context`). Opening more returns 429.
    pub max_open_scrolls: usize,
    /// Default async-search result TTL when the submit request carries no
    /// `keep_alive` (default: 300 seconds, the pre-rc.4 hardcoded expiry).
    pub async_default_keep_alive_secs: u64,
    /// Hard cap on any async-search `keep_alive` (default: 86 400 = 24 h).
    pub async_max_keep_alive_secs: u64,
    /// Maximum concurrently-stored async-search results (default: 500).
    /// Submitting more returns 429.
    pub max_open_async_searches: usize,
    /// How often the background sweeper drops expired scroll and
    /// async-search contexts (default: 30 seconds).
    pub sweep_interval_secs: u64,
}

impl Default for SearchContextConfig {
    fn default() -> Self {
        Self {
            scroll_default_keep_alive_secs: 60,
            scroll_max_keep_alive_secs: 86_400,
            max_open_scrolls: 500,
            async_default_keep_alive_secs: 300,
            async_max_keep_alive_secs: 86_400,
            max_open_async_searches: 500,
            sweep_interval_secs: 30,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// Index-lifecycle-management (ILM) executor settings — issue #199.
///
/// **2 settings.**
///
/// Until issue #199's fix `PUT /_ilm/policy/{name}` stored a policy and nothing
/// ever ran it: a retention policy was accepted, echoed back on GET, and
/// ignored, so the index grew forever while the API reported success. These
/// settings drive the executor that now applies the phases XERJ can actually
/// perform (see `xerj_engine::ilm`), and give an operator a way to slow it
/// down or turn it off entirely.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IlmConfig {
    /// Master switch for the background executor (default: `true`).
    ///
    /// When `false` no phase is ever applied. Policies are still validated
    /// and stored, `GET /_ilm/explain` still reports what *would* happen,
    /// and `GET /_ilm/status` reports `STOPPED` — the executor never
    /// silently pretends to run.
    pub enabled: bool,
    /// How often the executor evaluates every managed index
    /// (default: `600` seconds = 10 minutes, matching ES's
    /// `indices.lifecycle.poll_interval`).
    ///
    /// Retention is a coarse, long-horizon operation — quickwit's janitor
    /// runs its retention pass hourly
    /// (`quickwit-janitor/src/actors/retention_policy_executor.rs:35`) — so
    /// there is nothing to gain from a tight loop, and a tight loop on a
    /// node with thousands of indices is pure overhead.
    pub poll_interval_secs: u64,
}

impl Default for IlmConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_secs: 600,
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
    fn shipped_default_config_parses_and_validates() {
        // rc.3 regression: the shipped engine/xerj.default.toml failed to
        // parse (`unknown field hnsw_offload_threshold`, line 223) so the
        // documented `--config xerj.default.toml` invocation was a dead
        // boot. Every sub-config is `deny_unknown_fields`, so ANY key that
        // drifts out of the schema kills the boot — guard the shipped file
        // byte-for-byte at compile time.
        let toml_src = include_str!("../../../xerj.default.toml");
        let cfg = Config::from_toml_str(toml_src)
            .expect("shipped xerj.default.toml must parse against the current Config schema");
        cfg.validate()
            .expect("shipped xerj.default.toml must pass Config::validate");
    }

    #[test]
    fn onnx_throughput_controls_preserve_single_session_defaults() {
        let cfg = Config::from_toml_str("[embedding]\n").unwrap();
        assert_eq!(cfg.embedding.onnx_scheduling_window, 64);
        assert_eq!(cfg.embedding.onnx_session_pool_size, 1);
    }

    #[test]
    fn onnx_throughput_controls_accept_only_bounded_values() {
        for (key, valid) in [
            ("onnx_scheduling_window", [1, 64, 4096].as_slice()),
            ("onnx_session_pool_size", [1, 2].as_slice()),
        ] {
            for value in valid {
                Config::from_toml_str(&format!("[embedding]\n{key} = {value}\n"))
                    .unwrap_or_else(|error| panic!("{key}={value} must be valid: {error}"));
            }
        }
        for (key, invalid) in [
            ("onnx_scheduling_window", [0, 4097].as_slice()),
            ("onnx_session_pool_size", [0, 3].as_slice()),
        ] {
            for value in invalid {
                Config::from_toml_str(&format!("[embedding]\n{key} = {value}\n"))
                    .expect_err(&format!("{key}={value} must be rejected"));
            }
        }
    }

    /// The safe default: no proxy is trusted, so `X-Forwarded-For` carries no
    /// authority anywhere until an operator says otherwise (#76 S5-4).
    #[test]
    fn trusted_proxies_default_to_empty() {
        assert!(Config::default().server.trusted_proxies.is_empty());
        let cfg = Config::from_toml_str("[server]\nrest_port = 9000\n").unwrap();
        assert!(cfg.server.trusted_proxies.is_empty());
        assert!(
            crate::net::TrustedProxies::parse(&cfg.server.trusted_proxies)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn trusted_proxies_parse_from_toml() {
        let cfg = Config::from_toml_str("[server]\ntrusted_proxies = [\"10.0.0.0/8\", \"::1\"]\n")
            .unwrap();
        cfg.validate().unwrap();
        let t = crate::net::TrustedProxies::parse(&cfg.server.trusted_proxies).unwrap();
        assert!(t.contains(&"10.4.5.6".parse().unwrap()));
        assert!(!t.contains(&"11.4.5.6".parse().unwrap()));
    }

    /// A typo in a trust boundary fails startup rather than silently
    /// producing a set that trusts nothing (or everything).
    #[test]
    fn malformed_trusted_proxy_fails_validation() {
        let err = Config::from_toml_str("[server]\ntrusted_proxies = [\"10.0.0.0/99\"]\n")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("trusted_proxies"),
            "error should name the setting, got: {err}"
        );
    }

    #[test]
    fn count_user_facing_settings() {
        // 50 user-facing settings:
        //   server: 6      (rest_port, grpc_port, es_compat_port, data_dir,
        //                   bind_address, trusted_proxies)
        //   auth:   3      (enabled, admin_api_key, metrics_token)   ← RC4-W4 item 4
        //   tls:    4      (enabled, cert_path, key_path,
        //                   allow_insecure_grpc_h2c)             ← issue #229
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
        //   limits: 12     (query/concurrency/mapping/bulk-actions/body/result/mget/bucket
        //                   limits, snapshot repo allowlist, total memtable,
        //                   segment hydration cache, RSS, disk)
        //   indexing: 3    (turbo_batch_size, turbo_parallel, turbo_fast_analyzer)
        //   logging: 2     (format, access_log)                      ← RC4-W4 item 6
        //   ─────────
        //   total: 61 fields, minus 1 auto-generated (admin_api_key) = 60 meaningful user settings
        let total: usize = 6 + 3 + 4 + 10 + 5 + 3 + 1 + 6 + 2 + 4 + 12 + 3 + 2;
        assert_eq!(total, 61);
    }

    // ── gRPC h2c exposure: fail closed (issue #229) ──────────────────────────

    /// The reported default in `xerj.toml`-less deployments. `0.0.0.0` binds
    /// every interface, so "TLS on, defaults otherwise" is exactly the case
    /// that must be caught — this is the assertion that fails without the fix.
    #[test]
    fn tls_on_with_default_bind_flags_grpc_h2c_exposure() {
        let cfg = Config::from_toml_str(
            "[tls]\nenabled = true\ncert_path = \"/c.pem\"\nkey_path = \"/k.pem\"\n",
        )
        .unwrap();
        assert_eq!(cfg.server.bind_address, "0.0.0.0");
        assert!(!cfg.bind_address_is_loopback(), "0.0.0.0 is not loopback");
        assert!(cfg.grpc_h2c_exposed_off_loopback());
    }

    #[test]
    fn tls_on_bound_to_loopback_is_fine() {
        for bind in ["127.0.0.1", "127.0.0.5", "::1", "::ffff:127.0.0.1"] {
            let cfg = Config::from_toml_str(&format!(
                "[server]\nbind_address = \"{bind}\"\n\
                 [tls]\nenabled = true\ncert_path = \"/c.pem\"\nkey_path = \"/k.pem\"\n"
            ))
            .unwrap();
            assert!(cfg.bind_address_is_loopback(), "{bind} should be loopback");
            assert!(!cfg.grpc_h2c_exposed_off_loopback(), "{bind} must not trip");
        }
    }

    #[test]
    fn tls_off_never_trips_the_check() {
        // Nothing claims the port is encrypted, so there is no false promise
        // to fail closed on — the banner already says the listeners are plain.
        let cfg = Config::from_toml_str("[server]\nbind_address = \"0.0.0.0\"\n").unwrap();
        assert!(!cfg.tls.enabled);
        assert!(!cfg.grpc_h2c_exposed_off_loopback());
    }

    #[test]
    fn explicit_opt_out_permits_the_exposure() {
        let cfg = Config::from_toml_str(
            "[server]\nbind_address = \"10.0.0.7\"\n\
             [tls]\nenabled = true\ncert_path = \"/c.pem\"\nkey_path = \"/k.pem\"\n\
             allow_insecure_grpc_h2c = true\n",
        )
        .unwrap();
        assert!(!cfg.grpc_h2c_exposed_off_loopback());
    }

    #[test]
    fn routable_bind_with_tls_trips_the_check() {
        for bind in ["10.0.0.7", "0.0.0.0", "::", "fe80::1", "not-an-ip"] {
            let cfg = Config::from_toml_str(&format!(
                "[server]\nbind_address = \"{bind}\"\n\
                 [tls]\nenabled = true\ncert_path = \"/c.pem\"\nkey_path = \"/k.pem\"\n"
            ))
            .unwrap();
            assert!(cfg.grpc_h2c_exposed_off_loopback(), "{bind} must trip");
        }
    }

    // ── Cluster auth: fail closed (issue #75) ────────────────────────────────

    #[test]
    fn cluster_enabled_without_secret_is_rejected() {
        let err = Config::from_toml_str(
            "[cluster]\nenabled = true\nport = 9300\npeers = [\"n2=10.0.0.2:9300\"]\n",
        )
        .expect_err("cluster mode without a shared secret must not load");
        let msg = err.to_string();
        assert!(
            msg.contains("auth_secret") && msg.contains("XERJ_CLUSTER_AUTH_SECRET"),
            "error must name both secret sources, got: {msg}"
        );
    }

    #[test]
    fn cluster_enabled_with_secret_is_accepted() {
        let cfg = Config::from_toml_str(
            "[cluster]\nenabled = true\nauth_secret = \"0123456789abcdef0123\"\n",
        )
        .expect("cluster mode with a secret must load");
        assert!(cfg.cluster.enabled);
        assert_eq!(
            cfg.cluster.effective_auth_secret().as_deref(),
            Some("0123456789abcdef0123")
        );
    }

    #[test]
    fn cluster_short_secret_is_rejected() {
        let err = Config::from_toml_str("[cluster]\nenabled = true\nauth_secret = \"hunter2\"\n")
            .expect_err("a 7-char cluster secret must be rejected");
        assert!(err.to_string().contains("too short"), "{err}");
    }

    #[test]
    fn cluster_disabled_needs_no_secret() {
        // The overwhelmingly common case: cluster mode off, nothing to check.
        Config::from_toml_str("[cluster]\nenabled = false\n").expect("single-node must still load");
        Config::default()
            .validate()
            .expect("defaults must validate");
    }

    #[test]
    fn auth_secret_resolution_prefers_explicit_config() {
        let env = Some("from-environment".to_string());
        // Explicit config wins; a stray env var cannot silently re-key a cluster.
        assert_eq!(
            ClusterConfig::resolve_auth_secret("from-config", env.clone()),
            Some("from-config".to_string())
        );
        // Empty / whitespace-only config falls back to the environment.
        assert_eq!(
            ClusterConfig::resolve_auth_secret("", env.clone()),
            Some("from-environment".to_string())
        );
        assert_eq!(
            ClusterConfig::resolve_auth_secret("   ", env),
            Some("from-environment".to_string())
        );
        // Neither source → None, which is what makes validation fail closed.
        assert_eq!(ClusterConfig::resolve_auth_secret("", None), None);
        assert_eq!(
            ClusterConfig::resolve_auth_secret("  ", Some("  ".to_string())),
            None
        );
        // Surrounding whitespace (e.g. a trailing newline from `$(cat file)`)
        // is configuration noise, not key material.
        assert_eq!(
            ClusterConfig::resolve_auth_secret("", Some("  padded-secret\n".to_string())),
            Some("padded-secret".to_string())
        );
    }
}
