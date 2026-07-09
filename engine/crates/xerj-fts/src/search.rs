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
    postings::PostingsReader,
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

// ── Bounded top-N collector ─────────────────────────────────────────────────

/// Bounded min-by-quality top-N collector.
///
/// Retains only the best `cap` hits seen so far while counting **every** hit
/// pushed (the exact match total).  `push` is O(log cap); memory is O(cap)
/// regardless of how many matches the query produces — this is what turns the
/// size>0 scored FTS path from O(N log N) (build + full sort) into O(N log cap)
/// with O(cap) memory.
///
/// ## Ordering identity (why the output is byte-identical to `sort` + `truncate`)
///
/// `ScoredHit`'s `Ord` makes the BEST hit compare **`Less`** (higher score
/// first, doc_id ascending on ties).  A [`BinaryHeap`](std::collections::BinaryHeap)
/// is a MAX-heap, so `peek()` returns the GREATEST element by `Ord` = the
/// **worst** retained hit.  A new hit is admitted iff it is strictly better
/// (`hit < worst`).  Because `doc_id` is unique, `Ord` is a total order with no
/// real ties, so the retained set is exactly the `cap` best hits, and
/// [`into_sorted_vec`](std::collections::BinaryHeap::into_sorted_vec) yields
/// them in ascending `Ord` order = best-first — identical to the legacy
/// `execute()` → `sort_unstable()` → `truncate(cap)`.
pub struct TopN {
    heap: std::collections::BinaryHeap<ScoredHit>,
    cap: usize,
    total: u64,
}

impl TopN {
    pub fn new(cap: usize) -> Self {
        Self {
            // Bound the pre-allocation: `cap` is a page limit (hundreds to a
            // few thousand), but guard against a pathological caller value.
            heap: std::collections::BinaryHeap::with_capacity(cap.min(4096)),
            cap,
            total: 0,
        }
    }

    /// Offer one hit: always counted toward `total`; retained only if it ranks
    /// among the best `cap` seen so far.
    #[inline]
    pub fn push(&mut self, hit: ScoredHit) {
        self.total += 1;
        if self.cap == 0 {
            return;
        }
        if self.heap.len() < self.cap {
            self.heap.push(hit);
        } else if let Some(mut worst) = self.heap.peek_mut() {
            // `worst` is the heap max = the worst retained hit.  Replace it
            // only when the incoming hit is strictly better (`Ord::Less`).
            // Dropping the `PeekMut` sifts the new element back into place.
            if hit < *worst {
                *worst = hit;
            }
        }
    }

    /// Exact number of hits pushed so far (independent of `cap`).
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Consume the collector: `(best-first sorted hits, exact total)`.
    pub fn finish(self) -> (Vec<ScoredHit>, u64) {
        let total = self.total;
        (self.heap.into_sorted_vec(), total)
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
    /// When `true`, every matching doc scores a flat `boost` (ES rewrites
    /// `prefix` to a `constant_score` query).  When `false` (default, keyword
    /// path), the per-expansion BM25 scores are summed.
    #[serde(default)]
    pub constant_score: bool,
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
            constant_score: false,
        }
    }
}

/// A wildcard query — expands the field's FST term dictionary to every term
/// matching `pattern` (`*` = zero-or-more chars, `?` = exactly one), then
/// scores each as a term.  Matching is **case-insensitive** and applied to the
/// whole indexed term AND its non-alphanumeric-split sub-tokens, byte-identical
/// to the engine's `doc_matches_query` wildcard predicate — so routing a
/// keyword-field wildcard through the FST returns exactly the same hit set as
/// the per-doc stored scan, only far faster (term dictionary ≪ documents).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WildcardQuery {
    pub field: String,
    pub pattern: String,
    #[serde(default = "default_boost")]
    pub boost: f32,
    /// When `true` (the default, used for keyword fields) the pattern and each
    /// indexed term are case-folded before matching, and the whole term OR any
    /// non-alphanumeric sub-token may match — byte-identical to the engine's
    /// stored-scan wildcard predicate.  When `false` (analyzed **text** fields)
    /// the raw pattern is matched CASE-SENSITIVELY against the whole indexed
    /// term.  ES indexes text terms lowercased but does NOT analyze the wildcard
    /// pattern, so an uppercase pattern matches nothing — exactly what
    /// case-sensitive matching against the already-lowercased term dictionary
    /// reproduces.
    #[serde(default = "default_case_insensitive")]
    pub case_insensitive: bool,
    /// When `true`, every matching doc scores a flat `boost` (ES rewrites
    /// `wildcard` to a `constant_score` query).  When `false` (default, keyword
    /// path), the per-matching-term BM25 scores are summed.
    #[serde(default)]
    pub constant_score: bool,
}

