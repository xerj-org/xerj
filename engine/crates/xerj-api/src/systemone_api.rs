//! The local judge: System One wire-compatible decisions from a ladder.
//!
//! Two surfaces, one ladder:
//!
//! - `POST /v1/systemone` (native router) speaks TypeSafe's documented System
//!   One request/response shape, so a client written for their API —
//!   `jev-reranker`, the official SDKs — can point `TYPESAFE_ENDPOINT` at a
//!   XERJ node and keep working. Answers come from a weighted
//!   nearest-neighbour vote over a labelled-history index
//!   (`[decisions] index`), the retrieval analogue of a judge model that
//!   `benchmarks/decisions-as-retrieval` measures.
//! - `POST /_decide` (ES-compat router) is the same vote without the wire
//!   costume: it names its index per request and returns the neighbours,
//!   their labels and scores, and an abstain verdict.
//!
//! Nothing leaves the node. This module adds no outbound client; the vote is
//! an ordinary search against an ordinary index. That is the point of the
//! feature — the same interface, with the evidence staying home.
//!
//! # The decide ladder (#1057)
//!
//! Both surfaces answer through the same ordered ladder, per question:
//!
//! 1. **History vote** — when `[decisions] index` is configured and returns
//!    labelled support, the vote wins. It is the measured tier and it shows
//!    its evidence.
//! 2. **Local zero-shot head** (feature `decide-local`,
//!    `XERJ_DECIDE_MODE=local`, the env half of `--decide-mode local`) — a
//!    ModernBERT-class candle classifier ([`xerj_ai::decide`]) loaded from
//!    `XERJ_DECIDE_MODEL_DIR`. It answers every no-support outcome: no
//!    `[decisions] index` at all, a configured index that is missing, and a
//!    question whose payload retrieves no labelled neighbour. Its model echo
//!    is [`LOCAL_MODEL_ID`] — the same never-a-Jev-name discipline.
//! 3. **Hosted key** — reserved, not built. When tiers 1 and 2 cannot
//!    answer, the documented errors stand rather than a fabricated
//!    probability.
//!
//! The default build (no `decide-local`, or the mode unset) behaves exactly
//! as before: 503 with no `[decisions]` index, 422 `no_support` / on a
//! history that cannot answer.
//!
//! # The decision cache flywheel (#1061)
//!
//! Every tier-2 (and, when it is built, tier-3) answer is written back to
//! the `[decisions]` index as an ordinary document — the configured
//! `text_field`/`label_field` plus `p`, `source`, `ts` — so the answers a
//! node computes once become the history that answers the next request.
//! History-tier answers are not written: they ARE the index already, and
//! double-writing them would double-count their vote. The write-back is
//! spawned, never awaited by the request, and cannot fail it: a missing or
//! unwritable index is a log line, not an error the caller sees. Every
//! answer names its tier in a `source` field (`/_decide` top level,
//! `/v1/systemone` per-question in `decisions.evidence.*.source` — the same
//! string as `tier`, under the flywheel's name).
//!
//! Human corrections ride the same index: a history document carrying
//! `human: true` (written through the ordinary indexing API) is weighted
//! [`DecisionsConfig::human_weight`]× (default 2.0, the issue's ≥ 2x floor)
//! in the vote — see [`neighbour_weight`]. The weight is read in the vote
//! aggregation, so a correction outranks the cached answers it corrects.
//!
//! # Calibration: `p_cal` beside every `p_raw` (#1063)
//!
//! A ladder probability ranks before it odds: on the FiQA rerank baseline a
//! hosted noul of 0.93 meant relevance 34 % of the time (ECE 0.3109). With
//! `[decisions] calibration = isotonic | temperature`, the node fits a
//! correction on a held-out fifth of its recorded outcomes ([`xerj_common::
//! calibration`]) and answers carry the calibrated probability BESIDE the raw
//! one — `p_raw` never replaced, `p_cal` absent when no calibration is
//! configured and `null` (with a published reason) when configured but
//! unfitted, never a copy of `p_raw` pretending to be calibrated. The fit
//! applies to every tier's probability through one seam, so the hosted tier,
//! when it is built, calibrates its passthrough score the same way. The
//! calibrated quantity is the positive label's probability (a noul, `/_decide`'s
//! `p_raw`); per-option `choice` shares are a different quantity and are not
//! calibrated by this fit — every response that ships a probability carries
//! the fit's ECE beside it, and `GET /_decide/_calibration` publishes the
//! whole reliability curve.
//!
//! # The wire contract, and where we deliberately break it
//!
//! Request: `{ state, model, questions: { id: { type, instructions, criteria } } }`.
//! `state` may be a string or an object; question `instructions` may be a
//! string, object or array. **The vote text is payload data, never
//! instruction prose (#1000, #1001).** A question votes on exactly what it
//! points at: every `` `path` `` reference its instructions name — resolved
//! against the instructions' own string fields first (the data-field
//! pattern: `{"question": "Judge `document` …", "document": text}`) and then
//! against the state by dotted path (`` `documents.doc_0` ``, the shape
//! `jev-reranker` sends) — or, when it references nothing, on the state
//! alone: a string state whole, an object state by all its string leaves.
//! A reference that resolves to structure rather than text (the rubric
//! object `jev-reranker` ≥ 0.1.2 ships with its relevance preset) is
//! acknowledged and skipped: judge rules are not retrieval vocabulary.
//! References that resolve to nothing, and a payload with no text at all,
//! are 422s naming the question — never a confident vote on text the
//! question never saw. (These are request-shape refusals: the local head
//! does not rescue them, because there is no payload to classify.)
//!
//! Response: `{ model, answers, usage }` with `answers` keyed exactly by the
//! question ids sent and each answer carrying only its documented fields —
//! clients parse these strictly. Extras ride at the top level (`decisions`),
//! which every verified client ignores.
//!
//! Two deliberate breaks, both documented here and in docs/RERANK.md:
//!
//! 1. `model` is echoed as `xerj-history-vote-1`, never as a Jev model name.
//!    Echoing `jev-1.13.0` would claim these probabilities are the hosted
//!    model's. The requested name is reported as `decisions.requested_model`.
//!    When the local tier answered (all or part of the request), the echo is
//!    still `xerj-history-vote-1` for wire compatibility, `decisions.model`
//!    names the tier that answered each question per-question in
//!    `decisions.evidence.*.tier`, and `decisions.local_model` names the
//!    local head.
//! 2. A question with no support — an empty history, or no labelled
//!    neighbour — is a 422 naming the question ids. Zero support must not
//!    become a fabricated 0.5. (`/_decide` says the same thing as `abstain`.)
//!    The local tier is the documented exception: enabled, it answers those
//!    same questions with the head's probabilities instead.
//!
//! `score` questions (2–10 ordinal levels, weighted-average answer) have no
//! vote analogue and no benchmark: 422, in the docs.

use std::collections::BTreeMap;
use std::collections::HashMap;
#[cfg(feature = "decide-local")]
use std::path::Path;
#[cfg(feature = "decide-local")]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};
use xerj_common::calibration::CalibrationMethod;
use xerj_common::calibration::CalibrationPair;
use xerj_common::calibration::CalibrationReport;
use xerj_common::calibration::FittedCalibration;
use xerj_common::config::DecisionsConfig;
use xerj_query::parse_request;

use crate::state::AppState;

/// The model id this endpoint truthfully is. Never a Jev name.
pub const MODEL_ID: &str = "xerj-history-vote-1";

/// The model id the local zero-shot tier truthfully is — the same discipline
/// as [`MODEL_ID`]: never a Jev name, because these probabilities are the
/// local head's, not a hosted judge's.
pub const LOCAL_MODEL_ID: &str = "xerj-decide-local-1";

/// One question's vote text is bounded so a caller cannot make the node
/// search for a novel of its choosing; matches the rerank stage's philosophy
/// of caller-chosen cost with a server-side ceiling.
const MAX_VOTE_TEXT_CHARS: usize = 100_000;
/// The System One API documents at most 255 options per `choice`.
const MAX_CHOICE_OPTIONS: usize = 255;
/// Matches `rerank.max_window`: one request cannot ask for more questions
/// than the rerank stage would judge.
const MAX_QUESTIONS: usize = 300;

// ─────────────────────────────────────────────────────────────────────────────
// The decide ladder (#1057)
// ─────────────────────────────────────────────────────────────────────────────

/// The tiers of the decide ladder, in resolution order. History wins where
/// it has support; the local head answers what history cannot; the hosted
/// tier is reserved and unbuilt, so the ladder bottoms out in the documented
/// errors rather than a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecideTier {
    /// Tier 1: the weighted nearest-neighbour vote over `[decisions] index`.
    History,
    /// Tier 2: the local zero-shot decision head (feature `decide-local`,
    /// `XERJ_DECIDE_MODE=local`).
    Local,
    /// Tier 3: a hosted decision provider key. Not yet built — see the
    /// module header. When [`resolve_tier`] returns `None` the caller
    /// answers with today's error contract.
    Hosted,
}

/// What the history index said for one question. The ladder treats every
/// no-support outcome the same way: the local head may answer it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum HistorySupport {
    /// Labelled neighbours came back; the `f64` is the winning label's share
    /// of the total vote weight (`0.0..=1.0`).
    Supported(f64),
    /// No `[decisions] index` is configured at all.
    NotConfigured,
    /// The configured index does not exist or could not be searched; the
    /// string is the cause, surfaced to the operator when no tier can
    /// answer.
    Unusable(String),
    /// The index answered with no labelled neighbour.
    NoLabelledNeighbour,
}

/// Resolve the ladder for one question. Pure, and unit-tested: the order is
/// the contract.
///
/// When `local_enabled` is true and a history vote's winning share is strictly
/// below `escalate_below` (`#1094`), the question escalates to
/// `DecideTier::Local` so the local head answers and caches its judgement into
/// the history index. When `local_enabled` is false, a low-share vote still
/// resolves to `DecideTier::History`.
pub(crate) fn resolve_tier(
    support: &HistorySupport,
    escalate_below: f64,
    local_enabled: bool,
) -> Option<DecideTier> {
    match support {
        HistorySupport::Supported(share) => {
            if local_enabled && *share < escalate_below {
                Some(DecideTier::Local)
            } else {
                Some(DecideTier::History)
            }
        }
        HistorySupport::NotConfigured
        | HistorySupport::Unusable(_)
        | HistorySupport::NoLabelledNeighbour => {
            if local_enabled {
                Some(DecideTier::Local)
            } else {
                None // Hosted is not built; the caller answers with the error.
            }
        }
    }
}

