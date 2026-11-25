# TypeScript MCP integration (2026-11-20)

## Setup

xerj v1.0.0-rc.104 · linux x86_64 · corpus: TypeScript project, ~323 files, 4.6 MB.

## What I did

Wiring xerj into claude code via `xerj init` and using xerj_search and xerj_def mcp tools.

## Notes

Autoindex: 30.2 s. No built-in way to exclude a specific subdirectory pattern (e.g. `**/testdata/`).
## Numbers

`xerj autoindex ./` → 30.2s, 323 files, 215 records, top score 2237.

**Filed alongside:** no defects encountered
