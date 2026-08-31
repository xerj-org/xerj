//! Exact keyed ordering for document publication.
//!
//! A key exists in the registry only while a holder or waiter owns a strong
//! reference to its mutex. The registry keeps a `Weak`, so idle document IDs
//! do not accumulate forever.

use std::sync::{Arc, Weak};

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use tokio::sync::{Mutex, OwnedMutexGuard};

pub(crate) struct WritePublicationCoordinator {
    locks: DashMap<String, Weak<Mutex<()>>>,
    #[cfg(test)]
    before_mutex_await: parking_lot::RwLock<Option<Arc<dyn Fn(&str) + Send + Sync + 'static>>>,
}

impl Default for WritePublicationCoordinator {
    fn default() -> Self {
        Self {
            // #873 — one coordinator per index; dashmap's core-count-derived
            // default shard array is resident from construction, so it is
            // capped like every other per-index map.
            locks: DashMap::with_shard_amount(xerj_common::resource::per_index_map_shards()),
            #[cfg(test)]
            before_mutex_await: parking_lot::RwLock::new(None),
        }
    }
}

pub(crate) struct WritePublicationGuard {
    coordinator: Arc<WritePublicationCoordinator>,
    key: String,
    lock: Arc<Mutex<()>>,
    guard: Option<OwnedMutexGuard<()>>,
}

/// Owns the registry's strong reference while `lock_owned()` is pending.
///
/// This is deliberately separate from [`WritePublicationGuard`]: cancelling
/// the acquire future never constructs a guard, so cleanup must live in a
/// value that already exists before the `.await`.
struct PendingLock {
    coordinator: Arc<WritePublicationCoordinator>,
    key: String,
    lock: Option<Arc<Mutex<()>>>,
}

impl WritePublicationCoordinator {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) async fn lock(self: &Arc<Self>, key: impl Into<String>) -> WritePublicationGuard {
        self.lock_inner(key.into(), None).await
    }

    async fn lock_inner(
        self: &Arc<Self>,
        key: String,
        before_await: Option<Box<dyn FnOnce() + Send>>,
    ) -> WritePublicationGuard {
        let lock = match self.locks.entry(key.clone()) {
            Entry::Occupied(mut occupied) => match occupied.get().upgrade() {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(Mutex::new(()));
                    occupied.insert(Arc::downgrade(&lock));
                    lock
                }
            },
            Entry::Vacant(vacant) => {
                let lock = Arc::new(Mutex::new(()));
                vacant.insert(Arc::downgrade(&lock));
                lock
            }
        };
        // Runs after the registry entry guard is gone and immediately before
        // queueing. Tests use this handshake to cancel a known waiter without
        // sleeps or scheduler guesses.
        if let Some(before_await) = before_await {
            before_await();
        }
        #[cfg(test)]
        {
            let hook = self.before_mutex_await.read().clone();
            if let Some(hook) = hook {
                hook(&key);
            }
        }
        let mut pending = PendingLock {
            coordinator: Arc::clone(self),
            key: key.clone(),
            lock: Some(lock),
        };
        let guard = Arc::clone(pending.lock.as_ref().expect("pending lock present"))
            .lock_owned()
            .await;
        let lock = pending.lock.take().expect("pending lock present");
        WritePublicationGuard {
            coordinator: Arc::clone(self),
            key,
            lock,
            guard: Some(guard),
        }
    }

    #[cfg(test)]
    async fn lock_with_before_await(
        self: &Arc<Self>,
        key: impl Into<String>,
        before_await: impl FnOnce() + Send + 'static,
    ) -> WritePublicationGuard {
        self.lock_inner(key.into(), Some(Box::new(before_await)))
            .await
    }

    /// Acquire a deadlock-free set of exact document keys.
    ///
    /// Sorting gives every caller one global acquisition order; deduplication
    /// prevents a request from waiting on its own non-reentrant key.
    #[allow(dead_code)]
    pub(crate) async fn lock_many<I, S>(self: &Arc<Self>, keys: I) -> Vec<WritePublicationGuard>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut keys: Vec<String> = keys.into_iter().map(Into::into).collect();
        keys.sort_unstable();
        keys.dedup();

        let mut guards = Vec::with_capacity(keys.len());
        for key in keys {
            guards.push(self.lock(key).await);
        }
        guards
    }

    #[cfg(test)]
    fn registry_len(&self) -> usize {
        self.locks.len()
    }

    #[cfg(test)]
    pub(crate) fn set_before_mutex_await_hook(
        &self,
        hook: Option<Arc<dyn Fn(&str) + Send + Sync + 'static>>,
    ) {
        *self.before_mutex_await.write() = hook;
    }
}

fn remove_if_idle(coordinator: &WritePublicationCoordinator, key: &str, lock: &Arc<Mutex<()>>) {
    if let Entry::Occupied(occupied) = coordinator.locks.entry(key.to_string()) {
        let is_same = occupied
            .get()
            .upgrade()
            .map(|registered| Arc::ptr_eq(&registered, lock))
            .unwrap_or(false);
        if is_same && Arc::strong_count(lock) == 1 {
            occupied.remove();
        }
    }
}

