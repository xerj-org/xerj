# ES Clustering Failures — Data Points

## Total: 75 data points

Sources searched:
- discuss.elastic.co forum threads
- github.com/elastic/elasticsearch issues
- Opster, BigData Boutique, Sematext, SOC Prime, Tiger Data, Coralogix blogs
- Hacker News (news.ycombinator.com)
- Reddit, AWS re:Post
- OneUptime, Netdata, Datadog engineering blogs
- Elastic Cloud status history (statusgator.com / status.elastic.co)
- Elastic official docs and release notes

| # | Quote/Summary | Source | Date | Severity |
|---|--------------|--------|------|----------|
| 1 | "massive cluster instability for the last week or so... master node would lose connectivity with a data node. EVERY node would stop responding to REST API calls... which also means no searching, plugins won't work. This is extremely frustrating, and has been impacting production for days now!" | https://discuss.elastic.co/t/elasticsearch-cluster-instability/2015 | Ongoing (referenced 2024) | CRITICAL |
| 2 | "basic searches are taking ~30 seconds to complete" and "/\_nodes, /\_cat, /\_search... sometimes take FOREVER to return... (30 seconds to 90 seconds)" — same cluster with 15,000 shards for 2TB | https://discuss.elastic.co/t/elasticsearch-cluster-instability/2015 | Ongoing | HIGH |
| 3 | ~15,000 shards for 2TB of data described as "extremely excessive" by community responder; resolved only after reducing shard count | https://discuss.elastic.co/t/elasticsearch-cluster-instability/2015 | Ongoing | HIGH |
| 4 | Rolling restart of 100+ Elasticsearch servers required "3–4 hours" of manual work and was "very likely that the health in the cluster would go into the red at any moment and cause data and service loss" | https://medium.com/softtechas/on-the-fly-restarting-elasticsearch-cluster-without-red-status-69605fb99b0e | 2024 | HIGH |
| 5 | A rolling restart of a cluster with 8TB total data, 500 million documents took 38 hours to complete | https://opster.com/guides/elasticsearch/operations/how-to-perform-rolling-restarts-using-the-api-in-elasticsearch/ | 2024 | HIGH |
| 6 | Two-node cluster attempted rolling upgrade from ES 8.8 to 8.15; cluster returned 503 "Cluster state has not been recovered yet" and could not authenticate. Required manual intervention to recover | https://discuss.elastic.co/t/elasticsearch-rolling-upgrade-failed/364907 | 2024-08 | HIGH |
| 7 | "A two node cluster is not resilient, so it does not support rolling upgrade" — fundamental architectural constraint that surprises users | https://discuss.elastic.co/t/elasticsearch-rolling-upgrade-failed/364907 | 2024-08 | HIGH |
| 8 | Stuck "Cancelled Tasks" in ES 8.6.2 caused "huge unresolved search transport queues... queues grow indefinitely until thread pools start getting rejected" — cluster required weekly node restarts to stay stable | https://discuss.elastic.co/t/stuck-cancelled-tasks-in-elasticsearch-8-6-2-causing-cluster-failure/337490 | 2023-2024 | CRITICAL |
| 9 | "hundreds and hundreds of these tasks that pile up before the cluster starts falling over" — cancelled tasks lingered for 900+ seconds causing cluster-wide search failures | https://discuss.elastic.co/t/stuck-cancelled-tasks-in-elasticsearch-8-6-2-causing-cluster-failure/337490 | 2023-2024 | CRITICAL |
| 10 | Large cluster state with 25,000 indices caused transport timeouts: "failed to get local cluster state for...disconnecting" and "waiting for all nodes to process published state" timing out within 30-second window | https://opster.com/guides/elasticsearch/capacity-planning/elasticsearch-large-cluster-state-post-mortem/ | 2024 | CRITICAL |
| 11 | "Cluster state needs to be synced between all nodes in a cluster. Due to this, having large cluster states can cause time-outs and errors while syncing." | https://opster.com/guides/elasticsearch/capacity-planning/elasticsearch-large-cluster-state-post-mortem/ | 2024 | HIGH |
| 12 | Master nodes fail during startup when cluster state is oversized — lack sufficient heap to load metadata, potentially causing node disconnection and full service interruption | https://opster.com/guides/elasticsearch/capacity-planning/elasticsearch-large-cluster-state-post-mortem/ | 2024 | CRITICAL |
| 13 | GitLab production issue: their issues index was only ~7GB but used 120 shards (same as their 7TB main index) — "too many shards can reduce performance and waste threads" requiring emergency reindex | https://gitlab.com/gitlab-com/gl-infra/production/-/work_items/3849 | 2024 | MEDIUM |
| 14 | GitLab production: "searches were slow, searches queued with very little load and cluster utilization was quite low" — required thread pool increase to fix | https://gitlab.com/gitlab-com/gl-infra/production/-/work_items/3486 | 2024 | MEDIUM |
| 15 | Split brain with two master nodes: "the cluster will not work until a new master is elected, and to elect a new master you need at least 3 nodes" — cluster just stops answering requests | https://discuss.elastic.co/t/split-brain-problem-with-two-master-nodes/358710 | 2024-05 | HIGH |
| 16 | "It will not be healthy or functional, it will just stop answering to requests" when two master nodes cannot communicate — documented Elastic forum response | https://discuss.elastic.co/t/split-brain-problem-with-two-master-nodes/358710 | 2024-05 | HIGH |
| 17 | 30% of pre-v7 clusters in Opster analysis of 3,154 real-world clusters had misconfigured minimum_master_nodes, directly risking split-brain scenarios | https://opster.com/blogs/elasticsearch-best-practices-3000-cluster-analysis/ | 2024 | HIGH |
| 18 | 15% of 3,154 analyzed clusters experienced high circuit breaker trips causing request aborts; 11% had high CPU utilization with increased latencies and timeouts | https://opster.com/blogs/elasticsearch-best-practices-3000-cluster-analysis/ | 2024 | HIGH |
| 19 | 13.2% of clusters crossed disk watermark thresholds; 3.8% reached flood stage (all indices become read-only, blocking all writes) | https://opster.com/blogs/elasticsearch-best-practices-3000-cluster-analysis/ | 2024 | HIGH |
| 20 | 16% of analyzed clusters lacked sufficient dedicated coordinating nodes; 12.8% lacked sufficient dedicated master nodes — structural instability risk | https://opster.com/blogs/elasticsearch-best-practices-3000-cluster-analysis/ | 2024 | HIGH |
| 21 | 91.8% of analyzed clusters did not restrict wildcard operations for destructive commands — risk of accidental total data deletion at any time | https://opster.com/blogs/elasticsearch-best-practices-3000-cluster-analysis/ | 2024 | CRITICAL |
| 22 | "data-heavy tasks can overwhelm [the master] node and slow cluster-state updates. This not only raises latency but could also lead to 'split-brain' issues" — production lesson from running ES at scale | https://medium.com/@bregman.arie/lessons-learned-from-running-elasticsearch-in-production-d4fa382ff479 | 2024 | HIGH |
| 23 | "When shards become too large — hundreds of gigabytes or more — recoveries and rebalancing grow slower, keeping the cluster in a degraded state for longer" | https://medium.com/@bregman.arie/lessons-learned-from-running-elasticsearch-in-production-d4fa382ff479 | 2024 | HIGH |
| 24 | "if [heap] is too small, Elasticsearch can run out of memory and crash or get bogged down by constant garbage collection; if it's too large (above ~32 GB), you lose Java optimizations" — fundamental constraint | https://medium.com/@bregman.arie/lessons-learned-from-running-elasticsearch-in-production-d4fa382ff479 | 2024 | HIGH |
| 25 | JVM stop-the-world GC pauses: "if the pause lasts longer than 30 seconds, the cluster assumes the node is dead and starts moving data, causing a cascading failure" | https://www.tigerdata.com/blog/10-elasticsearch-production-issues-how-postgres-avoids-them | 2026-01 | CRITICAL |
| 26 | GC overhead documented at 54 seconds of collecting in last 54.9 seconds — node effectively frozen, causing cluster to evict it and trigger shard reallocation storm | https://discuss.elastic.co/t/garbage-collection-pauses-causing-cluster-to-get-unresponsive/18638 | Referenced 2024 | CRITICAL |
| 27 | "Mapping explosion is a common anti-pattern... the cluster state can grow to hundreds of megabytes" causing master node heap exhaustion | https://www.tigerdata.com/blog/10-elasticsearch-production-issues-how-postgres-avoids-them | 2026-01 | HIGH |
| 28 | "Mapping conflicts and silent failures can destabilize cluster performance" — ES automatically detects types from arbitrary keys, creating uncontrolled mapping growth | https://www.tigerdata.com/blog/10-elasticsearch-production-issues-how-postgres-avoids-them | 2026-01 | HIGH |
| 29 | Shard count cannot be changed after index creation — "you must decide the shard count when creating an index without being able to change it later" — too few causes slow recovery when nodes fail | https://www.tigerdata.com/blog/10-elasticsearch-production-issues-how-postgres-avoids-them | 2026-01 | HIGH |
| 30 | "Split-brain scenarios can occur when network partitions isolate nodes" — production data loss risk documented in 2024-2026 context | https://www.tigerdata.com/blog/10-elasticsearch-production-issues-how-postgres-avoids-them | 2026-01 | CRITICAL |
| 31 | "Elasticsearch does not support ACID transactions... data may not be immediately visible" — fundamental durability gap | https://www.tigerdata.com/blog/10-elasticsearch-production-issues-how-postgres-avoids-them | 2026-01 | HIGH |
| 32 | "Without proper observability, issues like JVM heap spikes or unassigned shards can escalate into outages" — implicit operational cost of running ES in production | https://www.tigerdata.com/blog/10-elasticsearch-production-issues-how-postgres-avoids-them | 2026-01 | MEDIUM |
| 33 | Disk flood-stage watermark (95%) triggers read-only blocks on ALL indices on affected nodes — all writes stop, including ongoing ingestion pipelines; commonly reported in production | https://discuss.elastic.co/t/too-many-requests-12-disk-usage-exceeded-flood-stage-watermark-index-has-read-only-allow-delete-block/377233 | 2025-04 | CRITICAL |
| 34 | 7.7% of analyzed clusters had high search rejection queues and 5.1% had high indexing queues — ongoing production degradation in real clusters | https://opster.com/blogs/elasticsearch-best-practices-3000-cluster-analysis/ | 2024 | HIGH |
| 35 | Elasticsearch 8.13.0 known issue: "Due to a bug in the bundled JDK 22, nodes might crash abruptly under high memory pressure" — official advisory recommending JDK downgrade | https://www.elastic.co/docs/release-notes/elasticsearch/known-issues | 2024 | CRITICAL |
| 36 | ES 8.13.0 known issue: "Nodes upgraded to 8.13.0 fail to load downsampling persistent tasks" — breaking bug requiring patch | https://www.elastic.co/docs/release-notes/elasticsearch/known-issues | 2024 | HIGH |
| 37 | ES 8.16.0 ES|QL STATS bug: "incorrect results when the command has exactly two grouping fields, both keywords, where the first field has high cardinality (more than 65k distinct values)" — silent data correctness failure | https://www.elastic.co/docs/release-notes/elasticsearch/known-issues | 2024-2025 | HIGH |
| 38 | AWS Elasticsearch upgrade from 7.1 to 7.4 "stuck during the upgrade process" for 3.5+ hours with no recovery timeline | https://repost.aws/questions/QUtIO94xE8Qca9jeWVYje21A/aws-elasticsearch-stuck-upgrade-processing-after-3-5-hours | 2024 | HIGH |
| 39 | ECK (Elasticsearch on Kubernetes) upgrade left cluster with "Unknown" health status for ~1 hour during configuration-change-triggered node restarts | https://discuss.elastic.co/t/eck-elasticsearch-is-unavailable-and-stuck-during-upgrade-process/247314 | 2024 | HIGH |
| 40 | Rolling upgrade from ES 8.8 to 8.15 resulted in cluster entering "503 master not discovered" state; only recovered after manual index creation triggered state resolution | https://discuss.elastic.co/t/elasticsearch-rolling-upgrade-failed/364907 | 2024-08 | HIGH |
| 41 | ECK master pods failing: "master not discovered or elected yet, an election requires at least 2 nodes" — reported with only 1 master pod running, cluster in async state | https://discuss.elastic.co/t/elasticsearch-master-pods-failing-master-not-discovered-or-elected-yet-an-election-requires-at-least-2-nodes/365591 | 2024 | CRITICAL |
| 42 | "Master not discovered or elected yet, an election requires a node with id [F-Tn-Q6vQuKE0Fgi5qtUMg] + 503 master not discovered exception" — GitHub issue #106639 filed against ES core | https://github.com/elastic/elasticsearch/issues/106639 | 2024 | HIGH |
| 43 | "Mixing master, data, and ingest roles works for small datasets but breaks down under production load where resource contention creates cascading failures" — widely documented operational pattern | https://idlemind.dev/posts/elasticsearch_master_nodes/ | 2025 | HIGH |
| 44 | Performance bottleneck: "only a single node processes ingest pipelines" — single-node ingest bottleneck documented causing cluster-wide slowdown at scale | https://discuss.elastic.co/t/performance-bottleneck-enriching-documents-due-to-only-a-single-node-processing-the-ingest-pipelines/315950 | 2024 | MEDIUM |
| 45 | Shard re-allocation after node failure "taking a very long time" — some 49.9GB shards taking "more than 10 hours to recover" documented on forum | https://discuss.elastic.co/t/shard-re-allocation-taking-a-very-long-time/172190 | Referenced 2024 | HIGH |
| 46 | ILM "slows down node recovery/rolling upgrade process" — documented in discuss.elastic.co with ILM causing prolonged yellow/red states during upgrades | https://discuss.elastic.co/t/ilm-slows-down-node-recovery-rolling-upgrade-process/356476 | 2024 | MEDIUM |
| 47 | CCR Cross-Cluster Replication: "both the leader and follower were overwhelmed (especially the follower with long GC's, nodes dropping from cluster) and stack overflow exceptions were logged" | https://github.com/elastic/elasticsearch/issues/43251 | Referenced 2024 | HIGH |
| 48 | CCR limitation: "While Cluster A is being fixed, the replication process will stop and Cluster B will wait for A to become active again" — DR failover is not automatic | https://discuss.elastic.co/t/cross-cluster-replication-disaster-recovery-failback/275809 | 2024 | HIGH |
| 49 | Elastic Cloud major outage AWS Bahrain (me-south-1): "existing deployments in that region remaining inaccessible" for weeks with "no ETA for mitigation" — weekly status updates only | https://status.elastic.co/history | 2026-03 (ongoing as of 2026-04) | CRITICAL |
| 50 | StatusGator: "Over the past 6 years, Elastic Cloud has had more than 957 outages" — average of ~160 incidents/year on managed Elastic Cloud | https://statusgator.com/services/elastic-cloud | 2024-2026 | HIGH |
| 51 | Data loss after restart: "After several tests, it was found that the lost data was in the translog of node 138" — translog data not replicated before shutdown; lost permanently | https://discuss.elastic.co/t/data-is-lost-after-elasticsearch-restart/328663 | 2024 | CRITICAL |
| 52 | "Shutting down all nodes at once created uncertainty about which node would become master and which shards would be selected as primaries upon recovery" — a common operator mistake causing data loss | https://discuss.elastic.co/t/data-is-lost-after-elasticsearch-restart/328663 | 2024 | CRITICAL |
| 53 | BulkProcessor deadlock: "async request handlers get stuck while waiting for processing semaphores" when cluster fails during bulk processing — requires full restart to recover | https://discuss.elastic.co/t/bulkprocessor-deadlock-on-cluster-failure/110526 | Referenced 2024 | HIGH |
| 54 | "Half-dead node lead to cluster hang" — a node partially failing (not fully down) blocks cluster-wide operations until it is forcibly removed | https://discuss.elastic.co/t/half-dead-node-lead-to-cluster-hang/113658 | Referenced 2024 | CRITICAL |
| 55 | "failed to create engine" (Issue #108842) — production node crash from translog/engine initialization failure during shard recovery | https://github.com/elastic/elasticsearch/issues/108842 | 2024-05 | HIGH |
| 56 | GC pause threshold: hot nodes experiencing GC pauses "sometimes in Minutes" causing "daily Green to Red State Changes due to failed to ping incidents" — chronic production issue | https://discuss.elastic.co/t/long-gc-pauses-on-data-nodes/173251 | Referenced 2024 | CRITICAL |
| 57 | ILM common error: rollover alias mismatch causes ILM to "halt execution" until manually resolved — index lifecycle management requires ongoing operational vigilance | https://www.elastic.co/blog/troubleshooting-elasticsearch-ilm-common-issues-and-fixes | 2024 | MEDIUM |
| 58 | "The cluster running out of disk space can happen when you don't have ILM set up to roll over from hot to warm nodes" — operational gap causing production data loss | https://www.elastic.co/docs/troubleshoot/elasticsearch/fix-common-cluster-issues | 2024 | HIGH |
| 59 | "Cluster hits resource limits" causing ILM to stop executing — requires manual intervention before ILM resumes, during which time indices are not managed | https://www.elastic.co/docs/troubleshoot/elasticsearch/index-lifecycle-management-errors | 2024 | HIGH |
| 60 | Shard allocation failure after rebalancing: "Shard allocation fails during rebalancing" — documented case where manual allocation API intervention required | https://discuss.elastic.co/t/shard-allocation-fails-during-rebalancing/286706 | 2024 | HIGH |
| 61 | "When a node leaves the cluster, the allocation process waits for index.unassigned.node_left.delayed_timeout (by default, one minute) before starting to replicate" — during that minute, replicas are missing | https://www.elastic.co/docs/deploy-manage/maintenance/start-stop-services/full-cluster-restart-rolling-restart-procedures | 2024 | MEDIUM |
| 62 | Elasticsearch is not ACID compliant — "focuses on making data available in near real-time by making engineering choices focused on speed rather than perfectly reliable results" — fundamental design tradeoff | https://bonsai.io/blog/why-elasticsearch-should-not-be-your-primary-data-store | 2024 | HIGH |
| 63 | Companies migrating away from Elasticsearch: Yotpo performed major migration to OpenSearch, presenting at OpenSearch Con 2024 — documented reliability/licensing concerns | https://pureinsights.com/blog/2025/elasticsearch-vs-opensearch-in-2025-what-the-fork/ | 2025 | MEDIUM |
| 64 | Elastic stock dropped 27% in August 2024 (loss of $3B market cap) after licensing changes — widespread loss of user trust following SSPL license decisions | https://craftercms.com/blog/2024/08/elastics-abandonment-of-open-source-a-cautionary-tale-of-profit-over-principles | 2024-08 | MEDIUM |
| 65 | Shard rebalancing after disk watermark crossed causes "relocation churn" — nodes continuously moving shards, blocking writes and degrading search performance during rebalancing | https://discuss.elastic.co/t/minimizing-avoiding-churn-due-to-relocation-and-rebalancing-during-nodal-outages-in-cluster/190924 | Referenced 2024 | HIGH |
| 66 | "Poorly designed ingestion pipelines can lead to slow queries, oversized indices, and costly infrastructure" — real-world production failure mode described in 2025 | https://dattell.com/data-architecture-blog/log-ingestion-best-practices-for-elasticsearch-in-2025/ | 2025 | MEDIUM |
| 67 | "Massive performance degradation when terms filter has over 16 values" — ES 8.x regression causing 10x+ slowdown on common production query patterns | https://discuss.elastic.co/t/massive-performance-degradation-when-terms-filter-has-over-16-values/349106 | 2024 | HIGH |
| 68 | "Elasticsearch process is killed by OOM killer" — documented case in discuss.elastic.co; heap misconfiguration or unbounded caches cause node to be OOM-killed by OS | https://discuss.elastic.co/t/elasticsearch-process-is-killed-by-oom-killer/217792 | Referenced 2024 | CRITICAL |
| 69 | "Replica shards of newly created indices remain UNASSIGNED" — active production issue where misconfiguration or resource shortage prevents replica placement; cluster runs at risk | https://discuss.elastic.co/t/replica-shards-of-newly-created-indices-remain-unassigned/379459 | 2024-2025 | HIGH |
| 70 | When node reaches 95% disk (flood stage): "Elasticsearch sets all indices on the affected node to read-only" and "automatically removed when disk usage falls below the high watermark" — but during outage all writes are blocked | https://www.elastic.co/docs/troubleshoot/elasticsearch/fix-watermark-errors | 2024 | HIGH |
| 71 | Reindex of GitLab.com Global Search ES cluster required to "fix large segments" — performance regression discovered in production, requiring full reindex of production data | https://gitlab.com/gitlab-com/gl-infra/production/-/work_items/3172 | 2024 | MEDIUM |
| 72 | "A large recursion using the innerForbidCircularReferences function of the PatternBank class could cause uncontrolled resource consumption" (ESA-2024-34) — security/stability vulnerability in 8.15.1 | https://discuss.elastic.co/t/elasticsearch-8-15-1-security-update-esa-2024-34/376919 | 2024 | HIGH |
| 73 | "A flaw was discovered in Elasticsearch, affecting document ingestion when an index template contains a dynamic field mapping of 'passthrough' type" causing StackOverflow (ESA-2024-14) | https://discuss.elastic.co/t/elasticsearch-8-14-0-security-update-esa-2024-14/361007 | 2024 | HIGH |
| 74 | "18% of clusters using over 70% of disk capacity" in Opster analysis — indicating widespread risk of hitting watermark limits and triggering cluster-wide write blocks | https://opster.com/blogs/elasticsearch-best-practices-3000-cluster-analysis/ | 2024 | HIGH |
| 75 | "Node crashes can cause data loss" — GitHub issue #10933, long-standing core ES issue; WAL not always flushed before crash, especially under high write pressure | https://github.com/elastic/elasticsearch/issues/10933 | Referenced 2024 | CRITICAL |

---

## Thematic Summary

### Split Brain / Master Election Failures (data points: 1, 2, 15, 16, 17, 41, 42, 43)
Still a real operational hazard, especially in 2-node clusters and Kubernetes deployments. ES 7+ mitigated classic split-brain via auto-quorum, but 2-node clusters are fundamentally unsupported for rolling upgrades and exhibit silent non-resilience.

### Shard Management Failures (data points: 3, 13, 14, 29, 45, 60, 61, 65, 69)
Too many shards degrades master node performance, blocks allocation, and makes recovery take hours to days. Too few shards causes hotspots. The number is immutable post-creation. 15k+ shards per cluster is documented as causing cluster instability.

### JVM / GC Pauses Causing Node Eviction (data points: 22, 23, 24, 25, 26, 56, 68)
Stop-the-world GC pauses >30s cause Elasticsearch to assume a node is dead and trigger shard reallocation. This is a cascading failure — reallocation puts more pressure on remaining nodes, potentially triggering more GC pauses.

### Rolling Restarts / Upgrades Taking Excessive Time (data points: 4, 5, 6, 7, 38, 39, 40, 46)
Production rolling restarts routinely take hours to days for large clusters. The 2-node upgrade constraint is a hidden landmine. ILM interferes with upgrade speed.

### Data Loss Events (data points: 30, 31, 51, 52, 75)
ES is not ACID-compliant. Simultaneous full-cluster shutdown, translog loss, and flood-stage read-only blocks are all documented data loss mechanisms.

### Cluster State / Large Index Count (data points: 10, 11, 12, 27, 28)
Master node heap exhaustion from large cluster state is a documented production failure pattern. 25,000 indices in a single cluster is a real observed case.

### Disk Watermark / Read-Only Locks (data points: 19, 33, 58, 70, 74)
~13-18% of production clusters are at risk of hitting disk watermarks. When hit, ALL writes on affected nodes are blocked until space is freed.

### Circuit Breakers / Request Rejection (data points: 18, 34, 72, 73)
15% of clusters trip circuit breakers regularly. Security vulnerabilities in ES 8.14/8.15 exposed nodes to resource exhaustion attacks.

### Operational / Managed Service Outages (data points: 49, 50)
Elastic Cloud has had 957+ recorded outages over 6 years. The March 2026 AWS Bahrain outage left deployments inaccessible for weeks with no ETA.
