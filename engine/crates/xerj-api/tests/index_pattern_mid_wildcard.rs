//! Issue #1933: an index pattern with `*` in the middle (`logs-*-2026`)
//! matched no index, so `_search` answered 200 with 0 hits and no error.
//! Leading and trailing wildcards worked. The matcher behind it
//! (`glob_match_simple`) is shared by `_search`, `_count`, `GET /{index}` and
//! the other index-selector endpoints, so this pins the read paths an agent
//! hits first.
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

async fn send(app: &axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    let body = if body.is_null() {
        String::new()
    } else {
        req = req.header("content-type", "application/json");
        body.to_string()
    };
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

async fn seeded() -> (axum::Router, tempfile::TempDir) {
    let (app, dir) = app().await;
    for index in ["logs-app-2026", "logs-web-2026", "logs-app-2025"] {
        let (st, resp) = send(
            &app,
            "PUT",
            &format!("/{index}"),
            json!({"mappings": {"properties": {"msg": {"type": "text"}}}}),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "create {index}: {resp}");
        let (st, resp) = send(
            &app,
            "PUT",
            &format!("/{index}/_doc/1?refresh=true"),
            json!({"msg": format!("disk full on {index}")}),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "index into {index}: {resp}");
    }
    (app, dir)
}

fn hit_indices(resp: &Value) -> Vec<String> {
    let mut out: Vec<String> = resp["hits"]["hits"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|h| h["_index"].as_str().map(str::to_string))
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn search_resolves_a_mid_name_wildcard() {
    let (app, _dir) = seeded().await;
    let query = json!({"query": {"match": {"msg": "disk"}}});

    let (st, resp) = send(&app, "POST", "/logs-*-2026/_search", query.clone()).await;
    assert_eq!(st, StatusCode::OK, "{resp}");
    assert_eq!(resp["hits"]["total"]["value"], 2, "{resp}");
    assert_eq!(
        hit_indices(&resp),
        ["logs-app-2026", "logs-web-2026"],
        "{resp}"
    );

    // Two stars, one of them in the middle.
    let (st, resp) = send(&app, "POST", "/*-app-*/_search", query.clone()).await;
    assert_eq!(st, StatusCode::OK, "{resp}");
    assert_eq!(
        hit_indices(&resp),
        ["logs-app-2025", "logs-app-2026"],
        "{resp}"
    );

    let (st, resp) = send(&app, "POST", "/logs-*-2026/_count", query).await;
    assert_eq!(st, StatusCode::OK, "{resp}");
    assert_eq!(resp["count"], 2, "{resp}");
}

#[tokio::test]
async fn get_index_resolves_a_mid_name_wildcard() {
    let (app, _dir) = seeded().await;
    let (st, resp) = send(&app, "GET", "/logs-*-2026", Value::Null).await;
    assert_eq!(st, StatusCode::OK, "{resp}");
    let mut names: Vec<&String> = resp.as_object().expect("object").keys().collect();
    names.sort();
    assert_eq!(names, ["logs-app-2026", "logs-web-2026"], "{resp}");
}
