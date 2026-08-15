//! Regression test for issue #398: `prefix` / `wildcard` must not change
//! their answer at `_flush`.
//!
//! `doc_matches_query` answers both leaves for every document still in the
//! memtable, and for every field the FTS projection declines; a flushed
//! segment answers them from the analysed term dictionary. Pre-fix the scan
//! had no field type, so it folded case on BOTH sides and split the stored
//! string on non-alphanumerics as a stand-in for index-time analysis — a
//! different question from the one the term dictionary answers. Measured on
//! `921f9e0` with the probe that became this file (`title` mapped `text`,
//! `code` mapped `keyword`, both holding `HnswGraphBuilder.java`):
//!
//! | query | pre-flush | post-flush |
//! |---|---|---|
//! | `{"prefix":{"title":"Hnsw"}}`    | 1 | 0 |
//! | `{"wildcard":{"title":"java"}}`  | 1 | 0 |
//! | `{"wildcard":{"title":"Hnsw*"}}` | 1 | 0 |
//! | `{"prefix":{"code":"hnsw"}}`     | 1 | 0 |
//!
//! The post-flush answer is the correct one: ES does not analyse a
//! multi-term query, so a capitalised prefix against an analyser-lowercased
//! dictionary matches nothing, and a `keyword` term keeps its case.
//!
//! Every case below is asserted in THREE states — all documents in the
//! memtable, all in a segment, and the mixed state where one of each is live
//! at once — because a divergence in the mixed state is a single answer that
//! contradicts itself.
//!
//! NOT fixed here, and deliberately not asserted: a `keyword` ARRAY. The
//! indexing path joins `["Alpha","Beta"]` into ONE keyword term
//! `"Alpha Beta"` (`index.rs::extract_field_text`), so
//! `{"prefix":{"tags":"Beta"}}` is 1 pre-flush and 0 post-flush both on
//! `921f9e0` and here. Making the scan mirror the join would "fix" the
//! divergence by breaking the common case ES gets right (one term per
//! element); the join is the defect, and it lives in the WRITE path, which
//! is a different change.

use std::collections::BTreeSet;

use serde_json::{json, Value};
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

fn req(q: Value, size: u64) -> xerj_query::ast::SearchRequest {
    parse_request(&json!({ "query": q, "size": size })).expect("parse_request")
}

/// Hit ids, `hits.total`, and the `size: 0` total — the shape `_count` sends,
/// which takes the short-circuit count path (`try_shortcut_count`) and its own
/// memtable recount rather than the hit walk. That is a second stored-doc scan
/// and it diverged the same way, so it is asserted too.
async fn hits(idx: &Index, q: &Value) -> (BTreeSet<String>, u64, u64) {
    let res = idx.search(&req(q.clone(), 50)).await.unwrap();
    let counted = idx.search(&req(q.clone(), 0)).await.unwrap();
    (
        res.hits.iter().map(|h| h.id.clone()).collect(),
        res.total.value,
        counted.total.value,
    )
}

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

/// `d1` carries a CamelCase identifier with an intra-word `.`; the standard
/// analyzer keeps it as the single term `hnswgraphbuilder.java`, so it
/// separates "analysed token" from "alphanumeric split" as well as from
/// "raw stored string". `d2` is already lowercase, so the queries that
/// SHOULD match still have something to match — no expectation below is
/// vacuously empty.
async fn seed(
    engine: &Engine,
    name: &str,
    flush_between: bool,
    flush_after: bool,
) -> std::sync::Arc<Index> {
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("title", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("code", FieldType::Keyword));
    engine.create_index(name, schema).unwrap();
    let idx = engine.get_index(name).unwrap();
    idx.index_document(
        Some("d1".into()),
        json!({
            "title": "HnswGraphBuilder.java",
            "code": "HnswGraphBuilder.java",
            // Not in the mapping above: dynamic mapping registers it as
            // `text`, so it must answer like `title`, not like a field with
            // no type at all.
            "note": "HnswGraphBuilder.java"
        }),
    )
    .await
    .unwrap();
    if flush_between {
        idx.flush().await.unwrap();
    }
    idx.index_document(
        Some("d2".into()),
        json!({"title": "hnsw plain lowercase", "code": "hnsw-plain", "note": "hnsw plain"}),
    )
    .await
    .unwrap();
    if flush_after {
        idx.flush().await.unwrap();
    }
    idx
}

