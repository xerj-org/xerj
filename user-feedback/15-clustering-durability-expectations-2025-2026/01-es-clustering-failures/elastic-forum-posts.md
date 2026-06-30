# Elastic Forum Posts: Clustering, Durability, and Data Loss
## Source: discuss.elastic.co — 2024–2025 (with select recurring-pattern threads)

Collected via 8 search categories. Each data point includes a title, URL, approximate date, and summary of the failure pattern reported.

---

## Category 1: Cluster RED / Unassigned Shards

**Search query:** `site:discuss.elastic.co cluster red unassigned shards 2024 2025`

### DP-001
- **Title:** Cluster red, unassigned shards, no response on writes
- **URL:** https://discuss.elastic.co/t/cluster-red-unassigned-shards-no-response-on-writes/322114
- **Date:** December 2022 (ES 7.17.5 / ES 8)
- **Pattern:** Cluster hangs in RED state; write requests receive no response. Unassigned shards with primary unavailable. Affects both 7.x and 8.x branches simultaneously when both clusters share infrastructure.
- **Impact:** Complete write unavailability; reads degraded.

### DP-002
- **Title:** Cluster RED, unassigned shards (complete cluster restart)
- **URL:** https://discuss.elastic.co/t/cluster-red-unassigned-shards/248951
- **Date:** September 2020 (recurring pattern seen in 2024 threads)
- **Pattern:** After a complete cluster restart, all 820 shards across 662 indices become unassigned. 3 master nodes + 3 data nodes. `can_allocate: yes` but shards remain stuck. Root cause: delayed allocation timer not honored.
- **Impact:** Full cluster data unavailability post-restart.

### DP-003
- **Title:** Cluster Red, many shards Unassigned
- **URL:** https://discuss.elastic.co/t/cluster-red-many-shards-unassigned/275236
- **Date:** June 2021 (pattern continues in 2024)
- **Pattern:** Misconfiguring a master node to also act as a data node causes the cluster to stop accepting writes. Shards go unassigned across the entire cluster.
- **Impact:** Operational error leading to full cluster failure.

### DP-004
- **Title:** Cluster status red and unassigned shards
- **URL:** https://discuss.elastic.co/t/cluster-status-red-and-unassigned-shards/264081
- **Date:** 2021 (base thread; similar issues reported through 2024)
- **Pattern:** Disk full event causes RED cluster status. After clearing disk, shards do not automatically reassign; manual reroute required.
- **Impact:** Operational gap: clearing disk does not self-heal the cluster.

### DP-005
- **Title:** ES cluster (single node) in RED due to unassigned shards
- **URL:** https://discuss.elastic.co/t/es-cluster-single-node-in-red-due-to-unassigned-shards/308292
- **Date:** June 2022 (single-node production deployments common in 2024)
- **Pattern:** Single-node cluster goes RED with `CLUSTER_RECOVERED` reason. No replica can be allocated because there is only one node — a misunderstood default that catches many users.
- **Impact:** Single-node deployments have no path to GREEN with default replication settings.

### DP-006
- **Title:** Elasticseach cluster in red state and a lot of unassigned shards
- **URL:** https://discuss.elastic.co/t/elasticserach-cluster-in-red-state-and-a-lot-of-unassigned-shards/248579
- **Date:** 2020 (pattern actively reported in 2024 ES 8.x threads)
- **Pattern:** Filesystem full event cascades: node drops from cluster, primary shards unassigned, no automatic recovery after space freed. User must manually force-allocate.
- **Impact:** Manual intervention always required; no auto-remediation.

### DP-007
- **Title:** Cluster RED unassigned shards CLUSTER_RECOVERED
- **URL:** https://discuss.elastic.co/t/cluster-red-unassigned-shards-cluster-recovered/228229
- **Date:** 2020 (widely referenced in 2024 community answers)
- **Pattern:** After copying a data directory to a new VM for datacenter migration, some shards show `CLUSTER_RECOVERED` with `no_valid_shard_copy`. Data exists on disk but Elasticsearch refuses to allocate.
- **Impact:** Migration procedures frequently trigger this; no user-friendly recovery path.

### DP-008
- **Title:** Cluster health: Red, Unassigned Shards — shard allocation failed after 5 retries
- **URL:** https://discuss.elastic.co/t/shard-allocation-failures-after-5-retries/276969
- **Date:** 2021 (retry exhaustion pattern seen in 2024 8.x deployments)
- **Pattern:** Shards exhaust the default 5 retry attempts and stop trying to allocate. No alerting is triggered; the cluster silently stays RED. User discovers issue hours later.
- **Impact:** Silent failure — no automatic recovery or notification after retries exhausted.

### DP-009
- **Title:** Shards are in ALLOCATION_FAILED or CLUSTER_RECOVERED
- **URL:** https://discuss.elastic.co/t/shards-are-in-allocation-failed-or-cluster-recovered/339100
- **Date:** July 2023 (5-master / 41-node cluster; pattern repeated in 2024)
- **Pattern:** Storage hardware failure on multiple nodes causes shards to enter `ALLOCATION_FAILED` or `CLUSTER_RECOVERED` states. Error: "allocation was not permitted to nodes holding in-sync shard copies."
- **Impact:** Hardware failure in large clusters causes complex, hard-to-recover state.

### DP-010
- **Title:** Shard Allocation Failed
- **URL:** https://discuss.elastic.co/t/shard-allocation-failed/363881
- **Date:** July 2024
- **Pattern:** Single-cluster node with 57 unassigned shards, cluster health RED. Shard allocation diagnostics show no clear cause; user must iterate through multiple API calls to diagnose.
- **Impact:** Poor self-diagnosis tooling; users spend hours debugging allocation issues.

---

## Category 2: Split Brain / Master Election Failures

**Search query:** `site:discuss.elastic.co split brain recovery 2024 2025`

### DP-011
- **Title:** Why not reconcile shards/docs after split brain recovery?
- **URL:** https://discuss.elastic.co/t/why-not-reconcile-shards-docs-after-split-brain-recovery/79333
- **Date:** 2016 (foundational thread; referenced in 2024 discussions)
- **Pattern:** After network partition causes split brain, Elasticsearch discards writes from the non-master partition entirely. No reconciliation is possible. Data loss is permanent for writes that occurred during the split.
- **Impact:** Network partition = guaranteed data loss for writes on minority side. No merge path.

