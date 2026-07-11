//! Doc-values (columnar) fast path for `size:0 + match_all + aggs` searches.
//!
//! Root-cause context (2026-07, agg-hang cluster): every agg family that the
//! old M2-G2 fast path didn't serve fell back to `run_aggs_with_all` over a
//! fully materialised JSON corpus — an O(N) stored-section decompress + parse
//! of EVERY live document per request, plus O(N × buckets) per-bucket clone
//! passes inside `run_terms`/`run_composite`, and an O(N²) merge loop in
//! `variable_width_histogram`.  At 1M+ docs that exceeds any network timeout,
//! so the benchmark marked ~27 agg families "collapsed".  The M2-G2 fast path
//! itself had been stubbed to `None` since the M5.1 sharded-memtable refactor
//! (see ARCHITECTURE_V5 §2.4/§6), so *nothing* was served columnar.
//!
//! This module revives the fast path as a real executor over the per-segment
//! `.dv` doc-values sidecars (`Index::dv_columns_for`, cached) plus the live
//! sharded memtable (materialised as a small doc slice — memtables are
//! bounded by the flush threshold).  Coverage: the metric leaves
//! (avg/sum/min/max/stats/value_count), terms (± metric / top_hits sub-aggs),
//! range, date_range, filter, filters, adjacency_matrix, composite,
//! sampler/random_sampler, variable_width_histogram, date_histogram
//! (± metric sub-aggs and the full pipeline-agg family).
//!
//! Correctness contract: the output must be byte-identical to the brute
//! `run_aggs_with_all` path for every shape this module accepts (modulo
//! float-summation rounding order).  Anything not *provably* identical bails
//! with `None` and the caller falls back to the brute path.  Shared shaping
//! helpers (`typed_term_key`, `run_top_hits_with_total`,
//! `apply_bucket_pipeline_ops`, `resolve_sibling_pipelines`,
//! `run_pipeline_agg`, `run_sampler`) are reused from `aggs.rs` so bucket
//! rendering cannot drift.
//!
//! Hard gates (any failure → `None` → brute fallback):
//! * `XERJ_DISABLE_FAST_AGGS=1` env kill switch.
//! * Index must have ≥ `FAST_AGG_MIN_DOCS` docs — small indices are fast on
//!   the brute path anyway, and this keeps the (tiny-corpus) ES-YAML
//!   conformance suite pinned to the reference implementation.
//! * No deletes/updates present (`.dv` columns are physical → delete-blind;
//!   same `deletes_present` signal as the F1 shortcut-count gate).
//! * Every non-empty segment must have a decodable `.dv` sidecar.
//!
//! Known accepted divergences vs brute (documented, benign for the fast-path
//! trigger conditions): array-valued fields (the `.dv` builder skips arrays)
//! and `_doc_count` rollup weights are not modelled for *segment-resident*
//! docs — memtable docs go through the exact brute extractors.

use super::*;
use crate::aggs::{
    apply_bucket_pipeline_ops, auto_date_offset_ms, auto_date_pick_interval, calendar_bucket_key,
    detect_fractional_digits, doc_count_weight, doc_matches_filter, extract_date_ms_values,
    extract_field_values, extract_numeric, extract_numeric_values, format_histogram_key,
    format_number_pattern, format_range_val, get_nested_field, interval_to_ms,
    is_calendar_interval, java_double_str, matrix_stats_from_rows, next_calendar_bucket,
    parse_date_ms, parse_offset_ms, render_date_format, render_iso_date, resolve_sibling_pipelines,
    run_pipeline_agg, run_sampler, run_top_hits_with_total, typed_term_key,
};
use crate::memtable::MemBoolPred;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use xerj_storage::doc_values::{Column, KeywordColumn, NumericColumn};

/// Compile the top-level agg filter JSON (the restricted shape
/// `query_node_to_agg_filter` emits: `term`/`range`/`bool{must,filter}`) into a
/// pure conjunction of memtable columnar predicates, for the O(matching)
/// filtered-memtable fold (`ShardedFtsMemtable::filtered_docs_arc`).
///
/// Only Term (keyword string) and numeric Range leaves — and a `bool` with just
/// `must`/`filter` clauses of those — are expressible as a `[MemBoolPred]`
/// conjunction.  `terms` (a disjunction), `match_all`, and anything richer yield
/// `None`, so the caller keeps the exact full-memtable walk.  This is the
/// columnar-predicate sibling of `compile_top_pred`; the fast-agg memtable fold
/// always re-applies `doc_matches_filter` on top, so this only needs to be a
/// no-false-negative candidate generator.
fn mem_preds_from_agg_filter(filter: &Value) -> Option<Vec<MemBoolPred>> {
    fn push_leaf(filter: &Value, out: &mut Vec<MemBoolPred>) -> Option<()> {
        let obj = filter.as_object()?;
        if obj.len() != 1 {
            return None;
        }
        let (qtype, body) = obj.iter().next()?;
        match qtype.as_str() {
            "term" => {
                let fm = body.as_object()?;
                if fm.len() != 1 {
                    return None;
                }
                let (field, expected) = fm.iter().next()?;
                let value = match expected {
                    Value::String(s) => {
                        if s.contains('/') {
                            return None; // CIDR — mirror mem_bool_preds
                        }
                        s.clone()
                    }
                    _ => return None,
                };
                out.push(MemBoolPred::Term {
                    field: field.clone(),
                    value,
                });
                Some(())
            }
            "range" => {
                let fm = body.as_object()?;
                if fm.len() != 1 {
                    return None;
                }
                let (field, bounds) = fm.iter().next()?;
                let b = bounds.as_object()?;
                let get = |k: &str| -> Option<Option<f64>> {
                    match b.get(k) {
                        None => Some(None),
                        Some(Value::Number(n)) => Some(n.as_f64()),
                        _ => None, // date-string / non-numeric bound → bail
                    }
                };
                // Reject any present-but-non-numeric bound.
                for k in ["gte", "gt", "lte", "lt"] {
                    if b.contains_key(k) && get(k)?.is_none() {
                        return None;
                    }
                }
                out.push(MemBoolPred::Range {
                    field: field.clone(),
                    gte: get("gte")?,
                    gt: get("gt")?,
                    lte: get("lte")?,
                    lt: get("lt")?,
                });
                Some(())
            }
            "bool" => {
                let bo = body.as_object()?;
                if bo.keys().any(|k| k != "must" && k != "filter") {
                    return None;
                }
                for key in ["must", "filter"] {
                    if let Some(clauses) = bo.get(key) {
                        for c in clauses.as_array()? {
                            push_leaf(c, out)?;
                        }
                    }
                }
                Some(())
            }
            _ => None, // terms / match_all / … → not a pure Term/Range conjunction
        }
    }
    let mut out = Vec::new();
    push_leaf(filter, &mut out)?;
    if out.is_empty() {
        return None;
    }
    Some(out)
}

/// Minimum index size (live docs) before the columnar path activates.
/// Below this the brute JSON path answers in single-digit milliseconds and
/// remains the reference implementation for conformance corpora.
const FAST_AGG_MIN_DOCS: u64 = 10_000;

