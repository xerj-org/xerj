//! Issue #361: adding ANY second clause to a `bool` — `filter`, a second
//! `must`, or `must_not` — collapses `_score` into a narrow band and reorders
//! the hits. In Elasticsearch, `bool.filter` (and `must_not`) are **non-scoring
//! by definition**: a filter that matches every document must change neither
//! the order nor the scores of the scoring clause. XERJ instead let the filter
//! contribute to `_score`, so a no-op filter flattened a relevance query into
//! an unordered scan (the reference-coding / faceted-search shape).
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

async fn call(app: &axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    let body = if body.is_null() {
        Body::empty()
    } else {
        req = req.header("content-type", "application/json");
        Body::from(body.to_string())
    };
    let response = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Return the ranked list of `(_id, _score)` for a search body.
async fn ranked(app: &axum::Router, query: Value) -> Vec<(String, f64)> {
    let (st, body) = call(app, "POST", "/docs/_search", query).await;
    assert_eq!(st, StatusCode::OK, "_search status: {body}");
    body.pointer("/hits/hits")
        .and_then(Value::as_array)
        .expect("hits array")
        .iter()
        .map(|h| {
            (
                h.get("_id").and_then(Value::as_str).unwrap_or("").to_string(),
                h.get("_score").and_then(Value::as_f64).unwrap_or(f64::NAN),
            )
        })
        .collect()
}

/// A no-op `bool.filter` (matches every doc) must leave a relevance query's
/// order AND scores exactly as they were: the filter is non-scoring.
///
/// WIP (#361): reproduction only. Currently `#[ignore]`d because the fix is a
/// scorer change still in progress — un-ignore it when the fix lands. Proven
/// fail-before on main (546fc225): `match alpha` scores d3=0.2217 bare but
/// d3=2.3863 once a no-op `exists` filter is added, i.e. the filter contributes
/// to `_score`. Do NOT remove the `#[ignore]` until the scorer stops scoring
/// filter/must_not clauses.
#[tokio::test]
#[ignore = "#361 WIP: bool.filter is still scoring; un-ignore when the scorer fix lands"]
async fn a_noop_bool_filter_changes_neither_order_nor_score() {
    let (app, _dir) = app().await;

    let (st, _) = call(
        &app,
        "PUT",
        "/docs",
        json!({"mappings": {"properties": {
            "body": {"type": "text"},
            "name": {"type": "keyword"}
        }}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create index");

    // Distinct term frequencies for "alpha" -> distinct descending scores.
    // Every doc has `name`, so `exists: name` is a whole-corpus no-op filter.
    let docs = [
        ("d3", "alpha alpha alpha beta"),
        ("d2", "alpha alpha gamma delta epsilon"),
        ("d1", "alpha zeta eta theta iota kappa lambda"),
    ];
    for (id, body) in docs {
        let (st, _) = call(
            &app,
            "POST",
            &format!("/docs/_doc/{id}"),
            json!({ "body": body, "name": id }),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "index {id}");
    }
    let (st, _) = call(&app, "POST", "/docs/_refresh", Value::Null).await;
    assert_eq!(st, StatusCode::OK, "refresh");

    // Baseline: the bare relevance query.
    let baseline = ranked(
        &app,
        json!({ "query": { "match": { "body": "alpha" } } }),
    )
    .await;
    assert_eq!(baseline.len(), 3, "baseline should return all 3 docs");
    assert!(
        baseline[0].1 > baseline[1].1 && baseline[1].1 > baseline[2].1,
        "precondition: the bare query must produce three DISTINCT descending \
         scores, else the test can't detect a collapse: {baseline:?}"
    );

    // Same query, plus a no-op filter that matches every document.
    let with_filter = ranked(
        &app,
        json!({ "query": { "bool": {
            "must": [ { "match": { "body": "alpha" } } ],
            "filter": [ { "exists": { "field": "name" } } ]
        }}}),
    )
    .await;

    // ES semantics: the filter is non-scoring, so the ranking is identical.
    let base_ids: Vec<&String> = baseline.iter().map(|(id, _)| id).collect();
    let filt_ids: Vec<&String> = with_filter.iter().map(|(id, _)| id).collect();
    assert_eq!(
        filt_ids, base_ids,
        "#361: a no-op bool.filter REORDERED the hits — bool.filter must be \
         non-scoring. baseline={baseline:?} with_filter={with_filter:?}"
    );
    for ((bid, bscore), (fid, fscore)) in baseline.iter().zip(with_filter.iter()) {
        assert_eq!(bid, fid, "rank mismatch: {baseline:?} vs {with_filter:?}");
        assert!(
            (bscore - fscore).abs() < 1e-4,
            "#361: doc {bid} scored {bscore} bare but {fscore} with a no-op \
             filter — bool.filter contributed to _score (it must not). \
             baseline={baseline:?} with_filter={with_filter:?}"
        );
    }
}
