//! # xerj-engine
//!
//! Integration crate that wires together all xerj subsystems into a working
//! search engine:
//!
//! - [`Engine`]    — manages multiple named indices
//! - [`Index`]     — per-index coordinator: WAL + storage + FTS
//! - [`FtsMemtable`] — in-memory inverted index for unflushed documents
//! - [`bulk`]      — NDJSON bulk operation processing

pub mod aggs;
pub mod audit;
pub mod bulk;
pub mod engine;
pub mod index;
pub mod memtable;
pub mod painless;
pub mod rbac;
pub mod slow_query;
pub mod sql;
pub mod turbo_ingest;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use engine::{Engine, HealthStatus, IndexInfo};
pub use index::{Index, IndexResponse, IndexStats, FieldEncodingInfo, LogFormat, EnrichTable, detect_log_format, resolve_field_alias, resolve_date_math};
pub use memtable::FtsMemtable;

// ── Analyzer helpers ──────────────────────────────────────────────────────────

use std::sync::Arc;
use xerj_fts::analyzer::{
    AnalyzerRegistry, LowercaseFilter, StemmerFilter, StopwordsFilter,
    StandardTokenizer, Tokenizer, TokenFilter,
};

/// Return a default [`AnalyzerRegistry`] pre-populated with all built-in analyzers.
pub fn analyzer_registry() -> Arc<AnalyzerRegistry> {
    Arc::new(AnalyzerRegistry::with_defaults())
}

/// Step-by-step output of the standard analyzer pipeline.
pub struct AnalyzeExplainSteps {
    /// Token texts after the raw tokenizer (no filters applied yet).
    pub after_tokenizer: Vec<serde_json::Value>,
    /// Token texts after the lowercase filter.
    pub after_lowercase: Vec<serde_json::Value>,
    /// Token texts after the stop-word filter.
    pub after_stopwords: Vec<serde_json::Value>,
    /// Token texts after the stemmer (final output).
    pub after_stemmer: Vec<serde_json::Value>,
    /// Alias for `after_stemmer`.
    pub final_tokens: Vec<serde_json::Value>,
}

/// Run the standard analyzer pipeline step-by-step on `input`.
///
/// Returns each intermediate token list so the caller can expose a
/// `"explain": true` analysis response (Item #8 from user feedback).
pub fn analyze_explain_steps(input: &str) -> AnalyzeExplainSteps {
    fn token_json(t: &xerj_fts::analyzer::Token) -> serde_json::Value {
        serde_json::json!({
            "token": t.text,
            "start_offset": t.start_offset,
            "end_offset": t.end_offset,
            "position": t.position,
        })
    }

    // Stage 0: raw tokenizer
    let raw = StandardTokenizer.tokenize(input);
    let after_tokenizer: Vec<_> = raw.iter().map(token_json).collect();

    // Stage 1: lowercase
    let lc_tokens = LowercaseFilter.filter(raw.clone());
    let after_lowercase: Vec<_> = lc_tokens.iter().map(token_json).collect();

    // Stage 2: stop words
    let sw_tokens = StopwordsFilter::english().filter(lc_tokens);
    let after_stopwords: Vec<_> = sw_tokens.iter().map(token_json).collect();

    // Stage 3: stemmer (final)
    let stemmed = StemmerFilter::english().filter(sw_tokens);
    let after_stemmer: Vec<_> = stemmed.iter().map(token_json).collect();
    let final_tokens = after_stemmer.clone();

    AnalyzeExplainSteps {
        after_tokenizer,
        after_lowercase,
        after_stopwords,
        after_stemmer,
        final_tokens,
    }
}

// ── Error type ────────────────────────────────────────────────────────────────

use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error(transparent)]
    Common(#[from] xerj_common::XerjError),

    #[error("storage error: {0}")]
    Storage(#[from] xerj_storage::StorageError),

    #[error("query error: {0}")]
    Query(#[from] xerj_query::QueryError),

    #[error("FTS error: {0}")]
    Fts(anyhow::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Engine-level result alias.
pub type Result<T> = std::result::Result<T, EngineError>;

impl From<EngineError> for xerj_common::XerjError {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::Common(z) => z,
            EngineError::Storage(s) => xerj_common::XerjError::storage(s.to_string()),
            EngineError::Query(q) => xerj_common::XerjError::invalid_query(q.to_string()),
            EngineError::Fts(f) => xerj_common::XerjError::internal(f.to_string()),
            EngineError::Io(io) => xerj_common::XerjError::storage_io("IO error", io),
            EngineError::Serde(s) => xerj_common::XerjError::serialization(s.to_string()),
        }
    }
}
