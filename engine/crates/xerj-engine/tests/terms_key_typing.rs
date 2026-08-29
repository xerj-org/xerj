//! #864: a terms-agg bucket key is typed by the field's SOURCE shape. A keyword
//! value that looks numeric ("007") must stay the string "007" (ES keys a
//! keyword bucket by the string); a genuine numeric field still keys by number.
//! Asserted on the memtable (brute `run_terms`) AND after flush (columnar
//! `fast_aggs`) so the two paths agree.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::Engine;
use xerj_query::parse_request;

async fn bucket_keys(engine: &Engine, field: &str) -> Vec<Value> {
    let idx = engine.get_index("docs").expect("get index");
    let req = parse_request(&json!({
        "size": 0,
        "aggs": { "by": { "terms": { "field": field } } }
    }))
    .expect("parse_request");
    let res = idx.search(&req).await.expect("search");
    res.aggs.expect("aggs")["by"]["buckets"]
        .as_array()
        .expect("buckets")
        .iter()
        .map(|b| b["key"].clone())
        .collect()
}

#[tokio::test]
async fn terms_keyword_key_stays_string_numeric_stays_number() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config).expect("engine");

    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("code", FieldType::Keyword));
    schema.fields.push(FieldConfig::new("qty", FieldType::Long));
    engine.create_index("docs", schema).expect("create");
    let idx = engine.get_index("docs").expect("get");
    for (id, code, qty) in [("d0", "007", 5), ("d1", "007", 5), ("d2", "042", 9)] {
        idx.index_document(Some(id.to_string()), json!({ "code": code, "qty": qty }))
            .await
            .expect("index");
    }

    // Assert buffered (memtable) and flushed (segment) agree AND are ES-correct.
    let kw_pre = bucket_keys(&engine, "code").await;
    let num_pre = bucket_keys(&engine, "qty").await;
    idx.refresh().await.expect("refresh");
    let kw_post = bucket_keys(&engine, "code").await;
    let num_post = bucket_keys(&engine, "qty").await;

    assert_eq!(
        kw_pre, kw_post,
        "#864: keyword key typing must be flush-invariant"
    );
    assert_eq!(
        num_pre, num_post,
        "numeric key typing must be flush-invariant"
    );

    // Keyword: string keys with leading zero intact.
    assert!(
        kw_post.contains(&json!("007")) && kw_post.contains(&json!("042")),
        "keyword keys must stay strings, got {kw_post:?}"
    );
    assert!(
        !kw_post.contains(&json!(7)) && !kw_post.contains(&json!(42)),
        "keyword keys must NOT be coerced to numbers, got {kw_post:?}"
    );
    // Numeric field: number keys (control — coercion still applies).
    assert!(
        num_post.contains(&json!(5)) && num_post.contains(&json!(9)),
        "numeric keys must stay numbers, got {num_post:?}"
    );
}
