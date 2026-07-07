//! # xerj-common
//!
//! Shared types, configuration, error handling, and observability primitives
//! for the xerj search engine — an Elasticsearch-compatible search engine
//! written in Rust.
//!
//! ## Design philosophy
//!
//! Unlike Elasticsearch's 3000+ configuration knobs, xerj deliberately exposes
//! exactly **38 settings**, each meaningful and production-tested. Every default
//! is chosen so that a fresh deployment with zero configuration changes performs
//! well for the majority of workloads.
//!
//! ## Modules
//!
//! - [`config`]  — TOML-based configuration (38 settings)
//! - [`error`]   — Unified error type ([`XerjError`])
//! - [`types`]   — Core domain types (documents, fields, IDs)
//! - [`schema`]  — Index schema management and mapping evolution
//! - [`metrics`] — Prometheus counters, histograms, and gauges

pub mod config;
pub mod error;
pub mod metrics;
pub mod schema;
pub mod types;

// Convenience re-exports at the crate root
pub use config::Config;
pub use error::XerjError;
pub use types::{DocId, Document, FieldConfig, FieldType, IndexName, Schema, SegmentId, SeqNo};

/// Crate-level result alias — uses [`XerjError`] as the error type.
pub type Result<T> = std::result::Result<T, XerjError>;
