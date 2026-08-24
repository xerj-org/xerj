//! #399 — a one-clause `bool` is a no-op: it must not change `_score` or rank.
//!
//! `{"bool":{"must":[X]}}` and bare `X` are the same query.  On an UNFLUSHED
//! index they were not: `Index::search` steers on the *shape* of the tree, and
//! a `Bool` node is matched by `is_doc_scan_query`, so the memtable arm scored
//! it with the IDF-less brute scorer (`score_query_against_doc`, `1 + ln(1 +
//! tf)`) while the identical bare `Match` took the memtable BM25 arm
//! (`extract_query_text` → `search_text_boosted_with_total_using`, index-wide
//! stats).  Same 120 documents, same clause, ~400× apart and in a different
//! order.
//!
//! #361/#387 fixed the *flushed* half of this family (`bool.filter` summing
//! into `_score`, and the page-local IDF rescore overwriting exact BM25); the
//! flushed assertions below are the regression guard for that.  The memtable
//! half is what #399 reports and what the fix normalises away.
//!
//! Reference (apache/lucene, Apache-2.0, adapted not copied):
//!   * `BooleanQuery.rewrite` — "optimize 1-clause queries": with
//!     `minimumNumberShouldMatch == 0` a lone `SHOULD` or `MUST` clause
//!     rewrites to the clause's own query, and with
//!     `minimumNumberShouldMatch == 1` a lone `SHOULD` does too
//!     (BooleanQuery.java:279-298).  The wrapper is erased before a `Weight`
//!     is ever built, so it cannot contribute a score.
//!   * A lone `FILTER` clause instead rewrites to
//!     `new BoostQuery(new ConstantScoreQuery(query), 0)` — score 0, NOT the
//!     clause's score (BooleanQuery.java:290-292); `filter_only_bool_*` below
//!     pins that asymmetry.

use std::collections::HashMap;

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::{Engine, Index};
use xerj_query::executor::SearchResult;
use xerj_query::parse_request;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

fn req(query: Value, size: usize) -> xerj_query::ast::SearchRequest {
    parse_request(&json!({"query": query, "size": size, "track_total_hits": true}))
        .expect("parse_request")
}

/// The issue's corpus: 120 docs, one `text` field, one `keyword` field.
///
/// `alpha` is in every document with tf 1..=5, so exact BM25 gives it a tiny
/// IDF (`ln(1 + 0.5/120.5)`) and the brute scorer gives `1 + ln(1 + tf)` —
/// two scales that cannot be confused for each other.
async fn seed(name: &str, flush: bool) -> (TempDir, std::sync::Arc<Index>) {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("repo", FieldType::Keyword));
    engine.create_index(name, schema).unwrap();
    let idx = engine.get_index(name).unwrap();

    for i in 0..120usize {
        let alphas = "alpha ".repeat(1 + i % 5);
        let betas = if i < 60 {
            "beta ".repeat(1 + i % 3)
        } else {
            String::new()
        };
        let filler = "gamma ".repeat(1 + i % 7);
        idx.index_document(
            Some(format!("d{i:03}")),
            json!({
                "body": format!("{alphas}{betas}{filler}"),
                "repo": if i % 2 == 0 { "lucene" } else { "usearch" },
            }),
        )
        .await
        .unwrap();
    }
    if flush {
        idx.flush().await.unwrap();
    }

    // The two populations are the whole point of the test — assert which one
    // this fixture actually exercises rather than trusting the flush call.
    let stats = idx.stats().await;
    if flush {
        assert_eq!(
            stats.memtable_doc_count, 0,
            "{name}: flush did not drain the memtable"
        );
    } else {
        assert_eq!(
            stats.memtable_doc_count, 120,
            "{name}: fixture must stay memtable-resident"
        );
    }
    (dir, idx)
}

fn score_map(result: &SearchResult) -> HashMap<String, f32> {
    result
        .hits
        .iter()
        .map(|h| (h.id.clone(), h.score))
        .collect()
}

fn ids(result: &SearchResult) -> Vec<String> {
    result.hits.iter().map(|h| h.id.clone()).collect()
}

