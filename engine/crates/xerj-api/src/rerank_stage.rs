//! The `rerank` stage of `_search`.
//!
//! Runs after the engine has produced and rendered its hits, and before the
//! response is assembled. It never touches the engine: it widens the requested
//! page to the rerank window, hands the window to a [`xerj_rerank::Provider`],
//! and reorders the rendered hits by what comes back.
//!
//! # What the stage owns, and what it leaves alone
//!
//! It owns exactly one thing: the order (and, with `rerank.min_score`, the
//! membership) of `hits.hits`. Everything else in the response stays the
//! engine's — `hits.total`, `aggregations`, `suggest`, `profile` all describe
//! the full match set, not the window. That is the split Meilisearch's
//! personalisation rerank makes too
//! (`crates/meilisearch/src/routes/indexes/search.rs:788-801`: only
//! `search_result.hits` is replaced, facets and the estimated total are left as
//! computed; approach adapted, no code copied).
//!
//! # The judge sees what the caller sees
//!
//! Candidate text is read from the hit as the response will carry it — the
//! projected `_source`, then the hit's `fields`. Nothing the response does not
//! return is sent to the provider. That is a privacy property as much as a
//! design one: reranking is the only search-time feature that sends document
//! text off the node (proxy embeddings send query text at search time and
//! document text at write time; the WAL tap sends writes; the full list is
//! `docs/RERANK.md`, "Every way data leaves a XERJ node", kept complete by
//! `xerj-rerank/tests/egress_inventory.rs`), and "exactly what you were about
//! to receive, no more" is a rule a caller can audit. The cost is that
//! `_source` filtering which removes the text also removes it from the judge,
//! so that combination is refused by name instead of being judged blind.
//!
//! Two rules follow from treating this as an egress control rather than a
//! convenience. `rerank.fields` is EXHAUSTIVE: naming `["body"]` sends the body
//! and nothing else, not even the title. And what was actually returned is
//! checked on the rendered hits, not predicted from the request — a named field
//! that contributed no text is reported as `_rerank.fields_without_text`, and a
//! window with no text at all is a 400, never a batch of blank documents.
//!
//! # Strictness
//!
//! The stage refuses what it cannot do coherently. An explicit `sort` means the
//! caller has already said how results are ordered; paging past the window
//! means pages would be cut from two different orderings. Both are a 400 rather
//! than quietly honoured in a way that misleads. Surfaces that do not run the
//! stage at all (`_msearch`, search templates, `_async_search`, scroll and its
//! `_search/scroll` continuation, `_rank_eval`, the native `/v1` search API and
//! the gRPC Search RPC) refuse a body carrying `rerank` for the same reason: dropping
//! the key silently returns lexical order to a caller who asked for something
//! else. `"rerank": null` is the same as no `rerank` key on every surface.
//!
//! # One kind of `_score` per response
//!
//! When the stage applies, `_score` is a probability on every hit that has
//! one and `null` on every hit that does not — a hit the provider returned no
//! verdict for, or one that was skipped as blank. The engine's BM25 value is
//! never left in `_score` next to probabilities: a client that sorts by
//! `_score` or scales by `hits.max_score` (Kibana's score bars, the
//! elasticsearch-py helpers) cannot tell a `1.63` BM25 from a `0.9`
//! probability, and `max_score` would be smaller than a later hit's score.
//! `null` is what Elasticsearch itself puts in `_score` when a hit has no
//! comparable score (a field sort without `track_scores`), so every client
//! already handles it. The count is reported as `_rerank.unjudged`.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use xerj_rerank::{
    Candidate, Deadline, Provider, ProviderSettings, RerankConfig, RerankError, RerankOutcome,
};

use crate::es_compat::EsSearchBody;
use crate::responses::EsHit;
use crate::state::AppState;

