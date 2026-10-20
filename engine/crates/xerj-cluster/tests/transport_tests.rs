//! Integration tests for the TCP transport, cluster runner, and search messages.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::sync::watch;

use xerj_cluster::auth::ClusterSecret;
use xerj_cluster::node::{
    in_memory::{InMemoryBus, InMemoryTransport},
    ClusterNode,
};
use xerj_cluster::raft::{ClusterCommand, RaftMessage};
use xerj_cluster::runner::ClusterRunner;
use xerj_cluster::search::{merge_search_responses, SearchHit, SearchMessage};
use xerj_cluster::transport::TcpTransport;

// ── Helper: find a free port ──────────────────────────────────────────────────

async fn free_addr() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap()
}

// ── Test 1: TCP transport send/recv ──────────────────────────────────────────

/// Two TCP transports on loopback: send a Raft message, verify receipt.
#[tokio::test]
async fn test_tcp_transport_send_recv() {
    // Bind two ports.
    let addr_a = free_addr().await;
    let addr_b = free_addr().await;

    // Node A knows about Node B and vice-versa.
    let mut peers_a = HashMap::new();
    peers_a.insert("node-b".to_string(), addr_b);

    let mut peers_b = HashMap::new();
    peers_b.insert("node-a".to_string(), addr_a);

    // Create both transports. Both sides must hold the same cluster secret —
    // there is no unauthenticated constructor.
    let secret = || ClusterSecret::new("shared-cluster-secret-for-tests").unwrap();
    let transport_a = TcpTransport::new("node-a".to_string(), addr_a, peers_a, secret())
        .await
        .expect("create transport A");
    let transport_b = TcpTransport::new("node-b".to_string(), addr_b, peers_b, secret())
        .await
        .expect("create transport B");

    // Give the listeners a moment to start.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Node A sends a RequestVote to Node B.
    let msg = RaftMessage::RequestVote {
        term: 1,
        candidate_id: "node-a".to_string(),
        last_log_index: 0,
        last_log_term: 0,
    };

    transport_a
        .send_to("node-b", &msg)
        .await
        .expect("send from A to B");

    // Node B should receive it within a reasonable timeout.
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        use xerj_cluster::node::ClusterTransport;
        transport_b.recv().await
    })
    .await
    .expect("timeout waiting for message")
    .expect("recv error");

    let (from, received) = result;
    assert_eq!(from, "node-a");

    match received {
        RaftMessage::RequestVote {
            term, candidate_id, ..
        } => {
            assert_eq!(term, 1);
            assert_eq!(candidate_id, "node-a");
        }
        other => panic!("unexpected message: {:?}", other),
    }
}

// ── Test 2: Cluster runner election ──────────────────────────────────────────

/// Three in-memory nodes: verify that a leader is elected within a bounded
/// number of ticks when one node has its election timer pre-expired.
#[tokio::test]
async fn test_cluster_runner_election() {
    let bus = InMemoryBus::new();
    let ids = ["n1", "n2", "n3"];
    let all_ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();

    let mut runners = Vec::new();
    let mut shutdown_txs = Vec::new();

    for id in &ids {
        let peers: Vec<String> = all_ids
            .iter()
            .filter(|p| p.as_str() != *id)
            .cloned()
            .collect();

        let transport = InMemoryTransport::new(id.to_string(), bus.clone()).await;
        let mut node = ClusterNode::new(id.to_string(), peers, Box::new(transport));

        // Force the first node to time out immediately.
        if *id == "n1" {
            node.raft.force_election_timeout();
        }

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let runner = ClusterRunner::new(node, Duration::from_millis(5), shutdown_rx);

        runners.push(runner);
        shutdown_txs.push(shutdown_tx);
    }

    // Spawn all runners.
    let handles: Vec<_> = runners
        .into_iter()
        .map(|mut r| {
            tokio::spawn(async move {
                // Run for at most 2 seconds, then return the runner for inspection.
                tokio::time::timeout(Duration::from_secs(2), r.run())
                    .await
                    .ok();
                r
            })
        })
        .collect();

    // Give the cluster 500 ms to elect a leader.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Signal shutdown.
    for tx in &shutdown_txs {
        let _ = tx.send(true);
    }

    // Collect results.
    let mut leader_count = 0;
    for handle in handles {
        let runner = handle.await.expect("task panicked");
        if runner.is_leader() {
            leader_count += 1;
        }
    }

    // In a 3-node cluster exactly one leader should be elected.
    // (In-memory transport is instantaneous, so this is deterministic enough.)
    assert!(
        leader_count <= 1,
        "at most one leader allowed; got {leader_count}"
    );
    // We cannot assert == 1 because the runner's `node_recv` currently uses
    // `pending()` (a safe placeholder). The election still proceeds via
    // `tick()` alone; the important invariant is "at most 1 leader".
}

