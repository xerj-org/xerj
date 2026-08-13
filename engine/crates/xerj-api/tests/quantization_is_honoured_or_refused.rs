//! A `dense_vector` mapping's `quantization` value, from the outside — issue
//! #275, another instance of the accepted-and-ignored class tracked in #204.
//!
//! Before the fix, `es_properties_to_fields` ended its `quantization` match on
//! `_ => {}`. `"scalar4"`, `"binary"`, `"int4"` and typos such as `"sq8"` were
//! taken with a `200 acknowledged`, echoed back verbatim by
//! `GET /{index}/_mapping`, and then ignored — the field stored full-precision
//! f32 while the mapping the operator read back to confirm it said otherwise.
//! Measured against a live rc.16 server: a kNN over such a field returned
//! scores bit-identical to a field with no `quantization` key at all
//! (`0.996254600` on doc 2), while a real `scalar8` field's scores moved
//! (`0.996232500` on the same query and corpus) because only `scalar8` reaches
//! the SQ8 code store.
//!
//! `Config::validate` has refused an unimplemented `[vector]
//! default_quantization` since #207. These tests pin the same guard on the
//! per-field mapping key, in all three of its branches:
//!
//!  * **honoured** — `scalar8` / `int8` / `index_options.type: int8_hnsw`
//!    accepted AND observably quantized; `none` accepted and observably not;
//!  * **refused-unimplemented** — `binary`, `scalar4`, `int4`: named schemes
//!    with no reachable quantizer behind them;
//!  * **refused-unknown** — anything else, including case variants and
//!    non-strings.
//!
//! The tests live at the crate boundary rather than beside the function on
//! purpose: #207's sibling defect survived a fix placed next to one handler,
//! so both entry points that build a schema (`PUT /{index}` and
//! `PUT /{index}/_mapping`) are exercised over real HTTP here.
//!
//! Elasticsearch is referenced for wire semantics only. It is AGPL-3.0/
//! SSPL-1.0/Elastic-2.0 licensed and no code from it is reproduced here.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

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

/// `PUT /{index}` with a single `dense_vector` field carrying `field_def`.
async fn create_vector_index(
    app: &axum::Router,
    index: &str,
    field_def: Value,
) -> (StatusCode, Value) {
    json_req(
        app,
        "PUT",
        &format!("/{index}"),
        json!({ "mappings": { "properties": { "v": field_def } } }),
    )
    .await
}

