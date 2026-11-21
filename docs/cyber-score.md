# Cyber scoring (`cyber.score`, #2775, #2767, #2769)

`xerj_common::cyber_score` implements deterministic CVSS v3.1 and CVSS v4.0
scoring in Rust, attaches EPSS and CISA KEV context from hub corpora with
mandatory provenance and fetch dates, and derives business-impact priority
notes from user-supplied asset context.

## Deterministic CVSS v3.1 and v4.0

- **CVSS v3.1 (`score_cvss_v31`)**: parses and validates Base (`AV`, `AC`, `PR`,
  `UI`, `S`, `C`, `I`, `A`), Temporal (`E`, `RL`, `RC`), and Environmental
  (`CR`, `IR`, `AR`, `MAV`, `MAC`, `MPR`, `MUI`, `MS`, `MC`, `MI`, `MA`)
  metrics using integer-exact Appendix A `RoundUp` and the v3.1 Modified Impact
  exponent (`13`).
- **CVSS v4.0 (`score_cvss_v40`)**: validates canonical metric ordering across
  Base, Threat (`E`), Environmental, and Supplemental (`S`, `AU`, `R`, `V`,
  `RE`, `U`) metrics, derives the `EQ1..EQ6` MacroVector and nomenclature
  (`CVSS-B`, `CVSS-BT`, `CVSS-BE`, `CVSS-BTE`), and interpolates severity
  distances against the official FIRST lookup table.
- **Invalid vectors**: missing mandatory metrics, duplicate keys, unknown metric
  values, or out-of-order CVSS v4.0 metrics return `ScoreError::InvalidCvssVector`.

## EPSS and CISA KEV provenance

Both `epss` and `kev` are typed as `ThreatIntelField<T>`:

- `Present(T)` requires a valid `CVE-YYYY-NNNN+` identifier, `source_corpus`, a
  40-hex lowercase `pin_sha`, and a `fetched_at` date (`YYYY-MM-DD` or RFC 3339),
  plus optional `PinnedCitation` verification.
- `Absent { reason }` states explicitly why data is unavailable (`"status": "absent"`).
  Absent values are never guessed or defaulted to `0.0` or `false`.

## Business impact and scope guard

`CyberScoreReport::evaluate` combines CVSS, EPSS/KEV, and optional
`AssetContext` (`criticality`, `data_classification`, `exposure`,
`compensating_controls`, `endpoint_target`) into a deterministic
`BusinessImpact` (`PriorityTier::P1..P4` and provenance notes) sealed with an
RFC 8785 (JCS) `report_sha256`. When `endpoint_target` is set, evaluation
requires `CyberScoreReport::evaluate_with_scope` with an active
`cyber_scope::ScopeGuard`.
