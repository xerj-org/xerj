//! Regression test for #1303 — a request carrying BOTH a `query` and a `knn`
//! clause must keep returning the documented Elasticsearch hybrid ranking
//! (per-hit SUM of the two scores) EVEN on an index that has seen an
//! overwrite/delete.
//!
//! ## The bug
//!
//! The ES-compat layer folds a top-level `knn` beside a `query` into
//! `bool{should:[query, knn]}`, and the engine pre-executes the knn leg and
//! splices its top-k back as a `Bool{should:[Constant{score, Ids}…],
//! filter:[Ids]}` sub-tree (#825). #892 then made that sub-tree project to an
//! inverted-index `DocScores` leaf so the whole compound serves the union
//! through the normal FTS path with exact BM25 on the lexical half and the
//! vector scores summed in — BUT it gated that projection on
//! `!deletes_present`. On any index that had ever taken an overwrite or
//! delete (`ghost_events() > 0`, monotonic and global — the common case for a
//! re-indexed corpus) the pinned `Ids` sub-tree could not project, the WHOLE
//! hybrid query `?`-aborted the projection, and every segment fell to the
//! stored-doc scan. That scan scores a `multi_match` with a crude
//! page/heuristic score, not index-wide BM25, so the hybrid returned a flat,
//! id-ordered ranking that belonged to NEITHER half — exactly #1303's
//! "`_score` ~4.4 matching neither the BM25 range nor the cosine range, with
//! unrelated documents at the top".
//!
//! The bug is INVISIBLE on a 3-document probe (the reporter's note): with a
//! handful of docs every lexical match shares the same page-local score, so
//! the flat heuristic happens to agree with BM25. It only shows once the
//! corpus is large enough that index-wide BM25 differs from the page. This
//! test therefore uses a few thousand documents.
//!
//! ## What is asserted
//!
//! On a 3000-doc index that has been overwritten (so `deletes_present`):
//!   1. the lexical half keeps EXACT index-wide BM25 — every lexical-only hit
//!      in the hybrid carries the same `_score` it has in the pure-lexical
//!      query, not a flat constant;
//!   2. the one document reached by BOTH halves scores the SUM
//!      (`bm25 + knn`) and leads the page;
//!   3. the live (post-overwrite) `_source` is served, never a stale ghost.
//!
//! FAIL-BEFORE: with the `!deletes_present` gate on `pinned_probe` restored,
//! the hybrid page is flat ~1.0 id-ordered and assertion (1) reports the
//! lexical hits scored ~1.0 instead of their BM25 value.

use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::{Engine, Index};
use xerj_query::parse_request;

const N: usize = 3000;
/// The top vector neighbour, also made a lexical match → reached by BOTH halves.
const BOTH: usize = 2999;

fn body(i: usize) -> String {
    // The lexical probe "zebra" lands on every 50th doc AND on the top vector
    // neighbour. Identical text everywhere → identical (tied) BM25, so the
    // ordering is deterministic regardless of shard/segment topology (#188).
    if i.is_multiple_of(50) || i == BOTH {
        "zebra lexical probe".to_string()
    } else {
        "common filler text".to_string()
    }
}

fn vec_for(i: usize) -> Vec<f32> {
    // Smooth distinct fan [t, 1-t]; query [1,0] → the highest index is the
    // nearest neighbour.
    let t = i as f32 / N as f32;
    vec![t, 1.0 - t]
}

async fn page(idx: &std::sync::Arc<Index>, b: &serde_json::Value) -> Vec<(String, f32)> {
    let req = parse_request(b).unwrap();
    let res = idx.search(&req).await.unwrap();
    res.hits.iter().map(|h| (h.id.clone(), h.score)).collect()
}

