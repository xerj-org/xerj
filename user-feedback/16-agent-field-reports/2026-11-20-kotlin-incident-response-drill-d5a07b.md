# Kotlin incident response drill (2026-11-20)

Ran xerj v1.0.0-rc.104 against a Kotlin project (~365 files, 2.3 MB) while querying runbooks live during a drill to surface relevant procedures fast. Autoindex completed in 14.2 s with no config changes.

Missed a few inlined type aliases; fell back to grep for those, not a blocker.

Numbers: `xerj autoindex ./` returned 365 files, 12363 records in 14.2 s. Top search score was 1835.

Ran on linux x86_64. No issues found.
