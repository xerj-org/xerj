//! Fast resource-accounting regressions for the real engine ingest path.
//!
//! These tests deliberately assert logical ownership and collection-capacity
//! formulas. They do not sample RSS, allocator timing, CPU time, or background
//! thread scheduling, so they remain deterministic in the normal parallel
//! suite and require no model download or network access.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::{
    config::Config,
    types::{EmbeddingConfig, FieldConfig, FieldType, Schema},
};
use xerj_query::ast::SearchRequest;

use crate::{
    bulk::{
        process_bulk, with_semantic_lifecycle_hook, SemanticLifecycleFault, SemanticLifecycleHook,
        SemanticLifecyclePoint,
    },
    ingest_memory::{self, Category, Ledger, Measurement},
    Engine,
};

const SEMANTIC_CATEGORIES: [Category; 5] = [
    Category::RawSource,
    Category::ParsedSource,
    Category::ParsedVector,
    Category::PreparedDocument,
    Category::PreparedVector,
];

fn engine_with_index(name: &str, semantic: bool) -> (TempDir, Engine) {
    let dir = TempDir::new().expect("temp data directory");
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    let engine = Engine::new(config).expect("engine");
    let mut schema = Schema::empty();
    if semantic {
        let mut content = FieldConfig::new("content", FieldType::Text);
        content.options.dimensions = Some(16);
        content.embedding = Some(EmbeddingConfig {
            endpoint: None,
            model: None,
            target_field: Some("content_vector".into()),
        });
        schema.fields.push(content);
    }
    engine.create_index(name, schema).expect("create index");
    if semantic {
        engine
            .put_index_mapping(
                name,
                json!({"properties": {"content": {"type": "semantic_text"}}}),
            )
            .unwrap();
    }
    (dir, engine)
}

fn current(ledger: &Ledger, category: Category) -> u64 {
    ledger.gauge(category, Measurement::Estimated).current
}

fn semantic_current(ledger: &Ledger) -> u64 {
    SEMANTIC_CATEGORIES
        .into_iter()
        .map(|category| current(ledger, category))
        .sum()
}

#[test]
fn json_and_caller_vector_capacity_formulas_are_exact() {
    let value = json!({
        "content": "owned text",
        "nested": {"label": "inner"},
        "content_vector": [0.1, 0.2, [0.3, 0.4]],
    });
    let fields = HashSet::from(["content_vector".to_string()]);
    let vector = value["content_vector"].as_array().unwrap();
    let nested = vector[2].as_array().unwrap();
    let expected_vector = vector.capacity() * std::mem::size_of::<Value>()
        + nested.capacity() * std::mem::size_of::<Value>();
    assert_eq!(
        ingest_memory::vector_value_capacity(&value, &fields),
        expected_vector
    );

    let expected_document = ingest_memory::estimated_json_heap_excluding(&value, &fields);
    assert!(expected_document >= "owned text".len() + "inner".len());
    assert_eq!(
        expected_document + expected_vector,
        ingest_memory::estimated_json_heap(&value)
    );
}

