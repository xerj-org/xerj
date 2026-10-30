//! The local `judge` of `_search` — second-stage relevance scoring that never
//! leaves the node and never bills a token (issue #1060: "the zero-token
//! rerank").
//!
//! [`crate::Provider`] is the operator-pays arm: a hosted cross-encoder, one
//! outbound call per search, text off the node. This module is the other
//! option — a scorer that runs in process over the hits the response was
//! already about to return, so a caller can ask "how likely is THIS hit to
//! be relevant, as a number I can threshold" without paying for it or
//! exporting text. Each judged hit carries the probability as `_p_relevant`
//! (the hosted stage REPLACES `_score` with its probability; the judge
//! leaves the engine's `_score` alone and adds a field, so a client that
//! sorts by `_score` keeps seeing the engine's own number), and hits below
//! `judge.min_p` are dropped and counted in the response's `judged` block.
//!
//! # Honest scope: what the lexical scorer is, and is not
//!
//! The always-compiled arm, [`LocalJudge::Lexical`], is a DETERMINISTIC
//! LEXICAL scorer — window-local BM25 saturation over the judged page, IDF
//! computed from the page itself, mapped into `0.0..=1.0` by the maximum
//! attainable saturation (the formula and its provenance are on
//! [`lexical_scores`]). It is not a cross-encoder and has no semantic
//! signal: it re-reads the same words the first stage read, with page-local
//! statistics and an absolute scale the first stage does not offer. Whether
//! that beats the engine's own order is an empirical question with a written
//! bar — the BEIR gate in issue #1060 ("loses → does not ship": 0.699
//! SciFact / 0.345 NFCorpus beyond run spread, FiQA ≥ 0.30, harness at
//! `benchmarks/beir-hybrid`) — and NO quality claim is made here. What IS
//! claimed, and unit-tested, is: deterministic, bounded, and microseconds
//! for a top-30 page.
//!
//! [`LocalJudge::Model`] (cargo feature `judge-local`) is the semantic arm:
//! the node's local decision head — the candle NLI pair scorer landed in
//! `xerj-ai/src/decide.rs` for #1057 — scoring premise-=-document against a
//! relevance hypothesis and its negation. It is feature-gated exactly like
//! `decide-local` because it IS the same loader and the same model
//! directory: a node that armed `XERJ_DECIDE_MODE=local` has a judge model,
//! and a node without one uses the lexical arm and SAYS SO in the response's
//! `judged.scorer`. Model quality with the real trained checkpoint is
//! likewise unmeasured here: the gate binds at release, against the real
//! model.
//!
//! # Failure policy
//!
//! The lexical arm cannot fail. The model arm surfaces its failures
//! ([`RerankError::LocalModel`] → HTTP 503): a caller who asked for judged
//! hits and silently got unjudged ones has been misled, which is the
//! silent-fake class this crate refuses everywhere (see the crate header).
//! There is deliberately no degrade-to-lexical on a model fault —
//! substituting a different scorer than the one the response claims is the
//! same lie in the other direction.

use std::collections::HashMap;

use serde_json::Value;

use crate::{Candidate, RerankError, Scored};

/// `judge.min_p` is a probability. Both ends are valid: `0.0` judges and
/// reorders but drops nothing, `1.0` keeps only what the scorer scored at
/// certainty.
pub const MIN_P_RANGE: (f64, f64) = (0.0, 1.0);

/// Ceiling on `judge.query`. The same string, under the same ceiling, as
/// [`crate::MAX_QUERY_CHARS`]: it is not sent anywhere (the judge is
// in-process), but the lexical scorer tokenizes it on every judged search
/// and the model arm folds it into every pair it scores, so a bound the
/// caller cannot choose still belongs to the server.
pub const MAX_QUERY_CHARS: usize = crate::MAX_QUERY_CHARS;

// Only the `judge-local` arm names a model directory.
#[cfg(feature = "judge-local")]
use std::path::PathBuf;

/// Parsed `judge` block from a search body.
///
/// `local` is not a field: `true` is the only mode, so [`Self::from_json`]
/// accepts it (explicitly or by omission) and refuses anything else by name.
#[derive(Debug, Clone)]
pub struct JudgeConfig {
    /// The question to judge documents against. `None` defers to the search
    /// query when it is a shape a single question can be read from — the
    /// same inference rule the `rerank` stage uses.
    pub query: Option<String>,
    /// Drop hits scoring below this probability. `None` keeps all and only
    /// reorders.
    pub min_p: Option<f64>,
}

