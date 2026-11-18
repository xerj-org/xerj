//! Finalize-verify (`size:0` + `terms` agg + `exists` on a
//! `doc_values: false` text field) must not materialise the stored corpus.
//!
//! Issue #1940: `exists` on `semantic_text` / `text` had no doc-values
//! column, so the filtered fast path refused the request and the brute
//! corpus cloned every stored document. A 10k-document window then grew
//! RSS with the corpus. The presence bitmap answers the same
//! `value_present` predicate from the `.dv` sidecar.
//!
//! The counters are the bound. They do not move with document count:
//! `corpus_docs_hydrated` counts stored documents cloned into the brute
//! aggregation corpus, and `stored_sections_cached` counts decoded stored
//! sections. Both stay flat across a full window, a one-digest window, and
//! a zero-hit digest.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::aggs::{corpus_docs_hydrated, fast_path_aggs_served};
use xerj_engine::{Engine, Index};
use xerj_query::parse_request;

/// Clears `FAST_AGG_MIN_DOCS` (10_000). 100 groups × 100 docs.
const GROUPS: usize = 100;
const DOCS_PER_GROUP: usize = 100;
const DOCS: usize = GROUPS * DOCS_PER_GROUP;

static ONE_AT_A_TIME: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

fn schema() -> Schema {
    let mut s = Schema::empty();
    s.fields
        .push(FieldConfig::new("ax_file", FieldType::Keyword));
    let mut msg = FieldConfig::new("msg", FieldType::Text);
    msg.options.doc_values = false;
    s.fields.push(msg);
    s
}

fn digest(g: usize) -> String {
    format!("axf-{g:04}")
}

/// Variant `i % 10`. Present (exists = true) for `""` and any non-null
/// string, including a non-null element inside an array. Absent for a
/// missing key, JSON null, `[]`, and `[null]`.
fn msg_value(i: usize) -> Option<Value> {
    match i % 10 {
        0 => None,
        1 => Some(Value::Null),
        2 => Some(json!([])),
        3 => Some(json!("")),
        4 => Some(json!([null])),
        _ => Some(json!(format!("record {i} {}", "x".repeat(1024)))),
    }
}

fn exists_variant(i: usize) -> bool {
    matches!(i % 10, 3 | 5 | 6 | 7 | 8 | 9)
}

async fn seed(idx: &Arc<Index>) {
    for i in 0..DOCS {
        let g = i / DOCS_PER_GROUP;
        let mut doc = serde_json::Map::new();
        doc.insert("ax_file".to_string(), json!(digest(g)));
        if let Some(msg) = msg_value(i) {
            doc.insert("msg".to_string(), msg);
        }
        idx.index_document(Some(format!("doc-{i}")), Value::Object(doc))
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();
}

fn verify_query(digests: &[String]) -> Value {
    json!({
        "size": 0,
        "query": {
            "bool": {
                "filter": [
                    { "terms": { "ax_file": digests } },
                    { "exists": { "field": "msg" } }
                ]
            }
        },
        "aggs": {
            "values": {
                "terms": { "field": "ax_file", "size": digests.len().max(1) }
            }
        }
    })
}

async fn search(idx: &Arc<Index>, body: Value) -> xerj_query::executor::SearchResult {
    let req = parse_request(&body).unwrap();
    idx.search(&req).await.unwrap()
}

fn buckets(aggs: &Value) -> HashMap<String, u64> {
    aggs["values"]["buckets"]
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

#[tokio::test]
async fn text_exists_verify_does_not_hydrate() {
    let _sequential = ONE_AT_A_TIME.lock().await;
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("verify", schema()).unwrap();
    let idx = engine.get_index("verify").unwrap();
    seed(&idx).await;

    let expected_per_group = (0..DOCS_PER_GROUP).filter(|r| exists_variant(*r)).count() as u64;
    assert_eq!(expected_per_group, 60);
    let all: Vec<String> = (0..GROUPS).map(digest).collect();

    let cached_before = idx.stored_sections_cached();
    let hydrated_before = corpus_docs_hydrated();
    let served_before = fast_path_aggs_served();

    let full = search(&idx, verify_query(&all)).await;
    assert_eq!(full.total.value, expected_per_group * GROUPS as u64);
    let full_buckets = buckets(full.aggs.as_ref().unwrap());
    assert_eq!(full_buckets.len(), GROUPS);
    for g in 0..GROUPS {
        assert_eq!(full_buckets[&digest(g)], expected_per_group);
    }

    let one = search(&idx, verify_query(&[digest(3)])).await;
    assert_eq!(one.total.value, expected_per_group);
    let one_buckets = buckets(one.aggs.as_ref().unwrap());
    assert_eq!(one_buckets.len(), 1);
    assert_eq!(one_buckets[&digest(3)], expected_per_group);

    let miss = search(&idx, verify_query(&["no-such-digest".to_string()])).await;
    assert_eq!(miss.total.value, 0);
    assert!(buckets(miss.aggs.as_ref().unwrap()).is_empty());

    // size:1 zero-hit, no aggs. The scored path must answer `exists` from
    // the bitmap rather than decoding the stored section up front.
    let page = search(
        &idx,
        json!({
            "size": 1,
            "query": {
                "bool": {
                    "filter": [
                        { "terms": { "ax_file": ["no-such-digest"] } },
                        { "exists": { "field": "msg" } }
                    ]
                }
            }
        }),
    )
    .await;
    assert_eq!(page.total.value, 0);
    assert!(page.hits.is_empty());

    assert_eq!(
        corpus_docs_hydrated() - hydrated_before,
        0,
        "verify/exists cloned stored documents into the aggregation corpus"
    );
    assert_eq!(
        idx.stored_sections_cached(),
        cached_before,
        "verify/exists decoded a stored section"
    );
    assert!(
        fast_path_aggs_served() - served_before >= 3,
        "the three size:0 windows must be served by the columnar fast path"
    );

    // Brute reference. `missing` is not a fast-path terms parameter, so the
    // executor refuses and the source scan answers. Every document has
    // `ax_file`, so the placeholder bucket stays empty and the live buckets
    // match the columnar result.
    let mut reference = verify_query(&all);
    reference["aggs"]["values"]["terms"]["missing"] = json!("__absent__");
    let ref_served = fast_path_aggs_served();
    let brute = search(&idx, reference).await;
    assert_eq!(
        fast_path_aggs_served() - ref_served,
        0,
        "the missing parameter must force the brute executor"
    );
    assert_eq!(brute.total.value, full.total.value);
    assert_eq!(buckets(brute.aggs.as_ref().unwrap()), full_buckets);
}
