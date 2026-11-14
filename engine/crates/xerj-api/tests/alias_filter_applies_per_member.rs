//! Issue #3065: a filter on one alias member is applied to every member.
//!
//! Alias `ali` covers `t1` (no filter) and `t2` (filter
//! `{"term": {"cat": "c"}}`). With t1 holding docs 0 and 1, and t2 holding
//! doc 2 (`cat: a`) and doc 3 (`cat: c`), Elasticsearch 8.13.4 returns 0, 1
//! and 3 for a match-all search through the alias, and `_count` is 3.
//!
//! xerj collected every member's filter into one list and ANDed it onto the
//! query for all indices, so `_search` returned only doc 3 and `_count` was
//! 1. The same combined list fed `_delete_by_query`, which deleted only the
//! filtered member's slice instead of every member's matching documents.
//!
//! These go through the real HTTP routes because that is the only place the
//! alias resolution and the query fan-out meet.

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

async fn count(app: &axum::Router, index: &str) -> u64 {
    let (_, body) = call(app, "GET", &format!("/{index}/_count"), Value::Null).await;
    body.get("count")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX)
}

/// The #3065 repro, verbatim: t1 holds docs 0 and 1, t2 holds doc 2
/// (`cat: a`) and doc 3 (`cat: c`); only t2 carries a filter.
async fn seed(app: &axum::Router) {
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
    let (st, _) = call(
        app,
        "POST",
        "/_aliases",
        json!({"actions": [
            {"add": {"index": "t1", "alias": "ali"}},
            {"add": {"index": "t2", "alias": "ali", "filter": {"term": {"cat": "c"}}}}
        ]}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create alias ali: {st}");
}

async fn search_ids(app: &axum::Router) -> (u64, Vec<String>) {
    let (st, body) = call(
        app,
        "POST",
        "/ali/_search",
        json!({"query": {"match_all": {}}, "size": 10}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "_search status: {body}");
    let total = body["hits"]["total"]["value"].as_u64().unwrap_or(u64::MAX);
    let mut ids: Vec<String> = body["hits"]["hits"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|h| h["_id"].as_str().map(str::to_string))
        .collect();
    ids.sort();
    (total, ids)
}

/// The filter belongs to t2 only: t1's documents must still be returned by
/// `_search` and counted by `_count`.
#[tokio::test]
async fn search_and_count_through_a_partially_filtered_alias_see_the_unfiltered_member() {
    let (app, _dir) = app().await;
    seed(&app).await;

    let (total, ids) = search_ids(&app).await;
    assert_eq!(
        total, 3,
        "_search through the alias must return t1's docs plus t2's filtered doc"
    );
    assert_eq!(
        ids,
        vec!["0".to_string(), "1".to_string(), "3".to_string()],
        "_search through the alias must return docs 0, 1 and 3, not just the filtered member's doc"
    );

    assert_eq!(
        count(&app, "ali").await,
        3,
        "_count through the alias must count t1's docs plus t2's filtered doc"
    );
}

/// The same pairing on the write path: a match-all `_delete_by_query`
/// through the alias deletes t1's two documents (unfiltered) and t2's
/// filtered doc 3, leaving only doc 2 (`cat: a`, outside t2's filter).
#[tokio::test]
async fn delete_by_query_through_a_partially_filtered_alias_deletes_each_members_slice() {
    let (app, _dir) = app().await;
    seed(&app).await;

    let (status, body) = call(
        &app,
        "POST",
        "/ali/_delete_by_query",
        json!({"query": {"match_all": {}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "_delete_by_query status: {body}");
    assert_eq!(
        body.get("deleted").and_then(Value::as_u64),
        Some(3),
        "_delete_by_query through the alias must delete t1's docs and t2's filtered doc: {body}"
    );

    assert_eq!(
        count(&app, "t1").await,
        0,
        "t1 must be empty after the delete: its member carries no filter"
    );
    assert_eq!(
        count(&app, "t2").await,
        1,
        "t2 must keep doc 2: it sits outside the member's filter"
    );
    assert_eq!(
        count(&app, "ali").await,
        0,
        "the alias must count zero after the delete: the surviving doc 2 sits \
         outside t2's filter, so it is hidden behind the alias even though it \
         still exists in t2"
    );
}
