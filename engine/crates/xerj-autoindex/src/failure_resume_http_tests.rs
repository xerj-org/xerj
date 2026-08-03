//! HTTP-level regression tests for the file replacement transaction.
//!
//! These deliberately exercise `run_index`, `Es`, extraction, staging, bulk
//! splitting, and the durable journal together. The fake endpoint injects a
//! per-item backend failure after applying part of a bulk, which models the
//! most dangerous response shape: visibility changed, but `file_done` must
//! not be durable.

use super::*;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

static FAILPOINT_TEST_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct MockState {
    docs: HashMap<String, Value>,
    requests: Vec<(String, String)>,
    data_bulk_number: usize,
    fail_data_bulk: usize,
    failed_once: bool,
    delete_calls: usize,
    stop: bool,
    embedding_identity_sha256: String,
}

struct MockEndpoint {
    url: String,
    state: Arc<Mutex<MockState>>,
    join: Option<thread::JoinHandle<()>>,
}

impl MockEndpoint {
    fn start(fail_data_bulk: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(MockState {
            fail_data_bulk,
            embedding_identity_sha256: "a".repeat(64),
            ..MockState::default()
        }));
        let server_state = Arc::clone(&state);
        let join = thread::spawn(move || loop {
            match listener.accept() {
                Ok((stream, _)) => handle(stream, &server_state),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if server_state.lock().unwrap().stop {
                        break;
                    }
                    thread::yield_now();
                }
                Err(error) => panic!("mock endpoint accept failed: {error}"),
            }
        });
        Self {
            url,
            state,
            join: Some(join),
        }
    }
}

impl Drop for MockEndpoint {
    fn drop(&mut self) {
        self.state.lock().unwrap().stop = true;
        // Wake the nonblocking listener even on heavily loaded test hosts.
        let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
        self.join.take().unwrap().join().unwrap();
    }
}

fn handle(mut stream: TcpStream, state: &Arc<Mutex<MockState>>) {
    let clone = stream.try_clone().unwrap();
    let mut reader = BufReader::new(clone);
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

    let response = if method == "GET" && path == "/v1/embedding/identity" {
        let identity = state.lock().unwrap().embedding_identity_sha256.clone();
        json!({"data": {
            "version": 1,
            "backend": "lexical",
            "identity_sha256": identity,
            "dimensions": 384,
            "semantic_contract": "semantic_text-derived-vector.v1",
            "resumable": true
        }, "took_ms": 0, "request_id": "test"})
    } else if method == "POST" && path == "/_bulk" {
        bulk_response(&body, state)
    } else if method == "POST" && path.contains("/_delete_by_query") {
        let query: Value = serde_json::from_slice(&body).unwrap();
        let ax_file = query
            .pointer("/query/term/ax_file")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut locked = state.lock().unwrap();
        locked.delete_calls += 1;
        if let Some(key) = ax_file {
            locked
                .docs
                .retain(|_, doc| doc.get("ax_file").and_then(Value::as_str) != Some(key.as_str()));
        }
        json!({"deleted": 0, "failures": []})
    } else if method == "GET" && path.ends_with("/_count") {
        json!({"count": state.lock().unwrap().docs.len()})
    } else if method == "POST" && path.ends_with("/_search") {
        json!({"hits":{"total":{"value":0},"hits":[]},"aggregations":{}})
    } else {
        // ping, index creation/mapping, refresh, and catalog operations
        json!({"acknowledged": true})
    };
    let bytes = response.to_string();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        bytes.len(),
        bytes
    )
    .unwrap();
}

