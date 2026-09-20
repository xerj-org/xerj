//! BM25 relevance scoring — identical constants and formula to Elasticsearch/Lucene.
//!
//! ## Formula
//!
//! ```text
//! score(q, d) = Σ_t [ IDF(t) * TF(t, d) ]
//!
//! IDF(t) = ln(1 + (N - n_t + 0.5) / (n_t + 0.5))
//!
//! TF(t, d) = (tf * (k1 + 1)) / (tf + k1 * (1 - b + b * (dl / avgdl)))
//! ```
//!
//! Where:
//! - `N`    = total number of documents in the segment
//! - `n_t`  = number of documents containing term `t` (doc frequency)
//! - `tf`   = term frequency in document `d`
//! - `dl`   = document length (number of tokens in the field)
//! - `avgdl`= average document length across the segment
//! - `k1`   = 1.2  (term frequency saturation parameter)
//! - `b`    = 0.75 (length normalization parameter)
//!
//! These defaults match Elasticsearch's `BM25Similarity` exactly.

use serde::{Deserialize, Serialize};

/// Default term-frequency saturation constant (ES default = 1.2).
pub const DEFAULT_K1: f32 = 1.2;
/// Default length normalization constant (ES default = 0.75).
pub const DEFAULT_B: f32 = 0.75;

// ── Scorer ────────────────────────────────────────────────────────────────────

/// Stateless BM25 scorer seeded with per-segment statistics.
///
/// Create one instance per field per segment and reuse it for every query term.
#[derive(Debug, Clone)]
pub struct Bm25Scorer {
    /// Term frequency saturation (k₁ in the BM25 formula).
    pub k1: f32,
    /// Length normalization factor (b in the BM25 formula).
    pub b: f32,
    /// Average field length across all documents in the segment.
    pub avg_dl: f32,
    /// Total number of documents in the segment (for IDF).
    pub total_docs: u64,
}

impl Bm25Scorer {
    /// Creates a scorer with Elasticsearch-compatible defaults.
    pub fn new(avg_dl: f32, total_docs: u64) -> Self {
        Self {
            k1: DEFAULT_K1,
            b: DEFAULT_B,
            avg_dl,
            total_docs,
        }
    }

    /// Creates a scorer with custom k1/b parameters.
    pub fn with_params(k1: f32, b: f32, avg_dl: f32, total_docs: u64) -> Self {
        Self {
            k1,
            b,
            avg_dl,
            total_docs,
        }
    }

    /// Compute IDF for a term.
    ///
    /// Uses the Lucene smoothed IDF formula:
    /// `IDF = ln(1 + (N - n + 0.5) / (n + 0.5))`
    ///
    /// This is always positive and never zero, even for very common terms.
    #[inline]
    pub fn idf(&self, doc_freq: u64) -> f32 {
        let n = self.total_docs as f32;
        let df = doc_freq as f32;
        ((1.0 + (n - df + 0.5) / (df + 0.5)).ln()).max(0.0)
    }

    /// Compute the TF normalization factor.
    ///
    /// `TF_norm = (tf * (k1 + 1)) / (tf + k1 * (1 - b + b * dl/avgdl))`
    #[inline]
    pub fn tf_norm(&self, term_freq: u32, doc_length: u32) -> f32 {
        let tf = term_freq as f32;
        let dl = doc_length as f32;
        let avg = self.avg_dl.max(1.0);
        let norm = self.k1 * (1.0 - self.b + self.b * (dl / avg));
        (tf * (self.k1 + 1.0)) / (tf + norm)
    }

    /// The `norm` term of [`Self::tf_norm`] alone — the per-DOCUMENT factor,
    /// hoisted so a caller that already knows the length can precompute it
    /// (per quantised norm byte, per distinct length) and skip the divide in
    /// the per-posting loop.
    ///
    /// BIT-IDENTICAL to the same expression inside `tf_norm`: same
    /// operations, same order, `doc_length` converted the same way.
    #[inline]
    pub fn norm_denom(&self, doc_length: u32) -> f32 {
        let avg = self.avg_dl.max(1.0);
        self.k1 * (1.0 - self.b + self.b * (doc_length as f32 / avg))
    }

