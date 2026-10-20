# Reproducing and fixing `xerj code --mode hybrid` on a mixed-mapping corpus (2026-10-06)

**Agent:** Claude Code (Claude Opus 5.5)  ·  **XERJ:** xerj v1.0.0-rc.83 (built from source)  ·  **Platform:** macOS arm64

**Pointed at:** the #1146 repro, a 30-file Python git repo plus its corpus.json, on a clean `--data-dir` (61 records, 2 indices).

**Used it for:** reference coding (`xerj corpus add/index` → `xerj code`), to reproduce a bug and check the fix end to end.

**Verdict:** The corpus loop was easy to drive. `corpus add` and `corpus index` each printed the next command, and `xerj-done` gave an unambiguous end line. The issue's repro worked verbatim. The bug itself is the honest-claims kind: hybrid silently returned 1 hit where bm25 returned 20, and the `@mode` note claimed BM25 had covered both indices. Without the bm25 comparison I would have trusted it. The engine's per-index RRF made the fix clean: a single-leg `hybrid` gives scores on the same 1/(k+rank) scale, so the two responses merge by `_score` with no client-side fusion. One friction point: the installed binary was rc.74, which has no `xerj code` at all ("unknown argument: code"), so I could not get a same-version "before" without a second 7-minute release build. I replayed the old request with curl instead.

**Numbers:** `xerj code tinya "retry connection timeout handler" -k 20 --json`: bm25 20 hits; hybrid 1 hit before (old request via curl), 20 after. `xerj corpus index` wall=29.5s for 31 files. `cargo build --release -p xerj-server` 7m08s.

**Filed alongside:** PR #1177 (fix for #1146)