fn bulk_response(body: &[u8], state: &Arc<Mutex<MockState>>) -> Value {
    let lines: Vec<&[u8]> = body
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect();
    let is_data = lines
        .first()
        .and_then(|line| serde_json::from_slice::<Value>(line).ok())
        .and_then(|action| action.pointer("/index/_index").cloned())
        .and_then(|index| index.as_str().map(str::to_owned))
        .is_some_and(|index| index != catalog::CATALOG_INDEX);
    if !is_data {
        return json!({"errors": false, "items": []});
    }

    let mut locked = state.lock().unwrap();
    locked.data_bulk_number += 1;
    let fail = !locked.failed_once && locked.data_bulk_number == locked.fail_data_bulk;
    let pairs = lines.chunks_exact(2);
    let visible = if fail { pairs.len() / 2 } else { pairs.len() };
    for pair in lines.chunks_exact(2).take(visible) {
        let action: Value = serde_json::from_slice(pair[0]).unwrap();
        let doc: Value = serde_json::from_slice(pair[1]).unwrap();
        let id = action.pointer("/index/_id").unwrap().as_str().unwrap();
        locked.docs.insert(id.to_owned(), doc);
    }
    if fail {
        locked.failed_once = true;
        json!({
            "errors": true,
            "items": [{"index": {
                "status": 500,
                "error": {"type": "injected_failure", "reason": "partial bulk"}
            }}]
        })
    } else {
        json!({"errors": false, "items": []})
    }
}

fn cfg(root: &Path, state_dir: &Path, url: &str) -> IndexCfg {
    IndexCfg {
        root: root.to_owned(),
        url: url.to_owned(),
        api_key: None,
        workers: 1,
        pdf_workers: 1,
        pdf_timeout_secs: 30,
        bulk_mb: 64,
        bulk_timeout_secs: 3_600,
        prefix: "failure-test".into(),
        state_dir: Some(state_dir.to_owned()),
        fresh: false,
        follow_symlinks: false,
        max_file_gb: 1,
        sample: 50,
        no_semantic: true,
        // These tests count data bulks to place injected failures; graph
        // edge bulks would shift that numbering, so keep detection off here
        // (the graph pipeline has its own e2e suite in detect::e2e).
        brain: None,
        no_graph: true,
        dry_run: false,
        json: false,
        quiet: true,
    }
}

fn file_done_count(state_dir: &Path) -> usize {
    let journal = fs::read_to_string(state_dir.join("journal.ndjson")).unwrap();
    journal
        .lines()
        .filter(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|value| value.get("kind").cloned())
                .as_ref()
                .and_then(Value::as_str)
                == Some("file_done")
        })
        .count()
}

fn event_count(state_dir: &Path, kind: &str) -> usize {
    fs::read_to_string(state_dir.join("journal.ndjson"))
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|value| value.get("kind").and_then(Value::as_str) == Some(kind))
        .count()
}

fn assert_unsupported_delta_without_remote_mutation(
    endpoint: &MockEndpoint,
    config: IndexCfg,
    expected_added: &[&str],
    expected_vanished: &[&str],
) {
    let request_start = endpoint.state.lock().unwrap().requests.len();
    let journal_path = config.state_dir.as_ref().unwrap().join("journal.ndjson");
    let journal_before = fs::read(&journal_path).unwrap();
    let error = run_index(config).unwrap_err();
    let attempted_requests = endpoint.state.lock().unwrap().requests[request_start..].to_vec();
    assert_eq!(
        attempted_requests,
        [("GET".to_owned(), "/".to_owned())],
        "a refused attempt may perform only the endpoint-readiness GET"
    );
    assert_eq!(
        fs::read(journal_path).unwrap(),
        journal_before,
        "preflight refusal must not append a resume event or rewrite the journal"
    );
    let message = format!("{error:#}");
    assert!(message.contains("made no remote mutations"), "{message}");
    assert!(
        message.contains("existing destination may already be partial or stale"),
        "{message}"
    );
    assert!(
        message.contains("`--fresh` is not recovery or destination reconciliation"),
        "{message}"
    );
    assert!(
        message.contains("new --state-dir, a new --prefix"),
        "{message}"
    );
    assert!(message.contains("new --brain"), "{message}");
    for path in expected_added {
        assert!(
            message.contains(path),
            "missing added path {path}: {message}"
        );
    }
    for path in expected_vanished {
        assert!(
            message.contains(path),
            "missing vanished path {path}: {message}"
        );
    }
}

#[test]
fn completed_plan_rejects_added_content_group_before_remote_mutation() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("first.csv"), "id,value\n1,first\n").unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    fs::write(corpus.path().join("second.csv"), "id,value\n2,second\n").unwrap();
    assert_unsupported_delta_without_remote_mutation(&endpoint, config, &["second.csv"], &[]);
}

