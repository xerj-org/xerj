---
title: "XERJ vs ReMe: Alibaba's agent memory, tested side by side"
h1: "XERJ vs ReMe (Alibaba Tongyi Lab): a tested comparison"
description: "ReMe is Alibaba's open-source agent memory built on Markdown files. We ran it and XERJ on the same 75 questions. Recall, speed, setup and when to pick each."
slug: "xerj-vs-reme"
cluster: "Comparison: agent memory"
question: "Should I use XERJ or Alibaba's ReMe for AI agent memory?"
intent: "comparison"
published: "2026-10-31"
updated: "2026-10-31"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "Article"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, install XERJ, start a local node with --embed-mode neural, store eight facts about this project with POST /_memory/project, then recall each one with a reworded question using hybrid recall and tell me how many came back in the top 5."
answer: "XERJ or ReMe? In our test both found the right memory in the top 5 for 93.3% of questions with hybrid search on. ReMe's default setup is faster but keyword only, and embeddings need extra config. Pick ReMe for Markdown memory files you can read and edit. Pick XERJ for one binary with semantic recall built in."
key_fact: "ReMe is published by the EconML team of Alibaba Tongyi Lab. Its embeddings are off by default; BM25 search runs with no LLM or API key."
key_fact_source: "https://github.com/agentscope-ai/ReMe"
key_fact_date: "2026-10-31"
intent_group: "agent memory"
queries:
  - "reme alternative"
  - "xerj vs reme"
  - "alibaba reme agent memory"
  - "reme memory benchmark"
  - "agentscope reme alternatives"
commands:
  - cmd: "curl -fsSL https://xerj.org/get | sh"
    note: "Install the single XERJ binary."
  - cmd: "xerj --embed-mode neural"
    note: "Start a local node with the built-in MiniLM embedder."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/_memory/project/_recall' -H 'content-type: application/json' -d '{\"query\":\"which database do we use\",\"hybrid\":true,\"k\":5}'"
    note: "Hybrid recall: BM25 and vectors fused by RRF."
links_out:
  - "/compare/ai-agent-memory-benchmark"
  - "/compare/xerj-vs-mem0"
  - "/compare/xerj-vs-agentmemory"
  - "/compare/xerj-vs-agent-memory-tools"
  - "what-is-hybrid-search"
  - "bm25-vs-vector-search"
evidence:
  - claim: "Top 5 recall on 75 questions: XERJ semantic 94.7%, XERJ hybrid 93.3%, ReMe hybrid 93.3%, XERJ BM25 73.3%, ReMe default BM25 70.7%. Reworded questions top 5: XERJ BM25 24%, ReMe hybrid 80%, XERJ semantic 84%. New value first on changed facts: XERJ 13.3%. With recency weight 0.3, XERJ overall top 5 recall was 68%."
    source: "benchmarks/agent-memory/out/summary.csv"
  - claim: "After the last write, ReMe took about 6 s before the memory was searchable."
    source: "benchmarks/agent-memory/results/reme__bm25_default.json"
faq:
  - q: "Is ReMe from Alibaba?"
    a: "Yes. ReMe lives in the agentscope-ai GitHub organization, and its package lists the EconML team of Alibaba Tongyi Lab as authors. It is Apache 2.0."
  - q: "Does ReMe need an LLM?"
    a: "Not for basic use. Writing files, BM25 search and wikilinks run with no LLM key. Auto Memory, which turns sessions into memory cards, and memory evolution do call an LLM."
  - q: "Which one recalled more in your test?"
    a: "With default settings, XERJ BM25 found 73.3% in the top 5 and ReMe 70.7%. With MiniLM embeddings, both hybrid modes found 93.3%. XERJ semantic recall found 94.7%."
  - q: "Which one is faster?"
    a: "ReMe's default setup: 1.4 ms per query and 2.7 ms per write, against 3.8 ms and 11.3 ms for XERJ BM25. ReMe indexes in the background, so a new memory took about 6 seconds to become searchable. XERJ hybrid queries took 6.5 ms and ReMe hybrid 12.0 ms."
  - q: "How did you turn on ReMe embeddings?"
    a: "The default config points at DashScope's text-embedding-v4. We followed ReMe's README to enable the embedding components and pointed them at a local MiniLM endpoint, so both tools used the same model."
---

**TL;DR** ReMe, from Alibaba's Tongyi Lab, keeps agent memory as Markdown files and indexes them in the background. XERJ is one binary that indexes each memory as it is written. On our 75 question test, both reached 93.3% top 5 recall with hybrid search. XERJ ran in neural mode. ReMe's default is faster and keyword only.

```visual
reme-write-path
```

## Tested side by side

Same 85 memories, same 75 questions, same MiniLM model for embeddings. Top 5 is the share of questions where the right memory came back in the first five.

| | ReMe 0.4.2.1 | XERJ v1.0.0-rc.100 |
| --- | --- | --- |
| Top 5, default settings | 70.7% (BM25) | 73.3% (BM25) |
| Top 5, reworded questions, default | 20% | 24% |
| Top 5, hybrid with MiniLM | 93.3% | 93.3% |
| Top 5, reworded, hybrid | 80% | 84% |
| Top 5, semantic only | Not tested | 94.7% |
| New value first, changed facts | 20% default, 13.3% hybrid | 13.3%; 93.3% with recency weight |
| Median write, default | 2.7 ms, searchable after about 6 s | 11.3 ms, searchable after one refresh call |
| Median query, default | 1.4 ms | 3.8 ms |
| Median query, hybrid | 12.0 ms | 6.5 ms |
| LLM calls per write in this test | 0 | 0 |
| Python packages to install | 138 (reme-ai[as]) | 0, one binary |
| Memory format | Markdown files with wikilinks | Index per namespace, JSON over HTTP |
| License | Apache 2.0 | Apache 2.0 |

## Question by question

Pick a question type to see which memories each setup found. A check means the right memory was in the top 5.

```visual
memory-qx-reme
```

## When to pick ReMe

- You want memory as plain Markdown files your team can read, edit and keep in git.
- You use Claude Code, Hermes Agent, OpenClaw or another agent ReMe has a plugin for.
- You want Auto Memory to turn sessions into memory cards with an LLM.
- Keyword recall is enough, and you want the fastest default.

## When to pick XERJ

- You want semantic or hybrid recall with no extra embedding server or API key.
- You want a memory to be searchable right after the write.
- You want memory in the same engine as your logs, docs and code search.
- You want one binary with an HTTP API and an MCP server.

## Limits on the XERJ side

- In this test XERJ lost on raw speed to ReMe's default BM25.
- The recency weight that fixes changed facts dropped overall top 5 recall to 68%.
- XERJ does not summarize sessions for you. The agent decides what to store.
- XERJ is single-node.

See the full [AI agent memory benchmark](/compare/ai-agent-memory-benchmark) for all seven projects.

## Try it with XERJ

Use the Copy the prompt button and paste it into your coding agent. It installs XERJ, stores eight facts and checks how many reworded questions find them.

## Sources checked

All checked on 2026-10-31.

- ReMe: README and docs/en/memory_search.md in [agentscope-ai/ReMe](https://github.com/agentscope-ai/ReMe), commit 1e51e0dc. Authors from pyproject.toml. License Apache 2.0 from the GitHub API.
- Test runs, configs and per question results: [benchmarks/agent-memory](https://github.com/xerj-org/xerj/tree/main/benchmarks/agent-memory).
