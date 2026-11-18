//! Minimal Raft consensus implementation — no external Raft library.
//!
//! Implements the Raft protocol as described in "In Search of an Understandable
//! Consensus Algorithm" (Ongaro & Ousterhout, 2014).
//!
//! Safety properties upheld:
//! - Election Safety: at most one leader per term
//! - Leader Append-Only: a leader never overwrites a log prefix with different
//!   commands. Compaction replaces an applied prefix with a snapshot of those
//!   same commands; it does not rewrite them.
//! - Log Matching: if two logs contain an entry with the same index and term,
//!   the logs are identical in all entries up through that index
//! - Leader Completeness: if a log entry is committed in a given term, that entry
//!   will be present in the logs of all leaders for all higher-numbered terms.
//!   A compacted prefix counts: it is in the snapshot, and `last_log_index` /
//!   `last_log_term` include that snapshot.
//! - State Machine Safety: if a server has applied a log entry at a given index,
//!   no other server will ever apply a different log entry for that same index

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::shard::{ShardOp, ShardResult};

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
    /// `PUT /{index}/_settings` body, applied by the engine on every member.
    ///
    /// `settings_json` is the request body as the accepting node validated it.
    /// [`crate::metadata::ClusterMetadata`] records it; the engine applier is
    /// what merges it into the live index.
    UpdateIndexSettings {
        index: String,
        settings_json: String,
    },
    /// Replicas of one shard that a later read or copy may use.
    ///
    /// `nodes` is the subset of the create-time replica list that confirmed
    /// the last acknowledged write. The engine applier drops any id that is
    /// not a placed replica. An empty `nodes` list means no replica is in
    /// sync; the primary is not a member of this list.
    SetShardInSync {
        index: String,
        shard: u32,
        nodes: Vec<String>,
    },
    /// A client-originated metadata command.
    ///
    /// `request_id` is unique on the node that accepted the caller. That node
    /// completes the waiting request when this entry is applied. Other nodes
    /// apply `inner` and ignore the id. The wrapper is persisted with the
    /// log entry so a restarted node, which opens the file log, can still
    /// tell client entries from internal ones.
    ClientMetadata {
        /// Node that accepted the caller. Only that node completes a waiter.
        origin: String,
        request_id: u64,
        inner: Box<ClusterCommand>,
    },
    /// Entry a new leader appends at the start of its term.
    ///
    /// A leader only counts replicas for entries of its own term (§5.4.2).
    /// Without one, entries left over from an earlier term stay uncommitted
    /// until a client proposes something. Appliers ignore it. It is the last
    /// variant so the serde encoding of every older variant is unchanged.
    Noop,
}

impl ClusterCommand {
    /// The originator's request id, if this entry was proposed for a waiting caller.
    pub fn client_request_id(&self) -> Option<u64> {
        match self {
            ClusterCommand::ClientMetadata { request_id, .. } => Some(*request_id),
            _ => None,
        }
    }

    /// Node that accepted the waiting caller, if this entry is a client proposal.
    pub fn client_origin(&self) -> Option<&str> {
        match self {
            ClusterCommand::ClientMetadata { origin, .. } => Some(origin.as_str()),
            _ => None,
        }
    }

    /// The metadata operation, unwrapping one [`ClusterCommand::ClientMetadata`] layer.
    pub fn as_metadata(&self) -> &ClusterCommand {
        match self {
            ClusterCommand::ClientMetadata { inner, .. } => inner,
            other => other,
        }
    }
}

/// How many applied entries sit past the snapshot before the prefix is compacted.
///
/// Zero would snapshot on every apply. 64 keeps the on-disk log to a short
/// suffix without snapshotting a cluster that has only created a handful of
/// indices. Tests that need a snapshot sooner call [`RaftNode::compact_applied`]
/// or [`RaftNode::set_snapshot_every`].
pub const DEFAULT_SNAPSHOT_EVERY: u64 = 64;

/// InstallSnapshot has to fit in one cluster frame (10 MiB). Above this the
/// prefix stays in the log and catch-up keeps using AppendEntries.
const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;

