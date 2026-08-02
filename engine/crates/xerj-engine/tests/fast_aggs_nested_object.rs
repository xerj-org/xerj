//! Regression (#128): the columnar fast path answered a predicate or agg
//! TARGET on a field it has no doc-value column for AT ANY DEPTH as EMPTY,
//! while brute's `get_field_value` still resolves it — so fast and brute
//! disagreed. Two distinct shapes, one root cause (`build_doc_value_columns`
//! builds columns only for top-level SCALAR keys):
//!
//! * a scalar leaf nested under an OBJECT (`geo.city`, `geo` mapped object):
//!   no column for `geo` itself, none for `geo.city`; brute recurses into the
//!   `_source` object. The #120 `dotted_suffix_diverges_from_brute` bail only
//!   fires when an ANCESTOR owns a column, which a nested object never does,
//!   so the predicate resolved to `SegPred::Never` (segment matches nothing);
//!
//! * a TOP-LEVEL field whose column was SUPPRESSED because it is multi-valued
//!   (`tags` / `tags.raw`): `build_doc_value_columns` ships NO column for an
//!   array-poisoned field, so the columnar path sees no column and again
//!   answered `Never`, while brute reads the `_source` array.
//!
//! Both were identical before and after #120. The 300-vs-11,300 figures in
//! issue #128 came from a 12,000-flushed + 200-memtable corpus; the tests
//! below use a smaller fixture and assert fast/brute AGREEMENT rather than
//! those exact counts, so the numbers are the motivating measurement, not this
//! file's.
//!
//! The fix keeps columnar resolution narrow — there is no single brute
//! behaviour to widen TO — and instead BAILS to the brute path whenever a
//! field has no column at any depth in any segment yet its name (or, for a
//! dotted path, its top-level root) is one the schema maps, i.e. one brute's
//! `get_field_value` would still resolve. After a bail the request is answered
//! by the very resolver it would have been measured against.
//!
//! The invariant under test is fast-path/brute-path agreement; the absolute
//! assertions exist only so a change that makes BOTH paths return the same
//! wrong (empty) answer cannot pass. The divergence tests (nested-object and
//! suppressed-array) FAIL on the pre-fix source, where the fast leg keeps the
//! request and answers it from the memtable alone while the brute leg reads
//! the full corpus. `genuinely_absent_field_is_empty_on_both_paths` is a
//! control, not a divergence test: it passes on the pre-fix source too, and
//! exists to prove the bail does not fire for a field that is genuinely
//! absent everywhere.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::Engine;
use xerj_query::parse_request;

const CITIES: [&str; 4] = ["paris", "london", "berlin", "tokyo"];

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

