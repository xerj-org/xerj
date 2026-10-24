//! Spawning helper threads without taking the node down (#1122).
//!
//! The shipped release profile is `panic = "abort"`, so a panic on any thread
//! is a whole-node outage. Two different failures look the same from the
//! outside:
//!
//! * `std::thread::spawn` is `Builder::spawn(..).expect(..)`. An `EAGAIN`
//!   from `pthread_create` (thread or memory limit) aborts the process.
//!   `Builder::spawn` itself returns that `Err`.
//! * On Unix, `Builder::spawn` then **panics inside the new thread** when it
//!   cannot mmap the alternate signal stack (`failed to allocate an
//!   alternative stack: os error 12`). The parent already observed `Ok`.
//!   With `panic = "abort"` that child panic is not catchable.
//!
//! [`StdSpawn`] does not use `std::thread::Builder` on Unix. It calls
//! `pthread_create` directly and runs the job on that thread, so a stack
//! that cannot be reserved comes back as [`SpawnFailure`] and the child
//! never executes std's signal-stack setup. Windows has no alternate-stack
//! mmap; `CreateThread` failure is already an `Err` from `Builder::spawn`.
//! The global allocator aborting on its own (`memory allocation of N bytes
//! failed`) is a different failure and is not what this module handles.
//!
//! Because `Builder::spawn` / a failed `pthread_create` drops the closure,
//! the closure is handed *back* in [`SpawnFailure::job`] so the caller can
//! run it inline, retry, or drop it. Dropping it is the right degrade for
//! an infinite worker loop; running it is the right degrade for a finite
//! job such as freeing a memtable generation.
//!
//! [`try_spawn`] also consults a process-wide spawn gate (the engine
//! installs its governor's RSS-watermark check through
//! [`install_spawn_gate`]). That gate is a backstop: once the breaker is
//! already engaged, a best-effort spawn is not attempted at all. It is not
//! a substitute for the non-panicking spawn. Threads that must be tried
//! even above the watermark (the governor itself, WAL fsync) use
//! [`try_spawn_unguarded`] and still get an `Err` instead of an abort.
//!
//! The actual spawn goes through the [`Spawn`] trait so the failure paths
//! are unit-testable without exhausting a real machine.
//!
//! Scoped work cannot use [`try_spawn`]. The closure borrows the caller's
//! stack: autoindex workers, sync replay, an object-watch fetch, a mailbox
//! splitter. [`std::thread::Builder::spawn_scoped`] still returns `Ok` and
//! then panics in the child when the alternate signal stack cannot be
//! mapped. [`scope`] starts those threads with the same `pthread_create`
//! path and joins them before returning, so the borrow stays valid. A
//! failed spawn is an [`io::Error`]. The closure is dropped unrun; the
//! caller runs the work inline or records the error. This is not on the
//! per-document path: one `pthread_create` per thread, then the job the
//! caller already had.

use std::io;
use std::marker::PhantomData;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
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
    ///
    /// Failure to create the thread, including a failure while reserving its
    /// stack, is an `Err`. This must not panic: with `panic = "abort"` a
    /// panic here, or in the new thread before `job` runs, kills the process.
    fn spawn(&self, name: &str, job: BoxedJob) -> io::Result<SpawnedThread>;
}

/// `pthread_create` on Unix, `std::thread::Builder` elsewhere.
///
/// Unix does not go through `std::thread`. That path panics in the child
/// when the alternate signal stack cannot be mapped, after the parent has
/// already been told the thread exists (#1122).
#[derive(Debug, Clone, Copy, Default)]
pub struct StdSpawn;

impl Spawn for StdSpawn {
    fn spawn(&self, name: &str, job: BoxedJob) -> io::Result<SpawnedThread> {
        #[cfg(unix)]
        {
            // The OS name is applied on the new thread by the wrapper
            // `try_spawn_with` builds, before `job` runs. `std::thread`
            // would do that itself and then panic if the signal stack
            // cannot be mapped.
            let _ = name;
            spawn_pthread(job)
        }
        #[cfg(not(unix))]
        {
            spawn_std(name, job, None)
        }
    }
}

/// [`StdSpawn`] with an explicit stack reservation.
///
/// The in-process shard call needs 8 MiB: a search future overflows the
/// 2 MiB helper stack. On Unix this still uses `pthread_create`, so a
/// stack the OS will not reserve is an `Err` instead of a panic inside
/// the child after `Builder::spawn` has already returned `Ok` (#1122).
#[derive(Debug, Clone, Copy)]
pub struct StackSpawn {
    pub stack_bytes: usize,
}

impl Spawn for StackSpawn {
    fn spawn(&self, name: &str, job: BoxedJob) -> io::Result<SpawnedThread> {
        #[cfg(unix)]
        {
            let _ = name;
            spawn_pthread_with_stack(job, self.stack_bytes)
        }
        #[cfg(not(unix))]
        {
            spawn_std(name, job, Some(self.stack_bytes))
        }
    }
}

/// A thread started by [`Spawn::spawn`]. Dropping it detaches; [`join`](Self::join)
/// waits. There is no `std::thread::JoinHandle` on Unix because that type
/// only exists for threads std started, and those are the ones that panic
/// while mapping a signal stack.
pub struct SpawnedThread {
    inner: SpawnedInner,
}

impl std::fmt::Debug for SpawnedThread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SpawnedThread { .. }")
    }
}

