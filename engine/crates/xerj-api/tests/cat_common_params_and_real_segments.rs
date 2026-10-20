//! #1201 + #1202 endpoint coverage: the shared `_cat` common params (`h`, `v`,
//! `bytes`) through the real router, and `_cat/segments` reporting one row per
//! LIVE durable segment instead of a fabricated `_0` row.
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

/// Issue a request; return status + body. JSON when the body parses, raw text
/// otherwise (the `_cat` text format is not JSON).
async fn call(app: &axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, String) {
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
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn call_json(app: &axum::Router, method: &str, path: &str, body: Value) -> Value {
    let (st, text) = call(app, method, path, body).await;
    assert_eq!(st, StatusCode::OK, "{method} {path}: {text}");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path} body not JSON ({e}): {text}"))
}

/// Create an index, index one doc, refresh — the minimal state every test
/// below starts from.
async fn seeded_index(app: &axum::Router, name: &str) {
    let (st, body) = call(
        app,
        "PUT",
        &format!("/{name}"),
        json!({"mappings": {"properties": {"v": {"type": "keyword"}}}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create {name}: {body}");
    let (st, body) = call(app, "PUT", &format!("/{name}/_doc/1"), json!({"v": "one"})).await;
    assert_eq!(st, StatusCode::CREATED, "index doc into {name}: {body}");
    let (st, body) = call(app, "POST", &format!("/{name}/_refresh"), Value::Null).await;
    assert_eq!(st, StatusCode::OK, "refresh {name}: {body}");
}

// ── #1201: h / v / bytes through the router ──────────────────────────────────

/// `h` selects and reorders the printed columns; `v` adds the header row.
#[tokio::test]
async fn cat_indices_h_selects_and_v_adds_header() {
    let (app, _dir) = app().await;
    seeded_index(&app, "books").await;

    let (st, text) = call(
        &app,
        "GET",
        "/_cat/indices?h=docs.count,index&v=true",
        Value::Null,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{text}");
    let lines: Vec<&str> = text.trim_end().lines().collect();
    assert_eq!(
        lines[0], "docs.count index",
        "header is the selected columns in request order"
    );
    assert_eq!(lines.len(), 2, "one header + one row: {text}");
    assert!(
        lines[1].starts_with("1 ") && lines[1].ends_with(" books"),
        "row carries the values in the selected order: {text}"
    );

    // json honours h the same way, without the header.
    let rows = call_json(
        &app,
        "GET",
        "/_cat/indices?h=docs.count,index&format=json",
        Value::Null,
    )
    .await;
    let row = rows[0].as_object().unwrap();
    let keys: Vec<&str> = row.keys().map(|k| k.as_str()).collect();
    assert_eq!(
        keys,
        vec!["docs.count", "index"],
        "json keys follow the h order: {rows}"
    );
}

/// `bytes=b` re-renders the size columns as plain byte integers; an unknown
/// unit is a 400, never a silent ignore.
#[tokio::test]
async fn cat_indices_bytes_re_renders_and_refuses_unknown_units() {
    let (app, _dir) = app().await;
    seeded_index(&app, "films").await;

    let rows = call_json(
        &app,
        "GET",
        "/_cat/indices?bytes=b&format=json",
        Value::Null,
    )
    .await;
    let raw = rows[0]["store.size"].as_str().unwrap();
    assert!(
        raw.chars().all(|c| c.is_ascii_digit()),
        "bytes=b renders store.size as a plain integer, got {raw:?}"
    );

    let (st, body) = call(&app, "GET", "/_cat/indices?bytes=nonsense", Value::Null).await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "unknown bytes unit must 400: {body}"
    );
    assert!(body.contains("kb"), "the 400 names the valid units: {body}");

    let (st, body) = call(
        &app,
        "GET",
        "/_cat/segments/films?bytes=nonsense",
        Value::Null,
    )
    .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "every _cat endpoint refuses the same way: {body}"
    );
}

// ── #1202: real per-segment rows ─────────────────────────────────────────────

/// `_cat/segments` reports one row per live durable segment with real ids —
/// not one fabricated `_0` row per index — and the row count matches the
/// index's own segment count from `_stats`.
#[tokio::test]
async fn cat_segments_rows_are_real_per_segment() {
    let (app, _dir) = app().await;
    seeded_index(&app, "segidx").await;
    // A second flush forces a second durable segment, so row-count == 1 is
    // not vacuous.
    let (st, body) = call(&app, "PUT", "/segidx/_doc/2", json!({"v": "two"})).await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let (st, body) = call(&app, "POST", "/segidx/_flush", Value::Null).await;
    assert_eq!(st, StatusCode::OK, "flush: {body}");

    let rows = call_json(
        &app,
        "GET",
        "/_cat/segments/segidx?format=json&h=index,segment,docs.count,docs.deleted,size",
        Value::Null,
    )
    .await;
    assert!(
        !rows.as_array().unwrap().is_empty(),
        "at least one segment row"
    );
    for row in rows.as_array().unwrap() {
        assert_eq!(row["index"].as_str(), Some("segidx"));
        let id = row["segment"].as_str().unwrap();
        assert_ne!(id, "_0", "no fabricated _0 ids (#1202): {rows}");
        assert!(
            id.len() >= 8 && id.contains('-'),
            "segment id is the durable segment's UUID: {rows}"
        );
        assert!(
            row["docs.count"]
                .as_str()
                .unwrap()
                .chars()
                .all(|c| c.is_ascii_digit()),
            "docs.count is numeric: {rows}"
        );
    }

    // Row count must equal the engine's own live segment count.
    let stats = call_json(&app, "GET", "/segidx/_stats", Value::Null).await;
    let reported: usize = stats
        .pointer("/indices/segidx/total/segments/count")
        .or_else(|| stats.pointer("/_all/total/segment_count"))
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(rows.as_array().unwrap().len());
    assert_eq!(
        rows.as_array().unwrap().len(),
        reported,
        "one row per live segment (_stats says {reported}): rows={rows} stats={stats}"
    );
}

/// A delete followed by a flush shows up in the deleted column. A xerj
/// tombstone can persist in a LATER segment than the doc it kills, so the
/// honest reading is `Σ docs.count − Σ docs.deleted = live docs` — that
/// invariant, not per-row liveness, is what this pins (#1202). This is the
/// exact view the #1186 fabrication hid.
#[tokio::test]
async fn cat_segments_shows_deletes_after_flush() {
    let (app, _dir) = app().await;
    seeded_index(&app, "delidx").await;
    let (st, body) = call(&app, "PUT", "/delidx/_doc/2", json!({"v": "two"})).await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let (st, body) = call(&app, "PUT", "/delidx/_doc/3", json!({"v": "three"})).await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    let (st, body) = call(&app, "POST", "/delidx/_flush", Value::Null).await;
    assert_eq!(st, StatusCode::OK, "{body}");

    let before = call_json(
        &app,
        "GET",
        "/_cat/segments/delidx?format=json&h=docs.count,docs.deleted",
        Value::Null,
    )
    .await;
    let (live_before, del_before) = live_and_deleted(&before);
    assert_eq!(
        live_before - del_before,
        3,
        "3 live docs before the delete: {before}"
    );

    let (st, body) = call(&app, "DELETE", "/delidx/_doc/1", Value::Null).await;
    assert_eq!(st, StatusCode::OK, "delete doc 1: {body}");
    let (st, body) = call(&app, "POST", "/delidx/_flush", Value::Null).await;
    assert_eq!(st, StatusCode::OK, "flush the tombstone: {body}");

    let after = call_json(
        &app,
        "GET",
        "/_cat/segments/delidx?format=json&h=docs.count,docs.deleted",
        Value::Null,
    )
    .await;
    let (live_after, del_after) = live_and_deleted(&after);
    assert!(
        del_after > del_before,
        "the delete is visible as docs.deleted: {after}"
    );
    assert_eq!(
        live_after - del_after,
        2,
        "Σ docs.count − Σ docs.deleted is the live count: {after}"
    );
}

/// Σ docs.count and Σ docs.deleted over a `_cat/segments` json body.
fn live_and_deleted(rows: &Value) -> (u64, u64) {
    let live: u64 = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["docs.count"].as_str().unwrap().parse::<u64>().unwrap())
        .sum();
    let deleted: u64 = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["docs.deleted"].as_str().unwrap().parse::<u64>().unwrap())
        .sum();
    (live, deleted)
}

/// The whole-cluster `GET /_cat/segments` form lists every index's segments
/// (#1202), and `v` prints the header even where nothing matches.
#[tokio::test]
async fn cat_segments_whole_cluster_and_empty_header() {
    let (app, _dir) = app().await;
    seeded_index(&app, "cluster-a").await;
    seeded_index(&app, "cluster-b").await;

    let rows = call_json(
        &app,
        "GET",
        "/_cat/segments?format=json&h=index",
        Value::Null,
    )
    .await;
    let mut indices: Vec<String> = rows
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["index"].as_str().map(String::from))
        .collect();
    indices.sort();
    indices.dedup();
    assert_eq!(
        indices,
        vec!["cluster-a".to_string(), "cluster-b".to_string()],
        "the no-path form covers every index: {rows}"
    );

    // v prints the header line even when the selector matches nothing. With
    // no rows the column widths are the header widths themselves, so the
    // header cells join with a single space.
    let (st, text) = call(
        &app,
        "GET",
        "/_cat/segments/no-such-*?v=true&h=index,segment",
        Value::Null,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{text}");
    assert_eq!(text, "index segment\n", "header only, no rows: {text:?}");
}
