//! ES-compatible query DSL parser.
//!
//! Converts raw `serde_json::Value` (from the HTTP request body) into a
//! [`QueryNode`] tree.  The parser is intentionally lenient in the same places
//! ES is lenient (e.g. a missing `query` field defaults to `match_all`) and
//! strict — with **readable** error messages — where ES gives cryptic Java
//! stack traces.
//!
//! ## Entry points
//!
//! | Function | Description |
//! |---|---|
//! | [`parse_request`] | Parse a full `{ "query": …, "from": …, … }` body. |
//! | [`parse_query`] | Parse just the `query` object. |

use base64::Engine as _;
use serde_json::Value;
use std::cell::Cell;
use tracing::trace;

use crate::ast::{
    BoolOperator, BoostMode, FieldValueFactor, FusionStrategy, Fuzziness, GeoShapeType,
    MinShouldMatch, Modifier, MultiMatchType, QueryNode, RandomScore, ScoreFunction, ScoreMode,
    SearchRequest, SourceFilter, TrackTotalHits, WeightedQuery,
};
use crate::error::{ParseError, QueryError, Result};
use crate::sort::{SortField, SortMissing, SortMode, SortOrder};

// ── Recursion depth limit ──────────────────────────────────────────────────────
//
// Bool, dis_max, boosting and constant_score recurse into parse_query for every
// nested clause. Without a cap, a `{"bool":{"filter":[{"bool":{"filter":[…]}}]}}`
// payload of arbitrary depth blows the stack — a trivial unauthenticated DOS.
//
// ES uses a default of 20. We allow 64 (Rust frames are small relative to JVM
// frames). The counter is thread-local because parse_query is sync and runs on
// whichever tokio worker handles the request; parallel requests on different
// workers each get their own counter.
const MAX_QUERY_DEPTH: usize = 64;

/// Cap on `from + size`. Mirrors ES's `index.max_result_window` default.
/// Deep pagination beyond this should use `search_after`/PIT instead.
pub const MAX_RESULT_WINDOW: usize = 10_000;

