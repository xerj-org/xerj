//! Request-scoped index **visibility** — the engine-side half of the per-index
//! authorization boundary (issue #79).
//!
//! ## Why this lives in the engine and not only in the API middleware
//!
//! The first cut of per-brain authorization decided against the index named in
//! the **URL path**. Several handlers take the index they actually operate on
//! from the **request body** — `_msearch` header lines, `_bulk` action lines,
//! `_mget` `docs[]._index`, `_aliases` actions, `_reindex` `source`/`dest`, a
//! `terms` lookup buried in a query, the table name inside `_sql` — so a
//! caller could name one index in the path, be authorized against it, and have
//! the handler touch a different one. Four of those were proven live.
//!
//! Patching each handler is how that class of bug comes back: the next handler
//! that resolves a name from a body is written by someone who does not know the
//! list. So the check is placed where **every** path converges instead — the
//! two engine functions that turn a name into an [`crate::index::Index`]
//! ([`crate::Engine::get_index`] and [`crate::Engine::get_or_create_index`]),
//! the two that create and destroy one, and the two that enumerate them.
//! Nothing in the process reaches index data without going through one of
//! those, so a handler cannot forget this check — it does not call it.
//!
//! ## Shape
//!
//! The API layer installs an [`IndexVisibility`] for the duration of a request
//! (a `tokio` task-local, set by `xerj_api::authz::authz_middleware` around the
//! inner service call, so every future awaited while handling that request sees
//! it). Code running **outside** a request — startup index discovery, the
//! background flush/merge timers, WAL replay, snapshot restore — has no
//! task-local set and is unrestricted, which is what
//! [`visible`]'s `unwrap_or(true)` means: absent guard = engine-internal work,
//! not "permission granted to a caller".
//!
//! ## Detached work loses the guard, and that was a real hole
//!
//! A `tokio` task-local lives on the task, so anything a request *detaches*
//! from itself — `tokio::spawn`, `spawn_blocking`, a `rayon` closure that
//! lands on a pool worker — runs with no guard installed and is therefore
//! unrestricted.
//!
//! The first cut of this module recorded that shape as a residual risk and
//! claimed "no request path in the tree does that today". That was wrong. The
//! ML datafeed does: `POST /_ml/datafeeds/{id}/_start` spawned a detached
//! scorer that re-read its source index every `frequency` seconds, so a
//! principal with no access to `.xerj-memory-bob-edges` got an empty result
//! set from the synchronous start pass (correctly denied, inside the request's
//! scope) and the brain's field values a few seconds later, from the tick.
//!
//! So detaching now has an explicit contract instead of a footnote:
//!
//! - [`current`] captures the guard the request is running under.
//! - [`scoped`] re-installs it inside the spawned future.
//!
//! Any future `tokio::spawn` that can reach [`crate::Engine::get_index`] must
//! carry the rule across with those two, and any that cannot must say why in a
//! comment at the spawn site. `spawn_datafeed_task` in `xerj-api` is the
//! worked example.
//!
//! ### The audit
//!
//! Every non-test `tokio::spawn` / `spawn_blocking` in the workspace, and why
//! it is or is not a door. Only the first row was a hole.
//!
//! | Site | Verdict |
//! |---|---|
//! | `xerj-api::es_compat::spawn_datafeed_task` | **carries the rule** — the one detached path that resolves an index name |
//! | `es_compat::search_impl`'s search task | owns one already-authorized `Index`; `terms`/`lookup` targets are resolved earlier, on the request's task |
//! | `es_compat::delete_by_query` / `update_by_query` (`wait_for_completion=false`) | same: one already-authorized `Index` handle, no `Engine` |
//! | `Engine::spawn_pit_sweeper`, `spawn_search_context_sweeper` | started by `Engine::new`, capture only the PIT/scroll maps, touch no index |
//! | `Index`'s flush/merge/warm tasks (`index.rs`, `write_publication.rs`) | methods **on** an already-resolved `Index`; the name has already been through the funnel |
//! | `xerj-server::main`'s listeners, metrics loop, autoindex/brain CLIs | startup, outside any request; unrestricted by design (see `visible`) |
//! | `xerj-cluster` transport/replication/coordinator | peer-to-peer, authorized by `xerj_cluster::auth`, not by a caller's principal |
//! | `xerj-ai::embedder`, `xerj-storage` cache/backend/merge | no `Engine`, no index names |
//! | `xerj-api::binary_protocol::serve` | not wired to any listener in `xerj-server`; module is unreachable at runtime |
//! | `xerj-server::grpc` | its spawns are `#[cfg(test)]`; the live listener authorizes per call with `Principal::allows_index` |
//!
//! `rayon` does **not** inherit the task-local — the first cut claimed it did.
//! `visible` reads a thread-local that `tokio` maintains only while it polls
//! the owning task, and a rayon worker is an ordinary OS thread that `tokio`
//! never touches, so a closure that lands on one sees an absent guard.
//! `absent_guard_on_a_rayon_worker` below pins that. It is not a live hole
//! because no `rayon` closure in the tree resolves an index name: the bulk
//! ingest fan-out (`bulk::process_bulk_with_opts`) parallelises NDJSON
//! *parsing* only, and every `get_or_create_index` in that function runs
//! sequentially on the request's own task, after the parse collects.
//!
//! ## Denial shape: "not found", not "forbidden"
//!
//! A denied name reports the ordinary
//! [`xerj_common::XerjError::index_not_found`], for two reasons:
//!
//! 1. **No enumeration.** A caller cannot tell a brain it may not read from a
//!    brain that does not exist, so it cannot map the node by probing.
//! 2. **Fan-out keeps working.** `POST /_search`, `_cat/indices`, `_mapping`,
//!    a `logs-*` wildcard — all of them enumerate and skip what is missing.
//!    Filtering the enumeration ([`crate::Engine::list_indices`],
//!    [`crate::Engine::index_name_list`]) plus a not-found on the rest gives
//!    "you see exactly your own indices" for free, instead of the blanket 403
//!    that made global verbs unusable.
//!
//! The precise privilege (read vs write vs manage) and the ES-shaped 403 are
//! still the API middleware's job. This is the backstop that makes
//! *forgetting* impossible, not a replacement for the front-line decision.

