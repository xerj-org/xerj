---
title: "Local search offline with no cloud embeddings"
h1: "I want local search that works offline, no cloud embeddings. What are my options?"
description: "XERJ embeds locally in two ways. The default lexical embedder needs no model at all, and --embed-mode neural loads a cached MiniLM model on the CPU."
slug: "local-embeddings-without-openai-api"
cluster: "Embeddings: local execution"
question: "I want local search that works offline, no cloud embeddings. What are my options?"
intent: "how-to"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start one XERJ node with --embed-mode neural and a run-local HF_HOME, index 20 documents twice, and report both wall-clock times together with the output of GET /v1/embedding/identity."
commands:
  - cmd: "curl -s -XGET 'http://127.0.0.1:9440/v1/embedding/identity'"
    note: "Confirm the default node uses the built-in lexical embedder and downloads no model."
  - cmd: "env HF_HOME=./hf-cache xerj --insecure --data-dir ./xerj-neural --embed-mode neural"
    note: "Start the opt-in neural embedder with a run-local model cache. HF_HOME keeps the model download inside ./hf-cache instead of your home directory."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9200/v1/embedding/identity'"
    note: "Read the identity of the node you just started. The capture returned backend neural and resumable false."
links_out:
  - "do-search-embeddings-help"
  - "store-agent-memory-without-llm-call"
  - "rag-without-vector-database"
  - "/compare/xerj-vs-khoj"
faq:
  - q: "I want local search that works offline, no cloud embeddings. What are my options?"
    a: "Two, and both stay on the host. The default embedder is lexical feature hashing inside the binary, and `--embed-mode neural` runs a downloaded MiniLM model on the local CPU."
  - q: "How do I run embeddings locally without the OpenAI API?"
    a: "Start the node as usual for the lexical default, or with `--embed-mode neural` for the local model. Neither path needs an API key."
  - q: "Can I do RAG without a vector database?"
    a: "Yes. XERJ keeps the inverted index and the dense vector in one index, so retrieval for a prompt is a single `_search` call against the folder you indexed."
  - q: "How do I search a private folder without uploading it?"
    a: "Index it in place with `xerj autoindex`. Our sampler observed 0 non-loopback peers across the entire life of a default node; it samples every 50 ms, so it is not a packet capture."
  - q: "Which model does neural mode use?"
    a: "`sentence-transformers/all-MiniLM-L6-v2`, a 6-layer BERT model with 384 dimensions read from the loaded model config. XERJ downloads it from HuggingFace Hub on first use."
  - q: "How do I run this air-gapped?"
    a: "Point `embedding.local_model_dir` at a model you placed on disk yourself. XERJ pins no checksum on the Hub download, so the integrity guarantee is TLS plus the Hub."
  - q: "Can I switch embedders on an existing index?"
    a: "No. XERJ refuses a backend swap on a populated semantic index rather than mixing vector spaces. Start a fresh autoindex state instead."
---

**TL;DR** — XERJ embeds locally in two ways. The default embedder is lexical feature hashing inside the binary, which needs no model and made 0 outbound connections in our capture. The flag `--embed-mode neural` loads `all-MiniLM-L6-v2` from a local cache and runs it on the CPU.

## Two local embedders, one flag apart

XERJ ships two local embedders and picks the lexical one unless you say otherwise. The default resolves to lexical feature hashing: 384 dimensions, no model file, no training and no semantics. The flag `--embed-mode neural` loads `all-MiniLM-L6-v2`, a 6-layer BERT model, and pins execution to the CPU.

Ask the node which one it loaded rather than inferring it from the request shape.

```json
{"data":{"backend":"lexical","dimensions":384,"resumable":true,
  "semantic_contract":"semantic_text-derived-vector.v1","version":1}}
```

Under `--embed-mode neural` the same route answered differently.

```json
{"data":{"backend":"neural","resumable":false,
  "non_resumable_reason":"the neural model is not content-addressed; use a fresh autoindex state after changing it",
  "semantic_contract":"semantic_text-derived-vector.v1","version":1}}
```

## The default path needs no model at all

XERJ compiles the lexical embedder into the binary, so a default node embeds `semantic_text` fields with zero model files and zero downloads. Our network sampler watched a default node for its entire life and counted 0 non-loopback peers across 42 samples covering 3.51 s.

That sampler polls `/proc/net/tcp` every 50 ms. A connection that opens and closes inside one gap escapes it, so cite the number with that limit attached.

The trade is retrieval quality. On the same 8 judged queries, the lexical default scored 0.4167 precision at 3 while the neural embedder scored 0.6667 with recall 1.0.

## What the neural run actually measured

Our capture started the neural node once with `HF_HOME` pointed at a run-local directory. The first 20-document `_bulk` call took 0.571 s including model load. A second `_bulk` of the same size took 0.245 s from cache.

The node's whole life was 2.252 s.

Read those two numbers as an observation, not a rate. Two single `_bulk` calls of 20 documents each is not a throughput measurement. Other workloads shared this host throughout the run.

## No cold-download time exists in this capture

The `all-MiniLM-L6-v2` model was already in this machine's shared HuggingFace cache at 88 MB before the run started. Our harness pointed `HF_HOME` at a run-local directory and the model still loaded in under a second. This run therefore never exercised the first-download condition.

This pass publishes no download time. For a planning figure, use the project's own listed throughput weakness of about 15 docs/s on short strings.

## Air-gapped and offline use

XERJ downloads the neural model from HuggingFace Hub on first use and caches it after that. Set `embedding.local_model_dir` to a directory you populated yourself when the node has no outbound network.

One security note belongs beside that instruction. XERJ pins no checksum on the Hub download, so the integrity guarantee is TLS plus the Hub, not a digest that XERJ verifies for you.

## Changing embedders is not free

XERJ refuses a backend or model swap on a populated `semantic_text` index rather than mixing two vector spaces in one field. The identity route answered `"resumable": false` under neural mode for the same reason.

Plan a fresh index and a fresh `--state-dir` whenever you move between lexical and neural. A single-node XERJ holds no second copy. Run the switch against a new prefix, then delete the old index after the new one answers.

## What this capture does not show

This pass measured one node on one shared host with the model already cached. The timings carry the host's 1-minute load average and are not a benchmark. No first-download figure, no documents-per-second rate and no multi-node result exists in this run.
