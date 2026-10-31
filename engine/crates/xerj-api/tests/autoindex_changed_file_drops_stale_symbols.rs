//! Issue #2139: re-indexing a changed source file must not leave documents
//! for symbols that moved or disappeared.
//!
//! Per-symbol `_id`s are `code:<line>:<name>` (#500). A signature edit that
//! also shifts the declaration does not overwrite the old symbol document,
//! so the file-level delete on `ax_file` is what has to remove it. The
//! autoindex HTTP mocks honour that `term` exactly, and the CSV / same-line
//! reindex tests overwrite a stable locator, so a delete the real engine
//! does not apply still looks clean there.
//!
//! This boots the ES-compat router and drives `xerj autoindex` over HTTP,
//! once with the graph enabled (the resume path the field reports hit) and
//! once with `--no-graph`.

use std::path::Path;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;
use xerj_autoindex::cli::IndexCfg;

const CODE_V1: &str = "\
struct Jobs;\n\
\n\
impl Jobs {\n\
    pub fn gone_symbol() -> i32 {\n\
        1\n\
    }\n\
\n\
    pub fn query_job(group_id: &str, raw: &str) -> bool {\n\
        !raw.is_empty()\n\
    }\n\
}\n\
";

const CODE_V2: &str = "\
struct Jobs;\n\
\n\
impl Jobs {\n\
    // the signature gained &self and the declaration moved down\n\
\n\
\n\
    pub fn query_job(&self, group_id: &str, raw: &str) -> bool {\n\
        !raw.is_empty()\n\
    }\n\
}\n\
";

