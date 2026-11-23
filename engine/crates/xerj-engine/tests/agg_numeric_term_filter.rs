//! #3466: a `size:0` aggregation under a `term` filter on a numeric field
//! took the brute `_source` corpus path: `query_node_to_agg_filter` only
//! carried string `term` values into the columnar fast path. On an index with
//! overwrite history the corpus estimate is over `max_query_memory_mb`, so the
//! request answered 429 at `aggregation corpus materialisation`.
//!
//! A numeric `term` on a field mapped `long`/`double` is now the one-point
//! range [N, N], which the fast path serves from doc values. This test
//! overwrites half of a flushed index (ghosts resident), keeps a memtable
//! tail, installs a budget the brute corpus cannot meet, and checks the
//! numeric-term shapes are served columnarly with exact values. A numeric
//! needle on a keyword field and one at or past 2^53 are not rewritten.
//!
//! Own test binary: `Engine::new` installs `max_query_memory_mb` in the
//! process-wide governor, so no other suite may observe this budget.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::aggs::fast_path_aggs_served;
use xerj_engine::Engine;
use xerj_query::parse_request;

/// 12,000 live docs x 2 KB = 23 MiB of brute corpus against an 8 MiB budget.
const BUDGET_MB: u64 = 8;
const DOCS: u32 = 12_000;
/// Docs written after the last flush: they stay in the memtable.
const TAIL: u32 = 500;
const CATS: u32 = 10;

fn n_of(i: u32, generation: u32) -> i64 {
    (i + generation * 100_000) as i64
}

fn doc(i: u32, generation: u32) -> Value {
    json!({
        "cat": format!("c{}", i % CATS),
        "g": i % 4,
        "price": (i % 4) as f64 + 0.5,
        "code": format!("{}", i % 3),
        "n": n_of(i, generation),
        "body": format!("record {i} generation {generation} alpha beta gamma"),
    })
}

fn schema() -> Schema {
    let mut s = Schema::empty();
    s.fields.push(FieldConfig::new("cat", FieldType::Keyword));
    s.fields.push(FieldConfig::new("g", FieldType::Long));
    s.fields.push(FieldConfig::new("price", FieldType::Double));
    s.fields.push(FieldConfig::new("code", FieldType::Keyword));
    s.fields.push(FieldConfig::new("n", FieldType::Long));
    s.fields.push(FieldConfig::new("body", FieldType::Text));
    s
}

/// The live `n` of doc `i`: even docs below `DOCS` were overwritten once.
fn live_n(i: u32) -> i64 {
    if i < DOCS && i.is_multiple_of(2) {
        n_of(i, 1)
    } else {
        n_of(i, 0)
    }
}

