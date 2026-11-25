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
//! ## What the cap counts (#3362)
//!
//! `max_buckets` limits the buckets a terms agg RETURNS, as ES's final reduce
//! does, not the distinct keys it counts. #3383 made any terms agg that met
//! more distinct keys than the cap a 400, so a top-10 over a field with more
//! than 65,536 distinct values (user IDs, IPs) failed where ES answers 200.
//! Both executors now count every key: a small `size` over an over-cap field
//! is served (columnarly when the index qualifies) with exact counts, and only
//! a result that keeps more buckets than the cap is the 400 too_many_buckets.
//! The fast path bails to brute for that case, so brute `run_terms` stays the
//! one owner of the error and the two executors agree by construction. Never a
//! truncated bucket list.
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
use xerj_common::types::{FieldConfig, FieldType, Schema};
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
        // `tag` is a DECLARED keyword field (see `build_index`) → a keyword
        // doc-values column stored under the unsuffixed name, which
        // `terms: {field: "tag"}` reads on the fast path. It is declared rather
        // than inferred because #1329 made a *dynamically* mapped string a
        // `text` parent with NO doc-values (only its `.keyword` sub-field keeps
        // them, matching ES), so an inferred `tag` would have no unsuffixed
        // column for the fast path to read and every leg would bail to brute —
        // which would silently defeat this test's whole point (that the cap
        // binds the *fast* path). `i % nterms` gives exactly `nterms` distinct
        // keys.
        let tag = i % nterms;
        out.push_str(&format!("{{\"tag\":\"t{tag}\"}}\n"));
    }
    out
}

async fn build_index(engine: &Engine, name: &str, docs: usize, nterms: usize) {
    // Declare `tag` as keyword so it owns a doc-values column under its own
    // unsuffixed name — the column the columnar `exec_terms` fast path reads.
    // A declared keyword keeps `doc_values = true` (unaffected by #1329, which
    // only flipped the default for INFERRED `text` parents), so the fast path
    // engages here exactly as issue #121 requires.
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("tag", FieldType::Keyword));
    engine.create_index(name, schema).unwrap();
    for start in (0..docs).step_by(1_000) {
        let end = (start + 1_000).min(docs);
        let body = ndjson_batch(name, nterms, start..end);
        let res = process_bulk(engine, Some(name), &body).await;
        assert!(!res.errors, "bulk errors while seeding {name}");
    }
    // Flush to segments: the columnar path reads `.dv` sidecars.
    engine.get_index(name).unwrap().refresh().await.unwrap();
}

/// Run one `size:0 + match_all` terms agg with the given agg `size` (`0` =
/// every key, so the bucket count equals the distinct terms) and report both
/// the agg body and whether the columnar fast path served it.
async fn run_terms_sized(engine: &Engine, index: &str, size: usize) -> (Value, u64) {
    let idx = engine.get_index(index).unwrap();
    let req = parse_request(&json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": { "by_tag": { "terms": { "field": "tag", "size": size } } },
    }))
    .unwrap();
    let before = fast_path_aggs_served();
    let res = idx.search(&req).await.unwrap();
    let served = fast_path_aggs_served() - before;
    let aggs = res.aggs.clone().expect("aggs present");
    (aggs["by_tag"].clone(), served)
}

async fn run_terms(engine: &Engine, index: &str) -> (Value, u64) {
    run_terms_sized(engine, index, 0).await
}

/// A top-10 over NTERMS evenly spread keys: ten buckets, each holding
/// `docs / NTERMS` (or one more) docs, and every other doc in
/// `sum_other_doc_count`.
fn assert_exact_top10(agg: &Value, docs: usize, label: &str) {
    let buckets = agg["buckets"]
        .as_array()
        .unwrap_or_else(|| panic!("{label}: expected buckets, got {agg}"));
    assert_eq!(buckets.len(), 10, "{label}: {agg}");
    let kept: u64 = buckets
        .iter()
        .map(|b| b["doc_count"].as_u64().expect("doc_count"))
        .sum();
    for b in buckets {
        let c = b["doc_count"].as_u64().unwrap() as usize;
        assert!(
            c == docs / NTERMS || c == docs / NTERMS + 1,
            "{label}: bucket count {c} is not a true per-key count: {agg}"
        );
    }
    assert_eq!(
        agg["sum_other_doc_count"].as_u64(),
        Some(docs as u64 - kept),
        "{label}: sum_other_doc_count must account for every other doc: {agg}"
    );
    assert_eq!(agg["doc_count_error_upper_bound"], json!(0), "{label}");
}

/// #3362: a terms agg that keeps more buckets than the cap is the embedded
/// too_many_buckets failure
/// (the `_search` handler turns it into ES's HTTP 400), never a bucket list.
fn assert_too_many_buckets(agg: &Value, label: &str) {
    assert!(
        agg.get("buckets").is_none(),
        "{label}: an over-cap terms agg returned buckets instead of failing: {agg}"
    );
    assert_eq!(agg["__error_status__"], json!(400), "{label}: {agg}");
    let reason = agg["error"].as_str().unwrap_or_default();
    assert!(
        reason.contains("Trying to create too many buckets")
            && reason.contains(&format!("[{CAP}]")),
        "{label}: expected too_many_buckets naming max_buckets={CAP}, got {agg}"
    );
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

    // ── Fast path, cardinality PAST the cap, small `size` ───────────────
    // #3362 regression from #3383: a top-10 over 200 distinct keys at a cap
    // of 37 returns 10 buckets, so it is served (columnarly) with exact
    // counts. #3383 answered 400 here.
    let (agg, served) = run_terms_sized(&engine, "big-over", 10).await;
    assert_eq!(
        served, 1,
        "a top-10 over an over-cap field must stay columnar: {agg}"
    );
    assert_exact_top10(&agg, BIG_DOCS, "fast-path top-10 past the cap");

    // ── Brute path, same shape below the size gate ───────────────────────
    let (agg, served) = run_terms_sized(&engine, "small-over", 10).await;
    assert_eq!(served, 0, "the small index must not take the fast path");
    assert_exact_top10(&agg, SMALL_DOCS, "brute top-10 past the cap");

    // ── Fast path, every key (size 0) PAST the cap ───────────────────────
    // 200 returned buckets > 37: the fast path bails to brute (served == 0)
    // and the answer is too_many_buckets, never the first CAP keys.
    let (agg, served) = run_terms(&engine, "big-over").await;
    assert_eq!(
        served, 0,
        "exec_terms served an over-cap result columnarly instead of bailing: {agg}"
    );
    assert_too_many_buckets(&agg, "fast-path index, every key past the cap");

    // A `size` above the cap is refused the same way.
    let (agg, served) = run_terms_sized(&engine, "big-over", CAP + 1).await;
    assert_eq!(served, 0, "size > cap must bail to brute: {agg}");
    assert_too_many_buckets(&agg, "fast-path index, size > cap");

    // ── Brute path, every key past the cap ───────────────────────────────
    // The path the fast leg bails INTO. It agrees: the same too_many_buckets
    // failure, not the first CAP keys in doc order.
    let (agg, served) = run_terms(&engine, "small-over").await;
    assert_eq!(served, 0, "the small index must not take the fast path");
    assert_too_many_buckets(&agg, "brute index past the cap");

    // ── Fast path, cardinality exactly AT the cap ────────────────────────
    // Bail only when the returned buckets EXCEED the cap: an at-cap result
    // is still served columnarly and returns all CAP buckets.
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
    assert!(
        agg["buckets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b.get("__xy_terms_key__").is_some()),
        "the columnar path must keep the raw term identity the multi-index \
         coordinator merges on"
    );
}
