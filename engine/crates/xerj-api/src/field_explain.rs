//! Per-field score breakdown for the native search API (`explain: true` on
//! `POST /v1/indices/{name}/search`, issue #1798).
//!
//! Each hit gets a `_score_breakdown`: how much of its `_score` came from
//! each field the query scored. The numbers are not estimated. For every
//! field the query touches, the query is PROJECTED onto that field (every
//! scoring clause on another field removed), and the projection is run again
//! by the engine, restricted to the returned hit ids. A field's number is
//! therefore the engine's own score for "this query, on this field only",
//! with the same analyzers, statistics and boosts the full query used.
//!
//! Same idea as tantivy's `Weight::explain` (re-score one doc per sub-weight,
//! `BooleanWeight::explain` sums the included clauses:
//! tantivy `src/query/boolean_query/boolean_weight.rs:483-501`, MIT) and
//! meilisearch's per-rule `_rankingScoreDetails`; approach only, no code
//! copied. The ES `_explain` endpoint and `explain` on `_search` are a
//! separate path and are not changed by this module.
//!
//! `combine` says how the field scores relate to `_score`:
//! - `"sum"`: the query adds field contributions (`bool`, `match`,
//!   `most_fields`), so
//!   `_score == Σ fields + unattributed`. `unattributed` is the part no field
//!   owns (a `match_all`, a kNN clause, a `constant_score` over several
//!   fields) plus float rounding.
//! - `"other"`: the query combines fields some other way (`best_fields` /
//!   `dis_max` take the max, `function_score` and `boosting` multiply,
//!   `cross_fields` blends statistics). A field-less `q` (`multi_match` on
//!   `*`) and each unqualified `query_string` term are `best_fields` over
//!   every field `*` expands onto, so they report `"other"` by that same rule
//!   once more than one field is mapped. Field scores are still each field's
//!   own score, but they do not add up to `_score`, so no `unattributed` is
//!   reported.

use std::collections::HashMap;

use serde_json::{json, Value};
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_query::ast::{MultiMatchType, QueryNode};
use xerj_query::executor::Hit;

/// Upper bound on the fields one request re-scores (one engine search each).
pub const MAX_EXPLAIN_FIELDS: usize = 64;

/// The fields a field-less clause expands onto, as the engine expands it.
pub struct Fields {
    /// Searchable `text` fields: what an opaque field-less `query_string` and
    /// an empty `multi_match` field list are scored over.
    text: Vec<String>,
    /// Searchable `text` and `keyword` fields: what a `*` / `prefix*` field
    /// pattern resolves to (`q`, an unqualified `query_string` term,
    /// `multi_match` on `*`; #3413, `expand_field_specs` in the engine).
    pattern: Vec<String>,
}

impl Fields {
    pub fn from_schema(schema: &Schema) -> Self {
        fn searchable(f: &FieldConfig) -> bool {
            (f.options.indexed || f.options.doc_values) && !f.name.starts_with('_')
        }
        // `object` properties by dotted path, as the engine's pattern
        // expansion walks them; `nested` properties and multi-fields are not
        // pattern targets.
        fn walk(fields: &[FieldConfig], prefix: &str, out: &mut Vec<String>) {
            for f in fields {
                let path = if prefix.is_empty() {
                    f.name.clone()
                } else {
                    format!("{prefix}.{}", f.name)
                };
                match f.field_type {
                    FieldType::Object => walk(&f.fields, &path, out),
                    FieldType::Text | FieldType::Keyword if searchable(f) => out.push(path),
                    _ => {}
                }
            }
        }
        let mut pattern = Vec::new();
        walk(&schema.fields, "", &mut pattern);
        Fields {
            text: schema
                .fields
                .iter()
                .filter(|f| matches!(f.field_type, FieldType::Text) && searchable(f))
                .map(|f| f.name.clone())
                .collect(),
            pattern,
        }
    }
}

