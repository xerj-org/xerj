//! Regression tests for #362 — `_count` must answer the same question
//! `_search` does for a `term` query on a `text` field.
//!
//! The count-only (`size:0`) path resolved a `term` through the segment FTS
//! term dictionary and additionally re-spelled the term lowercase when the
//! byte-exact lookup missed:
//!
//! ```text
//! reader.term_doc_freq(field, &raw).or_else(|| reader.term_doc_freq(field, &lowered))
//! ```
//!
//! The hit-materialising path never did either, so the two APIs disagreed
//! about one and the same query:
//!
//! ```text
//! term title=TestSegmentReader.java   _count=1   _search total=1  hits=1
//! term title=testsegmentreader.java   _count=1   _search total=0  hits=0   <- disagree
//! ```
//!
//! Not just casing: the dictionary holds ANALYSED tokens, so
//! `{"term":{"title":"quick"}}` against `"the quick brown fox"` also counted
//! 1 while `_search` returned nothing. A count that describes a hit set
//! nobody can retrieve is worse than a slow count.
//!
//! Deleting the `.or_else(&lowered)` re-spelling on its own is NOT the fix,
//! and these tests are what showed it. With the re-spelling gone but the
//! dictionary lookup kept, the first two tests below still fail (the
//! dictionary already holds the lowercased token, so `raw` hits it directly,
//! and `quick` is a token outright) AND the third starts failing: the
//! byte-exact `term title=TestSegmentReader.java`, which `_search` answers
//! `1`, misses the lowercased dictionary and counts `0`. The re-spelling was
//! scaffolding over the real defect — consulting an ANALYSED dictionary for a
//! question the hit path answers against the raw `_source`. So the shortcut
//! now ABANDONS when the segment has no doc-values column for the field, and
//! the ordinary scan — the code that produces the hits — answers.
//!
//! The tests below are written against the property, not the symptom:
//! whatever the answer is, `size:0` and `size:10` must agree on it.

use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldOptions, FieldType, Schema};
use xerj_engine::{Engine, Index};
use xerj_query::parse_request;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.engine.ingest_shards = 1;
    Engine::new(config).expect("engine::new")
}

/// An ES `{"type": "text"}` field as `es_properties_to_fields` builds it:
/// analysed, indexed, and — like ES — **no doc-values column**
/// (`es_compat.rs:15400`, `doc_values_default`). That last part is what puts
/// the count shortcut on its FTS term-dictionary fallback, which is where the
/// two APIs part ways.
fn es_text_field(name: &str) -> FieldConfig {
    FieldConfig::new(name, FieldType::Text).with_options(FieldOptions {
        doc_values: false,
        ..FieldOptions::default()
    })
}

fn text_schema() -> Schema {
    let mut s = Schema::empty();
    s.add_field(es_text_field("title")).unwrap();
    s.add_field(es_text_field("body")).unwrap();
    s
}

/// `_count` total (the `size:0` shortcut) and `_search` total + materialised
/// hit count for the same `term` query.
async fn count_vs_search(idx: &Arc<Index>, field: &str, value: &str) -> (u64, u64, usize) {
    let q = json!({"term": {field: value}});
    let count = idx
        .search(&parse_request(&json!({"size": 0, "query": q})).expect("parse size:0"))
        .await
        .expect("count search")
        .total
        .value;
    let res = idx
        .search(&parse_request(&json!({"size": 10, "query": q})).expect("parse size:10"))
        .await
        .expect("hit search");
    (count, res.total.value, res.hits.len())
}

async fn corpus(dir: &TempDir) -> Arc<Index> {
    let engine = make_engine(dir);
    engine.create_index("t", text_schema()).unwrap();
    let idx = engine.get_index("t").unwrap();
    idx.index_document(
        Some("d1".into()),
        json!({"title": "TestSegmentReader.java", "body": "the quick brown fox"}),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("d2".into()),
        json!({"title": "HnswGraphBuilder.java", "body": "lazy dogs sleep"}),
    )
    .await
    .unwrap();
    // The disagreement lives in the SEGMENT arm of the count shortcut, so the
    // docs have to be flushed out of the memtable to reach it.
    idx.flush().await.unwrap();
    idx
}

/// The exact row from the issue that disagreed: a lowercased spelling of a
/// mixed-case value. `_count` re-spelled the term and reported 1; `_search`
/// stayed byte-exact and returned 0.
#[tokio::test]
async fn count_does_not_respell_a_term_lowercase() {
    let dir = TempDir::new().unwrap();
    let idx = corpus(&dir).await;

    let (count, total, hits) = count_vs_search(&idx, "title", "testsegmentreader.java").await;
    assert_eq!(
        count, total,
        "#362: _count={count} but _search total={total} (hits={hits}) for the same \
         term query — the count path re-spelled the term lowercase and reported a \
         document _search cannot return"
    );
    assert_eq!(
        count, hits as u64,
        "#362: _count={count} but _search materialised {hits} hit(s) for the same term query"
    );
}

