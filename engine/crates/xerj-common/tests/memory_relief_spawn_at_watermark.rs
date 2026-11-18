//! A memory-relief helper (`xerj-drain-free`) must start when the governor
//! watermark gate is closed — that is when it is needed. Its own test binary
//! because the spawn gate is process-global (first install wins).

use std::sync::mpsc;
use std::time::Duration;

use xerj_common::thread_spawn::{
    install_spawn_gate, spawn_allowed, try_spawn, try_spawn_memory_relief,
};

fn at_watermark() -> bool {
    false
}

#[test]
fn drain_free_helper_starts_at_the_watermark() {
    assert!(install_spawn_gate(at_watermark));
    assert!(!spawn_allowed(), "gate simulates RSS at the watermark");

    // An ordinary best-effort helper is refused at the watermark...
    assert!(try_spawn("xerj-best-effort", || {}).is_err());

    // ...but the memory-relief helper starts and runs its job.
    let (tx, rx) = mpsc::channel();
    let handle = try_spawn_memory_relief("xerj-drain-free", move || {
        tx.send(()).unwrap();
    })
    .unwrap_or_else(|f| panic!("relief spawn refused at the watermark: {}", f.error));
    rx.recv_timeout(Duration::from_secs(10))
        .expect("relief job ran");
    handle.join().unwrap();
}
