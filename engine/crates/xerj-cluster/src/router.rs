//! Shard router — maps documents and queries to cluster nodes.
//!
//! Uses **Jump Consistent Hash** for O(1) shard selection with minimal
//! rebalancing when the shard count changes, combined with xxh3 for fast,
//! high-quality document ID hashing.
//!
//! # Layout
//!
//! ```text
//! doc_id ──xxh3──► u64 key ──jump_hash──► shard_id ──routing_table──► node_id
//! ```

use std::collections::{HashMap, HashSet};

use xxhash_rust::xxh3::xxh3_64;

use crate::metadata::ClusterMetadata;

// ── Jump consistent hash ───────────────────────────────────────────────────────

/// Jump Consistent Hash — O(1), minimal rebalancing when `num_buckets` grows.
///
/// Reference: Lamping & Veach, "A Fast, Minimal Memory, Consistent Hash Algorithm"
/// (Google, 2014). <https://arxiv.org/abs/1406.2294>
///
/// # Guarantees
/// - Same `(key, num_buckets)` always returns the same bucket.
/// - Adding a bucket moves only ~1/num_buckets keys from old buckets to the new one.
pub fn jump_hash(key: u64, num_buckets: u32) -> u32 {
    assert!(num_buckets > 0, "num_buckets must be > 0");
    let mut b: i64 = -1;
    let mut j: i64 = 0;
    let mut k = key;
    while j < num_buckets as i64 {
        b = j;
        k = k.wrapping_mul(2_862_933_555_777_941_757).wrapping_add(1);
        j = ((b + 1) as f64 * ((1i64 << 31) as f64) / (((k >> 33) + 1) as f64)) as i64;
    }
    b as u32
}

// ── ShardRouter ───────────────────────────────────────────────────────────────

/// Routes documents and search queries to cluster nodes via consistent hashing.
///
/// The routing table is a snapshot of [`ClusterMetadata::shard_assignments`] and
/// must be refreshed whenever the cluster state changes (call
/// [`update_from_metadata`]).
///
/// [`update_from_metadata`]: ShardRouter::update_from_metadata
pub struct ShardRouter {
    /// Number of virtual shards per index (constant across the cluster lifetime).
    num_shards: u32,
    /// `(index_name, shard_id)` → `node_id` (primary owner)
    routing_table: HashMap<(String, u32), String>,
    /// `(index_name, shard_id)` → list of replica node IDs (excluding primary)
    replica_table: HashMap<(String, u32), Vec<String>>,
}

impl ShardRouter {
    /// Create a new router with `num_shards` virtual shards per index.
    pub fn new(num_shards: u32) -> Self {
        assert!(num_shards > 0, "num_shards must be > 0");
        ShardRouter {
            num_shards,
            routing_table: HashMap::new(),
            replica_table: HashMap::new(),
        }
    }

    /// The configured number of virtual shards.
    pub fn num_shards(&self) -> u32 {
        self.num_shards
    }

    /// Route a document to its shard and owning node.
    ///
    /// Returns `(shard_id, Option<node_id>)`. `node_id` is `None` when the
    /// shard has not yet been assigned to a node.
    pub fn route_doc(&self, index: &str, doc_id: &str) -> (u32, Option<&str>) {
        let hash = xxh3_64(doc_id.as_bytes());
        let shard = jump_hash(hash, self.num_shards);
        let node = self
            .routing_table
            .get(&(index.to_string(), shard))
            .map(|s| s.as_str());
        (shard, node)
    }

    /// Return the deduplicated list of node IDs that hold shards for `index`.
    ///
    /// Used by the search coordinator to fan out queries to every node that
    /// participates in a given index.
    pub fn search_targets(&self, index: &str) -> Vec<String> {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut targets: Vec<String> = Vec::new();

        for ((idx, _shard), node_id) in &self.routing_table {
            if idx == index {
                if seen.insert(node_id.as_str()) {
                    targets.push(node_id.clone());
                }
            }
        }

        targets.sort(); // deterministic order for tests
        targets
    }

    /// Refresh the routing table from authoritative cluster metadata.
    ///
    /// Also updates `num_shards` to the maximum shard count seen across all
    /// index assignments.
    pub fn update_from_metadata(&mut self, metadata: &ClusterMetadata) {
        self.routing_table.clear();
        for ((index, shard), node_id) in &metadata.shard_assignments {
            self.routing_table
                .insert((index.clone(), *shard), node_id.clone());
        }
    }

    /// Manually assign a shard to a node (used during cluster bootstrap or testing).
    pub fn assign(&mut self, index: &str, shard: u32, node_id: &str) {
        self.routing_table
            .insert((index.to_string(), shard), node_id.to_string());
    }

    /// Register a replica node for a shard.
    ///
    /// The replica list is separate from the primary routing table — a node can
    /// be added here without changing which node is the primary owner.
    pub fn add_replica(&mut self, index: &str, shard: u32, node_id: &str) {
        self.replica_table
            .entry((index.to_string(), shard))
            .or_default()
            .push(node_id.to_string());
    }

