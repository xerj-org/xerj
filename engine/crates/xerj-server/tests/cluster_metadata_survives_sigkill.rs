//! Issue #203, against the real binary and a real `SIGKILL`.
//!
//! The in-process test (`xerj-api/tests/cluster_metadata_survives_restart.rs`)
//! proves the reload path. This one proves the *durability* half: the state
//! has to be on disk at the moment the PUT is acknowledged, not at some later
//! shutdown hook, because the restart that matters is the one nobody planned.
//! `kill -9` gives the process no chance to flush anything, so anything that
//! comes back was already committed by `write_file_atomic` (write → fsync →
//! rename → fsync parent).
//!
//! The write also has to be *atomic*, not merely eventual: this test hard-kills
//! the node while it is answering, so a rewrite that truncated the file in
//! place could leave a half-document that takes every template with it.
//!
//! Linux/macOS only — `SIGKILL` has no Windows equivalent, and
//! `Child::kill()` there is a `TerminateProcess` that Windows may let the
//! runtime observe. The property under test is platform-independent, but the
//! way to provoke it is not.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Three distinct free ports — `Config::validate` requires all three listener
/// ports to differ.
fn three_free_ports() -> (u16, u16, u16) {
    let l1 = TcpListener::bind("127.0.0.1:0").unwrap();
    let l2 = TcpListener::bind("127.0.0.1:0").unwrap();
    let l3 = TcpListener::bind("127.0.0.1:0").unwrap();
    (
        l1.local_addr().unwrap().port(),
        l2.local_addr().unwrap().port(),
        l3.local_addr().unwrap().port(),
    )
}

fn toml_path(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// One HTTP/1.1 request over a fresh connection, returning `(status, body)`.
///
/// Hand-rolled rather than pulled from a client crate so the test has no
/// runtime of its own: it must stay a plain `#[test]`, since the thing it
/// kills is a separate process and any async machinery here would only be
/// another thing that can hang.
fn request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;

    let payload = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
        payload.len()
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

    // Drain headers, then everything else — `Connection: close` means the
    // body ends at EOF, so no chunk/length parsing is needed.
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

/// Spawn the real binary on `data_dir` and wait until it answers.
fn boot(
    data_dir: &std::path::Path,
    cfg_dir: &std::path::Path,
    es_port: u16,
    rest: u16,
    grpc: u16,
) -> Child {
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
    loop {
        if let Ok((200, _)) = request(es_port, "GET", "/_cluster/health", None) {
            return child;
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    // Never leave a half-booted node behind for the rest of the suite to
    // trip over — reap it, then fail.
    let _ = child.kill();
    let _ = child.wait();
    panic!("xerj did not become ready on port {es_port}");
}

/// `kill -9` and reap. No graceful shutdown, no flush hook, no chance to
/// write anything the acknowledged PUTs did not already write.
fn sigkill(mut child: Child) {
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGKILL);
    }
    let status = child.wait().expect("reap child");
    assert!(
        !status.success(),
        "the node was supposed to be hard-killed, but it exited cleanly ({status:?}) — \
         a graceful exit could have flushed state that a real crash never would"
    );
}

#[test]
fn cluster_metadata_survives_kill_dash_nine() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let (rest, grpc, es) = three_free_ports();
    let child = boot(&data_dir, dir.path(), es, rest, grpc);

    let (st, b) = request(
        es,
        "PUT",
        "/_index_template/logs",
        Some(r#"{"index_patterns":["logs-*"],"priority":200,"template":{"mappings":{"properties":{"host":{"type":"keyword"}}}}}"#),
    )
    .unwrap();
    assert_eq!(st, 200, "PUT _index_template: {b}");

    let (st, b) = request(
        es,
        "PUT",
        "/_ingest/pipeline/tagger",
        Some(r#"{"description":"tag","processors":[{"set":{"field":"tagged","value":"yes"}}]}"#),
    )
    .unwrap();
    assert_eq!(st, 200, "PUT _ingest/pipeline: {b}");

    let (st, b) = request(
        es,
        "PUT",
        "/_ilm/policy/hot-warm",
        Some(r#"{"policy":{"phases":{"hot":{"actions":{}}}}}"#),
    )
    .unwrap();
    assert_eq!(st, 200, "PUT _ilm/policy: {b}");

    let (st, b) = request(es, "PUT", "/_data_stream/metrics-app", Some("{}")).unwrap();
    assert_eq!(st, 200, "PUT _data_stream: {b}");

    // Roll the stream forward so the restored generation counter has to be
    // 2, not the 1 a re-create would produce.
    let (st, b) = request(es, "POST", "/metrics-app/_rollover", Some("{}")).unwrap();
    assert_eq!(st, 200, "POST _rollover: {b}");
    assert!(
        b.contains(".ds-metrics-app-000002"),
        "rollover should mint generation 2: {b}"
    );

    sigkill(child);

    // Same data dir, brand-new process. Everything above was acknowledged,
    // so everything above must still be here.
    let child = boot(&data_dir, dir.path(), es, rest, grpc);

    let (st, b) = request(es, "GET", "/_index_template/logs", None).unwrap();
    assert_eq!(st, 200, "index template lost across kill -9: {b}");
    assert!(
        b.contains("logs-*") && b.contains("keyword"),
        "partial template restore: {b}"
    );

    let (st, b) = request(es, "GET", "/_ingest/pipeline/tagger", None).unwrap();
    assert_eq!(st, 200, "ingest pipeline lost across kill -9: {b}");

    let (st, b) = request(es, "GET", "/_ilm/policy/hot-warm", None).unwrap();
    assert_eq!(st, 200, "ILM policy lost across kill -9: {b}");

    let (st, b) = request(es, "GET", "/_data_stream/metrics-app", None).unwrap();
    assert_eq!(st, 200, "data stream lost across kill -9: {b}");
    assert!(
        b.contains("\"generation\":2") && b.contains(".ds-metrics-app-000002"),
        "the rollover must survive too, or the next rollover reissues a \
         backing index that already holds data: {b}"
    );

    // The restored pipeline must still *run*, not just be readable.
    let (st, b) = request(
        es,
        "POST",
        "/_ingest/pipeline/tagger/_simulate",
        Some(r#"{"docs":[{"_source":{"host":"a"}}]}"#),
    )
    .unwrap();
    assert_eq!(st, 200, "simulate after restart: {b}");
    assert!(
        b.contains("\"tagged\":\"yes\""),
        "a restored pipeline that no longer transforms is accepted-and-ignored: {b}"
    );

    sigkill(child);
}
