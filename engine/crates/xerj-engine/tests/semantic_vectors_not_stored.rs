//! #1122 / #2433: the vectors the built-in lexical embedder derives for a
//! `semantic_text` field are not persisted in the stored `_source`. The
//! segment keeps the text and passage metadata; every reader gets the
//! vectors rebuilt bit-identically, before and after flush, merge and
//! reopen.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::Engine;
use xerj_query::parse_request;
use xerj_storage::{SectionType, SegmentReader};

const INDEX: &str = "sem-derived";
const WORDS: &[&str] = &[
    "liquidity",
    "quarterly",
    "revenue",
    "storage",
    "vector",
    "merge",
    "segment",
    "harbor",
    "orchard",
    "glacier",
    "copper",
    "lantern",
    "meadow",
    "compiler",
    "kernel",
    "ledger",
];

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

fn schema() -> Schema {
    let mut schema = Schema::empty();
    let mut content = FieldConfig::new("content", FieldType::Text);
    content.options.dimensions = Some(32);
    content.options.similarity = Some("cosine".to_string());
    content.embedding = Some(xerj_common::types::EmbeddingConfig {
        endpoint: None,
        model: None,
        target_field: Some("content_vector".to_string()),
    });
    schema.fields.push(content);
    schema.fields.push(FieldConfig::new("n", FieldType::Long));
    schema
}

fn text(i: usize) -> String {
    let base: Vec<&str> = (0..12)
        .map(|k| WORDS[(i * 7 + k * 3) % WORDS.len()])
        .collect();
    let base = base.join(" ");
    if i.is_multiple_of(10) {
        // Several passages: exercises `<target>_chunks`.
        format!("doc {i} {}", format!("{base} ").repeat(20))
    } else {
        format!("doc {i} {base}")
    }
}

async fn bulk(engine: &Engine, range: std::ops::Range<usize>) {
    let mut body = String::new();
    for i in range {
        body.push_str(&format!(
            "{{\"index\":{{\"_index\":\"{INDEX}\",\"_id\":\"{i}\"}}}}\n"
        ));
        body.push_str(&serde_json::to_string(&json!({"content": text(i), "n": i})).unwrap());
        body.push('\n');
    }
    let result = xerj_engine::bulk::process_bulk(engine, None, &body).await;
    assert!(
        !result.errors,
        "{:?}",
        result.items.iter().find(|i| i.error.is_some())
    );
}

fn seg_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            seg_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "seg") {
            out.push(path);
        }
    }
}

