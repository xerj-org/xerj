//! The `local` provider: a cross-encoder that runs inside this process.
//!
//! # Why it exists
//!
//! The hosted provider sends the text of the top-N hits, and the query, to a
//! third party, needs an API key and a network, and bills per token. This one
//! sends nothing anywhere: the model is a file on this machine and the scoring
//! is a function call. That makes calibrated second-stage relevance available
//! on an air-gapped node, on a laptop with no account anywhere, and on a corpus
//! whose text is not allowed to leave the host — and it is why `[rerank]
//! enabled = false`, which exists to forbid egress, does **not** switch this
//! provider off. `[judge] enabled = false` does.
//!
//! # What it is
//!
//! A cross-encoder reads the query and one document *together* and emits one
//! relevance logit. That is the opposite trade from the engine's embedder,
//! which reads each text alone so vectors can be indexed: a cross-encoder
//! cannot be precomputed and costs one forward pass per candidate, and in
//! exchange every attention layer sees both sides. It is therefore a second
//! stage over a short window, never a first stage. (`benchmarks/beir-hybrid/`
//! measured the tempting shortcut — reorder a BM25 shortlist with the
//! *bi*-encoder — and it lost to plain hybrid fusion; this is the thing that
//! note said would be worth building instead.)
//!
//! # The score
//!
//! `_score = sigmoid(scale × logit + bias)`. With `scale = 1, bias = 0` that is
//! the raw sigmoid, which orders documents correctly and is **not** a
//! probability — the MiniLM model emits logits of ±10, so nearly everything is
//! 0.00 or 1.00. Each model's `scale`/`bias` lives in
//! [`xerj_ai::judge::MODELS`] with the data it was fitted on;
//! `_rerank.local.calibration` reports which map produced a response, and
//! `docs/RERANK.md` says what the number does and does not mean, with the
//! measured calibration error before and after the fit.
//!
//! # Fail policy
//!
//! The same split as the hosted provider (see the crate doc): running out of
//! time degrades, configuration and contract faults surface.
//!
//! | what happened | outcome |
//! |---|---|
//! | deadline hit between forward passes | `Reordered`, the unreached documents unjudged, `partial_failures = 1` |
//! | deadline hit before any document was scored | `Degraded` |
//! | model still loading / downloading | `Degraded`, the load carries on for the next request |
//! | every admission slot busy for the whole budget | `Degraded` |
//! | unknown tier, option that does not apply | `Config` → 400 |
//! | `[judge] enabled = false` | `LocalDisabledByOperator` → 403 |
//! | model not on disk and downloads off, not enough memory, bad checksum | `LocalModel` → 503 |
//! | built without the `neural` feature | `LocalUnavailable` → 501 |

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::{Candidate, Deadline, RerankConfig, RerankError, RerankOutcome};
#[cfg(feature = "local")]
use crate::{ScoreKind, Scored, Usage};

/// Default `rerank.max_doc_chars` for this provider.
///
/// The hosted default (1,200) is a token-billing choice. Here the binding
/// limit is the model's 512-token window, roughly 2,000–2,500 characters of
/// English, so the character clip only has to stop a megabyte field from being
/// tokenised for nothing.
pub const LOCAL_DEFAULT_MAX_DOC_CHARS: usize = 4_000;

/// The provider names that select this arm.
pub fn is_local(provider: &str) -> bool {
    provider == "local"
}

