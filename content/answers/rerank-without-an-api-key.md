---
title: "Can I rerank search results without an API key?"
h1: "Can I rerank search results without an API key or a cloud reranker?"
description: "Yes. XERJ's local rerank provider runs a cross-encoder inside the node: no key, no network call, nothing leaves the host. Measured on BEIR, and it is opt-in, not a default."
slug: "rerank-without-an-api-key"
cluster: "Hybrid retrieval: reranking"
question: "Can I rerank search results without an API key or sending documents to a cloud service?"
intent: "informational"
published: "2026-09-20"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a throwaway XERJ node, index a handful of documents, then run the same query three ways: plain, with a hybrid query, and with rerank provider local. Report the _rerank block of the third response, including applied, score_kind and local.calibration.method, and say which of the three orders you would ship."
commands:
  - cmd: "curl -s 'http://127.0.0.1:9200/_xerj/rerank' | jq .local"
    note: "Ask the node about the local judge before using it. It reports whether it is compiled in, which tier is the default, and each model's download state. It loads nothing and contacts nothing. The route is superuser-only on any node not started with --insecure."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/kb/_search' -H 'content-type: application/json' -d '{\"query\":{\"match\":{\"body\":\"vitamin d bone density\"}},\"rerank\":{\"provider\":\"local\"}}'"
    note: "Rerank the top 30 hits with the in-process cross-encoder. No key is needed and nothing leaves the machine. The first call on a node that has never downloaded the model answers with the engine's own order and applied false while the download runs."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/kb/_search' -H 'content-type: application/json' -d '{\"query\":{\"hybrid\":{\"queries\":[{\"query\":{\"multi_match\":{\"query\":\"vitamin d bone density\",\"fields\":[\"title\",\"text\"]}}},{\"query\":{\"semantic\":{\"field\":\"body\",\"query\":\"vitamin d bone density\"}}}],\"fusion\":\"rrf\"}}}'"
    note: "Run the arm the local judge has to beat. Hybrid RRF ships today, costs no model forward pass per hit, and on two of three BEIR datasets it ranked as well as or better than a local rerank of a BM25 shortlist."
links_out:
  - "rerank-search-results-calibrated-judge"
  - "hybrid-search-quality-measured-ndcg"
  - "local-embeddings-without-openai-api"
  - "reciprocal-rank-fusion-when-to-use"
evidence:
  - claim: "The local rerank provider scores documents inside the node process. It needs no API key and opens no connection while scoring. Its only network use is the one-time model download from the HuggingFace Hub, which sends no document or query text and which judge.download false forbids."
    source: "engine/crates/xerj-rerank/src/local.rs"
  - claim: "On BEIR with the default small tier, reranking the hybrid RRF top 30 scored nDCG@10 0.6936 on SciFact against 0.7021 for hybrid alone, 0.3597 against 0.3445 on NFCorpus, and 0.3751 against 0.3572 on FiQA. The 95 percent paired bootstrap interval excluded zero on NFCorpus and FiQA and spanned zero on SciFact."
    source: "benchmarks/local-judge/README.md"
  - claim: "Reranking a BM25 only shortlist with the same model gained on all three datasets: 0.6572 to 0.6824 on SciFact, 0.3016 to 0.3370 on NFCorpus and 0.2382 to 0.3160 on FiQA."
    source: "benchmarks/local-judge/results/ndcg.txt"
  - claim: "Hybrid RRF itself moved between 0.7019 and 0.7034 on SciFact and between 0.3436 and 0.3445 on NFCorpus across three fresh node processes over unchanged indices, because tied fusion scores are ordered by a per-process seed. The reranked arms scored identically across the same three processes, to four decimal places."
    source: "benchmarks/local-judge/results/ndcg.txt"
  - claim: "One 30 document local rerank call cost seconds, not milliseconds, on a 32 core x86 CPU with the machine otherwise idle, and raising the thread count did not make it faster."
    source: "benchmarks/local-judge/results/latency.txt"
  - claim: "Every shipped local tier carries Calibration NONE, the raw sigmoid, so the response reports score_kind relevance rather than probability. The small tier's Platt and temperature fits both diverged on a held out split. The base tier's Platt fit converged and cut pooled expected calibration error from 0.0509 to 0.0094, but it was fitted and tested on the same two corpora, so it does not ship."
    source: "engine/crates/xerj-ai/src/judge.rs"
  - claim: "The base tier reranked the hybrid top 30 to nDCG@10 0.7186 on SciFact, the best SciFact figure measured, and to 0.3328 on NFCorpus, below both hybrid RRF and the smaller tier."
    source: "benchmarks/local-judge/results/ndcg.txt"
faq:
  - q: "Can I rerank search results without an API key?"
    a: "Yes. Send `\"rerank\": {\"provider\": \"local\"}` in a XERJ `_search` body. A cross-encoder runs inside the node, so there is no key, no network call and no per-token bill."
  - q: "Does the local reranker send my documents anywhere?"
    a: "No. Scoring happens in the node process. The only network use is the one-time model download from the HuggingFace Hub on first use, which sends no document or query text. `[judge] download = false` forbids even that."
  - q: "Is the local reranker better than XERJ's hybrid search?"
    a: "Not reliably. The default tier gained on NFCorpus and FiQA over a hybrid first stage and was indistinguishable on SciFact. The `base` tier was the other way round: best on SciFact, worse than hybrid on NFCorpus. No tier won everywhere, so it is opt-in."
  - q: "Is the local reranker's score a probability?"
    a: "No. Every shipped tier reports `score_kind: \"relevance\"` — a monotone 0 to 1 ranking score. The default tier's calibration fit diverged; the `base` tier's converged but was never tested outside the two corpora it was fitted across, so neither ships. Tune `rerank.min_score` per corpus."
  - q: "How slow is a local rerank on a CPU?"
    a: "Seconds per 30-document call on a 32-core x86 machine with nothing else running, and more threads did not help. Treat it as a deliberate, per-request cost, not something to switch on globally."
  - q: "Which model does it use and what is the licence?"
    a: "Three tiers: `small` (cross-encoder/ms-marco-MiniLM-L6-v2, Apache-2.0, the default), `base` (BAAI/bge-reranker-base, MIT) and `large` (BAAI/bge-reranker-v2-m3, Apache-2.0). XERJ ships no weights; each is downloaded at a pinned revision with a checked sha256. All three were trained on data that includes MS MARCO, whose terms say non-commercial research only, so take advice before commercial use."
  - q: "How do I turn the local reranker off?"
    a: "`[judge] enabled = false`. Note that `[rerank] enabled = false` does not: that setting exists to forbid egress, and the local provider has none."
---

**TL;DR** — Yes: `"rerank": {"provider": "local"}`. A cross-encoder runs inside the XERJ node — no key, no network call, no bill, and nothing about your query or documents leaves the host. We measured it on three BEIR datasets. Over a lexical first stage it is a large gain. Against the fused ranking XERJ already ships it did **not** win on every dataset. So it is opt-in rather than a default.

## The request

```json
POST /kb/_search
{
  "query": { "match": { "body": "vitamin d bone density" } },
  "rerank": { "provider": "local", "window": 30 }
}
```

That is the whole setup. The provider is compiled into the stock release binaries. It loads its model the first time a request asks for it. There is no key to obtain, and `_rerank.local.data_egress` in the response says `"none"`.

A cross-encoder reads the query and one document *together* and emits one relevance score. That is the opposite trade from the embedder. The embedder reads each text alone, so its vectors can be stored in the index. A cross-encoder cannot be precomputed. It costs one model forward pass per candidate. So it is a second stage over a short window, and the window is what you pay for.

## What it cost and what it bought

Measured on three public BEIR datasets, against a node running `--embed-mode neural` with all-MiniLM-L6-v2. That flag matters: it makes the hybrid arm a real hybrid, not the default lexical embedder. nDCG@10 on each `test` split.

| Arm | SciFact | NFCorpus | FiQA |
| --- | --- | --- | --- |
| BM25 | 0.6572 | 0.3016 | 0.2382 |
| BM25 top-30, reranked by `small` | 0.6824 | 0.3370 | 0.3160 |
| BM25 top-30, reranked by `base` | 0.7084 | 0.3141 | not run |
| Hybrid RRF (ships today) | 0.7021 | 0.3445 | 0.3572 |
| Hybrid top-30, reranked by `small` | 0.6936 | 0.3597 | 0.3751 |
| Hybrid top-30, reranked by `base` | 0.7186 | 0.3328 | not run |

nDCG@10 on each dataset's `test` split. Two readings, and they point different ways.

**Against a BM25-only first stage it is a clear, large win.** It adds 0.025, 0.035 and 0.078. If your node has no vectors, this is the cheapest relevance you can buy.

**Against hybrid RRF it is not a clear win.** It gained 0.0152 on NFCorpus and 0.0179 on FiQA. Both intervals exclude zero. It lost 0.0085 on SciFact, with an interval that spans zero. Two gains and one wash is not the evidence a default needs.

**A bigger model does not settle it either.** The `base` tier reranked the hybrid top 30 to 0.7186 on SciFact, the best SciFact figure we measured. On NFCorpus the same tier scored 0.3328, below both hybrid and the smaller model. The two tiers disagree about which corpus they help.

## Why "opt-in, not the default" is the honest answer

Three reasons, in order of weight.

1. **No tier beat hybrid RRF on every dataset.** Hybrid costs no model forward pass per hit. The default tier did not beat it on SciFact.
2. **A local rerank call costs seconds on a CPU.** That is one 30-document call on an idle 32-core x86 machine. More threads did not help. The model's CPU kernels stop scaling well short of the core count.
3. **The score is not calibrated.** The response reports `score_kind: "relevance"`, not `"probability"`. The default tier's calibration fit diverged. The `base` tier's fit converged, but only the two corpora it was fitted across were tested. Tune `rerank.min_score` per corpus.

## Noise, and why the numbers above can be trusted

XERJ's hybrid fusion breaks ties by a per-process seed. The hybrid baseline therefore moves a little between node restarts. We measured that rather than assuming it.

Over three fresh node processes on unchanged indices, hybrid scored 0.7021, 0.7034 and 0.7019 on SciFact. On NFCorpus it scored 0.3445, 0.3436 and 0.3438. Earlier runs widen the SciFact band to 0.0051.

The NFCorpus and FiQA gains above are larger than that band. The SciFact loss is not.

The candidate **set** was identical on 100 percent of queries across those three processes. Only the order of tied hits moved. A second stage that reorders the whole set by model score therefore cannot inherit that noise. It does not: every reranked arm scored identically across all three processes, to four decimal places.

## Turning it off, and the switch that does not

```toml
[judge]
enabled = false      # refuses provider "local" with HTTP 403
download = false     # never contacts huggingface.co; the model must be on disk
model_dir = "/opt/xerj/models"   # air-gapped: <model_dir>/rerank-<tier>/
```

`[rerank] enabled = false` does **not** switch the local provider off. That setting exists to forbid document text leaving the node, and the local provider does not send any. `[judge] enabled = false` is the one that refuses it.

## When to reach for it

- Your first stage is BM25 only, and re-indexing for vectors is not on the table.
- The alternative you were considering is a hosted reranker, and the documents cannot leave the machine.
- You are on an air-gapped node, or a laptop with no account anywhere.

And when not to: as a blanket quality setting on a node that already runs hybrid search. Run the hybrid query instead, measure both on your own corpus, and switch the rerank on per request where it earns its seconds.
