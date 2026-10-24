//! The catalog index (`autoindex-catalog`) — deliberately OUTSIDE the ax-*
//! wildcard so data-wide searches never hit metadata. Its dataset docs ARE
//! the agent-facing data map; `xerj autoindex map` renders them.

use std::collections::BTreeMap;

use crate::correlate::KeyCorr;
use crate::state::PlanDataset;
use serde_json::{json, Value};

pub const CATALOG_INDEX: &str = xerj_common::AUTOINDEX_CATALOG_INDEX;

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
    "indexes CREATED by this build stem English text fields (Snowball, via a declared `analysis.analyzer.default`) whenever the profiler marked the dataset's prose semantic — plurals and inflections match (`databases` ↔ `database`) on `text`/`semantic_text` fields of THOSE indexes; keyword fields are never stemmed, and indexes an older build created keep the unstemmed `standard` analyzer (analysis settings are fixed at index creation — reindex into a fresh index to change them)",
    "semantic_text fields are embedded server-side: the DEFAULT is the built-in LEXICAL feature-hash embedder (384-dim hybrid lexical+vector, NOT neural) — start the server with `--embed-mode neural` (built-in Candle BERT), `--embed-mode proxy`, or an ONNX-enabled build with `--embed-mode onnx-experimental --onnx-model … --onnx-tokenizer …` for neural semantics; ONNX runs only when this map shows a semantic_field, and its first real inference is confirmed by the server activation log. `--embed-mode` is fixed per index at ingest: changing it does not re-embed documents already written; re-index into a new index. A mapping inference_id or inference_endpoint does not switch the embedder",
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

/// Field list stored on a dataset doc.
///
/// The profiler never observes the `ax_*` provenance locators — they are
/// stamped after sampling — but the mapping builder maps every
/// `PROVENANCE_FIELDS` entry as `keyword` and every record carries them.
/// Leaving them out of
/// `fields_json` is what made `xerj_map` omit `ax_path` while `xerj_plan`
/// counted it (#2120). Appending here does not touch `pd.specs`: that vec
/// is hashed into `schema_identity`, and a new spec there would abort every
/// existing state dir.
pub fn catalog_field_specs(specs: &[crate::infer::FieldSpec]) -> Vec<crate::infer::FieldSpec> {
    let mut out = specs.to_vec();
    for name in crate::PROVENANCE_FIELDS {
        if out.iter().any(|s| s.name == *name) {
            continue;
        }
        out.push(provenance_catalog_spec(name));
    }
    out
}

/// A locator the pipeline stamps on every record. Coverage is 1 because the
/// stamp is unconditional. Cardinality is unmeasured (`0` plus the note) —
/// `ax_path` is high and `ax_dataset` is not, and inventing either number
/// would be a false claim.
fn provenance_catalog_spec(name: &str) -> crate::infer::FieldSpec {
    crate::infer::FieldSpec {
        name: name.to_string(),
        es_type: "keyword".into(),
        date_enc: None,
        semantic: None,
        cardinality_est: 0,
        cardinality_overflow: false,
        null_ratio: 0.0,
        avg_len: 0.0,
        coverage: 1.0,
        examples: Vec::new(),
        notes: vec![
            "autoindex provenance locator, mapped keyword and stamped on every record; \
             not sampled, so cardinality_est 0 means unmeasured"
                .into(),
        ],
        date_min: None,
        date_max: None,
        date_evidence: Vec::new(),
        num_min: None,
        num_max: None,
    }
}

pub fn dataset_doc(inp: &DatasetDocInput) -> (String, Value) {
    let id = format!("ds:{}:{}", inp.prefix, inp.pd.slug);
    let fields_json =
        serde_json::to_string(&catalog_field_specs(&inp.pd.specs)).unwrap_or_else(|_| "[]".into());
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

/// True when `index_name` belongs to corpus `prefix`.
///
/// Autoindex names dataset indices `{prefix}-{slug}`. When `slug` is stored on
/// the catalog document, reconstructing `{prefix}-{slug}` disambiguates
/// hyphenated prefixes (`prefix = "xc"` + `slug = "tantivy-code"` vs
/// `prefix = "xc-tantivy"` + `slug = "code"`). When `slug` is absent on a
/// legacy document, fall back to a `{prefix}-` boundary check.
pub fn index_matches_prefix(index_name: &str, slug: Option<&str>, prefix: &str) -> bool {
    match slug.filter(|s| !s.is_empty()) {
        Some(slug) => index_name
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|rest| rest == slug),
        None => index_name
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('-')),
    }
}

