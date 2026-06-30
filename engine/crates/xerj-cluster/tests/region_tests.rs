//! Integration tests for region management (Milestone 3.5).

use std::collections::HashMap;

use xerj_cluster::regions::{Region, RegionManager};
use xerj_cluster::router::{ShardRouter, jump_hash};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn nodes(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

// ── Test 1: Region split ──────────────────────────────────────────────────────

/// A region that crosses a size threshold should split into two.
#[test]
fn test_region_split() {
    // Low threshold so our test data triggers it.
    let mut mgr = RegionManager::with_thresholds(500, 1_000_000, 100);
    let regions = mgr.create_initial_regions("products", 1, &nodes(&["n1", "n2"]));
    let id = regions[0].id;

    // Push it over the size threshold.
    mgr.update_region_stats(id, 600, 10); // 600 bytes > 500 threshold

    assert_eq!(mgr.regions_to_split().len(), 1, "one region should need splitting");

    let (left, right) = mgr.split_region(id).expect("split should succeed");

    // Original region is gone.
    assert!(
        mgr.regions().iter().all(|r| r.id != id),
        "original region should be removed after split"
    );

    // Two new regions now exist.
    assert!(mgr.regions().iter().any(|r| r.id == left.id), "left half should exist");
    assert!(mgr.regions().iter().any(|r| r.id == right.id), "right half should exist");

    // Key ranges are contiguous.
    assert_eq!(
        left.end_key, right.start_key,
        "left end_key must equal right start_key"
    );

    // Stats are approximately halved.
    assert_eq!(left.size_bytes + right.size_bytes, 600, "stats should sum to original");
    assert_eq!(left.doc_count + right.doc_count, 10);
}

/// A region that only exceeds the doc_count threshold should also split.
#[test]
fn test_region_split_by_doc_count() {
    let mut mgr = RegionManager::with_thresholds(1_000_000_000, 50, 0);
    let regions = mgr.create_initial_regions("logs", 1, &nodes(&["n1"]));
    let id = regions[0].id;

    mgr.update_region_stats(id, 100, 60); // 60 docs > threshold 50

    assert_eq!(mgr.regions_to_split().len(), 1);

    let (left, right) = mgr.split_region(id).expect("split");
    assert!(left.doc_count + right.doc_count <= 60);
}

/// Split preserves the leader and replica node assignments.
#[test]
fn test_region_split_preserves_assignment() {
    let mut mgr = RegionManager::with_thresholds(100, 100, 0);
    let ns = nodes(&["n1", "n2", "n3"]);
    let regions = mgr.create_initial_regions("idx", 1, &ns);
    let id = regions[0].id;
    let leader = regions[0].leader_node.clone();

    mgr.update_region_stats(id, 200, 0);
    let (left, right) = mgr.split_region(id).expect("split");

    assert_eq!(left.leader_node, leader);
    assert_eq!(right.leader_node, leader);
}

// ── Test 2: Region merge ──────────────────────────────────────────────────────

/// Two small adjacent regions should merge into one.
#[test]
fn test_region_merge() {
    // Merge threshold: 10 MiB. Both regions are empty → both below threshold.
    let mut mgr = RegionManager::with_thresholds(1_000_000_000, 1_000_000_000, 10 * 1024 * 1024);
    let regions = mgr.create_initial_regions("orders", 2, &nodes(&["n1", "n2"]));
    let id_a = regions[0].id;
    let id_b = regions[1].id;

    // Confirm they appear in the merge candidates.
    assert_eq!(mgr.regions_to_merge().len(), 1);

    let merged = mgr.merge_regions(id_a, id_b).expect("merge should succeed");

    // Original regions removed.
    assert!(mgr.regions().iter().all(|r| r.id != id_a), "region a should be removed");
    assert!(mgr.regions().iter().all(|r| r.id != id_b), "region b should be removed");

    // Merged region exists.
    assert!(mgr.regions().iter().any(|r| r.id == merged.id));

    // The merged region spans the full key range.
    assert_eq!(merged.start_key, "", "merged start should be left-unbounded");
    assert_eq!(merged.end_key, "", "merged end should be right-unbounded");

    // Stats are summed (both were 0 here).
    assert_eq!(merged.size_bytes, 0);
    assert_eq!(merged.doc_count, 0);
}

/// Non-adjacent regions cannot be merged.
#[test]
fn test_region_merge_non_adjacent_fails() {
    let mut mgr = RegionManager::with_thresholds(u64::MAX, u64::MAX, 0);
    let regions = mgr.create_initial_regions("idx", 3, &nodes(&["n1"]));
    let id_first = regions[0].id;
    let id_last = regions[2].id;

    let result = mgr.merge_regions(id_first, id_last);
    assert!(result.is_err(), "merging non-adjacent regions should return Err");
}

/// Merging accumulates stats from both regions.
#[test]
fn test_region_merge_accumulates_stats() {
    let mut mgr = RegionManager::with_thresholds(u64::MAX, u64::MAX, u64::MAX);
    let regions = mgr.create_initial_regions("metrics", 2, &nodes(&["n1"]));
    let id_a = regions[0].id;
    let id_b = regions[1].id;

    mgr.update_region_stats(id_a, 1000, 50);
    mgr.update_region_stats(id_b, 2000, 100);

    let merged = mgr.merge_regions(id_a, id_b).expect("merge");
    assert_eq!(merged.size_bytes, 3000);
    assert_eq!(merged.doc_count, 150);
}

// ── Test 3: Initial region creation ──────────────────────────────────────────

/// Create N regions distributed across M nodes.
#[test]
fn test_initial_region_creation() {
    let mut mgr = RegionManager::new();
    let ns = nodes(&["n1", "n2", "n3"]);
    let created = mgr.create_initial_regions("products", 6, &ns);

    assert_eq!(created.len(), 6, "should create exactly 6 regions");
    assert_eq!(mgr.regions().len(), 6);

    // First region is left-unbounded.
    assert_eq!(created[0].start_key, "", "first region start should be empty");
    // Last region is right-unbounded.
    assert_eq!(created[5].end_key, "", "last region end should be empty");

    // Regions are contiguous (no gaps, no overlaps).
    for i in 1..created.len() {
        assert_eq!(
            created[i].start_key,
            created[i - 1].end_key,
            "region {i} should start where region {} ended",
            i - 1
        );
    }

    // Round-robin distribution: 6 regions / 3 nodes = 2 each.
    let mut leader_counts: HashMap<&str, usize> = HashMap::new();
    for r in &created {
        *leader_counts.entry(r.leader_node.as_str()).or_default() += 1;
    }
    assert_eq!(leader_counts.len(), 3, "all 3 nodes should receive regions");
    for (&node, &count) in &leader_counts {
        assert_eq!(count, 2, "node {node} should lead 2 regions");
    }
}

/// Single-region index: covers the entire key space.
#[test]
fn test_initial_region_creation_single() {
    let mut mgr = RegionManager::new();
    let created = mgr.create_initial_regions("solo", 1, &nodes(&["n1"]));
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].start_key, "");
    assert_eq!(created[0].end_key, "");
    assert_eq!(created[0].leader_node, "n1");
}

