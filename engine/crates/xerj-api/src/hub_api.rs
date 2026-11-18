//! Corpus Hub v2 — the hosted, read-only endpoints (plan §6,
//! `docs/CORPUS_HUB_V2_IMPLEMENTATION.md`).
//!
//! Four `GET` routes under `/_hub/v1/` answer from the pre-indexed packs that
//! `xerj hub pull` restored on this node, so an agent with no clone and no
//! local index still gets an answer in one request:
//!
//! | Route | Answer |
//! |---|---|
//! | `GET /_hub/v1/catalogue` | the bytes of the file at `hub.catalogue_path`, `ETag` = its sha256, `Cache-Control: public, max-age=3600`, `304` on a matching `If-None-Match` |
//! | `GET /_hub/v1/def/{slug}/{symbol}?k=` | an [`Answer`] from [`xerj_hub::def::lookup_in`] (the one `def` function the CLI uses) |
//! | `GET /_hub/v1/code/{slug}?q=&k=&max_tokens=&mode=` | an [`Answer`] built from [`xerj_common::xccode::run_code_query`] (the pipeline `xerj code` uses) |
//! | `GET /_hub/v1/search?q=&k=` | `{corpora:[{slug,best_score,best_hit}]}`, `multi_match` on `body,defs,symbol_text` across every pack index, grouped by corpus, best first, at most 10 |
//!
//! ## Limits (all four routes)
//!
//! `GET` only — any other method is `405` (behind authentication, unless
//! `hub.public` opens the route). Read-only: nothing here writes.
//! A corpus name must be in the catalogue (`404` otherwise, also for a name in
//! the catalogue with the `excluded` licence class — never served). `symbol`
//! and `q` are limited to [`MAX_INPUT_BYTES`] (`400`); `k` is clamped to
//! [`MAX_K`]. An unrecognised query parameter is a `400`, the way Elasticsearch
//! treats one, so a typo is never silently ignored.
//!
//! ## `hub.public` and the rate limit
//!
//! With `hub.public = true` exactly the four routes above — by exact path
//! shape, see [`is_public_path`] — skip authentication, authorization and the
//! audit log; `router.rs` wraps those three layers with a check that calls
//! [`is_public_request`]. Everything else on the node keeps all three. The
//! audit layer is skipped on purpose: it records every unauthenticated write-
//! shaped request, and a public route must not be a way to flush a bounded
//! evidence ring.
//!
//! `hub.public` makes the routes reachable by anyone, so they are rate-limited
//! per client address ([`RATE_LIMIT`] requests per [`RATE_WINDOW`], `429` with
//! `Retry-After`) — in both modes, so the limit never depends on how the node is
//! configured. `xerj-api` has no per-API-key limiter to reuse — the only
//! limiter in the crate is the share-claim one, which is private to
//! [`crate::share::ShareStore`] and shares a bounded table with claim
//! attempts, so hub traffic would throttle share claims. This module has its
//! own [`RateLimiter`]. The client address is [`crate::share::ClaimSource`]:
//! the TCP peer, believing `X-Forwarded-For` only from `server.trusted_proxies`.
//!
//! ## How a query reaches the engine — and why it is not a loopback call
//!
//! `lookup` and `run_code_query` are written against [`XcHttp`], a three-method
//! HTTP shell (`search`, `get_mapping`, `cat_indices_json`). The plan suggests
//! implementing it for the in-process case by calling the node's own loopback
//! address with the admin key. This module implements it **in-process
//! instead**: [`InProcessNode`] hands each call to a private router that holds
//! exactly the three read routes (`internal_router`) — the same handlers,
//! extractors and response encoding the real HTTP routes use, with no socket.
//! Reasons:
//!
//! * the node cannot reliably name its own address: `server.es_compat_port = 0`
//!   (ephemeral) is a supported configuration, `--port` rewrites ports, TLS may
//!   be on, and the bind address need not be loopback;
//! * no credential is needed, so no admin key is read from config or sent
//!   anywhere on a path a stranger can reach;
//! * the blocking client in `xerj-hub` (`HubNode`) has a 300 s timeout — far too
//!   long on a path a stranger can hold open — and would make the node an HTTP
//!   client of itself, a new kind of outbound connection for a server that
//!   publishes an inventory of every way data leaves it (`docs/RERANK.md`);
//! * the private router offers three read routes and nothing else, which is a
//!   smaller surface than "the admin key against the whole API".
//!
//! Four further guards bound what a hosted query can touch, even if a bug let
//! an unexpected name through:
//!
//! * [`InProcessNode`] refuses any index expression that is not made of `xc-`
//!   names in a conservative character set;
//! * every call runs under an engine-level [`index_guard`] that hides every
//!   index outside `xc-*` AND every index the caller's own request could not
//!   see. `HubVisibility` is the INTERSECTION of the hub namespace and the rule
//!   `authz_middleware` installed for the caller, captured before the work leaves
//!   the request's task: a key scoped to `tenant-a-*` is refused a corpus here
//!   exactly as it is by `_search`, and a hosted route is never a way to widen a
//!   grant;
//! * the ledger's `index_prefix` is pack-supplied text, so it is checked against
//!   the exact shape `xc-<slug>-b<digits>` for the corpus being asked about
//!   ([`valid_build_prefix`]) before any query is built from it, and a `def` or
//!   `code` request is then pinned to that build (`InProcessNode::pin_to_build`):
//!   the node refuses an index expression outside it, whatever the shared
//!   pipeline goes on to compute from its own second read of the ledger;
//! * the corpus name is checked against the catalogue before any filesystem path
//!   is built from it.
//!
//! The licence is decided the same way on every route: the STRICTER of the
//! catalogue's class and the corpus's own ledger's ([`effective_licence`]); either
//! can refuse (`excluded`), and either can require the read-do-not-copy warning
//! (`approach-only`).
//!
//! The `XcHttp` methods are synchronous and the handlers async, so each query
//! runs on the blocking pool (`run_query`) under a global concurrency cap
//! ([`MAX_CONCURRENT_QUERIES`], `503` + `Retry-After` when full) and a wall
//! clock timeout ([`QUERY_TIMEOUT`], `504`).
//!
//! ## What the in-process call changes about the search it makes
//!
//! The hub's call sets `es_compat::InProcessSearch`, a task-local that a request
//! off the wire cannot carry, and nothing else does:
//!
//! * **It is not audited.** The node's `_search` handler appends every search to
//!   the audit ring — 4,096 entries shared by the whole node — so one entry per
//!   public request would let a crowd of strangers age the operator's evidence out
//!   of it. The HTTP access log is the record of hub reads. (The audit LAYER in
//!   `router.rs` is skipped for the public routes for the same reason.)
//! * **A failing index is skipped, not fatal.** `search` fans out over every pack
//!   on the node, and the engine used to fail the whole request on the first index
//!   that errored: one pack with an unusable mapping took `search` down for every
//!   other pack. The index is skipped (logged, counted in `_shards.failed`); when
//!   EVERY index fails the first failure is the answer, so an outage is a `503`
//!   and never an empty list. `search` also sends `lenient`, which removes the
//!   commonest cause (a pack that maps `body` as a number) at the source.
//! * **An oversized response shrinks the window.** One in-process response is
//!   capped at 16 MiB (`MAX_INTERNAL_RESPONSE_BYTES`). `code` fetches whole records, ten
//!   times the `k` asked for, so two very large records overflow it even at
//!   `k=1`; the search is retried with half the window until it fits, and the
//!   answer says it was built from fewer candidates (`truncated: true`). A
//!   mapping read that overflows is a different matter — see `search` below.
//!
//! ## Known limits, stated so they are not rediscovered
//!
//! * `Answer.answered_from` is always `hosted` here: that is where the answer
//!   came from as far as the client is concerned, even though the server read a
//!   local pack.
//! * The shared query pipeline's JSON output does not report which arms ran for
//!   `mode=semantic|hybrid` (its "BM25 only" note is prose-only), so those
//!   modes carry a warning saying what each one DOES instead: `semantic` over a
//!   corpus with no `semantic_text` mapping returns no hits (it does not fall
//!   back), `hybrid` answers from BM25 alone.
//! * `search` reads the node-wide mappings to learn which of `body`, `defs` and
//!   `symbol_text` exist (this engine collapses a multi-token query to zero hits
//!   when a named field is mapped nowhere). If that read fails — typically
//!   because the mappings of every pack together are larger than the response
//!   cap — `search` is a `503`, not `{"corpora":[]}`, and the failure is
//!   remembered for a few seconds (`SEARCH_FIELDS_FAILURE_TTL`) so it is not recomputed by every
//!   request. A caller that runs under a visibility rule of its own reads (and
//!   shares) nothing from that cache.
//! * `search` queries `xc-*`, not the plan's `xc-*-docs,xc-*-defs`. This engine
//!   resolves index-name wildcards only as `*`, `prefix*` or `*suffix`
//!   (`glob_match_simple`, `es_compat.rs`), so a `*` in the middle of a name
//!   matches nothing: measured on this build, `xc-*-docs`, `xc-*-defs` and
//!   `xc-mini-*-docs` return zero hits where `xc-*`, `xc-m*` and an explicit
//!   list return the documents. The plan's pattern would make `search` answer
//!   "nothing matches" for everything, with a `200`. `xc-*` also covers every
//!   dataset index of a pack (autoindex names them after the repo's
//!   directories, not `docs`), so the route sees more of a pack than the plan's
//!   two suffixes would have. Hits that belong to no listed corpus are skipped.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use axum::body::{Body, Bytes};
use axum::extract::{FromRequestParts, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::NaiveDate;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use xerj_common::hub::{
    Answer, AnswerMode, AnsweredFrom, Catalogue, Hit, LicenceClass, CATALOGUE_FORMAT_VERSION,
};
use xerj_common::xccode::{self, passage, state as ledger, CodeParams, Mode, XcHttp};
use xerj_engine::index_guard::{self, IndexVisibility};

use crate::error::{EsErrorBody, EsErrorResponse, EsRootCause};
use crate::es_compat;
use crate::share::ClaimSource;
use crate::state::AppState;

// ─────────────────────────────────────────────────────────────────────────────
// Limits
// ─────────────────────────────────────────────────────────────────────────────

/// `symbol` and `q` may be at most this many bytes (after percent-decoding).
pub const MAX_INPUT_BYTES: usize = 512;
/// `k` is clamped to this.
pub const MAX_K: usize = 50;
/// `k` when the request names none (`def` and `code`).
pub const DEFAULT_K: usize = 5;
/// `k` when the request names none (`search`).
pub const SEARCH_DEFAULT_K: usize = 10;
/// `search` returns at most this many corpora, whatever `k` says.
pub const MAX_SEARCH_CORPORA: usize = 10;
/// Requests one client address may make per [`RATE_WINDOW`].
pub const RATE_LIMIT: usize = 60;
/// The sliding window [`RATE_LIMIT`] is counted over.
pub const RATE_WINDOW: Duration = Duration::from_secs(60);
/// Hard ceiling on tracked client addresses. Past it, addresses not yet seen
/// share one overflow bucket — throttled together rather than tracked without
/// bound, because this table is fed by unauthenticated callers.
pub const MAX_TRACKED_CLIENTS: usize = 10_000;
/// Engine-backed queries allowed in flight at once.
pub const MAX_CONCURRENT_QUERIES: usize = 16;
/// Wall-clock budget for one engine-backed query.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(20);
/// `max_tokens` is refused above this.
pub const MAX_TOKENS_CAP: u64 = 8192;
/// How many characters one token is taken to be when `max_tokens` is applied.
/// An approximation, stated in the response warning when it binds.
pub const CHARS_PER_TOKEN: usize = 4;

/// Passage width when the request names no `max_tokens` (the CLI's default).
const DEFAULT_PASSAGE_CHARS: usize = 800;
/// Cap on any single internal response, so one hosted query cannot pull an
/// unbounded body into memory.
const MAX_INTERNAL_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Cap on the catalogue file the node will read and serve.
const MAX_CATALOGUE_BYTES: u64 = 16 * 1024 * 1024;
/// What `search` queries. Not the plan's `xc-*-docs,xc-*-defs`: this engine
/// resolves only `*`, `prefix*` and `*suffix`, so a `*` mid-name matches no
/// index (see the module docs, "Known limits").
const SEARCH_INDEX_EXPR: &str = "xc-*";
/// The fields `search` matches on, before the mapping gate drops any no
/// searched index maps (see [`HubShared::search_fields_blocking`]).
const SEARCH_FIELDS: [&str; 3] = ["body", "defs", "symbol_text"];
/// Hits fetched so that grouping by corpus still has ten corpora to pick from.
const SEARCH_POOL: usize = 100;
/// How long a resolved `search` field list is reused.
const SEARCH_FIELDS_TTL: Duration = Duration::from_secs(60);
/// How long a FAILED read of the `search` field list is remembered. Short, so a
/// transient fault clears itself quickly; not zero, because the usual cause is a
/// node-wide mapping too large to read (see [`HubShared::search_fields_blocking`])
/// and that read costs seconds every time it is repeated.
const SEARCH_FIELDS_FAILURE_TTL: Duration = Duration::from_secs(15);
/// The `size` an Elasticsearch-style search has when its body names none.
const DEFAULT_SEARCH_SIZE: u64 = 10;
/// How many times, in all, one hosted request may halve a search window because
/// the response did not fit the cap. Each try is a whole search that is thrown
/// away, so the budget is per request and not per search: a pipeline that
/// searches three times (`hybrid`) gets no more tries than one that searches
/// once. Seven halvings take the largest window (200) down to one.
const MAX_WINDOW_SHRINKS: usize = 7;
/// Longest excerpt carried by a `search` best hit built from a docs record.
const EXCERPT_CHARS: usize = 240;
/// `_source` fields `search` asks the engine for — everything a best hit needs,
/// and none of the (large) `body`.
const SEARCH_SOURCE_FIELDS: [&str; 14] = [
    "symbol",
    "kind",
    "file",
    "line",
    "text",
    "value",
    "ax_path",
    "ax_file",
    "path",
    "lineno",
    "start_line",
    "title",
    "defs",
    "code",
];

// ─────────────────────────────────────────────────────────────────────────────
// Shared state
// ─────────────────────────────────────────────────────────────────────────────

/// Everything the four routes share, built once per router.
///
/// Held per router rather than in a `static` so two routers in one process
/// (every test file) never share a rate-limit budget or a catalogue.
pub struct HubShared {
    app: AppState,
    limiter: RateLimiter,
    slots: Arc<tokio::sync::Semaphore>,
    catalogue: Mutex<Option<CachedCatalogue>>,
    /// What the last read of the node-wide mappings found, and when. Shared by
    /// callers who run under no visibility rule of their own; see
    /// [`HubShared::search_fields_blocking`].
    search_fields: Mutex<Option<(Instant, FieldsRead)>>,
    tuning: Tuning,
    /// How many times the node-wide mappings were read for `search`.
    mapping_reads: AtomicU64,
    /// The answer flow's empty `cwd`/`hub_home`, made on the first `Ask` that
    /// needs it and reused after; see [`HubShared::routing_root`].
    routing_root: OnceLock<PathBuf>,
}

/// The outcome of reading the node-wide mappings for `search`.
enum FieldsRead {
    Resolved(Vec<&'static str>),
    /// The read failed. The detail is for the log; the client gets the fixed 503.
    Failed(String),
}

/// Limits a test may lower so that a failure which needs 16 MiB in production
/// needs a few KiB in a test. Production always uses [`Tuning::default`]; there
/// is no way to set this over HTTP or from configuration.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    /// Cap on any single in-process response, in bytes.
    pub response_cap_bytes: usize,
    /// How long a failed read of the `search` field list is remembered.
    pub fields_failure_ttl: Duration,
    /// Called with the ledger root and the slug right after a `def`/`code`
    /// request's ledger passed the build-prefix check and before the shared
    /// pipeline reads it again: the instant at which a `xerj hub pull` landing
    /// mid-request would change what that second read finds.
    pub after_ledger_check: Option<fn(&FsPath, &str)>,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            response_cap_bytes: MAX_INTERNAL_RESPONSE_BYTES,
            fields_failure_ttl: SEARCH_FIELDS_FAILURE_TTL,
            after_ledger_check: None,
        }
    }
}

