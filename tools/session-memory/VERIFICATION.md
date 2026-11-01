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
