//! #1267 — the batched finalize-verify windows against a data index whose
//! every record shares one of the window's digests (the cve-records resume
//! shape: ~52 records per source file, so a 1,024-digest `terms` window
//! covers an entire shard).  Two lanes, two questions:
//!
//! 1. COUNT lane — `bool.filter [terms ax_file ×1024]` (+ the semantic
//!    leg's `exists`) answering a `terms` agg at `size: window.len()`
//!    (1,024 buckets).  On the live crawl node one such window against
//!    the 53,153-doc `cves-28` shard ran 803,735 ms in `segment_loop`
//!    with `shortcut=None` — the brute agg corpus materialising the whole
//!    index per window.  The #1260 test (`fast_aggs_exists_and_deletes`)
//!    pins the same filter shape at TWO values; this file pins it at the
//!    client's real `VERIFY_WINDOW` (1,024) and at full-shard coverage,
//!    both on clean segments AND — the #1267 residual — on a shard whose
//!    flushed segment carries RESIDENT GHOSTS (a resume's overwrites and
//!    deletes), which is what had every window bailed to the brute corpus.
//! 2. DOC lane — the catalog read-back shape (`size: 8192`,
//!    `_source: ["file_key"]`, `terms file_key ×1024` + `term run_id`),
//!    measured for wall time only: this is the #1267 "verify windows
//!    with size > 0" residual, pinned clean and under tombstones.
//!
//! Both lanes assert fast-path/brute-path agreement, not just timing.

use serde_json::json;
use std::time::Instant;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::aggs::fast_path_aggs_served;
use xerj_engine::{Engine, Index};
use xerj_query::parse_request;

/// The client's window size (`SyncExecutor::VERIFY_WINDOW`).
const WINDOW: usize = 1024;
/// Groups (distinct digests); at `DOCS_PER_GROUP` records each the index
/// clears `FAST_AGG_MIN_DOCS` (10,000) and a full window covers the whole
/// index — the cve-records resume shape.
const GROUPS: usize = 1024;
const DOCS_PER_GROUP: usize = 60;

static ONE_AT_A_TIME: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

/// Declare the exact-match fields this verify-shape reads as `keyword`, the
/// way the real crawl's data index and the autoindex catalog declare theirs
/// (`ax_file` is a content digest, `ax_dataset` a dataset name, `cve_id` an
/// identifier — all keyword, never analysed text). These tests used
/// `Schema::empty()`, so since #1329 those fields became `text` parents with
/// NO doc-values column; the columnar count-lane (terms on `ax_file`) and the
/// `KwExists` arm both read that column, so the window fell to the brute path
/// and the "served columnar" assertions failed. `text` is declared keyword
/// too so the `exists` arm has a column to read — a declared keyword carries
/// no `ignore_above` by default, so the ~1 KB record body is still stored
/// whole (presence is all `exists` reads).
fn kw_schema(fields: &[&str]) -> Schema {
    let mut s = Schema::empty();
    for f in fields {
        s.fields.push(FieldConfig::new(*f, FieldType::Keyword));
    }
    s
}

async fn run(idx: &std::sync::Arc<Index>, body: serde_json::Value) -> serde_json::Value {
    let req = parse_request(&body).unwrap();
    let res = idx.search(&req).await.unwrap();
    json!({
        "total": res.total.value,
        "hits": res.hits.len(),
        "aggs": res.aggs.clone().unwrap_or(json!(null)),
    })
}

/// The record body of group `g`, record `r` — shared by the seed and the
/// byte-identical rewrite (whose ghost must be counted out while its
/// memtable replacement is counted in, live corpus unchanged).
fn doc_body(g: usize, r: usize) -> serde_json::Value {
    json!({
        "ax_file": format!("axf2-{:032x}-{:04x}", g * 7, g),
        "ax_dataset": "cvelistv5-cves-28",
        "cve_id": format!("CVE-2026-{g:04}"),
        "text": format!(
            "A heap-based buffer overflow was found in record {r} of group {g}. {}",
            "The parser reads a length prefix without validating it against the allocation. ".repeat(14),
        ),
    })
}