impl HubShared {
    /// State for one router, over the node's [`AppState`].
    pub fn new(app: AppState) -> Arc<Self> {
        Self::with_tuning(app, Tuning::default())
    }

    /// Test seam: [`HubShared::new`] with lowered limits. Not reachable over
    /// HTTP.
    #[doc(hidden)]
    pub fn with_tuning(app: AppState, tuning: Tuning) -> Arc<Self> {
        Arc::new(Self {
            app,
            limiter: RateLimiter::new(RATE_LIMIT, RATE_WINDOW, MAX_TRACKED_CLIENTS),
            slots: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_QUERIES)),
            catalogue: Mutex::new(None),
            search_fields: Mutex::new(None),
            tuning,
            mapping_reads: AtomicU64::new(0),
            routing_root: OnceLock::new(),
        })
    }

    /// Test seam: how many times `search` has read the node-wide mappings.
    #[doc(hidden)]
    pub fn mapping_reads_for_test(&self) -> u64 {
        self.mapping_reads.load(Ordering::Relaxed)
    }

    /// Test seam: take `n` query slots, as `n` slow queries would. Not
    /// reachable over HTTP.
    #[doc(hidden)]
    pub fn hold_slots_for_test(&self, n: usize) -> Vec<tokio::sync::OwnedSemaphorePermit> {
        (0..n)
            .filter_map(|_| self.slots.clone().try_acquire_owned().ok())
            .collect()
    }

    /// The catalogue, re-read only when the file's size or modification time
    /// changed. Blocking (a `stat`, and a read on change) — call from the
    /// blocking pool.
    fn catalogue_blocking(&self) -> Result<Arc<CatalogueData>, CatalogueError> {
        let configured = self.app.config.hub.catalogue_path.trim();
        if configured.is_empty() {
            return Err(CatalogueError::NotConfigured);
        }
        let path = PathBuf::from(configured);
        let meta = std::fs::metadata(&path)
            .map_err(|e| CatalogueError::Unavailable(format!("stat {}: {e}", path.display())))?;
        if !meta.is_file() {
            return Err(CatalogueError::Unavailable(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
        if meta.len() > MAX_CATALOGUE_BYTES {
            return Err(CatalogueError::Unavailable(format!(
                "{} is {} bytes, over the {MAX_CATALOGUE_BYTES}-byte cap",
                path.display(),
                meta.len()
            )));
        }
        let modified = meta.modified().ok();
        let mut cache = self.catalogue.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = cache.as_ref() {
            if c.path == path && c.len == meta.len() && c.modified == modified {
                return Ok(c.data.clone());
            }
        }
        let bytes = std::fs::read(&path)
            .map_err(|e| CatalogueError::Unavailable(format!("read {}: {e}", path.display())))?;
        let data = Arc::new(CatalogueData::from_bytes(Bytes::from(bytes)));
        *cache = Some(CachedCatalogue {
            path,
            len: meta.len(),
            modified,
            data: data.clone(),
        });
        Ok(data)
    }

    async fn catalogue(self: &Arc<Self>) -> Result<Arc<CatalogueData>, CatalogueError> {
        let me = self.clone();
        match tokio::task::spawn_blocking(move || me.catalogue_blocking()).await {
            Ok(r) => r,
            Err(e) => Err(CatalogueError::Unavailable(format!(
                "catalogue task failed: {e}"
            ))),
        }
    }

    /// The catalogue's rule for `slug` when it lists the corpus and permits
    /// serving it, else the response to send instead.
    ///
    /// Everything that is not a servable corpus — a malformed name, a name the
    /// catalogue does not list, a listed name with the `excluded` licence class
    /// — is the same `404`: the caller learns nothing about which it was. The
    /// rule is the catalogue's half of the licence decision; the corpus's own
    /// ledger is the other half, and [`corpus_facts`] takes the stricter.
    async fn resolve_corpus(self: &Arc<Self>, slug: &str) -> Result<CorpusRule, Response> {
        if !valid_slug(slug) {
            return Err(unknown_corpus());
        }
        let data = self
            .catalogue()
            .await
            .map_err(CatalogueError::into_response)?;
        let index = match &data.index {
            Ok(index) => index,
            Err(why) => {
                tracing::warn!(error = %why, "hub catalogue does not parse");
                return Err(error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "hub_unavailable",
                    "the hub catalogue cannot be read right now",
                ));
            }
        };
        match index.corpora.get(slug) {
            Some(rule) if rule.licence_class != LicenceClass::Excluded => Ok(rule.clone()),
            _ => Err(unknown_corpus()),
        }
    }

    /// The `search` fields that at least one searched index maps.
    ///
    /// This engine does not ignore an unmapped `multi_match` field the way
    /// Elasticsearch does: a field that NO searched index maps collapses a
    /// multi-token query to zero hits (`xccode::fields::resolve_fields` has the
    /// measurement). So the fixed list is intersected with the mappings'
    /// union — the same gate the CLI applies — and the answer is reused for
    /// [`SEARCH_FIELDS_TTL`], since reading the mappings of every pack is far
    /// more expensive than a search. Blocking.
    ///
    /// `Err` is a read that FAILED, and `search` answers it with the fixed 503:
    /// an unreadable mapping is an outage, not "no results". The usual cause is
    /// a node-wide mapping bigger than the cap on one in-process response
    /// (16 MiB: a few hundred wide indices will do it), and reading it costs
    /// seconds each time, so a failure is remembered for
    /// [`Tuning::fields_failure_ttl`] — short, so a transient fault clears
    /// itself — and the read is single-flight: the lock is held while the
    /// mappings are read, so a crowd of requests arriving together waits for one
    /// read instead of making sixteen.
    ///
    /// A caller running under a visibility rule of its own sees only its slice
    /// of the node, so what it reads is not shared and what others read is not
    /// reused for it: it neither reads nor writes the cache.
    fn search_fields_blocking(&self, node: &InProcessNode) -> Result<Vec<&'static str>, String> {
        if node.is_guarded() {
            return self.read_search_fields(node).map(Option::unwrap_or_default);
        }
        let mut cache = self.search_fields.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, read)) = cache.as_ref() {
            match read {
                FieldsRead::Resolved(fields) if at.elapsed() < SEARCH_FIELDS_TTL => {
                    return Ok(fields.clone());
                }
                FieldsRead::Failed(why) if at.elapsed() < self.tuning.fields_failure_ttl => {
                    return Err(why.clone());
                }
                _ => {}
            }
        }
        match self.read_search_fields(node) {
            Ok(Some(fields)) => {
                *cache = Some((Instant::now(), FieldsRead::Resolved(fields.clone())));
                Ok(fields)
            }
            // No `xc-*` index yet: nothing to search, and not an error. Not
            // remembered, so the first pack restored is searchable on the next
            // request rather than a minute later.
            Ok(None) => Ok(Vec::new()),
            Err(why) => {
                *cache = Some((Instant::now(), FieldsRead::Failed(why.clone())));
                Err(why)
            }
        }
    }

    /// One read of the node-wide mappings, as the caller sees them. `Ok(None)`:
    /// there is no `xc-*` index at all.
    fn read_search_fields(
        &self,
        node: &InProcessNode,
    ) -> Result<Option<Vec<&'static str>>, String> {
        self.mapping_reads.fetch_add(1, Ordering::Relaxed);
        match node.get_mapping(&format!("/{SEARCH_INDEX_EXPR}/_mapping")) {
            Ok(mapping) => Ok(Some(mapped_search_fields(&mapping))),
            Err(e) if e.contains("__404__") => Ok(None),
            Err(e) => Err(format!("cannot read the node-wide mappings: {e}")),
        }
    }
}

struct CachedCatalogue {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
    data: Arc<CatalogueData>,
}

/// The catalogue file as read: its bytes and etag (what `GET /catalogue`
/// serves), and the corpora it lists (what every other route validates against).
struct CatalogueData {
    bytes: Bytes,
    etag: String,
    index: Result<CatalogueIndex, String>,
    /// The whole catalogue, term sketches included, when it parses: what the
    /// gRPC `Ask` routes a question with (`ask_answer`).
    parsed: Option<Arc<Catalogue>>,
}

struct CatalogueIndex {
    corpora: HashMap<String, CorpusRule>,
}

/// What the catalogue says about one corpus that a route needs.
#[derive(Clone)]
struct CorpusRule {
    licence_spdx: String,
    licence_class: LicenceClass,
}

impl CatalogueData {
    fn from_bytes(bytes: Bytes) -> Self {
        let etag = format!("\"{}\"", hex(&Sha256::digest(&bytes)));
        let parsed = serde_json::from_slice::<Catalogue>(&bytes)
            .map_err(|e| e.to_string())
            .and_then(|c| {
                if !xerj_common::hub::catalogue_format_supported(c.format_version) {
                    return Err(format!(
                        "catalogue format_version {} (this build reads {}..={CATALOGUE_FORMAT_VERSION})",
                        c.format_version,
                        xerj_common::hub::CATALOGUE_FORMAT_VERSION_MIN
                    ));
                }
                Ok(c)
            });
        let index = parsed
            .as_ref()
            .map_err(Clone::clone)
            .map(|c| CatalogueIndex {
                corpora: c
                    .corpora
                    .iter()
                    .map(|e| {
                        (
                            e.slug.clone(),
                            CorpusRule {
                                licence_spdx: e.licence_spdx.clone(),
                                licence_class: e.licence_class,
                            },
                        )
                    })
                    .collect(),
            });
        Self {
            bytes,
            etag,
            index,
            parsed: parsed.ok().map(Arc::new),
        }
    }
}

enum CatalogueError {
    /// `hub.catalogue_path` is empty.
    NotConfigured,
    /// Configured, but the file cannot be used; the detail is for the log only.
    Unavailable(String),
}

