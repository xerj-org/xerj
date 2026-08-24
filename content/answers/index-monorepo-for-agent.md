---
title: "How do I stop my agent reading the whole repo?"
h1: "How do I stop my agent from reading the whole repo into context?"
description: "Index the tree once with xerj autoindex, then let the agent query the defs and symbols fields instead of opening files. A dry run first prints every ignore rule."
slug: "index-monorepo-for-agent"
cluster: "Files and formats"
question: "How do I stop my agent from reading the whole repo into context?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a XERJ node with --insecure, run xerj autoindex ./code --prefix code --dry-run and record files, ignored_files and ignored_dirs from the xerj-done line, run it again without --dry-run, then aggregate language and symbol_count over code-*/_search and answer with a defs query instead of opening any source file."
commands:
  - cmd: "xerj autoindex ./code --url http://127.0.0.1:9410 --prefix code --state-dir ./state-code --progress plain --disable-feedback --dry-run"
    note: "Print the plan and the ignore accounting without writing anything."
  - cmd: "xerj autoindex ./code --url http://127.0.0.1:9410 --prefix code --state-dir ./state-code --progress plain --disable-feedback"
    note: "Index the repository after the plan looks right."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9410/code-*/_search' -H 'content-type: application/json' -d '{\"size\":0,\"aggs\":{\"by_language\":{\"terms\":{\"field\":\"language\",\"size\":40}},\"total_symbols\":{\"sum\":{\"field\":\"symbol_count\"}}}}'"
    note: "Count the languages and the symbols in one request."
links_out:
  - "syntax-aware-code-search-refactoring"
  - "code-search-mcp-for-claude-code"
  - "xerj-vs-ripgrep-for-code-agents"
faq:
  - q: "How do I stop my agent from reading the whole repo into context?"
    a: "Index the tree once, then have the agent send a query instead of opening files. A `defs` hit names the declaration site with its path, so the agent reads one passage."
  - q: "My agent keeps opening every file in the repo. How do I let it search the folder instead?"
    a: "Run `xerj autoindex` on the tree and register `xerj mcp` with the agent host. The agent then calls a search tool and gets hits with paths, so the file-by-file walk stops."
  - q: "How do I give a coding agent the whole codebase as searchable context?"
    a: "Index the tree with `xerj autoindex`, which extracts definitions through tree-sitter for 34 languages, and expose the index over MCP. The agent queries it rather than loading the repository."
  - q: "How do I index a monorepo so my coding agent stops reading files one by one?"
    a: "Point `xerj autoindex` at the monorepo root. Built-in rules prune `node_modules/`, `vendor/`, `target/`, `dist/`, `build/`, `.venv/` and `__pycache__/`, and `--dry-run` prints the plan before anything is written."
  - q: "Which ignore file wins?"
    a: "`.xerjignore` beats `.gitignore`, which beats `.git/info/exclude`, which beats the built-in list. The global `core.excludesFile` is deliberately not read."
  - q: "Does XERJ index my .env file?"
    a: "No. Hidden dotfiles and dotted directories are always skipped, and `--no-ignore` does not turn that rule off."
  - q: "Can XERJ tell my agent who calls a function?"
    a: "No. Extraction is definitions only, with no call graph and no cross-file reference resolution. A `defs` hit names the declaration site."
---

**TL;DR** — Run `xerj autoindex --dry-run` on the repository first. In our capture the dry run reported `files=3`, `ignored_files=2` and `ignored_dirs=3`, with 60 non-hidden files inside the pruned directories. The indexed tree then gave an agent 3 languages and 11 symbols to query.

## Ask for the plan before you index

`xerj autoindex --dry-run` prints the job size and the ignore accounting and writes nothing. Run it first on any repository, because the plan tells you how much of the tree XERJ will keep.

```sh
xerj autoindex ./code --url http://127.0.0.1:9410 --prefix code --state-dir ./state-code --progress plain --disable-feedback --dry-run
```

The captured terminal line for a 3-source-file repository was exact about what it dropped.

```text
xerj-done ok=true exit=0 reason=dry-run wall=0.1s files=3 ignored_files=2 ignored_dirs=3 ignored_files_in_pruned_dirs=60 ignored_files_in_pruned_dirs_exact=true
```

## What XERJ pruned, rule by rule

XERJ names every ignore rule that fired and counts the files behind it. The capture pruned 3 directories holding 60 non-hidden files and read 1 ignore file.

```text
autoindex: ignore rules: skipped 2 files and pruned 3 directories (60 non-hidden files inside them); 1 ignore file read
autoindex: ignore rules:   <built-in>:target/ — 1 directory pruned (30 non-hidden files inside)
autoindex: ignore rules:   <built-in>:node_modules/ — 1 directory pruned (25 non-hidden files inside)
autoindex: ignore rules:   .gitignore:coverage/ — 1 directory pruned (5 non-hidden files inside)
```

The precedence is fixed: `.xerjignore`, then `.gitignore`, then `.git/info/exclude`, then the built-in list. A hidden dotfile such as `.env` is always skipped, and `--no-ignore` does not turn that off.

## Languages and symbols in the index

XERJ split the indexed tree into 3 languages and summed 11 symbols across it. One terms aggregation on `language` and one sum aggregation on `symbol_count` answer both questions in a single request.

```sh
curl -s -XPOST 'http://127.0.0.1:9410/code-*/_search' -H 'content-type: application/json' -d '{"size":0,"aggs":{"by_language":{"terms":{"field":"language","size":40}},"total_symbols":{"sum":{"field":"symbol_count"}}}}'
```

The `by_language` aggregation returned one bucket per language: `go`, `python` and `rust`. Do not carry a captured bucket size over to your own tree. Every per-declaration document carries the `language` of the file it came from, so a bucket counts declarations as well as files and moves with the code, not with the file count.

XERJ recognizes 34 languages through tree-sitter. This capture exercised 3 of them, so read the answer as the shape of the aggregation and not as a language-coverage test.

## One definition hit an agent can use

A `match` query on the `defs` field returns the file, the language and a `symbols` array. Each entry in that array carries a kind, a name and a line number. An agent reads the hit rather than the source file.

```sh
curl -s -XPOST 'http://127.0.0.1:9410/code-*/_search' -H 'content-type: application/json' -d '{"query":{"match":{"defs":"merge_segments"}},"size":10,"_source":["ax_path","language","title","defs","symbols"],"track_total_hits":true}'
```

```json
{"_index":"code-docs","_score":0.45315087,"_source":{"ax_path":"src/lib.rs","language":"rust",
 "symbols":[{"kind":"struct","line":3,"name":"SegmentReader"},
            {"kind":"function","line":8,"name":"open"},
            {"kind":"function","line":12,"name":"checkpoint_journal_offset"},
            {"kind":"function","line":17,"name":"merge_segments"}]}}
```

## The boundary: definitions only

XERJ extracts definitions only. There is no call graph, no import graph and no cross-file reference resolution. A `defs` hit therefore names the declaration site and never the call site.

Two caps bound the extractor: 2 MB per source file and 5,000 symbols per file. A parse failure is never fatal, because XERJ still indexes the file body as plain text.

## Counting documents, not files

XERJ writes a content document for each source file. On top of that it writes one document for every declaration in the file, keyed by a `code:<line>:<name>` locator. That is what makes a constant or a one-line signature retrievable on its own, rather than only inside the class or method around it. A declaration captured under two kinds at the same line and name collapses to a single document. A document total over a code tree therefore tracks the declaration count, and no fixed ratio to the file count exists. State which convention you count before you compare a document total with a file total, and take the total from your own run.

## What the capture is and is not

The capture indexed a 3-file, 3-language repository on 1 single-node XERJ process. This run measured no large monorepo. The project's own guidance is to plan corpora of at most a few million documents.
