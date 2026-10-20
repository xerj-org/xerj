//! Regression tests: `term`/`terms` on an analyzed `text` field whose mapping
//! asks for doc-values (the ES `mapper.text.doc_values` shape) must answer the
//! same question before and after the flush.
//!
//! The memtable scan matches the analyzed token stream
//! (`doc_matches_query_typed`'s `tokens_contain`): `terms {text: ["brown"]}`
//! over "quick brown fox" matches. The flushed segment used to read the
//! doc-values column — which stores the RAW "quick brown fox" — through the
//! term/terms prefilters and the doc-values count shortcut, so the same query
//! returned nothing after `_refresh`, and the count agreed with the wrong
//! answer. A column-derived set is not a superset of token matching, so those
//! paths now decline for analyzed text fields (`field_is_analyzed_text`) and
//! the stored scan answers on both sides.

use std::collections::BTreeSet;
use std::sync::Arc;

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

async fn search(idx: &Index, q: &Value) -> (u64, BTreeSet<String>) {
    let req = parse_request(&json!({ "query": q, "size": 50 })).expect("parse_request");
    let r = idx.search(&req).await.unwrap();
    (r.total.value, r.hits.iter().map(|h| h.id.clone()).collect())
}

async fn count_total(idx: &Index, q: &Value) -> u64 {
    let req = parse_request(&json!({ "query": q, "size": 0, "from": 0 })).expect("parse_request");
    idx.search(&req).await.unwrap().total.value
}

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

/// The shape the ES `mapper.text.doc_values` preview maps: an analyzed field
/// that also carries doc-values.
fn doc_values_text_schema() -> Schema {
    let mut schema = Schema::empty();
    let mut text = FieldConfig::new("text", FieldType::Text);
    text.options.doc_values = true;
    schema.fields.push(text);
    schema
}

#[tokio::test]
async fn terms_on_text_with_doc_values_is_flush_invariant() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine
        .create_index("dvt", doc_values_text_schema())
        .unwrap();
    let idx: Arc<Index> = engine.get_index("dvt").unwrap();
    for (id, t) in [
        ("d1", "quick brown fox"),
        ("d2", "quick brown fox"),
        ("d3", "lazy dog"),
    ] {
        idx.index_document(Some(id.into()), json!({"text": t}))
            .await
            .unwrap();
    }

    // The token query the memtable answers.
    let token_q = json!({"terms": {"text": ["brown"]}});
    let (pre_total, pre_hits) = search(&idx, &token_q).await;
    assert_eq!(
        pre_hits,
        set(&["d1", "d2"]),
        "pre-flush (memtable) token match"
    );
    assert_eq!(pre_total, 2);
    assert_eq!(count_total(&idx, &token_q).await, 2, "count before flush");

    // The whole-value query both sides agree on. NOTE: this pins flush
    // invariance of XERJ's current answer, not ES semantics — ES's term-level
    // `terms` on indexed text would match no token "quick brown fox" (0 hits).
    let whole_q = json!({"terms": {"text": ["quick brown fox"]}});
    assert_eq!(search(&idx, &whole_q).await.1, set(&["d1", "d2"]));

    idx.flush().await.unwrap();

    // The segment really carries the doc-values column this test is about.
    let seg_dir = dir.path().join("dvt").join("segments");
    let dv_files = std::fs::read_dir(&seg_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "dv"))
        .count();
    assert!(
        dv_files >= 1,
        "the mapping must have produced a .dv sidecar for the field"
    );

    let (post_total, post_hits) = search(&idx, &token_q).await;
    assert_eq!(
        post_hits,
        set(&["d1", "d2"]),
        "token match must survive the flush"
    );
    assert_eq!(post_total, pre_total, "hit total changed at flush");
    assert_eq!(
        count_total(&idx, &token_q).await,
        2,
        "count must not change at flush"
    );
    assert_eq!(
        search(&idx, &whole_q).await,
        (2, set(&["d1", "d2"])),
        "whole-value match must stay"
    );

    // The conjunction lane (bool prefilter) and the `term` shape, which
    // answers by whole value on both sides by design.
    let filtered = json!({"bool": {
        "must": [{"terms": {"text": ["brown"]}}],
        "filter": [{"term": {"text": "quick brown fox"}}],
    }});
    assert_eq!(search(&idx, &filtered).await.1, set(&["d1", "d2"]));
    assert_eq!(count_total(&idx, &filtered).await, 2);
    let term_q = json!({"term": {"text": "brown"}});
    assert_eq!(
        search(&idx, &term_q).await,
        (0, set(&[])),
        "term on text stays whole-value"
    );
}

