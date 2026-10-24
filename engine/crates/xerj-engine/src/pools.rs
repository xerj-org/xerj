//! Where the engine's thread pools get their widths.
//!
//! Every pool in this crate used to size itself inline from
//! `available_parallelism()` with its own magic fraction, while
//! `EngineConfig::{flush_workers, merge_workers, search_workers}` — declared,
//! defaulted, documented and validated in `xerj-common` — were read by nothing
//! at all. An operator who set `engine.search_workers = 2` to keep XERJ off
//! their laptop's cores got no effect and no warning: the accepted-and-ignored
//! class tracked in #204, and half of #240.
//!
//! This module is the single seam between configuration and pool construction:
//! [`resolve`] turns an [`EngineConfig`] plus the machine policy in
//! [`xerj_common::resource`] into concrete widths, [`init`] installs them once
//! per process, and the pools in `lib.rs` read them. Defaults reproduce the
//! widths the pools already used — the measured ingest/read tuning is
//! unchanged; what changes is that the knobs now do what they say.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use rayon::{ThreadPool, ThreadPoolBuildError, ThreadPoolBuilder};
use xerj_common::config::EngineConfig;
use xerj_common::resource::{self, Workload};

/// Concrete thread counts for one process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolSizing {
    /// `ingest_pool`: bulk parse/analyze/insert. Latency-critical for writers.
    pub ingest: usize,
    /// `background_pool`: flush side-cars and segment finalisation.
    pub background: usize,
    /// `merge_pool`: merge re-encode. Pure maintenance.
    pub merge: usize,
    /// `flush_finalize_gate`: how many shard finalizes may run at once.
    pub flush_finalize: usize,
    /// Rayon's global pool, which is what search segment fan-out runs on.
    pub search: usize,
}

/// What [`resolve`] decided, plus anything the operator asked for that could
/// not be honoured verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub sizing: PoolSizing,
    pub warnings: Vec<String>,
}

/// Turn configuration into pool widths.
///
/// `env_flush_finalize` is `XERJ_FIN_CONC`, kept as the tuning override it has
/// always been; it wins over the config value, and a value that cannot be used
/// is reported rather than dropped on the floor.
pub fn resolve(cfg: &EngineConfig, env_flush_finalize: Option<&str>) -> Resolved {
    let mut warnings = Vec::new();

    let mut flush_finalize = cfg.flush_workers.max(1);
    if let Some(raw) = env_flush_finalize.map(str::trim).filter(|s| !s.is_empty()) {
        match raw.parse::<usize>() {
            Ok(n) if n >= 1 => flush_finalize = n,
            _ => warnings.push(format!(
                "XERJ_FIN_CONC={raw} is not a positive integer; using \
                 engine.flush_workers={flush_finalize} instead"
            )),
        }
    }

    Resolved {
        sizing: PoolSizing {
            ingest: resource::threads_for(Workload::Latency),
            background: resource::threads_for(Workload::Background),
            merge: cfg.merge_workers.max(1),
            flush_finalize,
            search: cfg.search_workers.max(1),
        },
        warnings,
    }
}

static SIZING: std::sync::OnceLock<PoolSizing> = std::sync::OnceLock::new();

/// Install the process-wide pool sizing from config. Idempotent, like
/// [`crate::governor::init`]: the first caller wins, because rayon pools are
/// process-global and already-running threads cannot be resized.
///
/// A second [`Engine`](crate::Engine) built in the same process with *different*
/// widths logs a warning naming the values that were ignored — silently serving
/// one engine's config to another is exactly the bug this module exists to end.
pub fn init(cfg: &EngineConfig) {
    let resolved = resolve(cfg, std::env::var("XERJ_FIN_CONC").ok().as_deref());
    for warning in &resolved.warnings {
        tracing::warn!("{warning}");
    }
    let installed = *SIZING.get_or_init(|| resolved.sizing);
    if installed != resolved.sizing {
        tracing::warn!(
            "engine pool sizing was already fixed for this process ({installed:?}); ignoring \
             {:?} from this engine's config",
            resolved.sizing
        );
    }
    install_search_pool(installed.search);
}

/// The installed sizing, or the policy defaults when no engine has been built
/// yet (engine-only unit tests that touch a pool directly).
pub fn sizing() -> PoolSizing {
    *SIZING.get_or_init(|| resolve(&EngineConfig::default(), None).sizing)
}

