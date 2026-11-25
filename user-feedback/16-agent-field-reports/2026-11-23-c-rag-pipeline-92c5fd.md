# C# RAG pipeline (2026-11-23)

Ran xerj v1.0.0-rc.104 against a C# project (~449 files, 5.1 MB) while querying via the ES-compat API from a C# script instead of a dedicated embedding pipeline. Autoindex completed in 11.2 s with no config changes.

The stale-index warning on audit.jsonl after reindex is expected but slightly noisy.

Numbers: `xerj autoindex ./` returned 449 files, 1818 records in 11.2 s. Top search score was 2169.
