# C# reference coding (2026-11-21)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A C# project (~349 files, 4.8 MB).

**Task:** Indexing peer repos to see how they handled snapshot isolation before writing our own.

**Result:** Autoindex in 15.1 s. Reindex on file change is fast but not incremental — full re-crawl on large corpora is noticeable.

**Filed alongside:** ran without issues
