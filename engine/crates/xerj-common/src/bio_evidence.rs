//! Deterministic, provenance-preserving biomedical literature synthesis.
//!
//! This module classifies and grades supplied evidence; it does not make a
//! diagnosis, recommend treatment, or imply that search-result snippets are a
//! substitute for source review.

use crate::hub::LicenceClass;
use crate::hub_evidence::PinnedCitation;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const REPORT_VERSION: &str = "xerj-bio-evidence/v1";
const MAX_REPORT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum BioEvidenceError {
    #[error("corpus {0} has an excluded redistribution licence")]
    ExcludedCorpusLicence(String),
    #[error("invalid biomedical evidence: {0}")]
    Invalid(&'static str),
    #[error("biomedical evidence JSON: {0}")]
    Json(String),
}

impl From<serde_json::Error> for BioEvidenceError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value.to_string())
    }
}

type Result<T> = std::result::Result<T, BioEvidenceError>;

fn require(ok: bool, reason: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(BioEvidenceError::Invalid(reason))
    }
}

fn hex_40(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(serde_json_canonicalizer::to_vec(value)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceGrade {
    GradeA,
    GradeB,
    GradeC,
    GradeD,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StudyType {
    MetaAnalysis,
    SystematicReview,
    RandomizedControlledTrial,
    CohortStudy,
    CaseControlStudy,
    CaseSeriesOrReport,
    PreclinicalOrInVitro,
    ExpertOpinionOrReview,
}

/// Study metadata used only to select a deterministic grading rule.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StudyMetadata {
    pub multicenter: bool,
    pub prospective: bool,
    pub well_powered: bool,
    pub human_clinical: bool,
}

/// The exact reason an item received its grade. Metadata is a sorted map so
/// receipts remain byte-stable across callers and platforms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GradingRule {
    pub rule_id: String,
    pub matched_signals: Vec<String>,
    pub metadata: BTreeMap<String, String>,
    pub resulting_grade: EvidenceGrade,
}

pub fn grade_study(study_type: StudyType, metadata: &StudyMetadata) -> GradingRule {
    let mut matched_signals = vec![format!("study-type:{study_type:?}")];
    let metadata_map = BTreeMap::from([
        ("human-clinical".into(), metadata.human_clinical.to_string()),
        ("multicenter".into(), metadata.multicenter.to_string()),
        ("prospective".into(), metadata.prospective.to_string()),
        ("well-powered".into(), metadata.well_powered.to_string()),
    ]);
    let (rule_id, resulting_grade) = match study_type {
        StudyType::MetaAnalysis => ("bio.grade.meta-analysis.v1", EvidenceGrade::GradeA),
        StudyType::SystematicReview => ("bio.grade.systematic-review.v1", EvidenceGrade::GradeA),
        StudyType::RandomizedControlledTrial if metadata.multicenter => {
            matched_signals.push("design:multicenter-rct".into());
            ("bio.grade.multicenter-rct.v1", EvidenceGrade::GradeA)
        }
        StudyType::RandomizedControlledTrial => ("bio.grade.single-rct.v1", EvidenceGrade::GradeB),
        StudyType::CohortStudy if metadata.prospective && metadata.well_powered => {
            matched_signals.push("design:well-powered-prospective-cohort".into());
            ("bio.grade.prospective-cohort.v1", EvidenceGrade::GradeB)
        }
        StudyType::CohortStudy => ("bio.grade.cohort.v1", EvidenceGrade::GradeC),
        StudyType::CaseControlStudy => ("bio.grade.case-control.v1", EvidenceGrade::GradeC),
        StudyType::CaseSeriesOrReport => ("bio.grade.case-series.v1", EvidenceGrade::GradeC),
        StudyType::PreclinicalOrInVitro => {
            matched_signals.push("scope:non-clinical-preclinical".into());
            ("bio.grade.preclinical.v1", EvidenceGrade::GradeD)
        }
        StudyType::ExpertOpinionOrReview => {
            matched_signals.push("scope:non-clinical-expert-opinion".into());
            ("bio.grade.expert-opinion.v1", EvidenceGrade::GradeD)
        }
    };
    GradingRule {
        rule_id: rule_id.into(),
        matched_signals,
        metadata: metadata_map,
        resulting_grade,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BioEvidenceItem {
    pub study_type: StudyType,
    pub evidence_grade: EvidenceGrade,
    pub grading_rule: GradingRule,
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
            return Err(BioEvidenceError::ExcludedCorpusLicence(corpus));
        }
        let grading_rule = grade_study(study_type, &study_metadata);
        let item = Self {
            study_type,
            evidence_grade: grading_rule.resulting_grade,
            grading_rule,
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
            return Err(BioEvidenceError::ExcludedCorpusLicence(self.corpus.clone()));
        }
        require(
            !self.corpus.trim().is_empty()
                && !self.record_id.trim().is_empty()
                && !self.fetched_at.trim().is_empty()
                && !self.title.trim().is_empty()
                && !self.summary.trim().is_empty()
                && hex_40(&self.pin_sha)
                && self.grading_rule.rule_id.starts_with("bio.grade.")
                && !self.grading_rule.matched_signals.is_empty()
                && self.evidence_grade == self.grading_rule.resulting_grade,
            "item identity, provenance, or grading rule",
        )?;
        require(
            self.pinned_citation.corpus == self.corpus
                && self.pinned_citation.commit == self.pin_sha,
            "citation provenance does not match corpus pin",
        )?;
        let bool_value = |key: &str| {
            self.grading_rule
                .metadata
                .get(key)
                .and_then(|v| v.parse::<bool>().ok())
                .ok_or(BioEvidenceError::Invalid("invalid grading metadata"))
        };
        let metadata = StudyMetadata {
            multicenter: bool_value("multicenter")?,
            prospective: bool_value("prospective")?,
            well_powered: bool_value("well-powered")?,
            human_clinical: bool_value("human-clinical")?,
        };
        let expected = grade_study(self.study_type, &metadata);
        require(
            self.grading_rule == expected,
            "grading rule is not reproducible",
        )
    }

    pub fn is_clinical(&self) -> bool {
        !matches!(
            self.study_type,
            StudyType::PreclinicalOrInVitro | StudyType::ExpertOpinionOrReview
        ) && self.grading_rule.metadata["human-clinical"] == "true"
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
        for item in &evidence {
            item.validate()?;
        }
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
            abstention,
        };
        chapter.normalized()
    }

    fn normalized(mut self) -> Result<Self> {
        require(
            !self.chapter_id.trim().is_empty()
                && !self.title.trim().is_empty()
                && !self.thesis.trim().is_empty(),
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
        require(
            self.clinical_evidence
                .iter()
                .all(|i| i.evidence_grade != EvidenceGrade::GradeD)
                || self.clinical_evidence.is_empty(),
            "grade-d item cannot support clinical efficacy",
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

fn item_order(a: &BioEvidenceItem, b: &BioEvidenceItem) -> std::cmp::Ordering {
    a.evidence_grade
        .cmp(&b.evidence_grade)
        .then_with(|| a.corpus.cmp(&b.corpus))
        .then_with(|| a.record_id.cmp(&b.record_id))
        .then_with(|| a.pin_sha.cmp(&b.pin_sha))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BioEvidenceReport {
    pub report_version: String,
    pub topic: String,
    pub chapters: Vec<BioOutlineChapter>,
    pub not_medical_advice_notice: String,
    pub report_sha256: String,
}

#[derive(Serialize)]
struct BioReportPayload<'a> {
    report_version: &'a str,
    topic: &'a str,
    chapters: &'a [BioOutlineChapter],
    not_medical_advice_notice: &'a str,
}

impl BioEvidenceReport {
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
            report_version: REPORT_VERSION.into(),
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
        Ok(sha256(&canonical(&BioReportPayload {
            report_version: &self.report_version,
            topic: &self.topic,
            chapters: &self.chapters,
            not_medical_advice_notice: &self.not_medical_advice_notice,
        })?))
    }

    fn validate(&self) -> Result<()> {
        require(
            self.report_version == REPORT_VERSION
                && !self.topic.trim().is_empty()
                && !self.chapters.is_empty()
                && self
                    .not_medical_advice_notice
                    .to_ascii_lowercase()
                    .contains("literature synthesis")
                && self
                    .not_medical_advice_notice
                    .to_ascii_lowercase()
                    .contains("not medical advice")
                && self
                    .not_medical_advice_notice
                    .to_ascii_lowercase()
                    .contains("clinical diagnosis"),
            "missing report metadata or mandatory disclaimer",
        )?;
        let mut chapter_ids = std::collections::HashSet::new();
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
    use crate::hub_evidence::PinnedCitation;

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
    fn grading_is_deterministic_and_excludes_nonredistributable_corpora() {
        let clinical = StudyMetadata {
            human_clinical: true,
            ..StudyMetadata::default()
        };
        assert_eq!(
            grade_study(StudyType::MetaAnalysis, &clinical).resulting_grade,
            EvidenceGrade::GradeA
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
            .resulting_grade,
            EvidenceGrade::GradeB
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
        assert!(matches!(
            error,
            BioEvidenceError::ExcludedCorpusLicence(corpus) if corpus == "closed"
        ));
    }

    #[test]
    fn synthesis_sorts_grades_segregates_preclinical_and_abstains() {
        let human = StudyMetadata {
            human_clinical: true,
            ..StudyMetadata::default()
        };
        let chapter = BioOutlineChapter::new(
            "clinical".into(),
            "Clinical evidence".into(),
            "Only clinical findings support efficacy claims.".into(),
            vec![
                item(StudyType::CaseSeriesOrReport, human.clone(), "c"),
                item(StudyType::MetaAnalysis, human, "a"),
                item(
                    StudyType::PreclinicalOrInVitro,
                    StudyMetadata::default(),
                    "p",
                ),
            ],
        )
        .unwrap();
        assert_eq!(
            chapter.clinical_evidence[0].evidence_grade,
            EvidenceGrade::GradeA
        );
        assert_eq!(chapter.preclinical_evidence.len(), 1);
        assert_eq!(
            chapter.preclinical_evidence[0].study_type,
            StudyType::PreclinicalOrInVitro
        );

        let abstaining = BioOutlineChapter::new(
            "animal".into(),
            "Animal evidence".into(),
            "Preclinical evidence is hypothesis-generating.".into(),
            vec![item(
                StudyType::PreclinicalOrInVitro,
                StudyMetadata::default(),
                "p2",
            )],
        )
        .unwrap();
        assert!(abstaining.abstention.is_some());
    }

    #[test]
    fn report_disclaimer_digest_and_strict_verification_round_trip() {
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
        let report = BioEvidenceReport::new(
            "Research question".into(),
            vec![chapter],
            mandatory_not_medical_advice_notice().into(),
        )
        .unwrap();
        let json = report.to_jcs().unwrap();
        let verified = BioEvidenceReport::verify_json(&json).unwrap();
        assert_eq!(verified.to_jcs().unwrap(), json);
        assert!(BioEvidenceReport::new(
            "Research question".into(),
            report.chapters,
            "Use with care.".into()
        )
        .is_err());
        assert!(BioEvidenceReport::verify_json(
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
}
