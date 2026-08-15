//! #397 — `term` on a `text` field must resolve against the ANALYSED term
//! dictionary, not the document's `_source` value.
//!
//! ES analyses the INDEX side of a `text` field and does NOT analyse the query
//! side of a `term` query.  So `{"term":{"body":"quick"}}` matches a document
//! whose `body` is `"the quick brown fox"` (`quick` is an indexed token), and
//! `{"term":{"title":"HnswGraphBuilder.java"}}` does NOT match a document with
//! exactly that `title` (the indexed token is lowercased).  Before this fix
//! XERJ answered the opposite in both directions because every path compared
//! the query value against `_source`.
//!
//! Every assertion is checked BEFORE and AFTER `_flush` — the memtable answers
//! from `doc_matches_query`'s analysed-dictionary arm and the segment answers
//! from the FTS term dictionary, and the two must agree or the hit set changes
//! at flush (the #218 regression class).
//!
//! `path` is a `keyword` field and is asserted UNCHANGED: keyword terms stay
//! byte-exact and case-sensitive.

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

fn request(body: Value) -> xerj_query::ast::SearchRequest {
    parse_request(&body).expect("parse_request")
}

async fn seed() -> (TempDir, std::sync::Arc<Index>) {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("title", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("path", FieldType::Keyword));
    schema
        .fields
        .push(FieldConfig::new("rank", FieldType::Long));
    // An IP-shaped value under a `text` mapping: `{"term": {"ip": "<CIDR>"}}`
    // has a meaning that is NOT a dictionary lookup, and the analysed arm must
    // not swallow it.  No tokenizer emits a token containing `/`, so without
    // an explicit carve-out every CIDR term would answer zero.
    schema.fields.push(FieldConfig::new("ip", FieldType::Text));
    engine.create_index("tx", schema).unwrap();
    let idx = engine.get_index("tx").unwrap();

    for (id, title, body, path, rank, ip) in [
        (
            "hnsw",
            "HnswGraphBuilder.java",
            "the quick brown fox",
            "lucene/HnswGraphBuilder.java",
            1,
            "192.168.1.10",
        ),
        (
            "seg",
            "TestSegmentReader.java",
            "the quick red fox",
            "lucene/TestSegmentReader.java",
            2,
            "192.168.1.200",
        ),
        (
            "doc",
            "readme.md",
            "slow green turtle",
            "docs/readme.md",
            3,
            "10.0.0.1",
        ),
    ] {
        idx.index_document(
            Some(id.into()),
            json!({"title": title, "body": body, "path": path, "rank": rank, "ip": ip}),
        )
        .await
        .unwrap();
    }
    (dir, idx)
}

/// `(hits.total, sorted hit ids)` for a full search body.
///
/// Every `search` future in this file is boxed: `Index::search` composes a
/// very large state machine and a dozen of them inlined into one async test
/// body overflows the test thread's stack before any assertion runs.
async fn hits(idx: &Index, body: Value) -> (u64, Vec<String>) {
    let r = Box::pin(idx.search(&request(body))).await.unwrap();
    let mut ids: Vec<String> = r.hits.iter().map(|h| h.id.clone()).collect();
    ids.sort();
    (r.total.value, ids)
}

/// `hits.total` for the same query issued as a `size: 0` count — the shape
/// `_count` synthesises, which is served by an entirely different set of
/// shortcuts (doc-values / FST) from the `size > 0` path.  #362 closed as
/// "`_count` must agree with `_search`"; this keeps that true through #397.
async fn count(idx: &Index, query: Value) -> u64 {
    Box::pin(idx.search(&request(json!({"query": query, "size": 0}))))
        .await
        .unwrap()
        .total
        .value
}

/// Assert one query's hit set, its `size: 0` count, and that both are the same
/// before and after a flush.
async fn check(idx: &Index, arm: &str, label: &str, query: Value, expected: &[&str]) {
    let want: Vec<String> = {
        let mut v: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        v.sort();
        v
    };
    let (total, ids) = Box::pin(hits(idx, json!({"query": query.clone(), "size": 20}))).await;
    assert_eq!(ids, want, "[{arm}] {label}: matched ids (query = {query})");
    assert_eq!(
        total,
        want.len() as u64,
        "[{arm}] {label}: hits.total (query = {query})"
    );
    let c = Box::pin(count(idx, query.clone())).await;
    assert_eq!(
        c,
        want.len() as u64,
        "[{arm}] {label}: size:0 count disagrees with _search (query = {query})"
    );
}

