//! Executable statement of issue #329: the audit log must record **writes**,
//! must name **who** made them, and must not be readable by any authenticated
//! key that happens to reach the node.
//!
//! Three independent gaps, all reproduced on the pre-fix tree against a live
//! node with auth *enforced* (not `--insecure`):
//!
//! * A `PUT /_doc`, a `DELETE /_doc`, a `_bulk` and a `PUT /{index}` left the
//!   audit log completely empty — the only entry was the search that followed
//!   them. An auditor asking "did anyone change this record" got nothing, and
//!   `audit.rs`'s own module doc said an absent entry was not evidence a write
//!   had not happened.
//! * The one audited data-path op recorded `subject: "anonymous"` for a call
//!   authenticated as the admin key, so even the searches that were logged
//!   were not attributable.
//! * A key scoped to `read` on one index — correctly 403'd on every other
//!   index — read the whole audit log, security events included.
//!
//! Every assertion below fails on the pre-fix tree.

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

const ADMIN_KEY: &str = "admin-key-for-the-audit-coverage-test";

fn state_over(data_dir: &str) -> AppState {
    let mut config = Config::default();
    config.server.data_dir = data_dir.to_string();
    config.auth.enabled = true;
    config.auth.admin_api_key = ADMIN_KEY.to_string();
    let metrics = Metrics::new().expect("metrics");
    let engine = Engine::new(config.clone()).expect("engine");
    AppState::new(config, engine, metrics)
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    auth: &str,
    body: &str,
) -> (StatusCode, Value) {
    send_with_type(app, method, uri, auth, body, "application/json").await
}

async fn send_with_type(
    app: &axum::Router,
    method: &str,
    uri: &str,
    auth: &str,
    body: &str,
    content_type: &str,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", content_type);
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

fn admin() -> String {
    format!("ApiKey {ADMIN_KEY}")
}

/// Mint a key and return its `(id, Authorization header value)`.
async fn mint(app: &axum::Router, body: &str) -> (String, String) {
    let (status, resp) = send(app, "POST", "/_security/api_key", &admin(), body).await;
    assert_eq!(status, StatusCode::OK, "mint should succeed: {resp}");
    let id = resp["id"].as_str().expect("id").to_string();
    let encoded = resp["encoded"].as_str().expect("encoded").to_string();
    (id, format!("ApiKey {encoded}"))
}

async fn audit_entries(native: &axum::Router, auth: &str) -> Vec<Value> {
    let (status, body) = send(native, "GET", "/_audit/_search", auth, "").await;
    assert_eq!(status, StatusCode::OK, "audit read should succeed: {body}");
    body["entries"].as_array().cloned().unwrap_or_default()
}

/// Every entry recorded for `op`.
fn ops<'a>(entries: &'a [Value], op: &str) -> Vec<&'a Value> {
    entries
        .iter()
        .filter(|e| e["op"].as_str() == Some(op))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Gap 1 — writes are audited at all
// ─────────────────────────────────────────────────────────────────────────────

/// The whole point of the ask: a data change must produce an entry. Before the
/// fix this test found an audit log holding exactly one entry (the search),
/// however many documents had been written and deleted first.
#[tokio::test]
async fn a_write_leaves_an_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_over(dir.path().to_str().expect("utf8 path"));
    let es = build_es_compat_router(state.clone());
    let native = build_native_router(state);

    let (status, _) = send(&es, "PUT", "/payroll", &admin(), "").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        &es,
        "PUT",
        "/payroll/_doc/1?refresh=true",
        &admin(),
        r#"{"employee":"alice","salary":250000}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = send(
        &es,
        "POST",
        "/payroll/_update/1",
        &admin(),
        r#"{"doc":{"salary":260000}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&es, "DELETE", "/payroll/_doc/1?refresh=true", &admin(), "").await;
    assert_eq!(status, StatusCode::OK);
    // Index-scoped bulk, so the entry names the index. The global `/_bulk`
    // records the endpoint instead — it may name a dozen indices in its body,
    // and a guessed one would be worse evidence than the endpoint plus the
    // counts the note carries.
    let (status, _) = send_with_type(
        &es,
        "POST",
        "/payroll/_bulk",
        &admin(),
        "{\"index\":{\"_id\":\"7\"}}\n{\"employee\":\"bob\"}\n",
        "application/x-ndjson",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&es, "DELETE", "/payroll", &admin(), "").await;
    assert_eq!(status, StatusCode::OK);

    let entries = audit_entries(&native, &admin()).await;
    for op in [
        "index.create",
        "index",
        "update",
        "delete",
        "bulk",
        "index.delete",
    ] {
        let found = ops(&entries, op);
        assert!(
            !found.is_empty(),
            "a {op} must leave an audit entry; got ops {:?}",
            entries
                .iter()
                .map(|e| e["op"].as_str().unwrap_or(""))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            found[0]["resource"].as_str(),
            Some("payroll"),
            "{op} must name the index it touched"
        );
        assert_eq!(found[0]["outcome"].as_str(), Some("ok"), "{op} succeeded");
    }
    // The chain still verifies with the write entries in it.
    let (status, verified) = send(&native, "GET", "/_audit/_verify", &admin(), "").await;
    assert_eq!(status, StatusCode::OK, "{verified}");
    assert_eq!(verified["ok"].as_bool(), Some(true));
}

