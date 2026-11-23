//! Caller-facing handle for proposing a metadata command and waiting until
//! this node has applied the committed entry.
//!
//! The Raft state machine is owned by the cluster run loop. HTTP handlers
//! do not touch it. They send a [`ProposalRequest`] through [`ClusterHandle`]
//! and await the oneshot that the run loop completes from `apply_ready` —
//! after the entry is committed, never before.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, watch};

use crate::raft::{ClusterCommand, RaftState};
use crate::shard::{ShardError, ShardHandler, ShardOp, ShardResult, ShardRpc};

/// How a committed metadata command changed the local engine.
///
/// `Applied` is the success the caller acknowledges. The other variants are
/// committed no-ops or local failures: the log entry exists either way, and
/// the caller must not treat them as a successful mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyEffect {
    Applied,
    AlreadyExists,
    NotFound,
    Failed(String),
}

/// Why a metadata proposal was not acknowledged.
///
/// Every variant means the caller did not observe a commit of this request.
/// The write must not be reported as success.
#[derive(Debug, thiserror::Error)]
pub enum ProposeError {
    #[error("no elected raft leader; metadata writes are not accepted until a leader is elected")]
    NoLeader,
    #[error(
        "timed out waiting for the metadata change to commit on a majority; the write was not acknowledged"
    )]
    Timeout,
    #[error("cluster node is shutting down; the metadata write was not acknowledged")]
    ShuttingDown,
    #[error(
        "raft leadership changed before the metadata write committed; the write was not acknowledged"
    )]
    LeadershipLost,
}

/// What the run loop publishes for [`ClusterHandle::is_leader`] / [`ClusterHandle::leader_id`].
#[derive(Debug, Clone)]
pub struct ClusterView {
    pub node_id: String,
    pub state: RaftState,
    pub leader_id: Option<String>,
    /// Highest index in this node's snapshot. 0 when it has not compacted.
    pub snapshot_index: u64,
    /// Log entries that are not covered by the snapshot.
    pub log_len: usize,
    /// Highest log index this node knows is committed on a majority.
    pub commit_index: u64,
    /// Highest log index whose command this node has applied, engine applier
    /// included. Never ahead of `commit_index`.
    pub applied_index: u64,
    /// This node's current raft term.
    pub term: u64,
}

/// A metadata command waiting to be proposed or forwarded by the run loop.
pub struct ProposalRequest {
    pub request_id: u64,
    pub command: ClusterCommand,
    pub deadline: Instant,
    pub ack: oneshot::Sender<Result<ApplyEffect, ProposeError>>,
}

/// What the run loop publishes for a [`ClusterHandle`] to wait on.
pub(crate) struct RunLoopSignals {
    /// See [`ClusterHandleInner::applied`].
    pub(crate) applied: watch::Receiver<u64>,
    /// See [`ClusterHandleInner::roles`].
    pub(crate) roles: watch::Receiver<u64>,
    /// See [`ClusterHandleInner::backfill`].
    pub(crate) backfill: watch::Receiver<u64>,
}

/// Cloneable handle shared by the HTTP/engine layer.
///
/// Proposing does not apply the command locally. The run loop proposes (or
/// forwards to the leader) and completes the future when this node applies
/// the committed entry, or with an error if that does not happen in time.
#[derive(Clone)]
pub struct ClusterHandle {
    inner: Arc<ClusterHandleInner>,
}

struct ClusterHandleInner {
    node_id: String,
    /// This node plus its peers, sorted, deduped. The index-create proposal
    /// builds the shard plan from this list.
    members: Vec<String>,
    tx: mpsc::UnboundedSender<ProposalRequest>,
    next: AtomicU64,
    timeout_ms: AtomicU64,
    view: Arc<RwLock<ClusterView>>,
    /// The run loop's applied index, published after each entry is applied
    /// and before that entry's waiter is completed.
    applied: watch::Receiver<u64>,
    /// Bumped by the run loop each time it publishes a different role,
    /// leader, or term.
    roles: watch::Receiver<u64>,
    /// Replica-backfill passes the runner has finished on this node.
    backfill: watch::Receiver<u64>,
    shard: Arc<dyn ShardRpc>,
}

