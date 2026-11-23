//! Cluster node — wraps [`RaftNode`] + [`ClusterMetadata`] + transport.
//!
//! [`ClusterNode`] is the top-level runtime object that ties together:
//! - the pure Raft state machine ([`RaftNode`]),
//! - the replicated metadata store ([`ClusterMetadata`]),
//! - and an I/O transport ([`ClusterTransport`]) for sending/receiving messages.
//!
//! In production, the transport will be a gRPC/QUIC channel. In tests, an
//! in-memory channel is used (see [`transport`] module).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::metadata::ClusterMetadata;
use crate::propose::{
    ApplyEffect, ClusterHandle, ClusterView, MetadataApplier, ProposalRequest, ProposeError,
    RunLoopSignals,
};
use crate::raft::{ClusterCommand, OutboundMessage, RaftMessage, RaftNode, RaftState};
use crate::shard::NullShardRpc;

// ── Transport abstraction ─────────────────────────────────────────────────────

/// Asynchronous message transport between cluster nodes.
///
/// Implementors are responsible for serialisation and delivery. The Raft state
/// machine itself is purely synchronous — the transport bridges it to async I/O.
#[async_trait]
pub trait ClusterTransport: Send + Sync {
    /// Send a Raft message to the node identified by `to`.
    async fn send(&self, to: &str, msg: RaftMessage) -> Result<()>;

    /// Receive the next inbound message. Returns `(sender_id, message)`.
    async fn recv(&self) -> Result<(String, RaftMessage)>;

    /// Data-plane handle for document forwarding. The default fails every
    /// call; TCP and the in-memory bus override it.
    fn shard_rpc(&self) -> Arc<dyn crate::shard::ShardRpc> {
        Arc::new(NullShardRpc)
    }
}

// ── Cluster node ──────────────────────────────────────────────────────────────

/// A running cluster node.
///
/// Drives the Raft state machine forward via periodic ticks and delivers
/// committed log entries to the local [`ClusterMetadata`] state machine.
enum DrivePlan {
    Proposed { index: u64, term: u64 },
    Forward(Option<String>),
    Wait,
}

enum WaiterState {
    /// Not yet proposed or forwarded.
    Pending,
    /// Sent to `to`; do not send again until the leader changes or they reject.
    Forwarded { to: String },
    /// `to` rejected. `hint` is who they believe the leader is.
    Rejected { hint: Option<String> },
    /// Appended locally at `index`/`term`. Completed when that entry is applied.
    Proposed { index: u64, term: u64 },
}

struct Waiter {
    command: ClusterCommand,
    deadline: Instant,
    state: WaiterState,
    ack: tokio::sync::oneshot::Sender<Result<ApplyEffect, ProposeError>>,
}

pub struct ClusterNode {
    pub raft: RaftNode,
    pub metadata: ClusterMetadata,
    transport: Box<dyn ClusterTransport>,
    /// Cloneable proposer the engine/HTTP layer holds.
    handle: ClusterHandle,
    proposal_rx: mpsc::UnboundedReceiver<ProposalRequest>,
    waiters: HashMap<u64, Waiter>,
    applier: Option<Arc<dyn MetadataApplier>>,
    view: Arc<RwLock<ClusterView>>,
    /// Applied index for [`ClusterHandle::wait_applied`].
    applied_tx: watch::Sender<u64>,
    /// Role/leader/term change count for [`ClusterHandle::wait_role_change`].
    roles_tx: watch::Sender<u64>,
    /// Finished backfill passes for [`ClusterHandle::wait_backfill_pass`].
    /// Shared with the runner's backfill task.
    backfill_tx: Arc<watch::Sender<u64>>,
}

