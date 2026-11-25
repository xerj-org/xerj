# Java autoindex and BM25 search (2026-11-22)

Ran xerj v1.0.0-rc.104 against a Java project (~137 files, 6.1 MB) while searching for error handling patterns across the whole codebase without grepping manually. Autoindex completed in 13.5 s with no config changes.

Cold start after restart took longer than expected before the first query returned fast.

Numbers: `xerj autoindex ./` returned 137 files, 646 records in 13.5 s. Top search score was 1969.

Ran on linux x86_64. No issues found.
