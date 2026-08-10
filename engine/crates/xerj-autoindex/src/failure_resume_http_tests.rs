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

/// Owns the process-global PDF worker environment for the duration of one
/// test. `XERJ_PDF_WORKER_BIN` is read inside `spawn_worker`, so a test that
/// sets it silently rewrites what every concurrently running test in this
/// binary observes — including the unit tests in `extract::pdf`. Acquire this
/// **before** the first `set_var`; the shared lock is the only thing that
/// makes those two suites safe to run at the default thread count.
#[cfg(unix)]
struct PdfWorkerEnvGuard {
    /// Held, never read: dropping it after the environment is cleared is the
    /// whole point.
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(unix)]
impl PdfWorkerEnvGuard {
    fn acquire() -> Self {
        Self {
            _lock: crate::extract::pdf::WORKER_BIN_ENV_LOCK
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
        }
    }
}

#[cfg(unix)]
impl Drop for PdfWorkerEnvGuard {
    fn drop(&mut self) {
        std::env::remove_var("XERJ_PDF_WORKER_BIN");
        std::env::remove_var("XERJ_TEST_PDF_COUNT");
    }
}

fn inject_pdf_spool_capacity(state_dir: &Path) {
    for (name, value) in [
        ("available-bytes", 16_u64 << 30),
        ("fd-limit", 4096),
        ("fd-open", 16),
    ] {
        fs::write(
            state_dir.join(format!(".autoindex-test-pdf-spool-{name}")),
            value.to_string(),
        )
        .unwrap();
    }
}

#[derive(Default)]
struct MockState {
    docs: HashMap<String, Value>,
    /// Catalog documents, kept apart from `docs` on purpose: `docs.len()` is
    /// what `_count`/`_search` answer with, and the run verifies live data
    /// counts against the journal (#195). Catalog rows are not data rows.
    catalog_docs: HashMap<String, Value>,
    data_bulk_number: usize,
    fail_data_bulk: usize,
    failed_once: bool,
    delete_calls: usize,
    response_delay_ms: u64,
    catalog_preexists: bool,
    catalog_mapping_upgraded: bool,
    catalog_bulk_before_upgrade: bool,
    /// Set if the additive mapping upgrade ever asks for `started`. This is
    /// the executable half of `catalog::catalog_mapping`'s tripwire: against a
    /// legacy (v1.0.0-rc.4) catalog that request is refused 400 and aborts the
    /// run, so the product must never send it. Asserted false rather than left
    /// to the 400 branch below, which a correct product never reaches.
    started_mapping_requested: bool,
    /// Catalog bulk actions the fixture does not model. Recorded rather than
    /// panicked on: this runs inside the server thread while the state guard
    /// is held, so a panic here would poison the `Mutex` and turn
    /// `MockEndpoint::drop`'s `join().unwrap()` into a second, misleading
    /// panic. Tests assert on it from the test thread instead.
    unexpected_catalog_actions: Vec<String>,
    stop: bool,
    embedding_identity_sha256: String,
    /// Reject every data-bulk item with a 403 explicit write-block error
    /// (the status ES gives `index.blocks.write` / `read_only`; only the
    /// flood-stage block is 429) without applying anything — the #195 shape.
    block_writes: bool,
    /// Accept every data bulk (`errors: false`) but persist nothing — the
    /// shape of any rejection path the client-side classifier misses.
    swallow_data_bulks: bool,
    /// Refuse the FIRST document of every data bulk with a per-item 400 and
    /// apply the rest. This is the one bulk shape that produces an
    /// `item_error` without a `server_error`: status is neither 429 nor 5xx
    /// and the type is not a block, so the run continues and must still
    /// report the rejection.
    reject_first_data_item: bool,
}

struct MockEndpoint {
    url: String,
    state: Arc<Mutex<MockState>>,
    join: Option<thread::JoinHandle<()>>,
}

impl MockEndpoint {
    fn start(fail_data_bulk: usize) -> Self {
        Self::start_with_delay(fail_data_bulk, 0)
    }

