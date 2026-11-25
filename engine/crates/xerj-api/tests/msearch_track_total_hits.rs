//! `track_total_hits` in an `_msearch` item means what it means in `_search`
//! (`SearchSourceBuilder#trackTotalHitsUpTo`, ES 8.13.4).
//!
//! XERJ's query parser read every negative value as `true`, so `-1` (ES's
//! "tracking off") counted every match, and `-2`, which ES refuses, ran. ES
//! parses each item body before running any of them (`RestMultiSearchAction`
//! hands every body to `SearchSourceBuilder#parseXContent`, and
//! `MultiSearchRequest#readMultiLineFormat` catches nothing), so one bad value
//! refuses the whole request.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

async fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn send(app: &axum::Router, path: &str, body: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/x-ndjson")
        .body(Body::from(body.to_owned()))
        .expect("request");
    let response = app.clone().oneshot(req).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Three documents, so a capped total differs from the exact one.
async fn seeded() -> (axum::Router, tempfile::TempDir) {
    let (app, dir) = app().await;
    let bulk = "{\"index\":{\"_index\":\"mtt\",\"_id\":\"1\"}}\n{\"foo\":\"bar\"}\n\
                {\"index\":{\"_index\":\"mtt\",\"_id\":\"2\"}}\n{\"foo\":\"bar\"}\n\
                {\"index\":{\"_index\":\"mtt\",\"_id\":\"3\"}}\n{\"foo\":\"bar\"}\n";
    let (status, got) = send(&app, "/_bulk?refresh=true", bulk).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["errors"], false, "{got}");
    (app, dir)
}

/// One `_msearch` body: an `{"index":"mtt"}` header before each item.
fn items(bodies: &[&str]) -> String {
    bodies
        .iter()
        .map(|b| format!("{{\"index\":\"mtt\"}}\n{b}\n"))
        .collect()
}

#[tokio::test]
async fn a_track_total_hits_below_minus_one_refuses_the_whole_request() {
    let (app, _dir) = seeded().await;
    let reason = "[track_total_hits] parameter must be positive or equals to -1, got -2";
    for path in ["/_msearch", "/mtt/_msearch"] {
        let body = items(&[
            r#"{"query":{"match_all":{}}}"#,
            r#"{"track_total_hits":-2}"#,
        ]);
        let (status, got) = send(&app, path, &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {got}");
        assert_eq!(got["status"], 400, "{path}: {got}");
        assert_eq!(
            got["error"]["type"], "illegal_argument_exception",
            "{path}: {got}"
        );
        assert_eq!(got["error"]["reason"], reason, "{path}: {got}");
        assert_eq!(
            got["error"]["root_cause"][0]["reason"], reason,
            "{path}: {got}"
        );
    }
}

#[tokio::test]
async fn minus_one_turns_tracking_off_as_false_does() {
    let (app, _dir) = seeded().await;
    let body = items(&[
        r#"{"track_total_hits":-1}"#,
        r#"{"track_total_hits":false}"#,
    ]);
    let (status, got) = send(&app, "/_msearch", &body).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let responses = got["responses"].as_array().expect("responses");
    assert_eq!(responses.len(), 2, "{got}");
    for (n, item) in responses.iter().enumerate() {
        assert_eq!(item["status"], 200, "item {n}: {got}");
        assert!(
            item["hits"].get("total").is_none(),
            "item {n} has no total: {got}"
        );
        assert_eq!(
            item["hits"]["hits"].as_array().map(Vec::len),
            Some(3),
            "item {n}: {got}"
        );
    }
}

#[tokio::test]
async fn every_value_es_accepts_still_counts() {
    let (app, _dir) = seeded().await;
    let body = items(&[
        r#"{"query":{"match_all":{}}}"#,
        r#"{"track_total_hits":true}"#,
        r#"{"track_total_hits":2}"#,
    ]);
    let (status, got) = send(&app, "/_msearch", &body).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let responses = got["responses"].as_array().expect("responses");
    let totals: Vec<&Value> = responses.iter().map(|r| &r["hits"]["total"]).collect();
    assert_eq!(
        totals[0],
        &serde_json::json!({"value": 3, "relation": "eq"}),
        "{got}"
    );
    assert_eq!(
        totals[1],
        &serde_json::json!({"value": 3, "relation": "eq"}),
        "{got}"
    );
    assert_eq!(
        totals[2],
        &serde_json::json!({"value": 2, "relation": "gte"}),
        "{got}"
    );
}

/// The parser change reaches `_search` too: `-1` there now leaves the total
/// out, as `false` already did.
#[tokio::test]
async fn a_search_with_minus_one_has_no_total_either() {
    let (app, _dir) = seeded().await;
    for body in [
        r#"{"track_total_hits":-1}"#,
        r#"{"track_total_hits":false}"#,
    ] {
        let req = Request::builder()
            .method("POST")
            .uri("/mtt/_search")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .expect("request");
        let response = app.clone().oneshot(req).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK, "{body}");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let got: Value = serde_json::from_slice(&bytes).expect("json");
        assert!(got["hits"].get("total").is_none(), "{body}: {got}");
        assert_eq!(
            got["hits"]["hits"].as_array().map(Vec::len),
            Some(3),
            "{body}: {got}"
        );
    }
}

