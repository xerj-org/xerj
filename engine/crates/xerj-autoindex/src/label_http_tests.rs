//! End-to-end tests for `--label` (#1062): a real `run_index_report` over a
//! small folder against a loopback node that answers `/_decide`, asserting the
//! labels land on the documents the run actually bulk-indexed.
//!
//! The endpoint here is deliberately NOT the incremental-reconcile harness:
//! that file's `HttpState` carries reconcile bookkeeping this test does not
//! exercise, and `/_decide` — the one endpoint this test exists to observe —
//! does not exist there. What is shared is the shape: a non-blocking accept
//! loop, one thread per connection, everything through one Mutex.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{json, Value};

use crate::cli::IndexCfg;
use crate::run_index_report;

/// A loopback node: enough of the indexing surface for `run_index_report` to
/// complete a generated run, plus a `/_decide` that answers canned votes keyed
/// by the POSITIVE LABEL the request carried (the Labeler sends one binary
/// vote per noul/option).
struct LabelNode {
    url: String,
    state: Arc<Mutex<NodeState>>,
    join: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct NodeState {
    stop: bool,
    /// When set, `/_decide` answers 500 — the refusal path under test.
    fail_decide: bool,
    /// Every document the run bulk-indexed, by (index, id).
    docs: Vec<(String, String, Value)>,
    /// Every (positive_label, question) POST /_decide received.
    decides: Vec<(String, String)>,
    /// Canned answers by positive label: (label, p, abstain).
    answers: BTreeMap<String, (String, f64, bool)>,
}

impl LabelNode {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(NodeState::default()));
        let server_state = Arc::clone(&state);
        let join = std::thread::spawn(move || loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    let server_state = Arc::clone(&server_state);
                    std::thread::spawn(move || serve(stream, &server_state));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if server_state.lock().unwrap().stop {
                        break;
                    }
                    std::thread::yield_now();
                }
                Err(e) => panic!("label node accept: {e}"),
            }
        });
        Self {
            url,
            state,
            join: Some(join),
        }
    }

    fn answer(&self, positive: &str, label: &str, p: f64, abstain: bool) {
        self.state
            .lock()
            .unwrap()
            .answers
            .insert(positive.into(), (label.into(), p, abstain));
    }

    fn docs(&self) -> Vec<(String, String, Value)> {
        self.state.lock().unwrap().docs.clone()
    }

    fn decides(&self) -> Vec<(String, String)> {
        self.state.lock().unwrap().decides.clone()
    }
}

impl Drop for LabelNode {
    fn drop(&mut self) {
        self.state.lock().unwrap().stop = true;
        let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
        self.join.take().unwrap().join().unwrap();
    }
}

fn serve(mut stream: TcpStream, state: &Mutex<NodeState>) {
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
        if let Some(v) = line
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .map(str::trim)
        {
            content_length = v.parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).unwrap();
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();

    let (status, response) = if method == "POST" && path == "/_decide" {
        if state.lock().unwrap().fail_decide {
            (
                500,
                json!({"error": {"type": "decide_unavailable",
                                 "reason": "injected for the refusal test"}}),
            )
        } else {
            decide_http(&body, state)
        }
    } else if method == "POST" && path == "/_bulk" {
        bulk_http(&body, state)
    } else if method == "POST" && path.ends_with("/_search") {
        (200, search_http(&path, &body, state))
    } else if method == "POST" && path.ends_with("/_delete_by_query") {
        (200, json!({"deleted": 0, "failures": []}))
    } else if method == "GET" && path.ends_with("/_count") {
        let index = path.trim_start_matches('/').trim_end_matches("/_count");
        let count = state
            .lock()
            .unwrap()
            .docs
            .iter()
            .filter(|(doc_index, _, _)| doc_index == index)
            .count();
        (200, json!({"count": count}))
    } else if method == "GET" && path == "/v1/embedding/identity" {
        (
            200,
            json!({"data": {
                "version": 1, "backend": "lexical",
                "identity_sha256": "l".repeat(64), "dimensions": 384,
                "semantic_contract": "semantic_text-derived-vector.v1",
                "resumable": true,
            }}),
        )
    } else {
        // Ping, index/mapping PUTs, anything else a generated run needs to be
        // told "yes" about.
        (200, json!({"acknowledged": true}))
    };

    let bytes = response.to_string();
    // write!, not writeln! — a trailing newline would land between the header
    // terminator and the body and corrupt the framing.
    let _ = write!(
        stream,
        "HTTP/1.1 {status} XERJTEST\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n",
        bytes.len()
    );
    let _ = stream.write_all(bytes.as_bytes());
    let _ = stream.flush();
}

