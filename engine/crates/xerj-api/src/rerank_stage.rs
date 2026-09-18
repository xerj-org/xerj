//! The `rerank` stage of `_search`.
//!
//! Runs after the engine has produced and rendered its hits, and before the
//! response is assembled. It never touches the engine: it widens the requested
//! page to the rerank window, hands the window to a [`xerj_rerank::Provider`],
//! and reorders the rendered hits by what comes back.
//!
//! The stage is strict about what it cannot do coherently. An explicit `sort`
//! means the caller has already said how results are ordered; paging past the
//! window means pages would be cut from two different orderings. Both are
//! refused with a 400 rather than quietly honoured in a way that misleads.

use serde_json::{json, Value};
use std::time::Duration;
use xerj_rerank::{Candidate, Deadline, Provider, RerankConfig, RerankError, RerankOutcome};

use crate::es_compat::EsSearchBody;
use crate::responses::EsHit;

/// A validated rerank request, produced before the search runs.
pub struct RerankPlan {
    cfg: RerankConfig,
    query: String,
    requested_size: usize,
}

/// Read a plain question out of the simple query shapes that carry one.
///
/// Deliberately shallow. A `bool` tree, a `hybrid` with two different arms or a
/// `query_string` with operators has no single "question", and picking one
/// would rank against a guess. Those callers pass `rerank.query`.
fn infer_query(q: &Value) -> Option<String> {
    let obj = q.as_object()?;
    if obj.len() != 1 {
        return None;
    }
    let (kind, params) = obj.iter().next()?;
    match kind.as_str() {
        "multi_match" | "semantic" | "simple_query_string" => {
            params.get("query")?.as_str().map(str::to_string)
        }
        "match" | "match_phrase" => {
            let field = params.as_object()?.values().next()?;
            match field {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => o.get("query")?.as_str().map(str::to_string),
                _ => None,
            }
        }
        _ => None,
    }
}

impl RerankPlan {
    /// Validate the request and widen the page to the rerank window.
    ///
    /// Returns `Ok(None)` when the body has no `rerank` block. `Err` carries the
    /// reason for a 400.
    pub fn prepare(body: &mut EsSearchBody, scrolling: bool) -> Result<Option<Self>, String> {
        let Some(raw) = body.rerank.as_ref() else {
            return Ok(None);
        };
        let cfg = RerankConfig::from_json(raw).map_err(|e| e.to_string())?;

        if scrolling {
            return Err(
                "`rerank` cannot be combined with `scroll`: a scroll streams the \
                        engine's order and a reranked window has a different one"
                    .into(),
            );
        }
        if body.sort.is_some() {
            return Err(
                "`rerank` cannot be combined with `sort`: an explicit sort already \
                        fixes the order, and reranking would discard it"
                    .into(),
            );
        }
        if body.search_after.is_some() {
            return Err("`rerank` cannot be combined with `search_after`".into());
        }
        if body.collapse.is_some() {
            return Err("`rerank` cannot be combined with `collapse`".into());
        }
        if matches!(body.source, Some(Value::Bool(false))) {
            return Err(
                "`rerank` needs document text: `_source: false` leaves nothing to \
                        judge. Use `rerank.fields` to limit what is sent instead"
                    .into(),
            );
        }
        if body.from + body.size > cfg.window {
            return Err(format!(
                "`from` + `size` ({}) exceeds `rerank.window` ({}): results past the window \
                 keep the engine's order, so a page cut across the boundary would mix two \
                 rankings. Raise `rerank.window` or page within it",
                body.from + body.size,
                cfg.window
            ));
        }

        let query = match cfg.query.clone() {
            Some(q) => q,
            None => body.query.as_ref().and_then(infer_query).ok_or_else(|| {
                "`rerank.query` is required: the search query is not a shape a single \
                 question can be read from (supported for inference: match, match_phrase, \
                 multi_match, semantic, simple_query_string)"
                    .to_string()
            })?,
        };

        let requested_size = body.size;
        body.size = body.size.max(cfg.window);

        Ok(Some(Self {
            cfg,
            query,
            requested_size,
        }))
    }