    /// Score a single (term, document) pair.
    ///
    /// # Arguments
    /// * `doc_freq`    — number of documents in the segment containing this term
    /// * `term_freq`   — occurrences of the term in the candidate document
    /// * `doc_length`  — field length of the candidate document (in tokens)
    #[inline]
    pub fn score_term(&self, doc_freq: u64, term_freq: u32, doc_length: u32) -> f32 {
        self.idf(doc_freq) * self.tf_norm(term_freq, doc_length)
    }

    /// [`Self::score_term`] with both hoistable factors already computed:
    /// `idf` is the per-TERM value (one `ln` per clause, not per posting)
    /// and `denom` is [`Self::norm_denom`] for the posting's document.
    ///
    /// BIT-IDENTICAL to `score_term(doc_freq, term_freq, doc_length)` when
    /// `idf == self.idf(doc_freq)` and `denom == self.norm_denom(doc_length)`
    /// — the same multiplications in the same order.  Pinned by
    /// `score_term_denom_is_bit_identical_to_score_term`.
    #[inline]
    pub fn score_term_denom(&self, idf: f32, term_freq: u32, denom: f32) -> f32 {
        let tf = term_freq as f32;
        idf * ((tf * (self.k1 + 1.0)) / (tf + denom))
    }

    /// Score with a full explanation breakdown.
    pub fn score_term_explain(
        &self,
        term: &str,
        doc_freq: u64,
        term_freq: u32,
        doc_length: u32,
    ) -> ScoreBreakdown {
        let idf = self.idf(doc_freq);
        let tf = self.tf_norm(term_freq, doc_length);
        let score = idf * tf;

        ScoreBreakdown {
            score,
            term: term.to_owned(),
            idf,
            tf_norm: tf,
            term_freq,
            doc_freq,
            doc_length,
            avg_dl: self.avg_dl,
            total_docs: self.total_docs,
            k1: self.k1,
            b: self.b,
        }
    }
}

// ── Explanation ───────────────────────────────────────────────────────────────

/// Detailed breakdown of how a BM25 score was computed.
///
/// Matches the structure of Elasticsearch's `_explain` API response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreBreakdown {
    /// Final BM25 score = `idf * tf_norm`.
    pub score: f32,
    /// The term that generated this contribution.
    pub term: String,
    /// Inverse document frequency component.
    pub idf: f32,
    /// Normalized term frequency component.
    pub tf_norm: f32,
    /// Raw term frequency in the document.
    pub term_freq: u32,
    /// Number of documents containing this term in the segment.
    pub doc_freq: u64,
    /// Length of the field in the document (tokens).
    pub doc_length: u32,
    /// Average field length across the segment.
    pub avg_dl: f32,
    /// Total documents in the segment.
    pub total_docs: u64,
    /// k1 parameter used.
    pub k1: f32,
    /// b parameter used.
    pub b: f32,
}

impl ScoreBreakdown {
    /// Render a human-readable description similar to ES `_explain`.
    pub fn describe(&self) -> String {
        format!(
            "score({term}) = {score:.6} = idf({idf:.6}) * tf_norm({tf_norm:.6})\n\
             idf = ln(1 + ({N} - {n} + 0.5) / ({n} + 0.5))\n\
             tf_norm = ({tf} * ({k1} + 1)) / ({tf} + {k1} * (1 - {b} + {b} * ({dl} / {avgdl})))",
            term = self.term,
            score = self.score,
            idf = self.idf,
            tf_norm = self.tf_norm,
            N = self.total_docs,
            n = self.doc_freq,
            tf = self.term_freq,
            k1 = self.k1,
            b = self.b,
            dl = self.doc_length,
            avgdl = self.avg_dl,
        )
    }
}

