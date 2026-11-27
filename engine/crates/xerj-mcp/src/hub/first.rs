//! The no-node path of the `xerj_hub_*` tools (#2760, epic #2769): `xerj mcp`
//! answers a hub question with no node running, through
//! [`xerj_common::hub_client_cache`].
//!
//! # What changes, and when
//!
//! The tools in [`super`] run the `xerj ask` flow, which answers from packs
//! already on the machine and never downloads. With no node reachable and
//! nothing pulled, all it can do is name the `xerj hub pull` line. This module
//! is the other half, and it is **opt-in**: the operator sets
//! `XERJ_HUB_CACHE_DIR` (or `--hub-cache-dir <DIR>`). Without it nothing here
//! runs, and the published rule that a hub tool call never waits on the network
//! stays true.
//!
//! With it set, each hub tool call first probes the node it proxies to
//! ([`McpRuntimeMode::detect`], [`DEFAULT_PROBE_BUDGET_MS`]):
//!
//! * **A node answers** — the call runs the `xerj ask` flow, unchanged.
//! * **No node answers** — a definition lookup (`xerj_hub_def`, or
//!   `xerj_hub_ask` in `def`/`files` mode with a `corpus`) is answered by
//!   [`HubFirstRouter::query_corpus`]; `xerj_hub_find` reads the same cache's
//!   catalogue. The signed catalogue and the signed pack are fetched from the
//!   hub (`XERJ_HUB_URL`, default `https://hub.xerj.org`) once, verified
//!   (sha256, ed25519, `SHA256SUMS`, format version), and kept in a bounded LRU
//!   disk cache; the next call is served from disk.
//!
//! Three things stay with the `xerj ask` flow because they need a node or a
//! synced catalogue: passage (`how`) questions, a `xerj_hub_ask` that names no
//! corpus while no catalogue was synced, and any corpus already pulled with
//! `xerj hub pull` (local packs first).
//!
//! # How this squares with "a tool call never downloads"
//!
//! That rule exists so an agent's turn never blocks on the network and so a
//! tool call never deletes anything behind the agent's back. Under the opt-in:
//! every request has a connect and a total timeout; a pack over
//! [`AUTO_PULL_MAX_BYTES`] is refused with the `xerj hub pull` line, as the
//! auto-pull cap does for the CLI; the cache directory is the operator's own,
//! separate from `~/.xerj/hub/packs`, so eviction can only remove what this
//! path downloaded; and every answer says what it did (the hub-only notice, a
//! `fetched …` warning, and `_meta["xerj.org/hub_first"]` carrying the cache
//! outcome and the fetch / disk-hit counts).

use std::cell::OnceCell;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use xerj_common::hub::{render, AnswerMode, CatalogueEntry, AUTO_PULL_MAX_BYTES, DEFAULT_HUB_URL};
use xerj_common::hub_client_cache::{
    CacheLookupOutcome, Freshness, HubDiskCache, HubFirstRouter, HubModeError, HubQueryRequest,
    HubQueryResult, McpRuntimeMode, SignedDownload, DEFAULT_CACHE_TTL_SECS,
    DEFAULT_MCP_CACHE_MAX_BYTES, DEFAULT_PROBE_BUDGET_MS,
};
use xerj_hub::ask::{self, AskOutcome, FindOutcome, ModeRequest, PullPolicy};

use super::EvidenceRequest;
use crate::Ctx;

/// The environment variable that turns hub-first mode on and names its cache.
pub(crate) const CACHE_DIR_ENV: &str = "XERJ_HUB_CACHE_DIR";
/// The hub's base URL, shared with `xerj ask` / `xerj hub`.
const HUB_URL_ENV: &str = "XERJ_HUB_URL";
/// `_meta` key (MCP reserves unprefixed and `mcp`-prefixed ones) for the cache
/// facts of one answer.
const META_KEY: &str = "xerj.org/hub_first";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CATALOGUE_TIMEOUT: Duration = Duration::from_secs(30);
const PACK_TIMEOUT: Duration = Duration::from_secs(60);
/// The signed catalogue is one small file; a body past this is not it.
const CATALOGUE_MAX_BYTES: u64 = 32 * 1024 * 1024;
/// A detached ed25519 signature is 128 hex characters.
const SIG_MAX_BYTES: u64 = 4096;

