//! Query AST — the central data type for xerj query processing.
//!
//! The entire ES query DSL maps to a single `QueryNode` enum.  This is the
//! polar opposite of Elasticsearch's ~60 Java `Query` classes, each with their
//! own visitor / rewriter interface.  A flat enum means:
//!
//! - Pattern matching is exhaustive and compiler-checked.
//! - Rewriters are plain recursive functions — no visitor boilerplate.
//! - Adding a new query type is a single enum variant (the compiler tells you
//!   what to update elsewhere).
//!
//! ## Variant groups
//!
//! | Group | Variants |
//! |---|---|
//! | Structural | `MatchAll`, `MatchNone`, `Bool`, `Constant`, `Boosted` |
//! | Term-level | `Term`, `Terms`, `Range`, `Prefix`, `Wildcard`, `Exists`, `Ids` |
//! | Full-text | `Match`, `MatchPhrase`, `MultiMatch`, `QueryString` |
//! | AI-native | `SemanticSearch`, `Knn`, `Hybrid` |

use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// Supporting enums
// ─────────────────────────────────────────────────────────────────────────────

/// Boolean operator used in `Match` queries when multiple tokens are produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum BoolOperator {
    /// Any token must match (default, same as ES).
    #[default]
    Or,
    /// All tokens must match.
    And,
}

/// Determines how scores are combined for `MultiMatch` queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MultiMatchType {
    /// Takes the best score across all fields (default).
    #[default]
    BestFields,
    /// Sum of scores across all matching fields.
    MostFields,
    /// Treats all fields as if they were concatenated into one.
    CrossFields,
    /// Exact phrase match.
    Phrase,
    /// Prefix on the last token; phrase on the rest.
    PhrasePrefix,
}

/// Fusion strategy for `Hybrid` queries combining multiple sub-queries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum FusionStrategy {
    /// Reciprocal Rank Fusion — robust, parameter-free combination.
    Rrf {
        /// Smoothing constant (default 60, same as ES / OpenSearch).
        k: u32,
    },
    /// Weighted linear combination of normalised scores.
    Linear,
    /// Learned combiner (weights stored in the index metadata).
    Learned,
}

impl Default for FusionStrategy {
    fn default() -> Self {
        Self::Rrf { k: 60 }
    }
}

/// Controls how many edit operations are allowed in a fuzzy query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[derive(Default)]
pub enum Fuzziness {
    /// AUTO fuzziness: 0 edits for 1-2 chars, 1 edit for 3-5 chars, 2 edits for 6+ chars.
    #[default]
    Auto,
    /// Fixed maximum number of edit operations.
    Fixed(u32),
}

/// `minimum_should_match` — either an absolute count or a percentage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MinShouldMatch {
    /// Exact number of `should` clauses that must match.
    Fixed(u32),
    /// Percentage of `should` clauses that must match (0–100).
    Percentage(u32),
}

// ─────────────────────────────────────────────────────────────────────────────
// QueryNode — the core type
// ─────────────────────────────────────────────────────────────────────────────

// ─────────────────────────────────────────────────────────────────────────────
// FunctionScore supporting types
// ─────────────────────────────────────────────────────────────────────────────

/// How multiple function scores are combined into one function score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScoreMode {
    #[default]
    Multiply,
    Sum,
    Avg,
    First,
    Max,
    Min,
}

/// How the combined function score is merged with the query score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BoostMode {
    #[default]
    Multiply,
    Replace,
    Sum,
    Avg,
    Max,
    Min,
}

/// Math modifier applied to a field value before multiplying by `factor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    #[default]
    None,
    Log,
    Log1p,
    Log2p,
    Ln,
    Ln1p,
    Ln2p,
    Square,
    Sqrt,
    Reciprocal,
}

/// Field-value-factor function: score = modifier(field_value) * factor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldValueFactor {
    pub field: String,
    #[serde(default = "default_factor")]
    pub factor: f32,
    #[serde(default)]
    pub modifier: Modifier,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing: Option<f64>,
}

