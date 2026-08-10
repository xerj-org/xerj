//! Issue #228: a cleartext node must not publish itself to the network.
//!
//! TLS is off by default. A `server.bind_address` that is not loopback
//! therefore puts every listener — and the `Authorization: ApiKey` header of
//! every request that reaches them — on the wire in the clear. The REAL binary
//! must exit non-zero before binding anything unless the operator has said the
//! exposure is intended.
//!
//! Why a process-level test and not only the unit tests on
//! `Config::cleartext_exposed_off_loopback`: the pre-fix binary starts happily
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
    let (a, b, c, _) = four_free_ports();
    (a, b, c)
}

/// As above plus a fourth for the cluster transport.
fn four_free_ports() -> (u16, u16, u16, u16) {
    let held: Vec<TcpListener> = (0..4)
        .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    let p: Vec<u16> = held
        .iter()
        .map(|l| l.local_addr().unwrap().port())
        .collect();
    (p[0], p[1], p[2], p[3])
}

/// TOML-safe rendering of a temp path (Windows backslashes would be escape
/// sequences inside a basic string; forward slashes work on every platform).
fn toml_path(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Run the real binary against `config_body` (plus `extra_args`) and return
/// `(success, stderr)`.
///
/// The fixed binary rejects at step 3a or 3c of startup — before the data
/// directory, the first-run admin key, or any listener — so well under a
/// second even on a 2-core CI runner; 60 s is pure headroom.
fn run_until_exit(config_body: &str, dir: &std::path::Path, extra_args: &[&str]) -> (bool, String) {
    let config_path = dir.join("xerj.toml");
    std::fs::write(&config_path, config_body).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .arg("--config")
        .arg(&config_path)
        .args(extra_args)
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
                    "xerj kept running on a bind_address the startup checks must \
                     refuse — with tls.enabled = false that means plain HTTP on a \
                     network-reachable interface, so every API key it accepts \
                     crosses the wire in cleartext"
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

/// Build a config body binding `bind` on three free ports under `dir`.
fn config_for(bind: &str, dir: &std::path::Path, ports: (u16, u16, u16)) -> String {
    let (rest, grpc, es) = ports;
    format!(
        r#"
[server]
bind_address = "{bind}"
rest_port = {rest}
grpc_port = {grpc}
es_compat_port = {es}
data_dir = "{data}"
"#,
        data = toml_path(&dir.join("data")),
    )
}

/// `0.0.0.0` binds every interface the host has. With TLS off — the default —
/// that publishes the admin API key in cleartext to all of them. This is the
/// assertion that fails without the fix.
#[test]
fn cleartext_bind_on_every_interface_refuses_to_start() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let ports = three_free_ports();

    let (ok, stderr) = run_until_exit(&config_for("0.0.0.0", dir.path(), ports), dir.path(), &[]);

    assert!(
        !ok,
        "a non-loopback bind with TLS off must exit non-zero; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("cleartext") && stderr.contains("Refusing to start"),
        "the exit reason must name the exposure and the refusal; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("server.allow_insecure_network_bind"),
        "the operator must be told the setting that unblocks the boot, or the \
         refusal is a dead end; stderr:\n{stderr}"
    );
    // Refused at step 3c, before anything is created: no data directory, no
    // first-run admin key minted and printed. A rejected boot that still seeds
    // credentials leaves state to clean up — and an admin key on stdout that
    // was never actually used.
    assert!(
        !data_dir.exists(),
        "a rejected boot must not create the data dir; stderr:\n{stderr}"
    );
}

/// A private-network address is no more encrypted than a public one, and
/// `--insecure` drops authentication on top — which the shipped docs already
/// claimed refuses to run off-loopback while the binary happily served
/// unauthenticated writes to the LAN.
#[test]
fn insecure_flag_does_not_buy_a_network_bind() {
    let dir = tempfile::tempdir().unwrap();
    let ports = three_free_ports();

    let (ok, stderr) = run_until_exit(
        &config_for("10.0.0.7", dir.path(), ports),
        dir.path(),
        &["--insecure"],
    );

    assert!(
        !ok,
        "--insecure clears tls.enabled, so it must trip the check rather than \
         evade it; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("10.0.0.7"),
        "the refusal must quote the offending bind address; stderr:\n{stderr}"
    );
}

/// The default posture has to boot. `xerj` with no `[server]` block at all is
/// the first thing anyone runs, and the fix is worthless if it turns that into
/// a startup failure.
#[test]
fn default_and_loopback_binds_start_normally() {
    for bind in [None, Some("127.0.0.1"), Some("::1")] {
        let dir = tempfile::tempdir().unwrap();
        let (rest, grpc, es) = three_free_ports();
        let bind_line = match bind {
            Some(b) => format!("bind_address = \"{b}\"\n"),
            None => String::new(),
        };
        let config_path = dir.path().join("xerj.toml");
        std::fs::write(
            &config_path,
            format!(
                r#"
[server]
{bind_line}rest_port = {rest}
grpc_port = {grpc}
es_compat_port = {es}
data_dir = "{data}"
"#,
                data = toml_path(&dir.path().join("data")),
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

        // Generous enough for engine init on a slow runner, and far past 3c.
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
            "bind {bind:?} must start; it exited instead. stderr:\n{stderr}"
        );
        assert!(
            !stderr.contains("Refusing to start"),
            "the cleartext check must not fire on loopback; stderr:\n{stderr}"
        );
    }
}

/// The documented escape hatch has to actually work, or operators will reach
/// for something worse to get past the refusal. Asserted through the
/// environment variable specifically, because that is the path a container
/// image and the Helm chart take — a config key they cannot reach would make
/// the default change unshippable for Docker and Kubernetes.
///
/// Binds loopback so the test starts no publicly reachable listener; the
/// opt-out is what is under test, and the non-loopback half of the matrix is
/// covered by the unit tests in `xerj-common::config`.
#[test]
fn declared_exposure_is_accepted_from_config_and_env() {
    let dir = tempfile::tempdir().unwrap();
    let (rest, grpc, es) = three_free_ports();
    let config_path = dir.path().join("xerj.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[server]
bind_address = "127.0.0.1"
allow_insecure_network_bind = true
rest_port = {rest}
grpc_port = {grpc}
es_compat_port = {es}
data_dir = "{data}"
"#,
            data = toml_path(&dir.path().join("data")),
        ),
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .arg("--config")
        .arg(&config_path)
        .env("XERJ_ALLOW_INSECURE_NETWORK_BIND", "true")
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
        "allow_insecure_network_bind must be accepted as a config key and as \
         XERJ_ALLOW_INSECURE_NETWORK_BIND, and must not block startup. \
         stderr:\n{stderr}"
    );
}

/// A knob the binary accepts but will not honour is the bug class tracked in
/// #204. `XERJ_ALLOW_INSECURE_NETWORK_BIND=maybe` is not a boolean, and
/// treating it as `false` would refuse the boot with a message naming a
/// setting the operator believes they already set.
#[test]
fn unparseable_opt_out_env_fails_loudly() {
    let dir = tempfile::tempdir().unwrap();
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
"#,
            data = toml_path(&dir.path().join("data")),
        ),
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .arg("--config")
        .arg(&config_path)
        .env("XERJ_ALLOW_INSECURE_NETWORK_BIND", "maybe")
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
                panic!("a non-boolean XERJ_ALLOW_INSECURE_NETWORK_BIND was silently ignored");
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

    assert!(!status.success(), "must exit non-zero; stderr:\n{stderr}");
    assert!(
        stderr.contains("XERJ_ALLOW_INSECURE_NETWORK_BIND"),
        "the error must name the variable it could not read; stderr:\n{stderr}"
    );
}

