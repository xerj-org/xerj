//! `xerj feedback --submit` against a local mock, with no GitHub token.
//!
//! `--submit` sends automatically, with no `--yes`. It tries the GitHub PR
//! first and POSTs to the endpoint only when GitHub is unavailable. The POST
//! must not attach a GitHub token or an `Authorization` header — the admin key
//! belongs to the local node, not to this endpoint. Every test here keeps
//! GitHub out of reach (`--no-github`, or a PATH with no real `gh`).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Mutex;
use std::time::Duration;

use xerj_autoindex::feedback::{parse, run, Cfg};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// The env lock, even after another test panicked while holding it, so one
/// failure (a denied loopback bind) does not show as a `PoisonError` in the rest.
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

struct EnvVar {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVar {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVar {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

fn filled(endpoint: &str, no_github: bool) -> Cfg {
    let mut cfg = parse(vec![
        "--submit".into(),
        "--no-autofill".into(),
        "--agent".into(),
        "Test Agent".into(),
        "--used-for".into(),
        "a local mock, no GitHub".into(),
        "--verdict".into(),
        "offline submit works".into(),
    ])
    .expect("parse")
    .expect("cfg");
    cfg.no_github = no_github;
    cfg.endpoint = endpoint.to_string();
    cfg
}

/// A loopback listener for the mock endpoint. A sandbox that denies the bind
/// gets a message naming the requirement, not a bare `PermissionDenied` (#2886).
fn bind_loopback() -> TcpListener {
    xerj_common::loopback_test::expect_loopback(TcpListener::bind("127.0.0.1:0"))
}

fn read_one_http(listener: TcpListener) -> String {
    let (mut sock, _) = listener.accept().expect("accept");
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        match sock.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&buf[..header_end]);
                    let length = headers.lines().find_map(|line| {
                        let (name, rest) = line.split_once(':')?;
                        if !name.eq_ignore_ascii_case("content-length") {
                            return None;
                        }
                        rest.trim().parse::<usize>().ok()
                    });
                    let body_at = header_end + 4;
                    if let Some(length) = length {
                        while buf.len() < body_at + length {
                            match sock.read(&mut tmp) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                            }
                        }
                    }
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let response_body = r#"{"ok":true,"stored":true}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
        response_body.len()
    );
    let _ = sock.write_all(response.as_bytes());
    String::from_utf8_lossy(&buf).into_owned()
}

/// No `--yes`: `--submit` sends on its own (here straight to the endpoint).
#[test]
fn submit_sends_without_yes() {
    let _lock = env_lock();
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let endpoint = format!("http://127.0.0.1:{port}/api/field-report");
    let server = std::thread::spawn(move || read_one_http(listener));
    let cfg = filled(&endpoint, true);
    assert!(!cfg.yes);
    let code = run(&cfg);
    let request = server.join().expect("server thread");
    assert_eq!(code, 0, "request was:\n{request}");
    assert!(request.contains("\"agent\":\"Test Agent\""), "{request}");
}

/// GitHub first: with no `gh` on PATH the PR path is unavailable, and the
/// report falls back to the endpoint automatically.
#[test]
fn submit_falls_back_to_the_endpoint_when_github_is_unavailable() {
    let _lock = env_lock();
    let empty = tempfile::tempdir().unwrap();
    let _path = EnvVar::set("PATH", empty.path().to_str().unwrap());
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let endpoint = format!("http://127.0.0.1:{port}/api/field-report");
    let server = std::thread::spawn(move || read_one_http(listener));
    let code = run(&filled(&endpoint, false));
    let request = server.join().expect("server thread");
    assert_eq!(code, 0, "request was:\n{request}");
    assert!(
        request
            .to_ascii_lowercase()
            .starts_with("post /api/field-report"),
        "{request}"
    );
}

