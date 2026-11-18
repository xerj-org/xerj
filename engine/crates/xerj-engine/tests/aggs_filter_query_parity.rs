//! #1745: `filter` / `filters` aggregations must count exactly what the same
//! clause counts as a query.
//!
//! Those aggs evaluate their clause per document through `doc_matches_filter`,
//! a separate matcher over `_source` that had no mapping or analyzer: `match`
//! was a lowercase `contains` per word (so `dog` matched "dogs"), a multi-word
//! `match` required EVERY word (ES defaults to OR), and `match_phrase` never
//! checked adjacency. The same five clauses counted as
//! `m=3, t=0, or=0, phr=1` in an agg while `_count` answered
//! `2, 0, 2, 0` (and ES 8.13 answers `2, 2, 2, 0`).
//!
//! The aggregation now runs with the index schema in scope, so
//! `run_filter`/`run_filters` evaluate the parsed clause with
//! `doc_matches_query_typed` — the matcher `_search`/`_count` use. This test
//! pins parity clause-by-clause (the issue's actual invariant) plus the
//! ES-pinned counts for the clauses where XERJ's query path already matches
//! ES.
//!
//! `term` on a `text` field is deliberately asserted for PARITY only:
//! XERJ's query path answers it by whole value (0 here) — a pre-existing
//! contract this PR does not change — so the agg must join it rather than
//! keep a second opinion. ES answers 2 by matching the analyzed token; that
//! difference belongs to the query path, not to this aggregation fix.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::{Engine, Index};
use xerj_query::parse_request;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

fn books_schema() -> Schema {
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("title", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("kw", FieldType::Keyword));
    schema
}

/// The issue's three documents, plus a keyword column so the exact-field
/// control surface is covered too.
async fn seed(engine: &Engine) -> std::sync::Arc<Index> {
    engine.create_index("books", books_schema()).unwrap();
    let idx = engine.get_index("books").unwrap();
    for (id, title, kw) in [
        ("1", "lazy dog sleeps", "alpha"),
        ("2", "Quick dogs run", "beta"),
        ("3", "the fox and the dog", "alpha"),
    ] {
        idx.index_document(Some(id.into()), json!({"title": title, "kw": kw}))
            .await
            .unwrap();
    }
    idx.refresh().await.unwrap();
    idx
}

async fn count(idx: &Index, clause: &Value) -> u64 {
    let req = parse_request(&json!({ "query": clause, "size": 0 })).expect("parse_request");
    idx.search(&req).await.unwrap().total.value
}

async fn run_aggs(idx: &Index, aggs: Value) -> Value {
    let req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": aggs,
    }))
    .expect("parse_request");
    idx.search(&req)
        .await
        .unwrap()
        .aggs
        .expect("aggs in the response")
}

/// The issue's clauses, plus exact-field controls.
fn clauses() -> Vec<(&'static str, Value)> {
    vec![
        ("match_dog", json!({"match": {"title": "dog"}})),
        ("term_dog", json!({"term": {"title": "dog"}})),
        ("match_or", json!({"match": {"title": "quick fox"}})),
        (
            "phrase_adjacent",
            json!({"match_phrase": {"title": "fox and"}}),
        ),
        ("phrase_gap", json!({"match_phrase": {"title": "fox dog"}})),
        ("term_kw", json!({"term": {"kw": "alpha"}})),
        (
            "bool_kw",
            json!({"bool": {"should": [{"term": {"kw": "beta"}}], "minimum_should_match": 1}}),
        ),
    ]
}

#[tokio::test]
async fn filter_and_filters_aggs_count_what_the_same_clause_counts_as_a_query() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine).await;

    for (name, clause) in clauses() {
        let expected = count(&idx, &clause).await;
        let body = run_aggs(
            &idx,
            json!({
                "one": { "filter": clause },
                "multi": { "filters": { "filters": { name: clause } } },
            }),
        )
        .await;
        assert_eq!(
            body["one"]["doc_count"].as_u64(),
            Some(expected),
            "filter agg `{name}` must equal _count({clause}): {body}"
        );
        assert_eq!(
            body["multi"]["buckets"][name]["doc_count"].as_u64(),
            Some(expected),
            "filters agg `{name}` must equal _count({clause}): {body}"
        );
    }

    // ES-pinned counts for the clauses the query path itself already answers
    // like ES 8.13 (`_count` was 2 / 2 / 0 before this change; only the agg
    // disagreed).
    let body = run_aggs(
        &idx,
        json!({
            "m":   { "filter": { "match": { "title": "dog" } } },
            "t":   { "filter": { "term": { "title": "dog" } } },
            "or":  { "filter": { "match": { "title": "quick fox" } } },
            "phr": { "filter": { "match_phrase": { "title": "fox dog" } } },
            "ok":  { "filter": { "match_phrase": { "title": "fox and" } } },
        }),
    )
    .await;
    assert_eq!(body["m"]["doc_count"], json!(2), "match(dog) != ES: {body}");
    assert_eq!(
        body["or"]["doc_count"],
        json!(2),
        "match(quick fox) != ES: {body}"
    );
    assert_eq!(
        body["phr"]["doc_count"],
        json!(0),
        "match_phrase(fox dog) != ES: {body}"
    );
    assert_eq!(
        body["ok"]["doc_count"],
        json!(1),
        "match_phrase(fox and) != ES: {body}"
    );
    // Whole-value `term` on text: the query path's answer, not ES's (see the
    // module comment). The point is that the agg no longer has its own answer.
    assert_eq!(
        body["t"]["doc_count"],
        json!(0),
        "term(dog) must match _count: {body}"
    );
}

/// The schema scope must survive the bucket boundary: a `filter` nested under
/// a `terms` bucket runs through `run_aggs_in_bucket`.
#[tokio::test]
async fn filter_aggs_inside_bucket_subaggs_use_the_same_matcher() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine).await;

    let body = run_aggs(
        &idx,
        json!({
            "by_kw": {
                "terms": { "field": "kw" },
                "aggs": {
                    "dog": { "filter": { "match": { "title": "dog" } } },
                    "fox": { "filter": { "match": { "title": "quick fox" } } },
                }
            }
        }),
    )
    .await;

    let mut by_key = std::collections::HashMap::new();
    for b in body["by_kw"]["buckets"].as_array().expect("buckets") {
        by_key.insert(b["key"].as_str().unwrap().to_string(), b.clone());
    }

    // alpha: docs 1 ("lazy dog sleeps") and 3 ("the fox and the dog").
    // beta:  doc 2 ("Quick dogs run") — token `dogs`, not `dog`.
    assert_eq!(by_key["alpha"]["dog"]["doc_count"], json!(2));
    assert_eq!(by_key["alpha"]["fox"]["doc_count"], json!(1));
    assert_eq!(by_key["beta"]["dog"]["doc_count"], json!(0));
    assert_eq!(by_key["beta"]["fox"]["doc_count"], json!(1));
}
