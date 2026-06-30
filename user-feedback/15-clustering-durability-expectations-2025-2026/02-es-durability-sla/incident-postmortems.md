# Search Database Incident Post-Mortems: 2024–2025

Research compiled from public post-mortems, status pages, engineering blogs, and incident analyses.
Data points: 70+

---

## 1. AWS US-EAST-1 DynamoDB / DNS Cascading Failure (October 20, 2025)

**Source:** [InfoQ](https://www.infoq.com/news/2025/11/aws-dynamodb-outage-postmortem/) | [The Register](https://www.theregister.com/2025/10/23/amazon_outage_postmortem/) | [ThousandEyes](https://www.thousandeyes.com/blog/aws-outage-analysis-october-20-2025) | [DevTo](https://dev.to/usxcloud/the-aws-outage-of-october-20-2025-what-happened-who-was-affected-and-lessons-learned-5b35)

### Data Points

1. **Root cause:** Race condition in DynamoDB's internal DNS management microservice. Two independent components — a DNS Planner and a DNS Enactor — interacted in an untested combination, leaving an empty DNS record for DynamoDB's regional endpoint.

2. **Trigger:** No single code change. Two independent automated systems operated concurrently without locking, producing an empty DNS record during simultaneous updates.

3. **Initial outage scope:** DynamoDB in US-EAST-1 went offline completely. The 3-hour DynamoDB window then triggered cascading failures across every AWS service that depends on DynamoDB for metadata or auth.

4. **Total affected services:** 75+ AWS services impacted, including EC2, Lambda, S3, RDS, Redshift, IAM, STS. 140+ services reported degradation at peak.

5. **Total outage duration:** 15+ hours from initial failure (October 19 evening) to full recovery at 6:01 PM ET October 20.

6. **Time to root cause identification:** 37 minutes after incident onset.

7. **DNS fix completion:** 5:24 AM ET — engineers resolved the initial DNS problem. Services did not recover because cascading dependencies had already propagated.

8. **Cascading recovery problem:** Each fix uncovered a new hidden dependency. Restoring DynamoDB did not restore downstream services; each had to be manually stabilized in sequence.

9. **User impact scale:** Over 1,000 companies worldwide affected. Over 6 million user disruption reports.

10. **Financial impact estimate:** CyberCube estimated insurance losses up to $581 million from this single event.

11. **Downstream search services affected:** Elasticsearch clusters running on EC2 in US-EAST-1 went offline as their underlying compute was inaccessible. AWS OpenSearch Service in the region was similarly unavailable.

12. **Key lesson:** A race condition in a background automation system — not a code deployment — caused the largest AWS outage in years. Search clusters with no cross-region failover had zero recourse for 15 hours.

---

## 2. Cloudflare Global CDN Outage — ClickHouse Database Regression (November 18, 2025)

**Source:** [Cloudflare Blog](https://blog.cloudflare.com/18-november-2025-outage/) | [InfoQ](https://www.infoq.com/news/2025/11/cloudflare-global-outage-cause/) | [ThousandEyes](https://www.thousandeyes.com/blog/cloudflare-outage-analysis-november-18-2025)

### Data Points

13. **Root cause:** A routine permissions improvement to a ClickHouse database cluster changed query behavior. A metadata query previously returning a clean column list began returning duplicate rows from underlying `r0` database shards, more than doubling the row count in the response.

14. **Propagation mechanism:** The doubled query results fed into a "feature file" used by Cloudflare's Bot Management system. That file doubled in size and was automatically propagated to every machine on the global network within minutes.

15. **Onset:** 11:05 UTC — database permissions change applied. 11:20 UTC — network failures began. Propagation from a single database change to global network failure took 15 minutes.

16. **Symptoms:** Core CDN and security services returned HTTP 5xx errors. Turnstile authentication (login) failed globally. Workers KV experienced elevated error rates. Customer dashboards became inaccessible.

17. **Detection lag:** Engineers did not identify the database as root cause immediately. Initial investigation focused on network/routing layers.

18. **Root cause identified:** 13:37 UTC (2h 17m after onset).

19. **Remediation:** Engineers stopped generating new configuration files, manually deployed a known-good version of the feature file, forced proxy restarts across the network.

20. **Recovery:** 14:30 UTC — core traffic normalized. Total outage duration: ~3 hours 10 minutes.

21. **Key lesson:** A read-only database schema change (permissions only, no data modification) propagated into a full global outage in under 15 minutes. Configuration files derived from database queries are a critical path with no circuit breaker in this architecture.

22. **Relevance to search:** Elasticsearch clusters that generate configuration or index mappings via automated queries face similar risk: a query returning malformed results can be silently propagated into cluster state without validation.

---

## 3. Google Cloud Global Outage — Service Control Null Pointer (June 12, 2025)

**Source:** [iLert Post-Mortem](https://www.ilert.com/postmortems/google-cloud-outage-june-2025) | [Google Cloud Status](https://status.cloud.google.com/incidents/ow5i3PPK96RduMcb1SsW) | [ByteByteGo](https://blog.bytebytego.com/p/how-the-google-cloud-outage-crashed-the-internet)

### Data Points

23. **Root cause:** A new feature added to Google Cloud's Service Control system (quota policy enforcement) on May 29, 2025 introduced a null pointer in a code path that was never exercised during phased rollout. A policy update with blank fields triggered the untested path on June 12.

24. **Latent bug window:** 14 days between code deployment and trigger. The bug existed in production but was dormant until a specific policy configuration was applied.

25. **Trigger:** A policy update containing blank fields hit the null pointer, crashing Service Control binaries in every region simultaneously via crash loop.

26. **Blast radius:** 50+ distinct Google Cloud services across 40+ regions worldwide. BigQuery, Vertex AI, Cloud Functions, Cloud Storage, IAM, Compute Engine, Cloud Run all failed simultaneously.

27. **Onset:** 10:51 AM PDT. Within minutes, API requests across dozens of regions were failing with 503 errors.

28. **Time to SRE triage:** 2 minutes.

29. **Time to root cause:** 10 minutes.

30. **Time to remediation (red-button rollout):** 25 minutes to initiate rollback, 40 minutes to complete rollout disabling the offending policy path.

31. **Outage duration:** Approximately 3 hours total for most services.

32. **Relevance to search:** Google Cloud Vertex AI Search was among the services taken offline. Any Elasticsearch or search workloads hosted on GCP in the affected regions lost all API connectivity during the window.

33. **Key lesson:** A single null pointer in a quota-enforcement service took down 50+ services globally. No amount of Elasticsearch HA configuration protects against the underlying cloud control plane failing.

---

## 4. Matrix.org RAID Failure and Database Outage (September 2, 2025)

**Source:** [Matrix.org Post-Mortem](https://matrix.org/blog/2025/10/post-mortem/) | [The Register](https://www.theregister.com/2025/09/03/matrixorg_raid_failure/) | [Hacker News](https://news.ycombinator.com/item?id=45107696)

### Data Points

34. **Trigger:** Routine disk capacity expansion. At 11:03 UTC on September 2, Mythic Beasts added 2 NVMe drives to the primary and secondary database servers.

35. **Failure cascade:** During the maintenance, the primary database failed unexpectedly. Operators fell back to the secondary. When attempting to restore the primary, they lost the secondary-turned-primary as well — leaving no available database instance.

36. **Outage duration:** Approximately 24 hours. Homeserver unavailable from 17:45 UTC September 2 to 18:00 UTC September 3.

37. **Dataset size:** 51TB — the scale of the restore from S3 was the primary constraint on recovery time.

38. **Recovery complexity:** Required restoring from full backup, applying incremental backups, then replaying WAL logs to close the gap. WAL archiving to S3 had also started failing during the incident, requiring manual retrieval of locally-stored WAL segments.

39. **Data loss:** None. All data preserved via WAL + incremental backup chain.

40. **Key lesson:** Even with a working primary/secondary failover configuration, a maintenance operation can eliminate both nodes simultaneously. Recovery from backup at 51TB scale takes hours regardless of backup validity.

41. **Relevance to Elasticsearch:** ES clusters with 2-node configurations face an equivalent quorum collapse risk. A maintenance operation on node 1 that causes node 2 to take over, followed by a failure during node 1 restoration, leaves the cluster without a master. Recovery requires manual snapshot restore.

---

## 5. Matrix.org PostgreSQL Corruption (July 2025)

**Source:** [Matrix.org Blog](https://matrix.org/blog/2025/07/postgres-corruption-postmortem/)

### Data Points

42. **Incident type:** Database corruption discovered on the production homeserver, separate from the September RAID incident.

43. **Discovery method:** Internal consistency checks and error patterns in application logs revealed data corruption on the postgres instance.

44. **Key lesson:** Two separate database-layer incidents affected matrix.org within 3 months (July and September 2025), demonstrating that infrastructure incidents cluster — a system under stress surfaces multiple failure modes.

---

## 6. AdGuard VPN — Redis Cluster Outage via Kubernetes (December 2, 2025)

**Source:** [AdGuard Post-Mortem](https://adguard-vpn.com/en/blog/redis-kubernetes-vpn-incident-post-mortem.html)

### Data Points

45. **Root cause:** Redis cluster ran out of disk space. To increase storage, operators attempted to recreate the StatefulSet controller object without deleting running pods — a seemingly safe workaround to avoid downtime during a storage template change.

46. **Kubernetes behavior:** Kubernetes interpreted the controller recreation as a signal to restart the entire Redis cluster, not just update the controller metadata.

47. **Degraded recovery:** The cluster restart was not "cautious" — it moved through pod restarts too quickly without waiting for Redis to report healthy before proceeding to the next pod. Cluster came back partially degraded.

48. **Missing safeguard:** No startup probes configured. Kubernetes had no mechanism to verify "do not mark pod as ready until dataset is loaded" before shifting traffic.

49. **Impact:** VPN service degradation for AdGuard's user base during the incident window.

50. **Key lesson:** Kubernetes StatefulSet storage templates are effectively immutable after creation. Any disk expansion requires operators to work around Kubernetes abstractions — creating a gap where the orchestrator's behavior diverges from operator intent. The same applies to Elasticsearch StatefulSets in k8s deployments.

51. **Relevance to search:** Elasticsearch deployed on Kubernetes faces identical risk. Disk expansion on Elasticsearch PVCs often requires recreating StatefulSets, which can trigger unexpected pod restarts. Without careful orchestration and startup probes, nodes can rejoin clusters before they are ready.

---

## 7. Healthchecks.io — PostgreSQL Hardware Failure (April 30, 2025)

**Source:** [Healthchecks.io Blog](https://blog.healthchecks.io/2025/05/post-mortem-database-outage-on-april-30-2025/)

### Data Points

52. **Root cause:** Hardware instability on database server. PostgreSQL crashed with a segfault; shortly after, the entire server stopped responding to pings.

53. **Outage duration:** Approximately 30 minutes (15:46 UTC onset, recovery after hardware reset).

54. **Decision point:** Operators chose hardware reset over immediate promotion of the standby. This was a judgment call — promotion would have been faster but required verifying standby health first.

55. **Recovery action:** Hardware reset of primary server, followed by full ECC memory test (stressapptest, 2 hours), OS reinstall, PostgreSQL reinstall, replication restart.

56. **Time to new hardware migration:** ~1 day of operation and monitoring after reinstall, then failover to new hardware to reduce recurrence risk.

57. **Key lesson:** Hardware-level segfaults are indistinguishable from software crashes until post-incident analysis. A standby that is "ready" on paper may not be the fastest path to recovery if operators need to verify its state first.

---

## 8. AWS Bahrain (me-south-1) Physical Infrastructure Failure (March 2026, Ongoing)

**Source:** [Elastic Cloud Status](https://status.elastic.co/) | [Zilliz Blog](https://zilliz.com/blog/the-aws-outage-was-a-wake-up-call-for-vector-database-cross-region-disaster-recovery)

### Data Points

58. **Event:** Physical damage to AWS Bahrain (me-south-1) data center infrastructure. Two of three availability zones in the Bahrain facility were knocked out due to physical damage.

59. **Elastic Cloud impact:** AWS Bahrain was removed from the available region selection on cloud.elastic.co. All existing Elasticsearch deployments in me-south-1 are inaccessible with no ETA for recovery.

60. **Status cadence:** Elastic provides weekly updates only, given the open-ended nature of physical infrastructure recovery.

61. **Duration as of research date (April 2026):** 4+ weeks with no resolution timeline.

62. **Companion incident:** Separate AWS UAE region (ME-CENTRAL-1) also experienced data center damage, knocking out two of three AZs simultaneously.

63. **Zilliz response:** Zilliz Cloud launched native cross-region disaster recovery (March 27, 2026) specifically citing this event as the trigger, enabling automated failover with sub-60-second RTO and near-zero RPO.

64. **Key lesson:** Physical data center failures are not recoverable through software. Elasticsearch deployments with no cross-region snapshot replication lost access to data indefinitely. No SLA applies when the underlying infrastructure is physically destroyed.

---

## 9. Elastic Cloud — Aggregated Incident Statistics (2024–2025)

**Source:** [StatusGator](https://statusgator.com/services/elastic-cloud) | [IsDown](https://isdown.app/status/elastic) | [IncidentHub](https://incidenthub.cloud/status/elasticcloud)

### Data Points

65. **6-year total:** 957+ outages affecting Elastic Cloud users across all services (2019–2025).

66. **2-year total:** 237+ outages affecting Elastic Elasticsearch Availability specifically.

67. **Recent 90-day window:** 14 incidents — 13 major outages and 1 minor incident.

68. **Median outage duration (recent 90 days):** 6 hours 23 minutes per incident.

69. **Notable 2025 incident:** Kibana Security Solutions Page degradation lasting 5 hours 35 minutes beginning February 7, 2025.

70. **Notable 2026 incident:** Kibana connection issue via Cloud UI SSO (April 9, 2026) — affecting all hosted Cloud deployments, resolved via fix rollout.

71. **Notable 2026 incident:** Privatelink hostnames reported by API being incorrect following an implementation change — causing connectivity failures for customers relying on programmatic deployment URL resolution.

72. **Implied SLA gap:** 13 major outages in 90 days = roughly 1 major outage per week on Elastic Cloud. At median 6h23m per incident, that is approximately 83 hours of major outage exposure in a 90-day period across the service.

---

## 10. OpenSearch — 2-Node Cluster Quorum Loss (Structural, AWS-documented)

**Source:** [DEV Community](https://dev.to/aws-builders/avoid-this-costly-aws-opensearch-mistake-the-complete-guide-to-quorum-loss-77j) | [AWS Docs](https://docs.aws.amazon.com/opensearch-service/latest/developerguide/managedomains-dedicatedmasternodes.html)

### Data Points

73. **Pattern:** 2-node OpenSearch clusters (commonly deployed to reduce cost) cannot achieve quorum if either node fails. With 2 dedicated master nodes, the cluster cannot elect a new master when one node is lost.

74. **Recovery path:** Unlike most Elasticsearch errors, quorum loss in AWS OpenSearch cannot be self-remediated. Only AWS Support can perform the backend intervention required.

75. **Recovery time:** AWS Support intervention for OpenSearch quorum loss typically takes 24–72 hours of complete downtime.

76. **Frequency:** This is a recurring pattern documented as one of the most common costly AWS OpenSearch mistakes. It is not an edge case — it is the default behavior of any 2-node cluster experiencing a single-node failure.

77. **OpenSearch bug (GitHub #10790):** After quorum loss when cluster manager nodes are replaced using Remote Store, indices fail to recover and throw `TranslogCorruptedException` — a separate bug compounding the quorum loss recovery.

78. **Key lesson:** The minimum safe configuration for a search cluster that can self-heal is 3 master-eligible nodes. 2-node clusters provide the appearance of high availability while guaranteeing a complete outage on any single node failure.

---

## 11. Elasticsearch — Production OOM / Memory Crash Patterns (Documented Cases)

**Source:** [Plaid Engineering Blog](https://plaid.com/blog/how-we-stopped-memory-intensive-queries-from-crashing-elasticsearch/) | [Opster](https://opster.com/blogs/elasticsearch-downtime-stories-and-what-you-can-learn-from-them/) | [HackerNoon](https://medium.com/hackernoon/how-we-stopped-memory-intensive-queries-from-crashing-amazon-elasticsearch-2b6303a4c6bd)

### Data Points

79. **Plaid (2019, multi-week incident):** Aggregation queries generating millions of unique bucket counters exhausted JVM heap, crashing all data nodes. Cluster crashed multiple times per week for 2+ weeks before root cause was identified.

80. **Fix applied:** Set `indices.breaker.request.limit` to 40% and `search.max_buckets` to 10,000. Circuit breakers were not configured by default in the version deployed.

81. **Instacart (early 2018):** 60,000+ timeouts/day with site outages during peak traffic. Root cause: a handful of poorly coded queries using unoptimized filters and heavy nested aggregations. Post-fix: reduced to ~2,000 timeouts/day.

82. **Cybersecurity company (documented by Opster):** Cluster stopped indexing entirely — indexing volume dropped to near-zero, latency spiked to several seconds per document. Root cause: a misconfigured analysis pipeline on a field with many slashes caused each document to take excessive time to analyze. Resolution: disable field analysis.

83. **emc2net:** Post-upgrade (v5.6.16 → v6.7.1) performance degradation caused the new cluster to consume 5× more resources than the previous version, effectively causing a capacity-based denial of service.

84. **JVM heap sizing risk:** Elasticsearch recommends setting heap to ~50% of available RAM with a hard cap at ~32GB (above which compressed oops are disabled, worsening GC performance). Misconfigured containers or nodes with default heap settings are routinely undersized for production workloads.

85. **GC pause cascades:** JVM GC pauses exceeding 30–45 seconds on mixed-role nodes (data + master) have been observed causing cluster state publication failures, triggering master re-elections and potentially split-brain conditions.

86. **Circuit breaker defaults:** Parent circuit breaker defaults to 95% heap utilization before rejecting requests. By this point, GC is already under severe pressure. Recommended intervention threshold is 85%.

---

## 12. Elasticsearch Split-Brain — Structural Risk (Production Documented Cases)

**Source:** [Opster](https://opster.com/guides/elasticsearch/best-practices/elasticsearch-split-brain/) | [Netdata](https://www.netdata.cloud/academy/elasticsearch-yellow-cluster-access/) | [BigData Boutique](https://bigdataboutique.com/blog/avoiding-the-elasticsearch-split-brain-problem-and-how-to-recover-f6451c)

### Data Points

87. **Split-brain definition:** Two (or more) groups of Elasticsearch nodes each elect themselves as master, forming independent clusters. Both halves accept writes, leading to divergent data that cannot be automatically merged.

88. **Trigger scenario (documented):** Heavy indexing on mixed-role nodes causes GC pauses of 45+ seconds, failing cluster state publication. The remaining nodes, not hearing from the current master, elect a new one. When the original master GC-recovers, two masters exist simultaneously.

89. **Pre-v7 risk:** Before Elasticsearch 7.0, split-brain was a common production failure mode. The fix (`discovery.zen.minimum_master_nodes` = N/2 + 1) was not enforced by default in older versions and was frequently misconfigured.

90. **Post-v7 improvement:** Elasticsearch 7.0 introduced a new cluster coordination algorithm that eliminates the classic split-brain problem for properly configured clusters. However, clusters still running pre-7 versions (common in enterprise environments) remain exposed.

91. **Data loss scenario:** In a split-brain event where both halves accept writes before detection, data written to the "losing" half is lost when the cluster is forced to reconcile. There is no automatic merge — operators must choose which half survives.

92. **Recovery process:** Manual intervention required. Operators must identify which half has authoritative data, shut down the other half, and restore cluster state. In large clusters with significant write volume during the split, this can mean hours of downtime and potential data loss analysis.

---

## 13. Elasticsearch Shard Allocation Failure — Production Recovery (2026)

**Source:** [OneUptime Blog](https://oneuptime.com/blog/post/2026-01-21-elasticsearch-shard-allocation-failures/view)

### Data Points

93. **Pattern:** Unassigned shards cause cluster to enter yellow or red status, degrading or halting search and indexing operations.

94. **Common causes:** Node disk watermark exceeded (default: 85% — cluster stops allocating shards; 90% — cluster moves shards away; 95% — cluster blocks all indexing), node failures removing shard copies without enough replicas to maintain availability, index lifecycle policy misconfigurations leaving shards in an unrecoverable state.

95. **Indexing block impact:** When disk watermark reaches 95%, Elasticsearch sets `index.blocks.read_only_allow_delete = true` on all indices, making all indices read-only. Writes fail silently or with errors depending on client configuration.

96. **Key lesson:** Disk watermark thresholds are poorly understood by operators until they are hit in production. At 85% disk full, no immediate visible symptom exists — the cluster quietly stops allocating new shards. At 95%, all indexing stops globally across the cluster.

---

## 14. Elasticsearch Misconfiguration Data Exposure (December 2024)

**Source:** [Hackread](https://hackread.com/elasticsearch-leak-6-billion-record-scraping-breaches/) | [SOCRadar](https://socradar.io/blog/elasticsearch-instances-43m-records-data/)

### Data Points

97. **Incident type:** Publicly exposed Elasticsearch instances without authentication. While not an availability incident, data exposure incidents create SLA and compliance failures equivalent in business impact.

98. **Scale of December 2024 exposure:** A misconfigured Elasticsearch server holding 1.12TB of data exposed 6+ billion records without authentication. Operated from a Russian-speaking entity. Contained scraped data from past and recent breaches.

99. **Secondary exposure (separate incident):** Researchers found 43M+ records across three exposed public Elasticsearch instances, including 5M+ valid credentials, thousands of credit cards, and large-scale PII.

100. **December 2024 breach-adjacent incident:** Researchers found another misconfigured server containing stolen data associated with the ShinyHunters threat actor group.

101. **Root cause pattern:** Elasticsearch's default configuration (pre-8.x) does not enable authentication or network binding restrictions. Clusters deployed without explicit security configuration are accessible to anyone who can reach the network endpoint.

102. **Key lesson:** Elasticsearch security is opt-in through version 7.x. Any cluster deployed without explicit TLS + authentication configuration is a data exposure incident waiting to happen. This is the most common Elasticsearch production failure mode by frequency.

---

## 15. Azure Database for PostgreSQL — Degradation (November 2025)

**Source:** [Can Artuc / Database Disasters 2024–2025](https://www.canartuc.com/database-disasters-2024-2025-eight-production-failures-and-how-to-survive-them/)

### Data Points

103. **Incident:** Azure Database for PostgreSQL experienced multiple warning periods in early November 2025.
- November 5: 3 hours 56 minutes of service warnings
- November 6: Additional 2 hours 43 minutes of warnings

104. **Autovacuum bloat (general pattern, 2024–2025):** Countless PostgreSQL instances suffered from table bloat due to inadequate autovacuum configuration. Bloated tables increase physical disk usage, slow all reads, and degrade query performance progressively — often discovered only when a full table scan becomes unbearably slow in production.

105. **PostgreSQL December 2024 security update:** Critical patches fixing a buffer over-read vulnerability in GB18030 encoding validation across PostgreSQL versions 13–17, plus 60+ bug fixes affecting query planning, parallel execution, index operations, and replication. Delayed patching creates both security exposure and functional risk.

---

## 16. Uptime Institute — Annual Outage Analysis 2025 (Industry Statistics)

**Source:** [Uptime Institute](https://uptimeinstitute.com/resources/research-and-reports/annual-outage-analysis-2025)

### Data Points

106. **Overall trend:** For the fourth consecutive year, overall outage frequency and reported severity declined industry-wide.

107. **Severity floor:** Only 9% of reported incidents in 2024 were classified as "serious or severe" — the lowest level Uptime has recorded.

108. **Human error increase:** The proportion of human error-related outages caused by failure to follow procedures rose by 10 percentage points in 2025 vs. 2024. As automation reduces simple failures, procedure-following errors become the dominant human factor.

109. **Financial sector improvement:** Third consecutive year of declining outage frequency in financial services — attributed to stricter regulation and heightened oversight following several major public incidents pre-2021.

110. **Cyber security rising:** Cyber security incidents are increasing and often have severe, lasting impacts — contrasting the general trend of declining outage frequency.

---

## 17. ThousandEyes — 2025 Internet Outage Pattern Analysis

**Source:** [ThousandEyes Blog](https://www.thousandeyes.com/blog/internet-report-outage-patterns-in-2025) | [Network World](https://www.networkworld.com/article/4124642/top-11-network-outages-and-application-failures-of-2025.html)

### Data Points

111. **Subtle failures increasing:** Analysis of H1 2025 shows a growing pattern of subtle functional failures and service degradations where symptoms appear disconnected from root causes. Systems appear healthy (metrics normal) while certain functions silently break.

112. **Cascading failure architecture risk:** Systems architected to work together accidentally spread failures across service boundaries.

113. **US-centric concentration:** U.S.-centric outages peaked at 55% of global incidents in late January 2025, declining to ~39% by end of June 2025.

114. **October 2025 spike:** 701 global network outage incidents in October 2025, dropping 40% to 153 in November 2025.

115. **AWS October cascade:** The AWS US-EAST-1 outage alone impacted Slack, Atlassian, Snapchat, and hundreds of other services — demonstrating how a single cloud control-plane failure amplifies to affect all tenants simultaneously.

---

## 18. Zilliz / Vector Database Industry — Cross-Region DR Response (2025–2026)

**Source:** [Zilliz Blog](https://zilliz.com/blog/the-aws-outage-was-a-wake-up-call-for-vector-database-cross-region-disaster-recovery) | [PR Newswire](https://www.prnewswire.com/news-releases/zilliz-cloud-launches-native-cross-region-disaster-recovery-for-vector-databases-302726773.html)

### Data Points

116. **Industry acknowledgment:** The vector database industry explicitly cited the AWS Bahrain physical damage incident and the Azure Central US 14.5-hour outage as catalysts for mandatory cross-region DR capabilities.

117. **Azure Central US (referenced):** A faulty configuration change took down Azure's Central US region for 14.5 hours. Elasticsearch and OpenSearch clusters hosted in that region were completely unavailable for the full window.

118. **Google Cloud Run/GKE/Firebase simultaneous failure (referenced):** A bug in Google Cloud took down Cloud Run, Google Kubernetes Engine, and Firebase simultaneously for 8 hours — any containerized Elasticsearch deployment on GKE in the affected region was unavailable.

119. **CrowdStrike cascade (2024 reference):** A flawed CrowdStrike software update cascaded through Azure-hosted infrastructure, costing Fortune 500 companies an estimated $5.4 billion. Elasticsearch clusters on those Azure hosts experienced unplanned reboots and potential data integrity issues depending on flush state at time of crash.

120. **Zilliz target SLA:** Sub-60-second RTO with near-zero RPO on cross-region failover — establishing this as the emerging benchmark expectation for production vector/search databases.

---

## Summary Statistics

| Category | Value |
|----------|-------|
| Total data points collected | 120 |
| Named incidents analyzed | 18 |
| Incidents with confirmed data loss | 1 (ES misconfiguration exposures) |
| Incidents with zero data loss but significant downtime | 8+ |
| Shortest recovery time documented | 30 minutes (Healthchecks.io hardware reset) |
| Longest recovery time documented | 4+ weeks ongoing (AWS Bahrain physical damage) |
| Median Elastic Cloud major outage duration (Q1 2026) | 6 hours 23 minutes |
| OpenSearch 2-node quorum recovery (AWS Support) | 24–72 hours |
| AWS DynamoDB cascade total duration | 15+ hours |
| Google Cloud Service Control outage | ~3 hours |
| Cloudflare ClickHouse cascade | ~3 hours 10 minutes |
| Matrix.org RAID cascade | ~24 hours |
| Major Elastic Cloud incidents in 90 days (Q1 2026) | 13 |
| Estimated total major outage hours (90-day window) | ~83 hours |

---

## Cross-Incident Lessons for Search Database Durability

1. **Cloud control-plane failures bypass all cluster HA.** AWS, GCP, and Azure have each experienced multi-hour regional failures in 2024–2025. Elasticsearch replica counts and shard redundancy provide zero protection when the underlying compute is inaccessible.

2. **Physical infrastructure failures have no SLA.** The AWS Bahrain incident is approaching 6 weeks with no recovery timeline. No Elasticsearch HA configuration survives physical data center destruction.

3. **2-node clusters guarantee outage on first node failure.** Documented as causing 24–72-hour recovery windows requiring vendor support intervention. This is not a theoretical risk.

4. **Maintenance operations are the most common trigger for cascading failures.** Matrix.org (disk expansion), AdGuard VPN (storage resize), Cloudflare (permissions change), Healthchecks.io (hardware failure during stable operation) — all occurred during or immediately after routine maintenance.

5. **Recovery time from backup is dominated by dataset size, not process quality.** Matrix.org's 51TB restore took ~24 hours regardless of backup integrity. Organizations must plan RTO around data volume, not backup existence.

6. **Configuration-derived failures propagate faster than operators can respond.** Cloudflare's database permissions change propagated to a global outage in 15 minutes. Elasticsearch configuration changes pushed via automation face identical risk.

7. **Silent failures are increasing.** ThousandEyes' 2025 analysis documents a growing pattern of systems appearing healthy while silently failing — applicable to Elasticsearch yellow status (partial shard availability) being treated as operational.

8. **Cross-region DR is becoming table stakes.** Every major vector and search database provider launched or significantly enhanced cross-region capabilities in 2025–2026, explicitly citing real production incidents as justification.

---

## Sources

- [Elastic Cloud Incident History](https://status.elastic.co/history)
- [StatusGator — Elastic Cloud Elasticsearch Availability](https://statusgator.com/services/elastic-cloud/elasticsearch-availability)
- [InfoQ — AWS DynamoDB Outage Postmortem](https://www.infoq.com/news/2025/11/aws-dynamodb-outage-postmortem/)
- [The Register — Amazon Outage Postmortem](https://www.theregister.com/2025/10/23/amazon_outage_postmortem/)
- [ThousandEyes — AWS Outage Analysis October 2025](https://www.thousandeyes.com/blog/aws-outage-analysis-october-20-2025)
- [Cloudflare Blog — November 18 2025 Outage](https://blog.cloudflare.com/18-november-2025-outage/)
- [InfoQ — Cloudflare Global Outage Cause](https://www.infoq.com/news/2025/11/cloudflare-global-outage-cause/)
- [iLert — Google Cloud Outage June 2025](https://www.ilert.com/postmortems/google-cloud-outage-june-2025)
- [Google Cloud Status — June 2025 Incident](https://status.cloud.google.com/incidents/ow5i3PPK96RduMcb1SsW)
- [Matrix.org — September 2 Outage Post-Mortem](https://matrix.org/blog/2025/10/post-mortem/)
- [Matrix.org — Postgres Corruption Post-Mortem](https://matrix.org/blog/2025/07/postgres-corruption-postmortem/)
- [The Register — Matrix.org RAID Failure](https://www.theregister.com/2025/09/03/matrixorg_raid_failure/)
- [AdGuard VPN — December 2025 Post-Mortem](https://adguard-vpn.com/en/blog/redis-kubernetes-vpn-incident-post-mortem.html)
- [Healthchecks.io — April 30 2025 Database Outage Post-Mortem](https://blog.healthchecks.io/2025/05/post-mortem-database-outage-on-april-30-2025/)
- [Zilliz Blog — AWS Outage Wake-Up Call for Vector Databases](https://zilliz.com/blog/the-aws-outage-was-a-wake-up-call-for-vector-database-cross-region-disaster-recovery)
- [Zilliz — Cross-Region DR Launch Announcement](https://www.prnewswire.com/news-releases/zilliz-cloud-launches-native-cross-region-disaster-recovery-for-vector-databases-302726773.html)
- [DEV Community — OpenSearch 2-Node Quorum Loss](https://dev.to/aws-builders/avoid-this-costly-aws-opensearch-mistake-the-complete-guide-to-quorum-loss-77j)
- [Opster — Elasticsearch Downtime Stories](https://opster.com/blogs/elasticsearch-downtime-stories-and-what-you-can-learn-from-them/)
- [Plaid Engineering — Stopping Memory-Intensive ES Queries](https://plaid.com/blog/how-we-stopped-memory-intensive-queries-from-crashing-elasticsearch/)
- [OneUptime — Elasticsearch Shard Allocation Failures](https://oneuptime.com/blog/post/2026-01-21-elasticsearch-shard-allocation-failures/view)
- [Hackread — Elasticsearch 6 Billion Record Leak](https://hackread.com/elasticsearch-leak-6-billion-record-scraping-breaches/)
- [SOCRadar — Elasticsearch 43M Record Exposure](https://socradar.io/blog/elasticsearch-instances-43m-records-data/)
- [Can Artuc — Database Disasters 2024–2025](https://www.canartuc.com/database-disasters-2024-2025-eight-production-failures-and-how-to-survive-them/)
- [Uptime Institute — Annual Outage Analysis 2025](https://uptimeinstitute.com/resources/research-and-reports/annual-outage-analysis-2025)
- [ThousandEyes — Three Outage Patterns 2025](https://www.thousandeyes.com/blog/internet-report-outage-patterns-in-2025)
- [ThousandEyes — Top Internet Outages of 2025](https://www.thousandeyes.com/blog/the-top-internet-outages-of-2025-analyses-and-takeaways)
- [Network World — 11 Biggest Network Outages 2025](https://www.networkworld.com/article/4124642/top-11-network-outages-and-application-failures-of-2025.html)
- [Opster — Elasticsearch Split-Brain](https://opster.com/guides/elasticsearch/best-practices/elasticsearch-split-brain/)
- [Netdata — Elasticsearch Yellow Cluster / Split-Brain](https://www.netdata.cloud/academy/elasticsearch-yellow-cluster-access/)
- [Medium — Google Cloud June 2025 Outage](https://dhanushnehru.medium.com/a-single-line-of-code-crashed-the-internet-google-cloud-outage-of-june-2025-86b58a9d67e8)
- [Medium — AWS DynamoDB October 2025](https://navaneethsen.medium.com/the-aws-dynamodb-outage-of-october-2025-a-story-of-cascading-failures-42f4b23b6379)
- [TechScoop — 15 Hours AWS Outage](https://techscoop.substack.com/p/how-15-hours-of-aws-outage-exposed)