/// `GROUPS × DOCS_PER_GROUP` flushed docs shaped like the crawl's data
/// records: every doc of a group carries the group's `ax_file` content
/// digest, ~1 KB of prose (a real CVE record's description block), and the
/// dataset keyword the exact-dataset read-back filters on.
async fn seed(idx: &std::sync::Arc<Index>) -> Vec<String> {
    let mut digests = Vec::with_capacity(GROUPS);
    for g in 0..GROUPS {
        digests.push(format!("axf2-{:032x}-{:04x}", g * 7, g));
        for r in 0..DOCS_PER_GROUP {
            idx.index_document(
                Some(format!("doc-{}", g * DOCS_PER_GROUP + r)),
                doc_body(g, r),
            )
            .await
            .unwrap();
        }
    }
    idx.flush().await.unwrap();
    digests
}

fn count_lane(digests: &[String], exists: bool) -> serde_json::Value {
    let mut filter = vec![json!({ "terms": { "ax_file": digests } })];
    if exists {
        filter.push(json!({ "exists": { "field": "text" } }));
    }
    json!({
        "size": 0,
        "query": { "bool": { "filter": filter } },
        "aggs": { "values": { "terms": { "field": "ax_file", "size": digests.len() } } }
    })
}

/// `key → doc_count` for a terms bucket array (order varies with counts).
fn bucket_counts(buckets: &serde_json::Value) -> std::collections::HashMap<String, u64> {
    buckets
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            (
                b["key"].as_str().unwrap().to_string(),
                b["doc_count"].as_u64().unwrap(),
            )
        })
        .collect()
}

/// The count-lane window must be served by the columnar fast path even at
/// the client's full window size with whole-index coverage, and its buckets
/// must agree with the brute path (same request under
/// `XERJ_DISABLE_FAST_AGGS`-equivalent… which cannot be flipped per-test in
/// one process — so agreement is pinned against the per-digest expectation:
/// every bucket holds exactly `DOCS_PER_GROUP`).
#[tokio::test]
async fn count_lane_full_window_is_served_columnar() {
    let _sequential = ONE_AT_A_TIME.lock().await;
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine
        .create_index(
            "cves28",
            kw_schema(&["ax_file", "ax_dataset", "cve_id", "text"]),
        )
        .unwrap();
    let idx = engine.get_index("cves28").unwrap();
    let digests = seed(&idx).await;

    for exists in [false, true] {
        let before = fast_path_aggs_served();
        let t0 = Instant::now();
        let res = run(&idx, count_lane(&digests, exists)).await;
        let elapsed = t0.elapsed();
        let served = fast_path_aggs_served() - before;

        assert_eq!(
            res["total"].as_u64().unwrap(),
            (GROUPS * DOCS_PER_GROUP) as u64,
            "exists={exists} total"
        );
        let buckets = res["aggs"]["values"]["buckets"].as_array().unwrap();
        assert_eq!(buckets.len(), WINDOW, "exists={exists} bucket count");
        for b in buckets {
            assert_eq!(b["doc_count"].as_u64().unwrap(), DOCS_PER_GROUP as u64);
        }
        // THE regression signal: the live crawl node answered this shape from
        // the brute agg corpus (803,735 ms segment_loop).  If the fast path
        // refuses it again, fail loudly rather than ship the slow answer.
        assert!(
            served >= 1,
            "exists={exists}: columnar fast path did not serve the full-window verify shape ({} ms)",
            elapsed.as_millis()
        );
    }
}

