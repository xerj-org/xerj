//! Regression tests for issue #354: `match` / `multi_match` / `query_string`
//! on a **keyword** field must return the same hit set before and after
//! `flush()`.
//!
//! A keyword field is indexed with the keyword analyzer, so both sides of the
//! comparison are ONE case-preserved token: `"red blue"` is a single term, it
//! does not equal `"red"`, and `match {tags: "red blue"}` must not match the
//! document `{"tags": "red"}`. The segment path already did this — its
//! projection lowers an `exact_fields` member to a whole-value `FtsQuery::Term`.
//!
//! Pre-flush, TWO independent evaluators got it wrong, and both are fixed here:
//!
//!  * `doc_matches_query` / `score_query_against_doc` (the stored-source scan,
//!    which answers `multi_match`, `bool`, and every container shape) took no
//!    schema argument at all, so they analyzed every field as if it were `text`;
//!  * a top-level `match` never reached the scan in the first place — it is
//!    answered by the memtable's BM25 index, whose postings for a keyword field
//!    are STANDARD-analyzed (`memtable::collect_text_fields` sweeps every
//!    string-valued key of the source into it, not just declared `text`
//!    fields), so both the tokenisation and the case folding were wrong there.
//!
//! Every test seeds one index, runs ALL its queries against the memtable
//! (pre-flush), flushes ONCE, re-runs the same queries against the segment
//! (post-flush), and asserts each query's matched id set is the expected one in
//! BOTH states — i.e. flush timing never changes the hit set. Same harness
//! shape as `multi_match_flush_parity.rs` (issue #218).

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

/// `hits.total.value` for the same request. Asserted alongside the hit set
/// because the two are computed by DIFFERENT code in this engine (the count
/// shortcuts vs. the materialising scan), and a `_count` / `size: 0` that
/// disagrees with `size: 50` is the same silent-wrong-answer class as the bug
/// under test.
async fn total(idx: &Index, q: &Value) -> u64 {
    idx.search(&req(q.clone())).await.unwrap().total.value
}

/// Run every (query, expected-ids, label) case against the memtable, flush
/// once, run every case again against the segment, and assert the hit set —
/// and its total — is the expected one in both states.
async fn assert_flush_parity(idx: &std::sync::Arc<Index>, cases: &[(Value, &[&str], &str)]) {
    let expected: Vec<BTreeSet<String>> = cases
        .iter()
        .map(|(_, exp, _)| exp.iter().map(|s| s.to_string()).collect())
        .collect();
    for ((q, _, label), exp) in cases.iter().zip(&expected) {
        let pre = ids(idx, q).await;
        assert_eq!(
            &pre, exp,
            "{label}: PRE-flush (memtable) hit set wrong for {q}"
        );
        assert_eq!(
            total(idx, q).await,
            exp.len() as u64,
            "{label}: PRE-flush (memtable) hits.total wrong for {q}"
        );
    }
    idx.flush().await.unwrap();
    for ((q, _, label), exp) in cases.iter().zip(&expected) {
        let post = ids(idx, q).await;
        assert_eq!(
            &post, exp,
            "{label}: POST-flush (segment) hit set wrong for {q}"
        );
        assert_eq!(
            total(idx, q).await,
            exp.len() as u64,
            "{label}: POST-flush (segment) hits.total wrong for {q}"
        );
    }
}

