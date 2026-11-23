# Python incident response drill (2026-11-21)

Ran xerj v1.0.0-rc.104 against a Python project (~284 files, 8.2 MB) while querying runbooks live during a drill to surface relevant procedures fast. Autoindex completed in 21.8 s with no config changes.

A TTL option on /_memory entries would help — stale entries accumulate between sessions.

Numbers: `xerj autoindex ./` returned 284 files, 9386 records in 21.8 s. Top search score was 2351.