/// A small-but-real `_search` over the captured documents. The generated
/// run's exact read-back verifies its sealed record counts with
/// `size: 0, query: bool.filter [terms ax_file, term ax_dataset]` and reads
/// `hits.total.value` — an empty-result stub would make every run abort as a
/// "corruption signal". Supports that filter shape and match_all; anything
/// else matches nothing (and the tests would notice).
fn search_http(path: &str, body: &[u8], state: &Mutex<NodeState>) -> Value {
    let indices: Vec<String> = path
        .trim_start_matches('/')
        .trim_end_matches("/_search")
        .split(',')
        .map(str::to_owned)
        .collect();
    let query: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let size = query.get("size").and_then(Value::as_u64).unwrap_or(10) as usize;
    let from = query.get("from").and_then(Value::as_u64).unwrap_or(0) as usize;
    let filters = query
        .pointer("/query/bool/filter")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let is_match_all = query.pointer("/query/match_all").is_some()
        || (filters.is_empty() && query.get("query").is_none());
    let docs = state.lock().unwrap().docs.clone();
    let matching: Vec<&(String, String, Value)> = docs
        .iter()
        .filter(|(index, _, _)| indices.contains(index))
        .filter(|(_, _, doc)| {
            if is_match_all {
                return true;
            }
            filters.iter().all(|filter| {
                if let Some(term) = filter.get("term").and_then(Value::as_object) {
                    let Some((field, value)) = term.iter().next() else {
                        return false;
                    };
                    return doc.get(field) == Some(value);
                }
                if let Some(terms) = filter.get("terms").and_then(Value::as_object) {
                    let Some((field, values)) = terms.iter().next() else {
                        return false;
                    };
                    return values.as_array().is_some_and(|values| {
                        values.iter().any(|value| doc.get(field) == Some(value))
                    });
                }
                if let Some(field) = filter
                    .get("exists")
                    .and_then(|exists| exists.get("field"))
                    .and_then(Value::as_str)
                {
                    return doc.get(field).is_some();
                }
                false
            })
        })
        .collect();
    let total = matching.len();
    let hits = matching
        .iter()
        .skip(from)
        .take(size)
        .map(|(_, id, doc)| json!({"_id": id, "_source": doc}))
        .collect::<Vec<_>>();
    // The batched finalize-verify (#1183's count lane) reads per-digest counts
    // as an exact `terms` aggregation; evaluate that one shape over the
    // matched set the way the engine's precise terms `doc_count` would.
    let aggregations = query
        .get("aggs")
        .or_else(|| query.get("aggregations"))
        .and_then(Value::as_object)
        .map(|aggs| {
            let mut body = serde_json::Map::new();
            for (name, spec) in aggs {
                let Some(field) = spec
                    .get("terms")
                    .and_then(|terms| terms.get("field"))
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                let mut counts: std::collections::BTreeMap<&str, u64> =
                    std::collections::BTreeMap::new();
                for (_, _, doc) in &matching {
                    if let Some(key) = doc.get(field).and_then(Value::as_str) {
                        *counts.entry(key).or_insert(0) += 1;
                    }
                }
                body.insert(
                    name.clone(),
                    json!({"buckets": counts
                        .into_iter()
                        .map(|(key, doc_count)| json!({"key": key, "doc_count": doc_count}))
                        .collect::<Vec<_>>()}),
                );
            }
            Value::Object(body)
        })
        .unwrap_or_else(|| json!({}));
    json!({
        "hits": {"total": {"value": total, "relation": "eq"}, "hits": hits},
        "aggregations": aggregations,
    })
}

/// The endpoint this file exists to observe: record the vote, answer canned.
fn decide_http(body: &[u8], state: &Mutex<NodeState>) -> (u16, Value) {
    let value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let positive = value
        .get("positive_label")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let question = value
        .get("question")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let (label, p, abstain) = {
        let mut locked = state.lock().unwrap();
        locked.decides.push((positive.clone(), question));
        locked
            .answers
            .get(&positive)
            .cloned()
            .unwrap_or_else(|| ("unknown".into(), 0.0, true))
    };
    (
        200,
        json!({
            "label": if abstain { Value::Null } else { Value::String(label) },
            "confidence": p,
            "abstain": abstain,
            "tier": "history",
            "neighbours": [],
            "took_ms": 0,
        }),
    )
}

