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
| `ExpertOpinion` | Opinion or narrative review, no primary data | `Level5` | `bio.grade.expert-opinion.v1` |

The six study types above `Preclinical` are graded as shown **only when
`human_clinical = true`** (the default is `false`). A study that is not human
clinical evidence, such as a meta-analysis of animal studies, is capped at
`Level5` under `bio.grade.non-human-cap.v1`, and its `matched_signals` name the
rule it would otherwise have met (`scope:non-human-capped-from:<rule_id>`). A
non-human study is therefore never graded `Level1A`. `StudyType` has no serde
aliases: `expert-opinion`, `in-vitro` and `preclinical` are three designs, and
an old label such as `expert-opinion-or-review` is rejected, not mapped to
one of them.

## Redistribution, clinical separation, and boundary guarantees

- **Licence gate (`LicenceClass`)**: every `BioEvidenceItem` carries its corpus
  `LicenceClass`, 40-hex `pin_sha`, and `PinnedCitation`. Any item from a
  corpus with `LicenceClass::Excluded` is refused with
  `BioPackError::ExcludedCorpusLicence`.
- **Clinical vs. non-clinical separation**: both `BioEvidencePacket`
  (`bio_evidence`) and `BioOutlineChapter` (`bio_outline`) partition human
  clinical studies (`human_clinical = true`, `Level1A`..`Level4`) from
  everything else. `clinical_evidence` holds only the former.
  `preclinical_evidence` holds every other item, all `Level5`: `Preclinical`,
  `InVitro` and `ExpertOpinion` studies, and any non-human study of a clinical
  design (capped as above). Each section is sorted deterministically, and a
  mandatory clinical abstention string is emitted whenever `clinical_evidence`
  is empty. `validate()` re-derives every item's grade from its metadata, so a
  hand-edited item cannot move into `clinical_evidence` or keep a tier its
  metadata does not support.
- **Contradictory and adverse findings**: items tagged with
  `FindingDirection::Contradictory` or `FindingDirection::Adverse` are
  additionally surfaced in `contradictory_findings` so negative or safety
  signals are never silently omitted.
- **Not medical advice boundary & JCS receipt**: both `BioEvidencePacket` and
  `BioOutlineReport` require `mandatory_not_medical_advice_notice()` (stating
  literature synthesis only, not medical advice, and not clinical diagnosis)
  and seal their payload with an RFC 8785 (JCS) `report_sha256` verified by
  `to_jcs()` and `verify_json()`.
