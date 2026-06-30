//! Minimal Raft consensus implementation — no external Raft library.
//!
//! Implements the Raft protocol as described in "In Search of an Understandable
//! Consensus Algorithm" (Ongaro & Ousterhout, 2014).
//!
//! Safety properties upheld:
//! - Election Safety: at most one leader per term
//! - Leader Append-Only: a leader never overwrites or deletes entries in its log
//! - Log Matching: if two logs contain an entry with the same index and term,
//!   the logs are identical in all entries up through that index
//! - Leader Completeness: if a log entry is committed in a given term, that entry
//!   will be present in the logs of all leaders for all higher-numbered terms
//! - State Machine Safety: if a server has applied a log entry at a given index,
//!   no other server will ever apply a different log entry for that same index

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// State a Raft node can be in.
#[derive(Debug, Clone, PartialEq)]
pub enum RaftState {
    Follower,
    Candidate,
    Leader,
}

impl std::fmt::Display for RaftState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RaftState::Follower => write!(f, "Follower"),
            RaftState::Candidate => write!(f, "Candidate"),
            RaftState::Leader => write!(f, "Leader"),
        }
    }
}

/// Commands replicated through Raft consensus.
///
/// These are the operations that change cluster-wide metadata. All nodes apply
/// committed entries to their local [`crate::metadata::ClusterMetadata`] store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ClusterCommand {
    CreateIndex {
        name: String,
        schema_json: String,
    },
    DeleteIndex {
        name: String,
    },
    UpdateMapping {
        index: String,
        mapping_json: String,
    },
    AssignShard {
        index: String,
        shard: u32,
        node_id: String,
    },
    AddNode {
        node_id: String,
        address: String,
    },
    RemoveNode {
        node_id: String,
    },
    UpdateConfig {
        key: String,
        value: String,
    },
}

/// A single entry in the Raft replicated log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// The term in which this entry was created.
    pub term: u64,
    /// 1-based log position. Index 0 is a sentinel (empty log guard).
    pub index: u64,
    /// The command to apply when this entry is committed.
    pub command: ClusterCommand,
}

/// Messages exchanged between Raft peers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RaftMessage {
    /// Leader → Follower: heartbeat / log replication RPC.
    AppendEntries {
        term: u64,
        leader_id: String,
        prev_log_index: u64,
        prev_log_term: u64,
        entries: Vec<LogEntry>,
        leader_commit: u64,
    },
    /// Follower → Leader: response to AppendEntries.
    AppendEntriesResponse {
        /// The sender's node ID (so the leader knows who responded).
        from: String,
        term: u64,
        success: bool,
        /// Highest log index the follower has successfully stored.
        match_index: u64,
    },
    /// Candidate → All: vote request RPC.
    RequestVote {
        term: u64,
        candidate_id: String,
        last_log_index: u64,
        last_log_term: u64,
    },
    /// Any node → Candidate: vote response.
    RequestVoteResponse {
        /// The sender's node ID.
        from: String,
        term: u64,
        vote_granted: bool,
    },
}

/// An outbound message — the node ID of the recipient plus the payload.
#[derive(Debug, Clone)]
pub struct OutboundMessage {
    pub to: String,
    pub msg: RaftMessage,
}

/// Core Raft node — a pure state machine, no I/O.
///
/// The caller drives the node forward by:
/// 1. Calling [`RaftNode::tick`] periodically (e.g. every ~10 ms).
/// 2. Calling [`RaftNode::handle_message`] for each inbound message.
/// 3. Sending every [`OutboundMessage`] returned by those calls to the named peer.
/// 4. Calling [`RaftNode::ready`] after each tick/handle_message to drain newly
///    committed log entries for application to the state machine.
pub struct RaftNode {
    // ── Identity ────────────────────────────────────────────────────────────
    pub id: String,

    // ── Persistent state ────────────────────────────────────────────────────
    current_term: u64,
    voted_for: Option<String>,
    log: Vec<LogEntry>, // index 0 = sentinel with term 0
    /// Optional file-backed log for durability across restarts.
    /// When `Some`, every `propose()` / `handle_append_entries` call
    /// persists the entry before committing to the in-memory `log` Vec.
    persistent: Option<crate::raft_log::FileRaftLog>,

