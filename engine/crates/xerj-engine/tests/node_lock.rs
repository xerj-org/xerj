//! RC4 blocker 13 regression: data-dir exclusivity via `<data_dir>/node.lock`.
//!
//! Before the fix a second xerj process pointed at a live data dir booted
//! happily: it re-opened every index, replayed the WAL, and flushed
//! duplicate segments into the directory the first process was still
//! serving (live-reproduced: duplicate `min_seq=1/max_seq=1` segments from
//! the second boot). The engine now takes an exclusive OS-level lock on
//! `<data_dir>/node.lock` before any index open / WAL replay, and fails
//! fast with the holder's pid.

use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_engine::Engine;

fn config_for(dir: &TempDir) -> Config {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_engine_on_same_data_dir_fails_fast_with_holder_pid() {
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
    let reopened = Engine::new(config_for(&dir));
    assert!(
        reopened.is_ok(),
        "reopen after the holder exits must succeed: {:?}",
        reopened.err()
    );
}
