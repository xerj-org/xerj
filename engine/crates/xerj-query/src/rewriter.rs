//! Query rewriter / optimizer.
//!
//! Performs up to `MAX_PASSES` rewrites of the query tree, stopping early when
//! a pass produces no changes (convergence).  This is the same strategy ES
//! uses internally (also 16 passes), but here it is a plain recursive
//! function rather than a visitor hierarchy.
//!
//! ## Rewrites applied (in order each pass)
//!
//! 1. **Bool flattening** — `Bool(must=[Bool(must=[A,B])])` → `Bool(must=[A,B])`
//! 2. **MatchAll removal from `must`** — `Bool(must=[MatchAll, X])` → `Bool(must=[X])`
//! 3. **MatchNone removal from `should`** — `Bool(should=[MatchNone, X])` → `Bool(should=[X])`
//! 4. **Constant folding**:
//!    - `Bool(must=[MatchNone, …])` → `MatchNone`
//!    - `Bool(must=[X])` with empty should/must_not/filter → unwrap to `X`
//!    - `Constant(score, MatchAll)` stays as-is (already minimal)
//! 5. **Duplicate removal** in bool clause lists.
//! 6. **Filter push-down** — scoring-free clauses in `must` that are also
//!    present in `filter` are removed from `must`.
//! 7. **Single-clause bool unwrapping** — `Bool(must=[X])` → `X` when no
//!    other clauses exist and no `minimum_should_match`.

use tracing::trace;

use crate::ast::QueryNode;

/// Maximum number of rewrite passes before we give up trying to converge.
const MAX_PASSES: usize = 16;

/// Rewrite a query tree to its canonical, optimal form.
///
/// The returned query is semantically equivalent to the input but may be
/// structurally simpler, which leads to faster execution.
pub fn rewrite(query: QueryNode) -> QueryNode {
    let mut current = query;
    for pass in 1..=MAX_PASSES {
        let (next, changed) = rewrite_once(current);
        current = next;
        if !changed {
            trace!(pass, "query rewriter converged");
            break;
        }
        trace!(pass, "query rewriter pass made changes");
    }
    current
}