impl CatalogueError {
    fn into_response(self) -> Response {
        match self {
            CatalogueError::NotConfigured => error(
                StatusCode::NOT_FOUND,
                "resource_not_found_exception",
                "this node has no hub catalogue (hub.catalogue_path is not set)",
            ),
            CatalogueError::Unavailable(detail) => {
                tracing::warn!(error = %detail, "hub catalogue unavailable");
                error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "hub_unavailable",
                    "the hub catalogue cannot be read right now",
                )
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Which requests are public
// ─────────────────────────────────────────────────────────────────────────────

/// Is `path` exactly one of the four hosted routes?
///
/// By shape, never by prefix: `/_hub/v1/catalogue`, `/_hub/v1/search`,
/// `/_hub/v1/code/{slug}` and `/_hub/v1/def/{slug}/{symbol}`, each segment
/// non-empty and nothing after the last one. A trailing slash, an extra
/// segment, a different case or an empty segment is NOT public — it falls
/// through to authentication like any other path, which is the safe direction
/// for a predicate that grants access.
pub fn is_public_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/_hub/v1/") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        ["catalogue"] | ["search"] => true,
        ["code", slug] => !slug.is_empty(),
        ["def", slug, symbol] => !slug.is_empty() && !symbol.is_empty(),
        _ => false,
    }
}

/// Should this request skip authentication, authorization and the audit log?
///
/// True only when the node is configured `hub.public = true` AND the path is
/// one of the four hosted routes ([`is_public_path`]). Any method: a `POST` to
/// one of them reaches the router and gets its `405`, and it must not reach the
/// audit layer, which would record the unauthenticated attempt.
pub fn is_public_request(state: &AppState, req: &Request) -> bool {
    state.config.hub.public && is_public_path(req.uri().path())
}

// ─────────────────────────────────────────────────────────────────────────────
// Rate limiting
// ─────────────────────────────────────────────────────────────────────────────

/// A per-client sliding-window limiter: at most `limit` requests in any
/// `window`, and `Retry-After` is the time until the oldest of them ages out.
///
/// A sliding window rather than a refilling token bucket: the plan asks for
/// "60 requests/minute, the 61st in a minute is `429`", and a token bucket
/// that refills continuously lets a client make `limit` requests at once and
/// then more as tokens trickle back — up to twice the stated rate over the same
/// minute — so the 61st request could succeed when the machine is slow.
///
/// Memory is bounded: each client holds at most `limit` timestamps, and at most
/// `max_clients` clients are tracked; past that, new addresses share one
/// overflow bucket.
pub struct RateLimiter {
    limit: usize,
    window: Duration,
    max_clients: usize,
    inner: Mutex<LimiterInner>,
}

struct LimiterInner {
    clients: HashMap<String, VecDeque<Instant>>,
    calls: u64,
}

const OVERFLOW_KEY: &str = "overflow";
/// Expired clients are swept every this-many calls.
const SWEEP_EVERY: u64 = 256;

impl RateLimiter {
    pub fn new(limit: usize, window: Duration, max_clients: usize) -> Self {
        Self {
            limit,
            window,
            max_clients,
            inner: Mutex::new(LimiterInner {
                clients: HashMap::new(),
                calls: 0,
            }),
        }
    }

    /// Admit one request from `client` at `now`, or say how many whole seconds
    /// (at least one) until a slot frees. A refused request is not recorded:
    /// hammering a `429` does not extend it.
    pub fn admit(&self, client: &str, now: Instant) -> Result<(), u64> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.calls = inner.calls.wrapping_add(1);
        let window = self.window;
        let live = |q: &VecDeque<Instant>| {
            q.back()
                .is_some_and(|t| now.saturating_duration_since(*t) < window)
        };
        if inner.calls.is_multiple_of(SWEEP_EVERY) {
            inner.clients.retain(|_, q| live(q));
        }
        let mut key = client;
        if !inner.clients.contains_key(key) && inner.clients.len() >= self.max_clients {
            // Table full: reclaim what has expired before conceding to the
            // shared overflow bucket.
            inner.clients.retain(|_, q| live(q));
            if inner.clients.len() >= self.max_clients {
                key = OVERFLOW_KEY;
            }
        }
        let q = inner.clients.entry(key.to_string()).or_default();
        while q
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= window)
        {
            q.pop_front();
        }
        if q.len() >= self.limit {
            let oldest = q.front().copied().unwrap_or(now);
            let wait = window.saturating_sub(now.saturating_duration_since(oldest));
            let secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
            return Err(secs.max(1));
        }
        q.push_back(now);
        Ok(())
    }
}

/// The rate-limit key for a client address: the address itself for IPv4, the
/// enclosing /64 for IPv6 (one host is routinely delegated a whole /64 and can
/// take a fresh address per request, so per-address buckets there are no
/// throttle at all), and anything that is not an address as its own bucket.
/// Mirrors `share::source_bucket`, which is private to that module.
pub fn client_bucket(source: &str) -> String {
    match source.parse::<IpAddr>() {
        Ok(IpAddr::V6(v6)) => {
            let mut seg = v6.segments();
            seg[4..].fill(0);
            format!("{}/64", std::net::Ipv6Addr::from(seg))
        }
        _ => source.to_string(),
    }
}

async fn rate_limit(State(shared): State<Arc<HubShared>>, req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    let source = match ClaimSource::from_request_parts(&mut parts, &shared.app).await {
        Ok(ClaimSource(source)) => source,
        Err(never) => match never {},
    };
    let req = Request::from_parts(parts, body);
    match shared
        .limiter
        .admit(&client_bucket(&source), Instant::now())
    {
        Ok(()) => next.run(req).await,
        Err(retry_after) => {
            let mut resp = error(
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_requests",
                format!(
                    "more than {RATE_LIMIT} requests in {} seconds from this address; \
                     retry in {retry_after}s",
                    RATE_WINDOW.as_secs()
                ),
            );
            if let Ok(v) = HeaderValue::from_str(&retry_after.to_string()) {
                resp.headers_mut().insert(header::RETRY_AFTER, v);
            }
            resp
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The in-process XcHttp
// ─────────────────────────────────────────────────────────────────────────────

/// Engine-level visibility for hosted queries: `xc-*` indices only, and only
/// those the caller could already see.
///
/// Installed around every internal call. The engine consults it at the
/// funnel every name passes through (`Engine::get_index`, `list_indices`), so
/// an alias, a wildcard or a bug upstream of this module still cannot reach an
/// index outside the hub namespace.
///
/// It is the INTERSECTION of two rules and a replacement for neither. The hub
/// namespace is one. The other is `outer`: the rule the caller's own request was
/// running under, which `authz_middleware` installs for every principal that is
/// not the superuser (an API key's grants). A key scoped to `tenant-a-*` is
/// refused `xc-mini-c-b1-docs` by the ordinary API, and must be refused it here
/// too — a hosted route is a different way to ask the engine, not a different
/// set of rights. With no `outer` the caller is unrestricted (the superuser, an
/// open-mode node, or a `hub.public` request, which skips authorization) and
/// only the namespace applies.
struct HubVisibility {
    outer: Option<Arc<dyn IndexVisibility>>,
}

impl IndexVisibility for HubVisibility {
    fn visible(&self, index: &str) -> bool {
        index.starts_with("xc-") && self.outer.as_ref().is_none_or(|rule| rule.visible(index))
    }
}

/// The three read routes `lookup` and `run_code_query` need, and nothing else,
/// with no middleware: reachable only from [`InProcessNode`], never from a
/// socket.
fn internal_router(app: &AppState) -> Router {
    Router::new()
        .route(
            "/_cat/indices/:pattern",
            get(es_compat::cat_indices_pattern),
        )
        .route("/:index/_mapping", get(es_compat::get_mapping))
        .route("/:index/_search", post(es_compat::search))
        .with_state(app.clone())
}

/// Is `expr` a comma list of `xc-` index names in a character set that cannot
/// change the meaning of a URL path? Anything else is refused before a request
/// is built.
fn hub_index_expr_ok(expr: &str) -> bool {
    !expr.is_empty()
        && expr.len() <= 4096
        && expr.split(',').all(|part| {
            part.starts_with("xc-")
                && part.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'*' | b'-')
                })
        })
}

/// [`XcHttp`] over the node's own engine, in-process (see the module docs).
///
/// Methods block; call from the blocking pool, never from an async task.
pub struct InProcessNode {
    router: Router,
    rt: tokio::runtime::Handle,
    /// The caller's `Authorization` header, forwarded so the internal request
    /// carries the principal it came from. The private router has no
    /// authentication layer; nothing here grants anything.
    auth: Option<HeaderValue>,
    /// Cap on any single response read back from the engine, in bytes.
    cap: usize,
    /// The visibility rule the caller's own request was running under, captured
    /// on the request's task before the work moved to the blocking pool (a
    /// task-local does not follow a `spawn_blocking`). `None`: unrestricted.
    outer: Option<Arc<dyn IndexVisibility>>,
    /// When set, only index expressions that begin with this build prefix are
    /// sent: see [`InProcessNode::pin_to_build`].
    scope: OnceLock<String>,
    /// How many candidate records the last search that had to be cut back to fit
    /// the response cap was limited to; 0 when none was.
    cut_to: AtomicUsize,
    /// The most indices any one search of this request skipped because they
    /// failed (`_shards.failed`); 0 when none did.
    skipped: AtomicU64,
    /// Halvings of a search window this request may still spend
    /// ([`MAX_WINDOW_SHRINKS`] in all).
    shrinks_left: AtomicUsize,
}

/// Why an internal request produced no response.
enum SendError {
    /// The response was larger than the cap on one in-process response.
    TooLarge,
    /// Anything else; the text is for the log.
    Failed(String),
}

impl SendError {
    fn describe(self, cap: usize) -> String {
        match self {
            SendError::TooLarge => format!("internal response over {cap} bytes"),
            SendError::Failed(why) => why,
        }
    }
}

impl InProcessNode {
    fn new(
        app: &AppState,
        auth: Option<HeaderValue>,
        cap: usize,
        outer: Option<Arc<dyn IndexVisibility>>,
    ) -> Self {
        Self {
            router: internal_router(app),
            rt: tokio::runtime::Handle::current(),
            auth,
            cap,
            outer,
            scope: OnceLock::new(),
            cut_to: AtomicUsize::new(0),
            skipped: AtomicU64::new(0),
            shrinks_left: AtomicUsize::new(MAX_WINDOW_SHRINKS),
        }
    }

    /// Spend one of this request's window halvings; `false` when none are left.
    fn take_shrink(&self) -> bool {
        self.shrinks_left
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
            .is_ok()
    }

    /// How many of the indices a search covered could not be searched and were
    /// skipped (see [`es_compat::InProcessSearch::skip_failed_indices`]).
    fn skipped_indices(&self) -> u64 {
        self.skipped.load(Ordering::Relaxed)
    }

    /// Is the caller running under a visibility rule of its own?
    fn is_guarded(&self) -> bool {
        self.outer.is_some()
    }

    /// Confine this node to ONE corpus build: from now on an index expression is
    /// sent only if every comma-separated part begins with `prefix`.
    ///
    /// `prefix` is the build prefix [`valid_build_prefix`] accepted. The `def`
    /// and `code` pipelines re-read the corpus's ledger themselves to decide
    /// what to query, so validating the ledger once, up front, would not stop a
    /// ledger that changed in between (a `xerj hub pull` landing mid-request)
    /// from aiming a query somewhere else. Pinning the node makes the validated
    /// prefix the only thing that request can address, whatever the pipeline
    /// then computes. Set once; a second call changes nothing.
    fn pin_to_build(&self, prefix: &str) {
        let _ = self.scope.set(prefix.to_string());
    }

    /// [`hub_index_expr_ok`], and inside the build this node is pinned to.
    fn expr_ok(&self, expr: &str) -> bool {
        hub_index_expr_ok(expr)
            && self.scope.get().is_none_or(|pinned| {
                expr.split(',')
                    .all(|part| part.starts_with(pinned.as_str()))
            })
    }

    /// The number of candidate records the answer was cut back to because the
    /// full window did not fit the response cap, if it was.
    fn window_cut_to(&self) -> Option<usize> {
        match self.cut_to.load(Ordering::Relaxed) {
            0 => None,
            n => Some(n),
        }
    }

    fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Bytes), SendError> {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(a) = &self.auth {
            builder = builder.header(header::AUTHORIZATION, a.clone());
        }
        let req = match body {
            Some(b) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(b).map_err(|e| SendError::Failed(e.to_string()))?,
                )),
            None => builder.body(Body::empty()),
        }
        .map_err(|e| SendError::Failed(format!("cannot build the internal request: {e}")))?;
        let router = self.router.clone();
        let cap = self.cap;
        let call = async move {
            let resp = match router.oneshot(req).await {
                Ok(resp) => resp,
                Err(never) => match never {},
            };
            let status = resp.status().as_u16();
            let bytes = axum::body::to_bytes(resp.into_body(), cap)
                .await
                .map_err(|_| SendError::TooLarge)?;
            Ok::<_, SendError>((status, bytes))
        };
        // This search is the node talking to itself on behalf of a public route:
        // it is not audited, and one bad index must not fail it for the rest.
        let call = es_compat::scoped_in_process_search(
            es_compat::InProcessSearch {
                skip_audit: true,
                skip_failed_indices: true,
            },
            call,
        );
        let guard: Arc<dyn IndexVisibility> = Arc::new(HubVisibility {
            outer: self.outer.clone(),
        });
        // `block_on` is legal here: this runs on a blocking-pool thread, never
        // on an async worker. The visibility guard is a task-local, so it must
        // be installed inside the future this thread drives.
        self.rt.block_on(index_guard::scoped(guard, call))
    }

    fn json(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value), SendError> {
        let (status, bytes) = self.send(method, path, body)?;
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Ok((status, value))
    }
}

impl XcHttp for InProcessNode {
    fn get_mapping(&self, path: &str) -> Result<Value, String> {
        let expr = path
            .strip_prefix('/')
            .and_then(|p| p.strip_suffix("/_mapping"))
            .filter(|e| self.expr_ok(e))
            .ok_or_else(|| "refused: not a hub mapping path".to_string())?;
        match self
            .json(Method::GET, &format!("/{expr}/_mapping"), None)
            .map_err(|e| e.describe(self.cap))?
        {
            (s, v) if (200..300).contains(&s) => Ok(v),
            (404, _) => Err("__404__".to_string()),
            (s, v) => Err(format!("HTTP {s}: {v}")),
        }
    }

