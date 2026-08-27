//! Issue #848: ES `prefix_length` on a fuzzy/match query — the first N
//! characters of the query and each candidate term must match EXACTLY (not
//! subject to edits) before Levenshtein applies. XERJ ignored `prefix_length`
//! entirely (grep = 0 hits across query/engine/fts), so a `prefix_length: N`
//! (N>0) was silently dropped and terms differing in their first N chars were
//! wrongly fuzzy-matched — MORE hits than ES.
//!
//! Covered here for BOTH a keyword field (memtable doc-scan + segment
//! `KeywordFuzzy`) and a text field (memtable doc-scan + segment FST
//! `expand_fuzzy`), each asserted flush-invariant: every query is answered
//! buffered AND flushed, and the two must agree.

use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::Engine;
use xerj_query::parse_request;

async fn hits(engine: &Engine, q: &serde_json::Value) -> u64 {
    let idx = engine.get_index("docs").expect("get index");
    let req = parse_request(&json!({ "query": q, "size": 0 })).expect("parse_request");
    idx.search(&req).await.expect("search").total.value
}

#[tokio::test]
async fn fuzzy_prefix_length_requires_exact_leading_chars() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config).expect("engine");

    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("name", FieldType::Keyword));
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    engine.create_index("docs", schema).expect("create");
    let idx = engine.get_index("docs").expect("get");
    idx.index_document(
        Some("d0".to_string()),
        json!({ "name": "axble", "body": "axble" }),
    )
    .await
    .expect("index");

    // fuzziness 1: "exble" is 1 edit from "axble" (leading char differs);
    // "axcle" is 1 edit (a middle char differs, prefix "ax" intact).
    let kw = |v: &str, pl: u64| json!({ "fuzzy": { "name": { "value": v, "fuzziness": 1, "prefix_length": pl } } });
    let tx = |v: &str, pl: u64| json!({ "fuzzy": { "body": { "value": v, "fuzziness": 1, "prefix_length": pl } } });

    // (query, expected_hits, label). Keyword field exercises the memtable
    // doc-scan (buffered) and the columnar `KeywordFuzzy` (flushed); text field
    // exercises the doc-scan (buffered) and the FST `expand_fuzzy` (flushed).
    let cases = [
        (
            kw("exble", 0),
            1,
            "kw prefix_length 0 imposes no constraint",
        ),
        (kw("exble", 1), 0, "kw leading char must match exactly"),
        (kw("axcle", 2), 1, "kw in-prefix edit still matches"),
        (
            tx("exble", 0),
            1,
            "tx prefix_length 0 imposes no constraint",
        ),
        (tx("exble", 1), 0, "tx leading char must match exactly"),
        (tx("axcle", 2), 1, "tx in-prefix edit still matches"),
    ];

    // Answer every case BUFFERED first (memtable path), then flush ONCE and
    // answer every case again (segment path) — so both paths are exercised for
    // every query, not just the first.
    let mut pre = Vec::new();
    for (q, _, _) in &cases {
        pre.push(hits(&engine, q).await);
    }
    idx.refresh().await.expect("refresh");
    for (i, (q, expected, label)) in cases.iter().enumerate() {
        let post = hits(&engine, q).await;
        assert_eq!(
            pre[i], post,
            "#848: fuzzy prefix_length must be flush-invariant — {label}"
        );
        assert_eq!(post, *expected, "#848: {label}");
    }
}
