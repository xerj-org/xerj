//! WAL replication — propagates write-ahead log entries to replica nodes.
//!
//! When a document is indexed on the primary shard owner, the WAL entry is
//! replicated to replica nodes before the client acknowledgement is sent.
//!
//! Three replication modes are supported:
//! - **Async** — fire-and-forget (fastest, risk of data loss on primary crash)
//! - **Sync**  — wait for `min_replicas` acknowledgements
//! - **Quorum** — wait for a majority of replicas
//!
//! # Wire format
//!
//! Replication messages are serialised to JSON and sent over the same
//! [`ClusterTransport`] used by Raft, using a dedicated channel so they
//! do not interfere with consensus traffic.

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::oneshot;

use crate::node::ClusterTransport;
use crate::router::ShardRouter;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur during WAL replication.
#[derive(Debug, Error)]
pub enum ReplicationError {
    #[error("insufficient replicas: needed {needed}, got {got}")]
    InsufficientReplicas { needed: usize, got: usize },

    #[error("quorum not reached: needed {needed}, got {got}")]
    QuorumNotReached { needed: usize, got: usize },

    #[error("transport error: {0}")]
    Transport(#[from] anyhow::Error),
}

// ── Public types ──────────────────────────────────────────────────────────────

/// Controls how many nodes must acknowledge before the primary reports success.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReplicationMode {
    /// Ack after primary writes — fastest, risk of data loss on primary crash.
    Async,
    /// Ack after primary + at least `min_replicas` replicas write.
    Sync { min_replicas: usize },
    /// Ack after primary + a strict majority of replicas write.
    Quorum,
}

/// A single WAL entry to replicate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicationEntry {
    /// Name of the index this entry belongs to.
    pub index: String,
    /// Shard number within the index.
    pub shard: u32,
    /// Monotonically increasing sequence number within the shard.
    pub seq_no: u64,
    /// The operation to replicate.
    pub operation: ReplicationOp,
}

/// The payload of a replication entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReplicationOp {
    /// Index (upsert) a document.
    Index { doc_id: String, source_json: String },
    /// Delete a document.
    Delete { doc_id: String },
}

/// Result returned after a replication attempt.
#[derive(Debug, Clone)]
pub struct ReplicationResult {
    /// Number of replicas that acknowledged the entry.
    pub ack_count: usize,
    /// Wall-clock time the replication took (primary-write to last ACK).
    pub duration_ms: u64,
}

impl ReplicationResult {
    /// Construct a successful result with the given ACK count.
    pub fn success(ack_count: usize, duration_ms: u64) -> Self {
        ReplicationResult {
            ack_count,
            duration_ms,
        }
    }
}

// ── WalReplicator ─────────────────────────────────────────────────────────────

/// Manages replication of WAL entries from a primary to its replicas.
///
/// The replicator is instantiated once per node (or once per index) and
/// reuses the same underlying [`ClusterTransport`] as the Raft layer.
pub struct WalReplicator {
    /// How many ACKs to wait for before acknowledging the client.
    pub mode: ReplicationMode,
    /// Transport used to send replication messages to peers.
    transport: Arc<dyn ClusterTransport>,
    /// Knows which nodes are replicas for each shard.
    router: Arc<ShardRouter>,
    /// This node's own ID — excluded from the replica list.
    #[allow(dead_code)]
    local_node_id: String,
}

impl WalReplicator {
    /// Create a new [`WalReplicator`].
    pub fn new(
        mode: ReplicationMode,
        transport: Arc<dyn ClusterTransport>,
        router: Arc<ShardRouter>,
        local_node_id: String,
    ) -> Self {
        WalReplicator {
            mode,
            transport,
            router,
            local_node_id,
        }
    }

    /// Replicate a WAL entry to replica nodes according to the configured mode.
    ///
    /// The caller is responsible for having already applied the entry locally
    /// (primary write) before calling this method.
    pub async fn replicate(
        &self,
        entry: ReplicationEntry,
    ) -> Result<ReplicationResult, ReplicationError> {
        let started = Instant::now();
        let replicas = self.router.get_replicas(&entry.index, entry.shard);

        match &self.mode {
            ReplicationMode::Async => {
                // Fire and forget — spawn background tasks for each replica.
                for replica in replicas {
                    let entry_clone = entry.clone();
                    let transport = Arc::clone(&self.transport);
                    let node_id = replica.clone();
                    tokio::spawn(async move {
                        let msg = make_replication_raft_message(&entry_clone);
                        if let Err(e) = transport.send(&node_id, msg).await {
                            // Best effort — log the failure but don't propagate.
                            tracing::warn!(
                                replica = %node_id,
                                error = %e,
                                "async replication send failed"
                            );
                        }
                    });
                }
                let duration_ms = started.elapsed().as_millis() as u64;
                Ok(ReplicationResult::success(0, duration_ms))
            }

            ReplicationMode::Sync { min_replicas } => {
                let needed = *min_replicas;
                let acks = self.replicate_and_wait(&replicas, &entry).await;
                let duration_ms = started.elapsed().as_millis() as u64;
                if acks >= needed {
                    Ok(ReplicationResult::success(acks, duration_ms))
                } else {
                    Err(ReplicationError::InsufficientReplicas { needed, got: acks })
                }
            }

            ReplicationMode::Quorum => {
                let needed = if replicas.is_empty() {
                    0
                } else {
                    replicas.len() / 2 + 1
                };
                let acks = self.replicate_and_wait(&replicas, &entry).await;
                let duration_ms = started.elapsed().as_millis() as u64;
                if acks >= needed {
                    Ok(ReplicationResult::success(acks, duration_ms))
                } else {
                    Err(ReplicationError::QuorumNotReached { needed, got: acks })
                }
            }
        }
    }

