//! Regression: multi-field suffixes OTHER than `.keyword` on the columnar
//! fast path (`<field>.raw`, the classic Logstash multi-field).
//!
//! The brute reference `aggs::get_nested_field` walks `_source`, fails, and
//! then strips ONE trailing `.<segment>` and reads the parent — so
//! `extension.raw` resolves to `extension`'s value.  `fast_aggs::dv_col`
//! strips only `.keyword`, so the same name found no column and the
//! columnar path treated the segment as holding no such data.  That is
//! invisible in the response: predicates resolved to "this segment matches
//! nothing" and aggregation targets simply skipped the segment, so a panel
//! returned a small plausible number built from the memtable alone.
//!
//! The fix keeps resolution narrow (widening it would make the fast path
//! claim shapes it cannot prove byte-identical, and the two brute resolvers
//! do not even agree with each other) and instead BAILS to the brute path
//! whenever a dotted name misses but its parent column exists.
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

/// 12 000 flushed docs + 200 still in the memtable — past
/// `FAST_AGG_MIN_DOCS` (10 000), so the columnar path is the one under
/// test, and split across both arms so a segment-only defect cannot hide
/// behind a correct memtable answer.
///
/// Flushed: `extension` cycles css/gz/zip/php (3 000 each), `group`
/// alternates a/b (6 000 each), `v` is 1.0 for a and 100.0 for b, `ok` is
/// the boolean mirror of `group == "a"`.
/// Memtable: 200 more css/a/1.0/true docs, so the two arms disagree on every
/// bucket and any arm that silently contributes zero is visible.
async fn seed(idx: &std::sync::Arc<xerj_engine::Index>) {
    const EXTS: [&str; 4] = ["css", "gz", "zip", "php"];
    for i in 0..12_000u32 {
        let a = i % 2 == 0;
        let group = if a { "a" } else { "b" };
        let v = if a { 1.0 } else { 100.0 };
        idx.index_document(
            Some(i.to_string()),
            json!({
                "extension": EXTS[(i % 4) as usize],
                "group": group,
                "v": v,
                "ok": a,
                "i": i,
            }),
        )
        .await
        .unwrap();
    }
    idx.flush().await.unwrap();
    for i in 12_000..12_200u32 {
        idx.index_document(
            Some(i.to_string()),
            json!({ "extension": "css", "group": "a", "v": 1.0, "ok": true, "i": i }),
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
        json!({ "extension": "css", "group": "a", "v": 1.0, "ok": true, "i": 0 }),
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

fn filters_term_on_raw() -> serde_json::Value {
    json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": {
            "by_ext": {
                "filters": {
                    "filters": {
                        "css": { "term": { "extension.raw": "css" } },
                        "gz":  { "term": { "extension.raw": "gz" } }
                    }
                }
            }
        }
    })
}

fn filter_terms_on_raw() -> serde_json::Value {
    json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": {
            "css_or_gz": {
                "filter": { "terms": { "extension.raw": ["css", "gz"] } }
            }
        }
    })
}

fn query_term_on_raw() -> serde_json::Value {
    json!({
        "query": { "term": { "group.raw": "b" } },
        "size": 0,
        "aggs": { "total_v": { "sum": { "field": "v" } } }
    })
}

fn terms_target_on_raw() -> serde_json::Value {
    json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": { "exts": { "terms": { "field": "extension.raw", "size": 10 } } }
    })
}

fn metric_target_on_raw() -> serde_json::Value {
    json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": { "sum_v": { "sum": { "field": "v.raw" } } }
    })
}

fn nested_sub_metric_on_raw() -> serde_json::Value {
    json!({
        "query": { "match_all": {} },
        "size": 0,
        "aggs": {
            "by_group": {
                "terms": { "field": "group", "size": 10 },
                "aggs": { "sum_v": { "sum": { "field": "v.raw" } } }
            }
        }
    })
}

/// `filters` sub-agg whose leaf is a `term` on `<field>.raw` — the
/// predicate resolver's fail-closed path.
#[tokio::test]
async fn fast_and_brute_agree_on_raw_filters_agg() {
    let brute = agree(
        "rawfilters",
        filters_term_on_raw,
        "columnar fast path disagrees with the brute reference on a \
         `filters` agg keyed by `<field>.raw`",
    )
    .await;
    assert_eq!(
        brute["aggs"]["by_ext"]["buckets"]["css"]["doc_count"],
        3_200
    );
    assert_eq!(brute["aggs"]["by_ext"]["buckets"]["gz"]["doc_count"], 3_000);
}

/// Single `filter` agg with a `terms` leaf on `<field>.raw`.
#[tokio::test]
async fn fast_and_brute_agree_on_raw_filter_terms_agg() {
    let brute = agree(
        "rawfilterterms",
        filter_terms_on_raw,
        "columnar fast path disagrees with the brute reference on a \
         `filter` agg with a `terms` leaf on `<field>.raw`",
    )
    .await;
    assert_eq!(brute["aggs"]["css_or_gz"]["doc_count"], 6_200);
}