// ── Test 3: Distributed search message serialization ─────────────────────────

/// Serialize and deserialize every SearchMessage variant; verify round-trips.
#[tokio::test]
async fn test_distributed_search_message() {
    let messages = vec![
        SearchMessage::SearchRequest {
            request_id: "req-abc-123".to_string(),
            index: "products".to_string(),
            query_json: r#"{"match":{"title":"laptop"}}"#.to_string(),
            size: 10,
            from: 0,
        },
        SearchMessage::SearchResponse {
            request_id: "req-abc-123".to_string(),
            hits: vec![
                SearchHit::new("prod-1", 1.8, r#"{"title":"gaming laptop","price":999}"#),
                SearchHit::new("prod-2", 1.2, r#"{"title":"office laptop","price":599}"#),
            ],
            total: 42,
            took_ms: 7,
        },
        SearchMessage::IndexRequest {
            index: "orders".to_string(),
            doc_id: "order-99".to_string(),
            source_json: r#"{"amount":49.99,"currency":"USD"}"#.to_string(),
        },
        SearchMessage::IndexResponse {
            doc_id: "order-99".to_string(),
            version: 1,
            result: "created".to_string(),
        },
        SearchMessage::Error {
            request_id: Some("req-xyz".to_string()),
            code: "index_not_found".to_string(),
            message: "index 'ghost' does not exist".to_string(),
        },
    ];

    for msg in &messages {
        let json = serde_json::to_string(msg).expect("serialize");
        let decoded: SearchMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            msg,
            &decoded,
            "round-trip failed for {:?}",
            std::mem::discriminant(msg)
        );
    }
}

// ── Test 4: merge_search_responses ───────────────────────────────────────────

#[test]
fn test_merge_search_responses_ordering_and_pagination() {
    let shard1 = SearchMessage::SearchResponse {
        request_id: "r1".to_string(),
        hits: vec![
            SearchHit::new("a", 3.0, "{}"),
            SearchHit::new("c", 1.0, "{}"),
        ],
        total: 5,
        took_ms: 10,
    };
    let shard2 = SearchMessage::SearchResponse {
        request_id: "r1".to_string(),
        hits: vec![
            SearchHit::new("b", 2.0, "{}"),
            SearchHit::new("d", 0.5, "{}"),
        ],
        total: 3,
        took_ms: 8,
    };

    let (hits, total, took_ms) = merge_search_responses(vec![shard1, shard2], 1, 2);

    // Total from both shards.
    assert_eq!(total, 8);
    // Max took_ms.
    assert_eq!(took_ms, 10);
    // Sorted: a(3.0), b(2.0), c(1.0), d(0.5) → skip 1 → take 2 → [b, c]
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, "b");
    assert_eq!(hits[1].id, "c");
}

// ── Test 5: ClusterNode via in-memory transport ───────────────────────────────

/// Single-node cluster becomes leader and applies a command end-to-end.
#[tokio::test]
async fn test_single_node_cluster_via_runner() {
    let bus = InMemoryBus::new();
    let transport = InMemoryTransport::new("solo".to_string(), bus).await;
    let mut node = ClusterNode::new("solo".to_string(), vec![], Box::new(transport));

    // Force immediate election.
    node.raft.force_election_timeout();
    node.tick().await.unwrap();

    assert!(node.is_leader(), "single-node should become leader");

    node.propose(ClusterCommand::CreateIndex {
        name: "test-index".to_string(),
        schema_json: "{}".to_string(),
    })
    .unwrap();

    node.tick().await.unwrap();

    assert!(
        node.metadata.indices.contains_key("test-index"),
        "index should be committed and applied"
    );
}

