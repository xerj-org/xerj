//! Executable statement of the second-brain authorization posture (issue #79).
//!
//! A brain **is** a security boundary. This file is the inverted descendant of
//! `brain_is_not_a_security_boundary.rs`, which pinned the measured fact that
//! one did *not* exist: any authenticated caller could read, write, forge and
//! destroy every brain, and `GET /_mapping` handed over the list of brain
//! names so the attacker did not even have to guess one. Every assertion that
//! file made now runs in reverse.
//!
//! The point of enumerating so many doors is that the exposure was never about
//! the four `/_graph/*` handlers. A brain's edges live in an ordinary index and
//! `IndexName::validate` admits a leading `.`, so `.xerj-memory-{brain}-edges`
//! was reachable through the generic ES-compat surface *and* through the
//! native router's `/v1/indices/{name}/…` spelling. An access check on
//! `/_graph/*` alone would have left all of that open — a boundary that only
//! looks like one. So this test walks every door, and a fix that closes only
//! some of them fails here.
//!
//! Every request below is made with a **minted, non-admin** API key, because
//! the issue is about what an ordinary authenticated caller can reach — not
//! about the superuser.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use xerj_api::{
    router::{build_es_compat_router, build_native_router},
    state::AppState,
};
use xerj_common::{config::Config, metrics::Metrics};
use xerj_engine::Engine;

const ADMIN_KEY: &str = "admin-secret-key-for-boundary-test";

/// Fixture instants. Pinned rather than "now" so a link and the `as_of` that
/// reads it back can never land in the same millisecond.
const T0: i64 = 1_753_600_000_000;
const AS_OF: i64 = 1_753_700_000_000;

/// An auth-enabled node over a fresh data directory. The `TempDir` is returned
/// and must be held for the whole test: the Engine keeps the directory open,
/// and these are engine data dirs, so leaking them fills `/tmp`.
fn auth_enabled_state() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_over(dir.path().to_str().unwrap(), true);
    (state, dir)
}

fn state_over(data_dir: &str, auth: bool) -> AppState {
    let mut config = Config::default();
    config.server.data_dir = data_dir.to_string();
    config.auth.enabled = auth;
    config.auth.admin_api_key = if auth {
        ADMIN_KEY.to_string()
    } else {
        String::new()
    };
    let metrics = Metrics::new().expect("metrics");
    let engine = Engine::new(config.clone()).expect("engine");
    AppState::new(config, engine, metrics)
}

/// Like [`send`] but returns the body verbatim — `_cat` answers a plain-text
/// table, which is exactly the shape the line-level pruning has to handle.
async fn send_text(
    app: &axum::Router,
    method: &str,
    uri: &str,
    auth: &str,
) -> (StatusCode, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    if !auth.is_empty() {
        builder = builder.header("authorization", auth);
    }
    let req = builder.body(Body::empty()).expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    auth: &str,
    body: &str,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if !auth.is_empty() {
        builder = builder.header("authorization", auth);
    }
    let req = builder.body(Body::from(body.to_string())).expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// Mint an API key with the given JSON body and return its `Authorization`
/// header value.
async fn mint(app: &axum::Router, minter: &str, body: &str) -> String {
    let (status, resp) = send(app, "POST", "/_security/api_key", minter, body).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "minting a key should succeed: {resp}"
    );
    let encoded = resp["encoded"].as_str().expect("encoded key").to_string();
    format!("ApiKey {encoded}")
}

/// A key scoped to exactly one brain: its edges index and its nodes index.
/// This is the documented grant shape from `graph_api`'s module docs.
fn brain_grant(brain: &str, privileges: &str) -> String {
    format!(
        r#"{{"name":"{brain}-agent","role_descriptors":{{"{brain}":{{"indices":[
             {{"names":[".xerj-memory-{brain}-edges",".xerj-memory-{brain}"],
               "privileges":[{privileges}]}}]}}}}}}"#
    )
}

/// Seed two brains, as the admin, so there is something to keep apart.
async fn seed_two_brains(app: &axum::Router) {
    let admin = format!("ApiKey {ADMIN_KEY}");
    for (brain, dst) in [("alice", "doc:2"), ("bob", "secret:2")] {
        let (status, body) = send(
            app,
            "POST",
            &format!("/_graph/{brain}/link"),
            &admin,
            &format!(
                r#"{{"src":"doc:1","type":"mentions","dst":"{dst}",
                     "valid_at":{T0},"created_at":{T0}}}"#
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "seeding {brain}: {body}");
    }
    let (status, _) = send(app, "POST", "/.xerj-memory-bob-edges/_refresh", &admin, "").await;
    assert_eq!(status, StatusCode::OK);
}

