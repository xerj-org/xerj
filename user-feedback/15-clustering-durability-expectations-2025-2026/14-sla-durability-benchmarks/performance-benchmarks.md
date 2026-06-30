# Search & Vector Database Performance Benchmarks 2025-2026

Collected from published benchmarks, independent studies, and vendor reports.
Last updated: April 2026. All searches run fresh against live web sources.

---

## 1. Elasticsearch vs. OpenSearch

### Source: Trail of Bits Independent Benchmark (March 2025)
Testing OpenSearch v2.17.1 vs Elasticsearch v8.15.4 using OpenSearch Benchmark (OSB) Big5 workload over 4 months.

| Metric | OpenSearch v2.17.1 | Elasticsearch v8.15.4 | Winner |
|--------|-------------------|----------------------|--------|
| Big5 geometric mean (p90) | **12.1 ms** | 18.8 ms | OS (1.6x faster) |
| Text queries (p90) | 18.11 ms | **7.47 ms** | ES (2.42x faster) |
| Sorting (p90) | 5.82 ms | 6.14 ms | OS (1.05x faster) |
| Term aggregations (p90) | **104.90 ms** | 354.52 ms | OS (3.38x faster) |
| Range queries (p90) | **1.47 ms** | 1.49 ms | OS (1.02x faster) |
| Date histograms (p90) | **124.79 ms** | 2,064.61 ms | OS (16.55x faster) |
| Vector search (NMSLIB vs Lucene) | **11.3% faster** | baseline | OS wins |
| Vector search (FAISS vs Lucene) | **13.8% faster** | baseline | OS wins |
| Vector search (Lucene vs Lucene) | 258.2% slower | baseline | ES wins |

**Key data points:**
- DP-1: OpenSearch is **1.6x faster** than Elasticsearch on Big5 overall workload (12.1 ms vs 18.8 ms p90)
- DP-2: Elasticsearch is **2.42x faster** on text queries specifically (7.47 ms vs 18.11 ms p90)
- DP-3: OpenSearch is **16.55x faster** on date histogram aggregations (124.79 ms vs 2,064.61 ms)
- DP-4: OpenSearch max outlier ratio: **1,412x** for composite date histogram operation
- DP-5: Elasticsearch max outlier ratio: **43x** for query-string operation
- DP-6: Elasticsearch had outliers in **19 of 98** tasks; OpenSearch in **11 of 98** tasks
- DP-7: Cache artificially inflates date histogram performance by **1,400x** (1-5 ms cached vs 4,163-7,136 ms uncached)

### Source: Elastic-Sponsored ESG Benchmark (2025)
- DP-8: Elasticsearch is **40%–140% faster** than OpenSearch on classic workloads (text search, sorting, aggregations)
- DP-9: Elasticsearch used **37% less disk space** than OpenSearch with default settings
- DP-10: Elasticsearch remained **13% more space efficient** even with best_compression enabled on both

### Source: Elastic Blog — Elasticsearch vs. OpenSearch Performance Gap (2025)
- DP-11: Elasticsearch achieves **76% faster** results on text querying vs OpenSearch
- DP-12: Term query: ~20 ms (ES) vs ~40 ms (OS) at p90
- DP-13: Date histogram: ~40 ms (ES) vs ~100 ms (OS) at p90

### Source: Blunders.io Independent Latency Benchmark
Testing at p90 latency across 40+ operations:
- DP-14: Default query: **4.76 ms (ES)** vs 5.18 ms (OS)
- DP-15: Term query: 2.46 ms (ES) vs **2.32 ms (OS)**
- DP-16: Date histogram hourly: 2.68 ms (ES) vs 2.95 ms (OS)
- DP-17: Range queries: **1,964.79 ms (ES)** vs **3,537.2 ms (OS)** — 80% difference, ES wins
- DP-18: Sorting operations: 729–841 ms range, ES is 10–25% faster

---

## 2. Elasticsearch vs. ClickHouse — Log Analytics