enum SpawnedInner {
    #[cfg(unix)]
    Raw {
        thread: libc::pthread_t,
        joined: bool,
    },
    #[cfg(not(unix))]
    Std(Option<std::thread::JoinHandle<()>>),
    /// Spawn was refused or failed and the caller still needs a handle that
    /// `join`s immediately (a background loop that never started).
    Inert,
}

impl SpawnedThread {
    /// A handle for a thread that was never started. [`join`](Self::join)
    /// returns immediately.
    pub fn inert() -> Self {
        Self {
            inner: SpawnedInner::Inert,
        }
    }

    /// Wait for the thread to finish.
    ///
    /// A panic inside the job aborts the process on the release profile
    /// (`panic = "abort"`). On unwind builds the panic is caught at the
    /// `extern "C"` boundary so it cannot unwind into C; `join` then
    /// returns `Ok` and the panic hook has already printed it. Callers
    /// that detach (the normal best-effort path) do not observe it.
    pub fn join(mut self) -> std::thread::Result<()> {
        self.join_inner()
    }

    fn join_inner(&mut self) -> std::thread::Result<()> {
        match &mut self.inner {
            #[cfg(unix)]
            SpawnedInner::Raw { thread, joined } => {
                if !*joined {
                    *joined = true;
                    // SAFETY: `thread` came from a successful pthread_create
                    // in this process, and this is the only join or detach.
                    let rc = unsafe { libc::pthread_join(*thread, std::ptr::null_mut()) };
                    if rc != 0 {
                        return Err(Box::new(io::Error::from_raw_os_error(rc)));
                    }
                }
                Ok(())
            }
            #[cfg(not(unix))]
            SpawnedInner::Std(handle) => match handle.take() {
                Some(handle) => handle.join(),
                None => Ok(()),
            },
            SpawnedInner::Inert => Ok(()),
        }
    }
}

impl Drop for SpawnedThread {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let SpawnedInner::Raw { thread, joined } = &mut self.inner {
            if !*joined {
                *joined = true;
                // SAFETY: `thread` came from a successful pthread_create in
                // this process, and join has not been called. Detach is what
                // dropping a `JoinHandle` does for a std thread.
                unsafe {
                    libc::pthread_detach(*thread);
                }
            }
        }
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

/// Log one degradation for `once`. Hot paths (every memtable publish, every
/// pool call) must not emit a line per failed attempt. Returns whether this
/// call logged.
pub fn warn_best_effort_degraded(once: &AtomicBool, name: &str, error: &io::Error) -> bool {
    if once.swap(true, Ordering::Relaxed) {
        return false;
    }
    tracing::warn!(
        thread = name,
        error = %error,
        "best-effort helper thread was not started; degrading instead of aborting the process (#1122)"
    );
    true
}

/// The error a gate refusal carries.
fn gate_refused(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::OutOfMemory,
        format!(
            "not spawning thread {name}: memory is at the governor watermark, \
             so this best-effort spawn is skipped (#1122)"
        ),
    )
}

/// Spawn a named thread, honouring the process-wide spawn gate. Never
/// panics on spawn failure; see [`SpawnFailure`].
pub fn try_spawn<F>(name: &str, f: F) -> Result<SpawnedThread, SpawnFailure<F>>
where
    F: FnOnce() + Send + 'static,
{
    try_spawn_with(&StdSpawn, spawn_allowed(), name, f)
}

/// [`try_spawn`] without the watermark gate.
///
/// For threads that should still be attempted when RSS is already high
/// (the governor, a WAL fsync worker). A real OS failure is still an
/// `Err` with the job handed back — it does not abort.
pub fn try_spawn_unguarded<F>(name: &str, f: F) -> Result<SpawnedThread, SpawnFailure<F>>
where
    F: FnOnce() + Send + 'static,
{
    try_spawn_with(&StdSpawn, true, name, f)
}

/// [`try_spawn`] with an explicit stack size.
///
/// Used by the in-process shard call, whose handler does not fit on the
/// 2 MiB helper stack. A refused or failed spawn returns the job; the
/// caller must not run that job on its own stack (that is the overflow
/// the extra stack exists to avoid). Drop it and answer the client.
pub fn try_spawn_stack<F>(
    name: &str,
    stack_bytes: usize,
    f: F,
) -> Result<SpawnedThread, SpawnFailure<F>>
where
    F: FnOnce() + Send + 'static,
{
    try_spawn_with(&StackSpawn { stack_bytes }, spawn_allowed(), name, f)
}

/// A thread whose job returns a value.
///
/// [`Spawn`] jobs return `()`. Callers that need the result (a pipe
/// reader) send it through a channel. [`join`](Self::join) waits for the
/// thread and then for that value.
pub struct JoinedThread<T> {
    handle: SpawnedThread,
    rx: std::sync::mpsc::Receiver<T>,
}

impl<T> std::fmt::Debug for JoinedThread<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinedThread").finish_non_exhaustive()
    }
}

impl<T> JoinedThread<T> {
    /// Wait for the job to finish and return its value.
    ///
    /// `Err` means the thread ended without sending a value (it panicked
    /// on an unwind build, where the panic is caught at the `extern "C"`
    /// boundary). On the release profile a panic aborts the process before
    /// this returns.
    pub fn join(self) -> std::thread::Result<T> {
        self.handle.join()?;
        self.rx.recv().map_err(|_| {
            Box::new(io::Error::other("helper thread ended without a result"))
                as Box<dyn std::any::Any + Send>
        })
    }
}

