//! Thin blocking ES-compat client (reqwest). Retries with exponential
//! backoff on 429/5xx/transport errors; parses per-item bulk errors; and
//! lets a 429 shrink how much load the run offers, not just when it offers
//! it ([`BulkAdmission`]).

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct Es {
    base: String,
    http: reqwest::blocking::Client,
    api_key: Option<String>,
    bulk_timeout: Duration,
    retry_initial_delay: Duration,
    retry_max_delay: Duration,
    /// Shared by every clone of this client on purpose: congestion is a
    /// property of the server, so all of a run's workers must see the same
    /// admission limit.
    admission: Arc<BulkAdmission>,
    /// Every delay this client has actually backed off for, in order.
    /// Test-only, and per-instance rather than global so a concurrent test
    /// cannot perturb it — `--test-threads=2` in CI is exactly the shape that
    /// would.
    ///
    /// The DELAYS and not a count. Counting alone pins the arity and nothing
    /// else, so removing the doubling, ignoring `retry_max_delay`, or deleting
    /// the sleep outright while still counting it all pass — the last turning
    /// this into a six-shot hot loop against a struggling server, which is the
    /// retry storm the doc comment on `with_retry` exists to prevent. The
    /// sequence pins the arity, the ordering, the doubling and the cap at once.
    /// It replaces a wall-clock bound that sat 20 ms above a 240 ms budget and
    /// failed about one run in five under load (#436).
    /// `(requested, observed)` per backoff, in order. Shared by every clone of
    /// this client, like `admission` — a clone is the same client.
    #[cfg(test)]
    backoff_delays: Arc<std::sync::Mutex<Vec<(Duration, Duration)>>>,
}

/// How many bulk requests a run may have in flight, and how a 429 changes
/// that number (#240 §8).
///
/// Before this, backpressure handling was transport-only: a 429 slept
/// 250 ms..8 s and then re-offered exactly the same load from exactly as
/// many workers, six times, before failing the file. The sleep is the right
/// *first* move and is kept; what was missing is the feedback edge — the
/// signal never reached the thing generating the load.
///
/// The rule is AIMD, the same shape TCP uses and for the same reason:
/// congestion is expensive and must be answered immediately, while probing
/// for recovered capacity must be slow enough not to re-cause it.
///
/// * **Multiplicative decrease** — a 429 halves the limit at once, floor 1.
///   Further 429s inside [`SHRINK_COOLDOWN`] are the same congestion event
///   seen by other workers, so they do not compound: without that damping,
///   8 workers × 6 retries would collapse the limit to 1 on a single
///   momentary stall.
/// * **Additive increase** — after [`RECOVER_AFTER`] consecutive clean bulks
///   the limit rises by one, never above the ceiling the operator asked for
///   with `--workers`. Recovery is a probe, not a reset: a run that meets
///   sustained pressure stays small for the rest of the run.
///
/// Honest scope: no peer engine does this. quickwit's rest client sleeps
/// 500 ms and retries the same batch
/// (`quickwit-rest-client/src/rest_client.rs:371-380`), meilisearch's REST
/// embedder classifies 429 as retry-later
/// (`crates/milli/src/vector/embedder/rest.rs:550`), and its S3 path treats
/// it as a transient backoff (`.../enterprise_edition/s3.rs:447`). All three
/// are the transport half XERJ already had. The AIMD shape is adapted from
/// congestion control, not copied from a search engine.
pub struct BulkAdmission {
    inner: Mutex<AdmissionState>,
    space: Condvar,
    /// Upper bound on the limit — what the operator asked for. `0` means the
    /// gate is off entirely (probes and one-shot clients).
    ceiling: usize,
    congestion_events: AtomicU64,
    /// Whether limit changes are announced on stderr. Off for `--quiet` and
    /// for clients that are not a user-visible run.
    announce: bool,
}

/// One congestion event may be reported by every worker that was in flight
/// when the server pushed back; they must count once.
const SHRINK_COOLDOWN: Duration = Duration::from_secs(1);
/// Clean bulks required before the limit is probed upward by one.
const RECOVER_AFTER: usize = 8;

struct AdmissionState {
    limit: usize,
    in_flight: usize,
    ok_streak: usize,
    last_shrink: Option<Instant>,
}

/// A slot in the bulk admission window, returned to the pool on drop —
/// including on the error and panic paths, which is why it is a guard and
/// not a pair of calls.
pub struct BulkPermit<'a> {
    admission: Option<&'a BulkAdmission>,
}

impl Drop for BulkPermit<'_> {
    fn drop(&mut self) {
        if let Some(admission) = self.admission {
            let mut state = admission.inner.lock().unwrap();
            state.in_flight = state.in_flight.saturating_sub(1);
            drop(state);
            admission.space.notify_one();
        }
    }
}

impl BulkAdmission {
    /// A gate that never blocks and never shrinks — the behaviour of every
    /// client that is not a bulk-loading run.
    pub fn off() -> Self {
        Self::new(0, false)
    }

    /// `ceiling` is the operator's concurrency (`--workers`); `0` disables.
    pub fn new(ceiling: usize, announce: bool) -> Self {
        Self {
            inner: Mutex::new(AdmissionState {
                limit: ceiling,
                in_flight: 0,
                ok_streak: 0,
                last_shrink: None,
            }),
            space: Condvar::new(),
            ceiling,
            congestion_events: AtomicU64::new(0),
            announce,
        }
    }

    fn enabled(&self) -> bool {
        self.ceiling > 0
    }

    /// Current admission limit; equals the ceiling until the server pushes
    /// back. `0` when the gate is off.
    pub fn limit(&self) -> usize {
        if !self.enabled() {
            return 0;
        }
        self.inner.lock().unwrap().limit
    }

    /// How many distinct congestion events this run has answered.
    pub fn congestion_events(&self) -> u64 {
        self.congestion_events.load(Ordering::Relaxed)
    }

    /// Block until a bulk slot is free. Cheap and non-blocking when off.
    fn acquire(&self) -> BulkPermit<'_> {
        if !self.enabled() {
            return BulkPermit { admission: None };
        }
        let mut state = self.inner.lock().unwrap();
        while state.in_flight >= state.limit {
            state = self.space.wait(state).unwrap();
        }
        state.in_flight += 1;
        BulkPermit {
            admission: Some(self),
        }
    }

    /// The server said 429. Halve the offered load, once per event.
    fn on_congestion(&self) {
        if !self.enabled() {
            return;
        }
        let mut state = self.inner.lock().unwrap();
        let now = Instant::now();
        if state
            .last_shrink
            .is_some_and(|last| now.duration_since(last) < SHRINK_COOLDOWN)
        {
            return;
        }
        state.last_shrink = Some(now);
        state.ok_streak = 0;
        self.congestion_events.fetch_add(1, Ordering::Relaxed);
        let previous = state.limit;
        state.limit = (previous / 2).max(1);
        if state.limit != previous && self.announce {
            eprintln!(
                "autoindex: server pushed back (HTTP 429); lowering bulk concurrency \
                 {previous} → {} for this run",
                state.limit
            );
        }
    }

    /// A bulk was accepted. Probe upward once a clean streak says the
    /// server has room again.
    fn on_success(&self) {
        if !self.enabled() {
            return;
        }
        let mut state = self.inner.lock().unwrap();
        if state.limit >= self.ceiling {
            return;
        }
        state.ok_streak += 1;
        if state.ok_streak < RECOVER_AFTER {
            return;
        }
        state.ok_streak = 0;
        let previous = state.limit;
        state.limit = (previous + 1).min(self.ceiling);
        if self.announce {
            eprintln!(
                "autoindex: server healthy again; raising bulk concurrency {previous} → {}",
                state.limit
            );
        }
        drop(state);
        self.space.notify_one();
    }
}

