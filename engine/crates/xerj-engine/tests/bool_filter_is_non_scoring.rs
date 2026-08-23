//! #361 — a `bool` clause that must not score, must not score.
//!
//! Two defects, one symptom.  `bool.filter` was projected onto the FTS bool's
//! `must` slot, so a filter's BM25 was SUMMED into `_score`; and the
//! page-local "IDF-weighted rescore" then overwrote the exact BM25 the FTS
//! path had produced whenever the bool had two or more Match/MultiMatch/Term
//! children — counting `filter` and `must_not` among them — deriving its `N`
//! from `final_hits.len()`, the RETURNED PAGE, which makes `_score` a
//! function of `size`.
//!
//! Reference (apache/lucene, Apache-2.0, adapted not copied):
//!   * `BooleanClause.isScoring()` — `occur == MUST || occur == SHOULD`
//!     (BooleanClause.java:84-87): FILTER and MUST_NOT are non-scoring by
//!     definition.
//!   * `BooleanWeight`'s ctor builds each clause's Weight with
//!     `c.isScoring() ? scoreMode : ScoreMode.COMPLETE_NO_SCORES`
//!     (BooleanWeight.java:49-64) — a filter is structurally incapable of
//!     contributing a score.
//!   * `BM25Similarity.idfExplain` takes `N` from `fieldStats.docCount()`
//!     ("N, total number of documents with field", BM25Similarity.java:177-197)
//!     and `TermQuery.createWeight` gathers term statistics from
//!     `searcher.getTopReaderContext()` — the whole index
//!     (TermQuery.java:298-312).  Never from the collected page.

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
    parse_request(&json!({"query": query, "size": size})).expect("parse_request")
}