impl ClusterNode {
    /// Create a new cluster node with the given identity, peers, and transport.
    pub fn new(id: String, peers: Vec<String>, transport: Box<dyn ClusterTransport>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let view = Arc::new(RwLock::new(ClusterView {
            node_id: id.clone(),
            state: RaftState::Follower,
            leader_id: None,
            snapshot_index: 0,
            log_len: 0,
            commit_index: 0,
            applied_index: 0,
            term: 0,
        }));
        let (applied_tx, applied_rx) = watch::channel(0u64);
        let (roles_tx, roles_rx) = watch::channel(0u64);
        let (backfill_tx, backfill_rx) = watch::channel(0u64);
        let shard = transport.shard_rpc();
        let mut members = peers.clone();
        members.push(id.clone());
        members.sort();
        members.dedup();
        let handle = ClusterHandle::new(
            id.clone(),
            members,
            tx,
            Arc::clone(&view),
            RunLoopSignals {
                applied: applied_rx,
                roles: roles_rx,
                backfill: backfill_rx,
            },
            shard,
        );
        ClusterNode {
            raft: RaftNode::new(id, peers),
            metadata: ClusterMetadata::new(),
            transport,
            handle,
            proposal_rx: rx,
            waiters: HashMap::new(),
            applier: None,
            view,
            applied_tx,
            roles_tx,
            backfill_tx: Arc::new(backfill_tx),
        }
    }

    /// Like [`ClusterNode::new`], opening the Raft log at `storage_dir`.
    ///
    /// The committed entries are loaded into the Raft log and are not applied
    /// yet. Call [`ClusterNode::replay_committed`] after [`ClusterNode::set_applier`]
    /// so the engine sees the same entries the metadata store does. Applying
    /// here would run before the applier exists and a restarted index plan
    /// would stay in metadata only.
    pub fn with_storage(
        id: String,
        peers: Vec<String>,
        transport: Box<dyn ClusterTransport>,
        storage_dir: impl AsRef<std::path::Path>,
    ) -> Result<Self> {
        let mut node = Self::new(id.clone(), peers, transport);
        let peer_ids = node.raft.peers().to_vec();
        node.raft = RaftNode::with_storage(id, peer_ids, storage_dir)?;
        Ok(node)
    }

    /// Apply every committed log entry that this process has not applied yet.
    ///
    /// On a fresh open that is the whole committed prefix. The applier, when
    /// one is installed, runs for each entry before this returns. Safe to call
    /// again: a second call finds nothing pending.
    pub async fn replay_committed(&mut self) {
        let commit = self.raft.commit_index();
        if commit == 0 {
            info!(node = %self.raft.id, "raft log has no committed entries to replay");
            return;
        }
        info!(node = %self.raft.id, commit, "replaying committed raft log");
        self.apply_ready().await;
    }

    /// Handle the engine layer uses to propose metadata and read leadership.
    pub fn handle(&self) -> ClusterHandle {
        self.handle.clone()
    }

    /// Install the engine applier. Committed entries received after this call
    /// are applied to local storage as well as [`ClusterMetadata`].
    ///
    /// Must be set before the first proposal. Entries applied with no applier
    /// update only the in-memory metadata store.
    pub fn set_applier(&mut self, applier: Arc<dyn MetadataApplier>) {
        self.applier = Some(applier);
    }

    /// Applier installed by [`ClusterNode::set_applier`], if any.
    ///
    /// The runner uses this to copy replica documents off the Raft step.
    pub(crate) fn applier(&self) -> Option<Arc<dyn MetadataApplier>> {
        self.applier.clone()
    }

    /// Sender the runner bumps after each replica-backfill pass.
    pub(crate) fn backfill_passes(&self) -> Arc<watch::Sender<u64>> {
        Arc::clone(&self.backfill_tx)
    }

    /// See [`crate::raft::RaftNode::set_snapshot_every`].
    pub fn set_snapshot_every(&mut self, every: u64) {
        self.raft.set_snapshot_every(every);
    }

    /// Snapshot the applied prefix now. The run loop also does this after
    /// [`crate::raft::DEFAULT_SNAPSHOT_EVERY`] new applied entries.
    pub fn compact_applied(&mut self) -> bool {
        self.raft.compact_applied()
    }

    /// Index covered by the snapshot. 0 when this node has not compacted.
    pub fn snapshot_index(&self) -> u64 {
        self.raft.snapshot_index()
    }

    /// Tick the Raft state machine and dispatch any outbound messages.
    ///
    /// Call this periodically (e.g. every 10 ms) from the cluster run loop.
    pub async fn tick(&mut self) -> Result<()> {
        let msgs = self.raft.tick();
        self.dispatch(msgs).await?;
        self.apply_ready().await;
        Ok(())
    }

