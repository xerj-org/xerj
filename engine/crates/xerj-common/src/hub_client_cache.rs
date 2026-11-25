//! The client/cache library for a hub-first MCP runtime (#2760, epic #2769).
//!
//! **Status: wired into `xerj mcp`, opt-in.** `xerj-mcp`'s `xerj_hub_find`,
//! `xerj_hub_def` and `xerj_hub_ask` (`src/hub/first.rs`) answer through this
//! module when `XERJ_HUB_CACHE_DIR` (or `--hub-cache-dir`) is set and no node
//! answers at the URL they proxy to; with a node answering, or the setting
//! unset, they run the `xerj ask` flow as before. `xerj-server` does not use
//! it. `docs/mcp-hub-mode.md` has the whole contract. This module is the part
//! that needs no running node:
//!
//! 1. **Mode detection and routing ([`McpRuntimeMode`], [`HubFirstRouter`])**:
//!    probe the local node within a fixed budget ([`DEFAULT_PROBE_BUDGET_MS`]),
//!    say which tools need it, and refuse `xerj_hub_*` tools in a build with no
//!    hub key, as `xerj mcp` does today.
//! 2. **A verified, bounded disk cache ([`HubDiskCache`])** for the hub
//!    catalogue and for real hub packs. A pack is the signed
//!    `<slug>-<pin8>.hubpack.tar.zst` the hub serves (format: `xerj-hub`
//!    `pack.rs`), stored as downloaded with its detached `.sig`. Every read
//!    re-checks the archive's sha256 against the catalogue's `pack_sha256` and
//!    the signature against the hub key ([`crate::hub::hub_pubkey`]), then reads
//!    `pack.json`, `defs.jsonl` and `files.jsonl` in memory under the same size
//!    and member caps the client's extractor uses. The disk budget defaults to
//!    [`crate::hub::DISK_BUDGET_BYTES`], the ceiling `xerj hub pull` prunes to.
//! 3. **Cross-process safety.** Each agent session starts its own process, and
//!    they share one cache directory. Index updates and eviction run under the
//!    advisory file lock `cache.lock` ([`crate::flock`], the lock the node start
//!    and `xerj hub pull` take), re-reading `index.json` inside the lock.
//! 4. **Loud errors ([`HubModeError`])**: unknown corpus, bad slug, excluded
//!    licence, digest or signature mismatch, unsupported format, empty query —
//!    never a silent empty result.
//!
//! A stale or damaged entry is re-fetched, not served: a stale entry that still
//! verifies is the fallback only when the fetch itself fails, and says so.
//!
//! What a node-free pack can answer: a definition lookup ([`AnswerMode::Def`],
//! from `defs.jsonl`) and a file list ([`AnswerMode::Files`], from
//! `files.jsonl`). Passage search ([`AnswerMode::How`]) needs the pack's
//! `snapshot/` restored into a node, which this module does not do, so it is
//! refused with [`HubModeError::PassageSearchNeedsNode`].

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use ring::signature::UnparsedPublicKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::flock::{held_by, FileLock, LockError};
use crate::fsio::write_file_durable;
use crate::hub::{
    approach_only_warning, catalogue_format_supported, hub_pubkey, render, Answer, AnswerMode,
    AnsweredFrom, Catalogue, CatalogueEntry, DefRecord, Hit, LicenceClass, PackManifest,
    CATALOGUE_FORMAT_VERSION, CATALOGUE_FORMAT_VERSION_MIN, DEFAULT_HUB_URL, DISK_BUDGET_BYTES,
    NO_HUB_KEY_MESSAGE, PACK_FORMAT_VERSION,
};

/// Default per-session local-node reachability probe budget in milliseconds.
pub const DEFAULT_PROBE_BUDGET_MS: u64 = 150;

/// Default disk cache budget: the ceiling `xerj hub pull` keeps the pack cache
/// under ([`DISK_BUDGET_BYTES`]).
pub const DEFAULT_MCP_CACHE_MAX_BYTES: u64 = DISK_BUDGET_BYTES;

/// Default staleness TTL in seconds: an entry whose age reaches it is re-fetched
/// (24 hours).
pub const DEFAULT_CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// Default cap on the hits one answer carries.
pub const DEFAULT_MAX_HITS: usize = 50;

/// How long a cache operation waits for another process to let go of
/// `cache.lock`. The lock is held only for an index read-modify-write (never
/// across a download), so this is long.
pub const DEFAULT_LOCK_WAIT: Duration = Duration::from_secs(10);

/// Maximum corpus slug length in bytes (matches the hub server's `valid_slug` cap).
pub const MAX_SLUG_BYTES: usize = 64;

/// Largest decompressed tar a reader accepts. The same cap `xerj-hub`'s
/// `pack::MAX_UNPACKED_BYTES` applies, so the catalogue never offers a pack this
/// reader refuses; an unbounded read would let a small archive expand to
/// terabytes in memory.
pub const MAX_UNPACKED_BYTES: u64 = 1 << 30;

/// Largest number of tar entries a pack may hold (`xerj-hub`'s
/// `pack::MAX_MEMBERS`). A zip-bomb guard.
pub const MAX_MEMBERS: usize = 100_000;

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

/// Archive member names (fixed by the pack format).
const PACK_JSON: &str = "pack.json";
const SHA256SUMS: &str = "SHA256SUMS";
const DEFS_JSONL: &str = "defs.jsonl";
const FILES_JSONL: &str = "files.jsonl";

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

    #[error("signature verification failed for '{subject}': {reason}")]
    SignatureInvalid { subject: String, reason: String },

    #[error("{}", NO_HUB_KEY_MESSAGE)]
    NoHubKey,

    #[error("hub pack '{slug}' is not a readable .hubpack.tar.zst: {reason}")]
    InvalidPack { slug: String, reason: String },

    #[error("cache entry '{slug}' ({entry_bytes} bytes) exceeds total disk cache budget ({max_bytes} bytes)")]
    EntryExceedsCacheBudget {
        slug: String,
        entry_bytes: u64,
        max_bytes: u64,
    },

    #[error("tool '{tool}' requires a running local xerj node ({notice})")]
    LocalNodeRequired { tool: String, notice: String },

    #[error("an empty query matches every definition: a {mode} lookup needs a symbol")]
    EmptyQuery { mode: &'static str },

    #[error("corpus '{slug}': passage questions need the pack's search index, which only a running node restores from the pack snapshot; use a definition or file-list lookup, or start a node")]
    PassageSearchNeedsNode { slug: String },

    #[error("hub cache lock {path}: {reason}")]
    CacheLock { path: String, reason: String },

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

fn decode_hex_exact(hex: &str, bytes: usize, what: &str) -> Result<Vec<u8>, String> {
    if hex.len() != bytes * 2 || !hex.is_ascii() {
        return Err(format!("{what} must be {} hex characters", bytes * 2));
    }
    (0..bytes)
        .map(|i| {
            u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|_| format!("{what} is not hex"))
        })
        .collect()
}

