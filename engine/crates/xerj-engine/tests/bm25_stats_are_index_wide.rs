//! Regression tests for #188 — BM25 collection statistics must be
//! INDEX-WIDE (every segment + the memtable), not per scoring arm.
//!
//! ## What #188 was
//!
//! Every arm scored against its own statistics: each segment used its own
//! `FieldStats`, and the memtable used the union over its own shards only.
//! BM25 is a comparison between documents, and it is only meaningful when
//! both were normalised against the same `N`, `avgdl` and `doc_freq`.  Two
//! user-visible consequences fell out of that:
//!
//!   1. **Overwriting a document promoted it to first place.**  An overwrite
//!      moves the live copy into the memtable.  Alone there it scores
//!      `N = 1`, `df = 1`, `dl/avgdl = 1` → `idf = ln(4/3) = 0.2876821` and
//!      `tf_norm = 1.0`: a fixed 0.28768212 that outranks almost any
//!      correctly-normalised segment hit, no matter how long the document
//!      actually is.  That is the failure
//!      `search_bounded_under_ghosts::size5_returns_global_top5_under_ghosts_on_large_matchset`
//!      caught on a 2-core box.
//!   2. **Scores tracked segment TOPOLOGY.**  `engine.ingest_shards` defaults
//!      to `(cpus/2).next_power_of_two()` and a flush drains one segment per
//!      non-empty shard, so the SAME corpus lands in 1 segment on a 2-core
//!      runner and ~16 on a 32-core one.  Before the fix that alone moved
//!      `strong0` from 0.177 to 0.570 — 3.2× — and moved a never-touched
//!      weak doc by 13×.
//!
//! (2) is why (1) reproduced only on 2 cores: with 16 shards the strong docs
//! land in near-single-doc segments whose inflated local scores happen to
//! clear the memtable doc's fixed 0.28768.  One instance of the bug masked
//! the other, so "passes on 32 cores" was a false green and nothing in the
//! suite could see it.
//!
//! The test below is therefore written against the property, not the
//! symptom: **the same corpus and query must produce the same scores
//! whatever the shard/segment topology.**  It needs no `taskset` — it varies
//! `ingest_shards` directly — and it fails on every pair of topologies before
//! the fix.

use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::{Engine, Index};
use xerj_query::parse_request;

fn make_engine(dir: &TempDir, ingest_shards: usize) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.engine.ingest_shards = ingest_shards;
    Engine::new(config).expect("engine::new")
}

/// [`make_engine`] with tiny-segment merging pinned off (#1186's `nomerge`
/// config): the tiered merge otherwise collapses the many small segments the
/// probe-budget test needs, mid-test and timing-dependent.
fn make_engine_nomerge(dir: &TempDir, ingest_shards: usize) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.engine.ingest_shards = ingest_shards;
    config.merge.min_merge_count = 100_000;
    Engine::new(config).expect("engine::new")
}

/// One buried occurrence of the term in a LONG field that grows with `i` —
/// low BM25, strictly decreasing.
fn weak_body(i: usize) -> String {
    let pad = "pad ".repeat(40 + i * 3);
    format!("{pad} quicklist {pad}")
}

fn match_body(size: usize) -> xerj_query::ast::SearchRequest {
    parse_request(&json!({
        "size": size,
        "query": {"match": {"body": "quicklist"}}
    }))
    .expect("parse_request")
}

fn page(hits: &[xerj_query::Hit]) -> Vec<(String, f32)> {
    hits.iter().map(|h| (h.id.clone(), h.score)).collect()
}