    /// Process a single inbound message from a peer.
    pub async fn handle_message(&mut self, from: &str, msg: RaftMessage) -> Result<()> {
        debug!(node = %self.raft.id, %from, "Handling inbound Raft message");
        let _ = from; // currently unused directly — sender is embedded in message
        if let RaftMessage::ForwardRejected {
            request_id,
            leader_id,
            ..
        } = &msg
        {
            self.note_rejection(*request_id, leader_id.clone());
        }
        let replies = self.raft.handle_message(msg);
        self.dispatch(replies).await?;
        self.apply_ready().await;
        Ok(())
    }

    /// Propose a command through Raft consensus (leader only).
    ///
    /// After calling this, the entry will be replicated and, once committed,
    /// automatically applied to the local [`ClusterMetadata`] via the run loop.
    pub fn propose(&mut self, cmd: ClusterCommand) -> Result<u64> {
        self.raft.propose(cmd)
    }

    /// Perform one iteration of the node event loop:
    /// 1. Wait for an inbound message, bounded by `tick_interval`.
    /// 2. If one arrives, deliver it to the Raft state machine.
    /// 3. Tick the state machine (advancing elections / heartbeats).
    ///
    /// This is the unit of work that [`crate::runner::ClusterRunner`] drives,
    /// so the caller can interleave it with a shutdown signal via
    /// `tokio::select!`. It is cancellation-safe: dropping the returned future
    /// before it resolves only abandons an in-flight `recv`/`send`, which Raft
    /// tolerates via retransmission.
    pub async fn step(&mut self, tick_interval: Duration) -> Result<()> {
        tokio::select! {
            biased;
            req = self.proposal_rx.recv() => {
                if let Some(req) = req {
                    self.accept_proposal(req).await;
                }
            }
            recv_result = self.transport.recv() => {
                match recv_result {
                    Ok((from, msg)) => {
                        if let Err(e) = self.handle_message(&from, msg).await {
                            warn!(node = %self.raft.id, error = %e, "Raft message handling error");
                        }
                    }
                    Err(e) => {
                        warn!(node = %self.raft.id, error = %e, "Transport recv error");
                    }
                }
            }
            _ = tokio::time::sleep(tick_interval) => {}
        }

        self.tick().await?;
        self.expire_waiters();
        self.pump_waiters().await;
        self.refresh_view();
        Ok(())
    }

    /// Run the node event loop forever (until an error is hit).
    ///
    /// Each iteration is a single [`ClusterNode::step`]: waits for an inbound
    /// message (with a timeout equal to the tick interval), delivers it to the
    /// Raft state machine, then ticks.
    ///
    /// In production you would wrap this with a `tokio::select!` that listens
    /// for a shutdown signal — see [`crate::runner::ClusterRunner::run`].
    pub async fn run(&mut self, tick_interval: Duration) -> Result<()> {
        info!(node = %self.raft.id, "Starting cluster node run loop");
        loop {
            self.step(tick_interval).await?;
        }
    }

    /// Whether this node is the current Raft leader.
    pub fn is_leader(&self) -> bool {
        self.raft.is_leader()
    }

    /// The node ID of the current Raft leader, if known.
    pub fn leader_id(&self) -> Option<&str> {
        self.raft.leader_id()
    }