#[tokio::test]
async fn hybrid_knn_query_keeps_bm25_scoring_under_deletes() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config).unwrap();

    let mut schema = Schema::empty();
    schema
        .add_field(FieldConfig::new("body", FieldType::Text))
        .unwrap();
    let mut v = FieldConfig::new("vec", FieldType::Vector);
    v.options.dimensions = Some(2);
    schema.add_field(v).unwrap();
    engine.create_index("hy1303", schema).unwrap();
    let idx = engine.get_index("hy1303").unwrap();

    for i in 0..N {
        idx.index_document(
            Some(format!("d{i}")),
            json!({ "body": body(i), "vec": vec_for(i) }),
        )
        .await
        .unwrap();
    }
    idx.flush().await.unwrap();

    // Overwrite a handful of documents (including the both-halves neighbour,
    // tagged with a stored marker) so `ghost_events() > 0` and the index is
    // `deletes_present` — the condition under which #1303 bit. The overwrite
    // keeps `body` byte-identical, so BM25 is unchanged.
    for i in [50usize, 100, 150, 200] {
        idx.index_document(
            Some(format!("d{i}")),
            json!({ "body": body(i), "vec": vec_for(i) }),
        )
        .await
        .unwrap();
    }
    idx.index_document(
        Some(format!("d{BOTH}")),
        json!({ "body": body(BOTH), "vec": vec_for(BOTH), "marker": "live" }),
    )
    .await
    .unwrap();
    idx.flush().await.unwrap();

    // Pure-lexical reference: every "zebra" doc shares one tied BM25 score.
    let lex = json!({
        "size": 10, "track_total_hits": true,
        "query": {"bool": {"should": [
            {"multi_match": {"query": "zebra", "fields": ["body"]}}
        ]}}
    });
    // kNN leg alone (lexical clause matches nothing) → the vector score of the
    // top neighbour.
    let knn_only = json!({
        "size": 10, "track_total_hits": true,
        "query": {"bool": {"should": [
            {"multi_match": {"query": "zzzznomatch", "fields": ["body"]}},
            {"knn": {"field": "vec", "query_vector": [1.0, 0.0], "k": 5, "num_candidates": 500}}
        ]}}
    });
    // The hybrid: the folded shape the ES-compat layer produces for
    // `query` + `knn`.
    let hybrid = json!({
        "size": 10, "track_total_hits": true,
        "query": {"bool": {"should": [
            {"multi_match": {"query": "zebra", "fields": ["body"]}},
            {"knn": {"field": "vec", "query_vector": [1.0, 0.0], "k": 5, "num_candidates": 500}}
        ]}}
    });

    let lex_page = page(&idx, &lex).await;
    let knn_page = page(&idx, &knn_only).await;
    let hybrid_page = page(&idx, &hybrid).await;

    // The tied BM25 score of a "zebra" document (all equal — identical text).
    let bm25 = lex_page[0].1;
    assert!(
        bm25 > 3.0,
        "sanity: index-wide BM25 for the probe term should be well above the \
         ~1.0 knn constant, got {bm25} (lex={lex_page:?})"
    );
    assert!(
        lex_page.iter().all(|(_, s)| (*s - bm25).abs() < 1e-4),
        "sanity: the identical-text lexical matches must all tie: {lex_page:?}"
    );

    // The top vector neighbour and its score.
    assert_eq!(
        knn_page[0].0,
        format!("d{BOTH}"),
        "sanity: d{BOTH} must be the nearest neighbour: {knn_page:?}"
    );
    let knn_score = knn_page[0].1;
    assert!(
        (0.9..=1.0001).contains(&knn_score),
        "sanity: cosine of the nearest neighbour should be ~1.0, got {knn_score}"
    );

    // ── (2) the both-halves document leads with the SUMMED score ───────────
    assert_eq!(
        hybrid_page[0].0,
        format!("d{BOTH}"),
        "#1303: the document reached by BOTH halves must lead the hybrid page; \
         got {hybrid_page:?}"
    );
    let both_score = hybrid_page[0].1;
    assert!(
        (both_score - (bm25 + knn_score)).abs() < 1e-3,
        "#1303: the both-halves document must score the SUM bm25 ({bm25}) + knn \
         ({knn_score}) = {}, got {both_score} (hybrid={hybrid_page:?})",
        bm25 + knn_score
    );

    // ── (1) the lexical half keeps EXACT index-wide BM25 ───────────────────
    // Every lexical-only hit in the hybrid page (everything after the
    // both-halves leader) must carry the SAME BM25 score as the pure-lexical
    // query — NOT the flat ~1.0 stored-scan heuristic the bug produced.
    let lexical_tail: Vec<&(String, f32)> = hybrid_page
        .iter()
        .filter(|(id, _)| *id != format!("d{BOTH}"))
        .collect();
    assert!(
        lexical_tail.len() >= 5,
        "expected several lexical-only hits in the hybrid page: {hybrid_page:?}"
    );
    for (id, score) in &lexical_tail {
        assert!(
            (*score - bm25).abs() < 1e-4,
            "#1303: lexical hit {id} in the hybrid scored {score}, but its \
             index-wide BM25 is {bm25}; the knn clause must not rewrite the \
             lexical half (this is the flat-heuristic regression — the hybrid \
             fell to the stored-doc scan). hybrid={hybrid_page:?}"
        );
    }

    // ── (3) the LIVE (post-overwrite) source is served, never a ghost ──────
    let req = parse_request(&hybrid).unwrap();
    let res = idx.search(&req).await.unwrap();
    let leader = &res.hits[0];
    assert_eq!(leader.id, format!("d{BOTH}"));
    assert_eq!(
        leader.source.get("marker").and_then(|m| m.as_str()),
        Some("live"),
        "#1303: the pinned-knn route under deletes must serve the LIVE copy of \
         d{BOTH}, not a superseded segment ghost; source={:?}",
        leader.source
    );
}
