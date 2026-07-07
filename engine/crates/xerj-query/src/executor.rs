//! Query executor — runs an [`ExecutionPlan`] and produces [`SearchResult`].
//!
//! ## Design
//!
//! Unlike Elasticsearch's two-phase query+fetch model (first collect doc IDs
//! and scores, then fetch source), xerj uses a **single-pass** model:
//!
//! 1. Each segment engine implements the [`SegmentExecutor`] trait.
//! 2. The top-level executor fans out to all segments in parallel (via rayon).
//! 3. Results from all segments are merged into a single top-K heap.
//! 4. Source is fetched as hits are produced — no second network round-trip.
//!
//! ## Pagination
//!
//! `from`/`size` pagination is supported for small offsets.  For deep
//! pagination, use `search_after` (keyset pagination), which avoids the
//! O(from + size) cost of the heap.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::planner::ExecutionPlan;
use crate::sort::{compare_sort_keys, SortField};

// ─────────────────────────────────────────────────────────────────────────────
// Result types
// ─────────────────────────────────────────────────────────────────────────────

/// A single matched document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    /// The document's external string ID (the ES `_id`).
    pub id: String,
    /// BM25 / vector / fusion score.
    pub score: f32,
    /// The document source fields (filtered per `_source`).
    #[serde(rename = "_source")]
    pub source: serde_json::Value,
    /// The sort key values for this hit (used by `search_after`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sort: Vec<serde_json::Value>,
    /// Per-field scoring explanation (only when `explain: true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<Explanation>,
    /// Highlight fragments per field (only when `highlight` was in the request).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highlight: Option<std::collections::HashMap<String, Vec<String>>>,
    /// Names of named queries that matched this document (only present when `_name` was used).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub matched_queries: Vec<String>,
}

/// Total hits information (mirrors ES semantics exactly).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TotalHits {
    /// The number of matching documents.
    pub value: u64,
    /// Whether `value` is exact (`eq`) or a lower bound (`gte`).
    pub relation: TotalHitsRelation,
}

/// Indicates whether the total hit count is exact or a lower bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TotalHitsRelation {
    /// `value` is the exact count.
    Eq,
    /// `value` is a lower bound (e.g. when `track_total_hits: false` or a
    /// timeout occurred before all segments were scanned).
    Gte,
}

/// A scoring explanation node (mirrors ES `_explanation`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Explanation {
    pub value: f32,
    pub description: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<Explanation>,
}

impl Explanation {
    pub fn leaf(value: f32, description: impl Into<String>) -> Self {
        Self {
            value,
            description: description.into(),
            details: vec![],
        }
    }

    pub fn compound(value: f32, description: impl Into<String>, details: Vec<Explanation>) -> Self {
        Self {
            value,
            description: description.into(),
            details,
        }
    }
}

/// The complete search response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// The hits returned for this page.
    pub hits: Vec<Hit>,
    /// Total matching documents (may be approximate).
    pub total: TotalHits,
    /// Wall-clock time from request receipt to response.
    pub took_ms: u64,
    /// Aggregation results (opaque JSON blob, shaped by the `aggs` request).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggs: Option<serde_json::Value>,
    /// Whether the query was cut short by a timeout (partial results).
    #[serde(default)]
    pub timed_out: bool,
    /// Profile data — timing breakdown for query execution phases.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<serde_json::Value>,
    /// Highest score across ALL matched docs, before collapse/pagination.
    /// Used for ES `max_score` with collapse + track_scores (search/111).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub max_score: Option<f32>,
}

