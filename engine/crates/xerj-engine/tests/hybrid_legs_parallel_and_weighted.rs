//! #3018 / #3016 / #3006 — a hybrid request on the multi-thread runtime runs
//! its legs on their own threads, and the fused page is still exactly the
//! weighted RRF of the legs it would have run one after the other.
//!
//! #3018 moved the legs of a hybrid query off the single request thread: on
//! the multi-thread runtime each leg now runs on its own OS thread instead of
//! being polled in turn, because a BM25 scan and a vector scan never await and
//! the old join ran them back to back. Nothing about the answer may change, so
//! this test computes the expected page itself — each leg run as its own
//! search, fused with the documented `weight / (k + rank)` formula and the
//! `score DESC, seq_no ASC, _id ASC` tie order — and requires the hybrid page
//! to match id for id and bit for bit, for equal and unequal leg weights
//! (#3016) and for `k` given as `rank_constant`.
//!
//! The kNN leg runs on a corpus over `HNSW_MIN_DOCS` whose vectors fit the
//! #3006 exact-scan budget, so it is served by the exact scan with page-only
//! hydration; the oracle legs go through the same executor at the size the
//! hybrid asks of them.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::{Engine, Index};
use xerj_query::parse_request;

const N: usize = 1_500;
const DIMS: usize = 8;
/// `per_query_topk` for a page of 10: `max(from + size, 50)`.
const LEG_DEPTH: usize = 50;

/// One leg's page: `(_id, _score, _seq_no)` in rank order.
type LegPage = Vec<(String, f32, Option<u64>)>;

fn vector(i: usize) -> Vec<f32> {
    // Deterministic, well spread, with exact duplicates every 11th document
    // so the vector leg carries real score ties.
    let j = if i.is_multiple_of(11) { 11 } else { i };
    (0..DIMS)
        .map(|d| (((j * 31 + d * 17) % 97) as f32 / 97.0) - 0.5)
        .collect()
}

fn body(i: usize) -> String {
    let words = ["alpha", "beta", "gamma", "delta", "omega"];
    let mut tokens: Vec<&str> = Vec::new();
    for (w, word) in words.iter().enumerate() {
        for _ in 0..((i + w * 3) % 5) {
            tokens.push(word);
        }
    }
    tokens.resize(12, "filler");
    tokens.join(" ")
}

async fn hits(idx: &Arc<Index>, body: &Value) -> LegPage {
    idx.query_cache.clear();
    let res = idx.search(&parse_request(body).unwrap()).await.unwrap();
    res.hits
        .into_iter()
        .map(|h| (h.id, h.score, h.seq_no))
        .collect()
}

/// The documented fusion, computed from the legs' own pages.
fn expected_rrf(legs: &[(LegPage, f32)], k: u32) -> Vec<(String, f32)> {
    let mut acc: HashMap<String, (f32, u64)> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for (leg, weight) in legs {
        for (rank, (id, _, seq_no)) in leg.iter().enumerate() {
            let contrib = weight / (k as f32 + (rank + 1) as f32);
            let entry = acc.entry(id.clone()).or_insert_with(|| {
                order.push(id.clone());
                (0.0, seq_no.unwrap_or(u64::MAX))
            });
            entry.0 += contrib;
        }
    }
    let mut fused: Vec<(String, f32, u64)> = order
        .into_iter()
        .map(|id| {
            let (score, seq_no) = acc[&id];
            (id, score, seq_no)
        })
        .collect();
    fused.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.0.cmp(&b.0))
    });
    fused
        .into_iter()
        .map(|(id, score, _)| (id, score))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_legs_fuse_exactly_like_the_legs_run_alone() {
    let dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config).unwrap();
    let mut schema = Schema::empty();
    schema
        .add_field(FieldConfig::new("body", FieldType::Text))
        .unwrap();
    let mut v = FieldConfig::new("vec", FieldType::Vector);
    v.options.dimensions = Some(DIMS);
    schema.add_field(v).unwrap();
    engine.create_index("hy3018", schema).unwrap();
    let idx = engine.get_index("hy3018").unwrap();
    for i in 0..N {
        idx.index_document(
            Some(format!("d{i:04}")),
            json!({ "body": body(i), "vec": vector(i) }),
        )
        .await
        .unwrap();
    }
    idx.flush().await.unwrap();

    for (q, text) in [
        (3usize, "alpha gamma"),
        (500, "omega"),
        (1_234, "beta delta"),
    ] {
        let qv = vector(q);
        let lexical = json!({"match": {"body": text}});
        let knn = json!({"knn": {"field": "vec", "query_vector": qv, "k": 300}});
        let bm25_leg = hits(&idx, &json!({"query": lexical, "size": LEG_DEPTH})).await;
        let knn_leg = hits(&idx, &json!({"query": knn, "size": LEG_DEPTH})).await;
        assert_eq!(bm25_leg.len(), LEG_DEPTH, "query {q}: lexical leg too thin");
        assert_eq!(knn_leg.len(), LEG_DEPTH, "query {q}: vector leg too thin");

        for (w_bm25, w_knn, fusion, k) in [
            (1.0f32, 1.0f32, json!("rrf"), 60u32),
            (0.25, 1.0, json!({"type": "rrf", "k": 20}), 20),
            (2.0, 0.5, json!({"type": "rrf", "rank_constant": 100}), 100),
        ] {
            let request = json!({
                "size": 10,
                "query": {"hybrid": {
                    "queries": [
                        {"query": lexical, "weight": w_bm25},
                        {"query": knn, "weight": w_knn}
                    ],
                    "fusion": fusion
                }}
            });
            let want = expected_rrf(&[(bm25_leg.clone(), w_bm25), (knn_leg.clone(), w_knn)], k);
            for _ in 0..3 {
                let got = hits(&idx, &request).await;
                let got: Vec<(String, f32)> =
                    got.into_iter().map(|(id, score, _)| (id, score)).collect();
                assert_eq!(
                    got,
                    want[..10].to_vec(),
                    "query {q}, weights ({w_bm25}, {w_knn}), k {k}"
                );
            }
        }
    }
}