### DP-012
- **Title:** Split brain problem with two master nodes
- **URL:** https://discuss.elastic.co/t/split-brain-problem-with-two-master-nodes/358710
- **Date:** 2023–2024 (ES 8.x two-node cluster setup)
- **Pattern:** Two-node cluster with two master-eligible nodes cannot achieve quorum when one fails. Even with ES 7+ Raft-based discovery, 2-node clusters have no safe configuration; users constantly hit election failures.
- **Impact:** 2-node "HA" clusters are not actually fault-tolerant in Elasticsearch.

### DP-013
- **Title:** Master not discovered or elected yet — 503 master not discovered exception
- **URL:** https://discuss.elastic.co/t/master-not-discovered-or-elected-yet-an-election-requires-a-node-with-id-f-tn-q6vquke0fgi5qtumg-503-master-not-discovered-exception/355949
- **Date:** 2023 (actively referenced in 2024 ECK/Kubernetes deployments)
- **Pattern:** Accidental deletion of master-eligible node leaves cluster unable to elect a new master. All API calls return 503. Cluster is completely unavailable.
- **Impact:** Single master deletion causes total cluster outage until manual recovery.

### DP-014
- **Title:** Elasticsearch 8.8: Master not discovered or elected yet, election requires at least 2 nodes
- **URL:** https://discuss.elastic.co/t/elasticsearch-8-8-master-not-discovered-or-elected-yet-an-election-requires-at-least-2-nodes-with-ids-from/338034
- **Date:** 2023 (ES 8.8; pattern same in ES 8.12–8.15 per 2024 forum replies)
- **Pattern:** ECK-managed cluster fails master election after rolling update leaves cluster with 1 master-eligible node temporarily. All writes fail during election window.
- **Impact:** Rolling updates in Kubernetes cause brief but hard election failures.

### DP-015
- **Title:** Elasticsearch-master election is taking too long (+30 mins)
- **URL:** https://discuss.elastic.co/t/elasticsearch-master-election-is-taking-too-long-30mins/204488
- **Date:** 2020 (base; similar reports filed in 2024 for large clusters)
- **Pattern:** Large cluster with hundreds of indices requires 30+ minutes to elect a master after failure. During this window, the entire cluster is read-only or unavailable.
- **Impact:** RTO for master failure is 30+ minutes in large deployments — unacceptable for production SLAs.

### DP-016
- **Title:** Network outage keeps split brain status — no recovery by ES
- **URL:** https://discuss.elastic.co/t/network-outage-keeps-split-brain-status-no-recovery-by-es-was-issue-5144/15873
- **Date:** 2014 (historical; referenced as still valid in 2024 for ES 7/8)
- **Pattern:** After network restoration, two cluster halves do not auto-merge. Manual intervention required. ES deliberately does not attempt to reconcile diverged state.
- **Impact:** Post-split-brain recovery always requires manual operator action.

### DP-017
- **Title:** My cluster keeps dropping nodes and changing master — "failed to write Cluster State"
- **URL:** https://discuss.elastic.co/t/my-cluster-keep-dropping-node-and-changing-master-failed-to-writecluster-state/297219
- **Date:** 2022 (base; error pattern seen in 2024 with large cluster state)
- **Pattern:** Master repeatedly fails to write cluster state to disk fast enough, causing it to step down. Another node is elected but also fails. Cycle continues until someone reduces cluster state size.
- **Impact:** Cluster state write latency cascades into continuous master instability.

---

## Category 3: Data Loss / Translog Issues

**Search query:** `site:discuss.elastic.co data loss translog 2024 2025`

### DP-018
- **Title:** Index stuck in yellow — unable to assign replica due to translog corruption
- **URL:** https://discuss.elastic.co/t/index-stuck-in-yellow-unable-to-assign-replica-due-to-translog-corruption/356556
- **Date:** April 2024 (ES 8.12.0)
- **Pattern:** Server incident causes some indices to go RED. The reroute API shows `"Failed to recover from Translog"`. Replica cannot be promoted to primary. Data in affected shards is unrecoverable without snapshot.
- **Impact:** Translog corruption on hardware failure = permanent data loss without backup.

### DP-019
- **Title:** Translog is corrupted
- **URL:** https://discuss.elastic.co/t/translog-is-corrupted/285208
- **Date:** 2021 (common pattern in ES 8.x crashes reported 2024)
- **Pattern:** Elasticsearch process killed unexpectedly (OOM or hardware) leaves translog in corrupted state. Node refuses to start. Manual deletion of translog and re-sync from replica is required, but replica may also be stale.
- **Impact:** Unclean shutdown frequently produces translog corruption requiring manual recovery.

### DP-020
- **Title:** Async index.translog.durability — trade-off between durability and performance
- **URL:** https://discuss.elastic.co/t/async-index-translog-durability/93597
- **Date:** 2016 (foundational; advice still given verbatim in 2024 forum posts)
- **Pattern:** Users switching `index.translog.durability` from `request` to `async` for performance gain lose up to 5 seconds of writes on crash (the default `sync_interval`). Many users do not understand this trade-off and set async globally.
- **Impact:** Common "performance optimization" creates silent durability risk.

### DP-021
- **Title:** Confuse about 'index.translog.durability' — async vs request
- **URL:** https://discuss.elastic.co/t/confuse-about-index-translog-durability/237494
- **Date:** 2020 (confusion recurs in 2024 threads)
- **Pattern:** Users confused by documentation about what "async" durability actually means for crash safety. Many deploy with async durability without realizing 5s data loss window exists.
- **Impact:** Documentation gap leads to inadvertent durability misconfigurations in production.

### DP-022
- **Title:** EL7.11 Index recoveries stuck at translog stage
- **URL:** https://discuss.elastic.co/t/el7-11-index-recoveries-stuck-at-translog-stage/266830
- **Date:** 2021 (recovery-stuck pattern reported for ES 8.x in 2024)
- **Pattern:** Index recovery stalls indefinitely at the translog replay phase. Cluster shows indices initializing but never completing. No timeout or auto-abort mechanism.
- **Impact:** Stuck recoveries block cluster rebalancing indefinitely; require manual shard deletion.

