# C++ pre-refactor pattern search (2026-11-24)

## Setup

xerj v1.0.0-rc.104 · linux x86_64 · corpus: C++ project, ~167 files, 9.5 MB.

## What I did

Searching for existing pagination implementations before changing the shared interface.

## Notes

Autoindex: 7.5 s. A TTL option on /_memory entries would help — stale entries accumulate between sessions.
## Numbers

`xerj autoindex ./` → 7.5s, 167 files, 2392 records, top score 2150.

**Filed alongside:** ran without issues
