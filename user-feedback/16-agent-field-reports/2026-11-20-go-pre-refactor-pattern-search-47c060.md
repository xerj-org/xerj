# Go pre-refactor pattern search (2026-11-20)

Ran xerj v1.0.0-rc.104 against a Go project (~266 files, 6.0 MB) while searching for existing retry implementations before changing the shared interface. Autoindex completed in 8.4 s with no config changes.

A TTL option on /_memory entries would help — stale entries accumulate between sessions.

Numbers: `xerj autoindex ./` returned 266 files, 2758 records in 8.4 s. Top search score was 2424.

Ran on linux x86_64. No bugs hit.