thread_local! {
    static QUERY_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// RAII guard that increments the thread-local query-depth counter on
/// construction and decrements it on drop. Returns an error if the depth
/// exceeds `MAX_QUERY_DEPTH` so callers can `?` straight through.
struct DepthGuard;

impl DepthGuard {
    fn enter() -> Result<Self> {
        let exceeded = QUERY_DEPTH.with(|d| {
            let next = d.get() + 1;
            d.set(next);
            next > MAX_QUERY_DEPTH
        });
        if exceeded {
            QUERY_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
            return Err(QueryError::Parse(ParseError::Invalid(format!(
                "query nesting exceeds max depth of {MAX_QUERY_DEPTH}"
            ))));
        }
        Ok(DepthGuard)
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        QUERY_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

// ── Error helpers ──────────────────────────────────────────────────────────────

/// Construct `Err(QueryError::Parse(ParseError::Invalid(msg)))`.
#[inline(always)]
fn invalid<T>(msg: impl Into<String>) -> Result<T> {
    Err(QueryError::Parse(ParseError::Invalid(msg.into())))
}

/// Wrap a `QueryNode` in a `Named` node if `name` is `Some`, otherwise return it as-is.
#[inline(always)]
fn maybe_named(node: QueryNode, name: Option<String>) -> QueryNode {
    match name {
        Some(n) => QueryNode::Named {
            name: n,
            query: Box::new(node),
        },
        None => node,
    }
}

/// Construct `Err(QueryError::Parse(ParseError::UnknownQueryType(name)))`.
#[inline(always)]
fn unknown_type<T>(name: impl Into<String>) -> Result<T> {
    Err(QueryError::Parse(ParseError::UnknownQueryType(name.into())))
}

// ─────────────────────────────────────────────────────────────────────────────
// Public entry points
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a complete ES search request body.
///
/// ```json
/// {
///   "query": { "match": { "title": "rust" } },
///   "from": 0,
///   "size": 10,
///   "sort": [{ "date": "desc" }],
///   "_source": ["title", "body"],
///   "aggs": { … }
/// }
/// ```
pub fn parse_request(body: &Value) -> Result<SearchRequest> {
    let obj = body.as_object().ok_or_else(|| {
        QueryError::Parse(ParseError::Invalid(
            "request body must be a JSON object".into(),
        ))
    })?;

    let query = match obj.get("query") {
        Some(q) => parse_query(q)?,
        None => QueryNode::MatchAll,
    };

    let from = match obj.get("from") {
        Some(v) => v.as_u64().ok_or_else(|| {
            QueryError::Parse(ParseError::Invalid(
                "`from` must be a non-negative integer".into(),
            ))
        })? as usize,
        None => 0,
    };

    let size = match obj.get("size") {
        Some(v) => v.as_u64().ok_or_else(|| {
            QueryError::Parse(ParseError::Invalid(
                "`size` must be a non-negative integer".into(),
            ))
        })? as usize,
        None => 10,
    };

    // ES default `index.max_result_window` is 10_000. Without this cap a
    // single request with `size=2_000_000_000` allocates a Vec<Hit> for two
    // billion entries before pagination. Trust the user the same way ES
    // does — i.e., not at all.
    if from.saturating_add(size) > MAX_RESULT_WINDOW {
        return invalid(format!(
            "from + size must be <= {MAX_RESULT_WINDOW} (got {})",
            from.saturating_add(size)
        ));
    }

    let sort = match obj.get("sort") {
        Some(v) => parse_sort(v)?,
        None => Vec::new(),
    };

    let search_after = match obj.get("search_after") {
        Some(Value::Array(arr)) => Some(arr.clone()),
        Some(_) => return invalid("`search_after` must be an array"),
        None => None,
    };

    let aggs = obj.get("aggs").or_else(|| obj.get("aggregations")).cloned();

    let explain = obj
        .get("explain")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let source = match obj.get("_source") {
        Some(v) => parse_source_filter(v)?,
        None => SourceFilter::default(),
    };

    let timeout_ms = obj.get("timeout").and_then(parse_timeout);

    // Parse optional highlight configuration.
    let highlight = obj
        .get("highlight")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    // Parse track_total_hits: true | false | integer
    let track_total_hits = match obj.get("track_total_hits") {
        Some(Value::Bool(true)) | None => TrackTotalHits::True,
        Some(Value::Bool(false)) => TrackTotalHits::False,
        Some(Value::Number(n)) => {
            // ES accepts `track_total_hits: -1` as an alias for `true`
            // (track the exact total).
            if n.as_i64().map(|v| v < 0).unwrap_or(false) {
                TrackTotalHits::True
            } else {
                let limit = n.as_u64().unwrap_or(10_000);
                TrackTotalHits::Limit(limit)
            }
        }
        _ => TrackTotalHits::True,
    };

    // script_fields — opaque `{name: {script: {...}}}`; evaluated per hit
    // against the doc source by the ES-compat search handler.
    let script_fields = obj.get("script_fields").cloned();

    // fields — stored/doc-value fields to return alongside _source.
    let fields: Vec<String> = match obj.get("fields") {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    };

    // profile — include timing breakdown
    let profile = obj.get("profile").and_then(Value::as_bool).unwrap_or(false);

    Ok(SearchRequest {
        query,
        from,
        size,
        sort,
        search_after,
        aggs,
        explain,
        source,
        timeout_ms,
        highlight,
        track_total_hits,
        script_fields,
        fields,
        profile,
        collapse: None,
        rescore: Vec::new(),
        min_score: None,
    })
}

/// Parse just the query portion of a search body.
///
/// The `json` value should be the object **value** of the `"query"` key,
/// e.g. `{ "match": { "title": "hello" } }`.
pub fn parse_query(json: &Value) -> Result<QueryNode> {
    // Bail out early on excessive nesting. The guard decrements on drop so
    // an error return here does not leak the increment to a sibling call.
    let _depth = DepthGuard::enter()?;

    let obj = json.as_object().ok_or_else(|| {
        QueryError::Parse(ParseError::Invalid("query must be a JSON object".into()))
    })?;

    if obj.len() != 1 {
        if obj.is_empty() {
            return Ok(QueryNode::MatchAll);
        }
        return invalid(format!(
            "query object must have exactly one key, found {}: {}",
            obj.len(),
            obj.keys().cloned().collect::<Vec<_>>().join(", ")
        ));
    }

    let (query_type, params) = obj.iter().next().unwrap();
    trace!(query_type = %query_type, "parsing query");

    match query_type.as_str() {
        "match_all" => parse_match_all(params),
        "match_none" => Ok(QueryNode::MatchNone),
        "match" => parse_match(params),
        "match_phrase" => parse_match_phrase(params),
        "multi_match" => parse_multi_match(params),
        // ES 7.13+ `combined_fields` — treats multiple text fields as a
        // single virtual field and runs a match query over their
        // concatenation. We map it to multi_match with `type: cross_fields`
        // (the closest existing behavior) so the query parses and executes;
        // scoring doesn't exactly match ES's term-statistics pooling.
        "combined_fields" => parse_combined_fields(params),
        "term" => parse_term(params),
        "terms" => parse_terms(params),
        "range" => parse_range(params),
        "prefix" => parse_prefix(params),
        "wildcard" => parse_wildcard(params),
        "exists" => parse_exists(params),
        "ids" => parse_ids(params),
        "bool" => parse_bool(params),
        "query_string" => parse_query_string(params),
        "constant_score" => parse_constant_score(params),
        "boosting" => parse_boosting(params),
        "dis_max" => parse_dis_max(params),
        "knn" => parse_knn(params),
        "semantic" => parse_semantic(params),
        "hybrid" => parse_hybrid(params),
        "fuzzy" => parse_fuzzy(params),
        "regexp" => parse_regexp(params),
        "match_phrase_prefix" => parse_match_phrase_prefix(params),
        "simple_query_string" => parse_simple_query_string(params),
        "geo_distance" => parse_geo_distance(params),
        "geo_bounding_box" => parse_geo_bounding_box(params),
        "function_score" => parse_function_score(params),
        // ── Nested / join queries ──────────────────────────────────────────────
        "nested" => parse_nested(params),
        "has_child" => parse_has_child(params),
        "has_parent" => parse_has_parent(params),
        "more_like_this" => parse_more_like_this(params),
        "percolate" => parse_percolate(params),
        "pinned" => parse_pinned(params),
        // ── Span queries ───────────────────────────────────────────────────────
        "span_term" => parse_span_term(params),
        "span_near" => parse_span_near(params),
        "span_or" => parse_span_or(params),
        "span_not" => parse_span_not(params),
        "span_first" => parse_span_first(params),
        "span_containing" | "span_within" => parse_span_containing_like(query_type, params),
        // ── Geo shape queries ──────────────────────────────────────────────────
        "geo_polygon" => parse_geo_polygon(params),
        "geo_shape" => parse_geo_shape(params),
        // ── Specialised query types ────────────────────────────────────────────
        "match_bool_prefix" => parse_match_bool_prefix(params),
        "terms_set" => parse_terms_set(params),
        "intervals" => parse_intervals(params),
        "script_score" => parse_script_score(params),
        "distance_feature" => parse_distance_feature(params),
        "rank_feature" => parse_rank_feature(params),
        // ── Deprecated / pass-through queries ─────────────────────────────────
        "type" => Ok(QueryNode::MatchAll), // everything is type _doc in modern ES
        "wrapper" => parse_wrapper(params),
        unknown => unknown_type(unknown),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Leaf parsers
// ─────────────────────────────────────────────────────────────────────────────

fn parse_match_all(params: &Value) -> Result<QueryNode> {
    if !params.is_object() && !params.is_null() {
        return invalid("`match_all` value must be an object");
    }
    // ES scores `match_all { boost }` as exactly `boost` per hit — the
    // same constant-score semantics as `constant_score { match_all }`
    // (live-verified vs ES 8.13.4: boost 3.5 → every _score 3.5, boost
    // 0.0 → 0.0). Dropping the boost scored every doc 1.0. Only wrap
    // when it changes anything so the plain `match_all` keeps its
    // dedicated fast paths.
    match params.get("boost").and_then(|v| v.as_f64()) {
        Some(b) if (b as f32) != 1.0 => Ok(QueryNode::Constant {
            score: b as f32,
            query: Box::new(QueryNode::MatchAll),
        }),
        _ => Ok(QueryNode::MatchAll),
    }
}

fn parse_match(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`match` must be an object"))?;
    if obj.len() != 1 {
        return invalid("`match` query must have exactly one field");
    }

    let (field, value) = obj.iter().next().unwrap();
    let field = field.clone();

    // Shorthand forms: `match: {field: "value"}`, `match: {field: 42}`,
    // `match: {field: true}`.  ES silently stringifies non-strings.
    if let Some(query) = scalar_to_string(value) {
        return Ok(QueryNode::Match {
            field,
            query,
            operator: BoolOperator::Or,
            analyzer: None,
            boost: None,
            minimum_should_match: None,
        });
    }

    let vobj = value
        .as_object()
        .ok_or_else(|| qerr("`match` field value must be a scalar or object"))?;

    // Inside the object form, `query` can also be a number / bool.
    let query = vobj
        .get("query")
        .and_then(scalar_to_string)
        .ok_or_else(|| qerr("`match.query` must be a non-empty scalar"))?;
    let operator = parse_bool_operator(vobj.get("operator"))?;
    let analyzer = vobj
        .get("analyzer")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let boost = vobj.get("boost").and_then(|v| v.as_f64()).map(|b| b as f32);
    let minimum_should_match = vobj
        .get("minimum_should_match")
        .map(parse_min_should_match)
        .transpose()?;
    let name = vobj
        .get("_name")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let node = QueryNode::Match {
        field,
        query,
        operator,
        analyzer,
        boost,
        minimum_should_match,
    };
    Ok(maybe_named(node, name))
}

/// Best-effort conversion of a JSON scalar to a string for query
/// shorthand (ES accepts `match: {foo: 42}` same as `match: {foo: "42"}`).
fn scalar_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn parse_match_phrase(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`match_phrase` must be an object"))?;
    if obj.len() != 1 {
        return invalid("`match_phrase` query must have exactly one field");
    }

    let (field, value) = obj.iter().next().unwrap();
    let field = field.clone();

    if let Some(query) = value.as_str() {
        return Ok(QueryNode::MatchPhrase {
            field,
            query: query.to_string(),
            slop: 0,
            analyzer: None,
            boost: None,
        });
    }

    let vobj = value
        .as_object()
        .ok_or_else(|| qerr("`match_phrase` field value must be a string or object"))?;

    let query = string_field(vobj, "query")?;
    let slop = vobj.get("slop").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let analyzer = vobj
        .get("analyzer")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let boost = vobj.get("boost").and_then(|v| v.as_f64()).map(|b| b as f32);
    let name = vobj
        .get("_name")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let node = QueryNode::MatchPhrase {
        field,
        query,
        slop,
        analyzer,
        boost,
    };
    Ok(maybe_named(node, name))
}

/// ES `combined_fields` query — introduced in 7.13, treats N text fields as a
/// single virtual field for term-frequency scoring. Our approximation routes
/// it to `multi_match` with `type: "cross_fields"`, which is the closest
/// existing behavior. Accepts the same parameters ES does (query, fields,
/// operator, minimum_should_match, zero_terms_query).
fn parse_combined_fields(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`combined_fields` must be an object"))?;
    let mut rewritten = obj.clone();
    rewritten.insert(
        "type".to_string(),
        Value::String("cross_fields".to_string()),
    );
    parse_multi_match(&Value::Object(rewritten))
}

fn parse_multi_match(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`multi_match` must be an object"))?;

    let query = string_field(obj, "query")?;

    let fields = obj
        .get("fields")
        .and_then(|v| v.as_array())
        .ok_or_else(|| qerr("`multi_match.fields` must be an array"))?
        .iter()
        .map(|v| {
            v.as_str()
                .ok_or_else(|| qerr("`multi_match.fields` must contain strings"))
                .map(str::to_string)
        })
        .collect::<Result<Vec<_>>>()?;

    if fields.is_empty() {
        return invalid("`multi_match.fields` must not be empty");
    }

    let type_str = obj
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("best_fields");

    // bool_prefix: rewrite to a Bool::should over per-field match_bool_prefix
    // clauses. Tokens except the last become Match queries, the last becomes
    // a Prefix query — matching ES `match_bool_prefix` semantics across all
    // supplied fields.
    if type_str == "bool_prefix" {
        let analyzer_opt = obj
            .get("analyzer")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let analyzer_lowercases = !matches!(
            analyzer_opt.as_deref(),
            Some("whitespace") | Some("keyword")
        );
        let operator_and = matches!(
            obj.get("operator")
                .and_then(Value::as_str)
                .map(|s| s.to_ascii_lowercase())
                .as_deref(),
            Some("and")
        );
        let mm_ms = obj.get("minimum_should_match").and_then(|v| match v {
            Value::Number(n) => n.as_u64().map(|i| MinShouldMatch::Fixed(i as u32)),
            Value::String(s) => {
                if let Some(p) = s.strip_suffix('%') {
                    p.parse::<u32>().ok().map(MinShouldMatch::Percentage)
                } else {
                    s.parse::<u32>().ok().map(MinShouldMatch::Fixed)
                }
            }
            _ => None,
        });
        let fuzziness_opt = obj.get("fuzziness").map(|v| match v {
            Value::String(s) if s.eq_ignore_ascii_case("auto") => crate::ast::Fuzziness::Auto,
            Value::String(s) => s
                .parse::<u32>()
                .ok()
                .map(crate::ast::Fuzziness::Fixed)
                .unwrap_or(crate::ast::Fuzziness::Auto),
            Value::Number(n) => crate::ast::Fuzziness::Fixed(n.as_u64().unwrap_or(0) as u32),
            _ => crate::ast::Fuzziness::Auto,
        });
        let raw_tokens: Vec<String> = if analyzer_opt.as_deref() == Some("keyword") {
            vec![query.to_string()]
        } else {
            query.split_whitespace().map(str::to_string).collect()
        };
        let tokens: Vec<String> = raw_tokens
            .into_iter()
            .map(|t| {
                if analyzer_lowercases {
                    t.to_lowercase()
                } else {
                    t
                }
            })
            .collect();
        if tokens.is_empty() {
            return Ok(QueryNode::MatchAll);
        }
        let boost = obj.get("boost").and_then(|v| v.as_f64()).map(|b| b as f32);
        let mut should: Vec<QueryNode> = Vec::new();
        for raw_field in &fields {
            // Strip any ^boost suffix (e.g. "title^2").
            let (field, fb): (&str, Option<f32>) = match raw_field.rfind('^') {
                Some(idx) => (&raw_field[..idx], raw_field[idx + 1..].parse::<f32>().ok()),
                None => (raw_field.as_str(), None),
            };
            if tokens.len() == 1 {
                let prefix = QueryNode::Prefix {
                    field: field.to_string(),
                    value: tokens[0].clone(),
                    boost: fb,
                    constant_score: false,
                };
                should.push(prefix);
                continue;
            }
            let last = tokens.len() - 1;
            let build_leaf = |tok: &str| -> QueryNode {
                if let Some(fz) = fuzziness_opt {
                    QueryNode::Fuzzy {
                        field: field.to_string(),
                        value: tok.to_string(),
                        fuzziness: fz,
                    }
                } else {
                    QueryNode::Match {
                        field: field.to_string(),
                        query: tok.to_string(),
                        operator: BoolOperator::Or,
                        boost: None,
                        analyzer: analyzer_opt.clone(),
                        minimum_should_match: None,
                    }
                }
            };
            let mut inner_clauses: Vec<QueryNode> =
                tokens[..last].iter().map(|t| build_leaf(t)).collect();
            inner_clauses.push(QueryNode::Prefix {
                field: field.to_string(),
                value: tokens[last].clone(),
                boost: None,
                constant_score: false,
            });
            let (im, is, imm) = if operator_and {
                (inner_clauses, vec![], None)
            } else {
                (vec![], inner_clauses, mm_ms.clone())
            };
            let inner_bool = QueryNode::Bool {
                must: im,
                should: is,
                filter: vec![],
                must_not: vec![],
                minimum_should_match: imm,
            };
            let field_clause = if let Some(b) = fb {
                QueryNode::Boosted {
                    boost: b,
                    query: Box::new(inner_bool),
                }
            } else {
                inner_bool
            };
            should.push(field_clause);
        }
        return Ok(QueryNode::Bool {
            must: vec![],
            should,
            filter: vec![],
            must_not: vec![],
            minimum_should_match: None,
        })
        .map(|n| match boost {
            Some(b) => QueryNode::Boosted {
                query: Box::new(n),
                boost: b,
            },
            None => n,
        });
    }

    let match_type = match type_str {
        "best_fields" => MultiMatchType::BestFields,
        "most_fields" => MultiMatchType::MostFields,
        "cross_fields" => MultiMatchType::CrossFields,
        "phrase" => MultiMatchType::Phrase,
        "phrase_prefix" => MultiMatchType::PhrasePrefix,
        other => return invalid(format!("unknown multi_match type `{other}`")),
    };

    let operator = parse_bool_operator(obj.get("operator")).ok();
    let analyzer = obj
        .get("analyzer")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let boost = obj.get("boost").and_then(|v| v.as_f64()).map(|b| b as f32);

    Ok(QueryNode::MultiMatch {
        fields,
        query,
        match_type,
        operator,
        analyzer,
        boost,
    })
}

fn parse_term(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`term` must be an object"))?;
    if obj.len() != 1 {
        return invalid("`term` query must have exactly one field");
    }

    let (field, raw) = obj.iter().next().unwrap();
    let field = field.clone();

    if let Some(inner) = raw.as_object() {
        if let Some(value) = inner.get("value") {
            let boost = inner
                .get("boost")
                .and_then(|v| v.as_f64())
                .map(|b| b as f32);
            let name = inner
                .get("_name")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let case_insensitive = inner
                .get("case_insensitive")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            // `term` with `case_insensitive:true` is semantically an
            // exact-match against the lowercased value. Our Wildcard
            // matcher already lowercases both sides before comparing and
            // treats a pattern with no `*` / `?` as an exact match, so
            // routing through Wildcard gives correct semantics without a
            // new QueryNode variant.
            if case_insensitive {
                let val_str = match value {
                    Value::String(s) => s.clone(),
                    other => other.to_string().trim_matches('"').to_string(),
                };
                let node = QueryNode::Wildcard {
                    field,
                    value: val_str,
                    boost,
                    constant_score: false,
                };
                return Ok(maybe_named(node, name));
            }
            let node = QueryNode::Term {
                field,
                value: value.clone(),
                boost,
            };
            return Ok(maybe_named(node, name));
        }
    }

    Ok(QueryNode::Term {
        field,
        value: raw.clone(),
        boost: None,
    })
}

fn parse_terms(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`terms` must be an object"))?;

    let field_entries: Vec<_> = obj.iter().filter(|(k, _)| k.as_str() != "boost").collect();

    if field_entries.len() != 1 {
        return invalid("`terms` query must have exactly one field");
    }

    let (field, raw) = field_entries[0];
    let field = field.clone();

    // Terms lookup: {"terms": {"category": {"index": "other", "id": "1", "path": "categories"}}}
    // We gracefully handle this by returning MatchNone (empty results) rather than crashing.
    if raw.is_object() {
        tracing::warn!(
            field = %field,
            "terms lookup not supported — returning empty results"
        );
        return Ok(QueryNode::MatchNone);
    }

    let values = raw
        .as_array()
        .ok_or_else(|| qerr("`terms` values must be an array"))?
        .clone();

    let boost = obj.get("boost").and_then(|v| v.as_f64()).map(|b| b as f32);

    Ok(QueryNode::Terms {
        field,
        values,
        boost,
    })
}

/// Resolve an ES date-math expression like `now`, `now-24h`, `now+1d/d`
/// to a concrete ISO-8601 timestamp string usable by the executor's
/// JSON-scan range comparator. Returns `None` when the input is not
/// a recognised `now…` form (caller leaves the value untouched in
/// that case so non-date string bounds round-trip unchanged).
fn resolve_now_expr(expr: &str) -> Option<String> {
    let rest = expr.strip_prefix("now")?;
    let base = chrono::Utc::now();
    let mut delta_secs: i64 = 0;
    let (sign, rest) = if let Some(r) = rest.strip_prefix('+') {
        (1i64, r)
    } else if let Some(r) = rest.strip_prefix('-') {
        (-1i64, r)
    } else if rest.is_empty() || rest.starts_with('/') {
        (0, rest)
    } else {
        return None;
    };
    if sign != 0 {
        let delta_end = rest.find('/').unwrap_or(rest.len());
        let delta_expr = &rest[..delta_end];
        let (num_part, unit_part): (String, String) = delta_expr
            .chars()
            .partition(|c| c.is_ascii_digit() || *c == '.');
        let n: f64 = num_part.parse().ok()?;
        let secs = match unit_part.as_str() {
            "ms" => n / 1000.0,
            "s" => n,
            "m" => n * 60.0,
            "h" => n * 3600.0,
            "d" => n * 86_400.0,
            "w" => n * 7.0 * 86_400.0,
            "M" => n * 30.0 * 86_400.0,
            "y" => n * 365.0 * 86_400.0,
            _ => return None,
        };
        delta_secs = (sign as f64 * secs) as i64;
    }
    let dt = base + chrono::Duration::seconds(delta_secs);
    Some(dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

fn parse_range(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`range` must be an object"))?;
    if obj.len() != 1 {
        return invalid("`range` query must have exactly one field");
    }

    let (field, raw) = obj.iter().next().unwrap();
    let field = field.clone();

    let inner = raw
        .as_object()
        .ok_or_else(|| qerr("`range` field value must be an object"))?;

    let mut gte = inner.get("gte").cloned();
    let mut gt = inner.get("gt").cloned();
    let mut lte = inner.get("lte").cloned();
    let mut lt = inner.get("lt").cloned();
    let boost = inner
        .get("boost")
        .and_then(|v| v.as_f64())
        .map(|b| b as f32);

    // ES date math: `gte: "now-24h"` etc. resolve at query-time against
    // wall clock. Without this, downstream `doc_matches_query` would
    // compare `"now-24h"` as a literal string against doc values and
    // fail to match anything for date fields.
    let resolve_now = |v: Value| -> Value {
        match &v {
            Value::String(s) if s.trim_start().starts_with("now") => {
                if let Some(iso) = resolve_now_expr(s.trim()) {
                    Value::String(iso)
                } else {
                    v
                }
            }
            _ => v,
        }
    };
    if let Some(v) = gte.take() {
        gte = Some(resolve_now(v));
    }
    if let Some(v) = gt.take() {
        gt = Some(resolve_now(v));
    }
    if let Some(v) = lte.take() {
        lte = Some(resolve_now(v));
    }
    if let Some(v) = lt.take() {
        lt = Some(resolve_now(v));
    }

    // `format: uuuu` (year-only): ES resolves the bound as year-first-day
    // (YEAR-01-01T00:00:00) and applies range rounding at day granularity —
    // gte floors to 00:00:00, lte ceils to 23:59:59.999 of the same first
    // day. So `gte:1500 lte:1500` matches only docs on 1500-01-01, not the
    // whole year.
    if let Some(fmt) = inner.get("format").and_then(|v| v.as_str()) {
        let rewrite = |b: Value, upper: bool| -> Value {
            let year: Option<i64> = match &b {
                Value::Number(n) => n.as_i64(),
                Value::String(s) => s.trim().parse::<i64>().ok(),
                _ => None,
            };
            let Some(y) = year else { return b };
            if !matches!(fmt, "uuuu" | "yyyy" | "year") {
                return b;
            }
            let iso = if upper {
                format!("{:04}-01-01T23:59:59.999Z", y)
            } else {
                format!("{:04}-01-01T00:00:00.000Z", y)
            };
            Value::String(iso)
        };
        if let Some(v) = gte.take() {
            gte = Some(rewrite(v, false));
        }
        if let Some(v) = lte.take() {
            lte = Some(rewrite(v, true));
        }
    }

    if gte.is_none() && gt.is_none() && lte.is_none() && lt.is_none() {
        return invalid("`range` query must have at least one bound (gte, gt, lte, lt)");
    }

    Ok(QueryNode::Range {
        field,
        gte,
        gt,
        lte,
        lt,
        boost,
    })
}

fn parse_prefix(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`prefix` must be an object"))?;
    if obj.len() != 1 {
        return invalid("`prefix` query must have exactly one field");
    }

    let (field, raw) = obj.iter().next().unwrap();
    let field = field.clone();

    if let Some(value) = raw.as_str() {
        return Ok(QueryNode::Prefix {
            field,
            value: value.to_string(),
            boost: None,
            constant_score: true,
        });
    }

    let inner = raw
        .as_object()
        .ok_or_else(|| qerr("`prefix` field value must be a string or object"))?;

    let value = string_field(inner, "value")?;
    let boost = inner
        .get("boost")
        .and_then(|v| v.as_f64())
        .map(|b| b as f32);

    Ok(QueryNode::Prefix {
        field,
        value,
        boost,
        constant_score: true,
    })
}

fn parse_wildcard(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`wildcard` must be an object"))?;
    if obj.len() != 1 {
        return invalid("`wildcard` query must have exactly one field");
    }

    let (field, raw) = obj.iter().next().unwrap();
    let field = field.clone();

    if let Some(value) = raw.as_str() {
        return Ok(QueryNode::Wildcard {
            field,
            value: value.to_string(),
            boost: None,
            constant_score: true,
        });
    }

    let inner = raw
        .as_object()
        .ok_or_else(|| qerr("`wildcard` field value must be a string or object"))?;

    let value = string_field(inner, "value")?;
    let boost = inner
        .get("boost")
        .and_then(|v| v.as_f64())
        .map(|b| b as f32);

    Ok(QueryNode::Wildcard {
        field,
        value,
        boost,
        constant_score: true,
    })
}

fn parse_exists(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`exists` must be an object"))?;

    let field = string_field(obj, "field")?;
    Ok(QueryNode::Exists { field })
}

fn parse_ids(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`ids` must be an object"))?;

    let values = obj
        .get("values")
        .and_then(|v| v.as_array())
        .ok_or_else(|| qerr("`ids.values` must be an array"))?
        .iter()
        .map(|v| match v {
            Value::String(s) => Ok(s.clone()),
            Value::Number(n) => Ok(n.to_string()),
            _ => Err(qerr("`ids.values` must contain strings or numbers")),
        })
        .collect::<Result<Vec<_>>>()?;

    let node = QueryNode::Ids { values };
    // `ids.boost` IS the hit score: ES scores an ids query as the constant
    // `boost` (default 1.0) — live-verified on 8.13.4: boost 2.0 / 0.37 /
    // 0.0 all come back verbatim as `_score`. Carried as a `Boosted`
    // wrapper (whose scorer arm already yields the constant `boost` on
    // match) instead of a new AST field, so match semantics are untouched.
    // Was silently dropped (always scored 1.0).
    if let Some(boost) = obj.get("boost").and_then(|v| v.as_f64()) {
        let boost = boost as f32;
        if boost != 1.0 {
            return Ok(QueryNode::Boosted {
                boost,
                query: Box::new(node),
            });
        }
    }
    Ok(node)
}

fn parse_query_string(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`query_string` must be an object"))?;

    let query = string_field(obj, "query")?;
    let default_field = obj
        .get("default_field")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let default_operator = parse_bool_operator(obj.get("default_operator")).ok();
    let boost = obj.get("boost").and_then(|v| v.as_f64()).map(|b| b as f32);

    // Try to lower the query string into a Bool tree so downstream matchers
    // can honor `field:value` + OR/AND syntax.  Fall back to the opaque
    // QueryString node if lowering isn't possible (the FTS path still
    // tokenizes it) — but malformed range syntax is a hard parse error,
    // never a silent term-match fallback.
    if let Some(lowered) =
        try_lower_query_string(&query, default_field.as_deref(), default_operator)?
    {
        return Ok(match boost {
            Some(b) if b != 1.0 => QueryNode::Boosted {
                boost: b,
                query: Box::new(lowered),
            },
            _ => lowered,
        });
    }

    Ok(QueryNode::QueryString {
        query,
        default_field,
        default_operator,
        boost,
    })
}

/// Lower a Lucene-style query string into a QueryNode::Bool tree.
///
/// Supports:
///  - `field:value` → Match on that field
///  - `value` → Match on default_field (if provided) else MultiMatch on all
///  - `A OR B` → Bool.should
///  - `A AND B` → Bool.must
///  - `+A` must, `-A` must_not
///  - parentheses for grouping
///  - quoted phrases `"foo bar"` → MatchPhrase
///  - range syntax: `field:>N`, `field:>=N`, `field:<N`, `field:<=N`,
///    `field:[A TO B]` (inclusive), `field:{A TO B}` (exclusive), mixed
///    `[A TO B}` forms, and `*` as an open end → Range
///
/// Returns `Ok(None)` for anything unrecognized (caller falls back to the
/// legacy opaque-QueryString path) and `Err` for malformed range syntax —
/// ranges must never silently degrade to a term match.
fn try_lower_query_string(
    q: &str,
    default_field: Option<&str>,
    default_op: Option<BoolOperator>,
) -> Result<Option<QueryNode>> {
    let Some(tokens) = tokenize_query_string(q)? else {
        return Ok(None);
    };
    if tokens.is_empty() {
        return Ok(None);
    }
    // Range clauses must target a concrete field: resolve unqualified
    // ranges against default_field up-front so `>10` with no usable
    // default errors instead of degrading to a term match.
    let mut has_range = false;
    for t in &tokens {
        if let QsTok::Range { field, .. } = t {
            has_range = true;
            if field.is_empty() && !matches!(default_field, Some(df) if !df.is_empty() && df != "*")
            {
                return Err(qerr(
                    "query_string range requires an explicit field (e.g. `price:>10`) or a non-wildcard default_field",
                ));
            }
        }
    }
    let mut pos = 0;
    let parsed = parse_qs_or(&tokens, &mut pos, default_field, default_op);
    match parsed {
        Some(node) if pos == tokens.len() => Ok(Some(node)),
        _ if has_range => Err(qerr(format!(
            "failed to parse query_string with range syntax: `{q}`"
        ))),
        _ => Ok(None),
    }
}

#[derive(Debug, Clone)]
// `Range` (a field + four `Option<Value>` bounds) dwarfs the unit
// operator variants; QsTok is a short-lived per-parse token buffer, so
// boxing it would cost more indirection than the transient size saves.
#[allow(clippy::large_enum_variant)]
enum QsTok {
    Term(String, String),   // (field, value) — field empty if unqualified
    Phrase(String, String), // (field, value)
    Range {
        // field empty if unqualified
        field: String,
        gt: Option<Value>,
        gte: Option<Value>,
        lt: Option<Value>,
        lte: Option<Value>,
    },
    Or,
    And,
    Not,
    Must,
    LParen,
    RParen,
}

/// Parse a range bound value from a Lucene query_string range expression.
/// Numbers become JSON numbers (so the Range matcher compares numerically),
/// `now`-style date math resolves to an ISO timestamp, and everything else
/// stays a string. Surrounding quotes are stripped; `*` / empty → open end.
fn qs_range_bound(raw: &str) -> Option<Value> {
    let mut t = raw.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        t = &t[1..t.len() - 1];
    }
    if t.is_empty() || t == "*" {
        return None;
    }
    if let Ok(n) = t.parse::<i64>() {
        return Some(Value::from(n));
    }
    if let Ok(f) = t.parse::<f64>() {
        if f.is_finite() {
            return Some(Value::from(f));
        }
    }
    if t.starts_with("now") {
        if let Some(iso) = resolve_now_expr(t) {
            return Some(Value::String(iso));
        }
    }
    Some(Value::String(t.to_string()))
}

/// Parse a bracket range (`[A TO B]` / `{A TO B}` / mixed) starting at byte
/// offset `open` in `q` (pointing at the opening bracket). Returns the token
/// and the byte offset just past the closing bracket.
fn qs_parse_bracket_range(q: &str, field: &str, open: usize) -> Result<(QsTok, usize)> {
    let bytes = q.as_bytes();
    let inclusive_lo = bytes[open] == b'[';
    let mut j = open + 1;
    while j < bytes.len() && bytes[j] != b']' && bytes[j] != b'}' {
        j += 1;
    }
    if j >= bytes.len() {
        return Err(qerr(format!(
            "unterminated range in query_string: `{}`",
            &q[open..]
        )));
    }
    let inclusive_hi = bytes[j] == b']';
    let inner = &q[open + 1..j];
    let parts: Vec<&str> = inner.split_whitespace().collect();
    if parts.len() != 3 || !parts[1].eq_ignore_ascii_case("TO") {
        return Err(qerr(format!(
            "malformed range in query_string: expected `[<lower> TO <upper>]`, got `{}`",
            &q[open..=j]
        )));
    }
    let lo = qs_range_bound(parts[0]);
    let hi = qs_range_bound(parts[2]);
    if lo.is_none() && hi.is_none() {
        return Err(qerr(
            "query_string range must have at least one non-`*` bound",
        ));
    }
    let (mut gt, mut gte, mut lt, mut lte) = (None, None, None, None);
    if inclusive_lo {
        gte = lo;
    } else {
        gt = lo;
    }
    if inclusive_hi {
        lte = hi;
    } else {
        lt = hi;
    }
    Ok((
        QsTok::Range {
            field: field.to_string(),
            gt,
            gte,
            lt,
            lte,
        },
        j + 1,
    ))
}

/// Parse a comparison range (`>N`, `>=N`, `<N`, `<=N`). `rest` is the text
/// after the `field:` prefix within the scanned token; `i` is the byte offset
/// in `q` where the token scanner stopped — the scanner breaks on `-`/`+`,
/// which are value characters here (negative numbers, dates like
/// `2020-01-01`), so the value is extended from the raw string. Returns the
/// token and the byte offset just past the value.
fn qs_parse_cmp_range(q: &str, field: &str, rest: &str, mut i: usize) -> Result<(QsTok, usize)> {
    let bytes = q.as_bytes();
    let (op, mut val) = if let Some(v) = rest.strip_prefix(">=") {
        (">=", v.to_string())
    } else if let Some(v) = rest.strip_prefix("<=") {
        ("<=", v.to_string())
    } else if let Some(v) = rest.strip_prefix('>') {
        (">", v.to_string())
    } else {
        ("<", rest[1..].to_string())
    };
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_whitespace() || c == '(' || c == ')' {
            break;
        }
        val.push(c);
        i += 1;
    }
    let bound = qs_range_bound(&val).ok_or_else(|| {
        qerr(format!(
            "query_string range `{field}:{op}` is missing a value"
        ))
    })?;
    let (mut gt, mut gte, mut lt, mut lte) = (None, None, None, None);
    match op {
        ">" => gt = Some(bound),
        ">=" => gte = Some(bound),
        "<" => lt = Some(bound),
        _ => lte = Some(bound),
    }
    Ok((
        QsTok::Range {
            field: field.to_string(),
            gt,
            gte,
            lt,
            lte,
        },
        i,
    ))
}

/// Tokenize a Lucene query string. `Ok(None)` means "can't tokenize — fall
/// back to the opaque QueryString path"; `Err` is a hard parse error
/// (malformed range syntax must not degrade to a term match).
fn tokenize_query_string(q: &str) -> Result<Option<Vec<QsTok>>> {
    let bytes = q.as_bytes();
    let mut out: Vec<QsTok> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '(' {
            out.push(QsTok::LParen);
            i += 1;
            continue;
        }
        if c == ')' {
            out.push(QsTok::RParen);
            i += 1;
            continue;
        }
        if c == '+' {
            out.push(QsTok::Must);
            i += 1;
            continue;
        }
        if c == '-' {
            out.push(QsTok::Not);
            i += 1;
            continue;
        }
        // Quoted phrase — optional `field:"value"` prefix handled below.
        if c == '"' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] as char != '"' {
                j += 1;
            }
            if j >= bytes.len() {
                return Ok(None);
            }
            let val = q[start..j].to_string();
            out.push(QsTok::Phrase(String::new(), val));
            i = j + 1;
            continue;
        }
        // Unqualified bracket range on the default field: `[A TO B]` / `{A TO B}`.
        if c == '[' || c == '{' {
            let (tok, next) = qs_parse_bracket_range(q, "", i)?;
            out.push(tok);
            i = next;
            continue;
        }
        // Bare token (possibly field:value or field:"value").
        //
        // NOTE: `+` / `-` are Lucene unary operators ONLY at the start of a
        // clause (handled by the top-of-loop checks above, which can only
        // fire at a fresh scan position). INSIDE a bare token they are
        // literal characters — `model:claude-haiku-4-5` is ONE term, not
        // `model:claude NOT haiku NOT 4 NOT 5`. Breaking on mid-token `-`
        // shredded hyphenated keyword values and made e.g.
        // `query_string: "model:claude-haiku-4-5 AND status:ok"` match a
        // garbage clause tree instead of the two field terms.
        let start = i;
        while i < bytes.len() {
            let ch = bytes[i] as char;
            if ch.is_whitespace() || ch == '(' || ch == ')' {
                break;
            }
            i += 1;
        }
        let tok = &q[start..i];
        // Keywords OR / AND / NOT (case-insensitive, must stand alone).
        match tok {
            "OR" | "or" | "||" => {
                out.push(QsTok::Or);
                continue;
            }
            "AND" | "and" | "&&" => {
                out.push(QsTok::And);
                continue;
            }
            "NOT" | "not" | "!" => {
                out.push(QsTok::Not);
                continue;
            }
            _ => {}
        }
        // Lucene query_string unescapes `\X` → `X` for any char X;
        // common cases are `\/`, `\+`, `\-`, `\(`, etc. Unescape here so
        // the downstream matcher compares against the literal value
        // rather than a value-with-stray-backslash.
        fn unescape_qs(s: &str) -> String {
            let mut out = String::with_capacity(s.len());
            let mut chars = s.chars();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    if let Some(nxt) = chars.next() {
                        out.push(nxt);
                    }
                } else {
                    out.push(c);
                }
            }
            out
        }
        // field:value or field:"value"
        if let Some(colon) = tok.find(':') {
            let field = &tok[..colon];
            let rest = &tok[colon + 1..];
            if let Some(after_quote) = rest.strip_prefix('"') {
                // Consume continuation until matching quote (may have been split).
                let mut buf = after_quote.to_string();
                if buf.ends_with('"') {
                    buf.pop();
                    out.push(QsTok::Phrase(field.to_string(), buf));
                } else {
                    // Rejoin from original string.
                    let mut j = i;
                    while j < bytes.len() && bytes[j] as char != '"' {
                        j += 1;
                    }
                    if j >= bytes.len() {
                        return Ok(None);
                    }
                    let whole = format!("{} {}", buf, &q[i..j]);
                    out.push(QsTok::Phrase(field.to_string(), whole));
                    i = j + 1;
                }
            } else if rest.starts_with('[') || rest.starts_with('{') {
                // field:[A TO B] / field:{A TO B} — the bare-token scan stops
                // at whitespace, so re-scan the raw string from the bracket.
                let (rtok, next) = qs_parse_bracket_range(q, field, start + colon + 1)?;
                out.push(rtok);
                i = next;
            } else if rest.starts_with('>') || rest.starts_with('<') {
                // field:>N / field:>=N / field:<N / field:<=N
                let (rtok, next) = qs_parse_cmp_range(q, field, rest, i)?;
                out.push(rtok);
                i = next;
            } else {
                out.push(QsTok::Term(field.to_string(), unescape_qs(rest)));
            }
        } else if tok.starts_with('>') || tok.starts_with('<') {
            // Unqualified comparison range on the default field: `>N`, `<=N`, …
            let (rtok, next) = qs_parse_cmp_range(q, "", tok, i)?;
            out.push(rtok);
            i = next;
        } else {
            out.push(QsTok::Term(String::new(), unescape_qs(tok)));
        }
    }
    Ok(Some(out))
}

