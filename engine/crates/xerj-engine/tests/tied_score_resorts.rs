//! Regression tests for #270 (part 2) — every post-sort re-sort in
//! `Index::search` must break score ties with the SAME total order as the
//! main page sort: `(score DESC, seq_no ASC, _id ASC)`.
//!
//! The main sort decorates each hit with its `seq_no` (arrival order — the
//! `_doc` analogue, see `HitQueue.lessThan` breaking equal scores by the
//! smaller doc id in `lucene/core/src/java/org/apache/lucene/search/
//! HitQueue.java:76-82`) so an all-tied page comes back in arrival order.
//! Five later re-sorts used to tie by `_id` ALONE — the IDF-weighted rescore
//! for bool-text queries, the TF-IDF fallback for near-zero BM25 scores, and
//! three in the `request.rescore` path — so any of them firing on a tied hit
//! set silently reordered the page into `_id` ASC, which for UUID-shaped ids
//! is exactly the "essentially random" ordering the main sort comment says
//! was replaced.
//!
//! The fixtures below make the misordering visible with readable ids: the
//! documents written first are named `seg…` and the later ones `mem…`, so
//! `_id` ASC ("m" < "s") puts every LATER document ahead of every earlier one
//! — the exact inversion measured in #270 (`mem0000…mem0599` before
//! `seg0000…seg0039`).
//!
//! **#361 note.** Four of the five re-sorts are still here.  The fifth — the
//! bool-text IDF rescore's — is gone: #361 removed the pass itself, because it
//! derived its `N`/`df` from the returned page and so made `_score` a function
//! of `size`.  `bool_text_all_tied_page_keeps_arrival_order` below now
//! certifies the same *property* (an all-tied multi-clause bool page comes back
//! in arrival order) through the main page sort instead, and it additionally
//! asserts bounded pages are prefixes of the full one, which the old
//! rescore-era fixture could not.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::Engine;
use xerj_query::ast::{RescoreQuery, RescoreQueryInner};
use xerj_query::parse_request;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

fn req(from: usize, size: usize, q: &Value) -> xerj_query::ast::SearchRequest {
    parse_request(&json!({"from": from, "size": size, "query": q})).expect("parse_request")
}

fn page_ids(hits: &[xerj_query::Hit]) -> Vec<String> {
    hits.iter().map(|h| h.id.clone()).collect()
}

/// `flushed` byte-identical documents flushed to segments, then `unflushed`
/// more left in the memtable.  Segment ids sort AFTER memtable ids in `_id`
/// order but arrived first, so any `_id`-only tie-break inverts the page.
async fn mixed_corpus(
    idx: &std::sync::Arc<xerj_engine::index::Index>,
    flushed: usize,
    unflushed: usize,
) -> Vec<String> {
    let mut inserted = Vec::new();
    for i in 0..flushed {
        let id = format!("seg{i:04}");
        idx.index_document(
            Some(id.clone()),
            json!({"body": "listpack encoding", "kind": "same"}),
        )
        .await
        .unwrap();
        inserted.push(id);
    }
    idx.flush().await.unwrap();
    for i in 0..unflushed {
        let id = format!("mem{i:04}");
        idx.index_document(
            Some(id.clone()),
            json!({"body": "listpack encoding", "kind": "same"}),
        )
        .await
        .unwrap();
        inserted.push(id);
    }
    inserted
}

/// Same two batches as [`mixed_corpus`], but the second one is flushed too, so
/// both live in SEGMENTS and every document is scored by the same arm.  Ids
/// still invert under `_id` ASC ("mem…" before "seg…"), so an `_id`-only
/// tie-break is still visible — what goes away is the memtable/segment
/// first-pass scoring divergence, which would otherwise stop the page tying at
/// all (see `bool_text_all_tied_page_keeps_arrival_order`).
async fn two_segment_corpus(
    idx: &std::sync::Arc<xerj_engine::index::Index>,
    first: usize,
    second: usize,
) -> Vec<String> {
    let inserted = mixed_corpus(idx, first, second).await;
    idx.flush().await.unwrap();
    inserted
}

fn assert_tied_and_in_arrival_order(
    label: &str,
    hits: &[xerj_query::Hit],
    inserted: &[String],
    total: u64,
    reported_total: u64,
) {
    assert_eq!(reported_total, total, "{label}: every doc matches");
    let scores: Vec<f32> = hits.iter().map(|h| h.score).collect();
    assert!(
        scores.windows(2).all(|w| w[0].to_bits() == w[1].to_bits()),
        "{label}: fixture must produce an EXACT tie, got {:?}…",
        &scores[..scores.len().min(5)]
    );
    assert_eq!(
        page_ids(hits),
        inserted,
        "{label}: an all-tied page must come back in arrival order \
         (segment documents before the later memtable ones), not `_id` ASC"
    );
}

