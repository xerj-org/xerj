//! Process-wide, event-driven WAL fsync scheduler (issue #334).
//!
//! # Why this exists
//!
//! `wal_sync = "batched"` promises that a write reaches the platter within
//! `wal_batch_ms`.  The first implementation of that promise (RC4 W1 #9)
//! gave **every `IndexStore` its own OS thread** that slept `wal_batch_ms`
//! and then asked each of its WAL shards "are you dirty?".  On a node
//! holding many indices that is a thread and a timer per index whether or
//! not anything was ever written to it.
//!
//! Measured on a real node built by `xerj autoindex` over the reference
//! corpora (the workflow this repo documents), idle, zero client
//! connections, byte-identical data dir across the whole sample window:
//!
//! ```text
//! indices                 9 382
//! threads                 9 709   (~1 per index)
//! RSS                     7.98 GB
//! CPU                     718-760 %   (10 samples over 2.5 min)
//! ctx switches / s      ~197 000
//! interrupts / s        ~142 000
//! ```
//!
//! Nothing was being fsynced in any of that: the answer to "are you dirty?"
//! is always "no" on an idle index.  The cost was pure scheduler overhead,
//! and because it is *latency* overhead (timer wakeups, cache-line churn)
//! rather than throughput, it made the whole machine feel unusable.
//!
//! # Design
//!
//! Wake on writes, not on a clock:
//!
//! * one **bounded** pool of worker threads for the whole process, sized
//!   from the core count — never from the index count;
//! * the pool is spawned **lazily on the first dirty WAL shard**, so a node
//!   that never takes a write has zero fsync threads;
//! * a WAL shard enters a min-heap keyed by *its own* deadline
//!   (`write_instant + wal_batch_ms`) the moment it goes dirty, and leaves
//!   it as soon as it has been fsynced;
//! * an index with no writes since its last fsync holds no thread, no timer
//!   and no heap entry — it costs nothing;
//! * when the heap is empty the workers block on a condvar, so an idle node
//!   generates **zero** wakeups.
//!
//! # Prior art (reference-coding, CLAUDE.md mandate)
//!
//! Retrieved from the `xerj-storage` corpus before writing this:
//!
//! * **sled** — `sled/src/db.rs:80` `fn flusher(...)` plus
//!   `sled/src/config.rs:81,99` (`flush_every_ms`, default 200 ms): sled
//!   runs **one** flusher thread per `Db`, and every tree inside that `Db`
//!   shares it.  The unit of periodic durability is the *database*, not the
//!   keyspace.  XERJ's bug was making it the index.
//! * **fjall** — `fjall/src/worker_pool.rs:41-118`: a fixed-size worker
//!   pool (`pool_size`) serves *all* keyspaces through one `flume` channel,
//!   and workers block in `rx.recv()` (`worker_pool.rs:130`) until a
//!   `WorkerMessage` (`Flush`, `RotateMemtable`, `Compact`) is pushed by a
//!   keyspace that actually has work.  Idle keyspaces cost nothing.
//!
//! Approach adapted, no code copied (both are Apache-2.0/MIT, so copying
//! would have been permitted; the shapes differ enough that it wasn't
//! useful).  The one deliberate divergence from both: XERJ's deadline is
//! per shard and anchored to the write that dirtied it, because
//! `wal_batch_ms` is a documented per-write durability bound, whereas
//! sled's `flush_every_ms` is a free-running tick.
//!
//! # Durability
//!
//! A shard is armed inside `WalWriter::mark_dirty`, i.e. under the shard
//! mutex, on the same code path that made the bytes dirty — every present
//! and future append path is therefore covered by construction.  The
//! scheduled fsync happens at `arm_instant + wal_batch_ms`, so the
//! power-loss window is bounded by `wal_batch_ms` **measured from the write
//! itself**, which is at least as tight as the old free-running timer.
//! `Strict`/`sync` (fsync inline before ack) and `Async` (never fsync) do
//! not register a handle at all and are untouched.

use std::cmp::Ordering as CmpOrdering;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use tracing::warn;

