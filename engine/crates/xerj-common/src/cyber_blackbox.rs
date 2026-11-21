//! Attack-surface mapping and corpus-backed test-case planning against
//! authorized targets (`cyber.blackbox`, #2772, #2767, #2769).
//!
//! `xerj_common::cyber_blackbox` is a pure-Rust, network-free planning primitive:
//! it sends no traffic and resolves no DNS names. Every endpoint in a black-box
//! plan is checked against a caller-supplied [`crate::cyber_scope::ScopeGuard`]
//! originating from [`crate::cyber_scope::InputOrigin::AuthorizedControlPlane`].
//! Missing scope, untrusted repository origin, expired windows, or out-of-scope
//! targets fail closed and refuse the entire plan.
//!
//! Every planned test case must cite at least one corpus pattern with a pinned
//! 40-hex commit (`pin_sha`), fetch date (`fetched_at`), CWE identifier, and
//! OWASP category. Endpoints for which no corpus pattern matches are recorded
//! as explicit abstentions rather than guessed.

use crate::cyber_scope::{InputOrigin, ScopeError, ScopeGuard, WorkOrder};
use crate::hub_evidence::{sha256, PinnedCitation};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const BLACKBOX_REPORT_VERSION: &str = "xerj-cyber-blackbox/v1";
const MAX_REPORT_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENDPOINTS: usize = 2048;
const MAX_PATTERNS: usize = 4096;
const MAX_TEXT: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum BlackboxError {
    #[error("cyber.blackbox refused: {0}")]
    Scope(#[from] ScopeError),
    #[error("invalid blackbox target or endpoint: {0}")]
    InvalidEndpoint(&'static str),
    #[error("invalid corpus pattern `{id}`: {reason}")]
    InvalidPattern { id: String, reason: &'static str },
    #[error("invalid blackbox plan: {0}")]
    InvalidPlan(&'static str),
    #[error("blackbox report digest mismatch")]
    DigestMismatch,
    #[error("blackbox report scope does not match the supplied ScopeGuard")]
    ScopeMismatch,
    #[error("blackbox JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, BlackboxError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthSurface {
    Unauthenticated,
    SessionCookie,
    BearerToken,
    ApiKey,
    MutualTls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterLocation {
    Path,
    Query,
    Header,
    BodyJson,
    BodyForm,
    Multipart,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointParam {
    pub name: String,
    pub location: ParameterLocation,
    /// Optional surface tag such as `id_reference`, `url`, `query_filter`,
    /// `file_path`, `template`, or `prompt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_tag: Option<String>,
}

impl EndpointParam {
    pub fn validate(&self) -> Result<()> {
        if !valid_token(&self.name, 128) {
            return Err(BlackboxError::InvalidEndpoint(
                "parameter name must be 1..=128 ASCII token characters",
            ));
        }
        if let Some(tag) = &self.surface_tag {
            if !valid_slug(tag, 64) {
                return Err(BlackboxError::InvalidEndpoint(
                    "surface_tag must be 1..=64 ASCII [A-Za-z0-9._:-]",
                ));
            }
        }
        Ok(())
    }
}

/// An observed endpoint supplied by the operator for surface mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedEndpoint {
    pub id: String,
    /// Bare hostname or IP literal (no scheme, port, userinfo, or path).
    pub target: String,
    /// Uppercase HTTP method (`GET`, `POST`, `PUT`, `PATCH`, `DELETE`, `OPTIONS`, `HEAD`).
    pub method: String,
    /// Absolute path starting with `/`, without `..`, query, or fragment.
    pub path: String,
    pub auth_surface: AuthSurface,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<EndpointParam>,
}

impl ObservedEndpoint {
    pub fn validate(&self) -> Result<()> {
        if !valid_slug(&self.id, 128) {
            return Err(BlackboxError::InvalidEndpoint(
                "endpoint id must be 1..=128 ASCII [A-Za-z0-9._:-]",
            ));
        }
        if !valid_line(&self.target)
            || self.target.contains("://")
            || self.target.contains('/')
            || self.target.contains('@')
            || self.target.chars().any(char::is_whitespace)
        {
            return Err(BlackboxError::InvalidEndpoint(
                "endpoint target must be a bare host or IP literal",
            ));
        }
        if !matches!(
            self.method.as_str(),
            "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS" | "HEAD"
        ) {
            return Err(BlackboxError::InvalidEndpoint(
                "endpoint method must be GET, POST, PUT, PATCH, DELETE, OPTIONS, or HEAD",
            ));
        }
        if !valid_endpoint_path(&self.path) {
            return Err(BlackboxError::InvalidEndpoint(
                "endpoint path must start with '/', contain no '..' segments, query, or fragment",
            ));
        }
        if self.parameters.len() > 128 {
            return Err(BlackboxError::InvalidEndpoint(
                "endpoint has too many parameters (max 128)",
            ));
        }
        let mut seen = HashSet::new();
        for p in &self.parameters {
            p.validate()?;
            if !seen.insert((&p.name, p.location)) {
                return Err(BlackboxError::InvalidEndpoint(
                    "duplicate parameter (name, location) on endpoint",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestPriority {
    P1,
    P2,
    P3,
    P4,
}

/// A corpus-backed test pattern used to plan non-destructive black-box checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusPatternRef {
    pub pattern_id: String,
    pub corpus: String,
    pub record_id: String,
    /// `CWE-<digits>` (e.g. `CWE-639`, `CWE-918`, `CWE-89`).
    pub cwe: String,
    /// OWASP category (e.g. `OWASP-API1:2023`, `OWASP-A01:2021`, `OWASP-LLM01:2025`).
    pub owasp_category: String,
    /// Surface tag this pattern targets (e.g. `id_reference`, `url`, `query_filter`,
    /// `file_path`, `unauthenticated_write`, `prompt`).
    pub target_surface_tag: String,
    pub summary: String,
    /// Non-destructive verification check description (never a weaponized exploit).
    pub verification_check: String,
    pub expected_safe_behavior: String,
    pub priority: TestPriority,
    /// Full 40-character lowercase hex commit SHA of the corpus pin.
    pub pin_sha: String,
    /// `YYYY-MM-DD` or RFC 3339 date the corpus was fetched.
    pub fetched_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_citation: Option<PinnedCitation>,
}

impl PartialEq for CorpusPatternRef {
    fn eq(&self, other: &Self) -> bool {
        self.pattern_id == other.pattern_id
            && self.corpus == other.corpus
            && self.record_id == other.record_id
            && self.cwe == other.cwe
            && self.owasp_category == other.owasp_category
            && self.target_surface_tag == other.target_surface_tag
            && self.summary == other.summary
            && self.verification_check == other.verification_check
            && self.expected_safe_behavior == other.expected_safe_behavior
            && self.priority == other.priority
            && self.pin_sha == other.pin_sha
            && self.fetched_at == other.fetched_at
    }
}
impl Eq for CorpusPatternRef {}

impl CorpusPatternRef {
    pub fn validate(&self) -> Result<()> {
        let bad = |reason: &'static str| {
            Err(BlackboxError::InvalidPattern {
                id: self.pattern_id.clone(),
                reason,
            })
        };
        if !valid_slug(&self.pattern_id, 128) {
            return bad("pattern_id must be 1..=128 ASCII [A-Za-z0-9._:-]");
        }
        if !valid_slug(&self.corpus, 128) || !valid_slug(&self.record_id, 128) {
            return bad("corpus and record_id must be 1..=128 ASCII [A-Za-z0-9._:-]");
        }
        if !valid_cwe(&self.cwe) {
            return bad("cwe must be CWE-<positive digits>");
        }
        if !valid_owasp(&self.owasp_category) {
            return bad(
                "owasp_category must start with OWASP- and contain only ASCII slug characters",
            );
        }
        if !valid_slug(&self.target_surface_tag, 64) {
            return bad("target_surface_tag must be 1..=64 ASCII [A-Za-z0-9._:-]");
        }
        if !valid_line(&self.summary)
            || !valid_line(&self.verification_check)
            || !valid_line(&self.expected_safe_behavior)
        {
            return bad("summary, verification_check, and expected_safe_behavior must be non-empty single-line text");
        }
        if self.pin_sha.len() != 40
            || !self
                .pin_sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return bad("pin_sha must be a 40-character lowercase hex git SHA");
        }
        if !valid_date_or_rfc3339(&self.fetched_at) {
            return bad("fetched_at must be YYYY-MM-DD or RFC 3339");
        }
        if let Some(cit) = &self.pinned_citation {
            if cit.corpus != self.corpus || cit.commit != self.pin_sha {
                return bad("pinned_citation corpus/commit must match pattern corpus and pin_sha");
            }
        }
        Ok(())
    }
}

/// One mapped endpoint in the attack surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttackSurfaceEntry {
    pub endpoint_id: String,
    pub normalized_target: String,
    pub method: String,
    pub path: String,
    pub auth_surface: AuthSurface,
    pub derived_surface_tags: Vec<String>,
    pub checked_at: String,
}

/// One prioritized, corpus-cited test case in the black-box plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedTestCase {
    pub case_id: String,
    pub endpoint_id: String,
    pub target: String,
    pub method: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter: Option<String>,
    pub matched_surface_tag: String,
    pub priority: TestPriority,
    pub cwe: String,
    pub owasp_category: String,
    pub verification_check: String,
    pub expected_safe_behavior: String,
    pub pattern: CorpusPatternRef,
}

/// Explicit abstention for an endpoint that matched no corpus pattern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointAbstention {
    pub endpoint_id: String,
    pub reason: String,
}

/// Deterministic, JCS-sealed black-box attack-surface and test plan report.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CyberBlackboxReport {
    pub report_version: String,
    pub plan_id: String,
    pub target_description: String,
    pub scope: WorkOrder,
    pub authorization_statement: String,
    pub surface_map: Vec<AttackSurfaceEntry>,
    pub test_cases: Vec<PlannedTestCase>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub abstentions: Vec<EndpointAbstention>,
    pub report_sha256: String,
}

#[derive(Serialize)]
struct Preimage<'a> {
    report_version: &'a str,
    plan_id: &'a str,
    target_description: &'a str,
    scope: &'a WorkOrder,
    authorization_statement: &'a str,
    surface_map: &'a [AttackSurfaceEntry],
    test_cases: &'a [PlannedTestCase],
    #[serde(skip_serializing_if = "Vec::is_empty")]
    abstentions: &'a Vec<EndpointAbstention>,
}

impl CyberBlackboxReport {
    /// Plan an attack-surface map and prioritized test cases against authorized
    /// endpoints. Sends no network traffic.
    pub fn plan(
        plan_id: impl Into<String>,
        target_description: impl Into<String>,
        endpoints: &[ObservedEndpoint],
        patterns: &[CorpusPatternRef],
        guard: Option<&ScopeGuard>,
        origin: InputOrigin,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        let plan_id = plan_id.into();
        let target_description = target_description.into();
        if !valid_slug(&plan_id, 128) {
            return Err(BlackboxError::InvalidPlan(
                "plan_id must be 1..=128 ASCII [A-Za-z0-9._:-]",
            ));
        }
        if !valid_line(&target_description) {
            return Err(BlackboxError::InvalidPlan(
                "target_description must be non-empty single-line text",
            ));
        }
        let guard = guard.ok_or(BlackboxError::Scope(ScopeError::MissingScope))?;
        if origin != InputOrigin::AuthorizedControlPlane {
            return Err(BlackboxError::Scope(ScopeError::UntrustedInput));
        }
        let wo = guard.scope();
        if now < wo.not_before {
            return Err(BlackboxError::Scope(ScopeError::NotYetValid));
        }
        if now > wo.expires_at {
            return Err(BlackboxError::Scope(ScopeError::Expired));
        }
        if endpoints.is_empty() || endpoints.len() > MAX_ENDPOINTS {
            return Err(BlackboxError::InvalidPlan(
                "endpoints must contain 1..=2048 entries",
            ));
        }
        if patterns.len() > MAX_PATTERNS {
            return Err(BlackboxError::InvalidPlan(
                "patterns exceeds maximum count (4096)",
            ));
        }

        let mut seen_ep = HashSet::new();
        for ep in endpoints {
            ep.validate()?;
            if !seen_ep.insert(&ep.id) {
                return Err(BlackboxError::InvalidEndpoint(
                    "duplicate endpoint id in plan",
                ));
            }
        }

        let mut seen_pat = HashSet::new();
        for pat in patterns {
            pat.validate()?;
            if !seen_pat.insert(&pat.pattern_id) {
                return Err(BlackboxError::InvalidPattern {
                    id: pat.pattern_id.clone(),
                    reason: "duplicate pattern_id",
                });
            }
        }

        let checked_at = now.to_rfc3339();
        let mut surface_map = Vec::with_capacity(endpoints.len());
        let mut test_cases = Vec::new();
        let mut abstentions = Vec::new();

        for ep in endpoints {
            // Authorize every endpoint target against ScopeGuard.
            let permit = guard.authorize(&ep.target, origin, now)?;
            let tags = derive_surface_tags(ep);

            surface_map.push(AttackSurfaceEntry {
                endpoint_id: ep.id.clone(),
                normalized_target: permit.target.clone(),
                method: ep.method.clone(),
                path: ep.path.clone(),
                auth_surface: ep.auth_surface,
                derived_surface_tags: tags.iter().map(|(t, _)| t.clone()).collect(),
                checked_at: checked_at.clone(),
            });

            let mut matched_any = false;
            for (tag, param_name) in &tags {
                for pat in patterns {
                    if &pat.target_surface_tag == tag {
                        matched_any = true;
                        let case_id = match param_name {
                            Some(p) => format!("{}:{}:{}", ep.id, p, pat.pattern_id),
                            None => format!("{}:{}", ep.id, pat.pattern_id),
                        };
                        test_cases.push(PlannedTestCase {
                            case_id,
                            endpoint_id: ep.id.clone(),
                            target: permit.target.clone(),
                            method: ep.method.clone(),
                            path: ep.path.clone(),
                            parameter: param_name.clone(),
                            matched_surface_tag: tag.clone(),
                            priority: pat.priority,
                            cwe: pat.cwe.clone(),
                            owasp_category: pat.owasp_category.clone(),
                            verification_check: pat.verification_check.clone(),
                            expected_safe_behavior: pat.expected_safe_behavior.clone(),
                            pattern: pat.clone(),
                        });
                    }
                }
            }

            if !matched_any {
                abstentions.push(EndpointAbstention {
                    endpoint_id: ep.id.clone(),
                    reason: "no corpus pattern matched endpoint surface tags; abstaining rather than guessing".into(),
                });
            }
        }

        // Sort test cases deterministically by (priority, endpoint_id, case_id).
        test_cases.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.endpoint_id.cmp(&b.endpoint_id))
                .then_with(|| a.case_id.cmp(&b.case_id))
        });

        let authorization_statement = format!(
            "Authorized under work order `{}` ({}) for window [{} ..= {}]; planner sends no traffic.",
            wo.id,
            wo.authorization,
            wo.not_before.to_rfc3339(),
            wo.expires_at.to_rfc3339()
        );

        let mut report = Self {
            report_version: BLACKBOX_REPORT_VERSION.to_string(),
            plan_id,
            target_description,
            scope: wo.clone(),
            authorization_statement,
            surface_map,
            test_cases,
            abstentions,
            report_sha256: String::new(),
        };
        report.report_sha256 = report.compute_digest()?;
        Ok(report)
    }

    fn compute_digest(&self) -> Result<String> {
        let pre = Preimage {
            report_version: &self.report_version,
            plan_id: &self.plan_id,
            target_description: &self.target_description,
            scope: &self.scope,
            authorization_statement: &self.authorization_statement,
            surface_map: &self.surface_map,
            test_cases: &self.test_cases,
            abstentions: &self.abstentions,
        };
        let bytes = serde_json_canonicalizer::to_vec(&pre)?;
        Ok(sha256(&bytes))
    }

    pub fn verify(&self) -> Result<()> {
        if self.report_version != BLACKBOX_REPORT_VERSION {
            return Err(BlackboxError::InvalidPlan("unsupported report_version"));
        }
        if !valid_slug(&self.plan_id, 128) || !valid_line(&self.target_description) {
            return Err(BlackboxError::InvalidPlan(
                "invalid plan_id or target_description",
            ));
        }
        if self.scope.id.trim().is_empty()
            || self.scope.authorization.trim().is_empty()
            || self.scope.targets.is_empty()
            || !self.authorization_statement.contains(&self.scope.id)
        {
            return Err(BlackboxError::Scope(ScopeError::MissingAuthorization));
        }
        if self.surface_map.is_empty() {
            return Err(BlackboxError::InvalidPlan("surface_map cannot be empty"));
        }
        for tc in &self.test_cases {
            tc.pattern.validate()?;
            if tc.cwe != tc.pattern.cwe || tc.owasp_category != tc.pattern.owasp_category {
                return Err(BlackboxError::InvalidPlan(
                    "test case CWE or OWASP category diverges from cited corpus pattern",
                ));
            }
        }
        let expected = self.compute_digest()?;
        if expected != self.report_sha256 {
            return Err(BlackboxError::DigestMismatch);
        }
        Ok(())
    }

    /// Re-verify a deserialized report against a live `ScopeGuard`.
    pub fn recheck_scope(
        &self,
        guard: &ScopeGuard,
        origin: InputOrigin,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.verify()?;
        let wo = guard.scope();
        if wo.id != self.scope.id
            || wo.authorization != self.scope.authorization
            || wo.targets != self.scope.targets
            || wo.not_before != self.scope.not_before
            || wo.expires_at != self.scope.expires_at
        {
            return Err(BlackboxError::ScopeMismatch);
        }
        for entry in &self.surface_map {
            let _ = guard.authorize(&entry.normalized_target, origin, now)?;
        }
        for tc in &self.test_cases {
            let _ = guard.authorize(&tc.target, origin, now)?;
        }
        Ok(())
    }

    pub fn to_jcs(&self) -> Result<Vec<u8>> {
        self.verify()?;
        let out = serde_json_canonicalizer::to_vec(self)?;
        if out.len() > MAX_REPORT_BYTES {
            return Err(BlackboxError::InvalidPlan("report exceeds size limit"));
        }
        Ok(out)
    }

    pub fn verify_json(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_REPORT_BYTES {
            return Err(BlackboxError::InvalidPlan("report exceeds size limit"));
        }
        reject_duplicate_json_keys(bytes)?;
        let report: Self = serde_json::from_slice(bytes)?;
        report.verify()?;
        Ok(report)
    }
}

