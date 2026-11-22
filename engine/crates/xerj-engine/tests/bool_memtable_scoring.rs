//! Issue #3349: a `bool` of scoring clauses scored every buffered (memtable)
//! hit with the stored-doc scan's IDF-less scorer, while the same query
//! scored BM25 once `_flush` moved the docs into a segment, so the ranking
//! changed at flush. The shapes: a `bool` with several `should` clauses
//! (the `xerj search` / `xerj eval` body), `query_string` with `fields`, and
//! `simple_query_string` (both parse to `bool` trees).
//!
//! The fix plans the tree (`mem_score_plan`): every scoring leaf goes through
//! the memtable BM25 path a standalone `match` takes, and `bool` /
//! `dis_max` / `constant_score` combine per doc the way the segment's
//! `FtsQuery` executor combines the projection. These tests pin, per query
//! shape, that the ranked `(id, score)` list is the same before the flush,
//! with the index split across a segment and the memtable, and after
//! everything is flushed.
//!
//! Elasticsearch is referenced for wire semantics only; no ES code is here.

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

fn schema() -> Schema {
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("title", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("tag", FieldType::Keyword));
    schema
}

/// Every doc carries every field, so per-field doc counts equal the doc
/// count and the memtable-only and segment statistics agree exactly.
fn docs() -> Vec<(&'static str, Value)> {
    vec![
        (
            "a",
            json!({"title": "cats cats cats dogs", "body": "a short note", "tag": "pet"}),
        ),
        (
            "b",
            json!({"title": "cats birds fish lizards snakes and more", "body": "cats again", "tag": "zoo"}),
        ),
        (
            "c",
            json!({"title": "nothing relevant here", "body": "dogs dogs and one cat named cats", "tag": "pet"}),
        ),
        (
            "d",
            json!({"title": "dogs", "body": "dogs and birds", "tag": "zoo"}),
        ),
        (
            "e",
            json!({"title": "fish tank cleaning guide for beginners", "body": "fish fish fish", "tag": "pet"}),
        ),
        (
            "f",
            json!({"title": "cats and dogs living together", "body": "mass hysteria cats dogs", "tag": "pet"}),
        ),
        (
            "g",
            json!({"title": "birds of prey", "body": "hawks hunt fish and small cats", "tag": "zoo"}),
        ),
    ]
}

async fn ranked(idx: &Index, q: &Value) -> Vec<(String, f32)> {
    let req = parse_request(&json!({ "query": q, "size": 50 })).expect("parse_request");
    idx.search(&req)
        .await
        .unwrap()
        .hits
        .into_iter()
        .map(|h| (h.id, h.score))
        .collect()
}

/// The `xerj search` body for a prose index mapping `title` and `body`
/// (`xerj_common::code_query::build_search_query`): a `most_fields`
/// `multi_match`, an exact phrase, and the two `constant_score` coverage
/// clauses (#2327).
fn xerj_search_body(q: &str) -> Value {
    let fields = json!(["title", "body"]);
    let per_term: Vec<Value> = q
        .split_whitespace()
        .map(|t| json!({"multi_match": {"query": t, "fields": fields, "type": "most_fields"}}))
        .collect();
    let need = per_term.len().div_ceil(2);
    json!({"bool": {"should": [
        {"multi_match": {"query": q, "fields": fields, "type": "most_fields"}},
        {"match_phrase": {"body": {"query": q, "boost": 4.0}}},
        {"constant_score": {"filter": {"bool": {"should": per_term, "minimum_should_match": need}}, "boost": 1000.0}},
        {"constant_score": {"filter": {"multi_match": {"query": q, "fields": fields, "type": "most_fields", "operator": "and"}}, "boost": 1000.0}}
    ]}})
}