    fn start_with_delay(fail_data_bulk: usize, response_delay_ms: u64) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(MockState {
            fail_data_bulk,
            response_delay_ms,
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

    fn start_with_existing_catalog() -> Self {
        let endpoint = Self::start(usize::MAX);
        endpoint.state.lock().unwrap().catalog_preexists = true;
        endpoint
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
    let response_delay_ms = state.lock().unwrap().response_delay_ms;
    if response_delay_ms > 0 {
        thread::sleep(std::time::Duration::from_millis(response_delay_ms));
    }

    let (status, response) = if method == "GET" && path == "/v1/embedding/identity" {
        let identity = state.lock().unwrap().embedding_identity_sha256.clone();
        (
            200,
            json!({"data": {
                "version": 1,
                "backend": "lexical",
                "identity_sha256": identity,
                "dimensions": 384,
                "semantic_contract": "semantic_text-derived-vector.v1",
                "resumable": true
            }, "took_ms": 0, "request_id": "test"}),
        )
    } else if method == "PUT" && path == "/autoindex-catalog" {
        if state.lock().unwrap().catalog_preexists {
            (
                400,
                json!({"error": {"type": "resource_already_exists_exception"}}),
            )
        } else {
            (200, json!({"acknowledged": true}))
        }
    } else if method == "PUT" && path == "/autoindex-catalog/_mapping" {
        let mapping: Value = serde_json::from_slice(&body).unwrap();
        let started_requested = mapping.pointer("/properties/started").is_some();
        let required = ["summary_generated_at", "invocation_telemetry_scope"];
        let upgraded = required.iter().all(|field| {
            mapping
                .pointer(&format!("/properties/{field}/type"))
                .and_then(Value::as_str)
                .is_some()
        });
        let mut locked = state.lock().unwrap();
        locked.started_mapping_requested |= started_requested;
        if locked.catalog_preexists && started_requested {
            // The shape a real engine returns when the additive upgrade asks
            // for `started` on a legacy (v1.0.0-rc.4) catalog, where `started`
            // was dynamically inferred as `text`. Measured against a live
            // v1.0.0-rc.13 engine, not read off the handler: the request falls
            // past the `illegal_argument_exception` guard — that one only sees
            // *declared* mappings, and `started` was never declared — and is
            // refused by the `idx.schema()` guard, which returns
            // `XerjError::invalid_mapping` and therefore a 400
            // `mapper_parsing_exception`.
            //
            // A correct product never reaches this branch (see
            // `started_mapping_requested`, asserted false in
            // `existing_catalog_is_upgraded_before_new_run_metadata_is_written`).
            // It is kept, and kept accurate, so that adding `started` to the
            // additive upgrade fails here with the message the operator would
            // really see rather than passing silently.
            let reason = "field [started] already exists as [text], cannot add [date]";
            (
                400,
                json!({
                    "error": {
                        "root_cause": [{"type": "mapper_parsing_exception", "reason": reason}],
                        "type": "mapper_parsing_exception",
                        "reason": reason,
                    },
                    "status": 400,
                }),
            )
        } else {
            locked.catalog_mapping_upgraded = upgraded;
            (200, json!({"acknowledged": true}))
        }
    } else if method == "POST" && path == "/_bulk" {
        (200, bulk_response(&body, state))
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
        (200, json!({"deleted": 0, "failures": []}))
    } else if method == "GET" && path.ends_with("/_count") {
        (200, json!({"count": state.lock().unwrap().docs.len()}))
    } else if method == "POST" && path.ends_with("/_search") {
        // Report the REAL number of stored docs: `run_index` now verifies
        // live counts against the journal (#195), so a mock that always
        // answered 0 hits would (rightly) fail every successful run.
        let total = state.lock().unwrap().docs.len();
        (
            200,
            json!({"hits":{"total":{"value":total},"hits":[]},"aggregations":{}}),
        )
    } else {
        // ping, index creation/mapping, refresh, and catalog operations
        (200, json!({"acknowledged": true}))
    };
    let bytes = response.to_string();
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
        // Catalog bulks mix `index` and `delete` actions (stale aliases, and
        // the junk sweep of #238), so walk the NDJSON rather than assuming
        // action/document pairs — a delete carries no document line.
        let mut locked = state.lock().unwrap();
        if locked.catalog_preexists && !locked.catalog_mapping_upgraded {
            locked.catalog_bulk_before_upgrade = true;
        }
        let mut line = 0;
        while line < lines.len() {
            let Ok(action) = serde_json::from_slice::<Value>(lines[line]) else {
                line += 1;
                continue;
            };
            if action.pointer("/delete/_index").and_then(Value::as_str)
                == Some(catalog::CATALOG_INDEX)
            {
                if let Some(id) = action.pointer("/delete/_id").and_then(Value::as_str) {
                    locked.catalog_docs.remove(id);
                }
                line += 1;
            } else if action.pointer("/index/_index").and_then(Value::as_str)
                == Some(catalog::CATALOG_INDEX)
                && line + 1 < lines.len()
            {
                let id = action.pointer("/index/_id").unwrap().as_str().unwrap();
                let doc: Value = serde_json::from_slice(lines[line + 1]).unwrap();
                locked.catalog_docs.insert(id.to_owned(), doc);
                line += 2;
            } else {
                // Fixture drift, not a product signal. Recorded rather than
                // panicked on: this runs on the server thread with the state
                // guard held, so a panic here poisons the `Mutex` and turns
                // `MockEndpoint::drop`'s `join().unwrap()` into a second,
                // misleading panic. Tests assert on it from the test thread.
                locked
                    .unexpected_catalog_actions
                    .push(format!("unexpected catalog bulk action: {action}"));
                line += 1;
            }
        }
        return json!({"errors": false, "items": []});
    }

    let mut locked = state.lock().unwrap();
    locked.data_bulk_number += 1;
    if locked.block_writes {
        // Explicit write block: per-item 403 (never 429/5xx), nothing applied.
        let items: Vec<Value> = lines
            .chunks_exact(2)
            .map(|_| {
                json!({"index": {
                    "status": 403,
                    "error": {
                        "type": "cluster_block_exception",
                        "reason": "index [failure-test-csv] is blocked for write operations",
                        "status": 403
                    }
                }})
            })
            .collect();
        return json!({"errors": true, "items": items});
    }
    if locked.swallow_data_bulks {
        return json!({"errors": false, "items": []});
    }
    if locked.reject_first_data_item {
        let mut items = Vec::new();
        for (nth, pair) in lines.chunks_exact(2).enumerate() {
            if nth == 0 {
                items.push(json!({"index": {
                    "status": 400,
                    "error": {
                        "type": "document_parsing_exception",
                        "reason": "refused: value could not be parsed"
                    }
                }}));
                continue;
            }
            let action: Value = serde_json::from_slice(pair[0]).unwrap();
            let doc: Value = serde_json::from_slice(pair[1]).unwrap();
            let id = action.pointer("/index/_id").unwrap().as_str().unwrap();
            locked.docs.insert(id.to_owned(), doc);
            items.push(json!({"index": {"status": 201}}));
        }
        return json!({"errors": true, "items": items});
    }
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
        scan_workers: 1,
        pdf_workers: 1,
        resource_notes: Vec::new(),
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
        progress: crate::progress::ProgressMode::None,
        progress_interval: None,
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

fn dataset_catalog_docs(endpoint: &MockEndpoint) -> Vec<Value> {
    let mut docs: Vec<Value> = endpoint
        .state
        .lock()
        .unwrap()
        .catalog_docs
        .values()
        .filter(|doc| doc.get("doc_kind").and_then(Value::as_str) == Some("dataset"))
        .cloned()
        .collect();
    docs.sort_by_key(|doc| {
        doc.get("slug")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    });
    docs
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
    // A fixed endpoint delay makes the start/summary chronology
    // deterministic instead of relying on scheduler timing.
    let endpoint = MockEndpoint::start_with_delay(usize::MAX, 5);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);

    let (initial_code, initial_summary) = run_index_report(config.clone()).unwrap();
    assert_eq!(initial_code, 0);
    assert_eq!(endpoint.state.lock().unwrap().delete_calls, 0);
    assert_eq!(event_count(state_dir.path(), "plan"), 1);
    assert_eq!(event_count(state_dir.path(), "file_replace_start"), 1);
    let source_bytes = fs::metadata(corpus.path().join("records.csv"))
        .unwrap()
        .len();
    let initial_dataset = endpoint
        .state
        .lock()
        .unwrap()
        .catalog_docs
        .values()
        .find(|doc| doc.get("doc_kind").and_then(Value::as_str) == Some("dataset"))
        .cloned()
        .expect("dataset catalog document");
    let initial_dataset_bytes = initial_dataset
        .get("bytes")
        .and_then(Value::as_u64)
        .unwrap();
    assert_eq!(initial_dataset_bytes, source_bytes);
    let initial_summary = initial_summary.unwrap();
    assert_eq!(
        initial_summary["invocation_telemetry_scope"],
        json!("latest_invocation_of_durable_run")
    );
    let initial_started =
        chrono::DateTime::parse_from_rfc3339(initial_summary["started"].as_str().unwrap()).unwrap();
    let initial_summary_generated_at = chrono::DateTime::parse_from_rfc3339(
        initial_summary["summary_generated_at"].as_str().unwrap(),
    )
    .unwrap();
    assert!(
        initial_summary_generated_at - initial_started >= chrono::Duration::milliseconds(5),
        "started must be captured before work, not synthesized with the summary"
    );

    let (resume_code, resume_summary) = run_index_report(config).unwrap();
    assert_eq!(resume_code, 0);
    assert_eq!(endpoint.state.lock().unwrap().delete_calls, 0);
    assert_eq!(
        event_count(state_dir.path(), "plan"),
        1,
        "an identical resume must reuse the durable plan"
    );
    let resumed_dataset = endpoint
        .state
        .lock()
        .unwrap()
        .catalog_docs
        .values()
        .find(|doc| doc.get("doc_kind").and_then(Value::as_str) == Some("dataset"))
        .cloned()
        .expect("dataset catalog document after resume");
    assert_eq!(
        resumed_dataset["bytes"],
        json!(source_bytes),
        "an unchanged resume must not replace durable dataset bytes with zero"
    );
    assert_eq!(
        resume_summary.unwrap()["invocation_telemetry_scope"],
        json!("latest_invocation_of_durable_run")
    );
}

#[test]
fn changed_source_replaces_durable_dataset_bytes_across_reopen() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let path = corpus.path().join("records.csv");
    fs::write(&path, "id,value\n0,old\n").unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);

