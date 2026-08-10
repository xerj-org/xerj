//! Issue #202, from the outside — a torn `schema.json` has to reach the
//! recovery surfaces, not just the engine.
//!
//! Refusing to open an index whose metadata is unparseable is only useful if
//! the ES-compat API (`:9200`, the port every ES client, Kibana and monitoring
//! dashboard talks to) then tells the truth about it and offers a way out. The
//! engine-level tests in `xerj-engine/tests/corrupt_index_metadata.rs` call
//! `engine.delete_index` / `engine.health()` directly and therefore cannot see
//! the HTTP layer at all.
//!
//! **The HTTP behaviour asserted here is not this change's work.** Issue #206
//! built it — failed indices in `_cat/indices` and `_cluster/state`, a red
//! `_cluster/health` that names them, `DELETE` reaching a failed index through
//! literal, `_all` and wildcard forms, `GET /_cluster/indices/failed`, the
//! retry endpoint, and a readiness probe scoped to "this node has nothing to
//! serve" rather than "cluster is red". What #202 adds is a *new way in*: an
//! unparseable sidecar is now one of the conditions that puts an index in that
//! failed set. This file pins the join between the two, so a change to either
//! side that stops a corrupt-metadata index from being visible, deletable and
//! survivable on `:9200` fails here rather than in production.
//!
//! Nine of the eleven cases below quarantine an index by truncating its
//! `schema.json` — the #202 trigger — and then assert the #206 contract over
//! `build_es_compat_router`. The other two are controls that pin the negative
//! half of that contract, and deliberately corrupt nothing:
//! `cluster_health_is_not_red_without_a_failed_index` (an intact node must not
//! be dragged red by the code that reports failures) and `an_empty_node_is_ready`
//! (a fresh pod with no indices at all must still join the Service).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;
use xerj_common::types::{FieldConfig, FieldType, Schema};

fn config_for(dir: &tempfile::TempDir) -> xerj_common::config::Config {
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config
}

fn mapped_schema() -> Schema {
    let mut schema = Schema::empty();
    schema
        .add_field(FieldConfig::new("code", FieldType::Keyword))
        .unwrap();
    schema
}

/// Boot a node over `dir`, create `names` with an explicit mapping and one
/// flushed document each, then drop the engine so every file is closed.
async fn seed_indices(dir: &tempfile::TempDir, names: &[&str]) {
    let engine = xerj_engine::Engine::new(config_for(dir)).expect("engine");
    for name in names {
        engine
            .create_index(name, mapped_schema())
            .expect("create_index");
        let idx = engine.get_index(name).expect("get_index");
        idx.index_document(Some("d1".into()), json!({ "code": "A-1" }))
            .await
            .expect("index_document");
        idx.flush().await.expect("flush");
    }
    drop(engine);
}

/// Truncate `schema.json` to half its bytes — the torn write this PR refuses.
fn tear_schema(dir: &tempfile::TempDir, name: &str) {
    let path = dir.path().join(name).join("schema.json");
    let good = std::fs::read(&path).expect("read schema.json");
    std::fs::write(&path, &good[..good.len() / 2]).expect("write torn schema.json");
}

/// A node booted over a data dir that already contains a corrupt index.
async fn app_over(dir: &tempfile::TempDir) -> axum::Router {
    let config = config_for(dir);
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    xerj_api::router::build_es_compat_router(state)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(req).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn get(app: &axum::Router, path: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::get(path).body(Body::empty()).expect("request"),
    )
    .await
}

async fn delete(app: &axum::Router, path: &str) -> (StatusCode, Value) {
    send(
        app,
        Request::delete(path).body(Body::empty()).expect("request"),
    )
    .await
}

// ── DELETE /{index} is the documented escape hatch ───────────────────────────

/// The recovery story for a corrupt sidecar ("restore the file and retry,
/// restore from a snapshot, or DELETE the index") has to hold on the ES-compat
/// port for an index quarantined by #202, not only for the #206 triggers the
/// delete path was built against.
#[tokio::test]
async fn delete_removes_a_failed_index_over_the_es_api() {
    let dir = tempfile::tempdir().unwrap();
    seed_indices(&dir, &["victim"]).await;
    tear_schema(&dir, "victim");

    let app = app_over(&dir).await;
    let (status, body) = delete(&app, "/victim").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "DELETE on a failed index must succeed on :9200, got {status} {body}"
    );
    assert_eq!(body["acknowledged"], json!(true));
    assert!(
        !dir.path().join("victim").exists(),
        "the acknowledgement must mean the directory is gone"
    );

    // The name is usable again — that is the whole point of the escape hatch.
    let (status, body) = send(
        &app,
        Request::put("/victim")
            .header("content-type", "application/json")
            .body(Body::from(json!({}).to_string()))
            .unwrap(),
    )
    .await;
    assert!(
        status.is_success(),
        "the name must be re-creatable after the delete, got {status} {body}"
    );
}

