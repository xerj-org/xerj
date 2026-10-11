//! Regression tests for issue #1280: a `match` / `match_phrase` that the
//! memtable answers by scanning documents must tokenize with the analyzer the
//! segment uses, so the hit set does not change at `flush()`.
//!
//! Pre-fix the scan split text on `!char::is_alphanumeric()`. That is not the
//! standard tokenizer (UAX#29): a run of Han ideographs stayed ONE token where
//! the analyzer emits one per character, so a Chinese `match` inside a
//! filtered `bool` (which is scan-routed) found nothing until `_refresh`; and
//! `foo_bar`, `don't` and `3.14` were split, so `foo`, `don` and `3` matched
//! before the flush and not after. `match_phrase` already analyzed the field
//! side (#230) but still split the query side.
//!
//! Each test runs every case against the memtable, flushes once, re-runs it
//! against the segment, and asserts the expected hit set in both states.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::{Engine, Index};
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

async fn assert_flush_parity(idx: &std::sync::Arc<Index>, cases: &[(Value, &[&str], &str)]) {
    let expected: Vec<BTreeSet<String>> = cases
        .iter()
        .map(|(_, exp, _)| exp.iter().map(|s| s.to_string()).collect())
        .collect();
    for ((q, _, label), exp) in cases.iter().zip(&expected) {
        let pre = ids(idx, q).await;
        assert_eq!(
            &pre, exp,
            "{label}: PRE-flush (memtable) hit set wrong for {q}"
        );
    }
    idx.flush().await.unwrap();
    for ((q, _, label), exp) in cases.iter().zip(&expected) {
        let post = ids(idx, q).await;
        assert_eq!(
            &post, exp,
            "{label}: POST-flush (segment) hit set wrong for {q}"
        );
    }
}

async fn seed(engine: &Engine, name: &str) -> std::sync::Arc<Index> {
    engine.create_index(name, Schema::empty()).unwrap();
    let idx = engine.get_index(name).unwrap();
    for (id, text) in [
        ("zh", "普通报表保留七日。"),
        ("ascii", "ERR_SCOPE_MISMATCH means outside scope."),
        ("us", "the foo_bar baz"),
        ("apos", "we don't stop now"),
        ("dot", "notes 3.14 here"),
    ] {
        idx.index_document(Some(id.into()), json!({"text": text, "scope": "synthetic"}))
            .await
            .unwrap();
    }
    idx
}

/// `bool` with a `filter` keeps the clause out of the single-clause unwrap,
/// so the memtable answers it by scanning documents.
fn filtered(clause: Value) -> Value {
    json!({"bool": {"must": [clause], "filter": [{"term": {"scope": "synthetic"}}]}})
}

/// Issue #1280's reproducer and its narrowing: CJK text through the scan.
#[tokio::test]
async fn cjk_match_hit_set_stable_across_flush() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "cjk_parity").await;

    assert_flush_parity(
        &idx,
        &[
            (
                filtered(
                    json!({"match": {"text": {"query": "普通报表保留多久？", "operator": "or"}}}),
                ),
                &["zh"],
                "issue #1280 filtered bool",
            ),
            (
                filtered(json!({"match": {"text": "报"}})),
                &["zh"],
                "one ideograph",
            ),
            (
                json!({"match": {"text": {"query": "普通报表", "operator": "and"}}}),
                &["zh"],
                "operator:and (scan-routed without a bool)",
            ),
            (
                json!({"match": {"text": {"query": "普通报表多久", "operator": "and"}}}),
                &[],
                "operator:and with an absent ideograph",
            ),
            (
                filtered(json!({"match_phrase": {"text": "报表"}})),
                &["zh"],
                "phrase, adjacent",
            ),
            (
                filtered(json!({"match_phrase": {"text": "报保"}})),
                &[],
                "phrase, not adjacent",
            ),
            (
                filtered(json!({"match": {"text": "ERR_SCOPE_MISMATCH"}})),
                &["ascii"],
                "ASCII control",
            ),
        ],
    )
    .await;
}

/// The same split also over-matched ASCII: UAX#29 keeps `_`, `'` and an
/// intra-number `.` inside one word.
#[tokio::test]
async fn word_boundary_match_hit_set_stable_across_flush() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "uax29_parity").await;

    assert_flush_parity(
        &idx,
        &[
            (
                filtered(json!({"match": {"text": "foo"}})),
                &[],
                "foo vs foo_bar",
            ),
            (
                filtered(json!({"match": {"text": "foo_bar"}})),
                &["us"],
                "foo_bar",
            ),
            (
                filtered(json!({"match": {"text": "don"}})),
                &[],
                "don vs don't",
            ),
            (
                filtered(json!({"match": {"text": "don't"}})),
                &["apos"],
                "don't",
            ),
            (
                filtered(json!({"match": {"text": "release 3"}})),
                &[],
                "3 vs 3.14",
            ),
            (
                filtered(json!({"match": {"text": "3.14"}})),
                &["dot"],
                "3.14",
            ),
            (
                filtered(json!({"match_phrase": {"text": "notes 3.14"}})),
                &["dot"],
                "phrase 3.14",
            ),
        ],
    )
    .await;
}
