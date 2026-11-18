---
title: "What is an MCP memory server?"
h1: "What is an MCP memory server?"
description: "An MCP memory server gives an AI assistant tools to save and recall facts across chats over the Model Context Protocol. How it works and how XERJ serves one locally."
slug: "what-is-an-mcp-memory-server"
cluster: "Agent memory: concepts"
question: "What is an MCP memory server?"
intent: "definition"
published: "2026-10-28"
updated: "2026-10-28"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, install XERJ, start a local node, register xerj mcp as an MCP server in this client, call xerj_memory_store with one fact in namespace agentmem, then call xerj_memory_recall and report the hit."
answer: "What is an MCP memory server? It is a Model Context Protocol server that gives an AI assistant tools to save facts and recall them later, so memory survives across chats and clients. The assistant calls the tools itself. XERJ ships one inside its binary: xerj mcp exposes xerj_memory_store and xerj_memory_recall."
intent_group: "agent memory"
queries:
  - "MCP memory server"
  - "what is an MCP memory server"
  - "mcp memory server claude code"
  - "local MCP memory server"
evidence:
  - claim: "The reference MCP memory server stores a graph of entities, relations and observations in a local JSONL file set by MEMORY_FILE_PATH."
    source: "https://github.com/modelcontextprotocol/servers/blob/main/src/memory/README.md"
  - claim: "xerj mcp exposes xerj_memory_store, which proxies POST /_memory/{namespace}, and xerj_memory_recall, which proxies POST /_memory/{namespace}/_recall."
    source: "landing/docs/agents/schemas/mcp-tools.json"
  - claim: "Each XERJ memory namespace is backed by its own .xerj-memory-{namespace} index."
    source: "engine/crates/xerj-api/src/memory_api.rs"
commands:
  - cmd: "curl -fsSL https://xerj.org/get | sh"
    note: "Install the single XERJ binary. The MCP server is the xerj mcp subcommand of the same binary."
  - cmd: "xerj mcp --help"
    note: "Show the MCP server options. It talks to a node you already started; it does not start one."
links_out:
  - "what-is-agentic-memory"
  - "set-mcp-memory-storage-path"
  - "private-agent-memory-namespaces"
  - "compare/xerj-vs-agent-memory-tools"
  - "/docs/recipes/agentic-memory"
faq:
  - q: "What does an MCP memory server do?"
    a: "It gives an AI assistant tools to write facts to a store and read them back later. Any MCP client, such as Claude Code or Cursor, can call those tools."
  - q: "How is it different from a normal MCP server?"
    a: "It is a normal MCP server whose tools happen to be about memory. The protocol is the same; only the tools differ."
  - q: "What does the reference MCP memory server store?"
    a: "The modelcontextprotocol reference server keeps entities, relations and observations in a local JSONL file. You choose the file with MEMORY_FILE_PATH."
  - q: "Which memory tools does xerj mcp expose?"
    a: "xerj_memory_store saves a fact to a namespace and xerj_memory_recall finds it again. They call POST /_memory/{namespace} and its _recall endpoint on a local node."
  - q: "Does the XERJ memory server need an API key or a model?"
    a: "The default lexical embedder needs no model download or external provider key. A secured XERJ node still requires its own API key. Neural or proxy embedding is optional and also runs when memories are stored."
  - q: "Where does XERJ keep the memories?"
    a: "In the node data directory, one .xerj-memory-{namespace} index per namespace. Change the location by starting the node with a different data directory."
---

**TL;DR**: An MCP memory server is a Model Context Protocol server whose tools save and recall facts, so an agent keeps what it learned across chats. XERJ ships one in the same binary as its search engine: `xerj mcp` exposes `xerj_memory_store` and `xerj_memory_recall`.

## The short definition

MCP is the protocol AI clients like Claude Code and Cursor use to call outside tools. A memory server is an MCP server with two jobs: save a fact when the assistant asks, and find it again later.

The assistant decides when to call those tools. That is what makes the memory agentic.

## Two common designs

| design | what it stores | how it finds things |
| --- | --- | --- |
| the reference server | entities, relations and observations in a JSONL file | reads the graph and matches names and text |
| XERJ | each fact as a document in a search index | BM25 search by default, with filters |

The reference server from the modelcontextprotocol project is a good small start. XERJ fits when memory should sit next to the files and data your agent already searches.

## How XERJ does it

XERJ is the search engine for AI agents. `xerj mcp` is an MCP stdio server inside the same binary. Point it at a local node you already started.

- `xerj_memory_store` saves a fact to a namespace. The configured embedder runs at write time; lexical feature hashing is the default.
- `xerj_memory_recall` finds it again. The default is BM25 over the stored text.
- Each namespace is its own index. Privacy between agents requires separately scoped API keys, not only different namespace names.

The default embedder is lexical feature hashing. Recall with `semantic: true` matches shared words unless the node runs with `--embed-mode neural`.

## Limits to know

XERJ is single-node. Memories are permanent until explicitly forgotten; there is no decay and no TTL.

## Try it

Copy the prompt on this page into your coding agent, or read the [agent memory recipe](/docs/recipes/agentic-memory).
