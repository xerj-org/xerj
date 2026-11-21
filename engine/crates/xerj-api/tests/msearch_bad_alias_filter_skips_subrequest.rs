//! Review follow-up on #3185 (fixes #3065): a member alias filter that fails
//! to parse must fail its whole `_msearch` / `_msearch/template` sub-request.
//!
//! The `continue` in the `Err` arm of the per-member filter parse loop only
//! continued that inner loop, so the error response was pushed and then the
//! sub-request still ran without the member's filter: one extra entry in
//! `responses` (every later item shifted by one) and documents the alias
//! exists to hide were returned unfiltered.
//!
//! These go through the real HTTP routes with a bad filter in the middle of
//! the batch: the middle item must answer with its error only, and the item
//! after it must still line up with its own request.

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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn ndjson(app: &axum::Router, path: &str, body: String) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/x-ndjson")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// t1 holds docs 0 and 1, t2 holds doc 2 (`cat: a`) and doc 3 (`cat: c`).
/// Alias `ali` covers both members, but t2's filter is malformed: an
/// unknown query type, so it fails at parse time. The filter is stored as
/// raw metadata (alias creation does not parse it), so the failure only
/// surfaces when a search parses it.
async fn seed_with_bad_filter(app: &axum::Router) {
    for member in ["t1", "t2"] {
        let (st, _) = call(
            app,
            "PUT",
            &format!("/{member}"),
            json!({"mappings": {"properties": {"v": {"type": "long"}, "cat": {"type": "keyword"}}}}),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "create {member}");
    }
    for (id, doc) in [
        ("0", json!({"v": 0})),
        ("1", json!({"v": 1})),
        ("2", json!({"v": 2, "cat": "a"})),
        ("3", json!({"v": 3, "cat": "c"})),
    ] {
        let member = if id < "2" { "t1" } else { "t2" };
        let (st, _) = call(app, "POST", &format!("/{member}/_doc/{id}"), doc).await;
        assert_eq!(st, StatusCode::CREATED, "index doc {id} into {member}");
    }
    for member in ["t1", "t2"] {
        let (st, _) = call(app, "POST", &format!("/{member}/_refresh"), Value::Null).await;
        assert_eq!(st, StatusCode::OK, "refresh {member}");
    }
    let (st, body) = call(
        app,
        "POST",
        "/_aliases",
        json!({"actions": [
            {"add": {"index": "t1", "alias": "ali"}},
            {"add": {"index": "t2", "alias": "ali", "filter": {"bogus_query": {"cat": "c"}}}}
        ]}),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::OK,
        "create alias ali with bad filter: {body}"
    );
}

fn response_ids(body: &Value, idx: usize) -> Vec<String> {
    let mut ids: Vec<String> = body
        .pointer(&format!("/responses/{idx}/hits/hits"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|h| h["_id"].as_str().map(str::to_string))
        .collect();
    ids.sort();
    ids
}

/// The bad filter sits in the middle item: the batch must still answer
/// exactly three responses, the middle one an error, the last one the
/// third item's own result (docs 0 and 1 from t1), not the unfiltered
/// alias run.
#[tokio::test]
async fn msearch_bad_member_filter_fails_only_its_own_item() {
    let (app, _dir) = app().await;
    seed_with_bad_filter(&app).await;

    let nd = [
        json!({ "index": "t1" }).to_string(),
        json!({ "query": { "match_all": {} }, "size": 10 }).to_string(),
        json!({ "index": "ali" }).to_string(),
        json!({ "query": { "match_all": {} }, "size": 10 }).to_string(),
        json!({ "index": "t1" }).to_string(),
        json!({ "query": { "match_all": {} }, "size": 10 }).to_string(),
    ]
    .join("\n")
        + "\n";
    let (status, body) = ndjson(&app, "/_msearch", nd).await;
    assert_eq!(status, StatusCode::OK, "_msearch status: {body}");

    let responses = body
        .get("responses")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        responses.len(),
        3,
        "a failed filter parse must fail its item, not add a second response for it: {body}"
    );
    assert_eq!(
        response_ids(&body, 0),
        vec!["0".to_string(), "1".to_string()],
        "first item searches t1 directly: {body}"
    );
    assert!(
        body.pointer("/responses/1/error").is_some(),
        "middle item must answer with the filter parse error: {body}"
    );
    assert_eq!(
        body.pointer("/responses/1/status").and_then(Value::as_u64),
        Some(400),
        "middle item error must carry the 400 the single-request path answers: {body}"
    );
    assert_eq!(
        response_ids(&body, 2),
        vec!["0".to_string(), "1".to_string()],
        "last item must still line up with its own request, not with the unfiltered alias run: {body}"
    );
    assert!(
        !response_ids(&body, 2)
            .iter()
            .any(|id| id == "2" || id == "3"),
        "no response may leak t2 documents the alias filter exists to hide: {body}"
    );
}

/// Same shape on `_msearch/template`: the rendered template search through
/// the alias with the bad filter must fail its item only.
#[tokio::test]
async fn msearch_template_bad_member_filter_fails_only_its_own_item() {
    let (app, _dir) = app().await;
    seed_with_bad_filter(&app).await;

    let item = json!({ "source": "{\"query\":{\"match_all\":{}},\"size\":10}", "params": {} });
    let nd = [
        json!({ "index": "t1" }).to_string(),
        item.to_string(),
        json!({ "index": "ali" }).to_string(),
        item.to_string(),
        json!({ "index": "t1" }).to_string(),
        item.to_string(),
    ]
    .join("\n")
        + "\n";
    let (status, body) = ndjson(&app, "/_msearch/template", nd).await;
    assert_eq!(status, StatusCode::OK, "_msearch/template status: {body}");

    let responses = body
        .get("responses")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        responses.len(),
        3,
        "a failed filter parse must fail its template item, not add a second response for it: {body}"
    );
    assert_eq!(
        response_ids(&body, 0),
        vec!["0".to_string(), "1".to_string()],
        "first item searches t1 directly: {body}"
    );
    assert!(
        body.pointer("/responses/1/error").is_some(),
        "middle item must answer with the filter parse error: {body}"
    );
    assert_eq!(
        response_ids(&body, 2),
        vec!["0".to_string(), "1".to_string()],
        "last item must still line up with its own request: {body}"
    );
}
