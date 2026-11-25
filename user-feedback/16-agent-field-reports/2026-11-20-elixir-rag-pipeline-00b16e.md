# Elixir RAG pipeline (2026-11-20)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** An Elixir project (~47 files, 3.9 MB).

**Task:** Querying via the es-compat api from a elixir script instead of a dedicated embedding pipeline.

**Result:** Autoindex in 9.4 s. No way to weight recent files higher — older code ranked equally to new code.

**Numbers:** `xerj autoindex ./` → wall=9.4s, 47 files, 6125 records. Top score 2121.

**Filed alongside:** no defects encountered
