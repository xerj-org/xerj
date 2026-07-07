//! Cluster metadata store.
//!
//! [`ClusterMetadata`] is the state machine that Raft log entries are applied to.
//! It holds the authoritative cluster-wide view of indices, nodes, and shard
//! assignments — updated only by applying committed [`ClusterCommand`]s in log order.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::raft::ClusterCommand;

// ── Node state ───────────────────────────────────────────────────────────────

/// Life-cycle state of a cluster node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NodeState {
    /// Actively accepting reads and writes.
    Active,
    /// Gracefully draining — new shards won't be assigned here.
    Draining,
    /// Unreachable or removed from the cluster.
    Down,
}

impl std::fmt::Display for NodeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeState::Active => write!(f, "active"),
            NodeState::Draining => write!(f, "draining"),
            NodeState::Down => write!(f, "down"),
        }
    }
}

// ── Data types ───────────────────────────────────────────────────────────────

/// Information about a data node registered in the cluster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    /// Unique node identifier (UUID or configured name).
    pub id: String,
    /// `host:port` reachable by other nodes.
    pub address: String,
    /// Current life-cycle state.
    pub state: NodeState,
}

/// Metadata about a single index stored in the cluster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexMetadata {
    /// Index name.
    pub name: String,
    /// JSON-serialized schema / mapping definition.
    pub schema_json: String,
    /// Number of primary shards.
    pub num_shards: u32,
    /// Unix timestamp (seconds since epoch) of index creation.
    pub created_at: u64,
}

// ── Cluster metadata store ───────────────────────────────────────────────────

/// The replicated cluster state machine.
///
/// Every node in the cluster maintains an identical copy of this structure
/// by applying committed Raft log entries in strict log-index order.
///
/// # Consistency guarantee
///
/// Because entries are applied in commit order and Raft's log-matching property
/// guarantees identical committed prefixes, all live nodes converge to the same
/// `ClusterMetadata` state for any given log index.
#[derive(Debug, Clone, Default)]
pub struct ClusterMetadata {
    /// All indices known to the cluster.
    pub indices: HashMap<String, IndexMetadata>,

    /// All data nodes registered in the cluster.
    pub nodes: HashMap<String, NodeInfo>,

    /// Shard → node assignment table.
    /// Key: `(index_name, shard_number)`, value: `node_id` of the primary.
    pub shard_assignments: HashMap<(String, u32), String>,

    /// Arbitrary configuration key-value store replicated via Raft.
    pub config: HashMap<String, String>,

    /// Last applied log index (for idempotency checks).
    pub last_applied_index: u64,
}

impl ClusterMetadata {
    /// Create an empty metadata store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a committed [`ClusterCommand`] at `log_index`.
    ///
    /// Commands are idempotent — re-applying the same `log_index` is a no-op.
    /// They must be applied in strictly increasing index order.
    pub fn apply(&mut self, log_index: u64, command: &ClusterCommand) {
        if log_index <= self.last_applied_index {
            warn!(
                log_index,
                last_applied = self.last_applied_index,
                "Ignoring already-applied log entry"
            );
            return;
        }

        match command {
            ClusterCommand::CreateIndex { name, schema_json } => {
                self.apply_create_index(name, schema_json);
            }
            ClusterCommand::DeleteIndex { name } => {
                self.apply_delete_index(name);
            }
            ClusterCommand::UpdateMapping {
                index,
                mapping_json,
            } => {
                self.apply_update_mapping(index, mapping_json);
            }
            ClusterCommand::AssignShard {
                index,
                shard,
                node_id,
            } => {
                self.apply_assign_shard(index, *shard, node_id);
            }
            ClusterCommand::AddNode { node_id, address } => {
                self.apply_add_node(node_id, address);
            }
            ClusterCommand::RemoveNode { node_id } => {
                self.apply_remove_node(node_id);
            }
            ClusterCommand::UpdateConfig { key, value } => {
                self.apply_update_config(key, value);
            }
        }

        self.last_applied_index = log_index;
    }

    // ── Command handlers ─────────────────────────────────────────────────────

