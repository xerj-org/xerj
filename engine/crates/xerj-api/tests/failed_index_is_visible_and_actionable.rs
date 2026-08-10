//! Issue #206, over HTTP: a failed index must be visible on the surfaces an
//! operator actually watches, and actionable from them.
//!
//! Three separate failures were reported and all three are pinned here.
//!
//! 1. **The index was invisible.** `_cat/indices` and `_cluster/state` list
//!    only indices that opened, so the broken one appeared nowhere; `DELETE`
//!    answered 404 `index_not_found`. The only recovery was to stop the server
//!    and remove the directory by hand.
//! 2. **Readiness went hard-fail.** `/health/ready` returned 503 for any red
//!    status, so one broken index removed a pod holding 199 healthy ones from
//!    service permanently — a single-index problem escalated to a total
//!    outage.
//! 3. **The health surfaces disagreed.** `/v1/health`, `/v1/cluster/health`
//!    and `/health/ready` used real engine health while `_cat/health` printed
//!    a hardcoded `green` — so the ES-compatible endpoint, the one existing
//!    dashboards point at, was the one that could not report a broken node.
//!    `_cluster/health` did not count an unopenable primary either.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use xerj_api::{
    router::{build_es_compat_router, build_native_router},
    state::AppState,
};
use xerj_common::types::Schema;
use xerj_common::{config::Config, metrics::Metrics};
use xerj_engine::Engine;

/// A node whose data dir holds one healthy index and one whose manifest is
/// corrupt, so the boot that produces `state` quarantines exactly one index.
async fn node_with_one_broken_index() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    {
        let engine = Engine::new(config.clone()).expect("first boot");
        for name in ["broken", "healthy"] {
            engine.create_index(name, Schema::empty()).unwrap();
            let idx = engine.get_index(name).unwrap();
            idx.index_document(Some("1".into()), serde_json::json!({"t": "hello"}))
                .await
                .unwrap();
            engine.flush_index(name).await.unwrap();
        }
    }
    std::fs::write(
        dir.path().join("broken").join("snapshot.json"),
        b"{not json",
    )
    .unwrap();

    let metrics = Metrics::new().expect("metrics");
    let engine = Engine::new(config.clone()).expect("boot with one broken index");
    assert_eq!(
        engine.list_failed_indices().len(),
        1,
        "fixture must produce exactly one failed index"
    );
    (AppState::new(config, engine, metrics), dir)
}