async fn app() -> axum::Router {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.auth.enabled = false;
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    // Leak the temp dir for the process lifetime: the router keeps the data
    // dir path, and dropping `TempDir` at the end of this function would
    // delete it out from under the engine.
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

fn index_cfg(
    root: &Path,
    state_dir: &Path,
    url: &str,
    prefix: &str,
    no_graph: bool,
    fresh: bool,
) -> IndexCfg {
    IndexCfg {
        root: root.to_owned(),
        endpoint_url: None,
        stub_globs: Vec::new(),
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
        prefix: prefix.to_owned(),
        replace_prefix: false,
        prefix_note: None,
        state_dir: Some(state_dir.to_owned()),
        fresh,
        follow_symlinks: false,
        follow_symlinks_outside_root: false,
        ignore: xerj_autoindex::ignore_rules::IgnoreOptions::default(),
        max_file_bytes: 1 << 30,
        sample: 50,
        // The field reports ran a default autoindex, which elects semantic
        // text on code. `no_semantic` hides any passage document that election
        // writes beside the symbol records.
        no_semantic: false,
        code_analyzer: Default::default(),
        brain: Some(format!("brain-{prefix}")),
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

fn run_autoindex(
    root: &Path,
    state_dir: &Path,
    url: &str,
    prefix: &str,
    no_graph: bool,
    fresh: bool,
) -> i32 {
    let cfg = index_cfg(root, state_dir, url, prefix, no_graph, fresh);
    match xerj_autoindex::run_index_report(cfg) {
        Ok((code, _)) => code,
        Err(error) => panic!("autoindex {prefix} failed: {error:#}"),
    }
}

/// `xerj autoindex` builds a reqwest blocking client, which owns a tokio
/// runtime. Constructing that client on the test runtime panics ("Cannot
/// drop a runtime in a context where blocking is not allowed"), so the run
/// happens on the blocking pool.
async fn run_autoindex_blocking(
    root: &Path,
    state_dir: &Path,
    url: &str,
    prefix: &str,
    no_graph: bool,
    fresh: bool,
) -> i32 {
    let root = root.to_owned();
    let state_dir = state_dir.to_owned();
    let url = url.to_owned();
    let prefix = prefix.to_owned();
    tokio::task::spawn_blocking(move || {
        run_autoindex(&root, &state_dir, &url, &prefix, no_graph, fresh)
    })
    .await
    .expect("autoindex task")
}

/// Every `_source` under `{prefix}-*`, file cards included.
async fn prefix_docs(app: &axum::Router, prefix: &str) -> Vec<Value> {
    let (status, body) = call(
        app,
        "POST",
        &format!("/{prefix}-*/_search"),
        json!({
            "size": 200,
            "query": {"match_all": {}},
            "sort": [{"_id": "asc"}]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "search {prefix}-*: {body}");
    body["hits"]["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("no hits array: {body}"))
        .iter()
        .map(|hit| hit["_source"].clone())
        .collect()
}

fn symbol_rows(docs: &[Value]) -> Vec<(String, u64, String)> {
    let mut rows: Vec<(String, u64, String)> = docs
        .iter()
        .filter(|doc| doc.get("defs").is_none() && doc["ax_locator"].as_str() != Some("file"))
        .filter_map(|doc| {
            Some((
                doc.get("name")?.as_str()?.to_string(),
                doc.get("line")?.as_u64()?,
                doc.get("code")?.as_str()?.to_string(),
            ))
        })
        .collect();
    rows.sort();
    rows
}

fn assert_only_current(label: &str, live: &[Value], fresh: &[Value]) {
    let rendered = serde_json::to_string(live).unwrap();
    assert!(
        !rendered.contains("gone_symbol"),
        "{label}: removed symbol gone_symbol is still indexed: {rendered}"
    );
    assert!(
        !rendered.contains("pub fn query_job(group_id"),
        "{label}: the pre-edit query_job signature is still indexed: {rendered}"
    );
    assert!(
        rendered.contains("pub fn query_job(&self"),
        "{label}: the current query_job signature is missing: {rendered}"
    );
    assert_eq!(
        symbol_rows(live),
        symbol_rows(fresh),
        "{label}: symbol documents (name, line, code) must match a fresh index of the edited file\n\
         live={rendered}\nfresh={}",
        serde_json::to_string(fresh).unwrap()
    );
    assert_eq!(
        live.len(),
        fresh.len(),
        "{label}: document count must match a fresh index of the edited file\n\
         live={rendered}\nfresh={}",
        serde_json::to_string(fresh).unwrap()
    );
}

/// Index v1, shift `query_job` and delete `gone_symbol`, reindex on the same
/// prefix, and compare with a from-scratch index of v2.
async fn reindex_changed_code(app: &axum::Router, url: &str, no_graph: bool) {
    let mode = if no_graph { "nograph" } else { "graph" };
    let edited = tempfile::tempdir().unwrap();
    let edited_state = tempfile::tempdir().unwrap();
    std::fs::write(edited.path().join("jobs.rs"), CODE_V1).unwrap();
    let prefix = format!("stale{mode}");
    let code = run_autoindex_blocking(
        edited.path(),
        edited_state.path(),
        url,
        &prefix,
        no_graph,
        false,
    )
    .await;
    assert_eq!(code, 0, "{mode} genesis exit {code}");
    let first = prefix_docs(app, &prefix).await;
    let first_rows = symbol_rows(&first);
    assert!(
        first_rows.iter().any(|(name, _, code)| {
            name == "query_job" && code.contains("group_id") && !code.contains("&self")
        }),
        "{mode} genesis must index the original query_job: {first_rows:?}"
    );
    assert!(
        first_rows.iter().any(|(name, _, _)| name == "gone_symbol"),
        "{mode} genesis must index gone_symbol: {first_rows:?}"
    );

    // Graph resume keeps the planned key and deletes by it. A document for
    // the same path under some other key (a previous `--fresh`) must go too.
    if !no_graph {
        let (status, body) = call(
            app,
            "POST",
            &format!("/{prefix}-*/_search"),
            json!({"size": 1, "query": {"match_all": {}}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let index = body["hits"]["hits"][0]["_index"]
            .as_str()
            .unwrap()
            .to_string();
        let (status, planted) = call(
            app,
            "PUT",
            &format!("/{index}/_doc/stale-plant"),
            json!({
                "ax_path": "jobs.rs",
                "ax_file": "stale-generation-key",
                "ax_locator": "code:1:query_job",
                "name": "query_job",
                "line": 1,
                "code": "pub fn query_job(group_id: &str, raw: &str) -> bool"
            }),
        )
        .await;
        assert!(
            status.is_success(),
            "plant stale symbol: {status} {planted}"
        );
    }

    std::fs::write(edited.path().join("jobs.rs"), CODE_V2).unwrap();
    let code = run_autoindex_blocking(
        edited.path(),
        edited_state.path(),
        url,
        &prefix,
        no_graph,
        false,
    )
    .await;
    assert_eq!(code, 0, "{mode} incremental reindex exit {code}");

    let fresh_root = tempfile::tempdir().unwrap();
    let fresh_state = tempfile::tempdir().unwrap();
    std::fs::write(fresh_root.path().join("jobs.rs"), CODE_V2).unwrap();
    let fresh_prefix = format!("fresh{mode}");
    let code = run_autoindex_blocking(
        fresh_root.path(),
        fresh_state.path(),
        url,
        &fresh_prefix,
        no_graph,
        false,
    )
    .await;
    assert_eq!(code, 0, "{mode} fresh index exit {code}");

    let live = prefix_docs(app, &prefix).await;
    let fresh = prefix_docs(app, &fresh_prefix).await;
    assert_only_current(&format!("{mode} incremental reindex"), &live, &fresh);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_code_file_reindex_drops_shifted_and_removed_symbols_on_the_engine() {
    let app = app().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = app.clone();
    tokio::spawn(async move {
        axum::serve(listener, served).await.unwrap();
    });
    let url = format!("http://{addr}");

    // The graph resume path is what the field reports ran. `--no-graph` is
    // the other incremental executor and has to drop the same documents.
    reindex_changed_code(&app, &url, false).await;
    reindex_changed_code(&app, &url, true).await;
    reindex_with_fresh(&app, &url).await;
}

/// `--fresh` rebuilds the content key. The previous symbol documents must
/// still disappear, and a from-scratch index of v2 is the oracle.
async fn reindex_with_fresh(app: &axum::Router, url: &str) {
    let edited = tempfile::tempdir().unwrap();
    let edited_state = tempfile::tempdir().unwrap();
    std::fs::write(edited.path().join("jobs.rs"), CODE_V1).unwrap();
    let prefix = "stalefresh";
    let code = run_autoindex_blocking(
        edited.path(),
        edited_state.path(),
        url,
        prefix,
        false,
        false,
    )
    .await;
    assert_eq!(code, 0, "fresh-path genesis exit {code}");

    std::fs::write(edited.path().join("jobs.rs"), CODE_V2).unwrap();
    let code =
        run_autoindex_blocking(edited.path(), edited_state.path(), url, prefix, false, true).await;
    assert_eq!(code, 0, "--fresh reindex exit {code}");

    let oracle_root = tempfile::tempdir().unwrap();
    let oracle_state = tempfile::tempdir().unwrap();
    std::fs::write(oracle_root.path().join("jobs.rs"), CODE_V2).unwrap();
    let code = run_autoindex_blocking(
        oracle_root.path(),
        oracle_state.path(),
        url,
        "oraclefresh",
        false,
        false,
    )
    .await;
    assert_eq!(code, 0, "oracle exit {code}");
    let live = prefix_docs(app, prefix).await;
    let oracle = prefix_docs(app, "oraclefresh").await;
    assert_only_current("--fresh reindex", &live, &oracle);
}