/// Verify a detached ed25519 signature (64 raw bytes as 128 hex characters, the
/// form the hub's `.sig` files carry) over `message` against a hex public key.
/// The same primitive and key format `xerj-autoindex`'s `harvest::sign` uses to
/// sign and verify a pack archive.
pub fn verify_ed25519_hex(message: &[u8], sig_hex: &str, pubkey_hex: &str) -> Result<(), String> {
    let sig = decode_hex_exact(sig_hex.trim(), 64, "signature")?;
    let key = decode_hex_exact(pubkey_hex.trim(), 32, "public key")?;
    UnparsedPublicKey::new(&ring::signature::ED25519, &key)
        .verify(message, &sig)
        .map_err(|_| {
            "signature does not verify: the bytes are not from the holder of the hub key, \
             or changed after signing"
                .to_string()
        })
}

/// Parse catalogue bytes, refusing a `format_version` this build cannot read
/// before the strict parse, so a future shape is named as unsupported instead of
/// failing on its new fields.
fn parse_catalogue(bytes: &[u8]) -> Result<Catalogue, HubModeError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let version = value
        .get("format_version")
        .and_then(serde_json::Value::as_u64)
        .map(|v| u32::try_from(v).unwrap_or(u32::MAX));
    if let Some(found) = version {
        if !catalogue_format_supported(found) {
            return Err(HubModeError::UnsupportedFormatVersion {
                kind: "catalogue",
                found,
                supported_min: CATALOGUE_FORMAT_VERSION_MIN,
                supported_max: CATALOGUE_FORMAT_VERSION,
            });
        }
    }
    let catalogue: Catalogue = serde_json::from_value(value)?;
    validate_catalogue_format(&catalogue)?;
    Ok(catalogue)
}

/// True when a catalogue `generated_at` (RFC 3339) is older than the cached
/// one's. An unparsable cached time cannot anchor a comparison, so it never
/// blocks; an unparsable fetched time against a parsable cached one cannot be
/// shown to be newer, so it is refused.
fn is_rollback(fetched: &str, cached: &str) -> bool {
    let Ok(cached) = chrono::DateTime::parse_from_rfc3339(cached.trim()) else {
        return false;
    };
    match chrono::DateTime::parse_from_rfc3339(fetched.trim()) {
        Ok(fetched) => fetched < cached,
        Err(_) => true,
    }
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

/// Runtime mode `xerj mcp` detects for a hub tool call: it probes the node it
/// proxies to each time, so a node started (or stopped) mid-session is noticed.
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
    /// Cached entry age is below `max_age_secs`.
    Fresh { age_secs: u64, max_age_secs: u64 },
    /// Cached entry age has reached `max_age_secs`; surfaced explicitly to callers.
    Stale { age_secs: u64, max_age_secs: u64 },
}

impl Freshness {
    /// Compute freshness from `fetched_at_unix` vs `now_unix` and `ttl_secs`: an
    /// entry is stale once `now_unix >= fetched_at_unix + ttl_secs`.
    pub fn evaluate(fetched_at_unix: u64, now_unix: u64, ttl_secs: u64) -> Self {
        let age_secs = now_unix.saturating_sub(fetched_at_unix);
        if age_secs < ttl_secs {
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

    /// True if this artifact has reached its configured TTL.
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
                "cached {subject} is stale (age {age_secs}s, ttl {max_age_secs}s)"
            )),
        }
    }
}

/// Whether a corpus/catalogue lookup was served from disk cache or fetched from the hub.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheLookupOutcome {
    /// Served directly from the verified on-disk cache with zero network requests,
    /// or, for a stale entry whose refresh failed, with the failure reported.
    HitFromDisk,
    /// Fetched from the remote hub, verified, and persisted to the disk cache.
    FetchedAndCached,
}

/// Bytes the hub serves, with the detached signature from the entry's `sig_url`
/// (the catalogue's `.sig`, or the pack's `<archive>.sig`): 128 hex characters.
#[derive(Debug, Clone)]
pub struct SignedDownload {
    pub bytes: Vec<u8>,
    pub signature_hex: String,
}

/// One line of a pack's `files.jsonl`: an indexed source file with its
/// per-path licence (`xerj-hub`'s `PackFileEntry`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackFileEntry {
    pub path: String,
    pub lang: String,
    pub bytes: u64,
    pub licence: String,
}

/// The members of a verified hub pack this module reads node-free: `pack.json`,
/// `defs.jsonl` and `files.jsonl`. The opaque `snapshot/` is checked against
/// `SHA256SUMS` but not read into memory.
#[derive(Debug, Clone)]
pub struct CachedCorpusPack {
    pub manifest: PackManifest,
    pub defs: Vec<DefRecord>,
    pub files: Vec<PackFileEntry>,
}

impl CachedCorpusPack {
    /// Validate internal format version, slug, and pin SHA.
    pub fn validate(&self) -> Result<(), HubModeError> {
        validate_pack_manifest_format(&self.manifest)
    }
}

/// `/`-joined relative path of a tar entry; rejects traversal and absolute paths.
fn sanitize_archive_path(path: &Path) -> Result<String, String> {
    let mut parts = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Normal(s) => parts.push(
                s.to_str()
                    .ok_or_else(|| "non-UTF-8 path in archive".to_string())?
                    .to_string(),
            ),
            Component::CurDir => {}
            _ => return Err(format!("unsafe path in archive: {}", path.display())),
        }
    }
    if parts.is_empty() {
        return Err("empty path in archive".to_string());
    }
    Ok(parts.join("/"))
}

fn parse_jsonl<T: serde::de::DeserializeOwned>(name: &str, bytes: &[u8]) -> Result<Vec<T>, String> {
    let mut out = Vec::new();
    for (n, line) in bytes.split(|b| *b == b'\n').enumerate() {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        out.push(serde_json::from_slice(line).map_err(|e| format!("{name} line {}: {e}", n + 1))?);
    }
    Ok(out)
}

