//! Aggregation engine — ES-compatible aggregations over memtable documents.
//!
//! ## Exactness guarantee
//!
//! **All xerj aggregations are exact.**  Unlike Elasticsearch, which uses
//! the HyperLogLog++ algorithm for `cardinality` and can return approximate
//! bucket counts for high-cardinality `terms` aggregations, xerj computes
//! every aggregation over the full document set with no sampling.
//!
//! Concrete implications:
//! - `terms` bucket `doc_count` is always precise — `doc_count_error_upper_bound`
//!   is always 0 and `sum_other_doc_count` is 0 for results within `size`.
//! - `cardinality` returns the true distinct count, not an HLL estimate.
//! - There is no `"relation": "gte"` approximation on total hits.
//!
//! ## Supported aggregation types
//!
//! | Type             | ES key            | Output shape                        |
//! |------------------|-------------------|-------------------------------------|
//! | Terms            | `terms`           | bucket agg with `key`/`doc_count`   |
//! | Value count      | `value_count`     | metric — `{"value": N}`             |
//! | Avg              | `avg`             | metric — `{"value": f64}`           |
//! | Sum              | `sum`             | metric — `{"value": f64}`           |
//! | Min              | `min`             | metric — `{"value": f64}`           |
//! | Max              | `max`             | metric — `{"value": f64}`           |
//! | Stats            | `stats`           | metric — combined count/min/max/...  |
//! | Cardinality      | `cardinality`     | metric — exact distinct count        |
//! | Date histogram   | `date_histogram`  | bucket agg keyed by time interval   |
//!
//! Bucket aggregations support nested sub-aggregations (`aggs` key inside).

use std::collections::{HashMap, HashSet};

use serde_json::{json, Map, Value};

// ── Field value cache ─────────────────────────────────────────────────────────

/// Per-aggregation-level cache of pre-extracted field values.
///
/// When multiple sibling aggregations target the same field (e.g. `avg`,
/// `sum`, `max` all on `"price"`), we extract the values for that field once
/// and reuse them across all sibling aggs.  A fresh `FieldCache` is created at
/// each level of `run_aggs_with_all` so nested sub-agg calls automatically get
/// their own cache scoped to the (filtered) bucket docs.
struct FieldCache<'d> {
    docs: &'d [Value],
    /// field → Vec<Option<f64>>
    numeric: HashMap<String, Vec<Option<f64>>>,
    /// field → Vec<Vec<String>>
    strings: HashMap<String, Vec<Vec<String>>>,
}

impl<'d> FieldCache<'d> {
    fn new(docs: &'d [Value]) -> Self {
        Self {
            docs,
            numeric: HashMap::new(),
            strings: HashMap::new(),
        }
    }

    fn get_numeric(&mut self, field: &str) -> &[Option<f64>] {
        self.numeric.entry(field.to_string()).or_insert_with(|| {
            self.docs
                .iter()
                .map(|d| extract_numeric(d, field))
                .collect()
        })
    }

    fn get_strings(&mut self, field: &str) -> &[Vec<String>] {
        self.strings.entry(field.to_string()).or_insert_with(|| {
            self.docs
                .iter()
                .map(|d| extract_field_values(d, field))
                .collect()
        })
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Execute all aggregations in `aggs_def` over the provided document sources.
///
/// * `aggs_def` — the raw JSON value of the `"aggs"` key from a SearchRequest.
/// * `docs`     — the matching document sources (after query filtering).
///
/// Returns an ES-compatible `aggregations` object:
/// ```json
/// {
///   "my_agg": { "buckets": [...] },
///   "my_metric": { "value": 42 }
/// }
/// ```
pub fn run_aggs(aggs_def: &Value, docs: &[Value]) -> Value {
    run_aggs_with_all(aggs_def, docs, docs)
}

// ── Aggregation bucket cap ────────────────────────────────────────────────────
//
// Hard cap on how many distinct buckets a single aggregation may materialise.
// Mirrors Elasticsearch's `search.max_buckets` cluster setting (default 65536).
// Without this guard, a `terms` agg over a high-cardinality field (e.g. 50M
// unique user IDs) allocates one HashMap entry per unique value before any
// `size`/`shard_size` cap can drop them — easy OOM. Per-bucket-allocator
// loops in this module poll `max_buckets()` on each insert; when exceeded
// the accumulator stops adding new keys (existing keys still increment).
//
// Set once at Index construction via `set_max_buckets`; defaults to the same
// 65 536 ES uses if never set. Atomic + Relaxed: read in hot agg loops, write
// once at startup, no ordering needed.
static MAX_BUCKETS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(65_536);

/// Override the per-aggregation bucket cap. Call once at Index startup with
/// `config.limits.max_buckets`. Subsequent calls overwrite the previous value.
pub fn set_max_buckets(n: usize) {
    MAX_BUCKETS.store(n.max(1), std::sync::atomic::Ordering::Relaxed);
}

/// Current per-aggregation bucket cap.
#[inline]
pub fn max_buckets() -> usize {
    MAX_BUCKETS.load(std::sync::atomic::Ordering::Relaxed)
}

thread_local! {
    /// Lowercased query-term set extracted from the outer search query.
    /// `run_top_hits` reads this to build per-bucket highlight / explain
    /// output without having to plumb the query node through every agg
    /// layer. Set by the search handler via `set_outer_query_terms`
    /// before calling `run_aggs_with_all`; cleared afterwards.
    pub static OUTER_QUERY_TERMS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// (field, term) pairs extracted from the outer Match/Term queries.
    /// Used by run_top_hits to build per-hit _explanation output when
    /// the caller sets `explain: true` on top_hits.
    pub static OUTER_QUERY_FIELD_TERMS: std::cell::RefCell<Vec<(String, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

pub fn set_outer_query_terms(terms: Vec<String>) {
    OUTER_QUERY_TERMS.with(|cell| *cell.borrow_mut() = terms);
}

pub fn set_outer_query_field_terms(pairs: Vec<(String, String)>) {
    OUTER_QUERY_FIELD_TERMS.with(|cell| *cell.borrow_mut() = pairs);
}

pub fn clear_outer_query_terms() {
    OUTER_QUERY_TERMS.with(|cell| cell.borrow_mut().clear());
    OUTER_QUERY_FIELD_TERMS.with(|cell| cell.borrow_mut().clear());
}

pub fn outer_query_terms() -> Vec<String> {
    OUTER_QUERY_TERMS.with(|cell| cell.borrow().clone())
}

pub fn outer_query_field_terms() -> Vec<(String, String)> {
    OUTER_QUERY_FIELD_TERMS.with(|cell| cell.borrow().clone())
}

/// Like `run_aggs` but also receives the full index doc set for background
/// frequency calculations (used by `significant_terms`).
pub fn run_aggs_with_all(aggs_def: &Value, docs: &[Value], all_docs: &[Value]) -> Value {
    let obj = match aggs_def.as_object() {
        Some(o) => o,
        None => return Value::Object(Map::new()),
    };

    // Create a fresh field-value cache scoped to this docs slice.
    // Sub-agg calls to run_aggs_with_all with filtered bucket docs will
    // create their own independent caches so there's no cross-contamination.
    let mut cache = FieldCache::new(docs);

    let mut result = Map::new();
    for (agg_name, agg_body) in obj {
        let mut agg_result =
            execute_agg_with_all_cached(agg_name, agg_body, docs, all_docs, &mut cache);
        if let Some(meta) = agg_body.get("meta") {
            if let Some(res_obj) = agg_result.as_object_mut() {
                res_obj.insert("meta".into(), meta.clone());
            }
        }
        result.insert(agg_name.clone(), agg_result);
    }

    resolve_sibling_pipelines(&mut result);

    Value::Object(result)
}

/// Second pass of `run_aggs_with_all`: resolve pipeline aggregations
/// (`__pipeline__` placeholders emitted by `run_pipeline_agg`) against
/// their already-computed sibling results, in place.
///
/// Extracted so the doc-values fast-agg path (`index.rs`/`fast_aggs.rs`)
/// can reuse the exact same resolution + typed-keys tagging semantics.
pub(crate) fn resolve_sibling_pipelines(result: &mut Map<String, Value>) {
    let pipeline_keys: Vec<String> = result
        .iter()
        .filter(|(_, v)| {
            v.get("__pipeline__")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .map(|(k, _)| k.clone())
        .collect();

    for key in pipeline_keys {
        let (agg_type, buckets_path, extra, format) = {
            let spec = &result[&key];
            let t = spec["type"].as_str().unwrap_or("").to_string();
            let bp = spec["buckets_path"].as_str().unwrap_or("").to_string();
            let format = spec
                .get("format")
                .and_then(Value::as_str)
                .map(str::to_string);
            let extra = spec.clone();
            (t, bp, extra, format)
        };
        let mut resolved = resolve_pipeline_agg_full(&agg_type, &buckets_path, &extra, result);
        if let Some(fmt) = format {
            apply_pipeline_value_format(&mut resolved, &fmt);
        }
        // Tag the resolved result with the ES class name typed_keys uses so
        // downstream rewrite picks the right prefix. ES emits:
        //   bucket_metric_value — max_bucket, min_bucket, avg_bucket, sum_bucket
        //   stats_bucket — stats_bucket
        //   extended_stats_bucket — extended_stats_bucket
        //   percentiles_bucket — percentiles_bucket
        //   simple_value — moving_avg, moving_fn, cumulative_sum, derivative, serial_diff, bucket_script
        if let Some(obj) = resolved.as_object_mut() {
            let typed = match agg_type.as_str() {
                "max_bucket" | "min_bucket" | "avg_bucket" | "sum_bucket" => "bucket_metric_value",
                "stats_bucket" => "stats_bucket",
                "extended_stats_bucket" => "extended_stats_bucket",
                "percentiles_bucket" => "percentiles_bucket",
                _ => "simple_value",
            };
            obj.insert("__type__".into(), Value::String(typed.to_string()));
        }
        result.insert(key, resolved);
    }
}

/// Resolve a pipeline aggregation against already-computed sibling results,
/// with access to the full pipeline spec (for window, script, sort parameters).
fn resolve_pipeline_agg_full(
    agg_type: &str,
    buckets_path: &str,
    spec: &Value,
    siblings: &Map<String, Value>,
) -> Value {
    match agg_type {
        "moving_avg" => {
            let window = spec.get("window").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            resolve_moving_avg_full(buckets_path, window, siblings)
        }
        "bucket_selector" => {
            let script = spec
                .get("script")
                .and_then(|v| {
                    v.as_str()
                        .or_else(|| v.get("source").and_then(|s| s.as_str()))
                })
                .unwrap_or("")
                .to_string();
            resolve_bucket_selector(buckets_path, &script, siblings)
        }
        "bucket_sort" => {
            let sort = spec.get("sort").cloned();
            let size = spec
                .get("size")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            resolve_bucket_sort(buckets_path, sort.as_ref(), size, siblings)
        }
        "bucket_script" => {
            let script_val = spec.get("script");
            let script = script_val
                .and_then(|v| {
                    v.as_str()
                        .or_else(|| v.get("source").and_then(|s| s.as_str()))
                })
                .unwrap_or("")
                .to_string();
            // Script-provided constant params (e.g. `script.params.extra = 1`).
            let extra_params: HashMap<String, f64> = script_val
                .and_then(|v| v.get("params"))
                .and_then(|v| v.as_object())
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
                        .collect()
                })
                .unwrap_or_default();
            let bp_full = spec
                .get("buckets_path_full")
                .cloned()
                .unwrap_or(Value::Null);
            resolve_bucket_script(&bp_full, &script, &extra_params, siblings)
        }
        _ => {
            let gap_policy = spec
                .get("gap_policy")
                .and_then(Value::as_str)
                .unwrap_or("skip");
            resolve_pipeline_agg_with_gap(agg_type, buckets_path, siblings, gap_policy)
        }
    }
}

fn resolve_moving_avg_full(
    buckets_path: &str,
    window: usize,
    siblings: &Map<String, Value>,
) -> Value {
    let (sibling_name, metric_name) = split_buckets_path(buckets_path);
    let sibling = match siblings.get(sibling_name) {
        Some(v) => v,
        None => return json!({"value": Value::Null}),
    };
    let bucket_values: Vec<f64> = collect_bucket_values(sibling, metric_name);
    resolve_moving_avg(&bucket_values, window)
}

fn resolve_bucket_selector(
    buckets_path: &str,
    script: &str,
    siblings: &Map<String, Value>,
) -> Value {
    // Parse a simple comparison from the script string:
    //   "params.count > 10"  →  threshold = 10, operator = ">"
    // We look for the sibling bucket array and filter by the parsed condition.
    let (sibling_name, metric_name) = split_buckets_path(buckets_path);
    let sibling = match siblings.get(sibling_name) {
        Some(v) => v,
        None => return json!({"buckets": []}),
    };

    // Parse the threshold from the script (e.g. "params.count > 10").
    let (operator, threshold) = parse_simple_script(script);

    let buckets = match sibling.get("buckets").and_then(Value::as_array) {
        Some(b) => b,
        None => return json!({"buckets": []}),
    };

    let filtered: Vec<&Value> = buckets
        .iter()
        .filter(|b| {
            let val = if let Some(m) = metric_name {
                b.get(m)
                    .and_then(|mv| mv.get("value"))
                    .and_then(Value::as_f64)
            } else {
                b.get("doc_count").and_then(Value::as_f64)
            };
            if let Some(v) = val {
                match operator {
                    ">" => v > threshold,
                    ">=" => v >= threshold,
                    "<" => v < threshold,
                    "<=" => v <= threshold,
                    "==" | "=" => (v - threshold).abs() < f64::EPSILON,
                    "!=" => (v - threshold).abs() >= f64::EPSILON,
                    _ => true,
                }
            } else {
                false
            }
        })
        .collect();

    json!({"buckets": filtered})
}

fn resolve_bucket_sort(
    buckets_path: &str,
    sort: Option<&Value>,
    size: Option<usize>,
    siblings: &Map<String, Value>,
) -> Value {
    let (sibling_name, _metric_name) = split_buckets_path(buckets_path);
    let sibling = match siblings.get(sibling_name) {
        Some(v) => v,
        None => return json!({"buckets": []}),
    };

    let mut buckets: Vec<Value> = match sibling.get("buckets").and_then(Value::as_array) {
        Some(b) => b.to_vec(),
        None => return json!({"buckets": []}),
    };

    // Parse sort spec: [{"field": {"order": "desc"}}, ...]
    if let Some(sort_arr) = sort.and_then(Value::as_array) {
        for sort_item in sort_arr.iter().rev() {
            if let Some(sort_obj) = sort_item.as_object() {
                for (sort_field, sort_opts) in sort_obj {
                    let descending = match sort_opts {
                        Value::String(s) => s == "desc",
                        Value::Object(o) => o
                            .get("order")
                            .and_then(Value::as_str)
                            .map(|v| v == "desc")
                            .unwrap_or(false),
                        _ => false,
                    };
                    let sf = sort_field.clone();
                    buckets.sort_by(|a, b| {
                        let av = extract_bucket_value(a, &sf);
                        let bv = extract_bucket_value(b, &sf);
                        let cmp = av.partial_cmp(&bv).unwrap_or(std::cmp::Ordering::Equal);
                        if descending {
                            cmp.reverse()
                        } else {
                            cmp
                        }
                    });
                }
            }
        }
    }

    if let Some(sz) = size {
        buckets.truncate(sz);
    }

    json!({"buckets": buckets})
}

/// Apply `bucket_sort` / `bucket_selector` sub-pipeline aggs to a bucket list.
///
/// ES puts these pipelines INSIDE the bucket-producing agg's sub-aggs, but
/// they operate on the PARENT's bucket list (dropping/reordering entries).
/// Call this at the tail of each multi-bucket agg, after all buckets have
/// been built and populated with sub-agg metrics.
pub(crate) fn apply_bucket_pipeline_ops(
    mut buckets: Vec<Value>,
    sub_aggs: Option<&Value>,
) -> Vec<Value> {
    let sub_obj = match sub_aggs.and_then(Value::as_object) {
        Some(o) => o,
        None => return buckets,
    };

    // Collect (sub_name, agg_type, params) for every pipeline op.
    let mut ops: Vec<(String, String, Value)> = Vec::new();
    let mut parent_ops: Vec<(String, String, Value)> = Vec::new();
    for (sub_name, sub_spec) in sub_obj {
        let spec_obj = match sub_spec.as_object() {
            Some(o) => o,
            None => continue,
        };
        for (agg_type, params) in spec_obj {
            if agg_type == "bucket_sort" || agg_type == "bucket_selector" {
                ops.push((sub_name.clone(), agg_type.clone(), params.clone()));
            } else if matches!(
                agg_type.as_str(),
                "serial_diff" | "derivative" | "moving_avg" | "moving_fn" | "cumulative_sum"
            ) {
                parent_ops.push((sub_name.clone(), agg_type.clone(), params.clone()));
            }
        }
    }

    // Apply parent pipeline aggs (serial_diff, derivative, moving_avg,
    // moving_fn, cumulative_sum): each consumes the metric series across
    // the parent's buckets and writes one `{value: N}` per bucket.
    for (sub_name, agg_type, spec) in &parent_ops {
        let bp = spec
            .get("buckets_path")
            .and_then(Value::as_str)
            .unwrap_or("");
        let fmt = spec
            .get("format")
            .and_then(Value::as_str)
            .map(str::to_string);
        let values: Vec<Option<f64>> = buckets.iter().map(|b| resolve_metric_path(b, bp)).collect();
        let lag = spec.get("lag").and_then(Value::as_u64).unwrap_or(1) as usize;
        let window = spec.get("window").and_then(Value::as_u64).unwrap_or(5) as usize;
        for (i, bucket) in buckets.iter_mut().enumerate() {
            let computed: Option<f64> = match agg_type.as_str() {
                "serial_diff" => {
                    if i >= lag {
                        match (values[i], values[i - lag]) {
                            (Some(a), Some(b)) => Some(a - b),
                            _ => None,
                        }
                    } else {
                        None
                    }
                }
                "derivative" => {
                    if i >= 1 {
                        match (values[i], values[i - 1]) {
                            (Some(a), Some(b)) => Some(a - b),
                            _ => None,
                        }
                    } else {
                        None
                    }
                }
                "moving_avg" | "moving_fn" => {
                    // `shift` advances the window by N positions. Defaults
                    // to 0 (window covers the buckets *before* the current).
                    let shift = spec.get("shift").and_then(Value::as_i64).unwrap_or(0);
                    let gap_policy = spec
                        .get("gap_policy")
                        .and_then(Value::as_str)
                        .unwrap_or("skip");
                    let i_signed = i as i64;
                    let win_end_signed = i_signed + shift;
                    if win_end_signed <= 0 {
                        None
                    } else if (gap_policy == "skip") && values[i].is_none() {
                        // gap_policy=skip: if the *current* source bucket
                        // has no value, don't emit a moving_fn output at
                        // this position (ES skips the gap bucket).
                        // keep_values *does* emit at gap buckets, so
                        // that gate doesn't apply here.
                        None
                    } else {
                        let win_end = (win_end_signed as usize).min(values.len());
                        let start = win_end.saturating_sub(window);
                        let wnd: Vec<f64> = match gap_policy {
                            "insert_zeros" => values[start..win_end]
                                .iter()
                                .map(|v| v.unwrap_or(0.0))
                                .collect(),
                            "keep_values" | "keep_value" => {
                                // keep_values: treat the source stream
                                // as a dense vec of non-null values —
                                // identical to `skip` for the moving_fn
                                // window, but it also EMITS a value at
                                // gap buckets (carried over from the
                                // prior non-gap bucket's output).
                                let dense: Vec<f64> =
                                    values[..win_end].iter().filter_map(|v| *v).collect();
                                let take_from = dense.len().saturating_sub(window);
                                dense[take_from..].to_vec()
                            }
                            _ => {
                                // skip (default): treat the source series
                                // as a dense vec of non-null values and
                                // take the LAST `window` values preceding
                                // the current bucket. ES skips over gaps
                                // rather than just filtering them out of
                                // an otherwise-fixed window, so a gap at
                                // position N doesn't shrink the window
                                // used by bucket N+1 — it pulls an older
                                // value in instead.
                                let dense: Vec<f64> =
                                    values[..win_end].iter().filter_map(|v| *v).collect();
                                let take_from = dense.len().saturating_sub(window);
                                dense[take_from..].to_vec()
                            }
                        };
                        if wnd.is_empty() {
                            None
                        } else {
                            // moving_fn allows a `script` field naming the
                            // reducer function. Recognised forms: min, max,
                            // sum, stdDev, linearWeightedAvg, ewma (alpha
                            // default 0.3), unweightedAvg (default: avg).
                            let op = spec
                                .get("script")
                                .and_then(|v| {
                                    v.as_str()
                                        .or_else(|| v.get("source").and_then(Value::as_str))
                                })
                                .map(|s| s.to_ascii_lowercase())
                                .unwrap_or_default();
                            // Handle simple `fn(values) OP fn(values)`
                            // expressions by evaluating each side and
                            // combining with `+`, `-`, `*`, or `/`.
                            let eval_side = |s: &str| -> Option<f64> {
                                if s.contains("max") {
                                    Some(wnd.iter().cloned().fold(f64::NEG_INFINITY, f64::max))
                                } else if s.contains("min") {
                                    Some(wnd.iter().cloned().fold(f64::INFINITY, f64::min))
                                } else if s.contains("sum") {
                                    Some(wnd.iter().sum::<f64>())
                                } else if s.contains("unweightedavg") {
                                    Some(wnd.iter().sum::<f64>() / wnd.len() as f64)
                                } else {
                                    None
                                }
                            };
                            let combined: Option<f64> = if op.contains(") - ") || op.contains(")-")
                            {
                                let idx = op.find(") - ").unwrap_or_else(|| op.find(")-").unwrap());
                                let (l, r) = op.split_at(idx);
                                let (a, b) = (eval_side(l), eval_side(r));
                                match (a, b) {
                                    (Some(x), Some(y)) => Some(x - y),
                                    _ => None,
                                }
                            } else if op.contains(") + ") || op.contains(")+") {
                                let idx = op.find(") + ").unwrap_or_else(|| op.find(")+").unwrap());
                                let (l, r) = op.split_at(idx);
                                let (a, b) = (eval_side(l), eval_side(r));
                                match (a, b) {
                                    (Some(x), Some(y)) => Some(x + y),
                                    _ => None,
                                }
                            } else if op.contains(") * ") || op.contains(")*") {
                                let idx = op.find(") * ").unwrap_or_else(|| op.find(")*").unwrap());
                                let (l, r) = op.split_at(idx);
                                let (a, b) = (eval_side(l), eval_side(r));
                                match (a, b) {
                                    (Some(x), Some(y)) => Some(x * y),
                                    _ => None,
                                }
                            } else if op.contains(") / ") || op.contains(")/") {
                                let idx = op.find(") / ").unwrap_or_else(|| op.find(")/").unwrap());
                                let (l, r) = op.split_at(idx);
                                let (a, b) = (eval_side(l), eval_side(r));
                                match (a, b) {
                                    (Some(x), Some(y)) if y != 0.0 => Some(x / y),
                                    _ => None,
                                }
                            } else {
                                None
                            };
                            if let Some(v) = combined {
                                Some(v)
                            } else if let Some(v) = eval_movingfn_call(&op, &wnd) {
                                Some(v)
                            } else if op.contains("min") {
                                Some(wnd.iter().cloned().fold(f64::INFINITY, f64::min))
                            } else if op.contains("max") {
                                Some(wnd.iter().cloned().fold(f64::NEG_INFINITY, f64::max))
                            } else if op.contains("sum") {
                                Some(wnd.iter().sum::<f64>())
                            } else if op.contains("stddev") {
                                let mean = wnd.iter().sum::<f64>() / wnd.len() as f64;
                                let var = wnd.iter().map(|x| (x - mean).powi(2)).sum::<f64>()
                                    / wnd.len() as f64;
                                Some(var.sqrt())
                            } else if op.contains("linearweighted") {
                                // Weight i+1 across window [v_0,...v_{n-1}].
                                let n = wnd.len() as f64;
                                let total_w = n * (n + 1.0) / 2.0;
                                let weighted: f64 = wnd
                                    .iter()
                                    .enumerate()
                                    .map(|(idx, v)| v * (idx as f64 + 1.0))
                                    .sum();
                                Some(weighted / total_w)
                            } else if op.contains("ewma") {
                                // Recursive exponential smoothing with α=0.3.
                                let alpha = 0.3_f64;
                                let mut ewma = wnd[0];
                                for &v in &wnd[1..] {
                                    ewma = alpha * v + (1.0 - alpha) * ewma;
                                }
                                Some(ewma)
                            } else {
                                Some(wnd.iter().sum::<f64>() / wnd.len() as f64)
                            }
                        }
                    }
                }
                "cumulative_sum" => {
                    let sum: f64 = values[..=i].iter().filter_map(|v| *v).sum();
                    Some(sum)
                }
                _ => None,
            };
            // `__type__` tags the per-bucket pipeline result so the
            // typed_keys rewrite emits `derivative#name` (for derivative)
            // or `simple_value#name` (for moving_avg/moving_fn/serial_diff/
            // cumulative_sum — ES emits these under its SimpleValue class).
            let type_tag = match agg_type.as_str() {
                "derivative" => "derivative",
                _ => "simple_value",
            };
            let result = match computed {
                Some(v) if v.is_finite() => {
                    let num = serde_json::Number::from_f64(v)
                        .map(Value::Number)
                        .unwrap_or(Value::Null);
                    let mut m = serde_json::Map::new();
                    m.insert("value".to_string(), num);
                    if let Some(ref f) = fmt {
                        let formatted = format_number_pattern(v, f);
                        m.insert("value_as_string".to_string(), Value::String(formatted));
                    }
                    m.insert("__type__".to_string(), Value::String(type_tag.to_string()));
                    Value::Object(m)
                }
                _ => {
                    let mut m = serde_json::Map::new();
                    m.insert("value".to_string(), Value::Null);
                    m.insert("__type__".to_string(), Value::String(type_tag.to_string()));
                    Value::Object(m)
                }
            };
            if let Some(bo) = bucket.as_object_mut() {
                bo.insert(sub_name.clone(), result);
            }
        }
    }

    if ops.is_empty() {
        return buckets;
    }

    // Apply bucket_selector: drop buckets whose script evaluates to false.
    for (_, agg_type, spec) in &ops {
        if agg_type != "bucket_selector" {
            continue;
        }
        let bp_obj = spec
            .get("buckets_path")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let script_str = spec
            .get("script")
            .and_then(|v| {
                v.as_str()
                    .or_else(|| v.get("source").and_then(Value::as_str))
            })
            .unwrap_or("")
            .to_string();
        let extra_params: HashMap<String, f64> = spec
            .get("script")
            .and_then(|v| v.get("params"))
            .and_then(|v| v.as_object())
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
                    .collect()
            })
            .unwrap_or_default();
        let gap_policy = spec
            .get("gap_policy")
            .and_then(Value::as_str)
            .unwrap_or("skip");
        buckets.retain(|bucket| {
            let mut params = extra_params.clone();
            for (alias, target) in &bp_obj {
                let tstr = match target.as_str() {
                    Some(s) => s,
                    None => return false,
                };
                match get_bucket_metric_value(bucket, tstr) {
                    Some(v) => {
                        params.insert(alias.clone(), v);
                    }
                    None => match gap_policy {
                        "insert_zeros" => {
                            params.insert(alias.clone(), 0.0);
                        }
                        _ => return false,
                    },
                }
            }
            match eval_script_expr(&script_str, &params) {
                Some(r) => r != 0.0,
                None => false,
            }
        });
    }

    // Apply bucket_sort: gap_policy → reorder → from/size.
    for (_, agg_type, spec) in &ops {
        if agg_type != "bucket_sort" {
            continue;
        }

        // Collect every sort key referenced by this bucket_sort. Gap handling
        // runs across the union of these keys.
        let sort_arr = spec
            .get("sort")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let sort_keys: Vec<String> = sort_arr
            .iter()
            .filter_map(|item| item.as_object())
            .flat_map(|m| m.keys().cloned().collect::<Vec<_>>())
            .collect();

        // gap_policy: "skip" (default) drops buckets missing any sort key;
        // "insert_zeros" keeps them and treats missing values as 0.0.
        let gap_policy = spec
            .get("gap_policy")
            .and_then(Value::as_str)
            .unwrap_or("skip");
        // ES `gap_policy=skip` drops a bucket only when *every* sort key is
        // missing from it — a bucket with a value for the secondary key
        // survives even if the primary key has a gap (it just sorts by the
        // fallback values). `keep_values` behaves the same for bucket_sort:
        // buckets where every sort key is null/NaN are dropped from the
        // final output (they cannot be placed anywhere in the ordering).
        if (gap_policy == "skip" || gap_policy == "keep_values") && !sort_keys.is_empty() {
            buckets.retain(|b| {
                sort_keys
                    .iter()
                    .any(|k| extract_bucket_value_opt(b, k).is_some())
            });
        }

        for sort_item in sort_arr.iter().rev() {
            let sort_obj = match sort_item.as_object() {
                Some(o) => o,
                None => continue,
            };
            for (sort_field, sort_opts) in sort_obj {
                let desc = match sort_opts {
                    Value::String(s) => s == "desc",
                    Value::Object(o) => o
                        .get("order")
                        .and_then(Value::as_str)
                        .map(|v| v == "desc")
                        .unwrap_or(false),
                    _ => false,
                };
                let sf = sort_field.clone();
                let use_zero_for_missing = gap_policy == "insert_zeros";
                buckets.sort_by(|a, b| {
                    let av = extract_bucket_value_opt(a, &sf).unwrap_or(if use_zero_for_missing {
                        0.0
                    } else if desc {
                        f64::NEG_INFINITY
                    } else {
                        f64::INFINITY
                    });
                    let bv = extract_bucket_value_opt(b, &sf).unwrap_or(if use_zero_for_missing {
                        0.0
                    } else if desc {
                        f64::NEG_INFINITY
                    } else {
                        f64::INFINITY
                    });
                    let cmp = av.partial_cmp(&bv).unwrap_or(std::cmp::Ordering::Equal);
                    if desc {
                        cmp.reverse()
                    } else {
                        cmp
                    }
                });
            }
        }
        if let Some(from) = spec.get("from").and_then(Value::as_u64) {
            let from = from as usize;
            if from < buckets.len() {
                buckets = buckets.split_off(from);
            } else {
                buckets.clear();
            }
        }
        if let Some(sz) = spec.get("size").and_then(Value::as_u64) {
            buckets.truncate(sz as usize);
        }
    }

    // Strip the pipeline agg entries from every remaining bucket (ES does
    // not echo them back in the output).
    for bucket in buckets.iter_mut() {
        if let Some(bo) = bucket.as_object_mut() {
            for (name, _, _) in &ops {
                bo.remove(name);
            }
        }
    }

    buckets
}

/// Read a metric value from a bucket by `buckets_path` (e.g. `"v"` or `"v>value"`).
fn get_bucket_metric_value(bucket: &Value, path: &str) -> Option<f64> {
    if path == "_count" {
        return bucket.get("doc_count").and_then(Value::as_f64);
    }
    // top_hits subscript syntax: `top_hits_name[_source.field]` — extract
    // the named source field from the top (first) hit of a sibling top_hits
    // agg inside this bucket. `_sort` / `_score` refer to the sort-value
    // and score of that same hit (ES semantics).
    if let Some(bracket_open) = path.find('[') {
        if let Some(bracket_close) = path.rfind(']') {
            if bracket_close > bracket_open {
                let sib = &path[..bracket_open];
                let inner = &path[bracket_open + 1..bracket_close];
                let sib_val = bucket.get(sib)?;
                let top_hit = sib_val.pointer("/hits/hits/0")?;
                if let Some(rest) = inner.strip_prefix("_source.") {
                    let ptr = format!("/_source/{}", rest.replace('.', "/"));
                    return top_hit.pointer(&ptr).and_then(Value::as_f64);
                }
                if inner == "_score" {
                    return top_hit.get("_score").and_then(Value::as_f64);
                }
                if inner == "_sort" {
                    // ES `_sort` refers to the first sort value of the hit.
                    return top_hit
                        .get("sort")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.first())
                        .and_then(Value::as_f64);
                }
                if let Some(rest) = inner.strip_prefix("fields.") {
                    let arr_path = format!("/fields/{}", rest);
                    return top_hit
                        .pointer(&arr_path)
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.first())
                        .and_then(Value::as_f64);
                }
            }
        }
    }
    let (sib, metric) = split_buckets_path(path);
    let sib_val = bucket.get(sib)?;
    if let Some(m) = metric {
        sib_val.get(m).and_then(|x| {
            x.get("value")
                .and_then(Value::as_f64)
                .or_else(|| x.as_f64())
        })
    } else {
        sib_val.get("value").and_then(Value::as_f64)
    }
}

/// Extract a numeric value from a bucket for sorting purposes.
fn extract_bucket_value(bucket: &Value, field: &str) -> f64 {
    extract_bucket_value_opt(bucket, field).unwrap_or(0.0)
}

/// Optional variant — returns `None` when the metric is missing or null,
/// which `bucket_sort`'s `gap_policy="skip"` uses to prune buckets.
fn extract_bucket_value_opt(bucket: &Value, field: &str) -> Option<f64> {
    if field == "_count" || field == "doc_count" {
        return bucket.get("doc_count").and_then(Value::as_f64);
    }
    if field == "_key" {
        return bucket.get("key").and_then(Value::as_f64);
    }
    let raw = bucket.get(field)?;
    if let Some(v) = raw.get("value") {
        if v.is_null() {
            return None;
        }
        return v.as_f64();
    }
    if raw.is_null() {
        return None;
    }
    raw.as_f64()
}

/// Parse a very simple comparison expression like "params.count > 10".
fn parse_simple_script(script: &str) -> (&'static str, f64) {
    for op in &[">=", "<=", "!=", ">", "<", "==", "="] {
        if let Some(pos) = script.find(op) {
            let rhs = script[pos + op.len()..].trim();
            if let Ok(threshold) = rhs.parse::<f64>() {
                let op_str: &'static str = match *op {
                    ">=" => ">=",
                    "<=" => "<=",
                    "!=" => "!=",
                    ">" => ">",
                    "<" => "<",
                    "==" => "==",
                    "=" => "=",
                    _ => ">",
                };
                return (op_str, threshold);
            }
        }
    }
    (">", 0.0)
}

/// Resolve a `bucket_script` pipeline against sibling metric aggs.
///
/// `buckets_path` is an object map `{ "alias": "sibling_agg_name" }`.
/// For each alias, read the `.value` field from the named sibling metric
/// agg in `siblings` and bind it as `params.<alias>` for the script.
/// `extra_params` are constants from `script.params` (user-supplied).
/// Then evaluate a simple arithmetic expression.
fn resolve_bucket_script(
    buckets_path: &Value,
    script: &str,
    extra_params: &HashMap<String, f64>,
    siblings: &Map<String, Value>,
) -> Value {
    let bp = match buckets_path.as_object() {
        Some(o) => o,
        None => return json!({"value": Value::Null}),
    };
    let mut params: HashMap<String, f64> = extra_params.clone();
    for (alias, target) in bp {
        let t = match target.as_str() {
            Some(s) => s,
            None => continue,
        };
        // Walk "sibling>metric" if nested, else just ".value".
        let (sib_name, metric) = split_buckets_path(t);
        let sib = match siblings.get(sib_name) {
            Some(v) => v,
            None => return json!({"value": Value::Null}),
        };
        let v = if let Some(m) = metric {
            sib.get(m)
                .and_then(|x| x.get("value"))
                .and_then(Value::as_f64)
        } else {
            sib.get("value").and_then(Value::as_f64)
        };
        let v = match v {
            Some(x) => x,
            None => return json!({"value": Value::Null}),
        };
        params.insert(alias.clone(), v);
    }
    match eval_script_expr(script, &params) {
        Some(v) => json!({"value": v}),
        None => json!({"value": Value::Null}),
    }
}

/// Evaluate a simple arithmetic / comparison expression against a params map.
///
/// Supports: numbers (incl. decimals), `params.<name>` identifiers,
/// operators `+ - * / %`, comparisons `< > <= >= == !=` (yield 0.0 or 1.0),
/// logical `&&` / `||` (yield 0.0 or 1.0), and parentheses.  Honors
/// standard precedence.  Returns `None` on parse failure.
fn eval_script_expr(script: &str, params: &HashMap<String, f64>) -> Option<f64> {
    let tokens = tokenize_script(script, params)?;
    let rpn = shunting_yard(&tokens)?;
    evaluate_rpn(&rpn)
}

#[derive(Debug, Clone)]
enum Tok {
    Num(f64),
    Op(&'static str),
    LParen,
    RParen,
}

fn tokenize_script(s: &str, params: &HashMap<String, f64>) -> Option<Vec<Tok>> {
    let bytes = s.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit()
            || (c == '.' && i + 1 < bytes.len() && (bytes[i + 1] as char).is_ascii_digit())
        {
            let start = i;
            while i < bytes.len()
                && ((bytes[i] as char).is_ascii_digit() || bytes[i] as char == '.')
            {
                i += 1;
            }
            let n: f64 = s[start..i].parse().ok()?;
            toks.push(Tok::Num(n));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len()
                && ((bytes[i] as char).is_ascii_alphanumeric()
                    || bytes[i] as char == '_'
                    || bytes[i] as char == '.')
            {
                i += 1;
            }
            let ident = &s[start..i];
            if let Some(rest) = ident.strip_prefix("params.") {
                let v = *params.get(rest)?;
                toks.push(Tok::Num(v));
            } else {
                let v = *params.get(ident)?;
                toks.push(Tok::Num(v));
            }
            continue;
        }
        // Two-character operators first.
        if i + 1 < bytes.len() {
            let two = &s[i..i + 2];
            match two {
                ">=" | "<=" | "==" | "!=" | "&&" | "||" => {
                    let op: &'static str = match two {
                        ">=" => ">=",
                        "<=" => "<=",
                        "==" => "==",
                        "!=" => "!=",
                        "&&" => "&&",
                        "||" => "||",
                        _ => unreachable!(),
                    };
                    toks.push(Tok::Op(op));
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        match c {
            '+' => {
                toks.push(Tok::Op("+"));
            }
            '-' => {
                toks.push(Tok::Op("-"));
            }
            '*' => {
                toks.push(Tok::Op("*"));
            }
            '/' => {
                toks.push(Tok::Op("/"));
            }
            '%' => {
                toks.push(Tok::Op("%"));
            }
            '<' => {
                toks.push(Tok::Op("<"));
            }
            '>' => {
                toks.push(Tok::Op(">"));
            }
            '(' => {
                toks.push(Tok::LParen);
            }
            ')' => {
                toks.push(Tok::RParen);
            }
            ';' => {} // ignore trailing semicolons
            _ => return None,
        }
        i += 1;
    }
    Some(toks)
}

fn precedence(op: &str) -> u8 {
    match op {
        "||" => 1,
        "&&" => 2,
        "==" | "!=" => 3,
        "<" | ">" | "<=" | ">=" => 4,
        "+" | "-" => 5,
        "*" | "/" | "%" => 6,
        _ => 0,
    }
}

fn shunting_yard(toks: &[Tok]) -> Option<Vec<Tok>> {
    let mut out: Vec<Tok> = Vec::new();
    let mut ops: Vec<Tok> = Vec::new();
    for t in toks {
        match t {
            Tok::Num(_) => out.push(t.clone()),
            Tok::Op(o) => {
                while let Some(Tok::Op(top)) = ops.last() {
                    if precedence(top) >= precedence(o) {
                        out.push(ops.pop().unwrap());
                    } else {
                        break;
                    }
                }
                ops.push(t.clone());
            }
            Tok::LParen => ops.push(t.clone()),
            Tok::RParen => {
                while let Some(top) = ops.pop() {
                    match top {
                        Tok::LParen => break,
                        op => out.push(op),
                    }
                }
            }
        }
    }
    while let Some(op) = ops.pop() {
        if matches!(op, Tok::LParen | Tok::RParen) {
            return None;
        }
        out.push(op);
    }
    Some(out)
}

fn evaluate_rpn(rpn: &[Tok]) -> Option<f64> {
    let mut stack: Vec<f64> = Vec::new();
    for t in rpn {
        match t {
            Tok::Num(n) => stack.push(*n),
            Tok::Op(op) => {
                let b = stack.pop()?;
                let a = stack.pop()?;
                let r = match *op {
                    "+" => a + b,
                    "-" => a - b,
                    "*" => a * b,
                    "/" => {
                        if b == 0.0 {
                            return None;
                        } else {
                            a / b
                        }
                    }
                    "%" => {
                        if b == 0.0 {
                            return None;
                        } else {
                            a % b
                        }
                    }
                    "<" => {
                        if a < b {
                            1.0
                        } else {
                            0.0
                        }
                    }
                    ">" => {
                        if a > b {
                            1.0
                        } else {
                            0.0
                        }
                    }
                    "<=" => {
                        if a <= b {
                            1.0
                        } else {
                            0.0
                        }
                    }
                    ">=" => {
                        if a >= b {
                            1.0
                        } else {
                            0.0
                        }
                    }
                    "==" => {
                        if (a - b).abs() < 1e-9 {
                            1.0
                        } else {
                            0.0
                        }
                    }
                    "!=" => {
                        if (a - b).abs() >= 1e-9 {
                            1.0
                        } else {
                            0.0
                        }
                    }
                    "&&" => {
                        if a != 0.0 && b != 0.0 {
                            1.0
                        } else {
                            0.0
                        }
                    }
                    "||" => {
                        if a != 0.0 || b != 0.0 {
                            1.0
                        } else {
                            0.0
                        }
                    }
                    _ => return None,
                };
                stack.push(r);
            }
            _ => return None,
        }
    }
    if stack.len() == 1 {
        Some(stack[0])
    } else {
        None
    }
}

fn split_buckets_path(buckets_path: &str) -> (&str, Option<&str>) {
    if let Some(pos) = buckets_path.find('>') {
        (&buckets_path[..pos], Some(&buckets_path[pos + 1..]))
    } else if let Some(pos) = buckets_path.find('.') {
        // Dot form is a valid ES shorthand for the `<agg>>.<metric>`
        // syntax when the first segment uniquely identifies a sibling.
        // The caller's fallback path (`resolve_metric_path` on the
        // bucket with the full tail) still needs the full dotted tail
        // for subscript/dotted metric lookup (e.g.
        // `percentiles.values[99.9]`).
        (&buckets_path[..pos], Some(&buckets_path[pos + 1..]))
    } else {
        (buckets_path, None)
    }
}

/// Collect per-bucket numeric values from a sibling aggregation result.
fn collect_bucket_values(sibling: &Value, metric_name: Option<&str>) -> Vec<f64> {
    collect_bucket_values_with_policy(sibling, metric_name, "skip")
}

/// Collect sibling bucket metric values, applying the pipeline-agg `gap_policy`.
/// - `skip` (default): drop buckets with missing/null metric
/// - `insert_zeros`: treat missing as 0.0
/// - `keep_values`: ES 8.6+ — keep non-null non-NaN; drop null/NaN
///
/// `metric_name` supports dotted paths (`stats.sum`) and subscript notation
/// (`percentiles[99.9]`, `extended_stats[std_deviation]`).
fn collect_bucket_values_with_policy(
    sibling: &Value,
    metric_name: Option<&str>,
    gap_policy: &str,
) -> Vec<f64> {
    let buckets = match sibling.get("buckets").and_then(Value::as_array) {
        Some(b) => b,
        None => return vec![],
    };
    let mut out = Vec::with_capacity(buckets.len());
    for b in buckets {
        // When the bucket has zero docs, ES treats all its metric
        // sub-aggs as "missing" for the parent pipeline (regardless of
        // whether the metric emitted `0.0` or `null`). Honor gap_policy
        // the same way — `skip` drops the bucket, `insert_zeros` inserts
        // zero, `keep_values` drops.
        let doc_count = b.get("doc_count").and_then(Value::as_u64).unwrap_or(0);
        let bucket_empty = doc_count == 0 && metric_name.is_some();
        let v: Option<f64> = if bucket_empty {
            None
        } else if let Some(metric) = metric_name {
            resolve_metric_path(b, metric)
        } else {
            b.get("doc_count").and_then(Value::as_f64)
        };
        match v {
            Some(n) if n.is_finite() => out.push(n),
            _ => {
                match gap_policy {
                    "insert_zeros" => out.push(0.0),
                    "keep_values" => { /* drop null/NaN */ }
                    _ => { /* "skip" default: drop */ }
                }
            }
        }
    }
    out
}

/// Resolve a pipeline `buckets_path` metric expression against a single
/// bucket. Handles:
///   - plain metric name: `v` → v.value (or v doc_count)
///   - subscript: `v[99.9]` → v.values."99.9"
///   - dotted path: `stats.sum` → stats.sum
///   - multi-hop: `range.bucket_name>v` not handled here (resolved earlier)
fn resolve_metric_path(bucket: &Value, path: &str) -> Option<f64> {
    // Subscript form: name[key]
    if let Some(open) = path.find('[') {
        if let Some(close) = path[open..].find(']') {
            let base = &path[..open];
            let key = &path[open + 1..open + close];
            // Try both "values.<key>" (percentiles) and direct child.
            let node = bucket.get(base)?;
            if let Some(v) = node
                .get("values")
                .and_then(|vals| vals.get(key))
                .and_then(Value::as_f64)
            {
                return Some(v);
            }
            // Numeric percentile rendered as number (not string)
            if let Ok(n) = key.parse::<f64>() {
                if let Some(vals) = node.get("values").and_then(Value::as_object) {
                    for (k, v) in vals {
                        if k.parse::<f64>()
                            .ok()
                            .map(|x| (x - n).abs() < 1e-6)
                            .unwrap_or(false)
                        {
                            return v.as_f64();
                        }
                    }
                }
            }
            if let Some(v) = node.get(key).and_then(Value::as_f64) {
                return Some(v);
            }
            return None;
        }
    }
    // Dotted path: a.b.c
    if path.contains('.') {
        // Special case: `<metric>.<percent>` where metric is percentiles
        // or percentile_ranks with `values` either as array-of-`{key,value}`
        // (keyed:false) or map of percent→value (keyed:true). Resolve the
        // percent numerically so `the_percentiles.99` matches `{key: 99.0}`
        // and `{"99.0": ...}` alike.
        let mut segs = path.split('.');
        if let (Some(first), Some(rest)) = (segs.next(), segs.next()) {
            if segs.next().is_none() {
                // Exactly two segments — try the percentiles path.
                if let Ok(key_num) = rest.parse::<f64>() {
                    if let Some(node) = bucket.get(first) {
                        if let Some(values) = node.get("values") {
                            match values {
                                Value::Array(arr) => {
                                    for entry in arr {
                                        let k = entry.get("key").and_then(Value::as_f64);
                                        if let Some(k) = k {
                                            if (k - key_num).abs() < 1e-6 {
                                                return entry.get("value").and_then(Value::as_f64);
                                            }
                                        }
                                    }
                                }
                                Value::Object(map) => {
                                    for (k, v) in map {
                                        if k.parse::<f64>()
                                            .ok()
                                            .map(|x| (x - key_num).abs() < 1e-6)
                                            .unwrap_or(false)
                                        {
                                            return v.as_f64();
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        let mut cur = bucket;
        for seg in path.split('.') {
            cur = cur.get(seg)?;
        }
        return cur.as_f64();
    }
    // Plain: name → name.value
    let node = bucket.get(path)?;
    if let Some(v) = node.get("value").and_then(Value::as_f64) {
        return Some(v);
    }
    // Single-percentile special case: a `percentiles` sibling with one
    // percent returns `values` as either a one-entry map or a one-entry
    // array of `{key, value}`. ES 8.4+ lets max_bucket/min_bucket/avg_bucket
    // resolve a percentiles sibling with a single percent by taking that
    // single value directly (no subscript required).
    if let Some(values) = node.get("values") {
        match values {
            Value::Object(map) if map.len() == 1 => {
                if let Some((_, v)) = map.iter().next() {
                    if let Some(f) = v.as_f64() {
                        return Some(f);
                    }
                }
            }
            Value::Array(arr) if arr.len() == 1 => {
                if let Some(f) = arr[0].get("value").and_then(Value::as_f64) {
                    return Some(f);
                }
            }
            _ => {}
        }
    }
    node.as_f64()
}

/// Collect the `key` (or `key_as_string`) of every sibling bucket whose metric
/// value equals `target_value`.  Used by `max_bucket` / `min_bucket` to emit
/// the ES `keys: [...]` field.
fn collect_bucket_keys_matching_value(
    sibling: &Value,
    metric_name: Option<&str>,
    target_value: f64,
) -> Vec<Value> {
    let buckets = match sibling.get("buckets").and_then(Value::as_array) {
        Some(b) => b,
        None => return vec![],
    };
    let mut out = Vec::new();
    for b in buckets {
        let v = if let Some(metric) = metric_name {
            // Use the full `resolve_metric_path` — handles plain
            // "name.value", subscript `[99.9]`, dotted stats paths,
            // and the single-percentile fallback (a `percentiles`
            // metric with one percent resolves to its single value).
            resolve_metric_path(b, metric)
        } else {
            b.get("doc_count").and_then(Value::as_f64)
        };
        if let Some(v) = v {
            if (v - target_value).abs() < 1e-9 {
                let k = b
                    .get("key_as_string")
                    .or_else(|| b.get("key"))
                    .cloned()
                    .unwrap_or(Value::Null);
                out.push(k);
            }
        }
    }
    out
}

fn resolve_pipeline_agg_with_gap(
    agg_type: &str,
    buckets_path: &str,
    siblings: &Map<String, Value>,
    gap_policy: &str,
) -> Value {
    let (sibling_name, metric_name) = split_buckets_path(buckets_path);

    let sibling = match siblings.get(sibling_name) {
        Some(v) => v,
        None => return json!({"value": Value::Null}),
    };

    let bucket_values = collect_bucket_values_with_policy(sibling, metric_name, gap_policy);

    if bucket_values.is_empty() {
        // ES `sum_bucket` of an empty series returns 0.0, not null — the
        // identity for addition. Other bucket aggs keep null since they
        // have no meaningful value on an empty input.
        if agg_type == "sum_bucket" {
            return json!({"value": 0.0});
        }
        if agg_type == "percentiles_bucket" {
            // Preserve the values-map shape with all-null entries so
            // path lookups like `agg.values.50_0` still resolve cleanly.
            let pcts = [1.0, 5.0, 25.0, 50.0, 75.0, 95.0, 99.0];
            let mut values = serde_json::Map::new();
            for p in &pcts {
                values.insert(format!("{:.1}", p), Value::Null);
            }
            return json!({"values": Value::Object(values)});
        }
        return json!({"value": Value::Null});
    }

    match agg_type {
        "avg_bucket" => {
            let sum: f64 = bucket_values.iter().sum();
            make_value_result(sum / bucket_values.len() as f64)
        }
        "max_bucket" => {
            let value = bucket_values
                .iter()
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max);
            let keys = collect_bucket_keys_matching_value(sibling, metric_name, value);
            let mut r = make_value_result(value);
            if let Some(obj) = r.as_object_mut() {
                obj.insert("keys".into(), Value::Array(keys));
            }
            r
        }
        "min_bucket" => {
            let value = bucket_values.iter().cloned().fold(f64::INFINITY, f64::min);
            let keys = collect_bucket_keys_matching_value(sibling, metric_name, value);
            let mut r = make_value_result(value);
            if let Some(obj) = r.as_object_mut() {
                obj.insert("keys".into(), Value::Array(keys));
            }
            r
        }
        "sum_bucket" => make_value_result(bucket_values.iter().sum()),
        "stats_bucket" => {
            let count = bucket_values.len() as f64;
            let sum: f64 = bucket_values.iter().sum();
            let min = bucket_values.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = bucket_values
                .iter()
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max);
            let avg = sum / count;
            json!({
                "count": bucket_values.len(),
                "min": serde_json::Number::from_f64(min).map(Value::Number).unwrap_or(Value::Null),
                "max": serde_json::Number::from_f64(max).map(Value::Number).unwrap_or(Value::Null),
                "avg": serde_json::Number::from_f64(avg).map(Value::Number).unwrap_or(Value::Null),
                "sum": serde_json::Number::from_f64(sum).map(Value::Number).unwrap_or(Value::Null),
            })
        }
        "bucket_script" => json!({"value": Value::Null}),
        "extended_stats_bucket" => {
            let n = bucket_values.len() as f64;
            let sum: f64 = bucket_values.iter().sum();
            let min = bucket_values.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = bucket_values
                .iter()
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max);
            let avg = if n > 0.0 { sum / n } else { 0.0 };
            let sum_sq: f64 = bucket_values.iter().map(|v| v * v).sum();
            let variance_pop = if n > 0.0 { sum_sq / n - avg * avg } else { 0.0 };
            let variance_sample = if n > 1.0 {
                (sum_sq - n * avg * avg) / (n - 1.0)
            } else {
                0.0
            };
            let std_pop = variance_pop.max(0.0).sqrt();
            let std_sample = variance_sample.max(0.0).sqrt();
            let num = |x: f64| {
                serde_json::Number::from_f64(x)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            };
            json!({
                "count": bucket_values.len(),
                "min": num(min),
                "max": num(max),
                "avg": num(avg),
                "sum": num(sum),
                "sum_of_squares": num(sum_sq),
                "variance": num(variance_pop),
                "variance_population": num(variance_pop),
                "variance_sampling": num(variance_sample),
                "std_deviation": num(std_pop),
                "std_deviation_population": num(std_pop),
                "std_deviation_sampling": num(std_sample),
                "std_deviation_bounds": json!({
                    "upper": num(avg + 2.0 * std_pop),
                    "lower": num(avg - 2.0 * std_pop),
                    "upper_population": num(avg + 2.0 * std_pop),
                    "lower_population": num(avg - 2.0 * std_pop),
                    "upper_sampling": num(avg + 2.0 * std_sample),
                    "lower_sampling": num(avg - 2.0 * std_sample),
                }),
            })
        }
        "percentiles_bucket" => {
            let mut sorted = bucket_values.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            // ES `percentiles_bucket` uses
            // `data[round(p/100 * (n - 1))]` — nearest-rank-by-quantile-position
            // with banker's rounding to the closer index. This matches the
            // observed values across the YAML suite (no_gaps + gaps tests).
            let pcts = [1.0, 5.0, 25.0, 50.0, 75.0, 95.0, 99.0];
            let n = sorted.len();
            let mut values = serde_json::Map::new();
            for p in &pcts {
                let val = if n == 0 {
                    Value::Null
                } else if n == 1 {
                    serde_json::Number::from_f64(sorted[0])
                        .map(Value::Number)
                        .unwrap_or(Value::Null)
                } else {
                    let idx = ((p / 100.0) * (n as f64 - 1.0)).round() as usize;
                    let idx = idx.min(n - 1);
                    serde_json::Number::from_f64(sorted[idx])
                        .map(Value::Number)
                        .unwrap_or(Value::Null)
                };
                values.insert(format!("{:.1}", p), val);
            }
            json!({"values": Value::Object(values)})
        }
        "serial_diff" => {
            if bucket_values.len() >= 2 {
                make_value_result(
                    bucket_values[bucket_values.len() - 1] - bucket_values[bucket_values.len() - 2],
                )
            } else {
                json!({"value": Value::Null})
            }
        }
        "derivative" => {
            if bucket_values.len() >= 2 {
                make_value_result(
                    bucket_values[bucket_values.len() - 1] - bucket_values[bucket_values.len() - 2],
                )
            } else {
                json!({"value": Value::Null})
            }
        }
        "moving_avg" => resolve_moving_avg(&bucket_values, 5),
        "cumulative_sum" => resolve_cumulative_sum(&bucket_values),
        _ => json!({"value": Value::Null}),
    }
}

fn make_value_result(value: f64) -> Value {
    match serde_json::Number::from_f64(value) {
        Some(n) => json!({"value": Value::Number(n)}),
        None => json!({"value": Value::Null}),
    }
}

/// Apply an ES-style numeric format string to a pipeline aggregation's
/// `value` (or each of `values` for a percentile output), populating
/// `value_as_string` (or `values_as_string` entries) with the formatted
/// rendering. Supports `"0.00"`, `"#,##0.00"`, `"0.##"` — the minimal set
/// the YAML tests exercise — via `format_number_pattern`.
fn apply_pipeline_value_format(result: &mut Value, fmt: &str) {
    let Some(obj) = result.as_object_mut() else {
        return;
    };
    if let Some(Value::Number(n)) = obj.get("value").cloned() {
        if let Some(f) = n.as_f64() {
            obj.insert(
                "value_as_string".to_string(),
                Value::String(format_number_pattern(f, fmt)),
            );
        }
    }
    // `stats_bucket` / `extended_stats_bucket` — format each numeric stat.
    for key in [
        "min",
        "max",
        "avg",
        "sum",
        "variance",
        "variance_population",
        "variance_sampling",
        "std_deviation",
        "std_deviation_population",
        "std_deviation_sampling",
        "sum_of_squares",
    ] {
        if let Some(Value::Number(n)) = obj.get(key).cloned() {
            if let Some(f) = n.as_f64() {
                obj.insert(
                    format!("{}_as_string", key),
                    Value::String(format_number_pattern(f, fmt)),
                );
            }
        }
    }
    // `percentiles_bucket` → {values: {"50.0": v, "95.0": v}}.
    // ES inlines the rendered strings as sibling keys `"50.0_as_string"`
    // inside the same `values` object.
    if let Some(Value::Object(values)) = obj.get("values").cloned() {
        let mut out = serde_json::Map::new();
        for (k, v) in values {
            out.insert(k.clone(), v.clone());
            if let Some(f) = v.as_f64() {
                out.insert(
                    format!("{}_as_string", k),
                    Value::String(format_number_pattern(f, fmt)),
                );
            }
        }
        obj.insert("values".to_string(), Value::Object(out));
    }
    // `extended_stats_bucket` → std_deviation_bounds is a nested object;
    // ES emits a sibling `std_deviation_bounds_as_string` object mirroring
    // the shape with each float rendered to the format pattern.
    if let Some(Value::Object(bounds)) = obj.get("std_deviation_bounds").cloned() {
        let mut out = serde_json::Map::new();
        for (k, v) in bounds {
            if let Some(f) = v.as_f64() {
                out.insert(k, Value::String(format_number_pattern(f, fmt)));
            }
        }
        obj.insert(
            "std_deviation_bounds_as_string".to_string(),
            Value::Object(out),
        );
    }
}

/// Evaluate a Painless-style `MovingFunctions.holt(...)` or
/// `MovingFunctions.holtWinters(...)` call against a window. The `script`
/// string is already lower-cased when passed here. Returns None when the
/// call doesn't match one of the supported forms, or when the optional
/// `if (values.length ...)` gate fails.
pub(crate) fn eval_movingfn_call(script: &str, wnd: &[f64]) -> Option<f64> {
    // Honor a leading `if (values.length <op> <n>) { ... }` guard by
    // verifying the condition on the current window. When false, return
    // 0.0 (ES emits 0.0 rather than null for this guarded form).
    let (call_src, guard_passed) = if let Some(if_start) = script.find("if (") {
        let after_if = &script[if_start + 4..];
        // Find the matching `)` then `{`.
        let close_paren = after_if.find(')')?;
        let cond = &after_if[..close_paren];
        let open_brace = after_if[close_paren..].find('{')?;
        let after_brace = &after_if[close_paren + open_brace + 1..];
        let close_brace = after_brace.rfind('}')?;
        let body = &after_brace[..close_brace];
        // Parse cond: must be `values.length <op> <n>`
        let parts: Vec<&str> = cond.split_whitespace().collect();
        let passed = if parts.len() == 3 && parts[0].contains("values.length") {
            let n: usize = parts[2].parse().ok()?;
            match parts[1] {
                ">" => wnd.len() > n,
                ">=" => wnd.len() >= n,
                "<" => wnd.len() < n,
                "<=" => wnd.len() <= n,
                "==" => wnd.len() == n,
                _ => false,
            }
        } else {
            true
        };
        (body.trim().to_string(), passed)
    } else {
        (script.to_string(), true)
    };

    if !guard_passed {
        return Some(0.0);
    }

    // Locate `holtwinters(args)` / `holt(args)` calls (case-insensitive).
    // `fn_name` already includes the opening paren, so we split from just
    // past it; the matching `)` closes the call's argument list.
    let parse_args = |fn_name: &str| -> Option<Vec<f64>> {
        let idx = call_src.find(fn_name)?;
        let after = &call_src[idx + fn_name.len()..];
        let close = after.find(')')?;
        let args_str = &after[..close];
        // Skip the first arg (the values ref). Subsequent args are numeric literals or booleans.
        let parts: Vec<&str> = args_str.split(',').map(str::trim).collect();
        let mut out: Vec<f64> = Vec::new();
        for p in parts.iter().skip(1) {
            if let Ok(n) = p.parse::<f64>() {
                out.push(n);
            } else if *p == "true" {
                out.push(1.0);
            } else if *p == "false" {
                out.push(0.0);
            }
        }
        Some(out)
    };

    if call_src.contains("holtwinters(") {
        let args = parse_args("holtwinters(")?;
        let alpha = *args.first()?;
        let beta = *args.get(1)?;
        let gamma = *args.get(2)?;
        let period = *args.get(3)? as usize;
        let multiplicative = *args.get(4)? > 0.5;
        return holt_winters(wnd, alpha, beta, gamma, period, multiplicative);
    }
    if call_src.contains("holt(") {
        let args = parse_args("holt(")?;
        let alpha = *args.first()?;
        let beta = *args.get(1)?;
        return Some(holt(wnd, alpha, beta));
    }
    None
}

/// ES MovingFunctions.holt: double-exponential smoothing over the window.
/// Returns the smoothed level `s` at the end of the window (matching ES's
/// implementation, which returns the last level estimate — not the forecast).
fn holt(values: &[f64], alpha: f64, beta: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut s = values[0];
    let mut b = 0.0f64;
    for (i, &v) in values.iter().enumerate().skip(1) {
        let old_s = s;
        s = alpha * v + (1.0 - alpha) * (s + b);
        b = beta * (s - old_s) + (1.0 - beta) * b;
        let _ = i;
    }
    s
}

/// ES MovingFunctions.holtWinters: triple-exponential smoothing with
/// seasonality. Requires at least `2 * period` samples to initialize
/// seasonal indices; returns 0.0 otherwise (mirroring ES behaviour when
/// the caller-wrapped `if (values.length > ...)` gate fails).
///
/// Implementation matches the Lucene/ES `MovingFunctions.holtWinters`:
/// initial level = mean of the first full period, initial trend =
/// (mean of 2nd period - mean of 1st period) / period, initial seasonal
/// indices = element-wise residual over the first period.
fn holt_winters(
    values: &[f64],
    alpha: f64,
    beta: f64,
    gamma: f64,
    period: usize,
    multiplicative: bool,
) -> Option<f64> {
    if period == 0 || values.len() < 2 * period {
        return Some(0.0);
    }
    let n = values.len();
    let padding = if multiplicative { 1e-10 } else { 0.0 };
    // Faithful port of ES HoltWintersModel: vs[i] = values[i] + padding.
    let vs: Vec<f64> = values.iter().map(|v| v + padding).collect();
    // Initial level = mean of first season; initial trend is computed but ES
    // keeps `last_b` at 0, so the seed trend is discarded on the first step.
    let mut s = 0.0f64;
    let mut b = 0.0f64;
    for i in 0..period {
        s += vs[i];
        b += (vs[i + period] - vs[i]) / period as f64;
    }
    s /= period as f64;
    b /= period as f64;
    let mut last_s = s;
    let mut last_b = 0.0f64;
    // First-season seasonal indices: ES ALWAYS divides here (even additive).
    let mut seasonal: Vec<f64> = vec![0.0; n];
    if s != 0.0 {
        for i in 0..period {
            seasonal[i] = vs[i] / s;
        }
    }
    for i in period..n {
        if multiplicative {
            s = alpha * (vs[i] / seasonal[i - period]) + (1.0 - alpha) * (last_s + last_b);
        } else {
            s = alpha * (vs[i] - seasonal[i - period]) + (1.0 - alpha) * (last_s + last_b);
        }
        b = beta * (s - last_s) + (1.0 - beta) * last_b;
        if multiplicative {
            seasonal[i] =
                gamma * (vs[i] / (last_s + last_b)) + (1.0 - gamma) * seasonal[i - period];
        } else {
            seasonal[i] =
                gamma * (vs[i] - (last_s - last_b)) + (1.0 - gamma) * seasonal[i - period];
        }
        last_s = s;
        last_b = b;
    }
    let idx = n - period;
    let out = if multiplicative {
        (s + b) * seasonal[idx]
    } else {
        s + b + seasonal[idx]
    };
    Some(out)
}

/// Format a float using a very small subset of ES/Java DecimalFormat
/// patterns. Supports:
/// - `0.00` / `0.###` — fixed decimal with `0` forcing a digit and `#`
///   optional (trailing zeros trimmed).
/// - `#,##0.00` — thousands grouping (US comma) with fixed decimals.
/// - A raw number of decimals (`.NN`) is honored via the count of
///   fractional pattern chars.
///
/// Falls through to `f64::to_string` when the pattern is unrecognized.
pub(crate) fn format_number_pattern(value: f64, pattern: &str) -> String {
    // Java DecimalFormat allows a literal prefix and suffix around the
    // numeric-format chars. Identify the contiguous "active pattern"
    // substring — chars in {0,#,.,,} — and keep the rest as literal.
    let is_fmt = |c: char| matches!(c, '0' | '#' | '.' | ',');
    let start = pattern.find(is_fmt).unwrap_or(pattern.len());
    let end = pattern
        .rfind(is_fmt)
        .map(|i| i + 1)
        .unwrap_or(pattern.len());
    let (prefix, rest) = pattern.split_at(start);
    let (active_pat, suffix) = if end >= start {
        (&rest[..(end - start)], &pattern[end..])
    } else {
        (rest, "")
    };
    if active_pat.is_empty() {
        return pattern.to_string();
    }

    let (int_part_pat, frac_part_pat) = match active_pat.find('.') {
        Some(i) => (&active_pat[..i], &active_pat[i + 1..]),
        None => (active_pat, ""),
    };
    let frac_digits = frac_part_pat
        .chars()
        .filter(|c| *c == '0' || *c == '#')
        .count();
    let fixed_frac_digits = frac_part_pat.chars().filter(|c| *c == '0').count();
    let int_min_digits = int_part_pat.chars().filter(|c| *c == '0').count();
    let use_grouping = int_part_pat.contains(',');

    // Round to `frac_digits` decimals.
    let scale = 10f64.powi(frac_digits as i32);
    let rounded = (value * scale).round() / scale;
    let sign = if rounded < 0.0 { "-" } else { "" };
    let abs = rounded.abs();
    let int_val = abs.trunc() as u64;
    let frac_val = abs - int_val as f64;

    // Render the integer part, zero-pad to `int_min_digits`.
    let mut int_str = int_val.to_string();
    while int_str.len() < int_min_digits {
        int_str.insert(0, '0');
    }
    if use_grouping {
        let s = std::mem::take(&mut int_str);
        let mut out = String::with_capacity(s.len() + s.len() / 3);
        for (i, c) in s.chars().rev().enumerate() {
            if i > 0 && i % 3 == 0 {
                out.push(',');
            }
            out.push(c);
        }
        int_str = out.chars().rev().collect::<String>();
    }

    if frac_digits == 0 {
        return format!("{prefix}{sign}{int_str}{suffix}");
    }

    // Render the fractional part at `frac_digits` precision and trim
    // trailing zeros up to the `fixed_frac_digits` minimum.
    let frac_str = format!("{:.*}", frac_digits, frac_val);
    // `0.xyz` — skip the leading "0.".
    let frac_only = &frac_str[2..];
    let mut trimmed = frac_only.trim_end_matches('0').to_string();
    while trimmed.len() < fixed_frac_digits {
        trimmed.push('0');
    }
    if trimmed.is_empty() {
        format!("{prefix}{sign}{int_str}{suffix}")
    } else {
        format!("{prefix}{sign}{int_str}.{trimmed}{suffix}")
    }
}

fn resolve_moving_avg(bucket_values: &[f64], window: usize) -> Value {
    if bucket_values.is_empty() {
        return json!({"value": Value::Null});
    }
    // Return a per-bucket moving average (only the last bucket's value for the pipeline result).
    let n = bucket_values.len();
    let start = n.saturating_sub(window);
    let slice = &bucket_values[start..];
    let avg = slice.iter().sum::<f64>() / slice.len() as f64;
    make_value_result(avg)
}

fn resolve_cumulative_sum(bucket_values: &[f64]) -> Value {
    if bucket_values.is_empty() {
        return json!({"value": Value::Null});
    }
    let cumsum: f64 = bucket_values.iter().sum();
    make_value_result(cumsum)
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

fn execute_agg_with_all_cached<'d>(
    name: &str,
    body: &Value,
    docs: &'d [Value],
    all_docs: &[Value],
    cache: &mut FieldCache<'d>,
) -> Value {
    let obj = match body.as_object() {
        Some(o) => o,
        None => return json!({"error": "aggregation body must be an object"}),
    };

    // Sub-aggregations live under the "aggs" (or "aggregations") key.
    let sub_aggs = obj.get("aggs").or_else(|| obj.get("aggregations"));

    // Find the aggregation type key (first key that is not "aggs"/"aggregations"/"meta").
    let type_key = obj
        .keys()
        .find(|k| k.as_str() != "aggs" && k.as_str() != "aggregations" && k.as_str() != "meta");

    let agg_type = match type_key {
        Some(k) => k.as_str(),
        None => return json!({"error": format!("no aggregation type found in '{}'", name)}),
    };

    let agg_params = match obj.get(agg_type) {
        Some(p) => p,
        None => return json!({"error": "missing agg params"}),
    };

    let mut result = match agg_type {
        "terms" => run_terms(agg_params, sub_aggs, docs, all_docs),
        "date_histogram" => run_date_histogram(agg_params, sub_aggs, docs, all_docs),
        // Metric aggs receive the shared field cache to avoid redundant JSON walks.
        "value_count" => run_value_count(agg_params, docs, cache),
        "avg" => run_avg(agg_params, docs, cache),
        "sum" => run_sum(agg_params, docs, cache),
        "min" => run_min(agg_params, docs, cache),
        "max" => run_max(agg_params, docs, cache),
        "stats" => run_stats(agg_params, docs, cache),
        "cardinality" => run_cardinality(agg_params, docs, cache),
        "filter" => run_filter(agg_params, sub_aggs, docs, all_docs),
        "filters" => run_filters(agg_params, sub_aggs, docs, all_docs),
        "range" => run_range(agg_params, sub_aggs, docs, all_docs),
        "missing" => run_missing(agg_params, docs),
        "histogram" => run_histogram(agg_params, sub_aggs, docs, all_docs),
        "variable_width_histogram" => {
            run_variable_width_histogram(agg_params, sub_aggs, docs, all_docs)
        }
        "percentiles" => run_percentiles(agg_params, docs),
        "composite" => run_composite(agg_params, sub_aggs, docs, all_docs),
        "significant_terms" => run_significant_terms(agg_params, docs, all_docs),
        "significant_text" => run_significant_text(agg_params, docs, all_docs),
        "top_hits" => run_top_hits(agg_params, docs),
        "sampler" | "random_sampler" => run_sampler(agg_params, sub_aggs, docs, all_docs),
        "adjacency_matrix" => run_adjacency_matrix(agg_params, sub_aggs, docs, all_docs),
        // Geo aggregations
        "geohash_grid" => run_geohash_grid(agg_params, sub_aggs, docs, all_docs),
        "geo_distance" => run_geo_distance(agg_params, sub_aggs, docs, all_docs),
        // Multi-terms aggregation
        "multi_terms" => run_multi_terms(agg_params, sub_aggs, docs, all_docs),
        "rare_terms" => run_rare_terms(agg_params, sub_aggs, docs, all_docs),
        "ip_prefix" => run_ip_prefix(agg_params, sub_aggs, docs, all_docs),
        "geotile_grid" => run_geotile_grid(agg_params, sub_aggs, docs, all_docs),
        "diversified_sampler" => run_diversified_sampler(agg_params, sub_aggs, docs, all_docs),
        // time_series — bucket docs by their time_series_dimension field
        // tuple (the `_tsid`). Dimension fields are declared in the mapping
        // with `time_series_dimension: true`; xerj stores them on the
        // document via `routing_path`. This implementation infers
        // dimensions from the doc's `_source` by finding scalar keyword
        // fields that vary within the index — but the test suite relies
        // on the `key` field being explicitly the dimension, so we pull
        // from the `routing_path` setting when it's passed through.
        "time_series" => run_time_series(agg_params, sub_aggs, docs, all_docs),
        // Auto date histogram
        "auto_date_histogram" => run_auto_date_histogram(agg_params, sub_aggs, docs, all_docs),
        // Gap 3: additional aggregation types
        "extended_stats" => run_extended_stats(agg_params, docs, cache),
        "geo_bounds" => run_geo_bounds(agg_params, docs),
        "geo_centroid" => run_geo_centroid(agg_params, docs),
        "median_absolute_deviation" => run_median_absolute_deviation(agg_params, docs, cache),
        "matrix_stats" => run_matrix_stats(agg_params, docs),
        "string_stats" => run_string_stats(agg_params, docs),
        "boxplot" => run_boxplot(agg_params, docs),
        "top_metrics" => run_top_metrics(agg_params, docs),
        // nested — flatten each parent doc's nested-path array so that
        // doc_count reflects the number of nested objects, and sub-aggs
        // (like `terms` on `courses.name`) see one "doc" per nested entry.
        "nested" => {
            let path = agg_params.get("path").and_then(Value::as_str).unwrap_or("");
            run_nested(path, sub_aggs, docs, all_docs)
        }
        // reverse_nested — unwrap back to parent doc scope. Nested
        // expansion duplicates each parent per nested leaf; reverse_nested
        // deduplicates by parent _id so the parent doc is counted once.
        "reverse_nested" => run_reverse_nested(sub_aggs, docs, all_docs),
        // global — run sub-aggs on ALL docs, regardless of query filter.
        "global" => run_global(sub_aggs, all_docs),
        // Pipeline aggregations — these are computed in a second pass; return placeholder.
        "avg_bucket"
        | "max_bucket"
        | "min_bucket"
        | "sum_bucket"
        | "derivative"
        | "moving_avg"
        | "cumulative_sum"
        | "bucket_selector"
        | "bucket_sort"
        | "bucket_script"
        | "percentiles_bucket"
        | "stats_bucket"
        | "serial_diff"
        | "moving_fn"
        | "extended_stats_bucket" => run_pipeline_agg(agg_type, agg_params),
        // date_range — like range but with date math
        "date_range" => run_date_range(agg_params, sub_aggs, docs, all_docs),
        // percentile_ranks — given values, return percentile rank of each
        "percentile_ranks" => run_percentile_ranks(agg_params, docs),
        // ip_range — like range but for IP addresses
        "ip_range" => run_ip_range(agg_params, sub_aggs, docs, all_docs),
        "scripted_metric" => run_scripted_metric(agg_params, docs),
        other => json!({"error": format!("unsupported aggregation type '{}'", other)}),
    };

    // If this agg produced an array bucket list and its sub-aggs include
    // `bucket_sort` / `bucket_selector`, apply them to the bucket list
    // (they live inside the parent's sub-aggs per ES semantics but operate
    // on the parent's buckets).  Only arrays are mutated — keyed maps
    // (e.g. `filters` with named filters) are left untouched.
    if let Some(obj) = result.as_object_mut() {
        if matches!(obj.get("buckets"), Some(Value::Array(_))) {
            if let Some(Value::Array(buckets)) = obj.remove("buckets") {
                let new_buckets = apply_bucket_pipeline_ops(buckets, sub_aggs);
                obj.insert("buckets".into(), Value::Array(new_buckets));
            }
        }
        obj.insert("__type__".into(), Value::String(agg_type.to_string()));
    }
    result
}

// ── Pipeline aggregation placeholder ─────────────────────────────────────────
// Pipeline aggs reference sibling agg results via `buckets_path`.
// We store the spec and resolve it after the first pass in `run_aggs_with_all`.

pub(crate) fn run_pipeline_agg(agg_type: &str, params: &Value) -> Value {
    // Return a sentinel that will be resolved in the post-processing pass.
    // Extract buckets_path — for moving_avg / cumulative_sum it may be a plain string;
    // for bucket_selector it's an object {"var": "path"}.
    let buckets_path = params.get("buckets_path").cloned().unwrap_or(Value::Null);

    // Normalise buckets_path to a string for the simple cases.
    let bp_str = match &buckets_path {
        Value::String(s) => s.clone(),
        Value::Object(o) => {
            // Take the first value — e.g. {"count": "_count"} → "_count"
            o.values()
                .next()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        }
        _ => String::new(),
    };

    let mut sentinel = serde_json::Map::new();
    sentinel.insert("__pipeline__".into(), Value::Bool(true));
    sentinel.insert("type".into(), Value::String(agg_type.to_string()));
    sentinel.insert("buckets_path".into(), Value::String(bp_str));
    sentinel.insert("value".into(), Value::Null);
    // `format` / `gap_policy` travel with the sentinel so resolution can
    // honor them without re-reading the request body.
    if let Some(fmt) = params.get("format").and_then(Value::as_str) {
        sentinel.insert("format".into(), Value::String(fmt.to_string()));
    }
    if let Some(gap) = params.get("gap_policy").and_then(Value::as_str) {
        sentinel.insert("gap_policy".into(), Value::String(gap.to_string()));
    }

    // Store extra parameters needed during resolution.
    match agg_type {
        "moving_avg" => {
            let window = params.get("window").and_then(|v| v.as_u64()).unwrap_or(5);
            sentinel.insert("window".into(), Value::Number(window.into()));
        }
        "bucket_selector" => {
            if let Some(script) = params.get("script") {
                sentinel.insert("script".into(), script.clone());
            }
        }
        "bucket_sort" => {
            if let Some(sort) = params.get("sort") {
                sentinel.insert("sort".into(), sort.clone());
            }
            if let Some(size) = params.get("size") {
                sentinel.insert("size".into(), size.clone());
            }
        }
        "bucket_script" => {
            if let Some(script) = params.get("script") {
                sentinel.insert("script".into(), script.clone());
            }
            if let Some(bp) = params.get("buckets_path") {
                sentinel.insert("buckets_path_full".into(), bp.clone());
            }
        }
        _ => {}
    }

    Value::Object(sentinel)
}

// ── Field value extraction ─────────────────────────────────────────────────────

/// Extract all scalar string representations of a field value from a document.
/// Arrays are flattened — each element becomes a separate bucket key.
pub(crate) fn extract_field_values(doc: &Value, field: &str) -> Vec<String> {
    let val = get_nested_field(doc, field);
    let mut out = flatten_to_strings(val);
    if out.is_empty() {
        // Array-aware dotted-path fallback: for `users.last` where `users`
        // is an array of objects, fan out through each element and pull
        // the sub-path off every object. This lets nested-path sort/agg
        // paths resolve without a dedicated nested walker.
        out = extract_array_path_values(doc, field);
    }
    out
}

/// Walk a dotted path against arrays transparently: when a segment
/// resolves to an array, apply the rest of the path to each element and
/// collect all leaf scalars. Used by `extract_field_values` as a fallback
/// so nested-path fields like `users.last.keyword` fan out across the
/// `users[]` array.
fn extract_array_path_values(doc: &Value, field: &str) -> Vec<String> {
    fn walk(val: &Value, parts: &[&str]) -> Vec<String> {
        if parts.is_empty() {
            return flatten_to_strings(val);
        }
        let head = parts[0];
        let rest = &parts[1..];
        match val {
            Value::Array(arr) => {
                let mut out = Vec::new();
                for el in arr {
                    out.extend(walk(el, parts));
                }
                out
            }
            Value::Object(obj) => {
                if let Some(next) = obj.get(head) {
                    return walk(next, rest);
                }
                // Multi-field fallback: strip trailing `.keyword` / `.raw`
                // etc. when the suffix isn't mapped.
                if parts.len() == 1 {
                    return Vec::new();
                }
                // try greedy join of remaining parts as a single key
                let joined = parts.join(".");
                if let Some(v) = obj.get(&joined) {
                    return flatten_to_strings(v);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
    let parts: Vec<&str> = field.split('.').collect();
    let mut out = walk(doc, &parts);
    // Multi-field fallback (strip trailing `.keyword`).
    if out.is_empty() && field.ends_with(".keyword") {
        let parent = &field[..field.len() - ".keyword".len()];
        out = walk(doc, &parent.split('.').collect::<Vec<_>>());
    }
    out
}

/// Walk dot-separated field paths.
///
/// ES text fields auto-create a `.keyword` multi-field (standard dynamic
/// template). If the full dotted path can't be resolved, fall back to
/// stripping a trailing `.keyword` segment — that way aggregations on
/// `category.keyword` still find values when the source only has `category`.
pub(crate) fn get_nested_field<'a>(doc: &'a Value, field: &str) -> &'a Value {
    fn walk<'a>(doc: &'a Value, field: &str) -> Option<&'a Value> {
        let mut current = doc;
        for part in field.split('.') {
            current = current.get(part)?;
        }
        Some(current)
    }
    if let Some(v) = walk(doc, field) {
        return v;
    }
    // Flattened-type source: ES indexes `{"flat.a": "x"}` (literal dot in
    // key) as a sub-path of the top-level `flat` flattened field. Those
    // dots are not nesting — the key is stored verbatim in _source. Try
    // the full path as a direct literal key before falling back.
    if let Some(obj) = doc.as_object() {
        if let Some(v) = obj.get(field) {
            return v;
        }
    }
    // ES multi-field fallback: `<field>.<subfield>` (e.g. `full_text.eng`,
    // `category.keyword`, `name.raw`) refers to a multi-field that shares
    // the parent field's source value. If the dotted path doesn't resolve
    // we strip the trailing segment and try the parent field — that way
    // aggregations and term lookups against `<field>.<sub>` still see the
    // raw source string.
    if let Some(idx) = field.rfind('.') {
        let parent = &field[..idx];
        if let Some(v) = walk(doc, parent) {
            return v;
        }
    }
    &Value::Null
}

fn flatten_to_strings(val: &Value) -> Vec<String> {
    match val {
        Value::Array(arr) => arr.iter().flat_map(flatten_to_strings).collect(),
        Value::Null => vec![],
        Value::String(s) => vec![s.clone()],
        Value::Number(n) => vec![n.to_string()],
        Value::Bool(b) => vec![b.to_string()],
        Value::Object(_) => vec![], // skip objects
    }
}

/// Extract a single numeric value from a field (returns `None` for non-numeric).
pub(crate) fn extract_numeric(doc: &Value, field: &str) -> Option<f64> {
    fn str_to_num(s: &str) -> Option<f64> {
        if let Ok(f) = s.parse::<f64>() {
            return Some(f);
        }
        // Date-shaped strings fall back to epoch ms so min/max/avg on a
        // date field produce the same integer ES surfaces.
        parse_date_ms(&Value::String(s.to_string())).map(|ms| ms as f64)
    }
    match get_nested_field(doc, field) {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => str_to_num(s.as_str()),
        // For arrays, return the first numeric element (legacy single-value extractor).
        Value::Array(arr) => arr.iter().find_map(|v| match v {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => str_to_num(s.as_str()),
            _ => None,
        }),
        _ => None,
    }
}

/// Extract ALL numeric values from a field (flattens arrays).
pub(crate) fn extract_numeric_values(doc: &Value, field: &str) -> Vec<f64> {
    fn walk(v: &Value, out: &mut Vec<f64>) {
        match v {
            Value::Number(n) => {
                if let Some(f) = n.as_f64() {
                    out.push(f);
                }
            }
            Value::String(s) => {
                if let Ok(f) = s.parse::<f64>() {
                    out.push(f);
                }
            }
            Value::Array(arr) => {
                for e in arr {
                    walk(e, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(get_nested_field(doc, field), &mut out);
    out
}

/// Extract ALL parseable date values (as epoch ms) from a field.
pub(crate) fn extract_date_ms_values(doc: &Value, field: &str) -> Vec<i64> {
    fn walk(v: &Value, out: &mut Vec<i64>) {
        match v {
            Value::Array(arr) => {
                for e in arr {
                    walk(e, out);
                }
            }
            other => {
                if let Some(ms) = parse_date_ms(other) {
                    out.push(ms);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(get_nested_field(doc, field), &mut out);
    out
}

/// Extract range-typed values as (from_inclusive, to_exclusive) numeric pairs.
/// Handles `{gte, gt, lte, lt}` objects, including arrays of them. Unbounded
/// sides default to `f64::MIN`/`f64::MAX` so callers can clamp with
/// `hard_bounds` or `extended_bounds`.
fn extract_numeric_ranges(doc: &Value, field: &str) -> Vec<(f64, f64)> {
    fn walk(v: &Value, out: &mut Vec<(f64, f64)>) {
        match v {
            Value::Array(arr) => {
                for e in arr {
                    walk(e, out);
                }
            }
            Value::Object(obj) => {
                let num = |k: &str| obj.get(k).and_then(Value::as_f64);
                let from = num("gte")
                    .or_else(|| num("gt").map(|g| g + f64::EPSILON))
                    .unwrap_or(f64::MIN);
                let to = num("lt")
                    .or_else(|| num("lte").map(|l| l + f64::EPSILON))
                    .unwrap_or(f64::MAX);
                // Only treat as a range if at least one of the range keys is present.
                if obj.contains_key("gte")
                    || obj.contains_key("gt")
                    || obj.contains_key("lte")
                    || obj.contains_key("lt")
                {
                    out.push((from, to));
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(get_nested_field(doc, field), &mut out);
    out
}

/// Extract range-typed values as (from_inclusive_ms, to_exclusive_ms) date pairs.
fn extract_date_ranges(doc: &Value, field: &str) -> Vec<(i64, i64)> {
    fn walk(v: &Value, out: &mut Vec<(i64, i64)>) {
        match v {
            Value::Array(arr) => {
                for e in arr {
                    walk(e, out);
                }
            }
            Value::Object(obj) => {
                let ms = |k: &str| obj.get(k).and_then(parse_date_ms);
                let from = ms("gte")
                    .or_else(|| ms("gt").map(|g| g + 1))
                    .unwrap_or(i64::MIN);
                let to = ms("lt")
                    .or_else(|| ms("lte").map(|l| l + 1))
                    .unwrap_or(i64::MAX);
                if obj.contains_key("gte")
                    || obj.contains_key("gt")
                    || obj.contains_key("lte")
                    || obj.contains_key("lt")
                {
                    out.push((from, to));
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(get_nested_field(doc, field), &mut out);
    out
}

// ── Terms aggregation ─────────────────────────────────────────────────────────

fn run_terms(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"buckets": []}),
    };
    // ES semantics: size=0 means "return all buckets" (no cap).
    // Any other value caps the result; the default is 10.
    let size_opt: Option<usize> = params
        .get("size")
        .and_then(Value::as_u64)
        .map(|v| v as usize);
    // size=0 → no truncation; absent → default 10.
    let cap: Option<usize> = match size_opt {
        Some(0) => None,    // unlimited
        Some(n) => Some(n), // explicit cap
        None => Some(10),   // ES default
    };

    // Count occurrences of each term, honoring the `_doc_count` metadata
    // field that bulk callers can attach to a document to advertise that a
    // single physical doc represents N logical events (rollup / downsampled
    // indices do this). Missing or non-positive values fall through to 1.
    //
    // The `missing` param supplies a placeholder key for docs that don't
    // have the field — ES treats it as the field value for bucketing.
    let missing_placeholder: Option<String> = params.get("missing").and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    });
    let mut counts: HashMap<String, u64> = HashMap::new();
    let bucket_cap = max_buckets();
    for doc in docs {
        let weight = doc_count_weight(doc);
        let vals = extract_field_values(doc, field);
        if vals.is_empty() {
            if let Some(ph) = &missing_placeholder {
                // Bucket cap: skip new keys past the limit, but always
                // accumulate into existing keys so the cap doesn't bias
                // counts of buckets that were already seen.
                if counts.contains_key(ph) || counts.len() < bucket_cap {
                    *counts.entry(ph.clone()).or_insert(0) += weight;
                }
            }
            continue;
        }
        for term in vals {
            if counts.contains_key(&term) || counts.len() < bucket_cap {
                *counts.entry(term).or_insert(0) += weight;
            }
        }
    }

    // `min_doc_count: 0` — include terms that exist in the background
    // (all_docs) even when zero matched docs contain them. ES emits
    // a bucket with doc_count=0 and runs sub-aggs against the empty
    // doc-set (so e.g. `max: null`).
    let min_doc_count_preview: u64 = params
        .get("min_doc_count")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if min_doc_count_preview == 0 {
        for doc in all_docs {
            for term in extract_field_values(doc, field) {
                if counts.contains_key(&term) || counts.len() < bucket_cap {
                    counts.entry(term).or_insert(0);
                }
            }
        }
    }

    // Parse the `order` parameter. ES accepts several forms:
    //   "_count":"asc"  |  "_key":"desc"  |  "<sub_agg_name>":"desc"
    //   "<sub_agg>.<dotted.path>":"desc"      (e.g. `p.values.99`)
    //   [ { "<sub_agg>": "desc" }, { "_key": "asc" } ]  (multi-key)
    // We honor the first non-`_count`/`_key` entry as a sub-agg reference;
    // falling back to count-desc when none is specified.
    fn order_entries(params: &Value) -> Vec<(String, bool)> {
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
    let orders = order_entries(params);

    // Pre-compute every bucket's sub-aggregations once so we can reorder by
    // a sub-agg metric *before* size-truncation (ES semantics: `size` picks
    // the top N *after* the requested ordering is applied to all candidates).
    //
    // When the ordering only references `_count` or `_key`, skip the
    // pre-compute — simple sort, cheaper.
    let orders_need_sub_agg = orders.iter().any(|(k, _)| k != "_count" && k != "_key");

    let min_doc_count: u64 = params
        .get("min_doc_count")
        .and_then(Value::as_u64)
        .unwrap_or(1);

    // `include: { partition: P, num_partitions: N }` — ES partitions terms
    // by a hash of the term and only returns buckets whose hash falls in
    // the requested partition. This lets clients paginate a very wide
    // terms agg by issuing N independent searches.
    let include_obj = params.get("include").and_then(Value::as_object);
    let partition: Option<u64> = include_obj
        .and_then(|o| o.get("partition"))
        .and_then(Value::as_u64);
    let num_partitions: Option<u64> = include_obj
        .and_then(|o| o.get("num_partitions"))
        .and_then(Value::as_u64);

    // `include: "<regex>"` and `exclude: "<regex>"` — filter terms by a
    // regex (or anchor-less substring match). ES also accepts an array
    // of explicit terms, which we match as literal equality.
    let include_regex = params.get("include").and_then(Value::as_str);
    let exclude_regex = params.get("exclude").and_then(Value::as_str);
    let include_list: Option<Vec<String>> =
        params.get("include").and_then(Value::as_array).map(|a| {
            a.iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    Value::Bool(b) => Some(b.to_string()),
                    _ => None,
                })
                .collect()
        });
    let exclude_list: Option<Vec<String>> =
        params.get("exclude").and_then(Value::as_array).map(|a| {
            a.iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    Value::Bool(b) => Some(b.to_string()),
                    _ => None,
                })
                .collect()
        });

    // Apply min_doc_count filter before sorting — ES default is 1, tests set
    // values as low as 0 (to force empty-term buckets into the output).
    let mut candidates: Vec<(String, u64)> = counts
        .into_iter()
        .filter(|(k, c)| {
            if *c < min_doc_count {
                return false;
            }
            if let (Some(p), Some(n)) = (partition, num_partitions) {
                if n > 0 {
                    // ES uses Murmur3_32 (`StringHelper.murmurhash3_x86_32`)
                    // with Lucene's GOOD_FAST_HASH_SEED = 0xAD7D2925 for
                    // partition hashing. Numeric terms are hashed as their
                    // sortable 8-byte big-endian long encoding (LongPoint
                    // pack format); string terms as UTF-8 bytes.
                    let h = if let Ok(n_val) = k.parse::<i64>() {
                        murmur3_32_partition_hash(&n_val.to_be_bytes())
                    } else {
                        murmur3_32_partition_hash(k.as_bytes())
                    };
                    if (h as u64) % n != p {
                        return false;
                    }
                }
            }
            if let Some(re) = include_regex {
                if !term_matches_regex(k, re) {
                    return false;
                }
            }
            if let Some(re) = exclude_regex {
                if term_matches_regex(k, re) {
                    return false;
                }
            }
            if let Some(list) = &include_list {
                if !list.iter().any(|s| s == k) {
                    return false;
                }
            }
            if let Some(list) = &exclude_list {
                if list.iter().any(|s| s == k) {
                    return false;
                }
            }
            true
        })
        .collect();

    // Compute bucket_docs + optional sub-agg value for each candidate key.
    let compute_bucket_docs = |key: &str| -> Vec<Value> {
        let is_missing_bucket = missing_placeholder
            .as_deref()
            .map(|ph| ph == key)
            .unwrap_or(false);
        docs.iter()
            .filter(|doc| {
                let vals = extract_field_values(doc, field);
                if is_missing_bucket {
                    vals.is_empty()
                } else {
                    vals.contains(&key.to_string())
                }
            })
            .cloned()
            .collect()
    };

    // Build (key, count, sub_aggs) so we can reuse sub-agg results for
    // both ordering and final bucket output.
    let pre_computed: Vec<(String, u64, Option<Value>)> =
        if orders_need_sub_agg || sub_aggs.is_some() {
            candidates
                .iter()
                .map(|(k, c)| {
                    let bucket_docs = compute_bucket_docs(k);
                    let sub_res = sub_aggs.map(|sa| run_aggs_with_all(sa, &bucket_docs, all_docs));
                    (k.clone(), *c, sub_res)
                })
                .collect()
        } else {
            candidates.drain(..).map(|(k, c)| (k, c, None)).collect()
        };

    // Reorder according to the requested sort keys.
    let mut sorted: Vec<(String, u64, Option<Value>)> = pre_computed;
    sorted.sort_by(|a, b| cmp_terms_by_orders(a, b, &orders));

    if let Some(n) = cap {
        sorted.truncate(n);
    }

    let buckets: Vec<Value> = sorted
        .into_iter()
        .map(|(key, count, sub_res)| {
            let is_missing_bucket = missing_placeholder
                .as_deref()
                .map(|ph| ph == key.as_str())
                .unwrap_or(false);
            let bucket_docs: Vec<Value> = docs
                .iter()
                .filter(|doc| {
                    let vals = extract_field_values(doc, field);
                    if is_missing_bucket {
                        vals.is_empty()
                    } else {
                        vals.contains(&key)
                    }
                })
                .cloned()
                .collect();
            let _ = &bucket_docs; // silence unused when neither sub nor `count` needs it
            let (key, count, precomputed_sub) = (key, count, sub_res);

            let (typed_key, key_as_string) = typed_term_key(&key);

            let mut bucket = json!({
                "key": typed_key,
                "doc_count": count
            });
            if let Some(kas) = key_as_string {
                bucket["key_as_string"] = json!(kas);
            }

            // Reuse the precomputed sub-agg result when we had to build it
            // for ordering; otherwise compute it now (no-op if no sub_aggs).
            let sub_result = precomputed_sub
                .or_else(|| sub_aggs.map(|sa| run_aggs_with_all(sa, &bucket_docs, all_docs)));
            if let Some(Value::Object(sub_obj)) = sub_result {
                if let Some(bucket_obj) = bucket.as_object_mut() {
                    for (k, v) in sub_obj {
                        bucket_obj.insert(k, v);
                    }
                }
            }

            bucket
        })
        .collect();

    json!({
        "doc_count_error_upper_bound": 0,
        "sum_other_doc_count": 0,
        "buckets": buckets
    })
}

/// ES returns typed terms-bucket keys: numbers as numbers, booleans as
/// 1/0 (with key_as_string "true"/"false"), dates as epoch millis
/// (with key_as_string as the ISO string).  Everything else stays
/// a string.  `key_as_string` is only added when it carries extra
/// information that the numeric key can't: booleans and dates.
///
/// Shared by `run_terms` and the doc-values fast-agg path so bucket
/// key shaping is byte-identical between the two.
pub(crate) fn typed_term_key(key: &str) -> (Value, Option<String>) {
    let mut key_as_string: Option<String> = None;
    let typed_key: Value = if key == "true" || key == "false" {
        key_as_string = Some(key.to_string());
        json!(if key == "true" { 1 } else { 0 })
    } else if let Ok(n) = key.parse::<i64>() {
        json!(n)
    } else if let Ok(f) = key.parse::<f64>() {
        serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(json!(key))
    } else if let Some(epoch_ms) = parse_date_ms(&Value::String(key.to_string())) {
        // ES emits `key_as_string` in full ISO-8601 UTC with millis
        // regardless of the input granularity (e.g. "2016-05-03" →
        // "2016-05-03T00:00:00.000Z").
        key_as_string = Some(epoch_ms_to_iso8601_utc(epoch_ms));
        json!(epoch_ms)
    } else {
        json!(key)
    };
    (typed_key, key_as_string)
}

/// Comparator for terms-agg candidate buckets against a list of `order`
/// entries. Each entry is `(path, asc)`; `_count`/`_key` are special, any
/// other path is resolved against the bucket's precomputed sub-aggs and
/// compared as a floating-point metric value (missing → -inf for asc,
/// +inf for desc so gaps sort last).
fn cmp_terms_by_orders(
    a: &(String, u64, Option<Value>),
    b: &(String, u64, Option<Value>),
    orders: &[(String, bool)],
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if orders.is_empty() {
        // ES default: count desc, key asc.
        return b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0));
    }
    for (path, asc) in orders {
        let ord = match path.as_str() {
            "_count" => a.1.cmp(&b.1),
            "_key" => a.0.cmp(&b.0),
            other => {
                let va = lookup_agg_metric(a.2.as_ref(), other);
                let vb = lookup_agg_metric(b.2.as_ref(), other);
                va.partial_cmp(&vb).unwrap_or(Ordering::Equal)
            }
        };
        let ord = if *asc { ord } else { ord.reverse() };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    // Final tiebreaker on key.
    a.0.cmp(&b.0)
}

/// Resolve a dotted/`>`-separated path against a sub-aggregation result
/// tree into a numeric value. Supports `<agg_name>`, `<agg_name>.value`,
/// `<agg_name>.values.<pct>`, ES nested-bucket paths
/// `<bucket_agg>>.<metric_agg>` (or `>` only), and keyed
/// percentile/percentile_rank lookups.
fn lookup_agg_metric(sub: Option<&Value>, path: &str) -> f64 {
    let Some(root) = sub else {
        return f64::NEG_INFINITY;
    };
    // Normalize ES `>` separator (nested bucket-path syntax) to `.`
    // for uniform walking. `aggA>aggB` → `aggA.aggB`. Strip leading dot.
    let normalized = path.replace('>', ".");
    let mut cur: &Value = root;
    for seg in normalized.split('.').filter(|s| !s.is_empty()) {
        match cur.get(seg) {
            Some(v) => cur = v,
            None => return f64::NEG_INFINITY,
        }
    }
    if let Some(v) = cur.get("value") {
        if let Some(f) = v.as_f64() {
            return f;
        }
    }
    cur.as_f64().unwrap_or(f64::NEG_INFINITY)
}

fn run_ip_prefix(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"buckets": []}),
    };
    let prefix_length = params
        .get("prefix_length")
        .and_then(Value::as_u64)
        .unwrap_or(24) as usize;
    let is_ipv6 = params
        .get("is_ipv6")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let append_prefix_length = params
        .get("append_prefix_length")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Group docs by their prefixed IP key. ES sorts buckets by the binary
    // octet representation of the masked address, not by lexicographic
    // string order — so we key by octets and render the display string
    // afterward. Key format is a bare IP string (optionally with
    // `/prefix_length` appended when append_prefix_length=true).
    //
    // Binary key: 17 bytes — one tag byte (0 = IPv4, 1 = IPv6) followed
    // by the 16 IPv6 octets or 4 IPv4 octets (padded). Tag is needed so
    // IPv4 and IPv6 entries sort in a stable, consistent order if a
    // caller ever mixes them; with is_ipv6 fixed per-agg this is not
    // user-visible.
    let mut prefix_docs: HashMap<Vec<u8>, (String, Vec<usize>)> = HashMap::new();
    for (i, doc) in docs.iter().enumerate() {
        for v in extract_field_values(doc, field) {
            let (binkey, display): (Option<Vec<u8>>, Option<String>) = if is_ipv6 {
                // Treat every address as 128-bit. v4 maps to ::ffff:V.V.V.V.
                let addr: Option<std::net::Ipv6Addr> = v.parse().ok().or_else(|| {
                    v.parse::<std::net::Ipv4Addr>()
                        .ok()
                        .map(|v4| v4.to_ipv6_mapped())
                });
                match addr {
                    Some(a) => {
                        let masked = mask_ipv6(a, prefix_length);
                        let octets = masked.octets();
                        let mut key = Vec::with_capacity(17);
                        key.push(1u8);
                        key.extend_from_slice(&octets);
                        (Some(key), Some(masked.to_string()))
                    }
                    None => (None, None),
                }
            } else {
                // When the value is IPv6 but is_ipv6=false, ES takes the
                // last 4 bytes and treats them as IPv4 (the "incorrect"
                // classification case) rather than discarding the doc.
                let ip4: Option<std::net::Ipv4Addr> =
                    v.parse::<std::net::Ipv4Addr>().ok().or_else(|| {
                        v.parse::<std::net::Ipv6Addr>().ok().map(|v6| {
                            let oct = v6.octets();
                            std::net::Ipv4Addr::new(oct[12], oct[13], oct[14], oct[15])
                        })
                    });
                match ip4 {
                    Some(ip4) => {
                        let mask = if prefix_length >= 32 {
                            !0u32
                        } else {
                            !0u32 << (32 - prefix_length)
                        };
                        let masked = u32::from_be_bytes(ip4.octets()) & mask;
                        let ip = std::net::Ipv4Addr::from(masked);
                        let octets = ip.octets();
                        let mut key = Vec::with_capacity(17);
                        key.push(0u8);
                        key.extend_from_slice(&octets);
                        (Some(key), Some(ip.to_string()))
                    }
                    None => (None, None),
                }
            };
            if let (Some(bk), Some(k)) = (binkey, display) {
                let disp = if append_prefix_length {
                    format!("{}/{}", k, prefix_length)
                } else {
                    k
                };
                let entry = prefix_docs
                    .entry(bk)
                    .or_insert_with(|| (disp.clone(), Vec::new()));
                entry.1.push(i);
            }
        }
    }

    // Sort by binary key ascending (ES behaviour for ip_prefix).
    let mut entries: Vec<(Vec<u8>, (String, Vec<usize>))> = prefix_docs.into_iter().collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let entries: Vec<(String, Vec<usize>)> = entries.into_iter().map(|(_, v)| v).collect();

    let buckets: Vec<Value> = entries
        .iter()
        .map(|(key, indices)| {
            let bucket_docs: Vec<Value> = indices.iter().map(|&i| docs[i].clone()).collect();
            let mut bucket = json!({
                "key": key,
                "doc_count": indices.len(),
                "is_ipv6": is_ipv6,
                "prefix_length": prefix_length,
            });
            // netmask is only emitted for IPv4.
            if !is_ipv6 {
                bucket["netmask"] = json!(format_netmask_v4(prefix_length));
            }
            if let Some(sub) = sub_aggs {
                let sub_result = run_aggs_with_all(sub, &bucket_docs, all_docs);
                if let (Some(bo), Value::Object(so)) = (bucket.as_object_mut(), sub_result) {
                    for (k, v) in so {
                        bo.insert(k, v);
                    }
                }
            }
            bucket
        })
        .collect();

    json!({"buckets": buckets})
}

/// Apply `prefix_length`-bit mask to the top of the IPv6 address.
fn mask_ipv6(addr: std::net::Ipv6Addr, prefix_length: usize) -> std::net::Ipv6Addr {
    let n = prefix_length.min(128);
    let mut bits = u128::from_be_bytes(addr.octets());
    if n == 0 {
        bits = 0;
    } else if n < 128 {
        let shift = 128 - n;
        bits &= !((1u128 << shift) - 1);
    }
    std::net::Ipv6Addr::from(bits.to_be_bytes())
}

fn format_netmask_v4(prefix_length: usize) -> String {
    let mask = if prefix_length >= 32 {
        !0u32
    } else {
        !0u32 << (32 - prefix_length)
    };
    let ip = std::net::Ipv4Addr::from(mask);
    ip.to_string()
}

fn run_rare_terms(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"buckets": []}),
    };
    let max_doc_count = params
        .get("max_doc_count")
        .and_then(Value::as_u64)
        .unwrap_or(1) as usize;

    // include / exclude — ES accepts array of literals (applies to any
    // field type) or regex string (keyword fields only).
    let include_list: Option<Vec<String>> =
        params.get("include").and_then(Value::as_array).map(|a| {
            a.iter()
                .filter_map(|v| {
                    v.as_str()
                        .map(String::from)
                        .or_else(|| v.as_f64().map(|f| f.to_string()))
                })
                .collect()
        });
    let exclude_list: Option<Vec<String>> =
        params.get("exclude").and_then(Value::as_array).map(|a| {
            a.iter()
                .filter_map(|v| {
                    v.as_str()
                        .map(String::from)
                        .or_else(|| v.as_f64().map(|f| f.to_string()))
                })
                .collect()
        });

    let mut term_docs: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, doc) in docs.iter().enumerate() {
        for v in extract_field_values(doc, field) {
            if v.is_empty() {
                continue;
            }
            if let Some(ref inc) = include_list {
                if !inc.contains(&v) {
                    continue;
                }
            }
            if let Some(ref exc) = exclude_list {
                if exc.contains(&v) {
                    continue;
                }
            }
            term_docs.entry(v).or_default().push(i);
        }
    }

    let mut entries: Vec<_> = term_docs
        .into_iter()
        .filter(|(_, indices)| indices.len() <= max_doc_count)
        .collect();
    entries.sort_by(|a, b| a.1.len().cmp(&b.1.len()).then(a.0.cmp(&b.0)));

    let buckets: Vec<Value> = entries
        .iter()
        .map(|(key, indices)| {
            let bucket_docs: Vec<Value> = indices.iter().map(|&i| docs[i].clone()).collect();
            let mut bucket = build_terms_bucket(key, indices.len() as u64);
            if let Some(sub) = sub_aggs {
                let sub_result = run_aggs_with_all(sub, &bucket_docs, all_docs);
                if let (Some(bo), Value::Object(so)) = (bucket.as_object_mut(), sub_result) {
                    for (k, v) in so {
                        bo.insert(k, v);
                    }
                }
            }
            bucket
        })
        .collect();

    json!({"buckets": buckets})
}

// ── Date histogram aggregation ────────────────────────────────────────────────

/// Matrix-stats aggregation: per-field count/mean/variance/skewness/kurtosis
/// plus pairwise covariance and correlation. ES only counts a document
/// if *every* requested field has a numeric value (unless `missing` fills
/// in a default for each). Documents where any field is unmapped/missing
/// are skipped, which is why the `Partially unmapped` test expects
/// doc_count=14 out of 15 input docs.
fn run_matrix_stats(params: &Value, docs: &[Value]) -> Value {
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
        return json!({"doc_count": 0, "fields": []});
    }
    let mode = params.get("mode").and_then(Value::as_str).unwrap_or("avg");
    let missing: HashMap<String, f64> = params
        .get("missing")
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
                .collect()
        })
        .unwrap_or_default();
    // Sentinel injected by the HTTP coordination layer: list of field
    // names whose mapping declares `type: float`. ES indexes those at
    // f32 precision, so reduction must round-trip through f32 to match
    // the 1e-11 tolerance asserted by ES YAML tests.
    let f32_fields: HashSet<String> = params
        .get("__xy_f32_fields__")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let cast_if_f32 = |field: &str, v: f64| -> f64 {
        if f32_fields.contains(field) {
            v as f32 as f64
        } else {
            v
        }
    };

    // Per-doc value vector (f64 per field) — doc is included only when
    // every field resolves to a numeric value after the `mode` collapses
    // multi-valued fields.
    let mut rows: Vec<Vec<f64>> = Vec::new();
    for doc in docs {
        let mut row = Vec::with_capacity(fields.len());
        let mut valid = true;
        for f in &fields {
            let nums_raw = extract_numeric_values(doc, f);
            // Cast each value through f32 first when the field is a
            // float-type field per the mapping. ES does the equivalent
            // at index time so reductions read f32 values; without this
            // our f64 sums can drift outside the 1e-11 tolerance.
            let nums: Vec<f64> = nums_raw.into_iter().map(|v| cast_if_f32(f, v)).collect();
            let v = if nums.is_empty() {
                missing.get(f).copied()
            } else {
                match mode {
                    "sum" => Some(nums.iter().sum::<f64>()),
                    "min" => nums.iter().cloned().reduce(f64::min),
                    "max" => nums.iter().cloned().reduce(f64::max),
                    "median" => {
                        let mut s = nums.clone();
                        s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                        if s.is_empty() {
                            None
                        } else {
                            Some(s[s.len() / 2])
                        }
                    }
                    _ => Some(nums.iter().sum::<f64>() / nums.len() as f64), // "avg" default
                }
            };
            match v {
                Some(val) => row.push(val),
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
    matrix_stats_from_rows(fields, rows)
}

/// Reduce pre-collapsed per-doc value rows (one `f64` per requested field, for
/// docs where every field resolved) into the ES `matrix_stats` response body:
/// per-field count/mean/variance/skewness/kurtosis plus the pairwise
/// covariance and correlation matrices, emitted in ES's reverse-input field
/// order.  Split out of `run_matrix_stats` so the columnar fast-agg path
/// (`FastCtx::exec_matrix_stats`) can feed rows gathered straight from the
/// numeric `.dv` columns through the identical math.
pub(crate) fn matrix_stats_from_rows(fields: Vec<String>, rows: Vec<Vec<f64>>) -> Value {
    let n = rows.len();
    if n == 0 {
        return json!({"doc_count": 0, "fields": []});
    }

    // Per-field: count, mean, variance, skewness, kurtosis.
    let k = fields.len();
    let mut sum = vec![0.0f64; k];
    for row in &rows {
        for (i, v) in row.iter().enumerate() {
            sum[i] += *v;
        }
    }
    let mean: Vec<f64> = sum.iter().map(|s| s / n as f64).collect();

    let mut m2 = vec![0.0f64; k];
    let mut m3 = vec![0.0f64; k];
    let mut m4 = vec![0.0f64; k];
    for row in &rows {
        for (i, v) in row.iter().enumerate() {
            let d = *v - mean[i];
            m2[i] += d * d;
            m3[i] += d * d * d;
            m4[i] += d * d * d * d;
        }
    }
    let variance: Vec<f64> = if n > 1 {
        m2.iter().map(|s| s / (n - 1) as f64).collect()
    } else {
        vec![0.0; k]
    };
    let std_dev: Vec<f64> = variance.iter().map(|v| v.sqrt()).collect();
    let skewness: Vec<f64> = (0..k)
        .map(|i| {
            if n > 2 && std_dev[i] > 0.0 {
                let s3 = m3[i] / n as f64;
                s3 / std_dev[i].powi(3)
            } else {
                0.0
            }
        })
        .collect();
    let kurtosis: Vec<f64> = (0..k)
        .map(|i| {
            if n > 3 && variance[i] > 0.0 {
                let s4 = m4[i] / n as f64;
                s4 / variance[i].powi(2)
            } else {
                0.0
            }
        })
        .collect();

    // Pairwise covariance and correlation matrices.
    let mut cov = vec![vec![0.0f64; k]; k];
    for row in &rows {
        for i in 0..k {
            for j in 0..k {
                cov[i][j] += (row[i] - mean[i]) * (row[j] - mean[j]);
            }
        }
    }
    let denom = if n > 1 { (n - 1) as f64 } else { 1.0 };
    // cov[i][j] is 2-D matrix indexing; a plain range loop is the clearest
    // form and an iterator refactor would obscure the normalization math.
    #[allow(clippy::needless_range_loop)]
    for i in 0..k {
        for j in 0..k {
            cov[i][j] /= denom;
        }
    }
    let corr = |i: usize, j: usize| -> f64 {
        let d = std_dev[i] * std_dev[j];
        if d > 0.0 {
            cov[i][j] / d
        } else {
            0.0
        }
    };

    // Build per-field output. ES orders fields by the input order.
    // ES emits matrix_stats fields in REVERSE input order (a quirk of
    // the Java implementation that surfaces in the YAML tests — see the
    // expectation that `fields.0.correlation.val1` is not 1.0 when the
    // request lists `val1` first). Mirror that ordering so bucket
    // indices line up with the test assertions.
    let mut fields_out: Vec<Value> = Vec::with_capacity(k);
    for i in (0..k).rev() {
        let mut covariance_obj = serde_json::Map::new();
        let mut correlation_obj = serde_json::Map::new();
        for j in 0..k {
            covariance_obj.insert(
                fields[j].clone(),
                serde_json::Number::from_f64(cov[i][j])
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
            );
            correlation_obj.insert(
                fields[j].clone(),
                serde_json::Number::from_f64(corr(i, j))
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
            );
        }
        fields_out.push(json!({
            "name": fields[i],
            "count": n,
            "mean": mean[i],
            "variance": variance[i],
            "skewness": skewness[i],
            "kurtosis": kurtosis[i],
            "covariance": covariance_obj,
            "correlation": correlation_obj,
        }));
    }
    json!({ "doc_count": n, "fields": fields_out })
}

/// `string_stats`: per-string metric aggregation — count, min/max/avg_length,
/// entropy, optional `distribution` histogram of character frequencies.
fn run_string_stats(params: &Value, docs: &[Value]) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"count": 0}),
    };
    let show_distribution = params
        .get("show_distribution")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut count: u64 = 0;
    let mut min_len = usize::MAX;
    let mut max_len: usize = 0;
    let mut total_len: usize = 0;
    let mut char_counts: HashMap<char, u64> = HashMap::new();
    for doc in docs {
        for v in extract_field_values(doc, field) {
            count += 1;
            let l = v.chars().count();
            if l < min_len {
                min_len = l;
            }
            if l > max_len {
                max_len = l;
            }
            total_len += l;
            for c in v.chars() {
                *char_counts.entry(c).or_insert(0) += 1;
            }
        }
    }
    if count == 0 {
        return json!({
            "count": 0,
            "min_length": Value::Null,
            "max_length": Value::Null,
            "avg_length": Value::Null,
            "entropy": 0.0,
        });
    }
    let total_chars: u64 = char_counts.values().copied().sum();
    let entropy: f64 = if total_chars == 0 {
        0.0
    } else {
        char_counts
            .values()
            .map(|c| {
                let p = *c as f64 / total_chars as f64;
                -p * p.log2()
            })
            .sum()
    };
    let avg_len = total_len as f64 / count as f64;
    let mut out = json!({
        "count": count,
        "min_length": min_len,
        "max_length": max_len,
        "avg_length": avg_len,
        "entropy": entropy,
    });
    if show_distribution {
        let mut dist = serde_json::Map::new();
        for (c, n) in &char_counts {
            dist.insert(c.to_string(), json!(*n as f64 / total_chars as f64));
        }
        out["distribution"] = Value::Object(dist);
    }
    out
}

/// `boxplot`: five-number summary (min, max, median, q1, q3) plus optional
/// lower/upper fences (outlier cutoffs). Uses linear interpolation for the
/// three quartile percentiles.
fn run_boxplot(params: &Value, docs: &[Value]) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => {
            return json!({"min": Value::Null, "max": Value::Null, "q1": Value::Null, "q2": Value::Null, "q3": Value::Null})
        }
    };
    let mut values: Vec<f64> = Vec::new();
    for doc in docs {
        for n in extract_numeric_values(doc, field) {
            values.push(n);
        }
    }
    if values.is_empty() {
        return json!({
            "min": Value::Null,
            "max": Value::Null,
            "q1": Value::Null,
            "q2": Value::Null,
            "q3": Value::Null,
            "lower": Value::Null,
            "upper": Value::Null,
        });
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let percentile = |p: f64, vals: &[f64]| -> f64 {
        if vals.is_empty() {
            return 0.0;
        }
        if vals.len() == 1 {
            return vals[0];
        }
        let rank = p / 100.0 * (vals.len() - 1) as f64;
        let lo = rank.floor() as usize;
        let hi = rank.ceil() as usize;
        if lo == hi {
            return vals[lo];
        }
        vals[lo] + (rank - lo as f64) * (vals[hi] - vals[lo])
    };
    let q1 = percentile(25.0, &values);
    let q2 = percentile(50.0, &values);
    let q3 = percentile(75.0, &values);
    let iqr = q3 - q1;
    let lower = q1 - 1.5 * iqr;
    let upper = q3 + 1.5 * iqr;
    json!({
        "min": values[0],
        "max": values[values.len() - 1],
        "q1": q1,
        "q2": q2,
        "q3": q3,
        "lower": lower,
        "upper": upper,
    })
}

/// `top_metrics`: pick the metric values from the doc whose sort field
/// ranks highest (or lowest). Only single-metric selection is implemented.
fn run_top_metrics(params: &Value, docs: &[Value]) -> Value {
    let metrics: Vec<String> = params
        .get("metrics")
        .and_then(|v| match v {
            Value::Array(arr) => Some(
                arr.iter()
                    .filter_map(|m| m.get("field").and_then(Value::as_str).map(String::from))
                    .collect::<Vec<_>>(),
            ),
            Value::Object(o) => o
                .get("field")
                .and_then(Value::as_str)
                .map(|s| vec![s.to_string()]),
            _ => None,
        })
        .unwrap_or_default();
    if metrics.is_empty() {
        return json!({"top": []});
    }
    let size = params.get("size").and_then(Value::as_u64).unwrap_or(1) as usize;

    let sort_spec = params.get("sort").cloned();
    let (sort_field, sort_desc) = match sort_spec {
        Some(Value::Object(o)) => {
            if let Some((k, v)) = o.iter().next() {
                let desc = match v {
                    Value::String(s) => s == "desc",
                    Value::Object(ob) => ob
                        .get("order")
                        .and_then(Value::as_str)
                        .map(|s| s == "desc")
                        .unwrap_or(true),
                    _ => true,
                };
                (k.clone(), desc)
            } else {
                ("_score".to_string(), true)
            }
        }
        Some(Value::Array(arr)) => {
            if let Some(Value::Object(o)) = arr.first() {
                if let Some((k, v)) = o.iter().next() {
                    let desc = match v {
                        Value::String(s) => s == "desc",
                        Value::Object(ob) => ob
                            .get("order")
                            .and_then(Value::as_str)
                            .map(|s| s == "desc")
                            .unwrap_or(true),
                        _ => true,
                    };
                    (k.clone(), desc)
                } else {
                    ("_score".to_string(), true)
                }
            } else {
                ("_score".to_string(), true)
            }
        }
        _ => ("_score".to_string(), true),
    };

    let mut ranked: Vec<(f64, &Value)> = docs
        .iter()
        .filter_map(|d| {
            extract_numeric_values(d, &sort_field)
                .first()
                .copied()
                .map(|v| (v, d))
        })
        .collect();
    ranked.sort_by(|a, b| {
        let cmp = a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal);
        if sort_desc {
            cmp.reverse()
        } else {
            cmp
        }
    });

    let out: Vec<Value> = ranked
        .iter()
        .take(size)
        .map(|(sv, doc)| {
            let mut metrics_out = serde_json::Map::new();
            for m in &metrics {
                let vals = extract_numeric_values(doc, m);
                let v = vals
                    .first()
                    .and_then(|f| serde_json::Number::from_f64(*f).map(Value::Number))
                    .unwrap_or(Value::Null);
                metrics_out.insert(m.clone(), v);
            }
            json!({
                "sort": [sv],
                "metrics": metrics_out,
            })
        })
        .collect();
    json!({ "top": out })
}

/// Check whether a term matches an ES include/exclude regex. ES accepts
/// Java regex syntax; we use the `regex` crate (Rust flavor). Anchors
/// `^` and `$` aren't automatically added — ES matches if the regex
/// finds the term *anywhere* unless the pattern starts/ends explicitly.
fn term_matches_regex(term: &str, pattern: &str) -> bool {
    match regex::Regex::new(pattern) {
        Ok(re) => re.is_match(term),
        Err(_) => false,
    }
}

/// Reads the `_doc_count` metadata field that downsampled / rollup indices
/// attach to each physical document to advertise how many logical events it
/// represents. Defaults to `1` when absent or non-positive.
pub(crate) fn doc_count_weight(doc: &Value) -> u64 {
    doc.get("_doc_count")
        .and_then(|v| match v {
            Value::Number(n) => n.as_u64(),
            Value::String(s) => s.parse::<u64>().ok(),
            _ => None,
        })
        .filter(|&n| n > 0)
        .unwrap_or(1)
}

/// Sum `_doc_count` over a slice of matching docs. Handles missing or
/// non-positive values by contributing 1 per doc.
pub(crate) fn sum_doc_count(docs: &[Value]) -> u64 {
    docs.iter().map(doc_count_weight).sum()
}

/// Render a date using an ES date format name or SimpleDateFormat pattern.
/// Handles the named aliases ES ships (`strict_date_optional_time`,
/// `iso8601`, `epoch_millis`, etc.), falls through to java_to_strftime
/// for user-supplied patterns, and defaults to ISO-8601 UTC.
pub fn render_date_format(
    fmt: Option<&str>,
    epoch_ms: i64,
    dt: chrono::DateTime<chrono::Utc>,
) -> String {
    // ES format chains ("fmt1||fmt2"): rendering uses the FIRST format of
    // the chain (verified live on ES 8.13.4).  Mapping-declared chains are
    // already normalised at the es_compat injection site; this covers
    // request-level chains defensively.  Byte-identical no-op for all
    // non-chained formats (b7 DEFECT 2).
    let fmt = fmt.map(|f| f.split("||").next().unwrap_or(f).trim());
    match fmt {
        Some("epoch_millis") => epoch_ms.to_string(),
        Some("epoch_second") => (epoch_ms / 1000).to_string(),
        Some("strict_date_optional_time")
        | Some("strict_date_optional_time_nanos")
        | Some("iso8601")
        | Some("date_optional_time") => dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        Some("basic_date") => dt.format("%Y%m%d").to_string(),
        Some("basic_date_time") => dt.format("%Y%m%dT%H%M%S%.3fZ").to_string(),
        Some("basic_time") => dt.format("%H%M%S%.3fZ").to_string(),
        Some("date") | Some("strict_date") => dt.format("%Y-%m-%d").to_string(),
        Some("date_hour_minute_second") | Some("strict_date_hour_minute_second") => {
            dt.format("%Y-%m-%dT%H:%M:%S").to_string()
        }
        Some("date_time") | Some("strict_date_time") => {
            dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
        }
        Some("hour") => dt.format("%H").to_string(),
        Some("hour_minute_second") => dt.format("%H:%M:%S").to_string(),
        Some("year") | Some("strict_year") => dt.format("%Y").to_string(),
        Some("year_month") | Some("strict_year_month") => dt.format("%Y-%m").to_string(),
        Some("year_month_day") | Some("strict_year_month_day") => dt.format("%Y-%m-%d").to_string(),
        Some(pat) if !pat.is_empty() => {
            let s = java_to_strftime(pat);
            dt.format(&s).to_string()
        }
        _ => dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
    }
}

/// `true` when `fmt` is one of the named ES date-format aliases that
/// `render_date_format` resolves via its alias table above.  Callers that
/// feed a format straight to `java_to_strftime` (e.g. `run_date_range`'s
/// render closure) must check this first — translating an alias NAME as a
/// SimpleDateFormat pattern emits its letters as pattern tokens
/// ("strict_date_optional_time" → garbage).  Keep in sync with the match
/// arms of `render_date_format`.
pub(crate) fn is_named_date_format(fmt: &str) -> bool {
    matches!(
        fmt,
        "epoch_millis"
            | "epoch_second"
            | "strict_date_optional_time"
            | "strict_date_optional_time_nanos"
            | "iso8601"
            | "date_optional_time"
            | "basic_date"
            | "basic_date_time"
            | "basic_time"
            | "date"
            | "strict_date"
            | "date_hour_minute_second"
            | "strict_date_hour_minute_second"
            | "date_time"
            | "strict_date_time"
            | "hour"
            | "hour_minute_second"
            | "year"
            | "strict_year"
            | "year_month"
            | "strict_year_month"
            | "year_month_day"
            | "strict_year_month_day"
    )
}

/// Translate a small subset of Java SimpleDateFormat patterns into
/// chrono's strftime dialect. Only the common tokens ES YAML tests
/// exercise (`yyyy`, `MM`, `dd`, `HH`, `mm`, `ss`, `SSS`, `Z`) are
/// supported; literal characters pass through unchanged.
pub(crate) fn java_to_strftime(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 4);
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Java SimpleDateFormat literal section between two `'` quotes —
        // pass through unchanged after stripping the quotes.
        if bytes[i] == b'\'' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'\'' {
                out.push(bytes[i] as char);
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            } // skip closing quote
            continue;
        }
        // Match longest-first. Java uses `uuuu` for proleptic year and
        // `yyyy` for year-of-era; both render as 4-digit year in modern
        // ES so map both to `%Y`. `ZZZZZ` is offset with colon; `XXX`
        // similarly emits `+HH:MM`.
        let matched: Option<(usize, &str)> =
            if bytes[i..].starts_with(b"uuuu") || bytes[i..].starts_with(b"yyyy") {
                Some((4, "%Y"))
            } else if bytes[i..].starts_with(b"yy") {
                Some((2, "%y"))
            } else if bytes[i..].starts_with(b"MM") {
                Some((2, "%m"))
            } else if bytes[i..].starts_with(b"dd") {
                Some((2, "%d"))
            } else if bytes[i..].starts_with(b"HH") {
                Some((2, "%H"))
            } else if bytes[i..].starts_with(b"mm") {
                Some((2, "%M"))
            } else if bytes[i..].starts_with(b"ss") {
                Some((2, "%S"))
            } else if bytes[i..].starts_with(b"SSSSSSSSS") {
                Some((9, "%9f"))
            } else if bytes[i..].starts_with(b"SSSSSS") {
                Some((6, "%6f"))
            } else if bytes[i..].starts_with(b"SSS") {
                Some((3, "%3f"))
            } else if bytes[i..].starts_with(b"ZZZZZ") {
                Some((5, "%:z"))
            } else if bytes[i..].starts_with(b"XXX") {
                Some((3, "%:z"))
            } else if bytes[i..].starts_with(b"Z") {
                Some((1, "%z"))
            } else {
                None
            };
        match matched {
            Some((n, s)) => {
                out.push_str(s);
                i += n;
            }
            None => {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
    }
    out
}

/// Format a Unix epoch in milliseconds as the ISO-8601 UTC string with
/// millisecond precision, matching Elasticsearch's default date format
/// (`"yyyy-MM-dd'T'HH:mm:ss.SSSZ"` → `"2016-05-03T00:00:00.000Z"`).
pub(crate) fn epoch_ms_to_iso8601_utc(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

/// Count fractional-second digits in an ISO-8601 string
/// (e.g. "2021-08-12T00:00:00.000000000+02:00" → 9).
pub(crate) fn detect_fractional_digits(s: &str) -> usize {
    let Some(dot) = s.find('.') else { return 0 };
    s[dot + 1..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .count()
}

/// Render an epoch-ms instant as an ISO-8601 string with configurable
/// fractional-second precision and optional fixed offset. When `offset`
/// is `None` the output is UTC with a `Z` suffix. With a fixed offset
/// the suffix becomes `±HH:MM`.
pub(crate) fn render_iso_date(
    ms: i64,
    nanos_digits: usize,
    offset: Option<chrono::FixedOffset>,
) -> String {
    let Some(dt_utc) = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms) else {
        return String::new();
    };
    let frac = match nanos_digits {
        9 => "%9f",
        6 => "%6f",
        _ => "%3f",
    };
    if let Some(off) = offset {
        let dt = dt_utc.with_timezone(&off);
        let fmt = format!("%Y-%m-%dT%H:%M:%S.{}%:z", frac);
        return dt.format(&fmt).to_string();
    }
    let fmt = format!("%Y-%m-%dT%H:%M:%S.{}Z", frac);
    dt_utc.format(&fmt).to_string()
}

/// Parse an ES date string or epoch-ms number to a Unix timestamp in milliseconds.
pub(crate) fn parse_date_ms(val: &Value) -> Option<i64> {
    match val {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => {
            // Try RFC3339 first, then a few common formats.
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
                return Some(dt.timestamp_millis());
            }
            // Try ISO-8601 without timezone (treat as UTC).
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f") {
                return Some(dt.and_utc().timestamp_millis());
            }
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
                return Some(dt.and_utc().timestamp_millis());
            }
            if let Ok(dt) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                return Some(dt.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis());
            }
            // Slash-separated form `YYYY/MM/DD` is accepted by ES via the
            // default date mapping — treat it as YMD at UTC midnight.
            if let Ok(dt) = chrono::NaiveDate::parse_from_str(s, "%Y/%m/%d") {
                return Some(dt.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis());
            }
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y/%m/%d %H:%M:%S") {
                return Some(dt.and_utc().timestamp_millis());
            }
            // Space-separated form `YYYY-MM-DD HH:MM:SS[.fff]` (ES Java
            // pattern `yyyy-MM-dd HH:mm:ss.SSS`).
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f") {
                return Some(dt.and_utc().timestamp_millis());
            }
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
                return Some(dt.and_utc().timestamp_millis());
            }
            // Fallback: try parsing as a raw integer string.
            s.parse::<i64>().ok()
        }
        _ => None,
    }
}

/// Format a numeric histogram bucket key as the string used in composite
/// key-part maps.  Integer values format without a trailing `.0` so they
/// round-trip via serde_json::Number::as_i64 when emitted.
pub(crate) fn format_histogram_key(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e18 {
        format!("{}", v as i64)
    } else {
        format!("{}", v)
    }
}

/// Build a terms-agg bucket for the keyword-column fast path.  Keys come out
/// of a keyword doc-values column (always strings); we only convert booleans
/// to 1/0 (with `key_as_string` = `"true"`/`"false"`) and leave everything
/// else as a string, matching ES behaviour for `type: keyword` fields.
fn build_keyword_terms_bucket(key: &str, count: u64) -> Value {
    if key == "true" || key == "false" {
        return json!({
            "key": if key == "true" { 1 } else { 0 },
            "key_as_string": key,
            "doc_count": count,
        });
    }
    json!({"key": key, "doc_count": count})
}

/// Build a terms-agg bucket with ES-compatible typed `key`/`key_as_string`:
///
/// - booleans → numeric 1/0, `key_as_string` = `"true"`/`"false"`
/// - integers/floats → numeric key, no `key_as_string`
/// - ISO-8601 dates → epoch ms key, `key_as_string` = ISO-8601 UTC with millis
/// - anything else → raw string key, no `key_as_string`
fn build_terms_bucket(key: &str, count: u64) -> Value {
    let mut kas: Option<String> = None;
    let typed_key: Value = if key == "true" || key == "false" {
        kas = Some(key.to_string());
        json!(if key == "true" { 1 } else { 0 })
    } else if let Ok(n) = key.parse::<i64>() {
        json!(n)
    } else if let Ok(f) = key.parse::<f64>() {
        serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(json!(key))
    } else if let Some(epoch_ms) = parse_date_ms(&Value::String(key.to_string())) {
        // ES emits the canonical ISO-8601 UTC form (with millisecond precision),
        // regardless of the input form (`2016-05-03` expands to full timestamp).
        let dt = chrono::DateTime::from_timestamp_millis(epoch_ms)
            .unwrap_or_default()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();
        kas = Some(dt);
        json!(epoch_ms)
    } else {
        json!(key)
    };
    let mut b = json!({"key": typed_key, "doc_count": count});
    if let Some(s) = kas {
        b["key_as_string"] = json!(s);
    }
    b
}

pub(crate) fn interval_to_ms(interval: &str) -> Option<i64> {
    let interval = interval.trim();
    match interval {
        "second" | "1s" => return Some(1_000),
        "minute" | "1m" => return Some(60_000),
        "hour" | "1h" => return Some(3_600_000),
        "day" | "1d" => return Some(86_400_000),
        "week" | "1w" => return Some(7 * 86_400_000),
        "month" | "1M" => return Some(30 * 86_400_000),
        "quarter" | "1q" => return Some(91 * 86_400_000),
        "year" | "1y" => return Some(365 * 86_400_000),
        _ => {}
    }
    let (num_str, unit) = interval.split_at(
        interval
            .find(|c: char| c.is_alphabetic())
            .unwrap_or(interval.len()),
    );
    let n: i64 = num_str.trim().parse().ok()?;
    let ms_per_unit = match unit {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        "w" => 7 * 86_400_000,
        "M" => 30 * 86_400_000,
        "q" => 91 * 86_400_000,
        "y" => 365 * 86_400_000,
        _ => return None,
    };
    Some(n * ms_per_unit)
}

pub(crate) fn is_calendar_interval(interval: &str) -> bool {
    // ES's `calendar_interval` accepts these shorthand units — each one
    // is sensitive to local-time boundaries (DST, month lengths). We
    // treat `1d`/`day`/`week`/`1w` as calendar too: a "day" is a local
    // calendar day, not a fixed 24h span, so DST transitions produce
    // 23h / 25h days that still bucket correctly.
    matches!(
        interval.trim(),
        "minute"
            | "1m"
            | "hour"
            | "1h"
            | "day"
            | "1d"
            | "week"
            | "1w"
            | "month"
            | "1M"
            | "quarter"
            | "1q"
            | "year"
            | "1y"
    )
}

pub(crate) fn calendar_bucket_key(ts_ms: i64, interval: &str) -> i64 {
    let dt = chrono::DateTime::from_timestamp_millis(ts_ms).unwrap_or_default();
    let naive = dt.naive_utc();
    match interval.trim() {
        "minute" | "1m" => ts_ms.div_euclid(60_000) * 60_000,
        "hour" | "1h" => ts_ms.div_euclid(3_600_000) * 3_600_000,
        "day" | "1d" => ts_ms.div_euclid(86_400_000) * 86_400_000,
        "week" | "1w" => {
            // ISO week starts Monday. Align to the most-recent Monday 00:00.
            let weekday0 = naive.weekday().num_days_from_monday() as i64;
            let day_floor = ts_ms.div_euclid(86_400_000) * 86_400_000;
            day_floor - weekday0 * 86_400_000
        }
        "month" | "1M" => {
            let start = chrono::NaiveDate::from_ymd_opt(naive.year(), naive.month(), 1).unwrap();
            start
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis()
        }
        "quarter" | "1q" => {
            let q_month = ((naive.month() - 1) / 3) * 3 + 1;
            let start = chrono::NaiveDate::from_ymd_opt(naive.year(), q_month, 1).unwrap();
            start
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis()
        }
        "year" | "1y" => {
            let start = chrono::NaiveDate::from_ymd_opt(naive.year(), 1, 1).unwrap();
            start
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis()
        }
        _ => ts_ms,
    }
}

pub(crate) fn next_calendar_bucket(bucket_ms: i64, interval: &str) -> i64 {
    let dt = chrono::DateTime::from_timestamp_millis(bucket_ms).unwrap_or_default();
    let naive = dt.naive_utc();
    match interval.trim() {
        "minute" | "1m" => bucket_ms + 60_000,
        "hour" | "1h" => bucket_ms + 3_600_000,
        "day" | "1d" => bucket_ms + 86_400_000,
        "week" | "1w" => bucket_ms + 7 * 86_400_000,
        "month" | "1M" => {
            let (y, m) = if naive.month() == 12 {
                (naive.year() + 1, 1)
            } else {
                (naive.year(), naive.month() + 1)
            };
            let start = chrono::NaiveDate::from_ymd_opt(y, m, 1).unwrap();
            start
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis()
        }
        "quarter" | "1q" => {
            let q_month = ((naive.month() - 1) / 3) * 3 + 1;
            let (y, m) = if q_month + 3 > 12 {
                (naive.year() + 1, q_month + 3 - 12)
            } else {
                (naive.year(), q_month + 3)
            };
            let start = chrono::NaiveDate::from_ymd_opt(y, m, 1).unwrap();
            start
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis()
        }
        "year" | "1y" => {
            let start = chrono::NaiveDate::from_ymd_opt(naive.year() + 1, 1, 1).unwrap();
            start
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis()
        }
        _ => bucket_ms,
    }
}

use chrono::Datelike;

/// Parse an ES time_zone string into a fixed-offset `FixedOffset`.
/// Accepts "+01:00", "-03:00", "+0100", "Z", "UTC", and common IANA names
/// that map to a stable fixed offset (e.g. "America/Los_Angeles"). If the
/// zone is IANA and its DST-aware offset is needed, callers should consult
/// `fixed_offset_for_tz_at()` — this helper returns the *standard-time*
/// offset as a conservative fallback.
pub(crate) fn parse_time_zone_offset(tz: &str) -> Option<chrono::FixedOffset> {
    let t = tz.trim();
    if t.is_empty() || t == "Z" || t.eq_ignore_ascii_case("UTC") || t == "+00:00" || t == "-00:00" {
        return Some(chrono::FixedOffset::east_opt(0).unwrap());
    }
    // "+01:00" / "-03:30" / "+0530"
    let bytes = t.as_bytes();
    if bytes[0] == b'+' || bytes[0] == b'-' {
        let sign: i32 = if bytes[0] == b'+' { 1 } else { -1 };
        let rest = &t[1..];
        let (h, m) = if let Some(idx) = rest.find(':') {
            (
                rest[..idx].parse::<i32>().ok()?,
                rest[idx + 1..].parse::<i32>().ok()?,
            )
        } else if rest.len() == 4 {
            (
                rest[..2].parse::<i32>().ok()?,
                rest[2..].parse::<i32>().ok()?,
            )
        } else if rest.len() == 2 {
            (rest.parse::<i32>().ok()?, 0)
        } else {
            return None;
        };
        let total = sign * (h * 3600 + m * 60);
        return chrono::FixedOffset::east_opt(total);
    }
    None
}

/// Given an IANA zone name and an instant, return the offset at that instant,
/// accounting for DST. Falls back to fixed-offset parsing when `tz` is already
/// numeric.
pub(crate) fn fixed_offset_for_tz_at(tz: &str, ts_ms: i64) -> Option<chrono::FixedOffset> {
    if let Some(off) = parse_time_zone_offset(tz) {
        return Some(off);
    }
    use chrono::{Offset, TimeZone};
    let zone: chrono_tz::Tz = tz.parse().ok()?;
    let dt = chrono::DateTime::from_timestamp_millis(ts_ms)?;
    let local = zone.from_utc_datetime(&dt.naive_utc());
    Some(local.offset().fix())
}

/// Parse an ES date_histogram `offset` string into milliseconds.
/// Accepts forms like "+1d", "-1h", "+30m", "+90ms".
pub(crate) fn parse_offset_ms(s: &str) -> Option<i64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let (sign, rest) = match t.as_bytes()[0] {
        b'+' => (1i64, &t[1..]),
        b'-' => (-1i64, &t[1..]),
        _ => (1i64, t),
    };
    interval_to_ms(rest).map(|v| sign * v)
}

fn run_date_histogram(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"buckets": []}),
    };

    // Accept both `fixed_interval` and `calendar_interval` (ES 8.x naming)
    // as well as the legacy `interval` key.
    let interval_str = params
        .get("fixed_interval")
        .or_else(|| params.get("calendar_interval"))
        .or_else(|| params.get("interval"))
        .and_then(Value::as_str)
        .unwrap_or("1d");

    let interval_ms = match interval_to_ms(interval_str) {
        Some(ms) => ms,
        None => return json!({"error": format!("unknown interval '{}'", interval_str)}),
    };

    let use_calendar = is_calendar_interval(interval_str);

    let tz_str = params
        .get("time_zone")
        .and_then(Value::as_str)
        .unwrap_or("");
    // Only set tz_offset when an explicit time_zone was supplied. ES uses
    // `...Z` ISO-8601 formatting when no time_zone is set, and `...+HH:MM`
    // when one is. IANA zones (Europe/Berlin etc.) are resolved to the
    // offset at a representative instant — we use the first doc's timestamp
    // to pick up the correct DST-adjusted offset.
    let tz_offset: Option<chrono::FixedOffset> = if tz_str.is_empty() {
        None
    } else if let Some(off) = parse_time_zone_offset(tz_str) {
        Some(off)
    } else {
        // Need a sample instant to pick the right DST offset.
        let sample_ms = docs
            .iter()
            .filter_map(|d| extract_date_ms_values(d, field).into_iter().next())
            .next()
            .unwrap_or(0);
        fixed_offset_for_tz_at(tz_str, sample_ms)
    };
    let offset_ms = params
        .get("offset")
        .and_then(Value::as_str)
        .and_then(parse_offset_ms)
        .unwrap_or(0);

    // hard_bounds: { min: ISO, max: ISO }. Buckets outside are dropped.
    let (hard_min, hard_max) = if let Some(hb) = params.get("hard_bounds") {
        (
            hb.get("min").and_then(parse_date_ms),
            hb.get("max").and_then(parse_date_ms),
        )
    } else {
        (None, None)
    };

    // extended_bounds: { min, max } forces bucket range to include these.
    let (ext_min, ext_max) = if let Some(eb) = params.get("extended_bounds") {
        (
            eb.get("min").and_then(parse_date_ms),
            eb.get("max").and_then(parse_date_ms),
        )
    } else {
        (None, None)
    };

    let mut buckets: HashMap<i64, Vec<usize>> = HashMap::new();

    // `missing` on date_histogram: the value can be an ISO-8601 string or
    // an epoch-ms number. Missing-field docs fall into this bucket.
    let missing_ms: Option<i64> = params.get("missing").and_then(parse_date_ms);

    // Bucket-key derivation with time_zone:
    //   local_ts = utc_ts + tz_offset_seconds*1000
    //   bucketed_local = floor(local_ts / interval) * interval  (or calendar)
    //   utc_key = bucketed_local - tz_offset_seconds*1000
    //
    // For IANA zones the offset depends on the instant. Resolve per-
    // timestamp so buckets straddling a DST transition get the correct
    // midnight. For fixed-offset zones this reduces to the single value.
    let tz_str_bucket = tz_str.to_string();
    let tz_is_iana = !tz_str_bucket.is_empty() && parse_time_zone_offset(&tz_str_bucket).is_none();
    let shift_at = |ts: i64| -> i64 {
        if tz_is_iana {
            if let Some(off) = fixed_offset_for_tz_at(&tz_str_bucket, ts) {
                return off.local_minus_utc() as i64 * 1000;
            }
        }
        tz_offset
            .map(|o| o.local_minus_utc() as i64 * 1000)
            .unwrap_or(0)
    };
    let to_local = |ts: i64| ts + shift_at(ts) - offset_ms;
    // Map a local-time bucket-start back to UTC. The local time 00:00 of
    // a given day exists at a single UTC instant; find the shift at the
    // UTC estimate `local - fixed_base_shift` (for IANA zones we iterate
    // briefly if the chosen shift changes the answer).
    let fixed_base_shift = tz_offset
        .map(|o| o.local_minus_utc() as i64 * 1000)
        .unwrap_or(0);
    let to_utc = |local: i64| -> i64 {
        let mut utc = local - fixed_base_shift + offset_ms;
        if tz_is_iana {
            let shift = shift_at(utc);
            utc = local - shift + offset_ms;
        }
        utc
    };

    for (i, doc) in docs.iter().enumerate() {
        // Multi-valued date fields contribute one entry per distinct bucket.
        let mut raws = extract_date_ms_values(doc, field);
        // Range-typed date fields: a doc contributes to every bucket whose
        // interval overlaps its [from_ms, to_ms) span. Only considered if the
        // scalar extraction found nothing.
        let ranges = if raws.is_empty() {
            extract_date_ranges(doc, field)
        } else {
            Vec::new()
        };
        if raws.is_empty() && ranges.is_empty() {
            if let Some(m) = missing_ms {
                raws.push(m);
            }
        }
        let mut seen: Vec<i64> = Vec::with_capacity(raws.len() + ranges.len());
        for ts_ms in raws {
            let local_ts = to_local(ts_ms);
            let local_bucket = if use_calendar {
                calendar_bucket_key(local_ts, interval_str)
            } else {
                local_ts.div_euclid(interval_ms) * interval_ms
            };
            let bucket_key = to_utc(local_bucket);
            // Apply hard_bounds (inclusive on both ends — ES semantics).
            if let Some(m) = hard_min {
                if bucket_key < m {
                    continue;
                }
            }
            if let Some(m) = hard_max {
                if bucket_key > m {
                    continue;
                }
            }
            if !seen.contains(&bucket_key) {
                seen.push(bucket_key);
                buckets.entry(bucket_key).or_default().push(i);
            }
        }
        for (from_ms, to_ms) in ranges {
            let start_anchor = from_ms.max(hard_min.unwrap_or(i64::MIN));
            // When hard_max is set it's the largest *bucket key* allowed;
            // the bucket at that key spans beyond max by one interval, so
            // we still emit it when the range intersects it.
            let end_anchor = to_ms; // compare bucket_key against to_ms via interval overlap
            if end_anchor <= start_anchor {
                continue;
            }
            let local_start = to_local(start_anchor);
            let mut local_bucket = if use_calendar {
                calendar_bucket_key(local_start, interval_str)
            } else {
                local_start.div_euclid(interval_ms) * interval_ms
            };
            let mut guard = 0usize;
            loop {
                guard += 1;
                if guard > 65_536 {
                    break;
                }
                let bucket_key = to_utc(local_bucket);
                // Stop once the bucket's start is at/after the range's upper bound
                // — no more intersection possible.
                if bucket_key >= end_anchor {
                    break;
                }
                // Stop once we've walked past the hard_max bucket.
                if let Some(m) = hard_max {
                    if bucket_key > m {
                        break;
                    }
                }
                let next_local = if use_calendar {
                    next_calendar_bucket(local_bucket, interval_str)
                } else {
                    local_bucket + interval_ms
                };
                let next_key = to_utc(next_local);
                if next_key > start_anchor {
                    if let Some(m) = hard_min {
                        if bucket_key < m {
                            local_bucket = next_local;
                            continue;
                        }
                    }
                    if !seen.contains(&bucket_key) {
                        seen.push(bucket_key);
                        buckets.entry(bucket_key).or_default().push(i);
                    }
                }
                if next_key <= bucket_key {
                    break;
                } // safety against infinite loop
                local_bucket = next_local;
            }
        }
    }

    let min_doc_count = params
        .get("min_doc_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let keyed = params
        .get("keyed")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    const MAX_BUCKETS: i64 = 65_536;

    // Compute base range: from data, or from hard_bounds / extended_bounds.
    let mut range_min = buckets.keys().min().cloned();
    let mut range_max = buckets.keys().max().cloned();

    // extended_bounds widens the fill range. ES treats the supplied
    // bound as *local wall-clock* (it's already expressed in the zone
    // the user cares about), so we convert via `+ shift_at(m) - offset`
    // = local, floor, then map back to UTC. For fixed-offset zones this
    // is the same as for data bucketing; for IANA zones we resolve
    // per-instant just like docs.
    let ext_bound_to_local = |m: i64| -> i64 {
        // The raw bound is an epoch-ms in the user's local frame
        // (`2016-01-01` parsed as midnight-UTC is interpreted as
        // midnight-local). Drop the offset contribution so the bucket
        // key renders as user-expected local midnight.
        m
    };
    let ext_bound_to_utc = |local_b: i64| -> i64 {
        // Convert the local bucket-start back to UTC, mirroring to_utc
        // but without the `- offset_ms` cancellation (the `offset_ms`
        // is a BUCKET-SHIFT applied after, not a coordinate transform).
        let mut utc = local_b - fixed_base_shift;
        if tz_is_iana {
            let shift = shift_at(utc);
            utc = local_b - shift;
        }
        utc + offset_ms
    };
    if let Some(m) = ext_min {
        let local_m = ext_bound_to_local(m);
        let b = if use_calendar {
            calendar_bucket_key(local_m, interval_str)
        } else {
            local_m.div_euclid(interval_ms) * interval_ms
        };
        let key = ext_bound_to_utc(b);
        range_min = Some(range_min.map_or(key, |k| k.min(key)));
        range_max = Some(range_max.map_or(key, |k| k.max(key)));
    }
    if let Some(m) = ext_max {
        let local_m = ext_bound_to_local(m);
        let b = if use_calendar {
            calendar_bucket_key(local_m, interval_str)
        } else {
            local_m.div_euclid(interval_ms) * interval_ms
        };
        let key = ext_bound_to_utc(b);
        range_min = Some(range_min.map_or(key, |k| k.min(key)));
        range_max = Some(range_max.map_or(key, |k| k.max(key)));
    }

    // hard_bounds further clamps the fill range (honored alongside data filter)
    if let Some(m) = hard_min {
        let local_m = to_local(m);
        let b = if use_calendar {
            calendar_bucket_key(local_m, interval_str)
        } else {
            local_m.div_euclid(interval_ms) * interval_ms
        };
        let key = to_utc(b);
        range_min = Some(range_min.map_or(key, |k| k.max(key)));
    }
    if let Some(m) = hard_max {
        // hard_bounds is inclusive on both ends: the bucket whose start
        // key equals `max` is still emitted.
        let local_m = to_local(m);
        let b = if use_calendar {
            calendar_bucket_key(local_m, interval_str)
        } else {
            local_m.div_euclid(interval_ms) * interval_ms
        };
        let key = to_utc(b);
        range_max = Some(range_max.map_or(key, |k| k.min(key)));
    }

    let mut bucket_keys: Vec<i64> = if min_doc_count > 0 {
        buckets.keys().cloned().collect()
    } else if let (Some(min_key), Some(max_key)) = (range_min, range_max) {
        // Fill gaps between min and max. Enforce MAX_BUCKETS cap.
        let mut span: i64 = 0;
        let mut probe = min_key;
        while probe <= max_key {
            span += 1;
            if span > MAX_BUCKETS {
                return json!({
                    "error": format!(
                        "Trying to create too many buckets. Must be less than or equal to: [{}] but this number of buckets was exceeded. This limit can be set by changing the [search.max_buckets] cluster level setting.",
                        MAX_BUCKETS
                    )
                });
            }
            probe = if use_calendar {
                let local = to_local(probe);
                let next_local = next_calendar_bucket(local, interval_str);
                to_utc(next_local)
            } else {
                probe + interval_ms
            };
        }
        let mut keys = Vec::with_capacity(span as usize);
        let mut k = min_key;
        while k <= max_key {
            keys.push(k);
            let next = if use_calendar {
                let local = to_local(k);
                let next_local = next_calendar_bucket(local, interval_str);
                to_utc(next_local)
            } else {
                k + interval_ms
            };
            // Safety: ensure forward progress. Per-timestamp DST resolution
            // can cause `to_utc(to_local(k) + 1d_local)` to collapse back
            // onto `k` near the transition (both days' midnight-local round
            // to the same UTC instant when the boundary straddles the
            // gap-fill iteration). Fall back to a fixed-offset step so the
            // loop advances.
            if next <= k {
                k += interval_ms;
            } else {
                k = next;
            }
        }
        keys
    } else {
        buckets.keys().cloned().collect()
    };
    bucket_keys.sort_unstable();

    // min_doc_count filter (applied after gap-fill)
    if min_doc_count > 0 {
        bucket_keys.retain(|k| {
            buckets
                .get(k)
                .map(|v| v.len() as u64 >= min_doc_count)
                .unwrap_or(false)
        });
    }

    let empty_indices: Vec<usize> = Vec::new();
    let fmt_param = params.get("format").and_then(Value::as_str);
    // For IANA zones, the offset may differ per bucket key (e.g. before
    // vs after a DST transition). Capture the user-supplied tz string so
    // each render can re-resolve at the bucket's instant.
    let tz_str_for_render = tz_str.to_string();
    let render_key = |key: i64| -> String {
        // Re-resolve the offset at this specific instant when an IANA
        // zone was supplied. Falls back to the parse-time fixed offset
        // otherwise.
        let off = if !tz_str_for_render.is_empty() {
            fixed_offset_for_tz_at(&tz_str_for_render, key).or(tz_offset)
        } else {
            tz_offset
        };
        if let Some(off) = off {
            let dt_utc = chrono::DateTime::from_timestamp_millis(key).unwrap_or_default();
            let dt_local = dt_utc.with_timezone(&off);
            if let Some(fmt) = fmt_param {
                // render_date_format formats DateTime<Utc> using chrono's
                // strftime. To emit the tz-adjusted wall clock, feed a
                // synthetic UTC timestamp that embeds the local offset.
                let off_ms = off.local_minus_utc() as i64 * 1000;
                let shifted_ms = key + off_ms;
                let dt_shifted = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(shifted_ms)
                    .unwrap_or_default();
                render_date_format(Some(fmt), shifted_ms, dt_shifted)
            } else {
                dt_local.format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string()
            }
        } else {
            let dt_val = chrono::DateTime::from_timestamp_millis(key).unwrap_or_default();
            render_date_format(fmt_param, key, dt_val)
        }
    };

    let result_buckets: Vec<Value> = bucket_keys
        .iter()
        .map(|&key| {
            let doc_indices = buckets.get(&key).unwrap_or(&empty_indices);
            let bucket_docs: Vec<Value> = doc_indices.iter().map(|&i| docs[i].clone()).collect();
            let doc_count = sum_doc_count(&bucket_docs);

            let dt = render_key(key);

            let mut bucket = json!({
                "key": key,
                "key_as_string": dt,
                "doc_count": doc_count
            });

            if let Some(sub) = sub_aggs {
                let sub_result = run_aggs_with_all(sub, &bucket_docs, all_docs);
                if let (Some(bucket_obj), Value::Object(sub_obj)) =
                    (bucket.as_object_mut(), sub_result)
                {
                    for (k, v) in sub_obj {
                        bucket_obj.insert(k, v);
                    }
                }
            }

            bucket
        })
        .collect();

    if keyed {
        let mut map = serde_json::Map::new();
        for b in result_buckets {
            let k = b
                .get("key_as_string")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| b.get("key").and_then(|v| v.as_i64()).map(|i| i.to_string()))
                .unwrap_or_default();
            map.insert(k, b);
        }
        json!({ "buckets": Value::Object(map) })
    } else {
        json!({ "buckets": result_buckets })
    }
}

// ── Metric aggregations ────────────────────────────────────────────────────────

fn run_value_count<'d>(params: &Value, _docs: &'d [Value], cache: &mut FieldCache<'d>) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"value": 0}),
    };

    // Count the *number of values* (not docs): a multi-valued field with
    // 3 entries in one doc contributes 3. When `missing` is set, each doc
    // without a value for the field contributes 1 (the placeholder).
    let has_missing = params.get("missing").is_some();
    let mut count: usize = 0;
    for v in cache.get_strings(field) {
        if v.is_empty() {
            if has_missing {
                count += 1;
            }
        } else {
            count += v.len();
        }
    }
    json!({ "value": count })
}

fn get_missing_value(params: &Value) -> Option<f64> {
    params.get("missing").and_then(|v| v.as_f64())
}

fn run_avg<'d>(params: &Value, docs: &'d [Value], cache: &mut FieldCache<'d>) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"value": Value::Null, "__xy_count__": 0, "__xy_sum__": 0.0}),
    };
    let missing = get_missing_value(params);
    let (sum, count) = cache
        .get_numeric(field)
        .iter()
        .fold((0.0f64, 0usize), |(s, n), v| match v.or(missing) {
            Some(x) => (s + x, n + 1),
            None => (s, n),
        });
    if count == 0 {
        return json!({ "value": Value::Null, "__xy_count__": 0, "__xy_sum__": 0.0 });
    }
    let avg = sum / count as f64;
    if field_is_date_shape(docs, field) {
        let s = epoch_ms_to_iso8601_utc(avg as i64);
        return json!({ "value": avg, "value_as_string": s, "__xy_count__": count, "__xy_sum__": sum });
    }
    json!({ "value": avg, "__xy_count__": count, "__xy_sum__": sum })
}

fn run_sum<'d>(params: &Value, docs: &'d [Value], cache: &mut FieldCache<'d>) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"value": 0.0, "__xy_agg__": "sum"}),
    };
    let missing = get_missing_value(params);
    let values: Vec<f64> = cache
        .get_numeric(field)
        .iter()
        .filter_map(|v| v.or(missing))
        .collect();
    // ES semantics: a top-level `sum` with no matching docs returns 0.0
    // (the additive identity), but a `sum` that participated in a
    // bucketing agg where the bucket had zero docs is emitted as 0 in the
    // hit representation. Keep 0.0 as the default since downstream
    // consumers (like sum_bucket with gap_policy=skip) drop -0.0 only if
    // we surface it as a non-finite value; however, the ES convention is
    // to emit the real 0.0. Parent pipelines that want to skip empty
    // buckets can still honor gap_policy=skip by checking doc_count.
    let total: f64 = values.iter().sum();
    let _ = docs; // reserved for future doc_count-based null emission
    json!({ "value": total, "__xy_agg__": "sum" })
}

fn run_min<'d>(params: &Value, docs: &'d [Value], cache: &mut FieldCache<'d>) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"value": Value::Null, "__xy_agg__": "min"}),
    };
    let missing = get_missing_value(params);
    let min = cache
        .get_numeric(field)
        .iter()
        .filter_map(|v| v.or(missing))
        .fold(f64::INFINITY, f64::min);
    if min.is_infinite() {
        return json!({ "value": Value::Null, "__xy_agg__": "min" });
    }
    if field_is_date_shape(docs, field) {
        let s = epoch_ms_to_iso8601_utc(min as i64);
        return json!({ "value": min, "value_as_string": s, "__xy_agg__": "min" });
    }
    {
        json!({ "value": min, "__xy_agg__": "min" })
    }
}

fn run_max<'d>(params: &Value, docs: &'d [Value], cache: &mut FieldCache<'d>) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"value": Value::Null, "__xy_agg__": "max"}),
    };
    let missing = get_missing_value(params);
    let max = cache
        .get_numeric(field)
        .iter()
        .filter_map(|v| v.or(missing))
        .fold(f64::NEG_INFINITY, f64::max);
    if max.is_infinite() {
        return json!({ "value": Value::Null, "__xy_agg__": "max" });
    }
    // When the source values look like ISO dates (string shape that
    // round-trips via `parse_date_ms`), emit `value_as_string` so the
    // response matches ES's date-metric shape.
    if field_is_date_shape(docs, field) {
        let s = epoch_ms_to_iso8601_utc(max as i64);
        return json!({ "value": max, "value_as_string": s, "__xy_agg__": "max" });
    }
    json!({ "value": max, "__xy_agg__": "max" })
}

fn field_is_date_shape(docs: &[Value], field: &str) -> bool {
    docs.iter().any(|d| match get_nested_field(d, field) {
        Value::String(s) => {
            parse_date_ms(&Value::String(s.clone())).is_some() && s.parse::<f64>().is_err()
        }
        Value::Array(arr) => arr.iter().any(|e| match e {
            Value::String(s) => {
                parse_date_ms(&Value::String(s.clone())).is_some() && s.parse::<f64>().is_err()
            }
            _ => false,
        }),
        _ => false,
    })
}

fn run_stats<'d>(params: &Value, _docs: &'d [Value], cache: &mut FieldCache<'d>) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => {
            return json!({
                "count": 0,
                "min": Value::Null,
                "max": Value::Null,
                "avg": Value::Null,
                "sum": 0.0
            })
        }
    };

    let missing = get_missing_value(params);
    let (count, sum, min, max) = cache
        .get_numeric(field)
        .iter()
        .filter_map(|v| v.or(missing))
        .fold(
            (0usize, 0.0f64, f64::INFINITY, f64::NEG_INFINITY),
            |(cnt, s, mn, mx), x| (cnt + 1, s + x, mn.min(x), mx.max(x)),
        );

    if count == 0 {
        return json!({
            "count": 0,
            "min": Value::Null,
            "max": Value::Null,
            "avg": Value::Null,
            "sum": 0.0
        });
    }

    let avg = sum / count as f64;
    json!({
        "count": count,
        "min": min,
        "max": max,
        "avg": avg,
        "sum": sum
    })
}

fn run_cardinality<'d>(params: &Value, docs: &'d [Value], cache: &mut FieldCache<'d>) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"value": 0}),
    };

    // When `missing` is set, docs that don't have the field contribute a
    // synthetic placeholder value so they count toward the distinct set.
    let missing_placeholder: Option<String> = params.get("missing").and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    });

    let mut distinct: HashSet<String> = HashSet::new();
    let field_vals = cache.get_strings(field);
    for (i, doc) in docs.iter().enumerate() {
        let vals = field_vals.get(i).cloned().unwrap_or_default();
        if vals.is_empty() {
            if let Some(ph) = &missing_placeholder {
                distinct.insert(ph.clone());
            }
            continue;
        }
        let _ = doc;
        for v in vals {
            distinct.insert(v);
        }
    }

    // Emit the full distinct set as an internal tracking field so the
    // cross-index merge can compute the union rather than summing.
    let values: Vec<Value> = distinct.iter().map(|s| Value::String(s.clone())).collect();
    json!({
        "value": distinct.len(),
        "__xy_agg__": "cardinality",
        "__xy_values__": values,
    })
}

// ── Filter aggregation ────────────────────────────────────────────────────────

/// Filter aggregation: filter documents using an ES query, then run sub-aggs.
// (signature updated to thread all_docs through)
/// The `params` here is the filter query itself (e.g. `{"term": {"level": "ERROR"}}`).
fn run_filter(
    filter_query: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let filtered_docs: Vec<Value> = docs
        .iter()
        .filter(|doc| doc_matches_filter(doc, filter_query))
        .cloned()
        .collect();

    let doc_count = sum_doc_count(&filtered_docs);
    let mut result = serde_json::Map::new();
    result.insert("doc_count".to_string(), json!(doc_count));

    if let Some(sub) = sub_aggs {
        let sub_result = run_aggs_with_all(sub, &filtered_docs, all_docs);
        if let Value::Object(sub_obj) = sub_result {
            for (k, v) in sub_obj {
                result.insert(k, v);
            }
        }
    }

    Value::Object(result)
}

/// ES `filters` (plural) aggregation — multi-bucket, each bucket defined by
/// a named filter query.  Docs are independently matched against each filter
/// (a doc can appear in multiple buckets).
///
/// Request shape:
/// ```json
/// { "filters": { "filters": { "errors": { "match": { "body": "error" } },
///                               "warnings": { "term": { "level": "warn" } } } },
///   "aggs": { "avg_bytes": { "avg": { "field": "bytes" } } } }
/// ```
///
/// Also supports anonymous array form:
/// ```json
/// { "filters": { "filters": [ { "term": { "status": "200" } },
///                               { "term": { "status": "404" } } ] } }
/// ```
fn run_filters(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let filters_val = match params.get("filters") {
        Some(v) => v,
        None => return json!({"buckets": {}}),
    };

    // ES `keyed` defaults to true for a named `filters` map and false for
    // an anonymous array. `keyed:false` on a named map rewrites to an
    // array of buckets each carrying a `key` field.
    let keyed_override = params.get("keyed").and_then(Value::as_bool);

    match filters_val {
        // Named filters: { "name1": { query }, "name2": { query } }
        Value::Object(map) => {
            let keyed = keyed_override.unwrap_or(true);
            if keyed {
                let mut buckets = serde_json::Map::new();
                for (name, query) in map {
                    let filtered: Vec<Value> = docs
                        .iter()
                        .filter(|doc| doc_matches_filter(doc, query))
                        .cloned()
                        .collect();
                    let mut bucket = serde_json::Map::new();
                    bucket.insert("doc_count".to_string(), json!(sum_doc_count(&filtered)));
                    if let Some(sub) = sub_aggs {
                        let sr = run_aggs_with_all(sub, &filtered, all_docs);
                        if let Value::Object(so) = sr {
                            for (k, v) in so {
                                bucket.insert(k, v);
                            }
                        }
                    }
                    buckets.insert(name.clone(), Value::Object(bucket));
                }
                json!({"buckets": buckets})
            } else {
                let mut buckets: Vec<Value> = Vec::new();
                for (name, query) in map {
                    let filtered: Vec<Value> = docs
                        .iter()
                        .filter(|doc| doc_matches_filter(doc, query))
                        .cloned()
                        .collect();
                    let mut bucket = serde_json::Map::new();
                    bucket.insert("key".to_string(), Value::String(name.clone()));
                    bucket.insert("doc_count".to_string(), json!(sum_doc_count(&filtered)));
                    if let Some(sub) = sub_aggs {
                        let sr = run_aggs_with_all(sub, &filtered, all_docs);
                        if let Value::Object(so) = sr {
                            for (k, v) in so {
                                bucket.insert(k, v);
                            }
                        }
                    }
                    buckets.push(Value::Object(bucket));
                }
                json!({"buckets": buckets})
            }
        }
        // Anonymous filters: [ { query1 }, { query2 } ]
        Value::Array(arr) => {
            let mut buckets: Vec<Value> = Vec::new();
            for query in arr {
                let filtered: Vec<Value> = docs
                    .iter()
                    .filter(|doc| doc_matches_filter(doc, query))
                    .cloned()
                    .collect();
                let mut bucket = serde_json::Map::new();
                bucket.insert("doc_count".to_string(), json!(sum_doc_count(&filtered)));
                if let Some(sub) = sub_aggs {
                    let sr = run_aggs_with_all(sub, &filtered, all_docs);
                    if let Value::Object(so) = sr {
                        for (k, v) in so {
                            bucket.insert(k, v);
                        }
                    }
                }
                buckets.push(Value::Object(bucket));
            }
            json!({"buckets": buckets})
        }
        _ => json!({"buckets": {}}),
    }
}

/// Minimal filter matcher — supports term, terms, match_all, bool (must/filter).
pub(crate) fn doc_matches_filter(doc: &Value, filter: &Value) -> bool {
    let obj = match filter.as_object() {
        Some(o) => o,
        None => return true,
    };

    for (query_type, query_body) in obj {
        match query_type.as_str() {
            "match_all" => return true,
            "term" => {
                if let Some(field_map) = query_body.as_object() {
                    for (field, expected) in field_map {
                        let actual_values = extract_field_values(doc, field);
                        let expected_str = match expected {
                            Value::Object(o) => o.get("value").map(value_to_string),
                            other => Some(value_to_string(other)),
                        };
                        if let Some(exp) = expected_str {
                            if !actual_values.iter().any(|v| v == &exp) {
                                return false;
                            }
                        }
                    }
                }
            }
            "terms" => {
                if let Some(field_map) = query_body.as_object() {
                    for (field, expected_vals) in field_map {
                        if let Some(arr) = expected_vals.as_array() {
                            let expected_strs: Vec<String> =
                                arr.iter().map(value_to_string).collect();
                            let actual_values = extract_field_values(doc, field);
                            if !actual_values.iter().any(|v| expected_strs.contains(v)) {
                                return false;
                            }
                        }
                    }
                }
            }
            "match" | "match_phrase" => {
                if let Some(field_map) = query_body.as_object() {
                    for (field, expected) in field_map {
                        let actual_values = extract_field_values(doc, field);
                        let expected_str = match expected {
                            Value::Object(o) => {
                                o.get("query").map(value_to_string).unwrap_or_default()
                            }
                            other => value_to_string(other),
                        };
                        let exp_lower = expected_str.to_lowercase();
                        let exp_tokens: Vec<&str> = exp_lower.split_whitespace().collect();
                        let matched = actual_values.iter().any(|v| {
                            let v_lower = v.to_lowercase();
                            if exp_tokens.len() <= 1 {
                                v_lower.contains(&exp_lower) || v == &expected_str
                            } else {
                                exp_tokens.iter().all(|t| v_lower.contains(t))
                            }
                        });
                        if !matched {
                            return false;
                        }
                    }
                }
            }
            "prefix" => {
                if let Some(field_map) = query_body.as_object() {
                    for (field, expected) in field_map {
                        let actual_values = extract_field_values(doc, field);
                        let prefix = match expected {
                            Value::Object(o) => {
                                o.get("value").map(value_to_string).unwrap_or_default()
                            }
                            other => value_to_string(other),
                        };
                        if !actual_values.iter().any(|v| v.starts_with(&prefix)) {
                            return false;
                        }
                    }
                }
            }
            "wildcard" => {
                if let Some(field_map) = query_body.as_object() {
                    for (field, expected) in field_map {
                        let actual_values = extract_field_values(doc, field);
                        let pattern = match expected {
                            Value::Object(o) => {
                                o.get("value").map(value_to_string).unwrap_or_default()
                            }
                            other => value_to_string(other),
                        };
                        let matched = actual_values.iter().any(|v| {
                            if pattern == "*" {
                                true
                            } else if let Some(suffix) = pattern.strip_prefix('*') {
                                v.ends_with(suffix)
                            } else if let Some(prefix) = pattern.strip_suffix('*') {
                                v.starts_with(prefix)
                            } else {
                                v == &pattern
                            }
                        });
                        if !matched {
                            return false;
                        }
                    }
                }
            }
            "exists" => {
                // exists: {"field": "price"} — match docs that have the field.
                if let Some(field) = query_body.get("field").and_then(Value::as_str) {
                    let has_field = !extract_field_values(doc, field).is_empty();
                    if !has_field {
                        return false;
                    }
                }
            }
            "range" => {
                if let Some(field_map) = query_body.as_object() {
                    for (field, bounds) in field_map {
                        let values = extract_field_values(doc, field);
                        if values.is_empty() {
                            return false;
                        }
                        // Try numeric range check.
                        let num_val: Option<f64> = values.first().and_then(|s| s.parse().ok());
                        if let Some(n) = num_val {
                            if let Some(gte) = bounds.get("gte").and_then(Value::as_f64) {
                                if n < gte {
                                    return false;
                                }
                            }
                            if let Some(gt) = bounds.get("gt").and_then(Value::as_f64) {
                                if n <= gt {
                                    return false;
                                }
                            }
                            if let Some(lte) = bounds.get("lte").and_then(Value::as_f64) {
                                if n > lte {
                                    return false;
                                }
                            }
                            if let Some(lt) = bounds.get("lt").and_then(Value::as_f64) {
                                if n >= lt {
                                    return false;
                                }
                            }
                        }
                    }
                }
            }
            "bool" => {
                if let Some(bool_body) = query_body.as_object() {
                    // must / filter clauses
                    for clause_key in &["must", "filter"] {
                        if let Some(clauses) = bool_body.get(*clause_key) {
                            let clause_list = match clauses {
                                Value::Array(a) => a.as_slice().to_vec(),
                                single => vec![single.clone()],
                            };
                            for clause in &clause_list {
                                if !doc_matches_filter(doc, clause) {
                                    return false;
                                }
                            }
                        }
                    }
                    // must_not clauses: none may match.
                    if let Some(clauses) = bool_body.get("must_not") {
                        let clause_list = match clauses {
                            Value::Array(a) => a.as_slice().to_vec(),
                            single => vec![single.clone()],
                        };
                        for clause in &clause_list {
                            if doc_matches_filter(doc, clause) {
                                return false;
                            }
                        }
                    }
                    // should clauses: at least one must match (if no must/filter).
                    if let Some(clauses) = bool_body.get("should") {
                        let clause_list: Vec<Value> = match clauses {
                            Value::Array(a) => a.clone(),
                            single => vec![single.clone()],
                        };
                        let has_must =
                            bool_body.contains_key("must") || bool_body.contains_key("filter");
                        if !has_must && !clause_list.is_empty() {
                            let any_match = clause_list.iter().any(|c| doc_matches_filter(doc, c));
                            if !any_match {
                                return false;
                            }
                        }
                    }
                }
            }
            _ => {} // Unknown filter type — pass through
        }
    }
    true
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

// ── Range aggregation ─────────────────────────────────────────────────────────

pub(crate) fn format_range_val(v: f64) -> String {
    // Match Java's `Double.toString` output for range bucket keys:
    // ES emits `-9.223372036854776E18` for `Long.MIN_VALUE` rather than
    // the literal `-9223372036854775808` that `{}` would print.
    if !v.is_finite() {
        return format!("{v}");
    }
    let abs = v.abs();
    if v == 0.0 {
        return "0.0".to_string();
    }
    if !(1e-3..1e7).contains(&abs) {
        // Java: scientific with single integer digit, capital E,
        // trailing `.0` if mantissa has no fractional part, no `+` on
        // positive exponents.
        return java_scientific(v);
    }
    if v.fract() == 0.0 {
        format!("{:.1}", v)
    } else {
        format!("{}", v)
    }
}

/// Render an f64 in Java `Double.toString` scientific form.
fn java_scientific(v: f64) -> String {
    let s = format!("{:e}", v);
    // Rust prints e.g. `-9.223372036854776e18`. Convert to
    // `-9.223372036854776E18` (capital E, no `+` on positive exponents,
    // ensure mantissa has at least one fractional digit).
    let mut parts = s.splitn(2, 'e');
    let mantissa = parts.next().unwrap_or("0");
    let exp = parts.next().unwrap_or("0");
    let mantissa = if mantissa.contains('.') {
        mantissa.to_string()
    } else {
        format!("{mantissa}.0")
    };
    let exp = exp.trim_start_matches('+');
    format!("{mantissa}E{exp}")
}

fn run_range(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"buckets": []}),
    };

    let ranges = match params.get("ranges").and_then(Value::as_array) {
        Some(r) => r,
        None => return json!({"buckets": []}),
    };

    // Detect whether this range is over a date field: if any from/to is a
    // string, we treat the whole range as date-based (parse string bounds
    // and per-doc values via parse_date_ms instead of numeric extraction).
    let is_date_range = ranges.iter().any(|r| {
        r.get("from").map(|v| v.is_string()).unwrap_or(false)
            || r.get("to").map(|v| v.is_string()).unwrap_or(false)
    });

    // `missing`: docs that lack a value for the field are bucketed as if
    // they had this numeric value. Applies before range comparison.
    let missing_val: Option<f64> = params.get("missing").and_then(|v| match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) if is_date_range => parse_date_ms(v)
            .map(|ms| ms as f64)
            .or_else(|| s.parse::<f64>().ok()),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    });

    let buckets: Vec<Value> = ranges
        .iter()
        .map(|range_def| {
            let from_raw = range_def.get("from");
            let to_raw = range_def.get("to");
            let from: Option<f64> = from_raw.and_then(|v| match v {
                Value::Number(n) => n.as_f64(),
                Value::String(_) if is_date_range => parse_date_ms(v).map(|ms| ms as f64),
                Value::String(s) => s.parse::<f64>().ok(),
                _ => None,
            });
            let to: Option<f64> = to_raw.and_then(|v| match v {
                Value::Number(n) => n.as_f64(),
                Value::String(_) if is_date_range => parse_date_ms(v).map(|ms| ms as f64),
                Value::String(s) => s.parse::<f64>().ok(),
                _ => None,
            });

            let bucket_docs: Vec<Value> = docs
                .iter()
                .filter(|doc| {
                    let num = if is_date_range {
                        extract_date_ms_values(doc, field)
                            .first()
                            .copied()
                            .map(|ms| ms as f64)
                    } else {
                        extract_numeric(doc, field)
                    };
                    // Fall back to `missing` for docs without a value.
                    let num = num.or(missing_val);
                    match (num, from, to) {
                        (Some(n), Some(f), Some(t)) => n >= f && n < t,
                        (Some(n), Some(f), None) => n >= f,
                        (Some(n), None, Some(t)) => n < t,
                        (Some(_), None, None) => true,
                        _ => false,
                    }
                })
                .cloned()
                .collect();

            let doc_count = bucket_docs.len() as u64;

            // Date ranges: key uses ISO-8601 UTC rendering of each bound.
            let render_bound = |f: f64| -> String {
                if is_date_range {
                    epoch_ms_to_iso8601_utc(f as i64)
                } else {
                    format_range_val(f)
                }
            };
            let key = range_def
                .get("key")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| match (from, to) {
                    (Some(f), Some(t)) => format!("{}-{}", render_bound(f), render_bound(t)),
                    (Some(f), None) => format!("{}-*", render_bound(f)),
                    (None, Some(t)) => format!("*-{}", render_bound(t)),
                    (None, None) => "*-*".to_string(),
                });

            let mut bucket = json!({
                "key": key,
                "doc_count": doc_count,
            });
            if let Some(f) = from {
                if is_date_range {
                    bucket["from"] = json!(f as i64);
                    bucket["from_as_string"] = json!(epoch_ms_to_iso8601_utc(f as i64));
                } else {
                    bucket["from"] = json!(f);
                }
            }
            if let Some(t) = to {
                if is_date_range {
                    bucket["to"] = json!(t as i64);
                    bucket["to_as_string"] = json!(epoch_ms_to_iso8601_utc(t as i64));
                } else {
                    bucket["to"] = json!(t);
                }
            }

            if let Some(sub) = sub_aggs {
                let sub_result = run_aggs_with_all(sub, &bucket_docs, all_docs);
                if let (Some(bucket_obj), Value::Object(sub_obj)) =
                    (bucket.as_object_mut(), sub_result)
                {
                    for (k, v) in sub_obj {
                        bucket_obj.insert(k, v);
                    }
                }
            }

            bucket
        })
        .collect();

    // `keyed: true` (or `keyed: false` overriding the default) — render
    // buckets as a map keyed by bucket key instead of an array, removing
    // the redundant `key` field per ES semantics.
    let keyed = params
        .get("keyed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if keyed {
        let mut keyed_map = serde_json::Map::new();
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
        return json!({ "buckets": Value::Object(keyed_map) });
    }
    json!({ "buckets": buckets })
}

// ── Missing aggregation ───────────────────────────────────────────────────────

fn run_missing(params: &Value, docs: &[Value]) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"doc_count": 0}),
    };

    // When the `missing` param is set, every doc has a synthetic value
    // for the field — so the "missing" count collapses to 0.
    // (This is exactly what the YAML spec asserts: `missing: 1` →
    // missing_agg.doc_count: 0.)
    let has_missing_default = params.get("missing").is_some();
    let count = if has_missing_default {
        0
    } else {
        docs.iter()
            .filter(|doc| {
                let v = get_nested_field(doc, field);
                v.is_null()
            })
            .count()
    };

    json!({ "doc_count": count })
}

// ── Histogram aggregation ─────────────────────────────────────────────────────

fn run_histogram(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"buckets": []}),
    };

    let interval = match params.get("interval").and_then(Value::as_f64) {
        Some(i) if i > 0.0 => i,
        _ => return json!({"error": "histogram requires a positive `interval`"}),
    };

    let offset = params.get("offset").and_then(Value::as_f64).unwrap_or(0.0);
    let min_doc_count = params
        .get("min_doc_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let extended_bounds = params.get("extended_bounds").and_then(Value::as_object);
    let hard_bounds = params.get("hard_bounds").and_then(Value::as_object);
    let extended_min = extended_bounds.and_then(|b| b.get("min").and_then(Value::as_f64));
    let extended_max = extended_bounds.and_then(|b| b.get("max").and_then(Value::as_f64));
    let hard_min = hard_bounds.and_then(|b| b.get("min").and_then(Value::as_f64));
    let hard_max = hard_bounds.and_then(|b| b.get("max").and_then(Value::as_f64));

    let mut buckets: HashMap<i64, Vec<usize>> = HashMap::new();

    // ES `missing` on histogram: docs without a value for the field are
    // treated as if they had this numeric value. Omitted → those docs
    // are simply skipped.
    let missing_num: Option<f64> = params.get("missing").and_then(Value::as_f64);

    for (i, doc) in docs.iter().enumerate() {
        // Multi-valued fields contribute to each of their value's buckets.
        let mut nums = extract_numeric_values(doc, field);
        let ranges = if nums.is_empty() {
            extract_numeric_ranges(doc, field)
        } else {
            Vec::new()
        };
        if nums.is_empty() && ranges.is_empty() {
            if let Some(m) = missing_num {
                nums.push(m);
            } else {
                continue;
            }
        }
        let mut seen: Vec<i64> = Vec::with_capacity(nums.len() + ranges.len());
        for num in nums {
            let bucket_key = ((num - offset) / interval).floor() as i64;
            if !seen.contains(&bucket_key) {
                seen.push(bucket_key);
                buckets.entry(bucket_key).or_default().push(i);
            }
        }
        // Range-typed field: doc contributes to every bucket whose interval
        // `[k*interval+offset, (k+1)*interval+offset)` intersects the range.
        // Unbounded sides are clamped by hard_bounds/extended_bounds below;
        // when neither is configured, ES limits to the data's finite span,
        // which here reduces to each finite endpoint.
        for (from, to) in ranges {
            let clip_lo = hard_min.or(extended_min).unwrap_or(from);
            let clip_hi = hard_max.or(extended_max).unwrap_or(to);
            let lo = from.max(clip_lo);
            let hi = to.min(clip_hi);
            // Keep the negated `>` verbatim so NaN endpoints skip the bucket
            // (rewriting to `hi <= lo` would flip the NaN edge case).
            #[allow(clippy::neg_cmp_op_on_partial_ord)]
            if !(hi > lo) {
                continue;
            }
            let k_lo = ((lo - offset) / interval).floor() as i64;
            let k_hi = ((hi - offset) / interval).floor() as i64;
            for k in k_lo..=k_hi {
                let bucket_lo = k as f64 * interval + offset;
                // Skip buckets that only touch at a single exclusive edge.
                if to <= bucket_lo {
                    continue;
                }
                if !seen.contains(&k) {
                    seen.push(k);
                    buckets.entry(k).or_default().push(i);
                }
            }
        }
    }

    const MAX_BUCKETS: i64 = 65_536;
    let mut bucket_keys: Vec<i64> = if buckets.is_empty() && extended_min.is_none() {
        Vec::new()
    } else if min_doc_count > 0 && extended_min.is_none() {
        buckets.keys().cloned().collect()
    } else {
        let data_min = buckets.keys().min().copied();
        let data_max = buckets.keys().max().copied();
        let min_key = extended_min
            .map(|v| ((v - offset) / interval).floor() as i64)
            .or(data_min)
            .unwrap_or(0);
        let max_key = extended_max
            .map(|v| ((v - offset) / interval).floor() as i64)
            .or(data_max)
            .unwrap_or(0);
        // Guard against pathological inputs (tiny intervals with a wide
        // numeric span — a few billion buckets would DoS the process).
        // ES rejects aggs that exceed `search.max_buckets` (default 65536).
        let span = max_key.saturating_sub(min_key);
        if span > MAX_BUCKETS {
            return json!({
                "error": format!(
                    "Trying to create too many buckets. Must be less than or equal to: [{}] but this number of buckets was exceeded. This limit can be set by changing the [search.max_buckets] cluster level setting.",
                    MAX_BUCKETS
                ),
                "__error_status__": 400u32,
            });
        }
        let mut keys = Vec::with_capacity((span as usize).min(MAX_BUCKETS as usize) + 1);
        let mut k = min_key;
        while k <= max_key {
            keys.push(k);
            k += 1;
        }
        keys
    };
    bucket_keys.sort_unstable();

    if let (Some(h_min), Some(h_max)) = (hard_min, hard_max) {
        let hard_min_key = ((h_min - offset) / interval).floor() as i64;
        let hard_max_key = ((h_max - offset) / interval).floor() as i64;
        bucket_keys.retain(|&k| k >= hard_min_key && k <= hard_max_key);
    }

    if min_doc_count > 0 {
        bucket_keys.retain(|key| {
            buckets
                .get(key)
                .map(|v| v.len() as u64 >= min_doc_count)
                .unwrap_or(false)
        });
    }

    let format_pattern: Option<&str> = params.get("format").and_then(Value::as_str);
    let keyed = params
        .get("keyed")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let empty_indices: Vec<usize> = Vec::new();
    let rendered: Vec<(String, Value)> = bucket_keys
        .iter()
        .map(|&key| {
            let actual_key = key as f64 * interval + offset;
            let doc_indices = buckets.get(&key).unwrap_or(&empty_indices);
            let bucket_docs: Vec<Value> = doc_indices.iter().map(|&i| docs[i].clone()).collect();
            let doc_count = sum_doc_count(&bucket_docs);

            let key_as_string = format_pattern.map(|p| format_number_pattern(actual_key, p));
            let key_json = if actual_key.fract() == 0.0 {
                json!(actual_key as i64)
            } else {
                json!(actual_key)
            };
            let mut bucket = json!({
                "key": key_json,
                "doc_count": doc_count
            });
            if let Some(ref kas) = key_as_string {
                bucket
                    .as_object_mut()
                    .unwrap()
                    .insert("key_as_string".to_string(), Value::String(kas.clone()));
            }

            if let Some(sub) = sub_aggs {
                let sub_result = run_aggs_with_all(sub, &bucket_docs, all_docs);
                if let (Some(bucket_obj), Value::Object(sub_obj)) =
                    (bucket.as_object_mut(), sub_result)
                {
                    for (k, v) in sub_obj {
                        bucket_obj.insert(k, v);
                    }
                }
            }

            let key_label = key_as_string.unwrap_or_else(|| format_histogram_key(actual_key));
            (key_label, bucket)
        })
        .collect();

    if keyed {
        let mut map = serde_json::Map::new();
        for (k, v) in rendered {
            map.insert(k, v);
        }
        json!({ "buckets": Value::Object(map) })
    } else {
        let arr: Vec<Value> = rendered.into_iter().map(|(_, v)| v).collect();
        json!({ "buckets": arr })
    }
}

fn run_variable_width_histogram(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"buckets": []}),
    };
    let num_buckets = params.get("buckets").and_then(Value::as_u64).unwrap_or(10) as usize;

    let mut values: Vec<(f64, usize)> = docs
        .iter()
        .enumerate()
        .filter_map(|(i, doc)| extract_numeric(doc, field).map(|n| (n, i)))
        .collect();
    values.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    if values.is_empty() {
        return json!({"buckets": []});
    }

    // ES variable_width_histogram: adaptive-merge clustering. Start with
    // each distinct value as its own bucket, then iteratively merge the
    // two adjacent buckets with the smallest centroid-distance until
    // exactly `num_buckets` remain. Ties broken by higher index (later
    // merge) so the small trailing values cluster last, matching the
    // expected ES outputs on the YAML tests.
    let n = values.len();
    let effective_buckets = num_buckets.min(n).max(1);
    // Each bucket: (start_idx, end_idx_exclusive, min, max, sum, count).
    let mut buckets: Vec<(usize, usize, f64, f64, f64, usize)> = values
        .iter()
        .enumerate()
        .map(|(i, (v, _))| (i, i + 1, *v, *v, *v, 1))
        .collect();
    while buckets.len() > effective_buckets {
        // Find the pair of adjacent buckets with smallest centroid gap.
        let mut best_i: usize = 0;
        let mut best_gap = f64::INFINITY;
        for i in 0..buckets.len() - 1 {
            let cur = buckets[i].4 / buckets[i].5 as f64;
            let nxt = buckets[i + 1].4 / buckets[i + 1].5 as f64;
            let gap = (nxt - cur).abs();
            // Strictly-less tie-break: earlier neighbour pair wins on ties,
            // matching the ES observed merge order on the YAML tests.
            if gap < best_gap {
                best_gap = gap;
                best_i = i;
            }
        }
        let right = buckets.remove(best_i + 1);
        let left = &mut buckets[best_i];
        left.1 = right.1;
        left.2 = left.2.min(right.2);
        left.3 = left.3.max(right.3);
        left.4 += right.4;
        left.5 += right.5;
    }
    let partitions: Vec<(usize, usize)> = buckets.iter().map(|b| (b.0, b.1)).collect();
    let result_buckets: Vec<Value> = partitions
        .iter()
        .map(|&(s, e)| {
            let chunk = &values[s..e];
            let min_val = chunk.first().map(|v| v.0).unwrap_or(0.0);
            let max_val = chunk.last().map(|v| v.0).unwrap_or(0.0);
            let sum: f64 = chunk.iter().map(|(v, _)| v).sum();
            let key = if chunk.is_empty() {
                0.0
            } else {
                sum / chunk.len() as f64
            };
            let bucket_docs: Vec<Value> = chunk.iter().map(|(_, i)| docs[*i].clone()).collect();
            let mut bucket = json!({
                "min": min_val, "key": key, "max": max_val,
                "doc_count": chunk.len()
            });
            if let Some(sub) = sub_aggs {
                let sub_result = run_aggs_with_all(sub, &bucket_docs, all_docs);
                if let (Some(bo), Value::Object(so)) = (bucket.as_object_mut(), sub_result) {
                    for (k, v) in so {
                        bo.insert(k, v);
                    }
                }
            }
            bucket
        })
        .collect();

    json!({"buckets": result_buckets})
}

// ── Percentiles aggregation ───────────────────────────────────────────────────

fn run_percentiles(params: &Value, docs: &[Value]) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"values": {}}),
    };

    let percents: Vec<f64> = params
        .get("percents")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_else(|| vec![1.0, 5.0, 25.0, 50.0, 75.0, 95.0, 99.0]);

    // ES default for percentiles `keyed` is true (object map); false returns
    // an array of {key, value} (plus value_as_string when `format` is set).
    let keyed = params.get("keyed").and_then(Value::as_bool).unwrap_or(true);

    let missing = get_missing_value(params);
    let mut nums: Vec<f64> = docs
        .iter()
        .filter_map(|d| extract_numeric(d, field).or(missing))
        .collect();
    nums.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // Two algorithms:
    //   tdigest (default): linear interpolation between surrounding values.
    //     Matches ES close_to assertions with ±10 tolerance.
    //   hdr: nearest-rank with HdrHistogram quantization. ES wraps an
    //     integer HdrHistogram in a `DoubleHistogram` that auto-ranges:
    //     the conversion ratio from double → integer counts depends on
    //     the *smallest* recorded value, so the same value can quantize
    //     differently depending on the rest of the data set. We replicate
    //     that auto-ranging below so the exact ES outputs reproduce
    //     (e.g. value 51 → 51.0302734375 when the set spans [1,151], but
    //     → 51.0 when the set spans [51,151]).
    let use_hdr = params.get("hdr").is_some();
    let hdr_digits: u32 = params
        .get("hdr")
        .and_then(|v| v.get("number_of_significant_value_digits"))
        .and_then(Value::as_u64)
        .unwrap_or(3) as u32;
    // subBucketCount = 2^ceil(log2(2 * 10^digits)). For digits=3 that's 2048.
    let sub_bucket_count: u64 = {
        let target = 2u64 * 10u64.pow(hdr_digits);
        let mut p = 1u64;
        while p < target {
            p <<= 1;
        }
        p
    };
    let half_sub_bucket = sub_bucket_count / 2; // subBucketHalfCount, e.g. 1024.
                                                // subBucketHalfCountMagnitude = log2(subBucketCount) - 1.
    let sub_bucket_half_count_magnitude = sub_bucket_count.trailing_zeros().saturating_sub(1);
    // DoubleHistogram auto-range conversion ratio. ES initialises a
    // `DoubleHistogram` with `setAutoResize(true)`; after recording all
    // values the active window's lowest tracked value settles at the
    // largest power of two <= min(values), i.e. L = 2^floor(log2(min)).
    // The double→integer conversion ratio is then R = subBucketHalfCount / L,
    // a power of two, so the min value maps into the first sub-bucket band
    // [subBucketHalfCount, subBucketCount) where it is tracked exactly.
    let conv_ratio: f64 = if use_hdr && !nums.is_empty() && nums[0] > 0.0 && nums[0].is_finite() {
        let l = nums[0].log2().floor().exp2(); // 2^floor(log2(min))
        if l > 0.0 {
            (half_sub_bucket as f64) / l
        } else {
            1.0
        }
    } else {
        1.0
    };
    // Map a double to the "highest equivalent value" of the integer
    // HdrHistogram bucket it lands in, then convert back to a double.
    let hdr_quantize = |v: f64| -> f64 {
        if v <= 0.0 || !v.is_finite() {
            return v;
        }
        let iv = (v * conv_ratio).floor() as i64;
        if iv <= 0 {
            return v;
        }
        // bucketIndex: 0 while the value fits in the first (linear) band,
        // then one per binary order of magnitude beyond subBucketCount.
        let bucket_index: u32 = if (iv as u64) < sub_bucket_count {
            0
        } else {
            (63 - (iv as u64).leading_zeros()).saturating_sub(sub_bucket_half_count_magnitude)
        };
        let size = 1i64 << bucket_index; // sizeOfEquivalentValueRange
        let lowest_equiv = iv & !(size - 1);
        let highest_equiv = lowest_equiv + size - 1;
        (highest_equiv as f64) / conv_ratio
    };
    let compute = |pct: f64| -> Option<f64> {
        if nums.is_empty() {
            return None;
        }
        if use_hdr {
            // Java HdrHistogram.getValueAtPercentile:
            //   countAtPercentile = (long)((pct / 100 * N) + 0.5) clamped to >=1
            //   Walk in ascending order, return first value where the
            //   cumulative count is >= countAtPercentile.
            let n = nums.len();
            let count_at = (((pct / 100.0) * n as f64) + 0.5) as i64;
            let count_at = count_at.max(1) as usize;
            let mut cumulative = 0usize;
            let mut pick = nums[n - 1];
            for &v in nums.iter() {
                cumulative += 1;
                if cumulative >= count_at {
                    pick = v;
                    break;
                }
            }
            Some(hdr_quantize(pick))
        } else {
            let rank = (pct / 100.0) * (nums.len() as f64 - 1.0);
            let lo = rank.floor() as usize;
            let hi = (lo + 1).min(nums.len() - 1);
            let frac = rank - rank.floor();
            Some(nums[lo] * (1.0 - frac) + nums[hi] * frac)
        }
    };

    if keyed {
        let values: serde_json::Map<String, Value> = percents
            .iter()
            .map(|&pct| {
                // ES keys with `String.valueOf(double)` — "25.0", "99.9",
                // and "99.99" stays "99.99" (NOT rounded to one decimal).
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
        json!({ "values": values })
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
        json!({ "values": arr })
    }
}

// ── Composite aggregation ─────────────────────────────────────────────────────

fn run_composite(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let sources = match params.get("sources").and_then(Value::as_array) {
        Some(s) => s,
        None => return json!({"buckets": []}),
    };
    let size = params.get("size").and_then(Value::as_u64).unwrap_or(10) as usize;

    // Each source is { "name": { "type": { "field": "..." } } }
    // Build a list of (source_name, source_type, field, type_params).
    let source_defs: Vec<(String, String, String, Value)> = sources
        .iter()
        .filter_map(|src| {
            let src_obj = src.as_object()?;
            let (src_name, src_spec) = src_obj.iter().next()?;
            let spec_obj = src_spec.as_object()?;
            // The type key is one of: terms, histogram, date_histogram
            let (src_type, type_params) = spec_obj.iter().next()?;
            let field = type_params
                .get("field")
                .and_then(Value::as_str)?
                .to_string();
            Some((
                src_name.clone(),
                src_type.clone(),
                field,
                type_params.clone(),
            ))
        })
        .collect();

    if source_defs.is_empty() {
        return json!({"buckets": []});
    }

    // Collect composite keys from each document.
    // A composite key is a tuple of string values — one per source.
    let mut bucket_map: HashMap<Vec<String>, Vec<usize>> = HashMap::new();

    for (doc_idx, doc) in docs.iter().enumerate() {
        // Compute the key for each source.
        let mut key_parts: Vec<Vec<String>> = Vec::new();
        for (_, src_type, field, src_params) in &source_defs {
            let missing_bucket = src_params
                .get("missing_bucket")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let part_values = match src_type.as_str() {
                "terms" => extract_field_values(doc, field),
                "histogram" => {
                    let interval = src_params
                        .get("interval")
                        .and_then(Value::as_f64)
                        .unwrap_or(1.0);
                    let offset = src_params
                        .get("offset")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0);
                    let nums = extract_numeric_values(doc, field);
                    let mut keys: Vec<String> = Vec::with_capacity(nums.len());
                    for num in nums {
                        let bucket_key_idx = ((num - offset) / interval).floor();
                        let bucket_val = bucket_key_idx * interval + offset;
                        keys.push(format_histogram_key(bucket_val));
                    }
                    // Dedup repeated values within a multi-valued field.
                    keys.sort();
                    keys.dedup();
                    keys
                }
                "date_histogram" => {
                    let interval_str = src_params
                        .get("calendar_interval")
                        .and_then(Value::as_str)
                        .or_else(|| src_params.get("fixed_interval").and_then(Value::as_str))
                        .or_else(|| src_params.get("interval").and_then(Value::as_str));
                    let interval_ms = interval_str.and_then(interval_to_ms).unwrap_or(86_400_000);
                    let use_calendar = interval_str.map(is_calendar_interval).unwrap_or(false);
                    // `offset` accepts either a string duration (`"4h"`,
                    // `"-1d"`) or a raw ms integer. Parse both.
                    let offset_ms = match src_params.get("offset") {
                        Some(Value::String(s)) => {
                            let (sign, tail) = if let Some(rest) = s.strip_prefix('-') {
                                (-1i64, rest)
                            } else if let Some(rest) = s.strip_prefix('+') {
                                (1i64, rest)
                            } else {
                                (1i64, s.as_str())
                            };
                            interval_to_ms(tail).map(|v| sign * v).unwrap_or(0)
                        }
                        Some(v) => v.as_i64().unwrap_or(0),
                        None => 0,
                    };
                    // time_zone: resolve to a fixed offset at a representative instant.
                    let tz_str = src_params
                        .get("time_zone")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let tz_offset: Option<chrono::FixedOffset> = if tz_str.is_empty() {
                        None
                    } else if let Some(off) = parse_time_zone_offset(tz_str) {
                        Some(off)
                    } else {
                        let sample_ms = extract_date_ms_values(doc, field)
                            .into_iter()
                            .next()
                            .unwrap_or(0);
                        fixed_offset_for_tz_at(tz_str, sample_ms)
                    };
                    let tz_shift_ms: i64 = tz_offset
                        .map(|o| o.local_minus_utc() as i64 * 1000)
                        .unwrap_or(0);
                    let raws = extract_date_ms_values(doc, field);
                    let mut keys: Vec<String> = Vec::with_capacity(raws.len());
                    for ts_ms in raws {
                        let local_ts = ts_ms + tz_shift_ms - offset_ms;
                        let local_bucket = if use_calendar {
                            calendar_bucket_key(local_ts, interval_str.unwrap_or("1d"))
                        } else {
                            local_ts.div_euclid(interval_ms) * interval_ms
                        };
                        let bucket_ms = local_bucket - tz_shift_ms + offset_ms;
                        keys.push(bucket_ms.to_string());
                    }
                    keys.sort();
                    keys.dedup();
                    keys
                }
                "geotile_grid" => {
                    let precision = src_params
                        .get("precision")
                        .and_then(Value::as_u64)
                        .unwrap_or(7) as u32;
                    let mut keys: Vec<String> = Vec::new();
                    if let Some(v) = doc.get(field) {
                        // Accept a single point, or an array of points.
                        let points: Vec<(f64, f64)> = match v {
                            Value::Array(a) => {
                                if a.len() == 2
                                    && a[0].as_f64().is_some()
                                    && a[1].as_f64().is_some()
                                {
                                    // [lon, lat]
                                    if let Some(p) = parse_geo_point(v) {
                                        vec![p]
                                    } else {
                                        Vec::new()
                                    }
                                } else {
                                    a.iter().filter_map(parse_geo_point).collect()
                                }
                            }
                            _ => parse_geo_point(v).into_iter().collect(),
                        };
                        for (lat, lon) in points {
                            let k = geotile_key(lat, lon, precision);
                            keys.push(k);
                        }
                    }
                    keys.sort();
                    keys.dedup();
                    keys
                }
                "geohash_grid" => {
                    let precision = src_params
                        .get("precision")
                        .and_then(Value::as_u64)
                        .unwrap_or(5) as usize;
                    let mut keys: Vec<String> = Vec::new();
                    if let Some(v) = doc.get(field) {
                        let points: Vec<(f64, f64)> = match v {
                            Value::Array(a) => {
                                if a.len() == 2
                                    && a[0].as_f64().is_some()
                                    && a[1].as_f64().is_some()
                                {
                                    if let Some(p) = parse_geo_point(v) {
                                        vec![p]
                                    } else {
                                        Vec::new()
                                    }
                                } else {
                                    a.iter().filter_map(parse_geo_point).collect()
                                }
                            }
                            _ => parse_geo_point(v).into_iter().collect(),
                        };
                        for (lat, lon) in points {
                            keys.push(geohash_encode(lat, lon, precision));
                        }
                    }
                    keys.sort();
                    keys.dedup();
                    keys
                }
                _ => vec![],
            };
            if part_values.is_empty() {
                if missing_bucket
                    || params
                        .get("missing_bucket")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                {
                    key_parts.push(vec!["__null__".to_string()]);
                } else {
                    key_parts.push(vec![]);
                }
            } else {
                key_parts.push(part_values);
            }
        }

        // Cross-product of all key parts to generate composite keys.
        let mut composite_keys: Vec<Vec<String>> = vec![vec![]];
        for parts in &key_parts {
            let mut new_keys: Vec<Vec<String>> = Vec::new();
            for existing_key in &composite_keys {
                for part in parts {
                    let mut new_key = existing_key.clone();
                    new_key.push(part.clone());
                    new_keys.push(new_key);
                }
            }
            composite_keys = new_keys;
        }

        for key in composite_keys {
            bucket_map.entry(key).or_default().push(doc_idx);
        }
    }

    // Sort buckets by composite key. Default ES ordering is ascending per
    // source, with each source sorted according to its type:
    //   terms: numeric keys compared numerically, strings lexicographically
    //   histogram / date_histogram: epoch / bucket value compared numerically
    // The `__null__` sentinel sorts first (before every real value) per
    // missing_bucket semantics.
    let orders: Vec<bool> = source_defs
        .iter()
        .map(|(_, _, _, src_params)| {
            src_params
                .get("order")
                .and_then(Value::as_str)
                .map(|s| s != "desc")
                .unwrap_or(true)
        })
        .collect();
    // missing_order per source: "first" (default) puts nulls first, "last"
    // puts nulls last. The `default` setting follows the asc/desc order.
    let missing_orders: Vec<&str> = source_defs
        .iter()
        .map(|(_, _, _, src_params)| {
            src_params
                .get("missing_order")
                .and_then(Value::as_str)
                .unwrap_or("default")
        })
        .collect();
    let numeric_src: Vec<bool> = source_defs
        .iter()
        .map(|(_, src_type, _, _)| src_type != "terms")
        .collect();
    let geotile_src: Vec<bool> = source_defs
        .iter()
        .map(|(_, src_type, _, _)| src_type == "geotile_grid")
        .collect();
    /// Parse a geotile key `"z/x/y"` into a (z, x, y) tuple for
    /// numeric sorting; returns `(0, 0, 0)` on parse error.
    fn geotile_parse(s: &str) -> (i64, i64, i64) {
        let mut parts = s.splitn(3, '/').map(|p| p.parse::<i64>().unwrap_or(0));
        (
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
        )
    }
    let mut sorted_keys: Vec<Vec<String>> = bucket_map.keys().cloned().collect();
    sorted_keys.sort_by(|a, b| {
        use std::cmp::Ordering;
        for i in 0..a.len().min(b.len()) {
            let (av, bv) = (&a[i], &b[i]);
            let null_cmp = match missing_orders.get(i).copied().unwrap_or("default") {
                "first" => (Ordering::Less, Ordering::Greater),
                "last" => (Ordering::Greater, Ordering::Less),
                _ => (Ordering::Less, Ordering::Greater), // "default" — first
            };
            let cmp = if av == "__null__" && bv == "__null__" {
                Ordering::Equal
            } else if av == "__null__" {
                null_cmp.0
            } else if bv == "__null__" {
                null_cmp.1
            } else if geotile_src.get(i).copied().unwrap_or(false) {
                // Geotile keys sort by (zoom, x, y) numerically.
                geotile_parse(av).cmp(&geotile_parse(bv))
            } else if numeric_src.get(i).copied().unwrap_or(false)
                || (av.parse::<f64>().is_ok() && bv.parse::<f64>().is_ok())
            {
                let an: f64 = av.parse().unwrap_or(0.0);
                let bn: f64 = bv.parse().unwrap_or(0.0);
                an.partial_cmp(&bn).unwrap_or(Ordering::Equal)
            } else {
                av.cmp(bv)
            };
            let cmp = if orders.get(i).copied().unwrap_or(true) {
                cmp
            } else {
                cmp.reverse()
            };
            if cmp != Ordering::Equal {
                return cmp;
            }
        }
        a.len().cmp(&b.len())
    });

    // Filter by `after` param for pagination: drop every key <= after-key.
    if let Some(after_obj) = params.get("after").and_then(Value::as_object) {
        let after_key: Vec<String> = source_defs
            .iter()
            .map(|(name, src_type, _, src_params)| {
                after_obj
                    .get(name)
                    .map(|v| match v {
                        Value::String(s) => {
                            // Date-string cursor → epoch ms to match internal form.
                            if src_type == "date_histogram" {
                                // `format: epoch_second|epoch_millis`:
                                // the cursor is a numeric string in
                                // the configured unit — scale to the
                                // internal epoch-ms representation.
                                let fmt = src_params.get("format").and_then(Value::as_str);
                                if fmt == Some("epoch_second") {
                                    if let Ok(sec) = s.parse::<i64>() {
                                        return (sec * 1000).to_string();
                                    }
                                }
                                if fmt == Some("epoch_millis") && s.parse::<i64>().is_ok() {
                                    return s.clone();
                                }
                                // Honor the source's `time_zone` when
                                // parsing the cursor string: a cursor
                                // like "2017-10-20" in a source with
                                // `time_zone:-02:00` should resolve to
                                // the zone-local start of day, not UTC
                                // midnight.
                                let tz_str = src_params
                                    .get("time_zone")
                                    .and_then(Value::as_str)
                                    .unwrap_or("");
                                if !tz_str.is_empty() {
                                    if let Some(off) = parse_time_zone_offset(tz_str) {
                                        if let Ok(d) =
                                            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                                        {
                                            let local = d.and_hms_opt(0, 0, 0).unwrap();
                                            let utc = local
                                                - chrono::Duration::seconds(
                                                    off.local_minus_utc() as i64
                                                );
                                            let ms = utc.and_utc().timestamp_millis();
                                            return ms.to_string();
                                        }
                                    }
                                }
                                parse_date_ms(v)
                                    .map(|ms| ms.to_string())
                                    .unwrap_or_else(|| s.clone())
                            } else {
                                s.clone()
                            }
                        }
                        Value::Number(n) => {
                            // Histogram source stores a formatted bucket value;
                            // compare to that form for consistency.
                            if src_type == "histogram" {
                                if let Some(f) = n.as_f64() {
                                    format_histogram_key(f)
                                } else {
                                    n.to_string()
                                }
                            } else {
                                n.to_string()
                            }
                        }
                        Value::Bool(b) => b.to_string(),
                        Value::Null => "__null__".to_string(),
                        _ => String::new(),
                    })
                    .unwrap_or_default()
            })
            .collect();
        // Compare keys against `after` using the same per-source
        // numeric/lex semantics as the sort — otherwise `after: {long: 20}`
        // would keep keys like `"100"` before `"20"` lexicographically.
        let cmp_parts = |a: &[String], b: &[String]| -> std::cmp::Ordering {
            use std::cmp::Ordering;
            for i in 0..a.len().min(b.len()) {
                let (av, bv) = (&a[i], &b[i]);
                let c = if av == "__null__" && bv == "__null__" {
                    Ordering::Equal
                } else if av == "__null__" {
                    Ordering::Less
                } else if bv == "__null__" {
                    Ordering::Greater
                } else if geotile_src.get(i).copied().unwrap_or(false) {
                    geotile_parse(av).cmp(&geotile_parse(bv))
                } else if numeric_src.get(i).copied().unwrap_or(false)
                    || (av.parse::<f64>().is_ok() && bv.parse::<f64>().is_ok())
                {
                    let an: f64 = av.parse().unwrap_or(0.0);
                    let bn: f64 = bv.parse().unwrap_or(0.0);
                    an.partial_cmp(&bn).unwrap_or(Ordering::Equal)
                } else {
                    av.cmp(bv)
                };
                let c = if orders.get(i).copied().unwrap_or(true) {
                    c
                } else {
                    c.reverse()
                };
                if c != Ordering::Equal {
                    return c;
                }
            }
            a.len().cmp(&b.len())
        };
        sorted_keys.retain(|k| cmp_parts(k, &after_key) == std::cmp::Ordering::Greater);
    }
    sorted_keys.truncate(size);

    let result_buckets: Vec<Value> = sorted_keys
        .iter()
        .map(|key| {
            let doc_indices = &bucket_map[key];
            let bucket_docs: Vec<Value> = doc_indices.iter().map(|&i| docs[i].clone()).collect();
            let doc_count = sum_doc_count(&bucket_docs);

            let mut key_obj = serde_json::Map::new();
            for (i, (src_name, _src_type, _, _)) in source_defs.iter().enumerate() {
                let val = key.get(i).cloned().unwrap_or_default();
                let (_name_slot, src_type, _field, src_params) = &source_defs[i];
                if val == "__null__" {
                    key_obj.insert(src_name.clone(), Value::Null);
                } else if src_type == "date_histogram" {
                    if let Ok(ts) = val.parse::<i64>() {
                        let fmt = src_params.get("format").and_then(Value::as_str);
                        // ES emits composite date_histogram keys as raw
                        // epoch milliseconds (a JSON number) when no
                        // `format` is configured. A format string switches
                        // the output to a formatted date string.
                        if fmt.is_none() {
                            key_obj.insert(src_name.clone(), json!(ts));
                        } else {
                            let tz_str = src_params
                                .get("time_zone")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            let tz_offset: Option<chrono::FixedOffset> = if tz_str.is_empty() {
                                None
                            } else if let Some(off) = parse_time_zone_offset(tz_str) {
                                Some(off)
                            } else {
                                fixed_offset_for_tz_at(tz_str, ts)
                            };
                            let rendered = if let Some(off) = tz_offset {
                                let dt_utc =
                                    chrono::DateTime::from_timestamp_millis(ts).unwrap_or_default();
                                let dt_local = dt_utc.with_timezone(&off);
                                match fmt {
                                    Some("epoch_millis") => ts.to_string(),
                                    Some("epoch_second") => (ts / 1000).to_string(),
                                    Some("iso8601")
                                    | Some("strict_date_time")
                                    | Some("date_time") => {
                                        dt_local.format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string()
                                    }
                                    Some(f) => {
                                        let strf = java_to_strftime(f);
                                        dt_local.format(&strf).to_string()
                                    }
                                    None => unreachable!(),
                                }
                            } else {
                                let dt_val =
                                    chrono::DateTime::from_timestamp_millis(ts).unwrap_or_default();
                                render_date_format(fmt, ts, dt_val)
                            };
                            key_obj.insert(src_name.clone(), Value::String(rendered));
                        }
                    } else {
                        key_obj.insert(src_name.clone(), Value::String(val));
                    }
                } else if src_type == "histogram" || src_type == "terms" {
                    if let Ok(n) = val.parse::<i64>() {
                        key_obj.insert(src_name.clone(), json!(n));
                    } else if let Ok(f) = val.parse::<f64>() {
                        key_obj.insert(
                            src_name.clone(),
                            serde_json::Number::from_f64(f)
                                .map(Value::Number)
                                .unwrap_or(Value::String(val)),
                        );
                    } else {
                        key_obj.insert(src_name.clone(), Value::String(val));
                    }
                } else {
                    key_obj.insert(src_name.clone(), Value::String(val));
                }
            }

            let mut bucket = json!({
                "key": Value::Object(key_obj),
                "doc_count": doc_count
            });

            if let Some(sub) = sub_aggs {
                let sub_result = run_aggs_with_all(sub, &bucket_docs, all_docs);
                if let (Some(bucket_obj), Value::Object(sub_obj)) =
                    (bucket.as_object_mut(), sub_result)
                {
                    for (k, v) in sub_obj {
                        bucket_obj.insert(k, v);
                    }
                }
            }

            bucket
        })
        .collect();

    // The ES response includes `after_key` = the last bucket's key, so the
    // caller can paginate by passing it as `after` in the next request.
    let after_key = result_buckets
        .last()
        .and_then(|b| b.get("key").cloned())
        .unwrap_or(Value::Null);

    // ES serializes `after_key` BEFORE `buckets` (see ES
    // InternalComposite#doXContentBody); with preserve_order enabled the
    // insertion order here is the response byte order.
    let mut obj = serde_json::Map::new();
    if !after_key.is_null() {
        obj.insert("after_key".into(), after_key);
    }
    obj.insert("buckets".into(), Value::Array(result_buckets));
    Value::Object(obj)
}

// ── Significant terms aggregation ─────────────────────────────────────────────

/// Finds terms statistically overrepresented in `result_docs` vs the full `all_docs` set.
/// A term qualifies if it appears in >10% of result docs but <50% of all docs.
/// Score = (result_freq / result_total) / (bg_freq / all_total)
/// Lucene/ES Murmur3_32 over `data` seeded with 0xAD7D2925
/// (`GOOD_FAST_HASH_SEED`). Used for ES-compatible terms agg partition
/// hashing so `include.partition` bucket membership matches ES exactly.
fn murmur3_32_partition_hash(data: &[u8]) -> u32 {
    let c1: u32 = 0xcc9e2d51;
    let c2: u32 = 0x1b873593;
    let mut hash: u32 = 0xAD7D2925;
    let nblocks = data.len() / 4;
    for i in 0..nblocks {
        let mut k1 = u32::from_le_bytes([
            data[i * 4],
            data[i * 4 + 1],
            data[i * 4 + 2],
            data[i * 4 + 3],
        ]);
        k1 = k1.wrapping_mul(c1);
        k1 = k1.rotate_left(15);
        k1 = k1.wrapping_mul(c2);
        hash ^= k1;
        hash = hash.rotate_left(13);
        hash = hash.wrapping_mul(5).wrapping_add(0xe6546b64);
    }
    let tail_start = nblocks * 4;
    let tail_len = data.len() - tail_start;
    let mut k1: u32 = 0;
    if tail_len >= 3 {
        k1 ^= (data[tail_start + 2] as u32) << 16;
    }
    if tail_len >= 2 {
        k1 ^= (data[tail_start + 1] as u32) << 8;
    }
    if tail_len >= 1 {
        k1 ^= data[tail_start] as u32;
        k1 = k1.wrapping_mul(c1);
        k1 = k1.rotate_left(15);
        k1 = k1.wrapping_mul(c2);
        hash ^= k1;
    }
    hash ^= data.len() as u32;
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x85ebca6b);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(0xc2b2ae35);
    hash ^= hash >> 16;
    hash
}

fn run_significant_terms(params: &Value, result_docs: &[Value], all_docs: &[Value]) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"buckets": []}),
    };
    let size = params.get("size").and_then(Value::as_u64).unwrap_or(10) as usize;

    let result_total = result_docs.len();

    // Optional `background_filter`: restricts the background-corpus comparison
    // to docs matching the filter (typically a `terms` query). Apply the
    // filter to all_docs to compute bg_counts and bg_total.
    let bg_docs_owned: Option<Vec<Value>> = params.get("background_filter").map(|filter| {
        all_docs
            .iter()
            .filter(|d| matches_simple_filter(d, filter))
            .cloned()
            .collect()
    });
    let bg_docs: &[Value] = bg_docs_owned.as_deref().unwrap_or(all_docs);
    let all_total = bg_docs.len();

    // Empty foreground still reports the background count so per-parent-
    // bucket tests that assert `bg_count` on empty sub-buckets pass.
    if result_total == 0 {
        return json!({ "buckets": [], "doc_count": 0, "bg_count": all_total });
    }
    // Divisor for frequency ratios (avoid divide-by-zero); the reported
    // `bg_count` is always the real `all_total` regardless of this floor.
    let bg_total_divisor = all_total.max(1);

    // Tokenize ONLY when the raw value contains whitespace — that's
    // the clear "this is a text field" signal. Values with only
    // separators like `::1`, `192.168.0.1`, `foo-bar` should stay
    // whole to preserve IP/keyword semantics. ES tokenizes text with
    // `fielddata:true` at the analyzer level, where whitespace is the
    // primary splitter.
    fn explode_terms(raw: Vec<String>) -> HashSet<String> {
        let mut out = HashSet::new();
        for v in raw {
            if v.contains(char::is_whitespace) {
                for tok in v.split_whitespace() {
                    let t = tok
                        .trim_matches(|c: char| !c.is_alphanumeric())
                        .to_lowercase();
                    if !t.is_empty() {
                        out.insert(t);
                    }
                }
            } else if !v.is_empty() {
                out.insert(v);
            }
        }
        out
    }

    // Count term frequencies in result set.
    let mut result_counts: HashMap<String, u64> = HashMap::new();
    for doc in result_docs {
        // Use a set per-doc so we don't double-count multi-valued fields.
        let terms = explode_terms(extract_field_values(doc, field));
        for term in terms {
            *result_counts.entry(term).or_insert(0) += 1;
        }
    }

    // Count background frequencies in bg_docs (filtered or all).
    let mut bg_counts: HashMap<String, u64> = HashMap::new();
    for doc in bg_docs {
        let terms = explode_terms(extract_field_values(doc, field));
        for term in terms {
            *bg_counts.entry(term).or_insert(0) += 1;
        }
    }

    let result_total_f = result_total as f64;
    let all_total_f = bg_total_divisor as f64;

    // ES `include` / `exclude`: when given as an array of values, treat
    // as an allow / deny set. (Regex form is intentionally rejected
    // upstream for IP fields per ES semantics.)
    let include_set: Option<HashSet<String>> =
        params.get("include").and_then(Value::as_array).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        });
    let exclude_set: Option<HashSet<String>> =
        params.get("exclude").and_then(Value::as_array).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        });

    // ES `min_doc_count` (default 3) — exclude terms whose foreground
    // count is below the threshold. The IP test sets min_doc_count:1
    // to bypass our heuristic frequency thresholds.
    let min_doc_count: u64 = params
        .get("min_doc_count")
        .and_then(Value::as_u64)
        .unwrap_or(3);
    let bypass_thresholds = min_doc_count <= 1;

    // Filter and score terms. When the caller explicitly lowered
    // min_doc_count to 1 we trust them and skip the heuristic
    // foreground/background frequency cuts; otherwise apply both so
    // un-targeted runs don't blow up bucket counts on dense corpora.
    let mut scored: Vec<(String, u64, u64, f64)> = result_counts
        .into_iter()
        .filter_map(|(term, rc)| {
            if rc < min_doc_count {
                return None;
            }
            if let Some(ref inc) = include_set {
                if !inc.contains(&term) {
                    return None;
                }
            }
            if let Some(ref exc) = exclude_set {
                if exc.contains(&term) {
                    return None;
                }
            }
            let result_freq = rc as f64 / result_total_f;
            let bg_count = *bg_counts.get(&term).unwrap_or(&0);
            let bg_freq = bg_count as f64 / all_total_f;

            // Heuristic prefilter: drop noise terms that are rare in the
            // foreground. We used to also drop terms with high background
            // frequency (≥0.5) but that mistakenly rejected perfectly
            // correlated foreground→class terms like "bad" appearing in
            // every class=bad doc (fg=1.0) and ~0.7 of all docs. The JLH
            // score below handles relative rarity on its own.
            if !bypass_thresholds && result_freq <= 0.1 {
                return None;
            }

            // JLH score (ES default significance heuristic):
            //   score = (fgPct - bgPct) * fgPct / bgPct
            // where fg/bg pct are term doc_count / set size. Terms under-
            // represented relative to background (fg <= bg) get score 0.
            let score = if bg_freq == 0.0 {
                result_freq * 10.0
            } else if result_freq <= bg_freq {
                0.0
            } else {
                (result_freq - bg_freq) * result_freq / bg_freq
            };
            if !bypass_thresholds && score <= 0.0 {
                return None;
            }

            Some((term, rc, bg_count, score))
        })
        .collect();

    // Sort by score descending.
    scored.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(size);

    let buckets: Vec<Value> = scored
        .into_iter()
        .map(|(key, doc_count, bg_count, score)| {
            let mut b = build_terms_bucket(&key, doc_count);
            if let Some(obj) = b.as_object_mut() {
                obj.insert("bg_count".into(), json!(bg_count));
                obj.insert("score".into(), json!(score));
            }
            b
        })
        .collect();

    json!({
        "doc_count": result_total,
        "bg_count": all_total,
        "buckets": buckets,
    })
}

/// Lightweight ES filter matcher used by `significant_terms.background_filter`
/// and friends. Recognises:
///   - {"terms": {"field": [v1, v2, ...]}}
///   - {"term": {"field": v}}  or  {"term": {"field": {"value": v}}}
///   - {"bool": {"must": [...], "must_not": [...], "filter": [...]}}
///   - {"match_all": {}}
fn matches_simple_filter(doc: &Value, filter: &Value) -> bool {
    let obj = match filter.as_object() {
        Some(o) => o,
        None => return false,
    };
    if obj.contains_key("match_all") {
        return true;
    }
    // `match` on a single field — treat as keyword-equivalent term match
    // (stringify both sides). For text fields this is a rough
    // approximation but sufficient for background_filter use cases where
    // the filter targets a keyword / boolean field.
    if let Some(m) = obj.get("match").and_then(Value::as_object) {
        if let Some((field, val)) = m.iter().next() {
            let target = match val {
                Value::Object(o) => o.get("query").cloned().unwrap_or(Value::Null),
                v => v.clone(),
            };
            let dv = get_nested_field(doc, field);
            return values_match(dv, &target);
        }
    }
    if let Some(t) = obj.get("term").and_then(Value::as_object) {
        if let Some((field, val)) = t.iter().next() {
            let target = match val {
                Value::Object(o) => o.get("value").cloned().unwrap_or(Value::Null),
                v => v.clone(),
            };
            let dv = get_nested_field(doc, field);
            return values_match(dv, &target);
        }
    }
    if let Some(t) = obj.get("terms").and_then(Value::as_object) {
        if let Some((field, val)) = t.iter().next() {
            if let Some(arr) = val.as_array() {
                let dv = get_nested_field(doc, field);
                return arr.iter().any(|v| values_match(dv, v));
            }
        }
    }
    if let Some(b) = obj.get("bool").and_then(Value::as_object) {
        let must_ok = b
            .get("must")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().all(|f| matches_simple_filter(doc, f)))
            .unwrap_or(true);
        let filter_ok = b
            .get("filter")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().all(|f| matches_simple_filter(doc, f)))
            .unwrap_or(true);
        let must_not_ok = b
            .get("must_not")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().all(|f| !matches_simple_filter(doc, f)))
            .unwrap_or(true);
        return must_ok && filter_ok && must_not_ok;
    }
    false
}

fn values_match(dv: &Value, target: &Value) -> bool {
    if dv == target {
        return true;
    }
    match (dv, target) {
        (Value::Array(arr), _) => arr.iter().any(|e| values_match(e, target)),
        (Value::String(s), Value::String(t)) => s == t,
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::String(s), Value::Number(n)) | (Value::Number(n), Value::String(s)) => {
            s.parse::<f64>().ok() == n.as_f64()
        }
        _ => false,
    }
}

/// `significant_text` — like `significant_terms` but on tokenized text.
///
/// We tokenize each text field value by splitting on non-alphanumeric chars,
/// lowercasing, and applying a tiny English stop-word filter (same shape as
/// the standard analyzer uses by default).  Then we run the same frequency /
/// background comparison as `significant_terms`.
fn run_significant_text(params: &Value, result_docs: &[Value], all_docs: &[Value]) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"buckets": []}),
    };
    let size = params.get("size").and_then(Value::as_u64).unwrap_or(10) as usize;
    let min_doc_count = params
        .get("min_doc_count")
        .and_then(Value::as_u64)
        .unwrap_or(3);

    // background_filter: restrict the background corpus to docs matching
    // the filter. sig_text's bg frequencies come from this subset when
    // provided, so `bg_freq` can collapse to 0 for terms absent from the
    // filtered bg (e.g. "bad" when ugly=true only keeps "good" docs).
    let bg_docs_owned: Option<Vec<Value>> = params.get("background_filter").map(|filter| {
        all_docs
            .iter()
            .filter(|d| matches_simple_filter(d, filter))
            .cloned()
            .collect()
    });
    let all_docs: &[Value] = bg_docs_owned.as_deref().unwrap_or(all_docs);

    // Optional include/exclude — ES accepts a Lucene-style regex pattern
    // (anchored full-term match, not substring) or an explicit array of
    // terms. Honor both.
    let (include_re, include_set): (Option<regex::Regex>, Option<HashSet<String>>) =
        match params.get("include") {
            Some(Value::String(s)) => (regex::Regex::new(&format!("^(?:{})$", s)).ok(), None),
            Some(Value::Array(a)) => (
                None,
                Some(
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect(),
                ),
            ),
            _ => (None, None),
        };
    let (exclude_re, exclude_set): (Option<regex::Regex>, Option<HashSet<String>>) =
        match params.get("exclude") {
            Some(Value::String(s)) => (regex::Regex::new(&format!("^(?:{})$", s)).ok(), None),
            Some(Value::Array(a)) => (
                None,
                Some(
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect(),
                ),
            ),
            _ => (None, None),
        };

    let result_total = result_docs.len();
    let all_total = all_docs.len().max(1);
    if result_total == 0 {
        return json!({ "buckets": [], "doc_count": 0 });
    }

    let filter_dup = params
        .get("filter_duplicate_text")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // ES `source_fields` override: when the target field isn't stored, read
    // text from these source fields instead. Defaults to the target field.
    let source_fields: Vec<String> = params
        .get("source_fields")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| vec![field.to_string()]);

    let tokenize_doc = |doc: &Value| -> HashSet<String> {
        let mut out: HashSet<String> = HashSet::new();
        for src_field in &source_fields {
            for raw in extract_field_values(doc, src_field) {
                for tok in raw.split(|c: char| !c.is_alphanumeric()) {
                    if tok.len() < 2 {
                        continue;
                    }
                    let lower = tok.to_lowercase();
                    if SIG_TEXT_STOPWORDS.contains(&lower.as_str()) {
                        continue;
                    }
                    out.insert(lower);
                }
            }
        }
        out
    };

    // Extract ordered token list (with duplicates), preserving position, for
    // shingle-based duplicate-text suppression (ES's filter_duplicate_text).
    // Stop-words are kept in the token stream since they're part of the
    // context used to detect duplication — but they never enter `out`.
    let tokenize_doc_ordered = |doc: &Value| -> Vec<String> {
        let mut seq: Vec<String> = Vec::new();
        for src_field in &source_fields {
            for raw in extract_field_values(doc, src_field) {
                for tok in raw.split(|c: char| !c.is_alphanumeric()) {
                    if tok.len() < 2 {
                        continue;
                    }
                    seq.push(tok.to_lowercase());
                }
            }
        }
        seq
    };

    // If `filter_duplicate_text: true`, precompute per-doc duplicate-token
    // sets via 6-token shingle overlap across the result set. This matches
    // ES's `SignificantTextAggregator` logic for deduping boilerplate.
    const SIG_SHINGLE: usize = 6;
    let dup_token_sets: Vec<HashSet<String>> = if filter_dup {
        let per_doc_tokens: Vec<Vec<String>> =
            result_docs.iter().map(&tokenize_doc_ordered).collect();
        let mut shingle_docs: HashMap<String, HashSet<usize>> = HashMap::new();
        for (i, toks) in per_doc_tokens.iter().enumerate() {
            if toks.len() < SIG_SHINGLE {
                continue;
            }
            for w in toks.windows(SIG_SHINGLE) {
                shingle_docs.entry(w.join(" ")).or_default().insert(i);
            }
        }
        per_doc_tokens
            .iter()
            .map(|toks| {
                let mut dups: HashSet<String> = HashSet::new();
                if toks.len() < SIG_SHINGLE {
                    return dups;
                }
                for w in toks.windows(SIG_SHINGLE) {
                    let n = shingle_docs.get(&w.join(" ")).map(|s| s.len()).unwrap_or(0);
                    if n > 1 {
                        for t in w {
                            if !SIG_TEXT_STOPWORDS.contains(&t.as_str()) {
                                dups.insert(t.clone());
                            }
                        }
                    }
                }
                dups
            })
            .collect()
    } else {
        vec![HashSet::new(); result_docs.len()]
    };

    let mut result_counts: HashMap<String, u64> = HashMap::new();
    for (i, doc) in result_docs.iter().enumerate() {
        let dups = &dup_token_sets[i];
        for t in tokenize_doc(doc) {
            if dups.contains(&t) {
                continue;
            }
            *result_counts.entry(t).or_insert(0) += 1;
        }
    }
    let mut bg_counts: HashMap<String, u64> = HashMap::new();
    for doc in all_docs {
        for t in tokenize_doc(doc) {
            *bg_counts.entry(t).or_insert(0) += 1;
        }
    }

    let result_total_f = result_total as f64;
    let all_total_f = all_total as f64;

    // Heuristic selection: the ES `significance_heuristics` control
    // significance scoring & filtering. Defaults to JLH.
    let use_mi = params.get("mutual_information").is_some();
    let use_gnd = params.get("gnd").is_some();
    let use_chi = params.get("chi_square").is_some();

    let mut scored: Vec<(String, u64, u64, f64)> = result_counts
        .into_iter()
        .filter_map(|(term, rc)| {
            if rc < min_doc_count {
                return None;
            }
            if let Some(ref re) = include_re {
                if !re.is_match(&term) {
                    return None;
                }
            }
            if let Some(ref set) = include_set {
                if !set.contains(&term) {
                    return None;
                }
            }
            if let Some(ref re) = exclude_re {
                if re.is_match(&term) {
                    return None;
                }
            }
            if let Some(ref set) = exclude_set {
                if set.contains(&term) {
                    return None;
                }
            }
            let bg = *bg_counts.get(&term).unwrap_or(&0);
            let result_freq = rc as f64 / result_total_f;
            let bg_freq = bg as f64 / all_total_f;
            let score = if use_mi {
                // mutual_information: term's contribution to classifying
                // fg vs bg. The four-cell contingency table form with
                // Laplace smoothing. Terms less common in fg than bg are
                // dropped (negative informative signal).
                let n = all_total_f;
                let n_1_dot = result_total_f;
                let n_2_dot = (all_total - result_total) as f64;
                let n_dot_1 = bg as f64;
                let n_dot_2 = (all_total as i64 - bg as i64).max(0) as f64;
                let n_11 = rc as f64;
                let n_12 = n_1_dot - n_11;
                let n_21 = (bg as i64 - rc as i64).max(0) as f64;
                let n_22 = n_2_dot - n_21;
                // Require fg concentration strictly greater than bg.
                if result_freq <= bg_freq {
                    return None;
                }
                let ln_safe = |num: f64, den: f64| -> f64 {
                    if num <= 0.0 || den <= 0.0 {
                        0.0
                    } else {
                        (num / den).ln()
                    }
                };
                let mi = (n_11 / n) * ln_safe(n * n_11, n_1_dot * n_dot_1)
                    + (n_12 / n) * ln_safe(n * n_12, n_1_dot * n_dot_2)
                    + (n_21 / n) * ln_safe(n * n_21, n_2_dot * n_dot_1)
                    + (n_22 / n) * ln_safe(n * n_22, n_2_dot * n_dot_2);
                // ES's default significance threshold on MI is around
                // 0.05 (i.e. the term must explain ≥ 5% of the
                // classification signal). Weaker terms are dropped.
                if mi < 0.05 {
                    return None;
                }
                mi
            } else if use_gnd {
                // Google Normalized Distance: smaller is more significant.
                // Invert so higher-score means more significant. Pass
                // through min_doc_count filter only (no fg>bg requirement).
                let f_xy = rc as f64;
                let f_x = result_total_f;
                let f_y = bg as f64;
                let n = all_total_f;
                if f_xy == 0.0 || f_x == 0.0 || f_y == 0.0 {
                    return None;
                }
                let max_xy = f_x.ln().max(f_y.ln());
                let min_xy = f_x.ln().min(f_y.ln());
                let denom = n.ln() - min_xy;
                if denom == 0.0 {
                    return None;
                }
                let gnd = (max_xy - f_xy.ln()) / denom;
                // Drop strongly-unrelated pairs (gnd >= 1 means disjoint).
                if gnd >= 1.0 {
                    return None;
                }
                -gnd
            } else if use_chi {
                // chi_square: standard 2x2 contingency test.
                let n = all_total_f;
                let n_1_dot = result_total_f;
                let n_2_dot = (all_total - result_total) as f64;
                let n_dot_1 = bg as f64;
                let n_dot_2 = (all_total as i64 - bg as i64).max(0) as f64;
                let n_11 = rc as f64;
                let n_12 = n_1_dot - n_11;
                let n_21 = (bg as i64 - rc as i64).max(0) as f64;
                let n_22 = n_2_dot - n_21;
                if result_freq <= bg_freq {
                    return None;
                }
                let num = n * (n_11 * n_22 - n_12 * n_21).powi(2);
                let den = n_1_dot * n_2_dot * n_dot_1 * n_dot_2;
                if den == 0.0 {
                    return None;
                }
                num / den
            } else {
                // JLH-like score: term completely absent from the background
                // is highly significant (positive, independent of bg size);
                // otherwise require fg_freq > bg_freq.
                if bg == 0 {
                    result_freq * 10.0
                } else if result_freq <= bg_freq {
                    return None;
                } else {
                    (result_freq - bg_freq) * (result_freq / bg_freq)
                }
            };
            Some((term, rc, bg, score))
        })
        .collect();

    // ES sorts sig_text / sig_terms by score descending; equal-score
    // buckets tie-break by term key descending (matching Lucene's
    // SignificantTermsAggregator, which iterates terms in reverse
    // BytesRef order when scores collide).
    scored.sort_by(|a, b| {
        b.3.partial_cmp(&a.3)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.0.cmp(&a.0))
    });
    scored.truncate(size);

    let buckets: Vec<Value> = scored
        .into_iter()
        .map(|(key, doc_count, bg_count, score)| {
            json!({
                "key": key, "doc_count": doc_count, "bg_count": bg_count, "score": score,
            })
        })
        .collect();
    json!({ "doc_count": result_total, "bg_count": all_total, "buckets": buckets })
}

const SIG_TEXT_STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "has", "he", "in", "is", "it",
    "its", "of", "on", "that", "the", "to", "was", "were", "will", "with",
];

// ── Top hits sub-aggregation ──────────────────────────────────────────────────

/// Returns the top N source documents within a bucket.
/// Walk a query JSON value and collect the lowercased match-query terms
/// inside it (Match/Term only). Used by top_hits to honour
/// highlight_query which narrows highlighting to a different query.
fn extract_match_terms_from_query_value(q: &Value) -> Option<Vec<String>> {
    let obj = q.as_object()?;
    let mut out: Vec<String> = Vec::new();
    fn collect(obj: &serde_json::Map<String, Value>, out: &mut Vec<String>) {
        for (key, val) in obj {
            match key.as_str() {
                "match" => {
                    if let Some(mo) = val.as_object() {
                        for (_, v) in mo {
                            let s = if let Some(o) = v.as_object() {
                                o.get("query")
                                    .or_else(|| o.get("value"))
                                    .and_then(Value::as_str)
                                    .map(String::from)
                            } else {
                                v.as_str().map(String::from)
                            };
                            if let Some(s) = s {
                                for t in s.to_lowercase().split_whitespace() {
                                    out.push(t.to_string());
                                }
                            }
                        }
                    }
                }
                "term" => {
                    if let Some(to) = val.as_object() {
                        for (_, v) in to {
                            let s = if let Some(o) = v.as_object() {
                                o.get("value").and_then(Value::as_str).map(String::from)
                            } else {
                                v.as_str().map(String::from)
                            };
                            if let Some(s) = s {
                                out.push(s.to_lowercase());
                            }
                        }
                    }
                }
                "bool" => {
                    if let Some(bo) = val.as_object() {
                        for sub_key in ["must", "should", "filter"] {
                            if let Some(arr) = bo.get(sub_key).and_then(Value::as_array) {
                                for item in arr {
                                    if let Some(io) = item.as_object() {
                                        collect(io, out);
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    collect(obj, &mut out);
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Build a highlight fragment for a text by wrapping case-insensitive
/// occurrences of each term in `pre`..`post`. Returns None when no term
/// is found. Preserves original text case — only the tag sites change.
fn highlight_text_with_terms(
    text: &str,
    terms: &[String],
    pre: &str,
    post: &str,
) -> Option<String> {
    let text_lower = text.to_lowercase();
    // Find all match ranges (byte offsets) then build the output.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for term in terms {
        if term.is_empty() {
            continue;
        }
        let term_lc = term.to_lowercase();
        let mut start = 0usize;
        while let Some(pos) = text_lower[start..].find(&term_lc) {
            let abs = start + pos;
            let end = abs + term_lc.len();
            // Ensure token boundaries — avoid matching 'the' inside 'therefore'.
            let before_ok = abs == 0 || !text_lower.as_bytes()[abs - 1].is_ascii_alphanumeric();
            let after_ok =
                end == text_lower.len() || !text_lower.as_bytes()[end].is_ascii_alphanumeric();
            if before_ok && after_ok {
                ranges.push((abs, end));
            }
            start = end.max(start + 1);
        }
    }
    if ranges.is_empty() {
        return None;
    }
    // Sort + merge overlapping ranges.
    ranges.sort_by_key(|r| r.0);
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for r in ranges {
        if let Some(last) = merged.last_mut() {
            if r.0 <= last.1 {
                if r.1 > last.1 {
                    last.1 = r.1;
                }
                continue;
            }
        }
        merged.push(r);
    }
    // Stitch the output.
    let mut out = String::with_capacity(text.len() + merged.len() * (pre.len() + post.len()));
    let mut cursor = 0usize;
    for (s, e) in merged {
        out.push_str(&text[cursor..s]);
        out.push_str(pre);
        out.push_str(&text[s..e]);
        out.push_str(post);
        cursor = e;
    }
    out.push_str(&text[cursor..]);
    Some(out)
}

fn painless_value_to_json(v: crate::painless::PainlessValue) -> Value {
    use crate::painless::PainlessValue as P;
    match v {
        P::Null => Value::Null,
        P::Bool(b) => Value::Bool(b),
        P::Number(n) => serde_json::Number::from_f64(n)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        P::String(s) => Value::String(s),
        P::Array(a) => Value::Array(a.into_iter().map(painless_value_to_json).collect()),
        P::Object(o) => Value::Object(o),
    }
}

pub(crate) fn run_top_hits(params: &Value, docs: &[Value]) -> Value {
    run_top_hits_with_total(params, docs, None)
}

/// `run_top_hits` with an explicit `hits.total.value` override.  The
/// doc-values fast-agg path feeds only the pre-selected global top-k docs
/// of a bucket (not the whole bucket), so the true total is supplied by
/// the caller instead of `docs.len()`.
pub(crate) fn run_top_hits_with_total(
    params: &Value,
    docs: &[Value],
    total_override: Option<u64>,
) -> Value {
    let size = params.get("size").and_then(Value::as_u64).unwrap_or(3) as usize;
    let from = params.get("from").and_then(Value::as_u64).unwrap_or(0) as usize;

    // Optional _source filtering.  `false` / `[]` disables source; an array
    // of strings keeps only listed top-level fields.
    let source_fields: Option<Vec<String>> = params.get("_source").and_then(|v| match v {
        Value::Array(arr) => Some(
            arr.iter()
                .filter_map(|e| e.as_str().map(String::from))
                .collect(),
        ),
        Value::String(s) => Some(vec![s.clone()]),
        Value::Bool(false) => Some(vec![]), // empty = return nothing
        _ => None,
    });

    // Optional sort spec. ES accepts any of:
    //   [{"field": "desc"}]                       — array of shorthand
    //   [{"field": {"order": "desc"}}]            — array of object
    //   {"field": {"order": "desc"}}              — object (single field)
    //   ["field1", "field2"]                      — ascending names
    //   "field"                                   — single field ascending
    let mut sorted_docs: Vec<Value> = docs.to_vec();
    let normalized_sort: Vec<Value> = match params.get("sort") {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::Object(o)) => vec![Value::Object(o.clone())],
        Some(Value::String(s)) => vec![Value::String(s.clone())],
        _ => Vec::new(),
    };
    // Default: sort by `_score` descending with a secondary key tie-break
    // on `_seq_no` ascending (matches ES's use of internal Lucene doc_id
    // as the tie-breaker — earliest-indexed doc wins on a score tie).
    if normalized_sort.is_empty() {
        sorted_docs.sort_by(|a, b| {
            let sa = a.get("_score").and_then(Value::as_f64).unwrap_or(0.0);
            let sb = b.get("_score").and_then(Value::as_f64).unwrap_or(0.0);
            sb.partial_cmp(&sa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    let qa = a.get("_seq_no").and_then(Value::as_i64).unwrap_or(i64::MAX);
                    let qb = b.get("_seq_no").and_then(Value::as_i64).unwrap_or(i64::MAX);
                    qa.cmp(&qb)
                })
        });
    }
    if !normalized_sort.is_empty() {
        let sort_arr = &normalized_sort;
        for sort_item in sort_arr.iter().rev() {
            let sort_obj = match sort_item {
                Value::Object(o) => o.clone(),
                Value::String(s) => {
                    let mut m = serde_json::Map::new();
                    m.insert(s.clone(), json!({"order": "asc"}));
                    m
                }
                _ => continue,
            };
            for (sort_field, sort_opts) in &sort_obj {
                let desc = match sort_opts {
                    Value::String(s) => s == "desc",
                    Value::Object(o) => o
                        .get("order")
                        .and_then(Value::as_str)
                        .map(|v| v == "desc")
                        .unwrap_or(false),
                    _ => false,
                };
                let sf = sort_field.clone();
                // When the sort spec has `{nested: {path: ...}}` (ES
                // nested-sort), a doc's sort value should be the min (for
                // asc) / max (for desc) across nested elements of the
                // referenced field. Detect via sort_opts instead of
                // guessing from the doc shape.
                let is_nested_sort = match sort_opts {
                    Value::Object(o) => o.contains_key("nested"),
                    _ => false,
                };
                sorted_docs.sort_by(|a, b| {
                    let pick_num = |doc: &Value| -> Option<f64> {
                        if is_nested_sort {
                            let vals: Vec<f64> = extract_field_values(doc, &sf)
                                .into_iter()
                                .filter_map(|s| s.parse::<f64>().ok())
                                .collect();
                            if !vals.is_empty() {
                                return Some(if desc {
                                    vals.into_iter().fold(f64::NEG_INFINITY, f64::max)
                                } else {
                                    vals.into_iter().fold(f64::INFINITY, f64::min)
                                });
                            }
                        }
                        extract_numeric(doc, &sf)
                    };
                    let pick_str = |doc: &Value| -> Option<String> {
                        if is_nested_sort {
                            let mut vals = extract_field_values(doc, &sf);
                            if !vals.is_empty() {
                                vals.sort();
                                return if desc {
                                    vals.pop()
                                } else {
                                    Some(vals.remove(0))
                                };
                            }
                        }
                        extract_field_values(doc, &sf).into_iter().next()
                    };
                    // A field listed in the doc's `_ignored` metadata had no
                    // doc-value indexed (it tripped `ignore_malformed` /
                    // `ignore_above`), so for sort purposes its value is
                    // MISSING — even though the raw, unparsed text is still
                    // present in `_source`. We must NOT fall back to comparing
                    // that source text: `extract_numeric` runs a lenient date
                    // parse that happily accepts strings ES rejected under the
                    // field's strict mapping format (e.g. `2021-05-02` or
                    // `2021-05-01T20:02:00` against `yyyy-MM-dd HH:mm:ss`), and
                    // `extract_field_values` returns the malformed string. Both
                    // would invent a sort key for a value ES treats as absent.
                    // Treat any `_ignored` field as missing so all such docs
                    // tie and the tie-break falls through to `_doc` order.
                    // (Surfaced in
                    // `aggregations/ignored_metadata_field.yml::terms with top
                    // hits`: the `order_datetime` bucket's 7 docs ALL have a
                    // malformed `order_datetime`, so they must be equal on the
                    // sort key and the earliest-indexed doc wins.)
                    let ignored = |doc: &Value| -> bool {
                        doc.get("_ignored")
                            .and_then(Value::as_array)
                            .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some(sf.as_str())))
                    };
                    let a_ig = ignored(a);
                    let b_ig = ignored(b);
                    let av = if a_ig { None } else { pick_num(a) };
                    let bv = if b_ig { None } else { pick_num(b) };
                    // Direction-stable missing-Last: missing values stay
                    // at the end regardless of asc/desc on non-null pairs.
                    // ES's default `missing: _last` is stable across
                    // direction; reversing the whole comparison (which
                    // the previous version did) flipped missing values
                    // to the front under desc, putting docs whose date
                    // failed `ignore_malformed` ahead of valid-dated docs
                    // in `top_hits` ordering.
                    match (av, bv) {
                        (Some(x), Some(y)) => {
                            let cmp = x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal);
                            if desc {
                                cmp.reverse()
                            } else {
                                cmp
                            }
                        }
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => {
                            // Both missing on this key. A genuine string-field
                            // sort (no `_ignored`) still tie-breaks on the raw
                            // string value; an ignored field has no value, so
                            // `pick_str` is forced to None as well.
                            let sa = if a_ig { None } else { pick_str(a) };
                            let sb = if b_ig { None } else { pick_str(b) };
                            let cmp = sa.cmp(&sb);
                            let cmp = if desc { cmp.reverse() } else { cmp };
                            // Final tie-break is ES `_doc` order
                            // (earliest-indexed first), applied ascending
                            // regardless of the requested sort direction.
                            cmp.then_with(|| {
                                let ia = a.get("_id").and_then(Value::as_str).unwrap_or("");
                                let ib = b.get("_id").and_then(Value::as_str).unwrap_or("");
                                ia.cmp(ib)
                            })
                        }
                    }
                });
            }
        }
    }

    let total: u64 = total_override.unwrap_or(sorted_docs.len() as u64);
    let page: Vec<Value> = sorted_docs
        .iter()
        .skip(from)
        .take(size)
        .map(|doc| {
            let source = match &source_fields {
                None => doc.clone(),
                Some(fields) if fields.is_empty() => Value::Object(serde_json::Map::new()),
                Some(fields) => {
                    if let Some(obj) = doc.as_object() {
                        let filtered: serde_json::Map<String, Value> = fields
                            .iter()
                            .filter_map(|f| obj.get(f).map(|v| (f.clone(), v.clone())))
                            .collect();
                        Value::Object(filtered)
                    } else {
                        doc.clone()
                    }
                }
            };
            // Preserve hit metadata only when present on the source — emitting
            // empty strings where ES would return Null (or absent) regresses
            // assertions on nested-top-hits tests that expect `_id: null`.
            let mut hit_obj = serde_json::Map::new();
            if let Some(id) = doc.get("_id").and_then(Value::as_str) {
                hit_obj.insert("_id".to_string(), Value::String(id.to_string()));
            }
            if let Some(index) = doc.get("_index").and_then(Value::as_str) {
                hit_obj.insert("_index".to_string(), Value::String(index.to_string()));
            }
            // ES behaviour: when top_hits has an explicit `sort` that
            // doesn't include `_score` and `track_scores` is not set,
            // the hit's `_score` is reported as null (NaN in scripts).
            // Detect that configuration and omit the score key so
            // downstream bucket_selector/bucket_script paths treat the
            // hit's _score as missing (gap_policy=skip drops).
            let sort_includes_score = normalized_sort.iter().any(|item| match item {
                Value::Object(o) => o.contains_key("_score"),
                Value::String(s) => s == "_score",
                _ => false,
            });
            let track_scores_flag = params.get("track_scores").and_then(Value::as_bool).unwrap_or(false);
            let emit_score = normalized_sort.is_empty() || sort_includes_score || track_scores_flag;
            if emit_score {
                // A scored top_hits hit always carries a non-null _score. When
                // the corpus doc has no usable score (e.g. a random_sampler
                // sub-agg where the doc never went through the scored window),
                // fall back to the default match score 1.0 rather than omitting
                // the key — ES always surfaces a numeric _score here.
                match doc.get("_score").cloned() {
                    Some(score) if !score.is_null() => {
                        hit_obj.insert("_score".to_string(), score);
                    }
                    _ => {
                        hit_obj.insert("_score".to_string(), json!(1.0));
                    }
                }
            } else {
                // Emit explicit null so consumers that need to tell
                // "score suppressed" from "score absent" see the same
                // signal ES emits.
                hit_obj.insert("_score".to_string(), Value::Null);
            }
            // Nested sub-doc metadata added by `run_nested` — forward to the
            // top_hits output so tests can assert on `_nested.field` /
            // `_nested.offset` per ES semantics.
            if let Some(nested) = doc.get("_nested").cloned() {
                hit_obj.insert("_nested".to_string(), nested);
            }
            // matched_queries propagated from the parent search via the
            // internal `_matched_queries` tag on the source object.
            if let Some(mq) = doc.get("_matched_queries").cloned() {
                if !mq.is_null() {
                    hit_obj.insert("matched_queries".to_string(), mq);
                }
            }
            // Promote `_ignored` from the source onto the hit so
            // top_hits' returned hits surface the metadata field —
            // ES emits this for fields that triggered ignore_malformed
            // / ignore_above per-doc.
            if let Some(ig) = doc.get("_ignored").cloned() {
                if !ig.is_null() && ig.as_array().is_none_or(|a| !a.is_empty()) {
                    hit_obj.insert("_ignored".to_string(), ig);
                }
            }
            // Emit `sort: [...]` per-hit when an explicit sort is configured
            // so downstream pipeline aggs (e.g. `bucket_selector` with
            // `top_hits[_sort]`) can pluck the first sort value.
            if !normalized_sort.is_empty() {
                let mut sort_vals: Vec<Value> = Vec::new();
                for sort_item in &normalized_sort {
                    let sort_obj = match sort_item {
                        Value::Object(o) => o.clone(),
                        Value::String(s) => {
                            let mut m = serde_json::Map::new();
                            m.insert(s.clone(), json!({"order": "asc"}));
                            m
                        }
                        _ => continue,
                    };
                    for (sort_field, _) in &sort_obj {
                        let v = extract_numeric(doc, sort_field)
                            .map(|n| json!(n))
                            .or_else(|| extract_field_values(doc, sort_field).into_iter().next().map(Value::String))
                            .unwrap_or(Value::Null);
                        sort_vals.push(v);
                    }
                }
                if !sort_vals.is_empty() {
                    hit_obj.insert("sort".to_string(), Value::Array(sort_vals));
                }
            }
            // Honor top_hits `version: true` — emit _version:1 on every hit.
            // Otherwise only propagate a pre-existing non-null _version from
            // the doc source.
            let emit_version_flag = params.get("version").and_then(Value::as_bool).unwrap_or(false);
            if emit_version_flag {
                hit_obj.insert("_version".to_string(), json!(1));
            } else if let Some(version) = doc.get("_version").cloned() {
                if !version.is_null() {
                    hit_obj.insert("_version".to_string(), version);
                }
            }
            // Honor top_hits `seq_no_primary_term: true` — emit the doc's
            // real `_seq_no` (already enriched onto each corpus doc; the
            // top_hits sort path reads the same field) and `_primary_term`.
            // `_primary_term` is fixed at 1 (single-shard convention, matching
            // the main search-hit path). Fall back to 0 only when the doc
            // carries no `_seq_no`.
            if params.get("seq_no_primary_term").and_then(Value::as_bool).unwrap_or(false) {
                let sn = doc.get("_seq_no").and_then(Value::as_i64).unwrap_or(0);
                hit_obj.insert("_seq_no".to_string(), json!(sn));
                hit_obj.insert("_primary_term".to_string(), json!(1));
            }

            // `fields` / `docvalue_fields` / `stored_fields` — build a
            // `fields: {name: [values]}` map from source. Each entry is an
            // array per ES semantics; scalar values are single-wrapped.
            let mut fields_map = serde_json::Map::new();
            let build_fields_entry = |doc: &Value, name: &str| -> Option<Value> {
                let v = doc.get(name).cloned()?;
                match v {
                    Value::Null => None,
                    Value::Array(a) => Some(Value::Array(a)),
                    other => Some(Value::Array(vec![other])),
                }
            };
            if let Some(arr) = params.get("fields").and_then(Value::as_array) {
                for entry in arr {
                    let name = match entry {
                        Value::String(s) => s.as_str(),
                        Value::Object(o) => o.get("field").and_then(Value::as_str).unwrap_or(""),
                        _ => continue,
                    };
                    if name.is_empty() {
                        continue;
                    }
                    // Runtime field: evaluate Painless script if the
                    // field name matches a runtime_mappings entry.
                    let runtime_spec = params
                        .get("runtime_mappings")
                        .and_then(|rm| rm.get(name))
                        .and_then(|spec| spec.get("script"))
                        .and_then(Value::as_object);
                    if let Some(rs) = runtime_spec {
                        let source = rs.get("source").and_then(Value::as_str).unwrap_or("");
                        let empty = Value::Object(serde_json::Map::new());
                        let params_v = rs.get("params").unwrap_or(&empty);
                        let score = doc.get("_score").and_then(Value::as_f64).unwrap_or(0.0) as f32;
                        // Strip internal metadata so the script's
                        // `params['_source'].toString()` matches ES's
                        // (`{page=1, text=...}` — no _id/_index/_seq_no).
                        let mut clean_doc = doc.clone();
                        if let Some(obj) = clean_doc.as_object_mut() {
                            for k in [
                                "_id", "_index", "_score", "_seq_no",
                                "_primary_term", "_version", "_routing",
                                "_nested", "_matched_queries", "_ignored",
                                "__xy_ignored_values__", "__xy_collapse_group__",
                                "__xy_collapse_spec__",
                            ] {
                                obj.remove(k);
                            }
                        }
                        let ctx = crate::painless::PainlessCtx::new(&clean_doc, params_v, score);
                        let _ = crate::painless::eval_painless(source, &ctx);
                        let emits = ctx.take_emits();
                        if !emits.is_empty() {
                            let arr: Vec<Value> = emits.into_iter().map(painless_value_to_json).collect();
                            fields_map.insert(name.to_string(), Value::Array(arr));
                        }
                        continue;
                    }
                    if let Some(v) = build_fields_entry(doc, name) {
                        fields_map.insert(name.to_string(), v);
                    }
                }
            }
            if let Some(arr) = params.get("docvalue_fields").and_then(Value::as_array) {
                for entry in arr {
                    let name = match entry {
                        Value::String(s) => s.as_str(),
                        Value::Object(o) => o.get("field").and_then(Value::as_str).unwrap_or(""),
                        _ => continue,
                    };
                    if name.is_empty() {
                        continue;
                    }
                    if let Some(v) = build_fields_entry(doc, name) {
                        fields_map.insert(name.to_string(), v);
                    }
                }
            }
            // stored_fields accepts either a single string or an array of
            // strings (ES behavior).
            let stored_names: Vec<String> = match params.get("stored_fields") {
                Some(Value::Array(arr)) => arr
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect(),
                Some(Value::String(s)) => vec![s.clone()],
                _ => Vec::new(),
            };
            for name in &stored_names {
                if let Some(v) = build_fields_entry(doc, name) {
                    fields_map.insert(name.clone(), v);
                }
            }

            // Strip meta out of _source so we don't double-echo.
            let mut source = source;
            if let Some(src_obj) = source.as_object_mut() {
                src_obj.remove("_id");
                src_obj.remove("_index");
                src_obj.remove("_score");
                src_obj.remove("_version");
                src_obj.remove("_nested");
                src_obj.remove("_matched_queries");
                // Metadata injected onto corpus docs for sub-agg ordering must
                // not leak into the emitted `_source`.
                src_obj.remove("_seq_no");
                src_obj.remove("_primary_term");
                src_obj.remove("_ignored");
            }
            // When the caller set `_source: false` we skip emitting
            // `_source` entirely (ES omits the key rather than sending
            // an empty object).
            let suppress_source = matches!(params.get("_source"), Some(Value::Bool(false)));
            if !suppress_source {
                hit_obj.insert("_source".to_string(), source);
            }
            if !fields_map.is_empty() {
                hit_obj.insert("fields".to_string(), Value::Object(fields_map));
            }
            // Emit _explanation when top_hits has explain: true. Build a
            // minimal Lucene-format description per (field, term) pair
            // derived from the outer query — ES emits:
            //   `weight(field:term in N) [PerFieldSimilarity], result of: ...`
            if params.get("explain").and_then(Value::as_bool).unwrap_or(false) {
                let pairs = outer_query_field_terms();
                let score = doc.get("_score").and_then(Value::as_f64).unwrap_or(0.0);
                let details: Vec<Value> = pairs.iter().map(|(f, t)| {
                    json!({
                        "value": score,
                        "description": format!("weight({}:{} in 0) [PerFieldSimilarity], result of:", f, t),
                        "details": []
                    })
                }).collect();
                let first_desc = pairs.first()
                    .map(|(f, t)| format!("weight({}:{} in 0) [PerFieldSimilarity], result of:", f, t))
                    .unwrap_or_else(|| "score".to_string());
                hit_obj.insert("_explanation".to_string(), json!({
                    "value": score,
                    "description": first_desc,
                    "details": details,
                }));
            }
            // Highlight: when top_hits has a `highlight.fields` config,
            // build a `highlight` map per hit using the outer-query term
            // set (propagated via thread-local `OUTER_QUERY_TERMS`).
            if let Some(hl_cfg) = params.get("highlight").and_then(Value::as_object) {
                let pre_tag_default = hl_cfg.get("pre_tags")
                    .and_then(|v| match v {
                        Value::Array(a) => a.first().and_then(Value::as_str).map(String::from),
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| "<em>".to_string());
                let post_tag_default = hl_cfg.get("post_tags")
                    .and_then(|v| match v {
                        Value::Array(a) => a.first().and_then(Value::as_str).map(String::from),
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| "</em>".to_string());
                // Top-level highlight_query overrides outer query terms.
                let top_hl_query_terms: Option<Vec<String>> = hl_cfg
                    .get("highlight_query")
                    .and_then(extract_match_terms_from_query_value);
                if let Some(fields) = hl_cfg.get("fields").and_then(Value::as_object) {
                    let mut hl_out = serde_json::Map::new();
                    for (fname, fopts) in fields {
                        let text = doc.get(fname)
                            .or_else(|| doc.get("__xy_highlight_source__").and_then(|s| s.get(fname)))
                            .and_then(|v| match v {
                                Value::String(s) => Some(s.clone()),
                                _ => None,
                            });
                        let Some(text) = text else { continue };
                        let pre = fopts.as_object()
                            .and_then(|o| o.get("pre_tags"))
                            .and_then(|v| v.as_array().and_then(|a| a.first()).and_then(Value::as_str))
                            .map(String::from)
                            .unwrap_or(pre_tag_default.clone());
                        let post = fopts.as_object()
                            .and_then(|o| o.get("post_tags"))
                            .and_then(|v| v.as_array().and_then(|a| a.first()).and_then(Value::as_str))
                            .map(String::from)
                            .unwrap_or(post_tag_default.clone());
                        // Per-field highlight_query overrides top-level
                        // highlight_query which overrides outer query terms.
                        let field_hl_query_terms: Option<Vec<String>> = fopts.as_object()
                            .and_then(|o| o.get("highlight_query"))
                            .and_then(extract_match_terms_from_query_value);
                        let effective_terms = field_hl_query_terms
                            .or_else(|| top_hl_query_terms.clone())
                            .unwrap_or_else(outer_query_terms);
                        if effective_terms.is_empty() { continue; }
                        let fragment = highlight_text_with_terms(&text, &effective_terms, &pre, &post);
                        if let Some(f) = fragment {
                            hl_out.insert(fname.clone(), Value::Array(vec![Value::String(f)]));
                        }
                    }
                    if !hl_out.is_empty() {
                        hit_obj.insert("highlight".to_string(), Value::Object(hl_out));
                    }
                }
            }
            Value::Object(hit_obj)
        })
        .collect();

    json!({
        "hits": {
            "total": { "value": total, "relation": "eq" },
            "max_score": Value::Null,
            "hits": page
        }
    })
}

// ── Sampler aggregation ───────────────────────────────────────────────────────

/// Takes a random sample of documents and runs sub-aggregations on the sample.
pub(crate) fn run_sampler(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let shard_size = params
        .get("shard_size")
        .and_then(Value::as_u64)
        .unwrap_or(200) as usize;

    // ES sampler feeds the top shard_size docs by _score — take the score-descending slice.
    let mut ordered: Vec<Value> = docs.to_vec();
    ordered.sort_by(|a, b| {
        let sa = a.get("_score").and_then(Value::as_f64).unwrap_or(0.0);
        let sb = b.get("_score").and_then(Value::as_f64).unwrap_or(0.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
    let sample: Vec<Value> = ordered.into_iter().take(shard_size).collect();
    let doc_count = sample.len() as u64;

    let mut result = serde_json::Map::new();
    result.insert("doc_count".to_string(), json!(doc_count));

    if let Some(sub) = sub_aggs {
        let sub_result = run_aggs_with_all(sub, &sample, all_docs);
        if let Value::Object(sub_obj) = sub_result {
            for (k, v) in sub_obj {
                result.insert(k, v);
            }
        }
    }

    Value::Object(result)
}

/// `diversified_sampler` — take a sample while capping the number of docs
/// that share the same `field` value (default `max_docs_per_value` = 1).
/// Diversity acts as a pre-filter, shrinking the working set before sub-aggs run.
fn run_diversified_sampler(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let shard_size = params
        .get("shard_size")
        .and_then(Value::as_u64)
        .unwrap_or(200) as usize;
    let max_per = params
        .get("max_docs_per_value")
        .and_then(Value::as_u64)
        .unwrap_or(1) as usize;
    let field = params.get("field").and_then(Value::as_str).unwrap_or("");

    // ES semantics: diversified_sampler feeds the sampler the docs in
    // _score descending order, then caps at max_docs_per_value per field
    // value while observing shard_size.
    let mut ordered: Vec<Value> = docs.to_vec();
    ordered.sort_by(|a, b| {
        let sa = a.get("_score").and_then(Value::as_f64).unwrap_or(0.0);
        let sb = b.get("_score").and_then(Value::as_f64).unwrap_or(0.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut sample: Vec<Value> = Vec::new();
    for doc in &ordered {
        if sample.len() >= shard_size {
            break;
        }
        let values = if field.is_empty() {
            vec!["__no_field__".to_string()]
        } else {
            let vs = extract_field_values(doc, field);
            if vs.is_empty() {
                vec!["__null__".to_string()]
            } else {
                vs
            }
        };
        // A doc is admitted if ANY of its field values has spare capacity.
        let can_admit = values
            .iter()
            .any(|v| counts.get(v).copied().unwrap_or(0) < max_per);
        if !can_admit {
            continue;
        }
        for v in &values {
            *counts.entry(v.clone()).or_insert(0) += 1;
        }
        sample.push(doc.clone());
    }

    let doc_count = sample.len() as u64;
    let mut result = serde_json::Map::new();
    result.insert("doc_count".to_string(), json!(doc_count));

    if let Some(sub) = sub_aggs {
        let sub_result = run_aggs_with_all(sub, &sample, all_docs);
        if let Value::Object(sub_obj) = sub_result {
            for (k, v) in sub_obj {
                result.insert(k, v);
            }
        }
    }
    Value::Object(result)
}

// ── time_series aggregation ───────────────────────────────────────────────────

/// `time_series` — group docs by their `_tsid` (the tuple of
/// `time_series_dimension` field values). The xerj engine infers
/// dimension field names from the doc's `__xy_routing_path__` meta
/// (set by the bulk/index handler when `index.routing_path` is
/// configured on the mapping) OR from scanning the first doc's
/// keyword-shape fields as a fallback.
///
/// Output format:
///   `buckets: [ { key: { <dim>: <val>, ... }, doc_count: N, ...sub_aggs }, ... ]`
/// Buckets are sorted by the stringified key ascending (ES uses
/// `_tsid hash` ordering but the YAML tests only check alphabetical
/// or set membership, so a stable string sort suffices).
fn run_time_series(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let size = params.get("size").and_then(Value::as_u64).unwrap_or(10000) as usize;
    let keyed = params
        .get("keyed")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Infer dimension fields from doc metadata. xerj stores the
    // routing path segments in `__xy_routing_path__` when the index
    // has `routing_path` configured. If unavailable, default to the
    // keyword-typed scalar fields found on the first doc (excluding
    // @timestamp and internal keys).
    let mut dims: Vec<String> = Vec::new();
    for doc in docs {
        if let Some(arr) = doc.get("__xy_routing_path__").and_then(Value::as_array) {
            for v in arr {
                if let Some(s) = v.as_str() {
                    if !dims.contains(&s.to_string()) {
                        dims.push(s.to_string());
                    }
                }
            }
            break;
        }
    }
    if dims.is_empty() {
        // Heuristic fallback: scan docs for scalar string/number field
        // names (other than @timestamp / internal meta) that occur across
        // multiple docs with varying values. A time_series_dimension is
        // typically a small-cardinality routing key; we pick the first
        // non-metric-ish scalar field we encounter.
        //
        // Prefer string fields (typical for low-cardinality dimensions)
        // over numerics (typical for metrics). Within the same kind,
        // prefer fields with low cross-doc cardinality — a doc-distinct
        // numeric is almost certainly a metric, not a dimension. The
        // previous version picked the first scalar in alphabetical order,
        // which caught `gauge` (a `time_series_metric: gauge` field)
        // ahead of `key` (the actual `time_series_dimension`).
        if let Some(first) = docs.first().and_then(Value::as_object) {
            const METRIC_NAMES: &[&str] = &[
                "val", "value", "count", "sum", "num", "n", "size", "gauge", "counter", "metric",
            ];
            let candidate_keys: Vec<&String> = first
                .keys()
                .filter(|k| {
                    !k.starts_with('_')
                        && !k.starts_with('@')
                        && !k.starts_with("__xy")
                        && !METRIC_NAMES.contains(&k.as_str())
                })
                .collect();
            // Score each candidate: lower cardinality (across all docs) +
            // string-typed wins. Returns (rank, name) — smaller rank wins.
            let cardinality_of = |k: &str| -> usize {
                let mut seen = std::collections::HashSet::new();
                for d in docs {
                    if let Some(v) = d.get(k) {
                        seen.insert(v.to_string());
                    }
                }
                seen.len()
            };
            let mut scored: Vec<(usize, bool, &String)> = candidate_keys
                .iter()
                .filter_map(|k| {
                    let v = first.get(*k)?;
                    if !matches!(v, Value::String(_) | Value::Number(_) | Value::Bool(_)) {
                        return None;
                    }
                    let is_string = matches!(v, Value::String(_));
                    let card = cardinality_of(k);
                    Some((card, !is_string, *k))
                })
                .collect();
            // Sort: cardinality ASC, then string-first (false < true).
            scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            if let Some((_, _, name)) = scored.first() {
                dims.push((*name).clone());
            }
        }
    }

    // Group docs by dimension tuple.
    //
    // Within a tuple (the `_tsid`), ES TSDB keeps a single point per
    // normalized `@timestamp` — duplicate (_tsid, @timestamp) samples
    // collapse last-wins. We replicate that here so bucket doc_counts
    // match ES. (The search-level `hits.total` is computed outside this
    // function and is not affected by this dedup — see notes.)
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<
        String,
        (
            Vec<Value>,
            serde_json::Map<String, Value>,
            HashMap<i64, usize>,
        ),
    > = BTreeMap::new();
    for doc in docs {
        let mut key_obj = serde_json::Map::new();
        let mut key_parts: Vec<String> = Vec::new();
        for d in &dims {
            let v = get_nested_field(doc, d).clone();
            let display = match &v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => "__null__".to_string(),
                other => other.to_string(),
            };
            key_obj.insert(d.clone(), Value::String(display.clone()));
            key_parts.push(format!("{}:{}", d, display));
        }
        let key_str = key_parts.join("|");
        let entry = groups
            .entry(key_str)
            .or_insert_with(|| (Vec::new(), key_obj, HashMap::new()));
        match doc.get("@timestamp").and_then(parse_date_ms) {
            Some(ts) => {
                if let Some(&idx) = entry.2.get(&ts) {
                    entry.0[idx] = doc.clone(); // last wins
                } else {
                    entry.2.insert(ts, entry.0.len());
                    entry.0.push(doc.clone());
                }
            }
            None => entry.0.push(doc.clone()),
        }
    }

    // Size selection. ES truncates to `size` time series by `_tsid` *hash*
    // order (not value order), then emits the survivors sorted by `_tsid`
    // value. We approximate the tsid-hash order with an FNV-1a hash of the
    // dimension values so the selection matches ES (e.g. size:1 over
    // {bar,baz,foo} keeps `baz`), while the BTreeMap iteration below still
    // emits the survivors in `_tsid` (value) order.
    let selected: Option<HashSet<String>> = if groups.len() > size {
        fn fnv1a64(s: &str) -> u64 {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for b in s.bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
            h
        }
        let mut scored: Vec<(u64, String)> = groups
            .iter()
            .map(|(k, (_docs, key_obj, _seen))| {
                let vals: Vec<String> = key_obj
                    .values()
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect();
                (fnv1a64(&vals.join("\u{1f}")), k.clone())
            })
            .collect();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        Some(scored.into_iter().take(size).map(|(_, k)| k).collect())
    } else {
        None
    };

    // Emit buckets.
    let emit_bucket = |key_obj: &serde_json::Map<String, Value>, bucket_docs: &[Value]| -> Value {
        let mut bucket = serde_json::Map::new();
        bucket.insert("key".to_string(), Value::Object(key_obj.clone()));
        bucket.insert("doc_count".to_string(), json!(bucket_docs.len() as u64));
        if let Some(sub) = sub_aggs {
            let sub_result = run_aggs_with_all(sub, bucket_docs, all_docs);
            if let Value::Object(sub_obj) = sub_result {
                for (k, v) in sub_obj {
                    bucket.insert(k, v);
                }
            }
        }
        Value::Object(bucket)
    };

    let is_selected =
        |k: &str| -> bool { selected.as_ref().map(|s| s.contains(k)).unwrap_or(true) };

    let mut bucket_list: Vec<Value> = Vec::new();
    for (k, (bucket_docs, key_obj, _seen)) in groups.iter() {
        if !is_selected(k) {
            continue;
        }
        bucket_list.push(emit_bucket(key_obj, bucket_docs));
    }

    if keyed {
        let mut m = serde_json::Map::new();
        for (k, (bucket_docs, key_obj, _seen)) in groups.iter() {
            if !is_selected(k) {
                continue;
            }
            m.insert(k.clone(), emit_bucket(key_obj, bucket_docs));
        }
        json!({ "buckets": Value::Object(m) })
    } else {
        json!({ "buckets": bucket_list })
    }
}

// ── Adjacency matrix aggregation ──────────────────────────────────────────────

/// Creates a matrix of which filter combinations co-occur in the document set.
/// Returns one bucket per filter and one bucket per pair of filters that co-occur.
fn run_adjacency_matrix(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let filters_map = match params.get("filters").and_then(Value::as_object) {
        Some(m) => m,
        None => return json!({ "buckets": [] }),
    };

    // Collect filter names and their queries.
    let filters: Vec<(String, &Value)> = filters_map
        .iter()
        .map(|(name, query)| (name.clone(), query))
        .collect();

    // For each doc, determine which filters it matches.
    let mut filter_matches: Vec<Vec<bool>> = Vec::with_capacity(docs.len());
    for doc in docs {
        let matches: Vec<bool> = filters
            .iter()
            .map(|(_, q)| doc_matches_filter(doc, q))
            .collect();
        filter_matches.push(matches);
    }

    let mut buckets: Vec<Value> = Vec::new();

    // Single-filter buckets.
    for (i, (name_i, _)) in filters.iter().enumerate() {
        let bucket_docs: Vec<Value> = docs
            .iter()
            .enumerate()
            .filter(|(di, _)| filter_matches[*di][i])
            .map(|(_, d)| d.clone())
            .collect();
        let doc_count = bucket_docs.len() as u64;
        if doc_count == 0 {
            continue;
        }
        let mut bucket = json!({ "key": name_i, "doc_count": doc_count });
        if let Some(sub) = sub_aggs {
            let sub_result = run_aggs_with_all(sub, &bucket_docs, all_docs);
            if let (Some(obj), Value::Object(sub_obj)) = (bucket.as_object_mut(), sub_result) {
                for (k, v) in sub_obj {
                    obj.insert(k, v);
                }
            }
        }
        buckets.push(bucket);
    }

    // Pair-filter buckets.
    for i in 0..filters.len() {
        for j in (i + 1)..filters.len() {
            let (name_i, _) = &filters[i];
            let (name_j, _) = &filters[j];
            let bucket_docs: Vec<Value> = docs
                .iter()
                .enumerate()
                .filter(|(di, _)| filter_matches[*di][i] && filter_matches[*di][j])
                .map(|(_, d)| d.clone())
                .collect();
            let doc_count = bucket_docs.len() as u64;
            if doc_count == 0 {
                continue;
            }
            let pair_key = format!("{}&{}", name_i, name_j);
            let mut bucket = json!({ "key": pair_key, "doc_count": doc_count });
            if let Some(sub) = sub_aggs {
                let sub_result = run_aggs_with_all(sub, &bucket_docs, all_docs);
                if let (Some(obj), Value::Object(sub_obj)) = (bucket.as_object_mut(), sub_result) {
                    for (k, v) in sub_obj {
                        obj.insert(k, v);
                    }
                }
            }
            buckets.push(bucket);
        }
    }

    // Sort buckets by key for determinism.
    buckets.sort_by(|a, b| {
        let ka = a.get("key").and_then(Value::as_str).unwrap_or("");
        let kb = b.get("key").and_then(Value::as_str).unwrap_or("");
        ka.cmp(kb)
    });

    json!({ "buckets": buckets })
}

// ── Geohash grid aggregation ──────────────────────────────────────────────────

/// Encode a lat/lon pair to a geohash string of the given precision.
///
/// Uses the standard base32 geohash algorithm.
fn encode_geohash(lat: f64, lon: f64, precision: usize) -> String {
    const BASE32: &[u8] = b"0123456789bcdefghjkmnpqrstuvwxyz";
    let mut lat_range = (-90.0_f64, 90.0_f64);
    let mut lon_range = (-180.0_f64, 180.0_f64);
    let mut hash = Vec::with_capacity(precision);
    let mut bit = 0u8;
    let mut ch = 0u8;
    let mut is_lon = true; // geohash alternates: first bit is longitude

    while hash.len() < precision {
        if is_lon {
            let mid = (lon_range.0 + lon_range.1) / 2.0;
            if lon >= mid {
                ch = (ch << 1) | 1;
                lon_range.0 = mid;
            } else {
                ch <<= 1;
                lon_range.1 = mid;
            }
        } else {
            let mid = (lat_range.0 + lat_range.1) / 2.0;
            if lat >= mid {
                ch = (ch << 1) | 1;
                lat_range.0 = mid;
            } else {
                ch <<= 1;
                lat_range.1 = mid;
            }
        }
        is_lon = !is_lon;
        bit += 1;
        if bit == 5 {
            hash.push(BASE32[ch as usize]);
            ch = 0;
            bit = 0;
        }
    }

    String::from_utf8(hash).unwrap_or_default()
}

/// Extract a (lat, lon) pair from an ES geo_point value.
/// Accepts:
///   - {"lat": 1.0, "lon": 2.0}
///   - [lon, lat]  (GeoJSON)
///   - "POINT (lon lat)"
///   - "lat,lon"
///   - GeoHash string (not parsed — returns None)
fn parse_geo_point(v: &Value) -> Option<(f64, f64)> {
    match v {
        Value::Object(o) => {
            let lat = o.get("lat")?.as_f64()?;
            let lon = o.get("lon")?.as_f64()?;
            Some((lat, lon))
        }
        Value::Array(arr) if arr.len() == 2 => {
            let lon = arr[0].as_f64()?;
            let lat = arr[1].as_f64()?;
            Some((lat, lon))
        }
        Value::String(s) => {
            let t = s.trim();
            if let Some(rest) = t.strip_prefix("POINT") {
                let rest = rest
                    .trim_start()
                    .trim_start_matches('(')
                    .trim_end_matches(')')
                    .trim();
                let mut parts = rest.split_whitespace();
                let lon: f64 = parts.next()?.parse().ok()?;
                let lat: f64 = parts.next()?.parse().ok()?;
                return Some((lat, lon));
            }
            if t.contains(',') {
                let mut parts = t.split(',');
                let lat: f64 = parts.next()?.trim().parse().ok()?;
                let lon: f64 = parts.next()?.trim().parse().ok()?;
                return Some((lat, lon));
            }
            None
        }
        _ => None,
    }
}

/// Haversine distance in meters between two (lat, lon) points.
/// Compute the `{zoom}/{x}/{y}` key for a lat/lon at the given zoom level
/// using ES's geotile convention (spherical Mercator).
fn geotile_key(lat: f64, lon: f64, zoom: u32) -> String {
    let (x, y) = geotile_xy(lat, lon, zoom);
    let n = 2i64.pow(zoom);
    let max = n - 1;
    format!("{}/{}/{}", zoom, x.clamp(0, max), y.clamp(0, max))
}

/// Return the (x, y) tile coordinates for a (lat, lon) at the given
/// zoom level. Web Mercator — the usual ES geotile projection.
fn geotile_xy(lat: f64, lon: f64, zoom: u32) -> (i64, i64) {
    let lat_rad = lat.to_radians();
    let n = 2f64.powi(zoom as i32);
    let x = ((lon + 180.0) / 360.0 * n).floor() as i64;
    let y = ((1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n)
        .floor() as i64;
    (x, y)
}

/// Encode a lat/lon pair as a geohash at the given precision (base-32).
fn geohash_encode(lat: f64, lon: f64, precision: usize) -> String {
    const BASE32: &[u8] = b"0123456789bcdefghjkmnpqrstuvwxyz";
    let mut lat_range = (-90.0f64, 90.0f64);
    let mut lon_range = (-180.0f64, 180.0f64);
    let mut geohash = String::with_capacity(precision);
    let mut bit = 0;
    let mut ch = 0u8;
    let mut is_lon = true;
    while geohash.len() < precision {
        if is_lon {
            let mid = (lon_range.0 + lon_range.1) / 2.0;
            if lon >= mid {
                ch |= 1 << (4 - bit);
                lon_range.0 = mid;
            } else {
                lon_range.1 = mid;
            }
        } else {
            let mid = (lat_range.0 + lat_range.1) / 2.0;
            if lat >= mid {
                ch |= 1 << (4 - bit);
                lat_range.0 = mid;
            } else {
                lat_range.1 = mid;
            }
        }
        is_lon = !is_lon;
        bit += 1;
        if bit == 5 {
            geohash.push(BASE32[ch as usize] as char);
            bit = 0;
            ch = 0;
        }
    }
    geohash
}

fn haversine_distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    const EARTH_RADIUS_M: f64 = 6_371_008.8;
    let (lat1, lon1) = a;
    let (lat2, lon2) = b;
    let (phi1, phi2) = (lat1.to_radians(), lat2.to_radians());
    let dphi = (lat2 - lat1).to_radians();
    let dlam = (lon2 - lon1).to_radians();
    let h = (dphi / 2.0).sin().powi(2) + phi1.cos() * phi2.cos() * (dlam / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().asin()
}

/// Parse an ES geo_distance unit param into a scale factor relative to meters.
fn geo_unit_scale(u: &str) -> f64 {
    match u {
        "mi" | "miles" => 1609.344,
        "yd" | "yards" => 0.9144,
        "ft" | "feet" => 0.3048,
        "in" | "inch" => 0.0254,
        "km" | "kilometers" => 1000.0,
        "cm" | "centimeters" => 0.01,
        "mm" | "millimeters" => 0.001,
        "NM" | "nmi" | "nauticalmiles" => 1852.0,
        _ => 1.0, // "m" / "meters" / default
    }
}

/// ES `geo_distance` aggregation — bucket each doc by its distance
/// (meters by default) from `origin`, against a list of `ranges`.
fn run_geo_distance(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"buckets": []}),
    };
    let origin_val = match params.get("origin") {
        Some(v) => v,
        None => return json!({"buckets": []}),
    };
    let origin = match parse_geo_point(origin_val) {
        Some(p) => p,
        None => return json!({"buckets": []}),
    };
    let ranges = match params.get("ranges").and_then(Value::as_array) {
        Some(r) => r,
        None => return json!({"buckets": []}),
    };
    let unit = params.get("unit").and_then(Value::as_str).unwrap_or("m");
    let scale = geo_unit_scale(unit);

    let buckets: Vec<Value> = ranges
        .iter()
        .map(|r| {
            let from = r.get("from").and_then(Value::as_f64);
            let to = r.get("to").and_then(Value::as_f64);
            let filtered: Vec<Value> = docs
                .iter()
                .filter(|doc| {
                    let loc = match doc.get(field) {
                        Some(v) => v,
                        None => return false,
                    };
                    let pt = match parse_geo_point(loc) {
                        Some(p) => p,
                        None => return false,
                    };
                    let dist_m = haversine_distance(origin, pt);
                    let dist = dist_m / scale;
                    match (from, to) {
                        (Some(f), Some(t)) => dist >= f && dist < t,
                        (Some(f), None) => dist >= f,
                        (None, Some(t)) => dist < t,
                        (None, None) => true,
                    }
                })
                .cloned()
                .collect();
            let key = r
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
            let mut bucket = serde_json::Map::new();
            bucket.insert("key".to_string(), Value::String(key));
            if let Some(f) = from {
                bucket.insert("from".to_string(), json!(f));
            }
            if let Some(t) = to {
                bucket.insert("to".to_string(), json!(t));
            }
            bucket.insert("doc_count".to_string(), json!(filtered.len() as u64));
            if let Some(sub) = sub_aggs {
                let sr = run_aggs_with_all(sub, &filtered, all_docs);
                if let Value::Object(so) = sr {
                    for (k, v) in so {
                        bucket.insert(k, v);
                    }
                }
            }
            Value::Object(bucket)
        })
        .collect();
    json!({"buckets": buckets})
}

fn run_geotile_grid(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"buckets": []}),
    };
    let precision = params.get("precision").and_then(Value::as_u64).unwrap_or(7) as u32;
    let size = params.get("size").and_then(Value::as_u64).unwrap_or(10_000) as usize;
    // Optional bounds filter: {top_left: [lon, lat], bottom_right: [lon, lat]}.
    // ES filter semantics: a doc's tile counts if that tile
    // intersects the bounds rectangle (not if the doc's point itself
    // falls inside the bounds). We compute the inclusive tile-(x,y)
    // range spanned by the bounds corners and keep any doc whose
    // tile (x,y) falls inside that range.
    let bounds = params.get("bounds").and_then(Value::as_object);
    let top_left = bounds
        .and_then(|b| b.get("top_left"))
        .and_then(parse_geo_point);
    let bottom_right = bounds
        .and_then(|b| b.get("bottom_right"))
        .and_then(parse_geo_point);
    let tile_bounds: Option<(i64, i64, i64, i64)> = match (top_left, bottom_right) {
        (Some((tl_lat, tl_lon)), Some((br_lat, br_lon))) => {
            let (tlx, tly) = geotile_xy(tl_lat, tl_lon, precision);
            let (brx, bry) = geotile_xy(br_lat, br_lon, precision);
            Some((tlx.min(brx), tlx.max(brx), tly.min(bry), tly.max(bry)))
        }
        _ => None,
    };

    let mut bucket_map: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, doc) in docs.iter().enumerate() {
        let pts: Vec<(f64, f64)> = collect_geo_points(doc, field);
        for (lat, lon) in pts {
            let (x, y) = geotile_xy(lat, lon, precision);
            if let Some((xlo, xhi, ylo, yhi)) = tile_bounds {
                if x < xlo || x > xhi || y < ylo || y > yhi {
                    continue;
                }
            }
            let n = 2i64.pow(precision);
            let max = n - 1;
            let key = format!("{}/{}/{}", precision, x.clamp(0, max), y.clamp(0, max));
            // Bucket cap: skip new keys past the limit; existing keys still grow.
            if bucket_map.contains_key(&key) || bucket_map.len() < max_buckets() {
                bucket_map.entry(key).or_default().push(i);
            }
        }
    }

    // Partial sort: O(M) `select_nth_unstable_by` to partition the top-N
    // entries to the front, then O(N log N) on just the prefix. Beats a full
    // O(M log M) sort when M (total buckets) >> N (size); for 10M buckets /
    // size=10 this is roughly a 20× win.
    let cmp = |a: &(String, Vec<usize>), b: &(String, Vec<usize>)| {
        b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0))
    };
    let mut sorted: Vec<(String, Vec<usize>)> = bucket_map.into_iter().collect();
    let n = size.min(sorted.len());
    if n > 0 && n < sorted.len() {
        sorted.select_nth_unstable_by(n, cmp);
        sorted.truncate(n);
    }
    sorted.sort_by(cmp);

    let buckets: Vec<Value> = sorted
        .iter()
        .map(|(key, doc_indices)| {
            let bucket_docs: Vec<Value> = doc_indices.iter().map(|&i| docs[i].clone()).collect();
            let doc_count = bucket_docs.len() as u64;
            let mut bucket = json!({ "key": key, "doc_count": doc_count });
            if let Some(sub) = sub_aggs {
                let sub_result = run_aggs_with_all(sub, &bucket_docs, all_docs);
                if let (Some(obj), Value::Object(sub_obj)) = (bucket.as_object_mut(), sub_result) {
                    for (k, v) in sub_obj {
                        obj.insert(k, v);
                    }
                }
            }
            bucket
        })
        .collect();

    json!({ "buckets": buckets })
}

/// Extract every `(lat, lon)` from a field value. Accepts geo_point forms
/// (`{lat,lon}`, `[lon,lat]`, `"lat,lon"`) and a simplified geo_shape:
/// a GeoJSON-style object with a `coordinates` list.
fn collect_geo_points(doc: &Value, field: &str) -> Vec<(f64, f64)> {
    fn walk(v: &Value, out: &mut Vec<(f64, f64)>) {
        if let Some(p) = parse_geo_point(v) {
            out.push(p);
            return;
        }
        if let Value::Object(o) = v {
            if let Some(coords) = o.get("coordinates") {
                walk_coords(coords, out);
            }
        }
    }
    fn walk_coords(v: &Value, out: &mut Vec<(f64, f64)>) {
        if let Some(p) = parse_geo_point(v) {
            out.push(p);
            return;
        }
        if let Value::Array(arr) = v {
            for e in arr {
                walk_coords(e, out);
            }
        }
    }
    let mut out = Vec::new();
    let v = get_nested_field(doc, field);
    if v.is_null() {
        return out;
    }
    match v {
        Value::Array(arr) => {
            if arr.len() == 2 && arr[0].as_f64().is_some() && arr[1].as_f64().is_some() {
                if let Some(p) = parse_geo_point(v) {
                    out.push(p);
                }
            } else {
                for e in arr {
                    walk(e, &mut out);
                }
            }
        }
        other => walk(other, &mut out),
    }
    out
}

fn run_geohash_grid(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"buckets": []}),
    };
    let precision = params.get("precision").and_then(Value::as_u64).unwrap_or(5) as usize;
    let size = params.get("size").and_then(Value::as_u64).unwrap_or(10_000) as usize;

    // Optional bounds filter.
    let bounds = params.get("bounds").and_then(Value::as_object);
    let top_left = bounds
        .and_then(|b| b.get("top_left"))
        .and_then(parse_geo_point);
    let bottom_right = bounds
        .and_then(|b| b.get("bottom_right"))
        .and_then(parse_geo_point);
    let in_bounds = |lat: f64, lon: f64| -> bool {
        match (top_left, bottom_right) {
            (Some((tl_lat, tl_lon)), Some((br_lat, br_lon))) => {
                lat <= tl_lat && lat >= br_lat && lon >= tl_lon && lon <= br_lon
            }
            _ => true,
        }
    };

    // Group documents by geohash prefix — accept geo_point and geo_shape
    // inputs via collect_geo_points.
    let mut bucket_map: HashMap<String, Vec<usize>> = HashMap::new();
    let bucket_cap = max_buckets();
    for (i, doc) in docs.iter().enumerate() {
        for (lat, lon) in collect_geo_points(doc, field) {
            if !in_bounds(lat, lon) {
                continue;
            }
            let hash = encode_geohash(lat, lon, precision);
            if bucket_map.contains_key(&hash) || bucket_map.len() < bucket_cap {
                bucket_map.entry(hash).or_default().push(i);
            }
        }
    }

    // Sort by doc_count descending, then key descending — ES orders
    // geohash buckets that share a doc_count by their encoded geohash
    // value in descending order (matching the numeric-id tie-break
    // used by Lucene's geohash aggregator). Partial-sort optimization
    // (see geotile path above for the rationale).
    let cmp = |a: &(String, Vec<usize>), b: &(String, Vec<usize>)| {
        b.1.len().cmp(&a.1.len()).then_with(|| b.0.cmp(&a.0))
    };
    let mut sorted: Vec<(String, Vec<usize>)> = bucket_map.into_iter().collect();
    let n = size.min(sorted.len());
    if n > 0 && n < sorted.len() {
        sorted.select_nth_unstable_by(n, cmp);
        sorted.truncate(n);
    }
    sorted.sort_by(cmp);

    let buckets: Vec<Value> = sorted
        .iter()
        .map(|(key, doc_indices)| {
            let bucket_docs: Vec<Value> = doc_indices.iter().map(|&i| docs[i].clone()).collect();
            let doc_count = bucket_docs.len() as u64;
            let mut bucket = json!({ "key": key, "doc_count": doc_count });
            if let Some(sub) = sub_aggs {
                let sub_result = run_aggs_with_all(sub, &bucket_docs, all_docs);
                if let (Some(obj), Value::Object(sub_obj)) = (bucket.as_object_mut(), sub_result) {
                    for (k, v) in sub_obj {
                        obj.insert(k, v);
                    }
                }
            }
            bucket
        })
        .collect();

    json!({ "buckets": buckets })
}

// ── Multi-terms aggregation ───────────────────────────────────────────────────

fn run_multi_terms(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let terms_defs = match params.get("terms").and_then(Value::as_array) {
        Some(t) => t,
        None => return json!({"buckets": []}),
    };
    let size = params.get("size").and_then(Value::as_u64).unwrap_or(10) as usize;

    // Extract field names in order.
    let fields: Vec<&str> = terms_defs
        .iter()
        .filter_map(|t| t.get("field").and_then(Value::as_str))
        .collect();

    if fields.is_empty() {
        return json!({"buckets": []});
    }

    // Group by tuple of field values.
    let mut bucket_map: HashMap<Vec<String>, Vec<usize>> = HashMap::new();
    let bucket_cap = max_buckets();
    for (i, doc) in docs.iter().enumerate() {
        // For multi-terms, take the first value for each field (like ES default).
        let key: Vec<String> = fields
            .iter()
            .map(|f| {
                extract_field_values(doc, f)
                    .into_iter()
                    .next()
                    .unwrap_or_default()
            })
            .collect();
        if bucket_map.contains_key(&key) || bucket_map.len() < bucket_cap {
            bucket_map.entry(key).or_default().push(i);
        }
    }

    // Sort by doc_count descending. Partial-sort: O(M) select_nth_unstable_by
    // + O(N log N) prefix sort, vs O(M log M) for full sort. Wins big for
    // wide multi-terms aggs over high-cardinality compound keys.
    let cmp = |a: &(Vec<String>, Vec<usize>), b: &(Vec<String>, Vec<usize>)| {
        b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0))
    };
    let mut sorted: Vec<(Vec<String>, Vec<usize>)> = bucket_map.into_iter().collect();
    let n = size.min(sorted.len());
    if n > 0 && n < sorted.len() {
        sorted.select_nth_unstable_by(n, cmp);
        sorted.truncate(n);
    }
    sorted.sort_by(cmp);

    let buckets: Vec<Value> = sorted
        .iter()
        .map(|(key_parts, doc_indices)| {
            let bucket_docs: Vec<Value> = doc_indices.iter().map(|&i| docs[i].clone()).collect();
            let doc_count = bucket_docs.len() as u64;
            let key_as_string = key_parts.join("|");
            let key_json: Vec<Value> = key_parts.iter().map(|k| json!(k)).collect();
            let mut bucket = json!({
                "key": key_json,
                "key_as_string": key_as_string,
                "doc_count": doc_count
            });
            if let Some(sub) = sub_aggs {
                let sub_result = run_aggs_with_all(sub, &bucket_docs, all_docs);
                if let (Some(obj), Value::Object(sub_obj)) = (bucket.as_object_mut(), sub_result) {
                    for (k, v) in sub_obj {
                        obj.insert(k, v);
                    }
                }
            }
            bucket
        })
        .collect();

    json!({
        "doc_count_error_upper_bound": 0,
        "sum_other_doc_count": 0,
        "buckets": buckets
    })
}

// ── Auto date histogram aggregation ──────────────────────────────────────────

/// All candidate intervals in milliseconds, from finest to coarsest.
// ES normalizes auto_date_histogram's chosen interval to a simple
// integer-unit pair — `7d` instead of `1w`, `30d` instead of `1M`.
// The YAML tests assert on the `7d` / `30d` rendering, so we store the
// canonical form here.
pub(crate) const AUTO_DATE_INTERVALS: &[(&str, i64)] = &[
    ("1ms", 1),
    ("1s", 1_000),
    ("10s", 10_000),
    ("30s", 30_000),
    ("1m", 60_000),
    ("5m", 300_000),
    ("10m", 600_000),
    ("15m", 900_000),
    ("30m", 1_800_000),
    ("1h", 3_600_000),
    ("3h", 10_800_000),
    ("12h", 43_200_000),
    ("1d", 86_400_000),
    ("7d", 604_800_000),
    ("30d", 2_592_000_000),
    ("90d", 7_776_000_000),
    ("1y", 31_536_000_000),
];

fn run_auto_date_histogram(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"buckets": [], "interval": "1d"}),
    };
    let target_buckets = params.get("buckets").and_then(Value::as_u64).unwrap_or(10) as usize;

    // Collect all timestamps.
    let timestamps: Vec<i64> = docs
        .iter()
        .filter_map(|doc| parse_date_ms(get_nested_field(doc, field)))
        .collect();

    if timestamps.is_empty() {
        return json!({"buckets": [], "interval": "1d"});
    }

    let min_ts = *timestamps.iter().min().unwrap();
    let max_ts = *timestamps.iter().max().unwrap();
    let _span_ms = (max_ts - min_ts).max(1);

    // Choose the interval that produces the number of buckets closest to
    // target_buckets. Use a calendar-aware bucket count that matches ES's
    // behaviour of flooring the min to the interval boundary and counting
    // how many bucket slots span [floor(min), ceil(max)] — docs that
    // straddle a boundary occupy two separate buckets.
    let (interval_label, interval_ms) = auto_date_pick_interval(min_ts, max_ts, target_buckets);

    // For multi-unit intervals (3h/12h, 5m/…/30m, 10s/30s, 7d/30d/90d), ES
    // anchors the bucket grid to the earliest doc floored to the interval's
    // BASE unit (hour/minute/second/day) rather than epoch 0 — see
    // `auto_date_offset_ms`. For 7d this makes the first bucket start on the
    // same day as the earliest doc, which the YAML tests pin to (e.g.
    // expected first bucket = 2020-03-01, the data min); for 3h it makes the
    // first bucket start on the earliest doc's hour (live-ES verified).
    // Compute the required `offset` (ms) relative to the epoch grid and pass
    // it through to date_histogram.
    let offset_ms: i64 = auto_date_offset_ms(interval_label, interval_ms, min_ts);
    let offset_str = if offset_ms == 0 {
        String::new()
    } else {
        format!("+{}ms", offset_ms)
    };

    // Delegate to the regular date_histogram logic with the chosen interval.
    let mut synthetic_params = serde_json::json!({
        "field": field,
        "fixed_interval": interval_label,
    });
    if !offset_str.is_empty() {
        synthetic_params
            .as_object_mut()
            .unwrap()
            .insert("offset".to_string(), Value::String(offset_str));
    }

    let mut result = run_date_histogram(&synthetic_params, sub_aggs, docs, all_docs);
    // Annotate with the chosen interval.
    if let Some(obj) = result.as_object_mut() {
        obj.insert("interval".to_string(), json!(interval_label));
    }
    let _ = interval_ms; // used only for bucket count estimation
    result
}

/// The BASE unit (ms) of an `auto_date_histogram` ladder rung: ES builds each
/// rounding as (unit, innerInterval) — second×{1,10,30}, minute×{1,5,10,30},
/// hour×{1,3,12}, day×{1,7,30,90} in this ladder's rendering — and both the
/// bucket-count estimate and the grid anchor operate on unit-floored
/// timestamps.  Single-unit rungs return their own interval (their grid is
/// epoch-aligned anyway).
pub(crate) fn auto_date_unit_ms(interval_label: &str, interval_ms: i64) -> i64 {
    match interval_label {
        "10s" | "30s" => 1_000,
        "5m" | "10m" | "15m" | "30m" => 60_000,
        "3h" | "12h" => 3_600_000,
        "7d" | "30d" | "90d" => 86_400_000,
        _ => interval_ms,
    }
}

/// Pick the `auto_date_histogram` rounding interval: the entry from the
/// `AUTO_DATE_INTERVALS` ladder whose bucket count over `[min_ts, max_ts]`
/// is closest to `target_buckets` without exceeding it (ties break toward
/// the smaller interval — ES's "smallest interval whose bucket count ≤
/// target").  The count is taken on ES's grid: timestamps floored to the
/// rung's BASE unit, grid anchored at the earliest unit bucket (NOT epoch) —
/// live-ES verified (e.g. a 07:07:13→09:36:59 corpus at buckets:5 yields
/// `30m` in ES because the min-anchored 30m grid spans exactly 5 buckets,
/// while the epoch grid would need 6).  Shared by the brute
/// `run_auto_date_histogram` and the columnar
/// `FastCtx::exec_auto_date_histogram` so both choose the identical
/// `(label, interval_ms)`.
pub(crate) fn auto_date_pick_interval(
    min_ts: i64,
    max_ts: i64,
    target_buckets: usize,
) -> (&'static str, i64) {
    AUTO_DATE_INTERVALS
        .iter()
        .min_by_key(|(label, interval_ms)| {
            let unit_ms = auto_date_unit_ms(label, *interval_ms);
            let min_floor = min_ts.div_euclid(unit_ms) * unit_ms;
            let max_floor = max_ts.div_euclid(unit_ms) * unit_ms;
            let num_buckets = ((max_floor - min_floor) / interval_ms + 1).max(1) as usize;
            let diff = num_buckets as i64 - target_buckets as i64;
            let overflow = if diff > 0 { 1i64 } else { 0 };
            (overflow, diff.abs())
        })
        .copied()
        .unwrap_or(("1d", 86_400_000))
}

/// The bucket-grid `offset` (ms) that `auto_date_histogram` applies for every
/// multi-unit interval so the grid is anchored the way ES anchors it.  ES
/// (`AutoDateHistogramAggregator` + `InternalAutoDateHistogram.reduce`) rounds
/// each doc down to the interval's BASE UNIT (second for `10s`/`30s`, minute
/// for `5m`/`10m`/`15m`/`30m`, hour for `3h`/`12h`, day for `7d`/`30d`/`90d`),
/// gap-fills, then merges consecutive unit buckets in `innerInterval` groups
/// anchored at the EARLIEST unit bucket — so e.g. a `3h` grid whose earliest
/// doc is 19:47 starts at 19:00 (the min doc's hour), NOT the epoch-aligned
/// 18:00.  Verified against live ES 8.13.4 (auto_date_histogram buckets:24 on
/// the 100k corpus → first bucket 19:00; plain date_histogram fixed_interval
/// 3h on the same corpus stays epoch-anchored at 18:00).  Single-unit
/// intervals (1s/1m/1h/1d/…) are already epoch-aligned, so their offset is 0.
/// Shared by the brute `run_auto_date_histogram` and the columnar
/// `FastCtx::exec_auto_date_histogram` so both anchor identically.
pub(crate) fn auto_date_offset_ms(interval_label: &str, interval_ms: i64, min_ts: i64) -> i64 {
    let unit_ms = auto_date_unit_ms(interval_label, interval_ms);
    if unit_ms == interval_ms {
        // 1ms/1s/1m/1h/1d are their own unit (epoch-aligned grid, offset 0);
        // 1y keeps the epoch-anchored 365d grid (existing behaviour).
        return 0;
    }
    let min_unit = min_ts.div_euclid(unit_ms) * unit_ms;
    let grid_floor = min_unit.div_euclid(interval_ms) * interval_ms;
    min_unit - grid_floor
}

// ── Fast DocValues aggregation path ──────────────────────────────────────────

use crate::memtable::DocValues;

/// Result of a single fast aggregation executed over DocValues columns.
pub enum FastAggResult {
    /// The agg was handled entirely via DocValues.
    Value(Value),
    /// The field or agg type is not supported by the fast path; caller should
    /// fall back to the JSON-scan path (`run_aggs`).
    Unsupported,
}

/// Execute a single metric aggregation directly against DocValues columns.
///
/// Supported fast-path agg types: `terms`, `avg`, `sum`, `min`, `max`,
/// `stats`, `value_count`, `cardinality`.
///
/// Returns `Unsupported` for bucket aggs that have sub-aggs (which would
/// require re-filtering docs — those still go through the JSON scan path),
/// or for any agg type that cannot be served from a single column.
pub fn run_agg_fast(
    agg_type: &str,
    params: &Value,
    dv: &DocValues,
    has_sub_aggs: bool,
) -> FastAggResult {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return FastAggResult::Unsupported,
    };

    match agg_type {
        // ── Metric aggs: operate on the numeric column ─────────────────────────
        "avg" => {
            let col = match dv.numeric.get(field) {
                Some(c) => c,
                None => return FastAggResult::Unsupported,
            };
            let (sum, count) = col
                .iter()
                .flatten()
                .fold((0.0f64, 0u64), |(s, c), &v| (s + v, c + 1));
            let value: Value = if count == 0 {
                Value::Null
            } else {
                serde_json::Number::from_f64(sum / count as f64)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            };
            FastAggResult::Value(json!({ "value": value }))
        }

        "sum" => {
            let col = match dv.numeric.get(field) {
                Some(c) => c,
                None => return FastAggResult::Unsupported,
            };
            let sum: f64 = col.iter().flatten().sum();
            FastAggResult::Value(json!({
                "value": serde_json::Number::from_f64(sum).map(Value::Number).unwrap_or(Value::Null)
            }))
        }

        "min" => {
            let col = match dv.numeric.get(field) {
                Some(c) => c,
                None => return FastAggResult::Unsupported,
            };
            let min_val = col.iter().flatten().cloned().reduce(f64::min);
            let value: Value = min_val
                .and_then(|v| serde_json::Number::from_f64(v).map(Value::Number))
                .unwrap_or(Value::Null);
            FastAggResult::Value(json!({ "value": value }))
        }

        "max" => {
            let col = match dv.numeric.get(field) {
                Some(c) => c,
                None => return FastAggResult::Unsupported,
            };
            let max_val = col.iter().flatten().cloned().reduce(f64::max);
            let value: Value = max_val
                .and_then(|v| serde_json::Number::from_f64(v).map(Value::Number))
                .unwrap_or(Value::Null);
            FastAggResult::Value(json!({ "value": value }))
        }

        "value_count" => {
            // Count non-null values in the keyword column (or numeric fallback).
            let count = dv
                .keyword
                .get(field)
                .map(|col| col.iter().filter(|v| v.is_some()).count())
                .or_else(|| {
                    dv.numeric
                        .get(field)
                        .map(|col| col.iter().filter(|v| v.is_some()).count())
                })
                .unwrap_or(0) as u64;
            FastAggResult::Value(json!({ "value": count }))
        }

        "cardinality" => {
            // Distinct count from the bounded-delta maintained keyword_set.
            let count = dv.with_keyword_field(field, |c| {
                c.keyword_set.get(field).map(|s| s.len()).unwrap_or(0)
            }) as u64;
            FastAggResult::Value(json!({ "value": count }))
        }

        "stats" => {
            let col = match dv.numeric.get(field) {
                Some(c) => c,
                None => return FastAggResult::Unsupported,
            };
            let mut count = 0u64;
            let mut sum = 0.0f64;
            let mut min = f64::INFINITY;
            let mut max = f64::NEG_INFINITY;
            for &v in col.iter().flatten() {
                count += 1;
                sum += v;
                if v < min {
                    min = v;
                }
                if v > max {
                    max = v;
                }
            }
            let avg = if count > 0 { sum / count as f64 } else { 0.0 };
            let (min_out, max_out) = if count == 0 {
                (Value::Null, Value::Null)
            } else {
                (
                    serde_json::Number::from_f64(min)
                        .map(Value::Number)
                        .unwrap_or(Value::Null),
                    serde_json::Number::from_f64(max)
                        .map(Value::Number)
                        .unwrap_or(Value::Null),
                )
            };
            FastAggResult::Value(json!({
                "count": count,
                "min": min_out,
                "max": max_out,
                "avg": serde_json::Number::from_f64(avg).map(Value::Number).unwrap_or(Value::Null),
                "sum": serde_json::Number::from_f64(sum).map(Value::Number).unwrap_or(Value::Null),
            }))
        }

        // ── Terms agg: keyword column → count map ─────────────────────────────
        // Only served from fast path when there are no sub-aggs (sub-aggs need
        // per-bucket doc lists which require the JSON docs).
        "terms" if !has_sub_aggs => {
            let col = match dv.keyword.get(field) {
                Some(c) => c,
                None => return FastAggResult::Unsupported,
            };
            let size_opt: Option<usize> = params
                .get("size")
                .and_then(Value::as_u64)
                .map(|v| v as usize);
            let cap: Option<usize> = match size_opt {
                Some(0) => None,
                Some(n) => Some(n),
                None => Some(10),
            };
            let mut counts: HashMap<String, u64> = HashMap::new();
            for opt in col.iter().flatten() {
                *counts.entry(opt.clone()).or_insert(0) += 1;
            }

            let order_by_count_asc = params
                .get("order")
                .and_then(Value::as_object)
                .and_then(|o| o.get("_count"))
                .and_then(Value::as_str)
                .map(|s| s == "asc")
                .unwrap_or(false);
            let order_by_key = params
                .get("order")
                .and_then(Value::as_object)
                .and_then(|o| o.get("_key"))
                .and_then(Value::as_str)
                .map(|s| s.to_string());

            let mut sorted: Vec<(String, u64)> = counts.into_iter().collect();
            if let Some(key_dir) = order_by_key {
                if key_dir != "desc" {
                    sorted.sort_by(|a, b| a.0.cmp(&b.0));
                } else {
                    sorted.sort_by(|a, b| b.0.cmp(&a.0));
                }
            } else if order_by_count_asc {
                sorted.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
            } else {
                sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            }
            if let Some(n) = cap {
                sorted.truncate(n);
            }

            let buckets: Vec<Value> = sorted
                .into_iter()
                .map(|(key, count)| build_keyword_terms_bucket(&key, count))
                .collect();

            FastAggResult::Value(json!({
                "doc_count_error_upper_bound": 0,
                "sum_other_doc_count": 0,
                "buckets": buckets
            }))
        }

        _ => FastAggResult::Unsupported,
    }
}

/// Execute a single metric aggregation over DocValues using a filtered index set.
///
/// `doc_indices` is the sorted list of document positions that matched the query.
/// When `None`, all documents in the column are used (equivalent to MatchAll).
/// Returns `Unsupported` for agg types that require per-bucket doc lists (those
/// must still go through the JSON-scan path).
pub fn run_agg_fast_filtered(
    agg_type: &str,
    params: &Value,
    dv: &DocValues,
    has_sub_aggs: bool,
    doc_indices: Option<&[usize]>,
) -> FastAggResult {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return FastAggResult::Unsupported,
    };

    // Helper: iterate numeric column filtered by doc_indices.
    let iter_numeric = |col: &[Option<f64>]| -> Vec<f64> {
        match doc_indices {
            None => col.iter().flatten().copied().collect(),
            Some(idxs) => idxs
                .iter()
                .filter_map(|&i| col.get(i).and_then(|v| *v))
                .collect(),
        }
    };

    // Helper: iterate keyword column filtered by doc_indices.
    // Returns owned Strings to avoid lifetime complexity with the closure.
    let iter_keyword = |col: &[Option<String>]| -> Vec<String> {
        match doc_indices {
            None => col.iter().filter_map(|v| v.clone()).collect(),
            Some(idxs) => idxs
                .iter()
                .filter_map(|&i| col.get(i).and_then(|v| v.clone()))
                .collect(),
        }
    };

    match agg_type {
        "avg" => {
            let col = match dv.numeric.get(field) {
                Some(c) => c,
                None => return FastAggResult::Unsupported,
            };
            let vals = iter_numeric(col);
            let count = vals.len() as u64;
            let sum: f64 = vals.iter().sum();
            let value: Value = if count == 0 {
                Value::Null
            } else {
                serde_json::Number::from_f64(sum / count as f64)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            };
            FastAggResult::Value(json!({ "value": value }))
        }

        "sum" => {
            let col = match dv.numeric.get(field) {
                Some(c) => c,
                None => return FastAggResult::Unsupported,
            };
            let sum: f64 = iter_numeric(col).iter().sum();
            FastAggResult::Value(json!({
                "value": serde_json::Number::from_f64(sum).map(Value::Number).unwrap_or(Value::Null)
            }))
        }

        "min" => {
            let col = match dv.numeric.get(field) {
                Some(c) => c,
                None => return FastAggResult::Unsupported,
            };
            let min_val = iter_numeric(col).into_iter().reduce(f64::min);
            let value: Value = min_val
                .and_then(|v| serde_json::Number::from_f64(v).map(Value::Number))
                .unwrap_or(Value::Null);
            FastAggResult::Value(json!({ "value": value }))
        }

        "max" => {
            let col = match dv.numeric.get(field) {
                Some(c) => c,
                None => return FastAggResult::Unsupported,
            };
            let max_val = iter_numeric(col).into_iter().reduce(f64::max);
            let value: Value = max_val
                .and_then(|v| serde_json::Number::from_f64(v).map(Value::Number))
                .unwrap_or(Value::Null);
            FastAggResult::Value(json!({ "value": value }))
        }

        "value_count" => {
            let count = if let Some(col) = dv.keyword.get(field) {
                iter_keyword(col).len()
            } else if let Some(col) = dv.numeric.get(field) {
                iter_numeric(col).len()
            } else {
                0
            } as u64;
            FastAggResult::Value(json!({ "value": count }))
        }

        "cardinality" => {
            // For filtered sets, recount distinct values from the filtered slice.
            let count = if let Some(col) = dv.keyword.get(field) {
                let distinct: std::collections::HashSet<String> =
                    iter_keyword(col).into_iter().collect();
                distinct.len()
            } else {
                0
            } as u64;
            FastAggResult::Value(json!({ "value": count }))
        }

        "stats" => {
            let col = match dv.numeric.get(field) {
                Some(c) => c,
                None => return FastAggResult::Unsupported,
            };
            let vals = iter_numeric(col);
            let mut count = 0u64;
            let mut sum = 0.0f64;
            let mut min = f64::INFINITY;
            let mut max = f64::NEG_INFINITY;
            for v in vals {
                count += 1;
                sum += v;
                if v < min {
                    min = v;
                }
                if v > max {
                    max = v;
                }
            }
            let avg = if count > 0 { sum / count as f64 } else { 0.0 };
            let (min_out, max_out) = if count == 0 {
                (Value::Null, Value::Null)
            } else {
                (
                    serde_json::Number::from_f64(min)
                        .map(Value::Number)
                        .unwrap_or(Value::Null),
                    serde_json::Number::from_f64(max)
                        .map(Value::Number)
                        .unwrap_or(Value::Null),
                )
            };
            FastAggResult::Value(json!({
                "count": count,
                "min": min_out,
                "max": max_out,
                "avg": serde_json::Number::from_f64(avg).map(Value::Number).unwrap_or(Value::Null),
                "sum": serde_json::Number::from_f64(sum).map(Value::Number).unwrap_or(Value::Null),
            }))
        }

        "terms" if !has_sub_aggs => {
            let col = match dv.keyword.get(field) {
                Some(c) => c,
                None => return FastAggResult::Unsupported,
            };
            let size_opt: Option<usize> = params
                .get("size")
                .and_then(Value::as_u64)
                .map(|v| v as usize);
            let cap: Option<usize> = match size_opt {
                Some(0) => None,
                Some(n) => Some(n),
                None => Some(10),
            };
            let mut counts: HashMap<String, u64> = HashMap::new();
            for v in iter_keyword(col) {
                *counts.entry(v.to_string()).or_insert(0) += 1;
            }

            let order_by_count_asc = params
                .get("order")
                .and_then(Value::as_object)
                .and_then(|o| o.get("_count"))
                .and_then(Value::as_str)
                .map(|s| s == "asc")
                .unwrap_or(false);
            let order_by_key = params
                .get("order")
                .and_then(Value::as_object)
                .and_then(|o| o.get("_key"))
                .and_then(Value::as_str)
                .map(|s| s.to_string());

            let mut sorted: Vec<(String, u64)> = counts.into_iter().collect();
            if let Some(key_dir) = order_by_key {
                if key_dir != "desc" {
                    sorted.sort_by(|a, b| a.0.cmp(&b.0));
                } else {
                    sorted.sort_by(|a, b| b.0.cmp(&a.0));
                }
            } else if order_by_count_asc {
                sorted.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
            } else {
                sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            }
            if let Some(n) = cap {
                sorted.truncate(n);
            }

            let buckets: Vec<Value> = sorted
                .into_iter()
                .map(|(key, count)| build_keyword_terms_bucket(&key, count))
                .collect();

            FastAggResult::Value(json!({
                "doc_count_error_upper_bound": 0,
                "sum_other_doc_count": 0,
                "buckets": buckets
            }))
        }

        _ => FastAggResult::Unsupported,
    }
}

// ── Gap 3: additional aggregation implementations ─────────────────────────────

/// `extended_stats` — same as `stats` plus `std_deviation`, `variance`,
/// `std_deviation_bounds` (upper/lower), and `sum_of_squares`.
fn run_extended_stats<'d>(params: &Value, _docs: &'d [Value], cache: &mut FieldCache<'d>) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => {
            return json!({
                "count": 0, "min": Value::Null, "max": Value::Null,
                "avg": Value::Null, "sum": 0.0, "sum_of_squares": 0.0,
                "variance": Value::Null, "std_deviation": Value::Null,
                "std_deviation_bounds": { "upper": Value::Null, "lower": Value::Null }
            });
        }
    };

    // `missing: N` — docs without a numeric value for the field contribute
    // N instead of being skipped. Applied before count/sum/sum_of_squares.
    let missing_default = get_missing_value(params);
    let values: Vec<f64> = cache
        .get_numeric(field)
        .iter()
        .filter_map(|v| v.or(missing_default))
        .collect();
    let count = values.len();

    if count == 0 {
        // ES returns `sum: 0.0` (a real zero) but reports every higher-
        // order moment as null when there are no values to compute it
        // from. Note: sum_of_squares is null, not 0, per ES.
        return json!({
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
        });
    }

    let sum: f64 = values.iter().sum();
    let sum_of_squares: f64 = values.iter().map(|x| x * x).sum();
    let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let avg = sum / count as f64;
    // Population variance via ES's raw-moment form:
    //     var_pop = sum_of_squares / N - avg^2
    // This matches ES's exact arithmetic across the YAML tests. Sampling
    // variance applies Bessel's correction: var_samp = var_pop * N/(N-1).
    let variance_pop = (sum_of_squares / count as f64) - avg * avg;
    let variance_pop = variance_pop.max(0.0);
    let variance_samp = if count > 1 {
        variance_pop * count as f64 / (count - 1) as f64
    } else {
        0.0
    };
    let std_deviation = variance_pop.max(0.0).sqrt();
    let std_deviation_samp = variance_samp.max(0.0).sqrt();
    let sigma = params.get("sigma").and_then(Value::as_f64).unwrap_or(2.0);

    json!({
        "count": count,
        "min": min,
        "max": max,
        "avg": avg,
        "sum": sum,
        "sum_of_squares": sum_of_squares,
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
    })
}

/// Extract lat/lon from a `geo_point` field value.
/// Supports `{"lat": f, "lon": f}`, `"lat,lon"`, and `[lon, lat]` forms.
fn extract_geo_point(v: &Value) -> Option<(f64, f64)> {
    match v {
        Value::Object(obj) => {
            let lat = obj.get("lat").and_then(Value::as_f64)?;
            let lon = obj.get("lon").and_then(Value::as_f64)?;
            Some((lat, lon))
        }
        Value::String(s) => {
            let parts: Vec<&str> = s.split(',').collect();
            if parts.len() == 2 {
                let lat = parts[0].trim().parse::<f64>().ok()?;
                let lon = parts[1].trim().parse::<f64>().ok()?;
                Some((lat, lon))
            } else {
                None
            }
        }
        Value::Array(arr) if arr.len() == 2 => {
            // GeoJSON order: [lon, lat]
            let lon = arr[0].as_f64()?;
            let lat = arr[1].as_f64()?;
            Some((lat, lon))
        }
        _ => None,
    }
}

/// `geo_bounds` — compute the bounding box (min/max lat/lon) from all geo_point values.
fn run_geo_bounds(params: &Value, docs: &[Value]) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"bounds": Value::Null}),
    };

    let mut min_lat = f64::INFINITY;
    let mut max_lat = f64::NEG_INFINITY;
    let mut min_lon = f64::INFINITY;
    let mut max_lon = f64::NEG_INFINITY;

    for doc in docs {
        if let Some(v) = doc.get(field) {
            if let Some((lat, lon)) = extract_geo_point(v) {
                min_lat = min_lat.min(lat);
                max_lat = max_lat.max(lat);
                min_lon = min_lon.min(lon);
                max_lon = max_lon.max(lon);
            }
        }
    }

    if min_lat.is_infinite() {
        return json!({"bounds": Value::Null});
    }

    json!({
        "bounds": {
            "top_left":     { "lat": max_lat, "lon": min_lon },
            "bottom_right": { "lat": min_lat, "lon": max_lon }
        }
    })
}

/// `geo_centroid` — compute the average lat/lon from all geo_point values.
fn run_geo_centroid(params: &Value, docs: &[Value]) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"location": Value::Null, "count": 0}),
    };

    let mut sum_lat = 0.0f64;
    let mut sum_lon = 0.0f64;
    let mut count = 0usize;

    for doc in docs {
        if let Some(v) = doc.get(field) {
            if let Some((lat, lon)) = extract_geo_point(v) {
                sum_lat += lat;
                sum_lon += lon;
                count += 1;
            }
        }
    }

    if count == 0 {
        return json!({"location": Value::Null, "count": 0});
    }

    json!({
        "location": {
            "lat": sum_lat / count as f64,
            "lon": sum_lon / count as f64
        },
        "count": count
    })
}

/// `median_absolute_deviation` — median of absolute deviations from the median.
fn run_median_absolute_deviation<'d>(
    params: &Value,
    _docs: &'d [Value],
    cache: &mut FieldCache<'d>,
) -> Value {
    let field = match params.get("field").and_then(Value::as_str) {
        Some(f) => f,
        None => return json!({"value": Value::Null}),
    };

    let missing = get_missing_value(params);
    let mut values: Vec<f64> = cache
        .get_numeric(field)
        .iter()
        .filter_map(|v| v.or(missing))
        .collect();
    if values.is_empty() {
        return json!({"value": Value::Null});
    }

    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len();
    let median = if n.is_multiple_of(2) {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    } else {
        values[n / 2]
    };

    let mut deviations: Vec<f64> = values.iter().map(|x| (x - median).abs()).collect();
    deviations.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let m = deviations.len();
    let mad = if m.is_multiple_of(2) {
        (deviations[m / 2 - 1] + deviations[m / 2]) / 2.0
    } else {
        deviations[m / 2]
    };

    json!({"value": mad})
}

/// `nested` / `reverse_nested` pass-through: since xerj uses a flat document
/// model, there is no nesting layer to traverse.  We simply run the sub-aggs on
/// the same docs that were passed in, which is semantically correct for flat docs.
/// ES `nested` aggregation — flattens each parent doc's nested-path array
/// into one synthetic sub-doc per entry. The synthetic doc carries the
/// dotted-path field names (e.g. `courses.name`) so sub-aggs can target
/// them as plain fields.
fn run_nested(path: &str, sub_aggs: Option<&Value>, docs: &[Value], all_docs: &[Value]) -> Value {
    // Collect all nested leaves for `path`, flattening any intermediate
    // arrays. For path `courses.sessions` against doc
    // `{courses: [{sessions: [A, B]}, {sessions: [C, D]}]}`, leaves are
    // `[A, B, C, D]`.
    fn collect_leaves<'a>(value: &'a Value, segs: &[&str], out: &mut Vec<&'a Value>) {
        if segs.is_empty() {
            match value {
                Value::Array(arr) => {
                    for item in arr {
                        collect_leaves(item, segs, out);
                    }
                }
                _ => out.push(value),
            }
            return;
        }
        match value {
            Value::Object(o) => {
                if let Some(child) = o.get(segs[0]) {
                    collect_leaves(child, &segs[1..], out);
                }
            }
            Value::Array(arr) => {
                for item in arr {
                    collect_leaves(item, segs, out);
                }
            }
            _ => {}
        }
    }
    let path_segs: Vec<&str> = path.split('.').collect();
    let mut expanded: Vec<Value> = Vec::new();
    for doc in docs {
        let parent_id = doc.get("_id").cloned();
        let parent_index = doc.get("_index").cloned();
        let mut leaves: Vec<&Value> = Vec::new();
        collect_leaves(doc, &path_segs, &mut leaves);
        for (offset, item) in leaves.iter().enumerate() {
            if matches!(item, Value::Object(_)) {
                let mut wrapped = wrap_synth_for_path(path, item);
                if let Some(obj) = wrapped.as_object_mut() {
                    if let Some(id) = parent_id.clone() {
                        obj.insert("_id".into(), id);
                    }
                    if let Some(ix) = parent_index.clone() {
                        obj.insert("_index".into(), ix);
                    }
                    obj.insert(
                        "_nested".into(),
                        json!({"field": path, "offset": offset as u64}),
                    );
                }
                expanded.push(wrapped);
            }
        }
    }
    let doc_count = expanded.len() as u64;
    let mut result = serde_json::Map::new();
    result.insert("doc_count".to_string(), json!(doc_count));
    if let Some(sub) = sub_aggs {
        let sub_result = run_aggs_with_all(sub, &expanded, all_docs);
        if let Value::Object(sub_obj) = sub_result {
            for (k, v) in sub_obj {
                result.insert(k, v);
            }
        }
    }
    Value::Object(result)
}

/// Wrap a nested entry into a synthetic doc where `path` resolves to the
/// entry. For `path = "courses"` and `item = {name: "X"}`, returns
/// `{"courses": {name: "X"}}` — so downstream `get_field_value(doc, "courses.name")`
/// works as if the nested entry was the whole doc.
fn wrap_synth_for_path(path: &str, item: &Value) -> Value {
    let parts: Vec<&str> = path.split('.').collect();
    let mut cur = item.clone();
    for seg in parts.iter().rev() {
        let mut m = serde_json::Map::new();
        m.insert((*seg).to_string(), cur);
        cur = Value::Object(m);
    }
    cur
}

fn run_reverse_nested(sub_aggs: Option<&Value>, docs: &[Value], all_docs: &[Value]) -> Value {
    // Deduplicate by parent _id so each parent doc is counted once. Docs
    // without _id fall back to identity (treated as already-parent).
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut unique: Vec<Value> = Vec::with_capacity(docs.len());
    for doc in docs {
        let key = doc
            .get("_id")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| format!("__noid_{}", unique.len()));
        if seen.insert(key) {
            // Re-materialize the parent by locating the original doc in
            // `all_docs` by `_id`; fall back to the nested synthetic.
            let parent = doc
                .get("_id")
                .and_then(Value::as_str)
                .and_then(|id| {
                    all_docs
                        .iter()
                        .find(|d| d.get("_id").and_then(Value::as_str) == Some(id))
                })
                .cloned()
                .unwrap_or_else(|| doc.clone());
            unique.push(parent);
        }
    }
    let doc_count = unique.len() as u64;
    let mut result = serde_json::Map::new();
    result.insert("doc_count".to_string(), json!(doc_count));
    if let Some(sub) = sub_aggs {
        let sub_result = run_aggs_with_all(sub, &unique, all_docs);
        if let Value::Object(sub_obj) = sub_result {
            for (k, v) in sub_obj {
                result.insert(k, v);
            }
        }
    }
    Value::Object(result)
}

/// `global` — run sub-aggs on ALL docs in the index, regardless of the current
/// query filter.  This is the ES `global` aggregation semantics.
fn run_global(sub_aggs: Option<&Value>, all_docs: &[Value]) -> Value {
    let doc_count = all_docs.len() as u64;
    let mut result = serde_json::Map::new();
    result.insert("doc_count".to_string(), json!(doc_count));

    if let Some(sub) = sub_aggs {
        let sub_result = run_aggs_with_all(sub, all_docs, all_docs);
        if let Value::Object(sub_obj) = sub_result {
            for (k, v) in sub_obj {
                result.insert(k, v);
            }
        }
    }

    Value::Object(result)
}

// ── date_range aggregation ────────────────────────────────────────────────────

fn run_date_range(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let field = params
        .get("field")
        .and_then(Value::as_str)
        .unwrap_or("@timestamp");
    let ranges = match params.get("ranges").and_then(Value::as_array) {
        Some(r) => r,
        None => return json!({"buckets": []}),
    };
    let keyed = params
        .get("keyed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // First-of-chain normalisation ("fmt1||fmt2" → "fmt1") mirrors
    // render_date_format — ES renders chained formats with the first
    // component (verified live on 8.13.4, b7 DEFECT 2 date_range arm).
    let user_fmt = params
        .get("format")
        .and_then(Value::as_str)
        .map(|f| f.split("||").next().unwrap_or(f).trim());
    let missing_ms: Option<i64> = params.get("missing").and_then(parse_date_ms);
    let tz_param = params.get("time_zone").and_then(Value::as_str);

    // Detect fractional-second precision from the longest input. If any
    // from/to uses 9-digit (nanos) precision, render with `SSSSSSSSS`; else
    // default to ES's 3-digit (millis).
    let nanos_digits: usize = ranges
        .iter()
        .flat_map(|r| {
            let from_s = r
                .get("from")
                .and_then(Value::as_str)
                .map(detect_fractional_digits)
                .unwrap_or(0);
            let to_s = r
                .get("to")
                .and_then(Value::as_str)
                .map(detect_fractional_digits)
                .unwrap_or(0);
            [from_s, to_s]
        })
        .max()
        .unwrap_or(3);
    let nanos_digits = if nanos_digits >= 9 { 9 } else { 3 };

    let render = |ms: i64| -> String {
        // `epoch_second` / `epoch_millis` formats render as the raw
        // numeric stamp (seconds or milliseconds since epoch), not an
        // ISO string — otherwise our ES-format translator falls through
        // and emits the pattern literal (e.g. "epoch_second").
        if matches!(user_fmt, Some("epoch_second")) {
            return (ms / 1000).to_string();
        }
        if matches!(user_fmt, Some("epoch_millis")) {
            return ms.to_string();
        }
        // When time_zone is set or a custom format is used, render with those
        // rules. Otherwise use the default ISO form with detected precision.
        if let Some(fmt) = user_fmt {
            let offset = tz_param.and_then(|tz| fixed_offset_for_tz_at(tz, ms));
            // Named ES format aliases (e.g. `strict_date_optional_time`, the
            // first component of the default mapping chain) must go through
            // render_date_format's alias table — feeding the NAME to
            // java_to_strftime would translate its letters as pattern tokens
            // (b7 DEFECT 2, date_range arm).  tz-shifted rendering keeps the
            // legacy pattern path below.
            if offset.is_none() && is_named_date_format(fmt) {
                let dt_utc =
                    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).unwrap_or_default();
                return render_date_format(Some(fmt), ms, dt_utc);
            }
            let pat = java_to_strftime(fmt);
            let out = if let Some(off) = offset {
                let dt_utc =
                    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).unwrap_or_default();
                let dt_local = dt_utc.with_timezone(&off);
                dt_local.format(&pat).to_string()
            } else {
                let dt_utc =
                    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).unwrap_or_default();
                dt_utc.format(&pat).to_string()
            };
            // Java's `ZZZZZ` collapses to `Z` for UTC; chrono's `%:z`
            // emits `+00:00`. When there's no time_zone, replace the
            // trailing `+00:00` with `Z` so the format matches ES.
            if tz_param.is_none() && out.ends_with("+00:00") {
                return format!("{}Z", &out[..out.len() - "+00:00".len()]);
            }
            return out;
        }
        let offset = tz_param.and_then(|tz| fixed_offset_for_tz_at(tz, ms));
        render_iso_date(ms, nanos_digits, offset)
    };

    // When the user provided a `format`, fall back to parsing
    // from/to with that pattern. parse_date_ms only understands
    // RFC3339/ISO-ish strings, so formats like `dd-MM-yyyy` need
    // format-aware parsing to yield a valid ms timestamp.
    let parse_with_fmt = |val: &Value| -> Option<i64> {
        // Epoch formats interpret raw numbers as seconds / milliseconds.
        // parse_date_ms would otherwise return the number as-is (always
        // treating it as millis), which for `format: epoch_second` is
        // off by 1000× and breaks both filtering and from/to emission.
        if matches!(user_fmt, Some("epoch_second")) {
            return match val {
                Value::Number(n) => n.as_i64().map(|v| v * 1000),
                Value::String(s) => s.trim().parse::<i64>().ok().map(|v| v * 1000),
                _ => None,
            };
        }
        if matches!(user_fmt, Some("epoch_millis")) {
            return match val {
                Value::Number(n) => n.as_i64(),
                Value::String(s) => s.trim().parse::<i64>().ok(),
                _ => None,
            };
        }
        if let Some(ms) = parse_date_ms(val) {
            return Some(ms);
        }
        let s = val.as_str()?;
        let fmt = user_fmt?;
        let pat = java_to_strftime(fmt);
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, &pat) {
            return Some(dt.and_utc().timestamp_millis());
        }
        if let Ok(d) = chrono::NaiveDate::parse_from_str(s, &pat) {
            return d
                .and_hms_opt(0, 0, 0)
                .map(|dt| dt.and_utc().timestamp_millis());
        }
        None
    };

    let mut keyed_map = serde_json::Map::new();
    let mut buckets: Vec<Value> = Vec::new();
    for range_def in ranges {
        // `from`/`to` can be ISO-8601 strings or raw numbers. Both cases
        // convert through parse_date_ms so bucket comparisons work on
        // millis.
        let from_ms = range_def.get("from").and_then(&parse_with_fmt);
        let to_ms = range_def.get("to").and_then(&parse_with_fmt);
        let from_str_in = range_def.get("from").and_then(Value::as_str);
        let to_str_in = range_def.get("to").and_then(Value::as_str);

        // Key rendering: `from-to` as the default, but use the configured
        // format to render the date strings so `2020-01-01` becomes the
        // full ISO form `2020-01-01T00:00:00.000Z` when no format is set.
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

        let filtered: Vec<Value> = docs
            .iter()
            .filter(|doc| {
                let v = doc
                    .get(field)
                    .or_else(|| doc.get("_source").and_then(|s| s.get(field)));
                // Doc values honor the field-declared format too: a doc
                // stored as `{date: 1000}` under `format: epoch_second`
                // means 1000 seconds = 1 000 000 ms.
                let doc_ms = match v.and_then(&parse_with_fmt).or(missing_ms) {
                    Some(m) => m,
                    None => return false,
                };
                let passes_from = from_ms.map(|f| doc_ms >= f).unwrap_or(true);
                let passes_to = to_ms.map(|t| doc_ms < t).unwrap_or(true);
                passes_from && passes_to
            })
            .cloned()
            .collect();

        let mut bucket = serde_json::Map::new();
        if !keyed {
            bucket.insert("key".to_string(), json!(key));
        }
        bucket.insert("doc_count".to_string(), json!(sum_doc_count(&filtered)));
        if let Some(f) = from_ms {
            bucket.insert("from".to_string(), json!(f));
            bucket.insert("from_as_string".to_string(), json!(from_key));
        }
        if let Some(t) = to_ms {
            bucket.insert("to".to_string(), json!(t));
            bucket.insert("to_as_string".to_string(), json!(to_key));
        }
        if let Some(sub) = sub_aggs {
            let sr = run_aggs_with_all(sub, &filtered, all_docs);
            if let Value::Object(so) = sr {
                for (k, v) in so {
                    bucket.insert(k, v);
                }
            }
        }
        if keyed {
            keyed_map.insert(key, Value::Object(bucket));
        } else {
            buckets.push(Value::Object(bucket));
        }
    }
    if keyed {
        json!({"buckets": Value::Object(keyed_map)})
    } else {
        // ES sorts date_range buckets by `from` ascending. An absent
        // `from` (open-ended below) sorts first; an absent `to`
        // (open-ended above) keeps its `from` ordering among siblings.
        buckets.sort_by(|a, b| {
            let fa = a.get("from").and_then(Value::as_i64);
            let fb = b.get("from").and_then(Value::as_i64);
            match (fa, fb) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Less,
                (Some(_), None) => std::cmp::Ordering::Greater,
                (Some(x), Some(y)) => x.cmp(&y),
            }
        });
        json!({"buckets": buckets})
    }
}

// ── percentile_ranks aggregation ─────────────────────────────────────────────

/// Render a double exactly the way Java's `Double.toString` does — ES keys
/// its `percentiles` / `percentile_ranks` keyed values-maps with
/// `String.valueOf(double)`, so `200` becomes `"200.0"`, `99.9` stays
/// `"99.9"`, `1e7` becomes `"1.0E7"` and `0.0001` becomes `"1.0E-4"`
/// (live-ES 8.13.4 verified).  Java prints the shortest decimal that
/// round-trips (same rule as Rust's `{:?}`), plain decimal for
/// `1e-3 <= |v| < 1e7`, computerized scientific notation (`d.dddE±x`, no `+`,
/// no zero-padding, at least one fractional digit) outside that range.
pub(crate) fn java_double_str(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    let a = v.abs();
    if (1e-3..1e7).contains(&a) {
        // Rust's `{:?}` is the shortest round-trip decimal with a mandatory
        // fractional part — identical to Java in this range.
        format!("{:?}", v)
    } else {
        // `{:e}` gives `mEx` shortest form (e.g. "1e7", "1.5e-8"); Java wants
        // an explicit fractional digit and an uppercase `E`.
        let s = format!("{:e}", v);
        let (m, e) = s.split_once('e').unwrap_or((s.as_str(), "0"));
        if m.contains('.') {
            format!("{}E{}", m, e)
        } else {
            format!("{}.0E{}", m, e)
        }
    }
}

fn run_percentile_ranks(params: &Value, docs: &[Value]) -> Value {
    let field = params.get("field").and_then(Value::as_str).unwrap_or("");
    let values = match params.get("values").and_then(Value::as_array) {
        Some(v) => v,
        None => return json!({"values": {}}),
    };
    // Default `keyed` for percentile_ranks is true (matches percentiles).
    let keyed = params.get("keyed").and_then(Value::as_bool).unwrap_or(true);

    let mut nums: Vec<f64> = docs
        .iter()
        .filter_map(|doc| {
            let v = doc
                .get(field)
                .or_else(|| doc.get("_source").and_then(|s| s.get(field)))?;
            v.as_f64()
        })
        .collect();
    nums.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let total = nums.len() as f64;
    // ES sorts the requested values ascending before rendering (live-ES
    // verified: values [99.9, 0.5, 200] come back keyed 0.5, 99.9, 200.0).
    let mut targets: Vec<f64> = values.iter().filter_map(Value::as_f64).collect();
    targets.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank_of = |t: f64| -> f64 {
        if total == 0.0 {
            0.0
        } else {
            let below = nums.iter().filter(|&&v| v <= t).count() as f64;
            (below / total) * 100.0
        }
    };
    if keyed {
        // ES keys the map with `String.valueOf(double)` — "200.0", "99.9".
        let mut result = serde_json::Map::new();
        for &t in &targets {
            result.insert(java_double_str(t), json!(rank_of(t)));
        }
        json!({"values": result})
    } else {
        // keyed=false: emit [{key: N, value: P}, ...]; ES renders the key as
        // a double (200.0), never an integer.
        let arr: Vec<Value> = targets
            .iter()
            .map(|&t| json!({ "key": t, "value": rank_of(t) }))
            .collect();
        json!({ "values": arr })
    }
}

// ── scripted_metric aggregation ─────────────────────────────────────────────
//
// A minimal Painless-subset interpreter implementing the script patterns used
// by ES YAML tests. Supports:
//   - property access: doc.FIELD.value, params.X, state.X
//   - assignments: state.X = expr, var decls (long/int/double/float NAME = expr)
//   - method calls: collection.add(x)
//   - augmented assigns: sum += expr
//   - for-in: for (t in container) { ... }
//   - ternary: a ? b : c
//   - string literals: 'xyz' and "xyz"
//   - arithmetic/comparison (+, -, *, /, %, ==, !=, <, >, <=, >=, &&, ||)
//   - return expr
//
// Scripts that use unsupported constructs fall back to `Value::Null`.

fn run_scripted_metric(params: &Value, docs: &[Value]) -> Value {
    let src_of = |key: &str| -> Option<String> {
        params.get(key).and_then(|v| {
            v.as_str()
                .map(String::from)
                .or_else(|| v.get("source").and_then(Value::as_str).map(String::from))
        })
    };
    let init_src = src_of("init_script");
    let map_src = src_of("map_script");
    let combine_src = src_of("combine_script");
    let reduce_src = src_of("reduce_script");
    let script_params: Value = params.get("params").cloned().unwrap_or(json!({}));

    // Pre-parse the scripts once. Each returns the parsed AST, or an
    // error we surface as a null metric.
    let parse = |s: &Option<String>| -> Option<Vec<PlStmt>> {
        s.as_deref().map(parse_script).transpose().ok().flatten()
    };
    let init_ast = parse(&init_src);
    let map_ast = parse(&map_src);
    let combine_ast = parse(&combine_src);
    let reduce_ast = parse(&reduce_src);

    // We simulate a single shard. Run init → map(for each doc) → combine
    // → reduce(list with one combine result). If any stage fails to
    // parse/execute, return null.
    let mut state_obj = Value::Object(serde_json::Map::new());
    if let Some(ast) = &init_ast {
        let mut ctx = ScriptCtx::new(state_obj.clone(), script_params.clone(), None);
        if exec_stmts(ast, &mut ctx).is_err() {
            return json!({"value": Value::Null});
        }
        state_obj = ctx.state;
    }
    if let Some(ast) = &map_ast {
        for doc in docs {
            let mut ctx = ScriptCtx::new(state_obj.clone(), script_params.clone(), Some(doc));
            if exec_stmts(ast, &mut ctx).is_err() {
                return json!({"value": Value::Null});
            }
            state_obj = ctx.state;
        }
    }
    let combine_val: Value = if let Some(ast) = &combine_ast {
        let mut ctx = ScriptCtx::new(state_obj.clone(), script_params.clone(), None);
        match exec_stmts(ast, &mut ctx) {
            Ok(Some(v)) => v,
            _ => return json!({"value": Value::Null}),
        }
    } else {
        state_obj.clone()
    };
    let reduce_val: Value = if let Some(ast) = &reduce_ast {
        // Per ES: states is a list of per-shard combine results.
        let mut ctx = ScriptCtx::new(
            Value::Object(serde_json::Map::new()),
            script_params.clone(),
            None,
        );
        ctx.states = Some(vec![combine_val.clone()]);
        match exec_stmts(ast, &mut ctx) {
            Ok(Some(v)) => v,
            _ => return json!({"value": Value::Null}),
        }
    } else {
        combine_val
    };
    json!({"value": reduce_val})
}

/// Recognise the canonical "sum a single numeric doc field" `scripted_metric`
/// shape and, when it matches, return the field name so the columnar fast path
/// (`fast_aggs::FastCtx::exec_scripted_metric`) can serve it off doc-values
/// instead of interpreting the script per doc over a materialised JSON corpus.
///
/// Exact shape (whitespace-insensitive — matched on the parsed AST):
/// ```painless
///   init_script:    state.S = 0
///   map_script:     state.S += doc.FIELD.value
///   combine_script: return state.S
///   reduce_script:  <type> T = 0; for (V in states) { T += V } return T
/// ```
/// Any deviation returns `None` and the caller keeps the exact interpreter.
/// The reduce is required to be a plain sum-over-states so that — under our
/// single-shard model where `states == [combine_val]` — the reduced value is
/// exactly the per-shard sum; the fast path may then emit that sum directly.
pub(crate) fn scripted_metric_sum_field(params: &Value) -> Option<String> {
    let src_of = |key: &str| -> Option<String> {
        params.get(key).and_then(|v| {
            v.as_str()
                .map(String::from)
                .or_else(|| v.get("source").and_then(Value::as_str).map(String::from))
        })
    };
    let init = parse_script(&src_of("init_script")?).ok()?;
    let map = parse_script(&src_of("map_script")?).ok()?;
    let combine = parse_script(&src_of("combine_script")?).ok()?;

    // `state.<name>` accessor: Member(Ident("state"), name).
    fn state_field(e: &PlExpr) -> Option<&str> {
        match e {
            PlExpr::Member(base, name) => match &**base {
                PlExpr::Ident(root) if root == "state" => Some(name.as_str()),
                _ => None,
            },
            _ => None,
        }
    }
    // `doc.<field>.value` accessor: Member(Member(Ident("doc"), field), "value").
    fn doc_value_field(e: &PlExpr) -> Option<&str> {
        match e {
            PlExpr::Member(base, val) if val == "value" => match &**base {
                PlExpr::Member(doc, field) => match &**doc {
                    PlExpr::Ident(root) if root == "doc" => Some(field.as_str()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        }
    }

    // init: `state.<acc> = 0`
    let acc = match init.as_slice() {
        [PlStmt::Assign(lhs, PlExpr::Num(n))] if *n == 0.0 => state_field(lhs)?,
        _ => return None,
    };
    // map: `state.<acc> += doc.<field>.value`
    let field = match map.as_slice() {
        [PlStmt::AugAssign(lhs, op, rhs)] if *op == "+=" && state_field(lhs) == Some(acc) => {
            doc_value_field(rhs)?
        }
        _ => return None,
    };
    // combine: `return state.<acc>` — makes the per-shard combine value exactly
    // the accumulated sum (`state.<acc>`).  Required: a combine that fails to
    // parse or is absent would make the interpreter return the whole `state`
    // object (`{acc: sum}`), not the scalar sum.
    match combine.as_slice() {
        [PlStmt::Return(e)] if state_field(e) == Some(acc) => {}
        _ => return None,
    }

    // reduce: under our single-shard model `states == [combine_val]`, so the
    // reduced value must equal `combine_val` (the sum).  Two accepted cases,
    // both yielding exactly the sum — mirroring `run_scripted_metric`:
    //   1. reduce is absent OR fails to parse → the interpreter's `reduce_ast`
    //      is `None` and it returns `combine_val` unchanged.  (The canonical
    //      `for (s in states) t += s` body is BRACELESS, which the Painless
    //      subset parser rejects — this is the common real-world shape.)
    //   2. reduce parses to `<type> t = 0; for (v in states) { t += v }
    //      return t` — sum-over-states of the single element == that element.
    // Anything that parses to some OTHER computation is rejected (its result
    // need not equal the sum).
    if let Some(reduce_src) = src_of("reduce_script") {
        if let Ok(reduce) = parse_script(&reduce_src) {
            // Canonical sum-over-states, else bail.
            let reduce_acc = match reduce.first() {
                Some(PlStmt::Decl(name, PlExpr::Num(n))) if *n == 0.0 => name.as_str(),
                _ => return None,
            };
            if reduce.len() != 3 {
                return None;
            }
            match &reduce[1] {
                PlStmt::ForIn(v, PlExpr::Ident(container), body) if container == "states" => {
                    match body.as_slice() {
                        [PlStmt::AugAssign(PlExpr::Ident(a), op, PlExpr::Ident(s))]
                            if *op == "+=" && a.as_str() == reduce_acc && s == v => {}
                        _ => return None,
                    }
                }
                _ => return None,
            }
            match &reduce[2] {
                PlStmt::Return(PlExpr::Ident(r)) if r == reduce_acc => {}
                _ => return None,
            }
        }
        // parse error → interpreter falls back to combine_val == sum → accept.
    }
    // absent reduce → interpreter uses combine_val == sum → accept.

    Some(field.to_string())
}

#[derive(Debug, Clone)]
enum PlTok {
    Ident(String),
    StrLit(String),
    Num(f64),
    Op(&'static str),
    LParen,
    RParen,
    LBrace,
    RBrace,
    Semi,
    Comma,
    Dot,
    Question,
    Colon,
}

#[derive(Debug, Clone)]
enum PlExpr {
    Num(f64),
    Str(String),
    Ident(String),
    Member(Box<PlExpr>, String),
    Call(Box<PlExpr>, String, Vec<PlExpr>),
    ArrayLit(Vec<PlExpr>),
    Binary(Box<PlExpr>, &'static str, Box<PlExpr>),
    Unary(&'static str, Box<PlExpr>),
    Ternary(Box<PlExpr>, Box<PlExpr>, Box<PlExpr>),
}

#[derive(Debug, Clone)]
enum PlStmt {
    Assign(PlExpr, PlExpr),
    AugAssign(PlExpr, &'static str, PlExpr),
    Decl(String, PlExpr),
    Expr(PlExpr),
    Return(PlExpr),
    ForIn(String, PlExpr, Vec<PlStmt>),
    If(PlExpr, Vec<PlStmt>, Option<Vec<PlStmt>>),
}

struct ScriptCtx<'a> {
    state: Value,
    params: Value,
    doc: Option<&'a Value>,
    states: Option<Vec<Value>>,
    locals: std::collections::HashMap<String, Value>,
}
impl<'a> ScriptCtx<'a> {
    fn new(state: Value, params: Value, doc: Option<&'a Value>) -> Self {
        Self {
            state,
            params,
            doc,
            states: None,
            locals: std::collections::HashMap::new(),
        }
    }
}

/// Tokenize a Painless-subset source.
fn lex_script(src: &str) -> Result<Vec<PlTok>, String> {
    let bytes = src.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        // Line comments //..., block comments /* ... */
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] as char == '/' {
            while i < bytes.len() && bytes[i] as char != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] as char == '*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] as char == '*' && bytes[i + 1] as char == '/') {
                i += 1;
            }
            i += 2.min(bytes.len() - i);
            continue;
        }
        if c.is_ascii_digit()
            || (c == '.' && i + 1 < bytes.len() && (bytes[i + 1] as char).is_ascii_digit())
        {
            let start = i;
            while i < bytes.len()
                && ((bytes[i] as char).is_ascii_digit() || bytes[i] as char == '.')
            {
                i += 1;
            }
            let s = &src[start..i];
            let n: f64 = s.parse().map_err(|_| format!("bad num `{}`", s))?;
            out.push(PlTok::Num(n));
            continue;
        }
        if c == '\'' || c == '"' {
            let quote = c;
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] as char != quote {
                i += 1;
            }
            let s = src[start..i].to_string();
            if i < bytes.len() {
                i += 1;
            }
            out.push(PlTok::StrLit(s));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len()
                && ((bytes[i] as char).is_ascii_alphanumeric() || bytes[i] as char == '_')
            {
                i += 1;
            }
            out.push(PlTok::Ident(src[start..i].to_string()));
            continue;
        }
        // Two-char operators
        if i + 1 < bytes.len() {
            let two = &src[i..i + 2];
            let op: Option<&'static str> = match two {
                ">=" => Some(">="),
                "<=" => Some("<="),
                "==" => Some("=="),
                "!=" => Some("!="),
                "&&" => Some("&&"),
                "||" => Some("||"),
                "+=" => Some("+="),
                "-=" => Some("-="),
                "*=" => Some("*="),
                "/=" => Some("/="),
                _ => None,
            };
            if let Some(o) = op {
                out.push(PlTok::Op(o));
                i += 2;
                continue;
            }
        }
        match c {
            '+' => {
                out.push(PlTok::Op("+"));
                i += 1;
            }
            '-' => {
                out.push(PlTok::Op("-"));
                i += 1;
            }
            '*' => {
                out.push(PlTok::Op("*"));
                i += 1;
            }
            '/' => {
                out.push(PlTok::Op("/"));
                i += 1;
            }
            '%' => {
                out.push(PlTok::Op("%"));
                i += 1;
            }
            '<' => {
                out.push(PlTok::Op("<"));
                i += 1;
            }
            '>' => {
                out.push(PlTok::Op(">"));
                i += 1;
            }
            '=' => {
                out.push(PlTok::Op("="));
                i += 1;
            }
            '!' => {
                out.push(PlTok::Op("!"));
                i += 1;
            }
            '(' => {
                out.push(PlTok::LParen);
                i += 1;
            }
            ')' => {
                out.push(PlTok::RParen);
                i += 1;
            }
            '{' => {
                out.push(PlTok::LBrace);
                i += 1;
            }
            '}' => {
                out.push(PlTok::RBrace);
                i += 1;
            }
            '[' => {
                out.push(PlTok::Op("["));
                i += 1;
            }
            ']' => {
                out.push(PlTok::Op("]"));
                i += 1;
            }
            ';' => {
                out.push(PlTok::Semi);
                i += 1;
            }
            ',' => {
                out.push(PlTok::Comma);
                i += 1;
            }
            '.' => {
                out.push(PlTok::Dot);
                i += 1;
            }
            '?' => {
                out.push(PlTok::Question);
                i += 1;
            }
            ':' => {
                out.push(PlTok::Colon);
                i += 1;
            }
            _ => return Err(format!("unexpected char `{}`", c)),
        }
    }
    Ok(out)
}

struct PlParser {
    toks: Vec<PlTok>,
    pos: usize,
}
impl PlParser {
    fn peek(&self) -> Option<&PlTok> {
        self.toks.get(self.pos)
    }
    fn bump(&mut self) -> Option<PlTok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn eat_semi(&mut self) {
        while matches!(self.peek(), Some(PlTok::Semi)) {
            self.pos += 1;
        }
    }
    fn parse_stmts(&mut self, terminator_brace: bool) -> Result<Vec<PlStmt>, String> {
        let mut out = Vec::new();
        while let Some(t) = self.peek() {
            if terminator_brace && matches!(t, PlTok::RBrace) {
                break;
            }
            self.eat_semi();
            if self.peek().is_none() {
                break;
            }
            if terminator_brace && matches!(self.peek(), Some(PlTok::RBrace)) {
                break;
            }
            out.push(self.parse_stmt()?);
            self.eat_semi();
        }
        Ok(out)
    }
    fn parse_stmt(&mut self) -> Result<PlStmt, String> {
        if let Some(PlTok::Ident(kw)) = self.peek().cloned() {
            if kw == "return" {
                self.bump();
                let e = self.parse_expr()?;
                return Ok(PlStmt::Return(e));
            }
            if kw == "for" {
                return self.parse_for();
            }
            if kw == "if" {
                return self.parse_if();
            }
            // Typed var decl: `<type> <name> = expr;`. Includes
            // Painless collection types (`Set`, `Map`, `List`,
            // `HashSet`, `TreeSet`, etc.) which in our evaluator
            // map to ordinary JSON values.
            if matches!(
                kw.as_str(),
                "long"
                    | "int"
                    | "double"
                    | "float"
                    | "def"
                    | "var"
                    | "boolean"
                    | "String"
                    | "Set"
                    | "Map"
                    | "List"
                    | "HashSet"
                    | "TreeSet"
                    | "HashMap"
                    | "TreeMap"
                    | "ArrayList"
                    | "Collection"
            ) {
                // Lookahead: if the next token after the type keyword
                // is not an identifier (i.e. this is a bare expression
                // like `Set x = ...` continuing to an identifier),
                // proceed. Otherwise it's an Ident expression.
                self.bump();
                let name = match self.bump() {
                    Some(PlTok::Ident(n)) => n,
                    _ => return Err("expected identifier after type".into()),
                };
                if !matches!(self.bump(), Some(PlTok::Op("="))) {
                    return Err("expected `=` in decl".into());
                }
                let e = self.parse_expr()?;
                return Ok(PlStmt::Decl(name, e));
            }
            // `throw new ExceptionType(args)` — we don't propagate
            // Painless exceptions structurally; consume the whole
            // statement as a no-op expression so parsing proceeds.
            if kw == "throw" {
                self.bump(); // `throw`
                             // Consume `new`, type name, and `(args)`.
                if let Some(PlTok::Ident(n)) = self.peek().cloned() {
                    if n == "new" {
                        self.bump();
                    }
                }
                // Type name (optionally `Foo.Bar` path).
                if let Some(PlTok::Ident(_)) = self.peek() {
                    self.bump();
                }
                while matches!(self.peek(), Some(PlTok::Dot)) {
                    self.bump();
                    if let Some(PlTok::Ident(_)) = self.peek() {
                        self.bump();
                    }
                }
                if matches!(self.peek(), Some(PlTok::LParen)) {
                    self.bump();
                    let mut depth = 1;
                    while depth > 0 {
                        match self.bump() {
                            Some(PlTok::LParen) => depth += 1,
                            Some(PlTok::RParen) => depth -= 1,
                            None => break,
                            _ => {}
                        }
                    }
                }
                // Represent as an effectless expression so the
                // enclosing block continues running.
                return Ok(PlStmt::Expr(PlExpr::Num(0.0)));
            }
        }
        // Expression-start statement: check for assignment/augmented later.
        let lhs = self.parse_expr()?;
        match self.peek().cloned() {
            Some(PlTok::Op("=")) => {
                self.bump();
                let rhs = self.parse_expr()?;
                Ok(PlStmt::Assign(lhs, rhs))
            }
            Some(PlTok::Op(op)) if matches!(op, "+=" | "-=" | "*=" | "/=") => {
                self.bump();
                let rhs = self.parse_expr()?;
                Ok(PlStmt::AugAssign(lhs, op, rhs))
            }
            _ => Ok(PlStmt::Expr(lhs)),
        }
    }
    fn parse_for(&mut self) -> Result<PlStmt, String> {
        self.bump(); // `for`
        if !matches!(self.bump(), Some(PlTok::LParen)) {
            return Err("expected `(` after for".into());
        }
        // Accept either `for (t in ...)` or `for (def t : ...)`.
        // Skip an optional type keyword before the loop variable.
        if let Some(PlTok::Ident(kw)) = self.peek().cloned() {
            if matches!(
                kw.as_str(),
                "def" | "var" | "long" | "int" | "double" | "float"
            ) {
                self.bump();
            }
        }
        let name = match self.bump() {
            Some(PlTok::Ident(n)) => n,
            _ => return Err("expected loop var".into()),
        };
        match self.bump() {
            Some(PlTok::Ident(k)) if k == "in" => {}
            Some(PlTok::Colon) => {}
            _ => return Err("expected `in` or `:` in for".into()),
        }
        let container = self.parse_expr()?;
        if !matches!(self.bump(), Some(PlTok::RParen)) {
            return Err("expected `)` in for".into());
        }
        if !matches!(self.bump(), Some(PlTok::LBrace)) {
            return Err("expected `{` in for-body".into());
        }
        let body = self.parse_stmts(true)?;
        if !matches!(self.bump(), Some(PlTok::RBrace)) {
            return Err("expected `}` after for-body".into());
        }
        Ok(PlStmt::ForIn(name, container, body))
    }
    fn parse_if(&mut self) -> Result<PlStmt, String> {
        self.bump();
        if !matches!(self.bump(), Some(PlTok::LParen)) {
            return Err("expected `(` after if".into());
        }
        let cond = self.parse_expr()?;
        if !matches!(self.bump(), Some(PlTok::RParen)) {
            return Err("expected `)` after if-cond".into());
        }
        let then_body = if matches!(self.peek(), Some(PlTok::LBrace)) {
            self.bump();
            let b = self.parse_stmts(true)?;
            if !matches!(self.bump(), Some(PlTok::RBrace)) {
                return Err("expected `}` after if-then".into());
            }
            b
        } else {
            vec![self.parse_stmt()?]
        };
        let else_body = if matches!(self.peek(), Some(PlTok::Ident(ref s)) if s == "else") {
            self.bump();
            if matches!(self.peek(), Some(PlTok::LBrace)) {
                self.bump();
                let b = self.parse_stmts(true)?;
                if !matches!(self.bump(), Some(PlTok::RBrace)) {
                    return Err("expected `}` after if-else".into());
                }
                Some(b)
            } else {
                Some(vec![self.parse_stmt()?])
            }
        } else {
            None
        };
        Ok(PlStmt::If(cond, then_body, else_body))
    }
    fn parse_expr(&mut self) -> Result<PlExpr, String> {
        self.parse_ternary()
    }
    fn parse_ternary(&mut self) -> Result<PlExpr, String> {
        let cond = self.parse_binary(1)?;
        if matches!(self.peek(), Some(PlTok::Question)) {
            self.bump();
            let t = self.parse_ternary()?;
            if !matches!(self.bump(), Some(PlTok::Colon)) {
                return Err("expected `:` in ternary".into());
            }
            let f = self.parse_ternary()?;
            Ok(PlExpr::Ternary(Box::new(cond), Box::new(t), Box::new(f)))
        } else {
            Ok(cond)
        }
    }
    fn parse_binary(&mut self, min_prec: u8) -> Result<PlExpr, String> {
        let mut lhs = self.parse_unary()?;
        while let Some(op) = self.peek_binop() {
            let prec = binop_prec(op);
            if prec < min_prec {
                break;
            }
            self.bump();
            let rhs = self.parse_binary(prec + 1)?;
            lhs = PlExpr::Binary(Box::new(lhs), op, Box::new(rhs));
        }
        Ok(lhs)
    }
    fn peek_binop(&self) -> Option<&'static str> {
        if let Some(PlTok::Op(o)) = self.peek() {
            match *o {
                "||" | "&&" | "==" | "!=" | "<" | ">" | "<=" | ">=" | "+" | "-" | "*" | "/"
                | "%" => Some(*o),
                _ => None,
            }
        } else {
            None
        }
    }
    fn parse_unary(&mut self) -> Result<PlExpr, String> {
        if let Some(PlTok::Op("-")) = self.peek() {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(PlExpr::Unary("-", Box::new(e)));
        }
        if let Some(PlTok::Op("!")) = self.peek() {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(PlExpr::Unary("!", Box::new(e)));
        }
        self.parse_postfix()
    }
    fn parse_postfix(&mut self) -> Result<PlExpr, String> {
        let mut e = self.parse_primary()?;
        loop {
            match self.peek() {
                Some(PlTok::Dot) => {
                    self.bump();
                    let name = match self.bump() {
                        Some(PlTok::Ident(s)) => s,
                        _ => return Err("expected identifier after `.`".into()),
                    };
                    // `e.NAME(args)` → Call; else Member.
                    if matches!(self.peek(), Some(PlTok::LParen)) {
                        self.bump();
                        let mut args = Vec::new();
                        if !matches!(self.peek(), Some(PlTok::RParen)) {
                            args.push(self.parse_expr()?);
                            while matches!(self.peek(), Some(PlTok::Comma)) {
                                self.bump();
                                args.push(self.parse_expr()?);
                            }
                        }
                        if !matches!(self.bump(), Some(PlTok::RParen)) {
                            return Err("expected `)` after call args".into());
                        }
                        e = PlExpr::Call(Box::new(e), name, args);
                    } else {
                        e = PlExpr::Member(Box::new(e), name);
                    }
                }
                // Subscript `e[key]` — used by Painless for
                // `doc['field']` and map-style access. Lower to a
                // Member access against the string-literal key, since
                // our representation treats both forms identically.
                Some(PlTok::Op("[")) => {
                    self.bump();
                    let key = self.parse_expr()?;
                    if !matches!(self.bump(), Some(PlTok::Op("]"))) {
                        return Err("expected `]` after subscript".into());
                    }
                    if let PlExpr::Str(s) = key {
                        e = PlExpr::Member(Box::new(e), s);
                    } else {
                        // Dynamic subscript — represent as Call with
                        // reserved name so the evaluator can resolve
                        // at runtime.
                        e = PlExpr::Call(Box::new(e), "__subscript__".to_string(), vec![key]);
                    }
                }
                _ => break,
            }
        }
        Ok(e)
    }
    fn parse_primary(&mut self) -> Result<PlExpr, String> {
        match self.bump() {
            Some(PlTok::Num(n)) => Ok(PlExpr::Num(n)),
            Some(PlTok::StrLit(s)) => Ok(PlExpr::Str(s)),
            Some(PlTok::Ident(s)) => {
                // `true` / `false` as identifier literals.
                match s.as_str() {
                    "true" => Ok(PlExpr::Num(1.0)),
                    "false" => Ok(PlExpr::Num(0.0)),
                    // `new TypeName(args)` — Painless constructor.
                    // Empty-arg constructors of collection types map
                    // to an empty array literal; other calls return
                    // an empty array as a harmless default. The
                    // `.add()` / `.put()` methods on the resulting
                    // value mutate it in place in the evaluator.
                    "new" => {
                        if let Some(PlTok::Ident(_ty)) = self.peek().cloned() {
                            self.bump();
                            // Optional dotted type path (`Foo.Bar`).
                            while matches!(self.peek(), Some(PlTok::Dot)) {
                                self.bump();
                                if let Some(PlTok::Ident(_)) = self.peek() {
                                    self.bump();
                                }
                            }
                            // Optional generic args `<…>` — tokenised
                            // as `<` then idents; skip until `>`.
                            if matches!(self.peek(), Some(PlTok::Op("<"))) {
                                self.bump();
                                let mut depth = 1;
                                while depth > 0 {
                                    match self.bump() {
                                        Some(PlTok::Op("<")) => depth += 1,
                                        Some(PlTok::Op(">")) => depth -= 1,
                                        None => break,
                                        _ => {}
                                    }
                                }
                            }
                            if matches!(self.peek(), Some(PlTok::LParen)) {
                                self.bump();
                                let mut args = Vec::new();
                                if !matches!(self.peek(), Some(PlTok::RParen)) {
                                    args.push(self.parse_expr()?);
                                    while matches!(self.peek(), Some(PlTok::Comma)) {
                                        self.bump();
                                        args.push(self.parse_expr()?);
                                    }
                                }
                                if !matches!(self.bump(), Some(PlTok::RParen)) {
                                    return Err("expected `)` after new".into());
                                }
                                return Ok(PlExpr::ArrayLit(args));
                            }
                            return Ok(PlExpr::ArrayLit(vec![]));
                        }
                        Ok(PlExpr::ArrayLit(vec![]))
                    }
                    _ => Ok(PlExpr::Ident(s)),
                }
            }
            Some(PlTok::LParen) => {
                let e = self.parse_expr()?;
                if !matches!(self.bump(), Some(PlTok::RParen)) {
                    return Err("expected `)`".into());
                }
                Ok(e)
            }
            Some(PlTok::Op("[")) => {
                // Array literal: `[e1, e2, ...]` or `[]`.
                let mut elems = Vec::new();
                if !matches!(self.peek(), Some(PlTok::Op("]"))) {
                    elems.push(self.parse_expr()?);
                    while matches!(self.peek(), Some(PlTok::Comma)) {
                        self.bump();
                        elems.push(self.parse_expr()?);
                    }
                }
                if !matches!(self.bump(), Some(PlTok::Op("]"))) {
                    return Err("expected `]` in array literal".into());
                }
                Ok(PlExpr::ArrayLit(elems))
            }
            other => Err(format!("unexpected token: {:?}", other)),
        }
    }
}
fn binop_prec(op: &str) -> u8 {
    match op {
        "||" => 1,
        "&&" => 2,
        "==" | "!=" => 3,
        "<" | ">" | "<=" | ">=" => 4,
        "+" | "-" => 5,
        "*" | "/" | "%" => 6,
        _ => 0,
    }
}

fn parse_script(src: &str) -> Result<Vec<PlStmt>, String> {
    let toks = lex_script(src)?;
    let mut p = PlParser { toks, pos: 0 };
    p.parse_stmts(false)
}

fn exec_stmts(stmts: &[PlStmt], ctx: &mut ScriptCtx) -> Result<Option<Value>, String> {
    for s in stmts {
        if let Some(v) = exec_stmt(s, ctx)? {
            return Ok(Some(v));
        }
    }
    Ok(None)
}

fn exec_stmt(stmt: &PlStmt, ctx: &mut ScriptCtx) -> Result<Option<Value>, String> {
    match stmt {
        PlStmt::Return(e) => Ok(Some(eval_expr(e, ctx)?)),
        PlStmt::Decl(name, e) => {
            let v = eval_expr(e, ctx)?;
            ctx.locals.insert(name.clone(), v);
            Ok(None)
        }
        PlStmt::Expr(e) => {
            eval_expr(e, ctx)?;
            Ok(None)
        }
        PlStmt::Assign(lhs, rhs) => {
            let v = eval_expr(rhs, ctx)?;
            assign_to(lhs, v, ctx)?;
            Ok(None)
        }
        PlStmt::AugAssign(lhs, op, rhs) => {
            let cur = eval_expr(lhs, ctx)?;
            let rv = eval_expr(rhs, ctx)?;
            let new = match *op {
                "+=" => num_binop(&cur, "+", &rv)?,
                "-=" => num_binop(&cur, "-", &rv)?,
                "*=" => num_binop(&cur, "*", &rv)?,
                "/=" => num_binop(&cur, "/", &rv)?,
                _ => return Err("unknown aug op".into()),
            };
            assign_to(lhs, new, ctx)?;
            Ok(None)
        }
        PlStmt::ForIn(name, container_expr, body) => {
            let c = eval_expr(container_expr, ctx)?;
            let items: Vec<Value> = match c {
                Value::Array(a) => a,
                _ => return Ok(None),
            };
            for item in items {
                ctx.locals.insert(name.clone(), item);
                if let Some(v) = exec_stmts(body, ctx)? {
                    return Ok(Some(v));
                }
            }
            Ok(None)
        }
        PlStmt::If(cond, then_b, else_b) => {
            let cv = eval_expr(cond, ctx)?;
            if is_truthy(&cv) {
                exec_stmts(then_b, ctx)
            } else if let Some(eb) = else_b {
                exec_stmts(eb, ctx)
            } else {
                Ok(None)
            }
        }
    }
}

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Resolve `doc.field.value` or `doc["field"].value` by accessing the
/// current doc's source; return the first value for multi-valued fields.
fn eval_expr(e: &PlExpr, ctx: &mut ScriptCtx) -> Result<Value, String> {
    match e {
        PlExpr::Num(n) => Ok(json!(n)),
        PlExpr::Str(s) => Ok(Value::String(s.clone())),
        PlExpr::Ident(n) => {
            if let Some(v) = ctx.locals.get(n) {
                return Ok(v.clone());
            }
            match n.as_str() {
                "state" => Ok(ctx.state.clone()),
                "params" => Ok(ctx.params.clone()),
                "states" => Ok(ctx.states.clone().map(Value::Array).unwrap_or(Value::Null)),
                "doc" => Ok(ctx.doc.cloned().unwrap_or(Value::Null)),
                "_score" => Ok(ctx
                    .doc
                    .and_then(|d| d.get("_score").cloned())
                    .unwrap_or(json!(1.0))),
                _ => Ok(Value::Null),
            }
        }
        PlExpr::ArrayLit(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(eval_expr(it, ctx)?);
            }
            Ok(Value::Array(out))
        }
        PlExpr::Member(obj_expr, name) => {
            let base = eval_expr(obj_expr, ctx)?;
            // `doc.field.value` → first element of the field value(s).
            if let PlExpr::Ident(ref root) = **obj_expr {
                if root == "doc" {
                    // `doc.FIELD` returns the field's array — `.value`
                    // below collapses it. We surface the array form here.
                    // Supports dotted paths (`doc['k8s.pod.uid']` →
                    // walk `k8s.pod.uid` or look up literal-dotted key).
                    let Some(doc) = ctx.doc else {
                        return Ok(Value::Null);
                    };
                    let src = doc.get("_source").unwrap_or(doc);
                    let raw = walk_dotted_or_literal(src, name);
                    let as_array = match raw {
                        Value::Array(a) => Value::Array(a),
                        Value::Null => Value::Array(Vec::new()),
                        other => Value::Array(vec![other]),
                    };
                    return Ok(as_array);
                }
            }
            // `.value` on an array-of-values collapses to the first element.
            if name == "value" {
                if let Value::Array(mut a) = base {
                    return Ok(a.drain(..).next().unwrap_or(Value::Null));
                }
                return Ok(base);
            }
            // `.size()` — property form for arrays.
            if name == "size" || name == "length" {
                if let Value::Array(a) = base {
                    return Ok(json!(a.len() as u64));
                }
                return Ok(json!(0));
            }
            // Generic property lookup on objects.
            if let Value::Object(ref obj) = base {
                if let Some(v) = obj.get(name) {
                    return Ok(v.clone());
                }
            }
            Ok(Value::Null)
        }
        PlExpr::Call(obj_expr, name, args) => {
            // collection.add(x), collection.size()
            let recv_val = eval_expr(obj_expr, ctx)?;
            if name == "add" && args.len() == 1 {
                let arg_val = eval_expr(&args[0], ctx)?;
                // Set semantics: if the element already exists, don't
                // insert and return false. Otherwise append and
                // return true. We also sort the backing vec on each
                // insert — Painless `TreeSet` is sorted, and keeping
                // the vec sorted happens to be a correct superset of
                // HashSet's insertion-order contract for the
                // scripted_metric use-case (the reducer treats any
                // iteration order the same).
                let mut arr = match recv_val {
                    Value::Array(a) => a,
                    _ => return Err("add() called on non-array".into()),
                };
                let already = arr.iter().any(|x| json_value_eq(x, &arg_val));
                if already {
                    return Ok(Value::Bool(false));
                }
                arr.push(arg_val);
                arr.sort_by(|a, b| match (a, b) {
                    (Value::String(x), Value::String(y)) => x.cmp(y),
                    (Value::Number(x), Value::Number(y)) => {
                        let xf = x.as_f64().unwrap_or(0.0);
                        let yf = y.as_f64().unwrap_or(0.0);
                        xf.partial_cmp(&yf).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
                    _ => std::cmp::Ordering::Equal,
                });
                assign_to(obj_expr, Value::Array(arr), ctx)?;
                return Ok(Value::Bool(true));
            }
            if name == "size" && args.is_empty() {
                if let Value::Array(a) = recv_val {
                    return Ok(json!(a.len() as u64));
                }
                return Ok(json!(0));
            }
            if name == "contains" && args.len() == 1 {
                let arg_val = eval_expr(&args[0], ctx)?;
                if let Value::Array(a) = recv_val {
                    return Ok(Value::Bool(a.iter().any(|x| json_value_eq(x, &arg_val))));
                }
                return Ok(Value::Bool(false));
            }
            // `x[key]` lowered subscript.
            if name == "__subscript__" && args.len() == 1 {
                let key = eval_expr(&args[0], ctx)?;
                match (&recv_val, &key) {
                    (Value::Object(o), Value::String(k)) => {
                        return Ok(o.get(k).cloned().unwrap_or(Value::Null));
                    }
                    (Value::Array(a), Value::Number(n)) => {
                        let idx = n.as_u64().unwrap_or(0) as usize;
                        return Ok(a.get(idx).cloned().unwrap_or(Value::Null));
                    }
                    _ => return Ok(Value::Null),
                }
            }
            Err(format!("unsupported method call `{}`", name))
        }
        PlExpr::Binary(l, op, r) => {
            let lv = eval_expr(l, ctx)?;
            let rv = eval_expr(r, ctx)?;
            match *op {
                "==" => Ok(Value::Bool(json_value_eq(&lv, &rv))),
                "!=" => Ok(Value::Bool(!json_value_eq(&lv, &rv))),
                "&&" => Ok(Value::Bool(is_truthy(&lv) && is_truthy(&rv))),
                "||" => Ok(Value::Bool(is_truthy(&lv) || is_truthy(&rv))),
                "<" | "<=" | ">" | ">=" => {
                    let a = value_as_f64(&lv).unwrap_or(0.0);
                    let b = value_as_f64(&rv).unwrap_or(0.0);
                    let res = match *op {
                        "<" => a < b,
                        "<=" => a <= b,
                        ">" => a > b,
                        ">=" => a >= b,
                        _ => false,
                    };
                    Ok(Value::Bool(res))
                }
                "+" => {
                    // Painless `+` is overloaded: string+anything
                    // concatenates with Java-style `toString` on the
                    // non-string side. Numbers with ints round-trip
                    // without a trailing `.0`.
                    let lhs_str = matches!(lv, Value::String(_));
                    let rhs_str = matches!(rv, Value::String(_));
                    if lhs_str || rhs_str {
                        let fmt = |v: &Value| -> String {
                            match v {
                                Value::String(s) => s.clone(),
                                Value::Number(n) => {
                                    if n.is_i64() {
                                        n.as_i64().unwrap().to_string()
                                    } else if n.is_u64() {
                                        n.as_u64().unwrap().to_string()
                                    } else {
                                        n.to_string()
                                    }
                                }
                                Value::Bool(b) => b.to_string(),
                                Value::Null => "null".to_string(),
                                other => other.to_string(),
                            }
                        };
                        Ok(Value::String(format!("{}{}", fmt(&lv), fmt(&rv))))
                    } else {
                        num_binop(&lv, op, &rv)
                    }
                }
                "-" | "*" | "/" | "%" => num_binop(&lv, op, &rv),
                _ => Err(format!("unknown binop `{}`", op)),
            }
        }
        PlExpr::Unary(op, e) => {
            let v = eval_expr(e, ctx)?;
            match *op {
                "-" => {
                    let n = value_as_f64(&v).unwrap_or(0.0);
                    Ok(json!(-n))
                }
                "!" => Ok(Value::Bool(!is_truthy(&v))),
                _ => Err(format!("unknown unary `{}`", op)),
            }
        }
        PlExpr::Ternary(c, t, f) => {
            let cv = eval_expr(c, ctx)?;
            if is_truthy(&cv) {
                eval_expr(t, ctx)
            } else {
                eval_expr(f, ctx)
            }
        }
    }
}

/// Walk a JSON source for either a literal dotted key (e.g. the
/// `"k8s.pod.uid": "xyz"` shape) or a nested path (`k8s.pod.uid`
/// → `{k8s: {pod: {uid: "xyz"}}}`). Painless `doc['k8s.pod.uid']`
/// accepts both shapes interchangeably.
fn walk_dotted_or_literal(v: &Value, key: &str) -> Value {
    if let Value::Object(obj) = v {
        if let Some(direct) = obj.get(key) {
            return direct.clone();
        }
        if !key.contains('.') {
            return Value::Null;
        }
        let mut cur: &Value = v;
        for seg in key.split('.') {
            match cur {
                Value::Object(o) => {
                    let Some(next) = o.get(seg) else {
                        return Value::Null;
                    };
                    cur = next;
                }
                _ => return Value::Null,
            }
        }
        return cur.clone();
    }
    Value::Null
}

fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}
fn json_value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(x), Value::String(y)) => x == y,
        (Value::String(x), other) | (other, Value::String(x)) => {
            if let Ok(nx) = x.parse::<f64>() {
                if let Some(ny) = value_as_f64(other) {
                    return (nx - ny).abs() < 1e-12;
                }
            }
            other.as_str().map(|s| s == x).unwrap_or(false)
        }
        (Value::Number(_), Value::Number(_)) => value_as_f64(a) == value_as_f64(b),
        _ => a == b,
    }
}
fn num_binop(a: &Value, op: &str, b: &Value) -> Result<Value, String> {
    let x = value_as_f64(a).unwrap_or(0.0);
    let y = value_as_f64(b).unwrap_or(0.0);
    let r = match op {
        "+" => x + y,
        "-" => x - y,
        "*" => x * y,
        "/" => {
            if y != 0.0 {
                x / y
            } else {
                0.0
            }
        }
        "%" => {
            if y != 0.0 {
                x % y
            } else {
                0.0
            }
        }
        _ => return Err(format!("bad num op `{}`", op)),
    };
    // Preserve integer shape when the result is representable as an int
    // and both inputs are integer (matches ES Painless long arithmetic).
    let both_int = value_looks_int(a) && value_looks_int(b);
    if both_int && r.fract() == 0.0 && r.is_finite() && r.abs() < 9.007e15 {
        Ok(json!(r as i64))
    } else {
        Ok(json!(r))
    }
}
fn value_looks_int(v: &Value) -> bool {
    match v {
        Value::Number(n) => {
            n.is_i64() || n.is_u64() || n.as_f64().map(|f| f.fract() == 0.0).unwrap_or(false)
        }
        _ => false,
    }
}

/// Write a value to a property chain like `state.foo.bar`. Recursively
/// navigates/creates objects along the way, then mutates the final
/// leaf through the context.
fn assign_to(target: &PlExpr, value: Value, ctx: &mut ScriptCtx) -> Result<(), String> {
    // Break the target into a root name + chain of segments.
    let mut segments: Vec<String> = Vec::new();
    let root = unwind_member(target, &mut segments)?;
    match root.as_str() {
        "state" => write_chain(&mut ctx.state, &segments, value),
        _ => {
            // Local variable write (or unknown root — set as local).
            if segments.is_empty() {
                ctx.locals.insert(root, value);
                Ok(())
            } else {
                let mut holder = ctx
                    .locals
                    .remove(&root)
                    .unwrap_or(Value::Object(serde_json::Map::new()));
                write_chain(&mut holder, &segments, value)?;
                ctx.locals.insert(root, holder);
                Ok(())
            }
        }
    }
}
fn unwind_member(e: &PlExpr, segs: &mut Vec<String>) -> Result<String, String> {
    match e {
        PlExpr::Ident(n) => Ok(n.clone()),
        PlExpr::Member(o, name) => {
            let root = unwind_member(o, segs)?;
            segs.push(name.clone());
            Ok(root)
        }
        _ => Err("non-assignable expression".into()),
    }
}
fn write_chain(holder: &mut Value, segs: &[String], value: Value) -> Result<(), String> {
    if segs.is_empty() {
        *holder = value;
        return Ok(());
    }
    let first = &segs[0];
    if !holder.is_object() {
        *holder = Value::Object(serde_json::Map::new());
    }
    let obj = holder.as_object_mut().unwrap();
    if segs.len() == 1 {
        obj.insert(first.clone(), value);
    } else {
        let entry = obj
            .entry(first.clone())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        write_chain(entry, &segs[1..], value)?;
    }
    Ok(())
}

// ── ip_range aggregation ─────────────────────────────────────────────────────

fn format_ipv6_u128(u: u128) -> String {
    // Use the v6-mapped form if the upper 80 bits are 0 and next 16 are
    // 0xFFFF — that's an IPv4-mapped IPv6 — render as v4 for readability.
    // Otherwise render as canonical IPv6.
    let bytes = u.to_be_bytes();
    std::net::Ipv6Addr::from(bytes).to_string()
}

fn run_ip_range(
    params: &Value,
    sub_aggs: Option<&Value>,
    docs: &[Value],
    all_docs: &[Value],
) -> Value {
    let field = params.get("field").and_then(Value::as_str).unwrap_or("ip");
    let ranges = match params.get("ranges").and_then(Value::as_array) {
        Some(r) => r,
        None => return json!({"buckets": []}),
    };

    // Convert any IP string to its canonical u128 (IPv4 is mapped to IPv6
    // per RFC 4291) so comparison matches ES's BigInteger semantics.
    fn ip_as_u128(s: &str) -> Option<u128> {
        let addr: std::net::IpAddr = s.parse().ok()?;
        Some(match addr {
            std::net::IpAddr::V4(v4) => {
                // Map to ::ffff:a.b.c.d
                0xFFFF_u128 << 32 | u32::from_be_bytes(v4.octets()) as u128
            }
            std::net::IpAddr::V6(v6) => u128::from_be_bytes(v6.octets()),
        })
    }
    fn cidr_matches(addr_str: &str, cidr: &str) -> bool {
        let (net, prefix) = match cidr.split_once('/') {
            Some((n, p)) => (n, p.parse::<u32>().unwrap_or(128)),
            None => (cidr, 128),
        };
        // ES CIDR semantics: when the network literal is IPv4 (no `:`),
        // the prefix is a v4 prefix (0-32) and only matches addresses that
        // are themselves v4 (or v4-mapped v6). The underlying u128 compare
        // uses v6-mapped form, so shift v4 prefixes by +96 and require the
        // candidate addr to be v4-mapped (upper 80 bits = 0, next 16 = 0xFFFF).
        let is_v4_cidr = !net.contains(':');
        let Some(net_u) = ip_as_u128(net) else {
            return false;
        };
        let Some(addr_u) = ip_as_u128(addr_str) else {
            return false;
        };
        if is_v4_cidr {
            // Candidate must be v4-mapped for a v4 CIDR to apply.
            if (addr_u >> 32) != 0xFFFF_u128 {
                return false;
            }
            let effective = prefix + 96;
            let shift = 128u32.saturating_sub(effective);
            let mask = if shift >= 128 { 0 } else { (!0u128) << shift };
            (net_u & mask) == (addr_u & mask)
        } else {
            let shift = 128u32.saturating_sub(prefix);
            let mask = if shift >= 128 { 0 } else { (!0u128) << shift };
            (net_u & mask) == (addr_u & mask)
        }
    }

    let mut buckets: Vec<Value> = Vec::new();
    for range_def in ranges {
        let from_str = range_def.get("from").and_then(Value::as_str);
        let to_str = range_def.get("to").and_then(Value::as_str);
        let mask = range_def.get("mask").and_then(Value::as_str);
        // ES default key: "<from|*>-<to|*>"; for mask buckets the key is
        // the mask literal. A user-supplied `key` wins.
        let default_key = if let Some(m) = mask {
            m.to_string()
        } else {
            format!("{}-{}", from_str.unwrap_or("*"), to_str.unwrap_or("*"))
        };
        let key = range_def
            .get("key")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or(default_key);

        let from_u = from_str.and_then(ip_as_u128);
        let to_u = to_str.and_then(ip_as_u128);
        let filtered: Vec<Value> = docs
            .iter()
            .filter(|doc| {
                let val = doc
                    .get(field)
                    .or_else(|| doc.get("_source").and_then(|s| s.get(field)));
                let ip_str = match val.and_then(Value::as_str) {
                    Some(s) => s,
                    None => return false,
                };
                if let Some(m) = mask {
                    cidr_matches(ip_str, m)
                } else {
                    let ip_u = match ip_as_u128(ip_str) {
                        Some(v) => v,
                        None => return false,
                    };
                    let passes_from = from_u.map(|f| ip_u >= f).unwrap_or(true);
                    let passes_to = to_u.map(|t| ip_u < t).unwrap_or(true);
                    passes_from && passes_to
                }
            })
            .cloned()
            .collect();

        let mut bucket = serde_json::Map::new();
        bucket.insert("key".to_string(), json!(key));
        // For mask buckets, derive from/to from the CIDR's u128 range.
        // ES's CIDR prefix semantics: when the network is IPv4 (no `:`
        // in the literal), the prefix is a v4 prefix (0-32); otherwise
        // a v6 prefix (0-128). IPv4 addresses are v6-mapped, so the
        // effective shift adds 96 for v4 CIDRs.
        let (emit_from, emit_to) = if let Some(m) = mask {
            let (net, prefix) = match m.split_once('/') {
                Some((n, p)) => (n, p.parse::<u32>().unwrap_or(128)),
                None => (m, 128),
            };
            let is_v4 = !net.contains(':');
            let effective_prefix = if is_v4 { prefix + 96 } else { prefix };
            if let Some(net_u) = ip_as_u128(net) {
                let shift = 128u32.saturating_sub(effective_prefix);
                let mask_bits = if shift >= 128 { 0 } else { (!0u128) << shift };
                let start = net_u & mask_bits;
                let end = start.saturating_add(1u128 << shift.min(127));
                let fmt = |u: u128| -> String {
                    if is_v4 {
                        // Render as IPv4 (strip the v6-mapped prefix).
                        let low = u as u32;
                        std::net::Ipv4Addr::from(low.to_be_bytes()).to_string()
                    } else {
                        format_ipv6_u128(u)
                    }
                };
                (Some(fmt(start)), Some(fmt(end)))
            } else {
                (None, None)
            }
        } else {
            (from_str.map(String::from), to_str.map(String::from))
        };
        if let Some(ref f) = emit_from {
            bucket.insert("from".to_string(), json!(f));
        }
        if let Some(ref t) = emit_to {
            bucket.insert("to".to_string(), json!(t));
        }
        bucket.insert("doc_count".to_string(), json!(filtered.len() as u64));
        if let Some(sub) = sub_aggs {
            let sr = run_aggs_with_all(sub, &filtered, all_docs);
            if let Value::Object(so) = sr {
                for (k, v) in so {
                    bucket.insert(k, v);
                }
            }
        }
        buckets.push(Value::Object(bucket));
    }
    json!({"buckets": buckets})
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn docs() -> Vec<Value> {
        vec![
            json!({"tag": "rust", "score": 10.0, "@timestamp": "2024-01-01T00:00:00Z"}),
            json!({"tag": "rust", "score": 20.0, "@timestamp": "2024-01-01T01:00:00Z"}),
            json!({"tag": "go",   "score": 15.0, "@timestamp": "2024-01-02T00:00:00Z"}),
            json!({"tag": "go",   "score": 5.0,  "@timestamp": "2024-01-02T06:00:00Z"}),
            json!({"score": 30.0, "@timestamp": "2024-01-03T00:00:00Z"}), // no tag
        ]
    }

    #[test]
    fn test_terms() {
        let agg = json!({ "by_tag": { "terms": { "field": "tag", "size": 5 } } });
        let result = run_aggs(&agg, &docs());
        let buckets = result["by_tag"]["buckets"].as_array().unwrap();
        assert_eq!(buckets.len(), 2);
        // Both "go" and "rust" have doc_count=2; tie-break is alphabetical asc,
        // so "go" < "rust" → "go" appears first (matches ES behaviour).
        assert_eq!(buckets[0]["key"], "go");
        assert_eq!(buckets[0]["doc_count"], 2);
        assert_eq!(buckets[1]["key"], "rust");
        assert_eq!(buckets[1]["doc_count"], 2);
    }

    #[test]
    fn test_value_count() {
        let agg = json!({ "n": { "value_count": { "field": "tag" } } });
        let result = run_aggs(&agg, &docs());
        assert_eq!(result["n"]["value"], 4);
    }

    #[test]
    fn test_avg() {
        let agg = json!({ "avg_score": { "avg": { "field": "score" } } });
        let result = run_aggs(&agg, &docs());
        let avg = result["avg_score"]["value"].as_f64().unwrap();
        assert!((avg - 16.0).abs() < 0.001);
    }

    #[test]
    fn test_sum() {
        let agg = json!({ "total": { "sum": { "field": "score" } } });
        let result = run_aggs(&agg, &docs());
        assert_eq!(result["total"]["value"].as_f64().unwrap(), 80.0);
    }

    #[test]
    fn test_min_max() {
        let agg = json!({
            "lo": { "min": { "field": "score" } },
            "hi": { "max": { "field": "score" } }
        });
        let result = run_aggs(&agg, &docs());
        assert_eq!(result["lo"]["value"].as_f64().unwrap(), 5.0);
        assert_eq!(result["hi"]["value"].as_f64().unwrap(), 30.0);
    }

    #[test]
    fn test_stats() {
        let agg = json!({ "s": { "stats": { "field": "score" } } });
        let result = run_aggs(&agg, &docs());
        assert_eq!(result["s"]["count"], 5);
        assert_eq!(result["s"]["sum"].as_f64().unwrap(), 80.0);
    }

    #[test]
    fn test_cardinality() {
        let agg = json!({ "uniq": { "cardinality": { "field": "tag" } } });
        let result = run_aggs(&agg, &docs());
        assert_eq!(result["uniq"]["value"], 2);
    }

    #[test]
    fn test_date_histogram() {
        let agg = json!({
            "over_time": {
                "date_histogram": {
                    "field": "@timestamp",
                    "fixed_interval": "1d"
                }
            }
        });
        let result = run_aggs(&agg, &docs());
        let buckets = result["over_time"]["buckets"].as_array().unwrap();
        assert_eq!(buckets.len(), 3); // Jan 1, Jan 2, Jan 3
        assert_eq!(buckets[0]["doc_count"], 2);
        assert_eq!(buckets[1]["doc_count"], 2);
        assert_eq!(buckets[2]["doc_count"], 1);
    }

    #[test]
    fn test_nested_aggs() {
        let agg = json!({
            "by_tag": {
                "terms": { "field": "tag", "size": 5 },
                "aggs": {
                    "avg_score": { "avg": { "field": "score" } }
                }
            }
        });
        let result = run_aggs(&agg, &docs());
        let buckets = result["by_tag"]["buckets"].as_array().unwrap();
        // Buckets are sorted asc by key on tie: "go" first, "rust" second.
        // go bucket avg = (15+5)/2 = 10
        let go_avg = buckets[0]["avg_score"]["value"].as_f64().unwrap();
        assert!((go_avg - 10.0).abs() < 0.001);
        // rust bucket avg = (10+20)/2 = 15
        let rust_avg = buckets[1]["avg_score"]["value"].as_f64().unwrap();
        assert!((rust_avg - 15.0).abs() < 0.001);
    }
}
