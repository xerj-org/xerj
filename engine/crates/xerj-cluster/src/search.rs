//! Inter-node search and indexing message types.
//!
//! These messages are exchanged between a gateway node (which receives external
//! search requests) and data nodes (which hold the actual index shards).
//!
//! The protocol is intentionally simple:
//! - The gateway fans out a [`SearchMessage::SearchRequest`] to each relevant
//!   shard node.
//! - Each shard node replies with a [`SearchMessage::SearchResponse`] containing
//!   local hits.
//! - The gateway merges the partial results, re-ranks, and returns to the client.
//!
//! For document indexing the flow is reversed: the gateway forwards an
//! [`SearchMessage::IndexRequest`] to the node owning the target shard, which
//! indexes locally and replies with a [`SearchMessage::IndexResponse`].

use serde::{Deserialize, Serialize};

// ── Message types ─────────────────────────────────────────────────────────────

/// All messages exchanged between cluster nodes for search and indexing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SearchMessage {
    // ── Search ────────────────────────────────────────────────────────────────
    /// Gateway → Search node: execute query on local shards.
    SearchRequest {
        /// Unique identifier for correlating requests and responses.
        request_id: String,
        /// Index (or alias / wildcard) to search.
        index: String,
        /// ES-compatible query JSON (the value of the `"query"` key).
        query_json: String,
        /// Maximum number of hits to return from this shard.
        size: usize,
        /// Starting offset for pagination.
        from: usize,
    },

    /// Search node → Gateway: partial results from a single shard.
    SearchResponse {
        /// Mirrors the `request_id` from the corresponding [`SearchRequest`].
        request_id: String,
        /// Scored hits from this shard, sorted by descending score.
        hits: Vec<SearchHit>,
        /// Total number of matching documents on this shard (before `from`/`size`).
        total: u64,
        /// Time taken to execute the query on this shard, in milliseconds.
        took_ms: u64,
    },

    // ── Indexing ──────────────────────────────────────────────────────────────
    /// Gateway → Node: forward a document for indexing on the target shard.
    IndexRequest {
        /// Index to write into.
        index: String,
        /// Document identifier.
        doc_id: String,
        /// Full document source as JSON.
        source_json: String,
    },

    /// Node → Gateway: acknowledgement of a successful index operation.
    IndexResponse {
        /// Document identifier that was indexed.
        doc_id: String,
        /// New document version number.
        version: u64,
        /// Human-readable result string, e.g. `"created"` or `"updated"`.
        result: String,
    },

    // ── Errors ────────────────────────────────────────────────────────────────
    /// Any node → requester: signals that a request could not be fulfilled.
    Error {
        /// The `request_id` from the original request, if available.
        request_id: Option<String>,
        /// Short machine-readable error code (e.g. `"index_not_found"`).
        code: String,
        /// Human-readable error description.
        message: String,
    },
}

impl SearchMessage {
    /// Returns the `request_id` for the message, if one is present.
    pub fn request_id(&self) -> Option<&str> {
        match self {
            SearchMessage::SearchRequest { request_id, .. } => Some(request_id),
            SearchMessage::SearchResponse { request_id, .. } => Some(request_id),
            SearchMessage::Error { request_id, .. } => request_id.as_deref(),
            SearchMessage::IndexRequest { .. } | SearchMessage::IndexResponse { .. } => None,
        }
    }

    /// Returns `true` if this is a response (i.e., flows back toward the gateway).
    pub fn is_response(&self) -> bool {
        matches!(
            self,
            SearchMessage::SearchResponse { .. }
                | SearchMessage::IndexResponse { .. }
                | SearchMessage::Error { .. }
        )
    }
}

// ── SearchHit ─────────────────────────────────────────────────────────────────

/// A single scored document returned from a shard.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchHit {
    /// Document identifier.
    pub id: String,
    /// Relevance score computed on this shard (BM25 / vector / hybrid).
    pub score: f32,
    /// Full document source JSON.
    pub source_json: String,
}

impl SearchHit {
    pub fn new(id: impl Into<String>, score: f32, source_json: impl Into<String>) -> Self {
        SearchHit {
            id: id.into(),
            score,
            source_json: source_json.into(),
        }
    }
}

