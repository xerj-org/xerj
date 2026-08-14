//! Executable statement of what the audit log can answer (issue #329).
//!
//! "Audit logs" in an enterprise sense means being able to answer **who
//! touched what data, and when**, and to hand that answer to an auditor. XERJ
//! shipped a real hash-chained, restart-surviving log that could not answer it,
//! for three independent reasons the issue enumerates and a fourth found while
//! reproducing them:
//!
//! 1. writes were not audited at all — a `PUT`, a `DELETE` and 30 000 bulk
//!    documents left zero entries;
//! 2. the one audited data-path op passed the literal `"anonymous"`, so even
//!    the recorded searches were unattributable — and on an auth-enforced node
//!    that literal was not merely uninformative, it was false;
//! 3. `/_audit/_search` and `/_audit/_verify` consulted no privilege, so a key
//!    scoped to read one index read the whole log, security events included;
//! 4. refused requests left no entry either, so "who tried and was told no"
//!    was unanswerable.
//!
//! Every assertion below fails on the pre-#329 tree. They are written against
//! the **real routers**, both of them, because the exposure was never about
//! one handler: writes arrive on `:9200` and the audit endpoints live on
//! `:8080`, over one shared engine.

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

const ADMIN_KEY: &str = "admin-secret-key-for-audit-test";

fn admin() -> String {
    format!("ApiKey {ADMIN_KEY}")
}

/// An auth-enabled node over a fresh data directory. The `TempDir` is returned
/// and must be held for the whole test — the Engine keeps the directory open.
fn auth_enabled_state() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
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

/// Mint an API key with the given `role_descriptors` body, returning its
/// `Authorization` header value.
async fn mint(app: &axum::Router, body: &str) -> String {
    let (status, resp) = send(app, "POST", "/_security/api_key", &admin(), body).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "minting a key should succeed: {resp}"
    );
    format!("ApiKey {}", resp["encoded"].as_str().expect("encoded key"))
}

/// Read the log back through the real endpoint, as the admin key.
async fn entries(native: &axum::Router) -> Vec<Value> {
    let (status, body) = send(native, "GET", "/_audit/_search", &admin(), "").await;
    assert_eq!(status, StatusCode::OK, "admin must read the audit log");
    body["entries"].as_array().cloned().unwrap_or_default()
}

/// Every entry whose `op` matches, in chain order.
fn with_op<'a>(entries: &'a [Value], op: &str) -> Vec<&'a Value> {
    entries.iter().filter(|e| e["op"] == op).collect()
}

// ─────────────────────────────────────────────────────────────────────────────

/// Gaps 1 and 2, together, because they are one question: an auditor asking
/// "did anyone change this record, and who" gets an answer.
///
/// This is the issue's own reproduction, verbatim in shape: an authenticated
/// write, an authenticated delete, an index creation, and a search. Before
/// #329 the first three produced **nothing** and the fourth said
/// `subject: "anonymous"` while holding the admin key.
#[tokio::test]
async fn a_write_leaves_an_attributed_entry() {
    let (state, _dir) = auth_enabled_state();
    let es = build_es_compat_router(state.clone());
    let native = build_native_router(state.clone());

    let (status, body) = send(
        &es,
        "PUT",
        "/payroll/_doc/1?refresh=true",
        &admin(),
        r#"{"employee":"alice","salary":250000}"#,
    )
    .await;
    assert!(status.is_success(), "indexing failed: {status} {body}");
    let (status, _) = send(&es, "DELETE", "/payroll/_doc/1?refresh=true", &admin(), "").await;
    assert!(status.is_success(), "delete failed: {status}");
    let (status, _) = send(&es, "PUT", "/ledger", &admin(), "{}").await;
    assert!(status.is_success(), "index create failed: {status}");
    let (status, _) = send(
        &es,
        "POST",
        "/payroll/_search",
        &admin(),
        r#"{"query":{"match_all":{}}}"#,
    )
    .await;
    assert!(status.is_success(), "search failed: {status}");

    let entries = entries(&native).await;

    // 1. The write, the delete and the index creation are all recorded, each
    //    naming the index it touched.
    for (op, resource) in [
        ("index", "payroll"),
        ("delete", "payroll"),
        ("index.create", "ledger"),
    ] {
        let found = with_op(&entries, op);
        assert_eq!(
            found.len(),
            1,
            "expected exactly one {op} entry, got {found:?} out of {entries:?}"
        );
        assert_eq!(found[0]["resource"], resource);
        assert_eq!(found[0]["outcome"], "ok");
    }

    // 2. Every entry names the authenticated caller. The literal that used to
    //    be here was not merely vague — auth was enforced and the caller was
    //    the admin key, so "anonymous" was the opposite of true.
    for entry in &entries {
        assert_eq!(
            entry["subject"], "superuser",
            "every entry must name the authenticated subject: {entry}"
        );
        assert_ne!(
            entry["subject"], "anonymous",
            "the literal is gone: {entry}"
        );
    }
    assert_eq!(
        with_op(&entries, "search").len(),
        1,
        "the search is still audited exactly once, from its own handler"
    );

    // A read that succeeded leaves nothing beyond the search the handler
    // records itself — otherwise reading the audit log would grow it.
    assert!(
        with_op(&entries, "audit").is_empty(),
        "a successful /_audit read must not audit itself: {entries:?}"
    );

    // And the chain is still intact over everything just added.
    let (status, verified) = send(&native, "GET", "/_audit/_verify", &admin(), "").await;
    assert_eq!(status, StatusCode::OK, "chain broke: {verified}");
    assert_eq!(verified["ok"], true);
}