fn default_factor() -> f32 {
    1.0
}

/// Random score function — deterministic hash of (doc_id, seed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RandomScore {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

/// A single function inside a `function_score` query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ScoreFunction {
    /// Optional filter — function only applies to matching documents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<QueryNode>,
    /// Constant weight multiplier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<f32>,
    /// Field-value-based scoring function.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_value_factor: Option<FieldValueFactor>,
    /// Randomised score (deterministic per doc+seed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub random_score: Option<RandomScore>,
    /// Literal numeric script_score source (e.g. `{"source": "3"}`).
    /// Used as a fast-path for the constant-score case so the engine
    /// doesn't have to spin up the Painless interpreter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_score: Option<f32>,
    /// Full Painless `script_score` source. When set, the engine invokes
    /// the `xerj_engine::painless` interpreter per matched doc with
    /// `_score` bound to the query's BM25 score and `params` bound to
    /// `script_score_params`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_source: Option<String>,
    /// Free-form params object passed to the script.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_params: Option<serde_json::Value>,
    /// ES 8.9+: a named function entry can carry `_name` at the
    /// function level. Its score in `matched_queries` is the function's
    /// own contribution (weight/script_score/factor).
    #[serde(rename = "_name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `distance_feature` scoring — pivot / (pivot + distance(origin, field)).
    /// The origin can be an ISO-8601 date string or a [lon, lat] geo point;
    /// the pivot is a duration (for date fields) or distance (for geo_point).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_feature: Option<DistanceFeature>,
}

/// Payload for a `distance_feature` function.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DistanceFeature {
    pub field: String,
    pub pivot: String,
    pub origin: serde_json::Value,
}

