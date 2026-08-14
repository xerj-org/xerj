//! Regression tests for issue #332: a field holding SEVERAL values must be
//! indexed as several values — before AND after `flush()`.
//!
//! Pre-fix the flush and merge paths flattened a source array into one
//! space-joined string (`extract_field_text`), and every non-Text field is
//! indexed with the keyword analyzer, which emits its whole input as ONE
//! token. `{"tags": ["red","blue"]}` therefore produced the single segment
//! term `"red blue"`:
//!
//!   * `term tags=red` had no posting to hit — same for `match`,
//!     `multi_match` and `query_string` with a keyword `default_field`,
//!     which all project to a whole-value `FtsQuery::Term`;
//!   * `term tags="red blue"` MATCHED — a false positive against a document
//!     that never carried that value;
//!   * for a Text field the join also erased the value boundary, so a
//!     `match_phrase` spanning two array elements matched after the flush
//!     and not before it.
//!
//! The pre-flush half had its own defect: the fused columnar bool walk
//! compared against the single-valued doc-values column, which keeps only the
//! FIRST array element, so `term tags=blue` missed while `term tags=red` hit.
//!
//! Every test seeds one index, runs ALL its queries against the memtable,
//! flushes ONCE, re-runs them against the segment, and asserts the matched id
//! set is the expected one in BOTH states — flush timing never changes it.
//! `terms` is included as the control: it never projects to FTS, so it took
//! the array-aware stored scan and was correct throughout.

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

/// Run every case against the memtable, flush once, run every case again
/// against the segment, and assert the hit set is the expected one in both
/// states.
async fn assert_flush_parity(idx: &std::sync::Arc<Index>, cases: &[(Value, &[&str], &str)]) {
    let expected: Vec<BTreeSet<String>> = cases
        .iter()
        .map(|(_, exp, _)| exp.iter().map(|s| s.to_string()).collect())
        .collect();
    for ((q, _, label), exp) in cases.iter().zip(&expected) {
        assert_eq!(
            &ids(idx, q).await,
            exp,
            "{label}: PRE-flush (memtable) hit set wrong for {q}"
        );
    }
    idx.flush().await.unwrap();
    for ((q, _, label), exp) in cases.iter().zip(&expected) {
        assert_eq!(
            &ids(idx, q).await,
            exp,
            "{label}: POST-flush (segment) hit set wrong for {q}"
        );
    }
}

/// Doc 1 carries a two-element `tags` array, doc 2 a one-element array, doc 3
/// a plain scalar — so the same field is multi-valued, single-valued-as-array
/// and scalar within one segment.
async fn seed_tags(engine: &Engine, name: &str) -> std::sync::Arc<Index> {
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("tags", FieldType::Keyword));
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    engine.create_index(name, schema).unwrap();
    let idx = engine.get_index(name).unwrap();
    for (id, doc) in [
        ("1", json!({"body": "hello", "tags": ["red", "blue"]})),
        ("2", json!({"body": "hello", "tags": ["green"]})),
        ("3", json!({"body": "hello", "tags": "red"})),
    ] {
        idx.index_document(Some(id.into()), doc).await.unwrap();
    }
    idx
}

/// The issue's own repro, as an assertion: every clause type that projects to
/// a whole-value `FtsQuery::Term` on a keyword field must see each array
/// element as its own value, in both states.
#[tokio::test(flavor = "multi_thread")]
async fn multivalued_keyword_hit_set_stable_across_flush() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed_tags(&engine, "mvk_parity").await;

    assert_flush_parity(
        &idx,
        &[
            // First element of the array, plus the scalar doc.
            (json!({"term": {"tags": "red"}}), &["1", "3"], "term first"),
            // Second element — post-flush this had no posting at all, and
            // pre-flush the lossy doc-values column dropped it.
            (json!({"term": {"tags": "blue"}}), &["1"], "term second"),
            (
                json!({"term": {"tags": "green"}}),
                &["2"],
                "term one-element",
            ),
            // The clause types that share the Term projection.
            (json!({"match": {"tags": "blue"}}), &["1"], "match"),
            (
                json!({"multi_match": {"query": "blue", "fields": ["tags"]}}),
                &["1"],
                "multi_match",
            ),
            (
                json!({"query_string": {"query": "blue", "default_field": "tags"}}),
                &["1"],
                "query_string default_field",
            ),
            // Control: `terms` never projects to FTS, so it took the
            // array-aware stored scan and was already correct.
            (
                json!({"terms": {"tags": ["blue"]}}),
                &["1"],
                "terms control",
            ),
            (
                json!({"bool": {"must": [{"match": {"body": "hello"}}],
                                "filter": [{"terms": {"tags": ["red"]}}]}}),
                &["1", "3"],
                "bool must + terms filter",
            ),
            // The mixed bool that made #325 keep the Term projection in the
            // FTS tree: it must now agree with the scan on an array field.
            (
                json!({"bool": {"must": [{"match": {"body": "hello"}},
                                         {"term": {"tags": "blue"}}]}}),
                &["1"],
                "bool must match + term",
            ),
        ],
    )
    .await;
}

