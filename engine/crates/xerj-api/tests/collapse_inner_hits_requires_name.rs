//! A `collapse.inner_hits` block must carry a `name`, as in Elasticsearch 8.13.
//!
//! ES refuses the body while it parses `collapse`: an `inner_hits` object, or
//! any entry of an `inner_hits` array, without `name` is a 400
//! `x_content_parse_exception` (`[collapse] failed to parse field
//! [inner_hits]`) caused by an `illegal_argument_exception` (`inner_hits must
//! have a [name]; set the [name] field in the inner_hits definition`). XERJ
//! ran the search and rendered the group under a made-up `inner_hits` key.
//! `_msearch` parses every item body before it runs any of them, so one
//! unnamed block refuses the whole request there.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

const CAUSE: &str =
    "inner_hits must have a [name]; set the [name] field in the inner_hits definition";

async fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn send(app: &axum::Router, path: &str, body: &str) -> (StatusCode, Value) {
    let content_type = if path.contains("_bulk") || path.contains("_msearch") {
        "application/x-ndjson"
    } else {
        "application/json"
    };
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", content_type)
        .body(Body::from(body.to_owned()))
        .expect("request");
    let response = app.clone().oneshot(req).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Three documents in two collapse groups.
async fn seeded() -> (axum::Router, tempfile::TempDir) {
    let (app, dir) = app().await;
    let bulk = "{\"index\":{\"_index\":\"cih\",\"_id\":\"1\"}}\n{\"g\":1,\"n\":1}\n\
                {\"index\":{\"_index\":\"cih\",\"_id\":\"2\"}}\n{\"g\":1,\"n\":2}\n\
                {\"index\":{\"_index\":\"cih\",\"_id\":\"3\"}}\n{\"g\":2,\"n\":3}\n";
    let (status, got) = send(&app, "/_bulk?refresh=true", bulk).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["errors"], false, "{got}");
    (app, dir)
}

fn assert_refused(status: StatusCode, got: &Value, what: &str) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{what}: {got}");
    assert_eq!(got["status"], 400, "{what}: {got}");
    assert_eq!(
        got["error"]["type"], "x_content_parse_exception",
        "{what}: {got}"
    );
    let reason = got["error"]["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("[collapse] failed to parse field [inner_hits]"),
        "{what}: {got}"
    );
    assert_eq!(
        got["error"]["caused_by"]["type"], "illegal_argument_exception",
        "{what}: {got}"
    );
    assert_eq!(got["error"]["caused_by"]["reason"], CAUSE, "{what}: {got}");
}

#[tokio::test]
async fn an_unnamed_inner_hits_object_is_refused() {
    let (app, _dir) = seeded().await;
    let body = r#"{"collapse":{"field":"g","inner_hits":{"size":1}},"sort":[{"n":"desc"}]}"#;
    let (status, got) = send(&app, "/cih/_search", body).await;
    assert_refused(status, &got, "object form");
}

#[tokio::test]
async fn an_unnamed_entry_in_an_inner_hits_array_is_refused() {
    let (app, _dir) = seeded().await;
    // The unnamed entry is not the first one: ES checks every entry.
    let body = r#"{"collapse":{"field":"g","inner_hits":[
        {"name":"first","size":1},
        {"size":2}
    ]}}"#;
    let (status, got) = send(&app, "/cih/_search", body).await;
    assert_refused(status, &got, "array form");
}

#[tokio::test]
async fn an_unnamed_inner_hits_refuses_the_whole_msearch() {
    let (app, _dir) = seeded().await;
    let body = "{\"index\":\"cih\"}\n{\"query\":{\"match_all\":{}}}\n\
                {\"index\":\"cih\"}\n{\"collapse\":{\"field\":\"g\",\"inner_hits\":{\"size\":1}}}\n";
    for path in ["/_msearch", "/cih/_msearch"] {
        let (status, got) = send(&app, path, body).await;
        assert_refused(status, &got, path);
    }
}

#[tokio::test]
async fn named_inner_hits_still_render() {
    let (app, _dir) = seeded().await;
    // ES checks only that a name is present, so an empty name passes too.
    let body = r#"{"collapse":{"field":"g","inner_hits":[
        {"name":"top","size":1},
        {"name":"","size":2}
    ]},"sort":[{"n":"desc"}]}"#;
    let (status, got) = send(&app, "/cih/_search", body).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let hits = got["hits"]["hits"].as_array().expect("hits");
    assert_eq!(hits.len(), 2, "one leader per group: {got}");
    let leader = &hits[0];
    assert_eq!(leader["_id"], "3", "{got}");
    assert!(leader["inner_hits"].get("top").is_some(), "{got}");
    assert!(leader["inner_hits"].get("").is_some(), "{got}");
    assert!(leader["inner_hits"].get("inner_hits").is_none(), "{got}");
}