/// Read a `.hubpack.tar.zst` in memory. Nothing is extracted to disk; the
/// decompressed stream is capped at `max_unpacked` bytes and `max_members`
/// entries however well it compresses, every member is hashed as it streams, and
/// `SHA256SUMS` must cover and match `pack.json`, `defs.jsonl` and `files.jsonl`.
fn decode_pack_archive_bounded(
    slug: &str,
    archive: &[u8],
    max_unpacked: u64,
    max_members: usize,
) -> Result<CachedCorpusPack, HubModeError> {
    let bad = |reason: String| HubModeError::InvalidPack {
        slug: slug.to_string(),
        reason,
    };
    let decoder = zstd::stream::read::Decoder::new(archive)
        .map_err(|e| bad(format!("zstd decompress: {e}")))?;
    let mut tar = tar::Archive::new(decoder.take(max_unpacked.saturating_add(1)));
    let entries = tar
        .entries()
        .map_err(|e| bad(format!("read tar entries: {e}")))?;

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut digests: BTreeMap<String, String> = BTreeMap::new();
    let mut kept: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut count = 0usize;
    let mut unpacked = 0u64;
    let mut chunk = vec![0u8; 64 * 1024];
    for entry in entries {
        let mut entry = entry.map_err(|e| {
            bad(format!(
                "tar entry unreadable (corrupt, or it decompresses to more than {max_unpacked} bytes): {e}"
            ))
        })?;
        count += 1;
        if count > max_members {
            return Err(bad(format!(
                "pack holds more than {max_members} entries, over the hub's per-pack cap"
            )));
        }
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let name = entry
            .path()
            .map_err(|e| bad(format!("tar entry path: {e}")))
            .and_then(|p| sanitize_archive_path(p.as_ref()).map_err(bad))?;
        unpacked = unpacked.saturating_add(entry.size());
        if unpacked > max_unpacked {
            return Err(bad(format!(
                "pack decompresses to more than {max_unpacked} bytes, over the hub's per-pack cap"
            )));
        }
        if !seen.insert(name.clone()) {
            return Err(bad(format!("duplicate archive member {name}")));
        }
        let keep = matches!(
            name.as_str(),
            PACK_JSON | SHA256SUMS | DEFS_JSONL | FILES_JSONL
        );
        let mut hasher = Sha256::new();
        let mut body = Vec::new();
        loop {
            let n = entry
                .read(&mut chunk)
                .map_err(|e| bad(format!("read archive member {name}: {e}")))?;
            if n == 0 {
                break;
            }
            hasher.update(&chunk[..n]);
            if keep {
                body.extend_from_slice(&chunk[..n]);
            }
        }
        digests.insert(name.clone(), format!("{:x}", hasher.finalize()));
        if keep {
            kept.insert(name, body);
        }
    }

    let sums = kept
        .get(SHA256SUMS)
        .ok_or_else(|| bad(format!("{SHA256SUMS} missing from archive")))?;
    let sums = std::str::from_utf8(sums).map_err(|_| bad(format!("{SHA256SUMS} is not UTF-8")))?;
    let mut listed: BTreeSet<&str> = BTreeSet::new();
    for line in sums.lines().filter(|l| !l.trim().is_empty()) {
        let (hash, path) = line
            .split_once("  ")
            .ok_or_else(|| bad(format!("malformed {SHA256SUMS} line: {line:?}")))?;
        let actual = digests
            .get(path)
            .ok_or_else(|| bad(format!("{SHA256SUMS} names {path}, absent from archive")))?;
        if !actual.eq_ignore_ascii_case(hash.trim()) {
            return Err(bad(format!("checksum mismatch for {path}")));
        }
        listed.insert(path);
    }
    for needed in [PACK_JSON, DEFS_JSONL, FILES_JSONL] {
        if !kept.contains_key(needed) {
            return Err(bad(format!("{needed} missing from archive")));
        }
        if !listed.contains(needed) {
            return Err(bad(format!("{needed} is not covered by {SHA256SUMS}")));
        }
    }

    // A future format is named as unsupported, not half-read.
    let manifest_value: serde_json::Value = serde_json::from_slice(&kept[PACK_JSON])
        .map_err(|e| bad(format!("parse {PACK_JSON}: {e}")))?;
    if let Some(found) = manifest_value
        .get("format_version")
        .and_then(serde_json::Value::as_u64)
        .map(|v| u32::try_from(v).unwrap_or(u32::MAX))
    {
        if found != PACK_FORMAT_VERSION {
            return Err(HubModeError::UnsupportedFormatVersion {
                kind: "pack_manifest",
                found,
                supported_min: PACK_FORMAT_VERSION,
                supported_max: PACK_FORMAT_VERSION,
            });
        }
    }
    let manifest: PackManifest = serde_json::from_value(manifest_value)
        .map_err(|e| bad(format!("parse {PACK_JSON}: {e}")))?;
    let defs = parse_jsonl::<DefRecord>(DEFS_JSONL, &kept[DEFS_JSONL]).map_err(bad)?;
    let files = parse_jsonl::<PackFileEntry>(FILES_JSONL, &kept[FILES_JSONL]).map_err(bad)?;
    let pack = CachedCorpusPack {
        manifest,
        defs,
        files,
    };
    pack.validate()?;
    Ok(pack)
}

/// [`decode_pack_archive_bounded`] under the shared caps.
fn decode_pack_archive(slug: &str, archive: &[u8]) -> Result<CachedCorpusPack, HubModeError> {
    decode_pack_archive_bounded(slug, archive, MAX_UNPACKED_BYTES, MAX_MEMBERS)
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
    /// sha256 of the archive, as the catalogue's `pack_sha256` records it.
    pub sha256: String,
    /// Archive plus signature bytes on disk.
    pub size_bytes: u64,
    pub fetched_at_unix: u64,
    pub last_accessed_seq: u64,
    pub file_name: String,
    pub sig_file_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogueMeta {
    sha256: String,
    fetched_at_unix: u64,
}

/// Persistent LRU index stored at `<cache_dir>/index.json`. Shared by every
/// process using the directory: read and written only under `cache.lock`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiskCacheIndex {
    format_version: u32,
    access_clock: u64,
    catalogue: Option<CatalogueMeta>,
    entries: BTreeMap<String, DiskCacheEntryMeta>,
}

/// 2: archives and signatures as the hub serves them. Version 1 held a
/// private JSON pack format the hub never served; such an index is rebuilt.
const CACHE_INDEX_FORMAT_VERSION: u32 = 2;
const CACHE_INDEX_FILE: &str = "index.json";
const CACHE_LOCK_FILE: &str = "cache.lock";
const CACHE_CATALOGUE_FILE: &str = "catalogue.json";
const CACHE_CATALOGUE_SIG_FILE: &str = "catalogue.json.sig";
const PACK_FILE_PREFIX: &str = "pack-";

impl Default for DiskCacheIndex {
    fn default() -> Self {
        Self {
            format_version: CACHE_INDEX_FORMAT_VERSION,
            access_clock: 0,
            catalogue: None,
            entries: BTreeMap::new(),
        }
    }
}

impl DiskCacheIndex {
    fn used_bytes(&self) -> u64 {
        self.entries.values().map(|e| e.size_bytes).sum()
    }

    fn references(&self, file_name: &str) -> bool {
        self.entries
            .values()
            .any(|m| m.file_name == file_name || m.sig_file_name == file_name)
    }
}

fn plain_file_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
}

/// Read `index.json`. `Ok((index, true))` means it was unreadable (corrupt JSON,
/// the unserved version-1 layout, an unsafe file name) and an empty index is
/// returned to be rebuilt. An index a newer build wrote is an error: it is not
/// ours to discard.
fn load_index(root: &Path) -> Result<(DiskCacheIndex, bool), HubModeError> {
    let raw = match std::fs::read(root.join(CACHE_INDEX_FILE)) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((DiskCacheIndex::default(), false))
        }
        Err(e) => return Err(e.into()),
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&raw) else {
        return Ok((DiskCacheIndex::default(), true));
    };
    let version = value
        .get("format_version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    if version > u64::from(CACHE_INDEX_FORMAT_VERSION) {
        return Err(HubModeError::UnsupportedFormatVersion {
            kind: "disk_cache_index",
            found: u32::try_from(version).unwrap_or(u32::MAX),
            supported_min: CACHE_INDEX_FORMAT_VERSION,
            supported_max: CACHE_INDEX_FORMAT_VERSION,
        });
    }
    if version < u64::from(CACHE_INDEX_FORMAT_VERSION) {
        return Ok((DiskCacheIndex::default(), true));
    }
    match serde_json::from_value::<DiskCacheIndex>(value) {
        Ok(index)
            if index
                .entries
                .values()
                .all(|m| plain_file_name(&m.file_name) && plain_file_name(&m.sig_file_name)) =>
        {
            Ok((index, false))
        }
        _ => Ok((DiskCacheIndex::default(), true)),
    }
}

