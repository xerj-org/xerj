//! `xerj_map` — the per-index field map from the autoindex catalog (#1055).
//!
//! The catalog index (`autoindex-catalog`) holds one `doc_kind: dataset`
//! document per dataset `xerj autoindex` indexed, and inside each of those,
//! `fields_json` carries the `FieldSpec` list the profiler inferred: name,
//! es_type, cardinality estimate, coverage, examples, and — since #1055 — the
//! sampled date/numeric min-max. The CLI renders that as `xerj autoindex
//! map`; an MCP agent had no way to reach it, which is why agents guessed
//! field names and ate unknown-field 400s on the DSL they then sent.
//!
//! This module is the READ side only and re-derives nothing: every fact in
//! the response comes from the catalog document verbatim. The catalog index
//! name is repeated here (a string constant in `xerj-autoindex/src/catalog.rs`)
//! because depending on that crate would drag the tree-sitter/SQLite/AWS
//! stack into this thin proxy; the name is a frozen on-disk contract there
//! (hashed into `index_identity`), which is what makes the copy safe — it
//! cannot be renamed in place.
//!
//! ## The per-index byte cap
//!
//! A response renders at most [`DEFAULT_MAX_BYTES`] of JSON per index entry.
//! The trim ladder, applied per entry until it fits and reported in the
//! entry's `trimmed` array (a stage that changes nothing is not reported):
//!
//! 1. per-field `examples` cut to the first example;
//! 2. `examples` dropped entirely;
//! 3. per-field `notes` dropped;
//! 4. fields dropped from the end of the list — it is sorted coverage
//!    descending, so the lowest-coverage fields go first — each counted in
//!    `fields_omitted`. A single field that alone exceeds the cap is dropped
//!    whole, which is what guarantees the ladder terminates.

use serde_json::{json, Map, Value};

use crate::{tool_text, Ctx};

/// The catalog index, written by `xerj autoindex`. See the module docs for
/// why this is a copy and not a dependency.
pub const CATALOG_INDEX: &str = "autoindex-catalog";

/// Default per-index byte budget for the rendered entry (#1055 acceptance:
/// "default response ≤ 4 KB per index").
pub const DEFAULT_MAX_BYTES: usize = 4096;
/// Floor for a caller-supplied budget: below this not even one field entry
/// plus the index header renders, and the ladder would drop everything.
pub const MIN_MAX_BYTES: usize = 1024;
/// Datasets fetched (and rendered) per call. Each catalog doc carries the
/// whole `fields_json`, so this bounds the transfer; datasets past it are
/// reported in `indexes_omitted`.
pub const MAX_INDEXES: usize = 100;
/// Examples kept per field. The profiler caps at 3 today; this cap is the
/// published contract if that ever grows.
pub const MAX_EXAMPLES: usize = 5;

/// Parsed, validated `xerj_map` arguments.
#[derive(Debug, Clone)]
pub(crate) struct MapArgs {
    /// Index name or `*`-glob; `None` = every catalogued index.
    pub index: Option<String>,
    /// Per-index byte budget for the rendered entry.
    pub max_bytes: usize,
}

/// Parse and validate the tool arguments. Present-but-mistyped is an error,
/// never a silent drop (the `opt_typed` policy the rest of this crate holds).
pub(crate) fn parse_args(args: &Value) -> Result<MapArgs, String> {
    let index = match args.get("index") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        Some(_) => return Err("`index` must be a string (index name or glob)".into()),
    };
    let max_bytes = match args.get("max_bytes") {
        None | Some(Value::Null) => DEFAULT_MAX_BYTES,
        Some(v) => v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n >= MIN_MAX_BYTES)
            .ok_or_else(|| {
                format!(
                    "`max_bytes` must be an integer >= {MIN_MAX_BYTES} (default \
                     {DEFAULT_MAX_BYTES})"
                )
            })?,
    };
    Ok(MapArgs { index, max_bytes })
}