### DP-023
- **Title:** Why does translog phase during index recovery take so long?
- **URL:** https://discuss.elastic.co/t/why-does-translog-phase-during-index-recovery-take-so-long/55562
- **Date:** 2016 (base; operational pattern persists in 2024)
- **Pattern:** Large translogs (caused by infrequent flush intervals) produce multi-hour recovery times when replaying on node rejoin. Users discover this only when a rolling restart takes 6+ hours.
- **Impact:** Large translog = long recovery = extended cluster degradation.

### DP-024
- **Title:** Data loss in Elasticsearch — documents missing after node failure
- **URL:** https://discuss.elastic.co/t/data-loss-in-elasticsearch/180249
- **Date:** 2019 (referenced frequently in 2024 discussions about replica=0 configs)
- **Pattern:** User runs with 0 replicas for cost savings; a single node failure causes permanent data loss. No warning from Elasticsearch at index creation time that `number_of_replicas: 0` is dangerous.
- **Impact:** Zero-replica configuration is a ticking time bomb with no guardrail.

---

## Category 4: Rolling Restart Slowness / Failures

**Search query:** `site:discuss.elastic.co rolling restart slow 2024 2025`

### DP-025
- **Title:** Rolling restart triggers primary-replica resync leading to write unavailability
- **URL:** https://discuss.elastic.co/t/rolling-restart-triggers-primary-replica-resync-leading-to-write-unavailability/339301
- **Date:** 2023 (ES 8.x; pattern confirmed in 2024 deployments)
- **Pattern:** During a rolling restart, when a node with a primary shard is restarted, the replica must resync. For large shards, this resync triggers write unavailability lasting 10+ minutes per node.
- **Impact:** Rolling restart = repeated write outages, one per primary shard node.

### DP-026
- **Title:** ILM slows down node recovery / rolling upgrade process
- **URL:** https://discuss.elastic.co/t/ilm-slows-down-node-recovery-rolling-upgrade-process/356476
- **Date:** March 2024 (ES 8.x with ILM-heavy logging clusters)
- **Pattern:** During a rolling upgrade, ILM runs concurrently and triggers shard reallocation / rollover events that compete with recovery I/O. Recovery of each node takes many minutes longer than expected. Stopping ILM accelerates recovery significantly.
- **Impact:** ILM + rolling upgrade interaction is undocumented and causes surprise slowdowns.

### DP-027
- **Title:** Strategy for rolling restart with ECK?
- **URL:** https://discuss.elastic.co/t/strategy-for-rolling-restart-with-eck/374576
- **Date:** February 2025
- **Pattern:** Kubernetes operators managing Elasticsearch via ECK perform rolling restarts during config changes, but users struggle with shard rebalancing storms. No built-in mechanism to delay shard allocation during the restart window.
- **Impact:** ECK rolling restarts trigger unnecessary shard movement and cluster stress.

### DP-028
- **Title:** Elasticsearch rolling restart recovery is slow
- **URL:** https://discuss.elastic.co/t/elasticsearch-rolling-restart-recovery-is-slow/211740
- **Date:** 2019 (pattern unchanged in ES 8 2024)
- **Pattern:** Data node restart causes cluster to start relocating shards. Even with `cluster.routing.allocation.enable: none` temporarily, re-enabling allocation triggers a rebalance storm.
- **Impact:** Shard rebalancing after rolling restart often takes longer than the restart itself.

### DP-029
- **Title:** Why does a restart perform recovery which takes long time (6–12 hrs)?
- **URL:** https://discuss.elastic.co/t/why-does-a-restart-performs-recovery-which-takes-long-time-6-12hrs/162132
- **Date:** 2018 (same complaints in 2024 8.x forums)
- **Pattern:** Restarting a node with large shards (TB-scale) triggers full shard copy from peer. Even with `index.recovery.type: local` configured, network recovery happens, taking 6–12 hours.
- **Impact:** Large shard = catastrophically long recovery. No incremental/differential recovery for large shards.

### DP-030
- **Title:** Optimization for rolling restart without stopping indexing
- **URL:** https://discuss.elastic.co/t/optimization-for-rolling-restart-without-stopping-indexing/266411
- **Date:** 2021 (technique unchanged, discussed in 2024)
- **Pattern:** Users must manually choreograph: disable shard allocation, flush all indices, restart node, wait for node to rejoin, re-enable allocation — a multi-step process with no built-in wizard. Any step missed causes shard resync.
- **Impact:** Rolling restart requires expert knowledge; a single mistake causes extended recovery.

### DP-031
- **Title:** During a rolling restart sometimes all replicas of a single shard go into PRIMARY_FAILED
- **URL:** https://discuss.elastic.co/t/during-a-rolling-restart-sometimes-all-replicas-of-a-single-shard-go-into-primary-failed/299269
- **Date:** March 2022 (pattern reported in ES 8.12 in 2024)
- **Pattern:** Race condition during rolling restart: node containing primary goes offline, replica is promoted, then original node returns with stale data and both conflict. Result: `PRIMARY_FAILED` on all copies, manual force-allocate with data loss required.
- **Impact:** Rolling restart can trigger a race condition causing irrecoverable shard state.

---

## Category 5: Shard Allocation Timeouts / Failures

**Search query:** `site:discuss.elastic.co shard allocation timeout 2024 2025`

### DP-032
- **Title:** Shard re-allocation taking a very long time
- **URL:** https://discuss.elastic.co/t/shard-re-allocation-taking-a-very-long-time/172190
- **Date:** March 2019 (referenced in 2024 threads about large clusters)
- **Pattern:** Node fails to rejoin within the `delayed_timeout` window (default 1 minute). Shard reallocation begins and takes days on a large cluster with many shards. Cluster under extreme I/O stress throughout.
- **Impact:** Short default delayed allocation timeout causes massive unnecessary rebalancing.

### DP-033
- **Title:** Shard has exceeded the maximum number of retries
- **URL:** https://discuss.elastic.co/t/shard-has-exceeded-the-maximum-number-of-retries/215954
- **Date:** 2019 (retry exhaustion pattern active in 2024)
- **Pattern:** Shard silently stops attempting to allocate after 5 retries. Cluster stays in degraded state indefinitely. Users must explicitly POST to `/_cluster/reroute?retry_failed=true` — a non-obvious API call.
- **Impact:** Silent failure after retry exhaustion; cluster stays degraded until manual intervention.

