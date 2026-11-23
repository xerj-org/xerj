//! #3510: a `size:0` aggregation under a top-level filter-only `bool` never
//! reached the doc-values fast path. `query_node_to_fts_projected` turns the
//! whole bool into an FTS conjunction, so `query_needs_fts` kept it on the
//! brute `_source` corpus path even when every leaf (keyword `term`, numeric
//! `range`, `exists`, ...) takes the fast path on its own.
//!
//! Randomised equivalence: a seeded mix of conjunctive bools (flat, nested,
//! under `constant_score`, `must` + `filter`, a clause that matches nothing)
//! and aggregation trees (keyword and numeric `terms`, metrics, `cardinality`,
//! `date_histogram`, a metric sub-agg). Each response must equal
//! `aggs::run_aggs_with_all` over the matching live docs, `hits.total`
//! included, and the conjunctive cases must be served by the fast path. Bools
//! that need FTS or are not conjunctions (`match` on text, `term` on analysed
//! text, `should`, `must_not`) must stay correct on the brute path. Run on a
//! clean index, then with overwrites and deletes resident plus a memtable
//! tail.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::aggs::{fast_path_aggs_served, run_aggs_with_all};
use xerj_engine::Engine;
use xerj_query::parse_request;

const DOCS: u32 = 12_000;
const TAIL: u32 = 600;
/// 2026-01-01T00:00:00Z.
const T0: i64 = 1_767_225_600_000;
const SPAN_MS: i64 = 90 * 86_400_000;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const WORDS: [&str; 4] = ["alpha", "beta", "gamma", "delta"];

fn make_doc(rng: &mut Rng, i: u32, generation: u32) -> Value {
    let mut d = json!({
        "cat": format!("c{}", rng.below(6)),
        "code": format!("k{}", rng.below(3)),
        "g": rng.below(5),
        "n": (i as i64) * 3 + generation as i64,
        "price": rng.below(1000) as f64 / 4.0,
        "body": format!("{} record {i}", WORDS[rng.below(4) as usize]),
        "ts": T0 + rng.below(SPAN_MS as u64) as i64,
    });
    // ~1 in 8 docs has no `opt` at all (an `exists` leaf must see it).
    if rng.below(8) != 0 {
        d["opt"] = json!(format!("o{}", rng.below(4)));
    }
    d
}

fn schema() -> Schema {
    let mut s = Schema::empty();
    for (name, ty) in [
        ("cat", FieldType::Keyword),
        ("code", FieldType::Keyword),
        ("opt", FieldType::Keyword),
        ("g", FieldType::Long),
        ("n", FieldType::Long),
        ("price", FieldType::Double),
        ("ts", FieldType::Date),
        ("body", FieldType::Text),
    ] {
        s.fields.push(FieldConfig::new(name, ty));
    }
    s
}

type Pred = Box<dyn Fn(&Value) -> bool>;