/// A validated rerank request, produced before the search runs.
pub struct RerankPlan {
    cfg: RerankConfig,
    query: String,
    requested_size: usize,
    from: usize,
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

/// `*`-glob match, the only wildcard `_source` filtering supports.
fn glob(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    // `split` always yields at least one piece: the literal head.
    let head = parts.next().unwrap_or_default();
    let Some(mut rest) = name.strip_prefix(head) else {
        return false;
    };
    let mut pieces: Vec<&str> = parts.collect();
    let Some(tail) = pieces.pop() else {
        // No `*` at all: the head had to be the whole name.
        return rest.is_empty();
    };
    for piece in pieces {
        match rest.find(piece) {
            Some(at) => rest = &rest[at + piece.len()..],
            None => return false,
        }
    }
    rest.ends_with(tail)
}

/// Whether a `_source` filter pattern keeps `field` — the field itself, an
/// ancestor object of it, or (for includes) a descendant of it.
fn pattern_covers(pattern: &str, field: &str) -> bool {
    glob(pattern, field)
        || field
            .match_indices('.')
            .any(|(i, _)| glob(pattern, &field[..i]))
}

/// Whether the request's `_source` clause leaves `field` in the response.
fn source_keeps(source: Option<&Value>, field: &str) -> bool {
    let strings = |v: &Value| -> Vec<String> {
        match v {
            Value::String(s) => vec![s.clone()],
            Value::Array(a) => a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
            _ => Vec::new(),
        }
    };
    let (includes, excludes) = match source {
        None | Some(Value::Bool(true)) | Some(Value::Null) => return true,
        Some(Value::Bool(false)) => return false,
        Some(v @ (Value::String(_) | Value::Array(_))) => (strings(v), Vec::new()),
        Some(Value::Object(o)) => (
            o.get("includes")
                .or_else(|| o.get("include"))
                .map(strings)
                .unwrap_or_default(),
            o.get("excludes")
                .or_else(|| o.get("exclude"))
                .map(strings)
                .unwrap_or_default(),
        ),
        Some(_) => return true,
    };
    if excludes.iter().any(|p| pattern_covers(p, field)) {
        return false;
    }
    includes.is_empty()
        || includes
            .iter()
            .any(|p| pattern_covers(p, field) || p.starts_with(&format!("{field}.")))
}

/// Whether the request's `fields` clause asks for `field`.
fn fields_requests(fields: Option<&Value>, field: &str) -> bool {
    let Some(Value::Array(arr)) = fields else {
        return false;
    };
    arr.iter().any(|entry| {
        let name = match entry {
            Value::String(s) => s.as_str(),
            Value::Object(o) => o.get("field").and_then(Value::as_str).unwrap_or(""),
            _ => "",
        };
        !name.is_empty() && glob(name, field)
    })
}

/// Prose collected for the judge, cut at `max_doc_chars` WHILE it is
/// collected.
///
/// The provider crate cuts title and text to `max_doc_chars` when it builds the
/// request, so nothing past that point is ever sent. The first draft collected
/// every returned string field whole, cloned the result into `sendable`, into
/// per-call batches and into each task, and only then cut it: on a window of
/// 300 documents of ~1 MB each that was roughly 0.45 GB of transient copies of
/// text that could never leave the node (measured in review). Stopping at the
/// budget here bounds every later copy by `window × 2 × max_doc_chars`
/// characters, and what is sent is character-for-character what it was.
struct Prose {
    buf: String,
    /// Characters still allowed.
    room: usize,
}

impl Prose {
    fn new(max_chars: usize) -> Self {
        Self {
            buf: String::new(),
            room: max_chars,
        }
    }

    /// Append one piece of prose, newline-separated from the last.
    fn push_str(&mut self, s: &str) {
        if self.room == 0 || s.trim().is_empty() {
            return;
        }
        if !self.buf.is_empty() {
            self.buf.push('\n');
            self.room -= 1;
            if self.room == 0 {
                return;
            }
        }
        let cut = xerj_rerank::clip(s, self.room);
        // `cut` is at most `room` characters; the count is only walked when
        // the piece fit whole.
        self.room = if cut.len() < s.len() {
            0
        } else {
            self.room - cut.chars().count()
        };
        self.buf.push_str(cut);
    }

    /// Append every string under `v` — a string, or an array of strings.
    /// Numbers, booleans, vectors and nested objects are not prose and are not
    /// sent.
    fn push(&mut self, v: &Value) {
        match v {
            Value::String(s) => self.push_str(s),
            Value::Array(items) => items
                .iter()
                .filter(|i| i.is_string())
                .for_each(|i| self.push(i)),
            _ => {}
        }
    }

    fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

/// `a.b.c` looked up first as a literal key, then as a path.
fn lookup<'a>(obj: &'a serde_json::Map<String, Value>, field: &str) -> Option<&'a Value> {
    if let Some(v) = obj.get(field) {
        return Some(v);
    }
    let (head, rest) = field.split_once('.')?;
    lookup(obj.get(head)?.as_object()?, rest)
}

