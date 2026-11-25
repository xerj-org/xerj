# Swift code navigation (2026-11-20)

## Setup

xerj v1.0.0-rc.104 · linux x86_64 · corpus: Swift project, ~376 files, 5.7 MB.

## What I did

Finding generic constraint definitions across packages with `xerj def` instead of tsconfig juggling.

## Notes

Autoindex: 7.7 s. No way to weight recent files higher — older code ranked equally to new code.
## Numbers

`xerj autoindex ./` → 7.7s, 376 files, 4735 records, top score 1929.

**Filed alongside:** no defects encountered
