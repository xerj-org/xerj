//! A new leader must commit an entry it inherited from an earlier term
//! without waiting for a client write (Raft §5.4.2, §8).
//!
//! The leader only counts replicas for entries of its own term, so it needs
//! one of those in its log. Before the leader no-op existed, an entry that
//! reached a majority under the old leader stayed at commit=0 until the next
//! client proposal, and metadata writes after an election hung.

use xerj_cluster::raft::{ClusterCommand, OutboundMessage, RaftNode};

const IDS: [&str; 3] = ["n1", "n2", "n3"];

fn peers(except: &str) -> Vec<String> {
    IDS.iter()
        .filter(|p| **p != except)
        .map(|p| p.to_string())
        .collect()
}

fn pos(nodes: &[RaftNode], id: &str) -> usize {
    nodes.iter().position(|n| n.id == id).expect("known node")
}

/// Deliver messages, and every reply they cause, among `alive` only.
fn deliver(nodes: &mut [RaftNode], alive: &[&str], msgs: Vec<OutboundMessage>) {
    let mut queue = msgs;
    while !queue.is_empty() {
        let mut next = Vec::new();
        for m in queue {
            if !alive.contains(&m.to.as_str()) {
                continue;
            }
            let i = pos(nodes, &m.to);
            next.extend(nodes[i].handle_message(m.msg));
        }
        queue = next;
    }
}

#[test]
fn a_new_leader_commits_a_previous_term_entry_held_by_a_majority() {
    let mut nodes: Vec<RaftNode> = IDS
        .iter()
        .map(|id| RaftNode::new(id.to_string(), peers(id)))
        .collect();

    // Term 1: n1 leads.
    nodes[0].force_election_timeout();
    let votes = nodes[0].tick();
    deliver(&mut nodes, &IDS, votes);
    assert!(nodes[0].is_leader());

    // n1 appends an entry and only n2 stores it. n2's ack is lost, so n1
    // never learns the entry is on a majority.
    let idx = nodes[0]
        .propose(ClusterCommand::UpdateConfig {
            key: "k".into(),
            value: "v".into(),
        })
        .expect("propose on the leader");
    for m in nodes[0].force_heartbeat() {
        if m.to == "n2" {
            let _lost_ack = nodes[1].handle_message(m.msg);
        }
    }
    assert!(nodes[1].log_len() as u64 >= idx, "n2 stores entry {idx}");
    assert!(nodes[0].commit_index() < idx, "n1 never saw a majority");

    // n1 goes away. n2 has the longer log and wins with n3's vote.
    let alive = ["n2", "n3"];
    nodes[1].force_election_timeout();
    let votes = nodes[1].tick();
    deliver(&mut nodes, &alive, votes);
    assert!(nodes[1].is_leader(), "n2 wins term 2");

    for _ in 0..20 {
        let hb = nodes[1].force_heartbeat();
        deliver(&mut nodes, &alive, hb);
    }
    assert!(
        nodes[2].log_len() as u64 >= idx,
        "n3 stores entry {idx}: it is on 2 of 3 members"
    );
    assert!(
        nodes[1].commit_index() >= idx,
        "entry {idx} on a majority is committed by the new leader (leader commit={})",
        nodes[1].commit_index()
    );
    assert!(
        nodes[2].commit_index() >= idx,
        "the follower learns the commit too (n3 commit={})",
        nodes[2].commit_index()
    );
}
