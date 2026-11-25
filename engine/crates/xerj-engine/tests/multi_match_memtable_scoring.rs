//! Issue #3329: `multi_match` scored every buffered (memtable) hit a flat
//! constant — 1.0 per matched field — because the memtable dispatch sent
//! every `MultiMatch` to the stored-doc scan, whose scorer has no BM25. The
//! same query scored with BM25 once `_flush` moved the docs into a segment,
//! so the ranking changed at flush (BEIR scifact nDCG@10 0.19 unflushed vs
//! 0.62 flushed, measured in the issue).
//!
//! The fix scores the field-centric `multi_match` types through the SAME
//! per-field memtable BM25 path a standalone `match` takes and combines per
//! doc like the segment projection (`query_node_to_fts_projected`): MAX for
//! `best_fields`, SUM for `most_fields` / `cross_fields`. The phrase shapes
//! (`multi_match` `type: phrase`, `match_phrase`) had the same defect: each
//! field now scores like a `match` of the phrase text — what the segment's
//! `execute_phrase` sums — gated by the stored-doc phrase predicate, combined
//! by MAX. A filtered `bool` whose lone `must` is a `multi_match` takes the
//! same path (the #2403 arm). These tests pin, per
//! query shape, that the ranked `(id, score)` list is the same before the
//! flush, with the index split across a segment and the memtable, and after
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

fn queries() -> Vec<(&'static str, Value)> {
    vec![
        // Control: `match` already scored BM25 on the memtable.
        ("match", json!({"match": {"title": "cats dogs"}})),
        (
            "match_phrase",
            json!({"match_phrase": {"title": "cats dogs"}}),
        ),
        // The issue's literal repro shape (best_fields default, one field).
        (
            "best_fields_one_field",
            json!({"multi_match": {"query": "cats", "fields": ["title"]}}),
        ),
        (
            "best_fields",
            json!({"multi_match": {"query": "cats dogs", "fields": ["title", "body"]}}),
        ),
        (
            "best_fields_boosted",
            json!({"multi_match": {"query": "cats fish", "fields": ["title^3", "body"], "boost": 2.0}}),
        ),
        (
            "most_fields",
            json!({"multi_match": {"query": "cats dogs", "fields": ["title", "body"], "type": "most_fields"}}),
        ),
        (
            "most_fields_boosted",
            json!({"multi_match": {"query": "dogs birds", "fields": ["title^2", "body"], "type": "most_fields"}}),
        ),
        (
            "cross_fields",
            json!({"multi_match": {"query": "cats fish", "fields": ["title", "body"], "type": "cross_fields"}}),
        ),
        (
            "and_single_token",
            json!({"multi_match": {"query": "fish", "fields": ["title", "body"], "operator": "and"}}),
        ),
        (
            "phrase_one_field",
            json!({"multi_match": {"query": "cats dogs", "fields": ["title^2"], "type": "phrase"}}),
        ),
        (
            "phrase_slop",
            json!({"multi_match": {"query": "cats living", "fields": ["title", "body"], "type": "phrase", "slop": 2}}),
        ),
        (
            "phrase",
            json!({"multi_match": {"query": "cats dogs", "fields": ["title", "body"], "type": "phrase"}}),
        ),
        (
            "bool_must_multi_match_filter",
            json!({"bool": {
                "must": [{"multi_match": {"query": "cats dogs", "fields": ["title", "body"], "type": "most_fields"}}],
                "filter": [{"term": {"tag": "pet"}}]
            }}),
        ),
        (
            "bool_must_multi_match_must_not",
            json!({"bool": {
                "must": [{"multi_match": {"query": "cats dogs", "fields": ["title", "body"]}}],
                "must_not": [{"term": {"tag": "zoo"}}]
            }}),
        ),
    ]
}

fn assert_same(phase: &str, name: &str, want: &[(String, f32)], got: &[(String, f32)]) {
    let ids = |v: &[(String, f32)]| v.iter().map(|(i, _)| i.clone()).collect::<Vec<_>>();
    assert_eq!(
        ids(got),
        ids(want),
        "{name}: {phase} ranking differs from the flushed one (#3329)\n got  {got:?}\n want {want:?}"
    );
    for ((id, g), (_, w)) in got.iter().zip(want) {
        assert!(
            (g - w).abs() <= 1e-4 * w.abs().max(1.0),
            "{name}: {phase} score of {id} = {g}, flushed = {w} (#3329)\n got  {got:?}\n want {want:?}"
        );
    }
}

/// Unflushed (memtable-only), then flushed: same ranking, same scores.
#[tokio::test]
async fn multi_match_scores_agree_before_and_after_flush() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("mm3329", schema()).unwrap();
    let idx = engine.get_index("mm3329").unwrap();
    for (id, doc) in docs() {
        idx.index_document(Some(id.into()), doc).await.unwrap();
    }

    let mut before = Vec::new();
    for (name, q) in queries() {
        before.push((name, ranked(&idx, &q).await));
    }
    idx.flush().await.unwrap();
    for ((name, q), (_, pre)) in queries().into_iter().zip(&before) {
        let post = ranked(&idx, &q).await;
        assert!(!post.is_empty(), "{name}: no hits after flush");
        assert_same("pre-flush", name, &post, pre);
    }

    // The literal issue symptom: a flat 1.0 for every buffered hit.
    let (_, repro) = before
        .iter()
        .find(|(n, _)| *n == "best_fields_one_field")
        .unwrap();
    assert!(
        repro.len() >= 2 && (repro[0].1 - repro[repro.len() - 1].1).abs() > 1e-4,
        "pre-flush best_fields scores are flat: {repro:?} (#3329)"
    );
}

/// Half the docs in a segment, half in the memtable (index-wide statistics,
/// #188), then everything flushed: same ranking, same scores.
#[tokio::test]
async fn multi_match_scores_agree_across_segment_and_memtable() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("mm3329mix", schema()).unwrap();
    let idx = engine.get_index("mm3329mix").unwrap();
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
    for ((name, q), (_, pre)) in queries().into_iter().zip(&mixed) {
        let post = ranked(&idx, &q).await;
        assert!(!post.is_empty(), "{name}: no hits after flush");
        assert_same("segment+memtable", name, &post, pre);
    }
}