    /// Current Raft state.
    pub fn state(&self) -> &RaftState {
        self.raft.state()
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    async fn dispatch(&self, msgs: Vec<OutboundMessage>) -> Result<()> {
        for out in msgs {
            if let Err(e) = self.transport.send(&out.to, out.msg).await {
                warn!(
                    node = %self.raft.id,
                    to = %out.to,
                    error = %e,
                    "Failed to send Raft message"
                );
                // Don't propagate — Raft handles message loss via retransmit
            }
        }
        Ok(())
    }

    /// Drain and apply all newly committed log entries.
    ///
    /// The engine applier runs before the waiting caller is released, so a
    /// successful return means this node has the metadata locally and a
    /// majority has the log entry. A snapshot installed since the last call
    /// is applied first: those commands are no longer in the log.
    async fn apply_ready(&mut self) {
        let installed = self.raft.take_installed_entries();
        for entry in installed {
            self.apply_entry(entry).await;
        }
        let entries = self.raft.ready();
        for entry in entries {
            self.apply_entry(entry).await;
        }
        self.raft.maybe_compact();
        self.refresh_view();
    }

    async fn apply_entry(&mut self, entry: crate::raft::LogEntry) {
        debug!(
            node = %self.raft.id,
            index = entry.index,
            term = entry.term,
            "Applying committed entry"
        );
        self.metadata.apply(entry.index, &entry.command);
        // The leader's term-start no-op has no engine effect and no waiter.
        if matches!(entry.command, ClusterCommand::Noop) {
            self.publish_view(entry.index);
            return;
        }
        let effect = if let Some(applier) = &self.applier {
            applier.apply_metadata(&entry.command).await
        } else {
            ApplyEffect::Applied
        };
        // Before the waiter: a caller whose proposal returned can read an
        // applied index that covers it, and wait for other members to reach
        // the same index instead of polling their HTTP state on a timer.
        self.publish_view(entry.index);
        // Request ids are per accepting node. A leader applying a
        // forwarded entry must not complete its own waiter that happens
        // to share the number.
        if entry.command.client_origin() == Some(self.raft.id.as_str()) {
            if let Some(request_id) = entry.command.client_request_id() {
                if let Some(waiter) = self.waiters.remove(&request_id) {
                    let _ = waiter.ack.send(Ok(effect));
                }
            }
        }
    }

    /// Publish the raft view with every entry handed out so far applied.
    fn refresh_view(&self) {
        self.publish_view(self.raft.last_applied());
    }

    /// Publish the raft view with the log applied through `applied`.
    ///
    /// Inside the apply loop `applied` is the entry just applied:
    /// `RaftNode::last_applied` already counts the entries still to apply in
    /// this batch, so it would report them early. The applied index never
    /// moves back.
    fn publish_view(&self, applied: u64) {
        let role_changed = {
            let mut view = self.view.write().unwrap_or_else(|e| e.into_inner());
            let state = self.raft.state().clone();
            let leader_id = self.raft.leader_id().map(|s| s.to_string());
            let term = self.raft.current_term();
            let role_changed =
                view.state != state || view.leader_id != leader_id || view.term != term;
            view.state = state;
            view.leader_id = leader_id;
            view.term = term;
            view.snapshot_index = self.raft.snapshot_index();
            view.log_len = self.raft.log_len();
            view.commit_index = self.raft.commit_index();
            view.applied_index = view.applied_index.max(applied);
            role_changed
        };
        if role_changed {
            self.roles_tx.send_modify(|changes| *changes += 1);
        }
        self.applied_tx.send_if_modified(|seen| {
            if applied > *seen {
                *seen = applied;
                true
            } else {
                false
            }
        });
    }

    async fn accept_proposal(&mut self, req: ProposalRequest) {
        if Instant::now() >= req.deadline {
            let _ = req.ack.send(Err(self.deadline_error()));
            return;
        }
        self.waiters.insert(
            req.request_id,
            Waiter {
                command: req.command,
                deadline: req.deadline,
                state: WaiterState::Pending,
                ack: req.ack,
            },
        );
        self.drive_waiter(req.request_id).await;
    }

    fn deadline_error(&self) -> ProposeError {
        if self.raft.is_leader() || self.raft.leader_id().is_some() {
            ProposeError::Timeout
        } else {
            ProposeError::NoLeader
        }
    }

    fn note_rejection(&mut self, request_id: u64, hint: Option<String>) {
        let Some(waiter) = self.waiters.get_mut(&request_id) else {
            return;
        };
        // A rejection from the node we forwarded to. Do not forward there again.
        if let WaiterState::Forwarded { to } = &waiter.state {
            if hint.as_deref() == Some(to.as_str()) {
                waiter.state = WaiterState::Rejected { hint: None };
                return;
            }
        }
        waiter.state = WaiterState::Rejected { hint };
    }

    fn expire_waiters(&mut self) {
        let now = Instant::now();
        let expired: Vec<u64> = self
            .waiters
            .iter()
            .filter(|(_, w)| now >= w.deadline)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            let Some(waiter) = self.waiters.remove(&id) else {
                continue;
            };
            let err = match waiter.state {
                WaiterState::Proposed { index, term }
                    if self.raft.entry_term(index) != Some(term) =>
                {
                    ProposeError::LeadershipLost
                }
                _ => self.deadline_error(),
            };
            let _ = waiter.ack.send(Err(err));
        }
    }

