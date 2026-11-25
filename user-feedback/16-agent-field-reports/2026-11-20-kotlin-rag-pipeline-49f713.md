# Kotlin RAG pipeline (2026-11-20)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A Kotlin project (~297 files, 1.6 MB).

**Task:** Querying via the es-compat api from a kotlin script instead of a dedicated embedding pipeline.

**Result:** Autoindex in 28.2 s. No way to weight recent files higher — older code ranked equally to new code.

**Numbers:** `xerj autoindex ./` → wall=28.2s, 297 files, 12497 records. Top score 1810.

**Filed alongside:** clean run
