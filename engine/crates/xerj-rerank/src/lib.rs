//! Second-stage relevance reranking over candidates the engine already retrieved.
//!
//! # Why this is not in the engine
//!
//! Reranking runs *after* hits are collected and hydrated, and it makes an
//! outbound network call. Neither property belongs in the segment loop, so this
//! crate is a leaf the API layer calls once per search, never something
//! `xerj-engine` links. That keeps the read hot path and the ES-compat
//! conformance surface untouched.
//!
//! # Why a second stage at all
//!
//! XERJ ranks with BM25 and fuses with RRF or a linear combination. Those scores
//! order results but carry no absolute meaning: a `_score` of 7.2 is not
//! comparable across queries and cannot be thresholded. A calibrated reranker
//! returns a *probability* per document, which is both a sort key and an
//! absolute cut — so `min_score` becomes a meaningful knob for the first time.
//!
//! This matters more than ordering quality for XERJ specifically: the default
//! embedder is lexical feature hashing, so the engine has no semantic signal of
//! its own to offer. A reranker supplies one without the project claiming a
//! neural embedder it does not ship.
//!
//! # Failure policy: degrade on time, surface on contract
//!
//! Not every failure deserves the same answer, and a blanket "always fall back
//! to BM25" is the wrong default: a caller who asked for calibrated relevance
//! and silently got lexical order has been misled, which is the silent-fake
//! class this project treats as a defect (see the `learned` fusion arm in
//! `xerj-query`, which fails loud rather than substituting RRF).
//!
//! So the policy is split, following the one Meilisearch arrived at for its
//! Cohere reranker (`crates/meilisearch/src/personalization/mod.rs:74-135`,
//! approach adapted, not copied):
//!
//! * **Deadline exceeded → degrade.** Return the engine's own order and say so.
//!   A slow third party is not a reason to deny the caller results it already
//!   has.
//! * **Auth, bad request, unknown provider, malformed response, rate limit that
//!   survives back-off → surface.** These are contract and configuration
//!   errors. They do not fix themselves and hiding them wastes the operator's
//!   time.
//!
//! Either way the outcome is explicit in [`RerankOutcome`], so the API layer can
//! tell the caller which ranking it is actually looking at.
//!
//! # Providers
//!
//! Dispatch is an enum rather than a `dyn` trait: the set is closed, small, and
//! every arm is async, which would otherwise require `async-trait` and a boxed
//! future per call. Add an arm to extend it. A `Disabled` arm exists so an empty
//! API key is a configured no-op rather than a hard error — also Meilisearch's
//! choice (`personalization/mod.rs:333-345`).
//!
//! The Jev arm speaks TypeSafe AI's System One API
//! (`POST https://api.typesafe.ai/v1/systemone`, documented at
//! <https://docs.typesafe.ai/api>). The one-`noul`-per-document shape is the
//! approach used by `hev/jev-rerank` (Apache-2.0); asking a separate yes/no per
//! document rather than one `choice` across them is deliberate, because real
//! corpora have more than one relevant document and a `choice` forces a single
//! winner.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Map, Value};

/// Hard ceiling on documents per Jev request.
///
/// The `hev/jev-rerank` README reports a ~32k-token request budget and
/// "~30–50 typical passages per call", and uses 30; this project has not
/// measured the provider's limit itself. Longer candidate lists are split
/// into several calls, and the scores stay comparable across them because each
/// answer is an absolute probability rather than a rank within its batch.
pub const JEV_MAX_DOCS_PER_CALL: usize = 30;

/// Default concurrent in-flight provider calls.
///
/// Conservative on purpose. The provider's rate limits are undocumented; the
/// `hev/jev-rerank` README reports sustained 429s at about 24 requests in
/// flight (their observation, not ours). A search path that provokes rate
/// limiting is worse than a slightly slower one.
pub const DEFAULT_MAX_CONCURRENCY: usize = 8;

/// Ceiling on `rerank.window`.
///
/// The operator pays per judged document, and the caller picks the window — so
/// the ceiling is the server's, not the caller's. 300 is ten full provider
/// calls, two waves at the default concurrency.
pub const MAX_WINDOW: usize = 300;

/// Ceiling on `rerank.max_concurrency`. Kept under the ~24 in flight at which
/// `hev/jev-rerank` reports sustained 429s: a caller must not be able to push
/// the server's key into rate limiting.
pub const MAX_CONCURRENCY: usize = 16;

/// Ceiling on `rerank.max_doc_chars`. 30 documents at this size is already past
/// what a ~32k-token context holds; anything larger only buys a provider 4xx.
pub const MAX_DOC_CHARS: usize = 8_000;

/// Ceiling on `rerank.timeout_ms`: a search must not be able to pin a
/// connection for minutes on a third party's behalf.
pub const MAX_TIMEOUT_MS: u64 = 60_000;

/// How much of each document body to send.
///
/// Reranking reads titles and leading text; shipping whole documents burns the
/// context budget that limits how many candidates fit per call.
pub const DEFAULT_MAX_DOC_CHARS: usize = 1200;

/// Ceiling on `rerank.instructions`, in characters.
///
/// The instructions are not sent once: the System One wire format carries them
/// inside EVERY per-document question, so whatever the caller writes here is
/// multiplied by the window. Uncapped, a 1 MB string at `window: 40` and
/// `max_doc_chars: 1` put 40 MB on the wire to the provider (measured in
/// review), and an 8 MB one grew the node's peak RSS by 700 MB for a single
/// `size: 1` search — every other cost knob had a ceiling and this, the one
/// multiplied by the window, did not. 2,000 characters is a dozen sentences of
/// domain guidance; at the largest window (300) it is 600 KB of instructions,
/// well under the 4.8 MB of document text the same window may already carry.
pub const MAX_INSTRUCTIONS_CHARS: usize = 2_000;

/// Ceiling on the question — `rerank.query`, or the one read from the search
/// query. Sent once per provider call (at most ten calls per search), and
/// echoed in `_rerank.query`. A question is a sentence or a short paragraph;
/// anything longer is a document, and the judge's context is the scarce thing.
pub const MAX_QUERY_CHARS: usize = 4_000;

/// Ceiling on `rerank.model` and `rerank.provider`: identifiers, not prose.
pub const MAX_MODEL_CHARS: usize = 128;

/// Ceiling on the number of `rerank.fields` entries. Every entry is looked up
/// on every hit in the window, and entries that returned no text are echoed in
/// `_rerank.fields_without_text`.
pub const MAX_FIELDS: usize = 64;

/// Ceiling on one `rerank.fields` entry, in characters — a field path.
pub const MAX_FIELD_NAME_CHARS: usize = 256;

/// What `GET /_xerj/rerank` says leaves the node, as `data_egress`.
///
/// A privacy statement an operator — or an agent quoting it to one — will rely
/// on, so it is complete rather than flattering: reranking is the only
/// SEARCH-TIME feature that sends document text off the node, it is not the
/// only feature that sends text, and it is not the only outbound connection.
/// `tests/egress_inventory.rs` checks it names every path the engine source
/// contains, and that `docs/RERANK.md` quotes it verbatim.
pub const DATA_EGRESS: &str = "A search that carries a `rerank` block sends the text of up to \
     `window` hits, and the query, to the endpoint above. It is the only search-time feature \
     that sends document text off the node. Two other features send text off the node, both \
     operator configuration and off by default: `[embedding] default_endpoint` \
     (`--embed-mode proxy`) sends document text at write time and query text at search time \
     to an external embeddings API, and the WAL tap (`PUT /_xerj/wal_tap`) replays every \
     write on tapped indices to an external `_bulk` endpoint. The node's other outbound \
     connections carry no document or query text: the one-time HuggingFace model download \
     for `--embed-mode neural`, Raft messages (index names, mappings, shard assignments) \
     to the configured peers in cluster mode, and object storage — the `S3Backend` client, \
     which nothing on the segment path constructs today, and `xerj autoindex s3://` \
     (with `--watch`, on an interval), which names buckets and keys to the endpoint you \
     configure and reads objects IN.";

/// Ceiling on one provider response body, in bytes.
///
/// A legitimate System One answer for a full 30-document call is one small
/// object per document — a few kilobytes. The endpoint is a third party (or
/// whatever answers at the configured URL), and reading its body whole let it
/// choose how much memory one search allocates: `max_concurrency` calls in
/// flight, each as large as the peer cared to make it. 2 MiB is hundreds of
/// times a real answer; a larger body is a contract break (502), not an
/// allocation.
pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// How much of a non-2xx response body is read. Only its first 400 characters
/// reach the error message, so there is no reason to read more.
const ERROR_BODY_READ_BYTES: usize = 4 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum RerankError {
    #[error(
        "no rerank API key is configured on this server: set `api_key` under `[rerank]` in the \
         config file, or the {0} environment variable, and restart"
    )]
    MissingKey(&'static str),
    #[error("provider `{0}` is not supported")]
    UnknownProvider(String),
    #[error("transport: {0}")]
    Transport(String),
    #[error("provider returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("malformed provider response: {0}")]
    Malformed(String),
    #[error("invalid configuration: {0}")]
    Config(String),
    #[error("rerank deadline exceeded after {elapsed_ms}ms")]
    Deadline { elapsed_ms: u128 },
    /// The operator switched reranking off (`[rerank] enabled = false`).
    /// Distinct from [`Self::MissingKey`]: that one is "not set up yet", this
    /// one is "deliberately never" — reranking sends document text to a third
    /// party, and an operator must be able to forbid that outright.
    #[error(
        "reranking is disabled on this server (`[rerank] enabled = false`): it would send \
         document text to a third-party provider, and the operator has forbidden that"
    )]
    DisabledByOperator,
}

