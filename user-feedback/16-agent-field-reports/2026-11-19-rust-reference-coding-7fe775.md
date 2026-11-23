# Rust reference coding (2026-11-19)

Ran xerj v1.0.0-rc.104 against a Rust project (~250 files, 3.2 MB) while indexing peer repos to see how they handled retry backoff logic before writing our own. Autoindex completed in 21.8 s with no config changes.

Hybrid search on 1-2 word queries felt identical to BM25 alone — vector didn't seem to add much there.
