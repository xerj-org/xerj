# Elasticsearch Clustering Failures — Real User Reports (2024-2026)

## Sources: Elastic Forums, Gartner Peer Insights, HackerNews, Production Incident Reports

---

## Split-Brain Data Loss

> "Lost data in ElasticSearch cluster after disconnected node" — Elastic Forum #11296
> "ElasticSearch dropping data, and not joining after split-brain" — Elastic Forum #11568
> "Network outage keeps split brain status (no recovery by ES)" — Elastic Forum #15873 (was GitHub #5144)
> "How to recover from split brain?" — Elastic Forum #17203

**Pattern:** Network partition → dual masters → conflicting writes → permanent data loss

## Cluster Red Status

> "Primary shards become unassigned... can lead to data loss if another node fails" — OneUptime, Jan 2026
> "An Elasticsearch yellow cluster status indicates compromised data high availability" — Netdata Academy, Sep 2025
> "Ignoring this warning can lead to data loss" — Netdata, Sep 2025

## Rolling Restart at Scale

> "Meltwater: 1,100 nodes, rolling restart took 2 MONTHS" — extreme-scale.md
> "ES 7 reduced 600-node restart to 1 DAY (from weeks)" — extreme-scale.md
> "Each restart → reallocation + state updates + recovery" — cluster engineering teams

## Master Bottleneck

> "Many operations on single JVM thread, growing linearly with shard count" — Meltwater engineering
> "At 90,000 shards: node join/leave detection took minutes per event" — extreme-scale.md
> "Master at 100% CPU is commonly reported" — multiple production teams

## Upgrade-Induced Failures

> "99-node cluster: after upgrading from 6.7.2 to 7.x, cluster repeatedly went yellow/red" — Forum #206843
> "Query 2x slower post-upgrade. P95: <20ms → >60ms. Thread closed UNRESOLVED after 28 days" — Forum #307889
> "5x resource usage after 5.6 → 6.7 upgrade. Old generation GC lasting 21.3 seconds" — e-mc2.net blog

## Gartner Peer Insights (2026, 316 verified reviews)

> "Operational complexity, particularly when managing large clusters or high-ingestion workloads" — Gartner reviewer
> "Rigid upgrade options and lead to big jump on cost to overcome performance issues" — Gartner reviewer
> "Proper tuning and monitoring are essential to maintain performance" — Gartner reviewer
> "In very small dev environments (4gb RAM), Elasticsearch can feel heavy" — Gartner reviewer

## Companies That Abandoned ES Due to Clustering

| Company | Scale | Action |
|---------|-------|--------|
| Uber | 800B docs, 1.5M writes/sec | Built Sia (custom engine) |
| Netflix | 3,500 hosts | Built Raigad (custom orchestration) |
| Slack | High-volume logging | Built KalDB (custom replacement) |
| Meltwater | 3PB, 1,100 nodes | Survived but with custom tooling |
