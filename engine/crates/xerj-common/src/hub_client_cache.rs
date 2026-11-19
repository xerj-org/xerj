//! Hub-first stdio MCP runtime mode and bounded LRU disk cache (#2760, epic #2769).
//!
//! `xerj mcp` works as a short-lived stdio MCP server that agent hosts launch
//! per session without requiring a running local `xerj` daemon:
//!
//! 1. **Mode detection & routing ([`McpRuntimeMode`], [`HubFirstRouter`])**:
//!    Probes the configured local node within a strict per-session startup
//!    budget ([`DEFAULT_PROBE_BUDGET_MS`]). When reachable, private-index tools
//!    (`xerj_search`, `xerj_def`, `xerj_map`, …) route to the local node while
//!    hub tools remain available. When unreachable, `xerj mcp` falls back to
//!    [`McpRuntimeMode::HubOnlyFallback`] with an explicit notice, lists the
//!    hub/pack tools, and answers hub queries directly through the hub client
//!    and disk cache.
//! 2. **Bounded LRU disk cache ([`HubDiskCache`])**:
//!    Caches the verified catalogue and per-corpus pack records on disk via
//!    [`crate::fsio::write_file_durable`], enforces a configurable byte ceiling
//!    (`max_bytes`) with deterministic least-recently-used eviction, verifies
//!    SHA-256 content digests and 40-hex commit pins, and reports cache
//!    staleness ([`Freshness::Fresh`] vs. [`Freshness::Stale`]) explicitly on
//!    every response.
//! 3. **Loud errors ([`HubModeError`])**:
//!    Unknown corpus slugs, invalid slug syntax, excluded licences, Integrity
//!    mismatches, and unsupported catalogue or pack `format_version` values
//!    always return a typed error — never a silent empty result.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fsio::write_file_durable;
use crate::hub::{
    catalogue_format_supported, Answer, AnswerMode, AnsweredFrom, Catalogue, CatalogueEntry,
    DefRecord, Hit, LicenceClass, PackManifest, CATALOGUE_FORMAT_VERSION,
    CATALOGUE_FORMAT_VERSION_MIN, DEFAULT_HUB_URL, PACK_FORMAT_VERSION,
};

/// Default per-session local-node reachability probe budget in milliseconds.
pub const DEFAULT_PROBE_BUDGET_MS: u64 = 150;

/// Default disk cache budget for stdio MCP hub-only mode (256 MiB).
pub const DEFAULT_MCP_CACHE_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// Default staleness TTL in seconds before a cached corpus/catalogue is marked
/// [`Freshness::Stale`] (24 hours).
pub const DEFAULT_CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// Maximum corpus slug length in bytes (matches the hub server's `valid_slug` cap).
pub const MAX_SLUG_BYTES: usize = 64;

/// Canonical MCP tools served directly from the hub / without a local node.
pub const HUB_DIRECT_TOOLS: &[&str] = &[
    "xerj_hub_find",
    "xerj_hub_def",
    "xerj_hub_ask",
    "xerj_corpus_feedback",
    "xerj_field_report",
];

/// Canonical MCP tools that require a running local `xerj` node (private indices,
/// agent memory, link graph, autoindex catalog).
pub const LOCAL_NODE_TOOLS: &[&str] = &[
    "xerj_def",
    "xerj_search",
    "xerj_batch_search",
    "xerj_semantic_search",
    "xerj_vector_search",
    "xerj_hybrid_search",
    "xerj_memory_store",
    "xerj_memory_recall",
    "xerj_brain_ego",
    "xerj_brain_link",
    "xerj_brain_unlink",
    "xerj_brain_overview",
    "xerj_plan",
    "xerj_map",
    "xerj_status",
];

/// Typed errors for hub-only MCP routing, format checking, and disk caching.
///
/// Every failure mode is explicit and loud — unknown corpora or unsupported
/// format versions never degrade into empty search hits.
#[derive(Debug, thiserror::Error)]
pub enum HubModeError {
    #[error("invalid corpus slug '{slug}': must be 1..={MAX_SLUG_BYTES} bytes of lowercase ASCII letters, digits, '.', '_', or '-' and start with an alphanumeric character")]
    InvalidCorpusSlug { slug: String },

    #[error("unknown hub corpus '{slug}': not present in catalogue (generated_at {catalogue_generated_at})")]
    UnknownCorpus {
        slug: String,
        catalogue_generated_at: String,
    },

    #[error("unsupported {kind} format_version {found}: this build supports {supported_min}..={supported_max}")]
    UnsupportedFormatVersion {
        kind: &'static str,
        found: u32,
        supported_min: u32,
        supported_max: u32,
    },

    #[error(
        "corpus '{slug}' has excluded licence ({licence_spdx}) and cannot be fetched or served"
    )]
    ExcludedCorpusLicence { slug: String, licence_spdx: String },

    #[error("integrity verification failed for '{slug}': {reason}")]
    IntegrityMismatch { slug: String, reason: String },

    #[error("cache entry '{slug}' ({entry_bytes} bytes) exceeds total disk cache budget ({max_bytes} bytes)")]
    EntryExceedsCacheBudget {
        slug: String,
        entry_bytes: u64,
        max_bytes: u64,
    },

    #[error("tool '{tool}' requires a running local xerj node ({notice})")]
    LocalNodeRequired { tool: String, notice: String },

    #[error("hub fetch failed for '{target}': {reason}")]
    HubFetch { target: String, reason: String },

    #[error("hub disk cache I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("hub disk cache serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Validate a corpus slug against the hub's strict naming rules.
pub fn valid_corpus_slug(slug: &str) -> bool {
    let bytes = slug.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_SLUG_BYTES {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(*b, b'.' | b'_' | b'-'))
}

