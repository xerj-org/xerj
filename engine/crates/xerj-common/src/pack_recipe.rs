//! Declarative TOML tool recipes and deterministic Rust recipe executor
//! (`Xerj Packs` 3/9, #2762, #2769).
//!
//! Pack tools (`writing`, `bio`, `cyber`, and user-defined packs) are defined
//! by versioned TOML recipe files (`format_version = "xerj-recipe/v1"`) and
//! executed by [`RecipeRunner`] in pure Rust:
//! - Automatic MCP tool projection ([`PackRecipe::to_mcp_tool_spec`]) turns any
//!   validated TOML recipe into a `tools/list` descriptor (`name`,
//!   `description`, `inputSchema`) with no Rust changes. `xerj-mcp`'s
//!   `RecipeRegistry` (`xerj mcp --recipe-dir <DIR>`) registers the recipes in a
//!   directory in `tools/list` and sends each `tools/call` to
//!   [`RecipeRunner::execute_from_source`], returning the JCS-sealed report.
//! - The recipe's `[corpora]` block selects the data. [`RecipeRunner::execute_from_source`]
//!   asks a [`RecipeCorpusSource`] for every `required` corpus (absent or
//!   unreadable refuses the call) and every `optional` one (absent is skipped),
//!   and nothing from an undeclared corpus reaches the report.
//!   [`RecipeRunner::execute`] still takes hits a caller already holds, and
//!   applies the same `[corpora]` rules to them.
//! - Invalid recipes fail at build/test time with a precise [`RecipeError`].
//! - Execution performs corpus selection, redistribution licence checks,
//!   input-bound filtering, study-type grading with a `GradeRule` receipt,
//!   deterministic ranking, theme/chapter grouping, an evidence floor that
//!   refuses instead of padding, output JSON Schema validation, and
//!   deterministic post-checks (citation completeness, banned-phrase /
//!   anti-slop refusal, a token ceiling measured on the RFC 8785 bytes of the
//!   output, and mandatory notices), sealing the result with an RFC 8785 (JCS)
//!   SHA-256 receipt.
//! - `builtin_recipes()` are `write.outline`, `write.section`, `bio.outline`
//!   and `bio.evidence`. `cyber.target_surface` is a sample until the Cyber pack
//!   (#2767) lands ([`sample_recipes`]).

use crate::cyber_scope::{InputOrigin, ScopeError, ScopeGuard, WorkOrder};
use crate::hub::LicenceClass;
use crate::hub_evidence::{sha256, PinnedCitation};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const RECIPE_FORMAT_VERSION: &str = "xerj-recipe/v1";
pub const RECIPE_REPORT_VERSION: &str = "xerj-recipe-execution/v1";
const MAX_REPORT_BYTES: usize = 4 * 1024 * 1024;
/// A `max_token_budget` check target meaning the whole output object.
const WHOLE_OUTPUT: &str = "$";

#[derive(Debug, thiserror::Error)]
pub enum RecipeError {
    #[error("invalid recipe TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid recipe `{recipe}`: {reason}")]
    InvalidRecipe { recipe: String, reason: String },
    #[error("invalid MCP arguments for recipe `{recipe}`: {reason}")]
    InvalidArguments { recipe: String, reason: String },
    #[error("recipe `{recipe}` refused corpus `{corpus}`: licence class is Excluded")]
    ExcludedCorpusLicence { recipe: String, corpus: String },
    #[error("recipe `{recipe}` refused: missing required corpus `{corpus}`")]
    MissingRequiredCorpus { recipe: String, corpus: String },
    /// What a [`RecipeCorpusSource`] answers for a corpus it does not hold.
    /// `execute_from_source` turns it into [`RecipeError::MissingRequiredCorpus`]
    /// for a required corpus and skips an optional one.
    #[error("corpus `{corpus}` is not installed")]
    CorpusNotInstalled { corpus: String },
    /// A corpus the source holds but cannot hand over: unreadable, malformed,
    /// too large, or a pack that does not verify. Never skipped, optional or not:
    /// a report built without evidence it was meant to carry would not say so.
    #[error("corpus `{corpus}` could not be read: {reason}")]
    CorpusUnreadable { corpus: String, reason: String },
    #[error("recipe `{recipe}` abstained: matched {matched} hits, need at least {min_hits}")]
    InsufficientEvidence {
        recipe: String,
        matched: usize,
        min_hits: usize,
    },
    #[error("recipe `{recipe}` cannot grade record `{record_id}`: {reason}")]
    InvalidEvidence {
        recipe: String,
        record_id: String,
        reason: String,
    },
    #[error("recipe `{recipe}` output failed schema validation: {reason}")]
    OutputSchemaViolation { recipe: String, reason: String },
    #[error("recipe `{recipe}` check `{check_id}` failed: {reason}")]
    CheckFailed {
        recipe: String,
        check_id: String,
        reason: String,
    },
    #[error("recipe `{recipe}` requires a declared ScopeGuard")]
    ScopeRequired { recipe: String },
    #[error(transparent)]
    Scope(#[from] ScopeError),
    #[error("recipe execution report digest mismatch")]
    DigestMismatch,
    #[error("recipe JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, RecipeError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackRecipe {
    pub recipe: RecipeHeader,
    #[serde(default)]
    pub inputs: Vec<RecipeInputField>,
    pub corpora: CorpusSelection,
    #[serde(default)]
    pub filters: Vec<FilterStep>,
    pub ranking: RankingStep,
    /// Group the selected items into chapters by `ranking.cluster_by`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouping: Option<GroupingStep>,
    /// Grade each selected item by study type and separate clinical evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grading: Option<GradingStep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partitions: Vec<PartitionStep>,
    pub output_schema: OutputSchemaSpec,
    #[serde(default)]
    pub checks: Vec<CheckStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeHeader {
    pub format_version: String,
    pub pack: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_tool_name: Option<String>,
    pub description: String,
    #[serde(default)]
    pub requires_scope: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub static_notice: Option<String>,
    /// A sample: shows the format for a pack whose tools are not served yet.
    /// [`builtin_recipes`] excludes it and its MCP description says so.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sample: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    String,
    Integer,
    Number,
    Boolean,
    StringArray,
    Object,
    Array,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeInputField {
    pub name: String,
    pub kind: FieldKind,
    #[serde(default)]
    pub required: bool,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_len: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_len: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusSelection {
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub optional: Vec<String>,
    #[serde(default = "default_true")]
    pub require_redistribution_licence: bool,
    /// Minimum candidate hits that pass the filters.
    #[serde(default = "default_one")]
    pub min_hits: usize,
    /// Minimum evidence items the output must carry after ranking, the
    /// per-cluster cap and `top_k`. Fewer is a refusal
    /// ([`RecipeError::InsufficientEvidence`]), never padding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_evidence_hits: Option<usize>,
    /// The same floor chosen by an enum input (`length = "standard"` needs 3).
    /// The larger of the two floors applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_evidence_hits_by_input: Option<InputTable>,
}

/// A number chosen by the value of a required enum input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputTable {
    pub input: String,
    pub values: BTreeMap<String, usize>,
}

fn default_true() -> bool {
    true
}
fn default_one() -> usize {
    1
}

/// Output chapters: the selected items grouped by `ranking.cluster_by` (the hit
/// field, or the derived `theme_key`: the first four alphanumeric tokens of the
/// excerpt, then the title, short tokens and digits kept).
///
/// Items sharing a key stay in one chapter. With a chapter count (`chapters`,
/// or `chapters_from_input`), boundaries fall between keys as close to an even
/// split as the keys allow, and only fewer keys than chapters splits one (the
/// largest first), so no chapter is empty or padded. With no count, every
/// distinct key is one chapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupingStep {
    /// Array output property that receives the chapters.
    pub output_field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chapters: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chapters_from_input: Option<InputTable>,
}

/// An output array of the items whose member `field` satisfies `op` `value`.
/// The items stay in `items` too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartitionStep {
    pub output_field: String,
    pub field: String,
    pub op: FilterOp,
    pub value: String,
    /// The first (best-ranked) match is part of the mandatory core: a
    /// `fit_by_dropping` token check never drops it.
    #[serde(default)]
    pub protect_first: bool,
}

/// Study design, as a hit's `study_type` field spells it (snake_case). Each is
/// its own design with its own rule; none is an alias of another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StudyType {
    MetaAnalysis,
    SystematicReview,
    RandomizedControlledTrial,
    CohortStudy,
    CaseControl,
    CaseReport,
    Preclinical,
    InVitro,
    ExpertOpinion,
}

impl StudyType {
    pub const ALL: [StudyType; 9] = [
        Self::MetaAnalysis,
        Self::SystematicReview,
        Self::RandomizedControlledTrial,
        Self::CohortStudy,
        Self::CaseControl,
        Self::CaseReport,
        Self::Preclinical,
        Self::InVitro,
        Self::ExpertOpinion,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            Self::MetaAnalysis => "meta_analysis",
            Self::SystematicReview => "systematic_review",
            Self::RandomizedControlledTrial => "randomized_controlled_trial",
            Self::CohortStudy => "cohort_study",
            Self::CaseControl => "case_control",
            Self::CaseReport => "case_report",
            Self::Preclinical => "preclinical",
            Self::InVitro => "in_vitro",
            Self::ExpertOpinion => "expert_opinion",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|t| t.slug() == raw)
    }
}

/// Evidence tier, strongest first. `Level5` is the floor: preclinical, in-vitro,
/// expert opinion, and any study that is not human clinical evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EvidenceTier {
    #[serde(rename = "level_1a")]
    Level1A,
    #[serde(rename = "level_1b")]
    Level1B,
    #[serde(rename = "level_2a")]
    Level2A,
    #[serde(rename = "level_2b")]
    Level2B,
    #[serde(rename = "level_3")]
    Level3,
    #[serde(rename = "level_4")]
    Level4,
    #[serde(rename = "level_5")]
    Level5,
}

impl EvidenceTier {
    pub fn slug(self) -> &'static str {
        match self {
            Self::Level1A => "level_1a",
            Self::Level1B => "level_1b",
            Self::Level2A => "level_2a",
            Self::Level2B => "level_2b",
            Self::Level3 => "level_3",
            Self::Level4 => "level_4",
            Self::Level5 => "level_5",
        }
    }
}

/// One row of the study-type grading table. Rows are tried in order; the first
/// whose study type matches and whose flags hold grades the item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GradeRuleSpec {
    pub id: String,
    pub study_type: StudyType,
    /// At least one of these hit flags is true (`"true"`) when listed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub any_flags: Vec<String>,
    /// Every one of these hit flags is true when listed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub all_flags: Vec<String>,
    pub tier: EvidenceTier,
    pub rationale: String,
}