#[test]
fn completed_plan_rejects_vanished_content_group_before_remote_mutation() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("first.csv"), "id,value\n1,first\n").unwrap();
    fs::write(corpus.path().join("second.csv"), "id,value\n2,second\n").unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    fs::remove_file(corpus.path().join("second.csv")).unwrap();
    assert_unsupported_delta_without_remote_mutation(&endpoint, config, &[], &["second.csv"]);
}

#[test]
fn completed_plan_rejects_mixed_membership_delta_in_stable_order() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("b-old.csv"), "id,value\n1,b\n").unwrap();
    fs::write(corpus.path().join("a-old.csv"), "id,value\n2,a\n").unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    fs::remove_file(corpus.path().join("b-old.csv")).unwrap();
    fs::remove_file(corpus.path().join("a-old.csv")).unwrap();
    fs::write(corpus.path().join("z-new.csv"), "id,value\n3,z\n").unwrap();
    fs::write(corpus.path().join("m-new.csv"), "id,value\n4,m\n").unwrap();
    config.json = true;
    let request_start = endpoint.state.lock().unwrap().requests.len();
    let error = run_index(config).unwrap_err();
    assert_eq!(
        endpoint.state.lock().unwrap().requests[request_start..],
        [("GET".to_owned(), "/".to_owned())]
    );

    let typed = error
        .downcast_ref::<UnsupportedInventoryDeltaError>()
        .unwrap();
    let value = typed.to_json();
    assert_eq!(value["schema"], "xerj.autoindex.unsupported_sync_delta.v1");
    assert_eq!(value["status"], "error");
    let added: Vec<&str> = value["added_content_groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap())
        .collect();
    let vanished: Vec<&str> = value["vanished_content_groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap())
        .collect();
    assert_eq!(added, ["m-new.csv", "z-new.csv"]);
    assert_eq!(vanished, ["a-old.csv", "b-old.csv"]);
}

#[test]
fn nonempty_fresh_cannot_erase_plan_and_bypass_membership_gate() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("old.csv"), "id,value\n1,old\n").unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    fs::remove_file(corpus.path().join("old.csv")).unwrap();
    fs::write(corpus.path().join("new.csv"), "id,value\n2,new\n").unwrap();
    config.fresh = true;
    assert_unsupported_delta_without_remote_mutation(&endpoint, config, &["new.csv"], &["old.csv"]);
}

#[test]
fn fresh_rejects_same_content_canonical_promotion_even_when_group_key_matches() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let bytes = "id,value\n1,same\n";
    fs::write(corpus.path().join("a.csv"), bytes).unwrap();
    fs::write(corpus.path().join("b.csv"), bytes).unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    // b.csv becomes canonical with the same content key. A key-only delta is
    // empty, but fresh would discard the alias/path cleanup knowledge.
    fs::remove_file(corpus.path().join("a.csv")).unwrap();
    config.fresh = true;
    assert_unsupported_delta_without_remote_mutation(&endpoint, config, &[], &[]);
}

#[test]
fn unchanged_planned_junk_is_not_reported_as_added_content() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("rows.csv"), "id,value\n1,kept\n").unwrap();
    fs::write(
        corpus.path().join("opaque.bin"),
        [0_u8, 159, 146, 150, 0, 255],
    )
    .unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    assert_eq!(run_index(config.clone()).unwrap(), 3);
    let result = run_index(config);
    assert!(
        result.is_ok(),
        "unchanged durable junk must not trip the membership gate: {result:?}"
    );
}

#[test]
fn deleted_planned_junk_fails_closed_before_catalog_can_stay_stale() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("rows.csv"), "id,value\n1,kept\n").unwrap();
    fs::write(
        corpus.path().join("opaque.bin"),
        [0_u8, 159, 146, 150, 0, 255],
    )
    .unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    assert_eq!(run_index(config.clone()).unwrap(), 3);

    fs::remove_file(corpus.path().join("opaque.bin")).unwrap();
    assert_unsupported_delta_without_remote_mutation(&endpoint, config, &[], &["opaque.bin"]);
}

#[test]
fn completed_plan_rejects_empty_current_folder_before_remote_mutation() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("only.csv"), "id,value\n1,only\n").unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    fs::remove_file(corpus.path().join("only.csv")).unwrap();
    // Even `--fresh` must not erase the only durable inventory evidence and
    // then report success while the destination still contains the document.
    config.fresh = true;
    assert_unsupported_delta_without_remote_mutation(&endpoint, config, &[], &["only.csv"]);
}