### Source: ClickHouse "The Billion-Row Matchup" Benchmark

#### Storage Compression (1 Billion Rows)
| Configuration | Storage Size |
|--------------|-------------|
| Elasticsearch + LZ4 + _source | 51.3 GB |
| Elasticsearch + LZ4 - _source | 38.3 GB |
| ClickHouse + LZ4 | ~5.5 GB |

- DP-19: ClickHouse uses **12x less storage** than Elasticsearch at 1B rows (5.5 GB vs 51.3 GB)

#### Storage at Scale (10 Billion Rows)
- DP-20: Elasticsearch + LZ4: ~500 GB; ClickHouse: ~40 GB — **12–19x smaller**
- DP-21: At 100B rows: ClickHouse LZ4 = 412 GB; ClickHouse ZSTD = 142 GB; Elasticsearch: **unable to load dataset**

#### Query Performance — Cold Cache (1B rows, raw data)
| Query | Elasticsearch DSL | Elasticsearch ESQL | ClickHouse | CH Advantage |
|-------|------------------|-------------------|------------|--------------|
| Top 3 projects aggregation | 3.5 s | 6.8 s | **700 ms** | 5x faster |
| Filtered data | 256 ms | 9.2 s | **42 ms** | 6x faster |

- DP-22: ClickHouse aggregation at 1B rows: **700 ms** vs Elasticsearch DSL 3.5 s (5x faster)
- DP-23: ClickHouse filtered query at 1B rows: **42 ms** vs Elasticsearch 256 ms (6x faster)
- DP-24: Elasticsearch ESQL filtered query at 10B rows: **96 seconds** vs ClickHouse **500 ms** (192x slower)

#### Query Performance — Cold Cache (10B rows, raw data)
- DP-25: Top 3 projects: Elasticsearch DSL 33 s, ESQL 32 s, ClickHouse **6.5 s** (5x faster)
- DP-26: Filtered data: Elasticsearch DSL 3.5 s, ClickHouse **500 ms** (7x faster)

#### Query Performance — Pre-aggregated (10B rows)
- DP-27: Top 3 projects: Elasticsearch 970 ms vs ClickHouse **81 ms** (12x faster)
- DP-28: Filtered: Elasticsearch 660 ms vs ClickHouse **132 ms** (5x faster)

#### ClickHouse Cloud Throughput (parallel execution)
- DP-29: 1B rows / 9 nodes: **5.2 billion rows/second**, ~100 GB/second data throughput
- DP-30: 10B rows / 9 nodes: **10.2 billion rows/second**, 192 GB/second data throughput

#### Memory Usage
- DP-31: Top 3 projects query at 1B rows: ~**50 MB** peak RAM
- DP-32: Top 3 projects query at 10B rows: ~**600 MB** peak RAM
- DP-33: Filtered query at 1B rows: **<20 MB** peak RAM

#### Cost Efficiency
- DP-34: For equivalent latency to 32-core Elasticsearch: ClickHouse requires **4x cheaper hardware** (8-core vs 32-core)
- DP-35: AdTech case study: ClickHouse reduced query processing from **8 seconds to sub-second**
- DP-36: ClickHouse achieves **60% cost reduction** in cloud storage vs Elasticsearch (AdTech case study)
- DP-37: Typical log compression: ClickHouse **10:1–20:1** vs Elasticsearch **1.5:1**

---

## 3. Vector Database Benchmarks: Qdrant vs. Milvus vs. Weaviate vs. Others

### Source: Tensorblue.com (1M vectors, 768-dimensional)

#### P95 Latency
| Database | P95 Latency |
|----------|-------------|
| FAISS (in-memory) | 10–20 ms |
| Qdrant | **30–40 ms** |
| Pinecone | 40–50 ms |
| Weaviate | 50–70 ms |
| Milvus | 50–80 ms |

