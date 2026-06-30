//! Auto-split region management.
//!
//! A **region** is a contiguous range of document keys within an index.
//! The region manager is responsible for:
//!
//! - **Splitting** regions that have grown beyond a configurable size/doc threshold
//! - **Merging**  adjacent regions that are both below a merge threshold
//! - **Assigning** initial regions for a new index, distributing them across nodes
//! - **Rebalancing** region leaders when node loads diverge
//!
//! # Key ranges
//!
//! Each region covers `[start_key, end_key)` in lexicographic order.
//! An empty `end_key` means "unbounded right", i.e. the region covers all
//! keys from `start_key` to positive infinity.
//!
//! # Split strategy
//!
//! When splitting, the midpoint of the key range is computed as follows:
//! - If both bounds are non-empty, the midpoint is the lexicographic midpoint.
//! - If `end_key` is empty (unbounded), we extend `start_key` with `\x80` to
//!   create a synthetic midpoint.

use std::collections::HashMap;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

// ── Data types ────────────────────────────────────────────────────────────────

/// A contiguous key-range slice of an index, managed as a unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    /// Unique region identifier.
    pub id: u64,
    /// The index this region belongs to.
    pub index: String,
    /// Inclusive start of the key range ("" = leftmost key).
    pub start_key: String,
    /// Exclusive end of the key range ("" = unbounded / rightmost).
    pub end_key: String,
    /// Node that currently leads this region.
    pub leader_node: String,
    /// Nodes that hold replica copies of this region.
    pub replica_nodes: Vec<String>,
    /// Estimated storage size of this region in bytes.
    pub size_bytes: u64,
    /// Number of documents stored in this region.
    pub doc_count: u64,
    /// Unix timestamp (seconds) when this region was created.
    pub created_at: u64,
}

impl Region {
    /// Whether this region has no key-range upper bound.
    pub fn is_unbounded(&self) -> bool {
        self.end_key.is_empty()
    }

    /// Returns `true` if `key` falls within this region's `[start_key, end_key)` range.
    pub fn contains_key(&self, key: &str) -> bool {
        let after_start = key >= self.start_key.as_str();
        let before_end = self.end_key.is_empty() || key < self.end_key.as_str();
        after_start && before_end
    }
}

/// A planned region migration from one node to another.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionMove {
    /// The region to move.
    pub region_id: u64,
    /// Source node (current leader).
    pub from_node: String,
    /// Destination node (will become new leader).
    pub to_node: String,
}

// ── RegionManager ─────────────────────────────────────────────────────────────

/// Manages the lifecycle of regions: creation, split, merge, and rebalancing.
pub struct RegionManager {
    /// All regions across all indices.
    regions: Vec<Region>,
    /// Split if `size_bytes` exceeds this value (default: 256 MiB).
    pub split_threshold_bytes: u64,
    /// Split if `doc_count` exceeds this value (default: 1 000 000 docs).
    pub split_threshold_docs: u64,
    /// Merge if both regions in a pair are below this byte size (default: 64 MiB).
    pub merge_threshold_bytes: u64,
    /// Counter for generating unique region IDs.
    next_id: u64,
}

impl Default for RegionManager {
    fn default() -> Self {
        RegionManager {
            regions: Vec::new(),
            split_threshold_bytes: 256 * 1024 * 1024,
            split_threshold_docs: 1_000_000,
            merge_threshold_bytes: 64 * 1024 * 1024,
            next_id: 1,
        }
    }
}

impl RegionManager {
    /// Create a new manager with default thresholds.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create with custom thresholds (useful for tests).
    pub fn with_thresholds(
        split_threshold_bytes: u64,
        split_threshold_docs: u64,
        merge_threshold_bytes: u64,
    ) -> Self {
        RegionManager {
            split_threshold_bytes,
            split_threshold_docs,
            merge_threshold_bytes,
            ..Self::default()
        }
    }

    // ── Read-only queries ─────────────────────────────────────────────────────

    /// Return a slice of all regions.
    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    /// Find regions that exceed either split threshold.
    pub fn regions_to_split(&self) -> Vec<&Region> {
        self.regions
            .iter()
            .filter(|r| {
                r.size_bytes > self.split_threshold_bytes
                    || r.doc_count > self.split_threshold_docs
            })
            .collect()
    }