/// What the caller should do with a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Keep the engine's own ranking and report that reranking did not run.
    Degrade,
    /// Return the error to the caller. Configuration and contract faults do not
    /// fix themselves, and masking them costs the operator more than it saves.
    Surface,
}

impl RerankError {
    pub fn policy(&self) -> Policy {
        match self {
            // Ran out of time. The caller still has usable results.
            Self::Deadline { .. } => Policy::Degrade,
            // A transport fault that outlived the retries is indistinguishable
            // from "the provider is down"; the operator needs to know.
            Self::Transport(_) => Policy::Surface,
            Self::MissingKey(_)
            | Self::UnknownProvider(_)
            | Self::Config(_)
            | Self::Malformed(_)
            | Self::DisabledByOperator
            | Self::Status { .. } => Policy::Surface,
        }
    }

    /// Whether another attempt could plausibly succeed.
    ///
    /// 429 and 529 are the two codes TypeSafe documents as "back off and retry";
    /// 5xx is retried as a courtesy. A 4xx other than 429 is a contract error
    /// and retrying it only burns the deadline.
    pub fn retryable(&self) -> bool {
        match self {
            Self::Transport(_) => true,
            Self::Status { status, .. } => {
                *status == 429 || *status == 529 || (500..600).contains(status)
            }
            _ => false,
        }
    }
}

/// A wall-clock budget for the whole reranking stage.
///
/// Reranking sits inside a search request that the caller is waiting on, so the
/// stage needs its own ceiling independent of any single HTTP timeout: three
/// sequential batches each finishing just inside a 10s timeout is a 30s search.
#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    started: std::time::Instant,
    budget: Duration,
}

impl Deadline {
    pub fn new(budget: Duration) -> Self {
        Self {
            started: std::time::Instant::now(),
            budget,
        }
    }
    pub fn exceeded(&self) -> bool {
        self.started.elapsed() >= self.budget
    }
    pub fn remaining(&self) -> Duration {
        self.budget.saturating_sub(self.started.elapsed())
    }
    fn err(&self) -> RerankError {
        RerankError::Deadline {
            elapsed_ms: self.started.elapsed().as_millis(),
        }
    }
}

/// What actually happened, so the API layer never has to guess.
#[derive(Debug)]
pub enum RerankOutcome {
    /// Provider scored the window; `scores` is the new order.
    Reordered {
        scores: Vec<Scored>,
        /// Batches that failed while others succeeded. Ordering among scored
        /// documents is still correct — the probabilities are absolute — but the
        /// caller should know the window was not fully covered.
        partial_failures: usize,
        /// Tokens the provider billed for, summed over every batch that
        /// answered. Reranking is a paid call inside a search, so the caller
        /// gets the meter reading rather than having to infer it.
        usage: Usage,
    },
    /// Reranking did not run. The engine's order stands.
    Degraded { reason: String },
}

/// Token usage as the provider reports it. Zero when the provider omits it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Usage {
    fn add(&mut self, other: Usage) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
    }
}

/// One candidate handed to the reranker.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Caller-side position. Returned untouched so the caller can reorder its
    /// own hit array without matching on document contents.
    pub ordinal: usize,
    pub title: Option<String>,
    pub text: String,
}

/// A relevance probability for one candidate, in `0.0..=1.0`.
///
/// Carried as `f64` end to end. The provider answers a JSON number, the hit's
/// `_score` is an `f64` on the wire, and a threshold the caller wrote as `0.9`
/// has to compare equal to the `0.9` the response prints: an `f32` in the
/// middle turned that into `0.8999999761581421` and made a client-side
/// `_score >= 0.9` disagree with the server's own `rerank.min_score`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scored {
    pub ordinal: usize,
    pub score: f64,
}

/// Parsed `rerank` block from a search body.
#[derive(Debug, Clone)]
pub struct RerankConfig {
    pub provider: String,
    pub model: String,
    /// How many of the engine's top hits to rerank. The rest keep their order
    /// and sort below the reranked window.
    pub window: usize,
    /// Documents per provider call, capped at [`JEV_MAX_DOCS_PER_CALL`].
    pub batch: usize,
    /// Drop candidates scoring below this. `None` keeps all and only reorders.
    pub min_score: Option<f64>,
    pub max_concurrency: usize,
    pub max_doc_chars: usize,
    pub timeout: Duration,
    /// Natural-language relevance question. Overridable because "relevant" is
    /// domain-specific — a support corpus and a code corpus do not mean the
    /// same thing by it.
    pub instructions: Option<String>,
    /// The question to judge documents against. Required unless the caller's
    /// search query is a shape the API layer can read a plain string out of —
    /// guessing at the intent behind a `bool` tree would rank against the
    /// wrong question without anyone noticing.
    pub query: Option<String>,
    /// Returned fields sent to the provider. An EXHAUSTIVE allow-list: it is an
    /// egress control, so `["body"]` sends the body and not even the title.
    /// `None` sends the title, then the matching `_passage` when the response
    /// carries one, then every other returned top-level string field — right
    /// for small documents and wasteful for wide ones. The API layer reads the
    /// values; this crate only carries the names.
    pub fields: Option<Vec<String>>,
}

impl Default for RerankConfig {
    fn default() -> Self {
        Self {
            provider: "jev".to_string(),
            model: "jev-latest".to_string(),
            window: JEV_MAX_DOCS_PER_CALL,
            batch: JEV_MAX_DOCS_PER_CALL,
            min_score: None,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            max_doc_chars: DEFAULT_MAX_DOC_CHARS,
            timeout: Duration::from_secs(10),
            instructions: None,
            query: None,
            fields: None,
        }
    }
}

impl RerankConfig {
    /// Parse the `rerank` object from a search body.
    ///
    /// Unknown keys are rejected rather than ignored: a caller who misspells
    /// `min_score` and silently gets unpruned results has been misled, and that
    /// is the failure mode this project treats as a defect.
    pub fn from_json(v: &Value) -> Result<Self, RerankError> {
        let obj = v
            .as_object()
            .ok_or_else(|| RerankError::Config("`rerank` must be an object".into()))?;

        const KNOWN: &[&str] = &[
            "provider",
            "model",
            "window",
            "batch",
            "min_score",
            "max_concurrency",
            "max_doc_chars",
            "timeout_ms",
            "instructions",
            "query",
            "fields",
        ];
        for k in obj.keys() {
            if !KNOWN.contains(&k.as_str()) {
                // Named, but never echoed whole: the key is the caller's, and a
                // megabyte-long one must not come back in the 400.
                let shown = clip(k, MAX_ECHOED_NAME_CHARS);
                let ellipsis = if shown.len() < k.len() { "…" } else { "" };
                return Err(RerankError::Config(format!(
                    "unknown `rerank` field `{shown}{ellipsis}`; supported: {}",
                    KNOWN.join(", ")
                )));
            }
        }

        let mut cfg = Self::default();

        if let Some(p) = obj.get("provider") {
            cfg.provider = bounded_string("provider", p, MAX_MODEL_CHARS)?;
        }
        if let Some(m) = obj.get("model") {
            cfg.model = bounded_string("model", m, MAX_MODEL_CHARS)?;
        }
        if let Some(w) = obj.get("window") {
            let w = w
                .as_u64()
                .ok_or_else(|| RerankError::Config("`rerank.window` must be a number".into()))?;
            if w == 0 {
                return Err(RerankError::Config("`rerank.window` must be > 0".into()));
            }
            if w as usize > MAX_WINDOW {
                return Err(RerankError::Config(format!(
                    "`rerank.window` must be <= {MAX_WINDOW}: every document in the window is \
                     a paid provider judgement"
                )));
            }
            cfg.window = w as usize;
        }
        if let Some(b) = obj.get("batch") {
            let b = b
                .as_u64()
                .ok_or_else(|| RerankError::Config("`rerank.batch` must be a number".into()))?;
            if b == 0 {
                return Err(RerankError::Config("`rerank.batch` must be > 0".into()));
            }
            // Clamp rather than reject: the ceiling is a property of the
            // provider, not a mistake by the caller.
            cfg.batch = (b as usize).min(JEV_MAX_DOCS_PER_CALL);
        }
        if let Some(t) = obj.get("min_score") {
            let t = t
                .as_f64()
                .ok_or_else(|| RerankError::Config("`rerank.min_score` must be a number".into()))?;
            if !(0.0..=1.0).contains(&t) {
                return Err(RerankError::Config(
                    "`rerank.min_score` must be between 0 and 1 — it is a probability".into(),
                ));
            }
            cfg.min_score = Some(t);
        }
        if let Some(c) = obj.get("max_concurrency") {
            let c = c.as_u64().ok_or_else(|| {
                RerankError::Config("`rerank.max_concurrency` must be a number".into())
            })?;
            if c == 0 || c as usize > MAX_CONCURRENCY {
                return Err(RerankError::Config(format!(
                    "`rerank.max_concurrency` must be between 1 and {MAX_CONCURRENCY}"
                )));
            }
            cfg.max_concurrency = c as usize;
        }
        if let Some(c) = obj.get("max_doc_chars") {
            let c = c.as_u64().ok_or_else(|| {
                RerankError::Config("`rerank.max_doc_chars` must be a number".into())
            })?;
            if c == 0 || c as usize > MAX_DOC_CHARS {
                return Err(RerankError::Config(format!(
                    "`rerank.max_doc_chars` must be between 1 and {MAX_DOC_CHARS}"
                )));
            }
            cfg.max_doc_chars = c as usize;
        }
        if let Some(t) = obj.get("timeout_ms") {
            let t = t.as_u64().ok_or_else(|| {
                RerankError::Config("`rerank.timeout_ms` must be a number".into())
            })?;
            if t == 0 || t > MAX_TIMEOUT_MS {
                return Err(RerankError::Config(format!(
                    "`rerank.timeout_ms` must be between 1 and {MAX_TIMEOUT_MS}"
                )));
            }
            cfg.timeout = Duration::from_millis(t);
        }
        if let Some(i) = obj.get("instructions") {
            // The one string that is multiplied by the window: it rides inside
            // every per-document question (see `build_jev_request`).
            cfg.instructions = Some(bounded_string("instructions", i, MAX_INSTRUCTIONS_CHARS)?);
        }

        if let Some(q) = obj.get("query") {
            let q = bounded_string("query", q, MAX_QUERY_CHARS)?;
            if q.trim().is_empty() {
                return Err(RerankError::Config(
                    "`rerank.query` must not be empty".into(),
                ));
            }
            cfg.query = Some(q);
        }
        if let Some(f) = obj.get("fields") {
            let arr = f.as_array().ok_or_else(|| {
                RerankError::Config("`rerank.fields` must be an array of field names".into())
            })?;
            if arr.len() > MAX_FIELDS {
                return Err(RerankError::Config(format!(
                    "`rerank.fields` must name at most {MAX_FIELDS} fields (got {}): every name \
                     is looked up on every hit in the window",
                    arr.len()
                )));
            }
            let mut fields = Vec::with_capacity(arr.len());
            for v in arr {
                let name = v.as_str().ok_or_else(|| {
                    RerankError::Config("`rerank.fields` entries must be strings".into())
                })?;
                if longer_than(name, MAX_FIELD_NAME_CHARS) {
                    return Err(RerankError::Config(format!(
                        "`rerank.fields` entries must be at most {MAX_FIELD_NAME_CHARS} \
                         characters — they are field paths (one entry is {} bytes)",
                        name.len()
                    )));
                }
                fields.push(name.to_string());
            }
            if fields.is_empty() {
                return Err(RerankError::Config(
                    "`rerank.fields` must not be empty".into(),
                ));
            }
            cfg.fields = Some(fields);
        }

        Ok(cfg)
    }

