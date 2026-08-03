//! Snapshot and restore are the two verbs that reach index data without going
//! through the engine's visibility funnel — and they were the two verbs a
//! non-superuser could point at every tenant's brain.
//!
//! `Engine::create_snapshot` walks the index map itself; `Engine::restore_snapshot`
//! expands the request's pattern against the snapshot's own manifest and then
//! `remove_dir_all`s and rewrites the index directory directly. Neither calls
//! `get_index` or `delete_index`, so `xerj_engine::index_guard` — the backstop
//! that catches everything `authz` did not know to parse — was never consulted
//! on either path. Two holes followed:
//!
//! * `POST /_snapshot/{repo}/{snap}/_restore {"indices":".xerj-memory-*"}` —
//!   authorization waved the wildcard through (a pattern is normally expanded
//!   over the caller's visible set, which is exactly what does *not* happen
//!   here), so any authenticated caller rolled every brain on the node back to
//!   the backup instant, destroying every write made since.
//! * `PUT /_snapshot/{repo}/{snap} {"indices":"*"}` — the create handler read
//!   `indices` only in its ARRAY form, so the string spelling ES also accepts
//!   resolved to `None` and the engine fell back to "every index on the node".
//!   The caller's authorized target list was simply not the list that got
//!   copied.
//!
//! Both are measured end-to-end here, against a real snapshot on disk, with a
//! minted non-admin key.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use xerj_api::{router::build_es_compat_router, state::AppState};
use xerj_common::{config::Config, metrics::Metrics};
use xerj_engine::Engine;

const ADMIN_KEY: &str = "admin-secret-key-for-snapshot-test";

/// Pinned fixture instants — a link and the read that follows it must never
/// land in the same millisecond.
const T0: i64 = 1_753_600_000_000;
const T1: i64 = 1_753_600_100_000;

fn auth_enabled_state() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().expect("utf8 path").to_string();
    config.auth.enabled = true;
    config.auth.admin_api_key = ADMIN_KEY.to_string();
    let metrics = Metrics::new().expect("metrics");
    let engine = Engine::new(config.clone()).expect("engine");
    (AppState::new(config, engine, metrics), dir)
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    auth: &str,
    body: &str,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", auth)
        .body(Body::from(body.to_string()))
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn mint(app: &axum::Router, minter: &str, body: &str) -> String {
    let (status, resp) = send(app, "POST", "/_security/api_key", minter, body).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "minting a key should succeed: {resp}"
    );
    format!("ApiKey {}", resp["encoded"].as_str().expect("encoded key"))
}

/// The index directories a snapshot actually captured — ground truth, read off
/// the filesystem rather than from the manifest the snapshot wrote about itself.
fn captured(repo_dir: &std::path::Path, snapshot: &str) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(repo_dir.join(snapshot))
        .expect("snapshot dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Two brains and one ordinary index, seeded as the admin, plus a snapshot
/// repository inside the data dir (the only location the engine accepts
/// without an explicit allowlist).
async fn seed(app: &axum::Router, data_dir: &std::path::Path) -> String {
    let admin = format!("ApiKey {ADMIN_KEY}");
    for brain in ["alice", "bob"] {
        let (status, body) = send(
            app,
            "POST",
            &format!("/_graph/{brain}/link"),
            &admin,
            &format!(
                r#"{{"src":"doc:1","type":"mentions","dst":"{brain}:secret",
                     "valid_at":{T0},"created_at":{T0}}}"#
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "seeding {brain}: {body}");
    }
    let (status, body) = send(
        app,
        "POST",
        "/logs-2026/_doc/1",
        &admin,
        r#"{"message":"tenant data"}"#,
    )
    .await;
    assert!(status.is_success(), "seeding logs-2026: {status} {body}");

    let repo_dir = data_dir.join("snaprepo");
    let (status, body) = send(
        app,
        "PUT",
        "/_snapshot/snaprepo",
        &admin,
        &format!(
            r#"{{"type":"fs","settings":{{"location":"{}"}}}}"#,
            repo_dir.to_str().expect("utf8 path")
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registering the repo: {body}");
    admin
}

/// A tenant key granted the ordinary `logs-*` namespace and nothing else —
/// the credential a routine per-tenant backup job would run under.
const LOGS_TENANT: &str = r#"{"name":"logs-tenant","role_descriptors":{"logs":{"indices":[
    {"names":["logs-*"],"privileges":["all"]}]}}}"#;

// ─────────────────────────────────────────────────────────────────────────────

