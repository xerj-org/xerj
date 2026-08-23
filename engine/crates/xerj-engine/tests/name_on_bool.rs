//! Issue #681 (bool half): `_name` on a `bool` clause (a top-level sibling of
//! `must`/`should`/…) was silently dropped by `parse_bool` (it never read
//! `_name` or wrapped the node), so a named bool never surfaced in
//! `matched_queries`. The fix reads `_name` and wraps via `maybe_named`.

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
async fn name_on_bool_surfaces_in_matched_queries() {
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

    // A named `bool` that matches (via match_all) must report "b".
    let m = matched(
        &engine,
        json!({ "bool": { "should": [{ "match_all": {} }], "_name": "b" } }),
    )
    .await;
    assert_eq!(m.len(), 1, "the bool matches -> one hit");
    assert!(
        m[0].contains(&"b".to_string()),
        "#681: a named bool must surface its `_name` in matched_queries: {:?}",
        m[0]
    );
}