    fn question(&self) -> &str {
        self.instructions.as_deref().unwrap_or(
            "Does this document contain information that answers the query? \
             Judge only whether it is relevant to the query, not whether it is \
             well written.",
        )
    }
}

/// Truncate on a char boundary. Byte slicing a `&str` here would panic on any
/// multi-byte character — the exact defect that crash-looped `autoindex` on
/// non-ASCII input, so it is not repeated.
pub fn clip(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}

/// Whether `s` is longer than `max` characters, without walking all of it: a
/// caller can send a 100 MB string, and the answer is known after `max` of
/// them. Byte length is a free first test — `max` bytes cannot hold more than
/// `max` characters.
pub fn longer_than(s: &str, max: usize) -> bool {
    s.len() > max && s.char_indices().nth(max).is_some()
}

/// How much of a caller-supplied NAME (an unknown key) a refusal echoes back.
const MAX_ECHOED_NAME_CHARS: usize = 64;

/// A `rerank.<key>` string under its server-side ceiling.
///
/// The refusal names the field and the limit and reports the size in bytes
/// (known without reading the string); it never echoes the value, which is the
/// caller's and may be megabytes.
fn bounded_string(key: &str, v: &Value, max_chars: usize) -> Result<String, RerankError> {
    let s = v
        .as_str()
        .ok_or_else(|| RerankError::Config(format!("`rerank.{key}` must be a string")))?;
    if longer_than(s, max_chars) {
        return Err(RerankError::Config(format!(
            "`rerank.{key}` must be at most {max_chars} characters (this one is {} bytes): \
             it is sent to the provider on the operator's key, so the ceiling is the server's",
            s.len()
        )));
    }
    Ok(s.to_string())
}

/// The ceiling on the question, for a question the API layer read out of the
/// search query rather than out of `rerank.query`. Same string, same wire, same
/// limit — and the refusal says what to do about it.
pub fn check_inferred_question(q: &str) -> Result<(), RerankError> {
    if longer_than(q, MAX_QUERY_CHARS) {
        return Err(RerankError::Config(format!(
            "the question read from the search query is longer than the {MAX_QUERY_CHARS} \
             characters `rerank.query` allows (this one is {} bytes). Pass a shorter question \
             as `rerank.query`: it is sent to the provider with every call, and the judge's \
             context is what limits how many documents fit in one",
            q.len()
        )));
    }
    Ok(())
}

/// Remove the provider key from text that is about to reach a caller or a log.
///
/// A provider's error body is returned to whoever ran the search (the 502's
/// `reason`) and written to the log, and that caller is not necessarily the
/// operator who owns the key. Some APIs echo the credential in an auth error
/// ("invalid key: sk-…"). Without this, any principal allowed to search could
/// read the operator's key by provoking one.
fn redact_key(text: &str, api_key: &str) -> String {
    if api_key.is_empty() {
        return text.to_string();
    }
    text.replace(api_key, "<redacted>")
}

/// The part of a provider's error body that reaches the caller: the key
/// redacted FIRST, then cut to 400 characters. Cutting first would leave a key
/// that straddles the cut as a prefix `redact_key` no longer recognises — the
/// first characters of the operator's key, in a search response.
fn error_body(text: &str, api_key: &str) -> String {
    clip(&redact_key(text, api_key), 400).to_string()
}

/// Build one System One request body for a batch of candidates.
///
/// Split out from the HTTP call so the wire format is testable without a network
/// or an API key.
///
/// Every caller-supplied string is cut to its ceiling HERE as well as refused
/// in [`RerankConfig::from_json`]. `RerankConfig`'s fields are public, so the
/// parser is not the only way to build one, and what one call can put on the
/// wire — on the operator's key, to a third party — should be bounded by
/// construction rather than by every caller remembering to validate. For a
/// request that came through the parser the cuts change nothing.
pub fn build_jev_request(query: &str, batch: &[Candidate], cfg: &RerankConfig) -> Value {
    let mut documents = Map::new();
    let mut questions = Map::new();
    let instructions = clip(cfg.question(), MAX_INSTRUCTIONS_CHARS);
    let max_doc_chars = cfg.max_doc_chars.min(MAX_DOC_CHARS);

    for cand in batch {
        // Keyed by the caller's ordinal, so a reordered or partial response
        // still maps back to the right hit.
        let key = format!("d{}", cand.ordinal);
        let mut doc = Map::new();
        if let Some(t) = &cand.title {
            doc.insert("title".into(), json!(clip(t, max_doc_chars)));
        }
        doc.insert("text".into(), json!(clip(&cand.text, max_doc_chars)));
        documents.insert(key.clone(), Value::Object(doc));

        questions.insert(
            key,
            json!({
                "type": "noul",
                "instructions": instructions,
                "criteria": {
                    "true":  "The document is relevant to the query.",
                    "false": "The document is not relevant to the query."
                }
            }),
        );
    }

    json!({
        "state": {
            "query": clip(query, MAX_QUERY_CHARS),
            "documents": Value::Object(documents),
        },
        "model": clip(&cfg.model, MAX_MODEL_CHARS),
        "questions": Value::Object(questions),
    })
}

#[derive(Deserialize)]
struct JevAnswer {
    #[serde(default)]
    noul: Option<f64>,
}

#[derive(Deserialize)]
struct JevResponse {
    #[serde(default)]
    answers: std::collections::HashMap<String, JevAnswer>,
    /// Read as loose JSON on purpose. `usage` is advisory metering and not part
    /// of the ranking contract: typed as `{input_tokens: u64, output_tokens:
    /// u64}` it made a provider reporting `-5` (or a float, a string, `null`)
    /// fail the whole call with a 502 and no hits, although every verdict in
    /// `answers` was valid. `answers` stays strict — that IS the contract.
    #[serde(default)]
    usage: Value,
}

/// The most tokens one provider call can plausibly bill for.
///
/// One call carries at most 30 documents of `2 × MAX_DOC_CHARS` characters plus
/// the instructions, and the question once: about 544,000 characters. Even at
/// several tokens per character that is a few million, so ten million is past
/// anything a call can produce. A larger figure is not metering, it is a
/// provider bug (review's test value was 2^63) — and because the counts feed a
/// Prometheus counter that only ever goes up, one such response would poison
/// `xerj_rerank_provider_tokens_total` until the node restarts.
pub const MAX_PLAUSIBLE_TOKENS_PER_CALL: u64 = 10_000_000;

/// One token count out of a provider `usage` block: a non-negative number, or
/// zero. Zero rather than a guess — the meter under-reads instead of inventing.
///
/// A count above [`MAX_PLAUSIBLE_TOKENS_PER_CALL`] is also zero: it is not one a
/// call can produce, and the counter it would feed never goes back down.
fn token_count(usage: &Value, key: &str) -> u64 {
    let n = match usage.get(key) {
        Some(n) => n.as_u64().unwrap_or_else(|| {
            n.as_f64()
                .filter(|f| f.is_finite() && *f >= 0.0)
                .map(|f| f as u64)
                .unwrap_or(0)
        }),
        None => 0,
    };
    if n > MAX_PLAUSIBLE_TOKENS_PER_CALL {
        0
    } else {
        n
    }
}

