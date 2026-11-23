# Scala code navigation (2026-11-24)

Ran xerj v1.0.0-rc.104 against a Scala project (~29 files, 8.1 MB) while finding abstract method definitions across packages with `xerj def` instead of tsconfig juggling. Autoindex completed in 9.6 s with no config changes.

Would help to have a `--dry-run` flag to preview what autoindex will touch.

Numbers: `xerj autoindex ./` returned 29 files, 639 records in 9.6 s. Top search score was 2345.

Ran on linux x86_64. No bugs hit.
