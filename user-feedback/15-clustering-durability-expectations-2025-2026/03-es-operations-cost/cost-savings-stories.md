# Elasticsearch Cost Savings Stories: Real Migration Data Points

Compiled: April 2026  
Sources: 8 targeted web searches + 15+ page fetches  
Data points collected: 70+

---

## 1. HEADLINE CASE STUDIES WITH SPECIFIC NUMBERS

### 1.1 Elastic Internal (Elasticsearch → Elasticsearch 7.15 Upgrade)
- **Savings: $1.2M/year ($100K/month, $3,500/day)**
- Inter-node traffic cut from 464 TB/day → 204.5 TB/day (56% reduction)
- Compression via lz4 achieved 70%+ reduction in specific cluster scenarios
- 207 production clusters across 4 cloud providers
- 1.2 trillion documents, ~1 TB/day ingest
- Source: [Elastic Blog](https://www.elastic.co/blog/elastic-observability-clusters-upgrade-latest-release-save-money)

### 1.2 Octus (Elasticsearch on Elastic Cloud → Amazon OpenSearch Service)
- **85% infrastructure cost reduction** (headline claim, zero-downtime migration)
- **52% cost reduction** achieved in the initial migration phase alone
- Further savings available through continued optimization post-migration
- Source: [AWS Big Data Blog](https://aws.amazon.com/blogs/big-data/how-octus-achieved-85-infrastructure-cost-reduction-with-zero-downtime-migration-to-amazon-opensearch-service/)

### 1.3 ProcessOut (Elasticsearch → ClickHouse Cloud)
- **Two-thirds (66%) reduction in analytics costs**
- Latency: minutes (cron-job-based) → ~2 seconds end-to-end
- Data volume: 35 TB of payment data, billions of transactions annually
- Single backend engineer can maintain the system (reduced operational burden)
- Source: [ClickHouse Blog](https://clickhouse.com/blog/processout-switched-from-elasticsearch-to-clickhouse-cloud)

### 1.4 Contentsquare (Elasticsearch → ClickHouse)
- **11x cheaper infrastructure cost** vs. previous Elasticsearch setup
- 10x improvement in p99 query latency
- Extended historical data from 1 month → 3 months queryable
- Extended retention from limited → 13 months
- Pre-migration setup: 14 ES clusters × 30 nodes each (m5.4xlarge with network-attached disks)
- Source: [ClickHouse Blog](https://clickhouse.com/blog/contentsquare-migration-from-elasticsearch-to-clickhouse)

### 1.5 Didi (Elasticsearch → ClickHouse — Log Storage System)
- **>30% reduction in hardware/machine costs**
- ClickHouse cluster: 400+ physical nodes at peak
- Peak write traffic: 40+ GB/s
- Daily query volume: ~15 million queries/day, ~200 QPS peak
- Query speed: ~4x faster than Elasticsearch; P99 latency <1 second
- Daily data: petabyte-level log generation
- Restart time: 1 hour → 1 minute (metadata optimization)
- Source: [ClickHouse Blog](https://clickhouse.com/blog/didi-migrates-from-elasticsearch-to-clickHouse-for-a-new-generation-log-storage-system)

### 1.6 Unnamed Company (Elasticsearch → Quickwit)
- **5x lower compute costs**
- **2x lower storage costs**
- Retention period: 3 days → 30 days (10x increase in retention at lower cost)
- Workload: hundreds of terabytes of logs ingested daily
- Source: [Mezmo / Quickwit comparison](https://www.mezmo.com/learn-observability/quickwit-vs-elasticsearch-choosing-the-right-search-tool)

### 1.7 Cloudflare (Elasticsearch → ClickHouse)
- **10x storage reduction**: document size 600 bytes → 60 bytes per record
- Enabled storing 100% of logs at 35–45 million requests/second within the same budget
- Source: [BigData Boutique ClickHouse vs ES](https://bigdataboutique.com/blog/clickhouse-vs-elasticsearch)

### 1.8 Uber (Elasticsearch → ClickHouse)
- **50%+ reduction in cluster footprint**
- Maintained or improved query throughput and write performance
- Source: [BigData Boutique ClickHouse vs ES](https://bigdataboutique.com/blog/clickhouse-vs-elasticsearch)

### 1.9 Unnamed Large US Data Management Company (Splunk → Elastic)
- **50% cost reduction** on observability and security
- Faster MTTR after consolidation
- Source: [Elastic Splunk Replacement](https://www.elastic.co/splunk-replacement/migration-guide)

### 1.10 Unnamed Elastic Customer (Splunk → Elastic)
- **60% cost reduction**
- 40% faster analytics response time
- Source: [Elastic Splunk Replacement](https://www.elastic.co/splunk-replacement/migration-guide)

### 1.11 Trip.com (Elasticsearch → ClickHouse)
- **4x–30x faster query performance** (range across query types)
- Source: [Tinybird ClickHouse vs ES](https://www.tinybird.co/blog/clickhouse-vs-elasticsearch-search)

---

## 2. FINANCIAL MODELING & TCO DATA POINTS

### 2.1 Elasticsearch Hot-Warm-Cold Architecture Savings (Elastic Blog)
- **Traditional single-tier cluster**: $3,772/month (5.184 TB, 232 GB RAM)
- **Hot-Warm with ILM**: $1,491/month — **60% cost reduction**
- **Hot-Warm with ILM + Data Rollups**: $1,025/month — **73% total reduction** vs. baseline
- Warm-tier data reduced from 1.99 TB → 5.52 GB via rollup operations
- Source: [Elastic Blog - Cost Saving Strategies](https://www.elastic.co/blog/cost-saving-strategies-for-the-elasticsearch-service-data-storage-efficiency)

### 2.2 Elastic Cloud → OpenSearch Managed Comparison (Production Cluster)
- Elastic Cloud Gold tier comparable cluster: **$1,200–$1,800/month**
- AWS OpenSearch (3x r6g.xlarge data nodes + 500 GB): **~$1,150/month on-demand**
- With 1-year Reserved Instances: **~$790/month** (~32% further savings)
- Self-managed on EC2 (3x r6g.xlarge + 3x c6g.large + 1.5 TB EBS): **~$1,092/month** visible costs
- Source: [BigData Boutique Pricing Guide](https://bigdataboutique.com/blog/opensearch-and-elasticsearch-pricing-guide)

### 2.3 Apache Doris vs. Elasticsearch at 100 TB/day Scale
- **Elasticsearch**: ~$200,000/month (100 TB/day, 30-day retention, 3-day hot)
- **Apache Doris (VeloDB Cloud)**: ~$27,000/month (same workload)
- **Savings: ~$173,000/month (~$2.07M/year), 86.5% reduction**
- Cloud Log Services baseline: ~$190,000/month
- Apache Doris write speed: 5x faster; query speed: 2x faster vs. ES
- CPU usage: 70% less than Elasticsearch under identical ingest loads
- Source: [VeloDB Blog](https://www.velodb.io/blog/elasticsearch-clickhouse-apache-doris-powers-observability-better)

### 2.4 Elastic's Own ROI Analysis (OpenSearch → Elasticsearch Enterprise)
- **593% ROI** (including hard + soft benefits)
- **145% ROI** (hard benefits only — infrastructure cost avoidance + procurement)
- **$1.9M+ in net hard and soft benefits** per modeled deployment
- Model assumes: 100 GB/day log ingest, 7-day hot / 23-day cold / 60-day frozen storage
- 7.8 work days/year saved per end-user employee in productivity
- Source: [Elastic Blog - ROI OpenSearch Migration](https://www.elastic.co/blog/return-on-investment-migrating-off-opensearch-search-logging)

### 2.5 Self-Managed Elasticsearch "Hidden Costs" Rule of Thumb
- Operational costs typically run **2–3x the visible infrastructure spend**
- 3-year TCO for modest ELK stack: ~$2,000,000
- Source: [OpenObserve Blog](https://openobserve.ai/blog/elasticsearch-alternatives/) / [Meilisearch Blog](https://www.meilisearch.com/blog/elasticsearch-pricing)

---

## 3. ELASTICSEARCH PRICING REFERENCE DATA POINTS

### 3.1 Elastic Cloud Pricing (2025)
- Minimum viable deployment: **$16.40/month** (down from former $45/month, >60% reduction in entry price)
- Standard tier: **~$95/month**
- Gold tier: **~$109–$114/month**
- Platinum tier: **~$125–$131/month**
- Enterprise tier: **$175+/month**
- Production clusters: commonly **$500–$2,000+/month**
- Large clusters: **$2K–$5K–$7K+/month**
- Enterprise licensing for mid-sized deployments: **high five figures annually**
- Large environments: **>$500,000/year** total cost
- Elastic Consumption Units (ECUs): **$1.00 per ECU**
- Serverless ingest rates (high volume): as low as **$0.11/GB**
- **Price increase Jan 27, 2025**: ~30% increase for typical production workloads
- Source: [Quesma Pricing](https://quesma.com/blog/elastic-pricing/) / [Airbyte Guide](https://airbyte.com/data-engineering-resources/elasticsearch-pricing) / [Meilisearch ES Review](https://www.meilisearch.com/blog/elasticsearch-pricing)

### 3.2 AWS OpenSearch Service Instance Pricing (2025)
- t3.medium.search: **$0.073/hr (~$53/month)**
- t3.small: **$0.038/hr**
- m6g.large.search: **$0.128/hr (~$93/month)**
- m6g vs m5 equivalent: ~**20% cheaper**
- c6g.large.search: **$0.113/hr (~$82/month)**
- r6g.large.search: **$0.167/hr (~$122/month)**
- r6g.xlarge.search: **$0.335/hr (~$245/month)**
- r6g.2xlarge.search: **$0.670/hr (~$489/month)**
- or1.medium.search (OpenSearch Optimized): **$0.105/hr**
- Source: [BigData Boutique](https://bigdataboutique.com/blog/opensearch-and-elasticsearch-pricing-guide) / [Lucidity](https://www.lucidity.cloud/blog/aws-opensearch-pricing)

### 3.3 AWS OpenSearch Storage Pricing
- Hot SSD (gp3): **$0.08–$0.10/GB/month**
- Provisioned IOPS SSD (io1): **$0.125/GB/month + $0.065/provisioned IOPS/month**
- UltraWarm (S3-backed): **$0.024–$0.03/GB/month**
- Cold storage: **$0.01/GB/month**
- Cross-AZ data transfer: **$0.01/GB each direction**
- Hot → UltraWarm savings: **~70% storage cost reduction**
- Hot → Cold storage savings: **~88% storage cost reduction**
- Source: [BigData Boutique](https://bigdataboutique.com/blog/opensearch-and-elasticsearch-pricing-guide) / [Lucidity](https://www.lucidity.cloud/blog/aws-opensearch-pricing)

### 3.4 OpenSearch Serverless Pricing
- Rate: **$0.24/OCU-hour** (indexing and search, US regions)
- Minimum production floor: 2 OCUs = **~$350/month**
- Dev/test minimum: 1 OCU = **~$175/month**
- S3 storage: **$0.024/GB/month**
- Ingestion: **$0.24/OCU-hour**
- Direct Query: **$8–$10/TB scanned**
- Source: [BigData Boutique](https://bigdataboutique.com/blog/opensearch-and-elasticsearch-pricing-guide)

### 3.5 Reserved Instance Discounts on OpenSearch
- 1-year No Upfront: **31% savings**
- 1-year All Upfront: **35% savings**
- 3-year No Upfront: **48% savings**
- 3-year All Upfront: **52% savings**
- Source: [BigData Boutique](https://bigdataboutique.com/blog/opensearch-and-elasticsearch-pricing-guide)

---

## 4. ARCHITECTURAL COST LEVERS & BENCHMARKS

### 4.1 Storage Compression Ratios
- Elasticsearch: **~1.5:1** compression ratio
- ClickHouse: **10:1 to 20:1** (typical log analytics workloads)
- ClickHouse range: **10:1 to 100:1** (depending on data types)
- Apache Doris: **5:1 to 10:1** (indexes included)
- Storage implication: ClickHouse typically uses **12x–19x less disk** than ES for same raw data
- Source: [BigData Boutique](https://bigdataboutique.com/blog/clickhouse-vs-elasticsearch) / [VeloDB](https://www.velodb.io/blog/elasticsearch-clickhouse-apache-doris-powers-observability-better)

### 4.2 Spot Instances for Elasticsearch Nodes
- Potential savings: **up to 90%** vs. on-demand pricing
- Source: [CloudKeeper](https://www.cloudkeeper.com/insights/blog/saving-aws-elasticsearch-costs-search-service-optimization-strategies)

### 4.3 AWS OpenSearch Data Lifecycle Savings
- Tiered storage approach: **50–80% storage cost reduction**
- Rollups for time-series data: **90%+ storage savings** with minimal query performance impact
- Source: [Lucidity](https://www.lucidity.cloud/blog/aws-opensearch-pricing)

### 4.4 AWS Graviton2 / ARM Instance Price-Performance
- m6g vs. equivalent Intel on OpenSearch: **>20% improvement in price-performance**
- Google Compute N2 vs. previous generation: **>20% improvement in price-performance**
- OpenSearch Optimized instances vs. general-purpose: **30–40% better price-performance**
- Source: [Elastic Blog - Top 5 Cost Optimization](https://www.elastic.co/blog/top-5-ways-to-optimize-your-elastic-cloud-costs)

### 4.5 Elasticsearch JVM Memory Overhead
- ES requires **50–70% of available RAM** reserved for JVM heap management
- This effectively doubles compute cost for memory-bound workloads
- Source: [OpenObserve Blog](https://openobserve.ai/blog/elasticsearch-alternatives/)

### 4.6 OpenSearch vs. Elasticsearch Security Cost Difference
- OpenSearch: advanced security (RBAC, field-level access control, audit logging) = **free**
- Elasticsearch: same features require **Platinum or Enterprise tier** (significant premium over Standard)
- Source: [BigData Boutique 2025 Update](https://bigdataboutique.com/blog/opensearch-and-elasticsearch-pricing-guide)

### 4.7 S3 Glacier / Frozen Tier Savings
- S3 Glacier vs. standard S3: **~95% cheaper**
- Frozen tier vs. hot gp3: **$0.024/GB/month vs. $0.08+/GB** = **~70% reduction**
- Source: [Elastic Blog - S3 Glacier](https://www.elastic.co/search-labs/blog/s3-glacier-archiving-elasticsearch-deepfreeze)

### 4.8 Apache Doris Resource Efficiency vs. Elasticsearch
- CPU usage: **70% less** under identical ingest load
- Write speed: **5x faster** sustained at 10 GB/s
- Aggregation performance: **6–21x better** than Elasticsearch
- Full-text search: **3–10x faster** than ClickHouse
- Source: [VeloDB Blog](https://www.velodb.io/blog/elasticsearch-clickhouse-apache-doris-powers-observability-better)

---

## 5. ALTERNATIVE TOOL PRICING BENCHMARKS

### 5.1 Meilisearch Cloud
- Build plan: **$30/month** (50K searches, 100K documents)
  - Overages: $0.40/1K searches, $0.30/1K documents
- Pro plan: **$300/month** (250K searches, 1M documents)
  - Overages: $0.30/1K searches, $0.20/1K documents
- Enterprise: quote-based
- Self-hosted: free
- Source: [Meilisearch Blog](https://www.meilisearch.com/blog/elasticsearch-pricing)

### 5.2 OpenObserve
- Self-hosted: free
- Developer managed: **free** (200 GB/month ingest)
- Pro managed: **$19/month** (unlimited users, multi-tenancy)
- Business: **$199/month**
- Claims: **140x lower storage costs** than Elasticsearch; **4x fewer resources** for equivalent query perf
- Source: [OpenObserve Blog](https://openobserve.ai/blog/elasticsearch-alternatives/)

### 5.3 Better Stack (Logtail)
- Paid plans start at **$29/month**
- Claims costs up to **10x lower than Datadog**
- Source: [Better Stack](https://betterstack.com/community/comparisons/elasticsearch-alternative/)

### 5.4 Algolia
- Build: $1/month (10K searches)
- Growth: $0.50/1K requests
- Enterprise: custom pricing
- Source: [OpenObserve Blog](https://openobserve.ai/blog/elasticsearch-alternatives/)

### 5.5 Manticore Search
- Fully free and open-source
- Claims: **2.83x faster than Elasticsearch** for full-text search
- **4–10x lower resource requirements** than Elasticsearch
- Source: [OpenObserve Blog](https://openobserve.ai/blog/elasticsearch-alternatives/)

### 5.6 Grafana Loki
- Free and open-source
- Grafana Cloud free tier: 50 GB logs, traces, and metrics included
- Architecture advantage: indexes metadata only (not full text) → significantly lower CPU, RAM, and storage per ingested log vs. Elasticsearch
- Source: [Better Stack](https://betterstack.com/community/comparisons/elasticsearch-alternative/)

### 5.7 Sonic (Search Backend)
- Free and open-source
- Resource requirement: **~30 MB of RAM**
- Source: [Better Stack](https://betterstack.com/community/comparisons/elasticsearch-alternative/)

### 5.8 ClickHouse Performance Claims
- Aggregation queries: **100–1,000x faster** than row-based systems
- Compression: **10–100x** compression ratios
- Source: [OpenObserve Blog](https://openobserve.ai/blog/elasticsearch-alternatives/)

---

## 6. OBSERVED SAVINGS RANGES BY MIGRATION TYPE

| Migration Path | Savings Range | Notes |
|---|---|---|
| ES self-managed → Hot-Warm-Cold (same platform) | 60–73% | Elastic Blog internal benchmark |
| ES Elastic Cloud → AWS OpenSearch Managed | 52–85% | Octus case study |
| ES → ClickHouse (analytics workloads) | 66–91% (11x) | ProcessOut, Contentsquare |
| ES → Apache Doris (observability at 100 TB/day) | ~86.5% | VeloDB modeling |
| ES → Quickwit (log workloads) | 50–80% compute+storage | Unnamed company |
| Splunk → Elasticsearch | 50–60% | Elastic case studies |
| ES upgrade (version 7.x optimization) | $1.2M/year | Elastic internal |
| Hot tier → Frozen/Glacier storage | 70–95% storage | Elastic Blog |
| On-demand → Reserved Instances (3-year) | 48–52% | AWS pricing |
| On-demand → Spot Instances | up to 90% | CloudKeeper |
| General-purpose → ARM/Graviton instances | 20–40% | AWS / GCP data |

---

## 7. MARKET CONTEXT DATA POINTS

- **Jan 27, 2025**: Elastic announced ~30% price increase for typical production workloads
- **2021**: Elastic changed license to SSPL + ELv2 (triggered OpenSearch fork by AWS)
- Enterprise Elastic mid-sized deployments: licensing starts in **high five figures/year**
- Large enterprise Elasticsearch environments: often **>$500,000/year** total cost
- 3-year ELK stack TCO for modest deployment: **~$2,000,000**
- In-house Elasticsearch admin: effectively **~$140K/year in FTE cost**
- Major cryptocurrency exchange Datadog bill: **~$65M/year** (2022, context for observability cost scale)
- Optimization strategies (architectural, not migration): can cut Elasticsearch bills **30–60%**
- Migrations from ES to alternatives: **60–90% infrastructure cost reductions** common
- Enterprise search TCO is typically **2–6x higher** than license pricing alone

---

## 8. SOURCES

- [Elastic Blog - Saved $100K/month by upgrading](https://www.elastic.co/blog/elastic-observability-clusters-upgrade-latest-release-save-money)
- [AWS Big Data Blog - Octus 85% cost reduction](https://aws.amazon.com/blogs/big-data/how-octus-achieved-85-infrastructure-cost-reduction-with-zero-downtime-migration-to-amazon-opensearch-service/)
- [ClickHouse Blog - ProcessOut](https://clickhouse.com/blog/processout-switched-from-elasticsearch-to-clickhouse-cloud)
- [ClickHouse Blog - Contentsquare](https://clickhouse.com/blog/contentsquare-migration-from-elasticsearch-to-clickhouse)
- [ClickHouse Blog - Didi](https://clickhouse.com/blog/didi-migrates-from-elasticsearch-to-clickHouse-for-a-new-generation-log-storage-system)
- [VeloDB Blog - ES vs ClickHouse vs Apache Doris](https://www.velodb.io/blog/elasticsearch-clickhouse-apache-doris-powers-observability-better)
- [Elastic Blog - ROI migrating off OpenSearch](https://www.elastic.co/blog/return-on-investment-migrating-off-opensearch-search-logging)
- [Elastic Blog - Cost saving strategies (storage)](https://www.elastic.co/blog/cost-saving-strategies-for-the-elasticsearch-service-data-storage-efficiency)
- [Elastic Blog - Top 5 ways to optimize Elastic Cloud costs](https://www.elastic.co/blog/top-5-ways-to-optimize-your-elastic-cloud-costs)
- [Elastic Blog - S3 Glacier Deepfreeze](https://www.elastic.co/search-labs/blog/s3-glacier-archiving-elasticsearch-deepfreeze)
- [BigData Boutique - OpenSearch & Elasticsearch Pricing Guide](https://bigdataboutique.com/blog/opensearch-and-elasticsearch-pricing-guide)
- [BigData Boutique - ClickHouse vs Elasticsearch](https://bigdataboutique.com/blog/clickhouse-vs-elasticsearch)
- [Lucidity - AWS OpenSearch Pricing](https://www.lucidity.cloud/blog/aws-opensearch-pricing)
- [Quesma - Understanding Elasticsearch Pricing](https://quesma.com/blog/elastic-pricing/)
- [OpenObserve - Best Elasticsearch Alternatives 2026](https://openobserve.ai/blog/elasticsearch-alternatives/)
- [Meilisearch - Elasticsearch Pricing](https://www.meilisearch.com/blog/elasticsearch-pricing)
- [Better Stack - Elasticsearch Alternatives](https://betterstack.com/community/comparisons/elasticsearch-alternative/)
- [CloudKeeper - Saving AWS Elasticsearch Costs](https://www.cloudkeeper.com/insights/blog/saving-aws-elasticsearch-costs-search-service-optimization-strategies)
- [Quickwit - Benchmarking Quickwit vs Loki](https://quickwit.io/blog/benchmarking-quickwit-loki)
- [Elastic - Cut Costs Migrating from Splunk](https://www.elastic.co/splunk-replacement/migration-guide)
- [Tinybird - ClickHouse vs Elasticsearch](https://www.tinybird.co/blog/clickhouse-vs-elasticsearch-search)
- [ClickHouse - Breaking Free from Rising Observability Costs](https://clickhouse.com/blog/breaking-free-from-rising-observability-costs-with-open-cost-efficient-architectures)