/// Granularity, pinned so it cannot regress into per-document.
///
/// A bulk is **one** entry. Per-document appends would serialise the batch
/// behind one lock-held `write(2)` per item and overrun the 4096-entry ring
/// inside a single request, turning the log into a few-seconds rolling window
/// — a regression disguised as a feature. The batch size and the indices it
/// actually touched go in the note instead, because `POST /_bulk` names its
/// indices per action line and the URL names none.
#[tokio::test]
async fn a_bulk_is_one_entry_that_names_its_indices() {
    let (state, _dir) = auth_enabled_state();
    let es = build_es_compat_router(state.clone());
    let native = build_native_router(state.clone());

    let ndjson = concat!(
        "{\"index\":{\"_index\":\"payroll\",\"_id\":\"1\"}}\n",
        "{\"employee\":\"alice\"}\n",
        "{\"index\":{\"_index\":\"payroll\",\"_id\":\"2\"}}\n",
        "{\"employee\":\"bob\"}\n",
        "{\"index\":{\"_index\":\"ledger\",\"_id\":\"1\"}}\n",
        "{\"amount\":7}\n",
    );
    let (status, body) = send(&es, "POST", "/_bulk", &admin(), ndjson).await;
    assert!(status.is_success(), "bulk failed: {status} {body}");

    let entries = entries(&native).await;
    let bulk = with_op(&entries, "bulk");
    assert_eq!(
        bulk.len(),
        1,
        "a bulk must leave exactly one entry, not one per document: {entries:?}"
    );
    assert_eq!(bulk[0]["subject"], "superuser");
    assert_eq!(bulk[0]["outcome"], "ok");
    let note = bulk[0]["note"].as_str().expect("note");
    assert!(
        note.contains("items=3"),
        "the note must carry the batch size, got {note:?}"
    );
    assert!(
        note.contains("indices=[ledger,payroll]"),
        "the note must carry the indices the action lines named, got {note:?}"
    );
}

/// Gap 3: `/_audit/*` is privilege-gated.
///
/// The pre-#329 handlers took `State` and nothing else, so a key scoped to
/// `read` on one index answered 200 and read every entry — including the
/// `security.api_key.create` events naming other keys.
///
/// Shipping the gate without a way to *hold* `AuditRead` would have been the
/// other half of the same bug (an endpoint that silently became admin-key-only
/// and a seeded `auditor` role that stayed decorative), so the grant path is
/// asserted here in the same test as the denial.
#[tokio::test]
async fn the_audit_endpoints_require_read_audit() {
    let (state, _dir) = auth_enabled_state();
    let es = build_es_compat_router(state.clone());
    let native = build_native_router(state.clone());

    let scoped = mint(
        &es,
        r#"{"name":"reader","role_descriptors":{"r":{"indices":[{"names":["payroll"],"privileges":["read"]}]}}}"#,
    )
    .await;
    let auditor = mint(
        &es,
        r#"{"name":"auditor","role_descriptors":{"a":{"indices":[{"names":["*"],"privileges":["read_audit"]}]}}}"#,
    )
    .await;

    for path in ["/_audit/_search", "/_audit/_verify"] {
        let (status, body) = send(&native, "GET", path, &scoped, "").await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "a key scoped to one index must not read {path}: {body}"
        );
        assert_eq!(body["error"]["type"], "security_exception");
        assert!(
            body["error"]["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("read_audit"),
            "the 403 must name the privilege that would fix it: {body}"
        );

        // A key that actually holds `read_audit` passes, and so does the
        // admin key — the gate is a gate, not a wall.
        let (status, _) = send(&native, "GET", path, &auditor, "").await;
        assert_eq!(
            status,
            StatusCode::OK,
            "a read_audit grant must reach {path}"
        );
        let (status, _) = send(&native, "GET", path, &admin(), "").await;
        assert_eq!(status, StatusCode::OK, "the admin key must reach {path}");
    }

    // The refusal is itself evidence: it names the endpoint and the key that
    // was turned away, not a bare 403 in an access log nobody kept.
    let entries = entries(&native).await;
    let denied: Vec<&Value> = entries
        .iter()
        .filter(|e| e["outcome"] == "denied")
        .collect();
    assert!(
        denied.iter().any(|e| e["resource"] == "_audit/_search"),
        "the refused audit read must be recorded: {entries:?}"
    );
    for entry in &denied {
        assert_ne!(
            entry["subject"], "superuser",
            "a denial must name the key that was refused: {entry}"
        );
        assert_ne!(entry["subject"], "unattributed");
    }
}

