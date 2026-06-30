# Chroma, pgvector, and LanceDB: Production Issues Research (2025-2026)

Research compiled: April 2026
Sources: GitHub issues, engineering blogs, G2 reviews, community forums, benchmarks

---

## 1. ChromaDB: Production Issues

### 1.1 Architecture Limitations

**Data point 1** — Chroma is a single-node database; it currently operates without distributed, multi-node clustering support. There is no built-in replication or horizontal scale-out for the self-hosted open-source version.
Source: AltexSoft, G2 reviews

**Data point 2** — Chroma Cloud (distributed/serverless) was still in "production private preview" as of late 2024 / early 2025. Teams needing HA had no GA managed option.
Source: AltexSoft blog

**Data point 3** — No enterprise support packages available. Users rely entirely on community forums, GitHub issues, and Discord. This is a blocker for regulated industries and teams with on-call SLA requirements.
Source: G2 reviews, AltexSoft

**Data point 4** — Chroma is suitable for thousands to hundreds of thousands of vectors. Performance degrades significantly at millions of vectors with high query throughput, where Pinecone or Weaviate are recommended instead.
Source: PE Collective review, G2

**Data point 5** — Users are responsible for all DevOps: scaling, maintenance, upgrades, and monitoring. Adds significant ops burden for teams without dedicated infrastructure expertise.
Source: AltexSoft blog

**Data point 6** — Memory leak issues noted in production reviews on G2. Chroma uses significant RAM for vector operations especially with large-scale data due to its reliance on in-memory storage.
Source: G2 reviews, AltexSoft

### 1.2 Persistence and Durability Bugs (GitHub Issues)

**Data point 7** — GitHub Issue #1976: Non-persistent clients retain data within the same program run. A `Client()` instance set to be non-persistent still recovers collections when a new instance is opened in the same process. Breaks expected in-memory isolation semantics.
Source: https://github.com/chroma-core/chroma/issues/1976

**Data point 8** — GitHub Issue #6132: Persistence not working in Docker containers. Users mount volumes, write data via API, but no data is actually persisted to disk. The bind-mount volume setup silently fails.
Source: https://github.com/chroma-core/chroma/issues/6132

**Data point 9** — GitHub Issue #865: PersistentClient failing to store embeddings. TypeError related to incompatible function arguments with hnswlib.Index initialization. Regression introduced in a point release.
Source: https://github.com/chroma-core/chroma/issues/865

**Data point 10** — GitHub Issue #527: Docker image persistence confusion. Multiple users unable to determine how to persist and reload data when using the Chroma Docker image.
Source: https://github.com/chroma-core/chroma/issues/527

**Data point 11** — GitHub Issue #5868: Unable to properly close PersistentClient. No close() method exposed. Long-running processes cannot flush and close cleanly, meaning in-memory state may not be committed to disk before process exit.
Source: https://github.com/chroma-core/chroma/issues/5868

**Data point 12** — langchain-ai/langchain Issue #20851: Chroma 0.4.x removed the `persist()` method without backward-compatible migration path. Large number of downstream applications broke silently — embeddings appeared to be saved but were not.
Source: https://github.com/langchain-ai/langchain/issues/20851

**Data point 13** — SQLite file locking and corruption reports. Chroma's local storage backend (SQLite + hnswlib files) is susceptible to corruption when the process is killed or crashes mid-write.
Source: GitHub issues aggregated

**Data point 14** — GitHub Issue #3818: Users attempting to run a production ChromaDB instance report confusion about correct deployment architecture; no documented production deployment guide exists for self-hosted.
Source: https://github.com/chroma-core/chroma/issues/3818

**Data point 15** — GitHub Issue #5392: chromadb client crashes on a persisted database. Loading a previously persisted collection results in a crash during read in certain versions.
Source: https://github.com/chroma-core/chroma/issues/5392