fn parse_qs_or(
    toks: &[QsTok],
    pos: &mut usize,
    default_field: Option<&str>,
    default_op: Option<BoolOperator>,
) -> Option<QueryNode> {
    let mut left = parse_qs_and(toks, pos, default_field, default_op)?;
    // `A OR B` — explicit OR operator.
    while *pos < toks.len() && matches!(toks[*pos], QsTok::Or) {
        *pos += 1;
        let right = parse_qs_and(toks, pos, default_field, default_op)?;
        left = QueryNode::Bool {
            must: vec![],
            must_not: vec![],
            filter: vec![],
            should: vec![left, right],
            minimum_should_match: Some(MinShouldMatch::Fixed(1)),
        };
    }
    // When default_operator is OR (the ES default when unset) and
    // parse_qs_and stopped because it hit a juxtaposed clause, fold the
    // remaining clauses into a should-bool. Without this, `field:foo
    // field:xyz` only evaluates the first clause and drops `field:xyz`.
    let implicit_or = matches!(default_op, None | Some(BoolOperator::Or));
    if implicit_or {
        while *pos < toks.len() && !matches!(toks[*pos], QsTok::RParen | QsTok::Or | QsTok::And) {
            let right = parse_qs_and(toks, pos, default_field, default_op)?;
            left = QueryNode::Bool {
                must: vec![],
                must_not: vec![],
                filter: vec![],
                should: vec![left, right],
                minimum_should_match: Some(MinShouldMatch::Fixed(1)),
            };
        }
    }
    Some(left)
}

fn parse_qs_and(
    toks: &[QsTok],
    pos: &mut usize,
    default_field: Option<&str>,
    default_op: Option<BoolOperator>,
) -> Option<QueryNode> {
    let mut clauses: Vec<QueryNode> = Vec::new();
    let mut not_clauses: Vec<QueryNode> = Vec::new();
    let mut explicit_and = false;
    loop {
        // Optional + / - / NOT prefix for each clause.
        let mut force_not = false;
        while *pos < toks.len() {
            match &toks[*pos] {
                QsTok::Must => {
                    *pos += 1;
                }
                QsTok::Not => {
                    force_not = true;
                    *pos += 1;
                }
                _ => break,
            }
        }
        let node = parse_qs_unary(toks, pos, default_field)?;
        if force_not {
            not_clauses.push(node);
        } else {
            clauses.push(node);
        }
        if *pos >= toks.len() {
            break;
        }
        match &toks[*pos] {
            QsTok::And => {
                explicit_and = true;
                *pos += 1;
                continue;
            }
            QsTok::Or | QsTok::RParen => break,
            _ => {
                // Juxtaposition — treat as OR unless default_op is AND.
                if matches!(default_op, Some(BoolOperator::And)) || explicit_and {
                    continue;
                }
                break;
            }
        }
    }
    if clauses.len() == 1 && not_clauses.is_empty() {
        return Some(clauses.pop().unwrap());
    }
    let should_mode =
        !explicit_and && !clauses.is_empty() && !matches!(default_op, Some(BoolOperator::And));
    let node = if should_mode {
        QueryNode::Bool {
            must: vec![],
            filter: vec![],
            must_not: not_clauses,
            should: clauses,
            minimum_should_match: Some(MinShouldMatch::Fixed(1)),
        }
    } else {
        QueryNode::Bool {
            must: clauses,
            filter: vec![],
            must_not: not_clauses,
            should: vec![],
            minimum_should_match: None,
        }
    };
    Some(node)
}

fn parse_qs_unary(
    toks: &[QsTok],
    pos: &mut usize,
    default_field: Option<&str>,
) -> Option<QueryNode> {
    if *pos >= toks.len() {
        return None;
    }
    match toks[*pos].clone() {
        QsTok::LParen => {
            *pos += 1;
            let n = parse_qs_or(toks, pos, default_field, None)?;
            if *pos >= toks.len() || !matches!(toks[*pos], QsTok::RParen) {
                return None;
            }
            *pos += 1;
            Some(n)
        }
        QsTok::Term(field, value) => {
            *pos += 1;
            let f = if field.is_empty() {
                default_field.unwrap_or("*").to_string()
            } else {
                field
            };
            // A bare term containing `*` or `?` is a Lucene wildcard —
            // emit a Wildcard query so `q=shor*` / `q=te?t` match text
            // tokens with the expected substitution semantics.
            //
            // ES lowercases wildcard/prefix terms in `query_string` (the field's
            // search analyzer normalizes them) — e.g. `q=field:BA*` matches the
            // indexed lowercased token `bar`.  The raw `wildcard` query does NOT
            // analyze its pattern (case-sensitive), so this lowering lives HERE
            // in the query_string path only.  Harmless for keyword fields, whose
            // FST-wildcard route case-folds both sides regardless.
            if value.contains('*') || value.contains('?') {
                return Some(QueryNode::Wildcard {
                    field: f,
                    value: value.to_lowercase(),
                    boost: None,
                    constant_score: false,
                });
            }
            Some(QueryNode::Match {
                field: f,
                query: value,
                operator: BoolOperator::Or,
                analyzer: None,
                boost: None,
                minimum_should_match: None,
            })
        }
        QsTok::Phrase(field, value) => {
            *pos += 1;
            let f = if field.is_empty() {
                default_field.unwrap_or("*").to_string()
            } else {
                field
            };
            Some(QueryNode::MatchPhrase {
                field: f,
                query: value,
                slop: 0,
                analyzer: None,
                boost: None,
            })
        }
        QsTok::Range {
            field,
            gt,
            gte,
            lt,
            lte,
        } => {
            *pos += 1;
            // Unqualified ranges with no usable default_field were already
            // rejected in try_lower_query_string.
            let f = if field.is_empty() {
                default_field.unwrap_or("*").to_string()
            } else {
                field
            };
            Some(QueryNode::Range {
                field: f,
                gte,
                gt,
                lte,
                lt,
                boost: None,
            })
        }
        _ => None,
    }
}

fn parse_fuzzy(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`fuzzy` must be an object"))?;
    if obj.len() != 1 {
        return invalid("`fuzzy` query must have exactly one field");
    }

    let (field, raw) = obj.iter().next().unwrap();
    let field = field.clone();

    // Shorthand: {"fuzzy": {"name": "prodct"}}
    if let Some(value) = raw.as_str() {
        return Ok(QueryNode::Fuzzy {
            field,
            value: value.to_string(),
            fuzziness: Fuzziness::Auto,
        });
    }

    let inner = raw
        .as_object()
        .ok_or_else(|| qerr("`fuzzy` field value must be a string or object"))?;

    let value = string_field(inner, "value")?;
    let fuzziness = match inner.get("fuzziness") {
        Some(Value::String(s)) if s.to_uppercase() == "AUTO" => Fuzziness::Auto,
        Some(Value::String(s)) => {
            let n = s.parse::<u32>().map_err(|_| {
                qerr(format!(
                    "`fuzzy.fuzziness` must be 'AUTO' or an integer, got '{s}'"
                ))
            })?;
            Fuzziness::Fixed(n)
        }
        Some(Value::Number(n)) => {
            let n = n
                .as_u64()
                .ok_or_else(|| qerr("`fuzzy.fuzziness` must be a non-negative integer"))?
                as u32;
            Fuzziness::Fixed(n)
        }
        None => Fuzziness::Auto,
        _ => return invalid("`fuzzy.fuzziness` must be 'AUTO' or an integer"),
    };

    Ok(QueryNode::Fuzzy {
        field,
        value,
        fuzziness,
    })
}

fn parse_regexp(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`regexp` must be an object"))?;
    if obj.len() != 1 {
        return invalid("`regexp` query must have exactly one field");
    }

    let (field, raw) = obj.iter().next().unwrap();
    let field = field.clone();

    if let Some(pattern) = raw.as_str() {
        return Ok(QueryNode::Regexp {
            field,
            pattern: pattern.to_string(),
        });
    }

    let inner = raw
        .as_object()
        .ok_or_else(|| qerr("`regexp` field value must be a string or object"))?;

    let pattern = string_field(inner, "value")?;
    Ok(QueryNode::Regexp { field, pattern })
}

fn parse_match_phrase_prefix(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`match_phrase_prefix` must be an object"))?;
    if obj.len() != 1 {
        return invalid("`match_phrase_prefix` query must have exactly one field");
    }

    let (field, raw) = obj.iter().next().unwrap();
    let field = field.clone();

    if let Some(query) = raw.as_str() {
        return Ok(QueryNode::MatchPhrasePrefix {
            field,
            query: query.to_string(),
            max_expansions: 50,
        });
    }

    let inner = raw
        .as_object()
        .ok_or_else(|| qerr("`match_phrase_prefix` field value must be a string or object"))?;

    let query = string_field(inner, "query")?;
    let max_expansions = inner
        .get("max_expansions")
        .and_then(Value::as_u64)
        .unwrap_or(50) as u32;

    Ok(QueryNode::MatchPhrasePrefix {
        field,
        query,
        max_expansions,
    })
}

