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
//!
//! The Local arm ([`local`]) makes no network call at all: a cross-encoder runs
//! in this process on the operator's own CPU. It exists so calibrated relevance
//! does not require an account, a key, a network or a per-token bill, and so
//! it is available on corpora whose text may not leave the host. It follows the
//! same fail policy and returns the same [`RerankOutcome`], so the API layer
//! cannot tell the two apart except by what `_rerank.provider` says.

pub mod local;

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
    /// This binary cannot run a local model (built without `neural`).
    #[error("{0}")]
    LocalUnavailable(String),
    /// The operator switched local models off (`[judge] enabled = false`).
    /// Separate from [`Self::DisabledByOperator`], which is about egress: the
    /// local provider sends nothing anywhere, so the egress switch does not
    /// govern it and this one does.
    #[error(
        "the local judge is disabled on this server (`[judge] enabled = false`), so provider \
         `local` is refused"
    )]
    LocalDisabledByOperator,
    /// The local model cannot be loaded on this server as configured: not on
    /// disk with downloads off, a checksum mismatch, or not enough memory.
    #[error("{0}")]
    LocalModel(String),
    /// The local model loaded and then failed to score.
    #[error("{0}")]
    LocalInference(String),
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
            | Self::LocalUnavailable(_)
            | Self::LocalDisabledByOperator
            | Self::LocalModel(_)
            | Self::LocalInference(_)
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
        /// Provider-specific facts about this call, reported verbatim as
        /// `_rerank.<provider>`. The local provider uses it for what a caller
        /// cannot otherwise see: which repository scored, which calibration
        /// produced the probabilities, how many documents were cut at the
        /// model's token limit. `None` for providers with nothing to add.
        detail: Option<Value>,
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
                return Err(RerankError::Config(format!(
                    "unknown `rerank` field `{k}`; supported: {}",
                    KNOWN.join(", ")
                )));
            }
        }

        let mut cfg = Self::default();

        if let Some(p) = obj.get("provider") {
            cfg.provider = p
                .as_str()
                .ok_or_else(|| RerankError::Config("`rerank.provider` must be a string".into()))?
                .to_string();
        }
        if let Some(m) = obj.get("model") {
            cfg.model = m
                .as_str()
                .ok_or_else(|| RerankError::Config("`rerank.model` must be a string".into()))?
                .to_string();
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
            cfg.instructions = Some(
                i.as_str()
                    .ok_or_else(|| {
                        RerankError::Config("`rerank.instructions` must be a string".into())
                    })?
                    .to_string(),
            );
        }

        if let Some(q) = obj.get("query") {
            let q = q
                .as_str()
                .ok_or_else(|| RerankError::Config("`rerank.query` must be a string".into()))?;
            if q.trim().is_empty() {
                return Err(RerankError::Config(
                    "`rerank.query` must not be empty".into(),
                ));
            }
            cfg.query = Some(q.to_string());
        }
        if let Some(f) = obj.get("fields") {
            let arr = f.as_array().ok_or_else(|| {
                RerankError::Config("`rerank.fields` must be an array of field names".into())
            })?;
            let mut fields = Vec::with_capacity(arr.len());
            for v in arr {
                fields.push(
                    v.as_str()
                        .ok_or_else(|| {
                            RerankError::Config("`rerank.fields` entries must be strings".into())
                        })?
                        .to_string(),
                );
            }
            if fields.is_empty() {
                return Err(RerankError::Config(
                    "`rerank.fields` must not be empty".into(),
                ));
            }
            cfg.fields = Some(fields);
        }

        if local::is_local(&cfg.provider) {
            local::adjust_request(obj, &mut cfg)?;
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
fn clip(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
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

/// Build one System One request body for a batch of candidates.
///
/// Split out from the HTTP call so the wire format is testable without a network
/// or an API key.
pub fn build_jev_request(query: &str, batch: &[Candidate], cfg: &RerankConfig) -> Value {
    let mut documents = Map::new();
    let mut questions = Map::new();

    for cand in batch {
        // Keyed by the caller's ordinal, so a reordered or partial response
        // still maps back to the right hit.
        let key = format!("d{}", cand.ordinal);
        let mut doc = Map::new();
        if let Some(t) = &cand.title {
            doc.insert("title".into(), json!(clip(t, cfg.max_doc_chars)));
        }
        doc.insert("text".into(), json!(clip(&cand.text, cfg.max_doc_chars)));
        documents.insert(key.clone(), Value::Object(doc));

        questions.insert(
            key,
            json!({
                "type": "noul",
                "instructions": cfg.question(),
                "criteria": {
                    "true":  "The document is relevant to the query.",
                    "false": "The document is not relevant to the query."
                }
            }),
        );
    }

    json!({
        "state": { "query": query, "documents": Value::Object(documents) },
        "model": cfg.model,
        "questions": Value::Object(questions),
    })
}

#[derive(Deserialize)]
struct JevAnswer {
    #[serde(default)]
    noul: Option<f64>,
}

#[derive(Deserialize, Default)]
struct JevUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

#[derive(Deserialize)]
struct JevResponse {
    #[serde(default)]
    answers: std::collections::HashMap<String, JevAnswer>,
    #[serde(default)]
    usage: JevUsage,
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
        input_tokens: parsed.usage.input_tokens,
        output_tokens: parsed.usage.output_tokens,
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
    /// The in-process judge behind provider `local`. Not governed by
    /// `enabled` above — that switch forbids egress and this sends nothing.
    local: local::LocalJudge,
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
            local: local::LocalJudge::default(),
        }
    }

    /// Install the server's local judge (`[judge]`). Without this call the
    /// settings carry a default one: enabled, downloads allowed, tier `small`.
    pub fn with_local(mut self, local: local::LocalJudge) -> Self {
        self.local = local;
        self
    }

    /// The in-process judge behind provider `local`.
    pub fn local(&self) -> &local::LocalJudge {
        &self.local
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
    /// A cross-encoder in this process. No key, no network, no egress.
    Local(local::LocalJudge),
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
            // `settings.enabled` is deliberately not consulted: it forbids
            // sending document text to a third party, and this arm sends none.
            "local" => {
                settings.local.check_request(cfg)?;
                Ok(Self::Local(settings.local.clone()))
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
                detail: None,
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
            Self::Local(judge) => match judge.rerank(query, candidates, cfg, deadline).await {
                Ok(outcome) => Ok(outcome),
                Err(e) if e.policy() == Policy::Degrade => Ok(RerankOutcome::Degraded {
                    reason: e.to_string(),
                }),
                Err(e) => Err(e),
            },
        }
    }

    /// The model name to report for this call: what the request named, or —
    /// for the local provider, where a request may name none — the tier the
    /// server resolved it to.
    pub fn model_label(&self, cfg: &RerankConfig) -> String {
        match self {
            Self::Local(judge) => judge.rerank_tier(&cfg.model).to_string(),
            _ => cfg.model.clone(),
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
        let text = resp.text().await.map_err(|e| {
            if e.is_timeout() {
                deadline.err()
            } else {
                RerankError::Transport(e.to_string())
            }
        })?;

        if !status.is_success() {
            // 429 and 529 are the documented back-off codes. Surfaced rather
            // than retried here: the caller is inside a search request with a
            // deadline, and fail-open beats spending it on retries.
            return Err(RerankError::Status {
                status: status.as_u16(),
                body: redact_key(clip(&text, 400), &self.api_key),
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
                detail: None,
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
            detail: None,
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
