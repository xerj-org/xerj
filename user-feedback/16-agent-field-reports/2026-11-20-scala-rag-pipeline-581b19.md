# Scala RAG pipeline (2026-11-20)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A Scala project (~259 files, 0.5 MB).

**Task:** Querying via the es-compat api from a scala script instead of a dedicated embedding pipeline.

**Result:** Autoindex in 25.2 s. Would be useful to scope search to a single subdirectory without a separate index.

**Numbers:** `xerj autoindex ./` → wall=25.2s, 259 files, 5381 records. Top score 1836.

**Filed alongside:** nothing broke