    assert_eq!(run_index(config.clone()).unwrap(), 0);
    let initial_bytes = fs::metadata(&path).unwrap().len();
    assert_eq!(dataset_catalog_docs(&endpoint)[0]["bytes"], initial_bytes);

    fs::write(
        &path,
        "id,value,description\n0,new,a longer replacement source\n1,new,second row\n",
    )
    .unwrap();
    let replacement_bytes = fs::metadata(&path).unwrap().len();
    assert_ne!(replacement_bytes, initial_bytes);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    // Reopen the durable journal independently of the product invocation.
    let root = config.root.canonicalize().unwrap();
    let reopened = state::Journal::open(
        state_dir.path(),
        &root.to_string_lossy(),
        &config.url,
        &config.prefix,
        config.bulk_timeout_secs,
        false,
    )
    .unwrap();
    assert_eq!(reopened.done.len(), 1);
    assert_eq!(
        reopened.done.values().next().unwrap().bytes,
        replacement_bytes
    );
    drop(reopened);

    // A following unchanged product rescan must publish the replacement
    // generation's durable bytes, not zero and not the historical size.
    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(
        dataset_catalog_docs(&endpoint)[0]["bytes"],
        replacement_bytes
    );
}

#[test]
fn deleted_path_bytes_remain_while_its_documents_remain_live() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let removed = corpus.path().join("removed.csv");
    let retained = corpus.path().join("retained.csv");
    fs::write(&removed, "id,value\n0,removed\n1,removed\n").unwrap();
    fs::write(&retained, "id,value\n2,retained\n3,retained\n").unwrap();
    let durable_live_bytes =
        fs::metadata(&removed).unwrap().len() + fs::metadata(&retained).unwrap().len();
    let endpoint = MockEndpoint::start(usize::MAX);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);

    assert_eq!(run_index(config.clone()).unwrap(), 0);
    assert_eq!(
        dataset_catalog_docs(&endpoint)[0]["bytes"],
        durable_live_bytes
    );
    assert_eq!(endpoint.state.lock().unwrap().docs.len(), 4);

    fs::remove_file(&removed).unwrap();
    // Current autoindex semantics do not reconcile a missing canonical path:
    // its indexed documents and FileDone remain live. Dataset bytes must
    // describe that durable live index rather than silently pretend deletion.
    for _ in 0..2 {
        assert_eq!(run_index(config.clone()).unwrap(), 0);
        assert_eq!(endpoint.state.lock().unwrap().docs.len(), 4);
        assert_eq!(
            dataset_catalog_docs(&endpoint)[0]["bytes"],
            durable_live_bytes
        );
    }
}

#[test]
fn product_path_counts_shared_source_once_per_distinct_dataset() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let path = corpus.path().join("finance.sqlite");
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE quarterly (quarter TEXT, revenue INTEGER);
             INSERT INTO quarterly VALUES ('2026-Q1', 100);
             CREATE TABLE annual (year INTEGER, filing TEXT);
             INSERT INTO annual VALUES (2026, '10-K');",
        )
        .unwrap();
    }
    let source_bytes = fs::metadata(&path).unwrap().len();
    let endpoint = MockEndpoint::start(usize::MAX);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);

    assert_eq!(run_index(config.clone()).unwrap(), 0);
    let root = config.root.canonicalize().unwrap();
    let mut journal = state::Journal::open(
        state_dir.path(),
        &root.to_string_lossy(),
        &config.url,
        &config.prefix,
        config.bulk_timeout_secs,
        false,
    )
    .unwrap();
    let mut plan = journal.plan.clone().unwrap();
    assert_eq!(plan.datasets.len(), 2);
    let assignment = plan.files.values_mut().next().unwrap();
    assert_eq!(assignment.assignments.len(), 2);
    // Model a compatible historical plan that repeated one group assignment.
    // Catalog accounting must deduplicate it by dataset slug.
    assignment
        .assignments
        .push(assignment.assignments[0].clone());
    journal.write_plan(&plan).unwrap();
    drop(journal);

    assert_eq!(run_index(config).unwrap(), 0);
    let datasets = dataset_catalog_docs(&endpoint);
    assert_eq!(datasets.len(), 2);
    assert!(datasets
        .iter()
        .all(|dataset| dataset["bytes"] == json!(source_bytes)));
}

#[test]
fn existing_catalog_is_upgraded_before_new_run_metadata_is_written() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("records.csv"), "id,value\n0,one\n").unwrap();
    let endpoint = MockEndpoint::start_with_existing_catalog();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);

    let (code, summary) = run_index_report(config).unwrap();
    assert_eq!(code, 0);
    let summary = summary.unwrap();
    assert!(summary["started"].is_string());
    assert!(summary["summary_generated_at"].is_string());
    assert_eq!(
        summary["invocation_telemetry_scope"],
        json!("latest_invocation_of_durable_run")
    );
    let state = endpoint.state.lock().unwrap();
    assert!(state.catalog_mapping_upgraded);
    assert!(
        !state.started_mapping_requested,
        "the additive upgrade asked a legacy catalog for `started`; a real v1.0.0-rc.4 catalog \
         answers that with 400 mapper_parsing_exception (\"field [started] already exists as \
         [text], cannot add [date]\") and the run aborts before any document work — see the \
         tripwire on catalog::catalog_mapping"
    );
    assert!(
        !state.catalog_bulk_before_upgrade,
        "new run metadata must not reach a legacy catalog before its additive mapping upgrade"
    );
    assert!(
        state.unexpected_catalog_actions.is_empty(),
        "fixture does not model these catalog bulk actions: {:?}",
        state.unexpected_catalog_actions
    );
}

