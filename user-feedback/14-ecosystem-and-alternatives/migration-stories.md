# Migration Stories: Why Companies Left Elasticsearch

## Severity: INFORMATIONAL | Frequency: GROWING

---

## Documented Migrations

### Cost-Driven Migrations

**Company → Typesense**
- From $8,000/month on Elasticsearch to $90/month on Typesense
- 99% cost reduction for search workload

**Arquivei → Grafana Loki**
- 11 Elastic nodes with 2TB SSD each (~$60,000/year)
- Only 30 days of log retention
- Migrated to Loki on cheap object storage (S3)
- Dramatic cost reduction

**Multiple companies → ClickHouse**
- Uber and Cloudflare abandoned ES for ClickHouse for log analytics
- 10-100x faster analytics queries
- 4x cost reduction documented

**Multiple companies → OpenObserve**
- Claims 140x lower storage costs via columnar compression
- ES-compatible APIs for easy migration

### Licensing-Driven Migrations

**Multiple organizations → OpenSearch**
- Triggered by 2021 SSPL license change
- OpenSearch: 496 contributors, 100M+ downloads in first year
- "OpenSearch has become the default choice for new users. It isn't even close."
- Developers report "zero motivation to move back"

### Performance-Driven Migrations

**Companies → Milvus/Qdrant/Weaviate (Vector Search)**
- 30x latency improvement (ES ~200ms vs Milvus ~6ms at 1M vectors)
- 3x throughput improvement
- 15x faster data loading

**Companies → Vespa.ai (Search)**
- 9x higher vector throughput
- 5x faster hybrid search
- 3x faster lexical search

### Operational-Burden Migrations

**GitHub → Custom Architecture**
- Seven-year struggle with ES upgrades
- Administrators had to follow maintenance steps in exactly the right order
- Search indexes became damaged from wrong ordering
- Took years until CCR solved it

**Appear Here → Alternative**
- "Why We Switched from Elasticsearch"
- Operational complexity and maintenance burden cited

---

## Common Migration Patterns

1. **Logs**: ES → ClickHouse or Loki (cost + performance)
2. **Search**: ES → Typesense or Meilisearch (simplicity + cost)
3. **Vector**: ES → Pinecone/Qdrant/Milvus (performance + AI-native)
4. **License**: ES → OpenSearch (Apache 2.0 + trust)
5. **Observability**: ES → Datadog or Grafana stack (operational simplicity)

---

## XERJ.ai Opportunity
Every migration category represents a customer segment for XERJ.ai:
- Cost-driven: 80% cost reduction via compression + Rust efficiency
- License-driven: clear open-source license from day one
- Performance-driven: Rust + SIMD + unified engine
- AI-driven: first-class vector + hybrid + RAG + memory
- Ops-driven: single binary, minimal config, no JVM

The ES-compatible API on port 9200 provides a migration bridge for all these segments.

## Sources
- Medium: The Day Our Elasticsearch Bill Hit $8,000
- Arquivei: Replacing Elasticsearch with Grafana Loki
- HyperDX: Why ClickHouse Over Elasticsearch
- Appear Here: Why We Switched from Elasticsearch
- GitHub Blog: Rebuilt Search Architecture
- Socket.dev: Developers Burned by License Change
- Analytics India Magazine: Why Companies Are Moving Away
- Zilliz/Milvus benchmarks
- Vespa.ai benchmarks