/// The `--hub-cache-dir` value, set once by [`super::configure`].
static CACHE_DIR_FLAG: OnceLock<PathBuf> = OnceLock::new();

/// One hub-first call at a time in this process: two concurrent first calls for
/// one corpus would each download it. Other processes still race on the cache,
/// which is what its file lock is for.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

pub(crate) fn set_cache_dir_flag(dir: &str) {
    let _ = CACHE_DIR_FLAG.set(PathBuf::from(dir.trim()));
}

/// Where the cache lives and which hub it fetches from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Config {
    pub(crate) cache_dir: PathBuf,
    pub(crate) hub_url: String,
}

impl Config {
    /// The configuration this process runs with: `Ok(None)` when hub-first is
    /// not turned on, `Err` when it is but cannot work (a relative path, a hub
    /// URL that is not http(s)) — loud, not a silent fallback to the old flow.
    pub(crate) fn from_process() -> Result<Option<Self>, String> {
        Self::resolve(CACHE_DIR_FLAG.get().map(PathBuf::as_path), &|key| {
            std::env::var(key).ok()
        })
    }

    fn resolve(
        flag: Option<&Path>,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, String> {
        let (dir, source) = match flag {
            Some(dir) => (dir.to_path_buf(), "--hub-cache-dir"),
            None => match env(CACHE_DIR_ENV)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
            {
                Some(dir) => (PathBuf::from(dir), CACHE_DIR_ENV),
                None => return Ok(None),
            },
        };
        if !dir.is_absolute() {
            return Err(format!(
                "{source} must be an absolute path, got '{}': an MCP host starts this server in \
                 a directory of its own choosing",
                dir.display()
            ));
        }
        let hub_url = env(HUB_URL_ENV)
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| DEFAULT_HUB_URL.to_string());
        if !(hub_url.starts_with("https://") || hub_url.starts_with("http://")) {
            return Err(format!(
                "{HUB_URL_ENV} must be an http or https URL, got '{hub_url}'"
            ));
        }
        Ok(Some(Self {
            cache_dir: dir,
            hub_url,
        }))
    }
}

/// A hub-first call that found no node: its configuration and the mode
/// detected, which the answer's notice and `_meta` report.
pub(super) struct Active {
    cfg: Config,
    mode: McpRuntimeMode,
}

/// `Some` when this call should run through the cache: hub-first is on, no node
/// answers at `ctx.base_url` within the probe budget, and `slug` (when given)
/// is not a pack already pulled to this machine. `Err` for a configuration
/// that cannot work.
pub(super) async fn active(ctx: &Ctx, slug: Option<&str>) -> Result<Option<Active>, String> {
    let Some(cfg) = Config::from_process()? else {
        return Ok(None);
    };
    let base_url = ctx.base_url.clone();
    let slug = slug.map(str::to_string);
    tokio::task::spawn_blocking(move || {
        let mode = McpRuntimeMode::detect(
            &base_url,
            &cfg.hub_url,
            DEFAULT_PROBE_BUDGET_MS,
            probe_local_node,
        );
        if mode.has_local_node() || slug.as_deref().is_some_and(pulled_locally) {
            return None;
        }
        Some(Active { cfg, mode })
    })
    .await
    .map_err(|e| format!("join error: {e}"))
}

/// Latency in milliseconds when a node accepts a connection within `budget_ms`.
/// Clamped to the budget: the connect timeout already decided it was in time.
fn probe_local_node(base_url: &str, budget_ms: u64) -> Option<u64> {
    let started = Instant::now();
    let accepted = xerj_common::localnode::port_accepts(
        &xerj_common::localnode::url_host(base_url),
        xerj_common::localauth::xerj_url_port(base_url),
        Duration::from_millis(budget_ms),
    );
    accepted.then(|| {
        u64::try_from(started.elapsed().as_millis()).map_or(budget_ms, |ms| ms.min(budget_ms))
    })
}