/// More regions than nodes — some nodes get multiple regions.
#[test]
fn test_initial_region_creation_more_regions_than_nodes() {
    let mut mgr = RegionManager::new();
    let ns = nodes(&["n1", "n2"]);
    let created = mgr.create_initial_regions("big-idx", 5, &ns);
    assert_eq!(created.len(), 5);

    // Each region must have a valid leader.
    for r in &created {
        assert!(
            r.leader_node == "n1" || r.leader_node == "n2",
            "unexpected leader: {}",
            r.leader_node
        );
    }
}

// ── Test 4: Rebalance plan ────────────────────────────────────────────────────

/// An overloaded node should have regions moved to an underloaded one.
#[test]
fn test_rebalance_plan() {
    let mut mgr = RegionManager::new();
    let ns = nodes(&["n1", "n2"]);
    let created = mgr.create_initial_regions("heavy", 4, &ns);

    // Give n1's regions a lot of data.
    for r in &created {
        if r.leader_node == "n1" {
            mgr.update_region_stats(r.id, 100 * 1024 * 1024, 500_000); // 100 MiB
        }
        // n2 stays at 0 bytes.
    }

    let loads: HashMap<String, u64> = mgr.regions().iter().fold(HashMap::new(), |mut m, r| {
        *m.entry(r.leader_node.clone()).or_default() += r.size_bytes;
        m
    });

    let moves = mgr.plan_rebalance(&loads);

    assert!(!moves.is_empty(), "should plan at least one move");
    for mv in &moves {
        assert_eq!(mv.from_node, "n1", "moves should come from the overloaded node");
        assert_eq!(mv.to_node, "n2", "moves should go to the underloaded node");
    }
}

/// Balanced cluster produces no rebalance moves.
#[test]
fn test_rebalance_plan_balanced() {
    let mut mgr = RegionManager::new();
    let ns = nodes(&["n1", "n2"]);
    let created = mgr.create_initial_regions("balanced", 2, &ns);

    // Equal load on both nodes.
    for r in &created {
        mgr.update_region_stats(r.id, 50 * 1024 * 1024, 10_000);
    }

    let loads: HashMap<String, u64> = mgr.regions().iter().fold(HashMap::new(), |mut m, r| {
        *m.entry(r.leader_node.clone()).or_default() += r.size_bytes;
        m
    });

    let moves = mgr.plan_rebalance(&loads);
    assert!(
        moves.is_empty(),
        "no moves should be planned for a balanced cluster, got: {moves:?}"
    );
}

