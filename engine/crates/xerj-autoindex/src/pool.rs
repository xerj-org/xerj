//! The phase-A worker pool.
//!
//! Phase A — full-content hashing (`content::resolve`) and sniff/sample
//! (`build_phase_a`) — is the CPU-bound half of an autoindex run. Both used
//! bare `par_iter()`, which runs on rayon's *global* pool: a pool nothing in
//! this crate ever sized, so it took every core on the machine regardless of
//! `--workers`. `--workers 2` on a 12-core Mac still produced a 12-wide digest
//! storm (#240 §2), which is why turning the knob down did not give the machine
//! back.
//!
//! This module owns a pool of the width the run's [`crate::resources::Plan`]
//! asked for, and phase A runs inside it.

use std::sync::OnceLock;

static SCAN_POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();

/// Fix the phase-A pool width for this process. Called once per run, before
/// phase A starts.
///
/// The first call wins, because rayon threads cannot be resized once started.
/// A later call asking for a different width says so rather than pretending —
/// autoindex runs one index per process today, so this only fires if that ever
/// changes.
pub fn configure(threads: usize) {
    let threads = threads.max(1);
    let installed = scan_pool_with(threads).current_num_threads();
    if installed != threads {
        eprintln!(
            "autoindex: phase-A pool is already running with {installed} threads; \
             ignoring the request for {threads}"
        );
    }
}

/// The phase-A pool, built on first use at the machine's full width if
/// [`configure`] was never called (library use, unit tests).
pub fn scan_pool() -> &'static rayon::ThreadPool {
    scan_pool_with(xerj_common::resource::cores())
}

fn scan_pool_with(threads: usize) -> &'static rayon::ThreadPool {
    SCAN_POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads.max(1))
            .thread_name(|i| format!("xerj-scan-{i}"))
            .build()
            .expect("failed to build the autoindex scan pool")
    })
}

/// Run `f` inside the phase-A pool. Any rayon work started by `f` — including
/// nested `par_iter`s — is confined to that pool's threads.
pub fn install<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    scan_pool().install(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rayon::prelude::*;

    /// The pool is process-global, so this asserts the property that holds
    /// whatever width the crate's other tests installed first: work submitted
    /// through `install` runs on the scan pool, never on rayon's default
    /// global pool.
    #[test]
    fn work_submitted_through_install_stays_in_the_scan_pool() {
        let widths: Vec<usize> = install(|| {
            (0..64)
                .into_par_iter()
                .map(|_| rayon::current_num_threads())
                .collect()
        });
        let pool_width = scan_pool().current_num_threads();
        assert!(widths.iter().all(|&w| w == pool_width));
        let name_is_ours = install(|| {
            (0..64).into_par_iter().all(|_| {
                std::thread::current()
                    .name()
                    .is_some_and(|n| n.starts_with("xerj-scan-"))
            })
        });
        assert!(name_is_ours, "phase A must not run on the global pool");
    }
}