/// Search fan-out runs on rayon's *global* pool, so honouring
/// `engine.search_workers` means building that pool explicitly — rayon
/// otherwise defaults it to every core on first use.
///
/// `build_global` can only succeed once per process. If some par_iter already
/// ran, the request cannot be honoured; that is reported with the width the
/// process actually has, instead of leaving the operator to guess.
fn install_search_pool(threads: usize) {
    static DONE: std::sync::Once = std::sync::Once::new();
    DONE.call_once(|| {
        if let Err(err) = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("xerj-search-{i}"))
            .spawn_handler(safe_rayon_spawn)
            .build_global()
        {
            let actual = rayon::current_num_threads();
            if actual != threads {
                tracing::warn!(
                    "engine.search_workers={threads} could not be applied ({err}); search \
                     fan-out runs on the already-initialised global pool of {actual} threads"
                );
            }
        }
    });
}

// ── Building the named pools without aborting (#1122) ────────────────────────
//
// `panic = "abort"` in the release profile turns a rayon `build()` failure
// into a dead node. `build()` fails when `pthread_create` returns `EAGAIN`
// (thread or memory limit) — the same moment the node is already degraded.
// A failed build is not cached, so the next call tries again. A wide pool
// that cannot start is retried at one thread and that success is cached.
// When even one thread cannot be started, `install` runs on the calling
// thread and `spawn` runs the job inline rather than aborting.

static POOL_BUILD_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    /// A one-thread pool made of the calling thread. Kept for the thread's
    /// lifetime so a later `par_iter` on this thread does not fall through to
    /// rayon's global pool, which panics if it cannot start its own threads.
    static INLINE_POOL: RefCell<Option<ThreadPool>> = const { RefCell::new(None) };
}

/// A named engine pool. Callers keep writing `ingest_pool().install(...)`.
/// The pool itself is started on first use, not when this handle is built.
#[derive(Clone, Copy)]
pub(crate) struct PoolCall {
    slot: &'static OnceLock<ThreadPool>,
    label: &'static str,
    threads: fn() -> usize,
    build: fn(usize) -> Result<ThreadPool, ThreadPoolBuildError>,
}

impl PoolCall {
    pub(crate) const fn new(
        slot: &'static OnceLock<ThreadPool>,
        label: &'static str,
        threads: fn() -> usize,
        build: fn(usize) -> Result<ThreadPool, ThreadPoolBuildError>,
    ) -> Self {
        Self {
            slot,
            label,
            threads,
            build,
        }
    }

    pub fn install<R: Send>(&self, op: impl FnOnce() -> R + Send) -> R {
        install_or_inline(self.slot, self.label, (self.threads)(), self.build, op)
    }

    pub fn spawn(&self, op: impl FnOnce() + Send + 'static) {
        spawn_or_inline(self.slot, self.label, (self.threads)(), self.build, op)
    }
}

pub(crate) fn install_or_inline<R: Send>(
    slot: &OnceLock<ThreadPool>,
    label: &str,
    threads: usize,
    build: fn(usize) -> Result<ThreadPool, ThreadPoolBuildError>,
    op: impl FnOnce() -> R + Send,
) -> R {
    match ensure_pool(slot, label, threads, build) {
        Some(pool) => pool.install(op),
        None => inline_install(op),
    }
}

pub(crate) fn spawn_or_inline(
    slot: &OnceLock<ThreadPool>,
    label: &str,
    threads: usize,
    build: fn(usize) -> Result<ThreadPool, ThreadPoolBuildError>,
    op: impl FnOnce() + Send + 'static,
) {
    match ensure_pool(slot, label, threads, build) {
        Some(pool) => pool.spawn(op),
        None => match xerj_common::thread_spawn::try_spawn(label, op) {
            Ok(_) => {}
            Err(failure) => {
                static LOGGED: AtomicBool = AtomicBool::new(false);
                xerj_common::thread_spawn::warn_best_effort_degraded(
                    &LOGGED,
                    label,
                    &failure.error,
                );
                if let Some(job) = failure.job {
                    job();
                }
            }
        },
    }
}

