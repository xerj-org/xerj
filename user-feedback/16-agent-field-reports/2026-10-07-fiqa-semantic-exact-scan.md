# Diagnosing and fixing slow semantic/hybrid search on BEIR FiQA (2026-10-07)

**Agent:** Claude Code (Claude Opus 5.5)  ·  **XERJ:** xerj v1.0.0-rc.87 (built from source), rebased onto rc.88  ·  **Platform:** macOS arm64 (Apple M5, 24 GB)

**Pointed at:** BEIR FiQA (57,638 docs, `semantic_text` body) loaded with `benchmarks/beir-hybrid/load.py` on a throwaway node.

**Used it for:** vector and hybrid search: reproducing #1091 (13-26 s hybrid queries), then A/B-testing the fix.

**Verdict:** The diagnosis was easier than the issue suggested. It reproduced with the default lexical embedder, so the neural model was a red herring, and `XERJ_TRACE_SEMANTIC_PHASES` plus the `segment_hydration_cache` block in `_nodes/stats` showed where the time went: per-segment stored-document decodes, a full hydration budget, and the memory-watermark drain wiping the caches. The real cost is opacity. Nothing in a normal response says "this field cannot use HNSW because some documents are passage-chunked", so a user sees a slow query with no reason; the guard's exit is only visible by instrumenting it. Two traps cost me time. A flush splits across memtable shards, so small segments are LZ4 and the typed projection does not apply to them. And `load.py` stopped at the first 429 from the memory breaker, so my first A/B silently compared a 36k-doc index against a 57k one. The storage layer already had exactly the primitives the fix needed (typed kNN projection, row-selective hydration), unused by the engine.

**Numbers:** FiQA, 50 queries per arm, matched release builds of `e4c6c027` and the fix: semantic p50 3,865 → 261 ms, hybrid p50 5,772 → 289 ms, memory-watermark crossings 25 → 0, ranked ids identical 50/50 in both arms. ES-YAML on the final head: 1385 passed, 0 failed. `cargo build --release -p xerj-server` 4-8 min.

**Filed alongside:** PR #1217 (fix for #1091)
