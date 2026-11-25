# C# hybrid search on internal docs (2026-11-21)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A C# project (~44 files, 3.9 MB).

**Task:** Running bm25 + vector search on internal docs to surface relevant runbook sections.

**Result:** Autoindex in 18.6 s. Reindex on file change is fast but not incremental — full re-crawl on large corpora is noticeable.

**Numbers:** `xerj autoindex ./` → wall=18.6s, 44 files, 3603 records. Top score 1852.

**Filed alongside:** no issues found