/// 120 docs over one `text` and one `keyword` field.
///
/// `alpha` is in every document with a varying term frequency (distinct BM25
/// scores, so a rescore that collapses them is visible); `beta` is in the
/// first 60 only (a different document frequency); `repo` splits the corpus
/// in half so a keyword filter selects exactly 60.  Flushed, because the bug
/// is on the segment FTS path — the memtable scorer is a separate,
/// deliberately-untouched population (see `heuristic_scored_applied`).
async fn seed(name: &str) -> (TempDir, std::sync::Arc<Index>) {
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
    idx.flush().await.unwrap();
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

/// Every hit's `_score` must equal the reference score for the same `_id`,
/// and the order must be the reference order restricted to the returned set.
fn assert_matches_reference(label: &str, reference: &SearchResult, actual: &SearchResult) {
    let reference_scores = score_map(reference);
    let reference_order = ids(reference);
    for hit in &actual.hits {
        let expected = reference_scores
            .get(&hit.id)
            .unwrap_or_else(|| panic!("{label}: {} is not in the reference result", hit.id));
        assert!(
            (hit.score - expected).abs() < 1e-4,
            "{label}: _score for {} is {} but the same clause alone scores {expected}",
            hit.id,
            hit.score
        );
    }
    let actual_ids = ids(actual);
    let expected_order: Vec<String> = reference_order
        .into_iter()
        .filter(|id| actual_ids.contains(id))
        .collect();
    assert_eq!(
        actual_ids, expected_order,
        "{label}: hit order diverged from the unfiltered ranking"
    );
}

const TEXT: &str = "alpha beta";

fn text_query() -> Value {
    json!({"match": {"body": TEXT}})
}

/// The headline of #361: `bool.must` + `bool.filter` — the most common query
/// shape in the ES ecosystem — must score exactly like the `must` clause on
/// its own.  `BooleanClause.isScoring()` (BooleanClause.java:84-87).
#[tokio::test]
async fn filter_clause_contributes_nothing_to_score_or_order() {
    let (_dir, idx) = seed("bool-filter-scoring").await;

    // Reference: the text clause alone, deep enough to cover every hit the
    // filtered queries can return.
    let reference = idx.search(&req(text_query(), 200)).await.unwrap();
    assert_eq!(reference.total.value, 120, "fixture: every doc has `alpha`");
    let reference_scores = score_map(&reference);
    assert!(
        reference_scores.values().any(|s| *s > 1.0),
        "fixture must expose the exact BM25 path, got {:?}",
        reference.hits.first().map(|h| h.score)
    );

    // Wrapping in a bool changes nothing.
    let wrapped = idx
        .search(&req(json!({"bool": {"must": [text_query()]}}), 50))
        .await
        .unwrap();
    assert_matches_reference("bool.must alone", &reference, &wrapped);

    // Neither does adding a filter.
    let filtered = idx
        .search(&req(
            json!({"bool": {
                "must": [text_query()],
                "filter": [{"term": {"repo": "lucene"}}]
            }}),
            50,
        ))
        .await
        .unwrap();
    assert_eq!(filtered.total.value, 60, "filter must still narrow the set");
    assert_matches_reference("bool.must + bool.filter", &reference, &filtered);
}

/// `must_not` is non-scoring for the same reason as `filter`.
#[tokio::test]
async fn must_not_clause_contributes_nothing_to_score_or_order() {
    let (_dir, idx) = seed("bool-must-not-scoring").await;
    let reference = idx.search(&req(text_query(), 200)).await.unwrap();

    let excluded = idx
        .search(&req(
            json!({"bool": {
                "must": [text_query()],
                "must_not": [{"term": {"repo": "usearch"}}]
            }}),
            50,
        ))
        .await
        .unwrap();
    assert_eq!(excluded.total.value, 60);
    assert_matches_reference("bool.must + bool.must_not", &reference, &excluded);
}

/// The assertion that would have caught the defect, and that the earlier
/// bool-scoring fix did not cover: BM25 statistics come from the index, not
/// from the collected page (`TermQuery.createWeight` → `TermStates.build`
/// over `searcher.getTopReaderContext()`, TermQuery.java:298-312), so
/// `_score` cannot depend on `size`.
///
/// This shape has no filter at all — it is what `query_string` lowers to —
/// and it is the half that the clause-walk fix alone does not reach.
#[tokio::test]
async fn score_is_invariant_to_page_size() {
    let (_dir, idx) = seed("bool-size-invariance").await;

    for query in [
        json!({"bool": {"must": [
            {"match": {"body": "alpha"}},
            {"match": {"body": "beta"}}
        ]}}),
        json!({"bool": {"should": [
            {"match": {"body": "alpha"}},
            {"match": {"body": "beta"}}
        ]}}),
        json!({"bool": {
            "must": [{"match": {"body": "alpha"}}],
            "filter": [{"term": {"repo": "lucene"}}]
        }}),
    ] {
        let deep = idx.search(&req(query.clone(), 50)).await.unwrap();
        let shallow = idx.search(&req(query.clone(), 1)).await.unwrap();
        assert!(!deep.hits.is_empty() && !shallow.hits.is_empty());
        assert_eq!(
            shallow.hits[0].id, deep.hits[0].id,
            "{query}: size:1 and size:50 disagree on the best hit"
        );
        assert!(
            (shallow.hits[0].score - deep.hits[0].score).abs() < 1e-4,
            "{query}: top hit scores {} at size:1 but {} at size:50",
            shallow.hits[0].score,
            deep.hits[0].score
        );
    }
}

/// A bool whose only clauses are filters matches, and scores 0 — there is no
/// scoring clause to sum.  Green before this change; it must stay green.
#[tokio::test]
async fn filter_only_bool_still_scores_zero() {
    let (_dir, idx) = seed("bool-filter-only").await;
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

/// The still-live #361 case after #387: a **non-projectable** filter (here
/// `exists`, which has no FTS projection arm) must not divert the MUST clause
/// off the exact-BM25 scorer onto the IDF-less heuristic. `exists: repo`
/// matches every doc, so this is a *no-op* filter — the headline of the issue
/// ("a no-op filter flattens every score to a constant") — and the result
/// must be byte-identical to the bare `match` reference.
///
/// Root cause: in `query_node_to_fts_with_keyword_fields` the Bool arm's
/// filter loop does `let fq = ...(sub)?;`, and `exists` returns `None` (no FTS
/// arm), so the `?` aborts the ENTIRE projection — dragging the scoring
/// `must` subtree onto `score_query_against_doc` (`boost·(1+ln(1+tf))`, no
/// IDF). #387 only covered *projectable* (`term`) filters.
#[tokio::test]
async fn nonprojectable_filter_does_not_divert_the_scorer() {
    let (_dir, idx) = seed("bool-nonprojectable-filter").await;
    let reference = idx.search(&req(text_query(), 200)).await.unwrap();
    assert!(
        score_map(&reference).values().any(|s| *s > 1.0),
        "fixture must expose the exact BM25 path, got {:?}",
        reference.hits.first().map(|h| h.score)
    );

    let filtered = idx
        .search(&req(
            json!({"bool": {
                "must": [text_query()],
                "filter": [{"exists": {"field": "repo"}}]
            }}),
            50,
        ))
        .await
        .unwrap();
    assert_eq!(
        filtered.total.value, 120,
        "exists:repo matches every doc — a no-op filter"
    );
    assert_matches_reference(
        "bool.must + non-projectable exists filter",
        &reference,
        &filtered,
    );
}

/// The correctness half of the non-projectable-filter fix: a residual filter
/// that genuinely EXCLUDES documents must still exclude them. Once the
/// projection stops `?`-aborting on a non-projectable filter, the FTS bool no
/// longer carries that filter — so membership must be re-applied by the
/// caller's `doc_matches_query` gate, or the query over-returns.
///
/// Fixture: `tag` (keyword) is present on the 60 even docs only. `exists:tag`
/// is non-projectable AND selective, so `bool{must:[match], filter:[exists:tag]}`
/// must return exactly those 60 — not all 120.
#[tokio::test]
async fn residual_filter_still_excludes_nonmatching_docs() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("tag", FieldType::Keyword));
    engine.create_index("residual-excl", schema).unwrap();
    let idx = engine.get_index("residual-excl").unwrap();
    for i in 0..120usize {
        let alphas = "alpha ".repeat(1 + i % 5);
        let mut doc = json!({ "body": alphas });
        if i % 2 == 0 {
            doc.as_object_mut()
                .unwrap()
                .insert("tag".into(), json!("kept"));
        }
        idx.index_document(Some(format!("d{i:03}")), doc)
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();

    let filtered = idx
        .search(&req(
            json!({"bool": {
                "must": [{"match": {"body": "alpha"}}],
                "filter": [{"exists": {"field": "tag"}}]
            }}),
            200,
        ))
        .await
        .unwrap();
    assert_eq!(
        filtered.total.value, 60,
        "exists:tag is selective — only the 60 even docs carry `tag`; a missing \
         membership gate over-returns all 120 (#361)"
    );
    for hit in &filtered.hits {
        let n: usize = hit.id[1..].parse().unwrap();
        assert_eq!(n % 2, 0, "{} has no `tag` and must be excluded", hit.id);
    }
}

/// #577: a `count_only` (size:0) residual query must return the exact total via
/// the membership gate with ZERO materialised hits. The residual pass now skips
/// the `Hit` build under count_only (mirroring the normal walk), rather than
/// hydrating O(matches) `Hit`s for a page that is empty. Result-invariant guard:
/// the count stays exact and the page stays empty.
#[tokio::test]
async fn residual_count_only_returns_exact_total_with_no_hits() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("tag", FieldType::Keyword));
    engine.create_index("residual-count-only", schema).unwrap();
    let idx = engine.get_index("residual-count-only").unwrap();
    for i in 0..120usize {
        let alphas = "alpha ".repeat(1 + i % 5);
        let mut doc = json!({ "body": alphas });
        if i % 2 == 0 {
            doc.as_object_mut()
                .unwrap()
                .insert("tag".into(), json!("kept"));
        }
        idx.index_document(Some(format!("d{i:03}")), doc)
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();

    let counted = idx
        .search(&req(
            json!({"bool": {
                "must": [{"match": {"body": "alpha"}}],
                "filter": [{"exists": {"field": "tag"}}]
            }}),
            0,
        ))
        .await
        .unwrap();
    assert_eq!(
        counted.total.value, 60,
        "count_only must still apply the residual membership gate — only the 60 \
         even docs carry `tag`"
    );
    assert_eq!(
        counted.hits.len(),
        0,
        "size:0 returns no hits — the residual pass must not materialise any (#577)"
    );
}