#[tokio::test]
async fn unique_docs_vectors_and_parse_failure_release_every_owner() {
    let name = "bounded-semantic";
    let (_dir, engine) = engine_with_index(name, true);
    let ledger: &'static Ledger = Box::leak(Box::new(Ledger::new()));
    let valid_derived = r#"{"content":"derive a deterministic lexical embedding"}"#;
    let valid_supplied = r#"{"content":"keep caller vector","content_vector":[0.1,0.2,0.3,0.4]}"#;
    let malformed = r#"{"content":"unterminated""#;
    let body = format!(
        "{{\"index\":{{\"_index\":\"{name}\",\"_id\":\"derived\"}}}}\n{valid_derived}\n\
         {{\"index\":{{\"_index\":\"{name}\",\"_id\":\"supplied\"}}}}\n{valid_supplied}\n\
         {{\"index\":{{\"_index\":\"{name}\",\"_id\":\"bad\"}}}}\n{malformed}\n"
    );

    let result = ingest_memory::with_test_ledger(ledger, process_bulk(&engine, None, &body)).await;
    assert!(result.errors);
    assert_eq!(
        result
            .items
            .iter()
            .map(|item| item.status)
            .collect::<Vec<_>>(),
        [201, 201, 400]
    );

    // Malformed lines are rejected before an Arc-backed semantic batch owner
    // exists. Thus the exact owner formula is admitted source-line bytes.
    let expected_raw = valid_derived.len() + valid_supplied.len();
    assert_eq!(
        ledger
            .gauge(Category::RawSource, Measurement::SerializedSize)
            .peak,
        expected_raw as u64
    );
    let supplied: Value = serde_json::from_str(valid_supplied).unwrap();
    let vector_fields = HashSet::from(["content_vector".to_string()]);
    let supplied_vector = ingest_memory::vector_value_capacity(&supplied, &vector_fields) as u64;
    assert!(
        ledger
            .gauge(Category::ParsedVector, Measurement::ExactCapacity)
            .peak
            >= supplied_vector
    );
    assert!(
        ledger
            .gauge(Category::PreparedVector, Measurement::ExactCapacity)
            .peak
            >= supplied_vector
    );
    assert_eq!(semantic_current(ledger), 0);
    assert_eq!(ledger.accounting_errors(), 0);
    assert_eq!(engine.get_index(name).unwrap().live_doc_count(), 2);
}

#[tokio::test]
async fn injected_embedding_and_publication_failures_release_exact_owners() {
    for (suffix, injected, expected_status) in [
        ("embed", SemanticLifecycleFault::Embedding, 400),
        ("publish", SemanticLifecycleFault::Publication, 400),
    ] {
        let name = format!("bounded-failure-{suffix}");
        let (_dir, engine) = engine_with_index(&name, true);
        let ledger: &'static Ledger = Box::leak(Box::new(Ledger::new()));
        let hook: SemanticLifecycleHook = Arc::new(move |_, point| match (injected, point) {
            (SemanticLifecycleFault::Embedding, SemanticLifecyclePoint::EmbeddingInside) => {
                injected
            }
            (SemanticLifecycleFault::Publication, SemanticLifecyclePoint::PublicationInside) => {
                injected
            }
            _ => SemanticLifecycleFault::None,
        });
        let source = r#"{"content":"fault cleanup","content_vector":[0.1,0.2]}"#;
        let body = format!("{{\"index\":{{\"_index\":\"{name}\",\"_id\":\"one\"}}}}\n{source}\n");
        let result = ingest_memory::with_test_ledger(
            ledger,
            with_semantic_lifecycle_hook(&name, hook, process_bulk(&engine, None, &body)),
        )
        .await;

        assert!(result.errors);
        assert_eq!(result.items[0].status, expected_status);
        assert_eq!(semantic_current(ledger), 0, "{suffix} leaked an owner");
        assert_eq!(ledger.accounting_errors(), 0);
        assert_eq!(engine.get_index(&name).unwrap().live_doc_count(), 0);
    }
}