**Data point 16** — GitHub Issue #2325: Single worker querying the DB — concurrency model limitations discovered in production. Running 10 Uvicorn workers against a single Chroma instance surfaces thread-safety and connection issues.
Source: https://github.com/chroma-core/chroma/issues/2325

### 1.3 Reliability Comparisons

**Data point 17** — Consensus from multiple 2025 comparison articles: Chroma is "developer-friendly and lightweight; excellent for prototyping and small/medium apps; not the tool you pick for billions of vectors or regulated, multi-tenant enterprise loads."
Source: LiquidMetal AI comparison, firecrawl.dev

**Data point 18** — Recommended migration path cited by practitioners: use Chroma for rapid development and prototyping, then migrate to Pinecone, Qdrant, or Weaviate for production scale. This implies Chroma is not trusted for production at scale.
Source: Digital One Agency, sysdebug.com

**Data point 19** — Chroma does not offer ACID-compliant transactions. This is contrasted against Qdrant which provides ACID-compliant operations ensuring data consistency.
Source: LiquidMetal AI comparison

**Data point 20** — In head-to-head production reliability assessments for 2025, "mature choices" cited are Pinecone, Weaviate, and Qdrant. Chroma is not listed as a mature production choice.
Source: firecrawl.dev best vector databases 2025

---

## 2. pgvector: Production Scaling and HA Issues

### 2.1 Scaling Thresholds and Performance Degradation

**Data point 21** — pgvector works well at 10,000 vectors but stops being the right choice at 5 million vectors. Sub-100ms queries become multi-second timeouts at production scale.
Source: "The Case Against pgvector" — alex-jacobs.com, simonwillison.net

**Data point 22** — Performance drops significantly at 10M+ vectors. Index build times are substantially longer than purpose-built vector databases at these scales.
Source: amitavroy.com, thenewstack.io

**Data point 23** — Postgres's query planner was not built for filtered vector search. Cost estimates for vector queries are frequently wrong, leading to the planner choosing sequential scans over index scans at inopportune times.
Source: "The reason your pgvector benchmark is lying to you" — thenewstack.io

**Data point 24** — Benchmarks at small scale (10,000 vectors at 128 dimensions) do not reflect real-world behavior at 5 million vectors and 1,536 dimensions. The community warns that pgvector benchmarks systematically mislead developers.
Source: thenewstack.io, actian.com

**Data point 25** — pgvector was architected as a bolt-on to PostgreSQL, which was not designed with vectors as a first-class citizen. At scale, "those bolts start to creak" — architectural impedance becomes visible.
Source: amitavroy.com

**Data point 26** — Reddit's engineering team, managing 340M+ vectors in 2025, identified metadata filtering as the primary performance bottleneck. As concurrent users grew, the DB spent more time resolving metadata filters than computing similarity distances.
Source: actian.com / embedded vector DB limitations research

**Data point 27** — Standard benchmarks only test a single concurrent client. Production systems require 100+ concurrent clients hitting different metadata subsets simultaneously. pgvector performance characteristics change dramatically under real concurrency.
Source: actian.com

### 2.2 HNSW Index Memory and Rebuild Issues

**Data point 28** — Rapid adoption of pgvector for RAG has created a documented "Vector Hangover": HNSW index memory bloat causing skyrocketing infrastructure costs.
Source: tech-champion.com

**Data point 29** — HNSW indexes demand massive RAM residency for performance. Building an HNSW index requires holding the entire graph in memory during construction.
Source: tech-champion.com, crunchydata.com

**Data point 30** — Default `maintenance_work_mem` is 64 MB. If not tuned upward, PostgreSQL falls back to a disk-based HNSW build that runs 10-50x slower. Most teams discover this mid-build after hours of waiting with no progress indication.
Source: dev.to pgvector scaling guide

**Data point 31** — For 5 million vectors at 1,536 dimensions, HNSW index construction requires 8-16 GB of working memory. This is non-obvious and underdocumented.
Source: dev.to pgvector scaling guide