    async fn pump_waiters(&mut self) {
        let ids: Vec<u64> = self.waiters.keys().copied().collect();
        for id in ids {
            self.drive_waiter(id).await;
        }
    }

    async fn drive_waiter(&mut self, id: u64) {
        let snapshot = {
            let Some(waiter) = self.waiters.get(&id) else {
                return;
            };
            if Instant::now() >= waiter.deadline {
                return;
            }
            match &waiter.state {
                WaiterState::Proposed { index, term } => DrivePlan::Proposed {
                    index: *index,
                    term: *term,
                },
                WaiterState::Forwarded { .. } => DrivePlan::Wait,
                WaiterState::Rejected { hint } => DrivePlan::Forward(hint.clone()),
                WaiterState::Pending => {
                    DrivePlan::Forward(self.raft.leader_id().map(|s| s.to_string()))
                }
            }
        };
        if let DrivePlan::Proposed { index, term } = snapshot {
            // The entry was truncated (new leader overwrote it) before it
            // committed. Fail the caller; do not append a second copy here.
            if self.raft.entry_term(index) != Some(term) && self.raft.commit_index() < index {
                if let Some(waiter) = self.waiters.remove(&id) {
                    let _ = waiter.ack.send(Err(ProposeError::LeadershipLost));
                }
            }
            return;
        }
        if self.raft.is_leader() {
            self.propose_waiter(id).await;
            return;
        }
        let DrivePlan::Forward(target) = snapshot else {
            return;
        };
        let Some(leader) = target.or_else(|| self.raft.leader_id().map(|s| s.to_string())) else {
            return;
        };
        if leader == self.raft.id {
            return;
        }
        self.forward_waiter(id, leader).await;
    }

    async fn propose_waiter(&mut self, id: u64) {
        let Some(waiter) = self.waiters.get(&id) else {
            return;
        };
        if matches!(waiter.state, WaiterState::Proposed { .. }) {
            return;
        }
        let command = waiter.command.clone();
        match self.raft.propose(command) {
            Ok(index) => {
                let term = self.raft.current_term();
                if let Some(waiter) = self.waiters.get_mut(&id) {
                    waiter.state = WaiterState::Proposed { index, term };
                }
                let msgs = self.raft.force_heartbeat();
                let _ = self.dispatch(msgs).await;
                // Single-node clusters commit inside propose(); apply now so
                // the caller is not waiting for the next tick.
                self.apply_ready().await;
            }
            Err(e) => {
                warn!(node = %self.raft.id, error = %e, "propose failed; will forward");
                if let Some(waiter) = self.waiters.get_mut(&id) {
                    waiter.state = WaiterState::Pending;
                }
            }
        }
    }

    async fn forward_waiter(&mut self, id: u64, leader: String) {
        let Some(waiter) = self.waiters.get(&id) else {
            return;
        };
        if matches!(&waiter.state, WaiterState::Forwarded { to } if to == &leader) {
            return;
        }
        if matches!(waiter.state, WaiterState::Proposed { .. }) {
            return;
        }
        let command = waiter.command.clone();
        let term = self.raft.current_term();
        let from = self.raft.id.clone();
        match self
            .transport
            .send(
                &leader,
                RaftMessage::ForwardProposal {
                    term,
                    from,
                    command,
                },
            )
            .await
        {
            Ok(()) => {
                if let Some(waiter) = self.waiters.get_mut(&id) {
                    waiter.state = WaiterState::Forwarded { to: leader };
                }
            }
            Err(e) => {
                warn!(node = %self.raft.id, to = %leader, error = %e, "failed to forward metadata proposal");
            }
        }
    }
}

// ── In-memory transport for tests ────────────────────────────────────────────

/// In-memory transport that routes messages via `tokio::sync::mpsc` channels.
///
/// Used in unit tests to avoid network I/O while still exercising the full
/// Raft message-passing logic.
pub mod in_memory {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::{mpsc, Mutex};

    use crate::shard::{MemoryShardDir, MemoryShardRpc};

    type Sender = mpsc::UnboundedSender<(String, RaftMessage)>;
    type Receiver = mpsc::UnboundedReceiver<(String, RaftMessage)>;