#[test]
fn semantic_resume_rejects_embedding_identity_drift_before_another_bulk() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("report.txt"),
        "Quarterly operating income improved materially after stronger subscription renewals. \
         Management expects durable demand across the next reporting period.",
    )
    .unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    config.no_semantic = false;
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    assert_eq!(event_count(state_dir.path(), "embedding_identity"), 1);
    let bulks_before = endpoint.state.lock().unwrap().data_bulk_number;
    endpoint.state.lock().unwrap().embedding_identity_sha256 = "b".repeat(64);
    let error = run_index(config).unwrap_err().to_string();
    assert!(error.contains("refusing to mix vector spaces"), "{error}");
    assert!(error.contains("--fresh"), "{error}");
    assert_eq!(
        endpoint.state.lock().unwrap().data_bulk_number,
        bulks_before,
        "identity drift must fail before another data bulk"
    );
}

#[test]
fn fresh_publication_skips_delete_and_noop_resume_does_not_append_plan() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    // run_index reaches Journal::file_done; the file_done IO failpoint is
    // a global one-shot, so every file_done-reaching test must hold its
    // lock (armer or not) or it can steal an armed injection. Acquired
    // after FAILPOINT_TEST_LOCK everywhere — one consistent order.
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("records.csv"),
        "id,value\n0,fresh\n1,fresh\n",
    )
    .unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);

    assert_eq!(run_index(config.clone()).unwrap(), 0);
    assert_eq!(endpoint.state.lock().unwrap().delete_calls, 0);
    assert_eq!(event_count(state_dir.path(), "plan"), 1);
    assert_eq!(event_count(state_dir.path(), "file_replace_start"), 1);

    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(endpoint.state.lock().unwrap().delete_calls, 0);
    assert_eq!(
        event_count(state_dir.path(), "plan"),
        1,
        "an identical resume must reuse the durable plan"
    );
}

#[test]
fn legacy_plan_without_completion_cleans_possible_partial_visibility() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    // run_index reaches Journal::file_done; the file_done IO failpoint is
    // a global one-shot, so every file_done-reaching test must hold its
    // lock (armer or not) or it can steal an armed injection. Acquired
    // after FAILPOINT_TEST_LOCK everywhere — one consistent order.
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let first_state = tempfile::tempdir().unwrap();
    let legacy_state = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("records.csv"),
        "id,value\n0,current\n1,current\n",
    )
    .unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let first_config = cfg(corpus.path(), first_state.path(), &endpoint.url);
    assert_eq!(run_index(first_config.clone()).unwrap(), 0);

    let root = first_config
        .root
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let first = state::Journal::open(
        first_state.path(),
        &root,
        &first_config.url,
        &first_config.prefix,
        first_config.bulk_timeout_secs,
        false,
    )
    .unwrap();
    let plan = first.plan.clone().unwrap();
    let key = plan.files.keys().next().unwrap().clone();
    drop(first);

    // A pre-intent journal may contain only its plan even though older or
    // partial bulk records are already visible.
    let mut legacy = state::Journal::open(
        legacy_state.path(),
        &root,
        &first_config.url,
        &first_config.prefix,
        first_config.bulk_timeout_secs,
        false,
    )
    .unwrap();
    legacy.write_plan(&plan).unwrap();
    drop(legacy);
    let deletes_before = {
        let mut state = endpoint.state.lock().unwrap();
        state.docs.insert(
            "obsolete-locator".to_string(),
            json!({
                "ax_file": key,
                "ax_locator": "obsolete",
                "value": "must-be-removed"
            }),
        );
        state.delete_calls
    };

    let mut resume_config = first_config;
    resume_config.state_dir = Some(legacy_state.path().to_owned());
    assert_eq!(run_index(resume_config).unwrap(), 0);
    let state = endpoint.state.lock().unwrap();
    assert_eq!(state.delete_calls, deletes_before + 1);
    assert!(!state.docs.contains_key("obsolete-locator"));
    assert!(state
        .docs
        .values()
        .all(|doc| doc.get("ax_locator").and_then(Value::as_str) != Some("obsolete")));
}

