//! Issue #3017, against the real binary: tied kNN and hybrid (RRF) rankings
//! must be identical in every server process that serves the same data.
//!
//! The kNN executors ranked equal scores in the order they collected them.
//! On the HNSW path that is the graph's traversal order, and the graph's
//! layers come from `thread_rng`, so the graph the indexing process built and
//! the one a restarted process (or a second node indexing the same data)
//! serves visit tied neighbours differently. RRF ranks each leg by position,
//! so a reordered tie in the kNN leg changed fused scores, not just the
//! order of equal ones. The fix ranks every kNN pool by the one score-ranked
//! page order, `score DESC, seq_no ASC, _id ASC`.
//!
//! Three processes: A indexes and answers, B restarts on A's data dir, C
//! indexes the same documents into a fresh data dir. Every corpus vector is
//! shared by two documents (a duplicated record), so every kNN score is a
//! tie. Before the fix the processes disagreed on most of the 12 kNN and 12
//! hybrid rankings.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn three_free_ports() -> (u16, u16, u16) {
    let l1 = xerj_common::loopback_test::expect_loopback(TcpListener::bind("127.0.0.1:0"));
    let l2 = xerj_common::loopback_test::expect_loopback(TcpListener::bind("127.0.0.1:0"));
    let l3 = xerj_common::loopback_test::expect_loopback(TcpListener::bind("127.0.0.1:0"));
    (
        l1.local_addr().unwrap().port(),
        l2.local_addr().unwrap().port(),
        l3.local_addr().unwrap().port(),
    )
}

fn toml_path(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn request(
    port: u16,
    method: &str,
    path: &str,
    content_type: &str,
    body: &str,
) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    stream.set_write_timeout(Some(Duration::from_secs(120)))?;
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\
         Content-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes())?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line)?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" {
            break;
        }
    }
    let mut body = String::new();
    reader.read_to_string(&mut body)?;
    Ok((status, body))
}

fn json_call(port: u16, method: &str, path: &str, body: &Value) -> Value {
    let (status, text) = request(port, method, path, "application/json", &body.to_string())
        .unwrap_or_else(|e| panic!("{method} {path}: {e}"));
    assert_eq!(status, 200, "{method} {path}: {text}");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{method} {path}: {e}: {text}"))
}

