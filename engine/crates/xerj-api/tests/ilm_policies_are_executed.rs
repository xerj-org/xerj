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

// ─────────────────────────────────────────────────────────────────────────────
// Repairs found by adversarial review of the first cut of this change.
// ─────────────────────────────────────────────────────────────────────────────

/// Detaching over HTTP must actually stop retention, in both spellings ES
/// accepts.
///
/// The first cut returned `{"acknowledged":true}` and then ignored it: the
/// detach cleared the in-memory attachment while the resolver fell back to the
/// index's persisted `settings.json`, which still carried the create-time
/// `index.lifecycle.name`. `_ilm/explain` still said `managed: true`, and the
/// next pass **deleted the index** — an acknowledged "stop deleting my data"
/// that deleted the data anyway. `deleting_a_policy_still_in_use_is_refused`
/// above performs this exact detach and asserted only the 200, which is how it
/// got through.
async fn detach_stops_retention(body: &str) {
    let (app, state, _dir) = app();
    send(&app, "PUT", "/_ilm/policy/logs-30d", DELETE_POLICY).await;
    let (status, _) = send(
        &app,
        "PUT",
        "/logs-000009",
        r#"{"settings":{"index.lifecycle.name":"logs-30d"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, ack) = send(&app, "PUT", "/logs-000009/_settings", body).await;
    assert_eq!(status, StatusCode::OK, "{ack}");
    assert_eq!(ack["acknowledged"], true, "{ack}");

    let (_, explained) = send(&app, "GET", "/logs-000009/_ilm/explain", "").await;
    assert_eq!(
        explained["indices"]["logs-000009"]["managed"], false,
        "an acknowledged detach must be visible in _ilm/explain: {explained}"
    );

    // `GET /_settings` must not still advertise the policy either — ES drops
    // the setting on a null rather than reporting it as null.
    let (_, settings) = send(&app, "GET", "/logs-000009/_settings", "").await;
    assert!(
        !settings.to_string().contains("lifecycle"),
        "detached index still reports a lifecycle setting, in some spelling: {settings}"
    );

    let now = xerj_engine::ilm::now_ms();
    let report = state.engine.run_ilm_once(now + 31 * DAY_MS).await;
    assert!(
        report.deleted.is_empty(),
        "a detached index must never be deleted: {report:?}"
    );

    let (status, _) = send(&app, "GET", "/logs-000009", "").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the index is still there over the wire, 31 days later"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_nested_detach_actually_stops_retention() {
    detach_stops_retention(r#"{"index":{"lifecycle":{"name":null}}}"#).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_flat_detach_actually_stops_retention() {
    detach_stops_retention(r#"{"index.lifecycle.name":null}"#).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_settings_put_that_does_not_mention_the_policy_does_not_detach() {
    // `index.lifecycle.origination_date` mentions `lifecycle` but says nothing
    // about the policy. Reading "does this body mention lifecycle?" and then
    // taking the absent name as a detach silently un-manages the index — the
    // same accepted-and-ignored failure from the other direction.
    let (app, state, _dir) = app();
    send(&app, "PUT", "/_ilm/policy/logs-30d", DELETE_POLICY).await;
    send(
        &app,
        "PUT",
        "/logs-000010",
        r#"{"settings":{"index.lifecycle.name":"logs-30d"}}"#,
    )
    .await;

    let origination = xerj_engine::ilm::now_ms() - 40 * DAY_MS;
    let (status, _) = send(
        &app,
        "PUT",
        "/logs-000010/_settings",
        &format!(r#"{{"index":{{"lifecycle":{{"origination_date":{origination}}}}}}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, explained) = send(&app, "GET", "/logs-000010/_ilm/explain", "").await;
    assert_eq!(
        explained["indices"]["logs-000010"]["managed"], true,
        "still managed — that body never asked to detach: {explained}"
    );
    let report = state.engine.run_ilm_once(xerj_engine::ilm::now_ms()).await;
    assert_eq!(
        report.deleted,
        vec!["logs-000010".to_string()],
        "and the origination_date it *did* set is honoured: {report:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_policy_an_upgraded_index_still_points_at_is_refused() {
    // The in-use check has to ask the same resolver the executor asks. Scanning
    // the attachment map alone missed an index that points at the policy only
    // through its persisted settings — so the DELETE returned 200 while the
    // executor was still managing that index, and the next pass skipped it
    // with "policy not found": retention silently stopped.
    let (app, state, _dir) = app();
    send(&app, "PUT", "/_ilm/policy/logs-30d", DELETE_POLICY).await;
    send(
        &app,
        "PUT",
        "/legacy-logs",
        r#"{"settings":{"index.lifecycle.name":"logs-30d"}}"#,
    )
    .await;
    // A node upgraded from before ilm_state.json existed: settings.json is the
    // only record that this index was ever attached.
    state.engine.ilm_index_state.remove("legacy-logs");

    let (status, err) = send(&app, "DELETE", "/_ilm/policy/logs-30d", "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    assert!(
        err["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("legacy-logs"),
        "and it names the index the executor would still act on: {err}"
    );

    // Detaching is what makes it deletable — and the detach is real.
    send(
        &app,
        "PUT",
        "/legacy-logs/_settings",
        r#"{"index.lifecycle.name":null}"#,
    )
    .await;
    let (status, _) = send(&app, "DELETE", "/_ilm/policy/logs-30d", "").await;
    assert_eq!(status, StatusCode::OK);
    let report = state
        .engine
        .run_ilm_once(xerj_engine::ilm::now_ms() + 31 * DAY_MS)
        .await;
    assert!(report.deleted.is_empty(), "{report:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ilm_remove_detaches_the_index_for_real() {
    // ES's own detach verb, which this engine did not implement at all — so
    // the only way to stop retention was the settings PUT that was being
    // ignored. Both routes now record the same tombstone.
    let (app, state, _dir) = app();
    send(&app, "PUT", "/_ilm/policy/logs-30d", DELETE_POLICY).await;
    send(
        &app,
        "PUT",
        "/logs-000011",
        r#"{"settings":{"index.lifecycle.name":"logs-30d"}}"#,
    )
    .await;

    let (status, body) = send(&app, "POST", "/logs-000011/_ilm/remove", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["has_failures"], false, "{body}");

    let (_, explained) = send(&app, "GET", "/logs-000011/_ilm/explain", "").await;
    assert_eq!(
        explained["indices"]["logs-000011"]["managed"], false,
        "{explained}"
    );
    let report = state
        .engine
        .run_ilm_once(xerj_engine::ilm::now_ms() + 31 * DAY_MS)
        .await;
    assert!(report.deleted.is_empty(), "{report:?}");
    let (status, _) = send(&app, "GET", "/logs-000011", "").await;
    assert_eq!(status, StatusCode::OK, "the index survives");

    // Idempotent: removing again is not an error, and the policy is now free.
    let (status, _) = send(&app, "POST", "/logs-000011/_ilm/remove", "").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&app, "DELETE", "/_ilm/policy/logs-30d", "").await;
    assert_eq!(status, StatusCode::OK);
}

/// The repair that added `POST /{index}/_ilm/remove` shipped the very defect
/// this PR exists to remove: the endpoint accepted an index name that was not
/// an index, answered `200 has_failures:false`, and wrote a permanent detach
/// tombstone for it.
///
/// `resolve_index_selector` returns a literal name whether or not it exists
/// ("include whether or not it exists; the caller decides"), so the handler's
/// own `targets.is_empty()` 404 guard was dead for exactly the case it was
/// written for. Measured before the fix: 200 calls to distinct names left 200
/// entries and a 4129-byte `ilm_state.json`, each call rewriting the whole
/// file — unbounded, unreachable persisted state driven from the public ES
/// port. ES answers 404 `index_not_found_exception`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ilm_remove_on_a_name_that_is_not_an_index_is_404_and_writes_nothing() {
    let (app, state, dir) = app();
    send(&app, "PUT", "/_ilm/policy/logs-30d", DELETE_POLICY).await;

    let (status, body) = send(&app, "POST", "/ghost-0/_ilm/remove", "").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "detaching a name that is not an index is not a success: {body}"
    );

    // The settings route is the same handler's twin and had the same hole.
    let (status, body) = send(
        &app,
        "PUT",
        "/ghost-1/_settings",
        r#"{"index":{"lifecycle":{"name":null}}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // Neither call left anything behind, in memory or on disk.
    assert!(
        state.engine.ilm_index_state.is_empty(),
        "no tombstone for a phantom index: {:?}",
        state
            .engine
            .ilm_index_state
            .iter()
            .map(|e| e.key().clone())
            .collect::<Vec<_>>()
    );
    let persisted = std::fs::read_to_string(dir.path().join("ilm_state.json")).unwrap_or_default();
    assert!(
        !persisted.contains("ghost-0") && !persisted.contains("ghost-1"),
        "ilm_state.json grew a phantom entry: {persisted}"
    );
}

/// The distinction the previous fix's doc comment glossed over: "the index
/// exists but nothing manages it" really is idempotent, and must stay a 200.
/// Only "this name is not an index" is the 404.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ilm_remove_on_an_existing_unmanaged_index_is_still_a_success() {
    let (app, state, _dir) = app();
    let (status, _) = send(&app, "PUT", "/plain-000001", "{}").await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(&app, "POST", "/plain-000001/_ilm/remove", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["has_failures"], false, "{body}");
    assert_eq!(body["failed_indexes"], json!([]), "{body}");

    // And a wildcard that matches nothing is ES's `allow_no_indices=true`: an
    // empty success, not a 404, and still no state written.
    let (status, body) = send(&app, "POST", "/nomatch-*/_ilm/remove", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["has_failures"], false, "{body}");
    assert!(
        !state.engine.ilm_index_state.contains_key("nomatch-*"),
        "a pattern is never an index name"
    );
}

/// `GET /_ilm/status`'s `managed_indices` counted `ilm_index_state` entries
/// rather than asking the executor's resolver, so it reported `0` for the one
/// population that resolver exists to catch: an index upgraded from before
/// `ilm_state.json`, managed through its persisted `settings.json` alone.
/// A retention feature that deletes an index while its status endpoint says
/// nothing is managed is not observable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_counts_the_upgraded_index_the_executor_would_actually_delete() {
    let (app, state, _dir) = app();
    send(&app, "PUT", "/_ilm/policy/logs-30d", DELETE_POLICY).await;
    send(
        &app,
        "PUT",
        "/logs-upgraded",
        r#"{"settings":{"index.lifecycle.name":"logs-30d"}}"#,
    )
    .await;

    // Simulate the upgrade: the recorded entry did not exist before this
    // release, only the index's own settings.
    state.engine.ilm_index_state.remove("logs-upgraded");

    let (status, body) = send(&app, "GET", "/_ilm/status", "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["xerj"]["managed_indices"], 1,
        "the resolver says this index is managed, so status must too: {body}"
    );

    // And the executor agrees — which is the point of asking one resolver.
    let report = state
        .engine
        .run_ilm_once(xerj_engine::ilm::now_ms() + 31 * DAY_MS)
        .await;
    assert_eq!(
        report.deleted,
        vec!["logs-upgraded".to_string()],
        "{report:?}"
    );

    let (_, body) = send(&app, "GET", "/_ilm/status", "").await;
    assert_eq!(
        body["xerj"]["managed_indices"], 0,
        "and a deleted index stops being counted: {body}"
    );
}

/// `_ilm/explain` had the two target cases exactly inverted — a mistyped index
/// name answered `200 {"managed": false}` and a wildcard matching nothing
/// answered `404`, the opposite of both ES and of the detach routes next to it.
///
/// The 200 is the one that matters. This endpoint is the only place an operator
/// can ask "is retention running on this index?", and answering "nothing is
/// managing it" about a name that is not an index reads as reassurance while
/// the index they actually meant is being deleted on a timer.
///
/// ES: `IndicesOptions.strictExpandOpen()`
/// (`RestExplainLifecycleAction.java:42`) = `ERROR_WHEN_UNAVAILABLE_TARGETS`
/// plus `allowEmptyExpressions(true)` (`IndicesOptions.java:561-563`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explain_404s_a_name_that_is_not_an_index_and_200s_an_empty_wildcard() {
    let (app, _state, _dir) = app();

    // A literal name that is not an index is an error, not a reassuring
    // "managed: false".
    let (status, body) = send(&app, "GET", "/typo-idx/_ilm/explain", "").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "explaining a name that is not an index must not answer 'unmanaged': {body}"
    );

    // A wildcard resolving to nothing is an empty success.
    let (status, body) = send(&app, "GET", "/nomatch-*/_ilm/explain", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["indices"], json!({}), "{body}");

    // And a real index still explains, so the guard did not just break the
    // endpoint.
    send(&app, "PUT", "/_ilm/policy/logs-30d", DELETE_POLICY).await;
    let (status, _) = send(
        &app,
        "PUT",
        "/real-idx",
        r#"{"settings":{"index.lifecycle.name":"logs-30d"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = send(&app, "GET", "/real-idx/_ilm/explain", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["indices"]["real-idx"]["managed"], true, "{body}");
    assert_eq!(body["indices"]["real-idx"]["policy"], "logs-30d", "{body}");
}
