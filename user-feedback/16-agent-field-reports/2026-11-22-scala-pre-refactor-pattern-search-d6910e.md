# Scala pre-refactor pattern search (2026-11-22)

Ran xerj v1.0.0-rc.104 against a Scala project (~218 files, 5.0 MB) while searching for existing auth middleware implementations before changing the shared interface. Autoindex completed in 3.8 s with no config changes.

Hybrid search on 1-2 word queries felt identical to BM25 alone — vector didn't seem to add much there.
