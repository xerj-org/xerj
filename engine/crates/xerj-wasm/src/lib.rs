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

    /// The stage type is legitimate but this build does not implement it.
    ///
    /// Issue #204 keeps this separate from [`WasmError::InvalidConfig`]: a
    /// caller who wrote a valid Elasticsearch processor xerj has not
    /// implemented has not made a mistake, and must not be told they have.
    /// What must never happen is the third option the code used to take —
    /// accepting it and quietly running a shorter pipeline.
    #[error("unsupported stage type '{stage}'")]
    UnsupportedStage { stage: String },

    /// The stage type IS implemented, but the caller set an option this build
    /// cannot honour — an Elasticsearch processor-level `if` guard, an
    /// `on_failure` recovery chain, a `grok` `patterns` array.
    ///
    /// Issue #204 again, one level down. Reporting these as `InvalidConfig`
    /// would tell a caller their valid ES processor is malformed; ignoring them
    /// is what the sweep exists to kill — a `set` that runs on documents its
    /// `if` excludes is a silently wrong document, not a cosmetic gap. Handled
    /// exactly like [`WasmError::UnsupportedStage`] at the API boundary: the
    /// definition is accepted (ES accepts it) and recorded as unrunnable, so
    /// every ingest through it refuses loudly.
    #[error("stage '{stage}': option '{option}' is not supported by this build ({reason})")]
    UnsupportedOption {
        stage: String,
        option: String,
        reason: String,
    },

    /// The pipeline is registered but carries a stage this build cannot run,
    /// so no document may pass through it.
    #[error("pipeline '{pipeline}' is registered but cannot run: {reason}")]
    PipelineNotRunnable { pipeline: String, reason: String },

    /// A document handed to a pipeline was not a JSON object, so no stage
    /// could run on it. Issue #3174: `set` / `add_field` index into the
    /// document root with `doc[field] = ...`, and `serde_json` `IndexMut`
    /// panics on a non-object root; the release profile aborts on panic, so
    /// one malformed document killed the whole node instead of a 400.
    /// The payload names the JSON kind the caller actually sent.
    #[error("document must be a JSON object, got {0}")]
    NonObjectDocument(String),

    #[error("plugin error in '{plugin}': {reason}")]
    PluginError { plugin: String, reason: String },

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, WasmError>;
