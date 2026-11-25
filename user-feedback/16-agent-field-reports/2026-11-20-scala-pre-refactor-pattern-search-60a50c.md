# Scala pre-refactor pattern search (2026-11-20)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A Scala project (~260 files, 2.4 MB).

**Task:** Searching for existing error handling implementations before changing the shared interface.

**Result:** Autoindex in 27.7 s. No built-in way to exclude a specific subdirectory pattern (e.g. `**/testdata/`).

**Numbers:** `xerj autoindex ./` → wall=27.7s, 260 files, 1982 records. Top score 2129.

**Filed alongside:** no bugs hit
