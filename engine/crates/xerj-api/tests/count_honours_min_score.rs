//! `POST /{index}/_count` honours the `min_score` field of its body.
//!
//! It rebuilds a `size: 0` search body from `query` alone, so the field used to
//! be dropped: `_search` with `min_score` returned 0 while `_count` with the
//! same body returned every match — a wrong count, not a slower one. The
//! single-index and multi-index/alias paths both carry it now, and a body with
//! `min_score` but no `query` runs `match_all` through the scoring path because
//! match_all scores 1.0 (a threshold above it counts zero).
//!
//! Elasticsearch is referenced for semantics only. It is AGPL-3.0/SSPL-1.0/
//! Elastic-2.0 licensed and no code from it is reproduced here.

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

async fn search_total(app: &axum::Router, index: &str, body: &Value) -> u64 {
    let (status, resp) = json_req(app, "POST", &format!("/{index}/_search"), body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    resp["hits"]["total"]["value"].as_u64().unwrap_or(0)
}

async fn count(app: &axum::Router, index: &str, body: &Value) -> u64 {
    let (status, resp) = json_req(app, "POST", &format!("/{index}/_count"), body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    resp["count"].as_u64().unwrap_or(0)
}

#[tokio::test]
async fn count_honours_min_score_on_single_and_multi_index_selectors() {
    let (app, _dir) = app().await;
    for (index, docs) in [
        (
            "c1",
            vec![
                json!({"t": "alpha", "k": "a"}),
                json!({"t": "alpha alpha alpha", "k": "b"}),
                json!({"t": "beta", "k": "c"}),
            ],
        ),
        ("c2", vec![json!({"t": "alpha", "k": "a"})]),
    ] {
        let (status, body) = json_req(
            &app,
            "PUT",
            &format!("/{index}"),
            json!({"mappings": {"properties": {"t": {"type": "text"}, "k": {"type": "keyword"}}}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        for (i, doc) in docs.into_iter().enumerate() {
            let (status, body) =
                json_req(&app, "PUT", &format!("/{index}/_doc/{}", i + 1), doc).await;
            assert!(status.is_success(), "{body}");
        }
        let (status, body) =
            json_req(&app, "POST", &format!("/{index}/_refresh"), Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    // Single index: the same body must give the same answer from both
    // endpoints, including the zero case that exposed the bug.
    for query in [
        json!({"query": {"match": {"t": "alpha"}}, "min_score": 10.0}),
        json!({"query": {"match": {"t": "alpha"}}, "min_score": 0.1}),
        // No query: match_all scores 1.0, so a threshold above it is zero and
        // one below it keeps every document.
        json!({"min_score": 2.0}),
        json!({"min_score": 0.1}),
    ] {
        assert_eq!(
            search_total(&app, "c1", &query).await,
            count(&app, "c1", &query).await,
            "single-index parity for {query}"
        );
    }
    assert_eq!(
        count(
            &app,
            "c1",
            &json!({"query": {"match": {"t": "alpha"}}, "min_score": 10.0})
        )
        .await,
        0,
        "min_score above every score must count zero"
    );

    // Comma selector: the multi-index path must carry the same threshold.
    for query in [
        json!({"query": {"match": {"t": "alpha"}}, "min_score": 10.0}),
        json!({"query": {"match": {"t": "alpha"}}, "min_score": 0.1}),
        json!({"min_score": 2.0}),
        json!({"min_score": 0.1}),
    ] {
        assert_eq!(
            search_total(&app, "c1,c2", &query).await,
            count(&app, "c1,c2", &query).await,
            "multi-index parity for {query}"
        );
    }
    assert_eq!(
        count(&app, "c1,c2", &json!({"min_score": 0.1})).await,
        4,
        "c1 has 3 docs, c2 has 1, and match_all scores 1.0"
    );

    // A non-numeric min_score is the same 400 `_search` gives, not a silently
    // ignored filter.
    for endpoint in ["_search", "_count"] {
        let (status, body) = json_req(
            &app,
            "POST",
            &format!("/c1/{endpoint}"),
            json!({"query": {"match_all": {}}, "min_score": "x"}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{endpoint}: {body}");
    }
}