/// 12 000 flushed docs + 200 still in the memtable — past `FAST_AGG_MIN_DOCS`
/// (10 000), so the columnar path is the one under test, and split across both
/// arms so a segment-only defect cannot hide behind a correct memtable answer.
///
/// Flushed: `geo.city` cycles the four `CITIES` (3 000 each). `tags` is the
/// two-element array `[city, "all"]` — a genuine multi-valued field, so its
/// `.dv` column is suppressed in the flushed segment. `v` is 1.0 everywhere,
/// so a filtered `sum` equals the match count. Memtable: 200 more `paris`
/// docs, so `paris` totals 3 200 while the other cities stay at 3 000, and a
/// segment leg that silently contributes zero is visible against the
/// non-zero memtable leg.
async fn seed(idx: &std::sync::Arc<xerj_engine::Index>) {
    for i in 0..12_000u32 {
        let city = CITIES[(i % 4) as usize];
        idx.index_document(
            Some(i.to_string()),
            json!({
                "geo": { "city": city },
                "tags": [city, "all"],
                "v": 1.0,
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
            json!({
                "geo": { "city": "paris" },
                "tags": ["paris", "all"],
                "v": 1.0,
                "i": i,
            }),
        )
        .await
        .unwrap();
    }
}

/// Force every subsequent agg request onto the brute `_source` path WITHOUT
/// changing a single live value: re-index one existing doc with a
/// byte-identical body. That records an overwrite in the version map, and
/// `ghost_events() > 0` is exactly the gate `try_fast_aggs` bails on. The live
/// corpus is unchanged, so the brute answer is the reference answer for the
/// same data the fast path just aggregated.
async fn force_brute(idx: &std::sync::Arc<xerj_engine::Index>) {
    idx.index_document(
        Some("0".to_string()),
        json!({
            "geo": { "city": CITIES[0] },
            "tags": [CITIES[0], "all"],
            "v": 1.0,
            "i": 0,
        }),
    )
    .await
    .unwrap();
}

async fn run(idx: &std::sync::Arc<xerj_engine::Index>, body: Value) -> Value {
    let req = parse_request(&body).unwrap();
    let res = idx.search(&req).await.unwrap();
    json!({
        "total": res.total.value,
        "aggs": res.aggs.clone().unwrap_or(json!(null)),
    })
}

/// Run `body` twice against an identical corpus — once with the columnar path
/// live, once with it disabled — and return the brute answer after asserting
/// the two agree.
async fn agree(name: &str, body: fn() -> Value, why: &str) -> Value {
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

// ── Predicates (top-level query filter — moves `hits.total`) ─────────────────

fn geo_city_query() -> Value {
    json!({
        "query": { "term": { "geo.city": "paris" } },
        "size": 0,
        "aggs": { "sum_v": { "sum": { "field": "v" } } }
    })
}

fn tags_query() -> Value {
    json!({
        "query": { "term": { "tags": "paris" } },
        "size": 0,
        "aggs": { "sum_v": { "sum": { "field": "v" } } }
    })
}

fn tags_raw_query() -> Value {
    json!({
        "query": { "term": { "tags.raw": "paris" } },
        "size": 0,
        "aggs": { "sum_v": { "sum": { "field": "v" } } }
    })
}

/// Predicate on a scalar leaf nested under an object. `geo` is mapped object,
/// so no column exists for it at any depth; brute recurses into `_source`.
#[tokio::test]
async fn fast_and_brute_agree_on_nested_object_predicate() {
    let brute = agree(
        "geocitypred",
        geo_city_query,
        "columnar fast path disagrees with the brute reference on a top-level \
         `term` query against a nested-object field `geo.city`",
    )
    .await;
    // 3 000 flushed `paris` + 200 memtable, each with v == 1.0.
    assert_eq!(brute["total"], 3_200);
    assert_eq!(brute["aggs"]["sum_v"]["value"], 3_200.0);
}

/// Predicate on a top-level array field whose `.dv` column is suppressed
/// because it is multi-valued.
#[tokio::test]
async fn fast_and_brute_agree_on_suppressed_array_predicate() {
    let brute = agree(
        "tagspred",
        tags_query,
        "columnar fast path disagrees with the brute reference on a top-level \
         `term` query against a suppressed multi-valued field `tags`",
    )
    .await;
    assert_eq!(brute["total"], 3_200);
    assert_eq!(brute["aggs"]["sum_v"]["value"], 3_200.0);
}

/// The `.raw` multi-field of the same suppressed array: brute's top-level
/// query resolver strips the suffix and reads `tags`, so the columnar path
/// must reach the same fallback via the field's root.
#[tokio::test]
async fn fast_and_brute_agree_on_suppressed_array_raw_suffix_predicate() {
    let brute = agree(
        "tagsrawpred",
        tags_raw_query,
        "columnar fast path disagrees with the brute reference on a top-level \
         `term` query against `tags.raw` (suppressed array, `.raw` multi-field)",
    )
    .await;
    assert_eq!(brute["total"], 3_200);
    assert_eq!(brute["aggs"]["sum_v"]["value"], 3_200.0);
}

// ── Aggregation TARGET on the suppressed array ───────────────────────────────

/// A `terms` aggregation TARGETING the suppressed array. Broader than the
/// predicate case: the executors `continue` past a column-less segment instead
/// of bailing, so pre-fix the buckets were built from the memtable alone.
#[tokio::test]
async fn fast_and_brute_agree_on_suppressed_array_terms_target() {
    let brute = agree(
        "tagsterms",
        || {
            json!({
                "query": { "match_all": {} },
                "size": 0,
                "aggs": { "by_tag": { "terms": { "field": "tags", "size": 10 } } }
            })
        },
        "columnar fast path disagrees with the brute reference on a `terms` \
         aggregation TARGETING the suppressed multi-valued field `tags`",
    )
    .await;
    let buckets = brute["aggs"]["by_tag"]["buckets"]
        .as_array()
        .unwrap()
        .clone();
    // Multi-valued: every one of the 12 200 docs carries `"all"`, and each of
    // the four cities is carried by its own docs (`paris` 3 200, the rest
    // 3 000). A memtable-only answer could never produce these counts.
    let all = buckets
        .iter()
        .find(|b| b["key"] == "all")
        .expect("all bucket");
    assert_eq!(all["doc_count"], 12_200);
    let paris = buckets
        .iter()
        .find(|b| b["key"] == "paris")
        .expect("paris bucket");
    assert_eq!(paris["doc_count"], 3_200);
    assert_eq!(buckets.len(), 5);
}

// ── The narrow-resolution half of the contract ───────────────────────────────

/// A genuinely absent field (root unmapped) must read empty on BOTH paths
/// WITHOUT the fast path bailing — this is what stops the fix from degenerating
/// into "every uncolumned field goes brute". `nosuchfield.city` is never
/// indexed, so its root isn't in the schema, and empty is the correct answer.
#[tokio::test]
async fn genuinely_absent_field_is_empty_on_both_paths() {
    let brute = agree(
        "absentnested",
        || {
            json!({
                "query": { "term": { "nosuchfield.city": "paris" } },
                "size": 0,
                "aggs": { "sum_v": { "sum": { "field": "v" } } }
            })
        },
        "a field whose root the schema never mapped must read empty on both paths",
    )
    .await;
    assert_eq!(brute["total"], 0);
}
