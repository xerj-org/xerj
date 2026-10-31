---
title: "What is agentic memory? A plain answer"
h1: "What is agentic memory?"
description: "Agentic memory is memory an AI agent writes and reads itself through tools, so facts survive past one chat. What it is, the common types, and how to run it locally."
slug: "what-is-agentic-memory"
cluster: "Agent memory: concepts"
question: "What is agentic memory?"
intent: "definition"
published: "2026-10-28"
updated: "2026-10-28"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, install XERJ, start one local node, store one durable fact with POST /_memory/agentmem, recall it with POST /_memory/agentmem/_recall, and report the exact hit you got back."
answer: "What is agentic memory? It is memory that an AI agent manages itself: the agent decides what to save, writes it through a tool call, and searches it again in a later task or session. It differs from a chat history because it is stored outside the model and survives restarts. XERJ serves it as a search endpoint."
intent_group: "agent memory"
queries:
  - "agentic memory"
  - "what is agentic memory"
  - "AI agent memory"
  - "agentic memory types"
  - "long-term memory for AI agents"
evidence:
  - claim: "LangChain describes agents updating memory in the hot path, where the agent explicitly decides to remember facts, usually via tool calling, or in the background by a separate process."
    source: "https://www.langchain.com/blog/memory-for-agents"
  - claim: "LangChain, after the CoALA paper, splits agent long-term memory into procedural, semantic and episodic memory."
    source: "https://www.langchain.com/blog/memory-for-agents"
  - claim: "XERJ memory: POST /_memory/{namespace} stores, POST /_memory/{namespace}/_recall recalls, DELETE /_memory/{namespace}/{id} forgets one entry, each namespace is its own .xerj-memory-{namespace} index."
    source: "engine/crates/xerj-api/src/memory_api.rs"
  - claim: "xerj mcp exposes xerj_memory_store and xerj_memory_recall as MCP tools."
    source: "landing/docs/agents/schemas/mcp-tools.json"
commands:
  - cmd: "curl -fsSL https://xerj.org/get | sh"
    note: "Install the single XERJ binary."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/_memory/agentmem' -H 'content-type: application/json' -d '{\"text\":\"The user wants answers in metric units.\"}'"
    note: "The agent saves one fact. Port 9200 is the default Elasticsearch-compatible listener."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/_memory/agentmem/_recall' -H 'content-type: application/json' -d '{\"query\":\"which units does the user want\",\"k\":3}'"
    note: "A later task reads it back. Plain query recall is BM25 over the stored text."
links_out:
  - "agentic-memory-vs-rag"
  - "what-is-an-mcp-memory-server"
  - "compare/xerj-vs-agent-memory-tools"
  - "agent-memory-without-postgres-qdrant"
  - "store-agent-memory-without-llm-call"
  - "/docs/recipes/agentic-memory"
faq:
  - q: "What is agentic memory in simple words?"
    a: "Notes an AI agent keeps for itself. The agent saves a fact with a tool call and looks it up later, so the fact outlives the chat that produced it."
  - q: "How is agentic memory different from a context window?"
    a: "The context window is what the model sees right now and it is lost when the session ends. Agentic memory lives in a store outside the model and is searched back in when needed."
  - q: "What types of agentic memory are there?"
    a: "A common split, from the CoALA paper as summarized by LangChain, is semantic memory for facts, episodic memory for past events and procedural memory for how to do a task."
  - q: "Does agentic memory need a vector database?"
    a: "No. Recall is a search problem. XERJ stores memories as documents in one local binary and recalls them with BM25 by default, with no second database."
  - q: "Can an agent use XERJ memory over MCP?"
    a: "Yes. The xerj mcp server exposes xerj_memory_store and xerj_memory_recall, which call the same /_memory endpoints."
  - q: "Does XERJ forget memories on its own?"
    a: "No. There is no decay and no TTL. A memory is permanent until explicitly forgotten with DELETE /_memory/{namespace}/{id}."
---

**TL;DR**: Agentic memory is memory an AI agent manages for itself. The agent decides what to keep, saves it with a tool call, and searches it again in a later task. Recall is a search problem, so XERJ serves agent memory as two endpoints on one local search engine.

## The short definition

An agent with memory can say "remember this" and find it again next week. The memory lives outside the model, in a store the agent reads and writes through tools.

LangChain calls the agent-driven version updating memory "in the hot path": the agent itself decides to remember a fact, usually via a tool call, before it answers. The other route is a background process that writes memory for the agent.

## The three common types

| type | what it holds | example |
| --- | --- | --- |
| semantic | facts | the user wants metric units |
| episodic | past events | the deploy failed last Tuesday on step 3 |
| procedural | how to do a task | run the linter before every commit |

This split comes from the CoALA paper, as summarized by LangChain. A memory application can represent these as text, structured records or links, depending on its retrieval needs.

## Memory is search

Saving a note is simple. The hard part is getting the right note back out when the agent asks a vague question months later. That is a retrieval job, which is why XERJ treats memory as a search feature and not as a separate database.

## How XERJ does it

XERJ is the search engine for AI agents: a new way for AI to search for data, in one local binary. Memory is part of the same API.

- `POST /_memory/{namespace}` saves a fact. The configured embedder runs at write time; the default is lexical feature hashing.
- `POST /_memory/{namespace}/_recall` finds it again. Plain `query` recall is BM25.
- `DELETE /_memory/{namespace}/{id}` forgets one entry.
- Each namespace is its own index. Use separately scoped API keys to restrict agents to their own namespaces; names alone are not an access boundary.
- Over MCP, `xerj mcp` exposes the same calls as `xerj_memory_store` and `xerj_memory_recall`.

The default embedder is lexical feature hashing, so `semantic: true` recall matches shared words, not meaning. Start the node with `--embed-mode neural` for a real neural embedder.

## Limits to know

XERJ is single-node: no replication and no failover. There is no decay and no TTL. A memory is permanent until explicitly forgotten. The only time signal is `stored_at`, which the optional `recency_weight` re-rank uses.

## Try it

Copy the prompt on this page into your coding agent, or follow the [agent memory recipe](/docs/recipes/agentic-memory).