/// Read a provider response body, stopping at `cap` bytes.
///
/// Returns the text read and whether the body went past `cap` (either by its
/// declared `Content-Length`, checked before reading anything, or by what
/// arrived). Past the cap nothing more is read: the connection is dropped with
/// the response. Invalid UTF-8 is replaced rather than refused — the JSON parse
/// that follows is what judges the content.
async fn read_capped(
    mut resp: reqwest::Response,
    cap: usize,
    deadline: &Deadline,
) -> Result<(String, bool), RerankError> {
    let io_err = |e: reqwest::Error| {
        if e.is_timeout() {
            deadline.err()
        } else {
            RerankError::Transport(e.to_string())
        }
    };
    if resp.content_length().is_some_and(|n| n > cap as u64) {
        return Ok((String::new(), true));
    }
    let mut buf: Vec<u8> = Vec::new();
    let mut oversized = false;
    while let Some(chunk) = resp.chunk().await.map_err(io_err)? {
        let room = cap - buf.len();
        if chunk.len() > room {
            buf.extend_from_slice(&chunk[..room]);
            oversized = true;
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    Ok((String::from_utf8_lossy(&buf).into_owned(), oversized))
}

/// Parse a System One response into scores keyed by the caller's ordinals.
///
/// A missing or non-`noul` answer is skipped rather than defaulted to zero:
/// inventing a "not relevant" verdict the model never gave would silently
/// demote a document.
pub fn parse_jev_response(body: &str) -> Result<Vec<Scored>, RerankError> {
    parse_jev_response_with_usage(body).map(|(scores, _)| scores)
}

/// [`parse_jev_response`], plus the `usage` block the provider bills against.
pub fn parse_jev_response_with_usage(body: &str) -> Result<(Vec<Scored>, Usage), RerankError> {
    let parsed: JevResponse =
        serde_json::from_str(body).map_err(|e| RerankError::Malformed(e.to_string()))?;
    let usage = Usage {
        input_tokens: token_count(&parsed.usage, "input_tokens"),
        output_tokens: token_count(&parsed.usage, "output_tokens"),
    };

    let mut out = Vec::with_capacity(parsed.answers.len());
    for (key, ans) in parsed.answers {
        let Some(ordinal) = key.strip_prefix('d').and_then(|n| n.parse::<usize>().ok()) else {
            continue;
        };
        let Some(noul) = ans.noul else { continue };
        // A probability outside 0..=1 is not a probability. Clamp rather than
        // reject the whole batch: the ordering signal is still usable, and the
        // caller's `min_score` contract (0..=1) must keep meaning something.
        out.push(Scored {
            ordinal,
            score: noul.clamp(0.0, 1.0),
        });
    }
    Ok((out, usage))
}

/// Keep only the verdicts for documents that were in `batch`.
///
/// The provider is a third party, and a call is answered per batch: an answer
/// keyed to a document another batch sent, or to a key nobody sent, is not a
/// verdict on anything this call asked about. Without this filter a provider
/// that echoed `d0..d59` on every call scored a 30-document window as 90
/// judged documents — three verdicts per hit, the last one winning, and the
/// operator's billing meter (`judged`) tripled. One verdict per ordinal per
/// batch, in the batch's own order, so `judged` counts documents rather than
/// answers.
fn keep_verdicts_for(batch: &[Candidate], scores: Vec<Scored>) -> Vec<Scored> {
    let mut by_ordinal: std::collections::HashMap<usize, f64> =
        std::collections::HashMap::with_capacity(scores.len());
    for s in scores {
        by_ordinal.insert(s.ordinal, s.score);
    }
    batch
        .iter()
        .filter_map(|c| {
            by_ordinal.get(&c.ordinal).map(|&score| Scored {
                ordinal: c.ordinal,
                score,
            })
        })
        .collect()
}

/// Reorder `candidates` by provider score.
///
/// Candidates the provider did not score keep their original relative order and
/// sort *below* every scored one — an unscored document is not evidence of
/// irrelevance, but it cannot be ranked against ones that were scored.
pub fn apply_scores(
    candidates: &[Candidate],
    scores: &[Scored],
    min_score: Option<f64>,
) -> Vec<Scored> {
    let mut by_ordinal: std::collections::HashMap<usize, f64> =
        std::collections::HashMap::with_capacity(scores.len());
    for s in scores {
        by_ordinal.insert(s.ordinal, s.score);
    }

    let mut scored: Vec<Scored> = Vec::with_capacity(candidates.len());
    for (rank, cand) in candidates.iter().enumerate() {
        if let Some(&score) = by_ordinal.get(&cand.ordinal) {
            if let Some(min) = min_score {
                if score < min {
                    continue;
                }
            }
            scored.push(Scored {
                ordinal: cand.ordinal,
                score,
            });
        } else if min_score.is_none() {
            // Negative score keeps unscored hits after every scored one while
            // preserving their engine order via the stable sort below.
            scored.push(Scored {
                ordinal: cand.ordinal,
                score: -1.0 - (rank as f64),
            });
        }
    }

    // Stable, so equal probabilities keep the engine's ordering — which is the
    // right tie-break: BM25 already ranked them.
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored
}

/// Where a resolved setting came from. Reported by `GET /_xerj/rerank` so an
/// operator can tell which of two places is actually in force — without the
/// endpoint ever having to print the value itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingSource {
    /// The server's config file (`[rerank]`).
    Config,
    /// The process environment.
    Env,
    /// Neither — the built-in default (endpoint) or nothing at all (key).
    Default,
}

impl SettingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Env => "env",
            Self::Default => "default",
        }
    }
}

/// The server's resolved provider settings: the injection seam between
/// configuration and a [`Provider`].
///
/// Resolved ONCE at startup and carried in the API layer's shared state. Two
/// reasons it is not read from the environment per request, which is what the
/// first draft did:
///
/// * tests run in parallel threads of one process, and a test that has to
///   `set_var("TYPESAFE_API_KEY")` races every other test that reads it;
/// * one `reqwest::Client` lives here, so consecutive searches share a
///   connection pool instead of paying a TLS handshake per rerank.
///
/// `Debug` is written by hand so the key can never reach a log line.
#[derive(Clone)]
pub struct ProviderSettings {
    /// `false` refuses every rerank request ([`RerankError::DisabledByOperator`]).
    pub enabled: bool,
    api_key: Option<String>,
    key_source: SettingSource,
    endpoint: String,
    endpoint_source: SettingSource,
    client: reqwest::Client,
}

impl std::fmt::Debug for ProviderSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderSettings")
            .field("enabled", &self.enabled)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("key_source", &self.key_source)
            .field("endpoint", &self.endpoint_for_display())
            .field("endpoint_source", &self.endpoint_source)
            .finish()
    }
}

impl Default for ProviderSettings {
    /// No key, the public endpoint, enabled: a server that has not been set up.
    fn default() -> Self {
        Self::resolve(true, "", "", None, None)
    }
}

impl ProviderSettings {
    /// Pure resolution rule, split from the environment read so it can be
    /// tested without mutating process-wide state.
    ///
    /// Precedence for both values: a non-empty config field wins, then the
    /// environment, then the default. Explicit configuration beats the ambient
    /// environment for the reason `cluster.auth_secret` does — a stray variable
    /// in the service's environment must not silently re-point where document
    /// text is sent.
    pub fn resolve(
        enabled: bool,
        config_key: &str,
        config_endpoint: &str,
        env_key: Option<String>,
        env_endpoint: Option<String>,
    ) -> Self {
        let non_empty =
            |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let (api_key, key_source) =
            match (non_empty(Some(config_key.to_string())), non_empty(env_key)) {
                (Some(k), _) => (Some(k), SettingSource::Config),
                (None, Some(k)) => (Some(k), SettingSource::Env),
                (None, None) => (None, SettingSource::Default),
            };
        let (endpoint, endpoint_source) = match (
            non_empty(Some(config_endpoint.to_string())),
            non_empty(env_endpoint),
        ) {
            (Some(e), _) => (e, SettingSource::Config),
            (None, Some(e)) => (e, SettingSource::Env),
            (None, None) => (
                JevProvider::DEFAULT_ENDPOINT.to_string(),
                SettingSource::Default,
            ),
        };
        Self {
            enabled,
            api_key,
            key_source,
            endpoint,
            endpoint_source,
            client: reqwest::Client::new(),
        }
    }

    /// Config fields first, then `TYPESAFE_API_KEY` / `TYPESAFE_ENDPOINT`.
    pub fn from_config_and_env(enabled: bool, config_key: &str, config_endpoint: &str) -> Self {
        Self::resolve(
            enabled,
            config_key,
            config_endpoint,
            std::env::var(JevProvider::ENV_KEY).ok(),
            std::env::var(JevProvider::ENV_ENDPOINT).ok(),
        )
    }

    /// Settings for a known key and endpoint — what a test points at its stub.
    pub fn with_key_and_endpoint(api_key: &str, endpoint: &str) -> Self {
        Self::resolve(true, api_key, endpoint, None, None)
    }

    /// Whether a key is in force. Never the key.
    pub fn has_key(&self) -> bool {
        self.api_key.is_some()
    }
    pub fn key_source(&self) -> Option<SettingSource> {
        self.api_key.as_ref().map(|_| self.key_source)
    }
    pub fn endpoint_source(&self) -> SettingSource {
        self.endpoint_source
    }

