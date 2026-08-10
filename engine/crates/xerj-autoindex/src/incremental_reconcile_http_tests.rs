//! Real-HTTP end-to-end coverage for incremental corpus generations.
//!
//! The endpoint below is deliberately stateful. It applies bulk index/update
//! actions, delete-by-query, and exact count searches so `run_index` traverses
//! the same HTTP client and validation barriers as a real server.

use super::*;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

static HTTP_E2E_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct HttpState {
    docs: HashMap<(String, String), Value>,
    requests: Vec<(String, String)>,
    data_bulk_requests: usize,
    fail_next_data_bulk: bool,
    /// Apply the first half of the next data bulk's items and *then* report
    /// `errors: true`. This is the dangerous backend shape the legacy
    /// replacement transaction was hardened against (visibility changed, but
    /// the durable record must not be): `no_graph` corpora take the generated
    /// executor now, so the shape has to be proven here too.
    partially_apply_next_data_bulk: bool,
    fail_embedding_identity: bool,
    stop: bool,
    embedding_identity_sha256: String,
    saw_dataset_mapping_update: bool,
    saw_catalog_mapping_update: bool,
}

struct HttpEndpoint {
    url: String,
    state: Arc<Mutex<HttpState>>,
    join: Option<thread::JoinHandle<()>>,
}

impl HttpEndpoint {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(HttpState {
            embedding_identity_sha256: "a".repeat(64),
            ..HttpState::default()
        }));
        let server_state = Arc::clone(&state);
        let join = thread::spawn(move || loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    // BSD/macOS: accepted sockets inherit the listener's
                    // O_NONBLOCK; the handler does blocking reads.
                    stream.set_nonblocking(false).unwrap();
                    handle_http(stream, &server_state)
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if server_state.lock().unwrap().stop {
                        break;
                    }
                    thread::yield_now();
                }
                Err(error) => panic!("HTTP test endpoint accept failed: {error}"),
            }
        });
        Self {
            url,
            state,
            join: Some(join),
        }
    }

    fn data_docs(&self) -> Vec<Value> {
        self.state
            .lock()
            .unwrap()
            .docs
            .iter()
            .filter(|((index, _), _)| index != catalog::CATALOG_INDEX)
            .map(|(_, doc)| doc.clone())
            .collect()
    }

    fn data_bulk_requests(&self) -> usize {
        self.state.lock().unwrap().data_bulk_requests
    }
}

impl Drop for HttpEndpoint {
    fn drop(&mut self) {
        self.state.lock().unwrap().stop = true;
        let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
        self.join.take().unwrap().join().unwrap();
    }
}

fn handle_http(mut stream: TcpStream, state: &Arc<Mutex<HttpState>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap() == 0 {
        return;
    }
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some(value) = line
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .map(str::trim)
        {
            content_length = value.parse().unwrap();
        }
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).unwrap();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    state
        .lock()
        .unwrap()
        .requests
        .push((method.to_owned(), path.to_owned()));

    let (status, response) = if method == "GET" && path == "/v1/embedding/identity" {
        let locked = state.lock().unwrap();
        if locked.fail_embedding_identity {
            (
                500,
                json!({"error": {
                    "type": "injected_identity_failure",
                    "reason": "genesis recovery boundary"
                }}),
            )
        } else {
            (
                200,
                json!({"data": {
                    "version": 1,
                    "backend": "lexical",
                    "identity_sha256": locked.embedding_identity_sha256.clone(),
                    "dimensions": 384,
                    "semantic_contract": "semantic_text-derived-vector.v1",
                    "resumable": true
                }, "took_ms": 0, "request_id": "incremental-http-test"}),
            )
        }
    } else if method == "POST" && path == "/_bulk" {
        let locked = state.lock().unwrap();
        assert!(
            locked.saw_dataset_mapping_update && locked.saw_catalog_mapping_update,
            "bulk publication preceded exact generation mapping updates"
        );
        drop(locked);
        bulk_http(&body, state)
    } else if method == "POST" && path.contains("/_delete_by_query") {
        let deleted = delete_http(path, &body, state);
        (200, json!({"deleted": deleted, "failures": []}))
    } else if method == "POST" && path.ends_with("/_search") {
        (200, search_http(path, &body, state))
    } else if method == "GET" && path.ends_with("/_count") {
        let index = path.trim_start_matches('/').trim_end_matches("/_count");
        let count = state
            .lock()
            .unwrap()
            .docs
            .keys()
            .filter(|(candidate, _)| candidate == index)
            .count();
        (200, json!({"count": count}))
    } else if method == "PUT" {
        let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let mut locked = state.lock().unwrap();
        if path.ends_with("/_mapping") {
            assert!(value.get("properties").is_some());
            assert!(value.get("mappings").is_none());
            if path.starts_with(&format!("/{}/", catalog::CATALOG_INDEX)) {
                locked.saw_catalog_mapping_update = true;
            } else {
                locked.saw_dataset_mapping_update = true;
            }
        } else {
            assert!(value.pointer("/mappings/properties").is_some());
            assert!(value.get("properties").is_none());
        }
        (200, json!({"acknowledged": true}))
    } else {
        // Readiness, index creation/mapping, and refresh.
        (200, json!({"acknowledged": true}))
    };
    let bytes = response.to_string();
    write!(
        stream,
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        if status == 200 {
            "OK"
        } else {
            "Internal Server Error"
        },
        bytes.len(),
        bytes
    )
    .unwrap();
}