fn parse_simple_query_string(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`simple_query_string` must be an object"))?;

    let query = string_field(obj, "query")?;

    let fields: Vec<String> = obj
        .get("fields")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    // ES default_operator: "or" (default) means tokens go into `should`
    // with `minimum_should_match`; "and" puts them all into `must`.
    let default_operator = obj
        .get("default_operator")
        .and_then(Value::as_str)
        .unwrap_or("or")
        .to_lowercase();

    let mm: Option<MinShouldMatch> = obj.get("minimum_should_match").and_then(|v| match v {
        Value::Number(n) => n.as_u64().map(|i| MinShouldMatch::Fixed(i as u32)),
        Value::String(s) => {
            if let Some(stripped) = s.strip_suffix('%') {
                stripped.parse::<u32>().ok().map(MinShouldMatch::Percentage)
            } else {
                s.parse::<u32>().ok().map(MinShouldMatch::Fixed)
            }
        }
        _ => None,
    });

    // Tokenize the query: split on whitespace; leading +/-/| signal per-term operators.
    let mut must: Vec<QueryNode> = Vec::new();
    let mut should: Vec<QueryNode> = Vec::new();
    let mut must_not: Vec<QueryNode> = Vec::new();

    for tok in query.split_whitespace() {
        // Detect a leading per-term sign.
        let (sign, term_text) = if let Some(rest) = tok.strip_prefix('+') {
            ('+', rest)
        } else if let Some(rest) = tok.strip_prefix('-') {
            ('-', rest)
        } else if let Some(rest) = tok.strip_prefix('|') {
            ('|', rest)
        } else {
            (' ', tok)
        };
        if term_text.is_empty() {
            continue;
        }
        let node = make_simple_query_node(term_text, &fields);
        match sign {
            '+' => must.push(node),
            '-' => must_not.push(node),
            '|' => should.push(node),
            _ => {
                // Unsigned token: dispatched per default_operator.
                if default_operator == "and" {
                    must.push(node);
                } else {
                    should.push(node);
                }
            }
        }
    }

    // No tokens parsed (rare empty query): treat query as a literal term.
    if must.is_empty() && should.is_empty() && must_not.is_empty() {
        let node = make_simple_query_node(&query, &fields);
        return Ok(node);
    }

    let final_mm = if !should.is_empty() && must.is_empty() && mm.is_none() {
        // Default: at least 1 should clause must match.
        Some(MinShouldMatch::Fixed(1))
    } else {
        mm
    };

    Ok(QueryNode::Bool {
        must,
        should,
        must_not,
        filter: Vec::new(),
        minimum_should_match: final_mm,
    })
}

/// Build a Match or MultiMatch node for a term in a simple_query_string.
fn make_simple_query_node(term: &str, fields: &[String]) -> QueryNode {
    if fields.len() == 1 {
        QueryNode::Match {
            field: fields[0].clone(),
            query: term.to_string(),
            operator: BoolOperator::Or,
            analyzer: None,
            boost: None,
            minimum_should_match: None,
        }
    } else if fields.is_empty() {
        // No fields specified — use a match_all-like placeholder.
        QueryNode::QueryString {
            query: term.to_string(),
            default_field: None,
            default_operator: None,
            boost: None,
        }
    } else {
        QueryNode::MultiMatch {
            fields: fields.to_vec(),
            query: term.to_string(),
            match_type: crate::ast::MultiMatchType::BestFields,
            operator: None,
            analyzer: None,
            boost: None,
        }
    }
}

/// Parse a `geo_distance` query.
///
/// ES format:
/// ```json
/// {
///   "geo_distance": {
///     "distance": "10km",
///     "location": { "lat": 40.7, "lon": -74.0 }
///   }
/// }
/// ```
///
/// The field name is inferred as the key that is not `"distance"`.
fn parse_geo_distance(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`geo_distance` must be an object"))?;

    // Parse the distance string (e.g. "10km", "5mi").
    let distance_str = obj
        .get("distance")
        .and_then(|v| v.as_str())
        .ok_or_else(|| qerr("`geo_distance` requires a `distance` field"))?;

    let distance_km = parse_distance_km(distance_str)?;

    // Find the field key (the one that is not "distance").
    let field_entry = obj
        .iter()
        .find(|(k, _)| k.as_str() != "distance")
        .ok_or_else(|| qerr("`geo_distance` requires a geo_point field"))?;

    let field = field_entry.0.clone();
    let location = field_entry.1;

    let (lat, lon) = parse_lat_lon(location)?;

    Ok(QueryNode::GeoDistance {
        field,
        lat,
        lon,
        distance_km,
    })
}

/// Parse a `geo_bounding_box` query.
///
/// ES format:
/// ```json
/// {
///   "geo_bounding_box": {
///     "location": {
///       "top_left": { "lat": 40.8, "lon": -74.0 },
///       "bottom_right": { "lat": 40.7, "lon": -73.9 }
///     }
///   }
/// }
/// ```
fn parse_geo_bounding_box(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`geo_bounding_box` must be an object"))?;

    // Find the field key (anything that is not a known option key).
    let known_keys = ["_name", "validation_method", "ignore_unmapped"];
    let field_entry = obj
        .iter()
        .find(|(k, _)| !known_keys.contains(&k.as_str()))
        .ok_or_else(|| qerr("`geo_bounding_box` requires a geo_point field"))?;

    let field = field_entry.0.clone();
    let bounds = field_entry.1;

    let bounds_obj = bounds
        .as_object()
        .ok_or_else(|| qerr("`geo_bounding_box` field value must be an object"))?;

    let tl_val = bounds_obj
        .get("top_left")
        .ok_or_else(|| qerr("`geo_bounding_box` requires `top_left`"))?;
    let br_val = bounds_obj
        .get("bottom_right")
        .ok_or_else(|| qerr("`geo_bounding_box` requires `bottom_right`"))?;

    let top_left = parse_lat_lon(tl_val)?;
    let bottom_right = parse_lat_lon(br_val)?;

    Ok(QueryNode::GeoBoundingBox {
        field,
        top_left,
        bottom_right,
    })
}

/// Parse a distance string like "10km", "5mi", "100m" into kilometres.
fn parse_distance_km(s: &str) -> Result<f64> {
    let s = s.trim();
    if let Some(km) = s.strip_suffix("km") {
        km.trim()
            .parse::<f64>()
            .map_err(|_| qerr(format!("invalid distance: `{s}`")))
    } else if let Some(mi) = s.strip_suffix("mi") {
        mi.trim()
            .parse::<f64>()
            .map(|m| m * 1.60934)
            .map_err(|_| qerr(format!("invalid distance: `{s}`")))
    } else if let Some(m) = s.strip_suffix('m') {
        m.trim()
            .parse::<f64>()
            .map(|m| m / 1000.0)
            .map_err(|_| qerr(format!("invalid distance: `{s}`")))
    } else {
        // Bare number interpreted as kilometres.
        s.parse::<f64>()
            .map_err(|_| qerr(format!("invalid distance: `{s}`")))
    }
}

/// Parse a lat/lon value from various ES formats:
///   - `{ "lat": 40.7, "lon": -74.0 }`
///   - `[lon, lat]` (GeoJSON order)
///   - `"40.7,-74.0"` (lat,lon string)
fn parse_lat_lon(value: &Value) -> Result<(f64, f64)> {
    match value {
        Value::Object(obj) => {
            let lat = obj
                .get("lat")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| qerr("geo_point object requires `lat` as a number"))?;
            let lon = obj
                .get("lon")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| qerr("geo_point object requires `lon` as a number"))?;
            Ok((lat, lon))
        }
        Value::Array(arr) if arr.len() == 2 => {
            // GeoJSON order: [lon, lat]
            let lon = arr[0]
                .as_f64()
                .ok_or_else(|| qerr("geo_point array element must be a number"))?;
            let lat = arr[1]
                .as_f64()
                .ok_or_else(|| qerr("geo_point array element must be a number"))?;
            Ok((lat, lon))
        }
        Value::String(s) => {
            // "lat,lon" string
            let parts: Vec<&str> = s.splitn(2, ',').collect();
            if parts.len() != 2 {
                return invalid(format!("geo_point string must be `lat,lon`, got `{s}`"));
            }
            let lat = parts[0]
                .trim()
                .parse::<f64>()
                .map_err(|_| qerr(format!("invalid lat in `{s}`")))?;
            let lon = parts[1]
                .trim()
                .parse::<f64>()
                .map_err(|_| qerr(format!("invalid lon in `{s}`")))?;
            Ok((lat, lon))
        }
        _ => invalid("geo_point must be an object {lat,lon}, array [lon,lat], or 'lat,lon' string"),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Compound parsers
// ─────────────────────────────────────────────────────────────────────────────

fn parse_bool(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`bool` must be an object"))?;

    let must = parse_clause_list(obj, "must")?;
    let should = parse_clause_list(obj, "should")?;
    let must_not = parse_clause_list(obj, "must_not")?;
    let filter = parse_clause_list(obj, "filter")?;

    // A `boost` on a compound clause must PROPAGATE into the leaf
    // weights (Lucene multiplies the parent boost into each child's
    // weight — weights compose multiplicatively down the tree).
    // Previously dropped silently. The `Boosted` wrapper carries it;
    // every matching/filter path peels the wrapper, and the scored
    // paths compose it into the leaves.
    let boost = obj
        .get("boost")
        .and_then(|v| v.as_f64())
        .map(|b| b as f32)
        .filter(|b| *b != 1.0);

    if must.is_empty() && should.is_empty() && must_not.is_empty() && filter.is_empty() {
        // Empty bool == match_all; with a boost, ES scores it `boost`
        // per hit (constant-score semantics), like `match_all{boost}`.
        return Ok(match boost {
            Some(b) => QueryNode::Constant {
                score: b,
                query: Box::new(QueryNode::MatchAll),
            },
            None => QueryNode::MatchAll,
        });
    }

    let minimum_should_match = obj
        .get("minimum_should_match")
        .map(parse_min_should_match)
        .transpose()?;

    let node = QueryNode::Bool {
        must,
        should,
        must_not,
        filter,
        minimum_should_match,
    };
    Ok(match boost {
        Some(b) => QueryNode::Boosted {
            boost: b,
            query: Box::new(node),
        },
        None => node,
    })
}

fn parse_constant_score(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`constant_score` must be an object"))?;

    let filter_val = obj
        .get("filter")
        .ok_or_else(|| qerr("`constant_score` requires a `filter` clause"))?;
    let query = parse_query(filter_val)?;

    // Keep the Constant wrapper: ES scores every `constant_score` hit as
    // exactly `boost` (default 1.0) — flattening returned the INNER query's
    // score instead (live-diverged vs ES 8.13.4: constant_score(term,
    // boost=2.5) scored 1.693… where ES returns 2.5 per hit).  The fast
    // paths the old flatten protected (is_doc_scan_query,
    // try_doc_values_query, try_shortcut_count, query_node_to_fts,
    // doc_matches_query, query_node_to_agg_filter) all peel
    // `QueryNode::Constant` today, and the scored-family columnar path
    // serves the keyword-filter shape bit-exactly.
    let boost = obj.get("boost").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
    Ok(QueryNode::Constant {
        score: boost,
        query: Box::new(query),
    })
}

fn parse_boosting(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`boosting` must be an object"))?;

    let positive_val = obj
        .get("positive")
        .ok_or_else(|| qerr("`boosting` requires a `positive` clause"))?;
    let negative_val = obj
        .get("negative")
        .ok_or_else(|| qerr("`boosting` requires a `negative` clause"))?;

    let positive = parse_query(positive_val)?;
    let negative = parse_query(negative_val)?;
    let negative_boost = obj
        .get("negative_boost")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.1) as f32;

    Ok(QueryNode::Boosting {
        positive: Box::new(positive),
        negative: Box::new(negative),
        negative_boost,
    })
}

fn parse_dis_max(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`dis_max` must be an object"))?;

    let queries_val = obj
        .get("queries")
        .and_then(|v| v.as_array())
        .ok_or_else(|| qerr("`dis_max.queries` must be an array"))?;

    let queries = queries_val
        .iter()
        .enumerate()
        .map(|(i, v)| {
            parse_query(v).map_err(|e| {
                crate::error::QueryError::Parse(crate::error::ParseError::Invalid(format!(
                    "`dis_max.queries[{i}]`: {e}"
                )))
            })
        })
        .collect::<Result<Vec<_>>>()?;

    if queries.is_empty() {
        return invalid("`dis_max.queries` must not be empty");
    }

    let tie_breaker = obj
        .get("tie_breaker")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as f32;

    let node = QueryNode::DisMax {
        queries,
        tie_breaker,
    };
    // Compound-clause boost — same propagation contract as `bool` (see
    // `parse_bool`): wrap, don't drop.
    Ok(match obj.get("boost").and_then(|v| v.as_f64()) {
        Some(b) if (b as f32) != 1.0 => QueryNode::Boosted {
            boost: b as f32,
            query: Box::new(node),
        },
        _ => node,
    })
}

fn parse_knn(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`knn` must be an object"))?;

    let field = string_field(obj, "field")?;
    let num_candidates = obj
        .get("num_candidates")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    // `k` derivation is unchanged: explicit `k`, else `num_candidates`, else 10.
    let k = obj
        .get("k")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .or(num_candidates)
        .unwrap_or(10);

    let vector = obj
        .get("query_vector")
        .or_else(|| obj.get("vector"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| qerr("`knn` requires `query_vector` (or `vector`) as a float array"))?
        .iter()
        .map(|v| {
            v.as_f64()
                .ok_or_else(|| qerr("`knn.query_vector` must contain numbers"))
                .map(|f| f as f32)
        })
        .collect::<Result<Vec<f32>>>()?;

    let filter = obj
        .get("filter")
        .map(parse_query)
        .transpose()?
        .map(Box::new);

    let boost = obj.get("boost").and_then(|v| v.as_f64()).map(|b| b as f32);

    Ok(QueryNode::Knn {
        field,
        vector,
        k,
        num_candidates,
        filter,
        boost,
    })
}

fn parse_semantic(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`semantic` must be an object"))?;

    let field = string_field(obj, "field")?;
    let text = string_field(obj, "query")?;
    let k = obj.get("k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

    let filter = obj
        .get("filter")
        .map(parse_query)
        .transpose()?
        .map(Box::new);

    let boost = obj.get("boost").and_then(|v| v.as_f64()).map(|b| b as f32);

    Ok(QueryNode::SemanticSearch {
        field,
        text,
        k,
        filter,
        boost,
    })
}

fn parse_hybrid(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`hybrid` must be an object"))?;

    let queries_val = obj
        .get("queries")
        .and_then(|v| v.as_array())
        .ok_or_else(|| qerr("`hybrid.queries` must be an array"))?;

    let queries = queries_val
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let entry = v
                .as_object()
                .ok_or_else(|| qerr(format!("`hybrid.queries[{i}]` must be an object")))?;
            let q_val = entry
                .get("query")
                .ok_or_else(|| qerr(format!("`hybrid.queries[{i}]` missing `query`")))?;
            let query = parse_query(q_val)?;
            let weight = entry.get("weight").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            Ok(WeightedQuery { query, weight })
        })
        .collect::<Result<Vec<_>>>()?;

    if queries.is_empty() {
        return invalid("`hybrid.queries` must not be empty");
    }

    let fusion = match obj.get("fusion") {
        Some(Value::String(s)) => match s.as_str() {
            "rrf" => FusionStrategy::Rrf { k: 60 },
            "linear" => FusionStrategy::Linear,
            // Learned fusion is not implemented — fail loud rather than
            // silently substituting RRF (which would misrepresent the
            // ranking the caller asked for). AST variant kept for future.
            "learned" => {
                return invalid("hybrid fusion learned is not yet supported; use rrf or linear")
            }
            other => return invalid(format!("unknown hybrid fusion strategy `{other}`")),
        },
        Some(Value::Object(m)) => {
            let t = m.get("type").and_then(|v| v.as_str()).unwrap_or("rrf");
            match t {
                "rrf" => {
                    let k = m.get("k").and_then(|v| v.as_u64()).unwrap_or(60) as u32;
                    FusionStrategy::Rrf { k }
                }
                "linear" => FusionStrategy::Linear,
                // Learned fusion is not implemented — fail loud (see above).
                "learned" => {
                    return invalid("hybrid fusion learned is not yet supported; use rrf or linear")
                }
                other => return invalid(format!("unknown hybrid fusion strategy `{other}`")),
            }
        }
        None => FusionStrategy::default(),
        _ => return invalid("`hybrid.fusion` must be a string or object"),
    };

    Ok(QueryNode::Hybrid { queries, fusion })
}

/// Parse a `function_score` query.
///
/// ```json
/// {
///   "function_score": {
///     "query": { "match_all": {} },
///     "functions": [
///       { "filter": { "term": { "status": "published" } },
///         "field_value_factor": { "field": "popularity", "factor": 1.5, "modifier": "log1p" } },
///       { "weight": 2.0, "filter": { "term": { "featured": true } } }
///     ],
///     "score_mode": "sum",
///     "boost_mode": "multiply",
///     "max_boost": 10.0
///   }
/// }
/// ```
fn parse_function_score(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`function_score` must be an object"))?;

    let name = obj
        .get("_name")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // Inner query (defaults to match_all).
    let query = match obj.get("query") {
        Some(q) => parse_query(q)?,
        None => QueryNode::MatchAll,
    };

    // Parse functions array.
    let functions: Vec<ScoreFunction> = match obj.get("functions") {
        Some(Value::Array(arr)) => arr
            .iter()
            .map(parse_score_function)
            .collect::<Result<Vec<_>>>()?,
        Some(_) => return invalid("`function_score.functions` must be an array"),
        None => {
            // Shorthand: a single function can be specified inline.
            let f = parse_score_function_inline(obj)?;
            if f.weight.is_none() && f.field_value_factor.is_none() && f.random_score.is_none() {
                Vec::new()
            } else {
                vec![f]
            }
        }
    };

    let score_mode = match obj
        .get("score_mode")
        .and_then(Value::as_str)
        .unwrap_or("multiply")
    {
        "multiply" => ScoreMode::Multiply,
        "sum" => ScoreMode::Sum,
        "avg" => ScoreMode::Avg,
        "first" => ScoreMode::First,
        "max" => ScoreMode::Max,
        "min" => ScoreMode::Min,
        other => return invalid(format!("unknown score_mode `{other}`")),
    };

    let boost_mode = match obj
        .get("boost_mode")
        .and_then(Value::as_str)
        .unwrap_or("multiply")
    {
        "multiply" => BoostMode::Multiply,
        "replace" => BoostMode::Replace,
        "sum" => BoostMode::Sum,
        "avg" => BoostMode::Avg,
        "max" => BoostMode::Max,
        "min" => BoostMode::Min,
        other => return invalid(format!("unknown boost_mode `{other}`")),
    };

    let max_boost = obj
        .get("max_boost")
        .and_then(|v| v.as_f64())
        .map(|b| b as f32);

    let mut node = QueryNode::FunctionScore {
        query: Box::new(query),
        functions,
        score_mode,
        boost_mode,
        max_boost,
    };
    // `function_score.boost` multiplies the BASE query score BEFORE the
    // boost_mode combine (Lucene propagates the boost into the subquery
    // weight; `_explain` shows `*:*^3.0` = 3.0 as the base branch).
    // Live-verified on ES 8.13.4: `{boost:3, fvf(rank=5)}` scores 15
    // (multiply), 8 (sum), 4 (avg), 5 (max), 3 (min), and 5 (replace —
    // the boost has NO effect when the base is discarded). The engine's
    // `peel_function_score_boosted` recovers this wrapper's boost and
    // applies it to the base with exactly those semantics. Was silently
    // dropped.
    if let Some(boost) = obj.get("boost").and_then(|v| v.as_f64()) {
        let boost = boost as f32;
        if boost != 1.0 {
            node = QueryNode::Boosted {
                boost,
                query: Box::new(node),
            };
        }
    }
    Ok(maybe_named(node, name))
}