    fn cat_indices_json(&self, pattern: &str) -> Result<Vec<String>, String> {
        if !self.expr_ok(pattern) {
            return Err("refused: not a hub index pattern".to_string());
        }
        let path = format!("/_cat/indices/{pattern}?format=json&h=index");
        match self
            .json(Method::GET, &path, None)
            .map_err(|e| e.describe(self.cap))?
        {
            // A wildcard matching no index is "none", not an error.
            (404, _) => Ok(Vec::new()),
            (s, v) if (200..300).contains(&s) => v
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|r| r.get("index").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect()
                })
                .ok_or_else(|| "non-array _cat reply".to_string()),
            (s, v) => Err(format!("HTTP {s}: {v}")),
        }
    }

    /// `POST /{index}/_search`, retried with a smaller `size` when the response
    /// does not fit the cap on one in-process response.
    ///
    /// The `code` pipeline asks for whole records (the passage is cut from the
    /// record's `body`) and for ten times the `k` it was asked for, up to 200.
    /// Two 9 MiB records therefore overflow a 16 MiB cap even at `k=1`, and a
    /// 503 for a question the corpus can answer is the wrong outcome. So an
    /// overflowing search is repeated with half the window until it fits, within
    /// a budget of [`MAX_WINDOW_SHRINKS`] halvings for the whole request (each
    /// try is a full search that is thrown away);
    /// [`InProcessNode::window_cut_to`] then reports that the answer was built
    /// from fewer candidates. A window of one that still does not fit, or an
    /// exhausted budget, is an error: there is no smaller answer to give.
    fn search(&self, index: &str, body: &Value) -> Result<Value, String> {
        if !self.expr_ok(index) {
            return Err("refused: not a hub index expression".to_string());
        }
        let path = format!("/{index}/_search");
        let mut body = std::borrow::Cow::Borrowed(body);
        loop {
            match self.json(Method::POST, &path, Some(&body)) {
                Err(SendError::TooLarge) => {
                    let size = body
                        .get("size")
                        .and_then(Value::as_u64)
                        .unwrap_or(DEFAULT_SEARCH_SIZE);
                    if size <= 1 || !self.take_shrink() {
                        return Err(SendError::TooLarge.describe(self.cap));
                    }
                    let mut smaller = body.into_owned();
                    smaller["size"] = json!((size / 2).max(1));
                    body = std::borrow::Cow::Owned(smaller);
                }
                Err(e) => return Err(e.describe(self.cap)),
                Ok((s, v)) if (200..300).contains(&s) => {
                    let failed = v
                        .pointer("/_shards/failed")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if failed > 0 {
                        self.skipped.fetch_max(failed, Ordering::Relaxed);
                    }
                    if matches!(body, std::borrow::Cow::Owned(_)) {
                        let size = body
                            .get("size")
                            .and_then(Value::as_u64)
                            .unwrap_or(DEFAULT_SEARCH_SIZE);
                        let returned = v
                            .pointer("/hits/hits")
                            .and_then(Value::as_array)
                            .map_or(0, Vec::len) as u64;
                        // A window that came back FULL may have left matches
                        // behind; one that did not returned everything there was.
                        if returned >= size {
                            self.cut_to.store(
                                usize::try_from(size).unwrap_or(usize::MAX),
                                Ordering::Relaxed,
                            );
                        }
                    }
                    return Ok(v);
                }
                Ok((404, _)) => return Err("__404__".to_string()),
                Ok((s, v)) => return Err(format!("HTTP {s}: {v}")),
            }
        }
    }
}

/// Run `work` on the blocking pool with an [`InProcessNode`], under the global
/// concurrency cap and the wall-clock timeout.
///
/// The permit is held by the blocking closure, not by this future, so a query
/// that outlives its timeout keeps counting against the cap until it really
/// finishes — the cap bounds blocking threads, not just waiting requests.
async fn run_query<T, F>(
    shared: &Arc<HubShared>,
    auth: Option<HeaderValue>,
    work: F,
) -> Result<T, Response>
where
    T: Send + 'static,
    F: FnOnce(&InProcessNode) -> T + Send + 'static,
{
    let Ok(permit) = shared.slots.clone().try_acquire_owned() else {
        let mut resp = error(
            StatusCode::SERVICE_UNAVAILABLE,
            "hub_busy",
            "the hub is answering as many queries as it can; retry shortly",
        );
        resp.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        return Err(resp);
    };
    // Captured HERE, on the request's own task, before the work moves to the
    // blocking pool: the caller's visibility rule is a task-local and does not
    // follow a `spawn_blocking`. Read inside the closure it would see nothing and
    // silently widen the caller to the whole `xc-*` namespace.
    let node = InProcessNode::new(
        &shared.app,
        auth,
        shared.tuning.response_cap_bytes,
        index_guard::current(),
    );
    let job = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work(&node)
    });
    match tokio::time::timeout(QUERY_TIMEOUT, job).await {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "hub query task failed");
            Err(error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "hub_internal_error",
                "the hub could not answer this request",
            ))
        }
        Err(_) => Err(error(
            StatusCode::GATEWAY_TIMEOUT,
            "hub_timeout",
            format!("the hub did not answer within {}s", QUERY_TIMEOUT.as_secs()),
        )),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Request validation
// ─────────────────────────────────────────────────────────────────────────────

/// A corpus name the catalogue could legitimately hold, and one that is safe to
/// put in a filesystem path and an index pattern: ASCII alphanumeric first,
/// then alphanumerics and `. _ + -`, at most 64 bytes. Checked before the
/// catalogue is consulted, so nothing else is ever looked up or built from it.
pub fn valid_slug(slug: &str) -> bool {
    let mut bytes = slug.bytes();
    slug.len() <= 64
        && bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
}

/// Is `prefix` exactly `xc-<slug>-b<digits>` — the build prefix of the corpus
/// named `slug` and of nothing else?
///
/// The state ledger is written by `xerj hub pull` from the pack's own manifest,
/// so its `index_prefix` is pack-supplied text. Every `code` and `def` query is
/// aimed at `<index_prefix>*` or `<index_prefix>-defs`, so an unchecked prefix is
/// a way to aim a hosted query at somebody else's indices: `xc-` is every hub
/// index on the node, `xc-mini-closed-b1` is another corpus's real build (the
/// excluded one, say), and a comma list is two expressions in one. The character
/// set [`hub_index_expr_ok`] allows does not stop any of those, so the shape does:
/// the corpus's OWN slug, then `-b`, then digits to the end — no wildcard, no
/// comma, no suffix, and no other corpus's name.
pub fn valid_build_prefix(prefix: &str, slug: &str) -> bool {
    valid_slug(slug)
        && prefix
            .strip_prefix("xc-")
            .and_then(|rest| rest.strip_prefix(slug))
            .and_then(|rest| rest.strip_prefix("-b"))
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// Collect the query string into a map, refusing a parameter this route does
/// not take and a parameter given twice.
fn collect_params(
    pairs: Vec<(String, String)>,
    allowed: &[&str],
) -> Result<HashMap<String, String>, Response> {
    let mut out = HashMap::new();
    for (key, value) in pairs {
        if !allowed.contains(&key.as_str()) {
            return Err(bad_request(format!(
                "request contains unrecognized parameter [{}]; this route takes {}",
                truncate_chars(&key, 32),
                allowed.join(", ")
            )));
        }
        if out.insert(key.clone(), value).is_some() {
            return Err(bad_request(format!(
                "parameter [{key}] given more than once"
            )));
        }
    }
    Ok(out)
}

/// A required text parameter: present, non-blank, at most [`MAX_INPUT_BYTES`].
fn text_param(raw: Option<&str>, name: &str) -> Result<String, Response> {
    let raw = raw.ok_or_else(|| bad_request(format!("`{name}` is required")))?;
    check_text(raw, name)
}

/// The same limit for a value that arrived as a path segment.
fn check_text(raw: &str, name: &str) -> Result<String, Response> {
    if raw.len() > MAX_INPUT_BYTES {
        return Err(bad_request(format!(
            "`{name}` is limited to {MAX_INPUT_BYTES} bytes, got {}",
            raw.len()
        )));
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(bad_request(format!("`{name}` must not be blank")));
    }
    Ok(trimmed.to_string())
}

/// `k`: absent is `default`; otherwise an integer of at least 1, clamped to
/// [`MAX_K`]. Zero and non-numbers are a `400`, not a silent default.
pub fn parse_k(raw: Option<&str>, default: usize) -> Result<usize, String> {
    let Some(raw) = raw else {
        return Ok(default);
    };
    match raw.trim().parse::<u64>() {
        Ok(0) | Err(_) => Err(format!(
            "`k` must be an integer from 1 to {MAX_K}, got [{}]",
            truncate_chars(raw, 32)
        )),
        Ok(n) => Ok(usize::try_from(n).unwrap_or(MAX_K).min(MAX_K)),
    }
}

/// `max_tokens`: absent is no budget; otherwise an integer from 1 to
/// [`MAX_TOKENS_CAP`].
pub fn parse_max_tokens(raw: Option<&str>) -> Result<Option<u64>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    match raw.trim().parse::<u64>() {
        Ok(n) if (1..=MAX_TOKENS_CAP).contains(&n) => Ok(Some(n)),
        _ => Err(format!(
            "`max_tokens` must be an integer from 1 to {MAX_TOKENS_CAP}, got [{}]",
            truncate_chars(raw, 32)
        )),
    }
}

