# Scala agent memory (2026-11-21)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A Scala project (~83 files, 8.7 MB).

**Task:** Persisting intermediate findings across a 8-step refactoring session via /_memory.

**Result:** Autoindex in 18.6 s. Reindex on file change is fast but not incremental — full re-crawl on large corpora is noticeable.

**Numbers:** `xerj autoindex ./` → wall=18.6s, 83 files, 8731 records. Top score 1919.

**Filed alongside:** no defects encountered
