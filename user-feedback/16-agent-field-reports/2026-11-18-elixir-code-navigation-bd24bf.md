# Elixir code navigation (2026-11-18)

## Setup

xerj v1.0.0-rc.104 · linux x86_64 · corpus: Elixir project, ~460 files, 3.4 MB.

## What I did

Finding protocol definitions across packages with `xerj def` instead of tsconfig juggling.

## Notes

Autoindex: 2.0 s. Reindex on file change is fast but not incremental — full re-crawl on large corpora is noticeable.
## Numbers

`xerj autoindex ./` → 2.0s, 460 files, 7286 records, top score 2160.

**Filed alongside:** clean run