/// The `_search` body for the catalog read. Filter exact names with `term`
/// and globs with `wildcard` before the 100-result cap. Filtering a glob only
/// after the fetch could hide a matching dataset behind 100 larger ones.
pub(crate) fn catalog_body(args: &MapArgs) -> Value {
    let mut must = vec![json!({ "term": { "doc_kind": "dataset" } })];
    if let Some(name) = args.index.as_deref() {
        if name.contains('*') {
            must.push(json!({ "wildcard": { "index_name": name } }));
        } else {
            must.push(json!({ "term": { "index_name": name } }));
        }
    }
    json!({
        "query": { "bool": { "must": must } },
        "size": MAX_INDEXES,
        "sort": [{ "record_count": "desc" }],
        // The dataset doc also carries sample_queries_json (by far its
        // largest field); project it out rather than transfer it to drop it.
        "_source": { "includes": [
            "index_name", "slug", "record_count", "time_field", "time_min",
            "time_max", "semantic_field", "fields_json", "notes",
        ] },
    })
}

/// `*`-glob match — the only wildcard the tool documents. The first and last
/// segments are anchored at the ends of `s`; inner segments may sit
/// anywhere, in order. (So `ax-*` matches `ax-logs` but not `tax-logs`.)
fn glob_match(pattern: &str, s: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == s,
        Some((head, tail)) => {
            let Some(rest) = s.strip_prefix(head) else {
                return false;
            };
            if tail.is_empty() {
                return true; // trailing star: everything left matches
            }
            (0..=rest.len())
                .filter(|i| rest.is_char_boundary(*i))
                .any(|i| glob_match(tail, &rest[i..]))
        }
    }
}

/// Normalise one raw `FieldSpec` JSON object into the field entry the tool
/// returns: the FieldSpec keys an agent acts on, examples capped, absent
/// keys omitted. Unknown keys in the catalog's `fields_json` pass through —
/// this is a projection, not a re-derivation.
fn project_field(f: &Value) -> Value {
    let Some(obj) = f.as_object() else {
        return f.clone();
    };
    let mut out = Map::new();
    for (k, v) in obj {
        match k.as_str() {
            // `null_ratio` is 1−coverage and `avg_len` drove the semantic
            // election; neither changes a query an agent writes, and
            // `date_evidence` is restated more compactly by `date_enc`.
            "null_ratio" | "avg_len" | "date_evidence" => {}
            "examples" => {
                let capped: Vec<Value> = v
                    .as_array()
                    .map(|a| a.iter().take(MAX_EXAMPLES).cloned().collect())
                    .unwrap_or_default();
                if !capped.is_empty() {
                    out.insert(k.clone(), Value::Array(capped));
                }
            }
            // Only the overflow is information: an absent key already means
            // "counted, not capped", so `false` is noise on every field.
            "cardinality_overflow" => {
                if v.as_bool().unwrap_or(false) {
                    out.insert(k.clone(), v.clone());
                }
            }
            "coverage" => {
                // 3 decimals: byte-lean, and beyond the sample's precision
                // anyway.
                if let Some(c) = v.as_f64() {
                    out.insert(k.clone(), json!((c * 1000.0).round() / 1000.0));
                }
            }
            _ => {
                if !(v.is_null() || v.as_array().is_some_and(Vec::is_empty)) {
                    out.insert(k.clone(), v.clone());
                }
            }
        }
    }
    Value::Object(out)
}

