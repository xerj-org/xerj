//! Real-HTTP end-to-end coverage for incremental corpus generations.
//!
//! The endpoint below is deliberately stateful. It applies bulk index/update
//! actions, delete-by-query, and exact count searches so `run_index` traverses
//! the same HTTP client and validation barriers as a real server.

use super::*;
use std::collections::{BTreeMap, HashMap};
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
                        // A per-dataset read-back scopes `terms` on `ax_file`
                        // and `term` on `ax_dataset` in the same `bool.filter`,
                        // so an endpoint that honoured only the latter would
                        // count every file's records into every dataset.
                        && filter
                            .get("terms")
                            .and_then(Value::as_object)
                            .and_then(|terms| terms.iter().next())
                            .is_none_or(|(field, values)| {
                                values.as_array().is_some_and(|values| {
                                    values.iter().any(|value| doc.get(field) == Some(value))
                                })
                            })
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
        api_key_file: None,
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
        follow_symlinks_outside_root: false,
        ignore: crate::ignore_rules::IgnoreOptions::default(),
        max_file_gb: 1,
        sample: 50,
        no_semantic: !semantic,
        brain: None,
        no_graph: true,
        // Gate off: these fixtures assert reconcile behaviour, not a
        // timing-derived stop. See `gate_tests` and `cli::tests`.
        max_minutes: 0,
        approve: None,
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
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
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
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
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
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
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
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
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

/// #580: re-indexing a **code file** must not abort. The AST file document
/// carries a `symbols` array (of objects); `FieldAcc` does not scalar-type it,
/// so it is pruned from the frozen dataset schema at genesis — and the second
/// generation observed `symbols` again and aborted with `field symbols is
/// absent from frozen dataset`. A field with no scalar values is now allowed to
/// be absent from the frozen schema.
#[test]
fn code_file_reindex_does_not_abort_on_absent_symbols_field() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("lib.rs"),
        "pub struct AlphaConfig {\n    pub retries: u32,\n}\n",
    )
    .unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    // Genesis.
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    // Edit the code file and re-index — before #580 this aborted the whole run
    // on the frozen `symbols` field.
    fs::write(
        corpus.path().join("lib.rs"),
        "pub struct AlphaConfig {\n    pub retries: u64,\n}\n",
    )
    .unwrap();
    assert_eq!(
        run_index(config).unwrap(),
        0,
        "re-indexing a code file must reconcile, not abort on the frozen `symbols` field (#580)"
    );
    // The struct is still indexed (file doc + its symbol doc).
    assert!(
        endpoint
            .data_docs()
            .iter()
            .any(|d| d.get("name").is_some_and(|n| n == "AlphaConfig")),
        "the struct's symbol doc must survive the re-index"
    );
}

#[test]
fn pending_generation_replays_sealed_source_after_live_source_mutates() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
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
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
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

