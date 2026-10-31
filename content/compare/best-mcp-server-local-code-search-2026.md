---
title: "Best MCP servers for local code search in 2026, compared"
h1: "The best MCP servers for searching a local codebase in 2026"
description: "Seven MCP servers for local code search compared on licence, install, index and API keys, and what each does better than XERJ. Checked 2026-10-19."
slug: "best-mcp-server-local-code-search-2026"
cluster: "Comparison: code retrieval"
question: "What is the best MCP server to search a local codebase in 2026?"
intent: "comparison"
published: "2026-10-19"
updated: "2026-10-28"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, then pick a local code-search MCP server for this repository from the table on this page: Serena if you need references and refactors, Semble or cocoindex-code for embedding-based search with no API key, ast-grep MCP for structural patterns, XERJ for one index over code plus docs and data files. Report which one you chose and why."
answer: "What is the best MCP server to search a local codebase in 2026? It depends on the job. Serena wins symbol navigation and refactoring through language servers. Semble and cocoindex-code give embedding-based search with no API key. ast-grep MCP matches syntax patterns. XERJ indexes code plus docs and data files behind one server."
key_fact: "Serena has 30,404 GitHub stars and shipped v2.1.0 on 2026-10-19, the most-starred server on this list; XERJ has 10,569."
key_fact_source: "https://github.com/oraios/serena"
key_fact_date: "2026-10-19"
intent_group: "code search for agents"
queries:
  - "best MCP server search local codebase 2026"
  - "best MCP server grep alternative fewer tokens 2026"
  - "best local code search MCP Claude Code fewer tokens than grep 2026"
  - "best code search tool for Cursor agent 2026"
  - "best search code by meaning offline 2026"
og_image: "og/compare-best-mcp-server-local-code-search-2026.png"
og_image_alt: "Cover: halftone bars of GitHub stars for seven local code-search MCP servers, checked 2026-10-19, with XERJ in yellow"
last_checked: "2026-10-19"
items:
  - name: "Serena"
    url: "https://github.com/oraios/serena"
    license: "GPL-3.0-or-later (SolidLSP: MIT)"
    install: "uv tool install -p 3.13 serena-agent"
  - name: "Claude Context"
    url: "https://github.com/zilliztech/claude-context"
    license: "MIT"
    install: "npx @zilliz/claude-context-mcp@latest"
  - name: "Semble"
    url: "https://github.com/MinishLab/semble"
    license: "MIT"
    install: "uv tool install semble"
  - name: "cocoindex-code"
    url: "https://github.com/cocoindex-io/cocoindex-code"
    license: "Apache-2.0"
    install: "pipx install 'cocoindex-code[full]'"
  - name: "codedb"
    url: "https://github.com/justrach/codedb"
    license: "BSD-3-Clause"
    install: "install script or npx -y codedeebee mcp"
  - name: "ast-grep MCP"
    url: "https://github.com/ast-grep/ast-grep-mcp"
    license: "MIT"
    install: "uvx --from git+https://github.com/ast-grep/ast-grep-mcp ast-grep-server"
  - name: "XERJ"
    url: "https://github.com/xerj-org/xerj"
    license: "Apache-2.0"
    install: "curl -fsSL https://xerj.org/get | sh"
links_out:
  - /answers/code-search-mcp-for-claude-code
  - /answers/local-code-search-mcp-fewer-tokens-than-grep
  - /compare/best-grep-alternative-for-ai-coding-agents-2026
  - /compare/xerj-vs-ripgrep-for-code-agents
  - /compare/xerj-vs-ast-grep
  - /answers/index-monorepo-for-agent
