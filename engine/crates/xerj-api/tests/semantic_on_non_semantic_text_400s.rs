//! A `semantic` query naming a field that is not a `semantic_text` (a plain
//! text/keyword field, or absent) must fail with a `400`, not silently degrade
//! — issue #530, the symmetric sibling of the `knn` case (#498).
//!
//! With no active embedding backend, the built-in lexical embedder only
//! auto-embeds `semantic_text` fields; a `semantic` query against anything else
//! has no comparable stored vector to match, so it must be rejected loudly.
//!
//! That 400 is for an index the caller named. Issue #2305: the same query
//! against a wildcard, `_all`, or alias must not fail the whole request when
//! only some of the expanded indices map the field. Those indices contribute
//! no hits (and a shard failure); indices that do map the field still answer.
//!
//! Elasticsearch is referenced for wire semantics only; no code from it is
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

async fn json_req(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A `semantic` query on a plain text field (not `semantic_text`) must 400.
#[tokio::test]
async fn semantic_on_text_field_is_400() {
    let (app, _dir) = app().await;
    let (status, body) = json_req(
        &app,
        "PUT",
        "/notes",
        json!({ "mappings": { "properties": {
            "title": { "type": "text" },
            "tag": { "type": "keyword" }
        } } }),
    )
    .await;
    assert!(status.is_success(), "create index: {status} {body}");
    let (_s, _b) = json_req(
        &app,
        "POST",
        "/notes/_doc/1",
        json!({ "title": "hello world", "tag": "a" }),
    )
    .await;
    let (_s, _b) = json_req(&app, "POST", "/notes/_refresh", json!({})).await;

    let (status, body) = json_req(
        &app,
        "POST",
        "/notes/_search",
        json!({ "query": { "semantic": { "field": "title", "query": "greetings" } } }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "semantic on a non-semantic_text field must be 400 (#530): got {status} {body}"
    );
    assert!(
        body.pointer("/error/reason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("title"),
        "the 400 must name the field (#530): {body}"
    );
}

/// A `semantic` query on a field absent from the mapping must 400.
#[tokio::test]
async fn semantic_on_absent_field_is_400() {
    let (app, _dir) = app().await;
    let (status, body) = json_req(
        &app,
        "PUT",
        "/notes",
        json!({ "mappings": { "properties": { "title": { "type": "text" } } } }),
    )
    .await;
    assert!(status.is_success(), "create index: {status} {body}");

    let (status, body) = json_req(
        &app,
        "POST",
        "/notes/_search",
        json!({ "query": { "semantic": { "field": "nonexistent_vec", "query": "greetings" } } }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "semantic on an absent field must be 400 (#530): got {status} {body}"
    );
    assert!(
        body.pointer("/error/reason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("nonexistent_vec"),
        "the 400 must name the field (#530): {body}"
    );
}

fn semantic_query() -> Value {
    json!({ "query": { "semantic": { "field": "body", "query": "walrus clams" } } })
}

async fn put_ok(app: &axum::Router, path: &str, body: Value) {
    let (status, resp) = json_req(app, "PUT", path, body).await;
    assert!(status.is_success(), "{path}: {status} {resp}");
}

/// `proj-notes` maps `body` as `semantic_text` and holds one document.
/// `proj-tags` is a plain keyword index and holds a different document.
async fn seed_proj_pair(app: &axum::Router) {
    put_ok(
        app,
        "/proj-notes",
        json!({ "mappings": { "properties": { "body": { "type": "semantic_text" } } } }),
    )
    .await;
    put_ok(
        app,
        "/proj-tags",
        json!({ "mappings": { "properties": { "label": { "type": "keyword" } } } }),
    )
    .await;
    put_ok(
        app,
        "/proj-notes/_doc/note-1?refresh=true",
        json!({ "body": "The walrus eats clams on the ice floe." }),
    )
    .await;
    put_ok(
        app,
        "/proj-tags/_doc/tag-1?refresh=true",
        json!({ "label": "walrus" }),
    )
    .await;
}

fn hit_ids_from(body: &Value, index: &str) -> Vec<String> {
    body["hits"]["hits"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter(|h| h["_index"].as_str() == Some(index))
        .filter_map(|h| h["_id"].as_str().map(str::to_string))
        .collect()
}

fn error_reason(body: &Value) -> String {
    body.pointer("/error/reason")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// #2305: `proj-*` covers one `semantic_text` index and one without the field.
/// The semantic query must be HTTP 200 with hits from the index that has it.
#[tokio::test]
async fn semantic_wildcard_skips_indices_without_the_field() {
    let (app, _dir) = app().await;
    seed_proj_pair(&app).await;

    let (status, body) = json_req(&app, "POST", "/proj-*/_search", semantic_query()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "wildcard semantic search must be 200 (#2305): {body}"
    );
    assert_eq!(
        hit_ids_from(&body, "proj-notes"),
        vec!["note-1".to_string()],
        "the index that maps body as semantic_text must answer: {body}"
    );
    assert!(
        hit_ids_from(&body, "proj-tags").is_empty(),
        "the index without the field must contribute no hits: {body}"
    );
    assert_eq!(
        body["_shards"]["failed"], 1,
        "the skipped index is a shard failure, not a silent success: {body}"
    );
    let failures = body["_shards"]["failures"]
        .as_array()
        .expect("shard failures");
    assert!(
        failures.iter().any(|f| {
            f["index"].as_str() == Some("proj-tags")
                && f.pointer("/reason/reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .contains("body")
        }),
        "the shard note must name the skipped index and the field: {body}"
    );

    // The same shape wrapped the way a bool filter is written still peels to
    // the semantic executor, so the wildcard must keep working.
    let inner = semantic_query()["query"].clone();
    let (status, body) = json_req(
        &app,
        "POST",
        "/proj-*/_search",
        json!({ "query": { "bool": { "must": [inner] } } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "bool-wrapped semantic: {body}");
    assert_eq!(
        hit_ids_from(&body, "proj-notes"),
        vec!["note-1".to_string()],
        "{body}"
    );
}

/// An alias is expansion too: members without the field are skipped.
#[tokio::test]
async fn semantic_alias_skips_members_without_the_field() {
    let (app, _dir) = app().await;
    seed_proj_pair(&app).await;
    put_ok(&app, "/proj-notes/_alias/proj", json!({})).await;
    put_ok(&app, "/proj-tags/_alias/proj", json!({})).await;

    let (status, body) = json_req(&app, "POST", "/proj/_search", semantic_query()).await;
    assert_eq!(status, StatusCode::OK, "alias semantic search: {body}");
    assert_eq!(
        hit_ids_from(&body, "proj-notes"),
        vec!["note-1".to_string()],
        "{body}"
    );
    assert!(hit_ids_from(&body, "proj-tags").is_empty(), "{body}");
}

/// A concrete name in the selector still 400s, including beside another name.
#[tokio::test]
async fn semantic_on_an_explicit_index_in_a_list_is_still_400() {
    let (app, _dir) = app().await;
    seed_proj_pair(&app).await;

    let (status, body) = json_req(&app, "POST", "/proj-tags/_search", semantic_query()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        error_reason(&body).contains("body"),
        "explicit index must still name the field: {body}"
    );

    let (status, body) = json_req(
        &app,
        "POST",
        "/proj-notes,proj-tags/_search",
        semantic_query(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a concrete name in a comma list must still 400 (#2305): {body}"
    );
    assert!(error_reason(&body).contains("body"), "{body}");
}

/// A wildcard whose every match lacks the field still 400s, so a wrong field
/// name is not reported as an empty result.
#[tokio::test]
async fn semantic_wildcard_is_400_when_no_index_has_the_field() {
    let (app, _dir) = app().await;
    put_ok(
        &app,
        "/plain-a",
        json!({ "mappings": { "properties": { "label": { "type": "keyword" } } } }),
    )
    .await;
    put_ok(
        &app,
        "/plain-b",
        json!({ "mappings": { "properties": { "label": { "type": "keyword" } } } }),
    )
    .await;

    let (status, body) = json_req(&app, "POST", "/plain-*/_search", semantic_query()).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "nothing to search must still be 400: {body}"
    );
    assert!(
        error_reason(&body).contains("body"),
        "the 400 must name the field: {body}"
    );
}

/// `_msearch` fans out on its own. The same wildcard must not 400 the item.
#[tokio::test]
async fn semantic_msearch_wildcard_skips_indices_without_the_field() {
    let (app, _dir) = app().await;
    seed_proj_pair(&app).await;
    let ndjson = format!("{}\n{}\n", json!({ "index": "proj-*" }), semantic_query());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/_msearch")
                .header("content-type", "application/x-ndjson")
                .body(Body::from(ndjson))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::OK, "msearch envelope: {body}");
    let item = &body["responses"][0];
    assert_eq!(
        item["status"], 200,
        "msearch item must be 200 (#2305): {body}"
    );
    assert_eq!(
        hit_ids_from(item, "proj-notes"),
        vec!["note-1".to_string()],
        "{body}"
    );
    assert!(hit_ids_from(item, "proj-tags").is_empty(), "{body}");
}
