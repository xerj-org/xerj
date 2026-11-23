//! #3509 timing: `bool` pages whose scores differ (`should`), on 200k docs in
//! 20 flushed segments, `size:10`. Each run uses a different query so the
//! whole-result cache misses; prints the warm p50 / p90 of 10 runs per shape.
//! Also checks that every timed page equals the head of the exhaustive page.
//!
//! `cargo test --profile ci-test -p xerj-engine --test
//! bool_should_score_bound_bench -- --ignored --nocapture`

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::Engine;
use xerj_query::parse_request;

const DOCS: u32 = 200_000;
const SEGMENTS: u32 = 20;
const RUNS: usize = 10;

fn doc(i: u32) -> Value {
    // Deterministic spread: cat cycles 10 values, rank 1..=12 on another
    // stride, so every (cat, rank) pair recurs in every segment.
    json!({
        "cat": format!("c{}", i % 10),
        "rank": 1 + (i / 7) % 12,
        "body": format!("record {i}"),
    })
}

async fn page(idx: &xerj_engine::index::Index, query: &Value, size: usize) -> (Vec<String>, u64) {
    let body = json!({"size": size, "track_total_hits": true, "query": query});
    let res = idx.search(&parse_request(&body).unwrap()).await.unwrap();
    (
        res.hits.iter().map(|h| h.id.clone()).collect(),
        res.total.value,
    )
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

#[tokio::test]
#[ignore]
async fn bool_should_score_bound_timings() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.merge.min_segments = 1_000_000;
    config.merge.min_merge_count = 1_000_000;
    let engine = Engine::new(config).unwrap();
    let mut schema = Schema::empty();
    for (f, t) in [
        ("cat", FieldType::Keyword),
        ("rank", FieldType::Long),
        ("body", FieldType::Text),
    ] {
        schema.add_field(FieldConfig::new(f, t)).unwrap();
    }
    engine.create_index("bench", schema).unwrap();
    let idx = engine.get_index("bench").unwrap();
    let per = DOCS / SEGMENTS;
    for s in 0..SEGMENTS {
        let batch: Vec<(Option<String>, Value)> = (s * per..(s + 1) * per)
            .map(|i| (Some(format!("p{i:07}")), doc(i)))
            .collect();
        for r in idx.index_documents_batched(batch).await {
            r.unwrap();
        }
        idx.flush().await.unwrap();
    }
    eprintln!("docs: {}", idx.live_doc_count());

    type Shape = (&'static str, Box<dyn Fn(usize) -> Value>);
    let shapes: Vec<Shape> = vec![
        (
            "bool should [term, range]",
            Box::new(|i| {
                json!({"bool": {"should": [
                    {"term": {"cat": format!("c{}", i % 10)}},
                    {"range": {"rank": {"gte": 6 + i % 5}}}
                ]}})
            }),
        ),
        (
            "bool should [term, term, range] (ceiling unreachable)",
            Box::new(|i| {
                json!({"bool": {"should": [
                    {"term": {"cat": format!("c{}", i % 10)}},
                    {"term": {"cat": format!("c{}", (i + 1) % 10)}},
                    {"range": {"rank": {"gte": 8 + i % 4}}}
                ]}})
            }),
        ),
        (
            "bool must [range] should [term]",
            Box::new(|i| {
                json!({"bool": {
                    "must": [{"range": {"rank": {"gte": 2 + i % 5}}}],
                    "should": [{"term": {"cat": format!("c{}", i % 10)}}]
                }})
            }),
        ),
        (
            "bool must [term, range]",
            Box::new(|i| {
                json!({"bool": {"must": [
                    {"term": {"cat": format!("c{}", i % 10)}},
                    {"range": {"rank": {"gte": 6 + i % 5}}}
                ]}})
            }),
        ),
        (
            "bool filter [term, range]",
            Box::new(|i| {
                json!({"bool": {"filter": [
                    {"term": {"cat": format!("c{}", i % 10)}},
                    {"range": {"rank": {"gte": 6 + i % 5}}}
                ]}})
            }),
        ),
    ];
    for (label, make) in &shapes {
        // Warm the segment caches with one query of this shape (another
        // size, so the timed runs still miss the whole-result cache).
        page(&idx, &make(0), 11).await;
        let mut ms: Vec<f64> = Vec::new();
        for i in 0..RUNS {
            let q = make(i);
            let t = std::time::Instant::now();
            let (got, total) = page(&idx, &q, 10).await;
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
            let (full, full_total) = page(&idx, &q, 3000).await;
            assert_eq!(total, full_total, "{label} run {i}: total");
            assert_eq!(
                got[..],
                full[..got.len()],
                "{label} run {i}: page != head of full"
            );
        }
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        eprintln!(
            "BENCH {label}: p50 {:.1} ms, p90 {:.1} ms, min {:.1} ms",
            pct(&ms, 0.5),
            pct(&ms, 0.9),
            ms[0]
        );
    }
}