/// Minimal `_bulk`: capture every action/doc pair, acknowledge each.
fn bulk_http(body: &[u8], state: &Mutex<NodeState>) -> (u16, Value) {
    let text = String::from_utf8_lossy(body);
    let mut items = Vec::new();
    let mut lines = text.lines();
    while let Some(action_line) = lines.next() {
        if action_line.trim().is_empty() {
            continue;
        }
        let doc_line = lines.next().unwrap_or("");
        let action: Value = serde_json::from_str(action_line).unwrap_or(Value::Null);
        let doc: Value = serde_json::from_str(doc_line).unwrap_or(Value::Null);
        let id = action
            .pointer("/index/_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let index = action
            .pointer("/index/_index")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        state.lock().unwrap().docs.push((index, id.clone(), doc));
        items.push(json!({"index": {"_id": id, "status": 201}}));
    }
    (200, json!({"errors": false, "items": items}))
}

fn cfg(root: &Path, state_dir: &Path, url: &str, label: &Path) -> IndexCfg {
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
        prefix: "label-http".into(),
        replace_prefix: false,
        state_dir: Some(state_dir.to_owned()),
        fresh: false,
        follow_symlinks: false,
        follow_symlinks_outside_root: false,
        ignore: crate::ignore_rules::IgnoreOptions::default(),
        max_file_bytes: 1 << 30,
        sample: 100,
        no_semantic: true,
        code_analyzer: Default::default(),
        brain: None,
        no_graph: true,
        max_minutes: 0,
        approve: None,
        dry_run: false,
        json: false,
        quiet: true,
        progress: crate::progress::ProgressMode::None,
        progress_interval: None,
        watch: false,
        debounce: std::time::Duration::from_millis(0),
        label: Some(label.to_owned()),
        exclude_rels: Vec::new(),
    }
}

/// THE end-to-end contract of #1062: `xerj autoindex <folder> --label <set>`
/// stamps `label`/`label_p` on every indexed document, decided per document
/// through the node's real `/_decide`, and the question templates see each
/// record's own fields. The decide endpoint answers by what the rendered
/// question CONTAINS — "prize" votes spam, anything else abstains — so the
/// two documents must come out differently labelled, which is only possible
/// if the record's payload reached the vote.
///
/// Also measures and prints the run's labelling throughput (documents per
/// second) against the loopback endpoint — the ingest-time cost statement,
/// made on a measurement instead of an estimate. The idle side of the budget
/// is structural and stated in `label.rs`: no `--label`, no Labeler, no idle
/// cost at all.
#[test]
fn label_end_to_end_through_a_real_run() {
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("mail.csv"),
        "id,text\n\
         1,claim your prize right now wire transfer\n\
         2,quarterly vendor fee schedule attached\n",
    )
    .unwrap();
    // The question set lives OUTSIDE the corpus: inside it, the walk would
    // index the operator's policy file as a document and vote on it too.
    let questions = tempfile::tempdir().unwrap();
    let question_set = questions.path().join("questions.json");
    fs::write(
        &question_set,
        r#"{
            "decide": { "index": "mail-history", "k": 10 },
            "questions": [
                { "id": "spam", "type": "noul", "positive": "spam",
                  "question": "Is this spam? {{text}}" }
            ]
        }"#,
    )
    .unwrap();

    let node = LabelNode::start();
    // One canned answer per positive label: both documents vote "spam" with
    // 0.9. That is enough for this test — the per-document rendering (that
    // each record's OWN text reached its vote) is asserted below from the
    // recorded questions, and the abstain path is pinned by the second test.
    node.answer("spam", "spam", 0.9, false);
    let config = cfg(corpus.path(), state_dir.path(), &node.url, &question_set);

    let started = Instant::now();
    let (code, summary) = run_index_report(config).expect("the labelled run completes");
    let elapsed = started.elapsed();

    assert_eq!(code, 0, "run succeeded: {summary:?}");
    // Only this corpus's dataset indices: the run also bulk-indexes into the
    // shared `autoindex-catalog`, which is not what the labels are about.
    let docs: Vec<_> = node
        .docs()
        .into_iter()
        .filter(|(index, _, _)| index.starts_with("label-http"))
        .collect();
    assert_eq!(docs.len(), 2, "both CSV rows indexed: {docs:?}");
    for (_, id, doc) in &docs {
        assert_eq!(
            doc["label"],
            json!("spam"),
            "doc {id} carries the decide answer: {doc}"
        );
        assert_eq!(doc["label_p"], json!(0.9), "raw p, uncalibrated: {doc}");
        assert_eq!(
            doc["label_spam"],
            json!("spam"),
            "per-question fields: {doc}"
        );
        assert_eq!(doc["label_spam_p"], json!(0.9), "{doc}");
    }
    // The votes the run made: one per record (single noul), each question
    // carrying the RECORD's own text — the {{text}} substitution.
    let decides = node.decides();
    assert_eq!(decides.len(), 2, "one decide per record: {decides:?}");
    let voted_prize = decides
        .iter()
        .any(|(positive, question)| positive == "spam" && question.contains("prize"));
    let voted_vendor = decides
        .iter()
        .any(|(positive, question)| positive == "spam" && question.contains("vendor"));
    assert!(
        voted_prize && voted_vendor,
        "each record's own text reached the vote: {decides:?}"
    );
    // Ingest-time cost, measured on this run and printed with the test: the
    // loopback round trip is the floor of the real cost, not the ceiling.
    let per_doc = elapsed.as_secs_f64() / docs.len() as f64;
    println!(
        "label e2e: {} docs in {:.3}s = {:.1} ms/doc end-to-end against a loopback node \
         (ingest-time only; idle cost is zero without --label)",
        docs.len(),
        elapsed.as_secs_f64(),
        per_doc * 1000.0
    );
}