#[tokio::test]
async fn overwrite_flush_and_merge_churn_returns_to_a_bounded_baseline() {
    let name = "bounded-overwrite";
    let (_dir, engine) = engine_with_index(name, false);
    let index = engine.get_index(name).unwrap();
    let mut cycle_high_water = Vec::new();

    for cycle in 0..3 {
        for revision in 0..12 {
            index
                .index_document(
                    Some("same-id".into()),
                    json!({
                        "content": format!("fixed-shape revision {revision:02}"),
                        "cycle": cycle,
                        "rank": revision,
                    }),
                )
                .await
                .unwrap();
        }
        assert_eq!(index.live_doc_count(), 1);
        let before_flush = index.memtable_bytes();
        assert!(before_flush > 0);
        cycle_high_water.push(before_flush);

        index.flush().await.unwrap();
        assert_eq!(index.memtable_bytes(), 0);
        assert_eq!(index.live_doc_count(), 1);

        // Exercise segment hydration and query-cache replacement after every
        // publication. Correctness is checked on each generation; retained
        // cache bytes are not asserted here because those logical categories
        // are deliberately marked unavailable until their owners are wired.
        let result = index.search(&SearchRequest::default()).await.unwrap();
        assert_eq!(result.total.value, 1);
        let cached = index.search(&SearchRequest::default()).await.unwrap();
        assert_eq!(cached.total.value, 1);
    }

    // Same number and shape of writes. Sequence metadata may change, but
    // retained memtable growth must not accumulate across flush boundaries.
    let first = cycle_high_water[0];
    assert!(
        cycle_high_water
            .iter()
            .all(|bytes| bytes.abs_diff(first) <= 128),
        "same-shape overwrite cycles grew across flushes: {cycle_high_water:?}"
    );

    index.force_merge(1).await.unwrap();
    assert_eq!(index.memtable_bytes(), 0);
    assert_eq!(index.live_doc_count(), 1);
}

/// #1940: a small semantic_text record must not retain one `serde_json::Value`
/// per embedding component. 32 docs at 384-d is enough to prove the per-doc
/// bound; the old tree was `384 * size_of::<Value>()` before the rest of the
/// object. Exact kNN after flush checks the packed form still scores, and
/// `_source: true` / GET still return a number array.
#[tokio::test]
async fn semantic_log_records_retain_packed_vectors_not_value_arrays() {
    let name = "packed-semantic-rss";
    let dir = TempDir::new().expect("temp data directory");
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    let engine = Engine::new(config).expect("engine");
    let mut schema = Schema::empty();
    let mut content = FieldConfig::new("content", FieldType::Text);
    content.options.dimensions = Some(384);
    content.embedding = Some(EmbeddingConfig {
        endpoint: None,
        model: None,
        target_field: Some("content_vector".into()),
    });
    schema.fields.push(content);
    engine.create_index(name, schema).expect("create index");
    engine
        .put_index_mapping(
            name,
            json!({"properties": {"content": {"type": "semantic_text"}}}),
        )
        .unwrap();

    let index = engine.get_index(name).unwrap();
    const N: usize = 32;
    for i in 0..N {
        index
            .index_document(
                Some(format!("d{i}")),
                json!({"content": format!("segment queue header field flush buffer {i}")}),
            )
            .await
            .unwrap();
    }

    let heaps = index.memtable_source_heaps_for_test();
    assert_eq!(heaps.len(), N);
    let expanded_floor = 384 * std::mem::size_of::<Value>();
    let worst = heaps.iter().copied().max().unwrap_or(0);
    assert!(
        worst < expanded_floor,
        "retained source heap {worst} bytes is at least one expanded 384-d Value array ({expanded_floor})"
    );
    // Packed hex of 384 f32s is 9 + 3072 bytes, plus the short log object.
    assert!(
        worst < 8 * 1024,
        "retained source heap {worst} bytes exceeds 8 KB"
    );

    index.flush().await.unwrap();
    assert_eq!(index.memtable_bytes(), 0);

    let query_vector =
        xerj_ai::local::local_embed("segment queue header field flush buffer 0", 384);
    let request = xerj_query::parse_request(&json!({
        "query": {
            "knn": {
                "field": "content_vector",
                "query_vector": query_vector,
                "k": 1,
                "num_candidates": N,
                "boost": 1.0
            }
        },
        "size": 1,
        "_source": true
    }))
    .unwrap();
    let result = index.search(&request).await.unwrap();
    assert_eq!(result.hits.len(), 1, "exact kNN must still answer");
    assert_eq!(result.hits[0].id, "d0");
    let returned = result.hits[0]
        .source
        .get("content_vector")
        .and_then(Value::as_array)
        .expect("search _source returns the embedding as a number array");
    assert_eq!(returned.len(), 384);

    let fetched = index.get_document("d0").await.unwrap().unwrap();
    assert_eq!(fetched["content_vector"].as_array().unwrap().len(), 384);
}
