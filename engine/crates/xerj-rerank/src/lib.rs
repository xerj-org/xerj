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
/// Jev's context budget is ~32k tokens; 30 passages plus the query is the
/// working figure the ecosystem settled on. Longer candidate lists are split
/// into several calls, and the scores stay comparable across them because each
/// answer is an absolute probability rather than a rank within its batch.
pub const JEV_MAX_DOCS_PER_CALL: usize = 30;

/// Default concurrent in-flight provider calls.
///
/// Conservative on purpose: Jev starts returning 429 around 24 concurrent
/// requests, and a search path that provokes rate limiting is worse than a
/// slightly slower one.
pub const DEFAULT_MAX_CONCURRENCY: usize = 8;

/// How much of each document body to send.
///
/// Reranking reads titles and leading text; shipping whole documents burns the
/// context budget that limits how many candidates fit per call.
pub const DEFAULT_MAX_DOC_CHARS: usize = 1200;

#[derive(Debug, thiserror::Error)]
pub enum RerankError {
    #[error("no API key: set {0} or pass it in the request")]
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
    },
    /// Reranking did not run. The engine's order stands.
    Degraded { reason: String },
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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scored {
    pub ordinal: usize,
    pub score: f32,
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
    pub min_score: Option<f32>,
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
    /// `_source` fields sent to the provider. `None` sends every top-level
    /// string field, which is right for small documents and wasteful for wide
    /// ones.
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
            cfg.min_score = Some(t as f32);
        }
        if let Some(c) = obj.get("max_concurrency") {
            let c = c.as_u64().ok_or_else(|| {
                RerankError::Config("`rerank.max_concurrency` must be a number".into())
            })?;
            if c == 0 {
                return Err(RerankError::Config(
                    "`rerank.max_concurrency` must be > 0".into(),
                ));
            }
            cfg.max_concurrency = c as usize;
        }
        if let Some(c) = obj.get("max_doc_chars") {
            let c = c.as_u64().ok_or_else(|| {
                RerankError::Config("`rerank.max_doc_chars` must be a number".into())
            })?;
            if c == 0 {
                return Err(RerankError::Config(
                    "`rerank.max_doc_chars` must be > 0".into(),
                ));
            }
            cfg.max_doc_chars = c as usize;
        }
        if let Some(t) = obj.get("timeout_ms") {
            let t = t.as_u64().ok_or_else(|| {
                RerankError::Config("`rerank.timeout_ms` must be a number".into())
            })?;
            if t == 0 {
                return Err(RerankError::Config(
                    "`rerank.timeout_ms` must be > 0".into(),
                ));
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

#[derive(Deserialize)]
struct JevResponse {
    #[serde(default)]
    answers: std::collections::HashMap<String, JevAnswer>,
}

/// Parse a System One response into scores keyed by the caller's ordinals.
///
/// A missing or non-`noul` answer is skipped rather than defaulted to zero:
/// inventing a "not relevant" verdict the model never gave would silently
/// demote a document.
pub fn parse_jev_response(body: &str) -> Result<Vec<Scored>, RerankError> {
    let parsed: JevResponse =
        serde_json::from_str(body).map_err(|e| RerankError::Malformed(e.to_string()))?;

    let mut out = Vec::with_capacity(parsed.answers.len());
    for (key, ans) in parsed.answers {
        let Some(ordinal) = key.strip_prefix('d').and_then(|n| n.parse::<usize>().ok()) else {
            continue;
        };
        let Some(noul) = ans.noul else { continue };
        out.push(Scored {
            ordinal,
            score: noul as f32,
        });
    }
    Ok(out)
}

/// Reorder `candidates` by provider score.
///
/// Candidates the provider did not score keep their original relative order and
/// sort *below* every scored one — an unscored document is not evidence of
/// irrelevance, but it cannot be ranked against ones that were scored.
pub fn apply_scores(
    candidates: &[Candidate],
    scores: &[Scored],
    min_score: Option<f32>,
) -> Vec<Scored> {
    let mut by_ordinal: std::collections::HashMap<usize, f32> =
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
                score: -1.0 - (rank as f32),
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

/// A configured reranking backend.
pub enum Provider {
    Jev(JevProvider),
    /// Configured but inert. An empty API key lands here rather than erroring,
    /// so a deployment that has not set one yet still serves searches.
    Disabled,
}

impl Provider {
    /// Build a provider from config plus the ambient environment.
    pub fn from_config(cfg: &RerankConfig) -> Result<Self, RerankError> {
        match cfg.provider.as_str() {
            "none" | "disabled" => Ok(Self::Disabled),
            "jev" | "typesafe" => JevProvider::from_env(cfg).map(Self::Jev),
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
            });
        }
        if deadline.exceeded() {
            return Ok(RerankOutcome::Degraded {
                reason: "deadline exceeded before the provider was called".into(),
            });
        }
        match self {
            Self::Disabled => Ok(RerankOutcome::Degraded {
                reason: "rerank provider disabled (no API key configured)".into(),
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
    pub const DEFAULT_ENDPOINT: &'static str = "https://api.typesafe.ai/v1/systemone";

    pub fn from_env(cfg: &RerankConfig) -> Result<Self, RerankError> {
        let api_key =
            std::env::var(Self::ENV_KEY).map_err(|_| RerankError::MissingKey(Self::ENV_KEY))?;
        if api_key.trim().is_empty() {
            return Err(RerankError::MissingKey(Self::ENV_KEY));
        }
        let endpoint = std::env::var("TYPESAFE_ENDPOINT")
            .unwrap_or_else(|_| Self::DEFAULT_ENDPOINT.to_string());
        Ok(Self::new(api_key, endpoint, cfg.timeout))
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
    ) -> Result<Vec<Scored>, RerankError> {
        const MAX_ATTEMPTS: u32 = 3;
        let mut attempt = 0;
        loop {
            if deadline.exceeded() {
                return Err(deadline.err());
            }
            match self.call_batch(query, batch, cfg).await {
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
    ) -> Result<Vec<Scored>, RerankError> {
        let body = build_jev_request(query, batch, cfg);
        let resp = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| RerankError::Transport(e.to_string()))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| RerankError::Transport(e.to_string()))?;

        if !status.is_success() {
            // 429 and 529 are the documented back-off codes. Surfaced rather
            // than retried here: the caller is inside a search request with a
            // deadline, and fail-open beats spending it on retries.
            return Err(RerankError::Status {
                status: status.as_u16(),
                body: clip(&text, 400).to_string(),
            });
        }
        parse_jev_response(&text)
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
            });
        }
        let batch_size = cfg.batch.clamp(1, JEV_MAX_DOCS_PER_CALL);
        let batches: Vec<Vec<Candidate>> =
            candidates.chunks(batch_size).map(|c| c.to_vec()).collect();

        let mut scores = Vec::with_capacity(candidates.len());
        let mut partial_failures = 0usize;
        let mut first_hard_error: Option<RerankError> = None;

        // Bounded waves rather than unbounded fan-out: the provider rate-limits,
        // and a search that trips a 429 is slower than one that paced itself.
        for wave in batches.chunks(cfg.max_concurrency.max(1)) {
            if deadline.exceeded() {
                break;
            }
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
                    Ok(Ok(mut part)) => scores.append(&mut part),
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