/// 60 weak docs in one flush + 5 strong docs in another, then an overwrite so
/// one live copy sits in the memtable while everything else is segment-resident.
async fn build_corpus(idx: &Arc<Index>) {
    for i in 0..60 {
        idx.index_document(Some(format!("weak{i:03}")), json!({"body": weak_body(i)}))
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();
    for i in 0..5 {
        let body = "quicklist ".repeat(10 - i) + &"filler ".repeat(i * 4);
        idx.index_document(Some(format!("strong{i}")), json!({"body": body}))
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();
    // The #188 trigger: an overwrite with IDENTICAL content.  Nothing about
    // the document changes — only WHERE its live copy lives.
    idx.index_document(Some("weak001".into()), json!({"body": weak_body(1)}))
        .await
        .unwrap();
}

/// The gate that defines "fixed": `_score` is a function of the index, not of
/// how many shards happened to flush.
#[tokio::test]
async fn scores_are_identical_across_shard_topologies() {
    let mut baseline: Option<(usize, Vec<(String, f32)>)> = None;

    for shards in [1usize, 2, 4, 16] {
        let dir = TempDir::new().unwrap();
        let engine = make_engine(&dir, shards);
        engine.create_index("t", Schema::empty()).unwrap();
        let idx = engine.get_index("t").unwrap();
        build_corpus(&idx).await;

        let got = page(&idx.search(&match_body(100)).await.unwrap().hits);
        assert!(!got.is_empty(), "shards={shards}: no hits at all");

        match &baseline {
            None => baseline = Some((shards, got)),
            Some((base_shards, base)) => assert_eq!(
                &got, base,
                "ingest_shards={shards} scored differently from ingest_shards={base_shards} \
                 — BM25 statistics are still per-arm, so `_score` depends on segment topology"
            ),
        }
    }
}

/// The reported symptom, stated directly: an overwrite must not change a
/// document's rank.  `weak001` is the second-longest weak body with a single
/// buried occurrence — it belongs near the BOTTOM, and it must land in exactly
/// the same place whether or not its live copy was just moved to the memtable.
#[tokio::test]
async fn overwriting_a_document_does_not_change_its_rank() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir, 4);
    engine.create_index("t", Schema::empty()).unwrap();
    let idx = engine.get_index("t").unwrap();

    for i in 0..60 {
        idx.index_document(Some(format!("weak{i:03}")), json!({"body": weak_body(i)}))
            .await
            .unwrap();
    }
    for i in 0..5 {
        let body = "quicklist ".repeat(10 - i) + &"filler ".repeat(i * 4);
        idx.index_document(Some(format!("strong{i}")), json!({"body": body}))
            .await
            .unwrap();
    }
    idx.flush().await.unwrap();

    let before = page(&idx.search(&match_body(100)).await.unwrap().hits);
    let rank_before = before.iter().position(|(id, _)| id == "weak001").unwrap();
    assert!(
        rank_before > 5,
        "sanity: weak001 is a long field with one buried hit, it must not start in the top 5 \
         (was rank {rank_before}): {:?}",
        &before[..6.min(before.len())]
    );

    // Re-index it with byte-identical content: the ONLY change is that its
    // live copy now lives in the memtable instead of a segment.
    idx.index_document(Some("weak001".into()), json!({"body": weak_body(1)}))
        .await
        .unwrap();

    let after = page(&idx.search(&match_body(100)).await.unwrap().hits);
    let rank_after = after.iter().position(|(id, _)| id == "weak001").unwrap();
    assert_eq!(
        rank_after,
        rank_before,
        "an overwrite with identical content moved weak001 from rank {rank_before} to \
         rank {rank_after} — the memtable arm is scoring against its own statistics \
         (top of page after the overwrite: {:?})",
        &after[..3.min(after.len())]
    );

    // The whole ORDER must be unchanged, not just weak001's slot.
    let ids = |p: &[(String, f32)]| p.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
    assert_eq!(
        ids(&after),
        ids(&before),
        "an overwrite with identical content reordered the page"
    );

    // Absolute scores DO move very slightly, and that is the designed
    // behaviour, not a leak: collection statistics are physical /
    // ghost-inclusive (Lucene counts deleted and superseded documents in
    // docFreq/docCount until a merge purges them), so the overwrite's
    // superseded copy adds one document and its 87 tokens to N and to
    // Σ field length until the next merge.  What must NOT happen is a
    // reordering — pin the drift to something far smaller than the gap
    // between adjacent ranks so a regression to per-arm statistics (which
    // moved weak001 by 28×) cannot hide inside it.
    for ((id_a, s_a), (id_b, s_b)) in after.iter().zip(before.iter()) {
        assert_eq!(id_a, id_b);
        let drift = (s_a - s_b).abs() / s_b.max(f32::MIN_POSITIVE);
        assert!(
            drift < 0.05,
            "{id_a}: score moved {:.1}% ({s_b} → {s_a}) on an identical-content overwrite — \
             more than the ghost-inclusive N can explain",
            drift * 100.0
        );
    }
}