/// The joined token was also a live FALSE POSITIVE: `"red blue"` is a value no
/// document ever carried, and post-flush it matched doc 1. Deleting the join
/// has to delete the phantom value with it — in both states.
#[tokio::test(flavor = "multi_thread")]
async fn joined_array_token_is_not_a_value() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed_tags(&engine, "mvk_phantom").await;

    assert_flush_parity(
        &idx,
        &[
            (
                json!({"term": {"tags": "red blue"}}),
                &[],
                "joined token is not a value",
            ),
            (
                json!({"terms": {"tags": ["red blue"]}}),
                &[],
                "joined token via terms",
            ),
        ],
    )
    .await;
    // NOT asserted here: `match tags="red blue"`. The memtable analyses every
    // field with the standard analyzer, so pre-flush it ORs "red"/"blue" and
    // matches, while the segment (keyword analyzer, correctly) does not. That
    // divergence is about analyzer selection in the memtable, not about how
    // many values a field carries, and it survives this fix untouched.
}

/// A Text-field array gets a position gap between elements, so a phrase
/// cannot span the boundary. Pre-fix the join put "world" and "quick"
/// adjacent in the segment, so the cross-element phrase matched after the
/// flush and missed before it — the same flush-invariance class as #218/#230.
#[tokio::test(flavor = "multi_thread")]
async fn text_array_phrase_does_not_span_values() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    engine.create_index("mvk_phrase", schema).unwrap();
    let idx = engine.get_index("mvk_phrase").unwrap();
    idx.index_document(
        Some("1".into()),
        json!({"body": ["hello world", "quick fox"]}),
    )
    .await
    .unwrap();
    idx.index_document(Some("2".into()), json!({"body": "world quick"}))
        .await
        .unwrap();

    assert_flush_parity(
        &idx,
        &[
            (
                json!({"match_phrase": {"body": "hello world"}}),
                &["1"],
                "phrase inside one value",
            ),
            (
                json!({"match_phrase": {"body": "world quick"}}),
                &["2"],
                "phrase must not span two values",
            ),
            // Every element's tokens are still searchable individually.
            (
                json!({"match": {"body": "fox"}}),
                &["1"],
                "second element token",
            ),
            (
                json!({"match": {"body": "hello"}}),
                &["1"],
                "first element token",
            ),
        ],
    )
    .await;
}

/// Flush/merge parity: a force-merge re-derives the segment FTS from the
/// stored source through a DIFFERENT extractor (`extract_fts_fields_excluding`
/// in `index.rs`, not the memtable's). If the two disagree, a merge
/// resurrects the joined token and the same document answers `term`
/// differently depending on whether it has been merged yet.
#[tokio::test(flavor = "multi_thread")]
async fn merge_keeps_per_element_terms() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed_tags(&engine, "mvk_merge").await;

    // Two segments, so the merge has something to do.
    idx.flush().await.unwrap();
    idx.index_document(
        Some("4".into()),
        json!({"body": "hello", "tags": ["blue", "violet"]}),
    )
    .await
    .unwrap();
    idx.flush().await.unwrap();

    let cases: &[(Value, &[&str], &str)] = &[
        (
            json!({"term": {"tags": "blue"}}),
            &["1", "4"],
            "term second",
        ),
        (json!({"term": {"tags": "red"}}), &["1", "3"], "term first"),
        (
            json!({"term": {"tags": "violet"}}),
            &["4"],
            "term second segment",
        ),
        (
            json!({"term": {"tags": "red blue"}}),
            &[],
            "joined token is not a value",
        ),
    ];

    for (q, exp, label) in cases {
        let expected: BTreeSet<String> = exp.iter().map(|s| s.to_string()).collect();
        assert_eq!(&ids(&idx, q).await, &expected, "{label}: before merge");
    }
    idx.force_merge(1).await.unwrap();
    for (q, exp, label) in cases {
        let expected: BTreeSet<String> = exp.iter().map(|s| s.to_string()).collect();
        assert_eq!(&ids(&idx, q).await, &expected, "{label}: after force-merge");
    }
}
