//! Issue #2306: a semantic autoindex run whose embedder is `--embed-mode proxy`
//! must resume after a mid-run failure. Documents already embedded are not
//! sent to the proxy again when the backend, model, endpoint, and vector
//! width are unchanged.
//!
//! The mock answers OpenAI `/v1/embeddings` with 384-wide vectors (the width
//! `semantic_text` uses when the mapping omits `dimensions`). The identity
//! probe always succeeds. The first document embed succeeds and is indexed;
//! every later document embed is refused with HTTP 400 until the rerun turns
//! the refusal off. 400 is permanent, so the interrupted run fails without
//! retry backoff. Which file is first does not matter.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;
use xerj_autoindex::cli::IndexCfg;

const DIMS: usize = 384;
const ALPHA: &str = "ALPHA_DOC_ONE";
const BETA: &str = "BETA_DOC_TWO";
/// Same text `confirm_proxy_embedding_width` sends. Kept here so the interrupt
/// does not treat the identity probe as a document.
const PROBE: &str = "xerj-proxy-identity-probe";

fn prose(marker: &str) -> String {
    format!(
        "{marker} The quarterly revenue report describes how the operating team closed the books \
         after a long review of customer contracts and deferred revenue. The narrative is ordinary \
         prose so the indexer treats the file as a document and embeds the body once, instead of \
         splitting it into line windows."
    )
}

struct EmbedLog {
    texts: Mutex<Vec<String>>,
    /// Document embed requests that may still succeed while `reject` is set.
    allow_document_requests: AtomicUsize,
    reject: AtomicBool,
}

async fn mock_embedder() -> (String, Arc<EmbedLog>) {
    let log = Arc::new(EmbedLog {
        texts: Mutex::new(Vec::new()),
        allow_document_requests: AtomicUsize::new(1),
        reject: AtomicBool::new(true),
    });
    let shared = Arc::clone(&log);
    let app = axum::Router::new().route(
        "/v1/embeddings",
        axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            let shared = Arc::clone(&shared);
            async move {
                let inputs: Vec<String> = match body.get("input") {
                    Some(Value::Array(items)) => items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_owned))
                        .collect(),
                    Some(Value::String(text)) => vec![text.clone()],
                    _ => Vec::new(),
                };
                shared.texts.lock().unwrap().extend(inputs.iter().cloned());
                let document = inputs.iter().any(|text| !text.contains(PROBE));
                let allowed = !document
                    || !shared.reject.load(Ordering::SeqCst)
                    || shared
                        .allow_document_requests
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                            left.checked_sub(1)
                        })
                        .is_ok();
                if !allowed {
                    return (
                        StatusCode::BAD_REQUEST,
                        axum::Json(json!({"error": "interrupted"})),
                    );
                }
                let data: Vec<Value> = inputs
                    .iter()
                    .enumerate()
                    .map(|(index, _)| {
                        let mut embedding = vec![0.0_f64; DIMS];
                        embedding[0] = 1.0;
                        json!({"index": index, "embedding": embedding})
                    })
                    .collect();
                (StatusCode::OK, axum::Json(json!({"data": data})))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock embedder");
    let addr = listener.local_addr().expect("mock embedder addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/v1/embeddings"), log)
}

async fn proxy_node(endpoint: String) -> (axum::Router, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.auth.enabled = false;
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config.embedding.mode = "proxy".into();
    config.embedding.default_endpoint = endpoint;
    config.embedding.default_model = "mock-embed".into();
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    let state = xerj_api::state::AppState::new(config, engine, metrics);
    std::mem::forget(dir);
    let app = xerj_api::router::build_es_compat_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind xerj");
    let addr = listener.local_addr().expect("xerj addr");
    let served = app.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, served).await;
    });
    (app, format!("http://{addr}"))
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

fn index_cfg(root: &Path, state_dir: &Path, url: &str, prefix: &str, no_graph: bool) -> IndexCfg {
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
        fresh: false,
        follow_symlinks: false,
        follow_symlinks_outside_root: false,
        ignore: xerj_autoindex::ignore_rules::IgnoreOptions::default(),
        max_file_bytes: 1 << 30,
        sample: 50,
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
) -> Result<i32, String> {
    let cfg = index_cfg(root, state_dir, url, prefix, no_graph);
    match xerj_autoindex::run_index_report(cfg) {
        Ok((code, _)) => Ok(code),
        Err(error) => Err(format!("{error:#}")),
    }
}