/// Every hit's `_score` must equal the bare clause's score for the same
/// `_id`, and the order must be identical.
fn assert_same_ranking(label: &str, reference: &SearchResult, actual: &SearchResult) {
    assert!(
        !actual.hits.is_empty(),
        "{label}: returned no hits — the comparison would be vacuous"
    );
    assert_eq!(
        reference.total.value, actual.total.value,
        "{label}: matched a different number of documents"
    );
    let reference_scores = score_map(reference);
    for hit in &actual.hits {
        let expected = reference_scores
            .get(&hit.id)
            .unwrap_or_else(|| panic!("{label}: {} is not in the bare-clause result", hit.id));
        assert!(
            (hit.score - expected).abs() < 1e-4,
            "{label}: _score for {} is {} but the bare clause scores it {expected}",
            hit.id,
            hit.score
        );
    }
    assert_eq!(
        ids(actual),
        ids(reference),
        "{label}: hit order diverged from the bare clause"
    );
}

const TEXT: &str = "alpha";

fn text_query() -> Value {
    json!({"match": {"body": TEXT}})
}

/// The four wrappers Lucene's 1-clause rewrite erases, each of which must
/// leave `_score` and rank byte-for-byte where the bare clause put them.
fn neutral_wrappers() -> Vec<(&'static str, Value)> {
    vec![
        ("bool.must[1]", json!({"bool": {"must": [text_query()]}})),
        (
            "bool.should[1]",
            json!({"bool": {"should": [text_query()]}}),
        ),
        (
            // `minimum_should_match: 1` on a lone should is the same query —
            // BooleanQuery.java:283-284.
            "bool.should[1]+msm1",
            json!({"bool": {"should": [text_query()], "minimum_should_match": 1}}),
        ),
        (
            // Nested wrappers collapse all the way down.
            "bool.must[bool.must[1]]",
            json!({"bool": {"must": [{"bool": {"must": [text_query()]}}]}}),
        ),
        (
            // #643: a lone `should` with a PERCENTAGE msm resolves to 1 for a
            // single optional clause (coerced to ≥1 when there is no required
            // clause), so it is the same query and must now collapse like
            // Fixed(0)/Fixed(1).
            "bool.should[1]+pct100",
            json!({"bool": {"should": [text_query()], "minimum_should_match": "100%"}}),
        ),
        (
            "bool.should[1]+pct50",
            json!({"bool": {"should": [text_query()], "minimum_should_match": "50%"}}),
        ),
    ]
}

/// #643 guard: a lone `should` whose `minimum_should_match` is a percentage
/// >= 200% resolves to `max(1, floor(1 * pct/100)) >= 2` — it requires 2 of 1
/// optional clause, which is unsatisfiable, so the bool matches NOTHING. It must
/// stay wrapped, NOT collapse to the bare clause (which matches). Guards the
/// over-broad `Percentage(_)` unwrap the 3-skeptic pass caught.
#[tokio::test]
async fn high_percentage_msm_lone_should_matches_nothing() {
    for flush in [false, true] {
        let (_dir, idx) = seed(&format!("bool-pct-high-{flush}"), flush).await;
        let bare = idx.search(&req(text_query(), 50)).await.unwrap();
        assert_eq!(
            bare.total.value, 120,
            "fixture: the bare clause matches all"
        );
        for pct in ["200%", "300%"] {
            let q = json!({"bool": {"should": [text_query()], "minimum_should_match": pct}});
            let r = idx.search(&req(q, 50)).await.unwrap();
            assert_eq!(
                r.total.value, 0,
                "#643: lone should + msm {pct} requires 2 of 1 clause \u{2192} must match nothing (flush={flush})"
            );
        }
    }
}

