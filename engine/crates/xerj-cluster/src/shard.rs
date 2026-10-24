//! Data-plane calls between shard owners.
//!
//! Metadata (index create, the shard plan) goes through the Raft log.
//! A document does not. The node that accepts the HTTP call asks
//! [`crate::router::ShardRouter`] which member owns the document and, when
//! that member is someone else, invokes [`ShardRpc::call`]. The owner's
//! [`ShardHandler`] applies the operation to local storage and returns the
//! JSON the caller's HTTP response forwards.
//!
//! In-process tests share a [`MemoryShardDir`]. The TCP transport opens a
//! fresh authenticated connection, writes one [`crate::raft::RaftMessage::ShardRequest`],
//! and reads one [`crate::raft::RaftMessage::ShardResponse`] before closing.
//! That exchange is not the Raft outbound queue: a dropped heartbeat is
//! retransmitted, a dropped document write must not be.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// One primary-shard assignment carried in a committed index-create entry.
///
/// `replicas` are other members that hold a copy of this shard. Empty when
/// the index was created with `number_of_replicas` 0 (the default) or when
/// the entry predates replica placement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardAssignment {
    pub shard: u32,
    pub node_id: String,
    #[serde(default)]
    pub replicas: Vec<String>,
}

/// One primary per member, shard `i` on the `i`th member id in sorted order.
///
/// No replicas. Equivalent to [`shard_plan`] with `number_of_replicas` 0.
/// The plan is computed by the node that proposes the create and stored in
/// the log entry. Every member applies that list; nobody recomputes it.
pub fn primary_shard_plan(members: &[String]) -> Vec<ShardAssignment> {
    shard_plan(members, 0)
}

/// Primary plan plus up to `number_of_replicas` copies of each shard.
///
/// Replicas are the next members in the sorted ring, wrapping, never the
/// primary. The count is capped at `members - 1`: a node does not hold a
/// replica of a shard it already owns. The proposer stores this list in the
/// create entry. Every member applies it; nobody recomputes it.
pub fn shard_plan(members: &[String], number_of_replicas: u32) -> Vec<ShardAssignment> {
    let mut members: Vec<&str> = members
        .iter()
        .map(String::as_str)
        .filter(|id| !id.is_empty())
        .collect();
    members.sort_unstable();
    members.dedup();
    if members.is_empty() {
        return Vec::new();
    }
    let cap = (members.len() - 1) as u32;
    let replicas = number_of_replicas.min(cap);
    (0..members.len() as u32)
        .map(|shard| {
            let mut copies = Vec::with_capacity(replicas as usize);
            for offset in 1..=replicas {
                let idx = (shard as usize + offset as usize) % members.len();
                copies.push(members[idx].to_string());
            }
            ShardAssignment {
                shard,
                node_id: members[shard as usize].to_string(),
                replicas: copies,
            }
        })
        .collect()
}

/// Hidden index that stores one shard's replica documents on the replica node.
///
/// Replica bytes are not written into the user index. That index is what a
/// primary search reads, and a node is primary for some shards and replica
/// for others — mixing the copies would make a fan-out count every document
/// twice. The name is a `.xerj_*` system index so the default catalogue hides
/// it. `None` when the name would not pass [`xerj_common`] index rules; the
/// caller refuses the create instead of truncating.
pub fn replica_index_name(index: &str, shard: u32) -> Option<String> {
    let name = format!(".xerj_replica_{index}_{shard}");
    if name.len() > 255 || name.contains("..") {
        return None;
    }
    Some(name)
}

/// `index.number_of_replicas` from a create-settings object.
///
/// Accepts the nested form (`{"index": {"number_of_replicas": 1}}`), the
/// flat form, and the dotted key. Strings and numbers both count. Absent or
/// unreadable means 0, which places no copies.
pub fn number_of_replicas_from_settings(settings: &serde_json::Value) -> u32 {
    fn parse(value: &serde_json::Value) -> Option<u32> {
        match value {
            serde_json::Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
            serde_json::Value::String(s) => s.parse().ok(),
            _ => None,
        }
    }
    settings
        .pointer("/index/number_of_replicas")
        .or_else(|| settings.get("number_of_replicas"))
        .or_else(|| settings.get("index.number_of_replicas"))
        .and_then(parse)
        .unwrap_or(0)
}

