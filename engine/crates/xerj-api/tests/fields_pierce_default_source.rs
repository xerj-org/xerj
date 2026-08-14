//! Issue #310 — a `fields` clause must see past the default `_source`
//! projection, and must not drag the projection open on the wire.
//!
//! Since #309 a request with no `_source` clause gets `SourceFilter::Default`
//! and the engine strips the generated embedding companions before the response
//! layer runs. `fields` and `docvalue_fields` resolve their values out of that
//! same `hit.source`, so a caller who explicitly named `body_vector` in `fields`
//! got nothing back — silently, since an unresolvable `fields` entry is legally
//! omitted — while the identical clause under `"_source": false` returned every
//! float, because `Enabled(false)` deliberately keeps the raw source.
//!
//! Every assertion below is paired. The POSITIVE half (the value comes back) is
//! the fix; the NEGATIVE half (`_source` still has no companion key) is what
//! makes the test worth having, because the fix works by asking the engine for
//! the intact source and re-narrowing it at emission — miss one emission site
//! and #309's guarantee regresses to a full-vector response with the positive
//! assertions still passing.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;
use xerj_common::types::{EmbeddingConfig, FieldConfig, FieldType, Schema};

const DIMS: usize = 32;

async fn seeded_app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);

    let mut schema = Schema::empty();
    let mut body = FieldConfig::new("body", FieldType::Text);
    body.options.dimensions = Some(DIMS);
    body.options.similarity = Some("cosine".into());
    body.embedding = Some(EmbeddingConfig {
        endpoint: None,
        model: None,
        target_field: Some("body_vector".into()),
    });
    schema.add_field(body).expect("body field");
    let mut companion = FieldConfig::new("body_vector", FieldType::Vector);
    companion.options.dimensions = Some(DIMS);
    companion.options.similarity = Some("cosine".into());
    schema.add_field(companion).expect("body_vector field");
    schema
        .add_field(FieldConfig::new("title", FieldType::Keyword))
        .expect("title field");

    state.engine.create_index("docs", schema).expect("create");
    let idx = state.engine.get_index("docs").expect("get index");
    idx.index_document(
        Some("1".into()),
        json!({
            "body": "XERJ resolves an explicitly named embedding companion \
                     through the fields API without widening _source. "
                .repeat(8),
            "title": "first",
        }),
    )
    .await
    .expect("index document");
    idx.refresh().await.expect("refresh");

    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn post(app: &axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::post(path)
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
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn first_hit(response: &Value) -> &Value {
    &response["hits"]["hits"][0]
}

/// The three spellings of the same `fields` request must agree.
///
/// `_source` omitted was the broken one: it returned nothing while `_source:
/// false` — a strictly *stronger* statement of "do not send me the document" —
/// returned all 32 floats.
#[tokio::test]
async fn fields_resolves_a_named_companion_under_every_source_spelling() {
    let (app, _dir) = seeded_app().await;

    for source_clause in [None, Some(json!(false)), Some(json!(true))] {
        let mut body = json!({
            "query": { "match_all": {} },
            "size": 1,
            "fields": ["body_vector"],
        });
        if let Some(clause) = source_clause.clone() {
            body["_source"] = clause;
        }
        let label = source_clause
            .as_ref()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "omitted".to_string());

        let (status, response) = post(&app, "/docs/_search", body).await;
        assert_eq!(status, StatusCode::OK, "_source {label}: {response}");

        let values = first_hit(&response)["fields"]["body_vector"]
            .as_array()
            .unwrap_or_else(|| panic!("_source {label}: no fields.body_vector in {response}"));
        assert_eq!(
            values.len(),
            DIMS,
            "_source {label}: fields.body_vector must carry the whole vector: {response}"
        );
    }
}

