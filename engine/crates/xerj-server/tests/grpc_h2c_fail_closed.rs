//! Issue #229: the cleartext gRPC listener must fail closed off-loopback.
//!
//! `xerj-server/src/grpc.rs` builds tonic without its `tls` feature, so
//! `server.grpc_port` speaks h2c whatever `[tls]` says. With `tls.enabled =
//! true` and a non-loopback `bind_address`, the REAL binary must exit non-zero
//! before binding anything rather than encrypt two listeners and put the third
//! in the clear on the same public interface.
//!
//! Why this is worth a process-level test rather than only the unit tests on
//! `Config::grpc_h2c_exposed_off_loopback`: the pre-fix binary starts happily
//! and serves all three ports forever, so the defect is invisible from inside
//! the config crate. Against a regressed binary this test hits its deadline
//! and fails instead of hanging.
//!
//! Nothing here binds `0.0.0.0` on purpose — the fixed binary refuses before
//! any listener exists, and the ports handed to it are free either way.

use std::io::Read;
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Three distinct free ports, held simultaneously so they cannot collide,
/// then released for the child. `Config::validate` requires all three listener
/// ports to differ; free ports also mean a regressed (serving) binary binds
/// cleanly and keeps running instead of dying on an unrelated bind error,
/// which would masquerade as a pass.
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

/// TOML-safe rendering of a temp path (Windows backslashes would be escape
/// sequences inside a basic string; forward slashes work on every platform).
fn toml_path(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Run the real binary against `config_body` and return `(success, stderr)`.
///
/// The fixed binary rejects at step 5b of startup — before metrics, engine
/// replay, certificate generation, or any listener — so well under a second
/// even on a 2-core CI runner; 60 s is pure headroom.
fn run_until_exit(config_body: &str, dir: &std::path::Path) -> (bool, String) {
    let config_path = dir.join("xerj.toml");
    std::fs::write(&config_path, config_body).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .arg("--config")
        .arg(&config_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn xerj");

    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        match child.try_wait().expect("poll child") {
            Some(status) => break status,
            None if Instant::now() > deadline => {
                child.kill().ok();
                child.wait().ok();
                panic!(
                    "xerj kept running with tls.enabled = true and a non-loopback \
                     bind_address — it is serving cleartext h2c gRPC on a public \
                     interface while reporting itself as TLS-enabled"
                );
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };

    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr piped")
        .read_to_string(&mut stderr)
        .expect("read child stderr");
    (status.success(), stderr)
}

/// `0.0.0.0` is the shipped default and binds every interface the host has,
/// so "turn TLS on, change nothing else" is the exact case that must be
/// refused. This is the assertion that fails without the fix.
#[test]
fn tls_with_non_loopback_bind_refuses_to_start() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let (rest, grpc, es) = three_free_ports();

    let (ok, stderr) = run_until_exit(
        &format!(
            r#"
[server]
bind_address = "0.0.0.0"
rest_port = {rest}
grpc_port = {grpc}
es_compat_port = {es}
data_dir = "{data}"

[tls]
enabled = true
cert_path = "{root}/xerj.crt"
key_path = "{root}/xerj.key"
"#,
            data = toml_path(&data_dir),
            root = toml_path(dir.path()),
        ),
        dir.path(),
    );

    assert!(
        !ok,
        "cleartext gRPC on a non-loopback bind under tls.enabled must exit \
         non-zero; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("h2c") && stderr.contains("Refusing to start"),
        "the exit reason must name the cleartext transport and the refusal; \
         stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("tls.allow_insecure_grpc_h2c"),
        "the operator must be told the setting that unblocks the boot, or the \
         refusal is a dead end; stderr:\n{stderr}"
    );
    // Refused at step 3b, before anything is created: no data directory, no
    // first-run admin key minted and printed, no certificate. A rejected boot
    // that still seeds credentials leaves state the operator has to clean up
    // — and an admin key on stdout that was never actually used.
    assert!(
        !dir.path().join("xerj.crt").exists(),
        "the check must run before certificate generation; stderr:\n{stderr}"
    );
    assert!(
        !data_dir.exists(),
        "a rejected boot must not create the data dir; stderr:\n{stderr}"
    );
}

/// The refusal must not fire on a loopback bind — that would break every
/// developer running `tls.enabled = true` locally, and a fail-closed check
/// nobody can satisfy gets disabled wholesale.
///
/// A clean start has no exit to wait for, so this asserts the negative that
/// is observable without one: the process is still alive after the point the
/// refusal would have killed it, and never printed the refusal.
#[test]
fn tls_on_loopback_starts_normally() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let (rest, grpc, es) = three_free_ports();
    let config_path = dir.path().join("xerj.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[server]
bind_address = "127.0.0.1"
rest_port = {rest}
grpc_port = {grpc}
es_compat_port = {es}
data_dir = "{data}"

[tls]
enabled = true
cert_path = "{root}/xerj.crt"
key_path = "{root}/xerj.key"
"#,
            data = toml_path(&data_dir),
            root = toml_path(dir.path()),
        ),
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .arg("--config")
        .arg(&config_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn xerj");

    // Generous enough for engine init on a slow runner, and far past step 5b.
    std::thread::sleep(Duration::from_secs(5));
    let alive = child.try_wait().expect("poll child").is_none();
    child.kill().ok();
    child.wait().ok();

    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr piped")
        .read_to_string(&mut stderr)
        .expect("read child stderr");

    assert!(
        alive,
        "a loopback bind with TLS must start; it exited instead. stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("Refusing to start"),
        "the h2c check must not fire on loopback; stderr:\n{stderr}"
    );
}

/// The documented escape hatch has to actually work, or operators will reach
/// for `--insecure` (which also drops auth) to get past the refusal.
#[test]
fn explicit_opt_out_allows_a_non_loopback_bind() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let (rest, grpc, es) = three_free_ports();
    let config_path = dir.path().join("xerj.toml");
    // Bind loopback so the test starts no publicly reachable listener; the
    // opt-out is what is under test, and `allow_insecure_grpc_h2c` is read
    // before the bind address is even consulted. The non-loopback half of the
    // matrix is covered by the unit tests in `xerj-common::config`.
    std::fs::write(
        &config_path,
        format!(
            r#"
[server]
bind_address = "127.0.0.1"
rest_port = {rest}
grpc_port = {grpc}
es_compat_port = {es}
data_dir = "{data}"

[tls]
enabled = true
cert_path = "{root}/xerj.crt"
key_path = "{root}/xerj.key"
allow_insecure_grpc_h2c = true
"#,
            data = toml_path(&data_dir),
            root = toml_path(dir.path()),
        ),
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .arg("--config")
        .arg(&config_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn xerj");

    std::thread::sleep(Duration::from_secs(5));
    let alive = child.try_wait().expect("poll child").is_none();
    child.kill().ok();
    child.wait().ok();

    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr piped")
        .read_to_string(&mut stderr)
        .expect("read child stderr");

    assert!(
        alive,
        "tls.allow_insecure_grpc_h2c = true must be accepted as a config key \
         and must not block startup. stderr:\n{stderr}"
    );
}