- DP-38: Qdrant P95 latency at 1M 768-dim vectors: **30–40 ms**
- DP-39: Milvus P95 latency at 1M 768-dim vectors: **50–80 ms**
- DP-40: Weaviate P95 latency at 1M 768-dim vectors: **50–70 ms**
- DP-41: FAISS in-memory P95 latency: **10–20 ms** (fastest, but requires full RAM)
- DP-42: Pinecone P95 latency: **40–50 ms** (managed service)

#### Throughput (QPS) at 1M vectors
| Database | QPS Range |
|----------|-----------|
| FAISS | 20,000–50,000 |
| Milvus | 10,000–20,000 |
| Qdrant | 8,000–15,000 |
| Pinecone | 5,000–10,000 |
| Weaviate | 3,000–8,000 |

- DP-43: FAISS throughput: **20,000–50,000 QPS** (pure in-memory, no persistence)
- DP-44: Milvus throughput: **10,000–20,000 QPS**
- DP-45: Qdrant throughput: **8,000–15,000 QPS**
- DP-46: Pinecone throughput: **5,000–10,000 QPS** (managed)
- DP-47: Weaviate throughput: **3,000–8,000 QPS**

#### Memory Usage at 1M 768-dim vectors
| Database | Memory |
|----------|--------|
| Qdrant (with quantization) | ~3 GB |
| FAISS | ~3 GB |
| Weaviate | ~3.5 GB |
| Pinecone | ~4 GB |
| Milvus | ~4 GB |

- DP-48: Qdrant memory with quantization: **~3 GB** for 1M 768-dim vectors
- DP-49: Milvus memory: **~4 GB** for 1M 768-dim vectors
- DP-50: Qdrant vector quantization reduces memory by **up to 75%** while maintaining accuracy

### Source: Qdrant Official Benchmarks

- DP-51: Elasticsearch indexing is **10x slower** than Qdrant for 10M+ vectors of 96 dimensions (5.5 hours vs 32 minutes)
- DP-52: Milvus has fastest indexing time but struggles with RPS at **higher dimensions or larger vector counts**
- DP-53: Redis achieves good RPS at low precision thresholds but latency rises quickly with concurrent requests

---

## 4. Vector Database Latency — Production Scale (2025-2026)

### Source: Multiple Production Reports / Simor Consulting / Actian / Dev.to

- DP-54: Qdrant p50 latency (production): **4 ms**; p99: **25 ms**
- DP-55: Redis in-memory p50 latency: **5 ms**
- DP-56: Milvus p50 latency: **6 ms** (with GPU acceleration available)
- DP-57: Pinecone p50 latency: **8 ms** (fully managed, no infrastructure overhead)
- DP-58: Reddit production deployment (340M+ vectors): metadata filtering identified as **primary bottleneck** causing p99 latency jumps of **10x** under concurrent load
- DP-59: pgvectorscale benchmarked at **471 QPS** vs Qdrant's **41 QPS** at 99% recall on 50M vectors
- DP-60: Selective metadata filters can make queries **4x more expensive** for binary quantization at 90–95% recall
- DP-61: P99 latency degrades user experience more than median — a system with 10 ms p50 / 500 ms p99 feels slower than 20 ms p50 / 50 ms p99

---

## 5. Search Engine Indexing Throughput

### Source: Elasticsearch Official Nightly Benchmarks / Multiple Sources

- DP-62: Meilisearch indexing is **~7x faster** than Elasticsearch, PostgreSQL, and Typesense on Wikipedia 6.3M document dataset
- DP-63: RediSearch indexing is **~2x slower** than Elasticsearch on same dataset
- DP-64: Elasticsearch out-of-box indexing performance: requires tuning to achieve acceptable throughput; default config not production-ready
- DP-65: ClickHouse handles **hundreds of millions of rows per second** ingestion throughput
- DP-66: GreptimeDB log ingestion: **121,000 rows/second** (1.5x higher than Loki)
- DP-67: GreptimeDB vs TimescaleDB: **2.17x write throughput** advantage for GreptimeDB
- DP-68: GreptimeDB edge: **600,000 data points/second** with under 8% CPU usage

---

## 6. Elasticsearch Query Latency — Large Scale

