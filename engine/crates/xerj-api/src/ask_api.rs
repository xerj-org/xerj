//! `POST /_ask` — prompt in, validated query DSL out (issue #1056).
//!
//! An agent sends `{index or pattern, prompt}` and gets back
//! `{query, plan[], confidence, indices[]}` where `query` is an ES query
//! clause it can paste straight into `POST /{index}/_search` as
//! `{"query": …}`. The point is tokens: the case study behind the issue
//! measured 9,982 output tokens for retrieval-assisted planning versus
//! 26,477 for hand-written DSL — this endpoint exists so an agent never
//! hand-writes DSL for a structured filter again.
//!
//! # Contract (the issue's hard lines, all enforced here)
//!
//! - **Zero invalid DSL out.** Every response is assembled into a full
//!   search body (`{"query": …}`) and passed through
//!   [`xerj_query::parse_request`] BEFORE the response is returned. A
//!   planner bug surfaces as a 422 naming the planner, never as a 200
//!   carrying a query the engine would 400 on.
//! - **Unresolved phrases are 422s naming them.** A value the index does
//!   not hold, a field that does not exist, a pattern that matches nothing,
//!   a multi-index pattern with no catalog to route by: each is a 422 whose
//!   body names the phrase. No silent guess, ever — the negative class in
//!   `benchmarks/ask-plan` is exactly this.
//! - **Deterministic.** Same request over the same data → byte-identical
//!   response. Every collection is ordered (`Vec`, `BTreeMap`) or reduced
//!   under an explicit total order, and the response carries no clock, no
//!   timings, no per-run ids (which is why there is no `took_ms`).
//! - **Index routing over `ax-*` by catalog description.** When the pattern
//!   matches several indices, each candidate is scored against its
//!   `autoindex-catalog` dataset description (slug, field names, examples,
//!   notes) and the top scorer wins (ties: lexicographic index name). The
//!   catalog is read through the engine's own index APIs — one search of
//!   the `autoindex-catalog` index — NOT through a `xerj-autoindex` crate
//!   dependency, which this crate deliberately does not have (the constant
//!   below mirrors `xerj_autoindex::catalog::CATALOG_INDEX`).
//! - **Field gating by one batched typed call.** The target index's field
//!   list comes from ONE `Index::schema()` read (typed), merged with the
//!   catalog's per-field specs when a dataset doc exists. No per-field
//!   round trips.
//! - **Values from a `terms` agg while cardinality allows it.** ONE batched
//!   aggregation fetches the distinct values of every candidate keyword
//!   field at once (≤ `TERMS_AGG_SIZE` buckets — the issue's ≤ 1,000 line;
//!   catalog `cardinality_est` above that excludes the field from the
//!   batch). A value found there is returned in its CANONICAL casing. When
//!   a field's distinct values overflow the agg (`sum_other_doc_count > 0`)
//!   or the catalog over-estimates it, the value is verified by a BM25
//!   `match` probe instead and the clause is a `match` — the "BM25 over
//!   distinct values" arm.
//! - **Numeric and date thresholds are never generated.** Every bound in a
//!   returned `range` is a literal the prompt supplied (or the day/year of
//!   a date literal the prompt supplied). No "last week", no invented
//!   buckets: a prompt whose threshold would have to be generated is a 422
//!   naming the phrase. `date_min`/`date_max` from catalog specs are the
//!   release-time "offer as choices" surface, not an input to planning.
//!
//! # What the planner is (honesty)
//!
//! A deterministic rule-based parser: date literals and year phrases,
//! comparator phrases (`or greater`, `deeper than`, `between … and …`,
//! `exactly`), a unit lexicon (`km`, `K`, `days`, `Earth radii`), a scale
//! lexicon (`million` → ×10⁶) and a concept lexicon (`magnitude` → `mag`,
//! `network` → `net`, `life expectancy` → `lifeExp`, `GDP per capita` →
//! `gdpPercap`, …) resolve field phrases by scored name matching against
//! the index's real fields; equality values resolve against the live value
//! map built by the batched agg. A slash-bearing token (`crates/app/src`) is
//! a path constraint: a keyword `prefix` on `ax_path` (then `path`,
//! `filepath`, `file`, `ax_file`), with a trailing slash when the last
//! segment is a directory so `src` does not also match `src2`. No such
//! field is a 422 naming the path — it is not dropped as decoration. It is
//! NOT an LLM and not fuzzy retrieval: anything it cannot ground in the
//! index is refused by name. `confidence`
//! reports how much soft matching the plan needed — 0.9 for a fully exact
//! plan, lower per fallback, 0.5 for a bare `match_all`.
//!
//! # Authorization posture
//!
//! `/_ask` names its index in the BODY, like `/_decide` and `/_msearch`; the
//! authz middleware classifies the path as a cluster verb and POST is not a
//! cluster read, so a scoped key is refused (fail-closed) unless it holds
//! the general surface — the exact posture of `POST /_decide`. Reads only:
//! this handler executes searches (the value agg, optional BM25 probes) and
//! never writes.
//!
//! # Not claimed here
//!
//! The p50 ≤ 300 ms CPU budget and the harness gates (macro result-set F1
//! ≥ 0.9 over ≥ 200 pairs, agent token parity) are measured by
//! `benchmarks/ask-plan` (PR #1071) at release time — nothing in this file
//! asserts them.

use std::collections::BTreeMap;
use std::mem;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::Datelike;
use serde_json::{json, Map, Value};
use xerj_engine::index::Index;
use xerj_query::parse_request;

use crate::state::AppState;

/// The autoindex catalog index, read here through the engine's own APIs.
/// Mirrors `xerj_autoindex::catalog::CATALOG_INDEX` — kept as a literal
/// because xerj-api intentionally does not depend on that crate.
const CATALOG_INDEX: &str = "autoindex-catalog";
/// The catalog doc kind that carries a dataset description.
const CATALOG_DATASET_KIND: &str = "dataset";

/// One prompt cannot make the node scan a novel; same philosophy as the
/// rerank stage's ceiling on caller-chosen cost.
const MAX_PROMPT_CHARS: usize = 4_000;
const MAX_PROMPT_TOKENS: usize = 400;
/// The issue's cardinality line: distinct values come from a `terms` agg
/// only while the field has at most this many of them.
const TERMS_AGG_SIZE: usize = 1_000;
/// Bound the batched value agg. Fields are ordered low-cardinality-first so
/// a wide index keeps its most filterable fields inside the batch.
const MAX_VALUE_FIELDS: usize = 24;
/// At most this many BM25 fallback probes per request (the
/// high-cardinality arm). Keeps the worst case one request, not one per
/// phrase.
const MAX_BM25_PROBES: usize = 4;

// ─────────────────────────────────────────────────────────────────────────────
// POST /_ask
// ─────────────────────────────────────────────────────────────────────────────

