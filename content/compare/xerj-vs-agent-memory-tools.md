---
title: "XERJ vs Mem0, Zep, Letta and Cognee for agent memory"
h1: "XERJ vs agent memory tools: Mem0, Zep, Letta and Cognee"
description: "How XERJ compares with Mem0, Zep, Letta and Cognee for AI agent memory: what each one stores, what it needs to run, and when to pick each one."
slug: "xerj-vs-agent-memory-tools"
cluster: "Comparison: agent memory"
question: "Should I use Mem0, Zep, Letta, Cognee or XERJ for AI agent memory?"
intent: "comparison"
published: "2026-10-28"
updated: "2026-10-28"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "Article"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, install XERJ, start a local node, store three facts about this project with POST /_memory/project, recall each one with POST /_memory/project/_recall, and tell me whether plain search recall is enough for my agent before I add an LLM-based memory tool."
answer: "Should I use Mem0, Zep, Letta, Cognee or XERJ for agent memory? Pick Mem0 or Cognee to have a model extract facts and links for you, Zep or Graphiti for facts that change over time, Letta for a full stateful agent. Pick XERJ for plain local memory, searched like any other data, with no generative fact-extraction step."
intent_group: "agent memory"
queries:
  - "mem0 vs zep vs letta"
  - "mem0 alternative open source"
  - "zep alternative self-hosted"
  - "agent memory tools comparison"
  - "self-hosted agent memory no LLM"
evidence:
  - claim: "Mem0 uses an LLM for its default fact-extraction flow and an embedding model for retrieval. Its add API also supports infer=false to store raw text without fact extraction. It ships as a library, a self-hosted server and a cloud platform, under Apache 2.0."
    source: "https://github.com/mem0ai/mem0/blob/main/docs/core-concepts/memory-operations/add.mdx"
  - claim: "Zep Cloud is the managed product. Zep Community Edition is deprecated and unsupported. Graphiti is the open-source framework inside Zep for temporal context graphs, and you supply the graph database, LLM and embedder."
    source: "https://github.com/getzep/zep"
  - claim: "Graphiti is an Apache-2.0 framework for building temporal context graphs for AI agents; you bring your own graph database such as Neo4j or FalkorDB."
    source: "https://github.com/getzep/graphiti"
  - claim: "Letta (formerly MemGPT) is a harness for stateful agents with memory blocks and a git-versioned memory filesystem (MemFS). The local runtime keeps agent state on the device. Apache 2.0."
    source: "https://github.com/letta-ai/letta-code"
  - claim: "Letta docs describe MemFS as the git-versioned memory filesystem agents read, write and reorganize."
    source: "https://docs.letta.com/llms.txt"
  - claim: "Cognee is an Apache-2.0 open-source AI memory platform that turns documents, code and conversations into a graph of entities and relationships plus searchable chunks. It runs locally without an LLM key using a local GLiNER extraction model; an LLM key adds generated answers."
    source: "https://github.com/topoteretes/cognee"
  - claim: "XERJ memory: POST /_memory/{namespace} stores the text the agent sends, recall defaults to BM25, the configured embedder runs at write time, with lexical feature hashing by default, and each namespace is its own index."
    source: "engine/crates/xerj-api/src/memory_api.rs"