/// [`try_spawn`] for a job that returns `T`.
///
/// Failure hands `f` back unrun, the same contract as [`try_spawn`]. The
/// success path is one extra channel send of `T` after the job returns.
pub fn try_spawn_returning<T, F>(name: &str, f: F) -> Result<JoinedThread<T>, SpawnFailure<F>>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    try_spawn_returning_with(&StdSpawn, spawn_allowed(), name, f)
}

/// [`try_spawn_returning`] with an explicit spawner and gate verdict.
pub fn try_spawn_returning_with<S, T, F>(
    spawner: &S,
    allowed: bool,
    name: &str,
    f: F,
) -> Result<JoinedThread<T>, SpawnFailure<F>>
where
    S: Spawn + ?Sized,
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    let slot = Arc::new(Mutex::new(Some(f)));
    let thread_slot = Arc::clone(&slot);
    match try_spawn_with(spawner, allowed, name, move || {
        let job = thread_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(job) = job {
            let _ = tx.send(job());
        }
    }) {
        Ok(handle) => Ok(JoinedThread { handle, rx }),
        Err(failure) => {
            // The wrapper did not run, so `f` is still in the slot.
            drop(failure.job);
            let job = slot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            Err(SpawnFailure {
                error: failure.error,
                job,
            })
        }
    }
}

/// Start `job` on a detached thread, for a pool worker that outlives the
/// caller (#1122).
///
/// `Ok` means the thread is running and will be detached when the returned
/// handle is dropped inside this function. `Err` means `job` did not run.
/// This does not consult the watermark gate: a pool is built once, and a
/// soft RSS watermark must not refuse its workers the way it refuses a
/// drain-free thread. `stack_bytes` is the reservation the pool asked for;
/// `None` uses the 2 MiB helper stack.
pub fn spawn_detached(
    name: &str,
    stack_bytes: Option<usize>,
    job: impl FnOnce() + Send + 'static,
) -> io::Result<()> {
    if let Some(stack_bytes) = stack_bytes {
        spawn_detached_with(&StackSpawn { stack_bytes }, name, job)
    } else {
        spawn_detached_with(&StdSpawn, name, job)
    }
}

/// [`spawn_detached`] with an explicit spawner. Tests pass one that fails
/// the way an alternate-stack `ENOMEM` does.
pub fn spawn_detached_with<S, F>(spawner: &S, name: &str, job: F) -> io::Result<()>
where
    S: Spawn + ?Sized,
    F: FnOnce() + Send + 'static,
{
    match try_spawn_with(spawner, true, name, job) {
        Ok(_detached) => Ok(()),
        Err(failure) => {
            drop(failure.job);
            Err(failure.error)
        }
    }
}

/// [`try_spawn`] with an explicit spawner and gate verdict (for tests and
/// callers that already sampled the gate).
pub fn try_spawn_with<S, F>(
    spawner: &S,
    allowed: bool,
    name: &str,
    f: F,
) -> Result<SpawnedThread, SpawnFailure<F>>
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
    #[cfg(unix)]
    let comm = os_comm_name(name);
    let wrapper: BoxedJob = Box::new(move || {
        // Before the caller's job, so a panic in the job still shows the
        // OS thread name. std would have set this and then mmap'd a signal
        // stack; that mmap is the panic #1122 hit.
        #[cfg(unix)]
        set_os_name(&comm);
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
) -> Result<SpawnedThread, SpawnFailure<F>>
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
) -> Result<SpawnedThread, SpawnFailure<F>>
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

/// A thread [`scope`] has started and not yet joined.
struct ScopedThread {
    handle: SpawnedThread,
    /// Set when a [`Scope::spawn`] job panics on an unwind build. A
    /// [`Scope::spawn_returning`] job sends its panic to the join handle
    /// instead, so a caller that turned the panic into an error is not
    /// panicked again when the scope ends.
    panic: Arc<Mutex<Option<Box<dyn std::any::Any + Send>>>>,
}

impl Drop for ScopedThread {
    fn drop(&mut self) {
        // Join, do not detach. Detach would let the job use the caller's
        // borrows after `scope` returned.
        let _ = self.handle.join_inner();
    }
}

/// Join handle for [`Scope::spawn_returning`].
///
/// The value is available once the job returns. The scope also joins the
/// operating-system thread if this handle is dropped, so a borrow inside
/// the job cannot outlive the scope. Dropping the handle discards the
/// value, including a panic payload.
pub struct ScopedJoinHandle<'scope, T> {
    rx: std::sync::mpsc::Receiver<std::thread::Result<T>>,
    _scope: PhantomData<&'scope ()>,
}

impl<T> std::fmt::Debug for ScopedJoinHandle<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopedJoinHandle").finish_non_exhaustive()
    }
}

impl<T> ScopedJoinHandle<'_, T> {
    /// Wait for the job and return its value.
    ///
    /// `Err` is a panic from the job on an unwind build. On the release
    /// profile (`panic = "abort"`) a panic aborts the process before this
    /// returns. A thread that exits without sending is also `Err`.
    pub fn join(self) -> std::thread::Result<T> {
        self.rx.recv().unwrap_or_else(|_| {
            Err(Box::new(io::Error::other(
                "scoped thread ended without a result",
            )))
        })
    }
}

/// The threads started inside one [`scope`] call.
///
/// Constructed only by [`scope`], which joins every thread before it
/// returns, including when the body panics. Jobs borrow the caller's
/// stack, so that join is what keeps the borrows valid.
pub struct Scope<'scope, 'env: 'scope> {
    threads: Mutex<Vec<ScopedThread>>,
    _scope: PhantomData<&'scope mut &'scope ()>,
    _env: PhantomData<&'env mut &'env ()>,
}

