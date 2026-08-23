//! Issue #651: a collapse `inner_hits` block must still render when the request
//! sets an explicit `_source: {"includes": [...]}` that does not list the
//! internal `__xy_collapse_group__` sentinel.
//!
//! The collapse group members are stashed into `__xy_collapse_group__` on the
//! leader's source; the API extracts them at render time. But the engine applies
//! the `_source` include projection to `hit.source` FIRST, dropping that
//! sentinel when it is not in the include list — so the leader came back with no
//! `inner_hits` at all. ES renders `inner_hits` regardless of the top-level
//! `_source` include list.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;
use xerj_common::types::{FieldConfig, FieldType, Schema};

async fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);

    let mut schema = Schema::empty();
    schema
        .add_field(FieldConfig::new("grp", FieldType::Keyword))
        .expect("grp field");
    schema
        .add_field(FieldConfig::new("title", FieldType::Text))
        .expect("title field");
    state.engine.create_index("docs", schema).expect("create");
    let idx = state.engine.get_index("docs").expect("get index");
    for (id, grp) in [("1", "A"), ("2", "A"), ("3", "B")] {
        idx.index_document(
            Some(id.into()),
            json!({ "grp": grp, "title": format!("doc {id}") }),
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
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// A collapse `inner_hits` must render even under an explicit
/// `_source.includes` that omits the collapse sentinel (#651).
#[tokio::test]
async fn collapse_inner_hits_render_under_explicit_source_includes() {
    let (app, _dir) = app().await;

    // Collapse on grp with inner_hits AND an explicit `_source.includes` that
    // lists only `grp` (NOT the internal collapse sentinel).
    let (st, resp) = post(
        &app,
        "/docs/_search",
        json!({
            "query": { "match_all": {} },
            "size": 10,
            "_source": { "includes": ["grp"] },
            "collapse": { "field": "grp", "inner_hits": { "name": "members", "size": 10 } }
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{resp}");

    let leaders = resp["hits"]["hits"].as_array().expect("hits");
    assert!(!leaders.is_empty(), "expected collapse leaders: {resp}");
    // Every leader must carry its inner_hits (the explicit include must not
    // starve them) — #651 fail-before: on main inner_hits is absent.
    for leader in leaders {
        assert!(
            leader
                .pointer("/inner_hits/members/hits/hits")
                .and_then(Value::as_array)
                .is_some(),
            "#651: a collapse leader must render inner_hits under _source.includes: {leader}"
        );
        // And the projection still applies to the leader's own _source.
        let src = leader["_source"].as_object().expect("leader _source");
        assert!(
            src.contains_key("grp") && !src.contains_key("title"),
            "#651: the explicit include still narrows the leader _source to grp: {leader}"
        );
        assert!(
            !src.contains_key("__xy_collapse_group__"),
            "#651: the internal sentinel must not leak on the wire: {leader}"
        );
    }
}
