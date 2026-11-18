//! A local node that outlives its terminal, comes back when a client needs
//! it, and survives a headless first launch (#3267, #3276).
//!
//! Process-level on purpose: the defects were in how the binary detaches,
//! how a client finds and restarts a stopped node, and what the first launch
//! writes to an unwritable stderr. Every node here runs on private ports
//! with a throwaway HOME and data dir; nothing touches :9200.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A base port whose three ports (ES, REST, gRPC) are free right now.
fn free_port_triple() -> u16 {
    for _ in 0..200 {
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = probe.local_addr().unwrap().port();
        drop(probe);
        if base > 65_000 {
            continue;
        }
        let held: Vec<_> = (0..3)
            .filter_map(|off| TcpListener::bind(("127.0.0.1", base + off)).ok())
            .collect();
        if held.len() == 3 {
            return base;
        }
    }
    panic!("no free port triple");
}

fn write_config(dir: &Path) -> PathBuf {
    let path = dir.join("xerj.toml");
    std::fs::write(&path, "[limits]\ndisk_flood_stage_percent = 0\n").unwrap();
    path
}

/// One HTTP/1.1 request; returns (status, body).
fn http(port: u16, method: &str, path: &str, body: Option<&str>) -> Option<(u16, String)> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    let body = body.unwrap_or("");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).ok()?;
    let mut out = String::new();
    s.read_to_string(&mut out).ok()?;
    let status = out.split_whitespace().nth(1)?.parse().ok()?;
    let body = out.split_once("\r\n\r\n").map(|(_, b)| b.to_string())?;
    Some((status, body))
}

fn wait_status(port: u16, want: &[u16], timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some((status, _)) = http(port, "GET", "/_cluster/health", None) {
            if want.contains(&status) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

fn wait_closed(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn kill(pid: u32, signal: i32) {
    unsafe {
        libc::kill(pid as i32, signal);
    }
}

fn pid_in(text: &str) -> u32 {
    let at = text.find("(pid ").expect("a pid in the output") + 5;
    text[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap()
}

/// Kills the node when the test ends, pass or fail.
struct NodeGuard(Option<u32>);
impl Drop for NodeGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            kill(pid, libc::SIGKILL);
        }
    }
}

fn session_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rsplit_once(')')?.1;
    after.split_whitespace().nth(3)?.parse().ok()
}