/// Apply this provider's request rules to a freshly parsed `rerank` block.
///
/// Called by [`RerankConfig::from_json`] when `provider` is `local`. Two jobs:
///
/// * options that mean something only to a hosted provider are refused by name
///   rather than ignored — a caller who set `instructions` and got a
///   cross-encoder that never read them has been misled;
/// * defaults that were chosen for the hosted provider are replaced where the
///   caller did not set them: no model name (the server's default tier
///   applies) and a character clip sized for a 512-token window.
pub fn adjust_request(
    obj: &serde_json::Map<String, Value>,
    cfg: &mut RerankConfig,
) -> Result<(), RerankError> {
    for (key, why) in [
        (
            "instructions",
            "a cross-encoder scores (query, document) pairs and reads no instructions; put \
             what matters in `rerank.query`",
        ),
        (
            "batch",
            "forward passes are sized by a padded-token budget, not a document count",
        ),
        (
            "max_concurrency",
            "the thread budget is the server's (`[judge] threads`)",
        ),
    ] {
        if obj.contains_key(key) {
            return Err(RerankError::Config(format!(
                "`rerank.{key}` does not apply to provider `local`: {why}"
            )));
        }
    }
    if !obj.contains_key("model") {
        cfg.model = String::new();
    }
    if !obj.contains_key("max_doc_chars") {
        cfg.max_doc_chars = LOCAL_DEFAULT_MAX_DOC_CHARS;
    }
    Ok(())
}

/// Operator settings for local models — `[judge]` in the server config,
/// carried as plain data so this crate does not link `xerj-common`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalJudgeConfig {
    /// `false` refuses the `local` provider (and `POST /_judge`) with a 403.
    pub enabled: bool,
    /// `false` never opens a network connection: models must be on disk.
    pub download: bool,
    /// Hugging Face cache override. `None`: `HF_HOME` or `~/.cache/huggingface`.
    pub cache_dir: Option<PathBuf>,
    /// Air-gapped root: `<model_dir>/rerank-<tier>/` holds `config.json`,
    /// `tokenizer.json` and `model.safetensors`.
    pub model_dir: Option<PathBuf>,
    /// Threads in the judge's pool; `0` lets the resource policy decide.
    pub threads: usize,
    /// Scoring calls admitted at once.
    pub max_inflight: usize,
    /// Tier used when a request names none.
    pub rerank_model: String,
}

impl Default for LocalJudgeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            download: true,
            cache_dir: None,
            model_dir: None,
            threads: 0,
            max_inflight: 2,
            rerank_model: "small".to_string(),
        }
    }
}

/// Handle to the local judge. Cheap to clone; one runtime behind it.
#[derive(Clone)]
pub struct LocalJudge {
    cfg: LocalJudgeConfig,
    #[cfg(feature = "local")]
    runtime: std::sync::Arc<xerj_ai::judge::JudgeRuntime>,
}

impl std::fmt::Debug for LocalJudge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalJudge")
            .field("cfg", &self.cfg)
            .finish()
    }
}

impl Default for LocalJudge {
    fn default() -> Self {
        Self::new(LocalJudgeConfig::default())
    }
}

/// Why this build cannot run a local model.
#[cfg(not(feature = "local"))]
const NOT_COMPILED_IN: &str =
    "this build has no local judge: it was compiled without the `neural` feature. The release \
     binaries include it; a `--no-default-features` build does not";

impl LocalJudge {
    pub fn new(cfg: LocalJudgeConfig) -> Self {
        #[cfg(feature = "local")]
        let runtime = xerj_ai::judge::JudgeRuntime::new(xerj_ai::judge::JudgeRuntimeConfig {
            enabled: cfg.enabled,
            allow_download: cfg.download,
            cache_dir: cfg.cache_dir.clone(),
            model_dir: cfg.model_dir.clone(),
            threads: cfg.threads,
            max_inflight: cfg.max_inflight,
        });
        Self {
            cfg,
            #[cfg(feature = "local")]
            runtime,
        }
    }

    /// Whether this binary can run a local model at all.
    pub const fn compiled_in() -> bool {
        cfg!(feature = "local")
    }

    pub fn config(&self) -> &LocalJudgeConfig {
        &self.cfg
    }

    /// The tier a request resolves to: what it named, else the server default.
    pub fn rerank_tier<'a>(&'a self, requested: &'a str) -> &'a str {
        if requested.trim().is_empty() {
            &self.cfg.rerank_model
        } else {
            requested
        }
    }

