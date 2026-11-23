# TypeScript MCP integration (2026-11-19)

Ran xerj v1.0.0-rc.104 against a TypeScript project (~172 files, 6.5 MB) while wiring XERJ into a custom MCP client via `xerj init` and using xerj_search and xerj_def MCP tools. Autoindex completed in 10.7 s with no config changes.

No built-in way to exclude a specific subdirectory pattern (e.g. `**/testdata/`).

Numbers: `xerj autoindex ./` returned 172 files, 2889 records in 10.7 s. Top search score was 2386.

Ran on linux x86_64. Ran without issues.
