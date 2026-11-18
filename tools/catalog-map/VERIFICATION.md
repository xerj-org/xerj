# Catalog namespace verification — 2026-10-26

OpenAI Codex, operated by Jarvis636431. macOS arm64 / Rust 1.97.1.
Implementation `9d02f048`, independently based on upstream `9cf3f29c`; independent of the
data-directory PR #2414, Codex-import PR #2413 and batch benchmark #2384.
The generated schema and final transport controls are in `1f03e684`.
The two diagnostics branches compose without conflicts via `git merge-tree`;
their local execution receipts are separate, not a combined-runtime claim.

## Commands and observed results

From `engine/`:

```sh
cargo +1.97.1 fmt --all -- --check
cargo +1.97.1 build --locked --offline --release -j 32 \
  -p xerj-autoindex -p xerj-mcp -p xerj-server -p es-yaml-runner
TMPDIR=/private/tmp cargo +1.97.1 test --locked --offline -p xerj-autoindex -- \
  --test-threads=1 \
  --skip content::tests::lossy_display_collisions_keep_distinct_alias_identities \
  --skip corpus_data_skip_tests::corpus_index_summary_splits_indexed_extensions_from_ignored_data \
  --skip inventory_delta_tests::a_non_utf8_excluded_name_is_matched_on_disk_via_path_id
RUST_MIN_STACK=16777216 TMPDIR=/private/tmp cargo +1.97.1 test --locked --offline \
  -p xerj-mcp -- --test-threads=1 \
  --skip loopback_ignores_an_unreachable_proxy_and_a_proxied_503_names_the_proxy
cargo +1.97.1 clippy --locked --offline --no-deps \
  -p xerj-autoindex -p xerj-mcp --all-targets -- -D warnings -A dead_code
cargo +1.97.1 clippy --locked --offline --no-deps \
  -p xerj-mcp --all-targets -- -D warnings
```

- Scoped release build, fmt/whitespace and both Clippy commands passed.
  The autoindex command-line dead-code allowance is for the existing
  `resolve_reporting` warning; no source-level suppression was added.