#[test]
fn unchanged_resume_keeps_durable_dataset_junk_and_coercion_notes() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("records.csv"), "id,value\n0,one\n").unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);

    assert_eq!(run_index(config.clone()).unwrap(), 0);
    let root = config.root.canonicalize().unwrap();
    let mut journal = state::Journal::open(
        state_dir.path(),
        &root.to_string_lossy(),
        &config.url,
        &config.prefix,
        config.bulk_timeout_secs,
        false,
    )
    .unwrap();
    let plan = journal.plan.clone().unwrap();
    let slug = plan.datasets[0].slug.clone();
    let mut completion = journal.done.values().next().unwrap().clone();
    completion.junk = 7;
    completion.dropped_by_dataset.insert(slug.clone(), 3);
    journal.file_done(&completion).unwrap();
    drop(journal);

    assert_eq!(run_index(config).unwrap(), 0);
    let dataset = dataset_catalog_docs(&endpoint)
        .into_iter()
        .find(|doc| doc["slug"] == slug)
        .unwrap();
    assert_eq!(dataset["junk_records"], 7);
    assert!(dataset["notes"].as_array().unwrap().iter().any(|note| note
        .as_str()
        .is_some_and(|text| { text.contains("3 field values could not be coerced") })));
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
    assert_eq!(run_index(config.clone()).unwrap(), 3);
    {
        let locked = endpoint.state.lock().unwrap();
        assert_eq!(locked.docs.len(), 2);
        assert!(locked.docs.values().all(|doc| {
            doc["ax_path"].as_str() == Some("a.csv") && doc["value"].as_str() == Some("rewritten")
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
    assert_eq!(plan.junk_files.len(), 1);
    assert_eq!(plan.junk_files[0].rel, "b.csv");
    assert!(plan.junk_files[0]
        .reason
        .contains("key ownership exclusive"));
    drop(replay);

    // The divergence is durable and deterministic: an identical rerun keeps
    // the same owner, appends no new plan, and changes no documents.
    let plans_before = event_count(state_dir.path(), "plan");
    assert_eq!(run_index(config).unwrap(), 3);
    assert_eq!(event_count(state_dir.path(), "plan"), plans_before);
    let locked = endpoint.state.lock().unwrap();
    assert_eq!(locked.docs.len(), 2);
    assert!(locked
        .docs
        .values()
        .all(|doc| doc["value"].as_str() == Some("rewritten")));
}

/// #238: a file added after the plan was frozen is skipped and reported in the
/// catalog. That report used to be immortal — `new_unplanned` was a per-run
/// `Vec`, so once the file left the corpus nothing remembered the document had
/// ever been written and no run could remove it. The catalog is the data map
/// every `map`/`status`/agent query reads; it must not keep advertising a file
/// that is gone.
#[test]
fn an_added_then_deleted_file_leaves_no_immortal_catalog_entry() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("planned.csv"),
        "id,value\n0,planned\n1,planned\n",
    )
    .unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    // Appears after the plan froze: not indexed, but reported as skipped.
    fs::write(
        corpus.path().join("added.csv"),
        "id,value\n2,added\n3,added\n",
    )
    .unwrap();
    assert_eq!(run_index(config.clone()).unwrap(), 3);
    let skipped_id = {
        let locked = endpoint.state.lock().unwrap();
        let (id, doc) = locked
            .catalog_docs
            .iter()
            .find(|(_, doc)| doc["path"].as_str() == Some("added.csv"))
            .expect("a file skipped for being post-freeze is reported in the catalog");
        assert_eq!(doc["status"].as_str(), Some("skipped"));
        assert_eq!(doc["doc_kind"].as_str(), Some("file"));
        id.clone()
    };

    // The durable plan is the only record that the document exists. Without
    // it no later run can ever delete it.
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
        replay
            .plan
            .as_ref()
            .unwrap()
            .junk_files
            .iter()
            .any(|junk| junk.rel == "added.csv"),
        "the skipped file must be recorded in the durable plan"
    );
    drop(replay);

    // Delete it: the catalog entry goes with it, and the run is clean again.
    fs::remove_file(corpus.path().join("added.csv")).unwrap();
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    {
        let locked = endpoint.state.lock().unwrap();
        assert!(
            !locked.catalog_docs.contains_key(&skipped_id),
            "the catalog entry for a deleted skipped file must not survive"
        );
        assert!(
            locked
                .catalog_docs
                .values()
                .all(|doc| doc["path"].as_str() != Some("added.csv")),
            "no catalog document may still name the deleted file"
        );
        // The file that is still there keeps its entry.
        assert!(locked
            .catalog_docs
            .values()
            .any(|doc| doc["path"].as_str() == Some("planned.csv")));
    }

    // Converged: nothing left to add or sweep, so no further plan is appended.
    let plans_before = event_count(state_dir.path(), "plan");
    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(event_count(state_dir.path(), "plan"), plans_before);
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
        assert_eq!(run_index(config.clone()).unwrap(), 0);
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

/// #195: an index write block rejects every bulk item with status 403 (the
/// status ES assigns explicit/API blocks — only the flood-stage
/// `read_only_allow_delete` block is 429). A status-based classifier filed
/// those items under "junk source records", journaled the file COMPLETE, and
/// the instructed rerun then resumed past the journal and reported success
/// over an empty index. Blocks must be classified by error type: fatal,
/// file left pending, and the rerun after the block lifts must really index.
#[test]
fn write_block_rejections_are_backend_fatal_and_the_file_stays_pending() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("records.csv"),
        "id,value\n0,blocked\n1,blocked\n",
    )
    .unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);

    endpoint.state.lock().unwrap().block_writes = true;
    let error = format!("{:#}", run_index(config.clone()).unwrap_err());
    assert!(error.contains("blocked"), "{error}");
    assert_eq!(
        file_done_count(state_dir.path()),
        0,
        "a write-blocked source file must stay pending in the journal"
    );
    assert!(endpoint.state.lock().unwrap().docs.is_empty());

    // Block lifted: the SAME command must resume and actually index.
    endpoint.state.lock().unwrap().block_writes = false;
    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(file_done_count(state_dir.path()), 1);
    assert_eq!(endpoint.state.lock().unwrap().docs.len(), 2);
}

/// Pins WHY the run document carries no backend-rejection count, so nobody
/// "restores" one that could only ever read 0.
///
/// Making `junk_records_total` durable narrowed it: it no longer folds in the
/// per-item rejections that `record_bulk_outcome` counts. That looks like a
/// lost signal, and it would be one — except that a single per-item rejection
/// also lands in `bulk_errors`, which aborts the run before the run document,
/// the map, the summary line and the exit code exist at all. So the narrowed
/// number was unreachable in any published map either way.
///
/// The rejection count is therefore reported in the one place a rejected run
/// ever reaches: the abort message, where it supplies the scale that
/// `bulk_errors`' first-five sample cannot.
#[test]
fn backend_rejected_records_abort_the_run_before_any_map_is_written() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("records.csv"),
        "id,value\n0,kept\n1,kept\n2,kept\n",
    )
    .unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    endpoint.state.lock().unwrap().reject_first_data_item = true;
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);

    // A per-item 400 is NOT a server error (not 429, not 5xx, not a block), so
    // it is the weakest rejection the classifier recognises — and even it is
    // fatal.
    let error = format!("{:#}", run_index_report(config).unwrap_err());
    assert!(
        error.contains("document_parsing_exception"),
        "the refusal itself must be quoted: {error}"
    );
    assert!(
        error.contains("The backend refused 1 record(s)."),
        "the abort must state how many records were refused, not just sample the errors: {error}"
    );

    // Measured, and deliberately pinned even though it is NOT what the abort
    // message implies: `record_bulk_outcome` treats a per-item rejection as
    // non-fatal to the worker (only `server_errors` set `send_err`), so the
    // file is journaled COMPLETE and the run aborts only at the end. A rerun
    // therefore resumes past this file and never retries the refused record,
    // while the message says failed sources "were not journaled complete".
    //
    // That divergence predates this branch — it is `main`'s behaviour for the
    // item-error class — and fixing it means changing when a worker treats a
    // rejection as fatal, which is a resume-semantics change with nothing to
    // do with map metadata. Recorded here so the next reader finds the fact
    // rather than the assumption; tracked in the PR body as a known gap.
    assert_eq!(
        file_done_count(state_dir.path()),
        1,
        "pre-existing: a per-item rejection still journals the source complete"
    );
}