/// The residual gate must be page-size-invariant: forcing full materialisation
/// and re-counting survivors must give the same top hit, the same score, and
/// the same total at size:1 and size:200 (a gate that miscounts or mis-ranks
/// would diverge).
#[tokio::test]
async fn residual_gate_is_page_size_invariant() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("tag", FieldType::Keyword));
    engine.create_index("residual-page", schema).unwrap();
    let idx = engine.get_index("residual-page").unwrap();
    for i in 0..120usize {
        let alphas = "alpha ".repeat(1 + i % 5);
        let mut doc = json!({ "body": alphas });
        if i % 2 == 0 {
            doc.as_object_mut()
                .unwrap()
                .insert("tag".into(), json!("kept"));
        }
        idx.index_document(Some(format!("d{i:03}")), doc)
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();

    let q = json!({"bool": {
        "must": [{"match": {"body": "alpha"}}],
        "filter": [{"exists": {"field": "tag"}}]
    }});
    let deep = idx.search(&req(q.clone(), 200)).await.unwrap();
    let shallow = idx.search(&req(q.clone(), 1)).await.unwrap();
    assert_eq!(deep.total.value, 60, "residual total wrong at size:200");
    assert_eq!(shallow.total.value, 60, "residual total wrong at size:1");
    assert!(!deep.hits.is_empty() && !shallow.hits.is_empty());
    assert_eq!(
        shallow.hits[0].id, deep.hits[0].id,
        "residual: size:1 and size:200 disagree on the best hit"
    );
    assert!(
        (shallow.hits[0].score - deep.hits[0].score).abs() < 1e-4,
        "residual: top hit scores {} at size:1 but {} at size:200",
        shallow.hits[0].score,
        deep.hits[0].score
    );
}

