//! Regression tests for issue #218: the matched SET of a multi-token
//! `match`/`multi_match` must be identical before and after `flush()`.
//!
//! Pre-fix, the memtable membership test for `multi_match` (best_fields /
//! most_fields, default operator) was `field_text.contains(whole_query)` —
//! whole-query substring containment, i.e. every token adjacent and in
//! order — while the segment path lowered the same query to a per-field
//! OR-bool over tokens (ES default `operator: or`). A doc containing only
//! SOME of the query tokens was therefore invisible until `_flush`, and
//! the hit set changed with flush timing. The reverse divergence existed
//! for `operator: and` and the phrase types: the segment FTS projection
//! ignored them and OR'd the tokens, so post-flush over-matched.
//!
//! Every test seeds one index, runs ALL its queries against the memtable
//! (pre-flush), flushes ONCE, re-runs the same queries against the segment
//! (post-flush), and asserts each query's matched id set is the expected
//! one in BOTH states — i.e. flush timing never changes the hit set.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
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

/// Run every (query, expected-ids, label) case against the memtable,
/// flush once, run every case again against the segment, and assert the
/// hit set is the expected one in both states.
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
    }
    idx.flush().await.unwrap();
    for ((q, _, label), exp) in cases.iter().zip(&expected) {
        let post = ids(idx, q).await;
        assert_eq!(
            &post, exp,
            "{label}: POST-flush (segment) hit set wrong for {q}"
        );
    }
}

/// One index, two docs. Doc 1 is the reproducer from issue #218; doc 2 is
/// a control that must never match any query in these tests.
async fn seed(engine: &Engine, name: &str) -> std::sync::Arc<Index> {
    engine.create_index(name, Schema::empty()).unwrap();
    let idx = engine.get_index(name).unwrap();
    idx.index_document(
        Some("1".into()),
        json!({
            "body": "the log merge policy groups segments into size buckets",
            "title": "merge policy"
        }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("2".into()),
        json!({"body": "quick brown fox", "title": "animals"}),
    )
    .await
    .unwrap();
    idx
}

/// Issue #218 reproducer: multi-token multi_match with the default
/// operator (OR) — one token present, one absent. ES returns the doc in
/// both states; pre-fix the memtable returned 0 hits.
#[tokio::test]
async fn multi_token_or_default_hit_set_stable_across_flush() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mmp_or").await;

    assert_flush_parity(
        &idx,
        &[
            (
                json!({"multi_match": {"query": "merge zzzznotpresent", "fields": ["body"]}}),
                &["1"],
                "single-field OR",
            ),
            (
                json!({"multi_match": {"query": "merge zzzznotpresent",
                                       "fields": ["body", "title"]}}),
                &["1"],
                "multi-field OR",
            ),
            (
                json!({"multi_match": {"query": "merge zzzznotpresent",
                                       "fields": ["body", "title"], "type": "most_fields"}}),
                &["1"],
                "most_fields OR",
            ),
            // `match` on the same field must agree with multi_match(["field"]).
            (
                json!({"match": {"body": "merge zzzznotpresent"}}),
                &["1"],
                "match OR",
            ),
        ],
    )
    .await;
}

/// The memtable hit must also carry a non-zero score: pre-fix,
/// `score_query_against_doc` used the same whole-query substring test, so
/// a doc admitted by the (fixed) membership check would still score 0.0
/// and be dropped by scored paths / rescore.
#[tokio::test]
async fn memtable_multi_match_or_hit_scores_nonzero() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mmp_score").await;

    let r = idx
        .search(&req(json!({
            "multi_match": {"query": "merge zzzznotpresent", "fields": ["body", "title"]}
        })))
        .await
        .unwrap();
    assert_eq!(r.total.value, 1, "memtable OR hit count");
    assert!(
        r.hits[0].score > 0.0,
        "memtable multi_match OR hit scored {} (expected > 0)",
        r.hits[0].score
    );
}

