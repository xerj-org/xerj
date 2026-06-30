//! Distributed search coordinator.
//!
//! [`SearchCoordinator`] fans out a search query to every node that holds
//! shards for the requested index, collects partial responses, then merges
//! them into a single ranked result set using [`merge_search_responses`].
//!
//! # Fan-out model
//!
//! ```text
//! Client ──► SearchCoordinator
//!                 │
//!       ┌─────────┼──────────┐
//!       ▼         ▼          ▼
//!    node-0    node-1     node-2   (parallel SearchRequest)
//!       │         │          │
//!       └─────────┼──────────┘
//!                 ▼
//!         merge_search_responses
//!                 │
//!                 ▼
//!           MergedSearchResult
//! ```
//!
//! Local node searches skip the network path entirely — the coordinator calls
//! back into the local engine through the [`LocalSearcher`] trait.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use futures::future::join_all;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::router::ShardRouter;
use crate::search::{merge_search_responses, SearchHit, SearchMessage};

// ── Result types ──────────────────────────────────────────────────────────────

/// The merged output of a distributed search.
#[derive(Debug)]
pub struct MergedSearchResult {
    /// Final ranked hits (after merging all shards and applying `from`/`size`).
    pub hits: Vec<SearchHit>,
    /// Sum of matching documents across all shards.
    pub total: u64,
    /// Wall-clock time of the slowest shard response, in milliseconds.
    pub took_ms: u64,
}

/// Outcome of a routed index request.
#[derive(Debug)]
pub struct IndexResponse {
    pub doc_id: String,
    pub version: u64,
    pub result: String,
}

// ── LocalSearcher trait ───────────────────────────────────────────────────────

/// Abstraction over the local search engine.
///
/// Implementations will typically delegate to `xerj-engine`'s `Index::search`.
/// In unit tests a mock is used instead.
pub trait LocalSearcher: Send + Sync {
    /// Execute a query on the local node and return a [`SearchMessage::SearchResponse`].
    fn search_local(
        &self,
        request_id: &str,
        index: &str,
        query_json: &str,
        size: usize,
        from: usize,
    ) -> SearchMessage;
}

// ── SearchTransport — thin wrapper that serialises SearchMessage over the wire ─

/// Extension trait that adds `send_search` / `recv_search` to [`ClusterTransport`].
///
/// We serialise [`SearchMessage`] as JSON inside the `data` field of a
/// [`RaftMessage::AppendEntries`]-shaped envelope.  In production this would be
/// a dedicated RPC channel; for now it is enough to make the coordinator
/// testable without a real network stack.
///
/// The coordinator uses [`MockSearchTransport`] in tests and a real TCP impl in
/// production (future work).
#[async_trait::async_trait]
pub trait SearchTransport: Send + Sync {
    async fn send_search(&self, to: &str, msg: SearchMessage) -> Result<SearchMessage>;
}

// ── SearchCoordinator ─────────────────────────────────────────────────────────

/// Coordinates distributed search and document routing across the cluster.
pub struct SearchCoordinator {
    /// Shard router — maps indices to nodes.
    router: Arc<ShardRouter>,
    /// Transport for remote shard queries.
    transport: Arc<dyn SearchTransport>,
    /// This node's own identifier (local searches bypass the network).
    local_node_id: String,
    /// Local search engine handle (used when the coordinator is on a data node).
    local_searcher: Option<Arc<dyn LocalSearcher>>,
}

impl SearchCoordinator {
    /// Create a new coordinator.
    ///
    /// * `router` — populated shard router.
    /// * `transport` — used for remote shard searches.
    /// * `local_node_id` — skip network for this node ID.
    /// * `local_searcher` — optional local engine handle (required when this
    ///   node also holds shards).
    pub fn new(
        router: Arc<ShardRouter>,
        transport: Arc<dyn SearchTransport>,
        local_node_id: impl Into<String>,
        local_searcher: Option<Arc<dyn LocalSearcher>>,
    ) -> Self {
        SearchCoordinator {
            router,
            transport,
            local_node_id: local_node_id.into(),
            local_searcher,
        }
    }

    // ── Search ────────────────────────────────────────────────────────────────