evidence:
  - claim: "Serena provides semantic code retrieval, editing and refactoring tools through language servers, supports over 40 programming languages with its language-server backend, and installs with uv tool install -p 3.13 serena-agent."
    source: "https://github.com/oraios/serena"
  - claim: "Claude Context needs a vector database (Milvus or Zilliz Cloud) and, in its documented configuration, an OpenAI API key for the embedding model; the MCP server runs with npx @zilliz/claude-context-mcp@latest."
    source: "https://github.com/zilliztech/claude-context"
  - claim: "Semble runs on CPU with no API keys, GPU or external services, downloads its embedding model from Hugging Face on first use, chunks files with tree-sitter, and exposes a search MCP tool."
    source: "https://github.com/MinishLab/semble"
  - claim: "cocoindex-code's full install pulls in sentence-transformers for local embeddings with no API key; the slim install requires a cloud embedding provider and an API key."
    source: "https://github.com/cocoindex-io/cocoindex-code"
  - claim: "codedb is alpha; structural search runs locally, and its default hybrid retrieval uses hosted embeddings unless semantic=local is chosen."
    source: "https://github.com/justrach/codedb"
  - claim: "ast-grep MCP is an experimental server exposing four tools and requires ast-grep and uv to be installed."
    source: "https://github.com/ast-grep/ast-grep-mcp"
  - claim: "ripgrep searches files directly with no index; on one 331-file tree it won 5 of 30 disclosed tasks outright against the XERJ index."
    source: "https://github.com/BurntSushi/ripgrep"
  - claim: "XERJ's tree-sitter registry covers 36 languages and extracts definitions."
    source: "engine/crates/xerj-autoindex/src/extract/code.rs"
  - claim: "On main xerj mcp serves 23 tools, or 20 in a build with no hub key."
    source: "landing/docs/agents/schemas/mcp-tools.json"
  - claim: "In the JSONL benchmark of 2026-10-19 the XERJ node peaked at 9,578,548 kB (about 9.1 GiB) resident while indexing one 9.7 MB file of 50,000 records."
    source: "benchmarks/best-of-jsonl-search-2026/README.md"
  - claim: "Semble's README claims about 99 % fewer tokens than grep plus read, measured by the vendor on about 1,250 queries over 63 repositories."
    source: "https://github.com/MinishLab/semble"
faq:
  - q: "Which MCP server for local code search needs no API key?"
    a: "Serena, Semble, cocoindex-code with its full install, ast-grep MCP and XERJ all run without an API key. Claude Context's documented setup needs a vector database and an OpenAI key for embeddings, and codedb's default hybrid retrieval uses hosted embeddings unless you choose semantic=local."
  - q: "Which one is best for finding references and renaming symbols?"
    a: "Serena. It drives real language servers, so it can find references and perform symbol-level edits. XERJ extracts definitions only and rewrites nothing, and the embedding-based servers return passages, not reference graphs."
  - q: "Which one searches by meaning offline?"
    a: "Semble and cocoindex-code ship local embedding models. XERJ's default embedder is lexical feature hashing; embedding-based retrieval needs the opt-in --embed-mode neural, which downloads a small model once."
  - q: "When does XERJ beat the code-only servers?"
    a: "When the agent's questions span code and the files around it: docs, JSONL, CSV, logs, PDFs and SQLite. XERJ indexes all of them into one Elasticsearch-compatible node behind one MCP server, and the same node stores agent memory."
  - q: "Were these servers benchmarked against each other?"
    a: "No. This page compares documented capabilities and GitHub data checked on 2026-10-19. No head-to-head retrieval benchmark was run, and vendor token-saving claims are quoted as vendor claims."
  - q: "How were stars and releases collected?"
    a: "From the GitHub REST API on 2026-10-19: stargazers_count from the repository endpoint and the latest release from the releases/latest endpoint. A server with no GitHub release shows none."
---

