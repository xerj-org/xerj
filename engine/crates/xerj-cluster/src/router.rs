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

/// Where a document of an index goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route<'a> {
    /// The index has no shard plan (single-node, or created outside the
    /// replicated plan). The receiving node stores the document.
    NoPlan { shard: u32 },
    /// The plan names `node` as the primary of `shard`.
    Owned { shard: u32, node: &'a str },
    /// The index has a plan, but no primary is assigned for `shard`. The
    /// write must be refused: storing it locally puts it outside the plan,
    /// where search and reads by id never look.
    Unowned { shard: u32 },
}

impl Route<'_> {
    pub fn shard(&self) -> u32 {
        match self {
            Route::NoPlan { shard } | Route::Owned { shard, .. } | Route::Unowned { shard } => {
                *shard
            }
        }
    }
}

/// The index has a shard plan but the document's shard has no primary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShardUnassigned {
    pub shard: u32,
}

/// Routes documents and search queries to cluster nodes via consistent hashing.
///
/// The routing table is a snapshot of [`ClusterMetadata::shard_assignments`] and
/// must be refreshed whenever the cluster state changes (call
/// [`update_from_metadata`]).
///
/// [`update_from_metadata`]: ShardRouter::update_from_metadata
pub struct ShardRouter {
    /// Hash width for an index that has no plan. Never applied to an index
    /// that has one: see `index_shards`.
    num_shards: u32,
    /// Hash width of each planned index: its highest assigned shard + 1.
    ///
    /// Per index, never cluster-wide. A single width shared by every index
    /// meant that creating one index with more shards re-bucketed every
    /// older index, so acknowledged ids routed to a shard that did not
    /// hold them.
    index_shards: HashMap<String, u32>,
    /// `(index_name, shard_id)` → `node_id` (primary owner)
    routing_table: HashMap<(String, u32), String>,
    /// `(index_name, shard_id)` → list of replica node IDs (excluding primary)
    replica_table: HashMap<(String, u32), Vec<String>>,
    /// Replicas a read or a later copy may use. Absent means every placed
    /// replica is in sync. Present, including empty, replaces that list.
    in_sync: HashMap<(String, u32), Vec<String>>,
}

impl ShardRouter {
    /// Create a new router with `num_shards` virtual shards per index.
    pub fn new(num_shards: u32) -> Self {
        assert!(num_shards > 0, "num_shards must be > 0");
        ShardRouter {
            num_shards,
            index_shards: HashMap::new(),
            routing_table: HashMap::new(),
            replica_table: HashMap::new(),
            in_sync: HashMap::new(),
        }
    }

    /// Hash width used for an index with no shard plan.
    pub fn num_shards(&self) -> u32 {
        self.num_shards
    }

    /// Hash width of `index`'s own plan, when it has one.
    pub fn index_num_shards(&self, index: &str) -> Option<u32> {
        self.index_shards.get(index).copied()
    }

    /// Set the hash width for indices that have no plan. A planned index
    /// keeps the width of its own plan.
    pub fn set_num_shards(&mut self, num_shards: u32) {
        assert!(num_shards > 0, "num_shards must be > 0");
        self.num_shards = num_shards;
    }

    /// Drop every primary and replica row for `index`.
    pub fn clear_index(&mut self, index: &str) {
        self.routing_table.retain(|(idx, _), _| idx != index);
        self.replica_table.retain(|(idx, _), _| idx != index);
        self.in_sync.retain(|(idx, _), _| idx != index);
        self.index_shards.remove(index);
    }

    /// Route a document to its shard and owning node.
    ///
    /// The shard comes from `index`'s own plan width, or from the fallback
    /// width when the index has no plan. Returns `(shard_id, Option<node_id>)`;
    /// `node_id` is `None` when no primary is assigned. Use [`Self::route`]
    /// to tell "no plan" from "planned but unassigned".
    pub fn route_doc(&self, index: &str, doc_id: &str) -> (u32, Option<&str>) {
        let route = self.route(index, doc_id);
        match route {
            Route::Owned { shard, node } => (shard, Some(node)),
            other => (other.shard(), None),
        }
    }

