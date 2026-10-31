---
title: "How do I run syntax-aware code search?"
h1: "How do I run syntax-aware code search?"
description: "XERJ indexes source files with a defs field and a symbols array, so one query finds declarations with kind, name and line while another finds raw text."
slug: "syntax-aware-code-search-refactoring"
cluster: "Files and formats"
question: "Is there a syntax-aware code search tool for refactoring?"
intent: "tool-selection"
published: "2026-08-21"
updated: "2026-10-30"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a XERJ node with --insecure, run xerj autoindex ./code --prefix code, then send two queries to code-*/_search: a match on body for a symbol name and a match on defs for the same name, and report ax_path, language and the symbols array with kind, name and line for each declaration hit."
answer: "Is there a syntax-aware code search tool for refactoring? XERJ gives you two code queries on one index: match on body for raw text, and match on defs for declared symbols, which returns a symbols array with kind, name and line. It extracts definitions only and rewrites nothing. For structural rewrites use ast-grep."
key_fact: "XERJ's tree-sitter registry covers 36 languages, pinned by a unit test."
key_fact_source: "engine/crates/xerj-autoindex/src/extract/code.rs"
key_fact_date: "2026-10-19"
intent_group: "code search for agents"
queries:
  - "tree-sitter code search CLI"
  - "find function definition across repo CLI"
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
evidence:
  - claim: "A source file over the 2 MiB code cap (CODE_CAP = 2 << 20) is refused, and the refusal reason names the cap."
    source: "engine/crates/xerj-autoindex/src/extract/code.rs"
faq:
  - q: "What makes the search syntax-aware?"
    a: "XERJ parses each source file with tree-sitter and writes the declared symbols into a `defs` field and a `symbols` array with a kind, a name and a line."
  - q: "How do I search declarations only?"
    a: "Send a `match` query on the `defs` field. In our capture `merge_segments` returned 2 files that declare the name, each with its `symbols` array."
  - q: "Does XERJ find every caller of a function?"
    a: "No. Extraction is definitions only, with no call graph and no cross-file reference resolution. Use a language server when a rename must be sound."
  - q: "Which languages does XERJ parse?"
    a: "36 languages through tree-sitter, including Rust, Go, Python, TypeScript, Java and C++. Our capture exercised `rust`, `python` and `go`."
  - q: "Does case matter in a symbol query?"
    a: "Yes, in practice. `src/query.go` declares `MergeSegments`, and the query token `merge_segments` did not match it in our capture."
  - q: "What happens if a file fails to parse?"
    a: "Nothing is lost. If a grammar fails to parse a file, XERJ indexes the file as plain text, so the text query keeps working."
  - q: "How large a source file can XERJ parse?"
    a: "2 MiB per file. A larger file is refused, and the refusal names the cap. A file keeps at most 5,000 symbols, and a symbol name longer than 200 bytes is skipped."
---

**TL;DR** XERJ gives you 2 code queries on one index. A `match` on `body` finds raw text, and a `match` on `defs` finds declared symbols with a kind, a name and a line. Extraction is definitions only, so XERJ rewrites nothing.

```visual
code-two-queries
```

## One index, two fields

`xerj autoindex` writes `body`, `defs`, `symbols`, `symbol_count`, `language` and `title` for every source file. Query `body` for any occurrence. Query `defs` for a declaration. The first command above indexes a source tree into the prefix `code`, and the next two are the queries in the figure.

## What the definition hit carries

A `defs` hit returns the file's `symbols` array. That array is what a refactoring step consumes: it says where each declaration starts.

```visual
code-symbol-lines
```

On this 3-file tree both queries returned 2 hits, so the definition query was not narrower here. The difference is the field searched and the fields returned. Case matters too. `src/query.go` declares `MergeSegments`, and `merge_segments` did not match it.

## Where XERJ stops

XERJ extracts definitions only. It builds no call graph, no import graph and no cross-file reference resolution, so it does not find the callers of a symbol.

| Step in a refactor | Tool |
| --- | --- |
| Find every declaration of a name across 36 languages | XERJ, `match` on `defs` |
| Find every textual occurrence, call sites included | XERJ, `match` on `body` |
| A rename that must be sound | A language server or a compiler plugin |
| A structural rewrite | ast-grep |

What an agent reads, against the lines grep prints, is on the [local code search over MCP page](/answers/local-code-search-mcp-fewer-tokens-than-grep).

## Caps and failure behavior

```visual
code-caps
```

The capture ran on 1 single-node XERJ process over 3 source files in 3 languages: `rust`, `python` and `go`. No large repository was measured for this page.
