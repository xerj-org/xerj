//! Criterion benchmarks for xerj core operations.
//!
//! Run with:
//!   cargo bench -p xerj-engine --bench search_bench
//!
//! Individual benchmark groups:
//!   cargo bench -p xerj-engine --bench search_bench -- bm25
//!   cargo bench -p xerj-engine --bench search_bench -- indexing
//!   cargo bench -p xerj-engine --bench search_bench -- aggregations

use criterion::{
    black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

use xerj_common::{
    config::Config,
    types::{FieldConfig, FieldType, Schema},
};
use xerj_engine::Engine;
use xerj_query::{ast::SearchRequest, parse_request};

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn make_config(data_dir: &str) -> Config {
    let mut cfg = Config::default();
    cfg.server.data_dir = data_dir.to_string();
    cfg.auth.enabled = false;
    cfg.tls.enabled = false;
    // Large thresholds so segments don't flush mid-benchmark.
    cfg.storage.flush_size_mb = 4096;
    cfg.storage.flush_interval_secs = 3600;
    cfg
}

fn text_schema() -> Schema {
    let mut schema = Schema::empty();
    let _ = schema.add_field(FieldConfig::new("title", FieldType::Text));
    let _ = schema.add_field(FieldConfig::new("content", FieldType::Text));
    let _ = schema.add_field(FieldConfig::new("category", FieldType::Keyword));
    let _ = schema.add_field(FieldConfig::new("score", FieldType::Double));
    schema
}

/// Build a sample document body for document `i`.
fn make_doc(i: usize) -> Value {
    let categories = ["technology", "science", "politics", "sports", "arts"];
    let category = categories[i % categories.len()];
    json!({
        "title": format!("Document number {} about {}", i, category),
        "content": format!(
            "This is the full text content of document {}. \
             It discusses topics related to {} and contains enough words \
             to exercise the BM25 scorer meaningfully. \
             Additional filler text to simulate realistic document lengths: \
             alpha beta gamma delta epsilon zeta eta theta iota kappa.",
            i, category
        ),
        "category": category,
        "score": (i % 100) as f64 / 10.0,
    })
}

/// Build an engine pre-loaded with `n` documents. Returns `(engine, _dir)`;
/// the caller must keep `_dir` alive for the benchmark duration.
fn engine_with_docs(n: usize) -> (Engine, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let cfg = make_config(dir.path().to_str().expect("utf8 path"));
    let engine = Engine::new(cfg).expect("engine");

    engine
        .create_index("bench", text_schema())
        .expect("create index");

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let idx = engine.get_index("bench").expect("get index");

    rt.block_on(async {
        for i in 0..n {
            let doc = make_doc(i);
            idx.index_document(Some(format!("{}", i)), doc)
                .await
                .expect("index doc");
        }
    });

    (engine, dir)
}

// ─────────────────────────────────────────────────────────────────────────────
// BM25 search benchmark
// ─────────────────────────────────────────────────────────────────────────────

/// Benchmark full-text BM25 search against 10K in-memory documents.
fn bench_bm25_search(c: &mut Criterion) {
    let (engine, _dir) = engine_with_docs(10_000);
    let idx = engine.get_index("bench").expect("get index");
    let rt = Arc::new(tokio::runtime::Runtime::new().expect("tokio runtime"));

    let mut group = c.benchmark_group("bm25");
    group.throughput(Throughput::Elements(1));

    let queries: &[(&str, Value)] = &[
        (
            "match_single_term",
            json!({"query": {"match": {"content": "technology"}}}),
        ),
        (
            "match_phrase",
            json!({"query": {"match_phrase": {"content": "document number"}}}),
        ),
        (
            "bool_must_should",
            json!({
                "query": {
                    "bool": {
                        "must": [{"match": {"content": "text"}}],
                        "should": [{"match": {"category": "science"}}]
                    }
                }
            }),
        ),
        (
            "term_filter",
            json!({"query": {"term": {"category": "sports"}}}),
        ),
        (
            "range_filter",
            json!({"query": {"range": {"score": {"gte": 5.0, "lte": 9.0}}}}),
        ),
    ];

    for (name, request_body) in queries {
        let request: SearchRequest = parse_request(request_body).expect("parse search request");

        let idx_ref = Arc::clone(&idx);
        let rt_ref = Arc::clone(&rt);

        group.bench_with_input(BenchmarkId::from_parameter(name), name, |b, _| {
            b.iter(|| {
                let idx = Arc::clone(&idx_ref);
                let req = request.clone();
                rt_ref.block_on(async move {
                    black_box(idx.search(&req).await.expect("search"))
                })
            });
        });
    }

    group.finish();
}

// ─────────────────────────────────────────────────────────────────────────────
// Document indexing throughput benchmark
// ─────────────────────────────────────────────────────────────────────────────

/// Benchmark the cost of indexing a single document into a warm engine.
fn bench_indexing(c: &mut Criterion) {
    let dir = TempDir::new().expect("tempdir");
    let cfg = make_config(dir.path().to_str().expect("utf8 path"));
    let engine = Engine::new(cfg).expect("engine");
    engine
        .create_index("bench_write", text_schema())
        .expect("create index");

    let idx = engine.get_index("bench_write").expect("get index");
    let rt = Arc::new(tokio::runtime::Runtime::new().expect("tokio runtime"));

    let mut group = c.benchmark_group("indexing");

    // ── single document ────────────────────────────────────────────────────

    group.throughput(Throughput::Elements(1));
    let counter = Arc::new(AtomicUsize::new(0));

    {
        let idx_ref = Arc::clone(&idx);
        let rt_ref = Arc::clone(&rt);
        let counter_ref = Arc::clone(&counter);

        group.bench_function("single_doc", |b| {
            b.iter_batched(
                || {
                    let i = counter_ref.fetch_add(1, Ordering::Relaxed);
                    (Some(format!("{}", i)), make_doc(i))
                },
                |(id, doc)| {
                    let idx = Arc::clone(&idx_ref);
                    rt_ref.block_on(async move {
                        black_box(idx.index_document(id, doc).await.expect("index"))
                    })
                },
                BatchSize::SmallInput,
            )
        });
    }

    // ── batch of 100 documents ─────────────────────────────────────────────

    group.throughput(Throughput::Elements(100));
    let batch_counter = Arc::new(AtomicUsize::new(1_000_000));

    {
        let idx_ref = Arc::clone(&idx);
        let rt_ref = Arc::clone(&rt);
        let bc = Arc::clone(&batch_counter);

        group.bench_function("batch_100", |b| {
            b.iter_batched(
                || {
                    let base = bc.fetch_add(100, Ordering::Relaxed);
                    (base..base + 100)
                        .map(|i| (Some(format!("{}", i)), make_doc(i)))
                        .collect::<Vec<_>>()
                },
                |docs| {
                    let idx = Arc::clone(&idx_ref);
                    rt_ref.block_on(async move {
                        for (id, doc) in docs {
                            black_box(idx.index_document(id, doc).await.expect("index"));
                        }
                    })
                },
                BatchSize::SmallInput,
            )
        });
    }

    group.finish();
}

// ─────────────────────────────────────────────────────────────────────────────
// Aggregation benchmark
// ─────────────────────────────────────────────────────────────────────────────

/// Benchmark aggregation execution over 10K documents.
fn bench_aggregations(c: &mut Criterion) {
    let (engine, _dir) = engine_with_docs(10_000);
    let idx = engine.get_index("bench").expect("get index");
    let rt = Arc::new(tokio::runtime::Runtime::new().expect("tokio runtime"));

    let mut group = c.benchmark_group("aggregations");
    group.throughput(Throughput::Elements(1));

    let agg_queries: &[(&str, Value)] = &[
        (
            "terms_agg",
            json!({
                "query": {"match_all": {}},
                "size": 0,
                "aggs": {
                    "by_category": {"terms": {"field": "category", "size": 10}}
                }
            }),
        ),
        (
            "stats_agg",
            json!({
                "query": {"match_all": {}},
                "size": 0,
                "aggs": {
                    "score_stats": {"stats": {"field": "score"}}
                }
            }),
        ),
        (
            "histogram_agg",
            json!({
                "query": {"match_all": {}},
                "size": 0,
                "aggs": {
                    "score_hist": {"histogram": {"field": "score", "interval": 1.0}}
                }
            }),
        ),
        (
            "multi_agg",
            json!({
                "query": {"match": {"content": "document"}},
                "size": 0,
                "aggs": {
                    "by_category": {"terms": {"field": "category", "size": 10}},
                    "avg_score":   {"avg":   {"field": "score"}},
                    "max_score":   {"max":   {"field": "score"}}
                }
            }),
        ),
    ];

    for (name, request_body) in agg_queries {
        let request: SearchRequest = parse_request(request_body).expect("parse agg request");

        let idx_ref = Arc::clone(&idx);
        let rt_ref = Arc::clone(&rt);

        group.bench_with_input(BenchmarkId::from_parameter(name), name, |b, _| {
            b.iter(|| {
                let idx = Arc::clone(&idx_ref);
                let req = request.clone();
                rt_ref.block_on(async move {
                    black_box(idx.search(&req).await.expect("search"))
                })
            });
        });
    }

    group.finish();
}

// ─────────────────────────────────────────────────────────────────────────────
// Registration
// ─────────────────────────────────────────────────────────────────────────────

criterion_group!(
    benches,
    bench_bm25_search,
    bench_indexing,
    bench_aggregations,
);
criterion_main!(benches);
