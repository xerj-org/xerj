# Local startup-diagnostics verification — 2026-10-26

OpenAI Codex, operated by Jarvis636431. macOS arm64, Rust 1.97.1.
Implementation `f1d97951`, independently based on upstream `9cf3f29c`.
No dependency on the catalog-namespace, Codex-import (#2413) or batch-benchmark
(#2384) branches. `git merge-tree --write-tree` composed the two diagnostic
branches without conflicts; their execution is separately verified below.

## Observed checks

From `engine/`:

```sh
cargo +1.97.1 fmt --all
cargo +1.97.1 fmt --all -- --check
TMPDIR=/private/tmp cargo +1.97.1 test --locked --offline -p xerj-server \
  --bin xerj --test data_directory_startup -- --test-threads=1
cargo +1.97.1 clippy --locked --offline --no-deps -p xerj-server \
  --bin xerj --test data_directory_startup -- -D warnings
cargo +1.97.1 build --locked --offline --release -j 32 -p xerj-server -p es-yaml-runner
```

- New startup integration test on the unmodified base: **0 passed, 1 failed**
  because the real banner lacked `Data source`.
- Candidate: **123 main tests passed** (including 4 new helper tests),
  **1 startup integration test passed**. The latter checks four source cases
  and their existing-directory restarts: selected config, `XERJ_CONFIG`, flag
  override and a selected config that omits the key/defaults to `./data`.
- Scoped release build, formatting, whitespace and scoped strict Clippy passed.
  No tests skipped/filtered in these server targets. This is not a full run of
  every server integration target or every workspace crate.
- Workflow timeout guard passed (40 jobs / 12 workflows). Python compilation
  passed. New Markdown is covered by the explicit `!tools/**/*.md` rule.

A private standalone copy of `startup_data_dir.rs` passed all 4 helper tests.
Changing its successful `create_dir` branch to report `created=false` made
exactly the nested-directory and file-refusal/recovery regressions fail
(2 failed, 2 passed). Production source was not mutated. This independently
checks that new-directory reporting is exercised, beyond matching banner text.

From the repository root:

```sh
python3 tools/startup-diagnostics/smoke.py --binary engine/target/release/xerj
```

Release candidate `xerj v1.0.0-rc.99`, SHA-256
`861f50c47d2ce7e0af86c1ce9b3588d16398e0739580c8ae6142c46282ebd9cd`,
was copied to a private executable snapshot used by lifecycle and conformance
runs. No source changed between the release build and those runs.

The private-node smoke passed: first launch reported `created this launch`,
stored one document, then exited gracefully with status 0. Restart reported
`existing directory`, returned the same document/body and exited 0. Both
structured JSON diagnostics and the banner identified the selected config and
resolved path. The test fixes `XERJ_LOG=info`; an initial harness invocation
inherited host `RUST_LOG=warn` and correctly could not observe INFO events.
No logging-policy or production-source change was needed for that correction.

On a separate fresh private node using the same snapshot:

```sh
engine/target/release/es-yaml-runner --url "$PRIVATE_NODE_URL" \
  --dir engine/tests/es-compat-yaml/yaml
```

**230 files: 1,447 passed, 0 failed, 10 skipped, 1,457 total; exit 0.**
The unchanged policy reported 75 catch-lenient cases. Node shutdown exited 0;
no YAML, runner or policy was edited. These are local source-build receipts,
not a verification of published release assets or hosted merge-ref CI.

## Audit and boundaries

Audited against `docs/CONTRIBUTION_REVIEW.md`: effective diff is limited to
server config provenance/startup presentation, directory creation reporting,
one existing workspace dependency, tests and documentation. The two TOML
parses use the same file bytes; typed validation still precedes overrides and
startup mutation. Atomic leaf creation determines status; another writer's
existing directory is not claimed as this launch's creation. Existing-file
refusal preserves bytes, and recovery at a new path succeeds. Symlink reuse,
nested paths and control-character escaping are covered. Display fallback
does not become a new availability requirement.

No index mapping, journal, storage format, auth policy or data-directory
selection/auto-port rule changes. No previous-directory lookup, migration or
deletion occurs. Recovery is the existing explicit `--data-dir` / `--config`
selection; the program does not guess where earlier data lives. Legacy data
migration, distributed writes, optional accelerators and model execution are
not applicable. No performance claim is made.

Not tested: Linux/Windows execution, real owner-account state, production
directories, other-known-directory comparisons or console empty-state changes.
These last two suggestions remain outside this scoped portion of #2409.
Website generation gates were not run because no landing files changed.