impl Drop for PendingLock {
    fn drop(&mut self) {
        if let Some(lock) = self.lock.as_ref() {
            remove_if_idle(&self.coordinator, &self.key, lock);
        }
    }
}

impl Drop for WritePublicationGuard {
    fn drop(&mut self) {
        // Release the mutex before attempting registry cleanup. Any queued
        // waiter already owns another strong Arc and therefore prevents
        // removal below.
        self.guard.take();

        remove_if_idle(&self.coordinator, &self.key, &self.lock);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn same_key_is_strictly_ordered_and_cleans_up() {
        let coordinator = WritePublicationCoordinator::new();
        let first = coordinator.lock("same").await;
        let (attempt_tx, attempt_rx) = tokio::sync::oneshot::channel();
        let (acquired_tx, mut acquired_rx) = tokio::sync::oneshot::channel();

        let task = {
            let coordinator = Arc::clone(&coordinator);
            tokio::spawn(async move {
                attempt_tx.send(()).unwrap();
                let _second = coordinator.lock("same").await;
                acquired_tx.send(()).unwrap();
            })
        };
        attempt_rx.await.unwrap();
        assert!(
            acquired_rx.try_recv().is_err(),
            "same key entered before the first guard was released"
        );
        assert_eq!(coordinator.registry_len(), 1);

        drop(first);
        acquired_rx.await.unwrap();
        task.await.unwrap();
        assert_eq!(coordinator.registry_len(), 0);
    }

    #[tokio::test]
    async fn disjoint_keys_do_not_block_each_other() {
        let coordinator = WritePublicationCoordinator::new();
        let _a = coordinator.lock("a").await;
        tokio::time::timeout(Duration::from_millis(100), coordinator.lock("b"))
            .await
            .expect("unrelated key must not wait");
    }

    #[tokio::test]
    async fn lock_many_sorts_deduplicates_and_cleans_up() {
        let coordinator = WritePublicationCoordinator::new();
        let guards = coordinator.lock_many(["c", "a", "b", "a"]).await;
        assert_eq!(guards.len(), 3);
        assert_eq!(coordinator.registry_len(), 3);
        drop(guards);
        assert_eq!(coordinator.registry_len(), 0);
    }

    #[tokio::test]
    async fn registry_is_bounded_by_live_keys_not_historical_keys() {
        let coordinator = WritePublicationCoordinator::new();
        for i in 0..10_000 {
            let guard = coordinator.lock(format!("historical-{i}")).await;
            drop(guard);
        }
        assert_eq!(
            coordinator.registry_len(),
            0,
            "idle document IDs must not remain in the registry"
        );
    }

    #[tokio::test]
    async fn cancellation_and_failure_style_early_return_release_key() {
        async fn fail_after_lock(
            coordinator: Arc<WritePublicationCoordinator>,
        ) -> Result<(), &'static str> {
            let _guard = coordinator.lock("wal-failure").await;
            Err("injected WAL failure")
        }

        let coordinator = WritePublicationCoordinator::new();
        assert_eq!(
            fail_after_lock(Arc::clone(&coordinator)).await,
            Err("injected WAL failure")
        );
        assert_eq!(coordinator.registry_len(), 0);
        tokio::time::timeout(Duration::from_millis(100), coordinator.lock("wal-failure"))
            .await
            .expect("key leaked after early error");
    }

    #[tokio::test]
    async fn cancelling_a_queued_waiter_does_not_leak_or_wedge_the_key() {
        let coordinator = WritePublicationCoordinator::new();
        let first = coordinator.lock("cancelled").await;
        let (attempt_tx, attempt_rx) = tokio::sync::oneshot::channel();
        let waiter = {
            let coordinator = Arc::clone(&coordinator);
            tokio::spawn(async move {
                let _guard = coordinator
                    .lock_with_before_await("cancelled", move || {
                        attempt_tx.send(()).unwrap();
                    })
                    .await;
            })
        };
        attempt_rx.await.unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        drop(first);

        assert_eq!(coordinator.registry_len(), 0);
        let guard = coordinator.lock("cancelled").await;
        drop(guard);
    }

    #[tokio::test]
    async fn reversed_multi_key_requests_complete_without_deadlock() {
        let coordinator = WritePublicationCoordinator::new();
        let start = Arc::new(tokio::sync::Barrier::new(3));
        let left = {
            let coordinator = Arc::clone(&coordinator);
            let start = Arc::clone(&start);
            tokio::spawn(async move {
                start.wait().await;
                let guards = coordinator.lock_many(["b", "a"]).await;
                tokio::task::yield_now().await;
                drop(guards);
            })
        };
        let right = {
            let coordinator = Arc::clone(&coordinator);
            let start = Arc::clone(&start);
            tokio::spawn(async move {
                start.wait().await;
                let guards = coordinator.lock_many(["a", "b"]).await;
                tokio::task::yield_now().await;
                drop(guards);
            })
        };
        start.wait().await;

        tokio::time::timeout(Duration::from_secs(1), left)
            .await
            .expect("sorted acquisition deadlocked")
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), right)
            .await
            .expect("second sorted acquisition stayed wedged")
            .unwrap();
        assert_eq!(coordinator.registry_len(), 0);
    }
}