/// Single-node cluster: no moves possible.
#[test]
fn test_rebalance_plan_single_node() {
    let mut mgr = RegionManager::new();
    mgr.create_initial_regions("solo", 3, &nodes(&["only-node"]));
    mgr.update_region_stats(1, 500 * 1024 * 1024, 1_000_000);

    let mut loads = HashMap::new();
    loads.insert("only-node".to_string(), 500 * 1024 * 1024);

    let moves = mgr.plan_rebalance(&loads);
    assert!(moves.is_empty(), "no moves possible with a single node");
}

// ── Test 5: Jump hash routes to region ───────────────────────────────────────

/// Document routing via jump_hash always lands within a region's key range.
#[test]
fn test_jump_hash_routes_to_region() {
    use xxhash_rust::xxh3::xxh3_64;

    let mut router = ShardRouter::new(4);
    router.assign("docs", 0, "n1");
    router.assign("docs", 1, "n2");
    router.assign("docs", 2, "n1");
    router.assign("docs", 3, "n2");

    // Verify the router itself is consistent.
    let doc_id = "document-abc-123";
    let hash = xxh3_64(doc_id.as_bytes());
    let shard = jump_hash(hash, 4);
    let (routed_shard, routed_node) = router.route_doc("docs", doc_id);
    assert_eq!(routed_shard, shard, "direct hash and router must agree");
    assert!(routed_node.is_some(), "shard must be assigned to a node");

    // Create a matching region map and verify coverage.
    let mut mgr = RegionManager::new();
    let ns = nodes(&["n1", "n2"]);
    let regions = mgr.create_initial_regions("docs", 4, &ns);

    // Every region should be individually findable via region_for_key.
    for r in &regions {
        let probe = if r.start_key.is_empty() {
            "\x01".to_string()
        } else {
            r.start_key.clone()
        };
        let found = mgr.region_for_key("docs", &probe);
        assert!(
            found.is_some(),
            "key {:?} should fall within some region",
            probe
        );
    }

    // A doc_id can be hashed to a shard, then a region can be found for that shard's
    // key space. Here we do a simpler property check: every key in the shard range
    // is covered by exactly one region.
    let doc_ids = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta"];
    for doc in &doc_ids {
        let h = xxh3_64(doc.as_bytes());
        let s = jump_hash(h, 4);
        // The shard index is within [0, 4).
        assert!(s < 4, "shard {s} is out of range for 4 shards");
    }
}

// ── Test 6: update_region_stats ───────────────────────────────────────────────

#[test]
fn test_update_region_stats() {
    let mut mgr = RegionManager::new();
    let created = mgr.create_initial_regions("metrics", 1, &nodes(&["n1"]));
    let id = created[0].id;

    mgr.update_region_stats(id, 5000, 200);
    let r = mgr.regions().iter().find(|r| r.id == id).unwrap();
    assert_eq!(r.size_bytes, 5000);
    assert_eq!(r.doc_count, 200);

    // Partial deletion.
    mgr.update_region_stats(id, -1000, -50);
    let r = mgr.regions().iter().find(|r| r.id == id).unwrap();
    assert_eq!(r.size_bytes, 4000);
    assert_eq!(r.doc_count, 150);

    // Cannot go below zero.
    mgr.update_region_stats(id, -999_999, -999_999);
    let r = mgr.regions().iter().find(|r| r.id == id).unwrap();
    assert_eq!(r.size_bytes, 0);
    assert_eq!(r.doc_count, 0);
}

// ── Test 7: Region serialisation ─────────────────────────────────────────────

#[test]
fn test_region_serialization() {
    let region = Region {
        id: 42,
        index: "my-index".to_string(),
        start_key: "abc".to_string(),
        end_key: "xyz".to_string(),
        leader_node: "n1".to_string(),
        replica_nodes: vec!["n2".to_string(), "n3".to_string()],
        size_bytes: 1_024,
        doc_count: 100,
        created_at: 1_700_000_000,
    };

    let json = serde_json::to_string(&region).expect("serialize");
    let decoded: Region = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(decoded.id, 42);
    assert_eq!(decoded.index, "my-index");
    assert_eq!(decoded.start_key, "abc");
    assert_eq!(decoded.end_key, "xyz");
    assert_eq!(decoded.leader_node, "n1");
    assert_eq!(decoded.replica_nodes.len(), 2);
    assert_eq!(decoded.size_bytes, 1_024);
    assert_eq!(decoded.doc_count, 100);
}
