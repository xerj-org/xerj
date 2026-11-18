//! Issue #1932: `_msearch` did not expand a wildcard index. A header
//! `{"index":"logs-*"}` or the path form `/logs-*/_msearch` answered the item
//! with 404 `index_not_found_exception` for the literal pattern, while
//! `_search` on the same pattern found the documents. The item now resolves
//! its target through the same selector the other read endpoints use.
//!
//! Elasticsearch is referenced for wire semantics only; no ES code is
//! reproduced here.

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

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    ctype: &str,
    body: String,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if !body.is_empty() {
        req = req.header("content-type", ctype);
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(body)).unwrap())
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

fn hit_indices(item: &Value) -> Vec<String> {
    let mut out: Vec<String> = item["hits"]["hits"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|h| h["_index"].as_str().map(str::to_string))
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn msearch_expands_a_wildcard_index_in_the_header_and_the_path() {
    let (app, _dir) = app().await;
    for index in ["logs-app", "logs-web", "metrics-app"] {
        let (st, resp) = send(
            &app,
            "PUT",
            &format!("/{index}"),
            "application/json",
            json!({"mappings": {"properties": {"msg": {"type": "text"}}}}).to_string(),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "create {index}: {resp}");
        let (st, resp) = send(
            &app,
            "PUT",
            &format!("/{index}/_doc/1?refresh=true"),
            "application/json",
            json!({"msg": format!("disk full on {index}")}).to_string(),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "index into {index}: {resp}");
    }

    let query = r#"{"query":{"match":{"msg":"disk"}}}"#;
    let cases = [
        ("/_msearch", format!("{{\"index\":\"logs-*\"}}\n{query}\n")),
        ("/logs-*/_msearch", format!("{{}}\n{query}\n")),
        (
            "/_msearch",
            format!("{{\"index\":\"logs-*,metrics-app\"}}\n{query}\n"),
        ),
    ];
    let expected: [&[&str]; 3] = [
        &["logs-app", "logs-web"],
        &["logs-app", "logs-web"],
        &["logs-app", "logs-web", "metrics-app"],
    ];
    for ((path, body), want) in cases.into_iter().zip(expected) {
        let (st, resp) = send(&app, "POST", path, "application/x-ndjson", body.clone()).await;
        assert_eq!(st, StatusCode::OK, "{path} {body}: {resp}");
        let item = &resp["responses"][0];
        assert_eq!(item["status"], 200, "{path} {body}: {resp}");
        assert_eq!(hit_indices(item), want, "{path} {body}: {resp}");
        assert_eq!(
            item["hits"]["total"]["value"],
            want.len(),
            "{path} {body}: {resp}"
        );
    }

    // A pattern that matches nothing is an empty result, as on `_search`,
    // not a 404 for the pattern.
    let (st, resp) = send(
        &app,
        "POST",
        "/_msearch",
        "application/x-ndjson",
        format!("{{\"index\":\"nomatch-*\"}}\n{query}\n"),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{resp}");
    let item = &resp["responses"][0];
    assert_eq!(item["status"], 200, "{resp}");
    assert_eq!(item["hits"]["total"]["value"], 0, "{resp}");

    // A missing concrete name still fails that item with 404.
    let (_, resp) = send(
        &app,
        "POST",
        "/_msearch",
        "application/x-ndjson",
        format!("{{\"index\":\"missing\"}}\n{query}\n"),
    )
    .await;
    assert_eq!(resp["responses"][0]["status"], 404, "{resp}");
}
