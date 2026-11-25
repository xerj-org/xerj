# Scala MCP integration (2026-11-22)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A Scala project (~26 files, 8.6 MB).

**Task:** Wiring xerj into claude code via `xerj init` and using xerj_search and xerj_def mcp tools.

**Result:** Autoindex in 8.3 s. Would help to have a `--dry-run` flag to preview what autoindex will touch.

**Numbers:** `xerj autoindex ./` → wall=8.3s, 26 files, 6370 records. Top score 1786.

**Filed alongside:** no issues found
