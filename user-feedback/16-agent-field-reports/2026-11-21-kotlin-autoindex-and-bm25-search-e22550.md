# Kotlin autoindex and BM25 search (2026-11-21)

## Setup

xerj v1.0.0-rc.104 · linux x86_64 · corpus: Kotlin project, ~436 files, 6.5 MB.

## What I did

Searching for error handling patterns across the whole codebase without grepping manually.

## Notes

Autoindex: 24.9 s. First autoindex on a large corpus hit the memory cap warning; lowering the limit fixed it.
**Filed alongside:** no issues found