impl RerankPlan {
    /// Validate the request and widen the page to the rerank window.
    ///
    /// Must run AFTER the URL parameters (`?sort=`, `?_source=`, `?size=`,
    /// `?from=`…) have been merged into `body`, or a `?sort=` slips past the
    /// refusal below. Returns `Ok(None)` when the body has no `rerank` block.
    /// `Err` carries the reason for a 400.
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
        // `_source: false` on its own returns no text, so there is certainly
        // nothing to judge and the search need not run. Beside a clause that
        // puts values on the hit — `fields`, `docvalue_fields`,
        // `stored_fields`, `script_fields` — it is a coherent request ("do not
        // return the source, return `body` through `fields`, judge on that")
        // and is let through to the post-render check in `apply`, which looks
        // at what actually came back. The first draft gated on `fields` alone
        // and refused `_source: false` + `docvalue_fields` with a message that
        // named `fields` as the only way, contradicting the comment below.
        let non_empty = |v: &Option<Value>| match v {
            Some(Value::Array(a)) => !a.is_empty(),
            Some(Value::Object(o)) => !o.is_empty(),
            Some(Value::String(s)) => !s.trim().is_empty(),
            _ => false,
        };
        let asks_for_fields = non_empty(&body.fields)
            || non_empty(&body.docvalue_fields)
            || non_empty(&body.stored_fields)
            || non_empty(&body.script_fields);
        if matches!(body.source, Some(Value::Bool(false))) && !asks_for_fields {
            return Err(
                "`rerank` needs document text: `_source: false` with no `fields` (or \
                 `docvalue_fields`, `stored_fields`, `script_fields`) clause leaves nothing \
                 to judge. Return the text through `fields`, or use `rerank.fields` to \
                 limit what is sent instead"
                    .into(),
            );
        }
        if body.size == 0 {
            return Err(
                "`rerank` cannot be combined with `size: 0`: the provider would be paid to \
                 judge documents the response does not return. Drop `rerank` for an \
                 aggregation-only request, or ask for at least one hit"
                    .into(),
            );
        }
        // `checked_add`, not `+`: `from` is the caller's, and `u64::MAX + 1`
        // wrapped to 0 in a release build (overflow checks are off there),
        // passed this check, paid for a whole window of judgements and
        // returned an empty page under a 200 — while the same request without
        // `rerank` is the engine's 400. `from` is zeroed below, so the engine's
        // own `from + size` guard never sees the caller's value; this is the
        // only place it can be refused.
        let page_end = body.from.checked_add(body.size);
        if page_end.is_none_or(|end| end > cfg.window) {
            return Err(format!(
                "`from` + `size` ({}) exceeds `rerank.window` ({}): results past the window \
                 keep the engine's order, so a page cut across the boundary would mix two \
                 rankings. Raise `rerank.window` or page within it",
                page_end.map_or_else(
                    || format!("{} + {}", body.from, body.size),
                    |end| end.to_string()
                ),
                cfg.window
            ));
        }
        // The judge reads the hit as the response carries it, so a field the
        // caller told us to judge but also told us not to return cannot be
        // judged. Say which one instead of sending blank documents.
        //
        // This is a prediction, and it is only made where it cannot be wrong:
        // `docvalue_fields`, `stored_fields` and `script_fields` also put values
        // on a hit, so when any of them is present the question is left to the
        // check on the rendered hits in `apply`.
        let other_channels = body.docvalue_fields.is_some()
            || body.stored_fields.is_some()
            || body.script_fields.is_some();
        if let (Some(fields), false) = (&cfg.fields, other_channels) {
            for f in fields {
                if !source_keeps(body.source.as_ref(), f)
                    && !fields_requests(body.fields.as_ref(), f)
                {
                    return Err(format!(
                        "`rerank.fields` names `{f}`, but the `_source` filter removes it from \
                         the response and `fields` does not ask for it. The judge only sees \
                         what the response returns, so nothing would be left to judge: include \
                         `{f}` in `_source`, or request it through `fields`"
                    ));
                }
            }
        }

        let query = match cfg.query.clone() {
            Some(q) => q,
            None => body.query.as_ref().and_then(infer_query).ok_or_else(|| {
                "`rerank.query` is required: the search query is not a shape a single \
                 question can be read from (supported for inference: match, match_phrase, \
                 multi_match, semantic, simple_query_string). A `bool`, a `hybrid`, a \
                 top-level `knn` with no text query, or no `query` at all needs the question \
                 spelled out"
                    .to_string()
            })?,
        };

        // `rerank.query` was checked against its ceiling by the parser; a
        // question read out of the search query is the same string on the same
        // wire and gets the same ceiling.
        if cfg.query.is_none() {
            xerj_rerank::check_inferred_question(&query).map_err(|e| e.to_string())?;
        }

        let requested_size = body.size;
        let from = body.from;
        body.size = body.size.max(cfg.window);
        // The stage pages the reranked window itself; the engine must return
        // the window from the top.
        body.from = 0;