/// #490 (fix #3): a corpus committed by the `--no-graph` generated path must not
/// be silently re-runnable on the default *graph* path. A committed generation
/// manifest is graph-disabled by construction (`sync::validate_manifest`), so
/// the graph path recognises the mismatch and refuses it — the way the
/// graph→no-graph direction is already refused.
///
/// Scope of what this harness proves. Without the guard the re-run still fails,
/// but only later and opaquely: `write_plan`'s own `ensure!` rejects it with
/// `legacy plan write cannot follow a committed generated manifest`, *after*
/// `open_after_preflight` and `gc_snapshots` have run. On real binaries whose
/// graph path reaches Phase B before that point the destination is mutated first
/// (#490 matrix: 79 → 80 docs); this in-process harness is backstopped by that
/// `ensure!`, so it exercises the *message and ordering*, not the remote row.
/// The load-bearing contract pinned here is therefore: the run is refused with
/// the clear cross-path message and NOT the opaque `write_plan` one (i.e. the
/// guard preempts it, before the journal is opened for write). The destination
/// staying clean is asserted as a belt-and-suspenders invariant — true here
/// regardless of the guard, so a future change that lets the graph path reach
/// Phase B before refusing is caught.
#[test]
fn no_graph_committed_corpus_is_refused_on_the_graph_path_with_a_clear_message() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let source = corpus.path().join("rows.csv");
    fs::write(&source, "id,value\n1,first\n").unwrap();
    let endpoint = HttpEndpoint::start();

    // Commit generation 1 with the `--no-graph` generated executor.
    let no_graph = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(no_graph.clone()).unwrap(), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    let committed = endpoint.data_docs().len();
    assert!(committed > 0, "the no-graph run published documents");

    // Grow the corpus so a graph re-run would have a new row to publish, then
    // re-run the *same* state dir on the default graph path (`no_graph: false`).
    fs::write(&source, "id,value\n1,first\n2,second\n").unwrap();
    let mut graph = no_graph.clone();
    graph.no_graph = false;
    let error = run_index(graph).unwrap_err();
    let rendered = format!("{error:#}");
    // Load-bearing pair. Fail-before, `rendered` IS the opaque `write_plan`
    // error (thrown after journal-open/gc_snapshots) and lacks this phrase; the
    // guard replaces it with the clear, actionable one *and* preempts the opaque
    // one, proving the refusal now lands earlier.
    assert!(
        rendered.contains("indexed with --no-graph"),
        "the graph re-run must be refused with the clear cross-path message, got: {rendered}"
    );
    assert!(
        !rendered.contains("legacy plan write cannot follow a committed generated manifest"),
        "the guard must preempt write_plan's opaque late error, got: {rendered}"
    );

    // Belt-and-suspenders (true here regardless of the guard, because this
    // harness is backstopped by write_plan's `ensure!`): the destination is left
    // clean — the added row was not published and no second generation committed.
    // Asserted so a future change that lets the graph path reach Phase B before
    // refusing is caught as a mutation.
    assert_eq!(
        endpoint.data_docs().len(),
        committed,
        "the refused graph re-run must not publish the added row"
    );
    assert_eq!(
        journal_events(state_dir.path(), "sync_commit"),
        1,
        "no second generation may be committed by the refused cross-path re-run"
    );
}

#[test]
fn pending_semantic_identity_drift_rejects_before_any_further_bulk() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
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
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
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
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
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
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
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
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
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
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
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
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
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
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
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

#[test]
fn issue_283_inferred_float_manifest_digest_replays_on_unchanged_rerun() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    // 2,209 bytes / 9 records produces the exact inferred f64 which used to
    // change from 245.44444444444446 to 245.44444444444449 on journal replay.
    let mut csv = String::from("body\n");
    for len in [245, 245, 245, 245, 245, 245, 245, 245, 249] {
        csv.push_str(&"x".repeat(len));
        csv.push('\n');
    }
    fs::write(corpus.path().join("data.csv"), csv).unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (first_code, first_summary) = run_index_report(config.clone()).unwrap();
    assert_eq!(first_code, 0);
    assert_eq!(first_summary.unwrap()["generation"], 1);

    let (second_code, second_summary) = run_index_report(config).unwrap();
    assert_eq!(second_code, 0);
    assert_eq!(second_summary.unwrap()["generation"], 1);
}

#[test]
fn issue_283_junk_bearing_generation_reconciles_a_shrinking_file_set() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    // Preserve the inferred f64 from the original failure while exercising the
    // user's actual second-run shape: generation one contains junk, then a
    // tracked file disappears without any ignore-policy change.
    let mut csv = String::from("body\n");
    for len in [245, 245, 245, 245, 245, 245, 245, 245, 249] {
        csv.push_str(&"x".repeat(len));
        csv.push('\n');
    }
    fs::write(corpus.path().join("data.csv"), csv).unwrap();
    fs::write(
        corpus.path().join("remove.md"),
        "# Temporary report\n\nThis file is removed before generation two.\n",
    )
    .unwrap();
    fs::write(corpus.path().join("empty.csv"), "").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (first_code, first_summary) = run_index_report(config.clone()).unwrap();
    assert_eq!(first_code, 3, "generation one completes with recorded junk");
    let first_summary = first_summary.unwrap();
    assert_eq!(first_summary["generation"], 1);
    assert_eq!(first_summary["files_junk"], 1);
    assert_eq!(paths(&endpoint.data_docs()), ["data.csv", "remove.md"]);

    fs::remove_file(corpus.path().join("remove.md")).unwrap();
    let (second_code, second_summary) = run_index_report(config).unwrap();
    assert_eq!(second_code, 3, "the smaller tree still records empty.csv");
    let second_summary = second_summary.unwrap();
    assert_eq!(second_summary["generation"], 2);
    assert_eq!(second_summary["files_junk"], 1);
    assert_eq!(paths(&endpoint.data_docs()), ["data.csv"]);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 2);
}

