# Codex rollout adapter verification — 2026-10-26

Local runs span October 25–26, Asia/Shanghai.

OpenAI Codex, operated by Jarvis636431. macOS arm64, Rust 1.97.1.
Independent branch from upstream `23016aa6d41123956860309a5aae3d3526b4415e`,
which includes merged #2406. No stacked commits or dependency on open #2384.
Implementation commit: `ea74d998`. After the final test-only corrections,
the scoped release rebuild passed and produced the same binary bytes as the
private executable snapshot used by both smoke flows and the ES-YAML gate:
SHA-256 `88e8be55b5fce5c5a515ebfc3306905a57ac852714431cb9a18e1223d160c4e8`.
This is a local artifact receipt, not a signature or published-release claim.
The supported format and pinned Apache-2.0 reference are in
[the import guide](README.md#codex-rollout-adapter).

## Commands and results

From `engine/`:

```sh
cargo +1.97.1 fmt --all
cargo +1.97.1 fmt --all -- --check
cargo +1.97.1 build --locked --offline --release -j 32 \
  -p xerj-autoindex -p xerj-server -p es-yaml-runner

TMPDIR=/private/tmp cargo +1.97.1 test --locked --offline -p xerj-autoindex -- \
  --test-threads=1 \
  --skip content::tests::lossy_display_collisions_keep_distinct_alias_identities \
  --skip corpus_data_skip_tests::corpus_index_summary_splits_indexed_extensions_from_ignored_data \
  --skip inventory_delta_tests::a_non_utf8_excluded_name_is_matched_on_disk_via_path_id

cargo +1.97.1 test --locked --offline -p xerj-autoindex --lib sessions::
TMPDIR=/private/tmp cargo +1.97.1 test --locked --offline \
  -p xerj-server --test sessions_cli --bin xerj -- --test-threads=1

cargo +1.97.1 clippy --locked --offline --no-deps \
  -p xerj-autoindex --all-targets -- -D warnings -A dead_code
cargo +1.97.1 clippy --locked --offline --no-deps \
  -p xerj-server --bin xerj --test sessions_cli -- -D warnings
```

- Autoindex crate: **1,844 passed, 0 failed, 5 existing ignored, 3 filtered**
  across unit/integration/doc-test invocations. Session module: **16 passed**
  (11 existing Claude Code/shared-I/O tests, 5 Codex tests).
- Main program: **119 passed**; CLI integration: **4 passed**.
- Both listed Clippy invocations passed. The existing `resolve_reporting`
  dead-code warning still needs the command-line allowance for autoindex; no
  lint suppression was added to source. The server command is scoped to its
  main binary and session integration target, not every server test target.
- Formatting and whitespace checks passed. From the repository root,
  `python3 .github/scripts/workflow-timeout-guard.py` passed (40 jobs across
  12 workflows); Python compilation of `tools/session-import/smoke.py` passed.

The three filesystem filters and original baseline reproductions are documented
in [the Claude Code verification](VERIFICATION.md#why-the-three-filters-exist).
The affected `content.rs` is byte-identical to that baseline; `lib.rs` differs
only by exporting the sessions module. No committed skip/assertion changed.
Loopback tests ran with permission to bind local sockets and a canonical
`TMPDIR`; sandbox-denied socket failures are not reported as product defects.
An early PII fixture without separators made the native credit-card pattern
consume adjacent numeric examples; labels/punctuation corrected the synthetic
test input, without changing the plugin or weakening the five-type assertions.

## Regression evidence

Before adding format support, the new real CLI regression failed with
`error: --format claude-code is required` (1 failed, 0 passed). It now passes
and pins 2 exported records from 10 physical lines, 8 skipped records, 1 skipped
attachment block, the decision on source line 9 and its explicit final phase.

The unit fixtures check input/output digests, unchanged source, byte offset,
private file permissions, deterministic archive event IDs, repeated provider IDs,
identical messages on distinct lines, source/context metadata redaction,
five native PII types, absent parent/file associations, malformed tails,
inclusive text bounds, unsupported fields, no-clobber output and successful
recovery after correcting an invalid synthetic source.

Private copies of the module were compiled for targeted mutations; production
source was not mutated:

- Bypassing the shared redactor failed exactly the 2 Codex privacy tests;
  the other 3 Codex tests passed.
- Removing the external `history_base` guard caused a one-record export to
  publish incorrectly; the refusal/recovery regression failed, and the other
  4 Codex tests passed.
- Restoring the unmodified copy passed all 16 session tests.

## Real index lifecycle

From the repository root, using a source-built release binary reporting
`xerj v1.0.0-rc.99`:

```sh
python3 tools/session-import/smoke.py --binary engine/target/release/xerj --format codex
python3 tools/session-import/smoke.py --binary engine/target/release/xerj --format claude-code
```

Both private-node runs passed: 2 redacted records, existing-output refusal,
dry-run **1 file (0 MB)**, successful first index and unchanged rerun with
**2 live documents**, no refused datasets/files, decision retrieval, and
graceful restart retaining 2 documents. Codex retained original source line 9
and an empty file-reference array; Claude Code retained line 3 and
`src/watch.rs`. Raw email and the fixture's private-payload sentinels were absent
from stored source. Each node stopped normally (exit 0).

Observed autoindex `done.wall_s`: Codex 0.8 / 0.3 seconds, Claude Code
0.8 / 0.4 seconds. These tiny fixtures are lifecycle checks, not a speed
benchmark. Both flows used default lexical retrieval and `--no-semantic`, with
no model, personal transcript, existing node or existing index involved.

## Compatibility gate

Against a separate fresh private node using the same release candidate:

```sh
engine/target/release/es-yaml-runner --url "$PRIVATE_NODE_URL" \
  --dir engine/tests/es-compat-yaml/yaml
```

Observed **229 files; 1,444 passed, 0 failed, 10 skipped, 1,454 total; exit 0**.
The unchanged suite reported **75 catch-lenient cases**. No YAML, runner,
catch-lenient policy or HTTP search implementation changed. The conformance
node shut down normally with exit 0. This validates the candidate source build,
not published release assets or hosted merge-ref CI.

## Review and limits

Audited against `docs/CONTRIBUTION_REVIEW.md`: the effective diff adds one
format parser and synthetic fixture, shares the existing bounded I/O/redaction/
no-clobber path, and extends the existing CLI/smoke documentation and tests.
Claude Code keeps its field meanings and admission rules. No normal JSONL
extractor, network endpoint, workflow, dependency, lockfile, model configuration
or existing index is changed. Existing recovery and publication helpers remain
in use. Text and metadata go through redaction before staging writes; only
validated IDs, enums, timestamps and numeric provenance bypass it.

Export schema 1 gains a separate `session_format=codex` record shape and
Codex-only nullable `message_phase`. No previous export/index is rewritten;
refresh uses a new export folder and index prefix/state directory. Message IDs
locate archive events, not provider messages or active conversation branches.
The parser preserves physical response-message order, including historical
text before rollback/compaction, and does not infer file edits or commits.

Legacy product migrations, distributed writes, optional accelerators, model
execution and provider behavior are not applicable to this offline adapter.
Real personal transcripts, live Codex client runs, Windows/Linux execution,
automatic discovery, external history resolution, active-context reconstruction,
incremental refresh, deep secret removal and generated-answer quality were not
tested or claimed. Website gates were not run: no landing files change.
Hosted merge-ref CI is separate from these local receipts.
