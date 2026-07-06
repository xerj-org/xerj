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

## Landed in 1.0.0-rc.2

These three shipped in rc-2 (each conformance-gated at 1,326 / 1,329 and verified by real requests). Honest limitations are noted.

### 1. Auto-embed on ingest + a built-in embedder ✅ (rc-2)

`semantic_text` now works end to end with **zero external configuration**. Indexing a document into a `semantic_text` field auto-embeds its text (previously returned `405`), and the `semantic` query embeds the query text with the same embedder and runs kNN — no external service required. A configured external `/v1/embeddings` proxy is still used, at higher quality, when `embedding.default_endpoint` is set.

- **Limitation:** the built-in embedder is a deterministic **lexical** model (feature-hashed word unigrams + character trigrams, L2-normalized) — it captures vocabulary/sub-word overlap, not deep semantics. Paraphrases that share vocabulary rank correctly; truly-synonymous text with no word overlap will not. For production-grade semantics, configure the external proxy. A bundled neural model remains future work.

### 2. Agent-memory REST API ✅ (rc-2)

A namespaced agent-memory API, backed by regular XERJ indices (reusing document + vector + BM25 + metadata-filter paths), working fully offline:
`POST /_memory/{ns}` (store), `POST /_memory/{ns}/_recall` (kNN by vector or BM25 by text, with optional metadata filter + `k`), `GET /_memory/{ns}` (list), `DELETE /_memory/{ns}/{id}` and `DELETE /_memory/{ns}` (forget / drop). Namespaces are physically isolated.

- **Limitation:** recall is pure relevance (kNN/BM25); recency-blended scoring and semantic dedup from the older internal module are not applied (recency ordering is available via the list endpoint). Single-node.

### 3. Anomaly detection (`_ml`) ✅ (rc-2)

A real statistical detector replaces the empty compat stubs:
`PUT /_ml/anomaly_detectors/{id}` (create: source index, time field, function `count|mean|min|max|sum`, bucket span, threshold), `GET` (fetch/list — now returns real jobs), `POST /_ml/anomaly_detectors/{id}/_score` (buckets the source over time, builds a moving mean/stddev baseline, flags buckets deviating beyond the threshold with a normalized anomaly score), `DELETE`.

- **Limitation:** on-demand scoring only — no continuous datafeed scheduler, no forecasting, no influencers/model-plot, single-node config registry. `_cat/ml/datafeeds` and `_cat/ml/trained_models` remain valid empty stubs.

## Planned / in progress

### Neural embeddings & richer ML

- A bundled/local **neural** embedding model (the current built-in embedder is lexical).
- An ingest-time embedding pipeline with the existing text chunker (overlapping chunks → per-chunk vectors).
- Continuous anomaly datafeeds (real-time jobs) and forecasting for capacity/write-load signals.

### Other tracked items

- **Distributed clustering maturity** — embedded Raft handles cluster metadata today; multi-node sharding/replication hardening is ongoing.
- **Broader aggregation coverage** — geo/IP/nested/join/span families are partially covered; see the conformance suite for the current surface.

---

Found something claimed but not working? That is a bug in our docs or our code — please [open an issue](https://github.com/xerj-org/xerj/issues). We would rather ship an honest roadmap than an overstated feature list.
