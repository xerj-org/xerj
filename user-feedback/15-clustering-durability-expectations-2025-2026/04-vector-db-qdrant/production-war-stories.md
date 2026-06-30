# Vector Database Production War Stories & Lessons Learned (2025–2026)

Compiled from 8 web searches and deep-fetches across developer communities, engineering blogs, and case studies. 60+ distinct data points organized by failure category.

**Sources searched:** DEV.to, Medium, Qdrant blog, Milvus blog, Notion engineering blog, VentureBeat, Particula, Actian, Salish Sea Consulting, TigerData, and others.

---

## 1. COST SHOCK & PRICING TRAPS

**DP-01 — Pinecone $6K/month vs $300/month in Postgres**
One team paid $6,000/month for Pinecone to store vectors that could have been stored in Postgres for $300/month. After migrating to a hybrid stack, total cost dropped to $700/month — an 88% reduction — with no meaningful accuracy loss.

**DP-02 — Pinecone $50/month minimum nuked hobby projects**
In October 2025, Pinecone implemented a $50/month minimum across all paid Standard plans. Teams running stable low-volume production workloads faced a 400–500% cost increase overnight. The resulting migration wave was large enough to generate a trending thread: "Pinecone's new $50/mo minimum just nuked my hobby project."

**DP-03 — Six-figure annual contracts for marginal gains**
Multiple teams signed six-figure annual contracts for managed vector database services, then benchmarked and discovered that pgvector + Postgres achieved similar recall at a fraction of the cost. The "built for this" narrative didn't survive production comparisons.

**DP-04 — SQL Server 2025 cost spiral**
Organizations evaluating SQL Server 2025's new vector capabilities discovered that a $500/month SQL Server setup could balloon to $3,000/month once vector indexing, memory requirements, and licensing were fully accounted for in production.

**DP-05 — AWS S3 Vectors: 90% savings claim**
AWS launched S3 Vectors in 2025/2026, claiming 90% cost reduction versus standalone vector databases. Industry analysts were split on whether this is "complementary" or effectively kills the standalone vector DB market. Cost disruption from cloud hyperscalers is now a real threat to every vector DB vendor.

**DP-06 — Over-provisioning costs from capacity model**
Notion's early vector infrastructure charged for database uptime, making over-provisioning "prohibitively expensive." Teams had to under-provision then scramble when they hit capacity. The generation-based sharding workaround they built added engineering cost that was never planned for.

---

## 2. CAPACITY CRISES & SCALING FAILURES

**DP-07 — Notion: indexes at capacity one month post-launch**
Just one month after Notion launched vector search (November 2023), original indexes were close to capacity. Risk: if they ran out of space, they'd be forced to pause onboarding entirely, delaying value for new users. They responded with generation-based sharding — each shard set got a "generation" ID determining where reads and writes would go, avoiding re-shard operations.

**DP-08 — pgvector brought production database to its knees**
A developer's team started small and vector search worked beautifully — then scale happened. What began as sub-100ms queries turned into multi-second timeouts. At 5 million vectors: query latency was inconsistent (sometimes 50ms, sometimes 5 seconds), index builds took hours and occasionally killed the database, memory spikes during index creation forced restarts, and production alerts were constant.

**DP-09 — Faiss GPU scaling illusion**
Faiss with GPU acceleration handled 50 QPS at P99 <100ms. At 500 QPS, P99 spiked to 1.2 seconds. The team invested in 8x more GPU capacity for 4x cost but achieved only 2x QPS gain — severely diminishing returns. Root cause: GPUs parallelize batch operations, not concurrent requests.

**DP-10 — Notion: 600x daily onboarding capacity increase required**
The hyper-growth phase demanded 600x more daily onboarding capacity, 15x active workspace growth, and 8x vector database capacity expansion. None of this was anticipated in the original architecture. The system survived only because the team moved fast with sharding hacks.