impl SearchResult {
    /// Construct an empty result (used for `MatchNone` and error recovery).
    pub fn empty(took_ms: u64) -> Self {
        Self {
            hits: vec![],
            total: TotalHits {
                value: 0,
                relation: TotalHitsRelation::Eq,
            },
            took_ms,
            aggs: None,
            timed_out: false,
            profile: None,
            max_score: None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SegmentExecutor trait
// ─────────────────────────────────────────────────────────────────────────────

/// The interface that each segment engine must implement.
///
/// The query executor calls `execute` on every live segment and then merges
/// the results.  Each segment returns the top-`limit` hits it found, which the
/// top-level executor merges into the global top-K.
///
/// # Thread safety
///
/// Segments are executed in parallel via rayon, so implementations must be
/// `Send + Sync`.  In practice this means holding segment data behind an
/// `Arc<RwLock<…>>` or using memory-mapped read-only slices.
pub trait SegmentExecutor: Send + Sync {
    /// Execute `plan` within this segment and return up to `limit` hits.
    ///
    /// * `limit`        — how many hits to return (at most `from + size`).
    /// * `sort_fields`  — determines hit ordering within the segment.
    /// * `search_after` — keyset cursor; skip hits that sort ≤ cursor.
    /// * `explain`      — populate `Hit::explain` if `true`.
    fn execute(
        &self,
        plan: &ExecutionPlan,
        limit: usize,
        sort_fields: &[SortField],
        search_after: Option<&[serde_json::Value]>,
        explain: bool,
    ) -> Result<Vec<Hit>>;

    /// Count matching documents without returning source or scores.
    ///
    /// May be much cheaper than `execute` when only the total count is needed.
    fn count(&self, plan: &ExecutionPlan) -> Result<u64>;

    /// A human-readable identifier for this segment (for logging/tracing).
    fn segment_id(&self) -> &str;
}

// ─────────────────────────────────────────────────────────────────────────────
// Top-level merge
// ─────────────────────────────────────────────────────────────────────────────

/// Merge hits from multiple segments into a single sorted top-K result.
///
/// # Parameters
///
/// * `segment_hits`  — Per-segment hit lists (each already locally sorted).
/// * `from`          — Skip this many hits from the merged result.
/// * `size`          — Return at most this many hits after `from`.
/// * `sort_fields`   — The sort specification used to compare hits.
///
/// This function runs in O((N·log·k)) time where N is total hits across all
/// segments and k = `from + size`.
pub fn merge_hits(
    segment_hits: Vec<Vec<Hit>>,
    from: usize,
    size: usize,
    sort_fields: &[SortField],
) -> Vec<Hit> {
    let limit = from + size;
    if limit == 0 {
        return vec![];
    }

    // We use a max-heap keyed by (score, id) for the default _score sort,
    // or a general comparator for field sorts.
    //
    // For simplicity we flatten all hits and sort.  For production we would
    // use a k-way merge with per-segment cursors.
    let mut all: Vec<Hit> = segment_hits.into_iter().flatten().collect();

    if sort_fields.is_empty() || (sort_fields.len() == 1 && sort_fields[0].is_score()) {
        // Default: sort by score descending, then id ascending as tiebreaker.
        all.sort_unstable_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
    } else {
        all.sort_unstable_by(|a, b| {
            compare_sort_keys(&a.sort, &b.sort, sort_fields).then_with(|| a.id.cmp(&b.id))
        });
    }

    all.into_iter().skip(from).take(size).collect()
}

/// Merge per-segment total-hit counts.
///
/// If any segment returned `Gte`, the merged relation is also `Gte`.
pub fn merge_totals(segment_totals: &[(u64, TotalHitsRelation)]) -> TotalHits {
    let mut total = 0u64;
    let mut relation = TotalHitsRelation::Eq;
    for (count, rel) in segment_totals {
        total = total.saturating_add(*count);
        if *rel == TotalHitsRelation::Gte {
            relation = TotalHitsRelation::Gte;
        }
    }
    TotalHits {
        value: total,
        relation,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// score-based Top-K heap  (used by segment implementations)
// ─────────────────────────────────────────────────────────────────────────────

/// A fixed-capacity max-heap for collecting top-K hits by score.
///
/// Segment executors should use this to bound memory during scan.
pub struct TopKHeap {
    capacity: usize,
    /// `Reverse` so the heap is a min-heap (we drop the lowest scorer).
    inner: BinaryHeap<Reverse<ScoredHit>>,
}

#[derive(Debug, PartialEq)]
struct ScoredHit {
    score: f32,
    id: String,
    hit: Hit,
}

impl Eq for ScoredHit {}
impl PartialOrd for ScoredHit {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ScoredHit {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.score
            .partial_cmp(&other.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| self.id.cmp(&other.id).reverse())
    }
}

impl TopKHeap {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            inner: BinaryHeap::with_capacity(capacity + 1),
        }
    }

    /// Push a hit, evicting the lowest scorer if over capacity.
    pub fn push(&mut self, hit: Hit) {
        let sh = ScoredHit {
            score: hit.score,
            id: hit.id.clone(),
            hit,
        };
        self.inner.push(Reverse(sh));
        if self.inner.len() > self.capacity {
            self.inner.pop(); // evict the lowest scorer
        }
    }

    /// Drain the heap in score-descending order.
    pub fn into_sorted_hits(self) -> Vec<Hit> {
        let mut hits: Vec<Hit> = self.inner.into_iter().map(|Reverse(sh)| sh.hit).collect();
        hits.sort_unstable_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits
    }

    /// Current number of hits in the heap.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the heap currently holds no hits.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Minimum score currently in the heap (used for early termination).
    pub fn min_score(&self) -> Option<f32> {
        self.inner.peek().map(|Reverse(sh)| sh.score)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sort::{SortMissing, SortMode, SortOrder};

    fn hit(id: &str, score: f32) -> Hit {
        Hit {
            id: id.to_string(),
            score,
            source: serde_json::Value::Null,
            sort: vec![],
            explain: None,
            highlight: None,
            matched_queries: vec![],
        }
    }

    // ── TopKHeap ──────────────────────────────────────────────────────────────

    #[test]
    fn test_topk_heap_basic() {
        let mut heap = TopKHeap::new(3);
        heap.push(hit("a", 0.5));
        heap.push(hit("b", 0.9));
        heap.push(hit("c", 0.3));
        heap.push(hit("d", 0.7)); // evicts "c" (lowest)

        let hits = heap.into_sorted_hits();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].id, "b"); // 0.9
        assert_eq!(hits[1].id, "d"); // 0.7
        assert_eq!(hits[2].id, "a"); // 0.5
    }

    #[test]
    fn test_topk_heap_capacity() {
        let mut heap = TopKHeap::new(2);
        for i in 0..10 {
            heap.push(hit(&i.to_string(), i as f32 * 0.1));
        }
        assert_eq!(heap.len(), 2);
        let hits = heap.into_sorted_hits();
        // Should keep the two highest: 9 (0.9) and 8 (0.8)
        assert_eq!(hits[0].id, "9");
        assert_eq!(hits[1].id, "8");
    }

    // ── merge_hits ────────────────────────────────────────────────────────────

    #[test]
    fn test_merge_hits_by_score() {
        let seg1 = vec![hit("a", 0.9), hit("b", 0.5)];
        let seg2 = vec![hit("c", 0.8), hit("d", 0.3)];

        let merged = merge_hits(vec![seg1, seg2], 0, 3, &[]);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].id, "a");
        assert_eq!(merged[1].id, "c");
        assert_eq!(merged[2].id, "b");
    }

