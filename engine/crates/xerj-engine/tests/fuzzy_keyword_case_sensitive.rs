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

    let fz = |v: &str| json!({ "fuzzy": { "name": { "value": v, "fuzziness": 0 } } });
    let fz_ci = json!({ "fuzzy": { "name": { "value": "hello", "fuzziness": 0, "case_insensitive": true } } });

    // Buffered (memtable) answers, then flush and re-query the segment — every
    // pair must agree (flush-invariance — the #668 lesson).
    let pre_lower = hits(&engine, fz("hello")).await;
    let pre_cased = hits(&engine, fz("Hello")).await;
    let pre_ci = hits(&engine, fz_ci.clone()).await;
    idx.refresh().await.expect("refresh");
    let post_lower = hits(&engine, fz("hello")).await;
    let post_cased = hits(&engine, fz("Hello")).await;
    let post_ci = hits(&engine, fz_ci.clone()).await;

    assert_eq!(
        pre_lower, post_lower,
        "#423/#406: keyword fuzzy must be flush-invariant"
    );
    assert_eq!(
        pre_cased, post_cased,
        "#423/#406: keyword fuzzy must be flush-invariant"
    );
    assert_eq!(
        pre_ci, post_ci,
        "#423/#406: fuzzy case_insensitive must be flush-invariant"
    );

    // ES default: case-sensitive → a lower-case value does not fuzzy-match a
    // capitalized keyword at fuzziness 0; the correctly-cased one does; and the
    // explicit `case_insensitive: true` still folds.
    assert_eq!(
        post_lower, 0,
        "#423/#406: keyword fuzzy must be case-sensitive by default"
    );
    assert_eq!(post_cased, 1, "case-correct fuzzy matches");
    assert_eq!(
        post_ci, 1,
        "#423/#406: fuzzy with case_insensitive true still folds"
    );
}
