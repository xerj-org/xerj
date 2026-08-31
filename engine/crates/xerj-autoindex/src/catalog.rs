//! The catalog index (`autoindex-catalog`) — deliberately OUTSIDE the ax-*
//! wildcard so data-wide searches never hit metadata. Its dataset docs ARE
//! the agent-facing data map; `xerj autoindex map` renders them.

use crate::correlate::KeyCorr;
use crate::state::PlanDataset;
use serde_json::{json, Value};

pub const CATALOG_INDEX: &str = "autoindex-catalog";

/// The corpus-scope field the #737/#693 exclusion sweeps term-query (#755).
///
/// It exists because `prefix` cannot be relied on to be a `keyword` on an
/// upgraded install. v1.0.0-rc.15 (`61b31ef3`) started writing `prefix` on the
/// catalog's run document while [`catalog_mapping`] still did not declare it,
/// so every catalog touched by rc.15..rc.67 has `prefix` **dynamically inferred
/// as `text`** — and a `term` query against an analyzed field does not match a
/// raw scope value. #737 (rc.68) then declared `prefix` as `keyword` and
/// installed it with a hard `update_mapping`, which is the 400 that aborted
/// every run on those installs.
///
/// This field is the migration: no release before this one ever wrote it, so no
/// existing catalog can hold a conflicting inferred type for it, and declaring
/// it `keyword` is always accepted. Every catalog document that carries
/// `prefix` carries `corpus_scope` with the same value, and the sweeps query
/// both — `prefix` for documents written by rc.68..this release on a catalog
/// where it really is a keyword, `corpus_scope` for everything written from
/// here on.
///
/// It is installed **additively**, by `ensure_generation_mappings` on the
/// generated path and by its own tolerant install on the legacy graph path —
/// deliberately not by [`catalog_mapping`], whose value is a frozen digest an
/// already-committed generation is compared against. See that function's second
/// tripwire.
pub const CORPUS_SCOPE_FIELD: &str = "corpus_scope";

