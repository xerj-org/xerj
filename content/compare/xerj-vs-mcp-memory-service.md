---
title: "XERJ vs mcp-memory-service: tested MCP memory servers"
h1: "XERJ vs mcp-memory-service: two MCP memory servers, tested"
description: "mcp-memory-service and XERJ both give agents memory over MCP and HTTP. We ran them on the same 75 questions. Recall, updated facts, speed and when to pick each."
slug: "xerj-vs-mcp-memory-service"
cluster: "Comparison: agent memory"
question: "Should I use XERJ or mcp-memory-service as an MCP memory server?"
intent: "comparison"
published: "2026-10-31"
updated: "2026-10-31"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "Article"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, install XERJ, start a local node with --embed-mode neural, store a fact about this project with POST /_memory/project, store a changed version of the same fact, then recall it with hybrid recall and recency_weight 0.3 and tell me whether the new value came first."
answer: "XERJ or mcp-memory-service? With MiniLM, both found 84% of reworded questions. mcp-memory-service found 89.3% of all answers in the top 5 and XERJ semantic recall 94.7%. At default settings mcp-memory-service rejected 4 of 15 updated facts as duplicates. XERJ stores every write unless you ask for dedup."
key_fact: "mcp-memory-service turns semantic dedup on by default: a new memory 0.85 similar or more to one from the last 24 hours is rejected."
key_fact_source: "https://github.com/doobidoo/mcp-memory-service/blob/main/src/mcp_memory_service/storage/mixins/base.py"
key_fact_date: "2026-10-31"
intent_group: "agent memory"
queries:
  - "mcp-memory-service alternative"
  - "xerj vs mcp-memory-service"
  - "mcp memory service alternatives"
  - "mcp memory server benchmark"
  - "doobidoo mcp memory service"
commands:
  - cmd: "curl -fsSL https://xerj.org/get | sh"
    note: "Install the single XERJ binary."
  - cmd: "xerj --embed-mode neural"
    note: "Start a local node with the built-in MiniLM embedder."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/_memory/project/_recall' -H 'content-type: application/json' -d '{\"query\":\"API port in production\",\"hybrid\":true,\"recency_weight\":0.3,\"k\":5}'"
    note: "Hybrid recall that ranks newer memories higher."
links_out:
  - "/compare/ai-agent-memory-benchmark"
  - "/compare/xerj-vs-agentmemory"
  - "/compare/xerj-vs-claude-mem-and-mempalace"
  - "/compare/xerj-vs-memvid"
  - "what-is-an-mcp-memory-server"
  - "what-is-hybrid-search"
evidence:
  - claim: "Top 5: mcp-memory-service 89.3%, XERJ semantic 94.7%, XERJ hybrid 93.3%. Top 1: mcp-memory-service 64.0%, XERJ semantic 65.3%, XERJ hybrid 60.0%. Reworded top 5: both 84%. New value first: mcp-memory-service 6.7%, XERJ 13.3%, XERJ with recency weight 93.3%."
    source: "benchmarks/agent-memory/out/summary.csv"
faq:
  - q: "What is mcp-memory-service?"
    a: "An Apache 2.0 memory backend for AI agents with a REST API, MCP, a CLI and a dashboard. It stores memories in SQLite with local ONNX embeddings by default, and can sync to Cloudflare or another instance."
  - q: "Which one recalled more in your test?"
    a: "On top 5, XERJ semantic found 94.7% and mcp-memory-service 89.3%. On the first result, XERJ semantic was 65.3% and mcp-memory-service 64.0%. Both found 84% of reworded questions."
  - q: "Why did mcp-memory-service miss changed facts?"
    a: "Its semantic dedup rejected 4 of our 15 updates, such as a new API port, because they were too close to the old note. Only the old value could come back. You can turn dedup off or pass a conversation_id to skip it."
  - q: "Which one is faster?"
    a: "XERJ semantic queries took 6.3 ms median and writes 7.2 ms. mcp-memory-service took 16.7 ms for both. It ran in process and XERJ over HTTP."
  - q: "Do I need Python for mcp-memory-service?"
    a: "Yes. It is a Python package. With the SQLite extra it resolved to 72 packages on Python 3.12. XERJ is one binary."
---

**TL;DR** mcp-memory-service and XERJ both serve agent memory over MCP and HTTP. Both can use a local MiniLM model; XERJ needs neural mode for it. Recall was close: 89.3% top 5 for mcp-memory-service, 94.7% for XERJ semantic. The big difference was changed facts: mcp-memory-service's default dedup threw away 4 of 15 updates.

```visual
mms-dedup-gate
```

## Tested side by side

Same 85 memories, same 75 questions, same MiniLM model. Top 5 is the share of questions where the right memory came back in the first five.

| | mcp-memory-service 12.4.0 | XERJ v1.0.0-rc.100 |
| --- | --- | --- |
| Top 5, all questions | 89.3% | 94.7% semantic, 93.3% hybrid |
| Top 1, all questions | 64.0% | 65.3% semantic, 60.0% hybrid |
| Top 5, reworded questions | 84% | 84% |
| New value first, changed facts | 6.7% | 13.3%; 93.3% with recency weight |
| Updates rejected at write | 4 of 15 | 0 |
| Median write | 16.7 ms | 7.2 ms |
| Median query | 16.7 ms | 6.3 ms |
| LLM calls per write | 0 | 0 |
| What you install | 72 Python packages | One binary |
| Storage | SQLite with sqlite-vec, optional cloud sync | Index per namespace |
| License | Apache 2.0 | Apache 2.0 |

## Question by question

This starts on the changed facts. A check means the right memory was in the top 5.

```visual
memory-qx-mms
```

## When to pick mcp-memory-service

- You want sync across machines through Cloudflare or another copy of the service.
- You want a dashboard, OAuth and scheduled consolidation included.
- You store lots of near duplicate notes and want them filtered for you.
- You are happy to run a Python service.

## When to pick XERJ

- Facts in your project change, and you want the new value kept and ranked first.
- You want one binary with no Python environment.
- You want memory in the same engine as your logs, docs and code search.
- You want BM25, semantic and hybrid recall from one API.

## Limits on the XERJ side

- mcp-memory-service beat XERJ hybrid on the first result, 64.0% vs 60.0%.
- XERJ is single-node, with no built-in cloud sync or replication.
- The recency weight that ranks new values first lowers overall recall, so use it for facts that change.

See the full [AI agent memory benchmark](/compare/ai-agent-memory-benchmark) for all seven projects.

## Try it with XERJ

Use the Copy the prompt button and paste it into your coding agent. It stores a fact, changes it and checks whether XERJ returns the new value first.

## Sources checked

All checked on 2026-10-31.

- mcp-memory-service: README and src/mcp_memory_service/storage/mixins/base.py in [doobidoo/mcp-memory-service](https://github.com/doobidoo/mcp-memory-service), commit 0071ab2a, PyPI release 12.4.0. License Apache 2.0 from the GitHub API.
- Test runs and per question results: [benchmarks/agent-memory](https://github.com/xerj-org/xerj/tree/main/benchmarks/agent-memory).