/// #195 last-resort gate: a backend that ACCEPTS every bulk but persists
/// nothing (the shape of any rejection path the per-item classifier does
/// not recognise) must not yield a success exit. The journal says records
/// landed; the live count says zero; the run must fail and say how to
/// recover.
#[test]
fn zero_live_documents_after_journaled_records_fails_the_run() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _io_guard = state::FILE_DONE_IO_FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("records.csv"),
        "id,value\n0,ghost\n1,ghost\n",
    )
    .unwrap();
    let endpoint = MockEndpoint::start(usize::MAX);
    endpoint.state.lock().unwrap().swallow_data_bulks = true;
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);

    let error = format!("{:#}", run_index(config).unwrap_err());
    assert!(error.contains("verification failed"), "{error}");
    assert!(error.contains("0 documents are live"), "{error}");
    assert!(error.contains("--fresh"), "{error}");
}

/// #241 regression: a run must never be silent.
///
/// Before this fix `autoindex` printed a handful of phase banners, one line
/// per 200 completed files, and then nothing at all through the entire
/// `finalize` block — measured at 47-64% of wall in the issue and at 45% (100
/// of 222 s) on the corpus used to reproduce it here. An agent watching the
/// stream could not tell a running index from a hung one.
///
/// This asserts the three properties that make the stream readable, through
/// the REAL progress surface a production run uses:
///   1. phase B reports as it goes, with a percent and an ETA field;
///   2. the previously-silent finalize block reports at all;
///   3. every run on a surface that prints at all ends with a terminal line
///      stating the outcome in words, because exit 3 is a success an agent
///      otherwise reads as failure. (`--progress none` prints nothing —
///      asserted by `quiet_runs_emit_no_progress_stream_at_all` below.)
#[test]
fn a_run_reports_progress_through_every_phase_and_closes_the_stream() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _sink_guard = crate::progress::SINK_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    // Distinct bytes per file: byte-identical files collapse into one
    // canonical file plus aliases, which would make `files=8` a lie.
    for n in 0..8 {
        fs::write(
            corpus.path().join(format!("rows-{n}.csv")),
            format!("id,value\n{n}0,alpha\n{n}1,beta\n{n}2,gamma\n"),
        )
        .unwrap();
    }

    let endpoint = MockEndpoint::start(usize::MAX);
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    config.quiet = false;
    config.progress = crate::progress::ProgressMode::Plain;
    config.progress_interval = Some(std::time::Duration::from_secs(1));

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let (code, _report) = {
        let _sink = crate::progress::install_test_sink(&buffer);
        run_index_report(config).unwrap()
    };
    let stream = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();

    let progress_lines: Vec<&str> = stream
        .lines()
        .filter(|line| line.starts_with("xerj-progress "))
        .collect();
    assert!(
        progress_lines.len() >= 5,
        "a run must narrate its phases, got {} line(s):\n{stream}",
        progress_lines.len()
    );
    assert!(
        progress_lines
            .iter()
            .any(|line| line.contains("phase=index")),
        "phase B must report:\n{stream}"
    );
    assert!(
        progress_lines
            .iter()
            .any(|line| line.contains("phase=finalize")),
        "the finalize block was the longest silence in the tool; it must report now:\n{stream}"
    );
    assert!(
        progress_lines
            .iter()
            .all(|line| line.contains(" pct=") && line.contains(" eta_s=")),
        "every progress line carries a percent and an ETA field (possibly `unknown`):\n{stream}"
    );

    let done = stream
        .lines()
        .find(|line| line.starts_with("xerj-done "))
        .unwrap_or_else(|| panic!("a run must close its own stream:\n{stream}"));
    assert!(
        done.contains("ok=true") && done.contains(&format!("exit={code}")),
        "{done}"
    );
    assert!(done.contains("reason=completed"), "{done}");
    assert!(done.contains("files=8"), "{done}");
}

/// The other half of the contract: `--quiet` still means silence, so nothing
/// here can spam a caller that asked for none.
#[test]
fn quiet_runs_emit_no_progress_stream_at_all() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _sink_guard = crate::progress::SINK_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("rows.csv"), "id,value\n0,alpha\n").unwrap();

    let endpoint = MockEndpoint::start(usize::MAX);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    assert_eq!(config.progress, crate::progress::ProgressMode::None);

    let buffer = Arc::new(Mutex::new(Vec::new()));
    {
        let _sink = crate::progress::install_test_sink(&buffer);
        run_index_report(config).unwrap();
    }
    assert!(
        buffer.lock().unwrap().is_empty(),
        "--quiet asked for nothing: {}",
        String::from_utf8_lossy(&buffer.lock().unwrap())
    );
}

/// Where #240 (the resource policy) and #241 (the progress stream) meet.
///
/// Two rules have to hold at once, and each is easy to break while honouring
/// the other:
///
///  * The resource decision is **announced on the progress surface**, not on a
///    bare `eprintln!` gated on `!quiet`. A raw write would survive
///    `--progress none` — which promises silence — and would inject a
///    non-JSON line into the middle of `--progress json`, which promises one
///    parseable stream. (`quiet_runs_emit_no_progress_stream_at_all` above is
///    the other side of this assertion: it fails if any of these lines escape
///    the surface.)
///  * The numbers announced are the ones the run **got**, not the ones it
///    asked for. `pool::configure` is first-call-wins, because rayon pools
///    cannot be resized, so a process that already indexed once — `xerj
///    brain` does exactly that — keeps its original phase-A width. Reporting
///    the request there would make the stream and the run summary a record of
///    an intention rather than of a run.
///
/// The second rule is made observable by asking for a width this process
/// provably cannot grant: the pool is built before the run, then one more
/// thread than it has is requested.
#[test]
fn the_resource_plan_is_announced_on_the_progress_surface_with_the_width_it_got() {
    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let _sink_guard = crate::progress::SINK_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("rows.csv"),
        "id,value\n0,alpha\n1,beta\n",
    )
    .unwrap();

    // Build the process-global scan pool first, so the width below is one this
    // run cannot possibly be granted.
    let installed = crate::pool::scan_pool().current_num_threads();
    let endpoint = MockEndpoint::start(usize::MAX);
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    config.quiet = false;
    config.progress = crate::progress::ProgressMode::Plain;
    config.progress_interval = Some(std::time::Duration::from_secs(1));
    config.scan_workers = installed + 1;
    config.workers = 3;
    config.resource_notes = vec!["memory safe zone 256 MiB allows 3 index workers".to_string()];

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let (_code, report) = {
        let _sink = crate::progress::install_test_sink(&buffer);
        run_index_report(config).unwrap()
    };
    let stream = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();

    assert!(
        stream.contains(&format!("{installed} scan threads")),
        "the run must announce the phase-A width it actually got, not {}: {stream}",
        installed + 1
    );
    assert!(
        !stream.contains(&format!("{} scan threads", installed + 1)),
        "a width the pool refused must never be reported as fact: {stream}"
    );
    assert!(
        stream.contains("3 index workers"),
        "phase B must name the count the policy chose: {stream}"
    );
    assert!(
        stream.contains("memory safe zone 256 MiB allows 3 index workers"),
        "what the machine forced on the run belongs in the same stream: {stream}"
    );
    assert!(
        stream.contains(&format!("with {installed} threads")),
        "the phase-A banner must carry the real width too: {stream}"
    );

    // The summary is read after the fact by agents and by `xerj brain`; it has
    // to agree with the stream rather than repeat the request.
    let report = report.expect("a completed run produces a summary");
    assert_eq!(report["scan_workers"], serde_json::json!(installed));
    assert_eq!(report["workers"], serde_json::json!(3));
}

