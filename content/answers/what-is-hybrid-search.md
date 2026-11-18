---
title: "What is hybrid search? BM25 plus vectors, explained"
h1: "What is hybrid search?"
description: "Hybrid search runs BM25 and vector search on one query and merges the two ranked lists. Try a live toy, a worked RRF example and a recorded benchmark."
slug: "what-is-hybrid-search"
cluster: "Search quality"
question: "What is hybrid search?"
intent: "definition"
published: "2026-10-30"
updated: "2026-10-30"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, install XERJ, start a local node with --embed-mode neural, index a folder of my docs with xerj autoindex, then run the same question three ways (match, knn, hybrid with fusion rrf) and show me the top 5 of each side by side so I can see what hybrid search adds."
answer: "What is hybrid search? It runs a keyword search such as BM25 and a vector search on the same query, then merges the two ranked lists into one. Keywords catch exact words like error codes and names. Vectors catch the same idea in different words. Fusion, often Reciprocal Rank Fusion, combines the ranks."
intent_group: "hybrid search"
queries:
  - "hybrid search"
  - "what is hybrid search"
  - "hybrid search without a vector database"
  - "hybrid search explained"
commands:
  - cmd: "curl -fsSL https://xerj.org/get | sh"
    note: "Install the single XERJ binary."
  - cmd: "xerj --embed-mode neural"
    note: "Start a local node that embeds text with all-MiniLM-L6-v2. The model downloads once, about 90 MB."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/docs/_search' -H 'content-type: application/json' -d '{\"query\":{\"hybrid\":{\"queries\":[{\"query\":{\"match\":{\"body\":\"server unreachable\"}},\"weight\":1.0},{\"query\":{\"knn\":{\"field\":\"body_vector\",\"k\":10,\"num_candidates\":50,\"query_vector\":[]}},\"weight\":1.0}],\"fusion\":\"rrf\"}},\"size\":5}'"
    note: "One hybrid request. Fill query_vector with your embedding; the empty array marks where the floats go."
links_out:
  - "how-xerj-combines-search"
  - "reciprocal-rank-fusion-when-to-use"
  - "hybrid-search-quality-measured-ndcg"
  - "bm25-vs-vector-search"
  - "rag-without-vector-database"
  - "vector-database-vs-full-text-search"
faq:
  - q: "What is hybrid search in one sentence?"
    a: "It runs a keyword search and a vector search on the same query and merges the two ranked lists into one list."
  - q: "Is hybrid search better than vector search alone?"
    a: "Often, but not always. On the recorded BEIR SciFact run, hybrid RRF scored 0.6993 nDCG@10 against 0.6764 for vectors alone. Test it on your own queries before you decide."
  - q: "Do I need a vector database for hybrid search?"
    a: "No. You need one place that holds both an inverted index and vectors. XERJ keeps both in the same index and fuses them inside one request."
  - q: "What does the k in Reciprocal Rank Fusion do?"
    a: "Each result adds weight / (k + rank) to its score. A larger k flattens the gap between rank 1 and rank 10. XERJ uses k = 60 by default and accepts another value."
  - q: "Should I use RRF or a weighted score sum?"
    a: "Start with RRF because it ignores raw scores, which sit on different scales. XERJ also offers linear fusion, which normalizes each list and adds the weighted scores."
  - q: "When is hybrid search not worth it?"
    a: "When every query is an exact identifier, plain BM25 is enough. When every query is a loose paraphrase over short texts, vectors may be enough. Hybrid helps when you get both kinds."
---

**TL;DR** Hybrid search runs two searches on the same query: a full-text search such as BM25 and a vector search. Each returns a ranked list. A fusion step, usually Reciprocal Rank Fusion, merges them into one list. You get exact matches and close meanings in the same results.

```visual
hybrid-search-flow
```

## Why two searches

Full-text search and vector search fail in different places.

BM25 scores the words a result shares with the query. It is very good at exact things: an error string, a function name, a ticket number like `INC-4821`. It finds nothing when the result uses other words for the same idea.

Vector search turns text into a list of numbers and returns the closest results by meaning. It finds "cannot reach the server" for the query "server unreachable". It is weaker on exact tokens, because a model can place `port 9200` and `port 9300` very close together.

Real queries mix both kinds. Hybrid search does not ask you to choose.

## Try it

Pick a query or type your own. Watch the two columns disagree, then move the sliders to see how fusion settles it.

```visual
hybrid-rrf-lab
```

Three things to notice:

- `network down` shares no word with any line, so BM25 returns nothing. The vector side still finds the connection error.
- `port 9200` is an exact token. BM25 puts the two lines that contain it on top.
- Set the vector weight to 0 and the fused list becomes the BM25 list. Fusion is a dial, not a switch.

## How Reciprocal Rank Fusion works

RRF ignores raw scores. It only reads where a result landed in each list. Each list adds `weight / (k + rank)` to a result. Results that rank well in both lists rise to the top.

A worked example with `k = 60` and both weights at 1:

| Result | BM25 rank | Vector rank | RRF score |
| --- | --- | --- | --- |
| A | 1 | 3 | 1/61 + 1/63 = 0.0323 |
| B | 2 | 1 | 1/62 + 1/61 = 0.0325 |
| C | not found | 2 | 1/62 = 0.0161 |

B wins because it ranks high in both lists. C still makes the list even though BM25 never saw it. That is the point of hybrid search.

The [RRF guide](/answers/reciprocal-rank-fusion-when-to-use) covers when to change `k` and when to use a weighted score sum instead.

## Does it help? A recorded run

Benchmarks are the honest check. This run used the public BEIR SciFact set on one XERJ node.

```visual
hybrid-beir-bars
```

Hybrid beat both single methods here, by about 2 to 4 points. On NFCorpus the same setup gave 0.3016 for BM25, 0.3291 for vectors and 0.3448 for hybrid. Your data may differ, so measure it. The [nDCG page](/answers/hybrid-search-quality-measured-ndcg) shows how.

## Hybrid search without a vector database

Many hybrid setups run two systems: a search engine for keywords and a vector database for embeddings, plus code to merge them. That means two stores to keep in sync.

XERJ keeps the inverted index and the vectors in the same index. One request carries both sub-queries and the node fuses them. The [how XERJ combines search](/answers/how-xerj-combines-search) page shows the full request and response.

| Setup | Stores to run | Who merges the lists |
| --- | --- | --- |
| Search engine plus vector database | 2 | Your code |
| One engine with both index types | 1 | The engine |

## When to skip it

- Every query is an exact ID or code: BM25 alone is simpler and fast.
- You have no budget for an embedder: start with BM25 and add vectors later.
- Your texts are very short and queries are always loose paraphrases: test vectors alone first.

## How to try it with XERJ

Use the Copy the prompt button and paste it into your coding agent, or run the commands above yourself. XERJ is one binary on your machine. The neural embedder runs locally, so the text you index stays on your disk.

If you want a ready-made corpus to test against, [hub.xerj.org](https://hub.xerj.org) lists search packs you can pull.

## Sources checked

All checked on 2026-10-30.

- RRF and linear fusion, default `k = 60`: `engine/crates/xerj-query/src/parser.rs` and `fuse_rrf` in `engine/crates/xerj-engine/src/index.rs`.
- BM25 defaults `k1 = 1.2`, `b = 0.75`: `engine/crates/xerj-fts/src/bm25.rs`.
- Benchmark numbers: `benchmarks/beir-hybrid/results/scifact.txt` and `nfcorpus.txt`, recorded 2026-09-18.
- Toy scores: a local XERJ v1.0.0-rc.97 node on the six lines shown above.
