//! #3515: a query on a dotted sub-field that the mapping does not declare
//! (`title.raw` under a `text` field whose only multi-field is `.keyword`)
//! must give the same answer from the memtable and from a flushed segment.
//! Pre-fix the segment path stripped the unknown `.raw` and ran the query
//! against the analysed parent `title`:
//!
//! | query | memtable | segment | ES |
//! |---|---|---|---|
//! | `match title.raw "fruit"` | 0 | 2 | 0 |
//! | `constant_score match title.raw "fRuIt"` | 3 | 3 | 0 |
//!
//! #3533 extends the same rule to the term-level leaves: `term`, `terms`,
//! `range`, `regexp` and `exists` on `title.raw` must also match nothing,
//! before and after `_flush`. Pre-fix they read the parent value on both
//! sides (`term title.raw "Fruit"` -> [1], `exists title.raw` -> [1, 2, 3]).
//!
//! Every query below runs twice on the same documents, once before and once
//! after `flush`, and both answers must equal the ES answer. Mapped fields
//! (`title`, `title.keyword`, the object leaf `meta.owner`, and the declared
//! multi-field `name.raw`) are the controls.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::Engine;
use xerj_query::parse_request;

async fn hits(engine: &Engine, q: &Value) -> u64 {
    let idx = engine.get_index("docs").expect("get index");
    let req = parse_request(&json!({ "query": q, "size": 10 })).expect("parse_request");
    idx.search(&req).await.expect("search").total.value
}

fn cases() -> Vec<(Value, u64)> {
    vec![
        (json!({"match": {"title.raw": "fruit"}}), 0),
        (json!({"match": {"title.raw": "Fruit Salad"}}), 0),
        (
            json!({"constant_score": {"filter": {"match": {"title.raw": "fRuIt"}}}}),
            0,
        ),
        (
            json!({"bool": {"filter": [{"match": {"title.raw": "fruit"}}]}}),
            0,
        ),
        (
            json!({"bool": {"must_not": [{"match": {"title.raw": "fruit"}}]}}),
            3,
        ),
        (json!({"match_phrase": {"title.raw": "fruit salad"}}), 0),
        (json!({"match_phrase_prefix": {"title.raw": "fruit sa"}}), 0),
        (
            json!({"multi_match": {"query": "fruit", "fields": ["title.raw"]}}),
            0,
        ),
        (json!({"prefix": {"title.raw": "fru"}}), 0),
        (json!({"wildcard": {"title.raw": "fru*"}}), 0),
        (json!({"match": {"meta.owner.raw": "alpha"}}), 0),
        // #3533: term-level leaves on the unmapped sub-field match nothing.
        (json!({"term": {"title.raw": "Fruit"}}), 0),
        (json!({"terms": {"title.raw": ["fruit", "Fruit"]}}), 0),
        (json!({"range": {"title.raw": {"gte": "a"}}}), 0),
        (json!({"regexp": {"title.raw": "fru.*"}}), 0),
        (json!({"exists": {"field": "title.raw"}}), 0),
        (
            json!({"constant_score": {"filter": {"term": {"title.raw": "Fruit"}}}}),
            0,
        ),
        (
            json!({"bool": {"must": [{"term": {"title.raw": "Fruit"}}]}}),
            0,
        ),
        (
            json!({"bool": {"must_not": [{"term": {"title.raw": "Fruit"}}]}}),
            3,
        ),
        (
            json!({"bool": {"filter": [{"exists": {"field": "title.raw"}}]}}),
            0,
        ),
        (json!({"term": {"meta.owner.raw": "Alpha"}}), 0),
        // Declared multi-field controls keep working.
        (json!({"term": {"title.keyword": "Fruit"}}), 1),
        (json!({"term": {"name.raw": "Fruit"}}), 1),
        (json!({"exists": {"field": "name.raw"}}), 3),
        (json!({"exists": {"field": "title"}}), 3),
        (json!({"exists": {"field": "title.keyword"}}), 3),
        (json!({"match": {"title": "fruit"}}), 3),
        (json!({"match": {"title.keyword": "Fruit"}}), 1),
        (json!({"match": {"title.keyword": "fruit"}}), 0),
        (json!({"match": {"meta.owner": "alpha"}}), 1),
        (
            json!({"constant_score": {"filter": {"match": {"title": "salad"}}}}),
            1,
        ),
    ]
}

#[tokio::test]
async fn unmapped_dotted_subfield_is_flush_invariant() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config).expect("engine");

    let mut schema = Schema::empty();
    let mut title = FieldConfig::new("title", FieldType::Text);
    title
        .fields
        .push(FieldConfig::new("keyword", FieldType::Keyword));
    schema.fields.push(title);
    let mut meta = FieldConfig::new("meta", FieldType::Object);
    meta.fields.push(FieldConfig::new("owner", FieldType::Text));
    schema.fields.push(meta);
    // Declared multi-field control for #3533: `name.raw` IS mapped, so
    // term-level queries on it keep matching while `title.raw` matches nothing.
    let mut name = FieldConfig::new("name", FieldType::Text);
    name.fields
        .push(FieldConfig::new("raw", FieldType::Keyword));
    schema.fields.push(name);
    engine.create_index("docs", schema).expect("create");
    let idx = engine.get_index("docs").expect("get");
    for (id, doc) in [
        (
            "1",
            json!({"title": "Fruit", "meta": {"owner": "Alpha"}, "name": "Fruit"}),
        ),
        (
            "2",
            json!({"title": "fruit salad", "meta": {"owner": "Beta"}, "name": "fruit salad"}),
        ),
        (
            "3",
            json!({"title": "FRUiT", "meta": {"owner": "Gamma"}, "name": "FRUiT"}),
        ),
    ] {
        idx.index_document(Some(id.to_string()), doc)
            .await
            .expect("index");
    }

    let mut pre = Vec::new();
    for (q, _) in cases() {
        pre.push(hits(&engine, &q).await);
    }
    idx.flush().await.expect("flush");
    let mut failures = Vec::new();
    for ((q, want), before) in cases().into_iter().zip(pre) {
        let after = hits(&engine, &q).await;
        if before != want || after != want {
            failures.push(format!(
                "{q}: memtable {before}, segment {after}, want {want}"
            ));
        }
    }
    assert!(failures.is_empty(), "#3515:\n{}", failures.join("\n"));
}