use std::sync::Arc;

/// Decides whether the principal behind the current request may see an index
/// at all. Implemented by `xerj_api::authz` over the request's `Principal`.
pub trait IndexVisibility: Send + Sync {
    /// May the current principal touch the concrete index `index`?
    ///
    /// `index` is always a real index name — aliases are resolved before this
    /// is consulted, so an alias cannot launder access to its target.
    fn visible(&self, index: &str) -> bool;
}

tokio::task_local! {
    static VISIBILITY: Arc<dyn IndexVisibility>;
}

/// Is `index` visible to the principal behind the current request?
///
/// `true` when no guard is installed — see the module docs: that means the
/// caller is the engine itself, not an unauthenticated request.
pub fn visible(index: &str) -> bool {
    VISIBILITY.try_with(|v| v.visible(index)).unwrap_or(true)
}

/// Run `fut` with `guard` installed as the current request's visibility rule.
pub async fn scoped<F>(guard: Arc<dyn IndexVisibility>, fut: F) -> F::Output
where
    F: std::future::Future,
{
    VISIBILITY.scope(guard, fut).await
}

/// The guard the caller is currently running under, if any.
///
/// The half of the detached-work contract that runs *before* the spawn: call
/// it on the request's own task, move the result into the spawned future, and
/// hand it back to [`scoped`] there. `None` means the caller is already
/// unrestricted (engine-internal work, or a superuser request, for which the
/// API layer installs no guard at all), and the spawned task inherits exactly
/// that — an unrestricted caller does not become restricted by detaching, and
/// a restricted one does not become unrestricted.
pub fn current() -> Option<Arc<dyn IndexVisibility>> {
    VISIBILITY.try_with(Arc::clone).ok()
}

