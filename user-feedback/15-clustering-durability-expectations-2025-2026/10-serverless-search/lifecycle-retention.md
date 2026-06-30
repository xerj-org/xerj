# Index Lifecycle Management, Retention & Data Tiering — User Expectations 2025–2026

Research compiled from surveys, vendor reports, engineering blogs, community forums, and industry analysis.

| # | Quote/Summary | Source | Date | Category |
|---|---------------|--------|------|----------|
| 1 | "ILM automates rollover (splitting hot indices), shrinking (optimizing warm data), freezing (making old data searchable but cold), and deletion (based on age, size, or conditions)" — core expectation that lifecycle is fully automated | Medium / Vaibhav Kamble | 2024 | ILM Automation |
| 2 | "Without rollover, a single index would continue to grow, causing search performance to drop and having a higher administrative burden on the cluster" — users expect rollover to handle growth invisibly | Elastic Docs | 2025 | Index Rollover |
| 3 | "Rollover is handled automatically for data streams in Elasticsearch Serverless projects" — serverless users expect zero-config rollover | Elastic Docs | 2025 | Serverless Lifecycle |
| 4 | "Over three-quarters of respondents keep data in hot storage for less than 90 days — and half keep it for just 30 days" — most teams use short hot retention due to cost pressure | Imply State of Log Management 2025 | 2025 | Hot Tier Retention |
| 5 | "Once it cools, 60% store data in warm tiers for only one to six months, and 58% archive or delete logs from cold storage within six months" — most data is deleted long before it provides trending or ML value | Imply State of Log Management 2025 | 2025 | Warm/Cold Retention |
| 6 | "Most teams lose access to the very data they need for trending, forensics, or machine learning before it can drive insight" — short retention is a structural problem | Imply State of Log Management 2025 | 2025 | Retention Gap |
| 7 | "74% of organizations are already using or piloting AI/ML-based detection, and another 24% plan to do so in the next year. These workloads demand more data, longer retention, and faster queries" — AI adoption is driving demand for extended retention | Imply State of Log Management 2025 | 2025 | AI/ML Retention Demand |
| 8 | "Saving 60% in monthly costs while still having searchable and resilient data" — hot-warm architecture cost savings are a key user motivation | Elastic Blog | 2024 | Cost Optimization |
| 9 | "Elasticsearch logsdb index mode reduces the storage footprint of log data by up to 65%" — users expect storage efficiency features to be built into the engine | Elastic Press Release | December 2024 | Storage Efficiency |
| 10 | "LogsDB cuts index size by up to 75% at no throughput cost" — further improvements exceed the original 65% claim | Elastic Observability Labs | 2025 | Storage Efficiency |
| 11 | "TCO reductions of up to 50% are expected" for customers who need long-term retention using logsdb + searchable snapshots | Elastic Press Release | December 2024 | Cost Optimization |
| 12 | "The frozen tier makes it cost effective to store years of data at a cost comparable to archiving data in S3 or object storage" — users expect archive-tier pricing for long data retention | Elastic Docs | 2025 | Frozen Tier |
| 13 | "Extends storage capacity by up to 20 times compared to the warm tier" — frozen tier is expected to provide dramatic storage multiplier | Elastic Docs | 2025 | Frozen Tier |
| 14 | "Data remains fully searchable in Elasticsearch with all Kibana dashboards working seamlessly, eliminating the need to manually find, restore, and make archived data available" — frozen tier users expect no operational overhead to access old data | Opster Guide | 2025 | Frozen Tier |
| 15 | "Fully mounted indices on the cold tier eliminate the need for replicas, reducing required disk space by approximately 50% compared to regular indices" — cold tier economics matter for adoption | Elastic Docs | 2025 | Cold Tier |
| 16 | "ILM is not available in Serverless because in that environment your cluster performance is optimized for you. Instead, data stream lifecycle is available as a data lifecycle option" — serverless users expect the system to handle lifecycle without them configuring ILM | Elastic Docs | 2025 | Serverless Lifecycle |
| 17 | "Data stream lifecycle is a simpler lifecycle management tool optimized for the most common lifecycle management needs, enabling you to configure retention duration for data without hardware-centric concepts like data tiers" — users want retention without hardware abstraction | Elastic Labs Blog | 2024 | Simplified Lifecycle |
| 18 | "Data older than the retention period is deleted automatically by Elasticsearch at a later time. Retention can be configured on the data stream level or on a global level" — expectation: simple global or per-stream retention setting | Elastic Docs | 2025 | Retention Automation |
| 19 | "Data stream lifecycles will automatically execute the data structure maintenance operations like rollover and force merge, and allow you to only deal with the business-related lifecycle functionality" — users expect all structural ops to be invisible | Elastic Labs Blog | 2024 | Maintenance Automation |
| 20 | "A new API was added in 8.19 / 9.1 for applying lifecycle directly at the data stream level" — lifecycle management is being simplified from policy-first to stream-first | Elastic Observability Labs | 2025 | Simplified Lifecycle |
| 21 | "Managing retention in Elasticsearch involves Data stream lifecycle, Index lifecycle management, templates, and individual index settings" — current complexity is a known problem; Streams attempts to unify it | Elastic Observability Labs | 2025 | UX Complexity |
| 22 | "Streams introduces a unified way to manage how long data lives, whether using DSL or ILM" — unified lifecycle view is an explicit design goal driven by user frustration | Elastic Observability Labs | 2025 | UX Simplification |
| 23 | "The index name must match the regex pattern ^.*-\d+ for the rollover action to work. The most common problem is that the index name does not contain trailing digits" — naming requirements cause frequent user errors | Elastic Docs | 2025 | ILM Pain Points |
| 24 | "ILM actions are run as though they are performed by the last user who modified the policy with the privileges that user had at that time" — ILM permission model is counterintuitive and creates subtle failures | Elastic Docs | 2025 | ILM Pain Points |
| 25 | "The error occurs because the index is assigned to an ILM policy that does not exist in the cluster" — missing policy errors are a common operational failure mode | Elastic Docs | 2025 | ILM Pain Points |
| 26 | "Because Elasticsearch can only perform certain clean up tasks on a green cluster, there might be unexpected side effects" — ILM behavior depends on cluster health in non-obvious ways | Elastic Docs | 2025 | ILM Pain Points |
| 27 | "Managing hundreds or thousands of lifecycle policies across different buckets, applications, and compliance requirements becomes incredibly complex, which can lead to policies conflicting, not being applied correctly, or simply being forgotten" | O11y Blog | 2025 | Policy Complexity |
| 28 | "Insufficient automation tools and processes contribute heavily to policy complexity and management overhead, and relying on manual processes or basic, inflexible lifecycle rules for dynamic data environments quickly becomes unmanageable" | O11y Blog | 2025 | Policy Complexity |
| 29 | "A four-tier storage implementation required more hardware, licenses, and overhead in operational management" — each additional tier adds operational burden | Opster Guide | 2025 | Operational Overhead |
| 30 | "Many organizations struggle to gain real-time insights into their data access patterns... which often leads to reactive rather than proactive optimization" — lack of observability into access patterns makes tiering decisions manual and error-prone | QodeQuay | 2024 | Access Pattern Visibility |
| 31 | "Optimized tiering goes beyond reactive, access-pattern-based automation. It uses proactive, policy-driven data governance to classify data by importance, retention requirements, and SLA expectations, then assigns each data type to the optimal tier from the moment of creation" — expectation that tiering is proactive, not reactive | Archon DataStore | 2025 | Smart Tiering |
| 32 | "Amazon S3 Intelligent-Tiering is the 'set it and forget it' approach to storage cost optimization" — users explicitly want automatic tiering that requires zero ongoing intervention | AWS | 2025 | Automatic Tiering |
| 33 | "A tiering policy automatically moves any chunks that only contain data older than the move_after threshold to the object storage tier... schedules a job that runs periodically to asynchronously migrate eligible chunks" — expectation: declare policy once, system handles movement | Timescale Docs | 2025 | Automatic Tiering |
| 34 | "With automated data tiering policies, you get a set it and forget it tool to cut storage costs" — automatic tiering framed as a selling point against manual approaches | Timescale | 2025 | Automatic Tiering |
| 35 | "Using a four-tiered storage system can lead to up to 98% cost savings compared to untiered storage" — cost savings justify complexity; users want the savings without the complexity | DataCore / Archon | 2025 | Cost Savings |
| 36 | "Automated storage tiering uses software to dynamically move data between different storage tiers based on access patterns and predefined policies" — expectation that tiering is policy-driven and access-aware | Aerospike | 2025 | Automatic Tiering |
| 37 | "Automated tiering technology which was once a relatively exotic feature of only the most cutting-edge disk arrays has become mainstream. Nearly all major vendors' storage systems now include automated storage tiering as a standard feature" — users now expect tiering to be standard, not premium | CIO / DataCore | 2025 | Market Baseline |
| 38 | "75% of companies say cost is an important criterion for observability solutions" — cost is the top evaluation criterion; retention/tiering decisions are cost-driven | Grafana Observability Survey 2025 | 2025 | Cost Priority |
| 39 | "74% of respondents say cost is a top priority for selecting tools. SaaS users are most likely to cite cost as their top concern" — managed/serverless users are most cost-sensitive | Grafana Observability Survey 2026 | 2026 | Cost Priority |
| 40 | "Top three observability concerns in 2026 are complexity/overhead (38%), signal-to-noise challenges (34%), and cost (31%)" — complexity overtakes cost as #1 concern | Grafana Observability Survey 2026 | 2026 | UX Complexity |
| 41 | "More than three-quarters (77%) say they have saved time or money through centralized observability" — centralization and automation are proven value-drivers | Grafana Observability Survey 2025 | 2025 | Centralization Value |
| 42 | "Only 41% of organizations are able to keep log data for longer than a few weeks" — the majority of teams are falling short of what compliance and AI/ML workloads require | Grafana Observability Survey 2025 | 2025 | Retention Gap |
| 43 | "51% of organizations expect their organization's data to grow by more than 75% in the next 12 months, including 19% that expect 100% or more growth" — retention costs will keep rising, making automation more critical | Grafana Observability Survey 2025 | 2025 | Data Growth |
| 44 | "19% of organizations are taking in more than a terabyte a day" — at this scale, manual lifecycle management is impossible | Grafana Observability Survey 2025 | 2025 | Data Volume |
| 45 | "Long-term log retention is something almost every engineering team struggles with, as logs pile up quietly in storage and become one of the fastest-growing cost centers in cloud bills" | Groundcover | 2025 | Cost Challenges |
| 46 | "One of the biggest offenders of tech spending is logging — while logs are a pillar of observability, too often they lead to massive storage bills and memory-constrained systems" | Work-Bench | 2025 | Cost Challenges |
| 47 | "Storing logs for three days cost $180,000/year at Uber, and extending retention to one month would have cost $1.8 million/year" — retention costs scale dramatically; automation and tiering are essential at scale | Observe Inc. | 2025 | Cost at Scale |
| 48 | "Customers of legacy observability tools can see significant annual increases in total cost of ownership, sometimes over 30%, while their budgets and headcounts are being slashed" | Last9 | 2025 | TCO Pressure |
| 49 | "Cold-storage solutions, truncated retention periods, and data filtering and sampling can help rein in growing costs, but not without trade-offs — overreliance on these methods can put critical data out of reach in moments of urgency" | Observe Inc. | 2025 | Retention Tradeoffs |
| 50 | "For a workload of 1TB of logs per day with one year of retention, a tiered log data lake costs approximately $15,240/year vs. $219,000/year with a traditional vendor — about 93% savings" | Grepr.ai | 2025 | S3 Tiering Savings |
| 51 | "Keeping 5 years of data with Datadog Flex Logs would cost approximately $1,095,000, while a log data lake approach would cost approximately $76,200" — users are increasingly aware of long-term retention cost multiples | Grepr.ai | 2025 | Long-term Retention Cost |
| 52 | "Log retention costs range from $0.10–$25 per GB annually, with mid-size companies typically spending $3,000–$20,000 yearly, while enterprises spend $500,000+" | EdgeDelta | 2025 | Cost Benchmarks |
| 53 | "Tiered storage approaches can cut costs by 60-75%" — tiering is the primary mechanism users expect to use for cost management | EdgeDelta | 2025 | Tiering Savings |
| 54 | "Security logs can be kept in hot storage for 90 days, then moved to warm storage for one year, then to cold storage" — 90-day hot / 1-year warm / archive is an emerging standard pattern | EdgeDelta | 2025 | Tiering Patterns |
| 55 | "PCI DSS 4.0 requires 12 months of audit log retention with 90 days immediately accessible for analysis (Requirement 10.5.1)" — compliance mandates specific hot-tier retention windows | LinfordCo / PCI DSS | 2025 | Compliance Requirements |
| 56 | "HIPAA Security Rule requires 6 years of retention from the date of creation" — healthcare workloads need long-tail retention that hot/warm tiers cannot economically serve | optro.ai | 2025 | Compliance Requirements |
| 57 | "If your organization processes credit cards and handles healthcare data, audit logs touching both domains require 6-year retention" — multi-framework compliance drives demand for flexible, long retention | optro.ai | 2025 | Compliance Requirements |
| 58 | "Splunk's ingestion-based pricing gets brutal at scale, with teams regularly reporting six-figure annual bills for log management alone" — Splunk pricing is the baseline pain point driving search for alternatives | Last9 | 2025 | Vendor Switching |
| 59 | "Companies have reduced logging costs by up to 60% with Elasticsearch/ELK as a Splunk alternative" — users expect migration to save substantially on retention costs | Sematext | 2025 | Vendor Switching |
| 60 | "Grafana Loki indexes labels, not full text, which makes it dramatically cheaper to run than Splunk" — users expect index-less or label-based storage for cost-efficient long retention | Atera | 2025 | Indexing Model |
| 61 | "An intelligent data retention recommendation system using Machine Learning can suggest optimal retention periods for indices dynamically, with models learning usage patterns to recommend when to tier or delete indices while minimizing cost" — ML-driven lifecycle management is an emerging expectation | IJCSE Research Paper | 2025 | AI-Driven Lifecycle |
| 62 | "Data logs requiring analysis at the enterprise level have grown as much as 250% year-over-year in the last 5 years" — data volume growth makes static lifecycle policies increasingly inadequate | IBM / ScienceLogic | 2025 | Data Growth |
| 63 | "In machine learning, usually the more data available, the more accurate the ML model will be. Systems could require weeks or months of data to serve accurate predictions" — AI workloads require much longer retention than operational monitoring | IBM | 2025 | AI/ML Retention Demand |
| 64 | "Data retention policies in the AI era are changing — AI-driven policies for archiving and pruning based on data value and compliance requirements are emerging" | Gimmal | 2025 | AI-Driven Lifecycle |
| 65 | "Optimal primary shard size should be between 30 GB and 50 GB" (OpenSearch) / "10–50 GB" (Elasticsearch) — users expect rollover thresholds to be configurable against recommended shard size targets | OpenSearch Docs / Elastic Docs | 2025 | Rollover Sizing |
| 66 | "By default, rollover is executed when the index reaches 30 days of age or when one or more primary shards reach 50 GB in size" — time-based and size-based rollover are both required by default | Elastic Docs | 2025 | Rollover Defaults |
| 67 | "Rollover action implicitly rolls over a data stream or alias if one or more shards contain 200,000,000 or more documents" — document-count limits are a hidden rollover trigger users must understand | Elastic Docs | 2025 | Rollover Limits |
| 68 | "For 2025, automated index tools are increasingly becoming part of cloud database services like AWS RDS, Azure SQL Database, and Google Cloud SQL, meaning they're built right into the databases" — expectation that index automation is a standard cloud feature | SQLFlash | 2025 | Cloud Automation |
| 69 | "ISM policies require users to get the current policy first to retrieve the sequence number and primary term before updating" — OpenSearch ISM update flow is complex and error-prone relative to expectations | OpenSearch Docs | 2025 | ILM Pain Points |
| 70 | "Wildcard operators are not supported except when used at the end, and multiple index names and patterns are not supported" for cold index ISM APIs — ISM API limitations force workarounds that add operational overhead | Amazon OpenSearch Docs | 2025 | ILM Pain Points |
| 71 | "CrateDB supports automatic or manual tiering, allowing administrators to define policies to move data automatically between tiers based on age, size, value or retention rules, or trigger movements manually when needed" — multi-vendor convergence on automatic tiering as standard | CrateDB | 2025 | Automatic Tiering |
| 72 | "TimescaleDB provides powerful built-in tools for automating data retention through policies that delete old data based on age thresholds" — expectation: retention policy as a first-class, single-command feature | Timescale | 2025 | Automatic Retention |
| 73 | "Diagnostic logs typically require 30–90 days of retention, and 90 days has become a common baseline for many log retention scenarios" — 90 days is the emerging hot-tier floor for operational logs | EdgeDelta | 2025 | Retention Standards |
| 74 | "Azure's Usage and AzureActivity tables keep data for at least 90 days at no charge" — cloud providers are making 90-day free retention an expectation baseline | Microsoft Azure | 2025 | Platform Standards |
| 75 | "Logsdb mode uses smart index sorting, synthetic source, and advanced compression (Zstd, delta encoding, run-length encoding)" — users expect compression and storage optimization to be automatic, not manual | Elastic Labs | 2025 | Storage Efficiency |