/// THE #1267 repro: a resumed crawl shard carries tombstones — the resume
/// overwrites/deletes rows whose superseded copies are still resident in a
/// flushed segment — and until this fix ANY dirty ghost bitmap bailed the
/// whole fast-agg request, so every verify window paid the brute agg corpus
/// (803,735 ms measured on the live 53,153-doc `cves-28`).  The admission
/// gate now admits the verify shape (filter + single plain terms agg) with
/// dirty bitmaps: the row pass skips ghosted rows, the filtered total
/// row-scans dirty segments, and the memtable replacement copies are
/// counted live.  Exactness is pinned by arithmetic (known deletes, known
/// byte-identical rewrites) AND by agreement with the brute reference —
/// the same terms agg plus a sibling `value_count`, which makes the aggs
/// tree non-row-exact and so falls to the brute executor.
#[tokio::test]
async fn count_lane_with_resident_ghosts_is_served_and_exact() {
    let _sequential = ONE_AT_A_TIME.lock().await;
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine
        .create_index(
            "cves28g",
            kw_schema(&["ax_file", "ax_dataset", "cve_id", "text"]),
        )
        .unwrap();
    let idx = engine.get_index("cves28g").unwrap();
    let digests = seed(&idx).await;

    // Group 0 loses its first DELETED rows (ghosts that MATCH the window
    // filter — their `ax_file` is `digests[0]`, in-window).
    const DELETED: usize = 10;
    // Group 1's first REWRITTEN rows are overwritten byte-identically:
    // ghosts in the segment, fresh live copies in the memtable, live
    // corpus and every bucket count unchanged.
    const REWRITTEN: usize = 25;
    for i in 0..DELETED {
        idx.delete_document(&format!("doc-{i}")).await.unwrap();
    }
    for r in 0..REWRITTEN {
        idx.index_document(Some(format!("doc-{}", DOCS_PER_GROUP + r)), doc_body(1, r))
            .await
            .unwrap();
    }

    let before = fast_path_aggs_served();
    let t0 = Instant::now();
    let fast = run(&idx, count_lane(&digests, false)).await;
    let elapsed = t0.elapsed();
    assert_eq!(
        fast_path_aggs_served() - before,
        1,
        "a dirty-but-admitted segment must serve the verify shape columnarly ({} ms)",
        elapsed.as_millis()
    );

    assert_eq!(
        fast["total"].as_u64().unwrap(),
        (GROUPS * DOCS_PER_GROUP - DELETED) as u64,
        "ghosted rows must leave the filtered total"
    );
    let counts = bucket_counts(&fast["aggs"]["values"]["buckets"]);
    assert_eq!(counts.len(), WINDOW, "every digest still has live docs");
    assert_eq!(
        counts[&digests[0]],
        (DOCS_PER_GROUP - DELETED) as u64,
        "deleted rows must leave their bucket"
    );
    assert_eq!(
        counts[&digests[1]], DOCS_PER_GROUP as u64,
        "byte-identical rewrites: ghost out, memtable copy in — net zero"
    );
    for (d, c) in &counts {
        if *d != digests[0] {
            assert_eq!(*c, DOCS_PER_GROUP as u64, "untouched group {d}");
        }
    }

    // Brute reference: the sibling agg makes the tree non-row-exact, so
    // the dirty segments bail admission and the brute executor answers.
    // Its terms buckets and total are the reference the fast path must
    // reproduce.
    let mut reference = count_lane(&digests, false);
    reference["aggs"]["n"] = json!({ "value_count": { "field": "cve_id" } });
    let ref_before = fast_path_aggs_served();
    let brute = run(&idx, reference).await;
    assert_eq!(
        fast_path_aggs_served() - ref_before,
        0,
        "the sibling agg must force the brute executor"
    );
    assert_eq!(fast["total"], brute["total"], "total disagrees with brute");
    assert_eq!(
        fast["aggs"]["values"]["buckets"], brute["aggs"]["values"]["buckets"],
        "columnar buckets disagree with the brute reference under ghosts"
    );

    // Wall-time guard: the row pass over one dirty segment + 1,024-bucket
    // build is milliseconds-scale work; the brute corpus this replaced
    // materialised every matched doc.
    assert!(
        elapsed.as_millis() < 5_000,
        "dirty-segment verify window took {} ms",
        elapsed.as_millis()
    );
}

