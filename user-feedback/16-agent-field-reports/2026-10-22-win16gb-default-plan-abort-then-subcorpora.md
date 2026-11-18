# Windows autoindex aborts a 16 GB node during ingest, then WAL replay restores every indexed corpus (2026-10-22)

> Written by an AI coding agent (GLM-5.3-Flash Free, OpenCode harness), run by @WYSTAF, who asked for this field trial.

**Agent:** GLM-5.3-Flash Free · OpenCode  ·  **XERJ:** xerj v1.0.0-rc.98  ·  **Platform:** Windows 11 Pro x86_64, 16 GB RAM, pagefileless (commit ≈ physical; ~12 GB in use by other processes)

**Pointed at:** the xerj checkout itself (2,889 files, 159 MB payload, default plan; twice, fresh data dir each time) and then subcorpora `docs/` (304 files) and `engine/crates` (623 files) under separate prefixes with `--no-graph --workers 2 --bulk-mb 4`.

**Used it for:** autoindex + BM25 full-text + code-symbol search (`def`) during a contributor-onboarding session; the goal was a working answerable corpus, honestly measured.

**Verdict:** The node died from allocation-failure aborts on three of the four ingest runs — twice at the default plan (262,144 B at 39.9%, 27,648 B at 34.6% of 2,876 items; memory trace: memtable_active ~800 MB vs process RSS 3.36 GB, ~4× the engine's own logical total, allocator unsupported) and once even under reduced flags (104,515,584 B during flushes/merges, killing the `engine/crates` run at 321.3 s). Meanwhile the autoindex client retried `_bulk` into the dead node with a mangled `client error (Connect): t` message. The smallest corpus (docs/) finished cleanly, search quality was the real product — exact passages with `file:line`, symbol definitions with full signatures — and after the third abort a plain restart replayed the WAL and restored both corpora fully queryable. Recommend gating the default plan on free commit headroom (no pagefile on this box) and adding ingest backpressure instead of aborting at the allocator.

**Numbers:** aborts: 262,144 B at 39.9%, 27,648 B at 34.6% (of 2,876 items), 104,515,584 B under reduced flags; docs corpus completed in 40.1 s — 292 files / 9,557 records / 91 code files, exit 3 (1 junk); ingest rate on the code corpus 661 KB/s early → 47 KB/s at 84% before that run aborted; audited via `xerj gain`: 11 searches, 100% hit rate, p50 44 ms, p95 1,602 ms (the `def` round); `xerj def build_defs_and_files --prefix xerj-code` → `xerj-hub/src/pack.rs:462` with full signature; after WAL replay: 9,557 + 19,080 docs queryable from disk with zero re-indexing.

**Filed alongside:** abort evidence compiled for [#1122](https://github.com/xerj-org/xerj/issues/1122).
