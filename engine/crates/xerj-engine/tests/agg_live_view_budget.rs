//! #3444: `size:0` aggregations over an index with overwrite history answered
//! 429 at `[aggregation corpus materialisation]` although the node was far
//! under its budget.
//!
//! A segment still holding superseded or deleted rows (no merge yet) made the
//! columnar fast path bail for every plan except the #1267 row-exact one, so
//! `terms`, `date_histogram`, `cardinality` and a `terms` + `avg` fell to the
//! brute corpus, which clones every live `_source` and is priced at 2 KB a doc
//! against `limits.max_query_memory_mb`. The estimate also counted the
//! superseded rows the corpus skips.
//!
//! This test overwrites half of a flushed index (ghosts resident), installs a
//! budget the brute corpus cannot meet but the doc-values columns easily can,
//! and asserts the four aggregations are served columnarly with exact values.
//! A shape with no columnar form (full-text query) must still be refused: 429
//! only when the bytes held really exceed the budget.
//!
//! Own test binary: `Engine::new` installs `max_query_memory_mb` in the
//! process-wide governor, so no other suite may observe this budget.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::aggs::fast_path_aggs_served;
use xerj_engine::Engine;
use xerj_query::parse_request;

/// 12,000 live docs x 2 KB = 23 MiB of brute corpus; the columns these
/// aggregations read are well under 1 MiB.
const BUDGET_MB: u64 = 8;
const DOCS: u32 = 12_000;
const CATS: u32 = 10;
const USERS: u32 = 700;

fn doc(i: u32, generation: u32) -> Value {
    json!({
        "cat": format!("c{}", i % CATS),
        "user": format!("u{}", i % USERS),
        "ts": format!("2026-{:02}-01T00:00:00Z", 1 + i % 12),
        "n": (i + generation * 100_000) as i64,
        "body": format!("record {i} generation {generation} alpha beta gamma"),
    })
}

fn schema() -> Schema {
    let mut s = Schema::empty();
    s.fields.push(FieldConfig::new("cat", FieldType::Keyword));
    s.fields.push(FieldConfig::new("user", FieldType::Keyword));
    s.fields.push(FieldConfig::new("ts", FieldType::Date));
    s.fields.push(FieldConfig::new("n", FieldType::Long));
    s.fields.push(FieldConfig::new("body", FieldType::Text));
    s
}

#[tokio::test]
async fn aggs_over_resident_ghosts_stream_doc_values_within_the_cap_3444() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.limits.max_query_memory_mb = BUDGET_MB;
    let engine = Engine::new(config).unwrap();
    engine.create_index("ghosted", schema()).unwrap();
    let idx = engine.get_index("ghosted").unwrap();

    for i in 0..DOCS {
        idx.index_document(Some(format!("d{i}")), doc(i, 0))
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();
    // Overwrite every even doc: the first segment keeps 6,000 superseded rows.
    for i in (0..DOCS).step_by(2) {
        idx.index_document(Some(format!("d{i}")), doc(i, 1))
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();
    assert_eq!(idx.live_doc_count(), DOCS as u64);
    assert_eq!(
        idx.deleted_doc_count(),
        (DOCS / 2) as u64,
        "the superseded rows must still be resident for this test to mean anything"
    );

    let search = |body: Value| {
        let idx = idx.clone();
        async move { idx.search(&parse_request(&body).unwrap()).await }
    };

    let before = fast_path_aggs_served();
    let terms = search(json!({
        "size": 0,
        "aggs": { "a": { "terms": { "field": "cat", "size": 100 },
                          "aggs": { "m": { "avg": { "field": "n" } } } } }
    }))
    .await
    .expect("terms + avg over resident ghosts must not 429");
    let hist = search(json!({
        "size": 0,
        "aggs": { "a": { "date_histogram": { "field": "ts", "calendar_interval": "month" } } }
    }))
    .await
    .expect("date_histogram over resident ghosts must not 429");
    let card = search(json!({
        "size": 0,
        "aggs": { "a": { "cardinality": { "field": "user" } } }
    }))
    .await
    .expect("cardinality over resident ghosts must not 429");
    assert_eq!(
        fast_path_aggs_served() - before,
        3,
        "all three must be served from doc values"
    );

    let terms = terms.aggs.unwrap();
    let buckets = terms["a"]["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), CATS as usize);
    for b in buckets {
        let c: u32 = b["key"].as_str().unwrap()[1..].parse().unwrap();
        let members: Vec<u32> = (0..DOCS).filter(|i| i % CATS == c).collect();
        assert_eq!(b["doc_count"].as_u64().unwrap(), members.len() as u64);
        let live_n: f64 = members
            .iter()
            .map(|&i| (i + if i % 2 == 0 { 100_000 } else { 0 }) as f64)
            .sum();
        let want = live_n / members.len() as f64;
        let got = b["m"]["value"].as_f64().unwrap();
        assert!(
            (got - want).abs() < 1e-6,
            "avg(n) for c{c}: got {got}, want {want} (superseded values must not count)"
        );
    }

    let hist = hist.aggs.unwrap();
    let hb = hist["a"]["buckets"].as_array().unwrap();
    assert_eq!(hb.len(), 12);
    for b in hb {
        assert_eq!(b["doc_count"].as_u64().unwrap(), (DOCS / 12) as u64);
    }

    assert_eq!(
        card.aggs.unwrap()["a"]["value"].as_u64().unwrap(),
        USERS as u64
    );

    // A full-text query has no columnar form: the brute corpus (12,000 docs x
    // 2 KB) really is over the 8 MiB budget, and stays refused.
    let fts = search(json!({
        "size": 0,
        "query": { "match": { "body": "alpha" } },
        "aggs": { "a": { "terms": { "field": "cat" } } }
    }))
    .await;
    let err = fts.expect_err("a brute corpus over the budget must still be refused");
    assert!(
        err.to_string()
            .contains("aggregation corpus materialisation"),
        "{err}"
    );
}