/// Removes pack files no index entry names: what a crash between writing a pack
/// and recording it leaves behind, and what a discarded index orphans. Called
/// only under the lock, which every writer holds from the file write to the
/// index write, so it cannot delete a file about to be indexed.
fn sweep_orphans(root: &Path, index: &DiskCacheIndex) {
    let Ok(dir) = std::fs::read_dir(root) else {
        return;
    };
    for item in dir.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        if name.starts_with(PACK_FILE_PREFIX) && !index.references(&name) {
            let _ = std::fs::remove_file(item.path());
        }
    }
}

fn lock_error(path: &Path, wait: Duration, error: LockError) -> HubModeError {
    let reason = match error {
        LockError::Timeout { holder } => format!(
            "another xerj process held it for {}ms{}",
            wait.as_millis(),
            held_by(holder)
        ),
        LockError::Io(e) => e.to_string(),
    };
    HubModeError::CacheLock {
        path: path.display().to_string(),
        reason,
    }
}

/// Result of an index transaction: the value, whether the index changed, and the
/// files to delete once the new index is on disk.
struct Tx<R> {
    value: R,
    dirty: bool,
    remove_after: Vec<String>,
}

impl<R> Tx<R> {
    fn clean(value: R) -> Self {
        Self {
            value,
            dirty: false,
            remove_after: Vec::new(),
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
    /// A stale entry was served because refreshing it failed: why.
    pub refresh_error: Option<String>,
    /// The entry on disk failed verification and was replaced: why.
    pub repaired: Option<String>,
}

impl<T> ResolvedCachedArtifact<T> {
    /// One warning line per condition the caller must not hide.
    pub fn warnings(&self, subject: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(stale) = self.freshness.warning_line(subject) {
            out.push(match &self.refresh_error {
                Some(why) => {
                    format!("{stale}; refresh failed ({why}); serving the verified disk entry")
                }
                None => stale,
            });
        }
        if let Some(why) = &self.repaired {
            out.push(format!(
                "cached {subject} failed verification ({why}) and was re-fetched"
            ));
        }
        out
    }
}

/// What the on-disk entry for a lookup turned out to be.
enum Cached<T> {
    /// Nothing usable is recorded, or it is for another pin.
    Missing,
    /// Recorded, but unreadable or failing its digest, signature or format.
    Damaged(String),
    /// Verified.
    Valid { value: T, fetched_at_unix: u64 },
}

/// A refresh either completed or the fetch itself failed (the only case where a
/// verified stale entry may be served instead).
enum Refresh<T> {
    Done(T),
    FetchFailed(HubModeError),
}

/// Bounded, digest- and signature-verified LRU disk cache for the hub catalogue
/// and corpus packs, safe to share between processes.
#[derive(Debug)]
pub struct HubDiskCache {
    root: PathBuf,
    max_bytes: u64,
    ttl_secs: u64,
    pubkey_hex: Option<String>,
    lock_wait: Duration,
    disk_hits: u64,
    network_fetches: u64,
    evictions: u64,
}

impl HubDiskCache {
    /// Open or initialise a bounded disk cache at `root`, verifying with
    /// [`crate::hub::hub_pubkey`] (the compiled-in hub key, or a test
    /// `XERJ_HUB_PUBKEY`). With no key every operation refuses with
    /// [`HubModeError::NoHubKey`]. An unreadable `index.json` is rebuilt; one a
    /// newer build wrote is an error.
    pub fn open(
        root: impl Into<PathBuf>,
        max_bytes: u64,
        ttl_secs: u64,
    ) -> Result<Self, HubModeError> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        load_index(&root)?;
        Ok(Self {
            root,
            max_bytes,
            ttl_secs,
            pubkey_hex: hub_pubkey(),
            lock_wait: DEFAULT_LOCK_WAIT,
            disk_hits: 0,
            network_fetches: 0,
            evictions: 0,
        })
    }

    /// Verify with `pubkey_hex` instead of the process-wide hub key. `None`
    /// makes the cache refuse everything, as a build with no key does.
    pub fn with_pubkey_hex(mut self, pubkey_hex: Option<&str>) -> Self {
        self.pubkey_hex = pubkey_hex.map(|k| k.trim().to_string());
        self
    }

    /// How long an operation waits for `cache.lock` ([`DEFAULT_LOCK_WAIT`]).
    pub fn with_lock_wait(mut self, wait: Duration) -> Self {
        self.lock_wait = wait;
        self
    }

    /// True when this cache has a hub key to verify with.
    pub fn can_verify(&self) -> bool {
        self.pubkey_hex.is_some()
    }

    fn require_key(&self) -> Result<&str, HubModeError> {
        self.pubkey_hex.as_deref().ok_or(HubModeError::NoHubKey)
    }

    /// Directory backing this disk cache.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The lock file every process sharing this directory takes.
    pub fn lock_path(&self) -> PathBuf {
        self.root.join(CACHE_LOCK_FILE)
    }

    /// Maximum total bytes allowed across cached corpus pack files.
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// The index as last written. Read without the lock: writers replace the
    /// file atomically, and this is for reporting, not for deciding anything.
    fn snapshot(&self) -> DiskCacheIndex {
        load_index(&self.root)
            .map(|(index, _)| index)
            .unwrap_or_default()
    }

    /// Total bytes currently occupied by cached corpus pack files.
    pub fn used_bytes(&self) -> u64 {
        self.snapshot().used_bytes()
    }

    /// Number of corpus packs currently stored in the disk cache.
    pub fn entry_count(&self) -> usize {
        self.snapshot().entries.len()
    }

    /// Measured count of lookups served from the on-disk cache by this handle.
    pub fn disk_hits(&self) -> u64 {
        self.disk_hits
    }

    /// Measured count of lookups that required a remote fetch by this handle.
    pub fn network_fetches(&self) -> u64 {
        self.network_fetches
    }

    /// Entries this handle evicted under the LRU policy.
    pub fn evictions(&self) -> u64 {
        self.evictions
    }

    /// Check whether `slug` is currently present in the disk cache index.
    pub fn contains_corpus(&self, slug: &str) -> bool {
        self.snapshot().entries.contains_key(slug)
    }

    /// Retrieve metadata for `slug` if cached.
    pub fn entry_meta(&self, slug: &str) -> Option<DiskCacheEntryMeta> {
        self.snapshot().entries.get(slug).cloned()
    }

    /// Run `f` on the index under `cache.lock`, re-reading `index.json` inside
    /// the lock so another process's update is never overwritten. The index is
    /// written (and orphan pack files swept) when `f` changed it or it had to be
    /// rebuilt; the files `f` asks to remove are deleted only after that write.
    fn locked<R>(
        &self,
        f: impl FnOnce(&mut DiskCacheIndex) -> Result<Tx<R>, HubModeError>,
    ) -> Result<R, HubModeError> {
        let lock_path = self.lock_path();
        let _guard = FileLock::acquire(&lock_path, self.lock_wait)
            .map_err(|e| lock_error(&lock_path, self.lock_wait, e))?;
        let (mut index, rebuilt) = load_index(&self.root)?;
        let tx = f(&mut index)?;
        if tx.dirty || rebuilt {
            let bytes = serde_json::to_vec_pretty(&index)?;
            write_file_durable(&self.root.join(CACHE_INDEX_FILE), &bytes)?;
            for name in &tx.remove_after {
                if plain_file_name(name) && !index.references(name) {
                    let _ = std::fs::remove_file(self.root.join(name));
                }
            }
            sweep_orphans(&self.root, &index);
        }
        Ok(tx.value)
    }

    /// Verify `download` as the signed catalogue and store it, replacing any
    /// cached one. Refuses an unverifiable key, a bad signature or an unsupported
    /// `format_version` before anything is written.
    pub fn put_catalogue(
        &mut self,
        download: &SignedDownload,
        now_unix: u64,
    ) -> Result<Catalogue, HubModeError> {
        let catalogue = self.verify_catalogue_download(download)?;
        self.store_catalogue(download, now_unix)?;
        Ok(catalogue)
    }

    /// The signature over the catalogue bytes, against the hub key, then the
    /// format check and the parse.
    fn verify_catalogue_download(
        &self,
        download: &SignedDownload,
    ) -> Result<Catalogue, HubModeError> {
        let key = self.require_key()?;
        verify_ed25519_hex(&download.bytes, &download.signature_hex, key).map_err(|reason| {
            HubModeError::SignatureInvalid {
                subject: "catalogue".into(),
                reason,
            }
        })?;
        parse_catalogue(&download.bytes)
    }

    /// Write verified catalogue bytes and their signature, and record them.
    fn store_catalogue(
        &self,
        download: &SignedDownload,
        now_unix: u64,
    ) -> Result<(), HubModeError> {
        let sha256 = sha256_hex(&download.bytes);
        self.locked(|index| {
            write_file_durable(&self.root.join(CACHE_CATALOGUE_FILE), &download.bytes)?;
            write_file_durable(
                &self.root.join(CACHE_CATALOGUE_SIG_FILE),
                download.signature_hex.trim().as_bytes(),
            )?;
            index.catalogue = Some(CatalogueMeta {
                sha256,
                fetched_at_unix: now_unix,
            });
            Ok(Tx {
                value: (),
                dirty: true,
                remove_after: Vec::new(),
            })
        })
    }

    /// The cached catalogue, if one is recorded and still verifies.
    fn read_cached_catalogue(&self) -> Result<Cached<Catalogue>, HubModeError> {
        let key = self.require_key()?;
        let recorded = self.locked(|index| {
            let Some(meta) = index.catalogue.clone() else {
                return Ok(Tx::clean(None));
            };
            let read = std::fs::read(self.root.join(CACHE_CATALOGUE_FILE)).and_then(|bytes| {
                std::fs::read_to_string(self.root.join(CACHE_CATALOGUE_SIG_FILE))
                    .map(|sig| (bytes, sig))
            });
            Ok(Tx::clean(Some((meta, read.map_err(|e| e.to_string())))))
        })?;
        let Some((meta, read)) = recorded else {
            return Ok(Cached::Missing);
        };
        let damaged = |why: String| Ok(Cached::Damaged(why));
        let (bytes, sig) = match read {
            Ok(pair) => pair,
            Err(why) => return damaged(format!("cannot read the cached catalogue: {why}")),
        };
        let actual = sha256_hex(&bytes);
        if actual != meta.sha256 {
            return damaged(format!(
                "sha256 mismatch: recorded {}, on disk {actual}",
                meta.sha256
            ));
        }
        if let Err(why) = verify_ed25519_hex(&bytes, &sig, key) {
            return damaged(why);
        }
        match parse_catalogue(&bytes) {
            Ok(value) => Ok(Cached::Valid {
                value,
                fetched_at_unix: meta.fetched_at_unix,
            }),
            Err(e) => damaged(e.to_string()),
        }
    }

    /// Read the cached [`Catalogue`], verifying its digest, signature and format.
    ///
    /// A fresh entry is a disk hit and `fetch` is not called. A stale one
    /// (`now_unix >= fetched_at + ttl`), a damaged one (unreadable, digest or
    /// signature mismatch, bad format) or none at all calls `fetch`, so a pin
    /// rotation is picked up and a damaged entry repairs itself. Only a stale
    /// entry that still verifies is served when `fetch` itself fails, with
    /// [`ResolvedCachedArtifact::refresh_error`] set. Bytes `fetch` returns that
    /// fail verification, or that are an older catalogue than the cached one
    /// (a rollback), are an error, never a fallback.
    pub fn get_or_fetch_catalogue<F>(
        &mut self,
        now_unix: u64,
        fetch: F,
    ) -> Result<ResolvedCachedArtifact<Catalogue>, HubModeError>
    where
        F: FnOnce() -> Result<SignedDownload, HubModeError>,
    {
        let cached = self.read_cached_catalogue()?;
        let (stale, repaired) = match cached {
            Cached::Valid {
                value,
                fetched_at_unix,
            } => {
                let freshness = Freshness::evaluate(fetched_at_unix, now_unix, self.ttl_secs);
                if !freshness.is_stale() {
                    self.disk_hits += 1;
                    return Ok(ResolvedCachedArtifact {
                        value,
                        outcome: CacheLookupOutcome::HitFromDisk,
                        freshness,
                        evicted_slugs: Vec::new(),
                        refresh_error: None,
                        repaired: None,
                    });
                }
                (Some((value, freshness)), None)
            }
            Cached::Damaged(why) => (None, Some(why)),
            Cached::Missing => (None, None),
        };

        let downloaded = match fetch() {
            Ok(download) => download,
            Err(error) => {
                return match stale {
                    Some((value, freshness)) => {
                        self.disk_hits += 1;
                        Ok(ResolvedCachedArtifact {
                            value,
                            outcome: CacheLookupOutcome::HitFromDisk,
                            freshness,
                            evicted_slugs: Vec::new(),
                            refresh_error: Some(error.to_string()),
                            repaired: None,
                        })
                    }
                    None => Err(error),
                };
            }
        };
        let value = self.verify_catalogue_download(&downloaded)?;
        // A signature proves the hub made these bytes, not that they are its
        // current catalogue: every catalogue the hub ever signed verifies for
        // ever. A refresh that would go back in time (a replay from a mirror, a
        // proxy or a `XERJ_HUB_URL` someone else chose) would walk every pin
        // back, so it is refused, as `xerj hub sync` refuses it.
        if let Some((cached, _)) = &stale {
            if is_rollback(&value.generated_at, &cached.generated_at) {
                return Err(HubModeError::IntegrityMismatch {
                    slug: "catalogue".into(),
                    reason: format!(
                        "refusing a catalogue generated {} that is older than the cached one \
                         ({}): a signed but older catalogue is a rollback, so the cache was kept",
                        value.generated_at, cached.generated_at
                    ),
                });
            }
        }
        self.store_catalogue(&downloaded, now_unix)?;
        self.network_fetches += 1;
        Ok(ResolvedCachedArtifact {
            value,
            outcome: CacheLookupOutcome::FetchedAndCached,
            freshness: Freshness::evaluate(now_unix, now_unix, self.ttl_secs),
            evicted_slugs: Vec::new(),
            refresh_error: None,
            repaired,
        })
    }

    /// Read and verify the cached pack for `entry`: the file bytes are read under
    /// the lock, the digest, signature and archive checks run outside it.
    fn read_cached_pack(
        &self,
        entry: &CatalogueEntry,
    ) -> Result<Cached<CachedCorpusPack>, HubModeError> {
        let key = self.require_key()?;
        let recorded = self.locked(|index| {
            let Some(meta) = index.entries.get(&entry.slug).cloned() else {
                return Ok(Tx::clean(None));
            };
            if meta.pin_sha != entry.pin_sha || meta.sha256 != entry.pack_sha256 {
                return Ok(Tx::clean(None));
            }
            let read = std::fs::read(self.root.join(&meta.file_name)).and_then(|archive| {
                std::fs::read_to_string(self.root.join(&meta.sig_file_name))
                    .map(|sig| (archive, sig))
            });
            Ok(Tx::clean(Some((meta, read.map_err(|e| e.to_string())))))
        })?;
        let Some((meta, read)) = recorded else {
            return Ok(Cached::Missing);
        };
        let damaged = |why: String| Ok(Cached::Damaged(why));
        let (archive, sig) = match read {
            Ok(pair) => pair,
            Err(why) => return damaged(format!("cannot read the cached pack: {why}")),
        };
        let actual = sha256_hex(&archive);
        if actual != meta.sha256 {
            return damaged(format!(
                "on-disk pack sha256 mismatch: expected {}, got {actual}",
                meta.sha256
            ));
        }
        if let Err(why) = verify_ed25519_hex(&archive, &sig, key) {
            return damaged(why);
        }
        let pack = match decode_pack_archive(&entry.slug, &archive) {
            Ok(pack) => pack,
            Err(e) => return damaged(e.to_string()),
        };
        if pack.manifest.slug != entry.slug || pack.manifest.pin_sha != entry.pin_sha {
            return damaged(format!(
                "pack manifest ({}, {}) does not match catalogue ({}, {})",
                pack.manifest.slug, pack.manifest.pin_sha, entry.slug, entry.pin_sha
            ));
        }
        Ok(Cached::Valid {
            value: pack,
            fetched_at_unix: meta.fetched_at_unix,
        })
    }

    /// Mark `slug` as just used, if the entry for that archive is still there.
    fn touch(&self, slug: &str, sha256: &str) -> Result<(), HubModeError> {
        self.locked(|index| {
            index.access_clock += 1;
            let seq = index.access_clock;
            let found = match index.entries.get_mut(slug) {
                Some(meta) if meta.sha256 == sha256 => {
                    meta.last_accessed_seq = seq;
                    true
                }
                _ => false,
            };
            Ok(Tx {
                value: (),
                dirty: found,
                remove_after: Vec::new(),
            })
        })
    }

    /// Verify a downloaded pack against the catalogue entry: digest first, then
    /// signature, then the archive's own checks and manifest.
    fn verify_pack_download(
        &self,
        entry: &CatalogueEntry,
        download: &SignedDownload,
    ) -> Result<CachedCorpusPack, HubModeError> {
        let key = self.require_key()?;
        let actual = sha256_hex(&download.bytes);
        if actual != entry.pack_sha256 {
            return Err(HubModeError::IntegrityMismatch {
                slug: entry.slug.clone(),
                reason: format!(
                    "downloaded pack sha256 mismatch: expected {}, got {actual}",
                    entry.pack_sha256
                ),
            });
        }
        verify_ed25519_hex(&download.bytes, &download.signature_hex, key).map_err(|reason| {
            HubModeError::SignatureInvalid {
                subject: entry.slug.clone(),
                reason,
            }
        })?;
        let pack = decode_pack_archive(&entry.slug, &download.bytes)?;
        if pack.manifest.slug != entry.slug || pack.manifest.pin_sha != entry.pin_sha {
            return Err(HubModeError::IntegrityMismatch {
                slug: entry.slug.clone(),
                reason: format!(
                    "downloaded manifest ({}, {}) does not match catalogue ({}, {})",
                    pack.manifest.slug, pack.manifest.pin_sha, entry.slug, entry.pin_sha
                ),
            });
        }
        Ok(pack)
    }

    /// Fetch, verify and store the pack for `entry`, replacing any older version
    /// of the same corpus and evicting least-recently-used entries to fit.
    fn refresh_pack<F>(
        &mut self,
        entry: &CatalogueEntry,
        now_unix: u64,
        fetch_pack: F,
    ) -> Result<Refresh<(CachedCorpusPack, Vec<String>)>, HubModeError>
    where
        F: FnOnce(&CatalogueEntry) -> Result<SignedDownload, HubModeError>,
    {
        let download = match fetch_pack(entry) {
            Ok(download) => download,
            Err(error) => return Ok(Refresh::FetchFailed(error)),
        };
        let pack = self.verify_pack_download(entry, &download)?;

        let signature = download.signature_hex.trim().as_bytes();
        let entry_bytes = (download.bytes.len() + signature.len()) as u64;
        if entry_bytes > self.max_bytes {
            return Err(HubModeError::EntryExceedsCacheBudget {
                slug: entry.slug.clone(),
                entry_bytes,
                max_bytes: self.max_bytes,
            });
        }
        let pin8 = &entry.pin_sha[..8];
        let file_name = format!("{PACK_FILE_PREFIX}{}-{pin8}.hubpack.tar.zst", entry.slug);
        let sig_file_name = format!("{file_name}.sig");

        let evicted = self.locked(|index| {
            let mut remove_after = Vec::new();
            if let Some(old) = index.entries.remove(&entry.slug) {
                remove_after.push(old.file_name);
                remove_after.push(old.sig_file_name);
            }
            let mut evicted = Vec::new();
            while index.used_bytes() + entry_bytes > self.max_bytes {
                let Some(victim) = index
                    .entries
                    .values()
                    .min_by_key(|m| (m.last_accessed_seq, m.slug.clone()))
                    .map(|m| m.slug.clone())
                else {
                    break;
                };
                if let Some(removed) = index.entries.remove(&victim) {
                    remove_after.push(removed.file_name);
                    remove_after.push(removed.sig_file_name);
                    evicted.push(victim);
                }
            }
            write_file_durable(&self.root.join(&file_name), &download.bytes)?;
            write_file_durable(&self.root.join(&sig_file_name), signature)?;
            index.access_clock += 1;
            let seq = index.access_clock;
            index.entries.insert(
                entry.slug.clone(),
                DiskCacheEntryMeta {
                    slug: entry.slug.clone(),
                    pin_sha: entry.pin_sha.clone(),
                    pin_date: entry.pin_date.clone(),
                    licence_spdx: entry.licence_spdx.clone(),
                    licence_class: entry.licence_class,
                    sha256: entry.pack_sha256.clone(),
                    size_bytes: entry_bytes,
                    fetched_at_unix: now_unix,
                    last_accessed_seq: seq,
                    file_name: file_name.clone(),
                    sig_file_name: sig_file_name.clone(),
                },
            );
            Ok(Tx {
                value: evicted,
                dirty: true,
                remove_after,
            })
        })?;
        self.evictions += evicted.len() as u64;
        self.network_fetches += 1;
        Ok(Refresh::Done((pack, evicted)))
    }

    /// Resolve a corpus pack for `entry`: from disk when the recorded archive
    /// matches the entry's `pin_sha` and `pack_sha256`, still verifies against
    /// the hub key, and is fresh; otherwise via `fetch_pack`, which returns the
    /// archive and its signature.
    ///
    /// The refresh rules are those of [`Self::get_or_fetch_catalogue`]: a stale
    /// or damaged entry is re-fetched, a stale one that verifies is the fallback
    /// only when the fetch itself fails, and downloaded bytes that fail the
    /// digest, signature or archive checks are an error. A pack larger than the
    /// whole budget is refused before it is downloaded.
    pub fn get_or_fetch_pack<F>(
        &mut self,
        entry: &CatalogueEntry,
        now_unix: u64,
        fetch_pack: F,
    ) -> Result<ResolvedCachedArtifact<CachedCorpusPack>, HubModeError>
    where
        F: FnOnce(&CatalogueEntry) -> Result<SignedDownload, HubModeError>,
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
        self.require_key()?;
        if !valid_pin_sha(&entry.pin_sha) || !valid_sha256_hex(&entry.pack_sha256) {
            return Err(HubModeError::IntegrityMismatch {
                slug: entry.slug.clone(),
                reason: "the catalogue entry has no valid pin_sha / pack_sha256".into(),
            });
        }
        if entry.pack_bytes > self.max_bytes {
            return Err(HubModeError::EntryExceedsCacheBudget {
                slug: entry.slug.clone(),
                entry_bytes: entry.pack_bytes,
                max_bytes: self.max_bytes,
            });
        }

        let (stale, repaired) = match self.read_cached_pack(entry)? {
            Cached::Valid {
                value,
                fetched_at_unix,
            } => {
                let freshness = Freshness::evaluate(fetched_at_unix, now_unix, self.ttl_secs);
                if !freshness.is_stale() {
                    self.touch(&entry.slug, &entry.pack_sha256)?;
                    self.disk_hits += 1;
                    return Ok(ResolvedCachedArtifact {
                        value,
                        outcome: CacheLookupOutcome::HitFromDisk,
                        freshness,
                        evicted_slugs: Vec::new(),
                        refresh_error: None,
                        repaired: None,
                    });
                }
                (Some((value, freshness)), None)
            }
            Cached::Damaged(why) => (None, Some(why)),
            Cached::Missing => (None, None),
        };

        match self.refresh_pack(entry, now_unix, fetch_pack)? {
            Refresh::Done((value, evicted_slugs)) => Ok(ResolvedCachedArtifact {
                value,
                outcome: CacheLookupOutcome::FetchedAndCached,
                freshness: Freshness::evaluate(now_unix, now_unix, self.ttl_secs),
                evicted_slugs,
                refresh_error: None,
                repaired,
            }),
            Refresh::FetchFailed(error) => match stale {
                Some((value, freshness)) => {
                    self.touch(&entry.slug, &entry.pack_sha256)?;
                    self.disk_hits += 1;
                    Ok(ResolvedCachedArtifact {
                        value,
                        outcome: CacheLookupOutcome::HitFromDisk,
                        freshness,
                        evicted_slugs: Vec::new(),
                        refresh_error: Some(error.to_string()),
                        repaired: None,
                    })
                }
                None => Err(error),
            },
        }
    }
}

