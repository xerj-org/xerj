//! #1260 — two walls between the finalize-verify query shape and the
//! columnar fast path, both measured live on the 402,814-doc
//! autoindex-catalog:
//!
//! 1. `exists` had no arm in `compile_pred`, so the verify window
//!    (`bool.filter [terms …, exists text]` + a terms agg) fell off the
//!    fast path to the brute `_source` path (15.9 s vs 63 ms cold for the
//!    exists-free twin).
//! 2. The `deletes_present` bail keyed on the MONOTONIC `ghost_events`
//!    counter, so an index that has EVER seen an overwrite — the catalog
//!    is rewritten by every finalize-catalog — never re-qualified, even
//!    after merges compacted every ghost away.  Admission is now
//!    per-segment via the ghost-position bitmap: empty bitmaps proceed
//!    (live == physical), dirty ones still bail.
//!
//! The invariant under test is fast-path/brute-path agreement, plus the
//! served/bailed signal from `aggs::fast_path_aggs_served` for the cases
//! where which executor answered IS the regression.

use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::aggs::fast_path_aggs_served;
use xerj_engine::{Engine, Index};
use xerj_query::parse_request;

/// The served/bailed assertions read the PROCESS-GLOBAL
/// `fast_path_aggs_served()` counter as a before/after delta, so two of
/// these tests running concurrently count each other's serves (CI caught
/// exactly that: run 37816544794 failed `exists_on_meta_field_bails_to_brute`
/// with delta 1 while a sibling test's fast-path serve landed inside the
/// window).  The whole file is ~1 s, so serialize it rather than inventing
/// a per-index counter only the tests would use.
static ONE_AT_A_TIME: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

/// A schema that DECLARES the given string fields as `keyword`, mirroring the
/// real autoindex catalog mapping (`catalog.rs::catalog_mapping` declares
/// `format`/`path`/`status`/`file_key` as keyword).
///
/// These tests stand in for that catalog but used `Schema::empty()`, letting
/// their string fields (`extension`, `text`, `group`) be dynamically mapped.
/// Since #1329 a dynamically mapped string is a `text` parent with NO
/// doc-values (only its `.keyword` sub-field carries them, matching ES), and a
/// `text` field never gets a doc-values column regardless of the flag
/// (`FieldConfig::stored_doc_values` excludes `Text`). The columnar fast path
/// — the terms executor and the `KwExists` arm alike — reads that doc-values
/// column, so an inferred stand-in makes every "must serve columnarly"
/// assertion bail to brute. Declaring the stand-ins keyword restores the
/// column the catalog's real keyword fields have. Numeric fields (`v`, `i`)
/// are unaffected and stay inferred.
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
        "aggs": res.aggs.clone().unwrap_or(json!(null)),
    })
}

/// 12,000 flushed docs — past `FAST_AGG_MIN_DOCS` (10,000) — cycling the
/// `text` field through present / JSON-null / absent / empty-string, so a
/// single segment's keyword column carries non-null rows, null rows for
/// both missing shapes, and the ES-visible `""` (which counts as present).
async fn seed_exists_shapes(idx: &std::sync::Arc<Index>) {
    for i in 0..12_000u32 {
        let text = match i % 4 {
            0 => json!("present"), // column row, value ""
            1 => json!(null),      // column row = null
            2 => json!(null),      // key omitted entirely below
            _ => json!(""),        // column row, empty string — PRESENT
        };
        let mut doc = json!({ "extension": if i % 3 == 0 { "css" } else { "gz" }, "i": i });
        if i % 4 != 2 {
            doc["text"] = text;
        }
        idx.index_document(Some(i.to_string()), doc).await.unwrap();
    }
    idx.flush().await.unwrap();
}

fn exists_terms_query() -> serde_json::Value {
    // The finalize-verify window shape: a terms-set filter conjoined with
    // exists, answering a terms agg.  Before #1260 the exists leaf alone
    // pushed this whole request to the brute path.
    json!({
        "query": {
            "bool": {
                "filter": [
                    { "terms": { "extension": ["css", "gz"] } },
                    { "exists": { "field": "text" } }
                ]
            }
        },
        "size": 0,
        "aggs": { "by_ext": { "terms": { "field": "extension" } } }
    })
}