/// A single node in the query tree.
///
/// All query types — leaf and compound — are variants of this one enum.
/// Rewriters and planners traverse it with ordinary recursive functions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum QueryNode {
    // ── Structural ────────────────────────────────────────────────────────────
    /// Matches every document in the index.
    MatchAll,

    /// Matches no documents; useful as a sentinel / after constant folding.
    MatchNone,

    /// Compound boolean query.
    Bool {
        /// All clauses must match; scores are summed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        must: Vec<QueryNode>,
        /// At least `minimum_should_match` clauses must match; scores summed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        should: Vec<QueryNode>,
        /// No clause may match; does not affect scoring.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        must_not: Vec<QueryNode>,
        /// All must match; scores are *not* summed (zero contribution).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        filter: Vec<QueryNode>,
        /// Minimum number (or %) of `should` clauses required.
        #[serde(skip_serializing_if = "Option::is_none")]
        minimum_should_match: Option<MinShouldMatch>,
    },

    /// Wraps a query, replacing its score with a fixed constant.
    Constant { score: f32, query: Box<QueryNode> },

    /// Multiplies the wrapped query's score by `boost`.
    Boosted { boost: f32, query: Box<QueryNode> },

    // ── Term-level ────────────────────────────────────────────────────────────
    /// Exact-value match on a single field.
    Term {
        field: String,
        value: serde_json::Value,
        /// Optional boost for this leaf (>1.0 increases score).
        #[serde(skip_serializing_if = "Option::is_none")]
        boost: Option<f32>,
    },

    /// Exact-value match against any of several values (OR semantics).
    Terms {
        field: String,
        values: Vec<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        boost: Option<f32>,
    },

    /// Numeric / date / keyword range query.
    Range {
        field: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        gte: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        gt: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        lte: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        lt: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        boost: Option<f32>,
    },

    /// Prefix match on a keyword field.
    Prefix {
        field: String,
        value: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        boost: Option<f32>,
    },

    /// Wildcard pattern match (`?` = any char, `*` = zero-or-more chars).
    Wildcard {
        field: String,
        value: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        boost: Option<f32>,
    },

    /// Matches documents where the field has any non-null value.
    Exists { field: String },

    /// Matches documents whose `_id` is in the given list.
    Ids { values: Vec<String> },

    // ── Full-text ─────────────────────────────────────────────────────────────
    /// Analysed text query against a single field.
    Match {
        field: String,
        query: String,
        #[serde(default)]
        operator: BoolOperator,
        #[serde(skip_serializing_if = "Option::is_none")]
        analyzer: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        boost: Option<f32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        minimum_should_match: Option<MinShouldMatch>,
    },

    /// Ordered phrase match with optional slop (token transpositions allowed).
    MatchPhrase {
        field: String,
        query: String,
        #[serde(default)]
        slop: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        analyzer: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        boost: Option<f32>,
    },

    /// Full-text query across multiple fields simultaneously.
    MultiMatch {
        fields: Vec<String>,
        query: String,
        #[serde(default)]
        match_type: MultiMatchType,
        #[serde(skip_serializing_if = "Option::is_none")]
        operator: Option<BoolOperator>,
        #[serde(skip_serializing_if = "Option::is_none")]
        analyzer: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        boost: Option<f32>,
    },

    /// Lucene query-string syntax (e.g. `"title:(foo AND bar) OR body:baz"`).
    QueryString {
        query: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        default_field: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        default_operator: Option<BoolOperator>,
        #[serde(skip_serializing_if = "Option::is_none")]
        boost: Option<f32>,
    },

    // ── AI-native extensions ──────────────────────────────────────────────────
    /// Dense vector k-NN search (exact or approximate).
    Knn {
        field: String,
        vector: Vec<f32>,
        k: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        filter: Option<Box<QueryNode>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        boost: Option<f32>,
    },

    /// Natural-language semantic search — text is embedded at query time.
    SemanticSearch {
        field: String,
        /// The natural-language query text to embed.
        text: String,
        k: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        filter: Option<Box<QueryNode>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        boost: Option<f32>,
    },

    /// Combine multiple sub-queries with a score-fusion strategy.
    Hybrid {
        /// Each sub-query paired with its relative weight.
        queries: Vec<WeightedQuery>,
        #[serde(default)]
        fusion: FusionStrategy,
    },

    /// Function score — modifies document scores using one or more scoring functions.
    FunctionScore {
        /// The base query whose hits are scored.
        query: Box<QueryNode>,
        /// Scoring functions applied to each hit.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        functions: Vec<ScoreFunction>,
        /// How multiple function scores are combined.
        #[serde(default)]
        score_mode: ScoreMode,
        /// How the combined function score is merged with the query score.
        #[serde(default)]
        boost_mode: BoostMode,
        /// Upper bound on the combined score (None = no cap).
        #[serde(skip_serializing_if = "Option::is_none")]
        max_boost: Option<f32>,
    },

    /// Boosting query: matches positive, penalises docs that also match negative.
    Boosting {
        /// Docs must match this query.
        positive: Box<QueryNode>,
        /// Docs that also match this query have their score multiplied by `negative_boost`.
        negative: Box<QueryNode>,
        /// Score multiplier for docs that match the negative query (0.0–1.0).
        negative_boost: f32,
    },

    /// Disjunction-Max query: score = max(sub-scores) + tie_breaker * Σ(other scores).
    DisMax {
        queries: Vec<QueryNode>,
        /// Tie-breaker added to the sum of non-winning sub-scores (default 0.0).
        #[serde(default)]
        tie_breaker: f32,
    },

    // ── Extended query types ───────────────────────────────────────────────────
    /// Fuzzy match — allows typo tolerance via Levenshtein edit distance.
    Fuzzy {
        field: String,
        value: String,
        #[serde(default)]
        fuzziness: Fuzziness,
    },

    /// Regular expression match against a field value.
    Regexp { field: String, pattern: String },

    /// Position-aware text match (ES `intervals` query). The `rule` is the
    /// raw rule JSON (one of match/all_of/any_of/prefix/wildcard/fuzzy,
    /// optionally with `filter:` clauses) — the executor tokenises the
    /// field at query time and evaluates the rule against the token
    /// positions, which is why it stays as an untyped JSON subtree here.
    Intervals {
        field: String,
        rule: serde_json::Value,
    },

    /// Match phrase with a prefix match on the last token.
    MatchPhrasePrefix {
        field: String,
        query: String,
        #[serde(default = "default_max_expansions")]
        max_expansions: u32,
    },

    /// Simple query string — splits on `+`/`|`/`-` operators, converted to a Bool query.
    SimpleQueryString {
        query: String,
        #[serde(default)]
        fields: Vec<String>,
    },

    /// Geo distance query — matches documents within `distance_km` of the given lat/lon.
    GeoDistance {
        /// The geo_point field name.
        field: String,
        /// Latitude of the query origin.
        lat: f64,
        /// Longitude of the query origin.
        lon: f64,
        /// Maximum distance in kilometres.
        distance_km: f64,
    },

    /// Geo bounding box query — matches documents whose geo_point falls within the box.
    GeoBoundingBox {
        /// The geo_point field name.
        field: String,
        /// Top-left corner (lat, lon).
        top_left: (f64, f64),
        /// Bottom-right corner (lat, lon).
        bottom_right: (f64, f64),
    },

    // ── Nested / join / specialised queries ───────────────────────────────────
    /// Nested query — runs an inner query against each element of a nested array.
    ///
    /// If any element matches the inner query, the document matches.
    Nested {
        /// Dot-path to the nested array field (e.g. `"comments"`).
        path: String,
        /// The query to run against each nested element.
        query: Box<QueryNode>,
        /// How scores from matching elements are combined (ignored in filtering).
        #[serde(skip_serializing_if = "Option::is_none")]
        score_mode: Option<String>,
    },

    /// More-like-this query — finds documents similar to example text.
    MoreLikeThis {
        /// Fields to compare (default: all text fields).
        fields: Vec<String>,
        /// One or more example text strings to find similar documents for.
        like: Vec<String>,
        /// Minimum number of times a term must appear in the input text.
        #[serde(default = "default_min_term_freq")]
        min_term_freq: u32,
        /// Maximum number of query terms to use.
        #[serde(default = "default_max_query_terms")]
        max_query_terms: u32,
    },

    /// Percolate query — reverse search. Each document stored in the index
    /// holds a serialized query in its `field` (a `percolator`-typed field);
    /// this query matches those stored documents whose stored query matches
    /// one of the supplied inline `documents`.
    Percolate {
        /// Name of the percolator field holding the stored query object.
        field: String,
        /// One or more inline documents to test the stored queries against.
        documents: Vec<serde_json::Value>,
    },

    /// Pinned query — ensures specific document IDs appear first in results.
    Pinned {
        /// IDs to pin to the top of results.
        ids: Vec<String>,
        /// The fallback query whose results follow the pinned IDs.
        organic: Box<QueryNode>,
    },

    /// Named query wrapper — attaches a logical name to any query.
    ///
    /// When a document matches, the name appears in `matched_queries` in the hit response.
    Named {
        /// The human-readable name for this query clause.
        name: String,
        /// The wrapped query.
        query: Box<QueryNode>,
    },

    // ── Span queries ──────────────────────────────────────────────────────────
    /// Span term — exact value match within span context.
    SpanTerm { field: String, value: String },

    /// Span near — span terms that appear within `slop` positions of each other.
    SpanNear {
        clauses: Vec<QueryNode>,
        slop: u32,
        in_order: bool,
    },

    /// Span or — matches if any of the span clauses match.
    SpanOr { clauses: Vec<QueryNode> },

    /// Span not — matches the include span but not the exclude span.
    SpanNot {
        include: Box<QueryNode>,
        exclude: Box<QueryNode>,
    },

    /// Span first — matches if the span term appears in the first `end` positions.
    SpanFirst {
        match_query: Box<QueryNode>,
        end: u32,
    },

    // ── Join queries ──────────────────────────────────────────────────────────
    /// Has-child — matches parent documents that have child documents matching the query.
    HasChild {
        child_type: String,
        query: Box<QueryNode>,
        score_mode: Option<String>,
    },

    /// Has-parent — matches child documents that have parent documents matching the query.
    HasParent {
        parent_type: String,
        query: Box<QueryNode>,
        score: bool,
    },

    // ── Geo shape queries ─────────────────────────────────────────────────────
    /// Geo polygon — matches documents whose geo_point falls within the polygon.
    GeoPolygon {
        field: String,
        points: Vec<(f64, f64)>,
    },

    /// Geo shape — matches documents whose geo field matches the given shape.
    GeoShape { field: String, shape: GeoShapeType },
}