### Source: Elastic Labs / QueryQuotient / Elastic Blog (2025)

- DP-69: Elasticsearch BBQ (Better Binary Quantization) achieves **sub-20 ms search latency** with 100 MB memory footprint regardless of index size at hundreds of millions of vectors
- DP-70: Elasticsearch BBQ vs OpenSearch FAISS: **up to 5x faster queries** and **3.9x higher throughput** at equivalent recall
- DP-71: Elasticsearch 9.0 BBQ throughput improvement: **8x–30x faster** with SIMD vector operations
- DP-72: Elasticsearch 9.0 BBQ recall improvement: **up to 20% higher recall** vs previous BBQ version
- DP-73: Recommended Elasticsearch shard size for optimal query performance: **10–50 GB per shard**
- DP-74: With optimization techniques, Elasticsearch can handle **10x more queries per second** and reduce latency from seconds to milliseconds
- DP-75: Elasticsearch heap recommendation: **~50% of total system RAM** (JVM heap cap: 31.5 GB effective)

---

## 7. Typesense vs. Meilisearch vs. Elasticsearch

### Source: Meilisearch Blog / Typesense Docs / GigaSearch Medium (2025)

- DP-76: Typesense search result latency: **under 50 ms** typical for most queries (C++ implementation)
- DP-77: Meilisearch search result latency: **under 50 ms** for single-node deployments
- DP-78: Meilisearch indexing: **~7x faster** than Elasticsearch on standard text search workloads
- DP-79: Typesense uses replicated cluster (full dataset per node), Meilisearch stays single-node — architectural tradeoff for scale vs simplicity
- DP-80: Elasticsearch scales to enterprise multi-PB; Typesense/Meilisearch optimized for single-server to small cluster (GB-TB range)
- DP-81: Typesense handles typos automatically with **sub-50 ms latency** while Elasticsearch requires explicit fuzzy query configuration
- DP-82: RediSearch: achieves sub-millisecond latency for some query types but is a slower outlier overall

---

## 8. Memory Usage Comparison

### Source: Various 2025 Benchmarks

| System | Workload | Memory Usage |
|--------|----------|-------------|
| Redis | Full dataset + overhead | Dataset size + 20–30% |
| Qdrant (quantized) | 1M 768-dim vectors | ~3 GB |
| Qdrant (unquantized) | 1M 768-dim vectors | ~12 GB est. |
| Milvus | 1M 768-dim vectors | ~4 GB |
| Weaviate | 1M 768-dim vectors | ~3.5 GB |
| Elasticsearch | Production cluster | Multiple high-memory nodes |
| ClickHouse | 1B row aggregation | ~50 MB peak per query |
| ClickHouse | 10B row aggregation | ~600 MB peak per query |
| SQL Server 2025 | Stabilized memory grant | ~467 MB (vs 2022 at ~934 MB) |

- DP-83: SQL Server 2025 memory grant feedback: stabilized at **~467 MB**, approximately half of SQL Server 2022 usage
- DP-84: General recommendation: allocate **60–80% of total system RAM** to database operations
- DP-85: ClickHouse query memory for 1B row filtered query: **<20 MB** peak RAM
- DP-86: Elasticsearch production clusters "often require dozens of high-memory nodes" for petabyte-scale deployments
- DP-87: Qdrant quantization cuts memory **4x–16x** (scalar to binary quantization) with configurable recall tradeoff

---

## 9. Elasticsearch Cold Start / Tier Behavior

### Source: Elastic Blog — Cold Tier Testing / Medium / Elastic Docs (2025)

