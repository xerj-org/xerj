# Rust MCP integration (2026-11-22)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A Rust project (~179 files, 7.8 MB).

**Task:** Wiring xerj into claude code via `xerj init` and using xerj_search and xerj_def mcp tools.

**Result:** Autoindex in 15.1 s. Hybrid search on 1-2 word queries felt identical to BM25 alone — vector didn't seem to add much there.

**Numbers:** `xerj autoindex ./` → wall=15.1s, 179 files, 9483 records. Top score 2203.

**Filed alongside:** nothing to report
