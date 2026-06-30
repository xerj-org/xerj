# ELK Stack Ecosystem Pain (Kibana, Logstash, Beats)

## Severity: HIGH | Frequency: HIGH

---

## Kibana

### Performance
- Dashboard rendering: ~3,500ms at p75, dominated by long-running ES queries
- Users consistently report "Kibana is extremely slow" loading graphs and pages
- Crashes from high memory usage with large reporting jobs or detection rules
- Must manually tune `--max-old-space-size` in node.options

### UX
- UI described as "very cluttered and clunky" in Kibana 7+ (GitHub #40719)
- Forum post titled: "Kibana is helpful but interface is very horrible"
- Visualization row limits: originally 100, raised to 1,000 -- still no unlimited option
- Discover view "very slow" after upgrades due to disabled HTTP compression

### Lock-in
- Can ONLY connect to Elasticsearch as data source
- Cannot build cross-source dashboards (no Prometheus, InfluxDB, PostgreSQL)
- Plugin version must match Kibana version exactly

### Metrics
- Focus is on log analysis; metrics support is weak compared to Grafana
- No native Prometheus, InfluxDB, or other metric source support

---

## Logstash

### Resource Consumption
- Uses 500MB-2GB RAM and 20-40% CPU
- Compare: Vector (Rust) uses 100-200MB RAM, 10-20% CPU
- Compare: Fluent Bit uses 50-100MB RAM, 5-15% CPU

### Memory Leaks
- JRuby frozen/deduped strings never freed when pipelines use dynamic hash keys
- Memory grows linearly until OOM crash (GitHub #14281)
- Complex grok patterns consume excessive memory

### Event Dropping
- Under backpressure (ES slow/unavailable), Logstash drops events
- No built-in event-level traceability for debugging drops
- If it crashes or buffer overflows, all queued events lost

### DX
- No GUI; CLI-only configuration
- Pipeline debugging: must run in debug mode, manually add plugin IDs
- No step-through debugger or event inspector
- Documentation described as "awful. Deprecated shit everywhere."

### Being Replaced
- Vector (Rust) outperforms on IO throughput, memory, disk writes
- CNCF recommends evaluation of Fluent Bit and Vector as alternatives
- Organizations actively migrating away

---

## Beats (Filebeat, Metricbeat)

### Memory Issues
- Filebeat consuming 6GB+ within minutes on high-volume multi-module configs
- OOMKilled after 8.x upgrades across 8.6.2, 8.7.1, and later (GitHub #35796)
- Filebeat does NOT release allocated memory until process restart
- Disk queues cause 30% more memory than memory queues

### Configuration
- Official `preset: throughput` mode causes 10x performance REGRESSION
- Users must ignore official recommendations and hand-tune
- Running multiple modules simultaneously triggers memory leaks not present with single modules
- No documentation warning about module incompatibilities

---

## Stack-Wide Issues

### Total Cost
- Midsize deployment (100GB/day → 1TB/day over 3 years): ~$1.9M on AWS
- 100GB/day standard config: ~$180K/year hosting + at least 1 FTE for maintenance
- 30% price increase effective January 2025

### Fragmented Observability
- Logs, metrics, and traces remain disconnected across ELK components
- Unified visibility requires extensive manual configuration and correlation

### "Full-Time Logging Administrator"
- Multiple sources describe ELK as turning engineers into full-time logging administrators
- Operational burden becomes a permanent staffing requirement

### Dramatically Slower Than Alternatives
- SigNoz: 2.5x faster ingestion, 13x faster aggregation queries, 50% less storage
- Better Stack (ClickHouse): sub-second queries across billions of records, 97% lower cost

---

## XERJ.ai Response
- **No Kibana needed** for basic operations (REST API + prometheus metrics)
- **No Logstash needed** -- native OTLP, Syslog, JSON ingestion built in
- **No Beats needed** -- gRPC streaming ingest for high-throughput
- Single binary replaces 4+ ELK components
- No JRuby, no Node.js, no JVM -- just Rust
- Unified search + vector + logs in one query interface

## Sources
- Kibana GitHub: #123591, #40719
- Logstash GitHub: #14281
- Beats GitHub: #35796, #26342
- SigNoz, Better Stack, Plural.sh, Dash0 comparisons
- Mezmo: True Cost of Elastic Stack
- ChaosSearch: ELK Pros and Cons
