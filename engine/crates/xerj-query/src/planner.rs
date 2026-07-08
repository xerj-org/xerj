//! Query planner — converts a [`QueryNode`] into an [`ExecutionPlan`].
//!
//! The planner answers: *given this query and the index schema, what is the
//! most efficient way to execute it?*
//!
//! ## Cost model (rough, unitless)
//!
//! | Operation | Estimated cost |
//! |---|---|
//! | `MatchAll` / doc scan | 1000 |
//! | `Term` / `Terms` | 1 |
//! | `Ids` | 0.5 |
//! | `Exists` | 2 |
//! | `Prefix` / `Wildcard` | 5 |
//! | `Range` | 3 |
//! | `Match` (FTS) | 5 × tokens |
//! | `MatchPhrase` | 8 |
//! | `MultiMatch` | 5 × fields × tokens |
//! | `QueryString` | 15 |
//! | `Knn` / `SemanticSearch` | 50 × k |
//! | `Bool` | sum of clause costs |
//! | `Constant` / `Boosted` | inner cost |
//! | `Hybrid` | sum of sub-query costs |
//!
//! These are intentionally approximate.  The real optimisation budget is in
//! the rewriter (constant folding, filter push-down) which runs before the
//! planner.

use tracing::trace;
use xerj_common::types::Schema;

use crate::ast::{MultiMatchType, QueryNode};
use crate::error::Result;

// ─────────────────────────────────────────────────────────────────────────────
// ExecutionPlan
// ─────────────────────────────────────────────────────────────────────────────

/// A physical execution plan node.
///
/// The planner maps each `QueryNode` variant to an appropriate plan node.
/// The executor then walks the plan tree to produce hits.
#[derive(Debug, Clone)]
pub enum ExecutionPlan {
    /// Return all documents in segment order (full scan).
    MatchAll,

    /// Return no documents.
    MatchNone,

    /// Term-level lookup via the inverted index.
    FtsScan {
        field: String,
        /// The term(s) to look up.  Single-element vec for `Term`; multi for `Terms`.
        terms: Vec<serde_json::Value>,
        /// Rough cost estimate.
        cost: f64,
    },

    /// Full-text search through the FTS engine (tokenised).
    FtsSearch {
        field: String,
        tokens: Vec<String>,
        require_all: bool,
        /// Phrase mode — tokens must appear in order (with `slop` transpositions).
        phrase: bool,
        slop: u32,
        cost: f64,
    },

    /// Range query on a numeric / date / keyword field.
    RangeScan {
        field: String,
        gte: Option<serde_json::Value>,
        gt: Option<serde_json::Value>,
        lte: Option<serde_json::Value>,
        lt: Option<serde_json::Value>,
        cost: f64,
    },

    /// Prefix / wildcard scan via the FST.
    PrefixScan {
        field: String,
        prefix: String,
        wildcard: bool,
        cost: f64,
    },

    /// Existence filter — checks the null-bitmap.
    ExistsScan { field: String },

    /// Fetch by document IDs — direct random access.
    IdLookup { ids: Vec<String> },

    /// Dense-vector k-NN via the HNSW index.
    VectorScan {
        field: String,
        vector: Vec<f32>,
        k: usize,
        filter: Option<Box<ExecutionPlan>>,
        cost: f64,
    },

    /// Semantic (text → embedding → k-NN) search.
    SemanticScan {
        field: String,
        text: String,
        k: usize,
        filter: Option<Box<ExecutionPlan>>,
        cost: f64,
    },

    /// Boolean combination of sub-plans.
    BoolCombine {
        must: Vec<ExecutionPlan>,
        should: Vec<ExecutionPlan>,
        must_not: Vec<ExecutionPlan>,
        filter: Vec<ExecutionPlan>,
        minimum_should_match: Option<crate::ast::MinShouldMatch>,
        cost: f64,
    },

    /// Apply a fixed score to all hits from the inner plan.
    ConstantScore {
        score: f32,
        inner: Box<ExecutionPlan>,
        cost: f64,
    },

    /// Multiply hit scores by `boost`.
    Boost {
        boost: f32,
        inner: Box<ExecutionPlan>,
        cost: f64,
    },

