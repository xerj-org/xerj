---
title: "How do I run syntax-aware code search?"
h1: "How do I run syntax-aware code search?"
description: "XERJ indexes source files with a defs field and a symbols array, so one query finds declarations with kind, name and line while another finds raw text."
slug: "syntax-aware-code-search-refactoring"
cluster: "Files and formats"
question: "Is there a syntax-aware code search tool for refactoring?"
intent: "tool-selection"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a XERJ node with --insecure, run xerj autoindex ./code --prefix code, then send two queries to code-*/_search: a match on body for a symbol name and a match on defs for the same name, and report ax_path, language and the symbols array with kind, name and line for each declaration hit."
commands:
  - cmd: "xerj autoindex ./code --url http://127.0.0.1:9410 --prefix code --state-dir ./state-code --progress plain --disable-feedback"
    note: "Index the source tree into the prefix code."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9410/code-*/_search' -H 'content-type: application/json' -d '{\"query\":{\"match\":{\"body\":\"checkpoint_journal_offset\"}},\"size\":10,\"_source\":[\"ax_path\",\"language\",\"title\",\"defs\",\"symbol_count\"],\"track_total_hits\":true}'"
    note: "Find every textual occurrence of a symbol name."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9410/code-*/_search' -H 'content-type: application/json' -d '{\"query\":{\"match\":{\"defs\":\"merge_segments\"}},\"size\":10,\"_source\":[\"ax_path\",\"language\",\"title\",\"defs\",\"symbols\"],\"track_total_hits\":true}'"
    note: "Find only the files that declare the symbol."
links_out:
  - "index-monorepo-for-agent"
  - "xerj-vs-ripgrep-for-code-agents"
  - "/case-studies/reference-coding"
faq:
  - q: "What makes the search syntax-aware?"
    a: "XERJ parses each source file with tree-sitter and writes the declared symbols into a `defs` field and a `symbols` array with a kind, a name and a line."
  - q: "How do I search declarations only?"
    a: "Send a `match` query on the `defs` field. In our capture `merge_segments` returned 2 files that declare the name, each with its `symbols` array."
  - q: "Does XERJ find every caller of a function?"
    a: "No. Extraction is definitions only, with no call graph and no cross-file reference resolution. Use a language server when a rename must be sound."
  - q: "Which languages does XERJ parse?"
    a: "34 languages through tree-sitter, including Rust, Go, Python, TypeScript, Java and C++. Our capture exercised `rust`, `python` and `go`."
  - q: "Does case matter in a symbol query?"
    a: "Yes, in practice. `src/query.go` declares `MergeSegments`, and the query token `merge_segments` did not match it in our capture."
  - q: "What happens if a file fails to parse?"
    a: "Nothing is lost. A parse failure is never fatal, and XERJ still indexes the file body as plain text so the text query keeps working."
  - q: "How large a source file can XERJ parse?"
    a: "2 MB per file, with at most 5,000 symbols per file. A symbol name longer than 200 characters is truncated."
---

**TL;DR** — XERJ gives you 2 code queries on the same index: a `match` on `body` for raw text, and a `match` on `defs` for declared symbols. The definition query returns a `symbols` array with a kind, a name and a line. Extraction is definitions only.

## Two queries over one index

`xerj autoindex` writes `body`, `defs`, `symbols`, `symbol_count`, `language` and `title` for every source file, so text search and symbol search share one index. Query `body` when you want any occurrence, and query `defs` when you want a declaration.

Index a source tree into the prefix `code` first.

```sh
xerj autoindex ./code --url http://127.0.0.1:9410 --prefix code --state-dir ./state-code --progress plain --disable-feedback
```

## The text match

A `match` on `body` for `checkpoint_journal_offset` returned 2 hits in our capture, `src/ingest.py` at `0.5359981` and `src/lib.rs` at `0.4608517`. The text query searches the whole file text, so it also matches a call site inside a function body.

```sh
curl -s -XPOST 'http://127.0.0.1:9410/code-*/_search' -H 'content-type: application/json' -d '{"query":{"match":{"body":"checkpoint_journal_offset"}},"size":10,"_source":["ax_path","language","title","defs","symbol_count"],"track_total_hits":true}'
```

## The definition-only match

A `match` on `defs` for `merge_segments` returned 2 hits, and each hit carried a `symbols` array with a kind, a name and a line number. The `defs` field holds only declared symbols, so a hit means the file declares the name.

```sh
curl -s -XPOST 'http://127.0.0.1:9410/code-*/_search' -H 'content-type: application/json' -d '{"query":{"match":{"defs":"merge_segments"}},"size":10,"_source":["ax_path","language","title","defs","symbols"],"track_total_hits":true}'
```

The Rust hit carried 4 symbols with the line of each declaration.

| kind | name | line |
| --- | --- | --- |
| `struct` | `SegmentReader` | 3 |
| `function` | `open` | 8 |
| `function` | `checkpoint_journal_offset` | 12 |
| `function` | `merge_segments` | 17 |

## What the capture actually proves

Both queries returned 2 hits on this 3-file tree, so the definition query was not narrower here. What differs is the field searched and the fields returned, and the `symbols` array is what a refactoring tool consumes.

One miss is worth naming. The file `src/query.go` declares `MergeSegments`, and the token `merge_segments` did not match it. Spell the identifier the way the language spells it.

## The boundary for refactoring work

XERJ extracts definitions only. There is no call graph, no import graph and no cross-file reference resolution, so XERJ does not find callers of a symbol.

That boundary decides the tool choice. Use XERJ to locate every declaration and every textual occurrence across 34 languages. Use a language server or a compiler plugin for a rename that must be sound.

## Caps and failure behavior

Tree-sitter extraction stops at 2 MB per source file and 5,000 symbols per file, and a symbol name is truncated at 200 characters. A parse failure is never fatal. XERJ still indexes the file body as plain text, so the text query keeps working.

The capture ran on 1 single-node XERJ process over 3 source files in 3 languages. This run measured no large repository.
