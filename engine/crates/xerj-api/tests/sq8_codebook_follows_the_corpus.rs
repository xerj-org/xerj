//! A `quantization: "scalar8"` field must score the vector the document
//! CURRENTLY holds even when the new vector falls outside the range the
//! codebook was fitted from — issue #371, second half.
//!
//! `sq8_codes_follow_document_updates.rs` pins the per-document half: codes
//! must not be cached write-once per document id. This file pins the CODEBOOK
//! half, which is the same defect one level up. The per-dimension min/scale
//! pair (`Sq8Params`) was fitted from the first ≤1000 candidate vectors the
//! field was ever scanned with and then kept for the life of the process. A
//! vector written afterwards that falls outside that fitted range is clamped
//! into it, and when the fitted range is narrow the clamped decode is
//! indistinguishable from the vector that was there before — so the headline
//! symptom of #371 survives unchanged:
//!
//!  * `codebook_tracks_a_vector_written_outside_the_fitted_range` — 20 docs
//!    whose first dimension is pinned at +1.0, so dimension 0 fits to the
//!    degenerate range [1,1]. Overwrite doc 0 with its exact negation and a
//!    stale codebook decodes -1.0 straight back to +1.0: doc 0 still first at
//!    `_score` 1.000000, whole score list frozen. Verbatim the issue.
//!
//!  * `codebook_tracks_a_whole_corpus_re_embedding` — the workload the issue
//!    names as what makes it a blocker. Fit the codebook on model-A vectors,
//!    bulk-rewrite every document with model-B vectors, query with a model-B
//!    vector. A stale codebook clamps the entire corpus onto a handful of
//!    codes: every document collapses onto one identical score and the exact
//!    match ranks nowhere.
//!
//! Both carry the full-precision index as a control through every step, so a
//! failure here cannot be a test artifact — the control performs the identical
//! manipulation and is asserted first.
//!
//! Elasticsearch is referenced for wire semantics only. It is AGPL-3.0/
//! SSPL-1.0/Elastic-2.0 licensed and no code from it is reproduced here.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

const SQ8: &str = "sq8";
const EXACT: &str = "exact";

async fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(req).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn json_req(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    send(
        app,
        Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request"),
    )
    .await
}

async fn get(app: &axum::Router, path: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::get(path).body(Body::empty()).expect("request"),
    )
    .await
}

async fn ndjson(app: &axum::Router, path: &str, body: String) -> (StatusCode, Value) {
    send(
        app,
        Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/x-ndjson")
            .body(Body::from(body))
            .expect("request"),
    )
    .await
}

