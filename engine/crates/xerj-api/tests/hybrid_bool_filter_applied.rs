//! Issue #943: filters wrapped AROUND a `hybrid` query were accepted and
//! wrong over the ES wire.
//!
//! * `bool { must: { hybrid … }, filter: […] }` → 200 with **0 hits** (the
//!   engine's generic doc matcher has no Hybrid arm, so the must clause
//!   matched nothing).
//! * top-level `post_filter` beside `hybrid` → 200 with the **unfiltered**
//!   hits (`post_filter` never reaches any executor — accepted-and-ignored,
//!   the #204 class).
//!
//! The fix: the engine peels `bool{must: hybrid, filter}` and pushes the
//! filter into every leg (exactly the "filter inside each leg" spelling the
//! issue verified as correct), and the API layer rejects `post_filter` beside
//! a hybrid query with a 400 naming the supported spellings — mirroring the
//! `aggs`-beside-hybrid 400 that already existed.
//!
//! Elasticsearch is referenced for wire semantics only; no ES code is here.

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
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn hyb943_app_with_docs() -> (axum::Router, tempfile::TempDir) {
    let (app, dir) = app().await;
    let (st, b) = json_req(
        &app,
        "PUT",
        "/hy943",
        json!({ "mappings": { "properties": {
            "title": { "type": "text" },
            "body": { "type": "text" },
            "ax_format": { "type": "keyword" }
        } } }),
    )
    .await;
    assert!(st.is_success(), "create index: {st} {b}");
    for (id, title, body, ax_format) in [
        ("d1", "alpha beta", "gamma delta", "eml"),
        ("d2", "gamma delta", "alpha beta", "pdf"),
        ("d3", "epsilon zeta", "eta theta", "txt"),
        ("d4", "alpha zeta", "alpha theta", "eml"),
    ] {
        let (st, b) = json_req(
            &app,
            "POST",
            &format!("/hy943/_doc/{id}"),
            json!({ "title": title, "body": body, "ax_format": ax_format }),
        )
        .await;
        assert!(st.is_success(), "index {id}: {st} {b}");
    }
    let (_s, _b) = json_req(&app, "POST", "/hy943/_refresh", json!({})).await;
    (app, dir)
}

fn hyb943_legs() -> Value {
    json!([
        { "query": { "match": { "title": { "query": "alpha beta", "operator": "and" } } },
           "weight": 1.0 },
        { "query": { "match": { "body": { "query": "alpha beta", "operator": "and" } } },
           "weight": 0.8 }
    ])
}

fn hit_ids_and_scores(body: &Value) -> Vec<(String, f64)> {
    body.pointer("/hits/hits")
        .and_then(Value::as_array)
        .map(|hits| {
            hits.iter()
                .filter_map(|h| {
                    h.get("_id")
                        .and_then(Value::as_str)
                        .map(String::from)
                        .zip(h.get("_score").and_then(Value::as_f64))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// #943 shape A over HTTP: `bool{must: hybrid, filter}` must return the
/// filtered hits (pre-fix: 200 with `hits.total.value == 0`), and must equal
/// the verified-correct "filter inside each leg" spelling on ids AND fused
/// scores.
#[tokio::test]
async fn bool_must_hybrid_with_filter_applies_the_filter() {
    let (app, _dir) = hyb943_app_with_docs().await;

    let hybrid = json!({
        "hybrid": { "queries": hyb943_legs(), "fusion": { "type": "rrf", "k": 60 } }
    });

    let (status, body) = json_req(
        &app,
        "POST",
        "/hy943/_search",
        json!({ "size": 10, "query": {
            "bool": {
                "must": hybrid,
                "filter": [ { "term": { "ax_format": "eml" } } ]
            }
        } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "_search status: {status} {body}");
    let total = body
        .pointer("/hits/total/value")
        .and_then(Value::as_u64)
        .expect("hits.total.value");
    assert!(
        total > 0,
        "#943: bool{{must: hybrid, filter}} returned 0 hits over HTTP: {body}"
    );
    let shape_a = hit_ids_and_scores(&body);
    assert!(
        shape_a.iter().all(|(id, _)| id == "d1"),
        "only the eml doc that matches a leg may survive: {shape_a:?}"
    );

    // Spelling B: the same filter inside each leg — same ids, same scores.
    let legs_eml: Vec<Value> = hyb943_legs()
        .as_array()
        .unwrap()
        .iter()
        .map(|leg| {
            json!({ "query": {
                "bool": {
                    "must": [leg["query"].clone()],
                    "filter": [ { "term": { "ax_format": "eml" } } ]
                }
            }, "weight": leg["weight"] })
        })
        .collect();
    let (status_b, body_b) = json_req(
        &app,
        "POST",
        "/hy943/_search",
        json!({ "size": 10, "query": {
            "hybrid": { "queries": legs_eml, "fusion": { "type": "rrf", "k": 60 } }
        } }),
    )
    .await;
    assert_eq!(
        status_b,
        StatusCode::OK,
        "spelling B status: {status_b} {body_b}"
    );
    assert_eq!(
        shape_a,
        hit_ids_and_scores(&body_b),
        "#943: shape A must equal the filter-inside-each-leg spelling"
    );
}

/// #943 shape C: `post_filter` beside a `hybrid` query was silently ignored
/// (the unfiltered hits came back). It must now be a 400 naming the supported
/// spellings, mirroring the existing aggs-beside-hybrid rejection.
#[tokio::test]
async fn post_filter_beside_hybrid_is_a_400_naming_the_spellings() {
    let (app, _dir) = hyb943_app_with_docs().await;

    let (status, body) = json_req(
        &app,
        "POST",
        "/hy943/_search",
        json!({
            "size": 4,
            "post_filter": { "term": { "ax_format": "eml" } },
            "query": { "hybrid": {
                "queries": hyb943_legs(),
                "fusion": { "type": "rrf", "k": 60 }
            } }
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "#943: post_filter beside hybrid must fail loud (400), not answer with \
         the unfiltered hits: {status} {body}"
    );
    let reason = body
        .pointer("/error/reason")
        .or_else(|| body.pointer("/error/root_cause/0/reason"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        reason.contains("post_filter") && reason.contains("hybrid"),
        "#943: the 400 must name the key and the supported spellings: {body}"
    );
}

/// Guard against over-rejecting: `post_filter` beside a PLAIN query is a 200
/// — the 400 fires only when the query contains a hybrid — and, since #2400,
/// it is applied: only the two eml docs come back.
#[tokio::test]
async fn post_filter_beside_a_plain_match_is_accepted_and_applied() {
    let (app, _dir) = hyb943_app_with_docs().await;

    let (status, body) = json_req(
        &app,
        "POST",
        "/hy943/_search",
        json!({
            "size": 10,
            "post_filter": { "term": { "ax_format": "eml" } },
            "query": { "match_all": {} }
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "post_filter beside a plain query must stay 200: {body}"
    );
    let ids: Vec<String> = body
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .map(|hits| {
            hits.iter()
                .filter_map(|h| h.get("_id").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        ids.len(),
        2,
        "post_filter must narrow the plain query's hits to the two eml docs (#2400): {ids:?}"
    );
}
