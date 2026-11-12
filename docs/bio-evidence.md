# Bio evidence synthesis

`xerj_common::bio_evidence` provides the typed, deterministic core for the
`bio.outline` and `bio.evidence` packs. Callers supply verified pinned records;
the module does not search, fetch, diagnose, or recommend treatment.

`StudyType` is graded by an explicit `GradingRule`. Meta-analyses, systematic
reviews, and multicenter RCTs receive Grade A; single RCTs and well-powered
prospective cohorts receive Grade B; retrospective cohorts, case-control, and
case-series evidence receive Grade C; and preclinical, in-vitro, and expert
opinion receive Grade D. The grading receipt records matched signals and sorted
metadata.

Every source retains its corpus, record ID, licence class, 40-hex pin, fetch
time, and `PinnedCitation`. `Excluded` corpus licences fail closed. Chapter
construction separates human clinical evidence from preclinical or
non-clinical findings, sorts each group by grade and stable provenance, and
adds a clinical-evidence abstention whenever no qualifying clinical evidence
exists. Preclinical findings are never placed in the clinical-efficacy section.

Reports require a non-empty notice stating that the output is literature
synthesis from cited corpora, is not medical advice, and is not a clinical
diagnosis. `BioEvidenceReport::to_jcs` emits RFC 8785 JSON and checks
`report_sha256`; `BioEvidenceReport::verify_json` enforces the same schema and
digest when reading a report.