/// Deleting the failed index must clear the failure, not just the directory:
/// health has to come back.
#[tokio::test]
async fn health_recovers_after_the_failed_index_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    seed_indices(&dir, &["victim", "healthy"]).await;
    tear_schema(&dir, "victim");

    let app = app_over(&dir).await;
    assert_eq!(
        get(&app, "/_cluster/health").await.1["status"],
        json!("red")
    );
    assert_eq!(delete(&app, "/victim").await.0, StatusCode::OK);

    let (status, body) = get(&app, "/_cluster/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(
        body["status"],
        json!("red"),
        "health must leave red once the corrupt index is gone, got {body}"
    );
    // The healthy index is untouched by the recovery.
    assert!(dir.path().join("healthy").exists());
}

// ── DELETE /_all must not acknowledge a no-op ────────────────────────────────

/// A wildcard expansion that skipped the failed index would answer
/// `200 {"acknowledged": true}` with the corrupt directory still on disk — the
/// accepted-and-ignored class tracked in #204. Pinned for a #202 failure.
#[tokio::test]
async fn delete_all_actually_removes_a_failed_index() {
    let dir = tempfile::tempdir().unwrap();
    seed_indices(&dir, &["victim", "healthy"]).await;
    tear_schema(&dir, "victim");

    let app = app_over(&dir).await;
    let (status, body) = delete(&app, "/_all").await;
    assert_eq!(status, StatusCode::OK, "got {status} {body}");
    assert!(
        !dir.path().join("victim").exists(),
        "DELETE /_all acknowledged while the failed index survived on disk: {body}"
    );
    assert!(
        !dir.path().join("healthy").exists(),
        "DELETE /_all must still delete the healthy indices too"
    );
}

/// A wildcard that matches the failed index resolves it as well.
#[tokio::test]
async fn delete_wildcard_resolves_a_failed_index() {
    let dir = tempfile::tempdir().unwrap();
    seed_indices(&dir, &["vic-1", "healthy"]).await;
    tear_schema(&dir, "vic-1");

    let app = app_over(&dir).await;
    let (status, body) = delete(&app, "/vic-*").await;
    assert_eq!(status, StatusCode::OK, "got {status} {body}");
    assert!(
        !dir.path().join("vic-1").exists(),
        "a wildcard delete must reach the failed index: {body}"
    );
    assert!(
        dir.path().join("healthy").exists(),
        "and must not touch anything it did not match"
    );
}

// ── GET /_cluster/health is the surface monitoring polls ─────────────────────

/// #202's user-visible promise is "the operator sees red". `engine.health()`
/// answering red is not enough: the claim only holds if `GET /_cluster/health`
/// — what every ES client and dashboard actually polls — reports it too, for an
/// index that failed on a torn sidecar.
#[tokio::test]
async fn cluster_health_reports_red_for_a_failed_index() {
    let dir = tempfile::tempdir().unwrap();
    seed_indices(&dir, &["victim", "healthy"]).await;
    tear_schema(&dir, "victim");

    let app = app_over(&dir).await;
    let (status, body) = get(&app, "/_cluster/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["status"],
        json!("red"),
        "GET /_cluster/health must agree with engine.health(), got {body}"
    );
    // A red status with 100% active shards and no unassigned primary is an
    // internally inconsistent answer that a dashboard cannot act on.
    assert_eq!(
        body["unassigned_primary_shards"],
        json!(1),
        "the failed index's primary must count as unassigned, got {body}"
    );
    assert!(
        body["active_shards_percent_as_number"].as_f64().unwrap() < 100.0,
        "active_shards_percent must reflect the unserved shard, got {body}"
    );

    // `level=indices` has to name the index the operator must repair.
    let (_, body) = get(&app, "/_cluster/health?level=indices").await;
    assert_eq!(
        body["indices"]["victim"]["status"],
        json!("red"),
        "level=indices must name the failed index, got {body}"
    );
    assert_eq!(body["indices"]["healthy"]["status"], json!("green"));
}

/// An intact node is unaffected — the red must come from the failure, not from
/// the code that reports it.
#[tokio::test]
async fn cluster_health_is_not_red_without_a_failed_index() {
    let dir = tempfile::tempdir().unwrap();
    seed_indices(&dir, &["healthy"]).await;

    let app = app_over(&dir).await;
    let (status, body) = get(&app, "/_cluster/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(
        body["status"],
        json!("red"),
        "a node with no failed index must not report red, got {body}"
    );
}

