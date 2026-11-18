//! A `term` / `terms` with a JSON-number value matches a numeric field by
//! value, whether the document spelled the number `10` or `10.0`.
//!
//! Elasticsearch parses a numeric term through the field's number type, so
//! `term {price: 10}` matches a document indexed as `{"price": 10.0}` and the
//! reverse. XERJ answered that only after a flush:
//!
//! * buffered, the fused columnar walk (`doc_values_bool_hits`) and the
//!   keyword-column walks compared the needle's string form (`"10"`) with
//!   the document's own spelling (`"10.0"`) — `size: 10` found no hits while
//!   the `size: 0` count shortcut, which compares f64 values, counted one;
//! * the `_source` comparator used serde_json's representational `Number`
//!   equality, where `PosInt(10) != Float(10.0)`.
//!
//! Every case runs buffered and flushed, at `size: 0` and `size: 10`, and once
//! more with a sort (which reaches the keyword-column walk instead of the
//! fused one).

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

async fn seed(engine: &Engine) {
    let mut schema = Schema::empty();
    for (name, ty) in [
        ("price", FieldType::Double),
        ("n", FieldType::Long),
        ("code", FieldType::Keyword),
    ] {
        schema.fields.push(FieldConfig::new(name, ty));
    }
    engine.create_index("t", schema).expect("create");
    let idx = engine.get_index("t").expect("get");
    for (id, doc) in [
        (
            "a",
            json!({"price": 10.0, "n": 2.0, "code": "10.0", "rank": 1}),
        ),
        ("b", json!({"price": 10, "n": 2, "code": "10", "rank": 2})),
        ("c", json!({"price": 10.5, "n": 3, "code": "x", "rank": 3})),
    ] {
        idx.index_document(Some(id.to_string()), doc)
            .await
            .expect("index");
    }
}

async fn run(engine: &Engine, query: &Value) -> (u64, u64, Vec<String>, Vec<String>) {
    let idx = engine.get_index("t").expect("get");
    let search = |body: Value| {
        let req = parse_request(&body).unwrap();
        let idx = idx.clone();
        async move { idx.search(&req).await.expect("search") }
    };
    let count = search(json!({"query": query, "size": 0})).await.total.value;
    let hits = search(json!({"query": query, "size": 10})).await;
    let sorted = search(json!({"query": query, "size": 10, "sort": [{"rank": "asc"}]})).await;
    let mut ids: Vec<String> = hits.hits.iter().map(|h| h.id.clone()).collect();
    ids.sort();
    let sorted_ids = sorted.hits.iter().map(|h| h.id.clone()).collect();
    (count, hits.total.value, ids, sorted_ids)
}

fn cases() -> Vec<(Value, Vec<&'static str>)> {
    vec![
        (json!({"term": {"price": 10}}), vec!["a", "b"]),
        (json!({"term": {"price": 10.0}}), vec!["a", "b"]),
        (json!({"term": {"price": 10.5}}), vec!["c"]),
        (json!({"term": {"n": 2}}), vec!["a", "b"]),
        (json!({"term": {"n": 2.0}}), vec!["a", "b"]),
        (json!({"terms": {"price": [10, 99]}}), vec!["a", "b"]),
        (json!({"terms": {"n": [2.0, 3]}}), vec!["a", "b", "c"]),
        (
            json!({"bool": {"filter": [{"term": {"price": 10}}, {"term": {"n": 2.0}}]}}),
            vec!["a", "b"],
        ),
        // A STRING needle on a keyword field stays an exact match: ES
        // compares keyword terms as bytes, so "10" is not "10.0".
        (json!({"term": {"code": "10"}}), vec!["b"]),
        (json!({"term": {"code": "10.0"}}), vec!["a"]),
        (json!({"terms": {"code": ["10"]}}), vec!["b"]),
    ]
}

async fn assert_cases(engine: &Engine, phase: &str) {
    for (query, want) in cases() {
        let (count, total, got, sorted) = run(engine, &query).await;
        assert_eq!(got, want, "[{phase}] {query}: hit ids (size:10)");
        assert_eq!(total, want.len() as u64, "[{phase}] {query}: hits.total");
        assert_eq!(
            count,
            want.len() as u64,
            "[{phase}] {query}: count (size:0)"
        );
        assert_eq!(sorted, want, "[{phase}] {query}: sorted hit ids");
    }
}

/// FAIL-BEFORE (buffered): `term price 10` → count 1 (or 2), hits 1 (`b`
/// only), so `size: 0` and `size: 10` disagreed and doc `a` was missing.
#[tokio::test]
async fn numeric_term_matches_by_value_buffered_and_flushed() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    seed(&engine).await;
    assert_cases(&engine, "buffered").await;
    engine
        .get_index("t")
        .unwrap()
        .refresh()
        .await
        .expect("refresh");
    assert_cases(&engine, "flushed").await;
}