// ── Merge helpers ─────────────────────────────────────────────────────────────

/// Merge partial search responses from multiple shards into a single result set.
///
/// * Concatenates all hits.
/// * Sorts by descending score (ties broken by document ID for stability).
/// * Trims to `[from, from+size)`.
/// * Sums `total` and takes the maximum `took_ms`.
pub fn merge_search_responses(
    responses: Vec<SearchMessage>,
    from: usize,
    size: usize,
) -> (Vec<SearchHit>, u64, u64) {
    let mut all_hits: Vec<SearchHit> = Vec::new();
    let mut total: u64 = 0;
    let mut took_ms: u64 = 0;

    for resp in responses {
        if let SearchMessage::SearchResponse {
            hits,
            total: t,
            took_ms: ms,
            ..
        } = resp
        {
            all_hits.extend(hits);
            total += t;
            took_ms = took_ms.max(ms);
        }
    }

    // Sort by descending score, then by id for determinism.
    all_hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });

    // Apply from/size window.
    let page: Vec<SearchHit> = all_hits.into_iter().skip(from).take(size).collect();

    (page, total, took_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_message_serde() {
        let msg = SearchMessage::SearchRequest {
            request_id: "req-001".to_string(),
            index: "products".to_string(),
            query_json: r#"{"match":{"title":"laptop"}}"#.to_string(),
            size: 10,
            from: 0,
        };

        let json = serde_json::to_string(&msg).unwrap();
        let decoded: SearchMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_search_response_serde() {
        let resp = SearchMessage::SearchResponse {
            request_id: "req-001".to_string(),
            hits: vec![
                SearchHit::new("doc-1", 1.5, r#"{"title":"laptop"}"#),
                SearchHit::new("doc-2", 1.2, r#"{"title":"gaming laptop"}"#),
            ],
            total: 42,
            took_ms: 3,
        };

        let json = serde_json::to_string(&resp).unwrap();
        let decoded: SearchMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, decoded);
    }

    #[test]
    fn test_index_request_serde() {
        let req = SearchMessage::IndexRequest {
            index: "orders".to_string(),
            doc_id: "order-42".to_string(),
            source_json: r#"{"amount":99.99,"status":"shipped"}"#.to_string(),
        };

        let json = serde_json::to_string(&req).unwrap();
        let decoded: SearchMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn test_index_response_serde() {
        let resp = SearchMessage::IndexResponse {
            doc_id: "order-42".to_string(),
            version: 1,
            result: "created".to_string(),
        };

        let json = serde_json::to_string(&resp).unwrap();
        let decoded: SearchMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, decoded);
    }

    #[test]
    fn test_merge_search_responses() {
        let r1 = SearchMessage::SearchResponse {
            request_id: "r".to_string(),
            hits: vec![
                SearchHit::new("a", 2.0, "{}"),
                SearchHit::new("c", 0.5, "{}"),
            ],
            total: 10,
            took_ms: 5,
        };
        let r2 = SearchMessage::SearchResponse {
            request_id: "r".to_string(),
            hits: vec![
                SearchHit::new("b", 1.5, "{}"),
                SearchHit::new("d", 0.3, "{}"),
            ],
            total: 8,
            took_ms: 3,
        };

        let (hits, total, took_ms) = merge_search_responses(vec![r1, r2], 0, 3);

        assert_eq!(total, 18);
        assert_eq!(took_ms, 5);
        assert_eq!(hits.len(), 3);
        // Should be sorted by descending score: a(2.0), b(1.5), c(0.5)
        assert_eq!(hits[0].id, "a");
        assert_eq!(hits[1].id, "b");
        assert_eq!(hits[2].id, "c");
    }

    #[test]
    fn test_is_response() {
        assert!(!SearchMessage::SearchRequest {
            request_id: "r".into(),
            index: "i".into(),
            query_json: "{}".into(),
            size: 10,
            from: 0,
        }
        .is_response());

        assert!(SearchMessage::SearchResponse {
            request_id: "r".into(),
            hits: vec![],
            total: 0,
            took_ms: 0,
        }
        .is_response());
    }
}