**DP-11 — Cold start penalty at billion-scale**
Loading 1B+ vector indexes takes 8+ minutes on cold start, creating unacceptable gaps in availability during restarts or failovers. Teams discovered this only in production; no benchmark warned them.

**DP-12 — Milvus 10M vector insertion: 25 minutes**
Hands-on benchmarking of 10M vectors (768-dim BERT embeddings): Milvus parallel bulk insertion took ~25 minutes. LanceDB single-threaded insertion took ~15 minutes. Milvus index size: ~40GB. LanceDB index size: ~25GB. Both numbers surprised teams expecting tighter figures from documentation.

---

## 3. PERFORMANCE DEGRADATION IN PRODUCTION

**DP-13 — Qdrant filter latency: 15ms → 210ms (14x increase)**
Qdrant's payload filters felt "magical" at 10K vectors with 15ms latency. At 10M vectors, that same query took 210ms — a 14x increase. Root cause: pre-filtering vs. post-filtering trade-offs weren't apparent until hitting high-cardinality fields (user_id). The team had tested with synthetic datasets, not actual data distributions.

**DP-14 — Reddit 340M vectors: metadata filtering as primary bottleneck**
Reddit's engineering team, managing 340M+ vectors, identified metadata filtering — not similarity calculation — as the primary performance bottleneck in their 2025 deployment. As concurrent users grew, the database spent more time resolving metadata filters than computing similarity distances.

**DP-15 — P99 latency 10x jump from cross-system data movement**
Moving data between a vector graph and a relational metadata store caused P99 latency to jump by 10x. The CPU waited on disk I/O during metadata resolution. This pattern appeared in multiple deployments where vector and metadata were stored in separate systems.

**DP-16 — Qdrant 800M vectors: 10–20 second query times**
A team running 800M vectors with multiple filters experienced 10–20 second query times. Fix required: HNSW `ef` parameter tuning, quantized vectors in RAM, and original vectors on disk. Final result: sub-second responses while maintaining accuracy. This took weeks to diagnose and tune.

**DP-17 — Fintech 500M records: bulk import collapsed within 1 hour**
A fintech team ingesting 500M records saw performance collapse within one hour. Root cause: HNSW indexing was enabled during uploads, causing CPU usage to soar and services to time out. Fix: disable indexing during ingestion, rebuild index post-upload in a single pass. Teams that read documentation carefully avoid this; teams that don't lose hours.

**DP-18 — SaaS multi-customer hot shard: 5x load imbalance**
A SaaS team's sharding scheme funneled "hot" customer data to a single node, which handled 5x more load than peers. Result: severe latency spikes from imbalanced distribution. Rebalancing required testing multiple shard configurations to find one that matched query patterns.

**DP-19 — Milvus Lite vs. distributed: latency parity breaks at tens of millions**
Milvus Lite (library mode) performed comparably to distributed Milvus up to tens of millions of vectors. Beyond that, the single-threaded architecture became the bottleneck and consistent latency was no longer achievable. Teams who started on Lite and planned to "scale up later" discovered this migration was harder than anticipated.

**DP-20 — Tail latency as the real signal**
P99 latency, index fragmentation, and write amplification degrade systems long before average QPS drops. Teams that monitored median latency missed early warning signs that only appeared in the 99th percentile.

---

## 4. CONSISTENCY & STALENESS BUGS

**DP-21 — Weaviate eventual consistency exposed stale reads**
A team almost shipped a Weaviate deployment before surfacing a critical bug: eventual consistency was chosen for throughput, but users saw outdated search results seconds after updates. The failure mode was intermittent and hard to reproduce in testing.

**DP-22 — Milvus session consistency: 12ms write overhead eliminated complaints**
Switching to session consistency in Milvus added 12ms to writes but eliminated all customer staleness complaints. The trade-off table from production: Annoy (never visible — data reindexing nightmare), Qdrant (immediate per shard, but staleness during rebalancing), Milvus (session guaranteed, 8–15ms higher write latency).