/// Gap 4, which the issue does not mention: a refused request is recorded.
///
/// Without this, "who tried and was refused" is unanswerable — an auditor can
/// see what succeeded and nothing about what was attempted. Only
/// *authenticated* callers are recorded: a 401 never reaches the router, on
/// purpose, so nobody who can merely reach the port can flood the ring and
/// evict real evidence.
#[tokio::test]
async fn a_refused_request_is_recorded_but_an_unauthenticated_one_is_not() {
    let (state, _dir) = auth_enabled_state();
    let es = build_es_compat_router(state.clone());
    let native = build_native_router(state.clone());

    let scoped = mint(
        &es,
        r#"{"name":"reader","role_descriptors":{"r":{"indices":[{"names":["payroll"],"privileges":["read"]}]}}}"#,
    )
    .await;

    // Refused: a read of an index the key was never granted, and a write to
    // the one it may only read.
    let (status, _) = send(&es, "POST", "/secrets/_search", &scoped, r#"{}"#).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(&es, "PUT", "/payroll/_doc/9", &scoped, r#"{"a":1}"#).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Not authenticated at all: a clean 401 that leaves no entry.
    let (status, _) = send(&es, "PUT", "/payroll/_doc/9", "ApiKey nope", r#"{"a":1}"#).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let entries = entries(&native).await;
    let denied: Vec<&Value> = entries
        .iter()
        .filter(|e| e["outcome"] == "denied")
        .collect();
    assert_eq!(
        denied.len(),
        2,
        "both refusals and only those must be recorded: {entries:?}"
    );
    let read = denied
        .iter()
        .find(|e| e["resource"] == "secrets")
        .expect("the refused read must be recorded");
    assert_eq!(read["op"], "search", "the attempted op is the useful half");
    let write = denied
        .iter()
        .find(|e| e["resource"] == "payroll")
        .expect("the refused write must be recorded");
    assert_eq!(write["op"], "index");
    // Both name the key, not "anonymous" and not "unattributed".
    for entry in &denied {
        let subject = entry["subject"].as_str().unwrap_or_default();
        assert!(
            !subject.is_empty() && subject != "anonymous" && subject != "unattributed",
            "a denial must name the key that made it: {entry}"
        );
    }
}

/// Retention is configurable, and the ring is still a ring.
///
/// Auditing writes is what made this a decision rather than a constant: 4096
/// entries is a few seconds of a node doing single-document writes, and the
/// hard-coded capacity meant an operator could not do anything about it. What
/// must NOT change is that the log stays a bounded rolling window — raising
/// the capacity buys history, it does not buy an archive.
#[tokio::test]
async fn capacity_is_configurable_and_still_bounded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.auth.enabled = true;
    config.auth.admin_api_key = ADMIN_KEY.to_string();
    config.audit.capacity = 8;
    let metrics = Metrics::new().expect("metrics");
    let engine = Engine::new(config.clone()).expect("engine");
    let state = AppState::new(config, engine, metrics);
    let es = build_es_compat_router(state.clone());
    let native = build_native_router(state.clone());

    for i in 0..20 {
        let (status, _) = send(
            &es,
            "PUT",
            &format!("/payroll/_doc/{i}"),
            &admin(),
            r#"{"a":1}"#,
        )
        .await;
        assert!(status.is_success());
    }

    let entries = entries(&native).await;
    assert_eq!(entries.len(), 8, "the ring must honour audit.capacity");
    // …and a rotated ring still verifies, so the window is evidence rather
    // than a permanent "tampered" report (issue #201's fix, still holding).
    let (status, verified) = send(&native, "GET", "/_audit/_verify", &admin(), "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(verified["ok"], true, "a rotated ring must still verify");

    // A zero capacity is a disabled log wearing an enabled one's name.
    let mut bad = Config::default();
    bad.audit.capacity = 0;
    assert!(
        bad.validate().is_err(),
        "audit.capacity = 0 must be refused at startup"
    );
}
