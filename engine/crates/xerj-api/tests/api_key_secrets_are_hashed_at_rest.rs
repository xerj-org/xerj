//! Executable statement of issue #201: a minted API-key secret must not be
//! recoverable from `<data_dir>/api_keys.json`, `GET /_security/_authenticate`
//! must describe the credential that actually made the call, and the
//! tamper-evident audit chain must survive a restart.
//!
//! All three used to be false at once:
//!
//! * `ApiKeyRecord.secret` held the plaintext credential and the auth path
//!   compared plaintext to plaintext, so one readable file — a backup, a
//!   snapshot, a container layer, a support bundle — handed over every live
//!   credential on the node.
//! * `security_authenticate` answered `{"username":"xerj","roles":
//!   ["superuser"]}` unconditionally, so a key with two index grants was told
//!   it was the superuser and an auditor was told every key was.
//! * the audit ring lived only in `AuditLog`'s `VecDeque`, so the evidence of
//!   an incident died with the process the incident was about.
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

const ADMIN_KEY: &str = "admin-secret-key-for-hash-at-rest-test";

/// An auth-enabled node over `data_dir`. Returned by value so a test can drop
/// it and build a second one over the same directory — that is what "restart"
/// means here.
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

fn admin() -> String {
    format!("ApiKey {ADMIN_KEY}")
}

/// Mint a key and return `(id, plaintext secret, Authorization header value)`.
async fn mint(app: &axum::Router, body: &str) -> (String, String, String) {
    let (status, resp) = send(app, "POST", "/_security/api_key", &admin(), body).await;
    assert_eq!(status, StatusCode::OK, "mint should succeed: {resp}");
    let id = resp["id"].as_str().expect("id").to_string();
    let secret = resp["api_key"].as_str().expect("api_key").to_string();
    let encoded = resp["encoded"].as_str().expect("encoded").to_string();
    (id, secret, format!("ApiKey {encoded}"))
}

fn store_text(data_dir: &std::path::Path) -> String {
    std::fs::read_to_string(data_dir.join("api_keys.json")).expect("api_keys.json")
}