/// The order, observed: a fake `gh` that is not signed in records that it was
/// asked first, and only then does the POST happen.
#[cfg(unix)]
#[test]
fn submit_asks_github_first_then_posts() {
    use std::os::unix::fs::PermissionsExt;
    let _lock = env_lock();
    let bin = tempfile::tempdir().unwrap();
    let log = bin.path().join("gh.log");
    let gh = bin.path().join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\n[ \"$1\" = --version ] && exit 0\nexit 1\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/usr/bin:/bin", bin.path().display());
    let _path = EnvVar::set("PATH", &path);
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let endpoint = format!("http://127.0.0.1:{port}/api/field-report");
    let server = std::thread::spawn(move || read_one_http(listener));
    let code = run(&filled(&endpoint, false));
    let request = server.join().expect("server thread");
    assert_eq!(code, 0, "request was:\n{request}");
    let asked = std::fs::read_to_string(&log).expect("gh was never asked");
    assert!(asked.contains("auth status"), "{asked}");
    assert!(request.contains("\"agent\":\"Test Agent\""), "{request}");
}

/// `--github-only` (what the MCP tool uses) never POSTs.
#[test]
fn github_only_never_posts() {
    let _lock = env_lock();
    let empty = tempfile::tempdir().unwrap();
    let _path = EnvVar::set("PATH", empty.path().to_str().unwrap());
    let listener = bind_loopback();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut cfg = filled(&format!("http://127.0.0.1:{port}/api/field-report"), false);
    cfg.github_only = true;
    assert_eq!(run(&cfg), 1);
    match listener.accept() {
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
        other => panic!("--github-only must not POST: {other:?}"),
    }
}

/// `XERJ_DISABLE_FEEDBACK=true` opts out: nothing is sent.
#[test]
fn disable_feedback_sends_nothing() {
    let _lock = env_lock();
    let _off = EnvVar::set("XERJ_DISABLE_FEEDBACK", "true");
    let listener = bind_loopback();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let code = run(&filled(
        &format!("http://127.0.0.1:{port}/api/field-report"),
        true,
    ));
    assert_eq!(code, 0);
    match listener.accept() {
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
        other => panic!("an opted-out report must not be sent: {other:?}"),
    }
}

#[test]
fn submit_posts_json_and_sends_no_github_token() {
    let _lock = env_lock();
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let endpoint = format!("http://127.0.0.1:{port}/api/field-report");
    let token = "ghp_should_not_appear";
    let _gh = EnvVar::set("GH_TOKEN", token);
    let _github = EnvVar::set("GITHUB_TOKEN", token);
    let server = std::thread::spawn(move || read_one_http(listener));
    let code = run(&filled(&endpoint, true));
    let request = server.join().expect("server thread");
    assert_eq!(code, 0, "request was:\n{request}");
    let header = request
        .split("\r\n\r\n")
        .next()
        .unwrap()
        .to_ascii_lowercase();
    assert!(
        header.starts_with("post /api/field-report http/1.1"),
        "{header}"
    );
    assert!(
        header.contains("content-type: application/json"),
        "{header}"
    );
    assert!(header.contains("user-agent: xerj-feedback"), "{header}");
    assert!(
        !header.contains("authorization"),
        "the node key must not be sent:\n{header}"
    );
    assert!(!request.contains(token), "{request}");
    assert!(request.contains("\"agent\":\"Test Agent\""), "{request}");
    assert!(
        request.contains("\"verdict\":\"offline submit works\""),
        "{request}"
    );
    assert!(
        request.contains("\"used_for\":\"a local mock, no GitHub\""),
        "{request}"
    );
}

#[test]
fn submit_refuses_placeholders_without_connecting() {
    let _lock = env_lock();
    let listener = bind_loopback();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut cfg = parse(vec!["--submit".into(), "--no-autofill".into()])
        .unwrap()
        .unwrap();
    cfg.no_github = true;
    cfg.endpoint = format!("http://127.0.0.1:{port}/api/field-report");
    let code = run(&cfg);
    assert_eq!(code, 2);
    match listener.accept() {
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
        other => panic!("a placeholder report must not be posted: {other:?}"),
    }
}
