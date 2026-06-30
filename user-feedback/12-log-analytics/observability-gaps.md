# Observability Platform Gaps

## Severity: HIGH | Frequency: HIGH

---

## Architecture Mismatch

### Storage Amplification
- "1TB of disk to store what is effectively 200MB of compressed actual data + 800MB of indexes" -- HyperDX
- 1TB ingested logs → 1.3TB with indexing overhead → 2.6TB with replication
- ES "built for search, not analytics" -- observability needs aggregation, not needle-in-haystack

### Write-Heavy / Read-Light Mismatch
- Observability generates TBs daily but engineers query only during incidents
- "The ratio of queries to writes is completely inverted from traditional search"
- Indexing dominates cost but queries are where value is delivered

### Limited Data Retention
- Teams typically keep only 7-30 days searchable due to cost
- Older data moved to cold storage, "making them practically unsearchable"
- Querying older data requires time-consuming rehydration

---

## Elastic Agent Problems

### 7x Memory Bloat vs Filebeat
- "Elastic-agent is using 1GB memory for monitoring one file" vs "Filebeat using 100-150MB for 3-4 files"
- Unwanted processes spawning automatically (osquerybeat, packetbeat, apm-server)
- "Moving from 150mb to 1GB is not really arguable"

### Kubernetes OOM Crashes
- 85MB+ jump in baseline memory between versions 8.14.0 and 8.15.9
- ECK default memory limit (350Mi) lower than needed (700Mi)
- On high-density K8s nodes (~100 instances), enters crash loops

### CPU Saturation
- VDI deployments: 80-90% CPU and RAM utilization from Elastic Defend
- Fleet agents hitting 100% CPU usage documented repeatedly
- Open meta-issue tracking "countless" efforts to reduce footprint

---

## APM Limitations

### 12x Latency Degradation
- Elastic APM Java agent: page load from 3s to 37s (12x slower)
- Bottleneck in `AbstractLoggingInstrumentation.LoggingAdvice` with large exception objects (~100MB)
- Organization with 600GB logs/day had to disable logging instrumentation

### Queue Saturation
- APM Server queues fill, causing "flush failed (429)" and "flush failed (503)" errors
- Happens when agents collect more data than APM Server can process

### Vendor Lock-In
- Proprietary agents send to APM Server → Elasticsearch
- "If you use Elastic APM, your telemetry data can only be used by Elastic APM"
- OTLP support incomplete: "not all features supported" and "not all agents support this approach"

---

## Competitor Benchmarks

| Metric | ELK | SigNoz | VeloDB/VLK |
|--------|-----|--------|------------|
| Ingestion speed | baseline | 2.5x faster | comparable |
| Aggregation queries | baseline | 13x faster | 10x faster |
| Storage usage | baseline | 50% less | 80% less |
| Cost | baseline | significantly lower | 80% reduction |

### Migration Success Stories
- **Blinkit**: "Spending half our time making sure everything was up and running with ELK and tuning logs so we wouldn't crash" → migrated to Grafana LGTM
- **HackerNoon case study**: ~7x cost reduction moving from Elastic to Grafana (Loki + Tempo)
- **Uber, Cloudflare**: Shifted from ES to ClickHouse for log analytics

---

## SIEM/Security Gaps

- Gartner 2025: Elastic classified as "Visionary" not "Leader" (Splunk is Leader)
- No native SOAR capabilities (Splunk has them)
- Average SOC analyst receives 4,484 alerts/day; 67% go uninvestigated
- Detection rules timeout on high-cardinality fields
- "Limited support for indicator match rules"

---

## XERJ.ai Response
- **Columnar log engine**: purpose-built for analytics, not search
- Domain-aware compression: 2-5x better on log data
- Block-skip indices: aggregation queries touch only relevant blocks
- Native OTLP ingestion: no proprietary agent lock-in
- Single binary: no separate APM server, no agent fleet
- Target: 100K events/sec single-node ingest

## Sources
- HyperDX, Observe, ClickHouse, SigNoz, VeloDB comparisons
- Grafana: Blinkit migration
- HackerNoon: Elastic to Grafana cost case study
- GitHub: APM Agent #1133, Elastic Agent #2756, #4730
- Elastic Forum: Agent overhead #311654
- PeerSpot, Gartner: Elastic Observability reviews