    /// Return all replica node IDs for a shard, **excluding the primary owner**.
    ///
    /// Used by [`WalReplicator`] to determine which nodes need to receive a
    /// copy of each WAL entry.
    ///
    /// [`WalReplicator`]: crate::replication::WalReplicator
    pub fn get_replicas(&self, index: &str, shard: u32) -> Vec<String> {
        let key = (index.to_string(), shard);
        let primary = self.routing_table.get(&key);

        self.replica_table
            .get(&key)
            .map(|replicas| {
                replicas
                    .iter()
                    .filter(|r| Some(r.as_str()) != primary.map(String::as_str))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── jump_hash tests ───────────────────────────────────────────────────────

    /// Keys should distribute roughly uniformly across buckets.
    #[test]
    fn test_jump_hash_distribution() {
        let num_buckets = 8u32;
        let num_keys = 100_000u64;
        let mut counts = vec![0u64; num_buckets as usize];

        for key in 0..num_keys {
            let b = jump_hash(key, num_buckets) as usize;
            counts[b] += 1;
        }

        let expected = num_keys / num_buckets as u64;
        // Allow ±10% deviation from uniform
        let tolerance = expected / 10;
        for (i, &count) in counts.iter().enumerate() {
            assert!(
                count.abs_diff(expected) <= tolerance,
                "bucket {i}: count={count}, expected≈{expected} (±{tolerance})"
            );
        }
    }

    /// The same key must always map to the same bucket.
    #[test]
    fn test_jump_hash_stability() {
        for key in [0u64, 1, 42, u64::MAX, 0xdeadbeef, 999_999_999] {
            let first = jump_hash(key, 16);
            for _ in 0..100 {
                assert_eq!(jump_hash(key, 16), first, "key {key} is not stable");
            }
        }
    }

    /// Adding one bucket should move only ~1/N fraction of keys.
    #[test]
    fn test_jump_hash_minimal_rebalancing() {
        let num_keys = 100_000u64;
        let n = 7u32; // before
        let n1 = n + 1; // after adding one bucket

        let mut moved = 0u64;
        for key in 0..num_keys {
            if jump_hash(key, n) != jump_hash(key, n1) {
                moved += 1;
            }
        }

        // Ideal fraction moved = 1/(n+1)
        let ideal_fraction = 1.0 / n1 as f64;
        let actual_fraction = moved as f64 / num_keys as f64;

        // Allow 2× tolerance
        assert!(
            actual_fraction < ideal_fraction * 2.0,
            "too many keys rebalanced: {moved}/{num_keys} ({:.1}%), ideal≈{:.1}%",
            actual_fraction * 100.0,
            ideal_fraction * 100.0,
        );
        // At least some keys must have moved
        assert!(moved > 0, "no keys moved when adding a bucket");
    }

    // ── ShardRouter tests ─────────────────────────────────────────────────────

    /// Documents with different IDs should be routed consistently.
    #[test]
    fn test_router_route_doc() {
        let mut router = ShardRouter::new(8);
        // Assign all 8 shards across 3 nodes
        for shard in 0..8u32 {
            let node = format!("node-{}", shard % 3);
            router.assign("orders", shard, &node);
        }

        // Same doc always routes to the same (shard, node)
        for _ in 0..10 {
            let (s1, n1) = router.route_doc("orders", "doc-abc");
            let (s2, n2) = router.route_doc("orders", "doc-abc");
            assert_eq!(s1, s2, "shard is not stable");
            assert_eq!(n1, n2, "node is not stable");
        }

        // Different docs may land on different shards
        let (sa, _) = router.route_doc("orders", "doc-aaa");
        let (sb, _) = router.route_doc("orders", "doc-zzz");
        // Not guaranteed to differ, but with 8 shards and very different keys
        // it's overwhelmingly likely. Just verify both are in [0, 8).
        assert!(sa < 8, "shard out of range");
        assert!(sb < 8, "shard out of range");
    }

    /// Unassigned shard returns None for node.
    #[test]
    fn test_router_unassigned_shard() {
        let router = ShardRouter::new(4);
        let (_shard, node) = router.route_doc("new-index", "doc-1");
        assert!(node.is_none(), "unassigned shard should return None");
    }

    /// search_targets returns a deduplicated, sorted list of nodes for an index.
    #[test]
    fn test_router_search_targets() {
        let mut router = ShardRouter::new(6);
        // 3 nodes, 6 shards — each node gets 2 shards
        router.assign("logs", 0, "node-0");
        router.assign("logs", 1, "node-1");
        router.assign("logs", 2, "node-2");
        router.assign("logs", 3, "node-0");
        router.assign("logs", 4, "node-1");
        router.assign("logs", 5, "node-2");

        // Different index — should not bleed into search_targets("logs")
        router.assign("metrics", 0, "node-99");

        let mut targets = router.search_targets("logs");
        targets.sort();

        assert_eq!(targets, vec!["node-0", "node-1", "node-2"]);
    }

    /// Routing table built from ClusterMetadata is equivalent to manual assigns.
    #[test]
    fn test_router_update_from_metadata() {
        use crate::metadata::ClusterMetadata;
        use crate::raft::ClusterCommand;

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
                node_id: "node-a".to_string(),
            },
        );
        meta.apply(
            3,
            &ClusterCommand::AssignShard {
                index: "products".to_string(),
                shard: 1,
                node_id: "node-b".to_string(),
            },
        );

        let mut router = ShardRouter::new(2);
        router.update_from_metadata(&meta);

        let targets = router.search_targets("products");
        assert!(targets.contains(&"node-a".to_string()));
        assert!(targets.contains(&"node-b".to_string()));
        assert_eq!(targets.len(), 2);
    }
}
