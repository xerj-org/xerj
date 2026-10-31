//! Issue #2403: `bool{must:[match], filter:[...]}` scored buffered documents
//! with the IDF-less brute scorer (`score_query_against_doc`:
//! `1 + ln(1 + tf)`), while the identical `must` clause on its own took the
//! memtable BM25 arm. Before `_flush` the filtered query ranked a one-word
//! document level with an eight-word one, and every score moved at flush.
//!
//! A `filter` or `must_not` clause is non-scoring (`BooleanClause.isScoring()`,
//! BooleanClause.java:84-87), so the contract is the one #361 pinned for the
//! segment path, now held in every phase: each hit scores exactly what the
//! `must` clause scores alone, in the same phase, and the hit set is the
//! `must` hits the filter keeps. `bool_filter_is_non_scoring.rs` covers the
//! flushed shapes; this file covers the memtable and the mixed index.
//!
//! Elasticsearch is referenced for wire semantics only; no ES code is here.

use std::collections::{BTreeSet, HashMap};

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::{Engine, Index};
use xerj_query::executor::SearchResult;
use xerj_query::parse_request;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

fn req(q: &Value) -> xerj_query::ast::SearchRequest {
    parse_request(&json!({ "query": q, "size": 100 })).expect("parse_request")
}

async fn search(idx: &Index, q: &Value) -> SearchResult {
    idx.search(&req(q)).await.unwrap()
}

/// `alpha` appears in most bodies with varying frequency and body length, so
/// BM25 and the brute scorer disagree on order; `tag` splits the corpus.
fn doc(i: usize) -> Value {
    let body = match i % 4 {
        0 => "alpha alpha alpha alpha".to_string(),
        1 => "alpha".to_string(),
        2 => "alpha beta gamma delta epsilon zeta eta theta".to_string(),
        _ => format!("beta gamma {i}"),
    };
    json!({ "body": body, "tag": if i.is_multiple_of(3) { "y" } else { "x" } })
}

async fn seed(name: &str, dir: &TempDir) -> std::sync::Arc<Index> {
    let engine = make_engine(dir);
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("tag", FieldType::Keyword));
    engine.create_index(name, schema).unwrap();
    engine.get_index(name).unwrap()
}

async fn index_range(idx: &Index, range: std::ops::Range<usize>) {
    for i in range {
        idx.index_document(Some(format!("d{i:02}")), doc(i))
            .await
            .unwrap();
    }
}

/// Every hit of `filtered` scores what `bare` scores for the same `_id`,
/// the order is `bare`'s order restricted to the kept set, the kept set is
/// `want`, and `hits.total` counts exactly the kept set.
async fn assert_scores_like_bare(
    idx: &Index,
    bare: &Value,
    filtered: &Value,
    want: &BTreeSet<String>,
    phase: &str,
) {
    let reference = search(idx, bare).await;
    let actual = search(idx, filtered).await;
    let got: BTreeSet<String> = actual.hits.iter().map(|h| h.id.clone()).collect();
    assert_eq!(&got, want, "{phase}: hit set of {filtered}");
    assert_eq!(
        actual.total.value as usize,
        want.len(),
        "{phase}: hits.total of {filtered}"
    );
    let reference_scores: HashMap<&str, f32> = reference
        .hits
        .iter()
        .map(|h| (h.id.as_str(), h.score))
        .collect();
    for hit in &actual.hits {
        let expected = reference_scores[hit.id.as_str()];
        assert!(
            (hit.score - expected).abs() < 1e-4,
            "{phase}: {} scores {} under {filtered} but {expected} under the bare \
             must clause; a non-scoring clause changed _score (#2403)",
            hit.id,
            hit.score
        );
    }
    let order: Vec<&str> = reference
        .hits
        .iter()
        .map(|h| h.id.as_str())
        .filter(|id| got.contains(*id))
        .collect();
    let actual_order: Vec<&str> = actual.hits.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(actual_order, order, "{phase}: order of {filtered} (#2403)");
}

/// The docs among `0..n` whose body holds `alpha` and that `keep` admits.
fn alpha_docs(n: usize, keep: impl Fn(usize) -> bool) -> BTreeSet<String> {
    (0..n)
        .filter(|i| i % 4 != 3 && keep(*i))
        .map(|i| format!("d{i:02}"))
        .collect()
}

async fn assert_phase(idx: &Index, n: usize, phase: &str) {
    let bare = json!({ "match": { "body": "alpha" } });
    let tag_x = |i: usize| !i.is_multiple_of(3);

    let filter = json!({ "bool": {
        "must": [ { "match": { "body": "alpha" } } ],
        "filter": [ { "term": { "tag": "x" } } ],
    }});
    assert_scores_like_bare(idx, &bare, &filter, &alpha_docs(n, tag_x), phase).await;

    let must_not = json!({ "bool": {
        "must": [ { "match": { "body": "alpha" } } ],
        "must_not": [ { "term": { "tag": "y" } } ],
    }});
    assert_scores_like_bare(idx, &bare, &must_not, &alpha_docs(n, tag_x), phase).await;

    // A filter that reads `_id`: the stored-doc matcher needs it injected.
    let ids_filter = json!({ "bool": {
        "must": [ { "match": { "body": "alpha" } } ],
        "filter": [ { "ids": { "values": ["d00", "d01", "d03"] } } ],
    }});
    let want: BTreeSet<String> = ["d00", "d01"].iter().map(|s| s.to_string()).collect();
    assert_scores_like_bare(idx, &bare, &ids_filter, &want, phase).await;

    // A boost on the must clause reaches the score the same way it does bare.
    let boosted_bare = json!({ "match": { "body": { "query": "alpha", "boost": 3.0 } } });
    let boosted = json!({ "bool": {
        "must": [ { "match": { "body": { "query": "alpha", "boost": 3.0 } } } ],
        "filter": [ { "term": { "tag": "x" } } ],
    }});
    assert_scores_like_bare(idx, &boosted_bare, &boosted, &alpha_docs(n, tag_x), phase).await;
}

