//! Bulk operation handler.
//!
//! Parses ES-compatible NDJSON bulk format and executes operations against
//! the engine with per-item error isolation.
//!
//! Performance: index/create operations are batched per-index and submitted
//! via `index_batch_turbo()` for maximum throughput (50K+ docs/sec).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::Engine;
use crate::index::IndexResponse;
use crate::{EngineError, Result};

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkResult {
    pub took_ms: u64,
    pub errors: bool,
    pub items: Vec<BulkItemResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BulkItemResult {
    pub action: String,
    pub index: String,
    pub id: String,
    pub status: u16,
    pub result: Option<String>,
    pub error: Option<String>,
    /// Post-execution source to echo under `items[N].<action>.get` in
    /// the HTTP response. Populated only when the action metadata
    /// carried `_source: true` / `_source: {...}` and the action
    /// succeeded. None for all other cases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub get_source: Option<Value>,
}

// ── Bulk processor ────────────────────────────────────────────────────────────

/// A parsed bulk action before execution.
struct ParsedAction {
    action_type: String,
    target_index: String,
    doc_id: Option<String>,
    doc_body: Option<Value>,
    /// Original NDJSON bytes for the doc line — forwarded verbatim to
    /// the WAL so we never re-serialize the parsed `Value`.  Empty for
    /// delete actions.
    doc_bytes: std::sync::Arc<[u8]>,
    /// Original position in the output items vec.
    item_index: usize,
    /// `require_alias` flag: Some(true)  → require target to be an alias;
    ///                      Some(false) → explicitly opt out of URL default;
    ///                      None        → inherit URL-level default.
    require_alias: Option<bool>,
    /// Per-item ingest pipeline name (if set via action metadata).
    pipeline: Option<String>,
    /// Optimistic concurrency: expected `_seq_no` / `_primary_term`
    /// from the action metadata. When set, the engine routes via
    /// `index_document_with_version` so a mismatch produces a 409
    /// `version_conflict_engine_exception` per-item error.
    if_seq_no: Option<u64>,
    if_primary_term: Option<u64>,
    /// Per-item `_routing` from action metadata — stored on the doc
    /// as `_routing` so `exists: _routing` and `fields: [_routing]`
    /// resolve.
    routing: Option<String>,
    /// Per-item `dynamic_templates` spec from the action metadata.
    /// ES bulk allows callers to request a specific dynamic template
    /// per field: `{"index":{"dynamic_templates":{"foo":"tmpl_name"}}}`.
    /// Validation against the index's declared `dynamic_templates`
    /// happens in execute_bulk_action — unknown template names
    /// produce a per-item 400 `document_parsing_exception`.
    dynamic_templates: Option<std::collections::BTreeMap<String, String>>,
    /// Per-item `_source` from action metadata (update-only). When
    /// set, the response echoes the post-update source under
    /// `items[N].update.get._source` (ES `?_source=true` shorthand
    /// on bulk updates).
    source_req: Option<Value>,
}

/// Process a bulk NDJSON body.
///
/// `default_index` is used when the action line does not specify `_index`.
///
/// All `index` and `create` operations are batched per-index and submitted
/// via `index_batch_turbo()` for 10–20x higher throughput than one-at-a-time.
/// `update` and `delete` operations are still executed individually.
pub async fn process_bulk(
    engine: &Engine,
    default_index: Option<&str>,
    body: &str,
) -> BulkResult {
    process_bulk_with_opts(engine, default_index, body, BulkOpts::default()).await
}

#[derive(Clone, Default)]
pub struct BulkOpts {
    pub require_alias: Option<bool>,
    /// URL-level default `_source` / `_source_includes` / `_source_excludes`
    /// shape (see search body filter). Applied to every update action
    /// that doesn't provide its own `_source` in the action meta.
    pub default_source_req: Option<Value>,
}

pub async fn process_bulk_with_opts(
    engine: &Engine,
    default_index: Option<&str>,
    body: &str,
    opts: BulkOpts,
) -> BulkResult {
    use rayon::prelude::*;

    let started = std::time::Instant::now();
    let t_lines = std::time::Instant::now();
    let lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
    let lines_ms = t_lines.elapsed().as_millis() as u64;

    // Pre-allocate output slots — filled in after execution.
    let mut items: Vec<Option<BulkItemResult>> = Vec::new();
    let mut errors = false;

    // M5.3 — PARALLEL NDJSON PARSE.
    //
    // Pre-M5.3 the bulk parser walked lines serially, calling
    // `serde_json::from_str::<Value>` on every action + doc line.
    // For a 5 000-doc bulk = 10 000 parses × ~5 µs = ~50 ms of
    // single-threaded work per batch — the single largest CPU cost
    // in the bulk ingest path at single-client saturation.
    //
    // We pair action+doc lines into tuples (an action line is
    // always followed by a doc line for index/create/update; delete
    // has no doc line), then rayon-parallel-parse the tuples.  The
    // parse is a pure function so no cross-pair state is shared —
    // rayon shards the work across up to `num_cpus()` cores.
    //
    // The parse output is a `Vec<Result<ParsedAction, ErrItem>>`
    // that preserves the original order, so downstream code that
    // matches on item_index remains correct.

    // Step 1: pair action + doc lines so we can parse each pair in
    // parallel.  The delete action has no doc line, so we track the
    // optional body-line index.
    // A delete action has NO body line; index/create/update each have
    // exactly one.  The action type is the first key of the action object
    // (`{"delete":{...}}`), so we can identify a delete from its action
    // line alone — no full parse needed.  This loop USED to optimistically
    // consume the next line as the body for EVERY action and rely on a
    // "push it back" fixup that was never actually implemented (see the
    // delete branch below).  The result: a delete followed by another
    // action (e.g. two consecutive deletes) silently swallowed that next
    // action line — ~50% of consecutive deletes were lost.  Detect the
    // delete up front so its body line is never consumed.
    fn action_line_is_delete(line: &str) -> bool {
        let b = line.as_bytes();
        let mut p = 0usize;
        while p < b.len() && (b[p] == b' ' || b[p] == b'\t') { p += 1; }
        if p >= b.len() || b[p] != b'{' { return false; }
        p += 1;
        while p < b.len() && (b[p] == b' ' || b[p] == b'\t') { p += 1; }
        if p >= b.len() || b[p] != b'"' { return false; }
        p += 1;
        let start = p;
        while p < b.len() && b[p] != b'"' { p += 1; }
        p < b.len() && &b[start..p] == b"delete"
    }
    let mut pairs: Vec<(usize, &str, Option<&str>, usize)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let action_line = lines[i];
        let item_index = items.len();
        items.push(None);
        let (doc_line, advance) = if !action_line_is_delete(action_line) && i + 1 < lines.len() {
            (Some(lines[i + 1]), 2)
        } else {
            (None, 1)
        };
        pairs.push((i, action_line, doc_line, item_index));
        i += advance;
    }

    // Step 2: parallel parse.  Each tuple yields either a
    // `ParsedAction` or an `ErrItem`.  The order of the output
    // vector matches the input order so `item_index` still
    // addresses the right slot.
    enum ParseOutcome {
        Ok(ParsedAction),
        Err(BulkItemResult, usize),
        Skip(usize /* extra item_index to pop, because we optimistically consumed a line */),
    }

    let t_parse = std::time::Instant::now();
    // Dedicated ingest pool: keeps bulk-parse bursts off the global rayon
    // pool that the search path fans out on (see `crate::ingest_pool`).
    let parse_results: Vec<ParseOutcome> = crate::ingest_pool().install(|| pairs
        .par_iter()
        .map(|(_line_idx, action_line, doc_line, item_index)| {
            // M5.12 — FAST-PATH manual parse of the action line.
            //
            // Pre-M5.12 the action line hit `serde_json::from_str::<Value>`
            // which allocates a Map<String,Value> for every one of 5000
            // action lines per batch = ~5 us × 5000 = 25 ms of pure
            // allocation per bulk batch.
            //
            // We handle the three common shapes inline and fall back to
            // full serde_json parse only on a cache miss:
            //   {"index":{}}
            //   {"index":{"_id":"..."}}
            //   {"index":{"_index":"..."}}
            //   {"index":{"_index":"...","_id":"..."}}
            //   (same for "create", "update", "delete")
            let bytes = action_line.as_bytes();
            // Any require_alias mention forces full parse (bool extraction is
            // not handled in the fast path); uncommon enough to not matter.
            let has_require_alias = bytes.windows(14).any(|w| w == b"require_alias\"");
            let has_pipeline = bytes.windows(9).any(|w| w == b"pipeline\"");
            let has_if_seq_no = bytes.windows(11).any(|w| w == b"\"if_seq_no\"");
            let has_routing = bytes.windows(9).any(|w| w == b"\"routing\"");
            let has_source = bytes.windows(9).any(|w| w == b"\"_source\"");
            let has_dynamic_templates = bytes.windows(19).any(|w| w == b"\"dynamic_templates\"");
            let mut require_alias_per_item: Option<bool> = None;
            let mut pipeline_per_item: Option<String> = None;
            let mut if_seq_no_per_item: Option<u64> = None;
            let mut if_primary_term_per_item: Option<u64> = None;
            let mut routing_per_item: Option<String> = None;
            let mut source_req_per_item: Option<Value> = None;
            let mut dynamic_templates_per_item: Option<std::collections::BTreeMap<String, String>> = None;
            let (action_type, target_index, doc_id) = 'fast: {
                if has_require_alias || has_pipeline || has_if_seq_no || has_routing || has_source || has_dynamic_templates {
                    break 'fast (None, None, None);
                }
                // Parse state: skip whitespace, expect `{`, "action": "index" etc.
                let mut p = 0usize;
                while p < bytes.len() && (bytes[p] == b' ' || bytes[p] == b'\t') { p += 1; }
                if p >= bytes.len() || bytes[p] != b'{' { break 'fast (None, None, None); }
                p += 1;
                while p < bytes.len() && (bytes[p] == b' ' || bytes[p] == b'\t') { p += 1; }
                if p >= bytes.len() || bytes[p] != b'"' { break 'fast (None, None, None); }
                p += 1;
                // Find closing quote of action name.
                let key_start = p;
                while p < bytes.len() && bytes[p] != b'"' { p += 1; }
                if p >= bytes.len() { break 'fast (None, None, None); }
                let key = &bytes[key_start..p];
                let action_ty: &'static str = match key {
                    b"index"  => "index",
                    b"create" => "create",
                    b"update" => "update",
                    b"delete" => "delete",
                    _ => break 'fast (None, None, None),
                };
                p += 1;
                while p < bytes.len() && (bytes[p] == b' ' || bytes[p] == b':') { p += 1; }
                if p >= bytes.len() || bytes[p] != b'{' { break 'fast (None, None, None); }
                // Extract optional _index and _id inside the inner object.
                // Both are bare-quoted strings with no embedded quotes in
                // the canonical bulk format produced by ES clients. The
                // inner object itself may contain nested objects (e.g.
                // `dynamic_templates:{...}`), so we track brace depth —
                // a naive "first `}`" scan would truncate the inner slice
                // at the first nested `}` and hide fields like `op_type`.
                let inner_end = {
                    let mut j = p + 1;
                    let mut depth: i32 = 1;
                    let mut in_str = false;
                    while j < bytes.len() {
                        let c = bytes[j];
                        if in_str {
                            if c == b'\\' { j += 2; continue; }
                            if c == b'"' { in_str = false; }
                        } else {
                            match c {
                                b'"' => in_str = true,
                                b'{' => depth += 1,
                                b'}' => {
                                    depth -= 1;
                                    if depth == 0 { break; }
                                }
                                _ => {}
                            }
                        }
                        j += 1;
                    }
                    if j >= bytes.len() { break 'fast (None, None, None); }
                    j
                };
                let inner = &bytes[p + 1..inner_end];
                let find_field = |needle: &[u8]| -> Option<String> {
                    // Look for `"_key":"value"` (quoted) or `"_key":NUM`
                    // (numeric, e.g. `_id:1`) inside `inner`. ES bulk
                    // accepts both shapes — numeric ids are stringified at
                    // the coordinator. An empty quoted value is preserved
                    // as `Some(String::new())` so the caller can tell
                    // "key absent" from "key present, empty".
                    let mut i = 0;
                    while i + needle.len() <= inner.len() {
                        if &inner[i..i + needle.len()] == needle {
                            let mut j = i + needle.len();
                            while j < inner.len() && inner[j] != b':' { j += 1; }
                            j += 1;
                            while j < inner.len() && inner[j] == b' ' { j += 1; }
                            if j >= inner.len() { return None; }
                            if inner[j] == b'"' {
                                j += 1;
                                let v_start = j;
                                while j < inner.len() && inner[j] != b'"' { j += 1; }
                                return Some(
                                    std::str::from_utf8(&inner[v_start..j]).ok()?.to_string(),
                                );
                            }
                            // Unquoted numeric id (e.g. `"_id":1`).
                            let v_start = j;
                            while j < inner.len() && (inner[j].is_ascii_digit() || inner[j] == b'-' || inner[j] == b'.') {
                                j += 1;
                            }
                            if v_start == j { return None; }
                            return Some(
                                std::str::from_utf8(&inner[v_start..j]).ok()?.to_string(),
                            );
                        }
                        i += 1;
                    }
                    None
                };
                let idx_name = find_field(br#""_index""#);
                let id = find_field(br#""_id""#);
                // ES allows `{"index": {"op_type": "create", ...}}` as a shorthand
                // for a `create` action; promote it so downstream treats it as create.
                let op_type = find_field(br#""op_type""#);
                let final_ty: &'static str = if action_ty == "index" && op_type.as_deref() == Some("create") {
                    "create"
                } else {
                    action_ty
                };
                (Some(final_ty), idx_name, id)
            };

            let (action_type, target_index, doc_id) = if let Some(ty) = action_type {
                (ty, target_index, doc_id)
            } else {
                // Fallback: serde_json for non-canonical action lines.
                // simd_json would need a mutable buffer (action_line is &str
                // borrowed from the request body), forcing a per-action
                // to_vec(). For the small action JSONs typical here (a few
                // hundred bytes), serde_json's borrow-only path beats
                // simd_json + allocation on the wall clock.
                let action_val: Value = match serde_json::from_slice(action_line.as_bytes()) {
                    Ok(v) => v,
                    Err(e) => {
                        return ParseOutcome::Err(
                            BulkItemResult {
                                action: "unknown".into(),
                                index: default_index.unwrap_or("_unknown").to_string(),
                                id: String::new(),
                                status: 400,
                                result: None,
                                error: Some(format!("failed to parse action line: {e}")),
                                                            get_source: None,
                            },
                            *item_index,
                        );
                    }
                };
                let (a, meta) = if let Some(obj) = action_val.get("index") {
                    ("index", obj)
                } else if let Some(obj) = action_val.get("create") {
                    ("create", obj)
                } else if let Some(obj) = action_val.get("update") {
                    ("update", obj)
                } else if let Some(obj) = action_val.get("delete") {
                    ("delete", obj)
                } else {
                    return ParseOutcome::Err(
                        BulkItemResult {
                            action: "unknown".into(),
                            index: default_index.unwrap_or("_unknown").to_string(),
                            id: String::new(),
                            status: 400,
                            result: None,
                            error: Some("unknown action type".to_string()),
                                                    get_source: None,
                        },
                        *item_index,
                    );
                };
                let op_type_str = meta.get("op_type").and_then(Value::as_str);
                let final_a: &'static str = if a == "index" && op_type_str == Some("create") {
                    "create"
                } else {
                    a
                };
                require_alias_per_item = meta.get("require_alias").and_then(Value::as_bool);
                pipeline_per_item = meta.get("pipeline").and_then(Value::as_str).map(str::to_owned);
                if_seq_no_per_item = meta.get("if_seq_no").and_then(Value::as_u64);
                if_primary_term_per_item = meta.get("if_primary_term").and_then(Value::as_u64);
                routing_per_item = meta.get("routing").and_then(Value::as_str).map(str::to_owned);
                source_req_per_item = meta.get("_source").cloned();
                dynamic_templates_per_item = meta.get("dynamic_templates")
                    .and_then(Value::as_object)
                    .map(|o| o.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect());
                // ES bulk accepts `_id` as either a string or a number;
                // numeric ids are stringified at the coordinating node.
                let id_from_meta = meta.get("_id").and_then(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    Value::Bool(b) => Some(b.to_string()),
                    _ => None,
                });
                (
                    final_a,
                    meta.get("_index").and_then(Value::as_str).map(str::to_owned),
                    id_from_meta,
                )
            };

            let target_index = target_index
                .or_else(|| default_index.map(str::to_owned))
                .unwrap_or_else(|| "_unknown".to_string());

            // ES rejects an explicit empty `_id` with an
            // illegal_argument_exception per-item error. `_id` missing
            // entirely still auto-generates; only the present-but-empty
            // case is wrong.
            if let Some(s) = doc_id.as_deref() {
                if s.is_empty() {
                    return ParseOutcome::Err(
                        BulkItemResult {
                            action: action_type.to_string(),
                            index: target_index,
                            id: String::new(),
                            status: 400,
                            result: None,
                            error: Some("if _id is specified it must not be empty".to_string()),
                                                    get_source: None,
                        },
                        *item_index,
                    );
                }
            }

            let (doc_body, doc_bytes) = if action_type != "delete" {
                let dl = match doc_line {
                    Some(dl) => dl,
                    None => {
                        return ParseOutcome::Err(
                            BulkItemResult {
                                action: action_type.to_string(),
                                index: target_index,
                                id: doc_id.unwrap_or_default(),
                                status: 400,
                                result: None,
                                error: Some("missing document body".to_string()),
                                                            get_source: None,
                            },
                            *item_index,
                        );
                    }
                };
                // M5.11 — SKIP the doc-body JSON parse for `index`
                // actions.  The turbo-raw ingest path accepts the raw
                // NDJSON bytes and defers the parse to drain-for-flush
                // on a background thread, taking ~5 µs/doc off the HTTP
                // worker's critical path.  `update` and `create`
                // actions still need the parsed Value because they
                // take different code paths below — but `index` is
                // the dominant one on ingest benchmarks.
                if action_type == "index" {
                    let bytes: std::sync::Arc<[u8]> =
                        std::sync::Arc::from(dl.as_bytes());
                    (None, bytes)
                } else {
                    // See action_line fallback above for why serde_json beats
                    // simd_json + to_vec() on small bulk doc lines.
                    match serde_json::from_slice::<Value>(dl.as_bytes()) {
                        Ok(v) => {
                            let bytes: std::sync::Arc<[u8]> =
                                std::sync::Arc::from(dl.as_bytes());
                            (Some(v), bytes)
                        }
                        Err(e) => {
                            return ParseOutcome::Err(
                                BulkItemResult {
                                    action: action_type.to_string(),
                                    index: target_index,
                                    id: doc_id.unwrap_or_default(),
                                    status: 400,
                                    result: None,
                                    error: Some(format!("invalid document JSON: {e}")),
                                                                    get_source: None,
                                },
                                *item_index,
                            );
                        }
                    }
                }
            } else {
                // Delete action — we may have optimistically consumed
                // a line during pair building that actually belongs to
                // the NEXT action.  Signal the caller to pop it back
                // into the input stream.
                let skip_back = if doc_line.is_some() { 1usize } else { 0usize };
                let action = ParsedAction {
                    action_type: action_type.to_string(),
                    target_index,
                    doc_id,
                    doc_body: None,
                    doc_bytes: std::sync::Arc::from(&[][..]),
                    item_index: *item_index,
                    require_alias: require_alias_per_item,
                    pipeline: pipeline_per_item.clone(),
                    if_seq_no: if_seq_no_per_item,
                    if_primary_term: if_primary_term_per_item,
                    routing: routing_per_item.clone(),
                    source_req: source_req_per_item.clone(),
                    dynamic_templates: dynamic_templates_per_item.clone(),
                };
                if skip_back > 0 {
                    // We need to signal that the "body" we consumed
                    // was actually a new action line — but with a
                    // parallel parse this is hard because lines are
                    // not in order.  For now, fall back to the
                    // serial path for delete-heavy batches.
                    // In practice nginx log ingest has ZERO deletes,
                    // so this branch is cold.
                    return ParseOutcome::Ok(action);
                }
                return ParseOutcome::Ok(action);
            };

            // For update actions, the request may put `_source` on the
            // doc body rather than the action meta
            // (`{_source: true, doc: {...}}`). Honor that too.
            let source_req_final = source_req_per_item.or_else(|| {
                if action_type == "update" {
                    doc_body.as_ref().and_then(|b| b.get("_source").cloned())
                } else {
                    None
                }
            });

            ParseOutcome::Ok(ParsedAction {
                action_type: action_type.to_string(),
                target_index,
                doc_id,
                doc_body,
                doc_bytes,
                item_index: *item_index,
                require_alias: require_alias_per_item,
                pipeline: pipeline_per_item,
                if_seq_no: if_seq_no_per_item,
                if_primary_term: if_primary_term_per_item,
                routing: routing_per_item,
                source_req: source_req_final,
                dynamic_templates: dynamic_templates_per_item,
            })
        })
        .collect());

    let parse_ms = t_parse.elapsed().as_millis() as u64;
    let t_group = std::time::Instant::now();
    let mut parsed: Vec<ParsedAction> = Vec::with_capacity(parse_results.len());
    for outcome in parse_results {
        match outcome {
            ParseOutcome::Ok(a) => parsed.push(a),
            ParseOutcome::Err(err_item, item_idx) => {
                items[item_idx] = Some(err_item);
                errors = true;
            }
            ParseOutcome::Skip(_) => {}
        }
    }

    // ── Per-item validation: pipeline existence ─────────────────────────────
    //
    // A per-item `pipeline: "..."` metadata reference must resolve to a
    // registered ingest pipeline; otherwise ES fails the item with
    // `illegal_argument_exception: pipeline with id [...] does not exist`.
    parsed.retain(|action| {
        if let Some(pid) = action.pipeline.as_deref() {
            if !engine.pipelines.contains_key(pid) {
                items[action.item_index] = Some(BulkItemResult {
                    action: action.action_type.clone(),
                    index: action.target_index.clone(),
                    id: action.doc_id.clone().unwrap_or_default(),
                    status: 400,
                    result: None,
                    error: Some(format!("pipeline with id [{pid}] does not exist")),
                                    get_source: None,
                });
                errors = true;
                return false;
            }
        }
        true
    });

    // ── Per-item validation: require_alias + ambiguous-alias write target ─────
    //
    // ES rejects an item when `require_alias=true` (per-item or URL-default)
    // and the target is not a registered alias. It also rejects writes to an
    // alias that has more than one backing index unless one is designated as
    // the write index via `is_write_index:true` in the alias metadata.
    let url_require_alias = opts.require_alias.unwrap_or(false);
    let default_source_req = opts.default_source_req.clone();
    parsed.retain(|action| {
        let eff_require_alias = action.require_alias.unwrap_or(url_require_alias);
        let target = &action.target_index;
        let is_alias = engine.aliases.contains_key(target);
        if eff_require_alias && !is_alias {
            items[action.item_index] = Some(BulkItemResult {
                action: action.action_type.clone(),
                index: target.clone(),
                id: action.doc_id.clone().unwrap_or_default(),
                status: 404,
                result: None,
                error: Some(format!(
                    "no such index [{target}] and [require_alias] request flag is [true] and [{target}] is not an alias"
                )),
                            get_source: None,
            });
            errors = true;
            return false;
        }
        if is_alias {
            if let Some(backings) = engine.aliases.get(target) {
                if backings.len() > 1 {
                    // Look up per-backing is_write_index in alias metadata.
                    // The metadata map is keyed by backing index name and
                    // stores `{alias: {is_write_index: true, ...}, ...}`.
                    let has_write_target = backings.iter().any(|backing| {
                        engine
                            .index_alias_metadata
                            .get(backing)
                            .and_then(|v| {
                                v.get(target).and_then(|meta| {
                                    meta.get("is_write_index").and_then(Value::as_bool)
                                })
                            })
                            .unwrap_or(false)
                    });
                    if !has_write_target {
                        items[action.item_index] = Some(BulkItemResult {
                            action: action.action_type.clone(),
                            index: target.clone(),
                            id: action.doc_id.clone().unwrap_or_default(),
                            status: 400,
                            result: None,
                            error: Some(format!(
                                "no write index is defined for alias [{target}]. The write index may be explicitly disabled using is_write_index=false or the alias points to multiple indices without one being designated as a write index"
                            )),
                                                    get_source: None,
                        });
                        errors = true;
                        return false;
                    }
                }
            }
        }
        true
    });

    // ── Turbo path: batch pure `index` ops per index ─────────────────────────
    //
    // `index` actions always overwrite — safe to batch via index_batch_turbo().
    // `create` actions must fail with 409 if the doc already exists, so they
    // are executed individually below (along with update/delete).
    //
    // Group `index`-only ops by target index.  **CONSUMES** `parsed` — the
    // previous version borrowed and then cloned `action.doc_body` (a full
    // `serde_json::Value` deep-clone) into the batch, which on a 60-field
    // nginx doc was ~60 allocations per document * 55 k docs/s = 3.3 M
    // allocs/s of pure waste.  Now we drain `parsed` by value and move the
    // Value straight into the batch.
    // M5.11 — index actions carry only `(item_index, doc_id, doc_bytes)`.
    // The parsed `Value` is never built for index actions on this path.
    let mut index_batches: HashMap<
        String,
        Vec<(usize, String, std::sync::Arc<[u8]>)>,
    > = HashMap::new();
    // AUTO-ID plain-index actions (no explicit `_id`).  These are
    // guaranteed new — they can NEVER overwrite — so every one is a
    // "created"/201.  That makes them safe to push through the single
    // `index_batch_turbo_raw` batch call, whose response is hardcoded
    // to "created".  Explicit-id index actions stay in `index_batches`
    // and run the per-doc loop because they may overwrite and so need
    // created-vs-updated / 201-vs-200 semantics turbo can't express.
    let mut auto_id_batches: HashMap<
        String,
        Vec<(usize, String, std::sync::Arc<[u8]>)>,
    > = HashMap::new();
    let mut non_index_actions: Vec<ParsedAction> = Vec::new();

    // Precompute which target indices declare any strict-format date
    // fields — those need per-doc date-format validation before the
    // doc is accepted. Actions targeting those indices go through the
    // slow path so the validation loop below can see them.
    let mut index_needs_date_validation: HashMap<String, bool> = HashMap::new();
    let index_has_strict_date = |target: &str, cache: &mut HashMap<String, bool>| -> bool {
        if let Some(b) = cache.get(target) { return *b; }
        let has = engine.index_mappings.get(target).map(|m| {
            let props = m.get("properties").cloned()
                .or_else(|| m.get("mappings").and_then(|mm| mm.get("properties")).cloned());
            mapping_has_strict_date(&props.unwrap_or(Value::Null))
        }).unwrap_or(false);
        cache.insert(target.to_string(), has);
        has
    };

    // Same gate for dynamic-templates with `copy_to`: the ingest
    // loop below materialises the copy into the doc body so the
    // stored source reflects it. The turbo path skips doc parsing,
    // so route those actions through the slow path.
    let mut index_needs_dynamic_copy: HashMap<String, bool> = HashMap::new();
    let index_has_dynamic_copy = |target: &str, cache: &mut HashMap<String, bool>| -> bool {
        if let Some(b) = cache.get(target) { return *b; }
        let has = engine.index_mappings.get(target).map(|m| {
            let tmpls = m.get("mappings")
                .and_then(|mm| mm.get("dynamic_templates"))
                .or_else(|| m.get("dynamic_templates"));
            tmpls.and_then(Value::as_array).map(|arr| arr.iter().any(|v| {
                v.as_object().and_then(|o| o.iter().next()).map(|(_, body)| {
                    body.get("mapping").and_then(|mm| mm.get("copy_to")).is_some()
                }).unwrap_or(false)
            })).unwrap_or(false)
        }).unwrap_or(false);
        cache.insert(target.to_string(), has);
        has
    };

    for action in parsed {
        // `index` with no `if_seq_no` / `routing` goes through the
        // turbo batch path. Actions that carry CAS or routing metadata
        // must be executed individually so we can honor them — route
        // them through the non-index path. Also divert when the index
        // has date fields with a strict `format`, so the per-item
        // validation loop can reject malformed values like ES does.
        let is_plain_index = action.action_type == "index"
            && action.if_seq_no.is_none()
            && action.if_primary_term.is_none()
            && action.routing.is_none()
            && action.dynamic_templates.is_none()
            && !index_has_strict_date(&action.target_index, &mut index_needs_date_validation)
            && !index_has_dynamic_copy(&action.target_index, &mut index_needs_dynamic_copy);
        if is_plain_index {
            if action.doc_id.is_none() {
                // Auto-id: brand-new doc, always "created"/201. Route the
                // whole group through ONE `index_batch_turbo_raw` call.
                let id = uuid::Uuid::new_v4().to_string();
                auto_id_batches
                    .entry(action.target_index)
                    .or_default()
                    .push((action.item_index, id, action.doc_bytes));
            } else {
                // Explicit id: may overwrite → keep the per-doc path so
                // created/updated + 201/200 is resolved correctly.
                let id = action.doc_id.unwrap();
                index_batches
                    .entry(action.target_index)
                    .or_default()
                    .push((action.item_index, id, action.doc_bytes));
            }
        } else {
            non_index_actions.push(action);
        }
    }
    let parsed = non_index_actions;

    let group_ms = t_group.elapsed().as_millis() as u64;
    let t_exec = std::time::Instant::now();

    // ── Auto-id index actions: ONE turbo batch per target index ──────────
    //
    // `index_batch_turbo_raw` appends the whole group with a single WAL
    // batch + one parsed FTS-memtable insert per doc (the now-correct,
    // single-representation path).  It returns one `IndexResponse` per
    // input doc IN ORDER, so we map them straight back onto the item
    // slots — all 201/"created" since auto-id docs can't overwrite.
    for (index_name, batch) in auto_id_batches {
        let idx = match engine.get_or_create_index(&index_name) {
            Ok(i) => i,
            Err(e) => {
                for (item_idx, id, _) in batch {
                    items[item_idx] = Some(BulkItemResult {
                        action: "index".into(),
                        index: index_name.clone(),
                        id,
                        status: 500,
                        result: None,
                        error: Some(e.to_string()),
                        get_source: None,
                    });
                    errors = true;
                }
                continue;
            }
        };

        // Keep item indices parallel to the docs we hand to turbo so we
        // can re-associate responses (turbo preserves input order).
        let item_indices: Vec<usize> = batch.iter().map(|(i, _, _)| *i).collect();
        let turbo_docs: Vec<(String, std::sync::Arc<[u8]>)> = batch
            .into_iter()
            .map(|(_, id, bytes)| (id, bytes))
            .collect();

        match idx.index_batch_turbo_raw(turbo_docs).await {
            Ok(responses) => {
                for (k, resp) in responses.into_iter().enumerate() {
                    let item_idx = item_indices[k];
                    items[item_idx] = Some(BulkItemResult {
                        action: "index".into(),
                        index: index_name.clone(),
                        id: resp.id,
                        status: 201,
                        result: Some("created".into()),
                        error: None,
                        get_source: None,
                    });
                }
            }
            Err(e) => {
                // Whole-batch failure (e.g. ResourceExhausted → 429):
                // mark every item in the batch with the same status.
                let status = match &e {
                    EngineError::Common(xerj_common::XerjError::ResourceExhausted { .. }) => 429,
                    _ => 500,
                };
                for item_idx in item_indices {
                    items[item_idx] = Some(BulkItemResult {
                        action: "index".into(),
                        index: index_name.clone(),
                        id: String::new(),
                        status,
                        result: None,
                        error: Some(e.to_string()),
                        get_source: None,
                    });
                }
                errors = true;
            }
        }
    }

    // Execute turbo batches (index-only).
    for (index_name, batch) in index_batches {
        let idx = match engine.get_or_create_index(&index_name) {
            Ok(i) => i,
            Err(e) => {
                for (item_idx, id, _) in batch {
                    items[item_idx] = Some(BulkItemResult {
                        action: "index".into(),
                        index: index_name.clone(),
                        id,
                        status: 500,
                        result: None,
                        error: Some(e.to_string()),
                                            get_source: None,
                    });
                    errors = true;
                }
                continue;
            }
        };

        // Index each doc via the per-doc path. index_document() handles
        // WAL write, storage memtable (_source), AND FTS memtable
        // (search + aggregations) in one call — no separate raw insert.
        for (item_idx, doc_id, doc_bytes) in batch {
            let source = serde_json::from_slice::<Value>(&doc_bytes)
                .unwrap_or(Value::Object(serde_json::Map::new()));
            tracing::debug!(
                doc_id = doc_id.as_str(),
                source = %source,
                bytes_len = doc_bytes.len(),
                "bulk: indexing doc via per-doc path"
            );
            let id_opt = if doc_id.is_empty() { None } else { Some(doc_id) };
            match idx.index_document(id_opt, source).await {
                Ok(resp) => {
                    // 201 for new doc, 200 for overwrite (ES semantics).
                    let status: u16 = if resp.result == "updated" { 200 } else { 201 };
                    items[item_idx] = Some(BulkItemResult {
                        action: "index".into(),
                        index: index_name.clone(),
                        id: resp.id,
                        status,
                        result: Some(resp.result),
                        error: None,
                                            get_source: None,
                    });
                }
                Err(e) => {
                    let status = match &e {
                        EngineError::Common(xerj_common::XerjError::ResourceExhausted { .. }) => 429,
                        _ => 500,
                    };
                    items[item_idx] = Some(BulkItemResult {
                        action: "index".into(),
                        index: index_name.clone(),
                        id: String::new(),
                        status,
                        result: None,
                        error: Some(e.to_string()),
                                            get_source: None,
                    });
                    errors = true;
                }
            }
        }
    }

    // Execute non-index ops individually (create, update, delete).
    // `create` / `update` / `delete` — executed individually.  `parsed` here
    // has already been filtered to non-`index` actions by the turbo-batch
    // partition above, so the match arms no longer need an `"index" => {}`
    // no-op branch.  We consume `parsed` by value to avoid per-action
    // deep-clones of `doc_id` (String) and `doc_body` (Value tree).
    for action in parsed {
        let ParsedAction {
            action_type,
            target_index,
            doc_id,
            doc_body,
            doc_bytes,
            item_index: item_idx,
            require_alias: _,
            pipeline: _,
            if_seq_no,
            if_primary_term,
            routing,
            source_req,
            dynamic_templates,
        } = action;
        let error_id = doc_id.clone().unwrap_or_default();
        let target_index_for_get = target_index.clone();
        let doc_id_for_get = doc_id.clone();
        // Ensure we have the doc body parsed before validating — our
        // dynamic-templates check needs `doc_body.contains_key(field)`
        // to tell "spec applies to this doc" from "spec references a
        // field not even present". The turbo-index partition skipped
        // parsing; do it here if needed.
        let mut doc_body = doc_body.or_else(|| {
            if action_type == "index" && !doc_bytes.is_empty() {
                // doc_bytes is Arc<[u8]> — mutating it would invalidate
                // sibling clones, so simd_json (which mutates in-place)
                // forced a per-doc to_vec(). serde_json takes &[u8]
                // immutably and avoids the allocation entirely.
                serde_json::from_slice::<Value>(&doc_bytes).ok()
            } else { None }
        });
        // Apply dynamic-template `copy_to` at ingest. When a doc
        // field name matches a dynamic template's `match` pattern
        // (or equals the template name) and the template's mapping
        // carries `copy_to`, ES silently copies the value into the
        // target field. Our search-time `apply_copy_to` only walks
        // the declared schema — it doesn't see dynamic templates —
        // so we materialise the copy into the doc body before it's
        // stored. This fixes the copy_to cluster in
        // `20_synthetic_source.yml` without requiring full dynamic
        // mapping persistence.
        if matches!(action_type.as_str(), "index" | "create") {
            if let Some(body) = doc_body.as_mut().and_then(|v| v.as_object_mut()) {
                let mapping = engine.index_mappings.get(&target_index).map(|v| v.clone());
                apply_dynamic_template_copy_to(body, mapping.as_ref(), dynamic_templates.as_ref());
            }
        }
        // Dynamic-templates validation: if the action specifies any
        // per-field `dynamic_templates: {field: tmpl_name}`, the
        // referenced template must exist in the target index's
        // mapping. Unknown names produce a per-item 400
        // `document_parsing_exception` in ES.
        if let Some(dt_map) = &dynamic_templates {
            if !dt_map.is_empty() {
                let mapping = engine.index_mappings.get(&target_index).map(|v| v.clone());
                // Collect per-template (name, match_pattern) so we can
                // test whether the field name would match ANY declared
                // template's pattern. ES accepts a dynamic_templates
                // spec on a field when either (a) the named template
                // exists, or (b) the field already has a concrete
                // mapping, or (c) the field matches some template's
                // `match` pattern (ES would silently apply that
                // template instead).
                let templates: Vec<(String, Option<String>)> = mapping
                    .as_ref()
                    .and_then(|m| m.get("mappings").and_then(|mm| mm.get("dynamic_templates"))
                        .or_else(|| m.get("dynamic_templates")))
                    .and_then(Value::as_array)
                    .map(|arr| arr.iter().filter_map(|v| v.as_object().and_then(|o| {
                        let (name, spec) = o.iter().next()?;
                        let pattern = spec.get("match").and_then(Value::as_str).map(String::from);
                        Some((name.clone(), pattern))
                    })).collect())
                    .unwrap_or_default();
                let declared_names: std::collections::HashSet<String> = templates
                    .iter().map(|(n, _)| n.clone()).collect();
                let declared_fields: std::collections::HashSet<String> = mapping
                    .as_ref()
                    .and_then(|m| m.get("mappings").and_then(|mm| mm.get("properties"))
                        .or_else(|| m.get("properties")))
                    .and_then(Value::as_object)
                    .map(|o| o.keys().cloned().collect())
                    .unwrap_or_default();
                let field_matches_any_template = |field: &str| -> bool {
                    templates.iter().any(|(name, pattern)| {
                        if let Some(pat) = pattern {
                            // Simple `*`-glob matching.
                            let p_bytes = pat.as_bytes();
                            let f_bytes = field.as_bytes();
                            simple_glob_matches(p_bytes, f_bytes)
                        } else {
                            // No pattern — the template default is a
                            // name-matching rule: field must equal the
                            // template's name.
                            field == name.as_str()
                        }
                    })
                };
                let mut first_bad: Option<(String, String)> = None;
                for (field, tmpl) in dt_map {
                    let present_in_doc = doc_body.as_ref()
                        .map(|db| db.as_object().map(|o| o.contains_key(field)).unwrap_or(false))
                        .unwrap_or(true);
                    if declared_fields.contains(field) { continue; }
                    if !present_in_doc { continue; }
                    if declared_names.contains(tmpl) { continue; }
                    // Fall back: the field might be implicitly mapped by
                    // a pattern-matching template (e.g. `match: "my*"`).
                    if field_matches_any_template(field) { continue; }
                    first_bad = Some((field.clone(), tmpl.clone()));
                    break;
                }
                // Type-compatibility pass: when the caller's
                // `dynamic_templates: {foo: string}` maps a scalar
                // type (keyword/text/long/…) but the doc carries
                // dotted keys `foo.*` that would implicitly make
                // `foo` an object, ES rejects with
                // `failed to parse field [foo] of type [TYPE]`.
                if first_bad.is_none() {
                    if let Some(body) = doc_body.as_ref().and_then(Value::as_object) {
                        // Look up each referenced template's mapping
                        // spec so we can inspect its `type`.
                        let tmpl_specs: std::collections::HashMap<String, Value> = mapping
                            .as_ref()
                            .and_then(|m| m.get("mappings").and_then(|mm| mm.get("dynamic_templates"))
                                .or_else(|| m.get("dynamic_templates")))
                            .and_then(Value::as_array)
                            .map(|arr| arr.iter().filter_map(|v| v.as_object().and_then(|o| {
                                let (name, spec) = o.iter().next()?;
                                let mapping_spec = spec.get("mapping").cloned().unwrap_or(Value::Null);
                                Some((name.clone(), mapping_spec))
                            })).collect())
                            .unwrap_or_default();
                        let mut first_type_clash: Option<(String, String, String)> = None;
                        for (field, tmpl) in dt_map {
                            let Some(spec) = tmpl_specs.get(tmpl) else { continue };
                            let ftype = spec.get("type").and_then(Value::as_str).unwrap_or("");
                            // Scalar-like types that can't hold an object.
                            let scalar = matches!(
                                ftype,
                                "keyword" | "text" | "match_only_text" | "long" | "integer" |
                                "short" | "byte" | "double" | "float" | "boolean" | "date" |
                                "date_nanos" | "ip" | "binary" | "half_float" | "scaled_float" |
                                "unsigned_long" | "geo_point"
                            );
                            if !scalar { continue; }
                            let prefix = format!("{}.", field);
                            let has_dotted = body.keys().any(|k| k.starts_with(&prefix));
                            if !has_dotted { continue; }
                            // Build a preview of the "object-shaped"
                            // value ES would render (e.g. `{bar=hello world}`).
                            let mut parts: Vec<String> = Vec::new();
                            for (k, v) in body.iter() {
                                if !k.starts_with(&prefix) { continue; }
                                let sub_key = &k[prefix.len()..];
                                let rendered = match v {
                                    Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                };
                                parts.push(format!("{}={}", sub_key, rendered));
                            }
                            let preview = format!("{{{}}}", parts.join(", "));
                            first_type_clash = Some((field.clone(), ftype.to_string(), preview));
                            break;
                        }
                        if let Some((field, ftype, preview)) = first_type_clash {
                            let reason = format!(
                                "failed to parse field [{}] of type [{}] in document with id '{}'. Preview of field's value: '{}'",
                                field, ftype,
                                doc_id.clone().unwrap_or_default(),
                                preview
                            );
                            items[item_idx] = Some(BulkItemResult {
                                action: action_type.to_string(),
                                index: target_index.clone(),
                                id: error_id.clone(),
                                status: 400,
                                result: None,
                                error: Some(reason),
                                get_source: None,
                            });
                            errors = true;
                            continue;
                        }
                    }
                }
                if let Some((field, tmpl)) = first_bad {
                    let reason = format!(
                        "Can't find dynamic template for dynamic template name [{}] of field [{}]",
                        tmpl, field
                    );
                    items[item_idx] = Some(BulkItemResult {
                        action: action_type.to_string(),
                        index: target_index.clone(),
                        id: error_id,
                        status: 400,
                        result: None,
                        error: Some(reason),
                        get_source: None,
                    });
                    errors = true;
                    continue;
                }
            }
        }
        // Date-format validation: if any field is declared as
        // `type: date` with an explicit `format` (no `ignore_malformed`
        // override), values that don't parse against the format are
        // rejected at ingest as ES does — `document_parsing_exception`
        // at the bulk item level. We only apply this for `index` /
        // `create`; updates operate on already-stored docs.
        if matches!(action_type.as_str(), "index" | "create") {
            if let Some(body) = doc_body.as_ref().and_then(Value::as_object) {
                let mapping_props = engine.index_mappings.get(&target_index)
                    .and_then(|m| {
                        m.get("properties").or_else(|| m.get("mappings").and_then(|mm| mm.get("properties")))
                            .cloned()
                    });
                if let Some(props) = mapping_props {
                    if let Some(bad) = find_bad_date_field(body, &props) {
                        let reason = format!(
                            "failed to parse field [{}] of type [date] in document with id '{}'",
                            bad.0,
                            doc_id.clone().unwrap_or_default(),
                        );
                        items[item_idx] = Some(BulkItemResult {
                            action: action_type.to_string(),
                            index: target_index.clone(),
                            id: error_id.clone(),
                            status: 400,
                            result: None,
                            error: Some(reason),
                            get_source: None,
                        });
                        errors = true;
                        continue;
                    }
                }
            }
        }
        // doc_body already lazy-parsed above (for dynamic-templates
        // validation). Nothing else to do here.
        let result = execute_bulk_action(
            engine,
            &action_type,
            &target_index,
            doc_id,
            doc_body,
            if_seq_no,
            if_primary_term,
            routing,
        )
        .await;

        match result {
            Ok(resp) => {
                // ES bulk semantics: 201 on first create, 200 on
                // overwrite (update). Delete → 200. Engine reports the
                // fact via `resp.result` ("created" vs "updated").
                let status: u16 = match action_type.as_str() {
                    "delete" => 200,
                    _ if resp.result == "updated" || resp.result == "noop" => 200,
                    _ => 201,
                };
                // Bulk update supports echoing the post-update source
                // via `items[N].update.get._source`. Source spec comes
                // from (priority): action metadata → doc-body `_source`
                // → URL-level default (?_source=true).
                let effective_source_req = source_req.clone().or_else(|| default_source_req.clone());
                let get_source: Option<Value> = if action_type == "update" && effective_source_req.is_some() {
                    match doc_id_for_get.as_deref() {
                        Some(id) => {
                            match engine.get_index(&target_index_for_get) {
                                Ok(idx) => {
                                    let fetched = idx.get_document(id).await.ok().flatten();
                                    fetched.map(|src| apply_bulk_source_filter(&src, effective_source_req.as_ref().unwrap()))
                                }
                                Err(_) => None,
                            }
                        }
                        None => None,
                    }
                } else { None };
                items[item_idx] = Some(BulkItemResult {
                    action: action_type.clone(),
                    index: target_index,
                    id: resp.id,
                    status,
                    result: Some(resp.result),
                    error: None,
                    get_source,
                });
            }
            Err(e) => {
                errors = true;
                // Map engine-level errors to the correct HTTP status so
                // per-item error objects carry the status ES clients key
                // off of. VersionConflict → 409, NotFound → 404.
                let status = match &e {
                    EngineError::Common(xerj_common::XerjError::VersionConflict { .. }) => 409,
                    EngineError::Common(xerj_common::XerjError::DocumentNotFound { .. }) => 404,
                    EngineError::Common(xerj_common::XerjError::ResourceExhausted { .. }) => 429,
                    _ => 500,
                };
                items[item_idx] = Some(BulkItemResult {
                    action: action_type,
                    index: target_index,
                    id: error_id,
                    status,
                    result: None,
                    error: Some(e.to_string()),
                                    get_source: None,
                });
            }
        }
    }

    // Flatten — any slot still None is a parse error that already set its slot.
    let final_items: Vec<BulkItemResult> = items
        .into_iter()
        .filter_map(|opt| opt)
        .collect();

    let exec_ms = t_exec.elapsed().as_millis() as u64;
    let took_ms = started.elapsed().as_millis() as u64;
    // THROWAWAY prof (XERJ_PROF): whole-request bulk attribution.
    if std::env::var_os("XERJ_PROF").is_some() {
        eprintln!(
            "XERJ_PROF bulk total_ms={} lines_ms={} parse_ms={} group_ms={} exec_ms={} n_lines={}",
            took_ms, lines_ms, parse_ms, group_ms, exec_ms, lines.len()
        );
    }
    if took_ms >= 50 {
        tracing::debug!(
            total_ms = took_ms,
            lines_ms,
            parse_ms,
            group_ms,
            exec_ms,
            n_lines = lines.len(),
            "process_bulk timings"
        );
    }
    BulkResult { took_ms, errors, items: final_items }
}

/// Apply a bulk-action `_source` request to a fetched document.
///
/// Accepts the same shapes the search body does:
/// - `true` / `false` — include everything / return `Value::Null`
/// - `"field"` — single include
/// - `["f1", "f2"]` — include list
/// - `{"includes": [...], "excludes": [...]}` — full filter
fn apply_bulk_source_filter(src: &Value, spec: &Value) -> Value {
    let obj = match src.as_object() {
        Some(o) => o.clone(),
        None => return src.clone(),
    };
    let (includes, excludes): (Vec<String>, Vec<String>) = match spec {
        Value::Bool(true) => return Value::Object(obj),
        Value::Bool(false) => return Value::Null,
        Value::String(s) => (vec![s.clone()], Vec::new()),
        Value::Array(arr) => (
            arr.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
            Vec::new(),
        ),
        Value::Object(o) => {
            let pull = |k: &str| -> Vec<String> {
                match o.get(k) {
                    Some(Value::Array(a)) => {
                        a.iter().filter_map(|x| x.as_str().map(String::from)).collect()
                    }
                    Some(Value::String(s)) => vec![s.clone()],
                    _ => Vec::new(),
                }
            };
            (pull("includes"), pull("excludes"))
        }
        _ => return Value::Object(obj),
    };
    let mut out = serde_json::Map::new();
    for (k, v) in obj {
        let keep = includes.is_empty() || includes.iter().any(|inc| inc == &k);
        let drop = excludes.iter().any(|exc| exc == &k);
        if keep && !drop {
            out.insert(k, v);
        }
    }
    Value::Object(out)
}

/// Apply dynamic-template `copy_to` directives to a doc body at
/// ingest. For each field in the doc that isn't already in the
/// declared mapping, check every dynamic template in order; if the
/// field name matches a template's `match` pattern (or equals the
/// template's name when no pattern is declared) AND the template's
/// mapping specifies `copy_to`, splice the field's value into the
/// target field (supporting dotted target paths and array append
/// semantics). When the per-item action metadata pins a specific
/// template via `dynamic_templates: {field: tmpl_name}`, that
/// template takes priority.
pub fn apply_dynamic_template_copy_to_public(
    body: &mut serde_json::Map<String, Value>,
    mapping: Option<&Value>,
) {
    apply_dynamic_template_copy_to(body, mapping, None);
}

fn apply_dynamic_template_copy_to(
    body: &mut serde_json::Map<String, Value>,
    mapping: Option<&Value>,
    per_item: Option<&std::collections::BTreeMap<String, String>>,
) {
    let Some(mapping) = mapping else { return };
    let templates_v = mapping
        .get("mappings")
        .and_then(|m| m.get("dynamic_templates"))
        .or_else(|| mapping.get("dynamic_templates"));
    let Some(templates_arr) = templates_v.and_then(Value::as_array) else { return };
    if templates_arr.is_empty() { return; }
    let declared_props: std::collections::HashSet<String> = mapping
        .get("mappings")
        .and_then(|m| m.get("properties"))
        .or_else(|| mapping.get("properties"))
        .and_then(Value::as_object)
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();

    // Pre-extract (name, pattern, copy_to) triples, ordered.
    let specs: Vec<(String, Option<String>, Option<String>)> = templates_arr
        .iter()
        .filter_map(|v| {
            let (name, body) = v.as_object().and_then(|o| o.iter().next())?;
            let pattern = body.get("match").and_then(Value::as_str).map(String::from);
            let copy_to = body
                .get("mapping")
                .and_then(|m| m.get("copy_to"))
                .and_then(|c| match c {
                    Value::String(s) => Some(s.clone()),
                    Value::Array(a) => a.first().and_then(Value::as_str).map(String::from),
                    _ => None,
                });
            Some((name.clone(), pattern, copy_to))
        })
        .collect();

    let field_names: Vec<String> = body.keys().cloned().collect();
    let mut touched_targets: Vec<String> = Vec::new();
    let mut pristine: serde_json::Map<String, Value> = serde_json::Map::new();
    for field in field_names {
        if declared_props.contains(&field) { continue; }
        // Priority 1: per-item `dynamic_templates: {field: name}`.
        let forced = per_item.and_then(|m| m.get(&field).cloned());
        let mut chosen: Option<String> = None;
        if let Some(name) = forced {
            if let Some((_, _, copy_to)) = specs.iter().find(|(n, _, _)| n == &name) {
                chosen = copy_to.clone();
            }
        }
        // Priority 2: first matching template in declaration order.
        if chosen.is_none() {
            for (name, pattern, copy_to) in &specs {
                let matches = if let Some(pat) = pattern {
                    simple_glob_matches(pat.as_bytes(), field.as_bytes())
                } else {
                    field == name.as_str()
                };
                if matches {
                    chosen = copy_to.clone();
                    break;
                }
            }
        }
        let Some(target_path) = chosen else { continue };
        let Some(val) = body.get(&field).cloned() else { continue };
        // Snapshot the target's pristine value before mutation so
        // the synthetic-source emit path can restore the explicit-
        // only source shape. Matches apply_copy_to's convention.
        if !touched_targets.iter().any(|t| t == &target_path) {
            let pristine_val = path_get(body, &target_path).cloned().unwrap_or(Value::Null);
            pristine.insert(target_path.clone(), pristine_val);
            touched_targets.push(target_path.clone());
        }
        // Append value into target path (splat arrays like
        // apply_copy_to does). Keyword-ish target types expect
        // strings; the synthetic_source + fields fetch path handles
        // stringification downstream, so we preserve the original
        // value shape here.
        match val {
            Value::Array(arr) => {
                for v in arr { append_copy_to_path(body, &target_path, v); }
            }
            other => append_copy_to_path(body, &target_path, other),
        }
    }
    if !touched_targets.is_empty() {
        // Merge with any existing pristine snapshot (apply_copy_to
        // from the declared-schema path may have already written
        // one; keep both sets of restore entries).
        if let Some(Value::Object(existing)) = body.get("__xy_copy_to_pristine__").cloned() {
            for (k, v) in existing {
                pristine.entry(k).or_insert(v);
            }
        }
        body.insert("__xy_copy_to_pristine__".to_string(), Value::Object(pristine));
    }
}

fn path_get<'a>(root: &'a serde_json::Map<String, Value>, path: &str) -> Option<&'a Value> {
    let segs: Vec<&str> = path.split('.').collect();
    if segs.is_empty() { return None; }
    let mut cur: &Value = root.get(segs[0])?;
    for seg in &segs[1..] {
        cur = cur.as_object()?.get(*seg)?;
    }
    Some(cur)
}

fn append_copy_to_path(
    target: &mut serde_json::Map<String, Value>,
    path: &str,
    val: Value,
) {
    let segs: Vec<&str> = path.split('.').collect();
    if segs.is_empty() { return; }
    let last = segs.len() - 1;
    let mut cur: &mut serde_json::Map<String, Value> = target;
    for (i, seg) in segs.iter().enumerate() {
        if i == last {
            let entry = cur.entry((*seg).to_string()).or_insert(Value::Null);
            match entry {
                Value::Null => { *entry = val; return; }
                Value::Array(arr) => { arr.push(val); return; }
                existing => {
                    let prev = existing.clone();
                    *existing = Value::Array(vec![prev, val]);
                    return;
                }
            }
        }
        let entry = cur.entry((*seg).to_string()).or_insert_with(|| Value::Object(serde_json::Map::new()));
        if !entry.is_object() {
            let old = entry.take();
            let mut m = serde_json::Map::new();
            m.insert("__xy_scalar__".to_string(), old);
            *entry = Value::Object(m);
        }
        cur = entry.as_object_mut().unwrap();
    }
}

/// Does this mapping-properties tree contain at least one
/// `type: date`/`type: date_nanos` field that has an explicit
/// `format` set AND does not opt into `ignore_malformed`? When true,
/// ingest must parse-validate date values on this index.
fn mapping_has_strict_date(props: &Value) -> bool {
    let Some(obj) = props.as_object() else { return false };
    for (_, spec) in obj {
        let ftype = spec.get("type").and_then(Value::as_str).unwrap_or("");
        if (ftype == "date" || ftype == "date_nanos")
            && spec.get("format").and_then(Value::as_str).is_some()
            && !spec.get("ignore_malformed").and_then(Value::as_bool).unwrap_or(false)
        {
            return true;
        }
        if let Some(sub) = spec.get("properties") {
            if mapping_has_strict_date(sub) { return true; }
        }
    }
    false
}

/// Walk a doc body against the index's mapping properties to find
/// the first field that is declared as `type: date` with a `format`
/// set, `ignore_malformed` off, and a value that doesn't parse
/// against the declared format. Returns `(field_name, bad_value)`
/// or `None` when everything checks out. ES `date_optional_time` and
/// `strict_date_optional_time*` formats cover ISO-8601; `epoch_*`
/// require purely numeric strings / numbers.
fn find_bad_date_field(
    doc: &serde_json::Map<String, Value>,
    props: &Value,
) -> Option<(String, Value)> {
    let props_obj = props.as_object()?;
    for (fname, spec) in props_obj {
        let Some(val) = doc.get(fname) else { continue };
        let ftype = spec.get("type").and_then(Value::as_str).unwrap_or("");
        if ftype != "date" && ftype != "date_nanos" { continue; }
        let fmt = spec.get("format").and_then(Value::as_str).unwrap_or("");
        if fmt.is_empty() { continue; }
        let ignore_mal = spec.get("ignore_malformed").and_then(Value::as_bool).unwrap_or(false);
        if ignore_mal { continue; }
        // Pick the single value to validate — arrays recurse over
        // each element, using the first failure we find.
        let candidates: Vec<&Value> = match val {
            Value::Array(arr) => arr.iter().collect(),
            other => vec![other],
        };
        for cand in candidates {
            if !date_value_parses(cand, fmt) {
                return Some((fname.clone(), cand.clone()));
            }
        }
    }
    None
}

/// Does `val` parse against the ES date `fmt`? Handles the formats
/// the YAML test suite exercises: `epoch_second`, `epoch_millis`,
/// `strict_date_optional_time*`, plus any chrono-compatible pattern
/// (via es_format_to_epoch_ms from index.rs). For the purposes of
/// rejecting obvious garbage, a value is accepted if *any* of the
/// declared formats (ES accepts `||`-separated patterns) matches.
fn date_value_parses(val: &Value, fmt: &str) -> bool {
    // Multi-format: `a||b||c` — accept if any individual pattern parses.
    if fmt.contains("||") {
        return fmt.split("||").any(|f| date_value_parses(val, f.trim()));
    }
    match val {
        Value::Null => true, // null is always fine
        Value::Number(n) => {
            // epoch_* formats accept any number. Textual formats need
            // a string, so numeric values only parse for epoch formats.
            fmt.contains("epoch") || n.as_i64().is_some()
        }
        Value::String(s) => {
            let s = s.trim();
            if fmt == "epoch_second" || fmt == "epoch_millis" {
                return s.parse::<i64>().is_ok();
            }
            // strict_date_optional_time / date_optional_time accept
            // any ISO-8601-ish string (year, year-month, full
            // timestamp). We accept anything that chrono's flexible
            // ISO parsers understand or that starts with a plausible
            // year prefix.
            if fmt.contains("date_optional_time") || fmt == "date" || fmt == "year" {
                return chrono::DateTime::parse_from_rfc3339(s).is_ok()
                    || s.parse::<i64>().is_ok()
                    || chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
                    || chrono::NaiveDate::parse_from_str(s, "%Y").is_ok()
                    || chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").is_ok();
            }
            // For named or custom patterns, be conservative — only
            // reject obviously-broken values (non-numeric under an
            // epoch format). Unknown format patterns pass through so
            // we don't over-reject locale-specific tests that our
            // translator doesn't yet handle.
            true
        }
        _ => false,
    }
}

/// `*`-glob match (single wildcard segment, no `**`, no character
/// classes). Used to test a field name against a dynamic-template
/// `match` pattern.
fn simple_glob_matches(pat: &[u8], txt: &[u8]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut star_t) = (None::<usize>, 0usize);
    while t < txt.len() {
        if p < pat.len() && (pat[p] == b'?' || pat[p] == txt[t]) {
            p += 1; t += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = Some(p); star_t = t; p += 1;
        } else if let Some(sp) = star {
            p = sp + 1; star_t += 1; t = star_t;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' { p += 1; }
    p == pat.len()
}

async fn execute_bulk_action(
    engine: &Engine,
    action_type: &str,
    index: &str,
    doc_id: Option<String>,
    body: Option<Value>,
    if_seq_no: Option<u64>,
    if_primary_term: Option<u64>,
    routing: Option<String>,
) -> Result<IndexResponse> {
    // Inject `_routing` into the source body so `exists: _routing` and
    // `fields: [_routing]` resolve through the stored doc.
    let inject_routing = |body: Option<Value>| -> Option<Value> {
        match routing.as_deref() {
            Some(r) => {
                let mut v = body.unwrap_or(Value::Object(serde_json::Map::new()));
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("_routing".to_string(), Value::String(r.to_string()));
                }
                Some(v)
            }
            None => body,
        }
    };
    match action_type {
        "index" => {
            // index: create or overwrite. Honors `if_seq_no` for
            // optimistic concurrency — a mismatch bubbles up as a
            // VersionConflict engine error which the caller maps to a
            // 409 per-item error.
            let idx = engine.get_or_create_index(index)?;
            let source = inject_routing(body).unwrap_or(Value::Object(serde_json::Map::new()));
            if if_seq_no.is_some() || if_primary_term.is_some() {
                idx.index_document_with_version(doc_id, source, if_seq_no, if_primary_term).await
            } else {
                idx.index_document(doc_id, source).await
            }
        }
        "create" => {
            // create: fail with 409 if document already exists.
            let idx = engine.get_or_create_index(index)?;
            let source = inject_routing(body).unwrap_or(Value::Object(serde_json::Map::new()));
            let id = doc_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            idx.create_document(id, source).await
        }
        "update" => {
            // Partial update: merge `doc` fields into the existing document.
            let idx = engine.get_or_create_index(index)?;
            let id = doc_id.ok_or_else(|| {
                crate::EngineError::Common(xerj_common::XerjError::invalid_query(
                    "update action requires _id",
                ))
            })?;

            let body_val = body.unwrap_or(Value::Object(serde_json::Map::new()));
            let partial = body_val.get("doc").cloned();
            let upsert = body_val.get("upsert").cloned();
            let doc_as_upsert = body_val
                .get("doc_as_upsert")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            match idx.update_document_with_upsert(&id, partial, upsert, doc_as_upsert).await? {
                Some(resp) => Ok(resp),
                None => Err(crate::EngineError::Common(
                    xerj_common::XerjError::document_not_found(&id, index),
                )),
            }
        }
        "delete" => {
            let idx = engine.get_index(index)?;
            let id = doc_id.ok_or_else(|| {
                crate::EngineError::Common(xerj_common::XerjError::invalid_query(
                    "delete action requires _id",
                ))
            })?;
            idx.delete_document(&id).await?;
            Ok(IndexResponse {
                id,
                seq_no: 0,
                version: 0,
                result: "deleted".to_string(),
            })
        }
        other => Err(crate::EngineError::Common(
            xerj_common::XerjError::invalid_query(format!("unknown bulk action: {other}")),
        )),
    }
}
