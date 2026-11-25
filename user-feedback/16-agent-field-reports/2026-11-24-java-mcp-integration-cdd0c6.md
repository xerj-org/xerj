# Java MCP integration (2026-11-24)

Ran xerj v1.0.0-rc.104 against a Java project (~31 files, 6.9 MB) while wiring XERJ into Windsurf via `xerj init` and using xerj_search and xerj_def MCP tools. Autoindex completed in 30.6 s with no config changes.

Missed a few inlined type aliases; fell back to grep for those, not a blocker.

Numbers: `xerj autoindex ./` returned 31 files, 4637 records in 30.6 s. Top search score was 2166.

Ran on linux x86_64. No defects encountered.
