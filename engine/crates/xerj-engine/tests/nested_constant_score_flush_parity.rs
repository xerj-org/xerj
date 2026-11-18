//! #2654 — a `constant_score` nested in a `bool` must add exactly its boost
//! on flushed segments.
//!
//! The segment FTS projection turned a nested `constant_score` into its inner
//! query and relied on the top-level override in `search`, which only
//! handles a `constant_score` at the root. After a flush the clause added
//! the inner query's BM25 (0.56 here) instead of the boost (1000). The
//! `xerj code` coverage clauses (#2329) are two boost-1000 `constant_score`
//! clauses inside a `bool`, so they scored the same wrong way.
//!
//! ES semantics: `constant_score` scores every match exactly `boost`
//! (Lucene `ConstantScoreQuery`), and `bool` sums its matching `must` and
//! `should` scores.

use std::collections::HashMap;

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::{Engine, Index};
use xerj_query::parse_request;

async fn seed_flushed(name: &str) -> (TempDir, std::sync::Arc<Index>) {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config).expect("engine::new");
    let mut schema = Schema::empty();
    schema.fields.push(FieldConfig::new("t", FieldType::Text));
    engine.create_index(name, schema).unwrap();
    let idx = engine.get_index(name).unwrap();
    for (id, t) in [("1", "alpha beta"), ("2", "alpha"), ("3", "gamma beta")] {
        idx.index_document(Some(id.to_string()), json!({ "t": t }))
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();
    (dir, idx)
}

async fn scores(idx: &Index, query: Value) -> HashMap<String, f32> {
    let req = parse_request(&json!({"query": query, "size": 10})).expect("parse_request");
    idx.search(&req)
        .await
        .unwrap()
        .hits
        .into_iter()
        .map(|h| (h.id, h.score))
        .collect()
}

fn alpha() -> Value {
    json!({"match": {"t": "alpha"}})
}

fn boosted_beta(boost: f32) -> Value {
    json!({"constant_score": {"filter": {"match": {"t": "beta"}}, "boost": boost}})
}

fn assert_close(label: &str, actual: Option<&f32>, expected: f32) {
    let actual = *actual.unwrap_or_else(|| panic!("{label}: missing hit"));
    assert!(
        (actual - expected).abs() < 1e-3,
        "{label}: _score is {actual}, expected {expected}"
    );
}

#[tokio::test]
async fn nested_constant_score_adds_its_boost_after_a_flush() {
    let (_dir, idx) = seed_flushed("cs-should").await;
    let must_only = scores(&idx, alpha()).await;

    let got = scores(
        &idx,
        json!({"bool": {"must": [alpha()], "should": [boosted_beta(1000.0)]}}),
    )
    .await;
    assert_eq!(got.len(), 2, "{got:?}");
    assert_close(
        "doc 1 (matches the should)",
        got.get("1"),
        must_only["1"] + 1000.0,
    );
    assert_close("doc 2 (does not)", got.get("2"), must_only["2"]);

    // In `must`, alone: every match scores exactly the boost.
    let got = scores(&idx, json!({"bool": {"must": [boosted_beta(7.0)]}})).await;
    assert_eq!(got.len(), 2, "{got:?}");
    assert_close("must-only doc 1", got.get("1"), 7.0);
    assert_close("must-only doc 3", got.get("3"), 7.0);

    // Two scoring clauses, one of them constant.
    let got = scores(
        &idx,
        json!({"bool": {"must": [boosted_beta(3.0), alpha()]}}),
    )
    .await;
    assert_eq!(got.len(), 1, "{got:?}");
    assert_close("conjunction doc 1", got.get("1"), must_only["1"] + 3.0);
}

#[tokio::test]
async fn top_level_constant_score_is_unchanged() {
    let (_dir, idx) = seed_flushed("cs-top").await;
    let got = scores(&idx, boosted_beta(2.5)).await;
    assert_eq!(got.len(), 2, "{got:?}");
    assert_close("doc 1", got.get("1"), 2.5);
    assert_close("doc 3", got.get("3"), 2.5);
}