/// The headline of #399, on the population that showed it: an UNFLUSHED index.
#[tokio::test]
async fn single_clause_bool_is_score_neutral_on_memtable() {
    let (_dir, idx) = seed("bool-one-clause-memtable", false).await;

    let reference = idx.search(&req(text_query(), 50)).await.unwrap();
    assert_eq!(reference.total.value, 120, "fixture: every doc has `alpha`");
    let distinct: std::collections::HashSet<u32> = reference
        .hits
        .iter()
        .map(|h| (h.score * 1e6) as u32)
        .collect();
    assert!(
        distinct.len() > 1,
        "fixture must produce distinct scores, got {:?}",
        reference.hits.iter().map(|h| h.score).collect::<Vec<_>>()
    );

    for (label, wrapped) in neutral_wrappers() {
        let actual = idx.search(&req(wrapped, 50)).await.unwrap();
        assert_same_ranking(label, &reference, &actual);
    }
}

/// Same assertions on the flushed (segment FTS) population — the half #387
/// fixed.  Guards against a fix for the memtable half re-introducing the
/// divergence here.
#[tokio::test]
async fn single_clause_bool_is_score_neutral_on_segments() {
    let (_dir, idx) = seed("bool-one-clause-segments", true).await;

    let reference = idx.search(&req(text_query(), 50)).await.unwrap();
    assert_eq!(reference.total.value, 120);

    for (label, wrapped) in neutral_wrappers() {
        let actual = idx.search(&req(wrapped, 50)).await.unwrap();
        assert_same_ranking(label, &reference, &actual);
    }
}

/// `_score` must not depend on `size` for either shape, and the two shapes
/// must agree at every page size — the property a client doing RRF/threshold
/// fusion actually relies on.
#[tokio::test]
async fn wrapped_and_bare_agree_at_every_page_size() {
    for flush in [false, true] {
        let (_dir, idx) = seed(&format!("bool-one-clause-size-{flush}"), flush).await;
        for size in [1usize, 10, 50] {
            let bare = idx.search(&req(text_query(), size)).await.unwrap();
            let wrapped = idx
                .search(&req(json!({"bool": {"must": [text_query()]}}), size))
                .await
                .unwrap();
            assert_same_ranking(&format!("flush={flush} size={size}"), &bare, &wrapped);
        }
    }
}

/// The same neutrality for a `term` on a `keyword` field.
///
/// This one is the anti-regression half: three places in the engine ALREADY
/// treat a single-`must` bool as the identity — `scored_fast_plan` wraps a
/// bare scoring leaf "as a single-must bool (Σ over one clause is the
/// identity)", `mem_bool_preds` accepts a bare `Term`/`Range` as its own
/// one-predicate fused walk, and `peel_knn_query` peels `bool{must:[knn]}` to
/// the same `PeeledKnn` as a bare `knn`.  Erasing the wrapper must not push
/// this shape off any of them onto a different scorer; green before and after.
#[tokio::test]
async fn single_clause_bool_is_score_neutral_for_term_on_keyword() {
    for flush in [false, true] {
        let (_dir, idx) = seed(&format!("bool-one-clause-term-{flush}"), flush).await;
        let bare = idx
            .search(&req(json!({"term": {"repo": "lucene"}}), 50))
            .await
            .unwrap();
        assert_eq!(bare.total.value, 60, "flush={flush}");
        let wrapped = idx
            .search(&req(
                json!({"bool": {"must": [{"term": {"repo": "lucene"}}]}}),
                50,
            ))
            .await
            .unwrap();
        assert_same_ranking(&format!("bool.must[term] flush={flush}"), &bare, &wrapped);
    }
}

