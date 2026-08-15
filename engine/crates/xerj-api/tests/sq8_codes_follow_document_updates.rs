//! A `quantization: "scalar8"` field must score the vector the document
//! CURRENTLY holds — issue #371.
//!
//! Before the fix, the per-field SQ8 code store was populated write-once on
//! the first kNN that observed a document (`index.rs`, `if
//! !store.codes.contains_key(id)`). Nothing invalidated an entry, so once a
//! document had been scored its codes were frozen: overwrite its vector with
//! the exact negation and the quantized index kept returning it at cosine
//! 1.000000, with scores byte-identical to the query before the update, while
//! the identical unquantized index correctly dropped it. Wrong results,
//! silently, through a documented ES mapping option (`int8_hnsw`).
//!
//! These tests pin the observable from the outside, over real HTTP, in the
//! shape the issue reported it:
//!
//!  * a `PUT /{index}/_doc/{id}` that negates a vector must move that
//!    document out of the top of the result list on the `scalar8` index, not
//!    just on the full-precision control;
//!  * the same through `_bulk`, which is how every realistic re-embedding
//!    pipeline writes.
//!
//! The full-precision index is carried through every step as a control: it
//! establishes that the manipulation itself is sound, so a failure here
//! cannot be a test artifact. Both indices are asserted to agree on the
//! ranking *before* the update, which is also what proves the SQ8 store was
//! populated at all — a store that was never consulted could not go stale.
//!
//! Elasticsearch is referenced for wire semantics only. It is AGPL-3.0/
//! SSPL-1.0/Elastic-2.0 licensed and no code from it is reproduced here.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

const DIM: usize = 8;
const DOCS: usize = 20;

/// The one document the tests move. `[1,0,0,...]` before, its exact negation
/// `[-1,0,0,...]` after — true cosine 1.0 then -1.0 against the query, the
/// widest possible swing, so no threshold in here is delicate.
const MOVED: &str = "0";

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

/// The two indices under test: the quantized one and its full-precision
/// control, identical in every other respect (8-dim, default `cosine`).
const SQ8: &str = "sq8";
const EXACT: &str = "exact";