    #[test]
    fn test_merge_hits_from_offset() {
        let seg = vec![hit("a", 0.9), hit("b", 0.8), hit("c", 0.7), hit("d", 0.6)];
        let merged = merge_hits(vec![seg], 2, 2, &[]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].id, "c");
        assert_eq!(merged[1].id, "d");
    }

    #[test]
    fn test_merge_hits_field_sort() {
        let mut h1 = hit("a", 0.5);
        h1.sort = vec![serde_json::json!(1)];
        let mut h2 = hit("b", 0.9);
        h2.sort = vec![serde_json::json!(3)];
        let mut h3 = hit("c", 0.1);
        h3.sort = vec![serde_json::json!(2)];

        let sort_fields = vec![SortField {
            field: "num".to_string(),
            order: SortOrder::Asc,
            mode: SortMode::default(),
            missing: SortMissing::Last,
            format: None,
        }];

        let merged = merge_hits(vec![vec![h1, h2, h3]], 0, 10, &sort_fields);
        // Should be sorted by num ascending: 1, 2, 3
        assert_eq!(merged[0].id, "a");
        assert_eq!(merged[1].id, "c");
        assert_eq!(merged[2].id, "b");
    }

    // ── merge_totals ──────────────────────────────────────────────────────────

    #[test]
    fn test_merge_totals_exact() {
        let totals = vec![
            (100u64, TotalHitsRelation::Eq),
            (50u64, TotalHitsRelation::Eq),
        ];
        let result = merge_totals(&totals);
        assert_eq!(result.value, 150);
        assert_eq!(result.relation, TotalHitsRelation::Eq);
    }

    #[test]
    fn test_merge_totals_approximate() {
        let totals = vec![
            (100u64, TotalHitsRelation::Eq),
            (50u64, TotalHitsRelation::Gte), // one segment was approximate
        ];
        let result = merge_totals(&totals);
        assert_eq!(result.value, 150);
        assert_eq!(result.relation, TotalHitsRelation::Gte);
    }

    // ── SearchResult ──────────────────────────────────────────────────────────

    #[test]
    fn test_empty_result() {
        let r = SearchResult::empty(5);
        assert!(r.hits.is_empty());
        assert_eq!(r.total.value, 0);
        assert_eq!(r.total.relation, TotalHitsRelation::Eq);
        assert_eq!(r.took_ms, 5);
    }
}
