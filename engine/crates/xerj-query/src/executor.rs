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

pub const PASSAGE_METADATA_PREFIX: &str = "__xerj_passage_meta__";
pub const PASSAGE_RESPONSE_FIELD: &str = "_passage";

/// Remove engine-owned passage-offset companions before `_source` reaches a
/// client. The metadata remains in stored source for restart/merge correctness.
pub fn strip_internal_passage_metadata(source: &mut serde_json::Value) {
    if let Some(object) = source.as_object_mut() {
        object.retain(|name, _| !name.starts_with(PASSAGE_METADATA_PREFIX));
    }
}

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
    /// ES-wire (0-based) sequence number, read from the SAME version-map
    /// entry `source` was resolved from — never re-resolved later, so a
    /// concurrent write can't pair a stale `source` with a live `seq_no`
    /// (#440). `None` for a doc that is unknown/tombstoned at read time, or
    /// when the deferred-hydration path hasn't reached this hit yet
    /// (resolved in that case by whatever later fills `source`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq_no: Option<u64>,
    /// Document version, from the same read as `seq_no`/`source`. Same
    /// `None` semantics as `seq_no`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
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
    /// Winning passage, populated only when the caller explicitly requests
    /// the `_passage` pseudo-field.
    ///
    /// Semantic/kNN hits derive it from compact ingest-time byte offsets;
    /// lexical hits compute the query-term-densest line-snapped window over
    /// the returned page at query time. Either way it is not persisted as a
    /// second text copy and is absent from ordinary responses.
    #[serde(rename = "_passage", skip_serializing_if = "Option::is_none", default)]
    pub passage: Option<PassageMatch>,
}

/// Provenance for the passage that made a hit relevant: the chunk that
/// supplied a semantic hit's max score, or the query-term-densest window of
/// a lexical hit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PassageMatch {
    /// Original `semantic_text` field, rather than its derived vector field.
    /// For lexical passages: the queried text field the window came from.
    pub field: String,
    /// Semantic hits: zero-based ordinal in the deterministic ingest-time
    /// chunk sequence. Lexical hits have no chunk sequence; here this is the
    /// zero-based LINE index where the passage starts — the "which line of
    /// the file" a caller slicing a large document needs.
    pub ordinal: u32,
    /// UTF-8 byte offsets into `field`.
    pub start_offset: u64,
    pub end_offset: u64,
    /// Winning text slice materialized only for the returned page.
    pub text: String,
    /// Autoindex PDF page identity when the source carries a numeric `page`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u64>,
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

/// What this response did **not** have to send, because of a choice **the
/// caller made** on **this** request.
///
/// The honesty invariants this type exists to enforce (see
/// `SAVINGS-STATS-REPORT.md` for the full rationale):
///
/// * **The baseline is the response the caller would otherwise have
///   received** — same query, same page, no `_source` clause. It is NOT a
///   response containing raw embedding vectors: this engine has not returned
///   those by default since #309, so measuring against them would credit the
///   caller for the engine's own default and overstate the realised saving by
///   ~9x. A blind UX dogfood caught exactly that; the counterfactual is now
///   the only one a reader would recognise.
/// * `bytes` counts only values that were materialised on this request and
///   then left out of this response. Never a hypothetical, never a running
///   total across requests.
/// * The whole struct is `None` when nothing was saved, and the rendered
///   block is `None` when the saving was too small to be worth the bytes
///   spent reporting it — see [`PayloadSavings::rescoped`].
/// * `bytes` is the measured quantity and the only number on the wire. There
///   is deliberately no token conversion: dividing a byte count by a constant
///   produces a bigger number, not more information, and printing
///   "14,008,655 tokens" beside a 22 KB response is what cost an earlier
///   revision its credibility with a real reader.
/// * `measured: "sampled"` appears when long arrays were extrapolated rather
///   than counted. Its **absence means the figure is exact** — an estimate is
///   always labelled, a precise number never needs to be.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PayloadSavings {
    /// Bytes of JSON omitted from this response, relative to the same
    /// response with no `_source` clause.
    pub bytes: u64,
    /// How `bytes` was obtained. Serialized only when `Sampled`.
    pub measured: SavingsMethod,
    /// Short sentence naming the mechanism the caller used. Deliberately
    /// factual rather than congratulatory: obeying a `_source` clause is not
    /// a favour, and a reader told "you asked for 2 fields" learns nothing
    /// they did not just type.
    pub note: String,
    /// Whether the response layer may still re-materialise some of these
    /// bytes through `fields` / `docvalue_fields` / `script_fields`, in which
    /// case it must subtract what it emitted before claiming anything.
    #[serde(skip)]
    pub substitutable: bool,
    /// Per-hit attribution, `(hit id, omitted bytes)`.
    ///
    /// Never serialized: the ES-compat layer re-paginates, de-duplicates and
    /// caps the merged hit set *after* the engine has run, so it re-sums this
    /// over the hits it actually emits. Summing the engine-side total instead
    /// would over-report on multi-index, PIT, kNN-capped and collapsed
    /// searches — i.e. it would inflate.
    #[serde(skip)]
    pub per_hit: Vec<(String, u64)>,
}