pub async fn ask(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let started = std::time::Instant::now();
    let obj = match body.as_object() {
        Some(o) => o,
        None => return bad_request("`/_ask` body must be a JSON object"),
    };
    let prompt_raw = match obj.get("prompt").and_then(Value::as_str) {
        Some(p) if !p.trim().is_empty() => p.to_string(),
        _ => return bad_request("`prompt` must be a non-empty string"),
    };
    if prompt_raw.len() > MAX_PROMPT_CHARS {
        return bad_request(&format!(
            "`prompt` must be at most {MAX_PROMPT_CHARS} chars (got {})",
            prompt_raw.len()
        ));
    }
    let pattern = match obj.get("index") {
        None | Some(Value::Null) => "ax-*".to_string(),
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
        Some(_) => return bad_request("`index` must be a string (name or pattern)"),
    };

    // ── 1. Route: expand the pattern, then narrow by catalog description ──
    let mut matched: Vec<String> = state
        .engine
        .list_indices()
        .await
        .iter()
        .filter(|info| pattern.split(',').any(|p| glob_match(p.trim(), &info.name)))
        .map(|info| info.name.clone())
        .collect();
    matched.sort();
    if matched.is_empty() {
        return unresolved(
            std::slice::from_ref(&pattern),
            &format!(
                "`index` pattern `{pattern}` matches no index on this node; pass an index \
                 name (or a pattern like `ax-*`) that exists"
            ),
        );
    }

    let mut plan_steps: Vec<Value> = Vec::new();
    let catalog = load_catalog(&state, &matched).await;
    let index = match matched.len() {
        1 => {
            plan_steps.push(json!({
                "step": plan_steps.len() + 1,
                "action": "route",
                "index": matched[0],
                "how": "explicit-name",
            }));
            matched[0].clone()
        }
        _ => match route_by_description(&matched, &catalog, &prompt_raw) {
            Some((routed, score)) => {
                plan_steps.push(json!({
                    "step": plan_steps.len() + 1,
                    "action": "route",
                    "index": routed,
                    "how": "catalog-description-top1",
                    "score": (score * 1000.0).round() / 1000.0,
                    "candidates": matched.len(),
                }));
                routed
            }
            None => {
                let listed = matched
                    .iter()
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return unresolved(
                    std::slice::from_ref(&pattern),
                    &format!(
                        "`index` pattern `{pattern}` matches {} indices ({listed}) and this \
                         node has no `{CATALOG_INDEX}` dataset description covering them to \
                         route by — name the index explicitly",
                        matched.len()
                    ),
                );
            }
        },
    };

    // ── 2. Fields: ONE batched typed read (schema), merged with catalog ──
    let idx = match state.engine.get_index(&index) {
        Ok(i) => i,
        Err(e) => {
            return unresolved(
                std::slice::from_ref(&index),
                &format!("index `{index}` could not be opened: {e}"),
            )
        }
    };
    let schema = idx.schema().await;
    let mut fields: Vec<FieldView> = schema
        .fields
        .iter()
        .filter(|f| !f.name.starts_with('_'))
        .map(|f| FieldView::from_type(&f.name, &f.field_type))
        .collect();
    fields.sort_by(|a, b| a.name.cmp(&b.name));
    let spec = catalog.as_ref().and_then(|c| c.dataset_for(&index));
    let time_field_hint = spec.and_then(|s| s.time_field.clone());
    if let Some(spec) = &spec {
        for f in fields.iter_mut() {
            if let Some(s) = spec.field_for(&f.name) {
                f.cardinality_est = s.cardinality_est;
                f.examples = s.examples.clone();
                f.in_catalog = true;
            }
        }
    }
    // `count` is every top-level mapping field (the set a filter can name).
    // `xerj_map`'s `field_count` is that same set: catalog specs union the
    // live mapping, which is what surfaces `ax_*` locators the profiler
    // never sampled (#2120). `provenance_fields` names the `ax_*` ones so a
    // caller can see them without a second request.
    let mut provenance_fields: Vec<&str> = fields
        .iter()
        .filter(|f| f.name.starts_with("ax_"))
        .map(|f| f.name.as_str())
        .collect();
    provenance_fields.sort_unstable();
    plan_steps.push(json!({
        "step": plan_steps.len() + 1,
        "action": "fields",
        "count": fields.len(),
        "counted": "top-level mapping fields",
        "provenance_fields": provenance_fields,
        "source": if spec.is_some() { "catalog+mapping" } else { "mapping" },
    }));
    if fields.is_empty() {
        return unresolved(
            std::slice::from_ref(&index),
            &format!("index `{index}` declares no fields — `/_ask` cannot ground any phrase in it"),
        );
    }

    // ── 3. Values: ONE batched terms agg over the candidate keyword fields ──
    let mut ctx = PlanCtx {
        values: BTreeMap::new(),
        incomplete: BTreeMap::new(),
        soft: 0,
        constraints: Vec::new(),
        ignored: Vec::new(),
    };
    let value_fields = value_agg_fields(&fields);
    if !value_fields.is_empty() {
        // Catalog over-estimates exclude a field from the batch up front:
        // the issue's "> 1,000 → BM25, not a truncated agg" line.
        for f in fields.iter().filter(|f| value_fields.contains(&f.name)) {
            if f.cardinality_est.unwrap_or(0) > TERMS_AGG_SIZE as u64 {
                ctx.incomplete.insert(f.name.clone(), true);
            }
        }
        let batched: Vec<&String> = value_fields
            .iter()
            .filter(|n| !ctx.incomplete.contains_key(*n))
            .collect();
        if !batched.is_empty() {
            let mut aggs = Map::new();
            for (i, f) in batched.iter().enumerate() {
                aggs.insert(
                    format!("v{i}"),
                    json!({ "terms": { "field": f, "size": TERMS_AGG_SIZE } }),
                );
            }
            let body = json!({ "size": 0, "aggs": Value::Object(aggs) });
            let req = match parse_request(&body) {
                Ok(r) => r,
                Err(e) => {
                    return unresolved(
                        std::slice::from_ref(&index),
                        &format!("internal value aggregation would not parse: {e}"),
                    )
                }
            };
            match idx.search(&req).await {
                Ok(res) => {
                    if let Some(aggs_out) = &res.aggs {
                        for (i, f) in batched.iter().enumerate() {
                            read_value_agg(aggs_out.get(format!("v{i}")), f, &mut ctx);
                        }
                    }
                }
                Err(e) => {
                    return unresolved(
                        std::slice::from_ref(&index),
                        &format!("value aggregation over `{index}` failed: {e}"),
                    )
                }
            }
            plan_steps.push(json!({
                "step": plan_steps.len() + 1,
                "action": "values",
                "via": "terms-agg",
                "fields": batched.len(),
                "size": TERMS_AGG_SIZE,
            }));
        }
    }

    // ── 4. Plan ──
    let tokens = tokenize(&prompt_raw);
    let mut used = vec![false; tokens.len()];
    let outcome = plan_prompt(
        &tokens,
        &fields,
        time_field_hint.as_deref(),
        &mut used,
        &mut ctx,
        &idx,
    )
    .await;
    let (clause, confidence) = match outcome {
        Ok(p) => p,
        Err(r) => return unresolved(&r.0, &r.1),
    };
    for c in ctx.constraints.iter() {
        plan_steps.push(json!({
            "step": plan_steps.len() + 1,
            "action": "constraint",
            "query": c.clause,
            "via": c.via,
            "phrase": c.phrase,
        }));
    }
    for phrase in &ctx.ignored {
        plan_steps.push(json!({
            "step": plan_steps.len() + 1,
            "action": "ignored",
            "phrase": phrase,
            "why": "lowercase modifier with no field or value match; another constraint \
                    grounded the prompt",
        }));
    }

    // ── 5. Validate BEFORE returning: zero invalid DSL out ──
    let full = json!({ "query": clause, "size": 1 });
    if let Err(e) = parse_request(&full) {
        // Unreachable by construction — the clause is assembled from typed,
        // field-gated parts. Refuse rather than emit it.
        return unresolved(
            std::slice::from_ref(&prompt_raw),
            &format!(
                "internal planner error: the assembled query failed this node's own parser \
                 ({e}); nothing is returned. This is a bug in /_ask — please report it"
            ),
        );
    }
    plan_steps.push(json!({
        "step": plan_steps.len() + 1,
        "action": "validate",
        "parser": "xerj_query::parse_request",
        "passed": true,
    }));

    state
        .metrics
        .record_query(&index, "ask", started.elapsed().as_secs_f64());
    let confidence =
        (confidence - 0.1 * ctx.soft as f64 - 0.05 * ctx.ignored.len() as f64).clamp(0.05, 0.95);
    Json(json!({
        "query": clause,
        "plan": Value::Array(plan_steps),
        "confidence": (confidence * 100.0).round() / 100.0,
        "indices": [index],
    }))
    .into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Errors — every one names its cause and never guesses an answer
// ─────────────────────────────────────────────────────────────────────────────

fn bad_request(reason: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": { "type": "illegal_argument", "reason": reason } })),
    )
        .into_response()
}

/// The issue's refusal shape: 422, `unresolved_phrase`, the phrases named.
fn unresolved(phrases: &[String], reason: &str) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({
            "error": {
                "type": "unresolved_phrase",
                "reason": reason,
                "phrases": phrases,
            }
        })),
    )
        .into_response()
}

/// `(phrases to name, full reason)` — a planner refusal.
type Refusal = (Vec<String>, String);

// ─────────────────────────────────────────────────────────────────────────────
// Field views + the catalog
// ─────────────────────────────────────────────────────────────────────────────

/// One index field as the planner sees it: typed (from the schema — the one
/// batched call), optionally enriched by the catalog's `FieldSpec`.
struct FieldView {
    name: String,
    /// The comparison type: keyword/text/long/double/date/none(unusable).
    es_type: &'static str,
    /// The name split into comparison tokens: `pl_orbper` → [pl, orbper],
    /// `lifeExp` → [life, exp], `st_teff` → [st, teff].
    tokens: Vec<String>,
    /// lowercase alphanumeric-only name, the matching key.
    norm: String,
    cardinality_est: Option<u64>,
    examples: Vec<String>,
    in_catalog: bool,
}

impl FieldView {
    fn from_type(name: &str, t: &xerj_common::types::FieldType) -> Self {
        use xerj_common::types::FieldType as F;
        let es_type = match t {
            F::Text => "text",
            F::Keyword => "keyword",
            F::Long => "long",
            F::Double => "double",
            F::Boolean => "boolean",
            F::Date => "date",
            F::Ip => "ip",
            // Not filterable by this planner; kept in the view so the field
            // count in the plan is honest.
            _ => "none",
        };
        Self::new(name.to_string(), es_type)
    }

    fn new(name: String, es_type: &'static str) -> Self {
        let tokens = name_tokens(&name);
        let norm = norm_text(&name);
        Self {
            name,
            tokens,
            norm,
            es_type,
            cardinality_est: None,
            examples: Vec::new(),
            in_catalog: false,
        }
    }

    fn numeric(&self) -> bool {
        matches!(self.es_type, "long" | "double")
    }
    fn date(&self) -> bool {
        self.es_type == "date"
    }
    fn categorical(&self) -> bool {
        self.es_type == "keyword"
    }
}

/// The catalog as `/_ask` reads it: the dataset docs of `autoindex-catalog`
/// whose `index_name` is among the routed candidates.
struct Catalog {
    datasets: Vec<DatasetSpec>,
}

struct DatasetSpec {
    index: String,
    #[allow(dead_code)]
    slug: String,
    /// Every description token (slug, index name, field names, examples,
    /// notes), normed — the routing vocabulary.
    text: Vec<String>,
    fields: Vec<CatalogField>,
    time_field: Option<String>,
}

struct CatalogField {
    name: String,
    cardinality_est: Option<u64>,
    examples: Vec<String>,
}

impl DatasetSpec {
    fn field_for(&self, name: &str) -> Option<&CatalogField> {
        self.fields.iter().find(|f| f.name == name)
    }
}

impl Catalog {
    fn dataset_for(&self, index: &str) -> Option<&DatasetSpec> {
        self.datasets.iter().find(|d| d.index == index)
    }
}

/// Read the catalog through the engine's own index APIs: one search over
/// `autoindex-catalog` for dataset docs naming the candidate indices. A
/// missing catalog index or a failed search is NOT an error — it degrades to
/// mapping-only planning (and multi-index routing refuses instead of
/// guessing).
async fn load_catalog(state: &AppState, candidates: &[String]) -> Option<Catalog> {
    let idx = state.engine.get_index(CATALOG_INDEX).ok()?;
    let body = json!({
        "query": { "term": { "doc_kind": CATALOG_DATASET_KIND } },
        "size": 500,
        "_source": true,
    });
    let req = parse_request(&body).ok()?;
    let res = idx.search(&req).await.ok()?;
    let mut datasets = Vec::new();
    for hit in &res.hits {
        let src = &hit.source;
        let Some(index) = src.get("index_name").and_then(Value::as_str) else {
            continue;
        };
        if !candidates.iter().any(|c| c == index) {
            continue;
        }
        let slug = src
            .get("slug")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let time_field = src
            .get("time_field")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let mut fields = Vec::new();
        let mut text: Vec<String> = vec![norm_text(&slug), norm_text(index)];
        if let Some(Value::Array(notes)) = src.get("notes") {
            for n in notes {
                if let Some(s) = n.as_str() {
                    text.extend(tokenize_normed(s));
                }
            }
        }
        if let Some(Value::String(fj)) = src.get("fields_json") {
            // `fields_json` is a serialized Vec<FieldSpec> (autoindex
            // catalog.rs). Parsed as untyped JSON on purpose: no
            // xerj-autoindex dependency, and the planner only needs three
            // keys of it.
            if let Ok(Value::Array(list)) = serde_json::from_str::<Value>(fj) {
                for f in list {
                    let Some(name) = f.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    let ex = f
                        .get("examples")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect::<Vec<String>>()
                        })
                        .unwrap_or_default();
                    fields.push(CatalogField {
                        name: name.to_string(),
                        cardinality_est: f.get("cardinality_est").and_then(Value::as_u64),
                        examples: ex.clone(),
                    });
                    text.push(norm_text(name));
                    text.extend(ex.iter().map(|e| norm_text(e)));
                }
            }
        }
        datasets.push(DatasetSpec {
            index: index.to_string(),
            slug,
            text,
            fields,
            time_field,
        });
    }
    datasets.sort_by(|a, b| a.index.cmp(&b.index));
    if datasets.is_empty() {
        None
    } else {
        Some(Catalog { datasets })
    }
}

/// Score every candidate by description-token overlap with the prompt and
/// return the top scorer. `candidates` arrives name-sorted, so on a score
/// tie the lexicographically first wins — deterministic. `None` when no
/// candidate's description covers any prompt token: refuse rather than
/// guess.
fn route_by_description(
    candidates: &[String],
    catalog: &Option<Catalog>,
    prompt: &str,
) -> Option<(String, f64)> {
    let catalog = catalog.as_ref()?;
    let content: Vec<String> = tokenize_normed(prompt)
        .into_iter()
        .filter(|t| t.len() >= 3 && !FUNCTION_WORDS.contains(&t.as_str()))
        .collect::<Vec<_>>();
    if content.is_empty() {
        return None;
    }
    let mut best: Option<(String, f64)> = None;
    for cand in candidates {
        let Some(spec) = catalog.dataset_for(cand) else {
            continue;
        };
        let hits = content
            .iter()
            .filter(|t| spec.text.iter().any(|d| d == *t || d.contains(t.as_str())))
            .count();
        if hits == 0 {
            continue;
        }
        let score = hits as f64 / content.len() as f64;
        if best.as_ref().is_none_or(|(_, s)| score > *s) {
            best = Some((cand.clone(), score));
        }
    }
    best
}