        Ok(Some(Self {
            cfg,
            query,
            requested_size,
            from,
        }))
    }

    /// The page the caller asked for, before the stage widened it. The search
    /// handler restores these on `body` for everything downstream that reads
    /// them (response hints, `terminated_early`), once the engine has run.
    pub fn requested_page(&self) -> (usize, usize) {
        (self.from, self.requested_size)
    }

    /// How many of the engine's top hits the stage asked for. The search
    /// handler checks it against each participating index's
    /// `index.max_result_window` before the engine runs, so the caller hears
    /// "`rerank.window` (30) exceeds `index.max_result_window` (20)" rather
    /// than the engine's `from + size > max_result_window` about a page size
    /// they never sent.
    pub fn window(&self) -> usize {
        self.cfg.window
    }

    /// Collect the prose a hit returns under one field name: `_source` first,
    /// then the hit's `fields` — which is how fetched-not-stored values get
    /// judged.
    fn field_text(hit: &EsHit, name: &str, out: &mut Prose) {
        let src = hit.source.as_ref().and_then(Value::as_object);
        if let Some(v) = src
            .and_then(|o| lookup(o, name))
            .or_else(|| hit.fields.as_ref().and_then(|f| f.get(name)))
        {
            if name == xerj_query::executor::PASSAGE_RESPONSE_FIELD {
                // `_passage` is `[{field, ordinal, start_offset, end_offset,
                // text, page?}]`, not a string: the prose is its `text`. The
                // one object shape that is read, because it is the engine's own
                // and its `text` is by construction a slice of a returned field.
                for passage in v.as_array().into_iter().flatten() {
                    if let Some(text) = passage.get("text") {
                        out.push(text);
                    }
                }
            } else {
                out.push(v);
            }
        }
    }

    fn candidate(&self, ordinal: usize, hit: &EsHit) -> Candidate {
        // `rerank.fields` is an egress control, so it is exhaustive: a caller
        // who wrote `["body"]` has said what may leave the machine, and the
        // title is not on that list. Unrestricted, the title is promoted to its
        // own slot because a judge reads it differently from body text.
        let title_allowed = match &self.cfg.fields {
            None => true,
            Some(fields) => fields.iter().any(|f| f == "title"),
        };
        // Title and text are each cut at `max_doc_chars` as they are
        // collected — see [`Prose`].
        let mut title = Prose::new(self.cfg.max_doc_chars);
        if title_allowed {
            Self::field_text(hit, "title", &mut title);
        }

        let mut text = Prose::new(self.cfg.max_doc_chars);
        match &self.cfg.fields {
            Some(fields) => {
                for f in fields.iter().filter(|f| f.as_str() != "title") {
                    Self::field_text(hit, f, &mut text);
                }
            }
            None => {
                // The matching passage goes FIRST when the response carries
                // one. Text is cut at `max_doc_chars`, so on a long document an
                // appended passage would be the part that gets cut — and it is
                // the part that says why this hit matched.
                let passage = xerj_query::executor::PASSAGE_RESPONSE_FIELD;
                Self::field_text(hit, passage, &mut text);
                let source = hit.source.as_ref().and_then(Value::as_object);
                if let Some(obj) = source {
                    obj.iter()
                        .filter(|(k, _)| k.as_str() != "title")
                        .for_each(|(_, v)| text.push(v));
                }
                // Then what the hit returns under `fields` — where `fields`,
                // `docvalue_fields`, `stored_fields` and `script_fields` put
                // their values. "The judge sees what the response returns"
                // includes them: the docs and `prepare`'s own refusal both say
                // "return the text through `fields`", and until this loop
                // existed `_source: false` + `fields: ["body"]` + `rerank: {}`
                // followed that advice into a second 400 ("nothing to judge").
                // A name `_source` already returned at the top level is
                // skipped, so a value present in both is sent once. `fields`
                // is a `HashMap`; the names are sorted so the same response
                // always sends the same text (the cut at `max_doc_chars` would
                // otherwise fall on a different field from call to call).
                if let Some(returned) = &hit.fields {
                    let mut names: Vec<&String> = returned
                        .keys()
                        .filter(|k| {
                            k.as_str() != "title"
                                && k.as_str() != passage
                                && !source.is_some_and(|o| o.contains_key(k.as_str()))
                        })
                        .collect();
                    names.sort();
                    for name in names {
                        text.push(&returned[name]);
                    }
                }
            }
        }
        Candidate {
            ordinal,
            title: (!title.is_empty()).then_some(title.buf),
            text: text.buf,
        }
    }

    /// Fields named in `rerank.fields` that no hit in the window returned any
    /// prose for.
    ///
    /// Checked on the RENDERED hits rather than predicted from the request,
    /// because prediction was wrong once already: `prepare` reasons that a
    /// `fields` clause returns a value `_source` filtering removed, and the
    /// engine does not always do that. A named field that silently contributes
    /// nothing means the caller believes documents were judged on text the
    /// judge never saw — so it is reported, whatever the cause (a projection, a
    /// typo in the name, or a field these hits simply lack).
    fn fields_without_text(&self, window: &[EsHit]) -> Vec<String> {
        let Some(fields) = &self.cfg.fields else {
            return Vec::new();
        };
        fields
            .iter()
            .filter(|f| {
                window.iter().all(|h| {
                    // One character is enough to know the field had prose.
                    let mut probe = Prose::new(1);
                    Self::field_text(h, f, &mut probe);
                    probe.is_empty()
                })
            })
            .cloned()
            .collect()
    }

    /// Rerank `hits` in place and return the `_rerank` block for the response.
    ///
    /// `Err` means a fault the caller must see (see `xerj_rerank::Policy`);
    /// degradable causes come back as `Ok` with `"applied": false`.
    pub async fn apply(
        &self,
        settings: &ProviderSettings,
        hits: &mut Vec<EsHit>,
        max_score: &mut Option<f64>,
    ) -> Result<Value, RerankError> {
        let started = Instant::now();
        let window = self.cfg.window.min(hits.len());
        let all: Vec<Candidate> = hits[..window]
            .iter()
            .enumerate()
            .map(|(i, h)| self.candidate(i, h))
            .collect();
        // A hit with no prose at all is not sent: a blank document costs a
        // judgement and the verdict on nothing means nothing.
        let sendable: Vec<Candidate> = all
            .iter()
            .filter(|c| c.title.is_some() || !c.text.trim().is_empty())
            .cloned()
            .collect();
        let skipped_no_text = all.len() - sendable.len();
        let fields_without_text = self.fields_without_text(&hits[..window]);

        // Misconfiguration surfaces whether or not this search had hits.
        let provider = Provider::from_settings(&self.cfg, settings)?;

        if window > 0 && sendable.is_empty() {
            return Err(RerankError::Config(format!(
                "nothing to judge: none of the top {window} hits carries a string field in \
                 the response{}. The judge only sees what the response returns — name the \
                 text field(s) in `rerank.fields` and make sure `_source` (or `fields`) \
                 returns them",
                match &self.cfg.fields {
                    Some(f) => format!(" under `rerank.fields` {f:?}"),
                    None => String::new(),
                }
            )));
        }

        let deadline = Deadline::new(self.cfg.timeout.max(Duration::from_millis(1)));
        let outcome = provider
            .rerank(&self.query, &sendable, &self.cfg, &deadline)
            .await?;

        let mut info = match outcome {
            RerankOutcome::Degraded { reason } => {
                tracing::warn!(%reason, "rerank degraded; engine order kept");
                json!({
                    "applied": false,
                    "reason": reason,
                    "provider": self.cfg.provider,
                    "score_kind": "engine",
                })
            }
            RerankOutcome::Reordered {
                scores,
                partial_failures,
                usage,
            } => {
                // A verdict counts only for a document that was actually sent.
                // The provider is a third party: an answer keyed to a hit that
                // was skipped as blank, or to a key nobody sent, would otherwise
                // score a document the judge never saw and inflate `judged` —
                // which is the operator's billing meter. The provider crate
                // already keeps one verdict per document per batch; this is
                // the same rule applied to whatever `Provider` arm answered.
                let sent: std::collections::HashSet<usize> =
                    sendable.iter().map(|c| c.ordinal).collect();
                let mut seen = std::collections::HashSet::with_capacity(scores.len());
                let scores: Vec<xerj_rerank::Scored> = scores
                    .into_iter()
                    .filter(|s| sent.contains(&s.ordinal) && seen.insert(s.ordinal))
                    .collect();
                // Documents were sent and not one came back with a verdict:
                // the provider answered, but not about anything it was asked.
                // That is the engine's order, and the caller must be told so —
                // `applied: true` over untouched BM25 scores was the silent
                // fake this stage exists to avoid, and it counted as "applied"
                // on the operator's meter.
                if !sendable.is_empty() && scores.is_empty() {
                    let reason = "provider returned no verdict for any document that was sent";
                    tracing::warn!(reason, "rerank degraded; engine order kept");
                    return self.finish(
                        started,
                        hits,
                        json!({
                            "applied": false,
                            "reason": reason,
                            "provider": self.cfg.provider,
                            "score_kind": "engine",
                        }),
                    );
                }
                let ordered = xerj_rerank::apply_scores(&all, &scores, self.cfg.min_score);
                let judged = scores.len();
                let below_min = match self.cfg.min_score {
                    Some(min) => scores.iter().filter(|s| s.score < min).count(),
                    None => 0,
                };
                let mut slots: Vec<Option<EsHit>> = hits.drain(..).map(Some).collect();
                let mut out = Vec::with_capacity(slots.len());
                let mut unjudged = 0usize;
                for s in &ordered {
                    if let Some(mut h) = slots.get_mut(s.ordinal).and_then(Option::take) {
                        // Unjudged hits carry a negative sort key internally.
                        if s.score >= 0.0 {
                            let p = s.score;
                            // `explain` described the engine's score. Keep it,
                            // under a node that says where `_score` now comes
                            // from — an `_explanation.value` that disagrees
                            // with `_score` would be a lie by omission.
                            if let Some(engine) = h.explanation.take() {
                                h.explanation = Some(json!({
                                    "value": p,
                                    "description": format!(
                                        "rerank: relevance probability from provider `{}` \
                                         (model `{}`); replaces the engine score explained below",
                                        self.cfg.provider, self.cfg.model
                                    ),
                                    "details": [engine],
                                }));
                            }
                            h.score = Some(p);
                        } else {
                            // No verdict. `_score` is `null`, never the engine's
                            // BM25 value beside probabilities (see the module
                            // doc). The explanation, if any, still describes
                            // the engine score, and says so.
                            unjudged += 1;
                            if let Some(engine) = h.explanation.take() {
                                let engine_value = engine.get("value").cloned();
                                h.explanation = Some(json!({
                                    "value": engine_value,
                                    "description": format!(
                                        "rerank: no verdict from provider `{}` for this hit; \
                                         `_score` is null and the engine score is explained below",
                                        self.cfg.provider
                                    ),
                                    "details": [engine],
                                }));
                            }
                            h.score = None;
                        }
                        out.push(h);
                    }
                }
                // Past the window nothing was judged. With a threshold those
                // hits cannot be shown to clear it, so they are dropped; with
                // none they follow in the engine's order, also with no score.
                if self.cfg.min_score.is_none() {
                    for mut h in slots.into_iter().skip(window).flatten() {
                        h.score = None;
                        unjudged += 1;
                        out.push(h);
                    }
                }
                let dropped_unjudged = match self.cfg.min_score {
                    Some(_) => window.saturating_sub(judged),
                    None => 0,
                };
                *hits = out;
                // Judged hits sort first and their probabilities descend, so
                // the first hit's score is the maximum of every `_score` on the
                // page — the invariant `max_score` is for.
                *max_score = hits.first().and_then(|h| h.score);
                json!({
                    "applied": true,
                    "provider": self.cfg.provider,
                    "model": self.cfg.model,
                    "score_kind": "probability",
                    "window": window,
                    "judged": judged,
                    "unjudged": unjudged,
                    "skipped_no_text": skipped_no_text,
                    "pruned_below_min_score": below_min,
                    "dropped_unjudged": dropped_unjudged,
                    "partial_failures": partial_failures,
                    "usage": {
                        "input_tokens": usage.input_tokens,
                        "output_tokens": usage.output_tokens,
                    },
                })
            }
        };
        if let Some(obj) = info.as_object_mut() {
            // Present only when there is something to say: a field the caller
            // asked to be judged on that contributed no text at all.
            if !fields_without_text.is_empty() && obj.get("applied") == Some(&json!(true)) {
                obj.insert("fields_without_text".into(), json!(fields_without_text));
            }
        }
        self.finish(started, hits, info)
    }

    /// Stamp the fields every `_rerank` block carries and cut the widened
    /// window back to the page the caller asked for.
    fn finish(
        &self,
        started: Instant,
        hits: &mut Vec<EsHit>,
        mut info: Value,
    ) -> Result<Value, RerankError> {
        if let Some(obj) = info.as_object_mut() {
            // The question that was judged — inferred or given. A caller who
            // let it be inferred should be able to see what was inferred.
            obj.insert("query".into(), json!(self.query));
            obj.insert(
                "took_ms".into(),
                json!(started.elapsed().as_millis() as u64),
            );
        }
        // The page was widened to the window; cut it back to what was asked.
        let page: Vec<EsHit> = std::mem::take(hits)
            .into_iter()
            .skip(self.from)
            .take(self.requested_size)
            .collect();
        *hits = page;
        Ok(info)
    }
}

