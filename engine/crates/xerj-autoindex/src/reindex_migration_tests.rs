//! Re-indexing a prefix in place must not leave a second copy behind.
//!
//! `ids::doc_id` mixes the dataset slug in, so a file that lands in a different
//! inferred dataset than it did last run is written under a NEW `_id` in a NEW
//! index. Nothing overwrites the old copy, and the per-file delete-before-replace
//! cannot reach it because it only clears the file's CURRENT assignment. The
//! mock endpoint here keys documents by `(index, _id)` — exactly the identity
//! the real backend uses — because a mock keyed by `_id` alone would silently
//! merge the two copies and pass.

use super::*;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Default)]
struct MockState {
    /// (index, _id) → source. Keyed the way a real cluster keys documents.
    docs: HashMap<(String, String), Value>,
    stop: bool,
}

impl MockState {
    fn live_under(&self, prefix: &str) -> usize {
        let pattern = format!("{prefix}-");
        self.docs
            .keys()
            .filter(|(index, _)| index.starts_with(&pattern))
            .count()
    }

    /// Every live copy of one source path, as (index, ax_run) pairs.
    fn copies_of(&self, prefix: &str, path: &str) -> Vec<(String, String)> {
        let pattern = format!("{prefix}-");
        let mut out: Vec<(String, String)> = self
            .docs
            .iter()
            .filter(|((index, _), doc)| {
                index.starts_with(&pattern)
                    && doc.get("ax_path").and_then(Value::as_str) == Some(path)
            })
            .map(|((index, _), doc)| {
                (
                    index.clone(),
                    doc.get("ax_run")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                )
            })
            .collect();
        out.sort();
        out
    }

    fn indices(&self) -> Vec<String> {
        let mut names: Vec<String> = self.docs.keys().map(|(index, _)| index.clone()).collect();
        names.sort();
        names.dedup();
        names
    }
}

struct MockEndpoint {
    url: String,
    state: Arc<Mutex<MockState>>,
    join: Option<thread::JoinHandle<()>>,
}

impl MockEndpoint {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(MockState::default()));
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
        if let Ok(mut state) = self.state.lock() {
            state.stop = true;
        }
        // Wake the nonblocking listener even on heavily loaded test hosts.
        let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
        // Deliberately not unwrapped: a failing assertion unwinds through this
        // drop, and a panicking destructor aborts the process and hides the
        // assertion message that says what actually broke.
        let _ = self.join.take().unwrap().join();
    }
}

/// The query shapes autoindex actually sends to `_delete_by_query`: a bare
/// `term`, a `terms` set, and `bool` with `must`/`filter`/`must_not`.
fn matches(query: &Value, doc: &Value) -> bool {
    if let Some(term) = query.get("term").and_then(Value::as_object) {
        return term
            .iter()
            .all(|(field, want)| doc.get(field) == Some(want));
    }
    if let Some(terms) = query.get("terms").and_then(Value::as_object) {
        return terms.iter().all(|(field, wanted)| {
            let Some(value) = doc.get(field) else {
                return false;
            };
            wanted
                .as_array()
                .is_some_and(|options| options.contains(value))
        });
    }
    if let Some(bool_query) = query.get("bool").and_then(Value::as_object) {
        let clauses = |name: &str| -> Vec<Value> {
            bool_query
                .get(name)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        return clauses("must").iter().all(|c| matches(c, doc))
            && clauses("filter").iter().all(|c| matches(c, doc))
            && !clauses("must_not").iter().any(|c| matches(c, doc));
    }
    panic!("mock endpoint got an unsupported query shape: {query}");
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

    // `_cat/indices` is plain text, not JSON — the real endpoint ignores
    // ?format=json, and `Es::cat_indices` parses columns positionally.
    if method == "GET" && path.starts_with("/_cat/indices") {
        let locked = state.lock().unwrap();
        let mut text = String::new();
        for index in locked.indices() {
            let docs = locked
                .docs
                .keys()
                .filter(|(name, _)| *name == index)
                .count();
            text.push_str(&format!(
                "green open {index} uuid-{index} 1 0 {docs} 0 1kb 1kb 1kb\n"
            ));
        }
        drop(locked);
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            text.len(),
            text
        )
        .unwrap();
        return;
    }

    let response = if method == "POST" && path == "/_bulk" {
        let mut locked = state.lock().unwrap();
        let lines: Vec<&[u8]> = body
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .collect();
        let mut i = 0;
        while i < lines.len() {
            let action: Value = serde_json::from_slice(lines[i]).unwrap();
            if let Some(target) = action.get("delete") {
                let index = target.get("_index").and_then(Value::as_str).unwrap();
                let id = target.get("_id").and_then(Value::as_str).unwrap();
                locked.docs.remove(&(index.to_string(), id.to_string()));
                i += 1;
                continue;
            }
            let target = action.get("index").expect("index or delete action");
            let index = target.get("_index").and_then(Value::as_str).unwrap();
            let id = target.get("_id").and_then(Value::as_str).unwrap();
            let doc: Value = serde_json::from_slice(lines[i + 1]).unwrap();
            locked.docs.insert((index.to_string(), id.to_string()), doc);
            i += 2;
        }
        json!({"errors": false, "items": []})
    } else if method == "POST" && path.contains("/_delete_by_query") {
        let index = path
            .trim_start_matches('/')
            .split('/')
            .next()
            .unwrap()
            .to_string();
        let request: Value = serde_json::from_slice(&body).unwrap();
        let query = request.get("query").cloned().unwrap_or(json!({}));
        let mut locked = state.lock().unwrap();
        let doomed: Vec<(String, String)> = locked
            .docs
            .iter()
            .filter(|((name, _), doc)| *name == index && matches(&query, doc))
            .map(|(key, _)| key.clone())
            .collect();
        for key in &doomed {
            locked.docs.remove(key);
        }
        json!({"deleted": doomed.len(), "failures": []})
    } else if method == "POST" && path.ends_with("/_search") {
        let index = path
            .trim_start_matches('/')
            .split('/')
            .next()
            .unwrap()
            .to_string();
        let locked = state.lock().unwrap();
        let total = locked
            .docs
            .keys()
            .filter(|(name, _)| *name == index)
            .count();
        json!({"hits": {"total": {"value": total}, "hits": []}, "aggregations": {}})
    } else {
        // ping, index creation, mapping upgrades, refresh
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
        prefix: "reindex-test".into(),
        state_dir: Some(state_dir.to_owned()),
        fresh: false,
        follow_symlinks: false,
        max_file_gb: 1,
        sample: 50,
        no_semantic: true,
        brain: None,
        no_graph: true,
        dry_run: false,
        json: false,
        quiet: true,
    }
}

