//! `_validate/query` must answer the same "will this build?" question as
//! `_search`.
//!
//! It used to parse the body and never open the index, so a query naming an
//! unsearchable field or a non-numeric value on a numeric field was reported
//! `valid: true` and then 400ed at search time (the "GAP 5" recorded in
//! `index_false_is_honoured.rs`). Both surfaces now call
//! `Index::validate_query`, which runs the same
//! `check_buildable_query` the search path builds with: alias and
//! keyword-full-text rewrites, then the unsearchable-field (#204) and
//! non-numeric-value (#1284) checks.
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

async fn validate(app: &axum::Router, query: Value) -> (StatusCode, Value) {
    json_req(
        app,
        "POST",
        "/vq/_validate/query",
        json!({ "query": query }),
    )
    .await
}

/// The refusal sentence from either response shape: a validate body carries
/// `error` as a string, a search body carries `error.root_cause[0].reason`.
fn refusal(body: &Value) -> &str {
    body.pointer("/error/root_cause/0/reason")
        .and_then(Value::as_str)
        .or_else(|| body.get("error").and_then(Value::as_str))
        .unwrap_or_default()
}

#[tokio::test]
async fn validate_query_matches_search_for_schema_refusals() {
    let (app, _dir) = app().await;
    let (status, body) = json_req(
        &app,
        "PUT",
        "/vq",
        json!({
            "mappings": { "properties": {
                "n":    { "type": "integer" },
                "note": { "type": "text", "index": false },
                "body": { "type": "text" }
            }}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = json_req(
        &app,
        "PUT",
        "/vq/_doc/1",
        json!({"n": 1, "body": "pelicans", "note": "hidden"}),
    )
    .await;
    assert!(status.is_success(), "{body}");

    for (name, query, needle) in [
        (
            "term on an integer with a string",
            json!({"term": {"n": "abc"}}),
            "For input string",
        ),
        (
            "range on an integer with a string bound",
            json!({"range": {"n": {"gte": "abc"}}}),
            "For input string",
        ),
        (
            "match on an unsearchable text field",
            json!({"match": {"note": "pelicans"}}),
            "Cannot search on field [note]",
        ),
    ] {
        let (status, body) = validate(&app, query.clone()).await;
        assert_eq!(status, StatusCode::OK, "{name}: {body}");
        assert_eq!(body["valid"].as_bool(), Some(false), "{name}: {body}");
        assert!(refusal(&body).contains(needle), "{name}: {body}");

        let (status, body) = json_req(&app, "POST", "/vq/_search", json!({ "query": query })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {body}");
        assert!(refusal(&body).contains(needle), "{name} search: {body}");
    }

    for query in [
        json!({"term": {"n": 1}}),
        json!({"match": {"body": "pelicans"}}),
    ] {
        let (status, body) = validate(&app, query.clone()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["valid"].as_bool(), Some(true), "{body}");
        let (status, body) = json_req(&app, "POST", "/vq/_search", json!({ "query": query })).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    // `explain=true` names the refusing index instead of one top-level
    // sentence, so a mixed multi-index validation is actionable.
    let (status, body) = json_req(
        &app,
        "POST",
        "/vq/_validate/query?explain=true",
        json!({"query": {"term": {"n": "abc"}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["valid"].as_bool(), Some(false), "{body}");
    assert_eq!(body["explanations"][0]["index"], "vq");
    assert_eq!(body["explanations"][0]["valid"].as_bool(), Some(false));
    assert!(
        body["explanations"][0]["error"]
            .as_str()
            .unwrap_or_default()
            .contains("For input string"),
        "{body}"
    );
}
