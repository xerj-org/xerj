//! #1812: `regexp` on a `keyword` field must honor `case_insensitive`.
//!
//! ES matches `regexp` against the RAW keyword value (case-SENSITIVE by
//! default); `case_insensitive: true` folds both sides instead.  XERJ's
//! `Regexp` AST node dropped the flag entirely, so
//! `regexp:{k:{value:"HEL+O.*",case_insensitive:true}}` on `"Hello World"`
//! returned 0 (wrong — ES returns the doc).
//!
//! Both the buffered (memtable) and flushed (segment) paths are asserted
//! pre- and post-`refresh`: the segment path answers via a dictionary
//! expansion pre-filter (`regexp_segment_expansion` /
//! `build_regexp_prefilter_cached`) that compiles the pattern itself, so a
//! fix only in the doc-scan arm would split-brain across a flush.

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
/// 0 docs.  ES returns 2 (both keyword values full-match "hel+o.*", folded).
#[tokio::test]
async fn regexp_case_insensitive_true_folds_both_paths() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = setup_docs(&engine).await;

    let q = json!({ "regexp": { "k": { "value": "HEL+O.*", "case_insensitive": true } } });

    // Buffered (memtable) path.
    let pre = hits(&idx, q.clone()).await;
    // Flushed (segment) path — exercised through the dictionary expansion.
    idx.refresh().await.expect("refresh");
    let post = hits(&idx, q).await;

    assert_eq!(
        pre, 2,
        "memtable: case_insensitive regexp must match both docs"
    );
    assert_eq!(
        pre, post,
        "flush-invariant: memtable={pre} vs segment={post} disagree"
    );
    assert_eq!(
        post, 2,
        "segment: case_insensitive regexp must match both docs"
    );
}

/// Explicit `case_insensitive: false` stays ES-correct case-SENSITIVE.
#[tokio::test]
async fn regexp_case_insensitive_false_is_case_sensitive() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = setup_docs(&engine).await;

    // "HEL+O.*" full-matches neither value byte-wise.
    let q_upper = json!({ "regexp": { "k": { "value": "HEL+O.*", "case_insensitive": false } } });
    // "hel+o.*" full-matches only the lowercase doc byte-wise.
    let q_lower = json!({ "regexp": { "k": { "value": "hel+o.*", "case_insensitive": false } } });

    let pre_upper = hits(&idx, q_upper.clone()).await;
    let pre_lower = hits(&idx, q_lower.clone()).await;
    idx.refresh().await.expect("refresh");
    let post_upper = hits(&idx, q_upper).await;
    let post_lower = hits(&idx, q_lower).await;

    assert_eq!(
        pre_upper, 0,
        "memtable: HEL+O.* must not match capitalized values"
    );
    assert_eq!(
        pre_lower, 1,
        "memtable: hel+o.* must match only the lowercase doc"
    );
    assert_eq!(pre_upper, post_upper, "flush-invariant (upper)");
    assert_eq!(pre_lower, post_lower, "flush-invariant (lower)");
    assert_eq!(
        post_upper, 0,
        "segment: HEL+O.* must not match capitalized values"
    );
    assert_eq!(
        post_lower, 1,
        "segment: hel+o.* must match only the lowercase doc"
    );
}

/// Omitted parameter defaults to `false` (shorthand and expanded forms).
#[tokio::test]
async fn regexp_omitted_case_insensitive_defaults_false() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = setup_docs(&engine).await;

    let q_upper = json!({ "regexp": { "k": "HEL+O.*" } });
    let q_lower = json!({ "regexp": { "k": "hel+o.*" } });

    let pre_upper = hits(&idx, q_upper.clone()).await;
    let pre_lower = hits(&idx, q_lower.clone()).await;
    idx.refresh().await.expect("refresh");
    let post_upper = hits(&idx, q_upper).await;
    let post_lower = hits(&idx, q_lower).await;

    assert_eq!(pre_upper, 0, "shorthand HEL+O.* defaults to case-sensitive");
    assert_eq!(
        pre_lower, 1,
        "shorthand hel+o.* matches only the lowercase doc"
    );
    assert_eq!(pre_upper, post_upper, "flush-invariant (upper)");
    assert_eq!(pre_lower, post_lower, "flush-invariant (lower)");
}

/// The flag must not change matching for values it shouldn't reach: a
/// case-insensitive pattern that only matches via folding must NOT match a
/// non-string value path, and an invalid pattern still matches nothing.
#[tokio::test]
async fn regexp_case_insensitive_invalid_pattern_matches_nothing() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = setup_docs(&engine).await;

    let q = json!({ "regexp": { "k": { "value": "([", "case_insensitive": true } } });
    assert_eq!(
        hits(&idx, q.clone()).await,
        0,
        "invalid pattern matches nothing (memtable)"
    );
    idx.refresh().await.expect("refresh");
    assert_eq!(
        hits(&idx, q).await,
        0,
        "invalid pattern matches nothing (segment)"
    );
}
