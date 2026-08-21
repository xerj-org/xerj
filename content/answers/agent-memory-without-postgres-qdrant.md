---
title: "Agent memory on a laptop, no extra database"
h1: "What's a simple way to give an agent long-term memory on my laptop without Qdrant?"
description: "Our capture ran agent memory on one XERJ process with three loopback sockets, no second data service, and a peak resident size of 100,928 kB."
slug: "agent-memory-without-postgres-qdrant"
cluster: "Agent memory: topology"
question: "I want persistent agent memory without standing up Postgres or Qdrant. What do people use?"
intent: "tool-selection"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start one XERJ node on this laptop, POST a durable fact to /_memory/agentmem, read it back with POST /_memory/agentmem/_recall, then run ss -ltnp and ps to list every listening socket and every process the node owns and report whether postgres, qdrant, redis, weaviate, milvus or chroma is running anywhere on the host."
commands:
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9430/_memory/agentmem' -H 'content-type: application/json' -d '{\"text\":\"The user prefers p50 and p95 latency, never the mean.\",\"metadata\":{\"session\":\"A\"}}'"
    note: "Store one memory against the running node. No other service takes part."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9430/_cat/indices?format=json&bytes=b'"
    note: "List the indices, including the reserved memory backing index."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9430/_memory/agentmem'"
    note: "Read the namespace back to confirm the memory landed."
links_out:
  - "coding-agent-memory-across-sessions"
  - "store-agent-memory-without-llm-call"
  - "xerj-vs-vector-database"
faq:
  - q: "I want persistent agent memory without standing up Postgres or Qdrant. What do people use?"
    a: "The process that already holds the index. Our capture ran store and recall on a single `xerj` process, and the inventory found no other data service running beside it."
  - q: "What's a simple way to give an agent long-term memory on my laptop?"
    a: "Store facts with `POST /_memory/{namespace}` on a local node and read them back with `_recall`. Peak resident size across the whole memory run was 100,928 kB."
  - q: "What's the simplest local memory for a coding agent?"
    a: "One namespace on one local node. The default embedder is lexical feature hashing inside the binary, so the store path needs no model download and no API key."
  - q: "Which ports does the node open?"
    a: "Three, all bound to 127.0.0.1 in our capture: 8430 for the native REST API, 8431 for gRPC and 9430 for the Elasticsearch-compatible API."
  - q: "Where is the memory actually stored?"
    a: "In a reserved index named `.xerj-memory-{namespace}` under the node data directory. Each memory is an ordinary XERJ document."
  - q: "What do I give up by not running a vector database?"
    a: "Multi-node serving. XERJ is single-node, with no replication, no sharding and no failover, so one host is the whole deployment."
  - q: "Does the memory path need a model or an API key?"
    a: "No. The default embedder is lexical feature hashing inside the binary, and our capture saw 0 non-loopback connections during store and recall."
---

**TL;DR** — Use the node that already holds your index. Agent memory is an endpoint on it: store a fact with `POST /_memory/{namespace}` and read it back with `_recall`. Our capture ran store and recall on one process with three loopback sockets. Peak resident size was 100,928 kB, and no second data service was running.

## What ran during the memory work

One process served every memory call. The inventory taken during the run lists a single `xerj` process, then states plainly what else was absent.

```text
2580800 2580463 78276 xerj  .../engine/target/release/xerj --insecure -c .../run-c.toml
--- any other xerj-ish process:
none of postgres/qdrant/redis/weaviate/milvus/chroma/elasticsearch/opensearch is running
```

## Which sockets the node opened

The same node owned three listening sockets and every one was bound to loopback. XERJ binds to `127.0.0.1` by default, and exposing a node with TLS off additionally needs `server.allow_insecure_network_bind = true`.

| port | surface |
| --- | --- |
| 8430 | native REST API |
| 8431 | gRPC |
| 9430 | Elasticsearch-compatible API |

Those port numbers are the isolated ones this capture used. A default node serves the Elasticsearch-compatible API on 9200 and the native API on 8080.

## Why no extra service is needed

A memory is an ordinary XERJ document in a reserved index named `.xerj-memory-{namespace}`. The backing mapping puts `text` into a `semantic_text` field that is both BM25-indexed and embedded, `stored_at` into an epoch-millisecond long, and `metadata` into dynamic mapping.

A caller-supplied `vector` becomes a `dense_vector` field on first store. Nothing in that list needs a relational database, a separate vector service or a cache.

Our capture found the backing directory `.xerj-memory-agentmem` at the top level of the node data directory, beside `audit.jsonl` and `node.lock`.

## What the run cost

Peak resident size for the node across the entire memory run was 100,928 kB. That figure is `VmHWM` read from the kernel, so it is a high-water mark for the whole run rather than a steady state.

The run manifest names everything the number depends on. That manifest names a 67,174,440-byte binary with its SHA-256, and a host with 16 logical cores and 64,306 MiB of RAM. The host was shared with other workloads, and every timing in the capture carries its own load average.

## What you give up

XERJ is single-node. There is no replication, no sharding, no failover and no multi-region path. The memory layer lives or dies with one host and one data directory.

Two more boundaries belong in the same decision. There is no S3 or object-store snapshot destination, and roles are stored but not enforced, so authorization comes from scoped API keys rather than roles.

Back the data directory up yourself, and treat a second host as a restore target, not a replica.

## What this capture does not show

This pass ran a small memory workload on one node for about 5 seconds, so the resident-size figure sizes a laptop deployment and nothing larger. The node ran with `--insecure`, so no API-key path was exercised. No comparison against another product was measured in this run.