/// #193 item 1 — `min_score` must mean the same thing at `size:0` as at
/// `size:N`.
///
/// The threshold applies to FINAL scores, so a `size:0 + min_score` count
/// (no aggs) must score its matches exactly like the `size:N` page does.
/// Before the fix that shape was classified `count_only`, which (a) closed
/// the index-wide stats gate — any scoring that did happen used PER-ARM
/// statistics, a genuinely different scale since #192 — and (b) skipped
/// materialisation entirely, so the post-collection min_score filter had
/// nothing to subtract and `hits.total` ignored the threshold.  ES's own
/// collector refuses the counting shortcut whenever min_score is set
/// (`QueryPhaseCollector`: collection cannot be shortcut via Weight#count).
#[tokio::test]
async fn min_score_total_is_the_same_at_size0_and_sizen() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir, 4);
    engine.create_index("t", Schema::empty()).unwrap();
    let idx = engine.get_index("t").unwrap();
    build_corpus(&idx).await;

    // Unthresholded page: 65 live matches, the 5 short "strong" docs on top.
    let full = idx.search(&match_body(100)).await.unwrap();
    let scores = page(&full.hits);
    assert!(scores.len() >= 6, "sanity: need at least 6 scored hits");
    let (rank4, rank5) = (scores[4].1, scores[5].1);
    assert!(
        rank4 > rank5,
        "sanity: ranks 4/5 must not tie for a clean threshold ({rank4} vs {rank5})"
    );
    let threshold = f64::from(rank4 + rank5) / 2.0;

    let with_min = |size: usize| {
        let mut req = match_body(size);
        req.min_score = Some(threshold);
        req
    };

    let paged = idx.search(&with_min(100)).await.unwrap();
    assert_eq!(
        paged.total.value, 5,
        "size:100 + min_score must count exactly the 5 docs clearing the threshold"
    );
    assert_eq!(paged.hits.len(), 5);

    let counted = idx.search(&with_min(0)).await.unwrap();
    assert!(counted.hits.is_empty(), "size:0 must render an empty page");
    assert_eq!(
        counted.total.value, paged.total.value,
        "size:0 + min_score counted {} docs where size:100 + min_score counted {} — \
         the count-only path is not applying the threshold against the same \
         (index-wide) scores as the paged path",
        counted.total.value, paged.total.value
    );
}

/// The union must not leak GHOSTS as live hits or lose the delete-aware
/// accounting: statistics stay physical (Lucene counts deleted docs until a
/// merge purges them) while `hits.total` stays live.
#[tokio::test]
async fn deletes_do_not_break_the_union() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir, 4);
    engine.create_index("t", Schema::empty()).unwrap();
    let idx = engine.get_index("t").unwrap();
    build_corpus(&idx).await;

    assert!(idx.delete_document("weak000").await.unwrap());

    let r = idx.search(&match_body(100)).await.unwrap();
    // 60 weak - 1 deleted + 5 strong = 64 live matches.
    assert_eq!(r.total.value, 64, "live total wrong after a delete");
    assert!(
        r.hits.iter().all(|h| h.id != "weak000"),
        "deleted doc leaked into the results"
    );
    assert!(
        r.hits.iter().filter(|h| h.id == "weak001").count() <= 1,
        "overwritten doc returned twice (stale ghost not skipped)"
    );
    // The five short docs are still the true top-5.
    for (i, (id, _)) in r.hits.iter().take(5).map(|h| (&h.id, h.score)).enumerate() {
        assert!(
            id.starts_with("strong"),
            "rank {i} is {id}, expected a strong doc: {:?}",
            page(&r.hits)[..5].to_vec()
        );
    }
}