/// Create the quantized index and its full-precision control, identical apart
/// from the `quantization` option.
async fn create_pair(app: &axum::Router, dim: usize) {
    for (index, field_def) in [
        (
            SQ8,
            json!({ "type": "dense_vector", "dims": dim, "quantization": "scalar8" }),
        ),
        (EXACT, json!({ "type": "dense_vector", "dims": dim })),
    ] {
        let (status, body) = json_req(
            app,
            "PUT",
            &format!("/{index}"),
            json!({ "mappings": { "properties": { "v": field_def } } }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "create {index}: {body}");
    }
}

async fn refresh(app: &axum::Router, index: &str) {
    let (status, body) = json_req(app, "POST", &format!("/{index}/_refresh"), json!({})).await;
    assert!(status.is_success(), "refresh {index}: {status} {body}");
}

/// Write `vectors` into `index` through `_bulk`, then refresh.
async fn bulk_index(app: &axum::Router, index: &str, vectors: &[Vec<f32>]) {
    let mut body = String::new();
    for (i, v) in vectors.iter().enumerate() {
        body.push_str(&json!({ "index": { "_index": index, "_id": i.to_string() } }).to_string());
        body.push('\n');
        body.push_str(&json!({ "v": v }).to_string());
        body.push('\n');
    }
    let (status, response) = ndjson(app, "/_bulk?refresh=true", body).await;
    assert_eq!(status, StatusCode::OK, "bulk {index}: {response}");
    assert_eq!(
        response["errors"],
        json!(false),
        "bulk {index} reported errors: {response}"
    );
    refresh(app, index).await;
}

/// `(id, score)` for the whole corpus, in hit order.
async fn knn(app: &axum::Router, index: &str, query: &[f32], n: usize) -> Vec<(String, f64)> {
    let (status, body) = json_req(
        app,
        "POST",
        &format!("/{index}/_search"),
        json!({
            "knn": {
                "field": "v",
                "query_vector": query,
                "k": n,
                "num_candidates": n * 4
            },
            "size": n,
            "_source": false
        }),
    )
    .await;
    assert!(status.is_success(), "knn on {index}: {status} {body}");
    body["hits"]["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("no hits on {index}: {body}"))
        .iter()
        .map(|h| {
            (
                h["_id"].as_str().expect("_id").to_string(),
                h["_score"].as_f64().expect("_score"),
            )
        })
        .collect()
}

fn score_of(hits: &[(String, f64)], id: &str) -> f64 {
    hits.iter()
        .find(|(hit_id, _)| hit_id == id)
        .unwrap_or_else(|| panic!("{id} missing from {hits:?}"))
        .1
}

fn rank_of(hits: &[(String, f64)], id: &str) -> usize {
    hits.iter()
        .position(|(hit_id, _)| hit_id == id)
        .unwrap_or_else(|| panic!("{id} missing from {hits:?}"))
}

/// Confirm the write landed in `_source`. If this fails nothing below means
/// anything.
async fn assert_source(app: &axum::Router, index: &str, id: &str, want: &[f32]) {
    let (status, body) = get(app, &format!("/{index}/_doc/{id}")).await;
    assert!(status.is_success(), "get {index}/{id}: {status} {body}");
    let stored: Vec<f64> = body["_source"]["v"]
        .as_array()
        .unwrap_or_else(|| panic!("no _source.v on {index}: {body}"))
        .iter()
        .map(|v| v.as_f64().expect("float"))
        .collect();
    let want: Vec<f64> = want.iter().map(|x| *x as f64).collect();
    assert_eq!(stored, want, "{index}/{id}: _source did not take the write");
}

/// How many distinct `_score` values the hit list carries. A codebook that has
/// clamped the whole corpus collapses this to 1.
fn distinct_scores(hits: &[(String, f64)]) -> usize {
    let mut seen: Vec<u64> = hits.iter().map(|(_, s)| s.to_bits()).collect();
    seen.sort_unstable();
    seen.dedup();
    seen.len()
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. Narrow fitted range: the update falls outside the codebook
// ─────────────────────────────────────────────────────────────────────────────

const NARROW_DIM: usize = 8;
const NARROW_DOCS: usize = 20;

/// Every document pins dimension 0 at +1.0 and varies only in dimension 1, so
/// the fitted range for dimension 0 is the degenerate `[1, 1]`. That is what a
/// real corpus of near-identical embeddings looks like to a per-dimension
/// min/max fit, and it is the case a codebook cannot represent a later `-1`
/// in: it clamps back to `+1`.
fn narrow_vector(i: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; NARROW_DIM];
    v[0] = 1.0;
    v[1] = i as f32 * 0.001;
    v
}

fn narrow_negated() -> Vec<f32> {
    narrow_vector(0).iter().map(|x| -x).collect()
}

#[tokio::test]
async fn codebook_tracks_a_vector_written_outside_the_fitted_range() {
    let (app, _dir) = app().await;
    create_pair(&app, NARROW_DIM).await;

    let corpus: Vec<Vec<f32>> = (0..NARROW_DOCS).map(narrow_vector).collect();
    bulk_index(&app, SQ8, &corpus).await;
    bulk_index(&app, EXACT, &corpus).await;

    let query = narrow_vector(0);

    // Step 1 — query once. This is what fits the codebook, over a corpus whose
    // dimension 0 never leaves +1.0.
    let sq8_before = knn(&app, SQ8, &query, NARROW_DOCS).await;
    let exact_before = knn(&app, EXACT, &query, NARROW_DOCS).await;
    for (index, hits) in [(SQ8, &sq8_before), (EXACT, &exact_before)] {
        assert_eq!(
            hits[0].0, "0",
            "{index}: doc 0 IS the query vector, it must rank first: {hits:?}"
        );
    }

    // Step 2/3 — overwrite doc 0 with its exact negation (true cosine -1) and
    // confirm the write landed.
    let negated = narrow_negated();
    for index in [SQ8, EXACT] {
        let (status, body) = json_req(
            &app,
            "PUT",
            &format!("/{index}/_doc/0"),
            json!({ "v": negated }),
        )
        .await;
        assert!(status.is_success(), "update {index}: {status} {body}");
        refresh(&app, index).await;
        assert_source(&app, index, "0", &negated).await;
    }

    // Step 4 — the same query again.
    let sq8_after = knn(&app, SQ8, &query, NARROW_DOCS).await;
    let exact_after = knn(&app, EXACT, &query, NARROW_DOCS).await;

    // Control first: if the full-precision index does not move doc 0 either,
    // the manipulation is wrong and the SQ8 assertion below is meaningless.
    assert_ne!(
        exact_after[0].0, "0",
        "EXACT control did not move doc 0 — the manipulation is wrong: {exact_after:?}"
    );
    assert!(
        score_of(&exact_after, "0") < 0.1,
        "EXACT control: doc 0 is the exact negation of the query, expected \
         _score 0.0, got {}: {exact_after:?}",
        score_of(&exact_after, "0")
    );

    // The assertion the stale codebook fails.
    assert_ne!(
        sq8_after[0].0, "0",
        "SQ8: doc 0 now holds the NEGATION of the query vector and must not be \
         the top hit. The codebook was fitted while dimension 0 never left \
         +1.0, so -1.0 clamps back to +1.0 and the document is scored on a \
         vector it no longer holds (#371): {sq8_after:?}"
    );
    let before_scores: Vec<f64> = sq8_before.iter().map(|(_, s)| *s).collect();
    let after_scores: Vec<f64> = sq8_after.iter().map(|(_, s)| *s).collect();
    assert_ne!(
        before_scores, after_scores,
        "SQ8: the score list is unchanged by a vector rewrite — the ranking is \
         frozen at first observation (#371): {sq8_after:?}"
    );
    let moved = score_of(&sq8_after, "0");
    assert!(
        moved < 0.1,
        "SQ8: doc 0 is the exact negation of the query (true cosine -1, _score \
         0.0) but scored {moved}: {sq8_after:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. Whole-corpus re-embedding — the workload #371 calls out as the blocker
// ─────────────────────────────────────────────────────────────────────────────

const EMBED_DIM: usize = 16;
const EMBED_DOCS: usize = 50;
/// The document whose model-B vector is the query vector itself.
const TARGET: usize = 9;

/// "model A": a narrow positive cone. Every component sits within 0.7% of 1.0,
/// so after cosine's L2 normalisation each dimension occupies a band about
/// 0.0006 wide around 0.25. Real embedding models are famously anisotropic in
/// exactly this way, and a per-dimension min/max fit over such a corpus is a
/// codebook that can represent almost nothing else.
fn model_a(i: usize) -> Vec<f32> {
    (0..EMBED_DIM)
        .map(|d| 1.0 + ((i * 31 + d * 17) % 64) as f32 / 10_000.0)
        .collect()
}

/// "model B": a different embedding space entirely — components spread over
/// `[-1, 1]`, which is what swapping an embedding model actually does to a
/// corpus. Nothing here is inside model A's fitted range.
fn model_b(i: usize) -> Vec<f32> {
    (0..EMBED_DIM)
        .map(|d| ((i * 53 + d * 29) % 128) as f32 / 63.5 - 1.0)
        .collect()
}

#[tokio::test]
async fn codebook_tracks_a_whole_corpus_re_embedding() {
    let (app, _dir) = app().await;
    create_pair(&app, EMBED_DIM).await;

    let a: Vec<Vec<f32>> = (0..EMBED_DOCS).map(model_a).collect();
    let b: Vec<Vec<f32>> = (0..EMBED_DOCS).map(model_b).collect();

    bulk_index(&app, SQ8, &a).await;
    bulk_index(&app, EXACT, &a).await;

    // Query once under model A. This is what fits the codebook — over vectors
    // that all live in [0, 0.25].
    let warm = model_a(0);
    knn(&app, SQ8, &warm, EMBED_DOCS).await;
    knn(&app, EXACT, &warm, EMBED_DOCS).await;

    // Re-embed: rewrite every document with its model-B vector.
    bulk_index(&app, SQ8, &b).await;
    bulk_index(&app, EXACT, &b).await;
    for index in [SQ8, EXACT] {
        assert_source(&app, index, &TARGET.to_string(), &b[TARGET]).await;
    }

    // Query with a model-B vector whose exact match is doc TARGET.
    let query = model_b(TARGET);
    let sq8 = knn(&app, SQ8, &query, EMBED_DOCS).await;
    let exact = knn(&app, EXACT, &query, EMBED_DOCS).await;

    // Control first.
    assert_eq!(
        exact[0].0,
        TARGET.to_string(),
        "EXACT control: doc {TARGET} IS the query vector and must rank first: {exact:?}"
    );

    // A codebook clamped to model A's [0, 0.25] band maps the whole re-embedded
    // corpus onto the same handful of codes, which collapses every score onto
    // one value and destroys ranking entirely.
    let distinct = distinct_scores(&sq8);
    assert!(
        distinct > EMBED_DOCS / 2,
        "SQ8: {distinct} distinct scores across {EMBED_DOCS} re-embedded \
         documents — the codebook is still fitted to the pre-re-embedding \
         corpus and has clamped them together (#371): {sq8:?}"
    );
    let rank = rank_of(&sq8, &TARGET.to_string());
    assert_eq!(
        rank, 0,
        "SQ8: doc {TARGET} IS the query vector after re-embedding but ranks \
         {rank} — the codebook did not follow the corpus (#371): {sq8:?}"
    );
    // Doc TARGET is the query vector itself, so its `_score` is 1.0 on the
    // control. A codebook fitted to the pre-re-embedding corpus crushes it
    // into the same ~0.59 band as everything else; a codebook fitted to the
    // corpus that is actually there has to put it back near 1.0.
    let target_score = score_of(&sq8, &TARGET.to_string());
    assert!(
        target_score > 0.95,
        "SQ8: doc {TARGET} IS the query vector after re-embedding but scored \
         {target_score} (control: {}) — the codebook did not follow the corpus \
         (#371): {sq8:?}",
        score_of(&exact, &TARGET.to_string())
    );
    // Ranking discrimination, pinned against the control rather than against a
    // threshold picked out of this corpus: the quantized index must agree with
    // the full-precision one on the top of the re-embedded list. Under a stale
    // codebook the SQ8 order here is unrelated to the control's.
    let sq8_order: Vec<&str> = sq8.iter().map(|(id, _)| id.as_str()).collect();
    let exact_order: Vec<&str> = exact.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        sq8_order[..5],
        exact_order[..5],
        "SQ8 and the full-precision control disagree on the post-re-embedding \
         top 5: {sq8:?} vs {exact:?}"
    );
    // And the score range must still span the corpus rather than sit in a
    // clamped band. The control spreads over roughly [0.22, 1.0]; a stale
    // codebook squeezed the whole 50 documents into 0.002.
    let spread = sq8[0].1 - sq8[sq8.len() - 1].1;
    assert!(
        spread > 0.5,
        "SQ8: the whole re-embedded corpus spans only {spread} in _score — \
         the codebook has clamped it into a band (#371): {sq8:?}"
    );
}
