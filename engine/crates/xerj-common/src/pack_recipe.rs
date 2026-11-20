//! Declarative TOML tool recipes and deterministic Rust recipe executor
//! (`Xerj Packs` 3/9, #2762, #2769).
//!
//! Pack tools (`writing`, `bio`, `cyber`, and user-defined packs) are defined
//! by versioned TOML recipe files (`format_version = "xerj-recipe/v1"`) and
//! executed by [`RecipeRunner`] in pure Rust:
//! - Automatic MCP tool projection ([`PackRecipe::to_mcp_tool_spec`]) turns any
//!   validated TOML recipe into a `tools/list` descriptor (`name`,
//!   `description`, `inputSchema`) with no Rust changes.
//! - Invalid recipes fail at build/test time with a precise [`RecipeError`].
//! - Execution performs corpus selection, redistribution licence checks,
//!   input-bound filtering, deterministic ranking and clustering, output JSON
//!   Schema validation, and deterministic post-checks (citation completeness,
//!   banned-phrase / anti-slop refusal, token ceiling, and mandatory notices),
//!   sealing the result with an RFC 8785 (JCS) SHA-256 receipt.

use crate::cyber_scope::{InputOrigin, ScopeError, ScopeGuard, WorkOrder};
use crate::hub::LicenceClass;
use crate::hub_evidence::{sha256, PinnedCitation};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const RECIPE_FORMAT_VERSION: &str = "xerj-recipe/v1";
pub const RECIPE_REPORT_VERSION: &str = "xerj-recipe-execution/v1";
const MAX_REPORT_BYTES: usize = 4 * 1024 * 1024;

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
    #[error("recipe `{recipe}` abstained: matched {matched} hits, need at least {min_hits}")]
    InsufficientEvidence {
        recipe: String,
        matched: usize,
        min_hits: usize,
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
    #[serde(default = "default_one")]
    pub min_hits: usize,
}

