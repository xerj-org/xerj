//! Search execution over a segment's FTS index.
//!
//! ## Query types
//!
//! | Query       | Description                                      |
//! |-------------|--------------------------------------------------|
//! | `TermQuery` | Single-term lookup: FST → postings → BM25 score  |
//! | `PhraseQuery`| Ordered adjacent term positions in same document |
//! | `BoolQuery` | must / should / must_not combinator              |
//! | `PrefixQuery`| FST range scan for all terms with a given prefix |
//!
//! ## Parallelism
//!
//! When searching multiple segments, `FtsSearcher` uses rayon to fan out the
//! per-segment search in parallel and merge sorted results.

use crate::{
    analyzer::AnalyzerRegistry,
    bm25::{Bm25Scorer, QueryExplanation, ScoreBreakdown},
    index::FtsIndexReader,
    postings::{DecodedPosting, PostingsReader},
};
use anyhow::Result;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ── Scored hit ────────────────────────────────────────────────────────────────

/// One scored result document.
#[derive(Debug, Clone)]
pub struct ScoredHit {
    /// Segment-local document ID.
    pub doc_id: u32,
    /// BM25 relevance score (higher = more relevant).
    pub score: f32,
    /// Optional score breakdown (populated when `explain=true`).
    pub explanation: Option<QueryExplanation>,
}

impl PartialEq for ScoredHit {
    fn eq(&self, other: &Self) -> bool {
        self.doc_id == other.doc_id
    }
}
impl Eq for ScoredHit {}

impl PartialOrd for ScoredHit {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScoredHit {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Higher score first; break ties by doc_id ascending
        other
            .score
            .partial_cmp(&self.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| self.doc_id.cmp(&other.doc_id))
    }
}

// ── Query types ───────────────────────────────────────────────────────────────

/// A single field + term lookup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermQuery {
    pub field: String,
    pub term: String,
    /// Boost multiplier for the score (default = 1.0, matches ES default).
    #[serde(default = "default_boost")]
    pub boost: f32,
}

fn default_boost() -> f32 {
    1.0
}

impl TermQuery {
    pub fn new(field: impl Into<String>, term: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            term: term.into(),
            boost: 1.0,
        }
    }

    pub fn boosted(field: impl Into<String>, term: impl Into<String>, boost: f32) -> Self {
        Self {
            field: field.into(),
            term: term.into(),
            boost,
        }
    }
}

/// An ordered phrase — terms must appear in the given order with adjacent positions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhraseQuery {
    pub field: String,
    /// Pre-analyzed phrase terms in order.
    pub terms: Vec<String>,
    /// Allowed number of intervening positions (slop).
    #[serde(default)]
    pub slop: u32,
    #[serde(default = "default_boost")]
    pub boost: f32,
}

impl PhraseQuery {
    pub fn new(field: impl Into<String>, terms: Vec<String>) -> Self {
        Self {
            field: field.into(),
            terms,
            slop: 0,
            boost: 1.0,
        }
    }
}

/// A prefix query — finds all terms beginning with `prefix` in the FST.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefixQuery {
    pub field: String,
    pub prefix: String,
    #[serde(default = "default_boost")]
    pub boost: f32,
    /// Maximum number of terms to expand (prevents explosion on short prefixes).
    #[serde(default = "default_max_expansions")]
    pub max_expansions: usize,
}

fn default_max_expansions() -> usize {
    50
}

impl PrefixQuery {
    pub fn new(field: impl Into<String>, prefix: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            prefix: prefix.into(),
            boost: 1.0,
            max_expansions: 50,
        }
    }
}

/// Boolean combinator query — mirrors Elasticsearch's `bool` query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoolQuery {
    /// All must clauses must match.
    #[serde(default)]
    pub must: Vec<Query>,
    /// At least `min_should_match` should clauses must match.
    #[serde(default)]
    pub should: Vec<Query>,
    /// No must_not clause must match.
    #[serde(default)]
    pub must_not: Vec<Query>,
    /// Minimum number of `should` clauses that must match (default = 1 when
    /// there are no `must` clauses, 0 otherwise — same as ES).
    pub min_should_match: Option<u32>,
    #[serde(default = "default_boost")]
    pub boost: f32,
}

