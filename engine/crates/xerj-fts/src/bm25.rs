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
        Self { k1, b, avg_dl, total_docs }
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
            assert!(idf >= 0.0, "IDF must not be negative, got {} for n={}", idf, n);
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
}