/// Junk *records* were fatal on the generated path
/// (`durable preparation of X produced N junk records`) while the legacy path
/// merely accumulated them — directly contradicting the documented contract
/// "3 completed-with-junk (junk recorded, never fatal)".
#[test]
fn a_junk_record_is_counted_into_the_generation_and_never_fatal() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
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

/// #360: one file can feed several datasets — a SQL dump is one file and N
/// tables — but the generated executor sealed a single flat record total per
/// *file* and reconciled that whole-file total against *each* dataset's
/// read-back. Every ordinary multi-table dump therefore aborted at catalog
/// publication with `exit=1` on a run whose data was already complete.
#[test]
fn a_multi_table_dump_reconciles_each_dataset_against_its_own_sealed_count() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("dump.sql"),
        "CREATE TABLE `users` (`id` int, `name` varchar(64));\n\
         INSERT INTO `users` VALUES (1,'ann'),(2,'bob');\n\
         CREATE TABLE `orders` (`id` int, `total` int);\n\
         INSERT INTO `orders` VALUES (10,100),(11,200);\n",
    )
    .unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (code, summary) = run_index_report(config).unwrap();
    let summary = summary.expect("a two-table dump must commit its generation");
    assert_eq!(code, 0, "a valid multi-table dump is not a failed run");
    assert_eq!(summary["generation"], 1);
    assert_eq!(summary["records_total"], 4);
    assert_eq!(summary["junk_records_total"], 0);
    assert_eq!(endpoint.data_docs().len(), 4);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    // Both tables became datasets, and each one's catalog document reports the
    // records it actually holds — not the whole file's total.
    let dataset_docs: BTreeMap<String, Value> = endpoint
        .state
        .lock()
        .unwrap()
        .docs
        .iter()
        .filter(|((index, _), doc)| {
            index == catalog::CATALOG_INDEX
                && doc.get("doc_kind").and_then(Value::as_str) == Some("dataset")
        })
        .map(|((_, id), doc)| (id.clone(), doc.clone()))
        .collect();
    assert_eq!(
        dataset_docs.len(),
        2,
        "one dataset per table: {dataset_docs:?}"
    );
    for (id, doc) in &dataset_docs {
        assert_eq!(doc["record_count"], 2, "dataset {id} holds two rows");
    }
    // Each table's rows really are in their own index, under their own slug.
    for doc in endpoint.data_docs() {
        let slug = doc["ax_dataset"].as_str().unwrap().to_owned();
        assert!(
            dataset_docs.contains_key(&format!("ds:incremental-http:{slug}")),
            "record published under an uncatalogued dataset {slug}"
        );
    }
}

/// The other everyday shape of #360: a real database file. One `.sqlite` is
/// one content group and one dataset per table, exactly like the dump above.
#[test]
fn a_two_table_sqlite_file_commits_its_generation() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let connection = rusqlite::Connection::open(corpus.path().join("shop.sqlite")).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);\n\
             INSERT INTO users VALUES (1,'ann'),(2,'bob');\n\
             CREATE TABLE orders (id INTEGER PRIMARY KEY, total INTEGER);\n\
             INSERT INTO orders VALUES (10,100),(11,200);",
        )
        .unwrap();
    drop(connection);
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (code, summary) = run_index_report(config).unwrap();
    let summary = summary.expect("a two-table database must commit its generation");
    assert_eq!(code, 0);
    assert_eq!(summary["records_total"], 4);
    assert_eq!(endpoint.data_docs().len(), 4);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
}