/// Committed metadata prefix that the log no longer stores entry by entry.
///
/// `entries` are the commands with index `<= last_included_index`, in index
/// order. A member that never stored them applies this list. The bytes are
/// the commands, not a folded image and not document bodies: replaying them
/// is what recreates index metadata. A history that does not fit in 8 MiB
/// is left in the log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaftSnapshot {
    pub last_included_index: u64,
    pub last_included_term: u64,
    pub entries: Vec<LogEntry>,
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
    /// Follower → leader: please propose this metadata command.
    ///
    /// Not a Raft RPC. A follower that accepted an HTTP metadata write forwards
    /// it here instead of appending to its own log. The leader proposes it
    /// through the normal path; the follower completes its caller when the
    /// committed entry is applied locally.
    ForwardProposal {
        term: u64,
        from: String,
        command: ClusterCommand,
    },
    /// Leader (or whoever received [`RaftMessage::ForwardProposal`]) → originator:
    /// this node did not propose the command.
    ForwardRejected {
        term: u64,
        from: String,
        request_id: u64,
        /// Who the rejector believes the leader is, if anyone.
        leader_id: Option<String>,
        reason: String,
    },
    /// Caller → shard owner, on its own connection. Not a Raft RPC and not
    /// appended to the log. The owner answers with [`RaftMessage::ShardResponse`]
    /// on the same connection.
    ShardRequest { request_id: u64, op: ShardOp },
    /// Shard owner → caller. See [`RaftMessage::ShardRequest`].
    ShardResponse {
        request_id: u64,
        result: ShardResult,
    },
    /// Leader → follower: the log prefix at or below `last_included_index`
    /// has been compacted. `data` is a [`RaftSnapshot`]. The follower applies
    /// any command in it that it has not applied, then discards its own
    /// prefix through that index.
    InstallSnapshot {
        term: u64,
        leader_id: String,
        last_included_index: u64,
        last_included_term: u64,
        data: Vec<u8>,
    },
    /// Follower → leader: response to [`RaftMessage::InstallSnapshot`].
    InstallSnapshotResponse {
        from: String,
        term: u64,
        /// Snapshot index the follower has stored. 0 when `success` is false.
        last_included_index: u64,
        success: bool,
    },
}

