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

// ── Ingest/flush/merge rayon pool ────────────────────────────────────────────

/// Dedicated rayon pool for ingest-side CPU work: bulk-body parse, P2.1
/// per-batch doc parse/analyze/insert, flush FTS+DV side-car builds, and
/// merge FTS rebuilds.
///
/// Pre-fix all of that ran on the GLOBAL rayon pool — the same pool the
/// search path uses for segment fan-out (`search_segments`) and the
/// fast-agg dv-warm par_iter.  A single long flush/merge job therefore
/// queued every search's rayon work behind it: with a background bulk
/// writer running, foreground `match_all` p99 was measured at 15 s+ while
/// steady-state reads were ~2 ms (BEAT_ES_MASTER_PLAN Phase 2, read-under-
/// write).  Ingest work now runs here; the global pool stays free for
/// search/agg fan-out, so neither side can starve the other's queue (the
/// OS scheduler time-slices the two pools fairly).
///
/// Sized to all available cores: ingest throughput is unchanged when no
/// searches run, and under mixed load the kernel — not the rayon job
/// queue — arbitrates.
pub(crate) fn ingest_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8);
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .thread_name(|i| format!("xerj-ingest-{i}"))
            // nice(5): bulk parse/analyze/insert are request-latency
            // sensitive for the WRITER, but under mixed load they were
            // the biggest nice(0) CPU block competing head-to-head with
            // query threads — every flush-storm/bulk burst showed up as
            // a read p95/p99 episode.  One CFS step below reads keeps
            // read tails flat while the writer retains far more
            // throughput than the sustained-ingest target (measured
            // 115 k docs/s at nice 0 vs a 40 k floor; nice 5 trades a
            // slice of that for read-tail stability).  Flush side-cars
            // are nice(10) (`background_pool`), merges nice(15)
            // (`merge_pool`) — the maintenance ladder stays below both.
            .start_handler(|_| unsafe {
                let _ = libc::nice(5);
            })
            .build()
            .expect("failed to build ingest rayon pool")
    })
}

/// Deprioritised rayon pool for BACKGROUND index maintenance: flush
/// side-car builds (FTS + doc-values), segment finalisation, and merge
/// rebuilds.
///
/// Separate from `ingest_pool` (foreground, normal priority — bulk parse
/// and memtable insert are request-latency-critical) and from the global
/// pool (search fan-out).  nice(10) lets CFS schedule foreground search
/// and bulk threads ahead of maintenance encode work, which otherwise
/// saturates every core for seconds per merged segment; with an idle
/// foreground the pool still gets every core, so writer-only throughput
/// is unchanged.
pub(crate) fn background_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8);
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .thread_name(|i| format!("xerj-bg-{i}"))
            .start_handler(|_| unsafe {
                let _ = libc::nice(10);
            })
            .build()
            .expect("failed to build background rayon pool")
    })
}

/// Dedicated rayon pool for **merge** encode work (byte-copy re-encode +
/// FTS side-car rebuild of merged segments).
///
/// Pre-fix merges ran on `ingest_pool()` — the same pool every `_bulk`
/// request's parse/analyze/insert par_iters use.  A single 16-segment
/// (~500 k-doc) merge saturates all N workers for many seconds, so every
/// concurrent bulk request's `install()` queued behind it: measured on the
/// 1 M×c8 ingest benchmark as a 4.7 s → 17.5 s FULL STALL of ingest
/// (window throughput 370 k docs/s → 7.8 k docs/s) the moment the first
/// background merge tick fired.  100 k-doc runs finish before the first
/// 5 s merge tick, which is why the regression only appeared at 1 M scale.
///
/// Merges are pure background work with no client waiting on them — ES
/// likewise runs merges on a small dedicated pool and throttles them under
/// indexing pressure.  Sizing: `max(2, ncores/8)` keeps a merge from ever
/// occupying more than a sliver of the machine, while `nice(15)` (one step
/// below ingest's `nice(10)`) lets both foreground reads AND ingest encode
/// pre-empt merge threads when cores are scarce.  Merges take a few times
/// longer to converge — acceptable: the 5 s merge loop just picks up where
/// it left off, and `merge_in_progress` already serialises passes.
pub(crate) fn merge_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let n = std::thread::available_parallelism()
            .map(|n| (n.get() / 8).max(2))
            .unwrap_or(2);
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .thread_name(|i| format!("xerj-merge-{i}"))
            .start_handler(|_| unsafe {
                let _ = libc::nice(15);
            })
            .build()
            .expect("failed to build merge rayon pool")
    })
}

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