#[tokio::test]
async fn filtered_match_scores_like_the_bare_match_before_and_after_flush() {
    let dir = TempDir::new().unwrap();
    let idx = seed("bfmem", &dir).await;
    index_range(&idx, 0..24).await;

    assert_phase(&idx, 24, "pre-flush").await;
    idx.flush().await.unwrap();
    assert_phase(&idx, 24, "post-flush").await;
}

#[tokio::test]
async fn filtered_match_scores_like_the_bare_match_on_a_mixed_index() {
    let dir = TempDir::new().unwrap();
    let idx = seed("bfmix", &dir).await;
    index_range(&idx, 0..12).await;
    idx.flush().await.unwrap();
    index_range(&idx, 12..24).await;

    assert_phase(&idx, 24, "mixed").await;
}

/// Shapes the memtable BM25 path cannot serve (`operator: and` needs the
/// per-token doc-scan) keep their existing route; the hit set is asserted
/// in both phases.
#[tokio::test]
async fn operator_and_must_keeps_its_hit_set() {
    let dir = TempDir::new().unwrap();
    let idx = seed("bfand", &dir).await;
    index_range(&idx, 0..24).await;

    let q = json!({ "bool": {
        "must": [ { "match": { "body": { "query": "alpha beta", "operator": "and" } } } ],
        "filter": [ { "term": { "tag": "x" } } ],
    }});
    let want: BTreeSet<String> = (0..24)
        .filter(|i| i % 4 == 2 && i % 3 != 0)
        .map(|i| format!("d{i:02}"))
        .collect();
    for phase in ["pre-flush", "post-flush"] {
        let got: BTreeSet<String> = search(&idx, &q)
            .await
            .hits
            .iter()
            .map(|h| h.id.clone())
            .collect();
        assert_eq!(got, want, "{phase}: operator:and hit set");
        if phase == "pre-flush" {
            idx.flush().await.unwrap();
        }
    }
}

/// A per-query `analyzer` keeps the existing route too: the memtable BM25
/// path tokenizes with the index default and would lowercase `ALPHA`, while
/// `whitespace` keeps its case, so the hit set would change at flush.
#[tokio::test]
async fn match_with_an_analyzer_keeps_its_hit_set() {
    let dir = TempDir::new().unwrap();
    let idx = seed("bfanalyzer", &dir).await;
    index_range(&idx, 0..24).await;

    let q = json!({ "bool": {
        "must": [ { "match": { "body": { "query": "ALPHA", "analyzer": "whitespace" } } } ],
        "filter": [ { "term": { "tag": "x" } } ],
    }});
    for phase in ["pre-flush", "post-flush"] {
        let got: BTreeSet<String> = search(&idx, &q)
            .await
            .hits
            .iter()
            .map(|h| h.id.clone())
            .collect();
        assert!(
            got.is_empty(),
            "{phase}: whitespace analyzer kept case, got {got:?}"
        );
        if phase == "pre-flush" {
            idx.flush().await.unwrap();
        }
    }
}

#[tokio::test]
async fn selective_filter_keeps_low_ranked_live_hits_across_overwrites_and_flushes() {
    let dir = TempDir::new().unwrap();
    let idx = seed("bfselective", &dir).await;
    for i in 0..128 {
        idx.index_document(
            Some(format!("strong-{i}")),
            json!({"body":"alpha", "tag":"drop"}),
        )
        .await
        .unwrap();
    }
    let weak = format!("alpha {}", "unrelated ".repeat(128));
    idx.index_document(Some("selected".into()), json!({"body":weak, "tag":"keep"}))
        .await
        .unwrap();
    let query = json!({"bool": {
        "must":[{"match":{"body":"alpha"}}],
        "filter":[{"term":{"tag":"keep"}}]
    }});
    let request = parse_request(&json!({"query":query, "size":1})).unwrap();
    for phase in ["memory", "mixed", "flushed"] {
        let result = idx.search(&request).await.unwrap();
        assert_eq!(
            result.total.value, 1,
            "{phase}: the filter must run before top-k truncation"
        );
        assert_eq!(result.hits[0].id, "selected", "{phase}");
        idx.flush().await.unwrap();
        if phase == "memory" {
            idx.index_document(Some("selected".into()), json!({"body":weak, "tag":"keep"}))
                .await
                .unwrap();
            idx.index_document(
                Some("deleted".into()),
                json!({"body":"alpha", "tag":"keep"}),
            )
            .await
            .unwrap();
            assert!(idx.delete_document("deleted").await.unwrap());
        }
    }
    idx.index_document(Some("selected".into()), json!({"body":weak, "tag":"drop"}))
        .await
        .unwrap();
    let result = idx.search(&request).await.unwrap();
    assert_eq!(
        result.total.value, 0,
        "the prior matching segment version is a ghost"
    );
    assert!(result.hits.is_empty());
}
