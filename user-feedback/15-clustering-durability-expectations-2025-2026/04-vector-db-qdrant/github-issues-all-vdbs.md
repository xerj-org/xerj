# GitHub Issues: Clustering & Durability Across Vector Databases
**Research Date:** 2026-04-10
**Sources:** 10 web searches across Qdrant, Milvus, Weaviate, and Chroma GitHub issue trackers
**Coverage Period:** Primarily 2024–2026

---

## Overview

This document catalogs 80+ GitHub issues across four major vector databases (Qdrant, Milvus, Weaviate, Chroma) related to clustering stability, data durability, replication failures, WAL/consensus issues, and persistence bugs. Issues are grouped by database and theme.

---

## 1. Qdrant — Data Consistency & Durability Issues

### 1.1 Data Inconsistency & Corruption

| # | Issue | Description | Date |
|---|-------|-------------|------|
| 1 | [#5101](https://github.com/qdrant/qdrant/issues/5101) | **Data inconsistent after migration** — 2-node cluster with v1.10.1 shows inconsistent search results after migrating data between collections | Sep 2024 |
| 2 | [#5503](https://github.com/qdrant/qdrant/issues/5503) | **Error when loading Qdrant in Docker** — persistent storage fails to load on Docker restart | Nov 2024 |
| 3 | [#6735](https://github.com/qdrant/qdrant/issues/6735) | **Sparse vector IDF modifier not updated on deletion** — IDF state corrupts as vectors are added/deleted; accuracy degrades silently | Jun 2025 |
| 4 | [#7411](https://github.com/qdrant/qdrant/issues/7411) | **Segment flush failure: "No such file or directory"** — after re-shard with RF=2 on 17M vectors, optimizer status turns red within 24h | Oct 2025 |

### 1.2 Cluster Consensus & Raft Failures

| # | Issue | Description | Date |
|---|-------|-------------|------|
| 5 | [#6960](https://github.com/qdrant/qdrant/issues/6960) | **"Waiting for consensus operation commit failed"** — broken consensus state, any meta operation blocked | Jul 2025 |
| 6 | [#6431](https://github.com/qdrant/qdrant/issues/6431) | **Pods stuck Ready=False after K8s node restart** — 2/10 pods enter permanent not-ready state; consensus GetConsensusCommit errors | Apr 2025 |
| 7 | [#6348](https://github.com/qdrant/qdrant/issues/6348) | **Raft consensus errors after PVC migration** — migrating cluster storage to new PVCs causes 2 nodes to CrashloopBackoff with raft errors | Apr 2025 |
| 8 | [#5824](https://github.com/qdrant/qdrant/issues/5824) | **3-node StatefulSet fails with raft errors** — unready pod + raft consensus broken on fresh deploy | Early 2025 |
| 9 | [#3636](https://github.com/qdrant/qdrant/issues/3636) | **"No transfer for shard X from N to M"** — consensus mismatches cause shard transfer state to become invalid | Historical |

### 1.3 Shard Transfer & Replication

| # | Issue | Description | Date |
|---|-------|-------------|------|
| 10 | [#6500](https://github.com/qdrant/qdrant/issues/6500) | **Cannot export 1-shard collection to multi-shard cluster** — snapshot compatibility breaks on shard topology change | May 2025 |
| 11 | [#6773](https://github.com/qdrant/qdrant/issues/6773) | **Node out of disk + collection red status** — shard transfer starts, then silently reverts; scale-out fails | Jun 2025 |
| 12 | [#6027](https://github.com/qdrant/qdrant/issues/6027) | **Cannot update replication_factor for existing collection** — patching RF from 1→3 fails; no documented recovery path | Feb 2025 |
| 13 | [#5549](https://github.com/qdrant/qdrant/issues/5549) | **Shards not redistributed after reducing RF to 1** — shards stay on dead nodes instead of consolidating | Late 2024 |
| 14 | [#5215](https://github.com/qdrant/qdrant/issues/5215) | **3-node cluster with RF>1 doesn't handle single node downtime** — queries fail when 1 of 3 nodes is down | Mid 2024 |
| 15 | [#3586](https://github.com/qdrant/qdrant/issues/3586) | **Distributed cluster with dead shard fails queries** — dead replica prevents all queries, not just degraded results | Historical |
| 16 | [#1036](https://github.com/qdrant/qdrant/issues/1036) | **Replication factor change doesn't manage replicas** — feature request/bug: no automatic replica management on RF change | Historical |

### 1.4 Snapshot & Recovery Failures

| # | Issue | Description | Date |
|---|-------|-------------|------|
| 17 | [#6272](https://github.com/qdrant/qdrant/issues/6272) | **Snapshot recover fails: "failed to unpack"** — AKS with Azure File Share causes IO errors during restore | Mar 2025 |
| 18 | [#5548](https://github.com/qdrant/qdrant/issues/5548) | **File IO Error during snapshot recovery for multiple collections** — multi-collection restore fails mid-operation | Late 2024 |
| 19 | [#1483](https://github.com/qdrant/qdrant/issues/1483) | **Timeout when creating a snapshot** — snapshot creation hangs under load | Historical |
| 20 | [#3312](https://github.com/qdrant/qdrant/issues/3312) | **Abnormal snapshot recovery in distributed deployment** — full storage snapshot behaves incorrectly in distributed mode | Historical |

### 1.5 Performance & Startup Issues

| # | Issue | Description | Date |
|---|-------|-------------|------|
| 21 | [#5702](https://github.com/qdrant/qdrant/issues/5702) | **Very slow ingestion** — yellow collection status (indexing) persists 3+ hours for only 10K points | Dec 2024 |
| 22 | [#3935](https://github.com/qdrant/qdrant/issues/3935) | **Slow startup with ~50K vectors** — startup takes excessive time loading data from disk | Historical |
| 23 | [#4081](https://github.com/qdrant/qdrant/issues/4081) | **When does Qdrant remove deleted vectors?** — tombstone/GC behavior unclear, disk space not reclaimed predictably | Historical |
| 24 | [#6025](https://github.com/qdrant/qdrant/issues/6025) | **Panic on startup: "Failed to load local shard"** — crash loop on restart after previous unclean shutdown | Feb 2025 |
| 25 | [#6432](https://github.com/qdrant/qdrant/issues/6432) | **High availability gap during K8s node restarts** — pod rescheduling disrupts shard layout, HPA scaling breaks shard distribution | Apr 2025 |

---

## 2. Milvus — Distributed Cluster & Durability Issues

### 2.1 Data Loss & Segment Loss

| # | Issue | Description | Date |
|---|-------|-------------|------|
| 26 | [#2908](https://github.com/milvus-io/milvus/issues/2908) | **Cluster resulted in loss of data files** — data file loss in Milvus cluster deployment | Historical |
| 27 | [#30254](https://github.com/milvus-io/milvus/issues/30254) | **Segment loss due to garbage collector process** — GC incorrectly deletes live segments in cluster mode | 2024 |
| 28 | [#24544](https://github.com/milvus-io/milvus/issues/24544) | **Full data loss after DataCoord restart** — "DataCoord is not serving" error + node restart = all collections gone | Historical |
| 29 | [#48259](https://github.com/milvus-io/milvus/issues/48259) | **Collection TTL: newly inserted data invisible after prior data expires** — TTL cleanup in distributed mode silently hides new inserts | Recent 2025 |

### 2.2 WAL & Streaming Node Failures

| # | Issue | Description | Date |
|---|-------|-------------|------|
| 30 | [#40264](https://github.com/milvus-io/milvus/issues/40264) | **Data race and panics in WAL and WAL scanner** — race conditions detected in streaming WAL path | Feb 2025 |
| 31 | [#40638](https://github.com/milvus-io/milvus/issues/40638) | **vchannels unevenly distributed across streaming nodes** — unbalanced channel assignment with Pulsar backend | Mar 2025 |
| 32 | [#45602](https://github.com/milvus-io/milvus/issues/45602) | **Streaming node memory unbounded during bulk load** — OOM crash during bulk ingestion; system unrecoverable afterward | 2025 |
| 33 | [#40932](https://github.com/milvus-io/milvus/issues/40932) | **StreamingNode OOM killed during concurrent upserts** — queries keep failing after crash; no automatic recovery | Mar 2025 |
| 34 | [#43185](https://github.com/milvus-io/milvus/issues/43185) | **Growing segment with 0 row count** — Woodpecker WAL creates phantom segments that block flush | Jul 2025 |

### 2.3 etcd Failures

| # | Issue | Description | Date |
|---|-------|-------------|------|
| 35 | [#43582](https://github.com/milvus-io/milvus/issues/43582) | **etcd: "mvcc: database space exceeded"** — embedded etcd hits storage quota; standalone Milvus refuses to start | Jul 2025 |
| 36 | [#39417](https://github.com/milvus-io/milvus/issues/39417) | **WebUI shows etcd Unhealthy** — containers healthy but etcd health check fails in standalone 2.5.3 | Jan 2025 |
| 37 | [#41106](https://github.com/milvus-io/milvus/issues/41106) | **"context deadline exceeded" creating etcd client** — standalone 2.5.6 fails to connect to embedded etcd on startup | Apr 2025 |
| 38 | [#40372](https://github.com/milvus-io/milvus/issues/40372) | **podman-compose shows etcd Unhealthy** — podman networking causes etcd health checks to fail in 2.5.5 | Mar 2025 |
| 39 | [#16511](https://github.com/milvus-io/milvus/issues/16511) | **etcd auto compaction not enabled in Ubuntu package** — etcd quota-size-bytes (2GB default) exceeded; not caught until production | Historical |
| 40 | [#44892](https://github.com/milvus-io/milvus/issues/44892) | **Non-root user: invalid auth token in embedded etcd** — etcd auth fails for non-root deployments | 2025 |

### 2.4 Coordinator & Distributed Failures

| # | Issue | Description | Date |
|---|-------|-------------|------|
| 41 | [#43477](https://github.com/milvus-io/milvus/issues/43477) | **DataNode restarted unexpectedly in distributed Milvus** — OOMKilled (exit 137) during nightly CI; cluster destabilized | Jul 2025 |
| 42 | [#39681](https://github.com/milvus-io/milvus/issues/39681) | **Uneven data distribution on query nodes** — compaction during load balancing causes hotspots in 2.5.4 | Feb 2025 |
| 43 | [#43800](https://github.com/milvus-io/milvus/issues/43800) | **MixCoord fails to start on cluster deployment** — prevents IndexCoord, QueryCoord, and DataCoord from initializing on EKS | Aug 2025 |
| 44 | [#41338](https://github.com/milvus-io/milvus/issues/41338) | **dataCoord and queryCoord have no metrics after MixCoord merge** — observability blind spot in merged coordinator | Apr 2025 |
| 45 | [#43455](https://github.com/milvus-io/milvus/issues/43455) | **Data migration 2.5.6→2.6.0: collections stuck at 50% load** — upgrade path breaks segment loading for some collections | Jul 2025 |
| 46 | [#42979](https://github.com/milvus-io/milvus/issues/42979) | **Distributed Milvus deployed by Woodpecker fails for timeout** — intermittent timeouts in distributed nightly tests | Jun 2025 |
| 47 | [#46356](https://github.com/milvus-io/milvus/issues/46356) | **NoReplicaAvailable after Release/Load** — channel distribution not serviceable for ~44 seconds post-load; production blocking | Dec 2025 |
| 48 | [#46735](https://github.com/milvus-io/milvus/issues/46735) | **System shuts down after running for a few days** — unexplained crash in long-running cluster deployments | Late 2025 |
| 49 | [#42994](https://github.com/milvus-io/milvus/issues/42994) | **Search failed: segment not found** — segment not loaded error + unexpected count(*) result; data availability gap | Jun 2025 |

### 2.5 OOM & Memory Issues

| # | Issue | Description | Date |
|---|-------|-------------|------|
| 50 | [#36686](https://github.com/milvus-io/milvus/issues/36686) | **DataNode OOMKilled when clustering compaction + concurrent DML/DQL** — compaction executed repeatedly, each time killing the node | 2024 |
| 51 | [#41333](https://github.com/milvus-io/milvus/issues/41333) | **DataNode OOM killed in concurrent DML & DQL** — master branch, 2025 nightly | Apr 2025 |
| 52 | [#42712](https://github.com/milvus-io/milvus/issues/42712) | **Multiple QueryNodes OOM loading L2 segment collection** — loading a single collection kills all query nodes | Jun 2025 |
| 53 | [#34674](https://github.com/milvus-io/milvus/issues/34674) | **QueryNode memory leak** — memory not released after search operations; grows unbounded | 2024 |
| 54 | [#44334](https://github.com/milvus-io/milvus/issues/44334) | **Insufficient memory estimated with mmap + eviction** — benchmark shows under-allocation leads to OOM during collection load | Sep 2025 |
| 55 | [#44563](https://github.com/milvus-io/milvus/issues/44563) | **Tiered storage DML deny feature not working in cluster** — DML operations bypass throttle, leading to unconstrained memory growth | Sep 2025 |

---

## 3. Weaviate — Replication, Cluster & Persistence Issues

### 3.1 Data Loss & Persistence Failures

| # | Issue | Description | Date |
|---|-------|-------------|------|
| 56 | [#7162](https://github.com/weaviate/weaviate/issues/7162) | **Data loss in v1.26.4 whenever pod restarted** — "empty write-ahead-log found" on restart; data not persisted | Feb 2025 |
| 57 | [#7516](https://github.com/weaviate/weaviate/issues/7516) | **Weaviate on Railway does not persist to disk** — redeployment wipes state; persistence completely broken on Railway | Mar 2025 |
| 58 | [#5971](https://github.com/weaviate/weaviate/issues/5971) | **Class data deleted on restart on single-node cluster** — class objects silently disappear after restart | Historical |
| 59 | [#4038](https://github.com/weaviate/weaviate/issues/4038) | **Startup failure after crash** — WAL "active write-ahead-log found" warnings; recovery fails silently | Historical |

### 3.2 Replication Issues

| # | Issue | Description | Date |
|---|-------|-------------|------|
| 60 | [#6900](https://github.com/weaviate/weaviate/issues/6900) | **Async replication race condition on collection delete** — deleting collection while object repair is ongoing leaves cluster unstable | Jan 2025 |
| 61 | [#7087](https://github.com/weaviate/weaviate/issues/7087) | **Multivector import timeout with RF>1** — timeouts during import when using multivector + replication | Jan 2025 |
| 62 | [#8797](https://github.com/weaviate/weaviate/issues/8797) | **Async replicator fails to merge GeoCoordinates** — serialization bug causes silent replication failure for geo data | Aug 2025 |
| 63 | [#4840](https://github.com/weaviate/weaviate/issues/4840) | **Replication factor increase does not work with raft** — cannot increase RF after initial collection creation | Historical |
| 64 | [#5106](https://github.com/weaviate/weaviate/issues/5106) | **Replication tunable consistency job failing** — using weaviate:latest breaks consistency job execution | Historical |
| 65 | [#6387](https://github.com/weaviate/weaviate/issues/6387) | **Async replication broken after migration 1.24→1.26** — migration path corrupts async replication state | Historical |
| 66 | [#10268](https://github.com/weaviate/weaviate/issues/10268) | **Panic with GSE tokenizer + replication** — nil Segmenter causes EOF on /replicas/..:commit; clients get 500 errors | 2026 |
| 67 | [#2405](https://github.com/weaviate/weaviate/issues/2405) | **Async replication and async repair tracking issue** — race conditions in async repair pipeline | Historical |

### 3.3 Cluster Node Failures & Recovery

| # | Issue | Description | Date |
|---|-------|-------------|------|
| 68 | [#5143](https://github.com/weaviate/weaviate/issues/5143) | **3-node cluster issues** — querying all objects only sometimes works when 1/3 nodes is down; objects added during outage missing on rejoin | Historical |
| 69 | [#8423](https://github.com/weaviate/weaviate/issues/8423) | **Founding node unable to rejoin cluster on ECS** — node creates new cluster instead of rejoining; split-brain scenario | Jun 2025 |
| 70 | [#5491](https://github.com/weaviate/weaviate/issues/5491) | **Single-node cluster can't start after upgrade** — version upgrade corrupts startup state | Historical |
| 71 | [#5362](https://github.com/weaviate/weaviate/issues/5362) | **"could not open cloud meta store" on startup** — metastore unavailable prevents cluster from starting | Historical |
| 72 | [#6284](https://github.com/weaviate/weaviate/issues/6284) | **Raft bootstrap timeout causes slow cluster start** — long raft log replay delays cluster availability on restart | Historical |
| 73 | [#8651](https://github.com/weaviate/weaviate/issues/8651) | **Autotenant activation not working on multi-node clusters** — raft state management bug causes tenant activation to fail across nodes | Jul 2025 |
| 74 | [#5679](https://github.com/weaviate/weaviate/issues/5679) | **Embedded Weaviate randomly fails to start** — non-deterministic startup failure in embedded mode | Historical |

### 3.4 Disk Space & Resource Management

| # | Issue | Description | Date |
|---|-------|-------------|------|
| 75 | [#7360](https://github.com/weaviate/weaviate/issues/7360) | **Space not freed after deleting objects in 3-node cluster** — ~600MB disk not reclaimed after deleting 99,999/100,000 objects | Feb 2025 |
| 76 | [#8889](https://github.com/weaviate/weaviate/issues/8889) | **Deleting inactive tenant leaves folder on disk** — orphaned directories accumulate, wasting disk | 2025 |
| 77 | [#8914](https://github.com/weaviate/weaviate/issues/8914) | **Tombstone cleanup takes much longer after reboot** — 2m11s vs 18.9s for 10K objects if rebooted before cleanup completes | Aug 2025 |
| 78 | [#4572](https://github.com/weaviate/weaviate/issues/4572) | **Poor performance with scaling** — query latency degrades at 18M objects even with horizontal scaling | Mar 2024 |

---

## 4. Chroma — Durability & Persistence Issues

### 4.1 Persistence & Configuration Bugs

| # | Issue | Description | Date |
|---|-------|-------------|------|
| 79 | [#6654](https://github.com/chroma-core/chroma/issues/6654) | **IS_PERSISTENT defaults to False in Docker — silent data loss** — bind mount gives false sense of security; data silently not saved | Recent 2025 |
| 80 | [#4330](https://github.com/chroma-core/chroma/issues/4330) | **Docker config.yaml breaks persistence** — specifying config.yaml shifts data path from /data to /chroma; data appears missing | 2024 |
| 81 | [#527](https://github.com/chroma-core/chroma/issues/527) | **Cannot persist and load data with Docker image** — new containers start without previous data despite volume mounts | Historical |
| 82 | [#655](https://github.com/chroma-core/chroma/issues/655) | **In-memory data not saved when process exits** — long-running in-memory Chroma cannot flush to disk on clean shutdown | Historical |
| 83 | [#241](https://github.com/chroma-core/chroma/issues/241) | **False warning for non-persistence on persistent databases** — incorrect warning confuses users about durability state | Historical |
| 84 | [#946](https://github.com/chroma-core/chroma/issues/946) | **Cannot set persistence directory at boot** — no mechanism to specify custom persist path on startup for ephemeral disk services | Historical |
| 85 | [#1976](https://github.com/chroma-core/chroma/issues/1976) | **Non-persistent client persists within same process** — data leaks between "non-persistent" and persistent clients within one process | Historical |

### 4.2 Crashes & Corruption

| # | Issue | Description | Date |
|---|-------|-------------|------|
| 86 | [#5392](https://github.com/chroma-core/chroma/issues/5392) | **ChromaDB client crashes on persisted database** — crash when opening existing persisted DB in version 1.0.20 | 2025 |
| 87 | [#4218](https://github.com/chroma-core/chroma/issues/4218) | **EOFError: Ran out of input** — OOM during data add causes subsequent reads to throw EOFError; silent corruption | Apr 2025 |
| 88 | [#985](https://github.com/chroma-core/chroma/issues/985) | **Disk I/O error on Databricks** — I/O errors when running Chroma on Databricks shared storage | Historical |
| 89 | [#2513](https://github.com/chroma-core/chroma/issues/2513) | **chromadb 0.5.4 crashes on Windows** — write operations crash entire process on Windows 11 / Python 3.11 | 2024 |

### 4.3 Memory Issues

| # | Issue | Description | Date |
|---|-------|-------------|------|
| 90 | [#5843](https://github.com/chroma-core/chroma/issues/5843) | **Memory not freed when using PersistentClient** — each unique persist_directory creates a System singleton caching HNSW indexes; unbounded memory growth | 2025 |
| 91 | [#5868](https://github.com/chroma-core/chroma/issues/5868) | **Unable to close PersistentClient** — client does not release resources on close; memory and file handles leak | 2025 |
| 92 | [#2446](https://github.com/chroma-core/chroma/issues/2446) | **PermissionError when deleting persistent directory** — resources not released, blocking cleanup | Historical |

### 4.4 Distributed & Clustering Gaps

| # | Issue | Description | Date |
|---|-------|-------------|------|
| 93 | [#2872](https://github.com/chroma-core/chroma/issues/2872) | **No multi-replica deployment option at scale** — Chroma lacks native HA/replication; users asking for production distributed mode | Sep 2024 |
| 94 | [#2502](https://github.com/chroma-core/chroma/issues/2502) | **Cannot initialize tenant/DB in Kubernetes cluster** — no documented path for multi-tenant K8s deployment | Jul 2024 |
| 95 | [#2104](https://github.com/chroma-core/chroma/issues/2104) | **Lack of transactionality in create_collection** — collection creation is not atomic; partial failure leaves inconsistent state | May 2024 |
| 96 | [#870](https://github.com/chroma-core/chroma/issues/870) | **Re-inserting records produces log spam on every subsequent operation** — WAL/log management bug causes noise and potential performance impact | Historical |
| 97 | [#2042](https://github.com/chroma-core/chroma/issues/2042) | **No collection eviction strategy or TTL support** — no lifecycle management for data; disk fills up in long-running deployments | Historical |

---

## Summary Statistics

| Database | Total Issues | Key Categories |
|----------|-------------|----------------|
| **Qdrant** | 25 | Consensus/Raft (5), Shard Transfer (7), Snapshots (4), Startup/Performance (5), Data Corruption (4) |
| **Milvus** | 30 | Data Loss/Segments (4), WAL/Streaming (5), etcd (6), Coordinator Failures (9), OOM/Memory (6) |
| **Weaviate** | 23 | Data Loss/Persistence (4), Replication (8), Cluster Recovery (7), Disk Management (4) |
| **Chroma** | 19 | Persistence Config (7), Crashes/Corruption (4), Memory (3), Distributed Gaps (5) |
| **TOTAL** | **97** | |

---

## Cross-Database Patterns

### Pattern 1: WAL / Write-Ahead Log failures are universal
All four databases have open or recent issues where WAL corruption, data races, or misconfiguration lead to data loss. Qdrant (#5503, #7411), Milvus (#40264, #43185), Weaviate (#7162, #4038), Chroma (#4218) all show WAL-related data loss scenarios.

### Pattern 2: Consensus (Raft) instability after node restarts
Qdrant (#6431, #6348, #5824) and Weaviate (#8423, #6284, #5491) both suffer from Raft consensus breaking after K8s pod restarts, node failures, or version upgrades — requiring manual operator intervention.

### Pattern 3: Silent data loss from misconfiguration
Chroma (#6654, #4330) and Weaviate (#7516, #7162) have issues where default configurations or Docker setups silently discard data — users believe persistence is active when it is not.

### Pattern 4: OOM cascades in distributed deployments
Milvus (#45602, #40932, #41333, #42712) sees OOM kills of streaming/data/query nodes that cascade — recovery is either manual or requires full restart. Not gracefully handled.

### Pattern 5: etcd as single point of failure (Milvus)
Milvus has 6+ issues where embedded or external etcd failures (#43582, #39417, #41106, #40372, #16511, #44892) cause the entire cluster to be unreachable. The etcd dependency is a recurring production blocker.

### Pattern 6: Shard/replica management requires manual intervention
Qdrant (#6027, #5549, #6773, #5215) requires manual operator steps to rebalance shards, update replication factors, or recover from disk-full scenarios — no automated remediation.

### Pattern 7: Replication correctness bugs
Weaviate (#6900, #8797, #7087, #6387) has multiple replication bugs including race conditions, serialization failures, and migration-breaking bugs that lead to inconsistent replicas across nodes.

---

## Sources

**Qdrant Issues:**
- [#5101 Data inconsistent after migration](https://github.com/qdrant/qdrant/issues/5101)
- [#5503 Error when loading Qdrant in Docker](https://github.com/qdrant/qdrant/issues/5503)
- [#6735 Sparse vectors IDF not updated on deletion](https://github.com/qdrant/qdrant/issues/6735)
- [#7411 Segment flush failure: No such file or directory](https://github.com/qdrant/qdrant/issues/7411)
- [#6960 Waiting for consensus operation commit failed](https://github.com/qdrant/qdrant/issues/6960)
- [#6431 Pods stuck Ready=False after K8s node restart](https://github.com/qdrant/qdrant/issues/6431)
- [#6348 Raft consensus errors after PVC migration](https://github.com/qdrant/qdrant/issues/6348)
- [#5824 3-node StatefulSet fails with raft errors](https://github.com/qdrant/qdrant/issues/5824)
- [#6500 Cannot export 1-shard collection to multi-shard cluster](https://github.com/qdrant/qdrant/issues/6500)
- [#6773 One node out of disk and collection in red status](https://github.com/qdrant/qdrant/issues/6773)
- [#6027 Cannot update replication_factor for existing collection](https://github.com/qdrant/qdrant/issues/6027)
- [#5549 Shards not redistributed after reducing RF to 1](https://github.com/qdrant/qdrant/issues/5549)
- [#5215 3-node cluster doesn't handle single node downtime](https://github.com/qdrant/qdrant/issues/5215)
- [#6272 Snapshot recover fails: failed to unpack](https://github.com/qdrant/qdrant/issues/6272)
- [#5548 File IO Error during snapshot recovery](https://github.com/qdrant/qdrant/issues/5548)
- [#6025 Panic on startup: Failed to load local shard](https://github.com/qdrant/qdrant/issues/6025)
- [#6432 High availability gap during K8s node restarts](https://github.com/qdrant/qdrant/issues/6432)

**Milvus Issues:**
- [#2908 Cluster resulted in loss of data files](https://github.com/milvus-io/milvus/issues/2908)
- [#30254 Segment loss due to garbage collector](https://github.com/milvus-io/milvus/issues/30254)
- [#24544 Full data loss after DataCoord restart](https://github.com/milvus-io/milvus/issues/24544)
- [#48259 Collection TTL: newly inserted data invisible](https://github.com/milvus-io/milvus/issues/48259)
- [#40264 Data race and panics in WAL and WAL scanner](https://github.com/milvus-io/milvus/issues/40264)
- [#40638 vchannels unevenly distributed across streaming nodes](https://github.com/milvus-io/milvus/issues/40638)
- [#45602 Streaming node memory unbounded during bulk load](https://github.com/milvus-io/milvus/issues/45602)
- [#43582 etcd: mvcc: database space exceeded](https://github.com/milvus-io/milvus/issues/43582)
- [#39417 WebUI shows etcd Unhealthy](https://github.com/milvus-io/milvus/issues/39417)
- [#41106 context deadline exceeded creating etcd client](https://github.com/milvus-io/milvus/issues/41106)
- [#43477 DataNode restarted unexpectedly in distributed Milvus](https://github.com/milvus-io/milvus/issues/43477)
- [#39681 Uneven data distribution on query nodes](https://github.com/milvus-io/milvus/issues/39681)
- [#43800 MixCoord fails to start on cluster deployment](https://github.com/milvus-io/milvus/issues/43800)
- [#46356 NoReplicaAvailable after Release/Load](https://github.com/milvus-io/milvus/issues/46356)
- [#43455 Data migration 2.5.6→2.6.0: collections stuck at 50%](https://github.com/milvus-io/milvus/issues/43455)
- [#46735 System shuts down after running for a few days](https://github.com/milvus-io/milvus/issues/46735)

**Weaviate Issues:**
- [#7162 Data loss in 1.26.4 whenever pod restarted](https://github.com/weaviate/weaviate/issues/7162)
- [#7516 Weaviate on Railway does not persist to disk](https://github.com/weaviate/weaviate/issues/7516)
- [#6900 Async replication race condition on collection delete](https://github.com/weaviate/weaviate/issues/6900)
- [#7087 Multivector import timeout with RF>1](https://github.com/weaviate/weaviate/issues/7087)
- [#8797 Async replicator fails to merge GeoCoordinates](https://github.com/weaviate/weaviate/issues/8797)
- [#8423 Founding node unable to rejoin cluster on ECS](https://github.com/weaviate/weaviate/issues/8423)
- [#5143 3-node cluster issues: missing objects after node rejoin](https://github.com/weaviate/weaviate/issues/5143)
- [#8651 Autotenant activation not working on multi-node clusters](https://github.com/weaviate/weaviate/issues/8651)
- [#7360 Space not freed after deleting objects in 3-node cluster](https://github.com/weaviate/weaviate/issues/7360)
- [#8889 Deleting inactive tenant leaves folder on disk](https://github.com/weaviate/weaviate/issues/8889)
- [#8914 Tombstone cleanup takes longer after reboot](https://github.com/weaviate/weaviate/issues/8914)
- [#10268 Panic with GSE tokenizer + replication in v1.34.10](https://github.com/weaviate/weaviate/issues/10268)

**Chroma Issues:**
- [#6654 IS_PERSISTENT defaults to False — silent data loss](https://github.com/chroma-core/chroma/issues/6654)
- [#4330 Docker config.yaml breaks persistence](https://github.com/chroma-core/chroma/issues/4330)
- [#5392 ChromaDB client crashes on persisted database](https://github.com/chroma-core/chroma/issues/5392)
- [#4218 EOFError: Ran out of Input after OOM](https://github.com/chroma-core/chroma/issues/4218)
- [#5843 Memory not freed when using PersistentClient](https://github.com/chroma-core/chroma/issues/5843)
- [#5868 Unable to close PersistentClient](https://github.com/chroma-core/chroma/issues/5868)
- [#2872 No multi-replica deployment option at scale](https://github.com/chroma-core/chroma/issues/2872)
- [#2104 Lack of transactionality in create_collection](https://github.com/chroma-core/chroma/issues/2104)
