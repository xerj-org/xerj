//! `config.limits.max_buckets` must bind the columnar fast path exactly as it
//! binds the brute path (issue #121).
//!
//! `fast_aggs.rs` used to carry its own `const MAX_BUCKETS: i64 = 65_536` in
//! `exec_date_histogram` and `exec_histogram`, so an operator who lowered the
//! cap to protect memory got that protection only on queries that happened to
//! miss the columnar path — and the discrepancy was invisible, because the two
//! executors otherwise return byte-identical results.
//!
//! Which executor answered is read from `aggs::fast_path_aggs_served`; the
//! brute half of each pair is forced by keeping that index under the columnar
//! path's own size gate (`fast_aggs::FAST_AGG_MIN_DOCS`, 10 000 docs) — same
//! agg, same bucket span, different executor.
//!
//! This suite gets its own test binary on purpose: `Engine::new` installs the
//! cap in a process-wide static (`aggs::set_max_buckets`), so every engine in
//! the file must agree on one value and no other suite may observe it. For the
//! same reason it is deliberately ONE test function — parallel tests in one
//! binary would race on both the cap and the fast-path counter.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::aggs::fast_path_aggs_served;
use xerj_engine::bulk::process_bulk;
use xerj_engine::Engine;
use xerj_query::parse_request;

/// Lowered far below the stock 65 536 so a corpus that trips it is still
/// small enough to index inside a test.
const CAP: usize = 64;

/// Docs in an index that must take the columnar path — `fast_aggs` refuses to
/// serve an index below its own `FAST_AGG_MIN_DOCS` (10 000).
const BIG_DOCS: usize = 10_050;

/// Docs in an index that must NOT take the columnar path: same shape, same
/// bucket spans, on the other side of that gate.
const SMALL_DOCS: usize = 900;

const HOUR_MS: i64 = 3_600_000;
/// Hour-aligned epoch-ms, so one distinct `hour` offset == one `1h` bucket.
const BASE_MS: i64 = 1_699_999_200_000;

/// A corpus shape: how many distinct `ts` hours and how many distinct `n`
/// values its docs cycle through — i.e. how many buckets a gap-filled
/// histogram over the whole corpus has to materialise.
#[derive(Clone, Copy)]
struct Shape {
    hours: usize,
    nvals: usize,
}

/// Exactly at the cap. `date_histogram` counts buckets, `histogram` compares
/// `max_key - min_key` (a difference, not a count — pre-existing, and the
/// same on both paths), so "at the cap" is `CAP` hours and `CAP + 1` values.
const AT_CAP: Shape = Shape {
    hours: CAP,
    nvals: CAP + 1,
};
/// One bucket past the cap on both aggs.
const OVER_CAP: Shape = Shape {
    hours: CAP + 1,
    nvals: CAP + 2,
};

fn ndjson_batch(index: &str, shape: Shape, ids: std::ops::Range<usize>) -> String {
    let mut out = String::with_capacity(ids.len() * 96);
    for i in ids {
        out.push_str(&format!(
            "{{\"index\":{{\"_index\":\"{index}\",\"_id\":\"{i}\"}}}}\n"
        ));
        let ts = BASE_MS + (i % shape.hours) as i64 * HOUR_MS;
        let n = (i % shape.nvals) as i64;
        out.push_str(&format!("{{\"ts\":{ts},\"n\":{n}}}\n"));
    }
    out
}

async fn build_index(engine: &Engine, name: &str, docs: usize, shape: Shape) {
    engine.create_index(name, Schema::empty()).unwrap();
    for start in (0..docs).step_by(1_000) {
        let end = (start + 1_000).min(docs);
        let body = ndjson_batch(name, shape, start..end);
        let res = process_bulk(engine, Some(name), &body).await;
        assert!(!res.errors, "bulk errors while seeding {name}");
    }
    // Flush to segments: the columnar path reads `.dv` sidecars, and
    // `exec_histogram` only runs when the field has a real numeric column.
    engine.get_index(name).unwrap().refresh().await.unwrap();
}

/// Run one `size:0 + match_all` agg request and report both the agg body and
/// whether the columnar fast path served it.
async fn run_agg(engine: &Engine, index: &str, agg: Value) -> (Value, u64) {
    let idx = engine.get_index(index).unwrap();
    let req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": { "a": agg },
    }))
    .unwrap();
    let before = fast_path_aggs_served();
    let res = idx.search(&req).await.unwrap();
    let served = fast_path_aggs_served() - before;
    let aggs = res.aggs.clone().expect("aggs present");
    (aggs["a"].clone(), served)
}