/// True when a `doc_kind: "dataset"` catalog document belongs to `prefix`.
pub fn dataset_matches_prefix(doc: &Value, prefix: &str) -> bool {
    let slug = doc.get("slug").and_then(Value::as_str);
    doc.get("index_name")
        .and_then(Value::as_str)
        .is_some_and(|idx| index_matches_prefix(idx, slug, prefix))
}

/// True when both endpoints of a `doc_kind: "correlation"` catalog document
/// belong to `prefix`.
pub fn correlation_matches_prefix(doc: &Value, prefix: &str) -> bool {
    let a_ok = doc
        .get("a_index")
        .and_then(Value::as_str)
        .is_some_and(|idx| {
            index_matches_prefix(idx, doc.get("a_dataset").and_then(Value::as_str), prefix)
        });
    let b_ok = doc
        .get("b_index")
        .and_then(Value::as_str)
        .is_some_and(|idx| {
            index_matches_prefix(idx, doc.get("b_dataset").and_then(Value::as_str), prefix)
        });
    a_ok && b_ok
}

/// True when a `doc_kind: "file"` catalog document belongs to `prefix`.
pub fn file_matches_prefix(doc: &Value, prefix: &str) -> bool {
    doc.get("prefix")
        .or_else(|| doc.get(CORPUS_SCOPE_FIELD))
        .and_then(Value::as_str)
        == Some(prefix)
}

/// Label used when a catalog run under `prefix` has no `root`.
///
/// An empty root cannot be proved to be this folder, so it counts as a
/// different source. The string is stable so the refusal can name it.
pub const UNRECORDED_PREFIX_ROOT: &str = "(root not recorded on the catalog run)";

/// Source roots of the *latest* catalog run under `prefix` that are not `root`.
///
/// Only `doc_kind: run` documents whose prefix matches exactly are considered.
/// A sibling prefix (`ax-2` when this run is `ax`) is not a collision: the
/// server filter can over-match a legacy analyzed `prefix` field. Older run
/// documents are history: after `--replace-prefix` the new folder's run is
/// the latest, and re-running that folder must not keep demanding the flag.
/// `started` is an ISO-8601 timestamp, so the greatest string is the latest.
/// Runs that share that timestamp are one cohort. Duplicate roots collapse
/// to one line.
pub fn foreign_prefix_roots(runs: &[Value], prefix: &str, root: &str) -> Vec<String> {
    let matching: Vec<&Value> = runs
        .iter()
        .filter(|run| run.get("doc_kind").and_then(Value::as_str) == Some("run"))
        .filter(|run| {
            run.get("prefix")
                .or_else(|| run.get(CORPUS_SCOPE_FIELD))
                .and_then(Value::as_str)
                == Some(prefix)
        })
        .collect();
    let latest = matching
        .iter()
        .filter_map(|run| run.get("started").and_then(Value::as_str))
        .max()
        .map(str::to_owned);
    let mut foreign = std::collections::BTreeSet::new();
    for run in matching {
        if let Some(started) = &latest {
            if run.get("started").and_then(Value::as_str) != Some(started.as_str()) {
                continue;
            }
        }
        let recorded_root = run.get("root").and_then(Value::as_str).unwrap_or("");
        if recorded_root == root {
            continue;
        }
        if recorded_root.is_empty() {
            foreign.insert(UNRECORDED_PREFIX_ROOT.to_string());
        } else {
            foreign.insert(recorded_root.to_string());
        }
    }
    foreign.into_iter().collect()
}

/// Prefixes other than `prefix` whose latest run recorded this same `root`.
///
/// #2078: indexing one folder twice, under `ax` and under a named prefix,
/// does not collide on catalog ids, so it is a warning rather than the
/// refusal [`foreign_prefix_roots`] produces. `root` is not a mapped catalog
/// field; callers fetch `doc_kind: run` and filter here.
pub fn other_prefixes_for_root(runs: &[Value], root: &str, prefix: &str) -> Vec<String> {
    let mut latest: BTreeMap<&str, &str> = BTreeMap::new();
    for run in runs {
        if run.get("doc_kind").and_then(Value::as_str) != Some("run") {
            continue;
        }
        let Some(recorded) = run.get("prefix").and_then(Value::as_str) else {
            continue;
        };
        if recorded == prefix || recorded.is_empty() {
            continue;
        }
        if run.get("root").and_then(Value::as_str) != Some(root) {
            continue;
        }
        let started = run.get("started").and_then(Value::as_str).unwrap_or("");
        match latest.get(recorded) {
            Some(prev) if *prev >= started => {}
            _ => {
                latest.insert(recorded, started);
            }
        }
    }
    latest.into_keys().map(str::to_string).collect()
}

