//! Deterministic, provenance-preserving biomedical literature grading and
//! synthesis (`bio.outline` and `bio.evidence`, #2766, Epic #2769).
//!
//! This module grades supplied biomedical records by study design
//! (`StudyType` -> `EvidenceTier` `Level1A`..`Level5`) with an explicit,
//! reproducible [`GradeRule`], enforces [`LicenceClass`] redistribution gates
//! (refusing [`LicenceClass::Excluded`]), separates human clinical evidence from
//! preclinical/in-vitro findings, surfaces contradictory and adverse findings
//! explicitly, and requires a non-medical-advice boundary notice on every
//! RFC 8785 (JCS) receipt.

use crate::hub::LicenceClass;
use crate::hub_evidence::{sha256, PinnedCitation};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

pub const OUTLINE_REPORT_VERSION: &str = "xerj-bio-outline/v1";
pub const EVIDENCE_REPORT_VERSION: &str = "xerj-bio-evidence/v1";
pub const MAX_REPORT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum BioPackError {
    #[error("corpus {0} has an excluded redistribution licence")]
    ExcludedCorpusLicence(String),
    #[error("insufficient biomedical evidence: need at least {required} items, got {available}")]
    InsufficientEvidence { required: usize, available: usize },
    #[error("invalid biomedical evidence: {0}")]
    Invalid(&'static str),
    #[error("biomedical evidence JSON: {0}")]
    Json(String),
}

pub type BioEvidenceError = BioPackError;

impl From<serde_json::Error> for BioPackError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, BioPackError>;

fn require(ok: bool, reason: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(BioPackError::Invalid(reason))
    }
}

fn hex_40(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(serde_json_canonicalizer::to_vec(value)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceTier {
    Level1A,
    Level1B,
    Level2A,
    Level2B,
    Level3,
    Level4,
    Level5,
}

pub type EvidenceGrade = EvidenceTier;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StudyType {
    MetaAnalysis,
    SystematicReview,
    RandomizedControlledTrial,
    CohortStudy,
    #[serde(alias = "case-control-study")]
    CaseControl,
    #[serde(alias = "case-series-or-report")]
    CaseReport,
    /// Animal-model study. Never merged with [`StudyType::InVitro`] or
    /// [`StudyType::ExpertOpinion`]: each is its own design and its own rule.
    Preclinical,
    /// Bench or mechanistic assay.
    InVitro,
    /// Expert opinion or a narrative review: no primary data, no systematic
    /// appraisal.
    ExpertOpinion,
}

/// Rule that caps a study that is not human clinical evidence at
/// [`EvidenceTier::Level5`].
pub const NON_HUMAN_CAP_RULE_ID: &str = "bio.grade.non-human-cap.v1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FindingDirection {
    #[default]
    Supporting,
    NullEffect,
    Contradictory,
    Adverse,
}

/// Study design metadata used only to select a deterministic [`GradeRule`].
///
/// `human_clinical` must be `true` for a study of human participants: a study
/// that is not (an animal model, or a meta-analysis of animal studies) is capped
/// at [`EvidenceTier::Level5`] whatever its design, and the default is `false`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StudyMetadata {
    pub multicenter: bool,
    pub prospective: bool,
    pub well_powered: bool,
    pub human_clinical: bool,
    #[serde(default)]
    pub finding_direction: FindingDirection,
}

/// Explicit, reproducible rule receipt showing why an item received its [`EvidenceTier`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GradeRule {
    pub rule_id: String,
    pub study_type: StudyType,
    pub tier: EvidenceTier,
    pub matched_signals: Vec<String>,
    pub metadata: BTreeMap<String, String>,
    pub rationale: String,
}

pub type GradingRule = GradeRule;

fn direction_slug(direction: FindingDirection) -> &'static str {
    match direction {
        FindingDirection::Supporting => "supporting",
        FindingDirection::NullEffect => "null-effect",
        FindingDirection::Contradictory => "contradictory",
        FindingDirection::Adverse => "adverse",
    }
}

fn parse_direction(value: &str) -> Result<FindingDirection> {
    match value {
        "supporting" => Ok(FindingDirection::Supporting),
        "null-effect" => Ok(FindingDirection::NullEffect),
        "contradictory" => Ok(FindingDirection::Contradictory),
        "adverse" => Ok(FindingDirection::Adverse),
        _ => Err(BioPackError::Invalid("invalid finding-direction metadata")),
    }
}