impl<'scope, 'env: 'scope> Scope<'scope, 'env> {
    /// Start `f` on a helper thread named `name`.
    ///
    /// `Err` means `f` did not run. The caller may do the work on this
    /// thread or skip it. The error is logged once per call.
    pub fn spawn<F>(&self, name: &str, f: F) -> io::Result<()>
    where
        F: FnOnce() + Send + 'scope,
    {
        self.spawn_with(&StdSpawn, spawn_allowed(), name, f)
    }

    /// [`Scope::spawn`] with an explicit spawner and gate verdict.
    ///
    /// Tests pass a spawner that fails. Production code uses [`Scope::spawn`].
    pub fn spawn_with<S, F>(&self, spawner: &S, allowed: bool, name: &str, f: F) -> io::Result<()>
    where
        S: Spawn + ?Sized,
        F: FnOnce() + Send + 'scope,
    {
        if !allowed {
            drop(f);
            let error = gate_refused(name);
            log_scoped_spawn_failure(name, &error);
            return Err(error);
        }
        let panic = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&panic);
        // SAFETY: `scope` joins this thread before it returns, or this
        // function drops the closure before returning `Err`. Either way
        // the `'scope` borrow is still alive.
        let job: Box<dyn FnOnce() + Send + 'static> = unsafe { erase_fnonce_lifetime(Box::new(f)) };
        self.start(
            spawner,
            name,
            Box::new(move || {
                if let Err(payload) = std::panic::catch_unwind(AssertUnwindSafe(job)) {
                    *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(payload);
                }
            }),
            panic,
        )
    }

    /// Start `f` and return a handle for its value.
    ///
    /// Same failure contract as [`Scope::spawn`]: on `Err`, `f` did not run.
    /// A panic on an unwind build is [`ScopedJoinHandle::join`]'s `Err`,
    /// not a panic of the scope, so the caller can record it and keep going.
    pub fn spawn_returning<T, F>(&self, name: &str, f: F) -> io::Result<ScopedJoinHandle<'scope, T>>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'scope,
    {
        self.spawn_returning_with(&StdSpawn, spawn_allowed(), name, f)
    }

    /// [`Scope::spawn_returning`] with an explicit spawner and gate verdict.
    pub fn spawn_returning_with<S, T, F>(
        &self,
        spawner: &S,
        allowed: bool,
        name: &str,
        f: F,
    ) -> io::Result<ScopedJoinHandle<'scope, T>>
    where
        S: Spawn + ?Sized,
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'scope,
    {
        if !allowed {
            drop(f);
            let error = gate_refused(name);
            log_scoped_spawn_failure(name, &error);
            return Err(error);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        // SAFETY: same as [`Scope::spawn_with`]. `T` is `'static`, so the
        // value can sit in the channel until `join` without borrowing
        // `'scope`. The closure itself is not `'static` and is only erased
        // for the thread.
        let job: Box<dyn FnOnce() -> T + Send + 'static> =
            unsafe { erase_fnonce_returning_lifetime(Box::new(f)) };
        self.start(
            spawner,
            name,
            Box::new(move || {
                let result = std::panic::catch_unwind(AssertUnwindSafe(job));
                let _ = tx.send(result);
            }),
            Arc::new(Mutex::new(None)),
        )?;
        Ok(ScopedJoinHandle {
            rx,
            _scope: PhantomData,
        })
    }

    fn start<S>(
        &self,
        spawner: &S,
        name: &str,
        job: Box<dyn FnOnce() + Send + 'static>,
        panic: Arc<Mutex<Option<Box<dyn std::any::Any + Send>>>>,
    ) -> io::Result<()>
    where
        S: Spawn + ?Sized,
    {
        match try_spawn_with(spawner, true, name, job) {
            Ok(handle) => {
                self.threads
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(ScopedThread { handle, panic });
                Ok(())
            }
            Err(failure) => {
                // The thread did not run. Dropping the job drops the
                // caller's closure while `'scope` is still alive.
                drop(failure.job);
                log_scoped_spawn_failure(name, &failure.error);
                Err(failure.error)
            }
        }
    }

    /// Join every thread started so far. A second call joins nothing.
    fn finish(&self) -> Option<Box<dyn std::any::Any + Send>> {
        let threads = {
            let mut guard = self
                .threads
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut *guard)
        };
        let mut panic = None;
        for mut thread in threads {
            // `ScopedThread`'s drop joins too. Joining here first means the
            // panic slot is filled before we read it.
            let _ = thread.handle.join_inner();
            if panic.is_none() {
                panic = thread
                    .panic
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take();
            }
        }
        panic
    }
}

/// Run `f`, then join every thread it started.
///
/// A panic in `f` is resumed after the join. A panic in a
/// [`Scope::spawn`] job is resumed after `f` returns, unless `f` itself
/// panicked. A [`Scope::spawn_returning`] panic is only visible from
/// [`ScopedJoinHandle::join`].
pub fn scope<'env, F, R>(f: F) -> R
where
    F: for<'scope> FnOnce(&'scope Scope<'scope, 'env>) -> R,
{
    let scope = Scope {
        threads: Mutex::new(Vec::new()),
        _scope: PhantomData,
        _env: PhantomData,
    };
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| f(&scope)));
    let child_panic = scope.finish();
    match result {
        Err(parent) => std::panic::resume_unwind(parent),
        Ok(value) => {
            if let Some(child) = child_panic {
                std::panic::resume_unwind(child);
            }
            value
        }
    }
}