/// `(query, expected ids)` — the segment's answer, which is the one ES gives.
fn cases() -> Vec<(Value, Vec<&'static str>)> {
    vec![
        // ── text field: terms are the standard analyzer's tokens ─────────
        // Issue #398 row 1: uppercase pattern vs a lowercased dictionary.
        (json!({"prefix": {"title": "Hnsw"}}), vec![]),
        (json!({"prefix": {"title": "hnsw"}}), vec!["d1", "d2"]),
        // The analyzed term keeps the intra-word `.`, so a prefix may run
        // straight through it. The pre-fix scan split there and could not.
        (
            json!({"prefix": {"title": "hnswgraphbuilder."}}),
            vec!["d1"],
        ),
        // Issue #398 row 2: `java` is not a term of its own — the whole
        // identifier is ONE analyzed token.
        (json!({"wildcard": {"title": "java"}}), vec![]),
        (json!({"wildcard": {"title": "*.java"}}), vec!["d1"]),
        (json!({"wildcard": {"title": "hnsw*"}}), vec!["d1", "d2"]),
        (json!({"wildcard": {"title": "Hnsw*"}}), vec![]),
        (json!({"wildcard": {"title": "*graph*"}}), vec!["d1"]),
        // ── keyword field: ONE whole-value, case-preserved term ──────────
        // `prefix` on a keyword is case-SENSITIVE (ES default), so the
        // capitalised pattern is the one that matches here — the mirror
        // image of the text rows above, which is why a blanket
        // `case_insensitive` default cannot fix either.
        (json!({"prefix": {"code": "Hnsw"}}), vec!["d1"]),
        (json!({"prefix": {"code": "hnsw"}}), vec!["d2"]),
        // `wildcard` on a keyword field folds case on both sides in XERJ
        // (`expand_wildcard(case_insensitive: true)`, which the parser's
        // `term{case_insensitive:true}` lowering depends on) — the scan has
        // to keep that, not "fix" it. Whether that default is right at all
        // is issue #396, which pins the disagreement between these two rows
        // and the two above as CURRENT behaviour; if #396 changes the
        // default, these two rows change with it and this file is not the
        // place that decides it.
        (json!({"wildcard": {"code": "hnsw*"}}), vec!["d1", "d2"]),
        (json!({"wildcard": {"code": "Hnsw*"}}), vec!["d1", "d2"]),
        // ── dynamically mapped field — `text`, like `title` ──────────────
        (json!({"prefix": {"note": "Hnsw"}}), vec![]),
        (json!({"prefix": {"note": "hnsw"}}), vec!["d1", "d2"]),
        // ── the same leaves nested in a compound query ───────────────────
        // Proves the field types reach a recursive arm, not just the leaf
        // the top-level match dispatches on.
        (
            json!({"bool": {"filter": [{"prefix": {"title": "Hnsw"}}]}}),
            vec![],
        ),
        (
            json!({"bool": {"must": [{"prefix": {"title": "hnsw"}}],
                            "must_not": [{"wildcard": {"title": "*graph*"}}]}}),
            vec!["d2"],
        ),
        (
            json!({"constant_score": {"filter": {"wildcard": {"title": "Hnsw*"}}}}),
            vec![],
        ),
    ]
}

async fn assert_state(idx: &Index, state: &str) {
    // Several rows below expect NO hits, which an index that lost its
    // documents would also satisfy. Prove the corpus is there first.
    let (all, all_total, all_counted) = hits(idx, &json!({"match_all": {}})).await;
    assert_eq!(all, set(&["d1", "d2"]), "{state}: both documents are live");
    assert_eq!(all_total, 2, "{state}: hits.total over match_all");
    assert_eq!(all_counted, 2, "{state}: size:0 total over match_all");

    for (q, expected) in cases() {
        let (ids, total, counted) = hits(idx, &q).await;
        assert_eq!(ids, set(&expected), "{state}: hit set for {q}");
        assert_eq!(total, expected.len() as u64, "{state}: hits.total for {q}");
        assert_eq!(
            counted,
            expected.len() as u64,
            "{state}: size:0 total for {q} (the `_count` path)"
        );
    }
}

#[tokio::test]
async fn prefix_and_wildcard_do_not_change_their_answer_at_flush() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    // Every document still buffered: the stored-doc scan answers everything.
    let memtable = seed(&engine, "pw-mem", false, false).await;
    assert_state(&memtable, "memtable (nothing flushed)").await;

    // Every document in a segment: the term dictionary answers everything.
    let segment = seed(&engine, "pw-seg", false, true).await;
    assert_state(&segment, "segment (all flushed)").await;

    // One of each, live at once — a divergence here is a single response
    // that contradicts itself.
    let mixed = seed(&engine, "pw-mixed", true, false).await;
    assert_state(&mixed, "mixed (d1 flushed, d2 buffered)").await;
}