/// Validate a 40-character lowercase hexadecimal Git SHA-1 commit pin.
pub fn valid_pin_sha(sha: &str) -> bool {
    sha.len() == 40
        && sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Validate a 64-character lowercase hexadecimal SHA-256 digest.
pub fn valid_sha256_hex(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Compute lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Verify that a [`Catalogue`] has a supported `format_version` and valid entries.
pub fn validate_catalogue_format(catalogue: &Catalogue) -> Result<(), HubModeError> {
    if !catalogue_format_supported(catalogue.format_version) {
        return Err(HubModeError::UnsupportedFormatVersion {
            kind: "catalogue",
            found: catalogue.format_version,
            supported_min: CATALOGUE_FORMAT_VERSION_MIN,
            supported_max: CATALOGUE_FORMAT_VERSION,
        });
    }
    for entry in &catalogue.corpora {
        if !valid_corpus_slug(&entry.slug) {
            return Err(HubModeError::InvalidCorpusSlug {
                slug: entry.slug.clone(),
            });
        }
        if !valid_pin_sha(&entry.pin_sha) {
            return Err(HubModeError::IntegrityMismatch {
                slug: entry.slug.clone(),
                reason: format!("invalid 40-hex pin_sha '{}'", entry.pin_sha),
            });
        }
        if !valid_sha256_hex(&entry.pack_sha256) {
            return Err(HubModeError::IntegrityMismatch {
                slug: entry.slug.clone(),
                reason: format!("invalid 64-hex pack_sha256 '{}'", entry.pack_sha256),
            });
        }
    }
    Ok(())
}

/// Verify that a [`PackManifest`] has a supported `format_version` and valid pin.
pub fn validate_pack_manifest_format(manifest: &PackManifest) -> Result<(), HubModeError> {
    if manifest.format_version != PACK_FORMAT_VERSION {
        return Err(HubModeError::UnsupportedFormatVersion {
            kind: "pack_manifest",
            found: manifest.format_version,
            supported_min: PACK_FORMAT_VERSION,
            supported_max: PACK_FORMAT_VERSION,
        });
    }
    if !valid_corpus_slug(&manifest.slug) {
        return Err(HubModeError::InvalidCorpusSlug {
            slug: manifest.slug.clone(),
        });
    }
    if !valid_pin_sha(&manifest.pin_sha) {
        return Err(HubModeError::IntegrityMismatch {
            slug: manifest.slug.clone(),
            reason: format!("invalid 40-hex pack pin_sha '{}'", manifest.pin_sha),
        });
    }
    Ok(())
}

/// Runtime mode detected by `xerj mcp` at session startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum McpRuntimeMode {
    /// A local `xerj` node responded within the startup probe budget.
    LocalNodeReachable {
        base_url: String,
        probe_latency_ms: u64,
    },
    /// No local `xerj` node is running; hub tools operate directly against
    /// `hub.xerj.org` and the bounded disk cache.
    HubOnlyFallback {
        attempted_url: String,
        hub_url: String,
        notice: String,
    },
}

impl McpRuntimeMode {
    /// Detect mode using a caller-supplied reachability probe closure.
    ///
    /// If `probe(base_url, budget_ms)` returns `Some(latency_ms)` within
    /// `budget_ms`, the router enters [`McpRuntimeMode::LocalNodeReachable`];
    /// otherwise it enters [`McpRuntimeMode::HubOnlyFallback`] with an explicit
    /// human-readable notice.
    pub fn detect<F>(base_url: &str, hub_url: &str, budget_ms: u64, probe: F) -> Self
    where
        F: FnOnce(&str, u64) -> Option<u64>,
    {
        let normalized_base = base_url.trim_end_matches('/').to_string();
        let normalized_hub = if hub_url.trim().is_empty() {
            DEFAULT_HUB_URL.to_string()
        } else {
            hub_url.trim_end_matches('/').to_string()
        };
        if let Some(latency_ms) = probe(&normalized_base, budget_ms) {
            if latency_ms <= budget_ms {
                return Self::LocalNodeReachable {
                    base_url: normalized_base,
                    probe_latency_ms: latency_ms,
                };
            }
        }
        let notice = format!(
            "no local xerj node reachable at {normalized_base} within {budget_ms}ms; \
             running in hub-only mode via {normalized_hub} with local disk cache \
             (private-index tools require starting a local node)"
        );
        Self::HubOnlyFallback {
            attempted_url: normalized_base,
            hub_url: normalized_hub,
            notice,
        }
    }

    /// True when a local `xerj` node is reachable.
    pub fn has_local_node(&self) -> bool {
        matches!(self, Self::LocalNodeReachable { .. })
    }

    /// Optional fallback notice when running without a local node.
    pub fn fallback_notice(&self) -> Option<&str> {
        match self {
            Self::LocalNodeReachable { .. } => None,
            Self::HubOnlyFallback { notice, .. } => Some(notice.as_str()),
        }
    }
}

/// Destination selected for an MCP tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case")]
pub enum RouteTarget {
    /// Route to the running local `xerj` node at `base_url`.
    LocalNode { base_url: String },
    /// Route to the hub client and bounded disk cache.
    HubClient {
        hub_url: String,
        fallback_notice: Option<String>,
    },
}

/// Summary descriptor of an MCP tool's availability in the current runtime mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolAvailability {
    pub name: String,
    pub available: bool,
    pub routed_via: &'static str,
    pub note: Option<String>,
}

/// Staleness status of a disk-cached hub artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Freshness {
    /// Cached entry age is within `max_age_secs`.
    Fresh { age_secs: u64, max_age_secs: u64 },
    /// Cached entry age exceeds `max_age_secs`; surfaced explicitly to callers.
    Stale { age_secs: u64, max_age_secs: u64 },
}

impl Freshness {
    /// Compute freshness from `fetched_at_unix` vs `now_unix` and `ttl_secs`.
    pub fn evaluate(fetched_at_unix: u64, now_unix: u64, ttl_secs: u64) -> Self {
        let age_secs = now_unix.saturating_sub(fetched_at_unix);
        if age_secs <= ttl_secs {
            Self::Fresh {
                age_secs,
                max_age_secs: ttl_secs,
            }
        } else {
            Self::Stale {
                age_secs,
                max_age_secs: ttl_secs,
            }
        }
    }

    /// True if this artifact is past its configured TTL.
    pub fn is_stale(&self) -> bool {
        matches!(self, Self::Stale { .. })
    }

    /// Explicit warning string when stale, `None` when fresh.
    pub fn warning_line(&self, subject: &str) -> Option<String> {
        match self {
            Self::Fresh { .. } => None,
            Self::Stale {
                age_secs,
                max_age_secs,
            } => Some(format!(
                "cached {subject} is stale (age {age_secs}s exceeds ttl {max_age_secs}s); \
                 serving verified disk cache entry"
            )),
        }
    }
}

/// Whether a corpus/catalogue lookup was served from disk cache or fetched from the hub.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheLookupOutcome {
    /// Served directly from the verified on-disk cache with zero network requests.
    HitFromDisk,
    /// Fetched from the remote hub, verified, and persisted to the disk cache.
    FetchedAndCached,
}

/// One passage record inside a cached corpus pack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackPassage {
    pub file: String,
    pub line: u32,
    pub kind: String,
    pub text: String,
}

/// Self-contained corpus pack payload cached on disk for hub-only MCP queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CachedCorpusPack {
    pub manifest: PackManifest,
    pub defs: Vec<DefRecord>,
    pub passages: Vec<PackPassage>,
}

impl CachedCorpusPack {
    /// Validate internal format version, slug, and pin SHA.
    pub fn validate(&self) -> Result<(), HubModeError> {
        validate_pack_manifest_format(&self.manifest)
    }
}

/// Metadata for a single corpus entry in the on-disk LRU cache index.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskCacheEntryMeta {
    pub slug: String,
    pub pin_sha: String,
    pub pin_date: String,
    pub licence_spdx: String,
    pub licence_class: LicenceClass,
    pub sha256: String,
    pub size_bytes: u64,
    pub fetched_at_unix: u64,
    pub last_accessed_seq: u64,
    pub file_name: String,
}

/// Persistent LRU index manifest stored at `<cache_dir>/index.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiskCacheIndex {
    format_version: u32,
    access_clock: u64,
    catalogue_fetched_at_unix: Option<u64>,
    catalogue_sha256: Option<String>,
    entries: BTreeMap<String, DiskCacheEntryMeta>,
}

const CACHE_INDEX_FORMAT_VERSION: u32 = 1;
const CACHE_INDEX_FILE: &str = "index.json";
const CACHE_CATALOGUE_FILE: &str = "catalogue.json";

impl Default for DiskCacheIndex {
    fn default() -> Self {
        Self {
            format_version: CACHE_INDEX_FORMAT_VERSION,
            access_clock: 0,
            catalogue_fetched_at_unix: None,
            catalogue_sha256: None,
            entries: BTreeMap::new(),
        }
    }
}

/// Result of resolving a corpus pack or catalogue through [`HubDiskCache`].
#[derive(Debug, Clone)]
pub struct ResolvedCachedArtifact<T> {
    pub value: T,
    pub outcome: CacheLookupOutcome,
    pub freshness: Freshness,
    pub evicted_slugs: Vec<String>,
}

/// Bounded, SHA-256-verified LRU disk cache for hub catalogue and corpus packs.
#[derive(Debug)]
pub struct HubDiskCache {
    root: PathBuf,
    max_bytes: u64,
    ttl_secs: u64,
    index: DiskCacheIndex,
    disk_hits: u64,
    network_fetches: u64,
    evictions: u64,
}