/// #1186 — padding a query with terms that match NOTHING must return the
/// same hits with the same scores as the unpadded query.
///
/// A term with df=0 contributes 0 to every document's BM25 score, so padding
/// is score-neutral — PROVIDED both queries are scored against the same
/// index-wide statistics. It stopped being neutral the moment the padded
/// query crossed the stats pre-pass probe budget
/// `(fields + pairs) * segments > 4096`: the pre-pass silently returned
/// `None`, every segment fell back to its OWN df/avgdl, and the whole top-5
/// reordered (reported: top hit `1-1:1.6797521` → `8-4:1.6904112`; after
/// `_forcemerge?max_num_segments=1` both queries agreed again — proof the
/// reorder was the stats fallback, not the extra terms).
///
/// The corpus mirrors the reporter's: 8 flush cycles of 20 docs each on a
/// 4-shard engine (a segment per non-empty shard per flush) with merging
/// pinned off. The padded query projects 1 field + 1502 terms over ~32
/// segments ≈ 48k probes — far past the old 4096 combined cap (where the
/// ranking changed) and well inside the split per-term budget (where both
/// queries are scored against the same union).
#[tokio::test]
async fn padding_a_query_with_terms_that_match_nothing_never_reorders_results() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine_nomerge(&dir, 4);
    engine.create_index("t", Schema::empty()).unwrap();
    let idx = engine.get_index("t").unwrap();

    // Per-batch term frequencies differ (batch b carries b `alpha`s and b
    // `beta`s), so per-segment statistics genuinely differ from index-wide
    // ones — a silent fallback MOVES scores, it does not just rescale them.
    for b in 1..=8 {
        for i in 1..=20 {
            let alphas = "alpha ".repeat(b);
            let beta = if i <= b { " beta" } else { "" };
            idx.index_document(
                Some(format!("{b}-{i}")),
                json!({"body": format!("doc {i} batch {b} gamma{alphas}{beta}")}),
            )
            .await
            .unwrap();
        }
        idx.flush().await.unwrap();
    }

    let segments = idx.stats().await.segment_count;
    assert!(
        segments >= 3,
        "sanity: need ≥3 segments for the padded query ({pairs} pairs × \
         {segments}) to cross the old 4096-probe cap; got {segments}",
        pairs = 1 + 1502
    );

    let unpadded = parse_request(&json!({
        "size": 5, "_source": false,
        "query": {"match": {"body": "alpha beta"}}
    }))
    .expect("parse_request");
    let pads = (1..=1500)
        .map(|i| format!("zqx{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let padded = parse_request(&json!({
        "size": 5, "_source": false,
        "query": {"match": {"body": format!("alpha beta {pads}")}}
    }))
    .expect("parse_request");

    let plain = idx.search(&unpadded).await.unwrap();
    let padded_r = idx.search(&padded).await.unwrap();

    assert_eq!(
        plain.total.value, padded_r.total.value,
        "terms that match nothing must not change hits.total"
    );
    let top = |hits: &[xerj_query::Hit]| {
        hits.iter()
            .map(|h| (h.id.clone(), h.score.to_bits()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        top(&plain.hits),
        top(&padded_r.hits),
        "padding the query with 1500 terms that match no document reordered \
         the top-5 or changed a score — the stats pre-pass declined and every \
         segment silently scored against its own df/avgdl (#1186)"
    );
}
