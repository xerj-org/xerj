//! #428: a scroll is a point-in-time view, so `_seq_no` / `_primary_term` /
//! `_version` must come from that point in time — not from the live index at
//! render time.
//!
//! Continuation pages once served the frozen `_source` beside meta-fields
//! resolved when the page was rendered. For a document updated mid-scroll the
//! lookup succeeded and stamped the CURRENT sequence number onto the OLD body.
//! A reindexer doing `PUT dest/_doc/x?if_seq_no=<that>` then passed optimistic
//! concurrency and overwrote the newer revision — a silent lost update, on
//! exactly the reindex/migration/CDC path scroll exists to serve.
//!
//! Lucene has no such gap: a scroll pins an `IndexReader`, a point-in-time view
//! whose `_seq_no` doc-values are written at index time and immutable per
//! segment. ES therefore emits a `_seq_no` that agrees with the `_source` next
//! to it, and a stale `if_seq_no` correctly 409s.

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

async fn req(app: &axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(path);
    let payload = if body.is_null() {
        Body::empty()
    } else {
        b = b.header("content-type", "application/json");
        Body::from(body.to_string())
    };
    let r = app.clone().oneshot(b.body(payload).unwrap()).await.unwrap();
    let st = r.status();
    let bytes = axum::body::to_bytes(r.into_body(), usize::MAX)
        .await
        .unwrap();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// A document updated after the scroll opened must still report the `_seq_no`
/// it had when the snapshot was taken, because that is the version the
/// `_source` beside it belongs to.
#[tokio::test]
async fn continuation_pages_report_the_snapshots_seq_no_not_the_live_one() {
    let (app, _d) = app().await;
    let (st, _) = req(
        &app,
        "PUT",
        "/skew",
        json!({"mappings":{"properties":{"n":{"type":"long"}}}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    for i in 0..6 {
        let (st, _) = req(
            &app,
            "PUT",
            &format!("/skew/_doc/d{i}"),
            json!({"n": i, "body": "v1"}),
        )
        .await;
        assert!(st.is_success());
    }
    req(&app, "POST", "/skew/_refresh", Value::Null).await;

    // Open the scroll and take page one — this is the point in time.
    let (st, first) = req(
        &app,
        "POST",
        "/skew/_search?scroll=5m",
        json!({"size":2,"seq_no_primary_term":true,"version":true,"sort":[{"_id":"asc"}]}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{first}");
    let sid = first["_scroll_id"].as_str().expect("scroll id").to_string();

    // Now move the documents that page two will serve. Twice, so the sequence
    // number is unambiguously different from the snapshot's.
    for round in ["v2", "v3"] {
        for i in 2..6 {
            let (st, _) = req(
                &app,
                "PUT",
                &format!("/skew/_doc/d{i}"),
                json!({"n": i, "body": round}),
            )
            .await;
            assert!(st.is_success());
        }
    }
    req(&app, "POST", "/skew/_refresh", Value::Null).await;

    // Walk the rest of the scroll.
    let mut sid = Some(sid);
    let mut page = first;
    let mut cont: Vec<(String, Option<u64>, String)> = Vec::new();
    let mut first_page = true;
    for _ in 0..8 {
        let hits = page["hits"]["hits"].as_array().cloned().unwrap_or_default();
        if hits.is_empty() {
            break;
        }
        if !first_page {
            for h in &hits {
                cont.push((
                    h["_id"].as_str().unwrap_or_default().to_string(),
                    h["_seq_no"].as_u64(),
                    h["_source"]["body"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                ));
            }
        }
        first_page = false;
        let Some(id) = sid.clone() else { break };
        let (st, next) = req(
            &app,
            "POST",
            "/_search/scroll",
            json!({"scroll":"5m","scroll_id":id}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        sid = next["_scroll_id"].as_str().map(str::to_string).or(sid);
        page = next;
    }

    assert!(!cont.is_empty(), "no continuation pages were produced");

    // The body must be the snapshot's. That is the whole point of a scroll.
    for (id, _, body) in &cont {
        assert_eq!(body, "v1", "{id}: continuation served a post-snapshot body");
    }

    // And the sequence number must belong to THAT body. Live values are what a
    // client would feed to `if_seq_no`, so a live value next to a stale body is
    // a CAS that succeeds and destroys the newer revision.
    let (st, live) = req(
        &app,
        "POST",
        "/skew/_search",
        json!({"size":10,"seq_no_primary_term":true,"version":true,"sort":[{"_id":"asc"}]}),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let live_sn: std::collections::HashMap<String, u64> = live["hits"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["_id"].as_str().unwrap().to_string(),
                h["_seq_no"].as_u64().unwrap_or(u64::MAX),
            )
        })
        .collect();

    let leaked: Vec<_> = cont
        .iter()
        .filter(|(id, sn, _)| sn.is_some() && *sn == live_sn.get(id).copied())
        .collect();
    assert!(
        leaked.is_empty(),
        "continuation pages reported the LIVE _seq_no beside a snapshot _source — \
         feeding these back as if_seq_no passes the CAS and overwrites the newer \
         revision: {leaked:?} (live: {live_sn:?})"
    );
}

/// The route that only ever omitted these fields must not start disagreeing
/// with itself: page one and continuation pages either both carry them or
/// neither does.
#[tokio::test]
async fn search_scroll_route_agrees_with_itself_across_pages() {
    let (app, _d) = app().await;
    req(
        &app,
        "PUT",
        "/ssr",
        json!({"mappings":{"properties":{"n":{"type":"long"}}}}),
    )
    .await;
    for i in 0..6 {
        req(&app, "PUT", &format!("/ssr/_doc/d{i}"), json!({"n": i})).await;
    }
    req(&app, "POST", "/ssr/_refresh", Value::Null).await;

    let (st, first) = req(
        &app,
        "POST",
        "/ssr/_search_scroll?scroll=5m",
        json!({"size":3,"seq_no_primary_term":true,"version":true,"sort":[{"_id":"asc"}]}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{first}");
    let sid = first["_scroll_id"].as_str().expect("scroll id").to_string();
    let p1_has = first["hits"]["hits"][0].get("_seq_no").is_some();

    let (st, second) = req(
        &app,
        "POST",
        "/_search/scroll",
        json!({"scroll":"5m","scroll_id":sid}),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let p2_has = second["hits"]["hits"][0].get("_seq_no").is_some();

    assert_eq!(
        p1_has, p2_has,
        "/{{index}}/_search_scroll disagrees with itself: page1 _seq_no present={p1_has}, \
         page2 present={p2_has} — a client reading the field unconditionally breaks on one page \
         of its own scroll (#428)"
    );
}
