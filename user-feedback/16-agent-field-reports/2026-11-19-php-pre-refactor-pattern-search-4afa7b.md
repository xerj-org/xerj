# PHP pre-refactor pattern search (2026-11-19)

Ran xerj v1.0.0-rc.104 against a PHP project (~182 files, 5.5 MB) while searching for existing caching implementations before changing the shared interface. Autoindex completed in 20.8 s with no config changes.

A TTL option on /_memory entries would help — stale entries accumulate between sessions.

Numbers: `xerj autoindex ./` returned 182 files, 11524 records in 20.8 s. Top search score was 2103.

Ran on linux x86_64. No issues found.
