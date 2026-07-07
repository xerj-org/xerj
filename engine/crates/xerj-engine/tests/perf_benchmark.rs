//! End-to-end performance benchmarks for xerj.
//!
//! These are *integration-level* performance tests, not micro-benchmarks.
//! They measure real throughput and latency across the full Engine stack.
//!
//! All tests are marked `#[ignore]` so they don't slow down `cargo test`.
//!
//! # Running
//!
//! ```bash
//! # All perf benchmarks with output
//! cargo test -p xerj-engine --test perf_benchmark -- --ignored --nocapture
//!
//! # Single benchmark
//! cargo test -p xerj-engine --test perf_benchmark perf_indexing_throughput -- --ignored --nocapture
//! ```

use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::bulk::process_bulk;
use xerj_engine::Engine;
use xerj_query::parse_request;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build an engine with large flush thresholds (so segments don't form
/// mid-benchmark and pollute timing).
fn bench_engine() -> (Engine, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.storage.flush_size_mb = 8192;
    config.storage.flush_interval_secs = 86400;
    let engine = Engine::new(config).expect("engine::new");
    (engine, dir)
}

/// Generate a realistic document body for document index `i`.
fn make_doc(i: usize) -> Value {
    let categories = [
        "technology",
        "science",
        "politics",
        "sports",
        "arts",
        "health",
        "finance",
    ];
    let cat = categories[i % categories.len()];
    let score = (i % 100) as f64 / 10.0;
    json!({
        "id": i,
        "title": format!("Document {} — a deep dive into {}", i, cat),
        "body": format!(
            "This document (number {i}) covers topics in {cat}. \
             It contains enough prose to exercise BM25 scoring meaningfully. \
             Keywords: alpha beta gamma delta epsilon zeta eta theta iota kappa \
             lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega. \
             Additional context: the year is {year}, the author is Author{author}.",
            i = i,
            cat = cat,
            year = 2000 + (i % 25),
            author = i % 50,
        ),
        "category": cat,
        "score": score,
        "tags": [cat, format!("tag{}", i % 10)],
        "published": !i.is_multiple_of(3),
        "price": (i % 500) as f64 + 0.99,
    })
}

/// Compute percentiles from a sorted slice of durations.
fn percentiles(sorted: &[Duration], ps: &[u32]) -> Vec<Duration> {
    ps.iter()
        .map(|&p| {
            if sorted.is_empty() {
                Duration::ZERO
            } else {
                let idx = ((p as usize) * sorted.len()).saturating_sub(1) / 100;
                sorted[idx.min(sorted.len() - 1)]
            }
        })
        .collect()
}

fn print_header(title: &str) {
    println!("\n{}", "═".repeat(60));
    println!("  {}", title);
    println!("{}", "═".repeat(60));
}

fn print_kv(key: &str, value: &str) {
    println!("  {:<30} {}", key, value);
}

// ═════════════════════════════════════════════════════════════════════════════
// Benchmark 1 — Indexing Throughput
// ═════════════════════════════════════════════════════════════════════════════

/// Index 100 000 documents one at a time and report docs/sec.
///
/// This exercises the hot path: WAL write, memtable insert, BM25 term
/// extraction, schema inference.
#[tokio::test]
#[ignore]
async fn perf_indexing_throughput() {
    const N: usize = 100_000;

    print_header("Indexing Throughput (100 K docs, single-doc API)");

    let (engine, _dir) = bench_engine();
    engine.create_index("bench", Schema::empty()).unwrap();
    let idx = engine.get_index("bench").unwrap();

    let start = Instant::now();
    for i in 0..N {
        idx.index_document(Some(format!("doc{i}")), make_doc(i))
            .await
            .unwrap();
    }
    let elapsed = start.elapsed();

    let docs_per_sec = N as f64 / elapsed.as_secs_f64();
    let ms_per_doc = elapsed.as_millis() as f64 / N as f64;

    print_kv("Documents indexed:", &format!("{}", N));
    print_kv("Total time:", &format!("{:.2?}", elapsed));
    print_kv("Throughput:", &format!("{:.0} docs/sec", docs_per_sec));
    print_kv("Avg latency per doc:", &format!("{:.3} ms", ms_per_doc));
    println!();
}

// ═════════════════════════════════════════════════════════════════════════════
// Benchmark 2 — Search Latency Distribution
// ═════════════════════════════════════════════════════════════════════════════

/// Index 10 000 documents, then run 1 000 searches and measure p50/p95/p99.
///
/// Each search is a `match` query on a rotating set of keywords so the
/// result set is non-trivial (exercises BM25 ranking).
#[tokio::test]
#[ignore]
async fn perf_search_latency() {
    const INDEX_N: usize = 10_000;
    const QUERY_N: usize = 1_000;

    print_header("Search Latency — p50 / p95 / p99 (10 K docs, 1 K queries)");

    let (engine, _dir) = bench_engine();
    engine.create_index("bench", Schema::empty()).unwrap();
    let idx = engine.get_index("bench").unwrap();

    // Index phase
    let index_start = Instant::now();
    for i in 0..INDEX_N {
        idx.index_document(Some(format!("d{i}")), make_doc(i))
            .await
            .unwrap();
    }
    let index_time = index_start.elapsed();
    print_kv(
        "Index phase:",
        &format!(
            "{:.2?} ({:.0} docs/sec)",
            index_time,
            INDEX_N as f64 / index_time.as_secs_f64()
        ),
    );

    // Query terms cycling over categories and body keywords
    let terms = [
        "technology",
        "science",
        "politics",
        "sports",
        "arts",
        "alpha",
        "beta",
        "gamma",
        "delta",
        "omega",
    ];

    // Warm up (not measured)
    for term in &terms {
        let req = parse_request(&json!({
            "query": { "match": { "body": term } },
            "size": 10
        }))
        .unwrap();
        let _ = idx.search(&req).await.unwrap();
    }

    // Measured query loop
    let mut latencies: Vec<Duration> = Vec::with_capacity(QUERY_N);
    let query_start = Instant::now();

    for i in 0..QUERY_N {
        let term = terms[i % terms.len()];
        let req = parse_request(&json!({
            "query": { "match": { "body": term } },
            "size": 10
        }))
        .unwrap();

        let t0 = Instant::now();
        let _ = idx.search(&req).await.unwrap();
        latencies.push(t0.elapsed());
    }

    let total_query_time = query_start.elapsed();
    latencies.sort();

    let pcts = percentiles(&latencies, &[50, 95, 99]);
    let qps = QUERY_N as f64 / total_query_time.as_secs_f64();
    let avg = total_query_time / QUERY_N as u32;

    print_kv("Queries run:", &format!("{}", QUERY_N));
    print_kv("Total query time:", &format!("{:.2?}", total_query_time));
    print_kv("Throughput:", &format!("{:.0} queries/sec", qps));
    print_kv("Avg latency:", &format!("{:.2?}", avg));
    print_kv("p50:", &format!("{:.2?}", pcts[0]));
    print_kv("p95:", &format!("{:.2?}", pcts[1]));
    print_kv("p99:", &format!("{:.2?}", pcts[2]));
    print_kv("min:", &format!("{:.2?}", latencies[0]));
    print_kv("max:", &format!("{:.2?}", latencies[latencies.len() - 1]));
    println!();
}

// ═════════════════════════════════════════════════════════════════════════════
// Benchmark 3 — Bulk Indexing Throughput
// ═════════════════════════════════════════════════════════════════════════════

/// Bulk index 50 000 documents in batches of 1 000 and report docs/sec and MB/sec.
///
/// This is the primary ingest path for log pipelines and data migrations.
#[tokio::test]
#[ignore]
async fn perf_bulk_throughput() {
    const TOTAL_DOCS: usize = 50_000;
    const BATCH_SIZE: usize = 1_000;

    print_header("Bulk Indexing Throughput (50 K docs, batch=1 000)");

    let (engine, _dir) = bench_engine();
    engine.create_index("bulk_bench", Schema::empty()).unwrap();

    let total_batches = TOTAL_DOCS / BATCH_SIZE;
    let mut total_bytes: usize = 0;

    let start = Instant::now();
    let mut docs_indexed: usize = 0;

    for batch in 0..total_batches {
        let mut ndjson = String::with_capacity(BATCH_SIZE * 200);
        for j in 0..BATCH_SIZE {
            let doc_idx = batch * BATCH_SIZE + j;
            let action =
                format!("{{\"index\":{{\"_index\":\"bulk_bench\",\"_id\":\"{doc_idx}\"}}}}\n");
            let doc_str = serde_json::to_string(&make_doc(doc_idx)).unwrap() + "\n";
            ndjson.push_str(&action);
            ndjson.push_str(&doc_str);
        }

        total_bytes += ndjson.len();
        let result = process_bulk(&engine, Some("bulk_bench"), &ndjson).await;
        assert!(!result.errors, "bulk batch {} had errors", batch);
        docs_indexed += result.items.len();
    }

    let elapsed = start.elapsed();
    let docs_per_sec = docs_indexed as f64 / elapsed.as_secs_f64();
    let mb_per_sec = (total_bytes as f64 / 1_048_576.0) / elapsed.as_secs_f64();
    let total_mb = total_bytes as f64 / 1_048_576.0;

    print_kv("Documents indexed:", &format!("{}", docs_indexed));
    print_kv(
        "Batches:",
        &format!("{} × {} docs", total_batches, BATCH_SIZE),
    );
    print_kv("Total payload size:", &format!("{:.1} MB", total_mb));
    print_kv("Total time:", &format!("{:.2?}", elapsed));
    print_kv(
        "Throughput (docs):",
        &format!("{:.0} docs/sec", docs_per_sec),
    );
    print_kv("Throughput (data):", &format!("{:.1} MB/sec", mb_per_sec));
    println!();
}

// ═════════════════════════════════════════════════════════════════════════════
// Benchmark 4 — Aggregation Speed
// ═════════════════════════════════════════════════════════════════════════════

/// Index 50 000 documents then run a complex multi-agg query 100 times.
///
/// Tests the aggregation execution engine at realistic scale: terms, stats,
/// range, and histogram all running in a single request.
#[tokio::test]
#[ignore]
async fn perf_aggregation_speed() {
    const INDEX_N: usize = 50_000;
    const QUERY_N: usize = 100;

    print_header("Aggregation Speed (50 K docs, complex multi-agg, 100 runs)");

    let (engine, _dir) = bench_engine();
    engine.create_index("agg_bench", Schema::empty()).unwrap();
    let idx = engine.get_index("agg_bench").unwrap();

    // Index phase
    let index_start = Instant::now();
    for i in 0..INDEX_N {
        idx.index_document(Some(format!("d{i}")), make_doc(i))
            .await
            .unwrap();
    }
    let index_time = index_start.elapsed();
    print_kv("Index phase:", &format!("{:.2?}", index_time));

    let req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": {
            "by_category": {
                "terms": { "field": "category", "size": 20 }
            },
            "price_stats": {
                "stats": { "field": "price" }
            },
            "price_ranges": {
                "range": {
                    "field": "price",
                    "ranges": [
                        { "to": 100.0 },
                        { "from": 100.0, "to": 300.0 },
                        { "from": 300.0 }
                    ]
                }
            },
            "price_hist": {
                "histogram": { "field": "price", "interval": 50 }
            }
        }
    }))
    .unwrap();

    // Warm-up
    let _ = idx.search(&req).await.unwrap();

    // Measured loop
    let mut latencies: Vec<Duration> = Vec::with_capacity(QUERY_N);
    let agg_start = Instant::now();

    for _ in 0..QUERY_N {
        let t0 = Instant::now();
        let result = idx.search(&req).await.unwrap();
        latencies.push(t0.elapsed());

        // Quick sanity check on the last run
        let aggs = result.aggs.as_ref().expect("aggs present");
        let _ = aggs["by_category"]["buckets"].as_array().expect("buckets");
    }

    let total_agg_time = agg_start.elapsed();
    latencies.sort();

    let pcts = percentiles(&latencies, &[50, 95, 99]);
    let agg_per_sec = QUERY_N as f64 / total_agg_time.as_secs_f64();

    print_kv("Agg queries run:", &format!("{}", QUERY_N));
    print_kv("Total time:", &format!("{:.2?}", total_agg_time));
    print_kv("Throughput:", &format!("{:.1} aggs/sec", agg_per_sec));
    print_kv("p50:", &format!("{:.2?}", pcts[0]));
    print_kv("p95:", &format!("{:.2?}", pcts[1]));
    print_kv("p99:", &format!("{:.2?}", pcts[2]));
    print_kv("min:", &format!("{:.2?}", latencies[0]));
    print_kv("max:", &format!("{:.2?}", latencies[latencies.len() - 1]));
    println!();
}

