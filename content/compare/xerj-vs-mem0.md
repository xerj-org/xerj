---
title: "XERJ vs Mem0: two ways to give an AI agent memory"
h1: "XERJ vs Mem0: which memory fits your AI agent?"
description: "Mem0 uses a language model to turn chats into facts. XERJ stores the text your agent sends and finds it with search. A live recall demo and when to pick each."
slug: "xerj-vs-mem0"
cluster: "Comparison: agent memory"
question: "Should I use XERJ or Mem0 for AI agent memory?"
intent: "comparison"
published: "2026-10-30"
updated: "2026-10-31"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "Article"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, install XERJ, start a local node, store five facts about this project with POST /_memory/project, then recall each one twice, once with words from the fact and once in other words, and tell me which recalls missed and whether hybrid or semantic recall fixed them."
answer: "Should I use XERJ or Mem0 for agent memory? Pick Mem0 if you want a language model to read chats and keep a tidy list of facts for you. Pick XERJ if you want the agent to store exactly what it writes, find it with keyword, vector or hybrid search, and run it all on your own machine."
key_fact: "Mem0's add() runs LLM fact extraction by default (infer=True); its default OpenAI model is gpt-5-mini and its default embedder is text-embedding-3-small."
key_fact_source: "https://github.com/mem0ai/mem0"
key_fact_date: "2026-10-30"
intent_group: "agent memory"
queries:
  - "mem0 alternatives"
  - "mem0 alternative open source"
  - "best memory for AI agents"
  - "xerj vs mem0"
  - "mem0 without openai"
