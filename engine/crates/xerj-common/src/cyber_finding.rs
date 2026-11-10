//! Shared cyber pack finding schema and evidence-gated report (#2767, #2769).
//!
//! One typed finding flows between the cyber scenarios (review, black-box,
//! fuzz planning, exploitability, scoring, advisory). The report fails closed:
//! a finding that is not an explicit abstention must cite corpus evidence, and
//! scenarios that reach a target (black-box, exploitability) are only built
//! against a declared `ScopeGuard`. Nothing here sends traffic or resolves
//! names. Hashes establish integrity of the report, not truth of its claims.

use crate::cyber_scope::{InputOrigin, ScopeError, ScopeGuard, WorkOrder};
use crate::hub_evidence::{jcs_canonicalize, sha256, Confidence, PinnedCitation};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const REPORT_VERSION: &str = "xerj-cyber-finding/v1";
const MAX_REPORT_BYTES: usize = 8 * 1024 * 1024;
const MAX_FINDINGS: usize = 10_000;
const MAX_EVIDENCE: usize = 256;
const MAX_TEXT: usize = 8 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FindingError {
    #[error("invalid cyber finding `{id}`: {reason}")]
    Invalid { id: String, reason: &'static str },
    #[error("cyber finding `{id}` refused: no corpus evidence; only an abstention may omit it")]
    MissingEvidence { id: String },
    #[error("invalid cyber finding report: {0}")]
    Report(&'static str),
    #[error("cyber finding refused: {scenario:?} findings require a declared scope")]
    ScopeRequired { scenario: CyberScenario },
    #[error("cyber finding `{id}` refused: black-box findings need an endpoint location")]
    EndpointRequired { id: String },
    #[error("cyber finding `{id}` refused: {source}")]
    Scope {
        id: String,
        #[source]
        source: ScopeError,
    },
    #[error(transparent)]
    ScopeWindow(#[from] ScopeError),
    #[error("cyber finding report digest mismatch")]
    DigestMismatch,
    #[error("cyber finding report scope does not match the supplied scope")]
    ScopeMismatch,
    #[error("cyber finding JSON: {0}")]
    Json(#[from] serde_json::Error),
}

type Result<T> = std::result::Result<T, FindingError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CyberScenario {
    CodeReview,
    Blackbox,
    FuzzPlan,
    Exploitability,
    Score,
    Advisory,
}

impl CyberScenario {
    /// Scenarios that reason about a live target need an authorization scope.
    pub fn requires_scope(self) -> bool {
        matches!(self, Self::Blackbox | Self::Exploitability)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FindingLocation {
    /// Source finding from taint-guided review (#2771).
    SourceSpan {
        file: String,
        line_start: u32,
        line_end: u32,
        sink: Option<String>,
        taint_source: Option<String>,
    },
    /// Black-box finding against a bare host or IP (#2772); no port or URL.
    Endpoint {
        target: String,
        method: Option<String>,
        path: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Advisory,
    FixCommit,
    Poc,
    Pattern,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusEvidenceRef {
    pub corpus: String,
    pub record_id: String,
    pub kind: EvidenceKind,
    /// Full lowercase git commit the corpus was pinned at.
    pub pin_sha: String,
    /// `YYYY-MM-DD` or RFC 3339.
    pub fetched_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_citation: Option<PinnedCitation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    Confirmed,
    Candidate,
    NotReachable,
    Abstained,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl FindingSeverity {
    /// CVSS qualitative rating band for a base score.
    pub fn from_score(score: f64) -> Self {
        if score >= 9.0 {
            Self::Critical
        } else if score >= 7.0 {
            Self::High
        } else if score >= 4.0 {
            Self::Medium
        } else if score > 0.0 {
            Self::Low
        } else {
            Self::Info
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CyberFinding {
    pub id: String,
    pub scenario: CyberScenario,
    pub title: String,
    pub cwe: Option<String>,
    pub location: FindingLocation,
    pub severity: FindingSeverity,
    pub cvss_vector: Option<String>,
    pub cvss_score: Option<f64>,
    pub evidence: Vec<CorpusEvidenceRef>,
    pub status: FindingStatus,
    pub confidence: Option<Confidence>,
    pub abstain: bool,
    pub rationale: String,
}

fn plain(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.chars().count() <= max && !s.chars().any(char::is_control)
}

fn lower_hex40(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// `CWE-<digits>`: positive, no leading zero, at most six digits.
pub fn valid_cwe(s: &str) -> bool {
    s.strip_prefix("CWE-").is_some_and(|d| {
        (1..=6).contains(&d.len()) && d.bytes().all(|b| b.is_ascii_digit()) && !d.starts_with('0')
    })
}

fn valid_fetched_at(s: &str) -> bool {
    let date = s.len() == 10 && NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok();
    date || DateTime::parse_from_rfc3339(s).is_ok()
}

fn relative_path(file: &str) -> bool {
    plain(file, 1024)
        && !file.starts_with('/')
        && !file.contains(['\\', ':'])
        && file
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn bare_target(target: &str) -> bool {
    !target.is_empty()
        && target.len() <= 253
        && target.is_ascii()
        && !target
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace() || b"/@?#%\\".contains(&b))
}

fn url_path(path: &str) -> bool {
    path.len() <= 2048
        && path.starts_with('/')
        && path.is_ascii()
        && !path
            .bytes()
            .any(|b| b.is_ascii_control() || b == b' ' || b == b'\\' || b == b'#')
        && !path
            .split('?')
            .next()
            .unwrap_or("")
            .split('/')
            .any(|segment| segment == "..")
        && !path.to_ascii_lowercase().contains("%2e%2e")
}

impl FindingLocation {
    fn validate(&self) -> std::result::Result<(), &'static str> {
        match self {
            Self::SourceSpan {
                file,
                line_start,
                line_end,
                sink,
                taint_source,
            } => {
                if !relative_path(file) {
                    return Err("source file must be a plain relative path");
                }
                if *line_start == 0 || line_start > line_end {
                    return Err("source line range");
                }
                if [sink, taint_source]
                    .into_iter()
                    .flatten()
                    .any(|s| !plain(s, 256))
                {
                    return Err("sink or taint source text");
                }
                Ok(())
            }
            Self::Endpoint {
                target,
                method,
                path,
            } => {
                if !bare_target(target) {
                    return Err("endpoint target must be a bare host or IP");
                }
                if method.as_deref().is_some_and(|m| {
                    !matches!(
                        m,
                        "GET"
                            | "POST"
                            | "PUT"
                            | "PATCH"
                            | "DELETE"
                            | "HEAD"
                            | "OPTIONS"
                            | "TRACE"
                            | "CONNECT"
                    )
                }) {
                    return Err("endpoint method");
                }
                if !url_path(path) {
                    return Err("endpoint path");
                }
                Ok(())
            }
        }
    }
}

impl CorpusEvidenceRef {
    fn validate(&self) -> std::result::Result<(), &'static str> {
        if !plain(&self.corpus, 128) || !plain(&self.record_id, 256) {
            return Err("evidence corpus or record id");
        }
        if !lower_hex40(&self.pin_sha) {
            return Err("evidence pin must be a full lowercase git commit");
        }
        if !valid_fetched_at(&self.fetched_at) {
            return Err("evidence fetch date must be YYYY-MM-DD or RFC 3339");
        }
        if let Some(citation) = &self.pinned_citation {
            citation
                .validate()
                .map_err(|_| "pinned citation is invalid")?;
            if citation.corpus != self.corpus || citation.commit != self.pin_sha {
                return Err("pinned citation does not match evidence corpus and pin");
            }
        }
        Ok(())
    }
}

impl CyberFinding {
    fn invalid(&self, reason: &'static str) -> FindingError {
        FindingError::Invalid {
            id: self.id.clone(),
            reason,
        }
    }

    /// Fail-closed structural check. A non-abstained finding without corpus
    /// evidence is refused, never repaired; see `abstain_if_unsupported`.
    pub fn validate(&self) -> Result<()> {
        if !valid_identifier(&self.id) {
            return Err(self.invalid("id must be 1-128 of [A-Za-z0-9._:-]"));
        }
        if !plain(&self.title, 512) {
            return Err(self.invalid("title"));
        }
        if self.rationale.trim().is_empty()
            || self.rationale.chars().count() > MAX_TEXT
            || self
                .rationale
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err(self.invalid("rationale"));
        }
        if self.cwe.as_deref().is_some_and(|c| !valid_cwe(c)) {
            return Err(self.invalid("cwe must be CWE-<digits>"));
        }
        self.location.validate().map_err(|r| self.invalid(r))?;
        if let Some(vector) = &self.cvss_vector {
            let ok = (vector.starts_with("CVSS:3.1/") || vector.starts_with("CVSS:4.0/"))
                && vector.len() <= 512
                && vector
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'/' | b'.'));
            if !ok {
                return Err(self.invalid("cvss vector must be a CVSS:3.1 or CVSS:4.0 string"));
            }
        }
        if let Some(score) = self.cvss_score {
            if !score.is_finite() || !(0.0..=10.0).contains(&score) {
                return Err(self.invalid("cvss score must be in 0.0..=10.0"));
            }
            if FindingSeverity::from_score(score) != self.severity {
                return Err(self.invalid("severity disagrees with cvss score"));
            }
        }
        if self.evidence.len() > MAX_EVIDENCE {
            return Err(self.invalid("too many evidence references"));
        }
        let mut seen = HashSet::new();
        for e in &self.evidence {
            e.validate().map_err(|r| self.invalid(r))?;
            if !seen.insert((&e.corpus, &e.record_id, e.kind, &e.pin_sha)) {
                return Err(self.invalid("duplicate evidence reference"));
            }
        }
        if let Some(c) = &self.confidence {
            c.validate().map_err(|_| self.invalid("confidence"))?;
        }
        if self.abstain != (self.status == FindingStatus::Abstained) {
            return Err(self.invalid("abstain flag and status disagree"));
        }
        if !self.abstain && self.evidence.is_empty() {
            return Err(FindingError::MissingEvidence {
                id: self.id.clone(),
            });
        }
        Ok(())
    }

    /// Convert an unsupported or internally inconsistent decision into an
    /// explicit abstention instead of letting it pass as a finding.
    pub fn abstain_if_unsupported(mut self) -> Self {
        let unsupported = !self.abstain && self.evidence.is_empty();
        if unsupported || self.abstain || self.status == FindingStatus::Abstained {
            self.abstain = true;
            self.status = FindingStatus::Abstained;
        }
        if unsupported {
            self.rationale = format!("{} [abstained: no corpus evidence cited]", self.rationale);
        }
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CyberFindingReport {
    pub report_version: String,
    pub session_id: String,
    pub findings: Vec<CyberFinding>,
    /// Echo of the declared work order for scoped scenarios.
    pub scope: Option<WorkOrder>,
    pub report_sha256: String,
}

#[derive(Serialize)]
struct ReportPayload<'a> {
    report_version: &'a str,
    session_id: &'a str,
    findings: &'a [CyberFinding],
    scope: &'a Option<WorkOrder>,
}

fn needs_scope(f: &CyberFinding) -> bool {
    f.scenario.requires_scope() || matches!(f.location, FindingLocation::Endpoint { .. })
}

fn check_window(guard: &ScopeGuard, origin: InputOrigin, now: DateTime<Utc>) -> Result<()> {
    if origin != InputOrigin::AuthorizedControlPlane {
        return Err(ScopeError::UntrustedInput.into());
    }
    let scope = guard.scope();
    if now < scope.not_before {
        return Err(ScopeError::NotYetValid.into());
    }
    if now > scope.expires_at {
        return Err(ScopeError::Expired.into());
    }
    Ok(())
}

fn authorize_endpoints(
    findings: &[CyberFinding],
    guard: &ScopeGuard,
    origin: InputOrigin,
    now: DateTime<Utc>,
) -> Result<()> {
    check_window(guard, origin, now)?;
    for f in findings {
        match &f.location {
            FindingLocation::Endpoint { target, .. } => {
                guard
                    .authorize(target, origin, now)
                    .map_err(|source| FindingError::Scope {
                        id: f.id.clone(),
                        source,
                    })?;
            }
            FindingLocation::SourceSpan { .. } if f.scenario == CyberScenario::Blackbox => {
                return Err(FindingError::EndpointRequired { id: f.id.clone() });
            }
            FindingLocation::SourceSpan { .. } => {}
        }
    }
    Ok(())
}

impl CyberFindingReport {
    fn digest(&self) -> Result<String> {
        Ok(sha256(
            &jcs_canonicalize(&ReportPayload {
                report_version: &self.report_version,
                session_id: &self.session_id,
                findings: &self.findings,
                scope: &self.scope,
            })
            .map_err(|_| FindingError::Report("canonicalization"))?,
        ))
    }

    fn check(&self) -> Result<()> {
        if self.report_version != REPORT_VERSION
            || !plain(&self.session_id, 256)
            || self.findings.len() > MAX_FINDINGS
        {
            return Err(FindingError::Report("report metadata"));
        }
        let mut ids = HashSet::new();
        for f in &self.findings {
            if !ids.insert(&f.id) {
                return Err(FindingError::Report("duplicate finding id"));
            }
            f.validate()?;
            if self.scope.is_none() && needs_scope(f) {
                return Err(FindingError::ScopeRequired {
                    scenario: f.scenario,
                });
            }
            if f.scenario == CyberScenario::Blackbox
                && !matches!(f.location, FindingLocation::Endpoint { .. })
            {
                return Err(FindingError::EndpointRequired { id: f.id.clone() });
            }
        }
        if let Some(scope) = &self.scope {
            if scope.id.trim().is_empty()
                || scope.authorization.trim().is_empty()
                || scope.targets.is_empty()
                || scope.not_before > scope.expires_at
            {
                return Err(FindingError::Report("scope echo"));
            }
        }
        Ok(())
    }

    fn seal(
        session_id: String,
        findings: Vec<CyberFinding>,
        scope: Option<WorkOrder>,
    ) -> Result<Self> {
        let mut report = Self {
            report_version: REPORT_VERSION.into(),
            session_id,
            findings: findings
                .into_iter()
                .map(CyberFinding::abstain_if_unsupported)
                .collect(),
            scope,
            report_sha256: String::new(),
        };
        report.check()?;
        report.report_sha256 = report.digest()?;
        Ok(report)
    }

    /// Build an unscoped report. Unsupported findings become abstentions;
    /// scenarios or locations that reach a target are refused without a scope.
    pub fn finalize(session_id: String, findings: Vec<CyberFinding>) -> Result<Self> {
        Self::seal(session_id, findings, None)
    }

    /// Build a report whose endpoint targets are all authorized by `guard`
    /// at `now`, echoing the declared work order. `origin` must come from the
    /// authenticated transport boundary. A missing, expired, untrusted, or
    /// out-of-scope declaration refuses the whole report.
    pub fn verify_with_scope(
        session_id: String,
        findings: Vec<CyberFinding>,
        guard: &ScopeGuard,
        origin: InputOrigin,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        authorize_endpoints(&findings, guard, origin, now)?;
        Self::seal(session_id, findings, Some(guard.scope().clone()))
    }

    /// Re-authorize a deserialized report against the caller's own scope.
    pub fn recheck_scope(
        &self,
        guard: &ScopeGuard,
        origin: InputOrigin,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let echoed = self.scope.as_ref().ok_or(FindingError::ScopeRequired {
            scenario: CyberScenario::Blackbox,
        })?;
        let same = jcs_canonicalize(echoed).ok() == jcs_canonicalize(guard.scope()).ok();
        if !same {
            return Err(FindingError::ScopeMismatch);
        }
        authorize_endpoints(&self.findings, guard, origin, now)
    }

    /// RFC 8785 canonical bytes, after re-checking invariants and the digest.
    pub fn to_jcs(&self) -> Result<Vec<u8>> {
        self.check()?;
        if self.report_sha256 != self.digest()? {
            return Err(FindingError::DigestMismatch);
        }
        let bytes = jcs_canonicalize(self).map_err(|_| FindingError::Report("canonicalization"))?;
        if bytes.len() > MAX_REPORT_BYTES {
            return Err(FindingError::Report("report byte limit"));
        }
        Ok(bytes)
    }

    /// Parse into typed structs (unknown members and duplicate keys are
    /// errors) and re-verify every invariant and the digest.
    pub fn verify_json(json: &[u8]) -> Result<Self> {
        if json.len() > MAX_REPORT_BYTES {
            return Err(FindingError::Report("report byte limit"));
        }
        let report: Self = serde_json::from_slice(json)?;
        report.to_jcs()?;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRUSTED: InputOrigin = InputOrigin::AuthorizedControlPlane;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(seconds, 0).unwrap()
    }

    fn guard(targets: &[&str]) -> ScopeGuard {
        ScopeGuard::from_work_order(
            Some(WorkOrder {
                id: "approved-work-42".into(),
                authorization: "Owner authorizes planning for these targets only.".into(),
                targets: targets.iter().map(|s| (*s).into()).collect(),
                not_before: at(100),
                expires_at: at(200),
            }),
            TRUSTED,
        )
        .unwrap()
    }

    fn evidence() -> CorpusEvidenceRef {
        CorpusEvidenceRef {
            corpus: "rust-vulns".into(),
            record_id: "RUSTSEC-2023-0001".into(),
            kind: EvidenceKind::Advisory,
            pin_sha: "a".repeat(40),
            fetched_at: "2026-11-05".into(),
            pinned_citation: None,
        }
    }

    fn span() -> FindingLocation {
        FindingLocation::SourceSpan {
            file: "src/parse.rs".into(),
            line_start: 10,
            line_end: 14,
            sink: Some("copy_nonoverlapping".into()),
            taint_source: Some("read_packet".into()),
        }
    }

    fn endpoint(target: &str) -> FindingLocation {
        FindingLocation::Endpoint {
            target: target.into(),
            method: Some("POST".into()),
            path: "/api/v1/login".into(),
        }
    }

    fn finding(id: &str, scenario: CyberScenario, location: FindingLocation) -> CyberFinding {
        CyberFinding {
            id: id.into(),
            scenario,
            title: "Unchecked length reaches unsafe copy".into(),
            cwe: Some("CWE-787".into()),
            location,
            severity: FindingSeverity::High,
            cvss_vector: Some("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H".into()),
            cvss_score: Some(7.5),
            evidence: vec![evidence()],
            status: FindingStatus::Candidate,
            confidence: None,
            abstain: false,
            rationale: "Matches the cited advisory's sink pattern.".into(),
        }
    }

    fn all_scenarios() -> Vec<CyberFinding> {
        use CyberScenario::*;
        vec![
            finding("f-review", CodeReview, span()),
            finding("f-black", Blackbox, endpoint("example.com")),
            finding("f-fuzz", FuzzPlan, span()),
            finding("f-exploit", Exploitability, endpoint("10.0.0.5")),
            finding("f-score", Score, span()),
            finding("f-adv", Advisory, span()),
        ]
    }

    #[test]
    fn round_trip_across_all_scenarios() {
        let g = guard(&["example.com", "10.0.0.0/24"]);
        let report = CyberFindingReport::verify_with_scope(
            "session-1".into(),
            all_scenarios(),
            &g,
            TRUSTED,
            at(150),
        )
        .unwrap();
        let bytes = report.to_jcs().unwrap();
        let back = CyberFindingReport::verify_json(&bytes).unwrap();
        assert_eq!(back.findings.len(), 6);
        assert_eq!(back.report_sha256, report.report_sha256);
        assert_eq!(back.to_jcs().unwrap(), bytes);
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["findings"][1]["scenario"], "blackbox");
        assert_eq!(json["findings"][3]["scenario"], "exploitability");
        assert_eq!(json["findings"][0]["location"]["kind"], "source_span");
        assert_eq!(json["findings"][1]["location"]["kind"], "endpoint");
        assert_eq!(json["findings"][0]["status"], "candidate");
        assert_eq!(json["findings"][0]["evidence"][0]["kind"], "advisory");
    }

    #[test]
    fn unscoped_scenarios_round_trip_without_scope() {
        use CyberScenario::*;
        let findings = vec![
            finding("a", CodeReview, span()),
            finding("b", FuzzPlan, span()),
            finding("c", Score, span()),
            finding("d", Advisory, span()),
        ];
        let report = CyberFindingReport::finalize("s".into(), findings).unwrap();
        assert!(report.scope.is_none());
        let back = CyberFindingReport::verify_json(&report.to_jcs().unwrap()).unwrap();
        assert_eq!(back.report_sha256, report.report_sha256);
    }

    #[test]
    fn non_abstained_finding_without_evidence_is_refused_or_abstained() {
        let mut f = finding("f1", CyberScenario::CodeReview, span());
        f.evidence.clear();
        assert!(matches!(
            f.validate(),
            Err(FindingError::MissingEvidence { .. })
        ));
        let report = CyberFindingReport::finalize("s".into(), vec![f.clone()]).unwrap();
        let out = &report.findings[0];
        assert!(out.abstain);
        assert_eq!(out.status, FindingStatus::Abstained);
        assert!(out.rationale.contains("no corpus evidence"));
        assert!(out.validate().is_ok());

        // A hand-built report that skips finalize is rejected on verification.
        let mut raw = report.clone();
        raw.findings[0].abstain = false;
        raw.findings[0].status = FindingStatus::Confirmed;
        assert!(raw.to_jcs().is_err());
    }

    #[test]
    fn abstain_flag_and_status_must_agree() {
        let mut f = finding("f1", CyberScenario::Score, span());
        f.abstain = true;
        assert!(matches!(f.validate(), Err(FindingError::Invalid { .. })));
        let mut g = finding("f2", CyberScenario::Score, span());
        g.status = FindingStatus::Abstained;
        assert!(matches!(g.validate(), Err(FindingError::Invalid { .. })));
        let fixed = g.abstain_if_unsupported();
        assert!(fixed.abstain && fixed.validate().is_ok());
    }

    #[test]
    fn cwe_format_is_validated() {
        for ok in ["CWE-79", "CWE-787", "CWE-1321", "CWE-999999"] {
            assert!(valid_cwe(ok), "{ok}");
        }
        for bad in [
            "",
            "CWE-",
            "cwe-79",
            "CWE79",
            "CWE-0",
            "CWE-079",
            "CWE-1234567",
            "CWE-7 9",
            "CWE--1",
            "CWE-+7",
            "79",
        ] {
            assert!(!valid_cwe(bad), "{bad}");
            let mut f = finding("f1", CyberScenario::CodeReview, span());
            f.cwe = Some(bad.into());
            assert!(f.validate().is_err(), "{bad}");
        }
        let mut none = finding("f1", CyberScenario::CodeReview, span());
        none.cwe = None;
        assert!(none.validate().is_ok());
    }

    #[test]
    fn blackbox_and_exploitability_require_a_declared_scope() {
        for (scenario, target) in [
            (CyberScenario::Blackbox, "example.com"),
            (CyberScenario::Exploitability, "example.com"),
        ] {
            let f = finding("f1", scenario, endpoint(target));
            assert!(matches!(
                CyberFindingReport::finalize("s".into(), vec![f]),
                Err(FindingError::ScopeRequired { .. })
            ));
        }
        // Exploitability reasoning over source still needs the scope note.
        let f = finding("f2", CyberScenario::Exploitability, span());
        assert!(matches!(
            CyberFindingReport::finalize("s".into(), vec![f]),
            Err(FindingError::ScopeRequired { .. })
        ));
        // Any endpoint location is scoped, whatever the scenario.
        let f = finding("f3", CyberScenario::CodeReview, endpoint("example.com"));
        assert!(matches!(
            CyberFindingReport::finalize("s".into(), vec![f]),
            Err(FindingError::ScopeRequired { .. })
        ));
    }

    #[test]
    fn out_of_scope_expired_and_untrusted_requests_fail_closed() {
        let g = guard(&["example.com"]);
        let f = || {
            vec![finding(
                "f1",
                CyberScenario::Blackbox,
                endpoint("sub.example.com"),
            )]
        };
        assert!(matches!(
            CyberFindingReport::verify_with_scope("s".into(), f(), &g, TRUSTED, at(150)),
            Err(FindingError::Scope {
                source: ScopeError::OutOfScope,
                ..
            })
        ));
        let ok = || {
            vec![finding(
                "f1",
                CyberScenario::Blackbox,
                endpoint("example.com"),
            )]
        };
        assert!(matches!(
            CyberFindingReport::verify_with_scope("s".into(), ok(), &g, TRUSTED, at(201)),
            Err(FindingError::ScopeWindow(ScopeError::Expired))
        ));
        assert!(matches!(
            CyberFindingReport::verify_with_scope("s".into(), ok(), &g, TRUSTED, at(99)),
            Err(FindingError::ScopeWindow(ScopeError::NotYetValid))
        ));
        assert!(matches!(
            CyberFindingReport::verify_with_scope(
                "s".into(),
                ok(),
                &g,
                InputOrigin::UntrustedRepository,
                at(150)
            ),
            Err(FindingError::ScopeWindow(ScopeError::UntrustedInput))
        ));
        // The window applies even when no endpoint is named.
        let src = vec![finding("f2", CyberScenario::Exploitability, span())];
        assert!(CyberFindingReport::verify_with_scope(
            "s".into(),
            src.clone(),
            &g,
            TRUSTED,
            at(201)
        )
        .is_err());
        assert!(
            CyberFindingReport::verify_with_scope("s".into(), src, &g, TRUSTED, at(150)).is_ok()
        );
    }

    #[test]
    fn blackbox_requires_an_endpoint_location() {
        let g = guard(&["example.com"]);
        let f = finding("f1", CyberScenario::Blackbox, span());
        assert!(matches!(
            CyberFindingReport::verify_with_scope("s".into(), vec![f], &g, TRUSTED, at(150)),
            Err(FindingError::EndpointRequired { .. })
        ));
    }

    #[test]
    fn abstained_findings_still_cannot_name_out_of_scope_targets() {
        let g = guard(&["example.com"]);
        let mut f = finding("f1", CyberScenario::Blackbox, endpoint("evil.test"));
        f.evidence.clear();
        assert!(
            CyberFindingReport::verify_with_scope("s".into(), vec![f], &g, TRUSTED, at(150))
                .is_err()
        );
    }

    #[test]
    fn scope_is_echoed_and_covered_by_the_digest() {
        let g = guard(&["example.com"]);
        let f = vec![finding(
            "f1",
            CyberScenario::Blackbox,
            endpoint("example.com"),
        )];
        let report =
            CyberFindingReport::verify_with_scope("s".into(), f, &g, TRUSTED, at(150)).unwrap();
        let echo = report.scope.as_ref().unwrap();
        assert_eq!(echo.id, "approved-work-42");
        assert_eq!(echo.targets, ["example.com"]);
        report.recheck_scope(&g, TRUSTED, at(150)).unwrap();
        assert!(report.recheck_scope(&g, TRUSTED, at(300)).is_err());
        assert!(matches!(
            report.recheck_scope(&guard(&["other.test"]), TRUSTED, at(150)),
            Err(FindingError::ScopeMismatch)
        ));

        let mut forged = report.clone();
        forged.scope.as_mut().unwrap().targets = vec!["evil.test".into()];
        assert!(matches!(forged.to_jcs(), Err(FindingError::DigestMismatch)));
        let mut stripped = report;
        stripped.scope = None;
        assert!(matches!(
            stripped.to_jcs(),
            Err(FindingError::ScopeRequired { .. })
        ));
    }

    #[test]
    fn jcs_digest_is_deterministic_and_tamper_evident() {
        let report = CyberFindingReport::finalize(
            "s".into(),
            vec![finding("f1", CyberScenario::CodeReview, span())],
        )
        .unwrap();
        let again = CyberFindingReport::finalize(
            "s".into(),
            vec![finding("f1", CyberScenario::CodeReview, span())],
        )
        .unwrap();
        assert_eq!(report.report_sha256, again.report_sha256);
        assert_eq!(report.report_sha256.len(), 64);

        // Key order and whitespace in the transport JSON do not change the digest.
        let canonical = String::from_utf8(report.to_jcs().unwrap()).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&canonical).unwrap();
        let reversed: serde_json::Map<String, serde_json::Value> = value
            .as_object()
            .unwrap()
            .iter()
            .rev()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let pretty = serde_json::to_vec_pretty(&reversed).unwrap();
        let parsed = CyberFindingReport::verify_json(&pretty).unwrap();
        assert_eq!(parsed.report_sha256, report.report_sha256);
        assert_eq!(
            String::from_utf8(parsed.to_jcs().unwrap()).unwrap(),
            canonical
        );

        value["findings"][0]["title"] = "Edited title".into();
        assert!(matches!(
            CyberFindingReport::verify_json(&serde_json::to_vec(&value).unwrap()),
            Err(FindingError::DigestMismatch)
        ));
        value["findings"][0]["unexpected"] = true.into();
        assert!(CyberFindingReport::verify_json(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn locations_reject_traversal_control_characters_and_bad_ranges() {
        let bad_span = |file: &str, a: u32, b: u32| FindingLocation::SourceSpan {
            file: file.into(),
            line_start: a,
            line_end: b,
            sink: None,
            taint_source: None,
        };
        for loc in [
            bad_span("../etc/passwd", 1, 1),
            bad_span("src/../../x.rs", 1, 1),
            bad_span("/abs/x.rs", 1, 1),
            bad_span("C:\\x.rs", 1, 1),
            bad_span("src\\x.rs", 1, 1),
            bad_span("src/x\n.rs", 1, 1),
            bad_span("src/x\0.rs", 1, 1),
            bad_span("src//x.rs", 1, 1),
            bad_span("", 1, 1),
            bad_span("src/x.rs", 0, 1),
            bad_span("src/x.rs", 5, 4),
        ] {
            let f = finding("f1", CyberScenario::CodeReview, loc.clone());
            assert!(f.validate().is_err(), "{loc:?}");
        }
        let ep = |target: &str, method: Option<&str>, path: &str| FindingLocation::Endpoint {
            target: target.into(),
            method: method.map(Into::into),
            path: path.into(),
        };
        for loc in [
            ep("http://example.com", None, "/x"),
            ep("example.com/x", None, "/x"),
            ep("user@example.com", None, "/x"),
            ep("exa mple.com", None, "/x"),
            ep("example.com\n", None, "/x"),
            ep("", None, "/x"),
            ep("example.com", Some("get"), "/x"),
            ep("example.com", Some("BREW"), "/x"),
            ep("example.com", None, "x"),
            ep("example.com", None, "/a/../b"),
            ep("example.com", None, "/a/%2e%2E/b"),
            ep("example.com", None, "/a\r\nHost: x"),
            ep("example.com", None, "/a#frag"),
        ] {
            let f = finding("f1", CyberScenario::Blackbox, loc.clone());
            assert!(f.validate().is_err(), "{loc:?}");
        }
        let good = ep("example.com", Some("GET"), "/search?q=a..b");
        assert!(finding("f1", CyberScenario::Blackbox, good)
            .validate()
            .is_ok());
    }

    #[test]
    fn evidence_references_are_validated() {
        let mut cases = Vec::new();
        let mut e = evidence();
        e.pin_sha = "A".repeat(40);
        cases.push(e);
        let mut e = evidence();
        e.pin_sha = "abc".into();
        cases.push(e);
        let mut e = evidence();
        e.fetched_at = "2026-13-40".into();
        cases.push(e);
        let mut e = evidence();
        e.fetched_at = "yesterday".into();
        cases.push(e);
        let mut e = evidence();
        e.record_id = "  ".into();
        cases.push(e);
        let mut e = evidence();
        e.corpus = "rust\nvulns".into();
        cases.push(e);
        for e in cases {
            let mut f = finding("f1", CyberScenario::CodeReview, span());
            f.evidence = vec![e.clone()];
            assert!(f.validate().is_err(), "{e:?}");
        }
        for date in [
            "2026-11-05",
            "2026-11-05T01:02:03Z",
            "2026-11-05T01:02:03+02:00",
        ] {
            let mut e = evidence();
            e.fetched_at = date.into();
            let mut f = finding("f1", CyberScenario::CodeReview, span());
            f.evidence = vec![e];
            assert!(f.validate().is_ok(), "{date}");
        }
        let mut f = finding("f1", CyberScenario::CodeReview, span());
        f.evidence = vec![evidence(), evidence()];
        assert!(f.validate().is_err());
    }

    #[test]
    fn pinned_citation_must_match_the_evidence_pin() {
        let bytes = b"fn main() {}\nunsafe { copy(); }\n";
        let citation = PinnedCitation::from_bytes(
            "rust-vulns".into(),
            "rust-lang/advisory-db".into(),
            "a".repeat(40),
            "crates/x/RUSTSEC-2023-0001.md".into(),
            bytes,
            13..32,
        )
        .unwrap();
        let mut f = finding("f1", CyberScenario::CodeReview, span());
        f.evidence[0].pinned_citation = Some(citation.clone());
        assert!(f.validate().is_ok());
        f.evidence[0].pin_sha = "b".repeat(40);
        assert!(f.validate().is_err());
        f.evidence[0].pin_sha = "a".repeat(40);
        f.evidence[0].corpus = "other".into();
        assert!(f.validate().is_err());
    }

    #[test]
    fn severity_score_and_vector_are_consistent() {
        let mut f = finding("f1", CyberScenario::Score, span());
        f.cvss_score = Some(9.8);
        assert!(f.validate().is_err());
        f.severity = FindingSeverity::Critical;
        assert!(f.validate().is_ok());
        for bad in [f64::NAN, f64::INFINITY, -0.1, 10.1] {
            f.cvss_score = Some(bad);
            assert!(f.validate().is_err());
        }
        f.cvss_score = Some(0.0);
        f.severity = FindingSeverity::Info;
        assert!(f.validate().is_ok());
        f.cvss_vector = Some("CVSS:2.0/AV:N".into());
        assert!(f.validate().is_err());
        f.cvss_vector = Some("CVSS:4.0/AV:N\n".into());
        assert!(f.validate().is_err());
    }

    #[test]
    fn duplicate_ids_and_bad_metadata_are_refused() {
        let a = finding("same", CyberScenario::CodeReview, span());
        let b = finding("same", CyberScenario::Advisory, span());
        assert!(matches!(
            CyberFindingReport::finalize("s".into(), vec![a, b]),
            Err(FindingError::Report(_))
        ));
        let f = finding("ok", CyberScenario::CodeReview, span());
        assert!(CyberFindingReport::finalize(" ".into(), vec![f.clone()]).is_err());
        let mut bad = f;
        bad.id = "has space".into();
        assert!(CyberFindingReport::finalize("s".into(), vec![bad]).is_err());
    }
}
