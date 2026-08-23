//! Engine: manages multiple named indices.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;
use tracing::{error, info, warn};
use xerj_common::config::Config;
use xerj_common::types::{IndexName, Schema};

use crate::cluster_state::{
    ClusterStateBootStatus, PersistedClusterStateV1, PersistedDataStreamV1,
    PersistedIndexTemplateV1, PreparedClusterState, CLUSTER_STATE_V1,
};
use crate::index::{Index, IndexStats};
use crate::{EngineError, Result};

/// Privacy-safe identity of the embedding execution contract exposed to
/// remote ingestion clients. The digest is opaque: model paths, provider URLs,
/// credentials, and model names never cross the API boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingExecutionIdentity {
    pub version: u32,
    pub backend: String,
    pub identity_sha256: String,
    /// Vector width, but only for the backends where the server actually
    /// pins it: `lexical` always emits [`xerj_ai::local::DEFAULT_DIMS`], and
    /// `onnx-experimental` is constrained to 384 by `validate_onnx_dimensions`.
    /// `neural` takes its width from the loaded model's `hidden_size` and
    /// `proxy` from whatever the remote returns, so neither is known here and
    /// the field is omitted rather than guessed — a reader must not treat an
    /// absent width as 384.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<usize>,
    pub semantic_contract: String,
    pub resumable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub non_resumable_reason: Option<String>,
}

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

/// An index directory that is present on disk but could not be opened.
///
/// Before issue #206 a failed open left only `name → reason` in a private map
/// that nothing but [`Engine::health`] ever read: the index was invisible to
/// `_cat/indices` and `_cluster/state`, `DELETE` answered 404, and the only
/// recovery was to stop the server and edit the data directory by hand. A
/// failed index is now a real, addressable state — it is listed, it carries
/// the open error verbatim, it can be deleted, and it can be retried once the
/// operator has fixed the cause.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedIndex {
    /// Index name, i.e. the directory name under `data_dir`.
    pub name: String,
    /// The verbatim error from the last open attempt. Never summarised —
    /// the storage layer's message already names the file and the fix.
    pub reason: String,
    /// Wall-clock ms of the first failure (boot, restore, or a failed retry).
    pub failed_at_ms: i64,
    /// How many explicit retries have been attempted since (0 at first boot).
    pub retries: u32,
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
///
/// Each context pins a fully-hydrated `Vec<Hit>` snapshot, so its lifetime
/// must be bounded: pre-rc.4 contexts lived until an explicit
/// `DELETE /_search/scroll` and accumulated forever otherwise (RC4
/// blocker 11). Every context now carries an `expires_at` deadline
/// (from the request's `scroll=…` keep-alive, capped by
/// `Config.search_context.scroll_max_keep_alive_secs`), refreshed on each
/// continuation, enforced on access, and swept in the background.
pub struct ScrollContext {
    pub index: String,
    /// Each hit paired with the name of the index it actually came from.
    /// Pairing is structural on purpose (#414): a multi-index scroll used to
    /// store bare hits plus one context-level `index`, so every continuation
    /// page stamped the same name onto hits from different indices and
    /// `(_index, _id)` stopped being distinct.
    pub hits: Vec<(String, xerj_query::executor::Hit)>,
    pub position: usize,
    pub page_size: usize,
    /// Whether to emit `_seq_no`/`_primary_term` on every page. Real ES
    /// fixes this at the scroll-opening request and does not re-read it
    /// from continuation bodies — the flag governs the whole scroll's
    /// lifetime, not each page — so it's captured once here, not re-parsed
    /// per continuation.
    pub seq_no_primary_term: bool,
    /// Whether to emit `_version` on every page. Same once-at-open semantics
    /// as `seq_no_primary_term`.
    pub version: bool,
    /// Whether the opening request suppressed `_source` at the REQUEST level —
    /// `"_source": false`, or `stored_fields` implying suppression when
    /// `_source` was unspecified. These are constant across the whole snapshot
    /// (index-independent), so a single captured boolean is correct. Captured
    /// once at open — like `seq_no_primary_term`/`version`, ES fixes the
    /// `_source` selection at the scroll-opening request — so the continuation
    /// render omits `_source` when this is set, matching the first page
    /// (#624/#637).
    pub source_disabled: bool,
    /// Whether the opening route's first page also applies mapping-level
    /// `_source.enabled: false` suppression (which is PER-HIT: a scroll can span
    /// indices with divergent `_source` settings). `search_impl` (`_search?scroll=`)
    /// does, so the continuation re-derives it per hit from each hit's own index;
    /// `search_with_scroll` (`_search_scroll`) does NOT suppress it on its first
    /// page, so its continuation must not either, or it would introduce a new
    /// first-page/continuation split on that route (#637; that route's own gap is
    /// tracked in #659).
    pub mapping_source_check: bool,
    pub created: Instant,
    /// The keep-alive window last requested for this context. A
    /// continuation without an explicit `scroll` parameter re-arms the
    /// deadline with this same duration (ES keeps the context alive for
    /// the previous keep-alive in that case).
    pub keep_alive: std::time::Duration,
    /// Wall-clock deadline after which the context is dead: continuations
    /// return the same 404 as an unknown id, and the background sweeper
    /// (or an opportunistic sweep on open) frees the pinned hits.
    pub expires_at: Instant,
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
/// the auth middleware.
///
/// Persisted across restarts (item 6): every mutation rewrites
/// `<data_dir>/api_keys.json` (0600, atomic) and the file is reloaded on boot,
/// so a key minted by `POST /_security/api_key` keeps working after the node
/// restarts — before this, the map was in-memory only and every restart
/// silently invalidated all minted keys (Kibana/agents would 401 until re-set).
/// Serialized as JSON, so the fields must stay `serde`-round-trippable.
///
/// # The secret is a hash (issue #201)
///
/// It used to be the credential itself, in the clear, in a file. 0600 and an
/// atomic rename are the right handling for a secret *while the process owns
/// it*, but a file has more readers than a process has: a backup, a volume
/// snapshot, a container layer, a support bundle, a decommissioned disk. Any
/// one of those handed over every live credential on the node.
///
/// Now only [`crate::secret_hash`]'s salted-SHA-256 digest is stored, and the
/// secret fields are **private**: outside this module the struct cannot be
/// built with a struct literal at all, so the only way to make a record is
/// [`ApiKeyRecord::new`], which hashes. That is deliberate — it makes storing
/// plaintext again a compile error rather than a code-review catch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyRecord {
    /// Caller-supplied key name (informational).
    pub name: String,
    /// Salted hash of the secret half of the credential (the `api_key` value
    /// returned to the caller, i.e. the part after `id:` in the decoded
    /// `ApiKey` header). Never the secret itself.
    ///
    /// `#[serde(default)]` so a pre-#201 `api_keys.json` — which has `secret`
    /// and no `secret_hash` — still deserializes; [`Engine::load_persisted_api_keys`]
    /// then migrates it. Anything here that
    /// [`crate::secret_hash::is_usable_hash`] rejects — empty (never migrated), or
    /// present but unparseable (truncated write, hand edit, a scheme this
    /// build does not know) — means "no usable credential": the load path
    /// drops such a record and [`ApiKeyRecord::verify_secret`] denies it if
    /// one ever reaches the auth path anyway.
    #[serde(default)]
    secret_hash: String,
    /// Pre-#201 plaintext secret, read only so it can be migrated away.
    ///
    /// Deserialized from the old `secret` field, never written back: the load
    /// path hashes it into `secret_hash`, clears this, and rewrites the file,
    /// after which the plaintext exists nowhere. `skip_serializing_if` means
    /// even a partially-migrated in-memory record never re-emits plaintext.
    #[serde(default, rename = "secret", skip_serializing_if = "Option::is_none")]
    legacy_plaintext_secret: Option<String>,
    /// Creation time in epoch milliseconds.
    pub creation_ms: u64,
    /// Absolute expiration in epoch milliseconds, or `None` if the key never
    /// expires.
    pub expiration_ms: Option<u64>,
    /// Set once the key has been invalidated (revoked).
    pub invalidated: bool,
    /// When the key was invalidated, in epoch milliseconds — `None` while the
    /// key is live. Reported as `invalidation` by `GET /_security/api_key`
    /// (ES stamps the analogous `invalidation_time` on its key doc in the
    /// same update that flips `api_key_invalidated`). `#[serde(default)]` so
    /// an `api_keys.json` written before this field existed still loads.
    #[serde(default)]
    pub invalidation_ms: Option<u64>,
    /// Index-scoped grants parsed from the `role_descriptors` supplied at
    /// creation (`crate::rbac::roles_from_role_descriptors`).
    ///
    /// **Empty means "no grant", not "all grants."** A key with no roles is an
    /// *unscoped* legacy credential: it keeps the historical superuser reach
    /// over the general ES-compat surface, but it holds no privilege at all on
    /// the reserved `.xerj-memory-*` namespace (agent-memory namespaces and
    /// second brains), which is the fail-closed half of the per-brain
    /// authorization model. A key with a non-empty list is *scoped* and may
    /// touch only what these roles allow.
    ///
    /// `#[serde(default)]` so an `api_keys.json` written before this field
    /// existed still loads — those keys deserialize as unscoped, which is the
    /// safe reading of "was minted when nothing was enforced".
    #[serde(default)]
    pub roles: Vec<crate::rbac::Role>,
}

impl ApiKeyRecord {
    /// Build a live key record from the plaintext secret handed to the caller.
    ///
    /// The plaintext is hashed here and dropped on return — this function is
    /// the only way to construct a record, so there is no path by which a
    /// secret reaches [`Engine::flush_api_keys`] and therefore the disk.
    pub fn new(
        name: impl Into<String>,
        secret: &str,
        creation_ms: u64,
        expiration_ms: Option<u64>,
        roles: Vec<crate::rbac::Role>,
    ) -> Self {
        Self {
            name: name.into(),
            secret_hash: crate::secret_hash::hash_secret(secret),
            legacy_plaintext_secret: None,
            creation_ms,
            expiration_ms,
            invalidated: false,
            invalidation_ms: None,
            roles,
        }
    }

    /// Does `presented` match this record's secret?
    ///
    /// Fail-closed on a record that carries no usable hash — never migrated
    /// (empty), or present but not one of our encodings.
    /// [`Engine::load_persisted_api_keys`] drops those before they can reach
    /// here, but if one ever did, "no usable stored hash" must mean "no",
    /// never "yes". [`crate::secret_hash::is_usable_hash`] is the discriminator,
    /// the same one the load path uses, so the two cannot disagree about what
    /// counts as a credential.
    pub fn verify_secret(&self, presented: &str) -> bool {
        if !crate::secret_hash::is_usable_hash(&self.secret_hash) {
            return false;
        }
        crate::secret_hash::verify_secret(presented, &self.secret_hash)
    }

    /// Migrate a record loaded from a pre-#201 `api_keys.json`.
    ///
    /// Returns `true` when the record changed and the store must be rewritten.
    /// `Err(())` means the record carries no usable credential in either form
    /// and must be dropped rather than kept as a key that silently never
    /// authenticates.
    ///
    /// "Usable credential" is [`crate::secret_hash::is_usable_hash`], which is
    /// a full decode — **not** `!secret_hash.is_empty()`, and not a check of
    /// the `$ssha256$` tag either. A `secret_hash` that carries the tag but
    /// does not decode (`"$ssha256$truncated"` from a hand edit, a scheme a
    /// future build writes and this one cannot read) is denied by every
    /// verifier, so a record holding only that can never authenticate.
    /// Restoring it would leave a key `GET /_security/api_key` lists as live
    /// while nothing can ever use it — the accept-then-ignore shape issue #204
    /// tracks, and exactly what dropping the empty case already avoids. Only
    /// the two shapes that really are credentials survive:
    ///
    /// * a `secret_hash` that decodes — the post-#201 shape, and the winner
    ///   whenever both forms are present;
    /// * a non-empty `secret` and **no** `secret_hash` at all — the exact
    ///   pre-#201 shape, which is what migration is for.
    ///
    /// Anything else is dropped rather than repaired. A record carrying both a
    /// plaintext and a hash-shaped value that does not decode is a store
    /// nobody can explain; deriving a live credential from the half of it that
    /// #201 exists to delete is the fail-open reading of an ambiguous record,
    /// and this is a credential path.
    ///
    /// The drop is in memory. `load_persisted_api_keys` only rewrites the file
    /// when something migrated, so a dropped record normally stays on disk for
    /// the operator to inspect after the error log points at it.
    fn migrate_from_plaintext(&mut self) -> std::result::Result<bool, ()> {
        let plaintext = self.legacy_plaintext_secret.take();
        if crate::secret_hash::is_usable_hash(&self.secret_hash) {
            // The hash is the credential. A leftover plaintext beside it is
            // discarded — dropping it is the whole point, and re-deriving from
            // it could silently swap which secret the record authenticates.
            // `Some` means the file still held plaintext, so it must be
            // rewritten.
            return Ok(plaintext.is_some());
        }
        match plaintext {
            Some(plain) if !plain.is_empty() && self.secret_hash.is_empty() => {
                self.secret_hash = crate::secret_hash::hash_secret(&plain);
                Ok(true)
            }
            _ => Err(()),
        }
    }
}

/// Atomically write a **secret** file (API-key store) with owner-only (0600)
/// permissions. The temp file is created 0600 *before* any bytes are written so
/// the secret is never briefly world-readable, then renamed over the target
/// (the rename carries the 0600 inode). On non-unix, perms are left to the OS
/// default. Mirrors `index::write_file_atomic` but hardens the mode.
pub(crate) fn write_secret_file_atomic(
    path: &std::path::Path,
    bytes: &[u8],
) -> std::io::Result<()> {
    use std::io::Write as _;
    // Unique staging name, for the same reason as `index::write_file_atomic`:
    // two concurrent key mints sharing one `api_keys.tmp` can interleave into a
    // key store that no longer parses, and a corrupt key store silently drops
    // every persisted key at the next boot.
    let tmp = crate::index::staging_path(path);
    let staged = (|| -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        // Belt and braces: pin 0600 even if the platform ignored the open mode.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, path)
    })();
    // Never leave a staging file behind on failure — it holds key material.
    if let Err(e) = staged {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
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
    /// template_name → IndexTemplate. Persisted in
    /// `<data_dir>/cluster_state.json`; mutate only through
    /// [`Engine::put_index_template`] / [`Engine::delete_index_template`],
    /// or the change will not survive a restart (issue #203).
    pub templates: Arc<DashMap<String, IndexTemplate>>,
    /// scroll_id → ScrollContext
    pub scrolls: Arc<DashMap<String, ScrollContext>>,
    /// pipeline_id → pipeline definition JSON. Persisted in
    /// `<data_dir>/cluster_state.json`; mutate only through
    /// [`Engine::put_pipeline`] / [`Engine::delete_pipeline`], or the change
    /// will not survive a restart (issue #203).
    pub pipelines: Arc<DashMap<String, Value>>,
    /// pipeline_id → why the stored definition has no compiled counterpart.
    ///
    /// Issue #204. A definition that carries a stage type this build does not
    /// implement is still stored (Elasticsearch accepts it, and
    /// `_ingest/pipeline/{id}/_simulate` still walks it), but nothing may be
    /// ingested through it: `process_through_pipeline` refuses with the reason
    /// recorded here instead of quietly indexing the untransformed document.
    pub unrunnable_pipelines: Arc<DashMap<String, String>>,
    /// pipeline_id → the reason a **gate-bearing build refused this exact
    /// definition at PUT time**, persisted so a restart cannot overturn the
    /// answer the operator already received.
    ///
    /// Issue #204. This is PROVENANCE, and it plays the same role for a
    /// pipeline that `analysis-binding.json` plays for an index: its ABSENCE is
    /// the load-bearing signal. A definition in `cluster_state.json` with no
    /// entry here was written by a build that had no gate, so the boot replay
    /// may soften it ([`CompileMode::ReplayPersisted`](xerj_wasm::pipeline::CompileMode)).
    /// A definition WITH an entry here was already answered "no" by this
    /// contract, and the replay must not resurrect it — without this marker,
    /// `ReplayPersisted` cannot tell the two apart, and a plain restart turned a
    /// PUT-time refusal back into a `201` that wrote the wrong document
    /// (`GET` hands the operator the stored shape, they edit it, PUT it back,
    /// get the refusal, restart, and the refusal is gone).
    ///
    /// Persisted in `<data_dir>/ingest-pipeline-refusals.json`, deliberately
    /// NOT in `cluster_state.json`: that envelope is a closed format-1 contract
    /// with `deny_unknown_fields` (see [`crate::cluster_state`]), so adding a
    /// field to it would make every file this build writes unreadable to an
    /// older binary. A sidecar an older build simply ignores does not.
    ///
    /// Reference: Lucene records the same kind of fact for the same reason.
    /// `SegmentInfos.indexCreatedVersionMajor`
    /// (`lucene/core/src/java/org/apache/lucene/index/SegmentInfos.java:172`,
    /// "The Lucene version major that was used to create the index") is written
    /// once into the commit
    /// (`SegmentInfos.java:633`, `out.writeVInt(indexCreatedVersionMajor)`) and
    /// then *decides how the bytes are read back* rather than being re-derived:
    /// `readCommit` throws `CorruptIndexException` when a segment predates the
    /// recorded creation version (`SegmentInfos.java:491-501`), and
    /// `readLatestCommit(dir, minSupportedMajor)` refuses a commit whose
    /// recorded creation major is below the caller's floor
    /// (`SegmentInfos.java:572-578`). The lesson taken here is the shape, not
    /// the code: what a *later* reader may assume about persisted bytes has to
    /// be recorded WITH them at write time, because it cannot be recovered
    /// afterwards. Lucene is Apache-2.0, the same licence as XERJ; no code is
    /// reproduced.
    pub definition_time_refusals: Arc<DashMap<String, String>>,
    /// Set when `ingest-pipeline-refusals.json` exists but could not be read or
    /// parsed, so no pipeline's provenance is known.
    ///
    /// Issue #204. "Unknown provenance" is not "no refusal": replaying softly
    /// there would resurrect exactly the refusals the marker exists to keep.
    /// Every persisted pipeline is replayed at DEFINITION strictness instead —
    /// fail closed, and loudly, with the repair named in the reason.
    pub pipeline_refusals_unreadable: Arc<std::sync::atomic::AtomicBool>,
    /// pipeline_id → configuration the running build cannot honour but that
    /// was already persisted by an older one.
    ///
    /// Issue #204. The pipeline RUNS — exactly as the build that stored it ran
    /// it — because failing it at boot would turn a working cluster's writes
    /// from 201 into 400 on a binary upgrade alone. This map is what stops that
    /// compatibility door from being silent: it is populated only by the boot
    /// replay ([`Engine::compile_pipeline_with_mode`] with
    /// [`CompileMode::ReplayPersisted`](xerj_wasm::pipeline::CompileMode)), and
    /// a re-`PUT` of the same definition answers 400 with the same reason.
    pub degraded_pipelines: Arc<DashMap<String, String>>,
    /// index_name → open/closed state (true = closed)
    pub closed_indices: Arc<DashMap<String, bool>>,
    /// data stream name → DataStream. Persisted in
    /// `<data_dir>/cluster_state.json`; mutate only through the
    /// `*_data_stream` methods, or the change will not survive a restart
    /// (issue #203).
    pub data_streams: Arc<DashMap<String, DataStream>>,
    /// ILM policy name → raw policy JSON, kept verbatim for `GET _ilm/policy`
    /// round-trip fidelity. The REAL execution engine reads `ism_policies`
    /// instead — `PUT _ilm/policy/{name}` writes to BOTH maps (see
    /// `xerj_api::es_compat::put_ilm_policy`), translating into the shared
    /// internal model. See `crate::lifecycle` for why one engine drives both
    /// surfaces.
    ///
    /// This map — the verbatim ILM document — is persisted in
    /// `<data_dir>/cluster_state.json`; mutate only through
    /// [`Engine::put_ilm_policy`] / [`Engine::delete_ilm_policy`], or the
    /// change will not survive a restart (issue #203). The translated
    /// `ism_policies` model has its own file (`ism_policies.json`, issue
    /// #199) — the two never write each other's file.
    pub ilm_policies: Arc<DashMap<String, Value>>,
    /// policy id → the internal ISM-shaped model every managed index is
    /// actually driven by, regardless of whether the policy was created via
    /// `_plugins/_ism/policies` (native) or `_ilm/policy` (translated).
    /// Persisted separately in `<data_dir>/ism_policies.json` — see
    /// [`Engine::put_ism_policy`].
    pub ism_policies: Arc<DashMap<String, crate::lifecycle::LifecyclePolicy>>,
    /// index name → lifecycle execution cursor (current state, pending
    /// action, timestamps). Presence in this map is what "managed" means —
    /// an index with no entry here is not touched by the background job.
    pub managed_indices: Arc<DashMap<String, crate::lifecycle::ManagedIndexState>>,
    /// Persisted detach tombstones (issue #282, ported from #262): an index
    /// name lands here when an operator explicitly detached it — `PUT
    /// /{index}/_settings {"index.lifecycle.name": null}`, `POST
    /// /{index}/_ilm/remove`, or `POST /_plugins/_ism/remove/{index}`. An
    /// acknowledged detach must be authoritative across restarts: mere
    /// *absence* from `managed_indices` cannot distinguish "never managed"
    /// from "operator said stop", and any future code path that re-derives
    /// attachments (e.g. from persisted index settings, which may still
    /// carry a stale `index.lifecycle.name`) MUST consult this set first.
    /// Cleared by an explicit re-attach or by deleting the index. Persisted
    /// in `ism_managed_indices.json` alongside the cursors.
    pub lifecycle_detached: Arc<DashMap<String, ()>>,
    /// Operator kill switch for lifecycle execution (`POST /_ilm/stop` /
    /// `/_ilm/start`): while false, `lifecycle::tick` returns without
    /// acting. In-memory only, matching #262's rc — a restart resumes
    /// execution, which errs on the side of retention running.
    pub lifecycle_running: Arc<std::sync::atomic::AtomicBool>,
    /// index name → creation time (epoch ms), for `min_index_age` and for
    /// `GET /{index}`'s `creation_date` (previously synthesized as
    /// `Utc::now()` on every request — see `record_index_created_at`).
    pub index_created_at: Arc<DashMap<String, i64>>,
    /// component template name → template JSON. Persisted in
    /// `<data_dir>/cluster_state.json`; mutate only through
    /// [`Engine::put_component_template`] /
    /// [`Engine::delete_component_template`], or the change will not survive
    /// a restart (issue #203).
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
    /// Index directories that are present but could not be opened, keyed by
    /// index name (health = red). See [`FailedIndex`] — these are inspectable
    /// (`GET /_cluster/indices/failed`), deletable (`DELETE /{index}`) and
    /// retryable (`POST /_cluster/indices/failed/{index}/_retry`).
    pub failed_indices: Arc<DashMap<String, FailedIndex>>,
    /// transform id → transform definition JSON
    pub transforms: Arc<DashMap<String, Value>>,
    /// index_name → frozen state (true = frozen / read-only)
    pub frozen_indices: Arc<DashMap<String, bool>>,
    /// rollup job id → job definition JSON
    pub rollup_jobs: Arc<DashMap<String, Value>>,
    /// CCR auto-follow pattern name → pattern JSON
    pub ccr_auto_follow: Arc<DashMap<String, Value>>,
    /// Single-node WAL tap (issue #320): live config, durable cursors and
    /// counters for the one-directional push of allowlisted indices to an
    /// external ES-compatible target. Always present; inert until
    /// `wal_tap.enabled` and a target URL are set. Not CCR — see
    /// [`crate::wal_tap`] for why the two are different features.
    pub wal_tap: Arc<crate::wal_tap::WalTap>,
    /// API key id → record. Populated by `POST /_security/api_key` so the
    /// auth middleware can re-authenticate `Authorization: ApiKey <encoded>`.
    /// In-memory only (lost on restart).
    pub api_keys: Arc<DashMap<String, ApiKeyRecord>>,
    /// `"{application}\0{name}"` → the ES-shaped privilege object
    /// (`application`/`name`/`actions`/`metadata`). Populated by
    /// `PUT /_security/privilege`, read by `GET /_security/privilege*`.
    /// Not enforced (same honest-surface convention as `roles`/`api_keys`'
    /// role_descriptors) — Kibana registers its `kibana-.kibana` privileges
    /// here at startup so `GET` stops reporting an empty set on every
    /// subsequent poll, but nothing actually gates on them yet.
    /// In-memory only (lost on restart).
    pub application_privileges: Arc<DashMap<String, Value>>,
    /// legacy index template name (v1 /_template) → template JSON.
    /// Persisted in `<data_dir>/cluster_state.json`; mutate only through
    /// [`Engine::put_legacy_template`] / [`Engine::delete_legacy_template`],
    /// or the change will not survive a restart (issue #203).
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

    /// Serializes rewrites of `<data_dir>/cluster_state.json` (issue #203).
    ///
    /// The atomic rewrite stages through a single fixed path
    /// (`cluster_state.tmp`), so two concurrent flushes — two provisioning
    /// PUTs arriving together is all it takes — would create/truncate the
    /// *same* temp file and rename each other's half-written bytes over the
    /// real one. Holding this across snapshot-and-write makes the whole
    /// rewrite one critical section; the last writer's snapshot necessarily
    /// includes every mutation that had already been applied to the maps, so
    /// serializing loses nothing.
    ///
    /// Never taken while holding a `DashMap` guard.
    cluster_state_write: Arc<parking_lot::Mutex<()>>,

    /// Immutable classification of the one `cluster_state.json` document read
    /// after `node.lock` and before any index/WAL open. A blocked status keeps
    /// readiness false and refuses every metadata mutation for this process
    /// lifetime; recovery requires replacing the bytes and restarting.
    ///
    /// Without this the failure is silent *and* destructive. The maps boot
    /// empty, so the first `PUT /_index_template/...` snapshots six empty
    /// maps and `write_file_atomic` renames `cluster_state.tmp` over the
    /// target — and `rename(2)` needs only write permission on the
    /// *directory*, so an unreadable-but-perfectly-intact document is
    /// unlinked by a write that answers `{"acknowledged": true}`. Every
    /// trigger listed above is transient or trivially repairable *until*
    /// xerj overwrites the file, which is the one outcome that is not.
    ///
    /// Refusing is also the honest answer rather than merely the safe one:
    /// the node is running without the templates that shape new indices and
    /// the pipelines `?pipeline=x` names, so accepting more configuration on
    /// top of a config that is silently a subset is the same
    /// accepted-and-ignored shape issue #204 tracks.
    ///
    /// Cleared only by a boot that loads cleanly — fix or move the file
    /// aside and restart. redb takes the same position after a failed
    /// integrity check or a failed I/O: an `AtomicBool` on the backend makes
    /// every later write return `StorageError::PreviousIo`, whose message is
    /// "Please close and re-open the database"
    /// (`redb/src/tree_store/page_store/cached_file.rs:125-145`,
    /// `redb/src/error.rs:65`; Apache-2.0/MIT, shape adapted, no code
    /// copied).
    cluster_state_boot: Arc<ClusterStateBootStatus>,
}