// ─────────────────────────────────────────────────────────────────────────────
// Value map (the terms-agg arm) + the BM25 fallback
// ─────────────────────────────────────────────────────────────────────────────

/// Planner state carried across the passes. Ordered collections only — the
/// determinism contract.
struct PlanCtx {
    /// field → (normed value → canonical value).
    values: BTreeMap<String, BTreeMap<String, Value>>,
    /// Fields whose distinct values did NOT fit the agg (or the catalog
    /// over-estimates them) — the BM25 arm.
    incomplete: BTreeMap<String, bool>,
    /// Count of soft (fallback) resolutions — lowers confidence.
    soft: usize,
    /// Resolved constraints, in prompt order (assembly re-orders eq-first).
    constraints: Vec<Constraint>,
    /// Lowercase decoration tokens ignored, reported in the plan.
    ignored: Vec<String>,
}

/// One resolved constraint.
#[derive(Clone)]
struct Constraint {
    clause: Value,
    phrase: String,
    via: &'static str,
    /// range-shaped (sorts after every eq constraint, matching the gold
    /// ordering: eq clauses in prompt order, then ranges in prompt order).
    is_range: bool,
    /// Prompt token position of the constraint's first token.
    pos: usize,
}

/// Which fields the batched value agg covers: keyword fields ordered by
/// (catalog cardinality ascending, then name) — low cardinality is what
/// makes a field filterable, so a wide index keeps the right ones inside the
/// cap.
fn value_agg_fields(fields: &[FieldView]) -> Vec<String> {
    let mut cands: Vec<&FieldView> = fields.iter().filter(|f| f.categorical()).collect();
    cands.sort_by(|a, b| {
        a.cardinality_est
            .unwrap_or(u64::MAX)
            .cmp(&b.cardinality_est.unwrap_or(u64::MAX))
            .then_with(|| a.name.cmp(&b.name))
    });
    cands
        .into_iter()
        .take(MAX_VALUE_FIELDS)
        .map(|f| f.name.clone())
        .collect()
}

