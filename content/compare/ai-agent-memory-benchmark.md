---
title: "AI agent memory benchmark: 7 open-source projects tested"
h1: "AI agent memory compared: real tests on 7 open-source projects"
description: "We ran mem0, MemPalace, Alibaba's ReMe, agentmemory, mcp-memory-service, memvid and XERJ on the same 85 memories and 75 questions. Recall, updates, speed and setup."
slug: "ai-agent-memory-benchmark"
cluster: "Comparison: agent memory"
question: "Which open-source agent memory finds the right memory most often?"
intent: "comparison"
published: "2026-10-31"
updated: "2026-10-31"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "Article"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, install XERJ, start a local node with --embed-mode neural, store ten facts about this project with POST /_memory/project, change two of them, then recall each fact with BM25, semantic and hybrid recall and tell me which mode found the most and whether the new value of each changed fact came first."
answer: "Which open-source agent memory recalls best? With the same MiniLM model, XERJ semantic recall found the right memory in the top 5 for 94.7% of 75 questions, ReMe and XERJ hybrid 93.3%, mcp-memory-service 89.3% and mem0 88.0%. Keyword only setups scored 71% to 75%. Only XERJ with a recency weight ranked changed facts correctly."
key_fact: "Same data for every system: 85 memories and 75 questions, scored by one script. Embedding systems all used all-MiniLM-L6-v2 on CPU."
key_fact_source: "https://github.com/xerj-org/xerj/tree/main/benchmarks/agent-memory"
key_fact_date: "2026-10-31"
intent_group: "agent memory"
queries:
  - "ai agent memory benchmark"
  - "open source agent memory"
  - "agent memory comparison"
  - "agent memory benchmark open source"
  - "llm memory benchmark recall"