impl Default for BoolQuery {
    /// NOTE: `boost` must default to **1.0**, not `f32::default()` (0.0).
    /// A derived `Default` zeroed the boost, and because `execute_bool`
    /// multiplies the combined clause score by `boost`, every query built
    /// through `BoolQuery::new()` (multi_match, multi-token match, bool)
    /// returned `_score: 0.0` for all hits on the segment FTS path.
    fn default() -> Self {
        Self {
            must: Vec::new(),
            should: Vec::new(),
            must_not: Vec::new(),
            min_should_match: None,
            boost: 1.0,
        }
    }
}

/// Disjunction-max query — mirrors Elasticsearch's `dis_max`.
///
/// A document's score is the MAXIMUM of its sub-query scores, plus
/// `tie_breaker` × the sum of the remaining matching sub-query scores.
/// `multi_match` with `type: best_fields` (the ES default) lowers to this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisMaxQuery {
    pub queries: Vec<Query>,
    /// Fraction of the non-best sub-scores added on top of the max (ES
    /// default = 0.0, i.e. pure max).
    #[serde(default)]
    pub tie_breaker: f32,
    #[serde(default = "default_boost")]
    pub boost: f32,
}

impl DisMaxQuery {
    pub fn new(queries: Vec<Query>) -> Self {
        Self {
            queries,
            tie_breaker: 0.0,
            boost: 1.0,
        }
    }

    pub fn tie_breaker(mut self, t: f32) -> Self {
        self.tie_breaker = t;
        self
    }

    pub fn boost(mut self, b: f32) -> Self {
        self.boost = b;
        self
    }
}

/// Top-level query enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Query {
    Term(TermQuery),
    Phrase(PhraseQuery),
    Prefix(PrefixQuery),
    Bool(Box<BoolQuery>),
    DisMax(Box<DisMaxQuery>),
    MatchAll,
}

// ── FtsSearcher ───────────────────────────────────────────────────────────────

/// Executes queries against a single segment's FTS index.
pub struct FtsSearcher {
    reader: Arc<FtsIndexReader>,
    /// Kept for future use: query-time analysis (e.g., synonym expansion, normalization).
    #[allow(dead_code)]
    registry: Arc<AnalyzerRegistry>,
}

impl FtsSearcher {
    pub fn new(reader: Arc<FtsIndexReader>, registry: Arc<AnalyzerRegistry>) -> Self {
        Self { reader, registry }
    }

    /// Execute a query and return up to `limit` scored hits, sorted by descending score.
    pub fn search(&self, query: &Query, limit: usize, explain: bool) -> Result<Vec<ScoredHit>> {
        let mut hits = self.execute(query, explain)?;
        hits.sort_unstable();
        hits.truncate(limit);
        Ok(hits)
    }

    fn execute(&self, query: &Query, explain: bool) -> Result<Vec<ScoredHit>> {
        match query {
            Query::Term(tq) => self.execute_term(tq, explain),
            Query::Phrase(pq) => self.execute_phrase(pq, explain),
            Query::Prefix(pq) => self.execute_prefix(pq, explain),
            Query::Bool(bq) => self.execute_bool(bq, explain),
            Query::DisMax(dq) => self.execute_dis_max(dq, explain),
            Query::MatchAll => self.execute_match_all(),
        }
    }

    // ── Term query ────────────────────────────────────────────────────────────

    fn execute_term(&self, tq: &TermQuery, explain: bool) -> Result<Vec<ScoredHit>> {
        let tp = match self.reader.lookup_term(&tq.field, &tq.term) {
            Some(tp) => tp,
            None => return Ok(Vec::new()),
        };

        let scorer = self.make_scorer(&tq.field);
        let post_data = match self.reader.postings_data(&tq.field, &tp) {
            Some(d) => d,
            None => return Ok(Vec::new()),
        };

        let has_positions = self.reader.field_has_positions(&tq.field);
        let mut reader =
            PostingsReader::new_with_positions(post_data, tp.doc_frequency, has_positions);
        let mut hits = Vec::with_capacity(tp.doc_frequency as usize);

        while let Some(posting) = reader.next() {
            let doc_len = self
                .reader
                .field_length(&tq.field, posting.doc_id)
                .unwrap_or(1) as u32;

            let (score, explanation) = if explain {
                let bd = scorer.score_term_explain(
                    &tq.term,
                    tp.doc_frequency as u64,
                    posting.term_freq,
                    doc_len,
                );
                let s = bd.score * tq.boost;
                let boosted_bd = ScoreBreakdown { score: s, ..bd };
                (s, Some(QueryExplanation::new(vec![boosted_bd])))
            } else {
                let s = scorer.score_term(tp.doc_frequency as u64, posting.term_freq, doc_len)
                    * tq.boost;
                (s, None)
            };

            hits.push(ScoredHit {
                doc_id: posting.doc_id,
                score,
                explanation,
            });
        }

        Ok(hits)
    }

