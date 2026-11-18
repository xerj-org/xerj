# Map report verification (#2643)

Written by OpenAI Codex, operated by @Jarvis636431. Verified on macOS arm64,
Rust `1.97.1 (8bab26f4f 2026-07-14)`, source-built XERJ v1.0.0-rc.100.

## Scope and ancestry

Independent branch `feat/2643-map-reports` from upstream `befe92035`.
No dependency on the open CJK judge PR #2635. No engine, storage, API, MCP
schema, dependency, workflow, journal or catalog-format change. The feature
is an autoindex client read/export path. Existing no-flag map and JSON
rendering remain on their original paths.

Submission update: rebased onto upstream `6a7b390ac`, retaining #2697's
MCP batch-response fix and the merged field report #2702. The rebase does
not change the autoindex crate's source tree. The original complete-run
and executable receipts below precede a final output-only correction that
removes the Markdown report's extra terminal blank line. All seven report
tests and the scoped release build were repeated after that correction;
the effective diff was audited again after rebasing. Hosted CI on the
published feature head is separate from these local receipts.

Repository contracts consulted before implementation:

- `engine/crates/xerj-autoindex/src/catalog.rs:245-279`: dataset counts,
  format labels and query metadata.
- `engine/crates/xerj-autoindex/src/detect/mod.rs:158-170`: graph file-card
  anchor identities.
- `engine/crates/xerj-api/src/graph_api.rs:1374-1397`: overview hub shape.
- `engine/crates/xerj-autoindex/src/sessions.rs:166-170`: no-clobber staged
  publication. The report reuses `NamedTempFile::persist_noclobber`.
- `engine/crates/xerj-autoindex/src/esclient.rs:2344-2350`: filtered metadata
  searches with savings accounting disabled, avoiding full body decoding.

These are the repository's Apache-2.0 code; no peer implementation or new
third-party dependency was copied. This is client templating, not new engine
or ranking implementation.

## Tests, builds and static checks

First added the report CLI test to the original implementation and ran it:
`map_report_flags_select_export_and_refuse_ambiguous_modes` failed with
`--html: Err("unknown argument: --html")`. It passes on the candidate.

Run from `engine/` (the TMPDIR setting below is macOS-specific):

```sh
TMPDIR=/private/tmp cargo test --locked --offline --profile ci-test \
  -p xerj-autoindex -- --test-threads=2 \
  --skip content::tests::lossy_display_collisions_keep_distinct_alias_identities \
  --skip corpus_data_skip_tests::corpus_index_summary_splits_indexed_extensions_from_ignored_data \
  --skip inventory_delta_tests::a_non_utf8_excluded_name_is_matched_on_disk_via_path_id
cargo build --locked --offline --release -j 32 -p xerj-autoindex
cargo build --locked --offline --profile ci-test -j 8 -p xerj-server -p es-yaml-runner
cargo fmt --all --check
cargo clippy --locked --offline -p xerj-autoindex --all-targets --no-deps -- -D warnings -A dead_code
```

Candidate crate totals: **1,947 passed, 0 failed, 5 existing ignored,
3 explicit baseline filters**. This includes 1,902 passing library tests
and 45 passing integration tests. All seven new report tests pass. They
cover flag conflicts/missing values, escaped HTML/GFM, redaction, source
allowlisting, read-only bounded enrichment, optional 403 errors, scope
isolation, newer no-graph runs, output races, cleanup and dangling symlinks.

The three filters were reproduced on a clean detached `befe92035` worktree:
**1,895 passed, 3 failed, 2 ignored** in its library suite. The two filename
cases fail with macOS `Illegal byte sequence`; the corpus-summary case has
the same unexpected ignored-directory summary. No existing test, assertion
or committed skip was changed. The first unfiltered candidate run also hit
`/var` versus `/private/var` temporary-root comparisons; canonical TMPDIR
removes those failures without changing source or tests.

