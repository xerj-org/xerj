# XERJ Recipes

Use-case-driven guides for building real things with XERJ. Every recipe was **verified end-to-end against a live XERJ** before being written — the runnable example for each lives under [`docs/examples/`](../examples).

These are practical "how do I actually do X" recipes, not an API reference (that follows separately).

| Recipe | What you build | Example |
|---|---|---|
| [Give an AI agent long-term memory](./agentic-memory.md) | A memory-backed agent using the `/_memory` API (store, semantic + keyword recall, metadata filters, forgetting, per-agent isolation) | [`agent_memory.py`](../examples/agentic-memory/agent_memory.py) |
| [Semantic search & RAG](./semantic-search-rag.md) | Retrieval by meaning with `semantic_text` (auto-embed on ingest, no separate vector DB) | [`rag_demo.py`](../examples/semantic-search-rag/rag_demo.py) |
| [Passage retrieval on long docs](./passage-retrieval.md) | `semantic_text` auto-embeds every overlapping passage; a long doc competes on any one of its sections via best-passage (max-sim) scoring — 98% top-3 vs 32% pooled | [`passage_demo.py`](../examples/passage-retrieval/passage_demo.py) |
| [Log analytics](./log-analytics.md) | From raw logs to dashboards — error rates, p95 latency, top services via aggregations | [`log_analytics.py`](../examples/log-analytics/log_analytics.py) |
| [Vector search (kNN)](./vector-search-knn.md) | Nearest-neighbor similarity search over `dense_vector` (HNSW), with filters | [`knn_demo.py`](../examples/vector-search-knn/knn_demo.py) |
| [Vector quantization](./vector-quantization.md) | Opt a `dense_vector` field into scalar8 (`int8_hnsw`) — 4× smaller vectors, recall@10 ≈ 0.99 | [`quant_demo.py`](../examples/vector-quantization/quant_demo.py) |
| [Hybrid search](./hybrid-search.md) | Keyword + vector in one query — results neither BM25 nor kNN finds alone | [`hybrid_search.py`](../examples/hybrid-search/hybrid_search.py) |
| [Anomaly detection](./anomaly-detection.md) | Statistical `_ml` detectors that flag spikes in metrics/logs | [`anomaly_detection.py`](../examples/anomaly-detection/anomaly_detection.py) |
| [Continuous anomaly datafeeds](./continuous-anomaly-datafeeds.md) | A live `_ml` datafeed that re-scores an index on a timer and stores new anomaly records you poll | [`datafeed_demo.py`](../examples/continuous-anomaly-datafeeds/datafeed_demo.py) |
| [Migrate from Elasticsearch](./migrate-from-elasticsearch.md) | Point your existing ES client at XERJ — same wire, change the URL | [`migrate_demo.sh`](../examples/migrate-from-elasticsearch/migrate_demo.sh) |

Every example is stdlib-only Python 3 or plain `curl` — no dependencies to install. Start XERJ (`./target/release/xerj --data-dir ./data --insecure`), then run any example.