#[cfg(unix)]
#[test]
fn fresh_pdf_is_parsed_once_and_failed_publication_retry_reparses_once() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    inject_pdf_spool_capacity(state_dir.path());
    let tools = tempfile::tempdir().unwrap();
    let pdf = corpus.path().join("quarterly-report.pdf");
    fs::write(
        &pdf,
        b"%PDF-1.4\nfake bytes consumed by the isolated test worker\n",
    )
    .unwrap();
    fs::copy(&pdf, corpus.path().join("quarterly-report-copy.pdf")).unwrap();

    let count = tools.path().join("worker-count");
    let worker = tools.path().join("pdf-worker");
    let worker_script = String::from_utf8(
        br#"#!/bin/sh
printf 'parse\n' >> "$XERJ_TEST_PDF_COUNT"
printf '%s' '{"schema":1,"extractor":"xerj-autoindex/__XERJ_VERSION__","parser":"pdf_oxide/0.3.75","containment":"test worker","records":[{"fields":{"title":"quarterly-report","page":1,"body":"Quarterly revenue increased while operating margin improved.","pdf_pages_total":1,"pdf_pages_with_text":1,"pdf_pages_omitted":0},"locator":"p1-s0","group":null,"origin":"extractor"}]}'
"#
        .to_vec(),
    )
    .unwrap()
    .replace("__XERJ_VERSION__", env!("CARGO_PKG_VERSION"));
    fs::write(&worker, worker_script).unwrap();
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();

    let endpoint = MockEndpoint::start(1);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    let _env = PdfWorkerEnvGuard::acquire();
    std::env::set_var("XERJ_PDF_WORKER_BIN", &worker);
    std::env::set_var("XERJ_TEST_PDF_COUNT", &count);

    let first = run_index(config.clone()).unwrap_err();
    assert!(format!("{first:#}").contains("bulk backend failed"));
    assert_eq!(
        fs::read_to_string(&count).unwrap().lines().count(),
        1,
        "Phase A's extraction must be replayed in Phase B, not parsed again"
    );
    assert_eq!(file_done_count(state_dir.path()), 0);

    // The process-local spool is intentionally not journal state. A retry has
    // a frozen plan, performs exactly one fresh extraction in Phase B, and
    // commits only after the backend accepts it.
    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(fs::read_to_string(&count).unwrap().lines().count(), 2);
    assert_eq!(file_done_count(state_dir.path()), 1);
    assert_eq!(endpoint.state.lock().unwrap().docs.len(), 1);
}

#[cfg(unix)]
#[test]
fn refused_pdf_spool_reparses_and_reports_fallback_without_weakening_publication() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("quarterly-report.pdf"),
        b"%PDF-1.4\nfake bytes consumed by the isolated test worker\n",
    )
    .unwrap();
    fs::write(
        state_dir
            .path()
            .join(".autoindex-test-pdf-spool-available-bytes"),
        ((4_u64 << 30) + (32 << 20) - 1).to_string(),
    )
    .unwrap();
    fs::write(
        state_dir.path().join(".autoindex-test-pdf-spool-fd-limit"),
        "4096",
    )
    .unwrap();
    fs::write(
        state_dir.path().join(".autoindex-test-pdf-spool-fd-open"),
        "16",
    )
    .unwrap();

    let count = tools.path().join("worker-count");
    let worker = tools.path().join("pdf-worker");
    let worker_script = String::from_utf8(
        br#"#!/bin/sh
printf 'parse\n' >> "$XERJ_TEST_PDF_COUNT"
printf '%s' '{"schema":1,"extractor":"xerj-autoindex/__XERJ_VERSION__","parser":"pdf_oxide/0.3.75","containment":"test worker","records":[{"fields":{"title":"quarterly-report","page":1,"body":"Quarterly revenue increased while operating margin improved.","pdf_pages_total":1,"pdf_pages_with_text":1,"pdf_pages_omitted":0},"locator":"p1-s0","group":null,"origin":"extractor"}]}'
"#
        .to_vec(),
    )
    .unwrap()
    .replace("__XERJ_VERSION__", env!("CARGO_PKG_VERSION"));
    fs::write(&worker, worker_script).unwrap();
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();

    let endpoint = MockEndpoint::start(0);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    let _env = PdfWorkerEnvGuard::acquire();
    std::env::set_var("XERJ_PDF_WORKER_BIN", &worker);
    std::env::set_var("XERJ_TEST_PDF_COUNT", &count);

    let (code, report) = run_index_report(config).unwrap();
    assert_eq!(code, 0);
    assert_eq!(fs::read_to_string(&count).unwrap().lines().count(), 2);
    assert_eq!(file_done_count(state_dir.path()), 1);
    assert_eq!(endpoint.state.lock().unwrap().docs.len(), 1);
    let reuse = report.unwrap()["pdf_extraction_reuse"].clone();
    assert_eq!(reuse["reservations_started"], 0);
    assert_eq!(reuse["capacity_fallbacks"], 1);
    assert_eq!(reuse["phase_b_pdf_parses"], 1);
    assert_eq!(reuse["replay_verified"], 0);
    assert_eq!(reuse["artifacts_not_created"], 1);
    assert_eq!(reuse["phase_a_pdf_parser_responses"], 1);
    assert_eq!(reuse["capacity_status"], "disabled");
    assert_eq!(
        reuse["fallback_examples"][0]["path"],
        "quarterly-report.pdf"
    );
}

#[cfg(unix)]
#[test]
fn refused_spool_does_not_pay_a_second_phase_a_source_hash() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    let pdf = corpus.path().join("quarterly-report.pdf");
    fs::write(&pdf, b"%PDF-1.4\nsame-size-source-generation\n").unwrap();
    let size = fs::metadata(&pdf).unwrap().len();
    let digest = content::resolve_reporting(
        vec![crate::walk::FileEntry {
            path: pdf.clone(),
            rel: "quarterly-report.pdf".into(),
            rel_id: "quarterly-report.pdf".into(),
            is_symlink: false,
            size,
        }],
        &|_| {},
    )
    .unwrap()
    .digests
    .remove(0);

    let worker = tools.path().join("pdf-worker");
    let worker_script = String::from_utf8(
        br#"#!/bin/sh
printf 'Z' | dd of="$2" bs=1 seek=10 conv=notrunc 2>/dev/null
printf '%s' '{"schema":1,"extractor":"xerj-autoindex/__XERJ_VERSION__","parser":"pdf_oxide/0.3.75","containment":"test worker","records":[{"fields":{"body":"Quarterly revenue increased while operating margin improved."},"locator":"p1-s0","group":null,"origin":"extractor"}]}'
"#
        .to_vec(),
    )
    .unwrap()
    .replace("__XERJ_VERSION__", env!("CARGO_PKG_VERSION"));
    fs::write(&worker, worker_script).unwrap();
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();
    let _env = PdfWorkerEnvGuard::acquire();
    std::env::set_var("XERJ_PDF_WORKER_BIN", &worker);

    let budget = crate::extract::pdf::ExtractionSpoolBudget::new(0, 0);
    let progress = Progress::silent();
    let scan = scan_file(
        &pdf,
        size,
        &digest,
        &PhaseAContext {
            state_dir: state_dir.path(),
            budget: &budget,
            capacity_warning: None,
            progress: &progress,
        },
        100,
        1,
    );
    assert!(scan.pdf_spool.is_none());
    assert_eq!(scan.pdf_spool_fallbacks.len(), 1);
    for fallback in &scan.pdf_spool_fallbacks {
        budget.record_fallback_example(
            "quarterly-report.pdf",
            fallback.category,
            &fallback.message,
        );
    }
    let report = budget.report();
    assert_eq!(report["artifacts_created"], 0);
    assert_eq!(report["phase_b_eligible_artifacts"], 0);
    assert_eq!(report["artifacts_discarded_before_replay"], 0);
    assert_eq!(report["fallback_categories"]["artifact_count_ceiling"], 1);
    assert!(report["fallback_categories"]
        .get("source_generation_changed")
        .is_none());
}

