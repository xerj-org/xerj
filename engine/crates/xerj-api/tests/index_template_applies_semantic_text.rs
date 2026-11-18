//! Issue #2281: a composable index template that declares `semantic_text`
//! and `index.default_pipeline` was stored and read back, and a `keyword`
//! field from it landed on a matching index, but the semantic field was
//! mapped as `object` and the default pipeline was not copied onto the
//! index. `xerj autoindex` creates that index with `PUT /{index}` (its own
//! mappings and settings), and the first bulk write can also auto-create
//! it. Both have to come out the way an explicit create that named the
//! field and the pipeline does.
//!
//! Elasticsearch is referenced for wire semantics only. It is AGPL-3.0 /
//! SSPL-1.0 / Elastic-2.0 licensed and no code from it is reproduced here.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = app.clone().oneshot(req).await.expect("response");
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .expect("body");
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

fn put(path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

fn post(path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .expect("request")
}

fn ndjson(path: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/x-ndjson")
        .body(Body::from(body.to_string()))
        .expect("request")
}

/// The pipeline from the report: copy `impl` onto `diagnostic_semantic`.
/// The ES `copy` processor is stored as `copy_field` and the config keys
/// (`source`, `target`) pass through, which is what that stage reads.
async fn define_copy_pipeline(app: &axum::Router, id: &str) {
    let (status, body) = send(
        app,
        put(
            &format!("/_ingest/pipeline/{id}"),
            json!({
                "processors": [{
                    "copy": { "source": "impl", "target": "diagnostic_semantic" }
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// The template from the report: dotted `index.default_pipeline`, a
/// `semantic_text` field, and a `keyword` marker that already applied.
async fn define_template(app: &axum::Router, name: &str, pattern: &str, pipeline: &str) {
    let (status, body) = send(
        app,
        put(
            &format!("/_index_template/{name}"),
            json!({
                "index_patterns": [pattern],
                "priority": 100,
                "template": {
                    "settings": { "index.default_pipeline": pipeline },
                    "mappings": {
                        "properties": {
                            "diagnostic_semantic": { "type": "semantic_text" },
                            "diagnostic_template_marker": { "type": "keyword" }
                        }
                    }
                }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["acknowledged"], true, "{body}");
}

fn field_type<'a>(mapping: &'a Value, index: &str, field: &str) -> Option<&'a str> {
    mapping
        .pointer(&format!("/{index}/mappings/properties/{field}/type"))
        .and_then(Value::as_str)
}

const IMPL_TEXT: &str = "graph edges connect nodes in a network";

async fn assert_template_mapping_and_pipeline(app: &axum::Router, index: &str, pipeline: &str) {
    let (status, mapping) = send(app, get(&format!("/{index}/_mapping"))).await;
    assert_eq!(status, StatusCode::OK, "{mapping}");
    assert_eq!(
        field_type(&mapping, index, "diagnostic_semantic"),
        Some("semantic_text"),
        "{mapping}"
    );
    assert_eq!(
        field_type(&mapping, index, "diagnostic_template_marker"),
        Some("keyword"),
        "{mapping}"
    );

    let (status, settings) = send(app, get(&format!("/{index}/_settings"))).await;
    assert_eq!(status, StatusCode::OK, "{settings}");
    assert_eq!(
        settings
            .pointer(&format!("/{index}/settings/index/default_pipeline"))
            .and_then(Value::as_str),
        Some(pipeline),
        "{settings}"
    );
}

async fn assert_copied_and_searchable(app: &axum::Router, index: &str, id: &str) {
    let (status, doc) = send(app, get(&format!("/{index}/_doc/{id}"))).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["_source"]["impl"], IMPL_TEXT, "{doc}");
    assert_eq!(
        doc["_source"]["diagnostic_semantic"], IMPL_TEXT,
        "the default pipeline must copy impl: {doc}"
    );

    let (status, hits) = send(
        app,
        post(
            &format!("/{index}/_search"),
            json!({
                "query": {
                    "semantic": {
                        "field": "diagnostic_semantic",
                        "query": "graph edges"
                    }
                }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{hits}");
    let total = hits["hits"]["total"]["value"].as_u64().unwrap_or(0);
    assert!(
        total >= 1,
        "semantic_text must be embedded, not just spelled in the mapping: {hits}"
    );
}

#[tokio::test]
async fn an_empty_create_applies_semantic_text_and_the_default_pipeline() {
    let (app, _dir) = app();
    define_copy_pipeline(&app, "js2281-copy").await;
    define_template(&app, "js2281", "js2281-*", "js2281-copy").await;

    let (status, body) = send(&app, put("/js2281-plain", json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_template_mapping_and_pipeline(&app, "js2281-plain", "js2281-copy").await;

    let (status, body) = send(
        &app,
        put(
            "/js2281-plain/_doc/1?refresh=true",
            json!({ "impl": IMPL_TEXT }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_copied_and_searchable(&app, "js2281-plain", "1").await;
}

#[tokio::test]
async fn an_autoindex_create_body_keeps_the_template_field_and_pipeline() {
    let (app, _dir) = app();
    define_copy_pipeline(&app, "js2281-copy").await;
    define_template(&app, "js2281", "js2281-*", "js2281-copy").await;

    // The body `xerj autoindex --code-analyzer code` PUTs: its own text
    // fields, the code analyzer, one WAL shard, and the dotted strict-unknown
    // setting. The template's fields are not in this body.
    let (status, body) = send(
        &app,
        put(
            "/js2281-auto",
            json!({
                "settings": {
                    "analysis": { "analyzer": { "default": { "type": "code" } } },
                    "index": { "xerj_ingest_shards": 1 },
                    "index.query.strict_unknown_fields": true
                },
                "mappings": { "properties": {
                    "impl": { "type": "text" },
                    "path": { "type": "keyword" },
                    "ax_paths": { "type": "keyword" }
                } }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_template_mapping_and_pipeline(&app, "js2281-auto", "js2281-copy").await;

    let (status, mapping) = send(&app, get("/js2281-auto/_mapping")).await;
    assert_eq!(status, StatusCode::OK, "{mapping}");
    assert_eq!(field_type(&mapping, "js2281-auto", "impl"), Some("text"));
    assert_eq!(field_type(&mapping, "js2281-auto", "path"), Some("keyword"));

    let (_, settings) = send(&app, get("/js2281-auto/_settings")).await;
    assert_eq!(
        settings.pointer("/js2281-auto/settings/index/xerj_ingest_shards"),
        Some(&json!("1")),
        "{settings}"
    );

    let (status, search) = send(
        &app,
        post(
            "/js2281-auto/_search",
            json!({ "query": { "term": { "not_a_field": "x" } } }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the dotted strict setting must still be in force: {search}"
    );

    let (status, bulk) = send(
        &app,
        ndjson(
            "/_bulk?refresh=true",
            &format!(
                "{}\n{}\n",
                json!({"index": {"_index": "js2281-auto", "_id": "1"}}),
                json!({"impl": IMPL_TEXT, "path": "src/a.ts"})
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{bulk}");
    assert_eq!(bulk["errors"], false, "{bulk}");
    assert_copied_and_searchable(&app, "js2281-auto", "1").await;
}

#[tokio::test]
async fn the_first_bulk_write_auto_creates_with_the_template() {
    let (app, _dir) = app();
    define_copy_pipeline(&app, "js2281-copy").await;
    define_template(&app, "js2281", "js2281-*", "js2281-copy").await;

    let (status, bulk) = send(
        &app,
        ndjson(
            "/_bulk?refresh=true",
            &format!(
                "{}\n{}\n",
                json!({"index": {"_index": "js2281-bulk", "_id": "1"}}),
                json!({"impl": IMPL_TEXT})
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{bulk}");
    assert_eq!(bulk["errors"], false, "{bulk}");
    assert_template_mapping_and_pipeline(&app, "js2281-bulk", "js2281-copy").await;
    assert_copied_and_searchable(&app, "js2281-bulk", "1").await;
}

#[tokio::test]
async fn the_first_document_write_auto_creates_with_the_template() {
    let (app, _dir) = app();
    define_copy_pipeline(&app, "js2281-copy").await;
    define_template(&app, "js2281", "js2281-*", "js2281-copy").await;

    let (status, body) = send(
        &app,
        put(
            "/js2281-doc/_doc/1?refresh=true",
            json!({ "impl": IMPL_TEXT }),
        ),
    )
    .await;
    assert!(
        status == StatusCode::CREATED || status == StatusCode::OK,
        "{status} {body}"
    );
    assert_template_mapping_and_pipeline(&app, "js2281-doc", "js2281-copy").await;
    assert_copied_and_searchable(&app, "js2281-doc", "1").await;
}

#[tokio::test]
async fn the_request_wins_on_a_field_and_on_the_pipeline_it_names() {
    let (app, _dir) = app();
    define_copy_pipeline(&app, "js2281-copy").await;
    let (status, body) = send(
        &app,
        put(
            "/_ingest/pipeline/js2281-set",
            json!({ "processors": [{ "set": { "field": "env", "value": "prod" } }] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    define_template(&app, "js2281", "js2281-*", "js2281-copy").await;

    let (status, body) = send(
        &app,
        put(
            "/js2281-override",
            json!({
                "mappings": { "properties": {
                    "diagnostic_semantic": { "type": "keyword" }
                } }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, mapping) = send(&app, get("/js2281-override/_mapping")).await;
    assert_eq!(
        field_type(&mapping, "js2281-override", "diagnostic_semantic"),
        Some("keyword"),
        "{mapping}"
    );
    assert_eq!(
        field_type(&mapping, "js2281-override", "diagnostic_template_marker"),
        Some("keyword"),
        "{mapping}"
    );
    let (_, settings) = send(&app, get("/js2281-override/_settings")).await;
    assert_eq!(
        settings.pointer("/js2281-override/settings/index/default_pipeline"),
        Some(&json!("js2281-copy")),
        "{settings}"
    );

    let (status, body) = send(
        &app,
        put(
            "/js2281-pipe",
            json!({ "settings": { "index": { "default_pipeline": "js2281-set" } } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, settings) = send(&app, get("/js2281-pipe/_settings")).await;
    assert_eq!(
        settings.pointer("/js2281-pipe/settings/index/default_pipeline"),
        Some(&json!("js2281-set")),
        "{settings}"
    );
    let (status, body) = send(
        &app,
        put(
            "/js2281-pipe/_doc/1?refresh=true",
            json!({ "impl": IMPL_TEXT }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (_, doc) = send(&app, get("/js2281-pipe/_doc/1")).await;
    assert_eq!(doc["_source"]["env"], "prod", "{doc}");
    assert!(
        doc["_source"].get("diagnostic_semantic").is_none(),
        "the request pipeline replaced the template copy: {doc}"
    );
}

#[tokio::test]
async fn an_index_outside_the_pattern_does_not_inherit_the_template() {
    let (app, _dir) = app();
    define_copy_pipeline(&app, "js2281-copy").await;
    define_template(&app, "js2281", "js2281-*", "js2281-copy").await;

    let (status, body) = send(&app, put("/other-plain", json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, mapping) = send(&app, get("/other-plain/_mapping")).await;
    assert!(
        field_type(&mapping, "other-plain", "diagnostic_semantic").is_none(),
        "{mapping}"
    );
    let (_, settings) = send(&app, get("/other-plain/_settings")).await;
    assert!(
        settings
            .pointer("/other-plain/settings/index/default_pipeline")
            .is_none(),
        "{settings}"
    );
}