// ═════════════════════════════════════════════════════════════════════════════
// Benchmark 5 — Concurrent Search
// ═════════════════════════════════════════════════════════════════════════════

/// Index 10 000 documents, then fire 32 concurrent tasks each running 100 queries.
///
/// Measures aggregate queries/sec and latency distribution under contention,
/// which exercises the `RwLock` hot paths in `Index::search`.
#[tokio::test]
#[ignore]
async fn perf_concurrent_search() {
    const INDEX_N: usize = 10_000;
    const TASKS: usize = 32;
    const QUERIES_PER_TASK: usize = 100;

    print_header("Concurrent Search (10 K docs, 32 tasks × 100 queries)");

    let (engine, _dir) = bench_engine();
    engine.create_index("conc_bench", Schema::empty()).unwrap();
    let idx = engine.get_index("conc_bench").unwrap();

    // Index phase (sequential for reproducibility)
    let index_start = Instant::now();
    for i in 0..INDEX_N {
        idx.index_document(Some(format!("d{i}")), make_doc(i))
            .await
            .unwrap();
    }
    print_kv("Index phase:", &format!("{:.2?}", index_start.elapsed()));

    let idx = std::sync::Arc::new(idx);
    let terms = std::sync::Arc::new([
        "technology",
        "science",
        "politics",
        "sports",
        "arts",
        "alpha",
        "beta",
        "gamma",
        "delta",
        "epsilon",
    ]);

    // Spawn concurrent tasks
    let start = Instant::now();
    let mut handles = Vec::with_capacity(TASKS);

    for task_id in 0..TASKS {
        let idx_clone = std::sync::Arc::clone(&idx);
        let terms_clone = std::sync::Arc::clone(&terms);

        handles.push(tokio::spawn(async move {
            let mut task_latencies: Vec<Duration> = Vec::with_capacity(QUERIES_PER_TASK);
            for q in 0..QUERIES_PER_TASK {
                let term = terms_clone[(task_id * QUERIES_PER_TASK + q) % terms_clone.len()];
                let req = parse_request(&json!({
                    "query": { "match": { "body": term } },
                    "size": 10
                }))
                .unwrap();
                let t0 = Instant::now();
                let _ = idx_clone.search(&req).await.unwrap();
                task_latencies.push(t0.elapsed());
            }
            task_latencies
        }));
    }

    // Collect all latencies
    let mut all_latencies: Vec<Duration> = Vec::with_capacity(TASKS * QUERIES_PER_TASK);
    for handle in handles {
        let task_lats = handle.await.expect("task panicked");
        all_latencies.extend(task_lats);
    }

    let total_time = start.elapsed();
    all_latencies.sort();

    let total_queries = TASKS * QUERIES_PER_TASK;
    let qps = total_queries as f64 / total_time.as_secs_f64();
    let pcts = percentiles(&all_latencies, &[50, 95, 99]);

    print_kv("Concurrent tasks:", &format!("{}", TASKS));
    print_kv("Queries per task:", &format!("{}", QUERIES_PER_TASK));
    print_kv("Total queries:", &format!("{}", total_queries));
    print_kv("Wall-clock time:", &format!("{:.2?}", total_time));
    print_kv("Aggregate throughput:", &format!("{:.0} queries/sec", qps));
    print_kv("p50:", &format!("{:.2?}", pcts[0]));
    print_kv("p95:", &format!("{:.2?}", pcts[1]));
    print_kv("p99:", &format!("{:.2?}", pcts[2]));
    print_kv("min:", &format!("{:.2?}", all_latencies[0]));
    print_kv(
        "max:",
        &format!("{:.2?}", all_latencies[all_latencies.len() - 1]),
    );
    println!();
}

