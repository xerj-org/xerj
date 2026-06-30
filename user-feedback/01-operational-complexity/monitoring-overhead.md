# Monitoring and Observability Overhead

## Severity: MODERATE | Frequency: MODERATE

---

## Core Complaints

### Metric Volume
- Hundreds of interlinked metrics must be tracked: heap usage, GC frequency/duration, thread pool queue sizes and rejections, circuit breaker trips, pending cluster tasks, segment counts, merge rates, indexing/search latency percentiles
- Deep Elasticsearch domain expertise needed to set meaningful alert thresholds
- Generic monitoring tools (Prometheus/Grafana) see numbers, not cluster health

### Additional Infrastructure Required
- Proper monitoring requires: exporter + time-series database + visualization layer
- Three additional components to manage, each with own retention policies, scaling needs, failure modes
- Frequent polling of complex stats endpoints (`_cluster/stats`, `_nodes/stats`) adds measurable load

### Silent Escalation
- Without proper observability, JVM heap spikes or unassigned shards escalate silently into full outages
- By the time alerts fire, the cluster may already be in a degraded state

### SRE Burden
- Some organizations report dedicating 2-3 full-time SREs solely to Elasticsearch cluster management
- On-call burden is significant: "issues commonly include slow performance, yellow/red indexes, and node issues"

---

## XERJ.ai Response
- Built-in Prometheus metrics endpoint (`/v1/metrics`)
- No JVM metrics to monitor (no JVM)
- No shard/cluster state to watch (single-node M1)
- Health endpoint returns simple status
- Meaningful defaults mean fewer knobs to watch

## Sources
- BigData Boutique: Monitoring Tool Selection
- Netdata: Elasticsearch Monitoring
- Mezmo: Scaling the Good Bad and Ugly