impl DecideTier {
    /// The tier's wire name — the `tier`/`source` field value on both
    /// surfaces and the `source` written into every cached answer, so all
    /// three name the same thing by construction.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            DecideTier::History => "history",
            DecideTier::Local => "local",
            DecideTier::Hosted => "hosted",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The decision cache flywheel (#1061)
// ─────────────────────────────────────────────────────────────────────────────

/// One answer bound for the decisions index: the payload as decided, the
/// label it won, the probability that was served, and the tier that produced
/// it. History-tier answers never appear here — they are the index already,
/// and writing them back would double their vote on the next request.
struct CachedAnswer {
    /// The text that was decided (the question's vote text, clipped).
    text: String,
    /// The winning label.
    label: String,
    /// The probability served for that label.
    p: f64,
    /// The tier that produced the answer — [`DecideTier::as_str`].
    source: &'static str,
}

/// The document one cached answer writes back as. Pure, and unit-tested: the
/// field names ARE the flywheel's contract — the configured text/label fields
/// (so the cached answer is retrievable and votable exactly like a seeded
/// example) plus the fixed `p`, `source`, `ts` of issue #1061.
fn write_back_doc(answer: &CachedAnswer, cfg: &DecisionsConfig, ts: &str) -> Value {
    // Built by hand, not json!: two of the keys are configuration, not
    // syntax, and the json! macro only takes literal keys.
    let mut doc = serde_json::Map::new();
    doc.insert(cfg.text_field.clone(), json!(answer.text));
    doc.insert(cfg.label_field.clone(), json!(answer.label));
    doc.insert("p".into(), json!(round6(answer.p)));
    doc.insert("source".into(), json!(answer.source));
    doc.insert("ts".into(), json!(ts));
    Value::Object(doc)
}

/// The document id one cached answer is written under: a hash of the text
/// that was decided, so each distinct payload holds at most ONE cached answer
/// per index. Since #1094 a low-share history vote escalates to the local
/// head, and the same question asked again can escalate again (its own cached
/// answer is one neighbour among several); appending a fresh document each
/// time would let the local head out-vote the history — human corrections
/// included — by repetition alone. Re-deciding the same text overwrites the
/// earlier cached answer instead. Pure, and unit-tested.
fn write_back_id(text: &str) -> String {
    format!(
        "decide-{:032x}",
        xxhash_rust::xxh3::xxh3_128(text.as_bytes())
    )
}

/// Cache `answers` into the configured decisions index — the flywheel's
/// write-back. Spawned and never awaited by the request, so it can neither
/// delay nor fail it: every error (index cannot be created or reached,
/// document rejected) is a log line naming the index, and the answer the
/// caller already holds stands. Skipped outright when no index is configured
/// — a node with no `[decisions] index` has nowhere to cache, which is the
/// documented state the local tier answers for.
fn spawn_write_back(state: &AppState, cfg: &DecisionsConfig, answers: Vec<CachedAnswer>) {
    if answers.is_empty() || cfg.index.is_empty() {
        return;
    }
    let state = state.clone();
    let cfg = cfg.clone();
    tokio::spawn(async move {
        // The same write path PUT /{index}/_doc takes, including
        // auto-creating a configured-but-missing index: that is how a node
        // armed with the local tier bootstraps its own history.
        let idx = match state.engine.get_or_create_index(&cfg.index) {
            Ok(idx) => idx,
            Err(e) => {
                tracing::warn!(
                    index = %cfg.index, error = %e,
                    "decide flywheel: could not open the decisions index for write-back; the \
                     answer was served but not cached"
                );
                return;
            }
        };
        for answer in &answers {
            let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            match idx
                .index_document(
                    Some(write_back_id(&answer.text)),
                    write_back_doc(answer, &cfg, &ts),
                )
                .await
            {
                Ok(_) => state.metrics.record_doc_indexed(&cfg.index),
                Err(e) => tracing::warn!(
                    index = %cfg.index, error = %e,
                    "decide flywheel: one cached answer was not written; the answer was served"
                ),
            }
        }
    });
}

/// One neighbour's vote weight: reciprocal rank, scaled by
/// [`DecisionsConfig::human_weight`] when the history document carries
/// `human: true` — a correction outranks the answers it corrects. Pure, and
/// unit-tested: this is the flywheel's correction arithmetic.
fn neighbour_weight(rank: usize, human: bool, human_weight: f64) -> f64 {
    let base = 1.0 / (rank as f64 + 1.0);
    if human {
        base * human_weight
    } else {
        base
    }
}

/// Whether a history document is a human correction. Strict — `human: true`
/// as a JSON boolean, exactly — because a correction's weight is trust, and
/// "true"/1/yes must not silently earn it.
fn is_human_correction(source: &Value) -> bool {
    source.get("human") == Some(&Value::Bool(true))
}

// ─────────────────────────────────────────────────────────────────────────────
// Calibration — p_cal beside every p_raw (#1063)
// ─────────────────────────────────────────────────────────────────────────────

/// How many history documents one fit reads, at most: the search page cap
/// (`index.max_result_window`, 10,000 by default). A history with more
/// labelled outcomes than this fits on the first page in engine order,
/// canonically re-sorted before the split — the histories this feature is
/// for (thousands of cached answers, hundreds of recorded outcomes) sit far
/// under it.
const MAX_FIT_DOCS: usize = 10_000;

/// Fewer labelled pairs than this and there is no fit to publish: the
/// response says so rather than extrapolate a curve from a handful of points.
/// Ten is the smallest set whose held-out fifth still evaluates.
const MIN_FIT_PAIRS: usize = 10;

/// A cached fit refreshes at most this often on demand, so a decide request
/// never pays a full index scan per call and an operator who indexes more
/// outcome documents sees the fit move within a minute without a restart.
/// `GET /_decide/_calibration` always refits, whatever this says.
const FIT_REFRESH: Duration = Duration::from_secs(60);

/// One per-index cache entry: the report (or the reason there is none) and
/// when it was computed. The clock stamps the CACHE, never the fit — the fit
/// is a pure function of the pair multiset, the #940 rule.
struct CachedFit {
    report: Result<Arc<CalibrationReport>, String>,
    fitted_at: Instant,
}

/// The node's calibration state: one cached fit per decisions index, held in
/// [`AppState::decide_calibration`] so every probability-shipping path reads
/// the same fit. Fitted lazily on first use, refreshed at most once a minute,
/// force-refreshed by [`decide_calibration`] (the publication endpoint).
#[derive(Default)]
pub struct CalibrationCache {
    entries: tokio::sync::Mutex<HashMap<String, CachedFit>>,
}

impl CalibrationCache {
    /// Publish a fit the caller already computed (the `_calibration`
    /// endpoint's own scan) as the index's cached fit.
    async fn refresh_from(&self, index: &str, report: Arc<CalibrationReport>) {
        self.entries.lock().await.insert(
            index.to_string(),
            CachedFit {
                report: Ok(report),
                fitted_at: Instant::now(),
            },
        );
    }
}

/// What calibration a request has available — the one lookup every
/// probability-emitting path on this surface shares.
enum CalibrationState {
    /// `[decisions] calibration` is `none`: `p_raw` ships alone and no
    /// `p_cal` exists at all.
    Off,
    /// Configured and fitted: the report every `p_cal` and ECE comes from.
    Fitted(Arc<CalibrationReport>),
    /// Configured but no usable fit — the string is why, published beside the
    /// `p_cal: null` it explains.
    Unfit(String),
}

/// Resolve the request's calibration state — at most one scan of the
/// decisions index, cached per `cfg.index`. `force` is the operator's
/// explicit refit. The scan and the fit are deterministic, so two requests
/// between refreshes serve bit-identical `p_cal`.
async fn calibration_state(
    state: &AppState,
    cfg: &DecisionsConfig,
    force: bool,
) -> CalibrationState {
    if cfg.calibration == CalibrationMethod::None {
        return CalibrationState::Off;
    }
    if cfg.index.is_empty() {
        return CalibrationState::Unfit(
            "no decisions index: calibration is fitted on labelled history — set [decisions] \
             index, or name one per request on /_decide"
                .to_string(),
        );
    }
    let mut entries = state.decide_calibration.entries.lock().await;
    let fresh = match entries.get(&cfg.index) {
        Some(cached) => !force && cached.fitted_at.elapsed() <= FIT_REFRESH,
        None => false,
    };
    if fresh {
        let cached = entries.get(&cfg.index).expect("checked above");
        return match &cached.report {
            Ok(report) => CalibrationState::Fitted(report.clone()),
            Err(reason) => CalibrationState::Unfit(reason.clone()),
        };
    }
    let (pairs, ts_range) = match scan_calibration_pairs(state, cfg).await {
        Ok(found) => found,
        Err(reason) => {
            entries.insert(
                cfg.index.clone(),
                CachedFit {
                    report: Err(reason.clone()),
                    fitted_at: Instant::now(),
                },
            );
            return CalibrationState::Unfit(reason);
        }
    };
    let outcome = fit_pairs(cfg, &pairs, ts_range);
    let state_out = match &outcome {
        Ok(report) => CalibrationState::Fitted(report.clone()),
        Err(reason) => CalibrationState::Unfit(reason.clone()),
    };
    entries.insert(
        cfg.index.clone(),
        CachedFit {
            report: outcome,
            fitted_at: Instant::now(),
        },
    );
    state_out
}

/// Fit the configured method on scanned pairs, or say why there is no fit to
/// ship. Pure plumbing over [`xerj_common::calibration::fit_calibration`].
fn fit_pairs(
    cfg: &DecisionsConfig,
    pairs: &[CalibrationPair],
    ts_range: Option<(String, String)>,
) -> Result<Arc<CalibrationReport>, String> {
    if pairs.len() < MIN_FIT_PAIRS {
        return Err(unfit_reason(cfg, pairs.len()));
    }
    let mut report = xerj_common::calibration::fit_calibration(cfg.calibration, pairs)
        .ok_or_else(|| "[decisions] calibration is none; nothing to fit".to_string())?;
    report.ts_range = ts_range;
    Ok(Arc::new(report))
}

/// The published reason a configured calibration has no fit — names what a
/// fit needs and how to record it.
fn unfit_reason(cfg: &DecisionsConfig, pairs: usize) -> String {
    format!(
        "only {pairs} labelled calibration pair{} in `{}` — a fit needs at least {MIN_FIT_PAIRS} \
         (#1063): record outcomes by indexing documents carrying the truth label, the p that \
         was served, and no `source` field (a `source` marks a cached answer, whose label is \
         the model's own prediction, and a fit on predictions would learn the model is right \
         by construction)",
        if pairs == 1 { "" } else { "s" },
        cfg.index
    )
}

/// One history document's calibration pair, or `None` when the document is
/// not a labelled outcome. Pure, and unit-tested: this function IS the
/// data contract of the fit.
///
/// A document is an OUTCOME — not a prediction — when it names a truth label
/// in the configured `label_field`, carries the probability that was served
/// in `p`, and has NO `source` field: `source` marks a flywheel answer
/// (#1061), whose label is the model's own guess, and calibrating on guesses
/// would teach the layer the model is right by construction. Record an
/// outcome with the ordinary indexing path:
/// `{"text": …, "label": <the truth>, "p": <what was served>}` — `human:
/// true` optional, the vote already weights it. The pair is the positive
/// label's probability — `p` when the label is the positive one, `1 − p`
/// when it is not — against the outcome `label == positive_label`.
fn pair_from_doc(source: &Value, cfg: &DecisionsConfig) -> Option<CalibrationPair> {
    if source.get("source").is_some() {
        return None; // a cached answer: a prediction, not an outcome
    }
    let label = source.get(&cfg.label_field).and_then(Value::as_str)?;
    let p = source.get("p").and_then(Value::as_f64)?;
    if !p.is_finite() || !(0.0..=1.0).contains(&p) {
        return None; // a p that is not a probability is not calibration data
    }
    let positive = label == cfg.positive_label;
    Some(CalibrationPair {
        p: if positive { p } else { 1.0 - p },
        y: positive as u8 as f64,
    })
}

/// Scan the decisions index for labelled outcome documents and return them
/// as calibration pairs, with the RFC 3339 range of their `ts` fields.
async fn scan_calibration_pairs(
    state: &AppState,
    cfg: &DecisionsConfig,
) -> Result<(Vec<CalibrationPair>, Option<(String, String)>), String> {
    let idx = state.engine.get_index(&cfg.index).map_err(|e| {
        format!(
            "decisions index `{}` could not be searched for calibration pairs: {e}",
            cfg.index
        )
    })?;
    let query_body = json!({
        "query": { "match_all": {} },
        "size": MAX_FIT_DOCS,
        "_source": true,
    });
    let search_req =
        parse_request(&query_body).map_err(|e| format!("internal query would not parse: {e}"))?;
    let result = idx
        .search(&search_req)
        .await
        .map_err(|e| format!("decisions index `{}` could not be searched: {e}", cfg.index))?;
    let mut pairs = Vec::with_capacity(result.hits.len());
    let mut oldest: Option<String> = None;
    let mut newest: Option<String> = None;
    for hit in &result.hits {
        if let Some(pair) = pair_from_doc(&hit.source, cfg) {
            pairs.push(pair);
            // The published range spans the FITTED documents — the ones that
            // became pairs — not every cached answer in the index.
            if let Some(ts) = hit.source.get("ts").and_then(Value::as_str) {
                // RFC 3339 sorts lexicographically, so min/max by string is
                // the range.
                oldest = Some(match oldest {
                    Some(o) if o.as_str() <= ts => o,
                    _ => ts.to_string(),
                });
                newest = Some(match newest {
                    Some(n) if n.as_str() >= ts => n,
                    _ => ts.to_string(),
                });
            }
        }
    }
    Ok((pairs, oldest.zip(newest)))
}

/// `p_cal` for one `p_raw` under the request's calibration state: absent
/// when calibration is off, `null` when configured but unfit (the reason
/// rides in the calibration block), and the fit's value otherwise — never a
/// copy of `p_raw` pretending to be calibrated.
fn p_cal_value(state: &CalibrationState, p_raw: f64) -> Option<Value> {
    match state {
        CalibrationState::Off => None,
        CalibrationState::Unfit(_) => Some(Value::Null),
        CalibrationState::Fitted(report) => Some(json!(round6(report.fit.apply(p_raw)))),
    }
}

/// The calibration block a response carries whenever calibration is
/// configured: the method, its scope, the fit's held-out ECE raw and
/// calibrated, and what it was fitted on. This block is the "ECE beside
/// every probability" — the surface's honesty contract (#1063).
fn calibration_block(state: &CalibrationState) -> Option<Value> {
    match state {
        CalibrationState::Off => None,
        CalibrationState::Fitted(report) => Some(json!({
            "method": report.method.as_str(),
            "scope": "noul",
            "ece": {
                "raw": round6(report.ece_raw),
                "calibrated": round6(report.ece_calibrated),
            },
            "fitted_on": report.fitted_on,
            "held_out": report.held_out,
            "labelled_pairs": report.labelled_pairs,
        })),
        CalibrationState::Unfit(reason) => Some(json!({
            "reason": reason,
        })),
    }
}

/// The fit's parameters, published by `/_decide/_calibration`: an isotonic
/// fit's knots, or the temperature.
fn fit_params_json(fit: &FittedCalibration) -> Value {
    match fit {
        FittedCalibration::Isotonic(iso) => json!({
            "knots": iso
                .knots()
                .iter()
                .map(|(x, y)| json!([round6(*x), round6(*y)]))
                .collect::<Vec<_>>(),
        }),
        FittedCalibration::Temperature(t) => json!({ "temperature": round6(t.t) }),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// DecideSettings — the node's ladder configuration
// ─────────────────────────────────────────────────────────────────────────────

/// The node's decide-ladder configuration, resolved once at boot and held in
/// [`AppState`] — the same seam `state.rerank` uses. The runtime switch is
/// `XERJ_DECIDE_MODE` / `XERJ_DECIDE_MODEL_DIR` (the env half of
/// `--decide-mode local`; the CLI flag lands with the xerj-server wiring),
/// mirroring how `XERJ_EMBED_MODE` backs `--embed-mode`.
#[derive(Clone, Default)]
pub struct DecideSettings {
    /// The local tier, when enabled and compiled in.
    #[cfg(feature = "decide-local")]
    local: Option<LocalTier>,
}

impl std::fmt::Debug for DecideSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Not the handle: whether the tier is armed. The model directory is
        // in the logs and the error bodies, not needed on every Debug print.
        f.debug_struct("DecideSettings")
            .field("local", &self.local_available())
            .finish()
    }
}

/// The enabled local tier: a lazily-loaded decision head and the directory
/// it loads from.
#[cfg(feature = "decide-local")]
#[derive(Clone)]
pub struct LocalTier {
    handle: xerj_ai::decide::DecideHandle,
}

impl DecideSettings {
    /// The default node: history vote only, today's error contract.
    pub fn history_only() -> Self {
        Self::default()
    }

    /// A node with the local tier enabled, pointing at `model_dir`. Public
    /// so tests can arm a node directly and the xerj-server `--decide-mode`
    /// wiring can construct the same thing.
    #[cfg(feature = "decide-local")]
    pub fn local(model_dir: PathBuf) -> Self {
        Self {
            local: Some(LocalTier {
                handle: xerj_ai::decide::DecideHandle::new(xerj_ai::decide::DecideConfig {
                    model_dir,
                }),
            }),
        }
    }

    /// Resolve the settings from raw strings — pure, and unit-tested.
    /// Returns the settings plus an operator-facing warning when the request
    /// could not be honoured as asked (unknown mode, missing directory,
    /// feature not compiled in). A warning, never a panic: a boot that
    /// dies inside state construction hides the message that explains it.
    pub fn resolve(mode: &str, model_dir: Option<&str>) -> (Self, Option<String>) {
        let mode = mode.trim().to_ascii_lowercase();
        match mode.as_str() {
            "" | "history" => (Self::history_only(), None),
            "local" => Self::resolve_local(model_dir),
            other => (
                Self::history_only(),
                Some(format!(
                    "XERJ_DECIDE_MODE={other:?} is not a decide mode; use history or local"
                )),
            ),
        }
    }

    #[cfg_attr(not(feature = "decide-local"), allow(unused_variables))]
    fn resolve_local(model_dir: Option<&str>) -> (Self, Option<String>) {
        #[cfg(feature = "decide-local")]
        {
            match model_dir.map(str::trim).filter(|d| !d.is_empty()) {
                Some(dir) => {
                    let path = PathBuf::from(dir);
                    if path.is_dir() {
                        (Self::local(path), None)
                    } else {
                        // Arm it anyway: the first request then fails loudly
                        // naming the directory, which is the honest failure
                        // for a mode the operator explicitly asked for.
                        (
                            Self::local(path),
                            Some(format!(
                                "XERJ_DECIDE_MODEL_DIR={dir:?} is not a directory; the \
                                 local decide tier is armed and its first request will \
                                 name this path until the model directory is in place"
                            )),
                        )
                    }
                }
                None => (
                    Self::history_only(),
                    Some(
                        "XERJ_DECIDE_MODE=local needs XERJ_DECIDE_MODEL_DIR naming the \
                         directory holding config.json, tokenizer.json and \
                         model.safetensors; staying on the history vote"
                            .to_string(),
                    ),
                ),
            }
        }
        #[cfg(not(feature = "decide-local"))]
        {
            (
                Self::history_only(),
                Some(
                    "XERJ_DECIDE_MODE=local was requested but this binary was built \
                     without the decide-local feature; rebuild with \
                     `cargo build --release -p xerj-server --features decide-local`"
                        .to_string(),
                ),
            )
        }
    }

    /// Read [`Self::resolve`]'s inputs from the environment and log any
    /// warning. Read exactly once, at [`AppState`] construction — the same
    /// resolve-once discipline as `state.rerank`, so request paths never
    /// race on process-wide env state.
    pub fn from_env() -> Self {
        let mode = std::env::var("XERJ_DECIDE_MODE").unwrap_or_default();
        let dir = std::env::var("XERJ_DECIDE_MODEL_DIR").ok();
        let (settings, warning) = Self::resolve(&mode, dir.as_deref());
        if let Some(warning) = warning {
            tracing::error!("{warning}");
        }
        settings
    }

    /// Whether the local tier can answer (compiled in, enabled, model not
    /// yet necessarily loaded — the load is lazy and its failure surfaces on
    /// the request that triggered it).
    pub fn local_available(&self) -> bool {
        #[cfg(feature = "decide-local")]
        {
            self.local.is_some()
        }
        #[cfg(not(feature = "decide-local"))]
        {
            false
        }
    }

    /// The armed local tier, cloned, when compiled in and enabled. `None`
    /// otherwise — including when the feature is off, so a call site needs no
    /// cfg of its own to ask "is there a local tier on this node?" (Only the
    /// code that USES the tier is feature-gated.)
    #[cfg(feature = "decide-local")]
    pub(crate) fn local_tier(&self) -> Option<LocalTier> {
        self.local.clone()
    }

    #[cfg(not(feature = "decide-local"))]
    pub(crate) fn local_tier(&self) -> Option<std::convert::Infallible> {
        None
    }
}

#[cfg(feature = "decide-local")]
impl LocalTier {
    /// The configured model directory, surfaced in error bodies.
    pub(crate) fn model_dir(&self) -> &Path {
        self.handle.model_dir()
    }

    /// Score one request per question. The head loads lazily on first use,
    /// and the forward pass runs off the async executor
    /// ([`xerj_ai::decide`]).
    pub(crate) async fn score(
        &self,
        requests: Vec<xerj_ai::decide::ScoreRequest>,
    ) -> anyhow::Result<Vec<Vec<f32>>> {
        self.handle.score(requests).await
    }
}

/// The candidate labels one question is judged against, in order — the local
/// head's label vocabulary for that question. Pure, and always compiled: it
/// is the shared definition of what "an answer" is for each kind, used by the
/// tier check above and the local scoring pass below.
fn labels_for(question: &Question, cfg: &DecisionsConfig) -> Vec<String> {
    match &question.kind {
        // A noul is scored as two competing statements — the positive label
        // and its negation — so the answer is a comparison, never one
        // unopposed score. `hypothesis("not spam")` reads exactly
        // `negation_hypothesis("spam")`.
        Kind::Noul => vec![
            cfg.positive_label.clone(),
            format!("not {}", cfg.positive_label),
        ],
        Kind::Choice { options } => options.clone(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// POST /v1/systemone
// ─────────────────────────────────────────────────────────────────────────────

pub async fn systemone(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> axum::response::Response {
    let started = Instant::now();
    let cfg = state.config.decisions.clone();
    let local_enabled = state.decide.local_available();
    if cfg.index.is_empty() && !local_enabled {
        return not_configured();
    }
    // One calibration lookup per request (#1063): every tier's noul
    // probability below reads the same fit, and the answers' strictly-parsed
    // wire objects stay untouched — p_raw/p_cal ride in `decisions.evidence`,
    // the extras channel established clients ignore.
    let calibration = calibration_state(&state, &cfg, false).await;

    let questions = match body.get("questions") {
        Some(Value::Object(q)) if !q.is_empty() => q,
        _ => return unprocessable("`questions` must be a non-empty object"),
    };
    if questions.len() > MAX_QUESTIONS {
        return unprocessable(&format!(
            "`questions` must hold at most {MAX_QUESTIONS} questions (got {})",
            questions.len()
        ));
    }
    let state_val = body.get("state").cloned().unwrap_or(Value::Null);

    // Per question: the resolved vote text and the parsed type/criteria.
    let mut parsed: Vec<(String, Question)> = Vec::with_capacity(questions.len());
    for (id, q) in questions {
        match parse_question(id, q, &state_val) {
            Ok(question) => parsed.push((id.clone(), question)),
            Err(reason) => return unprocessable(&reason),
        }
    }

    // One ladder resolution per question. Unsupported questions are
    // collected first so a no-support question still fails the whole
    // request when no tier can answer it — the wire has no per-question
    // error field, and a half-answered judgement history would look like a
    // ranking.
    let mut answers = serde_json::Map::new();
    let mut evidence = serde_json::Map::new();
    let mut unsupported: Vec<String> = Vec::new();
    // (index into `parsed`, the question's candidate labels). The labels are
    // resolved per kind here so the local scoring pass below never re-parses
    // the question.
    let mut local_needed: Vec<(usize, Vec<String>)> = Vec::new();
    for (i, (id, question)) in parsed.iter().enumerate() {
        let support = if cfg.index.is_empty() {
            Err(HistorySupport::NotConfigured)
        } else {
            vote_neighbours(&state, &cfg, &question.vote_text).await
        };
        match support {
            Ok(neighbours) => {
                let labelled: Vec<(f64, &str)> = neighbours
                    .iter()
                    .map(|(w, label, _)| (*w, label.as_str()))
                    .collect();
                // A choice also needs weight on at least one of ITS options;
                // neighbours that share no label with the criteria cannot
                // answer the question asked.
                let criteria_ok = match &question.kind {
                    Kind::Noul => true,
                    Kind::Choice { options } => options.iter().any(|o| {
                        labelled
                            .iter()
                            .any(|(w, label)| *label == o.as_str() && *w > 0.0)
                    }),
                };
                if labelled.is_empty() || !criteria_ok {
                    // Tier 2: the local head answers what the history
                    // cannot; without it the documented errors stand.
                    if resolve_tier(
                        &HistorySupport::NoLabelledNeighbour,
                        cfg.escalate_below,
                        local_enabled,
                    )
                    .is_some()
                    {
                        local_needed.push((i, labels_for(question, &cfg)));
                    } else {
                        unsupported.push(id.clone());
                    }
                    continue;
                }
                let total: f64 = labelled.iter().map(|(w, _)| *w).sum();
                let mut weight_by_label: BTreeMap<&str, f64> = BTreeMap::new();
                for (w, label) in &labelled {
                    *weight_by_label.entry(label).or_insert(0.0) += *w;
                }
                let (best_label, best_weight) = weight_by_label
                    .iter()
                    .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
                    .expect("non-empty");
                let share =
                    |label: &str| weight_by_label.get(label).copied().unwrap_or(0.0) / total;
                let winning_share = match &question.kind {
                    Kind::Noul => *best_weight / total,
                    Kind::Choice { options } => options
                        .iter()
                        .map(|o| share(o.as_str()))
                        .fold(0.0_f64, f64::max),
                };
                if resolve_tier(
                    &HistorySupport::Supported(winning_share),
                    cfg.escalate_below,
                    local_enabled,
                ) == Some(DecideTier::Local)
                {
                    local_needed.push((i, labels_for(question, &cfg)));
                    continue;
                }
                match &question.kind {
                    Kind::Noul => {
                        // Exactly the documented fields — clients parse answers
                        // strictly and break on extras.
                        answers.insert(
                            id.clone(),
                            json!({ "type": "noul", "noul": round6(share(&cfg.positive_label)) }),
                        );
                    }
                    Kind::Choice { options } => {
                        let mut probabilities = serde_json::Map::new();
                        for option in options {
                            probabilities.insert(
                                option.clone(),
                                json!(round6(
                                    weight_by_label.get(option.as_str()).copied().unwrap_or(0.0)
                                        / total
                                )),
                            );
                        }
                        let (winner, _) = options
                            .iter()
                            .map(|o| (o, weight_by_label.get(o.as_str()).copied().unwrap_or(0.0)))
                            .max_by(|(a, aw), (b, bw)| aw.partial_cmp(bw).unwrap().then(a.cmp(b)))
                            .expect("non-empty options");
                        answers.insert(
                            id.clone(),
                            json!({
                                "type": "choice",
                                "choice": winner,
                                "confidence": round6(weight_by_label.get(winner.as_str()).copied().unwrap_or(0.0) / total),
                                "probabilities": Value::Object(probabilities),
                            }),
                        );
                    }
                }
                let mut evidence_value = json!({
                    "tier": DecideTier::History.as_str(),
                    "source": DecideTier::History.as_str(),
                    "label": best_label,
                    "support": round6(best_weight / total),
                    "neighbours": labelled.len(),
                    "found": neighbours.len(),
                });
                // The calibrated quantity is the noul's — the positive
                // label's share — so only a noul question carries p_raw/p_cal
                // here. A choice's per-option shares are a different
                // quantity; the decisions.calibration block names the scope.
                if matches!(question.kind, Kind::Noul) {
                    let p_raw = share(&cfg.positive_label);
                    evidence_value["p_raw"] = json!(round6(p_raw));
                    if let Some(p_cal) = p_cal_value(&calibration, p_raw) {
                        evidence_value["p_cal"] = p_cal;
                    }
                }
                evidence.insert(id.clone(), evidence_value);
            }
            Err(failure) => {
                match resolve_tier(&failure, cfg.escalate_below, local_enabled) {
                    Some(DecideTier::Local) => {
                        local_needed.push((i, labels_for(question, &cfg)));
                    }
                    _ => {
                        // Unusable is surfaced as the index error it is;
                        // NotConfigured cannot occur (guarded above).
                        let HistorySupport::Unusable(cause) = failure else {
                            unreachable!("NotConfigured is guarded at the top of the handler");
                        };
                        return index_error(&cfg.index, &cause);
                    }
                }
            }
        }
    }
    if !unsupported.is_empty() {
        return no_support(&unsupported, &cfg);
    }

    // Tier 2: score every local-bound question in one batched pass. A model
    // that cannot load is an honest failure — never a fabricated answer and
    // never a silent fallthrough to the errors the operator switched away
    // from.
    #[cfg_attr(not(feature = "decide-local"), allow(unused_mut))]
    let mut local_answered = 0usize;
    // The flywheel's cargo (#1061): every answer tiers 2+ produced here is
    // written back to the decisions index after the response is built. The
    // hosted tier, when it is built, pushes into the same vector.
    #[cfg_attr(not(feature = "decide-local"), allow(unused_mut))]
    let mut cached: Vec<CachedAnswer> = Vec::new();
    #[cfg(feature = "decide-local")]
    if !local_needed.is_empty() {
        let Some(local) = state.decide.local_tier() else {
            unreachable!("local_needed is only populated when the local tier is armed");
        };
        let requests: Vec<xerj_ai::decide::ScoreRequest> = local_needed
            .iter()
            .map(|(i, labels)| {
                let question = &parsed[*i].1;
                xerj_ai::decide::ScoreRequest {
                    premise: question.vote_text.clone(),
                    hypotheses: labels
                        .iter()
                        .map(|l| xerj_ai::decide::hypothesis(l))
                        .collect(),
                }
            })
            .collect();
        let scores = match local.score(requests).await {
            Ok(scores) => scores,
            Err(e) => return local_unavailable(&e, local.model_dir()),
        };
        for ((i, labels), scores) in local_needed.iter().zip(scores) {
            let (id, question) = &parsed[*i];
            let mut by_label: serde_json::Map<String, Value> = serde_json::Map::new();
            for (label, score) in labels.iter().zip(&scores) {
                by_label.insert(label.clone(), json!(round6(*score as f64)));
            }
            match &question.kind {
                Kind::Noul => {
                    // The positive label is `labels[0]`; its negation is
                    // `labels[1]`. The pair sums to 1, so the positive's
                    // share is the noul.
                    answers.insert(
                        id.clone(),
                        json!({ "type": "noul", "noul": round6(scores[0] as f64) }),
                    );
                }
                Kind::Choice { options } => {
                    // `labels_for` made labels == options, in order, so the
                    // per-option score is a direct zip.
                    let score_of = |option: &str| {
                        scores[options
                            .iter()
                            .position(|o| o == option)
                            .expect("every option is a scored label")]
                    };
                    let winner = options
                        .iter()
                        .max_by(|a, b| {
                            score_of(a)
                                .partial_cmp(&score_of(b))
                                .unwrap()
                                .then(a.cmp(b))
                        })
                        .expect("non-empty options");
                    let mut probabilities = serde_json::Map::new();
                    for option in options {
                        probabilities
                            .insert(option.clone(), json!(round6(score_of(option) as f64)));
                    }
                    answers.insert(
                        id.clone(),
                        json!({
                            "type": "choice",
                            "choice": winner,
                            "confidence": round6(score_of(winner) as f64),
                            "probabilities": Value::Object(probabilities),
                        }),
                    );
                }
            }
            let (best_label, best_score) = labels
                .iter()
                .zip(&scores)
                .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
                .expect("labels are non-empty");
            let mut evidence_value = json!({
                "tier": DecideTier::Local.as_str(),
                "source": DecideTier::Local.as_str(),
                "model": LOCAL_MODEL_ID,
                "label": best_label,
                "support": round6(*best_score as f64),
                "hypotheses": Value::Object(by_label),
            });
            // The head's noul is `scores[0]` — the positive hypothesis's
            // probability — so it calibrates through the same seam as the
            // vote's (and, when that tier exists, a hosted score's).
            if matches!(question.kind, Kind::Noul) {
                let p_raw = scores[0] as f64;
                evidence_value["p_raw"] = json!(round6(p_raw));
                if let Some(p_cal) = p_cal_value(&calibration, p_raw) {
                    evidence_value["p_cal"] = p_cal;
                }
            }
            evidence.insert(id.clone(), evidence_value);
            cached.push(CachedAnswer {
                text: question.vote_text.clone(),
                label: best_label.clone(),
                p: *best_score as f64,
                source: DecideTier::Local.as_str(),
            });
            local_answered += 1;
        }
    }

    // The flywheel's write-back (#1061): cache what tiers 2+ answered, after
    // the answer is complete and without ever holding the response for it.
    // History-tier answers are not in `cached` — they are the index already.
    spawn_write_back(&state, &cfg, cached);

    let index_echo = if cfg.index.is_empty() {
        LOCAL_MODEL_ID.to_string()
    } else {
        cfg.index.clone()
    };
    state.metrics.record_query(
        &index_echo,
        if local_answered > 0 {
            "systemone_local"
        } else {
            "systemone_vote"
        },
        started.elapsed().as_secs_f64(),
    );
    let mut decisions = serde_json::Map::new();
    decisions.insert("index".into(), json!(index_echo));
    decisions.insert("k".into(), json!(cfg.k));
    decisions.insert("model".into(), json!(MODEL_ID));
    decisions.insert(
        "requested_model".into(),
        json!(body.get("model").and_then(Value::as_str).unwrap_or("")),
    );
    decisions.insert("evidence".into(), Value::Object(evidence));
    decisions.insert(
        "took_ms".into(),
        json!(started.elapsed().as_millis() as u64),
    );
    // The ECE beside every probability (#1063): present whenever calibration
    // is configured — fitted (method, ECE, fitted-on) or unfit (reason) —
    // and absent when the node never promised a p_cal.
    if let Some(block) = calibration_block(&calibration) {
        decisions.insert("calibration".into(), block);
    }
    if local_answered > 0 {
        decisions.insert("local_model".into(), json!(LOCAL_MODEL_ID));
        decisions.insert("local_answered".into(), json!(local_answered));
    }
    Json(json!({
        "model": MODEL_ID,
        "answers": Value::Object(answers),
        "usage": { "input_tokens": 0, "output_tokens": 0 },
        "decisions": Value::Object(decisions),
    }))
    .into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// GET /v1/models
// ─────────────────────────────────────────────────────────────────────────────

/// The official SDKs list models before calling. Truthful entries only: the
/// vote, and — when the local tier is armed — the local head, each under its
/// own never-a-Jev-name id.
pub async fn models(State(state): State<AppState>) -> axum::response::Response {
    let configured = !state.config.decisions.index.is_empty();
    let local = state.decide.local_available();
    let vote_description = if configured {
        "Weighted nearest-neighbour vote over the configured decisions index — local, no egress"
    } else if local {
        "Weighted nearest-neighbour vote over a labelled-history index (no [decisions] index \
         configured: /v1/systemone answers from the local decision head)"
    } else {
        "Weighted nearest-neighbour vote over a labelled-history index (no [decisions] index \
         configured: /v1/systemone answers 503)"
    };
    let mut models = vec![json!({
        "name": MODEL_ID,
        "description": vote_description,
        "release_date": "2026-09-20",
    })];
    if local {
        models.push(json!({
            "name": LOCAL_MODEL_ID,
            "description": "Local zero-shot decision head (ModernBERT-class, candle) — answers \
                            noul and choice with no history index; local files only, no egress",
            "release_date": "2026-09-29",
        }));
    }
    Json(json!({ "models": models })).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// POST /_decide
// ─────────────────────────────────────────────────────────────────────────────

/// The audit surface: the same ladder as `/v1/systemone`, naming its index
/// per request, returning the neighbours it judged by, and abstaining instead
/// of erroring. The local tier is the one difference in kind: with it armed,
/// `index` becomes optional — a caller with no labelled history at all still
/// gets an answer, tier-tagged.
pub async fn decide(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> axum::response::Response {
    let started = Instant::now();
    let defaults = &state.config.decisions;
    let local = state.decide.local_tier();
    let index = match body.get("index").and_then(Value::as_str) {
        Some(i) if !i.trim().is_empty() => i.to_string(),
        _ if local.is_some() => String::new(),
        _ => return unprocessable("`index` must name the judgement-history index"),
    };
    let question = match body.get("question").and_then(Value::as_str) {
        Some(q) if !q.trim().is_empty() => q.to_string(),
        _ => return unprocessable("`question` must be a non-empty string"),
    };
    let k = body
        .get("k")
        .and_then(Value::as_u64)
        .map(|v| v.clamp(1, 100) as usize)
        .unwrap_or(defaults.k);
    let positive = body
        .get("positive_label")
        .and_then(Value::as_str)
        .unwrap_or(&defaults.positive_label)
        .to_string();
    let mut cfg = DecisionsConfig {
        index: index.clone(),
        k,
        positive_label: positive.clone(),
        ..defaults.clone()
    };
    cfg.label_field = defaults.label_field.clone();
    cfg.text_field = defaults.text_field.clone();

    // One calibration lookup per request (#1063), against the index THIS
    // request named — the fit is cached per index, so the audit surface and
    // the wire surface read the same fit for the same history.
    let calibration = calibration_state(&state, &cfg, false).await;

    let neighbours = if cfg.index.is_empty() {
        Err(HistorySupport::NotConfigured)
    } else {
        vote_neighbours(&state, &cfg, &clip(&question, MAX_VOTE_TEXT_CHARS)).await
    };
    // The ladder, /_decide-shaped: an empty, unreachable, or below-floor
    // history hands the question to the local head; with no tier able to
    // answer, the audit surface does what it always did — abstain on zero
    // support, error on an unusable index.
    let (support, total, weight_by_label) = match &neighbours {
        Ok(n) if n.is_empty() => (HistorySupport::NoLabelledNeighbour, 0.0, BTreeMap::new()),
        Ok(n) => {
            let total: f64 = n.iter().map(|(w, _, _)| *w).sum();
            let mut by_label: BTreeMap<String, f64> = BTreeMap::new();
            for (w, label, _) in n {
                *by_label.entry(label.to_string()).or_insert(0.0) += *w;
            }
            let winning_share = if total > 0.0 {
                by_label.values().copied().fold(0.0_f64, f64::max) / total
            } else {
                0.0
            };
            (HistorySupport::Supported(winning_share), total, by_label)
        }
        Err(failure) => (failure.clone(), 0.0, BTreeMap::new()),
    };
    if resolve_tier(&support, cfg.escalate_below, local.is_some()) == Some(DecideTier::Local) {
        #[cfg(feature = "decide-local")]
        {
            let Some(local) = local.as_ref() else {
                unreachable!("resolve_tier names Local only when the tier is armed");
            };
            return decide_local(&state, local, &question, &cfg, started).await;
        }
    }
    let neighbours = match neighbours {
        Ok(n) => n,
        Err(failure) => {
            let HistorySupport::Unusable(cause) = failure else {
                unreachable!("NotConfigured implies the local tier, which answered above")
            };
            return index_error(&cfg.index, &cause);
        }
    };
    // The calibrated quantity (#1063): the positive label's share — the same
    // quantity a noul answers on the wire — NOT the winner's share that
    // `confidence` reports. `confidence` keeps its raw meaning and the
    // abstain gate keeps applying to it: calibration changes what the number
    // means, not the verdict.
    let p_raw = if total > 0.0 {
        weight_by_label.get(&positive).copied().unwrap_or(0.0) / total
    } else {
        0.0
    };
    let (label, confidence, abstain, reason) = match weight_by_label
        .iter()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
    {
        Some((l, w)) if total > 0.0 => {
            let c = *w / total;
            let abstain = c < defaults.min_confidence;
            let reason = if abstain {
                Some(format!(
                    "confidence {:.3} below decisions.min_confidence {:.3}",
                    c, defaults.min_confidence
                ))
            } else {
                None
            };
            (Some(l.clone()), c, abstain, reason)
        }
        _ => (
            None,
            0.0,
            true,
            Some("no labelled neighbour in the history index".to_string()),
        ),
    };

    let neighbour_list: Vec<Value> = neighbours
        .iter()
        .map(|(w, label, hit)| {
            json!({
                "_id": hit.id,
                "label": label,
                "_score": hit.score,
                "weight": round6(*w),
                "text": hit.source.get(&cfg.text_field).cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    state
        .metrics
        .record_query(&index, "decide", started.elapsed().as_secs_f64());

    let mut resp = json!({
        "index": index,
        "k": k,
        "positive_label": positive,
        "label": label,
        "confidence": round6(confidence),
        "abstain": abstain,
        "neighbours": neighbour_list,
        "tier": DecideTier::History.as_str(),
        "source": DecideTier::History.as_str(),
        "took_ms": started.elapsed().as_millis() as u64,
    });
    // p_cal beside p_raw, never replacing it (#1063): p_raw is always the
    // honest raw share, p_cal exists only when calibration is configured
    // (null, with the reason in the block, when configured but unfitted),
    // and the calibration block is the ECE that rides beside both.
    resp["p_raw"] = json!(round6(p_raw));
    if let Some(p_cal) = p_cal_value(&calibration, p_raw) {
        resp["p_cal"] = p_cal;
    }
    resp["calibration"] =
        calibration_block(&calibration).unwrap_or(json!({ "configured": "none" }));
    if let Some(r) = reason {
        resp["reason"] = json!(r);
    }
    Json(resp).into_response()
}

/// The local tier's `/_decide` answer: the head's probabilities over the
/// positive label and its negation, tier-tagged, abstaining by the same
/// `decisions.min_confidence` rule the history vote uses. `neighbours` is
/// empty — having none is exactly what tier 2 means here. A non-abstaining
/// answer over a named index is cached into it (the flywheel, #1061); an
/// abstain is not — it was not an answer, and caching it would seed the
/// history with a doubt.
#[cfg(feature = "decide-local")]
async fn decide_local(
    state: &AppState,
    local: &LocalTier,
    question: &str,
    cfg: &DecisionsConfig,
    started: Instant,
) -> axum::response::Response {
    // `cfg` is the caller's per-request view: its index is the index THIS
    // request named (empty when it named none), so the flywheel caches into
    // the index the question was asked of — never a different one.
    let (index, k, positive) = (&cfg.index, cfg.k, cfg.positive_label.as_str());
    let labels = [positive.to_string(), format!("not {positive}")];
    let requests = vec![xerj_ai::decide::ScoreRequest {
        premise: clip(question, MAX_VOTE_TEXT_CHARS),
        hypotheses: labels
            .iter()
            .map(|l| xerj_ai::decide::hypothesis(l))
            .collect(),
    }];
    let scores = match local.score(requests).await {
        Ok(scores) => scores,
        Err(e) => return local_unavailable(&e, local.model_dir()),
    };
    let scores = &scores[0];
    // The head's positive-label probability is `scores[0]` — the same
    // quantity a noul answers, through the same calibration seam as every
    // other tier's (#1063).
    let p_raw = scores[0] as f64;
    let calibration = calibration_state(state, cfg, false).await;
    let (label, confidence) = if scores[0] >= scores[1] {
        (labels[0].clone(), scores[0] as f64)
    } else {
        (labels[1].clone(), scores[1] as f64)
    };
    let abstain = confidence < cfg.min_confidence;
    if !abstain {
        spawn_write_back(
            state,
            cfg,
            vec![CachedAnswer {
                text: clip(question, MAX_VOTE_TEXT_CHARS),
                label: label.clone(),
                p: confidence,
                source: DecideTier::Local.as_str(),
            }],
        );
    }
    state.metrics.record_query(
        LOCAL_MODEL_ID,
        "decide_local",
        started.elapsed().as_secs_f64(),
    );
    let mut resp = json!({
        "index": index,
        "k": k,
        "positive_label": positive,
        "label": label,
        "confidence": round6(confidence),
        "abstain": abstain,
        "neighbours": [],
        "tier": DecideTier::Local.as_str(),
        "source": DecideTier::Local.as_str(),
        "model": LOCAL_MODEL_ID,
        "took_ms": started.elapsed().as_millis() as u64,
    });
    // The audit surface always names its calibration state, and p_cal rides
    // beside p_raw exactly as on the history tier (#1063).
    resp["p_raw"] = json!(round6(p_raw));
    if let Some(p_cal) = p_cal_value(&calibration, p_raw) {
        resp["p_cal"] = p_cal;
    }
    resp["calibration"] =
        calibration_block(&calibration).unwrap_or(json!({ "configured": "none" }));
    if abstain {
        resp["reason"] = json!(format!(
            "confidence {:.3} below decisions.min_confidence {:.3}",
            confidence, cfg.min_confidence
        ));
    }
    Json(resp).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// GET /_decide/_calibration
// ─────────────────────────────────────────────────────────────────────────────

/// Publish the calibration (#1063): the reliability curve of the labelled
/// history — binned `p_raw` against empirical frequency, with bin counts —
/// the ECE raw and calibrated, the fit's parameters, what it was fitted on,
/// and the date range of the fitted documents. Always refits, so this is the
/// operator's refresh as well as the publication; the fit it produces becomes
/// the cached fit every decide request reads.
///
/// `?index=` names the history (default: the configured `[decisions] index`).
/// The curve and raw ECE publish even when calibration is not configured —
/// the reliability of the raw probabilities is audit information in its own
/// right — with a `reason` naming the setting that would fit them.
pub async fn decide_calibration(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> axum::response::Response {
    let defaults = &state.config.decisions;
    let index = params
        .get("index")
        .map(String::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(defaults.index.as_str())
        .to_string();
    if index.is_empty() {
        return unprocessable(
            "name an index with ?index=… or set [decisions] index — calibration is fitted \
             on a labelled history",
        );
    }
    let cfg = DecisionsConfig {
        index: index.clone(),
        ..defaults.clone()
    };
    let (pairs, ts_range) = match scan_calibration_pairs(&state, &cfg).await {
        Ok(found) => found,
        Err(reason) => return index_error(&index, &reason),
    };
    let curve_raw = xerj_common::calibration::reliability_curve(
        &pairs,
        xerj_common::calibration::CALIBRATION_BINS,
    );
    let ece_raw = xerj_common::calibration::ece_from_curve(&curve_raw);

    let mut resp = serde_json::Map::new();
    resp.insert("index".into(), json!(index));
    resp.insert("configured".into(), json!(cfg.calibration.as_str()));
    resp.insert("labelled_pairs".into(), json!(pairs.len()));
    resp.insert(
        "bins".into(),
        json!(xerj_common::calibration::CALIBRATION_BINS),
    );
    resp.insert(
        "curve".into(),
        json!({ "raw": curve_raw, "calibrated": Value::Null }),
    );
    resp.insert(
        "ece".into(),
        json!({ "raw": round6(ece_raw), "calibrated": Value::Null }),
    );
    if let Some((lo, hi)) = &ts_range {
        resp.insert("ts_range".into(), json!([lo, hi]));
    }
    match fit_pairs(&cfg, &pairs, ts_range) {
        Ok(report) => {
            resp.insert("method".into(), json!(report.method.as_str()));
            resp.insert("scope".into(), json!("noul"));
            resp.insert("fitted_on".into(), json!(report.fitted_on));
            resp.insert("held_out".into(), json!(report.held_out));
            resp.insert("params".into(), fit_params_json(&report.fit));
            resp.insert(
                "ece".into(),
                json!({
                    "raw": round6(report.ece_raw),
                    "calibrated": round6(report.ece_calibrated),
                }),
            );
            resp.insert(
                "curve".into(),
                json!({
                    "raw": report.curve_raw,
                    "calibrated": report.curve_calibrated,
                }),
            );
            // The publication is the refresh: every decide request after
            // this reads the fit just published.
            state.decide_calibration.refresh_from(&index, report).await;
        }
        Err(reason) => {
            resp.insert("reason".into(), json!(reason));
        }
    }
    Json(Value::Object(resp)).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// The vote
// ─────────────────────────────────────────────────────────────────────────────

struct Question {
    vote_text: String,
    kind: Kind,
}

enum Kind {
    Noul,
    Choice { options: Vec<String> },
}

fn parse_question(id: &str, q: &Value, state_val: &Value) -> Result<Question, String> {
    let obj = q
        .as_object()
        .ok_or_else(|| format!("question `{id}` must be an object"))?;
    let kind = match obj.get("type").and_then(Value::as_str) {
        Some("noul") => Kind::Noul,
        Some("choice") => {
            let criteria = obj
                .get("criteria")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    format!("question `{id}` is a choice and needs `criteria` naming its options")
                })?;
            if criteria.is_empty() || criteria.len() > MAX_CHOICE_OPTIONS {
                return Err(format!(
                    "question `{id}`: `criteria` must name 1..{MAX_CHOICE_OPTIONS} options (got {})",
                    criteria.len()
                ));
            }
            Kind::Choice {
                options: criteria.keys().cloned().collect(),
            }
        }
        Some("score") => {
            return Err(format!(
                "question `{id}`: `score` questions (2-10 ordinal levels) have no vote analogue \
                 and are not served; split them into nouls or use /_decide"
            ))
        }
        other => {
            return Err(format!(
                "question `{id}`: `type` must be \"noul\" or \"choice\" (got {})",
                other.unwrap_or("<missing>")
            ))
        }
    };
    // The vote text is RETRIEVAL TEXT, and retrieval text must be payload
    // data, never instruction prose (#1000). An instruction is a question
    // ABOUT the payload; its vocabulary retrieves neighbours on its own —
    // measured 0.9867 accuracy with an empty instruction against 0.6700 with
    // a criteria-rich one, the same 300 messages. So a question votes on
    // exactly what it POINTS AT:
    //
    // - every `` `path` `` reference its instructions name, resolved first
    //   against the instructions' own string fields (the data-field pattern:
    //   `{"question": "Judge `document` …", "document": text}`, which
    //   xerj-rerank's stage and the provider's docs use) and then against
    //   the state (the jev-reranker pattern: documents in state, referenced
    //   by dotted path). The state does NOT ride along here — a question
    //   that named its payload gets its payload, not its payload plus
    //   whatever else the state holds;
    // - a reference that resolves to STRUCTURE, not text, contributes no
    //   vote text. `jev-reranker` ≥ 0.1.2's `rerank_relevance()` preset
    //   ships its rubric object in state and names it in backticks; the
    //   rubric is judge rules, and embedding its string leaves is exactly
    //   the #1000 defect (criteria-rich prose, the measured 0.6700 class).
    //   Structure is acknowledged and skipped — text is text, rules are
    //   rules;
    // - a question that references nothing votes on the state alone: a
    //   string state whole, an object state by ALL its string leaves (#1001:
    //   only `query` used to be read, so `{"message": …}` answered from the
    //   instruction alone, at the spam base rate);
    // - a question whose references resolve NOWHERE is refused naming them —
    //   a confident vote on text the question never saw is the silent-fake
    //   defect class, in another coat;
    // - a vote text that is still empty is refused naming the question.
    //
    // `criteria` descriptions are never embedded: no benchmark measures
    // criteria text, and it is the richest vocabulary on the wire.
    let instructions = obj.get("instructions");
    let mut paths: Vec<String> = Vec::new();
    if let Some(instr) = instructions {
        collect_backtick_paths(instr, &mut paths);
    }
    let mut vote_text = String::new();
    if paths.is_empty() {
        join_strings(state_val, &mut vote_text);
    } else {
        let mut unresolved: Vec<String> = Vec::new();
        for path in &paths {
            match resolve_reference(instructions, state_val, path) {
                Some(Value::String(text)) => {
                    if !vote_text.is_empty() {
                        vote_text.push(' ');
                    }
                    vote_text.push_str(text);
                }
                // Resolves, but to structure — an object, number, boolean.
                // Rules and figures carry no retrieval vocabulary: skipping
                // them is the difference between this and #1000, where the
                // prose joined the vote and wording moved accuracy 32 points.
                Some(_) => {}
                None => unresolved.push(path.clone()),
            }
        }
        if !unresolved.is_empty() {
            let listed = unresolved
                .iter()
                .map(|p| format!("`{p}`"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "question `{id}`: backtick reference{} {listed} resolve{} to nothing in \
                 this request — the vote would run on text the question never named; \
                 reference existing instructions fields or state paths, or drop the \
                 backticks",
                if unresolved.len() == 1 { "" } else { "s" },
                if unresolved.len() == 1 { "s" } else { "" },
            ));
        }
    }
    let vote_text = clip(vote_text.trim(), MAX_VOTE_TEXT_CHARS);
    if vote_text.is_empty() {
        return Err(format!(
            "question `{id}`: no text to vote on — the payload resolved to nothing; put \
             text in `state` (a string, or an object with string fields) or reference it \
             in backticks from the instructions, remembering that only string-valued \
             references carry text"
        ));
    }
    Ok(Question { vote_text, kind })
}

/// One weighted-neighbour record: (weight, label, hit). Unlabelled hits are
/// dropped here — they carry no vote. An index that cannot be reached at all
/// is [`HistorySupport::Unusable`] carrying the cause, so the decide ladder
/// can offer the question to the next tier instead of pre-baking the error.
async fn vote_neighbours(
    state: &AppState,
    cfg: &DecisionsConfig,
    text: &str,
) -> Result<Vec<(f64, String, xerj_query::executor::Hit)>, HistorySupport> {
    let idx = match state.engine.get_index(&cfg.index) {
        Ok(i) => i,
        Err(e) => return Err(HistorySupport::Unusable(e.to_string())),
    };
    // The text field is configuration, not a literal — build the match by
    // hand so the field name is data, not syntax.
    let mut match_on = serde_json::Map::new();
    match_on.insert(cfg.text_field.clone(), json!(text));
    let query_body = json!({
        "query": { "match": Value::Object(match_on) },
        "size": cfg.k,
        "_source": true,
    });
    let search_req = match parse_request(&query_body) {
        Ok(r) => r,
        Err(e) => {
            return Err(HistorySupport::Unusable(format!(
                "internal query would not parse: {e}"
            )))
        }
    };
    let result = match idx.search(&search_req).await {
        Ok(r) => r,
        Err(e) => return Err(HistorySupport::Unusable(e.to_string())),
    };
    let mut out = Vec::with_capacity(result.hits.len());
    for (i, hit) in result.hits.into_iter().enumerate() {
        if let Some(label) = hit.source.get(&cfg.label_field).and_then(Value::as_str) {
            // The flywheel's one refinement to the raw vote (#1061): a
            // document the operator marked `human: true` — a correction,
            // indexed through the ordinary write path — weighs
            // `decisions.human_weight`× its reciprocal rank. Everything else
            // keeps the 1/rank arithmetic the published measurements used.
            let w = neighbour_weight(i, is_human_correction(&hit.source), cfg.human_weight);
            out.push((w, label.to_string(), hit));
        }
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// Text helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Join every string leaf of a polymorphic `instructions` value.
fn join_strings(v: &Value, out: &mut String) {
    match v {
        Value::String(s) => {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(s);
        }
        Value::Array(a) => a.iter().for_each(|x| join_strings(x, out)),
        Value::Object(m) => m.values().for_each(|x| join_strings(x, out)),
        _ => {}
    }
}

/// Collect every `` `path` `` a question's instructions name, in order of
/// first appearance, deduplicated. The prose between backticks is ignored —
/// it is the question, not the payload (#1000). An unpaired backtick names
/// nothing.
fn collect_backtick_paths(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => {
            let mut rest = s.as_str();
            while let Some(start) = rest.find('`') {
                let after = &rest[start + 1..];
                match after.find('`') {
                    Some(end) => {
                        let path = &after[..end];
                        if !path.is_empty() && !out.iter().any(|p| p == path) {
                            out.push(path.to_string());
                        }
                        rest = &after[end + 1..];
                    }
                    None => break,
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_backtick_paths(x, out)),
        Value::Object(m) => m.values().for_each(|x| collect_backtick_paths(x, out)),
        _ => {}
    }
}

/// Resolve one backtick reference to the value it names. The instructions'
/// own string fields win over same-named state paths — the data-field
/// pattern puts the payload next to the prose that names it — then the
/// state is walked by dotted path. `None` means the reference names
/// nothing in this request; a caller decides what a non-string resolution
/// means (here: structure, not vote text).
fn resolve_reference<'a>(
    instructions: Option<&'a Value>,
    state: &'a Value,
    path: &str,
) -> Option<&'a Value> {
    instructions
        .and_then(|i| walk(i, path))
        .or_else(|| walk(state, path))
}

fn walk<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s[..cut].to_string()
}

fn round6(x: f64) -> f64 {
    (x * 1_000_000.0).round() / 1_000_000.0
}

// ─────────────────────────────────────────────────────────────────────────────
// Error bodies — every one names its cause and never guesses an answer
// ─────────────────────────────────────────────────────────────────────────────

fn error_body(status: u16, kind: &str, reason: String) -> axum::response::Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY),
        Json(json!({ "error": { "type": kind, "reason": reason } })),
    )
        .into_response()
}

fn unprocessable(reason: &str) -> axum::response::Response {
    error_body(422, "illegal_argument", reason.to_string())
}

fn not_configured() -> axum::response::Response {
    error_body(
        503,
        "not_configured",
        "no decisions index is configured: set [decisions] index to an index of labelled \
         examples (one document per example, with the label and text fields this node's \
         decisions settings name), then POST /v1/systemone again"
            .to_string(),
    )
}

fn index_error(index: &str, cause: &str) -> axum::response::Response {
    error_body(
        422,
        "history_index_error",
        format!("decisions index `{index}` could not be searched: {cause}"),
    )
}

/// The local tier could not answer — the model directory is wrong or the
/// weights will not load. 503, naming the directory and the env var, because
/// the tier was explicitly armed: every request fails loudly until the
/// operator fixes it, never with a fabricated probability and never by
/// silently falling back to the errors the operator switched away from.
#[cfg(feature = "decide-local")]
fn local_unavailable(err: &anyhow::Error, model_dir: &Path) -> axum::response::Response {
    error_body(
        503,
        "local_decide_unavailable",
        format!(
            "the local decision tier could not answer: {err:#}. The model directory is {} \
             (set by XERJ_DECIDE_MODEL_DIR) and must hold config.json, tokenizer.json and \
             model.safetensors",
            model_dir.display()
        ),
    )
}

fn no_support(ids: &[String], cfg: &DecisionsConfig) -> axum::response::Response {
    error_body(
        422,
        "no_support",
        format!(
            "no labelled neighbour in `{}` for question{} {} (k={}); zero support is an \
             error, not a fabricated probability — add labelled examples to the history \
             index or use POST /_decide to inspect what is there",
            cfg.index,
            if ids.len() == 1 { "" } else { "s" },
            ids.iter()
                .map(|i| format!("`{i}`"))
                .collect::<Vec<_>>()
                .join(", "),
            cfg.k,
        ),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests — the ladder's order is the contract, so it is unit-tested directly
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn no_support_classes() -> Vec<HistorySupport> {
        vec![
            HistorySupport::NotConfigured,
            HistorySupport::Unusable("index gone".into()),
            HistorySupport::NoLabelledNeighbour,
        ]
    }

    /// Tier 1 wins wherever it has confident support (`share >= escalate_below`).
    /// When the local head is armed and the history winner's share falls below
    /// `escalate_below`, the question escalates to tier 2 (`Local`) so the
    /// flywheel does not freeze once BM25 starts matching split neighbours
    /// (#1094). Without the local tier armed, low-share history votes stay on
    /// `History` and answer or abstain as before. Every no-support class falls
    /// to the local head when armed, and to nothing (the documented errors)
    /// when not. The hosted tier is reserved: it never resolves.
    #[test]
    fn tier_order_is_history_then_local_then_the_documented_errors() {
        for support in no_support_classes() {
            assert_eq!(
                resolve_tier(&support, 0.8, false),
                None,
                "{support:?}: no local tier → the documented error stands"
            );
        }
        assert_eq!(
            resolve_tier(&HistorySupport::Supported(1.0), 0.8, false),
            Some(DecideTier::History)
        );
        // Confident history (`share >= escalate_below`) wins even with the local tier armed.
        assert_eq!(
            resolve_tier(&HistorySupport::Supported(1.0), 0.8, true),
            Some(DecideTier::History)
        );
        assert_eq!(
            resolve_tier(&HistorySupport::Supported(0.8), 0.8, true),
            Some(DecideTier::History),
            "at the floor (`share == escalate_below`) history holds"
        );
        // Split / low-share history (`share < escalate_below`) escalates to local
        // when armed, and stays on history when not (#1094).
        assert_eq!(
            resolve_tier(&HistorySupport::Supported(2.0 / 3.0), 0.8, true),
            Some(DecideTier::Local),
            "a 2-to-1 split (0.667 < 0.8) escalates to the local head when armed"
        );
        assert_eq!(
            resolve_tier(&HistorySupport::Supported(2.0 / 3.0), 0.8, false),
            Some(DecideTier::History),
            "without a local tier, low-share history stays on history"
        );
        assert_eq!(
            resolve_tier(&HistorySupport::Supported(0.5), 0.0, true),
            Some(DecideTier::History),
            "escalate_below = 0.0 disables confidence escalation"
        );
        for support in no_support_classes() {
            assert_eq!(
                resolve_tier(&support, 0.8, true),
                Some(DecideTier::Local),
                "{support:?}: the local head answers what history cannot"
            );
        }
    }

    #[test]
    fn resolve_defaults_to_history_only_without_a_warning() {
        for mode in ["", "  ", "history", "History"] {
            let (settings, warning) = DecideSettings::resolve(mode, None);
            assert!(
                !settings.local_available(),
                "{mode:?}: default node is history-only"
            );
            assert!(warning.is_none(), "{mode:?}: {warning:?}");
        }
    }

    #[test]
    fn an_unknown_mode_stays_on_history_and_names_the_valid_ones() {
        let (settings, warning) = DecideSettings::resolve("hosted", None);
        assert!(!settings.local_available());
        let warning = warning.expect("warning");
        assert!(warning.contains("hosted"), "{warning}");
        assert!(warning.contains("local"), "{warning}");
        assert!(warning.contains("history"), "{warning}");
    }

    #[cfg(feature = "decide-local")]
    #[test]
    fn local_mode_arms_only_with_a_model_dir() {
        // The mode without a directory: history stays, the warning names the
        // env var and the files it expects.
        let (settings, warning) = DecideSettings::resolve("local", None);
        assert!(!settings.local_available(), "no dir → not armed");
        let warning = warning.expect("warning");
        assert!(warning.contains("XERJ_DECIDE_MODEL_DIR"), "{warning}");
        assert!(warning.contains("model.safetensors"), "{warning}");

        // A real directory: armed, silent.
        let dir = tempfile::tempdir().expect("tempdir");
        let (settings, warning) = DecideSettings::resolve("local", dir.path().to_str());
        assert!(settings.local_available(), "dir present → armed");
        assert!(warning.is_none(), "{warning:?}");

        // A missing directory: still armed — the operator asked for the tier,
        // so its first request names the path rather than the node quietly
        // pretending to be history-only.
        let (settings, warning) =
            DecideSettings::resolve("local", Some("/nonexistent/xerj-decide"));
        assert!(
            settings.local_available(),
            "armed even when the dir is absent"
        );
        let warning = warning.expect("warning");
        assert!(warning.contains("/nonexistent/xerj-decide"), "{warning}");
    }

    #[cfg(not(feature = "decide-local"))]
    #[test]
    fn local_mode_without_the_feature_warns_to_rebuild() {
        let (settings, warning) = DecideSettings::resolve("local", Some("/any/dir"));
        assert!(!settings.local_available());
        let warning = warning.expect("warning");
        assert!(warning.contains("decide-local"), "{warning}");
    }

    #[test]
    fn labels_for_gives_a_noul_two_sides_and_a_choice_its_options() {
        let cfg = DecisionsConfig::default();
        let noul = Question {
            vote_text: "payload".into(),
            kind: Kind::Noul,
        };
        assert_eq!(
            labels_for(&noul, &cfg),
            vec!["true".to_string(), "not true".to_string()],
            "a noul is a comparison, never one unopposed score"
        );
        let choice = Question {
            vote_text: "payload".into(),
            kind: Kind::Choice {
                options: vec!["billing".into(), "tech".into()],
            },
        };
        assert_eq!(labels_for(&choice, &cfg), vec!["billing", "tech"]);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // The flywheel (#1061)
    // ─────────────────────────────────────────────────────────────────────────

    /// The issue's arithmetic, exactly: at the default 2× a correction's
    /// vote weight doubles, and doubling flips a concrete two-label vote the
    /// 1/rank arithmetic settles the other way. Ranks are BM25 ranks; the
    /// label sums are what `/_decide` and `/v1/systemone` aggregate.
    #[test]
    fn two_x_human_weight_changes_the_vote() {
        // Rank 0 is a plain `billing` neighbour; ranks 1 and 2 are human
        // corrections saying `tech`.
        let billing = neighbour_weight(0, false, 2.0);
        let tech_at_1 = neighbour_weight(1, true, 2.0);
        let tech_at_2 = neighbour_weight(2, true, 2.0);
        // Sanity: plain weights are the reciprocal-rank arithmetic the
        // published measurements used, untouched.
        assert_eq!(neighbour_weight(0, false, 9.9), 1.0);
        assert_eq!(neighbour_weight(1, false, 9.9), 0.5);
        // With the boost disabled (weight 1.0) billing wins: 1.0 against
        // 0.5 + 1/3.
        let tech_unboosted = neighbour_weight(1, true, 1.0) + neighbour_weight(2, true, 1.0);
        assert!(
            billing > tech_unboosted,
            "at weight 1.0 corrections are ordinary neighbours: {billing} vs {tech_unboosted}"
        );
        // At the default 2× the corrections win: 0.5·2 + (1/3)·2 > 1.0.
        let tech_boosted = tech_at_1 + tech_at_2;
        assert!(
            tech_boosted > billing,
            "at weight 2.0 corrections outrank the vote they correct: {tech_boosted} vs {billing}"
        );
        // And the default IS 2.0 — the issue's ≥ 2x floor.
        assert_eq!(DecisionsConfig::default().human_weight, 2.0);
    }

    /// The weight is configuration: `[decisions] human_weight` parses, and a
    /// value that would erase or invert a correction's vote is refused at
    /// config load, not discovered in a wrong answer.
    #[test]
    fn human_weight_is_configurable_and_range_checked() {
        let cfg = xerj_common::config::Config::from_toml_str(
            "[decisions]\nindex = \"judgements\"\nhuman_weight = 3.5\n",
        )
        .expect("toml");
        assert_eq!(cfg.decisions.human_weight, 3.5);
        assert_eq!(
            cfg.decisions.index, "judgements",
            "the rest of the section parses unchanged"
        );
        for bad in ["0.0", "-2.0", "nan"] {
            let err = xerj_common::config::Config::from_toml_str(&format!(
                "[decisions]\nhuman_weight = {bad}\n"
            ))
            .expect_err(bad);
            assert!(err.to_string().contains("human_weight"), "{bad}: {err}");
        }
    }

    /// `[decisions] escalate_below` defaults to 0.8 (matching the gate's
    /// confidence bar so split BM25 votes escalate to tier 2 and unfreeze the
    /// flywheel) and validates `0.0..=1.0` (#1094).
    #[test]
    fn escalate_below_is_configurable_and_range_checked() {
        assert_eq!(DecisionsConfig::default().escalate_below, 0.8);
        let cfg = xerj_common::config::Config::from_toml_str(
            "[decisions]\nindex = \"judgements\"\nescalate_below = 0.65\n",
        )
        .expect("toml");
        assert_eq!(cfg.decisions.escalate_below, 0.65);
        let disabled =
            xerj_common::config::Config::from_toml_str("[decisions]\nescalate_below = 0.0\n")
                .expect("0.0 disables confidence escalation");
        assert_eq!(disabled.decisions.escalate_below, 0.0);
        for bad in ["-0.1", "1.1", "nan"] {
            let err = xerj_common::config::Config::from_toml_str(&format!(
                "[decisions]\nescalate_below = {bad}\n"
            ))
            .expect_err(bad);
            assert!(err.to_string().contains("escalate_below"), "{bad}: {err}");
        }
    }

    /// The cached answer's document is the flywheel's contract: the
    /// configured text/label fields (so it votes like any seeded example) and
    /// the fixed p / source / ts the issue names. Nothing else — a cached
    /// answer is NOT marked human, or it would outrank itself forever.
    #[test]
    fn write_back_id_is_one_per_decided_text() {
        let a = write_back_id("refund money back check");
        assert_eq!(a, write_back_id("refund money back check"));
        assert_ne!(a, write_back_id("refund money back check!"));
        assert!(
            a.starts_with("decide-") && a.len() == "decide-".len() + 32,
            "{a}"
        );
    }

    #[test]
    fn write_back_doc_carries_the_flywheel_fields() {
        let cfg = DecisionsConfig {
            text_field: "message".into(),
            label_field: "intent".into(),
            ..DecisionsConfig::default()
        };
        let doc = write_back_doc(
            &CachedAnswer {
                text: "refund my subscription".into(),
                label: "refund".into(),
                p: 0.8712345678,
                source: DecideTier::Local.as_str(),
            },
            &cfg,
            "2026-09-30T00:00:00.123Z",
        );
        assert_eq!(doc["message"], "refund my subscription");
        assert_eq!(doc["intent"], "refund");
        assert_eq!(doc["p"], 0.871235, "probability rounded like the wire");
        assert_eq!(doc["source"], "local");
        assert_eq!(doc["ts"], "2026-09-30T00:00:00.123Z");
        assert_eq!(
            doc.as_object().map(|o| o.len()),
            Some(5),
            "exactly the five fields: {doc}"
        );
    }

    /// Only a JSON boolean `true` earns the correction weight — "true", 1,
    /// and a missing field are ordinary history.
    #[test]
    fn only_a_boolean_true_marks_a_human_correction() {
        assert!(is_human_correction(&json!({"human": true})));
        assert!(!is_human_correction(&json!({"human": "true"})));
        assert!(!is_human_correction(&json!({"human": 1})));
        assert!(!is_human_correction(&json!({"human": false})));
        assert!(!is_human_correction(&json!({"text": "no human field"})));
        assert!(!is_human_correction(&Value::Null));
    }

    /// Every tier's wire name is stable — it is `tier`, `source`, and the
    /// cached document's `source` all at once.
    #[test]
    fn tier_names_are_the_source_names() {
        assert_eq!(DecideTier::History.as_str(), "history");
        assert_eq!(DecideTier::Local.as_str(), "local");
        assert_eq!(DecideTier::Hosted.as_str(), "hosted");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Calibration (#1063)
    // ─────────────────────────────────────────────────────────────────────────

    /// The fit's data contract: a labelled outcome document is a pair, the
    /// positive label's probability against the outcome; a flywheel answer
    /// (`source`) is NOT, because its label is the model's own prediction —
    /// calibrating on predictions would learn the model is right by
    /// construction.
    #[test]
    fn calibration_pairs_come_from_outcomes_not_predictions() {
        let cfg = DecisionsConfig::default(); // label_field "label", positive "true"
                                              // An outcome the system got wrong: it said 0.9, the truth is "false".
                                              // The positive-label probability is 1 − 0.9, the outcome 0.
        let doc = json!({"text": "…", "label": "false", "p": 0.9});
        let pair = pair_from_doc(&doc, &cfg).expect("outcome pair");
        assert!(
            (pair.p - 0.1).abs() < 1e-12,
            "p_pos = 1 − p when label ≠ positive"
        );
        assert_eq!(pair.y, 0.0);
        // An outcome it got right: p of the positive label, outcome 1.
        let doc = json!({"text": "…", "label": "true", "p": 0.8});
        let pair = pair_from_doc(&doc, &cfg).expect("outcome pair");
        assert!((pair.p - 0.8).abs() < 1e-12);
        assert_eq!(pair.y, 1.0);
        // A human correction carrying the p it corrects: still an outcome,
        // and the best kind — truth plus the number that was wrong.
        let doc = json!({"text": "…", "label": "true", "p": 0.3, "human": true});
        assert!(pair_from_doc(&doc, &cfg).is_some());
        // A flywheel answer: a prediction, never a pair.
        let doc = json!({"text": "…", "label": "true", "p": 0.8, "source": "local", "ts": "…"});
        assert!(
            pair_from_doc(&doc, &cfg).is_none(),
            "a `source` document is the model's own answer"
        );
        // No p, no pair — a seeded example has a truth but no raw
        // probability to calibrate.
        assert!(pair_from_doc(&json!({"text": "…", "label": "true"}), &cfg).is_none());
        // No label, no pair.
        assert!(pair_from_doc(&json!({"text": "…", "p": 0.8}), &cfg).is_none());
        // A p that is not a probability is not calibration data.
        assert!(pair_from_doc(&json!({"label": "true", "p": 1.4}), &cfg).is_none());
        assert!(pair_from_doc(&json!({"label": "true", "p": -0.1}), &cfg).is_none());
        assert!(pair_from_doc(&json!({"label": "true"}), &cfg).is_none());
    }

    /// The three states on the wire: off ships no p_cal at all, unfit ships
    /// null with the reason published in the block, fitted ships the fit's
    /// value — and never a copy of p_raw pretending to be calibrated.
    #[test]
    fn p_cal_is_absent_null_or_calibrated_never_a_copy() {
        // Off: nothing.
        assert!(p_cal_value(&CalibrationState::Off, 0.7).is_none());
        assert!(calibration_block(&CalibrationState::Off).is_none());
        // Unfit: null, with the reason riding in the block.
        let unfit = CalibrationState::Unfit("only 3 labelled pairs".into());
        assert_eq!(p_cal_value(&unfit, 0.7), Some(Value::Null));
        let block = calibration_block(&unfit).expect("block");
        assert_eq!(block["reason"], "only 3 labelled pairs");
        // Fitted: the fit's value and its ECE, not p_raw echoed back. The
        // identity fit (one knot per distinct p, already monotone) would echo
        // — so use a fit that learned the opposite of the raw number.
        let report = Arc::new(
            xerj_common::calibration::fit_calibration(
                xerj_common::calibration::CalibrationMethod::Isotonic,
                &[
                    CalibrationPair { p: 0.9, y: 0.0 },
                    CalibrationPair { p: 0.9, y: 0.0 },
                    CalibrationPair { p: 0.9, y: 0.0 },
                    CalibrationPair { p: 0.9, y: 0.0 },
                    CalibrationPair { p: 0.9, y: 0.0 },
                    CalibrationPair { p: 0.2, y: 0.0 },
                    CalibrationPair { p: 0.2, y: 0.0 },
                    CalibrationPair { p: 0.2, y: 0.0 },
                    CalibrationPair { p: 0.2, y: 0.0 },
                    CalibrationPair { p: 0.2, y: 0.0 },
                ],
            )
            .expect("fit"),
        );
        let fitted = CalibrationState::Fitted(report);
        let p_cal = p_cal_value(&fitted, 0.9).expect("a value");
        let p_cal = p_cal.as_f64().expect("a number");
        assert!(
            (p_cal - 0.9).abs() > 1e-9,
            "a fit that learned 0.9 means 0 must not echo 0.9 back (got {p_cal})"
        );
        assert!(
            (p_cal - 0.0).abs() < 1e-9,
            "five negatives at 0.9 → p_cal 0"
        );
        let block = calibration_block(&fitted).expect("block");
        assert_eq!(block["method"], "isotonic");
        assert_eq!(block["scope"], "noul");
        assert!(
            block["ece"]["calibrated"].is_number(),
            "the ECE rides beside: {block}"
        );
        assert!(block["ece"]["raw"].is_number());
        assert_eq!(block["fitted_on"], 8);
        assert_eq!(block["held_out"], 2);
    }

    /// The unfit reason names what a fit needs and how to record it — an
    /// operator who sees `p_cal: null` can act on the string beside it.
    #[test]
    fn the_unfit_reason_names_the_recording_path() {
        let reason = unfit_reason(&DecisionsConfig::default(), 3);
        assert!(reason.contains("3 labelled calibration pairs"), "{reason}");
        assert!(
            reason.contains("10"),
            "the minimum is in the message: {reason}"
        );
        assert!(
            reason.contains("source"),
            "the flywheel exclusion is explained: {reason}"
        );
    }
}