    /// The endpoint with anything secret-shaped removed: userinfo, query
    /// string and fragment. An operator who put a token in the URL should not
    /// find it echoed by a status endpoint.
    pub fn endpoint_for_display(&self) -> String {
        match reqwest::Url::parse(&self.endpoint) {
            Ok(mut u) => {
                let _ = u.set_username("");
                let _ = u.set_password(None);
                u.set_query(None);
                u.set_fragment(None);
                u.to_string()
            }
            Err(_) => "<unparseable endpoint>".to_string(),
        }
    }
}

/// A configured reranking backend.
pub enum Provider {
    Jev(JevProvider),
    /// Configured but inert. An empty API key lands here rather than erroring,
    /// so a deployment that has not set one yet still serves searches.
    Disabled,
}

impl Provider {
    /// Build a provider from config plus the ambient environment.
    ///
    /// Kept for callers without server state (a CLI, a benchmark). A server
    /// uses [`Self::from_settings`], which never touches the environment.
    pub fn from_config(cfg: &RerankConfig) -> Result<Self, RerankError> {
        Self::from_settings(cfg, &ProviderSettings::from_config_and_env(true, "", ""))
    }

    /// Build a provider from the request's `rerank` block and the server's
    /// resolved settings. The environment is not consulted.
    pub fn from_settings(
        cfg: &RerankConfig,
        settings: &ProviderSettings,
    ) -> Result<Self, RerankError> {
        match cfg.provider.as_str() {
            "none" | "disabled" => Ok(Self::Disabled),
            "jev" | "typesafe" => {
                if !settings.enabled {
                    return Err(RerankError::DisabledByOperator);
                }
                JevProvider::from_settings(settings).map(Self::Jev)
            }
            other => Err(RerankError::UnknownProvider(other.to_string())),
        }
    }

    /// Score and reorder `candidates`, honouring `deadline`.
    ///
    /// Never returns `Err` for a degradable cause — those come back as
    /// [`RerankOutcome::Degraded`] so the caller does not have to re-derive the
    /// policy from the error type.
    pub async fn rerank(
        &self,
        query: &str,
        candidates: &[Candidate],
        cfg: &RerankConfig,
        deadline: &Deadline,
    ) -> Result<RerankOutcome, RerankError> {
        if candidates.is_empty() {
            return Ok(RerankOutcome::Reordered {
                scores: Vec::new(),
                partial_failures: 0,
                usage: Usage::default(),
            });
        }
        if deadline.exceeded() {
            return Ok(RerankOutcome::Degraded {
                reason: "deadline exceeded before the provider was called".into(),
            });
        }
        match self {
            Self::Disabled => Ok(RerankOutcome::Degraded {
                reason: "rerank provider disabled (`rerank.provider` is `none`)".into(),
            }),
            Self::Jev(p) => match p.rerank(query, candidates, cfg, deadline).await {
                Ok(outcome) => Ok(outcome),
                Err(e) if e.policy() == Policy::Degrade => Ok(RerankOutcome::Degraded {
                    reason: e.to_string(),
                }),
                Err(e) => Err(e),
            },
        }
    }
}

/// TypeSafe AI System One client.
pub struct JevProvider {
    client: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl JevProvider {
    pub const ENV_KEY: &'static str = "TYPESAFE_API_KEY";
    pub const ENV_ENDPOINT: &'static str = "TYPESAFE_ENDPOINT";
    pub const DEFAULT_ENDPOINT: &'static str = "https://api.typesafe.ai/v1/systemone";

    /// Shares the settings' HTTP client, so the pool outlives this request.
    pub fn from_settings(settings: &ProviderSettings) -> Result<Self, RerankError> {
        let api_key = settings
            .api_key
            .clone()
            .ok_or(RerankError::MissingKey(Self::ENV_KEY))?;
        Ok(Self {
            client: settings.client.clone(),
            endpoint: settings.endpoint.clone(),
            api_key,
        })
    }

    pub fn new(api_key: String, endpoint: String, timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            endpoint,
            api_key,
        }
    }

