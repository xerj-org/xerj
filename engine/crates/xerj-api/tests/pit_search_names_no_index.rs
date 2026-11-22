//! A point-in-time search takes its indices from the PIT, so ES 8.13 refuses
//! one that also names an index: `POST /test/_search {"pit":{...}}`, and an
//! `_msearch` item whose header (or the `/{index}/_msearch` path) names one.
//! The answer is a 400 `action_request_validation_exception` for the whole
//! request. XERJ searched the PIT's indices and ignored the names.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

const INDICES: &str =
    "[indices] cannot be used with point in time. Do not specify any index with point in time.";

/// ES's message: every failing item's error, numbered, each ending in `;`.
fn reason(items: usize) -> String {
    let mut out = String::from("Validation Failed: ");
    for n in 1..=items {
        out.push_str(&format!("{n}: {INDICES};"));
    }
    out
}

async fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn send(app: &axum::Router, path: &str, ctype: &str, body: &str) -> (StatusCode, Value) {
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

/// The setup of ES's `search/350_point_in_time.yml`: three documents in
/// `test`, one in `test2`, and a PIT over `t*`.
async fn seeded() -> (axum::Router, tempfile::TempDir, String) {
    let (app, dir) = app().await;
    let bulk = "{\"index\":{\"_index\":\"test\",\"_id\":\"1\"}}\n{\"foo\":\"bar\",\"age\":18}\n\
                {\"index\":{\"_index\":\"test\",\"_id\":\"42\"}}\n{\"foo\":\"bar\",\"age\":18}\n\
                {\"index\":{\"_index\":\"test\",\"_id\":\"172\"}}\n{\"foo\":\"bar\",\"age\":24}\n\
                {\"index\":{\"_index\":\"test2\",\"_id\":\"45\"}}\n{\"foo\":\"bar\",\"age\":19}\n";
    let (status, got) = send(&app, "/_bulk?refresh=true", "application/x-ndjson", bulk).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["errors"], false, "{got}");
    let (status, got) = send(&app, "/t*/_pit?keep_alive=5m", "application/json", "{}").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let id = got["id"].as_str().expect("pit id").to_string();
    (app, dir, id)
}

fn assert_refused(path: &str, status: StatusCode, got: &Value, items: usize) {
    let reason = reason(items);
    assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {got}");
    assert_eq!(got["status"], 400, "{path}: {got}");
    assert_eq!(
        got["error"]["type"], "action_request_validation_exception",
        "{path}: {got}"
    );
    assert_eq!(got["error"]["reason"], reason, "{path}: {got}");
    assert_eq!(
        got["error"]["root_cause"][0]["type"], "action_request_validation_exception",
        "{path}: {got}"
    );
    assert_eq!(
        got["error"]["root_cause"][0]["reason"], reason,
        "{path}: {got}"
    );
}

/// The second `msearch` of the ES test: both headers name an index, and
/// ES reports both items.
#[tokio::test]
async fn an_msearch_item_whose_header_names_an_index_refuses_the_request() {
    let (app, _dir, id) = seeded().await;
    let body = format!(
        "{{\"index\":\"index\"}}\n{{\"query\":{{\"match\":{{\"foo\":\"bar\"}}}},\"pit\":{{\"id\":\"{id}\"}}}}\n\
         {{\"index\":\"index2\"}}\n{{\"query\":{{\"match_all\":{{}}}},\"pit\":{{\"id\":\"{id}\"}}}}\n"
    );
    let (status, got) = send(&app, "/_msearch", "application/x-ndjson", &body).await;
    assert_refused("/_msearch", status, &got, 2);
}