Scoped release build, final node/runner build and formatting passed.
Strict Clippy without an allowance refuses the existing unused
`content::resolve_reporting`; the command above has only the existing
command-line `dead_code` allowance and otherwise passes `-D warnings`.
Python syntax, `git diff --check` and the workflow-timeout guard passed
(41 existing jobs across 13 workflow files). No workflow was modified.

## Real private-node checks

From repository root:

```sh
python3 tools/map-report/smoke.py \
  --binary engine/target/ci-test/xerj \
  --runner engine/target/ci-test/es-yaml-runner \
  --samples-dir tools/map-report
```

The harness copies the executable before launch. The tested snapshot SHA-256
was `cf4d62083b07d02fca24ba1c0bffac3ddbf3026e2c9e2b61238770672a6d975b`.
On a fresh private node, ES-YAML reported **1,453 passed, 0 failed,
10 skipped across 233 files**, retaining 74 existing catch-lenient cases.
The runner and node exited 0. No suite or leniency rule was changed.

The same executable then indexed the committed synthetic parcel-ledger
fixture under separate graph/no-graph prefixes. The dry-run reported five
files and 1.4 KB of source; four supported files produced 12 records per
run. One injected binary was junked, so both runs succeeded with exit 3,
`ok=true reason=completed-with-junk`. No source data, pre-existing node,
personal corpus, model or external reranker was used.

HTML/Markdown exports, actual symbol names, graph/no-graph notes, path
redaction, existing-file refusal, empty-catalog refusal and byte-identical
ordinary map/JSON before and after exports passed. The final redacted
examples are `sample.html` and `sample.md`; counts are fixed-fixture
observations, not a throughput or retrieval-quality benchmark.

The same snapshot was then rerun with the expanded harness (without
`--runner`): raw paths and reasons, `--dataset docs` filtering, and distinct
HTML/Markdown file headers also passed for both prefixes. The harness and
node exited 0. The examples were regenerated by this final run.

Browser QA: rendered the HTML at 1280 x 900 and 390 x 844, inspecting the
desktop/mobile screenshots. Both had no page-level horizontal overflow;
the browser recorded zero scripts and zero external resource entries.
On the final sample, Playwright loaded its markup, switched the context
offline, and rendered that markup again: five tables, zero scripts, zero
resource entries, and a 390-pixel document in a 390-pixel viewport. This
checks offline rendering without relying on the automation host's blocked
`file://` navigation.

## Review boundary

Audited against `docs/CONTRIBUTION_REVIEW.md`:

- Effective scope is the client CLI/export module, styles, documentation
  and synthetic fixture/harness; upstream workflows and recovery paths
  remain intact.
- An existing output, including a symlink, is refused before contacting
  the node. Rendering writes into a sibling temporary file and uses
  no-clobber publication; a race leaves the prior destination unchanged.
  Failed required catalog reads leave no destination report.
- Optional enrichment failures are explicit unavailable notes, not empty
  corpus claims. No enrichment error body is copied into a report. Missing graph
  metadata does not trigger graph requests. Hydration uses source path
  fields only, scoped to the brain's selected prefixes.
- Paths and skip reasons are omitted by `--redact-paths`; other metadata
  names deliberately remain visible. HTML/GFM escaping prevents variable
  text from becoming active markup. Commands use POSIX shell quoting and
  do not contain configured endpoints or credentials.
- Durable corpus transitions, legacy-schema migration, distributed writes,
  model execution and accelerator fallbacks are not applicable: the feature
  only reads existing node data and publishes a new local report file.

Not verified: Linux/Windows execution, broad corpus relevance, large-node
latency/memory, a comprehensive global file-degree ranking, anonymity of
metadata names, or hosted CI on a published feature PR. The list bounds and
partial-report behavior are documented; no scale or quality claim is made.

Required single-file session feedback: [#2702](https://github.com/xerj-org/xerj/pull/2702).