/// `--bind` exists only because the default is loopback: without it there is
/// no way to expose the node other than writing a TOML file. If the flag were
/// parsed but not applied, the default change would look like a hard
/// regression to anyone who reached for the obvious flag.
#[test]
fn bind_flag_is_honoured_and_still_gated() {
    let dir = tempfile::tempdir().unwrap();
    let (rest, grpc, es) = three_free_ports();
    let config_path = dir.path().join("xerj.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[server]
rest_port = {rest}
grpc_port = {grpc}
es_compat_port = {es}
data_dir = "{data}"
"#,
            data = toml_path(&dir.path().join("data")),
        ),
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .arg("--config")
        .arg(&config_path)
        .arg("--bind")
        .arg("10.0.0.7")
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
                    "--bind 10.0.0.7 was accepted but not applied — the config file's \
                        loopback default survived, so the flag is inert"
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

    assert!(!status.success(), "must exit non-zero; stderr:\n{stderr}");
    assert!(
        stderr.contains("10.0.0.7"),
        "the refusal must quote the address --bind supplied, proving the flag \
         reached the config; stderr:\n{stderr}"
    );
}

/// The cluster transport must be reachable by the same IPv6 loopback spelling
/// the data listeners accept.
///
/// `bind_address = "::1"` is a spelling operators reach for now that the
/// default is loopback (#228), and the cluster listen address was the last
/// place composing one by `format!("{bind}:{port}")` — `"::1:9300"` is not a
/// socket address. The failure was worse than the data listeners': it lands at
/// step 8b, *after* the data directory exists and a first-run admin key has
/// been minted and printed, so the node leaves state behind and still cannot
/// start.
///
/// Alive is the assertion. A regressed binary exits with
/// `Error: parse cluster listen address`; the fixed one keeps running. If the
/// environment has no usable IPv6 transport the binary logs the degraded
/// single-node fallback and stays up — which still proves the address parsed,
/// the only thing under test here.
#[test]
fn ipv6_loopback_bind_starts_the_cluster_transport() {
    let dir = tempfile::tempdir().unwrap();
    let (rest, grpc, es, cluster) = four_free_ports();
    let config_path = dir.path().join("xerj.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[server]
bind_address = "::1"
rest_port = {rest}
grpc_port = {grpc}
es_compat_port = {es}
data_dir = "{data}"

[cluster]
enabled = true
port = {cluster}
auth_secret = "0123456789abcdef0123456789abcdef"
"#,
            data = toml_path(&dir.path().join("data")),
        ),
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .arg("--config")
        .arg(&config_path)
        .env("XERJ_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn xerj");

    // Long enough for engine init plus step 8b on a slow runner; the regressed
    // binary dies well before this and the loop breaks out early.
    let alive_until = Instant::now() + Duration::from_secs(20);
    let mut exited = None;
    while Instant::now() < alive_until {
        if let Some(status) = child.try_wait().expect("poll child") {
            exited = Some(status);
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
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
        !stderr.contains("parse cluster listen address"),
        "the cluster listen address must be composed from the parsed bind IP, \
         not from \"{{bind}}:{{port}}\"; stderr:\n{stderr}"
    );
    assert!(
        exited.is_none(),
        "bind_address = \"::1\" with cluster mode on must start; it exited \
         with {exited:?}. stderr:\n{stderr}"
    );
}

/// A `bind_address` that is not an IP literal must be told the truth about
/// itself, and must be told it before anything is created.
///
/// `Config::bind_address_is_loopback` fails closed on a value it cannot parse
/// — correct for a predicate guarding an exposure, and the reason `localhost`
/// used to be refused by step 3c with a message asserting that localhost "is
/// not loopback" and "would serve plain HTTP on a network-reachable
/// interface". Both false. Worse, the remedy that refusal named
/// (`allow_insecure_network_bind = true`) fixed nothing: it silenced 3c and
/// let the boot run to the bind at step 11, so the operator following the
/// instructions got a data directory, `.xerj_*` system indices, a master key,
/// a printed first-run `admin.key` — and then the same failure anyway.
///
/// Both halves are asserted here: the message is the true one, and the
/// declared-exposure path (the one the old message pointed at) refuses in the
/// same place rather than deferring.
#[test]
fn non_ip_bind_address_is_refused_early_and_truthfully() {
    for opt_out in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let (rest, grpc, es) = three_free_ports();
        let opt_out_line = if opt_out {
            "allow_insecure_network_bind = true\n"
        } else {
            ""
        };
        let body = format!(
            r#"
[server]
bind_address = "localhost"
{opt_out_line}rest_port = {rest}
grpc_port = {grpc}
es_compat_port = {es}
data_dir = "{data}"
"#,
            data = toml_path(&data_dir),
        );

        let (ok, stderr) = run_until_exit(&body, dir.path(), &[]);

        assert!(
            !ok,
            "a bind_address that is not an IP literal must exit non-zero \
             (allow_insecure_network_bind = {opt_out}); stderr:\n{stderr}"
        );
        assert!(
            stderr.contains("is not an IP address") && stderr.contains("localhost"),
            "the exit reason must name the real fault and quote the value \
             (allow_insecure_network_bind = {opt_out}); stderr:\n{stderr}"
        );
        assert!(
            !stderr.contains("is not loopback"),
            "localhost IS loopback — the refusal must not assert otherwise; \
             stderr:\n{stderr}"
        );
        assert!(
            !stderr.contains("allow_insecure_network_bind"),
            "the refusal must not prescribe an opt-out that does not fix the \
             fault and only defers the failure past the data directory; \
             stderr:\n{stderr}"
        );
        // The point of moving this to step 3a: with the opt-out set, the old
        // code reached step 11 and died there, leaving the data dir, the
        // system indices and a first-run admin key behind.
        assert!(
            !data_dir.exists(),
            "a rejected boot must create nothing (allow_insecure_network_bind \
             = {opt_out}); found {}. stderr:\n{stderr}",
            data_dir.display()
        );
    }
}

/// The same guard, reached through `--bind`. The flag is applied after the
/// config file is validated, so a check that lived in `Config::validate`
/// would never see it — this pins that the rejection is on the boot path both
/// sources funnel through.
#[test]
fn non_ip_bind_flag_is_refused_before_the_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let (rest, grpc, es) = three_free_ports();
    let config_path = dir.path().join("xerj.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[server]
rest_port = {rest}
grpc_port = {grpc}
es_compat_port = {es}
data_dir = "{data}"
"#,
            data = toml_path(&data_dir),
        ),
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .arg("--config")
        .arg(&config_path)
        .arg("--bind")
        .arg("db.internal.example")
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
                panic!("--bind db.internal.example must be refused, not served");
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

    assert!(!status.success(), "must exit non-zero; stderr:\n{stderr}");
    assert!(
        stderr.contains("is not an IP address") && stderr.contains("db.internal.example"),
        "host names are not resolved, and the refusal must say so while \
         quoting the value; stderr:\n{stderr}"
    );
    assert!(
        !data_dir.exists(),
        "a rejected boot must create nothing; found {}. stderr:\n{stderr}",
        data_dir.display()
    );
}