/// What to look up in a verified pack, and how much to return.
#[derive(Debug, Clone, Copy)]
pub struct AnswerQuery<'a> {
    pub query: &'a str,
    pub mode: AnswerMode,
    /// At most this many hits are returned (at least one). When more match,
    /// [`PackAnswer::truncated`] is set.
    pub max_hits: usize,
}

impl<'a> AnswerQuery<'a> {
    /// A query capped at [`DEFAULT_MAX_HITS`].
    pub fn new(query: &'a str, mode: AnswerMode) -> Self {
        Self {
            query,
            mode,
            max_hits: DEFAULT_MAX_HITS,
        }
    }

    /// The same query with a different hit cap.
    pub fn with_max_hits(mut self, max_hits: usize) -> Self {
        self.max_hits = max_hits;
        self
    }
}

/// The hits [`answer_from_pack`] selected.
#[derive(Debug, Clone)]
pub struct PackAnswer {
    pub hits: Vec<Hit>,
    /// More than `max_hits` matched; `hits` holds the best `max_hits`.
    pub truncated: bool,
    /// How many matched before the cap.
    pub total_matches: usize,
}

/// Answer from a verified pack with no node.
///
/// - A definition lookup ([`AnswerMode::Def`]) returns definitions whose symbol
///   equals the query, exact matches first, then those that contain it,
///   case-insensitively. An empty query is [`HubModeError::EmptyQuery`]: it
///   would match every definition.
/// - A file list ([`AnswerMode::Files`]) carries the per-file licence; a
///   non-empty query keeps the paths that contain it.
/// - A passage question ([`AnswerMode::How`]) is refused with
///   [`HubModeError::PassageSearchNeedsNode`].
///
/// Output is capped at `max_hits`, with `truncated` set when it was.
pub fn answer_from_pack(
    pack: &CachedCorpusPack,
    entry: &CatalogueEntry,
    query: &AnswerQuery<'_>,
) -> Result<PackAnswer, HubModeError> {
    let licence_warning = match entry.licence_class {
        LicenceClass::AdaptWithAttribution => None,
        LicenceClass::ApproachOnly => Some(approach_only_warning(&entry.licence_spdx)),
        LicenceClass::Excluded => {
            return Err(HubModeError::ExcludedCorpusLicence {
                slug: entry.slug.clone(),
                licence_spdx: entry.licence_spdx.clone(),
            })
        }
    };
    let needle = query.query.trim().to_ascii_lowercase();
    let mut hits: Vec<Hit> = match query.mode {
        AnswerMode::Def => {
            if needle.is_empty() {
                return Err(HubModeError::EmptyQuery { mode: "definition" });
            }
            pack.defs
                .iter()
                .filter_map(|d| {
                    let symbol = d.symbol.to_ascii_lowercase();
                    let score = if symbol == needle {
                        2.0
                    } else if symbol.contains(&needle) {
                        1.0
                    } else {
                        return None;
                    };
                    Some(Hit {
                        file: d.file.clone(),
                        line: d.line,
                        kind: d.kind.clone(),
                        text: d.text.clone(),
                        value: d.value.clone(),
                        score,
                        licence_warning: licence_warning.clone(),
                    })
                })
                .collect()
        }
        AnswerMode::Files => pack
            .files
            .iter()
            .filter(|f| needle.is_empty() || f.path.to_ascii_lowercase().contains(&needle))
            .map(|f| Hit {
                file: f.path.clone(),
                line: 1,
                kind: "file".to_string(),
                text: format!("licence: {}", f.licence),
                value: None,
                score: 1.0,
                licence_warning: licence_warning.clone(),
            })
            .collect(),
        AnswerMode::How => {
            return Err(HubModeError::PassageSearchNeedsNode {
                slug: entry.slug.clone(),
            })
        }
    };
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });
    let total_matches = hits.len();
    let max_hits = query.max_hits.max(1);
    let truncated = total_matches > max_hits;
    hits.truncate(max_hits);
    Ok(PackAnswer {
        hits,
        truncated,
        total_matches,
    })
}

