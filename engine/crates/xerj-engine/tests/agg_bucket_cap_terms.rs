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
//! ## Why the fast path BAILS rather than errors in place
//!
//! The two executors must AGREE past the cap. `exec_terms` bails to brute the
//! instant its distinct-term count would exceed the cap, and brute `run_terms`
//! answers an over-cap agg with a 400 too_many_buckets (#3362; before that it
//! kept the first `max_buckets` keys in doc order and dropped the rest
//! silently, with a `sum_other_doc_count` that concealed them). One owner of
//! the over-cap answer means the two AGREE by construction. What this test
//! pins is that agreement plus the observable consequences of the bail: the
//! fast path stops SERVING the over-cap agg (`fast_path_aggs_served` no longer
//! ticks) and the answer is the too_many_buckets failure naming the cap, never
//! a truncated bucket list.
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

/// #3362: past the cap a terms agg is the embedded too_many_buckets failure
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

    // ── Fast path, cardinality PAST the cap ──────────────────────────────
    // #121: before the bail this was served columnarly (served == 1) and
    // returned all NTERMS buckets. The fast path bails to brute, so it no
    // longer serves it, and brute answers with too_many_buckets (#3362).
    let (agg, served) = run_terms(&engine, "big-over").await;
    assert_eq!(
        served, 0,
        "exec_terms served an over-cap agg columnarly instead of bailing: {agg}"
    );
    assert_too_many_buckets(&agg, "fast-path index past the cap");

    // ── Brute path, same over-cap shape below the size gate ──────────────
    // The path the fast leg bails INTO. It agrees: the same too_many_buckets
    // failure, not the first CAP keys in doc order.
    let (agg, served) = run_terms(&engine, "small-over").await;
    assert_eq!(served, 0, "the small index must not take the fast path");
    assert_too_many_buckets(&agg, "brute index past the cap");

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