async fn send(app: &axum::Router, method: &str, uri: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::empty())
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn send_json(app: &axum::Router, method: &str, uri: &str) -> (StatusCode, Value) {
    let (status, text) = send(app, method, uri).await;
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cat_indices_lists_the_failed_index_as_red() {
    let (state, _dir) = node_with_one_broken_index().await;
    let app = build_es_compat_router(state);

    let (status, body) = send(&app, "GET", "/_cat/indices").await;
    assert_eq!(status, StatusCode::OK);
    let broken: Vec<&str> = body.lines().filter(|l| l.contains("broken")).collect();
    assert_eq!(
        broken.len(),
        1,
        "failed index missing from _cat/indices: {body:?}"
    );
    assert!(
        broken[0].starts_with("red open broken "),
        "a failed index must be listed red: {:?}",
        broken[0]
    );
    assert!(
        body.lines().any(|l| l.starts_with("green open healthy ")),
        "the healthy index must be untouched: {body:?}"
    );

    // A concrete-name request for it resolves instead of 404ing.
    let (status, body) = send(&app, "GET", "/_cat/indices/broken").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.starts_with("red open broken "), "{body:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_four_health_surfaces_agree_that_the_node_is_red() {
    let (state, _dir) = node_with_one_broken_index().await;
    let es = build_es_compat_router(state.clone());
    let native = build_native_router(state);

    // `_cat/health` used to print a hardcoded `green` here.
    let (status, body) = send(&es, "GET", "/_cat/health").await;
    assert_eq!(status, StatusCode::OK);
    let cols: Vec<&str> = body.split_whitespace().collect();
    assert_eq!(cols[3], "red", "_cat/health status column: {body:?}");
    assert_eq!(cols[10], "1", "_cat/health unassign column: {body:?}");

    // `_cluster/health` counted unassigned replicas only, never an
    // unopenable primary.
    let (status, health) = send_json(&es, "GET", "/_cluster/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["status"], "red", "{health}");
    assert_eq!(health["unassigned_primary_shards"], 1, "{health}");
    assert_eq!(health["unassigned_shards"], 1, "{health}");

    // `?level=indices` answers "which index, and why" in one call.
    let (_, detail) = send_json(&es, "GET", "/_cluster/health?level=indices").await;
    let broken = &detail["indices"]["broken"];
    assert_eq!(broken["status"], "red", "{detail}");
    assert!(
        broken["unassigned_info"]["details"]
            .as_str()
            .unwrap_or_default()
            .contains("snapshot.json"),
        "the open error must reach the operator: {detail}"
    );

    // The two native surfaces already agreed; they must keep agreeing.
    let (_, native_health) = send_json(&native, "GET", "/v1/health").await;
    assert_eq!(native_health["data"]["status"], "red", "{native_health}");
    let (_, native_cluster) = send_json(&native, "GET", "/v1/cluster/health").await;
    assert_eq!(native_cluster["data"]["status"], "red", "{native_cluster}");

    // `wait_for_active_shards=all` must not report success on a cluster whose
    // primary cannot be opened — "all shards active" is exactly what is false.
    let (status, waited) =
        send_json(&es, "GET", "/_cluster/health?wait_for_active_shards=all").await;
    assert_eq!(status, StatusCode::REQUEST_TIMEOUT, "{waited}");
    assert_eq!(waited["timed_out"], true, "{waited}");
}

/// `wait_for_status` is the standard bootstrap gate — the docker healthcheck,
/// the CI wait loop and Kibana's startup all issue
/// `GET /_cluster/health?wait_for_status=green&timeout=30s` and read a 200 with
/// `timed_out: false` as "the status I asked for was reached". It used to be
/// accepted and never consulted, which was invisible while `red` was
/// unreachable on this endpoint and actively wrong the moment an unopenable
/// primary made it reachable: a red node sailed through every one of those
/// gates.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_for_status_does_not_report_success_on_a_red_node() {
    let (state, _dir) = node_with_one_broken_index().await;
    let es = build_es_compat_router(state);

    for requested in ["green", "yellow", "GREEN"] {
        let uri = format!("/_cluster/health?wait_for_status={requested}&timeout=30s");
        let (status, body) = send_json(&es, "GET", &uri).await;
        assert_eq!(
            status,
            StatusCode::REQUEST_TIMEOUT,
            "wait_for_status={requested} on a red node: {body}"
        );
        assert_eq!(body["timed_out"], true, "{body}");
        assert_eq!(body["status"], "red", "{body}");
    }

    // `wait_for_status=red` IS satisfied by a red cluster — ES's rule is
    // "observed at least as good as requested", not "observed equals green".
    let (status, body) = send_json(&es, "GET", "/_cluster/health?wait_for_status=red").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["timed_out"], false, "{body}");

    // …and a healthy node is untouched: no wait_for_status request on a green
    // cluster may start timing out because of this.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(config.clone()).expect("clean boot");
    engine.create_index("fine", Schema::empty()).unwrap();
    let green = build_es_compat_router(AppState::new(
        config,
        engine,
        Metrics::new().expect("metrics"),
    ));
    for requested in ["green", "yellow", "red"] {
        let uri = format!("/_cluster/health?wait_for_status={requested}&timeout=30s");
        let (status, body) = send_json(&green, "GET", &uri).await;
        assert_eq!(status, StatusCode::OK, "{requested}: {body}");
        assert_eq!(body["timed_out"], false, "{requested}: {body}");
        assert_eq!(body["status"], "green", "{requested}: {body}");
    }
}