/// `mode`: the retrieval arm, as `xerj code --mode` spells it.
pub fn parse_mode(raw: Option<&str>) -> Result<Mode, String> {
    match raw {
        None => Ok(Mode::Bm25),
        Some(s) => Mode::parse(s.trim()).ok_or_else(|| {
            format!(
                "`mode` must be bm25, semantic or hybrid, got [{}]",
                truncate_chars(s, 32)
            )
        }),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Responses
// ─────────────────────────────────────────────────────────────────────────────

fn error(status: StatusCode, error_type: &str, reason: impl Into<String>) -> Response {
    let reason = reason.into();
    let body = EsErrorResponse {
        error: EsErrorBody {
            root_cause: vec![EsRootCause {
                error_type: error_type.to_string(),
                reason: reason.clone(),
                resource_type: None,
                resource_id: None,
                index_uuid: None,
                index: None,
            }],
            error_type: error_type.to_string(),
            reason,
            resource_type: None,
            resource_id: None,
            index_uuid: None,
            index: None,
            request_id: None,
        },
        status: status.as_u16(),
        xerj_hint: None,
    };
    (status, Json(body)).into_response()
}

fn bad_request(reason: impl Into<String>) -> Response {
    error(
        StatusCode::BAD_REQUEST,
        "illegal_argument_exception",
        reason,
    )
}

fn unknown_corpus() -> Response {
    error(
        StatusCode::NOT_FOUND,
        "resource_not_found_exception",
        "no such corpus in this hub's catalogue",
    )
}

fn not_loaded() -> Response {
    error(
        StatusCode::NOT_FOUND,
        "corpus_not_loaded",
        "this corpus is in the catalogue but is not loaded on this node",
    )
}

/// A backend failure. The detail goes to the log; the client gets a fixed
/// sentence, because the detail can name filesystem paths and engine internals.
fn unavailable(detail: &str) -> Response {
    // Debug formatting escapes control characters: the detail can echo what the
    // caller sent, and a raw newline in it would forge log lines. Bounded, so
    // an engine error carrying a large body cannot flood the log either.
    tracing::warn!(error = ?truncate_chars(detail, 500), "hub query failed");
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "hub_unavailable",
        "the hub could not answer from this corpus right now",
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Routes
// ─────────────────────────────────────────────────────────────────────────────

/// The four routes, with their own state and the rate limit in front of all of
/// them (a layer, not a per-handler call, so a route added here cannot forget
/// it). Generic over the outer router's state: this router carries its own.
pub fn routes<S>(shared: Arc<HubShared>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::<Arc<HubShared>>::new()
        .route("/_hub/v1/catalogue", get(catalogue_route))
        .route("/_hub/v1/def/:slug/:symbol", get(def_route))
        .route("/_hub/v1/code/:slug", get(code_route))
        .route("/_hub/v1/search", get(search_route))
        .layer(middleware::from_fn_with_state(shared.clone(), rate_limit))
        .with_state(shared)
}

/// `GET /_hub/v1/catalogue`.
async fn catalogue_route(State(shared): State<Arc<HubShared>>, headers: HeaderMap) -> Response {
    let data = match shared.catalogue().await {
        Ok(d) => d,
        Err(e) => return e.into_response(),
    };
    let mut resp = if etag_matches(headers.get(header::IF_NONE_MATCH), &data.etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        (
            [(header::CONTENT_TYPE, "application/json")],
            data.bytes.clone(),
        )
            .into_response()
    };
    if let Ok(v) = HeaderValue::from_str(&data.etag) {
        resp.headers_mut().insert(header::ETAG, v);
    }
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    resp
}

/// Does an `If-None-Match` header match `etag`? Handles `*`, a comma list, and
/// weak (`W/`) validators, which compare equal for a conditional GET.
pub fn etag_matches(header: Option<&HeaderValue>, etag: &str) -> bool {
    let Some(value) = header.and_then(|h| h.to_str().ok()) else {
        return false;
    };
    value.split(',').map(str::trim).any(|candidate| {
        candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag
    })
}

/// What a blocking query reports back.
enum Outcome {
    Done(Box<Answer>),
    NotLoaded,
    /// The corpus's own ledger says `excluded`, whatever the catalogue lists:
    /// both must permit serving, and either can refuse.
    NotServed,
    Failed(String),
}

/// A blocking query's outcome as the answer, or the response that refuses it.
fn settle(outcome: Result<Outcome, Response>) -> Result<Answer, Response> {
    match outcome {
        Ok(Outcome::Done(answer)) => Ok(*answer),
        Ok(Outcome::NotLoaded) => Err(not_loaded()),
        Ok(Outcome::NotServed) => Err(unknown_corpus()),
        Ok(Outcome::Failed(detail)) => Err(unavailable(&detail)),
        Err(resp) => Err(resp),
    }
}

fn respond(answer: Result<Answer, Response>) -> Response {
    match answer {
        Ok(answer) => Json(answer).into_response(),
        Err(resp) => resp,
    }
}

/// `GET /_hub/v1/def/{slug}/{symbol}?k=`.
async fn def_route(
    State(shared): State<Arc<HubShared>>,
    Path((slug, symbol)): Path<(String, String)>,
    Query(pairs): Query<Vec<(String, String)>>,
    headers: HeaderMap,
) -> Response {
    let params = match collect_params(pairs, &["k"]) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let symbol = match check_text(&symbol, "symbol") {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let k = match parse_k(params.get("k").map(String::as_str), DEFAULT_K) {
        Ok(k) => k,
        Err(why) => return bad_request(why),
    };
    respond(
        def_core(
            &shared,
            slug,
            &symbol,
            k,
            headers.get(header::AUTHORIZATION).cloned(),
        )
        .await,
    )
}

/// `def` for one corpus: the body of `GET /_hub/v1/def/{slug}/{symbol}` after
/// its query string was read, shared with the gRPC `XerjHub.Def` RPC
/// ([`def_answer`]) so both surfaces return the same [`Answer`] and refuse the
/// same requests. `auth` is the caller's `Authorization` value, forwarded to
/// the in-process node.
async fn def_core(
    shared: &Arc<HubShared>,
    slug: String,
    symbol: &str,
    k: usize,
    auth: Option<HeaderValue>,
) -> Result<Answer, Response> {
    let symbol = check_text(symbol, "symbol")?;
    let k = k.clamp(1, MAX_K);
    let rule = shared.resolve_corpus(&slug).await?;
    let after_check = shared.tuning.after_ledger_check;
    let outcome = run_query(shared, auth, move |node| {
        let root = hub_root();
        let facts = match corpus_facts(&root, &slug, &rule) {
            Ok(f) => f,
            Err(LedgerError::NotLoaded) => return Outcome::NotLoaded,
            Err(LedgerError::Invalid(why)) => return Outcome::Failed(why),
        };
        if facts.licence_class == LicenceClass::Excluded {
            return Outcome::NotServed;
        }
        // Nothing this request sends may name an index outside the build the
        // ledger was just checked to describe, whatever `lookup_in` then reads.
        node.pin_to_build(&facts.index_prefix);
        if let Some(hook) = after_check {
            hook(&root, &slug);
        }
        // Mirror the CLI's refusals: a corpus the ledger knows but this node
        // holds no index for is "not loaded", not "no such definition".
        match node.cat_indices_json(&format!("{}*", facts.index_prefix)) {
            Ok(live) if live.is_empty() => return Outcome::NotLoaded,
            Ok(_) => {}
            Err(e) => return Outcome::Failed(format!("listing indices: {e}")),
        }
        let mut answer = xerj_hub::def::lookup_in(node, &root, &slug, &symbol, k);
        // `lookup` reports a failed query as a warning on an empty answer. The
        // CLI maps that to an exit code earlier; over HTTP a failure must not
        // read as "no such definition".
        if let Some(w) = answer
            .warnings
            .iter()
            .find(|w| w.starts_with("defs query failed"))
        {
            return Outcome::Failed(w.clone());
        }
        answer.answered_from = AnsweredFrom::Hosted;
        // `lookup_in` describes the corpus from its ledger alone. This node
        // serves it under the stricter of the ledger and the catalogue, and the
        // header and every hit must say so (a `code` answer for the same corpus
        // does).
        answer.licence_class = facts.licence_class;
        answer.licence_spdx = facts.licence_spdx.clone();
        if let Some(note) = approach_only_note(&facts) {
            for hit in &mut answer.hits {
                hit.licence_warning = Some(note.clone());
            }
        }
        if let Some(window) = node.window_cut_to() {
            answer.truncated = true;
            answer.warnings.push(window_cut_warning(window));
        }
        Outcome::Done(Box::new(answer))
    })
    .await;
    settle(outcome)
}

/// `GET /_hub/v1/code/{slug}?q=&k=&max_tokens=&mode=`.
async fn code_route(
    State(shared): State<Arc<HubShared>>,
    Path(slug): Path<String>,
    Query(pairs): Query<Vec<(String, String)>>,
    headers: HeaderMap,
) -> Response {
    let params = match collect_params(pairs, &["q", "k", "max_tokens", "mode"]) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let query = match text_param(params.get("q").map(String::as_str), "q") {
        Ok(q) => q,
        Err(resp) => return resp,
    };
    let k = match parse_k(params.get("k").map(String::as_str), DEFAULT_K) {
        Ok(k) => k,
        Err(why) => return bad_request(why),
    };
    let max_tokens = match parse_max_tokens(params.get("max_tokens").map(String::as_str)) {
        Ok(m) => m,
        Err(why) => return bad_request(why),
    };
    let mode = match parse_mode(params.get("mode").map(String::as_str)) {
        Ok(m) => m,
        Err(why) => return bad_request(why),
    };
    respond(
        code_core(
            &shared,
            slug,
            &query,
            CodeOpts {
                k,
                max_tokens,
                mode,
            },
            headers.get(header::AUTHORIZATION).cloned(),
        )
        .await,
    )
}

/// What a `code` query asks for besides the corpus and the question.
#[derive(Clone, Copy, Debug)]
struct CodeOpts {
    k: usize,
    max_tokens: Option<u64>,
    mode: Mode,
}

/// `code` for one corpus: the body of `GET /_hub/v1/code/{slug}` after its
/// query string was read, shared with the gRPC `XerjHub.Ask` RPC (through
/// [`ask_answer`]) so both surfaces refuse the same requests.
async fn code_core(
    shared: &Arc<HubShared>,
    slug: String,
    query: &str,
    opts: CodeOpts,
    auth: Option<HeaderValue>,
) -> Result<Answer, Response> {
    let query = check_text(query, "q")?;
    let CodeOpts {
        k,
        max_tokens,
        mode,
    } = opts;
    let k = k.clamp(1, MAX_K);
    if let Some(m) = max_tokens {
        if !(1..=MAX_TOKENS_CAP).contains(&m) {
            return Err(bad_request(format!(
                "`max_tokens` must be an integer from 1 to {MAX_TOKENS_CAP}, got [{m}]"
            )));
        }
    }
    let rule = shared.resolve_corpus(&slug).await?;
    let width = passage_width(max_tokens);
    let after_check = shared.tuning.after_ledger_check;
    let outcome = run_query(shared, auth, move |node| {
        let root = hub_root();
        let facts = match corpus_facts(&root, &slug, &rule) {
            Ok(f) => f,
            Err(LedgerError::NotLoaded) => return Outcome::NotLoaded,
            Err(LedgerError::Invalid(why)) => return Outcome::Failed(why),
        };
        if facts.licence_class == LicenceClass::Excluded {
            return Outcome::NotServed;
        }
        // `run_code_query` reads the ledger again to decide what to query. Pin
        // the node to the build just validated, so nothing it computes can leave
        // that corpus.
        node.pin_to_build(&facts.index_prefix);
        if let Some(hook) = after_check {
            hook(&root, &slug);
        }
        let mut params = CodeParams::new(&slug, &query);
        params.k = k;
        params.mode = mode;
        params.full = width;
        // The raw server response, patched with the diversified page: the
        // same list the prose renderer prints, as data.
        params.as_json = true;
        let out = xccode::run_code_query(
            &root,
            node,
            "in-process",
            &params,
            "not available on the hosted route",
        );
        match out.exit {
            0 | 1 => {
                let resp = out.json.unwrap_or(Value::Null);
                let mut hits = record_hits(&resp, &query, width);
                let mut warnings = out.warnings;
                if let Some(note) = approach_only_note(&facts) {
                    for hit in &mut hits {
                        hit.licence_warning = Some(note.clone());
                    }
                }
                if let Some(note) = mode_warning(mode) {
                    warnings.push(note.to_string());
                }
                let mut truncated = match max_tokens {
                    Some(budget) => apply_token_budget(&mut hits, budget, &mut warnings),
                    None => false,
                };
                if let Some(window) = node.window_cut_to() {
                    truncated = true;
                    warnings.push(window_cut_warning(window));
                }
                // A miss must not read as "this code does not exist" when part
                // of the corpus could not be searched.
                let skipped = node.skipped_indices();
                if skipped > 0 {
                    warnings.push(skipped_warning(skipped));
                }
                Outcome::Done(Box::new(facts.into_answer(
                    AnswerMode::How,
                    hits,
                    truncated,
                    warnings,
                )))
            }
            3 => Outcome::NotLoaded,
            _ => Outcome::Failed(out.text),
        }
    })
    .await;
    settle(outcome)
}

/// `GET /_hub/v1/search?q=&k=`.
async fn search_route(
    State(shared): State<Arc<HubShared>>,
    Query(pairs): Query<Vec<(String, String)>>,
    headers: HeaderMap,
) -> Response {
    let params = match collect_params(pairs, &["q", "k"]) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let query = match text_param(params.get("q").map(String::as_str), "q") {
        Ok(q) => q,
        Err(resp) => return resp,
    };
    let k = match parse_k(params.get("k").map(String::as_str), SEARCH_DEFAULT_K) {
        Ok(k) => k.min(MAX_SEARCH_CORPORA),
        Err(why) => return bad_request(why),
    };
    let data = match shared.catalogue().await {
        Ok(d) => d,
        Err(e) => return e.into_response(),
    };
    if let Err(why) = &data.index {
        tracing::warn!(error = %why, "hub catalogue does not parse");
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "hub_unavailable",
            "the hub catalogue cannot be read right now",
        );
    }
    let blocking_shared = shared.clone();
    let auth = headers.get(header::AUTHORIZATION).cloned();
    let outcome = run_query(&shared, auth, move |node| {
        // A failed read of the mappings is an outage, not an empty node.
        let fields = blocking_shared.search_fields_blocking(node)?;
        if fields.is_empty() {
            return Ok(Vec::new());
        }
        // Only the top hits are read, never a total: with the fan-out over every
        // pack index, counting matches is work nobody asked for.
        //
        // `lenient`: the fan-out covers every pack on the node, and one pack that
        // maps `body` as a number must not fail a text query for all the others
        // (this engine answers a text query against a numeric field with a 400
        // unless the clause is lenient). An index that still fails is skipped
        // by the engine under [`es_compat::InProcessSearch`], not fatal.
        let body = json!({
            "size": SEARCH_POOL,
            "track_total_hits": false,
            "_source": SEARCH_SOURCE_FIELDS,
            "query": { "multi_match": { "query": query, "fields": fields, "lenient": true } },
        });
        let resp = match node.search(SEARCH_INDEX_EXPR, &body) {
            Ok(r) => r,
            Err(e) if e.contains("__404__") => return Ok(Vec::new()),
            Err(e) => return Err(format!("search failed: {e}")),
        };
        let root = hub_root();
        let corpora = match &data.index {
            Ok(index) => group_by_corpus(&resp, &index.corpora, k, |slug, rule| {
                servable_licence(&root, slug, rule)
            }),
            Err(_) => Vec::new(),
        };
        Ok(corpora)
    })
    .await;
    match outcome {
        Ok(Ok(corpora)) => {
            let rows: Vec<Value> = corpora
                .into_iter()
                .map(|c| json!({ "slug": c.slug, "best_score": c.hit.score, "best_hit": c.hit }))
                .collect();
            Json(json!({ "corpora": rows })).into_response()
        }
        Ok(Err(detail)) => unavailable(&detail),
        Err(resp) => resp,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The same answers for a surface that is not HTTP (gRPC `XerjHub`, plan §7)
// ─────────────────────────────────────────────────────────────────────────────
//
// The gRPC service is a thin adapter: it converts its request into the
// arguments below and the [`Answer`] that comes back into its own wire form.
// Every check and every query is the HTTP route's own code (`def_core`,
// `code_core`, `resolve_corpus`, the catalogue cache, the in-process node and
// its guards), so the two surfaces cannot drift apart: the same corpus is
// refused for the same reason on both, and `Def` returns the same `Answer`
// field for field as `GET /_hub/v1/def/{slug}/{symbol}`.

/// Why a hub request was refused, independent of the surface it arrived on:
/// the status the `/_hub/v1` route answers with and the reason it puts in the
/// body. A non-HTTP surface maps the status to its own error codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubRefusal {
    pub status: StatusCode,
    pub reason: String,
    /// The `Retry-After` value, when the route sends one (`429`, a busy `503`).
    pub retry_after: Option<String>,
}

impl HubRefusal {
    fn new(status: StatusCode, reason: impl Into<String>) -> Self {
        Self {
            status,
            reason: reason.into(),
            retry_after: None,
        }
    }
}

/// The refusal an error [`Response`] built by this module carries.
async fn refusal(resp: Response) -> HubRefusal {
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap_or_default();
    let reason = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/reason")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| status.canonical_reason().unwrap_or("refused").to_string());
    HubRefusal {
        status,
        reason,
        retry_after,
    }
}

impl HubShared {
    /// Count one request from a transport that is not an axum request (the
    /// gRPC `XerjHub` service) against the same per-client budget the HTTP
    /// routes' layer draws on. The client is read exactly as there
    /// ([`crate::share::client_source`]): the TCP `peer`, or the
    /// `x-forwarded-for` values only when `peer` is a declared
    /// `server.trusted_proxies` entry.
    pub fn admit_peer(&self, peer: Option<IpAddr>, forwarded: &[&str]) -> Result<(), HubRefusal> {
        self.admit(&crate::share::client_source(&self.app, peer, forwarded))
    }

    /// Count one request from `client` (an IP address, as text) against the same
    /// per-address budget the HTTP routes use ([`RATE_LIMIT`] per
    /// [`RATE_WINDOW`]); `Err` is the `429` refusal.
    pub fn admit(&self, client: &str) -> Result<(), HubRefusal> {
        self.limiter
            .admit(&client_bucket(client), Instant::now())
            .map_err(|retry_after| HubRefusal {
                status: StatusCode::TOO_MANY_REQUESTS,
                reason: format!(
                    "more than {RATE_LIMIT} requests in {} seconds from this address; retry in \
                     {retry_after}s",
                    RATE_WINDOW.as_secs()
                ),
                retry_after: Some(retry_after.to_string()),
            })
    }
}

/// `GET /_hub/v1/def/{slug}/{symbol}?k=` for a caller that is not HTTP: the
/// same `def_core` the route runs. `k == 0` means [`DEFAULT_K`]; anything
/// larger than [`MAX_K`] is clamped. `auth` is the caller's `Authorization`
/// value (`ApiKey …`), forwarded to the in-process node exactly as the route
/// forwards its header. Run it under the caller's visibility rule
/// (`index_guard::scoped`): it is captured here, as on the HTTP path.
pub async fn def_answer(
    shared: &Arc<HubShared>,
    slug: &str,
    symbol: &str,
    k: usize,
    auth: Option<HeaderValue>,
) -> Result<Answer, HubRefusal> {
    let k = if k == 0 { DEFAULT_K } else { k };
    match def_core(shared, slug.to_string(), symbol, k, auth).await {
        Ok(answer) => Ok(answer),
        Err(resp) => Err(refusal(resp).await),
    }
}

/// The catalogue file and its detached signature.
pub struct SignedCatalogue {
    /// The file's exact bytes — what `GET /_hub/v1/catalogue` serves and what
    /// the signature covers.
    pub bytes: Bytes,
    /// The raw ed25519 signature from `<hub.catalogue_path>.sig` (lower-case hex
    /// on disk, decoded here); empty when the node has no signature file, in
    /// which case the client must treat the catalogue as unverified.
    pub signature: Vec<u8>,
}

/// A detached signature file is 128 hex characters; anything near this size is
/// not one.
const MAX_SIGNATURE_FILE_BYTES: u64 = 1024;

/// `GET /_hub/v1/catalogue` for a caller that is not HTTP, plus the signature
/// the static site serves beside it. The path is the node's configuration, never
/// anything a request supplies.
pub async fn catalogue_signed(shared: &Arc<HubShared>) -> Result<SignedCatalogue, HubRefusal> {
    let data = match shared.catalogue().await {
        Ok(d) => d,
        Err(e) => return Err(refusal(e.into_response()).await),
    };
    let sig_path = format!("{}.sig", shared.app.config.hub.catalogue_path.trim());
    let signature = tokio::task::spawn_blocking(move || read_signature(FsPath::new(&sig_path)))
        .await
        .unwrap_or_default();
    Ok(SignedCatalogue {
        bytes: data.bytes.clone(),
        signature,
    })
}

/// Decode the hex signature at `path`; empty when it is absent, oversized or
/// not hex (logged — a node that serves a broken signature should be told).
fn read_signature(path: &FsPath) -> Vec<u8> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Vec::new();
    };
    if !meta.is_file() || meta.len() > MAX_SIGNATURE_FILE_BYTES {
        tracing::warn!(path = %path.display(), "hub catalogue signature file is not usable");
        return Vec::new();
    }
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let hex = text.trim();
    let decoded: Option<Vec<u8>> = hex
        .len()
        .is_multiple_of(2)
        .then(|| {
            (0..hex.len())
                .step_by(2)
                .map(|i| {
                    hex.get(i..i + 2)
                        .and_then(|b| u8::from_str_radix(b, 16).ok())
                })
                .collect()
        })
        .flatten();
    decoded.unwrap_or_else(|| {
        tracing::warn!(path = %path.display(), "hub catalogue signature is not hex");
        Vec::new()
    })
}

/// A question for [`ask_answer`] — the gRPC `HubAskRequest`, decoded.
#[derive(Debug, Clone)]
pub struct HubAsk {
    pub question: String,
    /// `None`: route by the catalogue's term sketches.
    pub corpus: Option<String>,
    /// `None`: decide from the question, as `xerj ask` does.
    pub mode: Option<AnswerMode>,
    /// A soft budget for the rendered answer; `None` = none.
    pub max_tokens: Option<u64>,
}

/// Distinct files a `files` answer lists at most (the `xerj ask --files` value).
const FILES_K: usize = 10;

/// Answer a question from the corpora restored on THIS node, through the one
/// answer flow `xerj ask` runs ([`xerj_hub::ask::ask`]): the same mode
/// detection, the same router (term sketches and exact-symbol evidence over this
/// node's catalogue), the same licence warnings and the same token cap. Only the
/// backend differs: [`NodeBackend`] answers through `def_core` / `code_core`,
/// the code behind `GET /_hub/v1/def` and `/_hub/v1/code`.
///
/// Read-only by construction: the flow runs with `offline` and `no_fetch`, no
/// hosted client, and a backend that refuses to pull — so a request can never
/// download, restore, evict or delete a pack, start a process, or reach the
/// network. An `excluded` corpus is removed from the catalogue before routing,
/// so it can neither be routed to nor named (`404`, the same as an unknown
/// corpus); the corpus's own ledger is checked again by the cores.
pub async fn ask_answer(
    shared: &Arc<HubShared>,
    ask: HubAsk,
    auth: Option<HeaderValue>,
) -> Result<Answer, HubRefusal> {
    use xerj_hub::ask::{
        self as flow, AskArgs, AskDeps, HostedSwitch, ModeRequest, EXIT_NEEDS_DECISION, EXIT_USAGE,
    };

    let question = match check_text(&ask.question, "question") {
        Ok(q) => q,
        Err(resp) => return Err(refusal(resp).await),
    };
    if let Some(corpus) = &ask.corpus {
        if !valid_slug(corpus) {
            return Err(refusal(unknown_corpus()).await);
        }
    }
    if let Some(m) = ask.max_tokens {
        if !(1..=MAX_TOKENS_CAP).contains(&m) {
            return Err(HubRefusal::new(
                StatusCode::BAD_REQUEST,
                format!("`max_tokens` must be an integer from 1 to {MAX_TOKENS_CAP}, got [{m}]"),
            ));
        }
    }
    let data = match shared.catalogue().await {
        Ok(d) => d,
        Err(e) => return Err(refusal(e.into_response()).await),
    };
    let Some(full) = data.parsed.clone() else {
        tracing::warn!("hub catalogue does not parse");
        return Err(HubRefusal::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "the hub catalogue cannot be read right now",
        ));
    };
    let routable = Catalogue {
        format_version: full.format_version,
        generated_at: full.generated_at.clone(),
        corpora: full
            .corpora
            .iter()
            .filter(|e| e.licence_class != LicenceClass::Excluded)
            .cloned()
            .collect(),
    };
    let mode = match ask.mode {
        None => ModeRequest::Auto,
        Some(AnswerMode::How) => ModeRequest::How,
        Some(AnswerMode::Files) => ModeRequest::Files,
        // `mode = def` names no symbol of its own: it is the one the question
        // names (`what is the value of X`), else the whole question.
        Some(AnswerMode::Def) => ModeRequest::Def(
            flow::detect_mode(&question, &ModeRequest::Auto)
                .1
                .unwrap_or_else(|| question.clone()),
        ),
    };
    let mut args = AskArgs::new(question);
    args.corpus = ask.corpus;
    args.mode = mode;
    args.offline = true;
    args.no_fetch = true;
    args.hosted = HostedSwitch::Off;
    args.json = true;
    args.max_tokens = ask.max_tokens.map(|m| m as usize);

    let root = shared.routing_root().map_err(|e| {
        tracing::warn!(error = %e, "hub routing directory");
        HubRefusal::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "the hub could not answer this request",
        )
    })?;
    let backend = NodeBackend {
        shared: shared.clone(),
        rt: tokio::runtime::Handle::current(),
        auth,
        // Captured on the caller's task: a task-local does not follow the
        // `spawn_blocking` below (see `run_query`).
        visibility: index_guard::current(),
        refused: std::sync::Mutex::new(None),
    };
    let job = tokio::task::spawn_blocking(move || {
        let outcome = flow::ask(
            &args,
            &AskDeps {
                // No pack cache and no project on the serving side: an empty
                // directory whose `.git` marker stops the dependency walk, so
                // the server's own working directory never steers routing.
                hub_home: &root,
                cwd: &root,
                catalogue: &routable,
                hosted: None,
                backend: &backend,
            },
        );
        let refused = backend.refused.lock().ok().and_then(|mut r| r.take());
        (outcome, refused)
    });
    let (outcome, refused) = match job.await {
        Ok(done) => done,
        Err(e) => {
            tracing::warn!(error = %e, "hub ask task failed");
            return Err(HubRefusal::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "the hub could not answer this request",
            ));
        }
    };
    if let Some(answer) = outcome.answer {
        // A miss is an answer (no hits), never an error.
        return Ok(answer);
    }
    if let Some(r) = refused {
        return Err(r);
    }
    let said = outcome.stderr.join("; ");
    Err(match outcome.exit {
        // `select_corpus`: a corpus the catalogue does not list (or lists as
        // excluded — removed above). Same words as the HTTP routes' 404.
        EXIT_USAGE => refusal(unknown_corpus()).await,
        EXIT_NEEDS_DECISION => HubRefusal::new(
            StatusCode::CONFLICT,
            format!(
                "{said}; candidates: {}",
                outcome.stdout.lines().collect::<Vec<_>>().join("; ")
            ),
        ),
        _ => HubRefusal::new(StatusCode::NOT_FOUND, said),
    })
}

