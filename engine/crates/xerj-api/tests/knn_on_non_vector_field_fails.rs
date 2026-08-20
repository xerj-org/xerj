//! Issue #498: a `knn` / `semantic` clause that names a field which is absent,
//! or is not a vector field, must fail loudly with a 400 — not return an empty,
//! successful 200. An agent that issues a vector query against an index built
//! without vectors otherwise gets zero hits and concludes "no relevant results
//! exist", when the truth is "this index cannot answer vector queries at all".
//!
//! The second test is the false-positive guard: a `knn` against a genuine
//! `dense_vector` field must NOT be rejected, even when the index is empty (an
//! empty result there is a legitimate "nothing matched").
//!
//! Elasticsearch is referenced for wire semantics only; no ES code is
//! reproduced here.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

fn config_for(dir: &std::path::Path) -> xerj_common::config::Config {
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config
}

fn app_over(dir: &std::path::Path) -> axum::Router {
    let config = config_for(dir);
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    xerj_api::router::build_es_compat_router(state)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(req).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

fn json_req(method: &str, path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

#[tokio::test]
async fn knn_on_an_absent_field_fails_with_400_not_empty_200() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_over(dir.path());

    // A text-only index — no `dense_vector` field anywhere.
    let (st, body) = send(
        &app,
        json_req(
            "PUT",
            "/docs",
            json!({ "mappings": { "properties": { "body": { "type": "text" } } } }),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create index: {body}");

    let (st, _) = send(
        &app,
        json_req(
            "POST",
            "/docs/_doc?refresh=true",
            json!({ "body": "hello world" }),
        ),
    )
    .await;
    assert!(st.is_success(), "index a document: {st}");

    // A `knn` naming a field that does not exist must 400, not answer 200/0.
    let (st, body) = send(
        &app,
        json_req(
            "POST",
            "/docs/_search",
            json!({ "knn": { "field": "embedding", "query_vector": [0.1, 0.2, 0.3],
                             "k": 5, "num_candidates": 50 } }),
        ),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "knn on an absent field must fail loudly, not return an empty 200: {body}"
    );
    let reason = body
        .pointer("/error/reason")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        reason.contains("embedding"),
        "the 400 must name the offending field: {body}"
    );
}

#[tokio::test]
async fn knn_on_a_real_dense_vector_field_is_not_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_over(dir.path());

    let (st, body) = send(
        &app,
        json_req(
            "PUT",
            "/vecs",
            json!({ "mappings": { "properties": {
                "vec": { "type": "dense_vector", "dims": 3 } } } }),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create dense_vector index: {body}");

    // No documents ingested: an empty result here is a legitimate "nothing
    // matched", NOT a reason to reject the query. The guard must fire only on
    // fields that cannot answer a vector clause at all.
    let (st, body) = send(
        &app,
        json_req(
            "POST",
            "/vecs/_search",
            json!({ "knn": { "field": "vec", "query_vector": [0.1, 0.2, 0.3],
                             "k": 5, "num_candidates": 50 } }),
        ),
    )
    .await;
    assert_ne!(
        st,
        StatusCode::BAD_REQUEST,
        "a knn on a genuine dense_vector field must not be rejected: {body}"
    );
}