/// Top-level query `term` on `<field>.raw` — this one moves `hits.total`,
/// not just a bucket count.
#[tokio::test]
async fn fast_and_brute_agree_on_raw_query_filter() {
    let brute = agree(
        "rawquery",
        query_term_on_raw,
        "columnar fast path disagrees with the brute reference on a \
         top-level `term` query against `<field>.raw`",
    )
    .await;
    assert_eq!(brute["total"], 6_000);
    assert_eq!(brute["aggs"]["total_v"]["value"], 600_000.0);
}

/// Aggregation TARGET on `<field>.raw`.  Broader than the predicate case:
/// the executors `continue` past a column-less segment instead of bailing,
/// so the buckets were built from the memtable alone.
#[tokio::test]
async fn fast_and_brute_agree_on_raw_terms_target() {
    let brute = agree(
        "rawterms",
        terms_target_on_raw,
        "columnar fast path disagrees with the brute reference on a \
         `terms` aggregation TARGETING `<field>.raw`",
    )
    .await;
    let buckets = brute["aggs"]["exts"]["buckets"].as_array().unwrap().clone();
    let css = buckets
        .iter()
        .find(|b| b["key"] == "css")
        .expect("css bucket");
    assert_eq!(css["doc_count"], 3_200);
    assert_eq!(buckets.len(), 4);
}

/// Numeric metric TARGET on `<field>.raw` — the parent column is
/// `Column::Numeric`, so this exercises the `RangeNum`/metric side of the
/// same resolution gap.
#[tokio::test]
async fn fast_and_brute_agree_on_raw_metric_target() {
    let brute = agree(
        "rawmetric",
        metric_target_on_raw,
        "columnar fast path disagrees with the brute reference on a \
         `sum` metric TARGETING `<field>.raw`",
    )
    .await;
    // 6 000 × 1.0 + 6 000 × 100.0 + 200 × 1.0
    assert_eq!(brute["aggs"]["sum_v"]["value"], 606_200.0);
}

/// The same `.raw` metric as a SUB-agg under a columnar `terms` parent:
/// sub-metric fields are planned in `plan_subs`, a different gate from the
/// top-level `exec_agg` one.
#[tokio::test]
async fn fast_and_brute_agree_on_raw_sub_metric() {
    let brute = agree(
        "rawsubmetric",
        nested_sub_metric_on_raw,
        "columnar fast path disagrees with the brute reference on a \
         SUB-agg `sum` metric TARGETING `<field>.raw`",
    )
    .await;
    let buckets = brute["aggs"]["by_group"]["buckets"]
        .as_array()
        .unwrap()
        .clone();
    let a = buckets.iter().find(|b| b["key"] == "a").expect("a bucket");
    let b = buckets.iter().find(|b| b["key"] == "b").expect("b bucket");
    assert_eq!(a["sum_v"]["value"], 6_200.0);
    assert_eq!(b["sum_v"]["value"], 600_000.0);
}

/// The related exact-name lookup called out alongside #120:
/// `FastCtx::bool_fields` is keyed by the schema name, so a boolean field
/// asked for as `<field>.keyword` resolves its column through `dv_col`'s
/// parent fallback but is NOT recognised as boolean.  It is left exact on
/// purpose — the numeric-column arm it gates is the only one that could
/// render `0`/`1` where the brute path renders `false`/`true`, so failing
/// the gate bails to brute instead.  This asserts that outcome directly:
/// same keys, same `key_as_string`, same counts as the reference.
#[tokio::test]
async fn fast_and_brute_agree_on_bool_field_via_keyword_suffix() {
    let brute = agree(
        "boolkw",
        || {
            json!({
                "query": { "match_all": {} },
                "size": 0,
                "aggs": { "by_ok": { "terms": { "field": "ok.keyword", "size": 10 } } }
            })
        },
        "columnar fast path disagrees with the brute reference on a `terms` \
         aggregation over a BOOLEAN field named as `<field>.keyword`",
    )
    .await;
    // Boolean term keys are the numeric 0/1 ordinal plus `key_as_string` —
    // asserting the exact shape is what proves the field really is
    // boolean-mapped here, so the agreement above is not vacuous.
    assert_eq!(
        brute["aggs"]["by_ok"]["buckets"],
        json!([
            { "key": 1, "doc_count": 6_200, "key_as_string": "true" },
            { "key": 0, "doc_count": 6_000, "key_as_string": "false" },
        ])
    );
}

/// The narrow-resolution half of the contract: a dotted name whose parent
/// has NO column either is a genuine absence, and both paths must agree it
/// is empty WITHOUT the fast path bailing.  This is what stops the fix from
/// degenerating into "every dotted field goes brute".
#[tokio::test]
async fn genuinely_absent_dotted_field_is_empty_on_both_paths() {
    let brute = agree(
        "rawabsent",
        || {
            json!({
                "query": { "term": { "nosuchfield.raw": "css" } },
                "size": 0,
                "aggs": { "n": { "value_count": { "field": "v" } } }
            })
        },
        "a dotted field absent from the corpus must read empty on both paths",
    )
    .await;
    assert_eq!(brute["total"], 0);
}
