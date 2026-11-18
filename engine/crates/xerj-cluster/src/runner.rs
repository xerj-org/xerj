//! Cluster runner — drives the Raft consensus loop and message passing.
//!
//! [`ClusterRunner`] owns a [`ClusterNode`] and runs a single async loop that,
//! each iteration, drives one [`ClusterNode::step`]: it drains an inbound
//! message from a peer (bounded by the tick interval), delivers it to the Raft
//! state machine, and then ticks — advancing elections and dispatching
//! heartbeat / log-replication RPCs.
//!
//! Shutdown is controlled via a `tokio::sync::watch` channel. When the watch
//! value becomes `true` the run loop exits cleanly; the shutdown wait is raced
//! against `step` via `tokio::select!` so a shutdown is observed promptly
//! rather than only between ticks.

use std::time::Duration;

use tokio::sync::watch;
use tracing::{info, warn};

use crate::node::ClusterNode;

/// How often a member asks primaries to fill replica shards it is missing.
///
/// The first pass runs as soon as the runner starts. Later passes wait this
/// long, or less when shutdown is signalled. A pass that finds nothing out
/// of sync does not call a peer.
const REPLICA_BACKFILL_INTERVAL: Duration = Duration::from_millis(200);

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
    /// Each iteration races two futures with `tokio::select!`:
    /// - [`ClusterNode::step`] — drain one inbound peer message (bounded by
    ///   `tick_interval`), deliver it to the Raft state machine, then tick.
    /// - `shutdown.changed()` — resolves when the shutdown watch is set (or the
    ///   sender is dropped), at which point the loop exits.
    ///
    /// This actually *handles* inbound messages, so leader election and log
    /// replication make progress. (The previous implementation only slept and
    /// ticked, never calling `recv()`, so a multi-node cluster could never
    /// elect a leader.)
    pub async fn run(&mut self) {
        info!(
            node = %self.node.raft.id,
            tick_ms = self.tick_interval.as_millis(),
            "ClusterRunner starting"
        );

        let tick = self.tick_interval;
        let backfill = self.spawn_replica_backfill();
        loop {
            // Fast path: already asked to shut down before we start a step.
            if *self.shutdown.borrow() {
                info!(node = %self.node.raft.id, "ClusterRunner received shutdown signal");
                break;
            }

            tokio::select! {
                // Shutdown requested (value changed) or sender dropped (Err).
                changed = self.shutdown.changed() => {
                    if changed.is_err() || *self.shutdown.borrow() {
                        info!(node = %self.node.raft.id, "ClusterRunner received shutdown signal");
                        break;
                    }
                }
                // Drive one iteration of the Raft loop: recv → handle → tick.
                res = self.node.step(tick) => {
                    if let Err(e) = res {
                        warn!(
                            node = %self.node.raft.id,
                            error = %e,
                            "Raft step error"
                        );
                    }
                }
            }
        }

        backfill.abort();
        info!(node = %self.node.raft.id, "ClusterRunner stopped");
    }

    /// Copy documents onto replica shards this node does not have in sync.
    ///
    /// Runs beside the Raft step so a copy does not delay an election. The
    /// task stops when this runner stops.
    fn spawn_replica_backfill(&self) -> tokio::task::JoinHandle<()> {
        let applier = self.node.applier();
        let mut shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            let Some(applier) = applier else {
                return;
            };
            loop {
                if *shutdown.borrow() {
                    break;
                }
                applier.backfill_local_replicas().await;
                tokio::select! {
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            break;
                        }
                    }
                    _ = tokio::time::sleep(REPLICA_BACKFILL_INTERVAL) => {}
                }
            }
        })
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

    pub fn build(self, node: ClusterNode, shutdown: watch::Receiver<bool>) -> ClusterRunner {
        ClusterRunner::new(node, self.tick_interval, shutdown)
    }
}

impl Default for ClusterRunnerBuilder {
    fn default() -> Self {
        Self::new()
    }
}
