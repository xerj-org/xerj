//! M5 exit criterion for the persistent Raft log.
//!
//! Given a single-node cluster with `FileRaftLog` wired in, a `propose`
//! call must survive a full `RaftNode` drop + re-open, and the replayed
//! state machine must show the same committed index / log entries.

use std::fs;

use tempfile::tempdir;

use xerj_cluster::raft::{ClusterCommand, RaftNode};

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

        // Single-node clusters commit immediately in propose().
        assert_eq!(node.commit_index(), 50);
        assert_eq!(node.log_len(), 50);
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
        let node2 = RaftNode::with_storage("n1".into(), vec![], &path).unwrap();
        assert_eq!(
            node2.log_len(),
            50,
            "all 50 entries must replay from disk on restart"
        );
        assert_eq!(
            node2.commit_index(),
            50,
            "commit_index must survive restart"
        );
        assert_eq!(
            node2.current_term(),
            1,
            "current_term must be at least the term of the replayed entries"
        );
    }

    // ── Phase 3: reopening must be idempotent — size stays identical
    let size_after_phase2 = fs::metadata(&log_path).unwrap().len();
    assert_eq!(
        size_after_phase1, size_after_phase2,
        "reopening the log must not re-serialize existing entries"
    );
}
