use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// How long a writer, having just dropped its bracket, will wait for a
/// search that is blocked on this publication to validate a capture before
/// the writer opens the next bracket anyway.
///
/// Per-document ingest (#1761) holds a bracket across WAL append and the
/// memtable insert, then opens the next one a few instructions later. The
/// gap is shorter than a capture that has to clone another run's buffered
/// catalog rows, so finalize-verify retried until its deadline and the phase
/// sat at 0/N. 250 ms is far longer than a columnar capture and far shorter
/// than the search deadline, so one waiting verify proceeds and ingest loses
/// at most one budget per document while that capture is in flight.
const READER_HANDOFF_BUDGET: std::time::Duration = std::time::Duration::from_millis(250);

/// Per-index seqlock-style publication barrier for collection-wide captures.
/// Writers may overlap, but a reader only accepts a capture made while no
/// writer was active and the generation remained unchanged.
///
/// After dropping a bracket, writers call [`CollectionPublication::handoff_to_readers`]
/// (or the blocking twin) so a search parked on this index can validate its
/// capture before the next bracket opens. See [`READER_HANDOFF_BUDGET`].
pub(crate) struct CollectionPublication {
    generation: AtomicU64,
    in_flight: AtomicU64,
    poisoned: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
    /// Searches currently parked in `WriterActive`.
    readers_waiting: AtomicU64,
    /// Searches admitted and not yet finished with the `search()` call that
    /// admitted them. Dropped when that call returns; the capture itself is
    /// published earlier via [`Self::note_capture_validated`].
    captures: AtomicU64,
    /// Bumped when a capture's segment snapshot and memtable snapshot have
    /// been checked against this publication and found stable.
    captures_validated: AtomicU64,
    writer_notify: tokio::sync::Notify,
    #[cfg(test)]
    reader_admission_attempts: AtomicU64,
}

pub(crate) struct CollectionPublicationGuard {
    state: Arc<CollectionPublication>,
    outcome: GuardOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CollectionReadToken {
    generation: u64,
}

impl CollectionReadToken {
    pub(crate) fn generation(self) -> u64 {
        self.generation
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadAdmission {
    Admitted(CollectionReadToken),
    WriterActive,
    Poisoned,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GuardOutcome {
    Pending,
    Committed,
    Cancelled,
}

impl CollectionPublication {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            generation: AtomicU64::new(0),
            in_flight: AtomicU64::new(0),
            poisoned: std::sync::atomic::AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
            readers_waiting: AtomicU64::new(0),
            captures: AtomicU64::new(0),
            captures_validated: AtomicU64::new(0),
            writer_notify: tokio::sync::Notify::new(),
            #[cfg(test)]
            reader_admission_attempts: AtomicU64::new(0),
        })
    }

    /// A search is about to sleep until this publication goes idle.
    pub(crate) fn arm_reader(&self) {
        self.readers_waiting.fetch_add(1, Ordering::AcqRel);
    }

    /// The wait ended (admitted, timed out, or the task dropped).
    pub(crate) fn disarm_reader(&self) {
        let previous = self.readers_waiting.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "reader wait counter underflow");
        if previous == 1 {
            self.writer_notify.notify_waiters();
        }
    }

    /// `search()` admitted a reader and owns the capture until it returns.
    pub(crate) fn begin_capture(&self) {
        self.captures.fetch_add(1, Ordering::AcqRel);
    }

