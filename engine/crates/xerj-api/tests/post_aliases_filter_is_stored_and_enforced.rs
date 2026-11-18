//! `POST /_aliases` must persist the alias metadata it is given.
//!
//! `AliasActionParams` used to carry only `index` and `alias`, so serde
//! silently discarded `filter` (and routing / is_write_index). The requested
//! alias was created, `acknowledged: true` came back, and every read through it
//! exposed the whole backing index — a filtered alias is the standard
//! poor-man's document boundary, so that is a data leak, not a cosmetic one.
//!
//! The standard action API now stores the same metadata `PUT
//! /{index}/_alias/{name}` does, persists it, and removes it on `remove` (or
//! when an alias is re-added without a filter) so a stale filter cannot
//! outlive its definition.
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

async fn get(app: &axum::Router, path: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::get(path).body(Body::empty()).expect("request"),
    )
    .await
}

async fn post(app: &axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    send(app, json_req("POST", path, body)).await
}

async fn put(app: &axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    send(app, json_req("PUT", path, body)).await
}

async fn seed(app: &axum::Router) {
    let (status, body) = put(
        app,
        "/di",
        json!({"mappings": {"properties": {"k": {"type": "keyword"}}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for (id, k) in [("1", "a"), ("2", "a"), ("3", "b")] {
        let (status, body) = put(app, &format!("/di/_doc/{id}"), json!({"k": k})).await;
        assert!(status.is_success(), "{body}");
    }
    let (status, body) = post(app, "/di/_refresh", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn search_total(app: &axum::Router, index: &str, body: Value) -> u64 {
    let (status, resp) = post(app, &format!("/{index}/_search"), body).await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    resp["hits"]["total"]["value"].as_u64().unwrap_or(0)
}

async fn count(app: &axum::Router, index: &str, body: Value) -> u64 {
    let (status, resp) = post(app, &format!("/{index}/_count"), body).await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    resp["count"].as_u64().unwrap_or(0)
}

async fn add_filtered_alias(app: &axum::Router) {
    let (status, body) = post(
        app,
        "/_aliases",
        json!({"actions": [{"add": {
            "index": "di",
            "alias": "da",
            "filter": {"term": {"k": "a"}},
        }}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["acknowledged"], true, "{body}");
}

#[tokio::test]
async fn post_aliases_filter_is_stored_and_enforced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_over(dir.path());
    seed(&app).await;
    add_filtered_alias(&app).await;

    // The definition itself: metadata came back rather than `-`.
    let (status, body) = get(&app, "/_alias/da").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body.pointer("/di/aliases/da/filter"),
        Some(&json!({"term": {"k": "a"}})),
        "the filter must be part of the alias definition: {body}"
    );
    let (status, body) = get(&app, "/_cat/aliases?format=json&h=alias,index,filter").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body[0]["filter"].as_str().unwrap_or_default().contains('-'),
        "cat/aliases must render the filter, not `-`: {body}"
    );

    // Every read path that resolves an alias must honour the slice.
    assert_eq!(
        search_total(&app, "da", json!({"size": 0, "query": {"match_all": {}}})).await,
        2,
        "the alias exposes only k=a"
    );
    assert_eq!(
        count(&app, "da", json!({"query": {"match_all": {}}})).await,
        2
    );
    assert_eq!(
        search_total(
            &app,
            "da",
            json!({"size": 0, "query": {"term": {"k": "b"}}})
        )
        .await,
        0,
        "a document outside the filter must not be reachable through the alias"
    );

    // The msearch alias path shares the same metadata.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/_msearch")
                .header("content-type", "application/x-ndjson")
                .body(Body::from(
                    "{\"index\":\"da\"}\n{\"size\":0,\"query\":{\"match_all\":{}}}\n".to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["responses"][0]["hits"]["total"]["value"], 2, "{body}");
}

#[tokio::test]
async fn post_aliases_filter_survives_restart_and_is_removed_with_the_alias() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let app = app_over(dir.path());
        seed(&app).await;
        add_filtered_alias(&app).await;
        assert_eq!(
            search_total(&app, "da", json!({"size": 0, "query": {"match_all": {}}})).await,
            2
        );
    }

    // Fresh engine, same data dir: only what reached disk is visible.
    let app = app_over(dir.path());
    let (status, body) = get(&app, "/_alias/da").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body.pointer("/di/aliases/da/filter"),
        Some(&json!({"term": {"k": "a"}})),
        "the POST /_aliases filter must be persisted: {body}"
    );
    assert_eq!(
        search_total(&app, "da", json!({"size": 0, "query": {"match_all": {}}})).await,
        2,
        "the persisted filter must still be enforced"
    );

    // Remove the alias: its metadata must go with it, so re-adding the alias
    // without a filter exposes the whole index instead of a stale slice.
    let (status, body) = post(
        &app,
        "/_aliases",
        json!({"actions": [{"remove": {"index": "di", "alias": "da"}}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = get(&app, "/_alias/da").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.get("di").is_none(),
        "the removed alias must be gone: {body}"
    );
    let (status, body) = post(
        &app,
        "/_aliases",
        json!({"actions": [{"add": {"index": "di", "alias": "da"}}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        search_total(&app, "da", json!({"size": 0, "query": {"match_all": {}}})).await,
        3,
        "a re-added alias without a filter is not filtered"
    );
}

#[tokio::test]
async fn re_putting_an_alias_without_a_body_clears_its_filter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_over(dir.path());
    seed(&app).await;
    let (status, body) = put(
        &app,
        "/di/_alias/da",
        json!({"filter": {"term": {"k": "a"}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        search_total(&app, "da", json!({"size": 0, "query": {"match_all": {}}})).await,
        2
    );
    let (status, body) = put(&app, "/di/_alias/da", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        search_total(&app, "da", json!({"size": 0, "query": {"match_all": {}}})).await,
        3,
        "an empty re-put replaces the alias definition, filter included"
    );
}
