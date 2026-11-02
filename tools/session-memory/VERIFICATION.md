# Session handoff verification (2026-11-01)

Written and executed by OpenAI Codex, operated by Jarvis636431, on macOS arm64
with Rust 1.97.1. Implementation `b099cde6` is based directly on upstream
`720b9990`; no unmerged prerequisite or duplicate prerequisite commits.
The source-built candidate reports `xerj v1.0.0-rc.100`. These receipts do not
claim this feature exists in the published release.

## Regression and final checks

Before implementation, added and ran from `engine/`:

```sh
cargo +1.97.1 test --locked --offline -p xerj-autoindex memory_hooks_are_an_explicit_init_opt_in
```

Observed **0 passed, 1 failed**:

```text
memory opt-in refused: Some("xerj init: unexpected argument '--memory'")
```

The final crate run passes this regression and the ten new session-memory
unit tests. They cover configuration merge/no-op/remove, dry-run, malformed
settings, edited/duplicated ownership, worker exclusion, remote/symlink refusal,
bounded quoted citations, independent deliveries, offline retained work,
redaction and mismatched-session refusal.

Commands from `engine/`:

```sh
cargo +1.97.1 fmt --all
cargo +1.97.1 fmt --all -- --check
cargo +1.97.1 build --locked --offline --release -j 32 -p xerj-autoindex -p xerj-server -p es-yaml-runner
TMPDIR=/private/tmp cargo +1.97.1 test --locked --offline -p xerj-autoindex -- --test-threads=1 --skip content::tests::lossy_display_collisions_keep_distinct_alias_identities --skip corpus_data_skip_tests::corpus_index_summary_splits_indexed_extensions_from_ignored_data --skip inventory_delta_tests::a_non_utf8_excluded_name_is_matched_on_disk_via_path_id
cargo +1.97.1 clippy --locked --offline --no-deps -p xerj-autoindex --all-targets -- -D warnings -A dead_code
```

Observed: scoped release build and formatting passed; **2017 passed, 0 failed,
5 existing ignored, 3 filtered** across the crate run. The filters are existing
macOS filesystem exclusions recorded in [session-import verification](../session-import/VERIFICATION.md).
No existing assertion or skip policy was weakened. Local mock HTTP ports were
allowed. Clippy passed with the existing `content.rs::resolve_reporting`
dead-code allowance. No dependency/lockfile change. Python compilation,
diff whitespace and workflow timeout guard passed (41 jobs / 13 workflows).

Executed binary SHA-256:
`ff806fdede92049e24de9e98f243a5c4eed1fdf3ebd0b668eff7295deb4234ed`.
The copied executable hashed is the same private copy actually run.

## CLI/HTTP workflow

From the repository root:

```sh
python3 tools/session-memory/smoke.py --binary engine/target/release/xerj
python3 tools/session-memory/smoke.py --binary engine/target/release/xerj --authenticated
```

Both passed on the same candidate, with an insecure private node and with
default node authentication plus a temporary runtime `XERJ_API_KEY`:

```text
PASS dry-run, quoted paths, install and unchanged re-install
Synthetic input: 645 bytes; dry-run output captured
PASS redaction, 2 searchable records, one memory and cited SessionStart brief
PASS duplicate event idempotence and changed-transcript replacement (3 records, one memory)
PASS offline cached brief, malformed-tail atomicity and retained-job recovery after restart
PASS uninstall keeps foreign hooks and data; stale hook is a silent no-op
PASS documented ownership-plan cleanup and dedicated memory deletion
```

The harness invokes commands read from installed settings, including binary
and project paths containing apostrophes/spaces. Every hook exited 0 with empty
stderr; SessionEnd stdout was empty. Each run's two nodes exited 0. The archive
excluded fixture email, thinking and tool payload sentinels. Repeated events
kept one archive/memory and stable corpus counts; changed input kept two
immutable archives and replaced the current session export. These are small
correctness fixtures, not benchmarks.

The scoped ES-YAML runner used another fresh temporary node, with
`--url <private-node-url> --dir engine/tests/es-compat-yaml/yaml` from the root:

```text
ES-COMPAT YAML RUNNER · 233 files
1453 passed · 0 failed · 10 skipped · 1463 total
74 cases ran catch-lenient (an expected error XERJ does not raise yet; see catch-lenient.txt)
private conformance node exit 0
```

Skip/catch-lenient policies are unchanged. This is the repository suite,
not complete ES-protocol coverage. Hosted PR CI remains separate.

## Contribution-review audit and limits

- Effective diff preserves ordinary init, explicit import, upstream workflows,
  parser contracts and unrelated hooks. Production changes stay in the
  autoindex client crate. There is no new server API, engine storage format,
  dependency or Cargo feature. Setup is explicit and loopback-only.