/// Explicit mapping for a **freshly created** catalog index.
///
/// Tripwire — `started` is bimodal across installs, on purpose. This function
/// declares it `date`; the additive upgrade in `run_index_report` deliberately
/// omits it. No release before this one declared `started` at all, so every
/// existing catalog got it from dynamic inference, and which type that produced
/// depends on which release wrote the catalog:
///
/// - **v1.0.0-rc.4** — the only release with `autoindex` (0f3ef60d, 2026-07-09)
///   but without dynamic ISO-date inference (a0f872ac, 2026-07-25, first in
///   rc.5). Its catalogs inferred `started` as **`text`**. Adding `started` as
///   `date` to those is refused **400 `mapper_parsing_exception`** — *"field
///   [started] already exists as [text], cannot add [date]"* — from the
///   `idx.schema()` guard in `xerj-api/src/es_compat.rs` (the
///   `XerjError::invalid_mapping` arm; `InvalidMapping` maps to
///   `mapper_parsing_exception` in `xerj-api/src/error.rs`). `es.update_mapping`
///   surfaces that as an `Err`, which aborts the invocation before any document
///   work.
/// - **v1.0.0-rc.5 and later** — inference already produced `date`, so the same
///   upgrade would simply be acknowledged 200.
///
/// It is specifically NOT the `illegal_argument_exception` *"mapper [started]
/// cannot be changed from type [text] to [date]"* guard earlier in the same
/// handler. That one reads `state.engine.index_mappings`, which holds only
/// *declared* mappings, and `started` was never declared — so for a legacy
/// catalog it cannot fire. (Measured against a live engine, v1.0.0-rc.13: text
/// inferred → `mapper_parsing_exception`; text declared →
/// `illegal_argument_exception`; date inferred → 200 acknowledged.)
///
/// So a catalog created from here sorts `started` server-side, an rc.4 catalog
/// does not, and nothing may rely on `started` being a `date`: `run_map` sorts
/// runs client-side, which is what keeps the split benign. Any future
/// server-side range query, `sort`, or date aggregation on `started` must first
/// migrate legacy catalogs by reindexing them — not by adding `started` to the
/// additive upgrade, which is exactly the abort above.
///
/// Second tripwire — **this value is a frozen on-disk contract** (#755). It is
/// hashed into `index_identity` by `generation_contract_identities`, and that
/// digest is written into every committed generation's execution record in the
/// journal. Two live comparisons then demand equality against a record an
/// *older binary* froze: the incremental-reconcile no-change arm, and
/// `provision_generation`'s replay of a pending generation. So adding or
/// removing a property here does not "plan a fresh generation" — it makes the
/// next run of every existing state dir abort, permanently on the no-change arm
/// because that arm writes no new generation. A field the catalog needs but the
/// contract does not can be installed additively in `ensure_generation_mappings`
/// (`duplicate_of`, `CORPUS_SCOPE_FIELD`) instead. `catalog_mapping_is_the_frozen_on_disk_contract`
/// pins the current value; changing it deliberately means also giving the
/// identity comparisons an upgrade path.
pub fn catalog_mapping() -> Value {
    // The trailing run-metadata fields are inserted after the literal rather
    // than written inside it: `serde_json::json!` recurses once per key, and at
    // 40 properties the macro exceeds the default `recursion_limit = 128` and
    // fails to compile. Adding another field here must use this same tail, not
    // the literal.
    let mut mapping = json!({
        "mappings": {"properties": {
            "doc_kind": {"type": "keyword"},
            "slug": {"type": "keyword"},
            "index_name": {"type": "keyword"},
            "record_count": {"type": "long"},
            "junk_records": {"type": "long"},
            "bytes": {"type": "long"},
            "file_count": {"type": "long"},
            "formats": {"type": "keyword"},
            "time_field": {"type": "keyword"},
            "time_min": {"type": "date", "format": "strict_date_optional_time||epoch_millis"},
            "time_max": {"type": "date", "format": "strict_date_optional_time||epoch_millis"},
            "semantic_field": {"type": "keyword"},
            "fields_json": {"type": "text"},
            "sample_queries_json": {"type": "text"},
            "notes": {"type": "text"},
            "path": {"type": "keyword"},
            "file_key": {"type": "keyword"},
            "format": {"type": "keyword"},
            "status": {"type": "keyword"},
            "reason": {"type": "text"},
            "duplicate_of": {"type": "keyword"},
            "records": {"type": "long"},
            "junk": {"type": "long"},
            "run_id": {"type": "keyword"},
            "corr_kind": {"type": "keyword"},
            "a_dataset": {"type": "keyword"},
            "b_dataset": {"type": "keyword"},
            "a_index": {"type": "keyword"},
            "b_index": {"type": "keyword"},
            "a_field": {"type": "keyword"},
            "b_field": {"type": "keyword"},
            "grade": {"type": "keyword"},
            "overlap": {"type": "long"},
            "containment": {"type": "double"},
            "range_overlap": {"type": "double"},
            "pearson_r": {"type": "double"},
            "activity_correlated": {"type": "boolean"},
        }}
    });
    let properties = mapping
        .pointer_mut("/mappings/properties")
        .and_then(Value::as_object_mut)
        .expect("catalog mapping properties");
    // See the tripwire on this function before touching `started`.
    properties.insert(
        "started".into(),
        json!({"type": "date", "format": "strict_date_optional_time||epoch_millis"}),
    );
    properties.insert(
        "summary_generated_at".into(),
        json!({"type": "date", "format": "strict_date_optional_time||epoch_millis"}),
    );
    properties.insert(
        "invocation_telemetry_scope".into(),
        json!({"type": "keyword"}),
    );
    properties.insert("junk_records_this_run".into(), json!({"type": "long"}));
    // #737: corpus scope, so a delete_by_query can constrain a sweep to THIS
    // corpus's docs. The doc `_id`s are already prefix-scoped (`file:{prefix}:…`)
    // but `_id` is not term-queryable; a byte-identical file shared with a live
    // sibling corpus would otherwise be caught by an unscoped `file_key`/`path`
    // sweep. Written by `file_doc`/`duplicate_file_doc` (the sweep's targets).
    properties.insert("prefix".into(), json!({"type": "keyword"}));
    // #755: `CORPUS_SCOPE_FIELD` is deliberately NOT declared here. This
    // function's value is hashed into `index_identity` (see the frozen-digest
    // tripwire on this function), so declaring it would move the digest and
    // abort every already-committed state dir on upgrade. It is installed
    // additively in `ensure_generation_mappings`, the same way `duplicate_of`
    // is, which is outside the hash.
    mapping
}

pub const GOTCHAS: &[&str] = &[
    "hybrid search: use {\"query\":{\"hybrid\":{\"queries\":[…]}}} ONLY — retriever.rrf is a silent stub and rank.rrf is ignored on this engine",
    "semantic_text fields are embedded server-side: the DEFAULT is the built-in LEXICAL feature-hash embedder (384-dim hybrid lexical+vector, NOT neural) — start the server with `--embed-mode neural` (built-in Candle BERT), `--embed-mode proxy`, or an ONNX-enabled build with `--embed-mode onnx-experimental --onnx-model … --onnx-tokenizer …` for neural semantics; ONNX runs only when this map shows a semantic_field, and its first real inference is confirmed by the server activation log",
    "semantic queries ignore _source filtering and return the ~8KB *_vector field in _source — strip client-side",
    "exact filters use TOP-LEVEL keyword fields (term on .keyword subfields returns 0 hits on this engine)",
    "all dates are normalized to RFC3339 UTC millis; mappings use strict_date_optional_time||epoch_millis",
    "query all data with the wildcard index pattern (e.g. ax-*) or comma lists — never multi-index aliases (they resolve to the first index only)",
    "documents were indexed with refresh at the end of the run; new writes need _refresh before they are searchable",
    "byte-identical paths are indexed once; canonical records retain every current filename in _source.ax_paths, and exact alias resolution is available through the catalog's duplicate_files entries",
];

