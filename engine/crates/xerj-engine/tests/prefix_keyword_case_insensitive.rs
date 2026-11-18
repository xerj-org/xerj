//! #1812: `prefix` on a `keyword` field must honor `case_insensitive`.
//!
//! ES matches `prefix` against the RAW keyword value (case-SENSITIVE by
//! default); `case_insensitive: true` folds both sides instead.  XERJ's
//! `Prefix` AST node dropped the flag entirely, so
//! `prefix:{k:{value:"HELLO",case_insensitive:true}}` on `"Hello World"`
//! returned 0 (wrong — ES returns the doc).
//!
//! Both the buffered (memtable) and flushed (segment) paths are asserted
//! pre- and post-`refresh`, mirroring `prefix_keyword_case_flush_parity.rs`:
//! a fix applied to only one path split-brains across a background flush,
//! which this repo treats as a regression class of its own.

use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::{Engine, Index};
use xerj_query::parse_request;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

/// Index two keyword docs (capitalized + lowercase) and return the index.
async fn setup_docs(engine: &Engine) -> Arc<Index> {
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("k", FieldType::Keyword));
    engine.create_index("docs", schema).expect("create");

    let idx = engine.get_index("docs").expect("get");
    idx.index_document(Some("d1".to_string()), json!({ "k": "Hello World" }))
        .await
        .expect("index");
    idx.index_document(Some("d2".to_string()), json!({ "k": "hello there" }))
        .await
        .expect("index");
    idx
}

async fn hits(idx: &Index, query: serde_json::Value) -> u64 {
    let req = parse_request(&json!({ "query": query, "size": 0 })).expect("parse_request");
    idx.search(&req).await.expect("search").total.value
}

/// FAIL-BEFORE (#1812): the flag was dropped at parse time, so this matched
/// 0 docs.  ES returns 2 (both keyword values start with "hello", folded).
#[tokio::test]
async fn prefix_case_insensitive_true_folds_both_paths() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = setup_docs(&engine).await;

    let q = json!({ "prefix": { "k": { "value": "HELLO", "case_insensitive": true } } });

    // Buffered (memtable) path.
    let pre = hits(&idx, q.clone()).await;
    // Flushed (segment) path.
    idx.refresh().await.expect("refresh");
    let post = hits(&idx, q).await;

    assert_eq!(
        pre, 2,
        "memtable: case_insensitive prefix must match both docs"
    );
    assert_eq!(
        pre, post,
        "flush-invariant: memtable={pre} vs segment={post} disagree"
    );
    assert_eq!(
        post, 2,
        "segment: case_insensitive prefix must match both docs"
    );
}

/// Explicit `case_insensitive: false` stays ES-correct case-SENSITIVE.
#[tokio::test]
async fn prefix_case_insensitive_false_is_case_sensitive() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = setup_docs(&engine).await;

    // "HELLO" prefixes neither "Hello World" nor "hello there" byte-wise.
    let q_upper = json!({ "prefix": { "k": { "value": "HELLO", "case_insensitive": false } } });
    // "hello" prefixes only the lowercase doc byte-wise.
    let q_lower = json!({ "prefix": { "k": { "value": "hello", "case_insensitive": false } } });

    let pre_upper = hits(&idx, q_upper.clone()).await;
    let pre_lower = hits(&idx, q_lower.clone()).await;
    idx.refresh().await.expect("refresh");
    let post_upper = hits(&idx, q_upper).await;
    let post_lower = hits(&idx, q_lower).await;

    assert_eq!(
        pre_upper, 0,
        "memtable: HELLO must not match capitalized values"
    );
    assert_eq!(
        pre_lower, 1,
        "memtable: hello must match only the lowercase doc"
    );
    assert_eq!(pre_upper, post_upper, "flush-invariant (upper)");
    assert_eq!(pre_lower, post_lower, "flush-invariant (lower)");
    assert_eq!(
        post_upper, 0,
        "segment: HELLO must not match capitalized values"
    );
    assert_eq!(
        post_lower, 1,
        "segment: hello must match only the lowercase doc"
    );
}

/// Omitted parameter defaults to `false` (shorthand and expanded forms).
#[tokio::test]
async fn prefix_omitted_case_insensitive_defaults_false() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = setup_docs(&engine).await;

    let q_upper = json!({ "prefix": { "k": "HELLO" } });
    let q_lower = json!({ "prefix": { "k": "hello" } });

    let pre_upper = hits(&idx, q_upper.clone()).await;
    let pre_lower = hits(&idx, q_lower.clone()).await;
    idx.refresh().await.expect("refresh");
    let post_upper = hits(&idx, q_upper).await;
    let post_lower = hits(&idx, q_lower).await;

    assert_eq!(pre_upper, 0, "shorthand HELLO defaults to case-sensitive");
    assert_eq!(
        pre_lower, 1,
        "shorthand hello matches only the lowercase doc"
    );
    assert_eq!(pre_upper, post_upper, "flush-invariant (upper)");
    assert_eq!(pre_lower, post_lower, "flush-invariant (lower)");
}

/// TEXT-field semantics are UNCHANGED by the flag: prefix runs against the
/// analyzed (lowercased) token stream, so a lowercase pattern matches and an
/// uppercase one does not — with or without `case_insensitive: true`.
#[tokio::test]
async fn prefix_case_insensitive_does_not_change_text_semantics() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    let mut schema = Schema::empty();
    schema.fields.push(FieldConfig::new("t", FieldType::Text));
    engine.create_index("docs", schema).expect("create");
    let idx = engine.get_index("docs").expect("get");
    idx.index_document(Some("d1".to_string()), json!({ "t": "Hello World" }))
        .await
        .expect("index");
    idx.refresh().await.expect("refresh");

    let q_lower = json!({ "prefix": { "t": { "value": "hel", "case_insensitive": false } } });
    let q_lower_ci = json!({ "prefix": { "t": { "value": "hel", "case_insensitive": true } } });
    let q_upper = json!({ "prefix": { "t": { "value": "HEL", "case_insensitive": true } } });

    assert_eq!(
        hits(&idx, q_lower).await,
        1,
        "text: lowercase prefix matches the analyzed token"
    );
    assert_eq!(
        hits(&idx, q_lower_ci).await,
        1,
        "text: case_insensitive must not change analyzed-token matching"
    );
    assert_eq!(
        hits(&idx, q_upper).await,
        0,
        "text: uppercase prefix does not match the lowercased token (ES analyzes the doc, not the pattern)"
    );
}
