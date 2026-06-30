//! Cluster node — wraps [`RaftNode`] + [`ClusterMetadata`] + transport.
//!
//! [`ClusterNode`] is the top-level runtime object that ties together:
//! - the pure Raft state machine ([`RaftNode`]),
//! - the replicated metadata store ([`ClusterMetadata`]),
//! - and an I/O transport ([`ClusterTransport`]) for sending/receiving messages.
//!
//! In production, the transport will be a gRPC/QUIC channel. In tests, an
//! in-memory channel is used (see [`transport`] module).

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use tracing::{debug, info, warn};

use crate::metadata::ClusterMetadata;
use crate::raft::{ClusterCommand, OutboundMessage, RaftMessage, RaftNode, RaftState};

// ── Transport abstraction ─────────────────────────────────────────────────────

/// Asynchronous message transport between cluster nodes.
///
/// Implementors are responsible for serialisation and delivery. The Raft state
/// machine itself is purely synchronous — the transport bridges it to async I/O.
#[async_trait]
pub trait ClusterTransport: Send + Sync {
    /// Send a Raft message to the node identified by `to`.
    async fn send(&self, to: &str, msg: RaftMessage) -> Result<()>;

    /// Receive the next inbound message. Returns `(sender_id, message)`.
    async fn recv(&self) -> Result<(String, RaftMessage)>;
}

// ── Cluster node ──────────────────────────────────────────────────────────────

/// A running cluster node.
///
/// Drives the Raft state machine forward via periodic ticks and delivers
/// committed log entries to the local [`ClusterMetadata`] state machine.
pub struct ClusterNode {
    pub raft: RaftNode,
    pub metadata: ClusterMetadata,
    transport: Box<dyn ClusterTransport>,
}

impl ClusterNode {
    /// Create a new cluster node with the given identity, peers, and transport.
    pub fn new(
        id: String,
        peers: Vec<String>,
        transport: Box<dyn ClusterTransport>,
    ) -> Self {
        ClusterNode {
            raft: RaftNode::new(id, peers),
            metadata: ClusterMetadata::new(),
            transport,
        }
    }

    /// Tick the Raft state machine and dispatch any outbound messages.
    ///
    /// Call this periodically (e.g. every 10 ms) from the cluster run loop.
    pub async fn tick(&mut self) -> Result<()> {
        let msgs = self.raft.tick();
        self.dispatch(msgs).await?;
        self.apply_ready();
        Ok(())
    }

    /// Process a single inbound message from a peer.
    pub async fn handle_message(&mut self, from: &str, msg: RaftMessage) -> Result<()> {
        debug!(node = %self.raft.id, %from, "Handling inbound Raft message");
        let _ = from; // currently unused directly — sender is embedded in message
        let replies = self.raft.handle_message(msg);
        self.dispatch(replies).await?;
        self.apply_ready();
        Ok(())
    }

    /// Propose a command through Raft consensus (leader only).
    ///
    /// After calling this, the entry will be replicated and, once committed,
    /// automatically applied to the local [`ClusterMetadata`] via the run loop.
    pub fn propose(&mut self, cmd: ClusterCommand) -> Result<u64> {
        self.raft.propose(cmd)
    }

    /// Run the node event loop until `shutdown` fires.
    ///
    /// Continuously:
    /// 1. Waits for an inbound message (with a timeout equal to the tick interval).
    /// 2. Delivers the message to the Raft state machine.
    /// 3. Ticks the state machine regardless.
    ///
    /// In production you would wrap this with a `tokio::select!` that listens
    /// for a shutdown signal and for the transport channel.
    pub async fn run(&mut self, tick_interval: Duration) -> Result<()> {
        info!(node = %self.raft.id, "Starting cluster node run loop");
        loop {
            // Try to receive a message within the tick interval
            let recv_result = tokio::time::timeout(
                tick_interval,
                self.transport.recv(),
            )
            .await;

            match recv_result {
                Ok(Ok((from, msg))) => {
                    self.handle_message(&from, msg).await?;
                }
                Ok(Err(e)) => {
                    warn!(node = %self.raft.id, error = %e, "Transport recv error");
                }
                Err(_timeout) => {
                    // Timeout is normal — just tick
                }
            }

            self.tick().await?;
        }
    }

    /// Whether this node is the current Raft leader.
    pub fn is_leader(&self) -> bool {
        self.raft.is_leader()
    }

    /// The node ID of the current Raft leader, if known.
    pub fn leader_id(&self) -> Option<&str> {
        self.raft.leader_id()
    }

    /// Current Raft state.
    pub fn state(&self) -> &RaftState {
        self.raft.state()
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    async fn dispatch(&self, msgs: Vec<OutboundMessage>) -> Result<()> {
        for out in msgs {
            if let Err(e) = self.transport.send(&out.to, out.msg).await {
                warn!(
                    node = %self.raft.id,
                    to = %out.to,
                    error = %e,
                    "Failed to send Raft message"
                );
                // Don't propagate — Raft handles message loss via retransmit
            }
        }
        Ok(())
    }

    /// Drain and apply all newly committed log entries.
    fn apply_ready(&mut self) {
        let entries = self.raft.ready();
        for entry in entries {
            debug!(
                node = %self.raft.id,
                index = entry.index,
                term = entry.term,
                "Applying committed entry"
            );
            self.metadata.apply(entry.index, &entry.command);
        }
    }
}

// ── In-memory transport for tests ────────────────────────────────────────────

/// In-memory transport that routes messages via `tokio::sync::mpsc` channels.
///
/// Used in unit tests to avoid network I/O while still exercising the full
/// Raft message-passing logic.
pub mod in_memory {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::{mpsc, Mutex};