    /// A registry of in-memory channels, one per node.
    #[derive(Clone)]
    pub struct InMemoryBus {
        senders: Arc<Mutex<HashMap<String, Sender>>>,
        shards: Arc<MemoryShardDir>,
    }

    impl InMemoryBus {
        pub fn new() -> Self {
            InMemoryBus {
                senders: Arc::new(Mutex::new(HashMap::new())),
                shards: MemoryShardDir::new(),
            }
        }

        /// Register a node and return its dedicated receiver channel.
        pub async fn register(&self, node_id: &str) -> Receiver {
            let (tx, rx) = mpsc::unbounded_channel();
            self.senders.lock().await.insert(node_id.to_string(), tx);
            rx
        }

        /// Send a message directly (used by [`InMemoryTransport`]).
        pub async fn send_to(&self, from: &str, to: &str, msg: RaftMessage) -> Result<()> {
            let senders = self.senders.lock().await;
            if let Some(tx) = senders.get(to) {
                tx.send((from.to_string(), msg))
                    .map_err(|e| anyhow::anyhow!("channel closed: {}", e))?;
            } else {
                // Node not registered — simulate network partition
                debug!(
                    from,
                    to, "InMemoryBus: target node not found (partitioned?)"
                );
            }
            Ok(())
        }
    }

    impl Default for InMemoryBus {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Transport implementation backed by [`InMemoryBus`].
    pub struct InMemoryTransport {
        pub node_id: String,
        bus: InMemoryBus,
        receiver: Arc<Mutex<Receiver>>,
        shard_rpc: Arc<MemoryShardRpc>,
    }

    impl InMemoryTransport {
        pub async fn new(node_id: String, bus: InMemoryBus) -> Self {
            let receiver = bus.register(&node_id).await;
            let shard_rpc = MemoryShardRpc::new(node_id.clone(), Arc::clone(&bus.shards));
            InMemoryTransport {
                node_id,
                bus,
                receiver: Arc::new(Mutex::new(receiver)),
                shard_rpc,
            }
        }
    }

    #[async_trait]
    impl ClusterTransport for InMemoryTransport {
        async fn send(&self, to: &str, msg: RaftMessage) -> Result<()> {
            self.bus.send_to(&self.node_id, to, msg).await
        }

        async fn recv(&self) -> Result<(String, RaftMessage)> {
            let mut rx = self.receiver.lock().await;
            rx.recv()
                .await
                .ok_or_else(|| anyhow::anyhow!("channel closed"))
        }

        fn shard_rpc(&self) -> Arc<dyn crate::shard::ShardRpc> {
            Arc::clone(&self.shard_rpc) as Arc<dyn crate::shard::ShardRpc>
        }
    }
}

#[cfg(test)]
mod tests {
    use super::in_memory::{InMemoryBus, InMemoryTransport};
    use super::*;
    #[allow(unused_imports)]
    use std::time::Duration;

    async fn make_cluster(ids: &[&str]) -> Vec<ClusterNode> {
        let bus = InMemoryBus::new();
        let id_list: Vec<String> = ids.iter().map(|s| s.to_string()).collect();

        let mut nodes = Vec::new();
        for id in ids {
            let peers: Vec<String> = id_list
                .iter()
                .filter(|p| p.as_str() != *id)
                .cloned()
                .collect();
            let transport = InMemoryTransport::new(id.to_string(), bus.clone()).await;
            let node = ClusterNode::new(id.to_string(), peers, Box::new(transport));
            nodes.push(node);
        }
        nodes
    }

    #[tokio::test]
    async fn test_cluster_node_propose_and_apply() {
        // Single-node cluster: easy path
        let mut nodes = make_cluster(&["n1"]).await;

        // Force election timeout
        nodes[0].raft.force_election_timeout();
        nodes[0].tick().await.unwrap();

        assert!(nodes[0].is_leader(), "single node should become leader");

        nodes[0]
            .propose(ClusterCommand::CreateIndex {
                name: "orders".to_string(),
                schema_json: "{}".to_string(),
            })
            .unwrap();

        // Apply ready entries
        nodes[0].tick().await.unwrap();

        assert!(
            nodes[0].metadata.indices.contains_key("orders"),
            "CreateIndex should be applied to metadata"
        );
    }