pub fn grade_study(study_type: StudyType, metadata: &StudyMetadata) -> GradeRule {
    let mut matched_signals = vec![format!("study-type:{study_type:?}")];
    let metadata_map = BTreeMap::from([
        (
            "finding-direction".into(),
            direction_slug(metadata.finding_direction).into(),
        ),
        ("human-clinical".into(), metadata.human_clinical.to_string()),
        ("multicenter".into(), metadata.multicenter.to_string()),
        ("prospective".into(), metadata.prospective.to_string()),
        ("well-powered".into(), metadata.well_powered.to_string()),
    ]);
    let (rule_id, tier, rationale) = match study_type {
        StudyType::MetaAnalysis => (
            "bio.grade.meta-analysis.v1",
            EvidenceTier::Level1A,
            "Quantitative meta-analysis across controlled studies",
        ),
        StudyType::SystematicReview => (
            "bio.grade.systematic-review.v1",
            EvidenceTier::Level1A,
            "Systematic review with explicit inclusion criteria",
        ),
        StudyType::RandomizedControlledTrial if metadata.multicenter || metadata.well_powered => {
            matched_signals.push("design:multicenter-or-powered-rct".into());
            (
                "bio.grade.rct-powered.v1",
                EvidenceTier::Level1B,
                "Multicenter or adequately powered randomized controlled trial",
            )
        }
        StudyType::RandomizedControlledTrial => (
            "bio.grade.rct-single.v1",
            EvidenceTier::Level2A,
            "Single-center or exploratory randomized controlled trial",
        ),
        StudyType::CohortStudy if metadata.prospective && metadata.well_powered => {
            matched_signals.push("design:well-powered-prospective-cohort".into());
            (
                "bio.grade.prospective-cohort.v1",
                EvidenceTier::Level2A,
                "Prospective, adequately powered observational cohort study",
            )
        }
        StudyType::CohortStudy => (
            "bio.grade.cohort.v1",
            EvidenceTier::Level2B,
            "Observational cohort study",
        ),
        StudyType::CaseControl => (
            "bio.grade.case-control.v1",
            EvidenceTier::Level3,
            "Retrospective case-control study",
        ),
        StudyType::CaseReport => (
            "bio.grade.case-report.v1",
            EvidenceTier::Level4,
            "Case report or uncontrolled case series",
        ),
        StudyType::Preclinical => {
            matched_signals.push("scope:non-clinical-preclinical".into());
            (
                "bio.grade.preclinical.v1",
                EvidenceTier::Level5,
                "Preclinical animal model study; non-clinical evidence",
            )
        }
        StudyType::InVitro => {
            matched_signals.push("scope:non-clinical-in-vitro".into());
            (
                "bio.grade.in-vitro.v1",
                EvidenceTier::Level5,
                "In-vitro bench or mechanistic assay; non-clinical evidence",
            )
        }
        StudyType::ExpertOpinion => {
            matched_signals.push("scope:non-clinical-expert-opinion".into());
            (
                "bio.grade.expert-opinion.v1",
                EvidenceTier::Level5,
                "Expert opinion or narrative review without primary data; non-clinical evidence",
            )
        }
    };
    // Only human clinical evidence is graded above Level5. A meta-analysis of
    // animal studies is a synthesis, but not of people: it is capped, under its
    // own rule, and the receipt names the rule it would otherwise have met.
    if !metadata.human_clinical && tier < EvidenceTier::Level5 {
        matched_signals.push(format!("scope:non-human-capped-from:{rule_id}"));
        return GradeRule {
            rule_id: NON_HUMAN_CAP_RULE_ID.into(),
            study_type,
            tier: EvidenceTier::Level5,
            matched_signals,
            metadata: metadata_map,
            rationale: "Study not of human clinical evidence (for example a meta-analysis of \
                        animal studies); capped at Level5"
                .into(),
        };
    }
    GradeRule {
        rule_id: rule_id.into(),
        study_type,
        tier,
        matched_signals,
        metadata: metadata_map,
        rationale: rationale.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BioEvidenceItem {
    pub study_type: StudyType,
    pub tier: EvidenceTier,
    pub grade_rule: GradeRule,
    pub finding_direction: FindingDirection,
    pub corpus: String,
    pub record_id: String,
    pub licence_class: LicenceClass,
    pub pin_sha: String,
    pub fetched_at: String,
    pub pinned_citation: PinnedCitation,
    pub title: String,
    pub summary: String,
}

impl BioEvidenceItem {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        study_type: StudyType,
        study_metadata: StudyMetadata,
        corpus: String,
        record_id: String,
        licence_class: LicenceClass,
        pin_sha: String,
        fetched_at: String,
        pinned_citation: PinnedCitation,
        title: String,
        summary: String,
    ) -> Result<Self> {
        if licence_class == LicenceClass::Excluded {
            return Err(BioPackError::ExcludedCorpusLicence(corpus));
        }
        let grade_rule = grade_study(study_type, &study_metadata);
        let item = Self {
            study_type,
            tier: grade_rule.tier,
            grade_rule,
            finding_direction: study_metadata.finding_direction,
            corpus,
            record_id,
            licence_class,
            pin_sha,
            fetched_at,
            pinned_citation,
            title,
            summary,
        };
        item.validate()?;
        Ok(item)
    }

    fn validate(&self) -> Result<()> {
        if self.licence_class == LicenceClass::Excluded {
            return Err(BioPackError::ExcludedCorpusLicence(self.corpus.clone()));
        }
        require(
            !self.corpus.trim().is_empty()
                && !self.record_id.trim().is_empty()
                && !self.fetched_at.trim().is_empty()
                && !self.title.trim().is_empty()
                && !self.summary.trim().is_empty()
                && hex_40(&self.pin_sha)
                && self.grade_rule.rule_id.starts_with("bio.grade.")
                && !self.grade_rule.matched_signals.is_empty()
                && !self.grade_rule.rationale.trim().is_empty()
                && self.grade_rule.study_type == self.study_type
                && self.tier == self.grade_rule.tier,
            "item identity, provenance, or grade rule",
        )?;
        require(
            self.pinned_citation.corpus == self.corpus
                && self.pinned_citation.commit == self.pin_sha,
            "citation provenance does not match corpus pin",
        )?;
        let bool_value = |key: &str| {
            self.grade_rule
                .metadata
                .get(key)
                .and_then(|v| v.parse::<bool>().ok())
                .ok_or(BioPackError::Invalid("invalid grading metadata"))
        };
        let direction = self
            .grade_rule
            .metadata
            .get("finding-direction")
            .ok_or(BioPackError::Invalid("missing finding-direction metadata"))
            .and_then(|v| parse_direction(v))?;
        require(
            direction == self.finding_direction,
            "finding direction does not match grade rule metadata",
        )?;
        let metadata = StudyMetadata {
            multicenter: bool_value("multicenter")?,
            prospective: bool_value("prospective")?,
            well_powered: bool_value("well-powered")?,
            human_clinical: bool_value("human-clinical")?,
            finding_direction: direction,
        };
        let expected = grade_study(self.study_type, &metadata);
        require(
            self.grade_rule == expected,
            "grade rule is not reproducible",
        )
    }

    pub fn is_clinical(&self) -> bool {
        !matches!(
            self.study_type,
            StudyType::Preclinical | StudyType::InVitro | StudyType::ExpertOpinion
        ) && self.tier < EvidenceTier::Level5
            && self
                .grade_rule
                .metadata
                .get("human-clinical")
                .map(String::as_str)
                == Some("true")
    }

    pub fn is_contradictory_or_adverse(&self) -> bool {
        matches!(
            self.finding_direction,
            FindingDirection::Contradictory | FindingDirection::Adverse
        )
    }
}

fn item_order(a: &BioEvidenceItem, b: &BioEvidenceItem) -> std::cmp::Ordering {
    a.tier
        .cmp(&b.tier)
        .then_with(|| a.corpus.cmp(&b.corpus))
        .then_with(|| a.record_id.cmp(&b.record_id))
        .then_with(|| a.pin_sha.cmp(&b.pin_sha))
}

fn validate_disclaimer(notice: &str) -> Result<()> {
    let lower = notice.to_ascii_lowercase();
    require(
        lower.contains("literature synthesis")
            && lower.contains("not medical advice")
            && lower.contains("clinical diagnosis"),
        "missing mandatory non-medical-advice boundary notice",
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BioEvidencePacket {
    pub report_version: String,
    pub query: String,
    pub highest_clinical_tier: Option<EvidenceTier>,
    pub clinical_evidence: Vec<BioEvidenceItem>,
    pub preclinical_evidence: Vec<BioEvidenceItem>,
    pub contradictory_findings: Vec<BioEvidenceItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abstention: Option<String>,
    pub not_medical_advice_notice: String,
    pub report_sha256: String,
}

#[derive(Serialize)]
struct BioEvidencePayload<'a> {
    report_version: &'a str,
    query: &'a str,
    highest_clinical_tier: Option<EvidenceTier>,
    clinical_evidence: &'a [BioEvidenceItem],
    preclinical_evidence: &'a [BioEvidenceItem],
    contradictory_findings: &'a [BioEvidenceItem],
    #[serde(skip_serializing_if = "Option::is_none")]
    abstention: &'a Option<String>,
    not_medical_advice_notice: &'a str,
}

pub fn bio_evidence(
    query: impl Into<String>,
    evidence: Vec<BioEvidenceItem>,
    not_medical_advice_notice: impl Into<String>,
) -> Result<BioEvidencePacket> {
    BioEvidencePacket::new(query.into(), evidence, not_medical_advice_notice.into())
}

impl BioEvidencePacket {
    pub fn new(
        query: String,
        evidence: Vec<BioEvidenceItem>,
        not_medical_advice_notice: String,
    ) -> Result<Self> {
        if evidence.is_empty() {
            return Err(BioPackError::InsufficientEvidence {
                required: 1,
                available: 0,
            });
        }
        for item in &evidence {
            item.validate()?;
        }
        let mut contradictory_findings: Vec<_> = evidence
            .iter()
            .filter(|item| item.is_contradictory_or_adverse())
            .cloned()
            .collect();
        contradictory_findings.sort_by(item_order);
        let (mut clinical_evidence, mut preclinical_evidence): (Vec<_>, Vec<_>) =
            evidence.into_iter().partition(BioEvidenceItem::is_clinical);
        clinical_evidence.sort_by(item_order);
        preclinical_evidence.sort_by(item_order);
        let highest_clinical_tier = clinical_evidence.first().map(|item| item.tier);
        let abstention = if clinical_evidence.is_empty() {
            Some(
                "No qualifying human clinical evidence is available; no clinical efficacy \
                 claim is made."
                    .into(),
            )
        } else {
            None
        };
        let mut packet = Self {
            report_version: EVIDENCE_REPORT_VERSION.into(),
            query,
            highest_clinical_tier,
            clinical_evidence,
            preclinical_evidence,
            contradictory_findings,
            abstention,
            not_medical_advice_notice,
            report_sha256: String::new(),
        };
        packet.validate()?;
        packet.report_sha256 = packet.digest()?;
        Ok(packet)
    }

    fn digest(&self) -> Result<String> {
        Ok(sha256(&canonical(&BioEvidencePayload {
            report_version: &self.report_version,
            query: &self.query,
            highest_clinical_tier: self.highest_clinical_tier,
            clinical_evidence: &self.clinical_evidence,
            preclinical_evidence: &self.preclinical_evidence,
            contradictory_findings: &self.contradictory_findings,
            abstention: &self.abstention,
            not_medical_advice_notice: &self.not_medical_advice_notice,
        })?))
    }

    fn validate(&self) -> Result<()> {
        require(
            self.report_version == EVIDENCE_REPORT_VERSION
                && !self.query.trim().is_empty()
                && (!self.clinical_evidence.is_empty() || !self.preclinical_evidence.is_empty()),
            "evidence packet metadata",
        )?;
        validate_disclaimer(&self.not_medical_advice_notice)?;
        for item in &self.clinical_evidence {
            item.validate()?;
            require(item.is_clinical(), "non-clinical item in clinical section")?;
        }
        for item in &self.preclinical_evidence {
            item.validate()?;
            require(!item.is_clinical(), "clinical item in preclinical section")?;
        }
        let mut expected_contradictory: Vec<_> = self
            .clinical_evidence
            .iter()
            .chain(&self.preclinical_evidence)
            .filter(|item| item.is_contradictory_or_adverse())
            .cloned()
            .collect();
        expected_contradictory.sort_by(item_order);
        require(
            self.contradictory_findings == expected_contradictory,
            "contradictory or adverse findings mismatch",
        )?;
        require(
            self.highest_clinical_tier == self.clinical_evidence.first().map(|item| item.tier),
            "highest clinical tier mismatch",
        )?;
        if self.clinical_evidence.is_empty() {
            require(
                self.abstention
                    .as_deref()
                    .is_some_and(|s| !s.trim().is_empty()),
                "missing clinical abstention",
            )?;
        } else {
            require(self.abstention.is_none(), "unexpected clinical abstention")?;
        }
        Ok(())
    }

    pub fn to_jcs(&self) -> Result<Vec<u8>> {
        self.validate()?;
        require(
            self.report_sha256 == self.digest()?,
            "evidence packet digest mismatch",
        )?;
        let bytes = canonical(self)?;
        require(bytes.len() <= MAX_REPORT_BYTES, "report byte limit")?;
        Ok(bytes)
    }

    pub fn verify_json(json: &[u8]) -> Result<Self> {
        require(json.len() <= MAX_REPORT_BYTES, "report byte limit")?;
        let packet: Self = serde_json::from_slice(json)?;
        packet.to_jcs()?;
        Ok(packet)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BioOutlineChapter {
    pub chapter_id: String,
    pub title: String,
    pub thesis: String,
    pub clinical_evidence: Vec<BioEvidenceItem>,
    pub preclinical_evidence: Vec<BioEvidenceItem>,
    pub contradictory_findings: Vec<BioEvidenceItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abstention: Option<String>,
}

impl BioOutlineChapter {
    pub fn new(
        chapter_id: String,
        title: String,
        thesis: String,
        evidence: Vec<BioEvidenceItem>,
    ) -> Result<Self> {
        if evidence.is_empty() {
            return Err(BioPackError::InsufficientEvidence {
                required: 1,
                available: 0,
            });
        }
        for item in &evidence {
            item.validate()?;
        }
        let contradictory_findings: Vec<_> = evidence
            .iter()
            .filter(|item| item.is_contradictory_or_adverse())
            .cloned()
            .collect();
        let (clinical_evidence, preclinical_evidence): (Vec<_>, Vec<_>) =
            evidence.into_iter().partition(BioEvidenceItem::is_clinical);
        let abstention = if clinical_evidence.is_empty() {
            Some(
                "No qualifying human clinical evidence is available; no clinical efficacy \
                 claim is made."
                    .into(),
            )
        } else {
            None
        };
        let chapter = Self {
            chapter_id,
            title,
            thesis,
            clinical_evidence,
            preclinical_evidence,
            contradictory_findings,
            abstention,
        };
        chapter.normalized()
    }

    fn normalized(mut self) -> Result<Self> {
        require(
            !self.chapter_id.trim().is_empty()
                && !self.title.trim().is_empty()
                && !self.thesis.trim().is_empty()
                && (!self.clinical_evidence.is_empty() || !self.preclinical_evidence.is_empty()),
            "chapter metadata",
        )?;
        for item in &self.clinical_evidence {
            item.validate()?;
            require(item.is_clinical(), "non-clinical item in clinical section")?;
        }
        for item in &self.preclinical_evidence {
            item.validate()?;
            require(!item.is_clinical(), "clinical item in preclinical section")?;
        }
        self.clinical_evidence.sort_by(item_order);
        self.preclinical_evidence.sort_by(item_order);
        let mut expected_contradictory: Vec<_> = self
            .clinical_evidence
            .iter()
            .chain(&self.preclinical_evidence)
            .filter(|item| item.is_contradictory_or_adverse())
            .cloned()
            .collect();
        expected_contradictory.sort_by(item_order);
        self.contradictory_findings = expected_contradictory;
        require(
            self.clinical_evidence
                .iter()
                .all(|i| i.tier != EvidenceTier::Level5),
            "level-5 item cannot support clinical efficacy",
        )?;
        if self.clinical_evidence.is_empty() {
            require(
                self.abstention
                    .as_deref()
                    .is_some_and(|s| !s.trim().is_empty()),
                "missing clinical abstention",
            )?;
        } else {
            self.abstention = None;
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BioOutlineReport {
    pub report_version: String,
    pub topic: String,
    pub chapters: Vec<BioOutlineChapter>,
    pub not_medical_advice_notice: String,
    pub report_sha256: String,
}

pub type BioEvidenceReport = BioOutlineReport;

#[derive(Serialize)]
struct BioOutlinePayload<'a> {
    report_version: &'a str,
    topic: &'a str,
    chapters: &'a [BioOutlineChapter],
    not_medical_advice_notice: &'a str,
}

pub fn bio_outline(
    topic: impl Into<String>,
    chapters: Vec<BioOutlineChapter>,
    not_medical_advice_notice: impl Into<String>,
) -> Result<BioOutlineReport> {
    BioOutlineReport::new(topic.into(), chapters, not_medical_advice_notice.into())
}

impl BioOutlineReport {
    pub fn new(
        topic: String,
        chapters: Vec<BioOutlineChapter>,
        not_medical_advice_notice: String,
    ) -> Result<Self> {
        let chapters = chapters
            .into_iter()
            .map(BioOutlineChapter::normalized)
            .collect::<Result<Vec<_>>>()?;
        let mut report = Self {
            report_version: OUTLINE_REPORT_VERSION.into(),
            topic,
            chapters,
            not_medical_advice_notice,
            report_sha256: String::new(),
        };
        report.validate()?;
        report.report_sha256 = report.digest()?;
        Ok(report)
    }

    fn digest(&self) -> Result<String> {
        Ok(sha256(&canonical(&BioOutlinePayload {
            report_version: &self.report_version,
            topic: &self.topic,
            chapters: &self.chapters,
            not_medical_advice_notice: &self.not_medical_advice_notice,
        })?))
    }

    fn validate(&self) -> Result<()> {
        require(
            self.report_version == OUTLINE_REPORT_VERSION
                && !self.topic.trim().is_empty()
                && !self.chapters.is_empty(),
            "missing report metadata",
        )?;
        validate_disclaimer(&self.not_medical_advice_notice)?;
        let mut chapter_ids = HashSet::new();
        for chapter in &self.chapters {
            let normalized = chapter.clone().normalized()?;
            require(
                canonical(chapter)? == canonical(&normalized)?,
                "chapter evidence is not deterministically ordered and separated",
            )?;
        }
        require(
            self.chapters
                .iter()
                .all(|c| chapter_ids.insert(c.chapter_id.as_str())),
            "duplicate chapter id",
        )?;
        Ok(())
    }

    pub fn to_jcs(&self) -> Result<Vec<u8>> {
        self.validate()?;
        require(
            self.report_sha256 == self.digest()?,
            "report digest mismatch",
        )?;
        let bytes = canonical(self)?;
        require(bytes.len() <= MAX_REPORT_BYTES, "report byte limit")?;
        Ok(bytes)
    }

    pub fn verify_json(json: &[u8]) -> Result<Self> {
        require(json.len() <= MAX_REPORT_BYTES, "report byte limit")?;
        let report: Self = serde_json::from_slice(json)?;
        report.to_jcs()?;
        Ok(report)
    }
}

pub fn mandatory_not_medical_advice_notice() -> &'static str {
    "Literature synthesis from cited corpora only. This is not medical advice and is not a \
     clinical diagnosis; consult appropriately qualified clinicians."
}

#[cfg(test)]
mod tests {
    use super::*;

    fn citation() -> PinnedCitation {
        let bytes = b"Randomized evidence.";
        PinnedCitation::from_bytes(
            "health".into(),
            "example/health".into(),
            "a".repeat(40),
            "paper.txt".into(),
            bytes,
            0..bytes.len(),
        )
        .unwrap()
    }

    fn item(study_type: StudyType, metadata: StudyMetadata, record: &str) -> BioEvidenceItem {
        BioEvidenceItem::new(
            study_type,
            metadata,
            "health".into(),
            record.into(),
            LicenceClass::AdaptWithAttribution,
            "a".repeat(40),
            "2026-11-12T00:00:00Z".into(),
            citation(),
            "Study".into(),
            "A measured result.".into(),
        )
        .unwrap()
    }

    #[test]
    fn grading_covers_all_study_types_and_excludes_nonredistributable_corpora() {
        let clinical = StudyMetadata {
            human_clinical: true,
            ..StudyMetadata::default()
        };
        assert_eq!(
            grade_study(StudyType::MetaAnalysis, &clinical).tier,
            EvidenceTier::Level1A
        );
        assert_eq!(
            grade_study(StudyType::SystematicReview, &clinical).tier,
            EvidenceTier::Level1A
        );
        assert_eq!(
            grade_study(
                StudyType::RandomizedControlledTrial,
                &StudyMetadata {
                    multicenter: true,
                    ..clinical.clone()
                }
            )
            .tier,
            EvidenceTier::Level1B
        );
        assert_eq!(
            grade_study(StudyType::RandomizedControlledTrial, &clinical).tier,
            EvidenceTier::Level2A
        );
        assert_eq!(
            grade_study(
                StudyType::CohortStudy,
                &StudyMetadata {
                    prospective: true,
                    well_powered: true,
                    ..clinical.clone()
                }
            )
            .tier,
            EvidenceTier::Level2A
        );
        assert_eq!(
            grade_study(StudyType::CohortStudy, &clinical).tier,
            EvidenceTier::Level2B
        );
        assert_eq!(
            grade_study(StudyType::CaseControl, &clinical).tier,
            EvidenceTier::Level3
        );
        assert_eq!(
            grade_study(StudyType::CaseReport, &clinical).tier,
            EvidenceTier::Level4
        );
        assert_eq!(
            grade_study(StudyType::Preclinical, &StudyMetadata::default()).tier,
            EvidenceTier::Level5
        );
        assert_eq!(
            grade_study(StudyType::InVitro, &StudyMetadata::default()).tier,
            EvidenceTier::Level5
        );

        let error = BioEvidenceItem::new(
            StudyType::MetaAnalysis,
            clinical,
            "closed".into(),
            "record".into(),
            LicenceClass::Excluded,
            "a".repeat(40),
            "2026-11-12".into(),
            citation(),
            "Closed".into(),
            "Cannot redistribute.".into(),
        )
        .unwrap_err();
        assert_eq!(error, BioPackError::ExcludedCorpusLicence("closed".into()));
    }

    #[test]
    fn bio_evidence_and_outline_sort_tiers_surface_contradictions_and_abstain() {
        let human = StudyMetadata {
            human_clinical: true,
            ..StudyMetadata::default()
        };
        let adverse_rct = item(
            StudyType::RandomizedControlledTrial,
            StudyMetadata {
                multicenter: true,
                finding_direction: FindingDirection::Adverse,
                ..human.clone()
            },
            "b-adverse",
        );
        let packet = bio_evidence(
            "GLP-1 receptor agonist cardiovascular outcomes",
            vec![
                item(StudyType::CaseReport, human.clone(), "c"),
                adverse_rct,
                item(StudyType::MetaAnalysis, human.clone(), "a"),
                item(StudyType::Preclinical, StudyMetadata::default(), "p"),
                item(StudyType::InVitro, StudyMetadata::default(), "v"),
            ],
            mandatory_not_medical_advice_notice(),
        )
        .unwrap();
        assert_eq!(packet.highest_clinical_tier, Some(EvidenceTier::Level1A));
        assert_eq!(packet.clinical_evidence[0].tier, EvidenceTier::Level1A);
        assert_eq!(packet.preclinical_evidence.len(), 2);
        assert_eq!(packet.contradictory_findings.len(), 1);
        assert_eq!(
            packet.contradictory_findings[0].finding_direction,
            FindingDirection::Adverse
        );
        let packet_json = packet.to_jcs().unwrap();
        assert_eq!(
            BioEvidencePacket::verify_json(&packet_json)
                .unwrap()
                .report_sha256,
            packet.report_sha256
        );

        let abstaining = BioOutlineChapter::new(
            "animal".into(),
            "Animal and in-vitro evidence".into(),
            "Preclinical evidence is hypothesis-generating.".into(),
            vec![item(StudyType::Preclinical, StudyMetadata::default(), "p2")],
        )
        .unwrap();
        assert!(abstaining.abstention.is_some());
    }

    #[test]
    fn outline_disclaimer_digest_and_strict_verification_round_trip() {
        let chapter = BioOutlineChapter::new(
            "overview".into(),
            "Overview".into(),
            "The cited human synthesis reports an association.".into(),
            vec![item(
                StudyType::SystematicReview,
                StudyMetadata {
                    human_clinical: true,
                    ..StudyMetadata::default()
                },
                "a",
            )],
        )
        .unwrap();
        let report = bio_outline(
            "Research question",
            vec![chapter],
            mandatory_not_medical_advice_notice(),
        )
        .unwrap();
        let json = report.to_jcs().unwrap();
        let verified = BioOutlineReport::verify_json(&json).unwrap();
        assert_eq!(verified.to_jcs().unwrap(), json);
        assert!(bio_outline("Research question", report.chapters, "Use with care.").is_err());
        assert!(BioOutlineReport::verify_json(
            &String::from_utf8(json)
                .unwrap()
                .replacen(
                    "\"report_sha256\":",
                    "\"unexpected\":1,\"report_sha256\":",
                    1
                )
                .into_bytes()
        )
        .is_err());
    }

    /// #3300 review: `expert-opinion-or-review` used to deserialize as `InVitro`
    /// and `preclinical-or-in-vitro` as `Preclinical`, merging distinct designs
    /// in a pack whose whole point is the distinction.
    #[test]
    fn study_types_are_never_merged_by_an_alias() {
        for slug in ["expert-opinion-or-review", "preclinical-or-in-vitro"] {
            assert!(
                serde_json::from_str::<StudyType>(&format!("\"{slug}\"")).is_err(),
                "{slug} must not name a study type"
            );
        }
        assert_eq!(
            serde_json::from_str::<StudyType>("\"expert-opinion\"").unwrap(),
            StudyType::ExpertOpinion
        );
        assert_eq!(
            serde_json::from_str::<StudyType>("\"in-vitro\"").unwrap(),
            StudyType::InVitro
        );
        let rule = grade_study(StudyType::ExpertOpinion, &StudyMetadata::default());
        assert_eq!(rule.rule_id, "bio.grade.expert-opinion.v1");
        assert_eq!(rule.tier, EvidenceTier::Level5);
        assert_ne!(
            rule.rule_id,
            grade_study(StudyType::InVitro, &StudyMetadata::default()).rule_id
        );

        // Expert opinion is never clinical evidence, even for human subjects.
        let human = StudyMetadata {
            human_clinical: true,
            ..StudyMetadata::default()
        };
        let opinion = item(StudyType::ExpertOpinion, human.clone(), "opinion");
        assert!(!opinion.is_clinical());
        let packet = bio_evidence(
            "Question",
            vec![opinion],
            mandatory_not_medical_advice_notice(),
        )
        .unwrap();
        assert!(packet.clinical_evidence.is_empty());
        assert!(packet.abstention.is_some());
    }

    /// #3300 review: a meta-analysis of animal studies went into
    /// `preclinical_evidence` graded `Level1A`, and `validate()` accepted it.
    #[test]
    fn a_study_that_is_not_human_clinical_is_never_graded_above_level_5() {
        let not_human = StudyMetadata::default();
        for study_type in [
            StudyType::MetaAnalysis,
            StudyType::SystematicReview,
            StudyType::RandomizedControlledTrial,
            StudyType::CohortStudy,
            StudyType::CaseControl,
            StudyType::CaseReport,
        ] {
            let rule = grade_study(study_type, &not_human);
            assert_eq!(rule.tier, EvidenceTier::Level5, "{study_type:?}");
            assert_eq!(rule.rule_id, NON_HUMAN_CAP_RULE_ID);
            assert!(rule
                .matched_signals
                .iter()
                .any(|s| s.starts_with("scope:non-human-capped-from:bio.grade.")));
        }
        let animal_meta = item(StudyType::MetaAnalysis, not_human.clone(), "animal-meta");
        assert_eq!(animal_meta.tier, EvidenceTier::Level5);
        assert!(!animal_meta.is_clinical());
        let packet = bio_evidence(
            "Question",
            vec![animal_meta.clone()],
            mandatory_not_medical_advice_notice(),
        )
        .unwrap();
        assert_eq!(packet.preclinical_evidence.len(), 1);
        assert!(packet
            .preclinical_evidence
            .iter()
            .all(|i| i.tier == EvidenceTier::Level5));
        assert_eq!(packet.highest_clinical_tier, None);

        // The same design with human subjects keeps its grade.
        assert_eq!(
            grade_study(
                StudyType::MetaAnalysis,
                &StudyMetadata {
                    human_clinical: true,
                    ..StudyMetadata::default()
                }
            )
            .tier,
            EvidenceTier::Level1A
        );

        // A hand-edited item cannot claim Level1A for a non-human study.
        let mut forged = animal_meta;
        forged.tier = EvidenceTier::Level1A;
        forged.grade_rule.tier = EvidenceTier::Level1A;
        forged.grade_rule.rule_id = "bio.grade.meta-analysis.v1".into();
        assert!(bio_evidence(
            "Question",
            vec![forged],
            mandatory_not_medical_advice_notice()
        )
        .is_err());
    }

    #[test]
    fn pinned_citation_equality_comes_from_its_own_derive() {
        let a = citation();
        let mut b = citation();
        assert_eq!(a, b);
        b.span_sha256.push('0');
        assert_ne!(a, b);
    }
}
