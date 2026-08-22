//! Issue #354: `match` / `multi_match` on a **keyword** field is mapping-aware
//! in a flushed segment but NOT in the memtable, so a multi-token query changes
//! its answer at `_flush`. No arrays are involved (independent of #332).
//!
//! `doc_matches_query` / `score_query_against_doc` (xerj-engine/src/index.rs)
//! evaluate the `Match` / `MultiMatch` arms by whitespace-tokenising the query
//! and OR-ing the tokens against the stored `_source`, never consulting the
//! mapping — so a `keyword` field is treated like an analyzed `text` field. The
//! segment side DOES consult the mapping (keyword analyzer), so the answer
//! diverges at flush.
//!
//! One doc, `tags` mapped `keyword`, scalar value `"red"`:
//! ```text
//! BEFORE flush | multi_match 'red blue' -> 1 hit  (WRONG: tokenised + OR-ed)
//! AFTER  flush | multi_match 'red blue' -> 0 hits (correct: "red" != "red blue")
//! ```
//! Elasticsearch takes the query text whole under the keyword analyzer, so
//! `"red blue"` is one term, does not equal `"red"`, and the doc matches in
//! NEITHER phase. This test asserts the correct (0-hit) answer in both phases.
//!
//! Elasticsearch is referenced for wire semantics only; no ES code is here.

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

fn expect(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

async fn score_of(idx: &Index, q: &Value, id: &str) -> f32 {
    idx.search(&req(q.clone()))
        .await
        .unwrap()
        .hits
        .iter()
        .find(|h| h.id == id)
        .unwrap_or_else(|| panic!("doc {id} not in hits for {q}"))
        .score
}

/// A multi-token `match`/`multi_match` over a scalar `keyword` field must match
/// the value WHOLE (keyword analyzer) — so `"red blue"` never matches `"red"` —
/// identically before and after `flush()`. Fixed by rewriting keyword-targeted
/// `Match`/`MultiMatch` clauses to whole-value `Term`s at the search entry
/// (`rewrite_keyword_full_text_to_term`), so the memtable agrees with the
/// segment. Proven fail-before on main (8831d85c): PRE-flush `multi_match
/// 'red blue'` returned {"2"} instead of {}.
#[tokio::test]
async fn keyword_match_takes_the_query_whole_in_both_phases() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("tags", FieldType::Keyword));
    engine.create_index("kw", schema).unwrap();
    let idx = engine.get_index("kw").unwrap();
    idx.index_document(Some("2".into()), json!({ "tags": "red" }))
        .await
        .unwrap();

    // (query, expected-ids, label). Same hit set required PRE- and POST-flush.
    let cases: &[(Value, &[&str], &str)] = &[
        (
            json!({ "multi_match": { "query": "red blue", "fields": ["tags"] } }),
            &[],
            "multi_match 'red blue' on scalar keyword",
        ),
        // A field spec carrying a `^boost` ("tags^2") must be recognised as
        // keyword too — the boost suffix is stripped before consulting the
        // mapping — so it takes the query whole and flips NEITHER phase.
        (
            json!({ "multi_match": { "query": "red blue", "fields": ["tags^2"] } }),
            &[],
            "multi_match 'red blue' on BOOSTED scalar keyword (tags^2)",
        ),
        // Positive control for the boosted spec: the whole value still matches.
        (
            json!({ "multi_match": { "query": "red", "fields": ["tags^2"] } }),
            &["2"],
            "multi_match 'red' on BOOSTED scalar keyword matches whole value",
        ),
        (
            json!({ "match": { "tags": "red blue" } }),
            &[],
            "match 'red blue' on scalar keyword",
        ),
        // Control: the whole value DOES match, in both phases.
        (
            json!({ "term": { "tags": "red" } }),
            &["2"],
            "term tags=red (control)",
        ),
    ];

    for (q, exp, label) in cases {
        assert_eq!(
            ids(&idx, q).await,
            expect(exp),
            "{label}: PRE-flush (memtable) hit set wrong for {q} — a keyword field \
             must take the query text whole, not whitespace-tokenise + OR it (#354)"
        );
    }
    idx.flush().await.unwrap();
    for (q, exp, label) in cases {
        assert_eq!(
            ids(&idx, q).await,
            expect(exp),
            "{label}: POST-flush (segment) hit set wrong for {q}"
        );
    }
}