/// Explicit `operator: and` requires EVERY token — in both states.
/// Pre-fix the segment FTS projection dropped the operator and OR'd the
/// tokens, so the absent-token query matched post-flush.
#[tokio::test]
async fn operator_and_hit_set_stable_across_flush() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mmp_and").await;

    assert_flush_parity(
        &idx,
        &[
            // One token absent → no match, before AND after flush.
            (
                json!({"multi_match": {"query": "merge zzzznotpresent", "fields": ["body"],
                                       "operator": "and"}}),
                &[],
                "AND absent token",
            ),
            // All tokens present (non-adjacent, out of order) → match in both.
            (
                json!({"multi_match": {"query": "buckets merge", "fields": ["body"],
                                       "operator": "and"}}),
                &["1"],
                "AND all tokens present",
            ),
        ],
    )
    .await;
}

/// cross_fields + operator:and pools tokens ACROSS fields: doc 1 has
/// "policy" only in `title` and "buckets" only in `body`, so no single
/// field holds both tokens but the pooled view does. Pre-fix the segment
/// path OR'd the tokens (over-matching the absent-token query below).
#[tokio::test]
async fn cross_fields_and_hit_set_stable_across_flush() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("mmp_cross", Schema::empty()).unwrap();
    let idx = engine.get_index("mmp_cross").unwrap();
    idx.index_document(
        Some("1".into()),
        json!({"body": "groups segments into size buckets", "title": "merge policy"}),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("2".into()),
        json!({"body": "quick brown fox", "title": "animals"}),
    )
    .await
    .unwrap();

    assert_flush_parity(
        &idx,
        &[
            // Tokens scattered across fields → pooled AND matches.
            (
                json!({"multi_match": {"query": "policy buckets", "fields": ["body", "title"],
                                       "type": "cross_fields", "operator": "and"}}),
                &["1"],
                "cross AND pooled",
            ),
            // One token absent anywhere → no match in either state.
            (
                json!({"multi_match": {"query": "policy zzzznotpresent",
                                       "fields": ["body", "title"],
                                       "type": "cross_fields", "operator": "and"}}),
                &[],
                "cross AND absent token",
            ),
            // SINGLE-token cross_fields + AND keeps the FTS projection (the
            // decline is gated on >1 analyzed token): with one token,
            // "present in the pooled text" and "present in at least one
            // field" — what the OR'd per-field clauses mean — are the same
            // predicate. Both states must agree anyway.
            (
                json!({"multi_match": {"query": "policy", "fields": ["body", "title"],
                                       "type": "cross_fields", "operator": "and"}}),
                &["1"],
                "cross AND single token present",
            ),
            (
                json!({"multi_match": {"query": "zzzznotpresent", "fields": ["body", "title"],
                                       "type": "cross_fields", "operator": "and"}}),
                &[],
                "cross AND single token absent",
            ),
        ],
    )
    .await;
}

/// `type: phrase` requires the tokens contiguous and in order — in both
/// states. Pre-fix the segment projection OR'd the tokens, so a reversed
/// phrase matched post-flush.
#[tokio::test]
async fn phrase_type_hit_set_stable_across_flush() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "mmp_phrase").await;

    assert_flush_parity(
        &idx,
        &[
            // In-order contiguous phrase → matches in both states.
            (
                json!({"multi_match": {"query": "merge policy", "fields": ["body", "title"],
                                       "type": "phrase"}}),
                &["1"],
                "phrase in order",
            ),
            // Reversed order → no match in either state.
            (
                json!({"multi_match": {"query": "policy merge", "fields": ["body", "title"],
                                       "type": "phrase"}}),
                &[],
                "phrase reversed",
            ),
        ],
    )
    .await;
}

/// Guard for the token-equality tightening: a single-token multi_match
/// must NOT substring-match inside a longer word (`jump` vs
/// "jumparound") — the segment term path never did; pre-fix the memtable
/// substring path did.
#[tokio::test]
async fn single_token_multi_match_is_token_equality_not_substring() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("mmp_tok", Schema::empty()).unwrap();
    let idx = engine.get_index("mmp_tok").unwrap();
    idx.index_document(Some("1".into()), json!({"body": "jumparound artist"}))
        .await
        .unwrap();
    idx.index_document(Some("2".into()), json!({"body": "big jump today"}))
        .await
        .unwrap();

    assert_flush_parity(
        &idx,
        &[(
            json!({"multi_match": {"query": "jump", "fields": ["body"]}}),
            &["2"],
            "single token equality",
        )],
    )
    .await;
}
