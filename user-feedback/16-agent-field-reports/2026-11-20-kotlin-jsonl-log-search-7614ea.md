# Kotlin JSONL log search (2026-11-20)

## Setup

xerj v1.0.0-rc.104 · linux x86_64 · corpus: Kotlin project, ~97 files, 0.9 MB.

## What I did

Indexing 6 jsonl log files and searching for error clusters semantically.

## Notes

Autoindex: 27.8 s. Reindex on file change is fast but not incremental — full re-crawl on large corpora is noticeable.
## Numbers

`xerj autoindex ./` → 27.8s, 97 files, 7748 records, top score 2092.

**Filed alongside:** no bugs hit
