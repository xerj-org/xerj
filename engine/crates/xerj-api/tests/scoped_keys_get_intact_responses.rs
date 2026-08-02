//! A scoped key gets **structurally intact** metadata responses (issue #79).
//!
//! Enumeration pruning removes index names a caller may not read. The first
//! cut did it by deleting any object key equal to a known index name, at every
//! depth of the response. An index name is an arbitrary string, so ordinary
//! names collide with the structural keys of the very responses being pruned —
//! and only for a scoped key, i.e. exactly the multi-tenant and Kibana case the
//! branch exists to serve. A superuser skips pruning entirely and never saw it.
//!
//! Every index created here has a perfectly ordinary name that happens to be a
//! structural key somewhere: `status`, `indices`, `type`, `nodes`, `count`.
//! None of them is granted to the scoped key, so all of them are legitimately
//! prunable *as index names* — which is what makes them the sharp test. The
//! responses must keep their shape anyway.
//!
//! The opposite failure is equally a failure, so every test also re-asserts
//! that bob's brain is still absent from the same response. "Prune nothing"
//! would pass the structural half and lose the boundary.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use xerj_api::{router::build_es_compat_router, state::AppState};
use xerj_common::{config::Config, metrics::Metrics};
use xerj_engine::Engine;

const ADMIN_KEY: &str = "admin-secret-key-for-pruning-test";

/// Ordinary index names that are also structural keys in the metadata
/// responses a client polls. Each one broke a different endpoint.
const COLLIDING: &[&str] = &["status", "indices", "type", "nodes", "count"];

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

fn admin() -> String {
    format!("ApiKey {ADMIN_KEY}")
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn send_text(app: &axum::Router, uri: &str, auth: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", auth)
        .body(Body::empty())
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// A node holding: the scoped key's own index, one brain it must never learn
/// of, and one ordinary index per colliding structural key.
async fn seed(app: &axum::Router) -> String {
    let mut names: Vec<String> = vec!["logs-2026".into(), ".xerj-memory-bob-edges".into()];
    names.extend(COLLIDING.iter().map(|s| s.to_string()));
    for name in &names {
        let (status, body) = send(
            app,
            "PUT",
            &format!("/{name}/_doc/1?refresh=true"),
            &admin(),
            r#"{"host":"h1","amount":3}"#,
        )
        .await;
        assert!(status.is_success(), "seeding {name}: {body}");
    }
    let (status, minted) = send(
        app,
        "POST",
        "/_security/api_key",
        &admin(),
        r#"{"name":"logs-agent","role_descriptors":{"logs":{"indices":[
             {"names":["logs-2026"],"privileges":["read","write"]}]}}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "minting: {minted}");
    format!("ApiKey {}", minted["encoded"].as_str().expect("encoded"))
}

