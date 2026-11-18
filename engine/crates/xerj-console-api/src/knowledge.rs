//! The knowledge surface — one payload that answers the three questions a
//! person has the moment `xerj brain` / `xerj autoindex` finishes:
//!
//! ```text
//! GET /_xerj-console/api/v1/knowledge
//! ```
//!
//! 1. **How large is the corpus?** — whole-corpus totals (datasets, records,
//!    files, source bytes summed over the catalog's dataset docs) plus the
//!    live per-index doc counts and on-disk store bytes the engine reports.
//! 2. **Which data is there?** — every catalog dataset with its full field
//!    list (type, semantic flag, null%, cardinality, example values), the
//!    formats and time span autoindex measured, the other user indices the
//!    catalog does not describe, and the cross-dataset relations autoindex
//!    actually inferred (key overlaps and time alignments — nothing is
//!    invented: a corpus with no correlations gets an empty list, not a
//!    guess).
//! 3. **What can I do with this now?** — a capability strip computed from
//!    real facts on the node (catalog present, brains present), each entry
//!    carrying the real console route, CLI command, or HTTP endpoint it
//!    names. No capability is listed that this tree does not ship. The
//!    `semantic` entry (#1099) is the node's own posture — the elected
//!    field, its companion vector field, dims, similarity and the
//!    embedder's own label, read from the live schema — and it NEVER
//!    carries a vector count: no engine stats surface exposes one, and a
//!    number the node cannot back must not reach the UI.
//!
//! This is the server side of the Console's Corpus home (the landing view).
//! It exists as ONE endpoint rather than a client-side fan-out because the
//! answer must be assertable: `tests/knowledge_surface.rs` pins that the
//! payload carries the catalog's own numbers (doc counts, field specs,
//! coverage), so the page cannot drift from what `xerj autoindex map`
//! prints for agents.
//!
//! ## Sources, all in-process
//!
//! - the `autoindex-catalog` index (`doc_kind: dataset` / `correlation`
//!   docs), read through the engine's own index APIs — the same choice
//!   `xerj-api::ask_api` documents ("no `xerj-autoindex` crate dependency"),
//!   so the constant below *mirrors* `xerj_autoindex::catalog::CATALOG_INDEX`
//!   rather than importing it. Keep them in sync.
//! - `Index::stats()` for live doc counts and `dir_size_bytes`-equivalent
//!   walks of each index's `data_dir()` for on-disk store bytes (the same
//!   computation `/_cat/indices` and `/_disk_usage` perform).
//! - the reserved `.xerj-memory-*-edges` brains, listed under the same
//!   operator-tier role gate as [`crate::graph::brains`] (a session that
//!   may not read brains gets an empty list, never a leak).
//!
//! Honesty stance: a missing catalog is the ordinary empty state (`catalog:
//! false`, empty datasets), not an error; every count is either the
//! catalog's own recorded number or the engine's live one, labelled by
//! which; nothing on this surface is sample or illustrative data.

use axum::{extract::State, response::Response};
use serde_json::{json, Map, Value};

use crate::auth::sessions::AuthSession;
use crate::error::{ConsoleApiError, ConsoleResult};
use crate::graph;
use crate::response::ok;
use crate::state::ConsoleState;
use xerj_common::types::RESERVED_INDEX_PREFIX;
use xerj_engine::Engine;

/// Mirror of `xerj_autoindex::catalog::CATALOG_INDEX` (see module docs for
/// why this crate does not depend on `xerj-autoindex`).
const CATALOG_INDEX: &str = "autoindex-catalog";

/// Max dataset docs read from the catalog (one per dataset; the corpus
/// prefix keeps ids distinct, real nodes hold a handful).
const MAX_DATASETS: usize = 200;

/// Max correlation docs read from the catalog.
const MAX_RELATIONS: usize = 200;

/// Example values kept per field (the catalog itself stores up to a handful;
/// three is what a card usefully shows).
const MAX_EXAMPLES: usize = 3;

/// Characters kept from one example value — the same bound the terminal map
/// applies (`catalog.rs#render_map` takes 40; the card also shows a type and
/// coverage, so it gets a little less).
const MAX_EXAMPLE_CHARS: usize = 40;