/// Which method produced a [`PayloadSavings::bytes`] figure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SavingsMethod {
    /// Long arrays were extrapolated from a bounded, stratified sample.
    /// Everything else — including every array short enough to be cheap —
    /// was counted exactly. Observed error is reported in
    /// `SAVINGS-STATS-REPORT.md`.
    Sampled,
    /// Every omitted byte was serialized and counted. Requested with
    /// `?savings=exact`.
    Exact,
}

/// How many times its own size a saving must be before it is worth printing.
///
/// A block that spends 135 bytes to announce a 19-byte saving is a net loss
/// to the very budget it claims to protect, and the dogfood found that shape
/// on ordinary single-document lookups. Silence is already correct when
/// nothing was saved; this is the same principle applied to "saved so little
/// that telling you costs more than it saved".
pub const SAVINGS_REPORT_FLOOR_RATIO: u64 = 10;

impl PayloadSavings {
    /// Build a savings record, or `None` when nothing was actually saved.
    ///
    /// The `None`-on-zero rule lives here so no call site can accidentally
    /// emit a `"bytes": 0` block.
    pub fn new(
        method: SavingsMethod,
        note: impl Into<String>,
        substitutable: bool,
        per_hit: Vec<(String, u64)>,
    ) -> Option<Self> {
        let bytes: u64 = per_hit.iter().map(|(_, b)| *b).sum();
        if bytes == 0 {
            return None;
        }
        Some(Self {
            bytes,
            measured: method,
            note: note.into(),
            substitutable,
            per_hit,
        })
    }

    /// Render the wire block for a byte total computed over a *different* hit
    /// set (the ES-compat layer's post-merge page).
    ///
    /// Returns `None` when the total is zero, and also when the saving is
    /// less than [`SAVINGS_REPORT_FLOOR_RATIO`] times what the block itself
    /// would add to the response. The floor is measured against the rendered
    /// block, not guessed, so it stays true if the wording changes.
    pub fn rescoped(&self, bytes: u64) -> Option<serde_json::Value> {
        if bytes == 0 {
            return None;
        }
        let mut block = serde_json::Map::new();
        block.insert("bytes".to_string(), bytes.into());
        // Absence means exact; only the estimate carries a label.
        if self.measured == SavingsMethod::Sampled {
            block.insert(
                "measured".to_string(),
                serde_json::to_value(self.measured).ok()?,
            );
        }
        block.insert("note".to_string(), self.note.clone().into());
        let block = serde_json::Value::Object(block);
        // `"_savings":` plus the block is what this costs the response.
        let cost = serde_json::to_vec(&block).ok()?.len() as u64 + 11;
        if bytes < cost.saturating_mul(SAVINGS_REPORT_FLOOR_RATIO) {
            return None;
        }
        Some(block)
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
    /// A Painless **resource-limit** trip (call depth, eval depth, invocation
    /// count, source size) hit while scoring or aggregating this request.
    ///
    /// The scoring paths have no error channel — `apply_function_score` and
    /// friends return a bare `f32` — so before this field a script that blew
    /// the closure call-depth limit silently scored the document `0.0` and
    /// the caller got a wrong number with no indication anything failed.
    /// Populated from the interpreter's fault sink; the API layer turns it
    /// into an error response rather than serving the degraded scores.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub script_failure: Option<String>,
    /// Measured payload the response did not have to carry, populated only
    /// when the caller opted in (`?savings=true`) **and** something was
    /// genuinely omitted. `None` is the correct value for a query that saved
    /// nothing — there is deliberately no zero-valued variant.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub savings: Option<PayloadSavings>,
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
            script_failure: None,
            savings: None,
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
            seq_no: None,
            version: None,
            sort: vec![],
            explain: None,
            highlight: None,
            matched_queries: vec![],
            passage: None,
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
            unmapped_type: None,
            numeric_type: None,
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
