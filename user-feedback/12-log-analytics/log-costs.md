# Log Analytics: Cost and Efficiency Problems

## Severity: HIGH | Frequency: HIGH

---

## Core Complaints

### Storage Cost Explosion
- 1TB uncompressed data → ~8TB EBS storage at ~$100/TB/month plus compute nodes
- Compressed inverted indexes frequently outweigh the compressed source data
- Full-fidelity data retention is prohibitively expensive
- Hidden costs: EBS volumes, cross-zone data transfer, monitoring infrastructure
- **Real-world**: 11 Elastic nodes x 2TB SSD = ~$60,000/year for only 30 days of log retention (Arquivei)

### Architectural Mismatch
- ES treats logs as regular documents in an inverted index
- This is why it needs 50-node clusters for log workloads
- Logs are time-series columnar data, NOT documents
- ES "was built on the belief that observability was just a search problem -- but observability is an analytics problem" (ClickHouse)

### Write-Heavy Workload Inefficiency
- Observability generates TBs of telemetry daily
- Engineers only query during incidents
- ES's write-heavy, read-light pattern is fundamentally inefficient for logs
- Frequent updates/deletes trigger segment merging overhead
- Default 1-second refresh creates massive I/O overhead for log-volume writes

### Resource Consumption
- "One of the biggest complaints of using Elasticsearch is that it hogs a lot of resources" (SigNoz)
- JVM heap sizing, GC tuning, circuit breaker thresholds need constant attention
- Memory-intensive aggregations impact cluster performance
- High-cardinality fields strain inverted indexes

### Analytics Limitations
- Shard-based architecture limits query parallelization to within shard boundaries
- 50GB max shard size forces horizontal scaling
- JVM heap caps (~64GB) necessitate horizontal sprawl
- Term aggregations on high-cardinality data are ESTIMATES, not exact counts
- Cannot unify logs, metrics, and traces in single system
- No native metrics support; never designed for analytical workloads on structured time-series data
- No full SQL support (joins, window functions, subqueries, CTEs)

---

## Competitor Advantages

| Competitor | Key Advantage Over ES for Logs |
|-----------|-------------------------------|
| **ClickHouse** | 10-100x faster analytics queries, 2x+ better compression, full SQL, 4x cost reduction |
| **Grafana Loki** | Indexes only metadata labels (not full content), cheap object storage (S3/GCS) |
| **OpenObserve** | Claims 140x lower storage costs via columnar compression, ES-compatible APIs |

### Migration Stories
- **Uber** and **Cloudflare** abandoned ES for ClickHouse due to log volume limitations
- **Arquivei**: replaced ES with Grafana Loki, dramatic cost reduction
- Alternative solutions deliver 60-90%+ infrastructure cost reductions consistently

---

## XERJ.ai Response
- **Dedicated columnar log engine** (not documents in an inverted index)
- Same-type columns compress dramatically better than row-oriented JSON
- Domain-aware compression: template extraction, delta encoding, dictionary encoding
- Target: 2-5x better compression than LZ4/zstd on log data
- Block-skip indices: range queries skip irrelevant blocks without decompression
- Time-partitioned segments: efficient time-range queries
- OTLP, Syslog, JSON ingestion: native log format support
- Target: 100K events/sec single-node ingest throughput

## Sources
- SigNoz: Loki vs Elasticsearch
- ClickHouse: Elastic for Observability comparison
- HyperDX: Why ClickHouse Over Elasticsearch
- Arquivei: Replacing Elasticsearch with Grafana Loki
- OpenObserve: Elasticsearch Alternatives
- ChaosSearch: Switching from ELK Stack
