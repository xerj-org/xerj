//! `config.limits.max_buckets` must bind the columnar `terms` fast path
//! (`fast_aggs::exec_terms`) exactly as it binds the brute `run_terms` — the
//! half of issue #121 left open by PR #125 (which fixed the histogram paths).
//!
//! `exec_terms` built its term map with NO bucket cap of any kind — only a
//! `size`-derived OUTPUT cap. Measured at `max_buckets = 37` over 200 distinct
//! terms, the fast path (a 12 000-doc index) materialised every one of the 200
//! terms while the brute path (a 600-doc index of identical shape) stopped at
//! 37. That is the OOM vector the cap exists to close, honoured on brute and
//! ignored on the columnar path.
//!
//! ## Why the fix BAILS rather than errors or truncates
//!
//! The two executors must AGREE past the cap. Brute `run_terms` does not error
//! there: it keeps the first `max_buckets` distinct terms in doc-iteration
//! order, drops the rest, and reports a `sum_other_doc_count` computed only
//! over the terms it kept — so the dropped terms vanish silently and the total
//! conceals them (order-dependent, and a bug in its own right). Reproducing
//! that on the fast path would copy the bug, not the contract; erroring would
//! make the fast path disagree with a brute path that still returns a body.
//!
//! So `exec_terms` bails to brute the instant its distinct-term count would
//! exceed the cap. After the bail the request is answered by the very
//! `run_terms` the fast path would be measured against, so the two AGREE past
//! the cap by construction. What this test pins is that agreement plus the two
//! observable consequences of the bail: the fast path stops SERVING the
//! over-cap agg (`fast_path_aggs_served` no longer ticks) and the answer honours
//! the cap (exactly `CAP` distinct buckets, not all `NTERMS`).
//!
//! Which executor answered is read from `aggs::fast_path_aggs_served`; the brute
//! leg is forced by keeping an index under the columnar path's own size gate
//! (`fast_aggs::FAST_AGG_MIN_DOCS`, 10 000 docs).
//!
//! Its own test binary, one test function: `Engine::new` installs the cap in a
//! process-wide static (`aggs::set_max_buckets`) and the fast-path counter is a
//! process-wide atomic, so every engine here must agree on one value and no
//! parallel test may race them — the same reason `agg_bucket_cap.rs` is one
//! function in its own binary.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::aggs::fast_path_aggs_served;
use xerj_engine::bulk::process_bulk;
use xerj_engine::Engine;
use xerj_query::parse_request;

/// The measured cap from the issue: far below the stock 65 536.
const CAP: usize = 37;

/// Distinct `tag` values in an over-cap corpus — the issue's 200.
const NTERMS: usize = 200;

/// Docs in an index that must take the columnar path — `fast_aggs` refuses to
/// serve an index below its own `FAST_AGG_MIN_DOCS` (10 000).
const BIG_DOCS: usize = 10_050;

/// Docs in an index that must NOT take the columnar path: same shape, on the
/// other side of that gate. 600 ≥ NTERMS, so every distinct term is present.
const SMALL_DOCS: usize = 600;

fn ndjson_batch(index: &str, nterms: usize, ids: std::ops::Range<usize>) -> String {
    let mut out = String::with_capacity(ids.len() * 72);
    for i in ids {
        out.push_str(&format!(
            "{{\"index\":{{\"_index\":\"{index}\",\"_id\":\"{i}\"}}}}\n"
        ));
        // `tag` is a plain string field → a keyword doc-values column stored
        // under the unsuffixed name, which `terms: {field: "tag"}` reads on the
        // fast path. `i % nterms` gives exactly `nterms` distinct keys.
        let tag = i % nterms;
        out.push_str(&format!("{{\"tag\":\"t{tag}\"}}\n"));
    }
    out
}

async fn build_index(engine: &Engine, name: &str, docs: usize, nterms: usize) {
    engine.create_index(name, Schema::empty()).unwrap();
    for start in (0..docs).step_by(1_000) {
        let end = (start + 1_000).min(docs);
        let body = ndjson_batch(name, nterms, start..end);
        let res = process_bulk(engine, Some(name), &body).await;
        assert!(!res.errors, "bulk errors while seeding {name}");
    }
    // Flush to segments: the columnar path reads `.dv` sidecars.
    engine.get_index(name).unwrap().refresh().await.unwrap();
}

/// Run one `size:0 + match_all` terms agg (with `size:0` on the agg too, so the
/// OUTPUT is uncapped and the bucket count equals the distinct terms KEPT) and
/// report both the agg body and whether the columnar fast path served it.
async fn run_terms(engine: &Engine, index: &str) -> (Value, u64) {
    let idx = engine.get_index(index).unwrap();
    let req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": { "by_tag": { "terms": { "field": "tag", "size": 0 } } },
    }))
    .unwrap();
    let before = fast_path_aggs_served();
    let res = idx.search(&req).await.unwrap();
    let served = fast_path_aggs_served() - before;
    let aggs = res.aggs.clone().expect("aggs present");
    (aggs["by_tag"].clone(), served)
}

fn bucket_count(agg: &Value) -> usize {
    agg["buckets"]
        .as_array()
        .unwrap_or_else(|| panic!("expected buckets, got {agg}"))
        .len()
}

#[tokio::test]
async fn lowered_max_buckets_binds_the_columnar_terms_path_and_the_brute_path_alike() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.limits.max_buckets = CAP;
    let engine = Engine::new(config).expect("engine::new");

    build_index(&engine, "big-over", BIG_DOCS, NTERMS).await;
    build_index(&engine, "big-at", BIG_DOCS, CAP).await;
    build_index(&engine, "small-over", SMALL_DOCS, NTERMS).await;

    // ── Fast path, cardinality PAST the cap ──────────────────────────────
    // The regression: before the fix this was served columnarly (served == 1)
    // and returned all NTERMS buckets. The fix bails to brute, so the fast path
    // no longer serves it and the brute cap (exactly CAP distinct terms) binds.
    let (agg, served) = run_terms(&engine, "big-over").await;
    assert_eq!(
        served,
        0,
        "exec_terms served an over-cap agg columnarly instead of bailing; it \
         returned {} buckets against max_buckets={CAP}",
        bucket_count(&agg)
    );
    assert_eq!(
        bucket_count(&agg),
        CAP,
        "over-cap terms agg did not honour max_buckets={CAP}: {}",
        bucket_count(&agg)
    );

    // ── Brute path, same over-cap shape below the size gate ──────────────
    // The path the fast leg bails INTO. It agrees on the cap: exactly CAP
    // distinct buckets. (The kept terms are order-dependent and the doc counts
    // differ from the big index, so agreement is on the count, per the issue.)
    let (agg, served) = run_terms(&engine, "small-over").await;
    assert_eq!(served, 0, "the small index must not take the fast path");
    assert_eq!(
        bucket_count(&agg),
        CAP,
        "brute run_terms disagrees with the fast path on the cap"
    );

    // ── Fast path, cardinality exactly AT the cap ────────────────────────
    // The fix must bail only when the count would EXCEED the cap: an at-cap
    // agg is still served columnarly and returns all CAP buckets. (This guards
    // against over-bailing; it is not a regression detector — the pre-fix code
    // also served this case.)
    let (agg, served) = run_terms(&engine, "big-at").await;
    assert_eq!(
        served, 1,
        "exec_terms bailed on an at-cap agg it should still serve"
    );
    assert_eq!(
        bucket_count(&agg),
        CAP,
        "at-cap terms agg dropped or invented a bucket"
    );
}