pub struct DatasetDocInput<'a> {
    /// Corpus scope (`--prefix`): keeps catalog ids from colliding across corpora (#416).
    pub prefix: &'a str,
    pub pd: &'a PlanDataset,
    pub record_count: u64,
    pub junk_records: u64,
    /// Canonical source bytes backing this dataset's durably live records.
    ///
    /// This is not a scan-time filesystem total: a removed path stays
    /// represented until autoindex also removes its live records, and one
    /// source feeding several datasets contributes its bytes to each.
    pub bytes: u64,
    pub file_count: usize,
    pub formats: Vec<String>,
    pub time_min: Option<String>,
    pub time_max: Option<String>,
    pub sample_queries: Vec<Value>,
    pub notes: Vec<String>,
    pub run_id: &'a str,
}

pub fn dataset_doc(inp: &DatasetDocInput) -> (String, Value) {
    let id = format!("ds:{}:{}", inp.prefix, inp.pd.slug);
    let fields_json = serde_json::to_string(&inp.pd.specs).unwrap_or_else(|_| "[]".into());
    let doc = json!({
        "doc_kind": "dataset",
        "slug": inp.pd.slug,
        "index_name": inp.pd.index,
        "formats": inp.formats,
        "record_count": inp.record_count,
        "junk_records": inp.junk_records,
        "bytes": inp.bytes,
        "file_count": inp.file_count,
        "time_field": inp.pd.time_field,
        "time_min": inp.time_min,
        "time_max": inp.time_max,
        "semantic_field": inp.pd.semantic_field,
        "fields_json": fields_json,
        "sample_queries_json": inp.sample_queries.iter()
            .map(|q| serde_json::to_string(q).unwrap_or_default())
            .collect::<Vec<_>>(),
        "notes": inp.notes,
        "run_id": inp.run_id,
    });
    (id, doc)
}

/// Catalog document id for a file. One definition, because the sweep that
/// removes a junk/skipped file's document (`lib.rs`, "stale junk-catalog
/// sweep") has to name the id without holding the document.
pub fn file_id(prefix: &str, file_key: &str) -> String {
    format!("file:{prefix}:{file_key}")
}

#[allow(clippy::too_many_arguments)] // 1:1 with the file-status doc's fields
pub fn file_doc(
    prefix: &str,
    file_key: &str,
    path: &str,
    format: &str,
    status: &str,
    reason: Option<&str>,
    records: u64,
    junk: u64,
    bytes: u64,
    run_id: &str,
) -> (String, Value) {
    (
        file_id(prefix, file_key),
        json!({
            "doc_kind": "file",
            "prefix": prefix, // #737: corpus scope for a scoped exclusion sweep
            // #755: the same scope on a field that is `keyword` even on a
            // catalog whose `prefix` an older build left inferred as `text`.
            CORPUS_SCOPE_FIELD: prefix,
            "file_key": file_key,
            "path": path,
            "format": format,
            "status": status,
            "reason": reason,
            "records": records,
            "junk": junk,
            "bytes": bytes,
            "run_id": run_id,
        }),
    )
}

pub fn duplicate_file_doc(
    prefix: &str,
    file_key: &str,
    path: &str,
    path_id: &str,
    duplicate_of: &str,
    bytes: u64,
    run_id: &str,
) -> (String, Value) {
    let alias_id = duplicate_file_id(prefix, file_key, path, path_id);
    (
        alias_id,
        json!({
            "doc_kind": "file",
            "prefix": prefix, // #737: corpus scope for a scoped exclusion sweep
            // #755: keyword-safe scope, see `file_doc`.
            CORPUS_SCOPE_FIELD: prefix,
            "file_key": file_key,
            "path": path,
            "format": "duplicate",
            "status": "duplicate",
            "reason": format!("byte-identical content already indexed from {duplicate_of}"),
            "duplicate_of": duplicate_of,
            "records": 0,
            "junk": 0,
            "bytes": bytes,
            "run_id": run_id,
        }),
    )
}

