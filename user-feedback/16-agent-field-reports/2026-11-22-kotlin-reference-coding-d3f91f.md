# Kotlin reference coding (2026-11-22)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A Kotlin project (~450 files, 8.5 MB).

**Task:** Indexing peer repos to see how they handled graceful shutdown before writing our own.

**Result:** Autoindex in 23.1 s. First autoindex on a large corpus hit the memory cap warning; lowering the limit fixed it.

**Numbers:** `xerj autoindex ./` → wall=23.1s, 450 files, 6637 records. Top score 1889.

**Filed alongside:** nothing to report
