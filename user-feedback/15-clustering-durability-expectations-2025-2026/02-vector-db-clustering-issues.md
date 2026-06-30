# Vector Database Clustering Issues — Real Reports (2025-2026)

## Sources: GitHub Issues, Release Notes, User Comparisons, Medium Articles

---

## Qdrant Clustering Issues (GitHub, 2025-2026)

### Data Integrity Bugs (from release notes)
- **#7558** — Data race in shard transfers (fixed in 1.13)
- **#7587** — WAL corruption with broken flush edge case (fixed)
- **#7564** — Panic at startup on old clusters with user-defined sharding (fixed)
- **#7400** — Corrupt segments on load if segment was partially flushed (fixed)
- **#7375** — Peer joining cluster with already-used URI breaks cluster (fixed)
- **#8584** — Flaky WAL delta transfer manual recovery (open, Apr 2026)
- **#8357** — Resource leak: shard snapshot temp files not cleaned on persistence failure (open)
- **#8349** — Snapshot consensus freeze test failure (open)

### Clustering Limitations (from docs)
> "The cluster cannot perform operations on collections when one node is down. Operations require >50% of nodes to be running, so this is only possible in a 3+ node cluster." — Qdrant documentation

> "If the data on one of the two nodes is permanently lost or corrupted, it cannot be recovered aside from snapshots. Only 3+ node clusters can recover from permanent loss of a single node." — Qdrant documentation

### User Experience
> "While Qdrant supports distributed deployments, its horizontal scaling features are still evolving compared to more mature systems, and operational tooling around large-scale clustering remains relatively limited." — Cipher Projects comparison, 2025

## Milvus Clustering Issues

### Architecture Complexity
> "Milvus's cloud-native architecture design is powerful for large-scale systems, but it also introduces more operational complexity compared to simpler deployments." — Multiple comparisons, 2025

> "Milvus splits much of its ingestion over separate node types from those that serve query traffic" — Technical comparison

### User Pain Points
> "Real users have reported problems picking Milvus for small prototypes then struggling with operational overhead. Recommendation: start with Pinecone or managed Weaviate for prototyping, migrate to self-hosted only if scale justifies it." — ML Journey, 2025

### External Dependencies
- Requires etcd for metadata
- Requires MinIO/S3 for object storage
- Requires Pulsar/Kafka for log stream
- **4 external systems before a single vector is stored**

## Weaviate Issues

### Scaling Limitations
> "Weaviate's clustered mode supports larger deployments, though scaling is less seamless than Milvus" — Tensor Blue comparison, 2025

### Performance
> "Weaviate shows a slightly higher latency range of 50-70 milliseconds [vs Pinecone 40-50ms]" — Production benchmarks, 2026

## Pinecone Issues

### Vendor Lock-in
> "Pinecone automatically scales resources but limits configuration control — you can't tweak indexing algorithms or storage layers" — Firecrawl comparison, 2026

### Cost at Scale
> "Pinecone is fully managed... [but] expenses rise in direct proportion to search volume growth" — Multiple sources

## Common Pattern: Everyone Has Clustering Problems

| Database | Core Clustering Issue |
|----------|---------------------|
| Elasticsearch | Split-brain, master bottleneck, shard explosion |
| Qdrant | WAL corruption, shard transfer races, 3+ node minimum |
| Milvus | 4 external dependencies, operational complexity |
| Weaviate | Scaling less seamless, higher latency |
| Pinecone | Vendor lock-in, no self-hosting, opaque architecture |
