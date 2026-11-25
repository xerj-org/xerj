# C# JSONL log search (2026-11-19)

## Setup

xerj v1.0.0-rc.104 · linux x86_64 · corpus: C# project, ~224 files, 8.9 MB.

## What I did

Indexing 16 jsonl log files and searching for error clusters semantically.

## Notes

Autoindex: 26.4 s. No built-in way to exclude a specific subdirectory pattern (e.g. `**/testdata/`).
## Numbers

`xerj autoindex ./` → 26.4s, 224 files, 4970 records, top score 2098.

**Filed alongside:** ran without issues