/// #3267 acceptance on Linux/macOS: `xerj --detach` returns once the node
/// serves, the node leads its own session (so the end of the terminal
/// session does not reach it), and after it is killed an MCP `xerj_search`
/// on the loopback URL starts it again, returns the hit, and says so.
#[test]
fn detach_then_mcp_search_restarts_a_killed_node() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let data = dir.path().join("data");
    let config = write_config(dir.path());
    let port = free_port_triple();

    let out = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .args(["--detach", "--insecure", "--disable-feedback", "--config"])
        .arg(&config)
        .arg("--data-dir")
        .arg(&data)
        .args(["--port", &port.to_string()])
        .env("HOME", &home)
        .env("XERJ_DISABLE_FEEDBACK", "true")
        .current_dir(dir.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("node started in the background"),
        "{stdout}"
    );
    let pid = pid_in(&stdout);
    let mut guard = NodeGuard(Some(pid));
    assert!(
        stdout.contains(&format!("127.0.0.1:{port}"))
            || stdout.contains(&format!("localhost:{port}")),
        "{stdout}"
    );
    assert!(stdout.contains("node.log"), "{stdout}");
    // Own session: the detached node is its session's leader.
    if let Some(sid) = session_of(pid) {
        assert_eq!(sid, pid, "the detached node must lead its own session");
    }
    let record = std::fs::read_to_string(data.join("node.launch.json")).unwrap();
    assert!(record.contains(&format!("\"port\": {port}")), "{record}");
    assert!(!record.contains("--detach"), "{record}");

    let (status, body) = http(
        port,
        "PUT",
        "/docs/_doc/1?refresh=true",
        Some(r#"{"title":"rotate the signing keys"}"#),
    )
    .unwrap();
    assert!(status < 300, "{status} {body}");

    kill(pid, libc::SIGKILL);
    assert!(
        wait_closed(port, Duration::from_secs(20)),
        "node did not stop"
    );
    guard.0 = None;

    let url = format!("http://127.0.0.1:{port}");
    let mut mcp = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .arg("mcp")
        .env("HOME", &home)
        .env("XERJ_URL", &url)
        .env_remove("XERJ_NO_AUTOSTART")
        .env("XERJ_DISABLE_FEEDBACK", "true")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    {
        let stdin = mcp.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","capabilities":{{}},"clientInfo":{{"name":"t","version":"0"}}}}}}"#
        )
        .unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"xerj_search","arguments":{{"index":"docs","query":"rotate"}}}}}}"#
        )
        .unwrap();
    }
    drop(mcp.stdin.take());
    let mut reply = String::new();
    for line in BufReader::new(mcp.stdout.take().unwrap()).lines() {
        let line = line.unwrap();
        if line.contains("\"id\":2") {
            reply = line;
            break;
        }
    }
    let _ = mcp.wait();
    let restarted = std::fs::read_to_string(data.join("node.launch.json"))
        .ok()
        .and_then(|r| {
            let v: serde_json::Value = serde_json::from_str(&r).ok()?;
            v["pid"].as_u64().map(|p| p as u32)
        });
    let _second = NodeGuard(restarted.filter(|p| *p != pid));

    let v: serde_json::Value = serde_json::from_str(&reply).expect("a JSON-RPC reply");
    let result = &v["result"];
    assert_eq!(result["isError"], false, "{reply}");
    let hits = result["content"][0]["text"].as_str().unwrap();
    assert!(hits.contains("rotate the signing keys"), "{hits}");
    let note = result["content"][1]["text"].as_str().unwrap_or_default();
    assert!(note.contains("started the local XERJ node"), "{reply}");
    assert!(note.contains("node.log"), "{note}");
}

/// #3276 acceptance: a first launch with auth on (the owner-claim banner is
/// printed) and an unwritable stderr serves instead of aborting with 134,
/// and the setup link is in `<data-dir>/owner-claim.txt`. `/dev/full` fails
/// every write (ENOSPC), the same class as a terminal that hung up (EIO);
/// `2>&-` closes the descriptor outright.
#[test]
fn a_headless_first_launch_with_an_unwritable_stderr_serves() {
    for (case, redirect) in [("dev-full", "2>/dev/full"), ("closed", "2>&-")] {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let data = dir.path().join("data");
        let config = write_config(dir.path());
        let port = free_port_triple();
        let script = format!(
            "exec \"$0\" --disable-feedback --config '{}' --data-dir '{}' --port {port} \
             </dev/null >/dev/null {redirect}",
            config.display(),
            data.display()
        );
        let mut child = Command::new("/bin/sh")
            .args(["-c", &script, env!("CARGO_BIN_EXE_xerj")])
            .env("HOME", &home)
            .env("XERJ_DISABLE_FEEDBACK", "true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut guard = NodeGuard(Some(child.id()));
        let served = wait_status(port, &[200, 401], Duration::from_secs(90));
        let exited = child.try_wait().unwrap();
        assert!(
            served && exited.is_none(),
            "{case}: node must serve, not exit (exit: {exited:?})"
        );
        let claim = std::fs::read_to_string(data.join("owner-claim.txt"))
            .unwrap_or_else(|e| panic!("{case}: owner-claim.txt: {e}"));
        assert!(
            claim.contains("/_xerj-console/setup#token="),
            "{case}: {claim}"
        );
        kill(child.id(), libc::SIGTERM);
        let _ = child.wait();
        guard.0 = None;
    }
}