    fn apply_create_index(&mut self, name: &str, schema_json: &str) {
        if self.indices.contains_key(name) {
            warn!(index = %name, "CreateIndex: index already exists, skipping");
            return;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        info!(index = %name, "Applying CreateIndex");
        self.indices.insert(
            name.to_string(),
            IndexMetadata {
                name: name.to_string(),
                schema_json: schema_json.to_string(),
                num_shards: 1, // default; set by shard assignment commands
                created_at: now,
            },
        );
    }

    fn apply_delete_index(&mut self, name: &str) {
        if self.indices.remove(name).is_none() {
            warn!(index = %name, "DeleteIndex: index not found, skipping");
            return;
        }
        // Clean up shard assignments for this index
        self.shard_assignments.retain(|(idx, _), _| idx != name);
        info!(index = %name, "Applying DeleteIndex");
    }

    fn apply_update_mapping(&mut self, index: &str, mapping_json: &str) {
        if let Some(meta) = self.indices.get_mut(index) {
            info!(index = %index, "Applying UpdateMapping");
            meta.schema_json = mapping_json.to_string();
        } else {
            warn!(index = %index, "UpdateMapping: index not found");
        }
    }

    fn apply_assign_shard(&mut self, index: &str, shard: u32, node_id: &str) {
        info!(index = %index, shard, node = %node_id, "Applying AssignShard");
        self.shard_assignments
            .insert((index.to_string(), shard), node_id.to_string());

        // Update num_shards on the index if needed
        if let Some(meta) = self.indices.get_mut(index) {
            if shard + 1 > meta.num_shards {
                meta.num_shards = shard + 1;
            }
        }
    }

    fn apply_add_node(&mut self, node_id: &str, address: &str) {
        info!(node = %node_id, address, "Applying AddNode");
        self.nodes.insert(
            node_id.to_string(),
            NodeInfo {
                id: node_id.to_string(),
                address: address.to_string(),
                state: NodeState::Active,
            },
        );
    }

    fn apply_remove_node(&mut self, node_id: &str) {
        if let Some(info) = self.nodes.get_mut(node_id) {
            info!( node = %node_id, "Applying RemoveNode — marking Down");
            info.state = NodeState::Down;
        } else {
            warn!(node = %node_id, "RemoveNode: node not found");
        }
    }

    fn apply_update_config(&mut self, key: &str, value: &str) {
        info!(key, value, "Applying UpdateConfig");
        self.config.insert(key.to_string(), value.to_string());
    }

    // ── Query helpers ─────────────────────────────────────────────────────────

    /// Nodes in `Active` state.
    pub fn active_nodes(&self) -> Vec<&NodeInfo> {
        self.nodes
            .values()
            .filter(|n| n.state == NodeState::Active)
            .collect()
    }

    /// Node responsible for a given shard of an index, if assigned.
    pub fn shard_node(&self, index: &str, shard: u32) -> Option<&str> {
        self.shard_assignments
            .get(&(index.to_string(), shard))
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raft::ClusterCommand;

    #[test]
    fn test_create_and_delete_index() {
        let mut meta = ClusterMetadata::new();

        meta.apply(
            1,
            &ClusterCommand::CreateIndex {
                name: "logs".to_string(),
                schema_json: r#"{"properties":{}}"#.to_string(),
            },
        );
        assert!(meta.indices.contains_key("logs"));

        // Duplicate create is a no-op
        meta.apply(
            2,
            &ClusterCommand::CreateIndex {
                name: "logs".to_string(),
                schema_json: "{}".to_string(),
            },
        );
        assert_eq!(
            meta.indices["logs"].schema_json, r#"{"properties":{}}"#,
            "duplicate create should not overwrite schema"
        );

        meta.apply(
            3,
            &ClusterCommand::DeleteIndex {
                name: "logs".to_string(),
            },
        );
        assert!(!meta.indices.contains_key("logs"));
    }

    #[test]
    fn test_add_remove_node() {
        let mut meta = ClusterMetadata::new();

        meta.apply(
            1,
            &ClusterCommand::AddNode {
                node_id: "n1".to_string(),
                address: "10.0.0.1:9200".to_string(),
            },
        );
        assert_eq!(meta.nodes["n1"].state, NodeState::Active);

        meta.apply(
            2,
            &ClusterCommand::RemoveNode {
                node_id: "n1".to_string(),
            },
        );
        assert_eq!(meta.nodes["n1"].state, NodeState::Down);

        assert!(
            meta.active_nodes().is_empty(),
            "no active nodes after removal"
        );
    }

    #[test]
    fn test_shard_assignment() {
        let mut meta = ClusterMetadata::new();

        meta.apply(
            1,
            &ClusterCommand::CreateIndex {
                name: "products".to_string(),
                schema_json: "{}".to_string(),
            },
        );
        meta.apply(
            2,
            &ClusterCommand::AssignShard {
                index: "products".to_string(),
                shard: 0,
                node_id: "n1".to_string(),
            },
        );

        assert_eq!(meta.shard_node("products", 0), Some("n1"));
        assert_eq!(meta.indices["products"].num_shards, 1);
    }

    #[test]
    fn test_idempotent_apply() {
        let mut meta = ClusterMetadata::new();
        let cmd = ClusterCommand::UpdateConfig {
            key: "replica_count".to_string(),
            value: "2".to_string(),
        };

        meta.apply(1, &cmd);
        assert_eq!(meta.config["replica_count"], "2");

        // Re-apply same index — should be a no-op
        meta.apply(1, &cmd);
        assert_eq!(meta.last_applied_index, 1);
    }

    #[test]
    fn test_metadata_apply_from_raft_entries() {
        use crate::raft::{LogEntry, RaftNode};

        let mut node = RaftNode::new("n1".to_string(), vec![]);
        // Force leader
        node.force_election_timeout();
        node.tick();
        assert!(node.is_leader());

        node.propose(ClusterCommand::CreateIndex {
            name: "events".to_string(),
            schema_json: "{}".to_string(),
        })
        .unwrap();

        node.propose(ClusterCommand::AddNode {
            node_id: "n2".to_string(),
            address: "10.0.0.2:9200".to_string(),
        })
        .unwrap();

        let entries: Vec<LogEntry> = node.ready();
        assert_eq!(entries.len(), 2, "two entries should be ready");

        let mut meta = ClusterMetadata::new();
        for entry in &entries {
            meta.apply(entry.index, &entry.command);
        }

        assert!(
            meta.indices.contains_key("events"),
            "CreateIndex should be applied"
        );
        assert!(meta.nodes.contains_key("n2"), "AddNode should be applied");
    }
}