fn log_scoped_spawn_failure(name: &str, error: &io::Error) {
    tracing::warn!(
        thread = name,
        error = %error,
        "scoped helper thread was not started; the caller degrades and the process stays up (#1122)"
    );
}

/// # Safety
///
/// The caller joins the thread that runs `job`, or drops `job`, before
/// the `'scope` borrows end.
unsafe fn erase_fnonce_lifetime<'scope>(
    job: Box<dyn FnOnce() + Send + 'scope>,
) -> Box<dyn FnOnce() + Send + 'static> {
    // SAFETY: caller contract. The fat pointer does not depend on the
    // lifetime. This is the same erasure `std::thread::scope` uses so a
    // stack borrow can cross `pthread_create`; the join is what makes it
    // sound, and that join is [`scope`]'s.
    unsafe {
        std::mem::transmute::<Box<dyn FnOnce() + Send + 'scope>, Box<dyn FnOnce() + Send + 'static>>(
            job,
        )
    }
}

/// # Safety
///
/// Same as [`erase_fnonce_lifetime`]. `T` is `'static`, so the return
/// value does not borrow `'scope`.
unsafe fn erase_fnonce_returning_lifetime<'scope, T: Send + 'static>(
    job: Box<dyn FnOnce() -> T + Send + 'scope>,
) -> Box<dyn FnOnce() -> T + Send + 'static> {
    // SAFETY: caller contract, same as [`erase_fnonce_lifetime`].
    unsafe {
        std::mem::transmute::<
            Box<dyn FnOnce() -> T + Send + 'scope>,
            Box<dyn FnOnce() -> T + Send + 'static>,
        >(job)
    }
}

/// Linux `TASK_COMM_LEN`, including the trailing NUL. `std::thread` truncates
/// to the same width for the OS name; the full name still goes in our logs.
#[cfg(unix)]
fn os_comm_name(name: &str) -> [u8; 16] {
    let mut buf = [0u8; 16];
    let bytes = if name.is_empty() {
        b"xerj".as_slice()
    } else {
        name.as_bytes()
    };
    let n = bytes.len().min(15);
    buf[..n].copy_from_slice(&bytes[..n]);
    buf
}

#[cfg(unix)]
fn set_os_name(bytes: &[u8; 16]) {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    {
        // SAFETY: `bytes` is NUL-terminated (index 15 is never written) and
        // the C string is only read for the duration of the setname call.
        let name = unsafe { std::ffi::CStr::from_ptr(bytes.as_ptr().cast()) };
        #[cfg(any(target_os = "linux", target_os = "android"))]
        unsafe {
            // SAFETY: `name` is a NUL-terminated C string. Setting the
            // calling thread's name cannot fail the spawn.
            let _ = libc::pthread_setname_np(libc::pthread_self(), name.as_ptr());
        }
        #[cfg(target_os = "macos")]
        unsafe {
            // SAFETY: same as Linux, but Darwin's setter only names the
            // calling thread and takes no `pthread_t`.
            let _ = libc::pthread_setname_np(name.as_ptr());
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = bytes;
    }
}

/// The name of the calling thread, as the spawner set it.
///
/// On Linux and macOS this is the pthread comm. Unix spawn does not go
/// through `Builder::name`, so `std::thread::current().name()` is empty
/// there (#1122). The comm is at most 15 bytes (`xerj-scan-0` fits).
/// Elsewhere this is the std thread name, which `Builder::name` did set.
pub fn current_thread_name() -> Option<String> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let mut buf = [0u8; 16];
        // SAFETY: `buf` is writable for `buf.len()` bytes. On success
        // `pthread_getname_np` writes a NUL-terminated name into it and
        // does not retain the pointer.
        let rc = unsafe {
            libc::pthread_getname_np(libc::pthread_self(), buf.as_mut_ptr().cast(), buf.len())
        };
        if rc != 0 {
            return None;
        }
        let end = buf.iter().position(|byte| *byte == 0).unwrap_or(buf.len());
        String::from_utf8(buf[..end].to_vec()).ok()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        std::thread::current().name().map(str::to_owned)
    }
}

/// Same default std uses for a spawned thread (`2 MiB` on Tier-1). A larger
/// stack would reserve more address space than the path this replaces,
/// which is the opposite of what a memory-pressure spawn should do.
#[cfg(unix)]
const HELPER_STACK_BYTES: usize = 2 * 1024 * 1024;

#[cfg(unix)]
struct AttrGuard(libc::pthread_attr_t);

#[cfg(unix)]
impl Drop for AttrGuard {
    fn drop(&mut self) {
        // SAFETY: the guard is built only after `pthread_attr_init` returned
        // 0, and nothing else destroys this attribute.
        unsafe {
            libc::pthread_attr_destroy(&mut self.0);
        }
    }
}

#[cfg(unix)]
fn spawn_pthread(job: BoxedJob) -> io::Result<SpawnedThread> {
    spawn_pthread_with_stack(job, HELPER_STACK_BYTES)
}