fn boot(data_dir: &std::path::Path, cfg_dir: &std::path::Path) -> (Child, u16) {
    let (rest, grpc, es_port) = three_free_ports();
    let config = format!(
        r#"
[server]
bind_address = "127.0.0.1"
rest_port = {rest}
grpc_port = {grpc}
es_compat_port = {es_port}
data_dir = "{data}"
"#,
        data = toml_path(data_dir),
    );
    let config_path = cfg_dir.join(format!("xerj-{es_port}.toml"));
    std::fs::write(&config_path, config).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .arg("--config")
        .arg(&config_path)
        .arg("--insecure")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn xerj");
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if let Ok((200, _)) = request(es_port, "GET", "/_cluster/health", "application/json", "") {
            return (child, es_port);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("xerj did not become ready on port {es_port}");
}

fn stop(mut child: Child) {
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Small deterministic generator, so the corpus needs no `rand` dependency.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn unit(&mut self) -> f64 {
        (self.next() % 2_000_001) as f64 / 1_000_000.0 - 1.0
    }
}

const DIMS: usize = 16;
const DOCS: usize = 1_600;
const PROTOS: usize = 800;
const QUERIES: usize = 12;
const K: usize = 100;

fn vector(rng: &mut Lcg) -> Vec<f64> {
    (0..DIMS)
        .map(|_| (rng.unit() * 1e4).round() / 1e4)
        .collect()
}

fn index_corpus(port: u16) {
    let mapping = json!({"mappings": {"properties": {
        "t": {"type": "text"},
        "v": {"type": "dense_vector", "dims": DIMS, "index": true, "similarity": "cosine"}
    }}});
    json_call(port, "PUT", "/ties", &mapping);
    let mut rng = Lcg(3017);
    let protos: Vec<Vec<f64>> = (0..PROTOS).map(|_| vector(&mut rng)).collect();
    let words = ["alpha", "beta", "gamma", "delta", "omega", "sigma"];
    for chunk in 0..(DOCS / 400) {
        let mut ndjson = String::new();
        for i in chunk * 400..(chunk + 1) * 400 {
            let text = format!(
                "{} {}",
                words[i % words.len()],
                words[(i / 7) % words.len()]
            );
            ndjson.push_str(&json!({"index": {"_id": format!("d{i}")}}).to_string());
            ndjson.push('\n');
            ndjson.push_str(&json!({"t": text, "v": protos[i % PROTOS]}).to_string());
            ndjson.push('\n');
        }
        let (status, body) =
            request(port, "POST", "/ties/_bulk", "application/x-ndjson", &ndjson).expect("bulk");
        assert_eq!(status, 200, "bulk: {body}");
        assert!(body.contains("\"errors\":false"), "bulk errors: {body}");
    }
    json_call(port, "POST", "/ties/_refresh", &json!({}));
}

/// The kNN and hybrid id lists for a fixed set of query vectors.
fn rankings(port: u16) -> Vec<(String, Vec<String>)> {
    let mut rng = Lcg(17);
    let mut out = Vec::new();
    for q in 0..QUERIES {
        let qv = vector(&mut rng);
        let knn = json!({"field": "v", "query_vector": qv, "k": K, "num_candidates": DOCS});
        let bodies = [
            ("knn", json!({"size": K, "_source": false, "knn": knn})),
            (
                "hybrid",
                json!({"size": K, "_source": false, "query": {"hybrid": {
                    "queries": [
                        {"query": {"match": {"t": "alpha gamma"}}, "weight": 1.0},
                        {"query": {"knn": knn}, "weight": 1.0}
                    ],
                    "fusion": {"type": "rrf", "k": 60}
                }}}),
            ),
        ];
        for (arm, body) in bodies {
            let res = json_call(port, "POST", "/ties/_search", &body);
            let ids: Vec<String> = res["hits"]["hits"]
                .as_array()
                .expect("hits")
                .iter()
                .map(|h| h["_id"].as_str().unwrap().to_string())
                .collect();
            assert!(!ids.is_empty(), "{arm} q{q}: no hits: {res}");
            out.push((format!("{arm} q{q}"), ids));
        }
    }
    out
}

fn assert_same(label: &str, a: &[(String, Vec<String>)], b: &[(String, Vec<String>)]) {
    let differing: Vec<&str> = a
        .iter()
        .zip(b)
        .filter(|(x, y)| x.1 != y.1)
        .map(|(x, _)| x.0.as_str())
        .collect();
    assert!(
        differing.is_empty(),
        "{label}: {} of {} rankings differ: {differing:?}",
        differing.len(),
        a.len()
    );
}

#[test]
fn tied_knn_and_hybrid_rankings_match_across_server_processes() {
    let dir = tempfile::tempdir().unwrap();
    let data_a = dir.path().join("a");
    let data_c = dir.path().join("c");
    std::fs::create_dir_all(&data_a).unwrap();
    std::fs::create_dir_all(&data_c).unwrap();

    let (a, port) = boot(&data_a, dir.path());
    index_corpus(port);
    let in_indexing_process = rankings(port);
    stop(a);

    let (b, port) = boot(&data_a, dir.path());
    let after_restart = rankings(port);
    stop(b);

    let (c, port) = boot(&data_c, dir.path());
    index_corpus(port);
    let second_node = rankings(port);
    stop(c);

    assert_same(
        "indexing process vs restart",
        &in_indexing_process,
        &after_restart,
    );
    assert_same(
        "indexing process vs a second node",
        &in_indexing_process,
        &second_node,
    );

    // Within a tie the page is in arrival order: equal scores never put a
    // later-indexed document first (ids are d<arrival index>).
    let knn_q0 = &in_indexing_process[0].1;
    let arrival: Vec<usize> = knn_q0
        .iter()
        .map(|id| id.trim_start_matches('d').parse().unwrap())
        .collect();
    let mut ties_checked = 0;
    for w in arrival.windows(2) {
        if w[0] % PROTOS == w[1] % PROTOS {
            assert!(
                w[0] < w[1],
                "tied documents out of arrival order: {knn_q0:?}"
            );
            ties_checked += 1;
        }
    }
    assert!(ties_checked > 0, "the corpus must produce ties: {knn_q0:?}");
}
