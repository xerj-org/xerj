# Kotlin autoindex and BM25 search (2026-11-19)

## Setup

xerj v1.0.0-rc.104 · linux x86_64 · corpus: Kotlin project, ~303 files, 2.1 MB.

## What I did

Searching for retry patterns across the whole codebase without grepping manually.

## Notes

Autoindex: 4.7 s. Reindex on file change is fast but not incremental — full re-crawl on large corpora is noticeable.
**Filed alongside:** ran without issues
