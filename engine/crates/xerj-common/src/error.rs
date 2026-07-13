//! Unified error type for xerj.
//!
//! All crates in the workspace convert their internal errors into [`XerjError`]
//! before they cross an API boundary. This keeps error handling consistent
//! from storage internals through to HTTP responses.

use thiserror::Error;

/// The canonical error type for the entire xerj engine.
///
/// Variants are kept coarse-grained on purpose — enough granularity to produce
/// useful HTTP status codes and log messages without fragmenting error handling
/// across dozens of leaf types.
#[derive(Debug, Error)]
pub enum XerjError {
    // ── Index lifecycle ────────────────────────────────────────────────────
    /// The requested index does not exist.
    #[error("index not found: {name}")]
    IndexNotFound { name: String },

    /// An index with this name already exists.
    #[error("index already exists: {name}")]
    IndexAlreadyExists { name: String },

    // ── Document operations ────────────────────────────────────────────────
    /// A document with the given ID was not found in the index.
    #[error("document not found: id={id} in index={index}")]
    DocumentNotFound { id: String, index: String },

    // ── Mapping / schema ───────────────────────────────────────────────────
    /// The provided field mapping is invalid or incompatible with the existing schema.
    #[error("invalid mapping: {reason}")]
    InvalidMapping { reason: String },

    // ── Query parsing / execution ──────────────────────────────────────────
    /// The query could not be parsed or executed.
    #[error("invalid query: {reason}")]
    InvalidQuery { reason: String },

    // ── Storage layer ──────────────────────────────────────────────────────
    /// A low-level storage I/O error.
    #[error("storage error: {reason}")]
    StorageError {
        reason: String,
        #[source]
        source: Option<std::io::Error>,
    },

    /// Write-ahead log error.
    #[error("WAL error: {reason}")]
    WalError { reason: String },

    // ── Serialization ──────────────────────────────────────────────────────
    /// Failed to serialize or deserialize data.
    #[error("serialization error: {reason}")]
    SerializationError { reason: String },

    // ── Configuration ──────────────────────────────────────────────────────
    /// The configuration file is invalid.
    #[error("config error: {reason}")]
    ConfigError { reason: String },

    // ── Auth ───────────────────────────────────────────────────────────────
    /// Authentication or authorization failure.
    #[error("auth error: {reason}")]
    AuthError { reason: String },

    // ── TLS ───────────────────────────────────────────────────────────────
    /// TLS setup or handshake failure.
    #[error("TLS error: {reason}")]
    TlsError { reason: String },

    // ── Embedding / AI ─────────────────────────────────────────────────────
    /// Failure communicating with an embedding endpoint.
    #[error("embedding error: {reason}")]
    EmbeddingError { reason: String },

    // ── Resource limits ────────────────────────────────────────────────────
    /// A configured resource limit (memory, concurrency, fields) was exceeded.
    #[error("resource exhausted: {reason}")]
    ResourceExhausted { reason: String },

    // ── Circuit breaker ────────────────────────────────────────────────────
    /// A memory circuit breaker tripped: admitting the request would push the
    /// process past its memtable / RSS budget. Surfaced as HTTP 429
    /// `circuit_breaking_exception`, mirroring Elasticsearch's parent breaker.
    /// Distinct from [`ResourceExhausted`] (thread-pool queue rejection →
    /// `es_rejected_execution_exception`). Display is the bare reason (no
    /// type prefix): the ES mapper already emits the `circuit_breaking_exception`
    /// `type`, so the reason should read like ES's (e.g. "[parent] …").
    #[error("{reason}")]
    CircuitBreaking { reason: String },

    // ── Optimistic concurrency ─────────────────────────────────────────────
    /// Optimistic concurrency control check failed (if_seq_no / if_primary_term).
    #[error(
        "version conflict on document [{id}]: expected seq_no={expected}, actual seq_no={actual}"
    )]
    VersionConflict {
        id: String,
        expected: u64,
        actual: u64,
    },

    // ── Result window ─────────────────────────────────────────────────────
    /// The requested from + size exceeds the max_result_window limit.
    #[error("result window is too large: from + size > max_result_window={max_result_window}; use search_after for deep pagination")]
    ResultWindowTooLarge {
        from: usize,
        size: usize,
        max_result_window: usize,
    },

    // ── Index blocks ──────────────────────────────────────────────────────
    /// Operation rejected because an index block is set.
    #[error("index [{index}] is blocked for {block_type} operations")]
    IndexBlocked { index: String, block_type: String },

    // ── Catch-all ─────────────────────────────────────────────────────────
    /// An unexpected internal error. Indicates a bug.
    #[error("internal error: {reason}")]
    Internal { reason: String },
}

impl XerjError {
    // ── Constructors (ergonomic helpers) ──────────────────────────────────

    pub fn index_not_found(name: impl Into<String>) -> Self {
        Self::IndexNotFound { name: name.into() }
    }

