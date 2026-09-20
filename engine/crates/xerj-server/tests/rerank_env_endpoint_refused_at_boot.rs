//! `docs/RERANK.md` says a `rerank.endpoint` with `user:password@` in it, or
//! without an `http(s)://` scheme, stops the node from starting — and that the
//! rule holds whichever place the value came from. The first draft validated
//! the config file only: `TYPESAFE_ENDPOINT='http://user:pw@…'` booted fine,
//! `GET /_xerj/rerank` showed the stripped URL, and `reqwest` would have sent
//! the userinfo as Basic auth beside the bearer key. This drives the real
//! binary with the variable set and checks it refuses, and that a clean value
//! still boots.

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn toml_path(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn three_free_ports() -> (u16, u16, u16) {
    let held: Vec<TcpListener> = (0..3)
        .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    let p: Vec<u16> = held
        .iter()
        .map(|l| l.local_addr().unwrap().port())
        .collect();
    (p[0], p[1], p[2])
}

fn config(dir: &std::path::Path, rest: u16, grpc: u16, es: u16) -> String {
    format!(
        r#"
[server]
bind_address = "127.0.0.1"
rest_port = {rest}
grpc_port = {grpc}
es_compat_port = {es}
data_dir = "{data}"

[limits]
disk_flood_stage_percent = 0
"#,
        data = toml_path(&dir.join("data")),
    )
}

fn drain_into(mut r: impl Read, buf: Arc<Mutex<String>>) {
    let mut chunk = [0u8; 4096];
    loop {
        match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf
                .lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&chunk[..n])),
            Err(_) => break,
        }
    }
}

struct Boot {
    exited: Option<i32>,
    es_port_open: bool,
    stderr: String,
}

/// Spawn the binary with `TYPESAFE_ENDPOINT` set and no `rerank.endpoint` in
/// the file, and wait until it exits or its ES-compat port accepts a
/// connection — whichever comes first — up to `timeout`.
fn boot_with_env_endpoint(dir: &std::path::Path, endpoint: &str, timeout: Duration) -> Boot {
    let (rest, grpc, es) = three_free_ports();
    let config_path = dir.join("xerj.toml");
    std::fs::write(&config_path, config(dir, rest, grpc, es)).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .arg("--config")
        .arg(&config_path)
        .arg("--insecure")
        .env("TYPESAFE_ENDPOINT", endpoint)
        .env_remove("TYPESAFE_API_KEY")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn xerj");
    let stderr_buf = Arc::new(Mutex::new(String::new()));
    let err_r = child.stderr.take().expect("stderr piped");
    let err_buf_t = stderr_buf.clone();
    let err_thread = std::thread::spawn(move || drain_into(err_r, err_buf_t));

    let deadline = Instant::now() + timeout;
    let mut exited = None;
    let mut es_port_open = false;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            exited = status.code();
            break;
        }
        if TcpStream::connect(("127.0.0.1", es)).is_ok() {
            es_port_open = true;
            break;
        }
        if Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    child.kill().ok();
    child.wait().ok();
    err_thread.join().ok();
    let stderr = stderr_buf.lock().unwrap().clone();
    Boot {
        exited,
        es_port_open,
        stderr,
    }
}

#[test]
fn a_credentialed_typesafe_endpoint_in_the_environment_stops_the_node() {
    let dir = tempfile::tempdir().unwrap();
    let boot = boot_with_env_endpoint(
        dir.path(),
        "http://user:pw@127.0.0.1:9/v1/systemone",
        Duration::from_secs(60),
    );
    assert!(
        !boot.es_port_open,
        "the node came up with a credentialed TYPESAFE_ENDPOINT; stderr:\n{}",
        boot.stderr
    );
    assert_ne!(
        boot.exited,
        Some(0),
        "exit status must be a failure; stderr:\n{}",
        boot.stderr
    );
    assert!(
        boot.exited.is_some(),
        "the node neither exited nor listened within the timeout; stderr:\n{}",
        boot.stderr
    );
    assert!(
        boot.stderr.contains("TYPESAFE_ENDPOINT")
            && boot.stderr.contains("must not carry credentials"),
        "the refusal names the variable and the rule; stderr:\n{}",
        boot.stderr
    );
    assert!(
        !boot.stderr.contains("user:pw@"),
        "the credential must not be echoed; stderr:\n{}",
        boot.stderr
    );
}

#[test]
fn a_non_http_typesafe_endpoint_in_the_environment_stops_the_node() {
    let dir = tempfile::tempdir().unwrap();
    let boot = boot_with_env_endpoint(
        dir.path(),
        "judge.example/v1/systemone",
        Duration::from_secs(60),
    );
    assert!(!boot.es_port_open, "stderr:\n{}", boot.stderr);
    assert!(
        boot.exited.is_some() && boot.exited != Some(0),
        "stderr:\n{}",
        boot.stderr
    );
    assert!(
        boot.stderr.contains("TYPESAFE_ENDPOINT")
            && boot.stderr.contains("absolute http:// or https:// URL"),
        "stderr:\n{}",
        boot.stderr
    );
}

/// The check must not refuse a well-formed value: the same boot with a clean
/// endpoint gets past config loading and starts listening.
#[test]
fn a_clean_typesafe_endpoint_in_the_environment_boots() {
    let dir = tempfile::tempdir().unwrap();
    let boot = boot_with_env_endpoint(
        dir.path(),
        "http://127.0.0.1:9/v1/systemone",
        Duration::from_secs(120),
    );
    assert!(
        boot.es_port_open,
        "the node did not start listening (exit {:?}); stderr:\n{}",
        boot.exited, boot.stderr
    );
}