/// One line. `warn` strips newlines, so this stays a single sentence.
pub fn same_root_other_prefix_line(prefix: &str, others: &[String]) -> String {
    let listed = others
        .iter()
        .map(|other| format!("{other} (indices {other}-*)"))
        .collect::<Vec<_>>()
        .join(", ");
    let first = others.first().map(String::as_str).unwrap_or(prefix);
    format!(
        "autoindex: this folder is already indexed under {listed}. This run uses {prefix} and \
         will index it again. Re-run with --prefix {first} to update that copy instead. Both \
         copies are kept: a {first}-* index can also hold other folders, so nothing here \
         suggests deleting it."
    )
}

/// What a reused prefix will do, for both the refusal and the
/// `--replace-prefix` warning. `replacing` is true only when the operator
/// passed the flag and the run is going to continue.
pub fn prefix_reuse_message(
    prefix: &str,
    root: &str,
    others: &[String],
    replacing: bool,
) -> String {
    let listed = others
        .iter()
        .map(|other| format!("  {other}"))
        .collect::<Vec<_>>()
        .join("\n");
    let ending = if replacing {
        format!(
            "--replace-prefix is set, so this run continues. Finalize will overwrite catalog \
             documents keyed ds:{prefix}:<slug> and write into {prefix}-* indices, retiring the \
             earlier datasets."
        )
    } else {
        "Nothing was written. The command above is the first remedy: one --prefix per source \
         folder. --replace-prefix retires those datasets on purpose; it is not the first option. \
         --dry-run does not bypass this check."
            .to_string()
    };
    let body = format!(
        "autoindex: --prefix {prefix} already indexes a different folder.\n\
         this run: {root}\n\
         already indexed from:\n\
         {listed}\n\
         Catalog datasets are keyed ds:{prefix}:<slug> and indices are named {prefix}-*. \
         Finalizing this run overwrites those catalog documents and retires the earlier datasets.\n\
         A shared prefix also drops per-repo provenance. Each repository indexed as its own \
         folder has an empty scope, so every one of them becomes the single dataset \
         {prefix}-docs (catalog id ds:{prefix}:docs). Index the parent folder that contains \
         the repositories, or pass a distinct --prefix per repository.\n\
         {ending}"
    );
    if replacing {
        return body;
    }
    let suggested = suggested_prefix(root, prefix);
    let command_root = root.replace(['\n', '\r'], " ");
    let command = format!(
        "xerj autoindex {} --prefix {}",
        shell_quote(&command_root),
        shell_quote(&suggested)
    );
    format!(
        "autoindex: this folder needs its own --prefix. Do not delete the other folder's indices.\n\
         Ready to copy:\n  {command}\n{body}"
    )
}

/// A prefix for `root` that is not `taken` and can name an index.
fn suggested_prefix(root: &str, taken: &str) -> String {
    let path = std::path::Path::new(root.trim());
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let parent = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let mut raw = Vec::new();
    if !name.is_empty() {
        raw.push(name.clone());
        if !parent.is_empty() {
            raw.push(format!("{parent}-{name}"));
        }
        raw.push(format!("{name}-2"));
        raw.push(format!("p{name}"));
    }
    raw.push(format!("p{taken}"));
    raw.push(format!("{taken}-2"));
    for candidate in raw {
        let sanitized = crate::dataset::sanitize_prefix(&candidate);
        if prefix_is_usable(&sanitized, taken) {
            return sanitized;
        }
    }
    for n in 3..64 {
        let sanitized = crate::dataset::sanitize_prefix(&format!("{taken}-{n}"));
        if prefix_is_usable(&sanitized, taken) {
            return sanitized;
        }
    }
    "folder".to_string()
}

fn prefix_is_usable(prefix: &str, taken: &str) -> bool {
    !prefix.is_empty() && prefix != taken && crate::dataset::index_prefix_error(prefix).is_none()
}

/// Quote a path for a shell command an agent can copy. Unquoted when every
/// character is safe; otherwise single quotes, with `'` written as `'\''`.
fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    let safe = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | '\\'));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// How many dataset detail sections the default map prints before it asks
/// for a page. The summary table always lists every dataset; the per-dataset
/// field tables and sample queries are what blow the map up to hundreds of
/// kilobytes.
pub const MAP_DETAIL_PAGE: usize = 8;

/// `(start, end)` of the detail page. `limit = None` prints every dataset
/// when there are at most [`MAP_DETAIL_PAGE`], and the first page otherwise.
pub fn detail_bounds(total: usize, offset: usize, limit: Option<usize>) -> (usize, usize) {
    let start = offset.min(total);
    let limit = limit.unwrap_or(if total > MAP_DETAIL_PAGE {
        MAP_DETAIL_PAGE
    } else {
        total
    });
    let end = start.saturating_add(limit).min(total);
    (start, end)
}

