# Go RAG pipeline (2026-11-19)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A Go project (~283 files, 2.5 MB).

**Task:** Querying via the es-compat api from a go script instead of a dedicated embedding pipeline.

**Result:** Autoindex in 19.1 s. No way to weight recent files higher — older code ranked equally to new code.

**Numbers:** `xerj autoindex ./` → wall=19.1s, 283 files, 8824 records. Top score 1997.

**Filed alongside:** no issues found