pub struct BulkOutcome {
    pub item_errors: u64,
    /// Per-item backend/admission failures — 5xx/429 statuses plus
    /// cluster/index write blocks recognised by TYPE (see
    /// [`is_index_block_error`]). These are not bad source records: the same
    /// record indexes fine once the server-side condition clears. Callers
    /// must not journal the source file complete.
    pub server_errors: u64,
    pub first_error: Option<String>,
    pub first_server_error: Option<String>,
}

/// Whether a per-item bulk `error` object reports a cluster/index write
/// block (e.g. `read_only_allow_delete` at the disk flood-stage watermark,
/// or an explicit `index.blocks.write`).
///
/// Blocks must be recognised by error TYPE and wording, never by HTTP
/// status: Elasticsearch maps explicit/API blocks to 403 FORBIDDEN and only
/// the flood-stage block to 429 (IndexMetadata block constants), and XERJ
/// mirrors that split, so a status-only classifier files a 403 block under
/// "bad source record". That is issue #195: every rejected document was
/// counted as junk, the source file was journaled complete, and the
/// instructed rerun then resumed past the journal and reported success over
/// an empty index. ES itself carries retryability on the block, not the
/// status (ClusterBlockException::retryable) — the type is the contract.
///
/// Matches (verified against live responses):
///   - XERJ:  `{"type":"engine_exception","reason":"index [x] is blocked
///     for write operations","status":403}`
///   - ES:    `{"type":"cluster_block_exception","reason":"index [x]
///     blocked by: [FORBIDDEN/8/index write (api)];"}`
///
/// A reason merely *containing* "blocked" could in principle be a
/// field-value echo in a mapping error; misclassifying that direction is
/// the safe one — the file is retried instead of silently dropped.
fn is_index_block_error(error: &Value) -> bool {
    let type_is_block = error
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t.contains("cluster_block") || t.contains("index_block"));
    let reason_is_block = error
        .get("reason")
        .and_then(Value::as_str)
        .is_some_and(|reason| reason.contains("blocked"));
    type_is_block || reason_is_block
}

/// How much of a 429/5xx body [`server_reason`] keeps. A 5xx body can be a
/// whole stack trace; the sentence that names the condition is at the front.
const SERVER_REASON_MAX: usize = 512;

/// The server's own explanation for a 429/5xx, bounded to a prefix.
///
/// [`Es::with_retry`] used to report only `HTTP 500 Internal Server Error` and
/// drop the body, which is the same sentence for a poisoned index, a full disk
/// and a panicking handler. That is why #345 was filed as "not investigated":
/// the reporter had a status line and nothing to act on, while the response
/// they discarded carried the reason — every by-query refusal answers
/// `{"error": {"type", "reason"}, "status"}` (`es_compat::by_query_response`),
/// and `_bulk` already surfaces the per-item half of the same thing
/// ([`BulkOutcome::first_server_error`]).
///
/// `error.type: error.reason` is preferred because that is the shape both XERJ
/// and Elasticsearch answer with; the raw body is the fallback for anything
/// else (an HTML proxy error page, a bare string), because a reverse proxy in
/// front of the server is exactly the case where the status alone is useless.
fn server_reason(resp: reqwest::blocking::Response) -> Option<String> {
    let body = resp.text().ok()?;
    let reason = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| {
            let error = value.get("error")?;
            let kind = error.get("type").and_then(Value::as_str);
            let why = error.get("reason").and_then(Value::as_str);
            match (kind, why) {
                (Some(kind), Some(why)) => Some(format!("{kind}: {why}")),
                (Some(only), None) | (None, Some(only)) => Some(only.to_owned()),
                (None, None) => None,
            }
        })
        .unwrap_or(body);
    // `chars`, not bytes: the body can be any encoding the server chose, and
    // slicing one mid-codepoint would panic on the error path (#326).
    let reason: String = reason.trim().chars().take(SERVER_REASON_MAX).collect();
    (!reason.is_empty()).then_some(reason)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingExecutionIdentity {
    pub version: u32,
    pub backend: String,
    pub identity_sha256: String,
    /// Absent for backends whose vector width the server does not pin
    /// (`proxy`, and a `neural` node that could not resolve its model assets;
    /// a resolved `neural` node reports its `config.json` `hidden_size` since
    /// #487). An absent width must not be read as 384.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<usize>,
    pub semantic_contract: String,
    pub resumable: bool,
    #[serde(default)]
    pub non_resumable_reason: Option<String>,
}

/// Ensure an autoindex index-create body pins the index to a single WAL shard.
///
/// autoindex creates one index per inferred dataset — hundreds for a large
/// repo. Each index otherwise opens one WAL file per ingest shard (a count that
/// scales with the server's CPU cores), so hundreds of indices exhaust the
/// process file-descriptor limit — fatal on macOS, whose default soft limit is
/// 256. `index.xerj_ingest_shards = 1` keeps each index at a single WAL fd.
/// Non-destructive: only fills the key when absent.
fn with_single_wal_shard(body: &Value) -> Value {
    let mut body = body.clone();
    if let Some(obj) = body.as_object_mut() {
        let settings = obj
            .entry("settings")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(sobj) = settings.as_object_mut() {
            let index = sobj.entry("index").or_insert_with(|| serde_json::json!({}));
            if let Some(iobj) = index.as_object_mut() {
                iobj.entry("xerj_ingest_shards")
                    .or_insert_with(|| serde_json::json!(1));
            }
        }
    }
    body
}

impl Es {
    pub fn new(url: &str, api_key: Option<String>) -> Result<Self> {
        Self::with_bulk_timeout(url, api_key, 300)
    }

    pub fn with_bulk_timeout(
        url: &str,
        api_key: Option<String>,
        bulk_timeout_secs: u64,
    ) -> Result<Self> {
        Self::with_bulk_policy(
            url,
            api_key,
            Duration::from_secs(bulk_timeout_secs),
            Duration::from_millis(250),
            Duration::from_secs(8),
        )
    }