#[test]
fn legacy_key_collision_fails_before_visibility_with_scoped_guidance() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    // run_index reaches Journal::file_done; the file_done IO failpoint is
    // a global one-shot, so every file_done-reaching test must hold its
    // lock (armer or not) or it can steal an armed injection. Acquired
    // after FAILPOINT_TEST_LOCK everywhere — one consistent order.
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let mut owner = vec![b'x'; 65_537];
    owner[65_536] = b'b';
    let owner_path = corpus.path().join("b.txt");
    fs::write(&owner_path, &owner).unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    // Convert the current full-content plan into the exact legacy shape which
    // keyed content from first-64-KiB + size.
    let legacy_key = ids::file_key(&owner_path, owner.len() as u64).unwrap();
    let root = config
        .root
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let mut journal = state::Journal::open(
        state_dir.path(),
        &root,
        &config.url,
        &config.prefix,
        config.bulk_timeout_secs,
        false,
    )
    .unwrap();
    let mut plan = journal.plan.clone().unwrap();
    let old_key = plan.files.keys().next().unwrap().clone();
    let assignment = plan.files.remove(&old_key).unwrap();
    plan.files.insert(legacy_key, assignment);
    journal.write_plan(&plan).unwrap();
    drop(journal);

    let mut sibling = owner;
    sibling[65_536] = b'a';
    fs::write(corpus.path().join("a.txt"), sibling).unwrap();
    let (bulks_before, deletes_before, docs_before) = {
        let state = endpoint.state.lock().unwrap();
        (
            state.data_bulk_number,
            state.delete_calls,
            state.docs.clone(),
        )
    };
    let error = run_index(config).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("collides with legacy resume key"));
    assert!(message.contains("remove or move one of these two files"));
    assert!(message.contains(
        &state_dir
            .path()
            .join("journal.ndjson")
            .display()
            .to_string()
    ));
    let state = endpoint.state.lock().unwrap();
    assert_eq!(state.data_bulk_number, bulks_before);
    assert_eq!(state.delete_calls, deletes_before);
    assert_eq!(state.docs, docs_before);
}

#[test]
fn existing_completion_is_invalidated_and_partial_visibility_is_deleted_on_resume() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    // run_index reaches Journal::file_done; the file_done IO failpoint is
    // a global one-shot, so every file_done-reaching test must hold its
    // lock (armer or not) or it can steal an armed injection. Acquired
    // after FAILPOINT_TEST_LOCK everywhere — one consistent order.
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    // 1: failure immediately after delete; 2: one complete 5,000-doc bulk
    // then a partial final bulk; 3: two complete bulks then a partial final
    // bulk immediately before the file_done journal append.
    for (fail_bulk, rows) in [(1, 6_001usize), (2, 6_001), (3, 10_001)] {
        let corpus = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let mut csv = String::from("id,value\n");
        for row in 0..rows {
            csv.push_str(&format!("{row},value-{row}\n"));
        }
        fs::write(corpus.path().join("records.csv"), &csv).unwrap();
        let endpoint = MockEndpoint::start(fail_bulk);
        let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);

        // Establish the dangerous case: an older generation is both live and
        // durably complete before the replacement begins.
        {
            let mut locked = endpoint.state.lock().unwrap();
            locked.fail_data_bulk = usize::MAX;
        }
        assert_eq!(run_index(config.clone()).unwrap(), 0);
        assert_eq!(file_done_count(state_dir.path()), 1);
        fs::write(
            corpus.path().join("records.csv"),
            csv.replace("value-", "replacement-"),
        )
        .unwrap();
        {
            let mut locked = endpoint.state.lock().unwrap();
            locked.data_bulk_number = 0;
            locked.fail_data_bulk = fail_bulk;
            locked.failed_once = false;
        }
        let error = run_index(config.clone()).unwrap_err();
        assert!(
            error.to_string().contains("bulk/backend failures"),
            "{error:#}"
        );
        assert_eq!(
            file_done_count(state_dir.path()),
            1,
            "failure after data bulk {fail_bulk} must retain only historical completion"
        );
        let replay = state::Journal::open(
            state_dir.path(),
            &config.root.to_string_lossy(),
            &config.url,
            &config.prefix,
            config.bulk_timeout_secs,
            false,
        )
        .unwrap();
        assert!(
            replay.done.is_empty(),
            "replacement intent must invalidate historical file_done on replay"
        );
        assert_eq!(replay.pending_replacements.len(), 1);
        drop(replay);
        {
            let locked = endpoint.state.lock().unwrap();
            assert!(locked.failed_once);
            assert!(locked.docs.len() < rows);
        }

        assert_eq!(run_index(config).unwrap(), 0);
        assert_eq!(file_done_count(state_dir.path()), 2);
        let locked = endpoint.state.lock().unwrap();
        assert_eq!(locked.docs.len(), rows);
        assert_eq!(
            locked.delete_calls, 2,
            "fresh publication skips delete; replacement and repair each clean once"
        );
        assert!(locked.docs.values().all(|doc| {
            doc["value"]
                .as_str()
                .is_some_and(|value| value.starts_with("replacement-"))
        }));
        let mut locators: Vec<usize> = locked
            .docs
            .values()
            .map(|doc| {
                doc["ax_locator"]
                    .as_str()
                    .unwrap()
                    .trim_start_matches('r')
                    .parse()
                    .unwrap()
            })
            .collect();
        locators.sort_unstable();
        assert_eq!(locators, (0..rows).collect::<Vec<_>>());
    }
}

