//! Issue #3171: a boolean field works alone in `query_string` but matches
//! nothing once combined with another clause. Index `p` with `active` mapped
//! `boolean` and 300 docs (163 with `active: false`, 41 of those with
//! `tag: red`):
//!
//! ```text
//! query_string "active:false"                 163
//! query_string "tag:red"                       77
//! query_string "tag:red AND active:false"       0    (41 in the data)
//! query_string "NOT active:true"              300    (163)
//! query_string "active:false OR tag:red"       77    (199)
//! ```
//!
//! Root cause: in `es_compat.rs`, `match` on numeric and boolean fields is
//! rewritten to an exact `term`, except when the clause is `lenient` and its
//! value does not parse as a number (`lenient_non_numeric`, from #1284).
//! That exemption was meant for numeric fields, but `"true"` and `"false"`
//! never parse as numbers, so every lenient boolean clause stays a `match`.
//! `query_string` without `default_field` is lenient by default, so the
//! boolean clause inside a compound query stays a `match`, which matches
//! nothing there. Elasticsearch 8.x returns the combined counts.

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
        "/p",
        "application/json",
        json!({"mappings": {"properties": {
            "active": {"type": "boolean"},
            "tag": {"type": "keyword"},
            "n": {"type": "long"}
        }}})
        .to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create index: {body}");
    // 300 docs: 41 active:false/tag:red/n:1, 122 active:false/tag:blue,
    // 36 active:true/tag:red, 101 active:true/tag:blue (n:2 elsewhere).
    let mut ndjson = String::new();
    let mut id = 0;
    for (count, active, tag, n) in [
        (41, false, "red", 1),
        (122, false, "blue", 2),
        (36, true, "red", 2),
        (101, true, "blue", 2),
    ] {
        for _ in 0..count {
            id += 1;
            ndjson.push_str(&format!(
                "{{\"index\":{{\"_id\":\"{id}\"}}}}\n{{\"active\":{active},\"tag\":\"{tag}\",\"n\":{n}}}\n"
            ));
        }
    }
    let (st, body) = send(
        &app,
        "POST",
        "/p/_bulk?refresh=true",
        "application/x-ndjson",
        ndjson,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "bulk index: {body}");
    assert_eq!(body["errors"], json!(false), "bulk errors: {body}");
    (app, dir)
}

async fn hits(app: &axum::Router, path: &str, query: &Value) -> (StatusCode, Value) {
    let (st, resp) = send(
        app,
        "POST",
        path,
        "application/json",
        json!({ "query": query }).to_string(),
    )
    .await;
    let got = if path.ends_with("_count") {
        resp["count"].clone()
    } else {
        resp["hits"]["total"]["value"].clone()
    };
    (st, got)
}

/// The reported cases: a lone boolean clause works, but AND / NOT / OR
/// combinations return the wrong count. Checked twice: once with the docs
/// memtable-resident (stored-doc scan path) and once after `_flush`
/// (segment path), so the counts cannot change at flush.
#[tokio::test]
async fn boolean_field_combines_in_query_string() {
    let (app, _dir) = seeded().await;
    let cases = [
        (json!({"query_string": {"query": "active:false"}}), 163),
        (json!({"query_string": {"query": "tag:red"}}), 77),
        (
            json!({"query_string": {"query": "tag:red AND active:false"}}),
            41,
        ),
        (json!({"query_string": {"query": "NOT active:true"}}), 163),
        (
            json!({"query_string": {"query": "active:false OR tag:red"}}),
            199,
        ),
        // The same shape as an explicit bool query, lenient and not.
        (
            json!({"bool": {"must": [
                {"match": {"tag": "red"}},
                {"match": {"active": {"query": "false", "lenient": true}}},
            ]}}),
            41,
        ),
        (
            json!({"bool": {"must": [
                {"match": {"tag": "red"}},
                {"match": {"active": "false"}},
            ]}}),
            41,
        ),
    ];
    check_cases(&app, &cases).await;
    let (st, body) = send(&app, "POST", "/p/_flush", "application/json", String::new()).await;
    assert_eq!(st, StatusCode::OK, "flush: {body}");
    check_cases(&app, &cases).await;
}

async fn check_cases(app: &axum::Router, cases: &[(Value, u64)]) {
    for (query, want) in cases {
        for path in ["/p/_count", "/p/_search"] {
            let (st, got) = hits(app, path, query).await;
            assert_eq!(st, StatusCode::OK, "{path} {query}");
            assert_eq!(got, json!(*want), "{path} {query}");
        }
    }
}

/// Keyword and integer fields still combine correctly, and the #1284
/// lenient exemption for numeric fields is unchanged: a lenient `match`
/// whose value is not a number stays a 200 with 0 hits.
#[tokio::test]
async fn numeric_lenient_and_other_fields_unchanged() {
    let (app, _dir) = seeded().await;
    for (query, want) in [
        (json!({"query_string": {"query": "tag:red AND n:1"}}), 41),
        (json!({"query_string": {"query": "n:1"}}), 41),
        (json!({"query_string": {"query": "n:abc"}}), 0),
        (
            json!({"match": {"n": {"query": "abc", "lenient": true}}}),
            0,
        ),
        // A lenient boolean clause with a value that is not a boolean
        // still answers 200 with 0 hits: the term rewrite leaves a value
        // the boolean predicate refuses untouched, matching nothing.
        (
            json!({"match": {"active": {"query": "abc", "lenient": true}}}),
            0,
        ),
        (json!({"match": {"active": "abc"}}), 0),
    ] {
        for path in ["/p/_count", "/p/_search"] {
            let (st, got) = hits(&app, path, &query).await;
            assert_eq!(st, StatusCode::OK, "{path} {query}");
            assert_eq!(got, json!(want), "{path} {query}");
        }
    }
}