/// Whether a raw search body carries a `rerank` block that means something.
///
/// `"rerank": null` is treated as absent everywhere: `EsSearchBody.rerank` is
/// an `Option<Value>` and serde reads `null` as `None`, so `_search` already
/// ignored it, while the surfaces that inspect the raw JSON (`_msearch`, the
/// templates, `_async_search`) saw a present key and refused. One rule.
pub fn carries_rerank(body: &Value) -> bool {
    body.get("rerank").is_some_and(|r| !r.is_null())
}

/// Count a request the stage refused before the search ran (`prepare` said no).
pub fn record_refused(metrics: &xerj_common::metrics::Metrics) {
    metrics
        .rerank_requests
        .with_label_values(&["refused"])
        .inc();
}

/// Count what [`RerankPlan::apply`] did.
///
/// `refused` means nothing reached the provider (400 / 403 / 503); `failed`
/// means it was called and the contract broke (502). The split matters to the
/// operator reading the counter: only `applied`, `degraded` and `failed` can
/// have cost money.
pub fn record_outcome(
    metrics: &xerj_common::metrics::Metrics,
    result: &Result<Value, RerankError>,
) {
    let outcome = match result {
        Ok(info) if info["applied"] == json!(true) => {
            metrics
                .rerank_documents_judged
                .inc_by(info["judged"].as_u64().unwrap_or(0));
            for (kind, key) in [("input", "input_tokens"), ("output", "output_tokens")] {
                metrics
                    .rerank_provider_tokens
                    .with_label_values(&[kind])
                    .inc_by(info["usage"][key].as_u64().unwrap_or(0));
            }
            "applied"
        }
        Ok(_) => "degraded",
        Err(e) if status_for(e) == 502 => "failed",
        Err(_) => "refused",
    };
    metrics.rerank_requests.with_label_values(&[outcome]).inc();
}