fn derive_surface_tags(ep: &ObservedEndpoint) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    if ep.auth_surface == AuthSurface::Unauthenticated
        && matches!(ep.method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE")
    {
        out.push(("unauthenticated_mutation".to_string(), None));
    }
    for p in &ep.parameters {
        if let Some(tag) = &p.surface_tag {
            out.push((tag.clone(), Some(p.name.clone())));
        } else {
            let lower = p.name.to_ascii_lowercase();
            let inferred = if lower == "id" || lower.ends_with("_id") || lower.ends_with("id") {
                Some("id_reference")
            } else if lower.contains("url")
                || lower.contains("uri")
                || lower.contains("webhook")
                || lower.contains("callback")
            {
                Some("url_fetch")
            } else if lower.contains("path") || lower.contains("file") {
                Some("file_path")
            } else if lower.contains("prompt") || lower.contains("instruction") {
                Some("prompt_input")
            } else {
                None
            };
            if let Some(tag) = inferred {
                out.push((tag.to_string(), Some(p.name.clone())));
            }
        }
    }
    out
}

fn valid_slug(s: &str, max_len: usize) -> bool {
    !s.is_empty()
        && s.len() <= max_len
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn valid_token(s: &str, max_len: usize) -> bool {
    !s.is_empty()
        && s.len() <= max_len
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'[' | b']'))
}

