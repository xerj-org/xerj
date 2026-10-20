//! Regression tests for the dynamic-mapping `doc_values` default.
//!
//! ES's dynamic mapping gives an inferred string field a `text` parent with a
//! `keyword` sub-field, and only the sub-field carries doc-values. The
//! engine's `es_properties_to_fields` already applies that default to a
//! DECLARED `text` mapping, but the dynamic-discovery path builds the parent
//! through `FieldConfig::new`, whose `FieldOptions` default the flag to
//! `true`. With the column present, the flushed segment answered a `terms`
//! query from the column's whole raw values while the memtable scan answered
//! per analyzed token — so the same query matched before `_refresh` and
//! returned nothing after it.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldType, Schema};
use xerj_engine::{Engine, Index};
use xerj_query::parse_request;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

/// What `POST /_search` runs: a real page of hits.
async fn search(idx: &Index, q: &Value) -> (u64, BTreeSet<String>) {
    let req = parse_request(&json!({ "query": q, "size": 50 })).expect("parse_request");
    let r = idx.search(&req).await.unwrap();
    (r.total.value, r.hits.iter().map(|h| h.id.clone()).collect())
}

/// What `POST /_count` runs: `size: 0`, total only.
async fn count_total(idx: &Index, q: &Value) -> u64 {
    let req = parse_request(&json!({ "query": q, "size": 0, "from": 0 })).expect("parse_request");
    idx.search(&req).await.unwrap().total.value
}

/// `_count` must answer the same question `_search` answers — the count lane
/// takes a different shortcut on both sides of the flush.
async fn assert_count_agrees_with_search(idx: &Index, q: &Value) {
    let count = count_total(idx, q).await;
    let (total, hits) = search(idx, q).await;
    assert_eq!(
        count, total,
        "_count total {count} != _search total {total}"
    );
    assert_eq!(
        count,
        hits.len() as u64,
        "_count says {count} but _search materialises {} document(s)",
        hits.len()
    );
}

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

/// The inferred schema mirrors ES's dynamic mapping: `text` parent with the
/// flag OFF, `keyword` sub-field with its own doc-values, and a numeric
/// sibling keeping theirs.
#[tokio::test]
async fn inferred_text_has_no_doc_values_but_its_keyword_subfield_and_numerics_do() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("dyn_schema", Schema::empty()).unwrap();
    let idx = engine.get_index("dyn_schema").unwrap();
    idx.index_document(
        Some("d1".into()),
        json!({"text": "quick brown fox", "n": 42}),
    )
    .await
    .unwrap();

    let schema = idx.schema().await;
    let text = schema
        .fields
        .iter()
        .find(|f| f.name == "text")
        .expect("text field");
    assert!(matches!(text.field_type, FieldType::Text));
    assert!(
        !text.options.doc_values,
        "ES dynamic mapping puts doc-values on the keyword sub-field, \
         not on the text parent"
    );
    let kw = text
        .fields
        .iter()
        .find(|f| f.name == "keyword")
        .expect("keyword sub-field");
    assert!(matches!(kw.field_type, FieldType::Keyword));
    assert!(kw.options.doc_values);
    assert_eq!(kw.options.ignore_above, Some(256));
    let n = schema
        .fields
        .iter()
        .find(|f| f.name == "n")
        .expect("n field");
    assert!(
        n.options.doc_values,
        "numeric doc-values default is unchanged"
    );
}

/// A `terms` query on a dynamically mapped text field answers the analyzed
/// tokens before and after the flush, with `_count` agreeing at both ends,
/// and the `.keyword` sub-field keeps answering the whole value.
#[tokio::test]
async fn dynamic_text_terms_matches_tokens_flush_invariantly() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("dyn_terms", Schema::empty()).unwrap();
    let idx: Arc<Index> = engine.get_index("dyn_terms").unwrap();
    for (id, text) in [("d1", "quick brown fox"), ("d2", "lazy dog")] {
        idx.index_document(Some(id.into()), json!({"text": text}))
            .await
            .unwrap();
    }

    let token_query = json!({"terms": {"text": ["brown"]}});
    let (pre_total, pre_hits) = search(&idx, &token_query).await;
    assert_eq!(pre_hits, set(&["d1"]), "pre-flush (memtable) token match");
    assert_eq!(pre_total, 1);
    assert_count_agrees_with_search(&idx, &token_query).await;

    idx.flush().await.unwrap();

    let (post_total, post_hits) = search(&idx, &token_query).await;
    assert_eq!(
        post_hits, pre_hits,
        "terms on a dynamically mapped text field changed at flush"
    );
    assert_eq!(post_total, pre_total, "hit total changed at flush");
    assert_count_agrees_with_search(&idx, &token_query).await;

    let keyword_query = json!({"term": {"text.keyword": "quick brown fox"}});
    assert_eq!(search(&idx, &keyword_query).await.1, set(&["d1"]));
}