/// A NON-projectable `must_not` (`exists`) is also skipped from the FTS bool by
/// the projection and re-applied by the same gate — it must still EXCLUDE the
/// docs it names (here: keep only the docs WITHOUT `tag`).
#[tokio::test]
async fn nonprojectable_must_not_still_excludes() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let mut schema = Schema::empty();
    schema
        .fields
        .push(FieldConfig::new("body", FieldType::Text));
    schema
        .fields
        .push(FieldConfig::new("tag", FieldType::Keyword));
    engine.create_index("residual-mustnot", schema).unwrap();
    let idx = engine.get_index("residual-mustnot").unwrap();
    for i in 0..120usize {
        let alphas = "alpha ".repeat(1 + i % 5);
        let mut doc = json!({ "body": alphas });
        if i % 2 == 0 {
            doc.as_object_mut()
                .unwrap()
                .insert("tag".into(), json!("kept"));
        }
        idx.index_document(Some(format!("d{i:03}")), doc)
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();

    let excluded = idx
        .search(&req(
            json!({"bool": {
                "must": [{"match": {"body": "alpha"}}],
                "must_not": [{"exists": {"field": "tag"}}]
            }}),
            200,
        ))
        .await
        .unwrap();
    assert_eq!(
        excluded.total.value, 60,
        "must_not exists:tag must exclude the 60 tagged docs, keeping the 60 untagged"
    );
    for hit in &excluded.hits {
        let n: usize = hit.id[1..].parse().unwrap();
        assert_eq!(
            n % 2,
            1,
            "{} HAS `tag` and must be excluded by must_not",
            hit.id
        );
    }
}
