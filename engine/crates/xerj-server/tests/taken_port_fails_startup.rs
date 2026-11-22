//! Issue #465: a node that could not bind a listener must not claim it did.
//!
//! Before the fix the three listeners were bound inside `tokio::spawn`s that
//! ran *after* the banner, and a bind error there was `error!`-logged and
//! explicitly non-fatal. A node whose ES-compat port was already held by
//! another process therefore printed the full success banner — that port, this
//! data directory, `Started in {ms}` — and then stayed up serving nothing on
//! it, because `tokio::join!` returns only once all three tasks end.
//!
//! The consequence is not cosmetic. The process that *does* own the port
//! answers `GET /_cluster/health` with `green`, which is the readiness probe
//! the docs prescribe, so `xerj autoindex` proceeds and streams the user's
//! corpus into a datastore they have never heard of and cannot name. Three
//! agents hit this independently in a field study; one wrote 905 files —
//! contracts, invoices, a bank export — into a stranger's data directory and
//! got `ok=true exit=0` for it.
//!
//! These are process-level tests because the defect is invisible from inside
//! the config crate: every value was valid, the machine simply said no. The
//! first two hold a port for real and assert the binary dies; the third holds
//! nothing and asserts it still lives, so a binary that refused to start for
//! any unrelated reason cannot pass this file.

use std::io::Read;
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Three foreign listeners this test owns for its whole lifetime, bound on
/// `:0` and **never released**: the port numbers are read off listeners the
/// test still holds, so nothing else on the machine can take one mid-test.
///
/// A taken-port test needs only the listeners *before* the taken one to be
/// bindable by the child. `main.rs` binds ES-compat, then native REST, then
/// gRPC, and stops at the first failure, so every port after the taken one is
/// never bound and can be one of these held listeners too. `Config::validate`
/// requires the three ports to differ, which holding them at once guarantees.
///
/// The earlier version released the ports the child had to bind (pick a free
/// port, drop it, hand it over). On a loaded machine another test's `:0` bind
/// took one in between, and the native-REST test failed with
/// "ES-compat: bind ...: Address already in use" (2 of 7 runs at load 10-14).
fn three_held_ports() -> [TcpListener; 3] {
    let bind = || xerj_common::loopback_test::expect_loopback(TcpListener::bind("127.0.0.1:0"));
    [bind(), bind(), bind()]
}

fn port_of(listener: &TcpListener) -> u16 {
    listener.local_addr().unwrap().port()
}

/// TOML-safe rendering of a temp path.
fn toml_path(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn config_for(dir: &std::path::Path, rest: u16, grpc: u16, es: u16) -> String {
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

struct Outcome {
    exited: bool,
    success: bool,
    stdout: String,
    stderr: String,
}

/// Spawn the real binary and watch it until it exits, until two seconds after
/// `ready` (when given) appears on its stdout, or for at most `wait`.
///
/// A binary that is still running when it returns is reported as
/// `exited: false` with whatever it has written so far — the pre-fix
/// behaviour every taken-port test here is written against. The pipes are
/// drained as the child writes, so a chatty child can never block on a full
/// pipe, and `ready` lets the control return the moment the node has started
/// instead of after a fixed window that a slow start on a loaded host
/// overruns (the old 20 s window failed 30 of 30 runs at load 40-60).
fn run(config_body: &str, dir: &std::path::Path, wait: Duration, ready: Option<&str>) -> Outcome {
    use std::sync::{Arc, Mutex};

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

    fn drain(
        mut pipe: impl Read + Send + 'static,
    ) -> (Arc<Mutex<String>>, std::thread::JoinHandle<()>) {
        let text = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&text);
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        (text, reader)
    }
    let (stdout, out_reader) = drain(child.stdout.take().expect("stdout piped"));
    let (stderr, err_reader) = drain(child.stderr.take().expect("stderr piped"));

    let mut deadline = Instant::now() + wait;
    let mut started = false;
    let mut exited = false;
    let mut success = false;
    loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            exited = true;
            success = status.success();
            break;
        }
        if !started && ready.is_some_and(|needle| stdout.lock().unwrap().contains(needle)) {
            // Started. Keep watching a moment so a node that dies right
            // after its banner is still caught; a healthy one never exits,
            // so this window cannot fail on a slow host.
            started = true;
            deadline = Instant::now() + Duration::from_secs(2);
        }
        if Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !exited {
        child.kill().ok();
        child.wait().ok();
    }
    // The child is gone, so both pipes reach EOF and the readers end.
    out_reader.join().ok();
    err_reader.join().ok();

    let stdout = stdout.lock().unwrap().clone();
    let stderr = stderr.lock().unwrap().clone();
    Outcome {
        exited,
        success,
        stdout,
        stderr,
    }
}