    #[tokio::test]
    async fn test_metadata_apply_full_pipeline() {
        let mut nodes = make_cluster(&["n1"]).await;

        nodes[0].raft.force_election_timeout();
        nodes[0].tick().await.unwrap();

        // Apply several commands
        nodes[0]
            .propose(ClusterCommand::AddNode {
                node_id: "n2".to_string(),
                address: "127.0.0.1:9201".to_string(),
            })
            .unwrap();
        nodes[0]
            .propose(ClusterCommand::UpdateConfig {
                key: "replicas".to_string(),
                value: "2".to_string(),
            })
            .unwrap();

        nodes[0].tick().await.unwrap();

        assert!(nodes[0].metadata.nodes.contains_key("n2"));
        assert_eq!(nodes[0].metadata.config["replicas"], "2");
    }

    /// #3469 follow-up: the handle's applied index covers a proposal by the
    /// time `propose` returns `Ok`, so a caller can wait for other members to
    /// reach that index instead of polling their state on a wall-clock
    /// deadline. A node whose run loop is gone ends the wait instead of
    /// hanging it.
    #[tokio::test]
    async fn applied_index_covers_a_returned_proposal_and_wait_applied_ends_on_shutdown() {
        let mut nodes = make_cluster(&["n1"]).await;
        let mut node = nodes.remove(0);
        let handle = node.handle();
        node.raft.force_election_timeout();
        node.tick().await.unwrap();
        assert!(node.is_leader(), "single node should become leader");

        let proposer = handle.clone();
        let waiter = tokio::spawn(async move {
            let effect = proposer
                .propose(ClusterCommand::CreateIndex {
                    name: "orders".to_string(),
                    schema_json: "{}".to_string(),
                })
                .await;
            (effect, proposer.applied_index(), proposer.commit_index())
        });
        while !waiter.is_finished() {
            node.step(Duration::from_millis(1)).await.unwrap();
        }
        let (effect, applied, commit) = waiter.await.unwrap();
        assert_eq!(effect.unwrap(), ApplyEffect::Applied);
        let create_at = node.raft.commit_index();
        assert!(create_at >= 2, "term no-op then the create: {create_at}");
        assert_eq!(
            applied, create_at,
            "applied index was published before the waiter completed"
        );
        assert!(commit >= applied, "commit {commit} < applied {applied}");
        assert_eq!(handle.wait_applied(create_at).await.unwrap(), create_at);

        let late = handle.clone();
        let blocked = tokio::spawn(async move { late.wait_applied(create_at + 1).await });
        node.step(Duration::from_millis(1)).await.unwrap();
        assert!(
            !blocked.is_finished(),
            "nothing past {create_at} is applied"
        );
        drop(node);
        assert!(matches!(
            blocked.await.unwrap(),
            Err(ProposeError::ShuttingDown)
        ));
    }

    /// #3496: an election publishes a role change a caller can wait on
    /// instead of polling `is_leader` on a wall-clock deadline, and a node
    /// whose run loop is gone ends that wait and the backfill wait.
    #[tokio::test]
    async fn an_election_wakes_wait_role_change_and_shutdown_ends_the_waits() {
        let mut nodes = make_cluster(&["n1"]).await;
        let mut node = nodes.remove(0);
        let handle = node.handle();
        assert_eq!(handle.role_changes(), 0);
        assert_eq!(handle.term(), 0);

        let waiter = handle.clone();
        let woke = tokio::spawn(async move { waiter.wait_role_change(0).await });
        node.raft.force_election_timeout();
        node.step(Duration::from_millis(1)).await.unwrap();
        assert!(handle.is_leader(), "single node should become leader");
        let changes = woke.await.unwrap().unwrap();
        assert!(changes >= 1, "the election published a change: {changes}");
        assert!(handle.term() >= 1, "term {}", handle.term());

        let seen = handle.role_changes();
        node.step(Duration::from_millis(1)).await.unwrap();
        assert_eq!(
            handle.role_changes(),
            seen,
            "a step with the same role, leader, and term publishes no change"
        );

        let roles = handle.clone();
        let blocked_role = tokio::spawn(async move { roles.wait_role_change(seen).await });
        let passes = handle.clone();
        let blocked_pass = tokio::spawn(async move { passes.wait_backfill_pass(0).await });
        tokio::task::yield_now().await;
        assert!(!blocked_role.is_finished());
        assert!(!blocked_pass.is_finished());
        drop(node);
        assert!(matches!(
            blocked_role.await.unwrap(),
            Err(ProposeError::ShuttingDown)
        ));
        assert!(matches!(
            blocked_pass.await.unwrap(),
            Err(ProposeError::ShuttingDown)
        ));
    }

