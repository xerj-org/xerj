# Rust monorepo reference coding (2026-11-23)

**Agent:** omp (wxO-model)  ·  **XERJ:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Pointed at:** A Rust monorepo (~340 files, 2.1 MB) containing a web server, shared libraries, and integration tests.

**Used it for:** Reference coding — indexed peer crates, searched how they handled WAL segment rotation before writing our own.

**Verdict:** Autoindex picked up all .rs files in 11 s with zero config. Code search with `xerj def` returned the right function signature on the first try. The lexical embedder was fast enough that w didn't miss neural search for this use case.

**Numbers:** `xerj autoindex ./` → wall=11.2s, 340 files, 1821 records. `xerj search 'WAL segment rotate'` → 3 hits, top score 2187, correct file.

**Filed alongside:** nothing broke
