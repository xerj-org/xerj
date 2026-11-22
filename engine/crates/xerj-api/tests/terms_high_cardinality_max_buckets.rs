//! #3362 regression from #3383: `search.max_buckets` counts the buckets a
//! search RETURNS, not the distinct keys a `terms` agg sees.
//!
//! #3383 answered 400 too_many_buckets for any terms agg over a field with
//! more than 65,536 distinct values, so a top-10 over user IDs, IPs or trace
//! IDs failed where Elasticsearch answers 200 (its final reduce counts only
//! the buckets it keeps). Before #3383 the same request returned 200 with
//! the first 65,536 keys in document order and a `sum_other_doc_count` short
//! by every dropped key's documents.
//!
//! Here, at the stock cap of 65,536, over 100,000 distinct keys:
//! * a small `size` is served with exact counts, with and without sub-aggs,
//!   under a single-bucket parent, and across indices in both orders;
//! * a `size` that would return more buckets than the cap is a 400.
//!
//! One test function in its own binary: the bucket cap is process-wide.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

const UNIQUE: usize = 100_000;
const HOT: usize = 100;
const CAP: u64 = 65_536;

async fn send(app: &axum::Router, method: &str, path: &str, body: String) -> (StatusCode, Value) {
    let content_type = if path.contains("_bulk") {
        "application/x-ndjson"
    } else {
        "application/json"
    };
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", content_type)
        .body(Body::from(body))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn create(app: &axum::Router, index: &str, docs: Vec<(String, u64)>) {
    let mapping = json!({"mappings": {"properties": {
        "u": {"type": "keyword"},
        "n": {"type": "long"}
    }}});
    let (status, got) = send(app, "PUT", &format!("/{index}"), mapping.to_string()).await;
    assert!(status.is_success(), "create {index}: {status} {got}");
    for chunk in docs.chunks(10_000) {
        let mut bulk = String::with_capacity(chunk.len() * 48);
        for (u, n) in chunk {
            bulk.push_str("{\"index\":{}}\n");
            bulk.push_str(&format!("{{\"u\":\"{u}\",\"n\":{n}}}\n"));
        }
        let (status, got) = send(app, "POST", &format!("/{index}/_bulk"), bulk).await;
        assert_eq!(status, StatusCode::OK, "bulk {index}");
        assert_eq!(got["errors"], false, "bulk {index} had errors");
    }
    let (status, _) = send(app, "POST", &format!("/{index}/_refresh"), String::new()).await;
    assert!(status.is_success(), "refresh {index}");
}

async fn search(app: &axum::Router, indices: &str, body: Value) -> (StatusCode, Value) {
    send(
        app,
        "POST",
        &format!("/{indices}/_search"),
        body.to_string(),
    )
    .await
}

fn terms(size: u64) -> Value {
    json!({"size": 0, "aggs": {"t": {"terms": {"field": "u", "size": size}}}})
}

fn served<'a>(status: StatusCode, body: &'a Value, label: &str) -> &'a Value {
    assert_eq!(status, StatusCode::OK, "{label}: {}", body["error"]);
    &body["aggregations"]
}

fn refused(status: StatusCode, body: &Value, label: &str) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{label}: expected 400");
    let cause = &body["error"]["caused_by"];
    assert_eq!(
        cause["type"], "too_many_buckets_exception",
        "{label}: {}",
        body["error"]
    );
    assert_eq!(cause["max_buckets"], CAP, "{label}: {}", body["error"]);
}

fn buckets<'a>(agg: &'a Value, label: &str) -> &'a Vec<Value> {
    agg["buckets"]
        .as_array()
        .unwrap_or_else(|| panic!("{label}: no buckets in {agg}"))
}