fn bulk_http(body: &[u8], state: &Arc<Mutex<HttpState>>) -> (u16, Value) {
    let lines: Vec<&[u8]> = body
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect();
    let is_data = lines.iter().step_by(2).any(|line| {
        serde_json::from_slice::<Value>(line)
            .ok()
            .and_then(|action| {
                action
                    .pointer("/index/_index")
                    .or_else(|| action.pointer("/update/_index"))
                    .and_then(Value::as_str)
                    .map(|index| index != catalog::CATALOG_INDEX)
            })
            .unwrap_or(false)
    });
    let mut locked = state.lock().unwrap();
    if is_data {
        locked.data_bulk_requests += 1;
        if std::mem::take(&mut locked.fail_next_data_bulk) {
            return (
                200,
                json!({
                    "errors": true,
                    "items": [{"index": {
                        "status": 500,
                        "error": {
                            "type": "injected_failure",
                            "reason": "leave durable generation pending"
                        }
                    }}]
                }),
            );
        }
    }
    // Half-applied bulk: apply the leading actions, then report failure.
    let applied_limit = if is_data && std::mem::take(&mut locked.partially_apply_next_data_bulk) {
        Some(lines.len() / 2)
    } else {
        None
    };
    let mut cursor = 0;
    while cursor < lines.len() {
        if applied_limit.is_some_and(|limit| cursor >= limit) {
            break;
        }
        let action: Value = serde_json::from_slice(lines[cursor]).unwrap();
        cursor += 1;
        if let Some(meta) = action.get("delete") {
            let index = meta["_index"].as_str().unwrap().to_owned();
            let id = meta["_id"].as_str().unwrap().to_owned();
            locked.docs.remove(&(index, id));
            continue;
        }
        let payload: Value = serde_json::from_slice(lines[cursor]).unwrap();
        cursor += 1;
        if let Some(meta) = action.get("index") {
            let index = meta["_index"].as_str().unwrap().to_owned();
            let id = meta["_id"].as_str().unwrap().to_owned();
            locked.docs.insert((index, id), payload);
        } else if let Some(meta) = action.get("update") {
            let index = meta["_index"].as_str().unwrap().to_owned();
            let id = meta["_id"].as_str().unwrap().to_owned();
            let patch = payload["doc"].as_object().unwrap();
            let target = locked.docs.get_mut(&(index, id)).unwrap();
            let target = target.as_object_mut().unwrap();
            target.extend(patch.clone());
        }
    }
    if applied_limit.is_some() {
        return (
            200,
            json!({
                "errors": true,
                "items": [{"index": {
                    "status": 500,
                    "error": {
                        "type": "injected_failure",
                        "reason": "partial bulk applied before failing"
                    }
                }}]
            }),
        );
    }
    (200, json!({"errors": false, "items": []}))
}

fn delete_http(path: &str, body: &[u8], state: &Arc<Mutex<HttpState>>) -> usize {
    let index = path
        .trim_start_matches('/')
        .split("/_delete_by_query")
        .next()
        .unwrap();
    let query: Value = serde_json::from_slice(body).unwrap();
    let term = query
        .pointer("/query/term")
        .and_then(Value::as_object)
        .and_then(|term| term.iter().next())
        .map(|(field, value)| (field.clone(), value.clone()));
    let mut locked = state.lock().unwrap();
    let before = locked.docs.len();
    locked.docs.retain(|(candidate, _), doc| {
        if candidate != index {
            return true;
        }
        let Some((field, expected)) = &term else {
            return false;
        };
        doc.get(field) != Some(expected)
    });
    before - locked.docs.len()
}