    // ── Volatile state ───────────────────────────────────────────────────────
    state: RaftState,
    commit_index: u64,
    last_applied: u64,

    // ── Leader volatile state ────────────────────────────────────────────────
    /// For each peer: index of next log entry to send.
    next_index: HashMap<String, u64>,
    /// For each peer: highest log index known to be replicated on that peer.
    match_index: HashMap<String, u64>,

    // ── Cluster membership ───────────────────────────────────────────────────
    peers: Vec<String>,
    leader_id: Option<String>,

    // ── Timing ───────────────────────────────────────────────────────────────
    election_timeout: Duration,
    last_heartbeat: Instant,
    heartbeat_interval: Duration,
    last_heartbeat_sent: Instant,

    // ── Candidate state ──────────────────────────────────────────────────────
    votes_received: usize,
}

impl RaftNode {
    /// Create a new Raft node.
    ///
    /// All nodes start as followers in term 0.
    pub fn new(id: String, peers: Vec<String>) -> Self {
        let election_timeout = Self::random_election_timeout();
        RaftNode {
            id,
            current_term: 0,
            voted_for: None,
            log: vec![],
            persistent: None,
            state: RaftState::Follower,
            commit_index: 0,
            last_applied: 0,
            next_index: HashMap::new(),
            match_index: HashMap::new(),
            peers,
            leader_id: None,
            election_timeout,
            last_heartbeat: Instant::now(),
            heartbeat_interval: Duration::from_millis(50),
            last_heartbeat_sent: Instant::now(),
            votes_received: 0,
        }
    }

    /// Like [`RaftNode::new`] but opens a [`crate::raft_log::FileRaftLog`]
    /// at the given directory and replays it to rebuild `self.log`,
    /// `current_term`, `voted_for`, and `commit_index`.  This is what
    /// the server calls on restart to recover persisted state.
    pub fn with_storage(
        id: String,
        peers: Vec<String>,
        storage_dir: impl AsRef<std::path::Path>,
    ) -> Result<Self> {
        let mut node = Self::new(id, peers);
        let mut flog = crate::raft_log::FileRaftLog::open(&storage_dir)
            .map_err(|e| anyhow::anyhow!("opening raft log: {e}"))?;

        // Replay every persisted entry into the in-memory log.  The
        // in-memory log is kept 1-indexed (sentinel at index 0) to
        // match the rest of this file.
        let last = flog.last_index();
        for idx in 1..=last {
            if let Some((term, payload)) = flog
                .read(idx)
                .map_err(|e| anyhow::anyhow!("read raft entry: {e}"))?
            {
                let command: ClusterCommand = serde_json::from_slice(&payload)
                    .map_err(|e| anyhow::anyhow!("decode raft entry: {e}"))?;
                node.log.push(LogEntry {
                    term,
                    index: idx,
                    command,
                });
                if term > node.current_term {
                    node.current_term = term;
                }
            }
        }
        node.commit_index = flog.commit_index();
        node.persistent = Some(flog);
        Ok(node)
    }

    // ── Public API ───────────────────────────────────────────────────────────

    /// Drive the Raft time-based logic forward.
    ///
    /// Returns messages that must be sent to peers. Should be called frequently
    /// (every ~10 ms in production; every ~1 ms in tests).
    pub fn tick(&mut self) -> Vec<OutboundMessage> {
        let mut out = Vec::new();
        match self.state {
            RaftState::Follower | RaftState::Candidate => {
                if self.last_heartbeat.elapsed() >= self.election_timeout {
                    out.extend(self.start_election());
                }
            }
            RaftState::Leader => {
                if self.last_heartbeat_sent.elapsed() >= self.heartbeat_interval {
                    out.extend(self.send_heartbeats());
                }
            }
        }
        out
    }

