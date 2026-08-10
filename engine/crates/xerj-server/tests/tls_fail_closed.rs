//! Issue #200: TLS must fail closed at startup.
//!
//! With `tls.enabled = true` and a certificate step that cannot deliver, the
//! REAL binary must exit non-zero before binding anything — never log the
//! failure and serve the same ports as cleartext HTTP. The pre-fix behavior
//! (downgrade + serve forever) makes every client keep working, which is
//! exactly why nobody notices; this test would then hit its deadline and
//! fail rather than hang.
//!
//! Failure shape: `Config::validate` rejects empty cert/key paths at load
//! time, so the reachable in-production failure is non-empty paths whose
//! files are missing — that routes `ensure_tls_cert` into self-signed
//! auto-generation targeting `<data_dir>/xerj.crt`, which a squatting
//! directory makes fail deterministically on every platform, for root too.

use std::io::Read;
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Three distinct free ports, held simultaneously so they cannot collide,
/// then released for the child. `Config::validate` requires all three
/// listener ports to differ; free ports also mean that a regressed
/// (downgrading) binary binds cleanly and keeps serving instead of dying on
/// an unrelated bind error — which would masquerade as a pass.
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

#[test]
fn tls_failure_refuses_to_start_instead_of_downgrading() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    // Obstruct the auto-generation target: `<data_dir>/xerj.crt` is a
    // directory, so writing the generated certificate must fail.
    std::fs::create_dir_all(data_dir.join("xerj.crt")).unwrap();

    let (rest, grpc, es) = three_free_ports();
    let config = format!(
        r#"
[server]
bind_address = "127.0.0.1"
rest_port = {rest}
grpc_port = {grpc}
es_compat_port = {es}
data_dir = "{data}"

[tls]
enabled = true
cert_path = "{root}/missing.crt"
key_path = "{root}/missing.key"
"#,
        data = toml_path(&data_dir),
        root = toml_path(dir.path()),
    );
    let config_path = dir.path().join("xerj.toml");
    std::fs::write(&config_path, config).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .arg("--config")
        .arg(&config_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn xerj");

    // The fixed binary aborts at step 6 of startup, before metrics, engine
    // replay, or any listener — well under a second even on a 2-core CI
    // runner; 60 s is pure headroom. A regressed binary serves forever, so
    // bound the wait and treat "still running" as the downgrade it is.
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        match child.try_wait().expect("poll child") {
            Some(status) => break status,
            None if Instant::now() > deadline => {
                child.kill().ok();
                child.wait().ok();
                panic!(
                    "xerj kept running although TLS could not be established — \
                     it downgraded to plain HTTP instead of failing closed"
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

    assert!(
        !status.success(),
        "a TLS setup failure must exit non-zero, got {status:?}; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("TLS") && stderr.contains("refusing to start"),
        "the exit reason must name TLS and the refusal, so the operator \
         learns why the server would not come up; stderr:\n{stderr}"
    );
}