**DP-23 — Qdrant indexed_only mode: eventual consistency tradeoff**
Setting `indexed_only=true` in Qdrant search requests ensures fast searches by only considering indexed data. New data becomes searchable only after indexing completes — a form of eventual consistency that surprised teams who assumed "insert then query" would immediately return new data.

**DP-24 — Annoy: no real-time updates, ever**
Annoy (the library used by many early RAG teams) has no update path. Adding documents means rebuilding the entire index. Teams discovered this when their first production document update required a 3+ hour index rebuild. There is no workaround — it's a fundamental design choice.

---

## 5. SEMANTIC ACCURACY FAILURES

**DP-25 — "Error 221" returns "Error 222"**
A team's RAG system confidently returned "Error 222" when users searched for "Error 221." The vectors were semantically similar enough to match, but the answer was categorically wrong. Their LLM then hallucinated a solution based on the incorrect context.

**DP-26 — "Premium tire insurance" returns "basic tire insurance"**
Similar semantic confusion caused a vector search for "premium tire insurance" to return "basic tire insurance" results. In financial or insurance domains, "similar" and "correct" are not the same thing.

**DP-27 — "Revenue growth" returns "revenue decline" documents**
A financial analyst queried "revenue growth" and received "revenue decline" documents. These concepts are semantically related (antonyms) but directionally opposite — a vector space cannot reliably distinguish them.

**DP-28 — Before/after hybrid retrieval: 65% → 94% accuracy**
One team measured accuracy explicitly before and after switching from pure vector to hybrid retrieval (vector + keyword + reranking). Pure vector: 65% accuracy. Hybrid: 94% accuracy. Hallucination reduction: 78%. Latency cost: +50ms.

**DP-29 — GraphRAG on multi-hop questions: 80%+ vs ~50% for traditional RAG**
Amazon benchmarked GraphRAG vs. traditional RAG on multi-hop questions requiring reasoning across multiple entities. Traditional RAG: ~50% answer correctness. GraphRAG: 80%+ correctness. Pure vector search is structurally unable to follow relationship chains.

**DP-30 — Vectors lose exact information: invoice #123456**
Converting "invoice #123456" to a vector loses the precise number. Semantically similar details collapse into the same neighborhood. Teams discovered this when customers complained that exact ID lookups returned wrong documents.

---

## 6. CONFIGURATION & OPERATIONAL GOTCHAS

**DP-31 — Default configurations failed under production load (e-commerce)**
An e-commerce product discovery team hit memory errors, disk I/O spikes, and search delays shortly after going live. Root cause: no adjusted configuration, write-ahead logs lacked dedicated paths, and the index was too large for available RAM. "Default configurations can fail spectacularly under production loads."

**DP-32 — HNSW indexing during ingestion: CPU saturation**
Enabling HNSW indexing during bulk data upload is a common mistake. The index build competes with write operations for CPU, causing request timeouts in adjacent services. The correct pattern: ingest with indexing disabled, then rebuild the index once in a single post-ingestion pass.

**DP-33 — Unindexed payload fields: full scans at scale**
Missing payload indexes on filtered fields cause expensive full scans of thousands of vectors for every query. This is invisible at small scale and catastrophic at large scale. Teams routinely forget to index fields added after initial deployment.

**DP-34 — Payload schema inconsistency: silent filter failures**
A healthcare team had a status field typed inconsistently across pipelines — string "active" in some records, numeric 1 in others. Result: filters broke silently or returned inconsistent results. No error was thrown; queries just returned wrong data.

**DP-35 — One collection per user: resource waste and index bloat**
A common multi-tenancy mistake is creating one collection per customer. At 10,000+ customers, this creates 10,000+ indexes, overwhelming the vector DB's metadata layer and causing query routing overhead. The correct pattern: single collection with a tenant-filtered `group_id` field.