/// The other half of the branch above: with capacity available the artifact
/// *is* created, and phase A must then throw it away because the source moved
/// underneath the parser. Without this the source-generation arm of
/// `scan_file` has no test at all — its sibling above deliberately runs with a
/// zero budget, so it never reaches the `content::verify` comparison.
#[cfg(unix)]
#[test]
fn source_changed_during_phase_a_discards_the_artifact_and_names_the_reason() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    inject_pdf_spool_capacity(state_dir.path());
    let tools = tempfile::tempdir().unwrap();
    let pdf = corpus.path().join("quarterly-report.pdf");
    fs::write(&pdf, b"%PDF-1.4\nsame-size-source-generation\n").unwrap();
    let size = fs::metadata(&pdf).unwrap().len();
    let digest = content::resolve_reporting(
        vec![crate::walk::FileEntry {
            path: pdf.clone(),
            rel: "quarterly-report.pdf".into(),
            rel_id: "quarterly-report.pdf".into(),
            is_symlink: false,
            size,
        }],
        &|_| {},
    )
    .unwrap()
    .digests
    .remove(0);

    // Same trick as the sibling test: the worker rewrites one byte of its own
    // input, so the file keeps its length and changes its digest.
    let worker = tools.path().join("pdf-worker");
    let worker_script = String::from_utf8(
        br#"#!/bin/sh
printf 'Z' | dd of="$2" bs=1 seek=10 conv=notrunc 2>/dev/null
printf '%s' '{"schema":1,"extractor":"xerj-autoindex/__XERJ_VERSION__","parser":"pdf_oxide/0.3.75","containment":"test worker","records":[{"fields":{"body":"Quarterly revenue increased while operating margin improved."},"locator":"p1-s0","group":null,"origin":"extractor"}]}'
"#
        .to_vec(),
    )
    .unwrap()
    .replace("__XERJ_VERSION__", env!("CARGO_PKG_VERSION"));
    fs::write(&worker, worker_script).unwrap();
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();
    let _env = PdfWorkerEnvGuard::acquire();
    std::env::set_var("XERJ_PDF_WORKER_BIN", &worker);

    let (budget, _) =
        crate::extract::pdf::ExtractionSpoolBudget::for_state_dir(state_dir.path(), 1, 1, 8);
    let progress = Progress::silent();
    let scan = scan_file(
        &pdf,
        size,
        &digest,
        &PhaseAContext {
            state_dir: state_dir.path(),
            budget: &budget,
            capacity_warning: None,
            progress: &progress,
        },
        100,
        1,
    );

    assert!(
        scan.pdf_spool.is_none(),
        "an artifact bound to a superseded source generation must not reach phase B"
    );
    let categories: Vec<&str> = scan
        .pdf_spool_fallbacks
        .iter()
        .map(|fallback| fallback.category)
        .collect();
    assert_eq!(categories, vec!["source_generation_changed"]);
    let report = budget.report();
    assert_eq!(report["artifacts_created"], 1);
    assert_eq!(report["artifacts_discarded_before_replay"], 1);
    assert_eq!(report["phase_b_eligible_artifacts"], 0);
    assert_eq!(report["current_live_artifacts"], 0);
    assert_eq!(report["current_retained_or_reserved_bytes"], 0);
    assert_eq!(
        report["fallback_categories"]["source_generation_changed"],
        1
    );
}

#[cfg(unix)]
#[test]
fn verified_but_junk_pdf_spool_is_created_then_discarded_not_eligible() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    inject_pdf_spool_capacity(state_dir.path());
    let tools = tempfile::tempdir().unwrap();
    let pdf = corpus.path().join("empty-report.pdf");
    fs::write(&pdf, b"%PDF-1.4\nempty test input\n").unwrap();
    let size = fs::metadata(&pdf).unwrap().len();
    let digest = content::resolve_reporting(
        vec![crate::walk::FileEntry {
            path: pdf.clone(),
            rel: "empty-report.pdf".into(),
            rel_id: "empty-report.pdf".into(),
            is_symlink: false,
            size,
        }],
        &|_| {},
    )
    .unwrap()
    .digests
    .remove(0);
    let worker = tools.path().join("pdf-worker");
    let script = format!(
        "#!/bin/sh\nprintf '%s' '{{\"schema\":1,\"extractor\":\"xerj-autoindex/{}\",\"parser\":\"pdf_oxide/0.3.75\",\"containment\":\"test worker\",\"records\":[]}}'\n",
        env!("CARGO_PKG_VERSION")
    );
    fs::write(&worker, script).unwrap();
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();
    let _env = PdfWorkerEnvGuard::acquire();
    std::env::set_var("XERJ_PDF_WORKER_BIN", &worker);

    let (budget, _) =
        crate::extract::pdf::ExtractionSpoolBudget::for_state_dir(state_dir.path(), 1, 1, 8);
    let progress = Progress::silent();
    let mut scan = scan_file(
        &pdf,
        size,
        &digest,
        &PhaseAContext {
            state_dir: state_dir.path(),
            budget: &budget,
            capacity_warning: None,
            progress: &progress,
        },
        100,
        1,
    );
    assert!(scan.junk.is_some());
    assert!(scan.pdf_spool.is_some());
    assert!(take_pdf_spool_if_indexable(&mut scan.pdf_spool, true, &budget).is_none());
    let report = budget.report();
    assert_eq!(report["artifacts_created"], 1);
    assert_eq!(report["artifacts_discarded_before_replay"], 1);
    assert_eq!(report["phase_b_eligible_artifacts"], 0);
}

