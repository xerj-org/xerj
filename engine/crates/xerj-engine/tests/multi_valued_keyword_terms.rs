//! Regression tests for issue #332: a multi-valued keyword field was
//! flattened into ONE FTS token.
//!
//! `{"tags": ["red", "blue"]}` on a `keyword` field used to be joined into the
//! single string `"red blue"` before the FTS layer saw it
//! (`memtable::extract_text_value` / `index::extract_field_text`, both doing
//! `arr.join(" ")`).  The keyword analyzer emits its whole input as one token,
//! so the segment carried the term `"red blue"` and neither `"red"` nor
//! `"blue"` existed as a posting.  Every clause that projects to a whole-value
//! `FtsQuery::Term` therefore missed the document once it was flushed —
//! visible in the live engine as `multi_match: {"query": "red"}` returning 0
//! hits while `multi_match: {"query": "red blue"}` returned 1, the exact
//! inverse of Elasticsearch.
//!
//! Every test asserts the hit set is the same BEFORE and AFTER `flush()`:
//! this class of defect is flush-dependent, and a post-flush-only assertion
//! would pass on a build that had simply stopped indexing the field.

use std::collections::BTreeSet;

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

fn req(q: Value) -> xerj_query::ast::SearchRequest {
    parse_request(&json!({ "query": q, "size": 50 })).expect("parse_request")
}

async fn ids(idx: &Index, q: &Value) -> BTreeSet<String> {
    idx.search(&req(q.clone()))
        .await
        .unwrap()
        .hits
        .iter()
        .map(|h| h.id.clone())
        .collect()
}

