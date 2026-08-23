//! Issue #668: `wildcard` on a `keyword` field must be case-SENSITIVE (ES
//! default — the pattern is not analysed). `wildcard:{name:"hell*"}` on
//! keyword `"Hello"` must NOT match. XERJ currently folds case on both the
//! memtable and segment paths, so it matches (wrong result — flush-invariant
//! but ES-incorrect).
//!
//! CONSTRAINT (why a blanket flip is wrong): XERJ's parser rewrites
//! `term{case_insensitive:true}` INTO a `Wildcard` node and relies on the
//! matcher folding case (index.rs:41061-41065). So the fix must thread a
//! `case_insensitive` flag through `QueryNode::Wildcard`: genuine wildcard =>
//! case-sensitive; rewritten term-ci => still folds. This test pins BOTH.
//!
//! IGNORED until the flag is threaded (AST + parser + memtable arm + segment
//! FTS route index.rs:41071 + DV path index.rs:22550).

use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::Engine;
use xerj_query::parse_request;

async fn hits(engine: &Engine, q: serde_json::Value) -> u64 {
    let idx = engine.get_index("docs").expect("get index");
    let req = parse_request(&json!({ "query": q, "size": 0 })).expect("parse_request");
    idx.search(&req).await.expect("search").total.value
}

#[tokio::test]
#[ignore = "#668: keyword wildcard folds case; un-ignored when the case_insensitive flag is threaded"]
async fn keyword_wildcard_is_case_sensitive_but_term_ci_still_folds() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config).expect("engine");

    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("name", FieldType::Keyword));
    engine.create_index("docs", schema).expect("create");
    let idx = engine.get_index("docs").expect("get");
    idx.index_document(Some("d0".to_string()), json!({ "name": "Hello" }))
        .await
        .expect("index");
    idx.refresh().await.expect("refresh");

    // ES-correct: keyword wildcard is case-sensitive, so a lower-case pattern
    // does not match a capitalized value. (Currently returns 1 — the bug.)
    assert_eq!(
        hits(&engine, json!({ "wildcard": { "name": "hell*" } })).await,
        0,
        "#668: keyword `wildcard` must be case-sensitive"
    );
    // Sanity: a correctly-cased pattern still matches.
    assert_eq!(
        hits(&engine, json!({ "wildcard": { "name": "Hell*" } })).await,
        1,
        "case-correct wildcard still matches"
    );
    // CONSTRAINT: `term{case_insensitive:true}` (rewritten to a Wildcard) must
    // KEEP folding — the fix must not break this passing path.
    assert_eq!(
        hits(
            &engine,
            json!({ "term": { "name": { "value": "hello", "case_insensitive": true } } })
        )
        .await,
        1,
        "#668: term with case_insensitive true must still fold after the fix"
    );
}
