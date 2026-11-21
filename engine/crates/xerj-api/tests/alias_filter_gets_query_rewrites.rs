//! Review follow-up on #3185 (fixes #3065): alias filters used to sit inside
//! `body.query`, so every rewrite in `search_impl` applied to them. After
//! #3065 they are parsed separately, and only the terms-lookup and keyword
//! `match` to `term` rewrites were carried over. Of the other two:
//!
//! * `rewrite_unqueryable_exists` IS needed and is now applied to the member
//!   filters in `search_impl`: an `exists` on a field whose mapping declares
//!   `index: false` with no doc values answers 0 hits in a user query
//!   (rewritten to `match_none`); without the rewrite the same filter was
//!   answered from source and matched the document carrying the field.
//! * The field-alias rewrite (`rewrite_leaf_field_values` with `alias_map`)
//!   is NOT needed: the engine resolves alias fields per index at execution
//!   time (`rewrite_query_aliases` in `crates/xerj-engine/src/index.rs`),
//!   which covers the leaf shapes a filter can carry (term, terms, range,
//!   exists, match, bool) and resolves against each member's own schema,
//!   where the API-layer union map would be first-wins across members. The
//!   alias-field test below passes on the code before this fix and pins
//!   that behaviour; only the `exists` test is red before it.
//!
//! Both go through the real HTTP routes because that is the only place the
//! alias resolution, the rewrites, and the per-member fan-out meet.

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

async fn search_ids(app: &axum::Router, index: &str, query: Value) -> (u64, Vec<String>) {
    let (st, body) = call(
        app,
        "POST",
        &format!("/{index}/_search"),
        json!({"query": query, "size": 10}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "_search {index} status: {body}");
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

/// A filter naming a `type: alias` field must resolve to the target field,
/// exactly as the user query does. Index `t1` maps `type` as an alias for
/// the keyword field `kind`; the alias filter `{"term": {"type": "a"}}`
/// must keep only doc 0.
#[tokio::test]
async fn alias_filter_naming_an_alias_field_resolves_to_the_target() {
    let (app, _dir) = app().await;
    let (st, _) = call(
        &app,
        "PUT",
        "/t1",
        json!({"mappings": {"properties": {
            "kind": {"type": "keyword"},
            "type": {"type": "alias", "path": "kind"}
        }}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create t1");
    for (id, doc) in [("0", json!({"kind": "a"})), ("1", json!({"kind": "b"}))] {
        let (st, _) = call(&app, "POST", &format!("/t1/_doc/{id}"), doc).await;
        assert_eq!(st, StatusCode::CREATED, "index doc {id} into t1");
    }
    let (st, _) = call(&app, "POST", "/t1/_refresh", Value::Null).await;
    assert_eq!(st, StatusCode::OK, "refresh t1");

    // Precondition: the user query gets the alias rewrite, so the same
    // clause as a query matches doc 0.
    let (total, ids) = search_ids(&app, "t1", json!({"term": {"type": "a"}})).await;
    assert_eq!(
        total, 1,
        "precondition: a query naming the alias field must match doc 0"
    );
    assert_eq!(
        ids,
        vec!["0".to_string()],
        "precondition: query on alias field"
    );

    let (st, body) = call(
        &app,
        "PUT",
        "/t1/_alias/ali",
        json!({ "filter": { "term": { "type": "a" } } }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create filtered alias: {body}");

    let (total, ids) = search_ids(&app, "ali", json!({"match_all": {}})).await;
    assert_eq!(
        total, 1,
        "a filter naming an alias field must resolve to the target field, like the query does"
    );
    assert_eq!(
        ids,
        vec!["0".to_string()],
        "the alias filter must keep only doc 0 (kind a), not drop every document: {ids:?}"
    );
}

/// An `exists` filter on a field whose mapping declares `index: false`
/// with no doc values must answer 0 hits, exactly as the user query does
/// (rewritten to `match_none`). Without the rewrite the filter is answered
/// from source and matches the doc that carries the field.
#[tokio::test]
async fn alias_filter_exists_on_an_unqueryable_field_matches_nothing() {
    let (app, _dir) = app().await;
    let (st, _) = call(
        &app,
        "PUT",
        "/t2",
        json!({"mappings": {"properties": {
            "v": {"type": "long"},
            "secret": {"type": "keyword", "index": false, "doc_values": false}
        }}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create t2");
    for (id, doc) in [
        ("0", json!({"v": 0, "secret": "x"})),
        ("1", json!({"v": 1})),
    ] {
        let (st, _) = call(&app, "POST", &format!("/t2/_doc/{id}"), doc).await;
        assert_eq!(st, StatusCode::CREATED, "index doc {id} into t2");
    }
    let (st, _) = call(&app, "POST", "/t2/_refresh", Value::Null).await;
    assert_eq!(st, StatusCode::OK, "refresh t2");

    // Precondition: the user query gets the unqueryable-exists rewrite, so
    // `exists` on this field matches nothing.
    let (total, _) = search_ids(&app, "t2", json!({"exists": {"field": "secret"}})).await;
    assert_eq!(
        total, 0,
        "precondition: a query exists on an unqueryable field must match nothing"
    );

    let (st, body) = call(
        &app,
        "PUT",
        "/t2/_alias/ali2",
        json!({ "filter": { "exists": { "field": "secret" } } }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create filtered alias: {body}");

    let (total, ids) = search_ids(&app, "ali2", json!({"match_all": {}})).await;
    assert_eq!(
        total, 0,
        "an exists filter on an unqueryable field must match nothing, like the query does, \
         not match doc 0 from source: {ids:?}"
    );
    assert!(ids.is_empty(), "no document may pass the filter: {ids:?}");
}