    /// #3496: each finished backfill pass is counted on the handle, so a
    /// caller waiting for a replica copy wakes on the pass that made it.
    #[tokio::test]
    async fn the_runner_counts_finished_backfill_passes() {
        use crate::runner::ClusterRunner;
        use std::sync::atomic::{AtomicU64, Ordering};
        use tokio::sync::watch;

        struct CountingApplier(Arc<AtomicU64>);

        #[async_trait::async_trait]
        impl MetadataApplier for CountingApplier {
            async fn apply_metadata(&self, _command: &ClusterCommand) -> ApplyEffect {
                ApplyEffect::Applied
            }
            async fn backfill_local_replicas(&self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let mut nodes = make_cluster(&["n1"]).await;
        let mut node = nodes.remove(0);
        let calls = Arc::new(AtomicU64::new(0));
        node.set_applier(Arc::new(CountingApplier(Arc::clone(&calls))));
        let handle = node.handle();
        let (shutdown, shutdown_rx) = watch::channel(false);
        let mut runner = ClusterRunner::new(node, Duration::from_millis(5), shutdown_rx);
        let task = tokio::spawn(async move { runner.run().await });

        let first = handle.wait_backfill_pass(0).await.unwrap();
        assert!(first >= 1);
        let second = handle.wait_backfill_pass(first).await.unwrap();
        assert!(second > first);
        assert!(
            calls.load(Ordering::SeqCst) >= second,
            "a pass is counted after the applier's backfill returns"
        );

        let _ = shutdown.send(true);
        task.await.unwrap();
        let seen = handle.backfill_passes();
        assert!(matches!(
            handle.wait_backfill_pass(seen).await,
            Err(ProposeError::ShuttingDown)
        ));
    }

    /// End-to-end proof that [`crate::runner::ClusterRunner::run`] actually
    /// drains and handles inbound messages (not just sleeps and ticks).
    ///
    /// Three real [`ClusterNode`]s are wrapped in `ClusterRunner`s over a shared
    /// [`InMemoryBus`] and driven by their `run()` loops concurrently. A leader
    /// can only emerge if RequestVote / vote-response messages are received and
    /// handled during the run, so asserting exactly one leader within ~2s proves
    /// the run loop processes messages. With the old recv-less `sleep + tick`
    /// body, votes were sent but never received, so zero leaders emerged and
    /// this assertion fails.
    #[tokio::test]
    async fn test_cluster_runner_elects_single_leader() {
        use crate::runner::ClusterRunner;
        use tokio::sync::watch;

        let nodes = make_cluster(&["n1", "n2", "n3"]).await;

        // Keep the sender alive for the whole test: dropping it would resolve
        // `shutdown.changed()` with an error and stop the run loops early.
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);

        let tick = Duration::from_millis(5);
        let mut runners = nodes
            .into_iter()
            .map(|n| ClusterRunner::new(n, tick, shutdown_rx.clone()));
        let mut r1 = runners.next().unwrap();
        let mut r2 = runners.next().unwrap();
        let mut r3 = runners.next().unwrap();

        // run() loops forever, so the 2s timeout branch is what ends the
        // select!. Because select! borrows (does not move) the runners, once
        // the timeout fires and the run() futures are cancelled we regain
        // access to inspect leadership.
        tokio::select! {
            _ = r1.run() => {}
            _ = r2.run() => {}
            _ = r3.run() => {}
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }

        let leaders = [r1.is_leader(), r2.is_leader(), r3.is_leader()]
            .into_iter()
            .filter(|&l| l)
            .count();
        assert_eq!(
            leaders, 1,
            "exactly one leader must be elected while ClusterRunner::run() drives the cluster"
        );
    }
}
