# XERJ Roadmap

This roadmap tracks capabilities that are **planned but not yet fully implemented**, so the project's public claims stay honest about what ships today versus what is coming. Status is verified against the actual code and by real API requests, not aspirational.

Last reviewed: 2026-07-06 (against `v1.0.0-rc.1`).

## Shipping today (for context)

These are implemented and exercised by the test suite / benchmarks:

- Elasticsearch REST wire compatibility (1,326 / 1,329 ES-YAML conformance cases).
- Full-text search (BM25) and all 38 supported query types.
- The standard aggregation suite.
- **Dense-vector kNN** over HNSW (`knn` query and top-level `knn`), at recall parity with Elasticsearch.
- **Hybrid search** — BM25 + kNN combined in a single request (`{"hybrid": {"query": …, "knn": …}}`).
- Columnar log analytics, bulk / scroll / delete-by-query, aliases, index templates.

## Planned / in progress

### 1. Built-in & local embeddings, auto-embed on ingest

**Status: partial — external proxy only.**

Today the `semantic` query resolves query text to a vector at search time by proxying to an **external, OpenAI-compatible `/v1/embeddings` endpoint** that the operator configures (`embedding.default_endpoint`, `embedding.default_model`). Without that configuration, `semantic` returns a clear error. There is **no bundled/local embedding model**, and the `semantic_text` field type is currently a mapping placeholder — it does not yet auto-embed documents on ingest (indexing into a `semantic_text` field is not wired up).

Planned:
- Auto-embed on ingest for `semantic_text` fields (text in → vector stored, transparently).
- An optional bundled/local embedding model so semantic search works with zero external dependencies.
- An ingest-time embedding pipeline with the existing text chunker (overlapping chunks → per-chunk vectors).

### 2. Agent-memory API

**Status: internal module only, not exposed over REST.**

`xerj-ai` contains a memory-store module (entry model + recall), but it is **not wired to any HTTP endpoint** — there is no `store` / `recall` / `forget` REST API today (all memory paths return `404`).

Planned:
- A REST API to store, semantically recall, and expire agent-memory entries per namespace.
- Recall backed by the existing vector index (kNN over stored memories) plus recency/metadata filters.

### 3. Anomaly detection & ML jobs

**Status: compatibility stubs only.**

The Elasticsearch machine-learning surface is present for wire compatibility (`_cat/ml/anomaly_detectors`, `_cat/ml/datafeeds`, `_cat/ml/trained_models` return empty), but there is **no detection or forecasting engine** behind it — creating an anomaly job (`PUT /_ml/anomaly_detectors/{id}`) is not implemented.

Planned:
- Time-series anomaly detection (baseline + deviation scoring) over indexed metrics/logs.
- Optional forecasting for capacity/write-load signals.

### 4. Other tracked items

- **Distributed clustering maturity** — embedded Raft handles cluster metadata today; multi-node sharding/replication hardening is ongoing.
- **Broader aggregation coverage** — geo/IP/nested/join/span families are partially covered; see the conformance suite for the current surface.

---

Found something claimed but not working? That is a bug in our docs or our code — please [open an issue](https://github.com/xerj-org/xerj/issues). We would rather ship an honest roadmap than an overstated feature list.