    // ── Phrase query ──────────────────────────────────────────────────────────

    fn execute_phrase(&self, pq: &PhraseQuery, explain: bool) -> Result<Vec<ScoredHit>> {
        if pq.terms.is_empty() {
            return Ok(Vec::new());
        }

        if pq.terms.len() == 1 {
            return self.execute_term(
                &TermQuery::boosted(&pq.field, &pq.terms[0], pq.boost),
                explain,
            );
        }

        // Load postings for every term
        let mut term_postings_list: Vec<(Vec<DecodedPosting>, u32)> = Vec::new();
        for term in &pq.terms {
            let tp = match self.reader.lookup_term(&pq.field, term) {
                Some(tp) => tp,
                None => return Ok(Vec::new()), // missing term → no phrase matches
            };
            let post_data = match self.reader.postings_data(&pq.field, &tp) {
                Some(d) => d,
                None => return Ok(Vec::new()),
            };
            // Phrase query on a docs-only field can never match (no
            // positions stored) — bail out early.
            let has_positions = self.reader.field_has_positions(&pq.field);
            if !has_positions {
                return Ok(Vec::new());
            }
            let mut reader =
                PostingsReader::new_with_positions(post_data, tp.doc_frequency, has_positions);
            let mut postings: Vec<DecodedPosting> = Vec::new();
            while let Some(p) = reader.next() {
                postings.push(p);
            }
            term_postings_list.push((postings, tp.doc_frequency));
        }

        // Intersect: start from the rarest term (smallest doc_freq)
        let min_idx = term_postings_list
            .iter()
            .enumerate()
            .min_by_key(|(_, (_, df))| *df)
            .map(|(i, _)| i)
            .unwrap_or(0);

        // Build doc_id sets from all other terms
        let anchor_postings = &term_postings_list[min_idx].0;
        let scorer = self.make_scorer(&pq.field);
        let mut hits = Vec::new();

        // For each doc in the anchor set, verify phrase constraint across all terms
        'doc: for anchor in anchor_postings {
            let doc_id = anchor.doc_id;

            // Collect per-term position lists for this doc
            let mut all_positions: Vec<Vec<u32>> = Vec::with_capacity(pq.terms.len());
            for (term_idx, (postings, _)) in term_postings_list.iter().enumerate() {
                if let Some(p) = postings.iter().find(|p| p.doc_id == doc_id) {
                    // Adjust positions for phrase offset
                    let offset = if term_idx > min_idx {
                        (term_idx - min_idx) as u32
                    } else {
                        0 // will adjust relative to anchor below
                    };
                    let adjusted: Vec<u32> = p
                        .positions
                        .iter()
                        .filter_map(|&pos| pos.checked_sub(offset))
                        .collect();
                    all_positions.push(adjusted);
                } else {
                    continue 'doc; // term not in this doc
                }
            }

            // Check if any start position creates a valid phrase (with slop)
            if !phrase_matches(&all_positions, pq.slop) {
                continue;
            }

            let doc_len = self.reader.field_length(&pq.field, doc_id).unwrap_or(1) as u32;
            // Score as sum of per-term BM25
            let mut total_score = 0.0f32;
            let mut breakdowns = Vec::new();

            for (term_idx, term) in pq.terms.iter().enumerate() {
                let (_, df) = &term_postings_list[term_idx];
                let tf = term_postings_list[term_idx]
                    .0
                    .iter()
                    .find(|p| p.doc_id == doc_id)
                    .map(|p| p.term_freq)
                    .unwrap_or(1);

                if explain {
                    let bd = scorer.score_term_explain(term, *df as u64, tf, doc_len);
                    total_score += bd.score;
                    breakdowns.push(bd);
                } else {
                    total_score += scorer.score_term(*df as u64, tf, doc_len);
                }
            }
            total_score *= pq.boost;

            let explanation = if explain {
                Some(QueryExplanation::new(breakdowns))
            } else {
                None
            };

            hits.push(ScoredHit {
                doc_id,
                score: total_score,
                explanation,
            });
        }

