//! Issue #834: `dis_max` scored every buffered match a flat 1.0 on the
//! memtable — the stored-doc scorer had no DisMax arm, so its catch-all
//! fired — while the segment path applies the real
//! `max(sub_scores) + tie_breaker × Σ(rest)` combination. A doc matching
//! MORE disjuncts (which must rank highest) tied with single-disjunct
//! docs pre-flush, and the ranking changed at `_flush`.
//!
//! The fix scores each single-field `match` sub through the SAME memtable
//! BM25 path a standalone `match` takes (`search_text_boosted`, per
//! (field, query) sub) and combines per doc exactly like the segment's
//! `execute_dis_max`. These tests pin both properties in BOTH phases:
//!
//! * the two-disjunct doc ranks strictly first;
//! * per doc, the `dis_max` score equals
//!   `max + tie_breaker × (Σ − max)` over the scores the SAME standalone
//!   sub-queries return in the SAME phase — the invariant that kept the
//!   reverted stored-doc-scorer attempt (PR #835) out (#572: sub scores
//!   inside the compound must equal the standalone sub scores).
//!
//! Elasticsearch is referenced for wire semantics only; no ES code is here.

use serde_json::{json, Value};
use std::collections::BTreeSet;
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

fn req(q: Value) -> xerj_query::ast::SearchRequest {
    parse_request(&json!({ "query": q, "size": 50 })).expect("parse_request")
}

/// Score of `id` under `q`, or 0.0 when the doc is not a hit — the
/// contribution a non-matching `dis_max` sub makes.
async fn score_or_zero(idx: &Index, q: &Value, id: &str) -> f32 {
    idx.search(&req(q.clone()))
        .await
        .unwrap()
        .hits
        .iter()
        .find(|h| h.id == id)
        .map(|h| h.score)
        .unwrap_or(0.0)
}

async fn hit_ids(idx: &Index, q: &Value) -> BTreeSet<String> {
    idx.search(&req(q.clone()))
        .await
        .unwrap()
        .hits
        .iter()
        .map(|h| h.id.clone())
        .collect()
}

fn expect(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

/// Per-doc dis_max contract: the compound's score equals
/// `max + tie·(Σ − max)` over the SAME standalone sub scores.
async fn assert_dis_max_contract(
    idx: &Index,
    dm: &Value,
    sub_x: &Value,
    sub_y: &Value,
    tie: f32,
    id: &str,
    phase: &str,
) {
    let s_dm = score_or_zero(idx, dm, id).await;
    let s_x = score_or_zero(idx, sub_x, id).await;
    let s_y = score_or_zero(idx, sub_y, id).await;
    let max = s_x.max(s_y);
    let want = max + tie * (s_x + s_y - max);
    assert!(
        (s_dm - want).abs() < 1e-4,
        "{phase}: dis_max({id}) = {s_dm}, want max + tie·(Σ−max) = \
         {want} from standalone subs x={s_x} y={s_y} (#834)"
    );
}

async fn assert_phase(
    idx: &Index,
    dm: &Value,
    sub_x: &Value,
    sub_y: &Value,
    tie: f32,
    phase: &str,
) {
    let res = idx.search(&req(dm.clone())).await.unwrap();
    let ids: BTreeSet<String> = res.hits.iter().map(|h| h.id.clone()).collect();
    assert_eq!(
        ids,
        expect(&["a", "b", "ab"]),
        "{phase}: docs matching ANY disjunct are hits (#834)"
    );
    assert_eq!(
        res.hits.first().map(|h| h.id.as_str()),
        Some("ab"),
        "{phase}: the two-disjunct doc must rank first (#834)"
    );

    assert_dis_max_contract(idx, dm, sub_x, sub_y, tie, "a", phase).await;
    assert_dis_max_contract(idx, dm, sub_x, sub_y, tie, "b", phase).await;
    assert_dis_max_contract(idx, dm, sub_x, sub_y, tie, "ab", phase).await;

    let s_ab = score_or_zero(idx, dm, "ab").await;
    let s_a = score_or_zero(idx, dm, "a").await;
    let s_b = score_or_zero(idx, dm, "b").await;
    assert!(
        s_ab > s_a + 1e-4 && s_ab > s_b + 1e-4,
        "{phase}: ab={s_ab} must score strictly above a={s_a} and b={s_b} (#834)"
    );
}

#[tokio::test]
async fn dis_max_ranks_multi_disjunct_doc_first_and_combines_like_the_segment() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    let mut schema = Schema::empty();
    schema.fields.push(FieldConfig::new("x", FieldType::Text));
    schema.fields.push(FieldConfig::new("y", FieldType::Text));
    engine.create_index("dmscore", schema).unwrap();
    let idx = engine.get_index("dmscore").unwrap();

    // `a` matches only the x-disjunct, `b` only the y-disjunct, `ab` both —
    // so `ab` must score max + tie_breaker·min and rank strictly first.
    idx.index_document(Some("a".into()), json!({ "x": "quick", "y": "other" }))
        .await
        .unwrap();
    idx.index_document(Some("b".into()), json!({ "x": "other", "y": "quick" }))
        .await
        .unwrap();
    idx.index_document(Some("ab".into()), json!({ "x": "quick", "y": "quick" }))
        .await
        .unwrap();

    let tie = 0.3_f32;
    let dm = json!({ "dis_max": {
        "queries": [ { "match": { "x": "quick" } }, { "match": { "y": "quick" } } ],
        "tie_breaker": tie,
    }});
    let sub_x = json!({ "match": { "x": "quick" } });
    let sub_y = json!({ "match": { "y": "quick" } });

    assert_phase(&idx, &dm, &sub_x, &sub_y, tie, "pre-flush").await;
    idx.flush().await.unwrap();
    assert_phase(&idx, &dm, &sub_x, &sub_y, tie, "post-flush").await;
}

/// A `dis_max` with a sub the memtable BM25 path cannot score (here a
/// `term`) must keep its existing doc-scan route unchanged — the #572
/// keyword-rewrite shape `DisMax{[Term, MultiMatch]}` depends on it. The
/// hit SET (docs matching any disjunct) is asserted in both phases.
#[tokio::test]
async fn dis_max_with_non_match_sub_keeps_its_hit_set() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("tag", FieldType::Keyword));
    schema.fields.push(FieldConfig::new("y", FieldType::Text));
    engine.create_index("dmmixed", schema).unwrap();
    let idx = engine.get_index("dmmixed").unwrap();

    idx.index_document(Some("t".into()), json!({ "tag": "vip", "y": "other" }))
        .await
        .unwrap();
    idx.index_document(Some("m".into()), json!({ "tag": "guest", "y": "quick" }))
        .await
        .unwrap();
    idx.index_document(Some("n".into()), json!({ "tag": "guest", "y": "other" }))
        .await
        .unwrap();

    let dm = json!({ "dis_max": {
        "queries": [ { "term": { "tag": "vip" } }, { "match": { "y": "quick" } } ],
        "tie_breaker": 0.5,
    }});

    assert_eq!(
        hit_ids(&idx, &dm).await,
        expect(&["t", "m"]),
        "pre-flush: mixed-sub dis_max hit set (#834 scope gate)"
    );
    idx.flush().await.unwrap();
    assert_eq!(
        hit_ids(&idx, &dm).await,
        expect(&["t", "m"]),
        "post-flush: mixed-sub dis_max hit set (#834 scope gate)"
    );
}
