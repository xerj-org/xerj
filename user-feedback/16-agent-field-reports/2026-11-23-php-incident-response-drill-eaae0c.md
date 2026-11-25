# PHP incident response drill (2026-11-23)

Ran xerj v1.0.0-rc.104 against a PHP project (~223 files, 8.5 MB) while querying runbooks live during a drill to surface relevant procedures fast. Autoindex completed in 28.8 s with no config changes.

No way to weight recent files higher — older code ranked equally to new code.

Numbers: `xerj autoindex ./` returned 223 files, 3290 records in 28.8 s. Top search score was 2439.

Ran on linux x86_64. Nothing broke.