impl JudgeConfig {
    /// Parse the `judge` object from a search body.
    ///
    /// Unknown keys are rejected rather than ignored, for the same reason
    /// [`crate::RerankConfig::from_json`] rejects them: a caller who
    /// misspells `min_p` and silently gets unpruned results has been
    /// misled.
    pub fn from_json(v: &Value) -> Result<Self, RerankError> {
        let obj = v
            .as_object()
            .ok_or_else(|| RerankError::Config("`judge` must be an object".into()))?;

        const KNOWN: &[&str] = &["local", "min_p", "query"];
        for k in obj.keys() {
            if !KNOWN.contains(&k.as_str()) {
                // Named, never echoed whole — the same clip the `rerank`
                // parser applies to an unknown key.
                let shown = crate::clip(k, crate::MAX_ECHOED_NAME_CHARS);
                let ellipsis = if shown.len() < k.len() { "…" } else { "" };
                return Err(RerankError::Config(format!(
                    "unknown `judge` field `{shown}{ellipsis}`; supported: {}",
                    KNOWN.join(", ")
                )));
            }
        }

        if let Some(l) = obj.get("local") {
            match l {
                Value::Bool(true) => {}
                Value::Bool(false) => {
                    return Err(RerankError::Config(
                        "`judge.local` cannot be false: the local judge is the only judge. \
                         Remove the `judge` block to search unjudged"
                            .into(),
                    ));
                }
                _ => {
                    return Err(RerankError::Config(
                        "`judge.local` must be true (the local judge is the only judge)".into(),
                    ));
                }
            }
        }

        let min_p = match obj.get("min_p") {
            None => None,
            Some(t) => {
                let t = t
                    .as_f64()
                    .ok_or_else(|| RerankError::Config("`judge.min_p` must be a number".into()))?;
                if !(MIN_P_RANGE.0..=MIN_P_RANGE.1).contains(&t) {
                    return Err(RerankError::Config(
                        "`judge.min_p` must be between 0 and 1 — it is a probability".into(),
                    ));
                }
                Some(t)
            }
        };

        let query = match obj.get("query") {
            None => None,
            Some(q) => {
                let q = q
                    .as_str()
                    .ok_or_else(|| RerankError::Config("`judge.query` must be a string".into()))?;
                if crate::longer_than(q, MAX_QUERY_CHARS) {
                    return Err(RerankError::Config(format!(
                        "`judge.query` must be at most {MAX_QUERY_CHARS} characters (this one \
                         is {} bytes): the judge reads it against every hit in the page",
                        q.len()
                    )));
                }
                if q.trim().is_empty() {
                    return Err(RerankError::Config(
                        "`judge.query` must not be empty".into(),
                    ));
                }
                Some(q.to_string())
            }
        };

        Ok(Self { query, min_p })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The lexical scorer — the always-compiled arm
// ─────────────────────────────────────────────────────────────────────────────

/// BM25 saturation parameters: the same values the engine's own first stage
/// uses (`xerj-fts/src/bm25.rs`, `DEFAULT_K1` = 1.2, `b` = 0.75 — BM25's
/// standard parameters, inherited from Lucene). Reused rather than re-chosen
/// so the judge's saturation curve behaves like the ranking the caller
/// already knows.
const K1: f64 = 1.2;
const B: f64 = 0.75;

/// Scripts for which an uninterrupted alphanumeric run is not a word (#2580).
/// Include Han's supplementary ideographic planes; all slicing below follows
/// char_indices, including four-byte characters. This is character n-gramming,
/// not dictionary segmentation, normalization or semantic understanding.
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF |
        0x20000..=0x3FFFF | 0x3040..=0x309F | 0x30A0..=0x30FF |
        0xAC00..=0xD7AF)
}

