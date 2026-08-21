---
title: "XERJ vs ast-grep — syntax search and an index"
h1: "ast-grep vs a code index for an agent?"
description: "ast-grep wins structural rewriting and XERJ does none of it. For an agent the honest recommendation is to install both and give each the job it was built for."
slug: "xerj-vs-ast-grep"
cluster: "Comparison: code retrieval"
question: "ast-grep vs a code index for an agent?"
intent: "comparison"
published: "2026-08-22"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, run `xerj autoindex ./repo --prefix code --state-dir ./state-code`, POST a match on defs for the function you are about to change to /code-*/_search and read its declaration site from the returned ax_path, apply the structural rewrite with ast-grep, then re-run the identical autoindex command and confirm the same defs query returns the new shape."
commands:
  - cmd: "xerj autoindex ./repo --prefix code --state-dir ./state-code"
    note: "Build the index that answers where a symbol is defined, once per tree."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/code-*/_search' -H 'content-type: application/json' -d '{\"query\":{\"match\":{\"defs\":\"parse_config\"}},\"_source\":[\"ax_path\"],\"size\":20}'"
    note: "The definition query, language-independent, read from the returned hits."
  - cmd: "xerj mcp --url http://127.0.0.1:9200"
    note: "Serve the 10 tools an agent calls before it decides what to change. Start the node first; this command does not start one."
links_out:
  - "syntax-aware-code-search-refactoring"
  - "code-search-mcp-for-claude-code"
  - "index-monorepo-for-agent"
  - "xerj-vs-ripgrep-for-code-agents"
evidence:
  - claim: "ast-grep is a command-line tool for code structural search, lint and rewriting, and it matches abstract syntax tree nodes rather than text."
    source: "https://github.com/ast-grep/ast-grep"
  - claim: "ast-grep rewrites either with the --rewrite flag on a run, or with a fix key inside a YAML rule applied by ast-grep scan."
    source: "https://ast-grep.github.io/guide/rewrite-code.html"
  - claim: "The ast-grep reference lists 26 built-in languages backed by tree-sitter grammars, and custom grammars can be loaded dynamically."
    source: "https://ast-grep.github.io/reference/languages.html"
  - claim: "The ast-grep project states that it builds an abstract syntax tree in memory for each query and does not support offline indexing."
    source: "https://ast-grep.github.io/blog/code-search-design-space.html"
  - claim: "The ast-grep tool comparison states that it has no deep semantic information and operates only on the syntactic level, with no type information, control flow, data flow or taint analysis."
    source: "https://ast-grep.github.io/advanced/tool-comparison.html"
  - claim: "The ast-grep organization publishes an experimental Model Context Protocol server exposing four tools for structural code search."
    source: "https://github.com/ast-grep/ast-grep-mcp"
  - claim: "GNU grep prints lines that contain a match for one or more patterns, which makes it a line-oriented tool rather than a syntax-aware one."
    source: "https://www.gnu.org/software/grep/manual/grep.html"
faq:
  - q: "Is there a syntax-aware code search I can use when refactoring?"
    a: "Yes, ast-grep. It changes code by syntax pattern, and XERJ answers questions about a tree it has already indexed. They do different jobs, so install both."
  - q: "Can XERJ rewrite code?"
    a: "No. XERJ performs no abstract syntax tree rewriting and no codemod of any kind. It returns passages and paths, and you change the file."
  - q: "What does XERJ extract from code?"
    a: "Definitions. Each code document carries a defs field and a symbols list with names, kinds and line numbers, across 34 languages."
  - q: "Does ast-grep keep an index?"
    a: "No. The project states that it builds a syntax tree in memory for each query and does not support offline indexing."
  - q: "Which one does an agent call first?"
    a: "The index, to find the function and read its contract. Then ast-grep, to apply the structural change across every match."
  - q: "Does ast-grep have an MCP server?"
    a: "Yes. Its organization publishes an experimental MCP server with four tools, so agent access is not a place where XERJ stands alone."
  - q: "When should a coding agent use ast-grep instead of grep?"
    a: "When the change is structural. Plain grep stays the right tool for a one-off literal on a tree that changed a second ago, and for a substring inside an identifier, which an analyzed index cannot match."
  - q: "Is there a measured comparison on this page?"
    a: "No. No head-to-head was run. Every ast-grep fact here comes from its own documentation and repository."
---

**TL;DR** — ast-grep changes code by syntax pattern, and XERJ does no rewriting at all. XERJ answers where a symbol is defined and what its contract says, across a tree it has already indexed. Install both. No head-to-head was run, and none is needed to split these jobs.

## This is not a knockout, and pretending otherwise would be dishonest

ast-grep is a command-line tool for structural search, lint and rewriting, and it matches syntax tree nodes rather than text. XERJ is an index that answers questions about a folder.

There is no task where one is a replacement for the other. A page that manufactured a winner here would be marketing, so this one recommends both and says which job goes where.

## What ast-grep does that XERJ cannot

ast-grep rewrites. A pattern and a replacement on one command line change every match:

```sh
ast-grep run --pattern 'foo' --rewrite 'bar' --lang python
```

At repository scale the same idea moves into YAML rules with a `fix` key, applied by `ast-grep scan`. That is a codemod engine, and it is the reason to install it.

XERJ performs no syntax tree rewriting, no codemod and no automatic fix. It has no `--rewrite`, and this page makes no claim in that direction.

The language coverage comes from tree-sitter grammars, with 26 languages built in and custom grammars loadable at run time. There is also `ast-grep lsp` for editors, and the project's own organization publishes an experimental MCP server with four tools.

## What XERJ does that ast-grep does not

ast-grep states its own position clearly: it builds a syntax tree in memory for each query, and it does not support offline indexing. Every run pays the parse cost again.

XERJ pays that cost once:

```sh
xerj autoindex ./repo --prefix code --state-dir ./state-code
```

After that, a code document carries `title`, `language`, a `defs` field with one line per definition, a `symbols` list with names, kinds and line numbers, and the full body. One query finds where a symbol is defined, and it is the same query in every one of 34 languages.

```json
{ "query": { "match": { "defs": "parse_config" } }, "_source": ["ax_path"] }
```

The result is a passage and a path rather than a whole file, which is what keeps an agent's context window from filling with source it did not need.

## The limits both sides publish

ast-grep documents its ceiling: no deep semantic information, syntactic level only, with no type information, no control flow, no data flow and no taint analysis.

XERJ has the matching one. Extraction is definitions only. There is no call graph and no cross-file reference resolution, so XERJ never reports who calls a function.

An analyzed index also cannot match a run of characters that starts in the middle of an identifier, because the analyzer emits whole terms. No setting closes that gap.

## The loop that uses both

Ask the index first. Find the function, read its contract, and list every file that defines a matching symbol. That is one query and a small answer.

Then apply the change with ast-grep, where a pattern and a rewrite handle every match at once.

Then index the tree again, because the index answers from the last run and the files just moved.

## What else is in the folder

A repository is rarely only code. The same `autoindex` run covers thirteen families, so the YAML in `deploy/`, the CSV fixtures, the SQL exports and the docs land in the same index as the source.

ast-grep is aimed at source files. That is the right scope for a rewriting tool, and it is a reason to keep the index around for everything else.

## When to choose ast-grep instead

Choose ast-grep whenever the task is to change code rather than to find it. Rewriting is its job and XERJ has no answer.

Choose ast-grep for a lint rule you want enforced by pattern, applied across a repository through `ast-grep scan`.

Choose ast-grep when the tree changed a second ago. It reads the disk, and an index answers from its last run.

## When to choose grep instead

Choose grep for a one-off literal you can hold in your head. It prints lines that match a pattern, and it needs no build step and no process.

Choose grep for a substring inside an identifier. An analyzed index returns nothing there, and grep returns the file.

## When to choose the index instead

Choose the index when one tree answers many questions in a session. The build cost amortises and the per-query cost does not.

Choose it when the question is about symbols rather than bytes. One definition query then replaces a regular expression per language.

Choose it when an agent needs the answer as a cited passage. The same binary also holds agent memory and an MCP tool list.

## The limits you inherit with XERJ

XERJ is single-node only. There is no data-plane replication and no failover, so one host is the whole deployment.

The server retains heap for every document it indexes, which is an open tracked defect. Indexing several large repositories into one long-lived node is the shape that grows it.

The default embedder is lexical feature hashing, not neural, so a differently worded question needs `--embed-mode neural`, which is opt-in and CPU-only.

## What was not measured

No head-to-head was run for this page. There is no timing here, no refactor score and no task list, because the two tools do not share a task.

Every ast-grep fact above comes from its own documentation, blog or repository. Read those before you decide, and then install both.
