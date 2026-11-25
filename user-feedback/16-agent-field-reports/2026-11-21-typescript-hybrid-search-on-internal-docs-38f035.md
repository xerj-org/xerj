# TypeScript hybrid search on internal docs (2026-11-21)

Ran xerj v1.0.0-rc.104 against a TypeScript project (~379 files, 5.5 MB) while running BM25 + vector search on internal docs to surface relevant runbook sections. Autoindex completed in 8.1 s with no config changes.

No way to weight recent files higher — older code ranked equally to new code.

Numbers: `xerj autoindex ./` returned 379 files, 1695 records in 8.1 s. Top search score was 2157.