/// The destructive half. A restore names its targets with a pattern, and that
/// pattern is executed verbatim against the snapshot's manifest — so it must be
/// refused here or it is refused nowhere.
#[tokio::test]
async fn a_tenant_cannot_restore_every_brain_on_the_node() {
    let (state, data_dir) = auth_enabled_state();
    let app = build_es_compat_router(state);
    let admin = seed(&app, data_dir.path()).await;
    let tenant = mint(&app, &admin, LOGS_TENANT).await;

    // The prerequisite an ordinary admin backup produces: a snapshot that
    // contains the brains.
    let (status, body) = send(&app, "PUT", "/_snapshot/snaprepo/full", &admin, "{}").await;
    assert_eq!(status, StatusCode::OK, "admin full backup: {body}");
    assert!(
        captured(&data_dir.path().join("snaprepo"), "full")
            .iter()
            .any(|n| n.starts_with(".xerj-memory-")),
        "the admin's full backup must contain the brains, or this test proves nothing"
    );

    // Bob keeps working after the backup. This edge exists ONLY in live state.
    let (status, body) = send(
        &app,
        "POST",
        "/_graph/bob/link",
        &admin,
        &format!(
            r#"{{"src":"doc:1","type":"mentions","dst":"bob:after-backup",
                 "valid_at":{T1},"created_at":{T1}}}"#
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "post-backup write: {body}");

    // The attack: one request, every brain rolled back to the backup instant.
    for indices in [
        r#""indices":".xerj-memory-*""#,
        r#""indices":[".xerj-memory-*"]"#,
        r#""indices":"*""#,
        r#""indices":"_all""#,
        r#""indices":".xerj-memory-bob-edges""#,
    ] {
        let (status, body) = send(
            &app,
            "POST",
            "/_snapshot/snaprepo/full/_restore?wait_for_completion=true",
            &tenant,
            &format!("{{{indices}}}"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "restore {{{indices}}} must be forbidden: {body}"
        );
    }

    // Bob's post-backup edge is still there — nothing was rolled back.
    let (status, body) = send(
        &app,
        "GET",
        "/_graph/bob/ego?node=doc:1&depth=1",
        &admin,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reading bob's brain back: {body}");
    let serialized = body.to_string();
    assert!(
        serialized.contains("bob:after-backup"),
        "the write made after the backup must survive: {serialized}"
    );
}

/// The enabling half. `indices` in its string spelling was not read at all, so
/// the engine snapshotted every index on the node — including the brains the
/// caller had just been refused by name.
#[tokio::test]
async fn a_tenant_snapshot_captures_only_what_it_named() {
    let (state, data_dir) = auth_enabled_state();
    let app = build_es_compat_router(state);
    let admin = seed(&app, data_dir.path()).await;
    let tenant = mint(&app, &admin, LOGS_TENANT).await;
    let repo_dir = data_dir.path().join("snaprepo");

    // An unbounded snapshot, in either spelling, is not this credential's to
    // take: `*` reaches the reserved namespace.
    for body in ["{}", r#"{"indices":"*"}"#, r#"{"indices":["*"]}"#] {
        let (status, resp) = send(&app, "PUT", "/_snapshot/snaprepo/pwn", &tenant, body).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "snapshot {body} must be forbidden: {resp}"
        );
    }
    assert!(
        !repo_dir.join("pwn").exists(),
        "a refused snapshot must not have written anything"
    );

    // The legitimate use keeps working — and captures the tenant's own index
    // and nothing else. This is the assertion the string form failed: it used
    // to copy every brain on the node into a repo the tenant controls.
    let (status, resp) = send(
        &app,
        "PUT",
        "/_snapshot/snaprepo/mine",
        &tenant,
        r#"{"indices":"logs-*"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "tenant snapshot of logs-*: {resp}");
    assert_eq!(
        captured(&repo_dir, "mine"),
        vec!["logs-2026".to_string()],
        "a `logs-*` snapshot captured indices it did not name"
    );

    // The array spelling of the same request agrees with the string one.
    let (status, resp) = send(
        &app,
        "PUT",
        "/_snapshot/snaprepo/mine-array",
        &tenant,
        r#"{"indices":["logs-*"]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "array spelling: {resp}");
    assert_eq!(
        captured(&repo_dir, "mine-array"),
        vec!["logs-2026".to_string()]
    );

    // And the tenant can roll its own index back, which is the whole point of
    // being allowed to take the snapshot.
    let (status, resp) = send(
        &app,
        "POST",
        "/_snapshot/snaprepo/mine/_restore?wait_for_completion=true",
        &tenant,
        r#"{"indices":"logs-*"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "tenant restore of its own: {resp}");
    assert_eq!(
        resp["snapshot"]["indices"],
        serde_json::json!(["logs-2026"]),
        "restore reported something other than the tenant's own index: {resp}"
    );
}

/// The operator path must be untouched: a superuser backs up and restores the
/// whole node, brains included.
#[tokio::test]
async fn the_superuser_still_backs_up_and_restores_everything() {
    let (state, data_dir) = auth_enabled_state();
    let app = build_es_compat_router(state);
    let admin = seed(&app, data_dir.path()).await;
    let repo_dir = data_dir.path().join("snaprepo");

    for (name, body) in [
        ("full", "{}"),
        ("star", r#"{"indices":"*"}"#),
        ("star-array", r#"{"indices":["*"]}"#),
        ("brains", r#"{"indices":".xerj-memory-*"}"#),
    ] {
        let (status, resp) = send(
            &app,
            "PUT",
            &format!("/_snapshot/snaprepo/{name}"),
            &admin,
            body,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "superuser snapshot {body}: {resp}");
        let names = captured(&repo_dir, name);
        assert!(
            names.iter().any(|n| n.starts_with(".xerj-memory-")),
            "superuser snapshot {body} captured no brain: {names:?}"
        );
        if body == "{}" || body.contains("\"*\"") {
            assert!(
                names.iter().any(|n| n == "logs-2026"),
                "an unbounded superuser snapshot must still cover ordinary indices: {names:?}"
            );
        }
    }

    let (status, resp) = send(
        &app,
        "POST",
        "/_snapshot/snaprepo/full/_restore?wait_for_completion=true",
        &admin,
        r#"{"indices":"*"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "superuser full restore: {resp}");
    let restored = resp["snapshot"]["indices"]
        .as_array()
        .expect("restored list")
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>();
    assert!(
        restored.iter().any(|n| n.starts_with(".xerj-memory-")),
        "the operator must still be able to restore a brain: {restored:?}"
    );
}