**Data point 32** — Vacuuming stalls and WAL file explosion during HNSW index maintenance are frequently reported in GitHub issues and community forums.
Source: tech-champion.com, GitHub pgvector issues

**Data point 33** — GitHub Issue #822: HNSW index creation gets stuck on tens of millions of entries. Users report index builds that appear to hang with no feedback.
Source: https://github.com/pgvector/pgvector/issues/822

**Data point 34** — GitHub Issue #844 and #843: HNSW graph memory usage is opaque and difficult to estimate before starting a build. No built-in tooling to predict RAM requirements.
Source: https://github.com/pgvector/pgvector/issues/844

**Data point 35** — GitHub Issue #745: Request to automate estimation of memory needed for fast HNSW index build — exists as an open feature request, meaning there is still no automated guidance.
Source: https://github.com/pgvector/pgvector/issues/745

**Data point 36** — Index management is fundamentally hard with pgvector. Rebuilds are memory-intensive, time-consuming, and disruptive to production query traffic during the rebuild window.
Source: alex-jacobs.com, amitavroy.com

### 2.3 Clustering and High Availability

**Data point 37** — pgvector scales the same way PostgreSQL scales: vertically (more RAM/CPU), or horizontally with read replicas, or via Citus for sharding. There is no native distributed vector index.
Source: instaclustr.com pgvector guide

**Data point 38** — pgvector uses WAL for replication, enabling standard PostgreSQL HA setups (streaming replication, patroni, etc.). However, HNSW index builds are not replicated via WAL streaming in real-time — replicas must build indexes independently.
Source: instaclustr.com, northflank.com

**Data point 39** — pgvector "may not yet meet the reliability expectations required for mission-critical or high-availability systems, as it is still under active development and may exhibit bugs or performance instability in some environments."
Source: instaclustr.com 2026 guide

**Data point 40** — HNSW indexing on the production database server means large index builds compete with live production queries for RAM and CPU. There is no built-in workload isolation.
Source: zenvanriel.com, amitavroy.com

**Data point 41** — For enterprise-grade HA with pgvector, pgEdge (announced 2025) offers distributed Postgres with vector support, but this requires adopting a third-party commercial extension layer, not a standard pgvector deployment.
Source: pgedge.com press release, prnewswire.com

**Data point 42** — pgvector with pgvectorscale extension achieves 471 QPS at 99% recall on 50M vectors — 11x faster than Qdrant at the same recall level in Supabase's benchmarks. However, these benchmarks have been criticized as using non-equivalent hardware configurations.
Source: zenvanriel.com, thenewstack.io

**Data point 43** — IVFFlat index type requires choosing the number of lists (clusters) at index creation time. If the number of lists is tuned at small scale, re-indexing is required as the dataset grows — another disruptive production operation.
Source: crunchydata.com, northflank.com

### 2.4 pgvector vs. Dedicated Databases

**Data point 44** — For 80% of SaaS AI features at single-digit millions of vectors, pgvector handles the load with proper indexing. The 20% edge case requiring dedicated DBs is where teams get burned — they build on pgvector and hit the wall later.
Source: zenvanriel.com

**Data point 45** — Key dedicated vector DB advantage: isolated workloads. Dedicated databases separate vector search from OLTP, providing predictable performance for both. pgvector does not offer this isolation.
Source: zenvanriel.com, amitavroy.com

**Data point 46** — pgvector advantage: atomic transactions spanning vector and relational data. When vector updates must be consistent with relational updates, pgvector's single-database model eliminates sync complexity.
Source: zenvanriel.com

---

## 3. LanceDB: Production Issues

### 3.1 Concurrent Writers and Storage Consistency

**Data point 47** — GitHub Issue #3086: S3+DynamoDB deployment with concurrent writes leads to storage bloat and inconsistency. After 5,000 single-record insertions, storage grew to ~800 MB because each `add()` creates new data fragments and version manifests without compaction.
Source: https://github.com/lancedb/lancedb/issues/3086