/// The half that is NOT about casing: the FTS term dictionary holds analysed
/// tokens, so a single word out of a multi-token `text` value resolved to a
/// non-zero doc-freq for `_count` while `_search` matched nothing.
#[tokio::test]
async fn count_does_not_match_a_single_token_of_an_analysed_text_value() {
    let dir = TempDir::new().unwrap();
    let idx = corpus(&dir).await;

    let (count, total, hits) = count_vs_search(&idx, "body", "quick").await;
    assert_eq!(
        count, total,
        "#362: _count={count} but _search total={total} (hits={hits}) for \
         {{\"term\":{{\"body\":\"quick\"}}}} against \"the quick brown fox\" — the count \
         path consulted the analysed term dictionary, the hit path did not"
    );
    assert_eq!(
        count, hits as u64,
        "#362: _count={count} but _search materialised {hits} hit(s)"
    );
}

/// The other way a field reaches this fallback: `build_doc_value_columns`
/// poisons a MULTI-VALUED field (it ships no column at all), so a `keyword`
/// field with an array value lands on the same code path, where flush
/// flattened `["AB-1","CD-2"]` into the single token `"AB-1 CD-2"` (#332).
///
/// Honest status of this test: it passes on unmodified `main` too — checked,
/// not assumed — because on this corpus both APIs answer 0 and therefore
/// already agree. It is a guard, not a reproduction: it pins the agreement
/// for the shape most likely to regress if the abandon is ever narrowed to
/// "analysed fields only", since a `keyword` field is exactly what such a
/// narrowing would let back onto the dictionary.
#[tokio::test]
async fn count_matches_search_on_a_multi_valued_keyword_field() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let mut schema = Schema::empty();
    schema
        .add_field(FieldConfig::new("code", FieldType::Keyword))
        .unwrap();
    engine.create_index("m", schema).unwrap();
    let idx = engine.get_index("m").unwrap();
    idx.index_document(Some("m0".into()), json!({"code": ["AB-1", "CD-2"]}))
        .await
        .unwrap();
    idx.index_document(Some("m1".into()), json!({"code": ["EF-3"]}))
        .await
        .unwrap();
    idx.flush().await.unwrap();

    let (count, total, hits) = count_vs_search(&idx, "code", "AB-1").await;
    assert_eq!(
        count, total,
        "#362: _count={count} but _search total={total} (hits={hits}) for a term on a \
         multi-valued keyword field — the count path read a flattened dictionary token"
    );
    assert_eq!(
        count, hits as u64,
        "#362: _count={count} but _search materialised {hits} hit(s)"
    );
}

/// A `keyword` field with `doc_values: false` has no `.dv` column either and
/// reaches the same fallback. `_count` and `_search` must agree here too —
/// and the case-insensitive fudge the count path used to apply must be gone,
/// so a lowercased spelling of a mixed-case keyword value counts 0.
#[tokio::test]
async fn keyword_field_without_doc_values_agrees_with_search() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let mut schema = Schema::empty();
    schema
        .add_field(
            FieldConfig::new("code", FieldType::Keyword).with_options(FieldOptions {
                doc_values: false,
                ..FieldOptions::default()
            }),
        )
        .unwrap();
    engine.create_index("k", schema).unwrap();
    let idx = engine.get_index("k").unwrap();
    for (i, code) in ["AB-1", "AB-1", "CD-2"].iter().enumerate() {
        idx.index_document(Some(format!("k{i}")), json!({ "code": code }))
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();

    let (count, total, hits) = count_vs_search(&idx, "code", "AB-1").await;
    assert_eq!(
        (count, total, hits),
        (2, 2, 2),
        "keyword term count: _count={count}, _search total={total}, hits={hits}"
    );
    // And the case-sensitivity the count path used to fudge away stays gone.
    let (lc_count, lc_total, lc_hits) = count_vs_search(&idx, "code", "ab-1").await;
    assert_eq!(
        (lc_count, lc_total, lc_hits),
        (0, 0, 0),
        "a lowercased spelling of a keyword value must not count: _count={lc_count}, \
         _search total={lc_total}, hits={lc_hits}"
    );
}

/// The byte-exact spelling has to keep working — the fix must not turn a
/// correct count into a zero.
#[tokio::test]
async fn count_and_search_agree_on_the_byte_exact_spelling() {
    let dir = TempDir::new().unwrap();
    let idx = corpus(&dir).await;

    let (count, total, hits) = count_vs_search(&idx, "title", "TestSegmentReader.java").await;
    // Stated as an absolute, not just as agreement: the two disagreement
    // tests above are satisfied by `0 == 0`, so something has to prove the
    // corpus is really there and the fix did not simply zero the count.
    assert_eq!(
        (count, total, hits),
        (1, 1, 1),
        "the byte-exact spelling must still find d1: _count={count}, \
         _search total={total}, hits={hits}"
    );
}
