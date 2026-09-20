//! Executable statement of the review findings on share links (PR #947) that
//! live in this crate. Each test was written against the pre-fix tree, failed
//! there, and names the finding it pins down.
//!
//! * **The share id reached every access log.** The guest page keeps the id in
//!   the URL fragment so that no server ever sees it — and then posted it as
//!   part of a request *path* (`POST /_share/{id}/claim`). With
//!   `logging.access_log = true` the node wrote the id to `server.log` twice
//!   per claim, and any reverse proxy or tunnel in front logged the same line.
//!   The claim is `POST /_share/claim` with `{id, passcode}` now, and the old
//!   shape is not a door of any kind.
//! * **A guest's escalation attempts left no audit line.** `POST /_share`,
//!   `GET /_share`, `DELETE /_share/{handle}` and `POST /_security/api_key`
//!   with a guest key: four `403`s, zero entries — the audit layer skipped
//!   both prefixes because "the handler audits them", and the handler never
//!   ran.
//! * **`autoindex-catalog` was shareable** while three public documents said
//!   it is never granted.
//! * **`GET _source/{id}` was documented as something a guest can call.** The
//!   router has no such route.
//! * **"Every `/_share` response is no-store"** was not true of the `401` and
//!   `403` the middleware produces, and nothing a guest *read* was marked
//!   uncacheable at all.

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use xerj_api::{
    router::{build_es_compat_router, build_native_router},
    state::AppState,
};
use xerj_common::{config::Config, metrics::Metrics};
use xerj_engine::Engine;

const ADMIN_KEY: &str = "admin-key-for-the-share-review-findings-test";

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
) -> (StatusCode, Value, HeaderMap) {
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
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, headers)
}

fn admin() -> String {
    format!("ApiKey {ADMIN_KEY}")
}

fn no_store(headers: &HeaderMap) -> bool {
    headers
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("no-store"))
}

