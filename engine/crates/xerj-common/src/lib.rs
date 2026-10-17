//! # xerj-common
//!
//! Shared types, configuration, error handling, and observability primitives
//! for the xerj search engine — an Elasticsearch-compatible search engine
//! written in Rust.
//!
//! ## Design philosophy
//!
//! Unlike Elasticsearch's 3000+ configuration knobs, xerj deliberately exposes
//! **134 settings**, each meaningful and production-tested. Every default
//! is chosen so that a fresh deployment with zero configuration changes performs
//! well for the majority of workloads.
//!
//! ## Modules
//!
//! - [`config`]  — TOML-based configuration (134 settings)
//! - [`code_query`] — query shaping shared by `xerj search` and the
//!   `xerj_search` MCP tool (multi-word glue-token stripping, #1364)
//! - [`feedback`] — the bug/UX-report invitation shared by every `--help`
//! - [`error`]   — Unified error type ([`XerjError`])
//! - [`types`]   — Core domain types (documents, fields, IDs)
//! - [`calibration`] — probability calibration for the decide surface:
//!   isotonic (PAVA) and temperature (Platt) scaling, reliability curves, ECE
//! - [`field_coercion`] — ES-faithful ingest-time coercion/enforcement for
//!   numeric and boolean fields (the one predicate every write path shares)
//! - [`schema`]  — Index schema management and mapping evolution
//! - [`metrics`] — Prometheus counters, histograms, and gauges
//! - [`net`]     — Network trust primitives (trusted-proxy CIDR matching)
//! - [`resource`] — The machine-resource policy: cores, memory budget, thread priority
//! - [`home`]    — the home directory: `HOME`, else `USERPROFILE` (native Windows)
//! - [`localauth`] — Loopback guard + `<data_dir>/admin.key` discovery for
//!   same-machine CLI/MCP commands (`xerj mcp`, `xerj autoindex`, `xerj init`)
//! - [`xccode`]  — the reference-coding semantics shared by `xerj code`,
//!   `xerj corpus`, and the `xerj_code_search` MCP tool (issue #977)
//! - [`thread_spawn`] — spawning helper threads without aborting the node
//!   on a failed spawn (#1122): the closure comes back, plus a process-wide
//!   memory spawn gate
//! - [`hub`]     — Corpus Hub v2 shared types: the catalogue, pack manifest,
//!   definition record, and the [`hub::Answer`] every surface renders (#1374)

pub mod argv;
pub mod calibration;
pub mod code_query;
pub mod config;
pub mod error;
pub mod feedback;
pub mod field_coercion;
pub mod fsio;
pub mod home;
pub mod hub;
pub mod localauth;
pub mod metrics;
pub mod net;
pub mod resource;
pub mod schema;
pub mod thread_spawn;
pub mod types;
pub mod xccode;

// Convenience re-exports at the crate root
pub use config::Config;
pub use error::XerjError;
pub use types::{DocId, Document, FieldConfig, FieldType, IndexName, Schema, SegmentId, SeqNo};

/// Name of the catalog index `xerj autoindex` maintains on every node it
/// onboards (`xerj-autoindex/src/catalog.rs` defines it; this copy is the
/// one the SERVER side reads). Single source of truth in xerj-common
/// because the two crates do not depend on each other, and both need the
/// exact spelling: the server classifies searches of this index as
/// machine traffic in the audit record (#1109), the client reads it back
/// from `xerj gain`.
pub const AUTOINDEX_CATALOG_INDEX: &str = "autoindex-catalog";

/// Crate-level result alias — uses [`XerjError`] as the error type.
pub type Result<T> = std::result::Result<T, XerjError>;