#[tokio::test]
async fn exists_in_top_filter_matches_brute() {
    let _sequential = ONE_AT_A_TIME.lock().await;
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine
        .create_index("existsq", kw_schema(&["extension", "text"]))
        .unwrap();
    let idx = engine.get_index("existsq").unwrap();
    seed_exists_shapes(&idx).await;

    let before = fast_path_aggs_served();
    let fast = run(&idx, exists_terms_query()).await;
    let served = fast_path_aggs_served() - before;
    assert_eq!(
        served, 1,
        "the terms+exists conjunction must serve columnarly"
    );

    // ES semantics pinned on the reference too: `""` is present, JSON null
    // and absent are not — 2 of the 4 cycle positions match (3,000 + 3,000).
    // css among them: i%4 ∈ {0,3} ∧ i%3 == 0 → 2 per period-12 → 2,000;
    // gz gets the other 4,000.
    assert_eq!(fast["total"], 6_000);
    assert_eq!(fast["aggs"]["by_ext"]["buckets"][0]["key"], "gz");
    assert_eq!(fast["aggs"]["by_ext"]["buckets"][0]["doc_count"], 4_000);
    assert_eq!(fast["aggs"]["by_ext"]["buckets"][1]["key"], "css");
    assert_eq!(fast["aggs"]["by_ext"]["buckets"][1]["doc_count"], 2_000);

    // Brute reference via an overwrite (records a ghost, trips admission).
    idx.index_document(
        Some("0".to_string()),
        json!({ "extension": "css", "i": 0, "text": "present" }),
    )
    .await
    .unwrap();
    let brute = run(&idx, exists_terms_query()).await;
    assert_eq!(
        fast, brute,
        "columnar exists disagrees with the brute reference"
    );
}

/// `exists` over a NUMERIC column (the `NumExists` arm) — parity with
/// `value_present` on the brute path.
#[tokio::test]
async fn exists_on_numeric_field_matches_brute() {
    let _sequential = ONE_AT_A_TIME.lock().await;
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine
        .create_index("existsn", kw_schema(&["group"]))
        .unwrap();
    let idx = engine.get_index("existsn").unwrap();
    for i in 0..12_000u32 {
        let mut doc = json!({ "group": if i % 2 == 0 { "a" } else { "b" } });
        if i % 3 != 0 {
            doc["v"] = serde_json::Value::from(i as f64); // absent for every third doc
        }
        idx.index_document(Some(i.to_string()), doc).await.unwrap();
    }
    idx.flush().await.unwrap();

    let q = json!({
        "query": { "exists": { "field": "v" } },
        "size": 0,
        "aggs": {
            "n": { "value_count": { "field": "v" } },
            "by_group": { "terms": { "field": "group" } }
        }
    });
    let before = fast_path_aggs_served();
    let fast = run(&idx, q.clone()).await;
    assert_eq!(fast_path_aggs_served() - before, 1);
    assert_eq!(fast["total"], 8_000);
    assert_eq!(fast["aggs"]["n"]["value"], 8_000);

    // Byte-identical overwrite of id 0 — whose `v` is ABSENT (0 % 3 == 0),
    // so the body must omit it too, or the reference corpus changes.
    idx.index_document(Some("0".to_string()), json!({ "group": "a" }))
        .await
        .unwrap();
    let brute = run(&idx, q).await;
    assert_eq!(
        fast, brute,
        "numeric exists disagrees with the brute reference"
    );
}

