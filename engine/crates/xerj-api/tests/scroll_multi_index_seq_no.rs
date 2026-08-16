//! #428 + #414 in composition: on a multi-index scroll, `_seq_no` / `_version`
//! must be read from the index each hit actually came from.
//!
//! These two fixes collided rather than composed. #414 made continuation pages
//! report a per-hit `_index`; #428's fix resolved the version map once, from the
//! context-level `ctx.index`. On #428's original base that was consistent — every
//! hit was stamped with the context index too, so the lookup agreed with what was
//! reported (both wrong, but agreeing). Once `_index` became per-hit, the lookup
//! did not follow, and hits from the second index reported `_seq_no` read out of
//! the first index's version map.
//!
//! That is worse than the absent field it replaced: `_seq_no`/`_primary_term`
//! exist to be fed back as `if_seq_no`/`if_primary_term`, so a consumer that
//! previously failed loudly now gets a confident wrong value — on exactly the
//! reindex/migration/CDC path scroll serves.
//!
//! Ids collide across the indices on purpose: that is the normal case when two
//! indices each number their own documents.

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

/// Write `times` distinct versions of each doc so `_seq_no` is not merely the
/// ordinal — an ordinal would let both the old hardcoded `0` and a wrong-index
/// lookup pass unnoticed.
async fn seed(app: &axum::Router, index: &str, times: usize) {
    let (st, _) = req(
        app,
        "PUT",
        &format!("/{index}"),
        json!({"mappings":{"properties":{"n":{"type":"long"}}}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "create {index}");
    for round in 0..times {
        for i in 0..4 {
            let (st, _) = req(
                app,
                "PUT",
                &format!("/{index}/_doc/{i}"),
                json!({"n": i, "round": round}),
            )
            .await;
            assert!(st.is_success(), "write {index}/{i}");
        }
    }
    req(app, "POST", &format!("/{index}/_refresh"), Value::Null).await;
}

/// Ground truth: a plain `_search` with the same flags, which resolves per hit.
async fn truth(app: &axum::Router, index: &str) -> Vec<(String, u64, u64)> {
    let (st, v) = req(
        app,
        "POST",
        &format!("/{index}/_search"),
        json!({"size":10,"seq_no_primary_term":true,"version":true,"sort":[{"_id":"asc"}]}),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    v["hits"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["_id"].as_str().unwrap().to_string(),
                h["_seq_no"].as_u64().expect("_seq_no on plain search"),
                h["_version"].as_u64().expect("_version on plain search"),
            )
        })
        .collect()
}

#[tokio::test]
async fn multi_index_scroll_reads_seq_no_from_the_hits_own_index() {
    let (app, _d) = app().await;
    seed(&app, "sq_a", 1).await;
    seed(&app, "sq_b", 4).await; // different write counts => different seq_no/version

    let a: Vec<_> = truth(&app, "sq_a").await;
    let b: Vec<_> = truth(&app, "sq_b").await;
    assert_ne!(
        a.iter().map(|x| x.1).collect::<Vec<_>>(),
        b.iter().map(|x| x.1).collect::<Vec<_>>(),
        "fixture is degenerate: both indices produced the same _seq_no values"
    );

    let (st, first) = req(
        &app,
        "POST",
        "/sq_a,sq_b/_search?scroll=2m",
        json!({"size":2,"seq_no_primary_term":true,"version":true}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "scroll start: {first}");

    let mut sid = first["_scroll_id"].as_str().map(str::to_string);
    let mut page = first;
    let mut seen = Vec::new();
    for _ in 0..16 {
        let hits = page["hits"]["hits"].as_array().cloned().unwrap_or_default();
        if hits.is_empty() {
            break;
        }
        for h in &hits {
            seen.push((
                h["_index"].as_str().unwrap_or_default().to_string(),
                h["_id"].as_str().unwrap_or_default().to_string(),
                h["_seq_no"].as_u64(),
                h["_version"].as_u64(),
            ));
        }
        let Some(id) = sid.clone() else { break };
        let (st, next) = req(
            &app,
            "POST",
            "/_search/scroll",
            json!({"scroll":"2m","scroll_id":id}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        sid = next["_scroll_id"].as_str().map(str::to_string).or(sid);
        page = next;
    }

    assert_eq!(seen.len(), 8, "scroll must return all 8 documents");

    let want = |idx: &str, id: &str| -> (u64, u64) {
        let src = if idx == "sq_a" { &a } else { &b };
        let row = src.iter().find(|r| r.0 == id).expect("id in truth");
        (row.1, row.2)
    };
    let wrong: Vec<_> = seen
        .iter()
        .filter(|(idx, id, sn, ver)| {
            let (w_sn, w_ver) = want(idx, id);
            *sn != Some(w_sn) || *ver != Some(w_ver)
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "hits carried _seq_no/_version from the wrong index's version map — \
         these feed straight back into if_seq_no/if_primary_term: {wrong:?}"
    );
}