/// One filter: the ES query, the same predicate in Rust, and whether the fast
/// path must serve it (wherever it serves the same aggs under one keyword
/// `term`).
fn filters() -> Vec<(Value, Pred, bool)> {
    let num = |d: &Value, f: &str| d.get(f).and_then(Value::as_f64).unwrap_or(f64::NAN);
    let s = |d: &Value, f: &str| d.get(f).and_then(Value::as_str).unwrap_or("").to_string();
    vec![
        // A single `term` on analysed text: never a doc-values filter.
        (
            json!({"term": {"body": "delta"}}),
            Box::new(move |d: &Value| s(d, "body").starts_with("delta ")),
            false,
        ),
        (
            json!({"bool": {"filter": [
                {"term": {"cat": "c1"}},
                {"range": {"n": {"gte": 9000}}}
            ]}}),
            Box::new(move |d: &Value| s(d, "cat") == "c1" && num(d, "n") >= 9000.0),
            true,
        ),
        (
            json!({"bool": {
                "must": [{"term": {"code": "k2"}}],
                "filter": [{"range": {"price": {"gt": 50.0, "lte": 200.0}}}]
            }}),
            Box::new(move |d: &Value| {
                s(d, "code") == "k2" && num(d, "price") > 50.0 && num(d, "price") <= 200.0
            }),
            true,
        ),
        (
            json!({"bool": {"filter": [
                {"terms": {"cat": ["c0", "c3", "c5"]}},
                {"term": {"g": 2}},
                {"exists": {"field": "opt"}}
            ]}}),
            Box::new(move |d: &Value| {
                ["c0", "c3", "c5"].contains(&s(d, "cat").as_str())
                    && num(d, "g") == 2.0
                    && d.get("opt").is_some()
            }),
            true,
        ),
        (
            json!({"bool": {"filter": [
                {"bool": {"must": [{"term": {"code": "k0"}}]}},
                {"range": {"ts": {"gte": T0 + 20 * 86_400_000, "lt": T0 + 50 * 86_400_000}}}
            ]}}),
            Box::new(move |d: &Value| {
                let t = num(d, "ts");
                s(d, "code") == "k0"
                    && t >= (T0 + 20 * 86_400_000) as f64
                    && t < (T0 + 50 * 86_400_000) as f64
            }),
            true,
        ),
        (
            json!({"constant_score": {"filter": {"bool": {"filter": [
                {"term": {"opt": "o1"}},
                {"range": {"g": {"lte": 1}}}
            ]}}}}),
            Box::new(move |d: &Value| s(d, "opt") == "o1" && num(d, "g") <= 1.0),
            true,
        ),
        (
            json!({"bool": {"filter": [
                {"term": {"cat": "c2"}},
                {"term": {"cat": "nope"}}
            ]}}),
            Box::new(|_: &Value| false),
            true,
        ),
        // Not admitted: a `match` on analysed text needs FTS.
        (
            json!({"bool": {"filter": [
                {"match": {"body": "beta"}},
                {"term": {"cat": "c4"}}
            ]}}),
            Box::new(move |d: &Value| s(d, "body").starts_with("beta ") && s(d, "cat") == "c4"),
            false,
        ),
        // Not admitted: `term` on analysed text matches tokens (#3259).
        (
            json!({"bool": {"filter": [
                {"term": {"body": "gamma"}},
                {"range": {"g": {"gte": 3}}}
            ]}}),
            Box::new(move |d: &Value| s(d, "body").starts_with("gamma ") && num(d, "g") >= 3.0),
            false,
        ),
        // Not admitted: not a conjunction.
        (
            json!({"bool": {
                "filter": [{"term": {"cat": "c1"}}],
                "must_not": [{"term": {"code": "k1"}}]
            }}),
            Box::new(move |d: &Value| s(d, "cat") == "c1" && s(d, "code") != "k1"),
            false,
        ),
        (
            json!({"bool": {"should": [
                {"term": {"cat": "c1"}},
                {"term": {"code": "k1"}}
            ]}}),
            Box::new(move |d: &Value| s(d, "cat") == "c1" || s(d, "code") == "k1"),
            false,
        ),
    ]
}

fn aggs(rng: &mut Rng) -> Value {
    match rng.below(6) {
        0 => json!({"a": {"terms": {"field": "cat", "size": 10}}}),
        1 => json!({"a": {"terms": {"field": "g", "size": 3}}}),
        2 => json!({"a": {"stats": {"field": "price"}}, "b": {"avg": {"field": "n"}}}),
        3 => json!({"a": {"cardinality": {"field": "opt"}}, "b": {"max": {"field": "ts"}}}),
        4 => json!({"a": {"date_histogram": {"field": "ts", "calendar_interval": "week"}}}),
        _ => json!({"a": {"terms": {"field": "code"},
                     "aggs": {"s": {"sum": {"field": "price"}}}}}),
    }
}

/// The response as a client sees it: the `__`-prefixed keys are internal
/// cross-shard merge state (`cardinality` carries its value set in hash order)
/// and are stripped before rendering.
fn visible(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(k, _)| !k.starts_with("__"))
                .map(|(k, v)| (k.clone(), visible(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(visible).collect()),
        other => other.clone(),
    }
}

