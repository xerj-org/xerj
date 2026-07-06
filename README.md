# XERJ

**A truly-open, Elasticsearch-wire-compatible search, vector, and log engine — written in Rust.**

[![CI](https://github.com/xerj-org/xerj/actions/workflows/ci.yml/badge.svg)](https://github.com/xerj-org/xerj/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](./LICENSE)
[![Version](https://img.shields.io/badge/version-1.0.0--rc.1-orange.svg)](https://github.com/xerj-org/xerj/releases)
[![Rust](https://img.shields.io/badge/Rust-stable-000000.svg?logo=rust)](https://www.rust-lang.org/)
[![ES conformance](https://img.shields.io/badge/ES%20conformance-1326%2F1329-brightgreen.svg)](https://xerj.org/benchmarks)

XERJ speaks the Elasticsearch REST wire protocol, so your existing ES clients, dashboards, and tooling talk to it unchanged. Under the hood it combines full-text search (BM25), dense-vector kNN (HNSW), aggregations, and log analytics in a single native binary — no JVM, sub-second cold start. It is released under **Apache-2.0**, a genuinely open license, as an alternative to Elasticsearch's move to SSPL.

---

## Why XERJ

- **Drop-in ES wire compatibility.** Implements the Elasticsearch REST surface — index/document APIs, `_search`, `_bulk`, aggregations, kNN, scroll, aliases, templates. Validated against **1,326 of 1,329** ES-YAML conformance cases extracted from Elasticsearch 8.13 source.
- **Truly open.** Apache-2.0 licensed. No SSPL, no source-available asterisks, no per-feature license gates.
- **One engine, four workloads.** Full-text BM25, vector kNN over HNSW, the standard aggregation suite, and columnar log analytics — all in the same process, over the same wire protocol.
- **A single native binary.** Roughly a ~23 MB statically-linked executable. Sub-second start, no JVM, no heap-tuning ritual.
- **Honest, reproducible benchmarks.** XERJ is measured head-to-head against Elasticsearch 8.13 across a 91-cell matrix. Results — wins *and* losses — are published at [xerj.org/benchmarks](https://xerj.org/benchmarks) and reproducible from the scripts in this repo.
- **Written in Rust.** Memory-safe, `panic = "abort"`, fat-LTO release builds. Embedded Raft consensus (no external Raft dependency) for cluster metadata.

---

## Quickstart

> The Cargo workspace lives in the `engine/` directory. Run build and test commands from there.

### Build

```bash
git clone https://github.com/xerj-org/xerj.git
cd xerj/engine
cargo build --release -p xerj-server
```

### Run

```bash
# Insecure mode: no TLS, no auth — great for local development
./target/release/xerj --data-dir ./data --insecure
```

XERJ listens on `http://0.0.0.0:9200` — the same default port as Elasticsearch.

```bash
curl http://localhost:9200
```

### A copy-pasteable walkthrough

**1. Create an index** with a text field and a 3-dimensional vector field:

```bash
curl -X PUT http://localhost:9200/articles -H 'Content-Type: application/json' -d '{
  "mappings": {
    "properties": {
      "title":     { "type": "text" },
      "category":  { "type": "keyword" },
      "views":     { "type": "integer" },
      "embedding": { "type": "dense_vector", "dims": 3 }
    }
  }
}'
```

**2. Index documents via the Bulk API:**

```bash
curl -X POST http://localhost:9200/_bulk -H 'Content-Type: application/x-ndjson' -d '
{ "index": { "_index": "articles", "_id": "1" } }
{ "title": "Rust for search engines", "category": "engineering", "views": 1200, "embedding": [0.10, 0.20, 0.30] }
{ "index": { "_index": "articles", "_id": "2" } }
{ "title": "Vector search explained", "category": "engineering", "views": 850, "embedding": [0.11, 0.19, 0.28] }
{ "index": { "_index": "articles", "_id": "3" } }
{ "title": "Scaling log analytics", "category": "ops", "views": 430, "embedding": [0.90, 0.10, 0.05] }
'
```

**3. Full-text search** with BM25 scoring:

```bash
curl -X POST http://localhost:9200/articles/_search -H 'Content-Type: application/json' -d '{
  "query": { "match": { "title": "search" } }
}'
```

**4. An aggregation** — total views per category:

```bash
curl -X POST http://localhost:9200/articles/_search -H 'Content-Type: application/json' -d '{
  "size": 0,
  "aggs": {
    "by_category": {
      "terms": { "field": "category" },
      "aggs": { "total_views": { "sum": { "field": "views" } } }
    }
  }
}'
```

**5. kNN vector search** (ES 8.x top-level `knn`):

```bash
curl -X POST http://localhost:9200/articles/_search -H 'Content-Type: application/json' -d '{
  "knn": {
    "field": "embedding",
    "query_vector": [0.10, 0.20, 0.29],
    "k": 2,
    "num_candidates": 10
  }
}'
```

Every request and response above follows the Elasticsearch shape, so the same calls work through the official ES client libraries.

---

## Elasticsearch compatibility

XERJ implements the Elasticsearch REST wire protocol. Because it is wire-compatible, existing ES client libraries and Kibana-style tooling can point at XERJ without code changes.

**Document & index APIs**

- `PUT` / `GET` / `DELETE /{index}/_doc/{id}`, `POST /{index}/_update/{id}`
- `POST /_bulk` with `index`, `create`, `update`, and `delete` actions
- `POST /{index}/_delete_by_query`
- Index creation with mappings, `PUT /_index_template/{name}`, `POST /_aliases` (`add` / `remove`)
- Scroll pagination: `POST /{index}/_search?scroll=1m`, `POST /_search/scroll`

**Search** — `POST /{index}/_search` accepts a standard ES request body with `query`, `from`, `size`, `sort`, `aggs`, `_source`, and `highlight`. Comma-separated multi-index and wildcard index patterns are supported.

**Supported query types**

`match_all`, `match_none`, `match`, `match_phrase`, `match_phrase_prefix`, `multi_match`, `term`, `terms`, `range`, `prefix`, `wildcard`, `exists`, `ids`, `bool`, `fuzzy`, `regexp`, `query_string`, `simple_query_string`, `constant_score`, `boosting`, `dis_max`, `geo_distance`, `knn`, `semantic`, `hybrid`.

> **Vector & semantic search notes.** `knn` (dense-vector HNSW) and `hybrid` (BM25 + kNN in one request) run entirely inside XERJ. The `semantic` query resolves query text to a vector at search time via an **external, OpenAI-compatible `/v1/embeddings` endpoint** that you configure (`embedding.default_endpoint` / `embedding.default_model`) — XERJ ships the proxy, not a built-in embedding model. Built-in/local embeddings, auto-embed-on-ingest, and an agent-memory API are on the [roadmap](#roadmap).

**Supported aggregations**

`terms`, `stats`, `avg`, `sum`, `min`, `max`, `value_count`, `cardinality`, `range`, `histogram`, `date_histogram`, `percentiles`, `filter`, `missing`, `composite`.

The [ES-YAML conformance suite](#running-the-conformance-tests) — 1,329 cases across search, aggregations, vectors, bulk, indices, scroll, and cluster suites — is the source of truth for compatibility. XERJ currently passes 1,326 of them.

---

## Benchmarks

XERJ is benchmarked head-to-head against **Elasticsearch 8.13** across a 91-cell matrix covering ingest, full-text search, aggregations, and vector search. The methodology and the full results — including the cases where Elasticsearch wins — are published at **[xerj.org/benchmarks](https://xerj.org/benchmarks)**.

The benchmarks are reproducible: the harness and playbooks live under [`demo/playbooks`](./demo/playbooks) in this repository. We publish results warts-and-all rather than cherry-picking; treat any number you cannot reproduce with skepticism.

---

## Architecture / project layout

XERJ is a Cargo workspace under [`engine/`](./engine). The crates:

| Crate | Purpose |
|---|---|
| `xerj-server` | Binary entry point: CLI parsing, config loading, starts the API. |
| `xerj-api` | Axum HTTP layer — ES-compatible REST handlers (`es_compat.rs`) and the native API. |
| `xerj-engine` | Integration crate: the `Engine` and `Index` types that tie everything together. |
| `xerj-query` | Query DSL — AST, ES JSON parser, planner, rewriter, and executor. |
| `xerj-storage` | WAL, segments, version map, and the index store. |
| `xerj-fts` | Full-text search: BM25 scoring, analyzer registry, postings lists. |
| `xerj-vector` | Dense-vector HNSW index for kNN / semantic search. |
| `xerj-logs` | Columnar log ingestion and retention. |
| `xerj-ai` | Text chunking and an embedding proxy (external OpenAI-compatible API) that powers the `semantic` query. Also houses an experimental memory store not yet exposed over the REST API — see the [roadmap](#roadmap). |
| `xerj-compress` | Block compression codecs (LZ4, Zstd). |
| `xerj-common` | Shared types: `Config`, `Schema`, `FieldType`, `XerjError`. |
| `xerj-cluster` | Embedded Raft consensus for cluster metadata (no external Raft dependency). |
| `xerj-wasm` | Trait-based document-transform pipeline with an optional WASM backend. |
| `xerj-console-api` | Backend for the Xerj Console (dashboards, auth, cluster awareness). |

A search request flows: **HTTP → `xerj-api` (Axum) → `xerj-query` parse → `Engine::get_index` → `Index::search` (memtable + segment BM25, term/geo matching, aggregations, source filtering, highlighting) → JSON response.** On restart, the engine replays the WAL to rebuild in-memory state.

---

## Building from source

Requirements: a stable Rust toolchain.

```bash
cd engine

# Just the server binary
cargo build --release -p xerj-server

# The whole workspace
cargo build --release --workspace
```

Run the unit and integration tests:

```bash
cd engine
cargo test --workspace
```

---

## Running the conformance tests

The ES-YAML runner replays test cases extracted from Elasticsearch 8.13 against a live XERJ instance. Start the server, then run the suite:

```bash
# Terminal 1 — start XERJ
./target/release/xerj --insecure --data-dir /tmp/xerj-test

# Terminal 2 — run all 1,329 cases
cd engine
cargo run -p es-yaml-runner -- --dir tests/es-compat-yaml/yaml --verbose

# Or a single suite
cargo run -p es-yaml-runner -- --dir tests/es-compat-yaml/yaml/search
cargo run -p es-yaml-runner -- --dir tests/es-compat-yaml/yaml/aggregations
cargo run -p es-yaml-runner -- --dir tests/es-compat-yaml/yaml/vectors
```

If a test expects a response and XERJ returns something different, that's a bug in XERJ — not an accepted divergence.

---

## Roadmap

XERJ ships full-text, aggregation, log-analytics, dense-vector kNN, and hybrid search today. Several AI-adjacent capabilities are **planned but not yet implemented** — they are called out here (and in [`ROADMAP.md`](./ROADMAP.md)) so the README stays honest about what is and isn't built:

- **Built-in / local embeddings & auto-embed on ingest.** Today, `semantic` search proxies to an external OpenAI-compatible embeddings endpoint you configure; there is no bundled embedding model, and the `semantic_text` field type is a mapping placeholder that does not yet auto-embed on ingest.
- **Agent-memory API.** An internal memory store exists in `xerj-ai` but is not yet wired to a REST endpoint (store/recall/forget over the wire).
- **Anomaly detection / ML jobs.** The Elasticsearch `_ml` / `_cat/ml/*` surface is present as compatibility stubs only; there is no detection or forecasting engine behind it yet.

See [ROADMAP.md](./ROADMAP.md) for the full list and status.

---

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](./CONTRIBUTING.md) for how to build, test, and structure changes. A few conventions:

- Changes land on a task-named branch with a descriptive commit body (motivation, what changed, and — for performance work — before/after numbers).
- Run the full ES-YAML conformance suite before opening a release-bound PR; a test that passed yesterday and fails today is a P0 regression.
- Keep the wire protocol honest: match Elasticsearch's behavior rather than inventing new semantics.

---

## License

XERJ is licensed under the **Apache License 2.0**. See [LICENSE](./LICENSE) for the full text.

---

## Links

- Website: **[xerj.org](https://xerj.org)**
- Benchmarks: **[xerj.org/benchmarks](https://xerj.org/benchmarks)**
- Source: **[github.com/xerj-org/xerj](https://github.com/xerj-org/xerj)**
