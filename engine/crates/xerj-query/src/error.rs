//! Error types for the query crate.

use thiserror::Error;

/// The query crate's error type.
#[derive(Debug, Error)]
pub enum QueryError {
    /// The query DSL JSON was syntactically invalid or semantically rejected.
    #[error("parse error: {0}")]
    Parse(#[from] ParseError),

    /// The query references a field that does not exist in the schema.
    #[error("unknown field `{field}` in {context}")]
    UnknownField { field: String, context: String },

    /// The rewriter hit its pass limit without converging — indicates a bug.
    #[error("query rewriter did not converge after {max_passes} passes")]
    RewriterNotConverged { max_passes: usize },

    /// The executor was given an unsupported execution plan variant.
    #[error("unsupported execution plan: {0}")]
    UnsupportedPlan(String),

    /// A timeout was configured and the query exceeded it.
    #[error("query timed out after {elapsed_ms}ms (limit {limit_ms}ms)")]
    Timeout { elapsed_ms: u64, limit_ms: u64 },

    /// An underlying I/O or segment error.
    #[error("storage error: {0}")]
    Storage(String),

    /// Propagated from `anyhow`.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// A parse-level error with a human-readable message.
///
/// Prefer `ParseError::invalid(…)` for straightforward "bad input" cases
/// rather than constructing the struct directly.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("{0}")]
    Invalid(String),
    #[error("unknown query type `{0}` — see https://xerj.io/docs/query-dsl for supported types")]
    UnknownQueryType(String),
}

impl ParseError {
    pub fn invalid(msg: impl Into<String>) -> Self {
        ParseError::Invalid(msg.into())
    }

    pub fn unknown_query_type(name: impl Into<String>) -> Self {
        ParseError::UnknownQueryType(name.into())
    }

    /// Shorthand for `Err(QueryError::Parse(ParseError::invalid(msg)))`.
    pub fn err<T>(msg: impl Into<String>) -> Result<T> {
        Err(QueryError::Parse(ParseError::Invalid(msg.into())))
    }

    /// Shorthand for `Err(QueryError::Parse(ParseError::UnknownQueryType(name)))`.
    pub fn err_unknown<T>(name: impl Into<String>) -> Result<T> {
        Err(QueryError::Parse(ParseError::UnknownQueryType(name.into())))
    }
}

/// Convenience alias — every public function in this crate returns this.
pub type Result<T> = std::result::Result<T, QueryError>;