pub fn duplicate_file_id(prefix: &str, file_key: &str, path: &str, path_id: &str) -> String {
    let identity = if path_id.is_empty() { path } else { path_id };
    format!(
        "{ALIAS_ID_PREFIX}{}:{}",
        prefix,
        crate::ids::doc_id("duplicate-file", file_key, identity)
    )
}

/// The `file-alias:` prefix every alias id has carried since v1.0.0-rc.10.
pub const ALIAS_ID_PREFIX: &str = "file-alias:";

/// Every `_id` a **pre-#416** build could have written for this alias.
///
/// #416 (v1.0.0-rc.57) put the corpus prefix into the id —
/// `file-alias:{prefix}:{body}`. Before it, from rc.10 (`c529e604`) onwards,
/// the id was `file-alias:{body}` with the identical body, and the document
/// carried no corpus field either (`prefix` arrived with #737/rc.68,
/// [`CORPUS_SCOPE_FIELD`] with #755). Such a document therefore names no
/// corpus anywhere, which is why #905's sweep reconstructs the ids its OWN
/// corpus would have written instead of guessing from the document.
///
/// Two candidates, not one, because `path_id` is `#[serde(default)]` on
/// `state::DuplicateFile`: a plan written before this alias's `path_id` was
/// recorded produced `identity = path`, and the same plan today produces
/// `identity = path_id`. The two coincide when `path_id` is empty, and the
/// caller collects them into a set.
pub fn unprefixed_duplicate_file_ids(file_key: &str, path: &str, path_id: &str) -> Vec<String> {
    let mut ids = vec![format!(
        "{ALIAS_ID_PREFIX}{}",
        crate::ids::doc_id("duplicate-file", file_key, path)
    )];
    if !path_id.is_empty() {
        ids.push(format!(
            "{ALIAS_ID_PREFIX}{}",
            crate::ids::doc_id("duplicate-file", file_key, path_id)
        ));
    }
    ids
}