/// Replica acknowledgements a document write needs before the caller is told it landed.
///
/// `placed` is how many replicas the create entry named, not how many are
/// still in sync. Copies are that count plus the primary. A quorum is
/// `copies / 2 + 1` (a majority of the group). The primary has already
/// applied the write, so it counts as one and the return value is the
/// number of replica confirmations still required.
///
/// | placed replicas | copies | quorum | replica acks |
/// | --- | --- | --- | --- |
/// | 0 | 1 | 1 | 0 |
/// | 1 | 2 | 2 | 1 |
/// | 2 | 3 | 2 | 1 |
/// | 3 | 4 | 3 | 2 |
///
/// This is the majority of the replication group. [`crate::replication::ReplicationMode::Quorum`]
/// is a different helper (a majority of replicas only, not counting the
/// primary) and is not the document path.
pub fn replica_quorum_acks(placed: usize) -> usize {
    let copies = placed.saturating_add(1);
    let quorum = copies / 2 + 1;
    quorum.saturating_sub(1)
}

/// Assignments embedded in a `CreateIndex` schema envelope, if any.
///
/// A schema that is not the replicated envelope — including the `"{}"`
/// fixtures the Raft tests propose — yields an empty plan.
pub fn shard_plan_from_schema_json(schema_json: &str) -> Vec<ShardAssignment> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(schema_json) else {
        return Vec::new();
    };
    let Some(items) = value.get("shard_plan").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let shard = item.get("shard")?.as_u64()?;
            let node_id = item.get("node_id")?.as_str()?.to_string();
            if node_id.is_empty() || shard > u32::MAX as u64 {
                return None;
            }
            let replicas = item
                .get("replicas")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str())
                        .filter(|id| !id.is_empty() && *id != node_id)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            Some(ShardAssignment {
                shard: shard as u32,
                node_id,
                replicas,
            })
        })
        .collect()
}

/// A document or search operation forwarded to the shard owner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ShardOp {
    /// Index or create one document. `source_json` is the body after the
    /// accepting node has run pipelines and field checks.
    IndexDoc {
        index: String,
        id: String,
        source_json: String,
        /// `"index"` or `"create"`.
        op: String,
        /// Internal (1-based) sequence, already translated from the ES wire.
        if_seq_no: Option<u64>,
        if_primary_term: Option<u64>,
        version: Option<u64>,
        version_type: Option<String>,
        /// Apply on the replica store for [`ShardOp::IndexDoc::shard`] and
        /// do not replicate further. The primary sets this when it copies
        /// an acknowledged write.
        #[serde(default)]
        replica: bool,
        /// Shard id. Meaningful when `replica` is set; the primary's own
        /// apply ignores it and routes by document id.
        #[serde(default)]
        shard: u32,
    },
    GetDoc {
        index: String,
        id: String,
        source: Option<String>,
        source_includes: Option<String>,
        source_excludes: Option<String>,
        /// Read the replica store instead of the primary index.
        #[serde(default)]
        replica: bool,
        #[serde(default)]
        shard: u32,
    },
    DeleteDoc {
        index: String,
        id: String,
        if_seq_no: Option<u64>,
        if_primary_term: Option<u64>,
        /// Apply on the replica store and do not replicate further.
        #[serde(default)]
        replica: bool,
        #[serde(default)]
        shard: u32,
    },
    /// Partial or scripted update. The primary applies it, then copies the
    /// stored document onto the in-sync replicas the same way an index does.
    ///
    /// `index` is the name the caller used. An alias is resolved on the
    /// primary before the write is placed.
    UpdateDoc {
        index: String,
        id: String,
        /// JSON update body (`doc`, `upsert`, `doc_as_upsert`, `script`).
        body_json: String,
        /// JSON update query params (`refresh`, `_source`, includes, excludes).
        params_json: String,
    },
    /// A bulk body whose actions belong to shards this node owns.
    ///
    /// The coordinating node assigns missing ids and groups actions by
    /// primary. This node runs the local bulk pipeline, then copies each
    /// successful index, create, update, and delete onto the in-sync replicas.
    Bulk {
        /// NDJSON. Action lines name `_index` and `_id`.
        body: String,
        default_index: Option<String>,
        require_alias: Option<bool>,
        default_pipeline: Option<String>,
        /// JSON value of the URL-level `_source` filter, when one was set.
        default_source_req_json: Option<String>,
    },
    /// Run `_update_by_query` on this primary's shard only.
    ///
    /// The coordinating node sends one of these to every primary. This node
    /// updates documents in its user index and copies each updated document
    /// onto the in-sync replicas. `check_only` confirms the index is open and
    /// the pipeline can run, and writes nothing.
    UpdateByQuery {
        index: String,
        /// JSON query object. Already combined with any alias filter.
        query_json: String,
        /// Painless source. Absent when the request has no script.
        #[serde(default)]
        script_source: Option<String>,
        /// JSON script params. Absent when there is no script.
        #[serde(default)]
        script_params_json: Option<String>,
        /// Ingest pipeline name. The primary runs it; pipelines are not
        /// replicated with the index.
        #[serde(default)]
        pipeline: Option<String>,
        #[serde(default)]
        max_docs: Option<u64>,
        scroll_size: u64,
        #[serde(default)]
        check_only: bool,
    },
    /// Run `_delete_by_query` on this primary's shard only.
    ///
    /// The coordinating node sends one of these to every primary. This node
    /// deletes documents in its user index and copies each delete onto the
    /// in-sync replicas. `check_only` confirms the index is open and writes
    /// nothing.
    DeleteByQuery {
        index: String,
        /// JSON query object. Already combined with any alias filter.
        query_json: String,
        #[serde(default)]
        max_docs: Option<u64>,
        scroll_size: u64,
        #[serde(default)]
        check_only: bool,
    },
    /// Search this member's local shard only. The caller merges hits.
    Search {
        index: String,
        /// JSON object, the search body, with `from`/`size` already widened
        /// to the window the merger needs.
        body_json: String,
        /// JSON [`crate`] search query params. `from` and `size` match the body.
        params_json: String,
    },
    /// Copy this primary's live documents onto `replica_id` and, once that
    /// copy matches, publish `replica_id` in the in-sync set.
    ///
    /// The caller is a replica that is placed for `shard` and is not in the
    /// in-sync set. The primary holds the shard's replica lock across the
    /// copy and the publish, so a document write that lands during the copy
    /// is replicated to the new set when the lock is released. Documents are
    /// not part of the Raft snapshot; this is the copy that fills them in.
    BackfillReplica {
        index: String,
        shard: u32,
        replica_id: String,
    },
    /// Delete replica-store documents whose ids are not in `ids`.
    ///
    /// Sent by the primary at the end of [`ShardOp::BackfillReplica`], after
    /// the live documents have been upserted. Does not replicate further.
    RetainReplicaDocs {
        index: String,
        shard: u32,
        ids: Vec<String>,
    },
}

