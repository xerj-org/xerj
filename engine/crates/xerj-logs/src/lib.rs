//! # xerj-logs
//!
//! Columnar log storage engine for the xerj search engine.
//!
//! Provides:
//! - [`columnar`]  — Column-oriented storage with type-specific encoding
//! - [`ingest`]    — Structured log ingestion with template extraction
//! - [`query`]     — Time-range queries with aggregations
//! - [`retention`] — Automatic data expiry based on retention policy

pub mod columnar;
pub mod ingest;
pub mod query;
pub mod retention;

pub use columnar::{Column, ColumnReader, ColumnType, ColumnWriter};
pub use ingest::{LogIngester, LogRecord};
pub use query::{Aggregation, LogQuery, LogQueryExecutor, QueryResult};
pub use retention::RetentionPolicy;

pub use xerj_common::Result;