    /// Validate a parsed request against this server's judge: is it switched
    /// on, and is the tier one that exists. Runs before any document is read.
    pub fn check_request(&self, cfg: &RerankConfig) -> Result<(), RerankError> {
        #[cfg(not(feature = "local"))]
        {
            let _ = cfg;
            Err(RerankError::LocalUnavailable(NOT_COMPILED_IN.to_string()))
        }
        #[cfg(feature = "local")]
        {
            use xerj_ai::judge::{model_spec, tiers, JudgeTask};
            if !self.cfg.enabled {
                return Err(RerankError::LocalDisabledByOperator);
            }
            let tier = self.rerank_tier(&cfg.model);
            if model_spec(JudgeTask::Rerank, tier).is_none() {
                return Err(RerankError::Config(format!(
                    "unknown `rerank.model` `{tier}` for provider `local`; available: {}. A \
                     request picks a tier by name and cannot make the server fetch another model",
                    tiers(JudgeTask::Rerank).join(", ")
                )));
            }
            Ok(())
        }
    }

    /// What `GET /_xerj/rerank` reports about this provider. Never loads a
    /// model and never touches the network.
    pub fn status(&self) -> Value {
        #[cfg(feature = "local")]
        let models: Vec<Value> = xerj_ai::judge::MODELS
            .iter()
            .filter(|m| m.task == xerj_ai::judge::JudgeTask::Rerank)
            .map(|m| {
                let presence = self.runtime.presence(m);
                let mut doc = json!({
                    "model": m.tier,
                    "repository": m.repo,
                    "revision": m.revision,
                    "download_mb": m.download_bytes / 1_000_000,
                    "resident_mb": m.resident_bytes() / (1024 * 1024),
                    "licence": m.licence,
                    "training_data": m.data_note,
                    "calibration": calibration_document(&m.calibration),
                    "state": presence.as_str(),
                });
                if let xerj_ai::judge::ModelPresence::Failed(reason) = presence {
                    doc["reason"] = json!(reason);
                }
                doc
            })
            .collect();
        #[cfg(not(feature = "local"))]
        let models: Vec<Value> = Vec::new();

        #[cfg(feature = "local")]
        let threads = self.runtime.threads();
        #[cfg(not(feature = "local"))]
        let threads = 0usize;

        json!({
            "compiled_in": Self::compiled_in(),
            "enabled": self.cfg.enabled,
            "download": self.cfg.download,
            "default_model": self.cfg.rerank_model,
            "threads": threads,
            "max_inflight": self.cfg.max_inflight,
            "max_pair_tokens": 512,
            "models": models,
            "data_egress": "none: documents are scored in this process. The only network \
                            use is the one-time model download from huggingface.co, which \
                            sends no document or query text and which `[judge] download = \
                            false` forbids.",
        })
    }

    /// Score and reorder `candidates` with the cross-encoder.
    #[cfg(not(feature = "local"))]
    pub async fn rerank(
        &self,
        _query: &str,
        _candidates: &[Candidate],
        _cfg: &RerankConfig,
        _deadline: &Deadline,
    ) -> Result<RerankOutcome, RerankError> {
        Err(RerankError::LocalUnavailable(NOT_COMPILED_IN.to_string()))
    }