/// Build the full index entry from the dataset doc and the (possibly
/// trimmed) field list.
fn assemble_entry(
    doc: &Value,
    kept: &[Value],
    field_count: usize,
    trimmed: &[&'static str],
) -> Value {
    let mut entry = Map::new();
    entry.insert(
        "index".into(),
        doc.get("index_name").cloned().unwrap_or(Value::Null),
    );
    if let Some(n) = doc.get("record_count").filter(|v| v.is_number()) {
        entry.insert("records".into(), n.clone());
    }
    for key in [
        "time_field",
        "time_min",
        "time_max",
        "semantic_field",
        "notes",
    ] {
        if let Some(v) = doc
            .get(key)
            .filter(|v| !v.is_null() && !v.as_array().is_some_and(Vec::is_empty))
        {
            entry.insert(key.into(), v.clone());
        }
    }
    entry.insert("field_count".into(), json!(field_count));
    entry.insert("fields".into(), Value::Array(kept.to_vec()));
    let omitted = field_count.saturating_sub(kept.len());
    if omitted > 0 {
        entry.insert("fields_omitted".into(), json!(omitted));
    }
    if !trimmed.is_empty() {
        entry.insert(
            "trimmed".into(),
            json!(trimmed.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
        );
    }
    Value::Object(entry)
}

/// Render one index entry under the byte budget. `fields` arrive already
/// projected and sorted (coverage desc, name asc) — the order the ladder's
/// "drop from the end" relies on.
fn render_entry(doc: &Value, mut kept: Vec<Value>, max_bytes: usize) -> Value {
    let field_count = kept.len();
    let mut trimmed: Vec<&'static str> = Vec::new();
    let fits = |entry: &Value| {
        serde_json::to_string(entry)
            .map(|s| s.len())
            .unwrap_or(usize::MAX)
            <= max_bytes
    };
    let mut entry = assemble_entry(doc, &kept, field_count, &trimmed);
    if fits(&entry) {
        return entry;
    }

    // Stage 1: examples cut to the first. Recorded only when it actually
    // cut something — an honest accounting, not a ritual label.
    let cut = kept.iter_mut().filter_map(|f| f.as_object_mut()).any(|o| {
        o.get("examples")
            .and_then(Value::as_array)
            .is_some_and(|ex| ex.len() > 1)
    });
    if cut {
        for f in &mut kept {
            if let Some(ex) = f.get("examples").and_then(Value::as_array) {
                if ex.len() > 1 {
                    let first = ex[..1].to_vec();
                    if let Some(o) = f.as_object_mut() {
                        o.insert("examples".into(), Value::Array(first));
                    }
                }
            }
        }
        trimmed.push("examples-cut-to-1");
        entry = assemble_entry(doc, &kept, field_count, &trimmed);
        if fits(&entry) {
            return entry;
        }
    }

    // Stage 2: examples dropped entirely.
    for f in &mut kept {
        if let Some(o) = f.as_object_mut() {
            o.remove("examples");
        }
    }
    trimmed.push("examples-dropped");
    entry = assemble_entry(doc, &kept, field_count, &trimmed);
    if fits(&entry) {
        return entry;
    }

    // Stage 3: per-field notes dropped.
    for f in &mut kept {
        if let Some(o) = f.as_object_mut() {
            o.remove("notes");
        }
    }
    trimmed.push("notes-dropped");
    entry = assemble_entry(doc, &kept, field_count, &trimmed);
    if fits(&entry) {
        return entry;
    }

    // Stage 4: drop fields from the end (lowest coverage first). A field
    // that alone exceeds the budget goes too — `kept` can reach empty, and
    // an entry with zero fields (but a truthful `field_count`) still
    // renders, which is what terminates this loop.
    trimmed.push("fields-dropped");
    while !kept.is_empty() {
        kept.pop();
        entry = assemble_entry(doc, &kept, field_count, &trimmed);
        if fits(&entry) {
            return entry;
        }
    }
    entry
}

/// Render the whole tool response from a catalog `_search` response.
/// Pure — every unit test below drives this with a hand-built catalog
/// response, no node required.
pub(crate) fn render_response(resp: &Value, args: &MapArgs) -> String {
    let hits = resp
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let total: u64 = resp
        .pointer("/hits/total/value")
        .and_then(Value::as_u64)
        .unwrap_or(hits.len() as u64);

    // Keep the local glob check so only the tool's documented `*` syntax
    // reaches the response even if the engine supports more wildcard syntax.
    let docs: Vec<Value> = hits
        .iter()
        .filter_map(|h| h.get("_source").cloned())
        .filter(|src| {
            args.index.as_deref().is_none_or(|p| {
                !p.contains('*')
                    || glob_match(
                        p,
                        src.get("index_name").and_then(Value::as_str).unwrap_or(""),
                    )
            })
        })
        .collect();

    let mut indexes: Vec<Value> = docs
        .iter()
        .map(|src| {
            let mut fields: Vec<Value> = src
                .get("fields_json")
                .and_then(Value::as_str)
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
                .iter()
                .map(project_field)
                .collect();
            // Coverage desc, then name — the CLI map's order, and the order
            // the trim ladder's "drop from the end" relies on.
            fn cov_of(f: &Value) -> f64 {
                f.get("coverage").and_then(Value::as_f64).unwrap_or(0.0)
            }
            fn name_of(f: &Value) -> &str {
                f.get("name").and_then(Value::as_str).unwrap_or("")
            }
            fields.sort_by(|a, b| {
                cov_of(b)
                    .partial_cmp(&cov_of(a))
                    .unwrap()
                    .then_with(|| name_of(a).cmp(name_of(b)))
            });
            render_entry(src, fields, args.max_bytes)
        })
        .collect();
    // The engine already sorted by record_count desc; make the order total
    // so tied counts (and cap-truncated result sets) render deterministically.
    indexes.sort_by(|a, b| {
        let rc = |v: &Value| v.get("records").and_then(Value::as_u64).unwrap_or(0);
        rc(b).cmp(&rc(a)).then_with(|| {
            a.get("index")
                .and_then(Value::as_str)
                .unwrap_or("")
                .cmp(b.get("index").and_then(Value::as_str).unwrap_or(""))
        })
    });

    let mut out = Map::new();
    out.insert("matched".into(), json!(indexes.len()));
    if let Some(p) = &args.index {
        out.insert("pattern".into(), json!(p));
    }
    out.insert("indexes".into(), Value::Array(indexes));
    let omitted = total.saturating_sub(hits.len() as u64);
    if omitted > 0 {
        out.insert("indexes_omitted".into(), json!(omitted));
    }
    if docs.is_empty() {
        out.insert(
            "hint".into(),
            json!(format!(
                "no datasets in {CATALOG_INDEX} match — the catalog is written by `xerj \
                 autoindex <folder>`; never-autoindexed indexes are not listed (ask the \
                 node's _cat/indices for those)"
            )),
        );
    }
    Value::Object(out).to_string()
}

/// `xerj_map` dispatch: read the catalog, render the map. A missing catalog
/// (the node never ran autoindex) is an error with the fix named — the
/// response an agent can act on — while an empty-but-present catalog is a
/// normal empty result, not an error.
pub(crate) async fn run(ctx: &Ctx, args: &Value) -> Value {
    let parsed = match parse_args(args) {
        Ok(a) => a,
        Err(msg) => return tool_text(format!("xerj_map: {msg}"), true),
    };
    let body = catalog_body(&parsed);
    let url = format!("{}/{CATALOG_INDEX}/_search", ctx.base_url);
    let mut req = ctx.client.post(&url).json(&body);
    if let Some(auth) = &ctx.auth {
        req = req.header("Authorization", auth);
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status.is_success() {
                match serde_json::from_str::<Value>(&text) {
                    Ok(v) => tool_text(render_response(&v, &parsed), false),
                    Err(e) => tool_text(
                        format!("xerj_map: catalog reply was not JSON ({e}): {text}"),
                        true,
                    ),
                }
            } else if status.as_u16() == 404 {
                tool_text(
                    format!(
                        "xerj_map: no autoindex catalog on this node (index \
                         `{CATALOG_INDEX}` not found, HTTP 404) — run `xerj autoindex \
                         <folder>` first. Engine said: {text}"
                    ),
                    true,
                )
            } else {
                tool_text(
                    format!("XERJ returned HTTP {status} from {CATALOG_INDEX}: {text}"),
                    true,
                )
            }
        }
        Err(e) => tool_text(format!("request to {url} failed: {e}"), true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dataset_doc(index: &str, records: u64, fields_json: &str) -> Value {
        json!({
            "_index": CATALOG_INDEX,
            "_id": format!("ds:ax:{index}"),
            "_source": {
                "doc_kind": "dataset",
                "slug": index,
                "index_name": index,
                "record_count": records,
                "fields_json": fields_json,
            }
        })
    }

    fn catalog_response(hits: Vec<Value>) -> Value {
        json!({ "took": 1, "hits": {
            "total": { "value": hits.len(), "relation": "eq" }, "hits": hits
        } })
    }

    fn args(index: Option<&str>) -> MapArgs {
        MapArgs {
            index: index.map(str::to_string),
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    fn disconnected_ctx() -> crate::Ctx {
        crate::Ctx {
            client: reqwest::Client::new(),
            base_url: "http://127.0.0.1:1".to_string(),
            auth: None,
        }
    }

    #[test]
    fn glob_match_handles_the_documented_shapes() {
        assert!(glob_match("ax-*", "ax-logs"));
        assert!(glob_match("ax-*", "ax-"));
        assert!(
            !glob_match("ax-*", "tax-logs"),
            "the star anchors the prefix"
        );
        assert!(glob_match("*", "anything"));
        assert!(glob_match("ax-logs", "ax-logs"), "no star = exact");
        assert!(!glob_match("ax-logs", "ax-logs2"));
        assert!(glob_match("ax-*-2026", "ax-logs-2026"));
        assert!(!glob_match("ax-*-2026", "ax-2026"));
    }

    #[test]
    fn a_small_catalog_renders_every_field_untrimmed() {
        let fields = r#"[
            {"name":"ts","es_type":"date","date_enc":"rfc3339","cardinality_est":900,
             "cardinality_overflow":false,"coverage":1.0,"null_ratio":0.0,"avg_len":24.0,
             "examples":["2026-03-01T00:00:00.000Z"],"date_min":"2026-03-01T00:00:00.000Z",
             "date_max":"2026-03-02T00:00:00.000Z","date_evidence":["rfc3339: 900"]},
            {"name":"status","es_type":"long","cardinality_est":4,"coverage":0.5,
             "null_ratio":0.5,"avg_len":0.0,"examples":["200"],"num_min":200,"num_max":404}
        ]"#;
        let resp = catalog_response(vec![dataset_doc("ax-logs", 10, fields)]);
        let out: Value = serde_json::from_str(&render_response(&resp, &args(None))).unwrap();
        assert_eq!(out["matched"], 1);
        let idx = &out["indexes"][0];
        assert_eq!(idx["index"], "ax-logs");
        assert_eq!(idx["records"], 10);
        assert_eq!(idx["field_count"], 2);
        assert!(idx.get("fields_omitted").is_none(), "nothing was dropped");
        assert!(idx.get("trimmed").is_none(), "nothing was trimmed: {idx}");
        let ts = &idx["fields"][0];
        assert_eq!(ts["name"], "ts");
        assert_eq!(ts["es_type"], "date");
        assert_eq!(ts["date_min"], "2026-03-01T00:00:00.000Z");
        assert_eq!(
            ts["num_min"],
            Value::Null,
            "a date field carries no numeric range"
        );
        let status = &idx["fields"][1];
        assert_eq!(status["num_min"], 200);
        assert_eq!(status["num_max"], 404);
        assert_eq!(status["coverage"], 0.5);
        // date_evidence / null_ratio / avg_len are projected out;
        // cardinality_overflow only appears when it is TRUE.
        for f in idx["fields"].as_array().unwrap() {
            assert!(f.get("date_evidence").is_none(), "{f}");
            assert!(f.get("null_ratio").is_none(), "{f}");
            assert!(f.get("avg_len").is_none(), "{f}");
            assert!(f.get("cardinality_overflow").is_none(), "{f}");
        }
        // ...and a capped field DOES carry the flag: cardinality_est is then
        // "at least this many".
        let capped = json!([{
            "name": "trace_id", "es_type": "keyword", "cardinality_est": 8192,
            "cardinality_overflow": true, "coverage": 1.0
        }]);
        let capped_json = serde_json::to_string(&capped).unwrap();
        let resp = catalog_response(vec![dataset_doc("ax-traces", 99, &capped_json)]);
        let out: Value = serde_json::from_str(&render_response(&resp, &args(None))).unwrap();
        assert_eq!(out["indexes"][0]["fields"][0]["cardinality_overflow"], true);
    }

    /// #1055's hard rule: every rendered index entry is <= the per-index byte
    /// cap, and when the cap binds, the accounting says what was cut.
    #[test]
    fn a_wide_dataset_is_trimmed_to_the_cap_with_accounting() {
        let mut fields = Vec::new();
        for i in 0..300 {
            fields.push(json!({
                "name": format!("field_with_a_deliberately_long_name_{i:03}"),
                "es_type": "keyword",
                "cardinality_est": i,
                "coverage": 1.0 - (i as f64) / 1000.0,
                "examples": [format!("example-value-{i:03}a"), format!("example-value-{i:03}b")],
                "notes": ["a note of ordinary length for the trim ladder to consider"],
            }));
        }
        let fields_json = serde_json::to_string(&Value::Array(fields)).unwrap();
        let resp = catalog_response(vec![dataset_doc("ax-wide", 5000, &fields_json)]);
        let out: Value = serde_json::from_str(&render_response(&resp, &args(None))).unwrap();
        let idx = &out["indexes"][0];
        let raw = serde_json::to_string(idx).unwrap();
        assert!(
            raw.len() <= DEFAULT_MAX_BYTES,
            "entry is {} bytes",
            raw.len()
        );
        assert_eq!(idx["field_count"], 300);
        assert!(
            idx["fields_omitted"].as_u64().unwrap() > 0,
            "fields had to go"
        );
        // The whole ladder ran, in order — examples were still gone by the
        // time fields started going, which is what the order pins.
        let trimmed: Vec<&str> = idx["trimmed"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|v| v.as_str())
            .collect();
        assert_eq!(
            trimmed,
            [
                "examples-cut-to-1",
                "examples-dropped",
                "notes-dropped",
                "fields-dropped"
            ],
            "{idx}"
        );
        // Highest coverage survived: the ladder drops from the END.
        let kept = idx["fields"].as_array().unwrap();
        assert!(kept.len() >= 2, "ladder kept {} fields", kept.len());
        let cov = |f: &Value| f["coverage"].as_f64().unwrap();
        assert!(
            (cov(&kept[0]) - 1.0).abs() < 1e-9,
            "first kept field is the best covered"
        );
        assert!(cov(&kept[kept.len() - 1]) < cov(&kept[0]));
        assert!(
            kept.iter().all(|f| f.get("examples").is_none()),
            "examples were dropped before fields were"
        );
    }

    /// The ladder order itself: examples before notes, notes before fields.
    /// A dataset whose fields all fit once examples and notes are gone must
    /// keep every field, and a stage that cut nothing must not be reported.
    #[test]
    fn the_ladder_trims_examples_then_notes_then_fields() {
        let with_payload = |examples: bool, notes: bool| {
            let mut fields = Vec::new();
            for i in 0..30 {
                let mut f = json!({
                    "name": format!("f{i:02}"),
                    "es_type": "keyword",
                    "cardinality_est": i,
                    "coverage": 0.9,
                });
                if examples {
                    f["examples"] = json!([format!("ex-{i:02}")]);
                }
                if notes {
                    f["notes"] = json!(["note"]);
                }
                fields.push(f);
            }
            serde_json::to_string(&Value::Array(fields)).unwrap()
        };
        let resp_full =
            catalog_response(vec![dataset_doc("ax-ladder", 9, &with_payload(true, true))]);
        let resp_base = catalog_response(vec![dataset_doc(
            "ax-ladder",
            9,
            &with_payload(false, false),
        )]);
        let huge = MapArgs {
            index: None,
            max_bytes: 1_000_000,
        };
        let full_len = serde_json::to_string(
            &serde_json::from_str::<Value>(&render_response(&resp_full, &huge)).unwrap()["indexes"]
                [0],
        )
        .unwrap()
        .len();
        let base_len = serde_json::to_string(
            &serde_json::from_str::<Value>(&render_response(&resp_base, &huge)).unwrap()["indexes"]
                [0],
        )
        .unwrap()
        .len();
        // A budget the notes-trimmed entry fits but the examples-trimmed one
        // cannot: examples cost more bytes than the slack we leave.
        let budget = base_len + 300;
        assert!(
            full_len > budget,
            "test premise: the full entry ({full_len}) must overshoot {budget}"
        );
        let tight = MapArgs {
            index: None,
            max_bytes: budget,
        };
        let out: Value = serde_json::from_str(&render_response(&resp_full, &tight)).unwrap();
        let idx = &out["indexes"][0];
        let raw = serde_json::to_string(idx).unwrap();
        assert!(raw.len() <= budget, "{} > {budget}: {raw}", raw.len());
        assert!(
            idx.get("fields_omitted").is_none(),
            "all 30 fields fit once notes went: {idx}"
        );
        let trimmed: Vec<&str> = idx["trimmed"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|v| v.as_str())
            .collect();
        assert_eq!(
            trimmed,
            ["examples-dropped", "notes-dropped"],
            "examples were already 1 each, so the cut-to-1 stage must not be reported"
        );
    }

    #[test]
    fn a_single_field_that_alone_exceeds_the_budget_is_dropped_whole() {
        let huge = json!([{
            "name": format!("n{}", "x".repeat(3000)),
            "es_type": "keyword",
            "cardinality_est": 1,
            "coverage": 1.0,
        }]);
        let fields_json = serde_json::to_string(&huge).unwrap();
        let resp = catalog_response(vec![dataset_doc("ax-huge", 1, &fields_json)]);
        let tight = MapArgs {
            index: None,
            max_bytes: MIN_MAX_BYTES,
        };
        let out: Value = serde_json::from_str(&render_response(&resp, &tight)).unwrap();
        let idx = &out["indexes"][0];
        let raw = serde_json::to_string(idx).unwrap();
        assert!(raw.len() <= MIN_MAX_BYTES, "{raw}");
        assert_eq!(idx["fields"].as_array().unwrap().len(), 0);
        assert_eq!(idx["fields_omitted"], 1);
        assert_eq!(idx["field_count"], 1, "the count still tells the truth");
    }

    #[test]
    fn exact_names_and_globs_filter_engine_side() {
        let f = r#"[{"name":"a","es_type":"keyword","cardinality_est":1,"coverage":1.0,"examples":["a"]}]"#;
        let hits = vec![
            dataset_doc("ax-logs", 10, f),
            dataset_doc("ax-metrics", 20, f),
            dataset_doc("other", 30, f),
        ];
        let resp = catalog_response(hits);

        // Exact name: engine filters, so the renderer passes it through.
        let body = catalog_body(&args(Some("ax-logs")));
        assert_eq!(body["query"]["bool"]["must"].as_array().unwrap().len(), 2);

        // A glob must filter before the 100-result cap: otherwise a matching
        // low-record dataset can be omitted by unrelated larger datasets.
        let body = catalog_body(&args(Some("ax-*")));
        assert_eq!(
            body["query"]["bool"]["must"][1],
            json!({ "wildcard": { "index_name": "ax-*" } })
        );
        let out: Value =
            serde_json::from_str(&render_response(&resp, &args(Some("ax-*")))).unwrap();
        assert_eq!(out["matched"], 2);
        let names: Vec<&str> = out["indexes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["index"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["ax-metrics", "ax-logs"],
            "records desc, deterministic"
        );
        assert_eq!(out["pattern"], "ax-*");

        // No match is NOT an error: 0 matched plus the hint.
        let out: Value =
            serde_json::from_str(&render_response(&resp, &args(Some("zz-*")))).unwrap();
        assert_eq!(out["matched"], 0);
        assert!(out["hint"].as_str().unwrap().contains("xerj autoindex"));
    }

    #[test]
    fn dataset_notes_ride_the_entry_and_survive_small_caps() {
        let f = r#"[{"name":"a","es_type":"keyword","cardinality_est":1,"coverage":1.0}]"#;
        let mut doc = dataset_doc("ax-notes", 7, f);
        doc["_source"]["notes"] = json!(["dataset-level note from the run"]);
        let resp = catalog_response(vec![doc]);
        let out: Value = serde_json::from_str(&render_response(&resp, &args(None))).unwrap();
        assert_eq!(
            out["indexes"][0]["notes"],
            json!(["dataset-level note from the run"])
        );
    }

    #[test]
    fn indexes_past_the_fetch_cap_are_reported_not_silently_dropped() {
        let f = r#"[{"name":"a","es_type":"keyword","cardinality_est":1,"coverage":1.0}]"#;
        let mut resp = catalog_response(vec![dataset_doc("ax-a", 1, f)]);
        resp["hits"]["total"]["value"] = json!(250);
        let out: Value = serde_json::from_str(&render_response(&resp, &args(None))).unwrap();
        assert_eq!(out["matched"], 1);
        assert_eq!(out["indexes_omitted"], 250 - 1);
    }

    #[test]
    fn a_corrupt_fields_json_renders_an_empty_field_list_not_an_error() {
        let resp = catalog_response(vec![dataset_doc("ax-bad", 5, "not json")]);
        let out: Value = serde_json::from_str(&render_response(&resp, &args(None))).unwrap();
        assert_eq!(out["matched"], 1);
        assert_eq!(out["indexes"][0]["field_count"], 0);
        assert_eq!(out["indexes"][0]["fields"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn argument_validation_refuses_mistyped_and_sub_floor_budgets() {
        assert_eq!(parse_args(&json!({})).unwrap().max_bytes, DEFAULT_MAX_BYTES);
        assert!(parse_args(&json!({ "index": null, "max_bytes": null })).is_ok());
        assert_eq!(parse_args(&json!({ "index": "  " })).unwrap().index, None);
        assert_eq!(
            parse_args(&json!({ "index": "ax-*" }))
                .unwrap()
                .index
                .as_deref(),
            Some("ax-*")
        );
        let e = parse_args(&json!({ "index": 5 })).unwrap_err();
        assert!(e.contains("`index` must be a string"), "{e}");
        let e = parse_args(&json!({ "max_bytes": 100 })).unwrap_err();
        assert!(e.contains(">= 1024"), "{e}");
        let e = parse_args(&json!({ "max_bytes": "4096" })).unwrap_err();
        assert!(e.contains("`max_bytes` must be an integer"), "{e}");
        assert_eq!(
            parse_args(&json!({ "max_bytes": 2048 })).unwrap().max_bytes,
            2048
        );
    }

    /// The dispatch seam: a bad argument is an isError tool result BEFORE any
    /// request is made (the same shape as the `max_tokens` refusal test).
    #[tokio::test]
    async fn a_bad_argument_is_refused_at_dispatch() {
        let ctx = disconnected_ctx();
        for bad in [json!({ "max_bytes": 10 }), json!({ "index": 7 })] {
            let msg = json!({ "params": { "name": "xerj_map", "arguments": bad } });
            let res = crate::call_tool(&ctx, &msg).await;
            assert_eq!(res["isError"], true, "{bad}: {res}");
            assert!(
                res["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("xerj_map"),
                "{res}"
            );
        }
    }

    /// Transport failure reaches the agent as isError with the URL named —
    /// the same honesty as every other proxied tool. A silently empty map
    /// here would be the worst failure mode: it reads as "no data".
    #[tokio::test]
    async fn an_unreachable_node_is_an_error_not_an_empty_map() {
        let ctx = disconnected_ctx();
        let msg = json!({ "params": { "name": "xerj_map", "arguments": {} } });
        let res = crate::call_tool(&ctx, &msg).await;
        assert_eq!(res["isError"], true);
        let text = res["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("request to"), "{text}");
        assert!(text.contains(CATALOG_INDEX), "{text}");
    }
}
