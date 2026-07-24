# External embeddings — use any OpenAI-compatible API

XERJ can embed `semantic_text` fields through an **external OpenAI-compatible
`/v1/embeddings` endpoint** instead of its built-in lexical or neural backends.
The same embedder is used at ingest *and* query time, so vectors stay
comparable — you bring the model, XERJ handles indexing, kNN, and hybrid
fusion.

This directory is a **key-free, reproducible proof** of that path.

## Run it

```bash
cargo build --release -p xerj-server        # once, from the repo root
docs/examples/external-embeddings/run.sh
```

Expected: XERJ logs `embedding backend: external proxy`, the mock server logs
one call per document at ingest and one per query, and a semantic search for
*"database outage from too many open connections"* returns the document about
*"the connection pool was exhausted"* — proving the query was embedded by the
same external API and matched by vector similarity.

## How it's wired

`xerj-proxy.toml`:

```toml
[embedding]
mode = "proxy"
default_endpoint = "http://127.0.0.1:8900/v1/embeddings"
default_model = "mock-embed-256"
```

Start XERJ with `--config xerj-proxy.toml` (or set `--embed-mode proxy` and put
the endpoint in your config). The API key is read from the
`XERJ_EMBEDDING_API_KEY` environment variable and sent as
`Authorization: Bearer <key>`.

The wire contract is the standard one:

```
POST {default_endpoint}
{"input": ["text one", "text two"], "model": "<default_model>"}
→ {"data": [{"embedding": [ ... ], "index": 0}, { ... }]}
```

`mock_embed_server.py` implements exactly this in ~40 dependency-free lines, so
the example runs offline with no account.

## Point it at a real provider

Any endpoint that speaks the contract above works — just change
`default_endpoint`, `default_model`, and set `XERJ_EMBEDDING_API_KEY`:

| provider | `default_endpoint` |
|---|---|
| OpenAI | `https://api.openai.com/v1/embeddings` |
| Gemini (OpenAI-compatible mode) | `https://generativelanguage.googleapis.com/v1beta/openai/embeddings` |
| Local (text-embeddings-inference, LM Studio, llama.cpp server, …) | `http://localhost:PORT/v1/embeddings` |

> Verified in this repo against the reproducible mock, which implements the
> exact request/response shape XERJ sends. Real providers that speak the same
> OpenAI `/v1/embeddings` contract are drop-in; confirm your provider's dims
> match your `dense_vector`/`semantic_text` field and set the key. Endpoints
> and model names change over time — check the provider's current docs.

## Why this matters for agents

The most common way to get *wrong* RAG results is an embedder mismatch: you
index with model A and later query with model B, and the vectors are silently
incomparable. XERJ uses the **one configured embedder for both**, so an agent
that indexes a corpus and later searches it gets comparable vectors by
construction — no external inference service to keep in sync, just one endpoint.
