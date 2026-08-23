//! Issue #677 (an #423 sibling of #413): `term` on an object **sub-field**
//! changes its answer at flush. Field `meta` mapped `object`, document
//! `{"meta":{"owner":"Alpha"}}`:
//!
//! | query | before flush | after flush |
//! |---|---|---|
//! | `{"term":{"meta.owner":"Alpha"}}` | 0 | 1 |
//!
//! The segment indexes `meta.owner` as its own term (via `collect_text_fields`
//! recursion), so post-flush answers 1; the memtable's `term` path returns 0
//! for the dotted sub-field. ES answers 1 consistently. Ignored until the fix
//! makes the memtable term path resolve object sub-fields; the fix un-ignores it.

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
#[ignore = "#677: memtable term path does not resolve object sub-fields; un-ignored by the fix"]
async fn term_on_object_subfield_is_flush_invariant() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config).expect("engine");

    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("meta", FieldType::Object));
    engine.create_index("docs", schema).expect("create");
    let idx = engine.get_index("docs").expect("get");
    idx.index_document(Some("d0".to_string()), json!({ "meta": {"owner":"Alpha"} }))
        .await
        .expect("index");

    let q = json!({"term":{"meta.owner":"Alpha"}});
    let pre = hits(&engine, q.clone()).await;
    idx.refresh().await.expect("refresh");
    let post = hits(&engine, q.clone()).await;

    assert_eq!(pre, post, "#677: term on an object sub-field must be flush-invariant");
    assert_eq!(post, 1, "#677: meta.owner is an indexed keyword sub-field (ES matches)");
}