/// The shape #360 was actually reported against, and the one that reaches the
/// fan-out through the *sniffer* rather than through an extension: ordinary
/// markdown prose with SQL blocks in it, as in `unum-cloud/usearch`'s
/// `sqlite/README.md`. `sniff` routes any text containing `CREATE TABLE` and a
/// `;` to the `sqldump` family, so a README documenting two tables is one
/// content group feeding two datasets — no dump and no database file involved.
/// Worth pinning separately: the reporter's corpus contained neither.
#[test]
fn a_readme_documenting_two_tables_is_not_a_fatal_condition() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("README.md"),
        "# vectors\n\nStore them like this:\n\n```sql\n\
         CREATE TABLE t1 (id INTEGER PRIMARY KEY, v JSON NOT NULL);\n\
         INSERT INTO t1 (id, v) VALUES (10, '[1.0]'), (11, '[2.0]');\n\
         CREATE TABLE t2 (id INTEGER PRIMARY KEY, v JSON NOT NULL);\n\
         INSERT INTO t2 (id, v) VALUES (20, '[1.0]'), (21, '[2.0]');\n\
         ```\n",
    )
    .unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (code, summary) = run_index_report(config).unwrap();
    let summary = summary.expect("a README describing two tables still commits");
    assert_eq!(code, 0, "a valid README is not a failed run");
    assert_eq!(summary["records_total"], 4);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    let mut counts: Vec<(String, u64)> = endpoint
        .state
        .lock()
        .unwrap()
        .docs
        .iter()
        .filter(|((index, _), doc)| {
            index == catalog::CATALOG_INDEX
                && doc.get("doc_kind").and_then(Value::as_str) == Some("dataset")
        })
        .map(|(_, doc)| {
            (
                doc["slug"].as_str().unwrap_or_default().to_owned(),
                doc["record_count"]
                    .as_u64()
                    .expect("a dataset catalog document reports its record count"),
            )
        })
        .collect();
    counts.sort();
    assert_eq!(
        counts.len(),
        2,
        "one dataset per declared table: {counts:?}"
    );
    assert!(
        counts.iter().all(|(_, records)| *records == 2),
        "each dataset carries only its own rows: {counts:?}"
    );
}