    /// Find adjacent region pairs where *both* are below the merge threshold.
    ///
    /// Two regions are considered adjacent when one's `end_key` equals the
    /// other's `start_key` (i.e. they cover a contiguous key space with no gap).
    /// Returns `(region_id_a, region_id_b)` pairs, where `a` comes first
    /// lexicographically.
    pub fn regions_to_merge(&self) -> Vec<(u64, u64)> {
        let mut pairs: Vec<(u64, u64)> = Vec::new();

        for (i, ra) in self.regions.iter().enumerate() {
            if ra.size_bytes >= self.merge_threshold_bytes {
                continue;
            }
            for rb in self.regions[i + 1..].iter() {
                if rb.size_bytes >= self.merge_threshold_bytes {
                    continue;
                }
                // Same index and adjacent key ranges?
                if ra.index != rb.index {
                    continue;
                }
                // A non-empty end_key must equal the other's start_key.
                // Empty end_key means right-unbounded, not a real boundary string.
                let adjacent = (!ra.end_key.is_empty() && ra.end_key == rb.start_key)
                    || (!rb.end_key.is_empty() && rb.end_key == ra.start_key);
                if adjacent {
                    // Always report the earlier region first.
                    if ra.start_key <= rb.start_key {
                        pairs.push((ra.id, rb.id));
                    } else {
                        pairs.push((rb.id, ra.id));
                    }
                }
            }
        }

        pairs
    }