// ═════════════════════════════════════════════════════════════════════════════
// Benchmark 6 — Memory Footprint
// ═════════════════════════════════════════════════════════════════════════════

/// Index 100 000 documents and measure the in-memory footprint (RSS from /proc).
///
/// Reports bytes/doc so you can track regressions in memory efficiency.
///
/// Note: RSS includes all Rust runtime overhead, so the per-doc figure is an
/// upper bound on index-specific memory.  Run in release mode for accuracy.
#[tokio::test]
#[ignore]
async fn perf_memory_footprint() {
    const N: usize = 100_000;

    print_header("Memory Footprint (100 K docs)");

    /// Read the OS page size in bytes.
    fn page_size_bytes() -> u64 {
        // Try reading from /proc/self/smaps or fall back to 4096.
        // On Linux, the page size is typically 4096.
        #[cfg(target_os = "linux")]
        {
            // SAFETY: sysconf(_SC_PAGESIZE) is always safe; it reads a kernel
            // constant and never touches user memory.
            extern "C" {
                fn sysconf(name: i32) -> i64;
            }
            let ps = unsafe { sysconf(30) }; // 30 == _SC_PAGESIZE
            if ps > 0 {
                return ps as u64;
            }
        }
        4096
    }

    /// Read the current process RSS in bytes from `/proc/self/statm`.
    fn rss_bytes() -> Option<u64> {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages * page_size_bytes())
    }

    let baseline_rss = rss_bytes().unwrap_or(0);

    let (engine, _dir) = bench_engine();
    engine.create_index("mem_bench", Schema::empty()).unwrap();
    let idx = engine.get_index("mem_bench").unwrap();

    let start = Instant::now();
    for i in 0..N {
        idx.index_document(Some(format!("d{i}")), make_doc(i))
            .await
            .unwrap();
    }
    let elapsed = start.elapsed();

    // Also read Engine's own accounting of memory via IndexStats
    let stats = idx.stats().await;
    let engine_reported_bytes = stats.memtable_size_bytes;

    let rss_after = rss_bytes().unwrap_or(0);
    let rss_delta = rss_after.saturating_sub(baseline_rss);
    let rss_per_doc = if N > 0 { rss_delta / N as u64 } else { 0 };
    let engine_per_doc = if N > 0 { engine_reported_bytes / N } else { 0 };

    print_kv("Documents indexed:", &format!("{}", N));
    print_kv("Indexing time:", &format!("{:.2?}", elapsed));
    print_kv(
        "RSS before:",
        &format!("{:.1} MB", baseline_rss as f64 / 1_048_576.0),
    );
    print_kv(
        "RSS after:",
        &format!("{:.1} MB", rss_after as f64 / 1_048_576.0),
    );
    print_kv(
        "RSS delta:",
        &format!("{:.1} MB", rss_delta as f64 / 1_048_576.0),
    );
    print_kv("RSS bytes/doc:", &format!("{}", rss_per_doc));
    print_kv(
        "Engine-reported memtable:",
        &format!("{:.1} MB", engine_reported_bytes as f64 / 1_048_576.0),
    );
    print_kv("Engine bytes/doc:", &format!("{}", engine_per_doc));
    println!();
}

