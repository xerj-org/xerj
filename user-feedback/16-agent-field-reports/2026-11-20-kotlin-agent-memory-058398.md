# Kotlin agent memory (2026-11-20)

Ran xerj v1.0.0-rc.104 against a Kotlin project (~297 files, 6.2 MB) while persisting intermediate findings across a 18-step refactoring session via /_memory. Autoindex completed in 29.0 s with no config changes.

A TTL option on /_memory entries would help — stale entries accumulate between sessions.

Numbers: `xerj autoindex ./` returned 297 files, 9327 records in 29.0 s. Top search score was 2137.
