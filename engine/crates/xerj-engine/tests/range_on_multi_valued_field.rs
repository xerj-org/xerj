//! Issue #1750: a `range` query must match a document when ANY one of a
//! multi-valued field's values is in range — Lucene's per-value semantics, and
//! what Elasticsearch 8.13.4 returns for the same requests.
//!
//! ROOT CAUSE (two independent defects):
//!
//! 1. The `_source` matcher (`doc_matches_query_typed`'s `Range` arm) compared
//!    the whole array. `json_compare` has no array arm, so it fell back to the
//!    serialized text (`["alpha","mid"]`, `[22,8080]`) and compared that
//!    lexicographically: `'['` sorts after every digit and before every
//!    lowercase letter, so an array passed any numeric/date lower bound, failed
//!    any upper bound, and failed a keyword `gte "m"` — whatever its elements.
//!    Every flushed segment reaches this matcher for a multi-valued field
//!    (`build_doc_value_columns` emits no column for it), as do memtable
//!    keyword/date/ip ranges.
//! 2. The memtable numeric range fast paths (`doc_values_range_query`, and the
//!    `try_shortcut_count` count over `for_each_numeric_value`) read a column
//!    that keeps only an array's FIRST element, with no `array_fields`
//!    bailout — unlike the `term` paths (#423) and the fused bool walk.
//!
//! Every case runs before and after a flush, at `size: 0` (the count
//! shortcuts) and `size: 10` (the hit path), so each path is pinned.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::Engine;
use xerj_query::parse_request;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

/// The #1750 reproduction, plus an `ip` field and two negative documents.
async fn seed(engine: &Engine) {
    let mut schema = Schema::empty();
    for (name, ty) in [
        ("tags", FieldType::Keyword),
        ("ports", FieldType::Long),
        ("seen", FieldType::Date),
        ("addr", FieldType::Ip),
    ] {
        schema.fields.push(FieldConfig::new(name, ty));
    }
    engine.create_index("events", schema).expect("create");
    let idx = engine.get_index("events").expect("get");
    let docs = [
        (
            "1",
            json!({"tags": ["alpha", "mid"], "ports": [22, 8080],
                   "seen": ["2020-01-01", "2024-06-01"],
                   "addr": ["10.0.0.1", "192.168.1.7"]}),
        ),
        (
            "2",
            json!({"tags": "mid", "ports": 443, "seen": "2024-01-01", "addr": "10.0.0.9"}),
        ),
        (
            "3",
            json!({"tags": "zeta", "ports": 5, "seen": ["2030-01-01", "2021-06-01"],
                   "addr": "172.16.0.1"}),
        ),
        // Every element below any bound used below: must never match a
        // lower-bounded range (the old matcher admitted every array there).
        (
            "4",
            json!({"tags": ["aa", "ab"], "ports": [1, 2],
                   "seen": ["1990-01-01", "1991-01-01"], "addr": ["1.1.1.1", "1.1.1.2"]}),
        ),
    ];
    for (id, doc) in docs {
        idx.index_document(Some(id.to_string()), doc)
            .await
            .expect("index");
    }
}

async fn ids(engine: &Engine, range: Value) -> (u64, u64, Vec<String>) {
    let idx = engine.get_index("events").expect("get");
    let count = idx
        .search(&parse_request(&json!({"query": {"range": range.clone()}, "size": 0})).unwrap())
        .await
        .expect("count search")
        .total
        .value;
    let res = idx
        .search(&parse_request(&json!({"query": {"range": range}, "size": 10})).unwrap())
        .await
        .expect("hit search");
    let mut ids: Vec<String> = res.hits.iter().map(|h| h.id.clone()).collect();
    ids.sort();
    (count, res.total.value, ids)
}

/// `(range body, expected ids)`. Expectations are Elasticsearch 8.13.4's for
/// the #1750 requests; the rest follow the same per-value rule.
fn cases() -> Vec<(Value, Vec<&'static str>)> {
    vec![
        // #1750 verbatim: ES counts 2, 1, 1.
        (json!({"tags": {"gte": "m", "lt": "n"}}), vec!["1", "2"]),
        (json!({"ports": {"gte": 8000, "lte": 9000}}), vec!["1"]),
        (
            json!({"seen": {"gte": "2021-01-01", "lt": "2023-01-01"}}),
            vec!["3"],
        ),
        // Bounds are not split across elements: doc 1 has a value below and
        // a value above this window, but none inside it.
        (json!({"ports": {"gt": 22, "lt": 443}}), vec![]),
        (
            json!({"seen": {"gte": "2021-01-01", "lt": "2024-01-01"}}),
            vec!["3"],
        ),
        // Lower bound only: doc 4's values are all below it.
        (json!({"seen": {"gte": "2024-05-01"}}), vec!["1", "3"]),
        (json!({"ports": {"gte": 100}}), vec!["1", "2"]),
        // Upper bound only: the old matcher never admitted an array here.
        (json!({"ports": {"lte": 5}}), vec!["3", "4"]),
        (json!({"seen": {"lt": "2000-01-01"}}), vec!["4"]),
        (
            json!({"addr": {"gte": "192.168.0.0", "lte": "192.168.255.255"}}),
            vec!["1"],
        ),
    ]
}

async fn assert_cases(engine: &Engine, phase: &str) {
    for (range, want) in cases() {
        let (count, total, got) = ids(engine, range.clone()).await;
        assert_eq!(
            got, want,
            "#1750 [{phase}] range {range}: hit ids (size:10) disagree"
        );
        assert_eq!(
            total,
            want.len() as u64,
            "#1750 [{phase}] range {range}: hits.total (size:10)"
        );
        assert_eq!(
            count,
            want.len() as u64,
            "#1750 [{phase}] range {range}: hits.total (size:0 count path)"
        );
    }
}

#[tokio::test]
async fn range_matches_any_element_of_a_multi_valued_field() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    seed(&engine).await;

    assert_cases(&engine, "buffered").await;

    engine
        .get_index("events")
        .unwrap()
        .refresh()
        .await
        .expect("refresh");
    assert_cases(&engine, "flushed").await;
}

/// `filter` aggregation with a `range` clause counts like the query does
/// (aggs.rs `doc_matches_filter` read only the first value).
#[tokio::test]
async fn filter_agg_range_matches_any_element() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    seed(&engine).await;
    let idx = engine.get_index("events").unwrap();

    for phase in ["buffered", "flushed"] {
        if phase == "flushed" {
            idx.refresh().await.expect("refresh");
        }
        let req = parse_request(&json!({
            "size": 0,
            "aggs": {
                "p": {"filter": {"range": {"ports": {"gte": 8000, "lte": 9000}}}},
                "s": {"filter": {"range": {"seen": {"gte": "2021-01-01", "lt": "2023-01-01"}}}}
            }
        }))
        .unwrap();
        let res = idx.search(&req).await.expect("agg search");
        let aggs = res.aggs.as_ref().expect("aggs present");
        assert_eq!(
            aggs["p"]["doc_count"],
            json!(1),
            "#1750 [{phase}] filter agg ports 8000..9000: {aggs}"
        );
        assert_eq!(
            aggs["s"]["doc_count"],
            json!(1),
            "#1750 [{phase}] filter agg seen 2021..2023: {aggs}"
        );
    }
}