**TL;DR** — There is no single best local code-search MCP server; there are four jobs. Serena owns symbol navigation and refactoring. Semble and cocoindex-code own embedding-based search with no API key (XERJ's default embedder is lexical feature hashing). ast-grep MCP owns structural patterns. XERJ owns one index over code and everything beside it. No head-to-head was run.

## Pick one in ten seconds

- Pick **Serena** if the agent must find references, rename symbols or edit at the symbol level. Language servers do that; nothing else here does.
- Pick **Semble** if you want embedding-based code search on a CPU with no API key and an installer that wires itself into Claude Code, Codex or OpenCode.
- Pick **cocoindex-code** if you want the same with an AST-based index and a plugin for Claude Code or Grok, and can afford its roughly 1 GB of local embedding dependencies.
- Pick **Claude Context** if you already run Milvus or Zilliz Cloud and are fine sending code to an embedding API.
- Pick **ast-grep MCP** if the question is a syntax pattern ("every call to `unwrap()` inside a loop"), not a topic.
- Pick **codedb** if you want a single binary with callers and dependency graphs and accept alpha status. XERJ has no call graph.
- Pick **XERJ** if the agent asks about code and the docs, logs, JSONL, CSV or SQLite files next to it, through one MCP server that also stores memory.

## Summary table

| Server | Licence | Install | Index and retrieval | API key needed | Stars · latest release (2026-10-19) |
|---|---|---|---|---|---|
| Serena | GPL-3.0-or-later (SolidLSP MIT) | `uv tool install -p 3.13 serena-agent` | language servers, 40+ languages; symbols, references, edits | no | 30,404 · v2.1.0 (2026-10-19) |
| Claude Context | MIT | `npx @zilliz/claude-context-mcp@latest` | embeddings in Milvus / Zilliz Cloud | yes, as documented | 12,627 · no GitHub release |
| Semble | MIT | `uv tool install semble` | tree-sitter chunks, static embeddings on CPU | no | 6,248 · v0.7.0 (2026-10-16) |
| cocoindex-code | Apache-2.0 | `pipx install 'cocoindex-code[full]'` | AST chunks, local sentence-transformers | no (full) / yes (slim) | 2,930 · v0.2.42 (2026-10-05) |
| codedb | BSD-3-Clause | install script or `npx -y codedeebee mcp` | parsers for 12 language families; hybrid | default uses hosted embeddings | 1,395 · v0.2.5860 (2026-09-27) |
| ast-grep MCP | MIT | `uvx --from git+https://github.com/ast-grep/ast-grep-mcp ast-grep-server` | no index; AST pattern match per call | no | 482 · no GitHub release |
| XERJ | Apache-2.0 | `curl -fsSL https://xerj.org/get \| sh` | BM25 index + tree-sitter definitions, 36 languages; code, docs and data files | no | 10,569 · v1.0.0-rc.97 (2026-10-19) |

## Serena: the one with an IDE behind it

Serena wraps language servers and exposes symbol-level retrieval, editing and refactoring as MCP tools. Its README claims support for over 40 languages through that backend, and a JetBrains plugin backend for more.

That is a capability XERJ does not have. XERJ records where a symbol is defined, has no call graph, and does not edit. If the agent's next step is a rename across files, Serena is the right tool.

## Semble and cocoindex-code: embedding-based search, no key

Semble chunks files with tree-sitter and scores chunks with a static embedding model that runs on a CPU. It needs no API key, GPU or external service; the model downloads from Hugging Face on first use. Its README claims about 99 % fewer tokens than grep plus read, measured by the vendor on about 1,250 queries over 63 repositories. We did not reproduce that.

cocoindex-code builds an AST-based index. Its full install pulls in sentence-transformers so local embeddings work with no key; the slim install requires a cloud embedding provider.

Both beat XERJ's default path on synonym-style questions, because XERJ's default embedder is lexical feature hashing. XERJ needs `--embed-mode neural` for that.

## Claude Context and codedb: hosted embeddings by default

Claude Context's documented setup asks for a vector database (Milvus or Zilliz Cloud) and an OpenAI API key for embeddings. It is a good fit if that infrastructure already exists; it is not an offline tool.

codedb is a single binary that registers itself in many MCP clients. Its README marks it alpha and says the default hybrid retrieval uses hosted embeddings, with `semantic=local` for local-only retrieval.

## ast-grep MCP: patterns, not topics

The ast-grep MCP server is experimental and exposes four tools, including one that dumps a syntax tree so the agent can write a working pattern. It builds no index, so it never goes stale, and it needs ast-grep and uv installed.

Use it beside an index, not instead of one. Choose ast-grep when the question is a pattern rather than a topic. The [XERJ vs ast-grep](/compare/xerj-vs-ast-grep) page splits those jobs in detail.

## XERJ: one index over the whole folder

XERJ is a search engine first. `xerj autoindex` reads a repository plus the docs, logs, JSONL, CSV, PDFs and SQLite files around it, writes a `defs` field and a `symbols` array for 36 tree-sitter languages, and `xerj mcp` serves 23 tools over that node, memory included.

What it gives up: no references, no edits, a lexical default embedder, single-node only, and heavy memory while indexing. Our own [JSONL benchmark](/compare/best-local-full-text-search-jsonl-csv-2026) measured the node peaking at about 9.1 GiB resident while indexing a 9.7 MB file. For code-only questions, compare it with ripgrep first: on 30 disclosed tasks ripgrep won 5 outright ([XERJ vs ripgrep](/compare/xerj-vs-ripgrep-for-code-agents)). Choose ripgrep when the agent searches a tree once.

## How this page was checked

Every licence, install line and capability above was read from the project's own README or docs on 2026-10-19. Stars and latest releases came from the GitHub REST API the same day. Nothing was benchmarked head to head, and every vendor performance number is labelled as the vendor's.
