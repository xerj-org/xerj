//! Issue #423 (symptom #408): a `term` query on a `keyword` ARRAY field must
//! return the same hit set before and after a flush. A document with
//! `"tags": ["red", "green", "blue"]` answers `term: {tags: "green"}` with 1
//! hit once flushed, but 0 hits while still buffered — only the FIRST array
//! element ("red") matches pre-flush. Same document, same query, two answers,
//! decided by a background flush the caller cannot see.
//!
//! ROOT CAUSE (confirmed): NOT `doc_matches_query` — `json_values_equal` already
//! matches any array element, and `size:5` returned the doc correctly. The miss
//! was in the size:0 COUNT path: `try_shortcut_count`'s bare-`term` keyword
//! shortcut counted the memtable via `doc_values_keyword_count`, whose
//! single-valued column stores only the FIRST array element (see `push_field`),
//! and — unlike `doc_values_term_query` / `doc_values_bool_hits` — it lacked the
//! array bailout. It returned a confident `Some(0)` that overwrote the correct
//! scan count. The fix adds that bailout (`term_count_needs_source_scan`) so the
//! shortcut abandons to the stored-source scan for array/whitespace fields.

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

async fn term_tag_hits(engine: &Engine, tag: &str) -> u64 {
    let idx = engine.get_index("docs").expect("get index");
    let req = parse_request(&json!({
        "query": { "term": { "tags": tag } },
        "size": 0,
        "from": 0
    }))
    .expect("parse_request");
    idx.search(&req).await.expect("search").total.value
}

/// `term` on a `keyword` array is flush-invariant: every element is matchable
/// whether the document is buffered or flushed. FAIL-BEFORE: the second array
/// element ("green") matches only after flush, so pre-flush=0, post-flush=1.
#[tokio::test]
async fn term_on_keyword_array_is_flush_invariant() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("tags", FieldType::Keyword));
    engine.create_index("docs", schema).expect("create");

    let idx = engine.get_index("docs").expect("get");
    idx.index_document(
        Some("d0".to_string()),
        json!({ "tags": ["red", "green", "blue"] }),
    )
    .await
    .expect("index");

    // Second element, document still buffered (no refresh): pre-flush answer.
    let pre = term_tag_hits(&engine, "green").await;

    // Flush to segments: post-flush answer.
    idx.refresh().await.expect("refresh");
    let post = term_tag_hits(&engine, "green").await;

    assert_eq!(
        pre, post,
        "#423/#408: `term: green` on `[\"red\",\"green\",\"blue\"]` must be \
         flush-invariant (pre-flush={pre}, post-flush={post}); the memtable term \
         path is matching only the first array element"
    );
    assert_eq!(
        post, 1,
        "sanity: the flushed segment matches the array element"
    );
}