/// Single rewrite pass.  Returns `(rewritten_query, did_change)`.
fn rewrite_once(query: QueryNode) -> (QueryNode, bool) {
    match query {
        QueryNode::Bool { mut must, mut should, mut must_not, mut filter, minimum_should_match } => {
            // ── Recurse into children first ──
            let (must2, c1) = rewrite_list(must);
            let (should2, c2) = rewrite_list(should);
            let (must_not2, c3) = rewrite_list(must_not);
            let (filter2, c4) = rewrite_list(filter);
            must = must2;
            should = should2;
            must_not = must_not2;
            filter = filter2;
            let mut changed = c1 || c2 || c3 || c4;

            // ── Short-circuit: any MatchNone in must → whole bool is MatchNone ──
            if must.iter().any(QueryNode::is_match_none) {
                return (QueryNode::MatchNone, true);
            }

            // ── Short-circuit: any MatchNone in filter → MatchNone ──
            if filter.iter().any(QueryNode::is_match_none) {
                return (QueryNode::MatchNone, true);
            }

            // ── Remove MatchAll from must (doesn't restrict anything) ──
            let before = must.len();
            must.retain(|q| !q.is_match_all());
            if must.len() != before {
                changed = true;
            }

            // ── Remove MatchAll from filter ──
            let before = filter.len();
            filter.retain(|q| !q.is_match_all());
            if filter.len() != before {
                changed = true;
            }

            // ── Remove MatchNone from should ──
            let before = should.len();
            should.retain(|q| !q.is_match_none());
            if should.len() != before {
                changed = true;
            }

            // ── Flatten nested Bool::must into parent must ──
            let before = must.len();
            must = flatten_must(must);
            if must.len() != before {
                changed = true;
            }

            // ── Flatten nested Bool::filter into parent filter ──
            let before = filter.len();
            filter = flatten_filter(filter);
            if filter.len() != before {
                changed = true;
            }

            // ── Deduplicate ──
            let (must3, dc1) = dedup(must);
            let (should3, dc2) = dedup(should);
            let (filter3, dc3) = dedup(filter);
            must = must3;
            should = should3;
            filter = filter3;
            if dc1 || dc2 || dc3 {
                changed = true;
            }

            // ── Push duplicate must clauses down to filter ──
            // If a clause appears in both `must` and `filter`, keep it only
            // in `filter` (scoring is already provided by other must clauses).
            let (must4, filter4, pushed) = push_to_filter(must, filter);
            must = must4;
            filter = filter4;
            if pushed {
                changed = true;
            }

            // ── Empty should with no minimum_should_match ──
            // nothing to do; empty should just means "all docs matching must".

            // ── Constant folding: unwrap trivial bools ──
            if must.is_empty() && should.is_empty() && must_not.is_empty() && filter.is_empty() {
                return (QueryNode::MatchAll, true);
            }

            // Unwrap Bool(must=[X]) → X (when no other clauses and no msm)
            if must.len() == 1
                && should.is_empty()
                && must_not.is_empty()
                && filter.is_empty()
                && minimum_should_match.is_none()
            {
                return (must.remove(0), true);
            }

            // Unwrap Bool(filter=[X]) → Constant(0, X)
            // (pure filter — no scoring needed)
            if must.is_empty()
                && should.is_empty()
                && must_not.is_empty()
                && filter.len() == 1
                && minimum_should_match.is_none()
            {
                let inner = filter.remove(0);
                return (
                    QueryNode::Constant { score: 0.0, query: Box::new(inner) },
                    true,
                );
            }

            (
                QueryNode::Bool { must, should, must_not, filter, minimum_should_match },
                changed,
            )
        }

        // ── Constant / Boosted — recurse into child ──
        QueryNode::Constant { score, query } => {
            let (q2, changed) = rewrite_once(*query);
            (QueryNode::Constant { score, query: Box::new(q2) }, changed)
        }
        QueryNode::Boosted { boost, query } => {
            let (q2, changed) = rewrite_once(*query);
            if q2.is_match_none() {
                return (QueryNode::MatchNone, true);
            }
            (QueryNode::Boosted { boost, query: Box::new(q2) }, changed)
        }

        // ── Vector queries — recurse into filter ──
        QueryNode::Knn { field, vector, k, filter, boost } => {
            match filter {
                None => (QueryNode::Knn { field, vector, k, filter: None, boost }, false),
                Some(f) => {
                    let (f2, changed) = rewrite_once(*f);
                    (QueryNode::Knn { field, vector, k, filter: Some(Box::new(f2)), boost }, changed)
                }
            }
        }
        QueryNode::SemanticSearch { field, text, k, filter, boost } => match filter {
            None => (QueryNode::SemanticSearch { field, text, k, filter: None, boost }, false),
            Some(f) => {
                let (f2, changed) = rewrite_once(*f);
                (
                    QueryNode::SemanticSearch { field, text, k, filter: Some(Box::new(f2)), boost },
                    changed,
                )
            }
        },

        // ── Hybrid — recurse into each sub-query ──
        QueryNode::Hybrid { queries, fusion } => {
            let mut changed = false;
            let queries2 = queries
                .into_iter()
                .map(|wq| {
                    let (q2, c) = rewrite_once(wq.query);
                    changed |= c;
                    crate::ast::WeightedQuery { query: q2, weight: wq.weight }
                })
                .collect();
            (QueryNode::Hybrid { queries: queries2, fusion }, changed)
        }

        // ── Leaf nodes — nothing to rewrite ──
        other => (other, false),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn rewrite_list(queries: Vec<QueryNode>) -> (Vec<QueryNode>, bool) {
    let mut changed = false;
    let out = queries
        .into_iter()
        .map(|q| {
            let (q2, c) = rewrite_once(q);
            changed |= c;
            q2
        })
        .collect();
    (out, changed)
}

/// Flatten `Bool(must=[Bool(must=[A,B]), C])` → `[A, B, C]`.
/// Only safe when the nested Bool has no should / must_not / filter clauses
/// and no `minimum_should_match`.
fn flatten_must(clauses: Vec<QueryNode>) -> Vec<QueryNode> {
    let mut out = Vec::with_capacity(clauses.len());
    for q in clauses {
        match q {
            QueryNode::Bool {
                must: inner_must,
                should,
                must_not,
                filter,
                minimum_should_match: None,
            } if should.is_empty() && must_not.is_empty() && filter.is_empty() => {
                out.extend(inner_must);
            }
            other => out.push(other),
        }
    }
    out
}

/// Flatten `Bool(filter=[Bool(filter=[A,B]), C])` → `[A, B, C]`.
fn flatten_filter(clauses: Vec<QueryNode>) -> Vec<QueryNode> {
    let mut out = Vec::with_capacity(clauses.len());
    for q in clauses {
        match q {
            QueryNode::Bool {
                must,
                should,
                must_not,
                filter: inner_filter,
                minimum_should_match: None,
            } if must.is_empty() && should.is_empty() && must_not.is_empty() => {
                out.extend(inner_filter);
            }
            other => out.push(other),
        }
    }
    out
}

/// Remove structurally duplicate query nodes from a clause list.
///
/// Equality is structural (via `PartialEq`), which is correct for term /
/// range / phrase queries.  Match queries may differ only in analyser and
/// still produce identical token sets — we do not deduplicate those (doing so
/// would require invoking the analyser at plan time).
fn dedup(mut clauses: Vec<QueryNode>) -> (Vec<QueryNode>, bool) {
    let before = clauses.len();
    let mut i = 0;
    while i < clauses.len() {
        let mut j = i + 1;
        while j < clauses.len() {
            if clauses[i] == clauses[j] {
                clauses.remove(j);
            } else {
                j += 1;
            }
        }
        i += 1;
    }
    let changed = clauses.len() != before;
    (clauses, changed)
}

/// If a clause exists in both `must` and `filter`, remove it from `must`.
/// The filter clause already enforces the restriction; the must clause's
/// only remaining purpose would be scoring, but if it matches exactly the
/// filter it contributes nothing distinctive.
fn push_to_filter(
    must: Vec<QueryNode>,
    filter: Vec<QueryNode>,
) -> (Vec<QueryNode>, Vec<QueryNode>, bool) {
    if filter.is_empty() {
        return (must, filter, false);
    }
    let mut changed = false;
    let must_out: Vec<QueryNode> = must
        .into_iter()
        .filter(|q| {
            if filter.contains(q) {
                changed = true;
                false
            } else {
                true
            }
        })
        .collect();
    (must_out, filter, changed)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::QueryNode;

    fn term(field: &str, value: &str) -> QueryNode {
        QueryNode::Term {
            field: field.to_string(),
            value: serde_json::json!(value),
            boost: None,
        }
    }

    fn bool_must(clauses: Vec<QueryNode>) -> QueryNode {
        QueryNode::Bool {
            must: clauses,
            should: vec![],
            must_not: vec![],
            filter: vec![],
            minimum_should_match: None,
        }
    }

    fn bool_filter(clauses: Vec<QueryNode>) -> QueryNode {
        QueryNode::Bool {
            must: vec![],
            should: vec![],
            must_not: vec![],
            filter: clauses,
            minimum_should_match: None,
        }
    }

    // ── match_all elimination ─────────────────────────────────────────────────

    #[test]
    fn test_remove_match_all_from_must() {
        let q = bool_must(vec![QueryNode::MatchAll, term("status", "active")]);
        let result = rewrite(q);
        assert_eq!(result, term("status", "active"));
    }

    #[test]
    fn test_match_all_alone_stays() {
        let q = QueryNode::MatchAll;
        let result = rewrite(q);
        assert_eq!(result, QueryNode::MatchAll);
    }

    // ── match_none propagation ────────────────────────────────────────────────

    #[test]
    fn test_match_none_in_must_folds_to_match_none() {
        let q = bool_must(vec![QueryNode::MatchNone, term("x", "y")]);
        assert_eq!(rewrite(q), QueryNode::MatchNone);
    }

    #[test]
    fn test_match_none_removed_from_should() {
        let q = QueryNode::Bool {
            must: vec![],
            should: vec![QueryNode::MatchNone, term("tag", "rust")],
            must_not: vec![],
            filter: vec![],
            minimum_should_match: None,
        };
        // After removing MatchNone from should, we have Bool(should=[term])
        // which then gets unwrapped? No — should single-clause is not unwrapped
        // (would change semantics — should doesn't require a match by default).
        let result = rewrite(q);
        if let QueryNode::Bool { should, .. } = &result {
            assert_eq!(should.len(), 1);
            assert_eq!(should[0], term("tag", "rust"));
        } else {
            panic!("expected Bool, got {result:?}");
        }
    }

    // ── bool flattening ───────────────────────────────────────────────────────

    #[test]
    fn test_flatten_nested_must() {
        // Bool(must=[Bool(must=[A, B])]) → Bool(must=[A, B]) → single clause unwrap → A if len=1
        let a = term("a", "1");
        let b = term("b", "2");
        let inner = bool_must(vec![a.clone(), b.clone()]);
        let outer = bool_must(vec![inner]);
        let result = rewrite(outer);
        // After flattening: Bool(must=[A, B])
        if let QueryNode::Bool { must, .. } = &result {
            assert_eq!(must.len(), 2);
        } else {
            panic!("expected Bool, got {result:?}");
        }
    }

    #[test]
    fn test_single_must_unwrapped() {
        let q = bool_must(vec![term("status", "ok")]);
        assert_eq!(rewrite(q), term("status", "ok"));
    }

    // ── deduplication ─────────────────────────────────────────────────────────

    #[test]
    fn test_dedup_must_clauses() {
        let t = term("status", "active");
        let q = bool_must(vec![t.clone(), t.clone(), term("x", "y")]);
        let result = rewrite(q);
        if let QueryNode::Bool { must, .. } = result {
            assert_eq!(must.len(), 2);
        } else {
            // Could also be unwrapped if only one unique clause remained
            // but here we have two distinct clauses
            panic!("expected Bool");
        }
    }

    // ── filter push-down ──────────────────────────────────────────────────────

    #[test]
    fn test_push_must_to_filter() {
        let t = term("status", "active");
        let q = QueryNode::Bool {
            must: vec![t.clone(), term("title", "rust")],
            should: vec![],
            must_not: vec![],
            filter: vec![t.clone()], // same as first must clause
            minimum_should_match: None,
        };
        let result = rewrite(q);
        if let QueryNode::Bool { must, filter, .. } = result {
            // "status=active" should be in filter only
            assert!(!must.contains(&t));
            assert!(filter.contains(&t));
        } else {
            panic!("expected Bool");
        }
    }

    // ── filter-only bool → Constant(0.0, inner) ──────────────────────────────

    #[test]
    fn test_filter_only_becomes_constant() {
        let q = bool_filter(vec![term("status", "active")]);
        let result = rewrite(q);
        assert!(
            matches!(result, QueryNode::Constant { score, .. } if score == 0.0),
            "expected Constant(0.0, …), got {result:?}"
        );
    }

    // ── empty bool → match_all ────────────────────────────────────────────────

    #[test]
    fn test_empty_bool_becomes_match_all() {
        let q = QueryNode::Bool {
            must: vec![],
            should: vec![],
            must_not: vec![],
            filter: vec![],
            minimum_should_match: None,
        };
        assert_eq!(rewrite(q), QueryNode::MatchAll);
    }

    // ── Boosted(MatchNone) → MatchNone ────────────────────────────────────────

    #[test]
    fn test_boosted_match_none() {
        let q = QueryNode::Boosted {
            boost: 2.0,
            query: Box::new(QueryNode::MatchNone),
        };
        assert_eq!(rewrite(q), QueryNode::MatchNone);
    }

    // ── deep nesting converges ────────────────────────────────────────────────

    #[test]
    fn test_deep_nesting_converges() {
        // Bool(must=[Bool(must=[Bool(must=[term])])])
        let t = term("x", "y");
        let q = bool_must(vec![bool_must(vec![bool_must(vec![t.clone()])])]);
        assert_eq!(rewrite(q), t);
    }
}