#[test]
fn resume_repairs_kills_after_plan_delete_and_final_bulk_before_file_done() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    // run_index reaches Journal::file_done; the file_done IO failpoint is
    // a global one-shot, so every file_done-reaching test must hold its
    // lock (armer or not) or it can steal an armed injection. Acquired
    // after FAILPOINT_TEST_LOCK everywhere — one consistent order.
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    for boundary in [1u8, 2, 4] {
        let rows = 6_001usize;
        let corpus = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let path = corpus.path().join("records.csv");
        let mut csv = String::from("id,value\n");
        for row in 0..rows {
            csv.push_str(&format!("{row},old-{row}\n"));
        }
        fs::write(&path, &csv).unwrap();
        let endpoint = MockEndpoint::start(usize::MAX);
        let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
        assert_eq!(run_index(config.clone()).unwrap(), 0);
        fs::write(&path, csv.replace("old-", "new-")).unwrap();

        REPLACEMENT_FAILPOINT.store(boundary, Ordering::SeqCst);
        let error = run_index(config.clone()).unwrap_err();
        assert!(
            format!("{error:#}").contains("injected replacement crash"),
            "boundary {boundary}: {error:#}"
        );
        let replay = state::Journal::open(
            state_dir.path(),
            &config.root.to_string_lossy(),
            &config.url,
            &config.prefix,
            config.bulk_timeout_secs,
            false,
        )
        .unwrap();
        assert!(replay.done.is_empty(), "boundary {boundary}");
        assert_eq!(replay.pending_replacements.len(), 1, "boundary {boundary}");
        drop(replay);

        assert_eq!(run_index(config).unwrap(), 0);
        let locked = endpoint.state.lock().unwrap();
        assert_eq!(locked.docs.len(), rows, "boundary {boundary}");
        assert!(locked.docs.values().all(|doc| {
            doc["value"]
                .as_str()
                .is_some_and(|value| value.starts_with("new-"))
        }));
    }
}

#[test]
fn file_done_append_and_fsync_failures_are_fatal_and_resume_repairs() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    for io_boundary in [1u8, 2, 3] {
        let corpus = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let path = corpus.path().join("records.csv");
        fs::write(&path, "id,value\n0,old\n1,old\n").unwrap();
        let endpoint = MockEndpoint::start(usize::MAX);
        let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
        assert_eq!(run_index(config.clone()).unwrap(), 0);
        fs::write(&path, "id,value\n0,new\n1,new\n").unwrap();

        state::fail_next_file_done_io(io_boundary);
        let error = run_index(config.clone()).unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("durably commit completed source"),
            "{message}"
        );
        assert!(
            message.contains(if io_boundary == 1 {
                "append failure"
            } else if io_boundary == 3 {
                "partial file_done write"
            } else {
                "fsync failure"
            }),
            "{message}"
        );
        let replay = state::Journal::open(
            state_dir.path(),
            &config.root.to_string_lossy(),
            &config.url,
            &config.prefix,
            config.bulk_timeout_secs,
            false,
        )
        .unwrap();
        assert!(replay.done.is_empty());
        assert_eq!(replay.pending_replacements.len(), 1);
        drop(replay);

        assert_eq!(run_index(config).unwrap(), 0);
        let locked = endpoint.state.lock().unwrap();
        assert_eq!(locked.docs.len(), 2);
        assert!(locked
            .docs
            .values()
            .all(|doc| doc["value"].as_str() == Some("new")));
    }
}

