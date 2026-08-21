---
title: "Store user preferences an agent can recall later"
h1: "How do people store user preferences so an agent can recall them next week?"
description: "XERJ keeps agent memory in a reserved index inside the data directory. We stored two memories, restarted the node, recalled both, then forgot one by id."
slug: "coding-agent-memory-across-sessions"
cluster: "Agent memory: setup"
question: "How do people store user preferences so an agent can recall them next week?"
intent: "how-to"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, POST two facts to /_memory/agentmem on a running XERJ node, stop the node, start it again against the same data directory, then POST to /_memory/agentmem/_recall and report both returned ids."
commands:
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9430/_memory/agentmem' -H 'content-type: application/json' -d '{\"text\":\"The user prefers p50 and p95 latency, never the mean.\",\"metadata\":{\"session\":\"A\",\"topic\":\"preferences\"}}'"
    note: "Store one memory. Port 9430 is the capture's Elasticsearch-compatible listener; the default is 9200."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9430/_memory/agentmem/_recall' -H 'content-type: application/json' -d '{\"query\":\"where is the deploy runbook\",\"k\":5}'"
    note: "Recall from a later session. This is BM25 over the memory text by default."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9430/_memory/agentmem'"
    note: "List the namespace and read every stored memory with its id and metadata."
links_out:
  - "agent-memory-without-postgres-qdrant"
  - "set-mcp-memory-storage-path"
  - "private-agent-memory-namespaces"
  - "/docs/recipes/agentic-memory"
faq:
  - q: "How do people store user preferences so an agent can recall them next week?"
    a: "Write each preference with `POST /_memory/{namespace}` and read it back with `POST /_memory/{namespace}/_recall`. The record lives in a reserved index in the data directory and survives a full node restart."
  - q: "How do I make my coding agent remember things across sessions?"
    a: "Store each fact with `POST /_memory/{namespace}` and read it back with `POST /_memory/{namespace}/_recall`. The memory survives a full node restart."
  - q: "What's the simplest local memory for a coding agent?"
    a: "One namespace on a local node. Nothing extra runs beside it: `_memory` writes into `.xerj-memory-{namespace}` in the node data directory, and a plain `query` recall ranks with BM25."
  - q: "Where does XERJ keep agent memory?"
    a: "In a reserved index named `.xerj-memory-{namespace}` inside the node data directory. Our capture found `.xerj-memory-agentmem` at the top level of that directory."
  - q: "Do memories expire?"
    a: "No. Memories are permanent until you explicitly forget them, and there is no TTL, no decay and no importance weighting. The only time signal is `stored_at`."
  - q: "How do I delete one memory?"
    a: "Send `DELETE /_memory/{namespace}/{id}`. Our capture got `{\"forgotten\": true}` and the next recall returned only the remaining memory."
  - q: "Does recall use vectors?"
    a: "Only if you ask. Plain `query` runs BM25, `semantic: true` embeds server-side, and a supplied `vector` runs pure kNN. There is no fusion in `_recall`."
---

**TL;DR** — XERJ stores each memory as an ordinary document in a reserved index named `.xerj-memory-{namespace}`. Our capture stored two memories, stopped the node, started it again against the same data directory, and recalled both by id. One forget call by id removed exactly that memory.

## Store a fact in the first session

An agent stores a memory with one HTTP request to `POST /_memory/{namespace}`. The body needs `text`, and it accepts `metadata`, an explicit `id`, a caller-supplied `vector`, `dedup` and `dedup_threshold`.

XERJ answered our first store call with a new identifier.

```json
{"created":true,"id":"d6ed79bf-7174-4a6d-9f1d-987a53045a11","namespace":"agentmem"}
```

## Recall in a later session

Our capture stopped the node completely, then started it again against the same data directory. `POST /_memory/agentmem/_recall` then returned both memories, ranked.

| id | score | text |
| --- | --- | --- |
| `d6ed79bf-7174-4a6d-9f1d-987a53045a11` | 1.4581499 | the deploy runbook memory |
| `2b00ed19-8741-49ca-8318-54e9ba8c7a58` | 0.27662587 | the latency preference memory |

Durability comes from the storage layer, not from a memory subsystem. Each memory is an ordinary document, and our capture found the backing directory `.xerj-memory-agentmem` at the top level of the node data directory.

## What recall actually runs

Recall picks one mode in strict order and never blends them. A supplied `vector` runs pure kNN, `semantic: true` embeds the query on the server, and a plain `query` runs `{"match":{"text":q}}`, which is BM25.

There is no fusion in `_recall`. To fuse BM25 and kNN, use the `hybrid` query type on an ordinary index instead.

The two modes disagree on our fixture, which is the point of naming them. The plain query put the deploy memory first at 1.4581499. The same question with `semantic: true` put the latency memory first at 0.627885, because the default embedder is lexical feature hashing rather than a neural model.

## A misspelled key fails loudly

Recall uses strict field parsing, so an unknown key is a hard 400 rather than a silent match-all at score 1.0. The strictness matters for an agent that trusts whatever recall hands back.

```text
malformed JSON in request body: unknown field `quety`, expected one of `query`,
`vector`, `semantic`, `k`, `filter`, `recency_weight`, `graph` at line 1 column 8
```

## Deduplicate and forget

A second store call with the identical text and `dedup: true` returned the existing id rather than a second document.

```json
{"created":false,"deduplicated":true,"id":"2b00ed19-8741-49ca-8318-54e9ba8c7a58",
 "namespace":"agentmem","score":1.0}
```

To forget one memory, send `DELETE /_memory/agentmem/{id}`. Our capture forgot `d6ed79bf-7174-4a6d-9f1d-987a53045a11`, and the same recall query afterwards returned only `2b00ed19-8741-49ca-8318-54e9ba8c7a58`.

## Nothing ages out on its own

Memories are permanent until explicitly forgotten. XERJ has no TTL, no decay and no importance weighting, so a stale fact stays until an agent or an operator deletes it.

The only time signal is `stored_at`. An optional `recency_weight` re-ranks on it: our capture passed `recency_weight: 0.5` with the query `latency` and got the newer memory at 0.80259144.

Build a deletion habit into the agent loop. On a single-node XERJ there is no second copy and no scheduled cleanup to save you from an unbounded namespace.

## Which surface an agent uses

Every call on this page is plain HTTP against a running node. The MCP tools `xerj_memory_store` and `xerj_memory_recall` wrap the same routes and ship with the `xerj mcp` subcommand.

No MCP client was connected in this capture. Every call recorded on this page went over plain HTTP against the node.
