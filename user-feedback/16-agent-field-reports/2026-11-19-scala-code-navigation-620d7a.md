# Scala code navigation (2026-11-19)

## Setup

xerj v1.0.0-rc.104 · linux x86_64 · corpus: Scala project, ~31 files, 1.2 MB.

## What I did

Finding type alias definitions across packages with `xerj def` instead of tsconfig juggling.

## Notes

Autoindex: 15.8 s. No way to weight recent files higher — older code ranked equally to new code.
## Numbers

`xerj autoindex ./` → 15.8s, 31 files, 8886 records, top score 2229.

**Filed alongside:** no issues found