/// Preserve lowercase non-CJK alphanumeric runs. CJK runs emit each character
/// and each adjacent pair, so a single-character query can match inside a run
/// and the pair `雷同` has evidence beyond the separate characters `同雷`.
/// Separators and non-CJK runs break pairs; title and body are tokenized apart.
///
/// Reference approach (MIT, no implementation copied): Tantivy's
/// src/tokenizer/ngram_tokenizer.rs:115-119 admits all inner n-grams, :154-164
/// emits codepoint-bounded slices, :253-280 defines the UTF-8 frontiers.
/// https://github.com/quickwit-oss/tantivy/blob/main/src/tokenizer/ngram_tokenizer.rs
/// Unlike applying that tokenizer to the whole string, restrict grams to CJK
/// here so the established Latin/number token and scoring contract survives.
fn tokens(s: &str) -> impl Iterator<Item = String> + '_ {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .flat_map(|word| {
            let mut chars = word.char_indices().peekable();
            let mut pair = None;
            std::iter::from_fn(move || {
                if let Some(pair) = pair.take() {
                    return Some(pair);
                }
                let (start, c) = chars.next()?;
                let mut end = start + c.len_utf8();
                if is_cjk(c) {
                    if let Some(&(next_start, next)) = chars.peek() {
                        if is_cjk(next) {
                            pair = Some(word[start..next_start + next.len_utf8()].to_string());
                        }
                    }
                    Some(c.to_string())
                } else {
                    while let Some(&(next_start, next)) = chars.peek() {
                        if is_cjk(next) {
                            break;
                        }
                        chars.next();
                        end = next_start + next.len_utf8();
                    }
                    Some(word[start..end].to_lowercase())
                }
            })
        })
}

