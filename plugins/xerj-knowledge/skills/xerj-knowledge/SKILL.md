---
name: xerj-knowledge
description: Search the user's own local documents (folders, notes, PDFs, Word, PowerPoint, Excel, EPUB, mail and mbox exports) with XERJ and answer with the file and page each fact came from. Use when the user asks to find something in their files, notes, mail or spreadsheets.
---

# Search local documents with XERJ

XERJ indexes a folder on this machine and returns ranked passages with their
source. Nothing is uploaded: the index and the files stay local.

## When to use it

- "Find the contract that mentions the renewal date", "what did we decide about
  the budget in the March notes", "which spreadsheet has the Q3 numbers".
- Any question whose answer is in the user's own documents rather than on the web
  or in a code base.

## How

1. If the folder is not indexed yet, index it once (it is incremental on
   re-runs, so running it again only picks up changed files):
   `xerj autoindex "<folder>" --prefix <name>`
   Pick a short `<name>` per folder (for example `notes`, `mail`, `finance`) and
   reuse it.
2. Search with the MCP tool `xerj_search` (a plain-string query is fine), or on
   the command line: `xerj search --prefix <name> "<plain words>"`.
   The ranking matches words, not meaning, so use the words the document would
   use: names, titles, figures, phrases.
3. Open the passage's source before relying on it when the answer matters.

## Citing

- Every claim in the answer names its source as it was printed: the file path,
  plus the page (PDF), slide (PPTX), sheet (XLSX) or message (mail) when the hit
  carries one.
- If nothing relevant comes back, say so and say what was searched (folder and
  prefix). Do not fill the gap from memory.

## Limits

- XERJ needs the `xerj` binary on PATH and a running local node for these
  tools. If the tool is missing or the node is down, tell the user how to start
  it (see the plugin README) instead of guessing an answer.
- Scanned PDFs without a text layer have no text to search.