**Data point 48** — Same issue: datasets enter a stuck state where DynamoDB commit store and S3 become inconsistent. Retry loops fail. Manual intervention required to recover.
Source: https://github.com/lancedb/lancedb/issues/3086

**Data point 49** — GitHub Issue #2002: Concurrent writes to S3-compatible object stores are not fully supported. This limits multi-process / distributed writer architectures.
Source: https://github.com/lancedb/lancedb/issues/2002

**Data point 50** — GitHub Issue #1614: LanceDB currently requires DynamoDB as a commit store for S3 deployments. There is no way to use S3 alone without DynamoDB. This forces an additional managed service dependency for every S3-backed deployment.
Source: https://github.com/lancedb/lancedb/issues/1614

### 3.2 Memory Leaks

**Data point 51** — GitHub Issue #2468: Memory leak with S3 storage backend (Python). Users report memory consumption exceeding 16 GB RAM while working with only ~2 GB datasets (2 million records). Process memory grows unbounded during sustained operations.
Source: https://github.com/lancedb/lancedb/issues/2468

**Data point 52** — Production deployment under Uvicorn (API server): memory leaks manifest when tables and connections are not explicitly closed after each operation. Connections must be manually closed to prevent excessive memory consumption. This is a non-obvious operational requirement.
Source: sprytnyk.dev (700M vector production case study)

**Data point 53** — The 700M vector production case study explicitly warns: "memory leaks became apparent when running LanceDB in production under Uvicorn as an API."
Source: https://sprytnyk.dev/posts/running-lancedb-in-production/

### 3.3 Operational Complexity

**Data point 54** — GitHub Issue #3201: No clear guidance on `optimize()` cadence. Documentation states optimization frequency should match data modification frequency, but provides no performance characteristics or metrics to guide this decision. Production teams are left guessing.
Source: https://github.com/lancedb/lancedb/issues/3201

**Data point 55** — Unstable row IDs: there is an ongoing effort (open GitHub issue) to make row IDs stable. This matters for applications that store LanceDB row IDs as external references — IDs can change after compaction/optimization.
Source: lancedb/lancedb GitHub issues

**Data point 56** — Storage fragmentation is a known production concern. The Lance columnar format creates many small fragment files over time, requiring explicit optimization passes. Without regular `optimize()` calls, query performance degrades.
Source: lancedb/lancedb GitHub issues, docs

**Data point 57** — LanceDB is primarily designed as an embedded/local library. Running it as a multi-tenant service requires the LanceDB Cloud offering or significant self-managed infrastructure work.
Source: lancedb.com/customers, GitHub README

### 3.4 Production Scale Data Points

**Data point 58** — One documented production deployment: 700 million vectors successfully migrated and run in production using LanceDB. However, the case study documents significant operational learnings around memory management.
Source: sprytnyk.dev

**Data point 59** — LanceDB raised $30M Series A in June 2025 and launched Multimodal Lakehouse Suite. Enterprise features (Search, EDA, Feature Engineering, Training) added to LanceDB Enterprise in 2025.
Source: lancedb.com/blog/newsletter-june-2025/

**Data point 60** — CodeRabbit, Cognee, and Continue (AI coding assistant) use LanceDB in production — but these are local/embedded deployments (IDE plugins, local dev tools), not distributed high-availability services.
Source: lancedb.com/customers

---

## 4. Cross-Cutting Themes

### 4.1 The Prototype-to-Production Trap

**Data point 61** — A consistent pattern across all three databases: they are chosen for prototyping due to ease of setup, but hit production blockers related to durability, HA, and scale. The migration cost to a purpose-built system (Qdrant, Weaviate, Pinecone) is underestimated.
Source: multiple comparison articles, practitioner blogs

**Data point 62** — The embedded database model (Chroma local, LanceDB embedded) works well in development but requires fundamentally different deployment patterns in production. Teams underestimate this gap.
Source: actian.com, lakefs.io

