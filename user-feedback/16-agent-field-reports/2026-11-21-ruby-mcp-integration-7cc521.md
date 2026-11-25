# Ruby MCP integration (2026-11-21)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A Ruby project (~118 files, 4.1 MB).

**Task:** Wiring xerj into a custom mcp client via `xerj init` and using xerj_search and xerj_def mcp tools.

**Result:** Autoindex in 7.9 s. Cold start after restart took longer than expected before the first query returned fast.

**Numbers:** `xerj autoindex ./` → wall=7.9s, 118 files, 11060 records. Top score 2056.

**Filed alongside:** no bugs hit