fn message_term(msg: &RaftMessage) -> u64 {
    match msg {
        RaftMessage::AppendEntries { term, .. }
        | RaftMessage::AppendEntriesResponse { term, .. }
        | RaftMessage::RequestVote { term, .. }
        | RaftMessage::RequestVoteResponse { term, .. }
        | RaftMessage::ForwardProposal { term, .. }
        | RaftMessage::ForwardRejected { term, .. }
        | RaftMessage::InstallSnapshot { term, .. }
        | RaftMessage::InstallSnapshotResponse { term, .. } => *term,
        // Not a Raft term. A shard frame that reaches the state machine must
        // not step a leader down.
        RaftMessage::ShardRequest { .. } | RaftMessage::ShardResponse { .. } => 0,
    }
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
    /// Applied prefix that has been removed from `log`. `None` until the
    /// first compaction. `last_log_index` / `last_log_term` fall back to it
    /// when the in-memory log is empty.
    snapshot: Option<RaftSnapshot>,
    /// A snapshot received or loaded from disk that [`RaftNode::take_installed_entries`]
    /// has not handed to the applier yet.
    pending_snapshot: Option<RaftSnapshot>,
    /// Applied entries past the snapshot before [`RaftNode::maybe_compact`]
    /// runs. Zero disables that automatic compaction.
    snapshot_every: u64,

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
    /// All nodes start as followers in term 0. A `peers` entry equal to this
    /// node's own id is **excluded** (with a warning): the documented config
    /// convention lists the full membership on every node, so a self entry is
    /// expected in the input — but leaving it in made `cluster_size` one too
    /// large (a 3-node ring computed majority = 3, i.e. unanimity — zero fault
    /// tolerance) and had every node address its own RequestVote to itself,
    /// counting its own vote twice (#1168).
    pub fn new(id: String, peers: Vec<String>) -> Self {
        if peers.iter().any(|p| p == &id) {
            warn!(
                node = %id,
                "peer list included this node's own id — excluded (self is not a peer: \
                 cluster_size would be wrong and the node would count its own vote twice)"
            );
        }
        let peers: Vec<String> = peers.into_iter().filter(|p| p != &id).collect();
        let election_timeout = Self::random_election_timeout();
        RaftNode {
            id,
            current_term: 0,
            voted_for: None,
            log: vec![],
            persistent: None,
            snapshot: None,
            pending_snapshot: None,
            snapshot_every: DEFAULT_SNAPSHOT_EVERY,
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

        if let Some(bytes) = flog.snapshot_bytes() {
            let snap: RaftSnapshot = serde_json::from_slice(bytes)
                .map_err(|e| anyhow::anyhow!("decode raft snapshot: {e}"))?;
            if snap.last_included_term > node.current_term {
                node.current_term = snap.last_included_term;
            }
            // Replay applies this before the log suffix. A restart has an
            // empty metadata store; the commands in the snapshot are what
            // put it back.
            node.pending_snapshot = Some(snap.clone());
            node.snapshot = Some(snap);
        }
        let covered = node.snapshot_index();

        // Replay every persisted entry into the in-memory log.  The
        // in-memory log is kept 1-indexed (sentinel at index 0) to
        // match the rest of this file. Indexes the snapshot already
        // covers are not loaded: the file may still hold them if a
        // rewrite did not finish, and applying them twice is the
        // applier's job only for the pending snapshot.
        let last = flog.last_index();
        for idx in 1..=last {
            if idx <= covered {
                continue;
            }
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
        if node.commit_index < covered {
            node.commit_index = covered;
        }
        // A vote is only valid in the term it was cast. A hard-state term
        // ahead of the log (we campaigned or stepped down without appending)
        // wins; a missing hard state leaves the term reconstructed from the log.
        if flog.hard_term() > node.current_term {
            node.current_term = flog.hard_term();
        }
        if flog.hard_term() == node.current_term && flog.hard_term() > 0 {
            node.voted_for = flog.hard_vote().map(str::to_string);
        }
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
        let msg_term = message_term(&msg);
        if msg_term > self.current_term {
            debug!(
                node = %self.id,
                old_term = self.current_term,
                new_term = msg_term,
                "Higher term observed — stepping down to follower"
            );
            // The vote from the previous term must not survive into this one.
            if !self.write_hard_state(msg_term, None) {
                warn!(
                    node = %self.id,
                    term = msg_term,
                    "failed to persist the higher term; stepping down anyway"
                );
            }
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
                out.extend(self.handle_request_vote(
                    term,
                    candidate_id,
                    last_log_index,
                    last_log_term,
                ));
            }
            RaftMessage::RequestVoteResponse {
                from,
                term,
                vote_granted,
            } => {
                out.extend(self.handle_request_vote_response(from, term, vote_granted));
            }
            RaftMessage::ForwardProposal {
                term,
                from,
                command,
            } => {
                if self.state == RaftState::Leader && term <= self.current_term {
                    match self.propose(command) {
                        Ok(_) => out.extend(self.force_heartbeat()),
                        Err(e) => {
                            warn!(node = %self.id, error = %e, "leader failed to propose forwarded command");
                        }
                    }
                } else {
                    out.push(OutboundMessage {
                        to: from,
                        msg: RaftMessage::ForwardRejected {
                            term: self.current_term,
                            from: self.id.clone(),
                            request_id: command.client_request_id().unwrap_or(0),
                            leader_id: self.leader_id.clone(),
                            reason: format!(
                                "node {} is not the raft leader (state={}, leader={:?})",
                                self.id, self.state, self.leader_id
                            ),
                        },
                    });
                }
            }
            RaftMessage::ForwardRejected { .. } => {
                // Term bookkeeping already ran above. The cluster node matches
                // the request id to a waiting caller; the state machine has
                // nothing else to record.
            }
            RaftMessage::ShardRequest { .. } | RaftMessage::ShardResponse { .. } => {
                // The transport answers these on the connection that carried
                // them. They are not log entries.
            }
            RaftMessage::InstallSnapshot {
                term,
                leader_id,
                last_included_index,
                last_included_term,
                data,
            } => {
                out.extend(self.handle_install_snapshot(
                    term,
                    leader_id,
                    last_included_index,
                    last_included_term,
                    data,
                ));
            }
            RaftMessage::InstallSnapshotResponse {
                from,
                term,
                last_included_index,
                success,
            } => {
                out.extend(self.handle_install_snapshot_response(
                    from,
                    term,
                    last_included_index,
                    success,
                ));
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
        self.append_own(cmd)
    }

    /// Append `cmd` at the current term, durably, to this leader's log.
    ///
    /// Shared by [`RaftNode::propose`] and the no-op a new leader appends.
    fn append_own(&mut self, cmd: ClusterCommand) -> Result<u64> {
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
            if let Err(e) = flog.set_log_end(index) {
                flog.truncate_from(index);
                let _ = flog.set_log_end(index.saturating_sub(1));
                return Err(anyhow::anyhow!("persist raft log end: {e}"));
            }
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

    /// The peer ids this node addresses (self already excluded — see
    /// [`RaftNode::new`]). Exposed so a caller can assert the config-shape
    /// fix (#1168): a membership list that included this node's own id must
    /// not reach the state machine as a peer.
    pub fn peers(&self) -> &[String] {
        &self.peers
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
            self.last_heartbeat_sent = Instant::now() - Duration::from_millis(1000);
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

    /// Index covered by the snapshot. 0 when this node has not compacted.
    pub fn snapshot_index(&self) -> u64 {
        self.snapshot
            .as_ref()
            .map(|snap| snap.last_included_index)
            .unwrap_or(0)
    }

    /// Applied entries past the snapshot before automatic compaction.
    /// Zero disables it. The default is [`DEFAULT_SNAPSHOT_EVERY`].
    pub fn set_snapshot_every(&mut self, every: u64) {
        self.snapshot_every = every;
    }

    /// Commands from a snapshot that this process has not applied yet.
    ///
    /// Advances `last_applied` through the snapshot index so [`RaftNode::ready`]
    /// does not look for those commands in the log. Empty when nothing is pending.
    pub fn take_installed_entries(&mut self) -> Vec<LogEntry> {
        let Some(snap) = self.pending_snapshot.take() else {
            return Vec::new();
        };
        let from = self.last_applied;
        if snap.last_included_index > self.last_applied {
            self.last_applied = snap.last_included_index;
        }
        let mut entries: Vec<LogEntry> = snap
            .entries
            .into_iter()
            .filter(|entry| entry.index > from)
            .collect();
        entries.sort_by_key(|entry| entry.index);
        entries
    }

    /// Replace the applied prefix with a snapshot and drop it from the log.
    ///
    /// No-op when nothing has been applied or the snapshot already covers
    /// `last_applied`. Returns false when the snapshot could not be made
    /// durable; the log is unchanged in that case.
    pub fn compact_applied(&mut self) -> bool {
        let through = self.last_applied;
        if through == 0 || self.snapshot_index() >= through {
            return false;
        }
        let mut by_index: BTreeMap<u64, LogEntry> = BTreeMap::new();
        if let Some(prev) = &self.snapshot {
            for entry in &prev.entries {
                if entry.index <= through {
                    by_index.insert(entry.index, entry.clone());
                }
            }
        }
        for entry in &self.log {
            if entry.index > 0 && entry.index <= through {
                by_index.insert(entry.index, entry.clone());
            }
        }
        let Some(term) = by_index.get(&through).map(|entry| entry.term) else {
            warn!(
                node = %self.id,
                through,
                "cannot compact: the applied index is not in the log or the snapshot"
            );
            return false;
        };
        let snap = RaftSnapshot {
            last_included_index: through,
            last_included_term: term,
            entries: by_index.into_values().collect(),
        };
        if !self.persist_snapshot(&snap) {
            return false;
        }
        // The snapshot is durable. Drop the prefix even if rewriting the
        // file fails: the next open ignores indexes the snapshot covers.
        if !self.discard_file_prefix(through) && self.persistent.is_some() {
            warn!(
                node = %self.id,
                through,
                "raft snapshot is stored but the log file still has the prefix"
            );
        }
        self.log.retain(|entry| entry.index > through);
        info!(
            node = %self.id,
            through,
            entries = snap.entries.len(),
            "compacted raft log prefix into a snapshot"
        );
        self.snapshot = Some(snap);
        true
    }

    /// Compact when the applied prefix past the snapshot reaches
    /// [`RaftNode::set_snapshot_every`].
    pub fn maybe_compact(&mut self) {
        if self.snapshot_every == 0 {
            return;
        }
        let covered = self.snapshot_index();
        if self.last_applied.saturating_sub(covered) >= self.snapshot_every {
            let _ = self.compact_applied();
        }
    }

    /// Term of the log entry at `index`, if that entry is still present.
    ///
    /// A waiting proposer uses this to notice that its uncommitted entry was
    /// truncated after a leadership change. `None` means the index is gone.
    pub fn entry_term(&self, index: u64) -> Option<u64> {
        self.log_entry(index).map(|e| e.term)
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    fn persist_commit_index(&mut self) {
        if let Some(flog) = self.persistent.as_mut() {
            if let Err(e) = flog.set_commit_index(self.commit_index) {
                warn!(node = %self.id, error = %e, "failed to persist raft commit index");
            }
        }
    }

    /// Persist `term` and `voted_for` before they take effect.
    ///
    /// Returns `true` when there is no file log, or the write replaced
    /// `hard.meta`. A `false` means the caller must not grant a vote or
    /// start an election: a crash would otherwise forget the vote and
    /// grant a second one in the same term.
    fn write_hard_state(&mut self, term: u64, voted_for: Option<String>) -> bool {
        let Some(flog) = self.persistent.as_mut() else {
            return true;
        };
        match flog.set_hard_state(term, voted_for.as_deref()) {
            Ok(()) => true,
            Err(e) => {
                warn!(node = %self.id, error = %e, term, "failed to persist raft term and vote");
                false
            }
        }
    }

    /// Copy the in-memory log onto the file log and fsync it.
    ///
    /// Heartbeats that do not change the log do not fsync. Returns `false`
    /// when the file log could not be updated; the caller must not tell the
    /// leader the append succeeded.
    fn mirror_log(&mut self) -> bool {
        if self.persistent.is_none() {
            return true;
        }
        let node_id = self.id.clone();
        let mem_last = self.last_log_index();
        let encoded: Vec<(u64, u64, Vec<u8>)> = match self
            .log
            .iter()
            .map(|entry| {
                serde_json::to_vec(&entry.command).map(|payload| (entry.index, entry.term, payload))
            })
            .collect()
        {
            Ok(entries) => entries,
            Err(e) => {
                warn!(node = %node_id, error = %e, "encode raft entry");
                return false;
            }
        };
        let flog = self
            .persistent
            .as_mut()
            .expect("persistent log checked above");
        let mut dirty = false;
        if flog.last_index() > mem_last {
            flog.truncate_from(mem_last + 1);
            dirty = true;
        }
        for (index, term, payload) in &encoded {
            match flog.term_of(*index) {
                Some(have) if have == *term => {}
                Some(_) => {
                    flog.truncate_from(*index);
                    if let Err(e) = flog.append(*term, *index, payload) {
                        warn!(node = %node_id, error = %e, index, "append raft log");
                        return false;
                    }
                    dirty = true;
                }
                None => {
                    if let Err(e) = flog.append(*term, *index, payload) {
                        warn!(node = %node_id, error = %e, index, "append raft log");
                        return false;
                    }
                    dirty = true;
                }
            }
        }
        if !dirty {
            match flog.persisted_log_end() {
                Some(end) if end == mem_last => return true,
                None if flog.last_index() == mem_last => return true,
                _ => {}
            }
        }
        if let Err(e) = flog.fsync() {
            warn!(node = %node_id, error = %e, "fsync raft log");
            return false;
        }
        if let Err(e) = flog.set_log_end(mem_last) {
            warn!(node = %node_id, error = %e, "persist raft log end");
            return false;
        }
        true
    }

    fn cluster_size(&self) -> usize {
        self.peers.len() + 1 // peers + self
    }

    fn majority(&self) -> usize {
        self.cluster_size() / 2 + 1
    }

    fn last_log_index(&self) -> u64 {
        self.log
            .last()
            .map(|e| e.index)
            .unwrap_or_else(|| self.snapshot_index())
    }

    fn last_log_term(&self) -> u64 {
        self.log.last().map(|e| e.term).unwrap_or_else(|| {
            self.snapshot
                .as_ref()
                .map(|snap| snap.last_included_term)
                .unwrap_or(0)
        })
    }

    /// Retrieve a log entry by its 1-based Raft index.
    fn log_entry(&self, index: u64) -> Option<&LogEntry> {
        if index == 0 {
            return None;
        }
        // Find entry with matching index (log is stored in order)
        self.log.iter().find(|e| e.index == index)
    }

    /// Term of the log entry at `index`, the snapshot's term when `index`
    /// falls inside a compacted prefix, or 0 when this node does not have it.
    fn log_term_at(&self, index: u64) -> u64 {
        self.term_at(index).unwrap_or(0)
    }

    fn term_at(&self, index: u64) -> Option<u64> {
        if index == 0 {
            return Some(0);
        }
        if let Some(entry) = self.log_entry(index) {
            return Some(entry.term);
        }
        let snap = self.snapshot.as_ref()?;
        if index > snap.last_included_index {
            return None;
        }
        if index == snap.last_included_index {
            return Some(snap.last_included_term);
        }
        snap.entries
            .iter()
            .find(|entry| entry.index == index)
            .map(|entry| entry.term)
    }

    fn persist_snapshot(&mut self, snap: &RaftSnapshot) -> bool {
        let bytes = match serde_json::to_vec(snap) {
            Ok(bytes) => bytes,
            Err(e) => {
                warn!(node = %self.id, error = %e, "encode raft snapshot");
                return false;
            }
        };
        if bytes.len() > MAX_SNAPSHOT_BYTES {
            warn!(
                node = %self.id,
                bytes = bytes.len(),
                "raft snapshot exceeds 8 MiB; leaving the log prefix in place"
            );
            return false;
        }
        let Some(flog) = self.persistent.as_mut() else {
            return true;
        };
        match flog.save_snapshot(&bytes) {
            Ok(()) => true,
            Err(e) => {
                warn!(node = %self.id, error = %e, "failed to persist raft snapshot");
                false
            }
        }
    }

    fn discard_file_prefix(&mut self, through: u64) -> bool {
        let Some(flog) = self.persistent.as_mut() else {
            return true;
        };
        match flog.rewrite_without_prefix(through) {
            Ok(()) => true,
            Err(e) => {
                warn!(node = %self.id, error = %e, through, "failed to rewrite raft log");
                false
            }
        }
    }

    fn snapshot_response(
        &self,
        to: String,
        last_included_index: u64,
        success: bool,
    ) -> Vec<OutboundMessage> {
        vec![OutboundMessage {
            to,
            msg: RaftMessage::InstallSnapshotResponse {
                from: self.id.clone(),
                term: self.current_term,
                last_included_index,
                success,
            },
        }]
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

    /// Take leadership and append this term's no-op entry.
    ///
    /// Returns false when the no-op could not be made durable. The node is a
    /// follower again in that case, as after a failed vote write in
    /// [`RaftNode::start_election`], and sends nothing.
    fn become_leader(&mut self) -> bool {
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

        // §5.4.2 / §8: commit an entry of this term so that entries from
        // earlier terms commit with it, without waiting for a client write.
        if let Err(e) = self.append_own(ClusterCommand::Noop) {
            warn!(node = %self.id, error = %e, term = self.current_term, "persist leader no-op; stepping down");
            self.become_follower(None);
            return false;
        }
        true
    }

    fn start_election(&mut self) -> Vec<OutboundMessage> {
        let new_term = self.current_term + 1;
        let vote = self.id.clone();
        // The self-vote has to be durable before the RequestVote goes out.
        // A failed disk stays a follower and tries again after the next timeout.
        if !self.write_hard_state(new_term, Some(vote.clone())) {
            self.last_heartbeat = Instant::now();
            return vec![];
        }
        self.current_term = new_term;
        self.state = RaftState::Candidate;
        self.voted_for = Some(vote);
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
            if !self.become_leader() {
                return vec![];
            }
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

    /// Build an AppendEntries RPC for the given peer, or an InstallSnapshot
    /// when the peer's next index falls inside the compacted prefix.
    fn build_append_entries(&self, peer: &str) -> Option<OutboundMessage> {
        let next_idx = *self.next_index.get(peer).unwrap_or(&1);
        if let Some(snap) = &self.snapshot {
            if next_idx <= snap.last_included_index {
                let data = match serde_json::to_vec(snap) {
                    Ok(data) => data,
                    Err(e) => {
                        warn!(node = %self.id, error = %e, "encode raft snapshot for install");
                        return None;
                    }
                };
                return Some(OutboundMessage {
                    to: peer.to_string(),
                    msg: RaftMessage::InstallSnapshot {
                        term: self.current_term,
                        leader_id: self.id.clone(),
                        last_included_index: snap.last_included_index,
                        last_included_term: snap.last_included_term,
                        data,
                    },
                });
            }
        }
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

        // A prev index inside the compacted prefix is not a missing entry.
        // The leader should send InstallSnapshot; tell it how far this
        // node already is so it stops walking backwards.
        if let Some(snap) = &self.snapshot {
            if prev_log_index > 0 && prev_log_index < snap.last_included_index {
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
        }
        let matched_snapshot = self.snapshot.as_ref().is_some_and(|snap| {
            prev_log_index == snap.last_included_index && prev_log_term == snap.last_included_term
        });

        // 2. Reply false if log doesn't contain an entry at prevLogIndex
        //    whose term matches prevLogTerm (§5.3). The snapshot boundary
        //    counts as that entry.
        if prev_log_index > 0 && !matched_snapshot {
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
                    //    follow it (§5.3). The shortened log is durable before
                    //    the rejection, so a crash cannot bring the tail back
                    //    and win an election with it.
                    self.log.retain(|e| e.index < prev_log_index);
                    let _ = self.mirror_log();
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

        // 4. Append any new entries not already in the log. Commands the
        // snapshot already covers are not appended again.
        for entry in &entries {
            if self
                .snapshot
                .as_ref()
                .is_some_and(|snap| entry.index <= snap.last_included_index)
            {
                continue;
            }
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

        // Durable before the success reply. A heartbeat that changes nothing
        // does not fsync. A failed mirror is a rejection: the leader retries,
        // and this process still has the entries in memory.
        if !self.mirror_log() {
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

        // 5. If leaderCommit > commitIndex, set commitIndex =
        //    min(leaderCommit, index of last new entry) (§5.3)
        if leader_commit > self.commit_index {
            self.commit_index = leader_commit.min(self.last_log_index());
            self.persist_commit_index();
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
            let prev_commit = self.commit_index;
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
            // Tell followers the new commit index now. Waiting for the next
            // heartbeat tick leaves a committed metadata write invisible on
            // the other members for a full tick after the caller was allowed
            // to proceed.
            if self.commit_index > prev_commit {
                self.persist_commit_index();
                return self.send_heartbeats();
            }
        } else {
            // A follower that already snapshotted past `next_index` reports
            // its real log end. Jump forward to that. A follower with nothing
            // that sits behind our snapshot lands on the snapshot index so
            // the retry is InstallSnapshot, not a walk back to index 1.
            // Anything else keeps the one-step decrement this node used
            // before snapshots existed.
            let reported = match_index.saturating_add(1).max(1);
            let floor = self.snapshot_index();
            let ni = self.next_index.entry(from.clone()).or_insert(1);
            if reported > *ni {
                *ni = reported;
            } else if floor > 0 && match_index < floor {
                *ni = floor;
            } else {
                *ni = ni.saturating_sub(1).max(1);
            }

            // Retry immediately with the updated next_index
            if let Some(msg) = self.build_append_entries(&from) {
                return vec![msg];
            }
        }
        vec![]
    }

    fn handle_install_snapshot(
        &mut self,
        term: u64,
        leader_id: String,
        last_included_index: u64,
        last_included_term: u64,
        data: Vec<u8>,
    ) -> Vec<OutboundMessage> {
        if term < self.current_term {
            return self.snapshot_response(leader_id, 0, false);
        }

        self.last_heartbeat = Instant::now();
        if self.state != RaftState::Follower {
            self.become_follower(Some(leader_id.clone()));
        } else {
            self.leader_id = Some(leader_id.clone());
        }

        if self.snapshot_index() >= last_included_index && last_included_index > 0 {
            return self.snapshot_response(leader_id, self.snapshot_index(), true);
        }

        let snap: RaftSnapshot = match serde_json::from_slice(&data) {
            Ok(snap) => snap,
            Err(e) => {
                warn!(node = %self.id, error = %e, "raft snapshot payload is not decodable");
                return self.snapshot_response(leader_id, 0, false);
            }
        };
        if snap.last_included_index != last_included_index
            || snap.last_included_term != last_included_term
            || !snap
                .entries
                .iter()
                .any(|entry| entry.index == last_included_index && entry.term == last_included_term)
        {
            warn!(
                node = %self.id,
                last_included_index,
                "raft snapshot payload does not match its index and term"
            );
            return self.snapshot_response(leader_id, 0, false);
        }

        let already_applied = self.last_applied >= last_included_index
            && self.term_at(last_included_index) == Some(last_included_term);
        let boundary_matches = self
            .log_entry(last_included_index)
            .is_some_and(|entry| entry.term == last_included_term);

        if !self.persist_snapshot(&snap) {
            return self.snapshot_response(leader_id, 0, false);
        }

        if boundary_matches {
            self.log.retain(|entry| entry.index > last_included_index);
        } else {
            // The snapshot is a committed prefix. A local tail that does not
            // contain that index and term is not part of this leader's log.
            self.log.clear();
        }
        self.snapshot = Some(snap.clone());
        if self.commit_index < last_included_index {
            self.commit_index = last_included_index;
            self.persist_commit_index();
        }
        if !already_applied {
            self.pending_snapshot = Some(snap);
        }
        // Logical end first, so a crash cannot resurrect a conflicting tail,
        // then drop the bytes the snapshot replaced.
        if !self.mirror_log() {
            warn!(node = %self.id, "installed raft snapshot but the log suffix did not persist");
            return self.snapshot_response(leader_id, 0, false);
        }
        if !self.discard_file_prefix(last_included_index) && self.persistent.is_some() {
            warn!(
                node = %self.id,
                last_included_index,
                "raft snapshot is stored but the log file still has the prefix"
            );
        }
        info!(
            node = %self.id,
            last_included_index,
            "installed raft snapshot"
        );
        self.snapshot_response(leader_id, last_included_index, true)
    }

    fn handle_install_snapshot_response(
        &mut self,
        from: String,
        term: u64,
        last_included_index: u64,
        success: bool,
    ) -> Vec<OutboundMessage> {
        if self.state != RaftState::Leader || term < self.current_term || !success {
            return vec![];
        }
        self.match_index
            .entry(from.clone())
            .and_modify(|m| *m = (*m).max(last_included_index))
            .or_insert(last_included_index);
        *self.next_index.entry(from.clone()).or_insert(1) = last_included_index.saturating_add(1);
        self.build_append_entries(&from).into_iter().collect()
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
            let can_vote =
                self.voted_for.is_none() || self.voted_for.as_deref() == Some(&candidate_id);

            // "Up-to-date" check: candidate's last log term > ours, OR
            //  same last log term and candidate's log is at least as long.
            let candidate_log_ok = last_log_term > self.last_log_term()
                || (last_log_term == self.last_log_term()
                    && last_log_index >= self.last_log_index());

            if can_vote && candidate_log_ok {
                // Grant only after the vote is durable. Otherwise a restart
                // in this term would vote for someone else.
                if self.write_hard_state(self.current_term, Some(candidate_id.clone())) {
                    self.voted_for = Some(candidate_id.clone());
                    self.last_heartbeat = Instant::now(); // reset election timer
                    vote_granted = true;
                    debug!(node = %self.id, candidate = %candidate_id, "Granting vote");
                }
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
                if !self.become_leader() {
                    return vec![];
                }
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

    /// A membership list that includes this node's own id (the documented
    /// full-membership config convention) must not reach the state machine as
    /// a peer (#1168): self-in-peers made cluster_size 4 in a 3-node ring —
    /// majority 3, i.e. unanimity, no fault tolerance — and had the node
    /// grant its own RequestVote to itself, counting its own vote twice.
    #[test]
    fn test_self_is_excluded_from_peers() {
        let node = RaftNode::new(
            "n1".to_string(),
            vec!["n1".to_string(), "n2".to_string(), "n3".to_string()],
        );
        assert_eq!(node.peers(), &["n2".to_string(), "n3".to_string()]);

        // And the majority reflects the REAL cluster: 3 members → 2.
        assert_eq!(node.majority(), 2);
    }

    #[test]
    fn test_single_node_leader() {
        let mut node = RaftNode::new("n1".to_string(), vec![]);

        // Force an immediate election by resetting the heartbeat timer
        node.last_heartbeat = Instant::now() - Duration::from_millis(500);

        let msgs = node.tick();
        // Single-node cluster: should become leader immediately, no messages needed
        assert!(
            node.is_leader(),
            "single node must become leader after timeout"
        );
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
        assert_eq!(idx, 2, "index 1 is the leader's no-op");

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

        assert_eq!(idx, 2, "index 1 is the leader's no-op");
        assert_eq!(node.commit_index(), 2); // single node commits immediately
    }

    #[test]
    fn a_new_leader_log_ends_with_a_noop_of_its_term() {
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
        let leader = nodes.iter().find(|n| n.is_leader()).expect("leader");
        let last = leader.log.last().expect("the leader appended an entry");
        assert_eq!(last.command, ClusterCommand::Noop);
        assert_eq!(last.term, leader.current_term());
        assert_eq!(last.index, leader.last_log_index());
    }

    #[test]
    fn a_leader_that_cannot_persist_its_noop_steps_down() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut n1 = RaftNode::with_storage("n1".to_string(), vec!["n2".to_string()], dir.path())
            .expect("open log");
        let mut n2 = RaftNode::new("n2".to_string(), vec!["n1".to_string()]);
        n1.force_election_timeout();
        let votes = n1.tick();
        assert_eq!(n1.state(), &RaftState::Candidate);
        // The vote is on disk. Block the next hard.meta replace, which is the
        // log-end write the no-op append needs.
        std::fs::create_dir(dir.path().join("hard.meta.tmp")).expect("block hard.meta");
        let mut replies = Vec::new();
        for m in votes {
            replies.extend(n2.handle_message(m.msg));
        }
        let mut out = Vec::new();
        for m in replies {
            out.extend(n1.handle_message(m.msg));
        }
        assert_eq!(n1.state(), &RaftState::Follower);
        assert!(out.is_empty(), "a node that stepped down sends nothing");
        assert_eq!(n1.log_len(), 0);
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

    /// n3 is in the membership but not in the simulation until after the
    /// prefix is compacted, so the create is not in any log it can copy.
    #[test]
    fn compacted_prefix_installs_on_a_peer_that_has_no_log() {
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
        ];
        nodes[0].last_heartbeat = Instant::now() - Duration::from_millis(500);
        drive_until_leader(&mut nodes, 100);
        let leader = nodes.iter().position(|n| n.is_leader()).expect("leader");
        nodes[leader]
            .propose(ClusterCommand::CreateIndex {
                name: "orders".to_string(),
                schema_json: "{}".to_string(),
            })
            .expect("propose");
        simulate_rounds(&mut nodes, 20);
        for node in &mut nodes {
            assert!(!node.ready().is_empty(), "{} did not apply", node.id);
            assert!(node.compact_applied(), "{} did not compact", node.id);
            assert_eq!(
                node.log_len(),
                0,
                "{} still has the create in its log",
                node.id
            );
            assert!(node.snapshot_index() >= 1, "{}", node.id);
        }

        nodes.push(RaftNode::new("n3".to_string(), peers("n3")));
        simulate_rounds(&mut nodes, 30);
        let late = nodes.iter_mut().find(|n| n.id == "n3").expect("n3");
        assert!(
            late.snapshot_index() >= 1,
            "a peer with an empty log must install the snapshot, not copy a log entry"
        );
        assert_eq!(late.log_len(), 0);
        let installed = late.take_installed_entries();
        assert!(
            installed.iter().any(|entry| {
                matches!(
                    &entry.command,
                    ClusterCommand::CreateIndex { name, .. } if name == "orders"
                )
            }),
            "the snapshot must carry the create: {installed:?}"
        );
    }
}