## Sources

- [Elasticsearch ILM Docs — Elastic](https://www.elastic.co/docs/manage-data/lifecycle/index-lifecycle-management)
- [Elasticsearch ILM: Automating Retention and Cost Optimization — Medium](https://medium.com/@vaibhavkamble154/elasticsearch-index-lifecycle-management-ilm-automating-retention-and-cost-optimization-3f86f1b2dc04)
- [Fix ILM Errors — Elastic Docs](https://www.elastic.co/docs/troubleshoot/elasticsearch/index-lifecycle-management-errors)
- [Data Stream Lifecycle — Elastic Docs](https://www.elastic.co/docs/manage-data/lifecycle/data-stream)
- [Data Lifecycle Simplified for Data Streams — Elasticsearch Labs](https://www.elastic.co/search-labs/blog/data-lifecycle-simplified-for-data-streams)
- [How Streams Simplifies Retention Management — Elastic Observability Labs](https://www.elastic.co/observability-labs/blog/simplifying-retention-management-with-streams)
- [Elasticsearch Logsdb Index Mode — Elastic Labs](https://www.elastic.co/search-labs/blog/elasticsearch-logsdb-index-mode)
- [Elastic Announces LogsDB (65% reduction) — Business Wire](https://www.businesswire.com/news/home/20241213828255/en/Elastic-Announces-Elasticsearch-Logsdb-Index-Mode-to-Reduce-Log-Data-Storage-Footprint-by-Up-to-65)
- [LogsDB cuts index size by up to 75% — Elastic Observability Labs](https://www.elastic.co/observability-labs/blog/elasticsearch-logsdb-storage-evolution)
- [Elasticsearch Data Tiers — Elastic Docs](https://www.elastic.co/docs/manage-data/lifecycle/data-tiers)
- [Hot-Warm-Cold with ILM — Elastic Blog](https://www.elastic.co/blog/implementing-hot-warm-cold-in-elasticsearch-with-index-lifecycle-management)
- [Frozen Tier / Searchable Snapshots — Elastic Blog](https://www.elastic.co/blog/introducing-elasticsearch-frozen-tier-searchbox-on-s3)
- [Elasticsearch Multi-Tier Architecture — Opster](https://opster.com/guides/elasticsearch/capacity-planning/elasticsearch-hot-warm-cold-frozen-architecture/)
- [Managing Data via Hot/Warm/Cold/Frozen Tiers (No Coding) — Elastic Blog](https://www.elastic.co/blog/managing-data-automation-through-hot-warm-cold-and-frozen-tiers-no-coding-needed)
- [State of Log Management 2025 — Imply](https://imply.io/blog/log-management-2025/)
- [Data Tiering in Imply and Apache Druid — Imply](https://imply.io/blog/an-overview-to-data-tiering-in-imply-and-apache-druid/)
- [Observability Survey Report 2025 — Grafana Labs](https://grafana.com/observability-survey/2025/)
- [Observability Survey Takeaways 2025 — Grafana Labs](https://grafana.com/blog/2025/03/25/observability-survey-takeaways/)
- [Managing Observability Costs 2025 — Grafana Labs](https://grafana.com/blog/2025/10/14/managing-observability-costs-at-scale-a-look-at-the-latest-cost-management-features-in-grafana-cloud/)
- [What is Log Retention? — EdgeDelta](https://edgedelta.com/company/knowledge-center/what-is-log-retention)
- [Log Retention Policies — Groundcover](https://www.groundcover.com/learn/logging/log-retention-policies)
- [Lost Logs: Retention vs Cost — Observe Inc.](https://www.observeinc.com/blog/lost-logs-retention-vs-cost)
- [Reduce Log Retention Costs by 93% — Grepr.ai](https://www.grepr.ai/blog/dirt-cheap-infinite-queryable-storage)
- [How to Reduce Log Data Costs — Last9](https://last9.io/blog/how-to-reduce-log-data-costs-without-losing-important-signals/)
- [Log Retention: Policies, Best Practices — Last9](https://last9.io/blog/log-retention/)
- [Optimizing Log Costs — Work-Bench](https://www.work-bench.com/post/optimizing-log-costs)
- [Security Log Retention Guide — optro.ai](https://optro.ai/blog/security-log-retention-best-practices-guide)
- [Log Retention Requirements for Compliance — observo.ai](https://observo.ai/post/log-retention-requirements-for-regulatory-compliance)
- [PCI DSS 4.0 Compliance Guide — Linford & Company](https://linfordco.com/blog/pci-dss-4-0-requirements-guide/)
- [Intelligent Data Retention with ML — IJCSE](https://www.internationaljournalssrg.org/IJCSE/paper-details?Id=611)
- [Data Retention Policies in the AI Era — Gimmal](https://gimmal.com/data-retention-policies-in-the-ai-era-whats-changing/)
- [What is Log Analysis with AI? — IBM](https://www.ibm.com/think/topics/ai-for-log-analysis)
- [Tiered Storage Guide — Aerospike](https://aerospike.com/blog/tiered-storage-guide/)
- [Auto Tiering — DataCore](https://www.datacore.com/products/sansymphony/auto-tiering/)
- [Amazon S3 Intelligent-Tiering — AWS](https://aws.amazon.com/s3/storage-classes/intelligent-tiering/)
- [Timescale Data Tiering — Timescale Docs](https://docs.timescale.com/use-timescale/latest/data-tiering/enabling-data-tiering/)
- [CrateDB Data Tiering — CrateDB](https://cratedb.com/storage/data-tiering)
- [CrateDB Hot/Cold Retention Policy — CrateDB Blog](https://cratedb.com/blog/building-a-hot-and-cold-storage-data-retention-policy-in-cratedb-with-apache-airflow)
- [Data Tiers: Value, Cost, Performance — o11y.ai](https://blog.o11yai.com/blog/data-tiers-value-cost-performance/)
- [OpenSearch ISM — OpenSearch Docs](https://opensearch.org/docs/latest/im-plugin/ism/index/)
- [ISM Amazon OpenSearch — AWS Docs](https://docs.aws.amazon.com/opensearch-service/latest/developerguide/ism.html)
- [Elasticsearch ILM vs OpenSearch ISM — Opster](https://opster.com/guides/opensearch/opensearch-data-architecture/elasticsearch-ilm-vs-opensearch-ism-policy/)
- [Shard Sizing Best Practices — Elastic Docs](https://www.elastic.co/guide/en/elasticsearch/reference/current/size-your-shards.html)
- [Index Rollover — Elastic Docs](https://www.elastic.co/docs/manage-data/lifecycle/index-lifecycle-management/rollover)
- [Automated Index Recommendations 2025 — SQLFlash](https://sqlflash.ai/article/20250624-2/)
- [Cost-Saving Strategies for ES Data Storage — Elastic Blog](https://www.elastic.co/blog/cost-saving-strategies-for-the-elasticsearch-service-data-storage-efficiency)
- [Splunk Alternatives 2025 — Last9](https://last9.io/blog/top-splunk-alternatives-for-2024-a-comprehensive-guide/)
- [Top Splunk Alternatives 2025 — ClickHouse](https://clickhouse.com/resources/engineering/top-splunk-alternatives-2025)
- [How to Implement Log Retention Policies in ES — OneUptime](https://oneuptime.com/blog/post/2026-01-21-elasticsearch-log-retention-ilm/view)
- [How to Configure Data Retention Policies in TimescaleDB — OneUptime](https://oneuptime.com/blog/post/2026-02-02-timescaledb-data-retention/view)
- [S3 Lifecycle Policies for Log Costs — OneUptime](https://oneuptime.com/blog/post/2026-02-12-reduce-s3-storage-costs-lifecycle-policies/view)
- [How We Reduced Log Storage Costs by 90% — Manoj Singh / Medium](https://manojsingh0302.medium.com/how-we-reduced-long-term-log-storage-costs-by-90-using-s3-lifecycle-policies-b40c3ff40964)
- [Azure Log Retention Configuration — Microsoft Learn](https://learn.microsoft.com/en-us/azure/azure-monitor/logs/data-retention-configure)
- [Manage Audit Log Retention — Microsoft Learn](https://learn.microsoft.com/en-us/purview/audit-log-retention-policies)
