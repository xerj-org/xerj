# Search hint verification — 2026-11-02

Written and executed by OpenAI Codex, operated by @Jarvis636431, on macOS arm64
with Rust 1.97.1. Independent branch based on upstream `9422d0f53`; builds on
already-merged session hooks (#2849), not an unmerged prerequisite. The diff
contains no Cargo dependency/lockfile, node API, storage, workflow or MCP schema
change. The candidate is source-built rc.100, not a published-release claim.

## Failing controls and regression coverage

Before implementation, the new init flag regression failed on `--hooks`
(`search_hints_are_an_explicit_separate_opt_in`: 0 passed, 1 failed).
It passes after implementation. The existing ordinary-init and session-memory
tests remain; none was weakened.

A temporary mutation from `INTERVAL = 10` to `INTERVAL = 0` failed the persistent
rate-limit test at `assertion failed: fire(p.path(), "a", "Grep").is_none()`.
Restored the source before final checks. An earlier mutation attempt failed
in the linker because the disk was full; that attempt is not defect evidence.

The HTTP regression first failed its `redirect` assertion: reqwest's
`error_for_status` does not reject 302, even with redirects disabled. Added an
explicit success-status check; the same regression now passes. Near-limit
settings are refused before publishing a configuration that could not be read
back. Settings-publication failure is injected after config publication: exact
old settings/config bytes survive, the hook still works, and removal succeeds
on retry.

Coverage lives in `engine/crates/xerj-autoindex/src/search_hints/tests.rs`:
configuration dry-run/merge/idempotence/binary-path update, exact ownership,
foreign and memory hooks preserved, malformed/edited/duplicated settings,
restore-and-remove recovery, failed publication rollback, read and response
budgets, disabled/busy/corrupt state, session isolation, bounded session FIFO,
third Read and ten-call spacing, catalog latest-root ownership, truncated and
ambiguous metadata, unsafe prefixes, wrong host events/cwd, HTTP/3xx handling,
no tool input in the request, and remote/symlink refusal.

## Commands

From `engine/` (loopback/process permission allowed for tests):

```sh
PATH="/usr/bin:/bin:/usr/sbin:/sbin:$PATH" GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 CARGO_INCREMENTAL=0 TMPDIR=/private/tmp cargo test --locked --offline -p xerj-autoindex -- --test-threads=2 --skip content::tests::lossy_display_collisions_keep_distinct_alias_identities --skip corpus_data_skip_tests::corpus_index_summary_splits_indexed_extensions_from_ignored_data --skip inventory_delta_tests::a_non_utf8_excluded_name_is_matched_on_disk_via_path_id
CARGO_INCREMENTAL=0 cargo test --locked --offline -p xerj-rerank
cargo clippy --locked --offline -p xerj-autoindex --all-targets -- -D warnings -A dead_code
cargo fmt --all -- --check
cargo build --locked --offline --release -j 32 -p xerj-autoindex
cargo build --locked --offline --profile ci-test -j 8 -p xerj-server -p es-yaml-runner
```

The three explicit filters are the previously documented macOS filename cases
in [session-import verification](../session-import/VERIFICATION.md); the changed
code does not touch those paths. The `dead_code` allowance is for existing
`content::resolve_reporting`. Shipping server fat-LTO linking was stopped on an
earlier snapshot; no completed shipping-server release build is claimed. The
changed crate has its own scoped release build; live tests use `ci-test` nodes.
A stale build-script cache pointed at an old deleted worktree; touching only
`xerj-server/build.rs`'s timestamp regenerated it (no source diff). Build-cache
cleanup retained source/data and did not use `cargo clean`.

From the repository root:

```sh
python3 tools/search-hints/smoke.py --binary engine/target/ci-test/xerj
python3 tools/search-hints/smoke.py --binary engine/target/ci-test/xerj --authenticated
python3 .github/scripts/workflow-timeout-guard.py
PYTHONPYCACHEPREFIX=/tmp/xerj-hint-pycache python3 -m py_compile tools/search-hints/smoke.py
git diff --check
```

The smoke harness copies the binary to an immutable, privately owned path
containing an apostrophe and space, hashes that copy, and actually invokes it.
It uses a fresh loopback node, generated config, a 99% disk flood watermark to
admit the tiny fixture on this shared disk, a two-core resource setting and the
default lexical mode. Only this temporary node/data and owned `hint-smoke`
corpus are touched. Authentication mode passes a fresh runtime `XERJ_AUTH` / `XERJ_API_KEY`;
this does not independently verify automatic key discovery.

## Contribution review and boundaries

Audited against `docs/CONTRIBUTION_REVIEW.md`: effective diff/ancestry,
project ownership, preflight ordering, independent rate state, bounded reads,
publication failure and recovery, output routing, read-only HTTP, platform
conditions, metadata ambiguity, auth origin and egress inventory. The existing
session-memory primitives change only visibility, not behavior. The host format
is from the official Claude Code hook reference; fs2's MIT/Apache-2.0 nonblocking
lock behavior and in-tree catalog cohort logic are cited in README.md. No peer
implementation was copied.

The local config/counters are version 1 and session IDs are hashed. FIFO eviction
resets the oldest session's quota; no unbounded per-session file growth. Setup
backs up previous settings, disables before removal, and rolls back an ordinary
settings-write failure. The two files are not a crash transaction. A power loss
or edited group can require the documented manual owned-group restoration.
Hostile same-user races, NFS stalls, forced power loss, Linux/Windows execution,
Gemini, remote nodes, huge catalogs, source freshness and manually deleted-index
recovery were not tested. The catalog cap intentionally abstains beyond 200 run
records. No model, ANN, performance, relevance or spontaneous adoption claim.
No engine schema migration, accelerator, cluster, telemetry or deployment change;
those review sections are not applicable. No landing files are changed. Hosted
merge-ref CI is separate from the local receipts below.

## Final executable and workflow receipts

The final immutable executable copies used by both smoke modes and the fresh
conformance node have SHA-256
`db649d8a45335f5c54bf2ffc0d967008e621a06877b39d6c4c910a291d874c12`.

Both smoke modes passed **26 installed hook invocations each**, every one exit
0 with empty stderr. Fresh/unchanged setup, ordinary init, foreign settings and
session-memory coexistence, malformed/oversized/wrong-event input, offline and
unindexed silence, third Read, quota, independent sessions, autoindex map, cited
local definition lookup, graceful node restart, owned corpus removal, edited
group refusal/restoration, uninstall and reinstall passed. Both private nodes
in each mode stopped with exit 0. The fixture had **2 files / 1.4 KB / 3 records**;
actual indexing walls were **1.7 s** (non-authenticated host sample) and **1.6 s**
(authenticated). These are fixture receipts, not throughput benchmarks.

### Controlled actual-host before/after

```sh
python3 tools/search-hints/smoke.py --binary engine/target/ci-test/xerj --claude-sample --receipt NEW.json
```

Claude Code **2.1.278**, two non-interactive sessions, each capped at four turns,
project-only settings and no persisted host session. The same prompt explicitly
asks for Grep first and an XERJ search *if a hook suggests it*. Baseline with
only our hook group removed: tool list `[Grep]`, **0 XERJ search calls**. Enabled:
`[Grep, Bash]`, **1 XERJ search call**. Both host commands exited 0 with no result
error. [host-sample.json](./host-sample.json) is the actual normalized receipt;
raw transcripts, session IDs and machine paths are not published. This proves
controlled delivery/uptake, not spontaneous preference, search quality, or a
statistically measured adoption improvement. Repeated Read and quota are covered
by deterministic replay, not by additional model-generated sessions.

One full-suite attempt on the URL-bearing final source hit an unrelated existing
`xc::tests::corpus_add_partial_clone_failure_preserves_retry_and_names_the_context`
failure: restoring the temporary offline origin returned `DirectoryNotEmpty`.
The test passed immediately in isolation. Its cause was not established; the
Git executable was `/usr/bin/git`, so no wrapper-caused diagnosis is claimed.
The final full-suite rerun isolates global/system Git configuration as shown
above, with the same three existing macOS filters and no source/test changes.

Final complete crate run with isolated Git configuration: **2037 passed,
0 failed, 5 existing ignored, 3 existing macOS filters**, across 12 test/doc
summary groups. Final focused hook run: **11 passed, 0 failed**. Complete rerank
run: **59 passed, 0 failed**, including the four outbound-inventory checks.
Scoped autoindex release build, final `ci-test` node/runner build, Clippy with
only the existing dead-code allowance, fmt/check, Python compilation, whitespace
and workflow timeout guard passed. The final hint carries the configured node
URL as well as prefix; the regression and both smoke modes assert it explicitly.

Fresh-node ES-YAML on that same executable snapshot, using the unchanged runner:

```sh
engine/target/ci-test/es-yaml-runner --url <fresh-private-loopback-node> --dir engine/tests/es-compat-yaml/yaml
```

Observed **1453 passed, 0 failed, 10 skipped**, 233 files, runner exit 0. It retains
**74 catch-lenient cases** under the existing suite policy. The private node
stopped with exit 0 and its temporary data directory was removed. No suite,
assertion, catch policy or production node was changed. This is the repository
suite, not complete ES protocol coverage. Hosted merge-ref CI remains pending.

### Last upstream composition

After validation and implementation commit `7095b0147`, fetched upstream again
and normally merged `50ea0bb58` (#2845). It changes the field-report HTTP function,
its JavaScript tests/CI step and changelog only. The merge was clean, including
the changelog. `git diff 7095b0147 -- engine` is empty: the entire Rust source and
lockfile serving the executable/gates above are unchanged. The upstream function
and workflow are preserved; they are not part of this PR's effective diff.
The newly merged plain-Node function tests and workflow timeout guard passed;
Rust tests were not repeated for that JavaScript-only upstream composition.