/// The second abort #360 reported — the one that killed both of the corpora
/// the reporter pointed autoindex at, on one node, one after the other.
///
/// `content::full_digest` derives a file's identity from its CONTENT alone, and
/// `catalog::CATALOG_INDEX` is a single global index that no `--prefix`
/// namespaces. Two unrelated checkouts that happen to share one byte-identical
/// file — an Apache-2.0 `LICENSE`, a `.gitignore`, an empty `__init__.py` —
/// therefore share that file's catalog document IDs. A corpus holding the
/// content twice publishes a canonical document plus one alias document; a
/// corpus holding it once republishes only the canonical, so the *other*
/// corpus's alias document survives under its own `run_id`. The generation-wide
/// barrier counted every catalog document carrying that `file_key`, across every
/// run on the node, against `1 + aliases.len()` for the group in front of it —
/// so the second run aborted with `exit=1` while its data was complete.
#[test]
fn a_file_shared_with_another_corpus_on_the_same_node_is_not_a_fatal_condition() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let shared = "id,value\n1,apache-2.0\n2,copied-verbatim\n";
    let first = tempfile::tempdir().unwrap();
    let first_state = tempfile::tempdir().unwrap();
    fs::create_dir(first.path().join("docs")).unwrap();
    fs::write(first.path().join("LICENSE.csv"), shared).unwrap();
    fs::write(first.path().join("docs").join("LICENSE.csv"), shared).unwrap();
    let endpoint = HttpEndpoint::start();

    let (code, summary) =
        run_index_report(cfg(first.path(), first_state.path(), &endpoint.url, false))
            .expect("the first corpus indexes");
    let summary = summary.expect("the first corpus commits its generation");
    assert_eq!(code, 0);
    assert_eq!(
        summary["duplicate_files"], 1,
        "the first corpus must publish one alias document, or nothing is left \
         behind for the second run to trip over: {summary}"
    );

    // A second, unrelated corpus on the same node, isolated exactly the way the
    // CLI's refusals recommend: its own --state-dir and its own --prefix. Its
    // one shared file is a group with no aliases at all.
    let second = tempfile::tempdir().unwrap();
    let second_state = tempfile::tempdir().unwrap();
    fs::write(second.path().join("LICENSE.csv"), shared).unwrap();
    fs::write(second.path().join("own.csv"), "id,value\n7,second\n").unwrap();
    let mut config = cfg(second.path(), second_state.path(), &endpoint.url, false);
    config.prefix = "incremental-http-second".into();

    let (code, summary) = run_index_report(config)
        .expect("a file shared with another corpus on the same node is not a fatal condition");
    let summary = summary.expect("the second corpus commits its generation");
    assert_eq!(code, 0, "the second corpus is a clean run: {summary}");
    assert_eq!(summary["generation"], 1);
    assert_eq!(summary["records_total"], 3);
    assert_eq!(summary["duplicate_files"], 0);
    assert_eq!(journal_events(second_state.path(), "sync_commit"), 1);

    // And the first corpus's alias document is still there — the barrier stopped
    // counting it, it was not swept away from under the other corpus's map.
    let aliases = endpoint
        .state
        .lock()
        .unwrap()
        .docs
        .iter()
        .filter(|((index, _), doc)| {
            index == catalog::CATALOG_INDEX
                && doc.get("status").and_then(Value::as_str) == Some("duplicate")
        })
        .count();
    assert_eq!(
        aliases, 1,
        "the other corpus's alias document must survive this run untouched"
    );
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
/// #294, at the level where the loss actually happened: a mixed corpus of
/// source code and prose, over the generated (`--no-graph`) path, through the
/// real HTTP client.
///
/// Every code file was walked, sniffed as family `code`, counted — and then
/// prepared as ZERO documents, so the generation committed and the run
/// reported success while the entire code half of the corpus was missing.
/// `sync_executor` pins the extraction half of that regression; this pins what
/// a caller can actually observe: the documents that reach the endpoint carry
/// their AST fields, and the run document and terminal line carry coverage
/// counters that a wholly-junked corpus could not fake.
#[test]
fn code_files_index_with_ast_fields_and_the_run_reports_code_coverage() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let _sink_guard = crate::progress::SINK_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("alpha.rs"),
        "pub struct AlphaConfig {\n    pub retries: u32,\n}\n\n\
         pub fn alpha_connect(cfg: &AlphaConfig) -> bool {\n    cfg.retries > 0\n}\n",
    )
    .unwrap();
    fs::write(
        corpus.path().join("gamma.py"),
        "def gamma_load(path):\n    return open(path).read()\n\n\n\
         class GammaStore:\n    def __init__(self, root):\n        self.root = root\n",
    )
    .unwrap();
    fs::write(
        corpus.path().join("notes.md"),
        "# Fixture notes\n\nTwo source files and this prose file, so the corpus is mixed.\n",
    )
    .unwrap();
    let endpoint = HttpEndpoint::start();
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    config.quiet = false;
    config.progress = crate::progress::ProgressMode::Plain;

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let (code, summary) = {
        let _sink = crate::progress::install_test_sink(&buffer);
        run_index_report(config).unwrap()
    };
    assert_eq!(code, 0, "nothing in this corpus is unparseable");
    let summary = summary.expect("a generated run returns its committed run projection");
    assert_eq!(summary["code_files"], 2, "{summary}");
    assert_eq!(
        summary["code_files_indexed"], 2,
        "both source files must reach the index: {summary}"
    );
    assert_eq!(summary["code_files_junked"], 0, "{summary}");

    let docs = endpoint.data_docs();
    // The per-file AST document carries `defs`/`symbols`; the #500 per-symbol
    // documents carry `code`/`name` but no `defs`, so filter on `defs` to keep
    // the one-per-file assertions honest about the file-level document.
    let mut code_docs: Vec<&Value> = docs
        .iter()
        .filter(|doc| doc.get("language").is_some() && doc.get("defs").is_some())
        .collect();
    code_docs.sort_by_key(|doc| doc["language"].as_str().unwrap_or("").to_owned());
    assert_eq!(
        code_docs.len(),
        2,
        "one AST (file-level) document per source file: {docs:?}"
    );
    // #500: each declaration is also promoted to its own retrievable document
    // (has `code` + `name`, no `defs`). At least the Rust struct + Python class
    // must each be their own symbol document, not only foldable inside a parent.
    let symbol_docs: Vec<&Value> = docs
        .iter()
        .filter(|doc| doc.get("code").is_some() && doc.get("name").is_some())
        .collect();
    assert!(
        symbol_docs
            .iter()
            .any(|d| d["name"] == "AlphaConfig" && d.get("defs").is_none()),
        "the AlphaConfig struct must be its own symbol document (#500): {symbol_docs:?}"
    );
    assert!(
        symbol_docs.iter().any(|d| d["name"] == "GammaStore"),
        "the GammaStore class must be its own symbol document (#500): {symbol_docs:?}"
    );
    assert_eq!(code_docs[0]["language"], "python");
    assert_eq!(code_docs[1]["language"], "rust");
    for doc in &code_docs {
        assert!(
            doc["defs"].as_str().is_some_and(|defs| !defs.is_empty()),
            "{doc}"
        );
        assert!(
            doc["symbols"]
                .as_array()
                .is_some_and(|symbols| !symbols.is_empty()),
            "{doc}"
        );
    }
    // The title comes from the logical file name, never the content-addressed
    // snapshot blob's ordinal — the other half of #294.
    assert_eq!(code_docs[0]["title"], "gamma.py");
    assert_eq!(code_docs[1]["title"], "alpha.rs");
    let rendered = serde_json::to_string(&code_docs).unwrap();
    assert!(rendered.contains("struct AlphaConfig"), "{rendered}");
    assert!(rendered.contains("class GammaStore"), "{rendered}");

    let stream = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    let done = stream
        .lines()
        .find(|line| line.starts_with("xerj-done "))
        .unwrap_or_else(|| panic!("{stream}"));
    assert!(
        done.contains("code_files=2 code_files_indexed=2 code_files_junked=0"),
        "the terminal line carries the coverage that made #294 invisible: {done}"
    );
    assert!(!stream.contains("warning:"), "{stream}");
}

