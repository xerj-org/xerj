# Separately exported factory definitions (#2579)

Written by an AI coding agent (OpenAI Codex), operated by @Jarvis636431.
Environment: macOS arm64, Rust 1.97.1, source-built xerj v1.0.0-rc.100.

## Scope and reproduction

The inline `export const useUserStore = create<UserState>()(...)` example
already extracts correctly on upstream `906400870`. The inline-export test
also exercises Redux/Pinia/tagged-template factories through sniffing and
per-symbol record emission in JavaScript, TypeScript and TSX.

The independently reproduced gap is a separate export:

```typescript
const namedStore = createStore({});
export { namedStore as publicStore };
const defaultStore = createStore({});
export default defaultStore;
```

Before the production change, the new scope test failed with
`typescript: missing namedStore`; the original inline-export control passed.
The fix keeps the declaration's local name and line, rather than inventing a
definition for the export alias. It does not expand discovery to private
constants, mutable bindings or function locals. Type-only and remote exports
do not publish a local value. The three new tests assert these boundaries.
This is related work for #2579, not a reproduction or explanation of its
original private-monorepo incident; the PR must not claim to close that report.

Grammar evidence: MIT-licensed tree-sitter-javascript 0.23.1 and
tree-sitter-typescript 0.23.2. The declaration/call patterns and the export
`name`/`alias`, `source` and `value` fields were read from their shipped
`queries/tags.scm` and `src/node-types.json` (TS uses
`typescript/src/node-types.json:1924` and `:1958`). No peer implementation was
copied. Existing scope rationale was read from commits `11d5a5199` (#285) and
`2dfcd41c5` (#304).

## Commands and observed boundaries

Run Cargo commands from `engine/`:

```sh
cargo test --locked --offline -p xerj-autoindex --lib issue_2579 -- --nocapture
cargo fmt --all --check
cargo build --locked --offline --release -j 32 -p xerj-autoindex
cargo clippy --locked --offline -p xerj-autoindex --all-targets -- -D warnings -A dead_code
```

Strict Clippy without the allowance refuses the existing unused
`content::resolve_reporting`; no source-level lint allowance was added.

The original complete candidate library run had 1,874 passed / 3 failed / 2
ignored. Unmodified `906400870`, in a separate clean worktree, had 1,871
passed / the same 3 failed / 2 ignored. The failures were:

- `content::tests::lossy_display_collisions_keep_distinct_alias_identities`
- `corpus_data_skip_tests::corpus_index_summary_splits_indexed_extensions_from_ignored_data`
- `inventory_delta_tests::a_non_utf8_excluded_name_is_matched_on_disk_via_path_id`

The two filename cases report macOS `Illegal byte sequence`; the corpus case
counts one ignored CSV where the assertion expects two. No assertion was
changed. An explicit three-filter complete crate run passed 1,916 tests with
5 existing ignored. The final composed run is recorded below separately.

The first sandboxed full run also failed HTTP fixtures because listening on
loopback was denied; meaningful HTTP runs were repeated outside that sandbox.

After rebasing onto `7d3ca96b1`, the complete crate command below passed
**1,927 tests, 0 failed, 5 existing ignored, 3 explicit baseline filters**:

```sh
TMPDIR=/private/tmp cargo test --locked --offline -p xerj-autoindex -- \
  --test-threads=2 \
  --skip content::tests::lossy_display_collisions_keep_distinct_alias_identities \
  --skip corpus_data_skip_tests::corpus_index_summary_splits_indexed_extensions_from_ignored_data \
  --skip inventory_delta_tests::a_non_utf8_excluded_name_is_matched_on_disk_via_path_id
```

The composed default-thread run first failed the unchanged PDF pipe-close
timeout test (`timeout took 30.015781292s`, expected under 10 s). The final
low-concurrency run includes that test and it passed; no PDF test or timeout
was modified. This remains evidence of test sensitivity, not a diagnosed
PDF fix. Final scoped release build, formatting and scoped Clippy with the
same existing allowance passed on the composed tree.

## Real CLI/MCP lifecycle

From the repository root:

```sh
python3 tools/factory-definitions/smoke.py --binary engine/target/ci-test/xerj
```

The harness snapshots the executable into a private temporary directory and
prints its SHA-256/version, generates 3 files (826 bytes), starts a private
lexical-mode loopback node, dry-runs, indexes, checks 9 declarations over CLI
and MCP, repeats the unchanged index, then stops/restarts and checks again.
CLI/MCP comparison uses hits, excluding request-dependent timing fields.
No existing node, index, personal source or model is used. Only the temporary
node uses a 99% disk flood watermark, leaving 1% reserved, to admit this tiny
fixture on the shared laptop that has reached its default 95% threshold.

The final source-built rc.100 executable snapshot is
`bf8e611d814f935e5a313cb735f33fb3d7ff532b78c04a5fe3af87eb020f3853`.
First indexing completed in 2.6 s, unchanged indexing in 0.3 s; these are
fixture lifecycle observations, not a performance benchmark. It produced 12
records (3 file records and 9 declaration records). All 9 CLI/MCP lookups
passed before/after the unchanged rerun and after a graceful restart.

The same final executable also passed the full ES-YAML suite on a fresh
private node: **1,449 passed, 0 failed, 10 skipped, 231 files**; 75 cases use
the existing catch-lenient policy. Runner and node exited 0. Reproduction:

```sh
python3 tools/factory-definitions/smoke.py \
  --binary engine/target/ci-test/xerj \
  --runner engine/target/ci-test/es-yaml-runner
```

That combined run repeats the nine-definition lifecycle check after the
compatibility suite (index 0.9 s, unchanged rerun 0.3 s, same 12 records).
An earlier pre-rebase suite attempt exceeded the initial 300-second harness
timeout without completing; it is excluded from the gate claim. The final
harness allows 900 seconds and completed normally.

## Review and limits

Independent branch, initially from `906400870`, rebased onto `7d3ca96b1`;
no prerequisite PR or duplicated prerequisite commits. The later upstream
session-import/corpus changes are preserved. No query scoring, endpoint,
mapping, journal format, workflow, dependency or feature gate changes.

Existing unchanged files are not automatically re-extracted: use a new index
prefix and state directory to rebuild without altering the old corpus.
Cross-version migration of a production journal was not exercised. The
isolated fresh build, unchanged run and restart are the lifecycle evidence.
Distributed writes, accelerators, model execution, telemetry migrations and
installation/deployment changes are not applicable. No retrieval-quality, speedup,
real-monorepo coverage, Linux or Windows claim is made. Arbitrary private
constants, destructured bindings and wrapped (`as`/parenthesized) initializers
are outside this scoped change. Hosted merge-ref CI is separate from local
receipts.
