//! Process-wide budget for query-time *transient* hydration (#2427, #1122).
//!
//! A query that misses the segment hydration cache still builds the whole
//! value — a segment's decompressed stored section, or every requested
//! field's decompressed postings and norms — and, when the cache budget
//! refuses it, uses that value once and throws it away. Nothing bounded
//! those transient builds: a `{prefix}-*` `multi_match` fanned out over 49
//! indexes and every cold segment in them, the RSS sampler saw the spike,
//! the shared memory breaker engaged, and the write path answered 429 for
//! memory the *read* path had allocated.
//!
//! Every such build now reserves its estimated size here first. A
//! reservation that does not fit waits (back-pressure) until earlier
//! transient values are dropped. Two rules keep that wait safe:
//!
//! * **Progress.** A request is always admitted when nothing is held, so a
//!   value larger than the whole budget is still served — alone.
//! * **No hold-and-wait.** A thread that already holds a reservation is
//!   admitted without waiting. A query that holds a postings reader while it
//!   asks for the same segment's stored bytes can therefore never deadlock
//!   against another query doing the same.
//!
//! The wait is capped (`XERJ_READ_TRANSIENT_WAIT_MS`, default 5 s). A
//! reservation that times out is still admitted and counted in `overruns`,
//! so a slow holder delays a query rather than failing it, and the counter
//! shows an operator that the budget is too small for the workload.
//!
//! This is retained-payload accounting, like the hydration budget: decode
//! scratch and allocator slack are outside it.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

const MIB: u64 = 1024 * 1024;
/// Fallback when no governor is installed (an `Index` built directly).
const DEFAULT_LIMIT_BYTES: u64 = 256 * MIB;
/// Share of the process memory limit given to transient read hydration.
const LIMIT_DIVISOR: u64 = 10;
const DEFAULT_MAX_WAIT: Duration = Duration::from_secs(5);

thread_local! {
    /// Reservations currently held by this thread (see "No hold-and-wait").
    static HELD_ON_THREAD: Cell<u32> = const { Cell::new(0) };
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, serde::Serialize)]
pub struct ReadTransientBudgetSnapshot {
    pub limit: u64,
    pub current: u64,
    pub peak: u64,
    pub admitted: u64,
    pub waited: u64,
    pub overruns: u64,
}

pub struct ReadTransientBudget {
    limit: u64,
    max_wait: Duration,
    used: Mutex<u64>,
    released: Condvar,
    peak: AtomicU64,
    admitted: AtomicU64,
    waited: AtomicU64,
    overruns: AtomicU64,
}