    /// One batch, retried while the deadline allows.
    ///
    /// Back-off shape follows Meilisearch's Cohere client
    /// (`personalization/mod.rs:308-321`): exponential in the attempt, with a
    /// flat additional penalty for rate limiting, because a 429 means the
    /// service wants distance rather than a slightly later retry.
    async fn call_batch_retrying(
        &self,
        query: &str,
        batch: &[Candidate],
        cfg: &RerankConfig,
        deadline: &Deadline,
    ) -> Result<(Vec<Scored>, Usage), RerankError> {
        const MAX_ATTEMPTS: u32 = 3;
        let mut attempt = 0;
        loop {
            if deadline.exceeded() {
                return Err(deadline.err());
            }
            match self.call_batch(query, batch, cfg, deadline).await {
                Ok(scores) => return Ok(scores),
                Err(e) => {
                    attempt += 1;
                    if attempt >= MAX_ATTEMPTS || !e.retryable() {
                        return Err(e);
                    }
                    let base = Duration::from_millis(10u64.pow(attempt));
                    let backoff = match &e {
                        RerankError::Status { status: 429, .. } => {
                            base + Duration::from_millis(100)
                        }
                        _ => base,
                    };
                    // Never sleep past the budget: a deadline that expires mid
                    // back-off should degrade, not wake up to fail.
                    if backoff >= deadline.remaining() {
                        return Err(deadline.err());
                    }
                    tracing::debug!(
                        attempt,
                        backoff_ms = backoff.as_millis(),
                        error = %e,
                        "retrying rerank batch"
                    );
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }

    async fn call_batch(
        &self,
        query: &str,
        batch: &[Candidate],
        cfg: &RerankConfig,
        deadline: &Deadline,
    ) -> Result<(Vec<Scored>, Usage), RerankError> {
        let body = build_jev_request(query, batch, cfg);
        // Per request, not per client: the client is shared across searches
        // (see `ProviderSettings`), and each search brings its own budget. The
        // HTTP timeout is whatever is left of the stage deadline, so a call
        // can never outlive the search that is waiting on it.
        let http_timeout = cfg
            .timeout
            .min(deadline.remaining())
            .max(Duration::from_millis(1));
        let resp = self
            .client
            .post(&self.endpoint)
            .timeout(http_timeout)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                // A timeout is the deadline speaking, not the network: report
                // it as one so the policy degrades instead of surfacing.
                if e.is_timeout() {
                    deadline.err()
                } else {
                    RerankError::Transport(e.to_string())
                }
            })?;

        let status = resp.status();
        let cap = if status.is_success() {
            MAX_RESPONSE_BYTES
        } else {
            ERROR_BODY_READ_BYTES
        };
        let (text, oversized) = read_capped(resp, cap, deadline).await?;

        if status.is_success() && oversized {
            return Err(RerankError::Malformed(format!(
                "response body larger than {MAX_RESPONSE_BYTES} bytes; a verdict for \
                 {JEV_MAX_DOCS_PER_CALL} documents is a few kilobytes"
            )));
        }
        if !status.is_success() {
            // 429 and 529 are the documented back-off codes. Surfaced rather
            // than retried here: the caller is inside a search request with a
            // deadline, and fail-open beats spending it on retries.
            return Err(RerankError::Status {
                status: status.as_u16(),
                body: error_body(&text, &self.api_key),
            });
        }
        let (scores, usage) = parse_jev_response_with_usage(&text)?;
        Ok((keep_verdicts_for(batch, scores), usage))
    }

    pub async fn rerank(
        &self,
        query: &str,
        candidates: &[Candidate],
        cfg: &RerankConfig,
        deadline: &Deadline,
    ) -> Result<RerankOutcome, RerankError> {
        if candidates.is_empty() {
            return Ok(RerankOutcome::Reordered {
                scores: Vec::new(),
                partial_failures: 0,
                usage: Usage::default(),
            });
        }
        let batch_size = cfg.batch.clamp(1, JEV_MAX_DOCS_PER_CALL);
        let batches: Vec<Vec<Candidate>> =
            candidates.chunks(batch_size).map(|c| c.to_vec()).collect();

        let mut scores = Vec::with_capacity(candidates.len());
        let mut usage = Usage::default();
        let mut partial_failures = 0usize;
        let mut first_hard_error: Option<RerankError> = None;

        // Bounded waves rather than unbounded fan-out: the provider rate-limits,
        // and a search that trips a 429 is slower than one that paced itself.
        let mut dispatched = 0usize;
        for wave in batches.chunks(cfg.max_concurrency.max(1)) {
            if deadline.exceeded() {
                break;
            }
            dispatched += wave.len();
            let mut set = tokio::task::JoinSet::new();
            for batch in wave {
                let this = self.clone_for_task();
                let q = query.to_string();
                let c = cfg.clone();
                let b = batch.clone();
                let d = *deadline;
                set.spawn(async move { this.call_batch_retrying(&q, &b, &c, &d).await });
            }
            while let Some(joined) = set.join_next().await {
                match joined {
                    Ok(Ok((mut part, used))) => {
                        scores.append(&mut part);
                        usage.add(used);
                    }
                    // One failed batch must not discard the batches that
                    // succeeded: their scores are absolute probabilities, so a
                    // partial result is still correctly ordered.
                    Ok(Err(e)) => {
                        partial_failures += 1;
                        tracing::warn!(error = %e, "rerank batch failed");
                        if first_hard_error.is_none() && e.policy() == Policy::Surface {
                            first_hard_error = Some(e);
                        }
                    }
                    Err(e) => {
                        partial_failures += 1;
                        tracing::warn!(error = %e, "rerank batch task panicked");
                    }
                }
            }
        }

        // A batch the deadline stopped from ever being sent left its documents
        // exactly as unjudged as a batch that failed. Counting it makes the
        // block auditable: `judged` plus `partial_failures` batches accounts
        // for the whole window, instead of a window of 36 with 10 judged and
        // one failure and 16 documents unexplained.
        partial_failures += batches.len() - dispatched;

        // Nothing came back at all. Report why rather than silently handing back
        // the engine's order as if it had been reranked.
        if scores.is_empty() {
            if let Some(e) = first_hard_error {
                return Err(e);
            }
            return Ok(RerankOutcome::Degraded {
                reason: if deadline.exceeded() {
                    format!("deadline exceeded after {}ms", deadline.budget.as_millis())
                } else {
                    "provider returned no scores".to_string()
                },
            });
        }

        Ok(RerankOutcome::Reordered {
            scores,
            partial_failures,
            usage,
        })
    }

    fn clone_for_task(&self) -> Self {
        // `reqwest::Client` is an Arc internally; cloning shares the pool.
        Self {
            client: self.client.clone(),
            endpoint: self.endpoint.clone(),
            api_key: self.api_key.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cands(n: usize) -> Vec<Candidate> {
        (0..n)
            .map(|i| Candidate {
                ordinal: i,
                title: Some(format!("title {i}")),
                text: format!("body {i}"),
            })
            .collect()
    }

    #[test]
    fn a_provider_error_that_echoes_the_key_is_redacted() {
        let body = r#"{"error":"invalid api key: sk-operator-secret (Bearer sk-operator-secret)"}"#;
        let out = redact_key(body, "sk-operator-secret");
        assert!(!out.contains("sk-operator-secret"), "{out}");
        assert_eq!(out.matches("<redacted>").count(), 2, "{out}");
        // Nothing to redact leaves the text alone, and an empty key is not a
        // pattern that matches between every character.
        assert_eq!(redact_key("rate limited", "sk-x"), "rate limited");
        assert_eq!(redact_key("rate limited", ""), "rate limited");
    }

    /// The error body is cut to 400 characters for the caller. A key that
    /// straddles that cut must not survive as a prefix: cutting first left
    /// `sk-operator-sec…` for `redact_key` to miss, in a search response.
    #[test]
    fn a_key_straddling_the_error_cut_leaks_no_prefix() {
        let key = "sk-operator-secret-0123456789";
        for pad in 380..400 {
            let body = format!("{}{key} trailing", "x".repeat(pad));
            let out = error_body(&body, key);
            assert!(out.chars().count() <= 400, "pad {pad}: {out}");
            assert!(
                !out.contains("sk-op"),
                "pad {pad} leaked a key prefix: {out}"
            );
        }
    }

    /// A 2xx body past [`MAX_RESPONSE_BYTES`] is a contract break, not an
    /// allocation: whatever answers at the endpoint does not get to choose how
    /// much memory a search uses. A body inside the ceiling still parses.
    #[test]
    fn an_oversized_provider_response_is_refused_not_read_whole() {
        use std::io::{Read, Write};
        fn serve(body: String, chunked: bool) -> String {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            std::thread::spawn(move || {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = [0u8; 65536];
                let _ = s.read(&mut buf);
                let head = if chunked {
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     transfer-encoding: chunked\r\nconnection: close\r\n\r\n"
                        .to_string()
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    )
                };
                let _ = s.write_all(head.as_bytes());
                if chunked {
                    // No Content-Length to refuse up front: the cap must hold
                    // while the body streams in.
                    for piece in body.as_bytes().chunks(64 * 1024) {
                        let _ = s.write_all(format!("{:x}\r\n", piece.len()).as_bytes());
                        let _ = s.write_all(piece);
                        let _ = s.write_all(b"\r\n");
                    }
                    let _ = s.write_all(b"0\r\n\r\n");
                } else {
                    let _ = s.write_all(body.as_bytes());
                }
            });
            format!("http://{addr}/v1/systemone")
        }
        let huge = format!(
            r#"{{"answers":{{"d0":{{"noul":0.9}}}},"padding":"{}"}}"#,
            "x".repeat(MAX_RESPONSE_BYTES)
        );
        let small = r#"{"answers":{"d0":{"noul":0.9}}}"#.to_string();
        let cfg = RerankConfig::default();
        let rt = rt();
        for chunked in [false, true] {
            let p = JevProvider::new(
                "k".into(),
                serve(huge.clone(), chunked),
                Duration::from_secs(10),
            );
            let d = Deadline::new(Duration::from_secs(10));
            let err = rt
                .block_on(p.call_batch("q", &cands(1), &cfg, &d))
                .expect_err("an oversized body must not parse");
            assert!(
                matches!(&err, RerankError::Malformed(m) if m.contains("larger than")),
                "chunked={chunked}: {err}"
            );
            assert_eq!(err.policy(), Policy::Surface, "chunked={chunked}");

            let p = JevProvider::new(
                "k".into(),
                serve(small.clone(), chunked),
                Duration::from_secs(10),
            );
            let (scores, _) = rt
                .block_on(p.call_batch("q", &cands(1), &cfg, &d))
                .expect("a body inside the ceiling parses");
            assert_eq!(scores.len(), 1, "chunked={chunked}");
        }
    }

    #[test]
    fn request_matches_documented_wire_format() {
        let cfg = RerankConfig::default();
        let body = build_jev_request("vitamin d bone density", &cands(2), &cfg);

        assert_eq!(body["model"], "jev-latest");
        assert_eq!(body["state"]["query"], "vitamin d bone density");
        assert_eq!(body["state"]["documents"]["d0"]["text"], "body 0");
        // One noul question per document, keyed identically to the document.
        assert_eq!(body["questions"]["d0"]["type"], "noul");
        assert_eq!(body["questions"]["d1"]["type"], "noul");
        assert!(body["questions"]["d0"]["criteria"]["true"].is_string());
        assert_eq!(body["questions"].as_object().unwrap().len(), 2);
    }

    #[test]
    fn parses_documented_response() {
        let body = r#"{"model":"jev-latest","answers":{
            "d0":{"type":"noul","noul":0.92},
            "d1":{"type":"noul","noul":0.03}},
            "usage":{"input_tokens":312,"output_tokens":48}}"#;
        let mut got = parse_jev_response(body).unwrap();
        got.sort_by_key(|s| s.ordinal);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].ordinal, 0);
        assert!((got[0].score - 0.92).abs() < 1e-6);
        assert!((got[1].score - 0.03).abs() < 1e-6);
    }

    #[test]
    fn unanswered_documents_are_skipped_not_zeroed() {
        let body = r#"{"answers":{"d0":{"type":"noul","noul":0.5},"d1":{"type":"noul"}}}"#;
        let got = parse_jev_response(body).unwrap();
        assert_eq!(got.len(), 1, "d1 has no noul and must not become 0.0");
        assert_eq!(got[0].ordinal, 0);
    }

    #[test]
    fn reorders_by_probability() {
        let c = cands(3);
        let scores = vec![
            Scored {
                ordinal: 0,
                score: 0.10,
            },
            Scored {
                ordinal: 1,
                score: 0.90,
            },
            Scored {
                ordinal: 2,
                score: 0.50,
            },
        ];
        let out = apply_scores(&c, &scores, None);
        assert_eq!(
            out.iter().map(|s| s.ordinal).collect::<Vec<_>>(),
            vec![1, 2, 0]
        );
    }

    #[test]
    fn min_score_prunes() {
        let c = cands(3);
        let scores = vec![
            Scored {
                ordinal: 0,
                score: 0.10,
            },
            Scored {
                ordinal: 1,
                score: 0.90,
            },
            Scored {
                ordinal: 2,
                score: 0.50,
            },
        ];
        let out = apply_scores(&c, &scores, Some(0.4));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].ordinal, 1);
        assert_eq!(out[1].ordinal, 2);
    }

    #[test]
    fn unscored_hits_sort_below_scored_and_keep_engine_order() {
        let c = cands(4);
        // Only d2 came back scored, and with a low probability at that.
        let scores = vec![Scored {
            ordinal: 2,
            score: 0.01,
        }];
        let out = apply_scores(&c, &scores, None);
        assert_eq!(out[0].ordinal, 2, "the only scored hit leads");
        assert_eq!(
            out[1..].iter().map(|s| s.ordinal).collect::<Vec<_>>(),
            vec![0, 1, 3],
            "unscored hits keep the engine's relative order"
        );
    }

    #[test]
    fn ties_keep_engine_order() {
        let c = cands(3);
        let scores = vec![
            Scored {
                ordinal: 0,
                score: 0.7,
            },
            Scored {
                ordinal: 1,
                score: 0.7,
            },
            Scored {
                ordinal: 2,
                score: 0.7,
            },
        ];
        let out = apply_scores(&c, &scores, None);
        assert_eq!(
            out.iter().map(|s| s.ordinal).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    /// A provider that answers for documents this batch never sent — another
    /// batch's keys, or keys nobody sent — must not score them here, and must
    /// not count them. `judged` is the operator's billing meter.
    #[test]
    fn verdicts_are_kept_per_batch_one_per_document() {
        let batch = cands(3); // ordinals 0, 1, 2
        let answers = vec![
            Scored {
                ordinal: 2,
                score: 0.2,
            },
            Scored {
                ordinal: 0,
                score: 0.9,
            },
            // Another batch's documents, echoed back.
            Scored {
                ordinal: 7,
                score: 0.99,
            },
            Scored {
                ordinal: 30,
                score: 0.99,
            },
            // A key nobody sent.
            Scored {
                ordinal: 999,
                score: 0.99,
            },
        ];
        let kept = keep_verdicts_for(&batch, answers);
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert_eq!(
            kept[0],
            Scored {
                ordinal: 0,
                score: 0.9
            }
        );
        assert_eq!(
            kept[1],
            Scored {
                ordinal: 2,
                score: 0.2
            }
        );

        // Only unknown keys: nothing was judged, and the caller must not be
        // told the order is the judge's.
        let none = keep_verdicts_for(
            &batch,
            vec![Scored {
                ordinal: 999,
                score: 0.99,
            }],
        );
        assert!(none.is_empty(), "{none:?}");
    }

    /// The probability the provider sent is the probability the caller gets,
    /// to the digit: `0.9` in, `0.9` out — not `0.8999999761581421`.
    #[test]
    fn probabilities_keep_double_precision() {
        let body =
            r#"{"answers":{"d0":{"type":"noul","noul":0.9},"d1":{"type":"noul","noul":0.6}}}"#;
        let scores = parse_jev_response(body).unwrap();
        let of = |o: usize| scores.iter().find(|s| s.ordinal == o).unwrap().score;
        assert_eq!(of(0), 0.9);
        assert_eq!(of(1), 0.6);
        assert_eq!(format!("{}", of(0)), "0.9");
        // The threshold a caller writes is compared without a narrowing step.
        let kept = apply_scores(&cands(2), &scores, Some(0.9));
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].ordinal, 0);
    }

    #[test]
    fn batch_is_clamped_to_provider_ceiling() {
        let cfg = RerankConfig::from_json(&json!({"batch": 500})).unwrap();
        assert_eq!(cfg.batch, JEV_MAX_DOCS_PER_CALL);
    }

    #[test]
    fn unknown_field_is_rejected_not_ignored() {
        let err = RerankConfig::from_json(&json!({"treshold": 0.5})).unwrap_err();
        assert!(matches!(err, RerankError::Config(_)));
        assert!(err.to_string().contains("treshold"));
    }

    #[test]
    fn min_score_outside_probability_range_is_rejected() {
        assert!(RerankConfig::from_json(&json!({"min_score": 7.0})).is_err());
        assert!(RerankConfig::from_json(&json!({"min_score": -0.1})).is_err());
        assert!(RerankConfig::from_json(&json!({"min_score": 0.0})).is_ok());
        assert!(RerankConfig::from_json(&json!({"min_score": 1.0})).is_ok());
    }

    #[test]
    fn unknown_provider_is_rejected() {
        let cfg = RerankConfig::from_json(&json!({"provider": "voyage"})).unwrap();
        assert!(matches!(
            Provider::from_config(&cfg),
            Err(RerankError::UnknownProvider(_))
        ));
    }

    #[test]
    fn clip_never_splits_a_multibyte_char() {
        // The byte-slice version of this panicked and aborted a whole
        // autoindex run on exactly these characters.
        assert_eq!(clip("اختبار", 3).chars().count(), 3);
        assert_eq!(clip("设计文档", 2), "设计");
        assert_eq!(clip("ascii", 99), "ascii");
    }

    #[test]
    fn caller_chosen_cost_knobs_have_server_side_ceilings() {
        for (body, needle) in [
            (json!({"window": MAX_WINDOW + 1}), "rerank.window"),
            (
                json!({"max_concurrency": MAX_CONCURRENCY + 1}),
                "max_concurrency",
            ),
            (json!({"max_doc_chars": MAX_DOC_CHARS + 1}), "max_doc_chars"),
            (json!({"timeout_ms": MAX_TIMEOUT_MS + 1}), "timeout_ms"),
        ] {
            let err = RerankConfig::from_json(&body).expect_err("over the ceiling");
            assert!(err.to_string().contains(needle), "{err}");
        }
        assert!(RerankConfig::from_json(&json!({"window": MAX_WINDOW})).is_ok());
    }

    /// The strings a caller writes are cost knobs too — `instructions` most of
    /// all, because the wire format repeats it once per judged document.
    #[test]
    fn caller_written_strings_have_server_side_ceilings() {
        let long = |n: usize| "y".repeat(n);
        for (body, needle) in [
            (
                json!({"instructions": long(MAX_INSTRUCTIONS_CHARS + 1)}),
                "rerank.instructions",
            ),
            (json!({"query": long(MAX_QUERY_CHARS + 1)}), "rerank.query"),
            (json!({"model": long(MAX_MODEL_CHARS + 1)}), "rerank.model"),
            (
                json!({"provider": long(MAX_MODEL_CHARS + 1)}),
                "rerank.provider",
            ),
            (
                json!({"fields": vec!["f"; MAX_FIELDS + 1]}),
                "rerank.fields",
            ),
            (
                json!({"fields": [long(MAX_FIELD_NAME_CHARS + 1)]}),
                "rerank.fields",
            ),
        ] {
            let err = match RerankConfig::from_json(&body) {
                Err(e) => e.to_string(),
                Ok(_) => panic!("over the ceiling must be refused: needle {needle}"),
            };
            assert!(err.contains(needle), "{err}");
            // The refusal names the limit and never echoes the oversized value.
            assert!(
                err.len() < 400,
                "refusal must not echo the value: {} bytes",
                err.len()
            );
        }
        // Exactly at the ceiling is accepted, and the ceiling counts
        // characters, not bytes: a multi-byte string of the same length fits.
        assert!(RerankConfig::from_json(&json!({
            "instructions": long(MAX_INSTRUCTIONS_CHARS),
            "query": long(MAX_QUERY_CHARS),
            "model": long(MAX_MODEL_CHARS),
            "fields": vec![long(MAX_FIELD_NAME_CHARS); MAX_FIELDS],
        }))
        .is_ok());
        assert!(RerankConfig::from_json(&json!({
            "instructions": "é".repeat(MAX_INSTRUCTIONS_CHARS),
        }))
        .is_ok());
    }

    /// An unknown key or provider is named in the refusal, but a caller must
    /// not be able to make the node echo megabytes back by naming a huge one.
    #[test]
    fn refusals_do_not_echo_oversized_names() {
        let huge = "k".repeat(1_000_000);
        let mut block = Map::new();
        block.insert(huge, json!(1));
        let err = RerankConfig::from_json(&Value::Object(block))
            .expect_err("unknown key")
            .to_string();
        assert!(
            err.contains("unknown `rerank` field"),
            "{}",
            &err[..200.min(err.len())]
        );
        assert!(err.len() < 1_000, "echoed {} bytes", err.len());
    }

    /// The wire is bounded by construction, whatever built the `RerankConfig`:
    /// the fields are public, so the parser's refusal is not the only guard.
    #[test]
    fn one_provider_call_is_bounded_whatever_the_config_holds() {
        let cfg = RerankConfig {
            instructions: Some("i".repeat(5_000_000)),
            model: "m".repeat(1_000_000),
            max_doc_chars: MAX_DOC_CHARS,
            ..RerankConfig::default()
        };
        let batch: Vec<Candidate> = (0..JEV_MAX_DOCS_PER_CALL)
            .map(|i| Candidate {
                ordinal: i,
                title: Some("t".repeat(100_000)),
                text: "x".repeat(100_000),
            })
            .collect();
        let question = "q".repeat(3_000_000);
        let body = build_jev_request(&question, &batch, &cfg).to_string();
        // 30 documents x (title + text at the ceiling + instructions at the
        // ceiling) + the question + the model + JSON framing.
        let bound = JEV_MAX_DOCS_PER_CALL * (2 * MAX_DOC_CHARS + MAX_INSTRUCTIONS_CHARS + 512)
            + MAX_QUERY_CHARS
            + MAX_MODEL_CHARS
            + 1_024;
        assert!(
            body.len() <= bound,
            "one call put {} bytes on the wire; the bound is {bound}",
            body.len()
        );
    }

    /// `usage` is advisory metering. It must never veto a ranking whose
    /// verdicts all parsed — a provider that reports `-5`, a float, a string
    /// or `null` there used to fail the whole call with a 502 and no hits.
    #[test]
    fn odd_usage_never_discards_valid_verdicts() {
        for usage in [
            r#"{"input_tokens":9223372036854775808,"output_tokens":-5}"#,
            r#"{"input_tokens":"many","output_tokens":null}"#,
            r#"{"input_tokens":12.0,"output_tokens":3.9}"#,
            r#""n/a""#,
            r#"null"#,
            r#"[1,2]"#,
        ] {
            let body = format!(
                r#"{{"answers":{{"d0":{{"type":"noul","noul":0.5}},"d1":{{"type":"noul","noul":0.25}}}},"usage":{usage}}}"#
            );
            let (mut scores, u) = match parse_jev_response_with_usage(&body) {
                Ok(v) => v,
                Err(e) => panic!("usage {usage} vetoed two valid verdicts: {e}"),
            };
            scores.sort_by_key(|s| s.ordinal);
            assert_eq!(scores.len(), 2, "usage {usage}");
            assert_eq!(scores[0].score, 0.5);
            // A number that is a whole, non-negative count is kept; anything
            // else meters as zero rather than as a guess.
            if usage.contains("12.0") {
                assert_eq!((u.input_tokens, u.output_tokens), (12, 3), "usage {usage}");
            }
            if usage.contains("-5") {
                // 2^63 input tokens is a number, but not one a call of 30
                // documents can produce: it must not reach the operator's
                // counter, where one such response would poison the series.
                assert_eq!(u, Usage::default(), "usage {usage}");
            }
        }
        // The largest count one call could plausibly report is kept.
        let (_, u) = parse_jev_response_with_usage(&format!(
            r#"{{"answers":{{}},"usage":{{"input_tokens":{MAX_PLAUSIBLE_TOKENS_PER_CALL},"output_tokens":{}}}}}"#,
            MAX_PLAUSIBLE_TOKENS_PER_CALL + 1
        ))
        .unwrap();
        assert_eq!(
            (u.input_tokens, u.output_tokens),
            (MAX_PLAUSIBLE_TOKENS_PER_CALL, 0)
        );
        // Malformed ANSWERS are still a contract break.
        assert!(parse_jev_response_with_usage(r#"{"answers":[1,2]}"#).is_err());
        assert!(parse_jev_response_with_usage(
            r#"{"answers":{"d0":{"type":"noul","noul":"high"}}}"#
        )
        .is_err());
    }

    // ── server-side settings: the env-free seam ─────────────────────────────

    #[test]
    fn config_beats_env_and_env_beats_default() {
        let s = ProviderSettings::resolve(
            true,
            "from-config",
            "http://config.example/v1",
            Some("from-env".into()),
            Some("http://env.example/v1".into()),
        );
        assert_eq!(s.key_source(), Some(SettingSource::Config));
        assert_eq!(s.endpoint_source(), SettingSource::Config);
        assert_eq!(s.endpoint_for_display(), "http://config.example/v1");

        let s = ProviderSettings::resolve(true, "", "  ", Some("from-env".into()), None);
        assert_eq!(s.key_source(), Some(SettingSource::Env));
        assert_eq!(s.endpoint_source(), SettingSource::Default);
        assert_eq!(s.endpoint_for_display(), JevProvider::DEFAULT_ENDPOINT);

        // Whitespace is not a key.
        let s = ProviderSettings::resolve(true, "", "", Some("   ".into()), None);
        assert!(!s.has_key());
        assert_eq!(s.key_source(), None);
    }

    #[test]
    fn debug_output_never_contains_the_key() {
        let s = ProviderSettings::with_key_and_endpoint(
            "sk-very-secret",
            "https://user:hunter2@api.example/v1/systemone?token=abc#frag",
        );
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("sk-very-secret"), "{dbg}");
        assert!(!dbg.contains("hunter2"), "{dbg}");
        assert!(!dbg.contains("token=abc"), "{dbg}");
        assert!(dbg.contains("<redacted>"), "{dbg}");
        assert_eq!(
            s.endpoint_for_display(),
            "https://api.example/v1/systemone",
            "userinfo, query and fragment are all places a secret hides"
        );
    }

    #[test]
    fn missing_key_is_a_surfaced_error_not_a_silent_degrade() {
        let settings = ProviderSettings::resolve(true, "", "", None, None);
        let err = match Provider::from_settings(&RerankConfig::default(), &settings) {
            Err(e) => e,
            Ok(_) => panic!("no key must not build a working provider"),
        };
        assert!(matches!(err, RerankError::MissingKey(_)));
        assert_eq!(err.policy(), Policy::Surface);
        // The message must not tell callers to put a key in the request: there
        // is no such field, and a key in a search body ends up in slow-query
        // logs and audit trails.
        assert!(!err.to_string().contains("in the request"), "{err}");
    }

    #[test]
    fn an_operator_can_forbid_reranking_outright() {
        let settings = ProviderSettings::resolve(false, "a-key", "", None, None);
        let err = match Provider::from_settings(&RerankConfig::default(), &settings) {
            Err(e) => e,
            Ok(_) => panic!("enabled = false must refuse even with a key"),
        };
        assert!(matches!(err, RerankError::DisabledByOperator));
        assert_eq!(err.policy(), Policy::Surface);
    }

    #[test]
    fn usage_is_parsed_and_absent_usage_is_zero() {
        let (_, u) = parse_jev_response_with_usage(
            r#"{"answers":{"d0":{"type":"noul","noul":0.5}},
                "usage":{"input_tokens":312,"output_tokens":48}}"#,
        )
        .unwrap();
        assert_eq!((u.input_tokens, u.output_tokens), (312, 48));
        let (_, u) =
            parse_jev_response_with_usage(r#"{"answers":{"d0":{"type":"noul","noul":0.5}}}"#)
                .unwrap();
        assert_eq!(u, Usage::default());
    }

    #[test]
    fn out_of_range_probabilities_are_clamped() {
        let mut got = parse_jev_response(
            r#"{"answers":{"d0":{"type":"noul","noul":1.7},"d1":{"type":"noul","noul":-0.2}}}"#,
        )
        .unwrap();
        got.sort_by_key(|s| s.ordinal);
        assert_eq!(got[0].score, 1.0);
        assert_eq!(got[1].score, 0.0);
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .enable_io()
            .build()
            .unwrap()
    }

    /// Unroutable on purpose — port 1 refuses immediately.
    fn dead_provider() -> JevProvider {
        JevProvider::new(
            "k".into(),
            "http://127.0.0.1:1/never".into(),
            Duration::from_millis(50),
        )
    }

    #[test]
    fn empty_candidate_list_makes_no_request() {
        let out = rt()
            .block_on(dead_provider().rerank(
                "q",
                &[],
                &RerankConfig::default(),
                &Deadline::new(Duration::from_secs(1)),
            ))
            .unwrap();
        assert!(matches!(
            out,
            RerankOutcome::Reordered { ref scores, .. } if scores.is_empty()
        ));
    }

    #[test]
    fn disabled_provider_degrades_it_does_not_error() {
        let out = rt()
            .block_on(Provider::Disabled.rerank(
                "q",
                &cands(3),
                &RerankConfig::default(),
                &Deadline::new(Duration::from_secs(1)),
            ))
            .unwrap();
        assert!(matches!(out, RerankOutcome::Degraded { .. }));
    }

    #[test]
    fn expired_deadline_degrades_without_calling_the_provider() {
        let cfg = RerankConfig::default();
        // Zero budget is already exceeded; a call would hang on the dead port.
        let out = rt()
            .block_on(Provider::Jev(dead_provider()).rerank(
                "q",
                &cands(3),
                &cfg,
                &Deadline::new(Duration::ZERO),
            ))
            .unwrap();
        match out {
            RerankOutcome::Degraded { reason } => assert!(reason.contains("deadline")),
            other => panic!("expected degrade, got {other:?}"),
        }
    }

    #[test]
    fn transport_failure_surfaces_rather_than_pretending_to_rerank() {
        let cfg = RerankConfig {
            // No retry budget to burn; we want the error, not a timeout.
            timeout: Duration::from_millis(50),
            ..RerankConfig::default()
        };
        let err = rt()
            .block_on(Provider::Jev(dead_provider()).rerank(
                "q",
                &cands(2),
                &cfg,
                &Deadline::new(Duration::from_secs(5)),
            ))
            .expect_err("a dead provider must not look like a successful rerank");
        assert_eq!(err.policy(), Policy::Surface);
    }

    #[test]
    fn error_policy_split_matches_the_documented_rule() {
        // Degrade: ran out of time, caller still has results.
        assert_eq!(
            RerankError::Deadline { elapsed_ms: 10 }.policy(),
            Policy::Degrade
        );
        // Surface: configuration and contract faults.
        assert_eq!(
            RerankError::MissingKey("TYPESAFE_API_KEY").policy(),
            Policy::Surface
        );
        assert_eq!(
            RerankError::Status {
                status: 401,
                body: String::new()
            }
            .policy(),
            Policy::Surface
        );
        assert_eq!(
            RerankError::Malformed("bad json".into()).policy(),
            Policy::Surface
        );
    }

    #[test]
    fn only_documented_backoff_codes_are_retried() {
        let retry = |code| {
            RerankError::Status {
                status: code,
                body: String::new(),
            }
            .retryable()
        };
        // TypeSafe documents 429 and 529 as back-off-and-retry.
        assert!(retry(429));
        assert!(retry(529));
        assert!(retry(500));
        // Contract errors: retrying only burns the deadline.
        assert!(!retry(401));
        assert!(!retry(422));
        assert!(!RerankError::Malformed("x".into()).retryable());
    }

    #[test]
    fn deadline_reports_remaining_budget() {
        let d = Deadline::new(Duration::from_millis(500));
        assert!(!d.exceeded());
        assert!(d.remaining() <= Duration::from_millis(500));
        let spent = Deadline::new(Duration::ZERO);
        assert!(spent.exceeded());
        assert_eq!(spent.remaining(), Duration::ZERO);
    }

    #[test]
    fn batching_splits_at_the_provider_ceiling() {
        let cfg = RerankConfig::default();
        let c = cands(70);
        let batches: Vec<_> = c.chunks(cfg.batch).collect();
        assert_eq!(batches.len(), 3, "70 docs at 30/call is 3 calls");
        assert_eq!(batches[0].len(), 30);
        assert_eq!(batches[2].len(), 10);
    }

    #[test]
    fn ordinals_survive_batching() {
        // The response keys carry the caller's ordinal, not a position within
        // the batch, so a second-batch document maps back correctly.
        let cfg = RerankConfig::default();
        let c = cands(40);
        let body = build_jev_request("q", &c[30..], &cfg);
        assert!(body["questions"]["d30"].is_object());
        assert!(body["questions"]["d39"].is_object());
        assert!(body["questions"]["d0"].is_null());
    }
}