async fn create_pair(app: &axum::Router, sq8_field: Value) {
    for (index, field_def) in [
        (SQ8, sq8_field),
        (EXACT, json!({ "type": "dense_vector", "dims": DIM })),
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

/// The document that takes the top slot once doc 0 is negated. Its cosine
/// against the query is 0.9939 against 0.4918 for the next filler doc, so the
/// post-update winner is unambiguous and SQ8's approximation error cannot
/// reorder it.
const RUNNER_UP: &str = "2";

/// Deterministic corpus.
///
///  * doc `0` is the query vector itself — cosine 1.0, the top hit;
///  * doc `1` is its negation. That also anchors the fitted SQ8 range for
///    dimension 0 at exactly -1, so doc 0's post-update code is representable
///    rather than clamped, and its decoded score is exactly 0.0;
///  * doc `2` is the clear runner-up (see [`RUNNER_UP`]);
///  * the rest are spread over `[-1,1]` by a prime-stride walk, all distinct
///    and all well below doc 2.
fn corpus_vector(i: usize) -> Vec<f32> {
    match i {
        0 => first_dim(1.0),
        1 => first_dim(-1.0),
        2 => {
            let mut v = vec![0.0f32; DIM];
            v[0] = 0.9;
            v[1] = 0.1;
            v
        }
        _ => (0..DIM)
            .map(|d| ((i * 37 + d * 101) % 199) as f32 / 99.0 - 1.0)
            .collect(),
    }
}

fn first_dim(x: f32) -> Vec<f32> {
    let mut v = vec![0.0f32; DIM];
    v[0] = x;
    v
}

fn negated() -> Vec<f32> {
    corpus_vector(0).iter().map(|x| -x).collect()
}

async fn index_corpus(app: &axum::Router, index: &str) {
    for i in 0..DOCS {
        let (status, body) = json_req(
            app,
            "PUT",
            &format!("/{index}/_doc/{i}"),
            json!({ "v": corpus_vector(i) }),
        )
        .await;
        assert!(status.is_success(), "index {index}/{i}: {status} {body}");
    }
    refresh(app, index).await;
}

async fn refresh(app: &axum::Router, index: &str) {
    let (status, body) = json_req(app, "POST", &format!("/{index}/_refresh"), json!({})).await;
    assert!(status.is_success(), "refresh {index}: {status} {body}");
}

/// `(id, score)` for the whole corpus, in hit order.
async fn knn(app: &axum::Router, index: &str) -> Vec<(String, f64)> {
    let (status, body) = json_req(
        app,
        "POST",
        &format!("/{index}/_search"),
        json!({
            "knn": {
                "field": "v",
                "query_vector": corpus_vector(0),
                "k": DOCS,
                "num_candidates": 100
            },
            // `size` defaults to 10 and caps the hit list independently of `k`.
            "size": DOCS,
            "_source": false
        }),
    )
    .await;
    assert!(status.is_success(), "knn on {index}: {status} {body}");
    let hits = body["hits"]["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("no hits on {index}: {body}"));
    assert_eq!(hits.len(), DOCS, "expected the whole corpus back: {body}");
    hits.iter()
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

/// Assert the corpus is in its pre-update shape on `index`: doc 0 is the query
/// vector, so it is the single best hit at cosine 1.0 (`_score` (1+cos)/2 = 1).
/// This is also what populates the SQ8 code store — without it there would be
/// no cached entry to go stale and the tests below would pass vacuously.
fn assert_top_before(index: &str, hits: &[(String, f64)]) {
    assert_eq!(
        hits[0].0, MOVED,
        "{index}: doc {MOVED} IS the query vector, it must rank first: {hits:?}"
    );
    assert!(
        hits[0].1 > 0.99,
        "{index}: doc {MOVED} must score ~1.0 before the update, got {}: {hits:?}",
        hits[0].1
    );
}

/// Assert the update landed in `_source` — the issue's step 3. If this fails
/// the write never happened and nothing below would mean anything.
async fn assert_source_negated(app: &axum::Router, index: &str) {
    let (status, body) = get(app, &format!("/{index}/_doc/{MOVED}")).await;
    assert!(status.is_success(), "get {index}/{MOVED}: {status} {body}");
    let stored: Vec<f64> = body["_source"]["v"]
        .as_array()
        .unwrap_or_else(|| panic!("no _source.v on {index}: {body}"))
        .iter()
        .map(|v| v.as_f64().expect("float"))
        .collect();
    let want: Vec<f64> = negated().iter().map(|x| *x as f64).collect();
    assert_eq!(stored, want, "{index}: _source did not take the update");
}

/// The assertion the bug fails: after the update, doc 0 holds the exact
/// negation of the query, so it must be scored on that vector.
fn assert_moved_out(index: &str, before: &[(String, f64)], after: &[(String, f64)]) {
    assert_ne!(
        after[0].0, MOVED,
        "{index}: doc {MOVED} now holds the NEGATION of the query vector and \
         must not be the top hit — it is being scored from a stale SQ8 code \
         (#371): {after:?}"
    );
    assert_eq!(
        after[0].0, RUNNER_UP,
        "{index}: doc {RUNNER_UP} (cosine 0.9939) must take the top slot once \
         doc {MOVED} is negated: {after:?}"
    );
    let moved = score_of(after, MOVED);
    assert!(
        moved < 0.1,
        "{index}: doc {MOVED} is the exact negation of the query (true cosine \
         -1, _score 0.0) but scored {moved}: {after:?}"
    );
    let before_scores: Vec<f64> = before.iter().map(|(_, s)| *s).collect();
    let after_scores: Vec<f64> = after.iter().map(|(_, s)| *s).collect();
    assert_ne!(
        before_scores, after_scores,
        "{index}: the score list is unchanged by a vector rewrite — the \
         ranking is frozen at first observation (#371)"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. `PUT /{index}/_doc/{id}` — the shape reported in #371
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sq8_scores_follow_a_document_update() {
    let (app, _dir) = app().await;
    create_pair(
        &app,
        json!({ "type": "dense_vector", "dims": DIM, "quantization": "scalar8" }),
    )
    .await;

    index_corpus(&app, SQ8).await;
    index_corpus(&app, EXACT).await;

    // Step 1 — query once, which is what populates the SQ8 code store.
    let sq8_before = knn(&app, SQ8).await;
    let exact_before = knn(&app, EXACT).await;
    assert_top_before(SQ8, &sq8_before);
    assert_top_before(EXACT, &exact_before);

    // Step 2/3 — overwrite doc 0 with its exact negation, and confirm it took.
    for index in [SQ8, EXACT] {
        let (status, body) = json_req(
            &app,
            "PUT",
            &format!("/{index}/_doc/{MOVED}"),
            json!({ "v": negated() }),
        )
        .await;
        assert!(status.is_success(), "update {index}: {status} {body}");
        refresh(&app, index).await;
        assert_source_negated(&app, index).await;
    }

    // Step 4 — the same query again.
    let sq8_after = knn(&app, SQ8).await;
    let exact_after = knn(&app, EXACT).await;

    // The control first: if the full-precision index does not move the
    // document either, the manipulation is wrong and the SQ8 failure below
    // would be meaningless.
    assert_moved_out(EXACT, &exact_before, &exact_after);
    assert_moved_out(SQ8, &sq8_before, &sq8_after);

    // The quantized index must not merely have moved doc 0 — it must land on
    // the same ordering the full-precision index does, to within SQ8 error.
    let sq8_order: Vec<&str> = sq8_after.iter().map(|(id, _)| id.as_str()).collect();
    let exact_order: Vec<&str> = exact_after.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        sq8_order[..3],
        exact_order[..3],
        "quantized and exact indices disagree on the post-update top 3: \
         {sq8_after:?} vs {exact_after:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. `_bulk` — the path a re-embedding pipeline actually writes through
// ─────────────────────────────────────────────────────────────────────────────

/// #371 was reported against `PUT /_doc/{id}` and explicitly did not test
/// `_bulk`. `_bulk` is how vectors are rewritten in bulk after a model change,
/// which is the workload that makes this a blocker, so it gets its own pin.
/// The mapping is spelled `index_options.type: int8_hnsw` here — the
/// Elasticsearch spelling an ES user reaches for by default, and the one that
/// makes the defect reachable without ever typing "scalar8".
#[tokio::test]
async fn sq8_scores_follow_a_bulk_update() {
    let (app, _dir) = app().await;
    create_pair(
        &app,
        json!({
            "type": "dense_vector",
            "dims": DIM,
            "index_options": { "type": "int8_hnsw" }
        }),
    )
    .await;

    index_corpus(&app, SQ8).await;
    index_corpus(&app, EXACT).await;

    let sq8_before = knn(&app, SQ8).await;
    let exact_before = knn(&app, EXACT).await;
    assert_top_before(SQ8, &sq8_before);
    assert_top_before(EXACT, &exact_before);

    for index in [SQ8, EXACT] {
        let body = format!(
            "{}\n{}\n",
            json!({ "index": { "_index": index, "_id": MOVED } }),
            json!({ "v": negated() })
        );
        let (status, response) = ndjson(&app, "/_bulk?refresh=true", body).await;
        assert_eq!(status, StatusCode::OK, "bulk {index}: {response}");
        assert_eq!(
            response["errors"],
            json!(false),
            "bulk {index} reported errors: {response}"
        );
        refresh(&app, index).await;
        assert_source_negated(&app, index).await;
    }

    let sq8_after = knn(&app, SQ8).await;
    let exact_after = knn(&app, EXACT).await;
    assert_moved_out(EXACT, &exact_before, &exact_after);
    assert_moved_out(SQ8, &sq8_before, &sq8_after);
}
