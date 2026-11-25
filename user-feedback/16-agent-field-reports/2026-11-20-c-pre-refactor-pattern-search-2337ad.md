# C# pre-refactor pattern search (2026-11-20)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A C# project (~296 files, 7.2 MB).

**Task:** Searching for existing pagination implementations before changing the shared interface.

**Result:** Autoindex in 14.2 s. Would help to have a `--dry-run` flag to preview what autoindex will touch.

**Numbers:** `xerj autoindex ./` → wall=14.2s, 296 files, 12211 records. Top score 2432.

**Filed alongside:** nothing broke