/// One index with an explicit mapping — the whole point of the issue is that
/// the mapping must reach the stored-source scan, so a dynamic schema would
/// test nothing.
///
/// `tags` is `keyword`, `body` is `text` (the over-correction control),
/// `when` is `date` and `code` is `long` (the two types whose current handling
/// must survive untouched).
async fn seed(engine: &Engine, name: &str) -> std::sync::Arc<Index> {
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("tags", FieldType::Keyword));
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("when", FieldType::Date));
    schema
        .fields
        .push(FieldConfig::new("code", FieldType::Long));
    engine.create_index(name, schema).unwrap();
    let idx = engine.get_index(name).unwrap();

    // Scalar keyword values only — no arrays anywhere, so nothing here depends
    // on #332 (multi-valued keyword fields flattened to one FTS token).
    idx.index_document(
        Some("1".into()),
        json!({"tags": "red", "body": "red things",
               "when": "2021-04-01T00:00:00Z", "code": 42}),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("2".into()),
        json!({"tags": "red blue", "body": "blue things",
               "when": "2022-07-09T00:00:00Z", "code": 7}),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("3".into()),
        json!({"tags": "Red", "body": "capital things",
               "when": "2023-01-02T00:00:00Z", "code": 9}),
    )
    .await
    .unwrap();
    idx
}

/// The issue's own reproducer, generalised: a multi-token query against a
/// scalar keyword value must match only the document whose WHOLE value is that
/// string. Pre-fix the memtable token-OR'd the query and admitted `"red"` and
/// `"Red"` as well, so every one of these returned 3 hits before `_flush` and 1
/// after.
#[tokio::test]
async fn multi_token_query_on_keyword_field_is_whole_value_in_both_phases() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "kw_multi_token").await;

    assert_flush_parity(
        &idx,
        &[
            (
                json!({"match": {"tags": "red blue"}}),
                &["2"],
                "match, multi-token, keyword field",
            ),
            (
                json!({"multi_match": {"query": "red blue", "fields": ["tags"]}}),
                &["2"],
                "multi_match, multi-token, keyword field",
            ),
            (
                json!({"term": {"tags": "red"}}),
                &["1"],
                "term control — always was whole-value",
            ),
            // The keyword clause nested inside a filter context: the container
            // arms recurse into the same evaluator, so this must converge too
            // (a rewrite that only touched the top-level clause would not).
            (
                json!({"bool": {"filter": [{"match": {"tags": "red blue"}}]}}),
                &["2"],
                "match on keyword inside bool.filter",
            ),
            (
                json!({"bool": {"must": [{"match": {"tags": "red blue"}}],
                                "must_not": [{"term": {"code": 7}}]}}),
                &[],
                "bool must + must_not over a keyword match",
            ),
        ],
    )
    .await;
}

/// SINGLE-token queries diverged too, which the issue title does not say: the
/// memtable tokenised the DOCUMENT as well, so `match {tags: "red"}` also
/// matched the doc whose keyword value is `"red blue"`. And because the keyword
/// analyzer preserves case, `"Red"` and `"red"` are different terms — the
/// memtable lowercased both sides and conflated them.
#[tokio::test]
async fn single_token_and_case_on_keyword_field_are_whole_value_in_both_phases() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "kw_single_token").await;

    assert_flush_parity(
        &idx,
        &[
            (
                json!({"match": {"tags": "red"}}),
                &["1"],
                "match, single token — must not match the \"red blue\" doc",
            ),
            (
                json!({"multi_match": {"query": "red", "fields": ["tags"]}}),
                &["1"],
                "multi_match, single token",
            ),
            (
                json!({"match": {"tags": "Red"}}),
                &["3"],
                "match, case-preserved — keyword analyzer does not lowercase",
            ),
            (
                json!({"match": {"tags": "blue"}}),
                &[],
                "match on a token that is only ever a SUBSTRING of a keyword value",
            ),
        ],
    )
    .await;
}

/// A mixed field list must converge per FIELD, not per clause: the keyword
/// field contributes a whole-value hit and the text field keeps its analyzed
/// token OR, in both phases. This is the shape a blanket "the whole clause goes
/// whole-value" fix would break.
#[tokio::test]
async fn mixed_keyword_and_text_field_list_converges_per_field() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "kw_mixed").await;

    assert_flush_parity(
        &idx,
        &[
            (
                json!({"multi_match": {"query": "red blue", "fields": ["tags", "body"]}}),
                // tags: whole value "red blue" → doc 2.
                // body: analyzed OR over {red, blue} → "red things" + "blue things".
                &["1", "2"],
                "multi_match over [keyword, text]",
            ),
            (
                json!({"multi_match": {"query": "red blue", "fields": ["tags", "body"],
                                       "type": "cross_fields"}}),
                &["1", "2"],
                "cross_fields over [keyword, text]",
            ),
        ],
    )
    .await;
}

