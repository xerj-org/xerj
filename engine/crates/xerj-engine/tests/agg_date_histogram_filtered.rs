//! #3476: a `size:0` `date_histogram` under a top-level query filter took the
//! brute `_source` corpus path (0.8-1.6 s on 200k docs, 429 past the corpus
//! budget at 300k). It is now served from doc values: the segment fold walks
//! the matching rows, and a bucket exists only once a matching row lands in it.
//!
//! Randomised equivalence: for a seeded mix of filters (keyword term, numeric
//! term, range, bool conjunction, range on the date itself) and histogram
//! shapes (calendar and fixed intervals, offset, min_doc_count, keyed, format,
//! a metric sub-agg), the fast-path response must equal
//! `aggs::run_aggs_with_all` over the matching live docs, byte for byte. Run
//! twice: on a clean index, then with overwrites and deletes resident (live-row
//! view) and a memtable tail. Both date column kinds are covered: epoch-millis
//! numbers in a `date` column and ISO strings in a `keyword` column.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::aggs::{fast_path_aggs_served, run_aggs_with_all};
use xerj_engine::Engine;
use xerj_query::parse_request;

const DOCS: u32 = 12_000;
const TAIL: u32 = 700;
/// 2026-01-01T00:00:00Z.
const T0: i64 = 1_767_225_600_000;
/// Docs span ~200 days.
const SPAN_MS: i64 = 200 * 86_400_000;

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

fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn make_doc(rng: &mut Rng, i: u32, generation: u32) -> Value {
    let mut d = json!({
        "cat": format!("c{}", rng.below(7)),
        "g": rng.below(5),
        "n": (i as i64) * 3 + generation as i64,
        "body": format!("record {i} generation {generation}"),
    });
    // ~5% of docs carry no date at all.
    if rng.below(20) != 0 {
        let ms = T0 + (rng.below(SPAN_MS as u64) as i64);
        d["ts"] = json!(ms);
        d["tss"] = json!(iso(ms));
    }
    d
}

fn schema() -> Schema {
    let mut s = Schema::empty();
    s.fields.push(FieldConfig::new("cat", FieldType::Keyword));
    s.fields.push(FieldConfig::new("g", FieldType::Long));
    s.fields.push(FieldConfig::new("n", FieldType::Long));
    s.fields.push(FieldConfig::new("ts", FieldType::Date));
    s.fields.push(FieldConfig::new("tss", FieldType::Keyword));
    s.fields.push(FieldConfig::new("body", FieldType::Text));
    s
}

/// One filter: the ES query, the same predicate in Rust, and whether the
/// fast path must serve it. A top-level `bool` reaches the aggregation on the
/// brute path today, before the columnar filter is ever tried (not this
/// change); it stays in the mix to cross-check the brute oracle.
type Filter = (Value, Box<dyn Fn(&Value) -> bool>, bool);

fn filters() -> Vec<Filter> {
    let num = |d: &Value, f: &str| d.get(f).and_then(Value::as_i64);
    vec![
        (
            json!({ "term": { "cat": "c3" } }),
            Box::new(|d: &Value| d["cat"] == "c3"),
            true,
        ),
        (
            json!({ "term": { "g": 2 } }),
            Box::new(move |d: &Value| num(d, "g") == Some(2)),
            true,
        ),
        (
            json!({ "range": { "g": { "gte": 1, "lte": 3 } } }),
            Box::new(move |d: &Value| matches!(num(d, "g"), Some(1..=3))),
            true,
        ),
        (
            json!({ "bool": { "filter": [
                { "term": { "cat": "c1" } },
                { "range": { "n": { "gte": 9000 } } }
            ] } }),
            Box::new(move |d: &Value| d["cat"] == "c1" && num(d, "n").unwrap() >= 9000),
            false,
        ),
        (
            json!({ "range": { "ts": { "gte": T0 + 40 * 86_400_000, "lt": T0 + 95 * 86_400_000 } } }),
            Box::new(move |d: &Value| {
                num(d, "ts")
                    .is_some_and(|t| (T0 + 40 * 86_400_000..T0 + 95 * 86_400_000).contains(&t))
            }),
            true,
        ),
        // Matches nothing: buckets must be empty, not the whole-index span.
        (
            json!({ "term": { "cat": "nope" } }),
            Box::new(|_: &Value| false),
            true,
        ),
    ]
}

