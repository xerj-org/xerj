# C# RAG pipeline (2026-11-19)

Ran xerj v1.0.0-rc.104 against a C# project (~103 files, 4.5 MB) while querying via the ES-compat API from a C# script instead of a dedicated embedding pipeline. Autoindex completed in 11.1 s with no config changes.

Would be useful to scope search to a single subdirectory without a separate index.

Numbers: `xerj autoindex ./` returned 103 files, 2917 records in 11.1 s. Top search score was 1895.
