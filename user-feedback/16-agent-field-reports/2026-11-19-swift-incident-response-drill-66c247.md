# Swift incident response drill (2026-11-19)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A Swift project (~310 files, 3.6 MB).

**Task:** Querying runbooks live during a drill to surface relevant procedures fast.

**Result:** Autoindex in 17.7 s. Reindex on file change is fast but not incremental — full re-crawl on large corpora is noticeable.

**Filed alongside:** ran without issues