- Queue delivery IDs, immutable archives, current exports, stable memory IDs
  and post-publication receipts serve different purposes. Failed publication
  retains pending work; replay reconciles identities. Memory/index/receipt
  publication is not one cross-system transaction. Removal rechecks its lease
  and keeps data; an autoindex child inherits the lease if its supervisor dies.
- Real tests cover first run, unchanged/changed input, malformed partial input,
  pending retry, node restart, cached reads, removal and documented cleanup.
  Config tests refuse busy workers and edited/foreign/duplicated/symlinked
  state. No model or hub corpus is used.
- Unsupported: Windows, remote nodes, Codex lifecycle hooks. Not exercised:
  actual Claude Code host delivery/process cleanup, Linux execution, forced
  worker SIGKILL, hostile same-user filesystem races, large histories, every
  transcript variant, authenticated automatic key discovery, or neural nodes.
  Authenticated smoke uses an explicit runtime key. There is no comprehensive
  secret-redaction guarantee, automatic retention purge or old-state migration.
  Unknown versions/foreign project configurations refuse.
- Engine migrations, query accelerators, feature-gated algorithms, ambiguous
  schema matching and performance/resource optimization are not applicable:
  those paths are unchanged. Existing feedback asks are unchanged; the
  agent-facing-file study was read before editing init help.