/// Shape type for geo_shape queries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeoShapeType {
    /// A single point (lat, lon).
    Point { lat: f64, lon: f64 },
    /// An envelope (bounding box) given by top-left and bottom-right corners (lat, lon).
    Envelope {
        top_left: (f64, f64),
        bottom_right: (f64, f64),
    },
    /// A polygon defined by a list of (lat, lon) points.
    Polygon { points: Vec<(f64, f64)> },
    /// A circle defined by a center point and a radius in kilometres.
    Circle { center: (f64, f64), radius_km: f64 },
}

// ─────────────────────────────────────────────────────────────────────────────
// Rescore types
// ─────────────────────────────────────────────────────────────────────────────

/// The inner parameters for a rescore query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RescoreQueryInner {
    /// The query used to re-score the top-N hits.
    pub rescore_query: QueryNode,
    /// Weight applied to the original query score (default 1.0).
    #[serde(default = "default_weight_1")]
    pub query_weight: f32,
    /// Weight applied to the rescore query score (default 1.0).
    #[serde(default = "default_weight_1")]
    pub rescore_query_weight: f32,
}

fn default_weight_1() -> f32 {
    1.0
}

/// A single rescore window: re-scores the top `window_size` hits using a secondary query.
///
/// Final score = `original_score * query_weight + rescore_score * rescore_query_weight`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RescoreQuery {
    /// Number of top hits to re-score (default 100).
    #[serde(default = "default_rescore_window")]
    pub window_size: usize,
    /// The secondary query and its weighting parameters.
    /// At least one of `query` or `script` must be set; when both are
    /// present the engine applies them in declaration order (matching ES).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<RescoreQueryInner>,
    /// Optional Painless script-based rescore. When set, the rescore
    /// score for each hit is the `script.source` evaluated with
    /// `script.params` and `_score` bound to the hit's current score.
    /// Independent of `query` (you can have one or both).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<ScriptRescore>,
}