/// Re-inferring the plan for a prefix that already holds records must leave one
/// copy of every file, even when a file's inferred dataset changed.
///
/// The migration is provoked the way the real one happens — by changing what
/// the corpus infers to. `d/a.jsonl` alone owns the base slug `d`; adding a
/// second file with a disjoint field set splits `d` into two single-file
/// clusters, and collision resolution renames both after their file stem. So
/// `a.jsonl` moves from `reindex-test-d` to `reindex-test-d-a`, which is the
/// same mechanism an extractor upgrade triggers when it gives a file a field it
/// did not have before (issue #170's `defs`).
#[test]
fn in_place_reindex_after_a_dataset_migration_does_not_duplicate() {
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::create_dir(corpus.path().join("d")).unwrap();
    fs::write(
        corpus.path().join("d/a.jsonl"),
        "{\"alpha\": 1, \"beta\": \"one\"}\n{\"alpha\": 2, \"beta\": \"two\"}\n",
    )
    .unwrap();

    let endpoint = MockEndpoint::start();
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    let after_first = endpoint.state.lock().unwrap().live_under(&config.prefix);
    assert_eq!(after_first, 2, "first run publishes both records");
    let first_copies = endpoint
        .state
        .lock()
        .unwrap()
        .copies_of(&config.prefix, "d/a.jsonl");
    assert_eq!(first_copies.len(), 2);
    let first_index = first_copies[0].0.clone();
    let first_run = first_copies[0].1.clone();

    // Second file, disjoint schema — splits the cluster and renames both slugs.
    fs::write(
        corpus.path().join("d/b.jsonl"),
        "{\"gamma\": 3, \"delta\": \"three\"}\n",
    )
    .unwrap();
    // A frozen resume plan skips new files AND never re-extracts old ones, so
    // --fresh is the only way to pick up a re-inference (or an upgraded
    // extractor) on an existing prefix. It is what `xc-index.sh … --fresh` runs.
    config.fresh = true;
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    let locked = endpoint.state.lock().unwrap();
    let copies = locked.copies_of(&config.prefix, "d/a.jsonl");
    assert_eq!(
        copies.len(),
        2,
        "a.jsonl must keep exactly its 2 records, one copy each; got {copies:?}"
    );
    assert!(
        copies.iter().all(|(index, _)| *index != first_index),
        "the migration under test did not happen — a.jsonl never left {first_index}, \
         so this test would pass even with the leak open"
    );
    // Not asserted on `ax_run`: run ids are `run-{UTC second}-{pid}`, so two
    // in-process runs inside one second share one — which is exactly why the
    // sweep discriminates on the plan's assignment and not on the stamp.
    let _ = first_run;
    assert_eq!(
        locked.live_under(&config.prefix),
        3,
        "3 records in the corpus, 3 live documents; indices: {:?}",
        locked.indices()
    );
}

/// The sweep is scoped to the running corpus's own file keys because the
/// default prefix is `ax` — every corpus indexed without `--prefix` shares it.
/// A second corpus re-indexed under the same prefix must not delete the first.
#[test]
fn reindex_leaves_another_corpus_under_the_same_prefix_alone() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let first_state = tempfile::tempdir().unwrap();
    let second_state = tempfile::tempdir().unwrap();
    fs::write(
        first.path().join("keep.jsonl"),
        "{\"alpha\": 1, \"beta\": \"one\"}\n",
    )
    .unwrap();
    fs::write(
        second.path().join("other.jsonl"),
        "{\"gamma\": 2, \"delta\": \"two\"}\n",
    )
    .unwrap();

    let endpoint = MockEndpoint::start();
    assert_eq!(
        run_index(cfg(first.path(), first_state.path(), &endpoint.url)).unwrap(),
        0
    );
    assert_eq!(
        run_index(cfg(second.path(), second_state.path(), &endpoint.url)).unwrap(),
        0
    );
    let mut third = cfg(second.path(), second_state.path(), &endpoint.url);
    third.fresh = true;
    assert_eq!(run_index(third.clone()).unwrap(), 0);

    let locked = endpoint.state.lock().unwrap();
    assert_eq!(
        locked.copies_of(&third.prefix, "keep.jsonl").len(),
        1,
        "the unrelated corpus sharing this prefix lost its record"
    );
    assert_eq!(locked.live_under(&third.prefix), 2);
}