#[tokio::test]
async fn max_buckets_counts_returned_buckets_not_distinct_keys() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    assert_eq!(config.limits.max_buckets as u64, CAP);
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    let app = xerj_api::router::build_es_compat_router(state);

    // `hc`: 100 x "hot" plus 100,000 unique keys, the hot docs split around
    // the uniques so a document-order cap would miss half of them.
    let mut hc: Vec<(String, u64)> = (0..HOT / 2).map(|_| ("hot".into(), 1)).collect();
    hc.extend((0..UNIQUE).map(|i| (format!("u{i}"), 1)));
    hc.extend((0..HOT / 2).map(|_| ("hot".into(), 1)));
    create(&app, "hc", hc).await;
    // `small`: 5 x "hot" plus a, b, c.
    let mut small: Vec<(String, u64)> = (0..5).map(|_| ("hot".into(), 1)).collect();
    small.extend(["a", "b", "c"].iter().map(|k| (k.to_string(), 1)));
    create(&app, "small", small).await;

    let hc_docs = (HOT + UNIQUE) as u64;

    // ── single index, small size: 200 with the exact top-10 ─────────────
    let (status, body) = search(&app, "hc", terms(10)).await;
    let aggs = served(status, &body, "hc size 10");
    let rows = buckets(&aggs["t"], "hc size 10");
    assert_eq!(rows.len(), 10);
    assert_eq!(rows[0]["key"], "hot");
    assert_eq!(rows[0]["doc_count"], HOT as u64);
    assert!(rows[1..].iter().all(|b| b["doc_count"] == 1));
    assert_eq!(aggs["t"]["sum_other_doc_count"], hc_docs - HOT as u64 - 9);
    assert_eq!(aggs["t"]["doc_count_error_upper_bound"], 0);

    // ── with a sub-agg ───────────────────────────────────────────────────
    let (status, body) = search(
        &app,
        "hc",
        json!({"size": 0, "aggs": {"t": {
            "terms": {"field": "u", "size": 10},
            "aggs": {"c": {"value_count": {"field": "n"}}}
        }}}),
    )
    .await;
    let aggs = served(status, &body, "hc size 10 + sub-agg");
    let rows = buckets(&aggs["t"], "hc size 10 + sub-agg");
    assert_eq!(rows.len(), 10);
    assert_eq!(rows[0]["doc_count"], HOT as u64);
    assert_eq!(rows[0]["c"]["value"], HOT as u64);

    // ── nested terms: 10 parents x 1 child = 20 returned buckets ────────
    let (status, body) = search(
        &app,
        "hc",
        json!({"size": 0, "aggs": {"t": {
            "terms": {"field": "u", "size": 10},
            "aggs": {"s": {"terms": {"field": "u", "size": 1}}}
        }}}),
    )
    .await;
    let aggs = served(status, &body, "hc nested terms");
    assert_eq!(buckets(&aggs["t"], "nested").len(), 10);

    // ── under a single-bucket parent ─────────────────────────────────────
    let (status, body) = search(
        &app,
        "hc",
        json!({"size": 0, "aggs": {"f": {
            "filter": {"match_all": {}},
            "aggs": {"t": {"terms": {"field": "u", "size": 3}}}
        }}}),
    )
    .await;
    let aggs = served(status, &body, "filter > terms");
    assert_eq!(
        buckets(&aggs["f"]["t"], "filter > terms")[0]["doc_count"],
        HOT as u64
    );

    // ── `_count: asc` top-5 ─────────────────────────────────────────────
    let (status, body) = search(
        &app,
        "hc",
        json!({"size": 0, "aggs": {"t": {"terms": {
            "field": "u", "size": 5, "order": {"_count": "asc"}
        }}}}),
    )
    .await;
    let aggs = served(status, &body, "hc count asc");
    let rows = buckets(&aggs["t"], "hc count asc");
    assert_eq!(rows.len(), 5);
    assert!(rows.iter().all(|b| b["doc_count"] == 1));

    // ── a size that would return more buckets than the cap: 400 ─────────
    let (status, body) = search(&app, "hc", terms(CAP + 1)).await;
    refused(status, &body, "hc size cap+1");
    // At the cap exactly it is served.
    let (status, body) = search(&app, "hc", terms(CAP)).await;
    let aggs = served(status, &body, "hc size cap");
    assert_eq!(buckets(&aggs["t"], "hc size cap").len() as u64, CAP);

    // ── multi-index, both orders ────────────────────────────────────────
    // Ties at doc_count 1 order by key: a, b < u*.
    for indices in ["hc,small", "small,hc"] {
        let (status, body) = search(&app, indices, terms(3)).await;
        let aggs = served(status, &body, indices);
        let rows = buckets(&aggs["t"], indices);
        let keys: Vec<&str> = rows.iter().map(|b| b["key"].as_str().unwrap()).collect();
        assert_eq!(keys, ["hot", "a", "b"], "{indices}");
        assert_eq!(rows[0]["doc_count"], HOT as u64 + 5, "{indices}");
        assert_eq!(
            aggs["t"]["sum_other_doc_count"],
            hc_docs + 8 - (HOT as u64 + 5) - 2,
            "{indices}"
        );

        let (status, body) = search(
            &app,
            indices,
            json!({"size": 0, "aggs": {"t": {
                "terms": {"field": "u", "size": 3},
                "aggs": {"c": {"value_count": {"field": "n"}}}
            }}}),
        )
        .await;
        let aggs = served(status, &body, &format!("{indices} + sub-agg"));
        let rows = buckets(&aggs["t"], indices);
        assert_eq!(rows[0]["key"], "hot", "{indices} + sub-agg");
        assert_eq!(rows[0]["doc_count"], HOT as u64 + 5, "{indices} + sub-agg");
        assert_eq!(
            rows[0]["c"]["value"].as_f64(),
            Some((HOT + 5) as f64),
            "{indices} + sub-agg"
        );

        let (status, body) = search(
            &app,
            indices,
            json!({"size": 0, "aggs": {"t": {"terms": {
                "field": "u", "size": 5, "order": {"_count": "asc"}
            }}}}),
        )
        .await;
        let aggs = served(status, &body, &format!("{indices} count asc"));
        let rows = buckets(&aggs["t"], indices);
        assert_eq!(rows.len(), 5, "{indices} count asc");
        assert!(
            rows.iter().all(|b| b["doc_count"] == 1),
            "{indices} count asc"
        );

        let (status, body) = search(&app, indices, terms(CAP + 1)).await;
        refused(status, &body, &format!("{indices} size cap+1"));
    }
}