/// Where [`HubShared::routing_root`] lives under the data directory.
///
/// An index's store is `<data_dir>/<index name>`, so this name must be one no
/// index can take: [`xerj_common::types::IndexName`] refuses a leading `_`
/// (a name starts with a lowercase letter or `.`), the same reason the ML
/// model store is `<data_dir>/_ml`. The old `hub-routing` was a legal index
/// name, so an `Ask` wrote a `.git` into a user's `hub-routing` index.
pub const ROUTING_DIR: &str = "_hub-routing";

impl HubShared {
    /// `<data_dir>/_hub-routing`, an empty directory holding only a `.git`
    /// marker: the `cwd` and `hub_home` [`ask_answer`] hands the answer flow.
    /// The router reads lockfiles from `cwd` up to the first `.git`, and the
    /// pack cache from `hub_home/packs`; here there must be neither.
    ///
    /// Made once per node, on the first `Ask`, not on every request. A failed
    /// attempt is not remembered, so the next `Ask` tries again.
    fn routing_root(&self) -> std::io::Result<PathBuf> {
        if let Some(root) = self.routing_root.get() {
            return Ok(root.clone());
        }
        let root = PathBuf::from(&self.app.config.server.data_dir).join(ROUTING_DIR);
        std::fs::create_dir_all(root.join(".git"))?;
        Ok(self.routing_root.get_or_init(|| root).clone())
    }
}

/// The answer flow's [`xerj_hub::ask::Backend`] on a serving node: "the local
/// pack" is the corpus restored on this node, answered through `def_core` /
/// `code_core`. It never pulls.
struct NodeBackend {
    shared: Arc<HubShared>,
    rt: tokio::runtime::Handle,
    auth: Option<HeaderValue>,
    visibility: Option<Arc<dyn IndexVisibility>>,
    /// The refusal behind the last failed answer, so the caller can report the
    /// route's own status rather than the flow's prose.
    refused: std::sync::Mutex<Option<HubRefusal>>,
}

impl xerj_hub::ask::Backend for NodeBackend {
    /// Whether the corpus is restored here is decided by the cores themselves
    /// (`404 corpus_not_loaded` when its ledger or its indices are missing), the
    /// same check the HTTP routes make, so it is not made twice.
    fn local_pack_ready(&self, _slug: &str) -> Result<bool, String> {
        Ok(true)
    }

