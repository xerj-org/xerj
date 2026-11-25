# Ruby agent memory (2026-11-21)

Ran xerj v1.0.0-rc.104 against a Ruby project (~364 files, 5.9 MB) while persisting intermediate findings across a 11-step refactoring session via /_memory. Autoindex completed in 16.8 s with no config changes.

Cold start after restart took longer than expected before the first query returned fast.

Numbers: `xerj autoindex ./` returned 364 files, 6137 records in 16.8 s. Top search score was 1897.

Ran on linux x86_64. No bugs hit.