/// HTTP status for a rerank fault that must reach the caller.
pub fn status_for(e: &RerankError) -> u16 {
    match e {
        RerankError::Config(_) | RerankError::UnknownProvider(_) => 400,
        // The operator has forbidden it. Not the caller's syntax (400) and not
        // something a retry or a key fixes (503).
        RerankError::DisabledByOperator => 403,
        // The server is not configured to reach the provider: not the
        // caller's mistake, and not a gateway fault either.
        RerankError::MissingKey(_) => 503,
        RerankError::Transport(_)
        | RerankError::Status { .. }
        | RerankError::Malformed(_)
        | RerankError::Deadline { .. } => 502,
    }
}

/// ES-shaped error `type` for a rerank fault: a request the caller can fix is
/// an `illegal_argument_exception` like every other 400; the rest are XERJ's.
pub fn error_type_for(e: &RerankError) -> &'static str {
    match e {
        RerankError::Config(_) | RerankError::UnknownProvider(_) => "illegal_argument_exception",
        _ => "rerank_exception",
    }
}

/// Why an endpoint that does not run the rerank stage refuses the block. One
/// sentence, shared by the ES-shaped 400 below, the native REST API and gRPC,
/// so the three surfaces cannot tell a caller three different things.
pub fn unsupported_reason(endpoint: &str) -> String {
    format!(
        "`rerank` is not supported on {endpoint}: only `POST /{{index}}/_search` runs the rerank \
         stage. Send this search there, or remove the `rerank` block — it is refused here \
         rather than silently ignored"
    )
}

/// The 400 for an endpoint that does not run the rerank stage.
///
/// `xerj_query::parse_request` ignores keys it does not know, so without this a
/// `rerank` block on `_msearch`, a search template, `_async_search` or a scroll
/// vanished without a word and the caller got the engine's order back under a
/// 200 — the accepted-and-ignored class this project treats as a defect.
pub fn unsupported_on(endpoint: &str) -> Value {
    let reason = unsupported_reason(endpoint);
    json!({
        "error": {
            "root_cause": [{ "type": "illegal_argument_exception", "reason": reason }],
            "type": "illegal_argument_exception",
            "reason": reason,
        },
        "status": 400,
    })
}