### 4.2 Concurrency and Multi-tenancy Gaps

**Data point 63** — Production requires 100+ concurrent clients; standard benchmarks test single clients. All three databases (Chroma, pgvector, LanceDB) show performance degradation patterns under real concurrent workloads that benchmarks do not reveal.
Source: actian.com, thenewstack.io

**Data point 64** — Embedded databases are single-node by default with limited authentication and multi-tenant controls. This is a shared limitation across Chroma (local mode) and LanceDB (embedded mode).
Source: actian.com

### 4.3 Index Management Operational Burden

**Data point 65** — All three systems require explicit index management: Chroma (hnswlib index files), pgvector (CREATE INDEX, REINDEX), LanceDB (optimize()). Dedicated managed databases like Pinecone and Qdrant Cloud abstract this away entirely.
Source: multiple sources

**Data point 66** — Data currency risk: if these databases cannot re-index as quickly as they ingest data, AI applications may serve stale or incorrect results for hours. This is a documented failure mode for high-ingest production workloads.
Source: actian.com

---

## 5. Summary: Production Readiness Ratings (Community Consensus 2025)

| Database | Prototype | Small Prod (<1M vec) | Large Prod (>5M vec) | HA / Clustering | Managed Option |
|---|---|---|---|---|---|
| ChromaDB | Excellent | Marginal | Not recommended | None (self-hosted) | Private preview only |
| pgvector | Good | Good | Degrades >5M | PostgreSQL HA patterns | Via cloud Postgres providers |
| LanceDB | Excellent | Good | Capable but complex ops | S3+DynamoDB required | LanceDB Cloud (Enterprise) |
| Qdrant | Good | Excellent | Excellent | Native clustering, ACID | Qdrant Cloud (GA) |

---

## Sources