/// Run `fut` under `guard` when there is one, unrestricted when there is not.
///
/// The spawn-site half of the contract, so a caller does not have to spell the
/// `match` out (and cannot get it backwards):
///
/// ```ignore
/// let rule = index_guard::current();          // on the request's task
/// tokio::spawn(async move {
///     index_guard::scoped_opt(rule, async move { /* … */ }).await
/// });
/// ```
pub async fn scoped_opt<F>(guard: Option<Arc<dyn IndexVisibility>>, fut: F) -> F::Output
where
    F: std::future::Future,
{
    match guard {
        Some(g) => scoped(g, fut).await,
        None => fut.await,
    }
}

/// Is a guard installed at all — i.e. is this a request rather than
/// engine-internal work? Distinguishes the two cases that [`visible`]
/// deliberately collapses to `true`; nothing in the decision path needs it,
/// but a caller reasoning about which side of the boundary it is on does.
pub fn guarded() -> bool {
    VISIBILITY.try_with(|_| true).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Only(&'static str);
    impl IndexVisibility for Only {
        fn visible(&self, index: &str) -> bool {
            index == self.0
        }
    }

    #[tokio::test]
    async fn absent_guard_is_unrestricted() {
        assert!(visible("anything"));
        assert!(!guarded());
    }

    /// The contract detached work runs under: capture on the request's task,
    /// re-install inside the spawned one. Without the capture the spawned task
    /// is unrestricted, which is the ML-datafeed hole.
    #[tokio::test]
    async fn a_captured_guard_survives_tokio_spawn() {
        let (uncarried, carried) = scoped(Arc::new(Only("mine")), async {
            // Detaching without carrying the rule loses it — this is the shape
            // the bug had.
            let uncarried = tokio::spawn(async { (guarded(), visible("yours")) })
                .await
                .expect("join");
            // Carrying it across keeps the denial.
            let rule = current();
            let carried = tokio::spawn(async move {
                scoped_opt(rule, async {
                    (guarded(), visible("yours"), visible("mine"))
                })
                .await
            })
            .await
            .expect("join");
            (uncarried, carried)
        })
        .await;
        assert_eq!(uncarried, (false, true), "a bare spawn loses the guard");
        assert_eq!(
            carried,
            (true, false, true),
            "a carried guard denies in the spawned task exactly as it did in the request"
        );
    }

    /// Outside a request there is nothing to carry, and carrying `None` must
    /// not invent a restriction.
    #[tokio::test]
    async fn carrying_an_absent_guard_stays_unrestricted() {
        assert!(current().is_none());
        let seen = tokio::spawn(scoped_opt(None, async { (guarded(), visible("anything")) }))
            .await
            .expect("join");
        assert_eq!(seen, (false, true));
    }

    /// The first cut of this module claimed `rayon` inherits the task-local.
    /// It does not: a rayon worker is an OS thread `tokio` never polls a task
    /// on, so the thread-local backing `VISIBILITY` is simply unset there.
    ///
    /// `ThreadPool::spawn` (rather than `install`/`join`/`par_iter`) is used
    /// deliberately — it *always* runs the closure on a pool worker, never on
    /// the calling thread, so this cannot pass by accidentally staying home.
    #[tokio::test]
    async fn absent_guard_on_a_rayon_worker() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("rayon pool");
        let (tx, rx) = std::sync::mpsc::channel();
        scoped(Arc::new(Only("mine")), async {
            assert!(!visible("yours"), "denied on the request's own thread");
            pool.spawn(move || {
                let _ = tx.send((guarded(), visible("yours")));
            });
        })
        .await;
        assert_eq!(
            rx.recv().expect("rayon result"),
            (false, true),
            "a rayon worker sees no guard — so no rayon closure may resolve an index name"
        );
    }

    #[tokio::test]
    async fn guard_applies_inside_the_scope_only() {
        scoped(Arc::new(Only("mine")), async {
            assert!(guarded());
            assert!(visible("mine"));
            assert!(!visible("yours"));
            // A nested await keeps the guard.
            tokio::task::yield_now().await;
            assert!(!visible("yours"));
        })
        .await;
        // Outside the scope it is gone again.
        assert!(visible("yours"));
    }
}
