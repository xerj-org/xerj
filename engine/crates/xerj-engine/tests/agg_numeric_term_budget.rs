//! #3466: a `size:0` aggregation under a `term` on a numeric field answered
//! 429 at `[aggregation corpus materialisation]` on an index with overwrite
//! history, although the node was far under its budget.
//!
//! `query_node_to_agg_filter` turned only a string `term` into an agg filter,
//! so a numeric `term` never reached the columnar path and fell to the brute
//! corpus, which clones every live `_source` and is priced at 2 KB a doc
//! against `limits.max_query_memory_mb`. The same aggregation with a keyword
//! `term` or a numeric `range` was served from doc values.
//!
//! This test overwrites half of a flushed index (superseded rows resident),
//! installs a budget the brute corpus cannot meet but the doc-values columns
//! easily can, and asserts that a numeric `term` is served columnarly with
//! exact values. A full-text query, which has no columnar form, must still be
//! refused: 429 only when the bytes held really exceed the budget.
//!
//! Own test binary: `Engine::new` installs `max_query_memory_mb` in the
//! process-wide governor, so no other suite may observe this budget.

use serde_json::{json, Value};
use std::collections::BTreeMap;
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

fn doc(i: u32, generation: u32) -> Value {
    json!({
        "cat": format!("c{}", i % CATS),
        "g": (i + generation) % 4,
        "n": i + generation * 100_000,
        "body": format!("record {i} generation {generation} alpha beta gamma"),
    })
}

fn schema() -> Schema {
    let mut s = Schema::empty();
    s.fields.push(FieldConfig::new("cat", FieldType::Keyword));
    s.fields.push(FieldConfig::new("g", FieldType::Long));
    s.fields.push(FieldConfig::new("n", FieldType::Long));
    s.fields.push(FieldConfig::new("body", FieldType::Text));
    s
}

#[tokio::test]
async fn numeric_term_aggs_over_resident_ghosts_stream_doc_values_within_the_cap_3466() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.limits.max_query_memory_mb = BUDGET_MB;
    // No background merge: it would compact the superseded rows this test
    // needs resident.
    config.merge.min_merge_count = 100_000;
    let engine = Engine::new(config).unwrap();
    engine.create_index("ghosted", schema()).unwrap();
    let idx = engine.get_index("ghosted").unwrap();

    for i in 0..DOCS {
        idx.index_document(Some(format!("d{i}")), doc(i, 0))
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();
    // Overwrite every even doc: the first segment keeps 6,000 superseded rows,
    // and each rewritten doc moves to the next `g`.
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
    let plain = search(json!({
        "size": 0,
        "query": { "term": { "g": 1 } },
        "aggs": { "a": { "terms": { "field": "cat", "size": 100 } } }
    }))
    .await
    .expect("a numeric term + terms over resident ghosts must not 429");
    let with_avg = search(json!({
        "size": 0,
        "query": { "term": { "g": 1 } },
        "aggs": { "a": { "terms": { "field": "cat", "size": 100 },
                          "aggs": { "m": { "avg": { "field": "n" } } } } }
    }))
    .await
    .expect("a numeric term + terms + avg over resident ghosts must not 429");
    assert_eq!(
        fast_path_aggs_served() - before,
        2,
        "both must be served from doc values"
    );

    // Live corpus: even docs were rewritten into g + 1, odd docs kept g.
    let live_g = |i: u32| (i + u32::from((i & 1) == 0)) % 4;
    let members = |c: u32| -> Vec<u32> {
        (0..DOCS)
            .filter(|&i| i % CATS == c && live_g(i) == 1)
            .collect()
    };
    let want_total: u64 = (0..DOCS).filter(|&i| live_g(i) == 1).count() as u64;
    let want_counts: BTreeMap<String, u64> = (0..CATS)
        .filter(|&c| !members(c).is_empty())
        .map(|c| (format!("c{c}"), members(c).len() as u64))
        .collect();
    let counts = |aggs: &Value| -> BTreeMap<String, u64> {
        aggs["a"]["buckets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| {
                (
                    b["key"].as_str().unwrap().to_string(),
                    b["doc_count"].as_u64().unwrap(),
                )
            })
            .collect()
    };

    assert_eq!(plain.total.value, want_total);
    assert_eq!(counts(&plain.aggs.unwrap()), want_counts);

    assert_eq!(with_avg.total.value, want_total);
    let with_avg = with_avg.aggs.unwrap();
    assert_eq!(counts(&with_avg), want_counts);
    for b in with_avg["a"]["buckets"].as_array().unwrap() {
        let c: u32 = b["key"].as_str().unwrap()[1..].parse().unwrap();
        let live_n: f64 = members(c)
            .iter()
            .map(|&i| (i + if i % 2 == 0 { 100_000 } else { 0 }) as f64)
            .sum();
        let want = live_n / members(c).len() as f64;
        let got = b["m"]["value"].as_f64().unwrap();
        assert!(
            (got - want).abs() < 1e-6,
            "avg(n) for c{c}: got {got}, want {want} (superseded values must not count)"
        );
    }

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