    /// Execute a distributed search across all shards of `index`.
    ///
    /// 1. Resolves the set of nodes that hold shards for `index`.
    /// 2. Fans out a [`SearchMessage::SearchRequest`] to each node in parallel.
    ///    Local node requests are served in-process; remote nodes are reached via
    ///    the configured [`SearchTransport`].
    /// 3. Collects all responses and merges them with [`merge_search_responses`].
    pub async fn search(
        &self,
        index: &str,
        query_json: &str,
        size: usize,
        from: usize,
    ) -> Result<MergedSearchResult> {
        let targets = self.router.search_targets(index);

        if targets.is_empty() {
            // No shards assigned yet — return empty result rather than an error.
            return Ok(MergedSearchResult {
                hits: vec![],
                total: 0,
                took_ms: 0,
            });
        }

        let request_id = Uuid::new_v4().to_string();

        // Build parallel futures — one per target node.
        let mut futures = Vec::with_capacity(targets.len());

        for node_id in &targets {
            let req = SearchMessage::SearchRequest {
                request_id: request_id.clone(),
                index: index.to_string(),
                query_json: query_json.to_string(),
                size: size + from, // request full page from each shard
                from: 0,           // always from 0; global from applied at merge
            };

            if *node_id == self.local_node_id {
                // Local search — bypass network
                debug!(node = %self.local_node_id, index, "Serving search locally");
                let response = match &self.local_searcher {
                    Some(searcher) => searcher.search_local(
                        &request_id,
                        index,
                        query_json,
                        size + from,
                        0,
                    ),
                    None => {
                        // No local searcher configured — return empty shard response.
                        warn!(
                            node = %self.local_node_id,
                            "Local search requested but no LocalSearcher configured"
                        );
                        SearchMessage::SearchResponse {
                            request_id: request_id.clone(),
                            hits: vec![],
                            total: 0,
                            took_ms: 0,
                        }
                    }
                };
                futures.push(tokio::task::spawn(async move { Ok(response) }));
            } else {
                // Remote search — send over the network
                debug!(node = %node_id, index, "Fanning out search to remote node");
                let transport = Arc::clone(&self.transport);
                let node_id = node_id.clone();

                futures.push(tokio::task::spawn(async move {
                    transport.send_search(&node_id, req).await
                }));
            }
        }

        // Await all futures in parallel.
        let join_results = join_all(futures).await;

        // Collect successful responses; log errors.
        let mut responses: Vec<SearchMessage> = Vec::with_capacity(join_results.len());
        for (i, join_result) in join_results.into_iter().enumerate() {
            match join_result {
                Ok(Ok(msg)) => responses.push(msg),
                Ok(Err(e)) => {
                    warn!(
                        shard_node = %targets[i],
                        error = %e,
                        "Search fan-out failed for node"
                    );
                }
                Err(e) => {
                    warn!(
                        shard_node = %targets[i],
                        error = %e,
                        "Task join error for search fan-out"
                    );
                }
            }
        }

        let (hits, total, took_ms) = merge_search_responses(responses, from, size);

        Ok(MergedSearchResult {
            hits,
            total,
            took_ms,
        })
    }

    // ── Index routing ─────────────────────────────────────────────────────────

