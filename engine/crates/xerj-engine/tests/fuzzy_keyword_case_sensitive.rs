//! Issue #423 (symptom #406): `fuzzy` on a `keyword` field must be
//! case-SENSITIVE by default (ES: `case_insensitive` defaults to false) — the
//! value is matched against the raw term dictionary, so `fuzzy{value:"hello",
//! fuzziness:0}` on `"Hello"` must NOT match. XERJ folds case on both the
//! memtable and segment paths (the FST fuzzy route hardcodes
//! `case_insensitive: true`, index.rs:~41133), so it matches (wrong result).
//!
//! CONSTRAINT (like keyword wildcard, #668): a `fuzzy{case_insensitive:true}`
//! must still fold. `QueryNode::Fuzzy` carries no `case_insensitive` flag today,
//! so the fix threads one through AST + parser + the memtable Fuzzy arm + the
//! FTS route, exactly as #668 did for `Wildcard`.
//!
//! IGNORED until that fix; un-ignored by it.

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
#[ignore = "#423/#406: fuzzy ignores case_insensitive (always folds); un-ignored by the fix"]
async fn fuzzy_keyword_is_case_sensitive_but_ci_flag_still_folds() {
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

    // ES default: case-sensitive → a lower-case value does not fuzzy-match a
    // capitalized keyword at fuzziness 0. (Currently returns 1 — the bug.)
    assert_eq!(
        hits(&engine, json!({ "fuzzy": { "name": { "value": "hello", "fuzziness": 0 } } })).await,
        0,
        "#423/#406: keyword fuzzy must be case-sensitive by default"
    );
    // Correct case still matches.
    assert_eq!(
        hits(&engine, json!({ "fuzzy": { "name": { "value": "Hello", "fuzziness": 0 } } })).await,
        1,
        "case-correct fuzzy matches"
    );
    // The explicit flag still folds.
    assert_eq!(
        hits(
            &engine,
            json!({ "fuzzy": { "name": { "value": "hello", "fuzziness": 0, "case_insensitive": true } } })
        )
        .await,
        1,
        "#423/#406: fuzzy with case_insensitive true still folds"
    );
}