- Autoindex: **1,839 passed, 0 failed, 5 existing ignored, 3 filtered** across
  unit/integration/doc-test invocations. The platform filters and earlier
  baseline reproductions are documented in
  [the existing import verification](../session-import/VERIFICATION.md#why-the-three-filters-exist).
- MCP: **167 passed, 0 failed, 1 filtered** across unit/integration/doc-test
  invocations, including **148 library tests** and published-schema drift.
  Two new library tests check empty/positive maps and actual HTTP 404/403
  tool-error classification; the existing description test gains scope checks.
- The initial full MCP run hit a Tokio worker stack overflow in `local_def`.
  Setting a 16 MiB minimum test-thread stack allowed all 5 local-def tests
  to pass. This is an explicit local test environment setting, not a product
  thread-stack or memory claim.
- The proxy integration above failed at its phase-2 search against
  `127.0.0.2:9`. It was then run alone on an unmodified detached **9cf3f29c**
  checkout with the same stack setting and failed with the same diagnostic.
  The cause was not diagnosed or patched here. Only that local invocation is
  filtered; no test, assertion or CI skip changed. Other MCP tests passed.

## CLI/MCP and generated schema

From the repository root:

```sh
python3 tools/catalog-map/smoke.py --binary engine/target/release/xerj
bash scripts/mcp-schema-check.sh --write
bash scripts/mcp-schema-check.sh
```

Source-built `xerj v1.0.0-rc.99` was copied to a private executable snapshot:
SHA-256 `e4aa924e5cc5d1f3cfde3dc4fb490054245106e139e51d6e4b1d11dd3c4b758f`.
That same snapshot served transport checks, schema generation/check and the
fresh-node ES-YAML gate. No production Rust changed after the release build.

The old map implementation failed exactly **4 empty/missing CLI/MCP scope
checks**; its **6 positive/401/403 controls passed**. The old executable's map
and catalog source was compared against 9cf3f29c and is unchanged; it is an
existing source-built executable, not a claimed clean full-baseline rebuild.
The candidate passed **all 10 CLI/MCP cases**. CLI `--json` stdout also remained
empty on failures and parsed correctly for populated datasets. Requests stayed
within read-only root/search/mapping routes on a private loopback fixture.
No live corpus registry or existing node was used to claim availability.

The generator/check served **22 tools**. Only `xerj_map.description` changed in
the generated JSON; no tool/input schema was removed or manually rewritten.

## Conformance and website gates

On a separate fresh private node using the snapshot:

```sh
engine/target/release/es-yaml-runner --url "$PRIVATE_NODE_URL" \
  --dir engine/tests/es-compat-yaml/yaml
```

Observed **230 files; 1,447 passed, 0 failed, 10 skipped, 1,457 total; exit 0**.
The unchanged policy reported 75 catch-lenient cases; node shutdown exited 0.
No YAML, runner or catch-lenient policy changed.

All 14 website checks passed: article-schema, URL-map and field-report tests;
article/sitemap/head/link/OG generator checks; SEO lint; factcheck self-test,
fixture-check and error gate; full audit; landing-constants guard. These are
the commands in `.github/workflows/seo.yml` plus the constants guard. Non-gating
warnings remain in untouched content/HTML; no HTML was regenerated or edited.
Workflow-timeout guard and Python compilation also passed. New Markdown's
final ignore match is `!tools/**/*.md`.

## Review and scope

Audited against `docs/CONTRIBUTION_REVIEW.md`: existing catalog queries,
filtering, pagination, positive output and CLI exit codes are preserved.
`search_present` uses the same request/retry path and classifies only HTTP 404
as absence; 401/403 and transport/server failures remain errors. MCP 404 keeps
`isError=true` and the raw engine body. Empty successes keep `matched:0` and
`indexes:[]`, adding explanatory metadata rather than dataset rows. No managed
corpus existence or name equivalence is inferred. The static hint triggers no
registry request, fallback search or indexing.

No persistent format, mapping, journal, ownership, mutation or recovery path
changes. Durable-state migrations, failure atomicity of writes, distributed
state, optional optimizations, models and performance benchmarking are not
applicable to this read-only hint change. No speed/quality claim is made.
Not tested: Linux/Windows execution, real managed corpus availability,
production registry configurations or fixes to ranking/identity discovery.
The known proxy fixture failure remains unresolved; hosted CI is separate.


## Independent maintainer review — 2026-10-29

Agent-assisted review on Linux x86_64, composed with the hybrid page-size fix
from #2572 and current main. Original contributor commits remain ancestors.
The source-built `xerj v1.0.0-rc.100` executable has SHA-256 `6de2bc0c54174b79c9bffdb94049e70630b8b029ec547c4b8d08eccdb58e2606`.
The release-tested engine tree is `26f5586be30c715d99bff86fece77616b3f033a4`;
subsequent changes add the YAML cases and this receipt, not runtime Rust.

- Negative catalog transport control: the previous binary failed exactly the
  four empty/missing namespace cases; the six populated/401/403 controls passed.
  The candidate passed all ten cases using `python3 tools/catalog-map/smoke.py
  --binary engine/target/release/xerj`.
- `RUST_MIN_STACK=16777216 cargo test --profile ci-test --locked -j 16
  -p xerj-autoindex -p xerj-mcp -- --test-threads=2`: 2,064 passed, zero failed,
  five existing ignored. No platform filters or skipped test names.
- The new CLI parsing comparison initially exposed concurrent cwd/key discovery
  changes. Holding the existing sandbox `ENV_LOCK` across both parses fixes
  that test race without changing authentication behavior.
- A separate initial default-parallel run failed the existing PDF-worker timeout
  assertion (30.01 seconds versus its ten-second ceiling). An unchanged repeat
  of the complete autoindex library at default parallelism passed all 1,849
  tests, with two existing ignored. That intermittent timeout remains a known
  test limitation; its assertion and the production timeout were not changed.
- The API crate passed 1,009 tests, zero failed, two existing ignored. Restoring
  the old unconditional page widening made the strengthened hybrid regression
  fail; the fix passed size-zero, pagination and complete-union controls.
- Strict Clippy passed for `xerj-api`, `xerj-autoindex`, and `xerj-mcp`, all
  targets, with `-D warnings` and no added allowances. Scoped release build of
  those crates plus `xerj-server` and `es-yaml-runner` passed.
- Live schema generation and checking passed with 22 tools. Only the map
  description changes relative to main; no tool schema is hand-written.
- Fresh private-node ES-YAML: **231 files, 1,449 passed, zero failed, ten skipped**,
  75 existing catch-lenient cases. This includes two new hybrid page-size YAML
  cases. An earlier harness launch hit an occupied adjacent listener port;
  reserving a free three-port range resolved that harness setup failure.
- All 19 local site/guard commands passed: the SEO workflow's tests and generator
  checks, factcheck self-test/fixture/error checks, landing constants, and
  workflow timeouts.

No performance improvement, Windows/macOS rerun, live corpus availability,
production authentication setup, or hosted CI completion is claimed here.