/// A `multi_match` spanning BOTH a keyword and a text field must split: the
/// keyword field takes the query WHOLE (so `"red blue"` misses `"red"`), while
/// the text field stays analyzed (so a shared token still matches) — identically
/// before and after flush.
#[tokio::test]
async fn mixed_multi_match_splits_keyword_and_text() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("tags", FieldType::Keyword));
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    engine.create_index("mm", schema).unwrap();
    let idx = engine.get_index("mm").unwrap();
    // "1": keyword "red" (whole), text "blue sky" (analyzed → tokens blue, sky).
    idx.index_document(
        Some("1".into()),
        json!({ "tags": "red", "body": "blue sky" }),
    )
    .await
    .unwrap();
    // "2": keyword "green", text "hello world" — matches neither half of "red blue".
    idx.index_document(
        Some("2".into()),
        json!({ "tags": "green", "body": "hello world" }),
    )
    .await
    .unwrap();

    // multi_match "red blue" over [tags(keyword), body(text)]:
    //   tags whole "red blue" != "red"  -> no keyword match on "1"
    //   body analyzed has token "blue"  -> text match on "1"
    // So only "1" matches (via the text half); "2" matches neither.
    let q = json!({ "multi_match": { "query": "red blue", "fields": ["tags", "body"] } });
    let cases: &[(Value, &[&str], &str)] = &[(q, &["1"], "mixed multi_match 'red blue'")];
    for (q, exp, label) in cases {
        assert_eq!(
            ids(&idx, q).await,
            expect(exp),
            "{label}: PRE-flush hit set wrong for {q} — keyword half must be whole-value, \
             text half analyzed (#354)"
        );
    }
    idx.flush().await.unwrap();
    for (q, exp, label) in cases {
        assert_eq!(
            ids(&idx, q).await,
            expect(exp),
            "{label}: POST-flush hit set wrong for {q}"
        );
    }
}

/// #572: a `best_fields` multi_match spanning BOTH a keyword and a text field
/// must combine the two halves by **dis_max** (MAX + tie_breaker), exactly as ES
/// does — not the **SUM** a `Bool.should` gives. Hit sets are unchanged (pinned
/// by `mixed_multi_match_splits_keyword_and_text`); this pins the `_score` in
/// both the memtable and segment phases. Fail-before (the pre-fix `Bool.should`):
/// the combined score equals `kw + text`, so both asserts below trip.
#[tokio::test]
async fn best_fields_multi_match_scores_keyword_and_text_by_max_not_sum() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("tags", FieldType::Keyword));
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    engine.create_index("mmscore", schema).unwrap();
    let idx = engine.get_index("mmscore").unwrap();
    // "1" matches BOTH halves of query "vip": keyword `tags == "vip"` (whole)
    // AND text `body` contains "vip".
    idx.index_document(
        Some("1".into()),
        json!({ "tags": "vip", "body": "vip lounge access" }),
    )
    .await
    .unwrap();
    // Extra docs skew each field's BM25 stats so the two half-scores differ,
    // making MAX vs SUM unambiguous, and so both halves have matching peers.
    idx.index_document(
        Some("2".into()),
        json!({ "tags": "guest", "body": "vip vip vip vip vip" }),
    )
    .await
    .unwrap();
    idx.index_document(
        Some("3".into()),
        json!({ "tags": "vip", "body": "nothing relevant here at all" }),
    )
    .await
    .unwrap();

    let both = json!({ "multi_match": { "query": "vip", "fields": ["tags", "body"] } });
    let kw_only = json!({ "multi_match": { "query": "vip", "fields": ["tags"] } });
    let text_only = json!({ "multi_match": { "query": "vip", "fields": ["body"] } });

    for phase in ["pre-flush", "post-flush"] {
        let s_both = score_of(&idx, &both, "1").await;
        let s_kw = score_of(&idx, &kw_only, "1").await;
        let s_text = score_of(&idx, &text_only, "1").await;
        let max = s_kw.max(s_text);
        let sum = s_kw + s_text;
        assert!(
            (s_both - max).abs() < 1e-3,
            "{phase}: best_fields multi_match over [keyword, text] must score \
             max(kw={s_kw}, text={s_text})={max} (dis_max), got {s_both} (#572)"
        );
        assert!(
            s_both + 1e-3 < sum,
            "{phase}: combined score {s_both} must be MAX, not the SUM \
             kw+text={sum} a Bool.should gives (#572)"
        );
        if phase == "pre-flush" {
            idx.flush().await.unwrap();
        }
    }
}