        Ok(hits)
    }

    // ── Prefix query ──────────────────────────────────────────────────────────

    fn execute_prefix(&self, pq: &PrefixQuery, explain: bool) -> Result<Vec<ScoredHit>> {
        // Expand prefix to term list using FST range scan
        let expanded_terms = self.expand_prefix(&pq.field, &pq.prefix, pq.max_expansions);

        if expanded_terms.is_empty() {
            return Ok(Vec::new());
        }

        // Run a term query for each expansion and merge scores
        let mut score_map: std::collections::HashMap<u32, (f32, Option<QueryExplanation>)> =
            std::collections::HashMap::new();

        for term in &expanded_terms {
            let tq = TermQuery::boosted(&pq.field, term, pq.boost);
            let term_hits = self.execute_term(&tq, explain)?;
            for hit in term_hits {
                let entry = score_map.entry(hit.doc_id).or_insert((0.0, None));
                entry.0 += hit.score;
                if explain {
                    if let Some(exp) = hit.explanation {
                        if let Some(ref mut existing) = entry.1 {
                            existing.term_breakdowns.extend(exp.term_breakdowns);
                            existing.total_score = entry.0;
                        } else {
                            entry.1 = Some(exp);
                        }
                    }
                }
            }
        }

        Ok(score_map
            .into_iter()
            .map(|(doc_id, (score, explanation))| ScoredHit {
                doc_id,
                score,
                explanation,
            })
            .collect())
    }

    fn expand_prefix(&self, field: &str, prefix: &str, max: usize) -> Vec<String> {
        // Use the reader's all_terms which does an FST stream scan
        let all_terms = self.reader.all_terms(field);
        all_terms
            .into_iter()
            .filter(|t| t.starts_with(prefix))
            .take(max)
            .collect()
    }

    // ── Bool query ────────────────────────────────────────────────────────────

    fn execute_bool(&self, bq: &BoolQuery, explain: bool) -> Result<Vec<ScoredHit>> {
        // Determine effective min_should_match
        let min_should = bq.min_should_match.unwrap_or(if bq.must.is_empty() {
            if bq.should.is_empty() {
                0
            } else {
                1
            }
        } else {
            0
        });

        // Execute all sub-queries
        let must_hits: Vec<Vec<ScoredHit>> = bq
            .must
            .iter()
            .map(|q| self.execute(q, explain))
            .collect::<Result<_>>()?;

        let should_hits: Vec<Vec<ScoredHit>> = bq
            .should
            .iter()
            .map(|q| self.execute(q, explain))
            .collect::<Result<_>>()?;

        let must_not_hits: Vec<Vec<ScoredHit>> = bq
            .must_not
            .iter()
            .map(|q| self.execute(q, explain))
            .collect::<Result<_>>()?;

        // Build must_not doc_id set
        let must_not_docs: std::collections::HashSet<u32> = must_not_hits
            .iter()
            .flat_map(|hits| hits.iter().map(|h| h.doc_id))
            .collect();

        // Intersect must clauses
        let mut candidate_docs: Option<std::collections::HashSet<u32>> = None;
        let mut score_map: std::collections::HashMap<u32, f32> = std::collections::HashMap::new();

        for must_result in &must_hits {
            let doc_set: std::collections::HashSet<u32> =
                must_result.iter().map(|h| h.doc_id).collect();
            candidate_docs = Some(match candidate_docs {
                None => doc_set,
                Some(prev) => prev.intersection(&doc_set).copied().collect(),
            });
            for hit in must_result {
                *score_map.entry(hit.doc_id).or_insert(0.0) += hit.score;
            }
        }

        // Should clauses: track per-doc match count and scores
        let mut should_match_count: std::collections::HashMap<u32, u32> =
            std::collections::HashMap::new();
        for should_result in &should_hits {
            for hit in should_result {
                *should_match_count.entry(hit.doc_id).or_insert(0) += 1;
                *score_map.entry(hit.doc_id).or_insert(0.0) += hit.score;
            }
        }

        // If no must clauses, candidate set is the union of should docs
        if bq.must.is_empty() {
            candidate_docs = Some(should_match_count.keys().copied().collect());
        }

        let candidates = match candidate_docs {
            Some(c) => c,
            None => {
                if bq.must.is_empty() && bq.should.is_empty() && !bq.must_not.is_empty() {
                    // must_not only — need all docs; treat as empty (no positive scoring)
                    return Ok(Vec::new());
                }
                return Ok(Vec::new());
            }
        };

        let hits = candidates
            .into_iter()
            .filter(|doc_id| !must_not_docs.contains(doc_id))
            .filter(|doc_id| {
                // Check min_should_match
                let matched_should = should_match_count.get(doc_id).copied().unwrap_or(0);
                matched_should >= min_should
            })
            .map(|doc_id| {
                let score = score_map.get(&doc_id).copied().unwrap_or(0.0) * bq.boost;
                ScoredHit {
                    doc_id,
                    score,
                    explanation: None,
                }
            })
            .collect();

        Ok(hits)
    }

    // ── Dis-max query ─────────────────────────────────────────────────────────

    /// ES `dis_max` scoring: docs matching ANY sub-query are candidates;
    /// score = max(sub scores) + tie_breaker × Σ(other matching sub scores),
    /// all multiplied by the query boost.
    fn execute_dis_max(&self, dq: &DisMaxQuery, explain: bool) -> Result<Vec<ScoredHit>> {
        // Per-doc (max, sum) accumulator over sub-query scores.
        let mut acc: std::collections::HashMap<u32, (f32, f32)> = std::collections::HashMap::new();
        for sub in &dq.queries {
            for hit in self.execute(sub, explain)? {
                let e = acc.entry(hit.doc_id).or_insert((f32::NEG_INFINITY, 0.0));
                if hit.score > e.0 {
                    e.0 = hit.score;
                }
                e.1 += hit.score;
            }
        }
        Ok(acc
            .into_iter()
            .map(|(doc_id, (max, sum))| {
                let score = (max + dq.tie_breaker * (sum - max)) * dq.boost;
                ScoredHit {
                    doc_id,
                    score,
                    explanation: None,
                }
            })
            .collect())
    }

    // ── Match all ─────────────────────────────────────────────────────────────

    fn execute_match_all(&self) -> Result<Vec<ScoredHit>> {
        // Return all docs with uniform score 1.0
        // In practice, the caller should maintain a doc count; we signal "all" with empty set
        // and let the segment scan handle it. For now return empty — callers layer this above.
        Ok(Vec::new())
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_scorer(&self, field: &str) -> Bm25Scorer {
        self.reader
            .field_stats(field)
            .map(|stats| stats.to_scorer())
            .unwrap_or_else(|| Bm25Scorer::new(1.0, 1))
    }
}