/// A `/_decide` that abstains must still label the documents — with `null`
/// and the raw p carried — because an abstention is an answer, and the corpus
/// keeps the vote's strength for later calibration (#1063). And a `/_decide`
/// that FAILS must fail the run before any unlabelled document is bulk-indexed:
/// an index the operator believes is labelled is the #204 defect class.
#[test]
fn label_end_to_end_abstains_and_failures() {
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("mail.csv"), "id,text\n1,hello\n").unwrap();
    // Outside the corpus, so the policy file is not itself indexed.
    let questions = tempfile::tempdir().unwrap();
    let question_set = questions.path().join("questions.json");
    fs::write(
        &question_set,
        r#"{"questions": [
            { "id": "spam", "type": "noul", "positive": "spam", "question": "{{text}}" }
        ]}"#,
    )
    .unwrap();

    // Abstention: label null, p carried.
    let node = LabelNode::start();
    node.answer("spam", "spam", 0.3, true);
    let config = cfg(corpus.path(), state_dir.path(), &node.url, &question_set);
    let (code, _) = run_index_report(config).expect("an abstaining run still completes");
    assert_eq!(code, 0);
    let docs: Vec<_> = node
        .docs()
        .into_iter()
        .filter(|(index, _, _)| index.starts_with("label-http"))
        .collect();
    assert_eq!(docs.len(), 1, "{docs:?}");
    let doc = &docs[0].2;
    assert_eq!(doc["label"], Value::Null, "abstention is label null: {doc}");
    assert_eq!(doc["label_p"], json!(0.3), "raw p carried: {doc}");
    assert_eq!(doc["label_spam"], Value::Null, "{doc}");

    // Failure: /_decide answers 500 -> the run must ERROR, not index the
    // document unlabelled. A fresh state dir so the first run's committed
    // generation cannot make the second a no-op that never reaches the sink.
    let state_dir = tempfile::tempdir().unwrap();
    let node = LabelNode::start();
    node.state.lock().unwrap().fail_decide = true;
    let config = cfg(corpus.path(), state_dir.path(), &node.url, &question_set);
    let error = run_index_report(config).expect_err("a failing decide must fail the run");
    let text = format!("{error:#}");
    assert!(
        text.contains("/_decide"),
        "the failure must name the decide call it died on: {text}"
    );
    assert!(
        node.docs().is_empty(),
        "no document may be bulk-indexed unlabelled: {:?}",
        node.docs()
    );
}
