# Elasticsearch User Feedback & Pain Points

> Collected 2026-04-10 | 41 files | 3,200+ lines | 14 categories
> Sources: G2, Gartner Peer Insights, TrustRadius, PeerSpot, Capterra, Software Advice,
> GitHub Issues, Hacker News, Reddit, DevRant, Team Blind, Stack Overflow, Elastic Forums,
> engineering blogs, Jepsen formal testing, CVE databases, migration case studies, competitor benchmarks

## How This Informs XERJ.ai

Every complaint is a design input for XERJ.ai. Each file includes a "XERJ.ai Response"
section mapping the pain point to our architectural decision.

---

## 01-operational-complexity/ (6 files)
- [cluster-management.md](01-operational-complexity/cluster-management.md) -- #1 complaint: requires dedicated specialists, cluster state bloat, recovery storms
- [learning-curve.md](01-operational-complexity/learning-curve.md) -- Query DSL complexity, 3,000+ settings, fast deprecation cycles
- [monitoring-overhead.md](01-operational-complexity/monitoring-overhead.md) -- Hundreds of metrics, separate monitoring cluster needed, SRE burden
- [community-horror-stories.md](01-operational-complexity/community-horror-stories.md) -- Direct quotes from HN/Reddit: "pain in the ass since day one", 90-hour recovery, death spirals
- [kubernetes-pain.md](01-operational-complexity/kubernetes-pain.md) -- 27 K8s-specific issues: privileged containers, OOM kills, ECK bugs, zone-awareness deadlocks
- [elk-stack-ecosystem.md](01-operational-complexity/elk-stack-ecosystem.md) -- Kibana lock-in, Logstash 500MB-2GB RAM, Filebeat memory leaks, Beats OOM

## 02-cost-and-pricing/ (3 files)
- [infrastructure-costs.md](02-cost-and-pricing/infrastructure-costs.md) -- $8K/month bills, $60K/year for 30 days logs, 85x more RAM than alternatives, hidden 2-3x multiplier
- [pricing-opacity.md](02-cost-and-pricing/pricing-opacity.md) -- Confusing tiers, opaque enterprise quotes, paywalled security, 30% price increase (Jan 2025)
- [industry-specific-costs.md](02-cost-and-pricing/industry-specific-costs.md) -- FinTech ERU "double penalty", healthcare HIPAA gaps, gaming breaches, SIEM alert fatigue, e-commerce relevance

## 03-jvm-and-memory/ (3 files)
- [gc-pauses.md](03-jvm-and-memory/gc-pauses.md) -- 20-40s stop-the-world pauses, cascading failures, 31GB heap ceiling, JDK version regressions
- [oom-incidents.md](03-jvm-and-memory/oom-incidents.md) -- 33GB consumed on empty install, doubling RAM doesn't fix it, circuit breaker leaks, 85x vs alternatives
- [resource-consumption.md](03-jvm-and-memory/resource-consumption.md) -- 50-70% RAM to heap, 15+ thread pools, 12-16 files per segment, 1s refresh I/O storm

