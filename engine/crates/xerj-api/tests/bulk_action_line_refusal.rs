//! A malformed `_bulk` action line refuses the whole request, as in
//! Elasticsearch 8, and nothing in the body is written.
//!
//! ES parses every action line before it executes any of them
//! (`BulkRequestParser`), so a bad line is a 400 `illegal_argument_exception`
//! naming the line, and the actions before it never run. XERJ used to answer
//! 200 with a per-item error for the bad line and write everything else.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

async fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn send(app: &axum::Router, method: &str, path: &str, body: &str) -> (StatusCode, Value) {
    send_as(app, method, path, "application/x-ndjson", body).await
}

async fn send_as(
    app: &axum::Router,
    method: &str,
    path: &str,
    content_type: &str,
    body: &str,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", content_type)
        .body(Body::from(body.to_owned()))
        .expect("request");
    let response = app.clone().oneshot(req).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn bulk(app: &axum::Router, body: &str) -> (StatusCode, Value) {
    send(app, "POST", "/_bulk?refresh=true", body).await
}

/// The request was refused as a whole with ES's reason.
fn assert_refused(status: StatusCode, got: &Value, reason: &str) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{got}");
    assert_eq!(got["status"], 400, "{got}");
    assert_eq!(got["error"]["type"], "illegal_argument_exception", "{got}");
    assert_eq!(got["error"]["reason"], reason, "{got}");
    assert_eq!(got["error"]["root_cause"][0]["reason"], reason, "{got}");
}

async fn doc_exists(app: &axum::Router, index: &str, id: &str) -> bool {
    let (status, _) = send(app, "GET", &format!("/{index}/_doc/{id}"), "").await;
    status == StatusCode::OK
}

#[tokio::test]
async fn an_empty_action_refuses_the_request_and_writes_nothing() {
    let (app, _dir) = app().await;
    let body = "{\"index\":{\"_index\":\"t\",\"_id\":\"1\"}}\n{\"f1\":\"v1\"}\n{}\n";
    let (status, got) = bulk(&app, body).await;
    assert_refused(
        status,
        &got,
        "Malformed action/metadata line [3], expected FIELD_NAME but found [END_OBJECT]",
    );
    assert!(!doc_exists(&app, "t", "1").await, "the first action ran");
}

#[tokio::test]
async fn an_unknown_parameter_refuses_the_request_and_writes_nothing() {
    let (app, _dir) = app().await;
    // ES 8 dropped mapping types: `_type` is an unknown parameter now.
    let body = "{\"index\":{\"_index\":\"t\",\"_id\":\"1\"}}\n{\"a\":1}\n\
                {\"update\":{\"_index\":\"t\",\"_type\":\"x\",\"_id\":\"1\"}}\n{\"doc\":{\"a\":2}}\n";
    let (status, got) = bulk(&app, body).await;
    assert_refused(
        status,
        &got,
        "Action/metadata line [3] contains an unknown parameter [_type]",
    );
    assert!(!doc_exists(&app, "t", "1").await, "the first action ran");

    // The ES 7 underscore spellings are unknown too.
    for key in ["_version", "_routing", "_retry_on_conflict"] {
        let body =
            format!("{{\"index\":{{\"_index\":\"t\",\"_id\":\"2\",\"{key}\":1}}}}\n{{\"a\":1}}\n");
        let (status, got) = bulk(&app, &body).await;
        assert_refused(
            status,
            &got,
            &format!("Action/metadata line [1] contains an unknown parameter [{key}]"),
        );
    }
    assert!(!doc_exists(&app, "t", "2").await);
}

#[tokio::test]
async fn an_unknown_action_is_named_with_its_line() {
    let (app, _dir) = app().await;
    // Blank lines count toward the line number, as in ES; a delete has no
    // source line, so the line after it is the next action.
    let body = "\n{\"delete\":{\"_index\":\"t\",\"_id\":\"9\"}}\n\n{\"upsert\":{\"_index\":\"t\"}}\n{\"a\":1}\n";
    let (status, got) = bulk(&app, body).await;
    assert_refused(
        status,
        &got,
        "Malformed action/metadata line [4], expected field [create], [delete], [index] or [update] but found [upsert]",
    );
}

