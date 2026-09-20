---
title: "How much better is hybrid search than BM25? Measured."
h1: "How much better is hybrid search than BM25? Measured nDCG numbers"
description: "XERJ measured BM25, vectors and hybrid RRF on two public BEIR datasets. Hybrid won both: nDCG@10 0.6993 against 0.6572 on SciFact, with a neural embedder."
slug: "hybrid-search-quality-measured-ndcg"
cluster: "Hybrid retrieval: quality"
question: "How much better is hybrid search than BM25 alone?"
intent: "informational"
published: "2026-09-18"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a throwaway XERJ node with --embed-mode neural, load the BEIR SciFact corpus with benchmarks/beir-hybrid/load.py, run benchmarks/beir-hybrid/eval.py, and report nDCG@10 for the BM25, vector-only, reorder and hybrid RRF arms next to the published figures."
commands:
  - cmd: "xerj --insecure --data-dir ./beir-data --embed-mode neural"
    note: "Start a throwaway node with the neural embedder. Without this flag the embedder is lexical feature hashing and the vector and hybrid figures on this page do not apply."
  - cmd: "curl -LO https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets/scifact.zip"
    note: "Download the public BEIR SciFact dataset: 5,183 documents and 300 test queries."
  - cmd: "curl -s 'http://127.0.0.1:9200/v1/embedding/identity'"
    note: "Confirm which embedder the node is running before you read any vector or hybrid number."
links_out:
  - "reciprocal-rank-fusion-when-to-use"
  - "do-search-embeddings-help"
  - "rerank-search-results-calibrated-judge"
  - "local-embeddings-without-openai-api"
evidence:
  - claim: "On BEIR SciFact (300 test queries, 5,183 documents) nDCG@10 was 0.6572 for BM25, 0.6764 for MiniLM vectors only, 0.6855 for a BM25 top-30 shortlist reordered by MiniLM, and 0.6993 for hybrid RRF."
    source: "benchmarks/beir-hybrid/results/scifact.txt"
  - claim: "On BEIR NFCorpus (323 test queries, 3,633 documents) nDCG@10 was 0.3016 for BM25, 0.3291 for MiniLM vectors only, 0.3323 for the reorder arm, and 0.3448 for hybrid RRF. BM25 returned zero hits for 25 of the 323 queries."
    source: "benchmarks/beir-hybrid/results/nfcorpus.txt"
  - claim: "Exactly 25 of the 323 NFCorpus test queries share no token with any document title or text, which accounts for every empty BM25 result list; the same check reports 0 for SciFact."
    source: "benchmarks/beir-hybrid/results/nfcorpus-lexical-gap.txt"
  - claim: "The run was made on 2026-09-18 with xerj v1.0.0-rc.74 and --embed-mode neural, using the built-in all-MiniLM-L6-v2 model at 384 dimensions on CPU, one node and one shard with default settings. Neural indexing ran at about 3 to 7 documents per second, SciFact took about 25 minutes, and hybrid queries cost about 220 to 370 ms at p50 against about 18 ms for BM25."
    source: "benchmarks/beir-hybrid/README.md"
  - claim: "The nDCG@10 figures for Jev (0.768 SciFact, 0.358 NFCorpus), Voyage rerank-3 (0.755, 0.357) and Cohere rerank-v3.5 (0.745, 0.340) are published in the hev/jev-rerank README. XERJ did not run them."
    source: "https://github.com/hev/jev-rerank"
faq:
  - q: "How much better is hybrid search than BM25?"
    a: "On two public BEIR datasets XERJ's hybrid RRF scored nDCG@10 0.6993 against 0.6572 for BM25 on SciFact, and 0.3448 against 0.3016 on NFCorpus. Both runs used `--embed-mode neural`."
  - q: "Do these numbers apply to a default XERJ node?"
    a: "Only the BM25 row does. The vector and hybrid rows need `--embed-mode neural`. The default embedder is lexical feature hashing, which has no model in it."
  - q: "Is reranking a BM25 shortlist with the same embedding model a good idea?"
    a: "Not in this run. Reordering the BM25 top 30 by MiniLM scored 0.6855 on SciFact, below 0.6993 for fusing with the same model. Fusion beat reordering on both datasets."
  - q: "How does XERJ hybrid compare to hosted rerankers?"
    a: "XERJ did not run them. Figures published elsewhere put Jev at 0.768 on SciFact and XERJ hybrid at 0.6993, but they use a different first stage, so it is not a controlled comparison."
  - q: "What does hybrid search cost in latency?"
    a: "In this run hybrid queries cost about 220 to 370 ms at p50 against about 18 ms for BM25, on a 5,000-document index on CPU. The machine was also compiling, so read those as upper bounds."
  - q: "How fast is neural indexing?"
    a: "About 3 to 7 documents per second on CPU in this run. SciFact's 5,183 short abstracts took about 25 minutes to index."
---

