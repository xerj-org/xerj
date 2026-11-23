//! #3510 timing: `size:0` aggregations under a filter-only `bool`, 300k docs
//! in 10 flushed segments plus a memtable tail. Each run uses a different
//! filter value so the whole-result cache misses; prints the p50 / p90 of 8
//! runs per shape, and whether the doc-values fast path served it. Every
//! timed response is checked against the same filter written so it cannot
//! take the fast path (`must_not` of a value no doc holds). The query budget
//! is raised so that brute oracle can run; at the default 512 MB it answers
//! 429 ("aggregation corpus materialisation", ~590 MB), as the bool filter
//! itself did before #3510.
//!
//! `cargo test --profile ci-test -p xerj-engine --test bool_filter_aggs_bench
//! -- --ignored --nocapture`

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::aggs::fast_path_aggs_served;
use xerj_engine::Engine;
use xerj_query::parse_request;

const DOCS: u32 = 300_000;
const SEGMENTS: u32 = 10;
const TAIL: u32 = 2_000;
const RUNS: usize = 8;

fn doc(i: u32) -> Value {
    json!({
        "cat": format!("c{}", i % 10),
        "code": format!("k{}", (i / 3) % 7),
        "n": i,
        "price": ((i * 7919) % 10_000) as f64 / 10.0,
        "body": format!("record {i}"),
    })
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn visible(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(k, _)| !k.starts_with("__"))
                .map(|(k, v)| (k.clone(), visible(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(visible).collect()),
        other => other.clone(),
    }
}

/// Equal, except floats may differ in summation order (relative 1e-12):
/// the fast path sums per segment, the brute path in corpus order.
fn close(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| close(v, w)))
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(v, w)| close(v, w))
        }
        (Value::Number(x), Value::Number(y)) if x.is_f64() || y.is_f64() => {
            let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
            (x - y).abs() <= 1e-12 * x.abs().max(y.abs())
        }
        _ => a == b,
    }
}

#[tokio::test]
#[ignore]
async fn bool_filter_aggs_timings() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.merge.min_segments = 1_000_000;
    config.merge.min_merge_count = 1_000_000;
    // The brute oracle (and the brute path before #3510) materialises the
    // whole corpus: ~590 MB here, over the default 512 MB budget (429).
    config.limits.max_query_memory_mb = 4096;
    let engine = Engine::new(config).unwrap();
    let mut schema = Schema::empty();
    for (f, t) in [
        ("cat", FieldType::Keyword),
        ("code", FieldType::Keyword),
        ("n", FieldType::Long),
        ("price", FieldType::Double),
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
    let tail: Vec<(Option<String>, Value)> = (DOCS..DOCS + TAIL)
        .map(|i| (Some(format!("p{i:07}")), doc(i)))
        .collect();
    for r in idx.index_documents_batched(tail).await {
        r.unwrap();
    }

    type Shape = (&'static str, Box<dyn Fn(usize) -> Vec<Value>>, Value);
    let shapes: Vec<Shape> = vec![
        // Control: one range leaf took the fast path before #3510 too.
        (
            "range only + stats (control)",
            Box::new(|i| vec![json!({"range": {"price": {"lt": 500.0 + i as f64}}})]),
            json!({"a": {"stats": {"field": "price"}}}),
        ),
        (
            "bool filter [term, range] + terms",
            Box::new(|i| {
                vec![
                    json!({"term": {"cat": format!("c{}", i % 10)}}),
                    json!({"range": {"n": {"gte": 1000 * i as u64}}}),
                ]
            }),
            json!({"a": {"terms": {"field": "code"}}}),
        ),
        (
            "bool filter [term, range] + stats",
            Box::new(|i| {
                vec![
                    json!({"term": {"code": format!("k{}", i % 7)}}),
                    json!({"range": {"price": {"lt": 500.0 + i as f64}}}),
                ]
            }),
            json!({"a": {"stats": {"field": "price"}}}),
        ),
        (
            "bool filter [terms, range, term] + terms/sum",
            Box::new(|i| {
                vec![
                    json!({"terms": {"cat": ["c1", "c2", format!("c{}", 3 + i % 7)]}}),
                    json!({"range": {"n": {"lt": 250_000 + i as u64}}}),
                    json!({"term": {"code": format!("k{}", i % 7)}}),
                ]
            }),
            json!({"a": {"terms": {"field": "cat"}, "aggs": {"s": {"sum": {"field": "price"}}}}}),
        ),
    ];
    for (label, make, aggs) in &shapes {
        let mut ms: Vec<f64> = Vec::new();
        let mut served = 0;
        let mut float_diffs = 0;
        for i in 0..RUNS {
            let clauses = make(i);
            let query = if clauses.len() == 1 {
                clauses[0].clone()
            } else {
                json!({"bool": {"filter": clauses}})
            };
            let body = json!({"size": 0, "query": query, "aggs": aggs});
            let before = fast_path_aggs_served();
            let t = std::time::Instant::now();
            let res = idx.search(&parse_request(&body).unwrap()).await.unwrap();
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
            served += (fast_path_aggs_served() - before) as usize;
            let brute = json!({"size": 0, "query": {"bool": {
                "filter": make(i),
                "must_not": [{"term": {"cat": "none"}}]
            }}, "aggs": aggs});
            let want = idx.search(&parse_request(&brute).unwrap()).await.unwrap();
            assert_eq!(res.total.value, want.total.value, "{label} run {i}: total");
            let (got, exp) = (
                visible(res.aggs.as_ref().unwrap()),
                visible(want.aggs.as_ref().unwrap()),
            );
            if got != exp {
                assert!(close(&got, &exp), "{label} run {i}: aggs\n{got}\n{exp}");
                float_diffs += 1;
            }
        }
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        eprintln!(
            "BENCH {label}: p50 {:.1} ms, p90 {:.1} ms, min {:.1} ms, fast {served}/{RUNS}, \
             float sums off in the last digits {float_diffs}/{RUNS}",
            pct(&ms, 0.5),
            pct(&ms, 0.9),
            ms[0]
        );
    }
}
