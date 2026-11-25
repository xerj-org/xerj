# Rust pre-refactor pattern search (2026-11-22)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A Rust project (~182 files, 0.5 MB).

**Task:** Searching for existing backpressure implementations before changing the shared interface.

**Result:** Autoindex in 14.7 s. Hybrid search on 1-2 word queries felt identical to BM25 alone — vector didn't seem to add much there.

**Filed alongside:** nothing to report
