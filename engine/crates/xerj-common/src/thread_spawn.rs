//! Spawning helper threads without taking the node down (#1122).
//!
//! The shipped release profile is `panic = "abort"`, so every way a thread
//! spawn can panic is a whole-node outage:
//!
//! * `std::thread::spawn` is `Builder::spawn(..).expect(..)` — an `EAGAIN`
//!   from `pthread_create` (thread or memory limit) aborts the process;
//! * `Builder::spawn` returns that `Err`, but std then **panics inside the
//!   new thread** when it cannot mmap the thread's alternate signal stack
//!   under memory exhaustion — a failure the caller can never observe.
//!
//! The first is handled by returning the error, and — because
//! `Builder::spawn` consumes and drops the closure on failure — by handing
//! the closure *back* in [`SpawnFailure::job`], so the caller can run it
//! inline, retry, or drop it deliberately. The second can only be avoided by
//! not spawning while memory is exhausted: a process-wide spawn gate (the
//! engine installs its governor's RSS-watermark check through
//! [`install_spawn_gate`]) refuses the spawn up front, with the same
//! hand-back.
//!
//! The actual spawn goes through the [`Spawn`] trait so the failure paths are
//! unit-testable without exhausting a real machine.

use std::io;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

/// A process-wide "is it safe to spawn a thread right now?" predicate.
pub type SpawnGate = fn() -> bool;

static GATE: OnceLock<SpawnGate> = OnceLock::new();

/// Install the process-wide spawn gate. The first installation wins (the
/// engine's governor is process-global too); returns whether this call
/// installed it.
pub fn install_spawn_gate(gate: SpawnGate) -> bool {
    GATE.set(gate).is_ok()
}

/// Whether the installed gate currently allows spawning. With no gate
/// installed (unit tests, library embedding, the CLI) spawning is
/// unrestricted.
pub fn spawn_allowed() -> bool {
    GATE.get().map(|gate| gate()).unwrap_or(true)
}

/// The boxed body handed to a [`Spawn`] implementation.
pub type BoxedJob = Box<dyn FnOnce() + Send + 'static>;

/// How a named thread is actually started. [`StdSpawn`] in production; tests
/// inject a spawner that fails.
pub trait Spawn {
    /// Start `job` on a new thread called `name`.
    fn spawn(&self, name: &str, job: BoxedJob) -> io::Result<JoinHandle<()>>;
}

/// `std::thread::Builder` with a thread name.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdSpawn;

impl Spawn for StdSpawn {
    fn spawn(&self, name: &str, job: BoxedJob) -> io::Result<JoinHandle<()>> {
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(job)
    }
}

/// A spawn that did not happen. `job` is the closure that would have run on
/// the thread, returned to the caller so it can degrade (run it inline),
/// retry, or drop it on purpose. It is `None` only if the spawner reported a
/// failure *after* running the job, which [`StdSpawn`] never does.
pub struct SpawnFailure<F> {
    /// Why the thread was not started.
    pub error: io::Error,
    /// The un-run job, handed back.
    pub job: Option<F>,
}

impl<F> std::fmt::Debug for SpawnFailure<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnFailure")
            .field("error", &self.error)
            .field("job_returned", &self.job.is_some())
            .finish()
    }
}

impl<F> std::fmt::Display for SpawnFailure<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

/// The error a gate refusal carries.
fn gate_refused(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::OutOfMemory,
        format!(
            "not spawning thread {name}: memory is at the governor watermark, \
             and a spawn under memory exhaustion aborts the process (#1122)"
        ),
    )
}

/// Spawn a named thread, honouring the process-wide spawn gate. Never
/// panics on spawn failure; see [`SpawnFailure`].
pub fn try_spawn<F>(name: &str, f: F) -> Result<JoinHandle<()>, SpawnFailure<F>>
where
    F: FnOnce() + Send + 'static,
{
    try_spawn_with(&StdSpawn, spawn_allowed(), name, f)
}

/// [`try_spawn`] with an explicit spawner and gate verdict (for tests and
/// callers that already sampled the gate).
pub fn try_spawn_with<S, F>(
    spawner: &S,
    allowed: bool,
    name: &str,
    f: F,
) -> Result<JoinHandle<()>, SpawnFailure<F>>
where
    S: Spawn + ?Sized,
    F: FnOnce() + Send + 'static,
{
    if !allowed {
        return Err(SpawnFailure {
            error: gate_refused(name),
            job: Some(f),
        });
    }
    // The thread takes the job out of the slot when it runs. If the spawn
    // fails, the thread never ran, and the job is still in the slot even
    // though the spawner dropped the boxed wrapper.
    let slot = Arc::new(Mutex::new(Some(f)));
    let thread_slot = Arc::clone(&slot);
    let wrapper: BoxedJob = Box::new(move || {
        let job = thread_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(job) = job {
            job();
        }
    });
    match spawner.spawn(name, wrapper) {
        Ok(handle) => Ok(handle),
        Err(error) => {
            let job = slot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            Err(SpawnFailure { error, job })
        }
    }
}

