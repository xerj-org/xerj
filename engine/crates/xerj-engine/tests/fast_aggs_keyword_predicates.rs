//! Regression: `<field>.keyword` predicates on the columnar fast path.
//!
//! A text field's auto-created `.keyword` multi-field shares the *same*
//! physical doc-values column, stored under the parent's unsuffixed name.
//! Aggregation *targets* learned to fall back from `x.keyword` to `x`, but
//! the predicate resolver did not: it looked the exact name up in the
//! column map and, on a miss, resolved the whole predicate to "matches
//! nothing in this segment".  Failing closed like that is invisible — the
//! memtable arm resolves `.keyword` correctly, so a filtered panel returns
//! a small, plausible, wrong number instead of an error.
//!
//! The invariant under test is fast-path/brute-path agreement, not any
//! particular hardcoded count: the brute `_source` path is the reference
//! implementation for every agg shape.

use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::Engine;
use xerj_query::parse_request;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

/// 12 000 flushed docs + 200 still in the memtable — past
/// `FAST_AGG_MIN_DOCS` (10 000), so the columnar path is the one under
/// test, and split across both arms so a segment-only defect cannot hide
/// behind a correct memtable answer.
///
/// Flushed: `extension` cycles css/gz/zip/php (3 000 each), `group`
/// alternates a/b (6 000 each), `v` is 1.0 for a and 100.0 for b.
/// Memtable: 200 more css/a/1.0 docs, so the two arms disagree on every
/// bucket and any arm that silently contributes zero is visible.
async fn seed(idx: &std::sync::Arc<xerj_engine::Index>) {
    const EXTS: [&str; 4] = ["css", "gz", "zip", "php"];
    for i in 0..12_000u32 {
        let group = if i % 2 == 0 { "a" } else { "b" };
        let v = if i % 2 == 0 { 1.0 } else { 100.0 };
        idx.index_document(
            Some(i.to_string()),
            json!({ "extension": EXTS[(i % 4) as usize], "group": group, "v": v, "i": i }),
        )
        .await
        .unwrap();
    }
    idx.flush().await.unwrap();
    for i in 12_000..12_200u32 {
        idx.index_document(
            Some(i.to_string()),
            json!({ "extension": "css", "group": "a", "v": 1.0, "i": i }),
        )
        .await
        .unwrap();
    }
}

/// Force every subsequent agg request onto the brute `_source` path
/// WITHOUT changing a single live value: re-index one existing doc with a
/// byte-identical body.  That records an overwrite in the version map, and
/// `ghost_events() > 0` is exactly the gate `try_fast_aggs` bails on.  The
/// live corpus is unchanged, so the brute answer is the reference answer
/// for the same data the fast path just aggregated.
///
/// (The `XERJ_DISABLE_FAST_AGGS=1` kill switch is memoised in a process-wide
/// `OnceLock`, so it cannot be flipped mid-process to get the same effect.)
async fn force_brute(idx: &std::sync::Arc<xerj_engine::Index>) {
    idx.index_document(
        Some("0".to_string()),
        json!({ "extension": "css", "group": "a", "v": 1.0, "i": 0 }),
    )
    .await
    .unwrap();
}

async fn run(
    idx: &std::sync::Arc<xerj_engine::Index>,
    body: serde_json::Value,
) -> serde_json::Value {
    let req = parse_request(&body).unwrap();
    let res = idx.search(&req).await.unwrap();
    json!({
        "total": res.total.value,
        "aggs": res.aggs.clone().unwrap_or(json!(null)),
    })
}

fn filters_on_keyword() -> serde_json::Value {
    json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": {
            "by_ext": {
                "filters": {
                    "filters": {
                        "css": { "term": { "extension.keyword": "css" } },
                        "gz":  { "term": { "extension.keyword": "gz" } }
                    }
                }
            }
        }
    })
}

fn query_term_on_keyword() -> serde_json::Value {
    json!({
        "query": { "term": { "group.keyword": "b" } },
        "size": 0,
        "aggs": { "total_v": { "sum": { "field": "v" } } }
    })
}

#[tokio::test]
async fn fast_and_brute_agree_on_keyword_filters_agg() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("kwfilters", Schema::empty()).unwrap();
    let idx = engine.get_index("kwfilters").unwrap();
    seed(&idx).await;

    let fast = run(&idx, filters_on_keyword()).await;
    force_brute(&idx).await;
    let brute = run(&idx, filters_on_keyword()).await;

    assert_eq!(
        fast, brute,
        "columnar fast path disagrees with the brute reference on a \
         `filters` agg keyed by `<field>.keyword`"
    );
    // Sanity: the fixture must actually exercise both arms, so a future
    // change that makes BOTH paths return zero cannot pass this test.
    assert_eq!(
        brute["aggs"]["by_ext"]["buckets"]["css"]["doc_count"],
        3_200
    );
    assert_eq!(brute["aggs"]["by_ext"]["buckets"]["gz"]["doc_count"], 3_000);
}

#[tokio::test]
async fn fast_and_brute_agree_on_keyword_query_filter() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("kwquery", Schema::empty()).unwrap();
    let idx = engine.get_index("kwquery").unwrap();
    seed(&idx).await;

    let fast = run(&idx, query_term_on_keyword()).await;
    force_brute(&idx).await;
    let brute = run(&idx, query_term_on_keyword()).await;

    assert_eq!(
        fast, brute,
        "columnar fast path disagrees with the brute reference on a \
         top-level `term` query against `<field>.keyword`"
    );
    assert_eq!(brute["total"], 6_000);
    assert_eq!(brute["aggs"]["total_v"]["value"], 600_000.0);
}