    /// Route a document index request to the correct shard owner.
    ///
    /// If the owning node is this node, returns a synthetic local success
    /// response. Otherwise forwards the request to the remote node.
    pub async fn route_index(
        &self,
        index: &str,
        doc_id: &str,
        source_json: &str,
    ) -> Result<IndexResponse> {
        let (_shard, target_opt) = self.router.route_doc(index, doc_id);

        match target_opt {
            Some(node) if node != self.local_node_id => {
                // Forward to remote shard owner
                debug!(
                    target = node,
                    index,
                    doc_id,
                    "Forwarding index request to remote node"
                );
                let req = SearchMessage::IndexRequest {
                    index: index.to_string(),
                    doc_id: doc_id.to_string(),
                    source_json: source_json.to_string(),
                };
                let resp = self.transport.send_search(node, req).await?;
                match resp {
                    SearchMessage::IndexResponse {
                        doc_id,
                        version,
                        result,
                    } => Ok(IndexResponse {
                        doc_id,
                        version,
                        result,
                    }),
                    SearchMessage::Error { message, .. } => {
                        Err(anyhow!("remote index error: {}", message))
                    }
                    other => Err(anyhow!(
                        "unexpected response to IndexRequest: {:?}",
                        other
                    )),
                }
            }
            // Local node owns the shard (or no assignment — handle locally).
            _ => {
                debug!(index, doc_id, "Indexing document locally");
                Ok(IndexResponse {
                    doc_id: doc_id.to_string(),
                    version: 1,
                    result: "created".to_string(),
                })
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // ── Mock search transport ─────────────────────────────────────────────────

    /// Captures all outbound requests and returns preset responses.
    struct MockSearchTransport {
        /// Responses to return keyed by node_id.
        responses: HashMap<String, SearchMessage>,
        /// Log of all calls received.
        calls: Mutex<Vec<(String, SearchMessage)>>,
    }

    use std::collections::HashMap;

    impl MockSearchTransport {
        fn new() -> Self {
            MockSearchTransport {
                responses: HashMap::new(),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn with_response(mut self, node_id: &str, resp: SearchMessage) -> Self {
            self.responses.insert(node_id.to_string(), resp);
            self
        }

        fn calls(&self) -> Vec<(String, SearchMessage)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl SearchTransport for MockSearchTransport {
        async fn send_search(&self, to: &str, msg: SearchMessage) -> Result<SearchMessage> {
            self.calls.lock().unwrap().push((to.to_string(), msg));
            self.responses
                .get(to)
                .cloned()
                .ok_or_else(|| anyhow!("no mock response for node {to}"))
        }
    }

    // ── Mock local searcher ───────────────────────────────────────────────────

    struct MockLocalSearcher {
        hits: Vec<SearchHit>,
        total: u64,
        took_ms: u64,
    }

    impl LocalSearcher for MockLocalSearcher {
        fn search_local(
            &self,
            request_id: &str,
            _index: &str,
            _query_json: &str,
            _size: usize,
            _from: usize,
        ) -> SearchMessage {
            SearchMessage::SearchResponse {
                request_id: request_id.to_string(),
                hits: self.hits.clone(),
                total: self.total,
                took_ms: self.took_ms,
            }
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_response(node_hits: Vec<(&str, f32)>, total: u64, took_ms: u64) -> SearchMessage {
        SearchMessage::SearchResponse {
            request_id: "r".to_string(),
            hits: node_hits
                .into_iter()
                .map(|(id, score)| SearchHit::new(id, score, "{}"))
                .collect(),
            total,
            took_ms,
        }
    }

    // ── Fan-out test ──────────────────────────────────────────────────────────

    /// Coordinator fans out to exactly 3 nodes and merges their responses.
    #[tokio::test]
    async fn test_coordinator_fan_out() {
        let mut router = ShardRouter::new(3);
        router.assign("events", 0, "node-0");
        router.assign("events", 1, "node-1");
        router.assign("events", 2, "node-2");

        let transport = MockSearchTransport::new()
            .with_response("node-1", make_response(vec![("b", 1.5)], 5, 4))
            .with_response("node-2", make_response(vec![("c", 1.2)], 3, 6));

        // node-0 is the "local" node — served by the local searcher.
        let local_searcher = MockLocalSearcher {
            hits: vec![SearchHit::new("a", 2.0, "{}")],
            total: 7,
            took_ms: 1,
        };

        let coordinator = SearchCoordinator::new(
            Arc::new(router),
            Arc::new(transport),
            "node-0",
            Some(Arc::new(local_searcher)),
        );

        let result = coordinator
            .search("events", r#"{"match_all":{}}"#, 10, 0)
            .await
            .unwrap();

        // All three shards contributed
        assert_eq!(result.total, 15, "total = 7+5+3");
        assert_eq!(result.hits.len(), 3, "3 hits across 3 shards");

        // Merged in descending score order
        assert_eq!(result.hits[0].id, "a"); // score 2.0
        assert_eq!(result.hits[1].id, "b"); // score 1.5
        assert_eq!(result.hits[2].id, "c"); // score 1.2

        // took_ms = max of all shards
        assert_eq!(result.took_ms, 6);
    }

    /// Coordinator fans out and remote nodes are called exactly once each.
    #[tokio::test]
    async fn test_coordinator_fan_out_call_count() {
        let mut router = ShardRouter::new(3);
        router.assign("logs", 0, "node-1");
        router.assign("logs", 1, "node-2");
        router.assign("logs", 2, "node-3");

        let transport = Arc::new(
            MockSearchTransport::new()
                .with_response("node-1", make_response(vec![], 0, 1))
                .with_response("node-2", make_response(vec![], 0, 1))
                .with_response("node-3", make_response(vec![], 0, 1)),
        );
        let transport_ref = Arc::clone(&transport);

        let coordinator = SearchCoordinator::new(
            Arc::new(router),
            transport_ref,
            "gateway",
            None,
        );

        coordinator
            .search("logs", "{}", 10, 0)
            .await
            .unwrap();

        let calls = transport.calls();
        assert_eq!(calls.len(), 3, "exactly one call per node");

        let called_nodes: Vec<&str> = calls.iter().map(|(n, _)| n.as_str()).collect();
        assert!(called_nodes.contains(&"node-1"));
        assert!(called_nodes.contains(&"node-2"));
        assert!(called_nodes.contains(&"node-3"));
    }

    // ── Merge test ────────────────────────────────────────────────────────────

    /// Merged results are correctly ranked and paginated.
    #[tokio::test]
    async fn test_coordinator_merge() {
        let mut router = ShardRouter::new(2);
        router.assign("products", 0, "node-a");
        router.assign("products", 1, "node-b");

        let transport = MockSearchTransport::new()
            .with_response(
                "node-a",
                make_response(
                    vec![("p1", 3.0), ("p3", 1.5), ("p5", 0.8)],
                    100,
                    10,
                ),
            )
            .with_response(
                "node-b",
                make_response(
                    vec![("p2", 2.5), ("p4", 1.2), ("p6", 0.5)],
                    80,
                    8,
                ),
            );

        let coordinator = SearchCoordinator::new(
            Arc::new(router),
            Arc::new(transport),
            "gateway",
            None,
        );

        // Page 1: size=2, from=0
        let page1 = coordinator
            .search("products", "{}", 2, 0)
            .await
            .unwrap();
        assert_eq!(page1.hits[0].id, "p1"); // 3.0
        assert_eq!(page1.hits[1].id, "p2"); // 2.5
        assert_eq!(page1.total, 180);

        // Page 2: size=2, from=2 — re-query (in practice the coordinator would
        // cache, but for fan-out each search is independent)
        let transport2 = MockSearchTransport::new()
            .with_response(
                "node-a",
                make_response(
                    vec![("p1", 3.0), ("p3", 1.5), ("p5", 0.8)],
                    100,
                    10,
                ),
            )
            .with_response(
                "node-b",
                make_response(
                    vec![("p2", 2.5), ("p4", 1.2), ("p6", 0.5)],
                    80,
                    8,
                ),
            );

        let mut router2 = ShardRouter::new(2);
        router2.assign("products", 0, "node-a");
        router2.assign("products", 1, "node-b");

        let coordinator2 = SearchCoordinator::new(
            Arc::new(router2),
            Arc::new(transport2),
            "gateway",
            None,
        );

        let page2 = coordinator2
            .search("products", "{}", 2, 2)
            .await
            .unwrap();
        assert_eq!(page2.hits[0].id, "p3"); // 1.5
        assert_eq!(page2.hits[1].id, "p4"); // 1.2
    }

    // ── Empty index test ──────────────────────────────────────────────────────

    /// No shards assigned → empty result, no transport calls.
    #[tokio::test]
    async fn test_coordinator_no_shards() {
        let router = ShardRouter::new(4);
        let transport = Arc::new(MockSearchTransport::new());
        let transport_ref = Arc::clone(&transport);

        let coordinator = SearchCoordinator::new(
            Arc::new(router),
            transport_ref,
            "node-0",
            None,
        );

        let result = coordinator
            .search("empty-index", "{}", 10, 0)
            .await
            .unwrap();

        assert_eq!(result.hits.len(), 0);
        assert_eq!(result.total, 0);
        assert!(transport.calls().is_empty(), "no transport calls for empty index");
    }

    // ── Index routing test ────────────────────────────────────────────────────

    /// route_index returns a local result when doc hashes to the local node.
    #[tokio::test]
    async fn test_coordinator_route_index_local() {
        let mut router = ShardRouter::new(1);
        router.assign("orders", 0, "node-local");

        let coordinator = SearchCoordinator::new(
            Arc::new(router),
            Arc::new(MockSearchTransport::new()),
            "node-local",
            None,
        );

        let resp = coordinator
            .route_index("orders", "doc-abc", r#"{"amount":42}"#)
            .await
            .unwrap();

        assert_eq!(resp.doc_id, "doc-abc");
        assert_eq!(resp.result, "created");
    }

    /// route_index forwards to remote when doc hashes to a different node.
    #[tokio::test]
    async fn test_coordinator_route_index_remote() {
        let mut router = ShardRouter::new(1);
        router.assign("orders", 0, "node-remote");

        let remote_resp = SearchMessage::IndexResponse {
            doc_id: "doc-xyz".to_string(),
            version: 3,
            result: "updated".to_string(),
        };

        let transport = MockSearchTransport::new().with_response("node-remote", remote_resp);

        let coordinator = SearchCoordinator::new(
            Arc::new(router),
            Arc::new(transport),
            "node-local",
            None,
        );

        let resp = coordinator
            .route_index("orders", "doc-xyz", r#"{"amount":99}"#)
            .await
            .unwrap();

        assert_eq!(resp.doc_id, "doc-xyz");
        assert_eq!(resp.version, 3);
        assert_eq!(resp.result, "updated");
    }
}