**DP-36 — Missing replication factor: single-node fragility**
Single-node Qdrant deployments have no failover. A node crash equals downtime. Qdrant's own production guidance: replication factor of 2+ is required for production workloads. Many teams skip this to save cost and pay in availability incidents.

**DP-37 — ORM support gaps for pgvector in multi-tenant setups**
Prisma does not fully support pgvector or table partitioning without workarounds. In multi-tenant deployments where partitioning is the standard approach for keeping indexes manageable per tenant, this gap forces custom SQL or alternative ORMs.

**DP-38 — Kubernetes requirement for production Milvus**
Milvus's multi-node cluster requires managing proxy, coordinator, and storage node components with Kubernetes or container orchestration. Teams without existing Kubernetes expertise face weeks to months of learning curve before going live.

**DP-39 — Vespa: 3-hour schema migration, 800+ line YAML config**
A team running Vespa in production discovered that migrating a schema change across 5 nodes took 3 hours with downtime during index rebalancing. The configuration required 800+ lines of YAML. "Throughput benchmarks ignore operational overhead at 3 AM."

**DP-40 — Running dev/staging alongside production: noisy neighbor**
Teams that run dev and staging workloads on the same cluster as production experience degraded performance and stability. Qdrant's production guidance: isolate production on a dedicated cluster to safeguard against development-induced slowdowns.

---

## 7. BACKUP, RECOVERY & DATA INTEGRITY FAILURES

**DP-41 — Digital publisher: backup restoration revealed index format mismatch**
A digital publisher only discovered their backup was corrupt during an actual disaster recovery attempt. Snapshot restoration revealed an index format mismatch. Partial data loss resulted. They had never tested their backups in a restore scenario.

**DP-42 — "Backup vector indexes — they are not trivial to rebuild"**
Multiple teams discovered that rebuilding a vector index from scratch (when backups fail) is extremely expensive: re-embedding all documents, rebuilding HNSW graphs, and reloading data can take hours to days for large datasets. One practitioner stated: "Backup vector indexes regularly — they are not trivial to rebuild."

**DP-43 — Faiss: no native persistence, full index serialization required**
Teams using Faiss for production workloads discovered it has no native persistence layer. Every restart required deserializing the entire index from disk. Index rebuilding for 5M vectors took 3+ hours. Faiss is appropriate for static research datasets, not operational production systems.

---

## 8. EMBEDDING MODEL MANAGEMENT

**DP-44 — Model upgrade changed vector dimensions: every search returned empty**
A team's data pipeline used an older embedding model outputting 768-dimensional vectors. When they upgraded the production API to the latest model (1536 dimensions), every search returned empty. No error was thrown — dimension mismatch just silently produced no results.

**DP-45 — Embedding drift: model switches make all existing vectors stale**
Switching embedding models (e.g., from `text-embedding-ada-002` to a custom model) makes every existing vector stale. Relevance degrades silently until someone notices search quality declining. Fix requires maintaining versioned indexes and incremental re-embedding with model lineage tracking.

**DP-46 — Notion: re-embedding entire pages on single-character edits**
Notion's pipeline originally re-chunked, re-embedded, and re-uploaded all spans in a page whenever any part of it changed — even a single character. This generated massive unnecessary API cost and write amplification. Fix: two-level hashing (content hash + metadata hash) to detect what actually changed.

**DP-47 — Tokenization errors cause poor embedding quality**
Incorrect tokenization in production vector search pipelines causes embeddings to capture semantics poorly. Example: tokenizers that split entity names incorrectly produce embeddings where the entity is unrecognizable. This degrades recall silently without obvious error signals.

**DP-48 — Embedding costs: <$0.02 per million tokens (OpenAI text-embedding-3-small)**
Production cost benchmarks show OpenAI's small embedding model at under $0.02 per million tokens in 2025. Reranking via cross-encoder adds ~$0.001 per ranking operation. These numbers inform build-vs-buy decisions for embedding infrastructure.

