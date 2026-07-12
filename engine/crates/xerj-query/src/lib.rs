//! # xerj-query
//!
//! Query parser, rewriter, planner, and executor for the xerj search engine.
//!
//! ## Architecture overview
//!
//! ```text
//! HTTP body (JSON)
//!      │
//!      ▼
//!  parser::parse_request()   ← ES-compatible query DSL
//!      │
//!      ▼ SearchRequest { query: QueryNode, … }
//!      │
//!  rewriter::rewrite()        ← up to 16 optimisation passes
//!      │
//!      ▼ QueryNode (canonical form)
//!      │
//!  planner::plan_query()     ← QueryNode → ExecutionPlan
//!      │
//!      ▼ ExecutionPlan
//!      │
//!  SegmentExecutor::execute() (per segment, in parallel)
//!      │
//!      ▼ Vec<Hit>  (per segment)
//!      │
//!  executor::merge_hits()    ← global top-K
//!      │
//!      ▼ SearchResult { hits, total, took_ms, aggs }
//! ```
//!
//! ## Why one enum?
//!
//! Elasticsearch's query layer has ~60 Java `Query` classes, each with its own
//! visitor, rewriter, and weight.  xerj collapses this into a single
//! [`QueryNode`] enum.  Rewriters and planners are plain recursive functions —
//! no visitor boilerplate, no dynamic dispatch, no allocation per node type.
//!
//! ## Supported query types
//!
//! All standard Elasticsearch query types are supported:
//!
//! | Category | Types |
//! |---|---|
//! | Full-text | `match`, `match_phrase`, `multi_match`, `query_string` |
//! | Term-level | `term`, `terms`, `range`, `prefix`, `wildcard`, `exists`, `ids` |
//! | Compound | `bool`, `constant_score`, `boosting` |
//! | Special | `match_all`, `match_none` |
//! | AI-native | `knn`, `semantic`, `hybrid` |

pub mod ast;
pub mod dates;
pub mod error;
pub mod executor;
pub mod parser;
pub mod planner;
pub mod rewriter;
pub mod sort;

// ─────────────────────────────────────────────────────────────────────────────
// Public re-exports (the "prelude" for crates that depend on xerj-query)
// ─────────────────────────────────────────────────────────────────────────────

// AST types
pub use ast::{
    BoolOperator, FusionStrategy, MinShouldMatch, MultiMatchType, QueryNode, SearchRequest,
    SourceFilter, WeightedQuery,
};

// Error types
pub use error::{ParseError, QueryError, Result};

// Parser
pub use parser::{parse_query, parse_request};

// Rewriter
pub use rewriter::rewrite;

// Planner
pub use planner::{plan_query, ExecutionPlan};

// Executor
pub use executor::{
    merge_hits, merge_totals, Explanation, Hit, SearchResult, SegmentExecutor, TopKHeap, TotalHits,
    TotalHitsRelation,
};

// Sort
pub use sort::{compare_sort_keys, SortField, SortMissing, SortMode, SortOrder};

// ─────────────────────────────────────────────────────────────────────────────
// High-level convenience API
// ─────────────────────────────────────────────────────────────────────────────

use xerj_common::types::Schema;

/// Parse, rewrite, and plan a query in one call.
///
/// This is the standard entry point used by the REST layer.
///
/// ```rust,ignore
/// let plan = xerj_query::prepare_query(&body, &schema)?;
/// ```
pub fn prepare_query(
    body: &serde_json::Value,
    schema: &Schema,
) -> Result<(SearchRequest, ExecutionPlan)> {
    let req = parse_request(body)?;
    let optimised = rewrite(req.query.clone());
    let plan = plan_query(optimised, schema)?;
    Ok((req, plan))
}
