//! Issue #122, round 2: a clause the parser refuses must fail the query, not
//! disappear from it.
//!
//! `parse_span_or` and `parse_span_near` collected their clauses with
//! `filter_map(|v| parse_query(v).ok())` — a clause the parser rejected was
//! dropped and parsing carried on. That was survivable while every clause the
//! parser could reject was malformed. Adding `MAX_CLAUSE_COUNT` made it a
//! correctness bug: a *well-formed* clause that is merely too wide now fails to
//! parse, so the query it belongs to used to come back 200 OK with that clause
//! silently missing — scored, ranked and returned as though it were the query
//! the caller asked for. `span_near` fared worse still: drop its only clause
//! and the empty list parses to `MatchNone`, i.e. 200 OK and zero hits.
//!
//! These live outside `parser.rs` on purpose. The tests inside it are removed
//! along with the fix when the source file is reverted, so they cannot show
//! that the fix is what makes them pass; these can.

use serde_json::json;
use xerj_query::{parse_query, QueryNode};

/// Comfortably above `MAX_CLAUSE_COUNT` (1,024) without depending on its exact
/// value from outside the crate.
const OVER_THE_CAP: usize = 2_000;

fn bool_too_wide() -> serde_json::Value {
    let clauses: Vec<serde_json::Value> = (0..OVER_THE_CAP)
        .map(|i| json!({ "term": { format!("f{i}"): format!("v{i}") } }))
        .collect();
    json!({ "bool": { "should": clauses } })
}

/// The clause cap must be reachable through a `span_or` clause at all: on its
/// own an over-wide `bool` is refused.
#[test]
fn an_over_wide_bool_is_refused_on_its_own() {
    let err = parse_query(&bool_too_wide()).expect_err("an over-wide bool must be refused");
    assert!(err.to_string().contains("too_many_clauses"), "got {err}");
}

/// A `span_or` whose first clause trips the cap must be refused — not reduced
/// to its surviving clause and answered as a different query.
#[test]
fn a_refused_span_or_clause_fails_the_query_instead_of_vanishing() {
    let query = json!({
        "span_or": {
            "clauses": [
                bool_too_wide(),
                { "span_term": { "body": "x" } }
            ]
        }
    });
    match parse_query(&query) {
        Err(e) => assert!(e.to_string().contains("too_many_clauses"), "got {e}"),
        Ok(QueryNode::SpanOr { clauses }) => panic!(
            "the over-wide clause was DROPPED: the query parsed to a SpanOr of \
             {} clause(s) and would have answered 200 OK for a query nobody asked for",
            clauses.len()
        ),
        Ok(other) => panic!("expected a refusal, got {other:?}"),
    }
}

/// `span_near` with a single over-wide clause: dropping it leaves an empty
/// clause list, which parses to `MatchNone` — 200 OK, zero hits, no error
/// anywhere.
#[test]
fn a_refused_span_near_clause_does_not_become_match_none() {
    let query = json!({
        "span_near": {
            "clauses": [ bool_too_wide() ],
            "slop": 2
        }
    });
    match parse_query(&query) {
        Err(e) => assert!(e.to_string().contains("too_many_clauses"), "got {e}"),
        Ok(QueryNode::MatchNone) => panic!(
            "the over-wide clause was dropped and the query collapsed to \
             MatchNone: 200 OK and zero hits for a query that was never refused"
        ),
        Ok(other) => panic!("expected a refusal, got {other:?}"),
    }
}

/// Not only the cap: any clause the parser cannot understand fails the query
/// rather than vanishing from it.
#[test]
fn an_unparseable_span_clause_fails_the_query() {
    let query = json!({
        "span_or": {
            "clauses": [
                { "span_term": { "body": "x" } },
                { "dis_max": { "queries": "not-an-array" } }
            ]
        }
    });
    assert!(
        parse_query(&query).is_err(),
        "an unparseable span clause was dropped and the query answered without it"
    );
}

/// Width is charged wherever it appears. A span clause list multiplies
/// per-document work exactly as a `bool` clause list does.
#[test]
fn span_clauses_count_towards_the_query_width() {
    let clauses: Vec<serde_json::Value> = (0..OVER_THE_CAP)
        .map(|i| json!({ "span_term": { "body": format!("t{i}") } }))
        .collect();
    let err = parse_query(&json!({ "span_or": { "clauses": clauses } }))
        .expect_err("an over-wide span_or must be refused");
    assert!(err.to_string().contains("too_many_clauses"), "got {err}");
}

/// `dis_max.queries` is a clause list under another name.
#[test]
fn dis_max_queries_count_towards_the_query_width() {
    let queries: Vec<serde_json::Value> = (0..OVER_THE_CAP)
        .map(|i| json!({ "term": { "f": format!("v{i}") } }))
        .collect();
    let err = parse_query(&json!({ "dis_max": { "queries": queries } }))
        .expect_err("an over-wide dis_max must be refused");
    assert!(err.to_string().contains("too_many_clauses"), "got {err}");
}

/// An array `knn.filter` is `bool.filter` spelled differently — ES ANDs it the
/// same way, and it is charged the same way.
#[test]
fn knn_filter_clauses_count_towards_the_query_width() {
    let filter: Vec<serde_json::Value> = (0..OVER_THE_CAP)
        .map(|i| json!({ "term": { "f": format!("v{i}") } }))
        .collect();
    let query = json!({
        "knn": { "field": "vec", "query_vector": [0.1, 0.2], "k": 3, "filter": filter }
    });
    let err = parse_query(&query).expect_err("an over-wide knn.filter must be refused");
    assert!(err.to_string().contains("too_many_clauses"), "got {err}");
}

/// The refusal must not cost ordinary span queries anything: every clause of a
/// well-formed `span_or` still arrives.
#[test]
fn a_well_formed_span_or_keeps_all_its_clauses() {
    let query = json!({
        "span_or": {
            "clauses": [
                { "span_term": { "body": "a" } },
                { "span_term": { "body": "b" } }
            ]
        }
    });
    match parse_query(&query).expect("a well-formed span_or must parse") {
        QueryNode::SpanOr { clauses } => assert_eq!(clauses.len(), 2),
        other => panic!("expected span_or, got {other:?}"),
    }
}

/// And a well-formed `span_near` keeps its slop and ordering.
#[test]
fn a_well_formed_span_near_still_parses() {
    let query = json!({
        "span_near": {
            "clauses": [
                { "span_term": { "body": "quick" } },
                { "span_term": { "body": "fox" } }
            ],
            "slop": 3,
            "in_order": true
        }
    });
    match parse_query(&query).expect("a well-formed span_near must parse") {
        QueryNode::SpanNear {
            clauses,
            slop,
            in_order,
        } => {
            assert_eq!(clauses.len(), 2);
            assert_eq!(slop, 3);
            assert!(in_order);
        }
        other => panic!("expected span_near, got {other:?}"),
    }
}
