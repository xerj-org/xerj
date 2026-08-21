---
title: "API search or vector search for an agent"
h1: "Should an agent use API search or vector search?"
description: "One question through 4 XERJ MCP tools returned 3 different result sets. Hybrid RRF returned 6 documents and merged both lists on the default lexical embedder."
slug: "mcp-api-search-or-vector-search"
cluster: "Agent access and memory"
question: "MCP/API search or vector search — which should an agent call?"
intent: "tool-selection"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, index a small labeled corpus into one XERJ index with a semantic_text field, then ask the same question through xerj_search, xerj_semantic_search, xerj_vector_search and xerj_hybrid_search over MCP, print the returned ids side by side, and say which tool you would call by default and why."
commands:
  - cmd: "curl -s -XPUT 'http://127.0.0.1:9500/eval' -H 'content-type: application/json' -d '{\"mappings\":{\"properties\":{\"doc_id\":{\"type\":\"keyword\"},\"title\":{\"type\":\"text\"},\"text\":{\"type\":\"semantic_text\"}}}}'"
    note: "One index holds the inverted index and the dense vectors together."
  - cmd: "curl -s http://127.0.0.1:9500/v1/embedding/identity"
    note: "Read which embedder the node uses before you judge any returned id."
  - cmd: "xerj mcp --url http://127.0.0.1:9500 --disable-feedback"
    note: "Run the MCP stdio server that exposes all 4 retrieval tools to the agent."
links_out:
  - "vector-database-vs-full-text-search"
  - "do-search-embeddings-help"
  - "give-chatgpt-claude-local-file-access"
faq:
  - q: "Should an agent call API search or vector search?"
    a: "Call `xerj_search` first. It is the only tool that needs no embedding and no vector, and it answered the question in our capture with 4 hits."
  - q: "Which tool returned the most documents?"
    a: "`xerj_hybrid_search` with `fusion: rrf`, at 6 documents. The 3 single-signal tools returned 4, 3 and 3 on the same question."
  - q: "Why did semantic and vector search return the same ids?"
    a: "Both read the same dense vectors. On this corpus they returned the identical 3 ids in the identical order with identical scores."
  - q: "Does the default embedder understand synonyms?"
    a: "No. The default embedder is lexical feature hashing with no model. Synonym matching needs `--embed-mode neural`, which is opt-in and CPU-only."
  - q: "How much does the neural embedder change the ranking?"
    a: "On 20 labeled documents and 8 judged queries it moved every vector-backed mode from precision@3 0.4167 to 0.6667. BM25 was unchanged at 0.375."
  - q: "What does `xerj_vector_search` need from the agent?"
    a: "A literal float array in `query_vector`. XERJ has no query-time text-embedding endpoint, so the agent embeds the query by indexing it and reading the companion vector."
  - q: "Why does my match query on a `semantic_text` field ignore the vectors?"
    a: "Because `match` runs BM25 over the analyzed text, not kNN. The response carries a `_xerj.hints` entry with code `lexical_on_semantic_text` that names the query to run instead."
  - q: "Is this comparison big enough to rank retrieval methods?"
    a: "No. It is 20 documents and 1 question through 4 tools. Read the ids as a shape, not as a score."
---

**TL;DR** — Start with `xerj_search`, because XERJ's default embedder is lexical feature hashing. In our MCP capture the same question through 4 XERJ tools returned 3 different result sets. The fused `xerj_hybrid_search` call returned 6 documents against 4, 3 and 3 for the single-signal tools.

## The 4 tools an agent can choose from

XERJ exposes `xerj_search`, `xerj_semantic_search`, `xerj_vector_search` and `xerj_hybrid_search` over MCP. All 4 proxy to the same node and the same index, and they differ only in which signal they score.

`xerj_search` needs only an `index`. The other 3 need a `field` and either a query string or a literal embedding. Each costs the agent extra setup before the first call.

## One question, 4 tools, 3 answers

We asked *how do I keep my car running well* through each tool against 20 labeled documents in one index. XERJ's default embedder is lexical feature hashing, and the neural embedder is opt-in through `--embed-mode neural`, so every vector row below is a lexical result.