    fn answer_local(&self, req: &xerj_hub::ask::Request<'_>) -> Result<Answer, String> {
        let shared = self.shared.clone();
        let auth = self.auth.clone();
        let slug = req.slug.to_string();
        let question = req.question.to_string();
        let symbol = req.symbol.map(str::to_string);
        let mode = req.mode;
        let k = req.k;
        let work = async move {
            match (mode, symbol) {
                (AnswerMode::Def, Some(symbol)) => {
                    def_core(&shared, slug, &symbol, k.unwrap_or(DEFAULT_K), auth).await
                }
                _ => {
                    let k = k.unwrap_or(if mode == AnswerMode::Files {
                        FILES_K
                    } else {
                        DEFAULT_K
                    });
                    let opts = CodeOpts {
                        k,
                        max_tokens: None,
                        mode: Mode::Bm25,
                    };
                    code_core(&shared, slug, &question, opts, auth)
                        .await
                        .map(|a| {
                            if mode == AnswerMode::Files {
                                files_answer(a)
                            } else {
                                a
                            }
                        })
                }
            }
        };
        // A blocking-pool thread (see `ask_answer`), so `block_on` is legal.
        let result = self
            .rt
            .block_on(index_guard::scoped_opt(self.visibility.clone(), work));
        match result {
            Ok(answer) => Ok(answer),
            Err(resp) => {
                let r = self.rt.block_on(refusal(resp));
                let reason = r.reason.clone();
                if let Ok(mut slot) = self.refused.lock() {
                    *slot = Some(r);
                }
                Err(reason)
            }
        }
    }

    fn pull_now(
        &self,
        entry: &xerj_common::hub::CatalogueEntry,
    ) -> Result<(), xerj_hub::ask::PullFailure> {
        Err(xerj_hub::ask::PullFailure {
            code: xerj_hub::ask::EXIT_NO_ANSWER,
            message: format!(
                "'{}' is not loaded on this node; a hub node never pulls on a request's behalf",
                xerj_hub::ask::printable(&entry.slug)
            ),
        })
    }

    fn pull_in_background(&self, _slug: &str) -> Result<xerj_hub::ask::BackgroundPull, String> {
        Err("a hub node never pulls on a request's behalf".to_string())
    }
}

/// A `how` answer reshaped as the `files` answer `xerj ask --files` prints: one
/// hit per distinct file, best first, carrying the corpus licence.
fn files_answer(mut answer: Answer) -> Answer {
    let mut seen = std::collections::HashSet::new();
    let spdx = answer.licence_spdx.clone();
    answer.hits.retain(|h| seen.insert(h.file.clone()));
    for hit in &mut answer.hits {
        hit.kind = "file".to_string();
        hit.text = format!("licence: {spdx}");
        hit.value = None;
    }
    answer.mode = AnswerMode::Files;
    answer
}

// ─────────────────────────────────────────────────────────────────────────────
// Facts about a loaded corpus
// ─────────────────────────────────────────────────────────────────────────────

/// Where the state ledger lives — the same root `xerj hub pull` writes and
/// `xerj_hub::def::lookup` reads.
fn hub_root() -> PathBuf {
    xerj_autoindex::xc::code_root()
}

/// What an [`Answer`] says about the corpus it came from, from the ledger the
/// pull wrote. Mirrors `xerj_hub::def`'s private `DefMeta` so a `def` answer
/// and a `code` answer for the same corpus carry the same header; a test pins
/// the two together.
///
/// `index_prefix` is the build prefix [`valid_build_prefix`] accepted, never
/// what the ledger merely said. `licence_class` and `licence_spdx` are the
/// effective licence ([`effective_licence`]): the stricter of the ledger's and
/// the catalogue's, not the ledger's alone.
struct CorpusFacts {
    corpus: String,
    index_prefix: String,
    pin_sha: String,
    pin_date: String,
    age_days: u32,
    licence_spdx: String,
    licence_class: LicenceClass,
}

/// Why a corpus's ledger cannot be used.
enum LedgerError {
    /// There is no readable ledger: the corpus is not loaded on this node.
    NotLoaded,
    /// A ledger that does not describe a servable pack. The detail is for the
    /// log; the client gets the fixed 503.
    Invalid(String),
}

/// The corpus's ledger, checked, and its licence reconciled with the
/// catalogue's `rule`.
///
/// The ledger is written by `xerj hub pull` from the pack's own manifest, so
/// nothing in it is trusted to say where to query: its build prefix must be
/// exactly `xc-<slug>-b<digits>` ([`valid_build_prefix`]) or the corpus is
/// refused before any query is built from it.
fn corpus_facts(root: &FsPath, slug: &str, rule: &CorpusRule) -> Result<CorpusFacts, LedgerError> {
    let st = ledger::load_state(root, slug).map_err(|_| LedgerError::NotLoaded)?;
    let index_prefix = ledger::query_prefix(&st);
    if !valid_build_prefix(&index_prefix, slug) {
        return Err(LedgerError::Invalid(format!(
            "the ledger for {slug} names the build prefix [{}], not xc-{slug}-b<digits>",
            truncate_chars(&index_prefix, 80)
        )));
    }
    let pin_date = st.pin_date.clone().unwrap_or_default();
    let (licence_class, licence_spdx) = effective_licence(
        (rule.licence_class, &rule.licence_spdx),
        (
            parse_licence_class(st.licence_class.as_deref()),
            st.licence_spdx.as_deref().unwrap_or_default(),
        ),
    );
    Ok(CorpusFacts {
        corpus: st.name(slug),
        index_prefix,
        age_days: age_days_from(&pin_date, st.indexed_at.as_deref()),
        pin_sha: st.pin_sha.clone().unwrap_or_default(),
        pin_date,
        licence_spdx,
        licence_class,
    })
}

/// How restrictive a class is: permissive < approach-only < excluded.
fn licence_rank(class: LicenceClass) -> u8 {
    match class {
        LicenceClass::AdaptWithAttribution => 0,
        LicenceClass::ApproachOnly => 1,
        LicenceClass::Excluded => 2,
    }
}

/// The licence a corpus is served under: the STRICTER of the catalogue's and the
/// ledger's class, each given as `(class, spdx)`.
///
/// Both must permit serving and either can refuse — the catalogue is what the
/// hub operator lists, the ledger is what this node's pack says about itself —
/// so neither may loosen the other: a catalogue entry that is servable does not
/// override a ledger that says `excluded`, and a ledger that says nothing (an
/// absent class reads as permissive) does not override a catalogue that says
/// `approach-only`. Every route takes its class from here, so `def`, `code` and
/// `search` cannot disagree about the same corpus.
///
/// The SPDX id travels with the class that decided: the stricter side's, falling
/// back to the other side's when it names none; on a tie the ledger's, because
/// that is what the pack on this node says it is.
pub fn effective_licence(
    catalogue: (LicenceClass, &str),
    ledger: (LicenceClass, &str),
) -> (LicenceClass, String) {
    let (decider, other) = if licence_rank(catalogue.0) > licence_rank(ledger.0) {
        (catalogue, ledger)
    } else {
        (ledger, catalogue)
    };
    let spdx = if decider.1.is_empty() {
        other.1
    } else {
        decider.1
    };
    (decider.0, spdx.to_string())
}

/// The effective licence for `search`, or `None` when the corpus must not be
/// served from it: the stricter class is `excluded`, or the corpus's ledger
/// cannot be used (not loaded here, or naming a build that is not its own) —
/// the same corpora `def` and `code` answer `404`/`503` for.
fn servable_licence(
    root: &FsPath,
    slug: &str,
    rule: &CorpusRule,
) -> Option<(LicenceClass, String)> {
    let facts = corpus_facts(root, slug, rule).ok()?;
    (facts.licence_class != LicenceClass::Excluded)
        .then_some((facts.licence_class, facts.licence_spdx))
}

/// What `mode` does on a hosted corpus, for the one warning that says so. The
/// shared query pipeline's JSON output does not report which arms ran, so the
/// answer cannot say; it says what each mode DOES instead, and the two modes
/// behave differently when the corpus has no `semantic_text` mapping for `body`.
fn mode_warning(mode: Mode) -> Option<&'static str> {
    match mode {
        Mode::Bm25 => None,
        Mode::Semantic => Some(
            "mode=semantic searches only the indices that map `body` as semantic_text; a corpus \
             with none returns no hits (it does not fall back to BM25), and this response does \
             not say which indices were searched",
        ),
        Mode::Hybrid => Some(
            "mode=hybrid combines BM25 with a vector search where the corpus maps `body` as \
             semantic_text and answers from BM25 alone where it does not; this response does not \
             say which arms ran",
        ),
    }
}

/// The warning for an answer from a corpus some of whose indices could not be
/// searched (the engine skipped them rather than fail the whole request).
fn skipped_warning(count: u64) -> String {
    format!(
        "{count} of this corpus's indices could not be searched and were skipped; the answer \
         is from the rest, so a missing result may be in one of them"
    )
}

/// The warning for an answer built from fewer candidate records than usual
/// because the full window did not fit the response cap.
fn window_cut_warning(window: usize) -> String {
    format!(
        "the matching records are too large to return together; the answer was built from the \
         top {window} candidate record(s) instead of the usual window, so lower-ranked matches \
         may be missing"
    )
}

impl CorpusFacts {
    fn into_answer(
        self,
        mode: AnswerMode,
        hits: Vec<Hit>,
        truncated: bool,
        warnings: Vec<String>,
    ) -> Answer {
        Answer {
            corpus: self.corpus,
            pin_sha: self.pin_sha,
            pin_date: self.pin_date,
            age_days: self.age_days,
            licence_spdx: self.licence_spdx,
            licence_class: self.licence_class,
            answered_from: AnsweredFrom::Hosted,
            mode,
            hits,
            truncated,
            warnings,
        }
    }
}

/// An unknown or absent class never upgrades to a stricter one by surprise.
fn parse_licence_class(s: Option<&str>) -> LicenceClass {
    match s {
        Some("approach-only") => LicenceClass::ApproachOnly,
        Some("excluded") => LicenceClass::Excluded,
        _ => LicenceClass::AdaptWithAttribution,
    }
}

/// Days from the pin date (else the index stamp) to today, never negative.
fn age_days_from(pin_date: &str, indexed_at: Option<&str>) -> u32 {
    if let Ok(d) = NaiveDate::parse_from_str(pin_date, "%Y-%m-%d") {
        return (chrono::Utc::now().date_naive() - d).num_days().max(0) as u32;
    }
    if let Some(stamp) = indexed_at {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%dT%H:%M:%SZ") {
            let age = chrono::Utc::now().naive_utc().signed_duration_since(dt);
            return age.num_days().max(0) as u32;
        }
    }
    0
}

fn approach_only_note(facts: &CorpusFacts) -> Option<String> {
    (facts.licence_class == LicenceClass::ApproachOnly).then(|| licence_note(&facts.licence_spdx))
}

/// The wording `xerj_hub::def` puts on every hit of an approach-only corpus.
fn licence_note(spdx: &str) -> String {
    format!(
        "approach-only licence ({}): read, do not copy",
        if spdx.is_empty() { "unknown" } else { spdx }
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Building hits
// ─────────────────────────────────────────────────────────────────────────────

/// `s` cut to at most `n` characters (never splitting a code point).
pub fn truncate_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// `path:12` → (`path`, 12); anything else is all path and line 0 (a line is
/// never invented).
fn split_locator(loc: &str) -> (String, u32) {
    if let Some((path, tail)) = loc.rsplit_once(':') {
        if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(n) = tail.parse::<u64>() {
                return (path.to_string(), u32::try_from(n).unwrap_or(u32::MAX));
            }
        }
    }
    (loc.to_string(), 0)
}

/// The text of a code record: `body`, else `text`, else its longest string
/// field (a raw-JSON record carries its content under schema-named keys).
/// Mirrors the renderer's choice in `xccode::render`, whose helper is private.
fn record_body(src: &Value) -> String {
    if let Some(s) = src.get("body").and_then(Value::as_str) {
        return s.to_string();
    }
    if let Some(s) = src.get("text").and_then(Value::as_str) {
        return s.to_string();
    }
    let mut best: Option<&str> = None;
    if let Some(obj) = src.as_object() {
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        for key in keys {
            if let Some(s) = obj[key].as_str() {
                if !s.trim().is_empty()
                    && best.is_none_or(|b| s.chars().count() > b.chars().count())
                {
                    best = Some(s);
                }
            }
        }
    }
    best.unwrap_or_default().to_string()
}

/// One retrieved record as a [`Hit`], with the passage the CLI would print: the
/// best-matching definition when the record carries symbols, else the densest
/// window.
fn hit_from_record(row: &Value, query: &str, width: usize) -> Hit {
    let src = row.get("_source").unwrap_or(&Value::Null);
    let score = row.get("_score").and_then(Value::as_f64).unwrap_or(0.0) as f32;
    // The pack's `-defs` index sits under the same prefix, so a definition
    // record can come back here. Keep what it is — its kind, its value, its
    // exact line — instead of passing it off as a prose passage.
    if src.get("symbol").is_some_and(Value::is_string) {
        return def_hit(src, score, width);
    }
    let (file, record_line) = split_locator(&passage::provenance(src));
    let body = record_body(src);
    let (text, kind, line) = match passage::symbol_passage(&body, src, query, width) {
        Some((text, label)) => {
            let (kind, symbol_line) = parse_symbol_label(&label);
            // The matched definition's line, as the extractor recorded it against
            // the whole-file `body`, so it is a file line. A record with no
            // symbol line falls back to its own `line`, or 0: a line is reported
            // only when the index supplied one.
            let line = if symbol_line > 0 {
                symbol_line
            } else {
                record_line
            };
            (text, kind, line)
        }
        None => {
            let (window, _start, _total) = passage::best_window(&body, query, width);
            (window, "passage".to_string(), record_line)
        }
    };
    Hit {
        file,
        line,
        kind,
        text,
        value: None,
        score,
        licence_warning: None,
    }
}

/// `"function addReplyNull @ line 12"` → (`function`, 12).
fn parse_symbol_label(label: &str) -> (String, u32) {
    let Some((head, line)) = label.rsplit_once(" @ line ") else {
        return ("passage".to_string(), 0);
    };
    let kind = head.split_whitespace().next().unwrap_or("passage");
    let line = line.trim().parse::<u32>().unwrap_or(0);
    (kind.to_string(), line)
}

fn record_hits(resp: &Value, query: &str, width: usize) -> Vec<Hit> {
    resp.pointer("/hits/hits")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| hit_from_record(row, query, width))
                .collect()
        })
        .unwrap_or_default()
}

