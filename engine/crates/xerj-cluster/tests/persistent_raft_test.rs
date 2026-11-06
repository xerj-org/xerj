//! M5 exit criterion for the persistent Raft log.
//!
//! Given a single-node cluster with `FileRaftLog` wired in, a `propose`
//! call must survive a full `RaftNode` drop + re-open, and the replayed
//! state machine must show the same committed index / log entries.

use std::fs;

use tempfile::tempdir;

use xerj_cluster::raft::{ClusterCommand, LogEntry, RaftMessage, RaftNode};

#[test]
fn single_node_persistent_roundtrip() {
    let dir = tempdir().unwrap();
    let path = dir.path().to_path_buf();

    // ── Phase 1: fresh node, elect, propose 50 commands ──────────────
    {
        let mut node = RaftNode::with_storage("n1".into(), vec![], &path).unwrap();

        // Single-node cluster — fire one election and this node becomes
        // leader because it votes for itself and there are no peers.
        node.force_election_timeout();
        let _ = node.tick();
        assert!(node.is_leader(), "single-node cluster must elect self");

        for i in 0..50 {
            let cmd = ClusterCommand::CreateIndex {
                name: format!("idx-{i}"),
                schema_json: "{}".into(),
            };
            node.propose(cmd).unwrap();
        }

        // Single-node clusters commit immediately in propose(). Index 1 is
        // the no-op the leader appended when it took term 1.
        assert_eq!(node.commit_index(), 51);
        assert_eq!(node.log_len(), 51);
    }

    // The file must have been fsynced to disk.
    let log_path = path.join("raft.log");
    assert!(log_path.exists(), "raft.log must exist after propose+fsync");
    let commit_path = path.join("commit.meta");
    assert!(commit_path.exists(), "commit.meta must exist");

    let size_after_phase1 = fs::metadata(&log_path).unwrap().len();
    assert!(size_after_phase1 > 0, "raft.log must be non-empty");

    // ── Phase 2: reopen and verify replay ────────────────────────────
    {
        let mut node2 = RaftNode::with_storage("n1".into(), vec![], &path).unwrap();
        assert_eq!(
            node2.log_len(),
            51,
            "the no-op and all 50 entries must replay from disk on restart"
        );
        assert_eq!(
            node2.commit_index(),
            51,
            "commit_index must survive restart"
        );
        assert_eq!(
            node2.current_term(),
            1,
            "current_term must be at least the term of the replayed entries"
        );
        let replayed = node2.ready();
        assert_eq!(replayed.len(), 51);
        assert_eq!(
            replayed[0].command,
            ClusterCommand::Noop,
            "the no-op decodes from the file log"
        );
    }

    // ── Phase 3: reopening must be idempotent — size stays identical
    let size_after_phase2 = fs::metadata(&log_path).unwrap().len();
    assert_eq!(
        size_after_phase1, size_after_phase2,
        "reopening the log must not re-serialize existing entries"
    );
}

fn entry(term: u64, index: u64, name: &str) -> LogEntry {
    LogEntry {
        term,
        index,
        command: ClusterCommand::CreateIndex {
            name: name.to_string(),
            schema_json: "{}".into(),
        },
    }
}

fn append_ok(replies: &[xerj_cluster::raft::OutboundMessage]) -> bool {
    replies.iter().any(|out| {
        matches!(
            out.msg,
            RaftMessage::AppendEntriesResponse { success: true, .. }
        )
    })
}

fn vote_granted(replies: &[xerj_cluster::raft::OutboundMessage]) -> bool {
    replies.iter().any(|out| {
        matches!(
            out.msg,
            RaftMessage::RequestVoteResponse {
                vote_granted: true,
                ..
            }
        )
    })
}

/// A follower has to store entries before it says yes. Reopening must not
/// bring back a tail the leader already replaced.
#[test]
fn follower_persists_appended_entries_and_drops_a_replaced_tail() {
    let dir = tempdir().unwrap();
    let path = dir.path();
    let mut node = RaftNode::with_storage("n2".into(), vec!["n1".into()], path).unwrap();

    let replies = node.handle_message(RaftMessage::AppendEntries {
        term: 1,
        leader_id: "n1".into(),
        prev_log_index: 0,
        prev_log_term: 0,
        entries: vec![entry(1, 1, "a"), entry(1, 2, "b"), entry(1, 3, "c")],
        leader_commit: 1,
    });
    assert!(append_ok(&replies), "first append must be stored");
    assert_eq!(node.commit_index(), 1);
    assert_eq!(node.log_len(), 3);

    let replies = node.handle_message(RaftMessage::AppendEntries {
        term: 2,
        leader_id: "n1".into(),
        prev_log_index: 1,
        prev_log_term: 1,
        entries: vec![entry(2, 2, "b2")],
        leader_commit: 2,
    });
    assert!(append_ok(&replies), "conflicting tail must be replaced");
    assert_eq!(node.entry_term(2), Some(2));
    assert_eq!(node.entry_term(3), None);

    drop(node);
    let node = RaftNode::with_storage("n2".into(), vec!["n1".into()], path).unwrap();
    assert_eq!(
        node.log_len(),
        2,
        "index 3 must not come back from the file"
    );
    assert_eq!(node.entry_term(1), Some(1));
    assert_eq!(node.entry_term(2), Some(2));
    assert_eq!(node.entry_term(3), None);
    assert_eq!(node.commit_index(), 2);
}

/// A vote that was granted has to stick across a restart in the same term.
#[test]
fn a_restart_does_not_grant_a_second_vote_in_the_same_term() {
    let dir = tempdir().unwrap();
    let path = dir.path();
    let mut node =
        RaftNode::with_storage("n1".into(), vec!["n2".into(), "n3".into()], path).unwrap();
    let replies = node.handle_message(RaftMessage::RequestVote {
        term: 1,
        candidate_id: "n2".into(),
        last_log_index: 0,
        last_log_term: 0,
    });
    assert!(vote_granted(&replies), "first vote in the term is granted");

    drop(node);
    let mut node =
        RaftNode::with_storage("n1".into(), vec!["n2".into(), "n3".into()], path).unwrap();
    assert_eq!(node.current_term(), 1);
    let replies = node.handle_message(RaftMessage::RequestVote {
        term: 1,
        candidate_id: "n3".into(),
        last_log_index: 0,
        last_log_term: 0,
    });
    assert!(
        !vote_granted(&replies),
        "a second candidate must not get a vote in this term"
    );
    let replies = node.handle_message(RaftMessage::RequestVote {
        term: 1,
        candidate_id: "n2".into(),
        last_log_index: 0,
        last_log_term: 0,
    });
    assert!(
        vote_granted(&replies),
        "the original candidate is still the vote"
    );
}