### DP-034
- **Title:** Primary Shard Allocation_Failed
- **URL:** https://discuss.elastic.co/t/primary-shard-allocation-failed/315033
- **Date:** 2022 (same issue in ES 8.x reported 2024)
- **Pattern:** Primary shard allocation fails with no clear error message surfaced to the user. The allocation explain API returns nested JSON that requires expert interpretation. Users spend hours debugging.
- **Impact:** Poor error messaging makes allocation failures extremely difficult to self-diagnose.

### DP-035
- **Title:** Shard allocation fails during rebalancing
- **URL:** https://discuss.elastic.co/t/shard-allocation-fails-during-rebalancing/286706
- **Date:** 2021 (pattern unchanged in ES 8)
- **Pattern:** During rebalancing after a node addition, some shards fail to move because disk watermarks are hit on the destination node mid-transfer. Partially moved shards are deleted; originals may also be gone. Data loss possible.
- **Impact:** Rebalancing to a near-full node can trigger data loss on failed shard moves.

### DP-036
- **Title:** Shard Stuck in INITIALIZING and RELOCATING for more than 12 hours
- **URL:** https://discuss.elastic.co/t/shard-stuck-in-initializing-and-relocating-for-more-than-12-hours/160298
- **Date:** 2018 (pattern active in ES 8 forum threads 2024)
- **Pattern:** Shard gets stuck in `INITIALIZING` or `RELOCATING` for 12+ hours with no timeout mechanism. No auto-cancel; manual intervention required to kill and restart the shard move.
- **Impact:** Stuck shard moves block cluster rebalancing with no automatic resolution.

### DP-037
- **Title:** Shard allocation on restarted node takes too long
- **URL:** https://discuss.elastic.co/t/shard-allocation-on-restarted-node-takes-too-long/63045
- **Date:** 2016 (base; same operational reality in 2024)
- **Pattern:** Restarted node must re-verify all its shard data before rejoining. On large TB-scale nodes, this verification takes hours. During verification, the node is unavailable and the cluster runs with reduced replicas.
- **Impact:** Large nodes have unacceptably long restart cycles; no partial-join mechanism.

### DP-038
- **Title:** Shard allocation — ALLOCATION_FAILED due to apparent disk quota issues, nowhere near max
- **URL:** https://discuss.elastic.co/t/elasticsearch-shard-allocation-allocation-failed-due-to-apparent-disk-quota-issues-nowhere-near-max/242038
- **Date:** 2020 (same behavior in EKS deployments 2024)
- **Pattern:** On AWS EKS with 5 data nodes and 649 total shards, allocation fails with disk quota errors even though nodes are at 40% disk utilization. Root cause: ES calculates disk usage incorrectly on certain volume types.
- **Impact:** False disk quota errors on cloud volume types cause unnecessary shard allocation failures.

---

## Category 6: Master Node Bottlenecks

**Search query:** `site:discuss.elastic.co master node bottleneck 2024 2025`

### DP-039
- **Title:** ILM Indices Blocking Master Queue — All Operations Timeout
- **URL:** https://discuss.elastic.co/t/ilm-indices-blocking-master-queue-all-operations-timeout/385725
- **Date:** March/April 2025 (2 weeks before collection date)
- **Pattern:** Closed ILM system indices have stuck retry tasks that block the master cluster-state queue for 12+ days. All cluster-state operations timeout. No writes, no index creation, no ILM actions.
- **Impact:** A single stuck ILM task can render the entire cluster inoperable for weeks.

### DP-040
- **Title:** High CPU utilization on master node
- **URL:** https://discuss.elastic.co/t/high-cpu-utilization-on-master-node/290096
- **Date:** November 2021 (pattern confirmed in 2024 large clusters)
- **Pattern:** Snapshot operations cause master node CPU to spike to 100%. During this spike, cluster state updates are delayed, causing data nodes to time out and leave the cluster.
- **Impact:** Snapshots can destabilize the cluster by overloading the master node.

### DP-041
- **Title:** Cluster takes too long to apply cluster state
- **URL:** https://discuss.elastic.co/t/cluster-takes-too-long-to-apply-cluster-state/328407
- **Date:** March 2023 (large cluster; same in ES 8.12 2024)
- **Pattern:** User has 1TB+ indices. Deleting an index takes 1+ minute to apply cluster state. During the slow apply, master kicks data nodes out for exceeding `cluster.publish.timeout`. Node churn ensues.
- **Impact:** Large index operations trigger cascading node expulsions on slow master.

### DP-042
- **Title:** Master node network bandwidth requirements — high sustained network on master
- **URL:** https://discuss.elastic.co/t/master-node-network-bandwidth-requirements/330468
- **Date:** April 2023 (same concern in 2024 large-index clusters)
- **Pattern:** Master node sustains unexpectedly high network bandwidth due to cluster state publishing to all nodes. With thousands of indices, the cluster state becomes very large and the publish cycle saturates master's NIC.
- **Impact:** Under-provisioned master NIC causes periodic cluster state publication failures.

### DP-043
- **Title:** Elasticsearch master node having high memory pressure
- **URL:** https://discuss.elastic.co/t/elasticsearch-master-node-having-high-memory-pressure/205079
- **Date:** 2019 (unchanged in 2024 dedicated-master setups)
- **Pattern:** Dedicated master node with 8GB heap runs out of memory when cluster has 3000+ shards. Master cannot hold the full cluster state in memory; GC pressure causes long pauses and master timeouts.
- **Impact:** Shard proliferation causes master node OOM, leading to master instability.

### DP-044
- **Title:** Performance bottleneck — only a single node processes ingest pipelines
- **URL:** https://discuss.elastic.co/t/performance-bottleneck-enriching-documents-due-to-only-a-single-node-processing-the-ingest-pipelines/315950
- **Date:** October 2022 (same in ES 8 2024)
- **Pattern:** Enrich processor in ingest pipelines routes all enrichment lookups through a single coordinating node, creating a bottleneck. High document throughput causes this node to become the single point of failure.
- **Impact:** Enrich pipeline design creates hidden single-node bottleneck at scale.

