//! Issue #681: `_name` on the expanded form of a `prefix`/`wildcard` clause
//! (e.g. `{"prefix":{"name":{"value":"Hel","_name":"p"}}}`) was dropped by the
//! parser (`parse_prefix`/`parse_wildcard` never called `maybe_named`), so a
//! named prefix/wildcard clause never surfaced in `matched_queries`. The fix
//! extracts `_name` and wraps the node, mirroring `parse_term`.

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
async fn name_on_prefix_and_wildcard_surfaces_in_matched_queries() {
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

    // A named `prefix` that matches ("Hel" prefixes "Hello") must report "p".
    let pre = matched(
        &engine,
        json!({ "prefix": { "name": { "value": "Hel", "_name": "p" } } }),
    )
    .await;
    assert_eq!(pre.len(), 1, "the prefix matches -> one hit");
    assert!(
        pre[0].contains(&"p".to_string()),
        "#681: a named prefix must surface its `_name` in matched_queries: {:?}",
        pre[0]
    );

    // A named `wildcard` that matches ("Hel*") must report "w".
    let wc = matched(
        &engine,
        json!({ "wildcard": { "name": { "value": "Hel*", "_name": "w" } } }),
    )
    .await;
    assert_eq!(wc.len(), 1, "the wildcard matches -> one hit");
    assert!(
        wc[0].contains(&"w".to_string()),
        "#681: a named wildcard must surface its `_name` in matched_queries: {:?}",
        wc[0]
    );
}