/// Whole days from a pin date (`YYYY-MM-DD`) to `now_unix`: 0 for a pin dated in
/// the future or one that does not parse, as `xerj ask` reports it.
pub fn pin_age_days(pin_date: &str, now_unix: u64) -> u32 {
    let Ok(pin) = chrono::NaiveDate::parse_from_str(pin_date.trim(), "%Y-%m-%d") else {
        return 0;
    };
    let Some(now) = i64::try_from(now_unix)
        .ok()
        .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
    else {
        return 0;
    };
    u32::try_from((now.date_naive() - pin).num_days().max(0)).unwrap_or(u32::MAX)
}

/// Parameters for a hub corpus query executed by [`HubFirstRouter::query_corpus`].
#[derive(Debug, Clone, Copy)]
pub struct HubQueryRequest<'a> {
    pub slug: &'a str,
    pub query: &'a str,
    pub mode: AnswerMode,
    /// Seconds since the Unix epoch. Drives staleness and the answer's
    /// `age_days`, which is the days from the entry's `pin_date` to this time.
    pub now_unix: u64,
    /// Cap on the hits returned ([`DEFAULT_MAX_HITS`]).
    pub max_hits: usize,
}

impl<'a> HubQueryRequest<'a> {
    /// A request capped at [`DEFAULT_MAX_HITS`].
    pub fn new(slug: &'a str, query: &'a str, mode: AnswerMode, now_unix: u64) -> Self {
        Self {
            slug,
            query,
            mode,
            now_unix,
            max_hits: DEFAULT_MAX_HITS,
        }
    }