/// Build the five ready-to-send query classes for a dataset.
/// Only verified-working forms are ever emitted.
pub fn build_sample_queries(pd: &PlanDataset, correlations: &[KeyCorr]) -> Vec<Value> {
    let mut out = Vec::new();
    let specs = &pd.specs;

    // 1. exact filter: keyword field, low-ish cardinality, best coverage
    let filter_field = specs
        .iter()
        .filter(|s| s.es_type == "keyword" && s.cardinality_est >= 2 && !s.examples.is_empty())
        .max_by(|a, b| {
            let score = |s: &crate::infer::FieldSpec| {
                let card_bonus = if s.cardinality_est <= 1000 { 1.0 } else { 0.0 };
                s.coverage + card_bonus
            };
            score(a).partial_cmp(&score(b)).unwrap()
        });
    if let Some(f) = filter_field {
        out.push(json!({
            "class": "exact_filter",
            "title": format!("Exact filter on {}", f.name),
            "request": format!("POST /{}/_search", pd.index),
            "body": {"query": {"term": {(f.name.clone()): f.examples[0].clone()}}, "size": 3}
        }));
    }

    // 2. full text
    let text_field = specs
        .iter()
        .filter(|s| (s.es_type == "text" || s.es_type == "semantic_text") && !s.examples.is_empty())
        .max_by(|a, b| a.avg_len.partial_cmp(&b.avg_len).unwrap());
    if let Some(f) = text_field {
        let word = f
            .examples
            .iter()
            .flat_map(|e| e.split_whitespace())
            .filter(|w| w.len() >= 4 && w.chars().all(|c| c.is_ascii_alphanumeric()))
            .max_by_key(|w| w.len())
            .unwrap_or("data")
            .to_string();
        // Code datasets additionally carry `defs` (newline-joined "kind
        // name" per symbol, identifying the file that DEFINES a symbol —
        // see `dataset.rs`). `defs` indexes identifiers as WHOLE tokens:
        // `strip_shortcodes` is one term, and neither `strip` nor
        // `shortcodes` matches it. A plain `match` on `defs` is an OR over
        // the query's tokens, so a conceptual query still hits many
        // documents through incidental overlap with symbol and kind words —
        // which is what a `defs` boost amplifies. The phrase clause instead
        // requires the whole token sequence, so a symbol lookup resolves to
        // its definition while a multi-word conceptual query matches nothing
        // at all and contributes no score. Note this is gated on word count,
        // not on intent: a single-word query is one token and can still
        // match a symbol of that name, which is why the boost stays modest.
        // Only emit this shape when `defs` actually exists on the dataset —
        // never assume it. `_source` is
        // projected to the two fields an agent needs to act on a hit
        // (`ax_path`, `title` — present on every ingested document, not just
        // code) and `fields: ["_passage"]` asks for the matching snippet
        // instead of the whole file body.
        let has_defs = specs.iter().any(|s| s.name == "defs");
        if has_defs {
            out.push(json!({
                "class": "full_text",
                "title": format!("Code-aware full-text (BM25) match on {} + defs", f.name),
                "request": format!("POST /{}/_search", pd.index),
                "body": {
                    "query": {"bool": {"should": [
                        {"multi_match": {
                            "query": word.clone(),
                            "fields": [f.name.clone(), "defs"],
                            "type": "most_fields",
                        }},
                        {"match_phrase": {"defs": {
                            "query": word.clone(),
                            "boost": 4,
                        }}},
                    ]}},
                    "_source": ["ax_path", "title"],
                    "fields": ["_passage"],
                    "size": 3,
                }
            }));
        } else {
            out.push(json!({
                "class": "full_text",
                "title": format!("Full-text (BM25) match on {}", f.name),
                "request": format!("POST /{}/_search", pd.index),
                "body": {"query": {"match": {(f.name.clone()): word.clone()}}, "size": 3}
            }));
        }
        // 3. hybrid — only when a semantic_text field exists
        if let Some(sf) = &pd.semantic_field {
            out.push(json!({
                "class": "hybrid_lexical_vector",
                "title": format!("Hybrid lexical+vector (RRF) on {sf} — embedder set server-side (lexical by default; Candle neural, proxy, or experimental ONNX if configured)"),
                "request": format!("POST /{}/_search", pd.index),
                "body": {"query": {"hybrid": {"queries": [
                    {"query": {"match": {(sf.clone()): word.clone()}}, "weight": 1},
                    {"query": {"semantic": {"field": sf, "query": word}}, "weight": 1}
                ]}}, "size": 3},
                "note": "strip *_vector from hits client-side (semantic queries ignore _source filtering)"
            }));
        }
    }

    // 4. analytics
    if let Some(t) = &pd.time_field {
        out.push(json!({
            "class": "analytics",
            "title": format!("Daily activity (date_histogram on {t})"),
            "request": format!("POST /{}/_search", pd.index),
            "body": {"size": 0, "aggs": {"per_day": {"date_histogram":
                {"field": t, "calendar_interval": "day"}}}}
        }));
    } else if let Some(f) = filter_field {
        out.push(json!({
            "class": "analytics",
            "title": format!("Top values of {}", f.name),
            "request": format!("POST /{}/_search", pd.index),
            "body": {"size": 0, "aggs": {"top": {"terms": {"field": f.name, "size": 10}}}}
        }));
    }

    // 5. cross-dataset pivot from a confirmed correlation
    let corr = correlations.iter().find(|c| {
        (c.a_slug == pd.slug || c.b_slug == pd.slug)
            && c.confirmed.map(|(n, _)| n > 0).unwrap_or(false)
    });
    if let Some(c) = corr {
        let (my_field, other_index, other_field, other_slug) = if c.a_slug == pd.slug {
            (&c.a_field, &c.b_index, &c.b_field, &c.b_slug)
        } else {
            (&c.b_field, &c.a_index, &c.a_field, &c.a_slug)
        };
        let example = c.examples.first().cloned().unwrap_or_default();
        out.push(json!({
            "class": "cross_dataset_pivot",
            "title": format!("Pivot {}.{} → {}.{}", pd.slug, my_field, other_slug, other_field),
            "steps": [
                {"request": format!("POST /{}/_search", pd.index),
                 "body": {"query": {"term": {(my_field.clone()): example.clone()}}, "size": 3},
                 "note": format!("step 1: find records in {} for a {} value", pd.index, my_field)},
                {"request": format!("POST /{}/_search", other_index),
                 "body": {"query": {"term": {(other_field.clone()): example}}, "size": 3},
                 "note": format!("step 2: pivot the same value into {}", other_index)}
            ],
            "evidence": format!("sampled overlap {} values, containment {:.2}, confirmed {}/{} values live",
                c.overlap, c.containment,
                c.confirmed.map(|(n,_)| n).unwrap_or(0),
                c.confirmed.map(|(_,t)| t).unwrap_or(0)),
        }));
    }
    out
}

// ─── map rendering ───────────────────────────────────────────────────────