/// The whole #397 table, run once against the memtable and once against a
/// flushed segment.
async fn assert_term_semantics(idx: &Index, arm: &str) {
    // ── the two rows from the issue ──────────────────────────────────────
    // `quick` IS an indexed token of `body`; `_source` never equals it.
    Box::pin(check(
        idx,
        arm,
        "term body=quick",
        json!({"term": {"body": "quick"}}),
        &["hnsw", "seg"],
    ))
    .await;
    // The literal `_source` spelling is NOT a dictionary entry — this is the
    // breaking half of #397 (it used to return the document).
    Box::pin(check(
        idx,
        arm,
        "term title=HnswGraphBuilder.java",
        json!({"term": {"title": "HnswGraphBuilder.java"}}),
        &[],
    ))
    .await;
    // The lowercased spelling IS the indexed token.
    Box::pin(check(
        idx,
        arm,
        "term title=hnswgraphbuilder.java",
        json!({"term": {"title": "hnswgraphbuilder.java"}}),
        &["hnsw"],
    ))
    .await;
    // A whole analysed field value is not a single dictionary entry.
    Box::pin(check(
        idx,
        arm,
        "term body=<whole source value>",
        json!({"term": {"body": "the quick brown fox"}}),
        &[],
    ))
    .await;
    // The query term is NOT analysed: an upper-cased token never matches the
    // lowercased dictionary entry, even though `match` would.
    Box::pin(check(
        idx,
        arm,
        "term body=QUICK",
        json!({"term": {"body": "QUICK"}}),
        &[],
    ))
    .await;

    // ── control: `match` is unchanged and still analyses both sides ───────
    Box::pin(check(
        idx,
        arm,
        "match body=quick (control)",
        json!({"match": {"body": "quick"}}),
        &["hnsw", "seg"],
    ))
    .await;
    Box::pin(check(
        idx,
        arm,
        "match body=QUICK (control)",
        json!({"match": {"body": "QUICK"}}),
        &["hnsw", "seg"],
    ))
    .await;

    // ── keyword fields stay byte-exact and case-sensitive ────────────────
    Box::pin(check(
        idx,
        arm,
        "term path=<exact> (keyword unchanged)",
        json!({"term": {"path": "lucene/HnswGraphBuilder.java"}}),
        &["hnsw"],
    ))
    .await;
    Box::pin(check(
        idx,
        arm,
        "term path=<lowercased> (keyword unchanged)",
        json!({"term": {"path": "lucene/hnswgraphbuilder.java"}}),
        &[],
    ))
    .await;

    // ── CIDR `term` on an analysed field keeps its non-dictionary meaning ─
    Box::pin(check(
        idx,
        arm,
        "term ip=<CIDR> on a text field",
        json!({"term": {"ip": "192.168.1.0/24"}}),
        &["hnsw", "seg"],
    ))
    .await;

    // ── the same term nested in the compound shapes that take their own
    //    doc-values shortcuts: fused bool filter, bool with must_not, and a
    //    conjunction with a numeric range.
    Box::pin(check(
        idx,
        arm,
        "bool.filter[term body=quick]",
        json!({"bool": {"filter": [{"term": {"body": "quick"}}]}}),
        &["hnsw", "seg"],
    ))
    .await;
    Box::pin(check(
        idx,
        arm,
        "bool.must_not[term body=quick]",
        json!({"bool": {"must_not": [{"term": {"body": "quick"}}],
                        "must": [{"match_all": {}}]}}),
        &["doc"],
    ))
    .await;
    Box::pin(check(
        idx,
        arm,
        "bool.filter[term body=quick, range rank>=2]",
        json!({"bool": {"filter": [
            {"term": {"body": "quick"}},
            {"range": {"rank": {"gte": 2}}}
        ]}}),
        &["seg"],
    ))
    .await;
    // Mixed keyword + analysed conjunction: the keyword leaf is still served
    // exactly, the analysed one no longer poisons it.
    Box::pin(check(
        idx,
        arm,
        "bool.filter[term path=<exact>, term body=quick]",
        json!({"bool": {"filter": [
            {"term": {"path": "lucene/HnswGraphBuilder.java"}},
            {"term": {"body": "quick"}}
        ]}}),
        &["hnsw"],
    ))
    .await;

    // ── field-sorted `term`: the sorted path narrows candidates from the
    //    same doc-values sets the prefilter uses.
    let (total, ids) = Box::pin(hits(
        idx,
        json!({
            "query": {"term": {"body": "quick"}},
            "size": 20,
            "sort": [{"rank": {"order": "desc"}}]
        }),
    ))
    .await;
    assert_eq!(total, 2, "[{arm}] sorted term body=quick: hits.total");
    assert_eq!(
        ids.into_iter().collect::<BTreeSet<_>>(),
        ["hnsw".to_string(), "seg".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>(),
        "[{arm}] sorted term body=quick: matched ids"
    );

    // ── size:0 + aggs: the columnar agg fast path compiles the query into a
    //    doc-values predicate, which cannot answer an analysed term.
    let r = Box::pin(idx.search(&request(json!({
        "query": {"term": {"body": "quick"}},
        "size": 0,
        "aggs": {"by_path": {"terms": {"field": "path"}}}
    }))))
    .await
    .unwrap();
    assert_eq!(
        r.total.value, 2,
        "[{arm}] term body=quick + aggs: hits.total"
    );
    let buckets = r
        .aggs
        .as_ref()
        .and_then(|a| a.get("by_path"))
        .and_then(|a| a.get("buckets"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let bucket_docs: u64 = buckets
        .iter()
        .filter_map(|b| b.get("doc_count").and_then(Value::as_u64))
        .sum();
    assert_eq!(
        bucket_docs, 2,
        "[{arm}] term body=quick + aggs: bucket doc_counts must cover only the \
         two analysed-dictionary matches, got {buckets:?}"
    );
}

#[tokio::test]
async fn term_on_text_resolves_against_the_analysed_dictionary() {
    let (_dir, idx) = seed().await;
    // Unflushed: answered by the memtable.
    Box::pin(assert_term_semantics(&idx, "memtable")).await;
    idx.flush().await.unwrap();
    // Flushed: answered by the segment term dictionary.  Identical answers
    // are the flush-invariance property, not a coincidence.
    Box::pin(assert_term_semantics(&idx, "segment")).await;
}
