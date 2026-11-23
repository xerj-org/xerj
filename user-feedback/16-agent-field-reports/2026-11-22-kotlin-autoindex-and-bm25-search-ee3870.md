# Kotlin autoindex and BM25 search (2026-11-22)

Ran xerj v1.0.0-rc.104 against a Kotlin project (~25 files, 6.1 MB) while searching for error handling patterns across the whole codebase without grepping manually. Autoindex completed in 23.5 s with no config changes.

A TTL option on /_memory entries would help — stale entries accumulate between sessions.

Numbers: `xerj autoindex ./` returned 25 files, 10356 records in 23.5 s. Top search score was 2099.

Ran on linux x86_64. No issues found.