/// The pool in `slot`, built at `threads` or, if that fails, at one thread.
/// `None` when neither could start. A failure is not stored, so the next
/// call tries again.
fn ensure_pool<'a>(
    slot: &'a OnceLock<ThreadPool>,
    label: &str,
    threads: usize,
    build: fn(usize) -> Result<ThreadPool, ThreadPoolBuildError>,
) -> Option<&'a ThreadPool> {
    if let Some(pool) = slot.get() {
        return Some(pool);
    }
    let _guard = POOL_BUILD_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    if let Some(pool) = slot.get() {
        return Some(pool);
    }
    let width = threads.max(1);
    let pool = match build(width) {
        Ok(pool) => pool,
        Err(err) if width > 1 => match build(1) {
            Ok(pool) => {
                log_pool_degraded_once(|| {
                    format!(
                        "could not start the {label} pool with {width} threads ({err}); \
                         using one thread (#1122)"
                    )
                });
                pool
            }
            Err(retry) => {
                log_pool_degraded_once(|| {
                    format!(
                        "could not start the {label} pool ({err}; one-thread retry: {retry}); \
                         this call runs on the calling thread (#1122)"
                    )
                });
                return None;
            }
        },
        Err(err) => {
            log_pool_degraded_once(|| {
                format!(
                    "could not start the {label} pool ({err}); \
                     this call runs on the calling thread (#1122)"
                )
            });
            return None;
        }
    };
    Some(slot.get_or_init(|| pool))
}

/// One line per process. A pool that cannot start is retried on the next
/// call; logging every call floods the node that is already out of threads.
fn log_pool_degraded_once(message: impl FnOnce() -> String) {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if LOGGED.swap(true, Ordering::Relaxed) {
        return;
    }
    tracing::error!("{}", message());
}

/// Rayon worker spawn that cannot abort the process (#1122).
///
/// Rayon's default handler is `std::thread::Builder::spawn`. That returns
/// `Ok` before the child maps its alternate signal stack, and the mmap
/// failure panics in the child. `panic = "abort"` then kills the node
/// during pool build, so the `Err` fallback in [`ensure_pool`] never runs.
/// This handler uses the same non-panicking spawn as the other best-effort
/// threads. The watermark gate is not consulted: these pools are built once
/// and a soft RSS watermark must not refuse them the way it refuses a
/// drain-free thread. A real `pthread_create` failure is an `Err`, which
/// fails the build and lets the caller run inline.
pub(crate) fn safe_rayon_spawn(thread: rayon::ThreadBuilder) -> std::io::Result<()> {
    let name = thread.name().unwrap_or("xerj-rayon").to_owned();
    xerj_common::thread_spawn::spawn_detached(name.as_str(), thread.stack_size(), move || {
        thread.run()
    })
}

