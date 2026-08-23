//! Issue #669 (follow-up to #396/#667): the `matched_queries` (`_name`) channel
//! must decide a named clause with the SAME schema-aware matcher hit-SELECTION
//! uses. `collect_matched_queries_inner` called the 2-arg schemaless
//! `doc_matches_query` shim (fold-both), so a named keyword `prefix`/`term`
//! clause could report a `_name` the hit set never granted, and disagree
//! pre- vs post-flush. The fix threads the index schema through so the `_name`
//! annotation matches selection.

use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::Engine;
use xerj_query::parse_request;

async fn matched(engine: &Engine, q: serde_json::Value) -> Vec<Vec<String>> {
    let idx = engine.get_index("docs").expect("get index");
    let req = parse_request(&json!({ "query": q, "size": 10 })).expect("parse_request");
    idx.search(&req)
        .await
        .expect("search")
        .hits
        .iter()
        .map(|h| h.matched_queries.clone())
        .collect()
}

#[tokio::test]
async fn matched_queries_name_is_schema_aware_and_flush_invariant() {
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

    // A `function_score` whose function carries `_name` and a keyword-`prefix`
    // FILTER. `match_all` makes the doc a hit; the function's `_name` "fp" is
    // reported only if its filter matches. Keyword prefix is case-SENSITIVE, so
    // "Hello" does not start with "hel" — the filter must NOT match and "fp"
    // must NOT appear (the schemaless fold-shim wrongly folded and reported it).
    let q = json!({
        "function_score": {
            "query": { "match_all": {} },
            "functions": [
                { "filter": { "prefix": { "name": "hel" } }, "weight": 2.0, "_name": "fp" }
            ]
        }
    });

    let pre = matched(&engine, q.clone()).await;
    idx.refresh().await.expect("refresh");
    let post = matched(&engine, q.clone()).await;

    assert_eq!(pre, post, "#669: matched_queries must be flush-invariant");
    assert_eq!(pre.len(), 1, "one hit (match_all)");
    assert!(
        !pre[0].contains(&"fp".to_string()),
        "#669: keyword-prefix filter is case-sensitive; the function `_name` must agree with hit selection (no 'fp'): {:?}",
        pre[0]
    );
}