/// Parse one entry in the `functions` array.
fn parse_score_function(v: &Value) -> Result<ScoreFunction> {
    let obj = v
        .as_object()
        .ok_or_else(|| qerr("each entry in `functions` must be an object"))?;
    parse_score_function_inline(obj)
}

/// Parse score function fields from an existing object map.
fn parse_score_function_inline(obj: &serde_json::Map<String, Value>) -> Result<ScoreFunction> {
    let filter = obj
        .get("filter")
        .map(parse_query)
        .transpose()?
        .map(Box::new);
    let weight = obj.get("weight").and_then(|v| v.as_f64()).map(|w| w as f32);

    let field_value_factor = match obj.get("field_value_factor") {
        Some(fvf_val) => {
            let fvf = fvf_val
                .as_object()
                .ok_or_else(|| qerr("`field_value_factor` must be an object"))?;
            let field = fvf
                .get("field")
                .and_then(Value::as_str)
                .ok_or_else(|| qerr("`field_value_factor.field` is required"))?
                .to_string();
            let factor = fvf.get("factor").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let modifier = match fvf
                .get("modifier")
                .and_then(Value::as_str)
                .unwrap_or("none")
            {
                "none" => Modifier::None,
                "log" => Modifier::Log,
                "log1p" => Modifier::Log1p,
                "log2p" => Modifier::Log2p,
                "ln" => Modifier::Ln,
                "ln1p" => Modifier::Ln1p,
                "ln2p" => Modifier::Ln2p,
                "square" => Modifier::Square,
                "sqrt" => Modifier::Sqrt,
                "reciprocal" => Modifier::Reciprocal,
                other => return invalid(format!("unknown modifier `{other}`")),
            };
            let missing = fvf.get("missing").and_then(|v| v.as_f64());
            Some(FieldValueFactor {
                field,
                factor,
                modifier,
                missing,
            })
        }
        None => None,
    };

    let random_score = match obj.get("random_score") {
        Some(rs_val) => {
            let seed = rs_val.get("seed").and_then(|v| v.as_u64());
            let field = rs_val
                .get("field")
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(RandomScore { seed, field })
        }
        None => None,
    };

    // script_score within a function: literal numeric source goes into
    // the fast-path `script_score` field; richer Painless source goes
    // into `script_source` and is evaluated at score time.
    let script_obj = obj.get("script_score").and_then(|ss| ss.get("script"));
    let script_source_str = script_obj
        .and_then(|s| s.get("source"))
        .and_then(Value::as_str)
        .map(String::from);
    let script_params = script_obj.and_then(|s| s.get("params").cloned());
    let literal_score = script_source_str
        .as_ref()
        .and_then(|s| s.trim().parse::<f32>().ok());
    let (script_score, script_source) = if let Some(n) = literal_score {
        (Some(n), None)
    } else {
        (None, script_source_str)
    };

    let name = obj.get("_name").and_then(Value::as_str).map(String::from);

    Ok(ScoreFunction {
        filter: filter.map(|b| *b),
        weight,
        field_value_factor,
        random_score,
        script_score,
        script_source,
        script_params,
        name,
        distance_feature: None,
        rank_feature: None,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Sort parsing
// ─────────────────────────────────────────────────────────────────────────────

fn parse_sort(value: &Value) -> Result<Vec<SortField>> {
    match value {
        Value::String(s) => Ok(vec![parse_sort_field_str(s)]),
        Value::Array(arr) => arr.iter().map(parse_sort_entry).collect(),
        _ => invalid("`sort` must be a string or array"),
    }
}

fn parse_sort_entry(value: &Value) -> Result<SortField> {
    match value {
        Value::String(s) => Ok(parse_sort_field_str(s)),
        Value::Object(obj) => {
            if obj.len() != 1 {
                return invalid("each sort entry must have exactly one field");
            }
            let (field, spec) = obj.iter().next().unwrap();
            parse_sort_field_spec(field, spec)
        }
        _ => invalid("sort entry must be a string or object"),
    }
}

fn parse_sort_field_str(s: &str) -> SortField {
    match s {
        "_score" => SortField {
            field: "_score".to_string(),
            order: SortOrder::Desc,
            mode: SortMode::default(),
            missing: SortMissing::default(),
            format: None,
        },
        "_doc" => SortField {
            field: "_doc".to_string(),
            order: SortOrder::Asc,
            mode: SortMode::default(),
            missing: SortMissing::default(),
            format: None,
        },
        other => SortField {
            field: other.to_string(),
            order: SortOrder::Asc,
            mode: SortMode::default(),
            missing: SortMissing::default(),
            format: None,
        },
    }
}

fn parse_sort_field_spec(field: &str, spec: &Value) -> Result<SortField> {
    if let Some(order_str) = spec.as_str() {
        let order = parse_sort_order(order_str)?;
        return Ok(SortField {
            field: field.to_string(),
            order,
            mode: SortMode::default(),
            missing: SortMissing::default(),
            format: None,
        });
    }

    let obj = spec
        .as_object()
        .ok_or_else(|| qerr("sort field spec must be a string or object"))?;

    let order = match obj.get("order").and_then(|v| v.as_str()) {
        Some(s) => parse_sort_order(s)?,
        None => SortOrder::Asc,
    };

    let mode = match obj.get("mode").and_then(|v| v.as_str()) {
        Some("min") => SortMode::Min,
        Some("max") => SortMode::Max,
        Some("avg") => SortMode::Avg,
        Some("sum") => SortMode::Sum,
        Some("median") => SortMode::Median,
        None => SortMode::default(),
        Some(other) => return invalid(format!("unknown sort mode `{other}`")),
    };

    let missing = match obj.get("missing").and_then(|v| v.as_str()) {
        Some("_last") | None => SortMissing::Last,
        Some("_first") => SortMissing::First,
        Some(other) => SortMissing::Value(Value::String(other.to_string())),
    };

    let format = obj.get("format").and_then(|v| v.as_str()).map(String::from);

    Ok(SortField {
        field: field.to_string(),
        order,
        mode,
        missing,
        format,
    })
}

fn parse_sort_order(s: &str) -> Result<SortOrder> {
    match s {
        "asc" => Ok(SortOrder::Asc),
        "desc" => Ok(SortOrder::Desc),
        other => invalid(format!(
            "unknown sort order `{other}` (expected `asc` or `desc`)"
        )),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Source filter parsing
// ─────────────────────────────────────────────────────────────────────────────

fn parse_source_filter(value: &Value) -> Result<SourceFilter> {
    match value {
        Value::Bool(b) => Ok(SourceFilter::Enabled(*b)),
        Value::String(s) => Ok(SourceFilter::Includes(vec![s.clone()])),
        Value::Array(arr) => {
            let fields = arr
                .iter()
                .map(|v| {
                    v.as_str()
                        .ok_or_else(|| qerr("`_source` array must contain strings"))
                        .map(str::to_string)
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(SourceFilter::Includes(fields))
        }
        Value::Object(obj) => {
            let includes = parse_string_list(obj.get("includes"))?;
            let excludes = parse_string_list(obj.get("excludes"))?;
            Ok(SourceFilter::Fields { includes, excludes })
        }
        _ => invalid("`_source` must be a bool, string, array, or object"),
    }
}

fn parse_string_list(value: Option<&Value>) -> Result<Vec<String>> {
    match value {
        None => Ok(vec![]),
        Some(Value::String(s)) => Ok(vec![s.clone()]),
        Some(Value::Array(arr)) => arr
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| qerr("field list must contain strings"))
                    .map(str::to_string)
            })
            .collect(),
        _ => invalid("field list must be a string or array of strings"),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Construct a `QueryError` for use with `ok_or_else`.
#[inline(always)]
fn qerr(msg: impl Into<String>) -> QueryError {
    QueryError::Parse(ParseError::Invalid(msg.into()))
}

/// Parse a `minimum_should_match` value.
fn parse_min_should_match(v: &Value) -> Result<MinShouldMatch> {
    match v {
        Value::Number(n) => {
            let i = n
                .as_u64()
                .ok_or_else(|| qerr("`minimum_should_match` must be non-negative"))?;
            Ok(MinShouldMatch::Fixed(i as u32))
        }
        Value::String(s) => {
            let s = s.trim();
            if let Some(pct) = s.strip_suffix('%') {
                let pct: i64 = pct.trim().parse().map_err(|_| {
                    qerr(format!("invalid `minimum_should_match` percentage: `{s}`"))
                })?;
                let pct = if pct < 0 {
                    (100 + pct).max(0) as u32
                } else {
                    pct as u32
                };
                Ok(MinShouldMatch::Percentage(pct))
            } else {
                let n: u32 = s
                    .parse()
                    .map_err(|_| qerr(format!("invalid `minimum_should_match` value: `{s}`")))?;
                Ok(MinShouldMatch::Fixed(n))
            }
        }
        _ => invalid("`minimum_should_match` must be a number or string"),
    }
}

fn parse_bool_operator(v: Option<&Value>) -> Result<BoolOperator> {
    match v {
        None => Ok(BoolOperator::Or),
        Some(Value::String(s)) => match s.to_uppercase().as_str() {
            "AND" => Ok(BoolOperator::And),
            "OR" => Ok(BoolOperator::Or),
            other => invalid(format!(
                "unknown boolean operator `{other}` (expected AND or OR)"
            )),
        },
        _ => invalid("`operator` must be a string"),
    }
}

/// Parse a bool clause list.  ES allows a single object or an array.
fn parse_clause_list(obj: &serde_json::Map<String, Value>, key: &str) -> Result<Vec<QueryNode>> {
    match obj.get(key) {
        None => Ok(vec![]),
        Some(Value::Array(arr)) => arr.iter().map(parse_query).collect(),
        Some(single) => Ok(vec![parse_query(single)?]),
    }
}

fn string_field(obj: &serde_json::Map<String, Value>, key: &str) -> Result<String> {
    obj.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| qerr(format!("`{key}` must be a non-empty string")))
}

/// Parse ES timeout strings like "1s", "100ms", "2m".
fn parse_timeout(v: &Value) -> Option<u64> {
    let s = v.as_str()?;
    if let Some(ms) = s.strip_suffix("ms") {
        ms.parse().ok()
    } else if let Some(s_str) = s.strip_suffix('s') {
        s_str.parse::<u64>().ok().map(|n| n * 1_000)
    } else if let Some(m) = s.strip_suffix('m') {
        m.parse::<u64>().ok().map(|n| n * 60_000)
    } else {
        s.parse().ok()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// New query type parsers
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a `nested` query.
///
/// ```json
/// {
///   "nested": {
///     "path": "comments",
///     "query": { "match": { "comments.text": "great" } },
///     "score_mode": "avg"
///   }
/// }
/// ```
fn parse_nested(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`nested` must be an object"))?;

    let path = string_field(obj, "path")?;
    let query_val = obj
        .get("query")
        .ok_or_else(|| qerr("`nested` requires a `query` clause"))?;
    let query = parse_query(query_val)?;
    let score_mode = obj
        .get("score_mode")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    Ok(QueryNode::Nested {
        path,
        query: Box::new(query),
        score_mode,
    })
}

/// Parse a `more_like_this` query.
///
/// ```json
/// {
///   "more_like_this": {
///     "fields": ["title", "body"],
///     "like": ["a very interesting document"],
///     "min_term_freq": 1,
///     "max_query_terms": 12
///   }
/// }
/// ```
fn parse_more_like_this(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`more_like_this` must be an object"))?;

    let fields: Vec<String> = obj
        .get("fields")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    // `like` can be a string, an array of strings, or an array of mixed
    // strings / {"_index":…,"_id":…} objects (we only use text strings).
    let like: Vec<String> = match obj.get("like") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => o.get("doc").and_then(|d| {
                    // Inline document: extract text from all string fields.
                    if let Some(doc_obj) = d.as_object() {
                        let text: Vec<String> = doc_obj
                            .values()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect();
                        if text.is_empty() {
                            None
                        } else {
                            Some(text.join(" "))
                        }
                    } else {
                        None
                    }
                }),
                _ => None,
            })
            .collect(),
        _ => return invalid("`more_like_this.like` must be a string or array"),
    };

    if like.is_empty() {
        return invalid("`more_like_this.like` must contain at least one text string");
    }

    let min_term_freq = obj
        .get("min_term_freq")
        .and_then(|v| v.as_u64())
        .unwrap_or(2) as u32;

    let max_query_terms = obj
        .get("max_query_terms")
        .and_then(|v| v.as_u64())
        .unwrap_or(25) as u32;

    // Rewrite `more_like_this` into a `bool.should` of `match` clauses — one
    // per (field × like-text) — exactly as Elasticsearch lowers MLT to a
    // disjunction of term queries over the analyzed like-text. This routes MLT
    // through the postings + scored path instead of an O(N) per-doc substring
    // scan, and produces BM25 scores bit-identical to ES (a keyword-field MLT
    // reduces to the same `term` query ES emits) instead of the old flat 1.0.
    // `match` handles both keyword (exact-token) and analyzed-text fields, so
    // no schema lookup is needed here. When `fields` is unspecified we cannot
    // build the disjunction, so we fall back to the legacy MoreLikeThis node
    // (which scans all string fields on the brute path).
    if !fields.is_empty() {
        let mut should: Vec<QueryNode> = Vec::with_capacity(fields.len() * like.len());
        for field in &fields {
            for text in &like {
                should.push(QueryNode::Match {
                    field: field.clone(),
                    query: text.clone(),
                    operator: BoolOperator::Or,
                    analyzer: None,
                    boost: None,
                    minimum_should_match: None,
                });
            }
        }
        // Bool with only `should` clauses defaults to "≥1 should must match",
        // matching ES's MLT default (`minimum_should_match: "30%"` still lands
        // at 1 for a single interesting term; we keep the engine default here).
        return Ok(QueryNode::Bool {
            must: vec![],
            should,
            must_not: vec![],
            filter: vec![],
            minimum_should_match: None,
        });
    }

    Ok(QueryNode::MoreLikeThis {
        fields,
        like,
        min_term_freq,
        max_query_terms,
    })
}

/// Parse a `pinned` query.
///
/// ```json
/// {
///   "pinned": {
///     "ids": ["1", "2"],
///     "organic": { "match": { "title": "search" } }
///   }
/// }
/// ```
/// Parse a `percolate` query.
///
/// Only the inline-document form is supported:
/// ```json
/// { "field": "query", "document": { … } }
/// { "field": "query", "documents": [ { … }, … ] }
/// ```
/// The index/id fetch form (`{ "field": "query", "index": "...", "id": "..." }`)
/// is rejected with a 400 rather than silently returning nothing.
fn parse_percolate(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`percolate` must be an object"))?;

    let field = string_field(obj, "field")?;

    let documents: Vec<Value> = if let Some(doc) = obj.get("document") {
        if !doc.is_object() {
            return invalid("`percolate.document` must be an object");
        }
        vec![doc.clone()]
    } else if let Some(docs) = obj.get("documents") {
        let arr = docs
            .as_array()
            .ok_or_else(|| qerr("`percolate.documents` must be an array"))?;
        if arr.iter().any(|d| !d.is_object()) {
            return invalid("`percolate.documents` must contain only objects");
        }
        arr.clone()
    } else {
        return invalid(
            "`percolate` requires an inline `document` or `documents`; the \
             index/id document-fetch form is not supported",
        );
    };

    if documents.is_empty() {
        return invalid("`percolate` requires at least one document");
    }

    Ok(QueryNode::Percolate { field, documents })
}

fn parse_pinned(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`pinned` must be an object"))?;

    let ids: Vec<String> = obj
        .get("ids")
        .and_then(|v| v.as_array())
        .ok_or_else(|| qerr("`pinned` requires an `ids` array"))?
        .iter()
        .map(|v| match v {
            Value::String(s) => Ok(s.clone()),
            Value::Number(n) => Ok(n.to_string()),
            _ => Err(qerr("`pinned.ids` must contain strings or numbers")),
        })
        .collect::<Result<Vec<_>>>()?;

    let organic_val = obj
        .get("organic")
        .ok_or_else(|| qerr("`pinned` requires an `organic` query"))?;
    let organic = parse_query(organic_val)?;

    Ok(QueryNode::Pinned {
        ids,
        organic: Box::new(organic),
    })
}

/// Parse a `wrapper` query — base64-encodes a query JSON string.
///
/// ```json
/// { "wrapper": { "query": "eyJtYXRjaF9hbGwiOiB7fX0=" } }
/// ```
fn parse_wrapper(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`wrapper` must be an object"))?;

    let encoded = obj
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| qerr("`wrapper.query` must be a base64-encoded query string"))?;

    let decoded_bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| qerr("`wrapper.query` is not valid base64"))?;

    let decoded_str = std::str::from_utf8(&decoded_bytes)
        .map_err(|_| qerr("`wrapper.query` base64 payload is not valid UTF-8"))?;

    let inner_json: Value = serde_json::from_str(decoded_str)
        .map_err(|_| qerr("`wrapper.query` base64 payload is not valid JSON"))?;

    // The decoded JSON may be a full query object {"match": {…}} or a
    // SearchRequest-style envelope {"query": {…}}.  Try both.
    if let Some(q) = inner_json.as_object().and_then(|o| o.get("query")) {
        parse_query(q)
    } else {
        parse_query(&inner_json)
    }
}

/// Parse a `span_term` query.
///
/// Accepts:
/// - `{"span_term": {"field": "value"}}` — shorthand string form
/// - `{"span_term": {"field": {"value": "text"}}}` — long form
fn parse_span_term(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`span_term` must be an object"))?;
    if obj.len() != 1 {
        return invalid("`span_term` query must have exactly one field");
    }

    let (field, raw) = obj.iter().next().unwrap();
    let field = field.clone();

    let value = if let Some(s) = raw.as_str() {
        s.to_string()
    } else if let Some(inner) = raw.as_object() {
        if let Some(v) = inner.get("value").and_then(|v| v.as_str()) {
            v.to_string()
        } else {
            return invalid("`span_term` object form requires a `value` string");
        }
    } else {
        return invalid("`span_term` field value must be a string or object");
    };

    Ok(QueryNode::SpanTerm { field, value })
}

/// Parse a `span_near` query.
///
/// `{"span_near": {"clauses": [...], "slop": 5, "in_order": true}}`
fn parse_span_near(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`span_near` must be an object"))?;

    let clauses: Vec<QueryNode> = obj
        .get("clauses")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| parse_query(v).ok()).collect())
        .unwrap_or_default();

    if clauses.is_empty() {
        return Ok(QueryNode::MatchNone);
    }

    let slop = obj.get("slop").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let in_order = obj
        .get("in_order")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    Ok(QueryNode::SpanNear {
        clauses,
        slop,
        in_order,
    })
}