#[test]
fn a_generated_run_narrates_its_scan_and_closes_its_own_stream() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let _sink_guard = crate::progress::SINK_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
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

/// Two byte-identical junk files — two empty files are the everyday case —
/// made the fresh plan carry a duplicate alias for content that has no
/// `plan.files` entry, so the generation cutover failed its own
/// alias-projection invariant and the whole run aborted with exit 1 (#283's
/// reproducible sibling: junk plus duplicates diverging between the fresh plan
/// and the incremental projection). The fresh plan now projects aliases the
/// same way `reconcile_plan` does, so the folder indexes, a no-op re-run
/// confirms instead of committing a spurious generation, and a shrinking file
/// set on the junk-bearing generation reconciles.
#[test]
fn two_byte_identical_junk_files_commit_reconcile_and_survive_a_shrink() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n2,beta\n").unwrap();
    fs::write(corpus.path().join("e1.dat"), "").unwrap();
    fs::write(corpus.path().join("e2.dat"), "").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (code, summary) = run_index_report(config.clone())
        .expect("a folder with two byte-identical junk files must index, not abort");
    let summary = summary.expect("the junk-bearing generation still commits");
    assert_eq!(code, 3, "junk is recorded, never fatal (cli.rs EXIT CODES)");
    assert_eq!(summary["generation"], 1);
    assert_eq!(summary["records_total"], 2);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    // The fresh plan and the incremental projection must agree byte-for-byte
    // on how junk-content aliases project, or this no-op re-run would see a
    // "changed" plan and commit generation 2 over nothing.
    assert_eq!(run_index(config.clone()).unwrap(), 3);
    assert_eq!(
        journal_events(state_dir.path(), "sync_commit"),
        1,
        "a no-op re-run over the junk-bearing generation confirms; it must not re-commit"
    );

    // The issue's headline shape: the file set shrinks on a generation that
    // carries junk records.
    fs::remove_file(corpus.path().join("a.csv")).unwrap();
    assert_eq!(run_index(config.clone()).unwrap(), 3);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 2);
    assert!(
        paths(&endpoint.data_docs()).is_empty(),
        "the deleted dataset's records must not survive the reconcile"
    );

    // And the junk files themselves disappearing reconciles back to clean.
    fs::remove_file(corpus.path().join("e1.dat")).unwrap();
    fs::remove_file(corpus.path().join("e2.dat")).unwrap();
    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 3);
}

