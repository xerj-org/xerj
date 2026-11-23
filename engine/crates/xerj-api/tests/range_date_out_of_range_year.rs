//! A `range` bound whose year is too large for epoch milliseconds is an
//! `illegal_argument_exception`, as in Elasticsearch 8.18.
//!
//! Non-strict `date_optional_time` takes a year of up to nine digits, so
//! `lte: 522000000` parses as the year 522000000. That instant does not fit a
//! long count of milliseconds, and ES answers 400 with root cause
//! `illegal_argument_exception`: `date[...] is too far in the future to be
//! represented in a long milliseconds variable` (or `in the past` for a year
//! before now). XERJ read only four year digits and reported the value as
//! unparseable (`parse_exception`). A value that does not parse at all keeps
//! the `parse_exception` answer.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
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

async fn send(app: &axum::Router, method: &str, path: &str, body: &str) -> (StatusCode, Value) {
    let content_type = if path.contains("_bulk") {
        "application/x-ndjson"
    } else {
        "application/json"
    };
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", content_type)
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

/// The `dates` index of the YAML suite's `search/500_date_range.yml`.
async fn seeded() -> (axum::Router, tempfile::TempDir) {
    let (app, dir) = app().await;
    let mapping = r#"{"mappings":{"properties":{"date":{"type":"date"}}}}"#;
    let (status, got) = send(&app, "PUT", "/dates", mapping).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let bulk = "{\"index\":{\"_index\":\"dates\",\"_id\":\"1\"}}\n\
                {\"date\":\"1900-01-01T12:12:12.123456789Z\",\"field\":1}\n\
                {\"index\":{\"_index\":\"dates\",\"_id\":\"2\"}}\n\
                {\"date\":\"2022-01-01T12:12:12.123456789Z\",\"field\":2}\n\
                {\"index\":{\"_index\":\"dates\",\"_id\":\"3\"}}\n\
                {\"date\":\"2022-01-03T12:12:12.123456789Z\",\"field\":3}\n\
                {\"index\":{\"_index\":\"dates\",\"_id\":\"4\"}}\n\
                {\"date\":\"1500-01-01T12:12:12.123456789Z\",\"field\":4}\n";
    let (status, got) = send(&app, "POST", "/_bulk?refresh=true", bulk).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["errors"], false, "{got}");
    (app, dir)
}

async fn range(app: &axum::Router, bounds: Value) -> (StatusCode, Value) {
    range_in(app, bounds, "date_optional_time").await
}

async fn range_in(app: &axum::Router, bounds: Value, format: &str) -> (StatusCode, Value) {
    let mut date = bounds;
    date["format"] = json!(format);
    let body = json!({"sort": "field", "query": {"range": {"date": date}}});
    send(
        app,
        "POST",
        "/dates/_search?allow_partial_search_results=false",
        &body.to_string(),
    )
    .await
}

fn assert_too_far(status: StatusCode, got: &Value, direction: &str, what: &str) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{what}: {got}");
    assert_eq!(got["status"], 400, "{what}: {got}");
    let root = &got["error"]["root_cause"][0];
    assert_eq!(root["type"], "illegal_argument_exception", "{what}: {got}");
    let reason = root["reason"].as_str().unwrap_or_default();
    let tail =
        format!("is too far in the {direction} to be represented in a long milliseconds variable");
    assert!(
        reason.starts_with("date[") && reason.ends_with(&tail),
        "{what}: {got}"
    );
}

#[tokio::test]
async fn a_year_too_far_in_the_future_is_an_illegal_argument() {
    let (app, _dir) = seeded().await;
    for (what, lte) in [("number", json!(522000000)), ("string", json!("522000000"))] {
        let (status, got) = range(&app, json!({"gte": 2020, "lte": lte})).await;
        assert_too_far(status, &got, "future", what);
    }
}

#[tokio::test]
async fn a_year_too_far_in_the_past_is_an_illegal_argument() {
    let (app, _dir) = seeded().await;
    for (what, gte) in [
        ("number", json!(-522000000)),
        ("string", json!("-522000000")),
    ] {
        let (status, got) = range(&app, json!({"gte": gte, "lte": 2023})).await;
        assert_too_far(status, &got, "past", what);
    }
}

#[tokio::test]
async fn an_unparseable_bound_is_still_a_parse_exception() {
    let (app, _dir) = seeded().await;
    let (status, got) = range(&app, json!({"gte": "bogus", "lte": 2023})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{got}");
    let root = &got["error"]["root_cause"][0];
    assert_eq!(root["type"], "parse_exception", "{got}");
    assert_eq!(
        root["reason"],
        "failed to parse date field [bogus] with format [date_optional_time]: \
         [failed to parse date field [bogus] with format [date_optional_time]]",
        "{got}"
    );
}

#[tokio::test]
async fn the_strict_format_keeps_the_parse_exception() {
    // strict_date_optional_time takes exactly four year digits in ES too, so
    // 522000000 does not parse there at all.
    let (app, _dir) = seeded().await;
    let format = "strict_date_optional_time";
    let (status, got) = range_in(&app, json!({"lte": "522000000"}), format).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{got}");
    let root = &got["error"]["root_cause"][0];
    assert_eq!(root["type"], "parse_exception", "{got}");
}

#[tokio::test]
async fn the_year_reading_wins_when_its_format_comes_first() {
    // ES tries the formats in order: date_optional_time reads the year
    // before epoch_millis could read 522000000 as a 1970 instant.
    let (app, _dir) = seeded().await;
    let format = "date_optional_time||epoch_millis";
    let (status, got) = range_in(&app, json!({"lte": "522000000"}), format).await;
    assert_too_far(status, &got, "future", "date_optional_time||epoch_millis");
}

#[tokio::test]
async fn date_math_on_such_a_year_is_an_illegal_argument() {
    let (app, _dir) = seeded().await;
    let (status, got) = range(&app, json!({"lte": "522000000||+1d"})).await;
    assert_too_far(status, &got, "future", "522000000||+1d");
}

#[tokio::test]
async fn four_digit_years_still_match() {
    let (app, _dir) = seeded().await;
    let (status, got) = range(&app, json!({"gte": 1500, "lte": 2000})).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let ids: Vec<&str> = got["hits"]["hits"]
        .as_array()
        .expect("hits")
        .iter()
        .filter_map(|h| h["_id"].as_str())
        .collect();
    assert_eq!(ids, ["1", "4"], "{got}");
}
