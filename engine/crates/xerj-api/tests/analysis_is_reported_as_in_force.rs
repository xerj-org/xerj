//! Issue #1092: an index's analysis configuration must be observable over the
//! wire exactly as search applies it.
//!
//! Before the fix:
//! * `GET /{index}/_settings` replayed an in-memory display copy of the create
//!   body. After a restart that copy is empty, so a declared
//!   `analysis.analyzer.default` stemmer vanished from the response while
//!   search kept stemming — and a top-level `analysis` beside an `index` block
//!   was never echoed at all.
//! * `POST /{index}/_analyze` ignored the index entirely: an inline lowercase
//!   splitter answered every request, so a stemmer index showed `bagels`
//!   where search indexes `bagel`.
//!
//! An agent verifying "is my stemmer on?" through the ES wire concluded it was
//! off. Elasticsearch and OpenSearch are referenced for wire semantics only
//! (public API docs: `_analyze` without `analyzer`/`field` uses the index's
//! default analyzer; `field` uses that field's analyzer); no code is reproduced.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

fn app_over(dir: &std::path::Path) -> axum::Router {
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
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

fn with_body(method: &str, path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

fn get(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).expect("request")
}

async fn analyze(app: &axum::Router, path: &str, body: Value) -> (StatusCode, Vec<String>, Value) {
    let (st, resp) = send(app, with_body("POST", path, body)).await;
    let tokens = resp["tokens"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|t| t["token"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    (st, tokens, resp)
}

const STEM: &str = "/stem";

async fn create_stem_index(app: &axum::Router) {
    let (st, body) = send(
        app,
        with_body(
            "PUT",
            STEM,
            json!({
                "settings": { "analysis": { "analyzer": { "default": { "type": "stemmer" } } } },
                "mappings": { "properties": {
                    "t": { "type": "text" }, "tag": { "type": "keyword" }
                }}
            }),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create: {body}");
}

#[tokio::test]
async fn settings_report_the_analysis_in_force_across_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let want = json!({ "analyzer": { "default": { "type": "stemmer" } } });
    {
        let app = app_over(dir.path());
        create_stem_index(&app).await;
        let (st, body) = send(&app, get("/stem/_settings")).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            body.pointer("/stem/settings/index/analysis"),
            Some(&want),
            "{body}"
        );
    }
    // Restart: only what reached disk survives.
    let app = app_over(dir.path());
    let (_, body) = send(&app, get("/stem/_settings")).await;
    assert_eq!(
        body.pointer("/stem/settings/index/analysis"),
        Some(&want),
        "the declared stemmer must still be reported after a restart: {body}"
    );
    let (_, body) = send(&app, get("/stem")).await;
    assert_eq!(
        body.pointer("/stem/settings/index/analysis"),
        Some(&want),
        "GET /{{index}} reports the same block: {body}"
    );
    // Search still stems after the restart — the thing being reported.
    let (_, tokens, _) = analyze(&app, "/stem/_analyze", json!({ "text": "bagels" })).await;
    assert_eq!(tokens, ["bagel"]);
}

#[tokio::test]
async fn top_level_analysis_beside_an_index_block_is_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_over(dir.path());
    let (st, body) = send(
        &app,
        with_body(
            "PUT",
            "/both",
            json!({ "settings": {
                "index": { "number_of_replicas": 0 },
                "analysis": { "analyzer": { "default": { "type": "stemmer" } } }
            }}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create: {body}");
    let (_, body) = send(&app, get("/both/_settings")).await;
    assert_eq!(
        body.pointer("/both/settings/index/analysis/analyzer/default/type"),
        Some(&json!("stemmer")),
        "{body}"
    );
    assert_eq!(
        body.pointer("/both/settings/index/number_of_replicas"),
        Some(&json!("0"))
    );
}

#[tokio::test]
async fn analyze_uses_the_index_default_and_the_field_mapping() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_over(dir.path());
    create_stem_index(&app).await;

    // No analyzer, no field → the index's default analyzer.
    let (st, tokens, body) =
        analyze(&app, "/stem/_analyze", json!({ "text": "Bagels running" })).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(tokens, ["bagel", "run"], "{body}");

    // A text field → the analyzer that field is indexed with.
    let (_, tokens, _) = analyze(
        &app,
        "/stem/_analyze",
        json!({ "field": "t", "text": "Bagels running" }),
    )
    .await;
    assert_eq!(tokens, ["bagel", "run"]);

    // A keyword field → one exact token.
    let (_, tokens, _) = analyze(
        &app,
        "/stem/_analyze",
        json!({ "field": "tag", "text": "Bagels running" }),
    )
    .await;
    assert_eq!(tokens, ["Bagels running"]);

    // An explicitly named built-in still wins over the default.
    let (_, tokens, _) = analyze(
        &app,
        "/stem/_analyze",
        json!({ "analyzer": "standard", "text": "Bagels running" }),
    )
    .await;
    assert_eq!(tokens, ["bagels", "running"]);

    // An index WITHOUT an analysis block analyses as `standard`.
    let (st, _) = send(&app, with_body("PUT", "/plain", json!({}))).await;
    assert_eq!(st, StatusCode::OK);
    let (_, tokens, _) =
        analyze(&app, "/plain/_analyze", json!({ "text": "Bagels running" })).await;
    assert_eq!(tokens, ["bagels", "running"]);
    let (_, body) = send(&app, get("/plain/_settings")).await;
    assert!(
        body.pointer("/plain/settings/index/analysis").is_none(),
        "no analysis is in force, so none is reported: {body}"
    );
}

#[tokio::test]
async fn analyze_refuses_what_it_cannot_honour() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_over(dir.path());
    create_stem_index(&app).await;

    for (path, body) in [
        ("/stem/_analyze", json!({ "analyzer": "nope", "text": "x" })),
        ("/_analyze", json!({ "analyzer": "nope", "text": "x" })),
        ("/_analyze", json!({ "field": "t", "text": "x" })),
        (
            "/_analyze",
            json!({ "tokenizer": "standard", "filter": ["no_such_filter"], "text": "x" }),
        ),
        (
            "/_analyze",
            json!({ "tokenizer": "no_such_tokenizer", "text": "x" }),
        ),
        (
            "/_analyze",
            json!({ "normalizer": "lowercase", "text": "x" }),
        ),
    ] {
        let (st, _, resp) = analyze(&app, path, body.clone()).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{path} {body}: {resp}");
        assert_eq!(
            resp.pointer("/error/type"),
            Some(&json!("illegal_argument_exception")),
            "{resp}"
        );
    }

    // Unknown index → 404, as ES and OpenSearch answer.
    let (st, _, _) = analyze(&app, "/missing/_analyze", json!({ "text": "x" })).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // The global endpoint runs the real built-in pipelines.
    let (st, tokens, _) = analyze(
        &app,
        "/_analyze",
        json!({ "analyzer": "english", "text": "the running bagels" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(tokens, ["run", "bagel"]);
    let (_, tokens, _) = analyze(&app, "/_analyze", json!({ "text": "Foo BAR" })).await;
    assert_eq!(tokens, ["foo", "bar"]);
}

/// An ad-hoc chain is built by the same registry code index creation uses,
/// with the Elasticsearch/OpenSearch defaults (filters alone → `keyword`
/// tokenizer) — it used to be answered by the `standard` splitter whatever
/// it named.
#[tokio::test]
async fn analyze_builds_an_adhoc_chain() {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app_over(dir.path());
    let (st, tokens, body) = analyze(
        &app,
        "/_analyze",
        json!({ "tokenizer": "whitespace", "filter": ["lowercase"], "text": "Foo-Bar BAZ" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(tokens, ["foo-bar", "baz"]);

    let (_, tokens, _) = analyze(
        &app,
        "/_analyze",
        json!({ "filter": ["lowercase"], "text": "Foo Bar" }),
    )
    .await;
    assert_eq!(
        tokens,
        ["foo bar"],
        "filters alone run on the keyword tokenizer"
    );
}