/// The #283 abort itself: a durable generation record that fails its own
/// re-validation used to surface as a bare internal invariant ("desired
/// manifest digest does not match sync_begin payload", exit 1) — text the user
/// can do nothing with. However the journal got that way, the refusal must
/// name the recovery route the way the exec-config guard already does, and
/// keep the invariant attached as the cause.
#[test]
fn a_journal_that_fails_its_own_revalidation_names_the_rebuild_route() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let source = corpus.path().join("rows.csv");
    fs::write(&source, "id,value\n1,first\n").unwrap();
    fs::write(corpus.path().join("empty.csv"), "").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 3);

    // Leave a durable pending generation behind: begin succeeds, the bulk is
    // partially applied, the run fails before commit.
    fs::write(&source, "id,value\n1,replaced\n2,second\n").unwrap();
    endpoint
        .state
        .lock()
        .unwrap()
        .partially_apply_next_data_bulk = true;
    assert!(run_index(config.clone()).is_err());
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 2);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    // Corrupt the pending sync_begin payload in a spot only the manifest
    // digest covers: the junk record's byte count. This makes the next run
    // hit exactly the #283 invariant.
    let journal_path = state_dir.path().join("journal.ndjson");
    let tampered: String = fs::read_to_string(&journal_path)
        .unwrap()
        .lines()
        .map(|line| {
            let mut record: Value = serde_json::from_str(line).unwrap();
            if record.get("kind").and_then(Value::as_str) == Some("sync_begin")
                && record
                    .pointer("/desired/generation")
                    .and_then(Value::as_u64)
                    == Some(2)
            {
                let bytes = record
                    .pointer_mut("/desired/plan/junk_files/0/bytes")
                    .expect("the junk-bearing desired manifest records its junk file");
                *bytes = Value::from(bytes.as_u64().unwrap() + 1);
            }
            let mut line = serde_json::to_string(&record).unwrap();
            line.push('\n');
            line
        })
        .collect();
    fs::write(&journal_path, tampered).unwrap();

    let error = run_index(config).expect_err("a tampered generation journal must refuse to run");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("desired manifest digest does not match sync_begin payload"),
        "the internal invariant must stay attached as the cause: {rendered}"
    );
    assert!(
        rendered.contains("Rebuild with a new --state-dir and a new --prefix"),
        "the refusal must name the recovery route, not just the invariant: {rendered}"
    );
}

/// #589: `sweep_excluded_groups` actually deletes an excluded file's published
/// documents (by `ax_file`, across the plan's dataset indices) and its catalog
/// document (by `path`), while leaving every OTHER file's documents intact.
/// Exercised over the real mock HTTP backend so the delete-by-query truly runs.
#[test]
fn sweep_excluded_groups_deletes_only_the_excluded_files_documents() {
    let ep = HttpEndpoint::start();

    // Seed two files' records in one dataset index, plus their catalog docs.
    {
        let mut st = ep.state.lock().unwrap();
        let ds = "incremental-http-ds".to_string();
        st.docs.insert(
            (ds.clone(), "d1".into()),
            json!({"ax_file": "key-excluded", "body": "secret"}),
        );
        st.docs.insert(
            (ds.clone(), "d2".into()),
            json!({"ax_file": "key-kept", "body": "kept"}),
        );
        st.docs.insert(
            (catalog::CATALOG_INDEX.to_string(), "file:excluded".into()),
            json!({"doc_kind": "file", "path": "secret.csv"}),
        );
        st.docs.insert(
            (catalog::CATALOG_INDEX.to_string(), "file:kept".into()),
            json!({"doc_kind": "file", "path": "kept.csv"}),
        );
    }

    let es = Es::with_bulk_timeout(&ep.url, None, 30).expect("es client");

    let mut plan = Plan::default();
    plan.datasets.push(crate::state::PlanDataset {
        slug: "ds".into(),
        index: "incremental-http-ds".into(),
        family: "csv".into(),
        group: None,
        specs: Vec::new(),
        time_field: None,
        semantic_field: None,
        sampled_records: 1,
        file_count: 1,
    });

    let excluded = vec![InventoryDeltaEntry {
        file_key: "key-excluded".into(),
        path: "secret.csv".into(),
    }];

    sweep_excluded_groups(&es, &plan, &excluded).expect("sweep");

    let st = ep.state.lock().unwrap();
    // The excluded file's dataset record is gone.
    assert!(
        !st.docs
            .values()
            .any(|d| d.get("ax_file").and_then(Value::as_str) == Some("key-excluded")),
        "#589: the excluded file's indexed records must be swept"
    );
    // Its catalog document is gone.
    assert!(
        !st.docs
            .iter()
            .any(|((idx, _), d)| idx == catalog::CATALOG_INDEX
                && d.get("path").and_then(Value::as_str) == Some("secret.csv")),
        "#589: the excluded file's catalog document must be swept"
    );
    // Every other file is untouched (no over-deletion).
    assert!(
        st.docs
            .values()
            .any(|d| d.get("ax_file").and_then(Value::as_str) == Some("key-kept")),
        "#589: a different file's records must NOT be swept"
    );
    assert!(
        st.docs
            .iter()
            .any(|((idx, _), d)| idx == catalog::CATALOG_INDEX
                && d.get("path").and_then(Value::as_str) == Some("kept.csv")),
        "#589: a different file's catalog document must NOT be swept"
    );
}