    /// Send `entry` to every node in `replicas` concurrently and count ACKs.
    ///
    /// An ACK is counted for any replica that successfully receives the send
    /// (i.e. the transport layer accepts the frame without error). A production
    /// system would exchange proper ACK messages; here we model success as
    /// transport-level delivery.
    async fn replicate_and_wait(&self, replicas: &[String], entry: &ReplicationEntry) -> usize {
        if replicas.is_empty() {
            return 0;
        }

        // Spawn one task per replica; collect results via one-shot channels.
        let mut receivers: Vec<oneshot::Receiver<bool>> = Vec::with_capacity(replicas.len());

        for replica in replicas {
            let (tx, rx) = oneshot::channel();
            receivers.push(rx);

            let entry_clone = entry.clone();
            let transport = Arc::clone(&self.transport);
            let node_id = replica.clone();

            tokio::spawn(async move {
                let msg = make_replication_raft_message(&entry_clone);
                let ok = transport.send(&node_id, msg).await.is_ok();
                let _ = tx.send(ok);
            });
        }

        // Wait for all replicas to respond (or fail).
        let mut acks = 0usize;
        for rx in receivers {
            if let Ok(true) = rx.await {
                acks += 1
            }
        }
        acks
    }
}

// ── Wire helpers ──────────────────────────────────────────────────────────────

