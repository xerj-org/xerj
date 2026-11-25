# Zig hybrid search on internal docs (2026-11-20)

## Setup

xerj v1.0.0-rc.104 · linux x86_64 · corpus: Zig project, ~106 files, 3.1 MB.

## What I did

Running bm25 + vector search on internal docs to surface relevant runbook sections.

## Notes

Autoindex: 20.1 s. Would be useful to scope search to a single subdirectory without a separate index.
**Filed alongside:** nothing broke
