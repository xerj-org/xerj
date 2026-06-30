# What Users Actually Expect — Clustering & Durability (2025-2026)

## Sources: Gartner, HackerNews, Cloud-Native Surveys, Industry Analysis

---

## 1. COMPUTE-STORAGE SEPARATION (The #1 Demand)

> "The key defining characteristic of modern platforms in 2025 is the decoupling of storage and compute, in contrast to older monolithic architectures where storage capacity was inextricably linked to compute power in fixed-size clusters." — RTInsights, 2025 Cloud Database Market Review

> "A cloud native database in 2026 must support horizontal scaling, automated failover, declarative management through Kubernetes operators, and seamless integration with modern observability stacks." — Tasrie IT, Cloud Native Database Guide 2026

**What users want:**
- Scale search compute independently from storage
- Pay for compute only when queries run (serverless)
- Store data in cheap object storage (S3/GCS), cache on fast NVMe
- Add/remove query nodes without data migration

**Who does this today:** Snowflake (data), Quickwit (search), Milvus (vectors), Neon (Postgres)

## 2. ZERO-DOWNTIME EVERYTHING

> "Serverless and auto-scaling DBaaS let teams match spend to demand while offloading capacity planning and operations." — Multiple cloud-native surveys, 2025

**What users want:**
- Zero-downtime upgrades (ES takes weeks/months)
- Zero-downtime scaling (add nodes without rebalancing storms)
- Zero-downtime schema changes (no reindex)
- Instant failover (not 30-60 second restarts)

**Who does this today:** CockroachDB (SQL), PlanetScale (MySQL), Neon (Postgres)

## 3. AUTOMATIC SHARDING & REBALANCING

> "Oversharding wastes heap and CPU while undersharding throttles throughput and recovery speed, and shard count is one of the most important (and hardest to fix later) architectural decisions in Elasticsearch." — Pureinsights, Top 7 ES Pitfalls, 2025

**What users want:**
- Don't make me choose shard count at index creation
- Auto-split when data grows
- Auto-merge when shards are too small
- Rebalance without O(N) data movement

**Who does this today:** TiKV (regions auto-split/merge), CockroachDB (ranges), DynamoDB (partitions)

## 4. STRONG CONSISTENCY WITHOUT COMPLEXITY

> "Data products must meet specific Service Level Objectives (SLOs) for quality, freshness, discoverability, and accessibility, using clear data contracts." — Cloud-native DB survey, 2025

**What users want:**
- Write a document → immediately searchable (not 1 second later)
- No "eventual consistency" surprises
- No split-brain possibility
- Clear SLAs: "99.99% availability, zero data loss"

**Who does this today:** Raft-based systems (etcd, TiKV, CockroachDB)

## 5. AI-NATIVE OPERATIONS (New in 2025-2026)

> "Vector databases are the wrong abstraction" — HackerNews discussion, Nov 2024 (500+ comments)
> "What place do vector-native databases have in 2025? I feel using pgvector or Redis is enough" — HackerNews, 2025

**What users want:**
- Don't make me run a separate vector database
- Unified search: text + vectors + filtering in ONE query
- Inline embedding (send text, get vector search — no external pipeline)
- Agent memory (store/recall for LLM agents)
- RAG-ready retrieval (chunking + parent-child + citation tracking)

**Who does this today:** Nobody fully. ES has basic kNN. Qdrant has vectors only. XERJ.ai is designed for this.

## 6. OPERATIONAL SIMPLICITY

> "Managing an Elasticsearch cluster requires significant expertise, with administrators needing to understand concepts like shards, replicas, and node roles, and handle capacity planning and performance tuning." — Gartner Peer Insights, 2026

> "Elasticsearch requires a deep technical understanding to set up, optimize, and manage properly." — Meilisearch ES Review, 2025

**What users want:**
- Single binary that just works
- No JVM tuning, no heap sizing, no GC optimization
- No shard management, no replica configuration
- Config that fits on one screen (not 3,000+ settings)
- Upgrade = replace file, restart

## 7. CONSUMPTION-BASED PRICING

> "Systems introduce metrics like Request Units (RUs) to quantify resource consumption per request, enabling consumption-based pricing." — Cloud RDBMS Innovations, 2025

**What users want:**
- Pay per query, not per node-hour
- No idle costs when nobody is searching
- Transparent pricing (not "contact sales for enterprise")
- No ERU (Elastic Resource Unit) licensing games

---

## SUMMARY: The Ideal Search Database in 2026

Based on all user feedback collected:

1. **Single binary** that starts in milliseconds, not minutes
2. **Compute-storage separation** — S3 for data, ephemeral compute for queries
3. **Automatic sharding** — no shard count decisions, auto-split/merge
4. **Raft consensus** — no split-brain, proven algorithm
5. **Instant write visibility** — no refresh delay
6. **AI-native** — text + vector + hybrid in one query
7. **WASM pipelines** — user-defined transforms at ingest speed
8. **38 settings, not 3,000** — works out of the box
9. **Serverless pricing** — pay per query, not per node
10. **Zero-downtime everything** — upgrades, scaling, schema changes

**This is exactly what XERJ.ai v2 is designed to be.**
