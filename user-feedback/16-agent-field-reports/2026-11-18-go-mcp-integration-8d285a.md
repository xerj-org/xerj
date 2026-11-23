# Go MCP integration (2026-11-18)

## Setup

xerj v1.0.0-rc.104 · linux x86_64 · corpus: Go project, ~344 files, 1.5 MB.

## What I did

Wiring xerj into a custom mcp client via `xerj init` and using xerj_search and xerj_def mcp tools.

## Notes

Autoindex: 12.0 s. Would help to have a `--dry-run` flag to preview what autoindex will touch.
## Numbers

`xerj autoindex ./` → 12.0s, 344 files, 13634 records, top score 2002.

**Filed alongside:** no defects encountered