pub fn render_map(
    run: Option<&Value>,
    datasets: &[Value],
    correlations: &[Value],
    junk_files: &[Value],
    duplicate_files: &[Value],
    junk_total: u64,
) -> String {
    let mut s = String::new();
    s.push_str("# Data map (xerj autoindex)\n\n");
    if let Some(r) = run {
        let g = |k: &str| r.get(k).map(pretty_val).unwrap_or_default();
        s.push_str(&format!(
            "run `{}` — root `{}` — {} paths ({} unique content, {} duplicate aliases), {} records indexed, {} junk records, wall {}s\n\n",
            g("run_id"),
            g("root"),
            g("files_total"),
            g("unique_content_files"),
            g("duplicate_files"),
            g("records_total"),
            g("junk_records_total"),
            g("wall_seconds"),
        ));
    }
    s.push_str("## Datasets\n\n");
    s.push_str("| index | records | files | formats | time field | time range |\n");
    s.push_str("|---|---|---|---|---|---|\n");
    for d in datasets {
        let g = |k: &str| d.get(k).map(pretty_val).unwrap_or_default();
        let range = match (d.get("time_min"), d.get("time_max")) {
            (Some(a), Some(b)) if a.is_string() => {
                format!("{} → {}", pretty_val(a), pretty_val(b))
            }
            _ => "—".into(),
        };
        s.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} |\n",
            g("index_name"),
            g("record_count"),
            g("file_count"),
            g("formats"),
            if g("time_field").is_empty() {
                "—".into()
            } else {
                g("time_field")
            },
            range
        ));
    }
    s.push('\n');

    for d in datasets {
        let g = |k: &str| d.get(k).map(pretty_val).unwrap_or_default();
        s.push_str(&format!("### `{}`\n\n", g("index_name")));
        if let Some(sem) = d.get("semantic_field").filter(|v| v.is_string()) {
            s.push_str(&format!(
                "semantic body field: `{}` (hybrid lexical+vector; embedder set server-side — lexical by default, Candle neural, proxy, or experimental ONNX if configured)\n\n",
                pretty_val(sem)
            ));
        }
        // fields table
        if let Some(fj) = d.get("fields_json").and_then(|v| v.as_str()) {
            if let Ok(specs) = serde_json::from_str::<Vec<crate::infer::FieldSpec>>(fj) {
                s.push_str("| field | type | semantic | cardinality | null% | examples |\n");
                s.push_str("|---|---|---|---|---|---|\n");
                let mut sorted: Vec<&crate::infer::FieldSpec> = specs.iter().collect();
                sorted.sort_by(|a, b| {
                    b.coverage
                        .partial_cmp(&a.coverage)
                        .unwrap()
                        .then(a.name.cmp(&b.name))
                });
                for f in sorted.iter().take(40) {
                    let card = if f.cardinality_overflow {
                        format!("{}+", crate::infer::DISTINCT_CAP)
                    } else {
                        f.cardinality_est.to_string()
                    };
                    let mut ty = f.es_type.clone();
                    if let Some(e) = &f.date_enc {
                        ty = format!("{ty} ({e})");
                    }
                    s.push_str(&format!(
                        "| `{}` | {} | {} | {} | {:.0}% | {} |\n",
                        f.name,
                        ty,
                        f.semantic.clone().unwrap_or_else(|| "—".into()),
                        card,
                        f.null_ratio * 100.0,
                        f.examples
                            .iter()
                            .map(|e| {
                                let short: String = clean(e).chars().take(40).collect();
                                format!("`{}`", short.replace('|', "\\|").replace('`', "'"))
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                if specs.len() > 40 {
                    s.push_str(&format!(
                        "| … {} more fields | | | | | |\n",
                        specs.len() - 40
                    ));
                }
                s.push('\n');
            }
        }
        // sample queries
        if let Some(qs) = d.get("sample_queries_json").and_then(|v| v.as_array()) {
            s.push_str("Ready-to-send queries:\n\n");
            for q in qs {
                if let Some(qv) = q
                    .as_str()
                    .and_then(|t| serde_json::from_str::<Value>(t).ok())
                {
                    let title = qv.get("title").map(pretty_val).unwrap_or_default();
                    s.push_str(&format!(
                        "**{}** — `{}`\n\n",
                        title,
                        qv.get("request").map(pretty_val).unwrap_or_default()
                    ));
                    if let Some(body) = qv.get("body") {
                        s.push_str("```json\n");
                        s.push_str(&serde_json::to_string_pretty(body).unwrap_or_default());
                        s.push_str("\n```\n\n");
                    }
                    if let Some(steps) = qv.get("steps").and_then(|x| x.as_array()) {
                        for st in steps {
                            s.push_str(&format!(
                                "{} — `{}`\n\n```json\n{}\n```\n\n",
                                st.get("note").map(pretty_val).unwrap_or_default(),
                                st.get("request").map(pretty_val).unwrap_or_default(),
                                st.get("body")
                                    .map(|b| serde_json::to_string_pretty(b).unwrap_or_default())
                                    .unwrap_or_default()
                            ));
                        }
                        if let Some(ev) = qv.get("evidence") {
                            s.push_str(&format!("evidence: {}\n\n", pretty_val(ev)));
                        }
                    }
                    if let Some(note) = qv.get("note") {
                        s.push_str(&format!("note: {}\n\n", pretty_val(note)));
                    }
                }
            }
        }
        // notes
        if let Some(notes) = d.get("notes").and_then(|v| v.as_array()) {
            if !notes.is_empty() {
                s.push_str("Notes:\n");
                for n in notes {
                    s.push_str(&format!("- {}\n", pretty_val(n)));
                }
                s.push('\n');
            }
        }
    }

    if !duplicate_files.is_empty() {
        s.push_str(&format!(
            "## Duplicate aliases ({} paths; content indexed once)\n\n",
            duplicate_files.len()
        ));
        for file in duplicate_files.iter().take(30) {
            let g = |key: &str| file.get(key).map(pretty_val).unwrap_or_default();
            s.push_str(&format!("- `{}` → `{}`\n", g("path"), g("duplicate_of")));
        }
        if duplicate_files.len() > 30 {
            s.push_str(&format!("- … and {} more\n", duplicate_files.len() - 30));
        }
        s.push('\n');
    }

    if !correlations.is_empty() {
        s.push_str("## Cross-dataset correlations\n\n");
        for c in correlations {
            let g = |k: &str| c.get(k).map(pretty_val).unwrap_or_default();
            match c.get("corr_kind").and_then(|v| v.as_str()) {
                Some("key_overlap") => {
                    s.push_str(&format!(
                        "- **{}** key overlap: `{}`.`{}` ↔ `{}`.`{}` — sampled overlap {} values (containment {}), live-confirmed {}/{} values. Examples: {}\n",
                        g("grade"),
                        g("a_index"), g("a_field"), g("b_index"), g("b_field"),
                        g("overlap"), trunc_f(&g("containment")),
                        g("confirmed_values"), g("tested_values"),
                        g("examples"),
                    ));
                }
                Some("time_alignment") => {
                    s.push_str(&format!(
                        "- time alignment: `{}`.`{}` ↔ `{}`.`{}` — range overlap {}, shared buckets {}, Pearson r {}{}\n",
                        g("a_index"), g("a_field"), g("b_index"), g("b_field"),
                        trunc_f(&g("range_overlap")), g("shared_buckets"),
                        {
                            let r = trunc_f(&g("pearson_r"));
                            if r.is_empty() { "n/a (constant series)".to_string() } else { r }
                        },
                        if c.get("activity_correlated").and_then(|v| v.as_bool()).unwrap_or(false)
                            { " (activity correlated)" } else { "" },
                    ));
                }
                _ => {}
            }
        }
        s.push('\n');
    }

    if junk_total > 0 || !junk_files.is_empty() {
        s.push_str(&format!(
            "## Junk / skipped ({} files recorded, never fatal)\n\n",
            junk_files.len()
        ));
        for f in junk_files.iter().take(30) {
            let g = |k: &str| f.get(k).map(pretty_val).unwrap_or_default();
            s.push_str(&format!(
                "- `{}` — {} ({})\n",
                g("path"),
                g("status"),
                g("reason")
            ));
        }
        if junk_files.len() > 30 {
            s.push_str(&format!("- … and {} more\n", junk_files.len() - 30));
        }
        s.push('\n');
    }

    s.push_str("## Gotchas (verified on this engine)\n\n");
    for gtc in GOTCHAS {
        s.push_str(&format!("- {gtc}\n"));
    }
    s
}

fn pretty_val(v: &Value) -> String {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    clean(&s)
}

/// Strip control characters (raw data can contain NULs etc. — they would
/// make the rendered map read as a binary file).
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

fn trunc_f(s: &str) -> String {
    match s.parse::<f64>() {
        Ok(f) => format!("{f:.2}"),
        Err(_) => s.to_string(),
    }
}

#[cfg(test)]
mod duplicate_tests {
    use super::*;

    #[test]
    fn duplicate_aliases_render_separately_from_junk() {
        let duplicate = json!({
            "path": "copy.pdf",
            "duplicate_of": "report.pdf",
            "status": "duplicate"
        });
        let rendered = render_map(None, &[], &[], &[], &[duplicate], 0);
        assert!(rendered.contains("## Duplicate aliases"));
        assert_eq!(rendered.matches("## Duplicate aliases").count(), 1);
        assert!(rendered.contains("`copy.pdf` → `report.pdf`"));
        assert!(!rendered.contains("## Junk / skipped"));
    }
}

#[cfg(test)]
mod sample_query_tests {
    use super::*;
    use crate::infer::FieldSpec;

    fn spec(name: &str, es_type: &str, avg_len: f64, examples: &[&str]) -> FieldSpec {
        FieldSpec {
            name: name.into(),
            es_type: es_type.into(),
            date_enc: None,
            semantic: None,
            cardinality_est: 0,
            cardinality_overflow: false,
            null_ratio: 0.0,
            avg_len,
            coverage: 1.0,
            examples: examples.iter().map(|s| s.to_string()).collect(),
            notes: vec![],
            date_min: None,
            date_max: None,
            date_evidence: vec![],
        }
    }

    fn dataset(slug: &str, specs: Vec<FieldSpec>) -> PlanDataset {
        PlanDataset {
            slug: slug.into(),
            index: format!("ax-{slug}"),
            family: "document".into(),
            group: None,
            specs,
            time_field: None,
            semantic_field: None,
            sampled_records: 1,
            file_count: 1,
        }
    }

    /// A dataset carrying `defs` (the code extractor's per-symbol index —
    /// see `dataset.rs`) must get the code-aware bool shape: a `[body, defs]`
    /// `most_fields` query plus a self-gating `match_phrase` on `defs`, with
    /// `_source` projected and `_passage` requested instead of the whole file.
    #[test]
    fn code_dataset_with_defs_gets_a_code_aware_projected_passage_query() {
        let pd = dataset(
            "repo",
            vec![
                spec("title", "keyword", 8.0, &["main.rs"]),
                spec("body", "text", 400.0, &["fn main implementation body"]),
                spec("defs", "text", 20.0, &["function main"]),
            ],
        );
        let queries = build_sample_queries(&pd, &[]);
        let full_text = queries
            .iter()
            .find(|q| q["class"] == "full_text")
            .expect("a full_text sample query");
        let should = full_text["body"]["query"]["bool"]["should"]
            .as_array()
            .expect("code-aware query should have bool.should");
        assert_eq!(should.len(), 2);
        assert_eq!(should[0]["multi_match"]["fields"], json!(["body", "defs"]));
        assert_eq!(should[0]["multi_match"]["type"], "most_fields");
        assert_eq!(
            should[1]["match_phrase"]["defs"]["query"],
            should[0]["multi_match"]["query"]
        );
        // 4, not 8: measured identical to ^8 on the sealed holdout, better on
        // the smaller corpus, and it amplifies the single-word case least.
        assert_eq!(should[1]["match_phrase"]["defs"]["boost"], json!(4));
        assert_eq!(full_text["body"]["_source"], json!(["ax_path", "title"]));
        assert_eq!(full_text["body"]["fields"], json!(["_passage"]));
    }

    /// A dataset with no `defs` field (the overwhelming majority — plain
    /// data/text datasets) must keep the original single-field `match`
    /// shape unchanged: no `defs` reference, no forced projection.
    #[test]
    fn non_code_dataset_keeps_the_plain_single_field_match() {
        let pd = dataset(
            "logs",
            vec![spec(
                "message",
                "text",
                200.0,
                &["connection reset by peer"],
            )],
        );
        let queries = build_sample_queries(&pd, &[]);
        let full_text = queries
            .iter()
            .find(|q| q["class"] == "full_text")
            .expect("a full_text sample query");
        assert!(full_text["body"]["query"]["match"]["message"].is_string());
        assert!(full_text["body"]["query"].get("bool").is_none());
        assert!(full_text["body"]["query"].get("match_phrase").is_none());
        assert!(full_text["body"].get("_source").is_none());
        assert!(full_text["body"].get("fields").is_none());
    }

    /// #416 (data-loss): the shared `autoindex-catalog` index is written by
    /// every corpus, so its doc ids MUST be scoped by the corpus `--prefix` —
    /// otherwise two corpora indexing the same file key (or producing the same
    /// slug) overwrite each other's catalog entry. FAIL-BEFORE: dropping the
    /// prefix from the id bodies makes these `assert_ne!` collide.
    #[test]
    fn catalog_ids_are_prefix_scoped_across_corpora() {
        // Same key, different corpus prefix -> distinct ids (no overwrite).
        assert_ne!(super::file_id("ax-a", "k"), super::file_id("ax-b", "k"));
        assert_ne!(
            super::duplicate_file_id("ax-a", "k", "p", "pi"),
            super::duplicate_file_id("ax-b", "k", "p", "pi"),
        );
        // Idempotent within one corpus (a re-run must upsert, not duplicate).
        assert_eq!(super::file_id("ax-a", "k"), super::file_id("ax-a", "k"));
        // The prefix is actually in the id (not just concatenated key).
        assert!(super::file_id("ax-a", "k").starts_with("file:ax-a:"));
    }
}