impl HubDiskCache {
    /// Open or initialize a bounded disk cache at `root`.
    pub fn open(
        root: impl Into<PathBuf>,
        max_bytes: u64,
        ttl_secs: u64,
    ) -> Result<Self, HubModeError> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        let index_path = root.join(CACHE_INDEX_FILE);
        let index = if index_path.is_file() {
            let raw = std::fs::read(&index_path)?;
            let parsed: DiskCacheIndex = serde_json::from_slice(&raw)?;
            if parsed.format_version != CACHE_INDEX_FORMAT_VERSION {
                return Err(HubModeError::UnsupportedFormatVersion {
                    kind: "disk_cache_index",
                    found: parsed.format_version,
                    supported_min: CACHE_INDEX_FORMAT_VERSION,
                    supported_max: CACHE_INDEX_FORMAT_VERSION,
                });
            }
            parsed
        } else {
            DiskCacheIndex::default()
        };
        Ok(Self {
            root,
            max_bytes,
            ttl_secs,
            index,
            disk_hits: 0,
            network_fetches: 0,
            evictions: 0,
        })
    }

    /// Directory backing this disk cache.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Maximum total bytes allowed across cached corpus pack files.
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Total bytes currently occupied by cached corpus pack files.
    pub fn used_bytes(&self) -> u64 {
        self.index.entries.values().map(|e| e.size_bytes).sum()
    }

    /// Number of corpus packs currently stored in the disk cache.
    pub fn entry_count(&self) -> usize {
        self.index.entries.len()
    }

    /// Measured count of lookups served from the on-disk cache.
    pub fn disk_hits(&self) -> u64 {
        self.disk_hits
    }

    /// Measured count of lookups that required a remote fetch.
    pub fn network_fetches(&self) -> u64 {
        self.network_fetches
    }

    /// Total number of corpus entries evicted by the LRU policy.
    pub fn evictions(&self) -> u64 {
        self.evictions
    }

    /// Check whether `slug` is currently present in the disk cache index.
    pub fn contains_corpus(&self, slug: &str) -> bool {
        self.index.entries.contains_key(slug)
    }

    /// Retrieve metadata for `slug` if cached.
    pub fn entry_meta(&self, slug: &str) -> Option<&DiskCacheEntryMeta> {
        self.index.entries.get(slug)
    }

    fn persist_index(&self) -> Result<(), HubModeError> {
        let bytes = serde_json::to_vec_pretty(&self.index)?;
        write_file_durable(&self.root.join(CACHE_INDEX_FILE), &bytes)?;
        Ok(())
    }

    /// Store a verified [`Catalogue`] in the disk cache.
    pub fn put_catalogue(
        &mut self,
        catalogue: &Catalogue,
        now_unix: u64,
    ) -> Result<Freshness, HubModeError> {
        validate_catalogue_format(catalogue)?;
        let bytes = serde_json::to_vec_pretty(catalogue)?;
        let digest = sha256_hex(&bytes);
        write_file_durable(&self.root.join(CACHE_CATALOGUE_FILE), &bytes)?;
        self.index.catalogue_fetched_at_unix = Some(now_unix);
        self.index.catalogue_sha256 = Some(digest);
        self.persist_index()?;
        Ok(Freshness::evaluate(now_unix, now_unix, self.ttl_secs))
    }

    /// Read the cached [`Catalogue`] from disk, verifying its SHA-256 and format version,
    /// or fetch and cache it using `fetch` on a cache miss.
    pub fn get_or_fetch_catalogue<F>(
        &mut self,
        now_unix: u64,
        fetch: F,
    ) -> Result<ResolvedCachedArtifact<Catalogue>, HubModeError>
    where
        F: FnOnce() -> Result<Catalogue, HubModeError>,
    {
        let cat_path = self.root.join(CACHE_CATALOGUE_FILE);
        if let (Some(fetched_at), Some(expected_sha)) = (
            self.index.catalogue_fetched_at_unix,
            self.index.catalogue_sha256.clone(),
        ) {
            if cat_path.is_file() {
                let bytes = std::fs::read(&cat_path)?;
                let actual_sha = sha256_hex(&bytes);
                if actual_sha != expected_sha {
                    return Err(HubModeError::IntegrityMismatch {
                        slug: "catalogue".into(),
                        reason: format!(
                            "cached catalogue sha256 mismatch: expected {expected_sha}, got {actual_sha}"
                        ),
                    });
                }
                let catalogue: Catalogue = serde_json::from_slice(&bytes)?;
                validate_catalogue_format(&catalogue)?;
                self.disk_hits += 1;
                return Ok(ResolvedCachedArtifact {
                    value: catalogue,
                    outcome: CacheLookupOutcome::HitFromDisk,
                    freshness: Freshness::evaluate(fetched_at, now_unix, self.ttl_secs),
                    evicted_slugs: Vec::new(),
                });
            }
        }

        let catalogue = fetch()?;
        let freshness = self.put_catalogue(&catalogue, now_unix)?;
        self.network_fetches += 1;
        Ok(ResolvedCachedArtifact {
            value: catalogue,
            outcome: CacheLookupOutcome::FetchedAndCached,
            freshness,
            evicted_slugs: Vec::new(),
        })
    }

    /// Resolve a corpus pack from the disk cache if present and matching `entry.pin_sha`
    /// and `entry.pack_sha256`; otherwise fetch via `fetch_pack`, verify integrity,
    /// enforce the LRU byte budget, and persist atomically to disk.
    pub fn get_or_fetch_pack<F>(
        &mut self,
        entry: &CatalogueEntry,
        now_unix: u64,
        fetch_pack: F,
    ) -> Result<ResolvedCachedArtifact<CachedCorpusPack>, HubModeError>
    where
        F: FnOnce(&CatalogueEntry) -> Result<Vec<u8>, HubModeError>,
    {
        if !valid_corpus_slug(&entry.slug) {
            return Err(HubModeError::InvalidCorpusSlug {
                slug: entry.slug.clone(),
            });
        }
        if entry.licence_class == LicenceClass::Excluded {
            return Err(HubModeError::ExcludedCorpusLicence {
                slug: entry.slug.clone(),
                licence_spdx: entry.licence_spdx.clone(),
            });
        }

        // Fast path: check disk cache if pin_sha and sha256 match the catalogue entry.
        if let Some(meta) = self.index.entries.get(&entry.slug).cloned() {
            let pack_path = self.root.join(&meta.file_name);
            if meta.pin_sha == entry.pin_sha
                && meta.sha256 == entry.pack_sha256
                && pack_path.is_file()
            {
                let raw = std::fs::read(&pack_path)?;
                let actual_sha = sha256_hex(&raw);
                if actual_sha != meta.sha256 {
                    return Err(HubModeError::IntegrityMismatch {
                        slug: entry.slug.clone(),
                        reason: format!(
                            "on-disk pack sha256 mismatch: expected {}, got {actual_sha}",
                            meta.sha256
                        ),
                    });
                }
                let pack: CachedCorpusPack = serde_json::from_slice(&raw)?;
                pack.validate()?;
                if pack.manifest.slug != entry.slug || pack.manifest.pin_sha != entry.pin_sha {
                    return Err(HubModeError::IntegrityMismatch {
                        slug: entry.slug.clone(),
                        reason: format!(
                            "pack manifest ({}, {}) does not match catalogue ({}, {})",
                            pack.manifest.slug, pack.manifest.pin_sha, entry.slug, entry.pin_sha
                        ),
                    });
                }

                self.index.access_clock += 1;
                let seq = self.index.access_clock;
                if let Some(m) = self.index.entries.get_mut(&entry.slug) {
                    m.last_accessed_seq = seq;
                }
                self.persist_index()?;
                self.disk_hits += 1;

                let freshness = Freshness::evaluate(meta.fetched_at_unix, now_unix, self.ttl_secs);
                return Ok(ResolvedCachedArtifact {
                    value: pack,
                    outcome: CacheLookupOutcome::HitFromDisk,
                    freshness,
                    evicted_slugs: Vec::new(),
                });
            }
        }

        // Slow path: fetch bytes from hub, verify digest + manifest format, evict LRU, persist.
        let raw_bytes = fetch_pack(entry)?;
        let actual_sha = sha256_hex(&raw_bytes);
        if actual_sha != entry.pack_sha256 {
            return Err(HubModeError::IntegrityMismatch {
                slug: entry.slug.clone(),
                reason: format!(
                    "downloaded pack sha256 mismatch: expected {}, got {actual_sha}",
                    entry.pack_sha256
                ),
            });
        }

        let pack: CachedCorpusPack = serde_json::from_slice(&raw_bytes)?;
        pack.validate()?;
        if pack.manifest.slug != entry.slug || pack.manifest.pin_sha != entry.pin_sha {
            return Err(HubModeError::IntegrityMismatch {
                slug: entry.slug.clone(),
                reason: format!(
                    "downloaded manifest ({}, {}) does not match catalogue ({}, {})",
                    pack.manifest.slug, pack.manifest.pin_sha, entry.slug, entry.pin_sha
                ),
            });
        }

        let entry_bytes = raw_bytes.len() as u64;
        if entry_bytes > self.max_bytes {
            return Err(HubModeError::EntryExceedsCacheBudget {
                slug: entry.slug.clone(),
                entry_bytes,
                max_bytes: self.max_bytes,
            });
        }

        // Remove any prior cached version of the same slug before computing eviction needs.
        if let Some(old) = self.index.entries.remove(&entry.slug) {
            let _ = std::fs::remove_file(self.root.join(&old.file_name));
        }

        // Evict least-recently-used entries (lowest `last_accessed_seq`, tie-broken by slug)
        // until `used_bytes() + entry_bytes <= max_bytes`.
        let mut evicted_slugs = Vec::new();
        while self.used_bytes() + entry_bytes > self.max_bytes {
            let victim_slug = self
                .index
                .entries
                .values()
                .min_by_key(|m| (m.last_accessed_seq, m.slug.clone()))
                .map(|m| m.slug.clone());
            let Some(victim) = victim_slug else {
                break;
            };
            if let Some(removed) = self.index.entries.remove(&victim) {
                let _ = std::fs::remove_file(self.root.join(&removed.file_name));
                evicted_slugs.push(victim);
                self.evictions += 1;
            }
        }

        let file_name = format!("pack-{}.json", entry.slug);
        write_file_durable(&self.root.join(&file_name), &raw_bytes)?;

        self.index.access_clock += 1;
        let seq = self.index.access_clock;
        self.index.entries.insert(
            entry.slug.clone(),
            DiskCacheEntryMeta {
                slug: entry.slug.clone(),
                pin_sha: entry.pin_sha.clone(),
                pin_date: entry.pin_date.clone(),
                licence_spdx: entry.licence_spdx.clone(),
                licence_class: entry.licence_class,
                sha256: actual_sha,
                size_bytes: entry_bytes,
                fetched_at_unix: now_unix,
                last_accessed_seq: seq,
                file_name,
            },
        );
        self.persist_index()?;
        self.network_fetches += 1;

        Ok(ResolvedCachedArtifact {
            value: pack,
            outcome: CacheLookupOutcome::FetchedAndCached,
            freshness: Freshness::evaluate(now_unix, now_unix, self.ttl_secs),
            evicted_slugs,
        })
    }
}

