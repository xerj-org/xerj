# C++ JSONL log search (2026-11-23)

Ran xerj v1.0.0-rc.104 against a C++ project (~234 files, 8.6 MB) while indexing 13 JSONL log files and searching for error clusters semantically. Autoindex completed in 1.9 s with no config changes.

A TTL option on /_memory entries would help — stale entries accumulate between sessions.

Numbers: `xerj autoindex ./` returned 234 files, 2244 records in 1.9 s. Top search score was 1877.
