//! Issue #450, from the outside: `_delete_by_query` and `_update_by_query`
//! through a multi-index alias touched only the alias's first member and
//! reported a complete success.
//!
//! `POST /tri/_delete_by_query {"match_all":{}}` over a three-member alias
//! holding thirty documents answered `HTTP 200 {"total":10,"deleted":10,
//! "failures":[]}`. Nothing in that body distinguishes it from a correct run:
//! the counts are internally consistent, there is no partial-failure flag, and
//! twenty documents were never considered. Both handlers began with
//! `Engine::get_index` on the raw path segment, and `get_index` resolves an
//! alias to `aliased.first()` — one member, silently.
//!
//! This is the read truncation of #433 on a destructive path, and it is
//! strictly worse: a short read is recoverable by reading again, a short delete
//! leaves the caller believing the operation finished.
//!
//! The assertions here are on ground truth after the call — what survives in
//! each concrete index — not on the reported counts, because the reported
//! counts were exactly what made the defect invisible.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn json_req(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    let body = if body.is_null() {
        Body::empty()
    } else {
        req = req.header("content-type", "application/json");
        Body::from(body.to_string())
    };
    let response = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, v)
}

/// Three indices, ten documents each, all behind one alias.
async fn three_members(app: &axum::Router, alias: &str, filter: Option<Value>) {
    for m in ["idx-a", "idx-b", "idx-c"] {
        json_req(
            app,
            "PUT",
            &format!("/{m}"),
            json!({"mappings":{"properties":{
                "n":{"type":"integer"},"tag":{"type":"keyword"}}}}),
        )
        .await;
        for n in 1..=10 {
            json_req(
                app,
                "POST",
                &format!("/{m}/_doc/{m}-{n}"),
                json!({"n": n, "tag": m}),
            )
            .await;
        }
        // `_aliases` with a `filter` in the `add` action does not store the
        // filter; the per-index alias route does.
        let (st, _) = match &filter {
            Some(f) => {
                json_req(
                    app,
                    "PUT",
                    &format!("/{m}/_alias/{alias}"),
                    json!({"filter": f}),
                )
                .await
            }
            None => json_req(app, "PUT", &format!("/{m}/_alias/{alias}"), Value::Null).await,
        };
        assert!(st.is_success(), "alias {alias} on {m}: {st}");
    }
    json_req(app, "POST", "/idx-a,idx-b,idx-c/_refresh", Value::Null).await;
}

async fn count_of(app: &axum::Router, index: &str) -> u64 {
    let (_, v) = json_req(app, "GET", &format!("/{index}/_count"), Value::Null).await;
    v["count"].as_u64().expect("count")
}

#[tokio::test]
async fn delete_by_query_through_an_alias_deletes_from_every_member() {
    let (app, _dir) = app().await;
    three_members(&app, "tri", None).await;

    for m in ["idx-a", "idx-b", "idx-c"] {
        assert_eq!(count_of(&app, m).await, 10, "{m} should start with 10");
    }

    let (status, body) = json_req(
        &app,
        "POST",
        "/tri/_delete_by_query",
        json!({"query":{"match_all":{}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    json_req(&app, "POST", "/idx-a,idx-b,idx-c/_refresh", Value::Null).await;

    // Ground truth first: this is the assertion the old code failed.
    for m in ["idx-a", "idx-b", "idx-c"] {
        assert_eq!(
            count_of(&app, m).await,
            0,
            "{m} still holds documents — the alias resolved to one member. body: {body}"
        );
    }
    // And the report must describe the whole run, not one member's share of it.
    assert_eq!(body["total"], 30, "reported total. body: {body}");
    assert_eq!(body["deleted"], 30, "reported deleted. body: {body}");
    assert_eq!(
        body["failures"].as_array().map(Vec::len),
        Some(0),
        "no member should have failed. body: {body}"
    );
}

#[tokio::test]
async fn update_by_query_through_an_alias_updates_every_member() {
    let (app, _dir) = app().await;
    three_members(&app, "tri", None).await;

    let (status, body) = json_req(
        &app,
        "POST",
        "/tri/_update_by_query",
        json!({"query":{"match_all":{}},
               "script":{"source":"ctx._source.tag = \"touched\""}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    json_req(&app, "POST", "/idx-a,idx-b,idx-c/_refresh", Value::Null).await;

    for m in ["idx-a", "idx-b", "idx-c"] {
        let (_, v) = json_req(
            &app,
            "POST",
            &format!("/{m}/_count"),
            json!({"query":{"term":{"tag":"touched"}}}),
        )
        .await;
        assert_eq!(
            v["count"], 10,
            "{m} was not updated — the alias resolved to one member. body: {body}"
        );
    }
    assert_eq!(body["total"], 30, "reported total. body: {body}");
    assert_eq!(body["updated"], 30, "reported updated. body: {body}");
}

/// A filtered alias must delete only what it can see. Expanding the alias to
/// its members without carrying the filter would delete the whole corpus —
/// a worse bug than the one being fixed.
#[tokio::test]
async fn a_filtered_alias_deletes_only_the_documents_it_selects() {
    let (app, _dir) = app().await;
    three_members(&app, "lowtri", Some(json!({"range":{"n":{"lte":3}}}))).await;

    let (status, body) = json_req(
        &app,
        "POST",
        "/lowtri/_delete_by_query",
        json!({"query":{"match_all":{}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    json_req(&app, "POST", "/idx-a,idx-b,idx-c/_refresh", Value::Null).await;

    // 3 of 10 per index match `n <= 3`, so 9 go and 21 stay.
    assert_eq!(
        count_of(&app, "idx-a,idx-b,idx-c").await,
        21,
        "the alias filter was not applied — deleting through a filtered alias \
         must not reach documents the alias cannot see. body: {body}"
    );
    let (_, left) = json_req(
        &app,
        "POST",
        "/idx-a,idx-b,idx-c/_count",
        json!({"query":{"range":{"n":{"lte":3}}}}),
    )
    .await;
    assert_eq!(
        left["count"], 0,
        "documents the alias selected survived. body: {body}"
    );
    assert_eq!(body["deleted"], 9, "reported deleted. body: {body}");
}

/// The single-index path must be untouched: same counts, same `batches`, same
/// shape. Multi-member aggregation is only reached when the selector really
/// does resolve to more than one index.
#[tokio::test]
async fn a_plain_single_index_request_is_unchanged() {
    let (app, _dir) = app().await;
    three_members(&app, "tri", None).await;

    let (status, body) = json_req(
        &app,
        "POST",
        "/idx-b/_delete_by_query",
        json!({"query":{"term":{"n":5}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["total"], 1, "body: {body}");
    assert_eq!(body["deleted"], 1, "body: {body}");
    assert_eq!(body["batches"], 1, "body: {body}");

    json_req(&app, "POST", "/idx-a,idx-b,idx-c/_refresh", Value::Null).await;
    assert_eq!(count_of(&app, "idx-a").await, 10, "untouched index");
    assert_eq!(count_of(&app, "idx-b").await, 9, "one document removed");
    assert_eq!(count_of(&app, "idx-c").await, 10, "untouched index");
}

/// An unknown selector still 404s rather than silently deleting nothing and
/// reporting success — the failure mode this whole issue is about.
#[tokio::test]
async fn an_unknown_index_is_still_a_404() {
    let (app, _dir) = app().await;
    three_members(&app, "tri", None).await;

    let (status, body) = json_req(
        &app,
        "POST",
        "/no-such-index/_delete_by_query",
        json!({"query":{"match_all":{}}}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
}
