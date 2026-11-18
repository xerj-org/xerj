---
title: "XERJ vs agentmemory: tested memory for coding agents"
h1: "XERJ vs agentmemory: which memory for your coding agent?"
description: "agentmemory captures coding sessions through hooks. XERJ stores what the agent chooses to save. We ran both on the same 75 questions. Results and when to pick each."
slug: "xerj-vs-agentmemory"
cluster: "Comparison: agent memory"
question: "Should I use XERJ or agentmemory for coding agent memory?"
intent: "comparison"
published: "2026-10-31"
updated: "2026-10-31"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "Article"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, install XERJ, start a local node with --embed-mode neural, store the five most useful facts about this repository with POST /_memory/project, then recall each one with a reworded question using semantic recall and report which ones came back in the top 5."
answer: "XERJ or agentmemory? In keyword mode they tied closely: agentmemory found 74.7% of answers in the top 5 and XERJ 73.3%. XERJ semantic recall, with a model built in, found 94.7%. Pick agentmemory to capture sessions through hooks. Pick XERJ to store what the agent chooses and search it by meaning."
key_fact: "agentmemory's keyless default disables vector embeddings, so memory_recall uses BM25. Setting EMBEDDING_PROVIDER=local adds on-device semantic recall."
key_fact_source: "https://github.com/rohitg00/agentmemory"
key_fact_date: "2026-10-31"
intent_group: "agent memory"
queries:
  - "agentmemory alternative"
  - "xerj vs agentmemory"
  - "agentmemory alternatives"
  - "agentmemory benchmark"
  - "memory for coding agents open source"
commands:
  - cmd: "curl -fsSL https://xerj.org/get | sh"
    note: "Install the single XERJ binary."
  - cmd: "xerj --embed-mode neural"
    note: "Start a local node with the built-in MiniLM embedder."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/_memory/project/_recall' -H 'content-type: application/json' -d '{\"query\":\"how do we deploy\",\"semantic\":true,\"k\":5}'"
    note: "Semantic recall: matches meaning, not only shared words."
links_out:
  - "/compare/ai-agent-memory-benchmark"
  - "/compare/xerj-vs-claude-mem-and-mempalace"
  - "/compare/xerj-vs-mcp-memory-service"
  - "/compare/xerj-vs-reme"
  - "coding-agent-memory-across-sessions"
  - "what-is-an-mcp-memory-server"
evidence:
  - claim: "Top 5: agentmemory keyless BM25 74.7%, XERJ BM25 73.3%, XERJ semantic 94.7%. Top 1: agentmemory 53.3%, XERJ BM25 54.7%. Reworded top 5: agentmemory 28%, XERJ BM25 24%. New value first: both 13.3%; XERJ with recency weight 93.3%."
    source: "benchmarks/agent-memory/out/summary.csv"
faq:
  - q: "What is agentmemory?"
    a: "An Apache 2.0 memory server for coding agents such as Claude Code, Codex CLI and Cursor. Hooks and plugins record session events, and agents recall them later over MCP or a REST API."
  - q: "Which one recalled more in your test?"
    a: "In keyword mode agentmemory was slightly ahead on top 5, 74.7% vs 73.3%, and XERJ slightly ahead on the first result, 54.7% vs 53.3%. With its MiniLM embedder on, XERJ semantic recall found 94.7%."
  - q: "Did you test agentmemory with embeddings?"
    a: "No. We tested its keyless default, which is what a new install runs. Its optional local embeddings use the same MiniLM model family, so its semantic score could be close to the other MiniLM systems. We did not measure it."
  - q: "Which one is faster?"
    a: "Writes: agentmemory 4.4 ms median, XERJ 11.3 ms. Queries: XERJ 3.8 ms, agentmemory 4.8 ms. Both ran over HTTP on the same shared machine."
  - q: "What do I install for each?"
    a: "agentmemory resolved to 143 npm packages and downloads the iii-engine runtime binary. XERJ is one binary."
---

**TL;DR** agentmemory captures what happens in your coding sessions through hooks and finds it later with BM25. XERJ stores what your agent decides to save. It is lexical by default; neural is opt-in. On our 75 question test the keyword modes were close, and XERJ semantic recall found 94.7%.

```visual
agentmemory-paths
```

## Tested side by side

Same 85 memories and 75 questions, one HTTP write per memory, no hooks. Top 5 is the share of questions where the right memory came back in the first five.

| | agentmemory 0.9.31 | XERJ v1.0.0-rc.100 |
| --- | --- | --- |
| Top 5, default settings | 74.7% (BM25, keyless) | 73.3% (BM25) |
| Top 1, default settings | 53.3% | 54.7% |
| Top 5, reworded questions, default | 28% | 24% |
| Top 5, semantic with MiniLM | Not tested | 94.7% |
| New value first, changed facts | 13.3% | 13.3%; 93.3% with recency weight |
| Median write | 4.4 ms | 11.3 ms |
| Median query | 4.8 ms | 3.8 ms |
| LLM calls per write in this test | 0 | 0 |
| What you install | 143 npm packages plus the iii-engine binary | One binary |
| How memories get in | Hooks and plugins, or REST and MCP | REST and MCP |
| License | Apache 2.0 | Apache 2.0 |

## Question by question

Pick a question type. A check means the right memory was in the top 5.

```visual
memory-qx-agentmemory
```

## When to pick agentmemory

- You want sessions captured for you, with no extra step in your prompts.
- You switch between several coding agents and want one memory server for all of them.
- You want a live viewer to inspect what was saved.
- Your recalls reuse the words in your notes, so BM25 is enough.

## When to pick XERJ

- You want semantic recall that works with no setup, using a built-in model.
- You want the agent to choose what is worth keeping.
- You want one binary with no Node runtime.
- You want memory in the same engine as your logs, docs and code search.

## Limits on the XERJ side

- agentmemory wrote faster and edged XERJ on keyword top 5 in this test.
- XERJ does not capture sessions by itself. Your agent has to call the store tool.
- XERJ runs on one node with no replication.

See the full [AI agent memory benchmark](/compare/ai-agent-memory-benchmark) for all seven projects.

## Try it with XERJ

Use the Copy the prompt button and paste it into your coding agent. It installs XERJ, stores five facts about your repository and checks which reworded questions find them.

## Sources checked

All checked on 2026-10-31.

- agentmemory: README in [rohitg00/agentmemory](https://github.com/rohitg00/agentmemory), commit c6c476d2, npm release 0.9.31. License Apache 2.0 from the GitHub API.
- Test runs and per question results: [benchmarks/agent-memory](https://github.com/xerj-org/xerj/tree/main/benchmarks/agent-memory).