#[cfg(unix)]
fn spawn_pthread_with_stack(job: BoxedJob, stack_bytes: usize) -> io::Result<SpawnedThread> {
    // A second box so the fat pointer of `dyn FnOnce` survives the trip
    // through `pthread_create`'s thin `void*`. This is the same shape std
    // uses. The allocation is the caller's job box plus this one; if it
    // fails, the global allocator aborts — that is every Rust allocation,
    // not the signal-stack panic this function exists to avoid.
    let job = Box::new(job);
    let job_ptr = Box::into_raw(job);

    let mut attr = std::mem::MaybeUninit::<libc::pthread_attr_t>::uninit();
    // SAFETY: `attr` is a valid out-pointer for pthread_attr_init.
    let rc = unsafe { libc::pthread_attr_init(attr.as_mut_ptr()) };
    if rc != 0 {
        // SAFETY: the new thread was not created, so it did not take the box.
        drop(unsafe { Box::from_raw(job_ptr) });
        return Err(io::Error::from_raw_os_error(rc));
    }
    // SAFETY: pthread_attr_init returned 0, so the attribute is initialized.
    let mut attr = AttrGuard(unsafe { attr.assume_init() });
    // SAFETY: `attr` is initialized. Failure leaves the platform default
    // stack; pthread_create still returns an error if that cannot be reserved.
    unsafe {
        let _ = libc::pthread_attr_setstacksize(&mut attr.0, stack_bytes);
    }

    let mut thread = std::mem::MaybeUninit::<libc::pthread_t>::uninit();
    // SAFETY: `attr` is initialized; `thread_main` has the C ABI and frees
    // `job_ptr` exactly once. On failure the thread did not start and the
    // pointer is freed below.
    let rc = unsafe {
        libc::pthread_create(
            thread.as_mut_ptr(),
            &attr.0,
            thread_main,
            job_ptr.cast::<libc::c_void>(),
        )
    };
    drop(attr);
    if rc != 0 {
        // SAFETY: pthread_create failed, so `thread_main` will not run.
        drop(unsafe { Box::from_raw(job_ptr) });
        return Err(io::Error::from_raw_os_error(rc));
    }
    // SAFETY: pthread_create returned 0, so `thread` was written.
    let thread = unsafe { thread.assume_init() };
    Ok(SpawnedThread {
        inner: SpawnedInner::Raw {
            thread,
            joined: false,
        },
    })
}

#[cfg(unix)]
extern "C" fn thread_main(ptr: *mut libc::c_void) -> *mut libc::c_void {
    // SAFETY: the parent leaked this box into pthread_create, and this
    // thread is the unique consumer. The closure is `Send`.
    let job = unsafe { Box::from_raw(ptr.cast::<BoxedJob>()) };
    run_job(*job);
    std::ptr::null_mut()
}

#[cfg(unix)]
fn run_job(job: BoxedJob) {
    // Unwinding out of `extern "C"` is undefined. Release builds abort
    // inside the job (`panic = "abort"`) and never reach the catch.
    // Unwind builds (tests) catch so a panicked helper cannot cross the
    // C boundary; the panic hook has already printed.
    #[cfg(panic = "unwind")]
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
    #[cfg(not(panic = "unwind"))]
    job();
}

