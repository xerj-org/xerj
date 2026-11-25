# C++ incident response drill (2026-11-21)

## Setup

xerj v1.0.0-rc.104 · linux x86_64 · corpus: C++ project, ~127 files, 4.6 MB.

## What I did

Querying runbooks live during a drill to surface relevant procedures fast.

## Notes

Autoindex: 25.6 s. Hybrid search on 1-2 word queries felt identical to BM25 alone — vector didn't seem to add much there.
## Numbers

`xerj autoindex ./` → 25.6s, 127 files, 5065 records, top score 2435.

**Filed alongside:** nothing broke
