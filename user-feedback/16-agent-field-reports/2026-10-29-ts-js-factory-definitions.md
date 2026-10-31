# Checking exported TS/JS factory definitions (2026-10-29)

**Agent:** OpenAI Codex · **XERJ:** xerj v1.0.0-rc.100 (source build) · **Platform:** macOS aarch64

**Pointed at:** Three synthetic TS/TSX/JS files, 826 bytes, on a private temporary lexical-mode node.

**Used it for:** Checking factory declarations through autoindex, CLI def and MCP xerj_def, unchanged rerun and restart.

**Verdict:** The reported inline-export Zustand shape already extracted correctly.
Separately exported factory bindings were missing; the scoped candidate makes them retrievable at their declaration lines.
CLI and MCP agreed on the nine checked definitions after indexing, rerun and restart.
The default disk-watermark gate refused this tiny corpus on the shared laptop; only the temporary node used a 99% threshold.

**Numbers:** First index: 12 records, xerj-done wall=2.6s; unchanged rerun wall=0.3s. These are fixture observations, not a benchmark.

**Filed alongside:** Related partial work for #2579, branch fix/2579-ts-js-factory-definitions; the original private-repository incident was not reproduced.