#[cfg(not(unix))]
fn spawn_std(name: &str, job: BoxedJob, stack_bytes: Option<usize>) -> io::Result<SpawnedThread> {
    // Windows (and any other non-Unix target) has no sigaltstack mmap.
    // `CreateThread` failure is an `Err` from `Builder::spawn`, which is
    // the degrade path. A global-allocator abort is out of scope here.
    let mut builder = std::thread::Builder::new().name(name.to_owned());
    if let Some(stack_bytes) = stack_bytes {
        builder = builder.stack_size(stack_bytes);
    }
    let handle = builder.spawn(job)?;
    Ok(SpawnedThread {
        inner: SpawnedInner::Std(Some(handle)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, AtomicUsize};

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
        fn spawn(&self, name: &str, job: BoxedJob) -> io::Result<SpawnedThread> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call < self.fail_first {
                drop(job); // exactly what Builder::spawn does on failure
                return Err(io::Error::from_raw_os_error(libc::EAGAIN));
            }
            StdSpawn.spawn(name, job)
        }
    }

    /// Models the failure std reports only by panicking in the child:
    /// the alternate-signal-stack mmap returns ENOMEM (os error 12).
    /// The production spawner must surface that as `Err` with the job
    /// handed back, so the caller can drop it inline and keep running.
    struct SigaltstackEnomem;

    impl Spawn for SigaltstackEnomem {
        fn spawn(&self, _name: &str, job: BoxedJob) -> io::Result<SpawnedThread> {
            drop(job);
            Err(io::Error::from_raw_os_error(12))
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
    fn a_detached_worker_spawn_returns_enomem_and_does_not_run_the_job() {
        let ran = Arc::new(AtomicUsize::new(0));
        let job_ran = Arc::clone(&ran);
        let error = spawn_detached_with(&SigaltstackEnomem, "xerj-rayon-0", move || {
            job_ran.store(1, Ordering::SeqCst);
        })
        .expect_err("setup failure is an Err, not a panic");
        assert_eq!(error.raw_os_error(), Some(12));
        assert_eq!(
            ran.load(Ordering::SeqCst),
            0,
            "a refused pool worker must not run, and must not abort"
        );
    }

    #[test]
    fn an_enomem_setup_failure_returns_the_job_instead_of_panicking() {
        let ran = Arc::new(AtomicUsize::new(0));
        let job_ran = Arc::clone(&ran);
        let failure = try_spawn_with(&SigaltstackEnomem, true, "xerj-drain-free", move || {
            job_ran.fetch_add(1, Ordering::SeqCst);
        })
        .expect_err("setup failure is an Err, not a panic");
        assert_eq!(failure.error.raw_os_error(), Some(12));
        assert_eq!(
            ran.load(Ordering::SeqCst),
            0,
            "the job did not run on a thread"
        );
        // Drain-free drops the job (the capture's destructors free the
        // memtable). Pools run it inline. Either degrade keeps the process up.
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
            tx.send(current_thread_name()).unwrap();
        })
        .expect("spawn");
        handle.join().unwrap();
        assert_eq!(rx.recv().unwrap().as_deref(), Some("xerj-t1122"));
        assert!(rx.try_recv().is_err(), "ran exactly once");
    }

    #[cfg(unix)]
    #[test]
    fn a_spawned_helper_does_not_install_an_alternate_signal_stack() {
        // #1122: std::thread::Builder's child calls
        // `stack_overflow::Handler::new`, which mmap's an alternate signal
        // stack and panics on ENOMEM. The parent has already been returned
        // `Ok`, and `panic = "abort"` turns that into SIGABRT. A helper
        // thread must not install that stack, so the failure mode cannot
        // happen. A std thread in this process does install one (the test
        // binary left SIGSEGV at SIG_DFL, so std's handler is active); this
        // assertion is false for `Builder::spawn`.
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = try_spawn_with(&StdSpawn, true, "xerj-drain-free", move || {
            tx.send(alternate_signal_stack_flags()).unwrap();
        })
        .expect("spawn");
        handle.join().unwrap();
        let (rc, flags) = rx.recv().unwrap();
        assert_eq!(rc, 0, "sigaltstack query failed");
        assert_ne!(
            flags & libc::SS_DISABLE,
            0,
            "helper installed an alternate signal stack; an ENOMEM mmap of it \
             panics inside the child and aborts the process (#1122)"
        );

        // The same process's std threads do install one. If they did not,
        // this test would pass for `Builder::spawn` too and would not be a
        // regression test for the panic.
        let (std_rc, std_flags) = std::thread::spawn(alternate_signal_stack_flags)
            .join()
            .unwrap();
        assert_eq!(std_rc, 0, "sigaltstack query failed on a std thread");
        assert_eq!(
            std_flags & libc::SS_DISABLE,
            0,
            "std::thread did not install an alternate signal stack in this \
             process, so the assertion above cannot tell the two spawners apart"
        );
    }

    /// The in-process shard call reserves 8 MiB. That reservation must not
    /// go through `std::thread::Builder`: the child's sigaltstack mmap is
    /// what aborted the node, and an 8 MiB stack is a larger mmap than the
    /// 2 MiB helper.
    #[cfg(unix)]
    #[test]
    fn an_eight_mebibyte_stack_does_not_install_an_alternate_signal_stack() {
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = try_spawn_with(
            &StackSpawn {
                stack_bytes: 8 * 1024 * 1024,
            },
            true,
            "xerj-shard",
            move || {
                tx.send(alternate_signal_stack_flags()).unwrap();
            },
        )
        .expect("8 MiB stack spawn");
        handle.join().unwrap();
        let (rc, flags) = rx.recv().unwrap();
        assert_eq!(rc, 0, "sigaltstack query failed");
        assert_ne!(
            flags & libc::SS_DISABLE,
            0,
            "shard helper installed an alternate signal stack"
        );
    }

    #[test]
    fn a_returning_spawn_yields_the_value_and_a_failure_returns_the_job() {
        let handle = try_spawn_returning("xerj-t1122-ret", || 7u32).expect("spawn");
        assert_eq!(handle.join().expect("join"), 7);

        let ran = Arc::new(AtomicUsize::new(0));
        let job_ran = Arc::clone(&ran);
        let failure = try_spawn_returning_with(&FlakySpawn::new(1), true, "xerj-pdf-stdout", {
            let job_ran = Arc::clone(&job_ran);
            move || {
                job_ran.fetch_add(1, Ordering::SeqCst);
                9u32
            }
        })
        .expect_err("spawner fails");
        assert_eq!(failure.error.raw_os_error(), Some(libc::EAGAIN));
        assert_eq!(
            ran.load(Ordering::SeqCst),
            0,
            "the job did not run on a thread"
        );
        assert_eq!((failure.job.expect("job handed back"))(), 9);
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn dropping_the_handle_detaches_and_the_job_still_runs() {
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = try_spawn_with(&StdSpawn, true, "xerj-t1122", move || {
            tx.send(()).unwrap();
        })
        .expect("spawn");
        drop(handle);
        rx.recv_timeout(Duration::from_secs(2))
            .expect("detached thread ran the job");
    }

    #[test]
    fn degradation_is_logged_once_per_flag() {
        let once = AtomicBool::new(false);
        let error = io::Error::from_raw_os_error(12);
        assert!(warn_best_effort_degraded(&once, "xerj-drain-free", &error));
        assert!(!warn_best_effort_degraded(&once, "xerj-drain-free", &error));
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

    #[test]
    fn a_scoped_job_borrows_the_caller_stack_and_finishes_before_scope_returns() {
        let value = 11u32;
        let finished = AtomicBool::new(false);
        scope(|s| {
            s.spawn("autoindex-worker", || {
                assert_eq!(value, 11, "the job sees the caller's stack");
                finished.store(true, Ordering::SeqCst);
            })
            .expect("spawn");
        });
        assert!(
            finished.load(Ordering::SeqCst),
            "scope returned before the job finished"
        );
    }

    #[test]
    fn a_failed_scoped_spawn_does_not_run_the_job() {
        let ran = AtomicUsize::new(0);
        let spawner = FlakySpawn::new(1);
        let error = scope(|s| {
            s.spawn_with(&spawner, true, "autoindex-worker", || {
                ran.fetch_add(1, Ordering::SeqCst);
            })
            .expect_err("the spawner fails")
        });
        assert_eq!(error.raw_os_error(), Some(libc::EAGAIN));
        assert_eq!(spawner.calls.load(Ordering::SeqCst), 1);
        assert_eq!(ran.load(Ordering::SeqCst), 0, "the job was dropped unrun");
        // Indexing workers and objwatch do the work on this thread instead.
        ran.fetch_add(1, Ordering::SeqCst);
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_closed_gate_does_not_call_the_scoped_spawner() {
        let spawner = FlakySpawn::new(0);
        let error = scope(|s| {
            s.spawn_with(&spawner, false, "xerj-mbox-parse", || {})
                .expect_err("gate closed")
        });
        assert_eq!(error.kind(), io::ErrorKind::OutOfMemory);
        assert_eq!(spawner.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_later_scoped_failure_does_not_cancel_a_thread_that_started() {
        let ran = Arc::new(AtomicUsize::new(0));
        let spawner = FailSecondSpawn::default();
        scope(|s| {
            let job_ran = Arc::clone(&ran);
            s.spawn_with(&spawner, true, "xerj-objwatch-fetch", move || {
                job_ran.fetch_add(1, Ordering::SeqCst);
            })
            .expect("first spawn");
            let job_ran = Arc::clone(&ran);
            let error = s
                .spawn_with(&spawner, true, "xerj-objwatch-fetch", move || {
                    job_ran.fetch_add(10, Ordering::SeqCst);
                })
                .expect_err("second spawn");
            assert_eq!(error.raw_os_error(), Some(libc::EAGAIN));
        });
        assert_eq!(ran.load(Ordering::SeqCst), 1, "only the started job ran");
    }

    #[test]
    fn a_returning_scoped_spawn_yields_the_value_and_a_failure_does_not_apply() {
        let ran = AtomicUsize::new(0);
        let value = scope(|s| {
            let handle = s
                .spawn_returning("xerj-sync-apply", || 7u32)
                .expect("spawn");
            handle.join().expect("join")
        });
        assert_eq!(value, 7);

        let error_value = scope(|s| {
            match s.spawn_returning_with(&FlakySpawn::new(1), true, "xerj-sync-apply", || {
                ran.fetch_add(1, Ordering::SeqCst);
                9u32
            }) {
                Ok(handle) => handle.join().expect("unexpected success"),
                Err(error) => {
                    assert_eq!(error.raw_os_error(), Some(libc::EAGAIN));
                    assert_eq!(ran.load(Ordering::SeqCst), 0, "apply did not run");
                    // replay_windowed applies the item on this thread instead.
                    9u32
                }
            }
        });
        assert_eq!(error_value, 9);
        assert_eq!(ran.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_returning_scoped_panic_is_the_join_error_and_scope_returns() {
        let joined = scope(|s| {
            let handle = s
                .spawn_returning("xerj-sync-apply", || -> u32 { panic!("apply-boom") })
                .expect("spawn");
            handle.join()
        });
        let payload = joined.expect_err("the panic is the join error");
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("");
        assert!(message.contains("apply-boom"), "{message}");
    }

    #[test]
    #[should_panic(expected = "scoped-boom")]
    fn a_scoped_child_panic_is_resumed_after_the_scope_joins() {
        scope(|s| {
            s.spawn("xerj-mbox-split", || panic!("scoped-boom"))
                .expect("spawn");
        });
    }

    #[cfg(unix)]
    #[test]
    fn a_scoped_helper_does_not_install_an_alternate_signal_stack() {
        // Same regression as `a_spawned_helper_does_not_install_an_alternate_signal_stack`.
        // `spawn_scoped` used to take `std::thread::Builder`, whose child
        // mmaps a signal stack after the parent has already been told `Ok`.
        let (tx, rx) = std::sync::mpsc::channel();
        scope(|s| {
            s.spawn("autoindex-worker", move || {
                tx.send(alternate_signal_stack_flags()).unwrap();
            })
            .expect("spawn");
        });
        let (rc, flags) = rx.recv().unwrap();
        assert_eq!(rc, 0, "sigaltstack query failed");
        assert_ne!(
            flags & libc::SS_DISABLE,
            0,
            "scoped helper installed an alternate signal stack; an ENOMEM \
             mmap of it panics inside the child and aborts the process (#1122)"
        );
    }

    /// Fails the second `spawn` and lets the first through.
    #[derive(Default)]
    struct FailSecondSpawn {
        calls: AtomicU32,
    }

    impl Spawn for FailSecondSpawn {
        fn spawn(&self, name: &str, job: BoxedJob) -> io::Result<SpawnedThread> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 1 {
                drop(job);
                return Err(io::Error::from_raw_os_error(libc::EAGAIN));
            }
            StdSpawn.spawn(name, job)
        }
    }

    /// `(rc, ss_flags)` from `sigaltstack`. `SS_DISABLE` set means the
    /// thread has no alternate signal stack — the mapping `std::thread`
    /// panics on if it cannot be created.
    #[cfg(unix)]
    fn alternate_signal_stack_flags() -> (libc::c_int, libc::c_int) {
        let mut stack = unsafe { std::mem::zeroed::<libc::stack_t>() };
        // SAFETY: a null current-stack pointer only reads the calling
        // thread's alternate signal stack into `stack`.
        let rc = unsafe { libc::sigaltstack(std::ptr::null(), &mut stack) };
        (rc, stack.ss_flags)
    }
}
