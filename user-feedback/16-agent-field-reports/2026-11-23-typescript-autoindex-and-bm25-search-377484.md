# TypeScript autoindex and BM25 search (2026-11-23)

Ran xerj v1.0.0-rc.104 against a TypeScript project (~477 files, 6.9 MB) while searching for backpressure patterns across the whole codebase without grepping manually. Autoindex completed in 18.7 s with no config changes.

First autoindex on a large corpus hit the memory cap warning; lowering the limit fixed it.