impl ReadTransientBudget {
    pub fn new(limit: u64, max_wait: Duration) -> Arc<Self> {
        Arc::new(Self {
            limit,
            max_wait,
            used: Mutex::new(0),
            released: Condvar::new(),
            peak: AtomicU64::new(0),
            admitted: AtomicU64::new(0),
            waited: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
        })
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// Reserve `bytes` for one transient value, waiting for room if needed.
    /// The reservation is returned when the permit is dropped.
    pub fn acquire(self: &Arc<Self>, bytes: u64) -> ReadPermit {
        let reentrant = HELD_ON_THREAD.with(|held| held.get() > 0);
        let mut used = self.used.lock().unwrap_or_else(|p| p.into_inner());
        let fits = |used: u64| used == 0 || used.saturating_add(bytes) <= self.limit;
        if !reentrant && !fits(*used) {
            self.waited.fetch_add(1, Ordering::Relaxed);
            let deadline = Instant::now() + self.max_wait;
            while !fits(*used) {
                let now = Instant::now();
                if now >= deadline {
                    self.overruns.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(
                        requested_bytes = bytes,
                        in_use_bytes = *used,
                        limit_bytes = self.limit,
                        "read transient budget: waited {:?} for room; admitting over budget \
                         (raise XERJ_READ_TRANSIENT_MB if this repeats)",
                        self.max_wait
                    );
                    break;
                }
                used = self
                    .released
                    .wait_timeout(used, deadline - now)
                    .unwrap_or_else(|p| p.into_inner())
                    .0;
            }
        }
        *used = used.saturating_add(bytes);
        self.peak.fetch_max(*used, Ordering::Relaxed);
        self.admitted.fetch_add(1, Ordering::Relaxed);
        drop(used);
        HELD_ON_THREAD.with(|held| held.set(held.get().saturating_add(1)));
        ReadPermit {
            budget: Arc::clone(self),
            bytes,
            thread: std::thread::current().id(),
        }
    }

    pub fn snapshot(&self) -> ReadTransientBudgetSnapshot {
        let current = *self.used.lock().unwrap_or_else(|p| p.into_inner());
        ReadTransientBudgetSnapshot {
            limit: self.limit,
            current,
            peak: self.peak.load(Ordering::Relaxed),
            admitted: self.admitted.load(Ordering::Relaxed),
            waited: self.waited.load(Ordering::Relaxed),
            overruns: self.overruns.load(Ordering::Relaxed),
        }
    }

    fn release(&self, bytes: u64) {
        let mut used = self.used.lock().unwrap_or_else(|p| p.into_inner());
        *used = used.saturating_sub(bytes);
        drop(used);
        self.released.notify_all();
    }
}

/// One transient value's reservation. Travels with the value (inside its
/// `CacheResident`) and is returned when the last reference is dropped.
pub struct ReadPermit {
    budget: Arc<ReadTransientBudget>,
    bytes: u64,
    thread: std::thread::ThreadId,
}

impl ReadPermit {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Correct the reservation to the value's measured size once it is
    /// built. The reservation was taken from a pre-build estimate; growing it
    /// never waits (the memory is already allocated), it only makes later
    /// reservations see the true usage. Shrinking returns the excess.
    pub fn true_up(&mut self, actual: u64) {
        if actual == self.bytes {
            return;
        }
        let mut used = self.budget.used.lock().unwrap_or_else(|p| p.into_inner());
        if actual > self.bytes {
            *used = used.saturating_add(actual - self.bytes);
            self.budget.peak.fetch_max(*used, Ordering::Relaxed);
            drop(used);
        } else {
            *used = used.saturating_sub(self.bytes - actual);
            drop(used);
            self.budget.released.notify_all();
        }
        self.bytes = actual;
    }
}

impl Drop for ReadPermit {
    fn drop(&mut self) {
        // A permit dropped on another thread leaves the acquiring thread's
        // count raised: that thread then never waits again, which fails open
        // (no deadlock) rather than closed.
        if std::thread::current().id() == self.thread {
            HELD_ON_THREAD.with(|held| held.set(held.get().saturating_sub(1)));
        }
        self.budget.release(self.bytes);
    }
}

/// Resolve the budget from `XERJ_READ_TRANSIENT_MB` (`off`/`unlimited`, or a
/// positive MiB value), else a tenth of the governor's process memory limit.
fn resolve_limit(env: Option<&str>, process_limit: Option<u64>) -> u64 {
    match env.map(str::trim) {
        Some("off") | Some("unlimited") => return u64::MAX,
        Some(value) => match value.parse::<u64>() {
            Ok(mb) if mb > 0 => return mb.saturating_mul(MIB),
            _ => tracing::warn!(
                "invalid XERJ_READ_TRANSIENT_MB={value:?}; expected off, unlimited, or a \
                 positive MiB value; using the default"
            ),
        },
        None => {}
    }
    match process_limit {
        Some(limit) if limit > 0 => (limit / LIMIT_DIVISOR).max(16 * MIB),
        _ => DEFAULT_LIMIT_BYTES,
    }
}

static GLOBAL: OnceLock<Arc<ReadTransientBudget>> = OnceLock::new();

/// The process-wide budget, created on first use.
pub fn global() -> &'static Arc<ReadTransientBudget> {
    GLOBAL.get_or_init(|| {
        let env = std::env::var("XERJ_READ_TRANSIENT_MB").ok();
        let process_limit = crate::governor::global().map(|g| g.process_memory_limit_bytes());
        let limit = resolve_limit(env.as_deref(), process_limit);
        let max_wait = std::env::var("XERJ_READ_TRANSIENT_WAIT_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_MAX_WAIT);
        ReadTransientBudget::new(limit, max_wait)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn reservations_over_the_limit_wait_for_a_release() {
        let budget = ReadTransientBudget::new(100, Duration::from_secs(10));
        let first = budget.acquire(80);
        let admitted = Arc::new(AtomicBool::new(false));
        let waiter = {
            let budget = Arc::clone(&budget);
            let admitted = Arc::clone(&admitted);
            std::thread::spawn(move || {
                let permit = budget.acquire(80);
                admitted.store(true, Ordering::SeqCst);
                drop(permit);
            })
        };
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !admitted.load(Ordering::SeqCst),
            "a second 80-byte reservation must wait while 80 of 100 bytes are held"
        );
        assert_eq!(budget.snapshot().current, 80);
        drop(first);
        waiter.join().unwrap();
        assert!(admitted.load(Ordering::SeqCst));
        let snap = budget.snapshot();
        assert_eq!(snap.current, 0);
        assert_eq!(snap.peak, 80, "the two reservations never overlapped");
        assert_eq!(snap.waited, 1);
        assert_eq!(snap.overruns, 0);
    }

    #[test]
    fn an_oversized_reservation_is_admitted_when_nothing_is_held() {
        let budget = ReadTransientBudget::new(10, Duration::from_secs(10));
        let permit = budget.acquire(1_000);
        assert_eq!(budget.snapshot().current, 1_000);
        drop(permit);
        assert_eq!(budget.snapshot().current, 0);
    }

    #[test]
    fn a_thread_that_holds_a_reservation_never_waits() {
        let budget = ReadTransientBudget::new(100, Duration::from_secs(10));
        let started = Instant::now();
        let a = budget.acquire(90);
        let b = budget.acquire(90);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(budget.snapshot().waited, 0);
        drop((a, b));
        assert_eq!(budget.snapshot().current, 0);
    }

    #[test]
    fn a_wait_past_the_cap_is_admitted_and_counted() {
        let budget = ReadTransientBudget::new(100, Duration::from_millis(50));
        let held = budget.acquire(100);
        let other = {
            let budget = Arc::clone(&budget);
            std::thread::spawn(move || drop(budget.acquire(10)))
        };
        other.join().unwrap();
        assert_eq!(budget.snapshot().overruns, 1);
        drop(held);
    }

    #[test]
    fn limit_resolution() {
        assert_eq!(resolve_limit(Some("64"), Some(1 << 40)), 64 * MIB);
        assert_eq!(resolve_limit(Some("off"), None), u64::MAX);
        assert_eq!(resolve_limit(None, Some(3072 * MIB)), 3072 * MIB / 10);
        assert_eq!(resolve_limit(Some("bogus"), None), DEFAULT_LIMIT_BYTES);
        assert_eq!(resolve_limit(None, Some(0)), DEFAULT_LIMIT_BYTES);
    }

    #[test]
    fn true_up_corrects_an_estimate_in_both_directions() {
        let budget = ReadTransientBudget::new(1_000, Duration::from_millis(50));
        let mut permit = budget.acquire(100);
        permit.true_up(400);
        assert_eq!(budget.snapshot().current, 400);
        assert_eq!(budget.snapshot().peak, 400);
        permit.true_up(50);
        assert_eq!(budget.snapshot().current, 50);
        drop(permit);
        assert_eq!(budget.snapshot().current, 0);
    }
}