fn expect(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

/// Two docs, one index:
/// * `1` — multi-valued keyword `tags: ["red","blue"]`, multi-valued text
///   `notes: ["alpha bravo","charlie delta"]`
/// * `2` — the single-valued control, `tags: "red"`, `notes: "alpha bravo"`
async fn seed(engine: &Engine, name: &str) -> std::sync::Arc<Index> {
    let mut schema = Schema::empty();
    schema.fields.push(FieldConfig::new("body", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("notes", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("tags", FieldType::Keyword));
    engine.create_index(name, schema).unwrap();
    let idx = engine.get_index(name).unwrap();
    idx.index_document(
        Some("1".into()),
        json!({
            "body": "hello",
            "tags": ["red", "blue"],
            "notes": ["alpha bravo", "charlie delta"]
        }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("2".into()),
        json!({"body": "hello", "tags": "red", "notes": "alpha bravo"}),
    )
    .await
    .unwrap();
    idx
}

/// Run every case pre-flush and post-flush and require the same hit set.
async fn assert_flush_parity(idx: &std::sync::Arc<Index>, cases: &[(Value, &[&str], &str)]) {
    for (q, exp, label) in cases {
        assert_eq!(
            ids(idx, q).await,
            expect(exp),
            "{label}: PRE-flush (memtable) hit set wrong for {q}"
        );
    }
    idx.flush().await.unwrap();
    for (q, exp, label) in cases {
        assert_eq!(
            ids(idx, q).await,
            expect(exp),
            "{label}: POST-flush (segment) hit set wrong for {q}"
        );
    }
}

#[tokio::test]
async fn term_match_multi_match_see_every_keyword_array_element() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mvk_terms").await;

    assert_flush_parity(
        &idx,
        &[
            // The headline: an exact `term` for one element of the array.
            (json!({"term": {"tags": "red"}}), &["1", "2"], "term red"),
            (json!({"term": {"tags": "blue"}}), &["1"], "term blue"),
            // `match` on a keyword field lowers to the same whole-value term.
            (json!({"match": {"tags": "red"}}), &["1", "2"], "match red"),
            (json!({"match": {"tags": "blue"}}), &["1"], "match blue"),
            // `multi_match` — the clause that visibly missed doc 1 on `main`.
            (
                json!({"multi_match": {"query": "red", "fields": ["tags", "body"]}}),
                &["1", "2"],
                "multi_match red",
            ),
            (
                json!({"multi_match": {"query": "blue", "fields": ["tags", "body"]}}),
                &["1"],
                "multi_match blue",
            ),
            // `terms` takes the stored-scan route and always agreed with ES;
            // keep it as the in-binary control that the two routes now match.
            (
                json!({"terms": {"tags": ["blue"]}}),
                &["1"],
                "terms control",
            ),
            (
                json!({"bool": {"must": [{"match": {"body": "hello"}}],
                                "filter": [{"terms": {"tags": ["red"]}}]}}),
                &["1", "2"],
                "bool must+filter control",
            ),
        ],
    )
    .await;
}

/// The joined string must NOT survive as a term. Pre-fix `"red blue"` was
/// the ONLY term the segment held for doc 1, so this query matched it —
/// exactly backwards from Elasticsearch, where no keyword value equals
/// `"red blue"`.
#[tokio::test]
async fn joined_array_is_not_a_term() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mvk_joined").await;

    assert_flush_parity(
        &idx,
        &[
            (json!({"term": {"tags": "red blue"}}), &[], "term joined"),
            (
                json!({"multi_match": {"query": "red blue", "fields": ["tags"]}}),
                &[],
                "multi_match joined",
            ),
        ],
    )
    .await;
}

/// A phrase must not span two elements of an array — Lucene separates them by
/// `position_increment_gap` (100), and so does the segment writer now.
/// Pre-fix the memtable said 0 hits and the flushed segment said 1: the
/// joined string put `bravo` and `charlie` at adjacent positions.
#[tokio::test]
async fn phrase_does_not_span_array_elements() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mvk_phrase").await;

    assert_flush_parity(
        &idx,
        &[
            (
                json!({"match_phrase": {"notes": "bravo charlie"}}),
                &[],
                "match_phrase across the boundary",
            ),
            (
                json!({"multi_match": {"query": "bravo charlie",
                                       "fields": ["notes"], "type": "phrase"}}),
                &[],
                "multi_match phrase across the boundary",
            ),
            // ... but a phrase WITHIN one element still matches, in both docs
            // for the first element and only doc 1 for the second.
            (
                json!({"match_phrase": {"notes": "alpha bravo"}}),
                &["1", "2"],
                "match_phrase inside element 0",
            ),
            (
                json!({"multi_match": {"query": "charlie delta",
                                       "fields": ["notes"], "type": "phrase"}}),
                &["1"],
                "multi_match phrase inside element 1",
            ),
        ],
    )
    .await;
}

/// A `terms` aggregation over a multi-valued keyword field buckets EVERY
/// value, and the answer does not change at flush.
#[tokio::test]
async fn terms_agg_buckets_every_array_element() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mvk_agg").await;

    let body = json!({
        "query": {"match_all": {}},
        "size": 0,
        "aggs": {"t": {"terms": {"field": "tags"}}}
    });
    let expected = json!([
        {"key": "red", "doc_count": 2},
        {"key": "blue", "doc_count": 1}
    ]);

    for state in ["PRE-flush", "POST-flush"] {
        if state == "POST-flush" {
            idx.flush().await.unwrap();
        }
        let res = idx.search(&parse_request(&body).unwrap()).await.unwrap();
        let buckets = res.aggs.as_ref().and_then(|a| a["t"]["buckets"].as_array())
            .cloned()
            .unwrap_or_default();
        let trimmed: Vec<Value> = buckets
            .iter()
            .map(|b| json!({"key": b["key"], "doc_count": b["doc_count"]}))
            .collect();
        assert_eq!(
            Value::Array(trimmed),
            expected,
            "{state}: terms agg over a multi-valued keyword field"
        );
    }
}

/// `_source` is stored verbatim and must come back as the array it went in
/// as — the fix touches the inverted index, never the stored document.
#[tokio::test]
async fn source_round_trips_unchanged() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mvk_source").await;

    for state in ["PRE-flush", "POST-flush"] {
        if state == "POST-flush" {
            idx.flush().await.unwrap();
        }
        let res = idx
            .search(&req(json!({"term": {"tags": "blue"}})))
            .await
            .unwrap();
        assert_eq!(res.hits.len(), 1, "{state}: expected exactly doc 1");
        assert_eq!(
            res.hits[0].source["tags"],
            json!(["red", "blue"]),
            "{state}: _source must keep the original array"
        );
    }
}
