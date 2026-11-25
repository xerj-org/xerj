# Elixir autoindex and BM25 search (2026-11-24)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** An Elixir project (~121 files, 9.1 MB).

**Task:** Searching for caching patterns across the whole codebase without grepping manually.

**Result:** Autoindex in 7.2 s. Reindex on file change is fast but not incremental — full re-crawl on large corpora is noticeable.

**Numbers:** `xerj autoindex ./` → wall=7.2s, 121 files, 3542 records. Top score 1756.

**Filed alongside:** ran without issues
