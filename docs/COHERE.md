# Cohere models on XERJ

Enterprise teams that run search on their own hardware keep asking the same
thing: index our files and databases here, keep the data here, and put Cohere
models on top. This page is the reference for doing that with XERJ.

XERJ is not affiliated with Cohere. "Cohere", "Command", "Embed" and "Rerank"
are Cohere's product names, used here only to say which API a setting talks to.

## What you get

| Piece | What it does | Status |
|---|---|---|
| Rerank provider `cohere` on `_search` | XERJ sends the top hits to Cohere Rerank and returns them in Cohere's order | on main, ships in rc.102 ([#2985](https://github.com/xerj-org/xerj/pull/2985)); stub-tested, live run pending |
| Embed through the proxy embedder | `semantic_text` fields get Cohere vectors through Cohere's OpenAI-compatible route | on main, ships in rc.102 ([#2983](https://github.com/xerj-org/xerj/pull/2983)); contract-tested, live run pending |
| Command tool schema | `cohere-tools.json`: six XERJ search and memory tools in the Chat v2 tool format | on main, ships in rc.102 ([#2983](https://github.com/xerj-org/xerj/pull/2983)); generated, live run pending |
| Command RAG cookbook | hybrid search, Rerank, then `/v2/chat` with citations that resolve to the source file and page | on main, ships in rc.102 ([#2983](https://github.com/xerj-org/xerj/pull/2983)); replay-tested, live run pending |
| MCP server | any MCP client can call XERJ search, memory and XERJ Query-1 | shipped (stdio). StreamableHTTP transport is on the roadmap |
| Native `/v2/embed` (`input_type`, int8/binary vectors) | asymmetric query/document vectors inside XERJ | roadmap |

"Live run pending" means: every request and response shape was tested against
a mock of Cohere's documented API, and no call to Cohere's real API has been
made yet. Nothing on this page claims a Cohere accuracy, latency or cost figure.

Everything is off by default. A stock XERJ node never calls Cohere.

## What leaves your machine

| Setting | What is sent | Where |
|---|---|---|
| `rerank: {"provider": "cohere"}` on a search | the query text, the model name, and the text of up to `window` hits | `https://api.cohere.com/v2/rerank`, or the `endpoint` you set |
| proxy embedder with the Cohere config | the text of every field it embeds, at write time and at query time | `https://api.cohere.ai/compatibility/v1/embeddings`, or the `default_endpoint` you set |
| the RAG cookbook script | query text, hit text, and the question | the `CO_BASE_URL` you set |

To keep all of it inside your network, point these settings at a private
Cohere deployment. We have not tested a private deployment yet.

## 1. Rerank on `_search`

```toml
# xerj.toml
[rerank.cohere]
api_key  = "co-..."                             # or COHERE_API_KEY
endpoint = "https://api.cohere.com/v2/rerank"   # or COHERE_ENDPOINT; private deployments go here
```

```sh
curl -s -XPOST localhost:9200/docs/_search -H 'content-type: application/json' -d '{
  "query": {"match": {"body": "claims appeal deadline"}},
  "rerank": {"provider": "cohere", "model": "rerank-v5.0-fast", "window": 50}
}'
```

Verify: `curl -s localhost:9200/_xerj/rerank` lists `cohere` with
`"configured": true`. The key is never printed.

Models accepted: `rerank-v5.0-fast` (default), `rerank-v5.0-pro`,
`rerank-v3.5`. Any other name is refused with HTTP 400 before anything is sent.
`instructions` and `min_score` are refused with HTTP 400 for this provider.
Up to 100 documents go to Cohere per call. Each response reports Cohere's
billed search units under `_rerank.usage.search_units`.

## 2. Embed through the proxy embedder

```toml
[embedding]
mode = "proxy"
default_endpoint = "https://api.cohere.ai/compatibility/v1"
default_model = "embed-v5.0-pro"
batch_size = 64
timeout_ms = 30000
```

```sh
export XERJ_EMBEDDING_API_KEY="$CO_API_KEY"
xerj --config cohere-embed.toml
```

Verify: the startup log names the backend; expect a line containing
`embedding backend` and `compatibility/v1/embeddings`.

Limits: the compatibility route takes no `input_type`, `dimensions` or
`truncate`, so query and document vectors are embedded the same way. For
asymmetric vectors today, call `/v2/embed` yourself and write the vectors to a
`dense_vector` field with `_bulk` (the cookbook does this).

## 3. Command tool use and RAG with citations

`landing/docs/agents/schemas/cohere-tools.json` holds `xerj_search`,
`xerj_semantic_search`, `xerj_vector_search`, `xerj_hybrid_search`,
`xerj_memory_store` and `xerj_memory_recall` in the Chat v2 envelope. It is
generated from `openai-tools.json`:

```sh
python3 tools/agents/gen_cohere_tools.py --check   # expect: ok: ... matches ...
```

The cookbook in `docs/examples/cohere/command-rag/` indexes autoindexed files
with Cohere vectors, runs a hybrid query, reranks, sends the best hits to
`/v2/chat` as documents whose `id` is the XERJ `_id`, and maps every citation
back to a file and page:

```sh
python3 docs/examples/cohere/command-rag/command_rag.py ask "What is the appeal deadline?" --assert-citations
```

Verify: the command prints the answer, one `file page N (_id ...)` line per
citation, and exits 0.

Test it without a key: `bash docs/examples/cohere/command-rag/run-offline-test.sh`
(expected last line: `ALL OK (replay-tested against a mock; no live Cohere call was made)`).

## 4. What XERJ adds under Cohere models

- **Your data, connected once.** `xerj autoindex` for files and folders, and
  `xerj connect postgres://...` for a live database (on main, ships in rc.102,
  [#2984](https://github.com/xerj-org/xerj/pull/2984)). See [SOURCES.md](./SOURCES.md)
  and [LIVE_SOURCES.md](./LIVE_SOURCES.md) once those land.
- **Hybrid retrieval in one engine.** BM25, kNN over Cohere vectors and RRF
  fusion in one `_search`, Elasticsearch-compatible, one binary, CPU only.
- **Exact answers without a model call.** For questions over structured facts
  (a table, a ledger, a policy register), XERJ Query-1 compiles the question
  into a program and runs it over the facts, with evidence, and says UNKNOWN
  instead of guessing. Use it next to Command, not instead of it. See
  [XERJ_QUERY_1.md](./XERJ_QUERY_1.md).

## Not verified yet

These need one run with a real Cohere key:

- the exact citation id form Command returns;
- whether the compatibility embed response carries a per-item `index`;
- whether Command accepts all six generated tool schemas;
- the default vector width of `embed-v5.0-pro`;
- any behaviour of a private Cohere deployment.

A worked example on public data, with measured numbers for the XERJ side:
[xerj.org/use-cases/enterprise-on-prem-cohere](https://xerj.org/use-cases/enterprise-on-prem-cohere).