#[test]
fn pending_generation_b_is_superseded_when_source_changes_to_c() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    // run_index reaches Journal::file_done; the file_done IO failpoint is
    // a global one-shot, so every file_done-reaching test must hold its
    // lock (armer or not) or it can steal an armed injection. Acquired
    // after FAILPOINT_TEST_LOCK everywhere — one consistent order.
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let path = corpus.path().join("records.csv");
    fs::write(&path, "id,value\n0,generation-a\n1,generation-a\n").unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    fs::write(&path, "id,value\n0,generation-b\n1,generation-b\n").unwrap();
    REPLACEMENT_FAILPOINT.store(2, Ordering::SeqCst);
    run_index(config.clone()).unwrap_err();
    let replay_b = state::Journal::open(
        state_dir.path(),
        &config.root.to_string_lossy(),
        &config.url,
        &config.prefix,
        config.bulk_timeout_secs,
        false,
    )
    .unwrap();
    let key = replay_b.pending_replacements.keys().next().unwrap().clone();
    let generation_b = replay_b.pending_replacements[&key].clone();
    drop(replay_b);

    fs::write(&path, "id,value\n0,generation-c\n1,generation-c\n").unwrap();
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    let replay_c = state::Journal::open(
        state_dir.path(),
        &config.root.to_string_lossy(),
        &config.url,
        &config.prefix,
        config.bulk_timeout_secs,
        false,
    )
    .unwrap();
    assert!(replay_c.pending_replacements.is_empty());
    let committed_generation = replay_c.done[&key].generation.as_deref().unwrap();
    assert_ne!(committed_generation, generation_b);
    let events: Vec<Value> = fs::read_to_string(state_dir.path().join("journal.ndjson"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let starts: Vec<&str> = events
        .iter()
        .filter(|event| {
            event["kind"] == "file_replace_start"
                && event["file_key"].as_str() == Some(key.as_str())
        })
        .filter_map(|event| event["generation"].as_str())
        .collect();
    assert_eq!(starts[starts.len() - 2], generation_b);
    assert_eq!(starts.last().copied(), Some(committed_generation));
    drop(replay_c);
    let locked = endpoint.state.lock().unwrap();
    assert_eq!(locked.docs.len(), 2);
    assert!(locked
        .docs
        .values()
        .all(|doc| doc["value"].as_str() == Some("generation-c")));
}

#[test]
fn a_planned_key_never_gains_two_owners_when_old_content_moves_paths() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    // run_index reaches Journal::file_done; the file_done IO failpoint is
    // a global one-shot, so every file_done-reaching test must hold its
    // lock (armer or not) or it can steal an armed injection. Acquired
    // after FAILPOINT_TEST_LOCK everywhere — one consistent order.
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let original = "id,value\n0,original\n1,original\n";
    fs::write(corpus.path().join("a.csv"), original).unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    // a.csv keeps its planned key by path while its OLD bytes reappear as
    // b.csv — whose content key is exactly the planned key a.csv claims. Both
    // files owning one ax_file key would let each replacement delete the
    // other's freshly published documents.
    fs::write(
        corpus.path().join("a.csv"),
        "id,value\n0,rewritten\n1,rewritten\n",
    )
    .unwrap();
    fs::write(corpus.path().join("b.csv"), original).unwrap();
    assert_unsupported_delta_without_remote_mutation(&endpoint, config.clone(), &["b.csv"], &[]);
    {
        let locked = endpoint.state.lock().unwrap();
        assert_eq!(locked.docs.len(), 2);
        assert!(locked.docs.values().all(|doc| {
            doc["ax_path"].as_str() == Some("a.csv") && doc["value"].as_str() == Some("original")
        }));
    }
    let replay = state::Journal::open(
        state_dir.path(),
        &config.root.to_string_lossy(),
        &config.url,
        &config.prefix,
        config.bulk_timeout_secs,
        false,
    )
    .unwrap();
    assert!(replay.pending_replacements.is_empty());
    assert_eq!(replay.done.len(), 1);
    let plan = replay.plan.clone().unwrap();
    assert!(plan.junk_files.is_empty());
    drop(replay);

    // The refusal is deterministic: an identical rerun keeps the old owner,
    // appends no new plan, and changes no documents.
    let plans_before = event_count(state_dir.path(), "plan");
    assert_unsupported_delta_without_remote_mutation(&endpoint, config, &["b.csv"], &[]);
    assert_eq!(event_count(state_dir.path(), "plan"), plans_before);
    let locked = endpoint.state.lock().unwrap();
    assert_eq!(locked.docs.len(), 2);
    assert!(locked
        .docs
        .values()
        .all(|doc| doc["value"].as_str() == Some("original")));
}