async fn check_round(
    idx: &std::sync::Arc<xerj_engine::index::Index>,
    live: &std::collections::BTreeMap<u32, Value>,
    rng: &mut Rng,
    round: &str,
    cases: usize,
) -> usize {
    let all: Vec<Value> = live.values().cloned().collect();
    let fs = filters();
    let mut served = 0;
    for case in 0..cases {
        let (q, pred, fast) = &fs[case % fs.len()];
        let aggs = aggs(rng);
        let body = json!({"size": 0, "query": q, "aggs": aggs});
        let before = fast_path_aggs_served();
        let res = idx
            .search(&parse_request(&body).unwrap())
            .await
            .unwrap_or_else(|e| panic!("{round} case {case}: {e}\n{body}"));
        let hit_fast = fast_path_aggs_served() - before == 1;
        if *fast {
            // The control: the same aggs under a single keyword `term`, which
            // reached the fast path before #3510. Where it does, the bool must.
            let control = json!({"size": 0, "query": {"term": {"cat": "c1"}}, "aggs": aggs});
            let before = fast_path_aggs_served();
            idx.search(&parse_request(&control).unwrap()).await.unwrap();
            if fast_path_aggs_served() - before == 1 {
                assert!(
                    hit_fast,
                    "{round} case {case}: conjunctive bool must be served from doc values\n{body}"
                );
                served += 1;
            }
        }
        let matching: Vec<Value> = all.iter().filter(|d| pred(d)).cloned().collect();
        assert_eq!(
            res.total.value,
            matching.len() as u64,
            "{round} case {case} (fast={hit_fast}): hits.total\n{body}"
        );
        let want = run_aggs_with_all(&aggs, &matching, &all);
        let got = res.aggs.expect("aggs");
        for name in ["a", "b"] {
            assert_eq!(
                got.get(name).map(visible),
                want.get(name).map(visible),
                "{round} case {case} (fast={hit_fast}): agg {name} differs from brute\n{body}"
            );
        }
    }
    served
}

#[tokio::test]
async fn filter_only_bool_aggs_match_brute_and_use_doc_values_3510() {
    // Every case repeats bodies (and the control below repeats one per agg
    // shape): a whole-result cache hit would skip the fast-path counter. Own
    // test binary with one test, so the process-wide switch reaches nothing
    // else.
    std::env::set_var("XERJ_DISABLE_QUERY_CACHE", "1");
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config).unwrap();
    engine.create_index("bf", schema()).unwrap();
    let idx = engine.get_index("bf").unwrap();

    let mut rng = Rng(0x3510);
    let mut live = std::collections::BTreeMap::new();
    for i in 0..DOCS {
        let d = make_doc(&mut rng, i, 0);
        idx.index_document(Some(format!("d{i}")), d.clone())
            .await
            .unwrap();
        live.insert(i, d);
        if i == DOCS / 3 || i == 2 * DOCS / 3 {
            idx.flush().await.unwrap();
        }
    }
    idx.flush().await.unwrap();
    for i in DOCS..DOCS + TAIL {
        let d = make_doc(&mut rng, i, 0);
        idx.index_document(Some(format!("d{i}")), d.clone())
            .await
            .unwrap();
        live.insert(i, d);
    }
    assert_eq!(idx.live_doc_count(), live.len() as u64);
    let served = check_round(&idx, &live, &mut rng, "clean", 60).await;
    assert!(served >= 25, "clean: {served} fast cases");

    // Overwrite ~1/3 and delete ~1/20 of the flushed docs: the superseded
    // rows stay resident, so the fast path reads the live-row view.
    for i in 0..DOCS {
        match rng.below(60) {
            0..=19 => {
                let d = make_doc(&mut rng, i, 1);
                idx.index_document(Some(format!("d{i}")), d.clone())
                    .await
                    .unwrap();
                live.insert(i, d);
            }
            20..=22 => {
                idx.delete_document(&format!("d{i}")).await.unwrap();
                live.remove(&i);
            }
            _ => {}
        }
    }
    idx.flush().await.unwrap();
    for i in DOCS + TAIL..DOCS + 2 * TAIL {
        let d = make_doc(&mut rng, i, 0);
        idx.index_document(Some(format!("d{i}")), d.clone())
            .await
            .unwrap();
        live.insert(i, d);
    }
    assert_eq!(idx.live_doc_count(), live.len() as u64);
    assert!(idx.deleted_doc_count() > 0, "deletes must be resident");
    let served = check_round(&idx, &live, &mut rng, "churn", 60).await;
    assert!(served >= 25, "churn: {served} fast cases");
}
