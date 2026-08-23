//! Issue #413 (an #423 symptom): a root-level object-mapped field is indexed on
//! flush as ONE whole, untokenised, case-preserved keyword-style blob (the pruned
//! `serde_json::to_string` emitted by `collect_text_fields`). The memtable
//! `doc_matches_query` path declined `Value::Object`, so `prefix`/`wildcard` on
//! such a field answered 0 before a flush and 1 after — a flush-divergence. The
//! fix mirrors the blob in the memtable Prefix/Wildcard arms (raw, case-
//! sensitive, root-level only), so every answer is flush-invariant.

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
async fn object_field_prefix_wildcard_is_flush_invariant() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config).expect("engine");

    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("meta", FieldType::Object));
    schema
        .fields
        .push(FieldConfig::new("tags", FieldType::Object));
    engine.create_index("docs", schema).expect("create");
    let idx = engine.get_index("docs").expect("get");
    idx.index_document(
        Some("d0".to_string()),
        json!({ "meta": {"owner":"Alpha"}, "tags": [{"owner":"Alpha"},{"owner":"Beta"}] }),
    )
    .await
    .expect("index");

    // (query, expected hit count) — the segment stores the blob
    // `{"owner":"Alpha"}` whole, untokenised, and case-preserved. For the
    // `tags` ARRAY of objects the write path joins the elements into one blob
    // (`{"owner":"Alpha"} {"owner":"Beta"}`); the memtable arms mirror the same
    // joined string, so both are flush-invariant.
    let cases = vec![
        (json!({"prefix":{"meta":"{"}}), 1u64), // blob starts with `{`
        (json!({"prefix":{"meta":"{\"owner"}}), 1), // deeper raw prefix
        (json!({"prefix":{"meta":"owner"}}), 0), // not tokenised: no `owner` token
        (json!({"prefix":{"meta":"alpha"}}), 0),
        (json!({"wildcard":{"meta":"*Alpha*"}}), 1), // contains `Alpha`
        (json!({"wildcard":{"meta":"*alpha*"}}), 0), // case-preserved: lower-case misses
        (json!({"wildcard":{"meta":"*owner*"}}), 1),
        // array-of-objects (`tags`):
        (json!({"prefix":{"tags":"{"}}), 1),
        (json!({"wildcard":{"tags":"*Alpha*"}}), 1),
        (json!({"wildcard":{"tags":"*Beta*"}}), 1),
        (json!({"wildcard":{"tags":"*alpha*"}}), 0), // case-preserved
    ];

    // Buffered (memtable) answers, then flush and re-query — each pair must agree.
    let mut pre = Vec::new();
    for (q, _) in &cases {
        pre.push(hits(&engine, q.clone()).await);
    }
    idx.refresh().await.expect("refresh");
    for ((q, expected), pre_hit) in cases.iter().zip(pre.iter()) {
        let post = hits(&engine, q.clone()).await;
        assert_eq!(
            *pre_hit, post,
            "#413: `{q}` must be flush-invariant (pre={pre_hit}, post={post})"
        );
        assert_eq!(
            post, *expected,
            "#413: `{q}` expected {expected}, got {post}"
        );
    }
}
