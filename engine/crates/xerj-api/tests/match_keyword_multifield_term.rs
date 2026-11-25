//! `match` on a keyword multi-field (`title.raw` under a `text` title, the
//! dynamic `title.keyword`, an object leaf's `obj.name.raw`) is a whole-value
//! term match, as in ES: a keyword field is not analysed.
//!
//! Before the fix the memtable answered 0 even for the exact value, and a
//! flushed segment tokenised the query, so `match title.raw: "World"` hit
//! "Hello World" after `_flush`. Every query below must give the same ids
//! before and after `_flush`, on `_search` and `_count`.

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

async fn req(app: &axum::Router, method: &str, path: &str, body: String) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body))
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

async fn ids(app: &axum::Router, index: &str, query: &Value) -> Vec<String> {
    let (status, body) = req(
        app,
        "POST",
        &format!("/{index}/_search"),
        json!({"query": query, "size": 10}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{query}: {body}");
    let mut out: Vec<String> = body["hits"]["hits"]
        .as_array()
        .expect("hits")
        .iter()
        .map(|h| h["_id"].as_str().expect("_id").to_string())
        .collect();
    out.sort();
    let (status, count) = req(
        app,
        "POST",
        &format!("/{index}/_count"),
        json!({"query": query}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{query}: {count}");
    assert_eq!(
        count["count"].as_u64(),
        Some(out.len() as u64),
        "_count disagrees with _search for {query}"
    );
    out
}

async fn seed(app: &axum::Router, index: &str, mapping: Option<Value>) {
    if let Some(m) = mapping {
        let (status, body) = req(app, "PUT", &format!("/{index}"), m.to_string()).await;
        assert_eq!(status, StatusCode::OK, "create {index}: {body}");
    }
    let bulk = concat!(
        "{\"index\":{\"_id\":\"1\"}}\n",
        "{\"title\":\"Hello World\",\"obj\":{\"name\":\"Hello World\"}}\n",
        "{\"index\":{\"_id\":\"2\"}}\n",
        "{\"title\":\"hello\",\"obj\":{\"name\":\"hello\"}}\n",
        "{\"index\":{\"_id\":\"3\"}}\n",
        "{\"title\":\"Goodbye World\",\"obj\":{\"name\":\"Goodbye World\"}}\n",
    );
    let (status, body) = req(app, "POST", &format!("/{index}/_bulk"), bulk.to_string()).await;
    assert_eq!(status, StatusCode::OK, "bulk {index}: {body}");
    assert_eq!(body["errors"], json!(false), "bulk {index}: {body}");
}

fn cases(sub: &str) -> Vec<(Value, Vec<&'static str>)> {
    let t = format!("title.{sub}");
    let o = format!("obj.name.{sub}");
    vec![
        (json!({"match": {&t: "Hello World"}}), vec!["1"]),
        (json!({"match": {&t: "hello"}}), vec!["2"]),
        (json!({"match": {&t: "World"}}), vec![]),
        (json!({"match": {&t: "hello world"}}), vec![]),
        (
            json!({"match": {&t: {"query": "Hello World", "operator": "and"}}}),
            vec!["1"],
        ),
        (
            json!({"constant_score": {"filter": {"match": {&t: "World"}}}}),
            vec![],
        ),
        (
            json!({"bool": {"filter": [{"match": {&t: "hello"}}]}}),
            vec!["2"],
        ),
        (
            json!({"multi_match": {"query": "Hello World", "fields": [&t]}}),
            vec!["1"],
        ),
        (json!({"match": {&o: "World"}}), vec![]),
        (json!({"match": {&o: "Hello World"}}), vec!["1"]),
        // The analysed parent is unchanged.
        (json!({"match": {"title": "World"}}), vec!["1", "3"]),
    ]
}

async fn assert_cases(app: &axum::Router, index: &str, sub: &str) {
    for phase in ["memtable", "flushed"] {
        if phase == "flushed" {
            let (status, body) = req(app, "POST", &format!("/{index}/_flush"), String::new()).await;
            assert_eq!(status, StatusCode::OK, "flush {index}: {body}");
        }
        for (query, want) in cases(sub) {
            assert_eq!(
                ids(app, index, &query).await,
                want,
                "{index} {phase}: {query}"
            );
        }
    }
}

#[tokio::test]
async fn match_on_mapped_keyword_multifield_is_a_term_match_before_and_after_flush() {
    let (app, _dir) = app().await;
    let raw = json!({"type": "text", "fields": {"raw": {"type": "keyword"}}});
    seed(
        &app,
        "mf_mapped",
        Some(json!({"mappings": {"properties": {
            "title": raw,
            "obj": {"properties": {"name": raw}}
        }}})),
    )
    .await;
    assert_cases(&app, "mf_mapped", "raw").await;
}

#[tokio::test]
async fn match_on_dynamic_keyword_multifield_is_a_term_match_before_and_after_flush() {
    let (app, _dir) = app().await;
    seed(&app, "mf_dynamic", None).await;
    assert_cases(&app, "mf_dynamic", "keyword").await;
}
