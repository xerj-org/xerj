//! High-level vector search interface.
//!
//! [`VectorSearcher`] wraps an [`HnswIndex`] and handles query normalization,
//! filtered search, and result formatting.

use xerj_common::XerjError;

use crate::distance::DistanceMetric;
use crate::hnsw::HnswIndex;

/// Result alias.
pub type Result<T> = std::result::Result<T, XerjError>;

/// A single search result.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    /// Internal document ID.
    pub doc_id: u64,
    /// Distance from the query (lower = closer).
    pub distance: f32,
    /// Similarity score in `[0, 1]` (higher = more similar).
    pub score: f32,
}

impl SearchResult {
    fn new(doc_id: u64, distance: f32, metric: DistanceMetric) -> Self {
        let score = distance_to_score(distance, metric);
        Self {
            doc_id,
            distance,
            score,
        }
    }
}

/// Convert a raw distance to a [0, 1] similarity score.
fn distance_to_score(dist: f32, metric: DistanceMetric) -> f32 {
    match metric {
        // dist = 1 - cosine_similarity, so similarity = 1 - dist
        DistanceMetric::Cosine => (1.0 - dist).max(0.0),
        // L2 squared: map to [0, 1] via 1/(1+dist)
        DistanceMetric::L2 => 1.0 / (1.0 + dist),
        // Dot product: dist = -dot_product, so score = -dist
        DistanceMetric::DotProduct => (-dist).max(0.0),
    }
}

/// High-level KNN search over an [`HnswIndex`].
pub struct VectorSearcher {
    index: HnswIndex,
    /// Default ef (beam width) for search. Higher = better recall, slower.
    default_ef: usize,
}

impl VectorSearcher {
    /// Create a searcher with the given index and default ef=50.
    pub fn new(index: HnswIndex) -> Self {
        Self {
            index,
            default_ef: 50,
        }
    }

    /// Create a searcher with a custom ef.
    pub fn with_ef(index: HnswIndex, ef: usize) -> Self {
        Self {
            index,
            default_ef: ef,
        }
    }

    /// Get a reference to the underlying index.
    pub fn index(&self) -> &HnswIndex {
        &self.index
    }

    /// Number of indexed vectors.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Perform a K-nearest-neighbor search.
    ///
    /// Returns results sorted by score descending (best first).
    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<SearchResult>> {
        self.search_with_ef(query, k, self.default_ef)
    }

    /// KNN search with an explicit ef override.
    pub fn search_with_ef(&self, query: &[f32], k: usize, ef: usize) -> Result<Vec<SearchResult>> {
        let metric = self.index.params().metric;
        let raw = self.index.search(query, k, ef)?;
        let mut results: Vec<SearchResult> = raw
            .into_iter()
            .map(|(id, dist)| SearchResult::new(id, dist, metric))
            .collect();
        // Sort by score descending
        results.sort_unstable_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(results)
    }

    /// KNN search with a filter predicate pushed into graph traversal.
    ///
    /// The filter is applied **during** graph exploration, not as a post-filter.
    /// This is critical for high-selectivity filters — post-filtering would
    /// require retrieving many more candidates to find k valid results.
    pub fn search_filtered(
        &self,
        query: &[f32],
        k: usize,
        filter: &dyn Fn(u64) -> bool,
    ) -> Result<Vec<SearchResult>> {
        self.search_filtered_with_ef(query, k, self.default_ef, filter)
    }

    /// Filtered KNN search with explicit ef.
    pub fn search_filtered_with_ef(
        &self,
        query: &[f32],
        k: usize,
        ef: usize,
        filter: &dyn Fn(u64) -> bool,
    ) -> Result<Vec<SearchResult>> {
        let metric = self.index.params().metric;
        // Use a larger ef to compensate for filtered nodes
        let effective_ef = (ef * 4).max(k * 2);
        let raw = self.index.search_filtered(query, k, effective_ef, filter)?;
        let mut results: Vec<SearchResult> = raw
            .into_iter()
            .map(|(id, dist)| SearchResult::new(id, dist, metric))
            .collect();
        results.sort_unstable_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(results)
    }

    /// Insert a vector into the underlying index.
    pub fn insert(&self, id: u64, vector: Vec<f32>) -> Result<()> {
        self.index.insert(id, vector)
    }

    /// Batch insert vectors.
    pub fn insert_batch(&self, items: Vec<(u64, Vec<f32>)>) -> Result<()> {
        self.index.insert_batch(items)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hnsw::{HnswIndex, HnswParams};

    fn make_searcher(dim: usize) -> VectorSearcher {
        let params = HnswParams::new(dim, DistanceMetric::Cosine);
        VectorSearcher::new(HnswIndex::new(params))
    }

    #[test]
    fn search_returns_best_match() {
        let s = make_searcher(3);
        s.insert(0, vec![1.0, 0.0, 0.0]).unwrap();
        s.insert(1, vec![0.0, 1.0, 0.0]).unwrap();
        s.insert(2, vec![0.0, 0.0, 1.0]).unwrap();

        let results = s.search(&[1.0, 0.0, 0.0], 1).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].doc_id, 0);
        // Score should be close to 1.0 for identical vectors
        assert!(results[0].score > 0.99, "score was {}", results[0].score);
    }

    #[test]
    fn scores_are_descending() {
        let s = make_searcher(3);
        for i in 0..10u64 {
            s.insert(i, vec![i as f32, 1.0, 0.0]).unwrap();
        }
        let results = s.search(&[5.0, 1.0, 0.0], 5).unwrap();
        for window in results.windows(2) {
            assert!(
                window[0].score >= window[1].score,
                "scores not descending: {} < {}",
                window[0].score,
                window[1].score
            );
        }
    }

    #[test]
    fn filtered_search_respects_predicate() {
        let s = make_searcher(4);
        for i in 0..20u64 {
            s.insert(i, vec![i as f32, 0.0, 0.0, 1.0]).unwrap();
        }

        let results = s
            .search_filtered(&[10.0, 0.0, 0.0, 1.0], 5, &|id| id % 3 == 0)
            .unwrap();

        for r in &results {
            assert_eq!(r.doc_id % 3, 0, "id {} should be divisible by 3", r.doc_id);
        }
    }

    #[test]
    fn empty_searcher() {
        let s = make_searcher(4);
        assert!(s.is_empty());
        let results = s.search(&[1.0, 0.0, 0.0, 0.0], 5).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn distance_to_score_cosine() {
        // distance 0 → score 1.0 (identical direction)
        assert!((distance_to_score(0.0, DistanceMetric::Cosine) - 1.0).abs() < 1e-6);
        // distance 2.0 → score 0.0 (opposite direction, clamped)
        assert_eq!(distance_to_score(2.0, DistanceMetric::Cosine), 0.0);
    }
}
