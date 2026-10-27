//! #2405 item 1 — `DELETE /<index>` on a node near its memory cap must not
//! wedge behind work on OTHER indices.
//!
//! Pre-fix the ES-compat DELETE handler resolved its selector through
//! `Engine::list_indices`, which computes `stats()` for every open index —
//! each one takes that index's schema read lock and its memtable shard
//! locks. A single sibling index whose schema lock was held (a mapping
//! update or flush stalled under memory pressure) parked the DELETE before
//! it began. And `list_indices` (like the periodic flush sweep) awaited
//! inside a `DashMap` iterator, so the shard read lock it held blocked the
//! `indices.remove` of a delete — synchronously, on a tokio worker thread.
//!
//! The schema-write hold (`lock-test-hook`) stands in for that stalled
//! sibling. Its own binary so `governor::init` sees this file's small
//! memtable budget (the first `Engine::new` in a process wins).

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

fn config(dir: &tempfile::TempDir) -> xerj_common::config::Config {
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    // 1 MiB summed-memtable budget: the breaker shape of an 8 GiB node at a
    // size a test can fill. No per-index or periodic flush gets in the way.
    config.limits.max_total_memtable_mb = 1;
    config.limits.memory_watermark_percent = 0;
    config.storage.flush_size_mb = 64;
    config.storage.flush_idle_secs = 0;
    config.storage.flush_interval_secs = 3600;
    config
}

fn app(dir: &tempfile::TempDir) -> (axum::Router, Arc<xerj_engine::Engine>) {
    let config = config(dir);
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    let engine = Arc::clone(&state.engine);
    (xerj_api::router::build_es_compat_router(state), engine)
}

async fn request(app: &axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
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
        .expect("response body");
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn create(app: &axum::Router, name: &str) {
    let (status, body) = request(
        app,
        "PUT",
        &format!("/{name}"),
        json!({"mappings": {"properties": {"body": {"type": "text"}}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create {name}: {body}");
}

/// Fill `victim` and `busy` up to the 1 MiB budget (or until the breaker
/// starts refusing writes), hold `busy`'s schema lock, and DELETE `victim`
/// over HTTP. The DELETE must answer promptly and release `victim`'s
/// memtable bytes while `busy` is still stuck.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_does_not_wait_on_a_stalled_sibling_under_memory_pressure() {
    let dir = tempfile::tempdir().expect("data dir");
    let (app, engine) = app(&dir);
    create(&app, "victim").await;
    create(&app, "busy").await;

    let body = format!("n{}", "a".repeat(4096));
    for n in 0..400 {
        let name = if n % 2 == 0 { "victim" } else { "busy" };
        let idx = engine.get_index(name).expect("index");
        if idx
            .index_document(Some(format!("{name}-{n}")), json!({ "body": body }))
            .await
            .is_err()
        {
            break; // breaker engaged: the node is at its memtable budget
        }
        if engine.total_memtable_bytes() >= 1024 * 1024 {
            break;
        }
    }
    let victim = engine.get_index("victim").expect("victim");
    let victim_bytes = victim.memtable_bytes() as u64;
    let before = engine.total_memtable_bytes();
    assert!(victim_bytes > 0, "fixture must leave data in victim's memtable");
    let victim_dir = dir.path().join("victim");
    assert!(victim_dir.exists());

    let busy = engine.get_index("busy").expect("busy");
    let hold = busy.test_hold_schema_write().await;

    let deleted = tokio::time::timeout(
        Duration::from_secs(10),
        request(&app, "DELETE", "/victim", json!({})),
    )
    .await;
    drop(hold);
    let (status, body) =
        deleted.expect("DELETE /victim did not return within 10s while a sibling index was stalled");
    assert_eq!(status, StatusCode::OK, "delete: {body}");
    assert!(!victim_dir.exists(), "victim's directory must be gone");
    assert!(engine.get_index("victim").is_err());
    assert!(
        engine.total_memtable_bytes() <= before - victim_bytes,
        "victim's memtable must no longer count against the budget"
    );
}

/// A listing parked on `busy` (its stats wait on the held schema lock) must
/// not block a DELETE of `busy` itself. Pre-fix `list_indices` awaited
/// inside the `DashMap` iterator, so it held the read lock on `busy`'s
/// shard and `indices.remove("busy")` blocked a worker thread until the
/// listing finished.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_does_not_block_behind_a_listing_parked_on_the_same_index() {
    let dir = tempfile::tempdir().expect("data dir");
    let (app, engine) = app(&dir);
    create(&app, "busy").await;

    let busy = engine.get_index("busy").expect("busy");
    let hold = busy.test_hold_schema_write().await;
    let lister = Arc::clone(&engine);
    let listing = tokio::spawn(async move { lister.list_indices().await });
    // Let the listing reach `busy`'s stats and park on the schema lock.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!listing.is_finished(), "listing must be parked on busy's schema lock");

    let deleter = Arc::clone(&engine);
    let delete = tokio::spawn(async move { deleter.delete_index("busy").await });
    let deleted = tokio::time::timeout(Duration::from_secs(10), async {
        while !delete.is_finished() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let finished_while_held = deleted.is_ok();
    drop(hold);
    let result = tokio::time::timeout(Duration::from_secs(30), delete)
        .await
        .expect("delete never finished")
        .expect("delete task");
    assert!(
        finished_while_held,
        "DELETE of busy waited for a listing parked on busy's schema lock"
    );
    result.expect("delete busy");
    let _ = tokio::time::timeout(Duration::from_secs(30), listing).await;
    assert!(!dir.path().join("busy").exists());
}