/// Parameters for a hub corpus query executed by [`HubFirstRouter::query_corpus`].
#[derive(Debug, Clone, Copy)]
pub struct HubQueryRequest<'a> {
    pub slug: &'a str,
    pub query: &'a str,
    pub mode: AnswerMode,
    pub age_days: u32,
    pub now_unix: u64,
}

/// Complete response from a hub-only or hybrid MCP hub query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubQueryResult {
    pub answer: Answer,
    pub rendered: String,
    pub cache_outcome: CacheLookupOutcome,
    pub freshness: Freshness,
    pub runtime_mode: McpRuntimeMode,
    pub evicted_slugs: Vec<String>,
}

/// Router and query executor that unifies local-node preference with hub-only
/// fallback and the bounded disk cache.
#[derive(Debug)]
pub struct HubFirstRouter {
    mode: McpRuntimeMode,
    cache: HubDiskCache,
}

impl HubFirstRouter {
    /// Create a new [`HubFirstRouter`] for the detected [`McpRuntimeMode`] and [`HubDiskCache`].
    pub fn new(mode: McpRuntimeMode, cache: HubDiskCache) -> Self {
        Self { mode, cache }
    }

    /// Current runtime mode (`LocalNodeReachable` or `HubOnlyFallback`).
    pub fn mode(&self) -> &McpRuntimeMode {
        &self.mode
    }

    /// Shared reference to the underlying bounded disk cache.
    pub fn cache(&self) -> &HubDiskCache {
        &self.cache
    }

    /// Mutable reference to the underlying bounded disk cache.
    pub fn cache_mut(&mut self) -> &mut HubDiskCache {
        &mut self.cache
    }

    /// List MCP tools and their availability in the current mode.
    ///
    /// When no local node is running, `xerj mcp` still starts and lists all hub
    /// tools as available while marking private-index node tools with a clear
    /// notice explaining that a local node is required.
    pub fn list_tools(&self) -> Vec<ToolAvailability> {
        let mut out = Vec::with_capacity(HUB_DIRECT_TOOLS.len() + LOCAL_NODE_TOOLS.len());
        for &tool in HUB_DIRECT_TOOLS {
            out.push(ToolAvailability {
                name: tool.to_string(),
                available: true,
                routed_via: "hub_client_cache",
                note: self.mode.fallback_notice().map(str::to_string),
            });
        }
        for &tool in LOCAL_NODE_TOOLS {
            match &self.mode {
                McpRuntimeMode::LocalNodeReachable { .. } => {
                    out.push(ToolAvailability {
                        name: tool.to_string(),
                        available: true,
                        routed_via: "local_node",
                        note: None,
                    });
                }
                McpRuntimeMode::HubOnlyFallback { notice, .. } => {
                    out.push(ToolAvailability {
                        name: tool.to_string(),
                        available: false,
                        routed_via: "local_node",
                        note: Some(notice.clone()),
                    });
                }
            }
        }
        out
    }

    /// Route an MCP tool call by name.
    ///
    /// - Hub tools (`xerj_hub_*`, `xerj_corpus_feedback`, `xerj_field_report`)
    ///   always succeed and route to [`RouteTarget::HubClient`].
    /// - Private-index node tools (`xerj_search`, `xerj_def`, `xerj_map`, …)
    ///   route to [`RouteTarget::LocalNode`] when a node is reachable, and
    ///   return [`HubModeError::LocalNodeRequired`] when in hub-only fallback.
    pub fn route_tool(&self, tool_name: &str) -> Result<RouteTarget, HubModeError> {
        if HUB_DIRECT_TOOLS.contains(&tool_name) {
            let (hub_url, fallback_notice) = match &self.mode {
                McpRuntimeMode::LocalNodeReachable { .. } => (DEFAULT_HUB_URL.to_string(), None),
                McpRuntimeMode::HubOnlyFallback {
                    hub_url, notice, ..
                } => (hub_url.clone(), Some(notice.clone())),
            };
            return Ok(RouteTarget::HubClient {
                hub_url,
                fallback_notice,
            });
        }

        match &self.mode {
            McpRuntimeMode::LocalNodeReachable { base_url, .. } => Ok(RouteTarget::LocalNode {
                base_url: base_url.clone(),
            }),
            McpRuntimeMode::HubOnlyFallback { notice, .. } => {
                Err(HubModeError::LocalNodeRequired {
                    tool: tool_name.to_string(),
                    notice: notice.clone(),
                })
            }
        }
    }