impl ClusterHandle {
    pub(crate) fn new(
        node_id: String,
        members: Vec<String>,
        tx: mpsc::UnboundedSender<ProposalRequest>,
        view: Arc<RwLock<ClusterView>>,
        signals: RunLoopSignals,
        shard: Arc<dyn ShardRpc>,
    ) -> Self {
        let RunLoopSignals {
            applied,
            roles,
            backfill,
        } = signals;
        ClusterHandle {
            inner: Arc::new(ClusterHandleInner {
                node_id,
                members,
                tx,
                next: AtomicU64::new(1),
                timeout_ms: AtomicU64::new(5_000),
                view,
                applied,
                roles,
                backfill,
                shard,
            }),
        }
    }

    pub fn node_id(&self) -> &str {
        &self.inner.node_id
    }

    /// Members the shard plan is built from: this node and the peers it was
    /// started with, sorted.
    pub fn members(&self) -> Vec<String> {
        self.inner.members.clone()
    }

    /// Install the handler that runs forwarded shard operations on this node.
    ///
    /// The server does this once the engine exists, before it accepts HTTP.
    /// A call that arrives with no handler fails; it is not applied locally
    /// on the sender.
    pub fn set_shard_handler(&self, handler: Arc<dyn ShardHandler>) {
        self.inner.shard.set_handler(handler);
    }

    /// Stop serving shard calls on this node. In-process tests use this as
    /// the stand-in for a member that has stopped.
    pub fn clear_shard_handler(&self) {
        self.inner.shard.clear_handler();
    }

    /// Forward `op` to `to` and wait for that member's result.
    pub async fn shard_call(&self, to: &str, op: ShardOp) -> Result<ShardResult, ShardError> {
        self.inner.shard.call(to, op).await
    }

    /// How long [`ClusterHandle::propose`] waits for a commit.
    ///
    /// The run loop uses the same budget: past the deadline it fails the
    /// waiter and does not append the command.
    pub fn set_timeout(&self, timeout: Duration) {
        self.inner
            .timeout_ms
            .store(timeout.as_millis() as u64, Ordering::Relaxed);
    }

    pub fn is_leader(&self) -> bool {
        self.view().state == RaftState::Leader
    }

    pub fn leader_id(&self) -> Option<String> {
        self.view().leader_id
    }

    /// Highest snapshotted index, or 0. Updated by the run loop, so a caller
    /// can see it only after the next step.
    pub fn snapshot_index(&self) -> u64 {
        self.view().snapshot_index
    }

    /// Entries still in the log, not counting a compacted prefix.
    pub fn log_len(&self) -> usize {
        self.view().log_len
    }

    /// Highest log index this node knows is committed. Updated by the run
    /// loop as it applies entries and at the end of each step.
    pub fn commit_index(&self) -> u64 {
        self.view().commit_index
    }

    /// Highest log index this node has applied, engine applier included.
    ///
    /// Published before the waiter of that entry is completed, so after a
    /// [`ClusterHandle::propose`] on this node returns `Ok`, this is at least
    /// the index of the proposed entry.
    pub fn applied_index(&self) -> u64 {
        *self.inner.applied.borrow()
    }

    /// Wait until this node has applied the log through `index`.
    ///
    /// Returns the applied index seen, which may be past `index`. Ends with
    /// [`ProposeError::ShuttingDown`] if the run loop is gone, so a caller
    /// never waits on a node that can no longer apply anything. There is no
    /// timer: the wait ends on raft progress, not on elapsed time. Callers
    /// that need a bound wrap it themselves.
    pub async fn wait_applied(&self, index: u64) -> Result<u64, ProposeError> {
        let mut rx = self.inner.applied.clone();
        let seen = rx
            .wait_for(|applied| *applied >= index)
            .await
            .map_err(|_| ProposeError::ShuttingDown)?;
        Ok(*seen)
    }

    /// This node's current raft term, as of the run loop's last publish.
    pub fn term(&self) -> u64 {
        self.view().term
    }

