//! `function_score { match_all, field_value_factor }` is served by the
//! columnar path; the same query with `min_score: 0` takes the stored scan.
//! On an all-tied page both must pick the same hits, in arrival order (#3506).
//!
//! Before the fix the columnar path broke ties by (segment, row), and segment
//! rows are not in arrival order: 6 docs, size 3 returned ids 1 2 4 after a
//! `_bulk` and 2 3 4 after one PUT per doc, where the stored scan returned
//! 1 2 3.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    // Several ingest shards: a flush writes one shard after another, so
    // segment rows are not in arrival order. The default depends on the
    // core count and can be 1 on a small box.
    config.engine.ingest_shards = 4;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    (xerj_api::router::build_es_compat_router(state), dir)
}

async fn req(app: &axum::Router, method: &str, path: &str, body: String) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body))
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

/// Hit ids and scores, in page order.
async fn page(app: &axum::Router, index: &str, body: Value) -> Vec<(String, f64)> {
    let (status, resp) = req(app, "POST", &format!("/{index}/_search"), body.to_string()).await;
    assert_eq!(status, StatusCode::OK, "{body}: {resp}");
    resp["hits"]["hits"]
        .as_array()
        .expect("hits")
        .iter()
        .map(|h| {
            (
                h["_id"].as_str().expect("_id").to_string(),
                h["_score"].as_f64().expect("_score"),
            )
        })
        .collect()
}

async fn seed(app: &axum::Router, index: &str, n: usize, bulk: bool) {
    if bulk {
        let mut body = String::new();
        for i in 1..=n {
            body.push_str(&format!(
                "{{\"index\":{{\"_id\":\"{i}\"}}}}\n{{\"r\":{i}}}\n"
            ));
        }
        let (status, resp) = req(app, "POST", &format!("/{index}/_bulk"), body).await;
        assert_eq!(status, StatusCode::OK, "bulk {index}: {resp}");
        assert_eq!(resp["errors"], json!(false), "bulk {index}: {resp}");
    } else {
        for i in 1..=n {
            let (status, resp) = req(
                app,
                "PUT",
                &format!("/{index}/_doc/{i}"),
                json!({ "r": i }).to_string(),
            )
            .await;
            assert!(status.is_success(), "put {index}/{i}: {resp}");
        }
    }
}

fn fvf_query(fvf: &Value, size: usize, brute: bool) -> Value {
    let mut body = json!({
        "size": size,
        "query": { "function_score": {
            "query": { "match_all": {} },
            "field_value_factor": fvf
        } }
    });
    if brute {
        // `min_score` defeats the columnar gate: the stored scan serves it.
        body["min_score"] = json!(0);
    }
    body
}

/// Every case scores all docs the same, so arrival order decides the page.
fn tied_cases() -> Vec<Value> {
    vec![
        // An absent, unmapped field (#3500 serves it columnarly).
        json!({ "field": "absent", "missing": 1 }),
        // No `missing` on an absent field is a 400 since #3512 (as in ES),
        // so the second tie is an explicit `missing: 0`.
        json!({ "field": "absent", "missing": 0 }),
        // A mapped numeric column with a zero factor.
        json!({ "field": "r", "factor": 0, "missing": 1 }),
    ]
}

async fn assert_columnar_equals_stored_scan(app: &axum::Router, index: &str, n: usize) {
    for phase in ["memtable", "flushed"] {
        if phase == "flushed" {
            let (status, body) = req(app, "POST", &format!("/{index}/_flush"), String::new()).await;
            assert_eq!(status, StatusCode::OK, "flush {index}: {body}");
        }
        for fvf in tied_cases() {
            for size in [1, 3, n - 1, n + 2] {
                let fast = page(app, index, fvf_query(&fvf, size, false)).await;
                let brute = page(app, index, fvf_query(&fvf, size, true)).await;
                let want: Vec<String> = (1..=n.min(size)).map(|i| i.to_string()).collect();
                let got: Vec<String> = fast.iter().map(|(id, _)| id.clone()).collect();
                assert_eq!(
                    got, want,
                    "{index} {phase} size={size} {fvf}: columnar page"
                );
                assert_eq!(
                    fast, brute,
                    "{index} {phase} size={size} {fvf}: columnar page != stored scan"
                );
            }
        }
        // Untied: the top of `r` (descending), same on both paths.
        let fvf = json!({ "field": "r" });
        let fast = page(app, index, fvf_query(&fvf, 3, false)).await;
        let brute = page(app, index, fvf_query(&fvf, 3, true)).await;
        let got: Vec<String> = fast.iter().map(|(id, _)| id.clone()).collect();
        let want: Vec<String> = (n - 2..=n).rev().map(|i| i.to_string()).collect();
        assert_eq!(got, want, "{index} {phase}: top r");
        assert_eq!(fast, brute, "{index} {phase}: top r != stored scan");
    }
}