    /// The same request with a different hit cap.
    pub fn with_max_hits(mut self, max_hits: usize) -> Self {
        self.max_hits = max_hits;
        self
    }
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
    /// When no local node is running, the hub tools are listed as available
    /// while private-index node tools carry a notice that a local node is
    /// required. The `xerj_hub_*` tools are unavailable in a build with no hub
    /// key, as in `xerj mcp`.
    ///
    /// `xerj mcp` does not call this: it serves one `tools/list` in every mode,
    /// and a node tool called with no node starts the node or answers with its
    /// own "no node is listening" error.
    pub fn list_tools(&self) -> Vec<ToolAvailability> {
        let mut out = Vec::with_capacity(HUB_DIRECT_TOOLS.len() + LOCAL_NODE_TOOLS.len());
        for &tool in HUB_DIRECT_TOOLS {
            let needs_key = tool.starts_with("xerj_hub_") && !self.cache.can_verify();
            out.push(ToolAvailability {
                name: tool.to_string(),
                available: !needs_key,
                routed_via: "hub_client_cache",
                note: if needs_key {
                    Some(NO_HUB_KEY_MESSAGE.to_string())
                } else {
                    self.mode.fallback_notice().map(str::to_string)
                },
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
    /// - `xerj_hub_*` tools route to [`RouteTarget::HubClient`] only when this
    ///   build can verify hub data; with no hub key they return
    ///   [`HubModeError::NoHubKey`], as `xerj mcp` does not serve them then.
    /// - `xerj_corpus_feedback` and `xerj_field_report` route to the hub client.
    /// - Private-index node tools (`xerj_search`, `xerj_def`, `xerj_map`, …)
    ///   route to [`RouteTarget::LocalNode`] when a node is reachable, and
    ///   return [`HubModeError::LocalNodeRequired`] when in hub-only fallback.
    ///
    /// `xerj mcp` does not call this either; it decides per tool in
    /// `xerj-mcp`'s `src/hub/first.rs`.
    pub fn route_tool(&self, tool_name: &str) -> Result<RouteTarget, HubModeError> {
        if HUB_DIRECT_TOOLS.contains(&tool_name) {
            if tool_name.starts_with("xerj_hub_") && !self.cache.can_verify() {
                return Err(HubModeError::NoHubKey);
            }
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

    /// Answer a hub definition or file-list question end-to-end through the
    /// verified disk cache (fetching on first access, and when an entry is stale
    /// or damaged; serving later calls from disk).
    ///
    /// - Invalid corpus slugs, an empty definition query and a passage question
    ///   fail before any I/O: [`HubModeError::InvalidCorpusSlug`],
    ///   [`HubModeError::EmptyQuery`], [`HubModeError::PassageSearchNeedsNode`].
    /// - Unknown corpus slugs fail loudly with [`HubModeError::UnknownCorpus`].
    /// - Unsupported catalogue or pack `format_version` fails loudly with
    ///   [`HubModeError::UnsupportedFormatVersion`].
    /// - A stale entry served because its refresh failed, a repaired entry and a
    ///   truncated result each add a line to `Answer.warnings`.
    pub fn query_corpus<FCat, FPack>(
        &mut self,
        request: HubQueryRequest<'_>,
        fetch_catalogue: FCat,
        fetch_pack: FPack,
    ) -> Result<HubQueryResult, HubModeError>
    where
        FCat: FnOnce() -> Result<SignedDownload, HubModeError>,
        FPack: FnOnce(&CatalogueEntry) -> Result<SignedDownload, HubModeError>,
    {
        let HubQueryRequest {
            slug,
            query,
            mode,
            now_unix,
            max_hits,
        } = request;
        if !valid_corpus_slug(slug) {
            return Err(HubModeError::InvalidCorpusSlug {
                slug: slug.to_string(),
            });
        }
        if mode == AnswerMode::How {
            return Err(HubModeError::PassageSearchNeedsNode {
                slug: slug.to_string(),
            });
        }
        if mode == AnswerMode::Def && query.trim().is_empty() {
            return Err(HubModeError::EmptyQuery { mode: "definition" });
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
        let found = answer_from_pack(
            &pack_resolved.value,
            entry,
            &AnswerQuery::new(query, mode).with_max_hits(max_hits),
        )?;

        let mut warnings = Vec::new();
        if let Some(notice) = self.mode.fallback_notice() {
            warnings.push(notice.to_string());
        }
        warnings.extend(cat_resolved.warnings("catalogue"));
        warnings.extend(pack_resolved.warnings(&format!("corpus '{}'", entry.slug)));
        if entry.licence_class == LicenceClass::ApproachOnly {
            warnings.push(approach_only_warning(&entry.licence_spdx));
        }
        if found.truncated {
            warnings.push(format!(
                "output truncated to the best {} of {} matches; narrow the query",
                found.hits.len(),
                found.total_matches
            ));
        }

        let answer = Answer {
            corpus: entry.slug.clone(),
            pin_sha: entry.pin_sha.clone(),
            pin_date: entry.pin_date.clone(),
            age_days: pin_age_days(&entry.pin_date, now_unix),
            licence_spdx: entry.licence_spdx.clone(),
            licence_class: entry.licence_class,
            answered_from: match pack_resolved.outcome {
                CacheLookupOutcome::HitFromDisk => AnsweredFrom::LocalPack,
                CacheLookupOutcome::FetchedAndCached => AnsweredFrom::Hosted,
            },
            mode,
            hits: found.hits,
            truncated: found.truncated,
            warnings,
        };
        let rendered = render(&answer);

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
mod tests;