    /// The admitting `search()` call is over. Wakes a writer that is handing
    /// off so a cache hit (no [`Self::note_capture_validated`]) still unblocks it.
    pub(crate) fn end_capture(&self) {
        let previous = self.captures.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "reader capture counter underflow");
        self.writer_notify.notify_waiters();
    }

    /// The memtable snapshot and the segment snapshot describe one generation
    /// and no writer is in flight. The search may scan that snapshot while
    /// later brackets open; the writer waiting in [`Self::handoff_to_readers`]
    /// can proceed.
    pub(crate) fn note_capture_validated(&self) {
        // A direct caller that never reserved a capture must not release a
        // writer who is waiting on someone else's search.
        if self.captures.load(Ordering::Acquire) == 0 {
            return;
        }
        self.captures_validated.fetch_add(1, Ordering::Release);
        self.writer_notify.notify_waiters();
    }

    /// After dropping a publication guard: if a search is waiting on this
    /// index or is inside a capture, keep the next bracket closed until that
    /// capture validates, the waiter goes away, or [`READER_HANDOFF_BUDGET`]
    /// elapses. Callers must not hold the publication guard or a memtable
    /// shard lock — the reader needs both to capture.
    pub(crate) async fn handoff_to_readers(&self) {
        if self.readers_waiting.load(Ordering::Acquire) == 0
            && self.captures.load(Ordering::Acquire) == 0
        {
            return;
        }
        let baseline = self.captures_validated.load(Ordering::Acquire);
        let deadline = tokio::time::Instant::now() + READER_HANDOFF_BUDGET;
        loop {
            let notified = self.writer_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.captures_validated.load(Ordering::Acquire) != baseline {
                return;
            }
            if self.readers_waiting.load(Ordering::Acquire) == 0
                && self.captures.load(Ordering::Acquire) == 0
            {
                return;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return;
            }
        }
    }

    /// [`Self::handoff_to_readers`] for the synchronous ingest path, which
    /// has no tokio context to await on. Polls; a validated capture is
    /// observed within a millisecond.
    pub(crate) fn handoff_to_readers_blocking(&self) {
        if self.readers_waiting.load(Ordering::Acquire) == 0
            && self.captures.load(Ordering::Acquire) == 0
        {
            return;
        }
        let baseline = self.captures_validated.load(Ordering::Acquire);
        let deadline = std::time::Instant::now() + READER_HANDOFF_BUDGET;
        while std::time::Instant::now() < deadline {
            if self.captures_validated.load(Ordering::Acquire) != baseline {
                return;
            }
            if self.readers_waiting.load(Ordering::Acquire) == 0
                && self.captures.load(Ordering::Acquire) == 0
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    pub(crate) fn begin(
        self: &Arc<Self>,
    ) -> Result<CollectionPublicationGuard, CollectionPublicationPoisoned> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(CollectionPublicationPoisoned);
        }
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        self.generation.fetch_add(1, Ordering::AcqRel);
        // A concurrent failed publisher may poison after our first check.
        // This writer has not mutated yet, so withdraw cleanly and reject it.
        if self.poisoned.load(Ordering::Acquire) {
            self.generation.fetch_add(1, Ordering::Release);
            let previous = self.in_flight.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous > 0, "collection publication counter underflow");
            if previous == 1 {
                self.notify.notify_waiters();
            }
            return Err(CollectionPublicationPoisoned);
        }
        Ok(CollectionPublicationGuard {
            state: Arc::clone(self),
            outcome: GuardOutcome::Pending,
        })
    }

    pub(crate) fn state(&self) -> (u64, u64, bool) {
        loop {
            let before = self.generation.load(Ordering::Acquire);
            let in_flight = self.in_flight.load(Ordering::Acquire);
            let after = self.generation.load(Ordering::Acquire);
            if before == after {
                return (after, in_flight, self.poisoned.load(Ordering::Acquire));
            }
        }
    }

    pub(crate) fn notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.notify.notified()
    }

    pub(crate) fn try_admit_reader(&self) -> ReadAdmission {
        #[cfg(test)]
        self.reader_admission_attempts
            .fetch_add(1, Ordering::Release);
        let (generation, in_flight, poisoned) = self.state();
        if poisoned {
            ReadAdmission::Poisoned
        } else if in_flight != 0 {
            ReadAdmission::WriterActive
        } else {
            ReadAdmission::Admitted(CollectionReadToken { generation })
        }
    }

    #[cfg(test)]
    pub(crate) fn reader_admission_attempts(&self) -> u64 {
        self.reader_admission_attempts.load(Ordering::Acquire)
    }

    pub(crate) fn validate_reader(&self, token: CollectionReadToken) -> ReadAdmission {
        let (generation, in_flight, poisoned) = self.state();
        if poisoned {
            ReadAdmission::Poisoned
        } else if in_flight != 0 || generation != token.generation {
            ReadAdmission::WriterActive
        } else {
            ReadAdmission::Admitted(token)
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CollectionPublicationPoisoned;

impl CollectionPublicationGuard {
    pub(crate) fn commit(&mut self) {
        self.outcome = GuardOutcome::Committed;
    }
    pub(crate) fn cancel(&mut self) {
        self.outcome = GuardOutcome::Cancelled;
    }
}

impl Drop for CollectionPublicationGuard {
    fn drop(&mut self) {
        if self.outcome == GuardOutcome::Pending {
            self.state.poisoned.store(true, Ordering::Release);
        }
        self.state.generation.fetch_add(1, Ordering::Release);
        let previous = self.state.in_flight.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "collection publication counter underflow");
        if previous == 1 {
            self.state.notify.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_writers_keep_capture_unstable_until_last_drop() {
        let state = CollectionPublication::new();
        let mut first = state.begin().unwrap();
        let mut second = state.begin().unwrap();
        assert_eq!(state.state().1, 2);
        first.cancel();
        drop(first);
        assert_eq!(state.state().1, 1);
        second.commit();
        drop(second);
        assert_eq!(state.state().1, 0);
        assert_eq!(state.state().0, 4);
        assert!(!state.state().2);
    }

    #[test]
    fn uncommitted_drop_poison_is_visible_after_liveness_releases() {
        let state = CollectionPublication::new();
        let result = std::panic::catch_unwind({
            let state = Arc::clone(&state);
            move || {
                let _guard = state.begin().unwrap();
                panic!("injected publication panic");
            }
        });
        assert!(result.is_err());
        assert_eq!(state.state().1, 0);
        assert!(state.state().2);
        assert!(state.begin().is_err());
    }

    #[test]
    fn reader_token_is_valid_only_for_one_stable_generation() {
        let state = CollectionPublication::new();
        let token = match state.try_admit_reader() {
            ReadAdmission::Admitted(token) => token,
            other => panic!("unexpected admission: {other:?}"),
        };
        assert_eq!(state.validate_reader(token), ReadAdmission::Admitted(token));
        let mut writer = state.begin().unwrap();
        assert_eq!(state.validate_reader(token), ReadAdmission::WriterActive);
        writer.commit();
        drop(writer);
        assert_eq!(state.validate_reader(token), ReadAdmission::WriterActive);
    }

    #[test]
    fn admitted_peer_may_commit_after_poison_but_poison_stays_sticky() {
        let state = CollectionPublication::new();
        let failed = state.begin().unwrap();
        let mut peer = state.begin().unwrap();
        drop(failed);
        assert!(state.state().2);
        peer.commit();
        drop(peer);
        assert!(state.state().2);
        assert!(state.begin().is_err());
        assert!(matches!(state.try_admit_reader(), ReadAdmission::Poisoned));
    }
}
