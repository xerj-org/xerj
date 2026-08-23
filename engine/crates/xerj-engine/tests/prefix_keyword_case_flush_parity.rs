//! Issue #396/#362/#398: a `prefix` query on a `keyword` field must return the
//! same hit set before and after a flush. ES matches `prefix` against the RAW
//! keyword value (case-SENSITIVE), so `prefix:{name:"hel"}` on `"Hello"` is 0.
//! The flushed segment answers 0 (correct); the buffered (memtable) matcher —
//! `doc_matches_query`'s Prefix arm — lowercases both sides (index.rs:35672,
//! "Without schema info at this layer we check both"), so it answers 1. Same
//! document, same query, two answers across a background flush.
//!
//! This is a #423-core manifestation (schemaless `doc_matches_query`): the fix
//! makes the Prefix arm schema-aware (keyword => case-sensitive raw compare,
//! matching the segment; text => analyzed-token semantics). A blanket
//! case-sensitive memtable prefix would break TEXT prefix (segment matches the
//! lowercased token stream), so schema is required. IGNORED until that fix.

use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::Engine;
use xerj_query::parse_request;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

async fn prefix_hits(engine: &Engine, p: &str) -> u64 {
    let idx = engine.get_index("docs").expect("get index");
    let req = parse_request(&json!({ "query": { "prefix": { "name": p } }, "size": 0 }))
        .expect("parse_request");
    idx.search(&req).await.expect("search").total.value
}

/// `prefix` on a `keyword` field is flush-invariant. FAIL-BEFORE: the buffered
/// matcher folds case so `prefix:hel` on `"Hello"` is 1 pre-flush but 0 after
/// flush (the segment is case-sensitive, which is ES-correct for `keyword`).
#[tokio::test]
#[ignore = "#396/#423: memtable prefix folds case for keyword; un-ignored by the schema-aware fix"]
async fn prefix_keyword_case_is_flush_invariant() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("name", FieldType::Keyword));
    engine.create_index("docs", schema).expect("create");

    let idx = engine.get_index("docs").expect("get");
    idx.index_document(Some("d0".to_string()), json!({ "name": "Hello" }))
        .await
        .expect("index");

    // Lowercase prefix against a capitalized keyword, document still buffered.
    let pre = prefix_hits(&engine, "hel").await;
    idx.refresh().await.expect("refresh");
    let post = prefix_hits(&engine, "hel").await;

    assert_eq!(
        pre, post,
        "#396: `prefix:hel` on keyword \"Hello\" must be flush-invariant \
         (pre-flush={pre}, post-flush={post}); the memtable Prefix arm folds \
         case while the segment is case-sensitive"
    );
    assert_eq!(
        post, 0,
        "ES: keyword prefix is case-sensitive, so `hel` != `Hello`"
    );
}

/// Companion: `prefix` on a `text` field is also flush-invariant. The segment
/// matches the LOWERCASED token with a RAW (un-lowercased) query, so an
/// uppercase prefix `"Hel"` on `"Hello World"` is 0; the memtable folds both
/// sides and answers 1. FAIL-BEFORE: pre-flush=1, post-flush=0.
#[tokio::test]
#[ignore = "#396/#423: memtable prefix folds the query for text too; un-ignored by the schema-aware fix"]
async fn prefix_text_case_is_flush_invariant() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    engine.create_index("docs", schema).expect("create");

    let idx = engine.get_index("docs").expect("get");
    idx.index_document(Some("d0".to_string()), json!({ "body": "Hello World" }))
        .await
        .expect("index");

    // Uppercase prefix: the segment token is lowercased but the query is not,
    // so `"Hel"` does not prefix `"hello"`.
    let req = |p: &str| {
        parse_request(&json!({ "query": { "prefix": { "body": p } }, "size": 0 })).unwrap()
    };
    let pre = idx.search(&req("Hel")).await.unwrap().total.value;
    idx.refresh().await.expect("refresh");
    let post = idx.search(&req("Hel")).await.unwrap().total.value;

    assert_eq!(
        pre, post,
        "#396: `prefix:Hel` on text \"Hello World\" must be flush-invariant \
         (pre-flush={pre}, post-flush={post})"
    );
    assert_eq!(
        post, 0,
        "segment lowercases the token but not the query: `Hel` !prefix `hello`"
    );
}
