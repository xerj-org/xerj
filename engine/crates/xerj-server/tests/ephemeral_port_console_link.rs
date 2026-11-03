//! Issue #469: the first-launch console setup link must name a port the node
//! actually holds, not the one it was asked for.
//!
//! The link used to be built from `cfg.server.es_compat_port` — the
//! *requested* port — and printed at the old step 9, before any listener was
//! bound (binding happened at the old step 10a). With an ephemeral port
//! (`es_compat_port = 0`) that produced a link like
//! `http://127.0.0.1:0/_xerj-console/setup#token=…`: `:0` is never a
//! reachable address, so the single action a first-run operator is told to
//! take could not be followed. #466 fixed the same class of problem for the
//! startup banner by binding every listener first and printing from
//! `local_addr()`; this is that same treatment applied to the console link.
//!
//! This is a process-level test, like `taken_port_fails_startup.rs`: the
//! defect is in how `main.rs` sequences the bind against the console
//! bootstrap, which is invisible from inside `xerj-console-api` (its own unit
//! tests pass a `bind_url` the caller already built correctly).

use std::io::Read;
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn toml_path(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// `rest_port`/`grpc_port` pinned to two free ports; `es_compat_port = 0` so
/// the kernel assigns the ES-compat listener whatever it has free — the
/// ephemeral-port case the issue reports against.
fn config_with_ephemeral_es_port(dir: &std::path::Path, rest: u16, grpc: u16) -> String {
    format!(
        r#"
[server]
bind_address = "127.0.0.1"
rest_port = {rest}
grpc_port = {grpc}
es_compat_port = 0
data_dir = "{data}"

[limits]
disk_flood_stage_percent = 0
"#,
        data = toml_path(&dir.join("data")),
    )
}

fn two_free_ports() -> (u16, u16) {
    let held: Vec<TcpListener> = (0..2)
        .map(|_| xerj_common::loopback_test::expect_loopback(TcpListener::bind("127.0.0.1:0")))
        .collect();
    let p: Vec<u16> = held
        .iter()
        .map(|l| l.local_addr().unwrap().port())
        .collect();
    (p[0], p[1])
}

struct Outcome {
    stdout: String,
    stderr: String,
}

/// Read `r` a chunk at a time, appending everything into `buf`, until EOF —
/// meant to run on its own thread against a still-live child pipe, since a
/// blocking `read` only returns once the child writes or closes it.
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

/// Spawn the real binary and poll its stdout/stderr for `until` to become
/// true, up to `timeout`, then kill it — this is a first-launch boot with no
/// active user yet, so it never exits on its own. Two reader threads drain
/// the pipes concurrently so a quiet stdout (still building system indices)
/// never blocks us from seeing what stderr already has, or vice versa.
fn boot_until(
    config_body: &str,
    dir: &std::path::Path,
    timeout: Duration,
    until: impl Fn(&str, &str) -> bool,
) -> Outcome {
    let config_path = dir.join("xerj.toml");
    std::fs::write(&config_path, config_body).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .arg("--config")
        .arg(&config_path)
        .arg("--insecure")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn xerj");

    let stdout_buf = Arc::new(Mutex::new(String::new()));
    let stderr_buf = Arc::new(Mutex::new(String::new()));
    let out_r = child.stdout.take().expect("stdout piped");
    let err_r = child.stderr.take().expect("stderr piped");
    let out_buf_t = stdout_buf.clone();
    let err_buf_t = stderr_buf.clone();
    let out_thread = std::thread::spawn(move || drain_into(out_r, out_buf_t));
    let err_thread = std::thread::spawn(move || drain_into(err_r, err_buf_t));

    let deadline = Instant::now() + timeout;
    loop {
        {
            let so = stdout_buf.lock().unwrap();
            let se = stderr_buf.lock().unwrap();
            if until(&so, &se) || Instant::now() > deadline {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    child.kill().ok();
    child.wait().ok();
    out_thread.join().ok();
    err_thread.join().ok();

    Outcome {
        stdout: Arc::try_unwrap(stdout_buf).unwrap().into_inner().unwrap(),
        stderr: Arc::try_unwrap(stderr_buf).unwrap().into_inner().unwrap(),
    }
}

/// Pull the port out of `…http://host:PORT/_xerj-console/setup#token=…`.
fn port_from_setup_link(stderr: &str) -> Option<u16> {
    let marker = "/_xerj-console/setup#token=";
    let end = stderr.find(marker)?;
    let authority_end = &stderr[..end];
    let start = authority_end.rfind("http://")?;
    let authority = &authority_end[start + "http://".len()..end];
    let port_str = authority.rsplit(':').next()?;
    port_str.parse().ok()
}

/// Pull the host out of the same `…http://HOST:port/_xerj-console/setup#token=…`.
fn host_from_setup_link(stderr: &str) -> Option<String> {
    let marker = "/_xerj-console/setup#token=";
    let end = stderr.find(marker)?;
    let authority_end = &stderr[..end];
    let start = authority_end.rfind("http://")?;
    let authority = &authority_end[start + "http://".len()..end];
    let (host, port_str) = authority.rsplit_once(':').unwrap_or((authority, ""));
    // Guard against a future authority shape confusing the split: whatever
    // follows the colon must be the port for the host to mean anything.
    if port_str.is_empty() || port_str.chars().all(|c| c.is_ascii_digit()) {
        Some(host.to_string())
    } else {
        None
    }
}

/// Pull the port out of the banner's `ES-compat    127.0.0.1:PORT [...]` line.
fn port_from_banner(stdout: &str) -> Option<u16> {
    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("ES-compat"))?;
    let after_colon = line.rsplit_once(':')?.1;
    let digits: String = after_colon
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// The whole defect in one assertion: with `es_compat_port = 0`, the port
/// named in the first-launch setup link must be a real, non-zero port — and
/// specifically the one the banner (already fixed by #466, from
/// `local_addr()`) reports the node actually bound.
#[test]
fn ephemeral_es_port_console_link_names_the_bound_port() {
    let dir = tempfile::tempdir().unwrap();
    let (rest, grpc) = two_free_ports();

    let out = boot_until(
        &config_with_ephemeral_es_port(dir.path(), rest, grpc),
        dir.path(),
        Duration::from_secs(60),
        |stdout, stderr| {
            stdout.contains("ES-compat") && stderr.contains("/_xerj-console/setup#token=")
        },
    );

    let link_port = port_from_setup_link(&out.stderr).unwrap_or_else(|| {
        panic!(
            "no first-launch setup link found on stderr within the boot window\n\
             --- stdout ---\n{}\n--- stderr ---\n{}",
            out.stdout, out.stderr
        )
    });
    let banner_port = port_from_banner(&out.stdout).unwrap_or_else(|| {
        panic!(
            "no ES-compat banner line found on stdout within the boot window\n\
             --- stdout ---\n{}\n--- stderr ---\n{}",
            out.stdout, out.stderr
        )
    });

    assert_ne!(
        link_port, 0,
        "the setup link named port 0 — unreachable, and the exact bug \
         (issue #469): `es_compat_port = 0` leaked into the link instead of \
         the port the kernel actually bound\n--- stderr ---\n{}",
        out.stderr
    );
    assert_eq!(
        link_port, banner_port,
        "the setup link named a different port than the banner — the node \
         is telling the operator to open a URL it is not listening \
         on\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.stdout, out.stderr
    );
}

/// Issue #935: the first-launch link must be *usable*. A browser treats
/// `http://127.0.0.1:<port>` as an IP-literal origin, and WebAuthn refuses
/// IP-literal origins outright — so a setup link printed with the bind
/// address could never complete passkey enrolment, whatever the relying
/// party is configured as. Every loopback bind is therefore printed (and
/// its relying party derived) as `localhost`.
#[test]
fn loopback_bind_prints_the_setup_link_as_localhost() {
    let dir = tempfile::tempdir().unwrap();
    let (rest, grpc) = two_free_ports();

    let out = boot_until(
        &config_with_ephemeral_es_port(dir.path(), rest, grpc),
        dir.path(),
        Duration::from_secs(60),
        |stdout, stderr| {
            stdout.contains("ES-compat") && stderr.contains("/_xerj-console/setup#token=")
        },
    );

    let host = host_from_setup_link(&out.stderr).unwrap_or_else(|| {
        panic!(
            "no parsable first-launch setup link found on stderr within the boot window\n\
             --- stdout ---\n{}\n--- stderr ---\n{}",
            out.stdout, out.stderr
        )
    });

    assert_eq!(
        host, "localhost",
        "a loopback bind (127.0.0.1) must be printed as localhost in the \
         setup link: an IP-literal origin cannot complete WebAuthn enrolment \
         (#935)\n--- stderr ---\n{}",
        out.stderr
    );
}
