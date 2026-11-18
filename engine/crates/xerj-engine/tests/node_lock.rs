//! RC4 blocker 13 regression: data-dir exclusivity via `<data_dir>/node.lock`.
//!
//! Before the fix a second xerj process pointed at a live data dir booted
//! happily: it re-opened every index, replayed the WAL, and flushed
//! duplicate segments into the directory the first process was still
//! serving (live-reproduced: duplicate `min_seq=1/max_seq=1` segments from
//! the second boot). The engine now takes an exclusive OS-level lock on
//! `<data_dir>/node.lock` before any index open / WAL replay, and fails
//! fast with the holder's pid.

use std::sync::{Mutex, OnceLock};

use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_engine::Engine;

fn config_for(dir: &TempDir) -> Config {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config
}

/// `Engine::new` installs process-wide governor and pool state, and the
/// stale-lock test rebuilds the tracing interest cache. The two tests in
/// this binary have to take turns.
fn serialize_tests() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_engine_on_same_data_dir_fails_fast_with_holder_pid() {
    let _guard = serialize_tests();
    let dir = TempDir::new().unwrap();
    let engine = Engine::new(config_for(&dir)).expect("first engine must boot");

    // Second engine on the SAME live dir must refuse to start…
    let err = match Engine::new(config_for(&dir)) {
        Ok(_) => panic!("second engine on a live data dir must fail fast"),
        Err(e) => e.to_string(),
    };
    // …with a self-explanatory message carrying the holder's pid.
    assert!(
        err.contains("already in use"),
        "unexpected lock error: {err}"
    );
    assert!(
        err.contains(&std::process::id().to_string()),
        "lock error should name the holding pid: {err}"
    );

    // Dropping the holder releases the OS lock; the stale node.lock FILE
    // left behind (as after kill -9) must never block the next boot.
    drop(engine);
    let reopened = reopen_after_drop(&dir);
    assert!(
        reopened.is_ok(),
        "reopen after the holder exits must succeed: {:?} (open node.lock fds: {})",
        reopened.err(),
        node_lock_fds()
    );
}

/// The OS lock is released when the holder's file is closed. Full-workspace
/// CI has still observed `WouldBlock` from this process immediately after
/// `drop` (this branch's run 40152941822, and `main` run 40145228672) and
/// a pass of the same test on run 40148648744. Wait briefly for that close
/// to become visible. A lock that stays held still fails.
fn reopen_after_drop(dir: &TempDir) -> Result<Engine, xerj_engine::EngineError> {
    let mut last = Engine::new(config_for(dir));
    for _ in 0..20 {
        if last.is_ok() {
            return last;
        }
        let still_ours = last
            .as_ref()
            .err()
            .map(|err| err.to_string())
            .is_some_and(|text| text.contains("already in use") && text.contains("node.lock"));
        if !still_ours {
            return last;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        last = Engine::new(config_for(dir));
    }
    last
}

fn node_lock_fds() -> String {
    let Ok(dir) = std::fs::read_dir("/proc/self/fd") else {
        return "unavailable".to_string();
    };
    let mut hits = Vec::new();
    for entry in dir.flatten() {
        if let Ok(target) = std::fs::read_link(entry.path()) {
            let text = target.to_string_lossy();
            if text.contains("node.lock") {
                hits.push(text.into_owned());
            }
        }
    }
    if hits.is_empty() {
        "none".to_string()
    } else {
        hits.join(", ")
    }
}

/// A `node.lock` left by a process that has exited must not block the next
/// boot, and the reclaim is logged. This is the file `kill -9` leaves once
/// the kernel has dropped `flock`, and the file a crashed Windows process
/// leaves behind when `LockFileEx` does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_node_lock_from_a_dead_pid_is_reclaimed_on_start() {
    let _guard = serialize_tests();
    let dir = TempDir::new().unwrap();
    let dead = reaped_pid();
    std::fs::write(dir.path().join("node.lock"), format!("{dead}\n")).unwrap();

    let logs = capture_warns(|| {
        let engine = Engine::new(config_for(&dir)).expect("dead pid lock file must be reclaimed");
        let stamped = std::fs::read_to_string(dir.path().join("node.lock")).unwrap();
        assert_eq!(
            stamped.trim().parse::<u32>().ok(),
            Some(std::process::id()),
            "the live process stamps node.lock, got {stamped:?}"
        );

        // The reclaimed lock is the real exclusive lock: a second start blocks.
        let err = match Engine::new(config_for(&dir)) {
            Ok(_) => panic!("second engine on the reclaimed data dir must fail fast"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("already in use"), "{err}");
        assert!(
            err.contains(&std::process::id().to_string()),
            "lock error should name the holding pid: {err}"
        );
        drop(engine);
    });
    assert!(
        logs.contains(&format!(
            "reclaimed stale node.lock left by pid {dead} (process is not running)"
        )),
        "reclaim must be logged: {logs}"
    );
}

fn reaped_pid() -> u32 {
    for _ in 0..20 {
        let mut cmd = if cfg!(windows) {
            let mut cmd = std::process::Command::new("cmd");
            cmd.args(["/C", "exit", "0"]);
            cmd
        } else {
            std::process::Command::new("true")
        };
        let mut child = cmd.spawn().expect("spawn a process that exits");
        let pid = child.id();
        let status = child.wait().unwrap();
        assert!(status.success());
        // The child has been waited on, so it is not a zombie. `ps` exiting
        // non-zero is "no such process". A recycled pid is still running;
        // try another one. `ps` missing (Windows) skips the probe.
        if !pid_still_listed(pid) {
            return pid;
        }
    }
    panic!("could not observe a reaped pid");
}

fn pid_still_listed(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string()])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

#[derive(Clone)]
struct LogBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

struct LogWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter(self.0.clone())
    }
}

fn capture_warns(body: impl FnOnce()) -> String {
    let buffer = LogBuffer(std::sync::Arc::new(std::sync::Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_target(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(buffer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    tracing::callsite::rebuild_interest_cache();
    body();
    let bytes = buffer.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}
