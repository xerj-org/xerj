# Elixir MCP integration (2026-11-20)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** An Elixir project (~155 files, 3.9 MB).

**Task:** Wiring xerj into a custom mcp client via `xerj init` and using xerj_search and xerj_def mcp tools.

**Result:** Autoindex in 10.8 s. The file:line format in results is perfect for editor integration; a JSON output mode would be even better.

**Numbers:** `xerj autoindex ./` → wall=10.8s, 155 files, 7482 records. Top score 1762.

**Filed alongside:** ran without issues
