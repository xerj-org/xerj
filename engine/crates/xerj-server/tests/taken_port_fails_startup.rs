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

/// The foreign process that already owns the ES-compat port, plus two more
/// free ports for the listeners the node must still get.
///
/// The squatter is bound on `:0` and **never released** — the port number is
/// read off a listener this test still holds. Deriving it the other way round
/// (pick a free port, drop it, re-bind it) is what the first version did, and
/// it lost the race constantly: the two `#[test]`s in this file run on
/// separate threads, both drew from the same just-released ephemeral pool, and
/// whichever re-bound second got `AddrInUse` from the *other test* rather than
/// from the node under test.
///
/// `rest` and `grpc` still go through hold-then-release, which is unavoidable
/// — the child has to be the one to bind them. `Config::validate` requires all
/// three to differ, which holding them simultaneously guarantees.
fn squatter_and_two_free_ports() -> (TcpListener, u16, u16) {
    let squatter = xerj_common::loopback_test::expect_loopback(TcpListener::bind("127.0.0.1:0"));
    let held: Vec<TcpListener> = (0..2)
        .map(|_| xerj_common::loopback_test::expect_loopback(TcpListener::bind("127.0.0.1:0")))
        .collect();
    let p: Vec<u16> = held
        .iter()
        .map(|l| l.local_addr().unwrap().port())
        .collect();
    (squatter, p[0], p[1])
}

/// Three distinct free ports for the control, which needs every one of them to
/// still be free when the child starts.
fn three_free_ports() -> (u16, u16, u16) {
    let held: Vec<TcpListener> = (0..3)
        .map(|_| xerj_common::loopback_test::expect_loopback(TcpListener::bind("127.0.0.1:0")))
        .collect();
    let p: Vec<u16> = held
        .iter()
        .map(|l| l.local_addr().unwrap().port())
        .collect();
    (p[0], p[1], p[2])
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

/// Spawn the real binary and watch it for `wait` seconds.
///
/// Returns as soon as it exits. A binary that is still running when the window
/// closes is reported as `exited: false` with whatever it has written so far —
/// the pre-fix behaviour every test here is written against.
fn run(config_body: &str, dir: &std::path::Path, wait: Duration) -> Outcome {
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

    let deadline = Instant::now() + wait;
    let mut exited = false;
    let mut success = false;
    loop {
        match child.try_wait().expect("poll child") {
            Some(status) => {
                exited = true;
                success = status.success();
                break;
            }
            None if Instant::now() > deadline => {
                // Read the pipes BEFORE killing: the child holds the write
                // ends, and on a kill we still want everything it printed.
                break;
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }

    let mut stdout = String::new();
    let mut stderr = String::new();
    if exited {
        child
            .stdout
            .take()
            .expect("stdout piped")
            .read_to_string(&mut stdout)
            .expect("read child stdout");
        child
            .stderr
            .take()
            .expect("stderr piped")
            .read_to_string(&mut stderr)
            .expect("read child stderr");
    } else {
        child.kill().ok();
        child.wait().ok();
        // The pipes are drained after the kill so `read_to_string` sees EOF.
        child
            .stdout
            .take()
            .map(|mut s| s.read_to_string(&mut stdout));
        child
            .stderr
            .take()
            .map(|mut s| s.read_to_string(&mut stderr));
    }

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
    // or a real Elasticsearch — already holds it, for the whole test.
    let (squatter, rest, grpc) = squatter_and_two_free_ports();
    let es = squatter.local_addr().unwrap().port();

    let out = run(
        &config_for(dir.path(), rest, grpc, es),
        dir.path(),
        Duration::from_secs(60),
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

    drop(squatter);
}

/// #1723 / #1738: the field reports hit a taken *native REST* port (8080), and
/// the error gave no route out. It must name that listener and its address,
/// keep the OS reason, say how `--port` moves it (`--port` is the ES-compat
/// port; native REST follows at BASE+1), and name the one config key that
/// moves only this listener.
#[test]
fn a_taken_native_rest_port_says_how_to_move_it() {
    let dir = tempfile::tempdir().unwrap();
    let (squatter, es, grpc) = squatter_and_two_free_ports();
    let rest = squatter.local_addr().unwrap().port();

    let out = run(
        &config_for(dir.path(), rest, grpc, es),
        dir.path(),
        Duration::from_secs(60),
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

    drop(squatter);
}

/// The banner is the lie the user acts on, so it gets its own assertion:
/// nothing that reads as success may reach stdout when a listener was lost.
#[test]
fn no_success_banner_is_printed_when_a_port_is_taken() {
    let dir = tempfile::tempdir().unwrap();
    let (squatter, rest, grpc) = squatter_and_two_free_ports();
    let es = squatter.local_addr().unwrap().port();

    let out = run(
        &config_for(dir.path(), rest, grpc, es),
        dir.path(),
        Duration::from_secs(60),
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

    drop(squatter);
}

/// Control. With every port free the same invocation must start and stay up —
/// otherwise the two assertions above would pass against a binary that simply
/// never starts, which is not the fix.
///
/// Retried, unlike the other two: the three ports have to be released before
/// the child can bind them, so on a busy machine an unrelated process can take
/// one in between and the node would then be *correctly* refusing to start.
/// Three losses in a row is not that race any more.
#[test]
fn with_the_ports_free_it_starts_and_stays_up() {
    let dir = tempfile::tempdir().unwrap();
    let mut out = None;
    let mut es = 0;
    for _ in 0..3 {
        let (rest, grpc, port) = three_free_ports();
        es = port;
        let attempt = run(
            &config_for(dir.path(), rest, grpc, es),
            dir.path(),
            Duration::from_secs(20),
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