/// Local packs first: a corpus `xerj hub pull` put on this machine is answered
/// by the `xerj ask` flow, which also answers its passage questions.
fn pulled_locally(slug: &str) -> bool {
    ask::cached_slugs(&xerj_hub::lockfile::default_hub_root())
        .iter()
        .any(|s| s == slug)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ── what to ask ──────────────────────────────────────────────────────────────

/// One lookup in one corpus.
pub(super) struct Query {
    slug: String,
    text: String,
    mode: AnswerMode,
    max_hits: usize,
    /// A line the answer carries as a warning, for what the mode cannot do here.
    note: Option<String>,
}

impl Query {
    /// `xerj_hub_def`: one definition; `k` hits (default 5, as the flow's).
    pub(super) fn def(slug: &str, symbol: &str, k: Option<usize>) -> Self {
        Self {
            slug: slug.to_string(),
            text: symbol.to_string(),
            mode: AnswerMode::Def,
            max_hits: k.unwrap_or(xerj_hub::def::DEFAULT_K),
            note: None,
        }
    }

    /// `xerj_hub_ask` in files mode. A pack archive holds the file list and no
    /// passages, so without a node the question can only be a path fragment:
    /// one word filters, a sentence lists the first files, and the answer says
    /// which it did.
    fn files(slug: &str, question: &str) -> Self {
        let q = question.trim();
        let (text, how) = if q.contains(char::is_whitespace) {
            (String::new(), "the question was not used")
        } else {
            (q.to_string(), "the question was used as a path fragment")
        };
        Self {
            slug: slug.to_string(),
            text,
            mode: AnswerMode::Files,
            max_hits: ask::FILES_K,
            note: Some(format!(
                "no node is running, so the file list is filtered by path fragment and not ranked \
                 by the question ({how})"
            )),
        }
    }
}

/// What `xerj_hub_ask` does in this call.
pub(super) enum AskPlan {
    /// The `xerj ask` flow, unchanged.
    Flow,
    /// Hub-first cannot route a question that names no corpus.
    NeedsCorpus(String),
    /// Answer through the cache.
    HubFirst(Active, Query),
}

/// Decide how `xerj_hub_ask` runs. Only a definition or file-list question that
/// names its `corpus` goes through the cache; see the module docs for the rest.
pub(super) async fn plan_ask(
    ctx: &Ctx,
    question: &str,
    corpus: Option<&str>,
    mode: &ModeRequest,
) -> Result<AskPlan, String> {
    let (answer_mode, symbol) = ask::detect_mode(question, mode);
    let Some(corpus) = corpus else {
        if active(ctx, None).await?.is_none() {
            return Ok(AskPlan::Flow);
        }
        // The flow can still route when a catalogue was synced with `xerj hub sync`.
        let synced = tokio::task::spawn_blocking(|| {
            ask::load_cached_catalogue(&xerj_hub::lockfile::default_hub_root()).is_ok()
        })
        .await
        .map_err(|e| format!("join error: {e}"))?;
        return Ok(if synced {
            AskPlan::Flow
        } else {
            AskPlan::NeedsCorpus(
                "no node is reachable and no catalogue was synced (`xerj hub sync`), so a \
                 question cannot be routed to a corpus: call xerj_hub_find to list candidates, \
                 then call xerj_hub_ask again with the `corpus` argument and `mode` def or files \
                 (passage questions need a node)"
                    .to_string(),
            )
        });
    };
    let query = match answer_mode {
        AnswerMode::Def => Query::def(
            corpus,
            &symbol.unwrap_or_else(|| question.to_string()),
            None,
        ),
        AnswerMode::Files => Query::files(corpus, question),
        AnswerMode::How => return Ok(AskPlan::Flow),
    };
    Ok(match active(ctx, Some(corpus)).await? {
        Some(active) => AskPlan::HubFirst(active, query),
        None => AskPlan::Flow,
    })
}

// ── running a query ──────────────────────────────────────────────────────────

struct Done {
    result: HubQueryResult,
    network_fetches: u64,
    disk_hits: u64,
}

fn open_cache(cfg: &Config) -> Result<HubDiskCache, HubModeError> {
    HubDiskCache::open(
        &cfg.cache_dir,
        DEFAULT_MCP_CACHE_MAX_BYTES,
        DEFAULT_CACHE_TTL_SECS,
    )
}

fn query_blocking(active: &Active, query: &Query) -> Result<Done, HubModeError> {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let cache = open_cache(&active.cfg)?;
    let http = LazyHttp::new(&active.cfg.hub_url);
    let mut router = HubFirstRouter::new(active.mode.clone(), cache);
    let request = HubQueryRequest::new(&query.slug, &query.text, query.mode, unix_now())
        .with_max_hits(query.max_hits);
    let result = router.query_corpus(request, || http.catalogue(), |entry| http.pack(entry))?;
    Ok(Done {
        result,
        network_fetches: router.cache().network_fetches(),
        disk_hits: router.cache().disk_hits(),
    })
}

/// Answer `query` through the cache and shape it as the tool result the
/// `xerj ask` flow would return, plus `_meta`. A failure is the
/// [`HubModeError`]'s own sentence: unknown corpus, unsupported format,
/// signature or digest mismatch, a hub that cannot be reached — never an empty
/// answer in their place.
pub(super) async fn answer(
    active: Active,
    query: Query,
    question: String,
    max_tokens: Option<usize>,
    evidence: Option<EvidenceRequest>,
) -> Result<Value, String> {
    let hub_url = active.cfg.hub_url.clone();
    let note = query.note.clone();
    let done = tokio::task::spawn_blocking(move || query_blocking(&active, &query))
        .await
        .map_err(|e| format!("join error: {e}"))?
        .map_err(|e| describe(&e))?;
    let Done {
        result,
        network_fetches,
        disk_hits,
    } = done;
    let fetched = result.cache_outcome == CacheLookupOutcome::FetchedAndCached;
    let mut answer = result.answer;
    if fetched {
        answer.warnings.push(format!(
            "fetched corpus '{}' (pin {}) from {hub_url} into the hub disk cache; later calls are \
             served from disk",
            answer.corpus,
            answer.pin_sha.get(..8).unwrap_or(&answer.pin_sha),
        ));
    }
    answer.warnings.extend(note);
    if let Some(n) = max_tokens {
        ask::cap_tokens(&mut answer, n);
    }
    super::auto_review(&answer, &question);
    let outcome = AskOutcome {
        exit: 0,
        stdout: render(&answer),
        stderr: Vec::new(),
        answer: Some(answer),
    };
    let mut value = super::outcome_result(outcome, &question, evidence);
    value["_meta"] = meta(
        &result.runtime_mode,
        result.cache_outcome,
        &result.freshness,
        network_fetches,
        disk_hits,
        &result.evicted_slugs,
    );
    Ok(value)
}

/// `xerj_hub_find` from the cache's catalogue, fetched when missing or stale.
pub(super) async fn find(active: Active, question: String, k: usize) -> Result<Value, String> {
    let (outcome, notes, cache_meta) =
        tokio::task::spawn_blocking(move || find_blocking(&active, &question, k))
            .await
            .map_err(|e| format!("join error: {e}"))?
            .map_err(|e| describe(&e))?;
    let mut text = String::new();
    for line in &notes {
        text.push_str(line);
        text.push('\n');
    }
    text.push_str(&ask::render_find(&outcome));
    Ok(json!({
        "content": [ { "type": "text", "text": text } ],
        "structuredContent": outcome,
        "isError": false,
        "_meta": cache_meta,
    }))
}

fn find_blocking(
    active: &Active,
    question: &str,
    k: usize,
) -> Result<(FindOutcome, Vec<String>, Value), HubModeError> {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut cache = open_cache(&active.cfg)?;
    let http = LazyHttp::new(&active.cfg.hub_url);
    let resolved = cache.get_or_fetch_catalogue(unix_now(), || http.catalogue())?;
    let mut notes: Vec<String> = active
        .mode
        .fallback_notice()
        .map(str::to_string)
        .into_iter()
        .collect();
    notes.extend(resolved.warnings("catalogue"));
    if resolved.outcome == CacheLookupOutcome::FetchedAndCached {
        notes.push(format!(
            "fetched the signed catalogue from {} into the hub disk cache",
            active.cfg.hub_url
        ));
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let hub_home = xerj_hub::lockfile::default_hub_root();
    let mut outcome = ask::find(question, k, &cwd, &hub_home, &resolved.value);
    for corpus in &mut outcome.corpora {
        corpus.cached |= cache.contains_corpus(&corpus.slug);
    }
    let cache_meta = meta(
        &active.mode,
        resolved.outcome,
        &resolved.freshness,
        cache.network_fetches(),
        cache.disk_hits(),
        &resolved.evicted_slugs,
    );
    Ok((outcome, notes, cache_meta))
}

fn meta(
    mode: &McpRuntimeMode,
    outcome: CacheLookupOutcome,
    freshness: &Freshness,
    network_fetches: u64,
    disk_hits: u64,
    evicted: &[String],
) -> Value {
    json!({
        (META_KEY): {
            "runtime_mode": mode,
            "cache_outcome": outcome,
            "freshness": freshness,
            "network_fetches": network_fetches,
            "disk_hits": disk_hits,
            "evicted_slugs": evicted,
        }
    })
}

/// The error's own sentence; an unknown corpus also says where to look.
fn describe(error: &HubModeError) -> String {
    match error {
        HubModeError::UnknownCorpus { .. } => {
            format!("{error}; call xerj_hub_find to see which corpora the hub serves")
        }
        other => other.to_string(),
    }
}

// ── fetching ─────────────────────────────────────────────────────────────────

/// The hub over HTTP(S), with the client built on first use: a call served from
/// disk never builds one. No `Authorization` header is ever sent: this is the
/// hub, not the node, and the node's key must not leave the machine.
struct LazyHttp<'a> {
    hub_url: &'a str,
    client: OnceCell<Result<reqwest::blocking::Client, String>>,
}

impl<'a> LazyHttp<'a> {
    fn new(hub_url: &'a str) -> Self {
        Self {
            hub_url,
            client: OnceCell::new(),
        }
    }

    fn client(&self) -> Result<&reqwest::blocking::Client, HubModeError> {
        self.client
            .get_or_init(|| {
                let mut builder = reqwest::blocking::Client::builder()
                    .connect_timeout(CONNECT_TIMEOUT)
                    .redirect(reqwest::redirect::Policy::limited(5));
                // A loopback hub (a test, a mirror on this machine) is never
                // reached through HTTP(S)_PROXY, as the node client's rule.
                if xerj_common::localauth::url_is_loopback(self.hub_url) {
                    builder = builder.no_proxy();
                }
                builder.build().map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(|reason| HubModeError::HubFetch {
                target: self.hub_url.to_string(),
                reason: reason.clone(),
            })
    }

    /// The signed catalogue and its detached signature.
    fn catalogue(&self) -> Result<SignedDownload, HubModeError> {
        let url = format!(
            "{}/{}",
            self.hub_url.trim_end_matches('/'),
            xerj_hub::catalogue::CATALOGUE_API_PATH
        );
        let bytes = self.get(&url, CATALOGUE_MAX_BYTES, CATALOGUE_TIMEOUT)?;
        // A static host answers an unknown path with its HTML page and a 200.
        if bytes.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
            return Err(HubModeError::HubFetch {
                target: ask::printable(&url),
                reason: format!(
                    "not a signed catalogue ({} bytes that do not start a JSON object); no \
                     catalogue is served at this address",
                    bytes.len()
                ),
            });
        }
        Ok(SignedDownload {
            signature_hex: self.get_text(&format!("{url}.sig"))?,
            bytes,
        })
    }

    /// One pack and its detached signature, within the auto-pull cap.
    fn pack(&self, entry: &CatalogueEntry) -> Result<SignedDownload, HubModeError> {
        let slug = ask::printable(&entry.slug);
        match ask::pull_policy(entry) {
            PullPolicy::Allow => {}
            PullPolicy::TooLarge => {
                return Err(HubModeError::HubFetch {
                    target: slug.clone(),
                    reason: format!(
                        "the pack is {} MB, over the {} MB a tool call downloads on its own; \
                         run `xerj hub pull {slug}` to fetch it once, then ask again",
                        ask::megabytes(entry.pack_bytes),
                        ask::megabytes(AUTO_PULL_MAX_BYTES),
                    ),
                })
            }
            PullPolicy::Excluded => {
                return Err(HubModeError::ExcludedCorpusLicence {
                    slug: entry.slug.clone(),
                    licence_spdx: entry.licence_spdx.clone(),
                })
            }
        }
        // `pack_bytes` is part of the signed catalogue: a body past it is not
        // the pack, and the read stops there.
        let bytes = self.get(&entry.pack_url, entry.pack_bytes, PACK_TIMEOUT)?;
        Ok(SignedDownload {
            signature_hex: self.get_text(&entry.sig_url)?,
            bytes,
        })
    }

    fn get_text(&self, url: &str) -> Result<String, HubModeError> {
        let bytes = self.get(url, SIG_MAX_BYTES, CATALOGUE_TIMEOUT)?;
        String::from_utf8(bytes)
            .map(|s| s.trim().to_string())
            .map_err(|_| HubModeError::HubFetch {
                target: ask::printable(url),
                reason: "the signature is not UTF-8 text".to_string(),
            })
    }

    /// `GET url`, refusing anything over `max_bytes` (both by `Content-Length`
    /// and by a bounded read, so a missing or lying length cannot fill memory).
    fn get(&self, url: &str, max_bytes: u64, timeout: Duration) -> Result<Vec<u8>, HubModeError> {
        let fail = |reason: String| HubModeError::HubFetch {
            target: ask::printable(url),
            reason,
        };
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(fail("only http and https URLs are fetched".to_string()));
        }
        let response = self
            .client()?
            .get(url)
            .timeout(timeout)
            .send()
            .map_err(|e| fail(error_chain(&e)))?;
        let status = response.status();
        if !status.is_success() {
            return Err(fail(format!("HTTP {status}")));
        }
        if let Some(len) = response.content_length() {
            if len > max_bytes {
                return Err(fail(format!(
                    "advertises {len} bytes, over the {max_bytes}-byte cap"
                )));
            }
        }
        let mut body = Vec::new();
        response
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut body)
            .map_err(|e| fail(format!("read body: {e}")))?;
        if body.len() as u64 > max_bytes {
            return Err(fail(format!("the body is over the {max_bytes}-byte cap")));
        }
        Ok(body)
    }
}

