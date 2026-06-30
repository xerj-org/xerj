# Competitor Benchmarks vs Elasticsearch

## Severity: INFORMATIONAL | Frequency: GROWING

---

## Vector Search Benchmarks

| Metric | Elasticsearch | Milvus | Qdrant | Vespa.ai |
|--------|--------------|--------|--------|----------|
| Latency (1M vectors) | ~200ms | ~6ms (30x) | ~10ms (20x) | ~22ms (9x) |
| QPS | ~1,900 | ~6,000 (3x) | ~5,000 (2.6x) | ~17,000 (9x) |
| Data loading speed | baseline | 15x faster | 5x faster | N/A |
| Max dimensions | 4,096 | 32,768 | 65,535 | unlimited |
| Hybrid search latency | baseline | N/A | N/A | 5x faster |
| Lexical search | baseline | N/A | N/A | 3x faster |

## Log Analytics Benchmarks

| Metric | Elasticsearch | ClickHouse | Loki | OpenObserve |
|--------|--------------|------------|------|-------------|
| Analytics query speed | baseline | 10-100x faster | N/A | N/A |
| Compression ratio | ~1.3x (ZSTD) | 2x+ better | N/A | 140x lower storage |
| Cost (equivalent workload) | baseline | 4x cheaper | 10x+ cheaper | 100x+ cheaper |
| SQL support | Limited (ES\|QL) | Full SQL | LogQL | SQL |

## General Search Benchmarks

| Metric | Elasticsearch | Typesense | Meilisearch |
|--------|--------------|-----------|-------------|
| Cost (equivalent) | baseline | 99% cheaper | 97% cheaper |
| Setup complexity | High | Low | Low |
| Docker image | ~800MB | <50MB | <100MB |
| Cold start | 30-60s | <5s | <5s |

## Resource Consumption

| Metric | Elasticsearch | Alternative |
|--------|--------------|-------------|
| RAM baseline | ~4.5GB | ~53MB (85x less) |
| RAM per 1M docs | 2-4GB | <500MB target |
| Docker image | ~800MB | ~30MB (Rust binary) |
| Files per segment | 12-16 | 2 (XERJ.ai target) |
| Config parameters | ~3,000+ | <50 (XERJ.ai target) |

---

## What This Means for XERJ.ai

XERJ.ai targets benchmarks competitive with the BEST in each category:
- Vector: Qdrant/Milvus-class latency (Rust + SIMD, same league)
- Logs: ClickHouse-class compression (domain-aware codec)
- Search: Typesense-class simplicity (single binary, minimal config)
- Cost: 80% reduction target is conservative based on competitor results

The unique advantage: ALL THREE in one engine. No competitor offers unified
FTS + vector + log analytics with AI-native features.

## Sources
- Zilliz: ES vs Milvus benchmarks
- Capella Solutions: Vector DB vs ES
- Vespa.ai: Elasticsearch Alternative benchmarks
- ClickHouse: Elastic for Observability comparison
- SigNoz: Loki vs Elasticsearch
- OpenObserve: Elasticsearch Alternatives
- Medium: The Day Our ES Bill Hit $8,000 (Typesense comparison)