fn default_case_insensitive() -> bool {
    true
}

impl WildcardQuery {
    pub fn new(field: impl Into<String>, pattern: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            pattern: pattern.into(),
            boost: 1.0,
            case_insensitive: true,
            constant_score: false,
        }
    }
}

/// A fuzzy query — expands the field's FST term dictionary to every term within
/// `max_edits` Damerau-Levenshtein distance of `value`, then scores each as a
/// term.  Distance is computed **case-insensitively** over the whole indexed
/// term AND its sub-tokens, byte-identical to the engine's `doc_matches_query`
/// fuzzy predicate.  `max_edits` is precomputed by the caller from the ES
/// `fuzziness` (AUTO resolves off the query term length), so the searcher needs
/// no `Fuzziness` type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuzzyQuery {
    pub field: String,
    pub value: String,
    pub max_edits: usize,
    #[serde(default = "default_boost")]
    pub boost: f32,
    /// See [`WildcardQuery::case_insensitive`].  `true` (default) folds both
    /// sides and also compares sub-tokens (keyword fields).  `false` compares
    /// the raw query value against the whole indexed term case-sensitively —
    /// ES fuzzy on a **text** field measures edit distance against the
    /// lowercased term dictionary without lowercasing the query term.
    #[serde(default = "default_case_insensitive")]
    pub case_insensitive: bool,
}

impl FuzzyQuery {
    pub fn new(field: impl Into<String>, value: impl Into<String>, max_edits: usize) -> Self {
        Self {
            field: field.into(),
            value: value.into(),
            max_edits,
            boost: 1.0,
            case_insensitive: true,
        }
    }
}