/// One ES-shaped `_search` against one index, in-process (the `graph` and
/// `data_sources` pattern). `Ok(None)` = the index does not exist.
async fn search_docs(
    engine: &Engine,
    index: &str,
    body: &Value,
) -> ConsoleResult<Option<Vec<Value>>> {
    let idx = match engine.get_index(index) {
        Ok(i) => i,
        Err(_) => return Ok(None),
    };
    let req = xerj_query::parser::parse_request(body)
        .map_err(|e| ConsoleApiError::Internal(e.to_string()))?;
    let r = idx
        .search(&req)
        .await
        .map_err(|e| ConsoleApiError::Internal(e.to_string()))?;
    Ok(Some(r.hits.into_iter().map(|h| h.source).collect()))
}

/// Strip control characters (raw document values can carry NULs; they would
/// make the JSON payload read as binary). Same rule as the map renderer's
/// `clean`.
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

fn u64_of(v: Option<&Value>) -> u64 {
    v.and_then(Value::as_u64).unwrap_or(0)
}

fn f64_of(v: Option<&Value>) -> f64 {
    v.and_then(Value::as_f64).unwrap_or(0.0)
}

fn str_of(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn strings_of(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(clean)
            .filter(|s| !s.is_empty())
            .collect(),
        Some(Value::String(s)) => vec![clean(s)],
        _ => Vec::new(),
    }
}

/// `fields_json` on a dataset doc is a JSON *string* holding the array of
/// `xerj_autoindex::infer::FieldSpec` values (the frozen catalog mapping
/// declares it `text`). Parse it and project each spec down to what a card
/// renders: name, type, semantic flag, cardinality, null% and examples.
/// Unknown/missing keys degrade to honest defaults — a spec without a
/// `null_ratio` reads as "not measured" (`null`), never as 0%.
fn parse_fields(fields_json: Option<&Value>) -> Vec<Value> {
    let raw = match fields_json.and_then(Value::as_str) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let specs = match serde_json::from_str::<Value>(raw) {
        Ok(Value::Array(a)) => a,
        _ => return Vec::new(),
    };
    let mut out = Vec::with_capacity(specs.len());
    for spec in specs {
        let obj = match spec.as_object() {
            Some(o) => o,
            None => continue,
        };
        let get = |k: &str| obj.get(k);
        let Some(name) = str_of(get("name")) else {
            continue;
        };
        let es_type = str_of(get("es_type")).unwrap_or_else(|| "object".into());
        let date_enc = str_of(get("date_enc"));
        let ty = match date_enc {
            Some(enc) => format!("{es_type} ({enc})"),
            None => es_type,
        };
        // FieldSpec.semantic is Option<String> (the embedder's label) — the
        // card only needs "is this the semantic body field".
        let semantic = get("semantic").and_then(Value::as_str).is_some();
        let card = get("cardinality_est").and_then(Value::as_u64);
        let overflow = get("cardinality_overflow")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let examples = strings_of(get("examples"));
        out.push(json!({
            "name": name,
            "type": ty,
            "semantic": semantic,
            // null when the spec carried no estimate — rendered as "—" not 0.
            "cardinality": card,
            "cardinality_overflow": overflow,
            "null_ratio": get("null_ratio").and_then(Value::as_f64),
            "coverage": get("coverage").and_then(Value::as_f64),
            // The spec's measured average value length — the corpus's content
            // lives in its LONG fields, so the SPA ranks the fields a person
            // reads first by coverage × avg_len (the dominant fields), not by
            // type. Not rendered as a claim; a ranking input only.
            "avg_len": get("avg_len").and_then(Value::as_f64).unwrap_or(0.0),
            "examples": examples
                .iter()
                .take(MAX_EXAMPLES)
                .map(|e| e.chars().take(MAX_EXAMPLE_CHARS).collect::<String>())
                .collect::<Vec<_>>(),
        }));
    }
    out
}

