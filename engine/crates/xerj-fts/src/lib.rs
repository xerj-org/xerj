//! # xerj-fts
//!
//! Full-text search engine for xerj — an Elasticsearch-compatible search
//! engine written in Rust.
//!
//! ## Architecture
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────────┐
//! │                       xerj-fts                                │
//! │                                                                │
//! │  analyzer  ──►  index (writer/reader)  ──►  search            │
//! │  pipeline        FST + posting lists        BM25 scoring       │
//! │                                                                │
//! │  bm25      ──►  postings (encoding)                           │
//! │  scorer          PFOR + vbyte                                  │
//! └────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Modules
//!
//! - [`analyzer`] — Text analysis pipeline: `CharFilter → Tokenizer → TokenFilter`
//! - [`postings`] — PFOR-compressed 128-doc posting list blocks
//! - [`bm25`]     — BM25 scoring with ES-compatible constants (k1=1.2, b=0.75)
//! - [`index`]    — FST term dictionary + FTS index writer/reader
//! - [`search`]   — Query execution: term, phrase, bool, prefix
//!
//! ## Quick example
//!
//! ```rust,no_run
//! use std::collections::HashMap;
//! use std::sync::Arc;
//! use xerj_fts::{
//!     analyzer::AnalyzerRegistry,
//!     index::{FtsIndexWriter, FtsIndexReader},
//!     search::{FtsSearcher, Query, TermQuery},
//! };
//!
//! // Build an index
//! let dir = tempfile::tempdir().unwrap();
//! let registry = Arc::new(AnalyzerRegistry::default());
//! let mut writer = FtsIndexWriter::new(dir.path(), "seg0", Arc::clone(&registry));
//!
//! let mut doc = HashMap::new();
//! doc.insert("body".to_owned(), "the quick brown fox".to_owned());
//! writer.add_document(0, &doc);
//! writer.finish().unwrap();
//!
//! // Search the index
//! let reader = Arc::new(FtsIndexReader::open(dir.path(), "seg0", &["body"]).unwrap());
//! let searcher = FtsSearcher::new(reader, registry);
//!
//! let hits = searcher.search(&Query::Term(TermQuery::new("body", "fox")), 10, false).unwrap();
//! println!("found {} hits", hits.len());
//! ```

pub mod analyzer;
pub mod bm25;
pub mod index;
pub mod postings;
pub mod search;

// ── Convenience re-exports ────────────────────────────────────────────────────

pub use analyzer::{
    AnalyzerPipeline, AnalyzerRegistry, AsciiFoldingFilter, CharFilter, CjkTokenizer,
    EdgeNGramTokenizer, IcuFoldingFilter, KeywordTokenizer, LengthFilter, LowercaseFilter,
    NGramTokenizer, PatternTokenizer, ShingleFilter, StandardTokenizer, StemmerFilter,
    StopwordsFilter, SynonymFilter, ThaiTokenizer, Token, TokenFilter, Tokenizer,
    WhitespaceTokenizer,
};

pub use bm25::{Bm25Scorer, FieldStats, QueryExplanation, ScoreBreakdown, DEFAULT_B, DEFAULT_K1};

pub use index::{FieldIndexConfig, FtsIndexReader, FtsIndexWriter};

pub use postings::{
    DecodedPosting, PostingsReader, PostingsWriter, TermPostings, BLOCK_SIZE, SKIP_INTERVAL,
};

pub use search::{
    search_segments, BoolQuery, FtsSearcher, PhraseQuery, PrefixQuery, Query, ScoredHit, TermQuery,
};