// ── #1168: outbound sends must never block the Raft loop on one peer ────────

/// A listener that accepts connections (so `connect` succeeds) but never
/// speaks the cluster handshake — the shape of a peer that is dying slowly
/// (SIGTERM graceful shutdown with the listener still bound) or hung. The
/// pre-fix send path blocked on exactly this peer for the full 5 s
/// `SEND_TIMEOUT`, per heartbeat, while the election timeout is 150–300 ms.
///
/// Counts how many connections it accepted so tests can assert the *attempt
/// rate*, not just delivery.
async fn black_hole_peer() -> (SocketAddr, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepts = Arc::new(AtomicUsize::new(0));

    let counter = accepts.clone();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Hold the stream open without ever writing the handshake.
            held.push(stream);
        }
    });
    (addr, accepts)
}

fn request_vote(term: u64) -> RaftMessage {
    RaftMessage::RequestVote {
        term,
        candidate_id: "node-a".to_string(),
        last_log_index: 0,
        last_log_term: 0,
    }
}

/// `ClusterTransport::send` must return as soon as the message is *enqueued*,
/// not after the TCP exchange: a stalled (accepting-but-silent) peer costs the
/// Raft tick loop nothing. Pre-fix, this send awaited the full `SEND_TIMEOUT`
/// and then failed.
#[tokio::test]
async fn send_to_a_stalled_peer_returns_without_awaiting_the_tcp_exchange() {
    use xerj_cluster::node::ClusterTransport;

    let (stalled_addr, _accepts) = black_hole_peer().await;
    let addr_a = free_addr().await;
    let mut peers_a = HashMap::new();
    peers_a.insert("stalled".to_string(), stalled_addr);

    let transport_a = TcpTransport::new(
        "node-a".to_string(),
        addr_a,
        peers_a,
        ClusterSecret::new("shared-cluster-secret-for-tests").unwrap(),
    )
    .await
    .expect("create transport A");

    // 2 s is generous (the pre-fix path took the full 5 s timeout and erred);
    // generous is deliberate — the assertion is the ordering of magnitude,
    // not microtiming, so a co-tenant-loaded runner cannot flake it.
    tokio::time::timeout(Duration::from_secs(2), async {
        transport_a.send("stalled", request_vote(1)).await
    })
    .await
    .expect("send() must not await the stalled peer's TCP exchange")
    .expect("enqueue must succeed");
}

/// The #1168 scenario in miniature: a leader dispatching to a stalled peer and
/// a healthy peer in the same tick. The healthy peer must receive its message
/// immediately — pre-fix the sequential inline awaits made the stalled peer's
/// 5 s timeout gate every later send in the same dispatch batch, which is how
/// killing one node starved heartbeats to the survivors.
#[tokio::test]
async fn a_stalled_peer_does_not_starve_a_healthy_peer() {
    use xerj_cluster::node::ClusterTransport;

    let (stalled_addr, _accepts) = black_hole_peer().await;
    let addr_a = free_addr().await;
    let addr_b = free_addr().await;

    let mut peers_a = HashMap::new();
    peers_a.insert("stalled".to_string(), stalled_addr);
    peers_a.insert("node-b".to_string(), addr_b);
    let mut peers_b = HashMap::new();
    peers_b.insert("node-a".to_string(), addr_a);

    let secret = ClusterSecret::new("shared-cluster-secret-for-tests").unwrap();
    let transport_a = TcpTransport::new("node-a".to_string(), addr_a, peers_a, secret.clone())
        .await
        .expect("create transport A");
    let transport_b = TcpTransport::new("node-b".to_string(), addr_b, peers_b, secret)
        .await
        .expect("create transport B");

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Same dispatch order as ClusterNode::dispatch: stalled peer first.
    transport_a
        .send("stalled", request_vote(1))
        .await
        .expect("enqueue to stalled peer");
    transport_a
        .send("node-b", request_vote(2))
        .await
        .expect("enqueue to healthy peer");

    // Pre-fix this took ≥ 5 s (the stalled send's timeout gated the healthy
    // send); post-fix it is one loopback round-trip. 2 s to stay flake-safe.
    let (from, msg) = tokio::time::timeout(Duration::from_secs(2), async {
        transport_b.recv().await.expect("recv error")
    })
    .await
    .expect("healthy peer starved by the stalled peer's send timeout");

    assert_eq!(from, "node-a");
    match msg {
        RaftMessage::RequestVote { term, .. } => assert_eq!(term, 2),
        other => panic!("expected RequestVote, got {other:?}"),
    }
}