/// A write that is refused must be recorded as refused. "Nothing happened" and
/// "someone tried and was stopped" are different answers to an auditor, and
/// the pre-fix log could express neither.
#[tokio::test]
async fn a_denied_write_is_recorded_as_denied() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_over(dir.path().to_str().expect("utf8 path"));
    let es = build_es_compat_router(state.clone());
    let native = build_native_router(state);

    let (_, reader) = mint(
        &es,
        r#"{"name":"reader","role_descriptors":{"r":{"indices":[
             {"names":["payroll"],"privileges":["read"]}]}}}"#,
    )
    .await;
    let (status, _) = send(
        &es,
        "PUT",
        "/payroll/_doc/1",
        &reader,
        r#"{"employee":"mallory"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "read-only key cannot write");

    let entries = audit_entries(&native, &admin()).await;
    let denied: Vec<_> = entries
        .iter()
        .filter(|e| e["outcome"].as_str() == Some("denied"))
        .collect();
    assert!(
        !denied.is_empty(),
        "the refused write must be in the log; got {:?}",
        entries
            .iter()
            .map(|e| (e["op"].as_str(), e["outcome"].as_str()))
            .collect::<Vec<_>>()
    );
    assert_eq!(denied[0]["resource"].as_str(), Some("payroll"));
}

