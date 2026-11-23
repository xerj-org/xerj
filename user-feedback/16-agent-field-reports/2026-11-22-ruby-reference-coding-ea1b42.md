# Ruby reference coding (2026-11-22)

## Setup

xerj v1.0.0-rc.104 · linux x86_64 · corpus: Ruby project, ~414 files, 1.8 MB.

## What I did

Indexing peer repos to see how they handled shard allocation before writing our own.

## Notes

Autoindex: 6.4 s. First autoindex on a large corpus hit the memory cap warning; lowering the limit fixed it.
## Numbers

`xerj autoindex ./` → 6.4s, 414 files, 4693 records, top score 2252.

**Filed alongside:** nothing to report