/// What the owner sends back. `status` is an HTTP status; `body` is JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShardResult {
    pub status: u16,
    pub body: String,
}

impl ShardResult {
    pub fn http(status: u16, body: impl Into<String>) -> Self {
        ShardResult {
            status,
            body: body.into(),
        }
    }
}

/// Why a shard call did not produce a [`ShardResult`].
///
/// The document was not acknowledged. Callers must not write it locally
/// instead.
#[derive(Debug, thiserror::Error)]
pub enum ShardError {
    #[error("shard call to {to} failed: {reason}")]
    Unavailable { to: String, reason: String },
    #[error("shard call to {to} timed out")]
    Timeout { to: String },
}

/// Executes a [`ShardOp`] against this node's engine.
#[async_trait::async_trait]
pub trait ShardHandler: Send + Sync {
    async fn handle(&self, op: ShardOp) -> ShardResult;
}

/// Sends a [`ShardOp`] to the named member and waits for its result.
#[async_trait::async_trait]
pub trait ShardRpc: Send + Sync {
    async fn call(&self, to: &str, op: ShardOp) -> Result<ShardResult, ShardError>;
    fn set_handler(&self, handler: Arc<dyn ShardHandler>);

    /// Drop this node's handler so later calls fail instead of applying.
    ///
    /// The in-process ring uses this to stand in for a dead member. A TCP
    /// node that has stopped accepting is already [`ShardError::Unavailable`].
    fn clear_handler(&self) {}
}

/// Transport that has no data plane. Calls fail; nothing is written locally.
pub struct NullShardRpc;

#[async_trait::async_trait]
impl ShardRpc for NullShardRpc {
    async fn call(&self, to: &str, _op: ShardOp) -> Result<ShardResult, ShardError> {
        Err(ShardError::Unavailable {
            to: to.to_string(),
            reason: "this transport has no shard path".to_string(),
        })
    }

    fn set_handler(&self, _handler: Arc<dyn ShardHandler>) {}

    fn clear_handler(&self) {}
}

/// Handler map shared by every in-process member of one ring.
#[derive(Default)]
pub struct MemoryShardDir {
    handlers: Mutex<HashMap<String, Arc<dyn ShardHandler>>>,
}