#[tokio::test]
async fn tied_columnar_page_equals_stored_scan_after_bulk() {
    let (app, _dir) = app().await;
    for n in [6, 40] {
        let index = format!("fvf_tie_bulk_{n}");
        seed(&app, &index, n, true).await;
        assert_columnar_equals_stored_scan(&app, &index, n).await;
    }
}

#[tokio::test]
async fn tied_columnar_page_equals_stored_scan_after_single_puts() {
    let (app, _dir) = app().await;
    for n in [6, 40] {
        let index = format!("fvf_tie_put_{n}");
        seed(&app, &index, n, false).await;
        assert_columnar_equals_stored_scan(&app, &index, n).await;
    }
}

/// Two flushes: the page spans two segments plus the memtable.
#[tokio::test]
async fn tied_columnar_page_spans_segments_and_memtable_in_arrival_order() {
    let (app, _dir) = app().await;
    let index = "fvf_tie_multi";
    let mut body = String::new();
    for i in 1..=30 {
        body.push_str(&format!(
            "{{\"index\":{{\"_id\":\"{i}\"}}}}\n{{\"r\":{i}}}\n"
        ));
        if i == 10 || i == 20 {
            let (status, resp) = req(&app, "POST", &format!("/{index}/_bulk"), body.clone()).await;
            assert_eq!(status, StatusCode::OK, "bulk: {resp}");
            body.clear();
            let (status, resp) =
                req(&app, "POST", &format!("/{index}/_flush"), String::new()).await;
            assert_eq!(status, StatusCode::OK, "flush: {resp}");
        }
    }
    let (status, resp) = req(&app, "POST", &format!("/{index}/_bulk"), body).await;
    assert_eq!(status, StatusCode::OK, "bulk: {resp}");
    for fvf in tied_cases() {
        for size in [5, 15, 25, 35] {
            let fast = page(&app, index, fvf_query(&fvf, size, false)).await;
            let brute = page(&app, index, fvf_query(&fvf, size, true)).await;
            let want: Vec<String> = (1..=30.min(size)).map(|i| i.to_string()).collect();
            let got: Vec<String> = fast.iter().map(|(id, _)| id.clone()).collect();
            assert_eq!(got, want, "size={size} {fvf}: columnar page");
            assert_eq!(
                fast, brute,
                "size={size} {fvf}: columnar page != stored scan"
            );
        }
    }
}

/// #3500 with `dynamic: false`: fields present in `_source` but not in the
/// declared mapping (`r`, `obj.r`) and a field no document has (`absent`).
/// The columnar page must equal the stored scan for each, tied or not.
#[tokio::test]
async fn dynamic_false_columnar_page_equals_stored_scan() {
    let (app, _dir) = app().await;
    let index = "fvf_dyn_false";
    let (status, resp) = req(
        &app,
        "PUT",
        &format!("/{index}"),
        json!({"mappings": {"dynamic": false, "properties": {"title": {"type": "text"}}}})
            .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create: {resp}");
    let mut body = String::new();
    for i in 1..=12 {
        body.push_str(&format!(
            "{{\"index\":{{\"_id\":\"{i}\"}}}}\n{{\"title\":\"t{i}\",\"r\":{i},\"obj\":{{\"r\":{i}}}}}\n"
        ));
    }
    let (status, resp) = req(&app, "POST", &format!("/{index}/_bulk"), body).await;
    assert_eq!(status, StatusCode::OK, "bulk: {resp}");
    assert_eq!(resp["errors"], json!(false), "bulk: {resp}");
    for phase in ["memtable", "flushed"] {
        if phase == "flushed" {
            let (status, b) = req(&app, "POST", &format!("/{index}/_flush"), String::new()).await;
            assert_eq!(status, StatusCode::OK, "flush: {b}");
        }
        let cases = [
            (json!({ "field": "r", "missing": 1 }), ["12", "11", "10"]),
            (
                json!({ "field": "obj.r", "missing": 1 }),
                ["12", "11", "10"],
            ),
            (json!({ "field": "absent", "missing": 1 }), ["1", "2", "3"]),
            (json!({ "field": "absent", "missing": 0 }), ["1", "2", "3"]),
        ];
        for (fvf, want) in cases {
            let fast = page(&app, index, fvf_query(&fvf, 3, false)).await;
            let brute = page(&app, index, fvf_query(&fvf, 3, true)).await;
            let got: Vec<&str> = fast.iter().map(|(id, _)| id.as_str()).collect();
            assert_eq!(got, want, "{phase} {fvf}");
            assert_eq!(fast, brute, "{phase} {fvf}: columnar page != stored scan");
        }
    }
}
