//! #3466: a `size:0` aggregation under a `term` on a numeric field took the
//! brute `_source` corpus path, because `query_node_to_agg_filter` only turned
//! a string `term` into an agg filter. On an index with overwrite history that
//! corpus is over the query-memory cap (429); below the cap it is just slow.
//!
//! On a numeric doc-values column `term: N` is the row set of
//! `range: {gte: N, lte: N}`, so a numeric `term` now takes the columnar path.
//! The rewrite is exact only while `f64` is: from 2^53 up the column merges
//! neighbouring integers and the `_source` comparator does not, so those stay
//! on the brute path.
//!
//! Every served request is compared with the same row set asked through a
//! shape that has no columnar form (`must_not`), which the brute corpus
//! answers, and with the answer computed from the documents in this file.
//! Which executor answered is read from `aggs::fast_path_aggs_served`.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::aggs::fast_path_aggs_served;
use xerj_engine::{Engine, Index};
use xerj_query::parse_request;

/// `fast_path_aggs_served()` is process-global and read as a before/after
/// delta, so two tests running at once would count each other's serves.
static ONE_AT_A_TIME: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Past `FAST_AGG_MIN_DOCS` (10,000), below which the brute path answers.
const DOCS: u32 = 12_000;
const CATS: u32 = 10;
/// Unflushed documents on top of the segment: the memtable leg.
const TAIL: u32 = 300;

/// -(2^53 + 1), -2^53, -(2^53 - 1), 2^53 - 1, 2^53, 2^53 + 1. In an f64
/// column the first two collapse onto one value and so do the last two.
const BIG: [i64; 6] = [
    -9_007_199_254_740_993,
    -9_007_199_254_740_992,
    -9_007_199_254_740_991,
    9_007_199_254_740_991,
    9_007_199_254_740_992,
    9_007_199_254_740_993,
];

fn schema() -> Schema {
    let mut s = Schema::empty();
    s.fields.push(FieldConfig::new("cat", FieldType::Keyword));
    s.fields.push(FieldConfig::new("code", FieldType::Keyword));
    s.fields.push(FieldConfig::new("g", FieldType::Long));
    s.fields.push(FieldConfig::new("x", FieldType::Double));
    s.fields.push(FieldConfig::new("big", FieldType::Long));
    s.fields.push(FieldConfig::new("n", FieldType::Long));
    s.fields.push(FieldConfig::new("flag", FieldType::Boolean));
    s
}

/// `g` cycles 0..4 and shifts by one in generation 1. `x` is the same value
/// halved (0.0, 0.5, 1.0, 1.5), `code` its keyword spelling, `label` an
/// undeclared string field (dynamically mapped to text + `.keyword`) and
/// `dyn_n` an undeclared number.
fn doc(i: u32, generation: u32) -> Value {
    let g = (i + generation) % 4;
    json!({
        "cat": format!("c{}", i % CATS),
        "code": g.to_string(),
        "label": g.to_string(),
        "g": g,
        "dyn_n": g,
        "x": f64::from(g) * 0.5,
        "big": BIG[(i % 6) as usize],
        "n": i + generation * 100_000,
        "flag": (i & 1) == 0,
    })
}

async fn engine_with_index(dir: &TempDir) -> (Engine, Arc<Index>) {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    // No background merge: it would compact the superseded rows the ghost
    // test needs resident.
    config.merge.min_merge_count = 100_000;
    let engine = Engine::new(config).expect("engine::new");
    engine.create_index("numeric_term", schema()).unwrap();
    let idx = engine.get_index("numeric_term").unwrap();
    (engine, idx)
}

async fn put(idx: &Arc<Index>, range: std::ops::Range<u32>, generation: u32) {
    for i in range {
        idx.index_document(Some(format!("d{i}")), doc(i, generation))
            .await
            .unwrap();
    }
}

struct Out {
    total: u64,
    aggs: Value,
    served: u64,
}

async fn run(idx: &Arc<Index>, body: Value) -> Out {
    let before = fast_path_aggs_served();
    let res = idx.search(&parse_request(&body).unwrap()).await.unwrap();
    Out {
        total: res.total.value,
        aggs: res.aggs.clone().unwrap_or(Value::Null),
        served: fast_path_aggs_served() - before,
    }
}

fn by_cat(query: Value) -> Value {
    json!({
        "size": 0,
        "query": query,
        "aggs": { "a": { "terms": { "field": "cat", "size": 100 } } }
    })
}

/// The same row set as `term`, through a shape the columnar path has no form
/// for. The `cat` value matches nothing, so `must_not` removes no row.
fn brute_twin(term: Value) -> Value {
    json!({ "bool": {
        "filter": [term],
        "must_not": [{ "term": { "cat": "no-such-cat" } }]
    } })
}