/// An index with one document, a share on it, and the share's
/// `(id, passcode, handle)`.
async fn casefile_share(es: &axum::Router, max_claims: u32) -> (String, String, String) {
    let (status, _, _) = send(es, "PUT", "/casefile", &admin(), "").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(
        es,
        "PUT",
        "/casefile/_doc/1?refresh=true",
        &admin(),
        r#"{"title":"Lease","body":"the deposit was not returned"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, share, _) = send(
        es,
        "POST",
        "/_share",
        &admin(),
        &json!({"index": "casefile", "max_claims": max_claims}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{share}");
    (
        share["share_id"].as_str().expect("share_id").to_string(),
        share["passcode"].as_str().expect("passcode").to_string(),
        share["handle"].as_str().expect("handle").to_string(),
    )
}

/// Claim and return the guest's `Authorization` header value.
async fn claim(es: &axum::Router, id: &str, passcode: &str) -> String {
    let (status, got, _) = send(
        es,
        "POST",
        "/_share/claim",
        "",
        &json!({"id": id, "passcode": passcode}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "claim should succeed: {got}");
    format!("ApiKey {}", got["api_key"].as_str().expect("api_key"))
}

async fn audit_entries(native: &axum::Router) -> Vec<Value> {
    let (status, body, _) = send(native, "GET", "/_audit/_search?size=1000", &admin(), "").await;
    assert_eq!(status, StatusCode::OK, "audit read should succeed: {body}");
    body["entries"].as_array().cloned().unwrap_or_default()
}

// ─────────────────────────────────────────────────────────────────────────────
// The share id is never part of a request path
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_share_id_travels_in_the_claim_body_never_in_a_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_over(dir.path().to_str().expect("utf8 path"));
    let es = build_es_compat_router(state);
    let (id, passcode, handle) = casefile_share(&es, 2).await;

    // The retired shape. Pre-fix this was the open door (200 + a key) and the
    // id was in the request line. It must not be a second way in: without a
    // credential it is an ordinary unauthenticated request.
    let (status, body, _) = send(
        &es,
        "POST",
        &format!("/_share/{id}/claim"),
        "",
        &json!({"passcode": passcode}).to_string(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the id-in-path claim shape must be gone: {body}"
    );
    assert!(body.get("api_key").is_none(), "{body}");

    // The body shape: wrong passcode, no id, the handle in place of the id.
    let (status, _, headers) = send(
        &es,
        "POST",
        "/_share/claim",
        "",
        &json!({"id": id, "passcode": "not-the-code"}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(no_store(&headers));
    let (status, _, _) = send(
        &es,
        "POST",
        "/_share/claim",
        "",
        &json!({"passcode": passcode}).to_string(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a claim with no id is an unknown link"
    );
    let (status, _, _) = send(
        &es,
        "POST",
        "/_share/claim",
        "",
        &json!({"id": handle, "passcode": passcode}).to_string(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the public handle is not a claim id"
    );

    // The claim route reads a few KiB and no more: a padded body is not
    // parsed, so it names no share.
    let padded = json!({"id": id, "passcode": passcode, "pad": "x".repeat(16 * 1024)}).to_string();
    let (status, body, _) = send(&es, "POST", "/_share/claim", "", &padded).await;
    assert_ne!(status, StatusCode::OK, "an oversized claim body: {body}");
    assert!(body.get("api_key").is_none(), "{body}");

    // And the real thing.
    let (status, got, headers) = send(
        &es,
        "POST",
        "/_share/claim",
        "",
        &json!({"id": id, "passcode": passcode}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert!(got["api_key"].as_str().is_some_and(|k| k.len() > 20));
    assert_eq!(got["index"].as_str(), Some("casefile"));
    assert!(no_store(&headers));

    // `claim` is a route, not a handle: it cannot be revoked into.
    let (status, _, _) = send(&es, "DELETE", "/_share/claim", &admin(), "").await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    // GET is not the open door either.
    let (status, _, _) = send(&es, "GET", "/_share/claim", "", "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ─────────────────────────────────────────────────────────────────────────────
// A guest's escalation attempts are audited
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_guests_refused_escalation_attempts_are_audited() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_over(dir.path().to_str().expect("utf8 path"));
    let es = build_es_compat_router(state.clone());
    let native = build_native_router(state);
    let (id, passcode, handle) = casefile_share(&es, 1).await;
    let guest = claim(&es, &id, &passcode).await;

    for (method, uri) in [
        ("POST", "/_share".to_string()),
        ("GET", "/_share".to_string()),
        ("DELETE", format!("/_share/{handle}")),
        ("POST", "/_security/api_key".to_string()),
        ("GET", "/_security/api_key".to_string()),
    ] {
        let (status, body, _) = send(&es, method, &uri, &guest, "{}").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body}");
    }

    let entries = audit_entries(&native).await;
    let denied = |op: &str| -> Vec<&Value> {
        entries
            .iter()
            .filter(|e| e["op"].as_str() == Some(op) && e["outcome"].as_str() == Some("denied"))
            .collect()
    };
    for (op, resource) in [
        ("share.create", "_share"),
        ("share.list", "_share"),
        ("share.revoke", handle.as_str()),
        ("security.api_key.create", "_security/api_key"),
        ("security.api_key.get", "_security/api_key"),
    ] {
        let found = denied(op);
        assert_eq!(
            found.len(),
            1,
            "a guest's refused {op} must leave exactly one `denied` entry; log: {:?}",
            entries
                .iter()
                .map(|e| format!("{} {}", e["op"], e["outcome"]))
                .collect::<Vec<_>>()
        );
        assert_eq!(found[0]["resource"].as_str(), Some(resource), "{op}");
        let subject = found[0]["subject"].as_str().unwrap_or("");
        assert!(
            !subject.is_empty() && subject != "superuser" && subject != "unauthenticated",
            "{op} must name the guest key, got {subject:?}"
        );
    }

    // An ordinary scoped key reaches the listing handler, which writes its own
    // entry — the request-level layer must not write a second one.
    let (status, minted, _) = send(
        &es,
        "POST",
        "/_security/api_key",
        &admin(),
        r#"{"name":"reader","role_descriptors":{"r":{"indices":[{"names":["casefile"],"privileges":["read"]}]}}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{minted}");
    let reader = format!("ApiKey {}", minted["encoded"].as_str().expect("encoded"));
    let before = audit_entries(&native).await.len();
    let (status, _, _) = send(&es, "GET", "/_share", &reader, "").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let after = audit_entries(&native).await;
    let new: Vec<&Value> = after[before.min(after.len())..]
        .iter()
        .filter(|e| e["op"].as_str() == Some("share.list"))
        .collect();
    assert_eq!(new.len(), 1, "one refusal, one entry: {new:?}");

    // Nothing above wrote the share id, the passcode or the guest key down.
    let log = serde_json::to_string(&after).expect("json");
    assert!(!log.contains(&id), "the share id is in the audit log");
    assert!(!log.contains(&passcode), "the passcode is in the audit log");
    assert!(
        !log.contains(guest.trim_start_matches("ApiKey ")),
        "the guest key is in the audit log"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Nothing under /_share, and nothing a guest reads, may be cached
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn share_responses_and_guest_reads_are_never_cacheable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_over(dir.path().to_str().expect("utf8 path"));
    let es = build_es_compat_router(state);
    let (id, passcode, handle) = casefile_share(&es, 1).await;
    let guest = claim(&es, &id, &passcode).await;

    // Refusals the handlers never see.
    for (method, uri, auth, want) in [
        ("GET", "/_share".to_string(), "", StatusCode::UNAUTHORIZED),
        ("POST", "/_share".to_string(), "", StatusCode::UNAUTHORIZED),
        (
            "DELETE",
            format!("/_share/{handle}"),
            "",
            StatusCode::UNAUTHORIZED,
        ),
        (
            "GET",
            "/_share/claim".to_string(),
            "",
            StatusCode::UNAUTHORIZED,
        ),
        (
            "GET",
            "/_share".to_string(),
            guest.as_str(),
            StatusCode::FORBIDDEN,
        ),
        (
            "POST",
            "/_share".to_string(),
            guest.as_str(),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let (status, _, headers) = send(&es, method, &uri, auth, "{}").await;
        assert_eq!(status, want, "{method} {uri}");
        assert!(
            no_store(&headers),
            "{method} {uri} → {status} must be no-store, got {:?}",
            headers.get("cache-control")
        );
    }

    // What the guest reads: the documents themselves.
    for (method, uri, body) in [
        ("POST", "/casefile/_search", r#"{"query":{"match_all":{}}}"#),
        ("GET", "/casefile/_doc/1", ""),
        ("GET", "/casefile/_mapping", ""),
        ("POST", "/casefile/_count", ""),
    ] {
        let (status, _, headers) = send(&es, method, uri, &guest, body).await;
        assert_eq!(status, StatusCode::OK, "{method} {uri}");
        assert!(
            no_store(&headers),
            "a guest's {method} {uri} must be no-store, got {:?}",
            headers.get("cache-control")
        );
    }
    // …and what it is refused.
    let (status, _, headers) = send(&es, "GET", "/_cat/indices", &guest, "").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(no_store(&headers));
}

// ─────────────────────────────────────────────────────────────────────────────
// The catalog is never granted; `_source/{id}` is not a guest route
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_autoindex_catalog_cannot_be_shared_by_name_list_or_alias() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_over(dir.path().to_str().expect("utf8 path"));
    let es = build_es_compat_router(state);
    let _ = casefile_share(&es, 1).await;
    let (status, _, _) = send(&es, "PUT", "/autoindex-catalog", &admin(), "").await;
    assert_eq!(status, StatusCode::OK);
    let (status, body, _) = send(
        &es,
        "POST",
        "/_aliases",
        &admin(),
        r#"{"actions":[{"add":{"index":"autoindex-catalog","alias":"what-is-here"}}]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    for index in [
        json!("autoindex-catalog"),
        json!(["casefile", "autoindex-catalog"]),
        json!("casefile,autoindex-catalog"),
        json!("what-is-here"),
    ] {
        let (status, body, _) = send(
            &es,
            "POST",
            "/_share",
            &admin(),
            &json!({ "index": index }).to_string(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "sharing {index} must be refused: {body}"
        );
        assert!(
            body["error"]["reason"]
                .as_str()
                .is_some_and(|r| r.contains("every corpus")),
            "{body}"
        );
    }
}

#[tokio::test]
async fn source_by_id_is_not_a_route_and_not_a_guest_permission() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_over(dir.path().to_str().expect("utf8 path"));
    let es = build_es_compat_router(state);
    let (id, passcode, _) = casefile_share(&es, 1).await;
    let guest = claim(&es, &id, &passcode).await;

    // If this ever answers 200 for the admin key, the route has been added:
    // decide then whether a guest gets it, and only then document it.
    let (status, _, _) = send(&es, "GET", "/casefile/_source/1", &admin(), "").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "there is no /{{index}}/_source/{{id}} route on this node"
    );
    let (status, _, _) = send(&es, "GET", "/casefile/_source/1", &guest, "").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a route that does not exist is not pre-approved for a guest"
    );
    let (status, _, _) = send(&es, "GET", "/casefile/_doc/1", &guest, "").await;
    assert_eq!(status, StatusCode::OK);
}
