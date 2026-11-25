//! #3418: one SIGINT stops a node within `--shutdown-timeout`, a second
//! exits at once with the WAL fsynced, and a SIGINT during startup is not
//! lost when the node inherited SIGINT as ignored (`cmd &` in a script).
//! Private ports and throwaway dirs; nothing touches :9200.

#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// A free (P, P+1, P+2) triple: es/rest/grpc for the booted node. See
/// `xerj_common::loopback_test::free_port_triple` for why bases come from
/// below the ephemeral range (`XERJ_TEST_PORT_BASE` confines them).
fn free_port_triple() -> u16 {
    xerj_common::loopback_test::free_port_triple().release()
}

fn http(port: u16, method: &str, path: &str, body: &str) -> Option<(u16, String)> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(60))).ok()?;
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/x-ndjson\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).ok()?;
    let mut out = String::new();
    s.read_to_string(&mut out).ok()?;
    let status = out.split_whitespace().nth(1)?.parse().ok()?;
    Some((status, out.split_once("\r\n\r\n")?.1.to_string()))
}

struct Node {
    child: Child,
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn(dir: &Path, port: u16, timeout_secs: u64, ignore_sigint: bool) -> Node {
    let config = dir.join("xerj.toml");
    std::fs::write(&config, "[limits]\ndisk_flood_stage_percent = 0\n").unwrap();
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_xerj"));
    cmd.args(["--insecure", "--disable-feedback", "--config"])
        .arg(&config)
        .arg("--data-dir")
        .arg(dir.join("data"))
        .args(["--port", &port.to_string()])
        .env("HOME", &home)
        .env("XERJ_DISABLE_FEEDBACK", "true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if ignore_sigint {
        cmd.env("XERJ_SHUTDOWN_TIMEOUT_SECS", timeout_secs.to_string());
    } else {
        cmd.args(["--shutdown-timeout", &timeout_secs.to_string()])
            .env_remove("XERJ_SHUTDOWN_TIMEOUT_SECS");
    }
    if ignore_sigint {
        unsafe {
            cmd.pre_exec(|| {
                libc::signal(libc::SIGINT, libc::SIG_IGN);
                Ok(())
            });
        }
    }
    Node {
        child: cmd.spawn().unwrap(),
    }
}

fn wait_ready(node: &mut Node, port: u16) {
    let deadline = Instant::now() + Duration::from_secs(240);
    while Instant::now() < deadline {
        assert!(
            node.child.try_wait().unwrap().is_none(),
            "node exited during boot"
        );
        if let Some((200, _)) = http(port, "GET", "/_cluster/health", "") {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("node never became ready");
}

fn wait_exit(node: &mut Node, limit: Duration) -> Option<(ExitStatus, Duration)> {
    let t0 = Instant::now();
    while t0.elapsed() < limit {
        if let Some(status) = node.child.try_wait().unwrap() {
            return Some((status, t0.elapsed()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

fn sigint(node: &Node) {
    unsafe {
        libc::kill(node.child.id() as i32, libc::SIGINT);
    }
}

fn hold_a_request(port: u16) -> TcpStream {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(
        b"POST /held/_bulk HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/x-ndjson\r\n\
          Content-Length: 1000000\r\n\r\n{\"index\":{}}\n",
    )
    .unwrap();
    s
}

fn bulk(port: u16, index: &str, n: usize) {
    let mut body = String::new();
    for i in 0..n {
        body.push_str(&format!(
            "{{\"index\":{{\"_id\":\"{i}\"}}}}\n{{\"n\":{i},\"t\":\"doc {i}\"}}\n"
        ));
    }
    let (status, resp) = http(port, "POST", &format!("/{index}/_bulk"), &body).unwrap();
    assert_eq!(status, 200, "{resp}");
    assert!(resp.contains("\"errors\":false"), "{resp}");
}

#[test]
fn sigint_during_startup_with_sigint_ignored_still_stops_the_node() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port_triple();
    let mut node = spawn(dir.path(), port, 5, true);
    std::thread::sleep(Duration::from_millis(150));
    sigint(&node);
    let (status, took) = wait_exit(&mut node, Duration::from_secs(60))
        .expect("one SIGINT during startup was dropped: the node is still running 60 s later");
    assert_ne!(
        status.code(),
        Some(2),
        "usage error, not a stop: {status:?}"
    );
    assert!(
        took <= Duration::from_secs(5 + 5),
        "took {took:?} with a 5 s budget"
    );
}

#[test]
fn one_sigint_stops_within_the_budget_and_a_second_exits_at_once_with_writes_kept() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port_triple();

    let mut node = spawn(dir.path(), port, 6, false);
    wait_ready(&mut node, port);
    let _held = hold_a_request(port);
    std::thread::sleep(Duration::from_millis(300));
    sigint(&node);
    let (_, took) = wait_exit(&mut node, Duration::from_secs(60))
        .expect("one SIGINT with a request held open did not stop the node");
    assert!(
        took <= Duration::from_secs(6 + 3),
        "one SIGINT took {took:?} with --shutdown-timeout 6"
    );
    drop(node);

    let mut node = spawn(dir.path(), port, 120, false);
    wait_ready(&mut node, port);
    bulk(port, "kept", 500);
    let _held = hold_a_request(port);
    std::thread::sleep(Duration::from_millis(300));
    sigint(&node);
    std::thread::sleep(Duration::from_secs(1));
    assert!(
        node.child.try_wait().unwrap().is_none(),
        "the first SIGINT must drain, not exit"
    );
    sigint(&node);
    let (status, took) = wait_exit(&mut node, Duration::from_secs(30))
        .expect("a second SIGINT did not exit the node");
    assert!(
        took <= Duration::from_secs(5),
        "second SIGINT took {took:?}"
    );
    assert_eq!(
        status.code(),
        Some(130),
        "{status:?} signal {:?}",
        status.signal()
    );
    drop(node);

    let mut node = spawn(dir.path(), port, 30, false);
    wait_ready(&mut node, port);
    http(port, "POST", "/kept/_refresh", "").unwrap();
    let (status, body) = http(port, "GET", "/kept/_count", "").unwrap();
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("\"count\":500"),
        "acknowledged writes lost by the forced exit: {body}"
    );
    sigint(&node);
    assert!(wait_exit(&mut node, Duration::from_secs(60)).is_some());
}