- [The Good and Bad of ChromaDB for RAG: Based on Our Experience](https://www.altexsoft.com/blog/chroma-pros-and-cons/)
- [Chroma Vector Database Reviews 2026 - G2](https://www.g2.com/products/chroma-vector-database/reviews)
- [Chroma Review (2026): The Simple Vector DB - PE Collective](https://pecollective.com/tools/chroma/)
- [GitHub: chroma-core/chroma Issues](https://github.com/chroma-core/chroma/issues)
- [Bug: Persistence within single program despite non-persistent clients #1976](https://github.com/chroma-core/chroma/issues/1976)
- [Bug: Persistence not working when running ChromaDB Docker container #6132](https://github.com/chroma-core/chroma/issues/6132)
- [Bug: chroma db PersistentClient not storing embeddings #865](https://github.com/chroma-core/chroma/issues/865)
- [Bug: How to persist and load data when using Chroma docker image #527](https://github.com/chroma-core/chroma/issues/527)
- [Bug: Unable to Close Persistent Client #5868](https://github.com/chroma-core/chroma/issues/5868)
- [Persist method in Chroma no longer exists in Chroma 0.4.x - langchain #20851](https://github.com/langchain-ai/langchain/issues/20851)
- [Install issue: Need help regarding production chromadb instance #3818](https://github.com/chroma-core/chroma/issues/3818)
- [chromadb client crashes on persisted database #5392](https://github.com/chroma-core/chroma/issues/5392)
- [Bug: Single worker is querying the DB #2325](https://github.com/chroma-core/chroma/issues/2325)
- [The Case Against pgvector - Alex Jacobs](https://alex-jacobs.com/posts/the-case-against-pgvector/)
- [The case against pgvector - Simon Willison](https://simonwillison.net/2025/Nov/3/the-case-against-pgvector/)
- [The reason your pgvector benchmark is lying to you - The New Stack](https://thenewstack.io/why-pgvector-benchmarks-lie/)
- [Beyond pgvector: Choosing the Right Vector Database for Production](https://www.amitavroy.com/articles/beyond-pgvector-choosing-the-right-vector-database-for-productions)
- [pgvector vs Dedicated Vector Databases: When PostgreSQL Is Enough](https://zenvanriel.com/ai-engineer-blog/pgvector-vs-dedicated-vector-db/)
- [Scaling Vector Data with Postgres - Crunchy Data](https://www.crunchydata.com/blog/scaling-vector-data-with-postgres)
- [Scaling pgvector: Memory, Quantization, and Index Build Strategies](https://dev.to/philip_mcclarence_2ef9475/scaling-pgvector-memory-quantization-and-index-build-strategies-8m2)
- [The 'Vector Hangover': HNSW Index Memory Bloat in Production RAG](https://tech-champion.com/database/the-vector-hangover-hnsw-index-memory-bloat-in-production-rag/)
- [HNSW index creation is stuck on dozens millions entries #822](https://github.com/pgvector/pgvector/issues/822)
- [Questions on HNSW graph memory usage and size estimation #844](https://github.com/pgvector/pgvector/issues/844)
- [Automate estimation of memory needed for fast HNSW index build #745](https://github.com/pgvector/pgvector/issues/745)
- [pgvector: Key features, tutorial, and pros and cons 2026 guide - Instaclustr](https://www.instaclustr.com/education/vector-database/pgvector-key-features-tutorial-and-pros-and-cons-2026-guide/)
- [pgEdge Announces pgEdge Agentic AI Toolkit for Postgres](https://www.pgedge.com/press-releases/pgedge-announces-pgedge-agentic-ai-toolkit-for-postgres)
- [Scaling LanceDB: Running 700 million vectors in production](https://sprytnyk.dev/posts/running-lancedb-in-production/)
- [Feedback on CRUD patterns with S3+DDB: storage growth vs concurrent writers #3086](https://github.com/lancedb/lancedb/issues/3086)
- [Bug: Memory Leak when using S3 storage #2468](https://github.com/lancedb/lancedb/issues/2468)
- [Feature: Support S3 without needing DynamoDB #1614](https://github.com/lancedb/lancedb/issues/1614)
- [Feature: Support concurrent writes in S3-compatible object stores #2002](https://github.com/lancedb/lancedb/issues/2002)
- [Guidance on performance characteristics of optimize() #3201](https://github.com/lancedb/lancedb/issues/3201)
- [June 2025: $30M Series A, Multimodal Lakehouse Launch - LanceDB Blog](https://lancedb.com/blog/newsletter-june-2025/)
- [LanceDB In Production - Customer Stories](https://www.lancedb.com/customers)
- [Vector Database Benchmarks are Misleading: What Matters - Actian](https://www.actian.com/blog/databases/how-to-evaluate-vector-databases-in-2026/)
- [Vector Database Performance Compared: pgvector vs Pinecone vs Qdrant vs Weaviate](https://dev.to/kencho/vector-database-performance-compared-pgvector-vs-pinecone-vs-qdrant-vs-weaviate-2ne6)
- [Vector Database Comparison: Pinecone vs Weaviate vs Qdrant vs FAISS vs Milvus vs Chroma (2025)](https://liquidmetal.ai/casesAndBlogs/vector-comparison/)
- [Best Vector Databases in 2025: A Complete Comparison - Firecrawl](https://www.firecrawl.dev/blog/best-vector-databases)
- [Supercharging vector search performance with pgvector 0.8.0 on Amazon Aurora PostgreSQL](https://aws.amazon.com/blogs/database/supercharging-vector-search-performance-and-relevance-with-pgvector-0-8-0-on-amazon-aurora-postgresql/)
- [Optimizing Vector Search at Scale: Lessons from pgvector and Supabase](https://medium.com/@dikhyantkrishnadalai/optimizing-vector-search-at-scale-lessons-from-pgvector-supabase-performance-tuning-ce4ada4ba2ed)
- [Pgvector vs. Qdrant - Tiger Data](https://www.tigerdata.com/blog/pgvector-vs-qdrant)
