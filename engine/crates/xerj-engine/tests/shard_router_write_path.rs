//! V4 M5.2 acceptance test — shard router is wired into the engine.
//!
//! Given/when/then from `ARCHITECTURE_V4_2026-04-14.md` § Appendix A
//! (reduced to what's testable inside a single process):
//!
//! GIVEN an Engine with the default single-node ShardRouter
//! WHEN I call route_write for a doc
//! THEN the router returns None (local ownership)
//! AND subsequent writes go to the local engine
//!
//! GIVEN an Engine whose router has been updated with a peer
//! assignment pointing a shard at a remote node
//! WHEN I call route_write for a doc that hashes to that shard
//! THEN the router returns Some(remote_node_id)
//! AND the caller knows to forward the write via cluster transport
//!
//! This proves the wiring without actually running a multi-node
//! cluster — the remote transport hop is tested separately in
//! `crates/xerj-cluster/tests/transport_tests.rs`.

use xerj_common::config::Config;
use xerj_engine::Engine;

#[tokio::test]
async fn single_node_router_always_local() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.server.data_dir = tmp.path().to_string_lossy().to_string();
    let engine = Engine::new(config).unwrap();

    // Default: num_shards = 1, no peer assignments, so every doc
    // resolves to "local" which means None (handle locally).
    for id in &["1", "abc", "zebra-99", "🦓"] {
        assert_eq!(
            engine.route_write("my-index", id),
            None,
            "single-node router must resolve doc {id:?} to local"
        );
    }
    assert_eq!(engine.local_node_id(), "local");
}

#[tokio::test]
async fn router_forwards_remote_shards() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.server.data_dir = tmp.path().to_string_lossy().to_string();
    let engine = Engine::new(config).unwrap();

    // Simulate a 4-shard cluster by replacing the router.  Without
    // a public setter we have to use the Arc<RwLock<...>> handle
    // directly, which is the same path the Raft commit handler will
    // use in M5.3.
    {
        let mut router = engine.shard_router.write();
        *router = xerj_cluster::router::ShardRouter::new(4);
        router.assign("my-index", 0, "local");
        router.assign("my-index", 1, "peer-b");
        router.assign("my-index", 2, "local");
        router.assign("my-index", 3, "peer-c");
    }

    // Exercise the router for 1000 doc ids and verify that every
    // shard is non-empty and every remote shard yields the right
    // peer name.
    let mut counts = std::collections::HashMap::<Option<String>, usize>::new();
    for i in 0..1000 {
        let id = format!("doc-{i}");
        let target = engine.route_write("my-index", &id);
        *counts.entry(target).or_insert(0) += 1;
    }
    // Exactly three classifications: None (local), Some("peer-b"),
    // Some("peer-c").
    assert!(counts.contains_key(&None), "some docs must be local");
    assert!(
        counts.contains_key(&Some("peer-b".to_string())),
        "some docs must route to peer-b"
    );
    assert!(
        counts.contains_key(&Some("peer-c".to_string())),
        "some docs must route to peer-c"
    );
    // Jump-consistent hash should distribute reasonably evenly.
    // Shards 0 and 2 both map to "local", so None should get ~500;
    // peer-b and peer-c each get ~250.
    assert!(
        counts[&None] >= 400 && counts[&None] <= 600,
        "local (2 shards) got {} hits, expected 400..600",
        counts[&None]
    );
    assert!(
        counts[&Some("peer-b".to_string())] >= 150 && counts[&Some("peer-b".to_string())] <= 350,
        "peer-b got {} hits, expected 150..350",
        counts[&Some("peer-b".to_string())]
    );
    assert!(
        counts[&Some("peer-c".to_string())] >= 150 && counts[&Some("peer-c".to_string())] <= 350,
        "peer-c got {} hits, expected 150..350",
        counts[&Some("peer-c".to_string())]
    );
    // Every doc must be accounted for.
    let total: usize = counts.values().sum();
    assert_eq!(total, 1000);
}
