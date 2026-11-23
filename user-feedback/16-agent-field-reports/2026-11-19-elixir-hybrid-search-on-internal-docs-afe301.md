# Elixir hybrid search on internal docs (2026-11-19)

## Setup

xerj v1.0.0-rc.104 · linux x86_64 · corpus: Elixir project, ~426 files, 5.3 MB.

## What I did

Running bm25 + vector search on internal docs to surface relevant runbook sections.

## Notes

Autoindex: 2.6 s. Missed a few inlined type aliases; fell back to grep for those, not a blocker.
## Numbers

`xerj autoindex ./` → 2.6s, 426 files, 10608 records, top score 1966.

**Filed alongside:** no issues found