/// Over-correction guards. Nothing outside `FieldType::Keyword` may change:
/// a `text` field keeps analyzed token semantics, a `long` field keeps
/// comparing by rendered value, and `match_phrase` keeps its positional walk
/// (the segment declines the phrase projection for exact fields and runs the
/// very same predicate, so parity there is pre-existing and must be preserved).
#[tokio::test]
async fn text_numeric_and_phrase_paths_are_untouched() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "kw_controls").await;

    assert_flush_parity(
        &idx,
        &[
            (
                json!({"match": {"body": "red blue"}}),
                &["1", "2"],
                "text field still token-ORs",
            ),
            (
                json!({"match": {"code": 42}}),
                &["1"],
                "long field still compares by rendered value",
            ),
            (
                json!({"match_phrase": {"tags": "red blue"}}),
                &["2"],
                "match_phrase on a keyword field keeps the positional walk",
            ),
        ],
    )
    .await;
}

/// `date` is the field type whose polarity is the OPPOSITE of `keyword`, and
/// this test exists to make that loud.
///
/// ES parses `match {when: "2021-04-01"}` as a date and matches the document
/// stored as `"2021-04-01T00:00:00Z"`. The stored-source scan already gets that
/// right via its date short-circuit, so the PRE-flush answer is the correct
/// one here — the reverse of #354. Widening the keyword whole-value rule to the
/// whole `exact_fields` set (keyword + numeric + date + bool + ip) would
/// destroy this correct half and turn a segment-only defect into a total one,
/// so the pre-flush expectation is pinned.
///
/// Only the pre-flush side is asserted on purpose: the post-flush answer is
/// currently EMPTY (the segment projects a whole-value term and never parses
/// the date), which is a separate segment-side defect that #354 does not cover.
/// Asserting parity here would fail for the wrong reason.
#[tokio::test]
async fn date_field_keeps_its_date_aware_pre_flush_semantics() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "kw_date_guard").await;

    let hit = ids(&idx, &json!({"match": {"when": "2021-04-01"}})).await;
    assert_eq!(
        hit,
        BTreeSet::from(["1".to_string()]),
        "PRE-flush `match` on a date field must stay date-aware — if this went \
         whole-value, the keyword fix has been over-generalised to exact_fields"
    );
}

/// `query_string` reaches the keyword field two ways and both must be
/// whole-value: through the parser's `field:value` lowering (which produces a
/// `Match` node) and through a `default_field` on the opaque node.
#[tokio::test]
async fn query_string_on_keyword_field_is_whole_value_in_both_phases() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "kw_query_string").await;

    assert_flush_parity(
        &idx,
        &[
            (
                json!({"query_string": {"query": "tags:red"}}),
                &["1"],
                "query_string field:value on a keyword field",
            ),
            (
                json!({"query_string": {"default_field": "tags", "query": "red"}}),
                &["1"],
                "query_string default_field on a keyword field",
            ),
            (
                json!({"query_string": {"query": "tags:Red"}}),
                &["3"],
                "query_string field:value keeps the keyword analyzer's case",
            ),
            // Deliberately NOT covered here: `tags:"red blue"`. The
            // `query_string` tokenizer lowers a quoted value to
            // `MatchPhrase {query: "red  blue"}` — note the doubled space — and
            // that shape diverges at `_flush` for reasons that have nothing to
            // do with the mapping. Asserting it would fail for the wrong reason.
            // `match_phrase {tags: "red blue"}` written directly is covered by
            // `text_numeric_and_phrase_paths_are_untouched`.
        ],
    )
    .await;
}
