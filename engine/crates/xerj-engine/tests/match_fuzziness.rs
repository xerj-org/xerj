//! Issue #3172: `fuzziness` on `match` and `multi_match` has no effect,
//! `parse_match` never read `fuzziness` (or `prefix_length`) and always
//! built a plain `QueryNode::Match`, so a typo-tolerant search returned
//! nothing while the standalone `fuzzy` query returned the expected hits.
//! Same for the plain (field-centric / term-centric) `multi_match` types;
//! only `match_bool_prefix` and bool_prefix `multi_match` built `Fuzzy`
//! leaves per token.
//!
//! Covered here end to end for a text field, each asserted flush-invariant:
//! every query is answered buffered AND flushed, and the two must agree.
//! The typo cases fail (0 hits) before the fix and pass after it.

use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::Engine;
use xerj_query::parse_request;

async fn hits(engine: &Engine, q: &serde_json::Value) -> u64 {
    let idx = engine.get_index("docs").expect("get index");
    let req = parse_request(&json!({ "query": q, "size": 0 })).expect("parse_request");
    idx.search(&req).await.expect("search").total.value
}

#[tokio::test]
async fn match_and_multi_match_honour_fuzziness() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config).expect("engine");

    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("title", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    engine.create_index("docs", schema).expect("create");
    let idx = engine.get_index("docs").expect("get");
    idx.index_document(
        Some("d0".to_string()),
        json!({ "title": "quick brown fox", "body": "lazy dog sleeps" }),
    )
    .await
    .expect("index");
    idx.index_document(
        Some("d1".to_string()),
        json!({ "title": "slow green turtle", "body": "quick red fox" }),
    )
    .await
    .expect("index");

    // (query, expected_hits, label)
    let cases = [
        (
            json!({ "match": { "title": "quick" } }),
            1,
            "plain match still finds the exact term",
        ),
        (
            json!({ "match": { "title": { "query": "quikc", "fuzziness": "AUTO" } } }),
            1,
            "#3172: match AUTO fuzziness finds the transposed typo",
        ),
        (
            json!({ "match": { "title": { "query": "qwick", "fuzziness": 1 } } }),
            1,
            "#3172: match fuzziness 1 finds the one-edit typo",
        ),
        (
            json!({ "match": { "title": "quikc" } }),
            0,
            "control: the same typo without fuzziness still matches nothing",
        ),
        (
            json!({ "match": { "title": { "query": "quikc brown", "fuzziness": 1 } } }),
            1,
            "#3172: multi-token fuzzy match (OR) finds the doc",
        ),
        (
            json!({ "match": { "title": { "query": "quikc brown", "operator": "and", "fuzziness": 1 } } }),
            1,
            "#3172: multi-token fuzzy match with operator AND finds the doc",
        ),
        (
            json!({ "match": { "title": { "query": "quikc zzzzz", "operator": "and", "fuzziness": 1 } } }),
            0,
            "operator AND still requires every fuzzy token to match",
        ),
        (
            json!({ "match": { "title": { "query": "auick", "fuzziness": 1, "prefix_length": 1 } } }),
            0,
            "#3172: prefix_length 1 blocks a typo in the first character",
        ),
        (
            json!({ "match": { "title": { "query": "auick", "fuzziness": 1 } } }),
            1,
            "same typo with prefix_length 0 matches",
        ),
        (
            json!({ "multi_match": { "query": "quikc", "fields": ["title", "body"], "fuzziness": 1 } }),
            2,
            "#3172: best_fields multi_match with fuzziness finds both fields",
        ),
        (
            json!({ "multi_match": { "query": "quikc", "type": "most_fields", "fields": ["title", "body"], "fuzziness": "AUTO" } }),
            2,
            "#3172: most_fields multi_match with AUTO fuzziness finds both fields",
        ),
        (
            json!({ "multi_match": { "query": "quikc", "type": "cross_fields", "fields": ["title", "body"], "fuzziness": 1 } }),
            2,
            "#3172: cross_fields multi_match with fuzziness finds both fields",
        ),
        (
            json!({ "multi_match": { "query": "quikc", "fields": ["title", "body"] } }),
            0,
            "control: multi_match typo without fuzziness still matches nothing",
        ),
    ];

    // Answer every case BUFFERED first (memtable path), then flush ONCE and
    // answer every case again (segment path), so both paths are exercised for
    // every query, not just the first.
    let mut pre = Vec::new();
    for (q, _, _) in &cases {
        pre.push(hits(&engine, q).await);
    }
    idx.refresh().await.expect("refresh");
    for (i, (q, expected, label)) in cases.iter().enumerate() {
        let post = hits(&engine, q).await;
        assert_eq!(
            pre[i], post,
            "#3172: match/multi_match fuzziness must be flush-invariant: {label}"
        );
        assert_eq!(post, *expected, "#3172: {label}");
    }
}
