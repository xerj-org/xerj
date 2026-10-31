//! Issue #2407: a `semantic_text` field created without `dimensions` was
//! always sized at 384, the built-in embedder's width, even when the server's
//! own embedding backend is a `proxy` whose confirmed width (#2306, published
//! at `GET /v1/embedding/identity`) is something else. Every document then
//! failed ingest with "embedding backend returned width N, mapping requires
//! 384", and an index created a moment earlier sat green with 0 documents.
//!
//! These tests pin the fix: a bare `semantic_text` is sized at the backend's
//! confirmed width at creation time — explicit create, `PUT _mapping`, and an
//! index template applied on create — the materialized width is echoed by
//! `GET _mapping`, and a document ingests end to end. An explicit
//! `dimensions` in the mapping still wins, unchanged.
//!
//! The mock answers OpenAI `/v1/embeddings` with 768-wide vectors, the shape
//! of the report's `embeddinggemma` gateway; nothing here needs a GPU or a
//! network beyond the loopback mock.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

const DIMS: usize = 768;

async fn mock_embedder() -> String {
    let app = axum::Router::new().route(
        "/v1/embeddings",
        axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
            let count = match body.get("input") {
                Some(Value::Array(items)) => items.len(),
                Some(Value::String(_)) => 1,
                _ => 0,
            };
            let data: Vec<Value> = (0..count)
                .map(|index| {
                    let mut embedding = vec![0.0_f64; DIMS];
                    embedding[0] = 1.0;
                    json!({"index": index, "embedding": embedding})
                })
                .collect();
            (StatusCode::OK, axum::Json(json!({"data": data})))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock embedder");
    let addr = listener.local_addr().expect("mock embedder addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/v1/embeddings")
}

fn proxy_node(endpoint: String) -> axum::Router {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.auth.enabled = false;
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config.embedding.mode = "proxy".into();
    config.embedding.default_endpoint = endpoint;
    config.embedding.default_model = "mock-embed".into();
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    std::mem::forget(dir);
    xerj_api::router::build_es_compat_router(state)
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

/// One document through the semantic path proves the mapping and the backend
/// agree: before the fix the bulk item fails with `mapper_parsing_exception`
/// ("embedding backend returned width 768, mapping requires 384").
async fn ingest_one(app: &axum::Router, index: &str) {
    // `call` sends JSON; `_bulk` is NDJSON, so this request is built by hand.
    let ndjson = format!(
        "{}\n{}\n",
        json!({"index": {"_index": index, "_id": "1"}}),
        json!({"body": "The quarterly revenue report describes how the operating team closed the books."})
    );
    let request = Request::builder()
        .method("POST")
        .uri("/_bulk?refresh=true")
        .header("content-type", "application/x-ndjson")
        .body(Body::from(ndjson))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::OK, "bulk failed: {body}");
    assert_eq!(
        body.get("errors"),
        Some(&Value::Bool(false)),
        "bulk item refused: {body}"
    );
    let (status, body) = call(app, "GET", &format!("/{index}/_count"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "count failed: {body}");
    assert_eq!(body.get("count"), Some(&json!(1)), "count: {body}");
}

fn mapped_dimensions(mapping: &Value, index: &str, field: &str) -> Option<u64> {
    mapping
        .pointer(&format!("/{index}/mappings/properties/{field}/dimensions"))
        .and_then(Value::as_u64)
}

#[tokio::test(flavor = "multi_thread")]
async fn create_index_sizes_bare_semantic_text_at_the_backend_width() {
    let endpoint = mock_embedder().await;
    let app = proxy_node(endpoint);
    let (status, body) = call(
        &app,
        "PUT",
        "/kb",
        json!({"mappings": {"properties": {"body": {"type": "semantic_text"}}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create failed: {body}");
    let (status, mapping) = call(&app, "GET", "/kb/_mapping", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "mapping failed: {mapping}");
    assert_eq!(
        mapped_dimensions(&mapping, "kb", "body"),
        Some(DIMS as u64),
        "a bare semantic_text must be sized at the confirmed backend width: {mapping}"
    );
    ingest_one(&app, "kb").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn put_mapping_sizes_bare_semantic_text_at_the_backend_width() {
    let endpoint = mock_embedder().await;
    let app = proxy_node(endpoint);
    let (status, body) = call(&app, "PUT", "/kb2", json!({})).await;
    assert_eq!(status, StatusCode::OK, "create failed: {body}");
    let (status, body) = call(
        &app,
        "PUT",
        "/kb2/_mapping",
        json!({"properties": {"body": {"type": "semantic_text"}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put_mapping failed: {body}");
    let (status, mapping) = call(&app, "GET", "/kb2/_mapping", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "mapping failed: {mapping}");
    assert_eq!(
        mapped_dimensions(&mapping, "kb2", "body"),
        Some(DIMS as u64),
        "PUT _mapping must size a bare semantic_text at the backend width: {mapping}"
    );
    ingest_one(&app, "kb2").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn template_applied_on_create_sizes_bare_semantic_text_at_the_backend_width() {
    let endpoint = mock_embedder().await;
    let app = proxy_node(endpoint);
    let (status, body) = call(
        &app,
        "PUT",
        "/_index_template/kb-template",
        json!({
            "index_patterns": ["kb3*"],
            "template": {"mappings": {"properties": {"body": {"type": "semantic_text"}}}}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "template failed: {body}");
    let (status, body) = call(&app, "PUT", "/kb3", json!({})).await;
    assert_eq!(status, StatusCode::OK, "create failed: {body}");
    let (status, mapping) = call(&app, "GET", "/kb3/_mapping", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "mapping failed: {mapping}");
    assert_eq!(
        mapped_dimensions(&mapping, "kb3", "body"),
        Some(DIMS as u64),
        "a template's bare semantic_text must be sized at the backend width: {mapping}"
    );
    ingest_one(&app, "kb3").await;
}

/// `_bulk` auto-create does not pass through the create-index handler: the
/// engine applies the matching template itself, so the engine-side sizing
/// (`apply_index_template`) is what this test pins.
#[tokio::test(flavor = "multi_thread")]
async fn bulk_auto_create_from_a_template_ingests_at_the_backend_width() {
    let endpoint = mock_embedder().await;
    let app = proxy_node(endpoint);
    let (status, body) = call(
        &app,
        "PUT",
        "/_index_template/kb5-template",
        json!({
            "index_patterns": ["kb5*"],
            "template": {"mappings": {"properties": {"body": {"type": "semantic_text"}}}}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "template failed: {body}");
    ingest_one(&app, "kb5").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_explicit_dimensions_still_wins() {
    let endpoint = mock_embedder().await;
    let app = proxy_node(endpoint);
    let (status, body) = call(
        &app,
        "PUT",
        "/kb4",
        json!({"mappings": {"properties": {"body": {"type": "semantic_text", "dimensions": 32}}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create failed: {body}");
    let (status, mapping) = call(&app, "GET", "/kb4/_mapping", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "mapping failed: {mapping}");
    assert_eq!(
        mapped_dimensions(&mapping, "kb4", "body"),
        Some(32),
        "an explicit dimensions declaration is never overridden: {mapping}"
    );
}