// ─────────────────────────────────────────────────────────────────────────────

/// Every door into someone else's brain, walked with a credential scoped to a
/// different brain. This is the assertion the issue asked for, and the exact
/// inverse of what the previous test measured.
#[tokio::test]
async fn a_scoped_caller_cannot_reach_another_brain_through_any_door() {
    let (state, _data_dir) = auth_enabled_state();
    let app = build_es_compat_router(state.clone());
    let native = build_native_router(state);
    seed_two_brains(&app).await;

    let admin = format!("ApiKey {ADMIN_KEY}");
    let alice = mint(&app, &admin, &brain_grant("alice", r#""read","write""#)).await;

    // ── 1. The graph API: all four entry points, denied for bob. ───────────
    for (method, uri) in [
        ("GET", "/_graph/bob/ego?node=doc:1"),
        ("GET", "/_graph/bob/overview"),
        ("POST", "/_graph/bob/link"),
        ("DELETE", "/_graph/bob/link/whatever"),
    ] {
        let (status, body) = send(
            &app,
            method,
            uri,
            &alice,
            r#"{"src":"a","type":"mentions","dst":"b"}"#,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} {uri} must be forbidden, got {status}: {body}"
        );
    }

    // ── 2. The raw index surface, which is where the real exposure lived. ──
    // Read around `/_graph`, forge an edge past the derived-`edge_id`
    // invariant, refresh, and destroy the brain outright.
    for (method, uri, body) in [
        (
            "POST",
            "/.xerj-memory-bob-edges/_search",
            r#"{"query":{"match_all":{}}}"#,
        ),
        (
            "POST",
            "/.xerj-memory-bob-edges/_doc/forged",
            r#"{"edge_id":"forged","src":"doc:1","dst":"attacker:payload","type":"mentions"}"#,
        ),
        ("POST", "/.xerj-memory-bob-edges/_refresh", ""),
        ("GET", "/.xerj-memory-bob-edges/_doc/anything", ""),
        ("DELETE", "/.xerj-memory-bob-edges", ""),
        // Percent-encoded, in case the check compared raw path text.
        ("POST", "/%2Exerj-memory-bob-edges/_search", "{}"),
        // Wildcards are refused rather than expanded-and-filtered.
        ("POST", "/.xerj-memory-*/_search", "{}"),
        ("POST", "/*/_search", "{}"),
        ("POST", "/_all/_search", "{}"),
    ] {
        let (status, resp) = send(&app, method, uri, &alice, body).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} {uri} must be forbidden, got {status}: {resp}"
        );
    }

    // ── 3. The agent-memory surface: bob's brain keeps its NODES in the
    // `.xerj-memory-bob` namespace, so `/_memory/*` is a door to the same data.
    for (method, uri) in [
        ("POST", "/_memory/bob/_recall"),
        ("GET", "/_memory/bob"),
        ("POST", "/_memory/bob"),
        ("DELETE", "/_memory/bob"),
        ("DELETE", "/_memory/bob/some-id"),
    ] {
        let (status, resp) = send(&app, method, uri, &alice, r#"{"text":"x","query":"x"}"#).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} {uri} must be forbidden, got {status}: {resp}"
        );
    }

    // ── 4. Enumeration: a scoped caller cannot list the node's indices at
    // all, so brain names stay unguessable.
    for uri in [
        "/_mapping",
        "/_cat/indices",
        "/_alias",
        "/_settings",
        "/_stats",
        "/_resolve/index/*",
    ] {
        let (status, resp) = send(&app, "GET", uri, &alice, "").await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "GET {uri} must be forbidden, got {status}: {resp}"
        );
    }

    // ── 5. Unnamed fan-out. `POST /_search` with no index reads every index
    // on the node; there is no target to authorize, so it is refused.
    for (method, uri) in [
        ("POST", "/_search"),
        ("POST", "/_bulk"),
        ("POST", "/_msearch"),
        ("POST", "/_mget"),
        ("POST", "/_count"),
    ] {
        let (status, resp) = send(&app, method, uri, &alice, "{}").await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} {uri} must be forbidden, got {status}: {resp}"
        );
    }

    // ── 6. The native router is the same engine under a different spelling.
    for (method, uri) in [
        ("POST", "/v1/indices/.xerj-memory-bob-edges/search"),
        ("GET", "/v1/indices/.xerj-memory-bob-edges"),
        ("DELETE", "/v1/indices/.xerj-memory-bob-edges"),
        ("POST", "/v1/indices/.xerj-memory-bob-edges/docs"),
    ] {
        let (status, resp) = send(&native, method, uri, &alice, r#"{"query":{}}"#).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "native {method} {uri} must be forbidden, got {status}: {resp}"
        );
    }

    // ── 7. Escalation: a scoped caller must not be able to mint itself a key
    // that grants bob.
    let (status, resp) = send(
        &app,
        "POST",
        "/_security/api_key",
        &alice,
        &brain_grant("bob", r#""all""#),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a scoped key must not mint keys, got {status}: {resp}"
    );

    // ── 8. Existence must not leak through the status code: a brain that does
    // not exist is refused exactly like one that does.
    let (status, _) = send(&app, "GET", "/_graph/nosuchbrain/overview", &alice, "").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "403 (not 404) for an unauthorized brain, or the code maps the node"
    );
}

