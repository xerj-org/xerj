//! `_reindex` must honour aliases on both sides.
//!
//! Elasticsearch accepts an index, alias, or data stream as `source.index`.
//! A multi-index alias is searched across every backing index, and a filtered
//! alias adds its filter to that search. The destination may be an alias too;
//! writes route to its single write index, and an ambiguous alias is refused.
//!
//! Before this change `_reindex` called `get_index(source)` (which resolves an
//! alias to its FIRST backing index) and `get_or_create_index(dest)` (same
//! first-member rule). A reindex from a filtered two-index alias therefore
//! copied only the first index and ignored the filter, and a reindex into an
//! alias with `is_write_index` on the second member wrote the first member
//! anyway. Both answered 200, so the wrong data was silent.
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

async fn create_keyword_index(app: &axum::Router, index: &str) {
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

async fn alias(app: &axum::Router, index: &str, name: &str, body: Value) {
    let (status, response) = call(app, "PUT", &format!("/{index}/_alias/{name}"), body).await;
    assert_eq!(status, StatusCode::OK, "{response}");
}

async fn ids(app: &axum::Router, index: &str) -> Vec<String> {
    let (status, body) = call(app, "POST", &format!("/{index}/_refresh"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
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

async fn count(app: &axum::Router, index: &str, query: Value) -> u64 {
    let (status, body) = call(app, "POST", &format!("/{index}/_refresh"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        app,
        "POST",
        &format!("/{index}/_count"),
        json!({"query": query}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["count"].as_u64().unwrap_or(0)
}

async fn reindex(app: &axum::Router, source: &str, dest: &str) -> (StatusCode, Value) {
    call(
        app,
        "POST",
        "/_reindex",
        json!({"source": {"index": source}, "dest": {"index": dest}}),
    )
    .await
}

#[tokio::test]
async fn source_alias_reads_every_member_and_applies_each_members_filter() {
    let (app, _dir) = app().await;
    for index in ["i1", "i2"] {
        create_keyword_index(&app, index).await;
    }
    doc(&app, "i1", "1", "a").await;
    doc(&app, "i1", "2", "b").await;
    doc(&app, "i2", "3", "a").await;
    doc(&app, "i2", "4", "b").await;

    // Same alias, different filter per backing index: the source view is the
    // union of each member's own filtered subset.
    alias(&app, "i1", "mixed", json!({"filter": {"term": {"k": "a"}}})).await;
    alias(&app, "i2", "mixed", json!({"filter": {"term": {"k": "b"}}})).await;

    let (status, body) = reindex(&app, "mixed", "dst_mixed").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 2, "{body}");
    assert_eq!(body["created"], 2, "{body}");
    assert_eq!(body["failures"], json!([]), "{body}");
    assert_eq!(ids(&app, "dst_mixed").await, vec!["1", "4"]);

    // A filter is applied to a one-index alias too: the old path used the
    // concrete index and copied every document.
    alias(
        &app,
        "i2",
        "only_b",
        json!({"filter": {"term": {"k": "b"}}}),
    )
    .await;
    let (status, body) = reindex(&app, "only_b", "dst_one").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 1, "{body}");
    assert_eq!(ids(&app, "dst_one").await, vec!["4"]);
}

#[tokio::test]
async fn unfiltered_alias_reads_every_member() {
    let (app, _dir) = app().await;
    for index in ["i1", "i2"] {
        create_keyword_index(&app, index).await;
    }
    doc(&app, "i1", "1", "a").await;
    doc(&app, "i1", "2", "b").await;
    doc(&app, "i2", "3", "a").await;
    doc(&app, "i2", "4", "b").await;
    alias(&app, "i1", "all", json!({})).await;
    alias(&app, "i2", "all", json!({})).await;

    let (status, body) = reindex(&app, "all", "dst_all").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 4, "{body}");
    assert_eq!(ids(&app, "dst_all").await, vec!["1", "2", "3", "4"]);
}

#[tokio::test]
async fn destination_alias_writes_its_write_index() {
    let (app, _dir) = app().await;
    create_keyword_index(&app, "source").await;
    doc(&app, "source", "1", "a").await;
    doc(&app, "source", "2", "b").await;
    for index in ["d1", "d2"] {
        create_keyword_index(&app, index).await;
    }
    let (status, body) = call(
        &app,
        "POST",
        "/_aliases",
        json!({"actions": [
            {"add": {"index": "d1", "alias": "dst"}},
            {"add": {"index": "d2", "alias": "dst", "is_write_index": true}},
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = reindex(&app, "source", "dst").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 2, "{body}");
    assert_eq!(body["created"], 2, "{body}");
    assert_eq!(count(&app, "d1", json!({"match_all": {}})).await, 0);
    assert_eq!(count(&app, "d2", json!({"match_all": {}})).await, 2);
    let (status, body) = call(&app, "GET", "/d2/_doc/1", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn ambiguous_destination_alias_is_refused_before_any_copy() {
    let (app, _dir) = app().await;
    create_keyword_index(&app, "source").await;
    doc(&app, "source", "1", "a").await;
    for index in ["d1", "d2"] {
        create_keyword_index(&app, index).await;
    }
    let (status, body) = call(
        &app,
        "POST",
        "/_aliases",
        json!({"actions": [
            {"add": {"index": "d1", "alias": "amb"}},
            {"add": {"index": "d2", "alias": "amb"}},
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = reindex(&app, "source", "amb").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body.pointer("/error/type").and_then(Value::as_str),
        Some("illegal_argument_exception"),
        "{body}"
    );
    let reason = body
        .pointer("/error/root_cause/0/reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        reason.contains("no write index is defined for alias [amb]"),
        "{body}"
    );
    assert_eq!(count(&app, "d1", json!({"match_all": {}})).await, 0);
    assert_eq!(count(&app, "d2", json!({"match_all": {}})).await, 0);
}

#[tokio::test]
async fn overlapping_alias_destination_is_refused_without_rewriting_source() {
    let (app, _dir) = app().await;
    for index in ["first", "second"] {
        create_keyword_index(&app, index).await;
        doc(&app, index, index, index).await;
        alias(&app, index, "both", json!({})).await;
    }
    alias(&app, "second", "write_second", json!({})).await;
    let before = call(&app, "GET", "/second/_doc/second", Value::Null).await;
    for (source, dest) in [
        ("both", "second"),
        ("both", "write_second"),
        ("second", "second"),
    ] {
        let (status, body) = reindex(&app, source, dest).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{source} -> {dest}: {body}"
        );
        assert!(
            body["error"]["reason"]
                .as_str()
                .unwrap()
                .contains("same index"),
            "{body}"
        );
        assert_eq!(
            call(&app, "GET", "/second/_doc/second", Value::Null).await,
            before
        );
        assert_eq!(ids(&app, "second").await, vec!["second"]);
    }
    let (status, body) = reindex(&app, "both", "separate").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&app, "separate").await, vec!["first", "second"]);
}

#[tokio::test]
async fn missing_source_or_invalid_query_does_not_create_destination() {
    let (app, _dir) = app().await;
    let (status, body) = reindex(&app, "missing", "new_destination").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(
        call(&app, "GET", "/new_destination", Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
    create_keyword_index(&app, "source").await;
    doc(&app, "source", "one", "a").await;
    let (status, body) = call(
        &app,
        "POST",
        "/_reindex",
        json!({
            "source":{"index":"source", "query":{"not_a_query":{}}},
            "dest":{"index":"new_destination"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        call(&app, "GET", "/new_destination", Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
    let (status, body) = reindex(&app, "source", "new_destination").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&app, "new_destination").await, vec!["one"]);
}

#[tokio::test]
async fn later_member_flush_failure_keeps_destination_unchanged_and_can_retry() {
    let (app, dir) = app().await;
    for index in ["first", "second"] {
        create_keyword_index(&app, index).await;
        doc(&app, index, index, "a").await;
        alias(&app, index, "both", json!({})).await;
    }
    create_keyword_index(&app, "destination").await;
    doc(&app, "destination", "existing", "keep").await;
    let before = call(&app, "GET", "/destination/_doc/existing", Value::Null).await;
    // Real I/O refusal, independent of permissions and test-only failpoints.
    let segments = dir.path().join("second/segments");
    std::fs::remove_dir(&segments).unwrap();
    std::fs::write(&segments, b"not a directory").unwrap();
    let (status, body) = reindex(&app, "both", "destination").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(body.to_string().contains("flushing"), "{body}");
    assert_eq!(
        ids(&app, "destination").await,
        vec!["existing"],
        "a failed preflight must not copy earlier members"
    );
    assert_eq!(
        call(&app, "GET", "/destination/_doc/existing", Value::Null).await,
        before
    );
    for index in ["first", "second"] {
        assert_eq!(
            call(&app, "GET", &format!("/{index}/_doc/{index}"), Value::Null)
                .await
                .0,
            StatusCode::OK
        );
    }
    std::fs::remove_file(&segments).unwrap();
    std::fs::create_dir(&segments).unwrap();
    let (status, body) = reindex(&app, "both", "destination").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&app, "destination").await,
        vec!["existing", "first", "second"]
    );
}

#[tokio::test]
async fn explicitly_disabled_single_member_alias_refuses_writes_and_can_be_enabled() {
    let (app, _dir) = app().await;
    create_keyword_index(&app, "source").await;
    doc(&app, "source", "one", "keep").await;
    create_keyword_index(&app, "destination").await;
    alias(
        &app,
        "destination",
        "disabled",
        json!({"is_write_index": false}),
    )
    .await;
    let (status, body) = reindex(&app, "source", "disabled").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string().contains("no write index is defined"),
        "{body}"
    );
    assert!(ids(&app, "destination").await.is_empty());
    let (status, body) = call(&app, "PUT", "/disabled/_doc/direct", json!({"k":"direct"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(ids(&app, "destination").await.is_empty());
    alias(
        &app,
        "destination",
        "disabled",
        json!({"is_write_index": true}),
    )
    .await;
    let (status, body) = reindex(&app, "source", "disabled").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&app, "destination").await, vec!["one"]);
}
