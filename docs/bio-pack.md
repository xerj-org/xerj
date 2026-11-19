# Bio pack (`bio.outline` and `bio.evidence`)

`xerj_common::bio_pack` provides the deterministic, provenance-preserving
biomedical evidence grading and synthesis primitives for `bio.outline` and
`bio.evidence` (#2766, Epic #2769). Callers pass verified `PinnedCitation`
records from Corpus Hub; the module performs no network I/O, makes no clinical
diagnosis, and never recommends treatment.

## Deterministic study-type grading (`StudyType` -> `EvidenceTier`)

`grade_study(study_type, metadata)` produces a reproducible `GradeRule`
recording `rule_id`, `study_type`, `tier` (`Level1A`..`Level5`),
`matched_signals`, sorted `metadata`, and `rationale`:

| `StudyType` | Design conditions | `EvidenceTier` | Rule ID |
| --- | --- | --- | --- |
| `MetaAnalysis` | Quantitative synthesis | `Level1A` | `bio.grade.meta-analysis.v1` |
| `SystematicReview` | Explicit inclusion criteria | `Level1A` | `bio.grade.systematic-review.v1` |
| `RandomizedControlledTrial` | `multicenter` or `well_powered` | `Level1B` | `bio.grade.rct-powered.v1` |
| `RandomizedControlledTrial` | Single-center / exploratory | `Level2A` | `bio.grade.rct-single.v1` |
| `CohortStudy` | `prospective` and `well_powered` | `Level2A` | `bio.grade.prospective-cohort.v1` |
| `CohortStudy` | Retrospective / standard | `Level2B` | `bio.grade.cohort.v1` |
| `CaseControl` | Case-control design | `Level3` | `bio.grade.case-control.v1` |
| `CaseReport` | Case report or series | `Level4` | `bio.grade.case-report.v1` |
| `Preclinical` | Animal model (non-clinical) | `Level5` | `bio.grade.preclinical.v1` |
| `InVitro` | Bench / assay (non-clinical) | `Level5` | `bio.grade.in-vitro.v1` |

## Redistribution, clinical separation, and boundary guarantees

- **Licence gate (`LicenceClass`)**: every `BioEvidenceItem` carries its corpus
  `LicenceClass`, 40-hex `pin_sha`, and `PinnedCitation`. Any item from a
  corpus with `LicenceClass::Excluded` is refused with
  `BioPackError::ExcludedCorpusLicence`.
- **Clinical vs. preclinical separation**: both `BioEvidencePacket`
  (`bio_evidence`) and `BioOutlineChapter` (`bio_outline`) partition human
  clinical studies (`Level1A`..`Level4` with `human_clinical = true`) from
  `Preclinical` and `InVitro` (`Level5`) evidence, sort each tier
  deterministically, and emit a mandatory clinical abstention string whenever
  `clinical_evidence` is empty.
- **Contradictory and adverse findings**: items tagged with
  `FindingDirection::Contradictory` or `FindingDirection::Adverse` are
  additionally surfaced in `contradictory_findings` so negative or safety
  signals are never silently omitted.
- **Not medical advice boundary & JCS receipt**: both `BioEvidencePacket` and
  `BioOutlineReport` require `mandatory_not_medical_advice_notice()` (stating
  literature synthesis only, not medical advice, and not clinical diagnosis)
  and seal their payload with an RFC 8785 (JCS) `report_sha256` verified by
  `to_jcs()` and `verify_json()`.