fn queries() -> Vec<(&'static str, Value)> {
    vec![
        // The issue's three rows.
        (
            "bool_should_mm_phrase",
            json!({"bool": {"should": [
                {"multi_match": {"query": "cats dogs", "fields": ["title", "body"], "type": "most_fields"}},
                {"match_phrase": {"body": {"query": "cats dogs", "boost": 8}}}
            ]}}),
        ),
        (
            "query_string_fields",
            json!({"query_string": {"query": "cats dogs", "fields": ["title", "body"]}}),
        ),
        (
            "simple_query_string_fields",
            json!({"simple_query_string": {"query": "cats dogs", "fields": ["title", "body"]}}),
        ),
        // The full `xerj search` body, with and without coverage clauses
        // matching.
        ("xerj_search_body", xerj_search_body("cats dogs birds")),
        ("xerj_search_body_fish", xerj_search_body("fish cats hunt")),
        // Combinators and leaf variants.
        (
            "bool_must_should",
            json!({"bool": {
                "must": [{"match": {"body": "dogs"}}],
                "should": [{"match": {"title": "cats"}}, {"match": {"title": "dogs"}}]
            }}),
        ),
        (
            "bool_should_msm",
            json!({"bool": {
                "should": [{"match": {"title": "cats"}}, {"match": {"title": "dogs"}}, {"match": {"body": "birds"}}],
                "minimum_should_match": 2
            }}),
        ),
        (
            "bool_match_and_msm",
            json!({"bool": {"should": [
                {"match": {"body": {"query": "dogs birds", "operator": "and"}}},
                {"match": {"title": {"query": "cats dogs fish", "minimum_should_match": 2}}}
            ]}}),
        ),
        (
            "bool_dis_max_constant",
            json!({"bool": {"should": [
                {"dis_max": {"queries": [{"match": {"title": "fish"}}, {"match": {"body": "fish"}}], "tie_breaker": 0.3}},
                {"constant_score": {"filter": {"match": {"body": "cats"}}, "boost": 2.5}}
            ]}}),
        ),
        (
            "bool_nested",
            json!({"bool": {"should": [
                {"bool": {"must": [{"match": {"title": "cats"}}], "should": [{"match": {"body": "cats"}}]}},
                {"multi_match": {"query": "birds fish", "fields": ["title^2", "body"]}}
            ]}}),
        ),
        (
            "bool_should_filter_must_not",
            json!({"bool": {
                "must": [{"multi_match": {"query": "cats dogs", "fields": ["title", "body"], "type": "most_fields"}}],
                "should": [{"match_phrase": {"title": "cats dogs"}}],
                "filter": [{"term": {"tag": "pet"}}],
                "must_not": [{"match": {"body": "hysteria"}}]
            }}),
        ),
        // A repeated query token is one term: the memtable used to count its
        // doc_freq once per occurrence, so its IDF fell below the segment's.
        (
            "match_repeated_token",
            json!({"match": {"title": "dogs dogs cats"}}),
        ),
        (
            "bool_phrase_multi_match",
            json!({"bool": {"should": [
                {"multi_match": {"query": "cats dogs", "fields": ["title", "body"], "type": "phrase"}},
                {"match": {"title": "birds"}}
            ]}}),
        ),
    ]
}

/// `None` when the ranked lists agree; otherwise what differs.
fn differs(
    phase: &str,
    name: &str,
    want: &[(String, f32)],
    got: &[(String, f32)],
) -> Option<String> {
    let ids = |v: &[(String, f32)]| v.iter().map(|(i, _)| i.clone()).collect::<Vec<_>>();
    let same = ids(got) == ids(want)
        && got
            .iter()
            .zip(want)
            .all(|((_, g), (_, w))| (g - w).abs() <= 1e-4 * w.abs().max(1.0));
    (!same).then(|| {
        format!(
            "{name}: {phase} differs from the flushed list (#3349)\n got  {got:?}\n want {want:?}"
        )
    })
}

/// Unflushed (memtable-only), then flushed: same ranking, same scores.
#[tokio::test]
async fn bool_scores_agree_before_and_after_flush() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("b3349", schema()).unwrap();
    let idx = engine.get_index("b3349").unwrap();
    for (id, doc) in docs() {
        idx.index_document(Some(id.into()), doc).await.unwrap();
    }

    let mut before = Vec::new();
    for (name, q) in queries() {
        before.push((name, ranked(&idx, &q).await));
    }
    idx.flush().await.unwrap();
    let mut bad = Vec::new();
    for ((name, q), (_, pre)) in queries().into_iter().zip(&before) {
        let post = ranked(&idx, &q).await;
        assert!(!post.is_empty(), "{name}: no hits after flush");
        bad.extend(differs("pre-flush", name, &post, pre));
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Half the docs in a segment, half in the memtable (index-wide statistics,
/// #188), then everything flushed: same ranking, same scores.
#[tokio::test]
async fn bool_scores_agree_across_segment_and_memtable() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("b3349mix", schema()).unwrap();
    let idx = engine.get_index("b3349mix").unwrap();
    let all = docs();
    let (first, second) = all.split_at(3);
    for (id, doc) in first {
        idx.index_document(Some((*id).into()), doc.clone())
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();
    for (id, doc) in second {
        idx.index_document(Some((*id).into()), doc.clone())
            .await
            .unwrap();
    }

    let mut mixed = Vec::new();
    for (name, q) in queries() {
        mixed.push((name, ranked(&idx, &q).await));
    }
    idx.flush().await.unwrap();
    // Two segments now: fold them so the comparison is against ONE arm.
    idx.force_merge(1).await.unwrap();
    let mut bad = Vec::new();
    for ((name, q), (_, pre)) in queries().into_iter().zip(&mixed) {
        let post = ranked(&idx, &q).await;
        assert!(!post.is_empty(), "{name}: no hits after flush");
        bad.extend(differs("segment+memtable", name, &post, pre));
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
