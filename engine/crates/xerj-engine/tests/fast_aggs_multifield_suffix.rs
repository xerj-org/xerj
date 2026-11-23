//! Regression: multi-field suffixes OTHER than `.keyword` on the columnar
//! fast path (`<field>.raw`, the classic Logstash multi-field) — at EVERY
//! depth of suffixing, not just one.
//!
//! There is no single "the brute path".  Which resolver a name is measured
//! against depends on where in the request it appears, and they disagree:
//!
//! * a top-level query filter runs through `index.rs::get_field_value`,
//!   whose multi-field fallback is RECURSIVE
//!   (`get_field_value(source, &field[..idx])`), so it strips an unbounded
//!   number of trailing segments — `extension.raw.raw`,
//!   `extension.keyword.raw` and `extension.raw.raw.raw` all read
//!   `extension`'s string;
//! * an agg TARGET and a `filters`/`filter` leaf run through
//!   `aggs::get_nested_field`, which strips exactly ONE segment, so it reads
//!   `extension.raw` but genuinely finds nothing for `extension.raw.raw`.
//!
//! Both facts are asserted below against the brute leg itself, not read off
//! the source.  `fast_aggs::dv_col` strips only `.keyword`, so every one of
//! these names found no column and the columnar path treated the segment as
//! holding no such data.  That is invisible in the response: predicates
//! resolved to "this segment matches nothing" and aggregation targets simply
//! skipped the segment, so a panel returned a small plausible number built
//! from the memtable alone.
//!
//! #3533 changed the first fact for QUERY and FILTER leaves: `extension` is
//! dynamically mapped as `text` with a `.keyword` multi-field, so every
//! other suffix on it is an unmapped leaf sub-field and a `term` / `terms`
//! on it now matches nothing on the brute path either, as in ES. The
//! lowering in `search_inner` and the guards in `doc_matches_query_typed`
//! and `doc_matches_filter` run before `get_field_value`'s recursive
//! fallback is reached. The bullets above still describe the resolvers for
//! the names #3533 does not cover (aggregation TARGETS, and schemaless
//! callers whose schema has no declared ancestor). The absolute assertions
//! below were moved with that change: the query-filter and
//! filters-predicate legs pin 0 for the unmapped suffixes and 3 200 for the
//! bare field and the declared `.keyword` multi-field, so the agreement
//! check still has a live non-zero leg to stand on.
//!
//! The fix keeps resolution narrow — widening it would make the fast path
//! claim shapes it cannot prove byte-identical, and per the above there is
//! no single behaviour to widen TO — and instead BAILS to the brute path
//! whenever a dotted name misses while ANY ancestor of it has a column.
//! After a bail the request is answered by whichever resolver it would have
//! been measured against, so bailing is right against all of them.
//!
//! The invariant under test is fast-path/brute-path agreement, not any
//! particular hardcoded count; the absolute assertions exist only so a
//! future change that makes BOTH paths return nothing cannot pass.

use serde_json::{json, Value};
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
///
/// #3533: `extension` is dynamically mapped as `text` with a `.keyword`
/// multi-field, so `extension.raw` is an unmapped leaf sub-field and a
/// `term` on it now matches nothing on BOTH paths, as in ES. The agreement
/// below is still the invariant (fast == brute); the absolute counts moved
/// from 3 200 / 3 000 to 0 with the brute reference itself.
#[tokio::test]
async fn fast_and_brute_agree_on_raw_filters_agg() {
    let brute = agree(
        "rawfilters",
        filters_term_on_raw,
        "columnar fast path disagrees with the brute reference on a \
         `filters` agg keyed by `<field>.raw`",
    )
    .await;
    assert_eq!(brute["aggs"]["by_ext"]["buckets"]["css"]["doc_count"], 0);
    assert_eq!(brute["aggs"]["by_ext"]["buckets"]["gz"]["doc_count"], 0);
}

/// Single `filter` agg with a `terms` leaf on `<field>.raw`.
/// #3533: see `fast_and_brute_agree_on_raw_filters_agg`; `terms` on the
/// unmapped `extension.raw` now matches nothing on both paths, as in ES.
#[tokio::test]
async fn fast_and_brute_agree_on_raw_filter_terms_agg() {
    let brute = agree(
        "rawfilterterms",
        filter_terms_on_raw,
        "columnar fast path disagrees with the brute reference on a \
         `filter` agg with a `terms` leaf on `<field>.raw`",
    )
    .await;
    assert_eq!(brute["aggs"]["css_or_gz"]["doc_count"], 0);
}

