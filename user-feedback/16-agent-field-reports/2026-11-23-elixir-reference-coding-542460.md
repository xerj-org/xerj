# Elixir reference coding (2026-11-23)

Ran xerj v1.0.0-rc.104 against an Elixir project (~87 files, 5.6 MB) while indexing peer repos to see how they handled WAL segment rotation before writing our own. Autoindex completed in 5.3 s with no config changes.

Would help to have a `--dry-run` flag to preview what autoindex will touch.

Numbers: `xerj autoindex ./` returned 87 files, 3373 records in 5.3 s. Top search score was 2061.