- DP-88: Elasticsearch cold data tier reduces cluster storage by **up to 50%** over warm tier with equivalent reliability
- DP-89: With searchable snapshots (ES 7.11+), cluster returns to **green status immediately** after node restart — no background downloading required
- DP-90: Elasticsearch cold tier rolling restarts result in **"practically instantaneous" green status** with no additional background downloads
- DP-91: Elasticsearch does not have traditional cold start (JVM process stays warm), but first query on rarely-accessed indices experiences **cache-miss latency spike**
- DP-92: Uncached date histogram query: **4,163–7,136 ms p90** vs cached **1–5 ms** — **1,400x difference**
- DP-93: Elasticsearch default index refresh interval: **1 second** (data visible to search after ~1 s)
- DP-94: Apache Lucene 2025 improvements contributed to measurable query speedups in Elasticsearch 8.x/9.x nightly benchmarks

---

## 10. Vector Search Filtering Performance

### Source: Qdrant / VDBBench 1.0 / Reddit Engineering / Actian (2025-2026)

- DP-95: Highly selective filters (filtering out **99%+** of data) cause query speed fluctuations by **orders of magnitude** — identified as "hidden performance killer"
- DP-96: Reddit (340M+ vectors): as concurrent users grew, metadata filter resolution exceeded similarity calculation time — filter selectivity became primary cost
- DP-97: Binary quantization at lower selectivity (90–95% recall with filters): **~4x more expensive** than equivalent unfiltered queries
- DP-98: Post-filtering approach: does not scale well — either loses result accuracy or requires many candidates in first stage
- DP-99: Pre-filtering approach: requires binary mask of whole dataset — mask size grows **linearly with dataset size** (not viable at 100M+ vectors)
- DP-100: Qdrant filtered search: uses "advanced query planning strategy" that avoids speed downturn, accuracy collapse, and disconnected HNSW graphs at high filter selectivity
- DP-101: VDBBench 1.0 (2025) tests filter selectivity scenarios from low to high — demonstrates **order-of-magnitude variance** across databases at high selectivity
- DP-102: Moving data between vector graph and relational metadata store causes **p99 latency to jump 10x** due to CPU waiting on disk I/O
- DP-103: MongoDB Atlas Vector Search benchmarked for filtered ANN — shows degradation curves as filter selectivity increases beyond 95%

---

## Summary Table — Key Numbers at a Glance

| Comparison | Metric | Result |
|-----------|--------|--------|
| ES vs OS (text search, p90) | Latency | ES: 7.47 ms vs OS: 18.11 ms |
| ES vs OS (date histogram, p90) | Latency | OS: 124.79 ms vs ES: 2,064.61 ms |
| ES vs OS (Big5 overall) | Latency | OS: 12.1 ms vs ES: 18.8 ms |
| ClickHouse vs ES (1B rows, aggregation) | Query time | CH: 700 ms vs ES: 3.5 s |
| ClickHouse vs ES (10B rows, filtered) | Query time | CH: 500 ms vs ES ESQL: 96 s |
| ClickHouse vs ES (storage, 1B rows) | Disk | CH: 5.5 GB vs ES: 51.3 GB |
| ClickHouse vs ES (hardware cost) | Cost | CH needs 4x cheaper hardware |
| Qdrant (production) | p50 / p99 | 4 ms / 25 ms |
| Milvus | p50 | 6 ms |
| Pinecone | p50 | 8 ms |
| Qdrant vs Weaviate vs Milvus (1M vecs) | P95 | 30–40 ms / 50–70 ms / 50–80 ms |
| Milvus throughput (1M vecs) | QPS | 10,000–20,000 |
| Qdrant throughput (1M vecs) | QPS | 8,000–15,000 |
| Elasticsearch BBQ vs OS FAISS | Speed | 5x faster queries, 3.9x throughput |
| ES 9.0 BBQ SIMD improvement | Speed | 8x–30x throughput gain |
| ES cold tier vs warm tier | Storage | 50% reduction |
| Cached vs uncached aggregation (ES) | Latency | 1,400x difference |
| Meilisearch vs ES indexing | Speed | Meilisearch ~7x faster |
| ClickHouse log compression | Ratio | 10:1–20:1 vs ES 1.5:1 |
| Filtered search p99 (production) | Latency spike | Up to 10x under concurrent load |

---

## Sources