/// `sample_queries_json` is an array of JSON strings (each a ready-to-send
/// query body). Parse each; unparseable entries are skipped, not guessed.
fn parse_sample_queries(v: Option<&Value>) -> Vec<Value> {
    let arr = match v.and_then(Value::as_array) {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .filter_map(|q| {
            let s = q.as_str()?;
            serde_json::from_str::<Value>(s).ok()
        })
        .take(12)
        .collect()
}

/// A dataset doc → the card payload. `live` carries the engine's current
/// numbers for the index when it exists (None = the catalog describes an
/// index this engine no longer holds — shown, labelled).
fn dataset_card(doc: &Value, live: Option<(u64, u64)>) -> Value {
    let g = |k: &str| doc.get(k);
    json!({
        "index": str_of(g("index_name")).unwrap_or_default(),
        "slug": str_of(g("slug")),
        "formats": strings_of(g("formats")),
        "records": u64_of(g("record_count")),
        "junk": u64_of(g("junk_records")),
        "files": u64_of(g("file_count")),
        "bytes": u64_of(g("bytes")),
        "live_docs": live.map(|(docs, _)| docs),
        "store_bytes": live.map(|(_, bytes)| bytes),
        "time_field": str_of(g("time_field")),
        "time_min": str_of(g("time_min")),
        "time_max": str_of(g("time_max")),
        "semantic_field": str_of(g("semantic_field")),
        "fields": parse_fields(g("fields_json")),
        "sample_queries": parse_sample_queries(g("sample_queries_json")),
        "notes": strings_of(g("notes")),
        "run_id": str_of(g("run_id")),
    })
}

/// A correlation doc → one relation row. Only the two `corr_kind`s autoindex
/// writes are understood; anything else is dropped (never reinterpreted).
fn relation_row(doc: &Value) -> Option<Value> {
    let g = |k: &str| doc.get(k);
    let base = json!({
        "a_dataset": str_of(g("a_dataset")),
        "a_index": str_of(g("a_index")).unwrap_or_default(),
        "a_field": str_of(g("a_field")).unwrap_or_default(),
        "b_dataset": str_of(g("b_dataset")),
        "b_index": str_of(g("b_index")).unwrap_or_default(),
        "b_field": str_of(g("b_field")).unwrap_or_default(),
    });
    let mut obj = base.as_object()?.clone();
    match g("corr_kind").and_then(Value::as_str) {
        Some("key_overlap") => {
            obj.insert("kind".into(), json!("key_overlap"));
            obj.insert(
                "grade".into(),
                json!(str_of(g("grade")).unwrap_or_default()),
            );
            obj.insert("overlap".into(), json!(u64_of(g("overlap"))));
            obj.insert("containment".into(), json!(f64_of(g("containment"))));
            obj.insert(
                "confirmed_values".into(),
                json!(g("confirmed_values").and_then(Value::as_u64)),
            );
            obj.insert(
                "tested_values".into(),
                json!(g("tested_values").and_then(Value::as_u64)),
            );
            obj.insert("examples".into(), json!(strings_of(g("examples"))));
        }
        Some("time_alignment") => {
            obj.insert("kind".into(), json!("time_alignment"));
            obj.insert("range_overlap".into(), json!(f64_of(g("range_overlap"))));
            obj.insert("shared_buckets".into(), json!(u64_of(g("shared_buckets"))));
            obj.insert(
                "pearson_r".into(),
                json!(g("pearson_r").and_then(Value::as_f64)),
            );
            obj.insert(
                "activity_correlated".into(),
                json!(g("activity_correlated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)),
            );
        }
        _ => return None,
    }
    Some(Value::Object(obj))
}

/// Recursive byte sum of a directory — the same computation `/_cat/indices`
/// and `/_disk_usage` perform for `store.size` (`es_compat::dir_size_bytes`,
/// mirrored here because it is private to `xerj-api`). Shared with the
/// data-sources facade so both surfaces report the same number.
pub(crate) fn dir_size_bytes(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let Ok(read) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in read.flatten() {
        let Ok(meta) = std::fs::metadata(entry.path()) else {
            continue;
        };
        if meta.is_dir() {
            total += dir_size_bytes(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}

/// Live `(doc_count, store_bytes)` for one index, when the engine holds it.
async fn live_stats(engine: &Engine, name: &str) -> Option<(u64, u64)> {
    let idx = engine.get_index(name).ok()?;
    let stats = idx.stats().await;
    Some((stats.doc_count, dir_size_bytes(idx.data_dir())))
}

/// The brains this node holds, under the same operator-tier gate as
/// `graph::brains` (see that module for the rule). Each row carries the
/// brain's link count — real edge docs only: the §2.5 meta doc
/// (`__xerj-brain-meta`) lives in the same index and is excluded, so the
/// number is what a person would count, not the index's raw doc count.
async fn brains_listing(engine: &Engine, may_read: bool) -> ConsoleResult<Vec<Value>> {
    if !may_read {
        return Ok(Vec::new());
    }
    let mut names: Vec<String> = engine
        .index_name_list()
        .into_iter()
        .filter(|n| {
            n.starts_with(RESERVED_INDEX_PREFIX)
                && n.ends_with("-edges")
                && n.len() > RESERVED_INDEX_PREFIX.len() + "-edges".len()
        })
        .map(|n| n[RESERVED_INDEX_PREFIX.len()..n.len() - "-edges".len()].to_string())
        .filter(|b| graph::validate_brain(b).is_ok())
        .collect();
    names.sort();
    let mut out = Vec::with_capacity(names.len());
    for brain in names {
        let edges = graph::edges_index(&brain);
        // Count edge documents, excluding the meta doc. `search_docs`
        // returns sources; a count query would be size-0, so count real
        // hits (an edge index holds one doc per link — bounded by the
        // graph contract, not by corpus size).
        let edge_docs = search_docs(
            engine,
            &edges,
            &json!({
                "query": { "bool": { "must_not": [{ "ids": { "values": [graph::BRAIN_META_ID] } }] } },
                "size": 10_000,
                "track_total_hits": true,
            }),
        )
        .await?;
        let links = edge_docs.map(|d| d.len() as u64).unwrap_or(0);
        let nodes_index = graph::resolve_nodes_index(engine, &brain, &edges).await;
        out.push(json!({
            "name": brain,
            "nodes_index": nodes_index,
            "links": links,
        }));
    }
    Ok(out)
}

/// The corpus's semantic posture, from the live engine — the facts behind
/// the `semantic` capability entry (#1099). Two honest shapes, nothing else:
///
/// - `Semantic` — the catalog elected a semantic field AND the index's live
///   schema carries its embedding config: dims/similarity/embedder exactly as
///   the query and ingest paths resolve them (`Index::semantic_field_facts`).
///   No vector count is included, because no engine surface exposes one — a
///   number the node cannot back must not reach the UI.
/// - `ElectedButMissing` — the catalog's last run elected a semantic field,
///   but this index has no embedding for it (it was rebuilt or mapped another
///   way): the entry then says plainly that there are no vectors on this node
///   for that field.
///
/// `None` (no catalog, or no dataset elected a semantic field) → no entry.
enum SemanticCapability {
    Semantic(xerj_engine::index::SemanticFieldFacts),
    ElectedButMissing { field: String, index: String },
}

/// Render the semantic capability's blurb from the facts above. Every phrase
/// is grounded: dims/similarity/embedder come from the live schema and the
/// embedder's own label; the lexical case says it is NOT neural and how to
/// get neural; the missing case says there is nothing to match.
fn semantic_blurb(sem: &SemanticCapability) -> String {
    match sem {
        SemanticCapability::Semantic(f) => {
            let dims = if f.dims_from_schema {
                format!("{}-D", f.dims)
            } else {
                format!("{}-D (the engine's built-in default)", f.dims)
            };
            if f.embedder_active {
                format!(
                    "Semantic and hybrid over `{}` → `{}`: {dims} · {} · {} — one vector per \
                     embedded document, embedded by that same backend at ingest.",
                    f.field, f.target_field, f.similarity, f.embedder
                )
            } else {
                format!(
                    "Semantic and hybrid over `{}` → `{}`: {dims} · {} · {} — one vector per \
                     embedded document. That embedder is NOT neural; start the node with \
                     `--embed-mode neural` (or `proxy` / `onnx-experimental`) for neural \
                     semantics.",
                    f.field, f.target_field, f.similarity, f.embedder
                )
            }
        }
        SemanticCapability::ElectedButMissing { field, index } => format!(
            "The catalog's last run elected `{field}` as the semantic field, but index \
             `{index}` carries no embedding for it on this node — there are no vectors to \
             match, and semantic/hybrid queries on that field are refused. Re-run \
             `xerj autoindex` over the source (or start with `--embed-mode neural`) to \
             give it one.",
        ),
    }
}

/// The capability strip. Every entry names a shipped surface — console route
/// (`href`), CLI command (`command`), or HTTP endpoint (`endpoint`) — and
/// availability is computed from facts on the node, not from a feature list.
fn capabilities(
    catalog: bool,
    brains: &[Value],
    semantic: Option<&SemanticCapability>,
) -> Vec<Value> {
    let mut caps = vec![json!({
        "id": "search",
        "title": "Search it",
        "blurb": "Seven query types — match, phrase, prefix, term, range, semantic, hybrid lexical+vector — with facets and a request preview.",
        "href": "#/discover",
        "kind": "console",
    })];
    // The semantic row is computed from the node's own schema and embedder
    // (#1099): dims, similarity and the embedder's label come from the engine,
    // and no vector count is ever claimed — the engine backs none.
    if let Some(sem) = semantic {
        caps.push(json!({
            "id": "semantic",
            "title": "Search it semantically",
            "blurb": semantic_blurb(sem),
            "href": "#/discover",
            "kind": "console",
        }));
    }
    caps.push(json!({
        "id": "read",
        "title": "Read the documents",
        "blurb": "Page through records with highlights and the per-record link list.",
        "href": "#/reader",
        "kind": "console",
    }));
    if let Some(brain) = brains.first() {
        let name = brain
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let links = brain.get("links").and_then(Value::as_u64).unwrap_or(0);
        caps.push(json!({
            "id": "graph",
            "title": "See the links between documents",
            "blurb": format!("Brain '{name}' holds {links} links over this corpus — the map, the evidence ledger, belief-time replay."),
            "href": format!("#/dashboards/second-brain?brain={name}"),
            "kind": "console",
        }));
    }
    if catalog {
        caps.push(json!({
            "id": "ask",
            "title": "Ask for a query, not a manual",
            "blurb": "POST /_ask — prompt in, validated query DSL out (it routes by this catalog; unresolved phrases are refused by name).",
            "endpoint": "POST /_ask",
            "kind": "http",
        }));
        caps.push(json!({
            "id": "map",
            "title": "Print the data map",
            "blurb": "The same field/coverage table this page shows, as markdown — for you or for an agent (MCP: xerj_map; `xerj mcp` exposes 20 tools).",
            "command": "xerj autoindex map",
            "kind": "cli",
        }));
    }
    caps.push(json!({
        "id": "decide",
        "title": "Decide with it",
        "blurb": "POST /_decide — the System One decide ladder (history vote by default; the local zero-shot head when the node arms it).",
        "endpoint": "POST /_decide",
        "kind": "http",
    }));
    caps.push(json!({
        "id": "watch",
        "title": "Keep it current",
        "blurb": "Stay resident and reindex what changes — this page follows on reload.",
        "command": "xerj autoindex <folder> --watch",
        "kind": "cli",
    }));
    caps.push(json!({
        "id": "share",
        "title": "Share one index",
        "blurb": "Read-only search over one index: a link, a passcode, an expiry.",
        "command": "xerj share <index>",
        "kind": "cli",
    }));
    caps
}

/// `GET /_xerj-console/api/v1/knowledge` — see the module docs.
pub async fn knowledge(
    State(state): State<ConsoleState>,
    sess: AuthSession,
) -> ConsoleResult<Response> {
    // 1. The catalog: dataset docs + correlation docs. A missing catalog
    //    index is the ordinary empty state, not an error.
    let dataset_docs = search_docs(
        &state.engine,
        CATALOG_INDEX,
        &json!({ "query": { "term": { "doc_kind": "dataset" } }, "size": MAX_DATASETS }),
    )
    .await?
    .unwrap_or_default();
    let corr_docs = search_docs(
        &state.engine,
        CATALOG_INDEX,
        &json!({ "query": { "term": { "doc_kind": "correlation" } }, "size": MAX_RELATIONS }),
    )
    .await?
    .unwrap_or_default();
    let catalog = !dataset_docs.is_empty();

    // 2. Dataset cards, largest first (the terminal map's order), each with
    //    the engine's live numbers for its index.
    let mut datasets: Vec<Value> = Vec::with_capacity(dataset_docs.len());
    for doc in &dataset_docs {
        if doc.get("doc_kind").and_then(Value::as_str) != Some("dataset") {
            continue;
        }
        let index = str_of(doc.get("index_name")).unwrap_or_default();
        let live = if index.is_empty() {
            None
        } else {
            live_stats(&state.engine, &index).await
        };
        datasets.push(dataset_card(doc, live));
    }
    datasets.sort_by(|a, b| {
        let rec = b["records"].as_u64().cmp(&a["records"].as_u64());
        rec.then_with(|| a["index"].as_str().cmp(&b["index"].as_str()))
    });

    // 3. Other user indices the catalog does not describe (an engine filled
    //    some other way must not read as "nothing indexed"). Same hidden-set
    //    rule as the data-sources facade.
    let catalog_indices: Vec<String> = datasets
        .iter()
        .filter_map(|d| d["index"].as_str().map(str::to_string))
        .collect();
    let mut others: Vec<Value> = Vec::new();
    let mut live_docs_total: u64 = 0;
    for name in state.engine.index_name_list() {
        if crate::data_sources::is_hidden_index(&name) || name == CATALOG_INDEX {
            continue;
        }
        let Some((docs, bytes)) = live_stats(&state.engine, &name).await else {
            continue;
        };
        live_docs_total += docs;
        if catalog_indices.contains(&name) {
            continue;
        }
        others.push(json!({ "index": name, "docs": docs, "store_bytes": bytes }));
    }
    others.sort_by(|a, b| a["index"].as_str().cmp(&b["index"].as_str()));

    // 4. Whole-corpus totals. `records`/`files`/`bytes` are the catalog's
    //    own last-run numbers (one run rewrites its dataset doc); `docs` is
    //    the engine's live count across every user index.
    let sum = |k: &str| {
        datasets
            .iter()
            .map(|d| d[k].as_u64().unwrap_or(0))
            .sum::<u64>()
    };
    let totals = json!({
        "datasets": datasets.len(),
        "records": sum("records"),
        "files": sum("files"),
        "bytes": sum("bytes"),
        "docs": live_docs_total,
        "relations": corr_docs.len(),
    });

    // 5. Relations — only what autoindex actually inferred.
    let mut relations: Vec<Value> = corr_docs.iter().filter_map(relation_row).collect();
    relations.sort_by(|a, b| {
        let rank = |v: &Value| match v["kind"].as_str() {
            Some("key_overlap") => v["overlap"].as_u64().unwrap_or(0),
            _ => 0,
        };
        rank(b).cmp(&rank(a))
    });

    // 6. The corpus's semantic posture — largest dataset first (the corpus's
    //    dominant one), from the LIVE index schema, never from a feature list.
    //    See `SemanticCapability` for the two honest shapes.
    let mut semantic: Option<SemanticCapability> = None;
    for d in &datasets {
        let Some(field) = str_of(d.get("semantic_field")) else {
            continue;
        };
        let index = d["index"].as_str().unwrap_or_default();
        let facts = match state.engine.get_index(index) {
            Ok(idx) => idx.semantic_field_facts(&field).await,
            Err(_) => None,
        };
        semantic = Some(match facts {
            Some(f) => SemanticCapability::Semantic(f),
            None => SemanticCapability::ElectedButMissing {
                field,
                index: index.to_string(),
            },
        });
        break;
    }

    // 7. Brains (operator tier) + the capability strip.
    let brains = brains_listing(&state.engine, graph::may_read_brains(&sess.user)).await?;
    let caps = capabilities(catalog, &brains, semantic.as_ref());

    let mut payload = Map::new();
    payload.insert("catalog".into(), json!(catalog));
    payload.insert("totals".into(), totals);
    payload.insert("datasets".into(), Value::Array(datasets));
    payload.insert("others".into(), Value::Array(others));
    payload.insert("relations".into(), Value::Array(relations));
    payload.insert("brains".into(), Value::Array(brains));
    payload.insert("capabilities".into(), Value::Array(caps));
    Ok(ok(Value::Object(payload), None))
}