**TL;DR** — On two public BEIR datasets, XERJ's hybrid RRF beat BM25 alone and beat vectors alone: nDCG@10 0.6993 against 0.6572 on SciFact, and 0.3448 against 0.3016 on NFCorpus. These runs used `--embed-mode neural` with all-MiniLM-L6-v2. The default embedder is lexical feature hashing, and these vector figures do not apply to it.

## The numbers

The metric is nDCG@10 on each dataset's BEIR `test` split. The evaluation script computes it from live queries against a running node. Nothing is carried over from another run.

| Arm | SciFact (300 queries, 5,183 docs) | NFCorpus (323 queries, 3,633 docs) |
| --- | --- | --- |
| BM25 (`multi_match` on title and text) | 0.6572 | 0.3016 |
| MiniLM vectors only (`semantic`) | 0.6764 | 0.3291 |
| BM25 top 30, reordered by MiniLM | 0.6855 | 0.3323 |
| Hybrid RRF (`hybrid`, server-side) | 0.6993 | 0.3448 |

The run was made on 2026-09-18 with xerj v1.0.0-rc.74, on one node and one shard with default settings. The raw output is committed next to the scripts in `benchmarks/beir-hybrid`.

## Which embedder these numbers need

Read this before you quote a row. XERJ embeds with lexical feature hashing by default. That embedder has no model in it and cannot connect synonyms.

The vector, reorder and hybrid rows were measured with `--embed-mode neural`. That flag loads the built-in all-MiniLM-L6-v2 model, 384 dimensions, on CPU. A default node does not score these figures on those three rows. The BM25 row uses no embedder at all, so it holds on any node.

## Fusion beat reordering

The third row is the one that changes a design decision. It takes the BM25 top 30 and reorders them with the same MiniLM model. It scored 0.6855 on SciFact and 0.3323 on NFCorpus.

Hybrid RRF uses the same model and scored 0.6993 and 0.3448. Fusing with the model beat reranking with it, on both datasets.

The reason is recall. A reorder can only move documents that BM25 already returned. Fusion also admits documents that only the vector list found. On NFCorpus BM25 returned zero hits for 25 of the 323 queries, and no reorder can fix an empty list.

Those 25 empty lists are not an engine defect. Each of those queries is a word or phrase that occurs nowhere in the corpus, such as `deafness`, `eggnog` or `Zoloft`. A script in the benchmark folder counts them without a running node, and it reports 0 for SciFact, where BM25 had no empty lists.

This is why XERJ ships no local MiniLM rerank provider. It would score below the hybrid search that already exists.

## What it costs

Neural mode is not free. These figures come from the same run.

| Cost | Measured |
| --- | --- |
| Neural indexing speed | about 3 to 7 documents per second on CPU |
| Time to index SciFact | about 25 minutes |
| BM25 query, p50 | about 18 ms |
| Hybrid query, p50 | about 220 to 370 ms |

Most of the hybrid latency is one BERT forward pass per query on CPU. The machine was also compiling during the run, so treat the absolute latencies as upper bounds. One vector-arm latency in the SciFact raw log is a cache artefact from an earlier aborted run. It is not quoted here and must not be.

## How this relates to hosted rerankers

The `hev/jev-rerank` README publishes nDCG@10 for three hosted rerankers on the same datasets.

| System | SciFact | NFCorpus | Run by |
| --- | --- | --- | --- |
| Jev (TypeSafe AI) | 0.768 | 0.358 | that README, not XERJ |
| Voyage rerank-3 | 0.755 | 0.357 | that README, not XERJ |
| Cohere rerank-v3.5 | 0.745 | 0.340 | that README, not XERJ |
| XERJ hybrid RRF, local MiniLM | 0.6993 | 0.3448 | XERJ, `--embed-mode neural` |

XERJ did not run those three systems. They rerank that project's own first-stage shortlist, which is not XERJ's. The dataset and the metric match, and nothing else is controlled.

What the table supports is narrow. On NFCorpus the local hybrid figure sits inside the range of the published reranker figures. On SciFact it is lower than all three. A controlled comparison needs a provider key and XERJ's `rerank` stage, and XERJ has not run one.

## When to choose a hosted reranker instead

Choose a hosted reranker when ranking quality on hard queries matters most. You pay for it in latency, in cost, and in text that leaves the machine. The published SciFact figures for all three hosted systems are higher than XERJ's local hybrid figure. A reranker also returns a calibrated probability, which a fused score is not.

Choose local hybrid when text must stay on the machine, or when a paid call per search is not acceptable. XERJ supports both. `hybrid` runs on the node with the built-in lexical or neural embedder. With proxy embeddings, the query text goes to that embeddings API. The `rerank` stage calls a judge when an operator has set a key.

## What this run does not show

Two datasets, one model and one run each. Both datasets are scientific and medical text, so the gap may differ on code, logs or chat. The node was a single shard on a shared host. These figures show the ordering of the four arms on this data, not what your corpus will score.