/// Painless script rescorer. Mirrors the
/// `rescore.script.script: { source, params }` body shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ScriptRescore {
    /// Painless source.
    pub source: String,
    /// Free-form params object passed to the script.
    #[serde(default)]
    pub params: serde_json::Value,
    /// Weight applied to the original `_score` after script evaluation
    /// (default 1.0). ES `script_score.script_score` rescore semantics.
    #[serde(default = "default_weight_1")]
    pub query_weight: f32,
    /// Weight applied to the script's returned score (default 1.0).
    #[serde(default = "default_weight_1")]
    pub rescore_query_weight: f32,
    /// Combine mode: "total" (default), "multiply", "min", "max", "avg",
    /// "replace". ES rescore default is "total" (sum of weighted scores).
    #[serde(default)]
    pub score_mode: Option<String>,
}

fn default_rescore_window() -> usize {
    100
}

fn default_min_term_freq() -> u32 {
    2
}

fn default_max_query_terms() -> u32 {
    25
}

fn default_max_expansions() -> u32 {
    50
}

/// A sub-query with an associated weight for `Hybrid` fusion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeightedQuery {
    pub query: QueryNode,
    pub weight: f32,
}

impl QueryNode {
    /// Returns `true` if this node is `MatchAll`.
    pub fn is_match_all(&self) -> bool {
        matches!(self, QueryNode::MatchAll)
    }

    /// Returns `true` if this node is `MatchNone`.
    pub fn is_match_none(&self) -> bool {
        matches!(self, QueryNode::MatchNone)
    }

