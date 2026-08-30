//! xerj configuration system.
//!
//! Configuration is intentionally minimal: **116 settings** versus
//! Elasticsearch's 3000+. Every option is named, documented, and has a sensible
//! production-ready default. The format is TOML, loaded from a single file.
//!
//! That number is measured, not asserted: `journey_zero_config` in
//! `xerj-engine/tests/product_experience.rs` counts the leaf keys of a default
//! `Config` and fails when it drifts. It used to say 38 here, 56 twenty lines
//! down and 60 in the test — three figures, none of them the truth, because the
//! test re-added a hardcoded sum instead of counting anything (#207).
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
    /// Network and data directory settings — 7 settings.
    pub server: ServerConfig,
    /// Authentication — 3 settings.
    pub auth: AuthConfig,
    /// CORS (cross-origin browser access) — 2 settings. Default restrictive.
    pub cors: CorsConfig,
    /// TLS — 4 settings.
    pub tls: TlsConfig,
    /// Write-ahead log and flush behaviour — 10 settings.
    pub storage: StorageConfig,
    /// Segment merging — 8 settings.
    pub merge: MergeConfig,
    /// Data compression — 3 settings.
    pub compression: CompressionConfig,
    /// Full-text search — 1 setting.
    pub fts: FtsConfig,
    /// Vector search (HNSW) — 6 settings.
    pub vector: VectorConfig,
    /// Log (time-series) retention — 2 settings.
    pub logs: LogsConfig,
    /// External embedding service — 19 settings.
    pub embedding: EmbeddingConfig,
    /// Resource limits — 14 settings.
    pub limits: LimitsConfig,
    /// High-throughput turbo indexing — 3 settings.
    pub indexing: IndexingConfig,
    /// Engine parallelism — 4 settings.
    pub engine: EngineConfig,
    /// Cluster / Raft settings — 5 settings.
    pub cluster: ClusterConfig,
    /// Point-in-time TTL + sweep cadence — 3 settings.
    pub pit: PitConfig,
    /// Scroll + async-search context TTLs and open-context caps — 7 settings.
    pub search_context: SearchContextConfig,
    /// Structured logging + access log — 2 settings.
    pub logging: LoggingConfig,
    /// Elasticsearch/OpenSearch wire-compatibility identity — 2 settings.
    pub compat: CompatConfig,
    /// ISM/ILM index-lifecycle-management background execution — 1 setting.
    pub lifecycle: LifecycleConfig,
    /// Single-node WAL tap: push a filtered index subset to an external
    /// ES-compatible target — 10 settings. Off by default.
    pub wal_tap: WalTapConfig,
}

