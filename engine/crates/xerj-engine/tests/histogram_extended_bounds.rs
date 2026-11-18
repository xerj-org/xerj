//! Issue #1819: `histogram` `extended_bounds` must only ADD empty buckets.
//! Buckets holding data outside the bounds stay, as in Elasticsearch 8.13.4;
//! only `hard_bounds` drops them.
//!
//! ROOT CAUSE: both executors picked the gap-fill range as
//! `extended_min.or(data_min)` / `extended_max.or(data_max)` — the bound
//! *instead of* the wider of the bound and the data — so a bound inside the
//! data range clipped the buckets on either side of it, the same as
//! `hard_bounds`. The brute runner (`aggs::run_histogram`) and its columnar
//! mirror (`fast_aggs::exec_histogram`) carried the same two lines.
//! `date_histogram` already widened (`range_min.min(key)` /
//! `range_max.max(key)`), which is why it matched ES.
//!
//! Both executors are pinned: the columnar path only serves an index at or
//! above `fast_aggs::FAST_AGG_MIN_DOCS` (10 000 docs), so the same values are
//! indexed once below that gate and once above it, and
//! `aggs::fast_path_aggs_served` says which executor answered. That counter
//! is process-global, so this is deliberately ONE test function.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::aggs::fast_path_aggs_served;
use xerj_engine::bulk::process_bulk;
use xerj_engine::Engine;
use xerj_query::parse_request;

/// One case: the histogram agg, the expected (key, doc_count) buckets, and a label.
type HistogramCase = (Value, Vec<(i64, u64)>, &'static str);

/// The #1819 reproduction's values. ES 8.13.4, `interval: 5`,
/// `extended_bounds: {min: 0, max: 10}`:
/// `-5:1, 0:2, 5:0, 10:1, 15:0, 20:0, 25:1`.
const VALUES: [f64; 5] = [-2.0, 1.0, 3.0, 12.0, 27.0];

/// Below the columnar gate: one copy of the values.
const SMALL_DOCS: usize = VALUES.len();
/// Above the columnar gate: 2 010 copies of the values (10 050 docs), so
/// every expected count is the ES count times `BIG_REPEAT`.
const BIG_REPEAT: usize = 2_010;

async fn build_index(engine: &Engine, name: &str, docs: usize) {
    engine.create_index(name, Schema::empty()).unwrap();
    for start in (0..docs).step_by(1_000) {
        let end = (start + 1_000).min(docs);
        let mut body = String::new();
        for i in start..end {
            body.push_str(&format!(
                "{{\"index\":{{\"_index\":\"{name}\",\"_id\":\"{i}\"}}}}\n"
            ));
            body.push_str(&format!("{{\"v\":{}}}\n", VALUES[i % VALUES.len()]));
        }
        let res = process_bulk(engine, Some(name), &body).await;
        assert!(!res.errors, "bulk errors while seeding {name}");
    }
    // The columnar path reads segment `.dv` sidecars; flush both indices so
    // the only difference between them is the size gate.
    engine.get_index(name).unwrap().refresh().await.unwrap();
}

/// `(key, doc_count)` pairs of one `size:0` histogram, and whether the
/// columnar fast path served it.
async fn histogram(engine: &Engine, index: &str, agg: Value) -> (Vec<(i64, u64)>, u64) {
    let idx = engine.get_index(index).unwrap();
    let req = parse_request(&json!({
        "query": {"match_all": {}},
        "size": 0,
        "aggs": {"h": agg},
    }))
    .unwrap();
    let before = fast_path_aggs_served();
    let res = idx.search(&req).await.unwrap();
    let served = fast_path_aggs_served() - before;
    let aggs = res.aggs.clone().expect("aggs present");
    let buckets = aggs["h"]["buckets"]
        .as_array()
        .unwrap_or_else(|| panic!("expected buckets, got {aggs}"))
        .iter()
        .map(|b| {
            (
                b["key"].as_f64().expect("numeric key") as i64,
                b["doc_count"].as_u64().expect("doc_count"),
            )
        })
        .collect();
    (buckets, served)
}

fn scaled(expected: &[(i64, u64)], by: usize) -> Vec<(i64, u64)> {
    expected.iter().map(|&(k, n)| (k, n * by as u64)).collect()
}

#[tokio::test]
async fn extended_bounds_keeps_data_buckets_outside_the_bounds_on_both_executors() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config).expect("engine::new");

    build_index(&engine, "small", SMALL_DOCS).await;
    build_index(&engine, "big", VALUES.len() * BIG_REPEAT).await;

    // (agg, expected buckets for ONE copy of VALUES, what the case pins)
    let cases: Vec<HistogramCase> = vec![
        (
            json!({"histogram": {"field": "v", "interval": 5,
                                 "extended_bounds": {"min": 0, "max": 10}}}),
            vec![(-5, 1), (0, 2), (5, 0), (10, 1), (15, 0), (20, 0), (25, 1)],
            "#1819: bounds inside the data keep the data on both sides",
        ),
        (
            json!({"histogram": {"field": "v", "interval": 5,
                                 "extended_bounds": {"min": -20, "max": 40}}}),
            vec![
                (-20, 0),
                (-15, 0),
                (-10, 0),
                (-5, 1),
                (0, 2),
                (5, 0),
                (10, 1),
                (15, 0),
                (20, 0),
                (25, 1),
                (30, 0),
                (35, 0),
                (40, 0),
            ],
            "bounds wider than the data still add empty buckets",
        ),
        (
            json!({"histogram": {"field": "v", "interval": 5,
                                 "extended_bounds": {"min": 10}}}),
            vec![(-5, 1), (0, 2), (5, 0), (10, 1), (15, 0), (20, 0), (25, 1)],
            "a min-only bound above the data min drops nothing",
        ),
        (
            json!({"histogram": {"field": "v", "interval": 5,
                                 "hard_bounds": {"min": 0, "max": 10}}}),
            vec![(0, 2), (5, 0), (10, 1)],
            "hard_bounds still drops buckets outside it (unchanged)",
        ),
    ];

    for (agg, expected, what) in cases {
        let (got, served) = histogram(&engine, "small", agg.clone()).await;
        assert_eq!(served, 0, "the small index must take the brute path");
        assert_eq!(got, expected, "brute path: {what}: {agg}");

        let (got, served) = histogram(&engine, "big", agg.clone()).await;
        assert_eq!(served, 1, "the big index must take the columnar path");
        assert_eq!(
            got,
            scaled(&expected, BIG_REPEAT),
            "columnar path: {what}: {agg}"
        );
    }
}