/// The #309 guarantee, restated as the negative control for the fix above: the
/// pierce hands the engine's intact source to the `fields` builder, so the ONLY
/// thing keeping the vector off the wire is the emission-time re-narrowing.
#[tokio::test]
async fn piercing_for_fields_does_not_widen_source_on_the_wire() {
    let (app, _dir) = seeded_app().await;

    let (status, response) = post(
        &app,
        "/docs/_search",
        json!({
            "query": { "match_all": {} },
            "size": 1,
            "fields": ["body_vector"],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");

    let hit = first_hit(&response);
    let source = hit["_source"]
        .as_object()
        .unwrap_or_else(|| panic!("_source must still be emitted: {response}"));
    assert!(
        !source.contains_key("body_vector"),
        "#309: the default projection must not return the generated companion in _source: {response}"
    );
    assert!(
        !source.contains_key("body_vector_chunks"),
        "#309: the default projection must not return the generated chunks in _source: {response}"
    );
    assert_eq!(
        source.get("title"),
        Some(&json!("first")),
        "ordinary user fields must survive the re-narrowing: {response}"
    );
    // …and the value the caller actually asked for is still there.
    assert_eq!(
        hit["fields"]["body_vector"]
            .as_array()
            .map(|v| v.len())
            .unwrap_or(0),
        DIMS,
        "{response}"
    );
}

/// A request that names nothing special must be byte-identical to the #309
/// default: no pierce, no `fields` block, no companion anywhere.
#[tokio::test]
async fn a_plain_default_search_is_untouched() {
    let (app, _dir) = seeded_app().await;

    let (status, response) = post(
        &app,
        "/docs/_search",
        json!({ "query": { "match_all": {} }, "size": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");

    let source = first_hit(&response)["_source"]
        .as_object()
        .expect("_source object");
    assert!(!source.contains_key("body_vector"), "{response}");
    assert!(!source.contains_key("body_vector_chunks"), "{response}");
    assert!(source.contains_key("body"), "{response}");
}

/// `docvalue_fields` is the same resolution path, and used to emit the wrong
/// thing rather than nothing: `{"body_vector": []}` — a positive claim that the
/// document has no values for that field. A field that resolves to nothing is
/// omitted; a field that resolves is complete. Never an empty array.
#[tokio::test]
async fn docvalue_fields_never_emits_a_bare_empty_array() {
    let (app, _dir) = seeded_app().await;

    let (status, response) = post(
        &app,
        "/docs/_search",
        json!({
            "query": { "match_all": {} },
            "size": 1,
            "docvalue_fields": ["body_vector", "no_such_field"],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");

    let hit = first_hit(&response);
    assert_eq!(
        hit["fields"]["body_vector"]
            .as_array()
            .map(|v| v.len())
            .unwrap_or(0),
        DIMS,
        "docvalue_fields must resolve the named companion: {response}"
    );
    assert!(
        hit["fields"].get("no_such_field").is_none(),
        "an unresolvable docvalue_field is omitted, not emitted as []: {response}"
    );
    let source = hit["_source"].as_object().expect("_source object");
    assert!(
        !source.contains_key("body_vector"),
        "#309 must hold on the docvalue_fields route too: {response}"
    );
}

/// Issue #310 claim 2: a dotted `embedding.target_field` used to route the
/// default projection through the nested `_source` filter, whose empty-object
/// pruning deleted unrelated `{}` fields from every hit of every search.
#[tokio::test]
async fn a_dotted_target_field_keeps_unrelated_empty_objects() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);

    let mut schema = Schema::empty();
    let mut body = FieldConfig::new("body", FieldType::Text);
    body.options.dimensions = Some(DIMS);
    body.options.similarity = Some("cosine".into());
    body.embedding = Some(EmbeddingConfig {
        endpoint: None,
        model: None,
        target_field: Some("semantic.embedding".into()),
    });
    schema.add_field(body).expect("body field");

    state.engine.create_index("dotted", schema).expect("create");
    let idx = state.engine.get_index("dotted").expect("get index");
    idx.index_document(
        Some("1".into()),
        json!({ "body": "a dotted target field is a literal top-level key", "meta": {} }),
    )
    .await
    .expect("index document");
    idx.refresh().await.expect("refresh");
    let app = xerj_api::router::build_es_compat_router(state);

    let (status, response) = post(
        &app,
        "/dotted/_search",
        json!({ "query": { "match_all": {} }, "size": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");

    let source = first_hit(&response)["_source"]
        .as_object()
        .expect("_source object");
    assert_eq!(
        source.get("meta"),
        Some(&json!({})),
        "an unrelated empty object must survive a dotted embedding target: {response}"
    );
    assert!(
        !source.contains_key("semantic.embedding"),
        "the dotted companion itself must still be omitted: {response}"
    );
}
