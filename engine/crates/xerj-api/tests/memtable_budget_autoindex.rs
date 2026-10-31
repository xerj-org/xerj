//! Issue #2321: `xerj autoindex` against a node whose memtable budget fills
//! must finish (the server flushes, the client backs off and resumes), and a
//! search after that run must stay fast.
//!
//! Scaled down from the report: 1 MiB process budget and a flush line the
//! corpus never reaches, so the only flush that can free the budget is the
//! pressure drain. This file is its own binary so its governor is the first
//! one installed.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;
use xerj_autoindex::cli::IndexCfg;
use xerj_engine::Engine;

const BUDGET: u64 = 1024 * 1024;

fn boot() -> (axum::Router, Arc<Engine>, TempDir) {
    let dir = tempfile::tempdir().expect("data dir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.auth.enabled = false;
    config.limits.max_total_memtable_mb = 1;
    config.limits.memory_watermark_percent = 0;
    config.storage.flush_size_mb = 64;
    config.storage.flush_idle_secs = 0;
    config.storage.flush_interval_secs = 3600;
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    state.engine.spawn_resource_sampler();
    let engine = Arc::clone(&state.engine);
    let app = xerj_api::router::build_es_compat_router(state);
    (app, engine, dir)
}

fn index_cfg(root: &Path, state_dir: &Path, url: &str) -> IndexCfg {
    IndexCfg {
        root: root.to_owned(),
        endpoint_url: None,
        stub_globs: Vec::new(),
        url: url.to_owned(),
        api_key: None,
        api_key_file: None,
        workers: 2,
        scan_workers: 1,
        pdf_workers: 1,
        resource_notes: Vec::new(),
        xerj_url_note: None,
        pdf_timeout_secs: 30,
        bulk_mb: 1,
        bulk_timeout_secs: 60,
        snapshot_max_bytes: 64 << 30,
        prefix: "budget".to_owned(),
        replace_prefix: false,
        prefix_note: None,
        state_dir: Some(state_dir.to_owned()),
        fresh: false,
        follow_symlinks: false,
        follow_symlinks_outside_root: false,
        ignore: xerj_autoindex::ignore_rules::IgnoreOptions::default(),
        max_file_bytes: 1 << 30,
        sample: 50,
        no_semantic: true,
        code_analyzer: Default::default(),
        brain: None,
        no_graph: true,
        max_minutes: 0,
        approve: None,
        max_index_bytes: None,
        budget_plan: None,
        drop_indexed: false,
        dry_run: false,
        walk_only: false,
        json: false,
        quiet: true,
        progress: xerj_autoindex::progress::ProgressMode::None,
        progress_interval: None,
        watch: false,
        label: None,
        debounce: Duration::from_millis(0),
        exclude_rels: Vec::new(),
        exclude: Vec::new(),
        skip_corpus_data: false,
    }
}

async fn call(app: &axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
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
        .expect("body");
    let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, parsed)
}

fn corpus(root: &Path) {
    let pad = "a".repeat(3000);
    let mut file = String::new();
    for i in 0..600 {
        let line = serde_json::to_string(&json!({
            "body": format!("budgettoken {i} {pad}")
        }))
        .unwrap();
        file.push_str(&line);
        file.push('\n');
    }
    std::fs::write(root.join("records.jsonl"), file).unwrap();
}

#[test]
fn autoindex_completes_when_the_memtable_budget_fills_and_search_stays_fast() {
    // `#[tokio::test]` workers use tokio's 2 MiB stack. The production runtime
    // is 4 MiB (`xerj-server`'s `RT_THREAD_STACK_SIZE`) because the ES-compat
    // handlers do not fit in 2 MiB in a debug build. Match production.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_stack_size(4 * 1024 * 1024)
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(autoindex_budget_run());
}

async fn autoindex_budget_run() {
    let (app, engine, _dir) = boot();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = app.clone();
    tokio::spawn(async move {
        axum::serve(listener, served).await.unwrap();
    });
    let url = format!("http://{addr}");

    let root = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    corpus(root.path());

    let peak = Arc::new(AtomicU64::new(0));
    let peak_engine = Arc::clone(&engine);
    let peak_slot = Arc::clone(&peak);
    let poller = tokio::spawn(async move {
        loop {
            let bytes = peak_engine.total_memtable_bytes();
            peak_slot.fetch_max(bytes, Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    let root_path = root.path().to_owned();
    let state_path = state_dir.path().to_owned();
    let run = tokio::task::spawn_blocking(move || {
        xerj_autoindex::run_index_report(index_cfg(&root_path, &state_path, &url))
    });
    let (code, report) = tokio::time::timeout(Duration::from_secs(120), run)
        .await
        .expect("autoindex did not finish within 120s — the memtable-budget stall")
        .expect("autoindex task")
        .expect("autoindex report");
    poller.abort();
    assert!(
        code == 0 || code == 3,
        "autoindex exit {code}, report {report:?}"
    );

    let peaked = peak.load(Ordering::Relaxed);
    assert!(
        peaked >= BUDGET,
        "the run must fill the 1 MiB memtable budget (peak {peaked} bytes). \
         A run that never reaches it does not exercise the drain."
    );

    let held = engine.total_memtable_bytes();
    assert!(
        held < BUDGET,
        "autoindex returned with the memtable still over budget ({held} bytes) — \
         the drain did not free it"
    );
    let infos = engine.list_indices().await;
    let segments: usize = infos
        .iter()
        .filter(|info| info.name.starts_with("budget"))
        .map(|info| info.segment_count)
        .sum();
    assert!(
        segments > 0,
        "the pressure flush must publish a segment under the budget prefix: {infos:?}"
    );

    let started = Instant::now();
    let (status, body) = call(
        &app,
        "POST",
        "/budget-*/_search",
        json!({
            "size": 3,
            "query": { "match": { "body": "budgettoken" } }
        }),
    )
    .await;
    let elapsed = started.elapsed();
    assert_eq!(status, StatusCode::OK, "search {body}");
    assert!(
        elapsed < Duration::from_secs(5),
        "search after autoindex took {elapsed:?}"
    );
    let total = body["hits"]["total"]["value"].as_u64().unwrap_or(0);
    assert!(
        total > 0,
        "budgettoken must be searchable after the run: {body}"
    );
}