/// Every columnar stored section on disk: vectors absent, marker present.
fn assert_segments_stripped(dir: &TempDir) {
    let mut files = Vec::new();
    seg_files(dir.path(), &mut files);
    assert!(!files.is_empty(), "no segment written");
    let mut columnar = 0;
    for path in files {
        let reader = SegmentReader::open(&path).unwrap();
        let Some(raw) = reader.section(SectionType::Stored).unwrap() else {
            continue;
        };
        // Small sections stay row-oriented LZ4 and keep their vectors.
        let Some(names) = xerj_storage::stored_codec::stored_v2_source_column_names(raw).unwrap()
        else {
            continue;
        };
        columnar += 1;
        assert!(
            names.iter().any(|n| n == "content_vector"),
            "virtual column listed"
        );
        let preserved = xerj_storage::stored_codec::decode_stored_preserving_derived(raw).unwrap();
        let preserved = String::from_utf8(preserved).unwrap();
        assert!(
            !preserved.contains("\"content_vector\":"),
            "{path:?} stores vectors"
        );
        assert!(!preserved.contains("\"content_vector_chunks\":"));
        assert!(preserved.contains(r#""derived":"lexical-v1""#));
        let full = xerj_storage::stored_codec::decode_stored(raw).unwrap();
        let full = String::from_utf8(full).unwrap();
        assert!(
            full.contains("\"content_vector\":"),
            "decode rebuilds vectors"
        );
        assert!(full.contains("\"content_vector_chunks\":"));
        assert!(
            !full.contains(r#""derived""#),
            "marker never reaches readers"
        );
    }
    assert!(columnar > 0, "no columnar stored section to check");
}

async fn snapshot(idx: &xerj_engine::Index, ids: &[usize]) -> Vec<Value> {
    let mut out = Vec::new();
    for id in ids {
        out.push(idx.get_document(&id.to_string()).await.unwrap().unwrap());
    }
    out
}

async fn semantic_hits(idx: &xerj_engine::Index) -> Vec<(String, f32, Value)> {
    let request = parse_request(&json!({
        "query": {"semantic": {"field": "content", "query": "quarterly liquidity ledger", "k": 10}},
        "size": 10,
        "_source": true
    }))
    .unwrap();
    let response = idx.search(&request).await.unwrap();
    response
        .hits
        .iter()
        .map(|hit| (hit.id.clone(), hit.score, hit.source.clone()))
        .collect()
}

async fn knn_top(idx: &xerj_engine::Index, vector: &Value) -> String {
    let request = parse_request(&json!({
        "query": {"knn": {"field": "content_vector", "query_vector": vector, "k": 3, "num_candidates": 50}},
        "size": 3
    }))
    .unwrap();
    idx.search(&request).await.unwrap().hits[0].id.clone()
}

fn assert_same_hits(got: &[(String, f32, Value)], want: &[(String, f32, Value)], step: &str) {
    let ids = |hits: &[(String, f32, Value)]| hits.iter().map(|h| h.0.clone()).collect::<Vec<_>>();
    assert_eq!(ids(got), ids(want), "{step} changed semantic ranking");
    for (g, w) in got.iter().zip(want) {
        assert!(
            (g.1 - w.1).abs() < 1e-4,
            "{step} changed a score: {g:?} vs {w:?}"
        );
        assert_eq!(g.2, w.2, "{step} changed a hit source");
    }
}

#[tokio::test]
async fn lexical_semantic_vectors_are_rebuilt_not_stored() {
    let dir = TempDir::new().unwrap();
    let probe = [0usize, 1, 10, 137, 250, 1999];
    let engine = make_engine(&dir);
    engine.create_index(INDEX, schema()).unwrap();
    let idx = engine.get_index(INDEX).unwrap();

    bulk(&engine, 0..1000).await;
    bulk(&engine, 1000..2000).await;
    let before_docs = snapshot(&idx, &probe).await;
    for doc in &before_docs {
        assert_eq!(doc["content_vector"].as_array().unwrap().len(), 32);
    }
    assert!(before_docs[0]["content_vector_chunks"].is_array());
    let before_hits = semantic_hits(&idx).await;
    assert!(!before_hits.is_empty());

    idx.flush().await.unwrap();
    assert_segments_stripped(&dir);
    assert_eq!(
        snapshot(&idx, &probe).await,
        before_docs,
        "flush changed a document"
    );
    assert_same_hits(&semantic_hits(&idx).await, &before_hits, "flush");
    assert_eq!(
        knn_top(&idx, &before_docs[3]["content_vector"]).await,
        "137"
    );

    bulk(&engine, 2000..2600).await;
    idx.flush().await.unwrap();
    idx.force_merge(1).await.unwrap();
    assert_segments_stripped(&dir);
    assert_eq!(
        snapshot(&idx, &probe).await,
        before_docs,
        "merge changed a document"
    );
    let merged_hits = semantic_hits(&idx).await;
    drop(idx);
    drop(engine);

    let reopened = make_engine(&dir);
    let idx = reopened.get_index(INDEX).unwrap();
    assert_eq!(
        snapshot(&idx, &probe).await,
        before_docs,
        "reopen changed a document"
    );
    assert_same_hits(&semantic_hits(&idx).await, &merged_hits, "reopen");
    assert_eq!(knn_top(&idx, &before_docs[1]["content_vector"]).await, "1");
}

#[test]
fn storage_and_query_agree_on_the_passage_metadata_prefix() {
    assert_eq!(
        xerj_storage::stored_codec::derived_vectors::PASSAGE_METADATA_PREFIX,
        xerj_query::executor::PASSAGE_METADATA_PREFIX
    );
}