/// Standard-alphabet base64 with `=` padding — the encoding the auth path
/// decodes. Local to the test so it exercises the wire format rather than the
/// engine's own (crate-private) helper.
fn b64(input: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        out.push(ALPHABET[((b0 >> 2) & 0x3F) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x3) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(((b1 & 0xF) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(b2 & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. Plaintext at rest
// ─────────────────────────────────────────────────────────────────────────────

/// The core of #201: the on-disk store must not contain the credential.
#[tokio::test]
async fn a_minted_secret_never_lands_on_disk_in_plaintext() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_over(dir.path().to_str().unwrap());
    let app = build_es_compat_router(state);

    let (_id, secret, auth) = mint(&app, r#"{"name":"leaky"}"#).await;

    let on_disk = store_text(dir.path());
    assert!(
        !on_disk.contains(&secret),
        "api_keys.json still holds the plaintext secret:\n{on_disk}"
    );

    // …and the key must still work — a "fix" that breaks authentication is
    // not a fix.
    let (status, body) = send(&app, "GET", "/_cluster/health", &auth, "").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "hashed key must authenticate: {body}"
    );
}

/// Hashing is only useful if the stored form survives the round trip that made
/// persistence worth having in the first place.
#[tokio::test]
async fn a_hashed_key_still_authenticates_after_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let auth = {
        let state = state_over(dir.path().to_str().unwrap());
        let app = build_es_compat_router(state);
        let (_id, _secret, auth) = mint(&app, r#"{"name":"survivor"}"#).await;
        auth
    };

    // Restart: a brand-new Engine over the same data directory.
    let state = state_over(dir.path().to_str().unwrap());
    let app = build_es_compat_router(state);
    let (status, body) = send(&app, "GET", "/_cluster/health", &auth, "").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "key minted before the restart must still authenticate: {body}"
    );

    // A wrong secret under the right id must still be refused — proving the
    // comparison is a comparison and not "the id was found, let them in".
    let forged = b64(&format!(
        "{}:{}",
        // id of the only key in the store
        store_key_id(dir.path()),
        "not-the-secret"
    ));
    let (status, _) = send(
        &app,
        "GET",
        "/_cluster/health",
        &format!("ApiKey {forged}"),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "forged secret must 401");
}

fn store_key_id(data_dir: &std::path::Path) -> String {
    let v: Value = serde_json::from_str(&store_text(data_dir)).expect("store json");
    v.as_object()
        .expect("object")
        .keys()
        .next()
        .expect("one key")
        .clone()
}

/// A node upgraded in place has a plaintext `api_keys.json` already on disk.
/// Loading it must keep the key working *and* rewrite the file without the
/// secret — a migration that only applies to new keys leaves every existing
/// deployment exposed.
#[tokio::test]
async fn a_legacy_plaintext_store_is_migrated_on_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let id = "11111111-2222-3333-4444-555555555555";
    let secret = "legacy-plaintext-secret-value";
    let legacy = format!(
        r#"{{"{id}":{{"name":"legacy","secret":"{secret}",
             "creation_ms":1753600000000,"expiration_ms":null,
             "invalidated":false}}}}"#
    );
    std::fs::write(dir.path().join("api_keys.json"), legacy).expect("seed legacy store");

    let state = state_over(dir.path().to_str().unwrap());
    let app = build_es_compat_router(state);

    let encoded = b64(&format!("{id}:{secret}"));
    let (status, body) = send(
        &app,
        "GET",
        "/_cluster/health",
        &format!("ApiKey {encoded}"),
        "",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a migrated legacy key must keep working: {body}"
    );

    let on_disk = store_text(dir.path());
    assert!(
        !on_disk.contains(secret),
        "legacy plaintext survived the migration:\n{on_disk}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. `_authenticate` must not lie
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn authenticate_describes_the_credential_that_called_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_over(dir.path().to_str().unwrap());
    let app = build_es_compat_router(state);

    // The admin key really is the superuser — that answer must not change.
    let (status, me) = send(&app, "GET", "/_security/_authenticate", &admin(), "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["roles"], serde_json::json!(["superuser"]));
    assert_eq!(me["authentication_type"], "realm");

    // A scoped key is not the superuser and must not be told that it is.
    let (id, _secret, scoped) = mint(
        &app,
        r#"{"name":"scoped-agent","role_descriptors":{"reader":{"indices":[
             {"names":["logs-*"],"privileges":["read"]}]}}}"#,
    )
    .await;
    let (status, me) = send(&app, "GET", "/_security/_authenticate", &scoped, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(
        me["roles"],
        serde_json::json!(["superuser"]),
        "a scoped key was told it is the superuser: {me}"
    );
    assert_eq!(me["roles"], serde_json::json!(["reader"]));
    assert_eq!(me["authentication_type"], "api_key");
    assert_eq!(me["authentication_realm"]["name"], "_es_api_key");
    assert_eq!(me["api_key"]["id"], id);
    assert_eq!(me["api_key"]["name"], "scoped-agent");

    // One descriptor with several `indices` entries becomes several internal
    // `Role`s (`reader[0]`, `reader[1]`, …). The caller named one role and
    // must be told about one role, under the name they used — reporting the
    // internal encoding would describe roles that do not exist.
    let (_id3, _s3, multi) = mint(
        &app,
        r#"{"name":"multi-entry","role_descriptors":{"analyst":{"indices":[
             {"names":["logs-*"],"privileges":["read"]},
             {"names":["metrics-*"],"privileges":["read","write"]}]},
           "auditor":{"indices":[{"names":["audit-*"],"privileges":["read"]}]}}}"#,
    )
    .await;
    let (status, me) = send(&app, "GET", "/_security/_authenticate", &multi, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        me["roles"],
        serde_json::json!(["analyst", "auditor"]),
        "roles must be the descriptor names, deduped: {me}"
    );

    // An unscoped key holds no named role at all.
    let (_id2, _s2, unscoped) = mint(&app, r#"{"name":"plain-agent"}"#).await;
    let (status, me) = send(&app, "GET", "/_security/_authenticate", &unscoped, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(
        me["roles"],
        serde_json::json!(["superuser"]),
        "an unscoped key was told it is the superuser: {me}"
    );
    assert_eq!(me["authentication_type"], "api_key");
}

/// A `role_descriptors` key is caller-chosen free text, and `_authenticate`
/// reports those names in `roles` — so nothing may stop a caller from *naming*
/// a descriptor `superuser` and being handed `{"roles":["superuser"]}` back for
/// a key confined to `logs-*`. That is the same "you are more privileged than
/// you are" drift #201 exists to remove, re-entered through a name.
///
/// `superuser` and `unscoped` are the two labels xerj assigns itself in this
/// field; a caller-chosen descriptor must never be able to produce either.
#[tokio::test]
async fn a_descriptor_name_cannot_forge_a_xerj_assigned_role_label() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_over(dir.path().to_str().unwrap());
    let app = build_es_compat_router(state);

    for forged in ["superuser", "unscoped"] {
        let (_id, _secret, auth) = mint(
            &app,
            &format!(
                r#"{{"name":"pretender-{forged}","role_descriptors":{{"{forged}":{{"indices":[
                     {{"names":["logs-*"],"privileges":["read"]}}]}}}}}}"#
            ),
        )
        .await;

        let (status, me) = send(&app, "GET", "/_security/_authenticate", &auth, "").await;
        assert_eq!(status, StatusCode::OK);
        let roles = me["roles"].as_array().expect("roles array");
        assert!(
            !roles.iter().any(|r| r == forged),
            "a key confined to logs-* reported the reserved label {forged:?}: {me}"
        );

        // The grant itself is unaffected — this is about what the key is
        // *told* it holds, not about narrowing what it holds. A read inside
        // the granted pattern still works; one outside it is still refused.
        let (status, body) = send(&app, "GET", "/logs-app/_search", &auth, "").await;
        assert_ne!(
            status,
            StatusCode::FORBIDDEN,
            "the logs-* grant must survive the label fix: {body}"
        );
        let (status, body) = send(&app, "GET", "/.xerj-memory-alice/_search", &auth, "").await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "a logs-* key must not reach the reserved namespace: {body}"
        );
    }
}