/// One bulk request is one entry, whatever it carries.
///
/// This is the retention half of the issue: the ring holds
/// `DEFAULT_AUDIT_CAPACITY` entries, so auditing per *document* would let a
/// single ingest evict every other event on the node. The entry carries the
/// item counts instead.
#[tokio::test]
async fn a_bulk_is_one_entry_not_one_per_document() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_over(dir.path().to_str().expect("utf8 path"));
    let es = build_es_compat_router(state.clone());
    let native = build_native_router(state);

    let mut ndjson = String::new();
    for i in 0..200 {
        ndjson.push_str(&format!(
            "{{\"index\":{{\"_index\":\"logs\",\"_id\":\"{i}\"}}}}\n{{\"n\":{i}}}\n"
        ));
    }
    let (status, _) = send_with_type(
        &es,
        "POST",
        "/_bulk",
        &admin(),
        &ndjson,
        "application/x-ndjson",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let entries = audit_entries(&native, &admin()).await;
    let bulks = ops(&entries, "bulk");
    assert_eq!(bulks.len(), 1, "200 documents, one bulk request, one entry");
    let note = bulks[0]["note"].as_str().unwrap_or("");
    assert!(
        note.contains("items=200") && note.contains("failed=0"),
        "the entry must say how much it covered, got {note:?}"
    );
    assert!(
        note.contains("indices=[logs]"),
        "…and which indices it reached, got {note:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Gap 2 — the entry names who did it
// ─────────────────────────────────────────────────────────────────────────────

/// `subject` was the string literal `"anonymous"` on every search, including
/// searches authenticated as the admin key. It must be the caller.
#[tokio::test]
async fn the_subject_is_the_caller_not_anonymous() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_over(dir.path().to_str().expect("utf8 path"));
    let es = build_es_compat_router(state.clone());
    let native = build_native_router(state);

    let (status, _) = send(
        &es,
        "PUT",
        "/payroll/_doc/1?refresh=true",
        &admin(),
        r#"{"employee":"alice"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = send(
        &es,
        "POST",
        "/payroll/_search",
        &admin(),
        r#"{"query":{"match_all":{}}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (reader_id, reader) = mint(
        &es,
        r#"{"name":"reader","role_descriptors":{"r":{"indices":[
             {"names":["payroll"],"privileges":["read"]}]}}}"#,
    )
    .await;
    let (status, _) = send(
        &es,
        "POST",
        "/payroll/_search",
        &reader,
        r#"{"query":{"match_all":{}}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let entries = audit_entries(&native, &admin()).await;
    assert!(
        !entries.iter().any(|e| e["subject"] == "anonymous"),
        "no authenticated request may be logged as anonymous: {:?}",
        entries
            .iter()
            .map(|e| (e["op"].as_str(), e["subject"].as_str()))
            .collect::<Vec<_>>()
    );
    let searches = ops(&entries, "search");
    assert_eq!(searches.len(), 2, "two searches");
    assert_eq!(searches[0]["subject"].as_str(), Some("superuser"));
    assert_eq!(
        searches[1]["subject"].as_str(),
        Some(reader_id.as_str()),
        "the scoped key's search must be attributed to that key"
    );
    // …and the write from the same caller carries the same subject.
    assert_eq!(ops(&entries, "index")[0]["subject"].as_str(), Some("superuser"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Gap 3 — the log is not readable by everyone
// ─────────────────────────────────────────────────────────────────────────────

/// A key scoped to one index read every entry on the node, security events
/// included. `Privilege::AuditRead` existed and was never consulted.
#[tokio::test]
async fn a_scoped_key_cannot_read_the_audit_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_over(dir.path().to_str().expect("utf8 path"));
    let es = build_es_compat_router(state.clone());
    let native = build_native_router(state);

    let (_, reader) = mint(
        &es,
        r#"{"name":"reader","role_descriptors":{"r":{"indices":[
             {"names":["payroll"],"privileges":["read"]}]}}}"#,
    )
    .await;

    for uri in ["/_audit/_search", "/_audit/_verify"] {
        let (status, body) = send(&native, "GET", uri, &reader, "").await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{uri} must require read_audit, got {body}"
        );
        assert!(
            body["error"]["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("read_audit"),
            "the 403 must name the privilege: {body}"
        );
    }

    // An unscoped key — one minted with no role_descriptors — is not an
    // auditor either.
    let (_, unscoped) = mint(&es, r#"{"name":"plain"}"#).await;
    let (status, _) = send(&native, "GET", "/_audit/_search", &unscoped, "").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The superuser still reads it.
    let (status, _) = send(&native, "GET", "/_audit/_search", &admin(), "").await;
    assert_eq!(status, StatusCode::OK);
}

/// The privilege has to be grantable, or the gate above just means
/// "superuser only" and the enterprise ask — hand an auditor a credential that
/// reads the audit log and nothing else — is still unmet.
#[tokio::test]
async fn an_auditor_key_reads_the_log_and_nothing_else() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_over(dir.path().to_str().expect("utf8 path"));
    let es = build_es_compat_router(state.clone());
    let native = build_native_router(state);

    let (status, _) = send(
        &es,
        "PUT",
        "/payroll/_doc/1?refresh=true",
        &admin(),
        r#"{"employee":"alice"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, auditor) = mint(
        &es,
        r#"{"name":"auditor","role_descriptors":{"a":{"cluster":["read_audit"],"indices":[]}}}"#,
    )
    .await;

    let (status, body) = send(&native, "GET", "/_audit/_search", &auditor, "").await;
    assert_eq!(status, StatusCode::OK, "an auditor key reads the log: {body}");
    assert!(
        !body["entries"].as_array().expect("entries").is_empty(),
        "and sees the writes it is auditing"
    );
    let (status, _) = send(&native, "GET", "/_audit/_verify", &auditor, "").await;
    assert_eq!(status, StatusCode::OK);

    // The audit grant is not an index grant: the auditor cannot read the data.
    let (status, _) = send(
        &es,
        "POST",
        "/payroll/_search",
        &auditor,
        r#"{"query":{"match_all":{}}}"#,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "read_audit must not grant index reads"
    );
}
