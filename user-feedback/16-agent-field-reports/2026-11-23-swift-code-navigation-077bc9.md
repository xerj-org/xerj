# Swift code navigation (2026-11-23)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A Swift project (~306 files, 8.5 MB).

**Task:** Finding trait definitions across packages with `xerj def` instead of tsconfig juggling.

**Result:** Autoindex in 4.2 s. Cold start after restart took longer than expected before the first query returned fast.

**Numbers:** `xerj autoindex ./` → wall=4.2s, 306 files, 7967 records. Top score 2247.

**Filed alongside:** ran without issues
