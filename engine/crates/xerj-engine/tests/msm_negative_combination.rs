//! Issue #846: bool `minimum_should_match` must honor NEGATIVE integers
//! (`-1` = all-but-one), negative percentages, and COMBINATION specs
//! (`"3<90%"`). These previously failed to parse and dropped to `None`, so the
//! bool silently behaved as a plain OR (default msm 1) — more hits than ES.
//!
//! Asserted flush-invariant (buffered memtable AND flushed segment) because the
//! FTS/columnar fast paths decline these specs and route to the stored-doc
//! scan; both must agree.

use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::Engine;
use xerj_query::parse_request;

async fn hits(engine: &Engine, q: &serde_json::Value) -> u64 {
    let idx = engine.get_index("docs").expect("get index");
    let req = parse_request(&json!({ "query": q, "size": 0 })).expect("parse_request");
    idx.search(&req).await.expect("search").total.value
}

#[tokio::test]
async fn bool_min_should_match_honors_negative_and_combination() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config).expect("engine");

    // Single-valued fields (one per should-clause) so "how many should clauses
    // match" is unambiguous — avoids multi-valued-array term counting.
    let mut schema = Schema::empty();
    for f in ["fa", "fb", "fc"] {
        schema.fields.push(FieldConfig::new(f, FieldType::Keyword));
    }
    engine.create_index("docs", schema).expect("create");
    let idx = engine.get_index("docs").expect("get");
    // How many of the three should-clauses each doc satisfies:
    for (id, doc) in [
        ("d0", json!({ "fa": "y", "fb": "y", "fc": "y" })), // 3
        ("d1", json!({ "fa": "y", "fb": "y" })),            // 2
        ("d2", json!({ "fa": "y" })),                       // 1
        ("d3", json!({ "fa": "n" })),                       // 0
    ] {
        idx.index_document(Some(id.to_string()), doc)
            .await
            .expect("index");
    }

    // Three should clauses on distinct fields. `required` varies by the msm spec.
    let boolq = |msm: serde_json::Value| {
        json!({ "bool": {
            "should": [
                {"term": {"fa": "y"}}, {"term": {"fb": "y"}}, {"term": {"fc": "y"}}
            ],
            "minimum_should_match": msm
        }})
    };

    // (spec, expected_hits, label). should_count = 3.
    let cases = [
        (json!(-1), 2, "negative -1: required 2 → d0,d1"),
        (json!("-1"), 2, "string -1: required 2 → d0,d1"),
        (json!(-2), 3, "negative -2: required 1 → d0,d1,d2"),
        (json!("2<-1"), 2, "combination 2<-1: 3>2 → -1 → required 2"),
        (
            json!("3<90%"),
            1,
            "combination 3<90%: 3<=3 → all required → d0",
        ),
    ];

    // Answer every case buffered, flush once, answer again — assert agreement
    // AND the ES-correct count.
    let mut pre = Vec::new();
    for (msm, _, _) in &cases {
        pre.push(hits(&engine, &boolq(msm.clone())).await);
    }
    idx.refresh().await.expect("refresh");
    for (i, (msm, expected, label)) in cases.iter().enumerate() {
        let post = hits(&engine, &boolq(msm.clone())).await;
        assert_eq!(pre[i], post, "#846: msm must be flush-invariant — {label}");
        assert_eq!(post, *expected, "#846: {label}");
    }
}