// 21 sub-configs, 116 leaf settings in total. Do not maintain that sum by hand
// — `journey_zero_config` in xerj-engine/tests/product_experience.rs counts a
// serialised `Config::default()` and fails if this comment and the module
// header stop matching. `Default` is derived: every field is a sub-config that
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

        // WAL tap: the target URL is a credential boundary, not just a URL.
        // Refusing it at boot is the only way a `user:pass@host` in the config
        // file never reaches the log line or the two endpoints that echo it.
        if let Err(reason) = WalTapConfig::check_target_url(&self.wal_tap.target_url) {
            return Err(XerjError::config(reason));
        }
        // …and the numeric knobs, with the same bounds `PUT /_xerj/wal_tap`
        // enforces. Without this the file was the way *around* the API's
        // validation: `PUT {"min_retained_generations": 100}` is a `400`
        // because the knob costs `n × storage.wal_max_size_mb` per WAL shard
        // per index, but `xerj.toml` took it in silence and the node held 100
        // rotated generations per shard forever. Same for
        // `max_retry_backoff_secs`, where a value above `u64::MAX / 1000` used
        // to reach an unchecked `* 1000` inside `WalTap::arm_backoff`.
        //
        // Same reasoning as `compression.block_size_docs` below: an
        // out-of-range value is a typo the operator wants to hear about now,
        // at boot, not as a disk-full page later.
        if let Err(reason) = self.wal_tap.check_limits() {
            return Err(XerjError::config(reason));
        }

        // Compression: block_size_docs is documented as 16–4096 and was
        // accepted at any value (#318 got `999999` past startup in silence).
        // Range-check it even though the knob is dormant: an out-of-range
        // value is a typo the operator wants to hear about now, and the check
        // must not start passing quietly if the knob is ever wired.
        if !CompressionConfig::BLOCK_SIZE_DOCS_RANGE.contains(&self.compression.block_size_docs) {
            return Err(XerjError::config(format!(
                "compression.block_size_docs must be in {}..={}, got {}",
                CompressionConfig::BLOCK_SIZE_DOCS_RANGE.start(),
                CompressionConfig::BLOCK_SIZE_DOCS_RANGE.end(),
                self.compression.block_size_docs
            )));
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

        // Merge: only the size-tiered policy is implemented. `run_merge_once`
        // (xerj-engine/src/index.rs) always builds a `SizeTieredMergePolicy`;
        // nothing anywhere reads `merge.strategy`. Accepting `log_structured`
        // would run size-tiered merging for an operator who chose a levelled
        // policy for its read amplification — the same silent substitution the
        // `storage.backend` guard above exists to prevent (#207).
        if self.merge.strategy != MergeStrategy::SizeTiered {
            return Err(XerjError::config(
                "merge.strategy: the log_structured merge policy is not implemented in this \
                 build; only \"size_tiered\" is supported",
            ));
        }

        // Vector: `scalar8` (SQ8) quantization is wired into the kNN serving
        // path (`Index::run_knn_brute_force`): a `scalar8` dense_vector field
        // scores candidates from 1-byte-per-dimension codes rather than their
        // f32 values, so it has the recall profile of SQ8. It does not reduce
        // resident memory today — the scan quantizes each candidate's current
        // vector per query rather than caching codes, which is what keeps an
        // updated document from being scored on a stale code (#371).
        // `none` and `scalar8` are therefore
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

    /// `server.bind_address` as an [`IpAddr`](std::net::IpAddr), or `None` if
    /// it is not an IP literal.
    ///
    /// The single place the setting is interpreted, so every caller agrees on
    /// what it means. Surrounding brackets are accepted and stripped: a v6
    /// address is conventionally written `[::1]` next to a port, and an
    /// operator who copies that form into the config should not get a
    /// different answer than one who writes `::1`.
    ///
    /// Listeners must be composed with [`std::net::SocketAddr::new`] from this
    /// rather than by formatting `"{bind}:{port}"` — `"::1:9200"` is not a
    /// parseable socket address, which is how a `bind_address = "::1"` node
    /// used to fail at bind time with `invalid socket address syntax` after
    /// it had already created its data directory and printed a first-run link.
    pub fn bind_ip(&self) -> Option<std::net::IpAddr> {
        let raw = self.server.bind_address.trim();
        let raw = raw
            .strip_prefix('[')
            .and_then(|r| r.strip_suffix(']'))
            .unwrap_or(raw);
        raw.parse::<std::net::IpAddr>().ok()
    }

    /// `server.bind_address` and `port` as a bindable socket address.
    pub fn socket_addr(&self, port: u16) -> Option<std::net::SocketAddr> {
        self.bind_ip().map(|ip| std::net::SocketAddr::new(ip, port))
    }

    /// Is `server.bind_address` confined to the local host?
    ///
    /// Only a loopback literal counts. `0.0.0.0` and `::` are *unspecified*,
    /// not loopback — they bind every interface the host has, which is the
    /// exposure this predicate exists to detect. An address that does not
    /// parse is reported as not confined — it fails closed here, because a
    /// predicate that guards an exposure must never answer "safe" about a
    /// value it does not understand.
    ///
    /// That fail-closed answer is correct and is *not* a message: on its own
    /// it would have the startup refusals tell an operator who wrote
    /// `bind_address = "localhost"` that localhost "is not loopback", which is
    /// false, and prescribe an opt-out that only defers the failure. So
    /// `xerj-server/src/main.rs` rejects a non-IP `bind_address` at step 3a
    /// with [`Config::bind_ip`], before either exposure check runs; by the
    /// time this predicate is consulted there, the address is known to parse.
    pub fn bind_address_is_loopback(&self) -> bool {
        self.bind_ip()
            .map(crate::net::canonical_ip)
            .is_some_and(|ip| ip.is_loopback())
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

    /// Would starting now publish *every* listener in cleartext on a
    /// network-reachable interface? (issue #228)
    ///
    /// True when all three hold: TLS is off, the bind address is not confined
    /// to loopback, and the operator has not declared the exposure intentional
    /// via `server.allow_insecure_network_bind`. The caller's job is to refuse
    /// to start — see `xerj-server/src/main.rs`.
    ///
    /// This is the TLS-off half of the pair whose TLS-on half is
    /// [`Config::grpc_h2c_exposed_off_loopback`] (#229), and the two are
    /// mutually exclusive by construction: exactly one of them can fire for a
    /// given `tls.enabled`. The exposure here is strictly larger — with TLS
    /// off it is not one uncovered listener but all of them, carrying the
    /// `Authorization: ApiKey` header of every request.
    ///
    /// Same shape as ES's bootstrap checks (approach only, no code — AGPL):
    /// `BootstrapChecks.java:50-53` enforces "once a node has the transport
    /// protocol bound to a non-loopback interface … we assume the node is
    /// running in production", with one explicit override
    /// (`es.enforce.bootstrap.checks`, `:59`). Departures: the override lives
    /// in the config file beside the setting it relaxes rather than in a
    /// system property, and link-local counts as exposed here (ES's
    /// `enforceLimits`, `:194-197`, tests only `isLoopbackAddress`, but a
    /// link-local address is still reachable by every other host on the link).
    pub fn cleartext_exposed_off_loopback(&self) -> bool {
        !self.tls.enabled
            && !self.server.allow_insecure_network_bind
            && !self.bind_address_is_loopback()
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Sub-configs  (116 user-facing settings total; counted by
// `journey_zero_config`, not by hand)
// ═════════════════════════════════════════════════════════════════════════════

/// Network and data-directory settings.
///
/// **7 settings.**
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
    /// Address to bind all listeners (default: `"127.0.0.1"` — **loopback
    /// only**; issue #228).
    ///
    /// A fresh node is reachable from the machine it runs on and nowhere
    /// else. That is deliberate: TLS is off by default, so an out-of-the-box
    /// node that bound every interface would put its admin API key and every
    /// document body on the network in cleartext, and nothing about a working
    /// `curl` would say so.
    ///
    /// Set this to expose the node — `"0.0.0.0"` for every interface, or a
    /// specific private address. Doing so while `tls.enabled = false` refuses
    /// to start unless [`ServerConfig::allow_insecure_network_bind`] says the
    /// cleartext exposure is intended; see
    /// [`Config::cleartext_exposed_off_loopback`].
    pub bind_address: String,
    /// Permit a network-reachable `bind_address` while TLS is off
    /// (default: `false` — refuse; issue #228).
    ///
    /// Binding off-loopback with `tls.enabled = false` means every request —
    /// including the `Authorization: ApiKey` header — crosses the network in
    /// the clear. That is a legitimate configuration when something in front
    /// of the node terminates TLS (reverse proxy, sidecar, service mesh,
    /// ingress controller) or when the link itself is trusted, and it is what
    /// a container image has to do because the container's network namespace
    /// is the boundary. It is *not* something to arrive at by accident, so it
    /// has to be stated rather than defaulted into.
    ///
    /// Irrelevant when `bind_address` is loopback (nothing off-host can reach
    /// the listeners) or when `tls.enabled = true` (REST and ES-compat are
    /// encrypted; the separate gRPC h2c exposure is governed by
    /// [`TlsConfig::allow_insecure_grpc_h2c`], issue #229).
    pub allow_insecure_network_bind: bool,
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
            // Loopback, not `0.0.0.0` (issue #228). TLS is off by default, so
            // the shipped default must not be one that publishes an API key
            // in cleartext to every interface the host happens to have.
            bind_address: "127.0.0.1".into(),
            allow_insecure_network_bind: false,
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
    ///   durable) and are fsynced within `wal_batch_ms` of arriving, which
    ///   bounds the power-loss window. Good balance.
    ///
    ///   Issue #334: the fsync is scheduled per WAL shard by a shared,
    ///   bounded worker pool (`xerj_storage::wal_fsync`) that is woken by
    ///   the write itself.  It used to be a dedicated OS thread per index
    ///   polling on a timer, which cost a thread and `1000 / wal_batch_ms`
    ///   wakeups per second per index even when nothing was ever written.
    ///   The guarantee is unchanged (arguably tighter: the deadline is now
    ///   anchored to the write instead of to a free-running tick).
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
    /// #873: flush a NON-EMPTY memtable whose contents have not changed for
    /// this many seconds, regardless of size (default: `300` s; `0` disables).
    /// Without it a dataset below the size thresholds never reached a
    /// segment: its documents stayed memtable-resident for the process
    /// lifetime, pinned their WAL generations, and replayed on every boot —
    /// measured at 100,001 docs held in RAM 30+ minutes after ingest on an
    /// idle node. 300 s matches Elasticsearch's
    /// `indices.memory.shard_inactive_time` default. Detection is a probe on
    /// the periodic flusher (`flush_interval_secs`), so worst-case latency is
    /// `flush_idle_secs + flush_interval_secs` and the write path pays
    /// nothing.
    pub flush_idle_secs: u64,

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
            flush_idle_secs: 300,
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
/// **8 settings**, of which three are **not read by the merge path that runs**
/// — see [`MergeConfig::dormant_overrides`]. `run_merge_once`
/// (`xerj-engine/src/index.rs`) reads `max_segment_mb`, `tier_floor_mb`,
/// `min_merge_count` and `max_merge_count`, and nothing else from this struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MergeConfig {
    /// Merge strategy (default: `"size_tiered"`).
    ///
    /// Only `"size_tiered"` is implemented: `run_merge_once` always builds a
    /// `SizeTieredMergePolicy`. `"log_structured"` is a documented variant with
    /// no implementation behind it, so [`Config::validate`] rejects it rather
    /// than run size-tiered merging while the operator believes otherwise.
    pub strategy: MergeStrategy,
    /// Minimum number of segments to trigger a merge (default: `10`).
    ///
    /// **DORMANT — accepted and validated, but nothing reads it.** The real
    /// trigger is per-tier and comes from `min_merge_count`. Setting this away
    /// from its default logs a warning at startup (see
    /// [`MergeConfig::dormant_overrides`]).
    pub min_segments: u32,
    /// Maximum merged segment size in MiB (default: `8192` = 8 GiB).
    /// Segments at or above this size are excluded from further merges.
    pub max_segment_mb: u64,
    /// I/O rate cap for merge operations in MiB/s (default: `100`).
    ///
    /// **DORMANT — accepted, but merges are not throttled.** The `RateLimiter`
    /// that honours this value is wired only into `xerj-storage`'s legacy
    /// `MergeExecutor`, which the engine does not use; the merge that actually
    /// runs (`run_merge_once`) never consults it. Setting it away from its
    /// default logs a warning at startup (see
    /// [`MergeConfig::dormant_overrides`]).
    pub io_rate_mb_per_sec: u64,
    /// Maximum number of concurrent merge operations (default: `1`).
    ///
    /// **DORMANT — accepted, but nothing reads it.** Merge parallelism comes
    /// from the `XERJ_MERGE_PARALLELISM` environment variable, which also
    /// defaults to 1. Setting this away from its default logs a warning at
    /// startup (see [`MergeConfig::dormant_overrides`]).
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

impl MergeConfig {
    /// Merge settings this build accepts but does not act on, reported as
    /// `("merge.io_rate_mb_per_sec", "what actually happens")` — and only when
    /// the operator has moved one away from its default, because leaving a
    /// dormant knob alone asks for nothing.
    ///
    /// Issue #207 found `io_rate_mb_per_sec` documented, validated and inert:
    /// the `RateLimiter` that honours it lives in `xerj-storage`'s legacy
    /// `MergeExecutor`, which the engine never constructs. `min_segments` and
    /// `max_concurrent` are in the same state. That is the accepted-and-ignored
    /// pattern tracked by #204, and the operator it hurts is the one throttling
    /// merges to protect query latency — they get no throttle and no signal.
    ///
    /// These stay accepted rather than becoming hard errors like
    /// `storage.backend`: the wrong value costs latency, not data, and
    /// `io_rate_mb_per_sec = 100` has shipped in `xerj.default.toml` since v0.1,
    /// so rejecting it would refuse to start on a config the project itself
    /// handed out. The caller (`xerj-server/src/main.rs`) logs each line at
    /// WARN. Wiring a real throttle into `run_merge_once` is the fix that makes
    /// this list shorter.
    pub fn dormant_overrides(&self) -> Vec<(&'static str, &'static str)> {
        let d = MergeConfig::default();
        let mut out = Vec::new();
        if self.io_rate_mb_per_sec != d.io_rate_mb_per_sec {
            out.push((
                "merge.io_rate_mb_per_sec",
                "merge I/O is not throttled in this build — the rate limiter is \
                 wired only into xerj-storage's unused MergeExecutor",
            ));
        }
        if self.min_segments != d.min_segments {
            out.push((
                "merge.min_segments",
                "nothing reads it — the merge trigger is per-tier and comes from \
                 merge.min_merge_count",
            ));
        }
        if self.max_concurrent != d.max_concurrent {
            out.push((
                "merge.max_concurrent",
                "nothing reads it — merge parallelism comes from the \
                 XERJ_MERGE_PARALLELISM environment variable",
            ));
        }
        out
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
    ///
    /// **Dormant.** Every durable artifact XERJ writes is a self-describing
    /// compressed envelope (`ZBS2` stored, `ZFM4` `.meta`, `ZPS1` `.post`,
    /// the zstd-flagged `.dv` columns); "off" would mean emitting the legacy
    /// uncompressed layouts, which only exist as read-side compatibility
    /// paths. Setting `false` warns at startup — see
    /// [`CompressionConfig::dormant_overrides`].
    pub enabled: bool,
    /// Compression level: `"fast"`, `"balanced"`, or `"best"` (default: `"balanced"`).
    ///
    /// Selects the Zstandard effort level used to re-encode a segment's
    /// durable artifacts **at merge time**. See [`CompressionLevel`] for the
    /// level each name maps to, and why flush ignores this knob.
    pub level: CompressionLevel,
    /// Number of documents per compressed block (default: `128`).
    ///
    /// **Dormant.** Kept because it has shipped in `xerj.default.toml` since
    /// v0.1, but XERJ's stored codec is columnar over the whole segment
    /// section rather than blocked by document count, so there is no
    /// doc-block for this to size. Validated against its documented 16–4096
    /// range and warned about when moved off the default — see
    /// [`CompressionConfig::dormant_overrides`].
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

impl CompressionConfig {
    /// The documented range for `block_size_docs`, quoted in
    /// `xerj.default.toml` and on `landing/docs/config.html`.
    pub const BLOCK_SIZE_DOCS_RANGE: std::ops::RangeInclusive<u32> = 16..=4096;

    /// Compression settings this build accepts but does not act on, in the
    /// same `("compression.key", "what actually happens")` shape as
    /// [`MergeConfig::dormant_overrides`], and reported the same way — only
    /// when the operator has moved one off its default.
    ///
    /// Issue #318 found the whole `[compression]` section inert: two servers
    /// differing only in `enabled` / `level` / `block_size_docs` produced
    /// byte-identical segments (`.seg` 261,481 / `.dv` 10,679 / `.post`
    /// 192,287 on a 3,000-doc corpus), with no error and no warning, while
    /// the docs described all three as live. `level` is now wired into the
    /// merge re-encode; these two are not, and stay accepted-but-warned for
    /// the reason `merge.io_rate_mb_per_sec` does — both ship non-default in
    /// no config the project itself hands out, and the cost of ignoring them
    /// is disk footprint, not data.
    pub fn dormant_overrides(&self) -> Vec<(&'static str, &'static str)> {
        let d = CompressionConfig::default();
        let mut out = Vec::new();
        if self.enabled != d.enabled {
            out.push((
                "compression.enabled",
                "durable artifacts are always compressed in this build — the \
                 stored (ZBS2), .meta (ZFM4), .post (ZPS1) and .dv envelopes \
                 have no uncompressed write path, only legacy read support",
            ));
        }
        if self.block_size_docs != d.block_size_docs {
            out.push((
                "compression.block_size_docs",
                "nothing reads it — the stored codec is columnar over the \
                 whole segment section, not blocked by document count",
            ));
        }
        out
    }
}

/// Compression level — the Zstandard effort applied when a segment's durable
/// artifacts are re-encoded at **merge**.
///
/// Three things about this enum are deliberate and were all decided against
/// what the docs used to promise (issue #318):
///
/// 1. **Merge only.** Flush stays pinned at [`CompressionLevel::Balanced`]'s
///    level 3 regardless of this setting. Raising the flush level to 19 is
///    exactly the regression recorded in
///    `engine/reports/2026-04-25T21-50-00_ingest_perf_regression_zstd19.md`
///    — 1.55 M docs/s peak collapsed to 21 K docs/s with 75 % of documents
///    rejected, because a ~50 ms segment flush became 5–10 s and tripped
///    back-pressure. Merge is off the ingest critical path; flush is not.
/// 2. **All three names are Zstandard**, not "LZ4 for fast". Every durable
///    envelope XERJ writes (`ZBS2`, `ZFM4`, `ZPS1`, zstd-flagged `.dv`) is
///    zstd-framed, and its magic is what the reader dispatches on; making
///    one level switch algorithm would mean writing the legacy LZ4 layouts
///    that survive only as read-compatibility paths. `"fast"` is therefore
///    zstd's own fastest level, not a different codec.
/// 3. **`"best"` is level 6, not 19.** The RFC #148 thread measured L6 at
///    −14.4 % stored / −14.3 % `.dv` / −15.4 % `.meta`, while L9 doubles the
///    decode window from 2.00 to 4.00 MiB and L19 takes it to 8.00 MiB — a
///    cost every streaming point-get pays on read, forever, for a ratio gain
///    that measurement did not show. Lucene and Elasticsearch land in the
///    same place from the other direction: ES's `best_compression` stored
///    fields format tops out at zstd **3** with a bigger block, not at a
///    high effort level (`Zstd814StoredFieldsFormat.java:38-46`, read for
///    approach only — that code is AGPL/SSPL/Elastic-2.0 and none of it is
///    copied here).
///
/// Decoding never depends on the level a segment was written at, so mixed
/// levels across segments — the normal state after changing this setting —
/// need no format flag and no migration. That is why XERJ does not need
/// Lucene's `Lucene90StoredFieldsFormat.MODE_KEY` segment attribute
/// (`Lucene90StoredFieldsFormat.java:113-137`, Apache-2.0): Lucene must
/// record the mode because BEST_SPEED and BEST_COMPRESSION are different
/// algorithms (LZ4 vs Deflate), whereas here they differ only in effort.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompressionLevel {
    /// Zstandard level 1 — maximum throughput, moderate ratio.
    Fast,
    /// Zstandard level 3 — good ratio with low CPU overhead. Also the level
    /// every flush uses, whatever this setting says.
    Balanced,
    /// Zstandard level 6 — best measured ratio per byte of decode window.
    Best,
}

impl CompressionLevel {
    /// The Zstandard level this name maps to.
    ///
    /// The single source of truth for the mapping: the encoder sites in
    /// `xerj-storage` and `xerj-fts` take an `i32` level, and
    /// `xerj-compress`'s codec factory routes through here too, so the
    /// meaning of "best" cannot drift between the config surface and the
    /// codecs.
    pub const fn zstd_level(self) -> i32 {
        match self {
            Self::Fast => 1,
            Self::Balanced => 3,
            Self::Best => 6,
        }
    }
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
    /// - `"scalar8"` — 8-bit scalar quantization — WIRED into the kNN serving
    ///   path, but it buys **precision, not memory**. A `scalar8` dense_vector
    ///   field scores candidates from 1-byte-per-dimension codes, so it has the
    ///   recall profile of int8 (measured recall@10 ≈ 0.998). It does NOT
    ///   reduce resident memory: the scan reads the full-precision vector from
    ///   `_source` and quantizes it per query, which is what keeps an updated
    ///   document from being scored on stale codes or a stale codebook (#371).
    ///   The ingest-time code array that would make a memory claim true is
    ///   tracked in #392. Typically opted into per field via
    ///   `index_options.type: int8_hnsw` on the mapping; this global default
    ///   applies the same scheme index-wide.
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
/// **19 settings.**
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
            onnx_intra_threads: crate::resource::threads_for(crate::resource::Workload::Latency),
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

/// Sentinel value of [`LimitsConfig::max_process_memory_mb`] meaning **AUTO**:
/// pick the process-memory cap from the machine size in binary-GiB steps
/// (8 / 16 / 32 GiB) rather than a flat number. It is the serde default, so an
/// omitted field resolves to AUTO; the governor intercepts it before any budget
/// math, so this literal never reaches a budget as a byte count. `u64::MAX` is
/// used because no operator sets a 16-EiB cap — it is a reserved, unmistakable
/// "not a real MiB value" marker, distinct from `0` (uncapped, the whole
/// machine) and every plausible explicit MiB ceiling.
pub const AUTO_PROCESS_MEMORY_MB: u64 = u64::MAX;

/// Resource limits.
///
/// **14 settings.**
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
    /// threshold. Set to `0` to disable the disk watermark. This threshold
    /// can also be raised or lowered at runtime, no restart required, via
    /// `PUT _cluster/settings` on the `cluster.routing.allocation.disk.watermark.flood_stage`
    /// persistent (or transient) key — e.g. `{"persistent":
    /// {"cluster.routing.allocation.disk.watermark.flood_stage": "97%"}}` —
    /// matching ES's own recovery flow; that override cannot re-enable the
    /// watermark if this config value is `0`.
    pub disk_flood_stage_percent: u8,
    /// Ceiling on the machine size every other memory budget is derived FROM,
    /// in MiB. **Omitted / default = AUTO** ([`AUTO_PROCESS_MEMORY_MB`]): XERJ
    /// picks the cap from the effective (cgroup-aware) memory it can actually
    /// see, in binary-GiB steps —
    ///
    ///   effective < 64 GiB             → 8 GiB cap
    ///   64 GiB ≤ effective < 128 GiB   → 16 GiB cap
    ///   effective ≥ 128 GiB            → 32 GiB cap
    ///
    /// so a laptop is never sized for ~20 GiB of memtables and a big server is
    /// no longer starved by a flat 8 GiB. Every budget in `ResourceGovernor`
    /// derives from `effective_memory_limit_bytes()` = min(cgroup limit, total
    /// system RAM); capping that ONE base is what makes every dependent budget
    /// shrink coherently instead of each growing its own ceiling.
    ///
    /// The cap only ever LOWERS the base — `min(machine, cap)`, never invented
    /// headroom: on a 40 GiB box the 8 GiB tier still yields 8, and under a
    /// smaller cgroup limit the smaller value wins. Two explicit escape hatches:
    ///
    ///   * `0`  → NO cap: derive every budget from the whole machine (the old
    ///     machine-proportional behaviour; a 64 GiB host then asks for
    ///     ~16 GiB of memtables before anything else allocates).
    ///   * `N`  (N > 0) → force a fixed ceiling of exactly N MiB, still min'd
    ///     with the machine.
    ///
    /// Override at runtime without editing config via the environment:
    /// `XERJ_MAX_PROCESS_MEMORY_MB=auto|off|unlimited|<MiB>` (env wins over this value;
    /// `off`/`unlimited` == `0`; a bare `0` is ambiguous and falls back here).
    pub max_process_memory_mb: u64,
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
            // AUTO: tier the cap (8/16/32 GiB) off the machine at governor
            // build time. 0 = uncapped (whole machine); N = a fixed MiB cap.
            max_process_memory_mb: AUTO_PROCESS_MEMORY_MB,
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
    /// Maximum concurrent shard flush finalizations (default: the maintenance
    /// share of the machine, `max(2, num_cpus / 8)` — see
    /// [`crate::resource::threads_for`]).  `XERJ_FIN_CONC` overrides it for
    /// one run.
    pub flush_workers: usize,
    /// Threads in the background merge pool (default: the maintenance share of
    /// the machine, `max(2, num_cpus / 8)`).  Merges are the one pool that is
    /// deliberately kept narrow: nobody waits on a merge, and an all-core merge
    /// pass was measured stalling ingest for 17.5 s.
    pub merge_workers: usize,
    /// Threads available to search segment fan-out — rayon's global pool
    /// (default: every core).  Search is the path a user waits on, so it gets
    /// the whole machine unless you deliberately hold cores back.
    pub search_workers: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        use crate::resource::{threads_for, Workload};
        let cpus = crate::resource::cores();
        Self {
            ingest_shards: (cpus / 2).max(1).next_power_of_two(),
            flush_workers: threads_for(Workload::Maintenance),
            merge_workers: threads_for(Workload::Maintenance),
            search_workers: threads_for(Workload::Latency),
        }
    }
}

impl EngineConfig {
    /// Upper bound on any of the worker knobs. Threads are not free: past this
    /// the request is far more likely to be a typo than an intention, and a
    /// silently clamped typo is the bug class in #204.
    pub const MAX_WORKERS: usize = 4096;

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
        // Every worker knob is validated, because every worker knob is now
        // wired to a pool: an out-of-range value must be refused at startup
        // rather than accepted and quietly replaced with something else.
        for (name, value) in [
            ("engine.flush_workers", self.flush_workers),
            ("engine.merge_workers", self.merge_workers),
            ("engine.search_workers", self.search_workers),
        ] {
            if value == 0 || value > Self::MAX_WORKERS {
                return Err(crate::XerjError::config(format!(
                    "{name} must be in the range 1..={}, got {value}",
                    Self::MAX_WORKERS
                )));
            }
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

/// ISM/ILM index-lifecycle-management background execution.
///
/// Drives `xerj_engine::lifecycle`'s tick: for every managed index, run the
/// current state's pending actions, then evaluate transitions in order and
/// move to the first one whose conditions are met.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LifecycleConfig {
    /// How often the background job ticks (default: 300 seconds = 5
    /// minutes — the same default OpenSearch ISM itself uses for
    /// `plugins.index_state_management.job_interval`).
    pub tick_interval_secs: u64,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            tick_interval_secs: 300,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// Single-node WAL tap — push a filtered subset of indices to an external
/// ES-compatible target (issue #320). **10 settings.**
///
/// This is deliberately *not* cross-cluster replication. It is one
/// directional, single node, and target-agnostic because the wire format is
/// just `_bulk`: an Elasticsearch cluster, an OpenSearch cluster, or another
/// xerj node all work.
///
/// Semantics that matter before you turn it on:
///
/// - **No backfill.** An index whose WAL has never been pruned (a new one) is
///   shipped whole. An index that has been running long enough to prune
///   starts from the moment it is allowlisted: it ships what happens next,
///   not what is already in segments. Use snapshot/restore to seed the target
///   first if you need the existing data.
/// - **At-least-once.** A batch is re-sent if the cursor could not be
///   advanced, so the target may see a document twice. `doc_id` is the
///   `_bulk` `_id` and each action carries `version_type: external` with the
///   entry's `seq_no`, so a redelivery (and an out-of-order one) is a no-op
///   on any target that honours external versioning.
/// - **Retention never waits for the target.** WAL generations are pruned as
///   soon as their entries are durable in a segment, whether or not the tap
///   has shipped them — coupling the two would let a slow remote fill the
///   local disk. A tap that falls that far behind loses entries and *says
///   so*: `gaps` in `GET /_xerj/wal_tap/_stats`, plus a warning per gap.
///   `min_retained_generations` buys a **bounded** amount of slack; it is a
///   floor, not a lease, so a stalled target still cannot fill the disk.
/// - **System indices are never shipped**, whatever the allowlist says.
/// - **Runtime edits are durable.** `PUT /_xerj/wal_tap` persists the patched
///   configuration next to the cursors and re-applies it over the file
///   config on the next boot (`DELETE /_xerj/wal_tap` drops the overlay and
///   reverts to this file). Otherwise a restart would silently revert to
///   `enabled = false` while the cursors froze and WAL pruning continued.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WalTapConfig {
    /// Master switch (default: `false` — off).
    pub enabled: bool,
    /// Base URL of the target cluster, e.g. `"https://central:9200"`. The
    /// tap POSTs to `{target_url}/_bulk`. Empty disables the tap even when
    /// `enabled = true`.
    pub target_url: String,
    /// Verbatim `Authorization` header value for the target, e.g.
    /// `"ApiKey abc123"` or `"Basic dXNlcjpwdw=="`. Empty sends none.
    /// Never echoed back by the REST surface.
    pub target_auth: String,
    /// Index allowlist. Glob patterns (`*` only, as in `_cat` expressions);
    /// empty ships nothing. `["*"]` ships every non-system index.
    pub indices: Vec<String>,
    /// How often each index's WAL is polled, in milliseconds (default:
    /// `500`). This is the floor on end-to-end latency.
    pub poll_interval_ms: u64,
    /// Maximum WAL entries in one `_bulk` request (default: `1000`).
    pub max_batch_docs: usize,
    /// Maximum `_bulk` body size in bytes (default: 5 MiB). A single
    /// document larger than this is still sent, alone.
    pub max_batch_bytes: usize,
    /// Per-request timeout against the target, in seconds (default: `30`).
    pub request_timeout_secs: u64,
    /// Ceiling on the exponential retry backoff, in seconds (default: `60`).
    pub max_retry_backoff_secs: u64,
    /// Rotated WAL generations kept per shard **after** every entry in them
    /// is durable in a segment, so a tap whose target is briefly unreachable
    /// still finds them (default: `0` — prune as soon as it is safe, the
    /// pre-#320 behaviour).
    ///
    /// The default loses data on an outage longer than one flush interval
    /// (`storage.flush_interval_secs`, 30 s): the entries are gone from the
    /// WAL and the tap reports `gaps`. Set this to cover the outage you want
    /// to survive — `2` buys roughly two flush windows.
    ///
    /// The cost is bounded and paid whether or not a tap is running: at most
    /// `n * storage.wal_max_size_mb` extra megabytes per WAL shard per index.
    /// It is deliberately **not** an Elasticsearch-style retention lease,
    /// which holds generations for as long as a follower is behind and is how
    /// a dead follower wedges a leader's disk.
    ///
    /// Unlike every other field here, this one does not live in the tap: it
    /// lives in each index's `WalWriter`. `PUT /_xerj/wal_tap` therefore
    /// pushes it onto the open writers itself
    /// (`Engine::apply_wal_retention_floor`) and reports how many it reached,
    /// rather than acknowledging a value that reaches nothing.
    pub min_retained_generations: u64,
}

impl Default for WalTapConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target_url: String::new(),
            target_auth: String::new(),
            indices: Vec::new(),
            poll_interval_ms: 500,
            max_batch_docs: 1000,
            max_batch_bytes: 5 * 1024 * 1024,
            request_timeout_secs: 30,
            max_retry_backoff_secs: 60,
            min_retained_generations: 0,
        }
    }
}

impl WalTapConfig {
    /// Largest `min_retained_generations` this node accepts.
    ///
    /// The knob costs `n × storage.wal_max_size_mb` of disk per WAL shard per
    /// index whether or not a tap is running, so an operator who types an
    /// extra digit must not be able to fill the disk quietly.
    pub const MAX_RETAINED_GENERATIONS: u64 = 64;
    /// Accepted range for `poll_interval_ms`.
    pub const POLL_INTERVAL_MS_RANGE: std::ops::RangeInclusive<u64> = 50..=60_000;
    /// Accepted range for `max_retry_backoff_secs` — one day is already far
    /// past any recovery window a `_bulk` target has, and the upper bound also
    /// keeps `secs × 1000` nowhere near `u64::MAX`.
    pub const MAX_RETRY_BACKOFF_SECS_RANGE: std::ops::RangeInclusive<u64> = 1..=86_400;

    /// Range-check every numeric knob, returning the operator message.
    ///
    /// Shared by `Config::validate` (the config file, at boot) and
    /// `PUT /_xerj/wal_tap` (the API), so the file cannot be used to get a
    /// value past the bound the API enforces.
    pub fn check_limits(&self) -> Result<(), String> {
        if !Self::POLL_INTERVAL_MS_RANGE.contains(&self.poll_interval_ms) {
            return Err(format!(
                "wal_tap.poll_interval_ms must be between {} and {}, got {}",
                Self::POLL_INTERVAL_MS_RANGE.start(),
                Self::POLL_INTERVAL_MS_RANGE.end(),
                self.poll_interval_ms
            ));
        }
        if !Self::MAX_RETRY_BACKOFF_SECS_RANGE.contains(&self.max_retry_backoff_secs) {
            return Err(format!(
                "wal_tap.max_retry_backoff_secs must be between {} and {} (one day), got {}",
                Self::MAX_RETRY_BACKOFF_SECS_RANGE.start(),
                Self::MAX_RETRY_BACKOFF_SECS_RANGE.end(),
                self.max_retry_backoff_secs
            ));
        }
        if self.min_retained_generations > Self::MAX_RETAINED_GENERATIONS {
            return Err(format!(
                "wal_tap.min_retained_generations must be at most {}: it holds that many \
                 rotated WAL files per shard per index, costing up to \
                 n * storage.wal_max_size_mb of disk each. Got {}",
                Self::MAX_RETAINED_GENERATIONS,
                self.min_retained_generations
            ));
        }
        if self.max_batch_docs == 0 {
            return Err("wal_tap.max_batch_docs must be at least 1".into());
        }
        if self.max_batch_bytes == 0 {
            return Err("wal_tap.max_batch_bytes must be at least 1".into());
        }
        if self.request_timeout_secs == 0 {
            return Err("wal_tap.request_timeout_secs must be at least 1".into());
        }
        Ok(())
    }

    /// Reject a `target_url` this node must not accept, returning the operator
    /// message for a `400`.
    ///
    /// Two rules, and the second one is a credential boundary:
    ///
    /// 1. It has to be an absolute `http://` / `https://` URL, because the tap
    ///    POSTs `{target_url}/_bulk` and a relative one silently targets
    ///    nothing.
    /// 2. **No userinfo.** `https://user:pass@host` is an ordinary URL and
    ///    `reqwest` turns its userinfo into a `Basic` `Authorization` header —
    ///    so it is `target_auth` wearing a disguise. `target_auth` is
    ///    write-only precisely so that "can call the admin API" never becomes
    ///    "holds the target's credential", and `target_url` is echoed by
    ///    `GET /_xerj/wal_tap`, by `_stats`, and by the boot log. Accepting
    ///    userinfo would defeat that in one line of config.
    pub fn check_target_url(url: &str) -> Result<(), String> {
        let trimmed = url.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let Some(rest) = trimmed
            .strip_prefix("http://")
            .or_else(|| trimmed.strip_prefix("https://"))
        else {
            return Err(
                "wal_tap.target_url must be an absolute http:// or https:// URL, e.g. \
                 \"https://central:9200\""
                    .to_string(),
            );
        };
        // Userinfo is everything before the first `@` of the authority, which
        // ends at the first `/`, `?` or `#`.
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.contains('@') {
            return Err("wal_tap.target_url must not carry credentials in the URL \
                 (user:password@host): target_url is echoed by GET /_xerj/wal_tap, by \
                 /_xerj/wal_tap/_stats and in the server log. Put the credential in \
                 wal_tap.target_auth, which is write-only."
                .to_string());
        }
        Ok(())
    }

    /// `target_url` as it is safe to show: any userinfo replaced by `***`.
    ///
    /// [`check_target_url`](Self::check_target_url) refuses userinfo on the way
    /// in, so this only fires for a URL that reached the process another way —
    /// an older state file, or a hand-edited config on a node whose operator
    /// skipped validation. Belt and braces on a credential is cheap.
    pub fn redacted_target_url(&self) -> String {
        redact_url_userinfo(&self.target_url)
    }
}

/// Replace `scheme://user:pass@host/…` with `scheme://***@host/…`.
pub fn redact_url_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    match authority.rsplit_once('@') {
        Some((_userinfo, host)) => format!("{scheme}://***@{host}{tail}"),
        None => url.to_string(),
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

    /// #320 — the config file must not be the way around the bounds
    /// `PUT /_xerj/wal_tap` enforces.
    ///
    /// `min_retained_generations` is refused above 64 by the API because the
    /// knob costs `n × storage.wal_max_size_mb` per WAL shard per index
    /// whether or not a tap is running; `xerj.toml` took any `u64` in silence
    /// and the node then held that many rotated generations per shard forever.
    /// `max_retry_backoff_secs` is the same shape and worse: values above
    /// `u64::MAX / 1000` used to reach an unchecked `* 1000` inside
    /// `WalTap::arm_backoff`.
    ///
    /// Same precedent as `compression.block_size_docs` (#318): an
    /// out-of-range value is a typo the operator wants to hear about at boot,
    /// not as a disk-full page later.
    #[test]
    fn wal_tap_numeric_knobs_are_range_checked_in_the_config_file_too() {
        let bad = [
            ("min_retained_generations = 100", "min_retained_generations"),
            ("max_retry_backoff_secs = 0", "max_retry_backoff_secs"),
            (
                "max_retry_backoff_secs = 18446744073709552",
                "max_retry_backoff_secs",
            ),
            ("poll_interval_ms = 10", "poll_interval_ms"),
            ("poll_interval_ms = 60001", "poll_interval_ms"),
            ("max_batch_docs = 0", "max_batch_docs"),
            ("max_batch_bytes = 0", "max_batch_bytes"),
            ("request_timeout_secs = 0", "request_timeout_secs"),
        ];
        for (line, field) in bad {
            // `from_toml_str` validates, so a bad file is refused at load —
            // the node never boots with it.
            let toml = format!("[wal_tap]\n{line}\n");
            let err = Config::from_toml_str(&toml)
                .err()
                .unwrap_or_else(|| panic!("[wal_tap] {line} must be refused at boot"));
            assert!(
                err.to_string().contains(field),
                "the error must name the field the operator typed ({field}): {err}"
            );
        }

        // The bounds themselves are accepted, so this is a range check and not
        // an accidental ban.
        for line in [
            "min_retained_generations = 64",
            "max_retry_backoff_secs = 86400",
            "max_retry_backoff_secs = 1",
            "poll_interval_ms = 50",
            "poll_interval_ms = 60000",
        ] {
            let toml = format!("[wal_tap]\n{line}\n");
            Config::from_toml_str(&toml)
                .unwrap_or_else(|e| panic!("[wal_tap] {line} is in range but was refused: {e}"));
        }
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

    /// Flatten a serialised config to `("merge.max_segment_mb", 8192)` pairs.
    fn flatten(prefix: &str, value: &serde_json::Value, out: &mut Vec<(String, String)>) {
        match value {
            serde_json::Value::Object(fields) => {
                for (name, v) in fields {
                    let path = if prefix.is_empty() {
                        name.clone()
                    } else {
                        format!("{prefix}.{name}")
                    };
                    flatten(&path, v, out);
                }
            }
            other => out.push((prefix.to_string(), other.to_string())),
        }
    }

    /// The shipped file opens with "documents … their production-ready
    /// defaults", so every value it sets must BE the default — otherwise
    /// `cp xerj.default.toml xerj.toml`, the documented first step, silently
    /// changes the engine's behaviour and the file becomes a second, divergent
    /// source of truth for every number it quotes.
    ///
    /// It had already drifted when #207 was filed: the file said
    /// `max_segment_mb = 5120` against a real default of 8192, so copying it
    /// shrank the largest mergeable segment by 37% without saying so.
    #[test]
    fn shipped_default_config_documents_the_real_defaults() {
        let toml_src = include_str!("../../../xerj.default.toml");
        let from_file = serde_json::to_value(Config::from_toml_str(toml_src).unwrap()).unwrap();
        let defaults = serde_json::to_value(Config::default()).unwrap();

        let (mut a, mut b) = (Vec::new(), Vec::new());
        flatten("", &from_file, &mut a);
        flatten("", &defaults, &mut b);

        let drift: Vec<String> = a
            .iter()
            .zip(b.iter())
            .filter(|((_, file), (_, code))| file != code)
            .map(|((key, file), (_, code))| format!("{key}: file says {file}, code says {code}"))
            .collect();
        assert!(
            drift.is_empty(),
            "engine/xerj.default.toml claims to document the shipped defaults but \
             disagrees with Config::default() on {} setting(s):\n  {}",
            drift.len(),
            drift.join("\n  ")
        );

        // …and the file's own header quotes how many of the 116 it sets. That
        // number was 38, then 56, and never once the truth (#207), so count the
        // assignments instead of trusting the sentence.
        let set_here = toml_src
            .lines()
            .filter(|l| {
                let l = l.trim_start();
                !l.starts_with('#')
                    && l.split_once('=')
                        .is_some_and(|(k, _)| !k.trim().is_empty() && !k.contains('['))
            })
            .count();
        let total: usize = SETTINGS_BY_SECTION.iter().map(|(_, c)| c).sum();
        let claim = format!("sets {set_here} of XERJ's {total} user-facing settings");
        assert!(
            toml_src.contains(&claim),
            "xerj.default.toml's header must say {claim:?} — it sets {set_here} keys"
        );
    }

    /// Undo the four HTML entities the docs pages actually use.
    fn html_unescape(s: &str) -> String {
        s.replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
    }

    /// Render a serialised default the way `config.html` writes it: strings
    /// quoted, empty arrays as `[]`, numbers and bools bare.
    fn as_documented(value: &serde_json::Value) -> String {
        match value {
            serde_json::Value::String(s) => format!("\"{s}\""),
            serde_json::Value::Array(items) if items.is_empty() => "[]".to_string(),
            other => other.to_string(),
        }
    }

    /// The reference table on `landing/docs/config.html`, as
    /// `(section, key, documented default)` in page order.
    fn docs_default_table(page: &str) -> Vec<(String, String, String)> {
        const GROUP: &str = "<div class=\"group\" id=\"";
        const CELL_K: &str = "<div class=\"cell k\">";
        const CELL_D: &str = "<div class=\"cell d\">";

        let mut rows = Vec::new();
        let mut section = String::new();
        let mut rest = page;
        loop {
            let group_at = rest.find(GROUP);
            let key_at = rest.find(CELL_K);
            let take_group = match (group_at, key_at) {
                (Some(g), Some(k)) => g < k,
                (Some(_), None) => true,
                _ => false,
            };
            if take_group {
                let after = &rest[group_at.unwrap() + GROUP.len()..];
                let (id, tail) = after.split_once('"').expect("group id must be quoted");
                section = id.to_string();
                rest = tail;
            } else if let Some(k) = key_at {
                let after = &rest[k + CELL_K.len()..];
                let (key, tail) = after.split_once("</div>").expect("cell k must close");
                let d = tail
                    .find(CELL_D)
                    .expect("every key cell has a default cell");
                let (default, tail) = tail[d + CELL_D.len()..]
                    .split_once("</div>")
                    .expect("cell d must close");
                rows.push((
                    section.clone(),
                    key.trim().to_string(),
                    html_unescape(default).trim().to_string(),
                ));
                rest = tail;
            } else {
                return rows;
            }
        }
    }

    /// Split `"batched"    # durability knob` into value and comment, without
    /// mistaking a `#` inside a quoted string for the comment marker.
    fn split_value_and_comment(rhs: &str) -> (&str, &str) {
        let rhs = rhs.trim();
        let end = if let Some(inner) = rhs.strip_prefix('"') {
            inner.find('"').map(|i| i + 2).unwrap_or(rhs.len())
        } else {
            rhs.find('#').unwrap_or(rhs.len())
        };
        let (value, comment) = rhs.split_at(end);
        (value.trim(), comment.trim())
    }

    /// The docs site is the *other* copy of the defaults, and it drifted
    /// exactly the way `xerj.default.toml` did (#207).
    ///
    /// `landing/docs/config.html` shipped `wal_max_size_mb = 512`,
    /// `flush_size_mb = 256` and `default_quantization = "scalar8"` in its
    /// copy-pasteable `[storage]` and `[vector]` blocks while its own DEFAULT
    /// table — thirty lines above, on the same page — said 1024, 512 and
    /// `"none"`. An operator pasting the `[vector]` block switched on 8-bit
    /// quantization and its 1–2% recall loss without asking for it.
    /// `shipped_default_config_documents_the_real_defaults` did not catch it
    /// because it only reads the TOML, so read the page too:
    ///
    /// 1. every DEFAULT cell in the reference table must equal
    ///    `Config::default()`, and
    /// 2. every assignment in an example block must equal the real default —
    ///    unless its comment says `not a default`, which is how the blocks
    ///    that exist to *turn something on* (TLS, cluster, embedding) declare
    ///    themselves to the reader as well as to this test.
    #[test]
    fn the_docs_site_config_page_agrees_with_the_real_defaults() {
        let page = include_str!("../../../../landing/docs/config.html");
        let defaults = serde_json::to_value(Config::default()).unwrap();
        let table = docs_default_table(page);
        assert!(
            table.len() > 40,
            "only parsed {} rows out of the config.html reference table — the page's \
             markup changed and this guard is no longer reading it",
            table.len()
        );

        let mut drift: Vec<String> = Vec::new();
        for (section, key, documented) in &table {
            match defaults.get(section).and_then(|s| s.get(key)) {
                None => drift.push(format!(
                    "table: [{section}] {key} is documented but is not a key in Config"
                )),
                Some(actual) => {
                    let real = as_documented(actual);
                    if &real != documented {
                        drift.push(format!(
                            "table: [{section}] {key} says {documented}, code says {real}"
                        ));
                    }
                }
            }
        }

        for block in page.split("<pre class=\"code\">").skip(1) {
            let block = html_unescape(block.split("</pre>").next().unwrap_or_default());
            let mut section = String::new();
            for line in block.lines() {
                let line = line.trim();
                if line.starts_with('#') {
                    continue;
                }
                if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                    section = name.to_string();
                    continue;
                }
                let Some((key, rhs)) = line.split_once('=') else {
                    continue;
                };
                let key = key.trim();
                if key.is_empty()
                    || !key
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                {
                    continue;
                }
                if section.is_empty() {
                    // Skipping quietly here is how a whole block escapes the
                    // check. Only a line that names a real setting matters, so
                    // report exactly those and stay quiet about `key = value`
                    // text that is not config at all.
                    if defaults
                        .as_object()
                        .is_some_and(|cfg| cfg.values().any(|s| s.get(key).is_some()))
                    {
                        drift.push(format!(
                            "example: `{key} = …` sits in a block with no [section] header \
                             above it, so nothing can check it against Config::default() — \
                             add the header"
                        ));
                    }
                    continue;
                }
                let Some(actual) = defaults.get(&section).and_then(|s| s.get(key)) else {
                    drift.push(format!(
                        "example: [{section}] {key} is not a key in Config — a reader pasting \
                         this block gets `unknown field` and no server"
                    ));
                    continue;
                };
                let (value, comment) = split_value_and_comment(rhs);
                let real = as_documented(actual);
                if value != real && !comment.contains("not a default") {
                    drift.push(format!(
                        "example: [{section}] {key} = {value} contradicts the real default \
                         {real} and does not say so — add `# not a default ({real})` to the \
                         line if the example means it"
                    ));
                }
            }
        }

        assert!(
            drift.is_empty(),
            "landing/docs/config.html disagrees with Config::default() in {} place(s):\n  {}",
            drift.len(),
            drift.join("\n  ")
        );
    }

    /// `merge.strategy = "log_structured"` used to parse, validate and then run
    /// size-tiered merging anyway — nothing in the tree reads the field (#207).
    /// An operator who picks a levelled policy for its read amplification must
    /// hear that it does not exist, not get the other one silently.
    #[test]
    fn the_unimplemented_merge_strategy_is_rejected_not_silently_substituted() {
        let err = Config::from_toml_str("[merge]\nstrategy = \"log_structured\"\n")
            .expect_err("log_structured must be refused while it is unimplemented")
            .to_string();
        assert!(
            err.contains("merge.strategy") && err.contains("not implemented"),
            "the error must name the setting and say why: {err}"
        );

        Config::from_toml_str("[merge]\nstrategy = \"size_tiered\"\n")
            .expect("the implemented policy must still be accepted");
    }

    /// The three merge knobs this build accepts without acting on. Silence was
    /// the bug (#207): the operator throttling merges to protect query latency
    /// got no throttle and no signal.
    #[test]
    fn dormant_merge_settings_are_named_only_when_an_operator_sets_them() {
        assert!(
            MergeConfig::default().dormant_overrides().is_empty(),
            "an untouched default asks for nothing, so it must not warn"
        );

        for (toml, expected) in [
            ("io_rate_mb_per_sec = 250", "merge.io_rate_mb_per_sec"),
            ("min_segments = 4", "merge.min_segments"),
            ("max_concurrent = 4", "merge.max_concurrent"),
        ] {
            let cfg = Config::from_toml_str(&format!("[merge]\n{toml}\n")).unwrap();
            let named: Vec<&str> = cfg
                .merge
                .dormant_overrides()
                .into_iter()
                .map(|(key, _)| key)
                .collect();
            assert_eq!(
                named,
                vec![expected],
                "setting {toml} must be reported, and nothing else"
            );
        }

        // A setting that IS read by `run_merge_once` must never be reported.
        let cfg = Config::from_toml_str("[merge]\nmax_segment_mb = 4096\n").unwrap();
        assert!(
            cfg.merge.dormant_overrides().is_empty(),
            "max_segment_mb reaches the merge policy, so it is not dormant"
        );
    }

    /// #318 — the `[compression]` section reached no encoder at all: two
    /// nodes differing only in `enabled` / `level` / `block_size_docs` wrote
    /// byte-identical segments, with no error and no warning, while the docs
    /// called all three live. `level` is wired now (see the engine-side merge
    /// test); the other two are dormant, and dormant must be *audible*.
    #[test]
    fn dormant_compression_settings_are_named_only_when_an_operator_sets_them() {
        assert!(
            CompressionConfig::default().dormant_overrides().is_empty(),
            "an untouched default asks for nothing, so it must not warn"
        );

        for (toml, expected) in [
            ("enabled = false", "compression.enabled"),
            ("block_size_docs = 512", "compression.block_size_docs"),
        ] {
            let cfg = Config::from_toml_str(&format!("[compression]\n{toml}\n")).unwrap();
            let named: Vec<&str> = cfg
                .compression
                .dormant_overrides()
                .into_iter()
                .map(|(key, _)| key)
                .collect();
            assert_eq!(
                named,
                vec![expected],
                "setting {toml} must be reported, and nothing else"
            );
        }

        // `level` now reaches the merge re-encode, so it must NOT be reported
        // — a warning on a knob that works is the same lie in the other
        // direction.
        let cfg = Config::from_toml_str("[compression]\nlevel = \"best\"\n").unwrap();
        assert!(
            cfg.compression.dormant_overrides().is_empty(),
            "compression.level reaches the merge encoder, so it is not dormant"
        );
    }

    /// The documented 16–4096 range was never enforced: #318's repro node
    /// booted happily on `block_size_docs = 999999`.
    #[test]
    fn out_of_range_block_size_docs_is_refused_at_startup() {
        for bad in [15u32, 4097, 999_999, 0] {
            // Rejected on the path a real boot takes — `from_toml_str`
            // validates, so #318's `block_size_docs = 999999` node would not
            // have started.
            let err = Config::from_toml_str(&format!("[compression]\nblock_size_docs = {bad}\n"))
                .unwrap_err();
            assert!(
                err.to_string().contains("compression.block_size_docs"),
                "the error must name the setting, got: {err}"
            );

            // And directly, so the check cannot be lost by a refactor that
            // moves parsing off `validate`.
            let mut cfg = Config::default();
            cfg.compression.block_size_docs = bad;
            assert!(cfg.validate().is_err(), "{bad} must fail validate()");
        }

        for ok in [16u32, 128, 4096] {
            Config::from_toml_str(&format!("[compression]\nblock_size_docs = {ok}\n"))
                .unwrap_or_else(|e| panic!("block_size_docs = {ok} is in range, got: {e}"));
        }
    }

    /// The zstd level behind each name. Pinned because these numbers are
    /// quoted in `xerj.default.toml` and on `landing/docs/compression.html`,
    /// and because `"best"` deliberately means 6 rather than the 19 the enum
    /// used to document — see the doc comment for the measurements.
    #[test]
    fn compression_level_names_map_to_the_documented_zstd_levels() {
        assert_eq!(CompressionLevel::Fast.zstd_level(), 1);
        assert_eq!(CompressionLevel::Balanced.zstd_level(), 3);
        assert_eq!(CompressionLevel::Best.zstd_level(), 6);

        // The default must stay byte-for-byte what flush already writes, so
        // that adopting this build changes nothing for an operator who never
        // touched the section.
        assert_eq!(
            CompressionConfig::default().level.zstd_level(),
            3,
            "the default level must equal the pinned flush level"
        );
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

    /// Leaf keys in a serialised value — one per setting a user can set.
    /// Sub-configs are grouping, not settings, so only leaves count.
    fn count_settings(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(fields) => fields.values().map(count_settings).sum(),
            _ => 1,
        }
    }

    /// Settings per section, and the total. Every one of these numbers is
    /// quoted in a doc comment above the matching struct (and the total in the
    /// module header, in `xerj-common/src/lib.rs`, in `engine/README.md` and in
    /// `xerj.default.toml`) — change the table and the prose together.
    const SETTINGS_BY_SECTION: &[(&str, usize)] = &[
        ("server", 7),
        ("auth", 3),
        ("cors", 2),
        ("tls", 4),
        ("storage", 10),
        ("merge", 8),
        ("compression", 3),
        ("fts", 1),
        ("vector", 6),
        ("logs", 2),
        ("embedding", 19),
        ("limits", 14),
        ("indexing", 3),
        ("engine", 4),
        ("cluster", 5),
        ("pit", 3),
        ("search_context", 7),
        ("logging", 2),
        ("compat", 2),
        ("lifecycle", 1),
        ("wal_tap", 10),
    ];

    /// Count the settings by *counting them*.
    ///
    /// This test used to read
    ///
    /// ```ignore
    /// let total = 6 + 3 + 4 + 10 + 5 + 3 + 1 + 6 + 2 + 4 + 12 + 3 + 2;
    /// assert_eq!(total, 61);
    /// ```
    ///
    /// — the identity `61 == 61`, which could not fail whatever `Config`
    /// actually held. By the time #207 was filed it had drifted so far that the
    /// tree quoted 38, 50, 56, 60 and 61 in five places, none of them right.
    /// `Config` is `Serialize` and every field defaults, so the honest count is
    /// the number of leaf keys in a default config.
    #[test]
    fn count_user_facing_settings() {
        let cfg = serde_json::to_value(Config::default()).expect("Config serialises");
        let sections = cfg.as_object().expect("Config is a table");

        let measured: Vec<(String, usize)> = sections
            .iter()
            .map(|(name, v)| (name.clone(), count_settings(v)))
            .collect();
        let expected: Vec<(String, usize)> = SETTINGS_BY_SECTION
            .iter()
            .map(|(n, c)| ((*n).to_string(), *c))
            .collect();
        assert_eq!(
            measured, expected,
            "the per-section settings counts changed. That is fine — but each is \
             quoted in the doc comment above its struct, so update both"
        );

        let total: usize = SETTINGS_BY_SECTION.iter().map(|(_, c)| c).sum();
        assert_eq!(
            total,
            count_settings(&cfg),
            "the section table must sum to the whole config"
        );
        assert_eq!(
            total, 116,
            "the total settings count changed. It is quoted in this module's \
             header, in xerj-common/src/lib.rs, in engine/README.md, in \
             xerj.default.toml and in EXPECTED_SETTINGS in \
             xerj-engine/tests/product_experience.rs — update all of them"
        );
    }

    // ── gRPC h2c exposure: fail closed (issue #229) ──────────────────────────

    /// `0.0.0.0` binds every interface, so "TLS on, bound to the world" is
    /// exactly the case that must be caught — this is the assertion that
    /// fails without the #229 fix.
    ///
    /// It used to reach `0.0.0.0` by saying nothing, because that was the
    /// shipped default; #228 made the default loopback, so the exposure is
    /// now written out. That is the point of the change, not an accident of
    /// it: this configuration has to be typed to happen.
    #[test]
    fn tls_on_with_exposed_bind_flags_grpc_h2c_exposure() {
        let cfg = Config::from_toml_str(
            "[server]\nbind_address = \"0.0.0.0\"\n\
             [tls]\nenabled = true\ncert_path = \"/c.pem\"\nkey_path = \"/k.pem\"\n",
        )
        .unwrap();
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

    // ── Cleartext network exposure: fail closed (issue #228) ─────────────────

    /// The whole point of #228: an operator who configures nothing gets a node
    /// only their own machine can reach. Before the fix this default was
    /// `0.0.0.0`, so a first `xerj` run published its admin API key in
    /// cleartext to every interface the host had.
    #[test]
    fn default_bind_is_loopback() {
        let cfg = Config::default();
        assert_eq!(cfg.server.bind_address, "127.0.0.1");
        assert!(cfg.bind_address_is_loopback());
        assert!(
            !cfg.tls.enabled,
            "TLS is off by default — that is the point"
        );
        assert!(
            !cfg.cleartext_exposed_off_loopback(),
            "a zero-config node must boot, not fail closed"
        );
        // An empty config file resolves to the same posture as no file.
        let from_toml = Config::from_toml_str("").unwrap();
        assert_eq!(from_toml.server.bind_address, "127.0.0.1");
    }

    /// Exposing every listener in cleartext must be stated, not stumbled into.
    /// `0.0.0.0` and `::` bind every interface; a link-local address is still
    /// reachable by every other host on the link; an unparseable address fails
    /// closed here and is rejected by the `SocketAddr` parse a moment later.
    #[test]
    fn cleartext_bind_off_loopback_trips_the_check() {
        for bind in [
            "0.0.0.0",
            "::",
            "10.0.0.7",
            "192.168.1.5",
            "fe80::1",
            "nope",
        ] {
            let cfg =
                Config::from_toml_str(&format!("[server]\nbind_address = \"{bind}\"\n")).unwrap();
            assert!(!cfg.tls.enabled);
            assert!(
                cfg.cleartext_exposed_off_loopback(),
                "{bind} with TLS off must trip"
            );
        }
    }

    /// Loopback binds are untouched — local development, the ES-YAML
    /// conformance harness and every `curl 127.0.0.1` quickstart keep working
    /// with TLS off. A fail-closed check that fires on the common case gets
    /// disabled wholesale.
    #[test]
    fn cleartext_bind_on_loopback_is_fine() {
        for bind in ["127.0.0.1", "127.0.0.5", "::1", "::ffff:127.0.0.1"] {
            let cfg =
                Config::from_toml_str(&format!("[server]\nbind_address = \"{bind}\"\n")).unwrap();
            assert!(
                !cfg.cleartext_exposed_off_loopback(),
                "{bind} must not trip"
            );
        }
    }

    /// Every loopback spelling must produce a *bindable* address, not merely
    /// pass the loopback predicate. `format!("{bind}:{port}")` cannot express
    /// an IPv6 literal — `"::1:9200"` does not parse — so a `::1` node used to
    /// pass every check here and then die at bind time with `invalid socket
    /// address syntax`, after creating its data directory and printing a
    /// first-run console link. Making loopback the default (#228) makes `::1`
    /// a spelling people will now actually reach for.
    #[test]
    fn loopback_spellings_produce_bindable_addresses() {
        for bind in ["127.0.0.1", "::1", "[::1]", " ::1 "] {
            let cfg =
                Config::from_toml_str(&format!("[server]\nbind_address = \"{bind}\"\n")).unwrap();
            assert!(cfg.bind_address_is_loopback(), "{bind} should be loopback");
            let addr = cfg
                .socket_addr(9200)
                .unwrap_or_else(|| panic!("{bind} must yield a socket address"));
            assert!(addr.ip().is_loopback());
            assert_eq!(addr.port(), 9200);
        }
        assert_eq!(
            Config::from_toml_str("[server]\nbind_address = \"::1\"\n")
                .unwrap()
                .socket_addr(9200)
                .unwrap()
                .to_string(),
            "[::1]:9200"
        );
    }

    /// A bind address that is not an IP has no socket address to offer, and
    /// says so rather than producing something that fails later.
    #[test]
    fn non_ip_bind_has_no_socket_addr() {
        let cfg = Config::from_toml_str("[server]\nbind_address = \"example.com\"\n").unwrap();
        assert!(cfg.bind_ip().is_none());
        assert!(cfg.socket_addr(9200).is_none());
        assert!(!cfg.bind_address_is_loopback(), "must fail closed");
    }

    /// The escape hatch for proxy/sidecar/mesh TLS termination and for
    /// container images, whose network namespace is the boundary.
    #[test]
    fn declared_insecure_bind_permits_the_exposure() {
        let cfg = Config::from_toml_str(
            "[server]\nbind_address = \"0.0.0.0\"\nallow_insecure_network_bind = true\n",
        )
        .unwrap();
        assert!(!cfg.cleartext_exposed_off_loopback());
    }

    /// With TLS on, REST and ES-compat are encrypted and this check has
    /// nothing to say — the residual gRPC h2c exposure is #229's job. The two
    /// predicates partition on `tls.enabled`, so they can never both fire.
    #[test]
    fn tls_on_is_the_other_checks_business() {
        let cfg = Config::from_toml_str(
            "[server]\nbind_address = \"0.0.0.0\"\n\
             [tls]\nenabled = true\ncert_path = \"/c.pem\"\nkey_path = \"/k.pem\"\n",
        )
        .unwrap();
        assert!(!cfg.cleartext_exposed_off_loopback());
        assert!(cfg.grpc_h2c_exposed_off_loopback());
        assert!(!(cfg.cleartext_exposed_off_loopback() && cfg.grpc_h2c_exposed_off_loopback()));
    }

    /// `allow_insecure_network_bind` relaxes only the cleartext check. It must
    /// not become a way to smuggle past #229's gRPC refusal as well, or one
    /// opt-out silently buys two exposures.
    #[test]
    fn insecure_bind_opt_out_does_not_relax_the_grpc_check() {
        let cfg = Config::from_toml_str(
            "[server]\nbind_address = \"0.0.0.0\"\nallow_insecure_network_bind = true\n\
             [tls]\nenabled = true\ncert_path = \"/c.pem\"\nkey_path = \"/k.pem\"\n",
        )
        .unwrap();
        assert!(cfg.grpc_h2c_exposed_off_loopback());
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
