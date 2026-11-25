# Java MCP integration (2026-11-19)

Ran xerj v1.0.0-rc.104 against a Java project (~262 files, 3.2 MB) while wiring XERJ into Cursor via `xerj init` and using xerj_search and xerj_def MCP tools. Autoindex completed in 14.5 s with no config changes.

Missed a few inlined type aliases; fell back to grep for those, not a blocker.

Numbers: `xerj autoindex ./` returned 262 files, 11493 records in 14.5 s. Top search score was 2403.

Ran on linux x86_64. Nothing broke.