/// A per-index health call for a *healthy* index must not inherit another
/// index's failure — red is scoped to the selector.
#[tokio::test]
async fn per_index_health_is_scoped_to_the_selector() {
    let dir = tempfile::tempdir().unwrap();
    seed_indices(&dir, &["victim", "healthy"]).await;
    tear_schema(&dir, "victim");

    let app = app_over(&dir).await;
    let (_, body) = get(&app, "/_cluster/health/healthy").await;
    assert_ne!(
        body["status"],
        json!("red"),
        "GET /_cluster/health/healthy must report on `healthy` alone, got {body}"
    );
    let (_, body) = get(&app, "/_cluster/health/victim").await;
    assert_eq!(
        body["status"],
        json!("red"),
        "GET /_cluster/health/victim must be red, got {body}"
    );
}

// ── readiness is a traffic gate, not an alarm channel ───────────────────────

/// `native::readiness` is mounted on BOTH routers (`router.rs:113` and
/// `router.rs:225`) and was rescoped by #206 away from `health() == "red"`.
/// #202 is what makes that rescope load-bearing: red now also means "an index
/// has an unparseable sidecar", which is permanent, so under the old predicate
/// one torn `schema.json` out of N indices would have removed the pod from
/// kubelet rotation for good while the other N-1 answered perfectly — and would
/// have bricked `xerj brain`, which gates on this probe. A node that is still
/// serving must stay in rotation; the alarm belongs on `_cluster/health`,
/// asserted here alongside it.
#[tokio::test]
async fn a_partially_failed_node_stays_in_rotation_and_keeps_serving() {
    let dir = tempfile::tempdir().unwrap();
    seed_indices(&dir, &["victim", "healthy"]).await;
    tear_schema(&dir, "victim");

    let app = app_over(&dir).await;

    let (status, body) = get(&app, "/health/ready").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "one failed index must not take a still-serving node out of rotation, got {status} {body}"
    );
    let (status, _) = get(&app, "/health/live").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "liveness must stay 200 — a failed index must not restart the pod"
    );
    // The alarm is on cluster health, not on the traffic gate.
    assert_eq!(
        get(&app, "/_cluster/health").await.1["status"],
        json!("red")
    );

    // …and the healthy index really is serving while that is true.
    let (status, body) = send(
        &app,
        Request::post("/healthy/_search")
            .header("content-type", "application/json")
            .body(Body::from(json!({"query": {"match_all": {}}}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the healthy index must keep serving, got {status} {body}"
    );
}

/// The other half of the chosen semantics: when nothing on the node opened,
/// there is nothing to send traffic to and the probe must say so.
#[tokio::test]
async fn a_node_whose_every_index_failed_reports_not_ready() {
    let dir = tempfile::tempdir().unwrap();
    seed_indices(&dir, &["victim"]).await;
    tear_schema(&dir, "victim");

    let app = app_over(&dir).await;
    let (status, _) = get(&app, "/health/ready").await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a node with no index serving must be out of rotation"
    );
}

/// And an empty node — no indices at all, none failed — is ready, as it was
/// before this change. (A fresh pod must join the Service.)
#[tokio::test]
async fn an_empty_node_is_ready() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_over(&dir).await;
    assert_eq!(get(&app, "/health/ready").await.0, StatusCode::OK);
}

// ── the create gate, over HTTP ───────────────────────────────────────────────

/// `PUT /{index}` and `_bulk` auto-create used to run `Index::create` over a
/// corrupt directory, overwriting `schema.json` with an empty mapping and
/// destroying the evidence. They are refused — and the refusal must name the
/// file and the way out rather than being an opaque 500.
#[tokio::test]
async fn create_over_a_failed_index_is_refused_over_http() {
    let dir = tempfile::tempdir().unwrap();
    seed_indices(&dir, &["victim"]).await;
    tear_schema(&dir, "victim");

    let app = app_over(&dir).await;
    let (status, body) = send(
        &app,
        Request::put("/victim")
            .header("content-type", "application/json")
            .body(Body::from(json!({}).to_string()))
            .unwrap(),
    )
    .await;
    assert!(
        !status.is_success(),
        "creating over a failed index must be refused, got {status} {body}"
    );
    let text = body.to_string();
    assert!(
        text.contains("failed to open") && text.contains("DELETE"),
        "the refusal must explain the state and the way out, got {status} {text}"
    );
    assert!(
        dir.path().join("victim").join("schema.json").exists(),
        "a refused create must not have rewritten the evidence"
    );
}