    /// Merge results from multiple plans using a score-fusion strategy.
    HybridMerge {
        plans: Vec<(ExecutionPlan, f32)>,
        fusion: crate::ast::FusionStrategy,
        cost: f64,
    },
}

impl ExecutionPlan {
    /// Return the estimated cost of this plan node.
    pub fn cost(&self) -> f64 {
        match self {
            ExecutionPlan::MatchAll => 1000.0,
            ExecutionPlan::MatchNone => 0.0,
            ExecutionPlan::FtsScan { cost, .. } => *cost,
            ExecutionPlan::FtsSearch { cost, .. } => *cost,
            ExecutionPlan::RangeScan { cost, .. } => *cost,
            ExecutionPlan::PrefixScan { cost, .. } => *cost,
            ExecutionPlan::ExistsScan { .. } => 2.0,
            ExecutionPlan::IdLookup { ids } => ids.len() as f64 * 0.5,
            ExecutionPlan::VectorScan { cost, .. } => *cost,
            ExecutionPlan::SemanticScan { cost, .. } => *cost,
            ExecutionPlan::BoolCombine { cost, .. } => *cost,
            ExecutionPlan::ConstantScore { cost, .. } => *cost,
            ExecutionPlan::Boost { cost, .. } => *cost,
            ExecutionPlan::HybridMerge { cost, .. } => *cost,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Planner entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Plan a (already-rewritten) query against the given schema.
///
/// The schema is used to pick the right scan strategy (e.g. use the BKD tree
/// for numeric ranges, use the vector index for `knn` fields).
pub fn plan_query(query: QueryNode, schema: &Schema) -> Result<ExecutionPlan> {
    let plan = plan_node(query, schema);
    trace!(cost = plan.cost(), "query plan created");
    Ok(plan)
}

// `schema` is threaded through only so recursive sub-plans can consult it;
// the current arms don't read it directly, so silence the recursion lint.
#[allow(clippy::only_used_in_recursion)]
fn plan_node(query: QueryNode, schema: &Schema) -> ExecutionPlan {
    match query {
        QueryNode::MatchAll => ExecutionPlan::MatchAll,
        QueryNode::MatchNone => ExecutionPlan::MatchNone,

        QueryNode::Term { field, value, .. } => ExecutionPlan::FtsScan {
            cost: 1.0,
            terms: vec![value],
            field,
        },

        QueryNode::Terms { field, values, .. } => {
            let cost = values.len() as f64;
            ExecutionPlan::FtsScan {
                field,
                terms: values,
                cost,
            }
        }

        QueryNode::Ids { values } => ExecutionPlan::IdLookup { ids: values },

        QueryNode::Exists { field } => ExecutionPlan::ExistsScan { field },

        QueryNode::Range {
            field,
            gte,
            gt,
            lte,
            lt,
            ..
        } => ExecutionPlan::RangeScan {
            field,
            gte,
            gt,
            lte,
            lt,
            cost: 3.0,
        },

        QueryNode::Prefix { field, value, .. } => ExecutionPlan::PrefixScan {
            field,
            prefix: value,
            wildcard: false,
            cost: 5.0,
        },

        QueryNode::Wildcard { field, value, .. } => ExecutionPlan::PrefixScan {
            field,
            prefix: value,
            wildcard: true,
            cost: 5.0,
        },

        QueryNode::Match {
            field,
            query,
            operator,
            ..
        } => {
            // Simple whitespace tokenisation for cost estimation.
            // Actual tokenisation happens in the FTS engine.
            let tokens: Vec<String> = query.split_whitespace().map(str::to_string).collect();
            let n = tokens.len().max(1);
            let require_all = matches!(operator, crate::ast::BoolOperator::And);
            ExecutionPlan::FtsSearch {
                field,
                tokens,
                require_all,
                phrase: false,
                slop: 0,
                cost: 5.0 * n as f64,
            }
        }

        QueryNode::MatchPhrase {
            field, query, slop, ..
        } => {
            let tokens: Vec<String> = query.split_whitespace().map(str::to_string).collect();
            ExecutionPlan::FtsSearch {
                cost: 8.0,
                field,
                tokens,
                require_all: true,
                phrase: true,
                slop,
            }
        }

        QueryNode::MultiMatch {
            fields,
            query,
            match_type,
            ..
        } => {
            // Expand to a Bool(should=[ Match(field, query) for field in fields ])
            // and plan that.
            let tokens: Vec<String> = query.split_whitespace().map(str::to_string).collect();
            let n = tokens.len().max(1);
            let phrase = matches!(
                match_type,
                MultiMatchType::Phrase | MultiMatchType::PhrasePrefix
            );
            let cost = 5.0 * fields.len() as f64 * n as f64;

            let sub_plans: Vec<ExecutionPlan> = fields
                .into_iter()
                .map(|field| ExecutionPlan::FtsSearch {
                    field,
                    tokens: tokens.clone(),
                    require_all: false,
                    phrase,
                    slop: 0,
                    cost: 5.0 * n as f64,
                })
                .collect();

            ExecutionPlan::BoolCombine {
                must: vec![],
                should: sub_plans,
                must_not: vec![],
                filter: vec![],
                minimum_should_match: None,
                cost,
            }
        }

        QueryNode::QueryString { query, .. } => {
            // Fallback only: the common case is lowered into a Bool tree by
            // try_lower_query_string (parser.rs), so this branch is reached
            // only for inputs that could not be translated. Split the raw
            // query on whitespace and OR the tokens across `_all` rather than
            // searching for the entire raw string as one opaque token (which
            // would never match). This is an approximation — operators
            // (AND/OR/NOT, +/-, parens, phrases, field:value) are not honored
            // on this path — but it yields sensible hits instead of none.
            let tokens: Vec<String> = query.split_whitespace().map(str::to_string).collect();
            let cost = 15.0 * tokens.len().max(1) as f64;
            ExecutionPlan::FtsSearch {
                field: "_all".to_string(),
                tokens,
                require_all: false,
                phrase: false,
                slop: 0,
                cost,
            }
        }

        QueryNode::Bool {
            must,
            should,
            must_not,
            filter,
            minimum_should_match,
        } => {
            // Plan each clause, then sort must/should by cost (cheapest first).
            let plan_list = |clauses: Vec<QueryNode>| {
                let mut plans: Vec<ExecutionPlan> =
                    clauses.into_iter().map(|q| plan_node(q, schema)).collect();
                plans.sort_by(|a, b| a.cost().partial_cmp(&b.cost()).unwrap());
                plans
            };

            let must_plans = plan_list(must);
            let should_plans = plan_list(should);
            let must_not_plans = plan_list(must_not);
            let filter_plans = plan_list(filter);

            let cost = must_plans.iter().map(|p| p.cost()).sum::<f64>()
                + should_plans.iter().map(|p| p.cost()).sum::<f64>()
                + filter_plans.iter().map(|p| p.cost()).sum::<f64>();

            ExecutionPlan::BoolCombine {
                must: must_plans,
                should: should_plans,
                must_not: must_not_plans,
                filter: filter_plans,
                minimum_should_match,
                cost,
            }
        }

        QueryNode::Constant { score, query } => {
            let inner = plan_node(*query, schema);
            let cost = inner.cost();
            ExecutionPlan::ConstantScore {
                score,
                inner: Box::new(inner),
                cost,
            }
        }

        QueryNode::Boosted { boost, query } => {
            let inner = plan_node(*query, schema);
            let cost = inner.cost();
            ExecutionPlan::Boost {
                boost,
                inner: Box::new(inner),
                cost,
            }
        }

        QueryNode::Knn {
            field,
            vector,
            k,
            filter,
            ..
        } => {
            let filter_plan = filter.map(|f| Box::new(plan_node(*f, schema)));
            let cost = 50.0 * k as f64;
            ExecutionPlan::VectorScan {
                field,
                vector,
                k,
                filter: filter_plan,
                cost,
            }
        }

        QueryNode::SemanticSearch {
            field,
            text,
            k,
            filter,
            ..
        } => {
            let filter_plan = filter.map(|f| Box::new(plan_node(*f, schema)));
            let cost = 50.0 * k as f64 + 20.0; // +20 for embedding inference
            ExecutionPlan::SemanticScan {
                field,
                text,
                k,
                filter: filter_plan,
                cost,
            }
        }

        QueryNode::Hybrid { queries, fusion } => {
            let mut total_cost = 0.0;
            let plans: Vec<(ExecutionPlan, f32)> = queries
                .into_iter()
                .map(|wq| {
                    let p = plan_node(wq.query, schema);
                    total_cost += p.cost();
                    (p, wq.weight)
                })
                .collect();
            ExecutionPlan::HybridMerge {
                plans,
                fusion,
                cost: total_cost,
            }
        }

        // Boosting: plan the positive sub-query; negative is evaluated at score time.
        QueryNode::Boosting {
            positive,
            negative,
            negative_boost,
        } => {
            let pos_plan = plan_node(*positive, schema);
            let neg_plan = plan_node(*negative, schema);
            let cost = pos_plan.cost() + neg_plan.cost();
            // Represent as a Bool must + boosted should for the planner.
            ExecutionPlan::BoolCombine {
                must: vec![pos_plan],
                should: vec![ExecutionPlan::Boost {
                    boost: negative_boost,
                    inner: Box::new(neg_plan),
                    cost: 0.0,
                }],
                must_not: vec![],
                filter: vec![],
                minimum_should_match: None,
                cost,
            }
        }

        // DisMax: plan all sub-queries; execution picks the max score.
        QueryNode::DisMax { queries, .. } => {
            let mut total_cost = 0.0;
            let sub_plans: Vec<ExecutionPlan> = queries
                .into_iter()
                .map(|q| {
                    let p = plan_node(q, schema);
                    total_cost += p.cost();
                    p
                })
                .collect();
            // Treat like a Bool-should for planning purposes.
            ExecutionPlan::BoolCombine {
                must: vec![],
                should: sub_plans,
                must_not: vec![],
                filter: vec![],
                minimum_should_match: Some(crate::ast::MinShouldMatch::Fixed(1)),
                cost: total_cost,
            }
        }

        // Fuzzy: treat as a term scan with higher cost due to edit-distance computation.
        QueryNode::Fuzzy { field, value, .. } => ExecutionPlan::FtsScan {
            field,
            terms: vec![serde_json::Value::String(value)],
            cost: 10.0,
        },

        // Regexp: full scan cost since we must evaluate each doc.
        QueryNode::Regexp { field, pattern } => ExecutionPlan::FtsScan {
            field,
            terms: vec![serde_json::Value::String(pattern)],
            cost: 20.0,
        },

        // Intervals: position-aware text match. Plan as an exists-scan
        // on the field — the doc-scan executor evaluates the rule
        // directly against tokens (see engine::doc_matches_query).
        QueryNode::Intervals { field, .. } => ExecutionPlan::ExistsScan { field },

        // MatchPhrasePrefix: treat as a phrase search on the given field.
        QueryNode::MatchPhrasePrefix { field, query, .. } => {
            let tokens: Vec<String> = query.split_whitespace().map(str::to_string).collect();
            ExecutionPlan::FtsSearch {
                field,
                tokens,
                require_all: true,
                phrase: true,
                slop: 0,
                cost: 10.0,
            }
        }

        // SimpleQueryString: resolved to Bool at parse time; fallback to full scan.
        QueryNode::SimpleQueryString { query, .. } => ExecutionPlan::FtsSearch {
            field: "_all".to_string(),
            tokens: vec![query],
            require_all: false,
            phrase: false,
            slop: 0,
            cost: 15.0,
        },

        // GeoDistance: full doc-scan with per-doc haversine computation.
        QueryNode::GeoDistance { field, .. } => ExecutionPlan::ExistsScan { field },

        // GeoBoundingBox: full doc-scan with per-doc bounding box check.
        QueryNode::GeoBoundingBox { field, .. } => ExecutionPlan::ExistsScan { field },

        // FunctionScore: plan the inner query; function application happens at score time.
        QueryNode::FunctionScore { query, .. } => {
            let inner = plan_node(*query, schema);
            let cost = inner.cost() + 5.0; // small overhead for function application
            ExecutionPlan::Boost {
                boost: 1.0,
                inner: Box::new(inner),
                cost,
            }
        }

        // Nested: plan the inner query with a doc-scan cost.
        QueryNode::Nested { query, .. } => {
            let inner = plan_node(*query, schema);
            let cost = inner.cost() + 10.0; // array traversal overhead
            ExecutionPlan::Boost {
                boost: 1.0,
                inner: Box::new(inner),
                cost,
            }
        }

        // MoreLikeThis: treat as a multi-term FTS search.
        QueryNode::MoreLikeThis { like, .. } => {
            let all_text = like.join(" ");
            let tokens: Vec<String> = all_text.split_whitespace().map(str::to_string).collect();
            let cost = 5.0 * tokens.len().max(1) as f64;
            ExecutionPlan::FtsSearch {
                field: "_all".to_string(),
                tokens,
                require_all: false,
                phrase: false,
                slop: 0,
                cost,
            }
        }

        // Pinned: plan the organic query; pinned ID boosting happens at score time.
        QueryNode::Pinned { organic, ids } => {
            // Include the organic plan plus an id-lookup for the pinned IDs.
            let organic_plan = plan_node(*organic, schema);
            let pin_cost = ids.len() as f64 * 0.5;
            let total_cost = organic_plan.cost() + pin_cost;
            ExecutionPlan::BoolCombine {
                must: vec![],
                should: vec![organic_plan, ExecutionPlan::IdLookup { ids }],
                must_not: vec![],
                filter: vec![],
                minimum_should_match: None,
                cost: total_cost,
            }
        }

        // Named: plan the inner query; name annotation has no effect on planning.
        QueryNode::Named { query, .. } => plan_node(*query, schema),

        // Percolate: pure doc-scan (reverse match). The plan is ignored by the
        // engine's doc-scan path; MatchAll satisfies exhaustiveness here.
        QueryNode::Percolate { .. } => ExecutionPlan::MatchAll,

        // ── Span queries — treat as doc-scans ────────────────────────────────
        QueryNode::SpanTerm { field, value } => ExecutionPlan::FtsScan {
            field,
            terms: vec![serde_json::Value::String(value)],
            cost: 2.0,
        },

        QueryNode::SpanNear { clauses, .. } => {
            let sub_plans: Vec<ExecutionPlan> =
                clauses.into_iter().map(|q| plan_node(q, schema)).collect();
            let cost = sub_plans.iter().map(|p| p.cost()).sum::<f64>() + 5.0;
            ExecutionPlan::BoolCombine {
                must: sub_plans,
                should: vec![],
                must_not: vec![],
                filter: vec![],
                minimum_should_match: None,
                cost,
            }
        }

        QueryNode::SpanOr { clauses } => {
            let sub_plans: Vec<ExecutionPlan> =
                clauses.into_iter().map(|q| plan_node(q, schema)).collect();
            let cost = sub_plans.iter().map(|p| p.cost()).sum::<f64>();
            ExecutionPlan::BoolCombine {
                must: vec![],
                should: sub_plans,
                must_not: vec![],
                filter: vec![],
                minimum_should_match: Some(crate::ast::MinShouldMatch::Fixed(1)),
                cost,
            }
        }

        QueryNode::SpanNot { include, exclude } => {
            let inc = plan_node(*include, schema);
            let exc = plan_node(*exclude, schema);
            let cost = inc.cost() + exc.cost();
            ExecutionPlan::BoolCombine {
                must: vec![inc],
                should: vec![],
                must_not: vec![exc],
                filter: vec![],
                minimum_should_match: None,
                cost,
            }
        }

        QueryNode::SpanFirst { match_query, .. } => plan_node(*match_query, schema),

        // ── Join queries — UNREACHABLE ────────────────────────────────────────
        // has_child/has_parent are rejected with a 400 at parse time
        // (see parser.rs::parse_has_child), so these AST variants are never
        // built and this branch is never taken. Kept for exhaustiveness and
        // in case the AST variant is ever wired to a real join executor.
        QueryNode::HasChild { query, .. } | QueryNode::HasParent { query, .. } => {
            let inner = plan_node(*query, schema);
            let cost = inner.cost() + 10.0;
            ExecutionPlan::Boost {
                boost: 1.0,
                inner: Box::new(inner),
                cost,
            }
        }

        // ── Geo shape queries — treat as doc-scans ────────────────────────────
        QueryNode::GeoPolygon { field, .. } => ExecutionPlan::ExistsScan { field },
        QueryNode::GeoShape { field, .. } => ExecutionPlan::ExistsScan { field },
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use xerj_common::types::Schema;

    fn empty_schema() -> Schema {
        Schema::empty()
    }

    fn term_node(field: &str, val: &str) -> QueryNode {
        QueryNode::Term {
            field: field.to_string(),
            value: serde_json::json!(val),
            boost: None,
        }
    }

    #[test]
    fn test_plan_match_all() {
        let plan = plan_query(QueryNode::MatchAll, &empty_schema()).unwrap();
        assert!(matches!(plan, ExecutionPlan::MatchAll));
    }

    #[test]
    fn test_plan_match_none() {
        let plan = plan_query(QueryNode::MatchNone, &empty_schema()).unwrap();
        assert!(matches!(plan, ExecutionPlan::MatchNone));
    }

    #[test]
    fn test_plan_term() {
        let plan = plan_query(term_node("status", "active"), &empty_schema()).unwrap();
        assert!(matches!(plan, ExecutionPlan::FtsScan { cost, .. } if cost == 1.0));
    }

    #[test]
    fn test_plan_terms() {
        let q = QueryNode::Terms {
            field: "tag".to_string(),
            values: vec![
                serde_json::json!("rust"),
                serde_json::json!("go"),
                serde_json::json!("zig"),
            ],
            boost: None,
        };
        let plan = plan_query(q, &empty_schema()).unwrap();
        assert!(matches!(plan, ExecutionPlan::FtsScan { cost, .. } if cost == 3.0));
    }

    #[test]
    fn test_plan_range() {
        let q = QueryNode::Range {
            field: "age".to_string(),
            gte: Some(serde_json::json!(18)),
            gt: None,
            lte: None,
            lt: Some(serde_json::json!(65)),
            boost: None,
        };
        let plan = plan_query(q, &empty_schema()).unwrap();
        assert!(matches!(plan, ExecutionPlan::RangeScan { .. }));
    }

    #[test]
    fn test_plan_bool_orders_by_cost() {
        // must=[knn(cost=500), term(cost=1)] → term should come first
        let q = QueryNode::Bool {
            must: vec![
                QueryNode::Knn {
                    field: "vec".to_string(),
                    vector: vec![0.1],
                    k: 10,
                    filter: None,
                    boost: None,
                },
                term_node("status", "ok"),
            ],
            should: vec![],
            must_not: vec![],
            filter: vec![],
            minimum_should_match: None,
        };
        let plan = plan_query(q, &empty_schema()).unwrap();
        if let ExecutionPlan::BoolCombine { must, .. } = plan {
            assert_eq!(must.len(), 2);
            // term (cost=1) should be cheaper than knn (cost=500)
            assert!(must[0].cost() < must[1].cost());
        } else {
            panic!("expected BoolCombine");
        }
    }

    #[test]
    fn test_plan_constant_score() {
        let q = QueryNode::Constant {
            score: 1.5,
            query: Box::new(term_node("status", "active")),
        };
        let plan = plan_query(q, &empty_schema()).unwrap();
        assert!(
            matches!(plan, ExecutionPlan::ConstantScore { score, .. } if (score - 1.5).abs() < 0.001)
        );
    }

    #[test]
    fn test_plan_knn() {
        let q = QueryNode::Knn {
            field: "embedding".to_string(),
            vector: vec![0.1, 0.2, 0.3],
            k: 20,
            filter: None,
            boost: None,
        };
        let plan = plan_query(q, &empty_schema()).unwrap();
        assert!(matches!(plan, ExecutionPlan::VectorScan { k: 20, .. }));
    }

    #[test]
    fn test_plan_hybrid() {
        let q = QueryNode::Hybrid {
            queries: vec![
                crate::ast::WeightedQuery {
                    query: term_node("x", "y"),
                    weight: 0.5,
                },
                crate::ast::WeightedQuery {
                    query: QueryNode::Knn {
                        field: "vec".to_string(),
                        vector: vec![1.0],
                        k: 5,
                        filter: None,
                        boost: None,
                    },
                    weight: 0.5,
                },
            ],
            fusion: crate::ast::FusionStrategy::Rrf { k: 60 },
        };
        let plan = plan_query(q, &empty_schema()).unwrap();
        assert!(matches!(plan, ExecutionPlan::HybridMerge { .. }));
    }
}