#[tokio::test]
async fn each_malformed_shape_gets_es_reason() {
    let (app, _dir) = app().await;
    let cases = [
        ("[]", "Malformed action/metadata line [1], expected START_OBJECT but found [START_ARRAY]"),
        (
            "{\"index\":\"t\"}",
            "Malformed action/metadata line [1], expected START_OBJECT or END_OBJECT but found [VALUE_STRING]",
        ),
        (
            "{\"index\":[]}",
            "Malformed action/metadata line [1], expected START_OBJECT or END_OBJECT but found [START_ARRAY]",
        ),
        (
            "{\"index\":{\"_id\":[\"a\"]}}",
            "Malformed action/metadata line [1], expected a simple value for field [_id] but found [START_ARRAY]",
        ),
        (
            "{\"index\":{\"routing\":{\"a\":1}}}",
            "Malformed action/metadata line [1], expected a simple value for field [routing] but found [START_OBJECT]",
        ),
        (
            // `dynamic_templates` is only accepted as an object.
            "{\"index\":{\"dynamic_templates\":\"x\"}}",
            "Action/metadata line [1] contains an unknown parameter [dynamic_templates]",
        ),
    ];
    for (line, reason) in cases {
        let body = format!("{line}\n{{\"a\":1}}\n");
        let (status, got) = send(&app, "POST", "/t/_bulk", &body).await;
        assert_refused(status, &got, reason);
    }
}

#[tokio::test]
async fn every_parameter_es_accepts_still_runs() {
    let (app, _dir) = app().await;
    let body = concat!(
        "{\"index\":{\"_index\":\"t\",\"_id\":\"1\",\"routing\":\"r\",\"op_type\":\"index\",",
        "\"require_alias\":false,\"require_data_stream\":false,\"list_executed_pipelines\":false,",
        "\"version_type\":\"internal\",\"dynamic_templates\":{},\"ignored\":null}}\n",
        "{\"a\":1}\n",
        "{ \"create\" : { \"_index\" : \"t\", \"_id\" : 2 } }\n",
        "{\"a\":2}\n",
        "{\"update\":{\"_index\":\"t\",\"_id\":\"1\",\"routing\":\"r\",\"retry_on_conflict\":3,",
        "\"_source\":{\"includes\":[\"a\"]}}}\n",
        "{\"doc\":{\"b\":2}}\n",
        "{\"delete\":{\"_index\":\"t\",\"_id\":\"2\"}}\n",
        "{\"index\":{}}\n",
        "{\"a\":3}\n",
    );
    let (status, got) = send(&app, "POST", "/t/_bulk?refresh=true", body).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["errors"], false, "{got}");
    assert_eq!(got["items"].as_array().map(Vec::len), Some(5), "{got}");
    assert!(doc_exists(&app, "t", "1").await);
    assert!(!doc_exists(&app, "t", "2").await);
}

#[tokio::test]
async fn a_line_the_check_cannot_read_keeps_its_per_item_error() {
    let (app, _dir) = app().await;
    // Not JSON: ES fails it in its JSON parser with an error this check does
    // not imitate, so the engine answers it as before.
    let body = concat!(
        "{\"index\":{\"_index\":\"t\",\"_id\":\"1\"}}\n{\"a\":1}\n",
        "{\"index\":}\n{\"b\":1}\n",
        "{\"index\":{\"_index\":\"t\",\"_id\":\"3\"}}\n{\"c\":1}\n",
    );
    let (status, got) = bulk(&app, body).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["errors"], true, "{got}");
    assert_eq!(got["items"][1]["index"]["status"], 400, "{got}");
    assert!(doc_exists(&app, "t", "1").await);
    assert!(doc_exists(&app, "t", "3").await);
}

#[tokio::test]
async fn a_client_in_7x_compatibility_may_still_send_type() {
    let (app, _dir) = app().await;
    let body = "{\"index\":{\"_index\":\"t\",\"_type\":\"_doc\",\"_id\":\"1\"}}\n{\"a\":1}\n";
    let (status, got) = send_as(
        &app,
        "POST",
        "/_bulk?refresh=true",
        "application/vnd.elasticsearch+x-ndjson; compatible-with=7",
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["errors"], false, "{got}");
    assert!(doc_exists(&app, "t", "1").await);

    // The same body from an 8.x client is refused.
    let (status, got) = send_as(
        &app,
        "POST",
        "/_bulk",
        "application/vnd.elasticsearch+x-ndjson; compatible-with=8",
        &body.replace("\"1\"", "\"2\""),
    )
    .await;
    assert_refused(
        status,
        &got,
        "Action/metadata line [1] contains an unknown parameter [_type]",
    );
}
