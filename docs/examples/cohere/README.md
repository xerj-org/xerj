# Cohere models on XERJ

Teams that run search on their own hardware asked how to put Cohere models on
top of the files XERJ already indexes. This folder has three pieces:

| Piece | What it does | Status |
|---|---|---|
| [`cohere-embed.toml`](./cohere-embed.toml) | XERJ calls Cohere Embed for `semantic_text` fields through Cohere's OpenAI-compatible route | contract-tested against a mock; live run pending |
| [`command-rag/`](./command-rag/) | Cohere Command answers over autoindexed files; every citation resolves to a file and page | replay-tested against a mock; live run pending |
| [`/docs/agents/schemas/cohere-tools.json`](../../../landing/docs/agents/schemas/cohere-tools.json) | the six XERJ tools in the Cohere Chat v2 tool format | generated from `openai-tools.json`; live run pending |

Everything is off by default. Nothing in a stock XERJ node talks to Cohere.

## What leaves your machine

When you use these, the text you embed, rerank or ask about is sent to the
Cohere endpoint you configure (default `https://api.cohere.com`, or
`https://api.cohere.ai/compatibility/v1` for the embedder). Point
`CO_BASE_URL` / `default_endpoint` at a private Cohere deployment to keep it
inside your network. We have not tested a private deployment.

## 1. Embed: Cohere vectors for `semantic_text`

```sh
export XERJ_EMBEDDING_API_KEY="$CO_API_KEY"
xerj --config docs/examples/cohere/cohere-embed.toml
```

Verify: the server's startup output names the backend. Expect a line
containing `embedding backend` and `api.cohere.ai/compatibility/v1/embeddings`.

Limits of this route, from Cohere's compatibility docs:

- No `input_type`, `dimensions` or `truncate`. Query and document vectors are
  embedded the same way. Cohere's native `/v2/embed` distinguishes
  `search_query` from `search_document`; XERJ does not call it yet (roadmap).
- The vector width is the model default. Record it from your first live run
  (`GET /<index>/_mapping`); we have not measured it.
- Cohere accepts up to 96 texts per call; the config sends 64.

Want asymmetric query/document vectors today? Call `/v2/embed` yourself, write
the vectors to a `dense_vector` field with `_bulk`, then query with `knn` or a
hybrid RRF query. The cookbook below does exactly that.

## 2. Command RAG with citations back to files

```sh
xerj autoindex ./my-pdfs --prefix docs              # one record per page/section
export CO_API_KEY=...                                 # Cohere key
python3 docs/examples/cohere/command-rag/command_rag.py index --source docs-docs
python3 docs/examples/cohere/command-rag/command_rag.py ask "What changed in the Q3 policy?" --assert-citations
```

Verify: the last command prints the answer, then one `file page N (_id ...)`
line per citation and exits 0. `--assert-citations` exits 1 if any citation
does not resolve to a stored record.

How it works:

1. `index` reads the records autoindex wrote, embeds their text with
   `/v2/embed` (`input_type=search_document`) and writes
   `{body, title, path, page, vec}` to a new index with the same `_id`.
2. `ask` embeds the question (`input_type=search_query`), runs one XERJ hybrid
   query (BM25 on `body` + kNN on `vec`, fused with RRF), reranks the top 30
   with `/v2/rerank`, and sends the best 5 to `/v2/chat` as `documents` whose
   `id` is the XERJ `_id`.
3. Each citation is mapped back to a record with `GET /{index}/_doc/{id}`. The
   script accepts both forms Cohere may return: the custom id it was given, or
   a positional `doc:N`.

Rerank runs in the script, on the client. A server-side Cohere rerank stage on
`_search` is a separate change and is not used here.

Models (env overrides `CO_EMBED_MODEL`, `CO_RERANK_MODEL`, `CO_CHAT_MODEL`):
`embed-v5.0-pro`, `rerank-v5.0-fast`, `command-a-plus-05-2026`, as listed on
Cohere's models page on 2026-11-05. Check the page before use.

## 3. Tool schema for Command

`cohere-tools.json` holds `xerj_search`, `xerj_semantic_search`,
`xerj_vector_search`, `xerj_hybrid_search`, `xerj_memory_store` and
`xerj_memory_recall` in the Chat v2 envelope
`{"type":"function","function":{name, description, parameters}}`. It is
generated, never edited by hand:

```sh
python3 tools/agents/gen_cohere_tools.py --check   # expect: ok: ... matches ...
```

The generator drops `additionalProperties`, moves `default`, `minimum`,
`maximum` and item bounds into the description, and keeps the first type of a
union (`sort` is given as an array; the description says the API also accepts
an object or a string).

## Test it without a key

```sh
bash docs/examples/cohere/command-rag/run-offline-test.sh
```

It starts a mock Cohere API and a throwaway XERJ node on a private port,
autoindexes the public PDFs in `landing/resources`, runs `index` and
`ask --assert-citations` for both citation forms, then checks that the proxy
embedder handles the compatibility route's response, which carries no
per-item `index`. Expected last line:
`ALL OK (replay-tested against a mock; no live Cohere call was made)`.

## Not verified yet

These need one run with a real key. Until then, treat them as unconfirmed:

- Whether the compatibility response includes `index` per item (XERJ handles
  both; the mock omits it).
- Whether citations echo our custom document ids or positional ids (the script
  handles both).
- Whether Cohere accepts all six tool schemas as generated.
- The default vector width of `embed-v5.0-pro`.

No accuracy, latency or cost figure for Cohere models is claimed here.