commands:
  - cmd: "curl -fsSL https://xerj.org/get | sh"
    note: "Install the single XERJ binary."
  - cmd: "xerj --embed-mode neural"
    note: "Start a local node with the built-in MiniLM embedder, needed for semantic and hybrid recall."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/_memory/project/_recall' -H 'content-type: application/json' -d '{\"query\":\"which port does the API use\",\"hybrid\":true,\"recency_weight\":0.3,\"k\":5}'"
    note: "Hybrid recall that favors newer memories, the setting that won the update test."
links_out:
  - "/compare/xerj-vs-reme"
  - "/compare/xerj-vs-mem0"
  - "/compare/xerj-vs-claude-mem-and-mempalace"
  - "/compare/xerj-vs-agentmemory"
  - "/compare/xerj-vs-mcp-memory-service"
  - "/compare/xerj-vs-memvid"
  - "/compare/xerj-vs-agent-memory-tools"
  - "what-is-hybrid-search"
  - "bm25-vs-vector-search"
evidence:
  - claim: "Top 1 % and top 5 % per system: XERJ semantic 65.3% and 94.7%, XERJ hybrid 60.0% and 93.3%, XERJ BM25 54.7% and 73.3%, XERJ with recency 0.3 68.0% top 5, mem0 raw 64.0% and 88.0%, mem0 infer 64.0% and 86.7%, ReMe hybrid 93.3%, mcp-memory-service 64.0% and 89.3%, agentmemory 74.7%. Embedding setups 88% to 95% top 5, keyword setups 71% to 75%; reworded questions with BM25 20% to 28%."
    source: "benchmarks/agent-memory/out/summary.csv"
  - claim: "mem0 infer=True: 1 LLM call per write, median write 16.5 s, 2 of 85 extraction responses failed to parse."
    source: "benchmarks/agent-memory/results/mem0__infer.json"
  - claim: "Test machine: shared 8 core Xeon with 15 GB of RAM, no GPU."
    source: "benchmarks/agent-memory/README.md"
faq:
  - q: "Which agent memory scored best?"
    a: "On finding the right memory in the top 5, XERJ semantic recall led with 94.7%, then XERJ hybrid and ReMe hybrid at 93.3%. The gap between the top systems is small. The bigger gap is between keyword only search and search with embeddings."
  - q: "Why do most systems fail the update test?"
    a: "When a fact changes, the old and new notes look almost the same to search. Without a sense of time, the old one ranks first about as often as the new one. Only XERJ with a recency weight put the new value first in 14 of 15 cases, and that setting cost accuracy elsewhere."
  - q: "Did you test mem0 with its LLM?"
    a: "Yes. With infer=True and Qwen2.5 3B on llama.cpp, mem0 made 1 LLM call per write. The median write took 16.5 s on our CPU machine. It found 86.7% in the top 5, close to the 88.0% of raw mode."
  - q: "Why were Letta, Zep, cognee and others not tested?"
    a: "They need an LLM call on each write, extra services such as a graph database, or a hosted key. We could not run those fairly on the shared CPU machine used for this round. Each one is listed below with the reason."
  - q: "Can I run the benchmark myself?"
    a: "Yes. The data set, one adapter per system, the score script and our raw results are in benchmarks/agent-memory in the XERJ repository."
  - q: "Is the latency comparable to production numbers?"
    a: "Treat it as a rough guide. All systems ran on one shared 8 core machine, one at a time, with no GPU. Some run in process and some over HTTP, which adds a little time."
---

**TL;DR** We ran seven open-source agent memory projects on the same 85 memories and 75 questions. With neural embeddings on, XERJ, ReMe, mcp-memory-service and mem0 all found the right memory in the top 5 for 88% to 95% of questions. Keyword only setups found 71% to 75%. Changed facts were the hard part: only XERJ with a recency weight reliably ranked the new value first.

```visual
memory-bench-method
```

## Results

Top 5 means the right memory was in the first five results. Reworded means the question used different words from the memory. Newest first counts how often the new value of a changed fact ranked above the old one. Times are the median, in milliseconds.

```visual
memory-bench-bars
```

Sort or filter the full table. The keyword and embedding filters split the setups by how they search.

```visual
memory-bench-table
```

The same table as plain text:

| System | Setup | Top 1 % | Top 5 % | Reworded % | Newest first % | Write ms | Query ms | LLM calls per write |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| XERJ | BM25, default recall | 54.7 | 73.3 | 24.0 | 13.3 | 11.31 | 3.77 | 0 |
| XERJ | semantic, MiniLM | 65.3 | 94.7 | 84.0 | 13.3 | 7.17 | 6.29 | 0 |
| XERJ | hybrid, MiniLM | 60.0 | 93.3 | 84.0 | 13.3 | 6.45 | 6.49 | 0 |
| XERJ | hybrid + recency 0.3 | 44.0 | 68.0 | 36.0 | 93.3 | 6.46 | 6.78 | 0 |
| mem0 | infer=False, MiniLM | 64.0 | 88.0 | 68.0 | 13.3 | 10.22 | 22.44 | 0 |
| mem0 | infer=True, Qwen2.5 3B | 64.0 | 86.7 | 72.0 | 20.0 | 16522.58 | 29.15 | 1 |
| MemPalace | default, MiniLM | 57.3 | 84.0 | 56.0 | 13.3 | 21.21 | 22.66 | 0 |
| ReMe | default, BM25 | 53.3 | 70.7 | 20.0 | 20.0 | 2.68 | 1.41 | 0 |
| ReMe | hybrid, MiniLM | 61.3 | 93.3 | 80.0 | 13.3 | 2.62 | 11.99 | 0 |
| agentmemory | default, no key | 53.3 | 74.7 | 28.0 | 13.3 | 4.39 | 4.75 | 0 |
| mcp-memory-service | default, MiniLM | 64.0 | 89.3 | 84.0 | 6.7 | 16.69 | 16.74 | 0 |
| memvid | default, lexical | 1.3 | 1.3 | 0.0 | 0.0 | 67.33 | 0.31 | 0 |

## What the numbers say

- Embeddings matter more than the brand. The five best top 5 scores all use the same MiniLM model, and they land within 7 points of each other.
- Full-text search is fine when the question reuses the memory's words. Every system except memvid found all 25 questions that reuse the memory's words. Reworded questions are where BM25 falls to 20% to 28%.
- Changed facts break almost everyone. Most systems put the new value first in 0 to 3 of 15 cases. mcp-memory-service rejected 4 of the updates as duplicates at its default settings.
- mem0 with its LLM on (infer=True) found 86.7% in the top 5 and put the new value first for 3 of 15 changed facts. Each write made 1 LLM call, with a median of 16.5 s. The model's output failed to parse on 2 of 85 writes.
- XERJ's recency weight fixes updates (14 of 15) but costs overall recall (68.0% top 5). Use it for state that changes, not for every recall.

## Where XERJ loses

- ReMe's default setup is faster: 1.4 ms per query and 2.7 ms per write, against 3.8 ms and 11.3 ms for XERJ BM25. ReMe indexes in the background, so memories took 6 seconds to become searchable.
- agentmemory's keyword mode beat XERJ's keyword mode on top 5 (74.7% vs 73.3%).
- mem0 raw (64.0%) and mcp-memory-service (64.0%) beat XERJ hybrid (60.0%) on the first result. XERJ semantic was 65.3%.
- memvid answered queries fastest, at 0.3 ms, and installs only 2 packages.

## How we tested

- Data: 85 short memories about one user and one project, written over 6 sessions. 15 later memories change an earlier fact, such as a new port number.
- Questions: 75. 25 reuse the memory's words, 25 are reworded, 15 ask about a changed fact and 10 are near misses next to a similar memory.
- Each system ran with its documented defaults through its own Python, HTTP or SDK API. We changed a setting only to point embeddings at the shared local model, and we say so in the table.
- Embeddings: all-MiniLM-L6-v2 on CPU for every system that uses them.
- LLM: Qwen2.5 3B Instruct on llama.cpp, the same for any system that calls a model on write.
- Scoring: a returned memory counts as a hit if it holds the answer as a whole word. For a changed fact, the new value must rank above the old one.
- Speed: one system at a time on a shared 8 core Xeon machine with 15 GB of RAM and no GPU. We paused the LLM server for every run without LLM calls.
- Packages: dependencies resolved for Python 3.12 or from the npm lock file.

## Not tested this round

| Project | Why not |
| --- | --- |
| claude-mem | Built into Claude Code. Hooks and a model write summaries, with no standalone store to call. |
| Hindsight | Runs in Docker and calls an LLM on every write. |
| cognee | Builds a knowledge graph from each document. Not run in this round. |
| Graphiti | Needs Neo4j, FalkorDB or Neptune, plus an LLM. |
| Supermemory | The local server asks for a model choice on first boot. |
| TencentDB Agent Memory | Needs two LLM configurations and a proxy. |
| Letta | A stateful agent harness, not a memory API you call. |
| Memori | Needs a Memori API key plus an LLM key. |
| memU | Needs a cloud key or an embedding provider, OpenAI by default. |
| EverOS | Needs an OpenRouter API key. |
| MemOS | Needs a hosted key or Docker with Neo4j and Qdrant. |

For mem0 the local 3B model took a median of 16.5 s per write on our CPU machine. Systems that call a model on every write need a GPU for a fair run. We plan to add them in a later round.

## Raw data

Everything is in [benchmarks/agent-memory](https://github.com/xerj-org/xerj/tree/main/benchmarks/agent-memory): the data set, the adapters, per question results, versions, commit SHAs and the exact commands.

## Try it with XERJ

Use the Copy the prompt button and paste it into your coding agent. It installs XERJ, stores ten facts, changes two and tells you which recall mode works best for your project.

## Sources checked

All checked on 2026-10-31. Star counts and licenses from the GitHub API. Versions tested: XERJ v1.0.0-rc.100, mem0ai 2.2.1, mempalace 3.11.0, reme-ai 0.4.2.1, agentmemory 0.9.31, mcp-memory-service 12.4.0 and memvid-sdk 2.0.160.