**DP-49 — Self-hosting embeddings: 20–30ms p50 latency improvement**
Notion migrated from third-party embedding APIs to self-hosted Ray-based inference. Result: p50 query latency improved from 70–100ms to 50–70ms. Reason: removed a third-party API hop from the critical path. Side effect: eliminated dependency on provider API stability.

---

## 9. OPERATIONAL COMPLEXITY & HIDDEN COSTS

**DP-50 — The "18-month cliff": failures happen post-launch, not at launch**
Analysis of 35 production vector database deployments found that "the most expensive failures happen not during implementation, but 6–12 months later when these systems need to scale, evolve, and integrate with broader enterprise workflows." Initial POC success consistently masked downstream governance, monitoring, and integration costs.

**DP-51 — Three-system operational burden: cache + operational DB + vector store**
Production AI applications typically require three separate systems: caching for performance (Redis), an operational database for application state (Postgres), and a vector store. That's three systems to deploy, monitor, secure, and keep in sync — tripling operational surface area vs. applications without AI retrieval.

**DP-52 — Index builds took hours and occasionally killed the database**
At pgvector scale (5M+ vectors), index build operations competed with live queries for memory and I/O. Builds occasionally consumed enough resources to trigger OOM kills. Teams had to move index builds to maintenance windows, adding operational complexity that wasn't planned for.

**DP-53 — Customer support startup: intermittent latency spikes from index memory overflow**
A customer support startup's index eventually outgrew available RAM. Queries that hit evicted pages triggered disk fetches, causing intermittent and hard-to-reproduce latency spikes. Fix: RAM upgrade plus quantizing data. Root cause wasn't apparent until profiling disk I/O.

**DP-54 — Only 2 of 8 CPUs in use: configuration limiting performance**
A Qdrant deployment showed only 2 of 8 CPUs actively utilized during heavy load — a sign of misconfiguration. Qdrant documentation notes that default HNSW indexing uses up to 16 threads; teams must explicitly increase this for 16+ core systems.

**DP-55 — 50–60 concurrent upload processes: significant throughput improvement**
Teams running 1–2 concurrent upload processes for vector ingestion saw significant throughput improvements when switching to 50–60 parallel processes. This was not documented in getting-started guides; teams discovered it through production profiling.

---

## 10. MARKET FAILURES & ARCHITECTURAL PIVOTS

**DP-56 — Vector database moat evaporated when cloud platforms added native support**
The standalone vector database market faced existential pressure in 2025 when PostgreSQL (pgvector), Oracle, MongoDB, and cloud platforms added native vector support. The competitive moat that justified standalone vector DB pricing effectively disappeared. By September 2025, multiple analysts called the era of standalone vector databases "over."

**DP-57 — Pinecone leadership change under competitive pressure**
September 2025: Pinecone appointed Ash Ashutosh as CEO; founder Edo Liberty moved to "chief scientist" role. The timing coincided with increasing competitive pressure from integrated solutions. The leadership change was widely interpreted as a signal of strategic difficulty.

**DP-58 — Weaviate pivoted from performance to enterprise governance**
Weaviate's 2025 product direction shifted from speed/performance positioning to enterprise governance and compliance features — a signal that the performance differentiation battle had been lost to commoditization.

**DP-59 — Hybrid retrieval as the actual production winner**
The consensus across 2025 production reports: pure vector search is insufficient for production AI applications. Winning architecture: parallel retrieval (vector + keyword) → metadata filtering → reranking (cross-encoder). The "vector database" category effectively became "vector as one component of a retrieval stack."

**DP-60 — Faiss ≠ production database: library vs. system confusion**
Multiple teams conflated Faiss (a library for approximate nearest neighbor search) with a production-grade database. Faiss has no server, no API, no persistence, no concurrent write safety, and no query planning. Teams discovered these gaps only after committing to Faiss for their first production deployment.

---

## 11. DEBUGGING FAILURE MODES (SYSTEMATIC)

