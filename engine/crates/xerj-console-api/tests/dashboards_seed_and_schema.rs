//! Dashboards: rich panel schema round-trip, first-launch seeding of the 13
//! built-ins as editable backend data, managed-default edit/delete rules, and
//! the bulk endpoint.
//!
//! These exercise the backend half of the "Kibana-quality dashboards"
//! rework: net-new dashboards persist at the backend, seeded defaults are
//! editable data (not code), and free-form per-panel geometry/query/viz
//! round-trips unchanged.

use axum::{body::Body, http::Request, Router};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;
use xerj_common::config::Config;
use xerj_console_api::{
    auth::{sessions, store},
    state::ClusterMode,
    xerj_console_router, ConsoleState,
};
use xerj_engine::Engine;

struct TestApp {
    router: Router,
    state: ConsoleState,
    engine: Engine,
    owner_cookie: String,
    dir: TempDir,
}

async fn boot() -> TestApp {
    let dir = TempDir::new().unwrap();
    let mut cfg = Config::default();
    cfg.server.data_dir = dir.path().to_str().unwrap().to_string();
    let engine = Engine::new(cfg).expect("engine");
    let outcome = xerj_console_api::bootstrap::run(&engine, dir.path(), "http://localhost:9200")
        .await
        .unwrap();
    let state = ConsoleState::new(
        engine.clone(),
        "local".into(),
        outcome.master_key,
        ClusterMode::Standalone,
    );

    let owner_cookie = cookie_for(&state, &engine, "owner-test", "owner").await;
    let router = xerj_console_router(state.clone());
    TestApp {
        router,
        state,
        engine,
        owner_cookie,
        dir,
    }
}

/// Create an active user with a role and mint a session cookie for them.
async fn cookie_for(state: &ConsoleState, engine: &Engine, id: &str, role: &str) -> String {
    let user = store::User {
        id: id.to_string(),
        email: format!("{id}@example.com"),
        display_name: id.to_string(),
        role: role.to_string(),
        status: store::UserStatus::Active,
        created_at: xerj_console_api::time::now_iso(),
        last_seen_at: Some(xerj_console_api::time::now_iso()),
    };
    store::upsert_user(engine, &user).await.unwrap();
    let (_s, signed) = sessions::mint_session(state, &user.id, "passkey", None, None)
        .await
        .unwrap();
    format!("xerj_session={signed}")
}

async fn body_json(resp: axum::response::Response) -> (axum::http::StatusCode, Value) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, v)
}