    fn with_bulk_policy(
        url: &str,
        api_key: Option<String>,
        bulk_timeout: Duration,
        retry_initial_delay: Duration,
        retry_max_delay: Duration,
    ) -> Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(300))
            .danger_accept_invalid_certs(true)
            .build()
            .context("build http client")?;
        Ok(Es {
            base: url.trim_end_matches('/').to_string(),
            http,
            api_key,
            bulk_timeout,
            retry_initial_delay,
            retry_max_delay,
            admission: Arc::new(BulkAdmission::off()),
            #[cfg(test)]
            backoff_delays: Arc::new(std::sync::Mutex::new(Vec::new())),
        })
    }

    /// Give this client a bulk admission window `workers` wide, so a 429 can
    /// shrink the load the run offers instead of only delaying it (#240 §8).
    ///
    /// Off by default: a probe or a one-shot client has nothing to throttle.
    /// A run enables it once, before its workers start, and every clone made
    /// afterwards shares the same window.
    pub fn with_bulk_concurrency(mut self, workers: usize, announce: bool) -> Self {
        self.admission = Arc::new(BulkAdmission::new(workers.max(1), announce));
        self
    }

    /// The current bulk admission limit (`0` when the gate is off).
    pub fn bulk_concurrency_limit(&self) -> usize {
        self.admission.limit()
    }

    /// How many distinct 429 congestion events this client answered.
    pub fn bulk_congestion_events(&self) -> u64 {
        self.admission.congestion_events()
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        let mut r = self.http.request(method, format!("{}{}", self.base, path));
        if let Some(k) = &self.api_key {
            r = r.header("Authorization", format!("ApiKey {k}"));
        }
        r
    }

    pub fn ping(&self) -> Result<Value> {
        let resp = self
            .req(reqwest::Method::GET, "/")
            .send()
            .with_context(|| format!("endpoint unreachable: {}", self.base))?;
        let status = resp.status();
        if status.as_u16() == 401 || status.as_u16() == 403 {
            anyhow::bail!(self.auth_help(status.as_u16()));
        }
        Ok(resp.json().unwrap_or(Value::Null))
    }

    /// The recovery message for a server that wants credentials we do not have.
    ///
    /// This exists because the failure it replaces was actively misleading. A
    /// server with auth on (which is the shipped default) answered the very
    /// first request with 401, `ping` threw the status away, and the run went
    /// on to print ~15 lines of healthy-looking progress before dying at the
    /// embedding-identity probe with "requires a XERJ server that exposes a
    /// resumable embedding identity". A real user read that, concluded their
    /// server lacked the feature, and disabled auth to get past it — when all
    /// that was missing was a key the server had already written to disk.
    ///
    /// So: name every way out, and name them at the first round trip.
    fn auth_help(&self, status: u16) -> String {
        let what = if self.api_key.is_some() {
            "rejected the API key we sent"
        } else {
            "requires authentication and no API key was supplied"
        };
        // Order and framing matter here. A blind onboarding run showed that
        // listing `--insecure` as a co-equal third bullet makes it the
        // cheapest thing to try — one flag, no path to look up — which is the
        // straight line to "I turned auth off", the very outcome this message
        // exists to prevent. So: the two fixes that keep auth on come first
        // and concretely, and turning auth off is fenced as a last resort for
        // a throwaway local server. The server's own 401 already words it
        // this way; this makes the CLI say what the server says.
        format!(
            "the server at {} {what} (HTTP {status}).\n\
             \x20 To fix, in order of preference:\n\
             \x20 1. export XERJ_API_KEY=\"$(cat <data_dir>/admin.key)\" — the server \
             generates this key on first startup and prints the path in its startup banner\n\
             \x20 2. or pass it explicitly: --api-key <key>\n\
             \x20 3. last resort, and only for a throwaway local server you can afford to \
             leave unauthenticated: restart it with --insecure, which disables TLS and auth \
             for every client, not just this command",
            self.base
        )
    }

    pub fn embedding_execution_identity(&self) -> Result<EmbeddingExecutionIdentity> {
        let response = self
            .req(reqwest::Method::GET, "/v1/embedding/identity")
            .send()
            .context("GET /v1/embedding/identity")?;
        let status = response.status();
        // Defence in depth: `ping` already fails closed on 401/403, but a
        // `--url` target can answer `GET /` anonymously and still guard this
        // endpoint. Blaming the server's capabilities for a missing credential
        // is what sent the original reporter down the wrong path.
        if status.as_u16() == 401 || status.as_u16() == 403 {
            anyhow::bail!(self.auth_help(status.as_u16()));
        }
        if !status.is_success() {
            anyhow::bail!(
                "GET /v1/embedding/identity failed: HTTP {status}; semantic autoindex requires \
                 a XERJ server that exposes a resumable embedding identity"
            );
        }
        let value: Value = response
            .json()
            .context("parse embedding identity response")?;
        let identity: EmbeddingExecutionIdentity = serde_json::from_value(
            value
                .get("data")
                .cloned()
                .ok_or_else(|| anyhow!("embedding identity response has no data object"))?,
        )
        .context("parse embedding identity")?;
        if identity.version != 1
            || identity.identity_sha256.len() != 64
            || !identity
                .identity_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || identity.dimensions == Some(0)
            // `lexical` and `onnx-experimental` are the backends whose width
            // the server does pin, so an absent width from one of them means
            // the response did not come from a server that pins it. Only the
            // explicitly unpinned backends may omit it.
            || (matches!(identity.backend.as_str(), "lexical" | "onnx-experimental")
                && identity.dimensions.is_none())
            || identity.semantic_contract != "semantic_text-derived-vector.v1"
            || !matches!(
                identity.backend.as_str(),
                "lexical" | "neural" | "proxy" | "onnx-experimental"
            )
        {
            anyhow::bail!("server returned an unsupported embedding execution identity");
        }
        Ok(identity)
    }

    /// GET an arbitrary path and return only the HTTP status code.
    ///
    /// One attempt, no retry/backoff: `xerj brain` uses this to probe
    /// whether a server is listening (`/health/ready`, auth-exempt) and
    /// whether its credentials are accepted (`/`), and a liveness probe
    /// that silently retried for ~16s would misreport "already running"
    /// as "boot took 16s". A transport error means nothing answered.
    pub fn get_status(&self, path: &str) -> Result<u16> {
        let resp = self
            .req(reqwest::Method::GET, path)
            .send()
            .with_context(|| format!("no response from {}{}", self.base, path))?;
        Ok(resp.status().as_u16())
    }

    /// Retry wrapper: 429/5xx/transport → backoff 250ms..8s, 6 attempts.
    ///
    /// A 429 is also reported to the bulk admission window: sleeping is how
    /// this request survives congestion, shrinking is how the *run* stops
    /// causing it (#240 §8). 5xx is not congestion and does not shrink.
    fn with_retry<T>(
        &self,
        what: &str,
        mut f: impl FnMut() -> Result<reqwest::blocking::Response>,
        parse: impl Fn(reqwest::blocking::Response) -> Result<T>,
    ) -> Result<T> {
        const MAX_ATTEMPTS: usize = 6;
        let mut delay = self.retry_initial_delay;
        let mut last_err = None;
        for attempt in 0..MAX_ATTEMPTS {
            match f() {
                Ok(resp) => {
                    let status = resp.status();
                    if status.as_u16() == 429 || status.is_server_error() {
                        if status.as_u16() == 429 {
                            self.admission.on_congestion();
                        }
                        last_err = Some(match server_reason(resp) {
                            Some(reason) => anyhow!("{what}: HTTP {status}: {reason}"),
                            None => anyhow!("{what}: HTTP {status}"),
                        });
                    } else {
                        return parse(resp);
                    }
                }
                Err(e) => last_err = Some(e),
            }
            if attempt + 1 < MAX_ATTEMPTS {
                self.backoff(delay);
                delay = (delay * 2).min(self.retry_max_delay);
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("{what}: retries exhausted")))
    }

    /// Sleep between attempts, recording how long was actually slept.
    ///
    /// One function so that moving the CALL SITE cannot separate recording from
    /// sleeping: an earlier version incremented a counter next to a bare
    /// `std::thread::sleep`, and moving only the sleep out of its guard produced
    /// a sixth backoff the counter still reported as five.
    ///
    /// What this does NOT defend against: a sleep that never routes through
    /// here. A bare `thread::sleep` elsewhere in the loop adds real delay that
    /// no assertion over recorded data can observe, and the test carries no
    /// wall-clock bound to catch it — deliberately, because a wall-clock bound
    /// is what failed one run in five under load (#436). That is the whole of
    /// the residual gap.
    ///
    /// An earlier version of this comment listed three survivors. Two of them
    /// — the sleep moved out of `backoff`, and the sleep deleted while the push
    /// stays — are killed by recording the OBSERVED duration alongside the
    /// requested one, which the same commit introduced. Verification caught the
    /// comment describing the code as it had been rather than as it was.
    ///
    /// Recording both is what makes `observed >= requested` checkable, so a
    /// delay computed correctly and then not honoured shows up instead of
    /// looking right.
    fn backoff(&self, delay: Duration) {
        #[cfg(test)]
        let started = Instant::now();
        std::thread::sleep(delay);
        #[cfg(test)]
        self.backoff_delays
            .lock()
            .expect("backoff_delays poisoned")
            .push((delay, started.elapsed()));
    }

    /// PUT index with explicit mapping; tolerates already-exists.
    pub fn ensure_index(&self, index: &str, body: &Value) -> Result<()> {
        let body = with_single_wal_shard(body);
        let resp = self
            .req(reqwest::Method::PUT, &format!("/{index}"))
            .json(&body)
            .send()
            .context("PUT index")?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let text = resp.text().unwrap_or_default();
        if text.contains("resource_already_exists")
            || status.as_u16() == 400 && text.contains("exists")
        {
            return Ok(());
        }
        Err(anyhow!("PUT /{index} failed: {status} {text}"))
    }

    /// Additive mapping update for fields introduced after an index was created.
    pub fn update_mapping(&self, index: &str, body: &Value) -> Result<()> {
        let resp = self
            .req(reqwest::Method::PUT, &format!("/{index}/_mapping"))
            .json(body)
            .send()
            .context("PUT mapping")?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let text = resp.text().unwrap_or_default();
        Err(anyhow!("PUT /{index}/_mapping failed: {status} {text}"))
    }

    /// Send one bulk request, holding a slot in the admission window for as
    /// long as the attempt (including its retries) is outstanding — that is
    /// what makes a shrunken limit actually reduce offered load rather than
    /// only rename it.
    pub fn bulk(&self, body: Vec<u8>) -> Result<BulkOutcome> {
        let _permit = self.admission.acquire();
        // A bulk can also be throttled *per item*: HTTP 200 with individual
        // documents rejected `status: 429`. That is the same congestion
        // signal wearing a different hat, and it must not be read as a
        // clean bulk that earns concurrency back.
        let item_throttled = std::cell::Cell::new(false);
        let outcome = self.with_retry(
            "_bulk",
            || self.send_bulk(body.clone()),
            |resp| {
                let status = resp.status();
                if !status.is_success() {
                    return Err(anyhow!("bulk HTTP {status}"));
                }
                let v: Value = resp.json().context("parse bulk response")?;
                let mut item_errors = 0u64;
                let mut server_errors = 0u64;
                let mut first_error = None;
                let mut first_server_error = None;
                if v.get("errors").and_then(|e| e.as_bool()).unwrap_or(false) {
                    if let Some(items) = v.get("items").and_then(|i| i.as_array()) {
                        for it in items {
                            let op = it
                                .get("index")
                                .or_else(|| it.get("create"))
                                .or_else(|| it.get("update"));
                            if let Some(op) = op {
                                if op.get("error").is_some() {
                                    item_errors += 1;
                                    let item_status =
                                        op.get("status").and_then(Value::as_u64).unwrap_or(500);
                                    if item_status == 429 {
                                        item_throttled.set(true);
                                    }
                                    if item_status == 429
                                        || item_status >= 500
                                        || is_index_block_error(&op["error"])
                                    {
                                        server_errors += 1;
                                        if first_server_error.is_none() {
                                            first_server_error = Some(
                                                op["error"].to_string().chars().take(500).collect(),
                                            );
                                        }
                                    }
                                    if first_error.is_none() {
                                        first_error = Some(
                                            op["error"].to_string().chars().take(300).collect(),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(BulkOutcome {
                    item_errors,
                    server_errors,
                    first_error,
                    first_server_error,
                })
            },
        );
        match (&outcome, item_throttled.get()) {
            (Ok(_), false) => self.admission.on_success(),
            (Ok(_), true) => self.admission.on_congestion(),
            // A failed bulk earns nothing back. If it failed *because* of a
            // 429, `with_retry` has already shrunk the window.
            (Err(_), _) => {}
        }
        outcome
    }

    fn send_bulk(&self, body: Vec<u8>) -> Result<reqwest::blocking::Response> {
        self.req(reqwest::Method::POST, "/_bulk")
            .timeout(self.bulk_timeout)
            .header("Content-Type", "application/x-ndjson")
            .header("X-Turbo", "1")
            .body(body)
            .send()
            .with_context(|| format!("bulk send (request timeout {:?})", self.bulk_timeout))
    }

    /// Remove every document matching `query` before a file-level replacement.
    /// Refresh is requested so a retry cannot observe or retain an older
    /// locator set alongside the replacement. The server executes ONE bounded
    /// search-and-delete pass per call (size-capped at 10k docs), so a single
    /// response is not complete removal: repeat until a pass deletes nothing.
    pub fn delete_by_query(&self, index: &str, query: &Value) -> Result<()> {
        const MAX_PASSES: usize = 1_000;
        for _ in 0..MAX_PASSES {
            if self.delete_by_query_pass(index, query)? == 0 {
                return Ok(());
            }
        }
        Err(anyhow!(
            "POST /{index}/_delete_by_query still reported deletions after {MAX_PASSES} passes; \
             refusing to treat the previous generation as fully removed"
        ))
    }

    /// One server-side delete pass; returns the reported `deleted` count.
    fn delete_by_query_pass(&self, index: &str, query: &Value) -> Result<u64> {
        self.with_retry(
            "delete_by_query",
            || {
                self.req(
                    reqwest::Method::POST,
                    &format!("/{index}/_delete_by_query?refresh=true"),
                )
                .json(&serde_json::json!({"query": query}))
                .send()
                .map_err(|e| anyhow!("delete_by_query: {e}"))
            },
            |resp| {
                let status = resp.status();
                let body: Value = resp.json().unwrap_or(Value::Null);
                if status.is_success()
                    && body
                        .get("failures")
                        .and_then(Value::as_array)
                        .is_none_or(Vec::is_empty)
                {
                    Ok(body.get("deleted").and_then(Value::as_u64).unwrap_or(0))
                } else {
                    Err(anyhow!(
                        "POST /{index}/_delete_by_query failed: HTTP {status}: {body}"
                    ))
                }
            },
        )
    }

    pub fn refresh(&self, pattern: &str) -> Result<()> {
        self.with_retry(
            "refresh",
            || {
                self.req(reqwest::Method::POST, &format!("/{pattern}/_refresh"))
                    .send()
                    .map_err(|e| anyhow!("refresh: {e}"))
            },
            |resp| {
                if resp.status().is_success() {
                    Ok(())
                } else {
                    Err(anyhow!("refresh HTTP {}", resp.status()))
                }
            },
        )
    }

    fn search_raw(&self, index: &str, body: &Value) -> Result<(reqwest::StatusCode, Value)> {
        self.with_retry(
            "search",
            || {
                self.req(reqwest::Method::POST, &format!("/{index}/_search"))
                    .json(body)
                    .send()
                    .map_err(|e| anyhow!("search: {e}"))
            },
            |resp| {
                let status = resp.status();
                let v: Value = resp.json().unwrap_or(Value::Null);
                Ok((status, v))
            },
        )
    }

    pub fn search(&self, index: &str, body: &Value) -> Result<Value> {
        let (status, v) = self.search_raw(index, body)?;
        if !status.is_success() {
            return Err(anyhow!("search /{index} HTTP {status}: {v}"));
        }
        Ok(v)
    }

    /// `search`, but a missing index is an answer rather than a failure.
    ///
    /// A probe whose whole job is to decide "does the server still hold what
    /// the journal says it holds?" must be able to tell *the index is gone*
    /// apart from *the server refused* — only the first has a recovery worth
    /// printing, and propagating its 404 replaces that recovery text with a raw
    /// HTTP error. Every other status keeps `search`'s behaviour, so this never
    /// launders a real failure into an absence.
    pub fn search_present(&self, index: &str, body: &Value) -> Result<Option<Value>> {
        let (status, v) = self.search_raw(index, body)?;
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(anyhow!("search /{index} HTTP {status}: {v}"));
        }
        Ok(Some(v))
    }

    pub fn count(&self, index: &str) -> Result<u64> {
        // _count may not exist on all builds — use size:0 search with totals.
        let v = self.search(
            index,
            &serde_json::json!({"size": 0, "track_total_hits": true}),
        )?;
        v.pointer("/hits/total/value")
            .and_then(|t| t.as_u64())
            .or_else(|| v.pointer("/hits/total").and_then(|t| t.as_u64()))
            .ok_or_else(|| anyhow!("no total in search response"))
    }

    /// `_cat/indices` is plain text, no header (?format=json is IGNORED —
    /// verified). Returns (index, docs_count) with `.xerj_*` system indices
    /// filtered out.
    pub fn cat_indices(&self) -> Result<Vec<(String, u64)>> {
        let resp = self
            .req(reqwest::Method::GET, "/_cat/indices")
            .send()
            .context("_cat/indices")?;
        let text = resp.text().unwrap_or_default();
        let mut out = Vec::new();
        for line in text.lines() {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() < 3 {
                continue;
            }
            // format: health status index uuid pri rep docs.count deleted size…
            let name = cols[2].to_string();
            if name.starts_with(".xerj") || name.starts_with('.') {
                continue;
            }
            let docs = cols
                .get(6)
                .and_then(|c| c.parse::<u64>().ok())
                // fallback for column-order variants: first integer after the
                // uuid that is not the 1-digit pri/rep pair
                .or_else(|| cols.iter().skip(7).find_map(|c| c.parse::<u64>().ok()))
                .unwrap_or(0);
            out.push((name, docs));
        }
        Ok(out)
    }

    pub fn get_doc(&self, index: &str, id: &str) -> Result<Option<Value>> {
        let resp = self
            .req(reqwest::Method::GET, &format!("/{index}/_doc/{id}"))
            .send()
            .context("GET doc")?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        let v: Value = resp.json().unwrap_or(Value::Null);
        if v.get("found").and_then(|f| f.as_bool()).unwrap_or(false) {
            Ok(v.get("_source").cloned())
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Es;
    use std::io::Read;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};

    #[test]
    fn bulk_request_uses_custom_timeout_and_drops_timed_out_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (closed_tx, closed_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..read]).contains("POST /_bulk"));
            std::thread::sleep(Duration::from_millis(1300));
            let closed = stream.read(&mut request).map(|n| n == 0).unwrap_or(true);
            closed_tx.send(closed).unwrap();
        });
        let es = Es::with_bulk_policy(
            &format!("http://{address}"),
            None,
            Duration::from_secs(1),
            Duration::from_millis(10),
            Duration::from_millis(20),
        )
        .unwrap();
        let started = Instant::now();
        let err = es.send_bulk(b"{}\n".to_vec()).unwrap_err();
        let elapsed = started.elapsed();
        assert!(format!("{err:#}").contains("timed out"), "{err:#}");
        assert!(elapsed >= Duration::from_millis(800), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
        assert!(closed_rx.recv_timeout(Duration::from_secs(4)).unwrap());
        server.join().unwrap();
    }

    fn read_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let count = stream.read(&mut buffer).unwrap();
            if count == 0 {
                // Peer closed. Without this the loop spins on `Ok(0)` forever
                // whenever the client gives up mid-request, and `server.join()`
                // never returns — a HANGING test, which under load is worse
                // than a failing one because CI has nothing to report. Seen
                // 19 times in 219 runs at 20x CPU oversubscription.
                break;
            }
            request.extend_from_slice(&buffer[..count]);
            if let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + length {
                    break;
                }
            }
        }
        request
    }

    fn success(stream: &mut std::net::TcpStream) {
        respond_json(stream, br#"{"errors":false,"items":[]}"#);
    }

    fn respond_json(stream: &mut std::net::TcpStream, body: &[u8]) {
        respond_status(stream, "200 OK", body);
    }

    fn respond_status(stream: &mut std::net::TcpStream, status: &str, body: &[u8]) {
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    }

    /// A deleted index is an answer for the probes that ask whether the server
    /// still holds what a journal claims it holds — `search` propagating that
    /// 404 replaces `xerj brain`'s recovery text with a raw HTTP error. Every
    /// other refusal must still propagate, so absence is never laundered out of
    /// a real failure.
    #[test]
    fn search_present_reports_a_missing_index_as_absence_and_nothing_else() {
        for (status, body, expect_absent) in [
            (
                "404 Not Found",
                &br#"{"error":{"type":"index_not_found_exception"},"status":404}"#[..],
                true,
            ),
            (
                "403 Forbidden",
                &br#"{"error":{"type":"security_exception"},"status":403}"#[..],
                false,
            ),
            (
                "200 OK",
                &br#"{"hits":{"total":{"value":7,"relation":"eq"}}}"#[..],
                false,
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let owned = body.to_vec();
            let status_line = status.to_owned();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                assert!(
                    String::from_utf8_lossy(&request).starts_with("POST /nodes/_search HTTP/1.1"),
                    "{}",
                    String::from_utf8_lossy(&request)
                );
                respond_status(&mut stream, &status_line, &owned);
            });
            let es = Es::new(&format!("http://{address}"), None).unwrap();
            let result = es.search_present("nodes", &serde_json::json!({"size": 0}));
            if expect_absent {
                assert!(result.unwrap().is_none(), "{status}");
            } else if status.starts_with("200") {
                let value = result.unwrap().expect("a served index is present");
                assert_eq!(value.pointer("/hits/total/value").unwrap(), 7);
            } else {
                let error = result.unwrap_err();
                assert!(format!("{error:#}").contains("403"), "{error:#}");
            }
            server.join().unwrap();
        }
    }

    #[test]
    fn embedding_identity_uses_native_endpoint_and_parses_sanitized_data() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            assert!(
                String::from_utf8_lossy(&request)
                    .starts_with("GET /v1/embedding/identity HTTP/1.1"),
                "{}",
                String::from_utf8_lossy(&request)
            );
            respond_json(
                &mut stream,
                br#"{"data":{"version":1,"backend":"lexical","identity_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","dimensions":384,"semantic_contract":"semantic_text-derived-vector.v1","resumable":true},"took_ms":0,"request_id":"test"}"#,
            );
        });
        let es = Es::new(&format!("http://{address}"), None).unwrap();
        let identity = es.embedding_execution_identity().unwrap();
        assert_eq!(identity.backend, "lexical");
        assert!(identity.resumable);
        assert_eq!(identity.dimensions, Some(384));
        server.join().unwrap();
    }

    /// `neural` and `proxy` omit the width entirely, so the client must accept
    /// a response without a `dimensions` key rather than failing to parse it.
    /// It still rejects an explicit zero, which no backend should ever send,
    /// and it rejects an omitted width from `lexical`/`onnx-experimental`,
    /// whose widths a real XERJ server always reports.
    #[test]
    fn embedding_identity_accepts_an_omitted_width_and_rejects_a_zero_one() {
        for (body, expect_ok) in [
            (
                br#"{"data":{"version":1,"backend":"proxy","identity_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","semantic_contract":"semantic_text-derived-vector.v1","resumable":false,"non_resumable_reason":"remote"},"took_ms":0,"request_id":"test"}"#.to_vec(),
                true,
            ),
            (
                br#"{"data":{"version":1,"backend":"lexical","identity_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","dimensions":0,"semantic_contract":"semantic_text-derived-vector.v1","resumable":true},"took_ms":0,"request_id":"test"}"#.to_vec(),
                false,
            ),
            (
                br#"{"data":{"version":1,"backend":"lexical","identity_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","semantic_contract":"semantic_text-derived-vector.v1","resumable":true},"took_ms":0,"request_id":"test"}"#.to_vec(),
                false,
            ),
            (
                br#"{"data":{"version":1,"backend":"onnx-experimental","identity_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","semantic_contract":"semantic_text-derived-vector.v1","resumable":true},"took_ms":0,"request_id":"test"}"#.to_vec(),
                false,
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let _ = read_request(&mut stream);
                respond_json(&mut stream, &body);
            });
            let es = Es::new(&format!("http://{address}"), None).unwrap();
            let result = es.embedding_execution_identity();
            assert_eq!(result.is_ok(), expect_ok, "{result:?}");
            if let Ok(identity) = result {
                assert_eq!(identity.dimensions, None);
                assert!(!identity.resumable);
            }
            server.join().unwrap();
        }
    }

    /// #195: cluster/index write blocks arrive with status 403 (only the
    /// flood-stage block is 429), so they must be classified as backend
    /// failures by error TYPE/wording — never counted as junk source
    /// records. A genuine per-item 400 stays junk.
    #[test]
    fn per_item_write_block_errors_are_backend_failures_not_junk() {
        for (body, expect_server_errors) in [
            // XERJ shape: explicit write block via the semantic bulk path.
            (
                br#"{"errors":true,"items":[{"index":{"_index":"i","_id":"a","status":403,"error":{"type":"engine_exception","reason":"index [i] is blocked for write operations","status":403}}}]}"#.to_vec(),
                1u64,
            ),
            // ES shape: cluster_block_exception recognised by TYPE alone
            // (reason deliberately free of the word "blocked").
            (
                br#"{"errors":true,"items":[{"index":{"_index":"i","_id":"a","status":403,"error":{"type":"cluster_block_exception","reason":"FORBIDDEN/8/index write (api)"}}}]}"#.to_vec(),
                1u64,
            ),
            // A real bad-record 400 must stay in the junk bucket.
            (
                br#"{"errors":true,"items":[{"index":{"_index":"i","_id":"a","status":400,"error":{"type":"document_parsing_exception","reason":"failed to parse field [ts] of type [date]"}}}]}"#.to_vec(),
                0u64,
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let _ = read_request(&mut stream);
                respond_json(&mut stream, &body);
            });
            let es = Es::new(&format!("http://{address}"), None).unwrap();
            let outcome = es.bulk(b"{\"index\":{}}\n{}\n".to_vec()).unwrap();
            assert_eq!(outcome.item_errors, 1);
            assert_eq!(
                outcome.server_errors, expect_server_errors,
                "{:?}",
                outcome.first_error
            );
            if expect_server_errors > 0 {
                let reason = outcome.first_server_error.expect("server error recorded");
                assert!(
                    reason.contains("block") || reason.contains("FORBIDDEN"),
                    "{reason}"
                );
            } else {
                assert!(outcome.first_server_error.is_none());
            }
            server.join().unwrap();
        }
    }

    #[test]
    fn retry_path_recovers_after_one_bulk_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            read_request(&mut first);
            let first = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(120));
                drop(first);
            });
            let (mut second, _) = listener.accept().unwrap();
            read_request(&mut second);
            success(&mut second);
            first.join().unwrap();
        });
        let es = Es::with_bulk_policy(
            &format!("http://{address}"),
            None,
            Duration::from_millis(50),
            Duration::from_millis(10),
            Duration::from_millis(20),
        )
        .unwrap();
        let outcome = es.bulk(b"{\"index\":{}}\n{}\n".to_vec()).unwrap();
        assert_eq!(outcome.item_errors, 0);
        server.join().unwrap();
    }

    #[test]
    fn all_timeout_retry_path_is_bounded_to_six_attempts_without_final_sleep() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = Arc::new(Mutex::new(0usize));
        let accepted_server = accepted.clone();
        let server = std::thread::spawn(move || {
            let mut handlers = Vec::new();
            for _ in 0..6 {
                let (mut stream, _) = listener.accept().unwrap();
                *accepted_server.lock().unwrap() += 1;
                handlers.push(std::thread::spawn(move || {
                    read_request(&mut stream);
                    std::thread::sleep(Duration::from_millis(100));
                }));
            }
            for handler in handlers {
                handler.join().unwrap();
            }
        });
        let es = Es::with_bulk_policy(
            &format!("http://{address}"),
            None,
            Duration::from_millis(25),
            Duration::from_millis(10),
            Duration::from_millis(20),
        )
        .unwrap();
        let error = match es.bulk(b"{}\n".to_vec()) {
            Ok(_) => panic!("all six delayed responses unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("timed out"), "{error:#}");
        assert_eq!(*accepted.lock().unwrap(), 6);
        // The property is a COUNT, not a duration: six attempts with five
        // backoffs between them and none after the last. This used to be
        // asserted as `elapsed < 260ms` against a 240ms budget, which measured
        // the machine rather than the code and failed about one run in five —
        // on `main`, so it reddened pull requests that had not touched it
        // (#436). A sixth sleep is what the old bound was really looking for,
        // and it is visible here directly.
        // The REQUESTED sequence is asserted exactly, and each observed sleep
        // only has to be at least what was asked for. That removes the last
        // wall-clock upper bound from this test: an earlier version compared
        // the observed value against `want + 50`, which is an upper bound on
        // real time and therefore the one assertion load could break — while
        // the comment eight lines above it said there was no such bound. A
        // sleep can overrun and cannot underrun, so `observed >= requested` is
        // safe at any load, and the requested sequence pins arity, ordering,
        // the doubling and the `retry_max_delay` cap on its own.
        let recorded = es.backoff_delays.lock().unwrap().clone();
        let requested: Vec<u64> = recorded.iter().map(|(r, _)| r.as_millis() as u64).collect();
        assert_eq!(
            requested,
            vec![10, 20, 20, 20, 20],
            "six attempts back off five times, never after the final failure, \
             doubling to the cap"
        );
        assert!(
            recorded.iter().all(|(r, o)| o >= r),
            "a delay that is computed correctly and then not honoured is the \
             one thing the requested sequence alone cannot see: {recorded:?}"
        );
        server.join().unwrap();
    }

    #[test]
    fn delete_by_query_repeats_until_a_pass_deletes_nothing() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requests_server = requests.clone();
        let server = std::thread::spawn(move || {
            // A previous generation larger than one server pass: the first
            // pass removes its 10k cap and only the second reports done.
            for body in [
                br#"{"deleted":10000,"failures":[]}"#.as_slice(),
                br#"{"deleted":0,"failures":[]}"#.as_slice(),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                requests_server
                    .lock()
                    .unwrap()
                    .push(read_request(&mut stream));
                respond_json(&mut stream, body);
            }
        });
        let es = Es::with_bulk_policy(
            &format!("http://{address}"),
            None,
            Duration::from_millis(100),
            Duration::from_millis(10),
            Duration::from_millis(20),
        )
        .unwrap();
        es.delete_by_query("data", &serde_json::json!({"term": {"ax_file": "key"}}))
            .unwrap();
        server.join().unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for request in requests.iter() {
            let text = String::from_utf8_lossy(request);
            assert!(text.contains("POST /data/_delete_by_query"), "{text}");
        }
    }

    /// #345: the reporter's whole issue was `delete_by_query: HTTP 500
    /// Internal Server Error` and nothing else, so it was filed "not
    /// investigated". The server had already said why — every by-query refusal
    /// answers `{"error": {"type", "reason"}, "status"}` — and the client threw
    /// the body away after the last retry. Cover both shapes the wire really
    /// produces: the structured refusal, and a proxy's HTML page.
    #[test]
    fn an_exhausted_5xx_retry_reports_the_server_reason_not_only_the_status() {
        for (body, expect) in [
            (
                br#"{"error":{"type":"internal_server_error_exception","reason":"collection publication was interrupted"},"status":500}"#.as_slice(),
                "internal_server_error_exception: collection publication was interrupted",
            ),
            (
                b"<html><body>502 upstream connect error</body></html>".as_slice(),
                "<html><body>502 upstream connect error</body></html>",
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                for _ in 0..6 {
                    let (mut stream, _) = listener.accept().unwrap();
                    read_request(&mut stream);
                    respond_status(&mut stream, "500 Internal Server Error", body);
                }
            });
            let es = Es::with_bulk_policy(
                &format!("http://{address}"),
                None,
                Duration::from_millis(100),
                Duration::from_millis(1),
                Duration::from_millis(2),
            )
            .unwrap();
            let error = es
                .delete_by_query("data", &serde_json::json!({"term": {"ax_file": "key"}}))
                .unwrap_err();
            let rendered = format!("{error:#}");
            assert!(rendered.contains("HTTP 500 Internal Server Error"), "{rendered}");
            assert!(rendered.contains(expect), "{rendered}");
            server.join().unwrap();
        }
    }

    #[test]
    fn delete_by_query_that_never_reaches_zero_fails_at_the_pass_cap() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut served = 0usize;
            loop {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                if request.starts_with(b"STOP") {
                    break;
                }
                respond_json(&mut stream, br#"{"deleted":10000,"failures":[]}"#);
                served += 1;
            }
            served
        });
        let es = Es::with_bulk_policy(
            &format!("http://{address}"),
            None,
            Duration::from_millis(100),
            Duration::from_millis(10),
            Duration::from_millis(20),
        )
        .unwrap();
        let error = es
            .delete_by_query("data", &serde_json::json!({"term": {"ax_file": "key"}}))
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("still reported deletions"),
            "{error:#}"
        );
        let mut stop = std::net::TcpStream::connect(address).unwrap();
        stop.write_all(b"STOP\r\n\r\n").unwrap();
        drop(stop);
        assert_eq!(server.join().unwrap(), 1_000);
    }

    #[test]
    fn ambiguous_dropped_response_retries_byte_identical_bulk_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requests_server = requests.clone();
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            requests_server
                .lock()
                .unwrap()
                .push(read_request(&mut first));
            drop(first); // backend may have committed, but response is ambiguous
            let (mut second, _) = listener.accept().unwrap();
            requests_server
                .lock()
                .unwrap()
                .push(read_request(&mut second));
            success(&mut second);
        });
        let body = b"{\"index\":{\"_index\":\"i\",\"_id\":\"stable-id\"}}\n{\"v\":1}\n".to_vec();
        let es = Es::with_bulk_policy(
            &format!("http://{address}"),
            None,
            Duration::from_millis(100),
            Duration::from_millis(10),
            Duration::from_millis(20),
        )
        .unwrap();
        es.bulk(body.clone()).unwrap();
        server.join().unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for request in requests.iter() {
            let offset = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
            assert_eq!(&request[offset..], body.as_slice());
        }
    }

    // ── #240 §8: backpressure must reach the thing generating the load ──

    fn throttled(stream: &mut std::net::TcpStream) {
        let body = br#"{"error":{"type":"es_rejected_execution_exception"},"status":429}"#;
        write!(
            stream,
            "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    }

    /// Serve `plan` responses, one per connection, in order.
    fn serve(listener: TcpListener, plan: Vec<bool>) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            for ok in plan {
                let (mut stream, _) = listener.accept().unwrap();
                let _ = read_request(&mut stream);
                if ok {
                    success(&mut stream);
                } else {
                    throttled(&mut stream);
                }
            }
        })
    }

    fn client(address: std::net::SocketAddr, workers: usize) -> Es {
        Es::with_bulk_policy(
            &format!("http://{address}"),
            None,
            Duration::from_secs(5),
            Duration::from_millis(10),
            Duration::from_millis(20),
        )
        .unwrap()
        .with_bulk_concurrency(workers, false)
    }

    const BULK: &[u8] = b"{\"index\":{\"_index\":\"i\",\"_id\":\"a\"}}\n{\"v\":1}\n";

    /// A 429 must shrink the window the run offers load through, and a clean
    /// streak must probe it back up — never past what the operator asked for.
    ///
    /// Before this, the client slept and re-offered exactly the same load
    /// from exactly as many workers: `bulk_concurrency_limit()` stayed at 8
    /// no matter how hard the server pushed back.
    #[test]
    fn a_429_shrinks_the_bulk_window_and_a_clean_streak_probes_it_back() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        // one throttled attempt, then its retry, then RECOVER_AFTER clean bulks
        let mut plan = vec![false, true];
        plan.extend(std::iter::repeat_n(true, super::RECOVER_AFTER));
        let server = serve(listener, plan);

        let es = client(address, 8);
        assert_eq!(es.bulk_concurrency_limit(), 8, "starts at --workers");

        es.bulk(BULK.to_vec()).unwrap();
        assert_eq!(
            es.bulk_concurrency_limit(),
            4,
            "a 429 must halve the offered concurrency"
        );
        assert_eq!(es.bulk_congestion_events(), 1);

        for _ in 0..super::RECOVER_AFTER {
            es.bulk(BULK.to_vec()).unwrap();
        }
        assert_eq!(
            es.bulk_concurrency_limit(),
            5,
            "recovery is additive: one worker back per clean streak, not a reset"
        );
        assert_eq!(es.bulk_congestion_events(), 1);
        server.join().unwrap();
    }

    /// HTTP 200 whose *items* were rejected 429 is the same congestion
    /// signal, and must not be counted as a clean bulk.
    #[test]
    fn per_item_429_counts_as_congestion_not_as_a_clean_bulk() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_request(&mut stream);
            respond_json(
                &mut stream,
                br#"{"errors":true,"items":[{"index":{"status":429,"error":{"type":"es_rejected_execution_exception","reason":"rejected"}}}]}"#,
            );
        });
        let es = client(address, 4);
        let outcome = es.bulk(BULK.to_vec()).unwrap();
        assert_eq!(outcome.server_errors, 1);
        assert_eq!(
            es.bulk_concurrency_limit(),
            2,
            "an item-level 429 must shrink the window too"
        );
        server.join().unwrap();
    }

    /// The window is a real gate, not a counter: once it is full, the next
    /// bulk waits for a slot instead of being sent anyway.
    #[test]
    fn a_full_window_blocks_the_next_bulk_until_a_slot_frees() {
        let admission = Arc::new(super::BulkAdmission::new(2, false));
        let first = admission.acquire();
        let _second = admission.acquire();

        let (tx, rx) = mpsc::channel();
        let waiter = {
            let admission = Arc::clone(&admission);
            std::thread::spawn(move || {
                let _permit = admission.acquire();
                tx.send(()).unwrap();
            })
        };
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "a third bulk must not be admitted into a 2-wide window"
        );
        drop(first);
        rx.recv_timeout(Duration::from_secs(5))
            .expect("freeing a slot must admit the waiting bulk");
        waiter.join().unwrap();
    }

    /// Every worker in flight sees the same congestion; they must count it
    /// once, or a momentary stall collapses the window to 1.
    #[test]
    fn concurrent_reports_of_one_stall_shrink_the_window_once() {
        let admission = super::BulkAdmission::new(16, false);
        for _ in 0..8 {
            admission.on_congestion();
        }
        assert_eq!(admission.limit(), 8);
        assert_eq!(admission.congestion_events(), 1);
    }

    /// A client that was never given a window has no gate at all: probes and
    /// one-shot callers must not serialise behind an admission limit.
    #[test]
    fn the_window_is_off_unless_a_run_asks_for_one() {
        let admission = super::BulkAdmission::off();
        assert_eq!(admission.limit(), 0);
        let _a = admission.acquire();
        let _b = admission.acquire();
        admission.on_congestion();
        assert_eq!(admission.congestion_events(), 0);
    }

    // ─── authentication failures ────────────────────────────────────────
    //
    // A real user hit HTTP 401 on the very first request of an `autoindex`
    // run, saw ~15 lines of healthy-looking progress, and was then told the
    // server "requires a XERJ server that exposes a resumable embedding
    // identity". They concluded the server lacked a feature and turned auth
    // off. Every test below exists to keep that specific failure impossible:
    // the run must stop at the first round trip, and the message must name
    // the credential, not a capability.

    /// Serve one request and answer with `status_line`, asserting the client
    /// asked for `path`. Returns the bound address to point a client at.
    fn one_shot(
        path: &'static str,
        status_line: &'static str,
    ) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            let request = String::from_utf8_lossy(&request);
            assert!(
                request.starts_with(&format!("GET {path} HTTP/1.1")),
                "{request}"
            );
            respond_status(
                &mut stream,
                status_line,
                br#"{"error":{"type":"security_exception","reason":"missing or invalid API key in Authorization header"},"status":401}"#,
            );
        });
        (address, server)
    }

    /// Every escape route the user has must be in the text, or the message is
    /// the same dead end with different words.
    fn assert_names_every_way_out(error: &str, base: &str) {
        for expected in [
            "--api-key",
            "XERJ_API_KEY",
            "<data_dir>/admin.key",
            "--insecure",
            base,
        ] {
            assert!(
                error.contains(expected),
                "recovery message must name {expected}: {error}"
            );
        }
    }

    /// `ping` used to be `Ok(resp.json().unwrap_or(Value::Null))` — a 401 was
    /// indistinguishable from a healthy server, which is what deferred the
    /// failure by fifteen lines of output. It must now fail closed, at the
    /// first round trip, on both refusal codes.
    #[test]
    fn ping_fails_closed_on_401_and_403_and_says_how_to_recover() {
        for (status_line, code, api_key, subject) in [
            (
                "401 Unauthorized",
                "401",
                None,
                "requires authentication and no API key was supplied",
            ),
            (
                "403 Forbidden",
                "403",
                Some("a-rejected-key".to_string()),
                "rejected the API key we sent",
            ),
        ] {
            let (address, server) = one_shot("/", status_line);
            let base = format!("http://{address}");
            let es = Es::new(&base, api_key).unwrap();
            let error = format!("{:#}", es.ping().unwrap_err());
            assert!(
                error.contains(subject),
                "message must distinguish sent-and-rejected from never-sent: {error}"
            );
            assert!(
                error.contains(code),
                "the HTTP status belongs in it: {error}"
            );
            assert_names_every_way_out(&error, &base);
            // The old capability sentence is what sent the reporter looking at
            // the wrong thing; a credential failure must never mention it.
            assert!(!error.contains("resumable embedding identity"), "{error}");
            server.join().unwrap();
        }
    }

    /// The fail-closed branch must be exactly two status codes wide: a healthy
    /// server still gets its banner parsed and the run still starts.
    #[test]
    fn ping_still_returns_the_banner_from_a_server_that_answers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_request(&mut stream);
            respond_json(
                &mut stream,
                br#"{"tagline":"XERJ","version":{"number":"1.0"}}"#,
            );
        });
        let es = Es::new(&format!("http://{address}"), Some("key".to_string())).unwrap();
        let banner = es.ping().unwrap();
        assert_eq!(banner.pointer("/tagline").unwrap(), "XERJ");
        server.join().unwrap();
    }

    /// Defence in depth for a `--url` target that answers `GET /` anonymously
    /// and guards the identity endpoint: the 401 must be reported as a missing
    /// credential, never as the server lacking a resumable identity. This is
    /// the exact sentence the original report pasted.
    #[test]
    fn the_identity_probe_reports_a_401_as_auth_not_as_a_missing_capability() {
        for (status_line, code, api_key, subject) in [
            (
                "401 Unauthorized",
                "401",
                None,
                "requires authentication and no API key was supplied",
            ),
            (
                "403 Forbidden",
                "403",
                Some("a-rejected-key".to_string()),
                "rejected the API key we sent",
            ),
        ] {
            let (address, server) = one_shot("/v1/embedding/identity", status_line);
            let base = format!("http://{address}");
            let es = Es::new(&base, api_key).unwrap();
            let error = format!("{:#}", es.embedding_execution_identity().unwrap_err());
            assert!(
                !error.contains("resumable embedding identity"),
                "the reported misleading message must be gone: {error}"
            );
            assert!(error.contains(subject), "{error}");
            assert!(error.contains(code), "{error}");
            assert_names_every_way_out(&error, &base);
            server.join().unwrap();
        }
    }

    /// …and the capability wording is still the right answer for a failure
    /// that really is about the server, so the auth branch has to be narrow.
    #[test]
    fn the_identity_probe_still_blames_the_capability_for_a_non_auth_failure() {
        for status_line in ["404 Not Found", "500 Internal Server Error"] {
            let (address, server) = one_shot("/v1/embedding/identity", status_line);
            let es = Es::new(&format!("http://{address}"), None).unwrap();
            let error = format!("{:#}", es.embedding_execution_identity().unwrap_err());
            assert!(
                error.contains("resumable embedding identity"),
                "{status_line}: {error}"
            );
            assert!(!error.contains("--insecure"), "{status_line}: {error}");
            server.join().unwrap();
        }
    }
}
