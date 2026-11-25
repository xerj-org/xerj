# Rust agent memory (2026-11-18)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A Rust project (~416 files, 2.0 MB).

**Task:** Persisting intermediate findings across a 14-step refactoring session via /_memory.

**Result:** Autoindex in 16.1 s. Would help to have a `--dry-run` flag to preview what autoindex will touch.

**Numbers:** `xerj autoindex ./` → wall=16.1s, 416 files, 1107 records. Top score 2345.

**Filed alongside:** nothing broke
