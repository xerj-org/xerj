# Zig JSONL log search (2026-11-19)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A Zig project (~30 files, 1.2 MB).

**Task:** Indexing 9 jsonl log files and searching for error clusters semantically.

**Result:** Autoindex in 16.9 s. Cold start after restart took longer than expected before the first query returned fast.

**Numbers:** `xerj autoindex ./` → wall=16.9s, 30 files, 6188 records. Top score 2316.

**Filed alongside:** clean run