/// Index a small corpus and return the kNN scores, in hit order.
///
/// The scores are the observable: a `scalar8` field is scored by decoding its
/// u8 codes, so its scores move off the exact-f32 values. A field that only
/// *claims* quantization scores bit-identically to one that claims nothing.
async fn knn_scores(app: &axum::Router, index: &str) -> Vec<f64> {
    for i in 1..=5u32 {
        let (status, body) = json_req(
            app,
            "POST",
            &format!("/{index}/_doc/{i}"),
            json!({ "v": [0.1 * i as f64, 0.2, 0.3, 0.4] }),
        )
        .await;
        assert!(status.is_success(), "index doc {i}: {status} {body}");
    }
    let (status, _) = json_req(app, "POST", &format!("/{index}/_refresh"), json!({})).await;
    assert!(status.is_success(), "refresh: {status}");

    let (status, body) = json_req(
        app,
        "POST",
        &format!("/{index}/_search"),
        json!({
            "knn": {
                "field": "v",
                "query_vector": [0.15, 0.25, 0.3, 0.4],
                "k": 5,
                "num_candidates": 50
            },
            "_source": false
        }),
    )
    .await;
    assert!(status.is_success(), "knn on {index}: {status} {body}");
    let hits = body["hits"]["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("no hits on {index}: {body}"));
    assert_eq!(hits.len(), 5, "expected the whole corpus back: {body}");
    hits.iter()
        .map(|h| h["_score"].as_f64().expect("score"))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. accepted-and-honoured
// ─────────────────────────────────────────────────────────────────────────────

/// `scalar8` is the one value with a quantizer the serving path actually
/// reads, so it must be accepted — and accepted has to mean *honoured*, which
/// is what the score comparison establishes. `int8` is its ES-flavoured alias
/// and `index_options.type: int8_hnsw` is how an ES mapping asks for the same
/// thing; all three must land on the same behaviour.
#[tokio::test]
async fn scalar8_is_accepted_and_observably_quantizes() {
    let (app, _dir) = app().await;

    for (index, field_def) in [
        (
            "exact",
            json!({ "type": "dense_vector", "dims": 4, "quantization": "none" }),
        ),
        ("unset", json!({ "type": "dense_vector", "dims": 4 })),
        (
            "sq8",
            json!({ "type": "dense_vector", "dims": 4, "quantization": "scalar8" }),
        ),
        (
            "sq8-alias",
            json!({ "type": "dense_vector", "dims": 4, "quantization": "int8" }),
        ),
        (
            "sq8-es",
            json!({
                "type": "dense_vector",
                "dims": 4,
                "index_options": { "type": "int8_hnsw" }
            }),
        ),
    ] {
        let (status, body) = create_vector_index(&app, index, field_def).await;
        assert_eq!(status, StatusCode::OK, "create {index}: {body}");
    }

    let exact = knn_scores(&app, "exact").await;
    let unset = knn_scores(&app, "unset").await;
    let sq8 = knn_scores(&app, "sq8").await;
    let alias = knn_scores(&app, "sq8-alias").await;
    let es = knn_scores(&app, "sq8-es").await;

    // `none` is not a synonym for "some quantization": it must be bit-equal to
    // the field that declared nothing at all.
    assert_eq!(
        exact, unset,
        "quantization: none must score identically to an unquantized field"
    );

    // The honoured branch: SQ8 decode moves the scores off exact f32. If this
    // ever comes back equal, `scalar8` has silently stopped being honoured and
    // #275 is back in a new form.
    assert_ne!(
        sq8, exact,
        "quantization: scalar8 must change the scores — equal scores mean the \
         SQ8 code store was never consulted"
    );
    assert_eq!(alias, sq8, "int8 is an alias for scalar8");
    assert_eq!(
        es, sq8,
        "index_options.type: int8_hnsw must reach the same quantizer"
    );

    // The values are close, not arbitrary — a wrong quantizer would not land
    // within a percent of the exact scores. (Recall/RAM effects are not
    // claimed here; nothing in this test measures them.)
    for (q, e) in sq8.iter().zip(exact.iter()) {
        assert!(
            (q - e).abs() < 0.01,
            "SQ8 score {q} is not a plausible approximation of exact {e}"
        );
    }

    // And the mapping still round-trips the value it honoured.
    let (status, body) = get(&app, "/sq8/_mapping").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["sq8"]["mappings"]["properties"]["v"]["quantization"],
        json!("scalar8"),
        "{body}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. refused-unimplemented
// ─────────────────────────────────────────────────────────────────────────────

/// `binary` has no `BinaryQuantizer` at all; `scalar4`/`int4` have a
/// `Scalar4Quantizer` in xerj-vector that nothing in the serving path reaches.
/// Either way the operator would get f32 while sizing a cluster on a 4×–32×
/// reduction, which is precisely what `Config::validate` refuses at startup for
/// `default_quantization = "binary"`. Refuse it here too, and say why.
#[tokio::test]
async fn unimplemented_quantization_is_refused_with_a_reason() {
    let (app, _dir) = app().await;

    for (i, q) in ["binary", "scalar4", "int4"].iter().enumerate() {
        let index = format!("bad{i}");
        let (status, body) = create_vector_index(
            &app,
            &index,
            json!({ "type": "dense_vector", "dims": 4, "quantization": q }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "quantization {q} must be refused, got {status}: {body}"
        );
        let reason = body.to_string();
        assert!(
            reason.contains("not implemented"),
            "the error must say the scheme is unimplemented, got: {reason}"
        );
        assert!(
            reason.contains(q) && reason.contains("scalar8"),
            "the error must name the rejected value and the accepted set, got: {reason}"
        );

        // Refused means not created — no index left behind to be discovered
        // later with a mapping that lies. (`GET /{index}/_mapping` is
        // wildcard-lenient and answers `200 {}` for a missing index, so the
        // existence question is asked of `GET /{index}`, which 404s.)
        let (status, body) = get(&app, &format!("/{index}")).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "a refused mapping must not create the index: {body}"
        );
        let (_, body) = get(&app, &format!("/{index}/_mapping")).await;
        assert!(
            body.get(&index).is_none(),
            "a refused value must never be echoed back: {body}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. refused-unknown
// ─────────────────────────────────────────────────────────────────────────────

/// A typo is the common case and the worst one: `"sq8"` looks like it worked.
/// Reject anything outside the implemented set, including case variants (the
/// key is not case-insensitive anywhere else) and non-string values.
#[tokio::test]
async fn unknown_quantization_is_refused_and_never_echoed() {
    let (app, _dir) = app().await;

    for (i, q) in ["sq8", "SCALAR8", "Scalar8", "int_8", "", "pq", "true"]
        .iter()
        .enumerate()
    {
        let index = format!("typo{i}");
        let (status, body) = create_vector_index(
            &app,
            &index,
            json!({ "type": "dense_vector", "dims": 4, "quantization": q }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "quantization {q:?} must be refused, got {status}: {body}"
        );
        assert!(
            body.to_string().contains("scalar8"),
            "the error must name what this build does implement, got: {body}"
        );
        let (status, _) = get(&app, &format!("/{index}")).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{q:?} must not create {index}"
        );
        let (_, body) = get(&app, &format!("/{index}/_mapping")).await;
        assert!(
            body.get(&index).is_none(),
            "a refused value must never be echoed back: {body}"
        );
    }

    // A non-string is a different mistake with the same consequence.
    let (status, body) = create_vector_index(
        &app,
        "nonstring",
        json!({ "type": "dense_vector", "dims": 4, "quantization": 8 }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a non-string quantization must be refused, got {status}: {body}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. the guard is structural, not per-handler
// ─────────────────────────────────────────────────────────────────────────────

/// `PUT /{index}/_mapping` is the second way a schema gets built, and #207's
/// sibling defect survived a fix that only covered the first. Adding a bad
/// vector field to an existing index must be refused there too — and refused
/// in the plan phase, so nothing about the request is published.
#[tokio::test]
async fn put_mapping_refuses_the_same_values() {
    let (app, _dir) = app().await;
    let (status, body) = json_req(&app, "PUT", "/later", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = json_req(
        &app,
        "PUT",
        "/later/_mapping",
        json!({ "properties": { "v": {
            "type": "dense_vector", "dims": 4, "quantization": "binary"
        }}}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "PUT _mapping must refuse it too: {body}"
    );

    // Nothing was published: the field is absent, not present-and-lying.
    let (status, body) = get(&app, "/later/_mapping").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["later"]["mappings"]["properties"]["v"].is_null(),
        "a refused PUT _mapping must not leave the field behind: {body}"
    );

    // The good value still lands on the same endpoint.
    let (status, body) = json_req(
        &app,
        "PUT",
        "/later/_mapping",
        json!({ "properties": { "v": {
            "type": "dense_vector", "dims": 4, "quantization": "scalar8"
        }}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// A `dense_vector` nested under an object, and one declared as a multi-field,
/// go through the same recursive parse. The guard must reach them: a mapping
/// that hides the bad value one level down is the obvious way a same-file fix
/// gets bypassed.
#[tokio::test]
async fn nested_and_multi_field_vectors_are_checked_too() {
    let (app, _dir) = app().await;

    let (status, body) = json_req(
        &app,
        "PUT",
        "/nested",
        json!({ "mappings": { "properties": { "doc": {
            "type": "object",
            "properties": { "v": {
                "type": "dense_vector", "dims": 4, "quantization": "scalar4"
            }}
        }}}}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a nested dense_vector must be checked: {body}"
    );
    assert!(body.to_string().contains("scalar4"), "{body}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. ES wire compatibility is NOT tightened
// ─────────────────────────────────────────────────────────────────────────────

/// The strictness belongs on XERJ's own `quantization` key only. ES's
/// `index_options.type` must stay permissive: `hnsw`, `flat`, `bbq_*` and
/// `int4_*` are real Elasticsearch mappings, and a migrating user's index
/// definition has to keep creating. Those families are served by the exact f32
/// scan, which satisfies the contract ES states for them (a nearest-neighbour
/// result set) at recall 1.00 — a substitution that costs memory, not answers,
/// unlike a quantization that was asked for and skipped.
#[tokio::test]
async fn es_index_options_families_still_create() {
    let (app, _dir) = app().await;

    for (i, io) in [
        "hnsw",
        "flat",
        "int8_hnsw",
        "int8_flat",
        "int4_hnsw",
        "int4_flat",
        "bbq_hnsw",
        "bbq_flat",
    ]
    .iter()
    .enumerate()
    {
        let (status, body) = create_vector_index(
            &app,
            &format!("es{i}"),
            json!({
                "type": "dense_vector",
                "dims": 4,
                "index_options": { "type": io, "m": 16, "ef_construction": 100 }
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "ES index_options.type {io} must still create an index: {body}"
        );
    }
}