/// Parse a `span_or` query.
///
/// `{"span_or": {"clauses": [...]}}`
fn parse_span_or(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`span_or` must be an object"))?;

    let clauses: Vec<QueryNode> = obj
        .get("clauses")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| parse_query(v).ok()).collect())
        .unwrap_or_default();

    if clauses.is_empty() {
        return Ok(QueryNode::MatchNone);
    }

    Ok(QueryNode::SpanOr { clauses })
}

/// Parse a `span_not` query.
///
/// `{"span_not": {"include": {...}, "exclude": {...}}}`
fn parse_span_not(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`span_not` must be an object"))?;

    let include = obj
        .get("include")
        .ok_or_else(|| qerr("`span_not` requires an `include` clause"))
        .and_then(parse_query)?;
    let exclude = obj
        .get("exclude")
        .ok_or_else(|| qerr("`span_not` requires an `exclude` clause"))
        .and_then(parse_query)?;

    Ok(QueryNode::SpanNot {
        include: Box::new(include),
        exclude: Box::new(exclude),
    })
}

/// Parse a `span_first` query.
///
/// `{"span_first": {"match": {...}, "end": 3}}`
fn parse_span_first(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`span_first` must be an object"))?;

    let match_query = obj
        .get("match")
        .ok_or_else(|| qerr("`span_first` requires a `match` clause"))
        .and_then(parse_query)?;
    let end = obj.get("end").and_then(|v| v.as_u64()).unwrap_or(1) as u32;

    Ok(QueryNode::SpanFirst {
        match_query: Box::new(match_query),
        end,
    })
}

/// Parse `span_containing` / `span_within`. Both take a `little` and a `big`
/// span clause; the difference is only which span must enclose the other,
/// which is captured by the returned variant.
fn parse_span_containing_like(query_type: &str, params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr(format!("`{}` must be an object", query_type)))?;

    let little = obj
        .get("little")
        .ok_or_else(|| qerr(format!("`{}` requires a `little` clause", query_type)))
        .and_then(parse_query)?;
    let big = obj
        .get("big")
        .ok_or_else(|| qerr(format!("`{}` requires a `big` clause", query_type)))
        .and_then(parse_query)?;

    let little = Box::new(little);
    let big = Box::new(big);

    Ok(match query_type {
        "span_within" => QueryNode::SpanWithin { little, big },
        // "span_containing"
        _ => QueryNode::SpanContaining { little, big },
    })
}

/// Parse a `has_child` query.
///
/// `{"has_child": {"type": "answer", "query": {...}, "score_mode": "avg"}}`
///
/// Parent-child joins are NOT supported. XERJ is single-type per index
/// and never materializes the parent/child join structure, so both the
/// planner and executor branches for `HasChild` would silently fall back
/// to running the inner query on flat docs (wrong results). We fail loud
/// (400) at parse time instead — the `QueryNode::HasChild` AST variant is
/// therefore never built, and the downstream branches are unreachable.
fn parse_has_child(_params: &Value) -> Result<QueryNode> {
    invalid(
        "parent-child join queries (has_child/has_parent) are not supported; \
         index as a single flat type or denormalize the relationship",
    )
}

/// Parse a `has_parent` query.
///
/// `{"has_parent": {"parent_type": "question", "query": {...}, "score": true}}`
///
/// See `parse_has_child`: parent-child joins are unsupported and rejected
/// with a 400 at parse time, so the `QueryNode::HasParent` variant is
/// never constructed.
fn parse_has_parent(_params: &Value) -> Result<QueryNode> {
    invalid(
        "parent-child join queries (has_child/has_parent) are not supported; \
         index as a single flat type or denormalize the relationship",
    )
}

/// Parse a `geo_polygon` query.
///
/// `{"geo_polygon": {"location": {"points": [{"lat":40,"lon":-74}, ...]}}}`
fn parse_geo_polygon(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`geo_polygon` must be an object"))?;

    // Find the field name (the first key that isn't "boost"/"_name").
    let (field, field_params) = obj
        .iter()
        .find(|(k, _)| k.as_str() != "boost" && k.as_str() != "_name")
        .ok_or_else(|| qerr("`geo_polygon` must specify a field"))?;

    let field = field.clone();

    let points_arr = field_params
        .get("points")
        .and_then(|v| v.as_array())
        .ok_or_else(|| qerr("`geo_polygon` field must have a `points` array"))?;

    let points = points_arr
        .iter()
        .map(parse_lat_lon_obj)
        .collect::<Result<Vec<_>>>()?;

    Ok(QueryNode::GeoPolygon { field, points })
}

/// Parse a geo point from a JSON object like `{"lat": 40, "lon": -74}`.
fn parse_lat_lon_obj(v: &Value) -> Result<(f64, f64)> {
    let obj = v
        .as_object()
        .ok_or_else(|| qerr("geo point must be an object {lat, lon}"))?;
    let lat = obj
        .get("lat")
        .and_then(|x| x.as_f64())
        .ok_or_else(|| qerr("geo point missing `lat`"))?;
    let lon = obj
        .get("lon")
        .and_then(|x| x.as_f64())
        .ok_or_else(|| qerr("geo point missing `lon`"))?;
    Ok((lat, lon))
}

/// Parse a `geo_shape` query.
///
/// `{"geo_shape": {"location": {"shape": {"type": "envelope", "coordinates": [[lon,lat],[lon,lat]]}}}}`
fn parse_geo_shape(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`geo_shape` must be an object"))?;

    let (field, field_params) = obj
        .iter()
        .find(|(k, _)| {
            k.as_str() != "boost" && k.as_str() != "_name" && k.as_str() != "ignore_unmapped"
        })
        .ok_or_else(|| qerr("`geo_shape` must specify a field"))?;

    let field = field.clone();

    // The shape may be under "shape" or "indexed_shape" key; we only support "shape".
    let shape_val = field_params
        .get("shape")
        .ok_or_else(|| qerr("`geo_shape` field must have a `shape` object"))?;

    let shape_type = shape_val
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();

    let shape = match shape_type.as_str() {
        "point" => {
            // coordinates: [lon, lat]
            let coords = shape_val
                .get("coordinates")
                .and_then(|v| v.as_array())
                .ok_or_else(|| qerr("`geo_shape` point needs `coordinates`"))?;
            let lon = coords.first().and_then(|v| v.as_f64()).unwrap_or(0.0);
            let lat = coords.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0);
            GeoShapeType::Point { lat, lon }
        }
        "envelope" => {
            // coordinates: [[top_left_lon, top_left_lat], [bottom_right_lon, bottom_right_lat]]
            let coords = shape_val
                .get("coordinates")
                .and_then(|v| v.as_array())
                .ok_or_else(|| qerr("`geo_shape` envelope needs `coordinates`"))?;
            let tl = coords
                .first()
                .and_then(|v| v.as_array())
                .ok_or_else(|| qerr("`geo_shape` envelope top-left missing"))?;
            let br = coords
                .get(1)
                .and_then(|v| v.as_array())
                .ok_or_else(|| qerr("`geo_shape` envelope bottom-right missing"))?;
            let tl_lon = tl.first().and_then(|v| v.as_f64()).unwrap_or(0.0);
            let tl_lat = tl.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0);
            let br_lon = br.first().and_then(|v| v.as_f64()).unwrap_or(0.0);
            let br_lat = br.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0);
            GeoShapeType::Envelope {
                top_left: (tl_lat, tl_lon),
                bottom_right: (br_lat, br_lon),
            }
        }
        "polygon" => {
            // coordinates: [[[lon, lat], ...]] (outer ring only)
            let rings = shape_val
                .get("coordinates")
                .and_then(|v| v.as_array())
                .ok_or_else(|| qerr("`geo_shape` polygon needs `coordinates`"))?;
            let outer = rings
                .first()
                .and_then(|v| v.as_array())
                .ok_or_else(|| qerr("`geo_shape` polygon outer ring missing"))?;
            let points = outer
                .iter()
                .filter_map(|p| {
                    let arr = p.as_array()?;
                    let lon = arr.first()?.as_f64()?;
                    let lat = arr.get(1)?.as_f64()?;
                    Some((lat, lon))
                })
                .collect();
            GeoShapeType::Polygon { points }
        }
        "circle" => {
            // coordinates: [lon, lat], radius: "10km" or number
            let coords = shape_val
                .get("coordinates")
                .and_then(|v| v.as_array())
                .ok_or_else(|| qerr("`geo_shape` circle needs `coordinates`"))?;
            let lon = coords.first().and_then(|v| v.as_f64()).unwrap_or(0.0);
            let lat = coords.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0);
            let radius_km = parse_distance_to_km(
                shape_val
                    .get("radius")
                    .ok_or_else(|| qerr("`geo_shape` circle needs `radius`"))?,
            )?;
            GeoShapeType::Circle {
                center: (lat, lon),
                radius_km,
            }
        }
        other => return invalid(format!("unsupported geo_shape type `{}`", other)),
    };

    Ok(QueryNode::GeoShape { field, shape })
}

/// Parse a distance value to kilometres. Accepts "10km", "5mi", or a bare number (metres).
fn parse_distance_to_km(v: &Value) -> Result<f64> {
    if let Some(n) = v.as_f64() {
        return Ok(n / 1000.0); // assume metres
    }
    if let Some(s) = v.as_str() {
        let s = s.trim().to_lowercase();
        if let Some(stripped) = s.strip_suffix("km") {
            return stripped
                .trim()
                .parse::<f64>()
                .map_err(|_| qerr("invalid distance km"));
        }
        if let Some(stripped) = s.strip_suffix("mi") {
            let miles: f64 = stripped
                .trim()
                .parse()
                .map_err(|_| qerr("invalid distance mi"))?;
            return Ok(miles * 1.60934);
        }
        if let Some(stripped) = s.strip_suffix('m') {
            let metres: f64 = stripped
                .trim()
                .parse()
                .map_err(|_| qerr("invalid distance m"))?;
            return Ok(metres / 1000.0);
        }
        if let Ok(n) = s.parse::<f64>() {
            return Ok(n / 1000.0);
        }
    }
    invalid("unrecognised distance format")
}

// ─────────────────────────────────────────────────────────────────────────────
// Gap 2: Additional ES query type parsers
// ─────────────────────────────────────────────────────────────────────────────

/// `match_bool_prefix` — tokenise the query string; the last token becomes a
/// `prefix` query and all preceding tokens become `term` queries, combined in a
/// `bool { should: [...] }`.
///
/// ES docs: <https://www.elastic.co/guide/en/elasticsearch/reference/current/query-dsl-match-bool-prefix-query.html>
fn parse_match_bool_prefix(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`match_bool_prefix` must be an object"))?;
    if obj.len() != 1 {
        return invalid("`match_bool_prefix` query must have exactly one field");
    }

    let (field, raw) = obj.iter().next().unwrap();
    let field = field.clone();

    // Long-form accepts the same options as `match` + `operator` + mm.
    let (query_str, mm, operator_and, analyzer, fuzziness) = if let Some(s) = raw.as_str() {
        (s.to_string(), None, false, None, None)
    } else {
        let inner = raw
            .as_object()
            .ok_or_else(|| qerr("`match_bool_prefix` field value must be a string or object"))?;
        let q = string_field(inner, "query")?;
        let mm_val = inner.get("minimum_should_match").and_then(|v| match v {
            Value::Number(n) => n.as_u64().map(|i| MinShouldMatch::Fixed(i as u32)),
            Value::String(s) => {
                if let Some(p) = s.strip_suffix('%') {
                    p.parse::<u32>().ok().map(MinShouldMatch::Percentage)
                } else {
                    s.parse::<u32>().ok().map(MinShouldMatch::Fixed)
                }
            }
            _ => None,
        });
        let op_and = matches!(
            inner
                .get("operator")
                .and_then(Value::as_str)
                .map(|s| s.to_ascii_lowercase())
                .as_deref(),
            Some("and")
        );
        let analyzer = inner
            .get("analyzer")
            .and_then(Value::as_str)
            .map(str::to_string);
        let fuzziness = inner.get("fuzziness").map(|v| match v {
            Value::String(s) if s.eq_ignore_ascii_case("auto") => crate::ast::Fuzziness::Auto,
            Value::String(s) => s
                .parse::<u32>()
                .ok()
                .map(crate::ast::Fuzziness::Fixed)
                .unwrap_or(crate::ast::Fuzziness::Auto),
            Value::Number(n) => crate::ast::Fuzziness::Fixed(n.as_u64().unwrap_or(0) as u32),
            _ => crate::ast::Fuzziness::Auto,
        });
        (q, mm_val, op_and, analyzer, fuzziness)
    };

    // Analyzer semantics: "whitespace" preserves case, "keyword" treats
    // input as single token, default/standard lowercases. We approximate
    // by case-folding unless the requested analyzer is case-preserving.
    let analyzer_lowercases = !matches!(analyzer.as_deref(), Some("whitespace") | Some("keyword"));
    let fold = |t: &str| -> String {
        if analyzer_lowercases {
            t.to_lowercase()
        } else {
            t.to_string()
        }
    };

    let raw_tokens: Vec<String> = if analyzer.as_deref() == Some("keyword") {
        vec![query_str.clone()]
    } else {
        query_str.split_whitespace().map(str::to_string).collect()
    };
    if raw_tokens.is_empty() {
        return Ok(QueryNode::MatchAll);
    }

    if raw_tokens.len() == 1 {
        return Ok(QueryNode::Prefix {
            field,
            value: fold(&raw_tokens[0]),
            boost: None,
            constant_score: false,
        });
    }

    let last_idx = raw_tokens.len() - 1;
    let build_leaf = |tok: &str| -> QueryNode {
        let folded = fold(tok);
        if let Some(fz) = fuzziness {
            QueryNode::Fuzzy {
                field: field.clone(),
                value: folded,
                fuzziness: fz,
            }
        } else {
            QueryNode::Match {
                field: field.clone(),
                query: folded,
                operator: BoolOperator::Or,
                boost: None,
                analyzer: analyzer.clone(),
                minimum_should_match: None,
            }
        }
    };
    let mut clauses: Vec<QueryNode> = raw_tokens[..last_idx]
        .iter()
        .map(|t| build_leaf(t))
        .collect();
    clauses.push(QueryNode::Prefix {
        field: field.clone(),
        value: fold(&raw_tokens[last_idx]),
        boost: None,
        constant_score: false,
    });

    // operator:and → every clause must match; otherwise should + mm.
    let (must, should, mm_final) = if operator_and {
        (clauses, vec![], None)
    } else {
        (vec![], clauses, mm)
    };

    Ok(QueryNode::Bool {
        must,
        should,
        filter: vec![],
        must_not: vec![],
        minimum_should_match: mm_final,
    })
}

/// `terms_set` — like `terms` but with a per-query `minimum_should_match`.
/// Parsed as `Bool { should: [term1, term2, ...], minimum_should_match: N }`.
///
/// ES docs: <https://www.elastic.co/guide/en/elasticsearch/reference/current/query-dsl-terms-set-query.html>
fn parse_terms_set(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`terms_set` must be an object"))?;

    let field_entries: Vec<_> = obj.iter().filter(|(k, _)| k.as_str() != "boost").collect();

    if field_entries.len() != 1 {
        return invalid("`terms_set` query must have exactly one field entry");
    }

    let (field, raw) = field_entries[0];
    let field = field.clone();

    let inner = raw
        .as_object()
        .ok_or_else(|| qerr("`terms_set` field value must be an object"))?;

    let terms = inner
        .get("terms")
        .and_then(Value::as_array)
        .ok_or_else(|| qerr("`terms_set.terms` must be an array"))?
        .clone();

    // `minimum_should_match_field` reads the per-doc required count from a
    // numeric field; `minimum_should_match_script` computes it from a
    // Painless script. Fall back to a literal `minimum_should_match`.
    let minimum_should_match = if let Some(field_name) = inner
        .get("minimum_should_match_field")
        .and_then(Value::as_str)
    {
        Some(crate::ast::MinShouldMatch::Field(field_name.to_string()))
    } else if let Some(script) = inner.get("minimum_should_match_script") {
        let source = script
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| qerr("`terms_set.minimum_should_match_script` requires a `source`"))?
            .to_string();
        let params = script.get("params").cloned();
        Some(crate::ast::MinShouldMatch::Script { source, params })
    } else {
        inner
            .get("minimum_should_match")
            .map(parse_min_should_match)
            .and_then(|r| r.ok())
    };

    let should: Vec<QueryNode> = terms
        .iter()
        .map(|v| QueryNode::Term {
            field: field.clone(),
            value: v.clone(),
            boost: None,
        })
        .collect();

    Ok(QueryNode::Bool {
        must: vec![],
        should,
        filter: vec![],
        must_not: vec![],
        minimum_should_match,
    })
}

