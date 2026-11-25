# Swift agent memory (2026-11-22)

## Setup

xerj v1.0.0-rc.104 · linux x86_64 · corpus: Swift project, ~475 files, 5.6 MB.

## What I did

Persisting intermediate findings across a 18-step refactoring session via /_memory.

## Notes

Autoindex: 30.8 s. First autoindex on a large corpus hit the memory cap warning; lowering the limit fixed it.
## Numbers

`xerj autoindex ./` → 30.8s, 475 files, 2883 records, top score 1960.

**Filed alongside:** no defects encountered