| tool | total hits | top 3 ids |
| --- | --- | --- |
| `xerj_search` (BM25) | 4 | `d02`, `d04`, `d20` |
| `xerj_semantic_search` | 3 | `d01`, `d13`, `d14` |
| `xerj_vector_search` | 3 | `d01`, `d13`, `d14` |
| `xerj_hybrid_search` (`rrf`) | 6 | `d01`, `d02`, `d04` |

Three of those rows are 3 different answers to one question. An agent that calls only one tool sees only one of them.

## Semantic and vector search returned the identical list

`xerj_semantic_search` and `xerj_vector_search` returned the same 3 ids, in the same order, with the same scores of `0.58867896`, `0.5647036` and `0.5570914`. Both tools read the same dense vectors, and on this corpus the two paths agreed exactly.

The top hit `d01` is *Automobile maintenance schedule*, which is relevant. The 2 hits under it are *Coffee bean storage* and *Latency percentiles*, which are not.

Lexical feature hashing produces that pattern. The query shares the tokens *keep* and *well* with `d01`, and the ranking below the top hit is token overlap rather than meaning.

## What hybrid fusion changed

`xerj_hybrid_search` sends both clauses in one request and fuses the ranked lists with Reciprocal Rank Fusion (RRF).

```json
{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{
  "name":"xerj_hybrid_search",
  "arguments":{"index":"eval","fusion":"rrf",
    "queries":[
      {"weight":1.0,"query":{"match":{"text":"how do I keep my car running well"}}},
      {"query":{"knn":{"field":"text_vector","k":3,"num_candidates":20,
                       "query_vector":[0.0,0.0,0.09866005182266235]}}}]}}}
```

The fused result returned 6 documents and put `d01`, `d02` and `d04` on top. Among the 4 tools it was the single row holding both the BM25 winner `d02` and the vector winner `d01`.

## The hint that tells the agent it queried lexically

A plain `match` on a `semantic_text` field runs BM25 over the analyzed text, not kNN. XERJ says so in the response.

```json
{"_xerj":{"hints":[{"code":"lexical_on_semantic_text",
  "reason":"`text` is `semantic_text`, but this query scored it with BM25 over the analysed text ...",
  "try":{"request":"POST /eval/_search",
         "body":{"query":{"semantic":{"field":"text","query":"how do I keep my car running well","k":10}}}}}]}}
```

An agent that reads `_xerj.hints` fixes its own query. An agent that ignores the block will report vector results that never touched a vector.

## What the embedder choice is worth

A separate run scored 8 judged queries over the same 20 labeled documents in 5 retrieval modes. The default embedder is lexical feature hashing; the neural embedder is opt-in.

| mode | precision@3, lexical default | precision@3, `--embed-mode neural` |
| --- | --- | --- |
| BM25 | 0.375 | 0.375 |
| `semantic` | 0.4167 | 0.6667 |
| kNN | 0.4167 | 0.6667 |
| Hybrid RRF | 0.4167 | 0.6667 |

That run used plain HTTP, not the MCP path. Treat the neural column as evidence about the embedder, and never as an MCP measurement. We captured no neural-embedder MCP session.

## How an agent gets a query vector at all

`xerj_vector_search` requires a literal embedding in `query_vector`. XERJ has no query-time text-embedding endpoint, so the agent cannot hand it a sentence.

The supported route is 2 steps. Index the query text into a `semantic_text` field, then read the companion `<field>_vector` back out of `_source` and pass that array.

```sh
curl -s -XPOST 'http://127.0.0.1:9500/qvec/_doc/q1' -H 'content-type: application/json' -d '{"text":"how do I keep my car running well"}'
```

## Which tool to call by default

Call `xerj_search` first. The tool needs no embedder, no field name and no vector, and it answers structured and phrase questions directly. Escalate to `xerj_hybrid_search` when the first result set looks too narrow.

Call `xerj_semantic_search` only on a node started with `--embed-mode neural`. On the default lexical embedder it costs an extra round trip to reach a ranking built from token overlap.

## What this capture is and is not

One single-node XERJ process, 20 labeled documents and 1 question through 4 tools. That corpus is far too small to rank retrieval methods. The binary was a `ci-test` build, so no timing from the run is a performance figure.