/// Fold one terms-agg result into the value map. Deterministic under bucket
/// ties: a norm collision reduces to the lexicographically smallest
/// canonical value.
fn read_value_agg(agg: Option<&Value>, field: &str, ctx: &mut PlanCtx) {
    let Some(buckets) = agg.and_then(|a| a.get("buckets")).and_then(Value::as_array) else {
        return;
    };
    let other = agg
        .and_then(|a| a.get("sum_other_doc_count"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let map = ctx.values.entry(field.to_string()).or_default();
    for b in buckets {
        let Some(key) = b.get("key") else { continue };
        let canon = match key {
            Value::String(s) => Value::String(s.clone()),
            other => other.clone(),
        };
        let norm = norm_value(key);
        let replace = match map.get(&norm) {
            None => true,
            Some(Value::String(prev)) => Some(prev.as_str()) < canon.as_str(),
            Some(_) => false,
        };
        if replace {
            map.insert(norm, canon);
        }
    }
    if other > 0 {
        ctx.incomplete.insert(field.to_string(), true);
    }
}

impl PlanCtx {
    /// Exact value lookup for one field.
    fn lookup_exact(&self, field: &str, phrase_normed: &str) -> Option<Value> {
        self.values.get(field)?.get(phrase_normed).cloned()
    }

    /// Prefix value lookup for one field: the prompt's adjectival form of a
    /// stored value ("European" → "Europe"). Allowed only when the shared
    /// prefix is ≥ 4 chars AND both sides are digit-free — a digit makes a
    /// value an identifier, and "v9999" → "v999" would be a guess about a
    /// DIFFERENT identifier, not a spelling of the same one. Deterministic:
    /// the smallest matching key.
    fn lookup_prefix(&self, field: &str, phrase_normed: &str) -> Option<Value> {
        let has_digit = |s: &str| s.chars().any(|c| c.is_ascii_digit());
        if has_digit(phrase_normed) {
            return None;
        }
        let map = self.values.get(field)?;
        let mut best: Option<(&String, &Value)> = None;
        for (k, v) in map {
            if k.len() >= 4
                && !has_digit(k)
                && phrase_normed.starts_with(k)
                && best.is_none_or(|(bk, _)| k < bk)
            {
                best = Some((k, v));
            }
        }
        best.map(|(_, v)| v.clone())
    }

    /// The field's value map is complete (every distinct value seen).
    fn complete(&self, field: &str) -> bool {
        !self.incomplete.contains_key(field)
    }

    /// BM25 fallback for a field whose values did not fit the agg: one
    /// `match` probe; a hit means the phrase exists in the field and a
    /// `match` clause is honest.
    async fn bm25_probe(&self, idx: &Arc<Index>, field: &str, phrase: &str) -> Option<Value> {
        let body = json!({ "query": { "match": { field: phrase } }, "size": 0 });
        let req = parse_request(&body).ok()?;
        let res = idx.search(&req).await.ok()?;
        (res.total.value > 0).then(|| match_clause(field, phrase))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Clause builders (json! cannot take dynamic keys safely; these can)
// ─────────────────────────────────────────────────────────────────────────────

fn term_clause(field: &str, value: Value) -> Value {
    let mut inner = Map::new();
    inner.insert(field.to_string(), value);
    let mut outer = Map::new();
    outer.insert("term".to_string(), Value::Object(inner));
    Value::Object(outer)
}

fn match_clause(field: &str, value: &str) -> Value {
    let mut inner = Map::new();
    inner.insert(field.to_string(), json!(value));
    let mut outer = Map::new();
    outer.insert("match".to_string(), Value::Object(inner));
    Value::Object(outer)
}

fn range_clause_1(field: &str, op: &str, value: Value) -> Value {
    let mut bound = Map::new();
    bound.insert(op.to_string(), value);
    let mut inner = Map::new();
    inner.insert(field.to_string(), Value::Object(bound));
    let mut outer = Map::new();
    outer.insert("range".to_string(), Value::Object(inner));
    Value::Object(outer)
}

fn prefix_clause(field: &str, value: &str) -> Value {
    let mut inner = Map::new();
    inner.insert(field.to_string(), json!(value));
    let mut outer = Map::new();
    outer.insert("prefix".to_string(), Value::Object(inner));
    Value::Object(outer)
}

fn range_clause_2(field: &str, lo: Value, lo_op: &str, hi: Value, hi_op: &str) -> Value {
    let mut bound = Map::new();
    bound.insert(lo_op.to_string(), lo);
    bound.insert(hi_op.to_string(), hi);
    let mut inner = Map::new();
    inner.insert(field.to_string(), Value::Object(bound));
    let mut outer = Map::new();
    outer.insert("range".to_string(), Value::Object(inner));
    Value::Object(outer)
}

// ─────────────────────────────────────────────────────────────────────────────
// Lexicons
// ─────────────────────────────────────────────────────────────────────────────

/// Words that carry grammar, not content. Never part of a field phrase, a
/// value, or a leftover run. Comparator vocabulary lives in the grammar
/// tables and is consumed there; listing it here too is safe because the
/// comparator passes run first.
const FUNCTION_WORDS: &[&str] = &[
    "the",
    "a",
    "an",
    "all",
    "every",
    "any",
    "each",
    "that",
    "which",
    "who",
    "whose",
    "with",
    "of",
    "for",
    "in",
    "on",
    "at",
    "by",
    "via",
    "from",
    "to",
    "and",
    "or",
    "is",
    "are",
    "was",
    "were",
    "be",
    "been",
    "than",
    "then",
    "there",
    "their",
    "its",
    "it",
    "this",
    "these",
    "those",
    "where",
    "when",
    "during",
    "through",
    "into",
    "reported",
    "discovered",
    "discovery",
    "found",
    "first",
    "observed",
    "happened",
    "occurred",
    "have",
    "has",
    "had",
    "having",
    "did",
    "does",
    "do",
    "please",
    "give",
    "show",
    "list",
    "find",
    "me",
    "only",
    "also",
    "both",
    "utc",
    "gmt",
    "between",
    "greater",
    "less",
    "more",
    "fewer",
    "most",
    "least",
    "exactly",
    "equal",
    "equals",
    "over",
    "under",
    "above",
    "below",
    "up",
    "down",
    "onward",
    "onwards",
    "later",
    "earlier",
    "inclusive",
    "including",
    "strictly",
    "stronger",
    "weaker",
    "deeper",
    "shallower",
    "hotter",
    "colder",
    "cooler",
    "warmer",
    "larger",
    "smaller",
    "bigger",
    "longer",
    "shorter",
    "higher",
    "lower",
    "exceeding",
    "exceeded",
    "exceeds",
    "newer",
    "older",
    "million",
    "billion",
    "thousand",
];

/// Generic head nouns / droppable modifiers. These never name a field or a
/// value; they end a leftover run so the value phrase inside survives.
const GENERIC_WORDS: &[&str] = &[
    "events",
    "event",
    "records",
    "record",
    "rows",
    "row",
    "documents",
    "document",
    "docs",
    "doc",
    "items",
    "item",
    "entries",
    "entry",
    "planets",
    "planet",
    "countries",
    "country",
    "systems",
    "system",
    "quakes",
    "quake",
    "network",
    "seismic",
    "method",
    "type",
    "known",
    "host",
    "hosts",
    "around",
    "orbiting",
    "orbit",
    "about",
    "probe",
];

/// Concept phrases → preferred field-name tokens, longest-first. All
/// matching is on normed (joined, lowercase, alphanumeric) phrases.
const CONCEPTS: &[(&str, &[&str])] = &[
    ("magnitudetype", &["magtype"]),
    ("magnitude", &["mag"]),
    ("depths", &["depth"]),
    ("depth", &["depth"]),
    ("network", &["net"]),
    ("lifeexpectancy", &["lifeexp"]),
    ("lifeexpectancies", &["lifeexp"]),
    ("orbitalperiods", &["orbper"]),
    ("orbitalperiod", &["orbper"]),
    ("periods", &["orbper", "period"]),
    ("period", &["orbper", "period"]),
    ("earthradii", &["rade", "radius"]),
    ("radii", &["rade", "radius"]),
    ("radius", &["rade", "radius"]),
    ("stars", &["snum"]),
    ("star", &["snum"]),
    ("planets", &["pnum"]),
    ("planet", &["pnum"]),
    ("temperature", &["teff", "temp"]),
    ("population", &["pop"]),
    ("gdppercapita", &["gdppercap"]),
    ("year", &["year"]),
    ("years", &["year"]),
    ("continent", &["continent"]),
    ("country", &["country"]),
    ("place", &["place"]),
    ("status", &["status"]),
    ("state", &["state"]),
];

/// Comparative adjectives that imply a quantity on their own.
const COMPARATIVE_CONCEPTS: &[(&str, &str)] = &[
    ("deeper", "depth"),
    ("shallower", "depth"),
    ("hotter", "temperature"),
    ("warmer", "temperature"),
    ("colder", "temperature"),
    ("cooler", "temperature"),
    ("stronger", "magnitude"),
    ("weaker", "magnitude"),
];

/// Unit tokens → preferred field tokens. The unit decides the field when the
/// prompt's field phrase is absent or names the wrong thing ("host stars
/// hotter than 6000 K" is a temperature, not a star count).
const UNITS: &[(&str, &[&str])] = &[
    ("km", &["depth"]),
    ("kilometer", &["depth"]),
    ("kilometers", &["depth"]),
    ("k", &["teff", "temp"]),
    ("kelvin", &["teff", "temp"]),
    ("celsius", &["teff", "temp"]),
    ("days", &["orbper", "period"]),
    ("day", &["orbper", "period"]),
    ("earthradii", &["rade", "radius"]),
    ("radii", &["rade", "radius"]),
];

/// Scale words directly after a number: "100 million" → 10⁸.
const SCALES: &[(&str, i64)] = &[
    ("thousand", 1_000),
    ("million", 1_000_000),
    ("billion", 1_000_000_000),
];

/// op phrases that START at the token AFTER a number (longest first).
const OP_AFTER: &[(&[&str], Op)] = &[
    (&["or", "greater", "than"], Op::Gte),
    (&["or", "greater"], Op::Gte),
    (&["or", "higher"], Op::Gte),
    (&["or", "more"], Op::Gte),
    (&["or", "later"], Op::Gte),
    (&["or", "newer"], Op::Gte),
    (&["or", "above"], Op::Gte),
    (&["and", "above"], Op::Gte),
    (&["and", "up"], Op::Gte),
    (&["onward"], Op::Gte),
    (&["onwards"], Op::Gte),
    (&["or", "less"], Op::Lte),
    (&["or", "lower"], Op::Lte),
    (&["or", "fewer"], Op::Lte),
    (&["or", "earlier"], Op::Lte),
    (&["or", "older"], Op::Lte),
    (&["and", "below"], Op::Lte),
    (&["up", "to", "and", "including"], Op::Lte),
];

/// op phrases that END at the token BEFORE a number (longest first).
const OP_BEFORE: &[(&[&str], Op)] = &[
    (&["up", "to", "and", "including"], Op::Lte),
    (&["at", "least"], Op::Gte),
    (&["at", "most"], Op::Lte),
    (&["greater", "than"], Op::Gt),
    (&["more", "than"], Op::Gt),
    (&["larger", "than"], Op::Gt),
    (&["bigger", "than"], Op::Gt),
    (&["hotter", "than"], Op::Gt),
    (&["warmer", "than"], Op::Gt),
    (&["deeper", "than"], Op::Gt),
    (&["longer", "than"], Op::Gt),
    (&["stronger", "than"], Op::Gt),
    (&["higher", "than"], Op::Gt),
    (&["older", "than"], Op::Gt),
    (&["newer", "than"], Op::Gt),
    (&["less", "than"], Op::Lt),
    // bare "before 2010" / "after 2018" — comparators in their own right
    (&["before"], Op::Lt),
    (&["after"], Op::Gt),
    (&["fewer", "than"], Op::Lt),
    (&["smaller", "than"], Op::Lt),
    (&["shorter", "than"], Op::Lt),
    (&["shallower", "than"], Op::Lt),
    (&["cooler", "than"], Op::Lt),
    (&["colder", "than"], Op::Lt),
    (&["exceeding"], Op::Gt),
    (&["exceeded"], Op::Gt),
    (&["exceeds"], Op::Gt),
    (&["at", "minimum"], Op::Gte),
    (&["at", "maximum"], Op::Lte),
    (&["exactly"], Op::Eq),
    (&["equal", "to"], Op::Eq),
    (&["equals"], Op::Eq),
    (&["over"], Op::Gt),
    (&["above"], Op::Gt),
    (&["under"], Op::Lt),
    (&["below"], Op::Lt),
    (&["beneath"], Op::Lt),
    (&["up", "to"], Op::Lte),
];

/// Connectors that make a bare 4-digit number a year ("discovered in 1992").
const YEAR_CONNECTORS: &[&str] = &[
    "in", "from", "during", "by", "between", "since", "on", "year", "years", "after", "before",
    "until",
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Op {
    Gt,
    Gte,
    Lt,
    Lte,
    Eq,
}

impl Op {
    fn key(self) -> &'static str {
        match self {
            Op::Gt => "gt",
            Op::Gte => "gte",
            Op::Lt => "lt",
            Op::Lte => "lte",
            Op::Eq => "eq",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tokens
// ─────────────────────────────────────────────────────────────────────────────

/// One prompt token, pre-normalised three ways.
#[derive(Clone, Debug)]
struct Tok {
    raw: String,
    norm: String,
    number: Option<f64>,
    /// Integral literals keep int fidelity ("4" stays 4, not 4.0).
    int_literal: Option<i64>,
    word_number: Option<f64>,
    /// concept implied by a spelled star-count word ("triple" → snum).
    concept: Option<&'static str>,
    capitalized: bool,
}

impl Tok {
    /// Numbers this planner will reason about. Non-finite literals ("1e999")
    /// are deliberately NOT numbers here: no clause may carry a bound the
    /// parser would reject, so they fall through to the leftover pass and are
    /// named in a 422 instead.
    fn is_num(&self) -> bool {
        self.number.is_some_and(|v| v.is_finite()) || self.word_number.is_some()
    }
    fn num_f64(&self) -> Option<f64> {
        self.number.or(self.word_number)
    }
    fn num_value(&self) -> Option<Value> {
        if let Some(i) = self.int_literal {
            return Some(json!(i));
        }
        self.num_f64()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
    }
    fn is_date(&self) -> bool {
        date_parts(&self.raw).is_some()
    }
}

fn tokenize(prompt: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    for w in prompt.split_whitespace() {
        let raw = w
            .trim_matches(|c: char| ".,;:()[]{}\"'!?".contains(c))
            .to_string();
        if raw.is_empty() {
            continue;
        }
        let lower = raw.to_lowercase();
        let norm = norm_text(&raw);
        let number = lower.parse::<f64>().ok();
        let int_literal = lower.parse::<i64>().ok();
        let (word_number, concept) = word_number(&norm);
        let capitalized = raw
            .chars()
            .next()
            .map(|c| {
                c.is_uppercase()
                    && raw
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            })
            .unwrap_or(false);
        out.push(Tok {
            raw,
            norm,
            number,
            int_literal,
            word_number,
            concept,
            capitalized,
        });
        if out.len() == MAX_PROMPT_TOKENS {
            break;
        }
    }
    out
}

/// Normed tokens of free text (catalog descriptions, notes).
fn tokenize_normed(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(norm_text)
        .filter(|t| !t.is_empty())
        .collect()
}

/// lowercase, ascii-folded, alphanumeric only — the comparison key for
/// fields, concepts, and values.
fn norm_text(s: &str) -> String {
    s.chars()
        .filter_map(|c| {
            if c.is_ascii_alphanumeric() {
                Some(c.to_ascii_lowercase())
            } else {
                None
            }
        })
        .collect()
}

fn norm_value(v: &Value) -> String {
    match v {
        Value::String(s) => norm_text(s),
        other => norm_text(&other.to_string()),
    }
}

/// field name → comparison tokens: split on `_`, `-`, `.`, and camelCase
/// (`pl_orbper` → [pl, orbper], `lifeExp` → [life, exp]).
fn name_tokens(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if c == '_' || c == '-' || c == '.' {
            if !cur.is_empty() {
                out.push(mem::take(&mut cur));
            }
            prev_lower = false;
        } else if c.is_uppercase() {
            if prev_lower && !cur.is_empty() {
                out.push(mem::take(&mut cur));
            }
            cur.push(c.to_ascii_lowercase());
            prev_lower = false;
        } else {
            cur.push(c);
            prev_lower = c.is_lowercase() || c.is_ascii_digit();
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Spelled numbers, plus the star-count words astronomy uses ("single",
/// "binary", "triple"… carry their own concept).
fn word_number(w: &str) -> (Option<f64>, Option<&'static str>) {
    // Hyphenated compounds like `triple-star`/`binary-star` norm to
    // `triplestar` and count the same way: the `-star` suffix is the unit
    // the number names.
    let stripped = w
        .strip_suffix("stars")
        .or_else(|| w.strip_suffix("star"))
        .unwrap_or(w);
    match stripped {
        "one" => (Some(1.0), None),
        "single" => (Some(1.0), Some("snum")),
        "two" => (Some(2.0), None),
        "binary" => (Some(2.0), Some("snum")),
        "three" => (Some(3.0), None),
        "triple" => (Some(3.0), Some("snum")),
        "four" => (Some(4.0), None),
        "quadruple" => (Some(4.0), Some("snum")),
        "five" => (Some(5.0), None),
        "quintuple" => (Some(5.0), Some("snum")),
        "six" => (Some(6.0), None),
        "seven" => (Some(7.0), None),
        "eight" => (Some(8.0), None),
        "nine" => (Some(9.0), None),
        "ten" => (Some(10.0), None),
        _ => (None, None),
    }
}

/// `2026-09-01` → a valid (y, m, d).
fn date_parts(word: &str) -> Option<(i32, u32, u32)> {
    let mut it = word.split('-');
    let y: i32 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    chrono::NaiveDate::from_ymd_opt(y, m, d)?;
    Some((y, m, d))
}

fn day_start(y: i32, m: u32, d: u32) -> String {
    format!("{y:04}-{m:02}-{d:02}T00:00:00Z")
}

fn next_day(y: i32, m: u32, d: u32) -> String {
    chrono::NaiveDate::from_ymd_opt(y, m, d)
        .and_then(|date| date.succ_opt())
        .map(|next| day_start(next.year(), next.month(), next.day()))
        .unwrap_or_else(|| day_start(y, m, d))
}

fn year_start(y: i64) -> String {
    format!("{y:04}-01-01T00:00:00Z")
}

fn next_year_start(y: i64) -> String {
    format!("{:04}-01-01T00:00:00Z", y + 1)
}

/// `*` wildcard match (the only wildcard the ES-compat surface promises for
/// index names; multiple stars each match any run, possibly empty —
/// `ax-*-2026-*` is a legal pattern). Everything else is literal. Classic
/// two-pointer backtrack: pure, allocation-free, deterministic.
fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = name.chars().collect();
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while si < s.len() {
        if pi < p.len() && p[pi] == s[si] {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = si;
            pi += 1;
        } else if star != usize::MAX {
            mark += 1;
            si = mark;
            pi = star + 1;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

// ─────────────────────────────────────────────────────────────────────────────
// Field-phrase resolution
// ─────────────────────────────────────────────────────────────────────────────

/// Concept lookup over a joined normed phrase → preferred field tokens.
fn concept_for(joined: &str) -> Option<&'static [&'static str]> {
    CONCEPTS
        .iter()
        .find(|(key, _)| {
            joined == *key
                || (joined.len() >= 4 && key.ends_with(joined))
                || (key.len() >= 4 && joined.ends_with(key))
        })
        .map(|(_, targets)| *targets)
}

/// Position (name order) of the first field carrying one of these targets and
/// passing the gate. A target matches a whole name token OR the full normed
/// name — concept tables speak both spellings: `mag` is a token of `mag`, but
/// `lifeexp`/`gdppercap` are the FULL names `lifeExp`/`gdpPercap`, whose
/// comparison tokens split differently ([life, exp], [gdp, percap]).
fn field_by_tokens(
    targets: &[&str],
    fields: &[FieldView],
    gate: impl Fn(&FieldView) -> bool,
) -> Option<usize> {
    fields.iter().position(|f| {
        gate(f)
            && targets
                .iter()
                .any(|t| f.tokens.iter().any(|ft| ft == t) || &f.norm == t)
    })
}

/// Scored name match between a joined normed phrase and one field.
fn name_score(joined: &str, phrase_tokens: &[String], f: &FieldView) -> Option<u32> {
    if joined == f.norm {
        return Some(1000);
    }
    if f.norm.len() >= 3 && joined.starts_with(&f.norm) {
        // mag ⊂ magnitude, pop ⊂ population, gdppercap ⊂ gdppercapita
        return Some(800 + f.norm.len() as u32);
    }
    if joined.len() >= 3 && f.norm.starts_with(joined) {
        return Some(700 + joined.len() as u32);
    }
    let matched = phrase_tokens
        .iter()
        .filter(|p| {
            f.tokens.iter().any(|ft| {
                ft == *p
                    || (ft.len() >= 3 && p.starts_with(ft))
                    || (p.len() >= 3 && ft.starts_with(p.as_str()))
            })
        })
        .count();
    if matched > 0 && matched == phrase_tokens.len() {
        return Some(600 + matched as u32);
    }
    None
}

/// Resolve a field phrase (token indexes) under a gate: concept table first,
/// then scored name matching. Ties resolve to the first field in name order
/// (fields arrive sorted) — deterministic.
fn resolve_field(
    toks: &[Tok],
    idxs: &[usize],
    fields: &[FieldView],
    gate: impl Fn(&FieldView) -> bool + Copy,
) -> Option<usize> {
    let phrase_tokens: Vec<String> = idxs.iter().map(|&i| toks[i].norm.clone()).collect();
    let joined = phrase_tokens.concat();
    if joined.is_empty() {
        return None;
    }
    if let Some(targets) = concept_for(&joined) {
        if let Some(pos) = fields
            .iter()
            .position(|f| gate(f) && targets.iter().any(|t| f.tokens.iter().any(|ft| ft == t)))
        {
            return Some(pos);
        }
    }
    let mut best: Option<(u32, usize)> = None;
    for (pos, f) in fields.iter().enumerate() {
        if !gate(f) {
            continue;
        }
        if let Some(score) = name_score(&joined, &phrase_tokens, f) {
            if best.is_none_or(|(bs, _)| score > bs) {
                best = Some((score, pos));
            }
        }
    }
    best.map(|(_, pos)| pos)
}

/// The year-ish field: a numeric field whose name carries "year" (gapminder
/// `year`, nasa `disc_year`), else a date field (then a year becomes a
/// year-wide date range — still only prompt-supplied bounds).
fn year_field(fields: &[FieldView]) -> Option<usize> {
    fields
        .iter()
        .position(|f| f.numeric() && f.tokens.iter().any(|t| t == "year"))
        .or_else(|| fields.iter().position(|f| f.date()))
}

/// The date field: the catalog's `time_field` when present, else date-typed
/// fields by name preference (time > date > timestamp) then name order.
fn date_field(fields: &[FieldView], hint: Option<&str>) -> Option<usize> {
    if let Some(h) = hint {
        if let Some(pos) = fields.iter().position(|f| f.name == h && f.date()) {
            return Some(pos);
        }
    }
    let pref = |f: &FieldView| {
        if f.tokens.iter().any(|t| t == "time") {
            0
        } else if f.tokens.iter().any(|t| t == "date") {
            1
        } else if f.tokens.iter().any(|t| t == "timestamp" || t == "ts") {
            2
        } else {
            3
        }
    };
    fields
        .iter()
        .enumerate()
        .filter(|(_, f)| f.date())
        .min_by_key(|(pos, f)| (pref(f), *pos))
        .map(|(pos, _)| pos)
}

// ─────────────────────────────────────────────────────────────────────────────
// The passes
// ─────────────────────────────────────────────────────────────────────────────

async fn plan_prompt(
    toks: &[Tok],
    fields: &[FieldView],
    time_field_hint: Option<&str>,
    used: &mut [bool],
    ctx: &mut PlanCtx,
    idx: &Arc<Index>,
) -> Result<(Value, f64), Refusal> {
    // Pass 0: the "multi-" quantifier ("multi-planet systems", "multi-star
    // systems") — "more than one" of the concept the suffix names. The bound
    // is the word's own meaning (as with "million" → 10⁶), not generated:
    // "multi" is strictly-greater-than one, which over integer counts is the
    // same set as ≥ 2.
    multi_pass(toks, fields, used, ctx);
    // Pass B first: comparator phrases claim their tokens, so "or greater"
    // never survives as a leftover value phrase.
    numeric_pass(toks, fields, used, ctx)?;
    // Pass A: date literals and bare years.
    date_pass(toks, fields, time_field_hint, used, ctx)?;
    // Pass P: a slash-bearing token is a path prefix, not a value. Claim it
    // before the value pass so `norm_text` (which strips `/`) cannot fold it
    // into an unrelated keyword, and before leftovers so it is not reported
    // as a dropped lowercase modifier (#2120).
    path_pass(toks, fields, used, ctx);
    // Pass C: equality from the value map (field+value splits, then values).
    let mut probes = 0usize;
    let leftovers = value_pass(toks, fields, used, ctx, idx, &mut probes).await;
    let grounded = !ctx.constraints.is_empty();
    settle_leftovers(toks, &leftovers, grounded, ctx)?;

    let mut constraints = ctx.constraints.clone();
    // Gold ordering, pinned by the harness: eq clauses in prompt order
    // first, then range clauses in prompt order. Deterministic either way,
    // but matching the reference makes exact-diff tests possible.
    constraints.sort_by_key(|c| (c.is_range, c.pos));
    let clause = match constraints.len() {
        0 => json!({ "match_all": {} }),
        1 => constraints.remove(0).clause,
        _ => {
            let filters: Vec<Value> = constraints.iter().map(|c| c.clause.clone()).collect();
            let mut inner = Map::new();
            inner.insert("filter".to_string(), Value::Array(filters));
            let mut outer = Map::new();
            outer.insert("bool".to_string(), Value::Object(inner));
            Value::Object(outer)
        }
    };
    let confidence = if ctx.constraints.is_empty() { 0.5 } else { 0.9 };
    Ok((clause, confidence))
}

/// `multi-<concept>` → `> 1` on the concept's field. Fires only when the
/// remainder names a concept with a numeric field in this index; anything
/// else ("multis") is left for the ordinary passes.
fn multi_pass(toks: &[Tok], fields: &[FieldView], used: &mut [bool], ctx: &mut PlanCtx) {
    for (i, t) in toks.iter().enumerate() {
        if used[i] {
            continue;
        }
        let Some(rest) = t.norm.strip_prefix("multi") else {
            continue;
        };
        if rest.len() < 4 {
            continue;
        }
        let Some(targets) = concept_for(rest) else {
            continue;
        };
        let Some(fpos) = field_by_tokens(targets, fields, |f| f.numeric()) else {
            continue;
        };
        let f = &fields[fpos];
        ctx.constraints.push(Constraint {
            clause: range_clause_1(&f.name, "gt", json!(1)),
            phrase: t.raw.clone(),
            via: "quantifier+concept",
            is_range: true,
            pos: i,
        });
        used[i] = true;
    }
}

/// Pass B: numeric comparisons. Every bound is a prompt literal; the scale
/// lexicon ("100 million" → 10⁸) is the only arithmetic, and it is the
/// prompt's own words.
fn numeric_pass(
    toks: &[Tok],
    fields: &[FieldView],
    used: &mut [bool],
    ctx: &mut PlanCtx,
) -> Result<(), Refusal> {
    let mut i = 0;
    while i < toks.len() {
        if used[i] || !toks[i].is_num() {
            i += 1;
            continue;
        }
        // A spelled star-count word carries its own field ("triple" → snum).
        if let Some(concept) = toks[i].concept {
            if let Some(f) =
                field_by_tokens(&[concept], fields, |f| f.numeric()).map(|pos| &fields[pos])
            {
                if let Some(v) = toks[i].num_value() {
                    ctx.constraints.push(Constraint {
                        clause: term_clause(&f.name, v),
                        phrase: toks[i].raw.clone(),
                        via: "literal+concept",
                        is_range: false,
                        pos: i,
                    });
                    used[i] = true;
                    i += 1;
                    continue;
                }
            }
        }

        // (1) pair: from/between … <num> and|to|through <num>
        if let Some((second, connector)) = pair_at(toks, used, i) {
            let (lo, _lo_x) = number_span(toks, i);
            let (hi, hi_x) = number_span(toks, second);
            // "magnitude from 4" hides the phrase before `from`;
            // "between magnitude 4" hides it after `between` — try both.
            let mut bwd = backward_phrase(toks, used, i);
            if bwd.is_empty() {
                if let Some(c) = connector {
                    bwd = backward_phrase(toks, used, c);
                }
            }
            let unit = unit_after(toks, second + 1 + hi_x).map(|(u, _)| u);
            let field = resolve_comparison(toks, &bwd, unit, None, &[], fields).or_else(|| {
                if is_yearish(&toks[i]) && is_yearish(&toks[second]) {
                    year_field(fields)
                } else {
                    None
                }
            });
            let Some(fpos) = field else {
                return Err(comparison_refusal(toks, &bwd, i));
            };
            let f = &fields[fpos];
            // A year pair against a date field stays prompt-supplied: the
            // years of the prompt, as year-wide bounds.
            let clause = if f.date() {
                let y1 = toks[i].int_literal.unwrap_or_default();
                let y2 = toks[second].int_literal.unwrap_or_default();
                range_clause_2(
                    &f.name,
                    json!(year_start(y1)),
                    "gte",
                    json!(next_year_start(y2)),
                    "lt",
                )
            } else {
                range_clause_2(&f.name, lo, "gte", hi, "lte")
            };
            let phrase = format!(
                "{}… {}",
                if bwd.is_empty() {
                    String::new()
                } else {
                    format!("{} ", phrase_text(toks, &bwd))
                },
                toks[second].raw
            );
            ctx.constraints.push(Constraint {
                clause,
                phrase: phrase.trim().to_string(),
                via: "range-pair",
                is_range: true,
                pos: bwd.first().copied().unwrap_or(i),
            });
            for &b in &bwd {
                used[b] = true;
            }
            for u in &mut used[i..=second + hi_x] {
                *u = true;
            }
            i = second + 1 + hi_x;
            continue;
        }

        // (2) op-after ("2.75 or greater", "2018 or later")
        let (val, sx) = number_span(toks, i);
        if let Some((olen, op)) = op_after_at(toks, i + 1 + sx) {
            let bwd = backward_phrase(toks, used, i);
            let unit = unit_after(toks, i + 1 + sx + olen).map(|(u, _)| u);
            let fwd = forward_phrase(toks, used, i + 1 + sx + olen, 3);
            let field = resolve_comparison(toks, &bwd, unit, None, &fwd, fields)
                .or_else(|| yearish_fallback(&toks[i], fields));
            let Some(fpos) = field else {
                return Err(comparison_refusal(toks, &bwd, i));
            };
            push_comparison(ctx, &fields[fpos], op, &toks[i], val, &bwd, i, toks);
            for u in &mut used[i..i + 1 + sx + olen] {
                *u = true;
            }
            for &b in &bwd {
                used[b] = true;
            }
            i += 1 + sx + olen;
            continue;
        }

        // (3) op-before ("deeper than 50", "at least 2 known planets"),
        // including the separated form where the field phrase sits between
        // the op and the number ("stronger than magnitude 3.5").
        let mut gap_hit: Option<Vec<usize>> = None;
        let mut op_hit = op_before_at(toks, used, i);
        if op_hit.is_none() {
            if let Some((start, olen, op, gap)) = op_before_gap_at(toks, used, i) {
                op_hit = Some((start, olen, op));
                gap_hit = Some(gap);
            }
        }
        if let Some((start, olen, op)) = op_hit {
            let gap = gap_hit.clone().unwrap_or_default();
            let bwd = backward_phrase(toks, used, start);
            let unit_hit = unit_after(toks, i + 1 + sx);
            let compar = comparative_concept(toks, start, olen);
            let fwd = forward_phrase(toks, used, i + 1 + sx, 3);
            let field =
                resolve_comparison(toks, &bwd, unit_hit.map(|(u, _)| u), compar, &fwd, fields)
                    .or_else(|| resolve_field(toks, &gap, fields, |f| f.numeric()))
                    .or_else(|| yearish_fallback(&toks[i], fields));
            let Some(fpos) = field else {
                return Err(comparison_refusal(toks, &bwd, i));
            };
            push_comparison(ctx, &fields[fpos], op, &toks[i], val, &bwd, i, toks);
            for u in &mut used[start..=i + sx] {
                *u = true;
            }
            if let Some((_, uidx)) = unit_hit {
                used[uidx] = true;
            }
            for &b in &fwd {
                used[b] = true;
            }
            for &b in &bwd {
                used[b] = true;
            }
            for &b in &gap {
                used[b] = true;
            }
            i += 1 + sx;
            continue;
        }

        // (4) bare number with a forward field phrase ("2 stars", "single
        // … " handled above). No comparator, no scale.
        let fwd = forward_phrase(toks, used, i + 1, 3);
        if let Some(fpos) = resolve_field(toks, &fwd, fields, |f| f.numeric()) {
            if let Some(v) = toks[i].num_value() {
                let f = &fields[fpos];
                let phrase = format!("{} {}", toks[i].raw, phrase_text(toks, &fwd));
                ctx.constraints.push(Constraint {
                    clause: term_clause(&f.name, v),
                    phrase,
                    via: "literal+field",
                    is_range: false,
                    pos: i,
                });
                used[i] = true;
                for &k in &fwd {
                    used[k] = true;
                }
                i += 1;
                continue;
            }
        }

        // Bare years fall through to the date pass; anything else is named
        // by the leftover pass.
        i += 1;
    }
    Ok(())
}

/// Emit one comparison constraint. Every bound is the prompt's own literal;
/// a year compared against a date field becomes year-wide bounds.
#[allow(clippy::too_many_arguments)]
fn push_comparison(
    ctx: &mut PlanCtx,
    f: &FieldView,
    op: Op,
    tok: &Tok,
    val: Value,
    bwd: &[usize],
    num_i: usize,
    toks: &[Tok],
) {
    let prefix = if bwd.is_empty() {
        String::new()
    } else {
        format!("{} ", phrase_text(toks, bwd))
    };
    if op == Op::Eq {
        ctx.constraints.push(Constraint {
            clause: term_clause(&f.name, val),
            phrase: format!("{prefix}{}", tok.raw),
            via: "literal+field",
            is_range: false,
            pos: bwd.first().copied().unwrap_or(num_i),
        });
    } else if f.date() && is_yearish(tok) {
        // A year compared against a date field: year-wide bounds, still
        // only the prompt's own number.
        let y = tok
            .int_literal
            .unwrap_or_else(|| tok.num_f64().unwrap_or(0.0) as i64);
        let clause = match op {
            Op::Gt => range_clause_1(&f.name, "gt", json!(next_year_start(y))),
            Op::Gte => range_clause_1(&f.name, "gte", json!(year_start(y))),
            Op::Lt => range_clause_1(&f.name, "lt", json!(year_start(y))),
            Op::Lte => range_clause_1(&f.name, "lte", json!(next_year_start(y))),
            Op::Eq => range_clause_2(
                &f.name,
                json!(year_start(y)),
                "gte",
                json!(next_year_start(y)),
                "lt",
            ),
        };
        ctx.constraints.push(Constraint {
            clause,
            phrase: format!("{prefix}{}", tok.raw),
            via: "comparator",
            is_range: true,
            pos: bwd.first().copied().unwrap_or(num_i),
        });
    } else {
        ctx.constraints.push(Constraint {
            clause: range_clause_1(&f.name, op.key(), val),
            phrase: format!("{prefix}{}", tok.raw),
            via: "comparator",
            is_range: true,
            pos: bwd.first().copied().unwrap_or(num_i),
        });
    }
}

/// `<num> and|to|through <num>` after i, plus the from/between connector
/// governing the pair (scanned back over content tokens).
fn pair_at(toks: &[Tok], used: &[bool], i: usize) -> Option<(usize, Option<usize>)> {
    let conn = i + 1;
    if conn + 1 >= toks.len() || used[conn] {
        return None;
    }
    match toks[conn].norm.as_str() {
        "and" | "to" | "through" => {}
        _ => return None,
    }
    let second = conn + 1;
    if second >= toks.len() || used[second] || !toks[second].is_num() {
        return None;
    }
    // connector: scan back over content tokens (up to 4) for from/between
    let mut connector = None;
    let mut j = i;
    let mut steps = 0;
    while j > 0 && steps < 4 {
        j -= 1;
        steps += 1;
        if used[j] {
            break;
        }
        match toks[j].norm.as_str() {
            "between" | "from" => {
                connector = Some(j);
                break;
            }
            _ if FUNCTION_WORDS.contains(&toks[j].norm.as_str()) => continue,
            _ => continue,
        }
    }
    Some((second, connector))
}

/// Scale handling: the effective value of the number at `i` plus how many
/// extra tokens it consumed ("100 million" → (10⁸, 1)).
fn number_span(toks: &[Tok], i: usize) -> (Value, usize) {
    if i + 1 < toks.len() {
        if let Some((_, mult)) = SCALES.iter().find(|(w, _)| toks[i + 1].norm == *w) {
            if let Some(base) = toks[i].int_literal {
                let v = base.saturating_mul(*mult);
                return (json!(v), 1);
            }
            if let Some(f) = toks[i].num_f64() {
                if let Some(n) = serde_json::Number::from_f64(f * (*mult as f64)) {
                    return (Value::Number(n), 1);
                }
            }
        }
    }
    (toks[i].num_value().unwrap_or(Value::Null), 0)
}

/// op phrase starting exactly at `at`.
fn op_after_at(toks: &[Tok], at: usize) -> Option<(usize, Op)> {
    for (words, op) in OP_AFTER {
        if words.iter().enumerate().all(|(k, w)| {
            let idx = at + k;
            idx < toks.len() && toks[idx].norm == *w
        }) {
            return Some((words.len(), *op));
        }
    }
    None
}

/// op phrase ending right before the number at `i`; (start, len, op).
fn op_before_at(toks: &[Tok], used: &[bool], i: usize) -> Option<(usize, usize, Op)> {
    for (words, op) in OP_BEFORE {
        let len = words.len();
        if i < len {
            continue;
        }
        let start = i - len;
        if words
            .iter()
            .enumerate()
            .all(|(k, w)| toks[start + k].norm == *w && !used[start + k])
        {
            return Some((start, len, *op));
        }
    }
    None
}

/// The separated form: op phrase, then the FIELD PHRASE, then the number —
/// "stronger than MAGNITUDE 3.5". Scans the number back over up to 3
/// content tokens to find an op phrase ending before them; the tokens
/// between the op and the number come back as the gap, a field-phrase
/// candidate `op_before_at` alone cannot express. Purely positional, so it
/// stays deterministic.
fn op_before_gap_at(
    toks: &[Tok],
    used: &[bool],
    i: usize,
) -> Option<(usize, usize, Op, Vec<usize>)> {
    for gap_len in 1..=3 {
        // `i - gap_len` must not underflow: a number early in the prompt
        // (gapminder-052: "in 1952, countries…") has no room for a gap at
        // all. Larger gap_len only makes it worse, so stop.
        if i <= gap_len {
            break;
        }
        let number_at = i - gap_len;
        let Some((start, olen, op)) = op_before_at(toks, used, number_at) else {
            continue;
        };
        let gap: Vec<usize> = (start + olen..i).collect();
        if gap
            .iter()
            .any(|&k| used[k] || toks[k].is_num() || toks[k].is_date())
        {
            continue;
        }
        return Some((start, olen, op, gap));
    }
    None
}

/// A unit token at/after `at` (skipping one "of"): (unit key, token index).
/// "Earth radii" is two tokens.
fn unit_after(toks: &[Tok], at: usize) -> Option<(&'static str, usize)> {
    let mut k = at;
    if k < toks.len() && toks[k].norm == "of" {
        k += 1;
    }
    if k >= toks.len() {
        return None;
    }
    let norm = toks[k].norm.as_str();
    if let Some((u, _)) = UNITS.iter().find(|(u, _)| *u == norm) {
        return Some((u, k));
    }
    if norm == "earth" && k + 1 < toks.len() && toks[k + 1].norm == "radii" {
        return Some(("earthradii", k));
    }
    None
}

/// The quantity a comparative adjective names, when the op phrase carries
/// one ("deeper than" → depth).
fn comparative_concept(toks: &[Tok], start: usize, len: usize) -> Option<&'static str> {
    COMPARATIVE_CONCEPTS
        .iter()
        .find(|(w, _)| (start..start + len).any(|k| toks[k].norm == *w))
        .map(|(_, c)| *c)
}

fn is_yearish(t: &Tok) -> bool {
    matches!(t.number, Some(v) if (1500.0..=2100.0).contains(&v) && v == v.trunc())
}

/// Years with a comparator ("2018 or later") fall back to the year/date
/// field — the number is still the prompt's own.
fn yearish_fallback(tok: &Tok, fields: &[FieldView]) -> Option<usize> {
    if is_yearish(tok) {
        year_field(fields)
    } else {
        None
    }
}

/// Copulas/auxiliaries: transparent in the backward scan ("life expectancy
/// WAS below 45" still finds `life expectancy`), unlike other function words
/// which terminate the phrase.
const AUX_WORDS: &[&str] = &[
    "is", "are", "was", "were", "be", "been", "being", "do", "does", "did", "has", "have", "had",
];

/// Content tokens ending just before `stop` (up to 3): the field phrase of
/// a comparison. Function and generic words terminate the scan, auxiliaries
/// are skipped over.
fn backward_phrase(toks: &[Tok], used: &[bool], stop: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut j = stop;
    while j > 0 && out.len() < 3 {
        j -= 1;
        if used[j] {
            break;
        }
        let t = &toks[j];
        if AUX_WORDS.contains(&t.norm.as_str()) {
            continue;
        }
        if FUNCTION_WORDS.contains(&t.norm.as_str())
            || GENERIC_WORDS.contains(&t.norm.as_str())
            || t.is_num()
            || t.is_date()
        {
            break;
        }
        out.push(j);
    }
    out.reverse();
    out
}

/// Up to `max` content tokens after `at`; generic words are skipped over —
/// EXCEPT when the generic word itself names a quantity concept ("2 known
/// planets": `planets` → pnum), in which case it joins the phrase, because
/// "at least 2 known planets" carries its field only in that word. Function
/// words end the phrase.
fn forward_phrase(toks: &[Tok], used: &[bool], at: usize, max: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut j = at;
    while j < toks.len() && out.len() < max {
        if used[j] {
            break;
        }
        let t = &toks[j];
        if FUNCTION_WORDS.contains(&t.norm.as_str()) {
            break;
        }
        if GENERIC_WORDS.contains(&t.norm.as_str()) {
            let carries_concept = concept_for(&t.norm).is_some();
            if !carries_concept {
                j += 1;
                continue;
            }
        }
        if t.is_num() || t.is_date() {
            break;
        }
        out.push(j);
        j += 1;
    }
    out
}

/// Field resolution for a comparison, in priority order: the unit decides
/// ("6000 K" is a temperature even when the phrase says "stars"), then the
/// backward field phrase, then the comparative's own concept ("deeper"
/// without a phrase still means depth), then the forward phrase ("at least
/// 2 known planets" carries its field after the number).
fn resolve_comparison(
    toks: &[Tok],
    bwd: &[usize],
    unit: Option<&str>,
    compar: Option<&str>,
    fwd: &[usize],
    fields: &[FieldView],
) -> Option<usize> {
    if let Some(u) = unit {
        let targets = UNITS.iter().find(|(key, _)| *key == u).map(|(_, t)| *t)?;
        if let Some(pos) = field_by_tokens(targets, fields, |f| f.numeric()) {
            return Some(pos);
        }
    }
    if let Some(pos) = resolve_field(toks, bwd, fields, |f| f.numeric()) {
        return Some(pos);
    }
    if let Some(c) = compar {
        if let Some(targets) = concept_for(c) {
            if let Some(pos) = field_by_tokens(targets, fields, |f| f.numeric()) {
                return Some(pos);
            }
        }
    }
    resolve_field(toks, fwd, fields, |f| f.numeric())
}

fn comparison_refusal(toks: &[Tok], bwd: &[usize], i: usize) -> Refusal {
    let phrase = if bwd.is_empty() {
        toks[i].raw.clone()
    } else {
        format!("{} {}", phrase_text(toks, bwd), toks[i].raw)
    };
    (
        vec![phrase.clone()],
        format!(
            "comparison on `{phrase}` resolves to no numeric or date field in this index — \
             refusing rather than guessing. Name a field that exists"
        ),
    )
}

fn phrase_text(toks: &[Tok], idxs: &[usize]) -> String {
    idxs.iter()
        .map(|&k| toks[k].raw.clone())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Pass A: date literals (single days and from/through pairs) and bare
/// years with a year connector. Bounds are the prompt's own days/years — a
/// year lands on the year FIELD when one exists (gapminder `year`, nasa
/// `disc_year`) and on the date field's year bounds otherwise.
fn date_pass(
    toks: &[Tok],
    fields: &[FieldView],
    time_field_hint: Option<&str>,
    used: &mut [bool],
    ctx: &mut PlanCtx,
) -> Result<(), Refusal> {
    let dpos = date_field(fields, time_field_hint);
    let ypos = fields
        .iter()
        .position(|f| f.numeric() && f.tokens.iter().any(|t| t == "year"));
    let mut i = 0;
    while i < toks.len() {
        if used[i] {
            i += 1;
            continue;
        }
        if let Some((y, m, d)) = date_parts(&toks[i].raw) {
            let Some(dfield) = dpos.map(|p| &fields[p]) else {
                // No date field: leave the literal to be named by the
                // leftover pass rather than silently dropped.
                i += 1;
                continue;
            };
            // pair: <date1> to|through <date2>
            if i + 2 < toks.len()
                && matches!(toks[i + 1].norm.as_str(), "to" | "through")
                && !used[i + 1]
                && !used[i + 2]
            {
                if let Some((y2, m2, d2)) = date_parts(&toks[i + 2].raw) {
                    ctx.constraints.push(Constraint {
                        clause: range_clause_2(
                            &dfield.name,
                            json!(day_start(y, m, d)),
                            "gte",
                            json!(next_day(y2, m2, d2)),
                            "lt",
                        ),
                        phrase: format!("{} … {}", toks[i].raw, toks[i + 2].raw),
                        via: "date-range",
                        is_range: true,
                        pos: i,
                    });
                    used[i] = true;
                    used[i + 1] = true;
                    used[i + 2] = true;
                    i += 3;
                    continue;
                }
            }
            // single day
            ctx.constraints.push(Constraint {
                clause: range_clause_2(
                    &dfield.name,
                    json!(day_start(y, m, d)),
                    "gte",
                    json!(next_day(y, m, d)),
                    "lt",
                ),
                phrase: toks[i].raw.clone(),
                via: "date-day",
                is_range: true,
                pos: i,
            });
            used[i] = true;
            i += 1;
            continue;
        }
        // bare year with a connector nearby
        if is_yearish(&toks[i]) && year_context(toks, used, i) {
            let y = toks[i].int_literal.unwrap_or_default();
            if let Some(yp) = ypos {
                let f = &fields[yp];
                ctx.constraints.push(Constraint {
                    clause: term_clause(&f.name, json!(y)),
                    phrase: toks[i].raw.clone(),
                    via: "year",
                    is_range: false,
                    pos: i,
                });
                used[i] = true;
                i += 1;
                continue;
            }
            if let Some(dp) = dpos {
                let f = &fields[dp];
                ctx.constraints.push(Constraint {
                    clause: range_clause_2(
                        &f.name,
                        json!(year_start(y)),
                        "gte",
                        json!(next_year_start(y)),
                        "lt",
                    ),
                    phrase: toks[i].raw.clone(),
                    via: "year",
                    is_range: true,
                    pos: i,
                });
                used[i] = true;
                i += 1;
                continue;
            }
        }
        i += 1;
    }
    Ok(())
}

/// Is a year connector adjacent (one token either side) to position i?
fn year_context(toks: &[Tok], used: &[bool], i: usize) -> bool {
    let check = |k: usize| !used[k] && YEAR_CONNECTORS.iter().any(|c| toks[k].norm == *c);
    (i > 0 && check(i - 1)) || (i + 1 < toks.len() && check(i + 1))
}

/// Keyword fields a path constraint may filter, most-specific first.
///
/// `ax_path` is the full root-relative path autoindex stamps on every
/// record. Code indexes also map `path`, but that value is the file name
/// (`worktree.rs`), so a directory prefix has to land on `ax_path` when
/// both exist.
const PATH_FIELDS: &[&str] = &["ax_path", "path", "filepath", "file", "ax_file"];

/// The keyword field a path prefix filters, or `None` when this index has
/// none of [`PATH_FIELDS`].
fn path_field(fields: &[FieldView]) -> Option<usize> {
    PATH_FIELDS.iter().find_map(|name| {
        fields
            .iter()
            .position(|f| f.name == *name && f.categorical())
    })
}

/// A filesystem path named by one prompt token, as the `prefix` value to
/// query, or `None` when `raw` is not a path.
///
/// Whitespace is the only token split, so `crates/herdr-gpui/src` arrives
/// whole. `norm_text` then deletes the slashes, which is why the value pass
/// cannot see it. A directory (last segment has no `.`) gets a trailing
/// slash: prefix `crates/app/src/` matches `crates/app/src/lib.rs` and not
/// `crates/app/src2/lib.rs`. A file (last segment contains `.`) is prefixed
/// as written. `and/or` is grammar, not a path. `http://…` is left for the
/// ordinary passes.
fn path_prefix(raw: &str) -> Option<String> {
    if !raw.contains('/') && !raw.contains('\\') {
        return None;
    }
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("file://")
    {
        return None;
    }
    let normalized = raw.replace('\\', "/");
    let trimmed = normalized.trim_matches('/');
    let trimmed = trimmed.strip_prefix("./").unwrap_or(trimmed);
    if trimmed.is_empty() {
        return None;
    }
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 2 || !trimmed.chars().any(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let contentful = segments.iter().any(|seg| {
        let n = norm_text(seg);
        !n.is_empty()
            && !FUNCTION_WORDS.contains(&n.as_str())
            && !GENERIC_WORDS.contains(&n.as_str())
    });
    if !contentful {
        return None;
    }
    let last = segments[segments.len() - 1];
    // Two bare words joined by a slash (`tcp/ip`, `client/server`,
    // `north/south`) are prose, not a path. Only take a token as a path when
    // it says so: three or more segments, a file extension on the last
    // segment, or a leading `./` `/` `~/` or trailing slash.
    let has_ext = last.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric())
    });
    let anchored = normalized.starts_with("./")
        || normalized.starts_with('/')
        || normalized.starts_with("~/")
        || normalized.ends_with('/');
    if segments.len() < 3 && !has_ext && !anchored {
        return None;
    }
    if last.contains('.') {
        Some(trimmed.to_string())
    } else {
        Some(format!("{trimmed}/"))
    }
}

/// Pass P: ground each path token as a keyword prefix. Tokens stay unused
/// when the index has no path field, so [`settle_leftovers`] can refuse
/// them by name instead of filing them as decoration.
fn path_pass(toks: &[Tok], fields: &[FieldView], used: &mut [bool], ctx: &mut PlanCtx) {
    let Some(fpos) = path_field(fields) else {
        return;
    };
    let field = fields[fpos].name.clone();
    for (i, t) in toks.iter().enumerate() {
        if used[i] {
            continue;
        }
        let Some(prefix) = path_prefix(&t.raw) else {
            continue;
        };
        ctx.constraints.push(Constraint {
            clause: prefix_clause(&field, &prefix),
            phrase: t.raw.clone(),
            via: "path-prefix",
            is_range: false,
            pos: i,
        });
        used[i] = true;
    }
}

/// Pass C: equality against the live value map. Returns the leftover runs
/// it could not ground, for the decoration/refusal decision.
async fn value_pass(
    toks: &[Tok],
    fields: &[FieldView],
    used: &mut [bool],
    ctx: &mut PlanCtx,
    idx: &Arc<Index>,
    probes: &mut usize,
) -> Vec<Vec<usize>> {
    let mut unresolved: Vec<Vec<usize>> = Vec::new();
    // Leftover runs: contiguous content tokens (function and generic words
    // break, so the value phrase inside "… for cities in Switzerland" and
    // "the ak network" survives on its own).
    let mut runs: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    for (k, t) in toks.iter().enumerate() {
        if used[k]
            || FUNCTION_WORDS.contains(&t.norm.as_str())
            || GENERIC_WORDS.contains(&t.norm.as_str())
        {
            if !cur.is_empty() {
                runs.push(mem::take(&mut cur));
            }
        } else {
            cur.push(k);
        }
    }
    if !cur.is_empty() {
        runs.push(cur);
    }

    'runs: for run in runs {
        // (a) field + value split ("magnitude type mb" with a non-generic
        // "type", "continent Europe")
        for split in (1..run.len()).rev() {
            let (left, right) = run.split_at(split);
            let Some(fpos) = resolve_field(toks, left, fields, |f| f.categorical()) else {
                continue;
            };
            let f = &fields[fpos];
            let joined = right
                .iter()
                .map(|&k| toks[k].norm.clone())
                .collect::<String>();
            if let Some(v) = ctx.lookup_exact(&f.name, &joined) {
                ctx.constraints.push(Constraint {
                    clause: term_clause(&f.name, v),
                    phrase: phrase_text(toks, &run),
                    via: "field+value",
                    is_range: false,
                    pos: run[0],
                });
                for &k in &run {
                    used[k] = true;
                }
                continue 'runs;
            }
            // BM25 arm: the field's values did not fit the agg.
            if !ctx.complete(&f.name) && *probes < MAX_BM25_PROBES {
                let phrase = phrase_text(toks, right);
                if let Some(clause) = ctx.bm25_probe(idx, &f.name, &phrase).await {
                    *probes += 1;
                    ctx.soft += 1;
                    ctx.constraints.push(Constraint {
                        clause,
                        phrase,
                        via: "field+value-bm25",
                        is_range: false,
                        pos: run[0],
                    });
                    for &k in &run {
                        used[k] = true;
                    }
                    continue 'runs;
                }
            }
        }
        // (b) value alone, longest subrun first, fields in name order;
        // prefix matching covers adjectival forms ("European" → "Europe").
        for len in (1..=run.len()).rev() {
            for start in 0..=(run.len() - len) {
                let sub = &run[start..start + len];
                let joined = sub
                    .iter()
                    .map(|&k| toks[k].norm.clone())
                    .collect::<String>();
                let mut hit: Option<(String, Value, bool)> = None;
                for fname in ctx.values.keys() {
                    if let Some(v) = ctx.lookup_exact(fname, &joined) {
                        hit = Some((fname.clone(), v, false));
                        break;
                    }
                    if let Some(v) = ctx.lookup_prefix(fname, &joined) {
                        if hit.is_none() {
                            hit = Some((fname.clone(), v, true));
                        }
                    }
                }
                if let Some((fname, v, soft)) = hit {
                    if soft {
                        ctx.soft += 1;
                    }
                    ctx.constraints.push(Constraint {
                        clause: term_clause(&fname, v),
                        phrase: phrase_text(toks, sub),
                        via: if soft { "value-prefix" } else { "value" },
                        is_range: false,
                        pos: sub[0],
                    });
                    for &k in sub {
                        used[k] = true;
                    }
                    continue 'runs;
                }
            }
        }
        unresolved.push(run);
    }
    unresolved
}

/// The decoration/refusal decision over the runs the passes could not
/// ground. A capitalized (proper-noun) run is a 422 naming it; so is any
/// multi-token run, and any run at all when nothing else grounded the
/// prompt. A single lowercase token next to a resolved constraint is
/// decoration, reported in the plan.
fn settle_leftovers(
    toks: &[Tok],
    unresolved: &[Vec<usize>],
    grounded: bool,
    ctx: &mut PlanCtx,
) -> Result<(), Refusal> {
    if unresolved.is_empty() {
        return Ok(());
    }
    for run in unresolved {
        // A path is a constraint the caller asked for. Dropping it as a
        // lowercase modifier returns a different document set and says only
        // that a modifier was ignored (#2120).
        if let Some(path) = run
            .iter()
            .find_map(|&k| path_prefix(&toks[k].raw).map(|_| toks[k].raw.clone()))
        {
            return Err((
                vec![path.clone()],
                format!(
                    "phrase `{path}` looks like a file path, but this index has no keyword \
                     field named ax_path, path, filepath, file, or ax_file to filter on — \
                     refusing rather than dropping the path. Map one of those fields, or \
                     remove the path from the prompt"
                ),
            ));
        }
        let single = run.len() == 1;
        let capitalized = toks[run[0]].capitalized;
        if capitalized || !single || !grounded {
            let phrase = phrase_text(toks, run);
            return Err((
                vec![phrase.clone()],
                format!(
                    "phrase `{phrase}` resolves to no field and no value in this index — \
                     refusing rather than guessing. Name a field that exists, or a value \
                     the index holds"
                ),
            ));
        }
        ctx.ignored.push(phrase_text(toks, run));
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests for the pure pieces (route-level tests: tests/ask_endpoint.rs)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_star_and_exact() {
        assert!(glob_match("ax-*", "ax-usgs-earthquakes"));
        assert!(glob_match("ax-*", "ax-"));
        assert!(!glob_match("ax-*", "sales"));
        assert!(glob_match("ax-quakes", "ax-quakes"));
        assert!(!glob_match("ax-quakes", "ax-quakes2"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("a*b*c", "aXXbYYc"));
        assert!(!glob_match("a*b*c", "aXXbYY"));
    }

    #[test]
    fn name_tokens_splits_snake_and_camel() {
        assert_eq!(name_tokens("pl_orbper"), vec!["pl", "orbper"]);
        assert_eq!(name_tokens("lifeExp"), vec!["life", "exp"]);
        assert_eq!(name_tokens("st_teff"), vec!["st", "teff"]);
        assert_eq!(name_tokens("magType"), vec!["mag", "type"]);
        assert_eq!(name_tokens("gdpPercap"), vec!["gdp", "percap"]);
    }

    #[test]
    fn date_parts_validates() {
        assert_eq!(date_parts("2026-09-01"), Some((2026, 9, 1)));
        assert_eq!(date_parts("2026-13-01"), None);
        assert_eq!(date_parts("2026-9-1"), Some((2026, 9, 1)));
        assert_eq!(date_parts("1992"), None);
        assert_eq!(day_start(2026, 9, 1), "2026-09-01T00:00:00Z");
        assert_eq!(next_day(2026, 9, 1), "2026-09-02T00:00:00Z");
        assert_eq!(next_day(2026, 12, 31), "2027-01-01T00:00:00Z");
    }

    #[test]
    fn tokenizer_marks_numbers_years_and_dates() {
        let t = tokenize("events deeper than 50 km on 2026-09-01 (UTC)");
        let nums: Vec<&Tok> = t.iter().filter(|x| x.is_num()).collect();
        assert_eq!(nums.len(), 1);
        assert_eq!(nums[0].int_literal, Some(50));
        let dates: Vec<&Tok> = t.iter().filter(|x| x.is_date()).collect();
        assert_eq!(dates.len(), 1);
        assert_eq!(dates[0].raw, "2026-09-01");
        // parens trimmed off UTC, which is a function word
        assert!(t.iter().any(|x| x.raw == "UTC"));
    }

    #[test]
    fn number_span_applies_scale_words() {
        let t = tokenize("population above 100 million");
        let i = t.iter().position(|x| x.int_literal == Some(100)).unwrap();
        let (v, extra) = number_span(&t, i);
        assert_eq!(v, json!(100_000_000));
        assert_eq!(extra, 1);
        let t = tokenize("magnitude 4.5");
        let i = t.iter().position(|x| x.number == Some(4.5)).unwrap();
        let (v, extra) = number_span(&t, i);
        assert_eq!(v, json!(4.5));
        assert_eq!(extra, 0);
    }

    #[test]
    fn concept_table_prefers_the_longest_phrase() {
        assert_eq!(concept_for("magnitudetype"), Some(&["magtype"][..]));
        assert_eq!(concept_for("magnitude"), Some(&["mag"][..]));
        assert_eq!(concept_for("lifeexpectancy"), Some(&["lifeexp"][..]));
        assert_eq!(concept_for("orbitalperiods"), Some(&["orbper"][..]));
        assert_eq!(concept_for("population"), Some(&["pop"][..]));
        assert_eq!(concept_for("gdppercapita"), Some(&["gdppercap"][..]));
        assert_eq!(concept_for("nothingknown"), None);
    }

    #[test]
    fn path_prefix_is_a_directory_or_a_file() {
        assert_eq!(
            path_prefix("crates/herdr-gpui/src"),
            Some("crates/herdr-gpui/src/".into())
        );
        // A trailing slash is the same directory. A sibling prefix is not.
        assert_eq!(
            path_prefix("crates/herdr-gpui/src/"),
            Some("crates/herdr-gpui/src/".into())
        );
        assert_eq!(
            path_prefix(r"crates\herdr-gpui\src"),
            Some("crates/herdr-gpui/src/".into())
        );
        assert_eq!(
            path_prefix("crates/herdr-gpui/src/worktree.rs"),
            Some("crates/herdr-gpui/src/worktree.rs".into())
        );
        assert_eq!(path_prefix("./src/lib.rs"), Some("src/lib.rs".into()));
        // Grammar, dates, and URLs are not paths.
        assert_eq!(path_prefix("and/or"), None);
        assert_eq!(path_prefix("2026/09/01"), None);
        assert_eq!(path_prefix("https://example.com/a"), None);
        assert_eq!(path_prefix("rust"), None);
        // Two bare words joined by a slash are prose, not a path.
        assert_eq!(path_prefix("tcp/ip"), None);
        assert_eq!(path_prefix("client/server"), None);
        assert_eq!(path_prefix("src/lib.rs"), Some("src/lib.rs".into()));
        assert_eq!(path_prefix("app/src/"), Some("app/src/".into()));
    }

    #[test]
    fn path_field_prefers_ax_path_over_the_filename_field() {
        let fields = vec![
            FieldView::new("path".into(), "keyword"),
            FieldView::new("ax_path".into(), "keyword"),
            FieldView::new("language".into(), "keyword"),
            FieldView::new("body".into(), "text"),
        ];
        assert_eq!(fields[path_field(&fields).unwrap()].name, "ax_path");
        let filename_only = vec![FieldView::new("path".into(), "keyword")];
        assert_eq!(fields_name(&filename_only), "path");
        assert!(path_field(&[FieldView::new("language".into(), "keyword")]).is_none());
        // A text `path` is not a keyword prefix target.
        assert!(path_field(&[FieldView::new("path".into(), "text")]).is_none());
    }

    fn fields_name(fields: &[FieldView]) -> &str {
        &fields[path_field(fields).unwrap()].name
    }

    #[test]
    fn value_agg_fields_orders_by_cardinality_then_name() {
        let mut fields = vec![
            FieldView::new("continent".to_string(), "keyword"),
            FieldView::new("country".into(), "keyword"),
            FieldView::new("place".into(), "keyword"),
            FieldView::new("mag".into(), "double"),
        ];
        fields[2].cardinality_est = Some(5); // place, lowest
        fields[0].cardinality_est = Some(50);
        let got = value_agg_fields(&fields);
        assert_eq!(got, vec!["place", "continent", "country"]);
    }
}