/// `GET /{index}` and `HEAD /{index}` are the two canonical existence probes —
/// `indices.exists()` in every ES client is the HEAD. Both answered
/// `404 index_not_found_exception "no such index [broken]"` for a failed index,
/// which contradicted `_cat/indices` (a red row for the same name) and
/// `_settings` (200) in the same run, and left a client that did HEAD (404)
/// then PUT (503) with two incompatible answers about one name.
///
/// ES resolves both out of cluster metadata and never touches a shard
/// (`TransportGetIndexAction.localClusterStateOperation` →
/// `concreteIndexNames(state.metadata(), request)`), so a red index answers
/// 200 with its metadata there. It does here now too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_index_exists_on_the_existence_probes() {
    let (state, _dir) = node_with_one_broken_index().await;
    let app = build_es_compat_router(state);

    let (status, _) = send(&app, "HEAD", "/broken").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "HEAD /{{index}} is indices.exists(); the name IS taken"
    );

    let (status, body) = send_json(&app, "GET", "/broken").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["broken"]["settings"]["index"]["uuid"].is_string(),
        "the metadata read must return the index, not an empty object: {body}"
    );

    // A wildcard/_all metadata read enumerates it for the same reason.
    let (status, body) = send_json(&app, "GET", "/_all").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["broken"].is_object(), "{body}");
    assert!(body["healthy"].is_object(), "{body}");

    // A name that never existed is still an honest 404 on both.
    let (status, _) = send(&app, "HEAD", "/never-existed").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = send_json(&app, "GET", "/never-existed").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // The *data* doors still refuse: existing-but-unservable is 503, which is
    // the distinction the 404 destroyed.
    let (status, body) = send_json(&app, "GET", "/broken/_search").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(
        body["error"]["type"], "no_shard_available_action_exception",
        "{body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cluster_state_carries_the_failed_index_as_an_unassigned_primary() {
    let (state, _dir) = node_with_one_broken_index().await;
    let app = build_es_compat_router(state);

    let (status, cs) = send_json(&app, "GET", "/_cluster/state").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        cs["metadata"]["indices"]["broken"].is_object(),
        "a failed index still exists and belongs in cluster state: {cs}"
    );
    let shard = &cs["routing_table"]["indices"]["broken"]["shards"]["0"][0];
    assert_eq!(shard["state"], "UNASSIGNED", "{cs}");
    assert_eq!(shard["primary"], true, "{cs}");
    assert_eq!(
        shard["unassigned_info"]["reason"], "ALLOCATION_FAILED",
        "{cs}"
    );
    assert!(
        shard["unassigned_info"]["details"]
            .as_str()
            .unwrap_or_default()
            .contains("could not be parsed"),
        "{cs}"
    );
    // ES's own `unassigned_info` field names, so an operator's existing
    // dashboard renders this block without knowing anything about XERJ.
    assert_eq!(
        shard["unassigned_info"]["allocation_status"], "no_valid_shard_copy",
        "{cs}"
    );
    assert!(
        shard["unassigned_info"]["at"]
            .as_str()
            .unwrap_or_default()
            .ends_with('Z'),
        "`at` must be an ISO-8601 instant like ES: {cs}"
    );
    assert_eq!(
        cs["routing_nodes"]["unassigned"].as_array().map(Vec::len),
        Some(1),
        "{cs}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readiness_keeps_a_partly_degraded_node_in_service() {
    let (state, _dir) = node_with_one_broken_index().await;
    let app = build_es_compat_router(state);

    // One broken index out of two: the node can still serve `healthy`, so
    // pulling it out of rotation forever would be the outage, not the fix.
    let (status, body) = send(&app, "GET", "/health/ready").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("degraded"),
        "the degradation must be named: {body:?}"
    );
    assert!(body.contains("1 of 2 indices serving"), "{body:?}");
    // Counts only — this path is auth-exempt, so it must not enumerate index
    // names to an unauthenticated prober.
    assert!(
        !body.contains("broken"),
        "readiness leaked an index name: {body:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readiness_still_fails_when_nothing_can_be_served() {
    // Every index in the data dir is broken — there is genuinely nothing to
    // send traffic to, so 503 is correct and must survive the fix above.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    {
        let engine = Engine::new(config.clone()).expect("first boot");
        engine.create_index("only", Schema::empty()).unwrap();
        let idx = engine.get_index("only").unwrap();
        idx.index_document(Some("1".into()), serde_json::json!({"t": "x"}))
            .await
            .unwrap();
        engine.flush_index("only").await.unwrap();
    }
    std::fs::write(dir.path().join("only").join("snapshot.json"), b"{not json").unwrap();
    let engine = Engine::new(config.clone()).expect("reboot");
    let state = AppState::new(config, engine, Metrics::new().expect("metrics"));
    let app = build_es_compat_router(state);

    let (status, body) = send(&app, "GET", "/health/ready").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body.contains("no index could be opened"), "{body:?}");
    assert!(
        !body.contains("only"),
        "readiness leaked an index name: {body:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_failed_index_can_be_inspected_retried_and_deleted_over_http() {
    let (state, dir) = node_with_one_broken_index().await;
    let app = build_es_compat_router(state);

    // Inspect: name AND reason, plus the two commands that act on it.
    let (status, listing) = send_json(&app, "GET", "/_cluster/indices/failed").await;
    assert_eq!(status, StatusCode::OK);
    let items = listing["data"]["failed_indices"].as_array().expect("array");
    assert_eq!(items.len(), 1, "{listing}");
    assert_eq!(items[0]["index"], "broken");
    assert!(
        items[0]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("snapshot.json"),
        "{listing}"
    );

    // Reading it names the real problem instead of claiming it does not exist.
    let (status, err) = send_json(&app, "GET", "/broken/_search").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{err}");
    assert_eq!(
        err["error"]["type"], "no_shard_available_action_exception",
        "{err}"
    );

    // Retry while still broken: 503 with the live reason, not a cheerful 200.
    let (status, err) = send_json(&app, "POST", "/_cluster/indices/failed/broken/_retry").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{err}");
    assert!(
        err["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("snapshot.json"),
        "{err}"
    );

    // Delete without a restart.
    let (status, body) = send_json(&app, "DELETE", "/broken").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["acknowledged"], true, "{body}");
    assert!(
        !dir.path().join("broken").exists(),
        "DELETE must remove the bytes on disk"
    );

    // The node is green again and nothing is left to inspect.
    let (_, listing) = send_json(&app, "GET", "/_cluster/indices/failed").await;
    assert_eq!(listing["data"]["count"], 0, "{listing}");
    let (_, health) = send_json(&app, "GET", "/_cluster/health").await;
    assert_eq!(health["status"], "green", "{health}");
    let (status, body) = send(&app, "GET", "/_cat/indices").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("broken"), "{body:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_over_http_reopens_the_index_once_the_cause_is_fixed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    {
        let engine = Engine::new(config.clone()).expect("first boot");
        engine.create_index("broken", Schema::empty()).unwrap();
        let idx = engine.get_index("broken").unwrap();
        idx.index_document(Some("1".into()), serde_json::json!({"t": "hello"}))
            .await
            .unwrap();
        engine.flush_index("broken").await.unwrap();
    }
    let manifest = dir.path().join("broken").join("snapshot.json");
    let good = std::fs::read(&manifest).unwrap();
    std::fs::write(&manifest, b"{not json").unwrap();

    let engine = Engine::new(config.clone()).expect("reboot");
    let state = AppState::new(config, engine, Metrics::new().expect("metrics"));
    let app = build_es_compat_router(state);

    // Operator restores the manifest the storage error pointed them at, then
    // retries — no restart.
    std::fs::write(&manifest, &good).unwrap();
    let (status, body) = send_json(&app, "POST", "/_cluster/indices/failed/broken/_retry").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["reopened"], true, "{body}");

    let (_, health) = send_json(&app, "GET", "/_cluster/health").await;
    assert_eq!(health["status"], "green", "{health}");
    let (status, count) = send_json(&app, "GET", "/broken/_count").await;
    assert_eq!(status, StatusCode::OK, "{count}");
    assert_eq!(count["count"], 1, "the reopened index must serve its data");

    // Retrying a name that is not a failed index is a 404, not a silent ok.
    let (status, err) = send_json(&app, "POST", "/_cluster/indices/failed/broken/_retry").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{err}");
}