    fn candidate(&self, ordinal: usize, hit: &EsHit) -> Candidate {
        let src = hit.source.as_ref().and_then(Value::as_object);
        let title = src
            .and_then(|o| o.get("title"))
            .and_then(Value::as_str)
            .map(str::to_string);

        let mut text = String::new();
        if let Some(obj) = src {
            let mut push = |v: &Value| {
                if let Some(s) = v.as_str() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(s);
                }
            };
            match &self.cfg.fields {
                Some(fields) => fields.iter().filter_map(|f| obj.get(f)).for_each(&mut push),
                None => obj
                    .iter()
                    .filter(|(k, _)| k.as_str() != "title")
                    .map(|(_, v)| v)
                    .for_each(&mut push),
            }
        }
        Candidate {
            ordinal,
            title,
            text,
        }
    }

    /// Rerank `hits` in place and return the `_rerank` block for the response.
    ///
    /// `Err` means a fault the caller must see (see `xerj_rerank::Policy`);
    /// degradable causes come back as `Ok` with `"applied": false`.
    pub async fn apply(
        &self,
        hits: &mut Vec<EsHit>,
        max_score: &mut Option<f64>,
        from: usize,
    ) -> Result<Value, RerankError> {
        let window = self.cfg.window.min(hits.len());
        let candidates: Vec<Candidate> = hits[..window]
            .iter()
            .enumerate()
            .map(|(i, h)| self.candidate(i, h))
            .collect();

        let provider = Provider::from_config(&self.cfg)?;
        let deadline = Deadline::new(self.cfg.timeout.max(Duration::from_millis(1)));
        let outcome = provider
            .rerank(&self.query, &candidates, &self.cfg, &deadline)
            .await?;

        let info = match outcome {
            RerankOutcome::Degraded { reason } => {
                tracing::warn!(%reason, "rerank degraded; engine order kept");
                json!({
                    "applied": false,
                    "reason": reason,
                    "provider": self.cfg.provider,
                })
            }
            RerankOutcome::Reordered {
                scores,
                partial_failures,
            } => {
                let ordered = xerj_rerank::apply_scores(&candidates, &scores, self.cfg.min_score);
                let judged = scores.len();
                let mut slots: Vec<Option<EsHit>> = hits.drain(..).map(Some).collect();
                let mut out = Vec::with_capacity(slots.len());
                for s in &ordered {
                    if let Some(mut h) = slots.get_mut(s.ordinal).and_then(Option::take) {
                        // Unjudged hits carry a negative sort key internally;
                        // they keep the engine's score rather than exposing it.
                        if s.score >= 0.0 {
                            h.score = Some(f64::from(s.score));
                        }
                        out.push(h);
                    }
                }
                // Past the window nothing was judged. With a threshold those
                // hits cannot be shown to clear it, so they are dropped; with
                // none they follow in the engine's order.
                if self.cfg.min_score.is_none() {
                    out.extend(slots.into_iter().skip(window).flatten());
                }
                let pruned = window.saturating_sub(ordered.len());
                *hits = out;
                *max_score = hits.first().and_then(|h| h.score);
                json!({
                    "applied": true,
                    "provider": self.cfg.provider,
                    "model": self.cfg.model,
                    "score_kind": "probability",
                    "window": window,
                    "judged": judged,
                    "pruned_below_min_score": pruned,
                    "partial_failures": partial_failures,
                })
            }
        };

        // The page was widened to the window; cut it back to what was asked.
        let page: Vec<EsHit> = std::mem::take(hits)
            .into_iter()
            .skip(from)
            .take(self.requested_size)
            .collect();
        *hits = page;
        Ok(info)
    }
}

/// HTTP status for a rerank fault that must reach the caller.
pub fn status_for(e: &RerankError) -> u16 {
    match e {
        RerankError::Config(_) | RerankError::UnknownProvider(_) => 400,
        // The server is not configured to reach the provider: not the
        // caller's mistake, and not a gateway fault either.
        RerankError::MissingKey(_) => 503,
        RerankError::Transport(_)
        | RerankError::Status { .. }
        | RerankError::Malformed(_)
        | RerankError::Deadline { .. } => 502,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_a_question_only_from_simple_shapes() {
        assert_eq!(
            infer_query(&json!({"match": {"body": "vitamin d"}})).as_deref(),
            Some("vitamin d")
        );
        assert_eq!(
            infer_query(&json!({"match": {"body": {"query": "vitamin d"}}})).as_deref(),
            Some("vitamin d")
        );
        assert_eq!(
            infer_query(&json!({"multi_match": {"query": "q", "fields": ["a"]}})).as_deref(),
            Some("q")
        );
        assert_eq!(
            infer_query(&json!({"semantic": {"field": "body", "query": "q"}})).as_deref(),
            Some("q")
        );
        // No single question in these — must not guess.
        assert!(infer_query(&json!({"bool": {"must": [{"match": {"a": "x"}}]}})).is_none());
        assert!(infer_query(&json!({"match_all": {}})).is_none());
        assert!(infer_query(&json!({"term": {"a": "x"}})).is_none());
    }

    #[test]
    fn fault_statuses() {
        assert_eq!(status_for(&RerankError::Config("x".into())), 400);
        assert_eq!(status_for(&RerankError::UnknownProvider("x".into())), 400);
        assert_eq!(status_for(&RerankError::MissingKey("K")), 503);
        assert_eq!(
            status_for(&RerankError::Status {
                status: 401,
                body: String::new()
            }),
            502
        );
    }
}