fn search_http(path: &str, body: &[u8], state: &Arc<Mutex<HttpState>>) -> Value {
    let indices = path
        .trim_start_matches('/')
        .trim_end_matches("/_search")
        .split(',')
        .collect::<Vec<_>>();
    let query: Value = serde_json::from_slice(body).unwrap();
    let term = query
        .pointer("/query/term")
        .or_else(|| query.pointer("/query/bool/must/0/term"))
        .and_then(Value::as_object)
        .and_then(|term| term.iter().next());
    let terms = query
        .pointer("/query/terms")
        .and_then(Value::as_object)
        .and_then(|terms| terms.iter().next());
    let filters = query
        .pointer("/query/bool/filter")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let exists = query
        .pointer("/query/bool/must/1/exists/field")
        .and_then(Value::as_str);
    let locked = state.lock().unwrap();
    let matching: Vec<_> = locked
        .docs
        .iter()
        .filter(|((index, _), doc)| {
            indices.contains(&index.as_str())
                && term
                    .map(|(field, value)| doc.get(field) == Some(value))
                    .unwrap_or(true)
                && terms
                    .map(|(field, values)| {
                        values.as_array().is_some_and(|values| {
                            values.iter().any(|value| doc.get(field) == Some(value))
                        })
                    })
                    .unwrap_or(true)
                && filters.iter().all(|filter| {
                    filter
                        .get("term")
                        .and_then(Value::as_object)
                        .and_then(|term| term.iter().next())
                        .is_none_or(|(field, value)| doc.get(field) == Some(value))
                })
                && exists.map(|field| doc.get(field).is_some()).unwrap_or(true)
        })
        .collect();
    let hits = matching
        .iter()
        .map(|((_, id), source)| json!({"_id": id, "_source": source}))
        .collect::<Vec<_>>();
    json!({
        "hits": {
            "total": {"value": matching.len(), "relation": "eq"},
            "hits": hits
        },
        "aggregations": {}
    })
}

fn cfg(root: &Path, state_dir: &Path, url: &str, semantic: bool) -> IndexCfg {
    IndexCfg {
        root: root.to_owned(),
        stub_globs: Vec::new(),
        url: url.to_owned(),
        api_key: None,
        workers: 1,
        scan_workers: 1,
        pdf_workers: 1,
        resource_notes: Vec::new(),
        pdf_timeout_secs: 30,
        bulk_mb: 1,
        bulk_timeout_secs: 30,
        snapshot_max_bytes: 64 << 30,
        prefix: "incremental-http".into(),
        state_dir: Some(state_dir.to_owned()),
        fresh: false,
        follow_symlinks: false,
        ignore: crate::ignore_rules::IgnoreOptions::default(),
        max_file_gb: 1,
        sample: 50,
        no_semantic: !semantic,
        brain: None,
        no_graph: true,
        dry_run: false,
        json: false,
        quiet: true,
        progress: crate::progress::ProgressMode::None,
        progress_interval: None,
    }
}

fn journal_events(state_dir: &Path, kind: &str) -> usize {
    fs::read_to_string(state_dir.join("journal.ndjson"))
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event.get("kind").and_then(Value::as_str) == Some(kind))
        .count()
}

fn paths(docs: &[Value]) -> Vec<String> {
    let mut paths: Vec<_> = docs
        .iter()
        .filter_map(|doc| doc.get("ax_path").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

fn snapshot_names(state_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(state_dir.join("sync-snapshots"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_type().is_ok_and(|kind| kind.is_dir())
                        && !entry.file_name().to_string_lossy().starts_with('.')
                })
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn final_snapshot_count(state_dir: &Path) -> usize {
    fs::read_dir(state_dir.join("sync-snapshots"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_type().is_ok_and(|kind| kind.is_dir())
                        && !entry.file_name().to_string_lossy().starts_with('.')
                })
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn genesis_recovers_after_snapshot_budget_failure_before_sync_begin() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap();
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    config.snapshot_max_bytes = 1;

    let error = run_index(config.clone()).unwrap_err();
    assert!(format!("{error:#}").contains("snapshot source footprint"));
    assert_eq!(journal_events(state_dir.path(), "sync_bootstrap"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 0);
    assert_eq!(endpoint.data_bulk_requests(), 0);

    config.snapshot_max_bytes = 64 << 30;
    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_bootstrap"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    assert_eq!(endpoint.data_docs().len(), 1);
}

#[test]
fn no_semantic_generation_does_not_require_embedding_identity_endpoint() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap();
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    endpoint.state.lock().unwrap().fail_embedding_identity = true;

    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_bootstrap"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    assert_eq!(journal_events(state_dir.path(), "finish"), 1);
    assert_eq!(final_snapshot_count(state_dir.path()), 1);
    assert_eq!(endpoint.data_docs().len(), 1);
}

#[test]
fn initial_generation_and_noop_are_end_to_end_idempotent() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap();
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n2,beta\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (code, summary) = run_index_report(config.clone()).unwrap();
    assert_eq!(code, 0);
    let summary = summary.expect("generated run must return its committed run projection");
    assert_eq!(summary["generation"], 1);
    assert_eq!(summary["records_total"], 2);
    let first_docs = endpoint.data_docs();
    assert_eq!(first_docs.len(), 2);
    assert_eq!(paths(&first_docs), ["a.csv"]);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    let root = corpus.path().canonicalize().unwrap();
    let preflight = state::Journal::preflight(
        state_dir.path(),
        &root.to_string_lossy(),
        &endpoint.url,
        "incremental-http",
        false,
    )
    .unwrap();
    assert!(
        preflight.committed_manifest.is_some(),
        "sync_commit must be visible to the next preflight"
    );
    drop(preflight);

    let bulks = endpoint.data_bulk_requests();
    let (_, noop_summary) = run_index_report(config).unwrap();
    assert_eq!(noop_summary.unwrap()["generation"], 1);
    assert_eq!(endpoint.data_docs(), first_docs);
    assert_eq!(endpoint.data_bulk_requests(), bulks);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    assert_eq!(journal_events(state_dir.path(), "finish"), 2);
}

#[test]
fn add_change_delete_and_rename_converge_over_real_http() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap();
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    fs::write(corpus.path().join("b.csv"), "id,value\n2,bravo\n").unwrap();
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    assert_eq!(paths(&endpoint.data_docs()), ["a.csv", "b.csv"]);

    fs::write(
        corpus.path().join("a.csv"),
        "id,value\n1,alpha-new\n3,charlie\n",
    )
    .unwrap();
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    assert_eq!(endpoint.data_docs().len(), 3);

    fs::remove_file(corpus.path().join("b.csv")).unwrap();
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    assert_eq!(paths(&endpoint.data_docs()), ["a.csv"]);

    fs::rename(
        corpus.path().join("a.csv"),
        corpus.path().join("renamed.csv"),
    )
    .unwrap();
    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(paths(&endpoint.data_docs()), ["renamed.csv"]);
    assert_eq!(endpoint.data_docs().len(), 2);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 5);
}