impl MemoryShardDir {
    pub fn new() -> Arc<Self> {
        Arc::new(MemoryShardDir {
            handlers: Mutex::new(HashMap::new()),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<dyn ShardHandler>>> {
        self.handlers.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Stack reserved for one in-process shard call. Smaller than this overflows
/// `search_impl`'s future; `std::thread` at this size is the sigaltstack
/// abort #1122 hit.
const SHARD_CALL_STACK_BYTES: usize = 8 * 1024 * 1024;

/// Map a spawn that did not happen into the error the HTTP caller sees.
///
/// The job is dropped. Running it on the caller's stack is the overflow
/// this thread exists to avoid, and dropping it does not write the document.
pub(crate) fn map_shard_spawn_failure<F>(
    to: &str,
    failure: xerj_common::thread_spawn::SpawnFailure<F>,
) -> ShardError {
    let error = failure.error;
    drop(failure.job);
    ShardError::Unavailable {
        to: to.to_string(),
        reason: format!(
            "could not start the shard call: {error}; the node stayed up and this call was not run"
        ),
    }
}

/// In-process [`ShardRpc`]. A call runs the target's handler on a fresh
/// stack and waits for it.
///
/// The handler is not polled on the caller's task. A search coordinator is
/// already inside `search_impl`, whose future is large enough that building
/// a second copy on that same stack overflows. A TCP shard call arrives on
/// its own task; this thread is the in-process equivalent. The caller's
/// index-visibility guard is intentionally not carried across: a TCP handler
/// has none, and an absent guard is how cluster-internal replica stores stay
/// reachable.
///
/// The thread is 8 MiB and is started with [`xerj_common::thread_spawn`], not
/// `std::thread::Builder`. Builder returns `Ok` and then panics in the child
/// when the alternate signal stack cannot be mapped; with `panic = "abort"`
/// that kills the node (#1122). A failed spawn answers the caller and does
/// not run the handler on this stack.
pub struct MemoryShardRpc {
    node_id: String,
    dir: Arc<MemoryShardDir>,
}

impl MemoryShardRpc {
    pub fn new(node_id: impl Into<String>, dir: Arc<MemoryShardDir>) -> Arc<Self> {
        Arc::new(MemoryShardRpc {
            node_id: node_id.into(),
            dir,
        })
    }
}

#[async_trait::async_trait]
impl ShardRpc for MemoryShardRpc {
    async fn call(&self, to: &str, op: ShardOp) -> Result<ShardResult, ShardError> {
        let handler = self.dir.lock().get(to).cloned();
        let Some(handler) = handler else {
            return Err(ShardError::Unavailable {
                to: to.to_string(),
                reason: "no shard handler registered on that member".to_string(),
            });
        };
        let runtime = tokio::runtime::Handle::current();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let target = to.to_string();
        let handle = match xerj_common::thread_spawn::try_spawn_stack(
            "xerj-shard",
            SHARD_CALL_STACK_BYTES,
            move || {
                let result = runtime.block_on(handler.handle(op));
                let _ = tx.send(result);
            },
        ) {
            Ok(handle) => handle,
            Err(failure) => return Err(map_shard_spawn_failure(&target, failure)),
        };
        let received = rx.await.map_err(|_| ShardError::Unavailable {
            to: target,
            reason: "shard call stopped before answering".to_string(),
        });
        // Reap the helper. It has already sent (or dropped the sender);
        // join does not run the handler again.
        drop(handle);
        received
    }

    fn set_handler(&self, handler: Arc<dyn ShardHandler>) {
        self.dir.lock().insert(self.node_id.clone(), handler);
    }

    fn clear_handler(&self) {
        self.dir.lock().remove(&self.node_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_is_one_primary_per_sorted_member() {
        let plan = primary_shard_plan(&["n3".into(), "n1".into(), "n2".into(), "n1".into()]);
        assert_eq!(
            plan,
            vec![
                ShardAssignment {
                    shard: 0,
                    node_id: "n1".into(),
                    replicas: Vec::new(),
                },
                ShardAssignment {
                    shard: 1,
                    node_id: "n2".into(),
                    replicas: Vec::new(),
                },
                ShardAssignment {
                    shard: 2,
                    node_id: "n3".into(),
                    replicas: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn replicas_walk_the_sorted_ring_and_cap_at_other_members() {
        let plan = shard_plan(&["n3".into(), "n1".into(), "n2".into()], 1);
        assert_eq!(plan[0].node_id, "n1");
        assert_eq!(plan[0].replicas, vec!["n2".to_string()]);
        assert_eq!(plan[1].replicas, vec!["n3".to_string()]);
        assert_eq!(plan[2].replicas, vec!["n1".to_string()]);

        let full = shard_plan(&["n1".into(), "n2".into(), "n3".into()], 9);
        assert_eq!(full[0].replicas, vec!["n2".to_string(), "n3".to_string()]);
        assert!(!full[0].replicas.iter().any(|id| id == "n1"));
        assert!(shard_plan(&["only".into()], 3)[0].replicas.is_empty());
    }

    #[test]
    fn settings_read_number_of_replicas_or_zero() {
        let nested = serde_json::json!({"index": {"number_of_replicas": "2"}});
        assert_eq!(number_of_replicas_from_settings(&nested), 2);
        let flat = serde_json::json!({"number_of_replicas": 1});
        assert_eq!(number_of_replicas_from_settings(&flat), 1);
        assert_eq!(number_of_replicas_from_settings(&serde_json::json!({})), 0);
        assert_eq!(replica_quorum_acks(0), 0);
        assert_eq!(replica_quorum_acks(1), 1);
        assert_eq!(replica_quorum_acks(2), 1);
        assert_eq!(replica_quorum_acks(3), 2);
        assert_eq!(replica_quorum_acks(4), 2);
        assert_eq!(
            replica_index_name("orders", 1).as_deref(),
            Some(".xerj_replica_orders_1")
        );
        assert!(replica_index_name(&"o".repeat(250), 1).is_none());
    }

    #[test]
    fn schema_without_a_plan_assigns_nothing() {
        assert!(shard_plan_from_schema_json("{}").is_empty());
        assert!(shard_plan_from_schema_json("not-json").is_empty());
        let parsed = shard_plan_from_schema_json(
            r#"{"v":1,"shard_plan":[{"shard":1,"node_id":"n2"},{"shard":0,"node_id":""}]}"#,
        );
        assert_eq!(
            parsed,
            vec![ShardAssignment {
                shard: 1,
                node_id: "n2".into(),
                replicas: Vec::new(),
            }]
        );
    }

    /// #1122: `std::thread::Builder::spawn` panics in the child when the
    /// alternate signal stack cannot be mapped (os error 12), after the
    /// parent has been told the thread exists. The shard call must surface
    /// that as [`ShardError`] and must not run the handler.
    #[test]
    fn a_shard_call_that_cannot_spawn_is_an_error_and_does_not_run() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use xerj_common::thread_spawn::{try_spawn_with, Spawn, SpawnedThread};

        struct Refuse;

        impl Spawn for Refuse {
            fn spawn(
                &self,
                _name: &str,
                job: xerj_common::thread_spawn::BoxedJob,
            ) -> std::io::Result<SpawnedThread> {
                drop(job);
                Err(std::io::Error::from_raw_os_error(12))
            }
        }

        let ran = Arc::new(AtomicBool::new(false));
        let job_ran = Arc::clone(&ran);
        let failure = try_spawn_with(&Refuse, true, "xerj-shard", move || {
            job_ran.store(true, Ordering::SeqCst);
        })
        .expect_err("spawn refused");
        let err = super::map_shard_spawn_failure("n2", failure);
        assert!(
            !ran.load(Ordering::SeqCst),
            "handler ran after a failed spawn"
        );
        let text = err.to_string();
        assert!(text.contains("n2"), "{text}");
        assert!(text.contains("could not start the shard call"), "{text}");
        assert!(text.contains("stayed up"), "{text}");
        assert!(
            text.contains("12") || text.contains("Cannot allocate memory"),
            "{text}"
        );
    }

    struct EchoHandler;

    #[async_trait::async_trait]
    impl ShardHandler for EchoHandler {
        async fn handle(&self, op: ShardOp) -> ShardResult {
            ShardResult::http(200, format!("{op:?}"))
        }
    }

    #[tokio::test]
    async fn an_in_process_shard_call_runs_the_handler_on_the_helper_stack() {
        let dir = MemoryShardDir::new();
        let rpc = MemoryShardRpc::new("n1", Arc::clone(&dir));
        rpc.set_handler(Arc::new(EchoHandler));
        let result = rpc
            .call(
                "n1",
                ShardOp::GetDoc {
                    index: "orders".into(),
                    id: "1".into(),
                    source: None,
                    source_includes: None,
                    source_excludes: None,
                    replica: false,
                    shard: 0,
                },
            )
            .await
            .expect("spawn and call");
        assert_eq!(result.status, 200);
        assert!(result.body.contains("orders"), "{}", result.body);
    }
}