/// Characters per passage: the CLI's default, or less when `max_tokens` is
/// smaller than that.
pub fn passage_width(max_tokens: Option<u64>) -> usize {
    match max_tokens {
        Some(t) => usize::try_from(t)
            .unwrap_or(usize::MAX)
            .saturating_mul(CHARS_PER_TOKEN)
            .clamp(1, DEFAULT_PASSAGE_CHARS),
        None => DEFAULT_PASSAGE_CHARS,
    }
}

/// Hold the hits' passage text to `max_tokens`, counted as
/// [`CHARS_PER_TOKEN`] characters a token.
///
/// Hits keep their rank order while they fit. The first that does not is cut to
/// what is left and every later one is dropped. The budget covers passage text
/// only — file, line and the rest of the JSON are not counted — and the
/// four-characters-a-token figure is an approximation, not a model tokenizer;
/// the warning added whenever the budget binds says both. Returns whether
/// anything was cut or dropped.
pub fn apply_token_budget(
    hits: &mut Vec<Hit>,
    max_tokens: u64,
    warnings: &mut Vec<String>,
) -> bool {
    let budget = usize::try_from(max_tokens)
        .unwrap_or(usize::MAX)
        .saturating_mul(CHARS_PER_TOKEN);
    let mut used = 0usize;
    let mut cut = false;
    let mut keep = 0usize;
    for (i, hit) in hits.iter_mut().enumerate() {
        let len = hit.text.chars().count();
        if used.saturating_add(len) <= budget {
            used += len;
            keep = i + 1;
            continue;
        }
        let left = budget - used;
        if left > 0 {
            hit.text = truncate_chars(&hit.text, left);
            keep = i + 1;
        }
        cut = true;
        break;
    }
    let omitted = hits.len() - keep;
    hits.truncate(keep);
    if cut || omitted > 0 {
        warnings.push(format!(
            "answer held to max_tokens={max_tokens} (about {CHARS_PER_TOKEN} characters a token, \
             passage text only); {omitted} lower-ranked hit(s) omitted"
        ));
        return true;
    }
    false
}

// ─────────────────────────────────────────────────────────────────────────────
// Federated search
// ─────────────────────────────────────────────────────────────────────────────

/// The `search` fields that some index in `mapping` (a `GET /{pattern}/_mapping`
/// reply) maps.
pub fn mapped_search_fields(mapping: &Value) -> Vec<&'static str> {
    let mut present = std::collections::BTreeSet::new();
    if let Some(obj) = mapping.as_object() {
        for m in obj.values() {
            if let Some(props) = m.pointer("/mappings/properties").and_then(Value::as_object) {
                present.extend(props.keys().map(String::as_str));
            }
        }
    }
    SEARCH_FIELDS
        .into_iter()
        .filter(|f| present.contains(f))
        .collect()
}

/// The corpus a hub index belongs to: `xc-<slug>-b<digits>` followed by `-…` or
/// the end, with `<slug>` one of `known`.
///
/// The rule `xerj corpus index` uses to tell a stamped build from a dataset
/// named like one (`stamped_prefix` in `xerj-autoindex`, which is private and
/// driven by a name already in hand): digits only between `-b` and the next
/// `-`. Driven by the catalogue here, so slugs containing `-` resolve, and the
/// shortest slug that fits wins, which is deterministic.
pub fn slug_of_index<'a, V>(index: &str, known: &'a HashMap<String, V>) -> Option<&'a str> {
    let rest = index.strip_prefix("xc-")?;
    for (at, _) in rest.match_indices("-b") {
        let after = &rest[at + 2..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            continue;
        }
        let tail = &after[digits..];
        if !(tail.is_empty() || tail.starts_with('-')) {
            continue;
        }
        if let Some((slug, _)) = known.get_key_value(&rest[..at]) {
            return Some(slug.as_str());
        }
    }
    None
}

struct BestHit {
    slug: String,
    hit: Hit,
}

/// Group a `multi_match` reply by corpus: the best hit of each, best first
/// (ties by name), at most `want`.
///
/// A hit's index is attributed to a corpus by `known`, the WHOLE catalogue
/// (never a filtered subset: with `a` excluded and `a-b1` listed, the index
/// `xc-a-b1-b2-docs` is `a`'s, and attributing it to `a-b1` would serve an
/// excluded corpus's records under another name). What may then be served is
/// decided by `licence`, asked once per corpus: it returns the licence class and
/// SPDX id to serve the corpus under, or `None` to skip it. Hits from an index
/// that belongs to no listed corpus are skipped too.
fn group_by_corpus<F>(
    resp: &Value,
    known: &HashMap<String, CorpusRule>,
    want: usize,
    mut licence: F,
) -> Vec<BestHit>
where
    F: FnMut(&str, &CorpusRule) -> Option<(LicenceClass, String)>,
{
    let mut best: HashMap<&str, Hit> = HashMap::new();
    let mut licences: HashMap<&str, Option<(LicenceClass, String)>> = HashMap::new();
    let rows = resp
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    for row in rows {
        let Some(index) = row.get("_index").and_then(Value::as_str) else {
            continue;
        };
        let Some(slug) = slug_of_index(index, known) else {
            continue;
        };
        let Some(rule) = known.get(slug) else {
            continue;
        };
        let Some((class, spdx)) = licences
            .entry(slug)
            .or_insert_with(|| licence(slug, rule))
            .as_ref()
        else {
            continue;
        };
        if *class == LicenceClass::Excluded {
            continue;
        }
        let score = row.get("_score").and_then(Value::as_f64).unwrap_or(0.0) as f32;
        if best.get(slug).is_some_and(|h| h.score >= score) {
            continue;
        }
        let src = row.get("_source").unwrap_or(&Value::Null);
        // A definition record carries a `symbol`; anything else is a document
        // record. By shape, not by index name: a dataset directory may be
        // called `defs` too.
        let mut hit = if src.get("symbol").is_some_and(Value::is_string) {
            def_hit(src, score, EXCERPT_CHARS)
        } else {
            doc_hit(src, score)
        };
        if *class == LicenceClass::ApproachOnly {
            hit.licence_warning = Some(licence_note(spdx));
        }
        best.insert(slug, hit);
    }
    let mut out: Vec<BestHit> = best
        .into_iter()
        .map(|(slug, hit)| BestHit {
            slug: slug.to_string(),
            hit,
        })
        .collect();
    out.sort_by(|a, b| {
        b.hit
            .score
            .total_cmp(&a.hit.score)
            .then_with(|| a.slug.cmp(&b.slug))
    });
    out.truncate(want);
    out
}

/// A best hit from a definition record: the definition itself.
fn def_hit(src: &Value, score: f32, max_chars: usize) -> Hit {
    let s = |k: &str| {
        src.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Hit {
        file: s("file"),
        line: src
            .get("line")
            .and_then(Value::as_u64)
            .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX)),
        kind: s("kind"),
        text: truncate_chars(&s("text"), max_chars),
        value: src.get("value").and_then(Value::as_str).map(str::to_string),
        score,
        licence_warning: None,
    }
}

/// A best hit from a document record: where it is, and a short excerpt —
/// its definitions line list, else its text, else its title. Never the body.
fn doc_hit(src: &Value, score: f32) -> Hit {
    let (file, line) = split_locator(&passage::provenance(src));
    let excerpt = ["code", "defs", "text", "title"]
        .iter()
        .filter_map(|k| src.get(*k).and_then(Value::as_str))
        .find(|s| !s.trim().is_empty())
        .unwrap_or_default();
    Hit {
        file,
        line,
        kind: "passage".to_string(),
        text: truncate_chars(excerpt, EXCERPT_CHARS),
        value: None,
        score,
        licence_warning: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> AppState {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let mut config = xerj_common::config::Config::default();
        config.server.data_dir = dir.to_string_lossy().into_owned();
        config.storage.wal_sync = xerj_common::config::WalSync::Async;
        let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
        let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
        AppState::new(config, engine, metrics)
    }

    /// The pin is what makes the validated build the ONLY thing a `def`/`code`
    /// request can address, whatever the shared pipeline computes from its own
    /// second read of a ledger that may have changed since it was checked.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_node_pinned_to_a_build_refuses_every_expression_outside_it() {
        let state = test_state();
        let pinned = InProcessNode::new(&state, None, 1 << 20, None);
        pinned.pin_to_build("xc-a-b1");
        let unpinned = InProcessNode::new(&state, None, 1 << 20, None);

        tokio::task::spawn_blocking(move || {
            let refused =
                |r: &Result<Value, String>| matches!(r, Err(e) if e.starts_with("refused"));
            let outside = [
                "xc-*",
                "xc-",
                "xc-b-b1*",
                "xc-b-b1-defs",
                "xc-a-b1*,xc-b-b1*",
                "xc-b-b1-docs,xc-a-b1-docs",
                "xc-a",
                "xc-a-b",
            ];
            for expr in outside {
                assert!(refused(&pinned.search(expr, &json!({}))), "search {expr}");
                assert!(
                    pinned
                        .cat_indices_json(expr)
                        .is_err_and(|e| e.starts_with("refused")),
                    "cat {expr}"
                );
                assert!(
                    refused(&pinned.get_mapping(&format!("/{expr}/_mapping"))),
                    "mapping {expr}"
                );
                // Without the pin the same expressions are well-formed hub names:
                // the pin, not the character set, is what refused them.
                if expr != "xc-" {
                    assert!(
                        !refused(&unpinned.search(expr, &json!({}))),
                        "unpinned {expr} is a legal hub expression"
                    );
                }
            }
            // Inside the build: well-formed, so the request reaches the engine,
            // which has no such index here (a 404, not a refusal).
            for expr in ["xc-a-b1*", "xc-a-b1-defs", "xc-a-b1-docs,xc-a-b1-defs"] {
                let r = pinned.search(expr, &json!({}));
                assert!(!refused(&r), "{expr}: {r:?}");
                assert!(pinned.cat_indices_json(expr).is_ok(), "cat {expr}");
            }
        })
        .await
        .expect("blocking task");
    }

    /// Every overflow retry is a whole search that is thrown away, so a request
    /// gets a fixed number of them however many searches its pipeline makes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_request_has_a_fixed_budget_of_window_halvings() {
        let state = test_state();
        let node = InProcessNode::new(&state, None, 1 << 20, None);
        for n in 0..MAX_WINDOW_SHRINKS {
            assert!(node.take_shrink(), "halving {n} is within the budget");
        }
        assert!(!node.take_shrink(), "the budget is spent");
        assert!(!node.take_shrink(), "and stays spent");
        // Seven halvings are exactly what takes the widest window to one.
        assert_eq!(
            (0..MAX_WINDOW_SHRINKS).fold(200u64, |s, _| (s / 2).max(1)),
            1
        );
        // A fresh request starts with a full budget.
        let next = InProcessNode::new(&state, None, 1 << 20, None);
        assert!(next.take_shrink());
    }

    #[test]
    fn effective_licence_is_the_stricter_class_with_the_deciding_spdx() {
        use LicenceClass::{AdaptWithAttribution as Adapt, ApproachOnly as Approach, Excluded};
        let el = effective_licence;
        // Agreement.
        assert_eq!(el((Adapt, "MIT"), (Adapt, "MIT")), (Adapt, "MIT".into()));
        assert_eq!(
            el((Approach, "GPL-3.0-only"), (Approach, "GPL-3.0-only")),
            (Approach, "GPL-3.0-only".into())
        );
        // Either side can be the stricter one, in every pair.
        assert_eq!(
            el((Adapt, "MIT"), (Approach, "AGPL")),
            (Approach, "AGPL".into())
        );
        assert_eq!(
            el((Approach, "GPL"), (Adapt, "MIT")),
            (Approach, "GPL".into())
        );
        assert_eq!(
            el((Adapt, "MIT"), (Excluded, "Closed")),
            (Excluded, "Closed".into())
        );
        assert_eq!(
            el((Excluded, "Closed"), (Adapt, "MIT")),
            (Excluded, "Closed".into())
        );
        assert_eq!(
            el((Approach, "GPL"), (Excluded, "Closed")),
            (Excluded, "Closed".into())
        );
        assert_eq!(
            el((Excluded, "Closed"), (Approach, "GPL")),
            (Excluded, "Closed".into())
        );
        // The stricter side names no licence: the other side's id is used.
        assert_eq!(el((Approach, "GPL"), (Adapt, "")), (Approach, "GPL".into()));
        assert_eq!(el((Adapt, "MIT"), (Approach, "")), (Approach, "MIT".into()));
        // A tie takes the ledger's id, else the catalogue's.
        assert_eq!(
            el((Adapt, "MIT"), (Adapt, "Apache-2.0")),
            (Adapt, "Apache-2.0".into())
        );
        assert_eq!(el((Adapt, "MIT"), (Adapt, "")), (Adapt, "MIT".into()));
    }
}