fn req(method: &str, path: &str, cookie: &str, body: Option<Value>) -> Request<Body> {
    let body = match body {
        Some(b) => Body::from(b.to_string()),
        None => Body::empty(),
    };
    Request::builder()
        .method(method)
        .uri(path)
        .header("cookie", cookie)
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

fn req_etag(method: &str, path: &str, cookie: &str, etag: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("cookie", cookie)
        .header("content-type", "application/json")
        .header("if-match", etag)
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn get_dash(app: &TestApp, id: &str) -> (axum::http::StatusCode, Value) {
    let r = app
        .router
        .clone()
        .oneshot(req(
            "GET",
            &format!("/_xerj-console/api/v1/dashboards/{id}"),
            &app.owner_cookie,
            None,
        ))
        .await
        .unwrap();
    body_json(r).await
}

async fn list_dash(app: &TestApp) -> Value {
    let r = app
        .router
        .clone()
        .oneshot(req(
            "GET",
            "/_xerj-console/api/v1/dashboards",
            &app.owner_cookie,
            None,
        ))
        .await
        .unwrap();
    body_json(r).await.1
}

// ─────────────────────────────────────────────────────────────────────────────
// Seeding
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn seeds_thirteen_editable_defaults_on_first_boot() {
    let app = boot().await;
    let body = list_dash(&app).await;
    let arr = body["data"]["dashboards"].as_array().unwrap();

    let defaults: Vec<&Value> = arr
        .iter()
        .filter(|d| d["id"].as_str().unwrap_or("").starts_with("default-"))
        .collect();
    assert_eq!(defaults.len(), 13, "must seed exactly 13 defaults: {body}");

    // Every registry dashboard is present by its stable id.
    for id in [
        "default-ai-overview",
        "default-rag-quality",
        "default-vector-index",
        "default-agent-memory",
        "default-logs-overview",
        "default-anomaly-detect",
        "default-ingest-pipeline",
        "default-system",
        "default-search-discover",
        "default-alerts",
        "default-data",
        "default-users",
        "default-settings",
    ] {
        assert!(
            defaults.iter().any(|d| d["id"] == id),
            "missing seeded default {id}"
        );
    }
}

#[tokio::test]
async fn seeded_default_is_managed_data_with_geometry() {
    let app = boot().await;
    let (status, body) = get_dash(&app, "default-ai-overview").await;
    assert_eq!(status, 200, "{body}");
    let d = &body["data"];
    assert_eq!(d["owner"], "system");
    assert_eq!(d["visibility"], "default");
    assert_eq!(d["managed"], true);
    assert_eq!(d["section"], "dashboards");
    assert_eq!(d["group"], "ai");
    assert_eq!(d["version"], 1);
    // ETag header + meta present.
    assert_eq!(body["meta"]["etag"], "W/\"1\"");

    let panels = d["panels"].as_array().unwrap();
    assert!(!panels.is_empty(), "seeded panels must be present");
    let first = &panels[0];
    assert_eq!(first["id"], "queries");
    assert_eq!(first["type"], "metric");
    assert_eq!(first["title"], "LLM QUERIES");
    // Free-form geometry (x/y/w/h), not a bare `cols`.
    for k in ["x", "y", "w", "h"] {
        assert!(first["layout"][k].is_u64(), "layout.{k} missing: {first}");
    }
    assert_eq!(first["layout"]["w"], 4);
    // Mock-backed seed panel: builtin provenance, null query.
    assert_eq!(first["builtin"], "ai-overview/queries");
    assert!(first["query"].is_null());

    // Duplicate `citations` id in rag-quality was disambiguated at seed time.
    let (_, rag) = get_dash(&app, "default-rag-quality").await;
    let ids: Vec<&str> = rag["data"]["panels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"citations"));
    assert!(ids.contains(&"citations-2"));
}

#[tokio::test]
async fn seeding_is_idempotent_and_preserves_edits() {
    let app = boot().await;

    // Edit a seeded default (as owner) — layout/title persist, version bumps.
    let patch = req_etag(
        "PATCH",
        "/_xerj-console/api/v1/dashboards/default-system",
        &app.owner_cookie,
        "W/\"1\"",
        json!({ "name": "System (edited)" }),
    );
    let (status, body) = body_json(app.router.clone().oneshot(patch).await.unwrap()).await;
    assert_eq!(
        status, 200,
        "owner must be able to edit a managed default: {body}"
    );
    assert_eq!(body["data"]["version"], 2);

    // Re-run bootstrap (simulates a restart) — seeding must NOT duplicate or
    // clobber.
    xerj_console_api::bootstrap::run(&app.engine, app.dir.path(), "http://localhost:9200")
        .await
        .unwrap();

    let body = list_dash(&app).await;
    let defaults: Vec<&Value> = body["data"]["dashboards"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["id"].as_str().unwrap_or("").starts_with("default-"))
        .collect();
    assert_eq!(defaults.len(), 13, "re-seed must not duplicate defaults");

    // The edit survived the re-seed.
    let (_, sys) = get_dash(&app, "default-system").await;
    assert_eq!(sys["data"]["name"], "System (edited)");
    assert_eq!(sys["data"]["version"], 2);
}

// ─────────────────────────────────────────────────────────────────────────────
// Managed-default write rules
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn managed_default_editable_by_owner_but_not_deletable() {
    let app = boot().await;

    // Replace (PUT) the whole managed default as owner — allowed.
    let put = req_etag(
        "PUT",
        "/_xerj-console/api/v1/dashboards/default-alerts",
        &app.owner_cookie,
        "W/\"1\"",
        json!({
            "name": "Alerts",
            "visibility": "default",
            "section": "alerts",
            "panels": [ { "id": "x", "type": "metric", "title": "X",
                          "layout": { "x": 0, "y": 0, "w": 4, "h": 2 } } ]
        }),
    );
    let (status, body) = body_json(app.router.clone().oneshot(put).await.unwrap()).await;
    assert_eq!(
        status, 200,
        "owner PUT of managed default must succeed: {body}"
    );
    assert_eq!(body["data"]["version"], 2);
    assert_eq!(body["data"]["managed"], true, "managed flag preserved");

    // DELETE of a managed default is forbidden even for the owner.
    let r = app
        .router
        .clone()
        .oneshot(req(
            "DELETE",
            "/_xerj-console/api/v1/dashboards/default-alerts",
            &app.owner_cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "managed defaults must not be deletable");
}

#[tokio::test]
async fn managed_default_not_editable_by_viewer() {
    let app = boot().await;
    let viewer = cookie_for(&app.state, &app.engine, "viewer-1", "viewer").await;
    let patch = req_etag(
        "PATCH",
        "/_xerj-console/api/v1/dashboards/default-system",
        &viewer,
        "W/\"1\"",
        json!({ "name": "nope" }),
    );
    let r = app.router.clone().oneshot(patch).await.unwrap();
    assert_eq!(r.status(), 403, "a viewer must not edit a managed default");
}

// ─────────────────────────────────────────────────────────────────────────────
// Rich panel schema round-trip
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn rich_panels_round_trip_byte_identical() {
    let app = boot().await;

    // A net-new dashboard with free-form geometry, a per-panel ES-DSL query,
    // per-panel viz config, a drilldown, and a static markdown panel.
    let panels = json!([
        {
            "id": "traffic",
            "type": "line",
            "title": "PROD API TRAFFIC",
            "layout": { "x": 0, "y": 0, "w": 8, "h": 4 },
            "query": {
                "index": "logs-*",
                "time_field": "@timestamp",
                "dsl": { "bool": { "must": [ { "match": { "service": "api" } } ] } },
                "aggs": { "over_time": { "date_histogram": { "field": "@timestamp", "fixed_interval": "1h" } } }
            },
            "viz": { "unit": "req/s", "value_field": "doc_count", "spark": true, "delta": true },
            "drilldown": { "to": "default-system", "filter_field": "host" },
            "builtin": Value::Null
        },
        {
            "id": "notes",
            "type": "markdown",
            "title": "RUNBOOK",
            "layout": { "x": 8, "y": 0, "w": 4, "h": 4 },
            "query": Value::Null,
            "viz": { "text": "## On call\nPage the SRE." },
            "drilldown": Value::Null,
            "builtin": Value::Null
        }
    ]);

    let create = req(
        "POST",
        "/_xerj-console/api/v1/dashboards",
        &app.owner_cookie,
        Some(json!({
            "name": "Prod API Traffic",
            "visibility": "private",
            "section": "dashboards",
            "group": "infra",
            "panels": panels,
            "filters_default": { "env": "prod" },
            "time_default": "24H"
        })),
    );
    let (status, body) = body_json(app.router.clone().oneshot(create).await.unwrap()).await;
    assert_eq!(status, 201, "{body}");
    let id = body["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        body["data"]["managed"], false,
        "user create is never managed"
    );
    assert_eq!(
        body["data"]["panels"], panels,
        "panels must persist verbatim"
    );

    // GET it back — panels + geometry + query + viz identical.
    let (status, got) = get_dash(&app, &id).await;
    assert_eq!(status, 200);
    assert_eq!(
        got["data"]["panels"], panels,
        "panels must round-trip identical"
    );
    assert_eq!(got["data"]["filters_default"], json!({ "env": "prod" }));
    assert_eq!(got["data"]["time_default"], "24H");
    assert_eq!(got["data"]["group"], "infra");

    // Replace the geometry (free resize/move) and confirm it sticks.
    let moved = json!([
        {
            "id": "traffic",
            "type": "line",
            "title": "PROD API TRAFFIC",
            "layout": { "x": 0, "y": 0, "w": 12, "h": 6 },
            "query": Value::Null,
            "viz": {},
            "drilldown": Value::Null,
            "builtin": Value::Null
        }
    ]);
    let put = req_etag(
        "PUT",
        &format!("/_xerj-console/api/v1/dashboards/{id}"),
        &app.owner_cookie,
        "W/\"1\"",
        json!({ "name": "Prod API Traffic", "visibility": "private", "panels": moved }),
    );
    let (status, body) = body_json(app.router.clone().oneshot(put).await.unwrap()).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["data"]["version"], 2);
    assert_eq!(body["data"]["panels"][0]["layout"]["w"], 12);
    assert_eq!(body["data"]["panels"][0]["layout"]["h"], 6);
}

// ─────────────────────────────────────────────────────────────────────────────
// Bulk endpoint
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn bulk_upsert_and_delete_as_owner() {
    let app = boot().await;

    // Create a user dashboard to delete via bulk.
    let (_, created) = body_json(
        app.router
            .clone()
            .oneshot(req(
                "POST",
                "/_xerj-console/api/v1/dashboards",
                &app.owner_cookie,
                Some(json!({ "name": "Doomed", "visibility": "private" })),
            ))
            .await
            .unwrap(),
    )
    .await;
    let doomed = created["data"]["id"].as_str().unwrap().to_string();

    // Bulk: upsert a full dashboard (with a fixed id) + delete the doomed one.
    let upsert_doc = json!({
        "id": "team-infra-board",
        "owner": "owner-test",
        "org_id": "default",
        "visibility": "shared",
        "managed": false,
        "name": "Team Infra",
        "section": "dashboards",
        "group": "infra",
        "cloned_from": Value::Null,
        "panels": [ { "id": "a", "type": "metric", "title": "A",
                      "layout": { "x": 0, "y": 0, "w": 4, "h": 2 } } ],
        "filters_default": {},
        "time_default": Value::Null,
        "version": 1,
        "created_at": "2026-07-13T00:00:00.000Z",
        "updated_at": "2026-07-13T00:00:00.000Z",
        "deleted_at": Value::Null
    });
    let bulk = req(
        "POST",
        "/_xerj-console/api/v1/dashboards/_bulk",
        &app.owner_cookie,
        Some(json!({ "upserts": [ upsert_doc ], "deletes": [ doomed ] })),
    );
    let (status, body) = body_json(app.router.clone().oneshot(bulk).await.unwrap()).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["data"]["upserted"], json!(["team-infra-board"]));
    assert_eq!(body["data"]["deleted"].as_array().unwrap().len(), 1);

    // The upsert is fetchable; the delete is gone.
    let (s1, _) = get_dash(&app, "team-infra-board").await;
    assert_eq!(s1, 200);
    let (s2, _) = get_dash(&app, &doomed).await;
    assert_eq!(s2, 404);
}

#[tokio::test]
async fn bulk_forbidden_for_non_admin() {
    let app = boot().await;
    let editor = cookie_for(&app.state, &app.engine, "editor-1", "editor").await;
    let r = app
        .router
        .clone()
        .oneshot(req(
            "POST",
            "/_xerj-console/api/v1/dashboards/_bulk",
            &editor,
            Some(json!({ "upserts": [], "deletes": [] })),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "bulk must require admin/owner");
}

#[tokio::test]
async fn bulk_route_matches_before_id_param() {
    // Guards the matchit static-vs-param sibling: `_bulk` must hit the bulk
    // handler (403 for a viewer), not the `:id` GET/PUT handler.
    let app = boot().await;
    let viewer = cookie_for(&app.state, &app.engine, "viewer-2", "viewer").await;
    let r = app
        .router
        .clone()
        .oneshot(req(
            "POST",
            "/_xerj-console/api/v1/dashboards/_bulk",
            &viewer,
            Some(json!({ "upserts": [], "deletes": [] })),
        ))
        .await
        .unwrap();
    // 403 (bulk authz), definitively not 404/405 from the :id route.
    assert_eq!(r.status(), 403);
}

// ─────────────────────────────────────────────────────────────────────────────
// Error envelope (contract-conformant code/message)
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn error_body_carries_contract_code_and_message() {
    let app = boot().await;
    let (status, body) = get_dash(&app, "does-not-exist").await;
    assert_eq!(status, 404);
    assert_eq!(body["error"]["code"], "not_found", "{body}");
    assert!(body["error"]["message"].is_string());
    // Legacy keys remain for older readers.
    assert_eq!(body["error"]["type"], "not_found");
}