    type Sender = mpsc::UnboundedSender<(String, RaftMessage)>;
    type Receiver = mpsc::UnboundedReceiver<(String, RaftMessage)>;

    /// A registry of in-memory channels, one per node.
    #[derive(Clone)]
    pub struct InMemoryBus {
        senders: Arc<Mutex<HashMap<String, Sender>>>,
    }

    impl InMemoryBus {
        pub fn new() -> Self {
            InMemoryBus {
                senders: Arc::new(Mutex::new(HashMap::new())),
            }
        }

        /// Register a node and return its dedicated receiver channel.
        pub async fn register(&self, node_id: &str) -> Receiver {
            let (tx, rx) = mpsc::unbounded_channel();
            self.senders
                .lock()
                .await
                .insert(node_id.to_string(), tx);
            rx
        }

        /// Send a message directly (used by [`InMemoryTransport`]).
        pub async fn send_to(
            &self,
            from: &str,
            to: &str,
            msg: RaftMessage,
        ) -> Result<()> {
            let senders = self.senders.lock().await;
            if let Some(tx) = senders.get(to) {
                tx.send((from.to_string(), msg))
                    .map_err(|e| anyhow::anyhow!("channel closed: {}", e))?;
            } else {
                // Node not registered — simulate network partition
                debug!(from, to, "InMemoryBus: target node not found (partitioned?)");
            }
            Ok(())
        }
    }

    impl Default for InMemoryBus {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Transport implementation backed by [`InMemoryBus`].
    pub struct InMemoryTransport {
        pub node_id: String,
        bus: InMemoryBus,
        receiver: Arc<Mutex<Receiver>>,
    }

    impl InMemoryTransport {
        pub async fn new(node_id: String, bus: InMemoryBus) -> Self {
            let receiver = bus.register(&node_id).await;
            InMemoryTransport {
                node_id,
                bus,
                receiver: Arc::new(Mutex::new(receiver)),
            }
        }
    }

    #[async_trait]
    impl ClusterTransport for InMemoryTransport {
        async fn send(&self, to: &str, msg: RaftMessage) -> Result<()> {
            self.bus.send_to(&self.node_id, to, msg).await
        }

        async fn recv(&self) -> Result<(String, RaftMessage)> {
            let mut rx = self.receiver.lock().await;
            rx.recv()
                .await
                .ok_or_else(|| anyhow::anyhow!("channel closed"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::in_memory::{InMemoryBus, InMemoryTransport};
    use super::*;
    #[allow(unused_imports)]
    use std::time::Duration;

    async fn make_cluster(
        ids: &[&str],
    ) -> Vec<ClusterNode> {
        let bus = InMemoryBus::new();
        let id_list: Vec<String> = ids.iter().map(|s| s.to_string()).collect();

        let mut nodes = Vec::new();
        for id in ids {
            let peers: Vec<String> = id_list
                .iter()
                .filter(|p| p.as_str() != *id)
                .cloned()
                .collect();
            let transport = InMemoryTransport::new(id.to_string(), bus.clone()).await;
            let node = ClusterNode::new(
                id.to_string(),
                peers,
                Box::new(transport),
            );
            nodes.push(node);
        }
        nodes
    }

    #[tokio::test]
    async fn test_cluster_node_propose_and_apply() {
        // Single-node cluster: easy path
        let mut nodes = make_cluster(&["n1"]).await;

        // Force election timeout
        nodes[0].raft.force_election_timeout();
        nodes[0].tick().await.unwrap();

        assert!(nodes[0].is_leader(), "single node should become leader");

        nodes[0]
            .propose(ClusterCommand::CreateIndex {
                name: "orders".to_string(),
                schema_json: "{}".to_string(),
            })
            .unwrap();

        // Apply ready entries
        nodes[0].tick().await.unwrap();

        assert!(
            nodes[0].metadata.indices.contains_key("orders"),
            "CreateIndex should be applied to metadata"
        );
    }

    #[tokio::test]
    async fn test_metadata_apply_full_pipeline() {
        let mut nodes = make_cluster(&["n1"]).await;

        nodes[0].raft.force_election_timeout();
        nodes[0].tick().await.unwrap();

        // Apply several commands
        nodes[0]
            .propose(ClusterCommand::AddNode {
                node_id: "n2".to_string(),
                address: "127.0.0.1:9201".to_string(),
            })
            .unwrap();
        nodes[0]
            .propose(ClusterCommand::UpdateConfig {
                key: "replicas".to_string(),
                value: "2".to_string(),
            })
            .unwrap();

        nodes[0].tick().await.unwrap();

        assert!(nodes[0].metadata.nodes.contains_key("n2"));
        assert_eq!(nodes[0].metadata.config["replicas"], "2");
    }
}