/// Run `op`, and every `par_iter` inside it, on the calling thread.
///
/// Calling `op()` directly would not do that: a `par_iter` on a thread that
/// is not a rayon worker goes to the global pool, which panics if its threads
/// cannot start. A `use_current_thread()` pool of width 1 spawns nothing.
fn inline_install<R: Send>(op: impl FnOnce() -> R + Send) -> R {
    if rayon::current_thread_index().is_some() {
        return op();
    }
    // Build, run, then keep the pool alive. Dropping it would unregister this
    // thread, and the next `par_iter` would go to the global pool.
    match ThreadPoolBuilder::new()
        .num_threads(1)
        .use_current_thread()
        .build()
    {
        Ok(pool) => {
            let out = pool.install(op);
            INLINE_POOL.with(|slot| *slot.borrow_mut() = Some(pool));
            out
        }
        Err(err) => {
            tracing::error!("could not register the calling thread with rayon ({err})");
            op()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(flush: usize, merge: usize, search: usize) -> EngineConfig {
        EngineConfig {
            flush_workers: flush,
            merge_workers: merge,
            search_workers: search,
            ..EngineConfig::default()
        }
    }

    #[test]
    fn configured_workers_reach_the_pools() {
        // The #240 §4 defect: these three were read by nothing.
        let r = resolve(&cfg(3, 5, 7), None);
        assert_eq!(r.sizing.flush_finalize, 3);
        assert_eq!(r.sizing.merge, 5);
        assert_eq!(r.sizing.search, 7);
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn defaults_reproduce_the_measured_widths() {
        let cores = xerj_common::resource::cores();
        let r = resolve(&EngineConfig::default(), None);
        // Latency-critical pools keep the whole machine ...
        assert_eq!(r.sizing.ingest, cores);
        assert_eq!(r.sizing.background, cores);
        assert_eq!(r.sizing.search, cores);
        // ... maintenance keeps the measured max(2, cores/8).
        assert_eq!(r.sizing.merge, (cores / 8).max(2).min(cores.max(1)));
        assert_eq!(r.sizing.flush_finalize, r.sizing.merge);
    }

    #[test]
    fn the_env_override_wins_and_a_bad_one_is_reported() {
        assert_eq!(resolve(&cfg(3, 2, 4), Some("9")).sizing.flush_finalize, 9);
        assert_eq!(resolve(&cfg(3, 2, 4), Some("")).sizing.flush_finalize, 3);
        let r = resolve(&cfg(3, 2, 4), Some("lots"));
        assert_eq!(r.sizing.flush_finalize, 3);
        assert!(r.warnings[0].contains("XERJ_FIN_CONC=lots"));
        let r = resolve(&cfg(3, 2, 4), Some("0"));
        assert_eq!(r.sizing.flush_finalize, 3);
        assert_eq!(r.warnings.len(), 1);
    }

    /// `pthread_create` returning `EAGAIN`. Injected through rayon's spawn
    /// hook, so `build()` takes its real failure path.
    fn failing_build(n: usize) -> Result<ThreadPool, ThreadPoolBuildError> {
        ThreadPoolBuilder::new()
            .num_threads(n)
            .spawn_handler(|_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "injected EAGAIN",
                ))
            })
            .build()
    }

    fn wide_fails_one_succeeds(n: usize) -> Result<ThreadPool, ThreadPoolBuildError> {
        if n > 1 {
            failing_build(n)
        } else {
            ThreadPoolBuilder::new().num_threads(1).build()
        }
    }

    /// #1122: `.build().expect(..)` aborted the process. A pool that cannot
    /// start must still produce the job's result, on the calling thread, and
    /// must not cache the failure (the next call tries again).
    #[test]
    fn a_pool_that_cannot_spawn_runs_install_on_the_caller() {
        let slot = OnceLock::new();
        let caller = std::thread::current().id();
        let seen = install_or_inline(&slot, "xerj-test", 4, failing_build, || {
            std::thread::current().id()
        });
        assert_eq!(seen, caller);
        assert!(slot.get().is_none(), "a failed build must not be cached");

        let sum: u32 = install_or_inline(&slot, "xerj-test", 4, failing_build, || {
            use rayon::prelude::*;
            (0..32u32).into_par_iter().sum()
        });
        assert_eq!(sum, (0..32u32).sum::<u32>());
    }

    /// The wide pool cannot start, one thread can. That one-thread pool is
    /// what later calls use — the work still runs, and the process stays up.
    #[test]
    fn a_wide_pool_that_cannot_spawn_falls_back_to_one_thread() {
        let slot = OnceLock::new();
        let width = install_or_inline(&slot, "xerj-test", 8, wide_fails_one_succeeds, || {
            rayon::current_num_threads()
        });
        assert_eq!(width, 1);
        assert_eq!(slot.get().unwrap().current_num_threads(), 1);
    }

    /// `spawn` is fire-and-forget on the live pools. When the pool cannot
    /// start, the job still runs (on a thread, or inline if that spawn fails)
    /// instead of aborting at pool construction.
    #[test]
    fn a_pool_that_cannot_spawn_still_runs_a_spawned_job() {
        let slot = OnceLock::new();
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_or_inline(&slot, "xerj-test", 4, failing_build, move || {
            tx.send(std::thread::current().id()).unwrap();
        });
        let ran = rx.recv_timeout(std::time::Duration::from_secs(2));
        assert!(ran.is_ok(), "the spawned job must run");
        assert!(slot.get().is_none());
    }

    /// #1122: production pools spawn workers through `safe_rayon_spawn`,
    /// not `std::thread::Builder`. The work has to actually run — a spawn
    /// handler that detaches wrong, or that never calls `ThreadBuilder::run`,
    /// would hang `install` or drop the job.
    #[test]
    fn the_safe_rayon_spawner_runs_pool_work() {
        let pool = ThreadPoolBuilder::new()
            .num_threads(2)
            .thread_name(|i| format!("xerj-t1122-{i}"))
            .spawn_handler(super::safe_rayon_spawn)
            .build()
            .expect("safe spawner starts a pool");
        let sum = pool.install(|| {
            use rayon::prelude::*;
            (0..64u32).into_par_iter().sum::<u32>()
        });
        assert_eq!(sum, (0..64u32).sum::<u32>());
    }
}