    /// How many times the run loop has published a different role, leader,
    /// or term on this node. Pass it to [`ClusterHandle::wait_role_change`].
    pub fn role_changes(&self) -> u64 {
        *self.inner.roles.borrow()
    }

    /// Wait until the run loop publishes a role, leader, or term change
    /// after `seen` (a value of [`ClusterHandle::role_changes`]).
    ///
    /// Elections run on wall-clock timers, so this is the event a caller
    /// waiting for a leader can wake on. Ends with
    /// [`ProposeError::ShuttingDown`] if the run loop is gone. No timer.
    pub async fn wait_role_change(&self, seen: u64) -> Result<u64, ProposeError> {
        let mut rx = self.inner.roles.clone();
        let now = rx
            .wait_for(|changes| *changes > seen)
            .await
            .map_err(|_| ProposeError::ShuttingDown)?;
        Ok(*now)
    }

    /// Replica-backfill passes finished on this node. Pass it to
    /// [`ClusterHandle::wait_backfill_pass`].
    pub fn backfill_passes(&self) -> u64 {
        *self.inner.backfill.borrow()
    }

    /// Wait until the runner finishes a replica-backfill pass after `seen`
    /// (a value of [`ClusterHandle::backfill_passes`]).
    ///
    /// Backfill and refill run off the raft step, so they have no log index
    /// to wait on. A finished pass is the point where a replica copy that
    /// pass made is in the store and the primary has applied the in-sync
    /// change it proposed. Ends with [`ProposeError::ShuttingDown`] if the
    /// run loop is gone. No timer.
    pub async fn wait_backfill_pass(&self, seen: u64) -> Result<u64, ProposeError> {
        let mut passes = self.inner.backfill.clone();
        let mut applied = self.inner.applied.clone();
        tokio::select! {
            now = passes.wait_for(|passes| *passes > seen) => {
                now.map(|now| *now).map_err(|_| ProposeError::ShuttingDown)
            }
            // Never true: ends only when the run loop drops its sender.
            _ = applied.wait_for(|_| false) => Err(ProposeError::ShuttingDown),
        }
    }

    fn view(&self) -> ClusterView {
        self.inner
            .view
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Propose `command` and wait until this node applies the committed entry.
    ///
    /// The command is wrapped in [`ClusterCommand::ClientMetadata`] so the
    /// run loop can match the applied entry to this caller.
    pub async fn propose(&self, command: ClusterCommand) -> Result<ApplyEffect, ProposeError> {
        let request_id = self.inner.next.fetch_add(1, Ordering::Relaxed);
        let wrapped = ClusterCommand::ClientMetadata {
            origin: self.inner.node_id.clone(),
            request_id,
            inner: Box::new(command),
        };
        let timeout = Duration::from_millis(self.inner.timeout_ms.load(Ordering::Relaxed));
        let (ack, rx) = oneshot::channel();
        self.inner
            .tx
            .send(ProposalRequest {
                request_id,
                command: wrapped,
                deadline: Instant::now() + timeout,
                ack,
            })
            .map_err(|_| ProposeError::ShuttingDown)?;
        // A little longer than the run loop's deadline so a `NoLeader` /
        // `Timeout` it sends wins over this timer.
        match tokio::time::timeout(timeout + Duration::from_millis(250), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(ProposeError::ShuttingDown),
            Err(_) => Err(ProposeError::Timeout),
        }
    }
}

/// Applies a committed metadata command to the local engine.
///
/// Called on the cluster run loop for every committed entry, on every member.
/// Implementations of [`MetadataApplier::apply_metadata`] must be
/// deterministic: the same command against the same prior state does the
/// same thing on every node, and that method must not propose a new command.
#[async_trait::async_trait]
pub trait MetadataApplier: Send + Sync {
    async fn apply_metadata(&self, command: &ClusterCommand) -> ApplyEffect;

    /// Copy documents this node is missing as a replica.
    ///
    /// The runner calls this off the Raft step. The default does nothing.
    /// The engine implementation asks each primary to copy that shard's live
    /// documents and to publish this node into the in-sync set. That call
    /// may propose. It is not part of applying a log entry.
    async fn backfill_local_replicas(&self) {}
}