async fn post(app: &axum::Router, path: &str, ctype: &str, body: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", ctype)
        .body(Body::from(body.to_owned()))
        .expect("request");
    let response = app.clone().oneshot(req).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A search template renders its own response. With tracking off the engine
/// stops counting at `size`, so a total there would be an undercount labelled
/// `eq`; ES leaves it out, and so do both template paths now.
#[tokio::test]
async fn a_template_with_tracking_off_has_no_total() {
    let (app, _dir) = seeded().await;
    for tth in ["-1", "false"] {
        let source =
            format!(r#"{{"size":1,"track_total_hits":{tth},"query":{{"match_all":{{}}}}}}"#);
        let (status, got) = post(
            &app,
            "/mtt/_search/template",
            "application/json",
            &format!(r#"{{"source":{source}}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{tth}: {got}");
        assert!(
            got["hits"].get("total").is_none(),
            "_search/template {tth}: {got}"
        );
        assert_eq!(
            got["hits"]["hits"].as_array().map(Vec::len),
            Some(1),
            "{tth}: {got}"
        );

        let (status, got) = post(
            &app,
            "/mtt/_msearch/template",
            "application/x-ndjson",
            &format!("{{}}\n{{\"source\":{source}}}\n"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{tth}: {got}");
        let item = &got["responses"][0];
        assert!(item["hits"].is_object(), "_msearch/template {tth}: {got}");
        assert!(
            item["hits"].get("total").is_none(),
            "_msearch/template {tth}: {got}"
        );
    }
    // The same template with tracking on, run right after the two above,
    // still reports its exact total rather than the cached tracking-off
    // count (1 of 3). That needs the query cache to key the two modes apart;
    // `query_cache_track_total_hits.rs` covers it on its own.
    let (status, got) = post(
        &app,
        "/mtt/_search/template",
        "application/json",
        r#"{"source":{"size":1,"query":{"match_all":{}}}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got["hits"]["total"],
        serde_json::json!({"value": 3, "relation": "eq"}),
        "{got}"
    );
}

/// The pre-scan pairs lines exactly as the item loop does: a blank header
/// skips its pair, and a trailing header with no body is ignored, so neither
/// turns into a false refusal.
#[tokio::test]
async fn the_pre_scan_pairs_lines_as_the_item_loop_does() {
    let (app, _dir) = seeded().await;
    let body = "{\"index\":\"mtt\"}\n{\"track_total_hits\":2}\n{\"index\":\"mtt\"}\n";
    let (status, got) = send(&app, "/_msearch", body).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["responses"].as_array().map(Vec::len), Some(1), "{got}");

    // An aggregation survives the total being left out.
    let body = items(&[r#"{"track_total_hits":false,"aggs":{"n":{"filter":{"match_all":{}}}}}"#]);
    let (status, got) = send(&app, "/_msearch", &body).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let item = &got["responses"][0];
    assert!(item["hits"].get("total").is_none(), "{got}");
    assert!(item["aggregations"]["n"].is_object(), "{got}");
}