// ═════════════════════════════════════════════════════════════════════════════
// Benchmark 7 — Turbo Indexing Throughput
// ═════════════════════════════════════════════════════════════════════════════

/// Index 100 000 documents using turbo mode and compare against the standard
/// single-document API.
///
/// Turbo mode uses parallel tokenisation via Rayon and a single write-lock
/// acquisition per batch, amortising lock overhead across many documents.
///
/// # How to run
///
/// ```bash
/// cargo test -p xerj-engine --test perf_benchmark perf_turbo_indexing_throughput \
///     -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore]
async fn perf_turbo_indexing_throughput() {
    const N: usize = 100_000;
    const TURBO_BATCH: usize = 1_000;

    print_header("Turbo Indexing Throughput (100 K docs) — standard vs turbo");

    // ── Standard path ─────────────────────────────────────────────────────

    let (engine_std, _dir_std) = bench_engine();
    engine_std
        .create_index("std_bench", Schema::empty())
        .unwrap();
    let idx_std = engine_std.get_index("std_bench").unwrap();

    let std_start = Instant::now();
    for i in 0..N {
        idx_std
            .index_document(Some(format!("doc{i}")), make_doc(i))
            .await
            .unwrap();
    }
    let std_elapsed = std_start.elapsed();
    let std_docs_per_sec = N as f64 / std_elapsed.as_secs_f64();

    // ── Turbo path ────────────────────────────────────────────────────────

    let (engine_turbo, _dir_turbo) = bench_engine();
    engine_turbo
        .create_index("turbo_bench", Schema::empty())
        .unwrap();
    let idx_turbo = engine_turbo.get_index("turbo_bench").unwrap();

    let turbo_start = Instant::now();
    let mut batch: Vec<(String, serde_json::Value, std::sync::Arc<[u8]>)> =
        Vec::with_capacity(TURBO_BATCH);

    for i in 0..N {
        let empty_bytes: std::sync::Arc<[u8]> = std::sync::Arc::from(&[][..]);
        batch.push((format!("doc{i}"), make_doc(i), empty_bytes));

        if batch.len() >= TURBO_BATCH {
            let b = std::mem::replace(&mut batch, Vec::with_capacity(TURBO_BATCH));
            idx_turbo
                .index_batch_turbo(b, /*parallel=*/ true, /*fast_analyzer=*/ false)
                .await
                .unwrap();
        }
    }

    // Flush the final partial batch.
    if !batch.is_empty() {
        idx_turbo
            .index_batch_turbo(batch, true, false)
            .await
            .unwrap();
    }

    let turbo_elapsed = turbo_start.elapsed();
    let turbo_docs_per_sec = N as f64 / turbo_elapsed.as_secs_f64();
    let speedup = turbo_docs_per_sec / std_docs_per_sec;

    // ── Results ───────────────────────────────────────────────────────────

    print_kv("Documents indexed:", &format!("{}", N));
    print_kv("Batch size (turbo):", &format!("{}", TURBO_BATCH));
    println!();

    print_kv("Standard — total time:", &format!("{:.2?}", std_elapsed));
    print_kv(
        "Standard — throughput:",
        &format!("{:.0} docs/sec", std_docs_per_sec),
    );
    println!();

    print_kv("Turbo    — total time:", &format!("{:.2?}", turbo_elapsed));
    print_kv(
        "Turbo    — throughput:",
        &format!("{:.0} docs/sec", turbo_docs_per_sec),
    );
    println!();

    print_kv("Speedup (turbo / standard):", &format!("{:.2}x", speedup));
    println!();
}
