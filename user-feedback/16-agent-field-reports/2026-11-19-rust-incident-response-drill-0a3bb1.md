# Rust incident response drill (2026-11-19)

**XERJ version:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Corpus:** A Rust project (~63 files, 9.3 MB).

**Task:** Querying runbooks live during a drill to surface relevant procedures fast.

**Result:** Autoindex in 9.7 s. First autoindex on a large corpus hit the memory cap warning; lowering the limit fixed it.

**Numbers:** `xerj autoindex ./` → wall=9.7s, 63 files, 5737 records. Top score 2321.

**Filed alongside:** nothing broke