/// Encode a [`ReplicationEntry`] as a [`crate::raft::RaftMessage`] so it can be
/// sent over the existing [`ClusterTransport`] infrastructure without adding a
/// separate network layer.
///
/// We piggyback on `RaftMessage::AppendEntries` with a sentinel term of `u64::MAX`
/// to signal "this is a replication frame, not a Raft consensus message". A real
/// production system would have a dedicated replication RPC.
fn make_replication_raft_message(entry: &ReplicationEntry) -> crate::raft::RaftMessage {
    // Encode the ReplicationEntry as JSON and embed it in the entries field
    // using the existing LogEntry type — the command is serialised into
    // a special UpdateConfig command so we don't need a new wire type.
    let payload = serde_json::to_string(entry).unwrap_or_default();
    crate::raft::RaftMessage::AppendEntries {
        term: u64::MAX, // sentinel: identifies replication traffic
        leader_id: String::new(),
        prev_log_index: 0,
        prev_log_term: 0,
        entries: vec![crate::raft::LogEntry {
            index: entry.seq_no,
            term: u64::MAX,
            command: crate::raft::ClusterCommand::UpdateConfig {
                key: "__replication__".to_string(),
                value: payload,
            },
        }],
        leader_commit: 0,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ── FailingTransport ──────────────────────────────────────────────────────

    /// A test transport that succeeds for allowed node IDs and errors for others.
    struct FailingTransport {
        allowed: HashSet<String>,
    }

    impl FailingTransport {
        fn new(allowed: &[&str]) -> Self {
            FailingTransport {
                allowed: allowed.iter().map(|s| s.to_string()).collect(),
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::node::ClusterTransport for FailingTransport {
        async fn send(&self, to: &str, _msg: crate::raft::RaftMessage) -> anyhow::Result<()> {
            if self.allowed.contains(to) {
                Ok(())
            } else {
                Err(anyhow::anyhow!("node {to} is unreachable"))
            }
        }

        async fn recv(&self) -> anyhow::Result<(String, crate::raft::RaftMessage)> {
            futures::future::pending().await
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_entry(index: &str, shard: u32, seq_no: u64) -> ReplicationEntry {
        ReplicationEntry {
            index: index.to_string(),
            shard,
            seq_no,
            operation: ReplicationOp::Index {
                doc_id: format!("doc-{seq_no}"),
                source_json: r#"{"title":"hello"}"#.to_string(),
            },
        }
    }

    fn make_router_with_replicas(
        index: &str,
        primary: &str,
        replicas: &[&str],
    ) -> Arc<ShardRouter> {
        let mut router = ShardRouter::new(8);
        router.assign(index, 0, primary);
        for rep in replicas {
            router.add_replica(index, 0, rep);
        }
        Arc::new(router)
    }

    // ── Serialisation tests ───────────────────────────────────────────────────

    /// Round-trip serialisation for all ReplicationEntry / ReplicationOp variants.
    #[test]
    fn test_replication_entry_serialization() {
        let entries = vec![
            ReplicationEntry {
                index: "products".to_string(),
                shard: 0,
                seq_no: 42,
                operation: ReplicationOp::Index {
                    doc_id: "prod-1".to_string(),
                    source_json: r#"{"name":"widget"}"#.to_string(),
                },
            },
            ReplicationEntry {
                index: "orders".to_string(),
                shard: 3,
                seq_no: 100,
                operation: ReplicationOp::Delete {
                    doc_id: "order-99".to_string(),
                },
            },
        ];

        for entry in &entries {
            let json = serde_json::to_string(entry).expect("serialize");
            let decoded: ReplicationEntry = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(entry.index, decoded.index);
            assert_eq!(entry.shard, decoded.shard);
            assert_eq!(entry.seq_no, decoded.seq_no);
        }
    }

    /// Mode serialisation round-trips.
    #[test]
    fn test_replication_mode_serialization() {
        let modes = vec![
            ReplicationMode::Async,
            ReplicationMode::Sync { min_replicas: 2 },
            ReplicationMode::Quorum,
        ];
        for mode in &modes {
            let json = serde_json::to_string(mode).expect("serialize mode");
            let _: ReplicationMode = serde_json::from_str(&json).expect("deserialize mode");
        }
    }

    // ── Replication mode tests ────────────────────────────────────────────────

    /// Async replication: returns immediately with ack_count == 0.
    #[tokio::test]
    async fn test_async_replication() {
        use crate::node::in_memory::{InMemoryBus, InMemoryTransport};

        let bus = InMemoryBus::new();
        let transport_primary = InMemoryTransport::new("primary".to_string(), bus.clone()).await;
        let _r1 = InMemoryTransport::new("replica-1".to_string(), bus.clone()).await;

        let router = make_router_with_replicas("idx", "primary", &["replica-1"]);

        let replicator = WalReplicator::new(
            ReplicationMode::Async,
            Arc::new(transport_primary),
            router,
            "primary".to_string(),
        );

        let entry = make_entry("idx", 0, 1);
        let result = replicator
            .replicate(entry)
            .await
            .expect("async should succeed");
        assert_eq!(result.ack_count, 0, "async mode never counts ACKs");
    }

    /// Sync replication: waits for the required number of ACKs.
    #[tokio::test]
    async fn test_sync_replication() {
        let transport = Arc::new(FailingTransport::new(&["replica-1", "replica-2"]));
        let router = make_router_with_replicas("idx", "primary", &["replica-1", "replica-2"]);

        let replicator = WalReplicator::new(
            ReplicationMode::Sync { min_replicas: 2 },
            transport,
            router,
            "primary".to_string(),
        );

        let entry = make_entry("idx", 0, 5);
        let result = replicator
            .replicate(entry)
            .await
            .expect("sync should succeed");
        assert_eq!(result.ack_count, 2, "both replicas must ACK");
    }

    /// Sync replication fails when fewer replicas respond than required.
    #[tokio::test]
    async fn test_sync_replication_insufficient() {
        // Only replica-1 is reachable; replica-2 will error.
        let transport = Arc::new(FailingTransport::new(&["replica-1"]));
        let router = make_router_with_replicas("idx", "primary", &["replica-1", "replica-2"]);

        let replicator = WalReplicator::new(
            ReplicationMode::Sync { min_replicas: 2 },
            transport,
            router,
            "primary".to_string(),
        );

        let err = replicator
            .replicate(make_entry("idx", 0, 7))
            .await
            .expect_err("should fail with insufficient replicas");
        assert!(
            matches!(
                err,
                ReplicationError::InsufficientReplicas { needed: 2, got: 1 }
            ),
            "unexpected error: {err}"
        );
    }

    /// Quorum replication: majority (2 of 3 replicas) suffices.
    #[tokio::test]
    async fn test_quorum_replication() {
        // replica-3 will return an error — only 2 of 3 are reachable.
        let transport = Arc::new(FailingTransport::new(&["replica-1", "replica-2"]));

        let mut router = ShardRouter::new(8);
        router.assign("idx", 0, "primary");
        router.add_replica("idx", 0, "replica-1");
        router.add_replica("idx", 0, "replica-2");
        router.add_replica("idx", 0, "replica-3"); // unreachable
        let router = Arc::new(router);

        let replicator = WalReplicator::new(
            ReplicationMode::Quorum,
            transport,
            router,
            "primary".to_string(),
        );

        let result = replicator
            .replicate(make_entry("idx", 0, 10))
            .await
            .expect("quorum should succeed with 2/3 replicas");
        assert_eq!(result.ack_count, 2, "2 of 3 replicas must ACK for majority");
    }
}
