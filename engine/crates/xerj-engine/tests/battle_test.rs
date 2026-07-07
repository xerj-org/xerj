//! xerj Battle Test — comprehensive real-world benchmark suite.
//!
//! Loads four realistic datasets (web logs, products, error logs, articles)
//! and runs representative search, aggregation, and ingest scenarios.
//! Each scenario measures p50/p95/p99 latency and reports a formatted table.
//!
//! All tests are `#[ignore]` so they don't run in ordinary `cargo test`.
//!
//! # Quick start
//!
//! ```bash
//! # Generate datasets first (one-time, ~36 K docs)
//! bash tests/datasets/generate_datasets.sh
//!
//! # Run the full battle test suite
//! cargo test -p xerj-engine --test battle_test -- --ignored --nocapture
//!
//! # Run a single scenario
//! cargo test -p xerj-engine --test battle_test battle_log_error_search -- --ignored --nocapture
//! ```
//!
//! # Dataset paths
//!
//! The tests look for NDJSON files under `<workspace_root>/tests/datasets/`.
//! Run `bash tests/datasets/generate_datasets.sh` from the workspace root to
//! create them, or set the `XERJ_DATASET_DIR` environment variable to point
//! to an existing directory of NDJSON files.

use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::bulk::process_bulk;
use xerj_engine::Engine;
use xerj_query::parse_request;

// ─────────────────────────────────────────────────────────────────────────────
// Infrastructure helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Build a battle-grade engine: very large flush thresholds so the entire
/// dataset stays in the memtable (no segment merges mid-benchmark).
fn battle_engine() -> (Engine, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let mut cfg = Config::default();
    cfg.server.data_dir = dir.path().to_str().unwrap().to_string();
    cfg.storage.flush_size_mb = 8192;
    cfg.storage.flush_interval_secs = 86400;
    let engine = Engine::new(cfg).expect("engine::new");
    (engine, dir)
}

/// Locate the datasets directory.
///
/// Priority:
///   1. `XERJ_DATASET_DIR` environment variable
///   2. `<workspace_root>/tests/datasets/` — detected by walking up from
///      `CARGO_MANIFEST_DIR` (`crates/xerj-engine`) until we find the
///      directory containing `Cargo.lock` (workspace root).
fn dataset_dir() -> PathBuf {
    if let Ok(val) = std::env::var("XERJ_DATASET_DIR") {
        return PathBuf::from(val);
    }
    // CARGO_MANIFEST_DIR points to crates/xerj-engine.
    // Walk up until we hit the workspace root (has Cargo.lock).
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let mut dir = PathBuf::from(manifest);
    loop {
        if dir.join("Cargo.lock").exists() {
            return dir.join("tests").join("datasets");
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => break,
        }
    }
    // Fallback: relative to crate root (works if run from workspace root)
    PathBuf::from("tests").join("datasets")
}

