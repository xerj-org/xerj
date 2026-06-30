# 15. Clustering & Durability Expectations (2025-2026)

Research collected: April 2026

## Files

1. **01-elasticsearch-clustering-failures.md** — Real ES clustering failures from forums, Gartner, and production incidents. Split-brain data loss, master bottleneck, rolling restart nightmares.

2. **02-vector-db-clustering-issues.md** — Qdrant GitHub issues (WAL corruption, shard transfer races), Milvus operational complexity (4 external deps), Weaviate scaling limits, Pinecone lock-in.

3. **03-user-expectations-2025-2026.md** — What users actually want in 2025-2026: compute-storage separation, zero-downtime everything, automatic sharding, AI-native operations, serverless pricing.

4. **04-how-competitors-cluster.md** — How ES, Qdrant, Milvus, Quickwit, TiKV, CockroachDB implement clustering. Analysis of which approach is best for XERJ.ai.

## Key Findings

- **ES clustering breaks at scale** — Uber, Netflix, Slack all abandoned or heavily customized it
- **Vector DBs have clustering bugs** — Qdrant has open WAL corruption + shard transfer issues in 2026
- **Users want S3-native + serverless** — not more node management
- **Best modern pattern:** Embedded Raft for metadata + S3 for segments + stateless compute
- **AI world expectation:** unified text+vector+hybrid search, not separate databases

## Sources
- Elastic Forums (#11296, #11568, #15873, #17203, #206843, #307889)
- Qdrant GitHub (#7375, #7400, #7558, #7564, #7587, #8349, #8357, #8584, #8630)
- Gartner Peer Insights (316 ES reviews, 2026)
- HackerNews discussions (vector DB threads, 2024-2025)
- Production reports: Meltwater, Uber, Netflix, Botify, Slack
- Cloud-native surveys: RTInsights, Tasrie IT, Rapydo