    /// Score and reorder `candidates` with the cross-encoder.
    #[cfg(feature = "local")]
    pub async fn rerank(
        &self,
        query: &str,
        candidates: &[Candidate],
        cfg: &RerankConfig,
        deadline: &Deadline,
    ) -> Result<RerankOutcome, RerankError> {
        use xerj_ai::judge::{model_spec, JudgeError, JudgeTask};

        self.check_request(cfg)?;
        let tier = self.rerank_tier(&cfg.model).to_string();
        let spec = model_spec(JudgeTask::Rerank, &tier)
            .ok_or_else(|| RerankError::Config(format!("unknown local rerank model `{tier}`")))?;
        let until = std::time::Instant::now() + deadline.remaining();

        let map_err = |e: JudgeError| -> Result<RerankOutcome, RerankError> {
            if e.degradable() {
                return Ok(RerankOutcome::Degraded {
                    reason: e.to_string(),
                });
            }
            Err(match e {
                JudgeError::Disabled => RerankError::LocalDisabledByOperator,
                JudgeError::UnknownModel { .. } => RerankError::Config(e.to_string()),
                JudgeError::LoadFailed(m) => RerankError::LocalModel(m),
                other => RerankError::LocalInference(other.to_string()),
            })
        };

        let model = match self.runtime.model(spec, until).await {
            Ok(model) => model,
            Err(e) => return map_err(e),
        };

        let pairs: Vec<(String, String)> = candidates
            .iter()
            .map(|c| (query.to_string(), document_text(c, cfg.max_doc_chars)))
            .collect();
        let scored = match self.runtime.score(model, pairs, until).await {
            Ok(scored) => scored,
            Err(e) => return map_err(e),
        };

        let scores: Vec<Scored> = candidates
            .iter()
            .zip(&scored.logits)
            .filter_map(|(c, logits)| {
                let logit = *logits.as_ref()?.first()?;
                Some(Scored {
                    ordinal: c.ordinal,
                    score: spec.calibration.probability(logit),
                })
            })
            .collect();

        if scores.is_empty() {
            return Ok(RerankOutcome::Degraded {
                reason: format!(
                    "deadline exceeded after {}ms before the local `{tier}` model scored any \
                     document (queued {}ms)",
                    cfg.timeout.as_millis(),
                    scored.queued.as_millis()
                ),
            });
        }

        Ok(RerankOutcome::Reordered {
            // `Calibration::NONE` is the raw sigmoid: an order, not a
            // probability. Every shipped tier carries it (the fit diverged —
            // `docs/RERANK.md`), so this is `Relevance` today for all three.
            score_kind: if spec.calibration.is_identity() {
                ScoreKind::Relevance
            } else {
                ScoreKind::Probability
            },
            // The deadline stopped the pass loop: the unreached documents are
            // one unfinished unit of work, reported the way a failed hosted
            // batch is, so `judged` plus this accounts for the whole window.
            partial_failures: usize::from(scored.stats.unscored > 0),
            scores,
            // Nothing is billed. The meter stays at zero so an operator's
            // token dashboard does not count work that cost no tokens.
            usage: Usage::default(),
            detail: Some(json!({
                "repository": spec.repo,
                "calibration": calibration_document(&spec.calibration),
                "tokens": scored.stats.real_tokens,
                "forward_passes": scored.stats.forward_passes,
                "truncated": scored.stats.truncated,
                "unscored_at_deadline": scored.stats.unscored,
                "queued_ms": scored.queued.as_millis() as u64,
                "inference_ms": scored.inference.as_millis() as u64,
                "threads": self.runtime.threads(),
                "data_egress": "none",
            })),
        })
    }
}

#[cfg(feature = "local")]
fn calibration_document(c: &xerj_ai::judge::Calibration) -> Value {
    json!({
        "method": if c.is_identity() { "none" } else { "platt" },
        "scale": c.scale,
        "bias": c.bias,
        "fitted_on": c.fitted_on,
    })
}

