---
title: "How do I reduce embedding API cost?"
h1: "How do I reduce embedding API cost?"
description: "XERJ embeds documents inside the node by default, so a captured lexical run made 0 outbound calls. Proxy mode sent exactly 2 calls for the same 100 documents."
slug: "reduce-embedding-api-cost"
cluster: "Embeddings: cost"
question: "My embedding bill is too high"
intent: "cost"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a single-node XERJ with the default embedder, index a semantic_text corpus, then call GET /v1/embedding/identity and confirm the backend field reads lexical before you budget for any external embedding endpoint."
commands:
  - cmd: "curl -s http://127.0.0.1:9200/v1/embedding/identity"
    note: "Read which embedder the node actually uses."
  - cmd: "xerj --insecure --data-dir ./xerj-data"
    note: "Start a single-node process with the default lexical embedder."
  - cmd: "xerj --insecure --data-dir ./xerj-neural --embed-mode neural"
    note: "Start a node with the opt-in neural embedder instead."
links_out:
  - "do-search-embeddings-help"
  - "local-embeddings-without-openai-api"
  - "xerj-vs-vector-database"
faq:
  - q: "Does XERJ call an external embedding API?"
    a: "No, not by default. A network sampler watched the default node for its whole life and observed 0 non-loopback peers while it embedded 100 documents."
  - q: "How many calls does proxy mode make?"
    a: "The capture recorded exactly 2 HTTP calls for 100 documents, batched 64 and 36. Proxy mode batches inputs, so the call count is far below the document count."
  - q: "Which embedder does XERJ use by default?"
    a: "The default embedder is lexical feature hashing with 384 dimensions and no model file. The neural embedder is opt-in through --embed-mode neural."
  - q: "Is the free default embedder good enough?"
    a: "Not always. On one 20-document judged set the lexical default reached recall 0.7083 while --embed-mode neural reached 1.0, so the cheap path costs quality."
  - q: "Does the neural embedder call an API?"
    a: "No. The neural embedder runs a downloaded MiniLM-class model on the CPU inside the node, so it needs no key and no per-token billing."
  - q: "How do I check what my node is doing?"
    a: "Call GET /v1/embedding/identity on the native REST port. The backend field reads lexical, neural or proxy, and proxy is the only one that leaves the host."
---

**TL;DR** — XERJ embeds `semantic_text` fields inside the node, so the default configuration bills nothing to an embedding API. A capture watched a default single-node process for its whole 3.51 s life and observed 0 non-loopback peers. Proxy mode, which does call out, sent exactly 2 requests for 100 documents.

## The default embedder never leaves the host

XERJ embeds documents in-process, and the default embedder is lexical feature hashing with no model file. A sampler polled the node's sockets every 50 ms across 42 samples covering 3.51 s, which was the entire life of the node. The sampler observed 0 non-loopback peers.

That window covered index creation, a 20-document evaluation corpus, 2,048 routing vectors, a 100-document cost corpus and every query. The method is a sampler and not a packet capture, so a connection that opened and closed inside one 50 ms gap would be missed.

Ask the node which embedder it runs before you budget for anything.

```sh
curl -s http://127.0.0.1:9200/v1/embedding/identity
```

The captured answer on a default node names the backend directly.

```json
{ "data": { "backend": "lexical", "dimensions": 384, "resumable": true } }
```

## What proxy mode actually sends

Proxy mode is the only XERJ configuration that calls an external endpoint, and the capture counted its calls at the receiving end. For 100 documents it sent 2 HTTP POST requests to `/v1/embeddings`, carrying 64 inputs and then 36.

| Fact | Captured value |
| --- | --- |
| HTTP calls | 2 |
| Inputs embedded | 100 |
| Inputs per call | 64, then 36 |
| Body keys | `input`, `model` |
| Model name sent | `mock-embed-384` |
| `Authorization` header | absent, because no key was configured |

Batching is the whole cost story here. A provider that bills per request sees 2 requests, not 100, and a provider that bills per token sees the same text either way.

## The cheap path costs retrieval quality

The default lexical embedder is measurably weaker at the job people buy embeddings for. On 8 judged queries over a 20-document labeled corpus, the lexical default scored precision@3 0.4167 and recall 0.7083. The same queries under `--embed-mode neural` scored 0.6667 and 1.0.

BM25 scored precision@3 0.375 and recall 0.6875 in both modes, exactly as it must, because term matching does not depend on the embedder. Read the gap as a fixture result on 20 documents, not as a general quality claim.

## Three ways to pay, ranked by bill

Each mode is a start-time choice on one single-node process. The neural model downloads once and then runs on the CPU inside the node, so it has no per-token price.

| Mode | Outbound calls | What it costs |
| --- | --- | --- |
| Lexical, the default | 0 observed | Nothing outside the host, and the weakest retrieval |
| `--embed-mode neural` | 0 after the model download | CPU time in the node |
| Proxy | 2 calls per 100 documents in the capture | Your provider's price list |

Start a node on the opt-in neural embedder when the lexical default misses synonyms.

```sh
xerj --insecure --data-dir ./xerj-neural --embed-mode neural
```

One warning belongs with that flag. The neural embedder is not content-addressed, so the node reports `resumable: false`, and a changed embedder needs a fresh `autoindex` state directory.

## Single-node, and what that fixes

XERJ is single-node, so the embedder, the inverted index and the dense vectors live in one process on one host. There is no separate embedding service to run, scale or pay for. There is also no data-plane replication and no failover, so plan snapshot and restore rather than node loss.