#[test]
fn pending_generation_replays_sealed_source_after_live_source_mutates() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap();
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let source = corpus.path().join("a.csv");
    fs::write(&source, "id,value\n1,committed\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    fs::write(&source, "id,value\n1,sealed-pending\n2,sealed-second\n").unwrap();
    endpoint.state.lock().unwrap().fail_next_data_bulk = true;
    assert!(run_index(config.clone()).is_err());
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 2);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    fs::write(&source, "id,value\n9,mutated-after-sync-begin\n").unwrap();
    assert_eq!(run_index(config).unwrap(), 0);
    let docs = endpoint.data_docs();
    assert_eq!(docs.len(), 2);
    let rendered = serde_json::to_string(&docs).unwrap();
    assert!(rendered.contains("sealed-pending"), "{rendered}");
    assert!(rendered.contains("sealed-second"), "{rendered}");
    assert!(!rendered.contains("mutated-after-sync-begin"), "{rendered}");
}

/// Crash/resume with a *partially applied* data bulk, on `--no-graph`.
///
/// `failure_resume_http_tests` used to run its whole injected-failure matrix
/// with `no_graph: true`; since `--no-graph` now takes the generated executor,
/// that module keeps the legacy graph-enabled transaction and this test carries
/// the no-graph half of the coverage. The endpoint applies half of a bulk and
/// *then* reports failure — visibility has changed, so the generation must stay
/// uncommitted, and the retry must converge on exactly the new content with no
/// duplicate or stale document surviving the half-applied attempt.
#[test]
fn partially_applied_data_bulk_leaves_the_generation_pending_and_the_retry_converges() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap();
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let source = corpus.path().join("rows.csv");
    fs::write(&source, "id,value\n1,first\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    fs::write(
        &source,
        "id,value\n1,replaced\n2,second\n3,third\n4,fourth\n",
    )
    .unwrap();
    endpoint
        .state
        .lock()
        .unwrap()
        .partially_apply_next_data_bulk = true;
    assert!(
        run_index(config.clone()).is_err(),
        "a partially applied bulk must fail the run, not be absorbed"
    );
    assert_eq!(
        journal_events(state_dir.path(), "sync_begin"),
        2,
        "the generation was begun"
    );
    assert_eq!(
        journal_events(state_dir.path(), "sync_commit"),
        1,
        "a partially applied bulk must never commit the generation"
    );

    // Retry: the sealed source replays, and the destination converges exactly.
    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 2);
    let docs = endpoint.data_docs();
    assert_eq!(paths(&docs), ["rows.csv"]);
    assert_eq!(
        docs.len(),
        4,
        "exactly the four new records survive — no half-applied duplicate, no stale first record"
    );
    let rendered = serde_json::to_string(&docs).unwrap();
    assert!(rendered.contains("replaced"), "{rendered}");
    assert!(rendered.contains("fourth"), "{rendered}");
    assert!(
        !rendered.contains("\"value\":\"first\""),
        "the replaced record must not survive: {rendered}"
    );
}

