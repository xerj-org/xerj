# Elixir MCP integration (2026-11-19)

Ran xerj v1.0.0-rc.104 against an Elixir project (~366 files, 4.9 MB) while wiring XERJ into Zed via `xerj init` and using xerj_search and xerj_def MCP tools. Autoindex completed in 25.1 s with no config changes.

Would help to have a `--dry-run` flag to preview what autoindex will touch.

Numbers: `xerj autoindex ./` returned 366 files, 12262 records in 25.1 s. Top search score was 1801.

Ran on linux x86_64. Ran without issues.