impl Engine {
    pub fn segment_hydration_cache_capacities(
        &self,
    ) -> [usize; crate::segment_cache_budget::CATEGORY_COUNT] {
        let mut total = [0_usize; crate::segment_cache_budget::CATEGORY_COUNT];
        for index in self.indices.iter() {
            let capacities = index.value().segment_hydration_cache_capacities();
            for (sum, capacity) in total.iter_mut().zip(capacities) {
                *sum = sum.saturating_add(capacity);
            }
        }
        total
    }

    /// Create a new engine, opening any existing indices from disk.
    pub fn new(mut config: Config) -> Result<Self> {
        // Runtime asset ownership belongs to this Engine, not to the caller's
        // clone lineage. This ensures a later Engine re-reads same-path ONNX
        // assets while all Index config clones within one Engine share bytes.
        config.embedding.runtime_onnx_assets = Arc::new(std::sync::OnceLock::new());
        let data_dir = PathBuf::from(&config.server.data_dir);
        std::fs::create_dir_all(&data_dir)?;

        // Data-dir exclusivity (RC4 blocker 13): take the node lock BEFORE
        // scanning/opening any index below — Index::open replays the WAL
        // and can flush segments, which must never happen while another
        // process serves the same directory.
        let node_lock = Arc::new(Self::acquire_node_lock(&data_dir)?);

        // Classify durable cluster metadata before Index::open can replay a
        // WAL or startup reconciliation can rewrite it. The parsed document
        // is retained and applied later; the path is never reread this boot.
        let PreparedClusterState {
            status: cluster_state_status,
            document: prepared_cluster_state,
        } = crate::cluster_state::preflight(&data_dir);
        let storage_available = cluster_state_status.is_writable();

        // Cloned before `config` is moved into the Arc below — the WAL tap
        // owns a live, runtime-editable copy (PUT /_xerj/wal_tap) rather than
        // reading the frozen boot config.
        let wal_tap_config = config.wal_tap.clone();

        // Built HERE, ahead of the struct literal, because `WalTap::new` is
        // what loads the persisted runtime configuration and it can override
        // `min_retained_generations` — which `store_config_from` reads out of
        // this `Config` when every index opens a few lines below. Freezing the
        // *file's* floor into the `Arc` while the tap ran with a different one
        // is how a floor set through `PUT /_xerj/wal_tap` survived a restart
        // in `_stats` and still reached no `WalWriter`.
        let wal_tap = Arc::new(crate::wal_tap::WalTap::new(&data_dir, wal_tap_config));
        config.wal_tap = wal_tap.config();

        // Apply operator-tunable aggregation bucket cap. Stored in a static
        // AtomicUsize inside aggs.rs so all per-bucket-allocator hot loops
        // can read it with no plumbing through every agg signature.
        crate::aggs::set_max_buckets(config.limits.max_buckets);

        // Initialise the process-wide resource governor (parent circuit
        // breaker) from config: the memtable byte budget, RSS watermark,
        // per-query memory guard, global search pool, and disk flood-stage
        // block. Idempotent — a second in-process Engine (tests) re-uses the
        // first governor. The background sampler that drives its trip flags
        // is started by `spawn_resource_sampler`, called once the engine is
        // Arc-wrapped.
        crate::governor::init(&config);

        // Install the engine's pool widths from `engine.{flush,merge,search}
        // _workers` before any pool is built. Idempotent, same as the governor
        // above: the first engine in a process fixes the widths, and a later
        // engine asking for different ones is told its request was ignored
        // rather than left to assume it took effect (#240 §4).
        crate::pools::init(&config.engine);

        let engine = Self {
            config: Arc::new(config),
            indices: Arc::new(DashMap::new()),
            data_dir: data_dir.clone(),
            aliases: Arc::new(DashMap::new()),
            templates: Arc::new(DashMap::new()),
            scrolls: Arc::new(DashMap::new()),
            pipelines: Arc::new(DashMap::new()),
            unrunnable_pipelines: Arc::new(DashMap::new()),
            definition_time_refusals: Arc::new(DashMap::new()),
            pipeline_refusals_unreadable: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            degraded_pipelines: Arc::new(DashMap::new()),
            closed_indices: Arc::new(DashMap::new()),
            data_streams: Arc::new(DashMap::new()),
            ilm_policies: Arc::new(DashMap::new()),
            ism_policies: Arc::new(DashMap::new()),
            managed_indices: Arc::new(DashMap::new()),
            lifecycle_detached: Arc::new(DashMap::new()),
            lifecycle_running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            index_created_at: Arc::new(DashMap::new()),
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
            wal_tap,
            api_keys: Arc::new(DashMap::new()),
            application_privileges: Arc::new(DashMap::new()),
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
            // Issue #201: durable, so the evidence outlives the incident it
            // is evidence of. Falls back to in-memory (with a warning) if the
            // file cannot be opened — an unwritable audit log must not stop
            // the node booting.
            // A blocked compatibility-fence boot is a diagnostic shell, not a
            // partially-open storage node. Opening the durable audit sink
            // rewrites audit.jsonl from its ring during construction, so keep
            // even that non-index store memory-only until a supported restart.
            audit: if storage_available {
                crate::audit::AuditLog::open(
                    crate::audit::DEFAULT_AUDIT_CAPACITY,
                    data_dir.join("audit.jsonl"),
                )
            } else {
                crate::audit::AuditLog::new(crate::audit::DEFAULT_AUDIT_CAPACITY)
            },
            roles: crate::rbac::RoleStore::new(),
            // Single-node default: 1 shard, "local" owner. Writes never
            // forward; multi-node mode overrides these via the Raft
            // commit handler when shard assignments change.
            node_id: Arc::new("local".to_string()),
            shard_router: Arc::new(parking_lot::RwLock::new(
                xerj_cluster::router::ShardRouter::new(1),
            )),
            _node_lock: node_lock,
            cluster_state_write: Arc::new(parking_lot::Mutex::new(())),
            cluster_state_boot: Arc::new(cluster_state_status),
        };

        // A rejected cluster-state document means this binary cannot know the
        // storage topology that document describes. Do not open *any* index in
        // that state: Index::open replays WALs and may publish a segment. This
        // includes `.xerj_*` system indices; treating them as harmless was the
        // exact path that mutated `.xerj_dashboards` during the causal A/B.
        if storage_available {
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
                            // #320 — `engine.config.wal_tap` is already the
                            // tap's effective configuration (see `WalTap::new`
                            // above), so the store opened with the right
                            // floor; this makes that true by construction
                            // rather than by argument, and covers a tap
                            // reconfigured between two boot-loop iterations.
                            idx.set_wal_min_retained_generations(engine.wal_retention_floor());
                            // Restore the raw ES mapping blob (analyzers, formats,
                            // dims — full fidelity) BEFORE any ingest/query can run,
                            // so GET /_mapping and mapping-dependent code paths see
                            // the same mapping as pre-restart. A corrupt blob fails
                            // the index (#202) rather than serving it with a
                            // silently reduced mapping.
                            if let Err(e) = engine.load_persisted_es_mapping(&name_str) {
                                warn!(name = name_str.as_str(), error = %e, "failed to open index");
                                engine.record_failed_index(&name_str, e.to_string());
                                continue;
                            }
                            // The index isn't registered yet, so the propagation
                            // inside load can't find it — set the toggles on the
                            // local handle instead.
                            if let Some(m) = engine.index_mappings.get(name_str.as_str()) {
                                Engine::apply_date_mapping_flags(&idx, m.value());
                            }
                            engine.load_or_backfill_index_created_at(&name_str);
                            engine.indices.insert(name_str, idx);
                        }
                        Err(e) => {
                            warn!(name = name_str.as_str(), error = %e, "failed to open index");
                            // The mapping blob lives beside the data, not inside
                            // the store, so it is readable even when the store
                            // refuses to open. Load it so the metadata surfaces
                            // (`GET /{index}`, `GET /{index}/_mapping`) can still
                            // tell the operator what was in the index they are
                            // trying to recover. Propagation into the (absent)
                            // handle no-ops. Since #202 this load can itself fail
                            // (unreadable/unparseable `es_mapping.json`); the index
                            // is already being quarantined for the store error, so
                            // the extra failure is reported rather than dropped —
                            // it tells the operator a second file needs repairing.
                            if let Err(map_err) = engine.load_persisted_es_mapping(&name_str) {
                                warn!(
                                    name = name_str.as_str(),
                                    error = %map_err,
                                    "es_mapping.json is also unreadable; the mapping surfaces \
                                     cannot show what this index held"
                                );
                            }
                            engine.record_failed_index(&name_str, e.to_string());
                        }
                    }
                }
            }

            // Restore persisted API keys (item 6) so keys minted before a restart
            // still authenticate. Must run before the server starts accepting
            // requests (i.e. here in `new`), and is cheap (one small JSON file).
            engine.load_persisted_api_keys();

            // Restore persisted aliases so e.g. `.kibana` (always an alias,
            // never a bare index) still resolves after a restart — see
            // `load_persisted_aliases`'s doc comment for the concrete failure
            // this caused (OpenSearch Dashboards stuck indefinitely on every
            // restart, mistaking a missing-alias 404 for a still-in-progress
            // migration by another instance).
            engine.load_persisted_aliases();
            // …and the alias filter metadata that pairs with that topology, or
            // a filtered alias comes back unfiltered — wrong reads, and a
            // by-query that empties the whole backing index (#524).
            engine.load_alias_metadata();

            // Restore the cluster-level management state — index templates,
            // legacy templates, component templates, ingest pipelines, data
            // streams, ILM policies (issue #203). Before this, "replace the
            // binary and restart" silently reverted every one of them: the
            // documents kept flowing, into indices that no longer had the shape
            // the operator designed. Must run after the index scan above, so a
            // restored data stream can report which of its backing indices are
            // actually present, and before the server accepts requests.
            // Issue #204: the refusal markers are read BEFORE the pipelines
            // they classify, and unconditionally — not from inside
            // `apply_cluster_state`, which returns early when there is no
            // `cluster_state.json`. On that path an unreadable sidecar left
            // `pipeline_refusals_unreadable` false, so the first refused `PUT`
            // would have overwritten the file with a one-entry map and erased
            // every marker still in it.
            engine.load_persisted_pipeline_refusals();
            engine.apply_cluster_state(prepared_cluster_state);

            // Then say plainly which `.ds-*` indices no restored stream claims.
            // Nothing is deleted — see the doc comment; this is the log line that
            // stops an unreachable backing index from being invisible.
            engine.warn_orphaned_backing_indices();

            // Restore ISM/ILM policies and managed-index execution state so a
            // policy attached before a restart keeps running afterward instead
            // of silently going idle. Separate files (`ism_policies.json`,
            // `ism_managed_indices.json`) from `cluster_state.json` above, and
            // deliberately so: that file holds the verbatim documents an
            // operator PUT, this holds the executor's own cursor. Loaded after
            // `apply_cluster_state` because a restored data stream is what a
            // rollover action operates on.
            engine.load_persisted_ism_policies();
            engine.load_persisted_managed_indices();

            // Spawn the PIT sweeper. Pre-v0.6.2 PITs accumulated forever;
            // every open without close was a memory leak. The sweeper
            // walks `engine.pits` every `pit.sweep_interval_secs` and
            // drops any with `expires_at < now`. Cheap (DashMap iter +
            // Instant compare) and bounded by the live PIT count.
            engine.spawn_pit_sweeper();

            // Spawn the scroll + async-search context sweeper (RC4 blocker
            // 11) — the scroll/async twin of the PIT sweeper above. Each
            // scroll pins a full Vec<Hit> and each async search pins its
            // response JSON; without a sweeper both maps grow until an
            // explicit client DELETE, i.e. forever under normal client
            // behavior.
            engine.spawn_search_context_sweeper();
        } else {
            warn!(
                "storage activation skipped because the boot-time cluster-state format fence is latched"
            );
        }

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
                Err(EngineError::Common(xerj_common::XerjError::config(
                    format!(
                        "data dir '{}' is already in use by another running xerj process \
                     (pid {holder}, lock file '{}') — refusing to start. Two processes \
                     serving one data dir would replay each other's WAL and corrupt \
                     segments; stop the other process or point this one at its own \
                     server.data_dir.",
                        data_dir.display(),
                        lock_path.display(),
                    ),
                )))
            }
            Err(std::fs::TryLockError::Error(e)) => {
                Err(EngineError::Common(xerj_common::XerjError::config(
                    format!("failed to acquire node lock '{}': {e}", lock_path.display()),
                )))
            }
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

    /// Drop scroll contexts whose `expires_at` is in the past. Mirrors
    /// [`Engine::sweep_expired_pits`]: cheap O(N) walk, run by the
    /// background sweeper and opportunistically before opening a new
    /// scroll so a tight open-without-clear loop self-bounds.
    pub fn sweep_expired_scrolls(&self) -> usize {
        let now = Instant::now();
        let expired: Vec<String> = self
            .scrolls
            .iter()
            .filter(|e| e.value().expires_at <= now)
            .map(|e| e.key().clone())
            .collect();
        for id in &expired {
            self.scrolls.remove(id);
        }
        expired.len()
    }

    /// Drop stored async-search results whose `expiration_time_in_millis`
    /// is in the past. The expiry lives inside the stored response JSON
    /// (it is part of the ES wire format), so the sweep reads it from
    /// there; a malformed/missing field counts as expired — every writer
    /// sets it, so that arm only fires on corruption.
    pub fn sweep_expired_async_searches(&self) -> usize {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(i64::MAX);
        let expired: Vec<String> = self
            .async_searches
            .iter()
            .filter(|e| {
                e.value()
                    .get("expiration_time_in_millis")
                    .and_then(Value::as_i64)
                    .map(|exp| exp <= now_ms)
                    .unwrap_or(true)
            })
            .map(|e| e.key().clone())
            .collect();
        for id in &expired {
            self.async_searches.remove(id);
        }
        expired.len()
    }

    /// Background sweeper for scroll + async-search contexts (RC4
    /// blocker 11) — the twin of [`Engine::spawn_pit_sweeper`].
    ///
    /// Deliberately captures ONLY the two context maps (never a full
    /// `Engine` clone): the engine holds the data-dir `node.lock`, and a
    /// long-lived task owning an `Engine` clone would keep that lock
    /// alive after the last user-visible engine is dropped.
    fn spawn_search_context_sweeper(&self) {
        let scrolls = Arc::clone(&self.scrolls);
        let async_searches = Arc::clone(&self.async_searches);
        let interval =
            std::time::Duration::from_secs(self.config.search_context.sweep_interval_secs.max(1));
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            // Skip the immediate first tick — Engine::new just ran, so
            // both maps are empty.
            tick.tick().await;
            loop {
                tick.tick().await;
                let now = Instant::now();
                let expired_scrolls: Vec<String> = scrolls
                    .iter()
                    .filter(|e| e.value().expires_at <= now)
                    .map(|e| e.key().clone())
                    .collect();
                for id in &expired_scrolls {
                    scrolls.remove(id);
                }
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(i64::MAX);
                let expired_async: Vec<String> = async_searches
                    .iter()
                    .filter(|e| {
                        e.value()
                            .get("expiration_time_in_millis")
                            .and_then(Value::as_i64)
                            .map(|exp| exp <= now_ms)
                            .unwrap_or(true)
                    })
                    .map(|e| e.key().clone())
                    .collect();
                for id in &expired_async {
                    async_searches.remove(id);
                }
                if !expired_scrolls.is_empty() || !expired_async.is_empty() {
                    tracing::debug!(
                        swept_scrolls = expired_scrolls.len(),
                        swept_async = expired_async.len(),
                        remaining_scrolls = scrolls.len(),
                        remaining_async = async_searches.len(),
                        "search-context sweep dropped expired contexts",
                    );
                }
            }
        });
    }

    /// Create a new index, applying any matching index template.
    pub fn create_index(&self, name: &str, schema: Schema) -> Result<()> {
        self.create_index_with_settings(name, schema, Value::Null)
    }

    /// Merge the highest-priority matching index template's declared fields
    /// into `schema` (adding any the schema does not already define). Applied
    /// on every create path so template behavior is independent of whether the
    /// caller supplied settings.
    fn apply_index_template(&self, name: &str, schema: Schema) -> Schema {
        let mut effective_schema = schema;
        if let Some(tmpl) = self.best_matching_template(name) {
            if let Some(props) = tmpl.mappings.get("properties") {
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
        effective_schema
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
        // Index creation is storage activation: it creates the directory and
        // WAL before any cluster-state serializer is involved. A blocked boot
        // must reject here so HTTP, gRPC, direct CLI, and Console callers share
        // the same fail-closed boundary.
        self.ensure_cluster_state_writable()?;
        let index_name = IndexName::new(name).map_err(EngineError::Common)?;

        // Per-index authorization backstop (issue #79): a caller that may not
        // see this name must not be able to bring it into existence either —
        // otherwise it could squat a brain name it cannot read. Reported as a
        // create failure rather than a distinct refusal so the two are
        // indistinguishable. See `crate::index_guard`.
        if !crate::index_guard::visible(name) {
            return Err(EngineError::Common(
                xerj_common::XerjError::index_not_found(name),
            ));
        }

        if self.indices.contains_key(name) {
            return Err(EngineError::Common(
                xerj_common::XerjError::index_already_exists(name),
            ));
        }

        // The name is taken by an index that exists on disk but would not
        // open. Creating over it would run `Index::create` across the existing
        // store and overwrite `schema.json` with an empty mapping — the very
        // mapping loss #202 is about, reached through the other door, and it
        // would destroy the evidence too. Refuse here with the recorded reason
        // instead (issue #206).
        if let Some(f) = self.failed_indices.get(name) {
            return Err(EngineError::Common(
                xerj_common::XerjError::index_unavailable(name, f.reason.clone()),
            ));
        }

        // Apply matching template (highest priority wins) on every create path.
        let effective_schema = self.apply_index_template(name, schema);
        let idx = Index::create_with_settings(
            index_name,
            effective_schema,
            settings,
            &self.config,
            &self.data_dir,
        )?;
        // #320 — the store was seeded from the boot `Config`; the tap's live
        // floor is what an operator last asked for. Applied before the index
        // is published, so no ingest (and therefore no prune) can run against
        // the wrong floor.
        idx.set_wal_min_retained_generations(self.wal_retention_floor());
        self.indices.insert(name.to_string(), idx);
        self.record_index_created_at(name);
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
    pub fn put_index_mapping(&self, name: &str, mapping: Value) -> Result<()> {
        self.ensure_cluster_state_writable()?;
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
        self.propagate_date_detection(name, &mapping);
        self.index_mappings.insert(name.to_string(), mapping);
        Ok(())
    }

    /// Push the mapping's `date_detection` toggle (default true) down to the
    /// open `Index` so dynamic inference honors it. The blob shape varies by
    /// caller (`{"date_detection": ..}` from PUT /_mapping, or nested under
    /// `"mappings"` from index-create), so both levels are checked.
    fn propagate_date_detection(&self, name: &str, mapping: &Value) {
        if let Ok(idx) = self.get_index(name) {
            Self::apply_date_mapping_flags(&idx, mapping);
        }
    }

    /// Push both date-related mapping toggles down to an open index:
    /// the `date_detection` bool and the set of date fields excluded from
    /// default-format ingest validation (explicit `format` — those are
    /// validated against their own format by the bulk path — or
    /// `ignore_malformed`).
    pub(crate) fn apply_date_mapping_flags(idx: &crate::index::Index, mapping: &Value) {
        idx.set_date_detection(Self::mapping_date_detection(mapping));
        idx.set_date_format_exclusions(Self::mapping_date_exclusions(mapping));
    }

    /// Read the `date_detection` toggle out of a raw mapping blob
    /// (defaulting to true, like ES).
    pub(crate) fn mapping_date_detection(mapping: &Value) -> bool {
        mapping
            .get("date_detection")
            .or_else(|| {
                mapping
                    .get("mappings")
                    .and_then(|m| m.get("date_detection"))
            })
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    /// Top-level date fields carrying an explicit `format` or
    /// `ignore_malformed` in the raw mapping blob (either shape).
    fn mapping_date_exclusions(mapping: &Value) -> std::collections::HashSet<String> {
        let props = mapping
            .get("properties")
            .or_else(|| mapping.get("mappings").and_then(|m| m.get("properties")));
        let mut out = std::collections::HashSet::new();
        let Some(obj) = props.and_then(Value::as_object) else {
            return out;
        };
        for (fname, spec) in obj {
            let ftype = spec.get("type").and_then(Value::as_str).unwrap_or("");
            if ftype != "date" && ftype != "date_nanos" {
                continue;
            }
            let has_format = spec
                .get("format")
                .and_then(Value::as_str)
                .is_some_and(|f| !f.is_empty());
            let ignore_malformed = spec
                .get("ignore_malformed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if has_format || ignore_malformed {
                out.insert(fname.clone());
            }
        }
        out
    }

    /// Load a previously-persisted raw ES mapping blob for `name` (if any)
    /// into the in-memory `index_mappings` map.  Called whenever an index is
    /// (re)opened from disk — boot scan and snapshot restore.  A missing
    /// file is fine (pre-fix indices, dynamic-only indices): readers fall
    /// back to schema-derived properties from `schema.json`.
    ///
    /// A file that is present but unreadable or unparseable is **not** fine and
    /// is no longer logged-and-ignored (#202). This blob is the full-fidelity
    /// mapping — analyzers, date formats, `dense_vector` dims — and is what
    /// `GET /{index}/_mapping` answers with; dropping it leaves the index
    /// serving a quietly emptier mapping than the one its own data was written
    /// under. The caller fails the index instead, which turns cluster health
    /// red and keeps the corrupt index out of the served set.
    fn load_persisted_es_mapping(&self, name: &str) -> Result<()> {
        let path = self.data_dir.join(name).join("es_mapping.json");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(EngineError::CorruptIndexMetadata {
                    file: path.display().to_string(),
                    reason: e.to_string(),
                })
            }
        };
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(mapping) => {
                self.propagate_date_detection(name, &mapping);
                self.index_mappings.insert(name.to_string(), mapping);
                Ok(())
            }
            Err(e) => Err(EngineError::CorruptIndexMetadata {
                file: path.display().to_string(),
                reason: format!("{e} ({} bytes on disk)", bytes.len()),
            }),
        }
    }

    /// Path of the persisted API-key store (`<data_dir>/api_keys.json`).
    fn api_keys_path(&self) -> PathBuf {
        self.data_dir.join("api_keys.json")
    }

    /// Insert (or overwrite) an API key and durably persist the whole store
    /// (item 6). The auth middleware re-authenticates `Authorization: ApiKey
    /// <encoded>` against `api_keys`; before this, that map lived only in
    /// memory, so a restart silently invalidated every minted key. Now each
    /// mutation snapshots the full map to `<data_dir>/api_keys.json` (0600,
    /// atomic temp+rename), reloaded on boot.
    ///
    /// A blocked boot returns the typed compatibility error before inserting
    /// the key. Once storage is writable, an ordinary persistence I/O failure
    /// remains best-effort: it is logged but does not fail the create — the key
    /// still works until the next restart, matching the old behavior.
    pub fn persist_api_key(&self, id: String, record: ApiKeyRecord) -> Result<()> {
        self.ensure_cluster_state_writable()?;
        self.api_keys.insert(id, record);
        self.flush_api_keys_best_effort();
        Ok(())
    }

    /// Invalidate (revoke) minted API keys by id — issue #208's missing half.
    /// `ApiKeyRecord.invalidated` has been honoured by the auth path since the
    /// field existed, but nothing could ever set it, so a leaked key was
    /// permanent and rotation impossible.
    ///
    /// Returns `(invalidated, previously_invalidated)` ids — the two non-error
    /// buckets of ES's `DELETE /_security/api_key` response. An id that
    /// matches no record lands in **neither** list: ES resolves selectors to
    /// keys first and answers with an empty response when nothing matches
    /// (`ApiKeyService#invalidateApiKeys`), it does not error per unknown id.
    ///
    /// The flag and `invalidation_ms` are flipped in-memory — the auth
    /// middleware reads this same map, so revocation takes effect on the very
    /// next request — and the store is flushed to `api_keys.json` once at the
    /// end, same durability contract as [`Self::persist_api_key`].
    pub fn invalidate_api_keys(
        &self,
        ids: &[String],
        now_ms: u64,
    ) -> Result<(Vec<String>, Vec<String>)> {
        self.ensure_cluster_state_writable()?;
        let mut invalidated = Vec::new();
        let mut previously = Vec::new();
        for id in ids {
            let Some(mut rec) = self.api_keys.get_mut(id) else {
                continue;
            };
            if rec.invalidated {
                previously.push(id.clone());
            } else {
                rec.invalidated = true;
                rec.invalidation_ms = Some(now_ms);
                invalidated.push(id.clone());
            }
            // `rec` (a DashMap guard) drops here, before `flush_api_keys`
            // re-iterates the map below.
        }
        if !invalidated.is_empty() {
            self.flush_api_keys_best_effort();
        }
        Ok((invalidated, previously))
    }

    /// Serialize the current `api_keys` map to `<data_dir>/api_keys.json`
    /// atomically with owner-only (0600) permissions.
    ///
    /// Since #201 the file holds only salted hashes, but 0600 stays: a hash
    /// plus a key id is still an offline target and still tells a reader
    /// exactly which credentials exist.
    fn flush_api_keys(&self) -> std::io::Result<()> {
        let snapshot: std::collections::HashMap<String, ApiKeyRecord> = self
            .api_keys
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        let bytes = serde_json::to_vec_pretty(&snapshot)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        write_secret_file_atomic(&self.api_keys_path(), &bytes)
    }

    /// [`Self::flush_api_keys`] for the mutation paths, where a persistence
    /// failure must not fail the request: the key still works until the next
    /// restart, which is the pre-persistence behaviour rather than a
    /// regression of key creation.
    fn flush_api_keys_best_effort(&self) {
        if let Err(e) = self.flush_api_keys() {
            warn!(error = %e, "failed to persist api_keys.json (keys work until restart)");
        }
    }

    /// Load persisted API keys from `<data_dir>/api_keys.json` into the
    /// in-memory map on boot. A missing file is normal (fresh node / no keys
    /// ever minted); a corrupt file is logged and ignored (the node still
    /// boots — the admin key path is unaffected).
    ///
    /// # Migration off plaintext (issue #201)
    ///
    /// A store written before #201 holds `"secret": "<the credential>"`. Those
    /// records are hashed here and the file is rewritten **during boot**,
    /// before the server accepts a request, so an upgraded node stops leaking
    /// on first start rather than on first key rotation. The migration is
    /// one-way and needs no flag day: [`crate::secret_hash::is_usable_hash`]
    /// is the discriminator ([`ApiKeyRecord::migrate_from_plaintext`] calls
    /// it), so a store whose hashes decode is untouched, and only the exact
    /// pre-#201 shape — a `secret` and no `secret_hash` at all — is migrated.
    ///
    /// A record that is neither of those two shapes is **dropped with an
    /// error**, not kept: keeping it would leave an entry that `GET
    /// /_security/api_key` lists as a live credential while nothing can ever
    /// authenticate as it — precisely the accept-then-ignore behaviour that
    /// issue #204 tracks. Since "usable" is a full decode, a `secret_hash`
    /// that is present but unparseable is dropped exactly like an absent one;
    /// both are equally unauthenticatable.
    fn load_persisted_api_keys(&self) {
        let path = self.api_keys_path();
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        match serde_json::from_slice::<std::collections::HashMap<String, ApiKeyRecord>>(&bytes) {
            Ok(map) => {
                let mut restored = 0usize;
                let mut migrated = 0usize;
                let mut dropped = 0usize;
                for (id, mut rec) in map {
                    match rec.migrate_from_plaintext() {
                        Ok(changed) => {
                            if changed {
                                migrated += 1;
                            }
                            restored += 1;
                            self.api_keys.insert(id, rec);
                        }
                        Err(()) => {
                            dropped += 1;
                            error!(
                                key_id = %id,
                                "api_keys.json record is not a credential this build can \
                                 use: its secret_hash does not decode (absent, truncated, \
                                 or an encoding this build does not know) and it is not \
                                 the pre-#201 plaintext shape either — dropping it; \
                                 nothing could ever authenticate as this key and listing \
                                 it would be a lie"
                            );
                        }
                    }
                }
                if restored > 0 {
                    info!(count = restored, "restored persisted API keys");
                }
                if migrated > 0 {
                    // Rewrite immediately: until this lands, the plaintext is
                    // still on disk. A failure here is not cosmetic, so it is
                    // an error, not a warning — the operator has to know the
                    // node is still leaking and why.
                    match self.flush_api_keys() {
                        Ok(()) => info!(
                            count = migrated,
                            "migrated API key secrets to salted hashes; plaintext removed \
                             from api_keys.json"
                        ),
                        Err(e) => error!(
                            error = %e,
                            count = migrated,
                            path = %path.display(),
                            "could not rewrite api_keys.json after hashing — PLAINTEXT API \
                             KEY SECRETS REMAIN ON DISK. Fix the permissions/space problem \
                             and restart, or rotate the keys."
                        ),
                    }
                }
                if dropped > 0 {
                    error!(count = dropped, "dropped unusable API key records");
                }
            }
            Err(e) => {
                warn!(error = %e, "ignoring corrupt api_keys.json");
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

    /// Path of the persisted alias catalog (`<data_dir>/aliases.json`).
    fn aliases_path(&self) -> PathBuf {
        self.data_dir.join("aliases.json")
    }

    /// Serialize the current `aliases` map to `<data_dir>/aliases.json`
    /// atomically (temp-file + rename), mirroring `flush_api_keys`. Every
    /// alias mutation snapshots the full map; a write failure is logged but
    /// non-fatal (the alias still works in-memory until the next restart).
    fn flush_aliases(&self) {
        let snapshot: std::collections::HashMap<String, Vec<String>> = self
            .aliases
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        let bytes = match serde_json::to_vec_pretty(&snapshot) {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "failed to serialize aliases for persistence");
                return;
            }
        };
        if let Err(e) = crate::index::write_file_atomic(&self.aliases_path(), &bytes) {
            warn!(error = %e, "failed to persist aliases.json (aliases work until restart)");
        }
    }

    /// Load persisted aliases from `<data_dir>/aliases.json` into the
    /// in-memory map on boot. A missing file is normal (fresh node / no
    /// aliases ever created); a corrupt file is logged and ignored.
    ///
    /// Without this, `.kibana` (always an alias, never a bare index —
    /// Kibana/OpenSearch Dashboards create it via `PUT .kibana_1` +
    /// `POST _aliases`) silently stopped resolving on every restart: the
    /// backing index's data survived, but the alias pointing at it didn't,
    /// so `GET /.kibana` 404'd and OSD's saved-objects migrator concluded
    /// the migration was still in progress (never actually completing) —
    /// found empirically via a real OpenSearch Dashboards container stuck
    /// on "Another OpenSearch Dashboards instance appears to be migrating
    /// the index" on every restart, indefinitely.
    fn load_persisted_aliases(&self) {
        let path = self.aliases_path();
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        match serde_json::from_slice::<std::collections::HashMap<String, Vec<String>>>(&bytes) {
            Ok(map) => {
                let n = map.len();
                for (alias, indices) in map {
                    self.aliases.insert(alias, indices);
                }
                if n > 0 {
                    info!(count = n, "restored persisted aliases");
                }
            }
            Err(e) => {
                warn!(error = %e, "ignoring corrupt aliases.json");
            }
        }
    }

    // ── Alias filter metadata durability (issue #524) ───────────────────────

    /// Path of the persisted alias-metadata sidecar
    /// (`<data_dir>/alias_metadata.json`).
    ///
    /// A SEPARATE file from `aliases.json` (which persists only the
    /// alias→members topology) on purpose. Each alias's `filter` (and the
    /// other create-time metadata) lived only in the in-memory
    /// `index_alias_metadata` map and evaporated on restart, so a filtered
    /// alias came back with its members but no filter — wrong `_search` /
    /// `_count` results, and once the by-query paths honour the filter, a
    /// `_delete_by_query` that empties the whole backing index instead of the
    /// slice the filter names (#524). A separate file keeps `aliases.json`'s
    /// shape unchanged; an older binary simply ignores this one, the same
    /// downgrade-safe call the ingest-pipeline-refusals sidecar makes.
    fn alias_metadata_path(&self) -> PathBuf {
        self.data_dir.join("alias_metadata.json")
    }

    /// Serialize `index_alias_metadata` to disk atomically (temp-file +
    /// rename), mirroring [`Engine::flush_aliases`]. Called after every
    /// mutation of the map; a write failure is logged but non-fatal (the
    /// filter still applies in-memory until the next restart).
    pub fn flush_alias_metadata(&self) {
        let snapshot: std::collections::HashMap<String, serde_json::Value> = self
            .index_alias_metadata
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        let bytes = match serde_json::to_vec_pretty(&snapshot) {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "failed to serialize alias metadata for persistence");
                return;
            }
        };
        if let Err(e) = crate::index::write_file_atomic(&self.alias_metadata_path(), &bytes) {
            warn!(error = %e, "failed to persist alias_metadata.json (alias filters work until restart)");
        }
    }

    /// Load persisted alias metadata from `<data_dir>/alias_metadata.json` into
    /// the in-memory map on boot. A missing file is normal (fresh node, or a
    /// node whose aliases were all created by a build that predates this
    /// sidecar); a corrupt file is logged and ignored.
    fn load_alias_metadata(&self) {
        let path = self.alias_metadata_path();
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        match serde_json::from_slice::<std::collections::HashMap<String, serde_json::Value>>(&bytes)
        {
            Ok(map) => {
                let n = map.len();
                for (index, meta) in map {
                    self.index_alias_metadata.insert(index, meta);
                }
                if n > 0 {
                    info!(count = n, "restored persisted alias metadata");
                }
            }
            Err(e) => {
                warn!(error = %e, "ignoring corrupt alias_metadata.json");
            }
        }
    }

    // ── Ingest-pipeline refusal provenance (issue #204) ─────────────────────

    /// Sidecar holding [`Engine::definition_time_refusals`].
    ///
    /// A separate file rather than an eighth field in `cluster_state.json`:
    /// that envelope is `deny_unknown_fields`, so widening it would make every
    /// file this build writes unreadable to an older binary — a downgrade
    /// break traded for an upgrade fix. An older binary ignores this file.
    fn pipeline_refusals_path(&self) -> PathBuf {
        self.data_dir.join("ingest-pipeline-refusals.json")
    }

    /// Write the whole refusal map atomically.
    ///
    /// `Err` means the marker did not reach disk, and the caller MUST NOT
    /// acknowledge: without it the next boot cannot tell this definition from
    /// one an older, gate-less build wrote, and would soften the refusal back
    /// into a `201` (issue #204). Same rule `flush_cluster_state` already
    /// enforces for the definition itself (issue #203).
    ///
    /// # Why the snapshot is taken inside the lock
    ///
    /// This writes the WHOLE map, so two concurrent refused `PUT`s are a
    /// lost-update race, not a merge: each inserts its own marker, each
    /// snapshots, and the one that writes SECOND with the OLDER snapshot
    /// erases the other's marker from disk — while both callers are told
    /// `{"acknowledged": true}`, which is exactly the promise this function's
    /// `Err` contract exists to keep. Measured on the unlocked version at a
    /// live node: 40 concurrent refused `PUT`s, 40 acknowledged, 37 markers on
    /// disk; after a plain restart the three lost ones replayed at
    /// `ReplayPersisted`, answered `201`, and dropped a field from a document
    /// their `if` guard excluded. Sequential tests never see it.
    ///
    /// `cluster_state_write` is the mutex, not a second one, because the two
    /// files are written back to back by the same callers and one order for
    /// both is easier to reason about than two. It is taken and released here,
    /// never held across `flush_cluster_state` — `parking_lot::Mutex` is not
    /// reentrant.
    fn flush_pipeline_refusals(&self) -> Result<()> {
        // A boot that could not READ this file has an empty in-memory map, so
        // writing it would erase every marker still on disk: the next boot
        // would parse the truncated file cleanly, conclude those definitions
        // have no provenance, and replay them at `ReplayPersisted` — the very
        // hole the marker closes, reopened by the fail-closed path itself.
        // Refuse instead, and name the file, so the operator's repair is to
        // move it aside rather than to discover the loss at the next restart
        // (issue #204).
        if self
            .pipeline_refusals_unreadable
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(EngineError::Common(xerj_common::XerjError::internal(
                format!(
                    "{} could not be read at boot, so its markers are not in memory and \
                     rewriting it here would erase every refusal it still holds; every \
                     persisted pipeline is being replayed at definition strictness until \
                     the file is moved aside and this node restarted",
                    self.pipeline_refusals_path().display()
                ),
            )));
        }
        // One writer at a time, snapshot included — see the doc comment.
        let _writing = self.cluster_state_write.lock();
        let snapshot: std::collections::BTreeMap<String, String> = self
            .definition_time_refusals
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        let bytes = serde_json::to_vec_pretty(&snapshot).map_err(|e| {
            xerj_common::XerjError::internal(format!(
                "could not serialise the ingest-pipeline refusal markers: {e}"
            ))
        })?;
        crate::index::write_file_atomic(&self.pipeline_refusals_path(), &bytes).map_err(|e| {
            EngineError::Common(xerj_common::XerjError::internal(format!(
                "could not persist {}: {e}",
                self.pipeline_refusals_path().display()
            )))
        })
    }

    /// Clearing a refusal marker is not on the acknowledge path — a marker
    /// that outlives its cause makes a pipeline *more* refused, never less —
    /// so a write failure is logged rather than propagated.
    fn flush_pipeline_refusals_best_effort(&self) {
        if let Err(e) = self.flush_pipeline_refusals() {
            warn!(
                error = %e,
                "could not clear an ingest-pipeline refusal marker; the pipeline will be \
                 refused again after the next restart until a re-PUT succeeds"
            );
        }
    }

    /// Read the refusal markers back at boot, before any pipeline is replayed.
    ///
    /// A missing file is the normal case for a data dir written before this
    /// build — every pipeline in it is then treated as "no provenance", which
    /// is exactly the compatibility door the replay mode exists for. A corrupt
    /// file is NOT treated as "no refusals": that would silently reopen the
    /// hole this marker closes, so the boot refuses to run any pipeline it
    /// cannot classify.
    fn load_persisted_pipeline_refusals(&self) {
        let path = self.pipeline_refusals_path();
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                error!(
                    path = %path.display(), error = %e,
                    "ingest-pipeline refusal markers are unreadable; every persisted pipeline \
                     will be replayed at DEFINITION strictness so a refusal cannot be lost"
                );
                self.pipeline_refusals_unreadable
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                return;
            }
        };
        match serde_json::from_slice::<std::collections::BTreeMap<String, String>>(&bytes) {
            Ok(map) => {
                let n = map.len();
                for (id, reason) in map {
                    self.definition_time_refusals.insert(id, reason);
                }
                if n > 0 {
                    info!(
                        count = n,
                        "restored ingest-pipeline refusals recorded at definition time"
                    );
                }
            }
            Err(e) => {
                error!(
                    path = %path.display(), error = %e,
                    "ingest-pipeline refusal markers are corrupt; every persisted pipeline will \
                     be replayed at DEFINITION strictness so a refusal cannot be lost"
                );
                self.pipeline_refusals_unreadable
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }

    // ── Lifecycle management (ISM/ILM) persistence ──────────────────────────

    fn ism_policies_path(&self) -> PathBuf {
        self.data_dir.join("ism_policies.json")
    }

    fn managed_indices_path(&self) -> PathBuf {
        self.data_dir.join("ism_managed_indices.json")
    }

    /// Insert (or overwrite) an ISM-shaped policy and persist the whole
    /// store, mirroring `flush_aliases`. Both `_plugins/_ism/policies` (as
    /// entered) and `_ilm/policy` (after translation) write through here —
    /// see the `ism_policies` field doc.
    pub fn put_ism_policy(
        &self,
        id: String,
        policy: crate::lifecycle::LifecyclePolicy,
    ) -> Result<()> {
        self.ensure_cluster_state_writable()?;
        self.ism_policies.insert(id, policy);
        self.flush_ism_policies();
        Ok(())
    }

    pub fn remove_ism_policy(&self, id: &str) -> Result<bool> {
        self.ensure_cluster_state_writable()?;
        let removed = self.ism_policies.remove(id).is_some();
        if removed {
            self.flush_ism_policies();
        }
        Ok(removed)
    }

    fn flush_ism_policies(&self) {
        let snapshot: std::collections::HashMap<String, crate::lifecycle::LifecyclePolicy> = self
            .ism_policies
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        let bytes = match serde_json::to_vec_pretty(&snapshot) {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "failed to serialize ism_policies for persistence");
                return;
            }
        };
        if let Err(e) = crate::index::write_file_atomic(&self.ism_policies_path(), &bytes) {
            warn!(error = %e, "failed to persist ism_policies.json (policies work until restart)");
        }
    }

    fn load_persisted_ism_policies(&self) {
        let Ok(bytes) = std::fs::read(self.ism_policies_path()) else {
            return;
        };
        match serde_json::from_slice::<
            std::collections::HashMap<String, crate::lifecycle::LifecyclePolicy>,
        >(&bytes)
        {
            Ok(map) => {
                let n = map.len();
                let mut restored = 0;
                for (id, policy) in map {
                    // Every REST-reachable insertion path (native ISM PUT and
                    // the ILM-shape translation) already runs `validate()`
                    // before a policy lands here — this is the one path that
                    // doesn't, since it reads back whatever was on disk. A
                    // hand-edited or corrupted single entry in an otherwise
                    // parseable file would silently satisfy `change_policy`'s
                    // "does this policy_id exist" check without ever having
                    // been checked structurally. Skip just that entry rather
                    // than discarding the whole file, matching the
                    // per-entry granularity of the corruption it's guarding
                    // against.
                    if let Err(reason) = policy.validate() {
                        warn!(policy_id = %id, error = %reason, "skipping invalid persisted ISM/ILM policy");
                        continue;
                    }
                    self.ism_policies.insert(id, policy);
                    restored += 1;
                }
                if restored > 0 {
                    info!(count = restored, "restored persisted ISM/ILM policies");
                }
                if restored < n {
                    warn!(
                        skipped = n - restored,
                        "some persisted ISM/ILM policies failed validation and were not restored"
                    );
                }
            }
            Err(e) => warn!(error = %e, "ignoring corrupt ism_policies.json"),
        }
    }

    /// Persist the whole `managed_indices` map plus the detach tombstones
    /// (issue #282). Called by `lifecycle::tick` after any change, and by
    /// the attach/detach handlers.
    ///
    /// On-disk shape is an envelope — `{"managed": {...}, "detached":
    /// [...]}` — so the acknowledged detaches survive a restart alongside
    /// the cursors. `load_persisted_managed_indices` still accepts the
    /// pre-#282 bare-map form.
    pub fn persist_managed_indices(&self) -> Result<()> {
        self.ensure_cluster_state_writable()?;
        let managed: std::collections::HashMap<String, crate::lifecycle::ManagedIndexState> = self
            .managed_indices
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        let mut detached: Vec<String> = self
            .lifecycle_detached
            .iter()
            .map(|e| e.key().clone())
            .collect();
        detached.sort();
        let envelope = serde_json::json!({ "managed": managed, "detached": detached });
        let bytes = match serde_json::to_vec_pretty(&envelope) {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "failed to serialize managed_indices for persistence");
                return Ok(());
            }
        };
        if let Err(e) = crate::index::write_file_atomic(&self.managed_indices_path(), &bytes) {
            warn!(error = %e, "failed to persist ism_managed_indices.json (state works until restart)");
        }
        Ok(())
    }

    fn load_persisted_managed_indices(&self) {
        let Ok(bytes) = std::fs::read(self.managed_indices_path()) else {
            return;
        };
        // `deny_unknown_fields` is what keeps the two on-disk shapes
        // unambiguous: without it, a pre-#282 bare-map file (index names as
        // top-level keys) would "successfully" parse as an envelope with
        // every cursor silently ignored as an unknown field — the exact
        // accepted-and-ignored class this issue removes.
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Envelope {
            #[serde(default)]
            managed: std::collections::HashMap<String, crate::lifecycle::ManagedIndexState>,
            #[serde(default)]
            detached: Vec<String>,
        }
        let (map, detached) = match serde_json::from_slice::<Envelope>(&bytes) {
            Ok(env) => (env.managed, env.detached),
            Err(_) => match serde_json::from_slice::<
                std::collections::HashMap<String, crate::lifecycle::ManagedIndexState>,
            >(&bytes)
            {
                Ok(map) => (map, Vec::new()),
                Err(e) => {
                    warn!(error = %e, "ignoring corrupt ism_managed_indices.json");
                    return;
                }
            },
        };
        let n = map.len();
        for (index_name, state) in map {
            self.managed_indices.insert(index_name, state);
        }
        for index_name in detached {
            self.lifecycle_detached.insert(index_name, ());
        }
        if n > 0 {
            info!(count = n, "restored persisted ISM managed-index state");
        }
    }

    /// Detach `index` from lifecycle management and record a persisted
    /// tombstone for it (issue #282). This is THE detach path — `PUT
    /// /{index}/_settings {"index.lifecycle.name": null}`, `POST
    /// /{index}/_ilm/remove` and `POST /_plugins/_ism/remove/{index}` all
    /// land here, so a detach is recorded once and the executor cannot
    /// disagree with the operator about which route it honoured (#262's
    /// `set_index_lifecycle_policy` made the same call).
    ///
    /// Also scrubs `index.lifecycle.name` from the stored display settings:
    /// ES drops the setting on removal rather than reporting a name that is
    /// no longer in force, and a stale name here is exactly the input a
    /// future settings-derived re-attach would trip over.
    ///
    /// Returns whether the index was managed before the call.
    pub fn detach_lifecycle(&self, index: &str) -> Result<bool> {
        self.ensure_cluster_state_writable()?;
        let was_managed = self.managed_indices.remove(index).is_some();
        self.lifecycle_detached.insert(index.to_string(), ());
        if let Some(mut stored) = self.index_settings.get_mut(index) {
            let v = stored.value_mut();
            if let Some(obj) = v.as_object_mut() {
                obj.remove("index.lifecycle.name");
            }
            if let Some(lifecycle) = v
                .pointer_mut("/index/lifecycle")
                .and_then(Value::as_object_mut)
            {
                lifecycle.remove("name");
            }
        }
        self.persist_managed_indices()?;
        Ok(was_managed)
    }

    /// Whether `lifecycle::tick` is allowed to act — the `POST /_ilm/stop`
    /// kill switch (issue #282).
    pub fn lifecycle_execution_running(&self) -> bool {
        self.lifecycle_running
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// `POST /_ilm/start` / `POST /_ilm/stop`.
    pub fn set_lifecycle_execution_running(&self, running: bool) {
        self.lifecycle_running
            .store(running, std::sync::atomic::Ordering::Relaxed);
    }

    /// Spawn the background lifecycle-execution job: every
    /// `config.lifecycle.tick_interval_secs` (default 300s = 5 minutes,
    /// matching OpenSearch ISM's own default job interval), runs
    /// `lifecycle::tick` over every managed index. Not latency-critical
    /// (unlike the resource governor's sampler), so a plain `tokio::spawn`
    /// on an interval is fine — uses a `Weak` self-pointer so it exits
    /// cleanly when the engine is dropped (matters for tests, which create
    /// and drop many `Engine`s).
    pub fn spawn_lifecycle_manager(self: &Arc<Self>) {
        let interval =
            std::time::Duration::from_secs(self.config.lifecycle.tick_interval_secs.max(1));
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            tick.tick().await; // skip the immediate first tick
            loop {
                tick.tick().await;
                let Some(engine) = weak.upgrade() else {
                    return; // engine dropped
                };
                crate::lifecycle::tick(&engine).await;
            }
        });
    }

    // ── Index creation-date tracking ─────────────────────────────────────────

    fn index_created_at_path(&self, name: &str) -> PathBuf {
        self.data_dir.join(name).join("created_at.json")
    }

    /// Record `name`'s creation time (now) and persist it — called exactly
    /// once, at the end of `create_index_with_settings`. Without this,
    /// `min_index_age` has no ground truth, and `GET /{index}`'s
    /// `creation_date` was synthesized fresh on every single request
    /// (always reporting "now"), which is what this replaces.
    fn record_index_created_at(&self, name: &str) {
        let now = crate::lifecycle::now_ms();
        self.index_created_at.insert(name.to_string(), now);
        match serde_json::to_vec_pretty(&serde_json::json!({ "creation_date_ms": now })) {
            Ok(bytes) => {
                if let Err(e) =
                    crate::index::write_file_atomic(&self.index_created_at_path(name), &bytes)
                {
                    warn!(index = name, error = %e, "failed to persist created_at.json");
                }
            }
            Err(e) => warn!(index = name, error = %e, "failed to serialize created_at.json"),
        }
    }

    /// Load `name`'s persisted creation time on startup. An index directory
    /// that predates this feature (no `created_at.json`) gets a synthetic
    /// baseline of "now" recorded and persisted on first post-upgrade boot —
    /// its true history is unknown, so `min_index_age` starts counting from
    /// the upgrade rather than from the index's real (unrecorded) creation.
    /// Documented tradeoff, not silently wrong: every index that existed
    /// before this shipped gets exactly one synthetic reset, once.
    fn load_or_backfill_index_created_at(&self, name: &str) {
        let path = self.index_created_at_path(name);
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                if let Some(ms) = v.get("creation_date_ms").and_then(Value::as_i64) {
                    self.index_created_at.insert(name.to_string(), ms);
                    return;
                }
            }
        }
        self.record_index_created_at(name);
    }

    // ── Cluster metadata persistence (issue #203) ────────────────────────────
    //
    // Index templates, legacy (v1) templates, component templates, ingest
    // pipelines, data streams and ILM policies used to live only in the
    // in-memory maps above: `PUT /_index_template/logs` answered `200
    // {"acknowledged": true}` and the template was gone after the next boot,
    // with no error anywhere and the next matching index silently created
    // unshaped. They are now snapshotted, as one consistent document, into
    // `<data_dir>/cluster_state.json` on every mutation and reloaded in
    // `Engine::new`.
    //
    // Durability contract: a full-file rewrite through
    // `index::write_file_atomic` — write to `cluster_state.tmp`, `fsync` it,
    // `rename` over the target, then `fsync` the parent directory so the
    // rename itself survives power loss. The rename is the commit point, so
    // a `kill -9` at any instant leaves either the previous complete
    // document or the new one, never a half-written mix. The same shape peer
    // engines use for their manifests (cf. `fjall/src/file.rs:17`
    // `fsync_directory`, which fjall calls after every atomic rewrite;
    // `sled/src/metadata_store.rs:696` discards leftover `*.tmp` files from
    // an interrupted rewrite on the next boot, which `apply_cluster_state`
    // mirrors below).
    //
    // One file rather than six keeps the maps mutually consistent — a data
    // stream and the template that shaped it can never be restored from
    // different generations.

    /// Path of the persisted cluster-metadata document
    /// (`<data_dir>/cluster_state.json`).
    fn cluster_state_path(&self) -> PathBuf {
        self.data_dir.join("cluster_state.json")
    }

    /// `Err` when boot did not classify `cluster_state.json` as absent or as
    /// the exact supported format 1.
    ///
    /// Every management mutation calls this, not only the ones that reach
    /// `flush_cluster_state` with something to write. A `DELETE` consults the
    /// in-memory map first, and while the load has failed that map is empty,
    /// so an unguarded delete answers `404 not found` about an object that is
    /// sitting in the document on disk — the same lie in a different shape.
    /// A typed 503 is the only honest answer while the node cannot see the
    /// operator's configuration.
    pub fn ensure_cluster_state_writable(&self) -> Result<()> {
        if self.cluster_state_boot.is_writable() {
            return Ok(());
        }
        let path = self.cluster_state_path();
        let detail = self
            .cluster_state_boot
            .detail()
            .unwrap_or("cluster state was blocked at boot");
        error!(
            path = %path.display(),
            detail,
            "refusing a cluster-metadata write because the boot-time format fence is latched"
        );
        Err(EngineError::Common(
            xerj_common::XerjError::cluster_state_unavailable(),
        ))
    }

    /// Snapshot every persisted management map into one document.
    fn cluster_state_snapshot(&self) -> PersistedClusterStateV1 {
        fn dump<V: Clone>(map: &DashMap<String, V>) -> std::collections::BTreeMap<String, V> {
            map.iter()
                .map(|e| (e.key().clone(), e.value().clone()))
                .collect()
        }
        PersistedClusterStateV1 {
            version: CLUSTER_STATE_V1,
            index_templates: self
                .templates
                .iter()
                .map(|entry| {
                    (
                        entry.key().clone(),
                        PersistedIndexTemplateV1::from(entry.value().clone()),
                    )
                })
                .collect(),
            legacy_templates: dump(&self.legacy_templates),
            component_templates: dump(&self.component_templates),
            pipelines: dump(&self.pipelines),
            data_streams: self
                .data_streams
                .iter()
                .map(|entry| {
                    (
                        entry.key().clone(),
                        PersistedDataStreamV1::from(entry.value().clone()),
                    )
                })
                .collect(),
            ilm_policies: dump(&self.ilm_policies),
        }
    }

    /// Durably write the current cluster metadata.
    ///
    /// Unlike `flush_aliases`, a failure here is **returned**, not swallowed:
    /// every caller is an API write that would otherwise answer
    /// `{"acknowledged": true}` for a change it cannot keep. Callers roll the
    /// in-memory change back and surface a 500, so the operator finds out at
    /// the moment of the write instead of at the next restart.
    ///
    /// Refuses outright when boot could not load the existing document — see
    /// the boot-time cluster-state fence. The snapshot below is taken from the live maps,
    /// which in that state hold only what this process was told after boot, so
    /// writing it would rename an empty-ish document over configuration that is
    /// still intact on disk.
    fn flush_cluster_state(&self) -> Result<()> {
        self.ensure_cluster_state_writable()?;
        // One writer at a time — see `cluster_state_write`. Snapshotting
        // inside the lock also means the bytes that land are never older
        // than a rewrite that has already returned.
        let _writing = self.cluster_state_write.lock();
        // The final writer owns the fence too. A future caller that misses a
        // preguard still cannot normalize an unsupported document.
        self.ensure_cluster_state_writable()?;
        let snapshot = self.cluster_state_snapshot();
        let bytes = serde_json::to_vec_pretty(&snapshot)?;
        crate::index::write_file_atomic(&self.cluster_state_path(), &bytes)?;
        Ok(())
    }

    /// Highest generation `N` for which a backing index `.ds-<stream>-<N>` is
    /// actually open in this data dir; `0` when there is none.
    ///
    /// Read off the open indices rather than the stream document, because the
    /// point is to catch the case where the two disagree. Suffix parsing keeps
    /// neighbouring streams apart on its own: for stream `a`, `.ds-a-b-000001`
    /// leaves `b-000001`, which is not a number.
    fn highest_backing_generation(&self, stream: &str) -> u64 {
        let prefix = format!(".ds-{stream}-");
        self.indices
            .iter()
            .filter_map(|e| {
                e.key()
                    .strip_prefix(&prefix)
                    .and_then(|gen| gen.parse::<u64>().ok())
            })
            .max()
            .unwrap_or(0)
    }

    /// Apply the exact format-1 document already read and validated before
    /// index discovery. `None` means either a fresh node or a blocked boot;
    /// the immutable boot status distinguishes those cases.
    fn apply_cluster_state(&self, state: Option<PersistedClusterStateV1>) {
        let Some(state) = state else {
            return;
        };

        for (name, tmpl) in state.index_templates {
            self.templates.insert(name, tmpl.into());
        }
        for (name, body) in state.legacy_templates {
            self.legacy_templates.insert(name, body);
        }
        for (name, body) in state.component_templates {
            self.component_templates.insert(name, body);
        }
        for (name, body) in state.ilm_policies {
            self.ilm_policies.insert(name, body);
        }

        // Data streams: restore the record, reconcile it against what is
        // actually on disk, then say plainly when a backing index it names is
        // not there. Reporting a stream whose data is gone as
        // `"status": "GREEN"` without a word in the log is the same quiet lie
        // this issue is about.
        let mut reconciled_a_stream = false;
        for (name, persisted) in state.data_streams {
            let mut ds: DataStream = persisted.into();
            // `rollover_data_stream` creates the new backing index before it
            // records the new generation, so `kill -9` in that window leaves
            // `.ds-<name>-00000N` on disk while the document still says N-1.
            // Restoring N-1 verbatim would wedge the stream permanently: the
            // next rollover computes the same name, and `create_index`
            // refuses it as `index_already_exists` — forever. Adopt the
            // highest generation that actually exists instead, which is also
            // what stops a second rollover writing into an index that already
            // holds the first one's documents.
            let on_disk = self.highest_backing_generation(&name);
            if on_disk > ds.generation {
                let adopted = format!(".ds-{name}-{on_disk:06}");
                warn!(
                    data_stream = name.as_str(),
                    recorded = ds.generation,
                    found = on_disk,
                    adopted = adopted.as_str(),
                    "adopting a backing index left behind by a rollover that \
                     was interrupted before its generation reached disk"
                );
                if !ds.backing_indices.contains(&adopted) {
                    ds.backing_indices.push(adopted);
                }
                ds.generation = on_disk;
                reconciled_a_stream = true;
            }

            let missing: Vec<&str> = ds
                .backing_indices
                .iter()
                .filter(|b| !self.indices.contains_key(b.as_str()))
                .map(|b| b.as_str())
                .collect();
            if !missing.is_empty() {
                warn!(
                    data_stream = name.as_str(),
                    missing = missing.join(","),
                    "restored data stream references backing indices that are \
                     not present in the data dir"
                );
            }
            self.data_streams.insert(name, ds);
        }

        // Pipelines carry behaviour, not just a document to hand back from
        // `GET /_ingest/pipeline`: the executable form has to be rebuilt or
        // `?pipeline=x` would be accepted after a restart and quietly do
        // nothing. Keep the stored document either way so GET round-trips
        // exactly as it did before the restart.
        //
        // Which strictness a definition is replayed at is decided by
        // PROVENANCE, never by the calendar (issue #204). The markers are
        // already loaded — `Engine::new` does it before this runs, so the
        // no-`cluster_state.json` path sees them too — and loading is
        // idempotent, so this call keeps every other caller of
        // `apply_cluster_state` correct on its own.
        self.load_persisted_pipeline_refusals();
        let provenance_unknown = self
            .pipeline_refusals_unreadable
            .load(std::sync::atomic::Ordering::SeqCst);
        let mut pipelines_restored = 0usize;
        for (name, config) in state.pipelines {
            self.pipelines.insert(name.clone(), config.clone());
            let refused_at_definition_time = self
                .definition_time_refusals
                .get(&name)
                .map(|r| r.value().clone());
            // Replay strictness is for definitions written by a build with NO
            // gate: those were accepted by whichever build wrote them and
            // failing them at boot would flip a working cluster's writes from
            // 201 to 400 on a binary upgrade alone. A definition THIS contract
            // already refused is the opposite case — the operator asked, was
            // told 400, and a restart must not answer differently. Softening
            // it here dropped `secret` from documents an `if` guard excluded,
            // under a 201, with no PUT in between.
            let mode = if refused_at_definition_time.is_some() || provenance_unknown {
                xerj_wasm::pipeline::CompileMode::Strict
            } else {
                xerj_wasm::pipeline::CompileMode::ReplayPersisted
            };
            match self.compile_pipeline_with_mode(&name, config, mode) {
                Ok(()) => {
                    if refused_at_definition_time.is_some() {
                        // This build honours what the refusing one could not,
                        // so the marker has outlived its cause. Clearing it is
                        // what stops a capability upgrade needing a re-PUT.
                        info!(
                            pipeline = name.as_str(),
                            "a pipeline refused at definition time by an earlier build compiles \
                             cleanly here; the recorded refusal is cleared and it now runs"
                        );
                        self.definition_time_refusals.remove(&name);
                        self.flush_pipeline_refusals_best_effort();
                    }
                    pipelines_restored += 1
                }
                Err(e) => {
                    // Issue #204: record WHY there is no executable form, so
                    // every ingest path refuses loudly with the reason
                    // instead of reporting a pipeline GET plainly shows as
                    // "not found" — the same contract as a fresh PUT.
                    let reason = match (&refused_at_definition_time, provenance_unknown) {
                        // Answer the operator with the words they already
                        // read at PUT time, so the restart is visibly a
                        // continuation rather than a new, different failure.
                        (Some(recorded), _) => recorded.clone(),
                        (None, true) => format!(
                            "the persisted definition could not be recompiled at startup ({e}), \
                             and `ingest-pipeline-refusals.json` could not be read, so this \
                             build cannot tell an already-refused definition from one an \
                             older build accepted; no document can be ingested through \
                             pipeline [{name}] — repair or delete that file, or re-PUT the \
                             definition"
                        ),
                        (None, false) => format!(
                            "the persisted definition could not be recompiled \
                             at startup ({e}); no document can be ingested \
                             through pipeline [{name}] — re-PUT the definition"
                        ),
                    };
                    self.unrunnable_pipelines.insert(name.clone(), reason);
                    error!(
                        pipeline = name.as_str(), error = %e,
                        refused_at_definition_time = refused_at_definition_time.is_some(),
                        "persisted ingest pipeline could not be recompiled — it is \
                         still visible to GET /_ingest/pipeline but will NOT \
                         transform anything; re-PUT the definition"
                    )
                }
            }
        }

        // Make an adopted generation durable straight away, so the recovery
        // happens once rather than on every boot from here on.
        if reconciled_a_stream {
            if let Err(e) = self.flush_cluster_state() {
                warn!(
                    error = %e,
                    "could not persist the reconciled data-stream generation; \
                     it will be recomputed from disk on the next boot"
                );
            }
        }

        let n = self.templates.len()
            + self.legacy_templates.len()
            + self.component_templates.len()
            + self.pipelines.len()
            + self.data_streams.len()
            + self.ilm_policies.len();
        if n > 0 {
            info!(
                index_templates = self.templates.len(),
                legacy_templates = self.legacy_templates.len(),
                component_templates = self.component_templates.len(),
                pipelines = pipelines_restored,
                data_streams = self.data_streams.len(),
                ilm_policies = self.ilm_policies.len(),
                "restored persisted cluster metadata"
            );
        }
    }

    /// Name every `.ds-*` index on disk that no restored data stream claims.
    ///
    /// This does **not** reconcile anything — nothing is deleted, adopted or
    /// repaired, because an orphan may hold the only copy of somebody's data.
    /// It exists so the state is not *silent*: an orphaned backing index is
    /// unreachable through the data-stream API (`GET` and `DELETE` on the
    /// stream answer 404, while `PUT /_data_stream/<name>` answers
    /// `409 resource_already_exists_exception` naming the orphan), so without
    /// a line in the boot log the operator has no way to learn the name they
    /// have to pass to `DELETE /<backing-index>` to clear it.
    ///
    /// Expected sources: a data dir written by a build that predates
    /// `cluster_state.json` (data streams were not persisted at all then), or
    /// a `DELETE /_data_stream` interrupted by a build that recorded the
    /// removal before destroying the backing indices. Skipped entirely when
    /// the cluster state did not load — then every stream is unknown and
    /// every backing index would look orphaned.
    fn warn_orphaned_backing_indices(&self) {
        if !self.cluster_state_boot.is_writable() {
            return;
        }
        // Snapshot both maps before comparing them: nothing here may hold a
        // shard guard on one map while taking one on the other.
        let backing: Vec<String> = self
            .indices
            .iter()
            .map(|e| e.key().clone())
            .filter(|n| n.starts_with(".ds-"))
            .collect();
        if backing.is_empty() {
            return;
        }
        let streams: Vec<(String, Vec<String>)> = self
            .data_streams
            .iter()
            .map(|s| (s.key().clone(), s.value().backing_indices.clone()))
            .collect();
        let claimed = |index: &str| {
            streams.iter().any(|(stream, indices)| {
                indices.iter().any(|b| b == index)
                    || index
                        .strip_prefix(&format!(".ds-{stream}-"))
                        .is_some_and(|gen| gen.parse::<u64>().is_ok())
            })
        };
        let mut orphans: Vec<String> = backing.into_iter().filter(|n| !claimed(n)).collect();
        if orphans.is_empty() {
            return;
        }
        orphans.sort();
        warn!(
            count = orphans.len(),
            indices = orphans.join(","),
            "backing indices on disk belong to no known data stream — they are \
             unreachable through the data-stream API and are NOT deleted \
             automatically; remove each with DELETE /<index> once you have \
             confirmed the data is not wanted"
        );
    }

    /// Insert into a persisted map and durably record the result, rolling the
    /// in-memory change back if the write fails so a 500 never leaves a
    /// change that only this process can see.
    fn persisted_insert<V: Clone>(
        &self,
        map: &DashMap<String, V>,
        name: String,
        value: V,
    ) -> Result<()> {
        self.ensure_cluster_state_writable()?;
        let previous = map.insert(name.clone(), value);
        if let Err(e) = self.flush_cluster_state() {
            match previous {
                Some(old) => map.insert(name, old),
                None => map.remove(&name).map(|(_, v)| v),
            };
            return Err(e);
        }
        Ok(())
    }

    /// Remove from a persisted map and durably record the result. Returns
    /// `false` when there was nothing to remove (the caller answers 404);
    /// an `Err` means the removal did not reach disk and was undone.
    fn persisted_remove<V: Clone>(&self, map: &DashMap<String, V>, name: &str) -> Result<bool> {
        // Before the map lookup: an empty map after a failed load would
        // otherwise turn "I cannot see your configuration" into "it does not
        // exist" — see `ensure_cluster_state_writable`.
        self.ensure_cluster_state_writable()?;
        let Some((key, removed)) = map.remove(name) else {
            return Ok(false);
        };
        if let Err(e) = self.flush_cluster_state() {
            map.insert(key, removed);
            return Err(e);
        }
        Ok(true)
    }

    /// Create or replace a v2 index template (`PUT /_index_template/{name}`).
    pub fn put_index_template(&self, name: String, template: IndexTemplate) -> Result<()> {
        self.persisted_insert(&self.templates, name, template)
    }

    /// Delete a v2 index template; `false` when it did not exist.
    pub fn delete_index_template(&self, name: &str) -> Result<bool> {
        self.persisted_remove(&self.templates, name)
    }

    /// Create or replace a legacy v1 template (`PUT /_template/{name}`).
    pub fn put_legacy_template(&self, name: String, body: Value) -> Result<()> {
        self.persisted_insert(&self.legacy_templates, name, body)
    }

    /// Delete a legacy v1 template; `false` when it did not exist.
    pub fn delete_legacy_template(&self, name: &str) -> Result<bool> {
        self.persisted_remove(&self.legacy_templates, name)
    }

    /// Create or replace a component template.
    pub fn put_component_template(&self, name: String, body: Value) -> Result<()> {
        self.persisted_insert(&self.component_templates, name, body)
    }

    /// Delete a component template; `false` when it did not exist.
    pub fn delete_component_template(&self, name: &str) -> Result<bool> {
        self.persisted_remove(&self.component_templates, name)
    }

    /// Create or replace an ILM policy.
    pub fn put_ilm_policy(&self, name: String, body: Value) -> Result<()> {
        self.persisted_insert(&self.ilm_policies, name, body)
    }

    /// Delete an ILM policy; `false` when it did not exist.
    pub fn delete_ilm_policy(&self, name: &str) -> Result<bool> {
        self.persisted_remove(&self.ilm_policies, name)
    }

    /// Durably store an ingest pipeline definition and (re)build its
    /// executable form. This is the only way a user-visible pipeline should
    /// ever be created — `compile_pipeline` alone does not reach disk.
    ///
    /// `config` is the xerj-shaped pipeline config to compile, and is what
    /// gets stored (and handed back by `GET /_ingest/pipeline/{id}`) when it
    /// compiles — the shape this endpoint has always returned.
    ///
    /// A definition that does not compile is NOT stored: `Ok(Some(err))` is
    /// returned, nothing is flushed, and any pipeline already registered under
    /// this id is left untouched. A caller that must keep an uncompilable
    /// definition readable — the ES-compat surface does, because Elasticsearch
    /// accepts processors this build does not implement — calls
    /// [`Self::register_unrunnable_pipeline`] instead, which stores the
    /// definition *and* the reason it cannot run, and persists the refusal
    /// marker that makes that reason survive a restart.
    ///
    /// (This used to take a third `keep_if_uncompilable: Option<Value>`
    /// argument whose `Some` arm stored the definition here. Every caller in
    /// the workspace passed `None` — the ES surface routes through
    /// `register_unrunnable_pipeline` — so the arm was unreachable, untested,
    /// and held its own copy of the refusal-marker bookkeeping that the live
    /// path already does. Issue #204: a security-relevant branch with no
    /// caller is a liability, not a spare part.)
    ///
    /// A compile failure drops the *executable* form under this id. Leaving
    /// the old one behind would mean `?pipeline={id}` kept running a
    /// definition `GET` no longer shows — and stopped running it at the next
    /// restart. A definition must behave the same before and after a reboot;
    /// that is the whole point of persisting it.
    ///
    /// `Err` means the definition did not reach disk. The in-memory change is
    /// rolled back first, so the caller can report the write as failed
    /// without leaving behind a pipeline only this process can see.
    pub fn put_pipeline(&self, id: &str, config: Value) -> Result<Option<xerj_wasm::WasmError>> {
        // Up front, so a refused write never disturbs the compiled form of a
        // pipeline that is still running.
        self.ensure_cluster_state_writable()?;
        let previous_doc = self.pipelines.get(id).map(|v| v.value().clone());
        // Taken out up front so a failed compile cannot leave a stale
        // executable behind; put back verbatim on every abort path.
        let previous_compiled = self.transform_pipelines.remove(id).map(|(_, p)| p);
        // `compile_pipeline` clears this on success; snapshot it so a failed
        // flush can put the previous definition's unrunnable reason back too
        // (issue #204 — a rolled-back write must not turn "stored but not
        // runnable, and here is why" into a bare "not found").
        let previous_reason = self.unrunnable_pipelines.get(id).map(|r| r.value().clone());
        // Same for the persisted refusal marker (issue #204): a rolled-back
        // write must not silently un-refuse a definition that is still the one
        // on disk.
        let previous_marker = self
            .definition_time_refusals
            .get(id)
            .map(|r| r.value().clone());
        // Same for the boot-replay degradation note: a rolled-back write must
        // leave the previous definition described exactly as it was.
        let previous_degradation = self.degraded_pipelines.get(id).map(|r| r.value().clone());

        // `compile_pipeline` mutates nothing when it fails, so an abort only
        // has to undo the two lines above.
        if let Err(e) = self.compile_pipeline(id, config) {
            if let Some(p) = previous_compiled {
                self.transform_pipelines.insert(id.to_string(), p);
            }
            return Ok(Some(e));
        }

        // Every abort path restores exactly the state this call found, so a
        // write that did not reach disk leaves nothing only this process can
        // see. Each expansion sits in a branch that returns, so the moves out
        // of the `previous_*` snapshots never overlap.
        macro_rules! rollback {
            () => {{
                match previous_doc {
                    Some(d) => self.pipelines.insert(id.to_string(), d),
                    None => self.pipelines.remove(id).map(|(_, v)| v),
                };
                self.transform_pipelines.remove(id);
                if let Some(p) = previous_compiled {
                    self.transform_pipelines.insert(id.to_string(), p);
                }
                match previous_reason {
                    Some(r) => self.unrunnable_pipelines.insert(id.to_string(), r),
                    None => self.unrunnable_pipelines.remove(id).map(|(_, v)| v),
                };
                match previous_marker {
                    Some(r) => self.definition_time_refusals.insert(id.to_string(), r),
                    None => self.definition_time_refusals.remove(id).map(|(_, v)| v),
                };
                match previous_degradation {
                    Some(r) => self.degraded_pipelines.insert(id.to_string(), r),
                    None => self.degraded_pipelines.remove(id).map(|(_, v)| v),
                };
            }};
        }

        // Issue #204: order the two files so the crash window fails CLOSED.
        //
        // Reaching here means the definition compiled, so the only marker
        // change this function can make is a REMOVAL — `compile_pipeline`
        // clears the marker on success, and nothing here sets one (a
        // definition that does not compile returned above without storing
        // anything; the caller that keeps such a definition readable goes
        // through `register_unrunnable_pipeline`, which inserts, and therefore
        // orders the two files the other way round for the same reason).
        //
        // A removal must land AFTER the definition:
        //
        //   definition without the clear — the marker is stale, so the boot
        //     replays the NEW definition at definition strictness. It compiled
        //     here, so it compiles there, and the self-heal clears the marker
        //     and logs it. Loud at worst, and it repairs itself.
        //   clear without the definition — the marker is gone while the OLD,
        //     refused definition is still the one on disk, so the boot replays
        //     it softly and the pipeline silently starts running with the
        //     refused option ignored. That is exactly the defect this marker
        //     exists to close, reached through the rollback path.
        //
        // Which is also why clearing is best-effort rather than on the
        // acknowledge path: a failed clear only ever leaves a pipeline MORE
        // refused, and the next boot undoes it without an operator.
        if let Err(e) = self.flush_cluster_state() {
            rollback!();
            return Err(e);
        }

        let marker_changed = self
            .definition_time_refusals
            .get(id)
            .map(|r| r.value().clone())
            != previous_marker;
        if marker_changed {
            self.flush_pipeline_refusals_best_effort();
        }

        info!(name = id, "transform pipeline created");
        // `Ok(None)` is now the only success: a definition that does not
        // compile returned `Ok(Some(err))` above without storing anything.
        Ok(None)
    }

    /// Delete an ingest pipeline — both the stored definition and the
    /// compiled form, so `?pipeline={name}` stops working at the same moment
    /// `GET /_ingest/pipeline/{name}` starts answering 404. `false` when it
    /// did not exist.
    pub fn delete_pipeline(&self, name: &str) -> Result<bool> {
        let removed = self.persisted_remove(&self.pipelines, name)?;
        if removed {
            self.transform_pipelines.remove(name);
            // Issue #204: a deleted pipeline must not leave a stale
            // "stored but not runnable" reason behind either — the next
            // definition under this id starts from a clean slate.
            self.unrunnable_pipelines.remove(name);
            self.degraded_pipelines.remove(name);
            // …and neither may the persisted refusal marker outlive the
            // definition it refers to, or a later PUT of a *different*
            // definition under the same id would inherit its 400 at the next
            // boot (issue #204).
            if self.definition_time_refusals.remove(name).is_some() {
                self.flush_pipeline_refusals_best_effort();
            }
        }
        Ok(removed)
    }

    /// Add an alias pointing to an index. A blocked boot fails before the map
    /// or alias file changes.
    pub fn add_alias(&self, alias: &str, index: &str) -> Result<()> {
        self.ensure_cluster_state_writable()?;
        let mut entry = self.aliases.entry(alias.to_string()).or_default();
        if !entry.contains(&index.to_string()) {
            entry.push(index.to_string());
        }
        drop(entry);
        self.flush_aliases();
        Ok(())
    }

    /// Remove an alias's association with an index. A blocked boot fails
    /// before the map or alias file changes.
    pub fn remove_alias(&self, alias: &str, index: &str) -> Result<()> {
        self.ensure_cluster_state_writable()?;
        if let Some(mut entry) = self.aliases.get_mut(alias) {
            entry.retain(|i| i != index);
        }
        // Clean up empty alias entries.
        self.aliases.retain(|_, v| !v.is_empty());
        self.flush_aliases();
        Ok(())
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
    ///
    /// A **failed** index (present on disk, refused at open — see
    /// [`FailedIndex`]) is deletable through this same door. It used to answer
    /// 404 `index_not_found`, which left an operator with no lever but
    /// stopping the server and removing the directory by hand (issue #206).
    pub async fn delete_index(&self, name: &str) -> Result<()> {
        self.ensure_cluster_state_writable()?;
        // Per-index authorization backstop (issue #79) — "destroy the brain"
        // is the loudest door, so it is checked before the index is removed
        // from the map. See `crate::index_guard`.
        if !crate::index_guard::visible(name) {
            return Err(EngineError::Common(
                xerj_common::XerjError::index_not_found(name),
            ));
        }
        match self.indices.remove(name).map(|(_, v)| v) {
            Some(idx) => {
                // The handle is pulled out of the map first so no write can
                // land in a directory that is being removed — but if the
                // removal then fails (read-only mount, EACCES, EROFS), the
                // name has been freed while the bytes are still there. That
                // is exactly the stuck state issue #206 is about, arrived at
                // from the other side: `Engine::indices` no longer holds it,
                // `failed_indices` never did, so `_cat/indices` cannot show
                // it, `DELETE` answers 404 and none of the three recovery
                // levers this module adds can name it. Put the handle back so
                // a delete that did not happen leaves the index addressable
                // and the operator can retry it.
                if let Err(e) = idx.delete_all_data().await {
                    self.indices.insert(name.to_string(), idx);
                    warn!(name, error = %e, "index delete failed; index left in service");
                    return Err(e);
                }
            }
            None => {
                // Not open. If it is a known failed index, its bytes are still
                // on disk and removing them is exactly what the operator asked
                // for; anything else is a genuine 404.
                if !self.failed_indices.contains_key(name) {
                    return Err(EngineError::Common(
                        xerj_common::XerjError::index_not_found(name),
                    ));
                }
                // Drop the bookkeeping only after the bytes are gone. Removing
                // it first would take a delete that failed (read-only mount,
                // fs error) out of the failed list while its directory
                // survives — the operator would believe the name was freed and
                // the index would reappear on the next boot.
                self.remove_index_dir(name)?;
                self.failed_indices.remove(name);
            }
        }

        self.forget_index_metadata(name);
        info!(name, "index deleted");
        Ok(())
    }

    /// Drop every piece of engine-side metadata that names `index` — aliases
    /// that pointed only at it, the closed flag, settings, mappings, alias
    /// metadata. Shared by the open-index and failed-index delete paths so the
    /// two cannot drift.
    fn forget_index_metadata(&self, name: &str) {
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
        self.flush_aliases();

        self.closed_indices.remove(name);
        self.index_settings.remove(name);
        self.index_mappings.remove(name);
        self.index_alias_metadata.remove(name);
        self.flush_alias_metadata();

        // The WAL tap's cursor is a byte offset into a WAL stream that has
        // just ceased to exist. Dropping it HERE, rather than waiting for a
        // poll to notice the index is gone, is what makes delete-and-recreate
        // inside one poll interval safe — see `wal_tap::WalTap::forget_index`.
        self.wal_tap.forget_index(name);

        // Lifecycle bookkeeping: a deleted index needs neither an execution
        // cursor nor a detach tombstone (#282) — and clearing the tombstone
        // here means a *recreated* index with the same name starts fresh
        // rather than inheriting a years-old "operator said stop".
        let had_lifecycle_state = self.managed_indices.remove(name).is_some()
            | self.lifecycle_detached.remove(name).is_some();
        if had_lifecycle_state {
            // `delete_index` checked the compatibility fence before reaching
            // this private cleanup path.
            let _ = self.persist_managed_indices();
        }
    }

    /// Remove `<data_dir>/<name>` from disk, refusing anything that does not
    /// resolve inside `data_dir`.
    ///
    /// The open-index path deletes through `Index::delete_all_data`, which can
    /// only ever point at a directory the engine itself built. The failed-index
    /// path has no `Index` to ask, so the name is re-validated here and the
    /// resolved path is proven to be under `data_dir` before anything is
    /// removed — the same canonicalisation rule the snapshot-restore path
    /// applies before it writes.
    fn remove_index_dir(&self, name: &str) -> Result<()> {
        // Reject traversal/absolute forms up front: only a legal index name
        // can name a directory we own.
        IndexName::new(name).map_err(EngineError::Common)?;
        let dir = self.data_dir.join(name);
        if !dir.exists() {
            return Ok(());
        }
        let dir_canon = dir.canonicalize().map_err(EngineError::Io)?;
        let root_canon = self.data_dir.canonicalize().map_err(EngineError::Io)?;
        if !dir_canon.starts_with(&root_canon) {
            return Err(EngineError::Common(xerj_common::XerjError::storage(
                format!("refusing to delete index [{name}] outside data_dir (canonical)"),
            )));
        }
        std::fs::remove_dir_all(&dir_canon).map_err(EngineError::Io)?;
        Ok(())
    }

    // ── Failed-index recovery (issue #206) ────────────────────────────────────

    /// Record (or re-record) an index that could not be opened.
    ///
    /// Preserves `failed_at_ms` across repeated failures so "since when" stays
    /// truthful, and counts retries so a flapping directory is visible as
    /// such.
    fn record_failed_index(&self, name: &str, reason: String) {
        match self.failed_indices.get_mut(name) {
            Some(mut existing) => {
                existing.reason = reason;
                existing.retries = existing.retries.saturating_add(1);
            }
            None => {
                self.failed_indices.insert(
                    name.to_string(),
                    FailedIndex {
                        name: name.to_string(),
                        reason,
                        failed_at_ms: now_millis(),
                        retries: 0,
                    },
                );
            }
        }
    }

    /// Every failed index the caller is allowed to see, sorted by name.
    pub fn list_failed_indices(&self) -> Vec<FailedIndex> {
        let mut out: Vec<FailedIndex> = self
            .failed_indices
            .iter()
            .filter(|e| crate::index_guard::visible(e.key()))
            .map(|e| e.value().clone())
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Re-attempt the open of a failed index.
    ///
    /// On success the index becomes a normal, serving index (mapping and date
    /// flags restored exactly as at boot) and leaves the failed set. On failure
    /// the recorded reason is refreshed, the retry counter advances, and the
    /// new error is returned — the operator gets the *current* reason, not the
    /// one from boot.
    pub fn retry_failed_index(&self, name: &str) -> Result<()> {
        self.ensure_cluster_state_writable()?;
        if !crate::index_guard::visible(name) || !self.failed_indices.contains_key(name) {
            return Err(EngineError::Common(
                xerj_common::XerjError::index_not_found(name),
            ));
        }
        let index_name = IndexName::new(name).map_err(EngineError::Common)?;
        match Index::open(index_name, &self.config, &self.data_dir) {
            Ok(idx) => {
                // #320 — a reopened index gets the tap's live retention floor,
                // not the boot file's.
                idx.set_wal_min_retained_generations(self.wal_retention_floor());
                // The store opened, but the full-fidelity mapping blob is part
                // of what makes this index serveable (#202). Putting the index
                // back with a silently reduced mapping is exactly the defect
                // the open refusal exists to prevent, so a retry that cannot
                // read `es_mapping.json` stays failed and says why.
                if let Err(e) = self.load_persisted_es_mapping(name) {
                    let reason = e.to_string();
                    self.record_failed_index(name, reason.clone());
                    warn!(name, error = %reason, "retry of failed index did not succeed");
                    return Err(EngineError::Common(
                        xerj_common::XerjError::index_unavailable(name, reason),
                    ));
                }
                if let Some(m) = self.index_mappings.get(name) {
                    Engine::apply_date_mapping_flags(&idx, m.value());
                }
                self.indices.insert(name.to_string(), idx);
                self.failed_indices.remove(name);
                info!(name, "failed index reopened");
                Ok(())
            }
            Err(e) => {
                let reason = e.to_string();
                self.record_failed_index(name, reason.clone());
                warn!(name, error = %reason, "retry of failed index did not succeed");
                Err(EngineError::Common(
                    xerj_common::XerjError::index_unavailable(name, reason),
                ))
            }
        }
    }

    /// Whether the concrete index `name` is currently held in memory (its
    /// `Arc<Index>` — memtable, per-segment caches, hydration budget — is live
    /// in `self.indices`). A `_close` that actually reclaims RAM drops the handle
    /// here (#463); a still-loaded name after close means the close was a no-op.
    /// Not alias-resolving — callers pass a concrete index name.
    pub fn is_index_loaded(&self, name: &str) -> bool {
        self.indices.contains_key(name)
    }

    /// `_close` that actually reclaims RAM (#463). Marks the index unqueryable
    /// AND releases its in-memory handle — the memtable, the per-segment caches,
    /// and the hydration budget deallocate once no in-flight request holds a
    /// clone of the `Arc<Index>`. The on-disk bytes remain untouched, so
    /// [`reopen_index`](Self::reopen_index) reconstructs the index losslessly.
    ///
    /// The memtable is flushed to disk FIRST: dropping the handle without
    /// flushing would discard every not-yet-published document — turning a memory
    /// win into silent data loss. A flush failure aborts the close and leaves the
    /// index fully in service.
    pub async fn close_index(&self, name: &str) -> Result<()> {
        self.ensure_cluster_state_writable()?;
        if let Some(idx) = self.indices.get(name).map(|r| Arc::clone(r.value())) {
            // Durability barrier: flush the memtable to disk BEFORE releasing the
            // caches, so no not-yet-published document is lost. A flush failure
            // aborts the close and leaves the index fully in service.
            idx.flush().await?;
            // Free the rebuildable in-memory caches (memtable is now empty; the
            // per-segment caches re-hydrate from disk on the next read). The
            // `Arc<Index>` shell STAYS in `self.indices`, so every loaded-index
            // view (`_cat/indices`, `_cluster/health`, `_cluster/state`, the
            // `expand_wildcards` filtering) is unchanged — a closed index is
            // loaded-and-flagged, matching the rest of the codebase's model.
            idx.release_memory();
        }
        self.closed_indices.insert(name.to_string(), true);
        info!(name, "index closed; in-memory caches released");
        Ok(())
    }

    /// `_open` counterpart to [`close_index`](Self::close_index): clears the
    /// closed flag. The index handle was never removed (only its caches were
    /// released), so there is nothing to reconstruct — subsequent reads
    /// re-hydrate segments from disk on demand.
    pub fn reopen_index(&self, name: &str) -> Result<()> {
        self.ensure_cluster_state_writable()?;
        self.closed_indices.remove(name);
        info!(name, "index reopened");
        Ok(())
    }

    /// Get a reference to an index by name, resolving aliases first.
    /// If the name is an alias pointing to multiple indices, returns the first one.
    ///
    /// This is the single funnel every handler goes through to reach index
    /// data, whether it read the name from the URL path, from a `_bulk` action
    /// line, from an `_mget` `docs[]._index`, from a `terms` lookup or from the
    /// table name inside an `_sql` statement — which is exactly why the
    /// per-index authorization backstop is here (issue #79, see
    /// [`crate::index_guard`]). The check runs **after** alias resolution, on
    /// the concrete name, so pointing an alias at someone else's index cannot
    /// launder access to it.
    pub fn get_index(&self, name: &str) -> Result<Arc<Index>> {
        // A blocked boot deliberately did not open any index or replay any
        // WAL. Report that unavailable state instead of turning every real
        // on-disk index into a misleading 404.
        if !self.cluster_state_boot.is_writable() {
            return Err(EngineError::Common(
                xerj_common::XerjError::cluster_state_unavailable(),
            ));
        }
        // Check if name is an alias — if so, resolve to the first backing index.
        if let Some(aliased) = self.aliases.get(name) {
            if let Some(real_name) = aliased.first() {
                if !crate::index_guard::visible(real_name) {
                    return Err(EngineError::Common(
                        xerj_common::XerjError::index_not_found(name),
                    ));
                }
                return self
                    .indices
                    .get(real_name.as_str())
                    .map(|r| Arc::clone(r.value()))
                    .ok_or_else(|| EngineError::Common(self.missing_index_error(real_name)));
            }
        }
        if !crate::index_guard::visible(name) {
            return Err(EngineError::Common(
                xerj_common::XerjError::index_not_found(name),
            ));
        }
        self.indices
            .get(name)
            .map(|r| Arc::clone(r.value()))
            .ok_or_else(|| EngineError::Common(self.missing_index_error(name)))
    }

    /// The error for a name that is not in `indices`.
    ///
    /// A name that failed to open is **not** "not found": the directory is
    /// still there and the operator has a lever. Reporting a 404 for it is how
    /// issue #206's stuck state stayed invisible, so a failed index answers
    /// 503 `no_shard_available_action_exception` carrying the open error and
    /// the three commands that act on it.
    fn missing_index_error(&self, name: &str) -> xerj_common::XerjError {
        match self.failed_indices.get(name) {
            Some(f) => xerj_common::XerjError::index_unavailable(name, f.reason.clone()),
            None => xerj_common::XerjError::index_not_found(name),
        }
    }

    /// Return an index by name, creating it if it doesn't exist (ES behaviour).
    ///
    /// The visibility check is repeated here rather than left to
    /// [`Engine::get_index`]: that call reports "not found" for a denied name,
    /// and "not found" is precisely the branch this function answers by
    /// **creating** the index. Without its own check, auto-create would be the
    /// bypass (`create_index` catches it too — belt and braces on the write
    /// door).
    pub fn get_or_create_index(&self, name: &str) -> Result<Arc<Index>> {
        if !crate::index_guard::visible(name) {
            return Err(EngineError::Common(
                xerj_common::XerjError::index_not_found(name),
            ));
        }
        if let Ok(idx) = self.get_index(name) {
            return Ok(idx);
        }
        // A failed index already occupies this name and its bytes are still on
        // disk. Auto-creating over it would either destroy recoverable data or
        // fail deep inside the store with an opaque message — refuse loudly
        // with the open reason and the operator's options instead (issue #206).
        if let Some(f) = self.failed_indices.get(name) {
            return Err(EngineError::Common(
                xerj_common::XerjError::index_unavailable(name, f.reason.clone()),
            ));
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
    /// Filtered to the current request's visible set (issue #79) — this and
    /// [`Engine::list_indices`] are where `*`, `_all` and `logs-*` are turned
    /// into concrete names, so filtering here is what makes a wildcard resolve
    /// to "the indices you may see" instead of having to be refused outright.
    /// Unfiltered outside a request; see [`crate::index_guard`].
    pub fn index_name_list(&self) -> Vec<String> {
        self.indices
            .iter()
            .map(|e| e.key().clone())
            .filter(|n| crate::index_guard::visible(n))
            .collect()
    }

    /// The WAL-consumer retention floor the tap is currently configured with.
    ///
    /// Read from the tap, never from `self.config`: the tap's copy is the one
    /// an operator can change at runtime, and after a restart it is the one
    /// that was persisted. `self.config` is the boot file, frozen in an `Arc`.
    fn wal_retention_floor(&self) -> u64 {
        self.wal_tap.config().min_retained_generations
    }

    /// Push the tap's live retention floor onto every open index's WAL shards.
    ///
    /// Returns the number of indices it reached, so the `PUT /_xerj/wal_tap`
    /// handler can report what actually happened instead of guessing.
    ///
    /// #320. A floor set through the API previously reached nothing: it lived
    /// in the tap's own `RwLock` and in the state file, while every
    /// `WalWriter` had been seeded from `Engine.config` — an `Arc<Config>`
    /// written once at boot from `xerj.toml` and never mutated. The API
    /// answered `200` with the new value and a warning that it would apply
    /// after a restart; it did not apply after a restart either. This is the
    /// half that makes it true for indices that are already open.
    ///
    /// Re-applying a policy to already-live state on change, rather than only
    /// at construction, is Lucene's `IndexFileDeleter.revisitPolicy`
    /// (lucene/core/src/java/org/apache/lucene/index/IndexFileDeleter.java:516-543,
    /// Apache-2.0), which re-runs `policy.onCommit(commits)` over the commits
    /// that already exist rather than waiting for the next one.
    pub fn apply_wal_retention_floor(&self) -> usize {
        let floor = self.wal_retention_floor();
        // Snapshot the handles first, then take the WAL mutexes — never both
        // at once. `DashMap::iter` holds a read guard on each map shard for as
        // long as the iterator lives, and `set_wal_min_retained_generations`
        // locks every `WalWriter` of the index; holding the first while
        // acquiring the second would put a lock-order edge between two
        // subsystems that otherwise have none, for the sake of one avoided
        // allocation on an operator-triggered path.
        let indices: Vec<Arc<Index>> = self.indices.iter().map(|e| e.value().clone()).collect();
        for index in &indices {
            index.set_wal_min_retained_generations(floor);
        }
        let applied = indices.len();
        tracing::debug!(
            floor,
            indices = applied,
            "WAL tap: retention floor applied to open indices"
        );
        applied
    }

    /// Sum the internal query-result cache hit/miss counters across every open
    /// index (RC4-W4 item 4). Returns `(hits, misses)`. Reconciled into the
    /// `xerj_query_cache_{hits,misses}` gauges by the `/v1/metrics` handler at
    /// scrape time — the engine owns the truth, the API layer owns Prometheus.
    pub fn query_cache_totals(&self) -> (u64, u64) {
        let mut hits = 0u64;
        let mut misses = 0u64;
        for entry in self.indices.iter() {
            hits = hits.saturating_add(entry.value().query_cache_hit_count());
            misses = misses.saturating_add(entry.value().query_cache_miss_count());
        }
        (hits, misses)
    }

    /// Filtered to the current request's visible set — see
    /// [`Engine::index_name_list`] and [`crate::index_guard`]. Every metadata
    /// listing (`_cat/indices`, `_mapping`, `_alias`, `_resolve/index`,
    /// `/v1/dashboard/summary`, cluster health) enumerates through here, so a
    /// scoped credential gets a filtered view of the cluster rather than a
    /// blanket refusal.
    pub async fn list_indices(&self) -> Vec<IndexInfo> {
        let mut list = Vec::new();
        for entry in self.indices.iter() {
            if !crate::index_guard::visible(entry.key()) {
                continue;
            }
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
        // Before `create_index` below, not just before the flush: refusing
        // after the backing index exists would leave a `.ds-*` directory with
        // no stream in front of it.
        self.ensure_cluster_state_writable()?;
        if self.data_streams.contains_key(name) {
            return Err(EngineError::Common(
                xerj_common::XerjError::index_already_exists(name),
            ));
        }
        let backing_name = format!(".ds-{}-000001", name);
        self.create_index(&backing_name, Schema::empty())?;
        // Alias: writing to the stream name → first backing index.
        self.add_alias(name, &backing_name)?;
        let ds = DataStream {
            name: name.to_string(),
            backing_indices: vec![backing_name],
            timestamp_field: "@timestamp".to_string(),
            generation: 1,
        };
        // Durable before the caller is told it worked (issue #203): the
        // backing index is already on disk, so a stream record that lives
        // only in memory would come back after a restart as an orphaned
        // `.ds-*` index with no stream in front of it.
        self.persisted_insert(&self.data_streams, name.to_string(), ds)?;
        info!(name, "data stream created");
        Ok(())
    }

    /// Drop a deleted backing index from its data stream's generation list
    /// so the stream does not keep advertising an index that no longer
    /// exists (#282, ported from #262's `detach_data_stream_backing_index`)
    /// — and persist the shrunken list, or a restart would reload the
    /// deleted generation from `cluster_state.json` as an orphan.
    pub(crate) fn detach_data_stream_backing_index(&self, index: &str) -> Result<()> {
        // This helper is crate-visible because lifecycle deletion calls it
        // after removing the physical backing index. Keep the compatibility
        // fence here as well: a future caller must not be able to edit the
        // live stream map merely because it bypassed the lifecycle entry
        // point's guard.
        self.ensure_cluster_state_writable()?;
        let mut changed = false;
        for mut entry in self.data_streams.iter_mut() {
            let before = entry.backing_indices.len();
            entry.backing_indices.retain(|b| b != index);
            changed |= entry.backing_indices.len() != before;
        }
        if changed {
            self.flush_cluster_state()?;
        }
        Ok(())
    }

    /// Roll over a data stream: create the next backing index and update the alias.
    pub fn rollover_data_stream(&self, name: &str) -> Result<String> {
        // Same reason as `create_data_stream`, plus: the generation counter is
        // the one value that must never be handed out twice, so a rollover
        // that cannot record it must not create its backing index at all.
        self.ensure_cluster_state_writable()?;
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
            drop(entry);
            self.flush_aliases();
        } else {
            self.add_alias(name, &new_backing)?;
        }

        if let Some(mut ds) = self.data_streams.get_mut(name) {
            ds.backing_indices.push(new_backing.clone());
        }
        // The generation counter is the thing that must not be lost: an
        // unpersisted rollover would hand `.ds-<name>-000002` out twice after
        // a restart and write into the previous generation's index.
        self.flush_cluster_state()?;

        info!(
            name,
            new_backing = new_backing.as_str(),
            "data stream rolled over"
        );
        Ok(new_backing)
    }

    /// Delete a data stream and all its backing indices.
    pub async fn delete_data_stream(&self, name: &str) -> Result<()> {
        // Before the map lookup, so an unloaded state cannot answer
        // `index_not_found` for a stream that is recorded on disk — and, more
        // importantly, cannot go on to destroy its backing indices.
        self.ensure_cluster_state_writable()?;
        let ds = self
            .data_streams
            .remove(name)
            .map(|(_, v)| v)
            .ok_or_else(|| EngineError::Common(xerj_common::XerjError::index_not_found(name)))?;

        // Destroy the backing indices FIRST, and only then record the removal.
        // The two orderings fail differently and only one of them is
        // recoverable:
        //
        //   destroy → record (this one): a crash in the window leaves
        //   `cluster_state.json` still describing the stream while some of its
        //   `.ds-*` directories are already gone. The next boot restores the
        //   stream and warns `restored data stream references backing indices
        //   that are not present in the data dir` (see `apply_cluster_state`),
        //   `GET /_data_stream/<name>` still answers, and re-issuing the
        //   DELETE finishes the job.
        //
        //   record → destroy: a crash in the window strands
        //   `.ds-<name>-00000N` directories that no data-stream API can reach
        //   — GET and DELETE answer 404 while `PUT /_data_stream/<name>`
        //   answers `409 resource_already_exists_exception` forever, and boot
        //   does not reconcile it. That is the same permanent wedge this PR
        //   fixes for an interrupted rollover, so it is not the ordering to
        //   ship.
        //
        // A backing index that cannot be destroyed therefore fails the
        // delete: the stream is put back, the removal is never recorded, and
        // the caller gets the error rather than being told `acknowledged`
        // about data that is still on disk.
        //
        // Note what "fails" does *not* mean. The loop does not stop at the
        // first error — every backing index that can be destroyed is
        // destroyed, and only the ones that could not are still there. The
        // 500 says the delete did not finish, not that nothing happened; the
        // caller asked for the whole stream to go, so making the progress we
        // can is the useful half. A retry is safe and completes the job — an
        // already-deleted backing index is simply absent from `self.indices`
        // on the next pass.
        let mut delete_err: Option<EngineError> = None;
        for backing in &ds.backing_indices {
            let Some((_, idx)) = self.indices.remove(backing) else {
                // Not open: nothing left to destroy under this name (a
                // previous attempt already removed it).
                continue;
            };
            match idx.delete_all_data().await {
                Ok(()) => {}
                // `remove_dir_all` reports an already-absent directory as
                // NotFound, which is the outcome we were asking for.
                Err(EngineError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    warn!(
                        data_stream = name,
                        backing = backing.as_str(), error = %e,
                        "backing index could not be deleted — the data stream \
                         is left in place so the DELETE can be retried"
                    );
                    // Put it back so a retry can try again, and so the stream
                    // we are about to restore does not point at an index the
                    // engine has forgotten how to reach.
                    self.indices.insert(backing.clone(), idx);
                    if delete_err.is_none() {
                        delete_err = Some(e);
                    }
                }
            }
        }
        if let Some(e) = delete_err {
            self.data_streams.insert(name.to_string(), ds);
            return Err(e);
        }

        // Only now is the removal safe to make durable. A write failure here
        // means the data is gone but the document still names the stream —
        // the recoverable half of the window above, so put the stream back in
        // memory as well, keeping memory and disk saying the same thing, and
        // report the error instead of acknowledging.
        if let Err(e) = self.flush_cluster_state() {
            self.data_streams.insert(name.to_string(), ds);
            return Err(e);
        }

        // Remove the alias. Last, so a failure anywhere above leaves the
        // stream fully addressable for the retry.
        self.aliases.remove(name);
        self.flush_aliases();

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

    /// Immutable boot classification used by readiness and diagnostics.
    pub fn cluster_state_boot_status(&self) -> &ClusterStateBootStatus {
        &self.cluster_state_boot
    }

    /// Return the effective embedding identity without exposing configuration
    /// secrets or local asset paths.
    pub fn embedding_execution_identity(&self) -> Result<EmbeddingExecutionIdentity> {
        crate::index::embedding_execution_identity(&self.config.embedding)
    }

    /// Sum of every open index's live memtable footprint, in bytes. This is
    /// the quantity the process-wide memtable budget (item 1) is checked
    /// against — per-index back-pressure only ever sees one index's slice.
    pub fn total_memtable_bytes(&self) -> u64 {
        self.indices
            .iter()
            .map(|e| e.value().memtable_bytes() as u64)
            .sum()
    }

    /// Spawn the background resource sampler (item 1/3): every
    /// [`crate::governor::SAMPLE_INTERVAL_MS`] it refreshes the governor's
    /// summed-memtable / RSS / disk-usage atomics, which drive the hot-path
    /// admission checks. Uses a `Weak` self-pointer so it exits when the
    /// engine is dropped. Idempotent-safe to call once after Arc-wrapping.
    ///
    /// LOAD-BEARING: runs on a dedicated OS thread, NOT a `tokio::spawn` task.
    /// The whole point of the parent breaker is to fire under a runaway ingest
    /// load — exactly the condition that saturates the tokio worker pool. A
    /// tokio-task sampler starves under that load: its `interval.tick()` wakes
    /// but never gets scheduled, so the trip flags never flip and the process
    /// OOMs anyway (observed: a MemoryMax=2G ingest reached the 2G cap and was
    /// OOM-killed while `memory_tripped` stayed false). A plain thread with
    /// `std::thread::sleep` is immune to runtime starvation. All work here is
    /// sync (DashMap scan + two syscalls), so no runtime is needed.
    pub fn spawn_resource_sampler(self: &Arc<Self>) {
        // Nothing to drive the flags if the governor was never installed.
        if crate::governor::global().is_none() {
            return;
        }
        let period = std::time::Duration::from_millis(crate::governor::SAMPLE_INTERVAL_MS);

        // ── Thread A: memory + disk ──────────────────────────────────────
        // Touches NO engine lock — just `/proc/self/statm` + `statvfs`. Kept
        // on its own thread so a memtable-sum stall (below) can NEVER delay
        // the memory breaker: the observed OOM was exactly RSS climbing past
        // the watermark while the sampler was blocked summing memtables under
        // a turbo batch's shard write-locks. Uses a `Weak` liveness check to
        // exit when the engine is dropped.
        let weak_a = Arc::downgrade(self);
        let data_dir = self.data_dir.to_string_lossy().to_string();
        let _ = std::thread::Builder::new()
            .name("xerj-mem-sampler".to_string())
            .spawn(move || {
                tracing::info!(
                    period_ms = crate::governor::SAMPLE_INTERVAL_MS,
                    "memory/disk sampler thread started (parent circuit breaker)"
                );
                loop {
                    std::thread::sleep(period);
                    if weak_a.strong_count() == 0 {
                        return; // engine dropped
                    }
                    let governor = match crate::governor::global() {
                        Some(g) => g,
                        None => return,
                    };
                    let rss = crate::governor::current_rss_bytes();
                    let disk_pct = crate::governor::disk_used_pct(&data_dir);
                    governor.refresh_memory_disk(rss, disk_pct);
                }
            });

        // ── Thread B: summed memtable budget ─────────────────────────────
        // Reads a lock on every memtable shard, so it may briefly block under
        // a turbo batch — that is fine here, isolated from the memory breaker.
        let weak_b = Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name("xerj-memtable-sampler".to_string())
            .spawn(move || loop {
                std::thread::sleep(period);
                let engine = match weak_b.upgrade() {
                    Some(e) => e,
                    None => return, // engine dropped
                };
                let governor = match crate::governor::global() {
                    Some(g) => g,
                    None => return,
                };
                governor.refresh_memtable(engine.total_memtable_bytes());
            });
    }

    // ── Transform pipeline methods ────────────────────────────────────────────

    /// The `index.default_pipeline` setting for `index`, if it has one.
    ///
    /// One implementation for every write path. It used to live only in the
    /// `_doc` handlers, which is why `_bulk` ignored the setting entirely and
    /// stored documents untransformed under a `201` while the single-document
    /// form of the same write refused (issue #204).
    ///
    /// Handles the three shapes settings arrive in — nested
    /// `{index: {default_pipeline: ..}}`, dotted `{"index.default_pipeline": ..}`
    /// and flat `{default_pipeline: ..}` — mirroring the `number_of_shards`
    /// lookup pattern used elsewhere.
    ///
    /// `index` is matched literally: an alias has no settings of its own, so a
    /// write addressed to an alias does not pick up its backing index's default
    /// pipeline. That is a known gap, identical on every write path.
    pub fn default_pipeline_for(&self, index: &str) -> Option<String> {
        self.index_settings.get(index).and_then(|v| {
            v.get("index")
                .and_then(|ix| ix.get("default_pipeline"))
                .or_else(|| v.get("index.default_pipeline"))
                .or_else(|| v.get("default_pipeline"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
    }

    /// Compile and register a typed transform pipeline from a JSON config,
    /// **in memory only**.
    ///
    /// `config_json` must be a valid [`PipelineConfig`](xerj_wasm::pipeline::PipelineConfig)
    /// object. The compiled pipeline is stored in `transform_pipelines` and
    /// the raw JSON in `pipelines`, so the ES-compatible ingest API can hand
    /// it back.
    ///
    /// This is the primitive underneath [`Engine::put_pipeline`] and the boot
    /// path; it deliberately does not touch disk, so it is private. Anything
    /// that serves a user request must go through `put_pipeline`, or the
    /// pipeline is gone at the next restart (issue #203).
    fn compile_pipeline(
        &self,
        name: &str,
        config_json: Value,
    ) -> std::result::Result<(), xerj_wasm::WasmError> {
        self.compile_pipeline_with_mode(name, config_json, xerj_wasm::pipeline::CompileMode::Strict)
    }

    /// [`Self::compile_pipeline`], with the strictness chosen by the caller.
    ///
    /// Issue #204. The boot replay passes
    /// [`CompileMode::ReplayPersisted`](xerj_wasm::pipeline::CompileMode) — a
    /// definition already in `cluster_state.json` was accepted by whichever
    /// build wrote it, and there is no caller left to answer 400. Refusing it
    /// at boot would flip a working cluster's writes from 201 to 400 on a
    /// binary upgrade alone, with no PUT and no operator action. Whatever the
    /// replay had to wave through is recorded in `degraded_pipelines` and
    /// logged at ERROR, so it is loud instead of silent.
    fn compile_pipeline_with_mode(
        &self,
        name: &str,
        config_json: Value,
        mode: xerj_wasm::pipeline::CompileMode,
    ) -> std::result::Result<(), xerj_wasm::WasmError> {
        let cfg: xerj_wasm::pipeline::PipelineConfig =
            serde_json::from_value(config_json.clone()).map_err(xerj_wasm::WasmError::Json)?;
        let pipeline = match mode {
            xerj_wasm::pipeline::CompileMode::Strict => {
                xerj_wasm::pipeline::Pipeline::from_config(name, &cfg)?
            }
            xerj_wasm::pipeline::CompileMode::ReplayPersisted => {
                xerj_wasm::pipeline::Pipeline::from_config_replaying_persisted(name, &cfg)?
            }
        };
        if pipeline.degradations.is_empty() {
            self.degraded_pipelines.remove(name);
        } else {
            let summary = pipeline.degradations.join("; ");
            error!(
                pipeline = name,
                degradations = summary.as_str(),
                "persisted ingest pipeline uses configuration this build cannot honour — it \
                 is still running exactly as the build that stored it ran it, but it is NOT \
                 doing what the definition says; re-PUT the definition to see the error"
            );
            self.degraded_pipelines.insert(name.to_string(), summary);
        }
        self.pipelines.insert(name.to_string(), config_json);
        self.transform_pipelines.insert(name.to_string(), pipeline);
        // Issue #204: a successful compile clears any recorded "stored but
        // not runnable" reason left by a previous definition under this id —
        // including the persisted refusal marker, whose only job is to keep a
        // refusal alive across a restart. Persisting the removal is the
        // caller's (`put_pipeline`, and the boot replay's self-heal); an
        // orphaned marker only ever makes a pipeline more refused, and the
        // next successful PUT clears it again.
        self.unrunnable_pipelines.remove(name);
        self.definition_time_refusals.remove(name);
        Ok(())
    }

    /// Store a pipeline definition that could not be compiled, together with
    /// the reason, and make sure nothing stale is left behind.
    ///
    /// Issue #204. This exists for one case: a definition xerj accepts for wire
    /// compatibility (Elasticsearch does) but cannot execute — because it names
    /// a processor this build does not implement, or sets an option on one that
    /// this build cannot honour (a processor-level `if` guard, an `on_failure`
    /// chain, a `grok` `patterns` array). Storing it keeps
    /// `GET`/`_simulate` truthful about what was submitted; recording the
    /// reason is what stops [`Self::process_through_pipeline`] from silently
    /// passing documents through untransformed. A previously-compiled pipeline
    /// under the same id is REMOVED — re-defining a pipeline must not leave the
    /// old stages quietly running under the new definition's name.
    ///
    /// Like [`Engine::put_pipeline`], the stored definition is flushed to
    /// `cluster_state.json` before the caller may acknowledge it (issue
    /// #203). `Err` means it did not reach disk; the in-memory change is
    /// rolled back first.
    ///
    /// The *reason* is persisted too, in
    /// `<data_dir>/ingest-pipeline-refusals.json`, and that is not
    /// belt-and-braces. This docstring used to claim the reason could stay in
    /// memory "because the boot replay fails to recompile the stored
    /// definition and re-records why" — which is false for the shape that
    /// matters. A `stages`-shaped definition (the one `GET` hands an operator
    /// to edit and re-`PUT`) recompiles at boot in the compatibility mode that
    /// exists for definitions written by a gate-less build, so the refusal was
    /// SOFTENED by a restart: measured, a refused `{"remove": {"field":
    /// "secret", "if": …}}` went from `400` to `201` across a plain restart,
    /// with `secret` dropped from a document the guard excludes. The marker is
    /// what tells the replay "this build already answered 400 for this exact
    /// definition", so the flush below is on the acknowledge path exactly like
    /// the definition's own.
    pub fn register_unrunnable_pipeline(
        &self,
        name: &str,
        definition: Value,
        reason: String,
    ) -> Result<()> {
        self.ensure_cluster_state_writable()?;
        let previous_doc = self.pipelines.get(name).map(|v| v.value().clone());
        let previous_compiled = self.transform_pipelines.remove(name).map(|(_, p)| p);
        let previous_reason = self
            .unrunnable_pipelines
            .get(name)
            .map(|r| r.value().clone());
        let previous_marker = self
            .definition_time_refusals
            .get(name)
            .map(|r| r.value().clone());
        self.pipelines.insert(name.to_string(), definition);
        self.unrunnable_pipelines
            .insert(name.to_string(), reason.clone());
        // The marker that makes this refusal survive a restart. Without it the
        // boot replay softens the refusal and the pipeline silently starts
        // running, ignoring the very option it was refused for (issue #204).
        self.definition_time_refusals
            .insert(name.to_string(), reason.clone());
        // Nothing is running under this id any more, so nothing is running
        // degraded either.
        let previous_degradation = self.degraded_pipelines.remove(name).map(|(_, v)| v);
        macro_rules! rollback {
            () => {{
                match previous_doc {
                    Some(d) => self.pipelines.insert(name.to_string(), d),
                    None => self.pipelines.remove(name).map(|(_, v)| v),
                };
                match previous_reason {
                    Some(r) => self.unrunnable_pipelines.insert(name.to_string(), r),
                    None => self.unrunnable_pipelines.remove(name).map(|(_, v)| v),
                };
                match previous_marker {
                    Some(r) => self.definition_time_refusals.insert(name.to_string(), r),
                    None => self.definition_time_refusals.remove(name).map(|(_, v)| v),
                };
                if let Some(p) = previous_compiled {
                    self.transform_pipelines.insert(name.to_string(), p);
                }
                if let Some(d) = previous_degradation {
                    self.degraded_pipelines.insert(name.to_string(), d);
                }
            }};
        }
        // Marker before definition — see `put_pipeline` for why that order is
        // load-bearing. An `Err` from either is NOT acknowledgeable: a refusal
        // the next boot cannot see is the whole defect.
        if let Err(e) = self.flush_pipeline_refusals() {
            rollback!();
            return Err(e);
        }
        if let Err(e) = self.flush_cluster_state() {
            rollback!();
            self.flush_pipeline_refusals_best_effort();
            return Err(e);
        }
        warn!(
            name,
            reason = reason.as_str(),
            "ingest pipeline stored but NOT runnable — documents may not be ingested through it"
        );
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
        let pipeline = self.transform_pipelines.get(pipeline_name).ok_or_else(|| {
            // Issue #204: distinguish "no such pipeline" from "stored but this
            // build cannot run it". Both must fail — what must not happen is a
            // document being indexed untransformed — but reporting the second
            // as the first sent operators hunting for a pipeline that is
            // plainly there in `GET /_ingest/pipeline`.
            match self.unrunnable_pipelines.get(pipeline_name) {
                Some(reason) => xerj_wasm::WasmError::PipelineNotRunnable {
                    pipeline: pipeline_name.to_string(),
                    reason: reason.clone(),
                },
                None => xerj_wasm::WasmError::PipelineNotFound(pipeline_name.to_string()),
            }
        })?;

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
        self.ensure_cluster_state_writable()?;
        let snap_dir = std::path::Path::new(repo_path).join(name);
        let snap_dir = validate_snapshot_path(
            repo_path,
            name,
            &snap_dir,
            &self.data_dir,
            &self.config.limits.snapshot_repo_allowlist,
        )?;
        std::fs::create_dir_all(&snap_dir).map_err(EngineError::Io)?;

        // Wall-clock start, captured BEFORE the flush+copy work so
        // duration_in_millis reflects the real elapsed time (it was hardcoded
        // to 0 because start_time == end_time were sampled at the same instant
        // after the copy).
        let start_ms = now_millis();

        let target_indices: Vec<String> = match indices {
            Some(list) if !list.is_empty() => list,
            // ES excludes system indices from an all-indices snapshot; exclude
            // our `.xerj_*` internals (auth, sessions, dashboards, …) by
            // default so a plain snapshot captures only user data.
            _ => self
                .indices
                .iter()
                .map(|e| e.key().clone())
                .filter(|n| !is_system_index(n))
                .collect(),
        };

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

            // Copy everything recursively (WAL + segments + schema). The
            // per-file list is intentionally discarded: it was serialized into
            // the response as `index_files` (~170 KB of dead weight that ES
            // never returns and `restore_snapshot` never reads — restore copies
            // the snapshot dir back wholesale and reopens by index name).
            let mut files: Vec<String> = Vec::new();
            copy_dir_recursive(&src_dir, &dst_dir, &mut files).map_err(EngineError::Io)?;
        }

        let end_ms = now_millis();

        let manifest = serde_json::json!({
            "snapshot": name,
            "uuid": uuid::Uuid::new_v4().to_string(),
            "version": "8.13.0",
            "indices": target_indices,
            "state": "SUCCESS",
            "start_time_in_millis": start_ms,
            "end_time_in_millis": end_ms,
            "duration_in_millis": (end_ms - start_ms).max(0),
            "failures": [],
            "shards": {
                "total": target_indices.len(),
                "failed": 0,
                "successful": target_indices.len(),
            },
        });

        let manifest_path = snap_dir.join("manifest.json");
        let bytes = serde_json::to_vec_pretty(&manifest).map_err(EngineError::Serde)?;
        std::fs::write(&manifest_path, bytes).map_err(EngineError::Io)?;

        info!(snapshot = name, repo = repo_path, "snapshot created");
        Ok(manifest)
    }

    /// Restore a snapshot: copies files back and reopens the indices.
    ///
    /// `indices` is the ES restore-body `indices` filter: a list of index
    /// names / wildcard patterns (each entry may itself be a comma-separated
    /// multi-target expression, ES accepts both the string and array forms).
    /// `None` / empty restores every index in the snapshot (ES default).
    /// A non-wildcard entry that matches nothing in the snapshot is an
    /// error — silently restoring nothing (or worse, everything) would
    /// misreport what was rewritten. Returns the list of index names
    /// actually restored.
    ///
    /// This filter used to be IGNORED entirely: a restore request naming
    /// one index rewrote EVERY index in the snapshot with snapshot-time
    /// state, destroying all writes made since (live-verified 2026-07-12).
    pub async fn restore_snapshot(
        &self,
        repo_path: &str,
        name: &str,
        indices: Option<Vec<String>>,
    ) -> Result<Vec<String>> {
        self.ensure_cluster_state_writable()?;
        let snap_dir = std::path::Path::new(repo_path).join(name);
        let snap_dir = validate_snapshot_path(
            repo_path,
            name,
            &snap_dir,
            &self.data_dir,
            &self.config.limits.snapshot_repo_allowlist,
        )?;
        let manifest_path = snap_dir.join("manifest.json");

        let manifest_bytes = std::fs::read(&manifest_path).map_err(EngineError::Io)?;
        let manifest: Value =
            serde_json::from_slice(&manifest_bytes).map_err(EngineError::Serde)?;

        let snapshot_indices: Vec<String> = manifest
            .get("indices")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default();

        // Apply the `indices` filter. Split each entry on commas, then match
        // each pattern against the snapshot's indices (glob `*` wildcards,
        // same matcher the search path uses for index selectors).
        let patterns: Vec<String> = indices
            .unwrap_or_default()
            .iter()
            .flat_map(|entry| entry.split(','))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let index_names: Vec<String> = if patterns.is_empty() {
            // Default restore: every index in the snapshot EXCEPT `.xerj_*`
            // system indices (ES excludes system indices from a default
            // restore). Snapshots taken by this build already omit them, but a
            // snapshot written by an older build may still carry them.
            snapshot_indices
                .into_iter()
                .filter(|n| !is_system_index(n))
                .collect()
        } else {
            let mut selected: Vec<String> = Vec::new();
            for pat in &patterns {
                // A wildcard never matches a `.xerj_*` system index (ES `*`
                // semantics); only a pattern that explicitly targets the
                // dot-namespace (`.xerj_*`, `.xerj_users`, …) may.
                let targets_system = pat.starts_with('.');
                let matched: Vec<&String> = snapshot_indices
                    .iter()
                    .filter(|n| glob_match(pat, n))
                    .filter(|n| targets_system || !is_system_index(n))
                    .collect();
                if matched.is_empty() && !pat.contains('*') {
                    // ES: restore of an index absent from the snapshot fails
                    // loud (snapshot_restore_exception). Mirror that. The
                    // repo's filesystem path is deliberately NOT echoed.
                    return Err(EngineError::Common(xerj_common::XerjError::invalid_query(
                        format!("[{name}] no index matches [{pat}] in snapshot"),
                    )));
                }
                for m in matched {
                    if !selected.contains(m) {
                        selected.push(m.clone());
                    }
                }
            }
            selected
        };

        // Canonicalize the data_dir once for the per-index containment check
        // below. `self.data_dir` always exists at this point (the engine opens
        // indices under it), but canonicalize defensively in case a test mounts
        // a relative path that hasn't been created yet.
        let data_dir_canon = self
            .data_dir
            .canonicalize()
            .unwrap_or_else(|_| self.data_dir.clone());

        for idx_name in &index_names {
            // Validate the index name BEFORE any filesystem op. Previously the
            // `IndexName::new` check ran AFTER `remove_dir_all`, so a manifest
            // carrying `..` as an index name deleted the parent of `data_dir`.
            // `IndexName::validate` now rejects `.`/`..`/separators/`..`
            // substrings, so this is belt-and-suspenders against a future
            // regression in the validator.
            let index_name = match IndexName::new(idx_name) {
                Ok(n) => n,
                Err(e) => {
                    warn!(index = idx_name, error = %e, "skipping restore of invalid index name");
                    continue;
                }
            };

            let src_dir = snap_dir.join(idx_name.as_str());
            if !src_dir.exists() {
                warn!(index = idx_name, "snapshot directory missing, skipping");
                continue;
            }

            let dst_dir = self.data_dir.join(idx_name);

            // Containment: lexical check BEFORE any filesystem op. `IndexName`
            // validation already rejects `.`/`..`/separators/`..` substrings,
            // so this is a cheap belt-and-suspenders guard against a future
            // regression in the validator that would let `dst_dir` escape the
            // data_dir.
            if !dst_dir.starts_with(&self.data_dir) {
                return Err(EngineError::Common(
                    xerj_common::XerjError::invalid_mapping(format!(
                        "refusing to restore index [{idx_name}] outside data_dir"
                    )),
                ));
            }

            // Remove existing index data (if any) and close it.
            if self.indices.contains_key(idx_name.as_str()) {
                self.indices.remove(idx_name.as_str());
            }
            if dst_dir.exists() {
                std::fs::remove_dir_all(&dst_dir).map_err(EngineError::Io)?;
            }
            std::fs::create_dir_all(&dst_dir).map_err(EngineError::Io)?;

            // After create_dir_all, re-verify the canonicalized path is still
            // inside the canonicalized data_dir (catches symlink-based escapes
            // that the lexical check misses).
            if let Ok(dst_canon) = dst_dir.canonicalize() {
                if !dst_canon.starts_with(&data_dir_canon) {
                    return Err(EngineError::Common(
                        xerj_common::XerjError::invalid_mapping(format!(
                            "refusing to restore index [{idx_name}] outside data_dir (canonical)"
                        )),
                    ));
                }
            }

            // Copy snapshot files back.
            let mut _files: Vec<String> = Vec::new();
            copy_dir_recursive(&src_dir, &dst_dir, &mut _files).map_err(EngineError::Io)?;

            // Reopen the index.
            match Index::open(index_name, &self.config, &self.data_dir) {
                Ok(idx) => {
                    // #320 — a restored index gets the tap's live retention
                    // floor, not the boot file's.
                    idx.set_wal_min_retained_generations(self.wal_retention_floor());
                    // Snapshot dirs carry es_mapping.json — reload it so the
                    // restored index serves the same mapping it was saved with.
                    // A corrupt blob in the snapshot fails the restore of that
                    // index instead of restoring it with a reduced mapping.
                    if let Err(e) = self.load_persisted_es_mapping(idx_name) {
                        warn!(index = idx_name, error = %e, "failed to reopen restored index");
                        self.record_failed_index(idx_name, e.to_string());
                        continue;
                    }
                    if let Some(m) = self.index_mappings.get(idx_name) {
                        Engine::apply_date_mapping_flags(&idx, m.value());
                    }
                    self.indices.insert(idx_name.clone(), idx);
                    // Restoring from a snapshot is *the* repair for an index
                    // that failed to open, so clear the recorded failure —
                    // otherwise cluster health stays red after the repair
                    // actually worked.
                    self.failed_indices.remove(idx_name);
                    info!(index = idx_name, "index restored from snapshot");
                }
                Err(e) => {
                    warn!(index = idx_name, error = %e, "failed to reopen restored index");
                    self.record_failed_index(idx_name, e.to_string());
                }
            }
        }

        info!(
            snapshot = name,
            repo = repo_path,
            indices = ?index_names,
            "snapshot restored"
        );
        Ok(index_names)
    }
} // end impl Engine

// ── Private helpers ───────────────────────────────────────────────────────────

/// Milliseconds since the Unix epoch (wall clock).
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// A XERJ-internal system index (`.xerj_*`: auth, sessions, dashboards,
/// audit, …). ES hides its own system indices from default snapshot/restore;
/// we do the same for ours so `_snapshot`/`_restore` operate on user data.
pub(crate) fn is_system_index(name: &str) -> bool {
    name.starts_with(".xerj_")
}

/// Validate a snapshot repo path and snapshot name before any filesystem op.
///
/// Returns the canonicalized `snap_dir` (creating it is the caller's job) or
/// an error. This is the chokepoint for the snapshot path-traversal surface:
///   - the snapshot `name` must not contain `..`, `/`, `\`, or NUL, so
///     `Path::new(repo).join(name)` cannot escape the repo;
///   - the canonicalized `snap_dir` must remain inside the canonicalized
///     `repo_path`, so a `repo_path` like `/tmp/repo/../..` is rejected;
///   - the `repo_path` itself must not be empty (defensive).
///
/// Pre-fix, `PUT /_snapshot/evil` with `location: "/etc"` plus a snapshot
/// `name = ".."` made `snap_dir = "/etc/.."`, so `create_dir_all` and
/// `manifest.json` landed in the parent of `/etc`. The snapshot name was
/// otherwise unvalidated.
fn validate_snapshot_path(
    repo_path: &str,
    name: &str,
    snap_dir: &std::path::Path,
    data_dir: &std::path::Path,
    allowlist: &[String],
) -> Result<std::path::PathBuf> {
    if repo_path.is_empty() {
        return Err(EngineError::Common(
            xerj_common::XerjError::invalid_mapping(
                "snapshot repository location must not be empty",
            ),
        ));
    }
    // F-PATH-02: the repo `location` is operator/attacker-supplied via
    // `PUT /_snapshot/{repo}`. The name-containment checks below only prove the
    // snapshot stays inside THAT location — they say nothing about whether the
    // location itself is sane. Bound the location to `data_dir` (default) or a
    // configured allowlist (an ES `path.repo` equivalent), so a repo cannot point
    // at `/etc`, `/root/.ssh`, another tenant's dir, etc.
    // F-PATH-02 (residual): a location whose fully-resolved target does not yet
    // exist makes `canonicalize()` below fail, so the lexical fallback keeps any
    // `..` components. A component-based `starts_with` then treats
    // `<data_dir>/../../escape` as *inside* `<data_dir>` (it never normalizes
    // `..`), and `create_dir_all` lets the OS walk out. Reject `..` outright —
    // no legitimate snapshot root needs parent-dir traversal — so neither the
    // location containment check nor the snapshot-name check can be bypassed
    // with an unresolved parent reference.
    if std::path::Path::new(repo_path)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(EngineError::Common(
            xerj_common::XerjError::invalid_mapping(format!(
                "snapshot repository location [{repo_path}] must not contain '..' path components"
            )),
        ));
    }
    let base_canon = |p: &std::path::Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let repo_base = base_canon(std::path::Path::new(repo_path));
    let mut allowed_bases: Vec<std::path::PathBuf> = vec![base_canon(data_dir)];
    allowed_bases.extend(
        allowlist
            .iter()
            .map(|b| base_canon(std::path::Path::new(b))),
    );
    if !allowed_bases.iter().any(|base| repo_base.starts_with(base)) {
        return Err(EngineError::Common(
            xerj_common::XerjError::invalid_mapping(format!(
                "snapshot repository location [{repo_path}] is outside data_dir and not in \
                 limits.snapshot_repo_allowlist; refusing (set the allowlist to permit external \
                 snapshot roots)"
            )),
        ));
    }
    if name.is_empty()
        || name.contains("..")
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return Err(EngineError::Common(xerj_common::XerjError::invalid_mapping(
            format!("invalid snapshot name [{name}]: must not be empty or contain '..', '/', '\\', or NUL"),
        )));
    }
    // Canonicalize repo_path. The repo may not yet exist (create_snapshot
    // creates it on the next line), so fall back to the lexically-cleaned
    // path. `Path::canonicalize` requires the path to exist.
    let repo_canon = std::path::Path::new(repo_path)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(repo_path));
    // If snap_dir already exists, canonicalize it and verify containment;
    // if it does not yet exist (the create_snapshot case), verify lexically
    // that it starts with the repo.
    if let Ok(snap_canon) = snap_dir.canonicalize() {
        if !snap_canon.starts_with(&repo_canon) {
            return Err(EngineError::Common(
                xerj_common::XerjError::invalid_mapping(format!(
                    "snapshot [{name}] resolves outside its repository; refusing"
                )),
            ));
        }
        Ok(snap_canon)
    } else {
        // Not yet on disk: lexical containment check. This catches `..` in
        // repo_path that canonicalize couldn't resolve because the parent
        // exists but the repo dir does not.
        let snap_lex = snap_dir.to_path_buf();
        if !snap_lex.starts_with(&repo_canon) {
            return Err(EngineError::Common(
                xerj_common::XerjError::invalid_mapping(format!(
                    "snapshot [{name}] resolves outside its repository; refusing"
                )),
            ));
        }
        Ok(snap_lex)
    }
}

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
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
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

#[cfg(test)]
mod snapshot_path_security_tests {
    use super::validate_snapshot_path;

    #[test]
    fn f_path_02_repo_location_outside_data_dir_is_rejected_by_default() {
        let data = tempfile::TempDir::new().unwrap();
        // A repo location pointing outside data_dir (the F-PATH-02 exploit shape)
        // with an empty allowlist must be refused.
        let outside = tempfile::TempDir::new().unwrap();
        let repo = outside.path().to_str().unwrap();
        let snap = std::path::Path::new(repo).join("s1");
        let err = validate_snapshot_path(repo, "s1", &snap, data.path(), &[])
            .expect_err("external repo location must be rejected");
        assert!(err.to_string().contains("outside data_dir"), "{err}");
    }

    #[test]
    fn location_inside_data_dir_is_allowed() {
        let data = tempfile::TempDir::new().unwrap();
        let repo_dir = data.path().join("backups");
        std::fs::create_dir_all(&repo_dir).unwrap();
        let repo = repo_dir.to_str().unwrap();
        let snap = repo_dir.join("s1");
        validate_snapshot_path(repo, "s1", &snap, data.path(), &[])
            .expect("a repo under data_dir must be allowed");
    }

    #[test]
    fn explicit_allowlist_permits_an_external_root() {
        let data = tempfile::TempDir::new().unwrap();
        let ext = tempfile::TempDir::new().unwrap();
        let repo = ext.path().to_str().unwrap().to_string();
        let snap = std::path::Path::new(&repo).join("s1");
        validate_snapshot_path(&repo, "s1", &snap, data.path(), std::slice::from_ref(&repo))
            .expect("an allowlisted external root must be permitted");
    }

    #[test]
    fn traversal_name_still_rejected_even_inside_an_allowed_base() {
        let data = tempfile::TempDir::new().unwrap();
        let snap = data.path().join("..");
        let repo = data.path().to_str().unwrap();
        assert!(validate_snapshot_path(repo, "..", &snap, data.path(), &[]).is_err());
    }

    #[test]
    fn f_path_02_residual_parent_ref_to_nonexistent_target_is_rejected() {
        // The residual escape (#73): a location containing `..` whose fully
        // resolved target does NOT yet exist. canonicalize() fails, the lexical
        // fallback keeps the `..`, and a component-based starts_with(data_dir)
        // would pass — letting create_dir_all write outside data_dir. It must be
        // refused before any directory is created.
        let data = tempfile::TempDir::new().unwrap();
        let repo = format!("{}/../../xerj-escape", data.path().display());
        let snap = std::path::Path::new(&repo).join("s1");
        let err = validate_snapshot_path(&repo, "s1", &snap, data.path(), &[])
            .expect_err("a location containing `..` must be rejected");
        assert!(err.to_string().contains(".."), "unexpected error: {err}");
        // And it stays rejected even if the operator allowlists a broad root:
        // the `..` guard runs before containment, so a parent-ref cannot be
        // laundered through the allowlist either.
        let err2 = validate_snapshot_path(&repo, "s1", &snap, data.path(), &["/".to_string()])
            .expect_err("`..` must be rejected even with a permissive allowlist");
        assert!(err2.to_string().contains(".."), "unexpected error: {err2}");
    }
}

#[cfg(test)]
mod ism_policy_persistence_validation_tests {
    use crate::lifecycle::{LifecyclePolicy, LifecycleState};
    use crate::Engine;
    use xerj_common::config::Config;

    fn valid_policy() -> LifecyclePolicy {
        LifecyclePolicy {
            description: None,
            default_state: "only".to_string(),
            states: vec![LifecycleState {
                name: "only".to_string(),
                actions: vec![],
                transitions: vec![],
            }],
        }
    }

    /// `load_persisted_ism_policies` is the one path into `ism_policies`
    /// that doesn't go through the REST layer's `validate()` call (native
    /// PUT and the ILM-translation path both validate before ever storing
    /// a policy) — it just reads back whatever was written to disk. A
    /// hand-edited or otherwise corrupted single entry must not silently
    /// load: `change_ism_policy` only checks "does this policy_id exist",
    /// so an unvalidated entry here would be reachable through it without
    /// ever having passed the same structural checks a direct PUT enforces.
    #[tokio::test]
    async fn corrupt_single_entry_is_skipped_valid_entries_still_restored() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.server.data_dir = dir.path().to_string_lossy().into_owned();

        // "broken" transitions to a state that doesn't exist — exactly what
        // `LifecyclePolicy::validate()` rejects, but nothing stops it being
        // written to disk directly (bypassing the REST layer entirely).
        let mut policies = std::collections::HashMap::new();
        policies.insert("good-policy".to_string(), valid_policy());
        policies.insert(
            "broken-policy".to_string(),
            LifecyclePolicy {
                description: None,
                default_state: "start".to_string(),
                states: vec![LifecycleState {
                    name: "start".to_string(),
                    actions: vec![],
                    transitions: vec![crate::lifecycle::LifecycleTransition {
                        state_name: "does-not-exist".to_string(),
                        conditions: None,
                    }],
                }],
            },
        );
        std::fs::write(
            dir.path().join("ism_policies.json"),
            serde_json::to_vec(&policies).unwrap(),
        )
        .unwrap();

        let engine = Engine::new(config).unwrap();
        assert!(
            engine.ism_policies.contains_key("good-policy"),
            "a structurally valid persisted policy must still load"
        );
        assert!(
            !engine.ism_policies.contains_key("broken-policy"),
            "a policy that would fail validate() must not be silently restored"
        );
    }
}

#[cfg(test)]
mod api_key_record_migration_tests {
    use super::ApiKeyRecord;

    /// A record as it comes off disk, before the load path touches it.
    fn loaded(secret_hash: &str, plaintext: Option<&str>) -> ApiKeyRecord {
        ApiKeyRecord {
            name: "loaded".to_string(),
            secret_hash: secret_hash.to_string(),
            legacy_plaintext_secret: plaintext.map(str::to_string),
            creation_ms: 1_753_600_000_000,
            expiration_ms: None,
            invalidated: false,
            invalidation_ms: None,
            roles: Vec::new(),
        }
    }

    /// The discriminator is `is_usable_hash`, not `is_empty` and not a check
    /// of the `$ssha256$` tag: a `secret_hash` that does not decode can never
    /// verify against anything, so a record carrying only that is as unusable
    /// as one carrying nothing and must be dropped, not restored as a live
    /// key. Three of these carry the tag — a prefix test would call them
    /// migrated and keep them.
    #[test]
    fn an_unparseable_hash_with_no_plaintext_is_dropped() {
        for stored in [
            "",
            "$ssha256$",
            "$ssha256$truncated",
            "$ssha256$deadbeef$cafe",
            "$argon2id$v=19$m=1,t=1,p=1$c2FsdA$aGFzaA",
            "left-over-plaintext",
        ] {
            for plaintext in [None, Some("")] {
                let mut rec = loaded(stored, plaintext);
                assert_eq!(
                    rec.migrate_from_plaintext(),
                    Err(()),
                    "{stored:?} + {plaintext:?} must be dropped, not restored"
                );
            }
        }
    }

    /// A plaintext beside a hash-shaped value that does not decode is a store
    /// nobody can explain — not the pre-#201 shape (that has no `secret_hash`
    /// at all), and not a migrated one. Deriving a live credential from the
    /// half of it #201 exists to delete is the fail-open reading, so the
    /// record is dropped instead.
    #[test]
    fn a_plaintext_beside_an_unparseable_hash_is_dropped_not_repaired() {
        for stored in [
            "$ssha256$truncated",
            "$argon2id$v=19$m=1,t=1,p=1$c2FsdA$aGFzaA",
            "left-over-plaintext",
        ] {
            let mut rec = loaded(stored, Some("the-plaintext"));
            assert_eq!(
                rec.migrate_from_plaintext(),
                Err(()),
                "{stored:?} + a plaintext is ambiguous and must be dropped"
            );
        }
    }

    /// A usable hash still wins over a leftover plaintext, and the plaintext is
    /// discarded — re-deriving there could silently swap which secret the
    /// record authenticates.
    #[test]
    fn a_usable_hash_wins_over_a_leftover_plaintext() {
        let hash = crate::secret_hash::hash_secret("hashed-secret");
        let mut rec = loaded(&hash, Some("stale-plaintext"));
        assert_eq!(rec.migrate_from_plaintext(), Ok(true));
        assert_eq!(rec.secret_hash, hash);
        assert!(rec.legacy_plaintext_secret.is_none());
        assert!(rec.verify_secret("hashed-secret"));
        assert!(!rec.verify_secret("stale-plaintext"));
    }

    /// An already-migrated record is left alone and reports no rewrite.
    #[test]
    fn an_already_hashed_record_is_untouched() {
        let hash = crate::secret_hash::hash_secret("s");
        let mut rec = loaded(&hash, None);
        assert_eq!(rec.migrate_from_plaintext(), Ok(false));
        assert_eq!(rec.secret_hash, hash);
    }

    /// The pre-#201 shape: plaintext only.
    #[test]
    fn a_pre_201_plaintext_record_is_hashed() {
        let mut rec = loaded("", Some("legacy-secret"));
        assert_eq!(rec.migrate_from_plaintext(), Ok(true));
        assert!(crate::secret_hash::is_usable_hash(&rec.secret_hash));
        assert!(rec.verify_secret("legacy-secret"));
    }

    /// Even if one reached the auth path unmigrated, an unusable hash denies.
    #[test]
    fn verify_secret_denies_an_unusable_hash() {
        for stored in ["", "$ssha256$truncated", "plaintext-secret"] {
            let rec = loaded(stored, None);
            assert!(
                !rec.verify_secret(stored),
                "{stored:?} must not verify against itself"
            );
            assert!(!rec.verify_secret("anything"));
        }
    }
}
