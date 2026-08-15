//! Regression tests for #361 — `_score` must be a function of (document,
//! query, index), never of how many hits the caller asked for.
//!
//! ## What #361 was
//!
//! The "IDF-weighted rescore for Bool queries with multiple terms" pass in
//! `Index::search` ran AFTER collection and derived its collection statistics
//! from `final_hits` — the collected page.  On the strictly-safe unsorted page
//! path the collector cap is exactly `from + size` (see the shadowed
//! `materialisation_limit`), so `N` in
//! `idf = ln(1 + (N - df + 0.5) / (df + 0.5))` *was* the requested `size`, and
//! `df` was counted over the same few documents.  The same query against the
//! same corpus therefore produced different `_score`s — and a different
//! ORDER — depending only on `size`.  As reported in #361, on a 75,578-doc
//! index:
//!
//! ```text
//! size=2    OffHeapHnswGraph=0.61739
//! size=5    OffHeapHnswGraph=0.29465
//! size=10   copyGraphStructure=0.23240   <- top hit changed
//! size=50   copyGraphStructure=0.08396
//! ```
//!
//! On the 80-document corpus below, measured against the pre-fix build: top
//! hit `d056` at `size:1` versus `d014` at `size:60`; `d035` scored
//! `0.74377024` at `size:2` and `3.7965584` at `size:60`; a six-page
//! `from`-sweep at `size:10` scored `d042` at `0.91919494` where one `size:60`
//! request scored it `2.6978195`.
//!
//! Consequences: `search_after`/`from` sweeps can duplicate or skip a
//! document (page 2 is not scored on the same basis as page 1), `_score` is
//! not comparable across requests, and RRF / hybrid fusion / any client-side
//! `_score` threshold is built on a moving scale.
//!
//! ## What the reference does
//!
//! Lucene has no post-collection rescoring stage, and its statistics are
//! index-wide by construction:
//!
//!   * `lucene/core/src/java/org/apache/lucene/search/similarities/BM25Similarity.java:177-197`
//!     — `idfExplain` takes `df` from `termStats.docFreq()` and `N` from
//!     `fieldStats.docCount()`, "N, total number of documents with field".
//!     Never the collected page.
//!   * `lucene/core/src/java/org/apache/lucene/search/BooleanWeight.java:49-64`
//!     — each clause weight is built once, up front, from the searcher's
//!     top-level reader context; nothing re-weights the hits afterwards.
//!
//! The tests below are written against the property, not the symptom: two
//! requests differing only in `size` must agree on every score and on the
//! order of their common prefix.

use serde_json::json;
use std::sync::Arc;
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

/// `(id, score)` page fingerprint.
fn page(hits: &[xerj_query::Hit]) -> Vec<(String, f32)> {
    hits.iter().map(|h| (h.id.clone(), h.score)).collect()
}

/// A corpus where the two queried terms have very different document
/// frequencies, so a page-derived IDF and an index-derived IDF disagree
/// loudly: `graph` is common (every doc), `neighbour` is rare (12 docs).
///
/// Flushed, no deletes, empty memtable — that is exactly the shape that takes
/// the `materialisation_limit = from + size` page path, i.e. the one where the
/// collected page IS the statistics population.
async fn build_corpus(idx: &Arc<Index>) {
    for i in 0..80usize {
        let rare = if i % 7 == 0 {
            "neighbour ".repeat(1 + (i % 3))
        } else {
            String::new()
        };
        let title = format!("graph node {i}");
        let body = format!(
            "{rare}graph traversal {} {}",
            "filler ".repeat(3 + i % 5),
            "graph ".repeat(1 + i % 4)
        );
        idx.index_document(
            Some(format!("d{i:03}")),
            json!({"title": title, "body": body}),
        )
        .await
        .unwrap();
    }
    idx.flush().await.unwrap();
}

/// `bool.should` with two text clauses.  The removed pass armed on exactly
/// this shape: its gate (`query_uses_bool_text`) fired at two or more
/// Match/MultiMatch/Term children of a Bool.
fn bool_two_terms(size: usize) -> xerj_query::ast::SearchRequest {
    parse_request(&json!({
        "size": size,
        "query": {"bool": {"should": [
            {"match": {"title": "graph"}},
            {"match": {"body":  "neighbour"}}
        ]}}
    }))
    .expect("parse_request")
}

/// The gate that defines "fixed": a document's `_score` is the same number
/// whatever `size` the caller asked for.
#[tokio::test]
async fn score_of_a_document_does_not_depend_on_size() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("p", Schema::empty()).unwrap();
    let idx = engine.get_index("p").unwrap();
    build_corpus(&idx).await;

    // size=60 is the reference population: every score the smaller pages
    // report must be this number, bit for bit.
    let reference = page(&idx.search(&bool_two_terms(60)).await.unwrap().hits);
    assert!(
        reference.len() >= 20,
        "sanity: need a populated reference page, got {}",
        reference.len()
    );

    for size in [2usize, 5, 10, 25] {
        let got = page(&idx.search(&bool_two_terms(size)).await.unwrap().hits);
        assert_eq!(got.len(), size, "size={size} returned the wrong hit count");
        for (id, score) in &got {
            let want = reference
                .iter()
                .find(|(rid, _)| rid == id)
                .unwrap_or_else(|| panic!("size={size}: {id} is absent from the size=60 page"))
                .1;
            assert_eq!(
                *score, want,
                "size={size}: {id} scored {score} but scored {want} at size=60 — \
                 `_score` is still derived from the returned page, so it is not \
                 comparable across requests (RRF/hybrid fusion, `min_score`, and \
                 any client-side threshold read a moving scale)"
            );
        }
    }
}

/// The pagination invariant stated directly: a smaller page must be the
/// PREFIX of a larger one.  If it is not, a `from`/`search_after` sweep can
/// return the same document twice or skip it entirely.
#[tokio::test]
async fn a_smaller_page_is_a_prefix_of_a_larger_one() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("p", Schema::empty()).unwrap();
    let idx = engine.get_index("p").unwrap();
    build_corpus(&idx).await;

    let reference = page(&idx.search(&bool_two_terms(60)).await.unwrap().hits);

    for size in [1usize, 2, 5, 10, 25] {
        let got = page(&idx.search(&bool_two_terms(size)).await.unwrap().hits);
        assert_eq!(
            got,
            reference[..size].to_vec(),
            "size={size} is not the first {size} hits of the size=60 result — \
             ranking is not stable under pagination"
        );
    }
}

/// The second page must contain the documents the first page did not, with the
/// same scores.  This is the `from`-sweep an agent actually performs.
#[tokio::test]
async fn from_sweep_visits_every_document_exactly_once() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("p", Schema::empty()).unwrap();
    let idx = engine.get_index("p").unwrap();
    build_corpus(&idx).await;

    let reference = page(&idx.search(&bool_two_terms(60)).await.unwrap().hits);

    let mut swept: Vec<(String, f32)> = Vec::new();
    for from in (0..60).step_by(10) {
        let mut req = bool_two_terms(10);
        req.from = from;
        swept.extend(page(&idx.search(&req).await.unwrap().hits));
    }

    let ids: Vec<&String> = swept.iter().map(|(id, _)| id).collect();
    let mut uniq = ids.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(
        uniq.len(),
        ids.len(),
        "a from-sweep in pages of 10 returned a duplicate document"
    );
    assert_eq!(
        swept, reference,
        "the concatenation of six size=10 pages differs from one size=60 page — \
         page N is not scored on the same basis as page 1"
    );
}