/// The document side of a pair: title, then text, each clipped.
///
/// The same concatenation the BEIR cross-encoder evaluations use, so the
/// numbers in `benchmarks/local-judge/` describe what this function feeds the
/// model.
pub fn document_text(c: &Candidate, max_chars: usize) -> String {
    let text = crate::clip(&c.text, max_chars);
    match &c.title {
        Some(title) if !title.trim().is_empty() => {
            format!("{}\n{}", crate::clip(title, max_chars), text)
        }
        _ => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(v: Value) -> RerankConfig {
        RerankConfig::from_json(&v).expect("valid rerank block")
    }

    #[test]
    fn the_document_side_is_title_then_text_each_clipped_on_a_char_boundary() {
        let c = Candidate {
            ordinal: 0,
            title: Some("Überschrift".into()),
            text: "körper".repeat(10),
        };
        assert_eq!(document_text(&c, 4), "Über\nkörp");
        let untitled = Candidate {
            ordinal: 1,
            title: Some("  ".into()),
            text: "body".into(),
        };
        assert_eq!(document_text(&untitled, 100), "body");
    }

    #[test]
    fn a_request_that_names_no_model_gets_the_server_default() {
        let judge = LocalJudge::new(LocalJudgeConfig {
            rerank_model: "base".into(),
            ..Default::default()
        });
        assert_eq!(judge.rerank_tier(""), "base");
        assert_eq!(judge.rerank_tier("large"), "large");
        let parsed = cfg(json!({"provider": "local"}));
        assert_eq!(
            parsed.model, "",
            "no hosted default leaks into a local request"
        );
        assert_eq!(parsed.max_doc_chars, LOCAL_DEFAULT_MAX_DOC_CHARS);
        let explicit = cfg(json!({"provider": "local", "max_doc_chars": 900, "model": "large"}));
        assert_eq!(explicit.max_doc_chars, 900);
        assert_eq!(explicit.model, "large");
    }

    #[test]
    fn hosted_only_options_are_refused_by_name() {
        for (block, needle) in [
            (
                json!({"provider": "local", "instructions": "be strict"}),
                "rerank.instructions",
            ),
            (json!({"provider": "local", "batch": 10}), "rerank.batch"),
            (
                json!({"provider": "local", "max_concurrency": 4}),
                "rerank.max_concurrency",
            ),
        ] {
            let err = RerankConfig::from_json(&block).unwrap_err();
            assert!(matches!(err, RerankError::Config(_)), "{err:?}");
            assert!(err.to_string().contains(needle), "{err}");
        }
        // The same keys stay valid for the hosted provider.
        assert!(RerankConfig::from_json(&json!({"provider": "jev", "batch": 10})).is_ok());
    }

    #[test]
    fn an_unknown_tier_is_a_400_that_lists_the_real_ones() {
        if !LocalJudge::compiled_in() {
            return;
        }
        let judge = LocalJudge::default();
        let err = judge
            .check_request(&cfg(json!({"provider": "local", "model": "jev-latest"})))
            .unwrap_err();
        assert!(matches!(err, RerankError::Config(_)), "{err:?}");
        assert!(err.to_string().contains("small, base, large"), "{err}");
        assert!(judge
            .check_request(&cfg(
                json!({"provider": "local", "model": "base", "window": 100})
            ))
            .is_ok());
    }

    #[test]
    fn the_operator_switch_is_judge_enabled_not_rerank_enabled() {
        if !LocalJudge::compiled_in() {
            return;
        }
        let off = LocalJudge::new(LocalJudgeConfig {
            enabled: false,
            ..Default::default()
        });
        let err = off
            .check_request(&cfg(json!({"provider": "local"})))
            .unwrap_err();
        assert!(matches!(err, RerankError::LocalDisabledByOperator));
    }

    #[test]
    fn status_reports_every_tier_with_its_licence_and_never_loads_anything() {
        let judge = LocalJudge::new(LocalJudgeConfig {
            download: false,
            cache_dir: Some(std::env::temp_dir().join("xerj-local-status-nothing-here")),
            ..Default::default()
        });
        let doc = judge.status();
        assert_eq!(doc["compiled_in"], LocalJudge::compiled_in());
        assert_eq!(doc["default_model"], "small");
        assert!(doc["data_egress"].as_str().unwrap().starts_with("none"));
        if LocalJudge::compiled_in() {
            let models = doc["models"].as_array().unwrap();
            assert_eq!(models.len(), 3);
            for m in models {
                assert_eq!(m["state"], "not_downloaded");
                assert!(m["licence"].is_string() && m["training_data"].is_string());
                assert_eq!(m["revision"].as_str().unwrap().len(), 40);
            }
        }
    }
}
