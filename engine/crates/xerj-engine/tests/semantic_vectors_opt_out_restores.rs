//! #1122 / #2433 review: the documented downgrade path. Rows written with
//! the default (derived vectors stripped from the stored `_source`) must get
//! their vectors back on disk when the process runs with
//! `XERJ_STORE_SEMANTIC_VECTORS=1` and the index is force-merged, so a binary
//! that cannot rebuild them still finds them.
//!
//! Its own test binary: the opt-out is process-wide.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::Engine;
use xerj_query::parse_request;
use xerj_storage::{SectionType, SegmentReader};

const INDEX: &str = "sem-optout";
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

/// Count columnar stored sections; assert every one is stripped (`want_stripped`)
/// or stores full vectors, as seen by the non-rehydrating decoder.
fn check_segments(dir: &TempDir, want_stripped: bool) -> usize {
    let mut files = Vec::new();
    seg_files(dir.path(), &mut files);
    let mut columnar = 0;
    for path in files {
        let reader = SegmentReader::open(&path).unwrap();
        let Some(raw) = reader.section(SectionType::Stored).unwrap() else {
            continue;
        };
        let raw_text = String::from_utf8(
            xerj_storage::stored_codec::decode_stored_preserving_derived(raw).unwrap(),
        )
        .unwrap();
        if xerj_storage::stored_codec::stored_v2_source_column_names(raw)
            .unwrap()
            .is_some()
        {
            columnar += 1;
        }
        if want_stripped {
            if xerj_storage::stored_codec::stored_v2_source_column_names(raw)
                .unwrap()
                .is_some()
            {
                assert!(
                    !raw_text.contains("\"content_vector\":"),
                    "{path:?} stores vectors"
                );
                assert!(raw_text.contains(r#""derived":"lexical-v1""#));
            }
        } else {
            assert!(
                !raw_text.contains(r#""derived""#),
                "{path:?} still has stripped rows after the opt-out merge"
            );
            let docs: Vec<Value> = serde_json::from_str(&raw_text).unwrap();
            for doc in &docs {
                let source = &doc["_source"];
                assert!(
                    source.get("content_vector").is_some(),
                    "{path:?}: doc {} has no stored vector",
                    doc["_id"]
                );
            }
            assert!(raw_text.contains("\"content_vector_chunks\":"));
        }
    }
    columnar
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

#[tokio::test]
async fn opt_out_force_merge_restores_stripped_vectors_on_disk() {
    xerj_storage::stored_codec::derived_vectors::set_store_vectors_opt_out(false);
    let dir = TempDir::new().unwrap();
    let probe = [0usize, 1, 10, 137, 999, 1500];
    let engine = make_engine(&dir);
    engine.create_index(INDEX, schema()).unwrap();
    let idx = engine.get_index(INDEX).unwrap();

    bulk(&engine, 0..1000).await;
    idx.flush().await.unwrap();
    idx.force_merge(1).await.unwrap();
    assert_eq!(idx.store_snapshot().segments.len(), 1);
    assert!(
        check_segments(&dir, true) > 0,
        "default write is not columnar"
    );
    let before_docs = snapshot(&idx, &probe[..5]).await;
    let before_hits = semantic_hits(&idx).await;
    assert!(!before_hits.is_empty());

    // "Restart with XERJ_STORE_SEMANTIC_VECTORS=1", then the documented
    // `_forcemerge?max_num_segments=1` on an index that is already one
    // segment: the stripped segment must still be rewritten.
    xerj_storage::stored_codec::derived_vectors::set_store_vectors_opt_out(true);
    assert!(
        idx.force_merge(1).await.unwrap() > 0,
        "single segment not rewritten"
    );
    assert_eq!(idx.store_snapshot().segments.len(), 1);
    assert!(
        check_segments(&dir, false) > 0,
        "rewritten section is not columnar"
    );
    assert_eq!(
        snapshot(&idx, &probe[..5]).await,
        before_docs,
        "rewrite changed a document"
    );
    let hits = |h: &[(String, f32, Value)]| {
        h.iter()
            .map(|h| (h.0.clone(), h.2.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(hits(&semantic_hits(&idx).await), hits(&before_hits));
    // Nothing left to restore: a second forcemerge is a no-op.
    assert_eq!(idx.force_merge(1).await.unwrap(), 0);

    // New docs under the opt-out, merged with the restored segment.
    bulk(&engine, 1000..1600).await;
    idx.flush().await.unwrap();
    let before_docs = snapshot(&idx, &probe).await;
    idx.force_merge(1).await.unwrap();
    assert!(
        check_segments(&dir, false) > 0,
        "merged section is not columnar"
    );
    assert_eq!(
        snapshot(&idx, &probe).await,
        before_docs,
        "merge changed a document"
    );
    drop(idx);
    drop(engine);

    // Reopened (still opted out): the restored vectors are what is read.
    let reopened = make_engine(&dir);
    let idx = reopened.get_index(INDEX).unwrap();
    assert_eq!(
        snapshot(&idx, &probe).await,
        before_docs,
        "reopen changed a document"
    );
    check_segments(&dir, false);
}