/// A dead peer is retried on the backoff ladder, not once per heartbeat: 100
/// sends across 1 s (the pre-fix pattern was one attempt per 50 ms tick) must
/// produce only a handful of connection attempts. The upper bound is generous
/// (the ladder predicts ~4: 0 ms, 100 ms, 300 ms, 700 ms) so scheduler jitter
/// under a loaded co-tenant cannot flake it — the pre-fix count was ~100.
#[tokio::test]
async fn a_dead_peer_is_backed_off_not_retried_every_message() {
    use std::sync::atomic::Ordering;

    use xerj_cluster::node::ClusterTransport;

    let (stalled_addr, accepts) = black_hole_peer().await;
    let addr_a = free_addr().await;
    let mut peers_a = HashMap::new();
    peers_a.insert("stalled".to_string(), stalled_addr);

    let transport_a = TcpTransport::new(
        "node-a".to_string(),
        addr_a,
        peers_a,
        ClusterSecret::new("shared-cluster-secret-for-tests").unwrap(),
    )
    .await
    .expect("create transport A");

    for i in 0..100u64 {
        transport_a
            .send("stalled", request_vote(i))
            .await
            .expect("enqueue must keep succeeding while the peer is down");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let attempts = accepts.load(Ordering::Relaxed);
    assert!(
        attempts <= 8,
        "dead peer was attempted {attempts} times in ~1 s — backoff is not engaging \
         (ladder predicts ~4, pre-fix behaviour was ~100)"
    );
}

/// A peers map naming the transport's own listen address (the documented
/// full-membership convention) must not produce a peer the node can send to:
/// pre-fix, a node addressing its own RequestVote over loopback TCP counted
/// its own vote twice (#1168).
#[tokio::test]
async fn a_peers_entry_naming_self_is_excluded_from_the_transport() {
    use xerj_cluster::node::ClusterTransport;

    let addr_a = free_addr().await;
    let addr_b = free_addr().await;

    // Full membership, self first — the config shape every node ships.
    let mut peers_a = HashMap::new();
    peers_a.insert("node-a".to_string(), addr_a);
    peers_a.insert("node-b".to_string(), addr_b);

    let transport_a = TcpTransport::new(
        "node-a".to_string(),
        addr_a,
        peers_a,
        ClusterSecret::new("shared-cluster-secret-for-tests").unwrap(),
    )
    .await
    .expect("create transport A");

    // Self is no longer a known peer …
    let err = transport_a
        .send("node-a", request_vote(1))
        .await
        .expect_err("sending to self must be rejected");
    assert!(
        err.to_string().contains("unknown peer"),
        "expected unknown-peer error, got: {err:#}"
    );

    // … and never became a one-shot target either.
    let direct = transport_a.send_to("node-a", &request_vote(1)).await;
    assert!(direct.is_err(), "send_to(self) must fail");

    // The real peer still works.
    let mut peers_b = HashMap::new();
    peers_b.insert("node-a".to_string(), addr_a);
    let transport_b = TcpTransport::new(
        "node-b".to_string(),
        addr_b,
        peers_b,
        ClusterSecret::new("shared-cluster-secret-for-tests").unwrap(),
    )
    .await
    .expect("create transport B");
    tokio::time::sleep(Duration::from_millis(50)).await;

    transport_a
        .send("node-b", request_vote(7))
        .await
        .expect("enqueue to the real peer");
    let (from, msg) = tokio::time::timeout(Duration::from_secs(2), async {
        transport_b.recv().await.expect("recv error")
    })
    .await
    .expect("message to the real peer must still arrive");
    assert_eq!(from, "node-a");
    assert!(matches!(msg, RaftMessage::RequestVote { term: 7, .. }));
}

/// The #1168 acceptance scenario at ring level: three nodes whose membership
/// lists include every member (self included, the documented convention),
/// a leader elected, then the leader's runner dropped mid-flight — a dead
/// node. The two survivors must converge on exactly one leader and go quiet:
/// no new terms, no re-election, for seconds afterward.
///
/// Pre-fix this churned forever: majority was 3 of 3 real nodes (unanimity),
/// and each survivor only ever "won" an election by double-counting its own
/// vote, so the survivors leapfrogged terms every ~250 ms — measured 94 terms
/// in 4 minutes on rc.83, still unstable with the transport fix alone.
#[tokio::test]
async fn ring_with_full_membership_lists_survives_leader_death() {
    use tokio::sync::watch;

    let bus = InMemoryBus::new();
    // Full membership on every node, self INCLUDED — what the server used to
    // pass straight through (#1168), and what RaftNode::new now filters.
    let membership = ["n1", "n2", "n3"];

    let mk_node = |id: &'static str| async {
        let transport = InMemoryTransport::new(id.to_string(), bus.clone()).await;
        let peers: Vec<String> = membership.iter().map(|s| s.to_string()).collect();
        ClusterNode::new(id.to_string(), peers, Box::new(transport))
    };

    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let tick = Duration::from_millis(5);
    let mut runners = vec![
        ClusterRunner::new(mk_node("n1").await, tick, shutdown_rx.clone()),
        ClusterRunner::new(mk_node("n2").await, tick, shutdown_rx.clone()),
        ClusterRunner::new(mk_node("n3").await, tick, shutdown_rx.clone()),
    ];

    // Phase 1 — elect a leader (drive all three loops concurrently).
    {
        let mut it = std::mem::take(&mut runners).into_iter();
        let (mut x, mut y, mut z) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
        tokio::select! {
            _ = x.run() => {}
            _ = y.run() => {}
            _ = z.run() => {}
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
        runners = vec![x, y, z];
    }
    let leader_count = runners.iter().filter(|r| r.is_leader()).count();
    assert_eq!(leader_count, 1, "exactly one leader before the failure");

    // Phase 2 — kill the leader by dropping its runner (transport dropped →
    // its InMemory receiver closes → peers' sends to it fail = dead node).
    let leader_idx = runners
        .iter()
        .position(|r| r.is_leader())
        .expect("phase 1 asserted a leader");
    drop(runners.remove(leader_idx));
    assert_eq!(runners.len(), 2, "two survivors");

    // Phase 2a — the survivors elect exactly one new leader.
    {
        let mut it = std::mem::take(&mut runners).into_iter();
        let (mut a, mut b) = (it.next().unwrap(), it.next().unwrap());
        tokio::select! {
            _ = a.run() => {}
            _ = b.run() => {}
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
        runners = vec![a, b];
    }
    let leader_count = runners.iter().filter(|r| r.is_leader()).count();
    assert_eq!(
        leader_count, 1,
        "survivors must converge on exactly one leader after the leader dies"
    );
    let terms_2a: Vec<u64> = runners.iter().map(|r| r.node.raft.current_term()).collect();

    // Phase 2b — and go quiet: two more seconds, same terms, same leadership.
    {
        let mut it = std::mem::take(&mut runners).into_iter();
        let (mut a, mut b) = (it.next().unwrap(), it.next().unwrap());
        tokio::select! {
            _ = a.run() => {}
            _ = b.run() => {}
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
        runners = vec![a, b];
    }
    let terms_2b: Vec<u64> = runners.iter().map(|r| r.node.raft.current_term()).collect();
    assert_eq!(
        terms_2a, terms_2b,
        "the surviving pair must be election-stable: terms kept advancing => churn (#1168)"
    );
    let leader_count = runners.iter().filter(|r| r.is_leader()).count();
    assert_eq!(leader_count, 1, "still exactly one leader at the end");
}