#[cfg(unix)]
#[test]
fn multi_pdf_run_reuses_every_artifact_and_reports_exact_success_counters() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    inject_pdf_spool_capacity(state_dir.path());
    let tools = tempfile::tempdir().unwrap();
    const PDFS: usize = 6;
    for index in 0..PDFS {
        fs::write(
            corpus.path().join(format!("quarterly-report-{index}.pdf")),
            format!("%PDF-1.4\nunique isolated worker input {index}\n"),
        )
        .unwrap();
    }

    let count = tools.path().join("worker-count");
    let worker = tools.path().join("pdf-worker");
    let worker_script = String::from_utf8(
        br#"#!/bin/sh
printf 'parse\n' >> "$XERJ_TEST_PDF_COUNT"
printf '%s' '{"schema":1,"extractor":"xerj-autoindex/__XERJ_VERSION__","parser":"pdf_oxide/0.3.75","containment":"test worker","records":[{"fields":{"title":"quarterly-report","page":1,"body":"Quarterly revenue increased while operating margin improved.","pdf_pages_total":1,"pdf_pages_with_text":1,"pdf_pages_omitted":0},"locator":"p1-s0","group":null,"origin":"extractor"}]}'
"#
        .to_vec(),
    )
    .unwrap()
    .replace("__XERJ_VERSION__", env!("CARGO_PKG_VERSION"));
    fs::write(&worker, worker_script).unwrap();
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();

    let endpoint = MockEndpoint::start(0);
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    config.workers = 8;
    config.pdf_workers = 2;
    let _env = PdfWorkerEnvGuard::acquire();
    std::env::set_var("XERJ_PDF_WORKER_BIN", &worker);
    std::env::set_var("XERJ_TEST_PDF_COUNT", &count);
    let (code, report) = run_index_report(config).unwrap();
    assert_eq!(code, 0);
    assert_eq!(
        fs::read_to_string(&count).unwrap().lines().count(),
        PDFS,
        "each unique PDF must be parsed once in Phase A and replayed in Phase B"
    );
    assert_eq!(endpoint.state.lock().unwrap().docs.len(), PDFS);
    let reuse = report.unwrap()["pdf_extraction_reuse"].clone();
    assert_eq!(reuse["reservations_started"], PDFS);
    assert_eq!(
        reuse["cumulative_reserved_bytes"],
        PDFS as u64 * (32_u64 << 20)
    );
    assert_eq!(reuse["artifacts_created"], PDFS);
    assert_eq!(reuse["artifacts_not_created"], 0);
    assert_eq!(reuse["phase_b_eligible_artifacts"], PDFS);
    assert_eq!(reuse["artifacts_discarded_before_replay"], 0);
    assert!(reuse["exact_artifact_bytes"].as_u64().unwrap() > 0);
    assert_eq!(reuse["current_retained_or_reserved_bytes"], 0);
    assert!(reuse["peak_retained_or_reserved_bytes"].as_u64().unwrap() > 0);
    assert_eq!(reuse["current_live_artifacts"], 0);
    assert!(reuse["peak_live_artifacts"].as_u64().unwrap() > 0);
    assert_eq!(reuse["phase_a_pdf_parser_responses"], PDFS);
    assert_eq!(reuse["capacity_fallbacks"], 0);
    assert_eq!(reuse["io_fallbacks"], 0);
    assert_eq!(reuse["replay_verified"], PDFS);
    assert_eq!(reuse["replay_integrity_failures"], 0);
    assert_eq!(reuse["phase_b_pdf_parses"], 0);
    assert_eq!(reuse["fallback_examples"].as_array().unwrap().len(), 0);
    assert_eq!(reuse["fallback_examples_truncated"], false);
    assert_eq!(reuse["fallback_categories"].as_object().unwrap().len(), 0);
}

#[cfg(unix)]
#[test]
fn corrupted_pdf_replay_reparses_without_blaming_the_source_or_backend() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = FAILPOINT_TEST_LOCK.lock().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    inject_pdf_spool_capacity(state_dir.path());
    let tools = tempfile::tempdir().unwrap();
    let pdf = corpus.path().join("report.pdf");
    fs::write(&pdf, b"%PDF-1.4\ntest worker input\n").unwrap();
    let worker = tools.path().join("pdf-worker");
    let count = tools.path().join("worker-count");
    let worker_script = String::from_utf8(
        br#"#!/bin/sh
printf 'parse\n' >> "$XERJ_TEST_PDF_COUNT"
printf '%s' '{"schema":1,"extractor":"xerj-autoindex/__XERJ_VERSION__","parser":"pdf_oxide/0.3.75","containment":"test worker","records":[{"fields":{"body":"Quarterly revenue increased while operating margin improved."},"locator":"p1-s0","group":null,"origin":"extractor"}]}'
"#
        .to_vec(),
    )
    .unwrap()
    .replace("__XERJ_VERSION__", env!("CARGO_PKG_VERSION"));
    fs::write(&worker, worker_script).unwrap();
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();

    let endpoint = MockEndpoint::start(0);
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    let _env = PdfWorkerEnvGuard::acquire();
    std::env::set_var("XERJ_PDF_WORKER_BIN", &worker);
    std::env::set_var("XERJ_TEST_PDF_COUNT", &count);
    crate::extract::pdf::corrupt_replay_for_source_size(fs::metadata(&pdf).unwrap().len());

    let (code, report) = run_index_report(config).unwrap();
    assert_eq!(code, 0);
    assert_eq!(fs::read_to_string(&count).unwrap().lines().count(), 2);
    assert_eq!(endpoint.state.lock().unwrap().docs.len(), 1);
    assert_eq!(file_done_count(state_dir.path()), 1);
    assert!(crate::extract::pdf::corrupted_replay_reservation_was_dropped());
    let reuse = report.unwrap()["pdf_extraction_reuse"].clone();
    assert_eq!(reuse["replay_verified"], 0);
    assert_eq!(reuse["replay_integrity_failures"], 1);
    assert_eq!(
        reuse["io_fallbacks"], 0,
        "a corrupted artifact is an integrity failure, not an I/O failure"
    );
    assert_eq!(reuse["phase_b_pdf_parses"], 1);
    assert_eq!(reuse["current_live_artifacts"], 0);
    assert_eq!(reuse["current_retained_or_reserved_bytes"], 0);
    assert_eq!(reuse["fallback_categories"]["replay_verification"], 1);
    assert_eq!(
        reuse["fallback_examples"][0]["category"],
        "replay_verification"
    );
}

#[test]
fn junk_scan_drops_its_spool_before_indexable_spools_are_retained() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct DropProbe(Arc<AtomicUsize>);
    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    let drops = Arc::new(AtomicUsize::new(0));
    let budget = crate::extract::pdf::ExtractionSpoolBudget::new(1, 1);
    let mut junk = Some(DropProbe(Arc::clone(&drops)));
    assert!(take_pdf_spool_if_indexable(&mut junk, true, &budget).is_none());
    assert!(junk.is_none());
    assert_eq!(drops.load(Ordering::SeqCst), 1);

    let mut indexable = Some(DropProbe(Arc::clone(&drops)));
    let retained = take_pdf_spool_if_indexable(&mut indexable, false, &budget);
    assert!(retained.is_some());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let report = budget.report();
    assert_eq!(report["artifacts_discarded_before_replay"], 1);
    assert_eq!(
        report["phase_b_eligible_artifacts"], 0,
        "eligibility is recorded only after the final todo set is known"
    );
    drop(retained);
    assert_eq!(drops.load(Ordering::SeqCst), 2);
}