fn histogram(rng: &mut Rng) -> Value {
    let field = if rng.below(2) == 0 { "ts" } else { "tss" };
    let mut p = json!({ "field": field });
    match rng.below(6) {
        0 => p["calendar_interval"] = json!("month"),
        1 => p["calendar_interval"] = json!("week"),
        2 => p["calendar_interval"] = json!("day"),
        3 => p["fixed_interval"] = json!("6h"),
        4 => p["fixed_interval"] = json!("1d"),
        _ => {
            p["fixed_interval"] = json!("7d");
            p["offset"] = json!("+30m");
        }
    }
    match rng.below(3) {
        0 => {}
        1 => p["min_doc_count"] = json!(1),
        _ => p["min_doc_count"] = json!(3),
    }
    if rng.below(4) == 0 {
        p["keyed"] = json!(true);
    }
    if rng.below(4) == 0 {
        p["format"] = json!("yyyy-MM-dd");
    }
    let mut agg = json!({ "date_histogram": p });
    if rng.below(2) == 0 {
        agg["aggs"] =
            json!({ "mx": { "max": { "field": "n" } }, "s": { "sum": { "field": "n" } } });
    }
    json!({ "h": agg })
}

async fn check_round(
    idx: &std::sync::Arc<xerj_engine::index::Index>,
    live: &std::collections::BTreeMap<u32, Value>,
    rng: &mut Rng,
    round: &str,
    cases: usize,
) {
    let all: Vec<Value> = live.values().cloned().collect();
    let fs = filters();
    for case in 0..cases {
        let (q, pred, fast) = &fs[case % fs.len()];
        let aggs = histogram(rng);
        let body = json!({ "size": 0, "query": q, "aggs": aggs });
        let before = fast_path_aggs_served();
        let res = idx
            .search(&parse_request(&body).unwrap())
            .await
            .unwrap_or_else(|e| panic!("{round} case {case}: {e}\n{body}"));
        if *fast {
            assert_eq!(
                fast_path_aggs_served() - before,
                1,
                "{round} case {case}: filtered date_histogram must be served from doc values\n{body}"
            );
        }
        let matching: Vec<Value> = all.iter().filter(|d| pred(d)).cloned().collect();
        assert_eq!(
            res.total.value,
            matching.len() as u64,
            "{round} case {case}: hits.total\n{body}"
        );
        let want = run_aggs_with_all(&aggs, &matching, &all);
        let got = res.aggs.expect("aggs");
        assert_eq!(
            got["h"], want["h"],
            "{round} case {case}: fast != brute\n{body}"
        );
    }
}

#[tokio::test]
async fn filtered_date_histogram_matches_brute_3476() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config).unwrap();
    engine.create_index("dh", schema()).unwrap();
    let idx = engine.get_index("dh").unwrap();

    let mut rng = Rng(0x3476);
    let mut live = std::collections::BTreeMap::new();
    for i in 0..DOCS {
        let d = make_doc(&mut rng, i, 0);
        idx.index_document(Some(format!("d{i}")), d.clone())
            .await
            .unwrap();
        live.insert(i, d);
        if i == DOCS / 2 {
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
    check_round(&idx, &live, &mut rng, "clean", 36).await;

    // Overwrite ~1/3 and delete ~1/20 of the flushed docs: the superseded rows
    // stay resident, so the fast path reads the live-row view.
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
    assert!(
        idx.deleted_doc_count() > 0,
        "ghost rows must be resident for the second round to mean anything"
    );
    check_round(&idx, &live, &mut rng, "ghosted", 48).await;
}