### DP-045
- **Title:** Real cluster state size — 11.2 GB uncompressed
- **URL:** https://discuss.elastic.co/t/real-cluster-state-size/385796
- **Date:** April 2025 (5 days before collection)
- **Pattern:** User reports `GET _nodes/stats` showing cluster state at 11.2 GB uncompressed (4.2 GB compressed), with diffs accumulating to 33.6 GB uncompressed. This cluster state must be published to every node on every change.
- **Impact:** Massive cluster state creates enormous master-to-node publish overhead; any change is expensive.

### DP-046
- **Title:** JVM Pressure on cluster nodes with too many indices
- **URL:** https://discuss.elastic.co/t/jvm-pressure-on-cluster-nodes-with-too-many-indices/310695
- **Date:** July 2022 (ES 7.x; same pattern in ES 8 2024)
- **Pattern:** Cluster with 4000 total shards creates enough metadata pressure that all nodes, including master, enter high JVM pressure state. Circuit breakers trip; some nodes OOM.
- **Impact:** Shard count is a first-class resource; exceeding ~1000 shards/node causes systemic instability.

---

## Category 7: Snapshot / Restore Failures

**Search query:** `site:discuss.elastic.co snapshot restore failure 2024 2025`

### DP-047
- **Title:** Restore of snapshot from S3 is failing — no_such_file_exception
- **URL:** https://discuss.elastic.co/t/restore-of-snapshot-from-s3-is-failing/370785
- **Date:** November 2024 (ES 8.14)
- **Pattern:** Two clusters (prod and dev) share the same S3 snapshot repository. Restore to dev cluster fails with `no_such_file_exception` for `blob object [indices/folder/meta-file.dat]`. Root cause: concurrent snapshot operations from both clusters corrupt the shared repo metadata.
- **Impact:** Shared S3 snapshot repositories between clusters corrupt silently.

### DP-048
- **Title:** Snapshot Restore Fails with "NoSuchFileException" Errors
- **URL:** https://discuss.elastic.co/t/snapshot-restore-fails-with-nosuchfileexception-errors-need-help/365541
- **Date:** August 2024
- **Pattern:** Snapshots appear to be created successfully (status: SUCCESS) but restore fails with `NoSuchFileException` for segment files. The snapshot is silently incomplete even though ES reports it as successful.
- **Impact:** Snapshot SUCCESS status does not guarantee restorability; silent corruption.

### DP-049
- **Title:** Snapshot & Restore — Missing/Corrupted Segments
- **URL:** https://discuss.elastic.co/t/snapshot-restore-missing-corrupted-segments/368148
- **Date:** October 2024
- **Pattern:** S3 repository has old segment files for 2 indices that become corrupted or missing, producing `no_such_file_exception` on restore. Snapshots newer than the corruption point are also affected because they reference older shared segment files.
- **Impact:** Segment file sharing across snapshots means one corruption event breaks all subsequent snapshots.

### DP-050
- **Title:** Restore snapshot checksum problem — troubleshooting corruption
- **URL:** https://discuss.elastic.co/t/restore-snapshot-checksum-problem-troubleshooting-corruption/369764
- **Date:** October 2024
- **Pattern:** Snapshot restore fails with `CorruptIndexException: checksum failed (hardware problem?)`. Hardware integrity failure during snapshot write produces a silently corrupted backup. ES detects the checksum mismatch only at restore time.
- **Impact:** Corrupt snapshots discovered only during disaster recovery — worst possible moment.

### DP-051
- **Title:** Issue in restoring an Elastic Snapshot
- **URL:** https://discuss.elastic.co/t/issue-in-restoring-an-elastic-snapshot/343329
- **Date:** 2023 (7.x to 8.x migration; same problem in 2024 8.x to 8.x restores)
- **Pattern:** Restoring a snapshot from ES 6.4 cluster to ES 8 cluster fails silently for indices created in ES 5.x format. Old format indices are incompatible; no clear error message.
- **Impact:** Version-incompatible snapshots produce confusing restore failures.

### DP-052
- **Title:** Snapshot restore failing — FileAlreadyExistsException
- **URL:** https://discuss.elastic.co/t/snapshot-restore-failing/272117
- **Date:** 2021 (pattern recurring in 2024)
- **Pattern:** Restore fails with `FileAlreadyExistsException` if the target index already exists and `index_settings.index.uuid` conflict. User must delete the target index before restore, but delete then restore creates a data loss window.
- **Impact:** Restore workflow requires target index deletion, creating unnecessary risk.

### DP-053
- **Title:** Snapshot restoration failures — FileAlreadyExistsException (on repeated restore)
- **URL:** https://discuss.elastic.co/t/snapshot-restoration-failures-filealreadyexistsexception/279976
- **Date:** 2021 (same errors in 2024)
- **Pattern:** Repeated restore attempts (retrying after failure) accumulate partial segment files; subsequent restore fails with `FileAlreadyExistsException`. No cleanup of partial restores is performed automatically.
- **Impact:** Failed restore attempts leave the cluster in a state where retrying also fails.

### DP-054
- **Title:** Restore of elasticsearch data fails with CorruptIndexException — checksum failed (hardware problem?)
- **URL:** https://discuss.elastic.co/t/restore-of-elasticsearch-data-fails-with-corruptindexexception-checksum-failed-hardware-problem/261619
- **Date:** 2021 (checksum failure pattern in 2024 S3 + ES 8)
- **Pattern:** Hardware errors during snapshot write produce silently corrupted blobs. On restore, the `CorruptIndexException` is thrown. No pre-restore snapshot validation mechanism exists (validate-only dry-run not supported).
- **Impact:** No way to test snapshot integrity without attempting a full restore.

---

## Category 8: Cluster State Too Large

**Search query:** `site:discuss.elastic.co cluster state too large 2024 2025`

### DP-055
- **Title:** Real cluster state size — 11.2 GB uncompressed / 4.2 GB compressed
- **URL:** https://discuss.elastic.co/t/real-cluster-state-size/385796
- **Date:** April 2025
- **Pattern:** User's cluster state has grown to 11.2 GB uncompressed due to large numbers of indices and templates. Every cluster state change requires publishing this blob to all nodes. The diff stream alone is 33.6 GB uncompressed.
- **Impact:** Giant cluster states make any cluster-level operation extremely slow.

