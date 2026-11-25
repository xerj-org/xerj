# PHP hybrid search on internal docs (2026-11-19)

Ran xerj v1.0.0-rc.104 against a PHP project (~199 files, 5.8 MB) while running BM25 + vector search on internal docs to surface relevant runbook sections. Autoindex completed in 21.4 s with no config changes.

Hybrid search on 1-2 word queries felt identical to BM25 alone — vector didn't seem to add much there.