/// `GET /_xerj/rerank` — whether a rerank provider is configured.
///
/// Reports THAT a key is set and where it came from; never the key, and the
/// endpoint only with userinfo, query and fragment stripped. Superuser-only,
/// like the rest of the `/_xerj/*` operator namespace (`authz.rs`).
pub async fn rerank_status(State(state): State<AppState>) -> Json<Value> {
    Json(status_document(&state.rerank))
}

/// The body of [`rerank_status`], split out so it is testable without a router.
pub fn status_document(settings: &ProviderSettings) -> Value {
    let defaults = RerankConfig::default();
    json!({
        "enabled": settings.enabled,
        "configured": settings.enabled && settings.has_key(),
        "providers": ["jev"],
        "api_key": {
            "set": settings.has_key(),
            "source": settings.key_source().map(|s| s.as_str()),
        },
        "endpoint": {
            "url": settings.endpoint_for_display(),
            "source": settings.endpoint_source().as_str(),
        },
        "defaults": {
            "provider": defaults.provider,
            "model": defaults.model,
            "window": defaults.window,
            "batch": defaults.batch,
            "max_concurrency": defaults.max_concurrency,
            "max_doc_chars": defaults.max_doc_chars,
            "timeout_ms": defaults.timeout.as_millis() as u64,
        },
        "limits": {
            "max_docs_per_call": xerj_rerank::JEV_MAX_DOCS_PER_CALL,
            "max_window": xerj_rerank::MAX_WINDOW,
            "max_concurrency": xerj_rerank::MAX_CONCURRENCY,
            "max_doc_chars": xerj_rerank::MAX_DOC_CHARS,
            "max_timeout_ms": xerj_rerank::MAX_TIMEOUT_MS,
            "max_instructions_chars": xerj_rerank::MAX_INSTRUCTIONS_CHARS,
            "max_query_chars": xerj_rerank::MAX_QUERY_CHARS,
            "max_model_chars": xerj_rerank::MAX_MODEL_CHARS,
            "max_fields": xerj_rerank::MAX_FIELDS,
            "max_field_name_chars": xerj_rerank::MAX_FIELD_NAME_CHARS,
        },
        "data_egress": xerj_rerank::DATA_EGRESS,
    })
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
        assert_eq!(status_for(&RerankError::DisabledByOperator), 403);
        assert_eq!(
            status_for(&RerankError::Status {
                status: 401,
                body: String::new()
            }),
            502
        );
        assert_eq!(
            error_type_for(&RerankError::Config("x".into())),
            "illegal_argument_exception"
        );
        assert_eq!(
            error_type_for(&RerankError::MissingKey("K")),
            "rerank_exception"
        );
    }

    #[test]
    fn source_filter_knows_what_survives() {
        let inc = json!(["title", "meta.*"]);
        assert!(source_keeps(Some(&inc), "title"));
        assert!(source_keeps(Some(&inc), "meta.summary"));
        assert!(!source_keeps(Some(&inc), "body"));

        let exc = json!({"excludes": ["body", "secret.*"]});
        assert!(!source_keeps(Some(&exc), "body"));
        assert!(!source_keeps(Some(&exc), "secret.note"));
        assert!(source_keeps(Some(&exc), "title"));

        // Excluding the parent object removes the child.
        let exc_parent = json!({"excludes": ["meta"]});
        assert!(!source_keeps(Some(&exc_parent), "meta.summary"));
        // Including a child keeps (part of) the parent.
        let inc_child = json!({"includes": ["meta.summary"]});
        assert!(source_keeps(Some(&inc_child), "meta"));

        assert!(source_keeps(None, "body"));
        assert!(source_keeps(Some(&json!(true)), "body"));
        assert!(!source_keeps(Some(&json!(false)), "body"));
        assert!(source_keeps(Some(&json!("bo*")), "body"));
    }

    #[test]
    fn glob_matches_like_source_filtering_does() {
        assert!(glob("body", "body"));
        assert!(!glob("body", "body2"));
        assert!(glob("*", "anything"));
        assert!(glob("meta.*", "meta.summary"));
        assert!(!glob("meta.*", "metadata"));
        assert!(glob("*_text", "body_text"));
        assert!(glob("a*b*c", "a-x-b-y-c"));
        assert!(!glob("a*b*c", "a-x-c"));
        assert!(glob("été*", "été-2026"), "multi-byte names must not panic");
    }

    #[test]
    fn text_is_strings_and_string_arrays_only() {
        let mut out = Prose::new(1_000);
        out.push(&json!("alpha"));
        out.push(&json!(["beta", 7, "gamma"]));
        out.push(&json!(42));
        out.push(&json!({"nested": "no"}));
        out.push(&json!([0.1, 0.2]));
        out.push(&json!("   "));
        assert_eq!(out.buf, "alpha\nbeta\ngamma");
    }

    /// Cutting while collecting must send exactly what collecting everything
    /// and cutting afterwards sent — for every budget, across piece
    /// boundaries, on the separator itself, and inside multi-byte text — while
    /// never holding more than the budget.
    #[test]
    fn prose_cut_while_collected_equals_collected_then_cut() {
        let pieces = [
            "héllo wörld",
            "  ",
            "второй кусок",
            "",
            "日本語のテキスト",
            "tail",
        ];
        let whole = pieces
            .iter()
            .filter(|p| !p.trim().is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        for budget in 0..=whole.chars().count() + 2 {
            let mut prose = Prose::new(budget);
            for p in pieces {
                prose.push_str(p);
            }
            assert_eq!(
                prose.buf,
                xerj_rerank::clip(&whole, budget),
                "budget {budget}"
            );
            assert!(prose.buf.chars().count() <= budget, "budget {budget}");
        }
    }

    fn hit(source: Value, fields: Option<Value>) -> EsHit {
        let mut value = json!({
            "_index": "i", "_id": "1", "_score": 1.0, "_source": source,
            "matched_queries": null
        });
        if let Some(f) = fields {
            value["fields"] = f;
        }
        serde_json::from_value(value).expect("an EsHit")
    }

    fn plan(rerank: Value) -> RerankPlan {
        RerankPlan {
            cfg: RerankConfig::from_json(&rerank).expect("a valid block"),
            query: "q".into(),
            requested_size: 10,
            from: 0,
        }
    }

    /// A megabyte-sized field costs the stage `max_doc_chars`, not a megabyte:
    /// the candidate is what gets cloned into batches and tasks.
    #[test]
    fn a_candidate_never_holds_more_than_max_doc_chars_of_each_part() {
        let big = "x".repeat(1_000_000);
        let h = hit(json!({"title": big, "body": big, "more": [big, big]}), None);
        let c = plan(json!({"max_doc_chars": 50})).candidate(0, &h);
        assert_eq!(c.title.as_deref().map(|t| t.chars().count()), Some(50));
        assert_eq!(c.text.chars().count(), 50);
        let c = plan(json!({"max_doc_chars": 50, "fields": ["more", "body"]})).candidate(0, &h);
        assert_eq!(c.title, None, "`rerank.fields` is exhaustive");
        assert_eq!(c.text.chars().count(), 50);
    }

    /// Default mode reads `_source`, then the hit's `fields` for names
    /// `_source` did not already return — once, and in a stable order.
    #[test]
    fn default_mode_reads_returned_fields_once_and_in_name_order() {
        let h = hit(
            json!({"title": "T", "body": "from source"}),
            Some(json!({
                "body": ["from source"],
                "zeta": ["last"],
                "alpha": ["first"],
                "n": [7],
                "title": ["T"]
            })),
        );
        let c = plan(json!({})).candidate(0, &h);
        assert_eq!(c.title.as_deref(), Some("T"));
        assert_eq!(c.text, "from source\nfirst\nlast");

        // `_source: false`: everything comes from `fields`, title included.
        let mut h = hit(
            json!({}),
            Some(json!({"body": ["only here"], "title": ["T"]})),
        );
        h.source = None;
        let c = plan(json!({})).candidate(0, &h);
        assert_eq!(c.title.as_deref(), Some("T"));
        assert_eq!(c.text, "only here");
    }

    #[test]
    fn dotted_fields_resolve_as_a_literal_key_then_as_a_path() {
        let doc = json!({"a.b": "literal", "a": {"b": "path", "c": {"d": "deep"}}});
        let obj = doc.as_object().unwrap();
        assert_eq!(lookup(obj, "a.b"), Some(&json!("literal")));
        assert_eq!(lookup(obj, "a.c.d"), Some(&json!("deep")));
        assert_eq!(lookup(obj, "a.x"), None);
    }

    /// `xerj-common` validates the environment endpoint at boot and cannot
    /// link `xerj-rerank`, so it carries its own copy of the variable name.
    #[test]
    fn the_env_endpoint_variable_name_is_the_same_in_both_crates() {
        assert_eq!(
            xerj_common::config::RerankProviderConfig::ENV_ENDPOINT,
            xerj_rerank::JevProvider::ENV_ENDPOINT
        );
    }

    #[test]
    fn status_document_never_carries_the_key() {
        let s = ProviderSettings::with_key_and_endpoint(
            "sk-do-not-print",
            "https://u:p@judge.example/v1/systemone?k=v",
        );
        let doc = status_document(&s);
        let text = doc.to_string();
        assert!(!text.contains("sk-do-not-print"), "{text}");
        assert!(!text.contains("u:p@"), "{text}");
        assert!(!text.contains("k=v"), "{text}");
        assert_eq!(doc["configured"], true);
        assert_eq!(doc["api_key"]["set"], true);
        assert_eq!(doc["api_key"]["source"], "config");
        assert_eq!(doc["endpoint"]["url"], "https://judge.example/v1/systemone");

        let unset = status_document(&ProviderSettings::resolve(true, "", "", None, None));
        assert_eq!(unset["configured"], false);
        assert_eq!(unset["api_key"]["set"], false);
        assert!(unset["api_key"]["source"].is_null());
        assert_eq!(unset["endpoint"]["source"], "default");

        let off = status_document(&ProviderSettings::resolve(false, "k", "", None, None));
        assert_eq!(off["enabled"], false);
        assert_eq!(
            off["configured"], false,
            "a key on a node that forbids reranking is not a working configuration"
        );
    }
}