// ── Multi-segment parallel search ─────────────────────────────────────────────

/// Search across multiple segment searchers in parallel via rayon.
///
/// Results are merged and re-sorted. `limit` is applied after merge.
pub fn search_segments(
    searchers: &[FtsSearcher],
    query: &Query,
    limit: usize,
    explain: bool,
) -> Result<Vec<ScoredHit>> {
    let results: Vec<Result<Vec<ScoredHit>>> = searchers
        .par_iter()
        .map(|s| s.search(query, limit, explain))
        .collect();

    let mut merged: Vec<ScoredHit> = Vec::new();
    for result in results {
        merged.extend(result?);
    }

    merged.sort_unstable();
    merged.truncate(limit);
    Ok(merged)
}

// ── Phrase matching helper ────────────────────────────────────────────────────

/// Returns `true` if the per-term adjusted position lists contain a common
/// position (meaning a phrase start where all terms follow in order within `slop`).
fn phrase_matches(all_positions: &[Vec<u32>], slop: u32) -> bool {
    if all_positions.is_empty() {
        return false;
    }
    if all_positions.len() == 1 {
        return !all_positions[0].is_empty();
    }

    // For each start position of the first term, check if subsequent terms
    // have a matching position within slop.
    'outer: for &start_pos in &all_positions[0] {
        let mut current_pos = start_pos;
        for positions in &all_positions[1..] {
            // Find the first position in `positions` that is within [current_pos, current_pos + slop + 1]
            let found = positions
                .iter()
                .any(|&p| p >= current_pos && p <= current_pos + slop + 1);
            if !found {
                continue 'outer;
            }
            // Advance current_pos to the matched position
            if let Some(&next_pos) = positions
                .iter()
                .find(|&&p| p >= current_pos && p <= current_pos + slop + 1)
            {
                current_pos = next_pos;
            }
        }
        return true;
    }

    false
}

// ── Query builder helpers ─────────────────────────────────────────────────────