// ─────────────────────────────────────────────────────────────────────────────
// The boundary half: making a failed index visible must not make it visible to
// everyone. A brain a scoped key may not read stays unreadable when it breaks.
// ─────────────────────────────────────────────────────────────────────────────

const ADMIN_KEY: &str = "admin-secret-key-for-failed-index-test";

async fn send_auth(
    app: &axum::Router,
    method: &str,
    uri: &str,
    auth: &str,
) -> (StatusCode, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", auth)
        .body(Body::empty())
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_brain_is_not_leaked_to_a_scoped_key() {
    // A brain and an ordinary index; the brain's manifest is corrupted so it
    // is a FAILED index on the boot that serves the requests below.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.auth.enabled = true;
    config.auth.admin_api_key = ADMIN_KEY.to_string();
    {
        let engine = Engine::new(config.clone()).expect("first boot");
        for name in [".xerj-memory-bob-edges", "logs-2026"] {
            engine.create_index(name, Schema::empty()).unwrap();
            let idx = engine.get_index(name).unwrap();
            idx.index_document(Some("1".into()), serde_json::json!({"t": "x"}))
                .await
                .unwrap();
            engine.flush_index(name).await.unwrap();
        }
    }
    std::fs::write(
        dir.path()
            .join(".xerj-memory-bob-edges")
            .join("snapshot.json"),
        b"{not json",
    )
    .unwrap();
    let engine = Engine::new(config.clone()).expect("reboot");
    let state = AppState::new(config, engine, Metrics::new().expect("metrics"));
    let app = build_es_compat_router(state);

    let admin = format!("ApiKey {ADMIN_KEY}");
    let mint = Request::builder()
        .method("POST")
        .uri("/_security/api_key")
        .header("content-type", "application/json")
        .header("authorization", &admin)
        .body(Body::from(
            r#"{"name":"logs-agent","role_descriptors":{"logs":{"indices":[
                 {"names":["logs-2026"],"privileges":["read","write"]}]}}}"#,
        ))
        .expect("request");
    let resp = app.clone().oneshot(mint).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let minted: Value = serde_json::from_slice(&bytes).expect("json");
    let agent = format!("ApiKey {}", minted["encoded"].as_str().expect("encoded"));

    for uri in [
        "/_cat/indices",
        "/_cat/shards",
        "/_cluster/state",
        "/_cluster/health?level=indices",
        "/_cluster/indices/failed",
    ] {
        let (status, body) = send_auth(&app, "GET", uri, &agent).await;
        assert!(status.is_success(), "{uri}: {status} {body}");
        assert!(
            !body.contains("bob"),
            "{uri} leaked a failed brain to a scoped key: {body}"
        );
    }

    // The admin still sees it — "prune everything" would pass the test above.
    let (status, body) = send_auth(&app, "GET", "/_cluster/indices/failed", &admin).await;
    assert!(status.is_success(), "{body}");
    assert!(body.contains("bob"), "the admin must still see it: {body}");
}
