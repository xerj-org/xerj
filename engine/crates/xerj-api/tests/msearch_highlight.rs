//! Issue #1945: `_msearch` dropped `highlight`. An item whose body asks for
//! it came back with `_index`/`_id`/`_score`/`_source` only, while `_search`
//! with the same body returned the fragment.
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

async fn seeded() -> (axum::Router, tempfile::TempDir) {
    let (app, dir) = app().await;
    let (st, resp) = send(
        &app,
        "PUT",
        "/hl",
        "application/json",
        json!({"mappings": {"properties": {"body": {"type": "text"}}}}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create: {resp}");
    let (st, resp) = send(
        &app,
        "PUT",
        "/hl/_doc/1?refresh=true",
        "application/json",
        json!({"body": "Pricing is per-node for Professional."}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "index: {resp}");
    (app, dir)
}

#[tokio::test]
async fn msearch_items_carry_highlight_like_search() {
    let (app, _dir) = seeded().await;
    let body = json!({
        "query": {"match": {"body": "pricing"}},
        "highlight": {"pre_tags": ["<b>"], "post_tags": ["</b>"], "fields": {"body": {}}}
    });
    let (st, single) = send(
        &app,
        "POST",
        "/hl/_search",
        "application/json",
        body.to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{single}");
    let want = single["hits"]["hits"][0]["highlight"].clone();
    assert_eq!(
        want,
        json!({"body": ["<b>Pricing</b> is per-node for Professional."]}),
        "{single}"
    );

    for path in ["/_msearch", "/hl/_msearch"] {
        let ndjson = format!("{{\"index\":\"hl\"}}\n{body}\n{{\"index\":\"hl\"}}\n{{\"query\":{{\"match\":{{\"body\":\"pricing\"}}}}}}\n");
        let (st, resp) = send(&app, "POST", path, "application/x-ndjson", ndjson).await;
        assert_eq!(st, StatusCode::OK, "{path}: {resp}");
        assert_eq!(
            resp["responses"][0]["hits"]["hits"][0]["highlight"], want,
            "{path}: {resp}"
        );
        assert!(
            resp["responses"][1]["hits"]["hits"][0]
                .get("highlight")
                .is_none(),
            "{path}: no highlight block unless one was asked for: {resp}"
        );
    }
}