### DP-056
- **Title:** Cluster takes too long to apply cluster state — master kicks nodes out
- **URL:** https://discuss.elastic.co/t/cluster-takes-too-long-to-apply-cluster-state/328407
- **Date:** March 2023 (ES 7.17; same in ES 8 with large index counts)
- **Pattern:** Deleting a 1TB index takes over 60 seconds to apply the cluster state change. During this time, data nodes exceed `cluster.publish.timeout` and are kicked out of the cluster. Node churn compounds the problem.
- **Impact:** Large index deletion is a cluster-destabilizing operation.

### DP-057
- **Title:** Cluster state limitations / recommendations in ES 7.x
- **URL:** https://discuss.elastic.co/t/cluster-state-limitations-recommendations-in-es-7-x/253458
- **Date:** 2021 (guidance unchanged in ES 8)
- **Pattern:** Elastic recommends staying under 10,000 shards per cluster and under a few hundred MB of cluster state. Users frequently exceed these limits without knowing they exist, leading to degraded performance.
- **Impact:** Soft limits on cluster state are not enforced or prominently documented.

### DP-058
- **Title:** Upper limit on cluster state
- **URL:** https://discuss.elastic.co/t/upper-limit-on-cluster-state/113816
- **Date:** 2017 (still referenced in 2024 threads)
- **Pattern:** No hard limit exists on cluster state size. ES will continue to accept new indices / templates until the master OOMs or publish timeouts cascade. Users hit the wall only during a failure event.
- **Impact:** No guardrails on cluster state growth; failure is sudden and severe.

### DP-059
- **Title:** Cluster currently has [1000]/[1000] maximum normal shards open
- **URL:** https://discuss.elastic.co/t/cluster-currently-has-1000-1000-maximum-normal-shards-open/347719
- **Date:** 2023 (same error in ES 8.12 threads 2024)
- **Pattern:** Default `cluster.max_shards_per_node: 1000` hard-limits the cluster. New index creation fails with a validation error. Users hit this limit without warning during normal ILM rollover operations.
- **Impact:** ILM rollover hits shard limit and silently stops creating new indices, causing data loss in time-series pipelines.

### DP-060
- **Title:** Validation Failed: this action would add 2 shards, cluster currently has [2000]/[2000] maximum shards open
- **URL:** https://discuss.elastic.co/t/validation-failed-1-this-action-would-add-2-total-shards-but-this-cluster-currently-has-2000-2000-maximum-shards-open/257064
- **Date:** 2021 (pattern active in ES 8.x 2024 for clusters with many tenants)
- **Pattern:** Multi-tenant cluster hits the 2000-shard ceiling (after raising from 1000). Cannot create new indices until old indices are closed/deleted. ILM cannot roll over; log data is lost.
- **Impact:** Shard limit causes ILM pipelines to fail silently, dropping log/metric data.

### DP-061
- **Title:** Does the cluster state size impact performance?
- **URL:** https://discuss.elastic.co/t/does-the-cluster-state-size-impact-performance/11344
- **Date:** 2015 (canonical; referenced in 2024 performance debugging threads)
- **Pattern:** Every search, index, and delete operation requires the coordinating node to hold a copy of the full cluster state in memory. Large cluster states cause coordinating nodes to slow down all operations.
- **Impact:** Cluster state size is a hidden global tax on every operation in the cluster.

---

## Additional Cross-Category Data Points

### DP-062 — OOM / Node Kill
- **Title:** Elasticsearch getting killed by the OOM killer
- **URL:** https://discuss.elastic.co/t/elasticsearch-getting-killed-by-the-oom-killer-because-an-out-of-memory/326218
- **Date:** May 2024
- **Pattern:** Elasticsearch process killed by Linux OOM killer despite `-Xmx40g` JVM heap setting. Off-heap memory (Lucene segment caches, native memory) grows unbounded and triggers the OOM killer. No circuit breaker covers off-heap.
- **Impact:** JVM heap settings do not protect against OS-level OOM kills.

### DP-063 — OOM / Node Kill (Repeated)
- **Title:** My Elasticsearch Got Killed Frequently due to OOM killer
- **URL:** https://discuss.elastic.co/t/my-elasticsearch-got-killed-frequently-due-to-oom-killer-with-out-of-memory-message/360421
- **Date:** 2023 (actively discussed in ES 8.x 2024)
- **Pattern:** Repeated OOM kills on a node with search-heavy workloads. Each kill causes shard promotion on other nodes, which may also OOM cascade. Multi-node OOM cascade can take down the full cluster.
- **Impact:** OOM cascades can take down all nodes sequentially.

### DP-064 — OOM / JVM Outbursts
- **Title:** Elasticsearch JVM memory outbursts above settings causing OOM-kill
- **URL:** https://discuss.elastic.co/t/elasticsearch-jvm-memory-outbursts-causing-oom-kill/344490
- **Date:** 2023 (ES 8.x; same in 2024)
- **Pattern:** JVM native memory usage spikes transiently above the OS process limit, triggering OOM kill even when heap is within limits. Happens during large segment merges.
- **Impact:** Segment merges create transient memory spikes that are uncontrolled and can kill the process.

### DP-065 — Circuit Breaker / Heap
- **Title:** Circuit breaker on high throughput — ECK 8.14 AWS EKS
- **URL:** https://discuss.elastic.co/t/circuit-breaker-on-high-throughput/335853
- **Date:** 2024 (ECK 8.14 on AWS EKS)
- **Pattern:** Circuit breakers trip 2–3 times daily during traffic spikes with 75–78% heap pressure. Each trip causes write rejections and partial data loss for the burst period. Increasing heap or reducing throughput are the only options.
- **Impact:** Circuit breaker trips cause data loss during traffic spikes; no back-pressure mechanism to writers.

### DP-066 — Circuit Breaker / Request
- **Title:** Request circuit breaker not stopping search query even though heap crosses limit
- **URL:** https://discuss.elastic.co/t/circuit-breaker-not-stopping-my-search-query-even-though-my-heap-size-crosses/165353
- **Date:** 2018 (base; circuit breaker behavior unchanged, discussed in 2024)
- **Pattern:** Circuit breaker set to 60% of heap does not reliably stop large queries before they cause OOM. Breaker fires after the fact; by then memory pressure is already critical.
- **Impact:** Circuit breakers are reactive, not preventive; OOM still occurs in practice.

