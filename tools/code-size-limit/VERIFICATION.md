# Code-cap diagnostic verification (2026-10-31)

Written and executed by OpenAI Codex, operated by Jarvis636431, on macOS arm64
with Rust 1.97.1. This is a source-built candidate reporting
`xerj v1.0.0-rc.100`, not a claim about a published release.

## Regression control

The three new tests were installed on `e5675dec` before the diagnostic/help
fix. Running `cargo +1.97.1 test --locked -p xerj-autoindex code_cap` failed
all three (0 passed, 3 failed). The extractor and scan failures printed:

```text
source file over the 2 MiB code cap (2.0 MiB): too large to be hand-written code (generated stubs, minified bundles), so it is not parsed.
```

The fixture was 2,097,153 bytes: rounding hid its one-byte excess. The help
test failed because the independent code cap was absent. After the fix the
same command passed all three. Tests cover cap minus one, cap, and cap plus
one on both source paths and extensionless snapshot blobs; scan classification
uses a larger whole-file allowance. No parser or cap admission logic changed.

## Final composed tree

The implementation commit is `469eb1b6`, based directly on upstream
`f855d39d`. Rebase retained the Vue/Svelte extraction and tests from #2735;
the only conflict was adjacent test insertion. This branch contains no
unmerged prerequisite commits. #2666 is already in the base.

Commands from `engine/`:

```sh
cargo +1.97.1 fmt --all
cargo +1.97.1 fmt --all -- --check
cargo +1.97.1 build --locked --offline --release -j 32 -p xerj-autoindex -p xerj-server -p es-yaml-runner
TMPDIR=/private/tmp cargo +1.97.1 test --locked --offline -p xerj-autoindex -- --test-threads=1 --skip content::tests::lossy_display_collisions_keep_distinct_alias_identities --skip corpus_data_skip_tests::corpus_index_summary_splits_indexed_extensions_from_ignored_data --skip inventory_delta_tests::a_non_utf8_excluded_name_is_matched_on_disk_via_path_id
cargo +1.97.1 clippy --locked --offline --no-deps -p xerj-autoindex --all-targets -- -D warnings -A dead_code
```

The three explicit filesystem filters are the existing macOS exclusions
documented in [session-import verification](../session-import/VERIFICATION.md).
They were not modified. Strict Clippy without the allowance encounters the
pre-existing `content.rs::resolve_reporting` dead-code warning; no source
allowance was added. No lockfile or dependency changes are part of this PR.

Observed on the composed tree: scoped release build and fmt check passed;
crate suite **1,976 passed, 0 failed, 5 existing ignored, 3 filtered**; scoped
Clippy passed with the allowance above. The crate command needs local test
ports: its first sandboxed attempt failed at port binding with
`PermissionDenied / Operation not permitted`; rerunning with local networking
allowed produced these results. Python compilation and the workflow timeout
guard also passed (41 jobs across 13 workflows).

## Live reproduction and scope

Run the two commands in [README.md](README.md) with the scoped release binary.
The script creates three synthetic ASCII Perl files totaling 6,291,456 bytes,
dry-runs first and uses its own temporary node, data, state and executable
snapshot. It exercises real HTTP catalog reads and CLI `def`, not mocks.
Below/at-cap files expose both leading and trailing functions. The oversized
file remains skipped with zero records and exact limit/file byte counts,
even with `--max-file-mb 6`. Repeated indexing and graceful restart must keep
the same record count and catalog outcome. Both node processes must exit 0.

Observed on the final binary: first run **1.0 s**, unchanged rerun **0.3 s**;
both `ok: true`, exit **3**, reason `completed-with-junk`, 2 indexed code files,
1 skipped code file and **6 live records**. All **4** expected definitions were
retrieved; `over_probe` had no hits. Restart preserved those results; both
nodes exited **0**. The independent `--estimate-only` invocation also passed.
Executable SHA-256 (the copied executable actually run):
`e204f8ac5f7a5c003f55a5fb4d87f68f1aa81440e8379123bf6554a56931254b`.
These are single-run correctness receipts, not performance measurements.

The scoped `es-yaml-runner` was run against a separate empty temporary node
using `--url <private-node-url> --dir engine/tests/es-compat-yaml/yaml` from
the repository root. Final composed-tree output:

```text
ES-COMPAT YAML RUNNER · 233 files
1453 passed · 0 failed · 10 skipped · 1463 total
74 cases ran catch-lenient (an expected error XERJ does not raise yet; see catch-lenient.txt)
private conformance node exit 0
```

Skip and catch-lenient policies are unchanged. This is the repository's current
suite, not a claim of complete coverage of the Elasticsearch protocol.

## Contribution-review audit

- Effective diff: diagnostic/help, three regression tests, changelog and this
  bounded reproduction. No upstream feature, workflow, skip policy, API schema,
  journal format, identity or durable transition is removed or replaced.
- The diagnostic names cause, no extracted records, independent flags and
  `.xerjignore` exclusion. Exclusion is not an indexing workaround. Larger-file
  support and configurable/chunked extraction remain outside this PR.
- First run, unchanged rerun, explicit oversized-file negative control, partial
  corpus retrieval, graceful stop and durable restart are exercised. The
  executable hash is taken from the same private copy that is run.
- Not exercised: changed-input reruns, interruption/pending-state recovery,
  old-plan upgrades, concurrent mutation, metadata-read failure, Windows/Linux,
  neural mode, all language grammars, real reporter data. Existing stored
  reasons are not migrated. There is no new recovery/state algorithm to audit.
- Optional acceleration/fallback, schema migration, destructive preflight,
  feature-gated implementation, mutable-asset identity, performance/resource
  optimization and ambiguous schema selection are not applicable: only strings
  and shared visibility of the existing cap constant change in production.
- No install/publish recipe or privileged artifact handoff is introduced.
  The smoke script owns a fresh temporary tree, starts no existing node and
  deletes only that tree. No hub corpus, model or real user content is used.
- No performance claim is made. No peer algorithm was implemented/copied;
  retrieved in-repository reference locations are listed in README.md.