/// #585 (case 1): a pending (uncommitted) `--no-graph` generation must NOT be
/// resumed and committed on the default graph path — doing so silently commits
/// a no-graph generation under a graph authority (the #584 hazard, but for a
/// not-yet-committed generation the committed-manifest guard never sees it
/// because the pending-sync branch returns first). The re-run must be refused
/// before any mutation, symmetric to #584.
#[test]
fn pending_no_graph_generation_is_refused_on_the_graph_path() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let source = corpus.path().join("rows.csv");
    fs::write(&source, "id,value\n1,first\n").unwrap();
    let endpoint = HttpEndpoint::start();

    // Commit generation 1 with the `--no-graph` generated executor.
    let no_graph = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(no_graph.clone()).unwrap(), 0);

    // Interrupt a second `--no-graph` generation after sync_begin: the data bulk
    // fails, leaving a pending sync (no-graph, uncommitted).
    fs::write(&source, "id,value\n1,first\n2,second\n").unwrap();
    endpoint.state.lock().unwrap().fail_next_data_bulk = true;
    assert!(run_index(no_graph.clone()).is_err());
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 2);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    // The destination as the refused re-run will find it (the interrupted run's
    // delete-before-replace already mutated it — irrelevant to the guard).
    let docs_before_rerun = endpoint.data_docs().len();

    // Re-run the SAME state dir on the default graph path. The pending no-graph
    // generation must be refused, not resumed-and-committed.
    let mut graph = no_graph.clone();
    graph.no_graph = false;
    let error = run_index(graph).unwrap_err();
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("pending sync") && rendered.contains("different graph authority"),
        "#585: a pending no-graph generation must be refused on the graph path with the clear message, got: {rendered}"
    );
    // No second generation committed, and the guard mutated nothing further.
    assert_eq!(
        journal_events(state_dir.path(), "sync_commit"),
        1,
        "#585: the refused graph re-run must not commit the pending no-graph generation"
    );
    assert_eq!(
        endpoint.data_docs().len(),
        docs_before_rerun,
        "#585: the refused re-run must mutate nothing"
    );
}

/// #585 (case 2): a `--no-graph` generation's genesis bootstrap (`sync_bootstrap`
/// committed, interrupted before `sync_begin`) must NOT be continued on the
/// default graph path either. Genesis carries no execution identity of its own
/// (`validate_genesis` requires `execution: None`), so unlike case 1 the guard
/// cannot compare a recorded mode — it instead relies on `sync_bootstrap` having
/// actually replayed, which only `begin_non_graph_generation` ever writes, and
/// only under `--no-graph`.
#[test]
fn no_graph_genesis_bootstrap_is_refused_on_the_graph_path() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let mut no_graph = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    no_graph.snapshot_max_bytes = 1;

    // Interrupt the very first --no-graph run between sync_bootstrap and
    // sync_begin: generation 0 (genesis) is committed, nothing else is.
    let error = run_index(no_graph.clone()).unwrap_err();
    assert!(format!("{error:#}").contains("snapshot source footprint"));
    assert_eq!(journal_events(state_dir.path(), "sync_bootstrap"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 0);
    assert_eq!(endpoint.data_docs().len(), 0);

    // Re-run the SAME state dir on the default graph path. The pending
    // --no-graph genesis bootstrap must be refused, not continued.
    let mut graph = no_graph.clone();
    graph.no_graph = false;
    graph.snapshot_max_bytes = 64 << 30;
    let error = run_index(graph).unwrap_err();
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("genesis bootstrap") && rendered.contains("different graph authority"),
        "#585 case 2: a --no-graph genesis bootstrap must be refused on the graph path with the \
         clear message, got: {rendered}"
    );
    // No mutation: nothing indexed, no new durable transaction beyond the
    // original sync_bootstrap.
    assert_eq!(journal_events(state_dir.path(), "sync_bootstrap"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 0);
    assert_eq!(endpoint.data_docs().len(), 0);

    // Positive control: re-running in the SAME (--no-graph) mode continues
    // the interrupted genesis normally.
    no_graph.snapshot_max_bytes = 64 << 30;
    assert_eq!(run_index(no_graph).unwrap(), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_bootstrap"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    assert_eq!(endpoint.data_docs().len(), 1);
}
