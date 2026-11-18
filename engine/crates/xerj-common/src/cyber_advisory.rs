//! Evidence-resolved vulnerability context and citation-checked advisories.

use crate::hub_evidence::{sha256, PinnedCitation};
use serde::{Deserialize, Serialize};

const VERSION: &str = "xerj-cyber-advisory/v1";
const MAX_REPORT_BYTES: usize = 4 * 1024 * 1024;
const BANNED_PHRASES: &[&str] = &[
    "todo",
    "tbd",
    "lorem ipsum",
    "insert here",
    "placeholder",
    "as an ai",
    "world-class",
    "best-in-class",
    "synerg",
    "cutting-edge",
    "comprehensive solution",
];

#[derive(Debug, thiserror::Error)]
pub enum AdvisoryError {
    #[error("unknown or malformed advisory identifier: {0}")]
    UnknownAdvisoryId(String),
    #[error("invalid advisory data: {0}")]
    Invalid(&'static str),
    #[error("advisory citation check failed: {0}")]
    CitationCheckFailed(&'static str),
    #[error("advisory JSON: {0}")]
    Json(#[from] serde_json::Error),
}

fn require(ok: bool, reason: &'static str) -> Result<(), AdvisoryError> {
    if ok {
        Ok(())
    } else {
        Err(AdvisoryError::Invalid(reason))
    }
}

fn citation_failure(reason: &'static str) -> AdvisoryError {
    AdvisoryError::CitationCheckFailed(reason)
}
fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>, AdvisoryError> {
    Ok(serde_json_canonicalizer::to_vec(value)?)
}
fn lower_hex(input: &str, len: usize) -> bool {
    input.len() == len
        && input
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn identifier(id: &str) -> bool {
    if let Some(rest) = id.strip_prefix("CVE-") {
        let mut parts = rest.split('-');
        let year = parts.next().unwrap_or("");
        let number = parts.next().unwrap_or("");
        return year.len() == 4
            && year.bytes().all(|b| b.is_ascii_digit())
            && number.len() >= 4
            && number.bytes().all(|b| b.is_ascii_digit())
            && parts.next().is_none();
    }
    if let Some(rest) = id.strip_prefix("GHSA-") {
        let parts: Vec<_> = rest.split('-').collect();
        return parts.len() == 3
            && parts.iter().all(|part| {
                part.len() == 4
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            });
    }
    false
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvisoryCitation {
    pub corpus: String,
    pub record_id: String,
    pub pin_sha: String,
    pub fetched_at: String,
    pub pinned_citation: PinnedCitation,
}

impl AdvisoryCitation {
    pub fn reference(&self) -> String {
        format!(
            "[{}:{}@{}]",
            self.corpus,
            self.record_id,
            &self.pin_sha[..8]
        )
    }

    fn validate(&self) -> Result<(), AdvisoryError> {
        require(
            !self.corpus.trim().is_empty()
                && !self.record_id.trim().is_empty()
                && lower_hex(&self.pin_sha, 40)
                && self.pin_sha == self.pinned_citation.commit
                && !self.fetched_at.trim().is_empty(),
            "invalid advisory citation",
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionEvidence {
    pub value: String,
    pub citation: AdvisoryCitation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AffectedCode {
    pub file: String,
    pub line_start: u64,
    pub line_end: u64,
    pub symbol: Option<String>,
    pub citation: AdvisoryCitation,
}

impl AffectedCode {
    pub fn location(&self) -> String {
        match &self.symbol {
            Some(symbol) => format!(
                "{}:{}-{} ({})",
                self.file, self.line_start, self.line_end, symbol
            ),
            None => format!("{}:{}-{}", self.file, self.line_start, self.line_end),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixCommit {
    pub commit: String,
    pub citation: AdvisoryCitation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimilarPastBug {
    pub advisory_id: String,
    pub summary: String,
    pub cwe: Option<String>,
    pub citation: AdvisoryCitation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvisoryCorpusRecord {
    pub advisory_id: String,
    pub aliases: Vec<String>,
    pub corpus: String,
    pub record_id: String,
    pub summary: String,
    pub impact: String,
    pub cwe: Option<String>,
    pub affected_versions: Vec<String>,
    pub patched_versions: Vec<String>,
    pub affected_code: Vec<AffectedCode>,
    pub fix_commit: Option<String>,
    pub similar_past_bugs: Vec<SimilarPastBug>,
    pub workarounds: Vec<String>,
    pub citation: AdvisoryCitation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CveContext {
    pub advisory_id: String,
    pub aliases: Vec<String>,
    pub cwe: Option<String>,
    pub summary: String,
    pub impact: String,
    pub affected_versions: Vec<VersionEvidence>,
    pub patched_versions: Vec<VersionEvidence>,
    pub affected_code: Vec<AffectedCode>,
    pub fix_commit: Option<FixCommit>,
    pub similar_past_bugs: Vec<SimilarPastBug>,
    pub workarounds: Vec<String>,
    pub citations: Vec<AdvisoryCitation>,
}

impl CveContext {
    pub fn resolve(
        requested_id: &str,
        records: &[AdvisoryCorpusRecord],
    ) -> Result<Self, AdvisoryError> {
        let trimmed = requested_id.trim();
        let normalized = if trimmed
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("GHSA-"))
        {
            format!("GHSA-{}", trimmed[5..].to_ascii_lowercase())
        } else {
            trimmed.to_ascii_uppercase()
        };
        if !identifier(&normalized) {
            return Err(AdvisoryError::UnknownAdvisoryId(requested_id.into()));
        }
        let record = records
            .iter()
            .find(|record| {
                record.advisory_id.eq_ignore_ascii_case(&normalized)
                    || record
                        .aliases
                        .iter()
                        .any(|alias| alias.eq_ignore_ascii_case(&normalized))
            })
            .ok_or_else(|| AdvisoryError::UnknownAdvisoryId(requested_id.into()))?;
        record.validate()?;
        let wrap_versions = |values: &[String]| {
            values
                .iter()
                .map(|value| VersionEvidence {
                    value: value.clone(),
                    citation: record.citation.clone(),
                })
                .collect()
        };
        let context = Self {
            advisory_id: record.advisory_id.clone(),
            aliases: record.aliases.clone(),
            cwe: record.cwe.clone(),
            summary: record.summary.clone(),
            impact: record.impact.clone(),
            affected_versions: wrap_versions(&record.affected_versions),
            patched_versions: wrap_versions(&record.patched_versions),
            affected_code: record.affected_code.clone(),
            fix_commit: record.fix_commit.as_ref().map(|commit| FixCommit {
                commit: commit.clone(),
                citation: record.citation.clone(),
            }),
            similar_past_bugs: record.similar_past_bugs.clone(),
            workarounds: record.workarounds.clone(),
            citations: deduplicate_citations(
                std::iter::once(record.citation.clone())
                    .chain(record.affected_code.iter().map(|v| v.citation.clone()))
                    .chain(record.similar_past_bugs.iter().map(|v| v.citation.clone())),
            ),
        };
        context.validate()?;
        Ok(context)
    }

    fn validate(&self) -> Result<(), AdvisoryError> {
        require(
            identifier(&self.advisory_id)
                && !self.summary.trim().is_empty()
                && !self.impact.trim().is_empty()
                && !self.citations.is_empty(),
            "invalid CVE context",
        )?;
        for citation in &self.citations {
            citation.validate()?;
        }
        let references: Vec<_> = self
            .citations
            .iter()
            .map(AdvisoryCitation::reference)
            .collect();
        for value in self.affected_versions.iter().chain(&self.patched_versions) {
            value.citation.validate()?;
            require(
                !value.value.trim().is_empty() && references.contains(&value.citation.reference()),
                "version missing context citation",
            )?;
        }
        for location in &self.affected_code {
            location.citation.validate()?;
            require(
                !location.file.trim().is_empty()
                    && location.line_start > 0
                    && location.line_start <= location.line_end
                    && references.contains(&location.citation.reference()),
                "invalid affected code location",
            )?;
        }
        if let Some(fix) = &self.fix_commit {
            fix.citation.validate()?;
            require(
                lower_hex(&fix.commit, 40) && references.contains(&fix.citation.reference()),
                "invalid fix commit",
            )?;
        }
        for bug in &self.similar_past_bugs {
            bug.citation.validate()?;
            require(
                !bug.advisory_id.trim().is_empty()
                    && !bug.summary.trim().is_empty()
                    && references.contains(&bug.citation.reference()),
                "invalid similar bug",
            )?;
        }
        Ok(())
    }
}

impl AdvisoryCorpusRecord {
    fn validate(&self) -> Result<(), AdvisoryError> {
        require(
            identifier(&self.advisory_id)
                && self.aliases.iter().all(|id| identifier(id))
                && !self.summary.trim().is_empty()
                && !self.impact.trim().is_empty()
                && self.corpus == self.citation.corpus
                && self.record_id == self.citation.record_id,
            "invalid advisory corpus record",
        )?;
        self.citation.validate()?;
        if let Some(commit) = &self.fix_commit {
            require(
                lower_hex(commit, 40),
                "fix commit must be a full lowercase SHA",
            )?;
        }
        Ok(())
    }
}

fn deduplicate_citations(
    items: impl IntoIterator<Item = AdvisoryCitation>,
) -> Vec<AdvisoryCitation> {
    let mut result = Vec::new();
    for item in items {
        if !result
            .iter()
            .any(|known: &AdvisoryCitation| known.reference() == item.reference())
        {
            result.push(item);
        }
    }
    result.sort_by_key(AdvisoryCitation::reference);
    result
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvisoryScoreSummary {
    pub cvss_vector: Option<String>,
    pub cvss_score: Option<f64>,
    pub epss_probability: Option<f64>,
    pub kev_known_exploited: Option<bool>,
}

impl AdvisoryScoreSummary {
    fn validate(&self) -> Result<(), AdvisoryError> {
        require(
            self.cvss_score
                .is_none_or(|v| v.is_finite() && (0.0..=10.0).contains(&v))
                && self
                    .epss_probability
                    .is_none_or(|v| v.is_finite() && (0.0..=1.0).contains(&v)),
            "invalid advisory score summary",
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CyberAdvisory {
    pub report_version: String,
    pub advisory_id: String,
    pub summary: String,
    pub impact: String,
    pub affected_versions: Vec<String>,
    pub patches: Vec<String>,
    pub workarounds: Vec<String>,
    pub score_summary: Option<AdvisoryScoreSummary>,
    pub citations: Vec<AdvisoryCitation>,
    pub rendered_markdown: String,
    pub report_sha256: String,
}

#[derive(Serialize)]
struct ReportPayload<'a> {
    report_version: &'a str,
    advisory_id: &'a str,
    summary: &'a str,
    impact: &'a str,
    affected_versions: &'a [String],
    patches: &'a [String],
    workarounds: &'a [String],
    score_summary: &'a Option<AdvisoryScoreSummary>,
    citations: &'a [AdvisoryCitation],
    rendered_markdown: &'a str,
}

impl CyberAdvisory {
    pub fn draft(
        context: &CveContext,
        score_summary: Option<AdvisoryScoreSummary>,
    ) -> Result<Self, AdvisoryError> {
        context.validate()?;
        if let Some(scores) = &score_summary {
            scores.validate()?;
        }
        let primary = context
            .citations
            .first()
            .ok_or(AdvisoryError::Invalid("context has no primary citation"))?
            .reference();
        let affected_versions: Vec<_> = context
            .affected_versions
            .iter()
            .map(|v| v.value.clone())
            .collect();
        let patches: Vec<_> = context
            .patched_versions
            .iter()
            .map(|v| format!("Patched in version {}", v.value))
            .collect::<Vec<_>>()
            .into_iter()
            .chain(
                context
                    .fix_commit
                    .iter()
                    .map(|fix| format!("Fix commit {}", fix.commit)),
            )
            .collect();
        let workarounds = if context.workarounds.is_empty() {
            vec!["No workaround is recorded in the cited corpus".into()]
        } else {
            context.workarounds.clone()
        };
        let mut advisory = Self {
            report_version: VERSION.into(),
            advisory_id: context.advisory_id.clone(),
            summary: context.summary.clone(),
            impact: context.impact.clone(),
            affected_versions: affected_versions.clone(),
            patches: patches.clone(),
            workarounds: workarounds.clone(),
            score_summary,
            citations: context.citations.clone(),
            rendered_markdown: String::new(),
            report_sha256: String::new(),
        };
        let mut sections = vec![
            format!("# Security advisory {} {}", context.advisory_id, primary),
            format!("\n## Summary\n{} {}", context.summary, primary),
            format!("\n## Impact\n{} {}", context.impact, primary),
        ];
        let affected = if affected_versions.is_empty() {
            format!("No affected versions are listed. {primary}")
        } else {
            affected_versions
                .iter()
                .zip(&context.affected_versions)
                .map(|(version, evidence)| format!("- {version} {}", evidence.citation.reference()))
                .collect::<Vec<_>>()
                .join("\n")
        };
        sections.push(format!("\n## Affected versions\n{affected}"));
        let patch_text = if patches.is_empty() {
            format!("No patch is listed in the cited corpus. {primary}")
        } else {
            patches
                .iter()
                .enumerate()
                .map(|(index, patch)| {
                    let reference = if patch.starts_with("Fix commit") {
                        context
                            .fix_commit
                            .as_ref()
                            .map(|f| f.citation.reference())
                            .unwrap_or_else(|| primary.clone())
                    } else {
                        context
                            .patched_versions
                            .get(index)
                            .map(|v| v.citation.reference())
                            .unwrap_or_else(|| primary.clone())
                    };
                    format!("- {patch} {reference}")
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        sections.push(format!("\n## Patches\n{patch_text}"));
        sections.push(format!(
            "\n## Workarounds\n{}",
            workarounds
                .iter()
                .map(|workaround| format!("- {workaround} {primary}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
        if let Some(scores) = &advisory.score_summary {
            let line = match (
                scores.cvss_vector.as_ref(),
                scores.cvss_score,
                scores.epss_probability,
                scores.kev_known_exploited,
            ) {
                (Some(vector), Some(score), epss, kev) => format!(
                    "CVSS {vector} score {score:.1}; EPSS {}; KEV {}. {primary}",
                    epss.map(|v| format!("{v:.4}"))
                        .unwrap_or_else(|| "not supplied".into()),
                    kev.map(|v| v.to_string())
                        .unwrap_or_else(|| "not supplied".into())
                ),
                _ => format!(
                    "CVSS score {}; EPSS {}; KEV {}. {primary}",
                    scores
                        .cvss_score
                        .map(|v| format!("{v:.1}"))
                        .unwrap_or_else(|| "not supplied".into()),
                    scores
                        .epss_probability
                        .map(|v| format!("{v:.4}"))
                        .unwrap_or_else(|| "not supplied".into()),
                    scores
                        .kev_known_exploited
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "not supplied".into())
                ),
            };
            sections.push(format!("\n## Score summary\n{line}"));
        }
        sections.push(format!(
            "\n## Citations\n{}",
            context
                .citations
                .iter()
                .map(|citation| format!("- {}", citation.reference()))
                .collect::<Vec<_>>()
                .join("\n")
        ));
        advisory.rendered_markdown = sections.join("\n");
        verify_advisory_prose(&advisory)?;
        advisory.report_sha256 = advisory.digest()?;
        advisory.to_jcs()?;
        Ok(advisory)
    }

    fn digest(&self) -> Result<String, AdvisoryError> {
        Ok(sha256(&canonical(&ReportPayload {
            report_version: &self.report_version,
            advisory_id: &self.advisory_id,
            summary: &self.summary,
            impact: &self.impact,
            affected_versions: &self.affected_versions,
            patches: &self.patches,
            workarounds: &self.workarounds,
            score_summary: &self.score_summary,
            citations: &self.citations,
            rendered_markdown: &self.rendered_markdown,
        })?))
    }

    fn validate(&self) -> Result<(), AdvisoryError> {
        require(
            self.report_version == VERSION
                && identifier(&self.advisory_id)
                && !self.summary.trim().is_empty()
                && !self.impact.trim().is_empty()
                && !self.citations.is_empty(),
            "advisory report metadata",
        )?;
        for citation in &self.citations {
            citation.validate()?;
        }
        if let Some(score) = &self.score_summary {
            score.validate()?;
        }
        verify_advisory_prose(self)?;
        Ok(())
    }

    pub fn to_jcs(&self) -> Result<Vec<u8>, AdvisoryError> {
        self.validate()?;
        if self.report_sha256 != self.digest()? {
            return Err(AdvisoryError::Invalid("report digest mismatch"));
        }
        let bytes = canonical(self)?;
        require(bytes.len() <= MAX_REPORT_BYTES, "report byte limit")?;
        Ok(bytes)
    }

    pub fn verify_json(json: &[u8]) -> Result<Self, AdvisoryError> {
        require(json.len() <= MAX_REPORT_BYTES, "report byte limit")?;
        let advisory: Self = serde_json::from_slice(json)?;
        advisory.to_jcs()?;
        Ok(advisory)
    }
}

fn references_in(line: &str) -> Vec<&str> {
    let mut refs = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('[') {
        rest = &rest[start + 1..];
        if let Some(end) = rest.find(']') {
            refs.push(&rest[..end]);
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
    refs
}

fn is_fact_line(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty() && !trimmed.starts_with('#') && trimmed != "---"
}

pub fn verify_advisory_prose(advisory: &CyberAdvisory) -> Result<(), AdvisoryError> {
    let lowered = advisory.rendered_markdown.to_ascii_lowercase();
    if BANNED_PHRASES.iter().any(|phrase| lowered.contains(phrase)) {
        return Err(citation_failure("banned filler or placeholder phrase"));
    }
    let allowed: Vec<_> = advisory
        .citations
        .iter()
        .map(AdvisoryCitation::reference)
        .collect();
    let mut section: Option<&str> = None;
    for line in advisory.rendered_markdown.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            section = Some(heading);
            continue;
        }
        if !is_fact_line(trimmed) || trimmed.starts_with("# Security advisory") {
            continue;
        }
        let refs = references_in(trimmed);
        if refs.is_empty() {
            return Err(citation_failure("factual claim lacks citation"));
        }
        if refs.iter().any(|reference| {
            !allowed
                .iter()
                .any(|known| known == &format!("[{reference}]"))
        }) {
            return Err(citation_failure("citation not present in verified context"));
        }
        if matches!(
            section,
            Some(
                "Summary"
                    | "Impact"
                    | "Affected versions"
                    | "Patches"
                    | "Workarounds"
                    | "Score summary"
            )
        ) {
            // Requiring a valid citation on every factual claim line makes version and hash claims fail closed.
            if trimmed.contains("0x") && refs.is_empty() {
                return Err(citation_failure("uncited commit hash"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn citation(corpus: &str, id: &str) -> AdvisoryCitation {
        let summary = format!("{id} evidence");
        let bytes = format!("{summary}\n").into_bytes();
        AdvisoryCitation {
            corpus: corpus.into(),
            record_id: id.into(),
            pin_sha: "0123456789abcdef0123456789abcdef01234567".into(),
            fetched_at: "2026-11-10T00:00:00Z".into(),
            pinned_citation: PinnedCitation::from_bytes(
                corpus.into(),
                "security/corpus".into(),
                "0123456789abcdef0123456789abcdef01234567".into(),
                "record.txt".into(),
                &bytes,
                0..summary.len(),
            )
            .unwrap(),
        }
    }

    fn record() -> AdvisoryCorpusRecord {
        let primary = citation("rustsec", "RUSTSEC-2026-0001");
        AdvisoryCorpusRecord {
            advisory_id: "CVE-2026-1234".into(),
            aliases: vec!["GHSA-abcd-ef12-3456".into()],
            corpus: "rustsec".into(),
            record_id: "RUSTSEC-2026-0001".into(),
            summary: "A parser accepts a length beyond its allocation".into(),
            impact: "A remote caller can trigger memory corruption".into(),
            cwe: Some("CWE-119".into()),
            affected_versions: vec!["1.0.0 - 1.4.2".into()],
            patched_versions: vec![">=1.4.3".into()],
            affected_code: vec![AffectedCode {
                file: "src/parser.rs".into(),
                line_start: 42,
                line_end: 48,
                symbol: Some("parse".into()),
                citation: primary.clone(),
            }],
            fix_commit: Some("abcdef0123456789abcdef0123456789abcdef01".into()),
            similar_past_bugs: vec![SimilarPastBug {
                advisory_id: "CVE-2025-9999".into(),
                summary: "A related unchecked parser length".into(),
                cwe: Some("CWE-119".into()),
                citation: primary.clone(),
            }],
            workarounds: vec!["Reject packets larger than the documented maximum".into()],
            citation: primary,
        }
    }

    #[test]
    fn resolves_cve_and_ghsa_from_pinned_corpus_only() {
        let record = record();
        let cve = CveContext::resolve("CVE-2026-1234", std::slice::from_ref(&record)).unwrap();
        let ghsa = CveContext::resolve("GHSA-ABCD-EF12-3456", &[record]).unwrap();
        assert_eq!(cve.advisory_id, ghsa.advisory_id);
        assert_eq!(
            cve.affected_code[0].location(),
            "src/parser.rs:42-48 (parse)"
        );
        assert_eq!(cve.fix_commit.as_ref().unwrap().commit.len(), 40);
        assert_eq!(cve.similar_past_bugs.len(), 1);
    }

    #[test]
    fn malformed_and_missing_identifiers_are_loud_errors() {
        assert!(matches!(
            CveContext::resolve("CVE-26-1", &[record()]),
            Err(AdvisoryError::UnknownAdvisoryId(_))
        ));
        assert!(matches!(
            CveContext::resolve("CVE-2026-9999", &[record()]),
            Err(AdvisoryError::UnknownAdvisoryId(_))
        ));
    }

    #[test]
    fn draft_is_anti_slop_checked_and_jcs_sealed() {
        let context = CveContext::resolve("CVE-2026-1234", &[record()]).unwrap();
        let advisory = CyberAdvisory::draft(
            &context,
            Some(AdvisoryScoreSummary {
                cvss_vector: Some("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H".into()),
                cvss_score: Some(9.8),
                epss_probability: Some(0.42),
                kev_known_exploited: Some(false),
            }),
        )
        .unwrap();
        assert!(advisory
            .rendered_markdown
            .contains("[rustsec:RUSTSEC-2026-0001@01234567]"));
        let json = advisory.to_jcs().unwrap();
        assert_eq!(
            CyberAdvisory::verify_json(&json).unwrap().to_jcs().unwrap(),
            json
        );
    }

    #[test]
    fn uncited_claim_and_slop_fail_closed() {
        let context = CveContext::resolve("CVE-2026-1234", &[record()]).unwrap();
        let mut advisory = CyberAdvisory::draft(&context, None).unwrap();
        advisory
            .rendered_markdown
            .push_str("\nUncited version 9.9.9 claims certainty");
        assert!(matches!(
            verify_advisory_prose(&advisory),
            Err(AdvisoryError::CitationCheckFailed(_))
        ));
        let mut slop = advisory;
        slop.rendered_markdown
            .push_str("\n- TODO cite this [rustsec:RUSTSEC-2026-0001@01234567]");
        assert!(matches!(
            verify_advisory_prose(&slop),
            Err(AdvisoryError::CitationCheckFailed(_))
        ));
    }
}
