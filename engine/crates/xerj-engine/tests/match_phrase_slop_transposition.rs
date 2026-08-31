//! Regression tests for issue #830: `match_phrase` `slop` must follow
//! Lucene `SloppyPhraseMatcher` move-distance semantics — a transposed
//! (reordered) pair costs 2 — instead of the old in-order-only gap walk,
//! under which a reordering never matched at ANY slop.
//!
//! Lucene's own class javadoc: for query `"a b"~2`, a document `x a b a y`
//! matches once as `a b` (distance 0) and once as `b a` (distance 2), so
//! ES answers `{"match_phrase": {"t": {"query": "quick brown", "slop": 2}}}`
//! on a document reading `brown quick` — and pre-fix XERJ answered it with
//! zero hits at slop 2, 3, 4, ….
//!
//! Every case is asserted BOTH pre-flush and post-flush, which pins
//! flush-invariance (#218/#222/#230 regression class) as well as the
//! semantics.  Note what the post-flush half does and does not exercise:
//! the planner keeps single-field `match_phrase`/`match_phrase_prefix` with
//! `slop > 0` on the STORED SCAN ("slop>0 and non-standard analyzers keep
//! the stored scan", index.rs), so those rows re-run `phrase_walk` after the
//! flush rather than the segment positional clause.  The segment arm is
//! covered directly by the xerj-fts unit tests
//! (`sloppy_phrase_*` in `search.rs`) and by the `multi_match` case here.
//! Both arms call the one evaluator, `phrase_positions_match`.

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

fn req(q: Value) -> xerj_query::ast::SearchRequest {
    parse_request(&json!({ "query": q, "size": 50 })).expect("parse_request")
}

async fn ids(idx: &Index, q: &Value) -> BTreeSet<String> {
    idx.search(&req(q.clone()))
        .await
        .unwrap()
        .hits
        .iter()
        .map(|h| h.id.clone())
        .collect()
}

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

/// Run every case against the memtable, flush once, run every case again
/// against the segment, and assert the hit set is the expected one in both
/// states — semantics AND flush-invariance in one assertion.
async fn assert_both_states(idx: &std::sync::Arc<Index>, cases: &[(Value, &[&str], &str)]) {
    for (q, exp, label) in cases {
        let pre = ids(idx, q).await;
        assert_eq!(
            pre,
            set(exp),
            "{label}: PRE-flush (memtable) hit set for {q}"
        );
    }
    idx.flush().await.unwrap();
    for (q, exp, label) in cases {
        let post = ids(idx, q).await;
        assert_eq!(
            post,
            set(exp),
            "{label}: POST-flush (segment) hit set for {q}"
        );
    }
}

/// Doc "adj" holds the phrase in order and adjacent; "rev" holds it
/// TRANSPOSED (the #830 case); "gap" holds it in order with one intervening
/// token; "ctrl" must never match.
async fn seed(engine: &Engine, name: &str) -> std::sync::Arc<Index> {
    engine.create_index(name, Schema::empty()).unwrap();
    let idx = engine.get_index(name).unwrap();
    for (id, text) in [
        ("adj", "quick brown fox"),
        ("rev", "brown quick fox"),
        ("gap", "quick lazy brown"),
        ("ctrl", "the lazy dog"),
    ] {
        idx.index_document(Some(id.into()), json!({ "t": text }))
            .await
            .unwrap();
    }
    idx
}

/// `match_phrase` "quick brown": slop 0 → adjacency only; slop 1 → the
/// forward gap joins; slop 2+ → the transposition joins (cost 2).  The
/// slop-1 rows double as the negative case: the transposition must NOT be
/// admitted below 2, and `ctrl` never matches.
#[tokio::test]
async fn match_phrase_slop_admits_transposition_at_two() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "phrase_transposition").await;

    let q =
        |slop: u32| json!({ "match_phrase": { "t": { "query": "quick brown", "slop": slop } } });
    let cases: Vec<(Value, &[&str], &str)> = vec![
        (q(0), &["adj"], "slop 0: adjacency only"),
        (
            q(1),
            &["adj", "gap"],
            "slop 1: forward gap yes, transposition no",
        ),
        (
            q(2),
            &["adj", "gap", "rev"],
            "slop 2: transposition matches",
        ),
        (
            q(3),
            &["adj", "gap", "rev"],
            "slop 3: superset stays stable",
        ),
    ];
    assert_both_states(&idx, &cases).await;
}