#[test]
fn pending_semantic_identity_drift_rejects_before_any_further_bulk() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap();
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let source = corpus.path().join("report.txt");
    fs::write(
        &source,
        "Initial quarterly report discusses durable subscription revenue and operating income.",
    )
    .unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, true);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    fs::write(
        &source,
        "Updated quarterly report discusses materially stronger subscription revenue and margin.",
    )
    .unwrap();
    endpoint.state.lock().unwrap().fail_next_data_bulk = true;
    assert!(run_index(config.clone()).is_err());
    let bulks_before = endpoint.data_bulk_requests();
    endpoint.state.lock().unwrap().embedding_identity_sha256 = "b".repeat(64);

    let error = run_index(config).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("different embedding execution identity"),
        "{message}"
    );
    assert!(
        message.contains("no remote mutation was attempted"),
        "{message}"
    );
    assert_eq!(
        endpoint.data_bulk_requests(),
        bulks_before,
        "identity drift must be rejected before another data bulk"
    );
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
}

/// `--fresh` must be refused before it can destroy generation authority.
///
/// `Journal::open_after_preflight` deletes `journal.ndjson` whenever `fresh` is
/// set, and `gc_snapshots` then sees an empty protected set and removes every
/// sealed snapshot. Both run inside the generated branches, ahead of the legacy
/// plan gate, so without the preflight refusal a single `--fresh` erases the
/// committed manifest and every sealed source the next run needs.
#[test]
fn fresh_cannot_destroy_a_committed_generation() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap();
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    fs::write(corpus.path().join("b.csv"), "id,value\n2,bravo\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    let committed_docs = endpoint.data_docs();
    assert_eq!(paths(&committed_docs), ["a.csv", "b.csv"]);

    let journal_path = state_dir.path().join("journal.ndjson");
    let journal_before = fs::read(&journal_path).unwrap();
    let snapshots_before = snapshot_names(state_dir.path());
    assert_eq!(snapshots_before.len(), 1, "{snapshots_before:?}");
    let requests_before = endpoint.state.lock().unwrap().requests.len();

    // The folder also changed, which is exactly when a user reaches for
    // --fresh.
    fs::remove_file(corpus.path().join("b.csv")).unwrap();
    config.fresh = true;
    let error = run_index(config.clone()).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("`--fresh` cannot discard committed corpus generation 1"),
        "the refusal must name the durable state that blocks it: {message}"
    );
    assert!(
        message.contains("without `--fresh`"),
        "the refusal must point at the incremental path that does work: {message}"
    );

    assert_eq!(
        fs::read(&journal_path).unwrap(),
        journal_before,
        "a refused --fresh must not rewrite the journal"
    );
    assert_eq!(
        snapshot_names(state_dir.path()),
        snapshots_before,
        "a refused --fresh must not garbage-collect sealed snapshots"
    );
    let attempted = endpoint.state.lock().unwrap().requests[requests_before..].to_vec();
    assert_eq!(
        attempted,
        [("GET".to_owned(), "/".to_owned())],
        "a refused --fresh may perform only the endpoint-readiness GET"
    );

    // The surviving authority still reconciles the real folder change.
    config.fresh = false;
    let (code, summary) = run_index_report(config).unwrap();
    assert_eq!(code, 0);
    assert_eq!(summary.unwrap()["generation"], 2);
    assert_eq!(paths(&endpoint.data_docs()), ["a.csv"]);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 2);
}

/// Every refusal on the generated path now sends the operator to an isolated
/// rebuild, so that recipe has to work. `--fresh` is no longer a rebuild-in-
/// place escape hatch, and a new `--prefix` alone is not enough when the state
/// directory was named explicitly: `preflight` refuses a journal recorded for a
/// different prefix before the run reaches anything else.
#[test]
fn the_isolated_rebuild_the_refusals_recommend_is_followable() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap();
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let committed_state = tempfile::tempdir().unwrap();
    let rebuild_state = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), committed_state.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    // A new --prefix on its own cannot rebuild: the recorded journal owns it.
    let mut prefix_only = config.clone();
    prefix_only.prefix = "incremental-http-rebuild".into();
    let message = format!("{:#}", run_index(prefix_only).unwrap_err());
    assert!(message.contains("was created for root="), "{message}");

    // A new --state-dir and a new --prefix together do rebuild, and leave the
    // original generation and its sealed snapshot untouched.
    let journal_before = fs::read(committed_state.path().join("journal.ndjson")).unwrap();
    let snapshots_before = snapshot_names(committed_state.path());
    let mut rebuild = cfg(corpus.path(), rebuild_state.path(), &endpoint.url, false);
    rebuild.prefix = "incremental-http-rebuild".into();
    let (code, summary) = run_index_report(rebuild).unwrap();
    assert_eq!(code, 0);
    assert_eq!(summary.unwrap()["generation"], 1);
    assert_eq!(journal_events(rebuild_state.path(), "sync_commit"), 1);
    assert_eq!(
        fs::read(committed_state.path().join("journal.ndjson")).unwrap(),
        journal_before,
        "an isolated rebuild must not touch the original journal"
    );
    assert_eq!(
        snapshot_names(committed_state.path()),
        snapshots_before,
        "an isolated rebuild must not touch the original sealed snapshots"
    );
}