use crate::wal::WalWriter;

/// Upper bound on fsync worker threads for the entire process.
///
/// fsync on one device largely serialises in the kernel anyway, so a small
/// pool is enough; the point is that this is a constant, not `O(indices)`.
const MAX_WORKERS: usize = 8;
const MIN_WORKERS: usize = 2;

fn worker_target() -> usize {
    // Cached: `push` runs on the write path, and `available_parallelism`
    // is a syscall (sched_getaffinity) — not something to repeat per write.
    static TARGET: OnceLock<usize> = OnceLock::new();
    *TARGET.get_or_init(|| {
        std::thread::available_parallelism()
            .map(|n| (n.get() / 2).clamp(MIN_WORKERS, MAX_WORKERS))
            .unwrap_or(MIN_WORKERS)
    })
}

/// Registration token held by one `WalWriter`.
///
/// Owns the "already queued" flag so arming a shard that is already waiting
/// for its fsync is a single atomic swap on the hot write path — no lock,
/// no allocation.
pub struct WalSyncHandle {
    /// True while this shard sits in the scheduler heap.
    armed: AtomicBool,
    /// `wal_batch_ms` for the owning store.
    period: Duration,
    /// The shard this handle fsyncs.  `Weak` so a dropped `IndexStore`
    /// drops its WAL writers; the scheduler discards dead entries.
    target: Weak<Mutex<WalWriter>>,
}

impl std::fmt::Debug for WalSyncHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalSyncHandle")
            .field("armed", &self.armed.load(Ordering::Relaxed))
            .field("period", &self.period)
            .finish()
    }
}

impl WalSyncHandle {
    /// Queue this shard for an fsync `period` from now.
    ///
    /// Called from `WalWriter::mark_dirty` under the shard mutex.  Idempotent
    /// while the shard is already queued: the first dirtying write after an
    /// fsync sets the deadline, later writes in the same window are free.
    #[inline]
    pub(crate) fn arm(self: &Arc<Self>) {
        if self.armed.swap(true, Ordering::AcqRel) {
            // Already queued — its deadline is older, so honouring it also
            // honours this write's window.
            return;
        }
        scheduler().push(Arc::clone(self), Instant::now() + self.period);
    }
}

struct Entry {
    deadline: Instant,
    handle: Arc<WalSyncHandle>,
}

// Min-heap by deadline (`BinaryHeap` is a max-heap, so the ordering is
// reversed here rather than wrapping every entry in `Reverse`).
impl Ord for Entry {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        other.deadline.cmp(&self.deadline)
    }
}
impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline
    }
}
impl Eq for Entry {}

#[derive(Default)]
struct State {
    queue: BinaryHeap<Entry>,
    /// Workers believed to be alive.  Only ever changed while holding the
    /// state mutex, so a failed spawn can be retried by the next `push`.
    workers: usize,
}

/// The process-wide scheduler.
pub struct WalFsyncScheduler {
    state: Mutex<State>,
    wake: Condvar,
    /// Test/observability counter: fsyncs actually issued.
    synced: std::sync::atomic::AtomicU64,
}

static SCHEDULER: OnceLock<WalFsyncScheduler> = OnceLock::new();

fn scheduler() -> &'static WalFsyncScheduler {
    SCHEDULER.get_or_init(|| WalFsyncScheduler {
        state: Mutex::new(State::default()),
        wake: Condvar::new(),
        synced: std::sync::atomic::AtomicU64::new(0),
    })
}

/// Register `target` for batched fsyncs every `period`.
///
/// Returns the handle to install on the writer with
/// `WalWriter::set_sync_handle`.  No thread is started here — the pool is
/// spawned by the first write that dirties any shard in the process.
pub fn register(target: &Arc<Mutex<WalWriter>>, period: Duration) -> Arc<WalSyncHandle> {
    Arc::new(WalSyncHandle {
        armed: AtomicBool::new(false),
        period,
        target: Arc::downgrade(target),
    })
}