fn split_boost(spec: &str) -> (&str, Option<&str>) {
    match spec.rfind('^') {
        Some(pos) => (&spec[..pos], Some(&spec[pos + 1..])),
        None => (spec, None),
    }
}

/// `*` glob over a field name, `*` matching any run of characters (ES
/// simple-match patterns: `*`, `title*`, `*_text`).
fn pattern_matches(pattern: &str, field: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or("");
    let Some(mut rest) = field.strip_prefix(first) else {
        return false;
    };
    let tail: Vec<&str> = parts.collect();
    let Some((last, middle)) = tail.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

/// Concrete fields a multi-field spec list names. Empty means every text
/// field; a pattern resolves over `fields.pattern`.
fn expand_specs(specs: &[String], fields: &Fields) -> Vec<(String, Option<String>)> {
    if specs.is_empty() {
        return fields.text.iter().map(|f| (f.clone(), None)).collect();
    }
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for spec in specs {
        let (name, boost) = split_boost(spec);
        if name.contains('*') {
            for f in fields.pattern.iter().filter(|f| pattern_matches(name, f)) {
                if !out.iter().any(|(o, _)| o == f) {
                    out.push((f.clone(), boost.map(str::to_string)));
                }
            }
        } else if !out.iter().any(|(o, _)| o == name) {
            out.push((name.to_string(), boost.map(str::to_string)));
        }
    }
    out
}

fn with_boost(field: &str, boost: &Option<String>) -> String {
    match boost {
        Some(b) => format!("{field}^{b}"),
        None => field.to_string(),
    }
}

/// `query_string` text that names fields itself (`title:foo`) cannot be
/// projected by swapping `default_field`.
fn query_string_is_field_free(query: &str) -> bool {
    !query.contains(':')
}

/// The single field a leaf clause scores on, if it is a single-field leaf.
fn leaf_field(q: &QueryNode) -> Option<&str> {
    match q {
        QueryNode::Match { field, .. }
        | QueryNode::MatchPhrase { field, .. }
        | QueryNode::MatchPhrasePrefix { field, .. }
        | QueryNode::Term { field, .. }
        | QueryNode::Terms { field, .. }
        | QueryNode::Range { field, .. }
        | QueryNode::Prefix { field, .. }
        | QueryNode::Wildcard { field, .. }
        | QueryNode::Fuzzy { field, .. }
        | QueryNode::Regexp { field, .. }
        | QueryNode::Exists { field }
        | QueryNode::Intervals { field, .. }
        | QueryNode::SpanTerm { field, .. } => Some(field.as_str()),
        _ => None,
    }
    .filter(|f| !f.contains('*'))
}

/// A field-less `match` / `match_phrase` (the parser's `"*"` placeholder for
/// an unqualified clause) and the field pattern it carries.
fn wildcard_leaf(q: &QueryNode) -> Option<&str> {
    match q {
        QueryNode::Match { field, .. } | QueryNode::MatchPhrase { field, .. }
            if field.contains('*') =>
        {
            Some(field.as_str())
        }
        _ => None,
    }
}

fn retarget(q: &QueryNode, to: &str) -> QueryNode {
    let mut out = q.clone();
    if let QueryNode::Match { field, .. } | QueryNode::MatchPhrase { field, .. } = &mut out {
        *field = to.to_string();
    }
    out
}

/// Every field the query scores on, in first-seen order.
pub fn scoring_fields(q: &QueryNode, fields: &Fields) -> Vec<String> {
    let mut out = Vec::new();
    collect_fields(q, fields, &mut out);
    out
}

fn push_unique(out: &mut Vec<String>, f: &str) {
    if !out.iter().any(|o| o == f) {
        out.push(f.to_string());
    }
}

fn collect_fields(q: &QueryNode, cx: &Fields, out: &mut Vec<String>) {
    if let Some(f) = leaf_field(q) {
        push_unique(out, f);
        return;
    }
    if let Some(pattern) = wildcard_leaf(q) {
        for f in cx.pattern.iter().filter(|f| pattern_matches(pattern, f)) {
            push_unique(out, f);
        }
        return;
    }
    match q {
        QueryNode::MultiMatch { fields, .. } | QueryNode::SimpleQueryString { fields, .. } => {
            for (f, _) in expand_specs(fields, cx) {
                push_unique(out, &f);
            }
        }
        QueryNode::QueryString {
            query,
            default_field,
            ..
        } if query_string_is_field_free(query) => match default_field.as_deref() {
            Some(f) if f != "*" => push_unique(out, f),
            _ => {
                for f in &cx.text {
                    push_unique(out, f);
                }
            }
        },
        QueryNode::Bool { must, should, .. } => {
            for c in must.iter().chain(should) {
                collect_fields(c, cx, out);
            }
        }
        QueryNode::DisMax { queries, .. } => {
            for c in queries {
                collect_fields(c, cx, out);
            }
        }
        QueryNode::Boosted { query, .. } | QueryNode::Named { query, .. } => {
            collect_fields(query, cx, out)
        }
        QueryNode::Constant { query, .. } => {
            let inner = scoring_fields(query, cx);
            if inner.len() == 1 {
                push_unique(out, &inner[0]);
            }
        }
        QueryNode::Boosting { positive, .. } => collect_fields(positive, cx, out),
        _ => {}
    }
}

/// `q` with every scoring clause on a field other than `field` removed, or
/// `None` when nothing in `q` scores on `field`.
pub fn project_onto_field(q: &QueryNode, field: &str, cx: &Fields) -> Option<QueryNode> {
    if let Some(f) = leaf_field(q) {
        return (f == field).then(|| q.clone());
    }
    if let Some(pattern) = wildcard_leaf(q) {
        let targets = cx.pattern.iter().any(|f| f == field) && pattern_matches(pattern, field);
        return targets.then(|| retarget(q, field));
    }
    match q {
        QueryNode::MultiMatch { fields, .. } => {
            let kept: Vec<String> = expand_specs(fields, cx)
                .iter()
                .filter(|(f, _)| f == field)
                .map(|(f, b)| with_boost(f, b))
                .collect();
            if kept.is_empty() {
                return None;
            }
            let mut projected = q.clone();
            if let QueryNode::MultiMatch { fields, .. } = &mut projected {
                *fields = kept;
            }
            Some(projected)
        }
        QueryNode::SimpleQueryString { query, fields } => {
            let kept: Vec<String> = expand_specs(fields, cx)
                .iter()
                .filter(|(f, _)| f == field)
                .map(|(f, b)| with_boost(f, b))
                .collect();
            (!kept.is_empty()).then(|| QueryNode::SimpleQueryString {
                query: query.clone(),
                fields: kept,
            })
        }
        QueryNode::QueryString {
            query,
            default_field,
            default_operator,
            boost,
        } if query_string_is_field_free(query) => {
            let targets = match default_field.as_deref() {
                Some(f) if f != "*" => f == field,
                _ => cx.text.iter().any(|f| f == field),
            };
            targets.then(|| QueryNode::QueryString {
                query: query.clone(),
                default_field: Some(field.to_string()),
                default_operator: *default_operator,
                boost: *boost,
            })
        }
        QueryNode::Bool { must, should, .. } => {
            let mut kept: Vec<QueryNode> = must
                .iter()
                .chain(should)
                .filter_map(|c| project_onto_field(c, field, cx))
                .collect();
            match kept.len() {
                0 => None,
                1 => kept.pop(),
                _ => Some(QueryNode::Bool {
                    must: Vec::new(),
                    should: kept,
                    must_not: Vec::new(),
                    filter: Vec::new(),
                    minimum_should_match: None,
                }),
            }
        }
        QueryNode::DisMax {
            queries,
            tie_breaker,
        } => {
            let mut kept: Vec<QueryNode> = queries
                .iter()
                .filter_map(|c| project_onto_field(c, field, cx))
                .collect();
            match kept.len() {
                0 => None,
                1 => kept.pop(),
                _ => Some(QueryNode::DisMax {
                    queries: kept,
                    tie_breaker: *tie_breaker,
                }),
            }
        }
        QueryNode::Boosted { boost, query } => {
            project_onto_field(query, field, cx).map(|inner| QueryNode::Boosted {
                boost: *boost,
                query: Box::new(inner),
            })
        }
        QueryNode::Named { query, .. } => project_onto_field(query, field, cx),
        QueryNode::Constant { query, .. } => {
            let inner = scoring_fields(query, cx);
            (inner.len() == 1 && inner[0] == field).then(|| q.clone())
        }
        QueryNode::Boosting { positive, .. } => project_onto_field(positive, field, cx),
        _ => None,
    }
}

/// Whether `q` adds its field contributions, so that
/// `_score == Σ field scores + unattributed`.
pub fn is_additive(q: &QueryNode, cx: &Fields) -> bool {
    if leaf_field(q).is_some() {
        return true;
    }
    // The engine turns a pattern leaf into a `best_fields` `multi_match` over
    // the fields the pattern names (#3413): additive only when that is one
    // field, the same rule as an explicit `multi_match` below.
    if let Some(pattern) = wildcard_leaf(q) {
        return cx
            .pattern
            .iter()
            .filter(|f| pattern_matches(pattern, f))
            .count()
            <= 1;
    }
    match q {
        QueryNode::MatchAll
        | QueryNode::MatchNone
        | QueryNode::Ids { .. }
        | QueryNode::Constant { .. }
        | QueryNode::Knn { .. }
        | QueryNode::SemanticSearch { .. } => true,
        // The engine scores an opaque `query_string` as one BM25 `should`
        // over its tokens on every field it targets, so per-field scores sum
        // to `_score` with or without a `default_field` (a `title*` pattern
        // there has no projection and is the exception).
        QueryNode::QueryString {
            query,
            default_field,
            ..
        } => {
            query_string_is_field_free(query)
                && !matches!(default_field.as_deref(), Some(f) if f != "*" && f.contains('*'))
        }
        QueryNode::MultiMatch {
            fields, match_type, ..
        } => {
            matches!(match_type, MultiMatchType::MostFields) || expand_specs(fields, cx).len() <= 1
        }
        QueryNode::SimpleQueryString { fields, .. } => expand_specs(fields, cx).len() <= 1,
        QueryNode::Bool { must, should, .. } => {
            must.iter().chain(should).all(|c| is_additive(c, cx))
        }
        QueryNode::DisMax { queries, .. } => {
            queries.len() <= 1 && queries.iter().all(|c| is_additive(c, cx))
        }
        QueryNode::Boosted { query, .. } | QueryNode::Named { query, .. } => is_additive(query, cx),
        _ => false,
    }
}

/// The query that re-scores `projected` on exactly `ids`.
pub fn rescore_query(projected: QueryNode, ids: Vec<String>) -> QueryNode {
    QueryNode::Bool {
        must: vec![projected],
        should: Vec::new(),
        must_not: Vec::new(),
        filter: vec![QueryNode::Ids { values: ids }],
        minimum_should_match: None,
    }
}

/// Per-field scores for one hit, already measured.
pub struct FieldScores<'a> {
    pub score: f32,
    /// `(field, score)` for every field whose projection matched this hit.
    pub fields: Vec<(&'a str, f32)>,
}

/// The `_score_breakdown` value for one hit.
pub fn breakdown_value(
    scores: FieldScores<'_>,
    additive: bool,
    truncated: bool,
    unscored: &[String],
) -> Value {
    let mut fields = scores.fields;
    fields.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let sum: f32 = fields.iter().map(|(_, s)| *s).sum();
    let mut out = json!({
        "fields": fields
            .iter()
            .map(|(f, s)| json!({ "field": f, "score": s }))
            .collect::<Vec<_>>(),
        "combine": if additive { "sum" } else { "other" },
    });
    if additive && !truncated && unscored.is_empty() {
        out["unattributed"] = json!(scores.score - sum);
    }
    if truncated {
        out["fields_truncated"] = json!(true);
    }
    if !unscored.is_empty() {
        out["unscored_fields"] = json!(unscored);
    }
    out
}

/// Measure the per-field scores of `hits` for `query` against `idx` and
/// return one `_score_breakdown` per hit, in hit order.
pub async fn breakdowns(idx: &xerj_engine::Index, query: &QueryNode, hits: &[Hit]) -> Vec<Value> {
    if hits.is_empty() {
        return Vec::new();
    }
    let text = Fields::from_schema(&idx.schema().await);
    let mut fields = scoring_fields(query, &text);
    let truncated = fields.len() > MAX_EXPLAIN_FIELDS;
    fields.truncate(MAX_EXPLAIN_FIELDS);
    let additive = is_additive(query, &text);
    let ids: Vec<String> = hits.iter().map(|h| h.id.clone()).collect();

    let mut per_field: Vec<(String, HashMap<String, f32>)> = Vec::new();
    let mut unscored: Vec<String> = Vec::new();
    for field in fields {
        let Some(projected) = project_onto_field(query, &field, &text) else {
            continue;
        };
        let mut req = match xerj_query::parse_request(&json!({ "size": ids.len() })) {
            Ok(r) => r,
            Err(_) => {
                unscored.push(field);
                continue;
            }
        };
        req.query = rescore_query(projected, ids.clone());
        match idx.search(&req).await {
            Ok(result) => {
                let scores = result.hits.into_iter().map(|h| (h.id, h.score)).collect();
                per_field.push((field, scores));
            }
            Err(_) => unscored.push(field),
        }
    }

    hits.iter()
        .map(|hit| {
            let fields = per_field
                .iter()
                .filter_map(|(f, m)| m.get(&hit.id).map(|s| (f.as_str(), *s)))
                .collect();
            breakdown_value(
                FieldScores {
                    score: hit.score,
                    fields,
                },
                additive,
                truncated,
                &unscored,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(field: &str, q: &str) -> QueryNode {
        QueryNode::Match {
            field: field.into(),
            query: q.into(),
            operator: Default::default(),
            analyzer: None,
            boost: None,
            minimum_should_match: None,
            lenient: false,
        }
    }

    fn text() -> Vec<String> {
        vec!["title".into(), "body".into()]
    }

    fn fx() -> Fields {
        Fields {
            text: text(),
            pattern: text(),
        }
    }

    #[test]
    fn bool_projects_each_field_and_drops_filters() {
        let q = QueryNode::Bool {
            must: vec![m("title", "bone")],
            should: vec![m("body", "vitamin"), m("title", "density")],
            must_not: vec![m("body", "china")],
            filter: vec![QueryNode::Term {
                field: "kind".into(),
                value: json!("trial"),
                boost: None,
            }],
            minimum_should_match: None,
        };
        assert_eq!(scoring_fields(&q, &fx()), vec!["title", "body"]);
        match project_onto_field(&q, "title", &fx()).unwrap() {
            QueryNode::Bool { should, must, .. } => {
                assert!(must.is_empty());
                assert_eq!(should, vec![m("title", "bone"), m("title", "density")]);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            project_onto_field(&q, "body", &fx()),
            Some(m("body", "vitamin"))
        );
        assert_eq!(project_onto_field(&q, "kind", &fx()), None);
        assert!(is_additive(&q, &fx()));
    }

    #[test]
    fn field_less_query_string_expands_onto_text_fields() {
        let q = QueryNode::QueryString {
            query: "vitamin bone".into(),
            default_field: None,
            default_operator: None,
            boost: None,
        };
        assert_eq!(scoring_fields(&q, &fx()), text());
        assert_eq!(
            project_onto_field(&q, "body", &fx()),
            Some(QueryNode::QueryString {
                query: "vitamin bone".into(),
                default_field: Some("body".into()),
                default_operator: None,
                boost: None,
            })
        );
        // The engine sums the opaque clause's per-field BM25.
        assert!(is_additive(&q, &fx()));

        // An unqualified term is `best_fields` over the fields `*` names.
        let star = m("*", "vitamin bone");
        assert_eq!(scoring_fields(&star, &fx()), text());
        assert_eq!(
            project_onto_field(&star, "title", &fx()),
            Some(m("title", "vitamin bone"))
        );
        assert!(!is_additive(&star, &fx()));
        assert!(is_additive(&m("tit*", "vitamin"), &fx()));

        let fielded = QueryNode::QueryString {
            query: "title:vitamin".into(),
            default_field: None,
            default_operator: None,
            boost: None,
        };
        assert!(scoring_fields(&fielded, &fx()).is_empty());
        assert!(!is_additive(&fielded, &fx()));
    }

    #[test]
    fn patterns_reach_keyword_fields_but_the_opaque_clause_does_not() {
        let cx = Fields {
            text: text(),
            pattern: vec!["title".into(), "body".into(), "cat".into()],
        };
        let star = m("*", "trial");
        assert_eq!(scoring_fields(&star, &cx), vec!["title", "body", "cat"]);
        assert_eq!(
            project_onto_field(&star, "cat", &cx),
            Some(m("cat", "trial"))
        );
        let opaque = QueryNode::QueryString {
            query: "trial".into(),
            default_field: None,
            default_operator: None,
            boost: None,
        };
        assert_eq!(scoring_fields(&opaque, &cx), text());
        assert_eq!(project_onto_field(&opaque, "cat", &cx), None);
        assert!(pattern_matches("*", "cat"));
        assert!(pattern_matches("*_text", "body_text"));
        assert!(pattern_matches("a*c*e", "abcde"));
        assert!(!pattern_matches("tit*", "body"));
        assert!(!pattern_matches("a*c*e", "abcd"));
    }

    #[test]
    fn multi_match_keeps_the_field_boost_and_reports_best_fields_as_other() {
        let q = QueryNode::MultiMatch {
            fields: vec!["title^3".into(), "body".into()],
            query: "bone".into(),
            match_type: MultiMatchType::BestFields,
            operator: None,
            analyzer: None,
            boost: None,
            slop: 0,
            max_expansions: 50,
            lenient: false,
        };
        match project_onto_field(&q, "title", &fx()).unwrap() {
            QueryNode::MultiMatch { fields, .. } => assert_eq!(fields, vec!["title^3"]),
            other => panic!("{other:?}"),
        }
        assert!(!is_additive(&q, &fx()));
        let mut most = q.clone();
        if let QueryNode::MultiMatch { match_type, .. } = &mut most {
            *match_type = MultiMatchType::MostFields;
        }
        assert!(is_additive(&most, &fx()));
    }

    #[test]
    fn breakdown_sorts_fields_and_reports_the_remainder_only_when_additive() {
        let v = breakdown_value(
            FieldScores {
                score: 3.0,
                fields: vec![("body", 0.5), ("title", 2.0)],
            },
            true,
            false,
            &[],
        );
        assert_eq!(v["fields"][0]["field"], "title");
        assert_eq!(v["fields"][1]["field"], "body");
        assert_eq!(v["combine"], "sum");
        assert!((v["unattributed"].as_f64().unwrap() - 0.5).abs() < 1e-6);

        let v = breakdown_value(
            FieldScores {
                score: 2.0,
                fields: vec![("body", 0.5), ("title", 2.0)],
            },
            false,
            false,
            &[],
        );
        assert_eq!(v["combine"], "other");
        assert!(v.get("unattributed").is_none());
    }
}
