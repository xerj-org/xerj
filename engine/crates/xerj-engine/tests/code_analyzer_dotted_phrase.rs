//! Issue #3285: `match_phrase` on an autoindexed code body found nothing for
//! a dotted identifier (`request.client.host`) that a plain `match` found.
//!
//! The `code` analyzer (the one `xerj autoindex --code-analyzer code` puts
//! on a dataset as the index default) keeps an identifier whole and stacks
//! its sub-words on the SAME position: `request.client.host` indexes
//! `request`, `client`, `host` and `request.client.host` all at one
//! position, at index time and at query time alike. The phrase query then
//! flattened the query-side stack into a four-term phrase that asked for
//! those terms one after another, which no document holds.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::{Engine, Index};
use xerj_fts::analyzer::AnalyzerRegistry;
use xerj_query::parse_request;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

async fn ids(idx: &Index, q: &Value) -> BTreeSet<String> {
    let req = parse_request(&json!({ "query": q, "size": 50 })).expect("parse_request");
    idx.search(&req)
        .await
        .unwrap()
        .hits
        .iter()
        .map(|h| h.id.clone())
        .collect()
}

/// The create body autoindex sends for a code dataset (see
/// `xerj_autoindex::dataset_create_body` under `--code-analyzer code`).
fn code_settings() -> Value {
    json!({"analysis": {"analyzer": {"default": {"type": "code"}}}})
}

/// `_analyze` contract: the code analyzer gives the identifier and its
/// sub-words one shared position, and the same stream whether the text is
/// a document body or a query.
#[test]
fn code_analyzer_positions_are_the_same_at_index_and_query_time() {
    let registry = AnalyzerRegistry::default();
    let code = registry.get_analyzer("code").expect("code analyzer");
    let body: Vec<(String, u32)> = code
        .analyze("x = request.client.host")
        .into_iter()
        .map(|t| (t.text, t.position))
        .collect();
    let query: Vec<(String, u32)> = code
        .analyze("request.client.host")
        .into_iter()
        .map(|t| (t.text, t.position))
        .collect();
    for term in ["request", "client", "host", "request.client.host"] {
        assert!(
            body.contains(&(term.to_string(), 1)),
            "index time: {term}@1 in {body:?}"
        );
        assert!(
            query.contains(&(term.to_string(), 0)),
            "query time: {term}@0 in {query:?}"
        );
    }
    // Same stream modulo the leading `x`: one position, four terms.
    let shifted: Vec<(String, u32)> = body
        .iter()
        .filter(|(_, p)| *p == 1)
        .map(|(t, p)| (t.clone(), p - 1))
        .collect();
    assert_eq!(shifted, query);
}

async fn seed(engine: &Engine) -> std::sync::Arc<Index> {
    engine
        .create_index_with_settings("code_body", Schema::empty(), code_settings())
        .unwrap();
    let idx = engine.get_index("code_body").unwrap();
    for (id, body) in [
        ("hit", "x = request.client.host"),
        ("other", "y = response.server.port"),
        ("near", "host = client"),
    ] {
        idx.index_document(Some(id.into()), json!({"body": body}))
            .await
            .unwrap();
    }
    idx
}

async fn assert_cases(idx: &Index, state: &str) {
    for (q, want) in [
        (
            json!({"match_phrase": {"body": "request.client.host"}}),
            vec!["hit"],
        ),
        (
            json!({"match_phrase": {"body": "client.host"}}),
            vec!["hit"],
        ),
        (
            json!({"match_phrase": {"body": "x = request.client.host"}}),
            vec!["hit"],
        ),
        (
            json!({"match_phrase": {"body": "response.server.port"}}),
            vec!["other"],
        ),
        (json!({"match_phrase": {"body": "request.server"}}), vec![]),
        (
            json!({"match": {"body": {"query": "request.client.host", "operator": "and"}}}),
            vec!["hit"],
        ),
    ] {
        let want: BTreeSet<String> = want.into_iter().map(String::from).collect();
        assert_eq!(ids(idx, &q).await, want, "{state}: {q}");
    }
}

#[tokio::test]
async fn match_phrase_finds_a_dotted_identifier_under_the_code_analyzer() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine).await;
    idx.flush().await.unwrap();
    assert_cases(&idx, "segment").await;
}

#[tokio::test]
async fn match_phrase_on_a_dotted_identifier_is_flush_invariant() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine).await;
    assert_cases(&idx, "memtable").await;
    idx.flush().await.unwrap();
    assert_cases(&idx, "segment").await;
}
