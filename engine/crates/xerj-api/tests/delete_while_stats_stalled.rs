//! #2405: stalled statistics must not hold index deletion behind a schema
//! lock or a DashMap iterator. The schema hold models stalled work without
//! depending on host RSS, an OOM, or scheduler sleeps.

use std::future::{poll_fn, Future};
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

fn app(dir: &tempfile::TempDir) -> (axum::Router, Arc<xerj_engine::Engine>) {
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config.storage.flush_idle_secs = 0;
    config.storage.flush_interval_secs = 3600;
    let metrics = xerj_common::metrics::Metrics::new().unwrap();
    let engine = xerj_engine::Engine::new(config.clone()).unwrap();
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
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn create(app: &axum::Router, name: &str) {
    let (status, body) = request(app, "PUT", &format!("/{name}"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "create {name}: {body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_selector_does_not_collect_a_stalled_siblings_stats() {
    let dir = tempfile::tempdir().unwrap();
    let (app, engine) = app(&dir);
    create(&app, "victim-one").await;
    create(&app, "victim-two").await;
    create(&app, "busy").await;
    let busy = engine.get_index("busy").unwrap();
    busy.index_document(Some("kept".into()), json!({"value": "survives"}))
        .await
        .unwrap();
    let hold = busy.test_hold_schema_write().await;

    let deletion = tokio::time::timeout(
        Duration::from_secs(5),
        request(&app, "DELETE", "/victim-one,victim-two", json!({})),
    )
    .await;
    drop(hold);
    let (status, body) = deletion.expect("DELETE waited for an unrelated index's statistics");
    assert_eq!(status, StatusCode::OK, "delete: {body}");
    assert_eq!(body["acknowledged"], true);
    for name in ["victim-one", "victim-two"] {
        assert!(engine.get_index(name).is_err());
        assert!(!dir.path().join(name).exists());
    }
    let (status, body) = request(&app, "GET", "/busy/_doc/kept", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["_source"]["value"], "survives");

    // The name is reusable after deletion; no stale handle owns the new index.
    create(&app, "victim-one").await;
    let (status, _) = request(
        &app,
        "PUT",
        "/victim-one/_doc/new",
        json!({"value": "new generation"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

async fn delete_while_stats_sweep_is_pending(health: bool) {
    let dir = tempfile::tempdir().unwrap();
    let (app, engine) = app(&dir);
    create(&app, "busy").await;
    let busy = engine.get_index("busy").unwrap();
    let hold = busy.test_hold_schema_write().await;
    let mut sweep = Box::pin(async {
        if health {
            let _ = engine.health().await;
        } else {
            let _ = engine.list_indices().await;
        }
    });
    // Poll once ourselves: the only index is held, so this proves that the
    // sweep reached stats(). No sleep guesses whether the task has started.
    poll_fn(|cx| {
        assert!(sweep.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;

    let deleter = Arc::clone(&engine);
    let mut task = tokio::spawn(async move { deleter.delete_index("busy").await });
    let finished = tokio::time::timeout(Duration::from_secs(5), &mut task).await;
    // Release both holds even on the negative control, where the delete
    // worker is blocked synchronously on the map shard held by `sweep`.
    drop(sweep);
    drop(hold);
    let completed_while_held = finished.is_ok();
    let result = match finished {
        Ok(result) => result,
        Err(_) => tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .expect("delete did not recover after releasing the sweep"),
    };
    result.expect("delete task").expect("delete index");
    assert!(
        completed_while_held,
        "DELETE waited for a stalled {} sweep's map lock",
        if health { "health" } else { "listing" }
    );
    assert!(!dir.path().join("busy").exists());
    assert!(engine.get_index("busy").is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_does_not_wait_for_a_parked_listing() {
    delete_while_stats_sweep_is_pending(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_does_not_wait_for_a_parked_health_check() {
    delete_while_stats_sweep_is_pending(true).await;
}