    pub fn index_already_exists(name: impl Into<String>) -> Self {
        Self::IndexAlreadyExists { name: name.into() }
    }

    pub fn document_not_found(id: impl Into<String>, index: impl Into<String>) -> Self {
        Self::DocumentNotFound {
            id: id.into(),
            index: index.into(),
        }
    }

    pub fn invalid_mapping(reason: impl Into<String>) -> Self {
        Self::InvalidMapping {
            reason: reason.into(),
        }
    }

    pub fn invalid_query(reason: impl Into<String>) -> Self {
        Self::InvalidQuery {
            reason: reason.into(),
        }
    }

    pub fn storage(reason: impl Into<String>) -> Self {
        Self::StorageError {
            reason: reason.into(),
            source: None,
        }
    }

    pub fn storage_io(reason: impl Into<String>, source: std::io::Error) -> Self {
        Self::StorageError {
            reason: reason.into(),
            source: Some(source),
        }
    }

    pub fn wal(reason: impl Into<String>) -> Self {
        Self::WalError {
            reason: reason.into(),
        }
    }

    pub fn serialization(reason: impl Into<String>) -> Self {
        Self::SerializationError {
            reason: reason.into(),
        }
    }

    pub fn config(reason: impl Into<String>) -> Self {
        Self::ConfigError {
            reason: reason.into(),
        }
    }

    pub fn auth(reason: impl Into<String>) -> Self {
        Self::AuthError {
            reason: reason.into(),
        }
    }

    pub fn tls(reason: impl Into<String>) -> Self {
        Self::TlsError {
            reason: reason.into(),
        }
    }

    pub fn embedding(reason: impl Into<String>) -> Self {
        Self::EmbeddingError {
            reason: reason.into(),
        }
    }

    pub fn resource_exhausted(reason: impl Into<String>) -> Self {
        Self::ResourceExhausted {
            reason: reason.into(),
        }
    }

    pub fn circuit_breaking(reason: impl Into<String>) -> Self {
        Self::CircuitBreaking {
            reason: reason.into(),
        }
    }

    pub fn internal(reason: impl Into<String>) -> Self {
        Self::Internal {
            reason: reason.into(),
        }
    }

    pub fn version_conflict(id: impl Into<String>, expected: u64, actual: u64) -> Self {
        Self::VersionConflict {
            id: id.into(),
            expected,
            actual,
        }
    }

    pub fn result_window_too_large(from: usize, size: usize, max_result_window: usize) -> Self {
        Self::ResultWindowTooLarge {
            from,
            size,
            max_result_window,
        }
    }

    pub fn index_blocked(index: impl Into<String>, block_type: impl Into<String>) -> Self {
        Self::IndexBlocked {
            index: index.into(),
            block_type: block_type.into(),
        }
    }

    // ── Classification helpers ────────────────────────────────────────────

    /// Returns the HTTP status code most appropriate for this error.
    pub fn http_status(&self) -> u16 {
        match self {
            Self::IndexNotFound { .. } | Self::DocumentNotFound { .. } => 404,
            Self::IndexAlreadyExists { .. } | Self::VersionConflict { .. } => 409,
            Self::InvalidMapping { .. }
            | Self::InvalidQuery { .. }
            | Self::ConfigError { .. }
            | Self::ResultWindowTooLarge { .. } => 400,
            Self::IndexBlocked { .. } => 403,
            Self::AuthError { .. } => 401,
            Self::ResourceExhausted { .. } | Self::CircuitBreaking { .. } => 429,
            Self::StorageError { .. }
            | Self::WalError { .. }
            | Self::SerializationError { .. }
            | Self::TlsError { .. }
            | Self::EmbeddingError { .. }
            | Self::Internal { .. } => 500,
        }
    }

    /// Returns `true` if the error is transient and the caller may retry.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::ResourceExhausted { .. }
                | Self::CircuitBreaking { .. }
                | Self::StorageError { .. }
                | Self::WalError { .. }
                | Self::EmbeddingError { .. }
        )
    }

    /// Returns `true` if this is a version conflict (OCC failure).
    pub fn is_version_conflict(&self) -> bool {
        matches!(self, Self::VersionConflict { .. })
    }
}

// ── Standard library conversions ──────────────────────────────────────────────

impl From<std::io::Error> for XerjError {
    fn from(e: std::io::Error) -> Self {
        Self::StorageError {
            reason: e.to_string(),
            source: Some(e),
        }
    }
}

impl From<serde_json::Error> for XerjError {
    fn from(e: serde_json::Error) -> Self {
        Self::SerializationError {
            reason: e.to_string(),
        }
    }
}

impl From<toml::de::Error> for XerjError {
    fn from(e: toml::de::Error) -> Self {
        Self::ConfigError {
            reason: e.to_string(),
        }
    }
}

impl From<anyhow::Error> for XerjError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal {
            reason: e.to_string(),
        }
    }
}
