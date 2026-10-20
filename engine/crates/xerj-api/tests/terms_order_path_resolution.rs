//! Issues #1282 and #1283: how a `terms` aggregation's `order` resolves a
//! sub-aggregation path.
//!
//! - #1282: ordering by a value of a multi-value metric — `"p.95"` or
//!   `"p[95.0]"` for a `percentiles` sub-agg — was silently ignored, so the
//!   buckets came back key-ascending. ES 8.13.4 orders by the 95th percentile.
//! - #1283: an order path naming an aggregation the request does not define
//!   (`"nope"`) returned HTTP 200 with the order dropped. ES 8.13.4 rejects
//!   it with a 400: `Invalid aggregation order path [nope]. Cannot find
//!   aggregation named [nope]`.
//!
//! Elasticsearch is referenced for wire semantics only; no ES code is
//! reproduced here.

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

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    ctype: &str,
    body: String,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if !body.is_empty() {
        req = req.header("content-type", ctype);
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The issue's 4-document fixture: hosts a (p95 10), b (900), c (59.5, 2 docs).
async fn seeded() -> (axum::Router, tempfile::TempDir) {
    let (app, dir) = app().await;
    let (st, body) = send(
        &app,
        "PUT",
        "/repro",
        "application/json",
        json!({"mappings": {"properties": {
            "host": {"type": "keyword"},
            "ms": {"type": "float"}
        }}})
        .to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create index: {body}");
    let mut bulk = String::new();
    for (id, host, ms) in [
        ("1", "a", 10),
        ("2", "c", 60),
        ("3", "b", 900),
        ("4", "c", 50),
    ] {
        bulk.push_str(&format!(
            "{{\"index\":{{\"_index\":\"repro\",\"_id\":\"{id}\"}}}}\n"
        ));
        bulk.push_str(&format!("{{\"host\":\"{host}\",\"ms\":{ms}}}\n"));
    }
    let (st, body) = send(
        &app,
        "POST",
        "/_bulk?refresh=true",
        "application/x-ndjson",
        bulk,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "bulk: {body}");
    assert_eq!(body["errors"], json!(false), "bulk: {body}");
    (app, dir)
}

fn keys(resp: &Value) -> Vec<String> {
    resp["aggregations"]["k"]["buckets"]
        .as_array()
        .unwrap_or_else(|| panic!("no buckets in {resp}"))
        .iter()
        .map(|b| b["key"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn percentile_order_body(path: &str, dir: &str) -> String {
    json!({
        "size": 0,
        "aggs": {"k": {
            "terms": {"field": "host", "order": {path: dir}},
            "aggs": {"p": {"percentiles": {"field": "ms", "percents": [95]}}}
        }}
    })
    .to_string()
}

#[tokio::test]
async fn terms_orders_by_a_percentile_value() {
    let (app, _dir) = seeded().await;
    for path in ["p.95", "p[95.0]", "p[95]"] {
        let (st, resp) = send(
            &app,
            "POST",
            "/repro/_search",
            "application/json",
            percentile_order_body(path, "desc"),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{path}: {resp}");
        assert_eq!(keys(&resp), ["b", "c", "a"], "{path} desc: {resp}");

        let (st, resp) = send(
            &app,
            "POST",
            "/repro/_search",
            "application/json",
            percentile_order_body(path, "asc"),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{path}: {resp}");
        assert_eq!(keys(&resp), ["a", "c", "b"], "{path} asc: {resp}");
    }
}

/// Single-value metric ordering already worked; it must keep working.
#[tokio::test]
async fn terms_orders_by_a_single_value_metric() {
    let (app, _dir) = seeded().await;
    let body = json!({
        "size": 0,
        "aggs": {"k": {
            "terms": {"field": "host", "order": {"m": "desc"}},
            "aggs": {"m": {"max": {"field": "ms"}}}
        }}
    })
    .to_string();
    let (st, resp) = send(&app, "POST", "/repro/_search", "application/json", body).await;
    assert_eq!(st, StatusCode::OK, "{resp}");
    assert_eq!(keys(&resp), ["b", "c", "a"], "{resp}");
}

fn unknown_order_body(path: &str, with_sub: bool) -> Value {
    let mut terms = json!({"k": {"terms": {"field": "host", "order": {path: "desc"}}}});
    if with_sub {
        terms["k"]["aggs"] = json!({"m": {"max": {"field": "ms"}}});
    }
    json!({"size": 0, "aggs": terms})
}

#[tokio::test]
async fn terms_order_on_an_unknown_aggregation_is_a_400_on_search() {
    let (app, _dir) = seeded().await;
    // Reasons verified byte-for-byte against ES 8.13.4 for these four shapes.
    for (path, with_sub, missing) in [
        ("nope", false, "nope"),
        ("nope", true, "nope"),
        ("nope.95", true, "nope"),
        ("nope[95.0]", true, "nope"),
        ("_term", false, "_term"),
    ] {
        let body = unknown_order_body(path, with_sub);
        let (st, resp) = send(
            &app,
            "POST",
            "/repro/_search",
            "application/json",
            body.to_string(),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{path}: {resp}");
        let reason = resp.to_string();
        assert!(
            reason.contains(&format!(
                "Invalid aggregation order path [{path}]. Cannot find aggregation named [{missing}]"
            )),
            "{path}: reason must name the path and the missing aggregation: {resp}"
        );
    }
}

/// A chain through a metric (`m>nope`): ES 8.13.4 answers 400 with a
/// different reason ("Metrics aggregations cannot have sub-aggregations"), so
/// only the status and the path are pinned here.
#[tokio::test]
async fn order_path_chained_past_a_metric_is_a_400() {
    let (app, _dir) = seeded().await;
    let body = unknown_order_body("m>nope", true);
    let (st, resp) = send(
        &app,
        "POST",
        "/repro/_search",
        "application/json",
        body.to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{resp}");
    assert!(
        resp.to_string()
            .contains("Invalid aggregation order path [m>nope]"),
        "{resp}"
    );
}

/// `_count` / `_key` are built-in order keys, never sub-aggregation names.
#[tokio::test]
async fn builtin_order_keys_stay_valid() {
    let (app, _dir) = seeded().await;
    for path in ["_count", "_key"] {
        let body = unknown_order_body(path, false);
        let (st, resp) = send(
            &app,
            "POST",
            "/repro/_search",
            "application/json",
            body.to_string(),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{path}: {resp}");
    }
}

/// Nested terms: the check applies at every level, against that level's own
/// sub-aggregations.
#[tokio::test]
async fn unknown_order_path_in_a_nested_terms_is_a_400() {
    let (app, _dir) = seeded().await;
    let body = json!({
        "size": 0,
        "aggs": {"outer": {
            "terms": {"field": "host"},
            "aggs": {
                "m": {"max": {"field": "ms"}},
                "inner": {"terms": {"field": "host", "order": {"m": "desc"}}}
            }
        }}
    });
    let (st, resp) = send(
        &app,
        "POST",
        "/repro/_search",
        "application/json",
        body.to_string(),
    )
    .await;
    // ES 8.13.4: 400 `invalid_path`, "Invalid aggregation order path [m]. The
    // provided aggregation [m] either does not exist, ..." — status and path
    // pinned, not the wording.
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "inner order names the outer's sibling: {resp}"
    );
    assert!(
        resp.to_string()
            .contains("Invalid aggregation order path [m]"),
        "{resp}"
    );
}

/// `_msearch` fails only the offending item, with ES's per-item error shape.
#[tokio::test]
async fn unknown_order_path_fails_only_that_msearch_item() {
    let (app, _dir) = seeded().await;
    let bad = unknown_order_body("nope", false);
    let good = json!({"size": 0, "query": {"match_all": {}}});
    let ndjson = format!("{{\"index\":\"repro\"}}\n{bad}\n{{\"index\":\"repro\"}}\n{good}\n");
    let (st, resp) = send(&app, "POST", "/_msearch", "application/x-ndjson", ndjson).await;
    assert_eq!(st, StatusCode::OK, "{resp}");
    let items = resp["responses"].as_array().expect("responses");
    assert_eq!(items[0]["status"], 400, "bad item: {resp}");
    assert!(
        items[0]
            .to_string()
            .contains("Invalid aggregation order path [nope]"),
        "bad item reason: {resp}"
    );
    assert_eq!(items[1]["status"], 200, "good item still runs: {resp}");
}