/// The same refusal on a crashed generation, where the journal additionally
/// holds the only record of the pending transaction and its sealed source.
#[test]
fn fresh_cannot_destroy_a_pending_generation() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap();
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let source = corpus.path().join("a.csv");
    fs::write(&source, "id,value\n1,committed\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    fs::write(&source, "id,value\n1,sealed-pending\n2,sealed-second\n").unwrap();
    endpoint.state.lock().unwrap().fail_next_data_bulk = true;
    assert!(run_index(config.clone()).is_err());
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 2);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    let journal_path = state_dir.path().join("journal.ndjson");
    let journal_before = fs::read(&journal_path).unwrap();
    let snapshots_before = snapshot_names(state_dir.path());
    assert_eq!(snapshots_before.len(), 2, "{snapshots_before:?}");
    let requests_before = endpoint.state.lock().unwrap().requests.len();

    config.fresh = true;
    let error = run_index(config.clone()).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("`--fresh` cannot discard an uncommitted pending corpus generation"),
        "the refusal must name the durable state that blocks it: {message}"
    );
    assert_eq!(
        fs::read(&journal_path).unwrap(),
        journal_before,
        "a refused --fresh must not rewrite a pending journal"
    );
    assert_eq!(
        snapshot_names(state_dir.path()),
        snapshots_before,
        "a refused --fresh must not delete the pending sealed source"
    );
    let attempted = endpoint.state.lock().unwrap().requests[requests_before..].to_vec();
    assert_eq!(
        attempted,
        [("GET".to_owned(), "/".to_owned())],
        "a refused --fresh may perform only the endpoint-readiness GET"
    );

    // The pending transaction is still resumable from its sealed source.
    config.fresh = false;
    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 2);
    let rendered = serde_json::to_string(&endpoint.data_docs()).unwrap();
    assert!(rendered.contains("sealed-pending"), "{rendered}");
    assert!(rendered.contains("sealed-second"), "{rendered}");
}

/// Two runs started in the same process and the same UTC second must not share
/// a `run_id`.
///
/// The generation catalog keys its managed documents on `run_id`, so a
/// collision makes the second run read the first run's documents back as its
/// own generation. `xerj brain` calls `run_index_report` more than once per
/// process, so this is reachable without any concurrency.
#[test]
fn same_second_runs_in_one_process_do_not_share_a_generation_identity() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap();
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let first_state = tempfile::tempdir().unwrap();
    let second_state = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    let endpoint = HttpEndpoint::start();

    assert_eq!(
        run_index(cfg(corpus.path(), first_state.path(), &endpoint.url, false)).unwrap(),
        0
    );
    let second = run_index_report(cfg(
        corpus.path(),
        second_state.path(),
        &endpoint.url,
        false,
    ));
    let (code, summary) = second.expect("a second run in the same second must commit");
    assert_eq!(code, 0);
    assert_eq!(summary.unwrap()["generation"], 1);

    let run_id = |state_dir: &Path| -> String {
        fs::read_to_string(state_dir.join("journal.ndjson"))
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find_map(|event| {
                event
                    .get("run_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .expect("journal records a run_id")
    };
    assert_ne!(run_id(first_state.path()), run_id(second_state.path()));
}

/// An upsert must clear the desired content identity as well as the committed
/// one before it replays.
///
/// A byte-identical retry of the same prepared stream is idempotent on its own,
/// because `ids::doc_id` is derived from dataset slug, content id and locator.
/// The guard earns its keep when a prior aborted attempt left documents under
/// the desired `ax_file` whose ids the current stream does not overwrite — an
/// earlier generation of the same content under a different dataset
/// assignment, or a partially rolled-back attempt. This test models that
/// survivor directly, because the exact record validation at the commit
/// barrier is what turns such a leftover into a hard, unrecoverable failure.
#[test]
fn upsert_clears_a_survivor_under_the_desired_content_identity() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap();
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let source = corpus.path().join("a.csv");
    let original = "id,value\n1,alpha\n";
    fs::write(&source, original).unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    let (index, content_id) = {
        let locked = endpoint.state.lock().unwrap();
        let ((index, _), doc) = locked
            .docs
            .iter()
            .find(|((index, _), _)| index != catalog::CATALOG_INDEX)
            .expect("the first generation published a data document");
        (
            index.clone(),
            doc.get("ax_file")
                .and_then(Value::as_str)
                .expect("data documents carry ax_file")
                .to_owned(),
        )
    };

    // Replace the content, then restore it, so the third generation upserts the
    // original content identity again.
    fs::write(&source, "id,value\n2,bravo\n").unwrap();
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    fs::write(&source, original).unwrap();

    // A document from an aborted earlier attempt at exactly this content, under
    // an id the prepared stream will not overwrite.
    endpoint.state.lock().unwrap().docs.insert(
        (index.clone(), "aborted-attempt-survivor".to_owned()),
        json!({
            "id": 1,
            "value": "alpha",
            "ax_file": content_id,
            "ax_path": "a.csv",
            "ax_locator": "aborted-attempt-survivor"
        }),
    );

    assert_eq!(run_index(config).unwrap(), 0);
    assert!(
        !endpoint
            .state
            .lock()
            .unwrap()
            .docs
            .contains_key(&(index, "aborted-attempt-survivor".to_owned())),
        "the upsert must delete the desired content identity before replaying it"
    );
    assert_eq!(paths(&endpoint.data_docs()), ["a.csv"]);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 3);
}