/// Aggregated score explanation for a multi-term query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryExplanation {
    /// Sum of all per-term scores.
    pub total_score: f32,
    /// Per-term breakdowns.
    pub term_breakdowns: Vec<ScoreBreakdown>,
}

impl QueryExplanation {
    pub fn new(breakdowns: Vec<ScoreBreakdown>) -> Self {
        let total_score = breakdowns.iter().map(|b| b.score).sum();
        Self {
            total_score,
            term_breakdowns: breakdowns,
        }
    }
}

// ── Field stats (per segment, per field) ─────────────────────────────────────

/// Aggregated field-level statistics needed to construct a `Bm25Scorer`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FieldStats {
    /// Total number of documents that have this field.
    pub total_docs: u64,
    /// Sum of all field lengths (in tokens) across all documents.
    pub total_field_length: u64,
}

impl FieldStats {
    pub fn avg_field_length(&self) -> f32 {
        if self.total_docs == 0 {
            return 0.0;
        }
        self.total_field_length as f32 / self.total_docs as f32
    }

    pub fn to_scorer(&self) -> Bm25Scorer {
        Bm25Scorer::new(self.avg_field_length(), self.total_docs)
    }

    pub fn to_scorer_with_params(&self, k1: f32, b: f32) -> Bm25Scorer {
        Bm25Scorer::with_params(k1, b, self.avg_field_length(), self.total_docs)
    }
}

/// `FieldStats` is additive: the union of two arms' statistics is the sum of
/// their doc counts and their total field lengths.  This is what makes an
/// index-wide [`CollectionStats`] a plain fold over the segments plus the
/// memtable (Lucene does the same fold in `IndexSearcher.fieldStats`, summing
/// `docCount`/`sumTotalTermFreq` over `reader.leaves()`).
impl std::ops::AddAssign<&FieldStats> for FieldStats {
    fn add_assign(&mut self, rhs: &FieldStats) {
        self.total_docs += rhs.total_docs;
        self.total_field_length += rhs.total_field_length;
    }
}

// ── Collection stats (index-wide: every segment + the memtable) ──────────────

/// Index-wide BM25 collection statistics — the union over every live scoring
/// arm (all segments **and** the memtable), for exactly the (field, term)
/// pairs one query needs.
///
/// ## Why this exists (#188)
///
/// BM25 is only comparable between two documents when both were scored
/// against the *same* `N`, `avgdl` and `doc_freq`.  Before this type each arm
/// scored against its own local statistics: every segment used its own
/// `FieldStats`, and the memtable used the union over its shards.  Two
/// consequences, both user-visible:
///
///  * **Overwriting a document promoted it.**  An overwrite moves the live
///    copy into the memtable; if it is the only doc there, `N = 1`,
///    `df = 1` and `dl/avgdl = 1`, so `idf = ln(4/3) = 0.2877` and
///    `tf_norm = 1.0` — a fixed 0.2877 that outranks almost any correctly
///    scored segment hit, regardless of the document's real length or the
///    corpus.
///  * **Scores depended on segment topology.**  The same corpus flushed into
///    1 segment vs 16 gave the same document scores differing by >3×,
///    because `avgdl`/`N` were per-segment.
///
/// Feeding one `CollectionStats` to every arm makes the score a function of
/// the index, not of where a document happens to be sitting.
///
/// ## Semantics
///
/// Statistics are **physical / ghost-inclusive** — tombstoned and superseded
/// versions still count until a merge purges them.  That matches Lucene (which
/// counts deleted docs in `docFreq`/`docCount` until they are merged away) and
/// the memtable's existing delete-aware aggregation.
///
/// `N` is the per-field *docs-with-field* count, not the index doc count —
/// the same pairing Lucene's `BM25Similarity.idfExplain` uses
/// (`fieldStats.docCount()` with `termStats.docFreq()`).
#[derive(Debug, Clone, Default)]
pub struct CollectionStats {
    /// Per-field union over every live arm.
    fields: std::collections::HashMap<String, FieldStats>,
    /// Index-wide doc_freq for exactly the (field, term) pairs the query needs.
    doc_freq: std::collections::HashMap<(String, String), u64>,
}

