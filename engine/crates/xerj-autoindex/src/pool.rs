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
//!
//! A pool that cannot be started is not fatal (#1122). `build()` fails when
//! `pthread_create` returns `EAGAIN` (thread or memory limit), and release
//! builds are `panic = "abort"`, so the old `.expect(..)` took the whole run
//! down. Now phase A runs on the calling thread instead, slower but complete,
//! and the next phase tries to start the pool again.

use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use rayon::{ThreadPool, ThreadPoolBuildError, ThreadPoolBuilder};

static SCAN_POOL: OnceLock<ThreadPool> = OnceLock::new();
/// Serialises pool construction, so two first callers do not both start a
/// pool's worth of threads.
static BUILD_LOCK: Mutex<()> = Mutex::new(());
/// The width [`configure`] asked for (0 = never configured). Kept so a pool
/// that failed to start is retried at the configured width, not at the
/// machine's.
static REQUESTED: AtomicUsize = AtomicUsize::new(0);

/// Fix the phase-A pool width for this process. Called once per run, before
/// phase A starts.
///
/// The first call wins, because rayon threads cannot be resized once started.
/// A later call asking for a different width says so rather than pretending —
/// autoindex runs one index per process today, so this only fires if that ever
/// changes.
pub fn configure(threads: usize) {
    let threads = threads.max(1);
    let _ = REQUESTED.compare_exchange(0, threads, Ordering::Relaxed, Ordering::Relaxed);
    // `None`: the pool could not be started and that was already reported.
    if let Some(pool) = scan_pool_with(threads) {
        let installed = pool.current_num_threads();
        if installed != threads {
            eprintln!(
                "autoindex: phase-A pool is already running with {installed} threads; \
                 ignoring the request for {threads}"
            );
        }
    }
}

/// The phase-A pool, built on first use at the configured width, or the
/// machine's full width if [`configure`] was never called (library use, unit
/// tests). `None` when the pool could not be started; [`install`] then runs
/// the work on the calling thread.
pub fn scan_pool() -> Option<&'static ThreadPool> {
    let threads = match REQUESTED.load(Ordering::Relaxed) {
        0 => xerj_common::resource::cores(),
        n => n,
    };
    scan_pool_with(threads)
}

/// How many threads phase A is running on right now: the scan pool's width,
/// or 1 when it could not be started.
pub fn width() -> usize {
    scan_pool().map_or(1, ThreadPool::current_num_threads)
}

fn scan_pool_with(threads: usize) -> Option<&'static ThreadPool> {
    pool_in(&SCAN_POOL, || {
        ThreadPoolBuilder::new()
            .num_threads(threads.max(1))
            .thread_name(|i| format!("xerj-scan-{i}"))
            .build()
    })
}

/// The pool in `slot`, built with `build` if it is not there yet. A failed
/// build leaves `slot` empty, so the next call tries again.
fn pool_in(
    slot: &OnceLock<ThreadPool>,
    build: impl FnOnce() -> Result<ThreadPool, ThreadPoolBuildError>,
) -> Option<&ThreadPool> {
    if let Some(pool) = slot.get() {
        return Some(pool);
    }
    let _guard = BUILD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(pool) = slot.get() {
        return Some(pool);
    }
    match build() {
        Ok(pool) => Some(slot.get_or_init(|| pool)),
        Err(e) => {
            eprintln!(
                "autoindex: could not start the phase-A scan pool ({e}); \
                 running this phase on the calling thread (#1122)"
            );
            None
        }
    }
}

/// Run `f` inside the phase-A pool. Any rayon work started by `f` — including
/// nested `par_iter`s — is confined to that pool's threads. If the pool could
/// not be started, `f` runs on the calling thread instead (see
/// [`sequential`]).
pub fn install<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    install_in(scan_pool(), f)
}

fn install_in<R: Send>(pool: Option<&ThreadPool>, f: impl FnOnce() -> R + Send) -> R {
    match pool {
        Some(pool) => pool.install(f),
        None => sequential(f),
    }
}

thread_local! {
    /// A one-thread pool made of the calling thread itself. Kept for the
    /// thread's lifetime: the thread stays registered as its worker either way.
    static INLINE_POOL: RefCell<Option<ThreadPool>> = const { RefCell::new(None) };
}

