//! Issue #199 from the outside: the ES-compat ILM surface no longer accepts a
//! retention policy it will not honour.
//!
//! The reported defect, verbatim from the wire:
//!
//! ```text
//! PUT _ilm/policy/logs-30d   -> 200 OK
//! GET _ilm/policy/logs-30d   -> their policy, exactly as written
//! ```
//!
//! …and then nothing ever ran, so the index grew forever while the API kept
//! reporting success. Every test here fails on the pre-fix tree: the handlers
//! stored any JSON body, the GET double-wrapped it in `policy`, there was no
//! `_ilm/explain`, no `_ilm/status`, and no executor to run.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use xerj_api::{router::build_es_compat_router, state::AppState};
use xerj_common::{config::Config, metrics::Metrics};
use xerj_engine::Engine;

const DAY_MS: i64 = 86_400_000;

fn app() -> (axum::Router, AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    let metrics = Metrics::new().expect("metrics");
    let engine = Engine::new(config.clone()).expect("engine");
    let state = AppState::new(config, engine, metrics);
    (build_es_compat_router(state.clone()), state, dir)
}

async fn send(app: &axum::Router, method: &str, uri: &str, body: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

const DELETE_POLICY: &str =
    r#"{"policy":{"phases":{"delete":{"min_age":"30d","actions":{"delete":{}}}}}}"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retention_policy_put_over_http_actually_deletes_the_index() {
    let (app, state, dir) = app();

    let (status, _) = send(&app, "PUT", "/_ilm/policy/logs-30d", DELETE_POLICY).await;
    assert_eq!(status, StatusCode::OK, "a delete-phase policy is accepted");

    let (status, _) = send(
        &app,
        "PUT",
        "/logs-000001",
        r#"{"settings":{"index.lifecycle.name":"logs-30d"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The index is managed, and says so.
    let (status, explained) = send(&app, "GET", "/logs-000001/_ilm/explain", "").await;
    assert_eq!(status, StatusCode::OK);
    let entry = &explained["indices"]["logs-000001"];
    assert_eq!(entry["managed"], true, "{explained}");
    assert_eq!(entry["policy"], "logs-30d");
    assert_eq!(entry["xerj"]["executable"], true);
    assert_eq!(entry["xerj"]["next_phase"], "delete");

    // Age it past min_age with an injected clock and run one pass.
    let now = xerj_engine::ilm::now_ms();
    let report = state.engine.run_ilm_once(now + 31 * DAY_MS).await;
    assert_eq!(
        report.deleted,
        vec!["logs-000001".to_string()],
        "the delete phase ran: {report:?}"
    );

    let (status, _) = send(&app, "GET", "/logs-000001", "").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the index is really gone over the wire"
    );
    drop(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_policy_whose_actions_we_do_not_execute_is_refused_not_stored() {
    let (app, _state, _dir) = app();

    // The canonical ES logs policy: rollover in hot, forcemerge in warm.
    let body = r#"{"policy":{"phases":{
        "hot":{"actions":{"rollover":{"max_age":"1d","max_size":"50gb"}}},
        "warm":{"min_age":"7d","actions":{"forcemerge":{"max_num_segments":1}}},
        "delete":{"min_age":"30d","actions":{"delete":{}}}
    }}}"#;
    let (status, err) = send(&app, "PUT", "/_ilm/policy/logs-full", body).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "accepting this policy would silently ignore rollover + forcemerge"
    );
    let reason = err["error"]["reason"].as_str().unwrap_or_default();
    assert!(reason.contains("hot.rollover"), "{reason}");
    assert!(reason.contains("warm.forcemerge"), "{reason}");
    assert_eq!(err["error"]["type"], "illegal_argument_exception");

    // …and nothing was stored: a rejected policy must not be half-live.
    let (status, _) = send(&app, "GET", "/_ilm/policy/logs-full", "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unparsable_min_age_is_refused_rather_than_treated_as_zero() {
    let (app, _state, _dir) = app();
    // "30" without a unit: if this were silently read as 0 ms the user's data
    // would be deleted on the next pass instead of in 30 days.
    let body = r#"{"policy":{"phases":{"delete":{"min_age":"30","actions":{"delete":{}}}}}}"#;
    let (status, err) = send(&app, "PUT", "/_ilm/policy/oops", body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    assert!(
        err["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("min_age"),
        "{err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_policy_round_trips_in_the_elasticsearch_shape() {
    let (app, _state, _dir) = app();
    send(&app, "PUT", "/_ilm/policy/logs-30d", DELETE_POLICY).await;

    let (status, body) = send(&app, "GET", "/_ilm/policy/logs-30d", "").await;
    assert_eq!(status, StatusCode::OK);
    // Pre-fix this was `{"logs-30d":{"policy":{"policy":{"phases":…}}}}` —
    // the stored body still carried its own envelope.
    assert_eq!(
        body["logs-30d"]["policy"]["phases"]["delete"]["min_age"], "30d",
        "{body}"
    );
    assert!(
        body["logs-30d"]["policy"]["policy"].is_null(),
        "the policy envelope is not double-wrapped: {body}"
    );

    let (status, all) = send(&app, "GET", "/_ilm/policy", "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(all["logs-30d"]["policy"]["phases"].is_object(), "{all}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_reports_whether_retention_is_actually_running() {
    let (app, _state, _dir) = app();

    let (status, body) = send(&app, "GET", "/_ilm/status", "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["operation_mode"], "RUNNING");
    assert_eq!(
        body["xerj"]["executable_actions"],
        json!(["delete", "readonly"]),
        "status names exactly what this build executes: {body}"
    );

    let (status, body) = send(&app, "POST", "/_ilm/stop", "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["operation_mode"], "STOPPED");
    let (_, body) = send(&app, "GET", "/_ilm/status", "").await;
    assert_eq!(body["operation_mode"], "STOPPED");

    let (_, body) = send(&app, "POST", "/_ilm/start", "").await;
    assert_eq!(body["operation_mode"], "RUNNING");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_policy_still_in_use_is_refused() {
    let (app, _state, _dir) = app();
    send(&app, "PUT", "/_ilm/policy/logs-30d", DELETE_POLICY).await;
    send(
        &app,
        "PUT",
        "/logs-000002",
        r#"{"settings":{"index.lifecycle.name":"logs-30d"}}"#,
    )
    .await;

    let (status, err) = send(&app, "DELETE", "/_ilm/policy/logs-30d", "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    assert!(
        err["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("logs-000002"),
        "the error names the index still using it: {err}"
    );

    // Detach, then the delete goes through.
    let (status, _) = send(
        &app,
        "PUT",
        "/logs-000002/_settings",
        r#"{"index":{"lifecycle":{"name":null}}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&app, "DELETE", "/_ilm/policy/logs-30d", "").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attaching_a_policy_by_put_settings_puts_the_index_under_management() {
    let (app, state, _dir) = app();
    send(&app, "PUT", "/_ilm/policy/logs-30d", DELETE_POLICY).await;
    send(&app, "PUT", "/late-attach", "{}").await;

    let (_, explained) = send(&app, "GET", "/late-attach/_ilm/explain", "").await;
    assert_eq!(
        explained["indices"]["late-attach"]["managed"], false,
        "unmanaged before the settings call: {explained}"
    );

    // The flat spelling ES also accepts — the nested one is covered by the
    // detach in `deleting_a_policy_still_in_use_is_refused`.
    let (status, _) = send(
        &app,
        "PUT",
        "/late-attach/_settings",
        r#"{"index.lifecycle.name":"logs-30d"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, explained) = send(&app, "GET", "/late-attach/_ilm/explain", "").await;
    assert_eq!(
        explained["indices"]["late-attach"]["managed"], true,
        "{explained}"
    );

    let now = xerj_engine::ilm::now_ms();
    let report = state.engine.run_ilm_once(now + 31 * DAY_MS).await;
    assert_eq!(
        report.deleted,
        vec!["late-attach".to_string()],
        "{report:?}"
    );
}
