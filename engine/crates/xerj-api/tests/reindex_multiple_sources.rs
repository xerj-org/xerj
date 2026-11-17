//! `_reindex` accepts several source indices, not just one literal name.
//!
//! ES accepts `source.index` as a string or an array, splits comma lists, and
//! expands wildcards. The old `String` field rejected an array at
//! deserialization and treated `"a,b"` / `"logs-*"` as one literal index name
//! that could not exist. The request was loud (422/404), but the documented
//! multi-source migration shapes did not work at all.
//!
//! This pins concrete arrays, comma lists, wildcards over concrete indices,
//! an exact alias's per-member filter alongside a concrete index, duplicate
//! selectors, self-copy refusal, and a wildcard that matches nothing.
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

async fn create_index(app: &axum::Router, index: &str) {
    let (status, body) = call(
        app,
        "PUT",
        &format!("/{index}"),
        json!({"mappings": {"properties": {"k": {"type": "keyword"}}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn doc(app: &axum::Router, index: &str, id: &str, k: &str) {
    let (status, body) = call(app, "PUT", &format!("/{index}/_doc/{id}"), json!({"k": k})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

async fn refresh(app: &axum::Router, index: &str) {
    let (status, body) = call(app, "POST", &format!("/{index}/_refresh"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn ids(app: &axum::Router, index: &str) -> Vec<String> {
    let (status, body) = call(
        app,
        "POST",
        &format!("/{index}/_search"),
        json!({"size": 100}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut ids: Vec<String> = body["hits"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["_id"].as_str().unwrap_or_default().to_string())
        .collect();
    ids.sort();
    ids
}

async fn reindex(app: &axum::Router, body: Value) -> (StatusCode, Value) {
    call(app, "POST", "/_reindex", body).await
}

fn reason(body: &Value) -> &str {
    body.pointer("/error/root_cause/0/reason")
        .and_then(Value::as_str)
        .or_else(|| body.get("error").and_then(Value::as_str))
        .unwrap_or_default()
}

#[tokio::test]
async fn array_sources_copy_every_index() {
    let (app, _dir) = app().await;
    for index in ["s1", "s2"] {
        create_index(&app, index).await;
    }
    doc(&app, "s1", "1", "a").await;
    doc(&app, "s2", "2", "b").await;

    let (status, body) = reindex(
        &app,
        json!({"source": {"index": ["s1", "s2"]}, "dest": {"index": "dst"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 2, "{body}");
    assert_eq!(body["created"], 2, "{body}");
    assert_eq!(ids(&app, "dst").await, vec!["1", "2"]);
}

#[tokio::test]
async fn comma_source_list_copies_every_index() {
    let (app, _dir) = app().await;
    for index in ["s1", "s2"] {
        create_index(&app, index).await;
    }
    doc(&app, "s1", "1", "a").await;
    doc(&app, "s2", "2", "b").await;

    let (status, body) = reindex(
        &app,
        json!({"source": {"index": "s1,s2"}, "dest": {"index": "dst"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 2, "{body}");
    assert_eq!(ids(&app, "dst").await, vec!["1", "2"]);
}

#[tokio::test]
async fn wildcard_source_copies_only_matching_indices() {
    let (app, _dir) = app().await;
    for index in ["logs-000001", "logs-000002", "other"] {
        create_index(&app, index).await;
    }
    doc(&app, "logs-000001", "1", "a").await;
    doc(&app, "logs-000002", "2", "b").await;
    doc(&app, "other", "3", "c").await;

    let (status, body) = reindex(
        &app,
        json!({"source": {"index": "logs-*"}, "dest": {"index": "dst"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 2, "{body}");
    assert_eq!(ids(&app, "dst").await, vec!["1", "2"]);
}

#[tokio::test]
async fn alias_filter_and_concrete_index_can_be_combined() {
    let (app, _dir) = app().await;
    for index in ["i1", "i2", "i3"] {
        create_index(&app, index).await;
    }
    doc(&app, "i1", "1", "a").await;
    doc(&app, "i1", "2", "b").await;
    doc(&app, "i2", "3", "a").await;
    doc(&app, "i3", "4", "b").await;
    let (status, body) = call(
        &app,
        "PUT",
        "/i1/_alias/a",
        json!({"filter": {"term": {"k": "a"}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &app,
        "PUT",
        "/i2/_alias/a",
        json!({"filter": {"term": {"k": "a"}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = reindex(
        &app,
        json!({"source": {"index": ["a", "i3"]}, "dest": {"index": "dst"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 3, "{body}");
    assert_eq!(
        ids(&app, "dst").await,
        vec!["1", "3", "4"],
        "both alias members are filtered, the concrete i3 is not"
    );
}

#[tokio::test]
async fn duplicate_source_selectors_copy_each_document_once() {
    let (app, _dir) = app().await;
    create_index(&app, "s1").await;
    doc(&app, "s1", "1", "a").await;

    let (status, body) = reindex(
        &app,
        json!({"source": {"index": ["s1", "s1"]}, "dest": {"index": "dst"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 1, "{body}");
    assert_eq!(ids(&app, "dst").await, vec!["1"]);
}

#[tokio::test]
async fn concrete_and_alias_duplicate_is_the_unfiltered_union() {
    let (app, _dir) = app().await;
    create_index(&app, "i1").await;
    doc(&app, "i1", "1", "a").await;
    doc(&app, "i1", "2", "b").await;
    let (status, body) = call(
        &app,
        "PUT",
        "/i1/_alias/a",
        json!({"filter": {"term": {"k": "a"}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    for sources in [json!(["i1", "a"]), json!(["a", "i1"])] {
        let dest = if sources == json!(["i1", "a"]) {
            "dst-concrete-first"
        } else {
            "dst-alias-first"
        };
        let (status, body) = reindex(
            &app,
            json!({"source": {"index": sources}, "dest": {"index": dest}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 2, "{body}");
        assert_eq!(
            ids(&app, dest).await,
            vec!["1", "2"],
            "the caller also named the concrete index, so its whole view is copied"
        );
    }
}

#[tokio::test]
async fn two_alias_filters_on_one_index_are_ored() {
    let (app, _dir) = app().await;
    create_index(&app, "i1").await;
    doc(&app, "i1", "1", "a").await;
    doc(&app, "i1", "2", "b").await;
    doc(&app, "i1", "3", "c").await;
    for (alias, value) in [("alias-a", "a"), ("alias-b", "b")] {
        let (status, body) = call(
            &app,
            "PUT",
            &format!("/i1/_alias/{alias}"),
            json!({"filter": {"term": {"k": value}}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    let (status, body) = reindex(
        &app,
        json!({"source": {"index": ["alias-a", "alias-b"]}, "dest": {"index": "dst"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 2, "{body}");
    assert_eq!(
        ids(&app, "dst").await,
        vec!["1", "2"],
        "a document visible through either alias must be copied once"
    );
}

#[tokio::test]
async fn self_copy_through_any_source_is_refused() {
    let (app, _dir) = app().await;
    create_index(&app, "s1").await;
    create_index(&app, "dst").await;
    doc(&app, "s1", "1", "a").await;
    doc(&app, "dst", "9", "existing").await;
    refresh(&app, "dst").await;

    let (status, body) = reindex(
        &app,
        json!({"source": {"index": ["s1", "dst"]}, "dest": {"index": "dst"}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        reason(&body).contains("cannot read and write the same index"),
        "{body}"
    );
    assert_eq!(
        ids(&app, "dst").await,
        vec!["9"],
        "a refused self-copy must not touch the destination"
    );
}

#[tokio::test]
async fn wildcard_matching_nothing_is_refused() {
    let (app, _dir) = app().await;

    let (status, body) = reindex(
        &app,
        json!({"source": {"index": "nope-*"}, "dest": {"index": "dst"}}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, _) = call(&app, "GET", "/dst", Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
