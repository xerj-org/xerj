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
//! A request that spawns a detached `tokio::task` loses the task-local; such a
//! task is unrestricted here and must authorize for itself. No request path in
//! the tree does that today (bulk ingest parallelises with `rayon` inside the
//! same task, which does inherit).
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