#[tokio::test]
async fn numeric_term_filter_aggs_are_served_from_doc_values_3466() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.limits.max_query_memory_mb = BUDGET_MB;
    let engine = Engine::new(config).unwrap();
    engine.create_index("ghosted", schema()).unwrap();
    let idx = engine.get_index("ghosted").unwrap();

    for i in 0..DOCS {
        idx.index_document(Some(format!("d{i}")), doc(i, 0))
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();
    for i in (0..DOCS).step_by(2) {
        idx.index_document(Some(format!("d{i}")), doc(i, 1))
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();
    for i in DOCS..DOCS + TAIL {
        idx.index_document(Some(format!("d{i}")), doc(i, 0))
            .await
            .unwrap();
    }
    let all = DOCS + TAIL;
    assert_eq!(idx.live_doc_count(), all as u64);
    assert_eq!(
        idx.deleted_doc_count(),
        (DOCS / 2) as u64,
        "the superseded rows must still be resident for this test to mean anything"
    );

    let search = |body: Value| {
        let idx = idx.clone();
        async move { idx.search(&parse_request(&body).unwrap()).await }
    };

    // term on a long + terms agg with an avg sub-agg (not row-exact: reads
    // the live-row view), term on a double, a term inside a bool with a range,
    // and a constant_score wrapper.
    let before = fast_path_aggs_served();
    let terms_avg = search(json!({
        "size": 0,
        "query": { "term": { "g": 1 } },
        "aggs": { "a": { "terms": { "field": "cat", "size": 100 },
                          "aggs": { "m": { "avg": { "field": "n" } } } } }
    }))
    .await
    .expect("numeric term + terms/avg must not 429");
    let dbl = search(json!({
        "size": 0,
        "query": { "term": { "price": 2.5 } },
        "aggs": { "s": { "sum": { "field": "n" } } }
    }))
    .await
    .expect("numeric term on a double must not 429");
    let booled = search(json!({
        "size": 0,
        "query": { "bool": { "filter": [
            { "term": { "g": 3 } },
            { "range": { "n": { "gte": 1000 } } }
        ] } },
        "aggs": { "c": { "value_count": { "field": "n" } } }
    }))
    .await
    .expect("bool of numeric term + range must not 429");
    let constant = search(json!({
        "size": 0,
        "query": { "constant_score": { "filter": { "term": { "n": 100_002 } } } },
        "aggs": { "a": { "terms": { "field": "cat" } } }
    }))
    .await
    .expect("constant_score numeric term must not 429");
    assert_eq!(
        fast_path_aggs_served() - before,
        4,
        "all four must be served from doc values"
    );

    // Exact values, computed from the write history.
    let members = |pred: &dyn Fn(u32) -> bool| (0..all).filter(|&i| pred(i)).collect::<Vec<_>>();

    let g1 = members(&|i| i % 4 == 1);
    assert_eq!(terms_avg.total.value, g1.len() as u64);
    let aggs = terms_avg.aggs.unwrap();
    let buckets = aggs["a"]["buckets"].as_array().unwrap();
    assert_eq!(
        buckets.len(),
        CATS as usize / 2,
        "g=1 docs hit odd cats only"
    );
    for b in buckets {
        let c: u32 = b["key"].as_str().unwrap()[1..].parse().unwrap();
        let in_bucket: Vec<u32> = g1.iter().copied().filter(|i| i % CATS == c).collect();
        assert_eq!(b["doc_count"].as_u64().unwrap(), in_bucket.len() as u64);
        let want =
            in_bucket.iter().map(|&i| live_n(i) as f64).sum::<f64>() / in_bucket.len() as f64;
        let got = b["m"]["value"].as_f64().unwrap();
        assert!(
            (got - want).abs() < 1e-6,
            "avg(n) c{c}: got {got} want {want}"
        );
    }

    let p = members(&|i| i % 4 == 2);
    assert_eq!(dbl.total.value, p.len() as u64);
    let want: f64 = p.iter().map(|&i| live_n(i) as f64).sum();
    let got = dbl.aggs.unwrap()["s"]["value"].as_f64().unwrap();
    assert!((got - want).abs() < 1e-3, "sum(n): got {got} want {want}");

    let b3 = members(&|i| i % 4 == 3 && live_n(i) >= 1000);
    assert_eq!(booled.total.value, b3.len() as u64);
    assert_eq!(
        booled.aggs.unwrap()["c"]["value"].as_u64().unwrap(),
        b3.len() as u64
    );

    // n = 100_002 is doc 2's overwritten value; its first value (2) is a ghost.
    assert_eq!(constant.total.value, 1);
    let cb = constant.aggs.unwrap();
    let cb = cb["a"]["buckets"].as_array().unwrap();
    assert_eq!(cb.len(), 1);
    assert_eq!(cb[0]["key"], "c2");
    let ghost = search(json!({
        "size": 0,
        "query": { "term": { "n": 2 } },
        "aggs": { "a": { "terms": { "field": "cat" } } }
    }))
    .await
    .expect("a term that only matches a superseded row must not 429");
    assert_eq!(ghost.total.value, 0, "superseded values must not match");

    // A numeric needle on a keyword field keeps the exact brute comparator:
    // not rewritten, so over this budget the brute corpus is still refused.
    let before = fast_path_aggs_served();
    let kw = search(json!({
        "size": 0,
        "query": { "term": { "code": 1 } },
        "aggs": { "a": { "terms": { "field": "cat" } } }
    }))
    .await;
    assert_eq!(
        fast_path_aggs_served(),
        before,
        "keyword field: not rewritten"
    );
    let err = kw.expect_err("brute corpus over the budget is still refused");
    assert!(
        err.to_string()
            .contains("aggregation corpus materialisation"),
        "{err}"
    );

    // 2^53 is outside the exact-f64 window: not rewritten either.
    let big = search(json!({
        "size": 0,
        "query": { "term": { "n": 9_007_199_254_740_992_i64 } },
        "aggs": { "a": { "terms": { "field": "cat" } } }
    }))
    .await;
    assert_eq!(fast_path_aggs_served(), before, "2^53: not rewritten");
    assert!(
        big.is_err(),
        "2^53 takes the brute path, refused over budget"
    );
}