/// `intervals` — position-aware match query.  xerj does not implement a full
/// interval automaton; we translate each rule to the closest Bool/Match/Phrase
/// equivalent.
///
/// * `match`: `ordered: true` → `match_phrase` with slop from `max_gaps`
///   (unset → large, effectively ordered-anywhere).
///   `ordered: false` (default) → `match` with operator AND
///   (all query tokens must be present; we do not model gap bounds here).
/// * `prefix`: → `prefix` query.
/// * `wildcard`: → `wildcard` query.
/// * `all_of` / `any_of`: → bool { must / should }.
///
/// ES docs: <https://www.elastic.co/guide/en/elasticsearch/reference/current/query-dsl-intervals-query.html>
fn parse_intervals(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`intervals` must be an object"))?;
    if obj.len() != 1 {
        return invalid("`intervals` query must have exactly one field");
    }
    let (field, raw) = obj.iter().next().unwrap();
    // Emit a specialised `Intervals` node — the executor evaluates the
    // rule (match/all_of/any_of/prefix/wildcard/fuzzy, with optional
    // filter:) against the field's tokenised positions at query time.
    Ok(QueryNode::Intervals {
        field: field.clone(),
        rule: raw.clone(),
    })
}

/// `script_score` — wrap the inner query and apply a Painless script
/// to compute each matched doc's score. The script's returned value
/// REPLACES the BM25 score (boost_mode: Replace).
///
/// ES docs: <https://www.elastic.co/guide/en/elasticsearch/reference/current/query-dsl-script-score-query.html>
fn parse_script_score(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`script_score` must be an object"))?;

    let inner_query_val = obj
        .get("query")
        .ok_or_else(|| qerr("`script_score.query` is required"))?;

    let inner = parse_query(inner_query_val)?;

    // Pull out the script source + params if present.
    let script = obj.get("script").and_then(|s| s.as_object());
    let source = script.and_then(|s| s.get("source").and_then(|v| v.as_str()).map(String::from));
    let s_params = script.and_then(|s| s.get("params").cloned());

    // If the script is just a numeric literal (e.g. `"3"`), shortcut
    // into the existing literal-score fast path.
    let literal_score = source.as_ref().and_then(|s| s.trim().parse::<f32>().ok());

    let func = ScoreFunction {
        filter: None,
        weight: None,
        field_value_factor: None,
        random_score: None,
        script_score: literal_score,
        script_source: if literal_score.is_some() {
            None
        } else {
            source
        },
        script_params: s_params,
        name: None,
        distance_feature: None,
        rank_feature: None,
    };

    Ok(QueryNode::FunctionScore {
        query: Box::new(inner),
        functions: vec![func],
        score_mode: crate::ast::ScoreMode::Multiply,
        // `script_score` replaces the inner query score with the script's
        // returned value (the script can incorporate `_score` if it
        // wants to combine with the inner score).
        boost_mode: crate::ast::BoostMode::Replace,
        max_boost: None,
    })
}

/// `distance_feature` — date/geo distance proximity scoring.  xerj converts
/// this to a `function_score` with a `field_value_factor` on the target field
/// so the query still filters correctly (the proximity score boost is not
/// computed exactly but the field is at least scored).
///
/// ES docs: <https://www.elastic.co/guide/en/elasticsearch/reference/current/query-dsl-distance-feature-query.html>
fn parse_distance_feature(params: &Value) -> Result<QueryNode> {
    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`distance_feature` must be an object"))?;

    let field = string_field(obj, "field")?;
    let boost = obj.get("boost").and_then(|v| v.as_f64()).map(|b| b as f32);
    let pivot = string_field(obj, "pivot")?;
    let origin = obj
        .get("origin")
        .cloned()
        .ok_or_else(|| qerr("`distance_feature.origin` is required"))?;

    let df = crate::ast::DistanceFeature {
        field: field.clone(),
        pivot,
        origin,
    };

    Ok(QueryNode::FunctionScore {
        // Hybrid: the inner query returns every doc that has the field
        // (so the function_score runs on the full candidate set) and
        // `boost_mode: replace` substitutes the distance score.
        query: Box::new(QueryNode::Exists { field }),
        functions: vec![ScoreFunction {
            filter: None,
            weight: boost,
            field_value_factor: None,
            random_score: None,
            script_score: None,
            script_source: None,
            script_params: None,
            name: None,
            distance_feature: Some(df),
            rank_feature: None,
        }],
        score_mode: ScoreMode::Multiply,
        boost_mode: BoostMode::Replace,
        max_boost: None,
    })
}

