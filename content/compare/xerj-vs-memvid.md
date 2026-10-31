---
title: "XERJ vs memvid: single-file agent memory, tested"
h1: "XERJ vs memvid: one memory file or one local server?"
description: "memvid packs agent memory into a single .mv2 file. XERJ runs memory as a local search server. We ran both on the same 75 questions. What we found and when to pick each."
slug: "xerj-vs-memvid"
cluster: "Comparison: agent memory"
question: "Should I use XERJ or memvid for AI agent memory?"
intent: "comparison"
published: "2026-10-31"
updated: "2026-10-31"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "Article"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, install XERJ, start a local node with --embed-mode neural, store five facts about this project with POST /_memory/project, then recall each one twice, once with key words and once as a full question, using hybrid recall, and tell me which recalls missed."
answer: "XERJ or memvid? memvid keeps memory in one portable file with almost no dependencies and answered queries in 0.3 ms. In our test its default lexical search found 1 of 75 answers, because full questions must match every word. XERJ found 73.3% with BM25 and 94.7% with semantic recall."
key_fact: "memvid packages data, embeddings, search structure and metadata into a single .mv2 file. Its lex feature is full-text search with BM25 ranking through Tantivy."
key_fact_source: "https://github.com/memvid/memvid"
key_fact_date: "2026-10-31"
intent_group: "agent memory"
queries:
  - "memvid alternative"
  - "xerj vs memvid"
  - "memvid alternatives"
  - "memvid benchmark"
  - "single file memory for ai agents"
commands:
  - cmd: "curl -fsSL https://xerj.org/get | sh"
    note: "Install the single XERJ binary."
  - cmd: "xerj --embed-mode neural"
    note: "Start a local node with the built-in MiniLM embedder."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/_memory/project/_recall' -H 'content-type: application/json' -d '{\"query\":\"What is the main database for this project?\",\"hybrid\":true,\"k\":5}'"
    note: "A full question works: hybrid recall ranks by any shared word and by meaning."
links_out:
  - "/compare/ai-agent-memory-benchmark"
  - "/compare/xerj-vs-mcp-memory-service"
  - "/compare/xerj-vs-reme"
  - "/compare/xerj-vs-mem0"
  - "bm25-vs-vector-search"
  - "agent-memory-without-postgres-qdrant"
evidence:
  - claim: "Top 5: memvid lexical 1.3%, XERJ BM25 73.3%, XERJ semantic 94.7%. Same-words questions top 5: memvid 4%, XERJ 100%. New value first: memvid 0%, XERJ 13.3%, XERJ with recency weight 93.3%."
    source: "benchmarks/agent-memory/out/summary.csv"
  - claim: "memvid lexical find() returned nothing for full questions and found the memory for the same key words."
    source: "benchmarks/agent-memory/probes/memvid_and_probe.json"
faq:
  - q: "Why did memvid score so low?"
    a: "Our questions are full sentences. memvid's lexical search returned nothing when a question held words that were not in the memory. The same key words with no question words did find the memory. We recorded both in a probe."
  - q: "Is that a fair test of memvid?"
    a: "It is a fair test of its default lexical search on natural questions, the same ones every system got. It is not a test of memvid's vector search, which we could not get working in the PyPI package."
  - q: "Why was vector search not tested?"
    a: "In memvid-sdk 2.0.160 the local embedding option returned error MV015, and a custom embedder stored no vectors, error MV011. So we report only lexical search."
  - q: "Where does memvid win?"
    a: "Queries took 0.3 ms median, the fastest in our test, and the SDK installs only 2 Python packages. The whole memory is one file you can copy."
  - q: "Where does XERJ win?"
    a: "Recall on natural questions, semantic and hybrid search with a built-in model, and writes: 11.3 ms median for XERJ BM25 against 67.3 ms for memvid."
---

**TL;DR** memvid stores agent memory in one portable .mv2 file. XERJ runs memory as a local server you call over HTTP or MCP. memvid's queries were the fastest we measured. But its default search needs every word of a question to match, so natural questions mostly came back empty.

```visual
memvid-one-file
```

## Tested side by side

Same 85 memories, same 75 questions. Top 5 is the share of questions where the right memory came back in the first five.

| | memvid-sdk 2.0.160 | XERJ v1.0.0-rc.100 |
| --- | --- | --- |
| Top 5, default settings | 1.3% (lexical) | 73.3% (BM25) |
| Top 5, questions that reuse the memory's words | 4% | 100% |
| Top 5, semantic with MiniLM | Not tested | 94.7% |
| New value first, changed facts | 0% | 13.3%; 93.3% with recency weight |
| Median write | 67.3 ms | 11.3 ms |
| Median query | 0.3 ms | 3.8 ms |
| LLM calls per write | 0 | 0 |
| What you install | 2 Python packages | One binary |
| Storage | One .mv2 file | Index per namespace on a local node |
| License | Apache 2.0 | Apache 2.0 |

## Why full questions missed

Type a question or pick one. The left side keeps only lines that hold every word. The right side ranks any shared word, like BM25 in XERJ.

```visual
memvid-and-lab
```

If you use memvid, search with key words, not full questions, or strip question words first.

## When to pick memvid

- You want memory as one file you can copy, ship or attach.
- You want no server process at all.
- Your agent searches with key words it knows are in the memory.
- You want the smallest install.

## When to pick XERJ

- Your agent asks in full sentences or in other words.
- You want semantic or hybrid recall with no extra setup.
- You want memory next to your logs, docs and code search.
- Several agents need the same memory over HTTP or MCP.

## Limits on the XERJ side

- XERJ queries were slower than memvid's in this test.
- XERJ is a server, not a file you can pass around.
- XERJ runs on one node with no replication.

See the full [AI agent memory benchmark](/compare/ai-agent-memory-benchmark) for all seven projects.

## Try it with XERJ

Use the Copy the prompt button and paste it into your coding agent. It stores five facts and checks both key word and full question recall.

## Sources checked

All checked on 2026-10-31.

- memvid: README in [memvid/memvid](https://github.com/memvid/memvid), commit e6bd9f7b. memvid-sdk 2.0.160 from PyPI. License Apache 2.0 from the GitHub API.
- Test runs, the all words probe and per question results: [benchmarks/agent-memory](https://github.com/xerj-org/xerj/tree/main/benchmarks/agent-memory).