/// Run `f`, and every `par_iter` inside it, on the calling thread, without
/// starting a thread.
///
/// Calling `f()` directly would not do: a `par_iter` on a thread that is not
/// a rayon worker goes to rayon's global pool, which is built lazily and
/// panics if it cannot start its threads, the same failure we are avoiding.
/// A `use_current_thread()` pool of width 1 spawns nothing and makes this
/// thread its only worker, so the parallel iterators run inline. rayon falls
/// back to the same construction when threads are unsupported
/// (`rayon-core` `registry.rs`, `default_global_registry`).
fn sequential<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    // Already a rayon worker (of a pool built here earlier, or of any other
    // pool): `par_iter` stays on that worker's own pool.
    if rayon::current_thread_index().is_some() {
        return f();
    }
    let built = ThreadPoolBuilder::new()
        .num_threads(1)
        .use_current_thread()
        .build();
    match built {
        Ok(pool) => {
            let out = pool.install(f);
            INLINE_POOL.with(|slot| *slot.borrow_mut() = Some(pool));
            out
        }
        Err(e) => {
            // Not expected: the builder starts no thread. Best effort.
            eprintln!("autoindex: could not register the calling thread with rayon ({e})");
            f()
        }
    }
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
        let pool_width = width();
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

    /// A pool whose every thread spawn fails, as `pthread_create` does with
    /// `EAGAIN` under memory exhaustion. Injected through rayon's own spawn
    /// hook, so `build()` takes its real failure path.
    fn build_with_failing_spawn() -> Result<ThreadPool, ThreadPoolBuildError> {
        ThreadPoolBuilder::new()
            .num_threads(4)
            .spawn_handler(|_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "injected EAGAIN",
                ))
            })
            .build()
    }

    /// #1122: a scan pool that cannot start used to `.expect(..)`, which is an
    /// abort under `panic = "abort"`. Now the build reports `None`, the slot
    /// stays empty so a later call can retry, and the phase still produces
    /// its full, ordered result, computed on the calling thread alone.
    #[test]
    fn a_pool_that_cannot_spawn_degrades_to_the_calling_thread() {
        // A fresh thread: the fallback registers its caller as a rayon worker
        // for the rest of that thread's life.
        std::thread::spawn(|| {
            let slot = OnceLock::new();
            assert!(pool_in(&slot, build_with_failing_spawn).is_none());
            assert!(slot.get().is_none(), "a failed build must not be cached");

            let caller = std::thread::current().id();
            let out: Vec<(u64, bool, usize)> =
                install_in(pool_in(&slot, build_with_failing_spawn), || {
                    (0..1000u64)
                        .into_par_iter()
                        .map(|i| {
                            (
                                i * i,
                                std::thread::current().id() == caller,
                                rayon::current_num_threads(),
                            )
                        })
                        .collect()
                });
            assert_eq!(out.len(), 1000);
            assert!(out
                .iter()
                .enumerate()
                .all(|(i, &(sq, _, _))| sq == (i * i) as u64));
            // Every item ran here: neither a scan-pool thread nor rayon's
            // global pool (which could not have started either) was used.
            assert!(out.iter().all(|&(_, here, _)| here));
            assert!(out.iter().all(|&(_, _, width)| width == 1));

            // A second phase on the same thread reuses the inline pool.
            let again: u64 = install_in(None, || (0..100u64).into_par_iter().sum());
            assert_eq!(again, 4950);

            // Once spawning works again, the next call builds the real pool.
            let pool = pool_in(&slot, || ThreadPoolBuilder::new().num_threads(2).build());
            assert_eq!(pool.map(ThreadPool::current_num_threads), Some(2));
            assert!(slot.get().is_some());
        })
        .join()
        .unwrap();
    }

    /// The fallback inside an existing rayon worker does not register a new
    /// pool (that would fail: a thread can belong to only one); it runs on the
    /// worker's own pool.
    #[test]
    fn the_fallback_inside_a_rayon_worker_stays_on_that_pool() {
        let outer = ThreadPoolBuilder::new().num_threads(2).build().unwrap();
        let widths: Vec<usize> = outer.install(|| {
            install_in(None, || {
                (0..64)
                    .into_par_iter()
                    .map(|_| rayon::current_num_threads())
                    .collect()
            })
        });
        assert!(widths.iter().all(|&w| w == 2));
    }
}