/// Study-type grading with a receipt, and the clinical / non-clinical split.
///
/// Each selected item gets `study_type`, `evidence_tier`, `human_clinical` and a
/// `grade_rule` (`rule_id`, `study_type`, `tier`, `matched_signals`, `metadata`,
/// `rationale`). A study whose `human_field` is not `"true"` is capped at
/// `Level5` under `non_human_rule_id`, whatever its design; the receipt names
/// the rule it would otherwise have met. Items that are human clinical evidence
/// (`human_clinical` and a tier above `Level5`) fill `clinical_field`; every
/// other item fills `non_clinical_field`. When there is no clinical item,
/// `abstention` is written to `abstention_field`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GradingStep {
    pub study_type_field: String,
    pub human_field: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flag_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction_field: Option<String>,
    pub non_human_rule_id: String,
    pub non_human_rationale: String,
    pub clinical_field: String,
    pub non_clinical_field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contradictory_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highest_clinical_tier_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abstention_field: Option<String>,
    pub abstention: String,
    pub rules: Vec<GradeRuleSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    Equals,
    NotEquals,
    Contains,
    In,
    Prefix,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterStep {
    pub field: String,
    pub op: FilterOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_input: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RankingStrategy {
    ScoreDesc,
    RecencyDesc,
    StudyGradeAsc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RankingStep {
    pub strategy: RankingStrategy,
    pub top_k: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_per_cluster: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSchemaSpec {
    pub required_fields: Vec<String>,
    pub properties: BTreeMap<String, OutputPropertySpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputPropertySpec {
    pub kind: FieldKind,
    #[serde(default)]
    pub non_empty: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub item_required_fields: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    RequireCitations,
    ForbidPhrases,
    MaxTokenBudget,
    RequireNotice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckStep {
    pub id: String,
    pub kind: CheckKind,
    /// The output property to check. `"$"` is the whole output object, which is
    /// what a consumer receives (a `max_token_budget` check only).
    pub target_field: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phrases: Vec<String>,
    /// `max_token_budget`: the ceiling, in tokens of the target's RFC 8785 JSON
    /// bytes divided by four, rounded up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<usize>,
    /// `max_token_budget`: take the ceiling from this integer input, which must
    /// also be a declared integer output property (the output echoes it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_from_input: Option<String>,
    /// `max_token_budget`: instead of failing, drop the lowest-ranked item that
    /// is not part of the mandatory core (the first item, or the first match of
    /// each `protect_first` partition) until the output fits. Still over
    /// budget with only the core left is a failure.
    #[serde(default)]
    pub fit_by_dropping: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_substring: Option<String>,
}

impl PackRecipe {
    pub fn from_toml_str(raw: &str) -> Result<Self> {
        let recipe: Self = toml::from_str(raw)?;
        recipe.validate()?;
        Ok(recipe)
    }

    pub fn mcp_name(&self) -> String {
        self.recipe.mcp_tool_name.clone().unwrap_or_else(|| {
            format!(
                "xerj_{}",
                self.recipe
                    .name
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        '_'
                    })
                    .collect::<String>()
            )
        })
    }

    pub fn validate(&self) -> Result<()> {
        let rname = if self.recipe.name.is_empty() {
            "<unnamed>".to_string()
        } else {
            self.recipe.name.clone()
        };
        let bad = |reason: String| -> Result<()> {
            Err(RecipeError::InvalidRecipe {
                recipe: rname.clone(),
                reason,
            })
        };

        if self.recipe.format_version != RECIPE_FORMAT_VERSION {
            return bad(format!(
                "unsupported format_version {:?} (expected {:?})",
                self.recipe.format_version, RECIPE_FORMAT_VERSION
            ));
        }
        if !valid_slug(&self.recipe.pack, 64) {
            return bad("recipe.pack must be 1..=64 ASCII [A-Za-z0-9._:-]".to_string());
        }
        if !valid_slug(&self.recipe.name, 64) || !self.recipe.name.contains('.') {
            return bad("recipe.name must be <pack>.<tool> (e.g. write.outline)".to_string());
        }
        let mcp = self.mcp_name();
        if mcp.is_empty()
            || mcp.len() > 64
            || !mcp
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return bad("mcp_tool_name must be 1..=64 lowercase ASCII [a-z0-9_]".to_string());
        }
        if self.recipe.description.trim().is_empty() {
            return bad("recipe.description cannot be empty".to_string());
        }

        let mut input_names = HashSet::new();
        for inp in &self.inputs {
            if !valid_ident(&inp.name, 64) {
                return bad(format!("invalid input field name {:?}", inp.name));
            }
            if !input_names.insert(inp.name.clone()) {
                return bad(format!("duplicate input field {:?}", inp.name));
            }
            if inp.description.trim().is_empty() {
                return bad(format!("input {:?} has an empty description", inp.name));
            }
            if matches!(inp.kind, FieldKind::Object | FieldKind::Array) {
                return bad(format!(
                    "input {:?} must use a scalar or string_array kind",
                    inp.name
                ));
            }
            if let (Some(min), Some(max)) = (inp.min_len, inp.max_len) {
                if min > max {
                    return bad(format!("input {:?} has min_len > max_len", inp.name));
                }
            }
        }

        if self.corpora.required.is_empty() && self.corpora.optional.is_empty() {
            return bad(
                "corpora must declare at least one required or optional corpus".to_string(),
            );
        }
        for c in self
            .corpora
            .required
            .iter()
            .chain(self.corpora.optional.iter())
        {
            if !valid_slug(c, 128) {
                return bad(format!("invalid corpus slug {c:?}"));
            }
        }
        if self.corpora.min_hits == 0 {
            return bad("corpora.min_hits must be at least 1".to_string());
        }
        if self.corpora.min_evidence_hits == Some(0) {
            return bad("corpora.min_evidence_hits must be at least 1".to_string());
        }
        // A table keyed by an enum input needs a row for every value, each >= 1.
        let table_problem = |table: &InputTable, what: &str| -> Option<String> {
            let Some(field) = self.inputs.iter().find(|i| i.name == table.input) else {
                return Some(format!("{what} references unknown input {:?}", table.input));
            };
            if field.kind != FieldKind::String || field.enum_values.is_empty() || !field.required {
                return Some(format!(
                    "{what} input {:?} must be a required string input with enum_values",
                    table.input
                ));
            }
            for value in &field.enum_values {
                match table.values.get(value) {
                    Some(n) if *n >= 1 => {}
                    _ => {
                        return Some(format!(
                            "{what} has no positive entry for {:?} = {value:?}",
                            table.input
                        ))
                    }
                }
            }
            table
                .values
                .keys()
                .find(|k| !field.enum_values.contains(k))
                .map(|k| {
                    format!(
                        "{what} lists {k:?}, which is not a value of {:?}",
                        table.input
                    )
                })
        };
        if let Some(table) = &self.corpora.min_evidence_hits_by_input {
            if let Some(why) = table_problem(table, "corpora.min_evidence_hits_by_input") {
                return bad(why);
            }
        }

        for f in &self.filters {
            if !valid_ident(&f.field, 64) {
                return bad(format!("invalid filter field {:?}", f.field));
            }
            match (&f.value, &f.from_input) {
                (Some(_), None) => {}
                (None, Some(inp_name)) => {
                    if !input_names.contains(inp_name) {
                        return bad(format!(
                            "filter on {:?} references unknown input {:?}",
                            f.field, inp_name
                        ));
                    }
                }
                _ => {
                    return bad(format!(
                        "filter on {:?} must set exactly one of `value` or `from_input`",
                        f.field
                    ))
                }
            }
        }

        if self.ranking.top_k == 0 || self.ranking.top_k > 1_000 {
            return bad("ranking.top_k must be in 1..=1000".to_string());
        }
        if let Some(mpc) = self.ranking.max_per_cluster {
            if mpc == 0 || self.ranking.cluster_by.is_none() {
                return bad(
                    "ranking.max_per_cluster requires cluster_by and a positive limit".to_string(),
                );
            }
        }

        if self.output_schema.required_fields.is_empty() || self.output_schema.properties.is_empty()
        {
            return bad("output_schema must declare required_fields and properties".to_string());
        }
        for rf in &self.output_schema.required_fields {
            if !self.output_schema.properties.contains_key(rf) {
                return bad(format!(
                    "output_schema.required_fields names undeclared property {rf:?}"
                ));
            }
        }

        let mut check_ids = HashSet::new();
        for chk in &self.checks {
            if !valid_slug(&chk.id, 64) {
                return bad(format!("invalid check id {:?}", chk.id));
            }
            if !check_ids.insert(&chk.id) {
                return bad(format!("duplicate check id {:?}", chk.id));
            }
            let whole_output =
                chk.target_field == WHOLE_OUTPUT && chk.kind == CheckKind::MaxTokenBudget;
            if !whole_output
                && !self
                    .output_schema
                    .properties
                    .contains_key(&chk.target_field)
            {
                return bad(format!(
                    "check {:?} targets undeclared output property {:?}",
                    chk.id, chk.target_field
                ));
            }
            match chk.kind {
                CheckKind::ForbidPhrases if chk.phrases.is_empty() => {
                    return bad(format!(
                        "check {:?} (forbid_phrases) must list at least one phrase",
                        chk.id
                    ));
                }
                CheckKind::MaxTokenBudget => match (chk.max_tokens, &chk.max_tokens_from_input) {
                    (Some(n), None) if n > 0 => {}
                    (None, Some(input)) => {
                        let integer_input = self.inputs.iter().any(|i| {
                            &i.name == input && i.kind == FieldKind::Integer && i.required
                        });
                        let echoed = self
                            .output_schema
                            .properties
                            .get(input)
                            .is_some_and(|p| p.kind == FieldKind::Integer);
                        if !integer_input || !echoed {
                            return bad(format!(
                                    "check {:?} (max_token_budget) max_tokens_from_input {input:?} must be a required integer input that is also a declared integer output property",
                                    chk.id
                                ));
                        }
                    }
                    _ => {
                        return bad(format!(
                                "check {:?} (max_token_budget) must set exactly one of a positive max_tokens or max_tokens_from_input",
                                chk.id
                            ));
                    }
                },
                CheckKind::RequireNotice
                    if chk
                        .expected_substring
                        .as_deref()
                        .unwrap_or("")
                        .trim()
                        .is_empty() =>
                {
                    return bad(format!(
                        "check {:?} (require_notice) must set expected_substring",
                        chk.id
                    ));
                }
                _ => {}
            }
            if chk.fit_by_dropping && chk.kind != CheckKind::MaxTokenBudget {
                return bad(format!(
                    "check {:?} sets fit_by_dropping, which only a max_token_budget check has",
                    chk.id
                ));
            }
        }

        let declared = |name: &str, kind: FieldKind| {
            self.output_schema.properties.get(name).is_some_and(|p| {
                p.kind == kind || (kind == FieldKind::Array && p.kind == FieldKind::StringArray)
            })
        };
        if let Some(grouping) = &self.grouping {
            if self.ranking.cluster_by.is_none() {
                return bad("grouping groups by ranking.cluster_by, which is not set".to_string());
            }
            if !declared(&grouping.output_field, FieldKind::Array) {
                return bad(format!(
                    "grouping.output_field {:?} must be a declared array output property",
                    grouping.output_field
                ));
            }
            match (&grouping.chapters, &grouping.chapters_from_input) {
                (Some(0), _) => return bad("grouping.chapters must be at least 1".to_string()),
                (Some(_), Some(_)) => {
                    return bad("grouping sets both chapters and chapters_from_input".to_string())
                }
                (None, Some(table)) => {
                    if let Some(why) = table_problem(table, "grouping.chapters_from_input") {
                        return bad(why);
                    }
                }
                _ => {}
            }
        }
        for part in &self.partitions {
            if !declared(&part.output_field, FieldKind::Array) || !valid_ident(&part.field, 64) {
                return bad(format!(
                    "partition {:?} must name a declared array output property and a valid item member",
                    part.output_field
                ));
            }
        }
        if let Some(grading) = &self.grading {
            let fields_ok = valid_ident(&grading.study_type_field, 64)
                && valid_ident(&grading.human_field, 64)
                && grading.flag_fields.iter().all(|f| valid_ident(f, 64))
                && grading
                    .direction_field
                    .as_deref()
                    .is_none_or(|f| valid_ident(f, 64));
            if !fields_ok {
                return bad("grading names an invalid hit field".to_string());
            }
            let arrays_ok = declared(&grading.clinical_field, FieldKind::Array)
                && declared(&grading.non_clinical_field, FieldKind::Array)
                && grading
                    .contradictory_field
                    .as_deref()
                    .is_none_or(|f| declared(f, FieldKind::Array));
            let strings_ok = grading
                .abstention_field
                .as_deref()
                .is_none_or(|f| declared(f, FieldKind::String))
                && grading
                    .highest_clinical_tier_field
                    .as_deref()
                    .is_none_or(|f| declared(f, FieldKind::String));
            if !arrays_ok || !strings_ok || grading.abstention.trim().is_empty() {
                return bad("grading output fields must be declared output properties of the right kind, with a non-empty abstention".to_string());
            }
            if grading.rules.is_empty() || grading.non_human_rule_id.trim().is_empty() {
                return bad("grading needs rules and a non_human_rule_id".to_string());
            }
            let mut rule_ids = HashSet::new();
            for rule in &grading.rules {
                if rule.id.trim().is_empty() || !rule_ids.insert(rule.id.as_str()) {
                    return bad(format!(
                        "grading rule id {:?} is empty or duplicated",
                        rule.id
                    ));
                }
                if rule
                    .any_flags
                    .iter()
                    .chain(&rule.all_flags)
                    .any(|f| !grading.flag_fields.contains(f))
                {
                    return bad(format!(
                        "grading rule {:?} uses a flag that is not in flag_fields",
                        rule.id
                    ));
                }
            }
            if let Some(missing) = StudyType::ALL
                .into_iter()
                .find(|t| grading.rules.iter().all(|r| r.study_type != *t))
            {
                return bad(format!(
                    "grading rules have no row for study type {:?}",
                    missing.slug()
                ));
            }
        }
        Ok(())
    }

    /// Project this recipe into a standard MCP `tools/list` tool specification.
    pub fn to_mcp_tool_spec(&self) -> Value {
        let mut props = Map::new();
        let mut req = Vec::new();
        for inp in &self.inputs {
            let mut p = Map::new();
            match inp.kind {
                FieldKind::String => {
                    p.insert("type".into(), json!("string"));
                }
                FieldKind::Integer => {
                    p.insert("type".into(), json!("integer"));
                }
                FieldKind::Number => {
                    p.insert("type".into(), json!("number"));
                }
                FieldKind::Boolean => {
                    p.insert("type".into(), json!("boolean"));
                }
                FieldKind::StringArray => {
                    p.insert("type".into(), json!("array"));
                    p.insert("items".into(), json!({"type": "string"}));
                }
                FieldKind::Object => {
                    p.insert("type".into(), json!("object"));
                }
                FieldKind::Array => {
                    p.insert("type".into(), json!("array"));
                }
            }
            p.insert("description".into(), json!(inp.description));
            if !inp.enum_values.is_empty() {
                p.insert("enum".into(), json!(inp.enum_values));
            }
            if let Some(min) = inp.min_len {
                p.insert("minLength".into(), json!(min));
            }
            if let Some(max) = inp.max_len {
                p.insert("maxLength".into(), json!(max));
            }
            props.insert(inp.name.clone(), Value::Object(p));
            if inp.required {
                req.push(Value::String(inp.name.clone()));
            }
        }
        let description = if self.recipe.sample {
            format!(
                "[SAMPLE recipe, not a served tool] {}",
                self.recipe.description
            )
        } else {
            self.recipe.description.clone()
        };
        json!({
            "name": self.mcp_name(),
            "description": description,
            "inputSchema": {
                "type": "object",
                "properties": props,
                "required": req,
                "additionalProperties": false,
            }
        })
    }

    /// Validate MCP `tools/call` arguments against `[[inputs]]`.
    pub fn validate_arguments(&self, args: &Value) -> Result<Map<String, Value>> {
        let rname = &self.recipe.name;
        let bad = |reason: String| -> Result<Map<String, Value>> {
            Err(RecipeError::InvalidArguments {
                recipe: rname.clone(),
                reason,
            })
        };
        let Some(obj) = args.as_object() else {
            return bad("arguments must be a JSON object".to_string());
        };
        let declared: HashSet<&str> = self.inputs.iter().map(|i| i.name.as_str()).collect();
        for k in obj.keys() {
            if !declared.contains(k.as_str()) {
                return bad(format!("unknown argument {k:?}"));
            }
        }
        for inp in &self.inputs {
            match obj.get(&inp.name) {
                None | Some(Value::Null) => {
                    if inp.required {
                        return bad(format!("missing required argument {:?}", inp.name));
                    }
                }
                Some(v) => match inp.kind {
                    FieldKind::String => {
                        let Some(s) = v.as_str() else {
                            return bad(format!("argument {:?} must be a string", inp.name));
                        };
                        if let Some(min) = inp.min_len {
                            if s.chars().count() < min {
                                return bad(format!(
                                    "argument {:?} is shorter than min_len {min}",
                                    inp.name
                                ));
                            }
                        }
                        if let Some(max) = inp.max_len {
                            if s.chars().count() > max {
                                return bad(format!(
                                    "argument {:?} exceeds max_len {max}",
                                    inp.name
                                ));
                            }
                        }
                        if !inp.enum_values.is_empty() && !inp.enum_values.iter().any(|e| e == s) {
                            return bad(format!(
                                "argument {:?} must be one of {:?}",
                                inp.name, inp.enum_values
                            ));
                        }
                    }
                    FieldKind::Integer => {
                        if !v.is_i64() && !v.is_u64() {
                            return bad(format!("argument {:?} must be an integer", inp.name));
                        }
                    }
                    FieldKind::Number => {
                        if !v.is_number() {
                            return bad(format!("argument {:?} must be a number", inp.name));
                        }
                    }
                    FieldKind::Boolean => {
                        if !v.is_boolean() {
                            return bad(format!("argument {:?} must be a boolean", inp.name));
                        }
                    }
                    FieldKind::StringArray => {
                        let Some(arr) = v.as_array() else {
                            return bad(format!(
                                "argument {:?} must be an array of strings",
                                inp.name
                            ));
                        };
                        if arr.iter().any(|item| !item.is_string()) {
                            return bad(format!(
                                "argument {:?} must contain only strings",
                                inp.name
                            ));
                        }
                    }
                    FieldKind::Object | FieldKind::Array => {}
                },
            }
        }
        Ok(obj.clone())
    }

    /// The text a retrieval-backed [`RecipeCorpusSource`] ranks its records by:
    /// the call's free-text inputs (string and string-array inputs that are not
    /// enums), in declaration order, separated by a space. An enum input picks a
    /// variant of the recipe (`length = "standard"`); it is not search text.
    pub fn corpus_query(&self, valid_args: &Map<String, Value>) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for inp in self.inputs.iter().filter(|i| i.enum_values.is_empty()) {
            match (inp.kind, valid_args.get(&inp.name)) {
                (FieldKind::String, Some(Value::String(s))) => parts.push(s),
                (FieldKind::StringArray, Some(Value::Array(items))) => {
                    parts.extend(items.iter().filter_map(Value::as_str));
                }
                _ => {}
            }
        }
        let parts: Vec<&str> = parts
            .into_iter()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        parts.join(" ")
    }

    /// Validate a candidate output JSON object against `[output_schema]`.
    pub fn validate_output(&self, output: &Value) -> Result<()> {
        let rname = &self.recipe.name;
        let bad = |reason: String| -> Result<()> {
            Err(RecipeError::OutputSchemaViolation {
                recipe: rname.clone(),
                reason,
            })
        };
        let Some(obj) = output.as_object() else {
            return bad("output must be a JSON object".to_string());
        };
        for req in &self.output_schema.required_fields {
            if !obj.contains_key(req) || obj.get(req) == Some(&Value::Null) {
                return bad(format!("missing required output field {req:?}"));
            }
        }
        for k in obj.keys() {
            if !self.output_schema.properties.contains_key(k) {
                return bad(format!("undeclared output field {k:?}"));
            }
        }
        for (k, spec) in &self.output_schema.properties {
            let Some(val) = obj.get(k) else {
                continue;
            };
            if val.is_null() {
                continue;
            }
            match spec.kind {
                FieldKind::String => {
                    let Some(s) = val.as_str() else {
                        return bad(format!("output field {k:?} must be a string"));
                    };
                    if spec.non_empty && s.trim().is_empty() {
                        return bad(format!("output field {k:?} cannot be blank"));
                    }
                    if let Some(pref) = &spec.pattern_prefix {
                        if !s.starts_with(pref) {
                            return bad(format!("output field {k:?} must start with {pref:?}"));
                        }
                    }
                }
                FieldKind::Integer => {
                    if !val.is_i64() && !val.is_u64() {
                        return bad(format!("output field {k:?} must be an integer"));
                    }
                }
                FieldKind::Number => {
                    if !val.is_number() {
                        return bad(format!("output field {k:?} must be a number"));
                    }
                }
                FieldKind::Boolean => {
                    if !val.is_boolean() {
                        return bad(format!("output field {k:?} must be a boolean"));
                    }
                }
                FieldKind::Array | FieldKind::StringArray => {
                    let Some(arr) = val.as_array() else {
                        return bad(format!("output field {k:?} must be an array"));
                    };
                    if spec.non_empty && arr.is_empty() {
                        return bad(format!("output field {k:?} cannot be empty"));
                    }
                    if !spec.item_required_fields.is_empty() {
                        for (idx, item) in arr.iter().enumerate() {
                            let Some(item_obj) = item.as_object() else {
                                return bad(format!("output field {k:?}[{idx}] must be an object"));
                            };
                            for req_f in &spec.item_required_fields {
                                match item_obj.get(req_f) {
                                    None | Some(Value::Null) => {
                                        return bad(format!(
                                            "output field {k:?}[{idx}] missing required member {req_f:?}"
                                        ));
                                    }
                                    Some(Value::String(s)) if s.trim().is_empty() => {
                                        return bad(format!(
                                            "output field {k:?}[{idx}].{req_f} cannot be blank"
                                        ));
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                FieldKind::Object => {
                    if !val.is_object() {
                        return bad(format!("output field {k:?} must be an object"));
                    }
                }
            }
        }
        Ok(())
    }

    /// The ceiling of a `max_token_budget` check: its `max_tokens`, or the
    /// integer input it names, which the output echoes.
    fn token_ceiling(&self, chk: &CheckStep, output: &Map<String, Value>) -> Result<usize> {
        match &chk.max_tokens_from_input {
            Some(input) => output
                .get(input)
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| RecipeError::CheckFailed {
                    recipe: self.recipe.name.clone(),
                    check_id: chk.id.clone(),
                    reason: format!("output has no integer budget {input:?}"),
                }),
            None => Ok(chk.max_tokens.unwrap_or(1)),
        }
    }

    /// The evidence floor for these arguments: the larger of the static
    /// `min_evidence_hits` and the row of `min_evidence_hits_by_input` the
    /// argument selects.
    fn evidence_floor(&self, args: &Map<String, Value>) -> usize {
        let by_input = self
            .corpora
            .min_evidence_hits_by_input
            .as_ref()
            .and_then(|t| {
                args.get(&t.input)
                    .and_then(Value::as_str)
                    .and_then(|v| t.values.get(v))
            })
            .copied()
            .unwrap_or(0);
        self.corpora.min_evidence_hits.unwrap_or(0).max(by_input)
    }

    /// Run all deterministic `[[checks]]` on a schema-valid output object.
    pub fn run_checks(&self, output: &Value) -> Result<Vec<CheckOutcome>> {
        let obj = output
            .as_object()
            .ok_or_else(|| RecipeError::OutputSchemaViolation {
                recipe: self.recipe.name.clone(),
                reason: "output must be a JSON object".into(),
            })?;
        let mut outcomes = Vec::with_capacity(self.checks.len());
        for chk in &self.checks {
            let val = obj.get(&chk.target_field).unwrap_or(&Value::Null);
            match chk.kind {
                CheckKind::RequireCitations => {
                    let arr = val.as_array().ok_or_else(|| RecipeError::CheckFailed {
                        recipe: self.recipe.name.clone(),
                        check_id: chk.id.clone(),
                        reason: format!(
                            "target field {:?} must be an array of cited items",
                            chk.target_field
                        ),
                    })?;
                    if arr.is_empty() {
                        return Err(RecipeError::CheckFailed {
                            recipe: self.recipe.name.clone(),
                            check_id: chk.id.clone(),
                            reason: format!(
                                "target field {:?} has 0 cited items",
                                chk.target_field
                            ),
                        });
                    }
                    for (idx, item) in arr.iter().enumerate() {
                        let c = item.get("corpus").and_then(Value::as_str).unwrap_or("");
                        let r = item.get("record_id").and_then(Value::as_str).unwrap_or("");
                        let p = item.get("pin_sha").and_then(Value::as_str).unwrap_or("");
                        if c.is_empty()
                            || r.is_empty()
                            || p.len() != 40
                            || !p
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                        {
                            return Err(RecipeError::CheckFailed {
                                recipe: self.recipe.name.clone(),
                                check_id: chk.id.clone(),
                                reason: format!(
                                    "item {idx} in {:?} is missing a valid corpus, record_id, or 40-hex pin_sha",
                                    chk.target_field
                                ),
                            });
                        }
                    }
                    outcomes.push(CheckOutcome {
                        check_id: chk.id.clone(),
                        passed: true,
                        detail: format!("verified {} cited items", arr.len()),
                    });
                }
                CheckKind::ForbidPhrases => {
                    let text = serde_json::to_string(val)?.to_ascii_lowercase();
                    for phrase in &chk.phrases {
                        if text.contains(&phrase.to_ascii_lowercase()) {
                            return Err(RecipeError::CheckFailed {
                                recipe: self.recipe.name.clone(),
                                check_id: chk.id.clone(),
                                reason: format!(
                                    "field {:?} contains banned phrase {phrase:?}",
                                    chk.target_field
                                ),
                            });
                        }
                    }
                    outcomes.push(CheckOutcome {
                        check_id: chk.id.clone(),
                        passed: true,
                        detail: format!("0 banned phrases found in {:?}", chk.target_field),
                    });
                }
                CheckKind::MaxTokenBudget => {
                    let max_t = self.token_ceiling(chk, obj)?;
                    let est_tokens = measured_tokens(output, &chk.target_field)?;
                    if est_tokens > max_t {
                        return Err(RecipeError::CheckFailed {
                            recipe: self.recipe.name.clone(),
                            check_id: chk.id.clone(),
                            reason: format!(
                                "field {:?} measures {est_tokens} tokens (max {max_t})",
                                chk.target_field
                            ),
                        });
                    }
                    outcomes.push(CheckOutcome {
                        check_id: chk.id.clone(),
                        passed: true,
                        detail: format!("measured {est_tokens} tokens <= {max_t}"),
                    });
                }
                CheckKind::RequireNotice => {
                    let needle = chk.expected_substring.as_deref().unwrap_or("");
                    let s = val.as_str().unwrap_or("");
                    if !s.contains(needle) {
                        return Err(RecipeError::CheckFailed {
                            recipe: self.recipe.name.clone(),
                            check_id: chk.id.clone(),
                            reason: format!(
                                "field {:?} is missing required notice substring {needle:?}",
                                chk.target_field
                            ),
                        });
                    }
                    outcomes.push(CheckOutcome {
                        check_id: chk.id.clone(),
                        passed: true,
                        detail: format!("required notice present in {:?}", chk.target_field),
                    });
                }
            }
        }
        Ok(outcomes)
    }
}

/// One candidate record from a hub corpus offered to [`RecipeRunner`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeCorpusHit {
    pub corpus: String,
    pub licence_class: LicenceClass,
    pub record_id: String,
    pub title: String,
    pub excerpt: String,
    pub score: f64,
    pub published_date: String,
    /// Optional study grade rank (`1` = Grade A .. `4` = Grade D) for bio recipes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub study_grade_rank: Option<u8>,
    pub pin_sha: String,
    pub fetched_at: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_citation: Option<PinnedCitation>,
}

/// The most hits one corpus contributes to one [`RecipeRunner::execute_from_source`]
/// call. Retrieval-backed sources rank by the query and return the best this
/// many; a pinned snapshot larger than this is cut to its best-scored records.
/// The runner filters, grades and ranks what it is given, so a recipe's
/// `top_k` and `min_hits` are far below it.
pub const MAX_SOURCE_HITS_PER_CORPUS: usize = 5_000;

/// Largest `<corpus>.jsonl` snapshot [`JsonlCorpusDir`] reads for one call.
const MAX_CORPUS_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Where [`RecipeRunner::execute_from_source`] gets the hits of each corpus the
/// recipe's `[corpora]` block names. The runner filters, grades and ranks them;
/// a source only holds them. Implementations: a map in memory (`BTreeMap`, for
/// tests and callers that already hold the hits), [`JsonlCorpusDir`] (pinned
/// `<corpus>.jsonl` files), [`ChainedCorpusSource`], and `xerj-hub`'s
/// `HubPackDir` (signed `.hubpack.tar.zst` archives).
pub trait RecipeCorpusSource: Send + Sync {
    /// The hits `corpus` holds for `query`, at most `max_hits`.
    ///
    /// `query` is the call's free-text inputs ([`PackRecipe::corpus_query`]); a
    /// source that retrieves ranks by it, and one that holds curated hits may
    /// ignore it. Every returned hit must say `corpus` as its own `corpus`.
    ///
    /// A corpus the source does not hold is
    /// [`RecipeError::CorpusNotInstalled`]; one it holds but cannot read is
    /// [`RecipeError::CorpusUnreadable`]. An empty `Ok` means installed, nothing
    /// to offer, which a required corpus cannot be.
    fn fetch_corpus_hits(
        &self,
        corpus: &str,
        query: &str,
        max_hits: usize,
    ) -> Result<Vec<RecipeCorpusHit>>;
}

/// The best-scored `max_hits` of `hits` (score, then `record_id`), or all of
/// them in their given order when there are no more than that.
fn cap_hits(mut hits: Vec<RecipeCorpusHit>, max_hits: usize) -> Vec<RecipeCorpusHit> {
    if hits.len() > max_hits {
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.record_id.cmp(&b.record_id))
        });
        hits.truncate(max_hits);
    }
    hits
}

impl RecipeCorpusSource for BTreeMap<String, Vec<RecipeCorpusHit>> {
    fn fetch_corpus_hits(
        &self,
        corpus: &str,
        _query: &str,
        max_hits: usize,
    ) -> Result<Vec<RecipeCorpusHit>> {
        match self.get(corpus) {
            Some(hits) => Ok(cap_hits(hits.clone(), max_hits)),
            None => Err(RecipeError::CorpusNotInstalled {
                corpus: corpus.to_string(),
            }),
        }
    }
}

/// Hits pinned in files: `<dir>/<corpus>.jsonl`, one [`RecipeCorpusHit`] JSON
/// object per line. A snapshot is curated for the recipes that name it, so it is
/// returned whole (the query is not applied): the recipe's own `[[filters]]`
/// select from it. A corpus with no file is
/// [`RecipeError::CorpusNotInstalled`].
#[derive(Debug, Clone)]
pub struct JsonlCorpusDir {
    dir: PathBuf,
}

impl JsonlCorpusDir {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl RecipeCorpusSource for JsonlCorpusDir {
    fn fetch_corpus_hits(
        &self,
        corpus: &str,
        _query: &str,
        max_hits: usize,
    ) -> Result<Vec<RecipeCorpusHit>> {
        let unreadable = |reason: String| RecipeError::CorpusUnreadable {
            corpus: corpus.to_string(),
            reason,
        };
        // A recipe slug may hold `:`, which is a stream separator on NTFS, and a
        // leading `.`; neither names a snapshot file.
        let file_name_ok = corpus
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
            && corpus
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if !file_name_ok {
            return Err(unreadable(
                "a snapshot file is named after its corpus, which may use only letters, digits, \
                 `.`, `_` and `-`, starting with a letter or digit"
                    .to_string(),
            ));
        }
        let path = self.dir.join(format!("{corpus}.jsonl"));
        let len = match std::fs::metadata(&path) {
            Ok(meta) => meta.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(RecipeError::CorpusNotInstalled {
                    corpus: corpus.to_string(),
                })
            }
            Err(e) => return Err(unreadable(format!("{}: {e}", path.display()))),
        };
        if len > MAX_CORPUS_FILE_BYTES {
            return Err(unreadable(format!(
                "{} is {len} bytes; the limit is {MAX_CORPUS_FILE_BYTES}",
                path.display()
            )));
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| unreadable(format!("{}: {e}", path.display())))?;
        let mut hits = Vec::new();
        for (n, line) in raw
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
        {
            let hit: RecipeCorpusHit = serde_json::from_str(line)
                .map_err(|e| unreadable(format!("{}:{}: {e}", path.display(), n + 1)))?;
            if hit.corpus != corpus {
                return Err(unreadable(format!(
                    "{}:{}: the hit says corpus `{}`, but this file is `{corpus}`",
                    path.display(),
                    n + 1,
                    hit.corpus
                )));
            }
            hits.push(hit);
        }
        Ok(cap_hits(hits, max_hits))
    }
}

/// Sources tried in order. The first that holds the corpus answers, so a pinned
/// snapshot can sit in front of a pack archive; a source that does not hold it
/// ([`RecipeError::CorpusNotInstalled`]) passes the question on, and any other
/// error stops the chain (a corpus that is present but broken is not "absent").
#[derive(Default)]
pub struct ChainedCorpusSource {
    sources: Vec<Box<dyn RecipeCorpusSource>>,
}

impl ChainedCorpusSource {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn then(mut self, source: impl RecipeCorpusSource + 'static) -> Self {
        self.sources.push(Box::new(source));
        self
    }
}

impl RecipeCorpusSource for ChainedCorpusSource {
    fn fetch_corpus_hits(
        &self,
        corpus: &str,
        query: &str,
        max_hits: usize,
    ) -> Result<Vec<RecipeCorpusHit>> {
        for source in &self.sources {
            match source.fetch_corpus_hits(corpus, query, max_hits) {
                Err(RecipeError::CorpusNotInstalled { .. }) => continue,
                held => return held,
            }
        }
        Err(RecipeError::CorpusNotInstalled {
            corpus: corpus.to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckOutcome {
    pub check_id: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeExecutionReport {
    pub report_version: String,
    pub recipe_name: String,
    pub mcp_tool_name: String,
    pub output: Value,
    pub checks: Vec<CheckOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<WorkOrder>,
    pub report_sha256: String,
}

#[derive(Serialize)]
struct Preimage<'a> {
    report_version: &'a str,
    recipe_name: &'a str,
    mcp_tool_name: &'a str,
    output: &'a Value,
    checks: &'a [CheckOutcome],
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: &'a Option<WorkOrder>,
}

pub struct RecipeRunner;

/// The `ScopeGuard` gate of a `requires_scope` recipe: the declared work order
/// it runs under, or `None` for a recipe that needs none.
fn check_scope(
    recipe: &PackRecipe,
    valid_args: &Map<String, Value>,
    guard: Option<&ScopeGuard>,
    origin: InputOrigin,
    now: DateTime<Utc>,
) -> Result<Option<WorkOrder>> {
    if !recipe.recipe.requires_scope {
        return Ok(None);
    }
    let g = guard.ok_or_else(|| RecipeError::ScopeRequired {
        recipe: recipe.recipe.name.clone(),
    })?;
    if origin != InputOrigin::AuthorizedControlPlane {
        return Err(RecipeError::Scope(ScopeError::UntrustedInput));
    }
    let wo = g.scope();
    if now < wo.not_before {
        return Err(RecipeError::Scope(ScopeError::NotYetValid));
    }
    if now > wo.expires_at {
        return Err(RecipeError::Scope(ScopeError::Expired));
    }
    if let Some(target) = valid_args.get("target").and_then(Value::as_str) {
        let _ = g.authorize(target, origin, now)?;
    }
    Ok(Some(wo.clone()))
}

impl RecipeRunner {
    /// Execute a declarative [`PackRecipe`] over the recipe's `[corpora]`: ask
    /// `source` for every `required` corpus, then every `optional` one, and run
    /// [`RecipeRunner::execute`] over what they hold.
    ///
    /// A required corpus the source does not hold, or holds with no hit, is
    /// [`RecipeError::MissingRequiredCorpus`]; one it holds but cannot read is
    /// [`RecipeError::CorpusUnreadable`]. Either way nothing is reported: a
    /// recipe never answers without the corpora it requires. An optional corpus
    /// the source does not hold is skipped and the others still answer; one that
    /// is held but unreadable is as fatal as a required one. Hits whose `corpus`
    /// is not the one asked for are dropped, so only declared corpora contribute.
    ///
    /// The arguments and the `ScopeGuard` gate are checked before the source is
    /// asked for anything.
    pub fn execute_from_source(
        recipe: &PackRecipe,
        args: &Value,
        source: &dyn RecipeCorpusSource,
        guard: Option<&ScopeGuard>,
        origin: InputOrigin,
        now: DateTime<Utc>,
    ) -> Result<RecipeExecutionReport> {
        recipe.validate()?;
        let valid_args = recipe.validate_arguments(args)?;
        check_scope(recipe, &valid_args, guard, origin, now)?;
        let query = recipe.corpus_query(&valid_args);

        let required = recipe.corpora.required.iter().map(|c| (c, true));
        let optional = recipe.corpora.optional.iter().map(|c| (c, false));
        let mut asked: HashSet<&str> = HashSet::new();
        let mut hits: Vec<RecipeCorpusHit> = Vec::new();
        for (corpus, is_required) in required.chain(optional) {
            // A corpus listed as both is asked for once, as required.
            if !asked.insert(corpus.as_str()) {
                continue;
            }
            let missing = || RecipeError::MissingRequiredCorpus {
                recipe: recipe.recipe.name.clone(),
                corpus: corpus.clone(),
            };
            match source.fetch_corpus_hits(corpus, &query, MAX_SOURCE_HITS_PER_CORPUS) {
                Ok(found) => {
                    let before = hits.len();
                    hits.extend(found.into_iter().filter(|h| h.corpus == *corpus));
                    if is_required && hits.len() == before {
                        return Err(missing());
                    }
                }
                Err(
                    RecipeError::CorpusNotInstalled { .. }
                    | RecipeError::MissingRequiredCorpus { .. },
                ) => {
                    if is_required {
                        return Err(missing());
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Self::execute(recipe, args, &hits, guard, origin, now)
    }

    /// Execute a declarative [`PackRecipe`] over supplied corpus hits and MCP
    /// arguments, validating the output against the recipe's schema and running
    /// every deterministic check step before sealing the report.
    ///
    /// `[corpora]` governs which hits count, whoever supplied them: a hit from a
    /// corpus the recipe does not declare (`required` or `optional`) is ignored,
    /// and a `required` corpus with no hit among those left is
    /// [`RecipeError::MissingRequiredCorpus`].
    pub fn execute(
        recipe: &PackRecipe,
        args: &Value,
        hits: &[RecipeCorpusHit],
        guard: Option<&ScopeGuard>,
        origin: InputOrigin,
        now: DateTime<Utc>,
    ) -> Result<RecipeExecutionReport> {
        recipe.validate()?;
        let valid_args = recipe.validate_arguments(args)?;
        let scope_echo = check_scope(recipe, &valid_args, guard, origin, now)?;

        let allowed_corpora: HashSet<&str> = recipe
            .corpora
            .required
            .iter()
            .chain(recipe.corpora.optional.iter())
            .map(String::as_str)
            .collect();

        let mut present_required: HashSet<&str> = HashSet::new();
        let mut filtered: Vec<&RecipeCorpusHit> = Vec::new();

        for hit in hits {
            if !allowed_corpora.contains(hit.corpus.as_str()) {
                continue;
            }
            if recipe.corpora.require_redistribution_licence
                && hit.licence_class == LicenceClass::Excluded
            {
                return Err(RecipeError::ExcludedCorpusLicence {
                    recipe: recipe.recipe.name.clone(),
                    corpus: hit.corpus.clone(),
                });
            }
            if recipe.corpora.required.iter().any(|c| c == &hit.corpus) {
                present_required.insert(hit.corpus.as_str());
            }
            if matches_filters(hit, &recipe.filters, &valid_args) {
                filtered.push(hit);
            }
        }

        for req_c in &recipe.corpora.required {
            if !present_required.contains(req_c.as_str()) {
                return Err(RecipeError::MissingRequiredCorpus {
                    recipe: recipe.recipe.name.clone(),
                    corpus: req_c.clone(),
                });
            }
        }

        if filtered.len() < recipe.corpora.min_hits {
            return Err(RecipeError::InsufficientEvidence {
                recipe: recipe.recipe.name.clone(),
                matched: filtered.len(),
                min_hits: recipe.corpora.min_hits,
            });
        }

        // Grade every candidate first when the recipe grades: the computed tier,
        // not a caller-supplied rank, orders `study_grade_asc`.
        let mut candidates: Vec<Candidate<'_>> = Vec::with_capacity(filtered.len());
        for hit in filtered {
            let grade = match &recipe.grading {
                Some(step) => Some(grade_hit(recipe, step, hit)?),
                None => None,
            };
            candidates.push(Candidate { hit, grade });
        }

        // Deterministic ranking
        candidates.sort_by(|a, b| {
            let (a_hit, b_hit) = (a.hit, b.hit);
            let by_score = || {
                b_hit
                    .score
                    .partial_cmp(&a_hit.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            };
            let primary = match recipe.ranking.strategy {
                RankingStrategy::ScoreDesc => by_score(),
                RankingStrategy::RecencyDesc => b_hit.published_date.cmp(&a_hit.published_date),
                RankingStrategy::StudyGradeAsc => match (&a.grade, &b.grade) {
                    (Some(ga), Some(gb)) => ga.tier.cmp(&gb.tier),
                    _ => a_hit
                        .study_grade_rank
                        .unwrap_or(99)
                        .cmp(&b_hit.study_grade_rank.unwrap_or(99)),
                }
                .then_with(by_score),
            };
            primary
                .then_with(|| a_hit.corpus.cmp(&b_hit.corpus))
                .then_with(|| a_hit.record_id.cmp(&b_hit.record_id))
        });

        // Optional per-cluster cap + top_k truncation
        let mut selected: Vec<Candidate<'_>> = Vec::new();
        let mut cluster_counts: HashMap<String, usize> = HashMap::new();
        for candidate in candidates {
            if selected.len() >= recipe.ranking.top_k {
                break;
            }
            if let (Some(cfield), Some(max_c)) =
                (&recipe.ranking.cluster_by, recipe.ranking.max_per_cluster)
            {
                let key = hit_field(candidate.hit, cfield)
                    .map(Cow::into_owned)
                    .unwrap_or_else(|| "default".to_string());
                let count = cluster_counts.entry(key).or_insert(0);
                if *count >= max_c {
                    continue;
                }
                *count += 1;
            }
            selected.push(candidate);
        }

        // The evidence floor counts what the output will carry. Fewer is a
        // refusal, not a padded structure.
        let floor = recipe.evidence_floor(&valid_args);
        if selected.len() < floor {
            return Err(RecipeError::InsufficientEvidence {
                recipe: recipe.recipe.name.clone(),
                matched: selected.len(),
                min_hits: floor,
            });
        }

        let mut output = assemble_output(recipe, &valid_args, &selected)?;
        // A `fit_by_dropping` budget removes the lowest-ranked items outside the
        // mandatory core until the output, as a consumer receives it, fits.
        for chk in recipe
            .checks
            .iter()
            .filter(|c| c.kind == CheckKind::MaxTokenBudget && c.fit_by_dropping)
        {
            let Value::Object(first) = &output else {
                break;
            };
            let ceiling = recipe.token_ceiling(chk, first)?;
            while measured_tokens(&output, &chk.target_field)? > ceiling {
                let kept = mandatory_core(recipe, &selected);
                let Some(drop_at) = (0..selected.len()).rev().find(|i| !kept.contains(i)) else {
                    break;
                };
                selected.remove(drop_at);
                output = assemble_output(recipe, &valid_args, &selected)?;
            }
        }

        recipe.validate_output(&output)?;
        let checks = recipe.run_checks(&output)?;

        let mut report = RecipeExecutionReport {
            report_version: RECIPE_REPORT_VERSION.to_string(),
            recipe_name: recipe.recipe.name.clone(),
            mcp_tool_name: recipe.mcp_name(),
            output,
            checks,
            scope: scope_echo,
            report_sha256: String::new(),
        };
        report.report_sha256 = report.compute_digest()?;
        Ok(report)
    }
}

impl RecipeExecutionReport {
    fn compute_digest(&self) -> Result<String> {
        let pre = Preimage {
            report_version: &self.report_version,
            recipe_name: &self.recipe_name,
            mcp_tool_name: &self.mcp_tool_name,
            output: &self.output,
            checks: &self.checks,
            scope: &self.scope,
        };
        let bytes = serde_json_canonicalizer::to_vec(&pre)?;
        Ok(sha256(&bytes))
    }

    pub fn to_jcs(&self, recipe: &PackRecipe) -> Result<Vec<u8>> {
        self.verify(recipe)?;
        let out = serde_json_canonicalizer::to_vec(self)?;
        if out.len() > MAX_REPORT_BYTES {
            return Err(RecipeError::InvalidRecipe {
                recipe: self.recipe_name.clone(),
                reason: "report exceeds size limit".into(),
            });
        }
        Ok(out)
    }

    pub fn verify(&self, recipe: &PackRecipe) -> Result<()> {
        recipe.validate()?;
        if self.report_version != RECIPE_REPORT_VERSION || self.recipe_name != recipe.recipe.name {
            return Err(RecipeError::InvalidRecipe {
                recipe: recipe.recipe.name.clone(),
                reason: "report_version or recipe_name mismatch".into(),
            });
        }
        if recipe.recipe.requires_scope && self.scope.is_none() {
            return Err(RecipeError::ScopeRequired {
                recipe: recipe.recipe.name.clone(),
            });
        }
        recipe.validate_output(&self.output)?;
        let _ = recipe.run_checks(&self.output)?;
        let expected = self.compute_digest()?;
        if expected != self.report_sha256 {
            return Err(RecipeError::DigestMismatch);
        }
        Ok(())
    }
}

/// The first four alphanumeric tokens of the excerpt, then the title,
/// lower-cased: the derived `theme_key` a hit is grouped under when a recipe
/// clusters by it. Short tokens and digits are kept, so "theme 1" and "theme 2"
/// are different themes.
fn theme_key(hit: &RecipeCorpusHit) -> String {
    hit.excerpt
        .split(|c: char| !c.is_alphanumeric())
        .chain(hit.title.split(|c: char| !c.is_alphanumeric()))
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .take(4)
        .collect::<Vec<_>>()
        .join(" ")
}

fn hit_field<'a>(hit: &'a RecipeCorpusHit, field: &str) -> Option<Cow<'a, str>> {
    match field {
        "corpus" => Some(Cow::Borrowed(hit.corpus.as_str())),
        "record_id" => Some(Cow::Borrowed(hit.record_id.as_str())),
        "title" => Some(Cow::Borrowed(hit.title.as_str())),
        "excerpt" => Some(Cow::Borrowed(hit.excerpt.as_str())),
        "published_date" => Some(Cow::Borrowed(hit.published_date.as_str())),
        "theme_key" => Some(
            hit.fields
                .get("theme_key")
                .map(|v| Cow::Borrowed(v.as_str()))
                .unwrap_or_else(|| Cow::Owned(theme_key(hit))),
        ),
        _ => hit.fields.get(field).map(|v| Cow::Borrowed(v.as_str())),
    }
}

fn op_matches(op: FilterOp, actual: &str, target: &str) -> bool {
    match op {
        FilterOp::Equals => actual.eq_ignore_ascii_case(target),
        FilterOp::NotEquals => !actual.eq_ignore_ascii_case(target),
        FilterOp::Contains => actual
            .to_ascii_lowercase()
            .contains(&target.to_ascii_lowercase()),
        FilterOp::Prefix => actual
            .to_ascii_lowercase()
            .starts_with(&target.to_ascii_lowercase()),
        FilterOp::In => target
            .split(',')
            .any(|part| actual.eq_ignore_ascii_case(part.trim())),
    }
}

fn matches_filters(
    hit: &RecipeCorpusHit,
    filters: &[FilterStep],
    args: &Map<String, Value>,
) -> bool {
    for f in filters {
        let target_val = if let Some(v) = &f.value {
            v.as_str()
        } else if let Some(inp) = &f.from_input {
            match args.get(inp).and_then(Value::as_str) {
                Some(s) => s,
                None => continue,
            }
        } else {
            continue;
        };
        let Some(actual) = hit_field(hit, &f.field) else {
            return false;
        };
        if !op_matches(f.op, &actual, target_val) {
            return false;
        }
    }
    true
}

/// A candidate hit and, when the recipe grades, its grade.
struct Candidate<'a> {
    hit: &'a RecipeCorpusHit,
    grade: Option<Graded>,
}

struct Graded {
    study_type: StudyType,
    tier: EvidenceTier,
    human: bool,
    /// The `GradeRule` receipt as it appears on the item.
    rule: Value,
    direction: Option<String>,
}

impl Graded {
    /// Human clinical evidence: a human study graded above the `Level5` floor.
    fn clinical(&self) -> bool {
        self.human && self.tier < EvidenceTier::Level5
    }

    fn adverse(&self) -> bool {
        matches!(
            self.direction.as_deref(),
            Some("contradictory") | Some("adverse")
        )
    }
}

fn grade_hit(recipe: &PackRecipe, step: &GradingStep, hit: &RecipeCorpusHit) -> Result<Graded> {
    let bad = |reason: String| RecipeError::InvalidEvidence {
        recipe: recipe.recipe.name.clone(),
        record_id: hit.record_id.clone(),
        reason,
    };
    let raw = hit_field(hit, &step.study_type_field)
        .ok_or_else(|| bad(format!("no {:?} field", step.study_type_field)))?;
    let study_type =
        StudyType::parse(&raw).ok_or_else(|| bad(format!("unknown study type {raw:?}")))?;
    let flag =
        |name: &str| hit_field(hit, name).is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
    let human = flag(&step.human_field);
    let direction = step
        .direction_field
        .as_deref()
        .and_then(|f| hit_field(hit, f))
        .map(|d| d.trim().to_ascii_lowercase());
    let spec = step
        .rules
        .iter()
        .find(|r| {
            r.study_type == study_type
                && (r.any_flags.is_empty() || r.any_flags.iter().any(|f| flag(f)))
                && r.all_flags.iter().all(|f| flag(f))
        })
        .ok_or_else(|| {
            bad(format!(
                "no grading rule for study type {:?}",
                study_type.slug()
            ))
        })?;

    let mut signals = vec![format!("study_type:{}", study_type.slug())];
    for name in spec.any_flags.iter().chain(&spec.all_flags) {
        let signal = format!("flag:{name}");
        if flag(name) && !signals.contains(&signal) {
            signals.push(signal);
        }
    }
    let (mut rule_id, mut tier, mut rationale) =
        (spec.id.clone(), spec.tier, spec.rationale.clone());
    // Only human clinical evidence is graded above the floor. A synthesis of
    // animal studies is capped, under its own rule, and the receipt names the
    // rule it would otherwise have met.
    if !human && tier < EvidenceTier::Level5 {
        signals.push(format!("scope:non-human-capped-from:{}", spec.id));
        rule_id = step.non_human_rule_id.clone();
        tier = EvidenceTier::Level5;
        rationale = step.non_human_rationale.clone();
    }
    let mut metadata = BTreeMap::new();
    metadata.insert(step.human_field.clone(), human.to_string());
    for name in &step.flag_fields {
        metadata.insert(name.clone(), flag(name).to_string());
    }
    if let Some(d) = &direction {
        metadata.insert("finding_direction".to_string(), d.clone());
    }
    Ok(Graded {
        study_type,
        tier,
        human,
        rule: json!({
            "rule_id": rule_id,
            "study_type": study_type.slug(),
            "tier": tier.slug(),
            "matched_signals": signals,
            "metadata": metadata,
            "rationale": rationale,
        }),
        direction,
    })
}

/// The item objects, one per selected candidate, in ranking order.
fn build_items(recipe: &PackRecipe, selected: &[Candidate<'_>]) -> Vec<Map<String, Value>> {
    selected
        .iter()
        .map(|candidate| {
            let h = candidate.hit;
            let mut m = Map::new();
            m.insert("corpus".into(), json!(h.corpus));
            m.insert("record_id".into(), json!(h.record_id));
            m.insert("title".into(), json!(h.title));
            m.insert("excerpt".into(), json!(h.excerpt));
            m.insert("pin_sha".into(), json!(h.pin_sha));
            m.insert("fetched_at".into(), json!(h.fetched_at));
            if let Some(cluster_k) = &recipe.ranking.cluster_by {
                if let Some(cv) = hit_field(h, cluster_k) {
                    m.insert("cluster".into(), json!(cv));
                }
            }
            for (fk, fv) in &h.fields {
                if !m.contains_key(fk) {
                    m.insert(fk.clone(), json!(fv));
                }
            }
            if let Some(g) = &candidate.grade {
                m.insert("study_type".into(), json!(g.study_type.slug()));
                m.insert("evidence_tier".into(), json!(g.tier.slug()));
                m.insert("human_clinical".into(), json!(g.human));
                m.insert("grade_rule".into(), g.rule.clone());
                if let Some(d) = &g.direction {
                    m.insert("finding_direction".into(), json!(d));
                }
            }
            m
        })
        .collect()
}

/// Items a `fit_by_dropping` budget never drops: the first match of each
/// `protect_first` partition, or the top-ranked item when none is declared.
fn mandatory_core(recipe: &PackRecipe, selected: &[Candidate<'_>]) -> HashSet<usize> {
    let items = build_items(recipe, selected);
    let mut core = HashSet::new();
    let mut declared = false;
    for part in recipe.partitions.iter().filter(|p| p.protect_first) {
        declared = true;
        if let Some(first) = items.iter().position(|m| {
            op_matches(
                part.op,
                m.get(&part.field).and_then(Value::as_str).unwrap_or(""),
                &part.value,
            )
        }) {
            core.insert(first);
        }
    }
    if !declared && !items.is_empty() {
        core.insert(0);
    }
    core
}

/// Chapter boundaries over `keys`, the group key of each item in sorted order:
/// `count + 1` indices (`0` first, `keys.len()` last). A key's items stay
/// together and boundaries fall between keys, as close to an even split as the
/// keys allow; only fewer keys than chapters splits one (the largest first).
/// The caller has checked `keys.len() >= count`.
fn chapter_bounds(keys: &[String], count: usize) -> Vec<usize> {
    let total = keys.len();
    let mut cuts: Vec<usize> = (1..total).filter(|&i| keys[i] != keys[i - 1]).collect();
    while cuts.len() + 1 < count {
        let mut bounds = vec![0];
        bounds.extend(&cuts);
        bounds.push(total);
        let (start, len) =
            bounds
                .windows(2)
                .map(|w| (w[0], w[1] - w[0]))
                .fold(
                    (0, 0),
                    |best, next| if next.1 > best.1 { next } else { best },
                );
        let cut = start + len / 2;
        let at = cuts.partition_point(|&c| c < cut);
        cuts.insert(at, cut);
    }
    let mut bounds = vec![0];
    let mut next_candidate = 0;
    for k in 1..count {
        let target = (total * k).div_ceil(count);
        let last_allowed = cuts.len() - (count - k);
        let best = (next_candidate..=last_allowed)
            .min_by_key(|&j| (cuts[j].abs_diff(target), j))
            .unwrap_or(next_candidate);
        bounds.push(cuts[best]);
        next_candidate = best + 1;
    }
    bounds.push(total);
    bounds
}

/// The item indices of each chapter, or `None` when more chapters are asked for
/// than there are items.
fn plan_chapters(keys: &[String], count: Option<usize>) -> Option<Vec<Vec<usize>>> {
    if keys.is_empty() {
        return Some(Vec::new());
    }
    let mut order: Vec<usize> = (0..keys.len()).collect();
    order.sort_by(|&a, &b| keys[a].cmp(&keys[b]).then(a.cmp(&b)));
    let sorted: Vec<String> = order.iter().map(|&i| keys[i].clone()).collect();
    let bounds = match count {
        Some(0) => return None,
        Some(n) if n > keys.len() => return None,
        Some(n) => chapter_bounds(&sorted, n),
        None => {
            let mut b = vec![0];
            b.extend((1..sorted.len()).filter(|&i| sorted[i] != sorted[i - 1]));
            b.push(sorted.len());
            b
        }
    };
    Some(
        bounds
            .windows(2)
            .map(|w| order[w[0]..w[1]].to_vec())
            .collect(),
    )
}

/// Tokens of one output value: one per four bytes of its RFC 8785 JSON,
/// rounded up. `"$"` is the whole output, which is what a consumer receives.
fn measured_tokens(output: &Value, target: &str) -> Result<usize> {
    let val = if target == WHOLE_OUTPUT {
        output
    } else {
        output.get(target).unwrap_or(&Value::Null)
    };
    Ok(serde_json_canonicalizer::to_vec(val)?.len().div_ceil(4))
}

/// The recipe's output object for these selected candidates: items, chapters,
/// partitions and the graded sections. A pure function of its inputs, so a
/// `fit_by_dropping` budget can re-assemble it with fewer items.
fn assemble_output(
    recipe: &PackRecipe,
    args: &Map<String, Value>,
    selected: &[Candidate<'_>],
) -> Result<Value> {
    let mut items = build_items(recipe, selected);

    let mut chapters_json: Option<Vec<Value>> = None;
    if let (Some(group), Some(by)) = (&recipe.grouping, recipe.ranking.cluster_by.as_deref()) {
        let keys: Vec<String> = selected
            .iter()
            .map(|c| {
                hit_field(c.hit, by)
                    .map(Cow::into_owned)
                    .unwrap_or_else(|| "default".to_string())
            })
            .collect();
        let count = group.chapters.or_else(|| {
            group.chapters_from_input.as_ref().and_then(|t| {
                args.get(&t.input)
                    .and_then(Value::as_str)
                    .and_then(|v| t.values.get(v))
                    .copied()
            })
        });
        let plan =
            plan_chapters(&keys, count).ok_or_else(|| RecipeError::InsufficientEvidence {
                recipe: recipe.recipe.name.clone(),
                matched: keys.len(),
                min_hits: count.unwrap_or(1),
            })?;
        let mut chapters = Vec::with_capacity(plan.len());
        for (n, members) in plan.iter().enumerate() {
            let chapter_id = format!("chapter-{:02}", n + 1);
            let mut group_keys: Vec<&str> = Vec::new();
            for &i in members {
                if !group_keys.contains(&keys[i].as_str()) {
                    group_keys.push(keys[i].as_str());
                }
                items[i].insert("chapter_id".into(), json!(chapter_id));
            }
            let title = match group_keys.as_slice() {
                [only] if !only.is_empty() => (*only).to_string(),
                [first, rest @ ..] if !first.is_empty() => {
                    format!("{first} (+{} more)", rest.len())
                }
                _ => format!("chapter {}", n + 1),
            };
            let mut chapter = Map::new();
            chapter.insert("chapter_id".into(), json!(chapter_id));
            chapter.insert("title".into(), json!(title));
            chapter.insert("group_keys".into(), json!(group_keys));
            chapter.insert(
                "record_ids".into(),
                json!(members
                    .iter()
                    .map(|&i| selected[i].hit.record_id.as_str())
                    .collect::<Vec<_>>()),
            );
            if let Some(step) = &recipe.grading {
                let clinical =
                    |i: &usize| selected[*i].grade.as_ref().is_some_and(Graded::clinical);
                let ids = |keep: bool| -> Vec<&str> {
                    members
                        .iter()
                        .filter(|i| clinical(i) == keep)
                        .map(|&i| selected[i].hit.record_id.as_str())
                        .collect()
                };
                let (clinical_ids, other_ids) = (ids(true), ids(false));
                if clinical_ids.is_empty() {
                    chapter.insert("abstention".into(), json!(step.abstention));
                }
                chapter.insert("clinical_record_ids".into(), json!(clinical_ids));
                chapter.insert("preclinical_record_ids".into(), json!(other_ids));
            }
            chapters.push(Value::Object(chapter));
        }
        chapters_json = Some(chapters);
    }

    let mut out_map = Map::new();
    out_map.insert("recipe".into(), json!(recipe.recipe.name));
    out_map.insert("pack".into(), json!(recipe.recipe.pack));
    if let Some(notice) = &recipe.recipe.static_notice {
        out_map.insert("notice".into(), json!(notice));
    }
    for (k, v) in args {
        if recipe.output_schema.properties.contains_key(k) && !out_map.contains_key(k) {
            out_map.insert(k.clone(), v.clone());
        }
    }
    if let (Some(group), Some(chapters)) = (&recipe.grouping, chapters_json) {
        out_map.insert(group.output_field.clone(), Value::Array(chapters));
    }
    for part in &recipe.partitions {
        let members: Vec<Value> = items
            .iter()
            .filter(|m| {
                op_matches(
                    part.op,
                    m.get(&part.field).and_then(Value::as_str).unwrap_or(""),
                    &part.value,
                )
            })
            .map(|m| Value::Object(m.clone()))
            .collect();
        out_map.insert(part.output_field.clone(), Value::Array(members));
    }
    if let Some(step) = &recipe.grading {
        let pick = |keep: &dyn Fn(&Graded) -> bool| -> Vec<Value> {
            selected
                .iter()
                .zip(&items)
                .filter(|(c, _)| c.grade.as_ref().is_some_and(keep))
                .map(|(_, m)| Value::Object(m.clone()))
                .collect()
        };
        let clinical = pick(&|g| g.clinical());
        out_map.insert(
            step.non_clinical_field.clone(),
            Value::Array(pick(&|g| !g.clinical())),
        );
        if let Some(field) = &step.contradictory_field {
            out_map.insert(field.clone(), Value::Array(pick(&|g| g.adverse())));
        }
        if let Some(field) = &step.highest_clinical_tier_field {
            let best = selected
                .iter()
                .filter_map(|c| c.grade.as_ref().filter(|g| g.clinical()))
                .map(|g| g.tier)
                .min();
            if let Some(tier) = best {
                out_map.insert(field.clone(), json!(tier.slug()));
            }
        }
        if clinical.is_empty() {
            if let Some(field) = &step.abstention_field {
                out_map.insert(field.clone(), json!(step.abstention));
            }
        }
        out_map.insert(step.clinical_field.clone(), Value::Array(clinical));
    }
    out_map.insert(
        "items".into(),
        Value::Array(items.into_iter().map(Value::Object).collect()),
    );
    Ok(Value::Object(out_map))
}

fn valid_slug(s: &str, max_len: usize) -> bool {
    !s.is_empty()
        && s.len() <= max_len
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn valid_ident(s: &str, max_len: usize) -> bool {
    !s.is_empty()
        && s.len() <= max_len
        && s.as_bytes()[0].is_ascii_alphabetic()
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The study-type grading table `bio.evidence` and `bio.outline` share, as a
/// literal so `concat!` can join it into each recipe.
macro_rules! bio_grading_toml {
    () => {
        r#"
[grading]
study_type_field = "study_type"
human_field = "human_clinical"
flag_fields = ["multicenter", "prospective", "well_powered"]
direction_field = "finding_direction"
non_human_rule_id = "bio.grade.non-human-cap.v1"
non_human_rationale = "Study not of human clinical evidence (for example a meta-analysis of animal studies); capped at level_5"
clinical_field = "clinical_evidence"
non_clinical_field = "preclinical_evidence"
contradictory_field = "contradictory_findings"
highest_clinical_tier_field = "highest_clinical_tier"
abstention_field = "abstention"
abstention = "No qualifying human clinical evidence is available; no clinical efficacy claim is made."

[[grading.rules]]
id = "bio.grade.meta-analysis.v1"
study_type = "meta_analysis"
tier = "level_1a"
rationale = "Quantitative meta-analysis across controlled studies"

[[grading.rules]]
id = "bio.grade.systematic-review.v1"
study_type = "systematic_review"
tier = "level_1a"
rationale = "Systematic review with explicit inclusion criteria"

[[grading.rules]]
id = "bio.grade.rct-powered.v1"
study_type = "randomized_controlled_trial"
any_flags = ["multicenter", "well_powered"]
tier = "level_1b"
rationale = "Multicenter or adequately powered randomized controlled trial"

[[grading.rules]]
id = "bio.grade.rct-single.v1"
study_type = "randomized_controlled_trial"
tier = "level_2a"
rationale = "Single-center or exploratory randomized controlled trial"

[[grading.rules]]
id = "bio.grade.prospective-cohort.v1"
study_type = "cohort_study"
all_flags = ["prospective", "well_powered"]
tier = "level_2a"
rationale = "Prospective, adequately powered observational cohort study"

[[grading.rules]]
id = "bio.grade.cohort.v1"
study_type = "cohort_study"
tier = "level_2b"
rationale = "Observational cohort study"

[[grading.rules]]
id = "bio.grade.case-control.v1"
study_type = "case_control"
tier = "level_3"
rationale = "Retrospective case-control study"

[[grading.rules]]
id = "bio.grade.case-report.v1"
study_type = "case_report"
tier = "level_4"
rationale = "Case report or uncontrolled case series"

[[grading.rules]]
id = "bio.grade.preclinical.v1"
study_type = "preclinical"
tier = "level_5"
rationale = "Preclinical animal model study; non-clinical evidence"

[[grading.rules]]
id = "bio.grade.in-vitro.v1"
study_type = "in_vitro"
tier = "level_5"
rationale = "In-vitro bench or mechanistic assay; non-clinical evidence"

[[grading.rules]]
id = "bio.grade.expert-opinion.v1"
study_type = "expert_opinion"
tier = "level_5"
rationale = "Expert opinion or narrative review without primary data; non-clinical evidence"
"#
    };
}

/// `write.outline`: group cited evidence by theme into 1, 3 or 5 chapters, and
/// refuse rather than pad when there are fewer sources than chapters.
pub const BUILTIN_WRITE_OUTLINE_RECIPE_TOML: &str = r#"
[recipe]
format_version = "xerj-recipe/v1"
pack = "writing"
name = "write.outline"
mcp_tool_name = "xerj_write_outline"
description = "Group cited hub evidence by theme into a chapter outline. Refuses, instead of padding, when there are fewer cited sources than chapters."
requires_scope = false

[[inputs]]
name = "topic"
kind = "string"
required = true
description = "Topic or thesis to outline."
min_len = 2
max_len = 256

[[inputs]]
name = "audience"
kind = "string"
required = true
description = "Target audience level."
enum_values = ["general", "technical", "executive"]

[[inputs]]
name = "length"
kind = "string"
required = true
description = "Outline length: 1, 3 or 5 chapters."
enum_values = ["brief", "standard", "comprehensive"]

[corpora]
required = ["agentic-security-ietf-corpus"]
optional = []
require_redistribution_licence = true
min_hits = 1

[corpora.min_evidence_hits_by_input]
input = "length"

[corpora.min_evidence_hits_by_input.values]
brief = 1
standard = 3
comprehensive = 5

[[filters]]
field = "excerpt"
op = "contains"
from_input = "topic"

[ranking]
strategy = "score_desc"
top_k = 40
cluster_by = "theme_key"

[grouping]
output_field = "chapters"

[grouping.chapters_from_input]
input = "length"

[grouping.chapters_from_input.values]
brief = 1
standard = 3
comprehensive = 5

[output_schema]
required_fields = ["recipe", "pack", "topic", "audience", "length", "chapters", "items"]

[output_schema.properties.recipe]
kind = "string"
non_empty = true

[output_schema.properties.pack]
kind = "string"
non_empty = true

[output_schema.properties.topic]
kind = "string"
non_empty = true

[output_schema.properties.audience]
kind = "string"
non_empty = true

[output_schema.properties.length]
kind = "string"
non_empty = true

[output_schema.properties.chapters]
kind = "array"
non_empty = true
item_required_fields = ["chapter_id", "title", "record_ids"]

[output_schema.properties.items]
kind = "array"
non_empty = true
item_required_fields = ["corpus", "record_id", "title", "excerpt", "pin_sha", "fetched_at", "chapter_id"]

[[checks]]
id = "citations-present"
kind = "require_citations"
target_field = "items"

[[checks]]
id = "no-slop"
kind = "forbid_phrases"
target_field = "items"
phrases = ["TODO", "TBD", "as an AI language model"]

[[checks]]
id = "token-budget"
kind = "max_token_budget"
target_field = "$"
max_tokens = 16384
"#;

/// `write.section`: one chapter's cited facts and counterpoints, bounded in
/// tokens measured on the serialized packet.
pub const BUILTIN_WRITE_SECTION_RECIPE_TOML: &str = r#"
[recipe]
format_version = "xerj-recipe/v1"
pack = "writing"
name = "write.section"
mcp_tool_name = "xerj_write_section"
description = "Resolve one outline chapter into cited facts and counterpoints, within a token budget measured on the serialized packet."
requires_scope = false
static_notice = "Ground every factual sentence in the cited sources. Cite each claim with [source:<record_id>] and keep its provenance. Present counterpoints as scope limits, never as omitted evidence."

[[inputs]]
name = "outline_id"
kind = "string"
required = true
description = "The outline the chapter belongs to."
min_len = 8
max_len = 64

[[inputs]]
name = "chapter"
kind = "string"
required = true
description = "Chapter id (chapter-01) to resolve."
min_len = 1
max_len = 128

[[inputs]]
name = "token_budget"
kind = "integer"
required = true
description = "Ceiling for the whole serialized packet, in tokens (RFC 8785 JSON bytes divided by four, rounded up)."

[corpora]
required = ["agentic-security-ietf-corpus"]
optional = []
require_redistribution_licence = true
min_hits = 1
min_evidence_hits = 1

[[filters]]
field = "outline_id"
op = "equals"
from_input = "outline_id"

[[filters]]
field = "chapter"
op = "equals"
from_input = "chapter"

[ranking]
strategy = "score_desc"
top_k = 60

[[partitions]]
output_field = "facts"
field = "stance"
op = "not_equals"
value = "counterpoint"
protect_first = true

[[partitions]]
output_field = "counterpoints"
field = "stance"
op = "equals"
value = "counterpoint"
protect_first = true

[output_schema]
required_fields = ["recipe", "pack", "outline_id", "chapter", "token_budget", "notice", "items", "facts", "counterpoints"]

[output_schema.properties.recipe]
kind = "string"
non_empty = true

[output_schema.properties.pack]
kind = "string"
non_empty = true

[output_schema.properties.outline_id]
kind = "string"
non_empty = true

[output_schema.properties.chapter]
kind = "string"
non_empty = true

[output_schema.properties.token_budget]
kind = "integer"

[output_schema.properties.notice]
kind = "string"
non_empty = true

[output_schema.properties.items]
kind = "array"
non_empty = true
item_required_fields = ["corpus", "record_id", "title", "excerpt", "pin_sha", "fetched_at"]

[output_schema.properties.facts]
kind = "array"

[output_schema.properties.counterpoints]
kind = "array"

[[checks]]
id = "citations-present"
kind = "require_citations"
target_field = "items"

[[checks]]
id = "no-slop"
kind = "forbid_phrases"
target_field = "items"
phrases = ["TODO", "TBD", "as an AI language model"]

[[checks]]
id = "token-budget"
kind = "max_token_budget"
target_field = "$"
max_tokens_from_input = "token_budget"
fit_by_dropping = true
"#;

/// `bio.evidence`: study-type grading with a receipt per item, the clinical /
/// non-clinical split, contradictory findings, and the not-medical-advice
/// notice.
pub const BUILTIN_BIO_EVIDENCE_RECIPE_TOML: &str = concat!(
    r#"
[recipe]
format_version = "xerj-recipe/v1"
pack = "bio"
name = "bio.evidence"
mcp_tool_name = "xerj_bio_evidence"
description = "Grade biomedical corpus evidence by study type, separate human clinical evidence from the rest, and surface contradictory findings. Literature synthesis only."
requires_scope = false
static_notice = "Literature synthesis from cited corpora only. This is not medical advice and is not a clinical diagnosis; consult appropriately qualified clinicians."

[[inputs]]
name = "condition"
kind = "string"
required = true
description = "Condition or intervention query."
min_len = 2
max_len = 256

[corpora]
required = ["open-biomed-trials"]
optional = []
require_redistribution_licence = true
min_hits = 1
min_evidence_hits = 1

[[filters]]
field = "excerpt"
op = "contains"
from_input = "condition"

[ranking]
strategy = "study_grade_asc"
top_k = 10
"#,
    bio_grading_toml!(),
    r#"
[output_schema]
required_fields = ["recipe", "pack", "condition", "notice", "items", "clinical_evidence", "preclinical_evidence", "contradictory_findings"]

[output_schema.properties.recipe]
kind = "string"
non_empty = true

[output_schema.properties.pack]
kind = "string"
non_empty = true

[output_schema.properties.condition]
kind = "string"
non_empty = true

[output_schema.properties.notice]
kind = "string"
non_empty = true

[output_schema.properties.items]
kind = "array"
non_empty = true
item_required_fields = ["corpus", "record_id", "title", "excerpt", "pin_sha", "fetched_at", "study_type", "evidence_tier", "grade_rule"]

[output_schema.properties.clinical_evidence]
kind = "array"

[output_schema.properties.preclinical_evidence]
kind = "array"

[output_schema.properties.contradictory_findings]
kind = "array"

[output_schema.properties.highest_clinical_tier]
kind = "string"

[output_schema.properties.abstention]
kind = "string"

[[checks]]
id = "citations-present"
kind = "require_citations"
target_field = "items"

[[checks]]
id = "notice-literature-synthesis"
kind = "require_notice"
target_field = "notice"
expected_substring = "Literature synthesis"

[[checks]]
id = "medical-disclaimer"
kind = "require_notice"
target_field = "notice"
expected_substring = "not medical advice"

[[checks]]
id = "notice-clinical-diagnosis"
kind = "require_notice"
target_field = "notice"
expected_substring = "clinical diagnosis"
"#
);

/// `bio.outline`: `bio.evidence` grading, grouped into the chapters the caller's
/// records name (the hit field `chapter`), each with its own clinical /
/// non-clinical split and abstention.
pub const BUILTIN_BIO_OUTLINE_RECIPE_TOML: &str = concat!(
    r#"
[recipe]
format_version = "xerj-recipe/v1"
pack = "bio"
name = "bio.outline"
mcp_tool_name = "xerj_bio_outline"
description = "Outline a biomedical topic into chapters of graded, cited evidence, each separating human clinical evidence from the rest. Literature synthesis only."
requires_scope = false
static_notice = "Literature synthesis from cited corpora only. This is not medical advice and is not a clinical diagnosis; consult appropriately qualified clinicians."

[[inputs]]
name = "topic"
kind = "string"
required = true
description = "Research question or topic to outline."
min_len = 2
max_len = 256

[corpora]
required = ["open-biomed-trials"]
optional = []
require_redistribution_licence = true
min_hits = 1
min_evidence_hits = 1

[[filters]]
field = "excerpt"
op = "contains"
from_input = "topic"

[ranking]
strategy = "study_grade_asc"
top_k = 40
cluster_by = "chapter"

[grouping]
output_field = "chapters"
"#,
    bio_grading_toml!(),
    r#"
[output_schema]
required_fields = ["recipe", "pack", "topic", "notice", "chapters", "items", "clinical_evidence", "preclinical_evidence", "contradictory_findings"]

[output_schema.properties.recipe]
kind = "string"
non_empty = true

[output_schema.properties.pack]
kind = "string"
non_empty = true

[output_schema.properties.topic]
kind = "string"
non_empty = true

[output_schema.properties.notice]
kind = "string"
non_empty = true

[output_schema.properties.chapters]
kind = "array"
non_empty = true
item_required_fields = ["chapter_id", "title", "record_ids", "clinical_record_ids", "preclinical_record_ids"]

[output_schema.properties.items]
kind = "array"
non_empty = true
item_required_fields = ["corpus", "record_id", "title", "excerpt", "pin_sha", "fetched_at", "study_type", "evidence_tier", "grade_rule", "chapter_id"]

[output_schema.properties.clinical_evidence]
kind = "array"

[output_schema.properties.preclinical_evidence]
kind = "array"

[output_schema.properties.contradictory_findings]
kind = "array"

[output_schema.properties.highest_clinical_tier]
kind = "string"

[output_schema.properties.abstention]
kind = "string"

[[checks]]
id = "citations-present"
kind = "require_citations"
target_field = "items"

[[checks]]
id = "notice-literature-synthesis"
kind = "require_notice"
target_field = "notice"
expected_substring = "Literature synthesis"

[[checks]]
id = "medical-disclaimer"
kind = "require_notice"
target_field = "notice"
expected_substring = "not medical advice"

[[checks]]
id = "notice-clinical-diagnosis"
kind = "require_notice"
target_field = "notice"
expected_substring = "clinical diagnosis"
"#
);

/// A SAMPLE recipe for the Cyber pack (`cyber.target_surface`). The cyber pack
/// (#2767) is not on main, so this tool does not exist: the recipe shows the
/// format (a `requires_scope` recipe under a `ScopeGuard`) and is excluded from
/// [`builtin_recipes`]. [`sample_recipes`] returns it.
pub const SAMPLE_CYBER_SURFACE_RECIPE_TOML: &str = r#"
[recipe]
format_version = "xerj-recipe/v1"
pack = "cyber"
name = "cyber.target_surface"
mcp_tool_name = "xerj_cyber_target_surface"
description = "Match authorized target surface patterns from vulnerability corpora under a ScopeGuard."
requires_scope = true
sample = true

[[inputs]]
name = "target"
kind = "string"
required = true
description = "Authorized bare hostname or IP literal."
min_len = 2
max_len = 253

[[inputs]]
name = "cwe"
kind = "string"
required = true
description = "CWE identifier to filter on (e.g. CWE-918)."
min_len = 5
max_len = 16

[corpora]
required = ["rust-vulns"]
optional = []
require_redistribution_licence = true
min_hits = 1

[[filters]]
field = "cwe"
op = "equals"
from_input = "cwe"

[ranking]
strategy = "recency_desc"
top_k = 10

[output_schema]
required_fields = ["recipe", "pack", "target", "cwe", "items"]

[output_schema.properties.recipe]
kind = "string"
non_empty = true

[output_schema.properties.pack]
kind = "string"
non_empty = true

[output_schema.properties.target]
kind = "string"
non_empty = true

[output_schema.properties.cwe]
kind = "string"
non_empty = true
pattern_prefix = "CWE-"

[output_schema.properties.items]
kind = "array"
non_empty = true
item_required_fields = ["corpus", "record_id", "title", "excerpt", "pin_sha", "fetched_at", "cwe"]

[[checks]]
id = "citations-present"
kind = "require_citations"
target_field = "items"
"#;

/// Load and validate the built-in pack recipes: `write.outline`,
/// `write.section`, `bio.outline` and `bio.evidence`. Sample recipes
/// ([`sample_recipes`]) are not included.
pub fn builtin_recipes() -> Result<Vec<PackRecipe>> {
    Ok(vec![
        PackRecipe::from_toml_str(BUILTIN_WRITE_OUTLINE_RECIPE_TOML)?,
        PackRecipe::from_toml_str(BUILTIN_WRITE_SECTION_RECIPE_TOML)?,
        PackRecipe::from_toml_str(BUILTIN_BIO_OUTLINE_RECIPE_TOML)?,
        PackRecipe::from_toml_str(BUILTIN_BIO_EVIDENCE_RECIPE_TOML)?,
    ])
}

/// Sample recipes for packs whose tools are not served yet
/// (`cyber.target_surface`, until #2767 lands). Their MCP descriptions say so,
/// and `xerj-mcp`'s `RecipeRegistry` refuses to register them.
pub fn sample_recipes() -> Result<Vec<PackRecipe>> {
    Ok(vec![PackRecipe::from_toml_str(
        SAMPLE_CYBER_SURFACE_RECIPE_TOML,
    )?])
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const IETF: &str = "agentic-security-ietf-corpus";
    const BIOMED: &str = "open-biomed-trials";

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 10, 12, 0, 0).unwrap()
    }

    fn hit(
        corpus: &str,
        record_id: &str,
        title: &str,
        excerpt: &str,
        score: f64,
        fields: &[(&str, &str)],
    ) -> RecipeCorpusHit {
        RecipeCorpusHit {
            corpus: corpus.into(),
            licence_class: LicenceClass::AdaptWithAttribution,
            record_id: record_id.into(),
            title: title.into(),
            excerpt: excerpt.into(),
            score,
            published_date: "2026-11-01".into(),
            study_grade_rank: None,
            pin_sha: "a".repeat(40),
            fetched_at: "2026-11-05".into(),
            fields: fields
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            pinned_citation: None,
        }
    }

    fn recipe(toml: &str) -> PackRecipe {
        PackRecipe::from_toml_str(toml).unwrap()
    }

    fn run(
        recipe: &PackRecipe,
        args: Value,
        hits: &[RecipeCorpusHit],
    ) -> Result<RecipeExecutionReport> {
        RecipeRunner::execute(
            recipe,
            &args,
            hits,
            None,
            InputOrigin::AuthorizedControlPlane,
            now(),
        )
    }

    fn ids(value: &Value) -> Vec<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["record_id"].as_str().unwrap().to_string())
            .collect()
    }

    fn outline_hits() -> Vec<RecipeCorpusHit> {
        [
            (
                "r1",
                "Agentic control planes bind every tool action to a receipt.",
                0.95,
            ),
            (
                "r2",
                "Agentic control planes bind actions to an identity.",
                0.94,
            ),
            ("r3", "Agentic rule 1 limits delegation depth.", 0.93),
            ("r4", "Agentic rule 2 limits delegation depth.", 0.92),
            ("r5", "Agentic receipts are signed and verifiable.", 0.91),
        ]
        .into_iter()
        .map(|(id, excerpt, score)| hit(IETF, id, "Draft", excerpt, score, &[]))
        .collect()
    }

    #[test]
    fn builtin_recipes_are_the_four_served_tools_and_the_cyber_sample_is_marked() {
        let recipes = builtin_recipes().unwrap();
        let names: Vec<_> = recipes.iter().map(|r| r.recipe.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "write.outline",
                "write.section",
                "bio.outline",
                "bio.evidence"
            ]
        );
        let tools: Vec<Value> = recipes.iter().map(PackRecipe::to_mcp_tool_spec).collect();
        let tool_names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            tool_names,
            [
                "xerj_write_outline",
                "xerj_write_section",
                "xerj_bio_outline",
                "xerj_bio_evidence"
            ]
        );
        assert!(recipes.iter().all(|r| !r.recipe.sample));
        assert!(tools
            .iter()
            .all(|t| !t["description"].as_str().unwrap().contains("SAMPLE")));

        let samples = sample_recipes().unwrap();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].recipe.name, "cyber.target_surface");
        assert!(samples[0].recipe.sample);
        assert!(samples[0].to_mcp_tool_spec()["description"]
            .as_str()
            .unwrap()
            .starts_with("[SAMPLE recipe, not a served tool]"));
        assert!(recipes
            .iter()
            .all(|r| r.recipe.name != samples[0].recipe.name));
    }

    #[test]
    fn the_cyber_sample_still_runs_under_a_scope_guard() {
        let sample = &sample_recipes().unwrap()[0];
        let hits = [hit(
            "rust-vulns",
            "RUSTSEC-2026-0099",
            "Redirect host allowlist bypass",
            "Unchecked 302 redirect reaches internal metadata endpoint.",
            0.94,
            &[("cwe", "CWE-918")],
        )];
        let args = json!({"target": "api.example.com", "cwe": "CWE-918"});
        assert!(matches!(
            run(sample, args.clone(), &hits),
            Err(RecipeError::ScopeRequired { .. })
        ));
        let guard = ScopeGuard::from_work_order(
            Some(WorkOrder {
                id: "wo-recipe-1".into(),
                authorization: "Signed ROE".into(),
                targets: vec!["api.example.com".into()],
                not_before: Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap(),
                expires_at: Utc.with_ymd_and_hms(2026, 11, 20, 0, 0, 0).unwrap(),
            }),
            InputOrigin::AuthorizedControlPlane,
        )
        .unwrap();
        let report = RecipeRunner::execute(
            sample,
            &args,
            &hits,
            Some(&guard),
            InputOrigin::AuthorizedControlPlane,
            now(),
        )
        .unwrap();
        assert!(report.scope.is_some());
        report.verify(sample).unwrap();
    }

    /// Short tokens and digits are part of the theme key, so two records that
    /// differ only by a number are different themes.
    #[test]
    fn the_derived_theme_key_keeps_short_tokens_and_digits() {
        let one = hit(IETF, "a", "T", "Rule 1 of the set.", 1.0, &[]);
        let two = hit(IETF, "b", "T", "Rule 2 of the set.", 1.0, &[]);
        assert_eq!(theme_key(&one), "rule 1 of the");
        assert_ne!(theme_key(&one), theme_key(&two));
        assert_eq!(
            hit_field(&one, "theme_key").unwrap().as_ref(),
            "rule 1 of the"
        );
        let explicit = hit(IETF, "c", "T", "x", 1.0, &[("theme_key", "given")]);
        assert_eq!(hit_field(&explicit, "theme_key").unwrap().as_ref(), "given");
    }

    #[test]
    fn write_outline_groups_by_theme_keeps_themes_whole_and_refuses_to_pad() {
        let outline = recipe(BUILTIN_WRITE_OUTLINE_RECIPE_TOML);
        let args =
            |length: &str| json!({"topic": "agentic", "audience": "technical", "length": length});

        let standard = run(&outline, args("standard"), &outline_hits()).unwrap();
        standard.verify(&outline).unwrap();
        let chapters = standard.output["chapters"].as_array().unwrap();
        assert_eq!(chapters.len(), 3);
        // Both "control planes" records share a theme and a chapter; the "rule
        // 1" and "rule 2" records are different themes.
        let chapter_of = |id: &str| {
            standard.output["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["record_id"] == id)
                .unwrap()["chapter_id"]
                .clone()
        };
        assert_eq!(chapter_of("r1"), chapter_of("r2"));
        assert_ne!(chapter_of("r3"), chapter_of("r4"));
        let mut all: Vec<String> = chapters
            .iter()
            .flat_map(|c| c["record_ids"].as_array().unwrap().iter())
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        all.sort();
        assert_eq!(
            all,
            ["r1", "r2", "r3", "r4", "r5"],
            "every record in one chapter"
        );

        let brief = run(&outline, args("brief"), &outline_hits()).unwrap();
        assert_eq!(brief.output["chapters"].as_array().unwrap().len(), 1);

        // Fewer cited sources than chapters: a refusal, not padded chapters.
        let err = run(&outline, args("comprehensive"), &outline_hits()[..4]).unwrap_err();
        assert!(
            matches!(
                err,
                RecipeError::InsufficientEvidence {
                    matched: 4,
                    min_hits: 5,
                    ..
                }
            ),
            "{err}"
        );
        let err = run(&outline, args("standard"), &outline_hits()[..2]).unwrap_err();
        assert!(matches!(
            err,
            RecipeError::InsufficientEvidence {
                matched: 2,
                min_hits: 3,
                ..
            }
        ));
    }

    #[test]
    fn a_theme_is_split_only_when_there_are_fewer_themes_than_chapters() {
        let one_theme: Vec<String> = vec!["same".to_string(); 5];
        let plan = plan_chapters(&one_theme, Some(5)).unwrap();
        assert!(plan.iter().all(|c| c.len() == 1));
        let mixed: Vec<String> = ["a", "a", "a", "a", "b"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let plan = plan_chapters(&mixed, Some(3)).unwrap();
        assert_eq!(plan.len(), 3);
        assert!(plan.iter().all(|c| !c.is_empty()));
        assert_eq!(plan.iter().map(Vec::len).sum::<usize>(), 5);
        assert!(plan_chapters(&mixed, Some(6)).is_none());
        // With no count, every key is a chapter.
        let by_key = plan_chapters(&mixed, None).unwrap();
        assert_eq!(by_key.iter().map(Vec::len).collect::<Vec<_>>(), [4, 1]);
    }

    fn section_hits() -> Vec<RecipeCorpusHit> {
        let mut hits: Vec<RecipeCorpusHit> = (0..6)
            .map(|n| {
                hit(
                    IETF,
                    &format!("support-{n}"),
                    "Draft",
                    &format!("Supporting evidence number {n} about bounded delegation."),
                    0.9 - f64::from(n) / 100.0,
                    &[
                        ("outline_id", "outline-0123abcd"),
                        ("chapter", "chapter-01"),
                        ("stance", "supporting"),
                    ],
                )
            })
            .collect();
        for (n, score) in [(0, 0.5), (1, 0.4)] {
            hits.push(hit(
                IETF,
                &format!("counter-{n}"),
                "Draft",
                &format!("A limit {n} on bounded delegation."),
                score,
                &[
                    ("outline_id", "outline-0123abcd"),
                    ("chapter", "chapter-01"),
                    ("stance", "counterpoint"),
                ],
            ));
        }
        // Another chapter and another outline: filtered out.
        hits.push(hit(
            IETF,
            "other-chapter",
            "Draft",
            "Elsewhere.",
            0.99,
            &[
                ("outline_id", "outline-0123abcd"),
                ("chapter", "chapter-02"),
                ("stance", "supporting"),
            ],
        ));
        hits.push(hit(
            IETF,
            "other-outline",
            "Draft",
            "Elsewhere.",
            0.99,
            &[
                ("outline_id", "outline-ffff0000"),
                ("chapter", "chapter-01"),
                ("stance", "supporting"),
            ],
        ));
        hits
    }

    fn section(budget: u64) -> Result<RecipeExecutionReport> {
        run(
            &recipe(BUILTIN_WRITE_SECTION_RECIPE_TOML),
            json!({"outline_id": "outline-0123abcd", "chapter": "chapter-01", "token_budget": budget}),
            &section_hits(),
        )
    }

    /// The budget is measured on the serialized output, and the lowest-ranked
    /// items go first: the mandatory core (the best fact and the best
    /// counterpoint) is never dropped.
    #[test]
    fn write_section_fits_its_token_budget_on_the_serialized_packet() {
        let section_recipe = recipe(BUILTIN_WRITE_SECTION_RECIPE_TOML);
        let full = section(1_000_000).unwrap();
        full.verify(&section_recipe).unwrap();
        assert_eq!(
            ids(&full.output["items"]).len(),
            8,
            "other chapter and outline filtered"
        );
        assert_eq!(ids(&full.output["facts"]).len(), 6);
        assert_eq!(
            ids(&full.output["counterpoints"]),
            ["counter-0", "counter-1"]
        );
        let full_tokens = measured_tokens(&full.output, "$").unwrap();

        // The smallest budget that still succeeds holds only the core.
        let (mut lo, mut hi) = (1u64, 1_000_000u64);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if section(mid).is_ok() {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        let core = section(lo).unwrap();
        assert_eq!(ids(&core.output["facts"]), ["support-0"]);
        assert_eq!(ids(&core.output["counterpoints"]), ["counter-0"]);
        assert!(measured_tokens(&core.output, "$").unwrap() <= usize::try_from(lo).unwrap());
        let err = section(lo - 1).unwrap_err();
        assert!(matches!(err, RecipeError::CheckFailed { .. }), "{err}");

        let middle = u64::try_from(full_tokens).unwrap() * 3 / 4;
        let partial = section(middle).unwrap();
        let kept = ids(&partial.output["items"]).len();
        assert!((2..8).contains(&kept), "{kept}");
        assert!(measured_tokens(&partial.output, "$").unwrap() <= usize::try_from(middle).unwrap());
        // Dropped from the tail: what is kept is the best-ranked prefix.
        assert_eq!(ids(&partial.output["facts"])[0], "support-0");
        partial.verify(&section_recipe).unwrap();
        let weaker = section_recipe.run_checks(&full.output).unwrap();
        assert_eq!(weaker.last().unwrap().check_id, "token-budget");
    }

    #[test]
    fn a_counterpoint_only_chapter_lists_each_source_once() {
        let only: Vec<RecipeCorpusHit> = section_hits()
            .into_iter()
            .filter(|h| h.record_id.starts_with("counter-"))
            .collect();
        let report = run(
            &recipe(BUILTIN_WRITE_SECTION_RECIPE_TOML),
            json!({"outline_id": "outline-0123abcd", "chapter": "chapter-01", "token_budget": 1_000_000}),
            &only,
        )
        .unwrap();
        assert!(report.output["facts"].as_array().unwrap().is_empty());
        assert_eq!(
            ids(&report.output["counterpoints"]),
            ["counter-0", "counter-1"]
        );
        let mut seen: Vec<String> = ids(&report.output["facts"]);
        seen.extend(ids(&report.output["counterpoints"]));
        let unique: HashSet<_> = seen.iter().collect();
        assert_eq!(unique.len(), seen.len(), "no source in two partitions");
    }

    fn trial(
        id: &str,
        study_type: &str,
        human: &str,
        extra: &[(&str, &str)],
        score: f64,
    ) -> RecipeCorpusHit {
        let mut fields = vec![("study_type", study_type), ("human_clinical", human)];
        fields.extend_from_slice(extra);
        hit(
            BIOMED,
            id,
            "Study",
            &format!("A glycemic response study ({id})."),
            score,
            &fields,
        )
    }

    fn bio_hits() -> Vec<RecipeCorpusHit> {
        vec![
            trial("h-meta", "meta_analysis", "true", &[], 0.5),
            trial(
                "h-rct",
                "randomized_controlled_trial",
                "true",
                &[("multicenter", "true"), ("finding_direction", "adverse")],
                0.9,
            ),
            trial("h-animal-meta", "meta_analysis", "false", &[], 0.99),
            trial("h-opinion", "expert_opinion", "true", &[], 0.8),
            trial("h-bench", "in_vitro", "false", &[], 0.7),
        ]
    }

    fn graded<'a>(items: &'a Value, id: &str) -> &'a Value {
        items
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["record_id"] == id)
            .unwrap()
    }

    #[test]
    fn bio_evidence_grades_with_a_receipt_and_never_ranks_a_non_human_study_above_level_5() {
        let bio = recipe(BUILTIN_BIO_EVIDENCE_RECIPE_TOML);
        let report = run(&bio, json!({"condition": "glycemic"}), &bio_hits()).unwrap();
        report.verify(&bio).unwrap();
        let out = &report.output;

        // Tier order, not score order: the strongest design first.
        assert_eq!(ids(&out["clinical_evidence"]), ["h-meta", "h-rct"]);
        assert_eq!(out["highest_clinical_tier"], "level_1a");
        assert_eq!(
            ids(&out["preclinical_evidence"]).len(),
            3,
            "{}",
            out["preclinical_evidence"]
        );
        assert!(out.get("abstention").is_none());
        assert_eq!(ids(&out["contradictory_findings"]), ["h-rct"]);

        // The receipt on each item.
        let meta = graded(&out["items"], "h-meta");
        assert_eq!(meta["grade_rule"]["rule_id"], "bio.grade.meta-analysis.v1");
        assert_eq!(meta["evidence_tier"], "level_1a");
        let rct = graded(&out["items"], "h-rct");
        assert_eq!(rct["grade_rule"]["rule_id"], "bio.grade.rct-powered.v1");
        assert_eq!(rct["grade_rule"]["tier"], "level_1b");
        assert_eq!(rct["grade_rule"]["metadata"]["multicenter"], "true");

        // A meta-analysis of animal studies is capped, under its own rule, and
        // the receipt names the rule it would otherwise have met.
        let animal = graded(&out["items"], "h-animal-meta");
        assert_eq!(animal["evidence_tier"], "level_5");
        assert_eq!(
            animal["grade_rule"]["rule_id"],
            "bio.grade.non-human-cap.v1"
        );
        assert!(animal["grade_rule"]["matched_signals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == "scope:non-human-capped-from:bio.grade.meta-analysis.v1"));
        assert!(ids(&out["preclinical_evidence"]).contains(&"h-animal-meta".to_string()));

        // Expert opinion has its own rule and is never clinical evidence, even
        // for human subjects.
        let opinion = graded(&out["items"], "h-opinion");
        assert_eq!(
            opinion["grade_rule"]["rule_id"],
            "bio.grade.expert-opinion.v1"
        );
        assert_eq!(opinion["evidence_tier"], "level_5");
        assert!(ids(&out["preclinical_evidence"]).contains(&"h-opinion".to_string()));
        assert!(out["notice"]
            .as_str()
            .unwrap()
            .contains("not medical advice"));
    }

    #[test]
    fn bio_evidence_abstains_without_human_clinical_evidence_and_rejects_unknown_designs() {
        let bio = recipe(BUILTIN_BIO_EVIDENCE_RECIPE_TOML);
        let only_non_clinical: Vec<_> = bio_hits()
            .into_iter()
            .filter(|h| ["h-animal-meta", "h-opinion", "h-bench"].contains(&h.record_id.as_str()))
            .collect();
        let report = run(&bio, json!({"condition": "glycemic"}), &only_non_clinical).unwrap();
        assert!(report.output["clinical_evidence"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(report.output["abstention"]
            .as_str()
            .unwrap()
            .contains("no clinical efficacy claim"));
        assert!(report.output.get("highest_clinical_tier").is_none());

        // `expert-opinion-or-review` is not a study type: no alias maps it to
        // another design.
        for bad in [
            "expert-opinion-or-review",
            "preclinical-or-in-vitro",
            "mystery",
        ] {
            let hits = [trial("h-bad", bad, "true", &[], 0.9)];
            let err = run(&bio, json!({"condition": "glycemic"}), &hits).unwrap_err();
            assert!(
                matches!(err, RecipeError::InvalidEvidence { .. }),
                "{bad}: {err}"
            );
        }
        let missing = [hit(
            BIOMED,
            "h-none",
            "Study",
            "A glycemic study.",
            0.9,
            &[],
        )];
        assert!(matches!(
            run(&bio, json!({"condition": "glycemic"}), &missing),
            Err(RecipeError::InvalidEvidence { .. })
        ));
    }

    #[test]
    fn bio_outline_groups_by_chapter_and_separates_clinical_evidence_in_each() {
        let outline = recipe(BUILTIN_BIO_OUTLINE_RECIPE_TOML);
        let with_chapter = |mut h: RecipeCorpusHit, chapter: &str| {
            h.fields.insert("chapter".into(), chapter.into());
            h
        };
        let hits = vec![
            with_chapter(
                trial("c-meta", "meta_analysis", "true", &[], 0.9),
                "1 human evidence",
            ),
            with_chapter(
                trial("c-animal", "preclinical", "false", &[], 0.8),
                "2 animal evidence",
            ),
            with_chapter(
                trial("c-bench", "in_vitro", "false", &[], 0.7),
                "2 animal evidence",
            ),
        ];
        let report = run(&outline, json!({"topic": "glycemic"}), &hits).unwrap();
        report.verify(&outline).unwrap();
        let chapters = report.output["chapters"].as_array().unwrap();
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[0]["title"], "1 human evidence");
        assert_eq!(chapters[0]["clinical_record_ids"], json!(["c-meta"]));
        assert!(chapters[0].get("abstention").is_none());
        assert_eq!(chapters[1]["clinical_record_ids"], json!([]));
        assert!(chapters[1]["abstention"]
            .as_str()
            .unwrap()
            .contains("No qualifying"));
        assert_eq!(
            chapters[1]["preclinical_record_ids"],
            json!(["c-animal", "c-bench"])
        );
        assert!(graded(&report.output["items"], "c-meta")["chapter_id"] == "chapter-01");
    }

    #[test]
    fn a_max_token_budget_is_measured_on_jcs_bytes_not_a_loose_estimate() {
        let outline = recipe(BUILTIN_WRITE_OUTLINE_RECIPE_TOML);
        let args = json!({"topic": "agentic", "audience": "technical", "length": "brief"});
        let report = run(&outline, args.clone(), &outline_hits()).unwrap();
        let bytes = serde_json_canonicalizer::to_vec(&report.output).unwrap();
        let measured = bytes.len().div_ceil(4);
        assert_eq!(measured_tokens(&report.output, "$").unwrap(), measured);
        let detail = &report.checks.last().unwrap().detail;
        assert_eq!(detail, &format!("measured {measured} tokens <= 16384"));

        // One token under what the output measures fails, naming the figure.
        let mut tight = outline.clone();
        tight.checks.last_mut().unwrap().max_tokens = Some(measured - 1);
        let err = run(&tight, args, &outline_hits()).unwrap_err();
        match err {
            RecipeError::CheckFailed { reason, .. } => {
                assert!(
                    reason.contains(&format!("measures {measured} tokens")),
                    "{reason}"
                )
            }
            other => panic!("{other}"),
        }
    }

    #[test]
    fn invalid_recipes_fail_with_precise_errors() {
        let expect = |toml: String, needle: &str| {
            let err = PackRecipe::from_toml_str(&toml).unwrap_err().to_string();
            assert!(err.contains(needle), "{needle}: {err}");
        };
        let outline = BUILTIN_WRITE_OUTLINE_RECIPE_TOML;
        expect(outline.replace("xerj-recipe/v1", "v0"), "format_version");
        expect(
            outline.replace("from_input = \"topic\"", "from_input = \"ghost\""),
            "unknown input",
        );
        expect(
            outline.replace(
                "required_fields = [\"recipe\", \"pack\", \"topic\", \"audience\", \"length\", \"chapters\", \"items\"]",
                "required_fields = [\"recipe\", \"missing_field\"]",
            ),
            "undeclared property",
        );
        // A floor table needs a positive row for every enum value.
        expect(
            outline.replace(
                "comprehensive = 5\n\n[[filters]]",
                "comprehensive = 0\n\n[[filters]]",
            ),
            "no positive entry",
        );
        expect(
            outline.replacen("standard = 3", "standard = 3\nextra = 9", 1),
            "not a value of",
        );
        // The grading table must cover every study type, expert opinion included.
        expect(
            BUILTIN_BIO_EVIDENCE_RECIPE_TOML.replace(
                "study_type = \"expert_opinion\"",
                "study_type = \"in_vitro\"",
            ),
            "no row for study type \"expert_opinion\"",
        );
        // Grouping groups by `ranking.cluster_by`.
        expect(
            outline.replace("cluster_by = \"theme_key\"\n", ""),
            "cluster_by",
        );
        // A budget from an input must be a required integer the output echoes.
        expect(
            BUILTIN_WRITE_SECTION_RECIPE_TOML.replace(
                "max_tokens_from_input = \"token_budget\"",
                "max_tokens_from_input = \"chapter\"",
            ),
            "max_tokens_from_input",
        );
        expect(
            outline.replace(
                "phrases = [\"TODO\", \"TBD\", \"as an AI language model\"]",
                "phrases = [\"TODO\"]\nfit_by_dropping = true",
            ),
            "fit_by_dropping",
        );
    }

    #[test]
    fn output_schema_and_deterministic_checks_reject_invalid_outputs_and_excluded_corpora() {
        let outline = recipe(BUILTIN_WRITE_OUTLINE_RECIPE_TOML);
        let args = json!({"topic": "agentic", "audience": "technical", "length": "brief"});

        // Excluded corpus licence -> refused
        let mut excluded = outline_hits();
        excluded[0].licence_class = LicenceClass::Excluded;
        assert!(matches!(
            run(&outline, args.clone(), &excluded),
            Err(RecipeError::ExcludedCorpusLicence { .. })
        ));

        // Banned slop phrase in an excerpt -> refused by the `no-slop` check
        let mut slop = outline_hits();
        slop[0].excerpt = "Agentic control plane TODO fill later".into();
        assert!(matches!(
            run(&outline, args, &slop),
            Err(RecipeError::CheckFailed { .. })
        ));

        // Direct schema validation rejects missing required fields
        assert!(outline
            .validate_output(&json!({"recipe": "write.outline"}))
            .is_err());
    }

    const SOURCES_RECIPE: &str = r#"
[recipe]
format_version = "xerj-recipe/v1"
pack = "demo"
name = "demo.sources"
description = "A recipe whose [corpora] block names alpha (required) and beta (optional)."

[[inputs]]
name = "topic"
kind = "string"
required = true
description = "Free text the source ranks by."

[[inputs]]
name = "tags"
kind = "string_array"
description = "More free text."

[[inputs]]
name = "length"
kind = "string"
description = "An enum: picks a variant, is not search text."
enum_values = ["brief", "long"]

[[inputs]]
name = "count"
kind = "integer"
description = "Not text."

[corpora]
required = ["alpha"]
optional = ["beta"]
min_hits = 1

[ranking]
strategy = "score_desc"
top_k = 10

[output_schema]
required_fields = ["items"]

[output_schema.properties.recipe]
kind = "string"

[output_schema.properties.pack]
kind = "string"

[output_schema.properties.items]
kind = "array"
non_empty = true
item_required_fields = ["corpus", "record_id"]
"#;

    fn sources_recipe() -> PackRecipe {
        recipe(SOURCES_RECIPE)
    }

    /// A source of `(corpus, [record ids])`, best score first by position.
    fn held(corpora: &[(&str, &[&str])]) -> BTreeMap<String, Vec<RecipeCorpusHit>> {
        corpora
            .iter()
            .map(|(corpus, ids)| {
                let hits = ids
                    .iter()
                    .enumerate()
                    .map(|(n, id)| hit(corpus, id, "T", "E", 0.9 - n as f64 * 0.1, &[]))
                    .collect();
                (corpus.to_string(), hits)
            })
            .collect()
    }

    fn run_from(
        recipe: &PackRecipe,
        args: Value,
        source: &dyn RecipeCorpusSource,
    ) -> Result<RecipeExecutionReport> {
        RecipeRunner::execute_from_source(
            recipe,
            &args,
            source,
            None,
            InputOrigin::AuthorizedControlPlane,
            now(),
        )
    }

    fn items(report: &RecipeExecutionReport) -> Vec<(String, String)> {
        report.output["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                (
                    i["corpus"].as_str().unwrap().to_string(),
                    i["record_id"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    fn pair(corpus: &str, id: &str) -> (String, String) {
        (corpus.to_string(), id.to_string())
    }

    #[test]
    fn corpora_selects_only_the_declared_corpora_from_a_multi_corpus_source() {
        // `gamma` is not in `[corpora]` and holds the best-scored hit of all.
        let mut source = held(&[
            ("alpha", &["a1", "a2"]),
            ("beta", &["b1"]),
            ("gamma", &["g1"]),
        ]);
        source.get_mut("gamma").unwrap()[0].score = 0.99;

        let report = run_from(&sources_recipe(), json!({"topic": "x"}), &source).unwrap();
        assert_eq!(
            items(&report),
            [pair("alpha", "a1"), pair("beta", "b1"), pair("alpha", "a2")]
        );
        assert!(items(&report).iter().all(|(c, _)| c != "gamma"));

        // The required corpus alone is enough.
        let report = run_from(
            &sources_recipe(),
            json!({"topic": "x"}),
            &held(&[("alpha", &["a1"])]),
        )
        .unwrap();
        assert_eq!(items(&report), [pair("alpha", "a1")]);
    }

    #[test]
    fn a_missing_required_corpus_refuses_and_a_missing_optional_one_does_not() {
        let args = json!({"topic": "x"});
        let recipe = sources_recipe();

        // `beta` is optional and absent: the report is built from `alpha`.
        let report = run_from(&recipe, args.clone(), &held(&[("alpha", &["a1"])])).unwrap();
        assert_eq!(items(&report), [pair("alpha", "a1")]);

        // `alpha` is required: absent, or present with nothing to offer, refuses
        // by name, and `beta` and `gamma` cannot stand in for it.
        for source in [
            held(&[("beta", &["b1"]), ("gamma", &["g1"])]),
            held(&[("alpha", &[]), ("beta", &["b1"])]),
        ] {
            let err = run_from(&recipe, args.clone(), &source).unwrap_err();
            assert!(
                matches!(
                    &err,
                    RecipeError::MissingRequiredCorpus { recipe, corpus }
                        if recipe == "demo.sources" && corpus == "alpha"
                ),
                "{err}"
            );
            assert!(err.to_string().contains("`alpha`"), "{err}");
        }
    }

    /// A corpus that is held but cannot be read: never skipped.
    struct Unreadable(&'static str);

    impl RecipeCorpusSource for Unreadable {
        fn fetch_corpus_hits(
            &self,
            corpus: &str,
            _query: &str,
            _max_hits: usize,
        ) -> Result<Vec<RecipeCorpusHit>> {
            if corpus == self.0 {
                return Err(RecipeError::CorpusUnreadable {
                    corpus: corpus.to_string(),
                    reason: "the pack does not verify".into(),
                });
            }
            held(&[("alpha", &["a1"]), ("beta", &["b1"])])
                .remove(corpus)
                .ok_or_else(|| RecipeError::CorpusNotInstalled {
                    corpus: corpus.to_string(),
                })
        }
    }

    #[test]
    fn a_corpus_that_is_held_but_unreadable_refuses_whether_required_or_optional() {
        for broken in ["alpha", "beta"] {
            let err = run_from(
                &sources_recipe(),
                json!({"topic": "x"}),
                &Unreadable(broken),
            )
            .unwrap_err();
            assert!(
                matches!(&err, RecipeError::CorpusUnreadable { corpus, .. } if corpus == broken),
                "{broken}: {err}"
            );
            let text = err.to_string();
            assert!(
                text.contains(&format!("`{broken}`")) && text.contains("does not verify"),
                "{text}"
            );
        }
    }

    /// Records what the runner asked for, and in what order.
    #[derive(Default)]
    struct Recording {
        asked: std::sync::Mutex<Vec<(String, String, usize)>>,
    }

    impl RecipeCorpusSource for Recording {
        fn fetch_corpus_hits(
            &self,
            corpus: &str,
            query: &str,
            max_hits: usize,
        ) -> Result<Vec<RecipeCorpusHit>> {
            self.asked
                .lock()
                .unwrap()
                .push((corpus.to_string(), query.to_string(), max_hits));
            Ok(vec![hit(corpus, "r1", "T", "E", 0.5, &[])])
        }
    }

    #[test]
    fn the_runner_asks_required_then_optional_with_the_calls_free_text_as_the_query() {
        let recipe = sources_recipe();
        let source = Recording::default();
        run_from(
            &recipe,
            json!({"topic": " agentic control ", "tags": ["a b", "c"], "length": "brief",
                   "count": 3}),
            &source,
        )
        .unwrap();
        let asked = source.asked.lock().unwrap().clone();
        let query = "agentic control a b c".to_string();
        assert_eq!(
            asked,
            [
                (
                    "alpha".to_string(),
                    query.clone(),
                    MAX_SOURCE_HITS_PER_CORPUS
                ),
                ("beta".to_string(), query, MAX_SOURCE_HITS_PER_CORPUS),
            ]
        );

        // A corpus named as both is asked once, as required: the optional entry
        // does not turn a missing required corpus into a skipped one.
        let both = PackRecipe::from_toml_str(
            &SOURCES_RECIPE.replace("optional = [\"beta\"]", "optional = [\"alpha\", \"beta\"]"),
        )
        .unwrap();
        let source = Recording::default();
        run_from(&both, json!({"topic": "x"}), &source).unwrap();
        let corpora: Vec<String> = source
            .asked
            .lock()
            .unwrap()
            .iter()
            .map(|(c, _, _)| c.clone())
            .collect();
        assert_eq!(corpora, ["alpha", "beta"]);
        let err = run_from(&both, json!({"topic": "x"}), &held(&[("beta", &["b1"])])).unwrap_err();
        assert!(
            matches!(err, RecipeError::MissingRequiredCorpus { .. }),
            "{err}"
        );
    }

    #[test]
    fn a_source_is_not_asked_for_a_call_that_the_arguments_or_the_scope_refuse() {
        let source = Recording::default();
        let err = run_from(&sources_recipe(), json!({}), &source).unwrap_err();
        assert!(matches!(err, RecipeError::InvalidArguments { .. }), "{err}");

        let cyber = &sample_recipes().unwrap()[0];
        let err = run_from(
            cyber,
            json!({"target": "api.example.com", "cwe": "CWE-918"}),
            &source,
        )
        .unwrap_err();
        assert!(matches!(err, RecipeError::ScopeRequired { .. }), "{err}");
        assert!(source.asked.lock().unwrap().is_empty());
    }

    #[test]
    fn execute_lets_the_corpora_block_govern_hits_supplied_directly() {
        let recipe = sources_recipe();
        let args = json!({"topic": "x"});
        let mut gamma = hit("gamma", "g1", "T", "E", 0.99, &[]);
        gamma.licence_class = LicenceClass::Excluded;
        let alpha = hit("alpha", "a1", "T", "E", 0.5, &[]);
        let beta = hit("beta", "b1", "T", "E", 0.6, &[]);

        // An undeclared corpus neither contributes nor trips the licence gate.
        let report = run(&recipe, args.clone(), &[gamma.clone(), alpha, beta.clone()]).unwrap();
        assert_eq!(items(&report), [pair("beta", "b1"), pair("alpha", "a1")]);

        // Nor can it, or an optional corpus, stand in for the required one.
        for hits in [vec![gamma], vec![beta]] {
            let err = run(&recipe, args.clone(), &hits).unwrap_err();
            assert!(
                matches!(&err, RecipeError::MissingRequiredCorpus { corpus, .. } if corpus == "alpha"),
                "{err}"
            );
        }
    }

    fn pinned_line(corpus: &str, id: &str, score: f64) -> String {
        serde_json::to_string(&hit(corpus, id, "T", "E", score, &[])).unwrap()
    }

    #[test]
    fn a_jsonl_dir_opens_only_the_corpora_the_recipe_names() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, body: String| {
            std::fs::write(dir.path().join(format!("{name}.jsonl")), body).unwrap()
        };
        write("alpha", format!("{}\n\n", pinned_line("alpha", "a1", 0.5)));
        write("beta", pinned_line("beta", "b1", 0.6));
        // Not in `[corpora]`, and not even valid: reading it would fail the call.
        write("gamma", "{not json".to_string());
        let source = JsonlCorpusDir::new(dir.path());
        assert_eq!(source.dir(), dir.path());

        let report = run_from(&sources_recipe(), json!({"topic": "x"}), &source).unwrap();
        assert_eq!(items(&report), [pair("beta", "b1"), pair("alpha", "a1")]);

        // No `alpha.jsonl`: the required corpus is missing, the optional one is not.
        std::fs::remove_file(dir.path().join("alpha.jsonl")).unwrap();
        let err = run_from(&sources_recipe(), json!({"topic": "x"}), &source).unwrap_err();
        assert!(
            matches!(&err, RecipeError::MissingRequiredCorpus { corpus, .. } if corpus == "alpha"),
            "{err}"
        );
        // No `beta.jsonl`: only the optional corpus is missing.
        write("alpha", pinned_line("alpha", "a1", 0.5));
        std::fs::remove_file(dir.path().join("beta.jsonl")).unwrap();
        let report = run_from(&sources_recipe(), json!({"topic": "x"}), &source).unwrap();
        assert_eq!(items(&report), [pair("alpha", "a1")]);

        // A snapshot that is present but wrong is not "absent", optional or not.
        write("beta", pinned_line("alpha", "b1", 0.6));
        let err = run_from(&sources_recipe(), json!({"topic": "x"}), &source).unwrap_err();
        assert!(
            matches!(&err, RecipeError::CorpusUnreadable { corpus, .. } if corpus == "beta"),
            "{err}"
        );
        write("beta", "{not json".to_string());
        let err = source.fetch_corpus_hits("beta", "", 10).unwrap_err();
        assert!(err.to_string().contains("beta.jsonl:1"), "{err}");

        // A corpus slug is a file name or it is refused, never joined into a path.
        for slug in ["ns:corpus", ".hidden", "-lead", "../alpha"] {
            let err = source.fetch_corpus_hits(slug, "", 10).unwrap_err();
            assert!(
                matches!(err, RecipeError::CorpusUnreadable { .. }),
                "{slug}: {err}"
            );
        }
    }

    #[test]
    fn a_source_never_returns_more_than_max_hits_and_keeps_the_best_scored() {
        let ids: Vec<String> = (0..6).map(|n| format!("r{n}")).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let source = held(&[("alpha", &refs[..])]);

        let top = source.fetch_corpus_hits("alpha", "", 2).unwrap();
        let top: Vec<&str> = top.iter().map(|h| h.record_id.as_str()).collect();
        assert_eq!(top, ["r0", "r1"]);
        assert_eq!(source.fetch_corpus_hits("alpha", "", 100).unwrap().len(), 6);
        assert!(matches!(
            source.fetch_corpus_hits("gamma", "", 2),
            Err(RecipeError::CorpusNotInstalled { corpus }) if corpus == "gamma"
        ));
    }

    #[test]
    fn a_chain_asks_each_source_until_one_holds_the_corpus() {
        let chain = ChainedCorpusSource::new()
            .then(held(&[("alpha", &["a1"])]))
            .then(held(&[("alpha", &["shadowed"]), ("beta", &["b1"])]));

        let report = run_from(&sources_recipe(), json!({"topic": "x"}), &chain).unwrap();
        assert_eq!(items(&report), [pair("alpha", "a1"), pair("beta", "b1")]);
        assert!(matches!(
            chain.fetch_corpus_hits("gamma", "", 5),
            Err(RecipeError::CorpusNotInstalled { .. })
        ));

        // A source that holds the corpus but cannot read it ends the chain: the
        // next source does not paper over it.
        let broken = ChainedCorpusSource::new()
            .then(Unreadable("alpha"))
            .then(held(&[("alpha", &["a1"])]));
        let err = broken.fetch_corpus_hits("alpha", "", 5).unwrap_err();
        assert!(matches!(err, RecipeError::CorpusUnreadable { .. }), "{err}");
    }
}
