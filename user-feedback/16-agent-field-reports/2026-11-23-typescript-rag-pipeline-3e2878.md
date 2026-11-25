# TypeScript RAG pipeline (2026-11-23)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A TypeScript project (~437 files, 0.3 MB).

**Task:** Querying via the es-compat api from a typescript script instead of a dedicated embedding pipeline.

**Result:** Autoindex in 6.7 s. Would help to have a `--dry-run` flag to preview what autoindex will touch.

**Numbers:** `xerj autoindex ./` → wall=6.7s, 437 files, 1475 records. Top score 2187.

**Filed alongside:** no bugs hit