fn default_true() -> bool {
    true
}
fn default_one() -> usize {
    1
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
    pub target_field: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phrases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<usize>,
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
            if !self
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
                CheckKind::MaxTokenBudget if chk.max_tokens.unwrap_or(0) == 0 => {
                    return bad(format!(
                        "check {:?} (max_token_budget) must set a positive max_tokens",
                        chk.id
                    ));
                }
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
        json!({
            "name": self.mcp_name(),
            "description": self.recipe.description,
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
                    let max_t = chk.max_tokens.unwrap_or(1);
                    let serialized = serde_json::to_string(val)?;
                    // Deterministic token estimate (~4 UTF-8 bytes per token, rounded up).
                    let est_tokens = serialized.len().div_ceil(4);
                    if est_tokens > max_t {
                        return Err(RecipeError::CheckFailed {
                            recipe: self.recipe.name.clone(),
                            check_id: chk.id.clone(),
                            reason: format!(
                                "field {:?} estimated at {est_tokens} tokens (max {max_t})",
                                chk.target_field
                            ),
                        });
                    }
                    outcomes.push(CheckOutcome {
                        check_id: chk.id.clone(),
                        passed: true,
                        detail: format!("estimated {est_tokens} tokens <= {max_t}"),
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

impl RecipeRunner {
    /// Execute a declarative [`PackRecipe`] over supplied corpus hits and MCP
    /// arguments, validating the output against the recipe's schema and running
    /// every deterministic check step before sealing the report.
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

        let scope_echo = if recipe.recipe.requires_scope {
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
            Some(wo.clone())
        } else {
            None
        };

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

        // Deterministic ranking
        filtered.sort_by(|a, b| {
            let primary = match recipe.ranking.strategy {
                RankingStrategy::ScoreDesc => b
                    .score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal),
                RankingStrategy::RecencyDesc => b.published_date.cmp(&a.published_date),
                RankingStrategy::StudyGradeAsc => a
                    .study_grade_rank
                    .unwrap_or(99)
                    .cmp(&b.study_grade_rank.unwrap_or(99))
                    .then_with(|| {
                        b.score
                            .partial_cmp(&a.score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    }),
            };
            primary
                .then_with(|| a.corpus.cmp(&b.corpus))
                .then_with(|| a.record_id.cmp(&b.record_id))
        });

        // Optional per-cluster cap + top_k truncation
        let mut selected: Vec<&RecipeCorpusHit> = Vec::new();
        let mut cluster_counts: HashMap<String, usize> = HashMap::new();
        for hit in filtered {
            if selected.len() >= recipe.ranking.top_k {
                break;
            }
            if let (Some(cfield), Some(max_c)) =
                (&recipe.ranking.cluster_by, recipe.ranking.max_per_cluster)
            {
                let key = hit_field(hit, cfield).unwrap_or("default").to_string();
                let count = cluster_counts.entry(key).or_insert(0);
                if *count >= max_c {
                    continue;
                }
                *count += 1;
            }
            selected.push(hit);
        }

        let items_json: Vec<Value> = selected
            .iter()
            .map(|h| {
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
                Value::Object(m)
            })
            .collect();

        let mut out_map = Map::new();
        out_map.insert("recipe".into(), json!(recipe.recipe.name));
        out_map.insert("pack".into(), json!(recipe.recipe.pack));
        out_map.insert("items".into(), Value::Array(items_json));
        if let Some(notice) = &recipe.recipe.static_notice {
            out_map.insert("notice".into(), json!(notice));
        }
        for (k, v) in &valid_args {
            if recipe.output_schema.properties.contains_key(k) && !out_map.contains_key(k) {
                out_map.insert(k.clone(), v.clone());
            }
        }
        let output = Value::Object(out_map);

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

fn hit_field<'a>(hit: &'a RecipeCorpusHit, field: &str) -> Option<&'a str> {
    match field {
        "corpus" => Some(&hit.corpus),
        "record_id" => Some(&hit.record_id),
        "title" => Some(&hit.title),
        "excerpt" => Some(&hit.excerpt),
        "published_date" => Some(&hit.published_date),
        _ => hit.fields.get(field).map(String::as_str),
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
        let ok = match f.op {
            FilterOp::Equals => actual.eq_ignore_ascii_case(target_val),
            FilterOp::NotEquals => !actual.eq_ignore_ascii_case(target_val),
            FilterOp::Contains => actual
                .to_ascii_lowercase()
                .contains(&target_val.to_ascii_lowercase()),
            FilterOp::Prefix => actual
                .to_ascii_lowercase()
                .starts_with(&target_val.to_ascii_lowercase()),
            FilterOp::In => target_val
                .split(',')
                .any(|part| actual.eq_ignore_ascii_case(part.trim())),
        };
        if !ok {
            return false;
        }
    }
    true
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

/// Embedded sample TOML recipe for the Writing pack (`write.outline`).
pub const BUILTIN_WRITE_OUTLINE_RECIPE_TOML: &str = r#"
[recipe]
format_version = "xerj-recipe/v1"
pack = "writing"
name = "write.outline"
mcp_tool_name = "xerj_write_outline"
description = "Cluster hub evidence into cited chapter items for an outline."
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
required = false
description = "Target audience level."
enum_values = ["general", "technical", "executive"]

[corpora]
required = ["agentic-security-ietf-corpus"]
optional = []
require_redistribution_licence = true
min_hits = 1

[[filters]]
field = "excerpt"
op = "contains"
from_input = "topic"

[ranking]
strategy = "score_desc"
top_k = 10
cluster_by = "chapter"
max_per_cluster = 3

[output_schema]
required_fields = ["recipe", "pack", "topic", "items"]

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
non_empty = false

[output_schema.properties.items]
kind = "array"
non_empty = true
item_required_fields = ["corpus", "record_id", "title", "excerpt", "pin_sha", "fetched_at"]

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
target_field = "items"
max_tokens = 4096
"#;

/// Embedded sample TOML recipe for the Bio pack (`bio.evidence`).
pub const BUILTIN_BIO_EVIDENCE_RECIPE_TOML: &str = r#"
[recipe]
format_version = "xerj-recipe/v1"
pack = "bio"
name = "bio.evidence"
mcp_tool_name = "xerj_bio_evidence"
description = "Grade biomedical corpus evidence by study type with redistribution and non-medical-advice guards."
requires_scope = false
static_notice = "Literature synthesis from cited corpora only; not medical advice or clinical diagnosis."

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

[[filters]]
field = "excerpt"
op = "contains"
from_input = "condition"

[ranking]
strategy = "study_grade_asc"
top_k = 10

[output_schema]
required_fields = ["recipe", "pack", "condition", "notice", "items"]

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
item_required_fields = ["corpus", "record_id", "title", "excerpt", "pin_sha", "fetched_at", "study_type", "evidence_grade"]

[[checks]]
id = "citations-present"
kind = "require_citations"
target_field = "items"

[[checks]]
id = "medical-disclaimer"
kind = "require_notice"
target_field = "notice"
expected_substring = "not medical advice"
"#;

/// Embedded sample TOML recipe for the Cyber pack (`cyber.target_surface`).
pub const BUILTIN_CYBER_SURFACE_RECIPE_TOML: &str = r#"
[recipe]
format_version = "xerj-recipe/v1"
pack = "cyber"
name = "cyber.target_surface"
mcp_tool_name = "xerj_cyber_target_surface"
description = "Match authorized target surface patterns from vulnerability corpora under a ScopeGuard."
requires_scope = true

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

/// Load and validate all built-in TOML pack recipes embedded in the binary.
pub fn builtin_recipes() -> Result<Vec<PackRecipe>> {
    Ok(vec![
        PackRecipe::from_toml_str(BUILTIN_WRITE_OUTLINE_RECIPE_TOML)?,
        PackRecipe::from_toml_str(BUILTIN_BIO_EVIDENCE_RECIPE_TOML)?,
        PackRecipe::from_toml_str(BUILTIN_CYBER_SURFACE_RECIPE_TOML)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_hits() -> Vec<RecipeCorpusHit> {
        vec![
            RecipeCorpusHit {
                corpus: "agentic-security-ietf-corpus".into(),
                licence_class: LicenceClass::AdaptWithAttribution,
                record_id: "draft-durga-trace-00".into(),
                title: "TRACE verification receipt format".into(),
                excerpt:
                    "Zero-trust agentic control planes bind every tool action to a signed receipt."
                        .into(),
                score: 0.96,
                published_date: "2026-11-01".into(),
                study_grade_rank: None,
                pin_sha: "a".repeat(40),
                fetched_at: "2026-11-05".into(),
                fields: BTreeMap::from([("chapter".into(), "1-architecture".into())]),
                pinned_citation: None,
            },
            RecipeCorpusHit {
                corpus: "open-biomed-trials".into(),
                licence_class: LicenceClass::AdaptWithAttribution,
                record_id: "NCT-2026-001".into(),
                title: "Multi-center randomized trial of metabolic intervention".into(),
                excerpt: "Randomized double-blind cohort evaluating glycemic response.".into(),
                score: 0.91,
                published_date: "2026-10-15".into(),
                study_grade_rank: Some(1),
                pin_sha: "b".repeat(40),
                fetched_at: "2026-11-05".into(),
                fields: BTreeMap::from([
                    ("study_type".into(), "randomized_controlled_trial".into()),
                    ("evidence_grade".into(), "grade_a".into()),
                ]),
                pinned_citation: None,
            },
            RecipeCorpusHit {
                corpus: "rust-vulns".into(),
                licence_class: LicenceClass::AdaptWithAttribution,
                record_id: "RUSTSEC-2026-0099".into(),
                title: "Redirect host allowlist bypass in HTTP client".into(),
                excerpt: "Unchecked 302 redirect reaches internal metadata endpoint.".into(),
                score: 0.94,
                published_date: "2026-11-02".into(),
                study_grade_rank: None,
                pin_sha: "c".repeat(40),
                fetched_at: "2026-11-05".into(),
                fields: BTreeMap::from([("cwe".into(), "CWE-918".into())]),
                pinned_citation: None,
            },
        ]
    }

    #[test]
    fn builtin_recipes_parse_export_mcp_tools_and_execute_end_to_end() {
        let recipes = builtin_recipes().unwrap();
        assert_eq!(recipes.len(), 3);

        let mcp_specs: Vec<Value> = recipes.iter().map(PackRecipe::to_mcp_tool_spec).collect();
        assert_eq!(mcp_specs[0]["name"], "xerj_write_outline");
        assert_eq!(mcp_specs[1]["name"], "xerj_bio_evidence");
        assert_eq!(mcp_specs[2]["name"], "xerj_cyber_target_surface");

        let hits = sample_hits();
        let now = Utc.with_ymd_and_hms(2026, 11, 10, 12, 0, 0).unwrap();

        // 1. Execute write.outline
        let write_rep = RecipeRunner::execute(
            &recipes[0],
            &json!({"topic": "agentic", "audience": "technical"}),
            &hits,
            None,
            InputOrigin::AuthorizedControlPlane,
            now,
        )
        .unwrap();
        let jcs = write_rep.to_jcs(&recipes[0]).unwrap();
        assert!(!jcs.is_empty());
        assert_eq!(write_rep.checks.len(), 3);

        // 2. Execute bio.evidence
        let bio_rep = RecipeRunner::execute(
            &recipes[1],
            &json!({"condition": "glycemic"}),
            &hits,
            None,
            InputOrigin::AuthorizedControlPlane,
            now,
        )
        .unwrap();
        assert!(bio_rep.output["notice"]
            .as_str()
            .unwrap()
            .contains("not medical advice"));
        bio_rep.verify(&recipes[1]).unwrap();

        // 3. Execute cyber.target_surface: requires ScopeGuard
        assert!(matches!(
            RecipeRunner::execute(
                &recipes[2],
                &json!({"target": "api.example.com", "cwe": "CWE-918"}),
                &hits,
                None,
                InputOrigin::AuthorizedControlPlane,
                now,
            ),
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

        let cyber_rep = RecipeRunner::execute(
            &recipes[2],
            &json!({"target": "api.example.com", "cwe": "CWE-918"}),
            &hits,
            Some(&guard),
            InputOrigin::AuthorizedControlPlane,
            now,
        )
        .unwrap();
        assert!(cyber_rep.scope.is_some());
        cyber_rep.verify(&recipes[2]).unwrap();
    }

    #[test]
    fn invalid_recipes_fail_with_precise_errors() {
        // Bad format_version
        let bad_ver = BUILTIN_WRITE_OUTLINE_RECIPE_TOML.replace("xerj-recipe/v1", "v0");
        assert!(PackRecipe::from_toml_str(&bad_ver)
            .unwrap_err()
            .to_string()
            .contains("format_version"));

        // Filter referencing non-existent input
        let bad_filter = BUILTIN_WRITE_OUTLINE_RECIPE_TOML
            .replace("from_input = \"topic\"", "from_input = \"ghost\"");
        assert!(PackRecipe::from_toml_str(&bad_filter)
            .unwrap_err()
            .to_string()
            .contains("unknown input"));

        // Undeclared required output field
        let bad_schema = BUILTIN_WRITE_OUTLINE_RECIPE_TOML.replace(
            "required_fields = [\"recipe\", \"pack\", \"topic\", \"items\"]",
            "required_fields = [\"recipe\", \"missing_field\"]",
        );
        assert!(PackRecipe::from_toml_str(&bad_schema)
            .unwrap_err()
            .to_string()
            .contains("undeclared property"));
    }

    #[test]
    fn output_schema_and_deterministic_checks_reject_invalid_outputs_and_excluded_corpora() {
        let recipe = PackRecipe::from_toml_str(BUILTIN_WRITE_OUTLINE_RECIPE_TOML).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 11, 10, 12, 0, 0).unwrap();

        // Excluded corpus licence -> refused
        let mut excluded_hits = sample_hits();
        excluded_hits[0].licence_class = LicenceClass::Excluded;
        assert!(matches!(
            RecipeRunner::execute(
                &recipe,
                &json!({"topic": "agentic"}),
                &excluded_hits,
                None,
                InputOrigin::AuthorizedControlPlane,
                now,
            ),
            Err(RecipeError::ExcludedCorpusLicence { .. })
        ));

        // Banned slop phrase in excerpt -> refused by `no-slop` check
        let mut slop_hits = sample_hits();
        slop_hits[0].excerpt = "Zero-trust agentic control plane TODO fill later".into();
        assert!(matches!(
            RecipeRunner::execute(
                &recipe,
                &json!({"topic": "agentic"}),
                &slop_hits,
                None,
                InputOrigin::AuthorizedControlPlane,
                now,
            ),
            Err(RecipeError::CheckFailed { .. })
        ));

        // Direct schema validation rejects missing required fields
        assert!(recipe
            .validate_output(&json!({"recipe": "write.outline"}))
            .is_err());
    }
}