/// The boundary half, asserted alongside every structural assertion so
/// "prune nothing" cannot pass.
fn must_not_leak_the_brain(what: &str, body: &str) {
    assert!(
        !body.contains("bob"),
        "{what} enumerated a brain the caller may not read: {body}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────

/// `GET /_cluster/health` keeps `status` — the single most-polled field in the
/// API — with an ordinary index called `status` on the node.
#[tokio::test]
async fn cluster_health_keeps_its_status_field() {
    let (state, _dir) = auth_enabled_state();
    let app = build_es_compat_router(state);
    let agent = seed(&app).await;

    let (status, body) = send(&app, "GET", "/_cluster/health", &agent, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.get("status").is_some(),
        "_cluster/health lost its `status` field: {body}"
    );
    assert!(body.get("number_of_nodes").is_some(), "{body}");
    assert!(body.get("active_shards").is_some(), "{body}");
    must_not_leak_the_brain("_cluster/health", &body.to_string());

    // With the per-index breakdown, the breakdown itself is still filtered:
    // the caller's own index is there and nothing else is.
    let (status, body) = send(&app, "GET", "/_cluster/health?level=indices", &agent, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.get("status").is_some(), "level=indices lost `status`");
    let indices = body["indices"].as_object().expect("indices map");
    assert!(
        indices.contains_key("logs-2026"),
        "own index missing: {body}"
    );
    for name in COLLIDING {
        assert!(
            !indices.contains_key(*name),
            "unreadable index `{name}` is still enumerated: {body}"
        );
    }
    must_not_leak_the_brain("_cluster/health?level=indices", &body.to_string());
}

/// `GET /_cluster/stats` keeps its whole `indices` section with an ordinary
/// index called `indices` on the node. That section is aggregate counters, not
/// a map keyed by index name, so there is nothing there to prune.
#[tokio::test]
async fn cluster_stats_keeps_its_indices_section() {
    let (state, _dir) = auth_enabled_state();
    let app = build_es_compat_router(state);
    let agent = seed(&app).await;

    let (status, body) = send(&app, "GET", "/_cluster/stats", &agent, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.get("indices").is_some(),
        "_cluster/stats lost its entire `indices` section: {body}"
    );
    assert!(
        body["indices"].get("count").is_some(),
        "_cluster/stats lost `indices.count`: {body}"
    );
    assert!(
        body["indices"].get("docs").is_some(),
        "_cluster/stats lost `indices.docs`: {body}"
    );
    assert!(body.get("status").is_some(), "lost `status`: {body}");
    assert!(
        body["nodes"].get("count").is_some(),
        "_cluster/stats lost `nodes.count`: {body}"
    );
    must_not_leak_the_brain("_cluster/stats", &body.to_string());
}

/// Global `GET /_mapping` keeps every field's `type` with an ordinary index
/// called `type` on the node. This is the endpoint Kibana reads to build an
/// index pattern; without `type` every field is undefined.
#[tokio::test]
async fn global_mapping_keeps_every_field_type() {
    let (state, _dir) = auth_enabled_state();
    let app = build_es_compat_router(state);
    let agent = seed(&app).await;

    let (status, body) = send(&app, "GET", "/_mapping", &agent, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let own = body
        .get("logs-2026")
        .unwrap_or_else(|| panic!("the caller's own index is missing from _mapping: {body}"));
    let props = own["mappings"]["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("_mapping lost `mappings.properties`: {body}"));
    assert!(!props.is_empty(), "_mapping returned no fields: {body}");
    for (field, spec) in props {
        assert!(
            spec.get("type").is_some(),
            "field `{field}` lost its `type` — Kibana cannot build a pattern \
             from this: {body}"
        );
    }

    // …and the enumeration boundary still holds.
    for name in COLLIDING {
        assert!(
            body.get(*name).is_none(),
            "unreadable index `{name}` is still enumerated by _mapping: {body}"
        );
    }
    must_not_leak_the_brain("_mapping", &body.to_string());
}

/// `_cat/indices` keeps every row whose index column the caller can read, and
/// drops exactly the rows it cannot — not every row that happens to contain a
/// colliding token.
#[tokio::test]
async fn cat_indices_keeps_the_rows_it_should() {
    let (state, _dir) = auth_enabled_state();
    let app = build_es_compat_router(state);
    let agent = seed(&app).await;

    let (status, table) = send_text(&app, "/_cat/indices", &agent).await;
    assert_eq!(status, StatusCode::OK, "{table}");
    assert!(
        table.contains("logs-2026"),
        "_cat/indices lost the caller's own index: {table}"
    );
    assert_eq!(
        table.lines().filter(|l| !l.trim().is_empty()).count(),
        1,
        "_cat/indices should list exactly the one readable index: {table}"
    );
    must_not_leak_the_brain("_cat/indices", &table);

    // `_cat/health` has no index column at all, so it must survive whole even
    // though `status` and `count` are unreadable index names on this node.
    let (status, health) = send_text(&app, "/_cat/health", &agent).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !health.trim().is_empty(),
        "_cat/health was emptied by index-name matching: {health:?}"
    );
}

/// `GET /_settings` and `GET /_alias` are the same top-level index-keyed shape
/// and must behave the same way.
#[tokio::test]
async fn settings_and_alias_listings_stay_intact() {
    let (state, _dir) = auth_enabled_state();
    let app = build_es_compat_router(state);
    let agent = seed(&app).await;

    for uri in ["/_settings", "/_alias"] {
        let (status, body) = send(&app, "GET", uri, &agent, "").await;
        assert_eq!(status, StatusCode::OK, "GET {uri}: {body}");
        assert!(
            body.get("logs-2026").is_some(),
            "GET {uri} lost the caller's own index: {body}"
        );
        for name in COLLIDING {
            assert!(
                body.get(*name).is_none(),
                "GET {uri} still enumerates `{name}`: {body}"
            );
        }
        must_not_leak_the_brain(uri, &body.to_string());
    }

    // `_settings` keeps the settings object under the index it did return.
    let (_, body) = send(&app, "GET", "/_settings", &agent, "").await;
    assert!(
        body["logs-2026"].get("settings").is_some(),
        "_settings lost its `settings` block: {body}"
    );
}

/// `_field_caps` names indices in two places — the top-level list and one per
/// field/type entry — and neither may take the `fields` map down with it.
#[tokio::test]
async fn field_caps_keeps_its_fields_map() {
    let (state, _dir) = auth_enabled_state();
    let app = build_es_compat_router(state);
    let agent = seed(&app).await;

    let (status, body) = send(&app, "GET", "/_field_caps?fields=*", &agent, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.get("fields").is_some(),
        "_field_caps lost its `fields` map: {body}"
    );
    let listed = body["indices"].as_array().expect("indices array");
    assert!(
        listed.iter().any(|v| v == "logs-2026"),
        "_field_caps lost the caller's own index: {body}"
    );
    for name in COLLIDING {
        assert!(
            !listed.iter().any(|v| v == name),
            "_field_caps still enumerates `{name}`: {body}"
        );
    }
    must_not_leak_the_brain("_field_caps", &body.to_string());
}

/// FINDING D. An alias `add` whose index is a wildcard that matches nothing
/// used to answer `acknowledged: true` and leave an alias pointing at the
/// pattern TEXT — a write that never happened, reported as one that did, on
/// the reserved namespace of all places.
#[tokio::test]
async fn a_wildcard_alias_that_matches_nothing_is_refused() {
    let (state, _dir) = auth_enabled_state();
    let app = build_es_compat_router(state);
    seed(&app).await;

    // `POST /_aliases` with an `add` naming a reserved wildcard.
    let (status, body) = send(
        &app,
        "POST",
        "/_aliases",
        &admin(),
        r#"{"actions":[{"add":{"index":".xerj-memory-nope-*","alias":"X"}}]}"#,
    )
    .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "a wildcard alias add that matched nothing was acknowledged: {body}"
    );

    // `PUT /{wildcard}/_alias/{name}`, the other spelling.
    let (status, body) = send(&app, "PUT", "/.xerj-memory-nope-*/_alias/Y", &admin(), "{}").await;
    assert_ne!(
        status,
        StatusCode::OK,
        "PUT /{{wildcard}}/_alias was acknowledged: {body}"
    );

    // Neither junk alias exists.
    let (_, aliases) = send(&app, "GET", "/_aliases", &admin(), "").await;
    let text = aliases.to_string();
    assert!(!text.contains("\"X\""), "junk alias X was created: {text}");
    assert!(!text.contains("\"Y\""), "junk alias Y was created: {text}");

    // A wildcard that DOES match still works, on both spellings — the fix is
    // "refuse a write that did not happen", not "refuse wildcards".
    let (status, body) = send(
        &app,
        "POST",
        "/_aliases",
        &admin(),
        r#"{"actions":[{"add":{"index":"logs-*","alias":"live"}}]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a matching wildcard add: {body}");
    let (status, body) = send(&app, "PUT", "/logs-*/_alias/live2", &admin(), "{}").await;
    assert_eq!(status, StatusCode::OK, "a matching wildcard PUT: {body}");

    // Both resolve to the real index, not to the pattern text.
    let (_, resolved) = send(&app, "GET", "/live/_search", &admin(), "{}").await;
    assert!(
        resolved.get("hits").is_some(),
        "the alias does not resolve to a real index: {resolved}"
    );
    let (_, aliases) = send(&app, "GET", "/_aliases", &admin(), "").await;
    assert!(
        aliases["logs-2026"]["aliases"].get("live").is_some(),
        "the alias was not attached to the resolved index: {aliases}"
    );
    assert!(
        aliases["logs-2026"]["aliases"].get("live2").is_some(),
        "PUT /{{pattern}}/_alias did not attach to the resolved index: {aliases}"
    );

    // A concrete name is unchanged: ES lets an alias be attached before the
    // index exists, and clients rely on that ordering.
    let (status, body) = send(
        &app,
        "POST",
        "/_aliases",
        &admin(),
        r#"{"actions":[{"add":{"index":"not-created-yet","alias":"early"}}]}"#,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a concrete-name alias add must still be accepted: {body}"
    );
}