**DP-61 — Cosine similarity threshold 0.95 rejects all production data**
Test data with controlled similarity required threshold 0.95. Real-world production data requires 0.7–0.8. Teams that didn't re-tune after moving to production received zero results from valid queries with no error indication.

**DP-62 — AND filter logic eliminates all results**
Metadata filter using AND logic required documents to match ALL criteria simultaneously. In practice, no documents matched all conditions, returning empty results. Teams assumed this was a database bug; it was a filter logic error.

**DP-63 — Collection/namespace typo: silent empty results**
Systems querying `dev_vectors` in production while data lived in `prod_vectors` returned empty results with no error. Typos in collection names cause silent failures — the database returns zero results rather than an error, making this class of bug difficult to detect without monitoring.

**DP-64 — Normalization inconsistency breaks cosine similarity**
Stored embeddings normalized (L2 norm = 1.0) but query embeddings unnormalized, or vice versa. Cosine similarity calculations produce incorrect distances when normalization is inconsistent, causing systematic relevance degradation without obvious failure signals.

**DP-65 — Zero-vector or null embeddings from silent ingestion failures**
Missing error handling during indexing creates null vectors or zero vectors in the database without alerting developers. Subsequent queries find no matches because zero vectors have no meaningful similarity to real embeddings. Production monitoring must include checks for non-zero embedding values.

---

## 12. NOTION CASE STUDY — QUANTIFIED SAVINGS OVER 2 YEARS

**DP-66 — May 2024: serverless migration, 50% cost reduction**
Notion migrated embeddings from dedicated-hardware pod architecture to serverless, achieving 50% cost reduction from peak usage — "several millions of dollars saved annually."

**DP-67 — January 2025: turbopuffer migration, 60% additional cost reduction**
Migration to turbopuffer as the underlying vector search engine achieved 60% cost reduction in search engine spend plus 35% reduction in AWS EMR compute costs.

**DP-68 — July 2025: page state hashing, 70% data volume reduction**
Implementing two-level hashing (content hash + metadata hash) reduced data volume by 70% by eliminating redundant re-embeddings. Only changed spans were re-embedded; metadata-only changes used cheap PATCH operations.

**DP-69 — July 2025: Ray self-hosted embeddings, 90%+ embeddings infrastructure cost reduction anticipated**
Migrating to self-hosted Ray-based inference eliminated third-party API dependency and reduced embeddings infrastructure costs by 90%+. DynamoDB chosen as state store: one record per page with span hashes, providing fast inserts and lookups.

---

## KEY PATTERNS ACROSS ALL DATA POINTS

1. **Benchmarks lie**: Vendor benchmarks use pre-filtered single-thread synthetic workloads. Real production involves 100+ concurrent clients, messy metadata, and continuous ingestion simultaneously.

2. **Filter performance is the hidden variable**: Metadata filtering performance, not raw similarity search speed, determines production viability at scale.

3. **Costs emerge over 18 months, not at launch**: Embeddings costs, index rebuild costs, schema migration costs, and operational overhead all surface long after initial deployment.

4. **Default configurations are not production configurations**: Every major vector database (Qdrant, Milvus, pgvector) requires significant tuning before production. Default settings optimize for getting started, not for scale.

5. **Backup testing is not optional**: Multiple teams discovered backup failures only during actual recovery attempts.

6. **Semantic similarity is not semantic correctness**: Vector search finds "close" — not "right." Hybrid retrieval with reranking is required for accuracy-critical applications.

7. **The standalone vector DB market consolidated in 2025**: Cloud platforms offering vector as a native feature at 90% lower cost changed the competitive landscape permanently.

---

## Sources