fn valid_line(s: &str) -> bool {
    let trimmed = s.trim();
    !trimmed.is_empty() && s.len() <= MAX_TEXT && !s.bytes().any(|b| b < 0x20 || b == 0x7f)
}

fn valid_endpoint_path(path: &str) -> bool {
    if !path.starts_with('/')
        || path.len() > 1024
        || path.contains('?')
        || path.contains('#')
        || path.contains('\\')
        || path.bytes().any(|b| b <= 0x20 || b == 0x7f)
    {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    if lower.contains("%2e%2e") || lower.contains("%2f") || lower.contains("%5c") {
        return false;
    }
    !path.split('/').any(|seg| seg == "..")
}

fn valid_cwe(cwe: &str) -> bool {
    let Some(digits) = cwe.strip_prefix("CWE-") else {
        return false;
    };
    !digits.is_empty()
        && digits.len() <= 6
        && !digits.starts_with('0')
        && digits.bytes().all(|b| b.is_ascii_digit())
}

fn valid_owasp(cat: &str) -> bool {
    cat.starts_with("OWASP-") && valid_slug(cat, 64)
}

fn valid_date_or_rfc3339(s: &str) -> bool {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok() || DateTime::parse_from_rfc3339(s).is_ok()
}

fn reject_duplicate_json_keys(bytes: &[u8]) -> Result<()> {
    use serde::de::{MapAccess, Visitor};
    struct DupCheck;
    impl<'de> Deserialize<'de> for DupCheck {
        fn deserialize<D: serde::Deserializer<'de>>(
            deserializer: D,
        ) -> std::result::Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = DupCheck;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("any JSON value without duplicate object keys")
                }
                fn visit_bool<E>(self, _: bool) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_i64<E>(self, _: i64) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_u64<E>(self, _: u64) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_f64<E>(self, _: f64) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_str<E>(self, _: &str) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_string<E>(self, _: String) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_none<E>(self) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_unit<E>(self) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut seq: A,
                ) -> std::result::Result<DupCheck, A::Error> {
                    while seq.next_element::<DupCheck>()?.is_some() {}
                    Ok(DupCheck)
                }
                fn visit_map<A: MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> std::result::Result<DupCheck, A::Error> {
                    let mut seen = HashSet::new();
                    while let Some((k, _)) = map.next_entry::<String, DupCheck>()? {
                        if !seen.insert(k) {
                            return Err(serde::de::Error::custom("duplicate JSON object key"));
                        }
                    }
                    Ok(DupCheck)
                }
            }
            deserializer.deserialize_any(V)
        }
    }
    let _: DupCheck = serde_json::from_slice(bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_guard() -> (ScopeGuard, DateTime<Utc>) {
        let t0 = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
        let t1 = Utc.with_ymd_and_hms(2026, 11, 15, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 11, 10, 12, 0, 0).unwrap();
        let g = ScopeGuard::from_work_order(
            Some(WorkOrder {
                id: "wo-bb-2026".into(),
                authorization: "Signed Pentest ROE #2026-11".into(),
                targets: vec!["api.staging.example.com".into(), "10.20.0.0/24".into()],
                not_before: t0,
                expires_at: t1,
            }),
            InputOrigin::AuthorizedControlPlane,
        )
        .unwrap();
        (g, now)
    }

    fn sample_patterns() -> Vec<CorpusPatternRef> {
        vec![
            CorpusPatternRef {
                pattern_id: "pat-bola-id".into(),
                corpus: "owasp-api-patterns".into(),
                record_id: "API1-2023-BOLA".into(),
                cwe: "CWE-639".into(),
                owasp_category: "OWASP-API1:2023".into(),
                target_surface_tag: "id_reference".into(),
                summary: "Object-level authorization check on resource identifiers".into(),
                verification_check: "Replay request with a second test tenant's resource identifier using two authorized test accounts".into(),
                expected_safe_behavior: "Server returns 403 Forbidden or 404 Not Found without leaking cross-tenant fields".into(),
                priority: TestPriority::P1,
                pin_sha: "a".repeat(40),
                fetched_at: "2026-11-05".into(),
                pinned_citation: None,
            },
            CorpusPatternRef {
                pattern_id: "pat-ssrf-webhook".into(),
                corpus: "owasp-api-patterns".into(),
                record_id: "API7-2023-SSRF".into(),
                cwe: "CWE-918".into(),
                owasp_category: "OWASP-API7:2023".into(),
                target_surface_tag: "url_fetch".into(),
                summary: "Outbound URL parameter allowlist and loopback/metadata refusal".into(),
                verification_check: "Verify webhook configuration rejects non-HTTPS schemes and RFC 1918 / link-local literals at validation time".into(),
                expected_safe_behavior: "Server refuses disallowed schemes and internal addresses with 400 Bad Request before connecting".into(),
                priority: TestPriority::P1,
                pin_sha: "b".repeat(40),
                fetched_at: "2026-11-05".into(),
                pinned_citation: None,
            },
        ]
    }

    #[test]
    fn missing_scope_untrusted_origin_expired_window_and_out_of_scope_targets_fail_closed() {
        let (guard, now) = sample_guard();
        let endpoints = vec![ObservedEndpoint {
            id: "ep-1".into(),
            target: "api.staging.example.com".into(),
            method: "GET".into(),
            path: "/v1/orders".into(),
            auth_surface: AuthSurface::BearerToken,
            parameters: vec![EndpointParam {
                name: "order_id".into(),
                location: ParameterLocation::Query,
                surface_tag: None,
            }],
        }];
        let patterns = sample_patterns();

        // 1. Missing scope -> loud refusal
        assert!(matches!(
            CyberBlackboxReport::plan(
                "plan-1",
                "Staging API review",
                &endpoints,
                &patterns,
                None,
                InputOrigin::AuthorizedControlPlane,
                now,
            ),
            Err(BlackboxError::Scope(ScopeError::MissingScope))
        ));

        // 2. Untrusted repository origin -> loud refusal
        assert!(matches!(
            CyberBlackboxReport::plan(
                "plan-1",
                "Staging API review",
                &endpoints,
                &patterns,
                Some(&guard),
                InputOrigin::UntrustedRepository,
                now,
            ),
            Err(BlackboxError::Scope(ScopeError::UntrustedInput))
        ));

        // 3. Expired window -> loud refusal
        let expired = Utc.with_ymd_and_hms(2026, 11, 20, 0, 0, 0).unwrap();
        assert!(matches!(
            CyberBlackboxReport::plan(
                "plan-1",
                "Staging API review",
                &endpoints,
                &patterns,
                Some(&guard),
                InputOrigin::AuthorizedControlPlane,
                expired,
            ),
            Err(BlackboxError::Scope(ScopeError::Expired))
        ));

        // 4. Out-of-scope target -> loud refusal
        let out_of_scope = vec![ObservedEndpoint {
            id: "ep-evil".into(),
            target: "prod.example.com".into(),
            method: "GET".into(),
            path: "/v1/orders".into(),
            auth_surface: AuthSurface::BearerToken,
            parameters: vec![],
        }];
        assert!(matches!(
            CyberBlackboxReport::plan(
                "plan-1",
                "Staging API review",
                &out_of_scope,
                &patterns,
                Some(&guard),
                InputOrigin::AuthorizedControlPlane,
                now,
            ),
            Err(BlackboxError::Scope(ScopeError::OutOfScope))
        ));
    }

    #[test]
    fn authorized_plan_maps_surface_cites_corpus_patterns_echoes_scope_and_seals_jcs() {
        let (guard, now) = sample_guard();
        let endpoints = vec![
            ObservedEndpoint {
                id: "ep-orders".into(),
                target: "api.staging.example.com".into(),
                method: "GET".into(),
                path: "/v1/orders".into(),
                auth_surface: AuthSurface::BearerToken,
                parameters: vec![EndpointParam {
                    name: "order_id".into(),
                    location: ParameterLocation::Query,
                    surface_tag: None,
                }],
            },
            ObservedEndpoint {
                id: "ep-webhook".into(),
                target: "10.20.0.15".into(),
                method: "POST".into(),
                path: "/v1/webhooks".into(),
                auth_surface: AuthSurface::BearerToken,
                parameters: vec![EndpointParam {
                    name: "callback_url".into(),
                    location: ParameterLocation::BodyJson,
                    surface_tag: None,
                }],
            },
            ObservedEndpoint {
                id: "ep-health".into(),
                target: "api.staging.example.com".into(),
                method: "GET".into(),
                path: "/healthz".into(),
                auth_surface: AuthSurface::Unauthenticated,
                parameters: vec![],
            },
        ];
        let patterns = sample_patterns();

        let report = CyberBlackboxReport::plan(
            "plan-staging-2026",
            "Staging API surface mapping",
            &endpoints,
            &patterns,
            Some(&guard),
            InputOrigin::AuthorizedControlPlane,
            now,
        )
        .unwrap();

        assert_eq!(report.surface_map.len(), 3);
        assert_eq!(report.test_cases.len(), 2);
        assert_eq!(report.abstentions.len(), 1);
        assert_eq!(report.abstentions[0].endpoint_id, "ep-health");

        for tc in &report.test_cases {
            assert!(tc.cwe.starts_with("CWE-"));
            assert!(tc.owasp_category.starts_with("OWASP-"));
            assert_eq!(tc.pattern.pin_sha.len(), 40);
        }
        assert!(report.authorization_statement.contains("wo-bb-2026"));
        assert_eq!(report.scope.id, "wo-bb-2026");

        let jcs = report.to_jcs().unwrap();
        let verified = CyberBlackboxReport::verify_json(&jcs).unwrap();
        assert_eq!(verified.report_sha256, report.report_sha256);
        verified
            .recheck_scope(&guard, InputOrigin::AuthorizedControlPlane, now)
            .unwrap();
    }
}