commands:
  - cmd: "curl -fsSL https://xerj.org/get | sh"
    note: "Install the single XERJ binary."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/_memory/project' -H 'content-type: application/json' -d '{\"text\":\"User prefers pnpm over npm\",\"dedup\":true}'"
    note: "Store one memory. With dedup on, near duplicates at 0.95 similarity or above are skipped."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/_memory/project/_recall' -H 'content-type: application/json' -d '{\"query\":\"pnpm\",\"hybrid\":true,\"k\":5}'"
    note: "Recall with BM25 and vectors fused by RRF."
links_out:
  - "/compare/ai-agent-memory-benchmark"
  - "/compare/xerj-vs-agent-memory-tools"
  - "/compare/xerj-vs-claude-mem-and-mempalace"
  - "coding-agent-memory-across-sessions"
  - "what-is-an-mcp-memory-server"
  - "what-is-agentic-memory"
  - "what-is-hybrid-search"
evidence:
  - claim: "Top 5: XERJ semantic 94.7%, XERJ hybrid 93.3%, mem0 infer=False 88.0%, mem0 infer=True 86.7%. First result: 65.3%, 60.0%, 64.0%, 64.0%. Reworded: 84.0%, 84.0%, 68.0%, 72.0%. Changed fact first: 13.3%, 13.3%, 13.3%, 20.0%. XERJ recency 0.3: 93.3% changed facts, 68.0% top 5. Write and query medians 7.17 ms, 6.45 ms, 10.22 ms, 16.5 s; 6.29 ms, 6.49 ms, 22.44 ms, 29.15 ms."
    source: "benchmarks/agent-memory/out/summary.csv"
  - claim: "mem0 infer=True made 1 LLM call per write with Qwen2.5 3B."
    source: "benchmarks/agent-memory/results/mem0__infer.json"
faq:
  - q: "What is the main difference between XERJ and Mem0?"
    a: "Mem0 calls a language model on write to extract facts and update old ones. XERJ makes no model call on write. It stores the text the agent sends and finds it later with search."
  - q: "Does Mem0 need an OpenAI key?"
    a: "Its default setup uses OpenAI: gpt-5-mini for extraction and text-embedding-3-small for embeddings. You can configure other providers, and infer=False stores raw messages with no extraction step."
  - q: "Is XERJ a drop-in replacement for Mem0?"
    a: "No. The APIs differ, and XERJ does not extract or merge facts for you. If your agent relies on Mem0 updating old facts, you would write that logic yourself."
  - q: "Which one is better for a coding agent on my laptop?"
    a: "If you want no cloud service and no model on the write path, XERJ fits. If you want automatic fact cleanup and are fine with a model call per write, Mem0 fits."
  - q: "Does XERJ separate memories per user?"
    a: "Each namespace is its own index, so recall in one namespace never sees another. There is no per-namespace password, so do not use namespaces as a security boundary."
  - q: "Was a benchmark run between the two?"
    a: "Yes. On the same 85 memories and 75 questions, XERJ semantic recall in neural mode found the right memory in the top 5 for 94.7% of questions. mem0 found 88.0% with infer=False and 86.7% with its LLM. mem0 beat XERJ hybrid on the first result."
---

**TL;DR** Mem0 and XERJ both give an agent memory that lasts across sessions. Mem0 asks a language model to turn chats into facts and keeps them up to date. XERJ stores what the agent writes, word for word, and finds it with keyword, vector or hybrid search on your machine.

```visual
memory-write-paths
```

## Side by side

Checked on 2026-10-30 against the Mem0 repository and the XERJ source.

| | Mem0 (open source) | XERJ |
| --- | --- | --- |
| What gets stored | Facts a model extracts, by default | The exact text the agent sends |
| Model call on write | Yes by default; off with infer=False | Never |
| Default models | gpt-5-mini, text-embedding-3-small (OpenAI) | None needed; optional local MiniLM |
| Updates old facts | Yes, the model can update or delete | No; you overwrite by id or forget |
| Search on recall | Vectors, plus BM25 and an entity boost when the store supports full-text search (Qdrant does) | BM25 by default; semantic or hybrid on request |
| Storage | Vector store, Qdrant by default | One local index per namespace |
| Scoping | user_id, agent_id, run_id | Namespace, plus metadata filters |
| Ways to run | Python library, self-hosted server, cloud platform | One binary, HTTP API and MCP server |
| License | Apache 2.0 | Apache 2.0 |

## Tested on the same data

We ran both on the same 85 memories and 75 questions, with the same local MiniLM embedder. XERJ is lexical by default, so for these rows it ran in neural mode. The [AI agent memory benchmark](/compare/ai-agent-memory-benchmark) has the method and the raw data.

| | mem0 infer=False | mem0 infer=True | XERJ semantic | XERJ hybrid |
| --- | --- | --- | --- | --- |
| Right memory in top 5 | 88.0% | 86.7% | 94.7% | 93.3% |
| Right memory first | 64.0% | 64.0% | 65.3% | 60.0% |
| Reworded questions, top 5 | 68.0% | 72.0% | 84.0% | 84.0% |
| Changed fact, new value first | 13.3% | 20.0% | 13.3% | 13.3% |
| Write, median | 10.22 ms | 16.5 s | 7.17 ms | 6.45 ms |
| Query, median | 22.44 ms | 29.15 ms | 6.29 ms | 6.49 ms |
| LLM calls per write | 0 | 1 | 0 | 0 |

- mem0 beat XERJ hybrid on the first result, 64.0% vs 60.0%.
- With its LLM on, mem0 put the new value first for 3 of 15 changed facts. XERJ with a recency weight of 0.3 did it for 14 of 15, but its top 5 score fell to 68.0%.
- mem0 infer=True used Qwen2.5 3B on CPU. A larger model may extract better facts. We did not test one.

## Try the XERJ side

This is XERJ's default recall: plain BM25 over three memories. Pick a recall and see what comes back.

```visual
memory-recall-lab
```

The empty results are the honest part. A word match cannot know that "package manager" means pnpm. You have three fixes:

- Write memories with the words you will search for later, such as "package manager: pnpm".
- Turn on `semantic` or `hybrid` recall with the neural embedder, which compares meaning.
- Use a tool like Mem0 whose model rewrites memories into cleaner facts at write time.

## When to pick Mem0

- You want the memory layer to decide what is worth keeping.
- Facts change, and you want old ones updated or removed for you.
- You are happy to call a language model on every write, or to use the hosted platform.
- You work in Python and want a library inside your app.

## When to pick XERJ

- You want to know exactly what was stored, with no model rewriting it.
- You want no model call and no cloud service on the write path.
- You want memory in the same engine as the rest of your agent's search: logs, docs, code and past sessions.
- Your agent speaks MCP. XERJ ships an MCP server with `xerj_memory_store` and `xerj_memory_recall`.

## Limits to know on the XERJ side

- XERJ runs on one node. There is no replication or failover.
- Namespaces isolate by name, not by credential.
- Nothing ages out on its own. Your agent forgets with `DELETE /_memory/{ns}/{id}`.

The [memory across sessions](/answers/coding-agent-memory-across-sessions) page shows a store, restart and recall run. The [agent memory tools comparison](/compare/xerj-vs-agent-memory-tools) also covers Zep, Letta and Cognee.

## How to try it with XERJ

Use the Copy the prompt button and paste it into your coding agent. It installs XERJ, stores five facts and tests which recalls miss. Or run the three commands above by hand.

## Sources checked

All checked on 2026-10-30.

- Mem0: `mem0/memory/main.py` (infer defaults to True; BM25 scoring when the vector store supports it), `mem0/llms/openai.py` (gpt-5-mini), `mem0/embeddings/openai.py` (text-embedding-3-small) and `mem0/vector_stores/configs.py` (Qdrant) in [mem0ai/mem0](https://github.com/mem0ai/mem0). License Apache 2.0 from the repository.
- XERJ memory API: `engine/crates/xerj-api/src/memory_api.rs`. MCP tools: `engine/crates/xerj-mcp/src/lib.rs`.
- Recall results: a local XERJ v1.0.0-rc.97 node with the three memories shown.
- Test numbers: [benchmarks/agent-memory](https://github.com/xerj-org/xerj/tree/main/benchmarks/agent-memory), run on 2026-10-31 with mem0ai 2.2.1 and XERJ v1.0.0-rc.100.
