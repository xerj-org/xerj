//! #836: a (lexical) `nested` query must roll the matching children's scores
//! into the parent score per `score_mode` (avg/max/sum/min), not score every
//! matching parent a flat 1.0. The child that matches "expert" with a higher
//! term frequency scores higher, so its parent must rank first. Asserted on the
//! memtable AND after flush (the issue reports the flat score on both paths).

use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_engine::Engine;
use xerj_query::parse_request;

async fn top_ids_and_scores(engine: &Engine, q: serde_json::Value) -> Vec<(String, f32)> {
    let idx = engine.get_index("docs").expect("get index");
    let req = parse_request(&json!({
        "query": q,
        "sort": [{ "_score": "desc" }],
        "size": 10
    }))
    .expect("parse_request");
    idx.search(&req)
        .await
        .expect("search")
        .hits
        .into_iter()
        .map(|h| (h.id, h.score))
        .collect()
}

#[tokio::test]
async fn nested_query_ranks_parents_by_child_score() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config).expect("engine");

    // Schemaless: `users` is a nested array of objects with a `bio` text field.
    engine
        .create_index("docs", xerj_common::types::Schema::empty())
        .expect("create");
    let idx = engine.get_index("docs").expect("get");
    idx.index_document(
        Some("p1".to_string()),
        json!({ "users": [{ "bio": "expert" }] }),
    )
    .await
    .expect("index p1");
    idx.index_document(
        Some("p2".to_string()),
        json!({ "users": [{ "bio": "expert expert expert" }] }),
    )
    .await
    .expect("index p2");

    let q = json!({ "nested": {
        "path": "users",
        "query": { "match": { "users.bio": "expert" } },
        "score_mode": "max"
    }});

    // Both parents match; p2's child matches "expert" with a higher TF, so under
    // score_mode:max p2 must outrank p1 — on the memtable and after flush.
    let pre = top_ids_and_scores(&engine, q.clone()).await;
    idx.refresh().await.expect("refresh");
    let post = top_ids_and_scores(&engine, q.clone()).await;

    for (label, hits) in [("memtable", &pre), ("segment", &post)] {
        assert_eq!(hits.len(), 2, "{label}: both parents match");
        assert_eq!(hits[0].0, "p2", "{label}: p2 (higher child TF) ranks first");
        assert_eq!(hits[1].0, "p1", "{label}: p1 second");
        assert!(
            hits[0].1 > hits[1].1,
            "{label}: p2 score {} must exceed p1 score {} (not a flat 1.0)",
            hits[0].1,
            hits[1].1
        );
    }
}
