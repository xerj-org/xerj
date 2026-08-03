//! Regression (#143): doc-value array-suppression is PER-SEGMENT, but the
//! columnar fast path's bail predicate treated it as whole-index.
//!
//! `build_doc_value_columns` drops a field's column for a segment when ANY
//! doc of THAT segment holds an array under the field — no column beats a
//! lying one — while the segment's docs still exist.  The fast path's
//! `field_needs_brute_fallback` only bailed when NO segment carried the
//! column: one scalar-only segment vetoed the bail for the whole set, and
//! every executor then did `let Some(col) = seg.col(field) else { continue }`
//! — silently SKIPPING the suppressed segment's docs.  A terms agg lost
//! whole buckets, a term predicate resolved to "matches nothing" for rows
//! that match, and numeric metrics folded only the scalar segment.
//!
//! The shape is completely ordinary: index scalar docs, flush, then index
//! docs where the same field is an array — the first segment has the column,
//! the second doesn't, and the second's docs vanish from every columnar
//! answer while `hits.total` still counts them.
//!
//! The invariant under test is fast-path/brute-path agreement, not any
//! particular hardcoded count; the absolute assertions exist only so a
//! future change that makes BOTH paths return nothing cannot pass.

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

/// 11 000 scalar docs flushed as one segment, then 1 000 array docs flushed
/// as a second — past `FAST_AGG_MIN_DOCS` (10 000), so the columnar path is
/// the one under test.  Segment 1's docs are scalar everywhere, so it KEEPS
/// its `tags`/`score` columns; segment 2's docs hold arrays under both, so
/// its columns are suppressed at build time.  `group` stays scalar in every
/// doc of both segments — a fully-covered column, so a `terms` parent on it
/// still runs columnar and its sub-metrics are the ones on trial.
async fn seed(idx: &std::sync::Arc<xerj_engine::Index>) {
    for i in 0..11_000u32 {
        idx.index_document(
            Some(i.to_string()),
            json!({
                "tags": "a",
                "score": 5,
                "group": if i % 2 == 0 { "g1" } else { "g2" },
                "i": i,
            }),
        )
        .await
        .unwrap();
    }
    idx.flush().await.unwrap();
    for i in 11_000..12_000u32 {
        idx.index_document(
            Some(i.to_string()),
            json!({
                "tags": ["b", "c"],
                "score": [1, 2],
                "group": if i % 2 == 0 { "g1" } else { "g2" },
                "i": i,
            }),
        )
        .await
        .unwrap();
    }
    idx.flush().await.unwrap();
}