    /// Answer a hub question or definition lookup end-to-end using the disk cache
    /// (fetching on first access and serving subsequent calls from disk).
    ///
    /// - Unknown corpus slugs fail loudly with [`HubModeError::UnknownCorpus`].
    /// - Invalid corpus slugs fail loudly with [`HubModeError::InvalidCorpusSlug`].
    /// - Unsupported catalogue or pack `format_version` fails loudly with
    ///   [`HubModeError::UnsupportedFormatVersion`].
    /// - Staleness is always attached to `Answer.warnings` when `now_unix`
    ///   exceeds `fetched_at_unix + ttl_secs`.
    pub fn query_corpus<FCat, FPack>(
        &mut self,
        request: HubQueryRequest<'_>,
        fetch_catalogue: FCat,
        fetch_pack: FPack,
    ) -> Result<HubQueryResult, HubModeError>
    where
        FCat: FnOnce() -> Result<Catalogue, HubModeError>,
        FPack: FnOnce(&CatalogueEntry) -> Result<Vec<u8>, HubModeError>,
    {
        let HubQueryRequest {
            slug,
            query,
            mode,
            age_days,
            now_unix,
        } = request;
        if !valid_corpus_slug(slug) {
            return Err(HubModeError::InvalidCorpusSlug {
                slug: slug.to_string(),
            });
        }

        let cat_resolved = self
            .cache
            .get_or_fetch_catalogue(now_unix, fetch_catalogue)?;
        let entry = cat_resolved
            .value
            .corpora
            .iter()
            .find(|c| c.slug == slug)
            .ok_or_else(|| HubModeError::UnknownCorpus {
                slug: slug.to_string(),
                catalogue_generated_at: cat_resolved.value.generated_at.clone(),
            })?;

        let pack_resolved = self.cache.get_or_fetch_pack(entry, now_unix, fetch_pack)?;
        let pack = &pack_resolved.value;

        let licence_warning = match entry.licence_class {
            LicenceClass::AdaptWithAttribution => None,
            LicenceClass::ApproachOnly => Some(format!(
                "approach-only licence ({}): read to understand the approach, do not copy verbatim",
                entry.licence_spdx
            )),
            LicenceClass::Excluded => {
                return Err(HubModeError::ExcludedCorpusLicence {
                    slug: entry.slug.clone(),
                    licence_spdx: entry.licence_spdx.clone(),
                });
            }
        };

        let needle = query.trim().to_ascii_lowercase();
        let hits: Vec<Hit> = match mode {
            AnswerMode::Def => pack
                .defs
                .iter()
                .filter(|d| {
                    d.symbol.to_ascii_lowercase() == needle
                        || d.symbol.to_ascii_lowercase().contains(&needle)
                })
                .map(|d| Hit {
                    file: d.file.clone(),
                    line: d.line,
                    kind: d.kind.clone(),
                    text: d.text.clone(),
                    value: d.value.clone(),
                    score: if d.symbol.eq_ignore_ascii_case(query.trim()) {
                        2.0
                    } else {
                        1.0
                    },
                    licence_warning: licence_warning.clone(),
                })
                .collect(),
            AnswerMode::How => {
                let terms: Vec<&str> = needle
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .filter(|t| !t.is_empty())
                    .collect();
                pack.passages
                    .iter()
                    .filter_map(|p| {
                        let hay = format!("{} {}", p.file, p.text).to_ascii_lowercase();
                        let matched = terms.iter().filter(|t| hay.contains(**t)).count();
                        if matched > 0 {
                            Some(Hit {
                                file: p.file.clone(),
                                line: p.line,
                                kind: p.kind.clone(),
                                text: p.text.clone(),
                                value: None,
                                score: matched as f32,
                                licence_warning: licence_warning.clone(),
                            })
                        } else {
                            None
                        }
                    })
                    .collect()
            }
            AnswerMode::Files => pack
                .passages
                .iter()
                .map(|p| Hit {
                    file: p.file.clone(),
                    line: p.line,
                    kind: "file".to_string(),
                    text: format!("{} ({})", p.file, entry.licence_spdx),
                    value: None,
                    score: 1.0,
                    licence_warning: licence_warning.clone(),
                })
                .collect(),
        };

        let mut warnings = Vec::new();
        if let Some(notice) = self.mode.fallback_notice() {
            warnings.push(notice.to_string());
        }
        if let Some(cat_stale) = cat_resolved.freshness.warning_line("catalogue") {
            warnings.push(cat_stale);
        }
        if let Some(pack_stale) = pack_resolved
            .freshness
            .warning_line(&format!("corpus '{}'", entry.slug))
        {
            warnings.push(pack_stale);
        }

        let answer = Answer {
            corpus: entry.slug.clone(),
            pin_sha: entry.pin_sha.clone(),
            pin_date: entry.pin_date.clone(),
            age_days,
            licence_spdx: entry.licence_spdx.clone(),
            licence_class: entry.licence_class,
            answered_from: match pack_resolved.outcome {
                CacheLookupOutcome::HitFromDisk => AnsweredFrom::LocalPack,
                CacheLookupOutcome::FetchedAndCached => AnsweredFrom::Hosted,
            },
            mode,
            hits,
            truncated: false,
            warnings,
        };
        let rendered = crate::hub::render(&answer);

        Ok(HubQueryResult {
            answer,
            rendered,
            cache_outcome: pack_resolved.outcome,
            freshness: pack_resolved.freshness,
            runtime_mode: self.mode.clone(),
            evicted_slugs: pack_resolved.evicted_slugs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::{PackIndex, PackSource, Sketch};
    use std::cell::Cell;

    fn sample_pack_bytes(
        slug: &str,
        pin_sha: &str,
        licence_class: LicenceClass,
        pad_len: usize,
    ) -> Vec<u8> {
        let pack = CachedCorpusPack {
            manifest: PackManifest {
                format_version: PACK_FORMAT_VERSION,
                slug: slug.to_string(),
                pin_sha: pin_sha.to_string(),
                pin_date: "2026-11-15".to_string(),
                built_at: "2026-11-15T10:00:00Z".to_string(),
                builder_version: "1.0.0-rc.104".to_string(),
                index_prefix: format!("xc-{slug}-b1"),
                indices: vec![PackIndex {
                    name: format!("xc-{slug}-b1-defs"),
                    docs: 2,
                    bytes: 512,
                }],
                snapshot_name: format!("snap-{slug}"),
                licence_spdx: "BSD-2-Clause".to_string(),
                licence_class,
                sources: vec![PackSource {
                    repo: format!("{slug}/{slug}"),
                    url: format!("https://github.com/{slug}/{slug}"),
                    sha: pin_sha.to_string(),
                    licence: "BSD-2-Clause".to_string(),
                }],
                defs_count: 1,
                files_count: 1,
            },
            defs: vec![DefRecord {
                symbol: "LZ4_compress_default".to_string(),
                kind: "function".to_string(),
                lang: "c".to_string(),
                file: "lib/lz4.h".to_string(),
                line: 142,
                text: "int LZ4_compress_default(const char* src, char* dst, int srcSize, int dstCapacity);".to_string(),
                value: Some("142".to_string()),
            }],
            passages: vec![PackPassage {
                file: "lib/lz4.c".to_string(),
                line: 310,
                kind: "function".to_string(),
                text: format!(
                    "LZ4 fast block compression implementation details {}",
                    "x".repeat(pad_len)
                ),
            }],
        };
        serde_json::to_vec(&pack).expect("serialize sample pack")
    }

    fn sample_catalogue_entry(
        slug: &str,
        pin_sha: &str,
        pack_bytes: &[u8],
        licence_class: LicenceClass,
    ) -> CatalogueEntry {
        CatalogueEntry {
            slug: slug.to_string(),
            domain: "NET".to_string(),
            topics: vec!["compression".to_string()],
            ask_for: "fast block compression".to_string(),
            upstream: format!("{slug}/{slug}"),
            pin_sha: pin_sha.to_string(),
            pin_date: "2026-11-15".to_string(),
            refresh_cadence_days: 7,
            licence_spdx: "BSD-2-Clause".to_string(),
            licence_class,
            pack_bytes: pack_bytes.len() as u64,
            pack_url: format!("https://hub.xerj.org/packs/{slug}.json"),
            pack_sha256: sha256_hex(pack_bytes),
            sig_url: format!("https://hub.xerj.org/packs/{slug}.json.sig"),
            dependency_names: vec![slug.to_string()],
            quality_grade: Some("G7 5/5".to_string()),
            term_sketch: Sketch {
                m_bits: 64,
                k_hashes: 3,
                bits_base64: "AAAAAAAAAAA=".to_string(),
            },
        }
    }

    fn sample_catalogue(entries: Vec<CatalogueEntry>) -> Catalogue {
        Catalogue {
            format_version: CATALOGUE_FORMAT_VERSION,
            generated_at: "2026-11-15T12:00:00Z".to_string(),
            corpora: entries,
        }
    }

    #[test]
    fn no_node_startup_detects_hub_only_fallback_and_lists_pack_tools() {
        let mode = McpRuntimeMode::detect(
            "http://127.0.0.1:9200",
            DEFAULT_HUB_URL,
            DEFAULT_PROBE_BUDGET_MS,
            |_url, _budget| None,
        );
        assert!(!mode.has_local_node());
        assert!(mode
            .fallback_notice()
            .unwrap()
            .contains("running in hub-only mode"));

        let dir = tempfile::tempdir().unwrap();
        let cache = HubDiskCache::open(
            dir.path(),
            DEFAULT_MCP_CACHE_MAX_BYTES,
            DEFAULT_CACHE_TTL_SECS,
        )
        .unwrap();
        let router = HubFirstRouter::new(mode, cache);
        let tools = router.list_tools();

        for hub_tool in ["xerj_hub_find", "xerj_hub_def", "xerj_hub_ask"] {
            let t = tools
                .iter()
                .find(|t| t.name == hub_tool)
                .expect("hub tool listed");
            assert!(t.available, "{hub_tool} must be available without a node");
            assert_eq!(t.routed_via, "hub_client_cache");
        }
        for node_tool in ["xerj_search", "xerj_def", "xerj_map"] {
            let t = tools
                .iter()
                .find(|t| t.name == node_tool)
                .expect("node tool listed");
            assert!(!t.available, "{node_tool} requires a local node");
            assert!(t
                .note
                .as_deref()
                .unwrap()
                .contains("no local xerj node reachable"));
        }
    }

    #[test]
    fn probe_exceeding_startup_budget_falls_back_to_hub_only() {
        let mode = McpRuntimeMode::detect(
            "http://localhost:9200",
            DEFAULT_HUB_URL,
            100,
            |_url, _budget| Some(250),
        );
        assert!(!mode.has_local_node());
        assert!(mode.fallback_notice().unwrap().contains("within 100ms"));
    }

    #[test]
    fn end_to_end_hub_question_and_second_call_served_from_disk_cache() {
        let pin = "0123456789abcdef0123456789abcdef01234567";
        let pack_bytes = sample_pack_bytes("lz4", pin, LicenceClass::AdaptWithAttribution, 32);
        let entry =
            sample_catalogue_entry("lz4", pin, &pack_bytes, LicenceClass::AdaptWithAttribution);
        let catalogue = sample_catalogue(vec![entry]);

        let dir = tempfile::tempdir().unwrap();
        let mode =
            McpRuntimeMode::detect("http://127.0.0.1:9200", DEFAULT_HUB_URL, 100, |_, _| None);
        let cache = HubDiskCache::open(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600).unwrap();
        let mut router = HubFirstRouter::new(mode, cache);

        let cat_fetches = Cell::new(0usize);
        let pack_fetches = Cell::new(0usize);

        // First call: fetches from hub and writes to disk cache.
        let first = router
            .query_corpus(
                HubQueryRequest {
                    slug: "lz4",
                    query: "LZ4_compress_default",
                    mode: AnswerMode::Def,
                    age_days: 2,
                    now_unix: 1_700_000_000,
                },
                || {
                    cat_fetches.set(cat_fetches.get() + 1);
                    Ok(catalogue.clone())
                },
                |_e| {
                    pack_fetches.set(pack_fetches.get() + 1);
                    Ok(pack_bytes.clone())
                },
            )
            .expect("first hub query succeeds");

        assert_eq!(first.cache_outcome, CacheLookupOutcome::FetchedAndCached);
        assert_eq!(first.answer.answered_from, AnsweredFrom::Hosted);
        assert_eq!(first.answer.hits.len(), 1);
        assert_eq!(first.answer.hits[0].value.as_deref(), Some("142"));
        assert!(first.rendered.contains("lib/lz4.h:142"));
        assert_eq!(cat_fetches.get(), 1);
        assert_eq!(pack_fetches.get(), 1);
        assert_eq!(router.cache().disk_hits(), 0);
        assert_eq!(router.cache().network_fetches(), 2);

        // Second call for the same corpus: served from disk cache with zero network fetches.
        let second = router
            .query_corpus(
                HubQueryRequest {
                    slug: "lz4",
                    query: "fast block compression",
                    mode: AnswerMode::How,
                    age_days: 2,
                    now_unix: 1_700_000_010,
                },
                || panic!("catalogue must not be re-fetched on second call"),
                |_e| panic!("pack must not be re-fetched on second call"),
            )
            .expect("second hub query succeeds from disk cache");

        assert_eq!(second.cache_outcome, CacheLookupOutcome::HitFromDisk);
        assert_eq!(second.answer.answered_from, AnsweredFrom::LocalPack);
        assert_eq!(second.answer.hits.len(), 1);
        assert!(second.rendered.contains("answered from: local pack"));
        assert_eq!(cat_fetches.get(), 1);
        assert_eq!(pack_fetches.get(), 1);
        assert_eq!(router.cache().disk_hits(), 2);
        assert_eq!(router.cache().network_fetches(), 2);
    }

    #[test]
    fn disk_cache_persists_across_reopen_without_refetching() {
        let pin = "abcdef0123456789abcdef0123456789abcdef01";
        let pack_bytes = sample_pack_bytes("sqlite", pin, LicenceClass::AdaptWithAttribution, 16);
        let entry = sample_catalogue_entry(
            "sqlite",
            pin,
            &pack_bytes,
            LicenceClass::AdaptWithAttribution,
        );
        let catalogue = sample_catalogue(vec![entry]);

        let dir = tempfile::tempdir().unwrap();
        {
            let mode =
                McpRuntimeMode::detect("http://127.0.0.1:9200", DEFAULT_HUB_URL, 50, |_, _| None);
            let cache = HubDiskCache::open(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600).unwrap();
            let mut router = HubFirstRouter::new(mode, cache);
            let res = router
                .query_corpus(
                    HubQueryRequest {
                        slug: "sqlite",
                        query: "LZ4_compress_default",
                        mode: AnswerMode::Def,
                        age_days: 1,
                        now_unix: 1_700_000_000,
                    },
                    || Ok(catalogue.clone()),
                    |_| Ok(pack_bytes.clone()),
                )
                .unwrap();
            assert_eq!(res.cache_outcome, CacheLookupOutcome::FetchedAndCached);
        }

        // Reopen a fresh HubDiskCache on the same directory in a new session.
        let mode =
            McpRuntimeMode::detect("http://127.0.0.1:9200", DEFAULT_HUB_URL, 50, |_, _| None);
        let reopened = HubDiskCache::open(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600).unwrap();
        assert!(reopened.contains_corpus("sqlite"));
        let mut router = HubFirstRouter::new(mode, reopened);
        let res = router
            .query_corpus(
                HubQueryRequest {
                    slug: "sqlite",
                    query: "LZ4_compress_default",
                    mode: AnswerMode::Def,
                    age_days: 1,
                    now_unix: 1_700_000_100,
                },
                || panic!("catalogue should be read from disk after reopen"),
                |_| panic!("pack should be read from disk after reopen"),
            )
            .unwrap();
        assert_eq!(res.cache_outcome, CacheLookupOutcome::HitFromDisk);
        assert_eq!(router.cache().disk_hits(), 2);
        assert_eq!(router.cache().network_fetches(), 0);
    }

    #[test]
    fn lru_disk_cache_evicts_least_recently_used_entry_when_budget_exceeded() {
        let pin = "1111111111111111111111111111111111111111";
        let bytes_a = sample_pack_bytes("corp-a", pin, LicenceClass::AdaptWithAttribution, 200);
        let bytes_b = sample_pack_bytes("corp-b", pin, LicenceClass::AdaptWithAttribution, 200);
        let bytes_c = sample_pack_bytes("corp-c", pin, LicenceClass::AdaptWithAttribution, 200);

        let entry_a =
            sample_catalogue_entry("corp-a", pin, &bytes_a, LicenceClass::AdaptWithAttribution);
        let entry_b =
            sample_catalogue_entry("corp-b", pin, &bytes_b, LicenceClass::AdaptWithAttribution);
        let entry_c =
            sample_catalogue_entry("corp-c", pin, &bytes_c, LicenceClass::AdaptWithAttribution);

        // Budget fits at most two packs at once, never all three.
        let max_bytes = bytes_a.len() as u64 + bytes_b.len() as u64 + 32;
        assert!(bytes_a.len() as u64 + bytes_b.len() as u64 + bytes_c.len() as u64 > max_bytes);

        let dir = tempfile::tempdir().unwrap();
        let mut cache = HubDiskCache::open(dir.path(), max_bytes, 3600).unwrap();

        cache
            .get_or_fetch_pack(&entry_a, 1000, |_| Ok(bytes_a.clone()))
            .unwrap();
        cache
            .get_or_fetch_pack(&entry_b, 1001, |_| Ok(bytes_b.clone()))
            .unwrap();
        assert_eq!(cache.entry_count(), 2);

        // Touch corp-a from disk so corp-b becomes the least-recently-used entry.
        let hit_a = cache
            .get_or_fetch_pack(&entry_a, 1002, |_| panic!("corp-a must hit disk"))
            .unwrap();
        assert_eq!(hit_a.outcome, CacheLookupOutcome::HitFromDisk);

        // Inserting corp-c must evict corp-b while retaining corp-a.
        let ins_c = cache
            .get_or_fetch_pack(&entry_c, 1003, |_| Ok(bytes_c.clone()))
            .unwrap();
        assert_eq!(ins_c.evicted_slugs, vec!["corp-b".to_string()]);
        assert!(cache.contains_corpus("corp-a"));
        assert!(!cache.contains_corpus("corp-b"));
        assert!(cache.contains_corpus("corp-c"));
        assert!(!dir.path().join("pack-corp-b.json").exists());
        assert!(cache.used_bytes() <= max_bytes);
        assert_eq!(cache.evictions(), 1);
    }

    #[test]
    fn single_entry_exceeding_max_bytes_is_refused_without_evicting_existing() {
        let pin = "2222222222222222222222222222222222222222";
        let small = sample_pack_bytes("small", pin, LicenceClass::AdaptWithAttribution, 10);
        let huge = sample_pack_bytes("huge", pin, LicenceClass::AdaptWithAttribution, 4096);
        let entry_small =
            sample_catalogue_entry("small", pin, &small, LicenceClass::AdaptWithAttribution);
        let entry_huge =
            sample_catalogue_entry("huge", pin, &huge, LicenceClass::AdaptWithAttribution);

        let dir = tempfile::tempdir().unwrap();
        let mut cache = HubDiskCache::open(dir.path(), small.len() as u64 + 100, 3600).unwrap();
        cache
            .get_or_fetch_pack(&entry_small, 1000, |_| Ok(small.clone()))
            .unwrap();

        let err = cache
            .get_or_fetch_pack(&entry_huge, 1001, |_| Ok(huge.clone()))
            .expect_err("oversized entry must fail loudly");
        assert!(
            matches!(err, HubModeError::EntryExceedsCacheBudget { slug, .. } if slug == "huge")
        );
        assert!(
            cache.contains_corpus("small"),
            "existing entry must remain intact"
        );
    }

    #[test]
    fn staleness_is_reported_honestly_in_freshness_and_answer_warnings() {
        let pin = "3333333333333333333333333333333333333333";
        let pack_bytes = sample_pack_bytes("zstd", pin, LicenceClass::AdaptWithAttribution, 16);
        let entry =
            sample_catalogue_entry("zstd", pin, &pack_bytes, LicenceClass::AdaptWithAttribution);
        let catalogue = sample_catalogue(vec![entry]);

        let dir = tempfile::tempdir().unwrap();
        let mode =
            McpRuntimeMode::detect("http://127.0.0.1:9200", DEFAULT_HUB_URL, 50, |_, _| None);
        let cache = HubDiskCache::open(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 60).unwrap();
        let mut router = HubFirstRouter::new(mode, cache);

        let fresh_res = router
            .query_corpus(
                HubQueryRequest {
                    slug: "zstd",
                    query: "LZ4_compress_default",
                    mode: AnswerMode::Def,
                    age_days: 1,
                    now_unix: 1_000,
                },
                || Ok(catalogue.clone()),
                |_| Ok(pack_bytes.clone()),
            )
            .unwrap();
        assert!(!fresh_res.freshness.is_stale());

        // Query 120s later (TTL is 60s): served from disk cache, marked Stale with explicit warnings.
        let stale_res = router
            .query_corpus(
                HubQueryRequest {
                    slug: "zstd",
                    query: "LZ4_compress_default",
                    mode: AnswerMode::Def,
                    age_days: 1,
                    now_unix: 1_120,
                },
                || panic!("no catalogue fetch"),
                |_| panic!("no pack fetch"),
            )
            .unwrap();
        assert_eq!(stale_res.cache_outcome, CacheLookupOutcome::HitFromDisk);
        assert_eq!(
            stale_res.freshness,
            Freshness::Stale {
                age_secs: 120,
                max_age_secs: 60
            }
        );
        assert!(stale_res
            .answer
            .warnings
            .iter()
            .any(|w| w.contains("cached corpus 'zstd' is stale (age 120s exceeds ttl 60s)")));
        assert!(stale_res
            .rendered
            .contains("warning: cached corpus 'zstd' is stale"));
    }

    #[test]
    fn local_node_reachable_routes_private_index_tools_to_node_and_keeps_hub_tools() {
        let mode = McpRuntimeMode::detect(
            "http://127.0.0.1:9200/",
            DEFAULT_HUB_URL,
            DEFAULT_PROBE_BUDGET_MS,
            |_url, _budget| Some(12),
        );
        assert!(mode.has_local_node());
        assert!(mode.fallback_notice().is_none());

        let dir = tempfile::tempdir().unwrap();
        let cache = HubDiskCache::open(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600).unwrap();
        let router = HubFirstRouter::new(mode, cache);

        for node_tool in ["xerj_search", "xerj_def", "xerj_map", "xerj_status"] {
            assert_eq!(
                router.route_tool(node_tool).unwrap(),
                RouteTarget::LocalNode {
                    base_url: "http://127.0.0.1:9200".to_string()
                }
            );
        }
        assert_eq!(
            router.route_tool("xerj_hub_ask").unwrap(),
            RouteTarget::HubClient {
                hub_url: DEFAULT_HUB_URL.to_string(),
                fallback_notice: None,
            }
        );

        // Conversely, in HubOnlyFallback, private-index tools return a loud error.
        let fallback_mode =
            McpRuntimeMode::detect("http://127.0.0.1:9200", DEFAULT_HUB_URL, 50, |_, _| None);
        let fallback_cache =
            HubDiskCache::open(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600).unwrap();
        let fallback_router = HubFirstRouter::new(fallback_mode, fallback_cache);
        let err = fallback_router
            .route_tool("xerj_search")
            .expect_err("private tool without node must error");
        assert!(
            matches!(err, HubModeError::LocalNodeRequired { tool, .. } if tool == "xerj_search")
        );
    }

    #[test]
    fn unknown_corpus_returns_loud_error_never_empty_result() {
        let pin = "4444444444444444444444444444444444444444";
        let pack_bytes = sample_pack_bytes("lz4", pin, LicenceClass::AdaptWithAttribution, 16);
        let entry =
            sample_catalogue_entry("lz4", pin, &pack_bytes, LicenceClass::AdaptWithAttribution);
        let catalogue = sample_catalogue(vec![entry]);

        let dir = tempfile::tempdir().unwrap();
        let mode =
            McpRuntimeMode::detect("http://127.0.0.1:9200", DEFAULT_HUB_URL, 50, |_, _| None);
        let cache = HubDiskCache::open(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600).unwrap();
        let mut router = HubFirstRouter::new(mode, cache);

        let err = router
            .query_corpus(
                HubQueryRequest {
                    slug: "nonexistent-corpus",
                    query: "anything",
                    mode: AnswerMode::How,
                    age_days: 1,
                    now_unix: 1_700_000_000,
                },
                || Ok(catalogue),
                |_| unreachable!("pack fetch must not run for unknown corpus"),
            )
            .expect_err("unknown corpus must return a loud error");

        assert!(matches!(
            err,
            HubModeError::UnknownCorpus { ref slug, .. } if slug == "nonexistent-corpus"
        ));
    }

    #[test]
    fn invalid_corpus_slug_is_refused_before_any_io_or_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let mode =
            McpRuntimeMode::detect("http://127.0.0.1:9200", DEFAULT_HUB_URL, 50, |_, _| None);
        let cache = HubDiskCache::open(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600).unwrap();
        let mut router = HubFirstRouter::new(mode, cache);

        let overlong = "a".repeat(MAX_SLUG_BYTES + 1);
        for bad_slug in [
            "",
            "../etc/passwd",
            "UPPERCASE",
            "-leading-dash",
            "a/b",
            overlong.as_str(),
        ] {
            let err = router
                .query_corpus(
                    HubQueryRequest {
                        slug: bad_slug,
                        query: "q",
                        mode: AnswerMode::Def,
                        age_days: 1,
                        now_unix: 1_000,
                    },
                    || panic!("must not fetch catalogue for invalid slug"),
                    |_| panic!("must not fetch pack for invalid slug"),
                )
                .expect_err("invalid slug must fail loudly");
            assert!(
                matches!(err, HubModeError::InvalidCorpusSlug { .. }),
                "{bad_slug}"
            );
        }
    }

    #[test]
    fn unsupported_catalogue_or_pack_format_version_is_refused_loudly() {
        let pin = "5555555555555555555555555555555555555555";
        let pack_bytes = sample_pack_bytes("lz4", pin, LicenceClass::AdaptWithAttribution, 16);
        let entry =
            sample_catalogue_entry("lz4", pin, &pack_bytes, LicenceClass::AdaptWithAttribution);

        let dir = tempfile::tempdir().unwrap();
        let mut cache = HubDiskCache::open(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600).unwrap();

        // Future catalogue format_version (e.g. 99) -> loud UnsupportedFormatVersion.
        let mut bad_cat = sample_catalogue(vec![entry.clone()]);
        bad_cat.format_version = CATALOGUE_FORMAT_VERSION + 1;
        let err = cache
            .put_catalogue(&bad_cat, 1_000)
            .expect_err("unsupported catalogue format must fail");
        assert!(matches!(
            err,
            HubModeError::UnsupportedFormatVersion {
                kind: "catalogue",
                found,
                ..
            } if found == CATALOGUE_FORMAT_VERSION + 1
        ));

        // Future pack manifest format_version -> loud UnsupportedFormatVersion.
        let mut bad_pack: CachedCorpusPack = serde_json::from_slice(&pack_bytes).unwrap();
        bad_pack.manifest.format_version = PACK_FORMAT_VERSION + 1;
        let bad_pack_bytes = serde_json::to_vec(&bad_pack).unwrap();
        let bad_entry = sample_catalogue_entry(
            "lz4",
            pin,
            &bad_pack_bytes,
            LicenceClass::AdaptWithAttribution,
        );
        let err = cache
            .get_or_fetch_pack(&bad_entry, 1_000, |_| Ok(bad_pack_bytes))
            .expect_err("unsupported pack format must fail");
        assert!(matches!(
            err,
            HubModeError::UnsupportedFormatVersion {
                kind: "pack_manifest",
                found,
                ..
            } if found == PACK_FORMAT_VERSION + 1
        ));
    }

    #[test]
    fn sha256_mismatch_and_excluded_or_approach_only_licences_are_enforced() {
        let pin = "6666666666666666666666666666666666666666";
        let pack_bytes = sample_pack_bytes("gpl-lib", pin, LicenceClass::ApproachOnly, 16);
        let entry_approach =
            sample_catalogue_entry("gpl-lib", pin, &pack_bytes, LicenceClass::ApproachOnly);
        let entry_excluded =
            sample_catalogue_entry("proprietary", pin, &pack_bytes, LicenceClass::Excluded);
        let catalogue = sample_catalogue(vec![entry_approach.clone(), entry_excluded]);

        let dir = tempfile::tempdir().unwrap();
        let mode =
            McpRuntimeMode::detect("http://127.0.0.1:9200", DEFAULT_HUB_URL, 50, |_, _| None);
        let cache = HubDiskCache::open(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600).unwrap();
        let mut router = HubFirstRouter::new(mode, cache);

        // 1. Excluded corpus licence is loudly refused.
        let excl_err = router
            .query_corpus(
                HubQueryRequest {
                    slug: "proprietary",
                    query: "LZ4_compress_default",
                    mode: AnswerMode::Def,
                    age_days: 1,
                    now_unix: 1_000,
                },
                || Ok(catalogue.clone()),
                |_| panic!("excluded pack must never be fetched"),
            )
            .expect_err("excluded corpus must fail");
        assert!(
            matches!(excl_err, HubModeError::ExcludedCorpusLicence { slug, .. } if slug == "proprietary")
        );

        // 2. Tampered download bytes fail SHA-256 verification before touching disk.
        let tamper_err = router
            .query_corpus(
                HubQueryRequest {
                    slug: "gpl-lib",
                    query: "LZ4_compress_default",
                    mode: AnswerMode::Def,
                    age_days: 1,
                    now_unix: 1_000,
                },
                || unreachable!("catalogue already cached"),
                |_| Ok(b"tampered-bytes".to_vec()),
            )
            .expect_err("tampered pack must fail sha256 verification");
        assert!(
            matches!(tamper_err, HubModeError::IntegrityMismatch { slug, .. } if slug == "gpl-lib")
        );

        // 3. Valid ApproachOnly corpus succeeds and attaches licence_warning on every hit.
        let approach_res = router
            .query_corpus(
                HubQueryRequest {
                    slug: "gpl-lib",
                    query: "LZ4_compress_default",
                    mode: AnswerMode::Def,
                    age_days: 1,
                    now_unix: 1_000,
                },
                || unreachable!("catalogue already cached"),
                |_| Ok(pack_bytes.clone()),
            )
            .expect("valid approach-only corpus succeeds");
        assert_eq!(approach_res.answer.hits.len(), 1);
        assert!(approach_res.answer.hits[0]
            .licence_warning
            .as_deref()
            .unwrap()
            .contains("approach-only licence"));
    }

    #[test]
    fn corrupted_on_disk_pack_file_is_detected_and_refused_loudly() {
        let pin = "7777777777777777777777777777777777777777";
        let pack_bytes = sample_pack_bytes("brotli", pin, LicenceClass::AdaptWithAttribution, 16);
        let entry = sample_catalogue_entry(
            "brotli",
            pin,
            &pack_bytes,
            LicenceClass::AdaptWithAttribution,
        );

        let dir = tempfile::tempdir().unwrap();
        let mut cache = HubDiskCache::open(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600).unwrap();
        cache
            .get_or_fetch_pack(&entry, 1_000, |_| Ok(pack_bytes.clone()))
            .unwrap();

        // Corrupt the on-disk pack file after caching.
        std::fs::write(dir.path().join("pack-brotli.json"), b"corrupted").unwrap();

        let err = cache
            .get_or_fetch_pack(&entry, 1_005, |_| {
                panic!("must detect corruption before refetch")
            })
            .expect_err("corrupted on-disk pack must fail integrity check");
        assert!(matches!(err, HubModeError::IntegrityMismatch { slug, .. } if slug == "brotli"));
    }

    #[test]
    fn pin_sha_rotation_in_catalogue_fetches_updated_pack_and_replaces_cached_entry() {
        let pin_v1 = "8888888888888888888888888888888888888888";
        let pin_v2 = "9999999999999999999999999999999999999999";
        let bytes_v1 = sample_pack_bytes("snappy", pin_v1, LicenceClass::AdaptWithAttribution, 16);
        let bytes_v2 = sample_pack_bytes("snappy", pin_v2, LicenceClass::AdaptWithAttribution, 24);
        let entry_v1 = sample_catalogue_entry(
            "snappy",
            pin_v1,
            &bytes_v1,
            LicenceClass::AdaptWithAttribution,
        );
        let entry_v2 = sample_catalogue_entry(
            "snappy",
            pin_v2,
            &bytes_v2,
            LicenceClass::AdaptWithAttribution,
        );

        let dir = tempfile::tempdir().unwrap();
        let mut cache = HubDiskCache::open(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600).unwrap();

        let r1 = cache
            .get_or_fetch_pack(&entry_v1, 1_000, |_| Ok(bytes_v1.clone()))
            .unwrap();
        assert_eq!(r1.outcome, CacheLookupOutcome::FetchedAndCached);
        assert_eq!(cache.entry_meta("snappy").unwrap().pin_sha, pin_v1);

        let r2 = cache
            .get_or_fetch_pack(&entry_v2, 2_000, |_| Ok(bytes_v2.clone()))
            .unwrap();
        assert_eq!(r2.outcome, CacheLookupOutcome::FetchedAndCached);
        assert_eq!(cache.entry_meta("snappy").unwrap().pin_sha, pin_v2);
        assert_eq!(cache.entry_count(), 1);
    }
}
