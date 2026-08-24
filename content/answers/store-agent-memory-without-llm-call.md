---
title: "Stop agent memory filling with duplicate notes"
h1: "How do I stop an agent memory store from filling with near-duplicate notes?"
description: "Send dedup true on the store call and XERJ returns the existing id instead of a second document. The write path runs no model and made no outbound call."
slug: "store-agent-memory-without-llm-call"
cluster: "Agent memory: model dependency"
question: "How do I dedup agent memory?"
intent: "how-to"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, then POST the same fact twice to /_memory/agentmem on a running XERJ node with dedup true and dedup_threshold 0.95, confirm the second response carries created false and deduplicated true and the same id, POST a reworded version of the same fact and report whether it deduplicated, then recall the namespace and report how many documents it holds."
commands:
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9430/_memory/agentmem' -H 'content-type: application/json' -d '{\"text\":\"The user prefers p50 and p95 latency, never the mean.\",\"dedup\":true,\"dedup_threshold\":0.95}'"
    note: "Store with the near-duplicate check on. Send this twice: the second call returns the first id with deduplicated true and writes nothing."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9430/_memory/agentmem' -H 'content-type: application/json' -d '{\"text\":\"The user prefers p50 and p95 latency, never the mean.\",\"metadata\":{\"topic\":\"preferences\"}}'"
    note: "Store one memory with the check off, which is the default. No model runs on this path and no summarization step exists."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9430/_memory/agentmem/_recall' -H 'content-type: application/json' -d '{\"query\":\"where is the deploy runbook\",\"k\":5}'"
    note: "Plain recall. This runs BM25 over the memory text and invokes no embedder."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9430/_memory/agentmem/_recall' -H 'content-type: application/json' -d '{\"query\":\"which file documents restarting the service\",\"semantic\":true,\"k\":5}'"
    note: "Recall with server-side embedding. On a default node the embedder is lexical feature hashing."
evidence:
  - claim: "The memory store call accepts dedup and dedup_threshold, dedup_threshold defaults to a cosine similarity of 0.95, and _recall accepts recency_weight."
    source: "engine/crates/xerj-api/src/memory_api.rs"
links_out:
  - "agent-memory-without-postgres-qdrant"
  - "do-search-embeddings-help"
  - "local-embeddings-without-openai-api"
faq:
  - q: "How do I dedup agent memory?"
    a: "Send `dedup: true` on the store call. When the cosine score against an existing memory meets `dedup_threshold`, XERJ returns that id with `\"deduplicated\": true` and writes nothing new."
  - q: "Can I prefer recent memories when recalling?"
    a: "Yes. `_recall` accepts `recency_weight`, clamped between 0 and 1, which blends the stored time into the ranking. Leave it out and the ranking is relevance only."
  - q: "How do I keep long-term memory from repeating itself?"
    a: "Deduplicate at write time and choose the threshold deliberately. The comparison uses the configured embedder, which by default is lexical feature hashing, so it matches wording rather than meaning."
  - q: "Can I store agent memory without an LLM call?"
    a: "Yes. A store call is one HTTP POST that writes a document. XERJ runs no model, no summarization and no extraction step on that path."
  - q: "Does recall call a model?"
    a: "Only when you ask. A plain `query` runs BM25, a supplied `vector` runs kNN on your own embedding, and `semantic: true` embeds server-side."
  - q: "What embedder runs for semantic recall?"
    a: "Lexical feature hashing by default, which is a hash function with no model and no network call. `--embed-mode neural` swaps in a downloaded MiniLM-class model."
  - q: "Does XERJ rewrite or summarize what I store?"
    a: "No. The stored `text` is the text you sent. There is no model in the write path to change it."
---

**TL;DR** — Send `dedup: true` on the store call and XERJ returns the existing id with `"deduplicated": true` instead of writing a second document. The check compares against the memories the namespace already holds, at a default cosine threshold of 0.95. No model and no summarization runs on the write path.

## Turn on the near-duplicate check at write time

`POST /_memory/{namespace}` accepts `dedup` and `dedup_threshold`. With `dedup` set, XERJ scores the new text against the memories the namespace already holds, using `dedup_threshold` or its default of 0.95.

A cosine score at or above the threshold returns the existing id. The response carries `"deduplicated": true`, and no second document is written.

The comparison uses the embedder the node runs. On the default that is lexical feature hashing, so it matches wording and not meaning. A node started with `--embed-mode neural` changes what counts as a near-duplicate.

## Prefer recent memories at recall time

`_recall` accepts `recency_weight`, clamped between 0 and 1, which blends the stored time into the ranking. Leave it out and the ranking is relevance only. Nothing ages out on its own, so the write-time check is what keeps the store small.

## The write path has no model in it

One request to `POST /_memory/{namespace}` with a `text` field stores a memory. XERJ writes the text verbatim into a document and returns an identifier. No model reads it, rewrites it or summarizes it.

```json
{"created":true,"id":"2b00ed19-8741-49ca-8318-54e9ba8c7a58","namespace":"agentmem"}
```

The stored document carries what you sent. A later `GET /_memory/{namespace}` returned the same sentence and the same `metadata` object our capture posted.

## What the network watch saw

Our capture polled `/proc/net/tcp` and `/proc/net/tcp6` against the node process tree during store and recall. The watch ran 31 samples at a 50 ms interval, covering 2.27 s and up to 7 processes. That watch counted 0 distinct non-loopback peers.

```json
{"count_distinct_non_loopback_peers":0,"samples_taken":31,
 "sample_interval_s":0.05,"watched_seconds":2.27,"max_pids_in_tree":7,
 "limitation":"SAMPLER, not a packet capture"}
```

Read that number as written. The harness sampled a loopback-bound node rather than denying it a network. A connection that opens and closes inside one 50 ms gap escapes the sampler.

## Which recall mode invokes an embedder

Recall chooses exactly one mode, in a strict order, and never blends them. There is no fusion in `_recall`.

| request field | what runs | embedder involved |
| --- | --- | --- |
| `vector` supplied | pure kNN against your embedding | no, you embedded it |
| `semantic: true` | server-side embed, then a `semantic` clause | yes |
| plain `query` | `{"match":{"text":q}}`, which is BM25 | no |

The choice changes the answer. Our plain-query recall put the deploy memory first at 1.4581499. The `semantic: true` recall of a related question put the latency memory first at 0.627885 instead.

## The default embedder is not a model

The `semantic: true` mode does invoke an embedder. On a default node that embedder is lexical feature hashing over word unigrams and character trigrams into 384 dimensions. That embedder loads no model file and makes no network call.

The node reports the backend directly.

```json
{"data":{"backend":"lexical","dimensions":384,"resumable":true}}
```

The flag `--embed-mode neural` replaces that hash with a downloaded MiniLM-class model on the local CPU. Even then the model is an embedder rather than a language model. No generation happens on the memory path.

## The one mode that leaves the machine

XERJ has a third embedder mode named `proxy`. That mode posts to an OpenAI-compatible endpoint that you configure with an `endpoint`, an optional `api_key` and a `model` name. That mode is opt-in and not the default.

Two facts protect you from turning it on by accident. The default resolves to lexical when no endpoint is configured, and the node's identity route names the backend it actually loaded.

## Failures are loud, not silent

Recall parses its body strictly, so an unrecognized key is an HTTP 400 that lists the accepted fields. Without that strictness an agent receives a match-all result at score 1.0 and never learns of the mistake.

```text
malformed JSON in request body: unknown field `quety`, expected one of `query`,
`vector`, `semantic`, `k`, `filter`, `recency_weight`, `graph` at line 1 column 8
```

## What this capture does not show

The content map asked for a network-denied store request, and the harness instead sampled the sockets of a loopback-bound single-node process. The harness applied no firewall and no network namespace, and it never exercised the proxy embedder mode.
