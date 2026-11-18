//! The file Raft log is what a restarted member and a member that starts
//! with an empty directory use to install committed metadata.
//!
//! Document bytes are not in this log. These tests stop at the metadata
//! state machine: the index and the in-sync set.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tempfile::TempDir;
use xerj_cluster::node::in_memory::{InMemoryBus, InMemoryTransport};
use xerj_cluster::{ApplyEffect, ClusterCommand, ClusterNode, MetadataApplier};

struct RecordingApplier {
    events: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl MetadataApplier for RecordingApplier {
    async fn apply_metadata(&self, command: &ClusterCommand) -> ApplyEffect {
        let label = match command.as_metadata() {
            ClusterCommand::CreateIndex { name, .. } => format!("create:{name}"),
            ClusterCommand::SetShardInSync { index, shard, .. } => {
                format!("insync:{index}:{shard}")
            }
            other => format!("other:{other:?}"),
        };
        self.events.lock().expect("events").push(label);
        ApplyEffect::Applied
    }
}

async fn open_node(id: &str, ids: &[&str], bus: &InMemoryBus, dir: &PathBuf) -> ClusterNode {
    let peers: Vec<String> = ids
        .iter()
        .filter(|peer| **peer != id)
        .map(|peer| (*peer).to_string())
        .collect();
    let transport = InMemoryTransport::new(id.to_string(), bus.clone()).await;
    ClusterNode::with_storage(id.to_string(), peers, Box::new(transport), dir)
        .expect("open raft log")
}

async fn boot(ids: &[&str], dirs: &[PathBuf]) -> Vec<ClusterNode> {
    let bus = InMemoryBus::new();
    let mut nodes = Vec::new();
    for (id, dir) in ids.iter().zip(dirs.iter()) {
        nodes.push(open_node(id, ids, &bus, dir).await);
    }
    nodes
}

fn fresh_dirs(n: usize) -> Vec<(TempDir, PathBuf)> {
    (0..n)
        .map(|_| {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().to_path_buf();
            (dir, path)
        })
        .collect()
}

fn summary(nodes: &[ClusterNode]) -> Vec<String> {
    nodes
        .iter()
        .map(|node| {
            format!(
                "{} state={} leader={:?} commit={} log={} indices={:?}",
                node.raft.id,
                node.raft.state(),
                node.leader_id(),
                node.raft.commit_index(),
                node.raft.log_len(),
                node.metadata.indices.keys().collect::<Vec<_>>()
            )
        })
        .collect()
}

/// One step on every member, run in parallel as separate machines would.
///
/// A step can block on fsync. Stepping members one after another adds those
/// waits up, and a slow disk then reads as a missed heartbeat on the member
/// that waits its turn, which a real cluster does not see.
async fn step_all(nodes: &mut [ClusterNode]) {
    tokio::task::block_in_place(|| {
        std::thread::scope(|scope| {
            for node in nodes.iter_mut() {
                let rt = tokio::runtime::Handle::current();
                scope.spawn(move || {
                    rt.block_on(node.step(Duration::from_millis(5)))
                        .expect("raft step");
                });
            }
        });
    });
}

async fn pump_until(nodes: &mut [ClusterNode], mut done: impl FnMut(&[ClusterNode]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        step_all(nodes).await;
        if done(nodes) {
            return;
        }
    }
    panic!("timed out: {:?}", summary(nodes));
}

/// Propose `cmd` on the current leader until `done` holds on every member.
///
/// A proposal is legitimately dropped when leadership moves before it
/// commits. It is proposed again only once it is provably dead: some member
/// has committed a different entry at its index. Re-proposing earlier (on a
/// timer, or because the new leader lacks it) can commit it twice, since a
/// later leader that does hold it would still commit it. The new leader's
/// term-start no-op is what makes the dead case show up promptly.
async fn propose_until(
    nodes: &mut [ClusterNode],
    cmd: ClusterCommand,
    mut done: impl FnMut(&[ClusterNode]) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    // (index, term) of the last proposal that was accepted.
    let mut pending: Option<(u64, u64)> = None;
    while Instant::now() < deadline {
        if done(nodes) {
            return;
        }
        let leader = nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.is_leader())
            .max_by_key(|(_, n)| n.raft.current_term())
            .map(|(i, _)| i);
        let dropped = match pending {
            None => true,
            Some((index, term)) => nodes.iter().any(|n| {
                n.raft.commit_index() >= index
                    && index > n.snapshot_index()
                    && n.raft.entry_term(index) != Some(term)
            }),
        };
        if let (true, Some(l)) = (dropped, leader) {
            let term = nodes[l].raft.current_term();
            if let Ok(index) = nodes[l].propose(cmd.clone()) {
                pending = Some((index, term));
            }
        }
        step_all(nodes).await;
    }
    panic!("proposal never applied: {:?}", summary(nodes));
}

