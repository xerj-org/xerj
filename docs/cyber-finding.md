# Cyber finding schema (#2767, #2769)

`xerj_common::cyber_finding` is the shared, network-free finding schema and
evidence-gated report for the cyber pack scenarios. It is a typed library
primitive, not yet a registered recipe, MCP tool, or scanner; it sends no
traffic and resolves no names.

## Finding

One `CyberFinding` flows between scenarios (review, exploitability, score,
advisory):

| Field | Meaning |
| --- | --- |
| `id` | Unique within a report; 1-128 of `[A-Za-z0-9._:-]` |
| `scenario` | `code_review`, `blackbox`, `fuzz_plan`, `exploitability`, `score`, `advisory` |
| `title`, `rationale` | Nonblank text without control characters (rationale may span lines) |
| `cwe` | Optional `CWE-<digits>` (positive, no leading zero, up to six digits) |
| `location` | `source_span` (#2771) or `endpoint` (#2772) |
| `severity` | `info`, `low`, `medium`, `high`, `critical` |
| `cvss_vector`, `cvss_score` | Optional; a score must be finite in `0.0..=10.0` and agree with `severity` (`0.0` info, `<4.0` low, `<7.0` medium, `<9.0` high, else critical) |
| `evidence` | Corpus references (below) |
| `status` | `confirmed`, `candidate`, `not_reachable`, `abstained` |
| `confidence` | Optional calibrated `hub_evidence::Confidence` |
| `abstain` | Must be true exactly when `status` is `abstained` |

Locations:

- `source_span { file, line_start, line_end, sink?, taint_source? }`: a plain
  relative path (no `..`, leading `/`, backslash, `:` or control characters)
  and a 1-based line range with `line_start <= line_end`.
- `endpoint { target, method?, path }`: a bare host or IP (no scheme, port,
  userinfo, slash or whitespace), an uppercase HTTP method, and an absolute
  path without `..` segments (raw or `%2e%2e`), fragments or control characters.

### Evidence

`CorpusEvidenceRef { corpus, record_id, kind, pin_sha, fetched_at,
pinned_citation? }` cites a corpus record. `kind` is `advisory`, `fix_commit`,
`poc` or `pattern`; `pin_sha` is the full lowercase 40-hex commit the corpus was
pinned at; `fetched_at` is `YYYY-MM-DD` or RFC 3339. When present, a
`PinnedCitation` (see `hub_evidence`) must be valid and carry the same corpus
and commit. Duplicate references are refused.

## No finding without evidence

`CyberFinding::validate` fails closed: any finding that is not an abstention
must cite at least one valid evidence reference. `CyberFindingReport::finalize`
does not guess; it converts such a finding into an explicit abstention
(`abstain = true`, `status = abstained`, rationale annotated) before sealing.
Malformed locations, CWEs, evidence or scores are errors, not abstentions.

## Scope

`blackbox` and `exploitability` findings, and any finding with an `endpoint`
location, require a declared `cyber_scope::ScopeGuard`:

```rust
let report = CyberFindingReport::verify_with_scope(
    session_id, findings, &guard, InputOrigin::AuthorizedControlPlane, now,
)?;
```

`finalize` refuses these findings with `ScopeRequired`. With a guard, the
authorization window and trusted origin are checked for the whole report, each
endpoint target must be authorized (out-of-scope refuses the whole report, even
for an abstained finding), `blackbox` findings must be endpoints, and the work
order is echoed in `scope`. `recheck_scope` re-authorizes a deserialized report
against the caller's own guard and refuses a mismatched echo. Authorization
remains the caller's control-plane decision; repository text cannot grant it.

## Receipt

`report_sha256` is the SHA-256 of the RFC 8785 (JCS) canonical form of
`{report_version, session_id, findings, scope}`; the digest is not part of its
own pre-image. `to_jcs` and `verify_json` re-check every invariant and the
digest, reject unknown or duplicate members, and cap size at 8 MiB. The digest
proves the report was not altered, not that its claims are true or who produced
them.