/// The whole defect in one assertion: someone else owns the ES-compat port, so
/// this process must not run.
#[test]
fn a_taken_es_compat_port_ends_the_process() {
    let dir = tempfile::tempdir().unwrap();
    // A foreign process — in the field, another XERJ node, a Docker publish,
    // or a real Elasticsearch — already holds it, for the whole test. ES-compat
    // is bound first, so REST and gRPC are never reached and stay held too.
    let held = three_held_ports();
    let (es, rest, grpc) = (port_of(&held[0]), port_of(&held[1]), port_of(&held[2]));

    let out = run(
        &config_for(dir.path(), rest, grpc, es),
        dir.path(),
        Duration::from_secs(60),
        None,
    );

    assert!(
        out.exited,
        "xerj kept running without the ES-compat port it was configured for. \
         Whoever holds :{es} now answers the readiness probe the docs \
         prescribe, so `xerj autoindex` will write this user's corpus into \
         that node's data directory instead of theirs.\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.stdout, out.stderr
    );
    assert!(
        !out.success,
        "xerj exited 0 after failing to bind :{es}; a supervisor or a script \
         reading the exit code learns nothing.\n--- stderr ---\n{}",
        out.stderr
    );
    // Deliberately matched against the bind error and not the bare port number:
    // the first-launch console link (`http://127.0.0.1:{es}/_xerj-console/setup`)
    // also reaches stderr, and before the fix, so `stderr.contains(port)` was
    // satisfied whether or not the failure was ever reported. Verified by
    // dropping `{addr}` from `bind_listener`'s context and watching the weaker
    // form still pass.
    assert!(
        out.stderr.contains(&format!("bind 127.0.0.1:{es}")),
        "the failure must name the port that was taken, so the operator can \
         act on it without reading the log\n--- stderr ---\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("Address already in use"),
        "the failure must say why the port could not be taken\n--- stderr ---\n{}",
        out.stderr
    );

    drop(held);
}

/// #1723 / #1738: the field reports hit a taken *native REST* port (8080), and
/// the error gave no route out. It must name that listener and its address,
/// keep the OS reason, say how `--port` moves it (`--port` is the ES-compat
/// port; native REST follows at BASE+1), and name the one config key that
/// moves only this listener.
#[test]
fn a_taken_native_rest_port_says_how_to_move_it() {
    let dir = tempfile::tempdir().unwrap();
    // ES-compat is bound before native REST and must succeed: port 0 has the
    // kernel assign it at the child's own bind, so no other process can hold
    // it first. gRPC comes after native REST and is never reached.
    let held = three_held_ports();
    let (es, rest, grpc) = (0, port_of(&held[0]), port_of(&held[1]));

    let out = run(
        &config_for(dir.path(), rest, grpc, es),
        dir.path(),
        Duration::from_secs(60),
        None,
    );

    assert!(
        out.exited && !out.success,
        "xerj must exit non-zero without its native REST port\n--- stderr ---\n{}",
        out.stderr
    );
    for needle in [
        format!("native REST: bind 127.0.0.1:{rest}: "),
        "Address already in use".to_string(),
        "native REST BASE+1".to_string(),
        "`rest_port`".to_string(),
        "--auto-port".to_string(),
    ] {
        assert!(
            out.stderr.contains(&needle),
            "the failure must contain {needle:?}\n--- stderr ---\n{}",
            out.stderr
        );
    }

    drop(held);
}

/// The banner is the lie the user acts on, so it gets its own assertion:
/// nothing that reads as success may reach stdout when a listener was lost.
#[test]
fn no_success_banner_is_printed_when_a_port_is_taken() {
    let dir = tempfile::tempdir().unwrap();
    let held = three_held_ports();
    let (es, rest, grpc) = (port_of(&held[0]), port_of(&held[1]), port_of(&held[2]));

    let out = run(
        &config_for(dir.path(), rest, grpc, es),
        dir.path(),
        Duration::from_secs(60),
        None,
    );

    for claim in ["Started in", "Xerj Console UI", "Data dir"] {
        assert!(
            !out.stdout.contains(claim),
            "the banner printed {claim:?} for a node that never got :{es}. \
             That line is what a user checks before pointing their documents \
             at it.\n--- stdout ---\n{}",
            out.stdout
        );
    }
    assert!(
        !out.stdout.contains(&format!("ES-compat    127.0.0.1:{es}")),
        "the banner advertised an ES-compat port this process does not \
         own\n--- stdout ---\n{}",
        out.stdout
    );

    drop(held);
}

/// Control. With every port free the same invocation must start and stay up —
/// otherwise the two assertions above would pass against a binary that simply
/// never starts, which is not the fix.
///
/// The only test here that must release ports for the child, so they come
/// from below the ephemeral range, where no other test's `:0` bind or
/// `connect()` can take one in between (`free_port_triple`). Still retried:
/// an explicit bind by an unrelated process would make the node *correctly*
/// refuse to start, and three losses in a row is not that race any more.
#[test]
fn with_the_ports_free_it_starts_and_stays_up() {
    let dir = tempfile::tempdir().unwrap();
    let mut out = None;
    let mut es = 0;
    for _ in 0..3 {
        let port = xerj_common::loopback_test::free_port_triple().release();
        let (rest, grpc) = (port + 1, port + 2);
        es = port;
        let attempt = run(
            &config_for(dir.path(), rest, grpc, es),
            dir.path(),
            // A hang guard, not a budget: `ready` ends the wait once the
            // node has started.
            Duration::from_secs(300),
            Some("Started in"),
        );
        let lost_a_port = attempt.exited && attempt.stderr.contains("Address already in use");
        out = Some(attempt);
        if !lost_a_port {
            break;
        }
    }
    let out = out.expect("at least one attempt");

    assert!(
        !out.exited,
        "xerj exited on its own with all three ports free\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.stdout, out.stderr
    );
    assert!(
        out.stdout.contains("Started in"),
        "a node that bound everything must still print its banner\n--- stdout ---\n{}",
        out.stdout
    );
    assert!(
        out.stdout.contains(&format!("ES-compat    127.0.0.1:{es}")),
        "the banner must name the ES-compat port it actually holds\n--- stdout ---\n{}",
        out.stdout
    );
}