/// One PIT item among plain ones still refuses the whole request, and an
/// index named in the header counts even when it exists.
#[tokio::test]
async fn one_pit_item_naming_an_index_refuses_its_plain_siblings_too() {
    let (app, _dir, id) = seeded().await;
    let body = format!(
        "{{\"index\":\"test\"}}\n{{\"query\":{{\"match_all\":{{}}}}}}\n\
         {{\"index\":\"test\"}}\n{{\"pit\":{{\"id\":\"{id}\"}}}}\n"
    );
    let (status, got) = send(&app, "/_msearch", "application/x-ndjson", &body).await;
    assert_refused("/_msearch", status, &got, 1);
}

/// The `/{index}/_msearch` path index is every header's default, so a PIT
/// item under it names an index too.
#[tokio::test]
async fn an_msearch_path_index_counts_as_naming_one() {
    let (app, _dir, id) = seeded().await;
    let body = format!("{{}}\n{{\"pit\":{{\"id\":\"{id}\"}}}}\n");
    let (status, got) = send(&app, "/test/_msearch", "application/x-ndjson", &body).await;
    assert_refused("/test/_msearch", status, &got, 1);
}

/// ES reads a header's `indices` key the same as `index`.
#[tokio::test]
async fn an_msearch_header_indices_key_names_an_index() {
    let (app, _dir, id) = seeded().await;
    let body = format!("{{\"indices\":[\"test\"]}}\n{{\"pit\":{{\"id\":\"{id}\"}}}}\n");
    let (status, got) = send(&app, "/_msearch", "application/x-ndjson", &body).await;
    assert_refused("/_msearch", status, &got, 1);
}

/// A header `index` replaces the path index, and an empty one names
/// nothing, so the PIT item runs over the PIT's indices.
#[tokio::test]
async fn an_empty_header_index_overrides_the_path_index() {
    let (app, _dir, id) = seeded().await;
    for header in ["{\"index\":[]}", "{\"index\":\"\"}"] {
        let body = format!("{header}\n{{\"pit\":{{\"id\":\"{id}\"}}}}\n");
        let (status, got) = send(&app, "/test/_msearch", "application/x-ndjson", &body).await;
        assert_eq!(status, StatusCode::OK, "{header}: {got}");
        assert_eq!(
            got["responses"][0]["hits"]["total"]["value"], 4,
            "{header}: {got}"
        );
    }
}

/// `POST /{index}/_search` with a PIT is refused the same way.
#[tokio::test]
async fn a_search_whose_path_names_an_index_is_refused() {
    let (app, _dir, id) = seeded().await;
    let body = format!("{{\"pit\":{{\"id\":\"{id}\"}}}}");
    for path in ["/test/_search", "/test,test2/_search", "/*/_search"] {
        let (status, got) = send(&app, path, "application/json", &body).await;
        assert_refused(path, status, &got, 1);
    }
}

/// The first `msearch` of the ES test, and `/_search`: no index named, so
/// the PIT's indices are searched as before.
#[tokio::test]
async fn pit_searches_that_name_no_index_still_run() {
    let (app, _dir, id) = seeded().await;
    let body = format!(
        "{{}}\n{{\"query\":{{\"match\":{{\"_index\":\"test\"}}}},\"pit\":{{\"id\":\"{id}\"}}}}\n\
         {{}}\n{{\"query\":{{\"match_all\":{{}}}},\"pit\":{{\"id\":\"{id}\"}}}}\n"
    );
    let (status, got) = send(&app, "/_msearch", "application/x-ndjson", &body).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["responses"][0]["hits"]["total"]["value"], 3, "{got}");
    assert_eq!(
        got["responses"][0]["hits"]["total"]["relation"], "eq",
        "{got}"
    );
    assert_eq!(got["responses"][1]["hits"]["total"]["value"], 4, "{got}");
    assert_eq!(
        got["responses"][1]["hits"]["total"]["relation"], "eq",
        "{got}"
    );

    let body = format!("{{\"pit\":{{\"id\":\"{id}\"}}}}");
    let (status, got) = send(&app, "/_search", "application/json", &body).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["hits"]["total"]["value"], 4, "{got}");
}
