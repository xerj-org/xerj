# XERJ vs Elasticsearch — head-to-head benchmark

Identical workload, same machine (localhost), single node, security off. End-to-end client wall-clock latency. Corpus: real LLM-telemetry, **1,000,000 docs**.

## Ingest (bulk, 10,000/batch)

| Engine | docs | wall | throughput |
|---|--:|--:|--:|
| XERJ | 1,000,000 | 10.4s | **96,493 docs/s** |
| Elasticsearch | 1,000,000 | 9.7s | **102,982 docs/s** |

_XERJ ingest is **0.94×** slower than Elasticsearch._

## Read-path latency (ms, end-to-end, 80 iters after warmup)

| operation | XERJ p50 | Elasticsearch p50 | XERJ p95 | Elasticsearch p95 | XERJ p99 | Elasticsearch p99 | p50 ratio |
|---|--:|--:|--:|--:|--:|--:|--:|
| match_all (size 10) | 1.55 | 3.07 | 1.99 | 4.98 | 2.22 | 5.66 | 1.98× |
| term filter | 1.55 | 3.01 | 1.85 | 7.74 | 2.16 | 24.26 | 1.94× |
| bool must+filter | 1.52 | 3.03 | 2.06 | 4.78 | 60.39 | 6.19 | 1.99× |
| range | 1.50 | 3.01 | 1.66 | 4.26 | 1.97 | 8.18 | 2.00× |
| agg: terms(model) | 1.40 | 2.32 | 1.65 | 3.33 | 4.46 | 4.67 | 1.66× |
| agg: stats(latency_ms) | 1.32 | 2.38 | 1.83 | 4.73 | 31.82 | 5.27 | 1.80× |
| agg: date_histogram(day) | 1.26 | 2.68 | 1.83 | 3.90 | 123.19 | 4.90 | 2.12× |
| agg: terms+avg(cost) | 1.31 | 2.24 | 1.39 | 3.54 | 1.50 | 3.92 | 1.71× |
| agg: cardinality(top_doc) | 1.27 | 2.39 | 1.42 | 3.51 | 1.73 | 4.94 | 1.88× |
| _count match_all | 1.22 | 2.23 | 1.36 | 3.13 | 8.39 | 3.78 | 1.83× |
| kNN k=10 (20k×16d) | 1.36 | 3.05 | 1.80 | 4.59 | 2.30 | 4.70 | 2.24× |

_p50 ratio = Elasticsearch p50 ÷ XERJ p50; >1 means XERJ is faster._
