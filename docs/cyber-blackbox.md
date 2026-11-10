# Cyber black-box planning (`cyber.blackbox`, #2772, #2767, #2769)

`xerj_common::cyber_blackbox` implements attack-surface mapping and
corpus-backed test-case planning against targets the operator explicitly
authorizes. It is a pure-Rust planning primitive: `xerj` plans and sends no
network traffic itself.

## Authorization gate (`ScopeGuard`)

`CyberBlackboxReport::plan` requires a `Some(&ScopeGuard)` constructed from
`InputOrigin::AuthorizedControlPlane` whose `[not_before, expires_at]` window
contains `now`:

- Missing scope (`None`), `InputOrigin::UntrustedRepository`, an expired or
  not-yet-valid window, or any `ObservedEndpoint` whose `target` is outside the
  work order allowlist fails closed and refuses the entire plan.
- Every `CyberBlackboxReport` echoes both `scope` (`WorkOrder`) and a human-readable
  `authorization_statement` covered by the RFC 8785 (JCS) `report_sha256`.
- `recheck_scope` re-verifies a deserialized report against the caller's live
  `ScopeGuard`.

## Corpus-cited test cases and abstentions

- Each `PlannedTestCase` embeds its `CorpusPatternRef` (`corpus`, `record_id`,
  40-hex `pin_sha`, `fetched_at`, and optional `PinnedCitation`) and maps to
  both a `CWE-<digits>` identifier and an `OWASP-*` category.
- Endpoints whose derived or explicit surface tags match no corpus pattern are
  recorded in `abstentions` (`EndpointAbstention`) rather than guessed.