async fn run_autoindex_blocking(
    root: &Path,
    state_dir: &Path,
    url: &str,
    prefix: &str,
    no_graph: bool,
) -> Result<i32, String> {
    let root = root.to_owned();
    let state_dir = state_dir.to_owned();
    let url = url.to_owned();
    let prefix = prefix.to_owned();
    tokio::task::spawn_blocking(move || run_autoindex(&root, &state_dir, &url, &prefix, no_graph))
        .await
        .expect("autoindex task")
}

fn texts_containing(log: &[String], marker: &str) -> usize {
    log.iter().filter(|text| text.contains(marker)).count()
}

async fn indexed_markers(app: &axum::Router, prefix: &str) -> (bool, bool) {
    let (refresh_status, refresh_body) =
        call(app, "POST", &format!("/{prefix}-*/_refresh"), Value::Null).await;
    assert!(
        refresh_status.is_success(),
        "refresh {prefix}: {refresh_status} {refresh_body}"
    );
    let (status, body) = call(
        app,
        "POST",
        &format!("/{prefix}-*/_search"),
        json!({"size": 50, "query": {"match_all": {}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "search {prefix}: {body}");
    let rendered = body.to_string();
    (rendered.contains(ALPHA), rendered.contains(BETA))
}

async fn interrupt_then_resume(no_graph: bool) {
    let (endpoint, log) = mock_embedder().await;
    let (app, url) = proxy_node(endpoint).await;
    let corpus = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(corpus.path().join("a-alpha.txt"), prose(ALPHA)).unwrap();
    std::fs::write(corpus.path().join("b-beta.txt"), prose(BETA)).unwrap();
    let prefix = if no_graph { "pxresumeng" } else { "pxresume" };

    let interrupted = run_autoindex_blocking(corpus.path(), state.path(), &url, prefix, no_graph)
        .await
        .expect_err("the refused document must stop the run");
    assert!(
        interrupted.contains("embedding") || interrupted.contains("bulk"),
        "the failure must name the embedding problem: {interrupted}"
    );

    let during = log.texts.lock().unwrap().clone();
    assert!(
        texts_containing(&during, ALPHA) >= 1 && texts_containing(&during, BETA) >= 1,
        "the run must embed one document and reach the next before it stops: {during:?}"
    );
    let (alpha_indexed, beta_indexed) = indexed_markers(&app, prefix).await;
    assert!(
        alpha_indexed != beta_indexed,
        "exactly one document is indexed before the refusal \
         (alpha={alpha_indexed} beta={beta_indexed})"
    );
    let already = if alpha_indexed { ALPHA } else { BETA };
    let pending = if alpha_indexed { BETA } else { ALPHA };

    let cutoff = during.len();
    log.reject.store(false, Ordering::SeqCst);
    let code = run_autoindex_blocking(corpus.path(), state.path(), &url, prefix, no_graph)
        .await
        .unwrap_or_else(|error| panic!("resume failed: {error}"));
    assert_eq!(code, 0, "resume must finish");

    let resumed = log.texts.lock().unwrap().clone();
    let again = &resumed[cutoff..];
    assert_eq!(
        texts_containing(again, already),
        0,
        "already-indexed documents must not be re-embedded: {again:?}"
    );
    assert!(
        texts_containing(again, pending) >= 1,
        "the document that failed must be embedded on the rerun: {again:?}"
    );
    let (alpha_indexed, beta_indexed) = indexed_markers(&app, prefix).await;
    assert!(
        alpha_indexed && beta_indexed,
        "both documents are searchable after resume (alpha={alpha_indexed} beta={beta_indexed})"
    );
}

#[tokio::test]
async fn proxy_semantic_autoindex_resumes_without_reembedding() {
    interrupt_then_resume(false).await;
}

#[tokio::test]
async fn proxy_semantic_autoindex_resumes_without_reembedding_no_graph() {
    interrupt_then_resume(true).await;
}
