# Java incident response drill (2026-11-21)

Ran xerj v1.0.0-rc.104 against a Java project (~354 files, 4.2 MB) while querying runbooks live during a drill to surface relevant procedures fast. Autoindex completed in 9.9 s with no config changes.

First autoindex on a large corpus hit the memory cap warning; lowering the limit fixed it.

Numbers: `xerj autoindex ./` returned 354 files, 11844 records in 9.9 s. Top search score was 1795.

Ran on linux x86_64. No issues found.