impl BoolQuery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn must(mut self, q: Query) -> Self {
        self.must.push(q);
        self
    }

    pub fn should(mut self, q: Query) -> Self {
        self.should.push(q);
        self
    }

    pub fn must_not(mut self, q: Query) -> Self {
        self.must_not.push(q);
        self
    }

    pub fn min_should_match(mut self, n: u32) -> Self {
        self.min_should_match = Some(n);
        self
    }

    pub fn boost(mut self, b: f32) -> Self {
        self.boost = b;
        self
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        analyzer::AnalyzerRegistry,
        index::{FieldIndexConfig, FtsIndexReader, FtsIndexWriter},
    };
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn setup_searcher(dir: &std::path::Path) -> FtsSearcher {
        let registry = Arc::new(AnalyzerRegistry::default());

        let mut writer = FtsIndexWriter::new(dir, "seg0", Arc::clone(&registry));

        // Use whitespace analyzer to avoid stemming surprises in tests
        let cfg = FieldIndexConfig {
            analyzer: "whitespace".to_owned(),
            ..Default::default()
        };
        writer.configure_field("body", cfg);

        let docs = [
            "the quick brown fox jumps over the lazy dog",
            "quick brown fox",
            "the lazy dog slept",
            "rust is fast and memory safe",
            "fox fox fox triple fox count",
        ];

        for (i, text) in docs.iter().enumerate() {
            let fields: HashMap<String, String> = [("body".to_owned(), text.to_string())]
                .into_iter()
                .collect();
            writer.add_document(i as u32, &fields);
        }
        writer.finish().unwrap();

        let reader = Arc::new(FtsIndexReader::open(dir, "seg0", &["body"]).unwrap());
        FtsSearcher::new(reader, registry)
    }

    #[test]
    fn term_query_finds_docs() {
        let dir = TempDir::new().unwrap();
        let searcher = setup_searcher(dir.path());

        let q = Query::Term(TermQuery::new("body", "fox"));
        let hits = searcher.search(&q, 10, false).unwrap();

        // "fox" appears in docs 0, 1, 4
        assert!(
            hits.len() >= 2,
            "expected at least 2 hits for 'fox', got {}",
            hits.len()
        );
        let doc_ids: Vec<u32> = hits.iter().map(|h| h.doc_id).collect();
        assert!(doc_ids.contains(&1), "doc 1 should match");
        assert!(doc_ids.contains(&4), "doc 4 should match (triple fox)");
    }

    #[test]
    fn term_query_scoring_favors_high_tf() {
        let dir = TempDir::new().unwrap();
        let searcher = setup_searcher(dir.path());

        let q = Query::Term(TermQuery::new("body", "fox"));
        let hits = searcher.search(&q, 10, false).unwrap();

        // Doc 4 has "fox fox fox triple fox count" — high TF should score highest
        let doc4_score = hits.iter().find(|h| h.doc_id == 4).map(|h| h.score);
        let doc1_score = hits.iter().find(|h| h.doc_id == 1).map(|h| h.score);
        if let (Some(s4), Some(s1)) = (doc4_score, doc1_score) {
            assert!(
                s4 > s1,
                "doc4 (TF=4) should outscore doc1 (TF=1): {} vs {}",
                s4,
                s1
            );
        }
    }

    #[test]
    fn term_query_with_explain() {
        let dir = TempDir::new().unwrap();
        let searcher = setup_searcher(dir.path());

        let q = Query::Term(TermQuery::new("body", "fox"));
        let hits = searcher.search(&q, 10, true).unwrap();

        for hit in &hits {
            assert!(
                hit.explanation.is_some(),
                "explain mode should populate explanation"
            );
            let exp = hit.explanation.as_ref().unwrap();
            assert!(!exp.term_breakdowns.is_empty());
            assert!(exp.total_score > 0.0);
        }
    }

    #[test]
    fn prefix_query_expands_terms() {
        let dir = TempDir::new().unwrap();
        let searcher = setup_searcher(dir.path());

        // "la" should match "lazy" (and "last" if present)
        let q = Query::Prefix(PrefixQuery::new("body", "la"));
        let hits = searcher.search(&q, 10, false).unwrap();

        // "lazy" appears in docs 0 and 2
        assert!(
            !hits.is_empty(),
            "prefix 'la' should match docs containing 'lazy'"
        );
    }

    #[test]
    fn bool_query_must_intersection() {
        let dir = TempDir::new().unwrap();
        let searcher = setup_searcher(dir.path());

        // Must contain both "fox" and "quick"
        let q = Query::Bool(Box::new(
            BoolQuery::new()
                .must(Query::Term(TermQuery::new("body", "fox")))
                .must(Query::Term(TermQuery::new("body", "quick"))),
        ));
        let hits = searcher.search(&q, 10, false).unwrap();

        // Docs 0 and 1 contain both "fox" and "quick"
        let doc_ids: Vec<u32> = hits.iter().map(|h| h.doc_id).collect();
        assert!(
            doc_ids.contains(&0),
            "doc0 should match must(fox AND quick)"
        );
        assert!(
            doc_ids.contains(&1),
            "doc1 should match must(fox AND quick)"
        );
        // Doc 4 has fox but not quick
        assert!(!doc_ids.contains(&4), "doc4 should NOT match (no 'quick')");
    }

    #[test]
    fn bool_query_should_scores_are_nonzero() {
        // Regression: BoolQuery's derived Default gave boost = 0.0, so every
        // bool-shaped query (multi_match, multi-token match) returned
        // _score 0.0 for all hits. The default boost must be 1.0, and a
        // single-should bool must score identically to the bare term query.
        let dir = TempDir::new().unwrap();
        let searcher = setup_searcher(dir.path());

        let term_hits = searcher
            .search(&Query::Term(TermQuery::new("body", "fox")), 10, false)
            .unwrap();
        let bool_hits = searcher
            .search(
                &Query::Bool(Box::new(
                    BoolQuery::new().should(Query::Term(TermQuery::new("body", "fox"))),
                )),
                10,
                false,
            )
            .unwrap();

        assert_eq!(term_hits.len(), bool_hits.len());
        for th in &term_hits {
            assert!(th.score > 0.0, "term score must be positive");
            let bh = bool_hits.iter().find(|h| h.doc_id == th.doc_id).unwrap();
            assert!(
                (bh.score - th.score).abs() < 1e-6,
                "bool(should:[term]) score {} must equal term score {} for doc {}",
                bh.score,
                th.score,
                th.doc_id
            );
        }
    }

    #[test]
    fn dis_max_takes_max_of_subquery_scores() {
        let dir = TempDir::new().unwrap();
        let registry = Arc::new(AnalyzerRegistry::default());
        let mut writer = FtsIndexWriter::new(dir.path(), "seg0", Arc::clone(&registry));
        let cfg = FieldIndexConfig {
            analyzer: "whitespace".to_owned(),
            ..Default::default()
        };
        writer.configure_field("title", cfg.clone());
        writer.configure_field("body", cfg);

        // doc 0: "golf" in both fields; doc 1: only title; doc 2: only body.
        let docs: Vec<(&str, &str)> = vec![
            ("golf highlights", "golf swing tips and golf drills"),
            ("golf weekly", "tennis recap"),
            ("morning news", "golf scores from sunday"),
        ];
        for (i, (title, body)) in docs.iter().enumerate() {
            let fields: HashMap<String, String> = [
                ("title".to_owned(), title.to_string()),
                ("body".to_owned(), body.to_string()),
            ]
            .into_iter()
            .collect();
            writer.add_document(i as u32, &fields);
        }
        writer.finish().unwrap();
        let reader =
            Arc::new(FtsIndexReader::open(dir.path(), "seg0", &["title", "body"]).unwrap());
        let searcher = FtsSearcher::new(reader, registry);

        let title_q = Query::Term(TermQuery::new("title", "golf"));
        let body_q = Query::Term(TermQuery::new("body", "golf"));
        let title_hits = searcher.search(&title_q, 10, false).unwrap();
        let body_hits = searcher.search(&body_q, 10, false).unwrap();

        let dm = Query::DisMax(Box::new(DisMaxQuery::new(vec![
            title_q.clone(),
            body_q.clone(),
        ])));
        let dm_hits = searcher.search(&dm, 10, false).unwrap();

        // Union of matching docs: all three.
        assert_eq!(
            dm_hits.len(),
            3,
            "dis_max must return the union of sub-query docs"
        );

        for hit in &dm_hits {
            let ts = title_hits
                .iter()
                .find(|h| h.doc_id == hit.doc_id)
                .map(|h| h.score);
            let bs = body_hits
                .iter()
                .find(|h| h.doc_id == hit.doc_id)
                .map(|h| h.score);
            let expected = ts.unwrap_or(0.0).max(bs.unwrap_or(0.0));
            assert!(hit.score > 0.0, "dis_max hit must have positive score");
            assert!(
                (hit.score - expected).abs() < 1e-6,
                "doc {}: dis_max score {} != max(field scores) {}",
                hit.doc_id,
                hit.score,
                expected
            );
        }

        // tie_breaker adds a fraction of the non-best scores on top of the max.
        let dm_tb = Query::DisMax(Box::new(
            DisMaxQuery::new(vec![title_q, body_q]).tie_breaker(0.5),
        ));
        let tb_hits = searcher.search(&dm_tb, 10, false).unwrap();
        let both = tb_hits.iter().find(|h| h.doc_id == 0).unwrap();
        let ts = title_hits.iter().find(|h| h.doc_id == 0).unwrap().score;
        let bs = body_hits.iter().find(|h| h.doc_id == 0).unwrap().score;
        let expected = ts.max(bs) + 0.5 * (ts + bs - ts.max(bs));
        assert!(
            (both.score - expected).abs() < 1e-6,
            "tie_breaker score {} != expected {}",
            both.score,
            expected
        );
    }

    #[test]
    fn bool_query_must_not_excludes() {
        let dir = TempDir::new().unwrap();
        let searcher = setup_searcher(dir.path());

        let q = Query::Bool(Box::new(
            BoolQuery::new()
                .must(Query::Term(TermQuery::new("body", "fox")))
                .must_not(Query::Term(TermQuery::new("body", "quick"))),
        ));
        let hits = searcher.search(&q, 10, false).unwrap();

        // Doc 4 has "fox" but not "quick"; docs 0 and 1 have both
        let doc_ids: Vec<u32> = hits.iter().map(|h| h.doc_id).collect();
        assert!(doc_ids.contains(&4), "doc4 should match (fox, no quick)");
        assert!(!doc_ids.contains(&0), "doc0 excluded by must_not(quick)");
        assert!(!doc_ids.contains(&1), "doc1 excluded by must_not(quick)");
    }

    #[test]
    fn bool_query_should_min_match() {
        let dir = TempDir::new().unwrap();
        let searcher = setup_searcher(dir.path());

        // Must match at least 2 of: fox, quick, lazy
        let q = Query::Bool(Box::new(
            BoolQuery::new()
                .should(Query::Term(TermQuery::new("body", "fox")))
                .should(Query::Term(TermQuery::new("body", "quick")))
                .should(Query::Term(TermQuery::new("body", "lazy")))
                .min_should_match(2),
        ));
        let hits = searcher.search(&q, 10, false).unwrap();

        // Doc 0: has all 3 → matches
        // Doc 1: has fox + quick → matches
        // Doc 2: has lazy only → does not match
        let doc_ids: Vec<u32> = hits.iter().map(|h| h.doc_id).collect();
        assert!(doc_ids.contains(&0), "doc0 should match (fox+quick+lazy)");
        assert!(doc_ids.contains(&1), "doc1 should match (fox+quick)");
        assert!(!doc_ids.contains(&2), "doc2 should not match (lazy only)");
    }

    #[test]
    fn phrase_query_matches_adjacent_terms() {
        let dir = TempDir::new().unwrap();
        let searcher = setup_searcher(dir.path());

        let q = Query::Phrase(PhraseQuery::new(
            "body",
            vec!["quick".to_owned(), "brown".to_owned()],
        ));
        let hits = searcher.search(&q, 10, false).unwrap();

        // "quick brown" appears in docs 0 and 1
        assert!(!hits.is_empty(), "phrase 'quick brown' should match");
        let doc_ids: Vec<u32> = hits.iter().map(|h| h.doc_id).collect();
        assert!(doc_ids.contains(&0) || doc_ids.contains(&1));
    }

    #[test]
    fn phrase_query_nonexistent_returns_empty() {
        let dir = TempDir::new().unwrap();
        let searcher = setup_searcher(dir.path());

        // "brown quick" is never in order in any doc
        let q = Query::Phrase(PhraseQuery::new(
            "body",
            vec!["brown".to_owned(), "quick".to_owned()],
        ));
        let hits = searcher.search(&q, 10, false).unwrap();
        assert!(hits.is_empty(), "reversed phrase should not match");
    }

    #[test]
    fn hits_sorted_by_descending_score() {
        let dir = TempDir::new().unwrap();
        let searcher = setup_searcher(dir.path());

        let q = Query::Term(TermQuery::new("body", "fox"));
        let hits = searcher.search(&q, 10, false).unwrap();

        let scores: Vec<f32> = hits.iter().map(|h| h.score).collect();
        for window in scores.windows(2) {
            assert!(
                window[0] >= window[1],
                "hits must be in descending score order: {:?}",
                scores
            );
        }
    }
}
