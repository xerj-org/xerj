//! Byte-exact evidence and conservative answer decisions (#1789, #1786).
//!
//! Hashes establish integrity, not signer identity or semantic entailment.
//! The caller must resolve files from a trusted immutable pin and supply
//! question-specific coverage and an in-domain calibration assessment.

use crate::calibration::{
    ece_from_curve, reliability_curve, CalibrationPair, IsotonicCalibration, CALIBRATION_BINS,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

const VERSION: &str = "xerj-trace-evidence/v1";
const MAX_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;
const MAX_REPORT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum EvidenceError {
    #[error("invalid evidence: {0}")]
    Invalid(&'static str),
    #[error("evidence JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("pinned source resolution: {0}")]
    Resolution(String),
}

type Result<T> = std::result::Result<T, EvidenceError>;

fn require(ok: bool, reason: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(EvidenceError::Invalid(reason))
    }
}

fn hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn probability(p: f64) -> bool {
    p.is_finite() && (0.0..=1.0).contains(&p)
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(serde_json_canonicalizer::to_vec(value)?)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedCitation {
    pub corpus: String,
    /// Identifies the repository within a multi-repository corpus.
    pub repository: String,
    pub commit: String,
    pub file: String,
    pub file_sha256: String,
    pub byte_start: u64,
    /// Exclusive; offsets refer to original bytes, never rendered text.
    pub byte_end: u64,
    pub line_start: u64,
    pub line_end: u64,
    pub span_sha256: String,
}

impl PinnedCitation {
    pub fn from_bytes(
        corpus: String,
        repository: String,
        commit: String,
        file: String,
        bytes: &[u8],
        span: std::ops::Range<usize>,
    ) -> Result<Self> {
        require(
            span.start < span.end && span.end <= bytes.len(),
            "byte range",
        )?;
        let line = |end| 1 + bytes[..end].iter().filter(|&&b| b == b'\n').count() as u64;
        let citation = Self {
            corpus,
            repository,
            commit,
            file,
            file_sha256: sha256(bytes),
            byte_start: span.start as u64,
            byte_end: span.end as u64,
            line_start: line(span.start),
            line_end: line(span.end - 1),
            span_sha256: sha256(&bytes[span]),
        };
        citation.validate()?;
        Ok(citation)
    }

    fn validate(&self) -> Result<()> {
        require(
            !self.corpus.trim().is_empty() && !self.repository.trim().is_empty(),
            "source identity",
        )?;
        require(hex(&self.commit, 40), "full lowercase git commit required")?;
        require(
            !self.file.is_empty()
                && !self.file.contains(['\\', ':', '\0'])
                && self
                    .file
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != ".."),
            "relative source path",
        )?;
        require(
            hex(&self.file_sha256, 64) && hex(&self.span_sha256, 64),
            "lowercase SHA-256 required",
        )?;
        require(
            self.byte_start < self.byte_end
                && self.byte_end <= MAX_SAFE_INTEGER
                && self.line_start > 0
                && self.line_start <= self.line_end
                && self.line_end <= MAX_SAFE_INTEGER,
            "citation bounds",
        )
    }

    pub fn verify_bytes(&self, bytes: &[u8]) -> Result<()> {
        self.validate()?;
        require(sha256(bytes) == self.file_sha256, "file digest mismatch")?;
        let start = usize::try_from(self.byte_start)
            .map_err(|_| EvidenceError::Invalid("offset overflow"))?;
        let end = usize::try_from(self.byte_end)
            .map_err(|_| EvidenceError::Invalid("offset overflow"))?;
        require(end <= bytes.len(), "byte range beyond source")?;
        let rebuilt = Self::from_bytes(
            self.corpus.clone(),
            self.repository.clone(),
            self.commit.clone(),
            self.file.clone(),
            bytes,
            start..end,
        )?;
        require(
            self.span_sha256 == rebuilt.span_sha256
                && self.line_start == rebuilt.line_start
                && self.line_end == rebuilt.line_end,
            "span digest or line mismatch",
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Confidence {
    pub calibration_sha256: String,
    pub domain: String,
    pub p_raw: f64,
    pub p_cal: f64,
    pub ece: f64,
    pub heldout_n: u64,
    pub reliability: Vec<ConfidenceBin>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfidenceBin {
    pub lo: f64,
    pub hi: f64,
    pub n: u64,
    pub mean_p: f64,
    pub positive_rate: f64,
}

impl Confidence {
    /// Held-out observations must not have been used to fit this model.
    /// `domain` names the scorer and task, not merely a corpus slug.
    pub fn isotonic(
        p_raw: f64,
        domain: String,
        model: &IsotonicCalibration,
        heldout: &[CalibrationPair],
    ) -> Result<Self> {
        require(probability(p_raw), "raw probability")?;
        let knots = model.knots();
        require(
            !domain.trim().is_empty()
                && !knots.is_empty()
                && !heldout.is_empty()
                && knots.iter().all(|&(p, y)| probability(p) && probability(y))
                && heldout.iter().all(|p| probability(p.p) && probability(p.y)),
            "missing or invalid calibration assessment",
        )?;
        let pairs: Vec<_> = heldout
            .iter()
            .map(|p| CalibrationPair {
                p: model.apply(p.p),
                y: p.y,
            })
            .collect();
        let curve = reliability_curve(&pairs, CALIBRATION_BINS);
        let observations: Vec<_> = heldout.iter().map(|p| (p.p, p.y)).collect();
        let result = Self {
            calibration_sha256: sha256(&canonical(&(&domain, &knots, &observations))?),
            domain,
            p_raw,
            p_cal: model.apply(p_raw),
            ece: ece_from_curve(&curve),
            heldout_n: heldout.len() as u64,
            reliability: curve
                .iter()
                .map(|b| ConfidenceBin {
                    lo: b.lo,
                    hi: b.hi,
                    n: b.n as u64,
                    mean_p: b.mean_p,
                    positive_rate: b.positive_rate,
                })
                .collect(),
        };
        result.validate()?;
        Ok(result)
    }

    fn validate(&self) -> Result<()> {
        require(
            hex(&self.calibration_sha256, 64)
                && !self.domain.trim().is_empty()
                && probability(self.p_raw)
                && probability(self.p_cal)
                && probability(self.ece)
                && self.heldout_n > 0
                && self.heldout_n <= MAX_SAFE_INTEGER
                && !self.reliability.is_empty()
                && self.reliability.len() <= CALIBRATION_BINS,
            "confidence metadata",
        )?;
        let mut total = 0_u64;
        let mut weighted_error = 0.0;
        let mut previous_hi = 0.0;
        for b in &self.reliability {
            require(
                probability(b.lo)
                    && probability(b.hi)
                    && b.lo < b.hi
                    && b.lo >= previous_hi
                    && probability(b.mean_p)
                    && b.mean_p + 1e-12 >= b.lo
                    && b.mean_p <= b.hi + 1e-12
                    && probability(b.positive_rate)
                    && b.n > 0
                    && b.n <= self.heldout_n,
                "reliability bin",
            )?;
            total = total
                .checked_add(b.n)
                .ok_or(EvidenceError::Invalid("bin count overflow"))?;
            weighted_error += b.n as f64 * (b.positive_rate - b.mean_p).abs();
            previous_hi = b.hi;
        }
        require(
            total == self.heldout_n && (weighted_error / total as f64 - self.ece).abs() <= 1e-12,
            "reliability count or ECE mismatch",
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub id: String,
    pub text: String,
    pub evidence_indices: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceEntry {
    pub tool_call_id: String,
    pub question: String,
    /// Exact text returned to the caller; not re-rendered during audit.
    pub shown_answer: String,
    pub claims: Vec<Claim>,
    pub citations: Vec<PinnedCitation>,
    pub confidence: Option<Confidence>,
    pub p_min: f64,
    /// Question-specific gaps supplied by the coverage evaluator.
    pub coverage_gaps: Vec<String>,
    pub abstain: bool,
    pub escalate: bool,
}

impl TraceEntry {
    fn gaps(&self) -> Vec<String> {
        let mut gaps = self.coverage_gaps.clone();
        if self.citations.is_empty() {
            gaps.push("no byte-exact pinned evidence".into());
        }
        if self.claims.is_empty() || self.claims.iter().any(|c| c.evidence_indices.is_empty()) {
            gaps.push("claim has no evidence binding".into());
        }
        match &self.confidence {
            None => gaps.push("no in-domain calibration assessment".into()),
            Some(c) if c.p_cal < self.p_min => {
                gaps.push("calibrated confidence below threshold".into());
            }
            Some(_) => {}
        }
        gaps
    }

    pub fn finalize(mut self) -> Result<Self> {
        let abstain = !self.gaps().is_empty();
        self.abstain = abstain;
        self.escalate = abstain;
        self.validate()?;
        Ok(self)
    }

    pub fn abstain_gaps(&self) -> Vec<String> {
        self.gaps()
    }

    fn validate(&self) -> Result<()> {
        require(
            !self.tool_call_id.trim().is_empty()
                && !self.question.trim().is_empty()
                && probability(self.p_min)
                && self.coverage_gaps.iter().all(|g| !g.trim().is_empty()),
            "entry metadata",
        )?;
        if let Some(c) = &self.confidence {
            c.validate()?;
        }
        for citation in &self.citations {
            citation.validate()?;
        }
        let mut ids = HashSet::new();
        for c in &self.claims {
            require(
                !c.id.trim().is_empty() && ids.insert(&c.id) && !c.text.trim().is_empty(),
                "claim identity",
            )?;
            let mut indices = HashSet::new();
            for &i in &c.evidence_indices {
                require(
                    (i as usize) < self.citations.len() && indices.insert(i),
                    "claim evidence reference",
                )?;
            }
        }
        let must_abstain = !self.gaps().is_empty();
        require(
            self.abstain == must_abstain && self.escalate == must_abstain,
            "decision must fail closed",
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceEvidenceReport {
    pub report_version: String,
    pub session_id: String,
    pub entries: Vec<TraceEntry>,
    pub evidence_receipt_sha256: String,
}

#[derive(Serialize)]
struct ReportPayload<'a> {
    report_version: &'a str,
    session_id: &'a str,
    entries: &'a [TraceEntry],
}

impl TraceEvidenceReport {
    /// The digest is not part of its own pre-image.
    fn digest(&self) -> Result<String> {
        Ok(sha256(&canonical(&ReportPayload {
            report_version: &self.report_version,
            session_id: &self.session_id,
            entries: &self.entries,
        })?))
    }

    fn validate(&self) -> Result<()> {
        require(
            self.report_version == VERSION
                && !self.session_id.trim().is_empty()
                && !self.entries.is_empty(),
            "report metadata",
        )?;
        let mut calls = HashSet::new();
        for e in &self.entries {
            require(calls.insert(&e.tool_call_id), "duplicate tool call")?;
            e.validate()?;
        }
        Ok(())
    }

    pub fn new(session_id: String, entries: Vec<TraceEntry>) -> Result<Self> {
        let mut report = Self {
            report_version: VERSION.into(),
            session_id,
            entries,
            evidence_receipt_sha256: String::new(),
        };
        report.validate()?;
        report.evidence_receipt_sha256 = report.digest()?;
        require(
            canonical(&report)?.len() <= MAX_REPORT_BYTES,
            "report byte limit",
        )?;
        Ok(report)
    }

    pub fn to_jcs(&self) -> Result<Vec<u8>> {
        self.validate()?;
        require(
            self.evidence_receipt_sha256 == self.digest()?,
            "report digest mismatch",
        )?;
        let bytes = canonical(self)?;
        require(bytes.len() <= MAX_REPORT_BYTES, "report byte limit")?;
        Ok(bytes)
    }

    /// Parse directly into typed structs: duplicate fields, unknown members,
    /// invalid UTF-16, bool-as-integer, and excessive JSON nesting are errors.
    /// The resolver MUST use (repository, commit, file), never mutable HEAD.
    pub fn verify(
        json: &[u8],
        mut resolve: impl FnMut(&PinnedCitation) -> Result<Vec<u8>>,
    ) -> Result<Self> {
        require(json.len() <= MAX_REPORT_BYTES, "report byte limit")?;
        let report: Self = serde_json::from_slice(json)?;
        report.to_jcs()?;
        for entry in &report.entries {
            for citation in &entry.citations {
                citation.verify_bytes(&resolve(citation)?)?;
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // CRLF and UTF-8 deliberately remain unnormalized.
    const FILE: &[u8] = b"first\r\nsecond \xc3\xa9\r\nthird\n";

    fn citation() -> PinnedCitation {
        PinnedCitation::from_bytes(
            "sample".into(),
            "owner/repo".into(),
            "a".repeat(40),
            "src/example.txt".into(),
            FILE,
            7..18,
        )
        .unwrap()
    }

    fn confidence() -> Confidence {
        let train = [
            CalibrationPair::new(0.1, false),
            CalibrationPair::new(0.9, true),
        ];
        let heldout = [
            CalibrationPair::new(0.05, false),
            CalibrationPair::new(0.95, true),
        ];
        Confidence::isotonic(
            0.95,
            "fixture-answer-support/v1".into(),
            &IsotonicCalibration::fit(&train),
            &heldout,
        )
        .unwrap()
    }

    fn entry() -> TraceEntry {
        TraceEntry {
            tool_call_id: "call-1".into(),
            question: "What is on line two?".into(),
            shown_answer: "second \u{e9}\r\n".into(),
            claims: vec![Claim {
                id: "claim-1".into(),
                text: "Line two contains second followed by an accented e.".into(),
                evidence_indices: vec![0],
            }],
            citations: vec![citation()],
            confidence: Some(confidence()),
            p_min: 0.8,
            coverage_gaps: vec![],
            abstain: false,
            escalate: false,
        }
        .finalize()
        .unwrap()
    }

    fn report() -> TraceEvidenceReport {
        TraceEvidenceReport::new("session-1".into(), vec![entry()]).unwrap()
    }

    #[test]
    fn exact_bytes_lines_and_round_trip() {
        let c = citation();
        assert_eq!((c.line_start, c.line_end), (2, 2));
        assert_eq!(&FILE[7..18], b"second \xc3\xa9\r\n");
        c.verify_bytes(FILE).unwrap();
        let r = report();
        let sample_hex: String = r
            .to_jcs()
            .unwrap()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        println!("SAMPLE_REPORT_HEX={sample_hex}");
        let verified = TraceEvidenceReport::verify(&r.to_jcs().unwrap(), |pin| {
            assert_eq!(pin.commit, "a".repeat(40));
            Ok(FILE.to_vec())
        })
        .unwrap();
        assert!(!verified.entries[0].abstain);
        assert_eq!(verified.entries[0].confidence.as_ref().unwrap().ece, 0.0);
    }

    #[test]
    fn modified_file_span_lines_and_report_are_rejected() {
        assert!(citation().verify_bytes(b"first\nsecond\nthird\n").is_err());
        let mut c = citation();
        c.line_end += 1;
        assert!(c.verify_bytes(FILE).is_err());
        let mut c = citation();
        c.span_sha256 = "0".repeat(64);
        assert!(c.verify_bytes(FILE).is_err());
        let mut r = report();
        r.entries[0].shown_answer.push('!');
        assert!(r.to_jcs().is_err());
    }

    #[test]
    fn gap_abstains_even_with_high_calibrated_confidence() {
        let mut e = entry();
        e.coverage_gaps = vec!["source does not cover the related service".into()];
        let e = e.finalize().unwrap();
        assert_eq!(e.confidence.as_ref().unwrap().p_cal, 1.0);
        assert!(e.abstain && e.escalate);
        assert!(e.abstain_gaps()[0].contains("related service"));
    }

    #[test]
    fn no_calibration_no_evidence_or_low_confidence_abstains() {
        let mut e = entry();
        e.confidence = None;
        assert!(e.finalize().unwrap().abstain);
        let mut e = entry();
        e.claims[0].evidence_indices.clear();
        e.citations.clear();
        assert!(e.finalize().unwrap().abstain);
        let mut e = entry();
        e.confidence.as_mut().unwrap().p_cal = 0.79;
        assert!(e.finalize().unwrap().abstain);
        let mut e = entry();
        e.confidence.as_mut().unwrap().p_cal = 0.8;
        assert!(!e.finalize().unwrap().abstain);
    }

    #[test]
    fn invalid_references_and_confidence_fail_instead_of_clamping() {
        let mut e = entry();
        e.claims[0].evidence_indices = vec![99];
        assert!(e.finalize().is_err());
        let mut e = entry();
        e.confidence.as_mut().unwrap().p_cal = f64::NAN;
        assert!(e.finalize().is_err());
        let mut e = entry();
        e.confidence.as_mut().unwrap().ece = 0.4;
        assert!(e.finalize().is_err());
        assert!(
            Confidence::isotonic(0.9, "domain".into(), &IsotonicCalibration::fit(&[]), &[])
                .is_err()
        );
    }

    #[test]
    fn floating_point_bin_boundary_is_a_valid_assessment() {
        let train: Vec<_> = (0..10).map(|i| CalibrationPair::new(0.5, i == 0)).collect();
        let heldout = vec![CalibrationPair::new(0.5, false); 6];
        Confidence::isotonic(
            0.5,
            "boundary-fixture/v1".into(),
            &IsotonicCalibration::fit(&train),
            &heldout,
        )
        .unwrap();
    }

    #[test]
    fn strict_json_and_safe_integer_bounds() {
        let json = String::from_utf8(report().to_jcs().unwrap()).unwrap();
        for bad in [
            json.replacen(
                "\"session_id\":",
                "\"session_id\":\"duplicate\",\"session_id\":",
                1,
            ),
            json.replacen("\"byte_start\":7", "\"byte_start\":true", 1),
            json.replacen("\"byte_start\":7", "\"byte_start\":9007199254740992", 1),
            json.replacen("\"session-1\"", "\"\\ud800\"", 1),
            json.replacen("\"p_cal\":1", "\"p_cal\":NaN", 1),
            json.replacen("{", "{\"unknown\":1,", 1),
        ] {
            assert!(TraceEvidenceReport::verify(bad.as_bytes(), |_| Ok(FILE.to_vec())).is_err());
        }
        let nested = "[".repeat(129) + &"]".repeat(129);
        assert!(TraceEvidenceReport::verify(nested.as_bytes(), |_| Ok(FILE.to_vec())).is_err());
    }

    #[test]
    fn jcs_uses_utf16_order_and_ecmascript_numbers() {
        let value = serde_json::json!({"\u{e000}": 1e30, "\u{1f600}": -0.0});
        let expected = "{\"\u{1f600}\":0,\"\u{e000}\":1e+30}";
        assert_eq!(
            String::from_utf8(canonical(&value).unwrap()).unwrap(),
            expected
        );
    }

    #[test]
    fn identities_paths_and_empty_spans_fail_closed() {
        for path in ["../secret", "/absolute", "src\\file", "C:file", "src//file"] {
            let mut c = citation();
            c.file = path.into();
            assert!(c.verify_bytes(FILE).is_err());
        }
        let mut c = citation();
        c.commit = "main".into();
        assert!(c.verify_bytes(FILE).is_err());
        assert!(PinnedCitation::from_bytes(
            "c".into(),
            "r".into(),
            "a".repeat(40),
            "f".into(),
            FILE,
            1..1
        )
        .is_err());
        assert!(TraceEvidenceReport::new("s".into(), vec![entry(), entry()]).is_err());
    }
}