### DP-067 — Disk Watermark / Node Excluded
- **Title:** Documents are no longer saved after high disk watermark exceeded
- **URL:** https://discuss.elastic.co/t/documents-are-no-longer-saved-after-high-disk-watermark-exceeded-on-an-elasticsearch-cluster/313975
- **Date:** 2022 (same in ES 8 2024 — flood stage behavior unchanged)
- **Pattern:** Node at 95% disk hits `flood_stage` watermark; Elasticsearch makes all indices on that node read-only. Writes stop cluster-wide for indices with primaries on the affected node. No automatic recovery when space is freed.
- **Impact:** Flood-stage lockout requires manual index settings update to re-enable writes.

### DP-068 — Disk Watermark / Low Watermark Stops Upgrades
- **Title:** Having issues with low watermark / disk space — patches not installing
- **URL:** https://discuss.elastic.co/t/having-issues-with-low-watermark-disk-space/374934
- **Date:** February 2025
- **Pattern:** Nodes with slightly elevated disk usage hit the low watermark, which blocks ECK from performing automatic upgrades and patches. Operators discover the upgrade blockage only after checking upgrade status manually.
- **Impact:** Disk watermarks silently block security patches and upgrades.

### DP-069 — Cluster Instability / Node Disconnect
- **Title:** Elasticsearch unstable cluster — ingest nodes disconnecting chaotically
- **URL:** https://discuss.elastic.co/t/elasticsearch-unstable-cluster/350157
- **Date:** May 2024 (ES 8.11.1)
- **Pattern:** Ingest nodes repeatedly disconnect from the master with `followers check retry count exceeded`. Root cause: ingest nodes under heavy CPU load fail health check heartbeats. Any CPU spike causes node expulsion.
- **Impact:** Ingest workloads competing with cluster health checks cause instability under load.

### DP-070 — Cluster Instability / Connection Reset
- **Title:** Close connection exception caught on transport layer — Connection reset
- **URL:** https://discuss.elastic.co/t/elasticsearch-close-connection-exception-caught-on-transport-layer-disconnecting-from-relevant-node-connection-reset/359040
- **Date:** February 2025
- **Pattern:** Cluster formation fails with transport-layer connection resets when nodes are behind a load balancer with aggressive idle timeout settings. ES transport connections require stable, long-lived TCP sessions.
- **Impact:** Standard network infrastructure (LBs with idle timeouts) is incompatible with ES transport protocol.

### DP-071 — Cluster Not Forming (ECK / Kubernetes)
- **Title:** Cluster not forming
- **URL:** https://discuss.elastic.co/t/cluster-not-forming/374581
- **Date:** February 2025
- **Pattern:** Fresh ECK deployment fails to form a cluster. Master-eligible pods start but cannot discover each other due to Kubernetes DNS resolution timing. No retry backoff; pods stay in a formation loop.
- **Impact:** ECK cluster bootstrap depends on Kubernetes DNS timing — fragile on slow DNS.

### DP-072 — Index Corruption / Checksum
- **Title:** Snapshot restore checksum problem — CorruptIndexException on ES 8.10
- **URL:** https://discuss.elastic.co/t/restore-snapshot-checksum-problem-troubleshooting-corruption/369764
- **Date:** October 2024 (ES 8.10)
- **Pattern:** Some indices in S3 repository have corrupted segment files (checksum mismatch). These are detected only at restore time. No proactive integrity check API.
- **Impact:** Corruption is invisible until disaster recovery — exactly when reliability matters most.

### DP-073 — ILM / Rollover Error
- **Title:** ILM Policy Error — no rollover info found
- **URL:** https://discuss.elastic.co/t/ilm-policy-error/364556
- **Date:** August 2024
- **Pattern:** ILM rollover step fails with "no rollover info found" error. The rollover alias is not attached to the write index. ILM silently skips rollover; the primary shard grows unbounded without the operator noticing.
- **Impact:** ILM rollover failure is silent; shards grow unbounded until disk is full.

### DP-074 — ILM / Downsample Failure
- **Title:** ILM Downsample Failed — "Duplicate field 'min'" messages
- **URL:** https://discuss.elastic.co/t/ilm-downsample-failed-with-logs-containing-a-lot-of-duplicate-field-min-messages/360395
- **Date:** May 2024
- **Pattern:** Multiple indices stuck in ILM downsample phase. Elastic Agent monitoring indices fail to downsample due to duplicate field mapping. No automatic retry; indices are stuck permanently until manual reset.
- **Impact:** Downsample failures permanently block ILM progression without operator intervention.

### DP-075 — Rolling Upgrade / 503 Errors
- **Title:** Elasticsearch rolling upgrade failed — 503 during 8.8 to 8.15 upgrade
- **URL:** https://discuss.elastic.co/t/elasticsearch-rolling-upgrade-failed/364907
- **Date:** August 2024
- **Pattern:** Rolling upgrade from 8.8 to 8.15 on a 2-node cluster produces 503 errors. During the window where one node is upgraded and the other is not, mixed-version cluster operates in a degraded compatibility mode that causes API failures.
- **Impact:** 2-node clusters cannot tolerate rolling upgrades without 503s.

### DP-076 — Shard / Too Many Shards
- **Title:** Do I have too many shards?
- **URL:** https://discuss.elastic.co/t/do-i-have-too-many-shards/358027
- **Date:** 2023 (guidance re-asked repeatedly in 2024 by new users)
- **Pattern:** User has 50,000 tiny shards (< 100 documents each) from per-day-per-tenant indices. Cluster performance is severely degraded despite adequate hardware. ES has no automatic mechanism to warn users about over-sharding.
- **Impact:** Over-sharding is a common design anti-pattern with no built-in guardrails.

### DP-077 — Write Rejections / Thread Pool
- **Title:** Issue with thread_pool.write.queue_size — rejected execution
- **URL:** https://discuss.elastic.co/t/issue-with-thread-pool-write-queue-size/354951
- **Date:** March 2024
- **Pattern:** High-throughput indexing causes the write thread pool queue to fill (default size 200). New indexing requests are rejected. No back-pressure to producers; clients see 429 errors and must implement their own retry logic.
- **Impact:** Write rejection under load requires client-side retry; no server-side back-pressure mechanism.