fn fast_aggs_disabled() -> bool {
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DISABLED.get_or_init(|| {
        std::env::var("XERJ_DISABLE_FAST_AGGS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

/// One on-disk segment's columns + identity.
#[derive(Clone)]
struct SegEntry {
    id: String,
    cols: std::sync::Arc<std::collections::BTreeMap<String, Column>>,
    docs: u32,
}

/// Memtable docs for the fast path.
///
/// `Owned` is the original brute-parity representation: every buffered doc
/// deep-cloned with `_id` / `_index` / `_seq_no` injected.  It is only
/// needed when the agg tree can actually OBSERVE those meta fields
/// (`top_hits`, or an agg targeting `_id`/`_index`/`_seq_no`).  For every
/// other shape we use `Shared`: Arc clones of the buffered sources — no
/// deep clone, no injection.  Under a bulk writer the memtable holds 10⁴-10⁵
/// docs, and the per-request Owned build (~100-300 ms of `clone_subtree` +
/// version-map lookups) dominated terms/cardinality latency.
enum MemDocs {
    Owned(Vec<Value>),
    Shared {
        ids: Vec<String>,
        srcs: Vec<std::sync::Arc<Value>>,
    },
}

impl MemDocs {
    fn len(&self) -> usize {
        match self {
            MemDocs::Owned(v) => v.len(),
            MemDocs::Shared { srcs, .. } => srcs.len(),
        }
    }
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn iter(&self) -> Box<dyn Iterator<Item = &Value> + '_> {
        match self {
            MemDocs::Owned(v) => Box::new(v.iter()),
            MemDocs::Shared { srcs, .. } => Box::new(srcs.iter().map(|a| a.as_ref())),
        }
    }
    /// Materialise doc `i` brute-corpus-style (meta fields injected).
    fn owned(&self, i: usize, idx: &Index) -> Option<Value> {
        match self {
            MemDocs::Owned(v) => v.get(i).cloned(),
            MemDocs::Shared { ids, srcs } => {
                let mut v: Value = srcs.get(i)?.as_ref().clone();
                let id = ids.get(i)?;
                if let Some(o) = v.as_object_mut() {
                    o.entry("_id".to_string())
                        .or_insert_with(|| Value::String(id.clone()));
                    o.entry("_index".to_string())
                        .or_insert_with(|| Value::String(idx.name.to_string()));
                    if let Some(seq) = idx.lookup_seq_no(id) {
                        o.entry("_seq_no".to_string()).or_insert_with(|| json!(seq));
                    }
                }
                Some(v)
            }
        }
    }
}

pub(super) struct FastCtx<'a> {
    idx: &'a Index,
    segs: Vec<SegEntry>,
    /// Memtable docs (see `MemDocs`), built LAZILY on first executor
    /// access: the columnar terms/cardinality memtable arms don't touch
    /// per-doc sources at all, so the per-request materialisation
    /// (doc_id String clones + Arc bumps over the whole buffered set —
    /// several ms at a drain-lagged memtable, EVERY agg request) is only
    /// paid by executors that genuinely walk docs.
    mem_docs: std::sync::OnceLock<MemDocs>,
    /// Whether the agg tree can observe meta fields (`top_hits`, `_id`,
    /// `_index`, `_seq_no`) — decides Owned vs Shared at build time.
    needs_owned_mem: bool,
    /// Schema fields mapped `boolean`.  Their `.dv` columns are numeric
    /// 0/1 (indistinguishable from real integers at the column level), but
    /// the brute path renders them as "false"/"true" term keys — so the
    /// terms executor needs the mapping to reproduce that.
    bool_fields: &'a std::collections::HashSet<String>,
    /// Top-level query filter (from `{size:0, query:Q, aggs:…}`), compiled to
    /// a columnar predicate.  `None` == match_all (the whole corpus). When
    /// present, EVERY executor restricts its columnar reduction to matching
    /// segment rows (via the `fused_seg_pass` row gate + `exec_metric_top`'s
    /// filtered loop) and every memtable-doc walk is gated by
    /// `doc_matches_filter(doc, top_filter_query)`.  Only metric + terms aggs
    /// support filtering; other agg types bail (None) so the caller falls back
    /// to the exact brute path.
    top_filter: Option<Pred>,
    /// The original ES-JSON query for the top-level filter, used for the
    /// memtable per-doc match (the trusted sibling of `compile_pred`, exactly
    /// as `exec_filter` pairs them).  `None` == match_all.
    top_filter_query: Option<Value>,
    /// The top-level filter compiled to memtable columnar predicates (a pure
    /// Term/Range conjunction), when it is one.  Drives the O(matching)
    /// filtered-memtable fold (`filtered_mem`); `None` when the filter isn't a
    /// pure conjunction (e.g. `terms`) or there is no filter — the executors
    /// then walk the full `mem()` gated by `doc_matches_filter`, exactly as
    /// before.
    top_filter_mem_preds: Option<Vec<MemBoolPred>>,
    /// Lazily-materialised columnar-filtered memtable docs (see `filtered_mem`).
    /// The outer `Option` is the OnceLock cell; the inner `Option` is the
    /// memtable's own "columnar path applicable?" answer (`None` == a bailout,
    /// fall back to full `mem()`).
    mem_filtered: std::sync::OnceLock<Option<Vec<(String, std::sync::Arc<Value>)>>>,
}

impl<'a> FastCtx<'a> {
    /// Lazy memtable-docs view (see the field doc).  Kept identical to the
    /// eager build this replaces: Owned (deep clone + meta injection) when
    /// the agg tree can observe meta fields, Arc-shared otherwise.
    fn mem(&self) -> &MemDocs {
        self.mem_docs.get_or_init(|| {
            if self.idx.memtable.doc_count() == 0 {
                return MemDocs::Owned(Vec::new());
            }
            if self.needs_owned_mem {
                MemDocs::Owned(
                    self.idx
                        .memtable
                        .all_docs_with_sources()
                        .into_iter()
                        .map(|(id, mut v)| {
                            if let Some(o) = v.as_object_mut() {
                                o.entry("_id".to_string())
                                    .or_insert_with(|| Value::String(id.clone()));
                                o.entry("_index".to_string())
                                    .or_insert_with(|| Value::String(self.idx.name.to_string()));
                                if let Some(seq) = self.idx.lookup_seq_no(&id) {
                                    o.entry("_seq_no".to_string()).or_insert_with(|| json!(seq));
                                }
                            }
                            v
                        })
                        .collect(),
                )
            } else {
                let pairs = self.idx.memtable.all_docs_with_sources_arc();
                let mut ids = Vec::with_capacity(pairs.len());
                let mut srcs = Vec::with_capacity(pairs.len());
                for (id, src) in pairs {
                    ids.push(id);
                    srcs.push(src);
                }
                MemDocs::Shared { ids, srcs }
            }
        })
    }

    /// Columnar-filtered memtable docs for the top-level filter, or `None` when
    /// the columnar fold doesn't apply — the caller then walks the full `mem()`
    /// gated by `doc_matches_filter`, exactly as before.
    ///
    /// Applies ONLY when the top-level filter compiled to a pure Term/Range
    /// conjunction AND the agg tree can't observe meta fields (`needs_owned_mem`
    /// == false — the columnar sources carry no `_id`/`_index`/`_seq_no`, and
    /// under that condition `plan.top_hits` is always `None`, so the positional
    /// `DocRef::Mem`/`mem().len()` paths never run) AND every predicate field is
    /// a scalar doc-valued column in every shard (the memtable returns `None`
    /// otherwise).  The returned set is a no-false-negative superset of the
    /// `doc_matches_filter` set — callers re-apply the JSON matcher for
    /// byte-identical results.
    fn filtered_mem(&self) -> Option<&[(String, std::sync::Arc<Value>)]> {
        if self.needs_owned_mem {
            return None;
        }
        let preds = self.top_filter_mem_preds.as_ref()?;
        self.mem_filtered
            .get_or_init(|| self.idx.memtable.filtered_docs_arc(preds))
            .as_deref()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColKind {
    Numeric,
    Keyword,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MetricKind {
    Avg,
    Sum,
    Min,
    Max,
    Stats,
    ValueCount,
}

#[derive(Clone, Copy, Default)]
struct MetricAcc {
    count: u64,
    sum: f64,
    min: f64,
    max: f64,
}

impl MetricAcc {
    #[inline]
    fn add(&mut self, v: f64) {
        if self.count == 0 {
            self.min = v;
            self.max = v;
        } else {
            if v < self.min {
                self.min = v;
            }
            if v > self.max {
                self.max = v;
            }
        }
        self.count += 1;
        self.sum += v;
    }
    /// value_count-style: value exists but only the count matters.
    #[inline]
    fn add_presence(&mut self) {
        self.count += 1;
    }
}

#[derive(Clone)]
struct MetricSpec {
    name: String,
    kind: MetricKind,
    field: String,
    meta: Option<Value>,
}

struct TopHitsSpec {
    name: String,
    params: Value,
    sort_field: String,
    desc: bool,
    k: usize,
    meta: Option<Value>,
}

/// Where a doc lives, for late-materialisation (top_hits winners).
#[derive(Clone, Copy)]
enum DocRef {
    Mem(usize),
    Seg(usize, u32),
}

/// Compiled sub-aggregation plan for a bucketing parent.
struct SubPlan {
    metrics: Vec<MetricSpec>,
    top_hits: Option<TopHitsSpec>,
    /// (name, agg_type, params, meta) for pipeline-family sub-aggs.  These
    /// are emitted as `run_pipeline_agg` placeholders per bucket (exactly
    /// like the brute path) and then resolved per-bucket + via
    /// `apply_bucket_pipeline_ops` on the parent's bucket array.
    pipelines: Vec<(String, String, Value, Option<Value>)>,
}

const PIPELINE_TYPES: &[&str] = &[
    "avg_bucket",
    "max_bucket",
    "min_bucket",
    "sum_bucket",
    "derivative",
    "moving_avg",
    "cumulative_sum",
    "bucket_selector",
    "bucket_sort",
    "bucket_script",
    "percentiles_bucket",
    "stats_bucket",
    "serial_diff",
    "moving_fn",
    "extended_stats_bucket",
];

fn metric_kind_of(t: &str) -> Option<MetricKind> {
    Some(match t {
        "avg" => MetricKind::Avg,
        "sum" => MetricKind::Sum,
        "min" => MetricKind::Min,
        "max" => MetricKind::Max,
        "stats" => MetricKind::Stats,
        "value_count" => MetricKind::ValueCount,
        _ => return None,
    })
}

/// Split an agg body into (agg_type, params, sub_aggs, meta) the same way
/// `execute_agg_with_all_cached` does.
fn split_agg_body(body: &Value) -> Option<(&str, &Value, Option<&Value>, Option<&Value>)> {
    let obj = body.as_object()?;
    let sub = obj.get("aggs").or_else(|| obj.get("aggregations"));
    let meta = obj.get("meta");
    let type_key = obj
        .keys()
        .find(|k| k.as_str() != "aggs" && k.as_str() != "aggregations" && k.as_str() != "meta")?;
    let params = obj.get(type_key)?;
    Some((type_key.as_str(), params, sub, meta))
}

/// Whitelist check: params object may only contain the listed keys.
fn params_only(params: &Value, allowed: &[&str]) -> bool {
    match params.as_object() {
        Some(o) => o.keys().all(|k| allowed.contains(&k.as_str())),
        None => false,
    }
}

impl Index {
    /// Entry point — see module docs.  Returns `Some(agg_result)` (shaped
    /// exactly like `run_aggs_with_all`'s return) or `None` to fall back.
    pub(super) fn try_fast_aggs(
        &self,
        aggs_def: &Value,
        filter: Option<&Value>,
        snap: &xerj_storage::index_store::IndexSnapshot,
        segments_dir: &std::path::Path,
        bool_fields: &std::collections::HashSet<String>,
    ) -> Option<(Value, Option<u64>)> {
        if fast_aggs_disabled() {
            return None;
        }
        let aggs_obj = aggs_def.as_object()?;
        if aggs_obj.is_empty() {
            return None;
        }

        // Top-level query filter (`{size:0, query:Q, aggs:…}`).  `None` ==
        // match_all — the pre-existing whole-corpus fast path.  When a filter
        // is present we MUST be able to columnarize it (term/terms/range/
        // match_all, or a pure-conjunction bool of those); anything else bails
        // to the exact brute path.
        let top_filter: Option<Pred> = match filter {
            None => None,
            Some(q) => Some(compile_top_pred(q)?),
        };
        let top_filter_query: Option<Value> = filter.cloned();
        // Columnar-predicate form of the filter for the O(matching) memtable
        // fold.  `None` for match_all or a non-conjunction filter (`terms`, …)
        // — those keep the full-memtable walk.
        let top_filter_mem_preds: Option<Vec<MemBoolPred>> =
            filter.and_then(mem_preds_from_agg_filter);

        // Size + delete gates.
        let seg_physical: u64 = snap.segments.iter().map(|m| m.doc_count).sum();
        let mem_physical = self.memtable.doc_count() as u64;
        let total_physical = seg_physical + mem_physical;
        if total_physical < FAST_AGG_MIN_DOCS {
            return None;
        }
        // Exact ghost signal — see the matching gate in `search_inner` and
        // `VersionMap::ghost_events` (the old live-vs-physical arithmetic
        // false-positived on physical-count drift and disabled this fast
        // path under append-only write load).
        let deletes_present = snap.segments.iter().any(|m| m.has_tombstones)
            || self.store.version_map.ghost_events() > 0;
        if deletes_present {
            return None;
        }

        // Warm the per-segment column cache in parallel first: a cold
        // many-segment index would otherwise decode every `.dv` sidecar
        // serially inside this request (~0.2 s × segments).  The decode work
        // is CPU-bound and `dv_columns_for` is DashMap-backed, so a rayon
        // fan-out is safe; the whole search body already runs inside
        // `block_in_place`.
        {
            use rayon::prelude::*;
            snap.segments.par_iter().for_each(|m| {
                if m.doc_count > 0 {
                    let _ = self.dv_columns_for(segments_dir, &m.id);
                }
            });
        }

        // Column sources.  A non-empty segment without a decodable `.dv`
        // sidecar makes the columnar view lossy → bail.
        let mut segs: Vec<SegEntry> = Vec::with_capacity(snap.segments.len());
        for meta in &snap.segments {
            if meta.doc_count == 0 {
                continue;
            }
            let cols = self.dv_columns_for(segments_dir, &meta.id)?;
            segs.push(SegEntry {
                id: meta.id.clone(),
                cols,
                docs: meta.doc_count as u32,
            });
        }

        // Memtable docs, materialised brute-style (see `search_inner`'s
        // full-corpus assembly): `_id` + `_index` + `_seq_no` injected.
        // Owned (deep-clone + meta-field injection) only when the agg tree
        // can observe meta fields; Arc-shared otherwise.  Conservative
        // substring probe — false positives merely fall back to the exact
        // legacy build.
        // Structural walk instead of `aggs_def.to_string()` + substring
        // search — the serialize-the-whole-tree probe ran on every fast-agg
        // request.  Semantics are preserved exactly: `top_hits` matches as a
        // substring of any key/string (a `top_hits` agg key, or a nested
        // one), while the meta fields match the serialized `"_id"` / `"_index"`
        // / `"_seq_no"` tokens, i.e. an exact object key or string value.
        let needs_owned_mem = agg_tree_mentions_meta(aggs_def);

        let ctx = FastCtx {
            idx: self,
            segs,
            mem_docs: std::sync::OnceLock::new(),
            needs_owned_mem,
            bool_fields,
            top_filter,
            top_filter_query,
            top_filter_mem_preds,
            mem_filtered: std::sync::OnceLock::new(),
        };

        // Filtered `hits.total`: the number of live docs matching the query.
        // For match_all the caller derives it from segment + memtable counts
        // (and later overwrites with the delete-aware live count), so we only
        // compute a total when a filter narrowed the corpus.  No deletes are
        // present on the fast path (gated above), so physical row counts are
        // exact, and each memtable doc is one hit (weights affect agg
        // `doc_count`, not `hits.total`).
        let filtered_total: Option<u64> = match &ctx.top_filter {
            None => None,
            Some(pred) => {
                let mut total: u64 = 0;
                for seg in &ctx.segs {
                    let sp = resolve_pred(&seg.cols, pred)?;
                    total += seg_pred_count(&sp, seg.docs);
                }
                if let Some(q) = &ctx.top_filter_query {
                    // O(matching) columnar fold when the filter columnarises;
                    // else the full-memtable walk.  Both re-check
                    // `doc_matches_filter` so the count is exact either way.
                    if let Some(fdocs) = ctx.filtered_mem() {
                        for (_id, src) in fdocs {
                            if doc_matches_filter(src, q) {
                                total += 1;
                            }
                        }
                    } else {
                        for doc in ctx.mem().iter() {
                            if doc_matches_filter(doc, q) {
                                total += 1;
                            }
                        }
                    }
                }
                Some(total)
            }
        };

        let result = ctx.eval_aggs_object(aggs_obj)?;
        Some((Value::Object(result), filtered_total))
    }
}

impl<'a> FastCtx<'a> {
    // ── field/kind helpers ───────────────────────────────────────────────

    /// The column kind of `field` across all segments.  `Ok(None)` = the
    /// field appears in no segment.  Mixed numeric/keyword → `Err` (bail).
    fn seg_field_kind(&self, field: &str) -> std::result::Result<Option<ColKind>, ()> {
        let mut kind: Option<ColKind> = None;
        for s in &self.segs {
            let k = match s.cols.get(field) {
                Some(Column::Numeric(_)) => ColKind::Numeric,
                Some(Column::Keyword(_)) => ColKind::Keyword,
                None => continue,
            };
            match kind {
                None => kind = Some(k),
                Some(prev) if prev == k => {}
                Some(_) => return Err(()),
            }
        }
        Ok(kind)
    }

    /// Global corpus-order rank offset of segment `si` (mem docs first, then
    /// segments in snapshot order) — mirrors the brute corpus assembly.
    fn seg_rank_offset(&self, si: usize) -> u64 {
        let mut off = self.mem().len() as u64;
        for s in &self.segs[..si] {
            off += s.docs as u64;
        }
        off
    }

    /// Materialise one segment-resident doc brute-corpus-style.
    fn fetch_seg_doc(&self, si: usize, row: u32) -> Option<Value> {
        let seg = &self.segs[si];
        let stored = self.idx.stored_values_for(&seg.id)?;
        let d = stored.get(row as usize)?;
        let id = d
            .get("_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let mut src = d.get("_source").cloned().unwrap_or_else(|| d.clone());
        if let Some(o) = src.as_object_mut() {
            if !id.is_empty() {
                o.entry("_id".to_string())
                    .or_insert(Value::String(id.clone()));
            }
            o.entry("_index".to_string())
                .or_insert_with(|| Value::String(self.idx.name.to_string()));
            if !id.is_empty() {
                if let Some(seq) = self.idx.lookup_seq_no(&id) {
                    o.entry("_seq_no".to_string()).or_insert_with(|| json!(seq));
                }
            }
        }
        Some(src)
    }

    fn fetch_doc(&self, r: DocRef) -> Option<Value> {
        match r {
            DocRef::Mem(i) => self.mem().owned(i, self.idx),
            DocRef::Seg(si, row) => self.fetch_seg_doc(si, row),
        }
    }

    // ── sub-agg planning ─────────────────────────────────────────────────

    /// Compile a parent's sub-agg tree.  `allow_top_hits` gates the (more
    /// expensive) winner-materialisation machinery to parents that support
    /// it.  Any unsupported shape → `None` (bail).
    fn plan_subs(&self, sub_aggs: Option<&Value>, allow_top_hits: bool) -> Option<SubPlan> {
        let mut plan = SubPlan {
            metrics: Vec::new(),
            top_hits: None,
            pipelines: Vec::new(),
        };
        let Some(sub) = sub_aggs else {
            return Some(plan);
        };
        let sub_obj = sub.as_object()?;
        for (name, body) in sub_obj {
            let (t, params, nested_sub, meta) = split_agg_body(body)?;
            if nested_sub.is_some() {
                return None; // no grandchildren on the fast path
            }
            if let Some(kind) = metric_kind_of(t) {
                if !params_only(params, &["field", "format"]) {
                    return None;
                }
                let field = params.get("field").and_then(Value::as_str)?.to_string();
                // Metric fold semantics require a numeric column (value_count
                // also accepts keyword).  Mixed kinds bail inside.
                match self.seg_field_kind(&field) {
                    Err(()) => return None,
                    Ok(Some(ColKind::Keyword)) if kind != MetricKind::ValueCount => return None,
                    _ => {}
                }
                // Memtable docs must hold plain numbers for the field (or
                // miss it) so brute string/date coercion can't diverge.
                if kind != MetricKind::ValueCount && !self.mem_field_numeric_safe(&field) {
                    return None;
                }
                plan.metrics.push(MetricSpec {
                    name: name.clone(),
                    kind,
                    field,
                    meta: meta.cloned(),
                });
            } else if t == "top_hits" {
                if !allow_top_hits || plan.top_hits.is_some() {
                    return None;
                }
                let spec = self.plan_top_hits(name, params, meta)?;
                plan.top_hits = Some(spec);
            } else if PIPELINE_TYPES.contains(&t) {
                plan.pipelines
                    .push((name.clone(), t.to_string(), params.clone(), meta.cloned()));
            } else {
                return None;
            }
        }
        Some(plan)
    }

    fn mem_field_numeric_safe(&self, field: &str) -> bool {
        // Under a columnarisable top filter, only the MATCHING docs are folded
        // (segments + the columnar memtable candidate set, `doc_matches_filter`-
        // rechecked), so the metric fold never touches a non-matching doc — the
        // numeric-safety check need only cover the candidate set.  That set is a
        // superset of the true matches, so this stays conservative (it may bail
        // to brute on a non-matching candidate, never the reverse) while
        // avoiding the full-memtable hydration.  When the columnar path doesn't
        // apply it walks the full `mem()` exactly as before.
        let is_num =
            |d: &Value| matches!(get_nested_field(d, field), Value::Number(_) | Value::Null);
        if let Some(fdocs) = self.filtered_mem() {
            fdocs.iter().all(|(_id, src)| is_num(src))
        } else {
            self.mem().iter().all(is_num)
        }
    }

    fn plan_top_hits(
        &self,
        name: &str,
        params: &Value,
        meta: Option<&Value>,
    ) -> Option<TopHitsSpec> {
        if !params_only(params, &["size", "from", "sort", "_source"]) {
            return None;
        }
        let size = params.get("size").and_then(Value::as_u64).unwrap_or(3) as usize;
        let from = params.get("from").and_then(Value::as_u64).unwrap_or(0) as usize;
        let k = from + size;
        if k == 0 || k > 100 {
            return None;
        }
        // Exactly one sort entry over a plain numeric field.
        let sort_arr: Vec<Value> = match params.get("sort") {
            Some(Value::Array(a)) => a.clone(),
            Some(Value::Object(o)) => vec![Value::Object(o.clone())],
            _ => return None, // default _score sort → bail
        };
        if sort_arr.len() != 1 {
            return None;
        }
        let obj = sort_arr[0].as_object()?;
        if obj.len() != 1 {
            return None;
        }
        let (field, opts) = obj.iter().next()?;
        if field.starts_with('_') {
            return None;
        }
        let desc = match opts {
            Value::String(s) => s == "desc",
            Value::Object(o) => {
                if !o.keys().all(|k| k == "order") {
                    return None; // mode / nested / missing → bail
                }
                o.get("order")
                    .and_then(Value::as_str)
                    .map(|v| v == "desc")
                    .unwrap_or(false)
            }
            _ => return None,
        };
        // Sort field must be numeric everywhere.
        match self.seg_field_kind(field) {
            Ok(Some(ColKind::Numeric)) => {}
            _ => return None,
        }
        if !self.mem_field_numeric_safe(field) {
            return None;
        }
        Some(TopHitsSpec {
            name: name.to_string(),
            params: params.clone(),
            sort_field: field.clone(),
            desc,
            k,
            meta: meta.cloned(),
        })
    }

    // ── metric emission (mirrors run_avg / run_sum / … exactly) ─────────

    fn emit_metric(kind: MetricKind, acc: &MetricAcc) -> Value {
        match kind {
            MetricKind::Avg => {
                if acc.count == 0 {
                    json!({ "value": Value::Null, "__xy_count__": 0, "__xy_sum__": 0.0 })
                } else {
                    let avg = acc.sum / acc.count as f64;
                    json!({ "value": avg, "__xy_count__": acc.count, "__xy_sum__": acc.sum })
                }
            }
            MetricKind::Sum => json!({ "value": acc.sum, "__xy_agg__": "sum" }),
            MetricKind::Min => {
                if acc.count == 0 {
                    json!({ "value": Value::Null, "__xy_agg__": "min" })
                } else {
                    json!({ "value": acc.min, "__xy_agg__": "min" })
                }
            }
            MetricKind::Max => {
                if acc.count == 0 {
                    json!({ "value": Value::Null, "__xy_agg__": "max" })
                } else {
                    json!({ "value": acc.max, "__xy_agg__": "max" })
                }
            }
            MetricKind::Stats => {
                if acc.count == 0 {
                    json!({ "count": 0, "min": Value::Null, "max": Value::Null, "avg": Value::Null, "sum": 0.0 })
                } else {
                    json!({
                        "count": acc.count,
                        "min": acc.min,
                        "max": acc.max,
                        "avg": acc.sum / acc.count as f64,
                        "sum": acc.sum,
                    })
                }
            }
            MetricKind::ValueCount => json!({ "value": acc.count }),
        }
    }

    /// Fold one memtable doc's field value into a metric accumulator using
    /// the brute extractors (so memtable semantics are exactly brute).
    fn fold_mem_metric(doc: &Value, spec: &MetricSpec, acc: &mut MetricAcc) {
        match spec.kind {
            MetricKind::ValueCount => {
                let vals = extract_field_values(doc, &spec.field);
                for _ in vals {
                    acc.add_presence();
                }
            }
            _ => {
                if let Some(v) = extract_numeric(doc, &spec.field) {
                    acc.add(v);
                }
            }
        }
    }

    /// Attach computed sub-agg results (metrics + pipeline placeholders) to
    /// a bucket object, then resolve per-bucket sibling pipelines exactly
    /// like the brute per-bucket `run_aggs_with_all` call does.
    fn finish_bucket(
        &self,
        bucket: &mut Map<String, Value>,
        plan: &SubPlan,
        accs: &[MetricAcc],
        top_hits_result: Option<Value>,
    ) {
        for (mi, spec) in plan.metrics.iter().enumerate() {
            let mut v = Self::emit_metric(spec.kind, &accs[mi]);
            if let Some(o) = v.as_object_mut() {
                o.insert(
                    "__type__".into(),
                    Value::String(metric_type_name(spec.kind).to_string()),
                );
                if let Some(m) = &spec.meta {
                    o.insert("meta".into(), m.clone());
                }
            }
            bucket.insert(spec.name.clone(), v);
        }
        if let (Some(spec), Some(mut th)) = (plan.top_hits.as_ref(), top_hits_result) {
            if let Some(o) = th.as_object_mut() {
                o.insert("__type__".into(), Value::String("top_hits".to_string()));
                if let Some(m) = &spec.meta {
                    o.insert("meta".into(), m.clone());
                }
            }
            bucket.insert(spec.name.clone(), th);
        }
        for (name, t, params, meta) in &plan.pipelines {
            let mut v = run_pipeline_agg(t, params);
            if let Some(o) = v.as_object_mut() {
                o.insert("__type__".into(), Value::String(t.clone()));
                if let Some(m) = meta {
                    o.insert("meta".into(), m.clone());
                }
            }
            bucket.insert(name.clone(), v);
        }
        if !plan.pipelines.is_empty() {
            resolve_sibling_pipelines(bucket);
        }
    }

    // ── dispatch ─────────────────────────────────────────────────────────

    /// Evaluate a whole `aggs` object over this context, mirroring the
    /// top-level dispatch in `try_fast_aggs`: each named entry is executed
    /// (or run as a sibling pipeline), tagged with `__type__`/`meta`, and
    /// then sibling-pipeline references are resolved.  Shared so the `global`
    /// bucket can re-evaluate its sub-aggs over the whole corpus (via a child
    /// context with no top filter) with byte-identical shaping.
    fn eval_aggs_object(&self, aggs_obj: &Map<String, Value>) -> Option<Map<String, Value>> {
        let mut result = Map::new();
        for (agg_name, agg_body) in aggs_obj {
            let (agg_type, params, sub, meta) = split_agg_body(agg_body)?;
            let mut agg_result = if PIPELINE_TYPES.contains(&agg_type) {
                // Sibling pipeline — placeholder now, resolved below.
                run_pipeline_agg(agg_type, params)
            } else {
                self.exec_agg(agg_type, params, sub)?
            };
            if let Some(obj) = agg_result.as_object_mut() {
                obj.insert("__type__".into(), Value::String(agg_type.to_string()));
                if let Some(m) = meta {
                    obj.insert("meta".into(), m.clone());
                }
            }
            result.insert(agg_name.clone(), agg_result);
        }
        resolve_sibling_pipelines(&mut result);
        Some(result)
    }

    /// `global` bucket — ignores the top-level query and aggregates the WHOLE
    /// corpus.  A child context with no top filter (sharing the same decoded
    /// columns via Arc + the same memtable source) evaluates the sub-aggs, so
    /// e.g. `value_count`/`avg`/`terms` run straight off doc-values instead of
    /// hydrating `_source` on the brute path.  Byte-identical to the brute
    /// `run_aggs_with_all` global bucket: `{doc_count, <subs>}`.
    fn exec_global(&self, params: &Value, sub: Option<&Value>) -> Option<Value> {
        // `global` takes no params.
        if !params.as_object().is_some_and(|o| o.is_empty()) {
            return None;
        }
        let child = FastCtx {
            idx: self.idx,
            segs: self.segs.clone(),
            mem_docs: std::sync::OnceLock::new(),
            needs_owned_mem: self.needs_owned_mem,
            bool_fields: self.bool_fields,
            top_filter: None,
            top_filter_query: None,
            top_filter_mem_preds: None,
            mem_filtered: std::sync::OnceLock::new(),
        };
        let mut bucket = Map::new();
        // Whole-corpus doc_count (no deletes on the fast path; segment rows are
        // weight-1, memtable docs honour `_doc_count`, matching every other
        // fast-path executor).
        let mut doc_count: u64 = 0;
        for seg in &child.segs {
            doc_count += seg.docs as u64;
        }
        for doc in child.mem().iter() {
            doc_count += doc_count_weight(doc);
        }
        bucket.insert("doc_count".to_string(), json!(doc_count));
        if let Some(sub_aggs) = sub.and_then(Value::as_object) {
            let subs = child.eval_aggs_object(sub_aggs)?;
            for (k, v) in subs {
                bucket.insert(k, v);
            }
        }
        Some(Value::Object(bucket))
    }

    fn exec_agg(&self, agg_type: &str, params: &Value, sub: Option<&Value>) -> Option<Value> {
        // Under a top-level query filter only the executors that thread the
        // filter through their columnar reduction are correct; every other
        // agg type falls back to the exact brute path (return None).  The
        // supported set covers the filtered-metric benchmark (avg/sum/…), the
        // common filtered `terms` dashboard, and `global` (which ignores the
        // filter entirely — it reduces over the whole corpus).
        if self.top_filter.is_some()
            && !matches!(
                agg_type,
                "avg"
                    | "sum"
                    | "min"
                    | "max"
                    | "stats"
                    | "value_count"
                    | "terms"
                    | "cardinality"
                    | "global"
            )
        {
            return None;
        }
        match agg_type {
            "avg" | "sum" | "min" | "max" | "stats" | "value_count" => {
                if sub.is_some() {
                    return None;
                }
                self.exec_metric_top(metric_kind_of(agg_type)?, params)
            }
            "extended_stats" => {
                if sub.is_some() {
                    return None;
                }
                self.exec_extended_stats(params)
            }
            "percentiles" => {
                if sub.is_some() {
                    return None;
                }
                self.exec_percentiles(params)
            }
            "percentile_ranks" => {
                if sub.is_some() {
                    return None;
                }
                self.exec_percentile_ranks(params)
            }
            "missing" => {
                if sub.is_some() {
                    return None;
                }
                self.exec_missing(params)
            }
            "median_absolute_deviation" => {
                if sub.is_some() {
                    return None;
                }
                self.exec_median_absolute_deviation(params)
            }
            "matrix_stats" => {
                if sub.is_some() {
                    return None;
                }
                self.exec_matrix_stats(params)
            }
            "auto_date_histogram" => self.exec_auto_date_histogram(params, sub),
            "terms" => self.exec_terms(params, sub),
            "rare_terms" => self.exec_rare_terms(params, sub),
            "significant_terms" => self.exec_significant_terms(params, sub),
            "cardinality" => self.exec_cardinality(params, sub),
            "range" => self.exec_range(params, sub),
            "date_range" => self.exec_date_range(params, sub),
            "filter" => self.exec_filter(params, sub),
            "filters" => self.exec_filters(params, sub),
            "adjacency_matrix" => self.exec_adjacency_matrix(params, sub),
            "composite" => self.exec_composite(params, sub),
            "global" => self.exec_global(params, sub),
            "sampler" | "random_sampler" => self.exec_sampler(params, sub),
            "variable_width_histogram" => self.exec_vwh(params, sub),
            "date_histogram" => self.exec_date_histogram(params, sub),
            "histogram" => self.exec_histogram(params, sub),
            "scripted_metric" => {
                if sub.is_some() {
                    return None;
                }
                self.exec_scripted_metric(params)
            }
            _ => None,
        }
    }

    // ── scripted_metric (reducible sum shape) ────────────────────────────
    //
    // The interpreter path (`aggs::run_scripted_metric`) runs only inside the
    // brute `run_aggs_with_all`, which first decompresses + JSON-parses EVERY
    // live doc, then tree-walks the map script per doc (≈730 ms on a 100 k
    // corpus).  When the four scripts form the canonical "sum one numeric doc
    // field" shape (`aggs::scripted_metric_sum_field`), the whole aggregation
    // is exactly `Σ doc.FIELD.value` over the matched docs — served here off the
    // numeric `.dv` column with no materialisation.
    //
    // Correctness contract (byte-identical to the interpreter):
    // * Painless long arithmetic renders an INTEGER as long as every summed
    //   value is integral (`num_binop`'s `both_int` rule); the moment a
    //   fractional value — or a missing field (`doc.F.value` → Null, which
    //   flips the accumulator to a float) — participates, the result becomes a
    //   float.  We therefore serve ONLY the wholly-integral, no-missing case
    //   and render `{"value": <i64>}`; anything else bails (`None`) to the exact
    //   interpreter.  Integer sums below 2^53 are order-independent in f64, so
    //   the columnar summation order is irrelevant to the emitted value.
    // * A top-level query filter never reaches here (scripted_metric is absent
    //   from `exec_agg`'s filtered whitelist, so `top_filter.is_some()` already
    //   returned `None`).
    fn exec_scripted_metric(&self, params: &Value) -> Option<Value> {
        let field = crate::aggs::scripted_metric_sum_field(params)?;
        if self.seg_field_kind(&field).ok()? != Some(ColKind::Numeric) {
            return None;
        }

        // ~9.007e15 == 2^53: the largest magnitude whose integer neighbours are
        // all exactly representable in f64 (mirrors `num_binop`'s int-shape cap).
        const INT_SAFE_ABS: f64 = 9.007e15;
        let mut sum = 0f64;

        // Segments: every doc in the segment must contribute a live, integral
        // value (no missing → no interpreter Null → stays integer).
        for s in &self.segs {
            if s.docs == 0 {
                continue;
            }
            match s.cols.get(&field) {
                Some(Column::Numeric(n)) => {
                    if !n.null_bitmap.is_empty() || n.live_count != s.docs as u64 {
                        return None; // some doc lacks the field
                    }
                    for bits in &n.data {
                        let v = f64::from_bits(*bits as u64);
                        if v.fract() != 0.0 {
                            return None; // fractional → float result
                        }
                        sum += v;
                    }
                }
                _ => return None, // field absent in this segment
            }
        }

        // Memtable docs (usually empty on the drained-corpus fast path): each
        // must carry the field as an integral number, else bail.
        for doc in self.mem().iter() {
            match doc.get(&field) {
                Some(Value::Number(num)) => {
                    let v = num.as_f64()?;
                    if v.fract() != 0.0 {
                        return None;
                    }
                    sum += v;
                }
                _ => return None,
            }
        }

        if !sum.is_finite() || sum.abs() >= INT_SAFE_ABS {
            return None; // interpreter would flip to float shape here
        }
        Some(json!({ "value": sum as i64 }))
    }

    // ── top-level metric ─────────────────────────────────────────────────

    fn exec_metric_top(&self, kind: MetricKind, params: &Value) -> Option<Value> {
        if !params_only(params, &["field", "format"]) {
            return None;
        }
        let field = params.get("field").and_then(Value::as_str)?;
        let seg_kind = self.seg_field_kind(field).ok()?;
        if kind != MetricKind::ValueCount {
            if seg_kind == Some(ColKind::Keyword) {
                return None;
            }
            if !self.mem_field_numeric_safe(field) {
                return None;
            }
        }

        // ── Filtered path ────────────────────────────────────────────────
        // A top-level query filter invalidates the O(1) precomputed column
        // stats (they cover the whole segment).  Reduce the metric over ONLY
        // the matching rows using the exact fused-pass fold (null handling,
        // value_count presence semantics identical to the unfiltered path),
        // then fold matching memtable docs with the brute extractors.
        if self.top_filter.is_some() {
            let spec = MetricSpec {
                name: String::new(),
                kind,
                field: field.to_string(),
                meta: None,
            };
            let plan = SubPlan {
                metrics: vec![spec.clone()],
                top_hits: None,
                pipelines: Vec::new(),
            };
            let mut accs: Vec<Vec<MetricAcc>> = vec![vec![MetricAcc::default()]];
            let mut tops: Vec<Vec<(f64, u64, DocRef)>> = vec![Vec::new()];
            let mut ms = false;
            for si in 0..self.segs.len() {
                let mut slot = |_row: u32| -> Option<usize> { Some(0) };
                self.fused_seg_pass(si, &mut slot, 1, &plan, &mut accs, &mut tops, &mut ms)?;
            }
            let mut acc = std::mem::take(&mut accs[0][0]);
            if let Some(q) = &self.top_filter_query {
                // O(matching) columnar fold when the filter columnarises; else
                // the full-memtable walk.  Both gate on `doc_matches_filter`, so
                // the folded set is byte-identical.
                if let Some(fdocs) = self.filtered_mem() {
                    for (_id, src) in fdocs {
                        if doc_matches_filter(src, q) {
                            Self::fold_mem_metric(src, &spec, &mut acc);
                        }
                    }
                } else {
                    for d in self.mem().iter() {
                        if doc_matches_filter(d, q) {
                            Self::fold_mem_metric(d, &spec, &mut acc);
                        }
                    }
                }
            }
            return Some(Self::emit_metric(kind, &acc));
        }

        let mut acc = MetricAcc::default();
        for s in &self.segs {
            match s.cols.get(field) {
                Some(Column::Numeric(n)) => {
                    if n.live_count > 0 {
                        if kind == MetricKind::ValueCount {
                            acc.count += n.live_count;
                        } else {
                            // O(1) per segment via precomputed column stats.
                            if acc.count == 0 {
                                acc.min = n.live_min;
                                acc.max = n.live_max;
                            } else {
                                if n.live_min < acc.min {
                                    acc.min = n.live_min;
                                }
                                if n.live_max > acc.max {
                                    acc.max = n.live_max;
                                }
                            }
                            acc.count += n.live_count;
                            acc.sum += n.live_sum;
                        }
                    }
                }
                Some(Column::Keyword(k)) => {
                    // Only reachable for value_count.
                    acc.count += (k.doc_count as u64).saturating_sub(k.null_bitmap.len());
                }
                None => {}
            }
        }
        let spec = MetricSpec {
            name: String::new(),
            kind,
            field: field.to_string(),
            meta: None,
        };
        for d in self.mem().iter() {
            Self::fold_mem_metric(d, &spec, &mut acc);
        }
        Some(Self::emit_metric(kind, &acc))
    }

    // ── extended_stats / percentiles / percentile_ranks (columnar) ───────
    //
    // These three leaves were the last O(N) brute-force uncached reads: the
    // pre-fix path fell into `run_aggs_with_all`, which decompresses +
    // JSON-parses EVERY live doc before `run_extended_stats` /
    // `run_percentiles` / `run_percentile_ranks` even see a value (~650 ms on
    // a 100 k corpus).  Served columnar they are a single linear fold over the
    // per-segment numeric `.dv` column (extended_stats) or a read of the
    // already-sorted column index (percentiles / percentile_ranks), dropping
    // to low-ms.  Each is byte-identical to its brute counterpart for the
    // shapes it accepts (modulo float-summation order — the same documented
    // divergence the other metric leaves carry), and bails (`None` → brute)
    // on any option that would change value gathering or rendering
    // (`missing`, `format`, `hdr`, `tdigest`) and under a top-level query
    // filter (handled by the whitelist in `exec_agg`).

    /// Gather every live numeric value of `field` (segments + memtable) into a
    /// single ascending-sorted `Vec<f64>`, mirroring the brute
    /// `nums.sort_by(partial_cmp)` collection so percentile interpolation is
    /// byte-identical.  Returns `None` when the field isn't a scalar numeric
    /// everywhere (keyword column, or a memtable doc holds a non-number) — the
    /// caller then bails to the exact brute path.
    ///
    /// Each segment's `.dv` numeric column already carries a `sorted`
    /// value-index (`Vec<(bits, doc_id)>`, ascending by f64), so the common
    /// single-segment / no-memtable case is a pure O(N) copy with no sort.
    fn gather_numeric_sorted(&self, field: &str) -> Option<Vec<f64>> {
        match self.seg_field_kind(field) {
            Ok(Some(ColKind::Keyword)) | Err(()) => return None,
            _ => {}
        }
        if !self.mem_field_numeric_safe(field) {
            return None;
        }
        let mut capacity = self.mem().len();
        for s in &self.segs {
            if let Some(Column::Numeric(n)) = s.cols.get(field) {
                capacity += n.live_count as usize;
            }
        }
        let mut vals: Vec<f64> = Vec::with_capacity(capacity);
        for s in &self.segs {
            match s.cols.get(field) {
                Some(Column::Numeric(n)) => {
                    for (bits, _) in &n.sorted {
                        vals.push(f64::from_bits(*bits as u64));
                    }
                }
                Some(Column::Keyword(_)) => return None,
                None => {}
            }
        }
        // The collected values are already globally sorted iff they came from a
        // single pre-sorted column source and the memtable added none.
        let mut pre_sorted = self.segs.len() <= 1;
        for doc in self.mem().iter() {
            if let Some(v) = extract_numeric(doc, field) {
                vals.push(v);
                pre_sorted = false;
            }
        }
        if !pre_sorted {
            vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        }
        Some(vals)
    }

    /// `extended_stats` — single columnar pass computing count / sum /
    /// sum_of_squares / min / max, then the same exact-arithmetic moments +
    /// `std_deviation_bounds` (honouring `sigma`, default 2) as
    /// `run_extended_stats`.  Whitelisted params only; `missing` / `format`
    /// bail to brute.
    fn exec_extended_stats(&self, params: &Value) -> Option<Value> {
        if !params_only(params, &["field", "sigma"]) {
            return None;
        }
        let field = params.get("field").and_then(Value::as_str)?;
        match self.seg_field_kind(field) {
            Ok(Some(ColKind::Keyword)) | Err(()) => return None,
            _ => {}
        }
        if !self.mem_field_numeric_safe(field) {
            return None;
        }

        let mut count: u64 = 0;
        let mut sum: f64 = 0.0;
        let mut sum_sq: f64 = 0.0;
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        let mut fold = |v: f64| {
            count += 1;
            sum += v;
            sum_sq += v * v;
            if v < min {
                min = v;
            }
            if v > max {
                max = v;
            }
        };
        for s in &self.segs {
            match s.cols.get(field) {
                Some(Column::Numeric(n)) => {
                    if n.null_bitmap.is_empty() {
                        for &bits in &n.data {
                            fold(f64::from_bits(bits as u64));
                        }
                    } else {
                        for row in 0..n.doc_count {
                            if !n.null_bitmap.contains(row) {
                                fold(f64::from_bits(n.data[row as usize] as u64));
                            }
                        }
                    }
                }
                Some(Column::Keyword(_)) => return None,
                None => {}
            }
        }
        for doc in self.mem().iter() {
            if let Some(v) = extract_numeric(doc, field) {
                fold(v);
            }
        }

        if count == 0 {
            // Mirror run_extended_stats' empty shape exactly: `sum` is a real
            // zero, every higher-order moment is null.
            return Some(json!({
                "count": 0, "min": Value::Null, "max": Value::Null,
                "avg": Value::Null, "sum": 0.0, "sum_of_squares": Value::Null,
                "variance": Value::Null, "variance_population": Value::Null,
                "variance_sampling": Value::Null,
                "std_deviation": Value::Null,
                "std_deviation_population": Value::Null,
                "std_deviation_sampling": Value::Null,
                "std_deviation_bounds": {
                    "upper": Value::Null, "lower": Value::Null,
                    "upper_population": Value::Null, "lower_population": Value::Null,
                    "upper_sampling": Value::Null, "lower_sampling": Value::Null,
                }
            }));
        }

        let avg = sum / count as f64;
        let variance_pop = (sum_sq / count as f64) - avg * avg;
        let variance_pop = variance_pop.max(0.0);
        let variance_samp = if count > 1 {
            variance_pop * count as f64 / (count - 1) as f64
        } else {
            0.0
        };
        let std_deviation = variance_pop.max(0.0).sqrt();
        let std_deviation_samp = variance_samp.max(0.0).sqrt();
        let sigma = params.get("sigma").and_then(Value::as_f64).unwrap_or(2.0);

        Some(json!({
            "count": count,
            "min": min,
            "max": max,
            "avg": avg,
            "sum": sum,
            "sum_of_squares": sum_sq,
            "variance": variance_pop,
            "variance_population": variance_pop,
            "variance_sampling": variance_samp,
            "std_deviation": std_deviation,
            "std_deviation_population": std_deviation,
            "std_deviation_sampling": std_deviation_samp,
            "std_deviation_bounds": {
                "upper": avg + sigma * std_deviation,
                "lower": avg - sigma * std_deviation,
                "upper_population": avg + sigma * std_deviation,
                "lower_population": avg - sigma * std_deviation,
                "upper_sampling": avg + sigma * std_deviation_samp,
                "lower_sampling": avg - sigma * std_deviation_samp,
            }
        }))
    }

    /// `percentiles` — the ES-default (non-hdr) algorithm is exact linear
    /// interpolation over the sorted values (`run_percentiles`), so the
    /// columnar path reads the pre-sorted column index and applies the
    /// identical `rank = (pct/100)*(N-1)` interpolation.  Honours `percents`
    /// (default `[1,5,25,50,75,95,99]`) and `keyed` (default true); `hdr` /
    /// `missing` / `format` / `tdigest` bail to brute.
    fn exec_percentiles(&self, params: &Value) -> Option<Value> {
        if !params_only(params, &["field", "percents", "keyed"]) {
            return None;
        }
        let field = params.get("field").and_then(Value::as_str)?;
        let percents: Vec<f64> = params
            .get("percents")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_else(|| vec![1.0, 5.0, 25.0, 50.0, 75.0, 95.0, 99.0]);
        let keyed = params.get("keyed").and_then(Value::as_bool).unwrap_or(true);

        let nums = self.gather_numeric_sorted(field)?;
        let compute = |pct: f64| -> Option<f64> {
            if nums.is_empty() {
                return None;
            }
            let rank = (pct / 100.0) * (nums.len() as f64 - 1.0);
            let lo = rank.floor() as usize;
            let hi = (lo + 1).min(nums.len() - 1);
            let frac = rank - rank.floor();
            Some(nums[lo] * (1.0 - frac) + nums[hi] * frac)
        };

        if keyed {
            let values: Map<String, Value> = percents
                .iter()
                .map(|&pct| {
                    // ES keys with `String.valueOf(double)` — "25.0", "99.99".
                    let key = java_double_str(pct);
                    let val = match compute(pct) {
                        Some(v) => serde_json::Number::from_f64(v)
                            .map(Value::Number)
                            .unwrap_or(Value::Null),
                        None => Value::Null,
                    };
                    (key, val)
                })
                .collect();
            Some(json!({ "values": values }))
        } else {
            let arr: Vec<Value> = percents
                .iter()
                .map(|&pct| {
                    let val = match compute(pct) {
                        Some(v) => serde_json::Number::from_f64(v)
                            .map(Value::Number)
                            .unwrap_or(Value::Null),
                        None => Value::Null,
                    };
                    let key_num = serde_json::Number::from_f64(pct)
                        .map(Value::Number)
                        .unwrap_or(Value::Null);
                    json!({ "key": key_num, "value": val })
                })
                .collect();
            Some(json!({ "values": arr }))
        }
    }

    /// `percentile_ranks` — for each target value, the fraction of the corpus
    /// at or below it, ×100.  Byte-identical to `run_percentile_ranks`: the
    /// sorted column index turns each brute `filter(v <= t).count()` into a
    /// `partition_point` (same exact count).  `hdr` / `missing` / `format`
    /// bail to brute.
    fn exec_percentile_ranks(&self, params: &Value) -> Option<Value> {
        if !params_only(params, &["field", "values", "keyed"]) {
            return None;
        }
        let field = params.get("field").and_then(Value::as_str)?;
        let targets = match params.get("values").and_then(Value::as_array) {
            Some(v) => v,
            None => return Some(json!({ "values": {} })),
        };
        let keyed = params.get("keyed").and_then(Value::as_bool).unwrap_or(true);

        let nums = self.gather_numeric_sorted(field)?;
        let total = nums.len() as f64;
        // `nums` is ascending, so the count of values <= t is a partition_point.
        let rank_of = |t: f64| -> f64 {
            if total == 0.0 {
                0.0
            } else {
                let below = nums.partition_point(|&v| v <= t) as f64;
                (below / total) * 100.0
            }
        };
        // ES sorts the requested values ascending before rendering, keys the
        // keyed map with `String.valueOf(double)` ("200.0"), and renders the
        // keyed=false key as a double (200.0) — byte-identical to brute
        // `run_percentile_ranks`.
        let mut sorted: Vec<f64> = targets.iter().filter_map(Value::as_f64).collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        if keyed {
            let mut result = Map::new();
            for &t in &sorted {
                result.insert(java_double_str(t), json!(rank_of(t)));
            }
            Some(json!({ "values": result }))
        } else {
            let arr: Vec<Value> = sorted
                .iter()
                .map(|&t| json!({ "key": t, "value": rank_of(t) }))
                .collect();
            Some(json!({ "values": arr }))
        }
    }

    // ── missing / median_absolute_deviation / matrix_stats / auto_date_histogram ──
    //
    // Four more O(N) brute leaves (`run_aggs_with_all` materialised every live
    // doc — ~1 s / 100 k) rerouted through the doc-values columns.  Each bails
    // (`None` → brute) on any option that would change value gathering or
    // rendering, and (via the `exec_agg` whitelist) under a top-level query
    // filter, so what it serves is byte-identical to its brute counterpart for
    // the shapes it accepts (modulo the documented float-summation-order and
    // array-skip divergences the other fast-agg leaves already carry).

    /// `missing` — count of docs where `field` has no value.  The brute
    /// `run_missing` walks `get_nested_field(doc, field).is_null()`; served
    /// columnar that is `total − present` = each segment column's null count
    /// (whole segment when the field's column is absent) plus the exact
    /// `is_null` walk over the small memtable.  Only `{field}` (and the
    /// `missing`-placeholder short-circuit → `doc_count: 0`, matching brute) is
    /// accepted; `run_missing` ignores sub-aggs, so the `exec_agg` arm bails on
    /// any sub.
    fn exec_missing(&self, params: &Value) -> Option<Value> {
        if !params_only(params, &["field", "missing"]) {
            return None;
        }
        let field = params.get("field").and_then(Value::as_str)?;
        // `missing` placeholder present → every doc has a synthetic value →
        // count collapses to 0 (identical to brute `has_missing_default`).
        if params.get("missing").is_some() {
            return Some(json!({ "doc_count": 0 }));
        }
        let mut missing: u64 = 0;
        for seg in &self.segs {
            match seg.cols.get(field) {
                Some(Column::Numeric(n)) => missing += n.null_bitmap.len(),
                Some(Column::Keyword(k)) => missing += k.null_bitmap.len(),
                // Field absent from this segment's columns → every row missing.
                None => missing += seg.docs as u64,
            }
        }
        for doc in self.mem().iter() {
            if get_nested_field(doc, field).is_null() {
                missing += 1;
            }
        }
        Some(json!({ "doc_count": missing }))
    }

    /// `median_absolute_deviation` — `median(|x_i − median(x)|)`.  XERJ's brute
    /// path is EXACT (sort + positional median), not ES's TDigest, so the
    /// columnar value equals brute byte-for-byte and differs from ES only by
    /// the TDigest approximation error (same accepted divergence as
    /// `percentiles`).  The sorted column (`gather_numeric_sorted`) gives the
    /// first median for free; the abs-deviations need one O(N) map + an
    /// `select_nth_unstable` (same positional order-statistics the brute full
    /// sort would land on).  `compression` (a TDigest knob) is tolerated and
    /// ignored since we are exact; `missing` / `format` change value gathering
    /// so they bail to brute (not in the whitelist).
    fn exec_median_absolute_deviation(&self, params: &Value) -> Option<Value> {
        if !params_only(params, &["field", "compression"]) {
            return None;
        }
        let field = params.get("field").and_then(Value::as_str)?;
        let nums = self.gather_numeric_sorted(field)?;
        if nums.is_empty() {
            return Some(json!({ "value": Value::Null }));
        }
        let n = nums.len();
        let median = if n % 2 == 0 {
            (nums[n / 2 - 1] + nums[n / 2]) / 2.0
        } else {
            nums[n / 2]
        };
        let mut dev: Vec<f64> = nums.iter().map(|x| (x - median).abs()).collect();
        let cmp = |a: &f64, b: &f64| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal);
        let m = dev.len();
        let mad = if m.is_multiple_of(2) {
            // `select_nth_unstable_by(m/2)` places the (m/2)-th order statistic
            // at `m/2` and leaves everything before it ≤ it, so the low middle
            // is the max of that left partition — exactly the two positions a
            // full sort would read.
            let hi = {
                let (_, p, _) = dev.select_nth_unstable_by(m / 2, cmp);
                *p
            };
            let lo = dev[..m / 2]
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max);
            (lo + hi) / 2.0
        } else {
            let (_, p, _) = dev.select_nth_unstable_by(m / 2, cmp);
            *p
        };
        Some(json!({ "value": mad }))
    }

    /// `matrix_stats` — single columnar pass gathering, per doc where EVERY
    /// requested field is present, a value vector straight from the numeric
    /// `.dv` columns, then the identical count/mean/variance/skewness/kurtosis
    /// plus pairwise covariance/correlation reduction as brute
    /// (`matrix_stats_from_rows`, shared).  Only the plain `{fields}` form is
    /// served; `mode` (multi-value collapse), `missing` (per-field default) and
    /// the `__xy_f32_fields__` sentinel (float-precision round-trip, injected by
    /// the REST layer only when a `type: float` field is involved) all change
    /// value gathering, so their presence bails to the exact brute reducer.
    fn exec_matrix_stats(&self, params: &Value) -> Option<Value> {
        if !params_only(params, &["fields"]) {
            return None;
        }
        let fields: Vec<String> = params
            .get("fields")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if fields.is_empty() {
            return Some(json!({ "doc_count": 0, "fields": [] }));
        }
        // Every field must be a scalar numeric doc-value column everywhere
        // (keyword / mixed / non-numeric memtable value → bail to brute).
        for f in &fields {
            match self.seg_field_kind(f) {
                Ok(Some(ColKind::Numeric)) | Ok(None) => {}
                _ => return None,
            }
            if !self.mem_field_numeric_safe(f) {
                return None;
            }
        }
        let k = fields.len();
        let mut rows: Vec<Vec<f64>> = Vec::new();
        // Memtable docs first (brute-parity scalar extraction); a doc is
        // included only when every field resolves to a number.
        for doc in self.mem().iter() {
            let mut row = Vec::with_capacity(k);
            let mut valid = true;
            for f in &fields {
                match extract_numeric(doc, f) {
                    Some(v) => row.push(v),
                    None => {
                        valid = false;
                        break;
                    }
                }
            }
            if valid {
                rows.push(row);
            }
        }
        // Segment rows — join the fields' columns by row id.  If any field's
        // column is absent for the whole segment, no row there can be valid.
        for seg in &self.segs {
            let mut cols: Vec<&NumericColumn> = Vec::with_capacity(k);
            let mut all_present = true;
            for f in &fields {
                match seg.cols.get(f) {
                    Some(Column::Numeric(n)) => cols.push(n),
                    _ => {
                        all_present = false;
                        break;
                    }
                }
            }
            if !all_present {
                continue;
            }
            for row_id in 0..seg.docs {
                let mut row = Vec::with_capacity(k);
                let mut valid = true;
                for n in &cols {
                    // `NumericColumn` stores f64 BITS — decode before use.
                    match n.get(row_id) {
                        Some(bits) => row.push(f64::from_bits(bits as u64)),
                        None => {
                            valid = false;
                            break;
                        }
                    }
                }
                if valid {
                    rows.push(row);
                }
            }
        }
        // Reduction is permutation-invariant up to float ULPs, so the
        // memtable-first row order (vs brute corpus order) only moves the
        // result within the documented summation-order tolerance.
        Some(matrix_stats_from_rows(fields, rows))
    }

    /// `auto_date_histogram` — compute the date column's min/max columnar, pick
    /// the same rounding interval as brute (`auto_date_pick_interval` — the ES
    /// sec/min/hour/day/month/year ladder), then bucket via the EXISTING
    /// columnar `exec_date_histogram` with that fixed interval and annotate the
    /// chosen `interval`.  Byte-identical to brute `run_auto_date_histogram`
    /// (which delegates to `run_date_histogram` the same way) for every case it
    /// serves.  Only `{field, buckets}` is accepted.  Multi-unit intervals
    /// (`3h`/`12h`/`5m`/…/`7d`/`30d`/`90d`) carry the ES min-doc-anchored grid
    /// `offset` (`auto_date_offset_ms`), which the columnar
    /// `exec_date_histogram` applies on its fixed-interval grid.
    fn exec_auto_date_histogram(&self, params: &Value, sub: Option<&Value>) -> Option<Value> {
        if !params_only(params, &["field", "buckets"]) {
            return None;
        }
        let field = params.get("field").and_then(Value::as_str)?;
        match self.seg_field_kind(field) {
            // Keyword = ISO-string date column; Numeric = epoch-ms column.
            Ok(Some(ColKind::Keyword)) | Ok(Some(ColKind::Numeric)) | Ok(None) => {}
            _ => return None,
        }
        let target_buckets = params.get("buckets").and_then(Value::as_u64).unwrap_or(10) as usize;

        // Columnar min/max of the parsed date, mirroring the brute
        // `parse_date_ms(get_nested_field(...))` collection.
        let mut min_ts = i64::MAX;
        let mut max_ts = i64::MIN;
        let mut any = false;
        for seg in &self.segs {
            match seg.cols.get(field) {
                Some(Column::Numeric(n)) => {
                    if n.live_count > 0 {
                        // f64::from_bits epoch-ms; exact as i64 (< 2^53).
                        let lo = n.live_min as i64;
                        let hi = n.live_max as i64;
                        if lo < min_ts {
                            min_ts = lo;
                        }
                        if hi > max_ts {
                            max_ts = hi;
                        }
                        any = true;
                    }
                }
                Some(Column::Keyword(k)) => {
                    let ord_ms = self.date_ord_index(seg, field)?;
                    for (ord, &ms) in ord_ms.iter().enumerate() {
                        if ms == i64::MIN || k.per_ord_count.get(ord).copied().unwrap_or(0) == 0 {
                            continue;
                        }
                        if ms < min_ts {
                            min_ts = ms;
                        }
                        if ms > max_ts {
                            max_ts = ms;
                        }
                        any = true;
                    }
                }
                None => {}
            }
        }
        for doc in self.mem().iter() {
            if let Some(ms) = parse_date_ms(get_nested_field(doc, field)) {
                if ms < min_ts {
                    min_ts = ms;
                }
                if ms > max_ts {
                    max_ts = ms;
                }
                any = true;
            }
        }
        if !any {
            // No parseable dates — brute returns this exact empty shape.
            return Some(json!({ "buckets": [], "interval": "1d" }));
        }

        let (interval_label, interval_ms) = auto_date_pick_interval(min_ts, max_ts, target_buckets);
        // Multi-unit intervals (3h/12h/5m/…/7d/30d/90d) carry the ES
        // min-doc-anchored grid offset (see `auto_date_offset_ms`); pass it to
        // the columnar date_histogram exactly like the brute path does.
        let offset_ms = auto_date_offset_ms(interval_label, interval_ms, min_ts);
        let mut synth = json!({ "field": field, "fixed_interval": interval_label });
        if offset_ms != 0 {
            synth
                .as_object_mut()
                .unwrap()
                .insert("offset".to_string(), json!(format!("+{}ms", offset_ms)));
        }
        let mut result = self.exec_date_histogram(&synth, sub)?;
        if let Some(obj) = result.as_object_mut() {
            obj.insert("interval".to_string(), json!(interval_label));
        }
        Some(result)
    }

    // ── shared fused row pass ────────────────────────────────────────────
    //
    // For a parent that assigns each segment row to at most one bucket slot
    // (terms ord → slot, date_histogram ord → slot, …), fold every metric
    // sub-agg and the optional top_hits accumulator in a single pass.

    #[allow(clippy::too_many_arguments)]
    fn fused_seg_pass(
        &self,
        si: usize,
        slot_of_row: &mut dyn FnMut(u32) -> Option<usize>,
        n_slots: usize,
        plan: &SubPlan,
        accs: &mut [Vec<MetricAcc>],
        tops: &mut [Vec<(f64, u64, DocRef)>],
        missing_sort_seen: &mut bool,
    ) -> Option<()> {
        let seg = &self.segs[si];
        // Top-level query filter: resolve its columnar predicate for this
        // segment ONCE (bail the whole fast path if it can't be resolved —
        // e.g. field-kind mismatch — so the caller uses the brute path).
        // Rows that don't match are skipped before any bucket assignment, so
        // every fused-pass consumer (terms, filtered metric) reduces over
        // exactly the matching doc set.
        let top_sp: Option<SegPred> = match &self.top_filter {
            Some(pred) => Some(resolve_pred(&seg.cols, pred)?),
            None => None,
        };
        // Resolve metric columns once.
        let mut num_cols: Vec<Option<&NumericColumn>> = Vec::with_capacity(plan.metrics.len());
        let mut kw_cols: Vec<Option<&KeywordColumn>> = Vec::with_capacity(plan.metrics.len());
        for m in &plan.metrics {
            match seg.cols.get(&m.field) {
                Some(Column::Numeric(n)) => {
                    num_cols.push(Some(n));
                    kw_cols.push(None);
                }
                Some(Column::Keyword(k)) => {
                    if m.kind != MetricKind::ValueCount {
                        return None;
                    }
                    num_cols.push(None);
                    kw_cols.push(Some(k));
                }
                None => {
                    num_cols.push(None);
                    kw_cols.push(None);
                }
            }
        }
        let th_col: Option<&NumericColumn> = match &plan.top_hits {
            Some(spec) => match seg.cols.get(&spec.sort_field) {
                Some(Column::Numeric(n)) => Some(n),
                Some(_) => return None,
                None => None,
            },
            None => None,
        };
        let rank_off = self.seg_rank_offset(si);
        debug_assert!(accs.len() == plan.metrics.len());
        let _ = n_slots;

        // Hoist the RoaringBitmap null probes: dense columns (no nulls —
        // the steady state for telemetry-style corpora) skip the per-row
        // `contains()` entirely, which otherwise dominates this loop.
        let dense_num: Vec<bool> = num_cols
            .iter()
            .map(|c| c.is_some_and(|n| n.null_bitmap.is_empty()))
            .collect();
        let dense_kw: Vec<bool> = kw_cols
            .iter()
            .map(|c| c.is_some_and(|k| k.null_bitmap.is_empty()))
            .collect();
        let th_dense = th_col.is_some_and(|n| n.null_bitmap.is_empty());

        for row in 0..seg.docs {
            if let Some(sp) = &top_sp {
                if !seg_pred_matches(sp, row) {
                    continue;
                }
            }
            let Some(slot) = slot_of_row(row) else {
                continue;
            };
            for (mi, m) in plan.metrics.iter().enumerate() {
                match m.kind {
                    MetricKind::ValueCount => {
                        let present = if let Some(n) = num_cols[mi] {
                            dense_num[mi] || !n.null_bitmap.contains(row)
                        } else if let Some(k) = kw_cols[mi] {
                            dense_kw[mi] || !k.null_bitmap.contains(row)
                        } else {
                            false
                        };
                        if present {
                            accs[mi][slot].add_presence();
                        }
                    }
                    _ => {
                        if let Some(n) = num_cols[mi] {
                            if dense_num[mi] || !n.null_bitmap.contains(row) {
                                let v = f64::from_bits(n.data[row as usize] as u64);
                                accs[mi][slot].add(v);
                            }
                        }
                    }
                }
            }
            if let Some(spec) = &plan.top_hits {
                let v = match th_col {
                    Some(n) if th_dense || !n.null_bitmap.contains(row) => {
                        f64::from_bits(n.data[row as usize] as u64)
                    }
                    _ => {
                        *missing_sort_seen = true;
                        continue;
                    }
                };
                push_top(
                    &mut tops[slot],
                    spec.k,
                    spec.desc,
                    v,
                    rank_off + row as u64,
                    DocRef::Seg(si, row),
                );
            }
        }
        Some(())
    }

    /// Materialise + render a bucket's top_hits from its winner set.
    fn render_top_hits(
        &self,
        spec: &TopHitsSpec,
        winners: &[(f64, u64, DocRef)],
        bucket_total: u64,
    ) -> Option<Value> {
        let mut sorted: Vec<(f64, u64, DocRef)> = winners.to_vec();
        sort_top(&mut sorted, spec.desc);
        let docs: Vec<Value> = sorted
            .iter()
            .map(|(_, _, r)| self.fetch_doc(*r))
            .collect::<Option<Vec<_>>>()?;
        Some(run_top_hits_with_total(
            &spec.params,
            &docs,
            Some(bucket_total),
        ))
    }

    // ── terms ────────────────────────────────────────────────────────────

    /// `cardinality` — exact distinct count from the keyword term
    /// dictionaries, matching the brute path bit-for-bit (xerj's
    /// cardinality is exact, not HLL — see aggs.rs `run_cardinality`).
    ///
    /// Pre-fix `cardinality` was NOT on the fast path, so every uncached
    /// `size:0 + match_all + cardinality` request fell into the
    /// `need_full_corpus` branch of `search_inner`: decompress + JSON-parse
    /// EVERY stored doc in the index (~22 s at 3.5 M docs).  Steady-state
    /// dashboards never noticed because the query cache absorbed it, but a
    /// background bulk writer bumps `dataset_version` on every batch — 100 %
    /// cache misses — and cardinality became the single biggest CPU bomb in
    /// the read-under-write collapse.
    ///
    /// Keyword-shaped columns only (plus schema-boolean 0/1 numerics):
    /// numeric distinct values can't be re-rendered into the brute path's
    /// string keys without risking formatting drift → bail to brute.
    fn exec_cardinality(&self, params: &Value, sub: Option<&Value>) -> Option<Value> {
        if sub.is_some() {
            return None;
        }
        // `precision_threshold` is accepted-and-ignored by the brute path
        // (exact count regardless), so accepting it here is bit-identical.
        if !params_only(params, &["field", "missing", "precision_threshold"]) {
            return None;
        }
        let field = params.get("field").and_then(Value::as_str)?;
        let is_bool = self.bool_fields.contains(field);
        match self.seg_field_kind(field) {
            Ok(Some(ColKind::Keyword)) | Ok(None) => {}
            Ok(Some(ColKind::Numeric)) if is_bool => {}
            _ => return None,
        }
        // Same `missing` placeholder semantics as `run_cardinality`.
        let missing_placeholder: Option<String> = params.get("missing").and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        });

        let mut distinct: std::collections::HashSet<String> = std::collections::HashSet::new();
        const BOOL_TERMS: [&str; 2] = ["false", "true"];
        // Under a top-level query filter the distinct set must cover ONLY the
        // matching docs — the whole-segment `per_ord_count` shortcut (which
        // sees every doc) is valid only for the unfiltered path. The empty
        // plan drives `fused_seg_pass` purely for its columnar filter gate;
        // `slot_of_row` records which term-ords appear among matching rows.
        // This is what stops a `{size:0, query:range, cardinality}` from
        // bailing the whole request to the O(N) brute `_source` hydrate
        // (~1.2 s on a 600 k-doc unflushed memtable) — the mixed-read tail.
        let filtered = self.top_filter.is_some();
        let empty_plan = if filtered {
            Some(self.plan_subs(None, false)?)
        } else {
            None
        };
        for si in 0..self.segs.len() {
            let seg = &self.segs[si];
            if !filtered {
                // ── Unfiltered: whole-segment term-dictionary scan (fast) ──
                match seg.cols.get(field) {
                    Some(Column::Keyword(k)) => {
                        for (ord, &cnt) in k.per_ord_count.iter().enumerate() {
                            if cnt > 0 {
                                distinct.insert(k.terms[ord].clone());
                            }
                        }
                        if let Some(ph) = &missing_placeholder {
                            if !k.null_bitmap.is_empty() {
                                distinct.insert(ph.clone());
                            }
                        }
                    }
                    Some(Column::Numeric(n)) if is_bool => {
                        // Same 0/1 purity check as exec_terms: stray numbers
                        // would render as "0"/"2.5" on the brute path → bail.
                        let zeros = n.range_count(0.0, 0.0, true, true);
                        let ones = n.range_count(1.0, 1.0, true, true);
                        if zeros + ones != n.live_count {
                            return None;
                        }
                        if zeros > 0 {
                            distinct.insert(BOOL_TERMS[0].to_string());
                        }
                        if ones > 0 {
                            distinct.insert(BOOL_TERMS[1].to_string());
                        }
                        if let Some(ph) = &missing_placeholder {
                            if !n.null_bitmap.is_empty() {
                                distinct.insert(ph.clone());
                            }
                        }
                    }
                    Some(_) => return None,
                    None => {
                        // Field absent from the whole segment: every doc counts
                        // as missing (brute: empty vals → placeholder).
                        if let Some(ph) = &missing_placeholder {
                            if seg.docs > 0 {
                                distinct.insert(ph.clone());
                            }
                        }
                    }
                }
                continue;
            }
            // ── Filtered: per-row pass, distinct over matching rows only ──
            let plan = empty_plan.as_ref().unwrap();
            let mut mss = false;
            match seg.cols.get(field) {
                Some(Column::Keyword(k)) => {
                    let mut seen = vec![false; k.terms.len()];
                    let mut saw_missing = false;
                    {
                        let dense = k.null_bitmap.is_empty();
                        let mut slot_of_row = |row: u32| -> Option<usize> {
                            let ord = if dense {
                                *k.ords.get(row as usize)? as usize
                            } else {
                                match k.ord_for(row) {
                                    Some(o) => o as usize,
                                    None => {
                                        saw_missing = true;
                                        return None;
                                    }
                                }
                            };
                            seen[ord] = true;
                            Some(ord)
                        };
                        self.fused_seg_pass(
                            si,
                            &mut slot_of_row,
                            k.terms.len(),
                            plan,
                            &mut [],
                            &mut [],
                            &mut mss,
                        )?;
                    }
                    for (ord, &s) in seen.iter().enumerate() {
                        if s {
                            distinct.insert(k.terms[ord].clone());
                        }
                    }
                    if saw_missing {
                        if let Some(ph) = &missing_placeholder {
                            distinct.insert(ph.clone());
                        }
                    }
                }
                Some(Column::Numeric(n)) if is_bool => {
                    let zeros = n.range_count(0.0, 0.0, true, true);
                    let ones = n.range_count(1.0, 1.0, true, true);
                    if zeros + ones != n.live_count {
                        return None;
                    }
                    let mut seen_bool = [false, false];
                    let mut saw_missing = false;
                    {
                        let dense = n.null_bitmap.is_empty();
                        let mut slot_of_row = |row: u32| -> Option<usize> {
                            let bits = if dense {
                                *n.data.get(row as usize)?
                            } else {
                                match n.get(row) {
                                    Some(b) => b,
                                    None => {
                                        saw_missing = true;
                                        return None;
                                    }
                                }
                            };
                            let idx = usize::from(f64::from_bits(bits as u64) != 0.0);
                            seen_bool[idx] = true;
                            Some(idx)
                        };
                        self.fused_seg_pass(
                            si,
                            &mut slot_of_row,
                            2,
                            plan,
                            &mut [],
                            &mut [],
                            &mut mss,
                        )?;
                    }
                    if seen_bool[0] {
                        distinct.insert(BOOL_TERMS[0].to_string());
                    }
                    if seen_bool[1] {
                        distinct.insert(BOOL_TERMS[1].to_string());
                    }
                    if saw_missing {
                        if let Some(ph) = &missing_placeholder {
                            distinct.insert(ph.clone());
                        }
                    }
                }
                Some(_) => return None,
                None => {
                    // Field absent from this segment → every matching row is
                    // "missing"; only need whether ≥1 row matches the filter.
                    if let Some(ph) = &missing_placeholder {
                        let mut any = false;
                        {
                            let mut slot_of_row = |_row: u32| -> Option<usize> {
                                any = true;
                                None
                            };
                            self.fused_seg_pass(
                                si,
                                &mut slot_of_row,
                                0,
                                plan,
                                &mut [],
                                &mut [],
                                &mut mss,
                            )?;
                        }
                        if any {
                            distinct.insert(ph.clone());
                        }
                    }
                }
            }
        }
        // ── Memtable docs ─────────────────────────────────────────────────
        if filtered {
            // Distinct over MATCHING memtable docs only. O(matching) columnar
            // candidates when the filter columnarises (re-checked against the
            // JSON matcher for byte-identical results); else the full walk
            // gated by `doc_matches_filter`.
            let cols = match (self.filtered_mem(), self.top_filter_query.as_ref()) {
                (Some(fdocs), Some(q)) => Some((fdocs, q)),
                _ => None,
            };
            if let Some((fdocs, q)) = cols {
                for (_id, doc) in fdocs {
                    if !doc_matches_filter(doc, q) {
                        continue;
                    }
                    let vals = extract_field_values(doc, field);
                    if vals.is_empty() {
                        if let Some(ph) = &missing_placeholder {
                            distinct.insert(ph.clone());
                        }
                    } else {
                        for v in vals {
                            distinct.insert(v);
                        }
                    }
                }
            } else {
                for doc in self.mem().iter() {
                    if let Some(q) = &self.top_filter_query {
                        if !doc_matches_filter(doc, q) {
                            continue;
                        }
                    }
                    let vals = extract_field_values(doc, field);
                    if vals.is_empty() {
                        if let Some(ph) = &missing_placeholder {
                            distinct.insert(ph.clone());
                        }
                    } else {
                        for v in vals {
                            distinct.insert(v);
                        }
                    }
                }
            }
        } else {
            // Unfiltered memtable.  Columnar fast arm first (same equivalence
            // gates as exec_terms — see `terms_counts_columnar`); the per-doc
            // extraction walk below was the cardinality read-under-write tail
            // at a drain-lagged memtable.
            let mem_columnar: Option<(std::collections::HashMap<String, u64>, u64)> =
                if !field.contains('.') {
                    self.idx.memtable.terms_counts_columnar(field)
                } else {
                    None
                };
            match mem_columnar {
                Some((counts, missing)) => {
                    for (term, cnt) in counts {
                        if cnt > 0 {
                            distinct.insert(term);
                        }
                    }
                    if missing > 0 {
                        if let Some(ph) = &missing_placeholder {
                            distinct.insert(ph.clone());
                        }
                    }
                }
                None => {
                    // Exact brute extractor so string rendering is identical.
                    for d in self.mem().iter() {
                        let vals = crate::aggs::extract_field_values(d, field);
                        if vals.is_empty() {
                            if let Some(ph) = &missing_placeholder {
                                distinct.insert(ph.clone());
                            }
                            continue;
                        }
                        for v in vals {
                            distinct.insert(v);
                        }
                    }
                }
            }
        }

        // Identical output shape to `run_cardinality` including the
        // internal `__xy_*` fields the cross-index merge consumes.
        let values: Vec<Value> = distinct.iter().map(|s| Value::String(s.clone())).collect();
        Some(json!({
            "value": distinct.len(),
            "__xy_agg__": "cardinality",
            "__xy_values__": values,
        }))
    }

    fn exec_terms(&self, params: &Value, sub: Option<&Value>) -> Option<Value> {
        if !params_only(
            params,
            &["field", "size", "order", "shard_size", "min_doc_count"],
        ) {
            return None;
        }
        // Brute default & only supported min_doc_count is 1 (0 needs the
        // background corpus, >1 is honored below since it's a plain filter).
        let min_doc_count = params
            .get("min_doc_count")
            .and_then(Value::as_u64)
            .unwrap_or(1);
        if params.get("min_doc_count").is_some() && min_doc_count == 0 {
            return None;
        }
        let field = params.get("field").and_then(Value::as_str)?;
        // Boolean-mapped fields land in numeric 0/1 columns; the schema is
        // what licenses rendering them back as "false"/"true" term keys
        // (`typed_term_key` then emits key 0/1 + key_as_string, exactly
        // like the brute path does for `Value::Bool`).
        let is_bool = self.bool_fields.contains(field);
        match self.seg_field_kind(field) {
            Ok(Some(ColKind::Keyword)) | Ok(None) => {}
            Ok(Some(ColKind::Numeric)) if is_bool => {}
            _ => return None, // numeric terms keys → typed-key subtleties, bail
        }
        // Orders: only _count/_key entries supported (sub-agg ordering needs
        // the per-bucket sub results *before* truncation — brute-only).
        let orders = parse_orders(params);
        if orders.iter().any(|(k, _)| k != "_count" && k != "_key") {
            return None;
        }
        let size_opt: Option<usize> = params
            .get("size")
            .and_then(Value::as_u64)
            .map(|v| v as usize);
        let cap: Option<usize> = match size_opt {
            Some(0) => None,
            Some(n) => Some(n),
            None => Some(10),
        };

        let plan = self.plan_subs(sub, true)?;
        // A top-level query filter forces the per-row pass (the `!has_row_work`
        // segment shortcut reads whole-segment `per_ord_count`, which ignores
        // the filter).  `fused_seg_pass` then gates rows by the filter, and
        // the memtable arm below matches each doc against it.
        let has_row_work =
            !plan.metrics.is_empty() || plan.top_hits.is_some() || self.top_filter.is_some();

        // Global per-term state.
        struct TermState {
            count: u64,
            accs: Vec<MetricAcc>,
            top: Vec<(f64, u64, DocRef)>,
        }
        let mut terms_map: HashMap<String, TermState> = HashMap::new();
        let new_state = |plan: &SubPlan| TermState {
            count: 0,
            accs: vec![MetricAcc::default(); plan.metrics.len()],
            top: Vec::new(),
        };
        let mut missing_sort_seen = false;

        // Per-segment term column view: keyword ords, or a boolean numeric
        // column exposed as the two-term dictionary ["false", "true"].
        enum TCol<'x> {
            Kw(&'x KeywordColumn),
            Bool(&'x NumericColumn),
        }
        const BOOL_TERMS: [&str; 2] = ["false", "true"];

        for si in 0..self.segs.len() {
            let seg = &self.segs[si];
            let Some(col) = seg.cols.get(field) else {
                continue;
            };
            let tcol = match col {
                Column::Keyword(k) => TCol::Kw(k),
                Column::Numeric(n) if is_bool => {
                    // Sanity: the column must be pure 0/1 (a mapping change
                    // could have let stray numbers in — brute would render
                    // those as "0"/"1"/"2.5", not booleans → bail).
                    let zeros = n.range_count(0.0, 0.0, true, true);
                    let ones = n.range_count(1.0, 1.0, true, true);
                    if zeros + ones != n.live_count {
                        return None;
                    }
                    TCol::Bool(n)
                }
                _ => return None,
            };
            if !has_row_work {
                match &tcol {
                    TCol::Kw(k) => {
                        for (ord, &cnt) in k.per_ord_count.iter().enumerate() {
                            if cnt > 0 {
                                terms_map
                                    .entry(k.terms[ord].clone())
                                    .or_insert_with(|| new_state(&plan))
                                    .count += cnt as u64;
                            }
                        }
                    }
                    TCol::Bool(n) => {
                        for (ord, term) in BOOL_TERMS.iter().enumerate() {
                            let v = ord as f64;
                            let cnt = n.range_count(v, v, true, true);
                            if cnt > 0 {
                                terms_map
                                    .entry((*term).to_string())
                                    .or_insert_with(|| new_state(&plan))
                                    .count += cnt;
                            }
                        }
                    }
                }
                continue;
            }
            // Row pass: per-ord local accumulators, then merged by term.
            let n_ords = match &tcol {
                TCol::Kw(k) => k.terms.len(),
                TCol::Bool(_) => 2,
            };
            let term_of_ord = |ord: usize| -> &str {
                match &tcol {
                    TCol::Kw(k) => k.terms[ord].as_str(),
                    TCol::Bool(_) => BOOL_TERMS[ord],
                }
            };
            let mut accs: Vec<Vec<MetricAcc>> = plan
                .metrics
                .iter()
                .map(|_| vec![MetricAcc::default(); n_ords])
                .collect();
            let mut tops: Vec<Vec<(f64, u64, DocRef)>> = vec![Vec::new(); n_ords];
            let mut counts: Vec<u64> = vec![0; n_ords];
            {
                // Dense term column (no nulls) → skip the per-row roaring
                // `contains()` inside `ord_for`/`get` and read the raw
                // ordinal/value arrays directly.
                let tcol_dense = match &tcol {
                    TCol::Kw(k) => k.null_bitmap.is_empty(),
                    TCol::Bool(n) => n.null_bitmap.is_empty(),
                };
                let mut slot_of_row = |row: u32| -> Option<usize> {
                    let ord = match &tcol {
                        TCol::Kw(k) => {
                            if tcol_dense {
                                *k.ords.get(row as usize)? as usize
                            } else {
                                k.ord_for(row)? as usize
                            }
                        }
                        TCol::Bool(n) => {
                            let bits = if tcol_dense {
                                *n.data.get(row as usize)?
                            } else {
                                n.get(row)?
                            };
                            usize::from(f64::from_bits(bits as u64) != 0.0)
                        }
                    };
                    counts[ord] += 1;
                    Some(ord)
                };
                self.fused_seg_pass(
                    si,
                    &mut slot_of_row,
                    n_ords,
                    &plan,
                    &mut accs,
                    &mut tops,
                    &mut missing_sort_seen,
                )?;
            }
            for ord in 0..n_ords {
                if counts[ord] == 0 {
                    continue;
                }
                let st = terms_map
                    .entry(term_of_ord(ord).to_string())
                    .or_insert_with(|| new_state(&plan));
                st.count += counts[ord];
                for (mi, acc) in st.accs.iter_mut().enumerate() {
                    merge_acc(acc, &accs[mi][ord]);
                }
                if let Some(spec) = &plan.top_hits {
                    for e in tops[ord].drain(..) {
                        push_top(&mut st.top, spec.k, spec.desc, e.0, e.1, e.2);
                    }
                }
            }
        }

        // Memtable docs.  Columnar fast arm first: when the shape is
        // provably brute-equivalent (no per-row sub-agg work, plain
        // un-dotted field, no array values ever seen for it, no
        // `_doc_count` weights — see `terms_counts_columnar`), count from
        // the keyword column directly.  The per-doc JSON extraction walk
        // below cost 100-300 ms/query against a drain-lagged 300 k-doc
        // memtable under a sustained bulk writer — the terms-agg
        // read-under-write p95/p99 tail.
        let mem_columnar: Option<(std::collections::HashMap<String, u64>, u64)> =
            if !has_row_work && !field.contains('.') {
                self.idx.memtable.terms_counts_columnar(field)
            } else {
                None
            };
        match mem_columnar {
            Some((counts, _missing)) => {
                for (term, cnt) in counts {
                    if cnt > 0 {
                        terms_map
                            .entry(term)
                            .or_insert_with(|| new_state(&plan))
                            .count += cnt;
                    }
                }
            }
            None => {
                // O(matching) columnar fold: when the top filter columnarises
                // AND no meta-observing sub-agg is present (`filtered_mem` is
                // Some ⇒ `needs_owned_mem` false ⇒ `plan.top_hits` is None, so
                // the positional `DocRef::Mem` path is unreachable here), fold
                // only the columnar-matched candidates — re-checked against the
                // JSON matcher for byte-identical results.  Otherwise the full
                // `mem()` walk (brute extractor semantics; multi-value, weights,
                // top_hits) exactly as before.
                let columnar = match (self.filtered_mem(), self.top_filter_query.as_ref()) {
                    (Some(fdocs), Some(q)) if plan.top_hits.is_none() => Some((fdocs, q)),
                    _ => None,
                };
                if let Some((fdocs, q)) = columnar {
                    for (_id, doc) in fdocs {
                        if !doc_matches_filter(doc, q) {
                            continue;
                        }
                        let weight = doc_count_weight(doc);
                        let vals = extract_field_values(doc, field);
                        for term in vals {
                            let st = terms_map.entry(term).or_insert_with(|| new_state(&plan));
                            st.count += weight;
                            for (mi, spec) in plan.metrics.iter().enumerate() {
                                Self::fold_mem_metric(doc, spec, &mut st.accs[mi]);
                            }
                        }
                    }
                } else {
                    for (di, doc) in self.mem().iter().enumerate() {
                        // Top-level query filter: skip non-matching memtable docs
                        // (segment rows are already gated inside `fused_seg_pass`).
                        if let Some(q) = &self.top_filter_query {
                            if !doc_matches_filter(doc, q) {
                                continue;
                            }
                        }
                        let weight = doc_count_weight(doc);
                        let vals = extract_field_values(doc, field);
                        for term in vals {
                            let st = terms_map.entry(term).or_insert_with(|| new_state(&plan));
                            st.count += weight;
                            for (mi, spec) in plan.metrics.iter().enumerate() {
                                Self::fold_mem_metric(doc, spec, &mut st.accs[mi]);
                            }
                            if let Some(spec) = &plan.top_hits {
                                match extract_numeric(doc, &spec.sort_field) {
                                    Some(v) => push_top(
                                        &mut st.top,
                                        spec.k,
                                        spec.desc,
                                        v,
                                        di as u64,
                                        DocRef::Mem(di),
                                    ),
                                    None => missing_sort_seen = true,
                                }
                            }
                        }
                    }
                }
            }
        }

        if plan.top_hits.is_some() && missing_sort_seen {
            // A bucket doc without the sort value would need brute
            // missing-last + _id tie-breaking — bail for exactness.
            return None;
        }

        // Order + truncate, mirroring run_terms' default and _count/_key
        // orders (cmp_terms_by_orders with no sub-agg references).
        let mut candidates: Vec<(String, TermState)> = terms_map
            .into_iter()
            .filter(|(_, st)| st.count >= min_doc_count)
            .collect();
        candidates.sort_by(|a, b| {
            cmp_by_orders(
                &(a.0.clone(), a.1.count),
                &(b.0.clone(), b.1.count),
                &orders,
            )
        });
        if let Some(n) = cap {
            candidates.truncate(n);
        }

        let mut buckets: Vec<Value> = Vec::with_capacity(candidates.len());
        for (key, st) in candidates {
            let (typed_key, key_as_string) = typed_term_key(&key);
            let mut bucket = Map::new();
            bucket.insert("key".to_string(), typed_key);
            if let Some(kas) = key_as_string {
                bucket.insert("key_as_string".to_string(), json!(kas));
            }
            bucket.insert("doc_count".to_string(), json!(st.count));
            let th = match &plan.top_hits {
                Some(spec) => Some(self.render_top_hits(spec, &st.top, st.count)?),
                None => None,
            };
            self.finish_bucket(&mut bucket, &plan, &st.accs, th);
            buckets.push(Value::Object(bucket));
        }
        let buckets = apply_bucket_pipeline_ops(buckets, sub);

        Some(json!({
            "doc_count_error_upper_bound": 0,
            "sum_other_doc_count": 0,
            "buckets": buckets
        }))
    }

    // ── rare_terms ───────────────────────────────────────────────────────

    /// Columnar `rare_terms`: `terms` restricted to the low-frequency tail
    /// (buckets whose GLOBAL `doc_count <= max_doc_count`, default 1).
    ///
    /// Brute (`run_rare_terms`) JSON-walks every doc to build `term → doc`
    /// indices, then keeps the rare tail — O(N) (≈1 s / 100 k docs). This arm
    /// reuses the exact per-term counting of `exec_terms`' whole-segment
    /// shortcut (segments' `per_ord_count` + the memtable columnar folder), then
    /// applies the same rare filter and the same `count asc, key asc` ordering
    /// and typed-key bucket rendering the brute path uses — so the result is
    /// byte-identical, just O(distinct terms) instead of O(N).
    ///
    /// Deliberately narrow — bails (`None` → exact brute) on anything the
    /// counting/rendering can't reproduce exactly: sub-aggs (need per-bucket
    /// docs), `include`/`exclude`/`missing`/`precision` (params_only), a
    /// non-keyword field (numeric/bool/date term-key subtleties render
    /// differently), a dotted field, or a memtable shape the columnar folder
    /// can't count (arrays / `_doc_count` weights). A top-level query filter is
    /// already gated out by `exec_agg` (rare_terms is not in its allowlist).
    fn exec_rare_terms(&self, params: &Value, sub: Option<&Value>) -> Option<Value> {
        if sub.is_some() {
            return None;
        }
        if !params_only(params, &["field", "max_doc_count"]) {
            return None;
        }
        // `exec_agg` only reaches this arm when there is no top-level query
        // filter (rare_terms is not in the filtered-executor allowlist), so the
        // corpus is the whole index — matching `run_rare_terms(docs=all_docs)`.
        debug_assert!(self.top_filter.is_none());
        let field = params.get("field").and_then(Value::as_str)?;
        if field.contains('.') {
            return None;
        }
        let max_doc_count = params
            .get("max_doc_count")
            .and_then(Value::as_u64)
            .unwrap_or(1);
        // Keyword-only: a numeric/bool/date-numeric term column would render its
        // keys differently from the brute string path.
        match self.seg_field_kind(field) {
            Ok(Some(ColKind::Keyword)) | Ok(None) => {}
            _ => return None,
        }

        // Whole-corpus per-term counts (identical to `exec_terms`' `!has_row_work`
        // shortcut): segments' `per_ord_count` + the memtable columnar folder.
        let mut counts: HashMap<String, u64> = HashMap::new();
        for seg in &self.segs {
            match seg.cols.get(field) {
                Some(Column::Keyword(k)) => {
                    for (ord, &cnt) in k.per_ord_count.iter().enumerate() {
                        // Skip empty-string values — `run_rare_terms` drops them
                        // (`extract_field_values` → `if v.is_empty() continue`).
                        if cnt > 0 && !k.terms[ord].is_empty() {
                            *counts.entry(k.terms[ord].clone()).or_default() += cnt as u64;
                        }
                    }
                }
                None => {}        // field absent in this segment → no terms
                _ => return None, // non-keyword column for a keyword field → bail
            }
        }
        match self.idx.memtable.terms_counts_columnar(field) {
            Some((mem_counts, _missing)) => {
                for (term, cnt) in mem_counts {
                    if cnt > 0 && !term.is_empty() {
                        *counts.entry(term).or_default() += cnt;
                    }
                }
            }
            // Array-valued field / `_doc_count` weights the columnar folder
            // can't reproduce → exact brute.
            None => return None,
        }

        // Rare filter + brute ordering (`count asc, key asc`, no truncation —
        // rare_terms has no `size`), mirroring `run_rare_terms`.
        let mut entries: Vec<(String, u64)> = counts
            .into_iter()
            .filter(|(_, c)| *c <= max_doc_count)
            .collect();
        entries.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

        let buckets: Vec<Value> = entries
            .into_iter()
            .map(|(key, count)| {
                let (typed_key, key_as_string) = typed_term_key(&key);
                let mut b = Map::new();
                b.insert("key".to_string(), typed_key);
                if let Some(kas) = key_as_string {
                    b.insert("key_as_string".to_string(), json!(kas));
                }
                b.insert("doc_count".to_string(), json!(count));
                Value::Object(b)
            })
            .collect();

        Some(json!({ "buckets": buckets }))
    }

    // ── significant_terms (degenerate no-query case only) ─────────────────

    /// Columnar `significant_terms` for the ONE case that is provably an empty
    /// result: no top-level query and no `background_filter`, so the foreground
    /// set equals the background set (the whole index). Every term then has
    /// `fg_freq == bg_freq`, so the JLH significance score is exactly `0.0` for
    /// all terms, and with the default `min_doc_count >= 2` (no threshold
    /// bypass) every term is dropped — brute (`run_significant_terms`) returns
    /// `{doc_count: N, bg_count: N, buckets: []}`. This arm returns the same in
    /// O(1)+segment-count instead of the O(N) double JSON walk (≈1 s / 100 k).
    ///
    /// Everything else bails to exact brute (`None`): a `background_filter`
    /// (foreground != background), `min_doc_count <= 1` (threshold bypass keeps
    /// score-0 terms in a HashMap-order-dependent, non-reproducible order),
    /// sub-aggs, or any param outside `field`/`size`/`min_doc_count`. A
    /// top-level query filter is already gated out by `exec_agg`
    /// (significant_terms is not in its allowlist), which is exactly what makes
    /// foreground == background here.
    fn exec_significant_terms(&self, params: &Value, sub: Option<&Value>) -> Option<Value> {
        if sub.is_some() {
            return None;
        }
        if !params_only(params, &["field", "size", "min_doc_count"]) {
            return None; // background_filter / include / exclude / … → brute
        }
        debug_assert!(self.top_filter.is_none());
        // Field must exist as a real agg field (keyword or numeric). `None`
        // (fully unmapped) still yields empty buckets in brute, but keep the arm
        // to the mapped case so `doc_count`/`bg_count` are unambiguous.
        params.get("field").and_then(Value::as_str)?;
        // Default ES `min_doc_count` is 3; only `<= 1` bypasses the score-0
        // filter (and produces non-reproducible ordering) → brute.
        let min_doc_count = params
            .get("min_doc_count")
            .and_then(Value::as_u64)
            .unwrap_or(3);
        if min_doc_count <= 1 {
            return None;
        }
        // Whole-corpus live doc count = brute's `result_docs.len()` ==
        // `all_docs.len()` (no deletes on the fast path; `run_significant_terms`
        // counts docs, unweighted). Foreground == background ⇒ every term scores
        // 0.0 ⇒ empty buckets.
        let total: u64 = self.segs.iter().map(|s| s.docs as u64).sum::<u64>()
            + self.idx.memtable.doc_count() as u64;
        Some(json!({
            "doc_count": total,
            "bg_count": total,
            "buckets": [],
        }))
    }

    // ── range (numeric) ──────────────────────────────────────────────────

    fn exec_range(&self, params: &Value, sub: Option<&Value>) -> Option<Value> {
        if !params_only(params, &["field", "ranges", "keyed"]) {
            return None;
        }
        let field = params.get("field").and_then(Value::as_str)?;
        let ranges = params.get("ranges").and_then(Value::as_array)?;
        if ranges.is_empty() {
            return None;
        }
        // Numeric-only (string bounds switch brute into date mode).
        for r in ranges {
            let o = r.as_object()?;
            if !o.keys().all(|k| k == "from" || k == "to" || k == "key") {
                return None;
            }
            for b in ["from", "to"] {
                if let Some(v) = o.get(b) {
                    if !v.is_number() {
                        return None;
                    }
                }
            }
        }
        match self.seg_field_kind(field) {
            Ok(Some(ColKind::Numeric)) | Ok(None) => {}
            _ => return None,
        }
        if !self.mem_field_numeric_safe(field) {
            return None;
        }
        let plan = self.plan_subs(sub, false)?;

        let mut buckets: Vec<Value> = Vec::with_capacity(ranges.len());
        for range_def in ranges {
            let from: Option<f64> = range_def.get("from").and_then(Value::as_f64);
            let to: Option<f64> = range_def.get("to").and_then(Value::as_f64);
            let lo = from.unwrap_or(f64::NEG_INFINITY);
            let hi = to.unwrap_or(f64::INFINITY);
            // match: n >= from && n < to  (to == +inf → inclusive is moot)
            let hi_incl = to.is_none();

            let mut count: u64 = 0;
            let mut accs: Vec<MetricAcc> = vec![MetricAcc::default(); plan.metrics.len()];
            for si in 0..self.segs.len() {
                let seg = &self.segs[si];
                let Some(Column::Numeric(n)) = seg.cols.get(field) else {
                    continue;
                };
                if plan.metrics.is_empty() {
                    count += n.range_count(lo, hi, true, hi_incl);
                } else {
                    let rows = n.range_doc_ids(lo, hi, true, hi_incl);
                    count += rows.len() as u64;
                    let mut wrapped: Vec<Vec<MetricAcc>> =
                        accs.iter().map(|_| vec![MetricAcc::default()]).collect();
                    let mut tops: Vec<Vec<(f64, u64, DocRef)>> = vec![Vec::new()];
                    // Dense membership bitmap — the fused row pass probes
                    // every row of the segment, so a HashSet here costs a
                    // SipHash per row per range (~50 ms/M rows); a Vec<bool>
                    // probe is ~1 ns.
                    let mut in_range: Vec<bool> = vec![false; seg.docs as usize];
                    for r in rows {
                        if let Some(slot) = in_range.get_mut(r as usize) {
                            *slot = true;
                        }
                    }
                    let mut slot = |row: u32| -> Option<usize> {
                        if in_range.get(row as usize).copied().unwrap_or(false) {
                            Some(0)
                        } else {
                            None
                        }
                    };
                    let mut ms = false;
                    self.fused_seg_pass(si, &mut slot, 1, &plan, &mut wrapped, &mut tops, &mut ms)?;
                    for (mi, w) in wrapped.iter().enumerate() {
                        merge_acc(&mut accs[mi], &w[0]);
                    }
                }
            }
            for doc in self.mem().iter() {
                let Some(v) = extract_numeric(doc, field) else {
                    continue;
                };
                let matches = match (from, to) {
                    (Some(f), Some(t)) => v >= f && v < t,
                    (Some(f), None) => v >= f,
                    (None, Some(t)) => v < t,
                    (None, None) => true,
                };
                if !matches {
                    continue;
                }
                count += 1;
                for (mi, spec) in plan.metrics.iter().enumerate() {
                    Self::fold_mem_metric(doc, spec, &mut accs[mi]);
                }
            }

            let key = range_def
                .get("key")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| match (from, to) {
                    (Some(f), Some(t)) => {
                        format!("{}-{}", format_range_val(f), format_range_val(t))
                    }
                    (Some(f), None) => format!("{}-*", format_range_val(f)),
                    (None, Some(t)) => format!("*-{}", format_range_val(t)),
                    (None, None) => "*-*".to_string(),
                });
            let mut bucket = Map::new();
            bucket.insert("key".to_string(), json!(key));
            bucket.insert("doc_count".to_string(), json!(count));
            if let Some(f) = from {
                bucket.insert("from".to_string(), json!(f));
            }
            if let Some(t) = to {
                bucket.insert("to".to_string(), json!(t));
            }
            self.finish_bucket(&mut bucket, &plan, &accs, None);
            buckets.push(Value::Object(bucket));
        }
        let buckets = apply_bucket_pipeline_ops(buckets, sub);

        let keyed = params
            .get("keyed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if keyed {
            let mut keyed_map = Map::new();
            for mut b in buckets {
                let k = b
                    .get("key")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if let Some(obj) = b.as_object_mut() {
                    obj.remove("key");
                }
                keyed_map.insert(k, b);
            }
            return Some(json!({ "buckets": Value::Object(keyed_map) }));
        }
        Some(json!({ "buckets": buckets }))
    }

    // ── date_range ───────────────────────────────────────────────────────

    fn exec_date_range(&self, params: &Value, sub: Option<&Value>) -> Option<Value> {
        if sub.is_some() {
            return None;
        }
        if !params_only(params, &["field", "ranges", "keyed"]) {
            return None;
        }
        let field = params
            .get("field")
            .and_then(Value::as_str)
            .unwrap_or("@timestamp");
        let ranges = params.get("ranges").and_then(Value::as_array)?;
        match self.seg_field_kind(field) {
            Ok(Some(ColKind::Keyword)) | Ok(None) => {}
            _ => return None, // numeric-date columns: brute parse semantics differ; bail
        }

        // Fractional-precision detection — mirror run_date_range.
        let nanos_digits: usize = ranges
            .iter()
            .flat_map(|r| {
                let f = r
                    .get("from")
                    .and_then(Value::as_str)
                    .map(detect_fractional_digits)
                    .unwrap_or(0);
                let t = r
                    .get("to")
                    .and_then(Value::as_str)
                    .map(detect_fractional_digits)
                    .unwrap_or(0);
                [f, t]
            })
            .max()
            .unwrap_or(3);
        let nanos_digits = if nanos_digits >= 9 { 9 } else { 3 };
        let render = |ms: i64| -> String { render_iso_date(ms, nanos_digits, None) };

        // Parallel pre-warm of the per-segment date-ordinal cache (chrono
        // parsing of every distinct term is the dominant first-call cost).
        {
            use rayon::prelude::*;
            self.segs.par_iter().for_each(|seg| {
                if matches!(seg.cols.get(field), Some(Column::Keyword(_))) {
                    let _ = self.date_sorted_prefix(seg, field);
                }
            });
        }
        // Per-segment sorted (ms, prefix-count) arrays (cached on the Index):
        // each range is answered with two binary searches instead of a walk
        // over every distinct term ordinal (~O(distinct timestamps) per
        // range pre-fix, which dominated on ms-resolution @timestamp data).
        let mut sorted_per_seg: Vec<Option<std::sync::Arc<(Vec<i64>, Vec<u64>)>>> = Vec::new();
        for s in &self.segs {
            match s.cols.get(field) {
                Some(Column::Keyword(_)) => {
                    sorted_per_seg.push(Some(self.date_sorted_prefix(s, field)?));
                }
                _ => sorted_per_seg.push(None),
            }
        }

        let keyed = params
            .get("keyed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut keyed_map = Map::new();
        let mut buckets: Vec<Value> = Vec::new();
        for range_def in ranges {
            let o = range_def.as_object()?;
            if !o.keys().all(|k| k == "from" || k == "to" || k == "key") {
                return None;
            }
            let from_ms = range_def.get("from").and_then(parse_date_ms);
            let to_ms = range_def.get("to").and_then(parse_date_ms);
            // Unparseable explicit bound → brute would fall to format-aware
            // parsing; bail to stay exact.
            if range_def.get("from").is_some() && from_ms.is_none() {
                return None;
            }
            if range_def.get("to").is_some() && to_ms.is_none() {
                return None;
            }
            let from_str_in = range_def.get("from").and_then(Value::as_str);
            let to_str_in = range_def.get("to").and_then(Value::as_str);

            let mut count: u64 = 0;
            for (si, s) in self.segs.iter().enumerate() {
                let Some(Column::Keyword(_)) = s.cols.get(field) else {
                    continue;
                };
                let sp = sorted_per_seg[si].as_ref()?;
                let (ms_v, prefix) = (&sp.0, &sp.1);
                // Semantics identical to the old per-ord walk: from is
                // inclusive (ms >= from), to is exclusive (ms < to);
                // unparseable terms are excluded at build time.
                let lo = match from_ms {
                    Some(f) => ms_v.partition_point(|&m| m < f),
                    None => 0,
                };
                let hi = match to_ms {
                    Some(t) => ms_v.partition_point(|&m| m < t),
                    None => ms_v.len(),
                };
                if hi > lo {
                    count += prefix[hi] - prefix[lo];
                }
            }
            for doc in self.mem().iter() {
                let v = doc.get(field);
                let Some(ms) = v.and_then(parse_date_ms) else {
                    continue;
                };
                let pass_from = from_ms.map(|f| ms >= f).unwrap_or(true);
                let pass_to = to_ms.map(|t| ms < t).unwrap_or(true);
                if pass_from && pass_to {
                    count += doc_count_weight(doc);
                }
            }

            let from_key = match (from_ms, from_str_in) {
                (Some(ms), _) => render(ms),
                (None, Some(s)) => s.to_string(),
                _ => "*".to_string(),
            };
            let to_key = match (to_ms, to_str_in) {
                (Some(ms), _) => render(ms),
                (None, Some(s)) => s.to_string(),
                _ => "*".to_string(),
            };
            let default_key = format!("{}-{}", from_key, to_key);
            let key = range_def
                .get("key")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or(default_key);

            let mut bucket = Map::new();
            if !keyed {
                bucket.insert("key".to_string(), json!(key));
            }
            if let Some(ms) = from_ms {
                bucket.insert("from".to_string(), json!(ms));
                bucket.insert("from_as_string".to_string(), json!(render(ms)));
            }
            if let Some(ms) = to_ms {
                bucket.insert("to".to_string(), json!(ms));
                bucket.insert("to_as_string".to_string(), json!(render(ms)));
            }
            bucket.insert("doc_count".to_string(), json!(count));
            if keyed {
                keyed_map.insert(key, Value::Object(bucket));
            } else {
                buckets.push(Value::Object(bucket));
            }
        }
        if keyed {
            Some(json!({ "buckets": Value::Object(keyed_map) }))
        } else {
            Some(json!({ "buckets": buckets }))
        }
    }

    /// Cached ord → epoch-ms mapping for a keyword date column.
    /// `i64::MIN` marks unparseable terms.
    fn date_ord_index(&self, seg: &SegEntry, field: &str) -> Option<std::sync::Arc<Vec<i64>>> {
        let key = (seg.id.clone(), field.to_string());
        if let Some(v) = self.idx.fast_date_cache.get(&key) {
            return Some(std::sync::Arc::clone(v.value()));
        }
        let Some(Column::Keyword(k)) = seg.cols.get(field) else {
            return None;
        };
        let mut ord_ms: Vec<i64> = Vec::with_capacity(k.terms.len());
        for t in &k.terms {
            ord_ms.push(parse_date_ms(&Value::String(t.clone())).unwrap_or(i64::MIN));
        }
        let arc = std::sync::Arc::new(ord_ms);
        self.idx
            .fast_date_cache
            .insert(key, std::sync::Arc::clone(&arc));
        Some(arc)
    }

    /// Cached sorted parsed dates + prefix doc counts for a keyword date
    /// column: `ms[i]` ascending, `prefix[i+1] - prefix[lo]` = number of
    /// docs whose parsed date falls in `ms[lo..=i]`.  Unparseable terms
    /// (i64::MIN) and zero-count ordinals are excluded — matching the
    /// brute/date_range walk exactly.
    fn date_sorted_prefix(
        &self,
        seg: &SegEntry,
        field: &str,
    ) -> Option<std::sync::Arc<(Vec<i64>, Vec<u64>)>> {
        let key = (seg.id.clone(), field.to_string());
        if let Some(v) = self.idx.fast_date_sorted_cache.get(&key) {
            return Some(std::sync::Arc::clone(v.value()));
        }
        let Some(Column::Keyword(k)) = seg.cols.get(field) else {
            return None;
        };
        let ord_ms = self.date_ord_index(seg, field)?;
        let mut pairs: Vec<(i64, u64)> = Vec::with_capacity(ord_ms.len());
        for (ord, &ms) in ord_ms.iter().enumerate() {
            let cnt = k.per_ord_count.get(ord).copied().unwrap_or(0) as u64;
            if ms == i64::MIN || cnt == 0 {
                continue;
            }
            pairs.push((ms, cnt));
        }
        pairs.sort_unstable_by_key(|p| p.0);
        let mut ms_v: Vec<i64> = Vec::with_capacity(pairs.len());
        let mut prefix: Vec<u64> = Vec::with_capacity(pairs.len() + 1);
        prefix.push(0);
        let mut acc = 0u64;
        for (ms, c) in pairs {
            ms_v.push(ms);
            acc += c;
            prefix.push(acc);
        }
        let arc = std::sync::Arc::new((ms_v, prefix));
        self.idx
            .fast_date_sorted_cache
            .insert(key, std::sync::Arc::clone(&arc));
        Some(arc)
    }

    // ── filter / filters / adjacency_matrix ─────────────────────────────

    fn exec_filter(&self, filter_query: &Value, sub: Option<&Value>) -> Option<Value> {
        let plan = self.plan_subs(sub, false)?;
        let pred = compile_pred(filter_query)?;
        self.check_pred_kinds(&pred)?;

        let mut count: u64 = 0;
        let mut accs: Vec<MetricAcc> = vec![MetricAcc::default(); plan.metrics.len()];
        for si in 0..self.segs.len() {
            let seg = &self.segs[si];
            let sp = resolve_pred(&seg.cols, &pred)?;
            if plan.metrics.is_empty() {
                count += seg_pred_count(&sp, seg.docs);
            } else {
                let mut wrapped: Vec<Vec<MetricAcc>> =
                    accs.iter().map(|_| vec![MetricAcc::default()]).collect();
                let mut tops: Vec<Vec<(f64, u64, DocRef)>> = vec![Vec::new()];
                let mut slot = |row: u32| -> Option<usize> {
                    if seg_pred_matches(&sp, row) {
                        count += 1;
                        Some(0)
                    } else {
                        None
                    }
                };
                let mut ms = false;
                self.fused_seg_pass(si, &mut slot, 1, &plan, &mut wrapped, &mut tops, &mut ms)?;
                for (mi, w) in wrapped.iter().enumerate() {
                    merge_acc(&mut accs[mi], &w[0]);
                }
            }
        }
        for doc in self.mem().iter() {
            if doc_matches_filter(doc, filter_query) {
                count += doc_count_weight(doc);
                for (mi, spec) in plan.metrics.iter().enumerate() {
                    Self::fold_mem_metric(doc, spec, &mut accs[mi]);
                }
            }
        }
        let mut bucket = Map::new();
        bucket.insert("doc_count".to_string(), json!(count));
        self.finish_bucket(&mut bucket, &plan, &accs, None);
        Some(Value::Object(bucket))
    }

    fn exec_filters(&self, params: &Value, sub: Option<&Value>) -> Option<Value> {
        if !params_only(params, &["filters", "keyed"]) {
            return None;
        }
        let plan = self.plan_subs(sub, false)?;
        let filters_val = params.get("filters")?;
        let keyed_override = params.get("keyed").and_then(Value::as_bool);

        // Normalise to (Option<name>, query) preserving order.
        let entries: Vec<(Option<String>, &Value)> = match filters_val {
            Value::Object(map) => map.iter().map(|(k, v)| (Some(k.clone()), v)).collect(),
            Value::Array(arr) => arr.iter().map(|v| (None, v)).collect(),
            _ => return None,
        };
        let named = matches!(filters_val, Value::Object(_));
        let keyed = if named {
            keyed_override.unwrap_or(true)
        } else {
            false
        };

        let mut rendered: Vec<(Option<String>, Value)> = Vec::with_capacity(entries.len());
        for (name, q) in &entries {
            let one = self.filter_bucket_body(q, &plan)?;
            rendered.push((name.clone(), one));
        }

        if named && keyed {
            let mut buckets = Map::new();
            for (name, b) in rendered {
                buckets.insert(name.unwrap_or_default(), b);
            }
            Some(json!({ "buckets": buckets }))
        } else if named {
            let mut buckets: Vec<Value> = Vec::new();
            for (name, mut b) in rendered {
                if let Some(o) = b.as_object_mut() {
                    o.insert("key".to_string(), Value::String(name.unwrap_or_default()));
                }
                buckets.push(b);
            }
            Some(json!({ "buckets": buckets }))
        } else {
            let buckets: Vec<Value> = rendered.into_iter().map(|(_, b)| b).collect();
            Some(json!({ "buckets": buckets }))
        }
    }

    /// A single filters-agg bucket body: `{doc_count, <subs>}`.
    fn filter_bucket_body(&self, filter_query: &Value, plan: &SubPlan) -> Option<Value> {
        let pred = compile_pred(filter_query)?;
        self.check_pred_kinds(&pred)?;
        let mut count: u64 = 0;
        let mut accs: Vec<MetricAcc> = vec![MetricAcc::default(); plan.metrics.len()];
        for si in 0..self.segs.len() {
            let seg = &self.segs[si];
            let sp = resolve_pred(&seg.cols, &pred)?;
            if plan.metrics.is_empty() {
                count += seg_pred_count(&sp, seg.docs);
            } else {
                let mut wrapped: Vec<Vec<MetricAcc>> =
                    accs.iter().map(|_| vec![MetricAcc::default()]).collect();
                let mut tops: Vec<Vec<(f64, u64, DocRef)>> = vec![Vec::new()];
                let mut slot = |row: u32| -> Option<usize> {
                    if seg_pred_matches(&sp, row) {
                        count += 1;
                        Some(0)
                    } else {
                        None
                    }
                };
                let mut ms = false;
                self.fused_seg_pass(si, &mut slot, 1, plan, &mut wrapped, &mut tops, &mut ms)?;
                for (mi, w) in wrapped.iter().enumerate() {
                    merge_acc(&mut accs[mi], &w[0]);
                }
            }
        }
        for doc in self.mem().iter() {
            if doc_matches_filter(doc, filter_query) {
                count += doc_count_weight(doc);
                for (mi, spec) in plan.metrics.iter().enumerate() {
                    Self::fold_mem_metric(doc, spec, &mut accs[mi]);
                }
            }
        }
        let mut bucket = Map::new();
        bucket.insert("doc_count".to_string(), json!(count));
        self.finish_bucket(&mut bucket, plan, &accs, None);
        Some(Value::Object(bucket))
    }

    fn exec_adjacency_matrix(&self, params: &Value, sub: Option<&Value>) -> Option<Value> {
        if sub.is_some() {
            return None;
        }
        if !params_only(params, &["filters"]) {
            return None;
        }
        let filters_map = params.get("filters").and_then(Value::as_object)?;
        let names: Vec<&String> = filters_map.keys().collect();
        let preds: Vec<Pred> = filters_map
            .values()
            .map(compile_pred)
            .collect::<Option<Vec<_>>>()?;
        for p in &preds {
            self.check_pred_kinds(p)?;
        }
        let n = preds.len();
        if n == 0 {
            return Some(json!({ "buckets": [] }));
        }
        if n > 32 {
            return None;
        }
        let mut singles: Vec<u64> = vec![0; n];
        let mut pairs: Vec<u64> = vec![0; n * n];
        for seg in &self.segs {
            let sps: Vec<SegPred<'_>> = preds
                .iter()
                .map(|p| resolve_pred(&seg.cols, p))
                .collect::<Option<Vec<_>>>()?;
            for row in 0..seg.docs {
                let mut mask: u32 = 0;
                for (i, sp) in sps.iter().enumerate() {
                    if seg_pred_matches(sp, row) {
                        mask |= 1 << i;
                    }
                }
                if mask == 0 {
                    continue;
                }
                let mut m = mask;
                while m != 0 {
                    let i = m.trailing_zeros() as usize;
                    m &= m - 1;
                    singles[i] += 1;
                    let mut m2 = mask & !((1u32 << (i + 1)) - 1);
                    while m2 != 0 {
                        let j = m2.trailing_zeros() as usize;
                        m2 &= m2 - 1;
                        pairs[i * n + j] += 1;
                    }
                }
            }
        }
        for doc in self.mem().iter() {
            let matches: Vec<bool> = filters_map
                .values()
                .map(|q| doc_matches_filter(doc, q))
                .collect();
            for i in 0..n {
                if matches[i] {
                    singles[i] += 1;
                    for j in (i + 1)..n {
                        if matches[j] {
                            pairs[i * n + j] += 1;
                        }
                    }
                }
            }
        }
        let mut buckets: Vec<Value> = Vec::new();
        for i in 0..n {
            if singles[i] > 0 {
                buckets.push(json!({ "key": names[i], "doc_count": singles[i] }));
            }
        }
        for i in 0..n {
            for j in (i + 1)..n {
                if pairs[i * n + j] > 0 {
                    buckets.push(json!({
                        "key": format!("{}&{}", names[i], names[j]),
                        "doc_count": pairs[i * n + j]
                    }));
                }
            }
        }
        buckets.sort_by(|a, b| {
            let ka = a.get("key").and_then(Value::as_str).unwrap_or("");
            let kb = b.get("key").and_then(Value::as_str).unwrap_or("");
            ka.cmp(kb)
        });
        Some(json!({ "buckets": buckets }))
    }

    fn check_pred_kinds(&self, pred: &Pred) -> Option<()> {
        match pred {
            Pred::MatchAll => Some(()),
            Pred::TermKw { field, .. } | Pred::TermsKw { field, .. } => {
                match self.seg_field_kind(field) {
                    Ok(Some(ColKind::Keyword)) | Ok(None) => Some(()),
                    _ => None,
                }
            }
            Pred::RangeNum { field, .. } => match self.seg_field_kind(field) {
                Ok(Some(ColKind::Numeric)) | Ok(None) => Some(()),
                _ => None,
            },
            Pred::And(subs) => {
                for s in subs {
                    self.check_pred_kinds(s)?;
                }
                Some(())
            }
        }
    }

    // ── composite ────────────────────────────────────────────────────────

    fn exec_composite(&self, params: &Value, sub: Option<&Value>) -> Option<Value> {
        if sub.is_some() {
            return None;
        }
        if !params_only(params, &["sources", "size"]) {
            return None;
        }
        let sources = params.get("sources").and_then(Value::as_array)?;
        let size = params.get("size").and_then(Value::as_u64).unwrap_or(10) as usize;

        // All sources must be plain `terms` over keyword columns.
        let mut src_names: Vec<String> = Vec::new();
        let mut src_fields: Vec<String> = Vec::new();
        for src in sources {
            let src_obj = src.as_object()?;
            if src_obj.len() != 1 {
                return None;
            }
            let (name, spec) = src_obj.iter().next()?;
            let spec_obj = spec.as_object()?;
            if spec_obj.len() != 1 {
                return None;
            }
            let (t, tp) = spec_obj.iter().next()?;
            if t != "terms" || !params_only(tp, &["field"]) {
                return None;
            }
            let field = tp.get("field").and_then(Value::as_str)?;
            match self.seg_field_kind(field) {
                Ok(Some(ColKind::Keyword)) | Ok(None) => {}
                _ => return None,
            }
            src_names.push(name.clone());
            src_fields.push(field.to_string());
        }
        if src_names.is_empty() {
            return Some(json!({ "buckets": [] }));
        }

        let mut counts: HashMap<Vec<String>, u64> = HashMap::new();
        for seg in &self.segs {
            let cols: Vec<&KeywordColumn> = match src_fields
                .iter()
                .map(|f| match seg.cols.get(f) {
                    Some(Column::Keyword(k)) => Some(k),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
            {
                Some(c) => c,
                None => continue, // some source field absent → no docs from this segment
            };
            let mut local: HashMap<Vec<u32>, u64> = HashMap::new();
            'rows: for row in 0..seg.docs {
                let mut key: Vec<u32> = Vec::with_capacity(cols.len());
                for k in &cols {
                    match k.ord_for(row) {
                        Some(o) => key.push(o),
                        None => continue 'rows,
                    }
                }
                *local.entry(key).or_insert(0) += 1;
            }
            for (ords, c) in local {
                let key: Vec<String> = ords
                    .iter()
                    .zip(cols.iter())
                    .map(|(o, k)| k.terms[*o as usize].clone())
                    .collect();
                *counts.entry(key).or_insert(0) += c;
            }
        }
        for doc in self.mem().iter() {
            let weight = doc_count_weight(doc);
            // Cross-product across multi-valued fields — brute semantics.
            let mut keys: Vec<Vec<String>> = vec![vec![]];
            let mut dead = false;
            for f in &src_fields {
                let vals = extract_field_values(doc, f);
                if vals.is_empty() {
                    dead = true;
                    break;
                }
                let mut next: Vec<Vec<String>> = Vec::new();
                for k in &keys {
                    for v in &vals {
                        let mut nk = k.clone();
                        nk.push(v.clone());
                        next.push(nk);
                    }
                }
                keys = next;
            }
            if dead {
                continue;
            }
            for k in keys {
                *counts.entry(k).or_insert(0) += weight;
            }
        }

        let mut sorted_keys: Vec<Vec<String>> = counts.keys().cloned().collect();
        sorted_keys.sort_by(|a, b| composite_cmp(a, b));
        sorted_keys.truncate(size);

        let result_buckets: Vec<Value> = sorted_keys
            .iter()
            .map(|key| {
                let mut key_obj = Map::new();
                for (i, name) in src_names.iter().enumerate() {
                    let val = key.get(i).cloned().unwrap_or_default();
                    if let Ok(n) = val.parse::<i64>() {
                        key_obj.insert(name.clone(), json!(n));
                    } else if let Ok(f) = val.parse::<f64>() {
                        key_obj.insert(
                            name.clone(),
                            serde_json::Number::from_f64(f)
                                .map(Value::Number)
                                .unwrap_or(Value::String(val.clone())),
                        );
                    } else {
                        key_obj.insert(name.clone(), Value::String(val));
                    }
                }
                json!({ "key": Value::Object(key_obj), "doc_count": counts[key] })
            })
            .collect();

        let after_key = result_buckets
            .last()
            .and_then(|b| b.get("key").cloned())
            .unwrap_or(Value::Null);
        let mut out = json!({ "buckets": result_buckets });
        if !after_key.is_null() {
            if let Some(obj) = out.as_object_mut() {
                obj.insert("after_key".into(), after_key);
            }
        }
        Some(out)
    }

    // ── sampler / random_sampler ─────────────────────────────────────────

    fn exec_sampler(&self, params: &Value, sub: Option<&Value>) -> Option<Value> {
        if !params_only(
            params,
            &["shard_size", "probability", "seed", "max_docs_per_value"],
        ) {
            return None;
        }
        if let Some(s) = sub {
            if sub_tree_needs_background(s) {
                return None;
            }
        }
        let shard_size = params
            .get("shard_size")
            .and_then(Value::as_u64)
            .unwrap_or(200) as usize;
        if shard_size > 10_000 {
            return None; // keep the materialisation bounded
        }
        // Sample = first `shard_size` docs in brute corpus order
        // (memtable docs, then segments in snapshot order).  With
        // match_all every doc scores equal, so the brute sampler's
        // stable score sort preserves exactly this prefix.
        let mut sample: Vec<Value> = Vec::with_capacity(shard_size);
        for d in self.mem().iter() {
            if sample.len() >= shard_size {
                break;
            }
            sample.push(d.clone());
        }
        let mut si = 0usize;
        while sample.len() < shard_size && si < self.segs.len() {
            let take = (shard_size - sample.len()).min(self.segs[si].docs as usize);
            for row in 0..take {
                sample.push(self.fetch_seg_doc(si, row as u32)?);
            }
            si += 1;
        }
        // Brute enriches the first `materialisation_limit` (256) corpus docs
        // with `_score: 1.0` (the match_all hit window).  Mirror it so
        // score-sensitive sub-aggs (top_hits default sort) behave the same.
        for d in sample.iter_mut().take(256) {
            if let Some(o) = d.as_object_mut() {
                o.insert("_score".to_string(), json!(1.0));
            }
        }
        Some(run_sampler(params, sub, &sample, &sample))
    }

    // ── variable_width_histogram ─────────────────────────────────────────

    fn exec_vwh(&self, params: &Value, sub: Option<&Value>) -> Option<Value> {
        if sub.is_some() {
            return None;
        }
        if !params_only(params, &["field", "buckets"]) {
            return None;
        }
        let field = params.get("field").and_then(Value::as_str)?;
        let num_buckets = params.get("buckets").and_then(Value::as_u64).unwrap_or(10) as usize;
        match self.seg_field_kind(field) {
            Ok(Some(ColKind::Numeric)) | Ok(None) => {}
            _ => return None,
        }
        if !self.mem_field_numeric_safe(field) {
            return None;
        }

        // Distinct (value → weight) across all sources, ascending by value.
        // Each segment's `n.sorted` is already value-ascending, so collapse
        // each into a distinct-run list and k-way merge — no HashMap (a 1M-row
        // corpus paid ~300 ms of SipHash there), no global re-sort.
        let mut runs: Vec<Vec<(f64, u64)>> = Vec::with_capacity(self.segs.len() + 1);
        for seg in &self.segs {
            let Some(Column::Numeric(n)) = seg.cols.get(field) else {
                continue;
            };
            let mut run: Vec<(f64, u64)> = Vec::new();
            let mut i = 0usize;
            while i < n.sorted.len() {
                let bits = n.sorted[i].0;
                let mut j = i + 1;
                while j < n.sorted.len() && n.sorted[j].0 == bits {
                    j += 1;
                }
                run.push((f64::from_bits(bits as u64), (j - i) as u64));
                i = j;
            }
            if !run.is_empty() {
                runs.push(run);
            }
        }
        if !self.mem().is_empty() {
            let mut mem_vals: Vec<f64> = self
                .mem()
                .iter()
                .filter_map(|doc| extract_numeric(doc, field))
                .collect();
            mem_vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let mut run: Vec<(f64, u64)> = Vec::new();
            for v in mem_vals {
                match run.last_mut() {
                    Some((lv, w)) if *lv == v => *w += 1,
                    _ => run.push((v, 1)),
                }
            }
            if !run.is_empty() {
                runs.push(run);
            }
        }
        let vals = merge_weighted_runs(runs);
        if vals.is_empty() {
            return Some(json!({ "buckets": [] }));
        }

        // Bound the exact agglomerative clustering working set.  The
        // heap-merge in `vwh_cluster` does (distinct − buckets) merges — on a
        // ~500 k-distinct float column that's hundreds of ms.  ES's own VWH
        // is approximate (it never keeps more than shard_size ≈ buckets×50
        // clusters while collecting), so pre-coarsening the sorted distinct
        // list into equal-weight groups before the exact pass matches ES's
        // accuracy class while keeping the whole agg in the ~10 ms range.
        // Below the cap the behaviour is byte-identical to before.
        const VWH_COARSE_CAP: usize = 5_000;
        let vals = if vals.len() > VWH_COARSE_CAP.max(num_buckets * 50) {
            coarsen_weighted(&vals, VWH_COARSE_CAP.max(num_buckets * 50))
        } else {
            vals
        };

        let buckets = vwh_cluster(&vals, num_buckets);
        let result_buckets: Vec<Value> = buckets
            .iter()
            .map(|b| {
                json!({
                    "min": b.min, "key": b.sum / b.count as f64, "max": b.max,
                    "doc_count": b.count
                })
            })
            .collect();
        Some(json!({ "buckets": result_buckets }))
    }

    // ── date_histogram ───────────────────────────────────────────────────

    fn exec_date_histogram(&self, params: &Value, sub: Option<&Value>) -> Option<Value> {
        if !params_only(
            params,
            &[
                "field",
                "calendar_interval",
                "fixed_interval",
                "interval",
                "format",
                "keyed",
                "min_doc_count",
                "offset",
            ],
        ) {
            return None;
        }
        let field = params.get("field").and_then(Value::as_str)?;
        match self.seg_field_kind(field) {
            // Keyword = date-as-ISO-string column (parsed via `date_ord_index`).
            // Numeric = epoch-millis column (`date`/`long` doc-values) — the
            // common case, bucketed directly below with no chrono parse. Before
            // this, a Numeric date field fell through to the O(N) brute path
            // (`run_aggs_with_all`), ~1 s / 200k docs.
            Ok(Some(ColKind::Keyword)) | Ok(Some(ColKind::Numeric)) | Ok(None) => {}
            _ => return None,
        }
        let interval_str = params
            .get("fixed_interval")
            .or_else(|| params.get("calendar_interval"))
            .or_else(|| params.get("interval"))
            .and_then(Value::as_str)
            .unwrap_or("1d");
        let interval_ms = interval_to_ms(interval_str)?;
        let use_calendar = is_calendar_interval(interval_str);
        // Fixed-interval grid `offset` (e.g. "+3600000ms" from the
        // auto_date_histogram anchoring, or a user "+30m"): shift the grid by
        // `offset_ms`, exactly the brute `to_local`/`to_utc` pair.  Calendar
        // intervals with an offset keep going through the brute path.
        let offset_ms = params
            .get("offset")
            .and_then(Value::as_str)
            .and_then(parse_offset_ms)
            .unwrap_or(0);
        if offset_ms != 0 && use_calendar {
            return None;
        }
        let bucket_of = |ms: i64| -> i64 {
            if use_calendar {
                calendar_bucket_key(ms, interval_str)
            } else {
                (ms - offset_ms).div_euclid(interval_ms) * interval_ms + offset_ms
            }
        };
        let next_bucket = |key: i64| -> i64 {
            let next = if use_calendar {
                next_calendar_bucket(key, interval_str)
            } else {
                key + interval_ms
            };
            if next <= key {
                key + interval_ms
            } else {
                next
            }
        };

        // Parallel pre-warm of the per-segment date-ordinal cache (chrono
        // parsing of every distinct term is the dominant first-call cost).
        {
            use rayon::prelude::*;
            self.segs.par_iter().for_each(|seg| {
                if matches!(seg.cols.get(field), Some(Column::Keyword(_))) {
                    let _ = self.date_ord_index(seg, field);
                }
            });
        }
        let plan = self.plan_subs(sub, false)?;
        let has_row_work = !plan.metrics.is_empty();

        // Global bucket table.
        let mut bucket_ids: HashMap<i64, usize> = HashMap::new();
        let mut bucket_keys: Vec<i64> = Vec::new();
        let mut counts: Vec<u64> = Vec::new();
        let mut accs: Vec<Vec<MetricAcc>> = plan.metrics.iter().map(|_| Vec::new()).collect();
        let ensure_bucket = |key: i64,
                             bucket_ids: &mut HashMap<i64, usize>,
                             bucket_keys: &mut Vec<i64>,
                             counts: &mut Vec<u64>,
                             accs: &mut Vec<Vec<MetricAcc>>|
         -> usize {
            *bucket_ids.entry(key).or_insert_with(|| {
                bucket_keys.push(key);
                counts.push(0);
                for a in accs.iter_mut() {
                    a.push(MetricAcc::default());
                }
                bucket_keys.len() - 1
            })
        };

        let mut missing_sort_seen = false;
        for si in 0..self.segs.len() {
            let seg = &self.segs[si];
            let Some(Column::Keyword(k)) = seg.cols.get(field) else {
                continue;
            };
            let ord_ms = self.date_ord_index(seg, field)?;
            // ord → bucket slot table.  Term ordinals are lexicographic and
            // ISO-8601 timestamps sort chronologically, so `ord_ms` is
            // (near-)ascending: memoise the current bucket's [start, end)
            // window and only pay `bucket_of` (chrono calendar math) + the
            // HashMap probe when the run breaks.  This turns ~1 per distinct
            // timestamp (≈1 per doc at ms resolution) into ~1 per bucket.
            // `bucket_of(ms) == key` for every ms in `[key, next_bucket(key))`
            // is the same invariant the min_doc_count:0 gap-fill below
            // already relies on.
            let mut ord_slot: Vec<i32> = Vec::with_capacity(k.terms.len());
            let mut run: Option<(i64, i64, i32)> = None; // (start, end, slot)
            for (ord, &ms) in ord_ms.iter().enumerate() {
                if ms == i64::MIN || k.per_ord_count.get(ord).copied().unwrap_or(0) == 0 {
                    ord_slot.push(-1);
                    continue;
                }
                let slot = match run {
                    Some((start, end, slot)) if ms >= start && ms < end => slot,
                    _ => {
                        let key = bucket_of(ms);
                        let slot = ensure_bucket(
                            key,
                            &mut bucket_ids,
                            &mut bucket_keys,
                            &mut counts,
                            &mut accs,
                        ) as i32;
                        run = Some((key, next_bucket(key), slot));
                        slot
                    }
                };
                ord_slot.push(slot);
            }
            // Bucket doc counts always come from the per-ord histogram —
            // exactly the count of rows with that (non-null) ordinal — so
            // the row pass below no longer pays a per-row counter bump.
            for (ord, &cnt) in k.per_ord_count.iter().enumerate() {
                if cnt > 0 && ord_slot[ord] >= 0 {
                    counts[ord_slot[ord] as usize] += cnt as u64;
                }
            }
            if !has_row_work {
                continue;
            }
            let n_slots = bucket_keys.len();
            let mut tops: Vec<Vec<(f64, u64, DocRef)>> = vec![Vec::new(); n_slots.max(1)];
            {
                let kw_dense = k.null_bitmap.is_empty();
                let mut slot_of_row = |row: u32| -> Option<usize> {
                    let ord = if kw_dense {
                        *k.ords.get(row as usize)? as usize
                    } else {
                        k.ord_for(row)? as usize
                    };
                    let s = ord_slot[ord];
                    if s < 0 {
                        return None;
                    }
                    Some(s as usize)
                };
                self.fused_seg_pass(
                    si,
                    &mut slot_of_row,
                    n_slots,
                    &plan,
                    &mut accs,
                    &mut tops,
                    &mut missing_sort_seen,
                )?;
            }
        }
        // Numeric (epoch-millis) date column: bucket the doc-values integers
        // directly — no chrono parse, no per-ord run memo (values are ~unique
        // per doc). Segments are keyword XOR numeric for a field, so exactly
        // one of these two loops does work. Counts are bumped here; sub-metrics
        // fold via `fused_seg_pass`, mirroring the keyword arm above.
        for si in 0..self.segs.len() {
            let seg = &self.segs[si];
            let Some(Column::Numeric(n)) = seg.cols.get(field) else {
                continue;
            };
            let mut row_slot: Vec<i32> = Vec::with_capacity(seg.docs as usize);
            for row in 0..seg.docs {
                match n.get(row) {
                    // `NumericColumn::data` stores f64 BITS (see its doc comment
                    // + `exec_vwh`), not the raw integer — decode to the epoch-ms
                    // value before bucketing.
                    Some(bits) => {
                        let ms = f64::from_bits(bits as u64) as i64;
                        let key = bucket_of(ms);
                        let slot = ensure_bucket(
                            key,
                            &mut bucket_ids,
                            &mut bucket_keys,
                            &mut counts,
                            &mut accs,
                        ) as i32;
                        counts[slot as usize] += 1;
                        row_slot.push(slot);
                    }
                    None => row_slot.push(-1),
                }
            }
            if !has_row_work {
                continue;
            }
            let n_slots = bucket_keys.len();
            let mut tops: Vec<Vec<(f64, u64, DocRef)>> = vec![Vec::new(); n_slots.max(1)];
            {
                let mut slot_of_row = |row: u32| -> Option<usize> {
                    let s = *row_slot.get(row as usize)?;
                    if s < 0 {
                        None
                    } else {
                        Some(s as usize)
                    }
                };
                self.fused_seg_pass(
                    si,
                    &mut slot_of_row,
                    n_slots,
                    &plan,
                    &mut accs,
                    &mut tops,
                    &mut missing_sort_seen,
                )?;
            }
        }

        // Memtable docs — brute multi-value date extraction with per-doc
        // bucket dedup.
        for doc in self.mem().iter() {
            let raws = extract_date_ms_values(doc, field);
            let weight = doc_count_weight(doc);
            let mut seen: Vec<i64> = Vec::with_capacity(raws.len());
            for ms in raws {
                let key = bucket_of(ms);
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
                let slot = ensure_bucket(
                    key,
                    &mut bucket_ids,
                    &mut bucket_keys,
                    &mut counts,
                    &mut accs,
                );
                counts[slot] += weight;
                for (mi, spec) in plan.metrics.iter().enumerate() {
                    Self::fold_mem_metric(doc, spec, &mut accs[mi][slot]);
                }
            }
        }

        let min_doc_count = params
            .get("min_doc_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let keyed = params
            .get("keyed")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        // Bucket-key set: gap-fill when min_doc_count == 0 (brute default).
        const MAX_BUCKETS: i64 = 65_536;
        let mut final_keys: Vec<i64> = if min_doc_count > 0 {
            bucket_keys.clone()
        } else if let (Some(&min_key), Some(&max_key)) =
            (bucket_keys.iter().min(), bucket_keys.iter().max())
        {
            let mut span: i64 = 0;
            let mut probe = min_key;
            while probe <= max_key {
                span += 1;
                if span > MAX_BUCKETS {
                    return Some(json!({
                        "error": format!(
                            "Trying to create too many buckets. Must be less than or equal to: [{}] but this number of buckets was exceeded. This limit can be set by changing the [search.max_buckets] cluster level setting.",
                            MAX_BUCKETS
                        )
                    }));
                }
                probe = next_bucket(probe);
            }
            let mut keys = Vec::with_capacity(span as usize);
            let mut k = min_key;
            while k <= max_key {
                keys.push(k);
                k = next_bucket(k);
            }
            keys
        } else {
            bucket_keys.clone()
        };
        final_keys.sort_unstable();
        if min_doc_count > 0 {
            final_keys.retain(|k| {
                bucket_ids
                    .get(k)
                    .map(|&s| counts[s] >= min_doc_count)
                    .unwrap_or(false)
            });
        }

        let fmt_param = params.get("format").and_then(Value::as_str);
        let render_key = |key: i64| -> String {
            let dt_val = chrono::DateTime::from_timestamp_millis(key).unwrap_or_default();
            render_date_format(fmt_param, key, dt_val)
        };

        let empty_accs: Vec<MetricAcc> = vec![MetricAcc::default(); plan.metrics.len()];
        let mut result_buckets: Vec<Value> = Vec::with_capacity(final_keys.len());
        for &key in &final_keys {
            let slot = bucket_ids.get(&key).copied();
            let count = slot.map(|s| counts[s]).unwrap_or(0);
            let mut bucket = Map::new();
            bucket.insert("key".to_string(), json!(key));
            bucket.insert("key_as_string".to_string(), json!(render_key(key)));
            bucket.insert("doc_count".to_string(), json!(count));
            let bucket_accs: Vec<MetricAcc> = match slot {
                Some(s) => plan
                    .metrics
                    .iter()
                    .enumerate()
                    .map(|(mi, _)| accs[mi][s])
                    .collect(),
                None => empty_accs.clone(),
            };
            self.finish_bucket(&mut bucket, &plan, &bucket_accs, None);
            result_buckets.push(Value::Object(bucket));
        }
        let result_buckets = apply_bucket_pipeline_ops(result_buckets, sub);

        if keyed {
            let mut map = Map::new();
            for b in result_buckets {
                let k = b
                    .get("key_as_string")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| b.get("key").and_then(Value::as_i64).map(|i| i.to_string()))
                    .unwrap_or_default();
                map.insert(k, b);
            }
            Some(json!({ "buckets": Value::Object(map) }))
        } else {
            Some(json!({ "buckets": result_buckets }))
        }
    }

    // Fixed-interval `histogram` over a NUMERIC doc-values column. Mirrors the
    // date_histogram numeric arm for the columnar mechanics (per-segment
    // row→slot fold + `fused_seg_pass` for sub-metrics + brute memtable fold)
    // and `aggs::run_histogram` for the exact bucketing/rendering semantics.
    // Before this, `histogram` had no exec_agg arm → every request hit the O(N)
    // `run_aggs_with_all` brute path (~950 ms / 200k docs).
    fn exec_histogram(&self, params: &Value, sub: Option<&Value>) -> Option<Value> {
        if !params_only(
            params,
            &[
                "field",
                "interval",
                "offset",
                "min_doc_count",
                "keyed",
                "format",
                "extended_bounds",
                "hard_bounds",
            ],
        ) {
            return None;
        }
        // `missing`, multi-value range-typed inputs, and every non-listed
        // param are handled only by the brute path — bail (return None) so
        // those requests stay exact.
        let field = params.get("field").and_then(Value::as_str)?;
        // Require a real numeric doc-values column. A Keyword (string) column,
        // a range-typed field (stored differently), or a field that lives only
        // in the memtable (`Ok(None)`) all fall back to the exact brute path —
        // a Numeric segment column proves the mapping is plain-numeric, so the
        // memtable fold below (`extract_numeric_values`) is exact too.
        match self.seg_field_kind(field) {
            Ok(Some(ColKind::Numeric)) => {}
            _ => return None,
        }
        let interval = params
            .get("interval")
            .and_then(Value::as_f64)
            .filter(|i| *i > 0.0)?;
        let offset = params.get("offset").and_then(Value::as_f64).unwrap_or(0.0);
        let min_doc_count = params
            .get("min_doc_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let keyed = params
            .get("keyed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let extended_bounds = params.get("extended_bounds").and_then(Value::as_object);
        let hard_bounds = params.get("hard_bounds").and_then(Value::as_object);
        let extended_min = extended_bounds.and_then(|b| b.get("min").and_then(Value::as_f64));
        let extended_max = extended_bounds.and_then(|b| b.get("max").and_then(Value::as_f64));
        let hard_min = hard_bounds.and_then(|b| b.get("min").and_then(Value::as_f64));
        let hard_max = hard_bounds.and_then(|b| b.get("max").and_then(Value::as_f64));

        let key_of = |v: f64| -> i64 { ((v - offset) / interval).floor() as i64 };

        let plan = self.plan_subs(sub, false)?;
        let has_row_work = !plan.metrics.is_empty();

        let mut bucket_ids: HashMap<i64, usize> = HashMap::new();
        let mut bucket_keys: Vec<i64> = Vec::new();
        let mut counts: Vec<u64> = Vec::new();
        let mut accs: Vec<Vec<MetricAcc>> = plan.metrics.iter().map(|_| Vec::new()).collect();
        let ensure_bucket = |key: i64,
                             bucket_ids: &mut HashMap<i64, usize>,
                             bucket_keys: &mut Vec<i64>,
                             counts: &mut Vec<u64>,
                             accs: &mut Vec<Vec<MetricAcc>>|
         -> usize {
            *bucket_ids.entry(key).or_insert_with(|| {
                bucket_keys.push(key);
                counts.push(0);
                for a in accs.iter_mut() {
                    a.push(MetricAcc::default());
                }
                bucket_keys.len() - 1
            })
        };

        let mut missing_sort_seen = false;
        // Numeric column: bucket the doc-values integers/doubles directly.
        for si in 0..self.segs.len() {
            let seg = &self.segs[si];
            let Some(Column::Numeric(n)) = seg.cols.get(field) else {
                continue;
            };
            let mut row_slot: Vec<i32> = Vec::with_capacity(seg.docs as usize);
            for row in 0..seg.docs {
                match n.get(row) {
                    // `NumericColumn::data` stores f64 BITS (see its doc
                    // comment) — decode to the real value before bucketing.
                    // Unlike date_histogram (epoch-ms integers) histogram keeps
                    // the fractional value, so DO NOT truncate to i64 here.
                    Some(bits) => {
                        let v = f64::from_bits(bits as u64);
                        let key = key_of(v);
                        let slot = ensure_bucket(
                            key,
                            &mut bucket_ids,
                            &mut bucket_keys,
                            &mut counts,
                            &mut accs,
                        ) as i32;
                        counts[slot as usize] += 1;
                        row_slot.push(slot);
                    }
                    None => row_slot.push(-1),
                }
            }
            if !has_row_work {
                continue;
            }
            let n_slots = bucket_keys.len();
            let mut tops: Vec<Vec<(f64, u64, DocRef)>> = vec![Vec::new(); n_slots.max(1)];
            {
                let mut slot_of_row = |row: u32| -> Option<usize> {
                    let s = *row_slot.get(row as usize)?;
                    if s < 0 {
                        None
                    } else {
                        Some(s as usize)
                    }
                };
                self.fused_seg_pass(
                    si,
                    &mut slot_of_row,
                    n_slots,
                    &plan,
                    &mut accs,
                    &mut tops,
                    &mut missing_sort_seen,
                )?;
            }
        }

        // Memtable docs — brute multi-value numeric extraction with per-doc
        // bucket dedup, matching `run_histogram`. No `missing` fold (bailed).
        for doc in self.mem().iter() {
            let nums = extract_numeric_values(doc, field);
            if nums.is_empty() {
                continue;
            }
            let weight = doc_count_weight(doc);
            let mut seen: Vec<i64> = Vec::with_capacity(nums.len());
            for v in nums {
                let key = key_of(v);
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
                let slot = ensure_bucket(
                    key,
                    &mut bucket_ids,
                    &mut bucket_keys,
                    &mut counts,
                    &mut accs,
                );
                counts[slot] += weight;
                for (mi, spec) in plan.metrics.iter().enumerate() {
                    Self::fold_mem_metric(doc, spec, &mut accs[mi][slot]);
                }
            }
        }

        // ── Bucket-key set (mirrors run_histogram exactly) ────────────────
        const MAX_BUCKETS: i64 = 65_536;
        let mut final_keys: Vec<i64> = if bucket_ids.is_empty() && extended_min.is_none() {
            Vec::new()
        } else if min_doc_count > 0 && extended_min.is_none() {
            bucket_ids.keys().copied().collect()
        } else {
            let data_min = bucket_ids.keys().min().copied();
            let data_max = bucket_ids.keys().max().copied();
            let min_key = extended_min.map(key_of).or(data_min).unwrap_or(0);
            let max_key = extended_max.map(key_of).or(data_max).unwrap_or(0);
            let span = max_key.saturating_sub(min_key);
            if span > MAX_BUCKETS {
                return Some(json!({
                    "error": format!(
                        "Trying to create too many buckets. Must be less than or equal to: [{}] but this number of buckets was exceeded. This limit can be set by changing the [search.max_buckets] cluster level setting.",
                        MAX_BUCKETS
                    ),
                    "__error_status__": 400u32,
                }));
            }
            let mut keys = Vec::with_capacity((span as usize).min(MAX_BUCKETS as usize) + 1);
            let mut k = min_key;
            while k <= max_key {
                keys.push(k);
                k += 1;
            }
            keys
        };
        final_keys.sort_unstable();

        if let (Some(h_min), Some(h_max)) = (hard_min, hard_max) {
            let hard_min_key = key_of(h_min);
            let hard_max_key = key_of(h_max);
            final_keys.retain(|&k| k >= hard_min_key && k <= hard_max_key);
        }
        if min_doc_count > 0 {
            final_keys.retain(|k| {
                bucket_ids
                    .get(k)
                    .map(|&s| counts[s] >= min_doc_count)
                    .unwrap_or(false)
            });
        }

        let format_pattern = params.get("format").and_then(Value::as_str);
        let empty_accs: Vec<MetricAcc> = vec![MetricAcc::default(); plan.metrics.len()];
        let mut result_buckets: Vec<Value> = Vec::with_capacity(final_keys.len());
        for &key in &final_keys {
            let slot = bucket_ids.get(&key).copied();
            let count = slot.map(|s| counts[s]).unwrap_or(0);
            let actual_key = key as f64 * interval + offset;
            let key_json = if actual_key.fract() == 0.0 {
                json!(actual_key as i64)
            } else {
                json!(actual_key)
            };
            let mut bucket = Map::new();
            bucket.insert("key".to_string(), key_json);
            bucket.insert("doc_count".to_string(), json!(count));
            if let Some(p) = format_pattern {
                bucket.insert(
                    "key_as_string".to_string(),
                    Value::String(format_number_pattern(actual_key, p)),
                );
            }
            let bucket_accs: Vec<MetricAcc> = match slot {
                Some(s) => plan
                    .metrics
                    .iter()
                    .enumerate()
                    .map(|(mi, _)| accs[mi][s])
                    .collect(),
                None => empty_accs.clone(),
            };
            self.finish_bucket(&mut bucket, &plan, &bucket_accs, None);
            result_buckets.push(Value::Object(bucket));
        }

        // Sibling pipeline ops (derivative, cumulative_sum, …) operate on the
        // ordered bucket list before keying, same as date_histogram.
        let result_buckets = apply_bucket_pipeline_ops(result_buckets, sub);
        if keyed {
            // Map key: `key_as_string` when a format is set, else the plain
            // numeric key rendered by `format_histogram_key` — identical to the
            // brute path (aggs::run_histogram).
            let mut map = Map::new();
            for b in result_buckets {
                let label = b
                    .get("key_as_string")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        b.get("key")
                            .and_then(Value::as_f64)
                            .map(format_histogram_key)
                    })
                    .unwrap_or_default();
                map.insert(label, b);
            }
            Some(json!({ "buckets": Value::Object(map) }))
        } else {
            Some(json!({ "buckets": result_buckets }))
        }
    }
}

fn metric_type_name(kind: MetricKind) -> &'static str {
    match kind {
        MetricKind::Avg => "avg",
        MetricKind::Sum => "sum",
        MetricKind::Min => "min",
        MetricKind::Max => "max",
        MetricKind::Stats => "stats",
        MetricKind::ValueCount => "value_count",
    }
}

fn merge_acc(into: &mut MetricAcc, other: &MetricAcc) {
    if other.count == 0 {
        return;
    }
    if into.count == 0 {
        *into = *other;
        return;
    }
    into.count += other.count;
    into.sum += other.sum;
    if other.min < into.min {
        into.min = other.min;
    }
    if other.max > into.max {
        into.max = other.max;
    }
}

/// Keep the top-`k` (by sort value + corpus-rank tiebreak) entries.
fn push_top(buf: &mut Vec<(f64, u64, DocRef)>, k: usize, desc: bool, v: f64, rank: u64, r: DocRef) {
    buf.push((v, rank, r));
    if buf.len() > k.saturating_mul(2).max(16) {
        sort_top(buf, desc);
        buf.truncate(k);
    }
}

fn sort_top(buf: &mut [(f64, u64, DocRef)], desc: bool) {
    buf.sort_by(|a, b| {
        let c = if desc {
            b.0.partial_cmp(&a.0)
        } else {
            a.0.partial_cmp(&b.0)
        }
        .unwrap_or(std::cmp::Ordering::Equal);
        c.then_with(|| a.1.cmp(&b.1))
    });
}

/// Parse a terms `order` param into (path, asc) entries — mirror of the
/// closure in `run_terms`.
fn parse_orders(params: &Value) -> Vec<(String, bool)> {
    let order = match params.get("order") {
        Some(v) => v,
        None => return Vec::new(),
    };
    let mut out: Vec<(String, bool)> = Vec::new();
    match order {
        Value::Object(o) => {
            for (k, v) in o {
                let asc = v.as_str().map(|s| s == "asc").unwrap_or(false);
                out.push((k.clone(), asc));
            }
        }
        Value::Array(arr) => {
            for item in arr {
                if let Some(o) = item.as_object() {
                    for (k, v) in o {
                        let asc = v.as_str().map(|s| s == "asc").unwrap_or(false);
                        out.push((k.clone(), asc));
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// `cmp_terms_by_orders` for (key, count) with no sub-agg entries.
fn cmp_by_orders(
    a: &(String, u64),
    b: &(String, u64),
    orders: &[(String, bool)],
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if orders.is_empty() {
        return b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0));
    }
    for (path, asc) in orders {
        let ord = match path.as_str() {
            "_count" => a.1.cmp(&b.1),
            "_key" => a.0.cmp(&b.0),
            _ => Ordering::Equal,
        };
        let ord = if *asc { ord } else { ord.reverse() };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.0.cmp(&b.0)
}

/// Composite key comparison for terms-only sources (mirror of the brute
/// sort: numeric when both sides parse as f64, else lexicographic; asc).
fn composite_cmp(a: &[String], b: &[String]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for i in 0..a.len().min(b.len()) {
        let (av, bv) = (&a[i], &b[i]);
        let cmp = if let (Ok(an), Ok(bn)) = (av.parse::<f64>(), bv.parse::<f64>()) {
            an.partial_cmp(&bn).unwrap_or(Ordering::Equal)
        } else {
            av.cmp(bv)
        };
        if cmp != Ordering::Equal {
            return cmp;
        }
    }
    a.len().cmp(&b.len())
}

/// True when a sub-agg tree references background-corpus semantics the
/// sampler fast path can't provide (it hands the sample as `all_docs`).
fn sub_tree_needs_background(sub: &Value) -> bool {
    match sub {
        Value::Object(o) => o.iter().any(|(k, v)| {
            matches!(
                k.as_str(),
                "significant_terms" | "significant_text" | "global"
            ) || (k == "min_doc_count" && v.as_u64() == Some(0))
                || sub_tree_needs_background(v)
        }),
        Value::Array(a) => a.iter().any(sub_tree_needs_background),
        _ => false,
    }
}

// ── predicate compilation (filter / filters / adjacency_matrix) ─────────────

enum Pred {
    MatchAll,
    TermKw {
        field: String,
        value: String,
    },
    TermsKw {
        field: String,
        values: Vec<String>,
    },
    RangeNum {
        field: String,
        lo: f64,
        lo_incl: bool,
        hi: f64,
        hi_incl: bool,
    },
    /// Conjunction of leaf predicates — a `bool` with only `must`/`filter`
    /// clauses (produced by `compile_top_pred` for the top-level query filter;
    /// `compile_pred` never yields this, so the filter/filters/adjacency
    /// executors are unaffected).
    And(Vec<Pred>),
}

/// Compile a TOP-LEVEL query filter (`{size:0, query:Q, aggs:…}`) into a
/// columnar predicate.  Leaves delegate to the trusted `compile_pred` (kept
/// perfectly in sync with `doc_matches_filter`); a `bool` with only
/// `must`/`filter` clauses becomes a pure conjunction.  Anything else
/// (must_not, should, minimum_should_match, FTS leaves, …) → `None`, so the
/// caller falls back to the exact brute path.
fn compile_top_pred(filter: &Value) -> Option<Pred> {
    let obj = filter.as_object()?;
    if obj.len() != 1 {
        return None;
    }
    let (qtype, body) = obj.iter().next()?;
    if qtype == "bool" {
        let bo = body.as_object()?;
        if bo.keys().any(|k| k != "must" && k != "filter") {
            return None; // must_not / should / minimum_should_match → bail
        }
        let mut subs: Vec<Pred> = Vec::new();
        for key in ["must", "filter"] {
            if let Some(clauses) = bo.get(key) {
                let arr = clauses.as_array()?;
                for c in arr {
                    subs.push(compile_top_pred(c)?);
                }
            }
        }
        return match subs.len() {
            0 => Some(Pred::MatchAll),
            1 => Some(subs.into_iter().next().unwrap()),
            _ => Some(Pred::And(subs)),
        };
    }
    compile_pred(filter)
}

/// Structural equivalent of the old
/// `aggs_def.to_string().contains("top_hits" | "\"_id\"" | ...)` probe.
/// Returns true when the agg tree references a meta field, meaning the
/// memtable docs must be materialised OWNED (with `_id`/`_index`/`_seq_no`
/// injected) rather than Arc-shared.  Conservative: a false positive only
/// costs an extra clone, never correctness — so this deliberately mirrors
/// the substring semantics (`top_hits` anywhere; the meta fields as exact
/// key/string tokens, i.e. the serialized quoted form).
fn agg_tree_mentions_meta(v: &Value) -> bool {
    fn is_meta(s: &str) -> bool {
        s.contains("top_hits") || s == "_id" || s == "_index" || s == "_seq_no"
    }
    match v {
        Value::Object(m) => m
            .iter()
            .any(|(k, val)| is_meta(k) || agg_tree_mentions_meta(val)),
        Value::Array(a) => a.iter().any(agg_tree_mentions_meta),
        Value::String(s) => is_meta(s),
        _ => false,
    }
}

/// Compile the (single-clause) filter queries `doc_matches_filter` supports
/// into a columnar predicate.  Anything not exactly mirrored → `None`.
fn compile_pred(filter: &Value) -> Option<Pred> {
    let obj = filter.as_object()?;
    if obj.len() != 1 {
        return None;
    }
    let (qtype, body) = obj.iter().next()?;
    match qtype.as_str() {
        "match_all" => Some(Pred::MatchAll),
        "term" => {
            let fm = body.as_object()?;
            if fm.len() != 1 {
                return None;
            }
            let (field, expected) = fm.iter().next()?;
            let value = match expected {
                Value::String(s) => s.clone(),
                Value::Object(o) => match o.get("value") {
                    Some(Value::String(s)) => s.clone(),
                    _ => return None,
                },
                _ => return None, // numeric/bool string-form subtleties → bail
            };
            Some(Pred::TermKw {
                field: field.clone(),
                value,
            })
        }
        "terms" => {
            let fm = body.as_object()?;
            if fm.len() != 1 {
                return None;
            }
            let (field, expected) = fm.iter().next()?;
            let arr = expected.as_array()?;
            let mut values = Vec::with_capacity(arr.len());
            for v in arr {
                match v {
                    Value::String(s) => values.push(s.clone()),
                    _ => return None,
                }
            }
            Some(Pred::TermsKw {
                field: field.clone(),
                values,
            })
        }
        "range" => {
            let fm = body.as_object()?;
            if fm.len() != 1 {
                return None;
            }
            let (field, bounds) = fm.iter().next()?;
            let bo = bounds.as_object()?;
            if !bo
                .keys()
                .all(|k| matches!(k.as_str(), "gte" | "gt" | "lte" | "lt"))
            {
                return None;
            }
            let get_num = |k: &str| -> Option<Option<f64>> {
                match bo.get(k) {
                    None => Some(None),
                    Some(Value::Number(n)) => Some(n.as_f64()),
                    Some(_) => None,
                }
            };
            let gte = get_num("gte")?;
            let gt = get_num("gt")?;
            let lte = get_num("lte")?;
            let lt = get_num("lt")?;
            let (lo, lo_incl) = match (gte, gt) {
                (Some(v), _) => (v, true),
                (None, Some(v)) => (v, false),
                (None, None) => (f64::NEG_INFINITY, true),
            };
            let (hi, hi_incl) = match (lte, lt) {
                (Some(v), _) => (v, true),
                (None, Some(v)) => (v, false),
                (None, None) => (f64::INFINITY, true),
            };
            Some(Pred::RangeNum {
                field: field.clone(),
                lo,
                lo_incl,
                hi,
                hi_incl,
            })
        }
        _ => None,
    }
}

enum SegPred<'a> {
    Never,
    Always,
    /// Last bool = column has no nulls (hoisted roaring probe).
    KwEq(&'a KeywordColumn, u32, bool),
    KwIn(&'a KeywordColumn, Vec<u32>, bool),
    Num(&'a NumericColumn, f64, bool, f64, bool, bool),
    /// Conjunction — every sub-predicate must match the row.
    And(Vec<SegPred<'a>>),
}

fn resolve_pred<'a>(
    cols: &'a std::collections::BTreeMap<String, Column>,
    pred: &Pred,
) -> Option<SegPred<'a>> {
    Some(match pred {
        Pred::MatchAll => SegPred::Always,
        Pred::TermKw { field, value } => match cols.get(field) {
            Some(Column::Keyword(k)) => match k.ord_for_term(value) {
                Some(ord) => SegPred::KwEq(k, ord, k.null_bitmap.is_empty()),
                None => SegPred::Never,
            },
            Some(Column::Numeric(_)) => return None,
            None => SegPred::Never,
        },
        Pred::TermsKw { field, values } => match cols.get(field) {
            Some(Column::Keyword(k)) => {
                let ords: Vec<u32> = values.iter().filter_map(|v| k.ord_for_term(v)).collect();
                if ords.is_empty() {
                    SegPred::Never
                } else {
                    SegPred::KwIn(k, ords, k.null_bitmap.is_empty())
                }
            }
            Some(Column::Numeric(_)) => return None,
            None => SegPred::Never,
        },
        Pred::RangeNum {
            field,
            lo,
            lo_incl,
            hi,
            hi_incl,
        } => match cols.get(field) {
            Some(Column::Numeric(n)) => {
                SegPred::Num(n, *lo, *lo_incl, *hi, *hi_incl, n.null_bitmap.is_empty())
            }
            Some(Column::Keyword(_)) => return None,
            None => SegPred::Never,
        },
        Pred::And(subs) => {
            let mut resolved: Vec<SegPred<'a>> = Vec::with_capacity(subs.len());
            for s in subs {
                let sp = resolve_pred(cols, s)?;
                if matches!(sp, SegPred::Never) {
                    return Some(SegPred::Never); // short-circuit: whole AND is empty
                }
                resolved.push(sp);
            }
            SegPred::And(resolved)
        }
    })
}

#[inline]
fn seg_pred_matches(sp: &SegPred<'_>, row: u32) -> bool {
    match sp {
        SegPred::Never => false,
        SegPred::Always => true,
        SegPred::KwEq(k, ord, no_nulls) => {
            if *no_nulls {
                k.ords.get(row as usize) == Some(ord)
            } else {
                k.ord_for(row) == Some(*ord)
            }
        }
        SegPred::KwIn(k, ords, no_nulls) => {
            let o = if *no_nulls {
                k.ords.get(row as usize).copied()
            } else {
                k.ord_for(row)
            };
            match o {
                Some(o) => ords.contains(&o),
                None => false,
            }
        }
        SegPred::Num(n, lo, lo_incl, hi, hi_incl, no_nulls) => {
            if !*no_nulls && n.null_bitmap.contains(row) {
                return false;
            }
            let v = f64::from_bits(n.data[row as usize] as u64);
            let lo_ok = if *lo_incl { v >= *lo } else { v > *lo };
            let hi_ok = if *hi_incl { v <= *hi } else { v < *hi };
            lo_ok && hi_ok
        }
        SegPred::And(subs) => subs.iter().all(|s| seg_pred_matches(s, row)),
    }
}

fn seg_pred_count(sp: &SegPred<'_>, docs: u32) -> u64 {
    match sp {
        SegPred::Never => 0,
        SegPred::Always => docs as u64,
        SegPred::KwEq(k, ord, _) => k.per_ord_count.get(*ord as usize).copied().unwrap_or(0) as u64,
        SegPred::KwIn(k, ords, _) => ords
            .iter()
            .map(|o| k.per_ord_count.get(*o as usize).copied().unwrap_or(0) as u64)
            .sum(),
        SegPred::Num(n, lo, lo_incl, hi, hi_incl, _) => n.range_count(*lo, *hi, *lo_incl, *hi_incl),
        // Conjunction has no O(1) form — count matching rows directly.  Only
        // reached from the top-level filter count (compile_pred, used by the
        // filter/filters executors, never yields `And`).
        SegPred::And(_) => (0..docs).filter(|&row| seg_pred_matches(sp, row)).count() as u64,
    }
}

// ── variable_width_histogram weighted clustering ─────────────────────────────

pub(super) struct VwhBucket {
    pub min: f64,
    pub max: f64,
    pub sum: f64,
    pub count: u64,
}

/// Weighted replay of the brute adaptive-merge clustering
/// (`run_variable_width_histogram`): the brute algorithm starts from one
/// bucket per value *instance* and repeatedly merges the adjacent pair with
/// the strictly-smallest centroid gap (earliest pair wins ties).  All
/// zero-gap pairs (duplicate values) merge before any positive gap, and
/// they merge left-to-right, so the instance-level process is exactly
/// reproducible from the sorted distinct (value, weight) sequence.
/// K-way merge of value-ascending `(value, weight)` runs into one distinct
/// ascending list, summing weights of equal values across runs.
fn merge_weighted_runs(mut runs: Vec<Vec<(f64, u64)>>) -> Vec<(f64, u64)> {
    match runs.len() {
        0 => return Vec::new(),
        1 => return runs.pop().unwrap(),
        _ => {}
    }
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;
    // Heap entries keyed on the value's total_cmp bit pattern (all runs come
    // from finite doc values, so the sign-flip trick keeps ordering exact).
    fn key(v: f64) -> u64 {
        let b = v.to_bits();
        if b >> 63 == 1 {
            !b
        } else {
            b | (1 << 63)
        }
    }
    let total: usize = runs.iter().map(Vec::len).sum();
    let mut out: Vec<(f64, u64)> = Vec::with_capacity(total);
    let mut heap: BinaryHeap<Reverse<(u64, usize)>> = BinaryHeap::new();
    let mut pos: Vec<usize> = vec![0; runs.len()];
    for (ri, run) in runs.iter().enumerate() {
        if let Some(&(v, _)) = run.first() {
            heap.push(Reverse((key(v), ri)));
        }
    }
    while let Some(Reverse((_, ri))) = heap.pop() {
        let (v, w) = runs[ri][pos[ri]];
        match out.last_mut() {
            Some((lv, lw)) if *lv == v => *lw += w,
            _ => out.push((v, w)),
        }
        pos[ri] += 1;
        if let Some(&(nv, _)) = runs[ri].get(pos[ri]) {
            heap.push(Reverse((key(nv), ri)));
        }
    }
    out
}

/// Coarsen an ascending distinct `(value, weight)` list into at most `cap`
/// equal-weight groups, each represented by its weighted mean.  Order and
/// total weight are preserved; only used above the exact-clustering budget
/// where the reference (brute) implementation cannot run anyway.
fn coarsen_weighted(vals: &[(f64, u64)], cap: usize) -> Vec<(f64, u64)> {
    let total_w: u64 = vals.iter().map(|&(_, w)| w).sum();
    let per_group: u64 = total_w.div_ceil(cap as u64).max(1);
    let mut out: Vec<(f64, u64)> = Vec::with_capacity(cap + 1);
    let mut g_sum = 0.0_f64;
    let mut g_w = 0_u64;
    for &(v, w) in vals {
        g_sum += v * w as f64;
        g_w += w;
        if g_w >= per_group {
            out.push((g_sum / g_w as f64, g_w));
            g_sum = 0.0;
            g_w = 0;
        }
    }
    if g_w > 0 {
        out.push((g_sum / g_w as f64, g_w));
    }
    out
}

fn vwh_cluster(vals: &[(f64, u64)], num_buckets: usize) -> Vec<VwhBucket> {
    let n_total: u64 = vals.iter().map(|(_, w)| w).sum();
    let effective: u64 = (num_buckets as u64).min(n_total).max(1);
    let mut merges_needed: u64 = n_total - effective;
    let zero_merges: u64 = n_total - vals.len() as u64;

    if merges_needed <= zero_merges {
        // Phase A only: duplicates collapse left-to-right until the target
        // bucket count is reached.
        let mut out: Vec<VwhBucket> = Vec::new();
        for &(v, w) in vals {
            if merges_needed >= w - 1 {
                merges_needed -= w - 1;
                out.push(VwhBucket {
                    min: v,
                    max: v,
                    sum: v * w as f64,
                    count: w,
                });
            } else {
                // Partially collapsed run: one merged bucket of
                // (merges_needed + 1) instances, then singletons.
                let m = merges_needed;
                merges_needed = 0;
                out.push(VwhBucket {
                    min: v,
                    max: v,
                    sum: v * (m + 1) as f64,
                    count: m + 1,
                });
                for _ in 0..(w - 1 - m) {
                    out.push(VwhBucket {
                        min: v,
                        max: v,
                        sum: v,
                        count: 1,
                    });
                }
            }
        }
        return out;
    }

    // Phase B: all duplicates collapsed; merge weighted adjacent buckets by
    // centroid distance (smallest gap, earliest pair) until `effective`
    // buckets remain.  Heap + doubly-linked list = O(d log d).
    let mut remaining = merges_needed - zero_merges;
    let d = vals.len();
    let mut sum: Vec<f64> = vals.iter().map(|&(v, w)| v * w as f64).collect();
    let mut cnt: Vec<u64> = vals.iter().map(|&(_, w)| w).collect();
    let mut mn: Vec<f64> = vals.iter().map(|&(v, _)| v).collect();
    let mut mx: Vec<f64> = vals.iter().map(|&(v, _)| v).collect();
    let mut next: Vec<usize> = (0..d).map(|i| i + 1).collect(); // d = sentinel end
    let mut prev: Vec<usize> = (0..d).map(|i| i.wrapping_sub(1)).collect();
    let mut gen: Vec<u64> = vec![0; d];
    let mut alive: Vec<bool> = vec![true; d];

    use std::cmp::Reverse;
    use std::collections::BinaryHeap;
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    struct Entry(
        u64,   /*gap bits (total_cmp key)*/
        usize, /*left idx*/
        u64,   /*gen sum*/
    );
    let gap_key = |g: f64| -> u64 {
        // total_cmp-compatible ordering for non-negative floats.
        g.to_bits()
    };
    let mut heap: BinaryHeap<Reverse<Entry>> = BinaryHeap::new();
    let centroid = |i: usize, sum: &[f64], cnt: &[u64]| sum[i] / cnt[i] as f64;
    for i in 0..d.saturating_sub(1) {
        let g = (centroid(i + 1, &sum, &cnt) - centroid(i, &sum, &cnt)).abs();
        heap.push(Reverse(Entry(gap_key(g), i, 0)));
    }
    while remaining > 0 {
        let Some(Reverse(Entry(_, left, gsum))) = heap.pop() else {
            break;
        };
        if !alive[left] {
            continue;
        }
        let right = next[left];
        if right >= d || !alive[right] {
            continue;
        }
        if gen[left] + gen[right] != gsum {
            continue; // stale entry
        }
        // Merge right into left.
        sum[left] += sum[right];
        cnt[left] += cnt[right];
        if mn[right] < mn[left] {
            mn[left] = mn[right];
        }
        if mx[right] > mx[left] {
            mx[left] = mx[right];
        }
        alive[right] = false;
        let rr = next[right];
        next[left] = rr;
        if rr < d {
            prev[rr] = left;
        }
        gen[left] += 1;
        remaining -= 1;
        // Refresh neighbour gaps.
        let pl = prev[left];
        if pl != usize::MAX && pl < d && alive[pl] {
            let g = (centroid(left, &sum, &cnt) - centroid(pl, &sum, &cnt)).abs();
            heap.push(Reverse(Entry(gap_key(g), pl, gen[pl] + gen[left])));
        }
        if rr < d && alive[rr] {
            let g = (centroid(rr, &sum, &cnt) - centroid(left, &sum, &cnt)).abs();
            heap.push(Reverse(Entry(gap_key(g), left, gen[left] + gen[rr])));
        }
    }
    let mut out = Vec::new();
    let mut i = 0usize;
    // Find the first alive bucket.
    while i < d && !alive[i] {
        i += 1;
    }
    while i < d {
        out.push(VwhBucket {
            min: mn[i],
            max: mx[i],
            sum: sum[i],
            count: cnt[i],
        });
        i = next[i];
        while i < d && !alive[i] {
            i += 1;
        }
    }
    out
}
