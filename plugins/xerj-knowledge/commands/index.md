---
description: Index a local folder of documents (PDF, Word, PowerPoint, Excel, EPUB, notes, mail) so it can be searched with citations
argument-hint: <folder> [prefix]
---

Index the folder `$1` with XERJ so its documents can be searched.

1. Choose the prefix: use `$2` if it was given, otherwise a short lowercase name
   derived from the folder name (letters, digits and dashes only).
2. Run `xerj autoindex "$1" --prefix <prefix>` and wait for it to finish. Re-runs
   are incremental, so this is also how to pick up changed files later.
   If it stops with exit code 4 and prints a JSON list of plans, the full
   folder would not fit the node's disk. Show the user the plans and ask which
   one to use before re-running with `--yes` (the recommended plan) or
   `--plan <name>`. Never pass `--drop-indexed` without asking: it confirms
   deleting already-indexed data.
3. Report how many files were indexed and any files that were skipped and why,
   as printed by the command. Tell the user the prefix to search with:
   `/xerj-knowledge:find <question>`.

If `xerj` is not installed or no node is running, do not try to work around it:
show the install and start steps from the plugin README.