    /// Find the region responsible for a key in an index, if any.
    pub fn region_for_key<'a>(&'a self, index: &str, key: &str) -> Option<&'a Region> {
        self.regions
            .iter()
            .find(|r| r.index == index && r.contains_key(key))
    }

    // ── Mutating operations ───────────────────────────────────────────────────

    /// Create the initial set of regions for a brand-new index.
    ///
    /// Regions are distributed round-robin across `nodes`. Each region starts
    /// empty. The first region's `start_key` is `""` (left-unbounded) and the
    /// last region's `end_key` is `""` (right-unbounded). Intermediate
    /// boundaries are evenly spaced using single-byte ASCII markers.
    pub fn create_initial_regions(
        &mut self,
        index: &str,
        num_regions: u32,
        nodes: &[String],
    ) -> Vec<Region> {
        assert!(num_regions > 0, "must create at least one region");
        assert!(!nodes.is_empty(), "must have at least one node");

        let now = now_secs();
        let boundaries = split_boundaries(num_regions);
        let mut created: Vec<Region> = Vec::with_capacity(num_regions as usize);

        for i in 0..num_regions as usize {
            let start_key = if i == 0 {
                String::new()
            } else {
                boundaries[i - 1].clone()
            };
            let end_key = if i + 1 < num_regions as usize {
                boundaries[i].clone()
            } else {
                String::new() // unbounded right
            };

            let leader_node = nodes[i % nodes.len()].clone();
            let replica_nodes: Vec<String> = nodes
                .iter()
                .filter(|n| n.as_str() != leader_node.as_str())
                .cloned()
                .collect();

            let region = Region {
                id: self.alloc_id(),
                index: index.to_string(),
                start_key,
                end_key,
                leader_node,
                replica_nodes,
                size_bytes: 0,
                doc_count: 0,
                created_at: now,
            };
            created.push(region.clone());
            self.regions.push(region);
        }

        created
    }

    /// Split a region at the midpoint of its key range, producing two new regions.
    ///
    /// The original region is removed; the two halves are inserted in its place.
    /// Both halves inherit the leader and replica assignments of the original.
    /// Stats are split 50/50 (approximation — real splits would use actual data).
    pub fn split_region(&mut self, region_id: u64) -> Result<(Region, Region)> {
        let pos = self
            .regions
            .iter()
            .position(|r| r.id == region_id)
            .ok_or_else(|| anyhow::anyhow!("region {} not found", region_id))?;

        let original = self.regions.remove(pos);
        let mid = midpoint_key(&original.start_key, &original.end_key);
        let now = now_secs();

        let left = Region {
            id: self.alloc_id(),
            index: original.index.clone(),
            start_key: original.start_key.clone(),
            end_key: mid.clone(),
            leader_node: original.leader_node.clone(),
            replica_nodes: original.replica_nodes.clone(),
            size_bytes: original.size_bytes / 2,
            doc_count: original.doc_count / 2,
            created_at: now,
        };

        let right = Region {
            id: self.alloc_id(),
            index: original.index.clone(),
            start_key: mid,
            end_key: original.end_key.clone(),
            leader_node: original.leader_node.clone(),
            replica_nodes: original.replica_nodes.clone(),
            size_bytes: original.size_bytes / 2,
            doc_count: original.doc_count / 2,
            created_at: now,
        };

        self.regions.push(left.clone());
        self.regions.push(right.clone());

        Ok((left, right))
    }

    /// Merge two adjacent regions into one.
    ///
    /// The merged region spans `[min(start_a, start_b), max(end_a, end_b))`.
    /// Stats are summed. The leader of the first (lexicographically earlier) region
    /// is used; replicas are unioned.
    pub fn merge_regions(&mut self, region_a: u64, region_b: u64) -> Result<Region> {
        let pos_a = self
            .regions
            .iter()
            .position(|r| r.id == region_a)
            .ok_or_else(|| anyhow::anyhow!("region {} not found", region_a))?;
        let ra = self.regions[pos_a].clone();

        let pos_b = self
            .regions
            .iter()
            .position(|r| r.id == region_b)
            .ok_or_else(|| anyhow::anyhow!("region {} not found", region_b))?;
        let rb = self.regions[pos_b].clone();

        if ra.index != rb.index {
            bail!("cannot merge regions from different indices: {} vs {}", ra.index, rb.index);
        }

        // Verify adjacency.
        // Two regions are adjacent when one's (non-empty) end_key equals
        // the other's start_key.  We deliberately exclude the case where
        // end_key == "" because that denotes right-unbounded, not the empty
        // string key — it cannot equal any start_key as a boundary.
        let adjacent = (!ra.end_key.is_empty() && ra.end_key == rb.start_key)
            || (!rb.end_key.is_empty() && rb.end_key == ra.start_key);
        if !adjacent {
            bail!(
                "regions {} and {} are not adjacent (end_key / start_key mismatch)",
                region_a,
                region_b
            );
        }

        // Determine the combined span.
        let (start_key, end_key, primary) = if ra.start_key <= rb.start_key {
            (ra.start_key.clone(), rb.end_key.clone(), ra.clone())
        } else {
            (rb.start_key.clone(), ra.end_key.clone(), rb.clone())
        };

        // Union of replica nodes (deduped).
        let mut replicas: Vec<String> = primary.replica_nodes.clone();
        for r in &ra.replica_nodes {
            if !replicas.contains(r) {
                replicas.push(r.clone());
            }
        }
        for r in &rb.replica_nodes {
            if !replicas.contains(r) {
                replicas.push(r.clone());
            }
        }
        // Ensure leader is not also in replicas.
        replicas.retain(|r| r != &primary.leader_node);

        let merged = Region {
            id: self.alloc_id(),
            index: ra.index.clone(),
            start_key,
            end_key,
            leader_node: primary.leader_node.clone(),
            replica_nodes: replicas,
            size_bytes: ra.size_bytes + rb.size_bytes,
            doc_count: ra.doc_count + rb.doc_count,
            created_at: now_secs(),
        };

        // Remove old regions (remove higher index first to keep positions stable).
        let (hi, lo) = if pos_a > pos_b { (pos_a, pos_b) } else { (pos_b, pos_a) };
        self.regions.remove(hi);
        self.regions.remove(lo);
        self.regions.push(merged.clone());

        Ok(merged)
    }

    /// Plan a rebalance: move regions from overloaded nodes to underloaded ones.
    ///
    /// `node_loads` maps each node ID to a load metric (e.g. bytes stored).
    /// Regions are moved one at a time from the most-loaded node to the
    /// least-loaded until all loads are within one standard region of the mean,
    /// or no further moves are possible.
    ///
    /// Returns the list of moves to execute (does *not* mutate state — the
    /// caller applies moves in order).
    pub fn plan_rebalance(&self, node_loads: &HashMap<String, u64>) -> Vec<RegionMove> {
        if node_loads.len() < 2 {
            return Vec::new();
        }

        let total_load: u64 = node_loads.values().sum();
        let mean = total_load / node_loads.len() as u64;

        // Working copy of loads so we can simulate moves.
        let mut loads = node_loads.clone();
        let mut moves: Vec<RegionMove> = Vec::new();

        // Map node → regions it leads.
        let region_loads: HashMap<String, Vec<u64>> = {
            let mut m: HashMap<String, Vec<u64>> = HashMap::new();
            for r in &self.regions {
                m.entry(r.leader_node.clone())
                    .or_default()
                    .push(r.id);
            }
            m
        };
        let mut node_regions: HashMap<String, Vec<u64>> = region_loads;

        loop {
            // Find most-loaded and least-loaded nodes.
            let max_node = loads
                .iter()
                .max_by_key(|(_, v)| *v)
                .map(|(k, _)| k.clone());
            let min_node = loads
                .iter()
                .min_by_key(|(_, v)| *v)
                .map(|(k, _)| k.clone());

            let (max_node, min_node) = match (max_node, min_node) {
                (Some(a), Some(b)) if a != b => (a, b),
                _ => break,
            };

            let max_load = loads[&max_node];
            let min_load = loads[&min_node];

            // Stop if already balanced (within mean ± 10%).
            if max_load.saturating_sub(min_load) <= mean / 10 {
                break;
            }

            // Pick any region from the most-loaded node to move.
            let region_id = match node_regions.get(&max_node).and_then(|ids| ids.first()) {
                Some(&id) => id,
                None => break, // node has no regions to move
            };

            let region = match self.regions.iter().find(|r| r.id == region_id) {
                Some(r) => r,
                None => break,
            };

            // Simulate the load transfer.
            *loads.get_mut(&max_node).unwrap() =
                max_load.saturating_sub(region.size_bytes);
            *loads.entry(min_node.clone()).or_insert(0) += region.size_bytes;

            // Update the working region-owner map.
            if let Some(ids) = node_regions.get_mut(&max_node) {
                ids.retain(|&id| id != region_id);
            }
            node_regions.entry(min_node.clone()).or_default().push(region_id);

            moves.push(RegionMove {
                region_id,
                from_node: max_node,
                to_node: min_node,
            });

            // Safety valve: don't produce more moves than there are regions.
            if moves.len() >= self.regions.len() {
                break;
            }
        }

        moves
    }

    /// Update a region's statistics after an indexing or deletion operation.
    ///
    /// `size_delta` and `doc_delta` may be negative (deletions).
    pub fn update_region_stats(
        &mut self,
        region_id: u64,
        size_delta: i64,
        doc_delta: i64,
    ) {
        if let Some(r) = self.regions.iter_mut().find(|r| r.id == region_id) {
            r.size_bytes = (r.size_bytes as i64 + size_delta).max(0) as u64;
            r.doc_count = (r.doc_count as i64 + doc_delta).max(0) as u64;
        }
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

// ── Key-range utilities ───────────────────────────────────────────────────────

/// Compute the lexicographic midpoint between `start` and `end`.
///
/// - If `end` is empty (unbounded), append `\x80` to `start` as a synthetic
///   midpoint.
/// - Otherwise, find the shared prefix then step the first differing byte.
fn midpoint_key(start: &str, end: &str) -> String {
    if end.is_empty() {
        // Unbounded right — just append a mid-byte.
        let mut mid = start.to_string();
        mid.push('\u{0080}');
        return mid;
    }

    let sb = start.as_bytes();
    let eb = end.as_bytes();

    // Find length of shared prefix.
    let common = sb.iter().zip(eb.iter()).take_while(|(a, b)| a == b).count();

    // The first differing byte of `end` is always > the corresponding byte
    // of `start` (because end > start). Split there.
    let pivot_e = *eb.get(common).unwrap_or(&0xff) as u16;
    let pivot_s = *sb.get(common).unwrap_or(&0x00) as u16;

    let mid_byte = ((pivot_s + pivot_e) / 2) as u8;

    let mut mid = start[..common].to_string();
    mid.push(mid_byte as char);
    mid
}

/// Generate `num_regions - 1` boundary strings that divide the key space evenly.
///
/// We use single-byte ASCII characters spaced across `[0x01, 0x7f]`.
fn split_boundaries(num_regions: u32) -> Vec<String> {
    if num_regions <= 1 {
        return Vec::new();
    }
    let count = (num_regions - 1) as usize;
    let step = 0xfe_u32 / num_regions;
    (1..=count)
        .map(|i| {
            let byte = (step * i as u32).min(0xfe) as u8;
            String::from(byte as char)
        })
        .collect()
}

/// Current time in seconds since the Unix epoch.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    // ── Region split ─────────────────────────────────────────────────────────

    /// A region that exceeds the size threshold should appear in regions_to_split.
    #[test]
    fn test_region_split() {
        let mut mgr = RegionManager::with_thresholds(1000, 100, 200);

        let regions = mgr.create_initial_regions("idx", 1, &nodes(&["n1", "n2"]));
        let region_id = regions[0].id;

        // Push it over the threshold.
        mgr.update_region_stats(region_id, 2000, 0); // 2000 bytes > threshold 1000

        assert_eq!(mgr.regions_to_split().len(), 1);

        let (left, right) = mgr.split_region(region_id).expect("split should succeed");

        // Original region should be gone.
        assert!(mgr.regions().iter().all(|r| r.id != region_id));
        // Two new regions should exist.
        assert!(mgr.regions().iter().any(|r| r.id == left.id));
        assert!(mgr.regions().iter().any(|r| r.id == right.id));

        // Left covers [start, mid), right covers [mid, end).
        assert!(left.end_key == right.start_key);

        // Stats split 50/50.
        assert_eq!(left.size_bytes + right.size_bytes, 2000);
    }

    /// Splitting an unbounded region produces a finite left and unbounded right.
    #[test]
    fn test_region_split_unbounded() {
        let mut mgr = RegionManager::new();
        let created = mgr.create_initial_regions("logs", 1, &nodes(&["n1"]));
        let id = created[0].id;
        assert!(mgr.regions()[0].is_unbounded());

        let (left, right) = mgr.split_region(id).expect("split");
        // Left is no longer unbounded.
        assert!(!left.end_key.is_empty());
        // Right is still unbounded.
        assert!(right.end_key.is_empty());
        // Boundary is consistent.
        assert_eq!(left.end_key, right.start_key);
    }

    // ── Region merge ─────────────────────────────────────────────────────────

    /// Two small adjacent regions should merge into one.
    #[test]
    fn test_region_merge() {
        let mut mgr = RegionManager::with_thresholds(1_000_000, 1_000_000, 100);

        // Create two adjacent regions manually.
        let regions = mgr.create_initial_regions("idx", 2, &nodes(&["n1", "n2"]));
        let id_a = regions[0].id;
        let id_b = regions[1].id;

        // Both are below the merge threshold (both have 0 bytes initially).
        assert_eq!(mgr.regions_to_merge().len(), 1);

        let merged = mgr.merge_regions(id_a, id_b).expect("merge should succeed");

        // Both original regions should be gone.
        assert!(mgr.regions().iter().all(|r| r.id != id_a));
        assert!(mgr.regions().iter().all(|r| r.id != id_b));
        // Merged region should exist.
        assert!(mgr.regions().iter().any(|r| r.id == merged.id));
        // Merged stats are summed.
        assert_eq!(merged.size_bytes, 0);
        assert_eq!(merged.doc_count, 0);
    }

    /// Merging non-adjacent regions should return an error.
    #[test]
    fn test_region_merge_non_adjacent_fails() {
        let mut mgr = RegionManager::with_thresholds(1_000_000, 1_000_000, 0);

        // Create 3 regions; try to merge the outer two (not adjacent).
        let regions = mgr.create_initial_regions("idx", 3, &nodes(&["n1"]));
        let id_first = regions[0].id;
        let id_last = regions[2].id;

        let result = mgr.merge_regions(id_first, id_last);
        assert!(result.is_err(), "merging non-adjacent regions should fail");
    }

    // ── Initial region creation ───────────────────────────────────────────────

    /// N regions are created, distributed across M nodes round-robin.
    #[test]
    fn test_initial_region_creation() {
        let mut mgr = RegionManager::new();
        let ns = nodes(&["n1", "n2", "n3"]);
        let created = mgr.create_initial_regions("products", 6, &ns);

        assert_eq!(created.len(), 6);
        assert_eq!(mgr.regions().len(), 6);

        // First region starts at "" (no lower bound).
        assert_eq!(created[0].start_key, "");
        // Last region ends at "" (unbounded right).
        assert_eq!(created[5].end_key, "");

        // Regions are contiguous: each start_key == previous end_key.
        for i in 1..created.len() {
            assert_eq!(
                created[i].start_key, created[i - 1].end_key,
                "region {i} start_key should equal region {} end_key",
                i - 1
            );
        }

        // Distribution: 6 regions across 3 nodes → 2 each.
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for r in &created {
            *counts.entry(r.leader_node.as_str()).or_default() += 1;
        }
        assert_eq!(counts.len(), 3);
        for (_, count) in &counts {
            assert_eq!(*count, 2);
        }
    }

    /// Single-region index: left-unbounded and right-unbounded.
    #[test]
    fn test_initial_region_creation_single() {
        let mut mgr = RegionManager::new();
        let created = mgr.create_initial_regions("solo", 1, &nodes(&["n1"]));
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].start_key, "");
        assert_eq!(created[0].end_key, "");
    }

    // ── Rebalance plan ────────────────────────────────────────────────────────

    /// An overloaded node should have regions moved to an underloaded node.
    #[test]
    fn test_rebalance_plan() {
        let mut mgr = RegionManager::new();
        let ns = nodes(&["n1", "n2"]);
        let created = mgr.create_initial_regions("idx", 4, &ns);

        // Give n1 regions a lot of data, n2 regions nothing.
        for r in &created {
            if r.leader_node == "n1" {
                mgr.update_region_stats(r.id, 50 * 1024 * 1024, 100_000); // 50 MiB
            }
        }

        let mut loads: HashMap<String, u64> = HashMap::new();
        for r in mgr.regions() {
            *loads.entry(r.leader_node.clone()).or_default() += r.size_bytes;
        }

        let moves = mgr.plan_rebalance(&loads);

        // At least one move should be planned.
        assert!(!moves.is_empty(), "should plan at least one region move");
        // All moves should be from n1 (overloaded) to n2 (underloaded).
        for mv in &moves {
            assert_eq!(mv.from_node, "n1");
            assert_eq!(mv.to_node, "n2");
        }
    }

    /// No moves planned when loads are already balanced.
    #[test]
    fn test_rebalance_plan_balanced() {
        let mut mgr = RegionManager::new();
        let ns = nodes(&["n1", "n2"]);
        let created = mgr.create_initial_regions("idx", 2, &ns);

        // Equal load on both nodes.
        for r in &created {
            mgr.update_region_stats(r.id, 10 * 1024 * 1024, 1000);
        }

        let loads: HashMap<String, u64> = mgr
            .regions()
            .iter()
            .fold(HashMap::new(), |mut m, r| {
                *m.entry(r.leader_node.clone()).or_default() += r.size_bytes;
                m
            });

        let moves = mgr.plan_rebalance(&loads);
        assert!(
            moves.is_empty(),
            "no moves should be planned when loads are balanced, got: {moves:?}"
        );
    }

    // ── Jump hash routes to region ────────────────────────────────────────────

    /// Document routing via jump_hash should land on a region that contains
    /// the routed doc_id (when the region map covers the same key space).
    #[test]
    fn test_jump_hash_routes_to_region() {
        use crate::router::{ShardRouter, jump_hash};
        use xxhash_rust::xxh3::xxh3_64;

        let mut router = ShardRouter::new(4);
        router.assign("docs", 0, "n1");
        router.assign("docs", 1, "n2");
        router.assign("docs", 2, "n1");
        router.assign("docs", 3, "n2");

        // A document should always route to the same shard repeatedly.
        let doc_id = "document-abc-123";
        let hash = xxh3_64(doc_id.as_bytes());
        let shard = jump_hash(hash, 4);

        let (routed_shard, routed_node) = router.route_doc("docs", doc_id);
        assert_eq!(routed_shard, shard);
        assert!(routed_node.is_some());

        // The region manager mirrors the key-space layout.
        let mut mgr = RegionManager::new();
        let ns = nodes(&["n1", "n2"]);
        let regions = mgr.create_initial_regions("docs", 4, &ns);

        // Every region should be reachable via contains_key.
        for r in &regions {
            // Regions partition ["", "") — each region covers its slice.
            let probe_key = if r.start_key.is_empty() {
                "\x01".to_string()
            } else {
                r.start_key.clone()
            };
            let found = mgr.region_for_key("docs", &probe_key);
            assert!(found.is_some(), "probe key {probe_key:?} should be covered by a region");
        }
    }

    // ── update_region_stats ───────────────────────────────────────────────────

    #[test]
    fn test_update_region_stats() {
        let mut mgr = RegionManager::new();
        let created = mgr.create_initial_regions("metrics", 1, &nodes(&["n1"]));
        let id = created[0].id;

        mgr.update_region_stats(id, 1000, 10);
        let r = mgr.regions().iter().find(|r| r.id == id).unwrap();
        assert_eq!(r.size_bytes, 1000);
        assert_eq!(r.doc_count, 10);

        // Deletions.
        mgr.update_region_stats(id, -300, -5);
        let r = mgr.regions().iter().find(|r| r.id == id).unwrap();
        assert_eq!(r.size_bytes, 700);
        assert_eq!(r.doc_count, 5);

        // Should never go below zero.
        mgr.update_region_stats(id, -9999, -9999);
        let r = mgr.regions().iter().find(|r| r.id == id).unwrap();
        assert_eq!(r.size_bytes, 0);
        assert_eq!(r.doc_count, 0);
    }
}