- [Benchmarking OpenSearch and Elasticsearch — Trail of Bits (March 2025)](https://blog.trailofbits.com/2025/03/06/benchmarking-opensearch-and-elasticsearch/)
- [Elasticsearch vs. OpenSearch: Performance and resource utilization — Elastic Blog](https://www.elastic.co/blog/elasticsearch-opensearch-performance-gap)
- [Elasticsearch Benchmarks — Elasticsearch vs. OpenSearch — Blunders.io](https://blunders.io/posts/es-benchmark-3-latency)
- [ClickHouse vs. Elasticsearch: The Billion-Row Matchup — ClickHouse Blog](https://clickhouse.com/blog/clickhouse_vs_elasticsearch_the_billion_row_matchup)
- [ClickHouse vs. Elasticsearch — DoubleCloud Comparison](https://double.cloud/comparison/elasticsearch-vs-clickhouse/)
- [ClickHouse vs Elasticsearch for Log Analytics — OneUptime (2026)](https://oneuptime.com/blog/post/2026-01-21-clickhouse-vs-elasticsearch/view)
- [Vector Search Benchmarks — Qdrant](https://qdrant.tech/benchmarks/)
- [Best Vector Database 2025: Pinecone vs Weaviate vs Qdrant vs Milvus — Tensorblue](https://tensorblue.com/blog/vector-database-comparison-pinecone-weaviate-qdrant-milvus-2025)
- [VDBBench 1.0: Real-World Benchmarking for Vector Databases — Milvus Blog](https://milvus.io/blog/vdbbench-1-0-benchmarking-with-your-real-world-production-workloads.md)
- [Vector Search Performance Benchmark: SingleStore, Pinecone, Zilliz — benchANT](https://benchant.com/blog/single-store-vector-vs-pinecone-zilliz-2025)
- [MongoDB Vector Search Benchmark Results — Atlas Docs](https://www.mongodb.com/docs/atlas/atlas-vector-search/benchmark/results/)
- [Elasticsearch 9.0 & 8.18: BBQ GA — Elastic Blog](https://www.elastic.co/cn/blog/whats-new-elastic-search-9-0-0)
- [Elasticsearch vs Qdrant vs Meilisearch: Which Fits 2025? — Meilisearch Blog](https://www.meilisearch.com/blog/elasticsearch-vs-qdrant)
- [Typesense vs Algolia vs Elasticsearch vs Meilisearch — Typesense Comparison](https://typesense.org/typesense-vs-algolia-vs-elasticsearch-vs-meilisearch/)
- [Meilisearch vs Typesense — Meilisearch Blog](https://www.meilisearch.com/blog/meilisearch-vs-typesense)
- [Benchmarking Performance: Elasticsearch vs Competitors — GigaSearch / Medium](https://medium.com/gigasearch/benchmarking-performance-elasticsearch-vs-competitors-d4778ef75639)
- [Testing Elasticsearch Cold Tier at Scale — Elastic Blog](https://www.elastic.co/blog/testing-the-new-elasticsearch-cold-tier-of-searchable-snapshots-at-scale)
- [Database Performance Benchmarks — GreptimeDB (2025)](https://greptime.com/tech-content/2025-06-11-database-performance-benchmarks)
- [Vector Database Benchmarks are Misleading: What Matters — Actian (2026)](https://www.actian.com/blog/databases/how-to-evaluate-vector-databases-in-2026/)
- [Choosing a vector database for ANN search at Reddit — Milvus Blog](https://milvus.io/blog/choosing-a-vector-database-for-ann-search-at-reddit.md)
- [VectorDBBench — Zilliz GitHub](https://github.com/zilliztech/VectorDBBench)
- [OpenSearch vs Elasticsearch (2025) — Medium / Jagadeesh Chandra](https://medium.com/@jagadeeshchandra/elasticsearch-vs-opensearch-2025-the-definitive-showdown-d2a31f0769e1)
- [ClickHouse vs Elasticsearch 2026 — Tasrie IT Services](https://tasrieit.com/blog/clickhouse-vs-elasticsearch-2026)
