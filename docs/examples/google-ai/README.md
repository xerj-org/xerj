# XERJ + Google AI

Worked, verified examples of using XERJ with Google's AI stack. XERJ speaks the
OpenAI-compatible `/v1/embeddings` contract and the Elasticsearch REST/MCP
surface, which is exactly what Google's embedding API and agent framework plug
into. Every claim below is labelled with how it was checked.

## Integration matrix

| Google product | How XERJ uses it | Verified here |
|---|---|---|
| **EmbeddingGemma** (open model, `google/embeddinggemma-300m`) | Local, private embeddings for `semantic_text`, served by Ollama's OpenAI endpoint | ✅ **end-to-end** — see numbers below |
| **Gemini Embeddings API** (`gemini-embedding-001`) | Managed embeddings for `semantic_text` via the OpenAI-compatible endpoint | ✅ config matches Google's docs; contract proven by the offline test + EmbeddingGemma run |
| **ADK** (Agent Development Kit) | XERJ as the retrieval *tool* of a Gemini-powered ADK agent | ✅ agent builds; tool verified returning cited answers from XERJ |

Two truths worth stating plainly: XERJ uses **one embedder for both ingest and
query**, so vectors are comparable by construction (the #1 cause of broken RAG
is indexing with model A and querying with model B). And it needs **no separate
vector database** — the same engine does BM25, kNN, and hybrid fusion.

---

## 1. EmbeddingGemma (Google's open model) — fully local, verified

`google/embeddinggemma-300m` is Google's open, on-device embedding model (Gemma
3 architecture, 100+ languages, Matryoshka dims). Serve it locally with Ollama's
OpenAI-compatible endpoint and point XERJ at it — no key, no cloud, data never
leaves the machine.

```bash
ollama pull embeddinggemma
xerj --insecure --data-dir ./data --config embeddinggemma.toml
```

`embeddinggemma.toml` points XERJ's proxy at `http://127.0.0.1:11434/v1/embeddings`,
model `embeddinggemma`.

**Verified end-to-end in this repo.** Four docs indexed through EmbeddingGemma,
then three semantic queries with *no shared words* with their answers:

| query | top match | score |
|---|---|--:|
| "database outage from too many open connections" | "…connection pool was exhausted…" | 0.878 |
| "when do young cats get their shots" | "kittens should receive their first vaccination…" | 0.876 |
| "how much did earnings go up" | "quarterly revenue grew twelve percent…" | 0.902 |

That's real neural understanding — the words don't overlap, the meaning does.

## 2. Gemini Embeddings API — managed

To use Google's hosted `gemini-embedding-001` instead, swap the config:

```bash
export XERJ_EMBEDDING_API_KEY="$GEMINI_API_KEY"
xerj --insecure --data-dir ./data --config gemini-embeddings.toml
```

`gemini-embeddings.toml` uses the endpoint from Google's OpenAI-compatibility
docs — `https://generativelanguage.googleapis.com/v1beta/openai/embeddings`,
model `gemini-embedding-001`. XERJ sends the key as `Authorization: Bearer`,
which is exactly what that endpoint expects.

> Verified structurally: the config matches Google's documented contract, and
> that contract is proven working by `run-offline-test.sh` (a mock speaking the
> identical shape) and by the EmbeddingGemma run above (same OpenAI contract via
> Ollama). Running against the managed API additionally needs a valid
> `GEMINI_API_KEY`; endpoints and model names change, so check Google's current
> docs.

## 3. ADK agent — Gemini reasons, XERJ retrieves

`adk_xerj_agent.py` is a Google ADK `Agent` (model `gemini-2.5-flash`) whose
`search_documents` tool queries XERJ and returns cited snippets. The model
decides when to search; XERJ supplies the grounded facts.

```bash
pip install google-adk
python3 adk_xerj_agent.py        # runs the tool against XERJ (no key needed)
```

**Verified:** the agent constructs (`doc_assistant`, tool `search_documents`),
and the tool returns real cited results from the EmbeddingGemma-backed index —
e.g. the "connection pool exhausted" doc at score 0.878. Driving the full
Gemini reasoning loop (`adk run` / `Runner`) additionally needs a
`GOOGLE_API_KEY`; the XERJ integration — the part this example is about — does
not.

## Offline, key-free check of the whole path

```bash
docs/examples/google-ai/run-offline-test.sh
```

Starts a mock `/v1/embeddings` server, runs XERJ in proxy mode, and shows the
external API is called at ingest *and* at query time — proving the wiring
without any account.

---

*Sources for the Google-side facts:*
[Gemini Embedding GA](https://developers.googleblog.com/gemini-embedding-available-gemini-api/) ·
[Gemini OpenAI compatibility](https://ai.google.dev/gemini-api/docs/openai) ·
[EmbeddingGemma](https://developers.googleblog.com/introducing-embeddinggemma/) ·
[`google/embeddinggemma-300m`](https://huggingface.co/google/embeddinggemma-300m) ·
[ADK](https://google.github.io/adk-docs/)
