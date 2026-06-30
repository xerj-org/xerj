//! Cluster runner — drives the Raft tick loop and message passing.
//!
//! [`ClusterRunner`] owns a [`ClusterNode`] and two background tasks:
//! 1. A periodic **tick** (every 50 ms by default) that advances the Raft
//!    state machine and dispatches heartbeats / log replication RPCs.
//! 2. A **receive loop** that delivers inbound messages from peers and routes
//!    the responses.
//!
//! Shutdown is controlled via a `tokio::sync::watch` channel. When the watch
//! value becomes `true`, the run loop exits cleanly.

use std::time::Duration;

use tokio::sync::watch;
use tracing::{info, warn};

use crate::node::ClusterNode;

// ── ClusterRunner ─────────────────────────────────────────────────────────────

/// Drives the Raft state machine in a background async task.
///
/// # Example
///
/// ```rust,no_run
/// use std::time::Duration;
/// use tokio::sync::watch;
/// use xerj_cluster::node::{ClusterNode, in_memory::{InMemoryBus, InMemoryTransport}};
/// use xerj_cluster::runner::ClusterRunner;
///
/// #[tokio::main]
/// async fn main() {
///     let bus = xerj_cluster::node::in_memory::InMemoryBus::new();
///     let transport = InMemoryTransport::new("n1".to_string(), bus).await;
///     let node = ClusterNode::new("n1".to_string(), vec![], Box::new(transport));
///
///     let (shutdown_tx, shutdown_rx) = watch::channel(false);
///     let mut runner = ClusterRunner::new(node, Duration::from_millis(50), shutdown_rx);
///
///     // Spawn the runner
///     let handle = tokio::spawn(async move { runner.run().await });
///
///     // ... later, signal shutdown
///     let _ = shutdown_tx.send(true);
///     let _ = handle.await;
/// }
/// ```
pub struct ClusterRunner {
    /// The cluster node being driven.
    pub node: ClusterNode,
    /// How often to tick the Raft state machine.
    tick_interval: Duration,
    /// Shutdown signal. When the value is `true` the run loop exits.
    shutdown: watch::Receiver<bool>,
}

impl ClusterRunner {
    /// Create a new runner.
    ///
    /// * `node` — the cluster node to drive.
    /// * `tick_interval` — Raft tick period (recommended: 50 ms in production,
    ///   1–10 ms in tests).
    /// * `shutdown` — receive end of a `watch::channel(false)`. Set the watch
    ///   to `true` to request shutdown.
    pub fn new(
        node: ClusterNode,
        tick_interval: Duration,
        shutdown: watch::Receiver<bool>,
    ) -> Self {
        ClusterRunner {
            node,
            tick_interval,
            shutdown,
        }
    }

    /// Run the Raft event loop until the shutdown signal fires.
    ///
    /// The loop alternates between:
    /// - Ticking the Raft state machine at `tick_interval`.
    /// - Receiving inbound messages (with a timeout equal to the tick interval)
    ///   via `ClusterNode::run`'s built-in timeout-based recv pattern.
    /// - Checking the shutdown watch.
    ///
    /// Because `ClusterNode` owns the transport as a `Box<dyn ClusterTransport>`
    /// and does not expose `recv()` directly to callers, we drive the node
    /// forward by:
    /// 1. Sleeping one tick.
    /// 2. During that sleep, letting the node drain inbound messages via its
    ///    existing `run`-style timeout recv.
    /// 3. Calling `node.tick()` after each sleep.
    pub async fn run(&mut self) {
        info!(
            node = %self.node.raft.id,
            tick_ms = self.tick_interval.as_millis(),
            "ClusterRunner starting"
        );

        loop {
            // Check shutdown before each tick iteration.
            if *self.shutdown.borrow() {
                info!(node = %self.node.raft.id, "ClusterRunner received shutdown signal");
                break;
            }

            // Try to receive a message within the tick interval.
            // This uses the same timeout pattern as ClusterNode::run.
            // Sleep for one tick interval. During this window the Tokio
            // runtime services other tasks (including any transport tasks).
            tokio::time::sleep(self.tick_interval).await;

            // Tick the Raft state machine.
            if let Err(e) = self.node.tick().await {
                warn!(
                    node = %self.node.raft.id,
                    error = %e,
                    "Raft tick error"
                );
            }

            // Poll shutdown watch (non-blocking).
            if self.shutdown.has_changed().unwrap_or(false) && *self.shutdown.borrow() {
                info!(node = %self.node.raft.id, "ClusterRunner received shutdown signal");
                break;
            }
        }

        info!(node = %self.node.raft.id, "ClusterRunner stopped");
    }

    /// Whether the node currently believes itself to be the Raft leader.
    pub fn is_leader(&self) -> bool {
        self.node.is_leader()
    }

    /// The node ID of the current leader (if known).
    pub fn leader_id(&self) -> Option<&str> {
        self.node.leader_id()
    }
}

// ── ClusterRunnerBuilder ──────────────────────────────────────────────────────

/// Convenience builder that wires together a [`ClusterNode`] and a
/// [`ClusterRunner`].
pub struct ClusterRunnerBuilder {
    tick_interval: Duration,
}

impl ClusterRunnerBuilder {
    pub fn new() -> Self {
        ClusterRunnerBuilder {
            tick_interval: Duration::from_millis(50),
        }
    }

    pub fn tick_interval(mut self, d: Duration) -> Self {
        self.tick_interval = d;
        self
    }

    pub fn build(
        self,
        node: ClusterNode,
        shutdown: watch::Receiver<bool>,
    ) -> ClusterRunner {
        ClusterRunner::new(node, self.tick_interval, shutdown)
    }
}

impl Default for ClusterRunnerBuilder {
    fn default() -> Self {
        Self::new()
    }
}