Reference code retrieved before implementing (baseline `720b9990`):
`init.rs:415` (backup), `init.rs:448` (merge), `init.rs:657` (ordinary setup),
`sessions.rs:280` (bounded redacting import), `memory_api.rs:389` / `1318`
(store/list), `state.rs:817` / `822` and
`tests/journal_lock_inheritance.rs:108` (open-file-description lock lifetime).
No peer implementation was copied. The [official Claude Code hook protocol](https://code.claude.com/docs/en/hooks)
was retrieved on this date for event fields, context output and SessionEnd's
short budget; import/index work is detached for that reason.


## PR review follow-up (2026-11-02)

Review target: `256fa5cbb72b65656268a89da1282c972a2414d7`, still directly
based on upstream `720b9990`. This follow-up addresses the queue/Unicode
blockers and the egress-inventory CI failure, without changing the node API,
index schema, dependencies, hooks protocol or existing test exclusions.

Observed before production changes: the three new `review_` regressions failed
for the intended reasons: the healthy queue sibling had no archive, removal
preview did not say `would disable`, and `clean()` retained U+202E, U+200B,
U+E0049, U+2066, U+FEFF and U+00AD. The pre-fix run selected two unrelated
existing tests as well; both passed. The hosted failing egress job independently
identified the unclassified `session_memory.rs` client.

The queue now snapshots a bounded batch before retiring entries and attempts
siblings even after import or finalization failure. Only explicit terminal
pre-publication refusals (missing/oversized transcript, session/queue identity
mismatch, malformed queue JSON) are isolated in private `rejected/`; original
bytes and a payload-free reason file are retained before removing pending work.
Other failures, including an incomplete transcript and an offline node, remain
retryable. Existing queues/configs need no migration. Status version stays 1;
`state: rejected` is additive and an unknown session is JSON null.

The new state-transition coverage includes multiple offline sessions, terminal
refusals mixed with a valid import, retention on quarantine-publication failure,
and retry after that obstruction is removed. Unicode category filtering uses
the existing regex dependency's Cc/Cf/Zl/Zp tables at both excerpt creation and
brief display, including old cached/remote data. No ASCII-only fallback or
hand-maintained Unicode ranges were introduced. Archives and full search
exports retain the existing importer's source-preservation contract.

Other review follow-ups: `--remove` without `--memory` is asserted, dry removal
names its action, the directory builder uses its mutable API on non-Unix too,
and the guide explains reinstalling hooks after a binary-path change. The
double dry-run/index traversal is retained under AGENTS.md's estimate-before-
index contract, with the shared deadline and unmeasured history cost documented.
The new client is classified as CLI-only, with loopback data flow documented
in RERANK.md and the air-gapped inventory; node DATA_EGRESS remains unchanged
because no node-side connection was added.

Local verification commands (from `engine/` unless a path says otherwise):

```sh
cargo fmt --all
cargo fmt --all -- --check
TMPDIR=/private/tmp cargo test --locked --offline -p xerj-autoindex --lib session_memory -- --test-threads=1
cargo test --locked --offline -p xerj-rerank --test egress_inventory
cargo clippy --locked --offline --no-deps -p xerj-autoindex --all-targets -- -D warnings -A dead_code
cargo build --locked --offline --release -j 32 -p xerj-autoindex -p xerj-server -p es-yaml-runner
```

The first complete-crate attempt in the restricted sandbox hit existing
loopback/process restrictions; its failures are not a passing receipt. The
host-permitted run uses the three pre-existing macOS filesystem filters listed
above. The existing `content::resolve_reporting` warning remains unchanged.
No existing assertion or skip policy is weakened.

Limits remain: actual Claude Code host delivery and shutdown, Linux/Windows
execution, worker SIGKILL, hostile same-user races and large histories are not
locally verified. No performance or broad retrieval-quality claims are made.

The added `--remove` assertion was also mutation-checked in isolation: extracted
the exact production `InitFlags`/`read_init_flags` and its regression module,
compiled with `rustc --test --edition=2021` plus the existing DEFAULT_URL
constant, and ran both controls. Unmodified source passed its one test (exit 0);
removing only the `if remove && !memory` guard failed it (exit 101). This was an
isolated parser/test-source probe, not a mutated full-crate build; production
source was not changed during concurrent builds.

Final-source test receipts: complete autoindex crate **2023 passed, 0 failed,
5 existing ignored, 3 existing macOS filesystem filters**; focused session
suite **16 passed, 0 failed**; complete default-feature rerank crate **59 passed,
0 failed**, including all four egress-inventory checks. The final autoindex
Clippy command above passed with only the explicit existing dead-code allowance.
Formatting/check, Python compilation (temporary external bytecode cache),
whitespace and workflow-timeout guard passed.

Contribution-review audit: effective ancestry stays independent of other open
PRs. Existing init/manual import, auth, hooks ownership, lock inheritance,
source redaction, retryable malformed tails, offline caches, restart and
explicit cleanup remain covered. Terminal rejection happens before archive,
export, index or memory publication; failure to retain a rejected entry keeps
its pending bytes. Original queue bytes and reason files are separate writes,
so this is not a cross-file crash-transaction guarantee. Memory text filtering
has no stored-index migration: new memory writes are clean and old cached or
remote briefs are sanitized on display. There is no retention purge or implicit
revival of a rejected delivery. The new recovery route is a corrected fresh
SessionEnd, or explicit manual import for oversized transcripts.

N/A: node API/ES semantics, index-schema migrations, clustering, model/provider
execution identity, telemetry, optional accelerators and feature algorithms:
none changes here. No performance claims or public release validation. Existing
operational setup/removal commands retain their contracts; the smoke exercises
the installed commands on owned temporary data, including negative opt-in/
dry-run checks, rejected input, offline/restart recovery and explicit cleanup.

Final shipping-profile build passed with the command above. Both committed
smoke invocations passed on executable SHA-256
`f860ba006cd1cedfb41589c609c912de213ba6416d400ad9184ba4ad5d5f6d11`:

- Insecure and API-key-authenticated temporary lexical nodes used the same
  source-built rc.100 executable bytes; no personal transcripts or hub packs.
- Missing/oversized/mismatched/corrupt deliveries were retained as four rejected
  queue files plus four reason files; the healthy sibling completed normally.
- The synthetic transcript was 669 bytes. Searchable records changed from two
  to three while memory remained one entry; duplicate events did not grow either.
- New memory writes and displayed briefs excluded bidi, zero-width and tag
  characters. An intentionally contaminated legacy offline cache was filtered
  again at display. Existing redaction/citation and byte/line bounds passed.
- The executable was moved to a new quoted path, hooks reconfigured, and the
  installed commands then executed from that path. Plain `--remove` refused
  before settings/state writes, and removal dry-run said `would disable`.
- Offline cache, malformed-tail publication refusal, retained-job recovery
  after node restart, ownership-plan cleanup and memory deletion passed.
  Uninstall retained all rejected bytes/reasons and foreign hooks. Stale hooks
  stayed silent. Every observed hook invocation exited 0 without stderr;
  every private node stopped with exit 0.

Final same-snapshot ES-YAML gate, against a fresh temporary insecure lexical
node, using the unchanged release runner and `--dir tests/es-compat-yaml/yaml`
from `engine/`:

```text
ES-COMPAT YAML RUNNER · 233 files
1453 passed · 0 failed · 10 skipped · 1463 total
74 cases ran catch-lenient
Runner exit: 0
Private node exit: 0
```

The outer Python wrapper's first 240-second cap interrupted an incomplete run;
its node also stopped normally. Repeating with a 900-second cap and retained
raw runner output completed in 280.98 seconds, with the same executable hash
above and no runner/YAML/policy edits. Those caps are local harness bounds,
not product timeouts or performance claims. Hosted CI for the pushed commit
remains a separate verification.
