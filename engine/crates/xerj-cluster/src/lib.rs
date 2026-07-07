//! # xerj-cluster
//!
//! Embedded Raft consensus for xerj cluster metadata.
//!
//! This crate implements Raft from scratch — **no external Raft library** is used.
//! The goals are:
//! - No heavy dependencies (no etcd, no ZooKeeper, no Consul)
//! - Suitable for embedding directly into the xerj server process
//! - Cluster-wide metadata only (index schemas, shard assignments, node roster)
//! - Full correctness: leader election, log replication, log safety, commit rules
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────┐
//! │                   ClusterNode                        │
//! │  ┌─────────────┐  ┌───────────────────────────────┐ │
//! │  │  RaftNode   │  │      ClusterMetadata          │ │
//! │  │  (pure SM)  │──│  indices / nodes / shards     │ │
//! │  └─────────────┘  └───────────────────────────────┘ │
//! │         │                                            │
//! │  ┌──────▼────────────────┐                          │
//! │  │   ClusterTransport    │  (gRPC / in-memory)      │
//! │  └───────────────────────┘                          │
//! └──────────────────────────────────────────────────────┘
//! ```
//!
//! ## Usage
//!
//! ```rust,no_run
//! use xerj_cluster::node::{ClusterNode, in_memory::{InMemoryBus, InMemoryTransport}};
//! use xerj_cluster::raft::ClusterCommand;
//!
//! #[tokio::main]
//! async fn main() {
//!     let bus = InMemoryBus::new();
//!     let transport = InMemoryTransport::new("node-1".to_string(), bus).await;
//!     let mut node = ClusterNode::new(
//!         "node-1".to_string(),
//!         vec![],
//!         Box::new(transport),
//!     );
//!
//!     // Propose a new index (once this node is the leader)
//!     // node.propose(ClusterCommand::CreateIndex { name: "my_index".into(), schema_json: "{}".into() }).unwrap();
//! }
//! ```

pub mod coordinator;
pub mod metadata;
pub mod node;
pub mod raft;
pub mod raft_log;
pub mod regions;
pub mod replication;
pub mod router;
pub mod runner;
pub mod search;
pub mod transport;

// ── Convenience re-exports ────────────────────────────────────────────────────

pub use coordinator::{
    IndexResponse, LocalSearcher, MergedSearchResult, SearchCoordinator, SearchTransport,
};
pub use metadata::{ClusterMetadata, IndexMetadata, NodeInfo, NodeState};
pub use node::{ClusterNode, ClusterTransport};
pub use raft::{ClusterCommand, LogEntry, RaftMessage, RaftNode, RaftState};
pub use regions::{Region, RegionManager, RegionMove};
pub use replication::{
    ReplicationEntry, ReplicationMode, ReplicationOp, ReplicationResult, WalReplicator,
};
pub use router::jump_hash;
pub use router::ShardRouter;
pub use runner::{ClusterRunner, ClusterRunnerBuilder};
pub use search::{merge_search_responses, SearchHit, SearchMessage};
pub use transport::TcpTransport;