    /// Route a document, distinguishing an unplanned index from a planned
    /// shard that has no primary.
    pub fn route(&self, index: &str, doc_id: &str) -> Route<'_> {
        let planned = self.index_shards.get(index).copied();
        let width = planned.unwrap_or(self.num_shards);
        let shard = jump_hash(xxh3_64(doc_id.as_bytes()), width);
        match self.routing_table.get(&(index.to_string(), shard)) {
            Some(node) => Route::Owned {
                shard,
                node: node.as_str(),
            },
            None if planned.is_some() => Route::Unowned { shard },
            None => Route::NoPlan { shard },
        }
    }

    /// Return the deduplicated list of node IDs that hold shards for `index`.
    ///
    /// Used by the search coordinator to fan out queries to every node that
    /// participates in a given index.
    pub fn search_targets(&self, index: &str) -> Vec<String> {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut targets: Vec<String> = Vec::new();

        for ((idx, _shard), node_id) in &self.routing_table {
            if idx == index && seen.insert(node_id.as_str()) {
                targets.push(node_id.clone());
            }
        }

        targets.sort(); // deterministic order for tests
        targets
    }

    /// Refresh the routing table from authoritative cluster metadata.
    ///
    /// Each index's hash width is derived from that index's own assignments
    /// (and its recorded `num_shards`, when larger). The fallback width for
    /// unplanned indices is left alone.
    pub fn update_from_metadata(&mut self, metadata: &ClusterMetadata) {
        self.routing_table.clear();
        self.replica_table.clear();
        self.in_sync.clear();
        self.index_shards.clear();
        for ((index, shard), node_id) in &metadata.shard_assignments {
            self.assign(index, *shard, node_id);
        }
        for (index, width) in self.index_shards.iter_mut() {
            if let Some(meta) = metadata.indices.get(index) {
                *width = (*width).max(meta.num_shards);
            }
        }
    }

    /// Assign a shard's primary. Widens `index`'s own hash width to cover
    /// `shard`; other indices are not affected.
    pub fn assign(&mut self, index: &str, shard: u32, node_id: &str) {
        self.routing_table
            .insert((index.to_string(), shard), node_id.to_string());
        let width = self.index_shards.entry(index.to_string()).or_insert(0);
        *width = (*width).max(shard.saturating_add(1));
    }

    /// Register a replica node for a shard.
    ///
    /// The replica list is separate from the primary routing table — a node can
    /// be added here without changing which node is the primary owner.
    pub fn add_replica(&mut self, index: &str, shard: u32, node_id: &str) {
        let replicas = self
            .replica_table
            .entry((index.to_string(), shard))
            .or_default();
        if !replicas.iter().any(|id| id == node_id) {
            replicas.push(node_id.to_string());
        }
    }

    /// Primary shard ids of `index` owned by `node_id`, sorted.
    pub fn shards_owned_by(&self, index: &str, node_id: &str) -> Vec<u32> {
        let mut shards: Vec<u32> = self
            .routing_table
            .iter()
            .filter(|((idx, _), owner)| idx == index && owner.as_str() == node_id)
            .map(|((_, shard), _)| *shard)
            .collect();
        shards.sort_unstable();
        shards
    }

    /// Shard ids of `index` for which `node_id` is a replica, sorted.
    pub fn replica_shards_for(&self, index: &str, node_id: &str) -> Vec<u32> {
        let mut shards: Vec<u32> = self
            .replica_table
            .iter()
            .filter(|((idx, _), replicas)| idx == index && replicas.iter().any(|id| id == node_id))
            .map(|((_, shard), _)| *shard)
            .collect();
        shards.sort_unstable();
        shards.dedup();
        shards
    }

    /// Primary node id for one shard, when the plan names one.
    pub fn primary_node(&self, index: &str, shard: u32) -> Option<String> {
        self.routing_table.get(&(index.to_string(), shard)).cloned()
    }

    /// Replica node ids a read or a copy uses, excluding the primary.
    ///
    /// This is the in-sync set when one has been published, and otherwise
    /// every replica placed at create.
    pub fn get_replicas(&self, index: &str, shard: u32) -> Vec<String> {
        let key = (index.to_string(), shard);
        if let Some(nodes) = self.in_sync.get(&key) {
            return nodes.clone();
        }
        self.placed_replicas(index, shard)
    }

    /// Replicas named when the index was created, excluding the primary.
    ///
    /// The quorum is computed from this count. A later in-sync shrink does
    /// not change it: losing replicas does not lower the bar to the primary
    /// alone.
    pub fn placed_replicas(&self, index: &str, shard: u32) -> Vec<String> {
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

    /// Publish the replicas that confirmed the last acknowledged write.
    ///
    /// Ids that are not placed replicas of this shard are dropped. A shard
    /// with no placed replicas is left unchanged, so a command that arrives
    /// before the plan does not install an empty overlay.
    pub fn set_in_sync(&mut self, index: &str, shard: u32, nodes: &[String]) {
        let placed = self.placed_replicas(index, shard);
        if placed.is_empty() {
            return;
        }
        let mut kept: Vec<String> = nodes
            .iter()
            .filter(|id| placed.iter().any(|placed_id| placed_id == *id))
            .cloned()
            .collect();
        kept.sort();
        kept.dedup();
        self.in_sync.insert((index.to_string(), shard), kept);
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

        // Start with a single virtual shard: refreshing from metadata that
        // assigns shards 0 and 1 must give `products` a width of 2.
        let mut router = ShardRouter::new(1);
        router.update_from_metadata(&meta);

        // The index's own width follows its assignments; the fallback width
        // for unplanned indices is not widened by another index's plan.
        assert_eq!(router.index_num_shards("products"), Some(2));
        assert_eq!(
            router.num_shards(),
            1,
            "fallback width is per-cluster, untouched"
        );

        let targets = router.search_targets("products");
        assert!(targets.contains(&"node-a".to_string()));
        assert!(targets.contains(&"node-b".to_string()));
        assert_eq!(targets.len(), 2);

        // Routing must resolve to a real assigned node now that both shards
        // are reachable — with num_shards==1 shard 1 was unreachable.
        let mut saw_node_a = false;
        let mut saw_node_b = false;
        for i in 0..64 {
            let (shard, node) = router.route_doc("products", &format!("doc-{i}"));
            assert!(shard < 2, "shard {shard} out of range for num_shards=2");
            match node {
                Some("node-a") => saw_node_a = true,
                Some("node-b") => saw_node_b = true,
                other => panic!("doc routed to unexpected node: {other:?}"),
            }
        }
        assert!(
            saw_node_a && saw_node_b,
            "both shards should be reachable after refresh (a={saw_node_a}, b={saw_node_b})"
        );
    }

    /// Regression: installing a wider index must not re-bucket an older one.
    /// One cluster-wide width made `a`'s ids move when `b` (4 shards) was
    /// assigned, so acknowledged ids routed to shards that did not hold them.
    #[test]
    fn a_wider_index_does_not_change_an_older_index_routes() {
        let mut router = ShardRouter::new(1);
        for shard in 0..3u32 {
            router.assign("a", shard, &format!("n{shard}"));
        }
        let ids: Vec<String> = (0..10_000).map(|i| format!("doc-{i}")).collect();
        let before: Vec<(u32, Option<String>)> = ids
            .iter()
            .map(|id| {
                let (shard, node) = router.route_doc("a", id);
                (shard, node.map(str::to_string))
            })
            .collect();
        assert!(before.iter().all(|(s, n)| *s < 3 && n.is_some()));

        for shard in 0..4u32 {
            router.assign("b", shard, &format!("n{}", shard % 3));
        }
        assert_eq!(router.index_num_shards("a"), Some(3));
        assert_eq!(router.index_num_shards("b"), Some(4));

        let mut moved = 0;
        for (id, want) in ids.iter().zip(&before) {
            let (shard, node) = router.route_doc("a", id);
            if (shard, node.map(str::to_string)) != *want {
                moved += 1;
            }
        }
        assert_eq!(moved, 0, "{moved} of 10000 ids of `a` changed route");

        let mut seen = [0u32; 4];
        for id in &ids {
            let (shard, node) = router.route_doc("b", id);
            assert_eq!(shard, jump_hash(xxh3_64(id.as_bytes()), 4));
            assert!(node.is_some());
            seen[shard as usize] += 1;
        }
        assert!(seen.iter().all(|c| *c > 0), "b uses width 4: {seen:?}");

        router.clear_index("b");
        assert_eq!(router.index_num_shards("b"), None);
        assert_eq!(router.index_num_shards("a"), Some(3));
    }

    /// A planned index whose shard 2 has no primary routes that shard as
    /// `Unowned`, not as "no plan, write locally".
    #[test]
    fn a_planned_shard_with_no_primary_is_unowned() {
        let mut router = ShardRouter::new(1);
        router.assign("gap", 0, "n0");
        router.assign("gap", 1, "n1");
        router.assign("gap", 3, "n3");
        assert_eq!(router.index_num_shards("gap"), Some(4));
        let id = (0..10_000)
            .map(|i| format!("doc-{i}"))
            .find(|id| jump_hash(xxh3_64(id.as_bytes()), 4) == 2)
            .expect("an id hashing to shard 2");
        assert_eq!(router.route("gap", &id), Route::Unowned { shard: 2 });
        assert_eq!(router.route_doc("gap", &id), (2, None));

        let (shard, node) = router.route_doc("gap", "doc-x");
        if shard != 2 {
            assert!(node.is_some());
        }
        assert!(matches!(router.route("other", &id), Route::NoPlan { .. }));
    }

    #[test]
    fn in_sync_replaces_the_read_list_and_leaves_placement_alone() {
        let mut router = ShardRouter::new(1);
        router.assign("orders", 0, "n1");
        router.add_replica("orders", 0, "n2");
        router.add_replica("orders", 0, "n3");
        assert_eq!(
            router.get_replicas("orders", 0),
            vec!["n2".to_string(), "n3".to_string()]
        );

        router.set_in_sync("orders", 0, &["n3".into(), "n1".into(), "n3".into()]);
        assert_eq!(router.get_replicas("orders", 0), vec!["n3".to_string()]);
        assert_eq!(
            router.placed_replicas("orders", 0),
            vec!["n2".to_string(), "n3".to_string()]
        );

        router.set_in_sync("missing", 0, &["n2".into()]);
        assert!(router.get_replicas("missing", 0).is_empty());
    }
}