    /// Returns `true` if this node is a `Bool` with only filter clauses
    /// (no scoring contribution from sub-queries).
    pub fn is_filter_only(&self) -> bool {
        match self {
            QueryNode::Bool {
                must,
                should,
                filter,
                must_not,
                ..
            } => must.is_empty() && should.is_empty() && !filter.is_empty() || !must_not.is_empty(),
            QueryNode::Exists { .. }
            | QueryNode::Term { .. }
            | QueryNode::Terms { .. }
            | QueryNode::Range { .. }
            | QueryNode::Ids { .. } => false,
            _ => false,
        }
    }

    /// Rough structural depth — used by the planner for cost estimation.
    pub fn depth(&self) -> usize {
        match self {
            QueryNode::Bool {
                must,
                should,
                must_not,
                filter,
                ..
            } => {
                let max_child = must
                    .iter()
                    .chain(should.iter())
                    .chain(must_not.iter())
                    .chain(filter.iter())
                    .map(|q| q.depth())
                    .max()
                    .unwrap_or(0);
                1 + max_child
            }
            QueryNode::Constant { query, .. } | QueryNode::Boosted { query, .. } => {
                1 + query.depth()
            }
            QueryNode::Knn { filter, .. } | QueryNode::SemanticSearch { filter, .. } => {
                1 + filter.as_ref().map(|f| f.depth()).unwrap_or(0)
            }
            QueryNode::Hybrid { queries, .. } => {
                let max_child = queries.iter().map(|wq| wq.query.depth()).max().unwrap_or(0);
                1 + max_child
            }
            QueryNode::FunctionScore { query, .. } => 1 + query.depth(),
            QueryNode::Boosting {
                positive, negative, ..
            } => 1 + positive.depth().max(negative.depth()),
            QueryNode::DisMax { queries, .. } => {
                let max_child = queries.iter().map(|q| q.depth()).max().unwrap_or(0);
                1 + max_child
            }
            QueryNode::Fuzzy { .. }
            | QueryNode::Regexp { .. }
            | QueryNode::MatchPhrasePrefix { .. }
            | QueryNode::SimpleQueryString { .. }
            | QueryNode::GeoDistance { .. }
            | QueryNode::GeoBoundingBox { .. }
            | QueryNode::MoreLikeThis { .. } => 1,
            QueryNode::Nested { query, .. } => 1 + query.depth(),
            QueryNode::Pinned { organic, .. } => 1 + organic.depth(),
            QueryNode::Named { query, .. } => 1 + query.depth(),
            _ => 1,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SearchRequest — the top-level request envelope
// ─────────────────────────────────────────────────────────────────────────────

/// Controls how total hits are tracked.
///
/// - `True`     — always count all matching docs (default, ES behaviour).
/// - `False`    — stop counting once `size` hits are found.
/// - `Limit(N)` — stop counting after N docs; return `Gte` relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[derive(Default)]
pub enum TrackTotalHits {
    /// Count all matching documents (default).
    #[serde(rename = "true")]
    #[default]
    True,
    /// Do not count beyond `size` hits.
    #[serde(rename = "false")]
    False,
    /// Count up to `N` documents; relation becomes `gte` when capped.
    Limit(u64),
}

/// The fully-parsed search request.
///
/// This is what the REST layer hands to the query engine after deserialising
/// the request body.  All ES-level options are included.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    /// The root query node.
    #[serde(default = "default_query")]
    pub query: QueryNode,

    /// First result offset (default 0).
    #[serde(default)]
    pub from: usize,

    /// Maximum number of hits to return (default 10).
    #[serde(default = "default_size")]
    pub size: usize,

    /// Sort specification (empty = sort by `_score` descending).
    #[serde(default)]
    pub sort: Vec<crate::sort::SortField>,

    /// Cursor value for keyset pagination (ES `search_after`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_after: Option<Vec<serde_json::Value>>,

    /// Aggregation definitions (passed through as opaque JSON for now).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggs: Option<serde_json::Value>,

    /// Whether to include per-hit scoring explanation.
    #[serde(default)]
    pub explain: bool,

    /// `_source` inclusion / exclusion filter.
    #[serde(default, rename = "_source")]
    pub source: SourceFilter,

    /// Hard timeout in milliseconds; query is cancelled if exceeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,

    /// Highlight configuration — which fields to highlight and how.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highlight: Option<HighlightRequest>,

    /// Controls total hit counting behaviour.
    #[serde(default)]
    pub track_total_hits: TrackTotalHits,

    /// Script fields — computed fields returned alongside hits (currently no-op).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_fields: Option<serde_json::Value>,

    /// Stored/doc-value fields to return alongside hits (different from `_source`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,

    /// When true, include execution timing breakdown in the response.
    #[serde(default)]
    pub profile: bool,

    /// Field collapsing — deduplicate hits by the given field, keeping the top hit per value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapse: Option<CollapseField>,

    /// Rescore specification — re-scores top hits using a secondary query.
    ///
    /// Each entry in the list is applied in order: the first rescorer re-scores
    /// the top `window_size` hits from the primary query; the second rescorer
    /// re-scores the top `window_size` hits from the first rescorer; and so on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rescore: Vec<RescoreQuery>,

    /// Minimum score threshold — hits with `_score < min_score` are
    /// dropped before pagination, aggregations, and total counting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_score: Option<f64>,
}

fn default_query() -> QueryNode {
    QueryNode::MatchAll
}

fn default_size() -> usize {
    10
}

impl Default for SearchRequest {
    fn default() -> Self {
        Self {
            query: QueryNode::MatchAll,
            from: 0,
            size: 10,
            sort: Vec::new(),
            search_after: None,
            aggs: None,
            explain: false,
            source: SourceFilter::default(),
            timeout_ms: None,
            highlight: None,
            track_total_hits: TrackTotalHits::default(),
            script_fields: None,
            fields: Vec::new(),
            profile: false,
            collapse: None,
            rescore: Vec::new(),
            min_score: None,
        }
    }
}

/// Controls which source fields are returned with each hit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SourceFilter {
    /// `true` → return all source fields; `false` → return none.
    Enabled(bool),
    /// Return only the listed fields.
    Includes(Vec<String>),
    /// Fine-grained include / exclude lists.
    Fields {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        includes: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        excludes: Vec<String>,
    },
}

impl Default for SourceFilter {
    fn default() -> Self {
        SourceFilter::Enabled(true)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// HighlightRequest — per-field highlighting configuration
// ─────────────────────────────────────────────────────────────────────────────

/// Highlight configuration for a search request.
///
/// Mirrors ES highlight syntax:
/// ```json
/// "highlight": {
///   "pre_tag": "<em>",
///   "post_tag": "</em>",
///   "fragment_size": 150,
///   "fields": { "content": {}, "title": { "fragment_size": 50 } }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct HighlightRequest {
    /// Fields to highlight. Keys are field names; values are per-field options.
    #[serde(default)]
    pub fields: std::collections::HashMap<String, HighlightFieldOptions>,

    /// Opening tag around each highlighted term (default `"<em>"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_tag: Option<String>,

    /// Closing tag around each highlighted term (default `"</em>"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_tag: Option<String>,

    /// Maximum length (in chars) of each returned fragment (default 150).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fragment_size: Option<usize>,

    /// Number of fragments to return per field (default 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_of_fragments: Option<usize>,
}

/// Per-field highlight options (can override the top-level defaults).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct HighlightFieldOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fragment_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_of_fragments: Option<usize>,
}

// ─────────────────────────────────────────────────────────────────────────────
// CollapseField — field collapsing (deduplication by field value)
// ─────────────────────────────────────────────────────────────────────────────

/// Field collapsing: keep only the top-scoring hit per unique value of `field`.
///
/// Mirrors ES `collapse` parameter:
/// ```json
/// "collapse": { "field": "category" }
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollapseField {
    /// The field to collapse on (keyword / numeric).
    pub field: String,
    /// Optional inner_hits — return additional hits per group (opaque JSON for now).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_hits: Option<serde_json::Value>,
}
