# Local verification — 2026-10-25

OpenAI Codex, operated by Jarvis636431. macOS arm64, Rust 1.97.1.
Candidate source is recorded in `2f46093b`, independently based on upstream
`b8c41cb6`. No dependency on the open batch-retrieval benchmark PR #2384.
The local release binary reports `xerj v1.0.0-rc.99`; this is a source-built
candidate, not a verification claim about published release assets.

## Commands and observed results

Run from `engine/`, except the end-to-end command marked below:

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

TMPDIR=/private/tmp cargo +1.97.1 test --locked --offline \
  -p xerj-server --test sessions_cli --bin xerj

cargo +1.97.1 clippy --locked --offline --no-deps \
  -p xerj-server --bin xerj --test sessions_cli -- -D warnings
cargo +1.97.1 clippy --locked --offline --no-deps \
  -p xerj-autoindex --all-targets -- -D warnings -A dead_code
```

- Scoped release build, formatting and whitespace checks passed.
- Autoindex: **1,839 passed, 0 failed, 5 existing ignored, 3 filtered** across
  its unit/integration/doc-test invocations. All **11** new session tests passed.
- Main program: **119 passed**. New CLI integration tests: **3 passed**.
- Both listed Clippy invocations passed. Autoindex's unmodified
  `content.rs:143` `resolve_reporting` dead-code warning prevents a blanket
  `-D warnings` run on this host; this exact failure was separately reproduced
  on the unmodified upstream baseline. No repository lint allowance was added.
- Workflow timeout guard passed; no workflows were changed.

The initial sandbox invocation could not bind test loopback sockets; the final
tests ran with loopback permitted. The normal parallel invocation also hit
existing process-global/path interference; final full tests are explicitly
serial with the canonical temporary root above. No test assertions were changed.

### Why the three filters exist

Each named test was run individually on the unmodified, detached `b8c41cb6`
worktree with the same Rust toolchain and `/private/tmp` root, and failed there:

| Test | Observed baseline failure on this filesystem |
|---|---|
| `lossy_display_collisions_keep_distinct_alias_identities` | Creating the fixture's non-UTF-8 filename returns OS error 92, Illegal byte sequence |
| `a_non_utf8_excluded_name_is_matched_on_disk_via_path_id` | Same non-UTF-8 filename restriction |
| `corpus_index_summary_splits_indexed_extensions_from_ignored_data` | `rows.csv` and `ROWS.CSV` name the same file on the case-insensitive filesystem; observed csv count 1 versus expected 2 |

These filters are local command arguments, not committed skips or weaker tests.
The unfiltered serial candidate suite observed **1,792 passed, 3 failed,
2 ignored** before the final inclusive-limit and FIFO regressions were added. Linux filesystem behavior
and hosted CI are not inferred from this local result.

## Real CLI, index and query

From the repository root:

```sh
python3 tools/session-import/smoke.py --binary engine/target/release/xerj
```

Observed on the final release candidate:

- Offline import exported **2** redacted text turns from the synthetic fixture;
  existing-output refusal preserved the file.
- Autoindex dry-run reported **1 file (0 MB)**. The first index and unchanged
  rerun both completed with exit **0**, no refused datasets/files and **2** live
  documents. The observed wall times were 0.818 s / 0.353 s respectively; these
  tiny checks are not a speed benchmark.
- CLI query `why did we drop polling` retrieved the decision text. Stored
  metadata retained original `session.jsonl:3` and `src/watch.rs`; raw email,
  thinking, tool payload and metadata sentinels were absent from stored source.
- Both private-node shutdowns exited **0**, and restart preserved **2** documents.

The example is lexical text retrieval with `--no-semantic`, no model/reranker,
no real personal transcript and no real Claude client release under test.
It is not generated-answer or confidence-calibration evidence.

## Compatibility gate

On a separate fresh private node running the same release candidate:

```sh
engine/target/release/es-yaml-runner --url "$PRIVATE_NODE_URL" \
  --dir engine/tests/es-compat-yaml/yaml
```

Observed: **229 files; 1,444 passed, 0 failed, 10 skipped; exit 0**.
The unchanged suite policy reported **75 catch-lenient cases**. No runner,
YAML, catch-lenient policy or server protocol implementation was changed.
The private conformance node shut down with exit **0**.

## Negative controls and review

| Invariant | Authority / exercised boundary | Result |
|---|---|---|
| Redaction precedes every exported text/metadata write | Native PII plugin, unit fixtures covering its five types and CLI stored-source check | Pass; an isolated source-copy mutation bypassing the plugin produced exactly the two intended privacy-test failures |
| Incomplete input is never a completed snapshot | Malformed later line/tail, unsupported shapes, duplicate UUIDs, mixed session IDs and size refusals | Pass; no output, staging removed, raw payload absent from diagnostics; corrected synthetic input then succeeds |
| Existing output survives a race after preflight | `persist_noclobber` boundary with an independently created winner | Pass; a separate source-copy mutation using overwrite publication fails that exact regression |
| Input source and source citation are explicit | Validated UUIDs, file bytes consumed, source-line/byte metadata, change-detection helper | Pass; source mutation refused; original input preserved; source hashes match |
| Non-file input cannot hang waiting for a writer | Unix FIFO opened nonblocking and rejected by file type | Pass; no writer or timing assumption in the regression |
| Prepared data survives the existing index lifecycle | Real CLI + private HTTP node, unchanged autoindex rerun and graceful restart | Pass; 2 documents throughout |

The mutation checks compiled **private copies** of the module. Production source
was never mutated during builds; its normal tests pass. Before implementation,
the initial scaffold had two expected session-test failures and one existing
output preservation check passing.

Review against `docs/CONTRIBUTION_REVIEW.md`: effective diff contains only the
new offline preparation path, existing-command dispatch/help, two existing
dependencies, tests/fixture and documentation. No existing workflow, JSONL
sniffer/extractor, index mapping, persisted product format or recovery path is
replaced. Export schema 1 is a new-file snapshot; future refresh/migration uses
an explicitly new output and prefix/state directory, not silent overwrite.
Tempfile no-clobber behavior and the possible extra hidden hard link are
documented without a cross-platform transactional-durability claim.

Legacy product migrations, optional accelerators, distributed writes, model
execution, providers and new egress are not applicable: this command only reads
the explicit source and writes the explicit export. Windows/Linux real-node
execution, live client-version compatibility, deep secret removal, automatic
discovery, active-chain reconstruction, incremental refresh, and commit/graph
association are not claimed. Website generation gates were not run because
no landing/generated website files changed. Hosted CI must validate composition.
