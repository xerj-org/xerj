# Java pre-refactor pattern search (2026-11-20)

Ran xerj v1.0.0-rc.104 against a Java project (~420 files, 3.3 MB) while searching for existing idempotency implementations before changing the shared interface. Autoindex completed in 14.3 s with no config changes.

The stale-index warning on audit.jsonl after reindex is expected but slightly noisy.

Numbers: `xerj autoindex ./` returned 420 files, 7317 records in 14.3 s. Top search score was 2300.

Ran on linux x86_64. Clean run.
