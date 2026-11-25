# Zig JSONL log search (2026-11-23)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A Zig project (~321 files, 7.4 MB).

**Task:** Indexing 20 jsonl log files and searching for error clusters semantically.

**Result:** Autoindex in 8.2 s. Hybrid search on 1-2 word queries felt identical to BM25 alone — vector didn't seem to add much there.

**Numbers:** `xerj autoindex ./` → wall=8.2s, 321 files, 11157 records. Top score 2021.

**Filed alongside:** clean run
