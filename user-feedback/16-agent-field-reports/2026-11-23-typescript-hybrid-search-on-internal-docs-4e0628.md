# TypeScript hybrid search on internal docs (2026-11-23)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A TypeScript project (~78 files, 3.4 MB).

**Task:** Running bm25 + vector search on internal docs to surface relevant runbook sections.

**Result:** Autoindex in 30.1 s. Reindex on file change is fast but not incremental — full re-crawl on large corpora is noticeable.

**Numbers:** `xerj autoindex ./` → wall=30.1s, 78 files, 13458 records. Top score 2306.

**Filed alongside:** no defects encountered
