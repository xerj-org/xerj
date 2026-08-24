---
title: "What is Reciprocal Rank Fusion?"
h1: "What is Reciprocal Rank Fusion?"
description: "Reciprocal Rank Fusion scores a document by 1 divided by k plus its rank in each list. XERJ uses k 60, and this page checks the arithmetic by hand."
slug: "reciprocal-rank-fusion-when-to-use"
cluster: "Hybrid retrieval: RRF"
question: "What is reciprocal rank fusion and when should I use it?"
intent: "informational"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, run the same query through a BM25 request and a kNN request against one XERJ index, then run it once more through the hybrid clause with fusion rrf, and confirm each fused score equals the sum of 1/(60+rank) over the lists that returned the document."
commands:
  - cmd: "curl -s -XPUT 'http://127.0.0.1:9440/eval' -H 'content-type: application/json' -d '{\"mappings\":{\"properties\":{\"doc_id\":{\"type\":\"keyword\"},\"title\":{\"type\":\"text\"},\"text\":{\"type\":\"semantic_text\"}}}}'"
    note: "Create the judged index first. Without this step the queries below answer 404. Port 9440 is the capture's Elasticsearch-compatible listener; the default is 9200."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9440/eval/_search' -H 'content-type: application/json' -d '{\"query\":{\"match\":{\"text\":\"car\"}},\"size\":3,\"_source\":[\"doc_id\",\"title\"]}'"
    note: "Get the BM25 ranked list. Port 9440 is the capture's Elasticsearch-compatible listener; the default is 9200."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9440/eval/_mapping'"
    note: "Find the companion vector field name that the kNN sub-query needs."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9440/v1/embedding/identity'"
    note: "Name the embedder before you read any fused ranking. The capture returned backend lexical."
links_out:
  - "rag-without-vector-database"
  - "improve-basic-faiss-rag-pipeline"
  - "vector-database-vs-full-text-search"
faq:
  - q: "What is Reciprocal Rank Fusion?"
    a: "Reciprocal Rank Fusion is a rank-only rule that scores each document as the sum of weight divided by k plus rank, over every list that returned it."
  - q: "What value of k does XERJ use?"
    a: "XERJ uses a default k of 60. Our captured fused scores match 1 divided by 60 plus rank to seven decimal places, which confirms the default."
  - q: "Why use rank instead of score?"
    a: "BM25 scores and cosine scores have different scales, so adding them favors whichever list produces larger numbers. Rank positions are comparable without normalization."
  - q: "When should I use linear fusion instead?"
    a: "Use `linear` when you want one signal to dominate and the score scales are comparable. On our 8 judged queries linear scored 0.4583 against 0.4167 for `rrf`."
  - q: "Does RRF find documents neither list returned?"
    a: "No. Reciprocal Rank Fusion reorders the union of the input lists. On the query `car` both judged synonyms were missing from both lists and stayed missing."
  - q: "Can XERJ learn the fusion weights?"
    a: "No. `fusion: \"learned\"` is rejected with an HTTP 400 at parse time. XERJ offers `rrf` and `linear` only."
---

**TL;DR** — Reciprocal Rank Fusion scores a document by the sum of `weight / (k + rank)` across every ranked list that returned it. XERJ uses a default k of 60 inside the `hybrid` clause. Our captured fused score for `d02` was 0.032787, which is exactly 1/61 plus 1/61.

## The rule in one line

Reciprocal Rank Fusion is a fusion rule that uses rank position and ignores the original scores. Each list contributes `weight / (k + rank)` for every document it returned, and the fused score is the sum of those contributions. Rank is 1-based, and `k` is a constant that damps the difference between the top positions.

XERJ implements this as `fusion: "rrf"` inside the `hybrid` clause, with a default `k` of 60 and a per-sub-query `weight` that defaults to 1.0. The empty array below marks where the 384 floats of the query vector go.

```json
{"query":{"hybrid":{"queries":[
  {"query":{"match":{"text":"car"}},"weight":1.0},
  {"query":{"knn":{"field":"text_vector","k":3,"num_candidates":50,"query_vector":[]}},"weight":1.0}],
  "fusion":"rrf"}},"size":3}
```

## A worked example you can check by hand

Our capture ran the query `car` against one 20-document index on the default lexical embedder. XERJ embeds a `semantic_text` field with lexical feature hashing unless the node starts with `--embed-mode neural`. Treat this ranking as a mechanism demonstration, not a quality claim.

The BM25 sub-query returned exactly 1 hit. The kNN sub-query returned 3.

| list | rank 1 | rank 2 | rank 3 |
| --- | --- | --- | --- |
| BM25 | `d02` (3.567053) | — | — |
| kNN | `d02` (0.710547) | `d13` (0.666096) | `d09` (0.55798) |

Now add the contributions with `k` at 60 and both weights at 1.0.

| document | BM25 contribution | kNN contribution | sum | XERJ returned |
| --- | --- | --- | --- | --- |
| `d02` | 1/61 = 0.0163934 | 1/61 = 0.0163934 | 0.0327869 | 0.032787 |
| `d13` | none | 1/62 = 0.0161290 | 0.0161290 | 0.016129 |
| `d09` | none | 1/63 = 0.0158730 | 0.0158730 | 0.015873 |

The three sums match the captured response to seven decimal places, and the small residual is float32 rounding in the response.

## A tie, and what breaks it

Two documents that hold mirrored positions receive the same fused score. Our capture found one. For the query `how does the index recover after a restart`, BM25 ranked `d04` first and `d08` second, while kNN ranked `d08` first and `d04` second.

| document | BM25 rank | kNN rank | sum | XERJ returned |
| --- | --- | --- | --- | --- |
| `d08` | 2 → 1/62 | 1 → 1/61 | 0.0325225 | 0.032522473 |
| `d04` | 1 → 1/61 | 2 → 1/62 | 0.0325225 | 0.032522473 |
| `d10` | deeper than rank 3 | 3 → 1/63 | 0.0312576 | 0.031257633 |

Both tied documents carry the identical fused score in the response, and XERJ placed `d08` first.

The third row is worth reading closely. The score 0.031257633 for `d10` equals 1/63 plus 1/65. Fusion therefore credited a BM25 position deeper than the 3 hits that the size-3 BM25 response showed.

## When Reciprocal Rank Fusion helps

Reciprocal Rank Fusion helps when the two lists disagree and both are partly right. On the restart question, BM25 alone put an irrelevant document first and kNN alone put a judged-relevant document first. The fused list kept the judged-relevant document in position 1.

Across the 8 judged queries on the lexical default, precision at 3 was 0.375 for BM25 and 0.4167 for both kNN and `rrf`. Weighted `linear` fusion reached 0.4583, the best single result in the lexical run. Fusion did not beat every alternative here.

## When it does not help

Reciprocal Rank Fusion reorders the union of the input lists and never adds a document. On the query `car` the BM25 list held 1 document and the kNN list held 3. The fused list is a reordering of those 3.

The two judged synonyms, `Automobile maintenance schedule` and `Vehicle inspection rules`, were absent from both lists and stayed absent.

That is a retrieval problem, not a fusion problem. On the same query with `--embed-mode neural`, the kNN list returned all 3 judged documents and recall reached 1.0.

## No learned fusion exists

The `FusionStrategy` type names three variants and XERJ serves two of them. Both `rrf` and `linear` work.

XERJ rejects `fusion: "learned"` with an HTTP 400 at parse time. There is no learned or trained fusion to configure, tune or sell.

## What this capture does not show

The corpus is 20 documents and the judged set is 8 queries, run once each on a shared single-node host. These figures show how the arithmetic behaves, not how a production corpus ranks. The run's summary file lists every returned document and every miss.
