# Cyber code review

`xerj_common::cyber_code_review` is a deterministic, local source-review primitive. It does not execute the reviewed code, fetch repositories, or authorize network access.

## Evidence contract

`CodeReviewReport::evaluate` combines `TaintCandidate` values with built-in rules for Rust, Python, TypeScript/JavaScript, Go, and SQL. Recipes can supply additional `SourceSinkRule` values. A candidate is ranked only when a source and sink match and at least one corpus citation has the same CWE and a structural/token similarity. Matching sanitizer or guard patterns become explicit `Sanitized` abstentions; candidates without a matching rule or corpus evidence also become explicit abstentions and are never emitted as findings.

Every finding carries an exact `file`, one-based inclusive `line_start` and `line_end`, sink, optional known source path, `CWE-<digits>`, and one or more `PriorVulnCitation` values. Each citation identifies its corpus record and 40-hex immutable pin and embeds a byte-exact `PinnedCitation`.

## Benchmark

`evaluate_seeded_benchmark` accepts caller-supplied candidate fixtures and the indices of known vulnerable cases, then computes precision, recall, and F1. The module fixture includes vulnerable command and SQL examples, a sanitized command, and an unevidenced path traversal. The unit benchmark requires precision 1.0, recall 1.0, F1 1.0, and zero false positives.

## Sealing and verification

`CodeReviewReport::to_jcs` emits RFC 8785 canonical JSON after validating required finding evidence and `report_sha256`. The digest covers all report fields except the digest itself. `verify_json` parses, validates, and verifies the digest; arbitrary re-serialization is unnecessary and tampering fails closed.

Callers remain responsible for obtaining the original corpus bytes at each immutable pin and verifying `PinnedCitation` spans with `PinnedCitation::verify_bytes`. This module deliberately never treats a mutable HEAD checkout as provenance.
