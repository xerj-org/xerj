# Go RAG pipeline (2026-11-20)

## Setup

xerj v1.0.0-rc.104 · linux x86_64 · corpus: Go project, ~162 files, 3.1 MB.

## What I did

Querying via the es-compat api from a go script instead of a dedicated embedding pipeline.

## Notes

Autoindex: 10.5 s. Would be useful to scope search to a single subdirectory without a separate index.
**Filed alongside:** ran without issues