/// The deterministic lexical judge.
///
/// For each candidate it computes the share of the query's term weight the
/// document saturates, where saturation is the BM25 TF-normalisation
/// `tf·(k1+1) / (tf + k1·(1 − b + b·dl/avgdl))` and the weight is
/// window-local IDF `ln(1 + (N − df + 0.5)/(df + 0.5))` computed over the
/// judged page itself — the only corpus this scorer has, which is what
/// "self-judged" means in the issue. The result is divided by the maximum
/// attainable score (every query term fully saturated: `Σidf·(k1+1)`), which
/// maps it into `0.0..=1.0` with a fixed meaning a caller can threshold:
///
/// * a document containing none of the query's words scores exactly `0.0`;
/// * a short document saturated with all of them approaches `1.0`;
/// * a term every document shares contributes little (common in the page),
///   a term one document holds contributes its full IDF weight.
///
/// Deterministic by construction: a pure function of `(query, candidates)`
/// with no clock, no randomness and no model. The same inputs always judge
/// to the same probabilities, which is what makes `min_p` a stable
/// threshold. Returns one [`Scored`] per candidate, in input order, with the
/// caller's ordinals untouched.
pub fn lexical_scores(query: &str, candidates: &[Candidate]) -> Vec<Scored> {
    // Query terms, unique, in first-seen order — the order fixes the
    // summation order, so f64 rounding is reproducible too.
    let mut q_terms: Vec<String> = Vec::new();
    {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for t in tokens(query) {
            if seen.insert(t.clone()) {
                q_terms.push(t);
            }
        }
    }
    if q_terms.is_empty() || candidates.is_empty() {
        // No query words to judge by (or nothing to judge): every candidate
        // scores 0.0, a score a `min_p` above 0 will drop and no `min_p`
        // will reorder. Inventing spread here would be ranking by nothing.
        return candidates
            .iter()
            .map(|c| Scored {
                ordinal: c.ordinal,
                score: 0.0,
            })
            .collect();
    }

    // Count tokens once, retaining frequencies only for query terms. CJK
    // expansion must not turn IDF into a scan of every document token for
    // every query gram. Length still counts all tokens, exactly as before.
    let qset: std::collections::HashSet<&str> = q_terms.iter().map(String::as_str).collect();
    let docs: Vec<(usize, HashMap<&str, u64>)> = candidates
        .iter()
        .map(|c| {
            let mut len = 0;
            let mut tf = HashMap::new();
            for token in c
                .title
                .iter()
                .flat_map(|t| tokens(t))
                .chain(tokens(&c.text))
            {
                len += 1;
                if let Some(&q) = qset.get(token.as_str()) {
                    *tf.entry(q).or_insert(0) += 1;
                }
            }
            (len, tf)
        })
        .collect();

    let n = docs.len() as f64;
    let avgdl = {
        let total: usize = docs.iter().map(|(len, _)| len).sum();
        (total as f64 / n).max(1.0)
    };

    // Window-local document frequency and IDF per query term, in q_terms
    // order. df ≤ N always, so the ln argument is ≥ 1 and every weight is
    // positive — a term no document holds simply saturates nowhere.
    let idf: Vec<f64> = q_terms
        .iter()
        .map(|q| {
            let df = docs
                .iter()
                .filter(|(_, tf)| tf.contains_key(q.as_str()))
                .count() as f64;
            (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
        })
        .collect();
    // The maximum attainable numerator: every query term at full saturation.
    let max_score: f64 = idf.iter().sum::<f64>() * (K1 + 1.0);

    candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (len, tf) = &docs[i];
            let norm = K1 * (1.0 - B + B * *len as f64 / avgdl);
            let numerator: f64 = idf
                .iter()
                .zip(q_terms.iter())
                .map(|(w, q)| {
                    let tf = tf.get(q.as_str()).copied().unwrap_or(0) as f64;
                    w * (tf * (K1 + 1.0) / (tf + norm))
                })
                .sum();
            Scored {
                ordinal: c.ordinal,
                score: (numerator / max_score).clamp(0.0, 1.0),
            }
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// The judge arms
// ─────────────────────────────────────────────────────────────────────────────

/// A local judge: scores (query, document) pairs into a relevance
/// probability, in process, with no network call and no tokens.
///
/// An enum, not a `dyn` trait, for the same reason [`crate::Provider`] is:
/// the set is closed and small, and the async arm would otherwise cost a
/// boxed future per call.
pub enum LocalJudge {
    /// The deterministic lexical scorer ([`lexical_scores`]). Always
    /// available, microseconds per page, no semantic signal — see the
    /// module header for the honest scope.
    Lexical,
    /// The node's local decision head as a cross-encoder: an NLI pair score
    /// of the document against "relevant to the query" and its negation.
    /// Feature `judge-local`.
    #[cfg(feature = "judge-local")]
    Model(ModelJudge),
}

impl LocalJudge {
    /// What the response's `judged.scorer` says. A caller must be able to
    /// see WHICH judge judged, because the two arms are different scorers
    /// with different ceilings.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Lexical => "lexical",
            #[cfg(feature = "judge-local")]
            Self::Model(_) => "model",
        }
    }

    /// The model id the response's `judged.model` reports — on the model arm
    /// only. The lexical arm is arithmetic, not a model, and must never have a
    /// model name next to it: a response naming a "model" that is a formula
    /// is the silent-fake this crate refuses everywhere.
    pub fn model_id(&self) -> Option<&'static str> {
        match self {
            Self::Lexical => None,
            #[cfg(feature = "judge-local")]
            Self::Model(_) => Some(ModelJudge::MODEL_ID),
        }
    }

    /// Score every candidate into a relevance probability. One [`Scored`]
    /// per candidate, input order, ordinals untouched. Empty input is empty
    /// output without running anything.
    pub async fn judge(
        &self,
        query: &str,
        candidates: &[Candidate],
    ) -> Result<Vec<Scored>, RerankError> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        match self {
            Self::Lexical => Ok(lexical_scores(query, candidates)),
            #[cfg(feature = "judge-local")]
            Self::Model(m) => m.judge(query, candidates).await,
        }
    }
}

/// The semantic arm: [`xerj_ai::decide::DecideHandle`] scoring one
/// (document, hypothesis) pair per hit.
///
/// The handle shares the process-wide model registry with the decide tier
/// (`xerj-ai`'s `shared_decide_cell`), so a node that armed
/// `XERJ_DECIDE_MODE=local` judges with the SAME loaded copy the decide
/// ladder uses — one model in memory, two questions asked of it.
#[cfg(feature = "judge-local")]
pub struct ModelJudge {
    handle: xerj_ai::decide::DecideHandle,
}

#[cfg(feature = "judge-local")]
impl ModelJudge {
    /// The model id the response reports. It is the decide head's own id —
    /// this arm IS that head asked a second question — and never a hosted
    /// provider's name.
    pub const MODEL_ID: &'static str = "xerj-decide-local-1";