    /// Handle an inbound Raft message from a peer.
    ///
    /// Returns zero or more messages to send back (or to other peers).
    pub fn handle_message(&mut self, msg: RaftMessage) -> Vec<OutboundMessage> {
        let mut out = Vec::new();

        // All servers: If RPC request or response contains term T > currentTerm:
        // set currentTerm = T, convert to follower (§5.1).
        let msg_term = match &msg {
            RaftMessage::AppendEntries { term, .. } => *term,
            RaftMessage::AppendEntriesResponse { term, .. } => *term,
            RaftMessage::RequestVote { term, .. } => *term,
            RaftMessage::RequestVoteResponse { term, .. } => *term,
        };
        if msg_term > self.current_term {
            debug!(
                node = %self.id,
                old_term = self.current_term,
                new_term = msg_term,
                "Higher term observed — stepping down to follower"
            );
            self.current_term = msg_term;
            self.voted_for = None;
            self.become_follower(None);
        }

        match msg {
            RaftMessage::AppendEntries {
                term,
                leader_id,
                prev_log_index,
                prev_log_term,
                entries,
                leader_commit,
            } => {
                out.extend(self.handle_append_entries(
                    term,
                    leader_id,
                    prev_log_index,
                    prev_log_term,
                    entries,
                    leader_commit,
                ));
            }
            RaftMessage::AppendEntriesResponse {
                from,
                term,
                success,
                match_index,
            } => {
                out.extend(self.handle_append_entries_response(from, term, success, match_index));
            }
            RaftMessage::RequestVote {
                term,
                candidate_id,
                last_log_index,
                last_log_term,
            } => {
                out.extend(self.handle_request_vote(term, candidate_id, last_log_index, last_log_term));
            }
            RaftMessage::RequestVoteResponse {
                from,
                term,
                vote_granted,
            } => {
                out.extend(self.handle_request_vote_response(from, term, vote_granted));
            }
        }
        out
    }

    /// Propose a command to be replicated (leader only).
    ///
    /// Returns the log index assigned to this entry.
    /// Returns an error if this node is not the leader.
    pub fn propose(&mut self, cmd: ClusterCommand) -> Result<u64> {
        if self.state != RaftState::Leader {
            bail!(
                "node {} is not the leader (state={}, leader={:?})",
                self.id,
                self.state,
                self.leader_id
            );
        }
        let index = self.last_log_index() + 1;
        let entry = LogEntry {
            term: self.current_term,
            index,
            command: cmd,
        };

        // Persist BEFORE committing to the in-memory log.  Once we've
        // returned Ok(index) to the caller the write is durable even if
        // the process dies before the next tick.
        if let Some(flog) = self.persistent.as_mut() {
            let payload = serde_json::to_vec(&entry.command)
                .map_err(|e| anyhow::anyhow!("encode raft entry: {e}"))?;
            flog.append(entry.term, entry.index, &payload)
                .map_err(|e| anyhow::anyhow!("append raft log: {e}"))?;
            flog.fsync()
                .map_err(|e| anyhow::anyhow!("fsync raft log: {e}"))?;
        }

        self.log.push(entry);
        info!(node = %self.id, index, term = self.current_term, "Proposed new log entry");

        // For a single-node cluster, we can commit immediately.
        if self.peers.is_empty() {
            self.commit_index = index;
            if let Some(flog) = self.persistent.as_mut() {
                let _ = flog.set_commit_index(index);
            }
        }
        Ok(index)
    }

    /// Drain newly committed entries that haven't been applied yet.
    ///
    /// The caller should apply each returned entry to its state machine in order.
    pub fn ready(&mut self) -> Vec<LogEntry> {
        let mut applied = Vec::new();
        while self.last_applied < self.commit_index {
            self.last_applied += 1;
            if let Some(entry) = self.log_entry(self.last_applied) {
                applied.push(entry.clone());
            }
        }
        applied
    }

    /// Returns `true` if this node believes itself to be the current leader.
    pub fn is_leader(&self) -> bool {
        self.state == RaftState::Leader
    }

    /// Returns the ID of the node this node believes to be the current leader.
    pub fn leader_id(&self) -> Option<&str> {
        self.leader_id.as_deref()
    }

