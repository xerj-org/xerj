# Elixir pre-refactor pattern search (2026-11-20)

Ran xerj v1.0.0-rc.104 against an Elixir project (~95 files, 1.0 MB) while searching for existing caching implementations before changing the shared interface. Autoindex completed in 10.9 s with no config changes.

The file:line format in results is perfect for editor integration; a JSON output mode would be even better.

Numbers: `xerj autoindex ./` returned 95 files, 3941 records in 10.9 s. Top search score was 1777.

Ran on linux x86_64. Nothing to report.