/// The catalog read-back shape: a fetching search, no aggs.  Wall-time
/// regression guard for the #1267 doc lane — a 1,024-digest window over a
/// fully-matching index must answer in seconds, not minutes, and return
/// exactly the window's documents — clean AND with tombstones resident
/// (the resume state of the live catalog).
#[tokio::test]
async fn doc_lane_window_fetch_is_bounded() {
    let _sequential = ONE_AT_A_TIME.lock().await;
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine
        .create_index("catalog", kw_schema(&["file_key", "run_id", "ax_path"]))
        .unwrap();
    let idx = engine.get_index("catalog").unwrap();
    // The catalog holds ONE doc per (digest, alias) — seed the same digests
    // plus a run_id that matches everything (the resume-rewrote-everything
    // case) and a second run's docs that the run_id term must exclude.
    let mut digests = Vec::with_capacity(GROUPS);
    let mut run_this_ids = Vec::with_capacity(GROUPS);
    let mut run_other_ids = Vec::new();
    let mut docs = Vec::new();
    for g in 0..GROUPS {
        let digest = format!("axf2-{:032x}-{:04x}", g * 7, g);
        digests.push(digest.clone());
        docs.push(json!({
            "file_key": digest,
            "run_id": "run-this",
            "ax_path": format!("CVE-2026-{g:04}.json"),
        }));
        if g % 3 == 0 {
            docs.push(json!({
                "file_key": digest,
                "run_id": "run-other",
                "ax_path": format!("CVE-2026-{g:04}.json"),
            }));
        }
    }
    for (i, doc) in docs.into_iter().enumerate() {
        if doc["run_id"] == json!("run-this") {
            run_this_ids.push(format!("cat-{i}"));
        } else {
            run_other_ids.push(format!("cat-{i}"));
        }
        idx.index_document(Some(format!("cat-{i}")), doc)
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();

    let body = json!({
        "size": 8192,
        "from": 0,
        "track_total_hits": true,
        "_source": ["file_key"],
        "query": { "bool": { "filter": [
            { "terms": { "file_key": digests } },
            { "term": { "run_id": "run-this" } }
        ]}}
    });
    let t0 = Instant::now();
    let res = run(&idx, body.clone()).await;
    let elapsed = t0.elapsed();

    assert_eq!(res["total"].as_u64().unwrap(), GROUPS as u64);
    assert_eq!(res["hits"].as_u64().unwrap(), GROUPS as u64);
    // The live catalog leg measured 31 ms cold / 14 ms warm per window on
    // 402,814 docs; a full second here means the fetch path regressed to
    // parsing the whole index per window.
    assert!(
        elapsed.as_millis() < 1_000,
        "catalog window fetch took {} ms",
        elapsed.as_millis()
    );

    // Dirty phase (#1267): the resume state — deletes on run-this rows
    // (ghosts that MATCH the window) and byte-identical rewrites of
    // run-other rows (ghosts the run_id term excludes anyway).  The fetch
    // must count exactly the live run-this docs.
    let mut deleted = 0usize;
    for (g, id) in run_this_ids.iter().enumerate() {
        if g % 10 == 0 {
            idx.delete_document(id).await.unwrap();
            deleted += 1;
        }
    }
    for id in run_other_ids.iter().take(40) {
        let doc = idx.get_document(id).await.unwrap().unwrap();
        idx.index_document(Some(id.clone()), doc).await.unwrap();
    }

    let t0 = Instant::now();
    let res = run(&idx, body.clone()).await;
    let elapsed = t0.elapsed();

    assert_eq!(
        res["total"].as_u64().unwrap(),
        (GROUPS - deleted) as u64,
        "tombstoned catalog rows must leave the fetch total"
    );
    assert_eq!(res["hits"].as_u64().unwrap(), (GROUPS - deleted) as u64);
    assert!(
        elapsed.as_millis() < 1_000,
        "catalog window fetch under tombstones took {} ms",
        elapsed.as_millis()
    );
}