/// `"index": false` + doc-values text (the ES `mapper.text.doc_values` preview
/// pinned by `aggregations/terms_text_docvalues.yml`): there are no postings,
/// so ES answers `term`/`terms` from the doc-values column — WHOLE values only.
/// A token-only value must not match once the segment answers.
#[tokio::test]
async fn terms_on_unindexed_text_with_doc_values_stays_whole_value() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let mut schema = Schema::empty();
    let mut text = FieldConfig::new("text", FieldType::Text);
    text.options.doc_values = true;
    text.options.indexed = false;
    schema.fields.push(text);
    engine.create_index("dvn", schema).unwrap();
    let idx: Arc<Index> = engine.get_index("dvn").unwrap();
    for (id, value) in [
        ("d1", json!("quick brown fox")),
        ("d2", json!("lazy dog")),
        ("d3", json!("fox")),
        ("d4", json!("Fox")),
        ("d5", json!(["fox", "lazy dog"])),
    ] {
        idx.index_document(Some(id.into()), json!({ "text": value }))
            .await
            .unwrap();
    }

    let cases = [
        (
            "match token absent",
            json!({"match": {"text": "quick"}}),
            set(&[]),
        ),
        (
            "match whole token",
            json!({"match": {"text": "fox"}}),
            set(&["d3", "d5"]),
        ),
        (
            "match default or",
            json!({"match": {"text": "quick brown fox"}}),
            set(&["d3", "d5"]),
        ),
        (
            "match and",
            json!({"match": {"text": {"query": "quick brown fox", "operator": "and"}}}),
            set(&[]),
        ),
        (
            "match keyword",
            json!({"match": {"text": {"query": "quick brown fox", "analyzer": "keyword"}}}),
            set(&["d1"]),
        ),
        (
            "match uppercase",
            json!({"match": {"text": "FOX"}}),
            set(&["d3", "d5"]),
        ),
        (
            "match whitespace",
            json!({"match": {"text": {"query": "Fox", "analyzer": "whitespace"}}}),
            set(&["d4"]),
        ),
        (
            "match msm",
            json!({"match": {"text": {"query": "fox lazy", "minimum_should_match": 2}}}),
            set(&[]),
        ),
        (
            "terms token absent",
            json!({"terms": {"text": ["quick"]}}),
            set(&[]),
        ),
        (
            "terms whole",
            json!({"terms": {"text": ["quick brown fox"]}}),
            set(&["d1"]),
        ),
        (
            "term whole",
            json!({"term": {"text": "fox"}}),
            set(&["d3", "d5"]),
        ),
        (
            "bool filter",
            json!({"bool": {"filter": [{"terms": {"text": ["quick"]}}]}}),
            set(&[]),
        ),
        (
            "bool must",
            json!({"bool": {"must": [{"match": {"text": "fox"}}]}}),
            set(&["d3", "d5"]),
        ),
        (
            "constant score",
            json!({"constant_score": {"filter": {"match": {"text": "fox"}}}}),
            set(&["d3", "d5"]),
        ),
        (
            "dis max",
            json!({"dis_max": {"queries": [{"match": {"text": "fox"}}]}}),
            set(&["d3", "d5"]),
        ),
    ];

    for phase in ["before", "after"] {
        for (name, query, expected) in &cases {
            let (total, hits) = search(&idx, query).await;
            assert_eq!(&hits, expected, "{phase} flush: {name}");
            assert_eq!(total, expected.len() as u64, "{phase} flush total: {name}");
            assert_eq!(
                count_total(&idx, query).await,
                expected.len() as u64,
                "{phase} flush size:0 total: {name}"
            );
        }
        if phase == "before" {
            idx.flush().await.unwrap();
        }
    }
}