/// A lone `filter` clause is NOT the neutral case: Lucene rewrites it to
/// `BoostQuery(ConstantScoreQuery(query), 0)` (BooleanQuery.java:290-292), so
/// the score is 0, not the clause's own score.  This is the guard that the
/// unwrap above cannot quietly grow a `filter` arm.
///
/// FLUSHED ONLY, deliberately.  On an unflushed index the same query scores
/// **1.0**, because the memtable's brute scorer falls back to 1.0 for a
/// zero-sum bool — a pre-existing, separately-tracked filter-context defect
/// that #399 does not touch and this change does not alter (see the `if score
/// == 0.0` arm of `score_query_against_doc`'s `Bool` case in
/// `xerj-engine/src/index.rs`, whose own comment scopes it out).  Asserting
/// 1.0 here would pin a wrong number as correct, so the case is stated and
/// left to its own fix instead.
#[tokio::test]
async fn filter_only_bool_still_scores_zero_when_flushed() {
    let (_dir, idx) = seed("bool-one-clause-filter", true).await;
    let result = idx
        .search(&req(
            json!({"bool": {"filter": [{"term": {"repo": "lucene"}}]}}),
            50,
        ))
        .await
        .unwrap();
    assert_eq!(result.total.value, 60);
    assert_eq!(result.hits.len(), 50);
    for hit in &result.hits {
        assert_eq!(
            hit.score, 0.0,
            "{} scored {} on a filter-only bool",
            hit.id, hit.score
        );
    }
}

/// A lone `must_not` is a pure-negative bool — it has no positive clause, so
/// it must not be unwrapped into its (negated) child.  Green before the fix;
/// it must stay green.  (`clauses.size() == clauseSets.get(MUST_NOT).size()`
/// → `MatchNoDocsQuery("pure negative BooleanQuery")`, BooleanQuery.java:274-277
/// — Lucene returns no matches; XERJ answers it as an exclusion over all docs,
/// which is the pre-existing behaviour this test pins, not a claim about ES.)
#[tokio::test]
async fn must_not_only_bool_is_not_unwrapped() {
    let (_dir, idx) = seed("bool-one-clause-must-not", false).await;
    let result = idx
        .search(&req(
            json!({"bool": {"must_not": [{"term": {"repo": "lucene"}}]}}),
            50,
        ))
        .await
        .unwrap();
    for hit in &result.hits {
        assert_eq!(
            hit.source.get("repo").and_then(Value::as_str),
            Some("usearch"),
            "{} survived a must_not on repo:lucene",
            hit.id
        );
    }
}

/// #643 remaining gap: before the fix, a single-clause `bool` nested inside a
/// score-preserving wrapper that `unwrap_single_clause_bool` does NOT recurse
/// through (`function_score` inner query, `dis_max` single query) stayed
/// wrapped, so the unwrap did not match Lucene's recursive rewrite for those
/// wrappers. A no-functions `function_score` and a single-query `dis_max`
/// (tie_breaker 0) are both score-neutral, so each must rank exactly like the
/// bare clause on BOTH populations — and empirically they ALREADY do, with or
/// without the fix (the #399 divergence does not manifest for these nested
/// shapes; the wrappers force the doc-scan path regardless). So this is a
/// neutrality REGRESSION GUARD, not a fail-before: the fix's real fail-before is
/// the unit tests `unwrap_recurses_through_*`, which assert the nested `bool` is
/// erased to the bare clause.
#[tokio::test]
async fn nested_single_clause_bool_in_wrappers_is_score_neutral() {
    let shapes = [
        (
            "function_score[bool.must[1]]",
            json!({"function_score": {"query": {"bool": {"must": [text_query()]}}}}),
        ),
        (
            "dis_max[bool.must[1]]",
            json!({"dis_max": {"queries": [{"bool": {"must": [text_query()]}}], "tie_breaker": 0.0}}),
        ),
    ];
    for flush in [false, true] {
        let (_dir, idx) = seed(&format!("bool-643-nested-{flush}"), flush).await;
        // Reference: the SAME wrapper around the BARE clause (isolates the inner
        // bool-unwrap effect from any wrapper scoring).
        for (label, wrapped) in &shapes {
            let bare_wrapped = match label {
                l if l.starts_with("function_score") => {
                    json!({"function_score": {"query": text_query()}})
                }
                _ => json!({"dis_max": {"queries": [text_query()], "tie_breaker": 0.0}}),
            };
            let reference = idx.search(&req(bare_wrapped, 50)).await.unwrap();
            let actual = idx.search(&req(wrapped.clone(), 50)).await.unwrap();
            assert_same_ranking(&format!("{label} flush={flush}"), &reference, &actual);
        }
    }
}
