//! `--workers N` must produce a phase-A pool of exactly N threads.
//!
//! Before #240 phase A (content hashing, sniffing, sampling) ran on rayon's
//! default global pool, which is sized to every core on first use. A user who
//! asked for 2 workers on a 12-core Mac still got a 12-wide CPU storm, so
//! turning the knob down did not give the machine back — the behaviour behind
//! "Macs are really slow while autoindexing a large code base".
//!
//! The pool is process-global, so this lives in its own integration-test file:
//! one test binary, one pool, no ordering assumptions.

use rayon::prelude::*;
use xerj_autoindex::pool;

#[test]
fn configure_bounds_phase_a_to_the_requested_width() {
    let requested = 2;
    let machine = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap();
    assert!(
        machine > requested,
        "only meaningful on a machine with more cores than we ask for (machine={machine})"
    );

    pool::configure(requested);

    // Pre-fix this was `machine`: phase A saw rayon's global pool.
    let widths: Vec<usize> = pool::install(|| {
        (0..256)
            .into_par_iter()
            .map(|_| rayon::current_num_threads())
            .collect()
    });
    assert!(
        widths.iter().all(|&w| w == requested),
        "phase A must run at the requested width, saw {:?}",
        widths.iter().max()
    );

    // And the work really is confined to those threads, not merely counted.
    let threads: std::collections::BTreeSet<String> = pool::install(|| {
        (0..256)
            .into_par_iter()
            .map(|_| {
                std::thread::current()
                    .name()
                    .unwrap_or("unnamed")
                    .to_string()
            })
            .collect()
    });
    assert!(
        threads.len() <= requested,
        "phase A used {} distinct threads: {threads:?}",
        threads.len()
    );
    assert!(threads.iter().all(|n| n.starts_with("xerj-scan-")));
}
