//! The catalog index (`autoindex-catalog`) — deliberately OUTSIDE the ax-*
//! wildcard so data-wide searches never hit metadata. Its dataset docs ARE
//! the agent-facing data map; `xerj autoindex map` renders them.

use crate::correlate::KeyCorr;
use crate::state::PlanDataset;
use serde_json::{json, Value};

pub const CATALOG_INDEX: &str = "autoindex-catalog";

pub fn catalog_mapping() -> Value {
    json!({
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
    })
}

pub const GOTCHAS: &[&str] = &[
    "hybrid search: use {\"query\":{\"hybrid\":{\"queries\":[…]}}} ONLY — retriever.rrf is a silent stub and rank.rrf is ignored on this engine",
    "the semantic embedder is hash-bucket LEXICAL (384-dim): this is hybrid lexical+vector retrieval, NOT neural semantic understanding",
    "semantic queries ignore _source filtering and return the ~8KB *_vector field in _source — strip client-side",
    "exact filters use TOP-LEVEL keyword fields (term on .keyword subfields returns 0 hits on this engine)",
    "all dates are normalized to RFC3339 UTC millis; mappings use strict_date_optional_time||epoch_millis",
    "query all data with the wildcard index pattern (e.g. ax-*) or comma lists — never multi-index aliases (they resolve to the first index only)",
    "documents were indexed with refresh at the end of the run; new writes need _refresh before they are searchable",
];

pub struct DatasetDocInput<'a> {
    pub pd: &'a PlanDataset,
    pub record_count: u64,
    pub junk_records: u64,
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
    let id = format!("ds:{}", inp.pd.slug);
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

#[allow(clippy::too_many_arguments)] // 1:1 with the file-status doc's fields
pub fn file_doc(
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
        format!("file:{file_key}"),
        json!({
            "doc_kind": "file",
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
        out.push(json!({
            "class": "full_text",
            "title": format!("Full-text (BM25) match on {}", f.name),
            "request": format!("POST /{}/_search", pd.index),
            "body": {"query": {"match": {(f.name.clone()): word.clone()}}, "size": 3}
        }));
        // 3. hybrid — only when a semantic_text field exists
        if let Some(sf) = &pd.semantic_field {
            out.push(json!({
                "class": "hybrid_lexical_vector",
                "title": format!("Hybrid lexical+vector (RRF) on {sf} — hash-bucket embedder, lexical not neural"),
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
    junk_total: u64,
) -> String {
    let mut s = String::new();
    s.push_str("# Data map (xerj autoindex)\n\n");
    if let Some(r) = run {
        let g = |k: &str| r.get(k).map(pretty_val).unwrap_or_default();
        s.push_str(&format!(
            "run `{}` — root `{}` — {} files, {} records indexed, {} junk records, wall {}s\n\n",
            g("run_id"),
            g("root"),
            g("files_total"),
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
                "semantic body field: `{}` (hybrid lexical+vector — hash-bucket embedder, lexical, not neural)\n\n",
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