/// `--dry-run` used to be consulted only *after* the generated branches had
/// already published and committed, so on any already-generated state
/// directory the flag was accepted, ignored, and the destination mutated.
#[test]
fn dry_run_on_a_generated_state_directory_publishes_nothing() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap();
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    let committed_docs = endpoint.data_docs();
    let bulks = endpoint.data_bulk_requests();
    let journal_before = fs::read(state_dir.path().join("journal.ndjson")).unwrap();

    // A real change, so the dry run has something it would publish.
    fs::write(
        corpus.path().join("a.csv"),
        "id,value\n1,CHANGED\n2,added\n",
    )
    .unwrap();
    let mut dry = config.clone();
    dry.dry_run = true;
    let (code, summary) = run_index_report(dry.clone()).unwrap();
    assert_eq!(code, 0);
    assert!(
        summary.is_none(),
        "--dry-run must not report a committed run summary"
    );
    assert_eq!(
        endpoint.data_bulk_requests(),
        bulks,
        "--dry-run issued a data bulk"
    );
    assert_eq!(
        endpoint.data_docs(),
        committed_docs,
        "--dry-run mutated the destination"
    );
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    assert_eq!(
        fs::read(state_dir.path().join("journal.ndjson")).unwrap(),
        journal_before,
        "--dry-run appended to the durable journal"
    );

    // An unchanged folder takes the no-op arm of the same branch: still inert.
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    assert_eq!(run_index(dry).unwrap(), 0);
    assert_eq!(endpoint.data_bulk_requests(), bulks);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    // And the real run that follows still converges on the change.
    fs::write(
        corpus.path().join("a.csv"),
        "id,value\n1,CHANGED\n2,added\n",
    )
    .unwrap();
    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(endpoint.data_docs().len(), 2);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 2);
}

/// A junk/skipped file has no `plan.files` entry by construction, so demanding
/// a prepared artifact for every inventory entry failed the whole run on any
/// folder containing one unreadable, empty or unrecognised file.
#[test]
fn a_junk_file_is_recorded_and_never_fatal() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap();
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n2,beta\n").unwrap();
    fs::write(corpus.path().join("empty.csv"), "").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (code, summary) = run_index_report(config.clone()).unwrap();
    let summary = summary.expect("a corpus with one junk file still commits a generation");
    assert_eq!(
        code, 3,
        "a completed run that skipped a file exits 3, not 0 (cli.rs EXIT CODES)"
    );
    assert_eq!(summary["generation"], 1);
    assert_eq!(summary["files_junk"], 1);
    assert_eq!(summary["records_total"], 2);
    assert_eq!(paths(&endpoint.data_docs()), ["a.csv"]);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    let junk_docs: Vec<Value> = endpoint
        .state
        .lock()
        .unwrap()
        .docs
        .iter()
        .filter(|((index, _), doc)| {
            index == catalog::CATALOG_INDEX
                && doc.get("doc_kind").and_then(Value::as_str) == Some("file")
                && doc.get("path").and_then(Value::as_str) == Some("empty.csv")
        })
        .map(|(_, doc)| doc.clone())
        .collect();
    assert_eq!(
        junk_docs.len(),
        1,
        "the skipped file must still get exactly one catalog entry"
    );

    // The generation reconciles normally afterwards, junk file and all.
    fs::write(corpus.path().join("b.csv"), "id,value\n3,gamma\n").unwrap();
    assert_eq!(run_index(config).unwrap(), 3);
    assert_eq!(paths(&endpoint.data_docs()), ["a.csv", "b.csv"]);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 2);
}