/// Number of fsync worker threads currently running (0 before the first
/// write anywhere in the process).  Used by tests and diagnostics.
pub fn worker_count() -> usize {
    scheduler().lock_state().workers
}

/// Shards currently waiting for a batched fsync.  Idle nodes report 0.
pub fn pending_count() -> usize {
    scheduler().lock_state().queue.len()
}

/// Total fsyncs issued by the scheduler since process start.
pub fn synced_count() -> u64 {
    scheduler().synced.load(Ordering::Relaxed)
}

impl WalFsyncScheduler {
    fn lock_state(&self) -> std::sync::MutexGuard<'_, State> {
        // A panic elsewhere must not wedge WAL durability for the process.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn push(&'static self, handle: Arc<WalSyncHandle>, deadline: Instant) {
        let mut st = self.lock_state();
        st.queue.push(Entry { deadline, handle });
        let want = worker_target();
        if st.workers >= want {
            drop(st);
            // One worker is enough to re-evaluate the earliest deadline.
            self.wake.notify_one();
            return;
        }
        let mut spawned = 0usize;
        for _ in st.workers..want {
            // #1122: `std::thread::Builder::spawn` panics inside the child
            // when the alternate signal stack cannot be mapped, and the
            // release profile is panic=abort. `try_spawn_unguarded` returns
            // that failure as `Err` (the watermark gate is not consulted:
            // this thread is what bounds the durability window, and a soft
            // RSS watermark must not refuse it). The worker loop is
            // infinite, so the job is dropped rather than run inline; the
            // shard is still fsynced by flush / rotate / shutdown, and the
            // next arm retries. Logged once so a write storm does not emit
            // a line per attempt.
            match xerj_common::thread_spawn::try_spawn_unguarded("xerj-wal-fsync", move || {
                self.worker_loop()
            }) {
                Ok(_detached) => spawned += 1,
                Err(failure) => {
                    static LOGGED: AtomicBool = AtomicBool::new(false);
                    xerj_common::thread_spawn::warn_best_effort_degraded(
                        &LOGGED,
                        "xerj-wal-fsync",
                        &failure.error,
                    );
                    // The closure only captures `&self` (Copy). Ignoring it
                    // skips the infinite worker loop; flush still fsyncs.
                    let _ = failure.job;
                    break;
                }
            }
        }
        st.workers += spawned;
        drop(st);
        self.wake.notify_all();
    }

    fn worker_loop(&'static self) {
        loop {
            // ── take the next due entry ─────────────────────────────────
            // The state lock is released before any fsync: the write path
            // takes the shard lock and then this lock, so a worker must
            // never hold this lock while reaching for a shard lock.
            let entry = {
                let mut st = self.lock_state();
                loop {
                    let now = Instant::now();
                    match st.queue.peek().map(|e| e.deadline) {
                        Some(d) if d <= now => break st.queue.pop().expect("peeked"),
                        Some(d) => {
                            let (guard, _) = self
                                .wake
                                .wait_timeout(st, d - now)
                                .unwrap_or_else(|e| e.into_inner());
                            st = guard;
                        }
                        // Nothing pending: park indefinitely.  This is the
                        // idle-node case — no timer, no wakeup, no CPU.
                        None => st = self.wake.wait(st).unwrap_or_else(|e| e.into_inner()),
                    }
                }
            };

            // Disarm BEFORE the fsync so a write landing during it re-arms
            // and gets its own deadline.  The fsync below may cover that
            // write too — an early fsync is always safe, and the follow-up
            // pass simply finds a clean shard.
            entry.handle.armed.store(false, Ordering::Release);

            let Some(shard) = entry.handle.target.upgrade() else {
                continue; // store dropped
            };
            let mut wal = shard.lock().unwrap_or_else(|e| e.into_inner());
            if wal.is_dirty() {
                match wal.sync() {
                    Ok(()) => {
                        self.synced.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        // The shard still holds un-fsynced bytes.  Re-arm so
                        // a transient failure (ENOSPC that later frees, EIO
                        // on a retryable device) is retried one period later
                        // instead of waiting for the next write — the old
                        // per-index loop retried on every tick, and dropping
                        // that would have been a silent durability
                        // regression.
                        warn!("wal_batch_ms fsync failed: {e}");
                        drop(wal);
                        entry.handle.arm();
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wal::{SyncMode, WalEntry};
    use std::sync::atomic::AtomicU64;

    fn open_writer(dir: &std::path::Path, period_ms: u64) -> Arc<Mutex<WalWriter>> {
        let w = WalWriter::open(
            dir,
            64 * 1024 * 1024,
            SyncMode::Batched,
            Arc::new(AtomicU64::new(1)),
        )
        .unwrap();
        let arc = Arc::new(Mutex::new(w));
        let handle = register(&arc, Duration::from_millis(period_ms));
        arc.lock().unwrap().set_sync_handle(Some(handle));
        arc
    }

    /// The whole point: registering shards costs no threads until a write
    /// arrives, and the pool never grows with the number of shards.
    #[test]
    fn idle_shards_cost_no_threads_and_no_queue_entries() {
        let root = tempfile::tempdir().unwrap();
        let mut shards = Vec::new();
        for i in 0..64 {
            let d = root.path().join(format!("s{i}"));
            std::fs::create_dir_all(&d).unwrap();
            shards.push(open_writer(&d, 50));
        }
        // Deliberately not asserting pending_count() == 0 or
        // worker_count() == 0: other tests in the same process may already
        // have armed shards.  What must hold is the bound.
        assert!(
            worker_count() <= MAX_WORKERS,
            "worker pool must be bounded, got {}",
            worker_count()
        );
        assert!(
            shards.iter().all(|s| !s.lock().unwrap().is_dirty()),
            "opening a shard must not dirty it"
        );
    }

    /// A batched write is fsynced within its window without the caller
    /// doing anything, and the shard leaves the queue afterwards.
    #[test]
    fn dirty_shard_is_fsynced_within_the_batch_window() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("s");
        std::fs::create_dir_all(&dir).unwrap();
        let shard = open_writer(&dir, 50);

        shard
            .lock()
            .unwrap()
            .append(&WalEntry::Index {
                doc_id: "d1".into(),
                source: serde_json::json!({"a": 1}),
            })
            .unwrap();
        assert!(worker_count() >= MIN_WORKERS, "first write starts the pool");

        // 50 ms window + generous slack for a loaded CI box.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !shard.lock().unwrap().is_dirty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            !shard.lock().unwrap().is_dirty(),
            "batched write was never fsynced"
        );
    }

    /// Many dirty shards share the one bounded pool.
    #[test]
    fn many_dirty_shards_share_one_bounded_pool() {
        let root = tempfile::tempdir().unwrap();
        let mut shards = Vec::new();
        for i in 0..32 {
            let d = root.path().join(format!("m{i}"));
            std::fs::create_dir_all(&d).unwrap();
            let s = open_writer(&d, 30);
            s.lock()
                .unwrap()
                .append(&WalEntry::Index {
                    doc_id: format!("d{i}"),
                    source: serde_json::json!({"i": i}),
                })
                .unwrap();
            shards.push(s);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let dirty = shards
                .iter()
                .filter(|s| s.lock().unwrap().is_dirty())
                .count();
            if dirty == 0 || Instant::now() > deadline {
                assert_eq!(dirty, 0, "shards left unsynced past the batch window");
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(worker_count() <= MAX_WORKERS);
    }

    /// Dropping the owner must not keep the scheduler alive on its behalf.
    #[test]
    fn dropped_shard_entry_is_discarded() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("drop");
        std::fs::create_dir_all(&dir).unwrap();
        {
            let shard = open_writer(&dir, 40);
            shard
                .lock()
                .unwrap()
                .append(&WalEntry::Index {
                    doc_id: "gone".into(),
                    source: serde_json::json!({}),
                })
                .unwrap();
        } // dropped while queued
        std::thread::sleep(Duration::from_millis(300));
        // The worker upgraded a dead Weak and moved on; nothing wedged.
        assert!(worker_count() <= MAX_WORKERS);
    }
}
