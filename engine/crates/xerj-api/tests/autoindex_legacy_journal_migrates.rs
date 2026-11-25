//! #3281: re-indexing an older prefix with `--no-graph` meets a
//! pre-generation `journal.ndjson`. The run used to refuse it and print a
//! rebuild into a NEW state dir and prefix; the field report recovered only by
//! deleting the journal by hand and running `xerj autoindex remove`. The prose
//! dataset was also mapped `semantic_text` by the older run, so a
//! `--no-semantic` plan that wants `text` met a mapper conflict.
//!
//! Now the run keeps the journal as `journal.ndjson.legacy-<ts>`, removes this
//! folder's indices for the prefix, and rebuilds them as the first generation,
//! with exit 0 and no manual step. The rebuilt prose field is `text`, the
//! document counts match the folder, and the next run is an ordinary
//! incremental one.
//!
//! The legacy journal comes from the default (graph) path, which still writes
//! the pre-generation resume plan that releases before the generation format
//! wrote; `rc.74` -> this build reproduced the same refusal by hand.

use std::path::Path;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;
use xerj_autoindex::cli::IndexCfg;

const PROSE: &str = "This is a longer prose paragraph about indexing, search engines and the \
                     weather in oslo and rome, written so that the profiler sees real sentences.";

async fn app() -> axum::Router {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.auth.enabled = false;
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    // The router keeps the data dir path; keep the directory for the process.
    std::mem::forget(dir);
    xerj_api::router::build_es_compat_router(state)
}

async fn call(app: &axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn index_cfg(root: &Path, state_dir: &Path, url: &str, no_graph: bool) -> IndexCfg {
    IndexCfg {
        root: root.to_owned(),
        endpoint_url: None,
        stub_globs: Vec::new(),
        default_stubs: true,
        url: url.to_owned(),
        api_key: None,
        api_key_file: None,
        workers: 1,
        scan_workers: 1,
        pdf_workers: 1,
        resource_notes: Vec::new(),
        xerj_url_note: None,
        pdf_timeout_secs: 30,
        bulk_mb: 1,
        bulk_timeout_secs: 30,
        snapshot_max_bytes: 64 << 30,
        prefix: "legacy".to_owned(),
        replace_prefix: false,
        prefix_note: None,
        state_dir: Some(state_dir.to_owned()),
        fresh: false,
        follow_symlinks: false,
        follow_symlinks_outside_root: false,
        ignore: xerj_autoindex::ignore_rules::IgnoreOptions::default(),
        max_file_bytes: 1 << 30,
        sample: 50,
        // The older run elected `semantic_text` for prose; the rerun does not.
        no_semantic: no_graph,
        code_analyzer: Default::default(),
        brain: None,
        no_graph,
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

async fn run_autoindex(root: &Path, state_dir: &Path, url: &str, no_graph: bool) -> i32 {
    let cfg = index_cfg(root, state_dir, url, no_graph);
    tokio::task::spawn_blocking(move || match xerj_autoindex::run_index_report(cfg) {
        Ok((code, _)) => code,
        Err(error) => panic!("autoindex (no_graph={no_graph}) failed: {error:#}"),
    })
    .await
    .expect("autoindex task")
}

/// `{index: (docs, body type)}` for every `legacy-*` index.
async fn indices(app: &axum::Router) -> Vec<(String, u64, Option<String>)> {
    let (status, mappings) = call(app, "GET", "/legacy-*/_mapping", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{mappings}");
    let mut out = Vec::new();
    for (name, mapping) in mappings.as_object().unwrap() {
        let (status, count) = call(app, "GET", &format!("/{name}/_count"), Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{count}");
        let body = mapping["mappings"]["properties"]["body"]["type"]
            .as_str()
            .map(str::to_owned);
        out.push((name.clone(), count["count"].as_u64().unwrap(), body));
    }
    out.sort();
    out
}

#[test]
fn a_legacy_journal_is_migrated_in_place_on_a_no_graph_rerun() {
    // Same runtime shape as the other in-process autoindex tests (#2903).
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(4 * 1024 * 1024)
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(inner());
}

async fn inner() {
    let app = app().await;
    let listener = xerj_common::loopback_test::expect_loopback(
        tokio::net::TcpListener::bind("127.0.0.1:0").await,
    );
    let addr = listener.local_addr().unwrap();
    let served = app.clone();
    tokio::spawn(async move {
        axum::serve(listener, served).await.unwrap();
    });
    let url = format!("http://{addr}");

    let corpus = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(
        corpus.path().join("people.csv"),
        "name,age,city\nann,31,oslo\nbob,42,rome\n",
    )
    .unwrap();
    std::fs::create_dir(corpus.path().join("docs")).unwrap();
    for i in 1..=3 {
        std::fs::write(
            corpus.path().join("docs").join(format!("n{i}.md")),
            format!("# Note {i}\n\n{PROSE} Note {i}.\n"),
        )
        .unwrap();
    }

    // The older shape: a pre-generation resume plan, prose as semantic_text.
    assert_eq!(
        run_autoindex(corpus.path(), state.path(), &url, false).await,
        0
    );
    let before = indices(&app).await;
    assert!(
        before
            .iter()
            .any(|(_, _, body)| body.as_deref() == Some("semantic_text")),
        "the older run maps prose as semantic_text: {before:?}"
    );
    let live_before: u64 = before.iter().map(|(_, n, _)| n).sum();

    // The rerun that used to refuse: exit 0, no manual step.
    assert_eq!(
        run_autoindex(corpus.path(), state.path(), &url, true).await,
        0
    );
    let kept: Vec<String> = std::fs::read_dir(state.path())
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with("journal.ndjson.legacy-"))
        .collect();
    assert_eq!(kept.len(), 1, "the legacy journal is kept aside: {kept:?}");
    let after = indices(&app).await;
    assert!(
        after
            .iter()
            .all(|(_, _, body)| body.as_deref() != Some("semantic_text")),
        "the rebuilt prose field follows the new plan: {after:?}"
    );
    assert!(
        after
            .iter()
            .any(|(_, _, body)| body.as_deref() == Some("text")),
        "prose is text now: {after:?}"
    );
    let live_after: u64 = after.iter().map(|(_, n, _)| n).sum();
    assert_eq!(
        live_after, live_before,
        "every record is rebuilt, none doubled: {before:?} -> {after:?}"
    );

    // And the next run is an ordinary incremental generation.
    assert_eq!(
        run_autoindex(corpus.path(), state.path(), &url, true).await,
        0
    );
    assert_eq!(indices(&app).await, after);
}