## 04-scaling-and-shards/ (5 files)
- [shard-management.md](04-scaling-and-shards/shard-management.md) -- Immutable shard count, over/undersharding, watermarks, no formula for optimal count
- [split-brain.md](04-scaling-and-shards/split-brain.md) -- Data loss from partitions, 2-node clusters unsafe, master bottleneck
- [multi-tenancy.md](04-scaling-and-shards/multi-tenancy.md) -- Noisy neighbor, index-per-tenant shard explosion, no resource quotas
- [forum-upgrade-failures.md](04-scaling-and-shards/forum-upgrade-failures.md) -- 2x-6.6x regressions after upgrades, upgrade traps (can't go forward OR back)
- [extreme-scale.md](04-scaling-and-shards/extreme-scale.md) -- Meltwater 3PB (2-month restarts), Uber 800B docs (built replacement), 2B doc/shard limit, O(N^2) shard scaling

## 05-licensing-and-trust/ (2 files)
- [license-controversy.md](05-licensing-and-trust/license-controversy.md) -- Full Apache→SSPL→AGPL timeline, developer betrayal quotes, stock decline, OpenSearch fork
- [vendor-lockin.md](05-licensing-and-trust/vendor-lockin.md) -- Ecosystem coupling, aggressive sales, demonstrated willingness to change terms

## 06-upgrades-and-migrations/ (3 files)
- [version-upgrades.md](06-upgrades-and-migrations/version-upgrades.md) -- No skip-version, breaking changes, rolling upgrade risks, Zalando/GitHub horror stories
- [mapping-explosion.md](06-upgrades-and-migrations/mapping-explosion.md) -- Dynamic mapping auto-creates fields, cluster state bloat, immutable types
- [ilm-problems.md](06-upgrades-and-migrations/ilm-problems.md) -- Rollover failures, min_age confusion, error halts, security interaction bugs

## 07-query-and-performance/ (5 files)
- [query-performance.md](07-query-and-performance/query-performance.md) -- Deep pagination cliff, version regressions, merge storms, global ordinals, scripting
- [write-performance.md](07-query-and-performance/write-performance.md) -- Eventual consistency, indexing throttling, non-in-place updates, GC data loss
- [data-loss.md](07-query-and-performance/data-loss.md) -- Jepsen 10-25% loss, filesystem bugs, Logstash drops, split-brain, slow restores
- [production-incidents.md](07-query-and-performance/production-incidents.md) -- 30+ incidents: Blinkit, Tideways, Plaid, GOV.UK, ransomware, Radar→Rust, Twitter, Uber, 90-hour recovery
- [data-pipeline-issues.md](07-query-and-performance/data-pipeline-issues.md) -- Kafka connector 70-90% data loss, CDC gaps, bulk failures, ingest DLQ missing, scroll exhaustion

## 08-data-model-limitations/ (1 file)
- [relational-gaps.md](08-data-model-limitations/relational-gaps.md) -- No transactions, no joins, sync burden, not a database

## 09-security/ (2 files)
- [insecure-defaults.md](09-security/insecure-defaults.md) -- 60% of NoSQL breaches, 1.2B records exposed, billions of records in major incidents
- [cves-and-vulnerabilities.md](09-security/cves-and-vulnerabilities.md) -- Log4Shell (CVSS 10.0), Groovy RCE (9.8), Kibana prototype pollution (9.9), 8 high-severity CVEs

## 10-documentation-and-ux/ (3 files)
- [documentation.md](10-documentation-and-ux/documentation.md) -- "Too complex and vague", no reference docs, API-centric, non-technical barriers
- [api-and-client-pain.md](10-documentation-and-ux/api-and-client-pain.md) -- Client libraries "just so bad", no reference docs, constant reorganization, rebranding
- [developer-experience.md](10-documentation-and-ux/developer-experience.md) -- Slow CI, worthless mocks, no migration tooling, nested field traps, black-box debugging

## 11-ai-and-vector-search/ (2 files)
- [vector-search-limitations.md](11-ai-and-vector-search/vector-search-limitations.md) -- 30x slower than Milvus, 4,096 dim limit, HNSW memory wall, broken hybrid scoring, commercial license gates
- [rag-and-agent-gaps.md](11-ai-and-vector-search/rag-and-agent-gaps.md) -- No chunking, no inline embedding, no agent memory, no contextual retrieval -- XERJ.ai's unique opportunity

## 12-log-analytics/ (2 files)
- [log-costs.md](12-log-analytics/log-costs.md) -- $60K/year for 30 days, 8TB storage for 1TB data, ClickHouse 10-100x faster, Loki 10x cheaper
- [observability-gaps.md](12-log-analytics/observability-gaps.md) -- Elastic Agent 7x memory vs Filebeat, APM 12x latency degradation, SIEM "Visionary" not "Leader", migration stories

## 13-vendor-and-support/ (1 file)
- [business-practices.md](13-vendor-and-support/business-practices.md) -- "Cowboy tactics" during renewal, feature offloading (v9 web crawler), limited trials

## 14-ecosystem-and-alternatives/ (3 files)
- [migration-stories.md](14-ecosystem-and-alternatives/migration-stories.md) -- Typesense (99% savings), Loki, ClickHouse, Milvus/Qdrant/Weaviate, GitHub 7-year struggle
- [competitor-benchmarks.md](14-ecosystem-and-alternatives/competitor-benchmarks.md) -- Vector: 30x latency gap. Logs: 10-100x query speed gap. Search: 99% cost gap.
- [opensearch-comparison.md](14-ecosystem-and-alternatives/opensearch-comparison.md) -- Fork proves market wants alternatives; 13 shared Lucene/JVM limitations neither can fix

---

## Source Index

| Source Type | Count | Examples |
|-------------|-------|---------|
| Review platforms | 6 | G2, Gartner, TrustRadius, PeerSpot, Capterra, Software Advice |
| Developer communities | 4 | Hacker News, Reddit, DevRant, Team Blind |
| GitHub issues | 30+ | elastic/elasticsearch, elastic/kibana, elastic/beats, confluent connectors |
| Elastic forums | 20+ | discuss.elastic.co performance, OOM, upgrade, ILM threads |
| Engineering blogs | 25+ | Zalando, GitHub, Uber, Cloudflare, Radar, Plaid, Blinkit, GoCardless, Meltwater |
| Formal testing | 1 | Jepsen/Aphyr: Elasticsearch 1.5.0 |
| CVE databases | 10+ | NVD, stack.watch |
| Competitor analyses | 15+ | ClickHouse, Milvus, Vespa, Loki, OpenObserve, Weaviate, SigNoz |
| Analyst reports | 3 | Gartner Magic Quadrant SIEM, Gartner Peer Insights, Trail of Bits benchmark |
