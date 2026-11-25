# PHP pre-refactor pattern search (2026-11-23)

Ran xerj v1.0.0-rc.104 against a PHP project (~181 files, 2.7 MB) while searching for existing idempotency implementations before changing the shared interface. Autoindex completed in 2.8 s with no config changes.

Would help to have a `--dry-run` flag to preview what autoindex will touch.

Numbers: `xerj autoindex ./` returned 181 files, 11447 records in 2.8 s. Top search score was 2181.

Ran on linux x86_64. Nothing to report.