/// `rank_feature` — logarithmic proximity scoring on a numeric field.
/// Converted to a `function_score` with a `field_value_factor` and `log1p`
/// modifier so the field is scored proportionally to its value.
///
/// ES docs: <https://www.elastic.co/guide/en/elasticsearch/reference/current/query-dsl-rank-feature-query.html>
fn parse_rank_feature(params: &Value) -> Result<QueryNode> {
    use crate::ast::{RankFeature, RankFeatureFn};

    let obj = params
        .as_object()
        .ok_or_else(|| qerr("`rank_feature` must be an object"))?;

    let field = string_field(obj, "field")?;
    let boost = obj.get("boost").and_then(|v| v.as_f64()).map(|b| b as f32);

    // Detect which of the four ES functions is requested. Default to
    // `saturation` (with the field's default pivot) when none is given.
    let function = if let Some(sat) = obj.get("saturation") {
        let pivot = sat.get("pivot").and_then(|v| v.as_f64());
        RankFeatureFn::Saturation { pivot }
    } else if let Some(log) = obj.get("log") {
        let scaling_factor = log
            .get("scaling_factor")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| qerr("`rank_feature.log` requires a `scaling_factor`"))?;
        RankFeatureFn::Log { scaling_factor }
    } else if let Some(sig) = obj.get("sigmoid") {
        let pivot = sig
            .get("pivot")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| qerr("`rank_feature.sigmoid` requires a `pivot`"))?;
        let exponent = sig
            .get("exponent")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| qerr("`rank_feature.sigmoid` requires an `exponent`"))?;
        RankFeatureFn::Sigmoid { pivot, exponent }
    } else if obj.contains_key("linear") {
        RankFeatureFn::Linear
    } else {
        RankFeatureFn::Saturation { pivot: None }
    };

    let rank_feature = RankFeature {
        field: field.clone(),
        function,
    };

    Ok(QueryNode::FunctionScore {
        // The inner Exists query supplies the candidate set (all docs that
        // have the rank_feature field); `boost_mode: replace` substitutes
        // the feature's own score for the query score.
        query: Box::new(QueryNode::Exists { field }),
        functions: vec![ScoreFunction {
            filter: None,
            weight: boost,
            field_value_factor: None,
            random_score: None,
            script_score: None,
            script_source: None,
            script_params: None,
            name: None,
            distance_feature: None,
            rank_feature: Some(rank_feature),
        }],
        score_mode: ScoreMode::Multiply,
        boost_mode: BoostMode::Replace,
        max_boost: None,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn q(j: serde_json::Value) -> QueryNode {
        parse_query(&j).expect("parse failed")
    }

    // ── match_all ─────────────────────────────────────────────────────────────

    #[test]
    fn test_match_all() {
        assert_eq!(q(json!({"match_all": {}})), QueryNode::MatchAll);
    }

    #[test]
    fn test_match_all_with_boost() {
        // ES scores match_all{boost} as `boost` per hit (constant-score
        // semantics) — the boost must not be dropped.
        assert_eq!(
            q(json!({"match_all": {"boost": 2.0}})),
            QueryNode::Constant {
                score: 2.0,
                query: Box::new(QueryNode::MatchAll),
            }
        );
    }

    #[test]
    fn test_match_none() {
        assert_eq!(q(json!({"match_none": {}})), QueryNode::MatchNone);
    }

    // ── match ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_match_shorthand() {
        let node = q(json!({"match": {"title": "hello world"}}));
        assert!(
            matches!(node, QueryNode::Match { ref field, ref query, operator: BoolOperator::Or, .. }
            if field == "title" && query == "hello world")
        );
    }

    #[test]
    fn test_match_long_form() {
        let node = q(json!({
            "match": {
                "title": {
                    "query": "hello world",
                    "operator": "AND",
                    "analyzer": "english"
                }
            }
        }));
        if let QueryNode::Match {
            field,
            query,
            operator,
            analyzer,
            ..
        } = node
        {
            assert_eq!(field, "title");
            assert_eq!(query, "hello world");
            assert_eq!(operator, BoolOperator::And);
            assert_eq!(analyzer, Some("english".to_string()));
        } else {
            panic!("wrong variant");
        }
    }

    // ── track_total_hits ──────────────────────────────────────────────────────

    #[test]
    fn test_track_total_hits_parsing() {
        // Absent → exact tracking at the parser level (the ES 10k default is
        // injected by the HTTP layer, NOT here — `_count` and internal
        // sub-searches rely on the parser staying exact when unset).
        let req = parse_request(&json!({"query": {"match_all": {}}})).unwrap();
        assert_eq!(req.track_total_hits, TrackTotalHits::True);

        // true → exact.
        let req = parse_request(&json!({"track_total_hits": true})).unwrap();
        assert_eq!(req.track_total_hits, TrackTotalHits::True);

        // false → don't track.
        let req = parse_request(&json!({"track_total_hits": false})).unwrap();
        assert_eq!(req.track_total_hits, TrackTotalHits::False);

        // Integer N → cap at N.
        let req = parse_request(&json!({"track_total_hits": 10_000})).unwrap();
        assert_eq!(req.track_total_hits, TrackTotalHits::Limit(10_000));

        // -1 is the ES alias for `true` (exact), NOT a 10k cap.
        let req = parse_request(&json!({"track_total_hits": -1})).unwrap();
        assert_eq!(req.track_total_hits, TrackTotalHits::True);
    }

    // ── match_phrase ──────────────────────────────────────────────────────────

    #[test]
    fn test_match_phrase_shorthand() {
        let node = q(json!({"match_phrase": {"body": "quick brown fox"}}));
        assert!(
            matches!(node, QueryNode::MatchPhrase { ref field, slop: 0, .. }
            if field == "body")
        );
    }

    #[test]
    fn test_match_phrase_with_slop() {
        let node = q(json!({"match_phrase": {"body": {"query": "quick fox", "slop": 2}}}));
        assert!(matches!(node, QueryNode::MatchPhrase { slop: 2, .. }));
    }

    // ── multi_match ───────────────────────────────────────────────────────────

    #[test]
    fn test_multi_match_basic() {
        let node = q(json!({
            "multi_match": {
                "query": "hello",
                "fields": ["title", "body^2"]
            }
        }));
        if let QueryNode::MultiMatch {
            fields,
            query,
            match_type,
            ..
        } = node
        {
            assert_eq!(query, "hello");
            assert_eq!(fields, vec!["title", "body^2"]);
            assert_eq!(match_type, MultiMatchType::BestFields);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn test_multi_match_cross_fields() {
        let node = q(json!({
            "multi_match": {
                "query": "Will Smith",
                "fields": ["first_name", "last_name"],
                "type": "cross_fields",
                "operator": "AND"
            }
        }));
        assert!(matches!(
            node,
            QueryNode::MultiMatch {
                match_type: MultiMatchType::CrossFields,
                ..
            }
        ));
    }

    // ── term ──────────────────────────────────────────────────────────────────

    #[test]
    fn test_term_shorthand() {
        let node = q(json!({"term": {"status": "active"}}));
        assert!(matches!(node, QueryNode::Term { ref field, ref value, .. }
            if field == "status" && *value == json!("active")));
    }

    #[test]
    fn test_term_long_form() {
        let node = q(json!({"term": {"status": {"value": "active", "boost": 1.5}}}));
        assert!(matches!(node, QueryNode::Term { boost: Some(b), .. } if (b - 1.5).abs() < 0.001));
    }

    #[test]
    fn test_term_numeric() {
        let node = q(json!({"term": {"age": 30}}));
        assert!(matches!(node, QueryNode::Term { ref field, ref value, .. }
            if field == "age" && *value == json!(30)));
    }

    // ── terms ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_terms() {
        let node = q(json!({"terms": {"status": ["active", "pending"]}}));
        if let QueryNode::Terms { field, values, .. } = node {
            assert_eq!(field, "status");
            assert_eq!(values, vec![json!("active"), json!("pending")]);
        } else {
            panic!("wrong variant");
        }
    }

    // ── range ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_range_numeric() {
        let node = q(json!({"range": {"age": {"gte": 18, "lt": 65}}}));
        if let QueryNode::Range {
            field,
            gte,
            lt,
            gt,
            lte,
            ..
        } = node
        {
            assert_eq!(field, "age");
            assert_eq!(gte, Some(json!(18)));
            assert_eq!(lt, Some(json!(65)));
            assert_eq!(gt, None);
            assert_eq!(lte, None);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn test_range_date() {
        let node = q(json!({"range": {"date": {"gte": "2024-01-01", "lte": "2024-12-31"}}}));
        assert!(matches!(node, QueryNode::Range { ref field, .. } if field == "date"));
    }

    #[test]
    fn test_range_no_bounds_fails() {
        assert!(parse_query(&json!({"range": {"age": {}}})).is_err());
    }

    // ── prefix ────────────────────────────────────────────────────────────────

    #[test]
    fn test_prefix_shorthand() {
        let node = q(json!({"prefix": {"name": "Joh"}}));
        assert!(
            matches!(node, QueryNode::Prefix { ref field, ref value, .. }
            if field == "name" && value == "Joh")
        );
    }

    #[test]
    fn test_prefix_long_form() {
        let node = q(json!({"prefix": {"name": {"value": "Joh", "boost": 2.0}}}));
        assert!(matches!(node, QueryNode::Prefix { ref value, .. } if value == "Joh"));
    }

    // ── wildcard ──────────────────────────────────────────────────────────────

    #[test]
    fn test_wildcard() {
        let node = q(json!({"wildcard": {"name": "Jo*n"}}));
        assert!(
            matches!(node, QueryNode::Wildcard { ref field, ref value, .. }
            if field == "name" && value == "Jo*n")
        );
    }

    // ── exists ────────────────────────────────────────────────────────────────

    #[test]
    fn test_exists() {
        let node = q(json!({"exists": {"field": "email"}}));
        assert!(matches!(node, QueryNode::Exists { ref field } if field == "email"));
    }

    // ── ids ───────────────────────────────────────────────────────────────────

    #[test]
    fn test_ids() {
        let node = q(json!({"ids": {"values": ["abc", "def", "123"]}}));
        if let QueryNode::Ids { values } = node {
            assert_eq!(values, vec!["abc", "def", "123"]);
        } else {
            panic!("wrong variant");
        }
    }

    // ── bool ──────────────────────────────────────────────────────────────────

    #[test]
    fn test_bool_basic() {
        let node = q(json!({
            "bool": {
                "must": [{"term": {"status": "active"}}],
                "must_not": [{"term": {"deleted": true}}],
                "filter": [{"range": {"age": {"gte": 18}}}]
            }
        }));
        if let QueryNode::Bool {
            must,
            must_not,
            filter,
            should,
            ..
        } = node
        {
            assert_eq!(must.len(), 1);
            assert_eq!(must_not.len(), 1);
            assert_eq!(filter.len(), 1);
            assert!(should.is_empty());
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn test_bool_single_must_not_array() {
        let node = q(json!({
            "bool": {
                "must": {"match": {"title": "rust"}}
            }
        }));
        if let QueryNode::Bool { must, .. } = node {
            assert_eq!(must.len(), 1);
        }
    }

    #[test]
    fn test_bool_empty_is_match_all() {
        let node = q(json!({"bool": {}}));
        assert_eq!(node, QueryNode::MatchAll);
    }

    #[test]
    fn test_bool_minimum_should_match() {
        let node = q(json!({
            "bool": {
                "should": [
                    {"term": {"tag": "a"}},
                    {"term": {"tag": "b"}},
                    {"term": {"tag": "c"}}
                ],
                "minimum_should_match": 2
            }
        }));
        if let QueryNode::Bool {
            minimum_should_match,
            ..
        } = node
        {
            assert_eq!(minimum_should_match, Some(MinShouldMatch::Fixed(2)));
        }
    }

    #[test]
    fn test_bool_minimum_should_match_pct() {
        let node = q(json!({
            "bool": {
                "should": [{"term": {"tag": "a"}}],
                "minimum_should_match": "75%"
            }
        }));
        if let QueryNode::Bool {
            minimum_should_match,
            ..
        } = node
        {
            assert_eq!(minimum_should_match, Some(MinShouldMatch::Percentage(75)));
        }
    }

    // ── constant_score ────────────────────────────────────────────────────────

    #[test]
    fn test_constant_score() {
        // constant_score keeps its Constant wrapper: ES scores every hit as
        // exactly `boost` (default 1.0), NOT the inner query's score — the
        // old flatten returned the inner term's brute score (live-diverged
        // vs ES 8.13.4). Matching fast paths peel the wrapper instead.
        let node = q(json!({
            "constant_score": {
                "filter": {"term": {"status": "active"}},
                "boost": 1.5
            }
        }));
        match node {
            QueryNode::Constant { score, query } => {
                assert_eq!(score, 1.5);
                assert!(matches!(*query, QueryNode::Term { .. }));
            }
            other => panic!("expected Constant wrapper, got {other:?}"),
        }
    }

    #[test]
    fn test_constant_score_default_boost() {
        let node = q(json!({
            "constant_score": {"filter": {"term": {"status": "active"}}}
        }));
        match node {
            QueryNode::Constant { score, .. } => assert_eq!(score, 1.0),
            other => panic!("expected Constant wrapper, got {other:?}"),
        }
    }

    // ── parent-child join (unsupported) ────────────────────────────────────────

    #[test]
    fn test_has_child_rejected() {
        // Parent-child joins are not materialized in XERJ; parsing must
        // fail loud (400) rather than build an AST node that silently runs
        // the inner query on flat docs.
        let err = parse_query(&json!({
            "has_child": {
                "type": "answer",
                "query": { "match": { "body": "hello" } }
            }
        }))
        .unwrap_err();
        assert!(
            format!("{err}").contains("parent-child join queries"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_has_parent_rejected() {
        let err = parse_query(&json!({
            "has_parent": {
                "parent_type": "question",
                "query": { "match": { "body": "hello" } }
            }
        }))
        .unwrap_err();
        assert!(
            format!("{err}").contains("parent-child join queries"),
            "unexpected error: {err}"
        );
    }

    // ── query_string ──────────────────────────────────────────────────────────

    #[test]
    fn test_query_string() {
        // `field:value AND field:value` is lowered into a Bool tree by
        // try_lower_query_string so the FTS / planner fast paths apply.
        // The opaque QueryString variant is kept only for inputs the
        // lowerer can't translate.
        let node = q(json!({
            "query_string": {
                "query": "title:rust AND body:ownership",
                "default_field": "title"
            }
        }));
        assert!(matches!(node, QueryNode::Bool { .. }));
    }

    fn qs(query: &str) -> QueryNode {
        q(json!({"query_string": {"query": query}}))
    }

    fn expect_range(
        node: QueryNode,
    ) -> (
        String,
        Option<Value>,
        Option<Value>,
        Option<Value>,
        Option<Value>,
    ) {
        match node {
            QueryNode::Range {
                field,
                gt,
                gte,
                lt,
                lte,
                ..
            } => (field, gt, gte, lt, lte),
            other => panic!("expected Range, got {other:?}"),
        }
    }

    #[test]
    fn test_query_string_range_gt() {
        let (field, gt, gte, lt, lte) = expect_range(qs("n:>1"));
        assert_eq!(field, "n");
        assert_eq!(gt, Some(json!(1)));
        assert!(gte.is_none() && lt.is_none() && lte.is_none());
    }

    #[test]
    fn test_query_string_range_gte() {
        let (field, gt, gte, lt, lte) = expect_range(qs("n:>=2"));
        assert_eq!(field, "n");
        assert_eq!(gte, Some(json!(2)));
        assert!(gt.is_none() && lt.is_none() && lte.is_none());
    }

    #[test]
    fn test_query_string_range_lt() {
        let (field, gt, gte, lt, lte) = expect_range(qs("n:<5"));
        assert_eq!(field, "n");
        assert_eq!(lt, Some(json!(5)));
        assert!(gt.is_none() && gte.is_none() && lte.is_none());
    }

    #[test]
    fn test_query_string_range_lte() {
        let (field, _, _, _, lte) = expect_range(qs("n:<=5"));
        assert_eq!(field, "n");
        assert_eq!(lte, Some(json!(5)));
    }

    #[test]
    fn test_query_string_range_inclusive_brackets() {
        let (field, gt, gte, lt, lte) = expect_range(qs("n:[2 TO 5]"));
        assert_eq!(field, "n");
        assert_eq!(gte, Some(json!(2)));
        assert_eq!(lte, Some(json!(5)));
        assert!(gt.is_none() && lt.is_none());
    }

    #[test]
    fn test_query_string_range_exclusive_brackets() {
        let (field, gt, gte, lt, lte) = expect_range(qs("n:{2 TO 5}"));
        assert_eq!(field, "n");
        assert_eq!(gt, Some(json!(2)));
        assert_eq!(lt, Some(json!(5)));
        assert!(gte.is_none() && lte.is_none());
    }

    #[test]
    fn test_query_string_range_mixed_brackets() {
        let (field, gt, gte, lt, lte) = expect_range(qs("n:[2 TO 5}"));
        assert_eq!(field, "n");
        assert_eq!(gte, Some(json!(2)));
        assert_eq!(lt, Some(json!(5)));
        assert!(gt.is_none() && lte.is_none());
    }

    #[test]
    fn test_query_string_range_open_upper() {
        let (field, gt, gte, lt, lte) = expect_range(qs("n:[2 TO *]"));
        assert_eq!(field, "n");
        assert_eq!(gte, Some(json!(2)));
        assert!(gt.is_none() && lt.is_none() && lte.is_none());
    }

    #[test]
    fn test_query_string_range_open_lower() {
        let (field, gt, gte, lt, lte) = expect_range(qs("n:[* TO 5]"));
        assert_eq!(field, "n");
        assert_eq!(lte, Some(json!(5)));
        assert!(gt.is_none() && gte.is_none() && lt.is_none());
    }

    #[test]
    fn test_query_string_range_negative_and_float() {
        let (_, gt, ..) = expect_range(qs("n:>-1"));
        assert_eq!(gt, Some(json!(-1)));
        let (_, gt, ..) = expect_range(qs("n:>1.5"));
        assert_eq!(gt, Some(json!(1.5)));
    }

    #[test]
    fn test_query_string_range_date() {
        let (field, gt, ..) = expect_range(qs("ts:>2020-01-01"));
        assert_eq!(field, "ts");
        assert_eq!(gt, Some(json!("2020-01-01")));
        let (field, _, gte, _, lte) = expect_range(qs("ts:[2020-01-01 TO 2020-12-31]"));
        assert_eq!(field, "ts");
        assert_eq!(gte, Some(json!("2020-01-01")));
        assert_eq!(lte, Some(json!("2020-12-31")));
    }

    #[test]
    fn test_query_string_range_combined_and() {
        // "msg:foo AND n:>1" → Bool.must = [Match(msg), Range(n, gt 1)]
        let node = qs("msg:foo AND n:>1");
        let QueryNode::Bool {
            must,
            must_not,
            should,
            ..
        } = node
        else {
            panic!("expected Bool");
        };
        assert!(must_not.is_empty() && should.is_empty());
        assert_eq!(must.len(), 2);
        assert!(matches!(&must[0], QueryNode::Match { field, .. } if field == "msg"));
        let (field, gt, ..) = expect_range(must[1].clone());
        assert_eq!(field, "n");
        assert_eq!(gt, Some(json!(1)));
    }

    #[test]
    fn test_query_string_range_combined_bracket_and() {
        let node = qs("msg:foo AND n:[2 TO *]");
        let QueryNode::Bool { must, .. } = node else {
            panic!("expected Bool")
        };
        assert_eq!(must.len(), 2);
        let (field, _, gte, ..) = expect_range(must[1].clone());
        assert_eq!(field, "n");
        assert_eq!(gte, Some(json!(2)));
    }

    #[test]
    fn test_query_string_range_default_field() {
        // Unqualified range resolves against default_field.
        let node = q(json!({
            "query_string": {"query": ">10", "default_field": "n"}
        }));
        let (field, gt, ..) = expect_range(node);
        assert_eq!(field, "n");
        assert_eq!(gt, Some(json!(10)));
    }

    #[test]
    fn test_query_string_range_errors_do_not_degrade() {
        // Malformed / unrepresentable ranges must be parse errors,
        // never a silent term match.
        for bad in [
            "n:[2 TO",    // unterminated bracket
            "n:[2 5]",    // missing TO
            "n:>",        // missing value
            "n:[* TO *]", // no usable bound
            ">10",        // no field and no default_field
        ] {
            let res = parse_query(&json!({"query_string": {"query": bad}}));
            assert!(
                res.is_err(),
                "expected parse error for {bad:?}, got {res:?}"
            );
        }
    }

    // ── knn ───────────────────────────────────────────────────────────────────

    #[test]
    fn test_knn() {
        let node = q(json!({
            "knn": {
                "field": "embedding",
                "query_vector": [0.1, 0.2, 0.3],
                "k": 5
            }
        }));
        if let QueryNode::Knn {
            field,
            vector,
            k,
            filter,
            ..
        } = node
        {
            assert_eq!(field, "embedding");
            assert_eq!(k, 5);
            assert_eq!(vector.len(), 3);
            assert!(filter.is_none());
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn test_knn_num_candidates_independent_of_k() {
        // num_candidates is carried through as its own value; k stays explicit.
        let node = q(json!({
            "knn": {
                "field": "embedding",
                "query_vector": [0.1, 0.2, 0.3],
                "k": 3,
                "num_candidates": 50
            }
        }));
        match node {
            QueryNode::Knn {
                k, num_candidates, ..
            } => {
                assert_eq!(k, 3);
                assert_eq!(num_candidates, Some(50));
            }
            other => panic!("expected knn, got {:?}", other),
        }
    }

    #[test]
    fn test_knn_k_defaults_to_num_candidates_when_k_omitted() {
        // With k omitted, k still derives from num_candidates (unchanged).
        let node = q(json!({
            "knn": {
                "field": "embedding",
                "query_vector": [0.1],
                "num_candidates": 7
            }
        }));
        match node {
            QueryNode::Knn {
                k, num_candidates, ..
            } => {
                assert_eq!(k, 7);
                assert_eq!(num_candidates, Some(7));
            }
            other => panic!("expected knn, got {:?}", other),
        }
    }

    #[test]
    fn test_knn_with_filter() {
        let node = q(json!({
            "knn": {
                "field": "embedding",
                "query_vector": [0.1, 0.2],
                "k": 10,
                "filter": {"term": {"status": "active"}}
            }
        }));
        assert!(matches!(
            node,
            QueryNode::Knn {
                filter: Some(_),
                ..
            }
        ));
    }

    // ── full request ──────────────────────────────────────────────────────────

    #[test]
    fn test_parse_full_request() {
        let body = json!({
            "query": {
                "bool": {
                    "must": [{"match": {"title": "rust"}}],
                    "filter": [{"term": {"status": "published"}}]
                }
            },
            "from": 20,
            "size": 5,
            "sort": [{"date": "desc"}, "_score"],
            "_source": ["title", "date"],
            "explain": true
        });
        let req = parse_request(&body).expect("parse_request failed");
        assert_eq!(req.from, 20);
        assert_eq!(req.size, 5);
        assert_eq!(req.sort.len(), 2);
        assert!(req.explain);
        assert!(matches!(req.query, QueryNode::Bool { .. }));
    }

    #[test]
    fn test_parse_request_defaults() {
        let req = parse_request(&json!({})).expect("should default to match_all");
        assert_eq!(req.query, QueryNode::MatchAll);
        assert_eq!(req.from, 0);
        assert_eq!(req.size, 10);
    }

    // ── error cases ───────────────────────────────────────────────────────────

    #[test]
    fn test_unknown_query_type_error() {
        let err = parse_query(&json!({"fuzzy_wuzzy": {}})).unwrap_err();
        assert!(err.to_string().contains("fuzzy_wuzzy"));
    }

    #[test]
    fn test_multi_key_object_error() {
        let err = parse_query(&json!({"match": {}, "term": {}})).unwrap_err();
        assert!(err.to_string().contains("exactly one key"));
    }

    #[test]
    fn test_hybrid_query() {
        let node = q(json!({
            "hybrid": {
                "queries": [
                    {"query": {"match": {"title": "rust"}}, "weight": 0.7},
                    {"query": {"knn": {"field": "vec", "query_vector": [0.1], "k": 5}}, "weight": 0.3}
                ],
                "fusion": "rrf"
            }
        }));
        if let QueryNode::Hybrid { queries, fusion } = node {
            assert_eq!(queries.len(), 2);
            assert!((queries[0].weight - 0.7).abs() < 0.001);
            assert!(matches!(fusion, FusionStrategy::Rrf { k: 60 }));
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn test_hybrid_learned_rejected() {
        // Learned fusion is not implemented — must fail loud rather than
        // silently substituting RRF. Both the string and object forms.
        for fusion in [json!("learned"), json!({ "type": "learned" })] {
            let err = parse_query(&json!({
                "hybrid": {
                    "queries": [
                        {"query": {"match": {"title": "rust"}}},
                        {"query": {"match": {"title": "go"}}}
                    ],
                    "fusion": fusion
                }
            }))
            .unwrap_err();
            assert!(
                format!("{err}").contains("learned is not yet supported"),
                "unexpected error: {err}"
            );
        }
    }

    #[test]
    fn test_nested_bool() {
        let node = q(json!({
            "bool": {
                "must": [
                    {
                        "bool": {
                            "should": [
                                {"term": {"tag": "rust"}},
                                {"term": {"tag": "systems"}}
                            ]
                        }
                    },
                    {"range": {"year": {"gte": 2020}}}
                ]
            }
        }));
        assert!(matches!(node, QueryNode::Bool { .. }));
    }

    // ── percolate ─────────────────────────────────────────────────────────────

    #[test]
    fn test_percolate_single_document() {
        let node = q(json!({
            "percolate": { "field": "query", "document": { "message": "hi" } }
        }));
        match node {
            QueryNode::Percolate { field, documents } => {
                assert_eq!(field, "query");
                assert_eq!(documents.len(), 1);
                assert_eq!(documents[0]["message"], json!("hi"));
            }
            other => panic!("expected percolate, got {:?}", other),
        }
    }

    #[test]
    fn test_percolate_multiple_documents() {
        let node = q(json!({
            "percolate": {
                "field": "query",
                "documents": [ { "message": "a" }, { "message": "b" } ]
            }
        }));
        match node {
            QueryNode::Percolate { documents, .. } => assert_eq!(documents.len(), 2),
            other => panic!("expected percolate, got {:?}", other),
        }
    }

    #[test]
    fn test_percolate_missing_document_is_400() {
        // Neither `document` nor `documents` present.
        assert!(parse_query(&json!({ "percolate": { "field": "query" } })).is_err());
        // Index/id fetch form is rejected.
        assert!(parse_query(&json!({
            "percolate": { "field": "query", "index": "i", "id": "1" }
        }))
        .is_err());
        // Missing field is rejected.
        assert!(parse_query(&json!({
            "percolate": { "document": { "m": "x" } }
        }))
        .is_err());
    }

    // ── span_containing / span_within ───────────────────────────────────────

    #[test]
    fn test_span_containing_parses_little_and_big() {
        let node = q(json!({
            "span_containing": {
                "little": { "span_term": { "text": "brown" } },
                "big": {
                    "span_near": {
                        "clauses": [
                            { "span_term": { "text": "quick" } },
                            { "span_term": { "text": "fox" } }
                        ],
                        "slop": 3,
                        "in_order": true
                    }
                }
            }
        }));
        match node {
            QueryNode::SpanContaining { little, big } => {
                assert!(matches!(*little, QueryNode::SpanTerm { .. }));
                assert!(matches!(*big, QueryNode::SpanNear { .. }));
            }
            other => panic!("expected span_containing, got {:?}", other),
        }
    }

    #[test]
    fn test_span_within_variant() {
        let node = q(json!({
            "span_within": {
                "little": { "span_term": { "text": "brown" } },
                "big": { "span_term": { "text": "quick" } }
            }
        }));
        assert!(matches!(node, QueryNode::SpanWithin { .. }));
    }

    #[test]
    fn test_span_containing_missing_clause_is_400() {
        assert!(parse_query(&json!({
            "span_containing": { "big": { "span_term": { "text": "x" } } }
        }))
        .is_err());
        assert!(parse_query(&json!({
            "span_within": { "little": { "span_term": { "text": "x" } } }
        }))
        .is_err());
    }

    // ── terms_set minimum_should_match_field / _script ──────────────────────

    #[test]
    fn test_terms_set_minimum_should_match_field() {
        let node = q(json!({
            "terms_set": {
                "codes": {
                    "terms": ["a", "b", "c"],
                    "minimum_should_match_field": "required"
                }
            }
        }));
        match node {
            QueryNode::Bool {
                minimum_should_match: Some(MinShouldMatch::Field(name)),
                should,
                ..
            } => {
                assert_eq!(name, "required");
                assert_eq!(should.len(), 3);
            }
            other => panic!("expected bool w/ field msm, got {:?}", other),
        }
    }

    #[test]
    fn test_terms_set_minimum_should_match_script() {
        let node = q(json!({
            "terms_set": {
                "codes": {
                    "terms": ["a", "b"],
                    "minimum_should_match_script": { "source": "params.num_terms" }
                }
            }
        }));
        match node {
            QueryNode::Bool {
                minimum_should_match: Some(MinShouldMatch::Script { source, .. }),
                ..
            } => assert_eq!(source, "params.num_terms"),
            other => panic!("expected bool w/ script msm, got {:?}", other),
        }
    }

    // ── rank_feature ────────────────────────────────────────────────────────

    fn rank_feature_fn(body: serde_json::Value) -> crate::ast::RankFeatureFn {
        use crate::ast::QueryNode;
        match q(body) {
            QueryNode::FunctionScore {
                functions,
                boost_mode,
                ..
            } => {
                assert_eq!(boost_mode, crate::ast::BoostMode::Replace);
                functions[0]
                    .rank_feature
                    .as_ref()
                    .expect("rank_feature payload present")
                    .function
                    .clone()
            }
            other => panic!("expected function_score, got {:?}", other),
        }
    }

    #[test]
    fn test_rank_feature_saturation_with_pivot() {
        let f = rank_feature_fn(json!({
            "rank_feature": { "field": "pagerank", "saturation": { "pivot": 8 } }
        }));
        assert_eq!(
            f,
            crate::ast::RankFeatureFn::Saturation { pivot: Some(8.0) }
        );
    }

    #[test]
    fn test_rank_feature_defaults_to_saturation() {
        let f = rank_feature_fn(json!({ "rank_feature": { "field": "pagerank" } }));
        assert_eq!(f, crate::ast::RankFeatureFn::Saturation { pivot: None });
    }

    #[test]
    fn test_rank_feature_log_sigmoid_linear() {
        assert_eq!(
            rank_feature_fn(json!({
                "rank_feature": { "field": "pr", "log": { "scaling_factor": 4 } }
            })),
            crate::ast::RankFeatureFn::Log {
                scaling_factor: 4.0
            }
        );
        assert_eq!(
            rank_feature_fn(json!({
                "rank_feature": { "field": "pr", "sigmoid": { "pivot": 7, "exponent": 0.6 } }
            })),
            crate::ast::RankFeatureFn::Sigmoid {
                pivot: 7.0,
                exponent: 0.6
            }
        );
        assert_eq!(
            rank_feature_fn(json!({ "rank_feature": { "field": "pr", "linear": {} } })),
            crate::ast::RankFeatureFn::Linear
        );
    }

    #[test]
    fn test_rank_feature_log_missing_scaling_is_400() {
        assert!(parse_query(&json!({
            "rank_feature": { "field": "pr", "log": {} }
        }))
        .is_err());
    }
}