/// A phrase-prefix query — mirrors Elasticsearch's `match_phrase_prefix`.
///
/// The leading `terms[..n-1]` must appear as an ordered adjacent phrase; the
/// LAST element is treated as a **prefix** that expands against the field's
/// analyzed term dictionary (bounded by `max_expansions`, in FST/lexicographic
/// order — the same order ES takes its first `max_expansions` terms).  A doc
/// matches when the head phrase is immediately followed by any expansion term.
/// `terms` are already analyzed by the caller (standard analyzer → lowercased),
/// so the prefix expansion is a plain case-sensitive `starts_with` against the
/// (lowercased) term dictionary — byte-identical to ES, whose analyzer
/// lowercases the query but does not re-case the indexed terms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhrasePrefixQuery {
    pub field: String,
    pub terms: Vec<String>,
    #[serde(default = "default_max_expansions")]
    pub max_expansions: usize,
    #[serde(default = "default_boost")]
    pub boost: f32,
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
    PhrasePrefix(PhrasePrefixQuery),
    Prefix(PrefixQuery),
    Wildcard(WildcardQuery),
    Fuzzy(FuzzyQuery),
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
        Ok(self.search_bounded(query, limit, explain)?.0)
    }

    /// Execute a query returning `(top-`cap` hits best-first, EXACT match total)`.
    ///
    /// The `total` is the full segment match count regardless of `cap`, so the
    /// caller's `hits.total` stays exact even when only the best `cap` sources
    /// are materialised.
    ///
    /// - `cap == usize::MAX` keeps the **legacy** full-set path (`execute()` +
    ///   `sort_unstable()`).  Callers that need the complete match set — an
    ///   active field sort (which narrows *all* matching doc_ids to sort
    ///   candidates), delete/dup-aware materialisation, or a count that must
    ///   not be page-capped — pass this and get behaviour byte-identical to the
    ///   historical `search(usize::MAX)`.
    /// - Any smaller `cap` streams into a bounded [`TopN`]: O(N log cap) work,
    ///   O(cap) memory, no O(N log N) sort.  Leaf `Term` queries stream each
    ///   scored posting straight into the heap (no intermediate `Vec`); the
    ///   compound arms (`Bool`/`DisMax`/`Prefix`/`Phrase`) build their
    ///   de-duplicated hit set once via `execute()` and drain it — still
    ///   O(matches) but with the sort removed.
    pub fn search_bounded(
        &self,
        query: &Query,
        cap: usize,
        explain: bool,
    ) -> Result<(Vec<ScoredHit>, u64)> {
        if cap == usize::MAX {
            let mut hits = self.execute(query, explain)?;
            let total = hits.len() as u64;
            hits.sort_unstable();
            return Ok((hits, total));
        }

        let mut top = TopN::new(cap);
        match query {
            // Streaming leaf: push each scored posting directly into the heap.
            Query::Term(tq) => self.collect_term(tq, explain, &mut top)?,
            // Compound / positional arms: reuse the exact hit-set builder, then
            // drain into the bounded heap (the sort is what we shed here).
            _ => {
                for hit in self.execute(query, explain)? {
                    top.push(hit);
                }
            }
        }
        Ok(top.finish())
    }

    fn execute(&self, query: &Query, explain: bool) -> Result<Vec<ScoredHit>> {
        match query {
            Query::Term(tq) => self.execute_term(tq, explain),
            Query::Phrase(pq) => self.execute_phrase(pq, explain),
            Query::PhrasePrefix(pq) => self.execute_phrase_prefix(pq, explain),
            Query::Prefix(pq) => self.execute_prefix(pq, explain),
            Query::Wildcard(wq) => self.execute_wildcard(wq, explain),
            Query::Fuzzy(fq) => self.execute_fuzzy(fq, explain),
            Query::Bool(bq) => self.execute_bool(bq, explain),
            Query::DisMax(dq) => self.execute_dis_max(dq, explain),
            Query::MatchAll => self.execute_match_all(),
        }
    }

    // ── Term query ────────────────────────────────────────────────────────────

    fn execute_term(&self, tq: &TermQuery, explain: bool) -> Result<Vec<ScoredHit>> {
        let mut hits = Vec::new();
        self.scan_term(tq, explain, |h| hits.push(h))?;
        Ok(hits)
    }

    /// Streaming variant of [`Self::execute_term`] for the bounded search path:
    /// each scored posting is offered straight to the `TopN` heap, so a keyword
    /// term matching tens of thousands of docs never materialises the full hit
    /// `Vec` for a size:10 request.
    fn collect_term(&self, tq: &TermQuery, explain: bool, top: &mut TopN) -> Result<()> {
        self.scan_term(tq, explain, |h| top.push(h))
    }

    /// Shared scoring scan used by both [`Self::execute_term`] (Vec) and
    /// [`Self::collect_term`] (bounded heap).  Emits one `ScoredHit` per
    /// matching posting in ascending doc_id order.
    fn scan_term<F: FnMut(ScoredHit)>(
        &self,
        tq: &TermQuery,
        explain: bool,
        mut emit: F,
    ) -> Result<()> {
        let tp = match self.reader.lookup_term(&tq.field, &tq.term) {
            Some(tp) => tp,
            None => return Ok(()),
        };

        let scorer = self.make_scorer(&tq.field);
        let post_data = match self.reader.postings_data(&tq.field, &tp) {
            Some(d) => d,
            None => return Ok(()),
        };

        let has_positions = self.reader.field_has_positions(&tq.field);
        let mut reader =
            PostingsReader::new_with_positions(post_data, tp.doc_frequency, has_positions);

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

            emit(ScoredHit {
                doc_id: posting.doc_id,
                score,
                explanation,
            });
        }

        Ok(())
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

        // Phrase matching needs positions.  A docs-only field (keyword,
        // numeric, ip) can never satisfy a multi-term phrase.
        if !self.reader.field_has_positions(&pq.field) {
            return Ok(Vec::new());
        }

        // Load postings for every term into a per-term
        // `doc_id → (term_freq, positions)` map.  This is O(Σ df) once and
        // turns the per-anchor-doc term lookup into O(1) — the previous
        // implementation `.find()`-scanned each other term's FULL posting list
        // for every anchor doc (O(anchor_df × other_df), quadratic on common
        // terms like a non-stop-worded "the").
        let mut term_maps: Vec<(std::collections::HashMap<u32, (u32, Vec<u32>)>, u32)> =
            Vec::with_capacity(pq.terms.len());
        for term in &pq.terms {
            let tp = match self.reader.lookup_term(&pq.field, term) {
                Some(tp) => tp,
                None => return Ok(Vec::new()), // missing term → no phrase matches
            };
            let post_data = match self.reader.postings_data(&pq.field, &tp) {
                Some(d) => d,
                None => return Ok(Vec::new()),
            };
            let mut reader =
                PostingsReader::new_with_positions(post_data, tp.doc_frequency, true);
            let mut map: std::collections::HashMap<u32, (u32, Vec<u32>)> =
                std::collections::HashMap::with_capacity(tp.doc_frequency as usize);
            while let Some(p) = reader.next() {
                map.insert(p.doc_id, (p.term_freq, p.positions));
            }
            term_maps.push((map, tp.doc_frequency));
        }

        // Anchor on the rarest term (smallest doc_freq) so the outer loop is
        // as short as possible; every candidate doc must contain it.
        let min_idx = term_maps
            .iter()
            .enumerate()
            .min_by_key(|(_, (_, df))| *df)
            .map(|(i, _)| i)
            .unwrap_or(0);

        let scorer = self.make_scorer(&pq.field);
        let mut hits = Vec::new();

        'doc: for (&doc_id, _) in &term_maps[min_idx].0 {
            // Gather each term's positions for this doc, in TERM ORDER (raw —
            // no offset games; the matcher below handles adjacency + slop).
            let mut all_positions: Vec<&Vec<u32>> = Vec::with_capacity(pq.terms.len());
            for (map, _) in &term_maps {
                match map.get(&doc_id) {
                    Some((_, positions)) => all_positions.push(positions),
                    None => continue 'doc, // term absent from this doc
                }
            }

            if !phrase_positions_match(&all_positions, pq.slop) {
                continue;
            }

            let doc_len = self.reader.field_length(&pq.field, doc_id).unwrap_or(1) as u32;
            let mut total_score = 0.0f32;
            let mut breakdowns = Vec::new();
            for (term_idx, term) in pq.terms.iter().enumerate() {
                let (map, df) = &term_maps[term_idx];
                let tf = map.get(&doc_id).map(|(f, _)| *f).unwrap_or(1);
                if explain {
                    let bd = scorer.score_term_explain(term, *df as u64, tf, doc_len);
                    total_score += bd.score;
                    breakdowns.push(bd);
                } else {
                    total_score += scorer.score_term(*df as u64, tf, doc_len);
                }
            }
            total_score *= pq.boost;

            hits.push(ScoredHit {
                doc_id,
                score: total_score,
                explanation: if explain {
                    Some(QueryExplanation::new(breakdowns))
                } else {
                    None
                },
            });
        }

        Ok(hits)
    }

    // ── Phrase-prefix query ─────────────────────────────────────────────────────

    /// ES `match_phrase_prefix`: the head `terms[..n-1]` form an ordered phrase
    /// and the last term is a prefix expanded against the analyzed term
    /// dictionary.  A doc matches when any expansion term immediately follows a
    /// head-phrase occurrence.  Implemented as a union of concrete phrase
    /// queries (one per expansion) — bounded by `max_expansions`, which caps the
    /// number of prefix terms exactly as ES does.
    fn execute_phrase_prefix(
        &self,
        ppq: &PhrasePrefixQuery,
        explain: bool,
    ) -> Result<Vec<ScoredHit>> {
        if ppq.terms.is_empty() {
            return Ok(Vec::new());
        }
        let last = ppq.terms.last().expect("non-empty");
        // Expand the trailing prefix against the field's term dictionary in FST
        // (lexicographic) order — matches ES's first-`max_expansions` selection.
        let expansions = self.expand_prefix(&ppq.field, last, ppq.max_expansions);
        if expansions.is_empty() {
            return Ok(Vec::new());
        }

        // Single-token phrase_prefix == prefix query (scored, not constant —
        // ES match_phrase_prefix is a real scoring query).
        if ppq.terms.len() == 1 {
            return self.score_expanded_terms(&ppq.field, &expansions, ppq.boost, explain, false);
        }

        let head = &ppq.terms[..ppq.terms.len() - 1];
        // Per-doc best score across expansions (ES takes the max-scoring
        // expansion for a doc; the exact hit SET — what `hits.total` counts — is
        // the union, which the accumulator captures).
        let mut score_map: std::collections::HashMap<u32, (f32, Option<QueryExplanation>)> =
            std::collections::HashMap::new();
        for exp in &expansions {
            let mut terms: Vec<String> = head.to_vec();
            terms.push(exp.clone());
            let pq = PhraseQuery {
                field: ppq.field.clone(),
                terms,
                slop: 0,
                boost: ppq.boost,
            };
            for hit in self.execute_phrase(&pq, explain)? {
                let entry = score_map
                    .entry(hit.doc_id)
                    .or_insert((f32::NEG_INFINITY, None));
                if hit.score > entry.0 {
                    entry.0 = hit.score;
                    entry.1 = hit.explanation;
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

    // ── Prefix query ──────────────────────────────────────────────────────────

    fn execute_prefix(&self, pq: &PrefixQuery, explain: bool) -> Result<Vec<ScoredHit>> {
        // Expand prefix to term list using FST range scan
        let expanded_terms = self.expand_prefix(&pq.field, &pq.prefix, pq.max_expansions);
        self.score_expanded_terms(
            &pq.field,
            &expanded_terms,
            pq.boost,
            explain,
            pq.constant_score,
        )
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

    // ── Wildcard query ──────────────────────────────────────────────────────────

    fn execute_wildcard(&self, wq: &WildcardQuery, explain: bool) -> Result<Vec<ScoredHit>> {
        let terms = self.expand_wildcard(&wq.field, &wq.pattern, wq.case_insensitive);
        self.score_expanded_terms(&wq.field, &terms, wq.boost, explain, wq.constant_score)
    }

    /// Enumerate the field's FST term dictionary and keep every term matching
    /// the wildcard pattern.
    ///
    /// - `case_insensitive` (keyword fields): fold both sides and match the
    ///   whole term OR any non-alphanumeric-split sub-token — identical to the
    ///   engine's doc-scan wildcard predicate.
    /// - case-**sensitive** (analyzed text fields): match the RAW pattern
    ///   against the whole indexed term.  Text terms are already lowercased by
    ///   the standard analyzer and ES does not lowercase the pattern, so an
    ///   uppercase pattern correctly matches nothing.
    fn expand_wildcard(&self, field: &str, pattern: &str, case_insensitive: bool) -> Vec<String> {
        if case_insensitive {
            let pat_lc = pattern.to_lowercase();
            self.reader
                .all_terms(field)
                .into_iter()
                .filter(|t| term_matches_wildcard(t, &pat_lc))
                .collect()
        } else {
            self.reader
                .all_terms(field)
                .into_iter()
                .filter(|t| wildcard_match(t, pattern))
                .collect()
        }
    }

    // ── Fuzzy query ─────────────────────────────────────────────────────────────

    fn execute_fuzzy(&self, fq: &FuzzyQuery, explain: bool) -> Result<Vec<ScoredHit>> {
        let terms = self.expand_fuzzy(&fq.field, &fq.value, fq.max_edits, fq.case_insensitive);
        // Fuzzy keeps per-term scoring (ES rewrites fuzzy to a blended-frequency
        // scoring query, NOT constant_score).
        self.score_expanded_terms(&fq.field, &terms, fq.boost, explain, false)
    }

    /// Enumerate the field's FST term dictionary and keep every term within
    /// `max_edits` Damerau-Levenshtein distance of `value`.
    ///
    /// - `case_insensitive` (keyword fields): fold both sides and also compare
    ///   sub-tokens — identical to the engine's doc-scan fuzzy predicate.
    /// - case-**sensitive** (analyzed text fields): measure distance from the
    ///   RAW query value to the whole indexed (lowercased) term, matching ES,
    ///   which does not lowercase the fuzzy query term.
    fn expand_fuzzy(
        &self,
        field: &str,
        value: &str,
        max_edits: usize,
        case_insensitive: bool,
    ) -> Vec<String> {
        if case_insensitive {
            let q_lower = value.to_lowercase();
            self.reader
                .all_terms(field)
                .into_iter()
                .filter(|t| term_matches_fuzzy(t, &q_lower, max_edits))
                .collect()
        } else {
            self.reader
                .all_terms(field)
                .into_iter()
                .filter(|t| levenshtein_distance(t, value) <= max_edits)
                .collect()
        }
    }

    /// Run one term query per expanded term and merge their per-doc scores — the
    /// shared scoring core behind prefix/wildcard/fuzzy multi-term expansion.
    /// (A doc's score is the sum of its matching-term BM25 scores; among
    /// constant-ish keyword matches this is effectively a flat score.)
    fn score_expanded_terms(
        &self,
        field: &str,
        terms: &[String],
        boost: f32,
        explain: bool,
        constant_score: bool,
    ) -> Result<Vec<ScoredHit>> {
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        // Constant-score (ES prefix/wildcard rewrite): every doc that matches
        // ANY expansion term scores a flat `boost`, so we only need the matching
        // doc-id set.  Deduplicate across expansions and emit `boost` once.
        if constant_score {
            let mut docs: std::collections::HashSet<u32> = std::collections::HashSet::new();
            for term in terms {
                let tp = match self.reader.lookup_term(field, term) {
                    Some(tp) => tp,
                    None => continue,
                };
                let post_data = match self.reader.postings_data(field, &tp) {
                    Some(d) => d,
                    None => continue,
                };
                let has_positions = self.reader.field_has_positions(field);
                let mut reader =
                    PostingsReader::new_with_positions(post_data, tp.doc_frequency, has_positions);
                while let Some(p) = reader.next() {
                    docs.insert(p.doc_id);
                }
            }
            return Ok(docs
                .into_iter()
                .map(|doc_id| ScoredHit {
                    doc_id,
                    score: boost,
                    explanation: None,
                })
                .collect());
        }
        let mut score_map: std::collections::HashMap<u32, (f32, Option<QueryExplanation>)> =
            std::collections::HashMap::new();
        for term in terms {
            let tq = TermQuery::boosted(field, term, boost);
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

// ── Wildcard / fuzzy term-matching predicates ─────────────────────────────────
//
// These reproduce, byte-for-byte, the wildcard/fuzzy predicates the engine's
// `doc_matches_query` applies per stored document, so expanding a keyword
// field's FST term dictionary through them yields exactly the same hit set as
// the per-doc stored scan — just far cheaper (terms ≪ documents).  Keep them in
// lock-step with `xerj-engine/src/index.rs::{wildcard_match, levenshtein_distance}`.

/// `true` if `term` (case-folded) matches the already-lowercased `pat_lc`,
/// either as the whole value or as any non-alphanumeric-split sub-token.
fn term_matches_wildcard(term: &str, pat_lc: &str) -> bool {
    let lc = term.to_lowercase();
    if wildcard_match(&lc, pat_lc) {
        return true;
    }
    lc.split(|c: char| !c.is_alphanumeric())
        .any(|tok| !tok.is_empty() && wildcard_match(tok, pat_lc))
}

/// `true` if `term` (case-folded) is within `max_edits` Damerau-Levenshtein
/// distance of the already-lowercased `q_lower`, as the whole value or any
/// sub-token.
fn term_matches_fuzzy(term: &str, q_lower: &str, max_edits: usize) -> bool {
    let s_lower = term.to_lowercase();
    if levenshtein_distance(&s_lower, q_lower) <= max_edits {
        return true;
    }
    s_lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .any(|tok| levenshtein_distance(tok, q_lower) <= max_edits)
}

/// Glob match: `*` = zero-or-more chars, `?` = exactly one.  No case folding —
/// callers fold both sides first.  Verbatim copy of the engine's helper.
fn wildcard_match(text: &str, pattern: &str) -> bool {
    let text: Vec<char> = text.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    wildcard_match_inner(&text, &pattern)
}

fn wildcard_match_inner(text: &[char], pattern: &[char]) -> bool {
    match (text, pattern) {
        (_, []) => text.is_empty(),
        (_, ['*', rest @ ..]) => {
            wildcard_match_inner(text, rest)
                || (!text.is_empty() && wildcard_match_inner(&text[1..], pattern))
        }
        ([], _) => false,
        ([tc, trest @ ..], [pc, prest @ ..]) => {
            (*pc == '?' || tc == pc) && wildcard_match_inner(trest, prest)
        }
    }
}

/// Damerau-Levenshtein edit distance (adjacent transposition = 1 edit), matching
/// ES `fuzzy_transpositions: true`.  Verbatim copy of the engine's helper.
fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    #[allow(clippy::needless_range_loop)]
    for i in 0..=m {
        dp[i][0] = i;
    }
    #[allow(clippy::needless_range_loop)]
    for j in 0..=n {
        dp[0][j] = j;
    }
    for i in 1..=m {
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            let mut best = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
            if i >= 2
                && j >= 2
                && a_chars[i - 1] == b_chars[j - 2]
                && a_chars[i - 2] == b_chars[j - 1]
            {
                best = best.min(dp[i - 2][j - 2] + 1);
            }
            dp[i][j] = best;
        }
    }
    dp[m][n]
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

/// Returns `true` if the per-term **raw** position lists (in TERM ORDER)
/// contain an in-order occurrence of the phrase within `slop`.
///
/// `all_positions[i]` is the ascending list of positions at which the i-th
/// phrase term occurs in one document.
///
/// - `slop == 0`: exact adjacency.  There must be a start `p` such that term i
///   occupies exactly `p + i` for every i (start-position intersection).
/// - `slop > 0`: greedy in-order walk — each next term is matched at the
///   earliest position `> ` the previous match, and the summed gaps between
///   adjacent matched terms must not exceed `slop` (the semantics the engine's
///   stored-scan `MatchPhrase` uses).
fn phrase_positions_match(all_positions: &[&Vec<u32>], slop: u32) -> bool {
    if all_positions.iter().any(|p| p.is_empty()) {
        return false;
    }
    if all_positions.len() == 1 {
        return true;
    }

    if slop == 0 {
        // Exact phrase: term i must sit at start + i.  Probe each candidate
        // start from the first term; membership via binary search (positions
        // are ascending).
        'start: for &start in all_positions[0] {
            for (i, positions) in all_positions.iter().enumerate().skip(1) {
                let want = match start.checked_add(i as u32) {
                    Some(w) => w,
                    None => continue 'start,
                };
                if positions.binary_search(&want).is_err() {
                    continue 'start;
                }
            }
            return true;
        }
        return false;
    }

    // Sloppy phrase: greedy earliest-match walk, accumulating the gaps.
    'outer: for &start_pos in all_positions[0] {
        let mut current_pos = start_pos;
        let mut total_gaps: u32 = 0;
        for positions in &all_positions[1..] {
            // Earliest position strictly after the previous matched term.
            match positions.iter().copied().find(|&p| p > current_pos) {
                Some(next_pos) => {
                    total_gaps += next_pos - current_pos - 1;
                    if total_gaps > slop {
                        continue 'outer;
                    }
                    current_pos = next_pos;
                }
                None => continue 'outer,
            }
        }
        if total_gaps <= slop {
            return true;
        }
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

    /// Regression: the phrase intersection must be correct when the rarest
    /// (anchor) term is NOT the first phrase term.  Doc 0 has
    /// "the quick brown fox …"; "quick"/"brown" are rarer than "the", so the
    /// anchor is a non-first term — the old offset-adjust logic mishandled this.
    #[test]
    fn phrase_query_correct_when_anchor_not_first() {
        let dir = TempDir::new().unwrap();
        let searcher = setup_searcher(dir.path());

        // "the quick" — "the" is the common (first) term, "quick" rarer.
        let hits = searcher
            .search(
                &Query::Phrase(PhraseQuery::new(
                    "body",
                    vec!["the".to_owned(), "quick".to_owned()],
                )),
                10,
                false,
            )
            .unwrap();
        let ids: Vec<u32> = hits.iter().map(|h| h.doc_id).collect();
        assert!(ids.contains(&0), "doc0 has 'the quick'");
        assert!(!ids.contains(&2), "doc2 has 'the lazy', not 'the quick'");

        // A NON-adjacent pair with a gap must NOT match at slop 0:
        // doc0 = "the quick brown fox jumps over the lazy dog" — "quick dog"
        // are not adjacent.
        let hits = searcher
            .search(
                &Query::Phrase(PhraseQuery::new(
                    "body",
                    vec!["quick".to_owned(), "dog".to_owned()],
                )),
                10,
                false,
            )
            .unwrap();
        assert!(
            hits.iter().all(|h| h.doc_id != 0),
            "'quick dog' is not an adjacent phrase in doc0"
        );
    }

    /// Case-sensitive wildcard/fuzzy (text-field path): the whitespace analyzer
    /// preserves case, so an indexed term "Rust" is matched by "R*" but NOT by
    /// "r*" when `case_insensitive = false`.
    #[test]
    fn case_sensitive_wildcard_and_fuzzy() {
        let dir = TempDir::new().unwrap();
        let registry = Arc::new(AnalyzerRegistry::default());
        let mut writer = FtsIndexWriter::new(dir.path(), "seg0", Arc::clone(&registry));
        let cfg = FieldIndexConfig {
            analyzer: "whitespace".to_owned(),
            ..Default::default()
        };
        writer.configure_field("body", cfg);
        writer.add_document(
            0,
            &[("body".to_owned(), "Rust Systems".to_owned())]
                .into_iter()
                .collect(),
        );
        writer.finish().unwrap();
        let reader = Arc::new(FtsIndexReader::open(dir.path(), "seg0", &["body"]).unwrap());
        let searcher = FtsSearcher::new(reader, registry);

        // Case-sensitive wildcard: "R*" matches, "r*" does not.
        let mut ci = WildcardQuery::new("body", "R*");
        ci.case_insensitive = false;
        assert_eq!(searcher.search(&Query::Wildcard(ci), 10, false).unwrap().len(), 1);
        let mut cs = WildcardQuery::new("body", "r*");
        cs.case_insensitive = false;
        assert_eq!(searcher.search(&Query::Wildcard(cs), 10, false).unwrap().len(), 0);
        // Case-insensitive (keyword-style) wildcard: "r*" DOES match.
        let ki = WildcardQuery::new("body", "r*"); // default case_insensitive = true
        assert_eq!(searcher.search(&Query::Wildcard(ki), 10, false).unwrap().len(), 1);

        // Case-sensitive fuzzy: "Rist" (1 edit from "Rust") matches; "rist" does not.
        let mut fz = FuzzyQuery::new("body", "Rist", 1);
        fz.case_insensitive = false;
        assert_eq!(searcher.search(&Query::Fuzzy(fz), 10, false).unwrap().len(), 1);
        let mut fzl = FuzzyQuery::new("body", "rist", 1);
        fzl.case_insensitive = false;
        assert_eq!(searcher.search(&Query::Fuzzy(fzl), 10, false).unwrap().len(), 0);
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

    /// `search_bounded(cap)` must return EXACTLY the same top-`cap` hits (same
    /// order, same scores) as the legacy `execute() + sort_unstable() +
    /// truncate(cap)`, and its reported `total` must equal the full match count
    /// regardless of `cap`.  Exercises both the streaming leaf-`Term` path and
    /// the `execute()`-then-drain compound (`Bool`) path.
    #[test]
    fn search_bounded_matches_legacy_topk_and_total() {
        let dir = TempDir::new().unwrap();
        let searcher = setup_searcher(dir.path());

        // Two query shapes with a MULTI-score match set:
        //  - Term "fox": docs 0,1,4 with distinct BM25 (doc 4 TF=4 ranks top).
        //  - Bool should(fox|quick|lazy): a wider, differently-scored set that
        //    routes through execute()-then-drain, not the streaming leaf.
        let term_q = Query::Term(TermQuery::new("body", "fox"));
        let bool_q = Query::Bool(Box::new(
            BoolQuery::new()
                .should(Query::Term(TermQuery::new("body", "fox")))
                .should(Query::Term(TermQuery::new("body", "quick")))
                .should(Query::Term(TermQuery::new("body", "lazy"))),
        ));

        for q in [&term_q, &bool_q] {
            // Legacy reference: usize::MAX keeps the full-set sort path.
            let full = searcher.search(q, usize::MAX, false).unwrap();
            let total_full = full.len() as u64;
            assert!(total_full >= 3, "query should match several docs");

            // Order must already be strictly non-increasing by score.
            for w in full.windows(2) {
                assert!(w[0].score >= w[1].score);
            }

            for cap in [0usize, 1, 2, 3, total_full as usize, total_full as usize + 5] {
                let (bounded, total) = searcher.search_bounded(q, cap, false).unwrap();

                // (1) Exact total is independent of the page cap.
                assert_eq!(
                    total, total_full,
                    "bounded total must equal full match count at cap={cap}"
                );

                // (2) Top-`cap` is byte-identical to legacy sort+truncate.
                let expected: Vec<(u32, u32)> = full
                    .iter()
                    .take(cap)
                    .map(|h| (h.doc_id, h.score.to_bits()))
                    .collect();
                let got: Vec<(u32, u32)> = bounded
                    .iter()
                    .map(|h| (h.doc_id, h.score.to_bits()))
                    .collect();
                assert_eq!(
                    got, expected,
                    "bounded top-{cap} must match legacy sort+truncate (doc_id+score)"
                );
            }
        }
    }
}
