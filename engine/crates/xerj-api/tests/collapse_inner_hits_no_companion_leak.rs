//! Issue #646 (follow-up to #310): a collapse `inner_hits` block must not ship
//! the generated embedding companions in each member's `_source` under the
//! default projection.
//!
//! The collapse machinery stashes each group member's source BEFORE the engine's
//! `_source` projection runs, so the members carry `*_vector` / `*_vector_chunks`
//! even though #309 keeps them off the top-level `_source`. The leader honours
//! #309; the `inner_hits` underneath it leaked the vectors.

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
        .add_field(FieldConfig::new("grp", FieldType::Keyword))
        .expect("grp field");

    state.engine.create_index("docs", schema).expect("create");
    let idx = state.engine.get_index("docs").expect("get index");
    // Two docs in the same collapse group so the leader has an inner_hits member.
    for id in ["1", "2"] {
        idx.index_document(
            Some(id.into()),
            json!({
                "body": format!("companion leak guard document number {id}. ").repeat(8),
                "grp": "A",
            }),
        )
        .await
        .expect("index document");
    }
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

/// A collapse `inner_hits` member's `_source` must omit the generated companion
/// under the default projection, exactly as the collapse leader's `_source`
/// does (#309 / #646).
#[tokio::test]
async fn collapse_inner_hits_member_source_omits_the_companion() {
    let (app, _dir) = seeded_app().await;
    let (status, response) = post(
        &app,
        "/docs/_search",
        json!({
            "query": { "match_all": {} },
            "size": 10,
            "collapse": {
                "field": "grp",
                "inner_hits": { "name": "members", "size": 10 }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");

    let leader = &response["hits"]["hits"][0];
    // #309 already holds on the leader.
    assert!(
        !leader["_source"]
            .as_object()
            .expect("leader _source")
            .contains_key("body_vector"),
        "the collapse leader must not leak the companion (#309): {response}"
    );

    // The inner_hits members must honour the same default projection (#646).
    let members = leader["inner_hits"]["members"]["hits"]["hits"]
        .as_array()
        .expect("inner_hits members");
    assert!(
        !members.is_empty(),
        "expected inner_hits members: {response}"
    );
    for m in members {
        let src = m["_source"].as_object().expect("member _source");
        assert!(
            !src.contains_key("body_vector"),
            "#646: a collapse inner_hits member must not leak body_vector in _source: {m}"
        );
        assert!(
            !src.contains_key("body_vector_chunks"),
            "#646: a collapse inner_hits member must not leak body_vector_chunks: {m}"
        );
    }
}