fn has_index(node: &ClusterNode) -> bool {
    node.metadata.indices.contains_key("orders")
}

/// Create `orders` and mark n2 in sync for shard 0, on every member.
async fn create_orders(nodes: &mut [ClusterNode]) {
    propose_until(
        nodes,
        ClusterCommand::CreateIndex {
            name: "orders".into(),
            schema_json: "{}".into(),
        },
        |nodes| nodes.iter().all(has_index),
    )
    .await;
    propose_until(
        nodes,
        ClusterCommand::SetShardInSync {
            index: "orders".into(),
            shard: 0,
            nodes: vec!["n2".into()],
        },
        |nodes| nodes.iter().all(has_orders),
    )
    .await;
}

fn has_orders(node: &ClusterNode) -> bool {
    node.metadata.indices.contains_key("orders")
        && node
            .metadata
            .config
            .get("shard_in_sync:orders:0")
            .map(String::as_str)
            == Some("n2")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_replays_the_committed_index_and_in_sync_set() {
    let ids = ["n1", "n2", "n3"];
    let held = fresh_dirs(3);
    let paths: Vec<PathBuf> = held.iter().map(|(_, path)| path.clone()).collect();
    let mut nodes = boot(&ids, &paths).await;

    pump_until(&mut nodes, |nodes| {
        nodes.iter().filter(|n| n.is_leader()).count() == 1
    })
    .await;
    create_orders(&mut nodes).await;
    drop(nodes);

    let mut restored = boot(&ids, &paths).await;
    for node in &restored {
        assert!(
            !node.metadata.indices.contains_key("orders"),
            "opening the log must not apply it before replay"
        );
        assert!(
            node.raft.commit_index() >= 2,
            "commit index must be on disk"
        );
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    for node in &mut restored {
        node.set_applier(Arc::new(RecordingApplier {
            events: Arc::clone(&events),
        }));
        node.replay_committed().await;
        assert!(
            has_orders(node),
            "replay must restore metadata on {}",
            node.raft.id
        );
    }
    let events = events.lock().expect("events").clone();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.as_str() == "create:orders")
            .count(),
        3
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.as_str() == "insync:orders:0")
            .count(),
        3
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_member_installs_the_leader_log_and_keeps_it_across_restart() {
    let ids = ["n1", "n2", "n3"];
    let held = fresh_dirs(3);
    let paths: Vec<PathBuf> = held.iter().map(|(_, path)| path.clone()).collect();
    let bus = InMemoryBus::new();

    // n3 is in the membership but not started. Two of three is a majority.
    let mut nodes = vec![
        open_node("n1", &ids, &bus, &paths[0]).await,
        open_node("n2", &ids, &bus, &paths[1]).await,
    ];
    pump_until(&mut nodes, |nodes| {
        nodes.iter().filter(|n| n.is_leader()).count() == 1
    })
    .await;
    create_orders(&mut nodes).await;

    let events = Arc::new(Mutex::new(Vec::new()));
    let mut late = open_node("n3", &ids, &bus, &paths[2]).await;
    late.set_applier(Arc::new(RecordingApplier {
        events: Arc::clone(&events),
    }));
    nodes.push(late);
    pump_until(&mut nodes, |nodes| nodes.iter().all(has_orders)).await;
    assert!(
        nodes[2].raft.log_len() >= 2,
        "the empty member must have stored the leader's entries"
    );
    let caught_up = events.lock().expect("events").clone();
    assert!(
        caught_up.iter().any(|e| e == "create:orders"),
        "the applier runs as the empty member catches up: {caught_up:?}"
    );

    // Drop the late member and reopen only its directory. The other two
    // stay up so a missing disk log cannot be refilled before the assert.
    let late = nodes.pop().expect("n3");
    assert_eq!(late.raft.id, "n3");
    drop(late);
    let mut restarted = open_node("n3", &ids, &bus, &paths[2]).await;
    assert!(
        !restarted.metadata.indices.contains_key("orders"),
        "restart loads the log and does not apply it until replay"
    );
    restarted.replay_committed().await;
    assert!(
        has_orders(&restarted),
        "the caught-up log must restore metadata with the peers stopped from helping"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_compacted_prefix_installs_on_an_empty_member_and_survives_restart() {
    let ids = ["n1", "n2", "n3"];
    let held = fresh_dirs(3);
    let paths: Vec<PathBuf> = held.iter().map(|(_, path)| path.clone()).collect();
    let bus = InMemoryBus::new();
    let mut nodes = vec![
        open_node("n1", &ids, &bus, &paths[0]).await,
        open_node("n2", &ids, &bus, &paths[1]).await,
    ];
    pump_until(&mut nodes, |nodes| {
        nodes.iter().filter(|n| n.is_leader()).count() == 1
    })
    .await;
    create_orders(&mut nodes).await;
    // An election in between leaves the new leader's no-op in the log;
    // wait until every member has applied everything it holds.
    pump_until(&mut nodes, |nodes| {
        nodes
            .iter()
            .all(|n| n.raft.commit_index() == n.snapshot_index() + n.raft.log_len() as u64)
    })
    .await;

    for node in &mut nodes {
        assert!(node.compact_applied(), "{} did not compact", node.raft.id);
        assert_eq!(
            node.raft.log_len(),
            0,
            "{} still has log entries after compacting the prefix",
            node.raft.id
        );
        assert!(
            node.snapshot_index() >= 2,
            "{} snapshot does not cover both entries",
            node.raft.id
        );
    }
    let leader = nodes.iter().position(|n| n.is_leader()).unwrap_or(0);
    let leader_id = nodes[leader].raft.id.clone();
    let leader_path = if leader_id == "n1" {
        paths[0].clone()
    } else {
        paths[1].clone()
    };
    let flog = xerj_cluster::raft_log::FileRaftLog::open(&leader_path).expect("reopen log");
    assert_eq!(flog.len(), 0, "raft.log must not keep the compacted prefix");
    assert!(
        flog.snapshot_bytes().is_some(),
        "the prefix has to be in snapshot.meta"
    );
    drop(flog);

    let events = Arc::new(Mutex::new(Vec::new()));
    let mut late = open_node("n3", &ids, &bus, &paths[2]).await;
    late.set_applier(Arc::new(RecordingApplier {
        events: Arc::clone(&events),
    }));
    nodes.push(late);
    pump_until(&mut nodes, |nodes| nodes.iter().all(has_orders)).await;
    let late = nodes.last().expect("n3");
    assert_eq!(late.raft.id, "n3");
    assert!(
        late.snapshot_index() >= 2,
        "n3 must install the snapshot; copying the log cannot work, it was compacted"
    );
    let caught_up = events.lock().expect("events").clone();
    assert!(
        caught_up.iter().any(|e| e == "create:orders"),
        "applying the snapshot must run the create applier: {caught_up:?}"
    );
    assert!(
        caught_up.iter().any(|e| e == "insync:orders:0"),
        "applying the snapshot must run the in-sync applier: {caught_up:?}"
    );

    propose_until(
        &mut nodes,
        ClusterCommand::UpdateConfig {
            key: "after".into(),
            value: "1".into(),
        },
        |nodes| {
            nodes
                .iter()
                .all(|node| node.metadata.config.get("after").map(String::as_str) == Some("1"))
        },
    )
    .await;

    let late = nodes.pop().expect("n3");
    assert_eq!(late.raft.id, "n3");
    drop(late);
    let mut restarted = open_node("n3", &ids, &bus, &paths[2]).await;
    restarted.replay_committed().await;
    assert!(
        has_orders(&restarted),
        "restart must restore the compacted prefix from snapshot.meta"
    );
    assert_eq!(
        restarted.metadata.config.get("after").map(String::as_str),
        Some("1"),
        "restart must also replay the entry that landed after the snapshot"
    );
}