/// Same semantics through `match_phrase_prefix` (the `last_is_prefix` arm of
/// the shared walk; the segment side evaluates it via per-expansion sloppy
/// phrase queries): "quick bro" finds `bro*` transposed against "quick" at
/// slop 2 and not below.
#[tokio::test]
async fn match_phrase_prefix_slop_admits_transposition_at_two() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "phrase_prefix_transposition").await;

    let q = |slop: u32| json!({ "match_phrase_prefix": { "t": { "query": "quick bro", "slop": slop } } });
    let cases: Vec<(Value, &[&str], &str)> = vec![
        (q(0), &["adj"], "prefix slop 0: adjacency only"),
        (
            q(1),
            &["adj", "gap"],
            "prefix slop 1: forward gap yes, transposition no",
        ),
        (
            q(2),
            &["adj", "gap", "rev"],
            "prefix slop 2: transposition matches",
        ),
    ];
    assert_both_states(&idx, &cases).await;
}

/// The `multi_match` phrase type lowers to the same per-field phrase
/// predicate — one case pins that arm too.
#[tokio::test]
async fn multi_match_phrase_slop_admits_transposition_at_two() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed(&engine, "multi_match_transposition").await;

    let q = |slop: u32| json!({ "multi_match": { "query": "quick brown", "fields": ["t"], "type": "phrase", "slop": slop } });
    let cases: Vec<(Value, &[&str], &str)> = vec![
        (
            q(1),
            &["adj", "gap"],
            "multi_match phrase slop 1: no transposition",
        ),
        (
            q(2),
            &["adj", "gap", "rev"],
            "multi_match phrase slop 2: transposition matches",
        ),
    ];
    assert_both_states(&idx, &cases).await;
}

/// Doc "twin" repeats a phrase term; "single" holds it only once.  The
/// review of #882 found that the first minimal-window sweep lost repeated
/// terms: `"i said no no"~1` stopped matching `i said uh no no` at every
/// slop, because on a collision it advanced the pointer at the minimum
/// adjusted position (a non-colliding, single-occurrence slot) and exhausted
/// it before ever trying the assignment that separates the two `no`s.
async fn seed_repeats(engine: &Engine, name: &str) -> std::sync::Arc<Index> {
    engine.create_index(name, Schema::empty()).unwrap();
    let idx = engine.get_index(name).unwrap();
    for (id, text) in [
        ("twin", "i said uh no no"),
        ("single", "i said no thanks"),
        ("noway", "no way no no"),
        ("ctrl", "the lazy dog"),
    ] {
        idx.index_document(Some(id.into()), json!({ "t": text }))
            .await
            .unwrap();
    }
    idx
}

/// A repeated phrase term must not cost a document its match.  `"i said no
/// no"` needs two DISTINCT `no` tokens: "twin" has them (adjusted 0,0,1,1 —
/// span 1), "single" has one `no` and must never match at any slop.
#[tokio::test]
async fn match_phrase_slop_repeated_term_still_matches() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed_repeats(&engine, "phrase_repeats").await;

    let q =
        |slop: u32| json!({ "match_phrase": { "t": { "query": "i said no no", "slop": slop } } });
    let cases: Vec<(Value, &[&str], &str)> = vec![
        (
            q(1),
            &["twin"],
            "slop 1: one intervening token, two distinct 'no' tokens",
        ),
        (q(2), &["twin"], "slop 2: still just the doc with two 'no's"),
        (
            q(3),
            &["twin"],
            "slop 3: 'single' has only one 'no' — never",
        ),
        (q(10), &["twin"], "slop 10: a repeat needs a second token"),
    ];
    assert_both_states(&idx, &cases).await;
}

/// Every slot the same term: `"no no no"~1` on "no way no no"
/// (no@[0,2,3] — pick 0,2,3, adjusted 0,1,1, span 1).
#[tokio::test]
async fn match_phrase_slop_all_slots_repeated() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed_repeats(&engine, "phrase_repeats_all").await;

    let q = |slop: u32| json!({ "match_phrase": { "t": { "query": "no no no", "slop": slop } } });
    let cases: Vec<(Value, &[&str], &str)> = vec![
        (q(1), &["noway"], "slop 1: three distinct 'no' positions"),
        (q(3), &["noway"], "slop 3: superset stays stable"),
    ];
    assert_both_states(&idx, &cases).await;
}

/// The same defect through `multi_match` — the arm that DOES reach the
/// segment positional clause after a flush.
#[tokio::test]
async fn multi_match_phrase_slop_repeated_term_still_matches() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = seed_repeats(&engine, "multi_match_repeats").await;

    let q = |slop: u32| json!({ "multi_match": { "query": "i said no no", "fields": ["t"], "type": "phrase", "slop": slop } });
    let cases: Vec<(Value, &[&str], &str)> = vec![
        (q(1), &["twin"], "multi_match phrase slop 1: repeat matches"),
        (q(3), &["twin"], "multi_match phrase slop 3: repeat matches"),
    ];
    assert_both_states(&idx, &cases).await;
}