/// [`try_spawn`] retried up to `attempts` times (at least once), sleeping
/// `backoff`, then twice that, … between attempts. For threads that must
/// exist (samplers, workers) where a transient `EAGAIN` or a brief trip of
/// the memory watermark should not cost the thread for the life of the
/// process. The gate is re-sampled on every attempt.
pub fn spawn_with_retry<F>(
    name: &str,
    attempts: u32,
    backoff: Duration,
    f: F,
) -> Result<JoinHandle<()>, SpawnFailure<F>>
where
    F: FnOnce() + Send + 'static,
{
    spawn_with_retry_using(&StdSpawn, spawn_allowed, name, attempts, backoff, f)
}

/// [`spawn_with_retry`] with an explicit spawner and gate.
pub fn spawn_with_retry_using<S, G, F>(
    spawner: &S,
    gate: G,
    name: &str,
    attempts: u32,
    backoff: Duration,
    f: F,
) -> Result<JoinHandle<()>, SpawnFailure<F>>
where
    S: Spawn + ?Sized,
    G: Fn() -> bool,
    F: FnOnce() + Send + 'static,
{
    let attempts = attempts.max(1);
    let mut job = f;
    let mut delay = backoff;
    let mut attempt = 1;
    loop {
        match try_spawn_with(spawner, gate(), name, job) {
            Ok(handle) => return Ok(handle),
            Err(failure) => {
                let Some(returned) = failure.job else {
                    return Err(failure);
                };
                if attempt >= attempts {
                    return Err(SpawnFailure {
                        error: failure.error,
                        job: Some(returned),
                    });
                }
                job = returned;
                std::thread::sleep(delay);
                delay = delay.saturating_mul(2);
                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

    /// Fails the first `fail_first` spawns with EAGAIN, then spawns for real.
    struct FlakySpawn {
        fail_first: u32,
        calls: AtomicU32,
    }

    impl FlakySpawn {
        fn new(fail_first: u32) -> Self {
            Self {
                fail_first,
                calls: AtomicU32::new(0),
            }
        }
    }

    impl Spawn for FlakySpawn {
        fn spawn(&self, name: &str, job: BoxedJob) -> io::Result<JoinHandle<()>> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call < self.fail_first {
                drop(job); // exactly what Builder::spawn does on failure
                return Err(io::Error::from_raw_os_error(libc::EAGAIN));
            }
            StdSpawn.spawn(name, job)
        }
    }

    #[test]
    fn a_failed_spawn_returns_the_job_instead_of_panicking() {
        let ran = Arc::new(AtomicUsize::new(0));
        let job_ran = Arc::clone(&ran);
        let failure = try_spawn_with(&FlakySpawn::new(1), true, "t1122", move || {
            job_ran.fetch_add(1, Ordering::SeqCst);
        })
        .expect_err("the spawner fails");
        assert_eq!(failure.error.raw_os_error(), Some(libc::EAGAIN));
        assert_eq!(ran.load(Ordering::SeqCst), 0, "nothing ran on a thread");
        // The caller can degrade by running the job inline.
        (failure.job.expect("job handed back"))();
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_closed_gate_refuses_without_calling_the_spawner() {
        let spawner = FlakySpawn::new(0);
        let failure = try_spawn_with(&spawner, false, "t1122", || {}).expect_err("gate closed");
        assert_eq!(failure.error.kind(), io::ErrorKind::OutOfMemory);
        assert!(failure.job.is_some());
        assert_eq!(spawner.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_successful_spawn_runs_the_job_once_on_the_named_thread() {
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = try_spawn_with(&StdSpawn, true, "xerj-t1122", move || {
            let name = std::thread::current().name().map(str::to_string);
            tx.send(name).unwrap();
        })
        .expect("spawn");
        handle.join().unwrap();
        assert_eq!(rx.recv().unwrap().as_deref(), Some("xerj-t1122"));
        assert!(rx.try_recv().is_err(), "ran exactly once");
    }

    #[test]
    fn retry_rides_out_transient_failures() {
        let spawner = FlakySpawn::new(2);
        let ran = Arc::new(AtomicUsize::new(0));
        let job_ran = Arc::clone(&ran);
        let handle = spawn_with_retry_using(
            &spawner,
            || true,
            "t1122",
            3,
            Duration::from_millis(1),
            move || {
                job_ran.fetch_add(1, Ordering::SeqCst);
            },
        )
        .expect("third attempt succeeds");
        handle.join().unwrap();
        assert_eq!(spawner.calls.load(Ordering::SeqCst), 3);
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn retry_gives_up_after_the_attempt_budget_and_hands_the_job_back() {
        let spawner = FlakySpawn::new(u32::MAX);
        let failure = spawn_with_retry_using(
            &spawner,
            || true,
            "t1122",
            3,
            Duration::from_millis(1),
            || {},
        )
        .expect_err("never succeeds");
        assert_eq!(spawner.calls.load(Ordering::SeqCst), 3);
        assert!(failure.job.is_some());
    }

    #[test]
    fn retry_resamples_the_gate_each_attempt() {
        let gate_calls = AtomicU32::new(0);
        let spawner = FlakySpawn::new(0);
        let handle = spawn_with_retry_using(
            &spawner,
            || gate_calls.fetch_add(1, Ordering::SeqCst) >= 1,
            "t1122",
            2,
            Duration::from_millis(1),
            || {},
        )
        .expect("gate opens on the second attempt");
        handle.join().unwrap();
        assert_eq!(gate_calls.load(Ordering::SeqCst), 2);
        assert_eq!(spawner.calls.load(Ordering::SeqCst), 1);
    }
}
