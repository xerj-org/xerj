//! Metadata commands proposed through a 3-node in-process ring commit on a
//! majority and apply on every member, including after the leader is stopped.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::watch;
use xerj_cluster::node::in_memory::{InMemoryBus, InMemoryTransport};
use xerj_cluster::runner::ClusterRunner;
use xerj_cluster::{
    ApplyEffect, ClusterCommand, ClusterHandle, ClusterNode, MetadataApplier, ProposeError,
};

struct RecordingApplier {
    events: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl MetadataApplier for RecordingApplier {
    async fn apply_metadata(&self, command: &ClusterCommand) -> ApplyEffect {
        let label = match command.as_metadata() {
            ClusterCommand::CreateIndex { name, .. } => format!("create:{name}"),
            ClusterCommand::DeleteIndex { name } => format!("delete:{name}"),
            other => format!("other:{other:?}"),
        };
        self.events.lock().expect("events").push(label);
        ApplyEffect::Applied
    }
}

struct Node {
    shutdown: watch::Sender<bool>,
    handle: ClusterHandle,
    events: Arc<Mutex<Vec<String>>>,
}

async fn boot() -> Vec<Node> {
    let bus = InMemoryBus::new();
    let ids = ["n1", "n2", "n3"];
    let mut nodes = Vec::new();
    for id in ids {
        let peers: Vec<String> = ids
            .iter()
            .filter(|peer| **peer != id)
            .map(|peer| (*peer).to_string())
            .collect();
        let transport = InMemoryTransport::new(id.to_string(), bus.clone()).await;
        let mut node = ClusterNode::new(id.to_string(), peers, Box::new(transport));
        let events = Arc::new(Mutex::new(Vec::new()));
        node.set_applier(Arc::new(RecordingApplier {
            events: Arc::clone(&events),
        }));
        let handle = node.handle();
        let (shutdown, shutdown_rx) = watch::channel(false);
        let mut runner = ClusterRunner::new(node, Duration::from_millis(5), shutdown_rx);
        tokio::spawn(async move {
            runner.run().await;
        });
        nodes.push(Node {
            shutdown,
            handle,
            events,
        });
    }
    nodes
}

async fn wait_leader(nodes: &[Node], dead: &[bool]) -> usize {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let leaders: Vec<usize> = nodes
            .iter()
            .enumerate()
            .filter(|(i, node)| !dead[*i] && node.handle.is_leader())
            .map(|(i, _)| i)
            .collect();
        if leaders.len() == 1 {
            return leaders[0];
        }
        if std::time::Instant::now() > deadline {
            panic!("timed out waiting for a single leader among the live nodes");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_event(node: &Node, label: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if node
            .events
            .lock()
            .expect("events")
            .iter()
            .any(|event| event == label)
        {
            return;
        }
        if std::time::Instant::now() > deadline {
            panic!(
                "timed out waiting for {label}; saw {:?}",
                node.events.lock().expect("events")
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn metadata_replicates_and_survives_leader_loss() {
    let nodes = boot().await;
    let mut dead = [false, false, false];
    let leader = wait_leader(&nodes, &dead).await;
    let follower = (0..3).find(|i| *i != leader).expect("follower");

    nodes[follower]
        .handle
        .propose(ClusterCommand::CreateIndex {
            name: "orders".into(),
            schema_json: r#"{"v":1,"kind":"index_create"}"#.into(),
        })
        .await
        .expect("create commits");

    for node in &nodes {
        wait_event(node, "create:orders").await;
    }

    let _ = nodes[leader].shutdown.send(true);
    dead[leader] = true;
    let next = wait_leader(&nodes, &dead).await;
    assert_ne!(
        next, leader,
        "the stopped node must not stay the only leader"
    );

    let proposer = (0..3).find(|i| !dead[*i] && *i != next).unwrap_or(next);
    nodes[proposer]
        .handle
        .propose(ClusterCommand::DeleteIndex {
            name: "orders".into(),
        })
        .await
        .expect("delete commits after the leader change");

    for (i, node) in nodes.iter().enumerate() {
        if dead[i] {
            continue;
        }
        wait_event(node, "delete:orders").await;
    }

    for node in &nodes {
        let _ = node.shutdown.send(true);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn propose_without_a_running_node_is_not_acknowledged() {
    let bus = InMemoryBus::new();
    let transport = InMemoryTransport::new("solo".into(), bus).await;
    let node = ClusterNode::new("solo".into(), vec!["other".into()], Box::new(transport));
    let handle = node.handle();
    handle.set_timeout(Duration::from_millis(200));
    // Keep the node so the proposal channel stays open. Nothing steps it,
    // so the command is never appended.
    let _keep = node;

    let err = handle
        .propose(ClusterCommand::CreateIndex {
            name: "orders".into(),
            schema_json: "{}".into(),
        })
        .await
        .expect_err("a write with no running raft loop must not succeed");
    assert!(
        matches!(err, ProposeError::Timeout | ProposeError::NoLeader),
        "{err:?}"
    );
}