/// #270's own reproduction: 40 flushed + 600 unflushed identical documents,
/// plain `match`.  With ~640 identical documents the BM25 IDF collapses to
/// `ln(1 + 0.5/(N+0.5))` ≈ 0.0008, the max page score drops under the 0.001
/// threshold, and the TF-IDF fallback fires — its recomputed score is
/// `tf.sqrt() * (1 + ln(N/df))` = exactly 1.0 for identical docs (the
/// measured bit pattern 1065353216), and its re-sort used to tie by `_id`
/// alone, returning `mem0000…mem0599` before `seg0000…seg0039`.
#[tokio::test]
async fn tfidf_fallback_resort_keeps_arrival_order() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("tfidf", Schema::empty()).unwrap();
    let idx = engine.get_index("tfidf").unwrap();
    let inserted = mixed_corpus(&idx, 40, 600).await;

    let q = json!({"match": {"body": "listpack"}});
    let full = idx.search(&req(0, 1000, &q)).await.unwrap();
    assert_tied_and_in_arrival_order("match/tfidf", &full.hits, &inserted, 640, full.total.value);

    // Bounded pages must stay prefixes of the full page through the re-sort.
    for size in [1usize, 5, 41, 100] {
        let p = idx.search(&req(0, size, &q)).await.unwrap();
        assert_eq!(
            page_ids(&p.hits),
            inserted[..size],
            "match/tfidf: size:{size} page disagrees with the full materialisation"
        );
    }
}

/// A Bool with two text clauses over 340 byte-identical documents: every
/// clause has `df == N`, so both clause scores are equal for every document
/// and the whole page ties exactly (measured: `0.002934686`, which is exactly
/// twice the single-clause `0.001467343`).  The corpus stays SMALL enough that
/// the TF-IDF fallback (`max_score < 0.001`) does not fire — this isolates the
/// bool page sort.
///
/// The two batches land in two SEGMENTS rather than one segment plus the
/// memtable, and that choice is load-bearing.  A `bool` over a mixed corpus
/// does not tie at all: the memtable arm falls to the IDF-less stored-doc
/// scorer while the segments score BM25, so identical documents come back
/// `1.6931472` (memtable) against `0.0122700855` (segment) — every unflushed
/// document outranking every flushed one by 138×.  That is reproducible on
/// unmodified `main` with a ONE-clause bool, which never reached the removed
/// #361 pass, so it is a pre-existing first-pass divergence between the
/// memtable and segment scorers (#188's remit) and not something #361 caused
/// or can fix.  A tie-break test must not be built on a fixture that does not
/// tie, hence two segments; the divergence itself needs its own issue and its
/// own fix.
#[tokio::test]
async fn bool_text_all_tied_page_keeps_arrival_order() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("booltext", Schema::empty()).unwrap();
    let idx = engine.get_index("booltext").unwrap();
    let inserted = two_segment_corpus(&idx, 40, 300).await;

    let q = json!({"bool": {"should": [
        {"match": {"body": "listpack"}},
        {"match": {"body": "encoding"}}
    ]}});
    let full = idx.search(&req(0, 1000, &q)).await.unwrap();
    assert_tied_and_in_arrival_order("bool-text", &full.hits, &inserted, 340, full.total.value);

    // #361: bounded pages must be prefixes of the full materialisation — the
    // invariant the page-derived rescore broke.  The rescore-era fixture could
    // not assert this at all.
    for size in [1usize, 5, 41, 100] {
        let p = idx.search(&req(0, size, &q)).await.unwrap();
        assert_eq!(
            page_ids(&p.hits),
            inserted[..size],
            "bool-text: size:{size} page disagrees with the full materialisation"
        );
    }
}

/// The three re-sorts in the `request.rescore` path (before the first stage,
/// between chained stages, and after the last).  A `match_all` primary with a
/// uniform rescore query keeps every score tied through every stage, so all
/// three sorts see an all-tied hit set; each used to tie by `_id` alone.
/// Two chained stages make sure the between-stages sort runs too.
#[tokio::test]
async fn rescore_path_resorts_keep_arrival_order() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("rescore", Schema::empty()).unwrap();
    let idx = engine.get_index("rescore").unwrap();
    let inserted = mixed_corpus(&idx, 40, 300).await;

    let stage = RescoreQuery {
        window_size: 1000,
        query: Some(RescoreQueryInner {
            rescore_query: xerj_query::parse_query(&json!({"term": {"kind": "same"}}))
                .expect("parse rescore query"),
            query_weight: 1.0,
            rescore_query_weight: 1.0,
        }),
        script: None,
    };
    let mut request = req(0, 1000, &json!({"match_all": {}}));
    request.rescore = vec![stage.clone(), stage];

    let full = idx.search(&request).await.unwrap();
    assert_tied_and_in_arrival_order("rescore", &full.hits, &inserted, 340, full.total.value);

    for size in [1usize, 5, 41, 100] {
        let mut bounded = req(0, size, &json!({"match_all": {}}));
        bounded.rescore = request.rescore.clone();
        let p = idx.search(&bounded).await.unwrap();
        assert_eq!(
            page_ids(&p.hits),
            inserted[..size],
            "rescore: size:{size} page disagrees with the full materialisation"
        );
    }
}