/// The too_many_buckets marker the agg runners embed, if present. It carries
/// the cap that was actually enforced — `[64]` here, `[65536]` before the fix.
fn too_many_buckets(agg: &Value) -> Option<String> {
    let msg = agg.get("error")?.as_str()?;
    assert_eq!(agg.get("__error_status__"), Some(&json!(400)));
    assert!(
        msg.contains("Trying to create too many buckets"),
        "unexpected agg error: {msg}"
    );
    Some(msg.to_string())
}

fn bucket_count(agg: &Value) -> usize {
    agg["buckets"]
        .as_array()
        .unwrap_or_else(|| panic!("expected buckets, got {agg}"))
        .len()
}

#[tokio::test]
async fn lowered_max_buckets_binds_the_columnar_fast_path_and_the_brute_path_alike() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.limits.max_buckets = CAP;
    let engine = Engine::new(config).expect("engine::new");

    build_index(&engine, "big-over", BIG_DOCS, OVER_CAP).await;
    build_index(&engine, "big-at", BIG_DOCS, AT_CAP).await;
    build_index(&engine, "small-over", SMALL_DOCS, OVER_CAP).await;
    build_index(&engine, "small-at", SMALL_DOCS, AT_CAP).await;

    let date_hist = json!({"date_histogram": {"field": "ts", "fixed_interval": "1h"}});
    let num_hist = json!({"histogram": {"field": "n", "interval": 1}});

    // ── Fast path: the lowered cap must trip it ──────────────────────────
    let (agg, served) = run_agg(&engine, "big-over", date_hist.clone()).await;
    assert_eq!(served, 1, "date_histogram did not take the columnar path");
    let fast_date_err = too_many_buckets(&agg).unwrap_or_else(|| {
        panic!(
            "fast path ignored max_buckets={CAP} and returned {} buckets: {agg}",
            bucket_count(&agg)
        )
    });
    assert!(
        fast_date_err.contains(&format!("[{CAP}]")),
        "fast path reported a cap it did not enforce: {fast_date_err}"
    );

    let (agg, served) = run_agg(&engine, "big-over", num_hist.clone()).await;
    assert_eq!(served, 1, "histogram did not take the columnar path");
    let fast_num_err = too_many_buckets(&agg).unwrap_or_else(|| {
        panic!(
            "fast path ignored max_buckets={CAP} and returned {} buckets: {agg}",
            bucket_count(&agg)
        )
    });
    assert!(
        fast_num_err.contains(&format!("[{CAP}]")),
        "fast path reported a cap it did not enforce: {fast_num_err}"
    );

    // ── Fast path: a span exactly at the cap must still be answered ──────
    let (agg, served) = run_agg(&engine, "big-at", date_hist.clone()).await;
    assert_eq!(served, 1, "date_histogram did not take the columnar path");
    assert_eq!(bucket_count(&agg), CAP, "cap-sized span must be served");

    let (agg, served) = run_agg(&engine, "big-at", num_hist.clone()).await;
    assert_eq!(served, 1, "histogram did not take the columnar path");
    assert_eq!(bucket_count(&agg), CAP + 1, "cap-sized span must be served");

    // ── Brute path: same shapes, below the columnar path's size gate ─────
    // Same trip point, same message, same bucket counts.
    let (agg, served) = run_agg(&engine, "small-over", date_hist.clone()).await;
    assert_eq!(served, 0, "the small index must not take the fast path");
    assert_eq!(
        too_many_buckets(&agg),
        Some(fast_date_err),
        "brute and fast date_histogram disagree about the cap"
    );

    let (agg, served) = run_agg(&engine, "small-over", num_hist.clone()).await;
    assert_eq!(served, 0, "the small index must not take the fast path");
    assert_eq!(
        too_many_buckets(&agg),
        Some(fast_num_err),
        "brute and fast histogram disagree about the cap"
    );

    let (agg, served) = run_agg(&engine, "small-at", date_hist).await;
    assert_eq!(served, 0, "the small index must not take the fast path");
    assert_eq!(bucket_count(&agg), CAP, "brute dropped a cap-sized span");

    let (agg, served) = run_agg(&engine, "small-at", num_hist).await;
    assert_eq!(served, 0, "the small index must not take the fast path");
    assert_eq!(
        bucket_count(&agg),
        CAP + 1,
        "brute dropped a cap-sized span"
    );
}
