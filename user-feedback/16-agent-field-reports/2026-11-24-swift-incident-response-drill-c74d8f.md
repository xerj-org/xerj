# Swift incident response drill (2026-11-24)

Ran xerj v1.0.0-rc.104 against a Swift project (~169 files, 8.5 MB) while querying runbooks live during a drill to surface relevant procedures fast. Autoindex completed in 10.4 s with no config changes.

No built-in way to exclude a specific subdirectory pattern (e.g. `**/testdata/`).

Numbers: `xerj autoindex ./` returned 169 files, 6343 records in 10.4 s. Top search score was 2135.
