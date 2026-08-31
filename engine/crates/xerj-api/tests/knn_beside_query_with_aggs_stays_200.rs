//! Issues #458/#825: the ES 8.x hybrid shape is a top-level `knn` beside a
//! `query`, optionally with `aggs`/`sort`/`rescore`/… ES's contract for it:
//! the knn side contributes the global top-k neighbours, the hit set is the
//! UNION of both halves, a document reached by both scores the SUM
//! `query_score + knn_score`, and aggregations run over the union.
//!
//! History: the original fold to `bool.should` dropped the kNN half entirely
//! (#395); #458 routed the no-extras case to the XERJ-native `hybrid` (RRF)
//! executor and kept the kNN-dropped lexical bool whenever aggs/sort/…
//! were present; #825 retires both halves of that split — the engine now
//! pre-executes the `knn` clause and pins its top-k into the tree, so the
//! `bool.should` fold serves the full ES contract for every request shape.
//!
//! Elasticsearch is referenced for wire semantics only; no ES code is here.

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

async fn json_req(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// `knn` + `query` + `aggs` (faceted hybrid) must return 200 with its facets —
/// not the 400 an unconditional hybrid fold produced — AND (#825) the kNN half
/// must actually contribute: a document reachable only through the vector side
/// appears in the hits, its category shows up in the aggregation buckets
/// (ES computes aggs over the combined knn+query match set), and the two
/// lexical docs are separated by their vector scores instead of answering
/// with identical pure-BM25 scores.
#[tokio::test]
async fn knn_beside_query_with_aggs_stays_200_with_facets() {
    let (app, _dir) = app().await;

    let (st, b) = json_req(
        &app,
        "PUT",
        "/hy",
        json!({ "mappings": { "properties": {
            "body": { "type": "text" },
            "v": { "type": "dense_vector", "dims": 3 },
            "cat": { "type": "keyword" }
        } } }),
    )
    .await;
    assert!(st.is_success(), "create index: {st} {b}");

    // Docs 1 and 2 match the lexical query ("alpha", identical tf and field
    // length, so identical BM25); doc 3 shares no term with it and is
    // reachable ONLY through the vector side (its vector IS the query
    // vector). knn k=3 puts all three in the vector top-k.
    for (id, body, vec, cat) in [
        ("1", "alpha beta", [0.1_f32, 0.2, 0.3], "x"),
        ("2", "alpha gamma", [0.2_f32, 0.1, 0.4], "y"),
        ("3", "zzz qqq", [0.1_f32, 0.2, 0.3], "z"),
    ] {
        let (st, b) = json_req(
            &app,
            "POST",
            &format!("/hy/_doc/{id}"),
            json!({ "body": body, "v": vec, "cat": cat }),
        )
        .await;
        assert!(st.is_success(), "index {id}: {st} {b}");
    }
    let (_s, _b) = json_req(&app, "POST", "/hy/_refresh", json!({})).await;

    // The ES 8.x faceted-hybrid shape: top-level knn + query + aggs.
    let (status, body) = json_req(
        &app,
        "POST",
        "/hy/_search",
        json!({
            "query": { "match": { "body": "alpha" } },
            "knn": { "field": "v", "query_vector": [0.1, 0.2, 0.3], "k": 3, "num_candidates": 10 },
            "aggs": { "by_cat": { "terms": { "field": "cat" } } }
        }),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "#458: knn + query + aggs (faceted hybrid) must stay 200, not regress to 400 — an \
         unconditional hybrid fold hits the executor's 'aggregations are not supported with \
         hybrid/fusion queries' path: {status} {body}"
    );
    let buckets = body
        .pointer("/aggregations/by_cat/buckets")
        .and_then(Value::as_array);
    assert!(
        buckets.is_some_and(|b| !b.is_empty()),
        "the aggregation must be computed and returned, not dropped: {body}"
    );
    // #825: aggs run over the UNION of both halves — the vector-only doc's
    // category must be bucketed even though the lexical query never
    // matches it.
    let bucket_keys: Vec<String> = buckets
        .into_iter()
        .flatten()
        .filter_map(|b| b.get("key").and_then(Value::as_str).map(String::from))
        .collect();
    assert!(
        bucket_keys.iter().any(|k| k == "z"),
        "#825: the aggregation must cover the vector-only document (cat 'z') — \
         aggs are computed over the combined knn+query match set, not the \
         lexical half alone: buckets={bucket_keys:?} body={body}"
    );
    let hits: Vec<(String, f64)> = body
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .map(|hits| {
            hits.iter()
                .filter_map(|h| {
                    h.get("_id")
                        .and_then(Value::as_str)
                        .map(String::from)
                        .zip(h.get("_score").and_then(Value::as_f64))
                })
                .collect()
        })
        .unwrap_or_default();
    assert!(
        hits.iter().any(|(id, _)| id == "3"),
        "#825: the vector-only document must be in the hit union: {hits:?} {body}"
    );
    // Doc 1's vector IS the query vector while doc 2's is not; their BM25
    // halves are identical. With the kNN half contributing, doc 1 must
    // outrank doc 2 — under the old silent drop both answered the same
    // pure-BM25 score.
    let score = |wanted: &str| {
        hits.iter()
            .filter(|(id, _)| id == wanted)
            .map(|(_, s)| *s)
            .next()
    };
    let (s1, s2) = (score("1"), score("2"));
    assert!(
        s1.zip(s2).is_some_and(|(a, b)| a > b),
        "#825: the vector contribution must separate the lexically-tied docs \
         (doc 1 carries the exact query vector): s1={s1:?} s2={s2:?} {body}"
    );
}

/// The core contract: `knn` beside a `query` must return the UNION of both
/// halves — a document reachable only via the vector side must appear — and a
/// document reached by BOTH halves scores the SUM `query_score + knn_score`
/// (#825, ES semantics), so the lexical+vector doc outranks the vector-only
/// doc even though the latter carries the stronger vector match (its vector
/// IS the query vector). The original fold to a two-clause `bool.should` never
/// dispatched the kNN half at all; the #458 interim fix served this shape with
/// RRF rank fusion instead of ES's score sum.
#[tokio::test]
async fn knn_beside_query_returns_the_vector_only_document() {
    let (app, _dir) = app().await;

    let (st, b) = json_req(
        &app,
        "PUT",
        "/u",
        json!({ "mappings": { "properties": {
            "body": { "type": "text" },
            "v": { "type": "dense_vector", "dims": 3 }
        } } }),
    )
    .await;
    assert!(st.is_success(), "create index: {st} {b}");

    // doc "lex" matches the lexical query; doc "vec" does NOT (its body shares no
    // term) but its vector is the query vector, so only the kNN half reaches it.
    for (id, body, vec) in [
        ("lex", "alpha beta", [1.0_f32, 0.0, 0.0]),
        ("vec", "zzz qqq", [0.1_f32, 0.2, 0.3]),
    ] {
        let (st, b) = json_req(
            &app,
            "POST",
            &format!("/u/_doc/{id}"),
            json!({ "body": body, "v": vec }),
        )
        .await;
        assert!(st.is_success(), "index {id}: {st} {b}");
    }
    let (_s, _b) = json_req(&app, "POST", "/u/_refresh", json!({})).await;

    let (status, body) = json_req(
        &app,
        "POST",
        "/u/_search",
        json!({
            "query": { "match": { "body": "alpha" } },
            "knn": { "field": "v", "query_vector": [0.1, 0.2, 0.3], "k": 2, "num_candidates": 10 }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "_search status: {status} {body}");

    let ids: Vec<String> = body
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .map(|hits| {
            hits.iter()
                .filter_map(|h| h.get("_id").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        ids.iter().any(|id| id == "vec"),
        "#458: knn beside query dropped the vector-only document — the kNN half was \
         never dispatched. hits={ids:?} body={body}"
    );
    assert!(
        ids.iter().any(|id| id == "lex"),
        "the lexical match must still be present: hits={ids:?} body={body}"
    );
    // #825: summed scores, not rank fusion. Both docs are in the vector
    // top-k (k=2 of 2), so "lex" scores BM25 + its (weaker) vector score
    // while "vec" scores its (perfect) vector match alone — the sum must
    // put "lex" first. Under RRF both halves' ranks fused to ~1/61-scale
    // micro-scores instead of the ES score sum.
    assert_eq!(
        ids.first().map(String::as_str),
        Some("lex"),
        "#825: the doc matching both halves must outrank the vector-only doc \
         (score = query_score + knn_score): hits={ids:?} body={body}"
    );
}

/// #825: a feature the fusion executor could not serve (here `rescore`) no
/// longer costs the request its vector half. The engine pins the knn top-k
/// into the `bool.should` tree, so the vector-only document appears in the
/// union AND rescore is honoured on the normal path. Under #458 this exact
/// shape silently dropped the kNN contribution (this test used to assert the
/// vector doc was ABSENT — that was the documented cost of keeping rescore).
#[tokio::test]
async fn knn_beside_query_with_rescore_keeps_the_vector_contribution() {
    let (app, _dir) = app().await;

    let (st, b) = json_req(
        &app,
        "PUT",
        "/r",
        json!({ "mappings": { "properties": {
            "body": { "type": "text" },
            "v": { "type": "dense_vector", "dims": 3 }
        } } }),
    )
    .await;
    assert!(st.is_success(), "create index: {st} {b}");
    for (id, body, vec) in [
        ("lex", "alpha beta", [1.0_f32, 0.0, 0.0]),
        ("vec", "zzz qqq", [0.1_f32, 0.2, 0.3]),
    ] {
        let (st, b) = json_req(
            &app,
            "POST",
            &format!("/r/_doc/{id}"),
            json!({ "body": body, "v": vec }),
        )
        .await;
        assert!(st.is_success(), "index {id}: {st} {b}");
    }
    let (_s, _b) = json_req(&app, "POST", "/r/_refresh", json!({})).await;

    let (status, body) = json_req(
        &app,
        "POST",
        "/r/_search",
        json!({
            "query": { "match": { "body": "alpha" } },
            "knn": { "field": "v", "query_vector": [0.1, 0.2, 0.3], "k": 2, "num_candidates": 10 },
            "rescore": { "window_size": 10, "query": { "rescore_query": { "match": { "body": "alpha" } } } }
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "knn+query+rescore status: {status} {body}"
    );

    let ids: Vec<String> = body
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .map(|hits| {
            hits.iter()
                .filter_map(|h| h.get("_id").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        ids.iter().any(|id| id == "vec"),
        "#825: knn+query+rescore must serve the UNION — the vector-only doc must appear \
         even with rescore present (the old bool.should fallback silently dropped the \
         kNN half for every rescore-bearing request): hits={ids:?} body={body}"
    );
    assert!(
        ids.iter().any(|id| id == "lex"),
        "the lexical (and rescored) match must still be present: hits={ids:?} body={body}"
    );
}

/// #825 (review follow-up): the summed-score contract must also hold when the
/// lexical half is itself a `bool` carrying two or more scoring text clauses —
/// the shape that reaches the post-scan IDF heuristic rescore, whose gate
/// `query_uses_bool_text` counts the user's inner bool and is blind to the
/// pinned kNN sub-tree. That pass rewrites `hit.score` from term frequencies
/// alone, so run blind it discards the vector contribution for exactly the
/// documents reached by BOTH halves — the same silent drop #825 exists to
/// close, one layer down. A bare `match`/`term` query half returns
/// text_children = 1 and never reaches it, which is why every other test here
/// misses this.
///
/// The two documents carry identical lexical content (identical tf and field
/// lengths, so identical BM25 *and* identical heuristic scores) and differ
/// only in their vector: the one whose vector IS the query vector must rank
/// strictly above the other. `near` is indexed SECOND, so if the scores
/// collapsed to a tie the arrival-order tie-break would put it last.
#[tokio::test]
async fn knn_beside_bool_query_keeps_the_vector_contribution() {
    let (app, _dir) = app().await;

    let (st, b) = json_req(
        &app,
        "PUT",
        "/bq",
        json!({ "mappings": { "properties": {
            "title": { "type": "text" },
            "body": { "type": "text" },
            "v": { "type": "dense_vector", "dims": 3 }
        } } }),
    )
    .await;
    assert!(st.is_success(), "create index: {st} {b}");

    for (id, vec) in [("far", [1.0_f32, 0.0, 0.0]), ("near", [0.1_f32, 0.2, 0.3])] {
        let (st, b) = json_req(
            &app,
            "POST",
            &format!("/bq/_doc/{id}"),
            json!({ "title": "alpha", "body": "beta", "v": vec }),
        )
        .await;
        assert!(st.is_success(), "index {id}: {st} {b}");
    }
    let (_s, _b) = json_req(&app, "POST", "/bq/_refresh", json!({})).await;

    let (status, body) = json_req(
        &app,
        "POST",
        "/bq/_search",
        json!({
            "query": { "bool": { "must": [
                { "match": { "title": "alpha" } },
                { "match": { "body": "beta" } }
            ] } },
            "knn": { "field": "v", "query_vector": [0.1, 0.2, 0.3], "k": 2, "num_candidates": 10 }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "_search status: {status} {body}");

    let hits: Vec<(String, f64)> = body
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .map(|hits| {
            hits.iter()
                .filter_map(|h| {
                    h.get("_id")
                        .and_then(Value::as_str)
                        .map(String::from)
                        .zip(h.get("_score").and_then(Value::as_f64))
                })
                .collect()
        })
        .unwrap_or_default();
    let score = |wanted: &str| {
        hits.iter()
            .filter(|(id, _)| id == wanted)
            .map(|(_, s)| *s)
            .next()
    };
    let (near, far) = (score("near"), score("far"));
    assert!(
        near.zip(far).is_some_and(|(n, f)| n > f),
        "#825: with a multi-clause bool as the lexical half the kNN score must still be \
         summed in — the post-scan IDF rescore must not overwrite the pinned vector \
         contribution: near={near:?} far={far:?} {body}"
    );
    assert_eq!(
        hits.first().map(|(id, _)| id.as_str()),
        Some("near"),
        "#825: the better vector match must lead the page: hits={hits:?} {body}"
    );
}