/// The boundary is only worth anything if the authorized side still works.
/// A key scoped to `alice` must be able to do everything a brain owner does.
#[tokio::test]
async fn a_scoped_caller_retains_full_use_of_its_own_brain() {
    let (state, _data_dir) = auth_enabled_state();
    let app = build_es_compat_router(state);
    seed_two_brains(&app).await;

    let admin = format!("ApiKey {ADMIN_KEY}");
    let alice = mint(&app, &admin, &brain_grant("alice", r#""read","write""#)).await;

    // Write.
    let (status, body) = send(
        &app,
        "POST",
        "/_graph/alice/link",
        &alice,
        &format!(
            r#"{{"src":"doc:1","type":"mentions","dst":"doc:3",
                 "valid_at":{T0},"created_at":{T0}}}"#
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "own-brain link failed: {body}");
    let edge_id = body["edge_id"].as_str().expect("edge_id").to_string();

    let (status, _) = send(
        &app,
        "POST",
        "/.xerj-memory-alice-edges/_refresh",
        &alice,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "own backing index is usable");

    // Read, both entry points.
    let (status, ego) = send(
        &app,
        "GET",
        &format!("/_graph/alice/ego?node=doc:1&as_of={AS_OF}"),
        &alice,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "own-brain ego failed: {ego}");
    assert!(
        ego.to_string().contains("doc:3"),
        "own edge should be visible: {ego}"
    );
    let (status, overview) = send(
        &app,
        "GET",
        &format!("/_graph/alice/overview?as_of={AS_OF}"),
        &alice,
        "",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "own-brain overview failed: {overview}"
    );

    // Invalidate.
    let (status, body) = send(
        &app,
        "DELETE",
        &format!("/_graph/alice/link/{edge_id}"),
        &alice,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "own-brain unlink failed: {body}");

    // A read-only grant reads but does not write — the privilege half of the
    // model, not just the resource half.
    let reader = mint(&app, &admin, &brain_grant("alice", r#""read""#)).await;
    let (status, _) = send(&app, "GET", "/_graph/alice/overview", &reader, "").await;
    assert_eq!(status, StatusCode::OK, "read grant must read");
    let (status, _) = send(
        &app,
        "POST",
        "/_graph/alice/link",
        &reader,
        r#"{"src":"a","type":"mentions","dst":"b"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "read grant must not write");
    let (status, _) = send(&app, "DELETE", "/.xerj-memory-alice-edges", &reader, "").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "read grant must not destroy the brain"
    );
}

/// The fail-closed half. A key minted with no `role_descriptors` — the shape
/// every key had before this existed — holds **nothing** in the reserved
/// namespace, and cannot mint itself out of that.
#[tokio::test]
async fn an_unconfigured_key_is_denied_every_brain_and_cannot_escalate() {
    let (state, _data_dir) = auth_enabled_state();
    let app = build_es_compat_router(state);
    seed_two_brains(&app).await;

    let admin = format!("ApiKey {ADMIN_KEY}");
    let legacy = mint(&app, &admin, r#"{"name":"legacy"}"#).await;

    for (method, uri) in [
        ("GET", "/_graph/alice/ego?node=doc:1"),
        ("GET", "/_graph/bob/overview"),
        ("POST", "/_graph/alice/link"),
        ("POST", "/.xerj-memory-alice-edges/_search"),
        ("DELETE", "/.xerj-memory-bob-edges"),
        ("POST", "/_memory/alice/_recall"),
    ] {
        let (status, resp) = send(
            &app,
            method,
            uri,
            &legacy,
            r#"{"src":"a","type":"t","dst":"b","query":"x"}"#,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "unconfigured key on {method} {uri} must be denied, got {status}: {resp}"
        );
    }

    // It keeps its historical reach over ordinary indices — broad RBAC over the
    // general surface is still deferred, and this fix does not pretend
    // otherwise.
    let (status, resp) = send(&app, "PUT", "/logs-2026", &legacy, "{}").await;
    assert!(
        status.is_success(),
        "a legacy key must still create an ordinary index, got {status}: {resp}"
    );
    let (status, resp) = send(
        &app,
        "POST",
        "/logs-2026/_doc/1",
        &legacy,
        r#"{"msg":"hello"}"#,
    )
    .await;
    assert!(
        status.is_success(),
        "a legacy key must still write an ordinary index, got {status}: {resp}"
    );

    // But enumeration hides the brains: `_mapping` answers, without them.
    let (status, mapping) = send(&app, "GET", "/_mapping", &legacy, "").await;
    assert_eq!(status, StatusCode::OK, "metadata still answers");
    let text = mapping.to_string();
    assert!(
        !text.contains(".xerj-memory-"),
        "brain names must not be enumerable: {text}"
    );
    assert!(
        text.contains("logs-2026"),
        "ordinary indices are still listed: {text}"
    );
    let (status, cat) = send_text(&app, "GET", "/_cat/indices", &legacy).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !cat.contains(".xerj-memory-"),
        "_cat/indices must not enumerate brains: {cat}"
    );
    assert!(
        cat.contains("logs-2026"),
        "_cat/indices still lists ordinary indices: {cat}"
    );

    // And it cannot mint its way in: `role_descriptors` from a non-superuser
    // are dropped, so the child key is unscoped too.
    let child = mint(&app, &legacy, &brain_grant("bob", r#""all""#)).await;
    let (status, resp) = send(&app, "GET", "/_graph/bob/overview", &child, "").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a key minted by a non-superuser must not carry grants: {resp}"
    );
}

/// The local-dev path must not have moved. `xerj --insecure` /
/// point-at-a-folder has one user and no configuration; every brain stays
/// reachable with no credential at all.
#[tokio::test]
async fn insecure_single_user_mode_is_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_over(dir.path().to_str().unwrap(), false);
    let app = build_es_compat_router(state);

    for brain in ["alice", "bob"] {
        let (status, body) = send(
            &app,
            "POST",
            &format!("/_graph/{brain}/link"),
            "",
            r#"{"src":"doc:1","type":"mentions","dst":"doc:2"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "insecure link failed: {body}");
        let (status, _) = send(&app, "GET", &format!("/_graph/{brain}/overview"), "", "").await;
        assert_eq!(status, StatusCode::OK, "insecure overview must work");
    }
    // Including the doors a scoped key is refused: there is nothing to scope.
    let (status, _) = send(&app, "GET", "/_mapping", "", "").await;
    assert_eq!(status, StatusCode::OK, "insecure enumeration must work");
    let (status, cat) = send_text(&app, "GET", "/_cat/indices", "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        cat.contains(".xerj-memory-"),
        "the single local user still sees their own brains: {cat}"
    );
    let (status, _) = send(
        &app,
        "POST",
        "/_search",
        "",
        r#"{"query":{"match_all":{}}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "insecure fan-out must work");
}

/// Grants are persisted with the key. A node restart must not silently turn a
/// scoped key back into an unscoped one — nor, worse, into a superuser.
#[tokio::test]
async fn grants_survive_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().to_str().unwrap().to_string();

    // Scope the first node so its data-dir lock is released before the second
    // one boots over the same directory.
    let alice = {
        let state = state_over(&data_dir, true);
        let app = build_es_compat_router(state.clone());
        seed_two_brains(&app).await;
        let admin = format!("ApiKey {ADMIN_KEY}");
        let alice = mint(&app, &admin, &brain_grant("alice", r#""read","write""#)).await;
        drop(app);
        drop(state);
        alice
    };

    // Boot a second engine over the same data directory — the restart.
    let restarted = build_es_compat_router(state_over(&data_dir, true));

    let (status, resp) = send(&restarted, "GET", "/_graph/alice/overview", &alice, "").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the grant must survive restart, got {status}: {resp}"
    );
    let (status, resp) = send(&restarted, "GET", "/_graph/bob/overview", &alice, "").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the boundary must survive restart, got {status}: {resp}"
    );
}
