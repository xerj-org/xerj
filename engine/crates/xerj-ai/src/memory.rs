//! Agent memory: append-only semantic memory with dedup and recency blending.
//!
//! Stores text snippets with embeddings. On recall, blends cosine similarity
//! with recency using a configurable weight, so recently stored memories are
//! surfaced even if slightly less semantically similar than older ones.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Mutex;
use tracing::debug;
use xerj_common::XerjError;

/// Result alias.
pub type Result<T> = std::result::Result<T, XerjError>;

// ─────────────────────────────────────────────────────────────────────────────
// MemoryEntry
// ─────────────────────────────────────────────────────────────────────────────

/// A stored memory entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// Unique ID (monotonically assigned).
    pub id: u64,
    /// The text content.
    pub text: String,
    /// Arbitrary metadata (tags, source, etc.).
    pub metadata: Value,
    /// Dense embedding vector.
    pub embedding: Vec<f32>,
    /// Unix timestamp (seconds) when this was stored.
    pub stored_at: i64,
}

impl MemoryEntry {
    /// Blended score combining similarity and recency.
    ///
    /// `recency_weight` ∈ [0, 1]:
    /// - 0.0 → pure semantic similarity
    /// - 1.0 → pure recency
    pub fn score(&self, similarity: f32, now_secs: i64, recency_weight: f32) -> f32 {
        let age_secs = (now_secs - self.stored_at).max(0) as f32;
        // Decay: e^(-age / half_life) where half_life = 7 days in seconds
        let half_life = 7.0 * 86_400.0f32;
        let recency = (-age_secs / half_life).exp();

        let semantic = (1.0 - recency_weight).max(0.0);
        let recency_w = recency_weight.min(1.0);

        similarity * semantic + recency * recency_w
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Similarity helpers
// ─────────────────────────────────────────────────────────────────────────────

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (&ai, &bi) in a.iter().zip(b.iter()) {
        dot += ai * bi;
        na += ai * ai;
        nb += bi * bi;
    }
    let denom = (na * nb).sqrt();
    if denom == 0.0 { 0.0 } else { dot / denom }
}

// ─────────────────────────────────────────────────────────────────────────────
// AgentMemory
// ─────────────────────────────────────────────────────────────────────────────

/// Append-only in-memory store with semantic dedup.
///
/// Store: embeds the text, checks similarity against existing entries, and
/// skips insertion if the closest match exceeds the dedup threshold.
///
/// Recall: scores all entries by blended similarity + recency, returns top-k.
///
/// # Note on embeddings
///
/// `AgentMemory` stores pre-computed embeddings. The caller is responsible for
/// embedding the text (e.g., via [`EmbeddingProxy`]) before calling
/// [`store_embedded`] or [`store`] with a pre-computed vector.
pub struct AgentMemory {
    entries: Mutex<Vec<MemoryEntry>>,
    next_id: Mutex<u64>,
    /// Cosine similarity threshold above which a new entry is considered
    /// duplicate and discarded.
    dedup_threshold: f32,
}

impl AgentMemory {
    /// Create a new memory store with the given dedup threshold.
    ///
    /// `dedup_threshold` ∈ [0, 1]: entries with similarity above this value
    /// are considered duplicates. Recommended: 0.95.
    pub fn new(dedup_threshold: f32) -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            next_id: Mutex::new(0),
            dedup_threshold,
        }
    }

    /// Store a text with a pre-computed embedding.
    ///
    /// Returns `None` if the entry was deduplicated (too similar to existing).
    pub fn store_embedded(
        &self,
        text: impl Into<String>,
        embedding: Vec<f32>,
        metadata: Value,
        stored_at_secs: i64,
    ) -> Option<u64> {
        let text = text.into();
        let mut entries = self.entries.lock().unwrap();

        // Semantic dedup check
        for existing in entries.iter() {
            if existing.embedding.len() == embedding.len() {
                let sim = cosine_similarity(&existing.embedding, &embedding);
                if sim >= self.dedup_threshold {
                    debug!(
                        "memory dedup: skipping '{}' (sim={:.3} vs '{}')",
                        &text[..text.len().min(40)],
                        sim,
                        &existing.text[..existing.text.len().min(40)]
                    );
                    return None;
                }
            }
        }

        let id = {
            let mut next = self.next_id.lock().unwrap();
            let id = *next;
            *next += 1;
            id
        };

        entries.push(MemoryEntry {
            id,
            text,
            metadata,
            embedding,
            stored_at: stored_at_secs,
        });

        Some(id)
    }

    /// Recall the top-k most relevant memories for a query embedding.
    ///
    /// `recency_weight` ∈ [0, 1]:
    /// - 0.0 → pure semantic similarity ordering
    /// - 1.0 → pure recency ordering
    pub fn recall(
        &self,
        query_embedding: &[f32],
        k: usize,
        recency_weight: f32,
        now_secs: i64,
    ) -> Vec<MemoryEntry> {
        let entries = self.entries.lock().unwrap();

        let mut scored: Vec<(f32, &MemoryEntry)> = entries
            .iter()
            .filter_map(|e| {
                if e.embedding.len() != query_embedding.len() {
                    return None;
                }
                let sim = cosine_similarity(query_embedding, &e.embedding);
                let score = e.score(sim, now_secs, recency_weight);
                Some((score, e))
            })
            .collect();

        // Sort descending by score
        scored.sort_unstable_by(|a, b| {
            b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
        });

        scored.into_iter().take(k).map(|(_, e)| e.clone()).collect()
    }

    /// Number of stored entries.
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clear all stored memories.
    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
        *self.next_id.lock().unwrap() = 0;
    }

    /// Return all stored entries (for inspection/serialization).
    pub fn all_entries(&self) -> Vec<MemoryEntry> {
        self.entries.lock().unwrap().clone()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vec_a() -> Vec<f32> { vec![1.0, 0.0, 0.0, 0.0] }
    fn vec_b() -> Vec<f32> { vec![0.0, 1.0, 0.0, 0.0] }
    fn vec_c() -> Vec<f32> { vec![0.0, 0.0, 1.0, 0.0] }
    // Very similar to vec_a
    fn vec_a_near() -> Vec<f32> { vec![0.999, 0.001, 0.0, 0.0] }

    #[test]
    fn store_and_recall_basic() {
        let mem = AgentMemory::new(0.95);
        let now = 1_700_000_000i64;

        mem.store_embedded("memory about cats", vec_a(), json!({}), now);
        mem.store_embedded("memory about dogs", vec_b(), json!({}), now);
        mem.store_embedded("memory about fish", vec_c(), json!({}), now);

        assert_eq!(mem.len(), 3);

        let results = mem.recall(&vec_a(), 1, 0.0, now);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].text, "memory about cats");
    }

    #[test]
    fn dedup_skips_near_duplicate() {
        let mem = AgentMemory::new(0.95);
        let now = 1_700_000_000i64;

        let id1 = mem.store_embedded("cats", vec_a(), json!({}), now);
        let id2 = mem.store_embedded("cats (near duplicate)", vec_a_near(), json!({}), now);

        assert!(id1.is_some());
        assert!(id2.is_none(), "near-duplicate should be deduped");
        assert_eq!(mem.len(), 1);
    }

    #[test]
    fn dedup_allows_distinct_entries() {
        let mem = AgentMemory::new(0.95);
        let now = 1_700_000_000i64;

        mem.store_embedded("cats", vec_a(), json!({}), now);
        mem.store_embedded("dogs", vec_b(), json!({}), now);

        assert_eq!(mem.len(), 2);
    }

    #[test]
    fn recency_weight_affects_ordering() {
        let mem = AgentMemory::new(0.5);
        let base = 1_700_000_000i64;

        // Store old entry with matching vector and recent entry with different vector
        mem.store_embedded("old cats", vec_a(), json!({}), base - 30 * 86_400); // 30 days ago
        mem.store_embedded("recent dogs", vec_b(), json!({}), base); // now

        // Query close to vec_a
        let query = vec![0.9, 0.1, 0.0, 0.0f32];

        // Pure similarity: old cats should rank first (closer to query)
        let by_sim = mem.recall(&query, 2, 0.0, base);
        assert_eq!(by_sim[0].text, "old cats");

        // Pure recency: recent dogs should rank first
        let by_recency = mem.recall(&query, 2, 1.0, base);
        assert_eq!(by_recency[0].text, "recent dogs");
    }

    #[test]
    fn recall_empty_memory() {
        let mem = AgentMemory::new(0.95);
        let results = mem.recall(&vec_a(), 5, 0.5, 1_700_000_000);
        assert!(results.is_empty());
    }

    #[test]
    fn clear_resets_state() {
        let mem = AgentMemory::new(0.95);
        mem.store_embedded("a", vec_a(), json!({}), 1_700_000_000);
        mem.clear();
        assert_eq!(mem.len(), 0);
    }

    #[test]
    fn ids_are_unique() {
        // Use threshold 2.0 (above max cosine similarity of 1.0) to disable dedup
        let mem = AgentMemory::new(2.0);
        let ids: Vec<u64> = (0..5)
            .map(|i| {
                // Each vector points in a completely different direction
                let mut v = vec![0.0f32; 5];
                v[i] = 1.0;
                mem.store_embedded(format!("entry {i}"), v, json!({}), 1_700_000_000)
                    .unwrap()
            })
            .collect();
        let unique: std::collections::HashSet<u64> = ids.into_iter().collect();
        assert_eq!(unique.len(), 5);
    }
}