fn buckets(aggs: &Value) -> BTreeMap<String, u64> {
    aggs["a"]["buckets"]
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

/// The live documents the index holds, as `(doc number, g)`.
type Model = Vec<(u32, u32)>;

fn expected(model: &Model, g: u32) -> (u64, BTreeMap<String, u64>) {
    let mut total = 0;
    let mut per_cat = BTreeMap::new();
    for (i, _) in model.iter().filter(|(_, mg)| *mg == g) {
        total += 1;
        *per_cat.entry(format!("c{}", i % CATS)).or_insert(0) += 1;
    }
    (total, per_cat)
}

/// A request `term` over the columns answers like the brute twin and like the
/// documents say, and is served from doc values.
async fn assert_served_and_exact(
    idx: &Arc<Index>,
    term: Value,
    want: (u64, BTreeMap<String, u64>),
) {
    let fast = run(idx, by_cat(term.clone())).await;
    assert_eq!(fast.served, 1, "{term} must be served from doc values");
    assert_eq!(fast.total, want.0, "{term}: total");
    assert_eq!(buckets(&fast.aggs), want.1, "{term}: buckets");

    let brute = run(idx, by_cat(brute_twin(term.clone()))).await;
    assert_eq!(
        brute.served, 0,
        "the twin of {term} must take the brute path"
    );
    assert_eq!(fast.total, brute.total, "{term}: total vs brute");
    assert_eq!(fast.aggs, brute.aggs, "{term}: aggs vs brute");
}

#[tokio::test]
async fn numeric_term_agg_is_served_from_doc_values() {
    let _sequential = ONE_AT_A_TIME.lock().await;
    let dir = TempDir::new().unwrap();
    let (_engine, idx) = engine_with_index(&dir).await;
    put(&idx, 0..DOCS, 0).await;
    idx.flush().await.unwrap();
    put(&idx, DOCS..DOCS + TAIL, 0).await;

    let model: Model = (0..DOCS + TAIL).map(|i| (i, i % 4)).collect();
    // An integer needle on a long column, the same value spelled as a float,
    // an integer needle on a double column, a fractional one, a value no
    // document holds, and a field the mapping never declared.
    assert_served_and_exact(&idx, json!({ "term": { "g": 1 } }), expected(&model, 1)).await;
    assert_served_and_exact(&idx, json!({ "term": { "g": 1.0 } }), expected(&model, 1)).await;
    assert_served_and_exact(&idx, json!({ "term": { "x": 1 } }), expected(&model, 2)).await;
    assert_served_and_exact(&idx, json!({ "term": { "x": 1.5 } }), expected(&model, 3)).await;
    assert_served_and_exact(&idx, json!({ "term": { "g": 1.5 } }), (0, BTreeMap::new())).await;
    assert_served_and_exact(&idx, json!({ "term": { "g": 99 } }), (0, BTreeMap::new())).await;
    assert_served_and_exact(&idx, json!({ "term": { "dyn_n": 2 } }), expected(&model, 2)).await;

    // Beside a keyword `terms` set (a singular keyword `term` on a declared
    // keyword field stays in the FTS scoring tree), as in a dashboard panel.
    let both = json!({ "bool": { "filter": [
        { "term": { "g": 1 } },
        { "terms": { "cat": ["c3"] } }
    ] } });
    let fast = run(&idx, by_cat(both)).await;
    assert_eq!(fast.served, 1);
    let want: u64 = model
        .iter()
        .filter(|(i, g)| *g == 1 && i % CATS == 3)
        .count() as u64;
    assert_eq!(fast.total, want);
    assert_eq!(
        buckets(&fast.aggs),
        BTreeMap::from([("c3".to_string(), want)])
    );
}

/// The #3444 shape: superseded rows still resident because nothing merged
/// them. Both the row-exact plan (a lone `terms` agg) and the live-row view
/// (a `terms` with a metric under it) must count live rows only.
#[tokio::test]
async fn numeric_term_agg_over_resident_ghosts_is_exact() {
    let _sequential = ONE_AT_A_TIME.lock().await;
    let dir = TempDir::new().unwrap();
    let (_engine, idx) = engine_with_index(&dir).await;
    put(&idx, 0..DOCS, 0).await;
    idx.flush().await.unwrap();
    // Rewrite every third doc into the next `g`: some leave g == 1, some join
    // it, and each leaves a superseded row behind.
    for i in (0..DOCS).step_by(3) {
        idx.index_document(Some(format!("d{i}")), doc(i, 1))
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();
    assert_eq!(idx.live_doc_count(), DOCS as u64);
    assert_eq!(
        idx.deleted_doc_count(),
        u64::from(DOCS / 3),
        "the superseded rows must still be resident for this test to mean anything"
    );

    let model: Model = (0..DOCS)
        .map(|i| (i, (i + u32::from(i % 3 == 0)) % 4))
        .collect();
    assert_served_and_exact(&idx, json!({ "term": { "g": 1 } }), expected(&model, 1)).await;
    assert_served_and_exact(&idx, json!({ "term": { "x": 1.5 } }), expected(&model, 3)).await;

    let with_metric = json!({
        "size": 0,
        "query": { "term": { "g": 1 } },
        "aggs": { "a": { "terms": { "field": "cat", "size": 100 },
                          "aggs": { "m": { "avg": { "field": "n" } } } } }
    });
    let fast = run(&idx, with_metric).await;
    assert_eq!(
        fast.served, 1,
        "terms + avg must be served from the live rows"
    );
    let (total, per_cat) = expected(&model, 1);
    assert_eq!(fast.total, total);
    assert_eq!(buckets(&fast.aggs), per_cat);
    for b in fast.aggs["a"]["buckets"].as_array().unwrap() {
        let c: u32 = b["key"].as_str().unwrap()[1..].parse().unwrap();
        let live: Vec<f64> = model
            .iter()
            .filter(|(i, g)| *g == 1 && i % CATS == c)
            .map(|(i, _)| f64::from(i + 100_000 * u32::from(i % 3 == 0)))
            .collect();
        let want = live.iter().sum::<f64>() / live.len() as f64;
        let got = b["m"]["value"].as_f64().unwrap();
        assert!(
            (got - want).abs() < 1e-6,
            "avg(n) for c{c}: got {got}, want {want} (superseded values must not count)"
        );
    }
}

/// Past 2^53 an f64 cannot tell neighbouring integers apart but the `_source`
/// comparator can, so those needles must stay on the brute path and count
/// exactly. Below it the column is exact and serves the request.
#[tokio::test]
async fn integers_from_two_pow_53_up_count_exactly_on_the_brute_path() {
    let _sequential = ONE_AT_A_TIME.lock().await;
    let dir = TempDir::new().unwrap();
    let (_engine, idx) = engine_with_index(&dir).await;
    put(&idx, 0..DOCS, 0).await;
    idx.flush().await.unwrap();

    let each = (DOCS / 6) as u64;
    for (n, served) in [
        (json!(-9_007_199_254_740_993_i64), 0),
        (json!(-9_007_199_254_740_992_i64), 0),
        (json!(-9_007_199_254_740_991_i64), 1),
        (json!(9_007_199_254_740_991_i64), 1),
        (json!(9_007_199_254_740_992_i64), 0),
        (json!(9_007_199_254_740_993_i64), 0),
    ] {
        let out = run(&idx, by_cat(json!({ "term": { "big": n } }))).await;
        assert_eq!(out.served, served, "term big={n}");
        // 2^53 and 2^53 + 1 share one f64: counting them from the column
        // would answer 2 * DOCS / 6. The buckets are the exact count; the
        // brute path's `hits.total` for these needles still comes from the
        // f64 count shortcut and is not what this change decides.
        let counted: u64 = buckets(&out.aggs).values().sum();
        assert_eq!(counted, each, "term big={n}");
    }
    for n in [json!(u64::MAX), json!(1e300)] {
        let out = run(&idx, by_cat(json!({ "term": { "big": n } }))).await;
        let counted: u64 = buckets(&out.aggs).values().sum();
        assert_eq!((out.served, counted), (0, 0), "term big={n}");
    }
}

/// A numeric needle on a field the columns hold as something other than a
/// number answers exactly as the brute path does.
#[tokio::test]
async fn numeric_term_on_a_non_numeric_column_falls_back_exactly() {
    let _sequential = ONE_AT_A_TIME.lock().await;
    let dir = TempDir::new().unwrap();
    let (_engine, idx) = engine_with_index(&dir).await;
    put(&idx, 0..DOCS, 0).await;
    idx.flush().await.unwrap();

    let quarter = (DOCS / 4) as u64;
    // `code` and `cat` are keyword columns, `label.keyword` the dynamic
    // keyword sub-field, `flag` a boolean stored as 0/1.
    for (term, total) in [
        (json!({ "term": { "code": 1 } }), quarter),
        (json!({ "term": { "label.keyword": 1 } }), quarter),
        (json!({ "term": { "cat": 5 } }), 0),
        (json!({ "term": { "flag": 1 } }), 0),
    ] {
        let out = run(&idx, by_cat(term.clone())).await;
        let twin = run(&idx, by_cat(brute_twin(term.clone()))).await;
        assert_eq!(
            (out.total, &out.aggs),
            (twin.total, &twin.aggs),
            "{term}: served={} twin_served={}",
            out.served,
            twin.served
        );
        assert_eq!(out.total, total, "{term}");
        assert_eq!(out.served, 0, "{term} has no numeric column to serve it");
    }
}