/// `error: cause: root cause`, each link once.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let line = cause.to_string();
        if !text.contains(&line) {
            text.push_str(": ");
            text.push_str(&line);
        }
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn abs(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    #[test]
    fn hub_first_is_off_unless_a_cache_dir_is_configured() {
        assert_eq!(Config::resolve(None, &env(&[])).unwrap(), None);
        // An empty value is the same as unset, as every other XERJ_* variable.
        assert_eq!(
            Config::resolve(None, &env(&[(CACHE_DIR_ENV, "  ")])).unwrap(),
            None
        );
        // A hub URL alone does not turn it on: `xerj ask` reads that too.
        assert_eq!(
            Config::resolve(None, &env(&[(HUB_URL_ENV, "http://hub.test")])).unwrap(),
            None
        );
    }

    #[test]
    fn the_flag_wins_over_the_environment_and_the_hub_url_is_normalised() {
        let flag = abs("flag-cache");
        let from_env = abs("env-cache");
        let e = env(&[
            (CACHE_DIR_ENV, from_env.to_str().unwrap()),
            (HUB_URL_ENV, " http://hub.test/ "),
        ]);
        let cfg = Config::resolve(Some(&flag), &e).unwrap().unwrap();
        assert_eq!(cfg.cache_dir, flag);
        assert_eq!(cfg.hub_url, "http://hub.test");
        let cfg = Config::resolve(None, &e).unwrap().unwrap();
        assert_eq!(cfg.cache_dir, from_env);
        let cfg = Config::resolve(None, &env(&[(CACHE_DIR_ENV, from_env.to_str().unwrap())]))
            .unwrap()
            .unwrap();
        assert_eq!(cfg.hub_url, DEFAULT_HUB_URL);
    }

    #[test]
    fn a_configuration_that_cannot_work_is_an_error_not_the_old_flow() {
        let relative = Config::resolve(None, &env(&[(CACHE_DIR_ENV, "cache")])).unwrap_err();
        assert!(relative.contains("absolute path"), "{relative}");
        let relative = Config::resolve(Some(Path::new("cache")), &env(&[])).unwrap_err();
        assert!(relative.starts_with("--hub-cache-dir"), "{relative}");
        let dir = abs("cache");
        let bad_url = Config::resolve(
            None,
            &env(&[
                (CACHE_DIR_ENV, dir.to_str().unwrap()),
                (HUB_URL_ENV, "ftp://hub.test"),
            ]),
        )
        .unwrap_err();
        assert!(bad_url.contains("http or https"), "{bad_url}");
    }

    #[test]
    fn a_listening_port_is_a_node_and_a_closed_one_is_not() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let latency = probe_local_node(&url, DEFAULT_PROBE_BUDGET_MS).expect("node accepts");
        assert!(latency <= DEFAULT_PROBE_BUDGET_MS);
        drop(listener);
        assert_eq!(probe_local_node(&url, DEFAULT_PROBE_BUDGET_MS), None);
    }

    #[test]
    fn a_file_question_is_a_path_fragment_only_and_says_so() {
        let one_word = Query::files("lz4", " lib/ ");
        assert_eq!(one_word.text, "lib/");
        assert_eq!(one_word.max_hits, ask::FILES_K);
        assert!(one_word.note.as_deref().unwrap().contains("path fragment"));
        let sentence = Query::files("lz4", "which files hash the buffer?");
        assert_eq!(sentence.text, "", "a sentence is not a path");
        assert!(sentence.note.as_deref().unwrap().contains("not used"));
    }

    #[test]
    fn a_definition_lookup_defaults_to_the_flows_hit_count() {
        assert_eq!(
            Query::def("lz4", "X", None).max_hits,
            xerj_hub::def::DEFAULT_K
        );
        assert_eq!(Query::def("lz4", "X", Some(7)).max_hits, 7);
    }

    #[test]
    fn an_unknown_corpus_error_points_at_xerj_hub_find() {
        let unknown = HubModeError::UnknownCorpus {
            slug: "nope".into(),
            catalogue_generated_at: "2026-11-25T00:00:00Z".into(),
        };
        let text = describe(&unknown);
        assert!(text.contains("unknown hub corpus 'nope'"), "{text}");
        assert!(text.contains("xerj_hub_find"), "{text}");
        assert!(!describe(&HubModeError::NoHubKey).contains("xerj_hub_find"));
    }

    #[test]
    fn a_pack_over_the_auto_pull_cap_is_refused_before_any_request() {
        let entry = CatalogueEntry {
            slug: "huge".into(),
            domain: "NET".into(),
            topics: vec![],
            ask_for: String::new(),
            upstream: String::new(),
            pin_sha: "a".repeat(40),
            pin_date: "2026-11-01".into(),
            refresh_cadence_days: 7,
            licence_spdx: "MIT".into(),
            licence_class: xerj_common::hub::LicenceClass::AdaptWithAttribution,
            pack_bytes: AUTO_PULL_MAX_BYTES + 1,
            // Nothing listens here: reaching it would be a transport error, not
            // the policy sentence asserted below.
            pack_url: "http://127.0.0.1:9/huge.hubpack.tar.zst".into(),
            pack_sha256: "0".repeat(64),
            sig_url: "http://127.0.0.1:9/huge.hubpack.tar.zst.sig".into(),
            dependency_names: vec![],
            quality_grade: None,
            term_sketch: xerj_hub::sketch::build(["x".to_string()]),
        };
        let http = LazyHttp::new("http://127.0.0.1:9");
        let err = http.pack(&entry).unwrap_err().to_string();
        assert!(err.contains("xerj hub pull huge"), "{err}");
        assert!(err.contains("over the 50.0 MB"), "{err}");
    }
}
