//! Issue #1284: a string that is not a number, sent to a numeric field by a
//! `term` / `terms` / `range` clause, or by a full-text clause that is not
//! lenient, is a query ES cannot build. ES 8.13.4 answers 400 with root cause
//! `failed to create query: For input string: "abc"` (`query_shard_exception`
//! / `number_format_exception`). XERJ answered 200 with 0 hits, so a type
//! error looked like an empty result.
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
    let (st, body) = send(
        &app,
        "PUT",
        "/t84",
        "application/json",
        json!({"mappings": {"properties": {
            "bytes": {"type": "long"},
            "ms": {"type": "float"},
            "h": {"type": "keyword"},
            "msg": {"type": "text"}
        }}})
        .to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create index: {body}");
    let (st, body) = send(
        &app,
        "PUT",
        "/t84/_doc/1?refresh=true",
        "application/json",
        json!({"bytes": 1, "ms": 1.5, "h": "a", "msg": "hello"}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "index doc: {body}");
    (app, dir)
}

async fn count(app: &axum::Router, query: Value) -> (StatusCode, Value) {
    send(
        app,
        "POST",
        "/t84/_count",
        "application/json",
        json!({ "query": query }).to_string(),
    )
    .await
}

/// Every shape here returned 400 on ES 8.13.4 with this exact reason.
#[tokio::test]
async fn non_numeric_term_level_values_are_a_400() {
    let (app, _dir) = seeded().await;
    for query in [
        json!({"term": {"bytes": "abc"}}),
        json!({"term": {"bytes": {"value": "abc"}}}),
        json!({"terms": {"bytes": ["1", "abc"]}}),
        json!({"range": {"bytes": {"gte": "abc"}}}),
        json!({"term": {"ms": "abc"}}),
        json!({"bool": {"filter": [{"term": {"bytes": "abc"}}]}}),
    ] {
        for path in ["/t84/_count", "/t84/_search"] {
            let (st, resp) = send(
                &app,
                "POST",
                path,
                "application/json",
                json!({ "query": query }).to_string(),
            )
            .await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "{path} {query}: {resp}");
            assert!(
                resp.to_string()
                    .contains(r#"failed to create query: For input string: \"abc\""#),
                "{path} {query}: reason must name the bad value: {resp}"
            );
        }
    }
}

/// Valid queries that ES answers 200 must keep answering 200.
#[tokio::test]
async fn numeric_strings_and_non_numeric_fields_stay_valid() {
    let (app, _dir) = seeded().await;
    for (query, hits) in [
        (json!({"term": {"bytes": "1"}}), 1),
        (json!({"term": {"bytes": 1}}), 1),
        // ES: a decimal against a long is a valid query that matches nothing.
        (json!({"term": {"bytes": "1.5"}}), 0),
        (json!({"range": {"bytes": {"gte": "0", "lte": "5"}}}), 1),
        (json!({"term": {"h": "abc"}}), 0),
        // `query_string` without `lenient`: 200 on ES 8.13.4 too.
        (json!({"query_string": {"query": "bytes:abc"}}), 0),
    ] {
        let (st, resp) = count(&app, query.clone()).await;
        assert_eq!(st, StatusCode::OK, "{query}: {resp}");
        assert_eq!(resp["count"], json!(hits), "{query}: {resp}");
    }
}

/// Full-text clauses that are not lenient. Every shape here returned 400 on
/// ES 8.13.4 with this exact reason: `match` with `lenient` absent or false,
/// and `multi_match` / `query_string` / `simple_query_string` whose field set
/// is narrower than every field (so ES defaults `lenient` to false), or that
/// say `lenient: false` outright.
#[tokio::test]
async fn non_lenient_full_text_clauses_are_a_400() {
    let (app, _dir) = seeded().await;
    for query in [
        json!({"match": {"bytes": "abc"}}),
        json!({"match": {"bytes": {"query": "abc", "lenient": false}}}),
        json!({"match": {"ms": "abc"}}),
        json!({"match_phrase": {"bytes": "abc"}}),
        json!({"multi_match": {"query": "abc", "fields": ["bytes", "msg"]}}),
        json!({"multi_match": {"query": "abc", "fields": ["b*"]}}),
        json!({"multi_match": {"query": "abc", "fields": ["bytes"], "type": "phrase"}}),
        json!({"query_string": {"query": "bytes:abc", "lenient": false}}),
        json!({"query_string": {"query": "abc", "default_field": "bytes"}}),
        json!({"query_string": {"query": "abc", "fields": ["bytes", "msg"]}}),
        json!({"query_string": {"query": "bytes:(1 OR abc)", "lenient": false}}),
        json!({"query_string": {"query": "bytes:>abc", "lenient": false}}),
        json!({"query_string": {"query": "bytes:\"abc\"", "lenient": false}}),
        json!({"query_string": {"query": "msg:abc AND bytes:abc", "lenient": false}}),
        json!({"simple_query_string": {"query": "abc", "fields": ["bytes"]}}),
        json!({"simple_query_string": {"query": "abc", "fields": ["bytes", "msg"]}}),
        json!({"bool": {"filter": [{"match": {"bytes": "abc"}}]}}),
    ] {
        for path in ["/t84/_count", "/t84/_search"] {
            let (st, resp) = query_on(&app, path, &query).await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "{path} {query}: {resp}");
            assert!(
                resp.to_string()
                    .contains(r#"failed to create query: For input string: \"abc\""#),
                "{path} {query}: reason must name the bad value: {resp}"
            );
        }
    }
}

/// Lenient full-text clauses stay a 200, as on ES 8.13.4: an explicit
/// `lenient: true`, and the every-field default (`query_string` with no
/// `default_field`, or `["*"]`), which is lenient even for a clause that names
/// the numeric field itself. The two `query_string` ranges were a 200 on main
/// and must not become a 400 now that ranges are checked.
#[tokio::test]
async fn lenient_full_text_clauses_stay_a_200() {
    let (app, _dir) = seeded().await;
    for (query, hits) in [
        (
            json!({"match": {"bytes": {"query": "abc", "lenient": true}}}),
            0,
        ),
        (
            json!({"match": {"bytes": {"query": "abc", "lenient": "true"}}}),
            0,
        ),
        (
            json!({"multi_match": {"query": "abc", "fields": ["bytes", "msg"], "lenient": true}}),
            0,
        ),
        (json!({"query_string": {"query": "bytes:abc"}}), 0),
        (
            json!({"query_string": {"query": "bytes:abc", "lenient": true}}),
            0,
        ),
        (json!({"query_string": {"query": "bytes:>abc"}}), 0),
        (json!({"query_string": {"query": "bytes:[abc TO 5]"}}), 0),
        (json!({"query_string": {"query": "bytes:\"abc\""}}), 0),
        (
            json!({"query_string": {"query": "bytes:>abc", "default_field": "*"}}),
            0,
        ),
        (
            json!({"simple_query_string": {"query": "abc", "fields": ["bytes"], "lenient": true}}),
            0,
        ),
        // Valid values on a numeric field, and full-text on text fields.
        (json!({"match": {"bytes": "1"}}), 1),
        (json!({"match": {"bytes": "1.5"}}), 0),
        (
            json!({"multi_match": {"query": "1", "fields": ["bytes"]}}),
            1,
        ),
        (
            json!({"query_string": {"query": "bytes:1", "lenient": false}}),
            1,
        ),
        (json!({"match": {"msg": "hello"}}), 1),
    ] {
        // `_search` matters on its own: its ES-compat layer rewrites a `match`
        // on a numeric field to a `term` before the engine sees it.
        for path in ["/t84/_count", "/t84/_search"] {
            let (st, resp) = query_on(&app, path, &query).await;
            assert_eq!(st, StatusCode::OK, "{path} {query}: {resp}");
            let got = if path.ends_with("_count") {
                &resp["count"]
            } else {
                &resp["hits"]["total"]["value"]
            };
            assert_eq!(got, &json!(hits), "{path} {query}: {resp}");
        }
    }
}

async fn query_on(app: &axum::Router, path: &str, query: &Value) -> (StatusCode, Value) {
    send(
        app,
        "POST",
        path,
        "application/json",
        json!({ "query": query }).to_string(),
    )
    .await
}

/// `_msearch` fails only the offending item.
#[tokio::test]
async fn non_numeric_value_fails_only_that_msearch_item() {
    let (app, _dir) = seeded().await;
    let ndjson = "{\"index\":\"t84\"}\n{\"query\":{\"term\":{\"bytes\":\"abc\"}}}\n\
                  {\"index\":\"t84\"}\n{\"query\":{\"term\":{\"bytes\":\"1\"}}}\n"
        .to_string();
    let (st, resp) = send(&app, "POST", "/_msearch", "application/x-ndjson", ndjson).await;
    assert_eq!(st, StatusCode::OK, "{resp}");
    let items = resp["responses"].as_array().expect("responses");
    assert_eq!(items[0]["status"], 400, "bad item: {resp}");
    assert_eq!(items[1]["status"], 200, "good item still runs: {resp}");
    assert_eq!(items[1]["hits"]["total"]["value"], 1, "{resp}");
}