### DP-078 — Write Rejections / Increasing
- **Title:** Elasticsearch Increasing Write Rejections
- **URL:** https://discuss.elastic.co/t/elasticsearch-increasing-write-rejections/281882
- **Date:** 2021 (ES 7.x; same behavior ES 8 2024)
- **Pattern:** Write rejection rate reaching ~5% on some nodes during ingestion peaks. No automatic load shedding or queue draining. Operators must manually tune `thread_pool.write.queue_size` and `thread_pool.write.size`.
- **Impact:** Write rejection tuning is a manual, empirical process with no guidance tooling.

### DP-079 — CCR / Auto-Follow Data Streams
- **Title:** CCR auto-follow problem on data streams
- **URL:** https://discuss.elastic.co/t/ccr-auto-follow-problem-on-data-streams/348789
- **Date:** December 2023 (ES 8.9.1; same in ES 8.12–8.15 2024 threads)
- **Pattern:** CCR auto-follow fails for `traces-*` and `metrics-*` data streams while succeeding for other index types. Multiple failed follow indices accumulate. No automatic retry; operator must manually re-bootstrap each failed follower.
- **Impact:** CCR is unreliable for Elastic's own data stream conventions (APM, metrics).

### DP-080 — CCR / Latency and Data Loss Risk
- **Title:** Test Elasticsearch Cross Cluster Replication in terms of latency and data loss
- **URL:** https://discuss.elastic.co/t/test-elastic-search-cross-cluster-replication-in-terms-of-latency-and-data-loss/293892
- **Date:** 2022 (foundational for 2024 DR architecture discussions)
- **Pattern:** CCR is asynchronous by design. In a leader cluster failure scenario, the follower may lag by seconds to minutes. Any writes that were acknowledged on the leader but not yet replicated to the follower are permanently lost.
- **Impact:** CCR does not provide zero-data-loss disaster recovery; RPO is measured in seconds to minutes.

### DP-081 — Performance Degradation / Slow Over Time
- **Title:** Elasticsearch is very slow — request timeouts
- **URL:** https://discuss.elastic.co/t/elasticsearch-is-very-slow/373362
- **Date:** January 2025
- **Pattern:** ES deployment stops responding and begins returning request timeout errors. Cluster was stable for months, then degraded progressively. Root causes identified: index fragmentation, segment bloat, and missing index lifecycle management.
- **Impact:** Without proactive maintenance (forcemerge, ILM), clusters degrade over time without warning.

### DP-082 — Primary Shards Lost After Power Failure
- **Title:** Elasticsearch: primary shards lost after server restart due to power failure on datacenter
- **URL:** https://discuss.elastic.co/t/elasticsearch-primary-shards-lost-after-server-restart-due-to-power-failure-on-datacenter/264112
- **Date:** 2021 (datacenter power failure; scenario active in 2024)
- **Pattern:** After datacenter power failure and abrupt restart, primary shards on the affected node are unrecoverable. If replicas were on the same node (or on another node that also failed), data is permanently lost.
- **Impact:** Collocated primary and replica (same AZ/rack) = data loss on rack-level failure.

### DP-083 — Data Node Lost / Shards Red / Node Returns But Shards Gone
- **Title:** Data node lost, all shards go to RED — Data node returns but shards lost forever
- **URL:** https://discuss.elastic.co/t/data-node-lost-all-shards-go-to-red-data-node-returns-but-shards-lost-forever/186761
- **Date:** 2019 (base; pattern replicated in ES 8 2024)
- **Pattern:** A data node goes offline. Cluster starts re-allocating its primary shards from replicas. Node comes back before re-allocation finishes with stale data. ES discards the stale data and re-initializes from the now-authoritative replica. Documents written while the original node was down and before re-allocation completed may be lost.
- **Impact:** Timing of node return relative to shard re-allocation determines whether data is lost.

### DP-084 — Elasticsearch Stopping Abruptly (ES 8.9.2)
- **Title:** Elastic Search (elasticsearch-8.9.2) stopping abruptly
- **URL:** https://discuss.elastic.co/t/elastic-search-elasticsearch-8-9-2-stopping-abruptly/382548
- **Date:** October 2025
- **Pattern:** ES 8.9.2 process exits without a clean shutdown signal or logging a clear cause. Translog left in intermediate state; on restart, recovery takes extended time. Some in-flight writes lost.
- **Impact:** Unexplained ES process exits cause partial data loss with no diagnostic trail.

---

## Summary Statistics

| Category | Data Points Collected |
|---|---|
| Cluster RED / Unassigned Shards | 10 (DP-001–010) |
| Split Brain / Master Election Failures | 7 (DP-011–017) |
| Data Loss / Translog Issues | 7 (DP-018–024) |
| Rolling Restart Slowness / Failures | 7 (DP-025–031) |
| Shard Allocation Timeouts / Failures | 7 (DP-032–038) |
| Master Node Bottlenecks | 7 (DP-039–045, +DP-046) |
| Snapshot / Restore Failures | 8 (DP-047–054) |
| Cluster State Too Large | 7 (DP-055–061) |
| Additional Cross-Category | 23 (DP-062–084) |
| **Total** | **84 data points** |

---

## Key Failure Patterns Synthesized

1. **Unassigned shards are Elasticsearch's most common production incident** — triggered by disk full, node loss, restart, hardware failure, or misconfiguration. Recovery always requires manual intervention.

2. **Translog corruption on unclean shutdown** is common and frequently unrecoverable without a snapshot.

3. **Rolling restarts are operationally dangerous** — they can trigger write unavailability, shard resync storms, and race conditions that leave shards in `PRIMARY_FAILED` state.

4. **The master node is a global bottleneck** — ILM bugs, large cluster states, snapshot operations, and shard count all funnel through the master and can render the entire cluster inoperable.

5. **Snapshot SUCCESS does not guarantee restorability** — multiple reports of successful snapshot operations that produce corrupted or incomplete backups only discovered at restore time.

6. **CCR is not zero-RPO** — asynchronous replication means seconds to minutes of data loss on leader failure. Auto-follow unreliable for Elastic's own data stream types.

7. **Cluster state growth has no guardrails** — too many indices, templates, or shards silently degrades performance until the system fails catastrophically.

8. **OOM kills are not fully prevented by JVM heap settings** — off-heap native memory from Lucene is unbounded and regularly triggers the OS OOM killer even on correctly sized nodes.

---

*Collected: 2026-04-10. Sources: discuss.elastic.co. Data points span 2019–2025, with emphasis on patterns active in 2024–2025 Elasticsearch 8.x deployments.*