/// Junk *records* were fatal on the generated path
/// (`durable preparation of X produced N junk records`) while the legacy path
/// merely accumulated them — directly contradicting the documented contract
/// "3 completed-with-junk (junk recorded, never fatal)".
#[test]
fn a_junk_record_is_counted_into_the_generation_and_never_fatal() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap();
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let mut log = String::from("!!! rotated by logrotate, not a log line\n");
    for n in 1..=40 {
        log.push_str(&format!(
            "10.0.0.{n} - - [09/Aug/2026:10:00:{n:02} +0000] \"GET /p/{n} HTTP/1.1\" 200 {n}\n"
        ));
    }
    fs::write(corpus.path().join("access.log"), log).unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (code, summary) = run_index_report(config).unwrap();
    let summary = summary.expect("a corpus with one junk record still commits a generation");
    assert_eq!(code, 3, "completed-with-junk exits 3");
    assert_eq!(summary["generation"], 1);
    assert_eq!(summary["files_junk"], 0);
    assert_eq!(
        summary["junk_records_total"], 1,
        "the unparseable line must be counted, not hardcoded to zero"
    );
    assert_eq!(summary["records_total"], 40);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    // The same count reaches the per-file catalog document.
    let file_doc = endpoint
        .state
        .lock()
        .unwrap()
        .docs
        .iter()
        .find(|((index, _), doc)| {
            index == catalog::CATALOG_INDEX
                && doc.get("doc_kind").and_then(Value::as_str) == Some("file")
                && doc.get("path").and_then(Value::as_str) == Some("access.log")
        })
        .map(|(_, doc)| doc.clone())
        .expect("the indexed file has a catalog document");
    assert_eq!(file_doc["junk"], 1);
    assert_eq!(file_doc["records"], 40);
}

/// Where this branch and #241 meet.
///
/// The generated `--no-graph` route returns from inside `run_index_report`,
/// several hundred lines before the legacy `pr.finish` at the bottom of the
/// function. `Ticker::drop` closes an unfinished stream with `ok=false`, so a
/// generated run that merely returned would print an *aborted* terminal line
/// immediately after committing a generation successfully — a silent
/// contradiction between two changes that each looked right on its own.
///
/// This pins all three properties on the generated path: the phase-A scan is
/// narrated (it is a per-file, minutes-long phase here exactly as on the legacy
/// path), the stream closes with a truthful terminal line, and `--progress
/// none` on the same path stays completely silent.
#[test]
fn a_generated_run_narrates_its_scan_and_closes_its_own_stream() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap();
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK.lock().unwrap();
    let _sink_guard = crate::progress::SINK_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    for n in 0..4 {
        fs::write(
            corpus.path().join(format!("rows-{n}.csv")),
            format!("id,value\n{n}0,alpha\n{n}1,beta\n"),
        )
        .unwrap();
    }
    let endpoint = HttpEndpoint::start();

    // Generation 1: the initial commit, through the real progress surface.
    let mut loud = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    loud.quiet = false;
    loud.progress = crate::progress::ProgressMode::Plain;
    loud.progress_interval = Some(std::time::Duration::from_secs(1));
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let code = {
        let _sink = crate::progress::install_test_sink(&buffer);
        run_index(loud.clone()).unwrap()
    };
    assert_eq!(code, 0);
    let stream = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    let done = stream
        .lines()
        .find(|line| line.starts_with("xerj-done "))
        .unwrap_or_else(|| panic!("a generated run must close its own stream:\n{stream}"));
    assert!(
        done.contains("ok=true") && done.contains("exit=0"),
        "a committed generation is not an aborted run: {done}"
    );
    assert!(done.contains("reason=completed"), "{done}");

    // Generation 2: the reconcile branch, which runs its own phase-A scan.
    fs::write(corpus.path().join("rows-0.csv"), "id,value\n00,CHANGED\n").unwrap();
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let code = {
        let _sink = crate::progress::install_test_sink(&buffer);
        run_index(loud).unwrap()
    };
    assert_eq!(code, 0);
    let stream = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert!(
        stream
            .lines()
            .any(|line| line.starts_with("xerj-progress ") && line.contains("phase=scan")),
        "the reconcile projection re-scans every file and must say so:\n{stream}"
    );
    let done = stream
        .lines()
        .find(|line| line.starts_with("xerj-done "))
        .unwrap_or_else(|| panic!("a reconciled generation must close its own stream:\n{stream}"));
    assert!(done.contains("ok=true"), "{done}");

    // The other half of the contract on the same path: silence means silence.
    fs::write(corpus.path().join("rows-1.csv"), "id,value\n10,CHANGED\n").unwrap();
    let quiet = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(quiet.progress, crate::progress::ProgressMode::None);
    let buffer = Arc::new(Mutex::new(Vec::new()));
    {
        let _sink = crate::progress::install_test_sink(&buffer);
        assert_eq!(run_index(quiet).unwrap(), 0);
    }
    assert!(
        buffer.lock().unwrap().is_empty(),
        "--progress none asked for nothing: {}",
        String::from_utf8_lossy(&buffer.lock().unwrap())
    );
}