/// Force every subsequent agg request onto the brute `_source` path WITHOUT
/// changing a single live value: re-index one existing doc with a
/// byte-identical body.  That records an overwrite in the version map, and
/// `ghost_events() > 0` is exactly the gate `try_fast_aggs` bails on.  The
/// live corpus is unchanged, so the brute answer is the reference answer
/// for the same data the fast path just aggregated.
async fn force_brute(idx: &std::sync::Arc<xerj_engine::Index>) {
    idx.index_document(
        Some("0".to_string()),
        json!({ "tags": "a", "score": 5, "group": "g1", "i": 0 }),
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

/// Run `body` twice against an identical corpus — once with the columnar
/// path live, once with it disabled — and return the brute answer after
/// asserting the two agree.
async fn agree(name: &str, body: fn() -> serde_json::Value, why: &str) -> serde_json::Value {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index(name, Schema::empty()).unwrap();
    let idx = engine.get_index(name).unwrap();
    seed(&idx).await;

    let fast = run(&idx, body()).await;
    force_brute(&idx).await;
    let brute = run(&idx, body()).await;

    assert_eq!(fast, brute, "{why}");
    brute
}

/// Terms agg TARGETING the mixed-suppression field: the executor skipped
/// the suppressed segment, so the `b`/`c` buckets vanished entirely and
/// `a` was the only bucket left.
#[tokio::test]
async fn fast_and_brute_agree_on_mixed_segment_terms_agg() {
    let brute = agree(
        "mixterms",
        || {
            json!({
                "query": { "match_all": {} },
                "size": 0,
                "aggs": { "by_tag": { "terms": { "field": "tags", "size": 10 } } }
            })
        },
        "columnar fast path disagrees with the brute reference on a `terms` \
         agg over a field whose column is suppressed in ONE segment only",
    )
    .await;
    let buckets = brute["aggs"]["by_tag"]["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 3, "a, b and c must all bucket");
    let count = |key: &str| {
        buckets
            .iter()
            .find(|b| b["key"] == key)
            .unwrap_or_else(|| panic!("{key} bucket"))["doc_count"]
            .clone()
    };
    assert_eq!(count("a"), 11_000);
    assert_eq!(count("b"), 1_000);
    assert_eq!(count("c"), 1_000);
}

/// Top-level `term` query on a value that lives ONLY in the suppressed
/// segment: the predicate resolved the column miss to "this segment matches
/// nothing", so `hits.total` read 0 where brute reads 1 000.
#[tokio::test]
async fn fast_and_brute_agree_on_mixed_segment_term_query() {
    let brute = agree(
        "mixquery",
        || {
            json!({
                "query": { "term": { "tags": "b" } },
                "size": 0,
                "aggs": { "n": { "value_count": { "field": "i" } } }
            })
        },
        "columnar fast path disagrees with the brute reference on a top-level \
         `term` query hitting only the suppressed segment's rows",
    )
    .await;
    assert_eq!(brute["total"], 1_000);
}

/// The same predicate via a `filters` agg leaf — the other resolve_pred
/// consumer, where the miscount hid inside a bucket instead of `hits.total`.
#[tokio::test]
async fn fast_and_brute_agree_on_mixed_segment_filters_predicate() {
    let brute = agree(
        "mixfilters",
        || {
            json!({
                "query": { "match_all": {} },
                "size": 0,
                "aggs": {
                    "split": {
                        "filters": {
                            "filters": {
                                "b": { "term": { "tags": "b" } },
                                "a": { "term": { "tags": "a" } }
                            }
                        }
                    }
                }
            })
        },
        "columnar fast path disagrees with the brute reference on a `filters` \
         agg leaf hitting only the suppressed segment's rows",
    )
    .await;
    assert_eq!(brute["aggs"]["split"]["buckets"]["b"]["doc_count"], 1_000);
    assert_eq!(brute["aggs"]["split"]["buckets"]["a"]["doc_count"], 11_000);
}

/// Numeric metrics TARGETING the mixed-suppression field: the fold skipped
/// the suppressed segment, so avg/sum/value_count reported the scalar
/// segment's statistics as the whole index's.
#[tokio::test]
async fn fast_and_brute_agree_on_mixed_segment_numeric_metrics() {
    let brute = agree(
        "mixmetrics",
        || {
            json!({
                "query": { "match_all": {} },
                "size": 0,
                "aggs": {
                    "s": { "sum": { "field": "score" } },
                    "a": { "avg": { "field": "score" } },
                    "n": { "value_count": { "field": "score" } }
                }
            })
        },
        "columnar fast path disagrees with the brute reference on numeric \
         metrics over a field whose column is suppressed in ONE segment only",
    )
    .await;
    // The reference is the BRUTE path, asserted as it actually behaves, not
    // as ES would: its sum/avg resolvers fold ONE value per doc (the array's
    // first element), while value_count counts every element.  So sum is
    // 11 000 × 5 + 1 000 × 1, avg divides by the 12 000 docs, and
    // value_count sees all 13 000 values.  What this test pins is the
    // fast/brute AGREEMENT above; brute's own array-metric semantics are a
    // separate (pre-existing) question.
    assert_eq!(brute["aggs"]["s"]["value"], 56_000.0);
    assert_eq!(brute["aggs"]["n"]["value"], 13_000);
    assert_eq!(brute["aggs"]["a"]["value"], 56_000.0 / 12_000.0);
}

/// The same metric as a SUB-agg under a fully-covered `terms` parent:
/// sub-metric fields are planned in `plan_subs` via `seg_field_kind`, a
/// different gate from the top-level `exec_agg` one, and the per-row fold
/// skipped the suppressed segment the same way.
#[tokio::test]
async fn fast_and_brute_agree_on_mixed_segment_sub_metric() {
    let brute = agree(
        "mixsubmetric",
        || {
            json!({
                "query": { "match_all": {} },
                "size": 0,
                "aggs": {
                    "by_group": {
                        "terms": { "field": "group", "size": 10 },
                        "aggs": { "s": { "sum": { "field": "score" } } }
                    }
                }
            })
        },
        "columnar fast path disagrees with the brute reference on a SUB-agg \
         `sum` over a field whose column is suppressed in ONE segment only",
    )
    .await;
    let buckets = brute["aggs"]["by_group"]["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 2);
    for b in buckets {
        // Each group: 5 500 scalar docs × 5 + 500 array docs × 1 (brute's
        // sum folds one value per doc — see the metrics test above).
        assert_eq!(b["doc_count"], 6_000);
        assert_eq!(b["s"]["value"], 28_000.0);
    }
}