#[test]
fn deleting_an_entire_duplicate_group_strands_no_pending_replacement() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    // run_index reaches Journal::file_done; the file_done IO failpoint is
    // a global one-shot, so every file_done-reaching test must hold its
    // lock (armer or not) or it can steal an armed injection. Acquired
    // after FAILPOINT_TEST_LOCK everywhere — one consistent order.
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("dup-a.csv"), "id,value\n0,dup\n1,dup\n").unwrap();
    fs::write(corpus.path().join("dup-b.csv"), "id,value\n0,dup\n1,dup\n").unwrap();
    fs::write(corpus.path().join("keep.csv"), "id,value\n2,keep\n3,keep\n").unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    let starts = event_count(state_dir.path(), "file_replace_start");

    // The whole duplicate group disappears. There is no current file left to
    // republish its key, so no replacement intent may be journaled — a
    // stranded intent would be re-appended forever without ever committing.
    fs::remove_file(corpus.path().join("dup-a.csv")).unwrap();
    fs::remove_file(corpus.path().join("dup-b.csv")).unwrap();
    for _ in 0..2 {
        assert_unsupported_delta_without_remote_mutation(
            &endpoint,
            config.clone(),
            &[],
            &["dup-a.csv"],
        );
        assert_eq!(
            event_count(state_dir.path(), "file_replace_start"),
            starts,
            "an orphaned duplicate-group key must not schedule a replacement"
        );
        let replay = state::Journal::open(
            state_dir.path(),
            &config.root.to_string_lossy(),
            &config.url,
            &config.prefix,
            config.bulk_timeout_secs,
            false,
        )
        .unwrap();
        assert!(replay.pending_replacements.is_empty());
    }
}

#[test]
fn partial_file_done_is_rolled_back_before_another_worker_commits() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n0,a-old\n1,a-old\n").unwrap();
    fs::write(corpus.path().join("b.csv"), "id,value\n2,b-old\n3,b-old\n").unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    config.workers = 2;
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    fs::write(corpus.path().join("a.csv"), "id,value\n0,a-new\n1,a-new\n").unwrap();
    fs::write(corpus.path().join("b.csv"), "id,value\n2,b-new\n3,b-new\n").unwrap();
    state::fail_next_file_done_io(3);
    let error = run_index(config.clone()).unwrap_err();
    assert!(format!("{error:#}").contains("partial file_done write"));

    // One worker hit the partial append and rolled back while the other was
    // allowed to commit. Replay must remain parseable and expose exactly one
    // pending generation rather than malformed middle corruption.
    let replay = state::Journal::open(
        state_dir.path(),
        &config.root.to_string_lossy(),
        &config.url,
        &config.prefix,
        config.bulk_timeout_secs,
        false,
    )
    .unwrap();
    assert_eq!(replay.pending_replacements.len(), 1);
    assert_eq!(replay.done.len(), 1);
    drop(replay);

    assert_eq!(run_index(config).unwrap(), 0);
    let locked = endpoint.state.lock().unwrap();
    assert_eq!(locked.docs.len(), 4);
    assert!(locked.docs.values().all(|doc| {
        doc["value"]
            .as_str()
            .is_some_and(|value| value.ends_with("-new"))
    }));
}
