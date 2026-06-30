//! # xerj-wasm
//!
//! Pluggable transform pipeline for xerj.
//!
//! Provides a trait-based plugin system for document transformation at ingest
//! time.  Built-in plugins (field rename, drop, add, JSON parse, timestamp
//! parse, PII redaction, grok, route) are always available as native Rust
//! code.
//!
//! A real WASM backend (via `wasmtime`) can be wired in later behind the
//! `wasm` feature flag without changing any public API.

pub mod builtins;
pub mod pipeline;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use pipeline::{ErrorPolicy, Pipeline, PipelineConfig, PipelineStageConfig, ProcessAction};

// ── Core trait ────────────────────────────────────────────────────────────────

/// A transform plugin — implemented by built-in Rust transforms (and, in the
/// future, WASM modules).
///
/// Plugins are `Send + Sync` so they can be shared across async tasks and
/// stored in `Arc<dyn TransformPlugin>`.
pub trait TransformPlugin: Send + Sync {
    /// Unique name used to reference this plugin in pipeline configs.
    fn name(&self) -> &str;

    /// Transform `doc` in-place and return the action to take.
    ///
    /// - [`ProcessAction::Pass`]  — continue to the next stage.
    /// - [`ProcessAction::Drop`]  — discard this document entirely.
    /// - [`ProcessAction::Route`] — send the document to a different target
    ///   index.
    fn process(&self, doc: &mut serde_json::Value) -> ProcessAction;
}

// ── Error type ────────────────────────────────────────────────────────────────

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WasmError {
    #[error("pipeline '{0}' not found")]
    PipelineNotFound(String),

    #[error("plugin '{0}' not found")]
    PluginNotFound(String),

    #[error("invalid plugin config for '{plugin}': {reason}")]
    InvalidConfig { plugin: String, reason: String },

    #[error("plugin error in '{plugin}': {reason}")]
    PluginError { plugin: String, reason: String },

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, WasmError>;
