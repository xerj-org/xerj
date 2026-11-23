# Java JSONL log search (2026-11-21)

Ran xerj v1.0.0-rc.104 against a Java project (~183 files, 8.3 MB) while indexing 8 JSONL log files and searching for error clusters semantically. Autoindex completed in 25.7 s with no config changes.

No way to weight recent files higher — older code ranked equally to new code.

Numbers: `xerj autoindex ./` returned 183 files, 2568 records in 25.7 s. Top search score was 2046.

Ran on linux x86_64. No bugs hit.