    /// Arm the judge on a model directory holding `config.json`,
    /// `tokenizer.json` and `model.safetensors`. Loading is lazy (first
    /// judged search) and shared process-wide with the decide tier.
    pub fn from_model_dir(model_dir: PathBuf) -> Self {
        Self {
            handle: xerj_ai::decide::DecideHandle::new(xerj_ai::decide::DecideConfig { model_dir }),
        }
    }

    /// The configured model directory, for diagnostics and error bodies.
    pub fn model_dir(&self) -> &std::path::Path {
        self.handle.model_dir()
    }

    /// Score every candidate: premise = the document (title then text, the
    /// same concatenation the hosted provider's wire format sends),
    /// hypotheses = the decide head's own relevance statement and its
    /// negation, built with the head's trained templates
    /// ([`xerj_ai::decide::hypothesis`] / `negation_hypothesis`) — the pair
    /// shape that head was trained against, so a checkpoint trained for
    /// decide scores correctly here without a second template to maintain.
    ///
    /// `p_relevant` is the entailment probability of the positive
    /// hypothesis renormalised against its negation — the head's
    /// `score_blocking` already returns each request's hypotheses summed to
    /// 1, so `probs[0]` is that comparison's answer.
    pub async fn judge(
        &self,
        query: &str,
        candidates: &[Candidate],
    ) -> Result<Vec<Scored>, RerankError> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let label = format!("a passage relevant to the question: {query}");
        let requests: Vec<xerj_ai::decide::ScoreRequest> = candidates
            .iter()
            .map(|c| {
                let mut premise = String::new();
                if let Some(t) = &c.title {
                    premise.push_str(t);
                    premise.push_str(". ");
                }
                premise.push_str(&c.text);
                xerj_ai::decide::ScoreRequest {
                    premise,
                    hypotheses: vec![
                        xerj_ai::decide::hypothesis(&label),
                        xerj_ai::decide::negation_hypothesis(&label),
                    ],
                }
            })
            .collect();
        let scored = self.handle.score(requests).await.map_err(|e| {
            RerankError::LocalModel(format!(
                "the judge model at {} could not score this page: {e:#}",
                self.model_dir().display()
            ))
        })?;
        // One probability per request in order; the interface guarantees the
        // lengths, and a request that somehow came back short is a contract
        // break, not a zero score to invent.
        if scored.len() != candidates.len() {
            return Err(RerankError::LocalModel(format!(
                "the judge model returned {} scores for {} documents",
                scored.len(),
                candidates.len()
            )));
        }
        Ok(candidates
            .iter()
            .zip(scored)
            .filter_map(|(c, probs)| {
                probs.first().map(|&p| Scored {
                    ordinal: c.ordinal,
                    score: (p as f64).clamp(0.0, 1.0),
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cand(ordinal: usize, title: Option<&str>, text: &str) -> Candidate {
        Candidate {
            ordinal,
            title: title.map(str::to_string),
            text: text.to_string(),
        }
    }

    // ── the block parser ────────────────────────────────────────────────────

    #[test]
    fn parses_the_documented_block() {
        let cfg = JudgeConfig::from_json(&json!({"local": true, "min_p": 0.5})).unwrap();
        assert_eq!(cfg.min_p, Some(0.5));
        assert_eq!(cfg.query, None);

        // `local` may be omitted (there is only one judge), `{}` means
        // "judge with defaults", and the ends of the range are valid —
        // the same acceptance shape `rerank` gives `{}` and `min_score`.
        let cfg = JudgeConfig::from_json(&json!({})).unwrap();
        assert_eq!(cfg.min_p, None);
        assert!(JudgeConfig::from_json(&json!({"min_p": 0.0})).is_ok());
        assert!(JudgeConfig::from_json(&json!({"min_p": 1.0})).is_ok());

        let cfg = JudgeConfig::from_json(&json!({"query": "why did the vpn drop?"})).unwrap();
        assert_eq!(cfg.query.as_deref(), Some("why did the vpn drop?"));
    }

    #[test]
    fn unknown_field_is_rejected_not_ignored() {
        let err = JudgeConfig::from_json(&json!({"min_score": 0.5})).unwrap_err();
        assert!(matches!(err, RerankError::Config(_)));
        assert!(
            err.to_string().contains("min_score"),
            "the misspelling must be named: {err}"
        );
        assert!(err.to_string().contains("min_p"), "{err}");
    }

    #[test]
    fn local_false_is_a_refusal_not_a_silent_no_op() {
        // A block that says `local: false` has no other judge to be: honouring
        // it silently would be the accepted-and-ignored class, so the caller
        // is told to remove the block instead.
        let err = JudgeConfig::from_json(&json!({"local": false})).unwrap_err();
        assert!(err.to_string().contains("cannot be false"), "{err}");
        let err = JudgeConfig::from_json(&json!({"local": "yes"})).unwrap_err();
        assert!(err.to_string().contains("must be true"), "{err}");
    }

    #[test]
    fn min_p_must_be_a_probability() {
        assert!(JudgeConfig::from_json(&json!({"min_p": 1.2})).is_err());
        assert!(JudgeConfig::from_json(&json!({"min_p": -0.1})).is_err());
        assert!(JudgeConfig::from_json(&json!({"min_p": "high"})).is_err());
    }

    #[test]
    fn the_question_has_a_ceiling_and_cannot_be_blank() {
        let err =
            JudgeConfig::from_json(&json!({"query": "q".repeat(MAX_QUERY_CHARS + 1)})).unwrap_err();
        assert!(err.to_string().contains("judge.query"), "{err}");
        assert!(err.to_string().len() < 400, "never echo the value: {err}");
        assert!(JudgeConfig::from_json(&json!({"query": "é".repeat(MAX_QUERY_CHARS)})).is_ok());
        assert!(JudgeConfig::from_json(&json!({"query": "   "})).is_err());
    }

    #[test]
    fn a_megabyte_key_name_is_not_echoed_back() {
        let mut block = serde_json::Map::new();
        block.insert("k".repeat(1_000_000), json!(1));
        let err = JudgeConfig::from_json(&Value::Object(block))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("unknown `judge` field"),
            "{}",
            &err[..200.min(err.len())]
        );
        assert!(err.len() < 1_000, "echoed {} bytes", err.len());
    }

    // ── the lexical scorer ──────────────────────────────────────────────────

    #[test]
    fn cjk_query_matches_inside_unsegmented_text() {
        let out = lexical_scores(
            "雷同",
            &[
                cand(7, None, "该段内容存在雷同需要核查"),
                cand(2, None, "今天的天气晴朗适合出行"),
            ],
        );
        assert!(
            out[0].score > 0.0,
            "embedded Chinese term must match: {out:?}"
        );
        assert_eq!(out[1].score, 0.0);
        assert_eq!(out.iter().map(|s| s.ordinal).collect::<Vec<_>>(), [7, 2]);
    }

    #[test]
    fn cjk_single_character_and_adjacent_pair_have_evidence() {
        let candidates = [
            cand(0, Some("中文检索"), "雷同内容"),
            cand(1, None, "同雷内容"),
        ];
        let single = lexical_scores("雷", &candidates);
        assert!(single.iter().all(|s| s.score > 0.0), "{single:?}");
        let pair = lexical_scores("雷同", &candidates);
        assert!(
            pair[0].score > pair[1].score,
            "adjacency adds evidence: {pair:?}"
        );
    }

    #[test]
    fn legacy_non_cjk_score_receipt() {
        let candidates = [
            cand(9, Some("Trial results"), "vitamin d improved bone density"),
            cand(3, None, "vitamin rich vegetables"),
            cand(2, None, "other topic"),
        ];
        let out = lexical_scores("vitamin bone density", &candidates);
        assert_eq!(
            out.iter().map(|s| s.score.to_bits()).collect::<Vec<_>>(),
            [4599937497660334437, 4591716393592272481, 0]
        );
    }

    #[test]
    fn cjk_tokens_stop_at_separators_and_keep_non_cjk_words() {
        assert_eq!(
            tokens("API中文v2 CAFÉ İSTANBUL 123").collect::<Vec<_>>(),
            [
                "api",
                "中",
                "中文",
                "文",
                "v2",
                "café",
                "i\u{307}stanbul",
                "123"
            ]
        );
        for text in ["雷，同", "雷 同", "雷😀同", "雷A同"] {
            assert!(!tokens(text).any(|t| t == "雷同"), "{text}");
        }
        assert_eq!(
            tokens("𠀀中文").collect::<Vec<_>>(),
            ["𠀀", "𠀀中", "中", "中文", "文"]
        );
        assert_eq!(
            tokens("東京都 한국어").collect::<Vec<_>>(),
            ["東", "東京", "京", "京都", "都", "한", "한국", "국", "국어", "어"]
        );
        assert_eq!(
            tokens("alpha_beta can't naïve").collect::<Vec<_>>(),
            ["alpha", "beta", "can", "t", "naïve"]
        );
    }

    #[test]
    fn cjk_title_and_text_do_not_create_a_cross_field_pair() {
        let scores = lexical_scores("雷同", &[cand(4, Some("雷"), "同"), cand(8, None, "雷同")]);
        assert!(scores[1].score > scores[0].score, "{scores:?}");
        assert_eq!(
            scores,
            lexical_scores("雷同", &[cand(4, Some("雷"), "同"), cand(8, None, "雷同"),])
        );
        assert!(scores
            .iter()
            .all(|s| s.score.is_finite() && (0.0..=1.0).contains(&s.score)));
    }

    #[test]
    fn cjk_scoring_handles_the_query_and_document_character_limits() {
        let query: String = (0..crate::MAX_QUERY_CHARS)
            .map(|i| char::from_u32(0x4e00 + i as u32).unwrap())
            .collect();
        let text = format!("{query}{query}");
        assert_eq!(text.chars().count(), crate::MAX_DOC_CHARS);
        let candidates: Vec<_> = (0..crate::JEV_MAX_DOCS_PER_CALL)
            .map(|i| cand(i, None, &text))
            .collect();
        let scores = lexical_scores(&query, &candidates);
        assert_eq!(scores.len(), candidates.len());
        assert!(scores.iter().all(|s| s.score > 0.0 && s.score <= 1.0));
    }

    #[test]
    fn no_query_words_scores_zero_everywhere() {
        let out = lexical_scores(
            "",
            &[cand(0, Some("t"), "body"), cand(1, None, "other body")],
        );
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|s| s.score == 0.0));
        // Punctuation-only queries have no words either.
        let out = lexical_scores("?! — …", &[cand(3, None, "body")]);
        assert_eq!(out[0].score, 0.0);
    }

    #[test]
    fn scores_are_ordered_by_term_coverage_and_bounded() {
        let q = "vitamin d supplementation bone density";
        let cands = vec![
            cand(0, Some("Cooking"), "vitamin rich vegetables for dinner"),
            cand(
                1,
                Some("Trial results"),
                "vitamin d supplementation improved bone density in the treatment group",
            ),
            cand(2, None, "nothing about the topic at all"),
        ];
        let out = lexical_scores(q, &cands);
        assert_eq!(
            out.iter().map(|s| s.ordinal).collect::<Vec<_>>(),
            vec![0, 1, 2],
            "input order preserved; the STAGE sorts"
        );
        let by = |o: usize| out.iter().find(|s| s.ordinal == o).unwrap().score;
        assert_eq!(by(2), 0.0, "no query term present scores exactly 0");
        assert!(by(1) > by(0), "full coverage beats one word: {:?}", out);
        for s in &out {
            assert!((0.0..=1.0).contains(&s.score), "{s:?}");
        }
    }

    #[test]
    fn a_rare_term_outweighs_a_term_every_document_shares() {
        // Two docs, two query terms: "shared" is in both (df = 2), "unique"
        // in one (df = 1). The doc holding the rare term must score higher
        // than an equally-long doc holding only the common one — this is
        // the window-local IDF doing its work.
        let q = "shared unique";
        let cands = vec![
            cand(0, None, "shared shared shared"),
            cand(1, None, "unique shared"),
        ];
        let out = lexical_scores(q, &cands);
        assert!(
            out[1].score > out[0].score,
            "rare term must outweigh common: {out:?}"
        );
    }

    #[test]
    fn deterministic_and_repeatable() {
        let q = "bone density trial results vitamin";
        let cands: Vec<Candidate> = (0..30)
            .map(|i| {
                cand(
                    i,
                    Some(&format!("title {i}")),
                    &format!(
                        "body {i} with varying vitamin d bone density content {}",
                        "x".repeat(i)
                    ),
                )
            })
            .collect();
        let a = lexical_scores(q, &cands);
        let b = lexical_scores(q, &cands);
        assert_eq!(a, b, "same inputs must judge to the same probabilities");
        assert!(a.iter().all(|s| (0.0..=1.0).contains(&s.score)));
    }

    /// The scorer's own cost for the issue's stated page (top-30), measured
    /// here so the PR can state it honestly: the ≤ 40 ms p50 figure in the
    /// issue is a RELEASE-time gate over the whole stage, not this micro
    /// figure — but this is the part this PR adds.
    #[test]
    fn a_top_30_page_scores_in_microseconds() {
        let q = "vitamin d supplementation and bone density in the treatment group";
        let cands: Vec<Candidate> = (0..30)
            .map(|i| {
                cand(
                    i,
                    Some(&format!("doc title {i}")),
                    &("vitamin d bone density body text. ".repeat(50)),
                )
            })
            .collect();
        let started = std::time::Instant::now();
        let out = lexical_scores(q, &cands);
        let elapsed = started.elapsed();
        assert_eq!(out.len(), 30);
        assert!(
            elapsed < std::time::Duration::from_millis(40),
            "scoring 30 documents took {elapsed:?}"
        );
    }

    #[test]
    fn the_lexical_arm_judges_without_erring_and_names_itself() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let out = rt
            .block_on(LocalJudge::Lexical.judge("q", &[cand(0, None, "q text")]))
            .unwrap();
        assert_eq!(
            out,
            vec![Scored {
                ordinal: 0,
                score: out[0].score
            }]
        );
        assert_eq!(LocalJudge::Lexical.name(), "lexical");
        // Empty page: no work, no error.
        let out = rt.block_on(LocalJudge::Lexical.judge("q", &[])).unwrap();
        assert!(out.is_empty());
    }

    // ── the model arm (compiled only with `judge-local`) ────────────────────

    /// Plumbing only, against the tiny untrained fixture: the arm scores,
    /// returns one bounded probability per candidate in order, and reuses
    /// the decide loader's shared registry. Says NOTHING about quality —
    /// the fixture is reproducible arithmetic, not judgement (see
    /// `xerj-ai`'s `decide::testing`).
    #[cfg(feature = "judge-local")]
    #[test]
    fn the_model_arm_scores_a_page_through_the_decide_loader() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        xerj_ai::decide::testing::write_fixture(&dir).expect("write decide fixture");
        let judge = ModelJudge::from_model_dir(dir.clone());
        assert_eq!(judge.model_dir(), dir.as_path());
        assert_eq!(ModelJudge::MODEL_ID, "xerj-decide-local-1");

        let cands = vec![
            cand(0, Some("Bone health"), "vitamin d and bone density"),
            cand(1, None, "cooking vegetables"),
        ];
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let out = rt
            .block_on(judge.judge("vitamin d bone density", &cands))
            .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].ordinal, 0);
        assert_eq!(out[1].ordinal, 1);
        assert!(
            out.iter().all(|s| (0.0..=1.0).contains(&s.score)),
            "{out:?}"
        );
        // Deterministic: the same page judges to the same probabilities.
        let again = rt
            .block_on(judge.judge("vitamin d bone density", &cands))
            .unwrap();
        assert_eq!(out, again);
        assert_eq!(LocalJudge::Model(judge).name(), "model");
    }

    /// A missing model directory is a surfaced 503-class fault, never a
    /// silent fall back to the lexical scorer.
    #[cfg(feature = "judge-local")]
    #[test]
    fn a_missing_model_dir_surfaces_as_local_model() {
        let judge = ModelJudge::from_model_dir(std::path::PathBuf::from("/nonexistent/judge"));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(judge.judge("q", &[cand(0, None, "text")]))
            .unwrap_err();
        assert!(matches!(err, RerankError::LocalModel(_)), "{err}");
        assert!(err.to_string().contains("config.json"), "{err}");
        assert_eq!(err.policy(), crate::Policy::Surface);
    }
}