commands:
  - cmd: "curl -fsSL https://xerj.org/get | sh"
    note: "Install the single XERJ binary."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/_memory/project' -H 'content-type: application/json' -d '{\"text\":\"Releases are cut from the main branch on Fridays.\"}'"
    note: "Store one fact exactly as the agent wrote it."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/_memory/project/_recall' -H 'content-type: application/json' -d '{\"query\":\"Releases Fridays\",\"k\":3}'"
    note: "Recall with words present in the stored fact. Plain query recall is BM25, not paraphrase understanding."
links_out:
  - "xerj-vs-vector-database"
  - "xerj-vs-khoj"
  - "answers/what-is-agentic-memory"
  - "answers/what-is-an-mcp-memory-server"
  - "answers/agentic-memory-vs-rag"
  - "answers/store-agent-memory-without-llm-call"
faq:
  - q: "What is the main difference between XERJ and Mem0?"
    a: "Mem0 extracts facts by default and supports raw inserts with infer=false. XERJ stores the supplied text and generates a search vector with its configured embedder, lexical feature hashing by default."
  - q: "Is Zep open source?"
    a: "Zep Cloud is a commercial cloud product. Zep Community Edition is deprecated. The open-source part is Graphiti, an Apache-2.0 framework where you bring your own graph database, LLM and embedder."
  - q: "Is Letta a memory layer?"
    a: "Letta is broader: it is a harness for stateful agents, with memory blocks and a git-versioned memory filesystem. You run your agent inside Letta rather than calling it as a store."
  - q: "Does Cognee need an LLM?"
    a: "Not for ingestion and retrieval. Its README says it runs locally with a GLiNER model and a local embedder, and an LLM key adds generated answers."
  - q: "When is XERJ the better fit?"
    a: "When you want memory as plain searchable records on your own machine, next to the files and data your agent already searches, without an LLM or a second database."
  - q: "Did you benchmark these tools against each other?"
    a: "No. No head-to-head was run. This page compares what each project documents about itself."
---

**TL;DR**: Mem0, Zep, Letta and Cognee add intelligence on top of memory: models that extract facts, graphs that track change, or a whole agent runtime. XERJ does less on purpose. It stores what your agent writes and finds it with search, in one local binary with no generative fact-extraction step. No head-to-head was run.

## What each one is

| tool | what it is | needs a model to store? | license |
| --- | --- | --- | --- |
| Mem0 | fact extraction by default; raw inserts with infer=false | LLM extraction optional; embeddings still apply | Apache 2.0 |
| Zep and Graphiti | Zep Cloud, plus the open Graphiti framework for temporal context graphs | Graphiti: you supply the LLM | Graphiti: Apache 2.0 |
| Letta | harness for stateful agents with memory blocks and MemFS | runs your agent's model | Apache 2.0 |
| Cognee | memory platform that builds a graph of entities and relationships | local extraction and embedding models; LLM optional | Apache 2.0 |
| XERJ | search engine for AI agents with a memory API | lexical by default; neural/proxy embedder optional | Apache 2.0 |

Each row comes from the project's own README, linked in the sources.

## The real difference

These tools support different memory workflows. Mem0 extracts facts by default and can also store raw text. Graphiti builds a graph whose facts have validity windows. Cognee pulls out entities and links. Letta lets the agent rewrite its own memory files.

XERJ keeps the raw text the agent sends and makes it searchable. Keeping the original text makes it possible to inspect what was stored. Two 2026 preprints report competitive raw-history retrieval on their evaluated tasks; neither evaluates XERJ or establishes its cost or quality. See [agentic memory vs RAG](/answers/agentic-memory-vs-rag).

## When to choose Mem0 instead

Choose Mem0 when you want a model to turn chats into short facts. Its default extraction path calls an LLM; infer=false skips that extraction, while embedding configuration still matters. Mem0 also offers a cloud platform. XERJ memory runs on your own machine.

## When to choose Zep or Graphiti instead

Choose Zep Cloud when you want Zep to run it for you. Choose self-hosted Graphiti when facts change over time and you must know what was true when. Graphiti expects a graph store such as Neo4j or FalkorDB.

## When to choose Letta instead

Choose Letta when you want the whole agent, not just a store. It puts memory blocks, a memory filesystem, skills and channels in one harness.

## When to choose Cognee instead

Choose Cognee when you want documents, code and chats turned into a graph of entities and relationships. Cognee is a Python stack.

## When to choose XERJ

Choose XERJ when you want memory and search in one place, on your machine, with nothing extra to run.

- Store with `POST /_memory/{namespace}` and recall with `_recall`. Over MCP, use `xerj_memory_store` and `xerj_memory_recall`.
- Each namespace is its own index; use separately scoped API keys to enforce access between agents.
- The same binary also searches your files, logs and code, so memory and data sit side by side.

## Limits you inherit with XERJ

XERJ is single-node: no replication and no failover. It does not extract facts or build entity links for you. The default embedder is lexical feature hashing, so `semantic: true` recall matches shared words unless you start the node with `--embed-mode neural`. Memories are permanent until explicitly forgotten.

## What was not measured

No head-to-head was run. No accuracy, speed or cost numbers are claimed for any tool here. Competitor behavior is described from the linked project documentation, checked on 2026-10-29. These integrations were not executed in this review.
