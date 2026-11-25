# Rust agent memory (2026-11-21)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A Rust project (~136 files, 7.4 MB).

**Task:** Persisting intermediate findings across a 13-step refactoring session via /_memory.

**Result:** Autoindex in 14.1 s. No built-in way to exclude a specific subdirectory pattern (e.g. `**/testdata/`).

**Numbers:** `xerj autoindex ./` → wall=14.1s, 136 files, 12453 records. Top score 2337.

**Filed alongside:** no bugs hit