/// Top-level query `term` on `<field>.raw` — this one moves `hits.total`,
/// not just a bucket count.
/// #3533: see `fast_and_brute_agree_on_raw_filters_agg`; the top-level
/// `term` on the unmapped `extension.raw` now matches nothing on both
/// paths, as in ES, so the `sum` over zero docs is 0.0.
#[tokio::test]
async fn fast_and_brute_agree_on_raw_query_filter() {
    let brute = agree(
        "rawquery",
        query_term_on_raw,
        "columnar fast path disagrees with the brute reference on a \
         top-level `term` query against `<field>.raw`",
    )
    .await;
    assert_eq!(brute["total"], 0);
    assert_eq!(brute["aggs"]["total_v"]["value"], 0.0);
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
            { "key": 1, "doc_count": 6_200, "key_as_string": "true", "__xy_terms_key__": "true" },
            { "key": 0, "doc_count": 6_000, "key_as_string": "false", "__xy_terms_key__": "false" },
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

/// Every multi-field suffix SHAPE, not just the one-segment `.raw` the issue
/// happened to name.
///
/// The resolver a top-level query filter used to be measured against is
/// `index.rs::get_field_value`, whose multi-field fallback RECURSES
/// (`get_field_value(source, &field[..idx])`) and strips an unbounded number
/// of trailing segments: `extension.raw.raw` and `extension.keyword.raw`
/// both resolved to `extension`'s value on the brute path. #3533 changed
/// that for query leaves: `extension` is dynamically mapped as `text` with
/// a `.keyword` multi-field, so every other suffix on it is an unmapped
/// leaf sub-field and a `term` on it now matches nothing on the brute path
/// either, as in ES. The doubled and tripled forms below still pin the
/// columnar bail's depth (a one-level bail would have missed them), now
/// with the brute leg at 0 instead of 3 200.
///
/// One corpus, one process: run every shape on the columnar path, flip the
/// index onto the brute path without changing a live value, run them all
/// again, and compare the two maps whole.  Agreement is the invariant; the
/// absolute assertions below exist only so a change that empties BOTH paths
/// cannot pass, and after #3533 the unmapped suffixes DO empty both paths
/// by design, so the non-vacuity for the query legs rests on the bare
/// field and the declared `.keyword` multi-field, which still resolve.
#[tokio::test]
async fn fast_and_brute_agree_across_multi_field_suffix_shapes() {
    /// `.raw`/`.sort`/`.en` are real Logstash/ES multi-field conventions;
    /// the doubled and tripled forms are the shapes an unbounded-strip brute
    /// resolver used to walk and a one-level columnar bail still missed.
    /// Since #3533 the brute query leg answers 0 for every unmapped suffix,
    /// so they now pin that the bail still agrees there rather than the old
    /// 3 200.
    /// `nosuchfield.raw` is the control: no ancestor column anywhere, so
    /// BOTH paths must read it as genuinely empty rather than falling back.
    const SHAPES: [&str; 10] = [
        "extension",
        "extension.raw",
        "extension.keyword",
        "extension.sort",
        "extension.en",
        "extension.raw.raw",
        "extension.keyword.raw",
        "extension.raw.keyword",
        "extension.raw.raw.raw",
        "nosuchfield.raw",
    ];

    fn requests(shape: &str) -> Vec<(String, serde_json::Value)> {
        vec![
            (
                format!("{shape}/query-term"),
                json!({
                    "query": { "term": { shape: "css" } },
                    "size": 0,
                    "aggs": { "n": { "value_count": { "field": "i" } } }
                }),
            ),
            (
                format!("{shape}/terms-target"),
                json!({
                    "query": { "match_all": {} },
                    "size": 0,
                    "aggs": { "by": { "terms": { "field": shape, "size": 10 } } }
                }),
            ),
            (
                format!("{shape}/filters-predicate"),
                json!({
                    "query": { "match_all": {} },
                    "size": 0,
                    "aggs": {
                        "split": {
                            "filters": {
                                "filters": {
                                    "css": { "term": { shape: "css" } },
                                    "gz":  { "terms": { shape: ["gz"] } }
                                }
                            }
                        }
                    }
                }),
            ),
        ]
    }

    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine
        .create_index("suffixshapes", Schema::empty())
        .unwrap();
    let idx = engine.get_index("suffixshapes").unwrap();
    seed(&idx).await;

    let mut fast = serde_json::Map::new();
    for shape in SHAPES {
        for (label, body) in requests(shape) {
            fast.insert(label, run(&idx, body).await);
        }
    }

    force_brute(&idx).await;

    let mut brute = serde_json::Map::new();
    for shape in SHAPES {
        for (label, body) in requests(shape) {
            brute.insert(label, run(&idx, body).await);
        }
    }

    assert_eq!(
        Value::Object(fast),
        Value::Object(brute.clone()),
        "columnar fast path disagrees with the brute reference on at least one \
         multi-field suffix shape"
    );

    // ── Non-vacuity, and the two brute resolvers pinned apart ────────────
    //
    // Everything below is asserted against the BRUTE leg, so it is a
    // measurement of what the reference actually does, not a restatement of
    // what the fix assumes.  Without it, "both paths returned nothing" would
    // pass the agreement check above.
    //
    // #3533 changed what the brute reference does for QUERY and FILTER
    // predicates: `extension` is dynamically mapped as `text` with a
    // `.keyword` multi-field, so every other suffix on it is an unmapped
    // leaf sub-field and a `term` / `terms` on it now matches nothing, as
    // in ES, on the query-filter leg AND on the `filters`-predicate leg,
    // which both used to resolve through the parent. The bare field and the
    // declared `.keyword` multi-field still resolve (3 200), so the
    // agreement above is not vacuous: the corpus is live and both paths
    // still answer real matches where a field exists. Aggregation TARGETS
    // are untouched by #3533 (they are not query leaves), so the
    // `terms`-target leg still measures the agg-side resolver's one-segment
    // strip below.

    // 1. The top-level query filter resolves the bare field and the declared
    //    `.keyword` multi-field only. Every other suffix is an unmapped
    //    leaf sub-field (#3533) and matches nothing. 3 000 flushed `css`
    //    docs + 200 in the memtable where the field exists.
    for shape in ["extension", "extension.keyword"] {
        assert_eq!(
            brute[&format!("{shape}/query-term")]["total"],
            3_200,
            "the top-level-query resolver must reach `extension` from `{shape}`"
        );
    }
    for shape in [
        "extension.raw",
        "extension.sort",
        "extension.en",
        "extension.raw.raw",
        "extension.keyword.raw",
        "extension.raw.keyword",
        "extension.raw.raw.raw",
    ] {
        assert_eq!(
            brute[&format!("{shape}/query-term")]["total"],
            0,
            "#3533: `term` on the unmapped sub-field `{shape}` must match nothing"
        );
    }

    // 2. The agg-side TARGET resolver still strips exactly one segment, so
    //    a `terms` target sees the value for a single suffix and nothing at
    //    all for a double one. The `filters`-predicate leg no longer
    //    measures that rule: a `term` / `terms` filter leaf on an unmapped
    //    suffix is a query leaf, so #3533 makes it match nothing, while the
    //    bare field and the declared `.keyword` multi-field still resolve.
    for shape in ["extension", "extension.keyword"] {
        assert_eq!(
            brute[&format!("{shape}/filters-predicate")]["aggs"]["split"]["buckets"]["css"]
                ["doc_count"],
            3_200,
            "the agg-side filter on `{shape}` must resolve"
        );
    }
    for shape in ["extension.raw", "extension.sort", "extension.en"] {
        assert_eq!(
            brute[&format!("{shape}/filters-predicate")]["aggs"]["split"]["buckets"]["css"]
                ["doc_count"],
            0,
            "#3533: a filter leaf on the unmapped sub-field `{shape}` must match nothing"
        );
    }
    for shape in [
        "extension",
        "extension.raw",
        "extension.keyword",
        "extension.sort",
        "extension.en",
    ] {
        assert_eq!(
            brute[&format!("{shape}/terms-target")]["aggs"]["by"]["buckets"]
                .as_array()
                .unwrap()
                .len(),
            4,
            "`{shape}` must bucket all four extensions on the agg side"
        );
    }
    for shape in [
        "extension.raw.raw",
        "extension.keyword.raw",
        "extension.raw.keyword",
        "extension.raw.raw.raw",
    ] {
        assert_eq!(
            brute[&format!("{shape}/filters-predicate")]["aggs"]["split"]["buckets"]["css"]
                ["doc_count"],
            0,
            "#3533: a filter leaf on the doubly-suffixed `{shape}` must \
             find nothing there"
        );
        assert_eq!(
            brute[&format!("{shape}/terms-target")]["aggs"]["by"]["buckets"],
            json!([]),
            "same, for an aggregation TARGET on `{shape}`"
        );
    }

    // 3. The absent-field control: empty on both paths, and empty because
    //    nothing resolves it — not because something fell back.
    assert_eq!(brute["nosuchfield.raw/query-term"]["total"], 0);
    assert_eq!(
        brute["nosuchfield.raw/terms-target"]["aggs"]["by"]["buckets"],
        json!([])
    );
    assert_eq!(
        brute["nosuchfield.raw/filters-predicate"]["aggs"]["split"]["buckets"]["css"]["doc_count"],
        0
    );
}