impl CollectionStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one arm's `FieldStats` for `field` into the union.
    pub fn add_field(&mut self, field: &str, stats: &FieldStats) {
        if stats.total_docs == 0 && stats.total_field_length == 0 {
            return;
        }
        match self.fields.get_mut(field) {
            Some(acc) => *acc += stats,
            None => {
                self.fields.insert(field.to_owned(), stats.clone());
            }
        }
    }

    /// Fold one arm's doc_freq for `(field, term)` into the union.
    pub fn add_doc_freq(&mut self, field: &str, term: &str, df: u64) {
        if df == 0 {
            return;
        }
        *self
            .doc_freq
            .entry((field.to_owned(), term.to_owned()))
            .or_insert(0) += df;
    }

    /// The union `FieldStats` for `field`, if any arm indexed it.
    pub fn field(&self, field: &str) -> Option<&FieldStats> {
        self.fields.get(field)
    }

    /// A scorer seeded with the index-wide `avgdl` + `N` for `field`.
    ///
    /// `None` when no arm reported the field — callers must then fall back to
    /// their local statistics rather than score against `N = 0` (which would
    /// clamp every IDF to zero).
    pub fn scorer(&self, field: &str) -> Option<Bm25Scorer> {
        self.fields
            .get(field)
            .filter(|s| s.total_docs > 0)
            .map(|s| s.to_scorer())
    }

    /// Index-wide doc_freq for `(field, term)`; `None` when the pair was not
    /// collected (caller falls back to its local df).
    pub fn df(&self, field: &str, term: &str) -> Option<u64> {
        // Borrowing a `(String, String)` key without allocating needs the
        // `Borrow` trick, which tuples don't support — this map is only ever
        // probed a handful of times per query (once per field × query term),
        // so the two short allocations are not worth a custom key type.
        self.doc_freq
            .get(&(field.to_owned(), term.to_owned()))
            .copied()
    }

    /// True when no arm contributed anything — the caller should use its
    /// local statistics unchanged.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.doc_freq.is_empty()
    }

    /// Per-field union view (used by the memtable arm, which needs `N` and
    /// `avgdl` separately from a `Bm25Scorer`).
    pub fn field_iter(&self) -> impl Iterator<Item = (&String, &FieldStats)> {
        self.fields.iter()
    }

    /// The `(field, term)` pairs this instance carries a df for — lets a
    /// caller that folded one arm's stats discover which terms that arm
    /// analysed, so the same set can be collected from the others.
    pub fn term_keys(&self) -> impl Iterator<Item = (&String, &String)> {
        self.doc_freq.keys().map(|(f, t)| (f, t))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify against known Elasticsearch output for a simple scenario.
    /// ES 8.x with default BM25 settings on a 3-doc index:
    ///   doc1: "the quick brown fox" (length=4, tf=1 for "fox")
    ///   doc2: "the fox" (length=2, tf=1 for "fox")
    ///   doc3: "the quick brown fox jumps" (length=5, tf=1 for "fox")
    /// All 3 docs have "fox": n=3, N=3
    #[test]
    fn idf_all_docs_contain_term() {
        let scorer = Bm25Scorer::new(3.67, 3);
        // IDF = ln(1 + (3 - 3 + 0.5) / (3 + 0.5)) = ln(1 + 0.5/3.5) = ln(1.1428...) ≈ 0.1335
        let idf = scorer.idf(3);
        assert!((idf - 0.1335).abs() < 0.001, "idf = {}", idf);
    }

    #[test]
    fn idf_rare_term() {
        let scorer = Bm25Scorer::new(100.0, 1_000_000);
        // 1 doc contains it out of 1M
        // IDF = ln(1 + (1_000_000 - 1 + 0.5) / (1 + 0.5)) ≈ ln(666667) ≈ 13.41
        let idf = scorer.idf(1);
        assert!(idf > 13.0 && idf < 14.0, "idf = {}", idf);
    }

    #[test]
    fn score_increases_with_term_freq() {
        let scorer = Bm25Scorer::new(10.0, 1000);
        let s1 = scorer.score_term(10, 1, 10);
        let s2 = scorer.score_term(10, 3, 10);
        let s3 = scorer.score_term(10, 10, 10);
        assert!(s1 < s2, "score should increase with tf");
        assert!(s2 < s3, "score should increase with tf");
    }

    #[test]
    fn score_decreases_with_doc_length() {
        let scorer = Bm25Scorer::new(10.0, 1000);
        let s_short = scorer.score_term(10, 2, 5);
        let s_long = scorer.score_term(10, 2, 100);
        assert!(s_short > s_long, "shorter docs score higher for same tf");
    }

    /// The hoisted scoring path (`score_term_denom` with a precomputed
    /// `idf` + `norm_denom`) must reproduce `score_term`'s exact BITS —
    /// bit-identity is the hard requirement on every score-path change, so
    /// this is an exhaustive-to_bits check over a deterministic parameter
    /// sweep, not an epsilon compare.
    #[test]
    fn score_term_denom_is_bit_identical_to_score_term() {
        let mut r = 1u32; // deterministic LCG
        let mut next = move || {
            r = r.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            r
        };
        for (k1, b, avg, n) in [
            (DEFAULT_K1, DEFAULT_B, 41.3f32, 120_000u64),
            (1.6, 0.4, 7.7, 999),
            (2.0, 0.0, 250.25, 65_536),
            (1.2, 0.75, 0.5, 10), // avgdl < 1 → the max(1.0) clamp
        ] {
            let scorer = Bm25Scorer::with_params(k1, b, avg, n);
            for _ in 0..5_000 {
                let df = (next() % 120_000) as u64;
                let tf = next() % 40;
                let dl = next() % 3_000;
                let direct = scorer.score_term(df.min(n), tf, dl);
                let hoisted =
                    scorer.score_term_denom(scorer.idf(df.min(n)), tf, scorer.norm_denom(dl));
                assert_eq!(
                    direct.to_bits(),
                    hoisted.to_bits(),
                    "k1={k1} b={b} avg={avg} df={} tf={tf} dl={dl}",
                    df.min(n)
                );
            }
        }
    }

    #[test]
    fn explain_format() {
        let scorer = Bm25Scorer::new(10.0, 100);
        let breakdown = scorer.score_term_explain("fox", 5, 2, 8);
        let desc = breakdown.describe();
        assert!(desc.contains("fox"));
        assert!(desc.contains("score"));
        assert!(desc.contains("idf"));
    }

    #[test]
    fn idf_never_negative() {
        // When all docs contain the term, IDF approaches 0 but stays non-negative
        let scorer = Bm25Scorer::new(1.0, 100);
        for n in 1u64..=100 {
            let idf = scorer.idf(n);
            assert!(
                idf >= 0.0,
                "IDF must not be negative, got {} for n={}",
                idf,
                n
            );
        }
    }

    #[test]
    fn field_stats_avg() {
        let stats = FieldStats {
            total_docs: 4,
            total_field_length: 20,
        };
        assert_eq!(stats.avg_field_length(), 5.0);
    }

    // ── #188: index-wide collection statistics ───────────────────────────

    #[test]
    fn field_stats_add_assign_is_the_union() {
        let mut a = FieldStats {
            total_docs: 2,
            total_field_length: 20,
        };
        let b = FieldStats {
            total_docs: 3,
            total_field_length: 60,
        };
        a += &b;
        assert_eq!(a.total_docs, 5);
        assert_eq!(a.total_field_length, 80);
        assert_eq!(a.avg_field_length(), 16.0);
    }

    #[test]
    fn collection_stats_folds_arms_and_answers_per_field() {
        let mut cs = CollectionStats::new();
        // Two "segments" and a "memtable", the shape the engine folds.
        cs.add_field(
            "body",
            &FieldStats {
                total_docs: 300,
                total_field_length: 300_000,
            },
        );
        cs.add_field(
            "body",
            &FieldStats {
                total_docs: 65,
                total_field_length: 1_300,
            },
        );
        cs.add_field(
            "body",
            &FieldStats {
                total_docs: 1,
                total_field_length: 87,
            },
        );
        cs.add_doc_freq("body", "quicklist", 300);
        cs.add_doc_freq("body", "quicklist", 65);
        cs.add_doc_freq("body", "quicklist", 1);

        let fs = cs.field("body").expect("body present");
        assert_eq!(fs.total_docs, 366);
        assert_eq!(fs.total_field_length, 301_387);
        assert_eq!(cs.df("body", "quicklist"), Some(366));
        // Unknown field / term fall back to the caller's local stats.
        assert!(cs.scorer("title").is_none());
        assert_eq!(cs.df("body", "absent"), None);

        let scorer = cs.scorer("body").expect("scorer");
        assert_eq!(scorer.total_docs, 366);
        assert!((scorer.avg_dl - 301_387.0 / 366.0).abs() < 0.01);
    }

    /// The single-arm identity: folding exactly ONE arm must produce a scorer
    /// bit-identical to that arm's own `FieldStats::to_scorer()`.  This is the
    /// property the engine's single-arm gate relies on being true.
    #[test]
    fn collection_stats_of_one_arm_is_that_arm() {
        let only = FieldStats {
            total_docs: 365,
            total_field_length: 416_960,
        };
        let mut cs = CollectionStats::new();
        cs.add_field("body", &only);
        let a = cs.scorer("body").unwrap();
        let b = only.to_scorer();
        assert_eq!(a.total_docs, b.total_docs);
        assert_eq!(a.avg_dl.to_bits(), b.avg_dl.to_bits());
        assert_eq!(a.k1.to_bits(), b.k1.to_bits());
        assert_eq!(a.b.to_bits(), b.b.to_bits());
    }

    /// An arm that reported the field but has zero documents must not produce
    /// a scorer: `N = 0` drives `idf` negative and the `.max(0.0)` clamp would
    /// silently zero every score.
    #[test]
    fn collection_stats_declines_an_empty_field() {
        let mut cs = CollectionStats::new();
        cs.add_field(
            "body",
            &FieldStats {
                total_docs: 0,
                total_field_length: 0,
            },
        );
        assert!(cs.scorer("body").is_none());
        assert!(cs.is_empty());
    }

    /// The concrete #188 arithmetic: the memtable-alone statistics that made
    /// an overwritten document jump to first place, versus the index-wide
    /// statistics that put it back where it belongs.
    #[test]
    fn index_wide_stats_demote_the_lone_memtable_document() {
        // weak001: one occurrence of the term in an 87-token field.
        let (tf, dl) = (1u32, 87u32);

        // BEFORE — memtable alone: N = 1, df = 1, avgdl = 87.
        let lone = FieldStats {
            total_docs: 1,
            total_field_length: 87,
        };
        let before = lone.to_scorer().score_term(1, tf, dl);
        assert!(
            (before - 0.28768212).abs() < 1e-6,
            "the reported failure value should reproduce exactly, got {before}"
        );

        // AFTER — index-wide: 366 physical docs, df = 366, Σ len = 417 047.
        let union = FieldStats {
            total_docs: 366,
            total_field_length: 417_047,
        };
        let scorer = union.to_scorer();
        let weak = scorer.score_term(366, tf, dl);
        // strong0: 10 occurrences in a 10-token field.
        let strong = scorer.score_term(366, 10, 10);
        assert!(
            strong > weak,
            "the short, term-dense document must outrank the long, buried one \
             (strong={strong}, weak={weak})"
        );
        assert!(
            before > strong,
            "sanity: it is precisely because the lone-memtable score ({before}) beat the \
             correctly-scored top hit ({strong}) that an overwrite promoted the document"
        );
    }
}