/// Meta-field `exists` has no columnar form (brute answers it from
/// bookkeeping) — the fast path must BAIL, not answer it empty.
#[tokio::test]
async fn exists_on_meta_field_bails_to_brute() {
    let _sequential = ONE_AT_A_TIME.lock().await;
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("existsmeta", Schema::empty()).unwrap();
    let idx = engine.get_index("existsmeta").unwrap();
    for i in 0..12_000u32 {
        idx.index_document(Some(i.to_string()), json!({ "i": i }))
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();

    let q = json!({
        "query": { "exists": { "field": "_id" } },
        "size": 0,
        "aggs": { "n": { "value_count": { "field": "i" } } }
    });
    let before = fast_path_aggs_served();
    let out = run(&idx, q).await;
    assert_eq!(
        fast_path_aggs_served() - before,
        0,
        "meta-field exists must bail"
    );
    assert_eq!(out["total"], 12_000, "every doc has an _id");
}

/// The admission gate's safety half: an overwrite whose superseded row is
/// still resident (no merge yet) keeps the fast path OFF, and the live
/// corpus answer is unchanged by a byte-identical overwrite.
#[tokio::test]
async fn unmerged_ghosts_still_bail() {
    let _sequential = ONE_AT_A_TIME.lock().await;
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine
        .create_index("ghosts", kw_schema(&["group"]))
        .unwrap();
    let idx = engine.get_index("ghosts").unwrap();
    for i in 0..12_000u32 {
        idx.index_document(
            Some(i.to_string()),
            json!({ "group": if i % 2 == 0 { "a" } else { "b" }, "v": i as f64 }),
        )
        .await
        .unwrap();
    }
    idx.flush().await.unwrap();

    let q = json!({
        "query": { "term": { "group": "a" } },
        "size": 0,
        "aggs": { "total_v": { "sum": { "field": "v" } } }
    });
    let clean = run(&idx, q.clone()).await;
    let served_clean = fast_path_aggs_served();

    // Overwrite + delete WITHOUT merging: the flushed segment still holds
    // the superseded rows — bitmaps are dirty.
    idx.index_document(Some("0".to_string()), json!({ "group": "a", "v": 0.0 }))
        .await
        .unwrap();
    idx.delete_document("2").await.unwrap();

    let dirty = run(&idx, q).await;
    assert_eq!(
        fast_path_aggs_served(),
        served_clean,
        "a segment with resident ghosts must not serve columnarly"
    );
    // Live corpus: doc 0 byte-identical, doc 2 deleted (was group a, v=2).
    assert_eq!(clean["total"], 6_000);
    assert_eq!(dirty["total"], 5_999);
    assert_eq!(
        clean["aggs"]["total_v"]["value"].as_f64().unwrap()
            - dirty["aggs"]["total_v"]["value"].as_f64().unwrap(),
        2.0
    );
}

/// The #1260 headline: after merges compact the ghosts away the index
/// RE-QUALIFIES for the fast path even though `ghost_events` (monotonic)
/// never resets — with a live memtable on top, the production shape of the
/// autoindex-catalog (rewritten continuously, merged in the background).
#[tokio::test]
async fn merged_delete_history_re_qualifies_for_fast_path() {
    let _sequential = ONE_AT_A_TIME.lock().await;
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine
        .create_index("readmit", kw_schema(&["extension"]))
        .unwrap();
    let idx = engine.get_index("readmit").unwrap();
    for i in 0..12_000u32 {
        idx.index_document(
            Some(i.to_string()),
            json!({ "extension": if i % 3 == 0 { "css" } else { "gz" }, "i": i }),
        )
        .await
        .unwrap();
    }
    idx.flush().await.unwrap();

    // Delete history: 100 overwrites (changed bodies) + 50 deletes.
    for i in 0..100u32 {
        idx.index_document(
            Some(i.to_string()),
            json!({ "extension": "css", "i": i, "rewritten": true }),
        )
        .await
        .unwrap();
    }
    for i in 100..150u32 {
        idx.delete_document(&i.to_string()).await.unwrap();
    }
    idx.flush().await.unwrap();
    idx.force_merge(1).await.unwrap();

    // A live memtable on top of the merged index (the catalog's steady
    // state): fresh ids only, so every entry is in-memory-live.
    for i in 12_000..12_200u32 {
        idx.index_document(Some(i.to_string()), json!({ "extension": "css", "i": i }))
            .await
            .unwrap();
    }

    let q = json!({
        "query": {
            "bool": {
                // A one-element `terms` SET, not a singular `term`: on a
                // declared keyword field a singular `term` is deliberately
                // kept in the FTS scoring tree (index.rs
                // `query_node_to_fts_projected`, so a mixed bool can add its
                // BM25 score), which makes `query_needs_fts` true and routes a
                // size:0 agg to the brute path — never the columnar fast path
                // this test asserts. The real finalize-verify window
                // (`sync_executor::windowed_value_counts`) filters with a
                // `terms` set over a digest window, so `terms` is also the
                // production-faithful shape. A one-element set matches exactly
                // the same docs as `term`. (Before #1329 `extension` was an
                // inferred `text` field, classified differently, so the
                // singular `term` happened to take the fast path.)
                "filter": [
                    { "terms": { "extension": ["css"] } },
                    { "exists": { "field": "i" } }
                ]
            }
        },
        "size": 0,
        "aggs": { "by_ext": { "terms": { "field": "extension" } } }
    });
    let before = fast_path_aggs_served();
    let fast = run(&idx, q.clone()).await;
    assert_eq!(
        fast_path_aggs_served() - before,
        1,
        "a merged index with clean bitmaps must serve columnarly despite \
         its monotonic ghost history"
    );

    // Brute reference: overwrite one merged doc byte-identically (trips
    // admission without changing the live corpus).
    idx.index_document(
        Some("0".to_string()),
        json!({ "extension": "css", "i": 0, "rewritten": true }),
    )
    .await
    .unwrap();
    let brute = run(&idx, q).await;
    assert_eq!(fast, brute, "admitted fast path disagrees with brute");

    // Arithmetic: originals css = |{i < 12_000 : i % 3 == 0}| = 4_000;
    // overwrites 0..99 moved ALL 100 to css, of which 34 were css before
    // (i%3==0 for i<100) → 4_000 + 100 − 34 = 4_066; deletes 100..149
    // removed 16 css (multiples of 3 in [100,150): 102..147) → 4_050;
    // memtable +200 css → 4_250.
    assert_eq!(fast["total"], 4_250);
}

/// A memtable doc whose value for the exists field is a JSON OBJECT (the
/// field is scalar in every segment) — `value_present` says present on the
/// brute query path; `doc_matches_filter` flattened objects to `[]` and
/// disagreed.  The alignment is pinned here.
#[tokio::test]
async fn object_valued_memtable_doc_counts_as_exists() {
    let _sequential = ONE_AT_A_TIME.lock().await;
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine
        .create_index("objmem", kw_schema(&["extension", "text"]))
        .unwrap();
    let idx = engine.get_index("objmem").unwrap();
    for i in 0..12_000u32 {
        idx.index_document(
            Some(i.to_string()),
            json!({ "extension": "seg", "text": "scalar" }),
        )
        .await
        .unwrap();
    }
    idx.flush().await.unwrap();
    // Buffered (un-flushed) docs with an object under the columned field.
    for i in 12_000..12_100u32 {
        idx.index_document(
            Some(i.to_string()),
            json!({ "extension": "mem", "text": { "nested": true } }),
        )
        .await
        .unwrap();
    }

    let q = json!({
        "query": { "exists": { "field": "text" } },
        "size": 0,
        "aggs": { "by_ext": { "terms": { "field": "extension" } } }
    });
    let before = fast_path_aggs_served();
    let fast = run(&idx, q.clone()).await;
    assert_eq!(fast_path_aggs_served() - before, 1);
    assert_eq!(
        fast["total"], 12_100,
        "objects count as present on both paths"
    );

    idx.index_document(
        Some("0".to_string()),
        json!({ "extension": "seg", "text": "scalar" }),
    )
    .await
    .unwrap();
    let brute = run(&idx, q).await;
    assert_eq!(fast, brute, "memtable object exists disagrees with brute");
}