    /// Current Raft term.
    pub fn current_term(&self) -> u64 {
        self.current_term
    }

    /// Current role.
    pub fn state(&self) -> &RaftState {
        &self.state
    }

    /// Force an immediate election timeout — useful in tests and for initial
    /// bootstrap where you want a specific node to become the first leader.
    pub fn force_election_timeout(&mut self) {
        self.last_heartbeat = Instant::now() - Duration::from_millis(500);
    }

    /// Force the leader to immediately emit heartbeat/replication messages.
    ///
    /// In tests, wall-clock time doesn't advance between ticks, so the
    /// heartbeat interval never fires. Calling this on a leader causes it to
    /// send AppendEntries to all peers right now.
    pub fn force_heartbeat(&mut self) -> Vec<OutboundMessage> {
        if self.state == RaftState::Leader {
            self.last_heartbeat_sent =
                Instant::now() - Duration::from_millis(1000);
            self.send_heartbeats()
        } else {
            vec![]
        }
    }

    /// Number of entries in the log.
    pub fn log_len(&self) -> usize {
        self.log.len()
    }

    /// Highest committed index.
    pub fn commit_index(&self) -> u64 {
        self.commit_index
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    fn cluster_size(&self) -> usize {
        self.peers.len() + 1 // peers + self
    }

    fn majority(&self) -> usize {
        self.cluster_size() / 2 + 1
    }

    fn last_log_index(&self) -> u64 {
        self.log.last().map(|e| e.index).unwrap_or(0)
    }

    fn last_log_term(&self) -> u64 {
        self.log.last().map(|e| e.term).unwrap_or(0)
    }

    /// Retrieve a log entry by its 1-based Raft index.
    fn log_entry(&self, index: u64) -> Option<&LogEntry> {
        if index == 0 {
            return None;
        }
        // Find entry with matching index (log is stored in order)
        self.log.iter().find(|e| e.index == index)
    }

    /// Term of the log entry at `index`, or 0 if `index == 0`.
    fn log_term_at(&self, index: u64) -> u64 {
        if index == 0 {
            return 0;
        }
        self.log_entry(index).map(|e| e.term).unwrap_or(0)
    }

    fn random_election_timeout() -> Duration {
        let mut rng = rand::thread_rng();
        Duration::from_millis(rng.gen_range(150..=300))
    }

    fn become_follower(&mut self, leader_id: Option<String>) {
        self.state = RaftState::Follower;
        self.leader_id = leader_id;
        self.votes_received = 0;
        self.election_timeout = Self::random_election_timeout();
        self.last_heartbeat = Instant::now();
    }

    fn become_leader(&mut self) {
        info!(
            node = %self.id,
            term = self.current_term,
            "Became leader"
        );
        self.state = RaftState::Leader;
        self.leader_id = Some(self.id.clone());
        self.votes_received = 0;

        // Initialize leader volatile state (§5.3)
        let next = self.last_log_index() + 1;
        for peer in &self.peers {
            self.next_index.insert(peer.clone(), next);
            self.match_index.insert(peer.clone(), 0);
        }
    }

    fn start_election(&mut self) -> Vec<OutboundMessage> {
        self.current_term += 1;
        self.state = RaftState::Candidate;
        self.voted_for = Some(self.id.clone()); // vote for self
        self.votes_received = 1; // count self-vote
        self.election_timeout = Self::random_election_timeout();
        self.last_heartbeat = Instant::now();
        self.leader_id = None;

        info!(
            node = %self.id,
            term = self.current_term,
            "Starting election"
        );

        // Single-node cluster wins immediately.
        if self.peers.is_empty() {
            self.become_leader();
            return self.send_heartbeats();
        }

        let last_log_index = self.last_log_index();
        let last_log_term = self.last_log_term();
        let term = self.current_term;
        let candidate_id = self.id.clone();

        self.peers
            .clone()
            .iter()
            .map(|peer| OutboundMessage {
                to: peer.clone(),
                msg: RaftMessage::RequestVote {
                    term,
                    candidate_id: candidate_id.clone(),
                    last_log_index,
                    last_log_term,
                },
            })
            .collect()
    }

    fn send_heartbeats(&mut self) -> Vec<OutboundMessage> {
        self.last_heartbeat_sent = Instant::now();
        let mut out = Vec::new();
        for peer in self.peers.clone() {
            if let Some(msgs) = self.build_append_entries(&peer) {
                out.push(msgs);
            }
        }
        out
    }

    /// Build an AppendEntries RPC for the given peer.
    fn build_append_entries(&self, peer: &str) -> Option<OutboundMessage> {
        let next_idx = *self.next_index.get(peer).unwrap_or(&1);
        let prev_log_index = next_idx.saturating_sub(1);
        let prev_log_term = self.log_term_at(prev_log_index);

        // Collect entries starting at next_idx
        let entries: Vec<LogEntry> = self
            .log
            .iter()
            .filter(|e| e.index >= next_idx)
            .cloned()
            .collect();

        Some(OutboundMessage {
            to: peer.to_string(),
            msg: RaftMessage::AppendEntries {
                term: self.current_term,
                leader_id: self.id.clone(),
                prev_log_index,
                prev_log_term,
                entries,
                leader_commit: self.commit_index,
            },
        })
    }

    // ── Message handlers ─────────────────────────────────────────────────────

    fn handle_append_entries(
        &mut self,
        term: u64,
        leader_id: String,
        prev_log_index: u64,
        prev_log_term: u64,
        entries: Vec<LogEntry>,
        leader_commit: u64,
    ) -> Vec<OutboundMessage> {
        // 1. Reply false if term < currentTerm (§5.1)
        if term < self.current_term {
            return vec![OutboundMessage {
                to: leader_id,
                msg: RaftMessage::AppendEntriesResponse {
                    from: self.id.clone(),
                    term: self.current_term,
                    success: false,
                    match_index: 0,
                },
            }];
        }

        // Valid AppendEntries from current leader — reset election timer
        self.last_heartbeat = Instant::now();
        if self.state != RaftState::Follower {
            self.become_follower(Some(leader_id.clone()));
        } else {
            self.leader_id = Some(leader_id.clone());
        }

        // 2. Reply false if log doesn't contain an entry at prevLogIndex
        //    whose term matches prevLogTerm (§5.3)
        if prev_log_index > 0 {
            match self.log_entry(prev_log_index) {
                None => {
                    return vec![OutboundMessage {
                        to: leader_id,
                        msg: RaftMessage::AppendEntriesResponse {
                            from: self.id.clone(),
                            term: self.current_term,
                            success: false,
                            match_index: self.last_log_index(),
                        },
                    }];
                }
                Some(e) if e.term != prev_log_term => {
                    // 3. If an existing entry conflicts with a new one (same index,
                    //    different terms), delete the existing entry and all that
                    //    follow it (§5.3)
                    self.log.retain(|e| e.index < prev_log_index);
                    return vec![OutboundMessage {
                        to: leader_id,
                        msg: RaftMessage::AppendEntriesResponse {
                            from: self.id.clone(),
                            term: self.current_term,
                            success: false,
                            match_index: self.last_log_index(),
                        },
                    }];
                }
                _ => {}
            }
        }

        // 4. Append any new entries not already in the log
        for entry in &entries {
            // If we already have this index check for conflicts
            if let Some(existing) = self.log_entry(entry.index) {
                if existing.term != entry.term {
                    // Conflict — truncate from this index onward
                    self.log.retain(|e| e.index < entry.index);
                    self.log.push(entry.clone());
                }
                // else: already have this exact entry, skip
            } else {
                self.log.push(entry.clone());
            }
        }
        // Ensure log is sorted by index (should already be, but be safe)
        self.log.sort_by_key(|e| e.index);

        // 5. If leaderCommit > commitIndex, set commitIndex =
        //    min(leaderCommit, index of last new entry) (§5.3)
        if leader_commit > self.commit_index {
            self.commit_index = leader_commit.min(self.last_log_index());
        }

        let match_index = self.last_log_index();
        debug!(
            node = %self.id,
            match_index,
            "AppendEntries success"
        );

        vec![OutboundMessage {
            to: leader_id,
            msg: RaftMessage::AppendEntriesResponse {
                from: self.id.clone(),
                term: self.current_term,
                success: true,
                match_index,
            },
        }]
    }

    fn handle_append_entries_response(
        &mut self,
        from: String,
        term: u64,
        success: bool,
        match_index: u64,
    ) -> Vec<OutboundMessage> {
        if self.state != RaftState::Leader {
            return vec![];
        }
        if term < self.current_term {
            return vec![];
        }

        if success {
            // Update next_index and match_index for the follower
            self.match_index
                .entry(from.clone())
                .and_modify(|m| *m = (*m).max(match_index))
                .or_insert(match_index);
            *self.next_index.entry(from).or_insert(1) = match_index + 1;

            // Check if we can advance commit_index.
            // Find the highest N such that a majority of match_index[i] ≥ N
            // and log[N].term == currentTerm (§5.4)
            let last_idx = self.last_log_index();
            for n in (self.commit_index + 1..=last_idx).rev() {
                if self.log_term_at(n) != self.current_term {
                    continue;
                }
                // Count how many nodes (including self) have replicated up to n
                let replication_count = 1 // self
                    + self.match_index.values().filter(|&&m| m >= n).count();
                if replication_count >= self.majority() {
                    info!(
                        node = %self.id,
                        commit_index = n,
                        "Advancing commit index"
                    );
                    self.commit_index = n;
                    break;
                }
            }
        } else {
            // Decrement next_index and retry
            let ni = self.next_index.entry(from.clone()).or_insert(1);
            *ni = ni.saturating_sub(1).max(1);

            // Retry immediately with the updated next_index
            if let Some(msg) = self.build_append_entries(&from) {
                return vec![msg];
            }
        }
        vec![]
    }

    fn handle_request_vote(
        &mut self,
        term: u64,
        candidate_id: String,
        last_log_index: u64,
        last_log_term: u64,
    ) -> Vec<OutboundMessage> {
        let mut vote_granted = false;

        // Grant vote if:
        //   a) haven't voted yet (or already voted for this candidate) this term
        //   b) candidate's log is at least as up-to-date as ours (§5.4)
        if term >= self.current_term {
            let can_vote = self.voted_for.is_none()
                || self.voted_for.as_deref() == Some(&candidate_id);

            // "Up-to-date" check: candidate's last log term > ours, OR
            //  same last log term and candidate's log is at least as long.
            let candidate_log_ok = last_log_term > self.last_log_term()
                || (last_log_term == self.last_log_term()
                    && last_log_index >= self.last_log_index());

            if can_vote && candidate_log_ok {
                self.voted_for = Some(candidate_id.clone());
                self.last_heartbeat = Instant::now(); // reset election timer
                vote_granted = true;
                debug!(node = %self.id, candidate = %candidate_id, "Granting vote");
            }
        }

        vec![OutboundMessage {
            to: candidate_id,
            msg: RaftMessage::RequestVoteResponse {
                from: self.id.clone(),
                term: self.current_term,
                vote_granted,
            },
        }]
    }

    fn handle_request_vote_response(
        &mut self,
        _from: String,
        term: u64,
        vote_granted: bool,
    ) -> Vec<OutboundMessage> {
        if self.state != RaftState::Candidate || term != self.current_term {
            return vec![];
        }

        if vote_granted {
            self.votes_received += 1;
            debug!(
                node = %self.id,
                votes = self.votes_received,
                needed = self.majority(),
                "Received vote"
            );
            if self.votes_received >= self.majority() {
                self.become_leader();
                return self.send_heartbeats();
            }
        }
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive_until_leader(nodes: &mut Vec<RaftNode>, max_ticks: usize) {
        for _ in 0..max_ticks {
            // Collect all outbound messages from every node's tick
            let mut all_msgs: Vec<(usize, OutboundMessage)> = Vec::new();
            for (i, node) in nodes.iter_mut().enumerate() {
                for msg in node.tick() {
                    all_msgs.push((i, msg));
                }
                // Force heartbeats so leader → follower replication isn't
                // gated on wall-clock time advancing in unit tests
                for msg in node.force_heartbeat() {
                    all_msgs.push((i, msg));
                }
            }
            // Deliver messages
            deliver_messages(nodes, all_msgs);

            if nodes.iter().any(|n| n.is_leader()) {
                return;
            }
        }
    }

    fn deliver_messages(nodes: &mut Vec<RaftNode>, msgs: Vec<(usize, OutboundMessage)>) {
        let mut responses: Vec<(usize, OutboundMessage)> = Vec::new();
        for (_from_idx, out_msg) in msgs {
            let to_idx = nodes.iter().position(|n| n.id == out_msg.to);
            if let Some(idx) = to_idx {
                let replies = nodes[idx].handle_message(out_msg.msg);
                for reply in replies {
                    responses.push((idx, reply));
                }
            }
        }
        if !responses.is_empty() {
            deliver_messages(nodes, responses);
        }
    }

    /// Run tick-then-deliver for `rounds` rounds.
    ///
    /// Each round also forces leaders to send heartbeats so replication works
    /// without waiting for wall-clock time to advance.
    fn simulate_rounds(nodes: &mut Vec<RaftNode>, rounds: usize) {
        for _ in 0..rounds {
            let mut all_msgs: Vec<(usize, OutboundMessage)> = Vec::new();
            for (i, node) in nodes.iter_mut().enumerate() {
                for msg in node.tick() {
                    all_msgs.push((i, msg));
                }
                // Force heartbeats regardless of wall-clock elapsed time
                for msg in node.force_heartbeat() {
                    all_msgs.push((i, msg));
                }
            }
            deliver_messages(nodes, all_msgs);
        }
    }

    #[test]
    fn test_single_node_leader() {
        let mut node = RaftNode::new("n1".to_string(), vec![]);

        // Force an immediate election by resetting the heartbeat timer
        node.last_heartbeat =
            Instant::now() - Duration::from_millis(500);

        let msgs = node.tick();
        // Single-node cluster: should become leader immediately, no messages needed
        assert!(node.is_leader(), "single node must become leader after timeout");
        // No peers, so no messages to send (or only heartbeat to self which is empty)
        assert!(
            msgs.is_empty() || msgs.iter().all(|m| m.to == "n1"),
            "single-node should emit no peer messages"
        );
    }

    #[test]
    fn test_three_node_election() {
        let peers = |exclude: &str| -> Vec<String> {
            vec!["n1", "n2", "n3"]
                .into_iter()
                .filter(|&id| id != exclude)
                .map(String::from)
                .collect()
        };

        let mut nodes = vec![
            RaftNode::new("n1".to_string(), peers("n1")),
            RaftNode::new("n2".to_string(), peers("n2")),
            RaftNode::new("n3".to_string(), peers("n3")),
        ];

        // Force election timeout on n1
        nodes[0].last_heartbeat = Instant::now() - Duration::from_millis(500);

        drive_until_leader(&mut nodes, 100);

        let leaders: Vec<&str> = nodes
            .iter()
            .filter(|n| n.is_leader())
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(leaders.len(), 1, "exactly one leader must be elected");
    }

    #[test]
    fn test_log_replication() {
        let peers = |exclude: &str| -> Vec<String> {
            vec!["n1", "n2", "n3"]
                .into_iter()
                .filter(|&id| id != exclude)
                .map(String::from)
                .collect()
        };

        let mut nodes = vec![
            RaftNode::new("n1".to_string(), peers("n1")),
            RaftNode::new("n2".to_string(), peers("n2")),
            RaftNode::new("n3".to_string(), peers("n3")),
        ];

        // Force n1 to be leader
        nodes[0].last_heartbeat = Instant::now() - Duration::from_millis(500);
        drive_until_leader(&mut nodes, 100);

        // Find leader index
        let leader_idx = nodes.iter().position(|n| n.is_leader()).unwrap();

        // Propose a command
        let idx = nodes[leader_idx]
            .propose(ClusterCommand::CreateIndex {
                name: "test_index".to_string(),
                schema_json: "{}".to_string(),
            })
            .expect("propose should succeed on leader");
        assert_eq!(idx, 1);

        // Replicate: a few rounds of simulation
        simulate_rounds(&mut nodes, 20);

        // All nodes should have the entry in their log
        for node in &nodes {
            assert!(
                node.log_len() >= 1,
                "node {} should have the log entry",
                node.id
            );
        }
    }

    #[test]
    fn test_leader_commit() {
        let peers = |exclude: &str| -> Vec<String> {
            vec!["n1", "n2", "n3"]
                .into_iter()
                .filter(|&id| id != exclude)
                .map(String::from)
                .collect()
        };

        let mut nodes = vec![
            RaftNode::new("n1".to_string(), peers("n1")),
            RaftNode::new("n2".to_string(), peers("n2")),
            RaftNode::new("n3".to_string(), peers("n3")),
        ];

        nodes[0].last_heartbeat = Instant::now() - Duration::from_millis(500);
        drive_until_leader(&mut nodes, 100);

        let leader_idx = nodes.iter().position(|n| n.is_leader()).unwrap();
        nodes[leader_idx]
            .propose(ClusterCommand::AddNode {
                node_id: "n4".to_string(),
                address: "10.0.0.4:9200".to_string(),
            })
            .unwrap();

        simulate_rounds(&mut nodes, 30);

        let leader_commit = nodes[leader_idx].commit_index();
        assert!(
            leader_commit >= 1,
            "leader should have committed the entry (got {})",
            leader_commit
        );
    }

    #[test]
    fn test_leader_failure_reelection() {
        let peers = |exclude: &str| -> Vec<String> {
            vec!["n1", "n2", "n3"]
                .into_iter()
                .filter(|&id| id != exclude)
                .map(String::from)
                .collect()
        };

        let mut nodes = vec![
            RaftNode::new("n1".to_string(), peers("n1")),
            RaftNode::new("n2".to_string(), peers("n2")),
            RaftNode::new("n3".to_string(), peers("n3")),
        ];

        nodes[0].last_heartbeat = Instant::now() - Duration::from_millis(500);
        drive_until_leader(&mut nodes, 100);

        let old_leader_idx = nodes.iter().position(|n| n.is_leader()).unwrap();
        let old_leader_id = nodes[old_leader_idx].id.clone();

        // "Kill" the leader — remove it from the simulation
        nodes.remove(old_leader_idx);

        // Force election timeout on only one of the remaining nodes so there's
        // no split-vote scenario — the first one to time out will win.
        nodes[0].last_heartbeat = Instant::now() - Duration::from_millis(500);

        drive_until_leader(&mut nodes, 300);

        let new_leaders: Vec<&str> = nodes
            .iter()
            .filter(|n| n.is_leader())
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(new_leaders.len(), 1, "exactly one new leader after failure");
        assert_ne!(
            new_leaders[0], old_leader_id,
            "new leader must be different from the failed one"
        );
    }

    #[test]
    fn test_propose_command() {
        let mut node = RaftNode::new("n1".to_string(), vec![]);
        node.last_heartbeat = Instant::now() - Duration::from_millis(500);
        node.tick(); // become leader

        assert!(node.is_leader());

        let idx = node
            .propose(ClusterCommand::CreateIndex {
                name: "products".to_string(),
                schema_json: r#"{"properties":{"title":{"type":"text"}}}"#.to_string(),
            })
            .expect("propose on single-node leader should succeed");

        assert_eq!(idx, 1);
        assert_eq!(node.commit_index(), 1); // single node commits immediately
    }

    #[test]
    fn test_propose_on_follower_fails() {
        let mut node = RaftNode::new("n1".to_string(), vec!["n2".to_string()]);
        // Still a follower — should error
        let result = node.propose(ClusterCommand::DeleteIndex {
            name: "foo".to_string(),
        });
        assert!(result.is_err(), "propose on follower must fail");
    }
}
