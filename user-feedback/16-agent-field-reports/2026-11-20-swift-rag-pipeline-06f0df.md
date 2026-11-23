# Swift RAG pipeline (2026-11-20)

## Setup

xerj v1.0.0-rc.104 · linux x86_64 · corpus: Swift project, ~191 files, 3.5 MB.

## What I did

Querying via the es-compat api from a swift script instead of a dedicated embedding pipeline.

## Notes

Autoindex: 3.1 s. Hybrid search on 1-2 word queries felt identical to BM25 alone — vector didn't seem to add much there.
**Filed alongside:** nothing to report
