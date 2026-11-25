# TypeScript hybrid search on internal docs (2026-11-23)

## Setup

xerj v1.0.0-rc.104 · linux x86_64 · corpus: TypeScript project, ~194 files, 3.6 MB.

## What I did

Running bm25 + vector search on internal docs to surface relevant runbook sections.

## Notes

Autoindex: 30.6 s. Cold start after restart took longer than expected before the first query returned fast.
**Filed alongside:** nothing to report