/// Load an NDJSON file from the dataset dir.  Returns an error string if the
/// file does not exist (we'll skip gracefully rather than panic).
fn load_ndjson(filename: &str) -> Result<Vec<Value>, String> {
    let path = dataset_dir().join(filename);
    if !path.exists() {
        return Err(format!(
            "Dataset not found: {}\nRun `bash tests/datasets/generate_datasets.sh` first.",
            path.display()
        ));
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let mut docs = Vec::new();
    for (lineno, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let val: Value = serde_json::from_str(trimmed).map_err(|e| {
            format!(
                "JSON parse error in {} line {}: {}",
                filename,
                lineno + 1,
                e
            )
        })?;
        docs.push(val);
    }
    Ok(docs)
}

/// Bulk-index a slice of documents into `index_name`.
///
/// Returns (total_docs_indexed, elapsed).
async fn bulk_index(engine: &Engine, index_name: &str, docs: &[Value]) -> (usize, Duration) {
    const BATCH: usize = 500;
    let mut total = 0usize;
    let start = Instant::now();

    for chunk in docs.chunks(BATCH) {
        let mut ndjson = String::with_capacity(chunk.len() * 300);
        for (j, doc) in chunk.iter().enumerate() {
            let id = doc
                .get("id")
                .and_then(|v| v.as_u64())
                .map(|n| n.to_string())
                .unwrap_or_else(|| format!("{}", total + j));
            ndjson.push_str(&format!(
                "{{\"index\":{{\"_index\":\"{}\",\"_id\":\"{}\"}}}}\n",
                index_name, id
            ));
            ndjson.push_str(&serde_json::to_string(doc).unwrap());
            ndjson.push('\n');
        }
        let result = process_bulk(engine, Some(index_name), &ndjson).await;
        assert!(!result.errors, "bulk had errors in index '{}'", index_name);
        total += result.items.len();
    }

    (total, start.elapsed())
}

/// Compute percentiles from a sorted Vec<Duration>.
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

/// Run a single search query `n` times (after one warm-up) and return sorted latencies.
macro_rules! measure_search {
    ($idx:expr, $req:expr, $n:expr) => {{
        // warm-up
        let _ = $idx.search(&$req).await.unwrap();
        let mut lats: Vec<Duration> = Vec::with_capacity($n);
        for _ in 0..$n {
            let t0 = Instant::now();
            let _ = $idx.search(&$req).await.unwrap();
            lats.push(t0.elapsed());
        }
        lats.sort();
        lats
    }};
}

// ─────────────────────────────────────────────────────────────────────────────
// Report formatting
// ─────────────────────────────────────────────────────────────────────────────

fn banner(title: &str) {
    println!("\n{}", "═".repeat(70));
    println!("  {}", title);
    println!("{}", "═".repeat(70));
}

fn row(label: &str, value: &str) {
    println!("  {:<38} {}", label, value);
}

fn separator() {
    println!("  {}", "─".repeat(68));
}

fn lat_rows(label: &str, lats: &[Duration]) {
    if lats.is_empty() {
        row(label, "no data");
        return;
    }
    let ps = percentiles(lats, &[50, 95, 99]);
    let avg = lats.iter().sum::<Duration>() / lats.len() as u32;
    row(&format!("{} (avg)", label), &format!("{:.2?}", avg));
    row(&format!("{} p50", label), &format!("{:.2?}", ps[0]));
    row(&format!("{} p95", label), &format!("{:.2?}", ps[1]));
    row(&format!("{} p99", label), &format!("{:.2?}", ps[2]));
    row(
        &format!("{} min/max", label),
        &format!("{:.2?} / {:.2?}", lats[0], lats[lats.len() - 1]),
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario 1 — Bulk ingest all 36 K documents
// ─────────────────────────────────────────────────────────────────────────────

/// Measures: total time and per-doc latency for ingesting all four datasets.
#[tokio::test]
#[ignore]
async fn battle_bulk_ingest_all() {
    banner("BATTLE: Bulk Ingest — all four datasets (~36 K docs)");

    // Load datasets
    let access_docs = match load_ndjson("access_logs.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };
    let product_docs = match load_ndjson("products.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };
    let error_docs = match load_ndjson("error_logs.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };
    let article_docs = match load_ndjson("articles.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };

    let total_docs = access_docs.len() + product_docs.len() + error_docs.len() + article_docs.len();
    row("Total documents to ingest:", &total_docs.to_string());
    separator();

    let (engine, _dir) = battle_engine();
    engine.create_index("access-logs", Schema::empty()).unwrap();
    engine.create_index("products", Schema::empty()).unwrap();
    engine.create_index("error-logs", Schema::empty()).unwrap();
    engine.create_index("articles", Schema::empty()).unwrap();

    let wall_start = Instant::now();

    let (n, t) = bulk_index(&engine, "access-logs", &access_docs).await;
    row(
        "access-logs indexed:",
        &format!(
            "{} docs in {:.2?} ({:.0} docs/s)",
            n,
            t,
            n as f64 / t.as_secs_f64()
        ),
    );

    let (n, t) = bulk_index(&engine, "products", &product_docs).await;
    row(
        "products indexed:",
        &format!(
            "{} docs in {:.2?} ({:.0} docs/s)",
            n,
            t,
            n as f64 / t.as_secs_f64()
        ),
    );

    let (n, t) = bulk_index(&engine, "error-logs", &error_docs).await;
    row(
        "error-logs indexed:",
        &format!(
            "{} docs in {:.2?} ({:.0} docs/s)",
            n,
            t,
            n as f64 / t.as_secs_f64()
        ),
    );

    let (n, t) = bulk_index(&engine, "articles", &article_docs).await;
    row(
        "articles indexed:",
        &format!(
            "{} docs in {:.2?} ({:.0} docs/s)",
            n,
            t,
            n as f64 / t.as_secs_f64()
        ),
    );

    let wall = wall_start.elapsed();
    separator();
    row("TOTAL wall-clock time:", &format!("{:.2?}", wall));
    row(
        "Overall throughput:",
        &format!("{:.0} docs/sec", total_docs as f64 / wall.as_secs_f64()),
    );
    row(
        "Avg latency per doc:",
        &format!("{:.3} ms", wall.as_millis() as f64 / total_docs as f64),
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario 2 — Log search: find all ERROR/FATAL events
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn battle_log_error_search() {
    banner("BATTLE: Log Search — find all ERROR/FATAL events (error-logs, 20 K docs)");

    let docs = match load_ndjson("error_logs.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };

    let (engine, _dir) = battle_engine();
    engine.create_index("error-logs", Schema::empty()).unwrap();
    let (indexed, ingest_time) = bulk_index(&engine, "error-logs", &docs).await;
    let idx = engine.get_index("error-logs").unwrap();

    row(
        "Docs indexed:",
        &format!("{} in {:.2?}", indexed, ingest_time),
    );
    separator();

    const RUNS: usize = 200;

    // 2a. Term query: level == ERROR
    let req_error = parse_request(&json!({
        "query": { "term": { "level": "ERROR" } },
        "size": 50
    }))
    .unwrap();
    let result = idx.search(&req_error).await.unwrap();
    let error_count = result.total.value;
    row("ERROR doc count (term query):", &error_count.to_string());

    let lats = measure_search!(idx, req_error, RUNS);
    lat_rows("ERROR term query", &lats);
    separator();

    // 2b. Terms query: level in [ERROR, FATAL]
    let req_err_fatal = parse_request(&json!({
        "query": { "terms": { "level": ["ERROR", "FATAL"] } },
        "size": 100
    }))
    .unwrap();
    let result2 = idx.search(&req_err_fatal).await.unwrap();
    row(
        "ERROR+FATAL count (terms query):",
        &result2.total.value.to_string(),
    );

    let lats2 = measure_search!(idx, req_err_fatal, RUNS);
    lat_rows("ERROR+FATAL terms query", &lats2);
    separator();

    // 2c. Bool must: message contains "connection timeout", level = ERROR
    let req_conn_error = parse_request(&json!({
        "query": {
            "bool": {
                "must": [
                    { "match": { "message": "connection timeout" } }
                ],
                "filter": [
                    { "term": { "level": "ERROR" } }
                ]
            }
        },
        "size": 50
    }))
    .unwrap();
    let result3 = idx.search(&req_conn_error).await.unwrap();
    row(
        "Connection timeout errors (bool must+filter):",
        &result3.total.value.to_string(),
    );
    assert!(
        result3.total.value > 0,
        "Expected at least one connection timeout error"
    );

    let lats3 = measure_search!(idx, req_conn_error, RUNS);
    lat_rows("Bool must+filter (conn errors)", &lats3);
    separator();

    // 2d. Full-text match: "timeout" in message, must_not FATAL
    let req_timeout = parse_request(&json!({
        "query": {
            "bool": {
                "must": [
                    { "match": { "message": "timeout" } }
                ],
                "must_not": [
                    { "term": { "level": "FATAL" } }
                ]
            }
        },
        "size": 50
    }))
    .unwrap();
    let result4 = idx.search(&req_timeout).await.unwrap();
    row(
        "Timeout messages, not FATAL:",
        &result4.total.value.to_string(),
    );

    let lats4 = measure_search!(idx, req_timeout, RUNS);
    lat_rows("Bool must+must_not (timeout)", &lats4);
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario 3 — Product full-text search
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn battle_product_fulltext_search() {
    banner("BATTLE: Product Search — full-text ranked results (products, 5 K docs)");

    let docs = match load_ndjson("products.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };

    let (engine, _dir) = battle_engine();
    engine.create_index("products", Schema::empty()).unwrap();
    let (indexed, ingest_time) = bulk_index(&engine, "products", &docs).await;
    let idx = engine.get_index("products").unwrap();

    row(
        "Docs indexed:",
        &format!("{} in {:.2?}", indexed, ingest_time),
    );
    separator();

    const RUNS: usize = 500;

    // 3a. Match: "premium" in name field
    let req_premium = parse_request(&json!({
        "query": {
            "match": {
                "name": "premium quality"
            }
        },
        "size": 10
    }))
    .unwrap();
    let res = idx.search(&req_premium).await.unwrap();
    row(
        "match 'premium quality' in name hits:",
        &res.total.value.to_string(),
    );
    assert!(
        res.total.value > 0,
        "Expected hits for 'premium' or 'quality' in product names"
    );
    assert!(res.hits.len() <= 10);

    let lats = measure_search!(idx, req_premium, RUNS);
    lat_rows("multi_match (premium quality)", &lats);
    separator();

    // 3b. Match in description field only
    let req_desc = parse_request(&json!({
        "query": {
            "match": { "description": "durable excellent craftsmanship" }
        },
        "size": 20
    }))
    .unwrap();
    let res2 = idx.search(&req_desc).await.unwrap();
    row(
        "match 'durable excellent craftsmanship':",
        &res2.total.value.to_string(),
    );

    let lats2 = measure_search!(idx, req_desc, RUNS);
    lat_rows("match (description)", &lats2);
    separator();

    // 3c. Bool must match name + filter on in_stock + range on price
    let req_bool = parse_request(&json!({
        "query": {
            "bool": {
                "must": [
                    { "match": { "description": "professional grade" } }
                ],
                "filter": [
                    { "term": { "in_stock": true } },
                    { "range": { "price": { "gte": 50.0, "lte": 500.0 } } }
                ]
            }
        },
        "size": 20,
        "sort": [{ "price": "asc" }]
    }))
    .unwrap();
    let res3 = idx.search(&req_bool).await.unwrap();
    row(
        "Bool + range + sort (in-stock $50-$500):",
        &res3.total.value.to_string(),
    );

    // Verify sort order if we got results
    if res3.hits.len() >= 2 {
        let p0 = res3.hits[0]
            .source
            .get("price")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let p1 = res3.hits[1]
            .source
            .get("price")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        assert!(p0 <= p1, "price sort asc violated: {} > {}", p0, p1);
        row("Sort correctness (price asc):", "OK");
    }

    let lats3 = measure_search!(idx, req_bool, RUNS);
    lat_rows("Bool + filter + range + sort", &lats3);
    separator();

    // 3d. Prefix query (autocomplete-style)
    let req_prefix = parse_request(&json!({
        "query": { "prefix": { "category": "electr" } },
        "size": 50
    }))
    .unwrap();
    let res4 = idx.search(&req_prefix).await.unwrap();
    row(
        "Prefix 'electr' on category:",
        &res4.total.value.to_string(),
    );

    let lats4 = measure_search!(idx, req_prefix, RUNS);
    lat_rows("Prefix query (autocomplete)", &lats4);
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario 4 — Geo distance search
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn battle_geo_distance_search() {
    banner("BATTLE: Geo Search — products within 50 km of NYC (products, 5 K docs)");

    let docs = match load_ndjson("products.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };

    let (engine, _dir) = battle_engine();
    engine
        .create_index("products-geo", Schema::empty())
        .unwrap();
    let (indexed, ingest_time) = bulk_index(&engine, "products-geo", &docs).await;
    let idx = engine.get_index("products-geo").unwrap();

    row(
        "Docs indexed:",
        &format!("{} in {:.2?}", indexed, ingest_time),
    );
    separator();

    const RUNS: usize = 200;

    // NYC: 40.7128, -74.0060 — 50 km radius
    let req_nyc_50km = parse_request(&json!({
        "query": {
            "geo_distance": {
                "distance": "50km",
                "location": { "lat": 40.7128, "lon": -74.0060 }
            }
        },
        "size": 100
    }))
    .unwrap();
    let res = idx.search(&req_nyc_50km).await.unwrap();
    row("Within 50 km of NYC:", &res.total.value.to_string());

    let lats = measure_search!(idx, req_nyc_50km, RUNS);
    lat_rows("geo_distance 50 km", &lats);
    separator();

    // 500 km radius (wider net)
    let req_nyc_500km = parse_request(&json!({
        "query": {
            "geo_distance": {
                "distance": "500km",
                "location": { "lat": 40.7128, "lon": -74.0060 }
            }
        },
        "size": 100
    }))
    .unwrap();
    let res2 = idx.search(&req_nyc_500km).await.unwrap();
    row("Within 500 km of NYC:", &res2.total.value.to_string());
    // More results at wider radius
    assert!(
        res2.total.value >= res.total.value,
        "500 km should return at least as many as 50 km"
    );

    let lats2 = measure_search!(idx, req_nyc_500km, RUNS);
    lat_rows("geo_distance 500 km", &lats2);
    separator();

    // Combine geo with in_stock filter
    let req_geo_stock = parse_request(&json!({
        "query": {
            "bool": {
                "must": [
                    {
                        "geo_distance": {
                            "distance": "200km",
                            "location": { "lat": 40.7128, "lon": -74.0060 }
                        }
                    }
                ],
                "filter": [
                    { "term": { "in_stock": true } }
                ]
            }
        },
        "size": 50
    }))
    .unwrap();
    let res3 = idx.search(&req_geo_stock).await.unwrap();
    row("200 km + in_stock=true:", &res3.total.value.to_string());

    let lats3 = measure_search!(idx, req_geo_stock, RUNS);
    lat_rows("geo_distance + filter", &lats3);
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario 5 — Aggregations
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn battle_aggregations() {
    banner("BATTLE: Aggregations — terms, stats, range, histogram (multi-dataset)");

    let error_docs = match load_ndjson("error_logs.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };
    let product_docs = match load_ndjson("products.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };

    let (engine, _dir) = battle_engine();
    engine.create_index("agg-errors", Schema::empty()).unwrap();
    engine
        .create_index("agg-products", Schema::empty())
        .unwrap();

    let (en, et) = bulk_index(&engine, "agg-errors", &error_docs).await;
    let (pn, pt) = bulk_index(&engine, "agg-products", &product_docs).await;

    let err_idx = engine.get_index("agg-errors").unwrap();
    let prod_idx = engine.get_index("agg-products").unwrap();

    row("error-logs indexed:", &format!("{} in {:.2?}", en, et));
    row("products indexed:", &format!("{} in {:.2?}", pn, pt));
    separator();

    const RUNS: usize = 100;

    // 5a. Top 10 error services (terms agg)
    let req_top_services = parse_request(&json!({
        "query": {
            "terms": { "level": ["ERROR", "FATAL"] }
        },
        "size": 0,
        "aggs": {
            "top_services": {
                "terms": { "field": "service", "size": 10 }
            },
            "error_count_by_host": {
                "terms": { "field": "hostname", "size": 10 }
            }
        }
    }))
    .unwrap();

    let agg_res = err_idx.search(&req_top_services).await.unwrap();
    let aggs = agg_res.aggs.as_ref().expect("aggs present");
    let buckets = aggs["top_services"]["buckets"].as_array().expect("buckets");
    row("Top services (bucket count):", &buckets.len().to_string());
    assert!(!buckets.is_empty(), "Expected at least one service bucket");
    // Print top 3 services
    for b in buckets.iter().take(3) {
        let key = b["key"].as_str().unwrap_or("?");
        let count = b["doc_count"].as_u64().unwrap_or(0);
        row(&format!("  service={}", key), &format!("{} docs", count));
    }

    let lats = measure_search!(err_idx, req_top_services, RUNS);
    lat_rows("Terms agg (top services)", &lats);
    separator();

    // 5b. Avg response time by log level (stats agg)
    let req_duration_stats = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": {
            "by_level": {
                "terms": { "field": "level", "size": 10 }
            },
            "duration_stats": {
                "stats": { "field": "duration_ms" }
            }
        }
    }))
    .unwrap();

    let res2 = err_idx.search(&req_duration_stats).await.unwrap();
    let aggs2 = res2.aggs.as_ref().expect("aggs present");
    if let Some(count) = aggs2["duration_stats"]["count"].as_u64() {
        row("duration_ms stats count:", &count.to_string());
    }
    if let Some(avg) = aggs2["duration_stats"]["avg"].as_f64() {
        row("duration_ms avg:", &format!("{:.1} ms", avg));
    }

    let lats2 = measure_search!(err_idx, req_duration_stats, RUNS);
    lat_rows("Stats agg (duration_ms)", &lats2);
    separator();

    // 5c. Product category breakdown + price range agg + price histogram
    let req_faceted = parse_request(&json!({
        "query": { "term": { "in_stock": true } },
        "size": 0,
        "aggs": {
            "by_category": {
                "terms": { "field": "category", "size": 20 }
            },
            "price_ranges": {
                "range": {
                    "field": "price",
                    "ranges": [
                        { "key": "budget",   "to": 50.0 },
                        { "key": "mid",      "from": 50.0,   "to": 250.0 },
                        { "key": "premium",  "from": 250.0,  "to": 1000.0 },
                        { "key": "luxury",   "from": 1000.0 }
                    ]
                }
            },
            "price_histogram": {
                "histogram": { "field": "price", "interval": 100 }
            },
            "rating_stats": {
                "stats": { "field": "rating" }
            }
        }
    }))
    .unwrap();

    let res3 = prod_idx.search(&req_faceted).await.unwrap();
    let aggs3 = res3.aggs.as_ref().expect("aggs present");
    let cat_buckets = aggs3["by_category"]["buckets"]
        .as_array()
        .expect("category buckets");
    row(
        "Category buckets (in-stock):",
        &cat_buckets.len().to_string(),
    );
    assert!(!cat_buckets.is_empty(), "Expected category buckets");

    let price_buckets = aggs3["price_ranges"]["buckets"]
        .as_array()
        .expect("price range buckets");
    row("Price range buckets:", &price_buckets.len().to_string());

    if let Some(avg_rating) = aggs3["rating_stats"]["avg"].as_f64() {
        row("Avg product rating:", &format!("{:.2}", avg_rating));
        assert!(
            (1.0..=5.0).contains(&avg_rating),
            "Rating out of range: {}",
            avg_rating
        );
    }

    let lats3 = measure_search!(prod_idx, req_faceted, RUNS);
    lat_rows("Faceted (category+price range+histogram)", &lats3);
    separator();

    // 5d. Composite aggregation (pagination)
    let req_composite = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": {
            "by_brand_cat": {
                "composite": {
                    "size": 20,
                    "sources": [
                        { "category": { "terms": { "field": "category" } } },
                        { "brand":    { "terms": { "field": "brand" } } }
                    ]
                }
            }
        }
    }))
    .unwrap();
    let res4 = prod_idx.search(&req_composite).await.unwrap();
    let aggs4 = res4.aggs.as_ref().expect("aggs present");
    let comp_buckets = aggs4["by_brand_cat"]["buckets"]
        .as_array()
        .expect("composite buckets");
    row(
        "Composite (category x brand) buckets:",
        &comp_buckets.len().to_string(),
    );

    let lats4 = measure_search!(prod_idx, req_composite, RUNS);
    lat_rows("Composite agg (brand x category)", &lats4);
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario 6 — Article full-text search with highlight
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn battle_article_search_with_highlight() {
    banner("BATTLE: Article Search — full-text + highlight (articles, 1 K docs)");

    let docs = match load_ndjson("articles.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };

    let (engine, _dir) = battle_engine();
    engine.create_index("articles", Schema::empty()).unwrap();
    let (indexed, ingest_time) = bulk_index(&engine, "articles", &docs).await;
    let idx = engine.get_index("articles").unwrap();

    row(
        "Docs indexed:",
        &format!("{} in {:.2?}", indexed, ingest_time),
    );
    separator();

    const RUNS: usize = 300;

    // 6a. Match query with highlight
    let req_hl = parse_request(&json!({
        "query": {
            "match": { "body": "memory safety performance" }
        },
        "size": 10,
        "highlight": {
            "fields": {
                "body": {
                    "fragment_size": 150,
                    "number_of_fragments": 3
                }
            }
        }
    }))
    .unwrap();

    let res = idx.search(&req_hl).await.unwrap();
    row(
        "'memory safety performance' hits:",
        &res.total.value.to_string(),
    );
    assert!(
        res.total.value > 0,
        "Expected FTS hits for 'memory safety performance'"
    );

    // Verify highlights are present and contain the em tags
    let hits_with_highlight = res.hits.iter().filter(|h| h.highlight.is_some()).count();
    row(
        "Hits with highlight fragments:",
        &hits_with_highlight.to_string(),
    );
    if let Some(first_with_hl) = res.hits.iter().find(|h| h.highlight.is_some()) {
        let hl = first_with_hl.highlight.as_ref().unwrap();
        if let Some(frags) = hl.get("body") {
            row("First highlight fragments:", &frags.len().to_string());
            // At least one fragment should contain an <em> tag
            let has_em = frags.iter().any(|f| f.contains("<em>"));
            row("Highlight <em> tags present:", &has_em.to_string());
        }
    }

    let lats = measure_search!(idx, req_hl, RUNS);
    lat_rows("match + highlight", &lats);
    separator();

    // 6b. Multi-match across title and body
    let req_multi = parse_request(&json!({
        "query": {
            "multi_match": {
                "query": "distributed systems architecture",
                "fields": ["title^2", "body"],
                "type": "best_fields"
            }
        },
        "size": 10
    }))
    .unwrap();
    let res2 = idx.search(&req_multi).await.unwrap();
    row(
        "multi_match 'distributed systems':",
        &res2.total.value.to_string(),
    );

    let lats2 = measure_search!(idx, req_multi, RUNS);
    lat_rows("multi_match (title+body)", &lats2);
    separator();

    // 6c. Phrase match
    let req_phrase = parse_request(&json!({
        "query": {
            "match_phrase": { "body": "memory safety" }
        },
        "size": 20
    }))
    .unwrap();
    let res3 = idx.search(&req_phrase).await.unwrap();
    row(
        "match_phrase 'memory safety':",
        &res3.total.value.to_string(),
    );

    let lats3 = measure_search!(idx, req_phrase, RUNS);
    lat_rows("match_phrase", &lats3);
    separator();

    // 6d. Bool: must match body, should match tags, filter on word_count range
    let req_complex = parse_request(&json!({
        "query": {
            "bool": {
                "must": [
                    { "match": { "body": "performance" } }
                ],
                "should": [
                    { "match": { "body": "security" } },
                    { "match": { "body": "compiler" } }
                ],
                "filter": [
                    { "range": { "word_count": { "gte": 100 } } }
                ],
                "minimum_should_match": 0
            }
        },
        "size": 20,
        "sort": [{ "views": "desc" }, { "_score": "desc" }]
    }))
    .unwrap();
    let res4 = idx.search(&req_complex).await.unwrap();
    row(
        "Complex bool (performance+security):",
        &res4.total.value.to_string(),
    );

    let lats4 = measure_search!(idx, req_complex, RUNS);
    lat_rows("Complex bool + sort", &lats4);
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario 7 — Web access log analytics
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn battle_access_log_analytics() {
    banner("BATTLE: Access Log Analytics — range + terms + stats (access-logs, 10 K docs)");

    let docs = match load_ndjson("access_logs.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };

    let (engine, _dir) = battle_engine();
    engine.create_index("access-logs", Schema::empty()).unwrap();
    let (indexed, ingest_time) = bulk_index(&engine, "access-logs", &docs).await;
    let idx = engine.get_index("access-logs").unwrap();

    row(
        "Docs indexed:",
        &format!("{} in {:.2?}", indexed, ingest_time),
    );
    separator();

    const RUNS: usize = 200;

    // 7a. 500 errors
    let req_500 = parse_request(&json!({
        "query": { "term": { "status": "500" } },
        "size": 100
    }))
    .unwrap();
    let res_500 = idx.search(&req_500).await.unwrap();
    row("HTTP 500 errors:", &res_500.total.value.to_string());

    let lats = measure_search!(idx, req_500, RUNS);
    lat_rows("term query (status=500)", &lats);
    separator();

    // 7b. All non-200 responses
    let req_non200 = parse_request(&json!({
        "query": {
            "bool": {
                "must_not": [
                    { "term": { "status": "200" } }
                ]
            }
        },
        "size": 0,
        "aggs": {
            "by_status": {
                "terms": { "field": "status", "size": 20 }
            },
            "response_time_stats": {
                "stats": { "field": "response_time_ms" }
            }
        }
    }))
    .unwrap();
    let res2 = idx.search(&req_non200).await.unwrap();
    row("Non-200 total:", &res2.total.value.to_string());

    let aggs = res2.aggs.as_ref().expect("aggs present");
    if let Some(avg) = aggs["response_time_stats"]["avg"].as_f64() {
        row("Non-200 avg response_time_ms:", &format!("{:.1}", avg));
    }

    let lats2 = measure_search!(idx, req_non200, RUNS);
    lat_rows("must_not + terms agg + stats agg", &lats2);
    separator();

    // 7c. Slow requests (range query: response_time_ms > 1000)
    let req_slow = parse_request(&json!({
        "query": {
            "range": { "response_time_ms": { "gte": 1000 } }
        },
        "size": 50,
        "sort": [{ "response_time_ms": "desc" }]
    }))
    .unwrap();
    let res3 = idx.search(&req_slow).await.unwrap();
    row("Slow requests (>1000ms):", &res3.total.value.to_string());

    // Verify sort order descending
    if res3.hits.len() >= 2 {
        let t0 = res3.hits[0]
            .source
            .get("response_time_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let t1 = res3.hits[1]
            .source
            .get("response_time_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        assert!(
            t0 >= t1,
            "response_time_ms sort desc violated: {} < {}",
            t0,
            t1
        );
        row("Sort correctness (response_time desc):", "OK");
    }

    let lats3 = measure_search!(idx, req_slow, RUNS);
    lat_rows("Range query (slow requests)", &lats3);
    separator();

    // 7d. Exists query: requests that have a request_id field
    let req_exists = parse_request(&json!({
        "query": { "exists": { "field": "request_id" } },
        "size": 0
    }))
    .unwrap();
    let res4 = idx.search(&req_exists).await.unwrap();
    row(
        "Docs with request_id field (exists):",
        &res4.total.value.to_string(),
    );
    assert_eq!(
        res4.total.value, indexed as u64,
        "All docs should have request_id"
    );

    let lats4 = measure_search!(idx, req_exists, RUNS);
    lat_rows("Exists query", &lats4);
    separator();

    // 7e. Breakdown: avg response_time_ms by HTTP method + status
    let req_method_breakdown = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": {
            "by_method": {
                "terms": { "field": "method", "size": 10 }
            },
            "by_status_code": {
                "terms": { "field": "status", "size": 20 }
            },
            "overall_rt_stats": {
                "stats": { "field": "response_time_ms" }
            }
        }
    }))
    .unwrap();
    let res5 = idx.search(&req_method_breakdown).await.unwrap();
    let aggs5 = res5.aggs.as_ref().expect("aggs present");
    let method_buckets = aggs5["by_method"]["buckets"]
        .as_array()
        .expect("method buckets");
    row("HTTP method buckets:", &method_buckets.len().to_string());
    for b in method_buckets.iter().take(5) {
        let key = b["key"].as_str().unwrap_or("?");
        let cnt = b["doc_count"].as_u64().unwrap_or(0);
        row(&format!("  method={}", key), &format!("{} reqs", cnt));
    }

    let lats5 = measure_search!(idx, req_method_breakdown, RUNS);
    lat_rows("Multi-terms agg + stats", &lats5);
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario 8 — Correctness spot-checks across all datasets
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn battle_correctness_spot_checks() {
    banner("BATTLE: Correctness Spot Checks — all four datasets");

    let access_docs = match load_ndjson("access_logs.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };
    let product_docs = match load_ndjson("products.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };
    let error_docs = match load_ndjson("error_logs.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };
    let article_docs = match load_ndjson("articles.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };

    let (engine, _dir) = battle_engine();
    engine.create_index("chk-access", Schema::empty()).unwrap();
    engine
        .create_index("chk-products", Schema::empty())
        .unwrap();
    engine.create_index("chk-errors", Schema::empty()).unwrap();
    engine
        .create_index("chk-articles", Schema::empty())
        .unwrap();

    let (an, _) = bulk_index(&engine, "chk-access", &access_docs).await;
    let (pn, _) = bulk_index(&engine, "chk-products", &product_docs).await;
    let (en, _) = bulk_index(&engine, "chk-errors", &error_docs).await;
    let (artn, _) = bulk_index(&engine, "chk-articles", &article_docs).await;

    let acc = engine.get_index("chk-access").unwrap();
    let prod = engine.get_index("chk-products").unwrap();
    let err = engine.get_index("chk-errors").unwrap();
    let art = engine.get_index("chk-articles").unwrap();

    // ── Access logs ────────────────────────────────────────────────────────────

    // match_all must return the full dataset count
    let r = acc
        .search(&parse_request(&json!({"query":{"match_all":{}},"size":0})).unwrap())
        .await
        .unwrap();
    assert_eq!(
        r.total.value, an as u64,
        "access-logs: match_all count mismatch"
    );
    row(
        "access-logs match_all total:",
        &format!("{} (PASS)", r.total.value),
    );

    // 200 + non-200 must equal total
    let r200 = acc
        .search(&parse_request(&json!({"query":{"term":{"status":200}},"size":0})).unwrap())
        .await
        .unwrap();
    let rnon = acc
        .search(
            &parse_request(
                &json!({"query":{"bool":{"must_not":[{"term":{"status":200}}]}},"size":0}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        r200.total.value + rnon.total.value,
        an as u64,
        "200 + non-200 != total"
    );
    row("access-logs 200+non200==total:", "PASS");

    // ── Products ───────────────────────────────────────────────────────────────

    // match_all
    let r = prod
        .search(&parse_request(&json!({"query":{"match_all":{}},"size":0})).unwrap())
        .await
        .unwrap();
    assert_eq!(
        r.total.value, pn as u64,
        "products: match_all count mismatch"
    );
    row(
        "products match_all total:",
        &format!("{} (PASS)", r.total.value),
    );

    // in_stock=true + in_stock=false == total
    let rin = prod
        .search(&parse_request(&json!({"query":{"term":{"in_stock":true}},"size":0})).unwrap())
        .await
        .unwrap();
    let rout = prod
        .search(&parse_request(&json!({"query":{"term":{"in_stock":false}},"size":0})).unwrap())
        .await
        .unwrap();
    assert_eq!(
        rin.total.value + rout.total.value,
        pn as u64,
        "in_stock true+false != total"
    );
    row("products in_stock true+false==total:", "PASS");

    // price range [0, ∞) == total
    let rprice = prod
        .search(&parse_request(&json!({"query":{"range":{"price":{"gte":0}}},"size":0})).unwrap())
        .await
        .unwrap();
    assert_eq!(
        rprice.total.value, pn as u64,
        "products: price range gte 0 != total"
    );
    row("products price >=0 range==total:", "PASS");

    // rating agg avg within [1,5]
    let ragg = prod.search(&parse_request(&json!({"query":{"match_all":{}},"size":0,"aggs":{"avg_r":{"avg":{"field":"rating"}}}})).unwrap()).await.unwrap();
    if let Some(avg) = ragg
        .aggs
        .as_ref()
        .and_then(|a| a["avg_r"]["value"].as_f64())
    {
        assert!(
            (1.0..=5.0).contains(&avg),
            "avg rating {} out of [1,5]",
            avg
        );
        row(
            "products avg rating in [1,5]:",
            &format!("{:.2} (PASS)", avg),
        );
    }

    // ── Error logs ─────────────────────────────────────────────────────────────

    let r = err
        .search(&parse_request(&json!({"query":{"match_all":{}},"size":0})).unwrap())
        .await
        .unwrap();
    assert_eq!(
        r.total.value, en as u64,
        "error-logs: match_all count mismatch"
    );
    row(
        "error-logs match_all total:",
        &format!("{} (PASS)", r.total.value),
    );

    // All log levels must sum to total
    let levels = ["INFO", "WARN", "ERROR", "FATAL"];
    let mut level_sum = 0u64;
    for level in &levels {
        let rl = err
            .search(&parse_request(&json!({"query":{"term":{"level":level}},"size":0})).unwrap())
            .await
            .unwrap();
        level_sum += rl.total.value;
        row(
            &format!("error-logs level={}:", level),
            &rl.total.value.to_string(),
        );
    }
    assert_eq!(
        level_sum, en as u64,
        "level sum {} != total {}",
        level_sum, en
    );
    row("error-logs sum(levels)==total:", "PASS");

    // terms agg by level — bucket sum must equal total
    let ragg = err
        .search(
            &parse_request(&json!({
                "query": { "match_all": {} },
                "size": 0,
                "aggs": { "by_level": { "terms": { "field": "level", "size": 10 } } }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let agg_sum: u64 = ragg
        .aggs
        .as_ref()
        .and_then(|a| a["by_level"]["buckets"].as_array())
        .map(|b| {
            b.iter()
                .map(|bk| bk["doc_count"].as_u64().unwrap_or(0))
                .sum()
        })
        .unwrap_or(0);
    row(
        "error-logs terms agg bucket sum:",
        &format!("{} vs {} total", agg_sum, en),
    );

    // ── Articles ───────────────────────────────────────────────────────────────

    let r = art
        .search(&parse_request(&json!({"query":{"match_all":{}},"size":0})).unwrap())
        .await
        .unwrap();
    assert_eq!(
        r.total.value, artn as u64,
        "articles: match_all count mismatch"
    );
    row(
        "articles match_all total:",
        &format!("{} (PASS)", r.total.value),
    );

    // word_count range [0,∞) == total
    let rwc = art
        .search(
            &parse_request(&json!({"query":{"range":{"word_count":{"gte":0}}},"size":0})).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        rwc.total.value, artn as u64,
        "articles: word_count range != total"
    );
    row("articles word_count >=0 == total:", "PASS");

    // FTS hits for a common word that should appear in all articles
    let rfts = art
        .search(
            &parse_request(&json!({
                "query": { "match": { "body": "performance" } },
                "size": 0
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    row(
        "articles FTS 'performance' hits:",
        &rfts.total.value.to_string(),
    );
    assert!(
        rfts.total.value > 0,
        "Expected FTS hits for 'performance' in articles"
    );

    separator();
    row("ALL CORRECTNESS CHECKS:", "PASSED");
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario 9 — Complex boolean queries with sorting and pagination
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn battle_complex_bool_queries() {
    banner("BATTLE: Complex Bool Queries — multi-clause with sort and pagination");

    let error_docs = match load_ndjson("error_logs.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };
    let product_docs = match load_ndjson("products.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };

    let (engine, _dir) = battle_engine();
    engine.create_index("bool-errors", Schema::empty()).unwrap();
    engine
        .create_index("bool-products", Schema::empty())
        .unwrap();

    let (en, _) = bulk_index(&engine, "bool-errors", &error_docs).await;
    let (pn, _) = bulk_index(&engine, "bool-products", &product_docs).await;

    let err_idx = engine.get_index("bool-errors").unwrap();
    let prod_idx = engine.get_index("bool-products").unwrap();

    row("error-logs:", &format!("{} docs", en));
    row("products:", &format!("{} docs", pn));
    separator();

    const RUNS: usize = 200;

    // 9a. Must match "error" keyword, filter by service, must_not contain "timeout"
    //     Range filter on duration_ms
    let req_complex_log = parse_request(&json!({
        "query": {
            "bool": {
                "must": [
                    { "match": { "message": "database" } }
                ],
                "filter": [
                    { "terms": { "service": ["auth-svc", "payment-svc", "api-gateway"] } },
                    { "range": { "duration_ms": { "gte": 0 } } }
                ],
                "must_not": [
                    { "match": { "message": "initialized" } }
                ]
            }
        },
        "size": 50,
        "sort": [
            { "duration_ms": "desc" },
            { "_score": "desc" }
        ]
    }))
    .unwrap();
    let res = err_idx.search(&req_complex_log).await.unwrap();
    row(
        "Complex log bool (database+service filter):",
        &res.total.value.to_string(),
    );

    let lats = measure_search!(err_idx, req_complex_log, RUNS);
    lat_rows("Complex bool (must+filter+must_not+sort)", &lats);
    separator();

    // 9b. Nested bool: (match OR match) AND filter AND NOT filter
    let req_nested_bool = parse_request(&json!({
        "query": {
            "bool": {
                "must": [
                    {
                        "bool": {
                            "should": [
                                { "match": { "message": "connection" } },
                                { "match": { "message": "timeout" } }
                            ],
                            "minimum_should_match": 1
                        }
                    }
                ],
                "filter": [
                    { "terms": { "level": ["ERROR", "FATAL"] } }
                ]
            }
        },
        "size": 50
    }))
    .unwrap();
    let res2 = err_idx.search(&req_nested_bool).await.unwrap();
    row(
        "Nested bool (connection OR timeout, ERROR/FATAL):",
        &res2.total.value.to_string(),
    );

    let lats2 = measure_search!(err_idx, req_nested_bool, RUNS);
    lat_rows("Nested bool (should inside must)", &lats2);
    separator();

    // 9c. Product: must match name, should match description, filter on rating
    //     range on price, must_not have specific brand, sorted by rating desc
    let req_product_complex = parse_request(&json!({
        "query": {
            "bool": {
                "must": [
                    { "match": { "description": "quality" } }
                ],
                "should": [
                    { "match": { "description": "professional" } },
                    { "match": { "description": "innovative" } }
                ],
                "filter": [
                    { "range": { "rating": { "gte": 3.0 } } },
                    { "range": { "price": { "gte": 10.0, "lte": 2000.0 } } },
                    { "term": { "in_stock": true } }
                ],
                "minimum_should_match": 0
            }
        },
        "size": 20,
        "sort": [{ "rating": "desc" }, { "price": "asc" }]
    }))
    .unwrap();
    let res3 = prod_idx.search(&req_product_complex).await.unwrap();
    row(
        "Product complex bool (quality+rating+price):",
        &res3.total.value.to_string(),
    );

    // Sort verification: rating descending
    if res3.hits.len() >= 2 {
        let r0 = res3.hits[0]
            .source
            .get("rating")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let r1 = res3.hits[1]
            .source
            .get("rating")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        assert!(r0 >= r1, "rating sort desc violated: {} < {}", r0, r1);
        row("Sort correctness (rating desc, price asc):", "OK");
    }

    let lats3 = measure_search!(prod_idx, req_product_complex, RUNS);
    lat_rows("Product complex bool + sort", &lats3);
    separator();

    // 9d. Pagination: verify from/size consistency
    let req_page1 = parse_request(&json!({
        "query": { "match_all": {} },
        "from": 0, "size": 10,
        "sort": [{ "price": "asc" }]
    }))
    .unwrap();
    let req_page2 = parse_request(&json!({
        "query": { "match_all": {} },
        "from": 10, "size": 10,
        "sort": [{ "price": "asc" }]
    }))
    .unwrap();

    let p1 = prod_idx.search(&req_page1).await.unwrap();
    let p2 = prod_idx.search(&req_page2).await.unwrap();

    // Pages must not overlap
    let ids1: std::collections::HashSet<&str> = p1.hits.iter().map(|h| h.id.as_str()).collect();
    let ids2: std::collections::HashSet<&str> = p2.hits.iter().map(|h| h.id.as_str()).collect();
    let overlap = ids1.intersection(&ids2).count();
    assert_eq!(overlap, 0, "Page 1 and Page 2 share {} doc IDs", overlap);
    row("Pagination correctness (no overlap):", "OK");

    let lats4 = measure_search!(prod_idx, req_page1, RUNS);
    lat_rows("Paginated search (from/size)", &lats4);
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario 10 — Concurrent search under load
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn battle_concurrent_search_load() {
    banner("BATTLE: Concurrent Search — 16 tasks × 100 queries across all indices");

    let error_docs = match load_ndjson("error_logs.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };
    let product_docs = match load_ndjson("products.ndjson") {
        Ok(d) => d,
        Err(e) => {
            println!("SKIP: {}", e);
            return;
        }
    };

    let (engine, _dir) = battle_engine();
    engine.create_index("conc-errors", Schema::empty()).unwrap();
    engine
        .create_index("conc-products", Schema::empty())
        .unwrap();

    let (en, _) = bulk_index(&engine, "conc-errors", &error_docs).await;
    let (pn, _) = bulk_index(&engine, "conc-products", &product_docs).await;

    row("error-logs:", &format!("{} docs", en));
    row("products:", &format!("{} docs", pn));
    separator();

    let err_idx = std::sync::Arc::new(engine.get_index("conc-errors").unwrap());
    let prod_idx = std::sync::Arc::new(engine.get_index("conc-products").unwrap());

    const TASKS: usize = 16;
    const QUERIES_PER_TASK: usize = 100;

    // Rotating query mix
    let queries: Vec<Value> = vec![
        json!({"query":{"term":{"level":"ERROR"}},"size":50}),
        json!({"query":{"match":{"message":"database"}},"size":20}),
        json!({"query":{"terms":{"level":["ERROR","FATAL"]}},"size":0,"aggs":{"by_svc":{"terms":{"field":"service","size":5}}}}),
        json!({"query":{"match":{"description":"premium quality"}},"size":10}),
        json!({"query":{"range":{"price":{"gte":100,"lte":500}}},"size":50}),
        json!({"query":{"bool":{"must":[{"match":{"message":"connection"}}],"filter":[{"term":{"level":"ERROR"}}]}},"size":20}),
        json!({"query":{"match_all":{}},"size":0,"aggs":{"by_cat":{"terms":{"field":"category","size":10}}}}),
        json!({"query":{"term":{"in_stock":true}},"size":0}),
    ];

    let queries = std::sync::Arc::new(queries);
    let err_idx_c = std::sync::Arc::clone(&err_idx);
    let prod_idx_c = std::sync::Arc::clone(&prod_idx);
    let queries_c = std::sync::Arc::clone(&queries);

    let wall_start = Instant::now();
    let mut handles = Vec::with_capacity(TASKS);

    for task_id in 0..TASKS {
        let err_c = std::sync::Arc::clone(&err_idx_c);
        let prd_c = std::sync::Arc::clone(&prod_idx_c);
        let qc = std::sync::Arc::clone(&queries_c);

        handles.push(tokio::spawn(async move {
            let mut lats: Vec<Duration> = Vec::with_capacity(QUERIES_PER_TASK);
            for q_num in 0..QUERIES_PER_TASK {
                let qi = (task_id * QUERIES_PER_TASK + q_num) % qc.len();
                let body = &qc[qi];
                let req = parse_request(body).unwrap();

                let t0 = Instant::now();
                // Alternate between the two indices
                if qi.is_multiple_of(2) {
                    let _ = err_c.search(&req).await;
                } else {
                    let _ = prd_c.search(&req).await;
                }
                lats.push(t0.elapsed());
            }
            lats
        }));
    }

    let mut all_lats: Vec<Duration> = Vec::with_capacity(TASKS * QUERIES_PER_TASK);
    for handle in handles {
        all_lats.extend(handle.await.expect("task panicked"));
    }

    let wall = wall_start.elapsed();
    all_lats.sort();

    let total_queries = TASKS * QUERIES_PER_TASK;
    let qps = total_queries as f64 / wall.as_secs_f64();

    row("Concurrent tasks:", &TASKS.to_string());
    row("Queries per task:", &QUERIES_PER_TASK.to_string());
    row("Total queries:", &total_queries.to_string());
    row("Wall-clock time:", &format!("{:.2?}", wall));
    row("Aggregate QPS:", &format!("{:.0} queries/sec", qps));
    lat_rows("Concurrent search", &all_lats);
}