- [Vector Databases Are Dead. Vector Search Is The Future (Medium - Reliable Data Engineering)](https://medium.com/@reliabledataengineering/vector-databases-are-dead-vector-search-is-the-future-heres-what-actually-works-in-2025-e7c9de0490a7)
- [What I Learned About Vector Databases When Production Demands Bite (DEV.to)](https://dev.to/m_smith_2f854964fdd6/what-i-learned-about-vector-databases-when-production-demands-bite-5b79)
- [Vector Databases in Production: Lessons from Building Semantic Retrieval Systems (Medium - Anand Rawat)](https://medium.com/@datarawatai/vector-databases-in-production-lessons-from-building-semantic-retrieval-systems-dfa61e8bbe1b)
- [Vector Database Migration and Implementation: Lessons from 20 Enterprise Deployments (Medium - Aarthy Ramachandran)](https://nimblewasps.medium.com/vector-database-migration-and-implementation-lessons-from-20-enterprise-deployments-027f09f7daa3)
- [Vector Database Operations at Scale: Governance, Monitoring, and Future-Proofing (Medium)](https://nimblewasps.medium.com/vector-database-operations-at-scale-governance-monitoring-and-future-proofing-enterprise-932d781294f2)
- [Two Years of Vector Search at Notion: 10x Scale, 1/10th Cost (Notion Engineering Blog)](https://www.notion.com/blog/two-years-of-vector-search-at-notion)
- [Vector Search in Production (Qdrant)](https://qdrant.tech/articles/vector-search-production/)
- [Vector Search at Scale: Hands-On Lessons from Milvus and LanceDB (Medium)](https://medium.com/@oliversmithth852/vector-search-at-scale-hands-on-lessons-from-milvus-and-lancedb-0c98ef27fa50)
- [Why Your Vector Search Returns Nothing: 7 Reasons and Fixes (Particula)](https://particula.tech/blog/vector-search-returns-nothing-troubleshooting)
- [From Shiny Object to Sober Reality: The Vector Database Story, Two Years Later (VentureBeat)](https://venturebeat.com/ai/from-shiny-object-to-sober-reality-the-vector-database-story-two-years-later)
- [AWS Claims 90% Vector Cost Savings with S3 Vectors GA (VentureBeat)](https://venturebeat.com/data-infrastructure/aws-claims-90-vector-cost-savings-with-s3-vectors-ga-calls-it-complementary)
- [Vector Database Benchmarks are Misleading: What Matters (Actian)](https://www.actian.com/blog/databases/how-to-evaluate-vector-databases-in-2026/)
- [How Airtable Built and Scaled Vector Infrastructure with Milvus (Milvus Blog)](https://milvus.io/blog/productionizing-semantic-search-how-we-built-and-scaled-vector-infrastructure-at-airtable.md)
- [The Hidden Cost of Vector Database Pricing Models (Actian)](https://www.actian.com/blog/databases/the-hidden-cost-of-vector-database-pricing-models/)
- [SQL Server 2025 Vector Database: Why Your $500/Month Server Becomes $3,000 (Azure Noob)](https://azure-noob.com/blog/sql-server-2025-vector-database-production-reality/)
- [Your Vector Database Migration Playbook (Salish Sea Consulting)](https://www.salishseaconsulting.com/blog/vector-database-migration/)
- [Six Lessons Learned Building RAG Systems in Production (Towards Data Science)](https://towardsdatascience.com/six-lessons-learned-building-rag-systems-in-production/)
- [Benchmarks Lie — Vector DBs Deserve a Real Test (Milvus Blog)](https://milvus.io/blog/benchmarks-lie-vector-dbs-deserve-a-real-test.md)
- [Vector Search Isn't the Answer to Everything (TigerData)](https://www.tigerdata.com/blog/vector-search-isnt-the-answer-to-everything-so-what-is-a-technical-deep-dive)
- [Qdrant 2025 Recap: Powering the Agentic Era](https://qdrant.tech/blog/2025-recap/)
- [Exploring Distributed Vector Databases Performance on HPC Platforms: A Study with Qdrant (arxiv)](https://arxiv.org/html/2509.12384v2)
