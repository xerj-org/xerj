//! Issue #2294: an index created with a persisted `index.default_pipeline`
//! kept reporting the binding from `GET /_settings` after a clean restart
//! (the response falls back to `settings.json`), and the pipeline definition
//! survived in cluster state, but an ordinary write no longer ran it.
//!
//! `Engine::default_pipeline_for` reads the in-memory `index_settings` map.
//! That map was filled on create and on `PUT /_settings`, and the boot,
//! failed-index retry, and snapshot-restore open paths never copied
//! `settings.json` back into it. Explicit `?pipeline=` still worked because
//! it does not consult the map.
//!
//! `index.final_pipeline` is not a setting this build reads.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

fn app_on(dir: &std::path::Path) -> (axum::Router, xerj_api::state::AppState) {
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    let app = xerj_api::router::build_es_compat_router(state.clone());
    (app, state)
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

/// The pipeline from the report: copy `source_marker` onto `copied_marker`.
/// The ES `copy` processor is stored as `copy_field`.
async fn define_copy_pipeline(app: &axum::Router) {
    let (status, body) = send(
        app,
        put(
            "/_ingest/pipeline/restart-copy",
            json!({
                "processors": [{
                    "copy": { "source": "source_marker", "target": "copied_marker" }
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

fn assert_copied(doc: &Value, marker: &str) {
    assert_eq!(
        doc["_source"]["source_marker"].as_str(),
        Some(marker),
        "source field missing: {doc}"
    );
    assert_eq!(
        doc["_source"]["copied_marker"].as_str(),
        Some(marker),
        "default pipeline did not copy the field: {doc}"
    );
}

/// Dotted `index.default_pipeline` (the spelling in #2294) and the nested
/// spelling both have to keep transforming writes after the process that
/// created the index is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persisted_default_pipeline_runs_after_clean_restart() {
    let dir = tempfile::tempdir().expect("data dir");

    {
        let (app, state) = app_on(dir.path());
        define_copy_pipeline(&app).await;

        let (status, body) = send(
            &app,
            put(
                "/restart-probe",
                json!({
                    "settings": { "index.default_pipeline": "restart-copy" },
                    "mappings": { "properties": {
                        "source_marker": { "type": "keyword" },
                        "copied_marker": { "type": "keyword" }
                    }}
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let (status, body) = send(
            &app,
            put(
                "/nested-probe",
                json!({
                    "settings": { "index": { "default_pipeline": "restart-copy" } },
                    "mappings": { "properties": {
                        "source_marker": { "type": "keyword" },
                        "copied_marker": { "type": "keyword" }
                    }}
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        // #2281 stored a template pipeline on the index at create. That copy
        // lives in settings.json and has to come back on open too.
        let (status, body) = send(
            &app,
            put(
                "/_index_template/restart-template",
                json!({
                    "index_patterns": ["from-template-*"],
                    "priority": 100,
                    "template": {
                        "settings": { "index.default_pipeline": "restart-copy" }
                    }
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = send(&app, put("/from-template-probe", json!({}))).await;
        assert_eq!(status, StatusCode::OK, "{body}");

        // A template registered after the index exists must not become that
        // index's pipeline on the next open.
        let (status, body) = send(&app, put("/plain-probe", json!({}))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = send(
            &app,
            put(
                "/_index_template/plain-template",
                json!({
                    "index_patterns": ["plain-*"],
                    "priority": 100,
                    "template": {
                        "settings": { "index.default_pipeline": "restart-copy" }
                    }
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let (status, body) = send(
            &app,
            put(
                "/restart-probe/_doc/before?refresh=true",
                json!({ "source_marker": "before-clean-restart" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let (status, doc) = send(&app, get("/restart-probe/_doc/before")).await;
        assert_eq!(status, StatusCode::OK, "{doc}");
        assert_copied(&doc, "before-clean-restart");

        assert_eq!(
            state
                .engine
                .default_pipeline_for("restart-probe")
                .as_deref(),
            Some("restart-copy")
        );
        assert_eq!(
            state.engine.default_pipeline_for("nested-probe").as_deref(),
            Some("restart-copy")
        );
        assert_eq!(
            state
                .engine
                .default_pipeline_for("from-template-probe")
                .as_deref(),
            Some("restart-copy")
        );
        assert_eq!(
            state.engine.default_pipeline_for("plain-probe"),
            None,
            "a template added after create must not bind the existing index"
        );

        drop(app);
        drop(state);
    }

    let (app, state) = app_on(dir.path());

    let (status, settings) = send(&app, get("/restart-probe/_settings")).await;
    assert_eq!(status, StatusCode::OK, "{settings}");
    assert_eq!(
        settings.pointer("/restart-probe/settings/index/index.default_pipeline"),
        Some(&json!("restart-copy")),
        "GET _settings must still report the persisted binding: {settings}"
    );
    let (status, settings) = send(&app, get("/nested-probe/_settings")).await;
    assert_eq!(status, StatusCode::OK, "{settings}");
    assert_eq!(
        settings.pointer("/nested-probe/settings/index/default_pipeline"),
        Some(&json!("restart-copy")),
        "nested default_pipeline must still be reported: {settings}"
    );

    let (status, pipeline) = send(&app, get("/_ingest/pipeline/restart-copy")).await;
    assert_eq!(status, StatusCode::OK, "{pipeline}");

    assert_eq!(
        state
            .engine
            .default_pipeline_for("restart-probe")
            .as_deref(),
        Some("restart-copy"),
        "the in-memory binding must be rebuilt from settings.json"
    );
    assert_eq!(
        state.engine.default_pipeline_for("nested-probe").as_deref(),
        Some("restart-copy"),
        "the nested spelling must be rebuilt too"
    );
    assert_eq!(
        state
            .engine
            .default_pipeline_for("from-template-probe")
            .as_deref(),
        Some("restart-copy"),
        "a pipeline copied from a template at create must survive restart"
    );
    assert_eq!(
        state.engine.default_pipeline_for("plain-probe"),
        None,
        "reopen must not attach a template the index was not created with"
    );

    let (status, before) = send(&app, get("/restart-probe/_doc/before")).await;
    assert_eq!(status, StatusCode::OK, "{before}");
    assert_copied(&before, "before-clean-restart");

    let (status, body) = send(
        &app,
        put(
            "/restart-probe/_doc/after?refresh=true",
            json!({ "source_marker": "after-clean-restart" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, doc) = send(&app, get("/restart-probe/_doc/after")).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_copied(&doc, "after-clean-restart");

    let (status, body) = send(
        &app,
        ndjson(
            "/_bulk?refresh=true",
            "{\"index\":{\"_index\":\"nested-probe\",\"_id\":\"after\"}}\n{\"source_marker\":\"after-nested\"}\n",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["errors"], false, "{body}");
    let (status, doc) = send(&app, get("/nested-probe/_doc/after")).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_copied(&doc, "after-nested");

    let (status, body) = send(
        &app,
        put(
            "/from-template-probe/_doc/after?refresh=true",
            json!({ "source_marker": "after-template" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, doc) = send(&app, get("/from-template-probe/_doc/after")).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_copied(&doc, "after-template");

    let (status, body) = send(
        &app,
        put(
            "/plain-probe/_doc/after?refresh=true",
            json!({ "source_marker": "after-plain" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, doc) = send(&app, get("/plain-probe/_doc/after")).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["_source"]["source_marker"], "after-plain", "{doc}");
    assert!(
        doc["_source"].get("copied_marker").is_none(),
        "plain-probe must stay unbound after restart: {doc}"
    );
}