/// `GET /_security/api_key` must describe a key with the descriptor names the
/// caller minted it with. It used to key `role_descriptors` by the internal
/// per-entry role name, so one `reader` descriptor over two index sets came
/// back as `reader[0]` and `reader[1]` — names the caller never wrote, in a
/// shape that could not be posted back to recreate the key.
#[tokio::test]
async fn listing_a_key_reports_the_descriptors_it_was_minted_with() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_over(dir.path().to_str().unwrap());
    let app = build_es_compat_router(state);

    let minted = r#"{"indices":[
         {"names":["logs-*"],"privileges":["read"]},
         {"names":["metrics-*"],"privileges":["read","write"]}]}"#;
    let (id, _secret, _auth) = mint(
        &app,
        &format!(r#"{{"name":"round-trip","role_descriptors":{{"reader":{minted}}}}}"#),
    )
    .await;

    let (status, body) = send(
        &app,
        "GET",
        &format!("/_security/api_key?id={id}"),
        &admin(),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let descriptors = &body["api_keys"][0]["role_descriptors"];
    assert_eq!(
        descriptors.as_object().map(|o| o.len()),
        Some(1),
        "one descriptor was minted, so one must be listed: {descriptors}"
    );
    assert!(
        descriptors.get("reader").is_some(),
        "descriptor must keep the caller's name, not the internal encoding: {descriptors}"
    );
    let entries = descriptors["reader"]["indices"]
        .as_array()
        .expect("indices array");
    assert_eq!(
        entries.len(),
        2,
        "both index entries survive: {descriptors}"
    );
    assert_eq!(entries[0]["names"], serde_json::json!(["logs-*"]));
    assert_eq!(entries[1]["names"], serde_json::json!(["metrics-*"]));
    assert_eq!(
        entries[1]["privileges"],
        serde_json::json!(["read", "write"])
    );
}

/// A record that carries a `secret_hash` which is *present but not a hash*
/// (truncated by a half-written file, mangled by a hand edit, written by a
/// future scheme this build does not know) can never authenticate: every
/// verifier in `secret_hash.rs` denies what it cannot decode. Keeping such a
/// record would list it through `GET /_security/api_key` as a live credential
/// that nothing can ever use — the accept-then-ignore shape issue #204 tracks,
/// and exactly what the empty-hash case is already dropped to avoid.
///
/// The discriminator is `secret_hash::is_usable_hash`, not `is_empty`.
#[tokio::test]
async fn an_unusable_secret_hash_is_dropped_not_listed_as_live() {
    for (case, stored) in [
        ("empty", ""),
        ("truncated", "$ssha256$truncated"),
        ("wrong-scheme", "$argon2id$v=19$m=1,t=1,p=1$c2FsdA$aGFzaA"),
        ("bare-plaintext-leftover", "not-a-hash-at-all"),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let id = "99999999-8888-7777-6666-555555555555";
        std::fs::write(
            dir.path().join("api_keys.json"),
            format!(
                r#"{{"{id}":{{"name":"bricked-{case}","secret_hash":"{stored}",
                     "creation_ms":1753600000000,"expiration_ms":null,
                     "invalidated":false}}}}"#
            ),
        )
        .expect("seed store");

        let state = state_over(dir.path().to_str().unwrap());
        let app = build_es_compat_router(state);

        let (status, body) = send(&app, "GET", "/_security/api_key", &admin(), "").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["api_keys"].as_array().map(Vec::len),
            Some(0),
            "[{case}] a record that can never authenticate was listed as a live key: {body}"
        );

        // And it must not authenticate either — including with the stored
        // string presented as if it were the secret.
        for presented in [stored, "anything"] {
            let encoded = b64(&format!("{id}:{presented}"));
            let (status, _) = send(
                &app,
                "GET",
                "/_cluster/health",
                &format!("ApiKey {encoded}"),
                "",
            )
            .await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "[{case}] a record with an unusable hash authenticated"
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. The audit chain must outlive the process
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_audit_chain_survives_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let state = state_over(dir.path().to_str().unwrap());
        let app = build_es_compat_router(state);
        mint(&app, r#"{"name":"audited"}"#).await;
    }

    let state = state_over(dir.path().to_str().unwrap());
    let native = build_native_router(state);
    let (status, body) = send(&native, "GET", "/_audit/_search", &admin(), "").await;
    assert_eq!(status, StatusCode::OK, "audit search: {body}");
    let entries = body["entries"].as_array().expect("entries array");
    assert!(
        entries.iter().any(|e| e["op"] == "security.api_key.create"),
        "the pre-restart audit entry is gone: {body}"
    );

    let (status, verified) = send(&native, "GET", "/_audit/_verify", &admin(), "").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "restored chain must still verify: {verified}"
    );
    assert_eq!(verified["ok"], true, "{verified}");
}