pub fn render_map(
    run: Option<&Value>,
    datasets: &[Value],
    correlations: &[Value],
    junk_files: &[Value],
    duplicate_files: &[Value],
    junk_total: u64,
) -> String {
    render_map_page(
        run,
        datasets,
        correlations,
        junk_files,
        duplicate_files,
        junk_total,
        0,
        None,
    )
}

#[allow(clippy::too_many_arguments)] // paging is two extra args on the existing map renderer
pub fn render_map_page(
    run: Option<&Value>,
    datasets: &[Value],
    correlations: &[Value],
    junk_files: &[Value],
    duplicate_files: &[Value],
    junk_total: u64,
    detail_offset: usize,
    detail_limit: Option<usize>,
) -> String {
    let mut s = String::new();
    s.push_str("# Data map (xerj autoindex)\n\n");
    if let Some(r) = run {
        let g = |k: &str| r.get(k).map(pretty_val).unwrap_or_default();
        let wall = g("wall_seconds");
        let wall_clause = if wall.is_empty() {
            String::new()
        } else {
            format!(", wall {wall}s")
        };
        // `junk_records_total` is records that failed extraction. A run can
        // list junk *files* (css, fonts, encrypted PDFs, "no records
        // extracted") while that counter stays 0. The headline has to name
        // the files the section below lists, and stay byte-identical when
        // the list is empty.
        let junk_records = g("junk_records_total");
        let junk_clause = if junk_files.is_empty() {
            format!("{junk_records} junk records")
        } else {
            format!(
                "{junk_records} junk records, {} junk/skipped files",
                junk_files.len()
            )
        };
        s.push_str(&format!(
            "run `{}` — root `{}` — {} paths ({} unique content, {} duplicate aliases), {} records indexed, {}{}\n\n",
            g("run_id"),
            g("root"),
            g("files_total"),
            g("unique_content_files"),
            g("duplicate_files"),
            g("records_total"),
            junk_clause,
            wall_clause,
        ));
    }
    // #2351: locator names differ by record kind. The paragraph is on every
    // map, including one rendered from a catalog written before the aliases.
    s.push_str(crate::HIT_LOCATOR_HINT);
    s.push_str("\n\n");
    // #929: a corpus that lacks a dataset says so at the top of its own map,
    // before the table that would otherwise read as the whole corpus.
    if let Some(refused) = run
        .and_then(|r| r.get("refused_datasets_json"))
        .and_then(Value::as_str)
        .and_then(|raw| serde_json::from_str::<Vec<Value>>(raw).ok())
        .filter(|refused| !refused.is_empty())
    {
        s.push_str("## Refused datasets — NOT indexed\n\n");
        s.push_str(
            "The server refused the mapping of these datasets, so their files are recorded as \
             junk and are not searchable. Every other dataset below is complete.\n\n",
        );
        for d in &refused {
            let g = |k: &str| d.get(k).map(pretty_val).unwrap_or_default();
            s.push_str(&format!(
                "- `{}` — {} file(s): {}\n",
                g("index"),
                g("files"),
                g("reason")
            ));
        }
        s.push('\n');
    }
    s.push_str(&format!("## Datasets ({})\n\n", datasets.len()));
    // `slug` is the argument to `--dataset`. The index name is
    // `{prefix}-{slug}`, and a hyphenated prefix makes the slug impossible
    // to recover from the index column alone.
    s.push_str("| slug | index | records | files | formats | time field | time range |\n");
    s.push_str("|---|---|---|---|---|---|---|\n");
    for d in datasets {
        let g = |k: &str| d.get(k).map(pretty_val).unwrap_or_default();
        let range = match (d.get("time_min"), d.get("time_max")) {
            (Some(a), Some(b)) if a.is_string() => {
                format!("{} → {}", pretty_val(a), pretty_val(b))
            }
            _ => "—".into(),
        };
        let slug = g("slug");
        s.push_str(&format!(
            "| {} | `{}` | {} | {} | {} | {} | {} |\n",
            if slug.is_empty() {
                "—".into()
            } else {
                format!("`{slug}`")
            },
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

    let (detail_start, detail_end) = detail_bounds(datasets.len(), detail_offset, detail_limit);
    let detail_truncated = detail_start > 0 || detail_end < datasets.len();
    if detail_truncated {
        s.push_str(&format!(
            "Field detail for datasets {}–{} of {} (largest first). \
             One dataset: `xerj autoindex map --dataset <slug>`. \
             Next page: `--offset {detail_end}`.\n\n",
            detail_start + 1,
            detail_end,
            datasets.len()
        ));
    }

    for d in datasets
        .iter()
        .skip(detail_start)
        .take(detail_end - detail_start)
    {
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
            if let Ok(parsed) = serde_json::from_str::<Vec<crate::infer::FieldSpec>>(fj) {
                // Old catalogs were written before locators were appended.
                // Rendering them here is what an already-indexed corpus's
                // `xerj autoindex map` shows, without a reindex (#2120).
                let specs = catalog_field_specs(&parsed);
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
                    let card = if f.notes.iter().any(|n| n.contains("not sampled")) {
                        "—".to_string()
                    } else if f.cardinality_overflow {
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
                            .map(|e| map_example_cell(e))
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

/// One markdown table cell for a field example.
///
/// [`clean`] keeps newlines and tabs, and this cell is interpolated into a
/// `| field | ... | examples |` row. A newline ends the row, so an example
/// whose next line starts with `##` is rendered as a heading. Collapse
/// whitespace onto one line, then cap the cell.
fn map_example_cell(raw: &str) -> String {
    let flat = clean(raw).split_whitespace().collect::<Vec<_>>().join(" ");
    let mut short: String = flat.chars().take(40).collect();
    if flat.chars().count() > 40 {
        short = flat.chars().take(39).collect();
        short.push('…');
    }
    format!("`{}`", short.replace('|', "\\|").replace('`', "'"))
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
    fn a_long_catalog_pages_field_detail_and_keeps_the_summary_table() {
        let datasets: Vec<Value> = (0..20)
            .map(|i| {
                json!({
                    "index_name": format!("ax-d{i:02}"),
                    "slug": format!("d{i:02}"),
                    "record_count": 100 - i,
                    "file_count": 1,
                    "formats": "json",
                    "fields_json": serde_json::to_string(&vec![crate::infer::FieldSpec {
                        name: format!("col{i}"),
                        es_type: "keyword".into(),
                        date_enc: None,
                        semantic: None,
                        cardinality_est: 1,
                        cardinality_overflow: false,
                        null_ratio: 0.0,
                        avg_len: 3.0,
                        coverage: 1.0,
                        examples: vec!["v".into()],
                        notes: vec![],
                        date_min: None,
                        date_max: None,
                        date_evidence: vec![],
                        num_min: None,
                        num_max: None,
                    }]).unwrap()
                })
            })
            .collect();
        let rendered = render_map(None, &datasets, &[], &[], &[], 0);
        assert!(
            rendered.contains("`ax-d00`"),
            "summary table lists every dataset"
        );
        assert!(rendered.contains("`ax-d19`"));
        assert!(
            rendered.contains("## Datasets (20)"),
            "the heading names how many datasets the table holds: {rendered}"
        );
        assert!(
            rendered.contains("| slug |"),
            "the summary names the slug `--dataset` takes"
        );
        assert!(
            rendered.contains("`d00`") && rendered.contains("`d19`"),
            "every row carries its slug, not only its index name: {rendered}"
        );
        assert_eq!(
            rendered.matches("### `").count(),
            super::MAP_DETAIL_PAGE,
            "detail sections stop at the page: {rendered}"
        );
        assert!(rendered.contains("Field detail for datasets 1–8 of 20"));
        assert!(rendered.contains("--offset 8"));
        let page = render_map_page(None, &datasets, &[], &[], &[], 0, 8, Some(2));
        assert!(page.contains("### `ax-d08`"));
        assert!(page.contains("### `ax-d09`"));
        assert!(!page.contains("### `ax-d10`"));
        assert!(page.contains("Field detail for datasets 9–10 of 20"));
    }

    fn one_field(name: &str, example: &str) -> crate::infer::FieldSpec {
        crate::infer::FieldSpec {
            name: name.into(),
            es_type: "text".into(),
            date_enc: None,
            semantic: None,
            cardinality_est: 1,
            cardinality_overflow: false,
            null_ratio: 0.0,
            avg_len: 3.0,
            coverage: 1.0,
            examples: vec![example.into()],
            notes: vec![],
            date_min: None,
            date_max: None,
            date_evidence: vec![],
            num_min: None,
            num_max: None,
        }
    }

    /// A newline in an example used to end the markdown row. The next line
    /// (`## Branded types…`) was then a heading in the middle of the table.
    #[test]
    fn catalog_fields_include_provenance_locators_the_profiler_never_saw() {
        let specs = catalog_field_specs(&[one_field("language", "rust")]);
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        for loc in crate::PROVENANCE_FIELDS {
            assert!(names.contains(loc), "missing {loc}: {names:?}");
        }
        let again = catalog_field_specs(&specs);
        assert_eq!(
            again.len(),
            specs.len(),
            "a locator already stored is not duplicated"
        );
        let ax_path = specs.iter().find(|s| s.name == "ax_path").unwrap();
        assert_eq!(ax_path.es_type, "keyword");
        assert_eq!(ax_path.coverage, 1.0);
        assert!(ax_path.notes.iter().any(|n| n.contains("not sampled")));
    }

    #[test]
    fn an_old_catalog_map_still_lists_ax_path() {
        let dataset = json!({
            "index_name": "ax-code",
            "slug": "code",
            "record_count": 10,
            "file_count": 1,
            "formats": "rs",
            "fields_json": serde_json::to_string(&vec![one_field("language", "rust")]).unwrap()
        });
        let rendered = render_map(None, &[dataset], &[], &[], &[], 0);
        assert!(rendered.contains("`ax_path`"), "{rendered}");
        assert!(rendered.contains("`ax_locator`"), "{rendered}");
        assert!(
            rendered.contains(crate::HIT_LOCATOR_HINT),
            "the map names locator fields even when the catalog predates them: {rendered}"
        );
        let row = rendered
            .lines()
            .find(|l| l.starts_with('|') && l.contains("`ax_path`"))
            .expect("ax_path row");
        assert!(
            row.contains("| — | 0%"),
            "unmeasured cardinality is not a fake 0: {row}"
        );
    }

    #[test]
    fn an_example_newline_stays_inside_its_table_row() {
        let dataset = json!({
            "index_name": "ax-branded",
            "slug": "branded",
            "record_count": 1,
            "file_count": 1,
            "formats": "json",
            "fields_json": serde_json::to_string(&vec![
                one_field("code", "code\n## Branded types for type guard"),
                one_field("wide", &format!("alpha\tbeta {}", "n".repeat(40))),
            ]).unwrap()
        });
        let rendered = render_map(None, &[dataset], &[], &[], &[], 0);
        assert!(
            !rendered.lines().any(|l| l.starts_with("## Branded")),
            "a newline in an example forged a heading:\n{rendered}"
        );
        let row = rendered
            .lines()
            .find(|l| l.contains("code ## Branded"))
            .expect("the example stays on the field row");
        assert!(row.starts_with('|'), "{row}");
        assert!(!row.contains('\n'));
        let wide = rendered
            .lines()
            .find(|l| l.contains("alpha beta"))
            .expect("a tab is a space");
        assert!(wide.starts_with('|'), "{wide}");
        assert!(wide.contains('…'), "a long example is capped: {wide}");
        assert!(!wide.contains(&"n".repeat(40)), "{wide}");
    }

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

    #[test]
    fn render_map_omits_blank_wall_seconds_on_generation_catalog_run_docs() {
        // `generation_catalog.rs` omits `wall_seconds` so the run document is a
        // deterministic projection of the generation; `render_map` must not
        // emit a dangling `, wall s` when the field is absent.
        let gen_run = json!({
            "doc_kind": "run",
            "run_id": "gen-1",
            "root": "/corpus",
            "prefix": "xc-tantivy",
            "files_total": 10,
            "unique_content_files": 9,
            "duplicate_files": 1,
            "records_total": 42,
            "junk_records_total": 0
        });
        let rendered = render_map(Some(&gen_run), &[], &[], &[], &[], 0);
        assert!(
            !rendered.contains("wall s"),
            "must not render blank `wall s` when wall_seconds is absent: {rendered}"
        );
        assert!(
            rendered.contains("42 records indexed, 0 junk records\n\n"),
            "run summary line should end cleanly at junk records: {rendered}"
        );

        // When `wall_seconds` IS present (legacy graph path), keep `, wall Xs`.
        let legacy_run = json!({
            "doc_kind": "run",
            "run_id": "run-legacy",
            "root": "/corpus",
            "files_total": 10,
            "unique_content_files": 9,
            "duplicate_files": 1,
            "records_total": 42,
            "junk_records_total": 0,
            "wall_seconds": 1.4
        });
        let rendered_legacy = render_map(Some(&legacy_run), &[], &[], &[], &[], 0);
        assert!(
            rendered_legacy.contains("0 junk records, wall 1.4s\n\n"),
            "legacy wall_seconds must still render: {rendered_legacy}"
        );
    }

    /// The summary used to say "0 junk records" on a map whose later section
    /// listed 17 skipped files. The file count belongs in the headline when
    /// the list is non-empty, and only then.
    #[test]
    fn the_summary_names_junk_files_when_the_list_is_nonempty() {
        let run = json!({
            "doc_kind": "run",
            "run_id": "nb",
            "root": "/corpus",
            "files_total": 36,
            "unique_content_files": 36,
            "duplicate_files": 0,
            "records_total": 286,
            "junk_records_total": 0
        });
        let junk = vec![
            json!({"path": "style.css", "status": "skipped", "reason": "no records extracted"}),
            json!({"path": "font.woff", "status": "skipped", "reason": "no records extracted"}),
        ];
        let rendered = render_map(Some(&run), &[], &[], &junk, &[], junk.len() as u64);
        assert!(
            rendered.contains("286 records indexed, 0 junk records, 2 junk/skipped files\n\n"),
            "{rendered}"
        );
        assert!(rendered.contains("## Junk / skipped (2 files recorded"));
    }

    #[test]
    fn prefix_scoping_disambiguates_hyphenated_prefixes_across_catalog_doc_kinds() {
        let tantivy_ds = json!({
            "doc_kind": "dataset",
            "slug": "code",
            "index_name": "xc-tantivy-code"
        });
        assert!(dataset_matches_prefix(&tantivy_ds, "xc-tantivy"));
        assert!(!dataset_matches_prefix(&tantivy_ds, "xc"));
        assert!(!dataset_matches_prefix(&tantivy_ds, "xc-tantivy-extra"));

        let xc_hyphen_slug_ds = json!({
            "doc_kind": "dataset",
            "slug": "tantivy-code",
            "index_name": "xc-tantivy-code"
        });
        assert!(dataset_matches_prefix(&xc_hyphen_slug_ds, "xc"));
        assert!(!dataset_matches_prefix(&xc_hyphen_slug_ds, "xc-tantivy"));

        // Legacy dataset doc without `slug` falls back to `{prefix}-` boundary.
        let legacy_ds = json!({
            "doc_kind": "dataset",
            "index_name": "ax-logs"
        });
        assert!(dataset_matches_prefix(&legacy_ds, "ax"));
        assert!(!dataset_matches_prefix(&legacy_ds, "ax2"));

        let corr = json!({
            "doc_kind": "correlation",
            "a_index": "xc-tantivy-code",
            "a_dataset": "code",
            "b_index": "xc-tantivy-symbols",
            "b_dataset": "symbols"
        });
        assert!(correlation_matches_prefix(&corr, "xc-tantivy"));
        assert!(!correlation_matches_prefix(&corr, "xc"));

        let file_with_prefix = json!({
            "doc_kind": "file",
            "prefix": "xc-tantivy",
            "path": "a.rs"
        });
        let file_with_scope_only = json!({
            "doc_kind": "file",
            CORPUS_SCOPE_FIELD: "xc-tantivy",
            "path": "b.rs"
        });
        assert!(file_matches_prefix(&file_with_prefix, "xc-tantivy"));
        assert!(!file_matches_prefix(&file_with_prefix, "xc"));
        assert!(file_matches_prefix(&file_with_scope_only, "xc-tantivy"));
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
            num_min: None,
            num_max: None,
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
            text_analyzer: None,
            scope: None,
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

#[cfg(test)]
mod prefix_reuse_tests {
    use super::*;

    fn run(prefix: &str, root: &str) -> Value {
        json!({"doc_kind": "run", "prefix": prefix, "root": root})
    }

    /// Same prefix, different folder: the earlier root is named, and the
    /// message says finalize retires `ds:<prefix>:<slug>` and that separately
    /// indexed repos collapse into `<prefix>-docs`.
    #[test]
    fn a_different_folder_under_the_same_prefix_is_named() {
        let runs = vec![
            run("ref", "/data/stockfish"),
            run("ref", "/data/stockfish"),
            run("ref-other", "/data/elsewhere"),
            json!({"doc_kind": "dataset", "prefix": "ref", "root": "/data/not-a-run"}),
        ];
        let foreign = foreign_prefix_roots(&runs, "ref", "/data/lucaschess");
        assert_eq!(foreign, vec!["/data/stockfish".to_string()]);
        let message = prefix_reuse_message("ref", "/data/lucaschess", &foreign, false);
        assert!(message.contains("this run: /data/lucaschess"), "{message}");
        assert!(message.contains("/data/stockfish"), "{message}");
        assert!(message.contains("ds:ref:<slug>"), "{message}");
        assert!(message.contains("ref-*"), "{message}");
        assert!(message.contains("ref-docs"), "{message}");
        assert!(message.contains("ds:ref:docs"), "{message}");
        assert!(message.contains("Nothing was written"), "{message}");
        assert!(message.contains("--replace-prefix"), "{message}");
        assert!(message.contains("Do not delete"), "{message}");
        let copy_at = message
            .find("--prefix lucaschess")
            .expect("the ready-to-copy command names a distinct prefix");
        let taken_at = message
            .find("--prefix ref already")
            .expect("the taken prefix is still named");
        let replace_at = message.find("--replace-prefix").expect("flag");
        assert!(
            copy_at < taken_at && copy_at < replace_at,
            "a distinct --prefix is the first remedy:\n{message}"
        );
        assert!(
            message.contains("xerj autoindex /data/lucaschess --prefix lucaschess"),
            "{message}"
        );
        assert!(
            !message.contains("/data/elsewhere"),
            "a sibling prefix is not this corpus: {message}"
        );
    }

    #[test]
    fn the_suggested_prefix_is_not_the_taken_one_and_the_path_is_quoted() {
        let message = prefix_reuse_message("ref", "/data/my folder", &["/data/old".into()], false);
        assert!(
            message.contains("xerj autoindex '/data/my folder' --prefix my-folder"),
            "{message}"
        );
        let digit = prefix_reuse_message("ref", "/data/2024", &["/data/old".into()], false);
        assert!(
            digit.contains("--prefix data-2024"),
            "a digit-leading folder name cannot be an index prefix, so the parent is used: {digit}"
        );
        assert!(
            !digit.contains("--prefix 2024\n") && !digit.contains("--prefix 2024 "),
            "{digit}"
        );
    }

    /// Re-running the same folder is not a collision, even when older run
    /// documents for that root are still in the catalog.
    #[test]
    fn the_same_folder_is_not_a_collision() {
        let runs = vec![run("ref", "/data/stockfish"), run("ref", "/data/stockfish")];
        assert!(foreign_prefix_roots(&runs, "ref", "/data/stockfish").is_empty());
    }

    /// After `--replace-prefix`, the new folder's run is the latest. Re-running
    /// that folder must not keep seeing the folder it replaced.
    #[test]
    fn the_latest_run_owns_the_prefix() {
        let runs = vec![
            json!({"doc_kind":"run","prefix":"ref","root":"/data/old","started":"2026-10-01T00:00:00Z"}),
            json!({"doc_kind":"run","prefix":"ref","root":"/data/new","started":"2026-10-02T00:00:00Z"}),
        ];
        assert!(foreign_prefix_roots(&runs, "ref", "/data/new").is_empty());
        assert_eq!(
            foreign_prefix_roots(&runs, "ref", "/data/old"),
            vec!["/data/new".to_string()]
        );
    }

    /// A run that never recorded its root cannot be proved to be this folder.
    #[test]
    fn a_run_with_no_root_is_a_collision() {
        let runs = vec![json!({"doc_kind": "run", "prefix": "ref"})];
        assert_eq!(
            foreign_prefix_roots(&runs, "ref", "/data/stockfish"),
            vec![UNRECORDED_PREFIX_ROOT.to_string()]
        );
    }

    #[test]
    fn the_same_folder_under_another_prefix_is_named() {
        let runs = vec![
            json!({"doc_kind": "run", "prefix": "ax", "root": "/data/proj", "started": "2026-01-01T00:00:00Z"}),
            json!({"doc_kind": "run", "prefix": "ax", "root": "/data/old", "started": "2026-02-01T00:00:00Z"}),
            json!({"doc_kind": "run", "prefix": "proj", "root": "/data/proj", "started": "2026-03-01T00:00:00Z"}),
        ];
        assert_eq!(
            other_prefixes_for_root(&runs, "/data/proj", "proj"),
            vec!["ax".to_string()]
        );
        let line = same_root_other_prefix_line("proj", &["ax".into()]);
        assert!(line.contains("under ax"), "{line}");
        // `ax` also holds /data/old here, so a delete hint would destroy a
        // folder this run never touched (#2114).
        assert!(!line.contains("DELETE"), "{line}");
        assert!(line.contains("--prefix ax"), "{line}");
        assert_eq!(
            other_prefixes_for_root(&runs, "/data/proj", "ax"),
            vec!["proj".to_string()]
        );
    }

    #[test]
    fn replace_prefix_says_the_run_continues() {
        let message = prefix_reuse_message("ref", "/data/new", &["/data/old".into()], true);
        assert!(message.contains("--replace-prefix is set"), "{message}");
        assert!(message.contains("this run continues"), "{message}");
        assert!(!message.contains("Nothing was written"), "{message}");
        assert!(
            !message.contains("Ready to copy"),
            "the opt-in warning is not the refusal's first remedy: {message}"
        );
    }
}
