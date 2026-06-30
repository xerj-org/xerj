//! Integration tests for WAL replication (Milestone 3.4).

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use xerj_cluster::node::in_memory::{InMemoryBus, InMemoryTransport};
use xerj_cluster::node::ClusterTransport;
use xerj_cluster::raft::RaftMessage;
use xerj_cluster::replication::{
    ReplicationEntry, ReplicationError, ReplicationMode, ReplicationOp, WalReplicator,
};
use xerj_cluster::router::ShardRouter;

// ── Test transports ───────────────────────────────────────────────────────────

/// A transport that succeeds for whitelisted node IDs and errors for all others.
struct PartialTransport {
    reachable: HashSet<String>,
}

impl PartialTransport {
    fn new(reachable: &[&str]) -> Self {
        PartialTransport {
            reachable: reachable.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[async_trait]
impl ClusterTransport for PartialTransport {
    async fn send(&self, to: &str, _msg: RaftMessage) -> anyhow::Result<()> {
        if self.reachable.contains(to) {
            Ok(())
        } else {
            Err(anyhow::anyhow!("node {to} is unreachable (simulated failure)"))
        }
    }

    async fn recv(&self) -> anyhow::Result<(String, RaftMessage)> {
        futures::future::pending().await
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_entry(index: &str, shard: u32, seq_no: u64) -> ReplicationEntry {
    ReplicationEntry {
        index: index.to_string(),
        shard,
        seq_no,
        operation: ReplicationOp::Index {
            doc_id: format!("doc-{seq_no}"),
            source_json: r#"{"title":"test document"}"#.to_string(),
        },
    }
}

fn router_with_replicas(
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

// ── Test 1: Async replication ─────────────────────────────────────────────────

/// Fire-and-forget: returns immediately without waiting for replica ACKs.
#[tokio::test]
async fn test_async_replication() {
    let bus = InMemoryBus::new();
    let transport: Arc<dyn ClusterTransport> =
        Arc::new(InMemoryTransport::new("primary".to_string(), bus.clone()).await);
    let _replica = InMemoryTransport::new("replica-1".to_string(), bus.clone()).await;

    let router = router_with_replicas("idx", "primary", &["replica-1"]);
    let replicator = WalReplicator::new(
        ReplicationMode::Async,
        transport,
        router,
        "primary".to_string(),
    );

    let result = replicator
        .replicate(make_entry("idx", 0, 1))
        .await
        .expect("async should succeed");

    // Async mode never waits — ack_count is always 0.
    assert_eq!(result.ack_count, 0, "async mode should not count ACKs");
}

// ── Test 2: Sync replication ──────────────────────────────────────────────────

/// Sync mode waits for the configured number of replicas to ACK.
#[tokio::test]
async fn test_sync_replication() {
    // Both replicas are reachable.
    let transport: Arc<dyn ClusterTransport> =
        Arc::new(PartialTransport::new(&["replica-1", "replica-2"]));
    let router = router_with_replicas("idx", "primary", &["replica-1", "replica-2"]);

    let replicator = WalReplicator::new(
        ReplicationMode::Sync { min_replicas: 2 },
        transport,
        router,
        "primary".to_string(),
    );

    let result = replicator
        .replicate(make_entry("idx", 0, 5))
        .await
        .expect("sync should succeed");

    assert_eq!(result.ack_count, 2, "both replicas must ACK");
}

/// Sync mode returns an error when fewer replicas are reachable than required.
#[tokio::test]
async fn test_sync_replication_insufficient_replicas() {
    // Only replica-1 is reachable; replica-2 will return an error.
    let transport: Arc<dyn ClusterTransport> =
        Arc::new(PartialTransport::new(&["replica-1"]));
    let router = router_with_replicas("idx", "primary", &["replica-1", "replica-2"]);

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
        matches!(err, ReplicationError::InsufficientReplicas { needed: 2, got: 1 }),
        "unexpected error: {err}"
    );
}

// ── Test 3: Quorum replication ────────────────────────────────────────────────

/// Quorum mode: majority (2 of 3) of replicas must ACK.
#[tokio::test]
async fn test_quorum_replication() {
    // replica-3 will error — only 2 of 3 reachable.
    let transport: Arc<dyn ClusterTransport> =
        Arc::new(PartialTransport::new(&["replica-1", "replica-2"]));

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

    // 2 of 3 replicas ACKed → majority (floor(3/2)+1 = 2) satisfied.
    assert_eq!(result.ack_count, 2, "2 of 3 replicas should ACK");
}

/// Quorum fails when fewer than majority respond.
#[tokio::test]
async fn test_quorum_replication_fails_below_majority() {
    // Only 1 of 3 replicas is reachable.
    let transport: Arc<dyn ClusterTransport> =
        Arc::new(PartialTransport::new(&["replica-1"]));

    let mut router = ShardRouter::new(8);
    router.assign("idx", 0, "primary");
    router.add_replica("idx", 0, "replica-1");
    router.add_replica("idx", 0, "replica-2");
    router.add_replica("idx", 0, "replica-3");
    let router = Arc::new(router);

    let replicator = WalReplicator::new(
        ReplicationMode::Quorum,
        transport,
        router,
        "primary".to_string(),
    );

    let err = replicator
        .replicate(make_entry("idx", 0, 11))
        .await
        .expect_err("quorum should fail");

    assert!(
        matches!(err, ReplicationError::QuorumNotReached { .. }),
        "expected QuorumNotReached, got: {err}"
    );
}

// ── Test 4: ReplicationEntry serialisation ────────────────────────────────────

/// Round-trip serialise every combination of ReplicationEntry + ReplicationOp.
#[test]
fn test_replication_entry_serialization() {
    let entries: Vec<ReplicationEntry> = vec![
        ReplicationEntry {
            index: "products".to_string(),
            shard: 0,
            seq_no: 1,
            operation: ReplicationOp::Index {
                doc_id: "prod-1".to_string(),
                source_json: r#"{"name":"widget","price":9.99}"#.to_string(),
            },
        },
        ReplicationEntry {
            index: "orders".to_string(),
            shard: 3,
            seq_no: 999,
            operation: ReplicationOp::Delete {
                doc_id: "order-42".to_string(),
            },
        },
        // Unicode content should survive the round-trip.
        ReplicationEntry {
            index: "интернет".to_string(),
            shard: 7,
            seq_no: u64::MAX,
            operation: ReplicationOp::Index {
                doc_id: "doc-\u{1F600}".to_string(),
                source_json: r#"{"emoji":"😀"}"#.to_string(),
            },
        },
    ];

    for entry in &entries {
        let json = serde_json::to_string(entry).expect("serialize entry");
        let decoded: ReplicationEntry =
            serde_json::from_str(&json).expect("deserialize entry");

        assert_eq!(entry.index, decoded.index, "index mismatch");
        assert_eq!(entry.shard, decoded.shard, "shard mismatch");
        assert_eq!(entry.seq_no, decoded.seq_no, "seq_no mismatch");

        match (&entry.operation, &decoded.operation) {
            (
                ReplicationOp::Index { doc_id: a, source_json: sa },
                ReplicationOp::Index { doc_id: b, source_json: sb },
            ) => {
                assert_eq!(a, b, "doc_id mismatch in Index");
                assert_eq!(sa, sb, "source_json mismatch");
            }
            (ReplicationOp::Delete { doc_id: a }, ReplicationOp::Delete { doc_id: b }) => {
                assert_eq!(a, b, "doc_id mismatch in Delete");
            }
            _ => panic!("operation variant changed after round-trip"),
        }
    }
}

/// ReplicationMode serialises for all variants.
#[test]
fn test_replication_mode_serialization() {
    let modes = vec![
        ReplicationMode::Async,
        ReplicationMode::Sync { min_replicas: 1 },
        ReplicationMode::Sync { min_replicas: 3 },
        ReplicationMode::Quorum,
    ];
    for mode in &modes {
        let json = serde_json::to_string(mode).expect("serialize mode");
        let _: ReplicationMode = serde_json::from_str(&json).expect("deserialize mode");
    }
}

// ── Test 5: No replicas — all modes succeed vacuously ────────────────────────

/// When there are no replicas configured, all modes should succeed without error.
#[tokio::test]
async fn test_replication_no_replicas() {
    // Transport that always succeeds (no replicas to send to anyway).
    let transport: Arc<dyn ClusterTransport> = Arc::new(PartialTransport::new(&[]));

    let mut router = ShardRouter::new(8);
    router.assign("solo", 0, "primary");
    let router = Arc::new(router);

    for mode in [
        ReplicationMode::Async,
        ReplicationMode::Sync { min_replicas: 0 },
        ReplicationMode::Quorum,
    ] {
        let replicator = WalReplicator::new(
            mode.clone(),
            Arc::clone(&transport),
            Arc::clone(&router),
            "primary".to_string(),
        );
        let result = replicator.replicate(make_entry("solo", 0, 1)).await;
        assert!(result.is_ok(), "no-replica {mode:?} should succeed: {result:?}");
    }
}
