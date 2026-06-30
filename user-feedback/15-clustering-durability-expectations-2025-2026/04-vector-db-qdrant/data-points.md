# Qdrant Clustering & Durability Issues — Data Points

## Total: 75 data points

Sources searched:
- github.com/qdrant/qdrant issues (primary)
- github.com/qdrant/qdrant releases (changelog)
- github.com/orgs/qdrant/discussions
- github.com/qdrant/qdrant-helm issues
- qdrant.tech documentation and blog posts
- G2, Sourceforge, AWS Marketplace reviews
- Medium engineering blogs
- drdroid.io, deepwiki.com

| # | Quote/Summary | Source | Date | Severity |
|---|--------------|--------|------|----------|
| 1 | 3-node cluster with replication factor 3: when one node is killed, "all search queries returned 500 errors until the node recovered" — despite replication, single-node failure breaks all reads | https://github.com/qdrant/qdrant/issues/5215 | 2024-10 | CRITICAL |
| 2 | Root cause of issue #5215: "When replication_factor is configured via global Qdrant configuration rather than explicitly in the collection creation request, replicas are not actually created" — silently broken HA | https://github.com/qdrant/qdrant/issues/5215 | 2024-10 | CRITICAL |
| 3 | 3-node Kubernetes cluster (AWS r6a.2xlarge), 6 shards, replication factor 2, ~14M vectors: when one shard becomes dead, "the cluster fails to run a query" — single dead shard kills all queries | https://github.com/qdrant/qdrant/issues/3586 | 2024-02 | CRITICAL |
| 4 | "Cannot deactivate the last active replica 419724648802618 of shard 3" — warning logged every 10 seconds; multiple users reported identical behavior, suggesting systemic issue with shard recovery mechanisms | https://github.com/qdrant/qdrant/issues/3586 | 2024-02 | CRITICAL |
| 5 | Dead shard workaround: "restarting all nodes restores functionality, but issue recurs" — no permanent fix without service interruption | https://github.com/qdrant/qdrant/issues/3586 | 2024-02 | HIGH |
| 6 | Custom sharding with 50 shards, replication factor 3, 3 nodes: after indexing ~700K vectors, "nodes could not sync" with consensus manager errors | https://github.com/qdrant/qdrant/issues/3636 | 2024-02 | HIGH |
| 7 | Consensus failure error: "Bad request: There is no transfer for shard X from N to M" appearing in consensus manager logs with custom sharding configuration | https://github.com/qdrant/qdrant/issues/3636 | 2024-02 | HIGH |
| 8 | 3-node Qdrant v1.7.4 Kubernetes cluster: nodes diverged to commit indices of 16,211,980 / 16,211,996 / 3,714,165 — "Each commit increases forever. Consensus is never reached." Cluster completely unusable | https://github.com/qdrant/qdrant/issues/6960 | 2025-07 | CRITICAL |
| 9 | "message sender task queue is full. Message will be dropped" — thousands of warnings per minute during consensus failure; cluster stuck for several days | https://github.com/qdrant/qdrant/issues/6960 | 2025-07 | CRITICAL |
| 10 | Recovery attempts (node reboots, /cluster/recover endpoint) "proved ineffective" during consensus failure — required sequential upgrade through all intermediate versions to resolve | https://github.com/qdrant/qdrant/issues/6960 | 2025-07 | CRITICAL |
| 11 | Root cause of consensus failure (issue #6960): resource exhaustion — "maximum number of open files (1,048,576) reached on nodes" with RocksDB flush errors cascading into consensus breakdown | https://github.com/qdrant/qdrant/issues/6960 | 2025-07 | CRITICAL |
| 12 | "Waiting for consensus operation commit failed" — consensus operations timeout with 10-second timeout; all write/admin operations blocked | https://github.com/qdrant/qdrant/issues/6960 | 2025-07 | HIGH |
| 13 | High throughput ingestion (>12,000 points/sec): "timeouts on the client side" with 10-second timeout configured; without timeout, latencies spike to ~500 seconds | https://github.com/qdrant/qdrant/issues/5642 | 2024-12 | HIGH |
| 14 | Consensus operations enter "continuous failure state during data ingestion" at high throughput — shards enter dead or partially dead states during ingestion spike | https://github.com/qdrant/qdrant/issues/5642 | 2024-12 | HIGH |
| 15 | With small batch size (20 points): "Consensus failures occurred above 750 upsert requests/second" — very low throughput ceiling before cluster instability begins | https://github.com/qdrant/qdrant/issues/5642 | 2024-12 | HIGH |
| 16 | TCP memory allocation "increases substantially on servers" during high-throughput ingestion — network resource exhaustion correlates with consensus failure | https://github.com/qdrant/qdrant/issues/5642 | 2024-12 | HIGH |
| 17 | 3-node cluster, replication factor 3: during node restart, upserts committed on 2 nodes but restarted node "doesn't sync the missed upsert" — missing points on restarted node, data inconsistency | https://github.com/qdrant/qdrant/issues/4626 | 2024-07 | HIGH |
| 18 | Qdrant maintainer on write consistency: "This would require either two-phase commit schema, or sequential writes. Both options would likely damage performance, so we decided against it" — explicit architectural tradeoff sacrificing consistency | https://github.com/qdrant/qdrant/issues/4626 | 2024-07 | HIGH |
| 19 | "Node restarts are not uncommon [in Kubernetes] due to rolling upgrades, out-of-memory conditions, or cluster maintenance" — making data inconsistency a routine production risk | https://github.com/qdrant/qdrant/issues/4626 | 2024-07 | HIGH |
| 20 | Node restart during delete operations: 3 nodes missing same delete → restarted node keeps points with empty payloads. "The points with empty payloads appear in search and scroll and require filtering" | https://github.com/qdrant/qdrant/issues/4627 | 2024-07 | HIGH |
| 21 | Data inconsistency confirmed in production: restarted node showed 1,860 points vs 1,857 on other nodes — three points persisted with empty payloads only on restarted node | https://github.com/qdrant/qdrant/issues/4627 | 2024-07 | HIGH |
| 22 | Issue #4627 was "observed in production" with Qdrant v1.10.0 — not a theoretical concern | https://github.com/qdrant/qdrant/issues/4627 | 2024-07 | HIGH |
| 23 | 2-node cluster migration produced "inconsistent search results depending on which node received the request" — data inconsistency after migration visible to end users | https://github.com/qdrant/qdrant/issues/5101 | 2024 | HIGH |
| 24 | Malformed snapshot: failed recovery leaves orphaned collection config directory; on next restart, Qdrant panics at "shard_holder/mod.rs line 607: No shard found: 0" — system unrecoverable without manual intervention | https://github.com/qdrant/qdrant/issues/5983 | 2025-02 | CRITICAL |
| 25 | Panic at startup on old clusters with user-defined sharding — fixed in release, but affected users required manual storage cleanup before service would start | https://github.com/qdrant/qdrant/releases | 2025 | HIGH |
| 26 | Qdrant 1.16.1 startup panic: "failed to set up alternative stack guard page: Cannot allocate memory (os error 12)" — crash in memory-constrained environments after HTTP server reports listening | https://github.com/qdrant/qdrant/issues/7831 | 2025-12 | HIGH |
| 27 | Startup panic on upgrade from 1.16.0 to 1.16.1 in Docker with mounted volume — version upgrade caused production systems to fail to start | https://github.com/qdrant/qdrant/issues/7610 | 2025-11 | HIGH |
| 28 | Panic in shard_holder: "ERROR qdrant::startup: Panic occurred in file lib/collection/src/shards/shard_holder/mod.rs at line 607" — startup failure requiring manual recovery | https://github.com/qdrant/qdrant/issues/5672 | 2024 | HIGH |
| 29 | "ERROR qdrant::startup: Panic occurred in file src/snapshots.rs at line 71" — crash during startup snapshot handling; collection becomes unavailable | https://github.com/qdrant/qdrant/issues/6951 | 2025-07 | HIGH |
| 30 | "Bumped into Panic while container restart" — production container restart causes panic, requiring manual collection directory cleanup | https://github.com/qdrant/qdrant/issues/6974 | 2025-07 | HIGH |
| 31 | scroll API with with_vectors=True causes Qdrant to crash with panic, returns 500 Internal Server Error — production API endpoint crashing on valid request | https://github.com/qdrant/qdrant/issues/7076 | 2025 | HIGH |
| 32 | "Failed to load local shard" panic on startup — collection with existing data fails to load after restart | https://github.com/qdrant/qdrant/issues/6025 | 2025 | HIGH |
| 33 | Consistent collection corruption during point upload: panic in gridstore.rs with "OutputTooSmall { expected: 4, actual: 0 }" — 4 independent reporters confirmed same issue | https://github.com/qdrant/qdrant/issues/6679 | 2025-06 | CRITICAL |
| 34 | Collection corruption (issue #6679) reproducible in Docker on Ubuntu and Docker Desktop on Windows — not environment-specific | https://github.com/qdrant/qdrant/issues/6679 | 2025-06 | CRITICAL |
| 35 | Gridstore panic issue #6758: "OutputTooSmall { expected: 4, actual: 0 }" causing collection instability — second distinct gridstore panic report; collections enter permanent error state | https://github.com/qdrant/qdrant/issues/6758 | 2025-06 | CRITICAL |
| 36 | Gridstore flush corruption fix (v1.16.3 #7741): "flush error in Gridstore, potentially corrupting data when quickly alternating inserts/deletes" — production data corruption scenario | https://github.com/qdrant/qdrant/releases/tag/v1.16.3 | 2025-12 | CRITICAL |
| 37 | Gridstore data race fix (v1.16.3 #7702): "flush data race in Gridstore, potentially corrupting data when storage is cleared in parallel" — concurrent operation data corruption | https://github.com/qdrant/qdrant/releases/tag/v1.16.3 | 2025-12 | CRITICAL |
| 38 | WAL corruption fix (#7587): "Fix corrupting WAL with broken flush edge case after WAL is cleared or truncated" — Write-Ahead Log could be silently corrupted | https://github.com/qdrant/qdrant/releases | 2025 | CRITICAL |
| 39 | Payload index corruption fix (#7400 / related): "prevents payload index corruption" and "fixes corrupt segments on load if a segment was partially flushed" | https://github.com/qdrant/qdrant/releases | 2025 | HIGH |
| 40 | Data race during snapshots fix (#7298, #7306): "could corrupt point data if a point is moved" during snapshot creation — data corruption during routine backup operations | https://github.com/qdrant/qdrant/releases | 2025 | HIGH |
| 41 | Segment ID tracker not flushed fix (#7263): "not flushing mutable ID tracker files after creation, potentially causing segment corruption" | https://github.com/qdrant/qdrant/releases | 2025 | HIGH |
| 42 | Fix for payload index storage still flushing after removal (#7621, #7626) — IO errors after payload index drop, potential data corruption | https://github.com/qdrant/qdrant/releases | 2025 | HIGH |
| 43 | Restore of large collection (37GB, 2.7M vectors, 3-node cluster) fails: "status turns yellow but does not stay yellow for long" then returns green with dead shards — snapshot DR broken for large collections | https://github.com/qdrant/qdrant/issues/5857 | 2025-01 | CRITICAL |
| 44 | Error during large collection restore: "Failed to remove tmp directory... No such file or directory" — reproducible with all collections >30GB | https://github.com/qdrant/qdrant/issues/5857 | 2025-01 | HIGH |
| 45 | File IO error during bulk snapshot recovery: first ~100 collections restore, subsequent ones fail with "failed to unpack... wal/open-1" — bulk disaster recovery blocked at scale | https://github.com/qdrant/qdrant/issues/5548 | 2024-11 | HIGH |
| 46 | Issue #5548 reproducible across Qdrant v1.9.1, v1.12.5, v1.13.4 — persistent issue across multiple release cycles | https://github.com/qdrant/qdrant/issues/5548 | 2024-11 | HIGH |
| 47 | Container fails to come up when restoring full storage snapshot — "Restoring snapshots is done through the Qdrant CLI at startup time" but fails silently | https://github.com/qdrant/qdrant/issues/3673 | 2024-02 | HIGH |
| 48 | Azure File Share snapshot restoration causes panic at startup — cloud storage integration broken for common enterprise backup target | https://github.com/qdrant/qdrant-helm/issues/126 | 2024-01 | HIGH |
| 49 | 3-node StatefulSet with podManagementPolicy: Parallel — "if node is not marked as Ready, Kubernetes does not include it in headless service DNS records, which blocks Raft consensus" — chicken-and-egg deployment deadlock | https://github.com/qdrant/qdrant/issues/5824 | 2025-01 | HIGH |
| 50 | "qdrant-2 significantly lagging behind in its commit index, indicating it has not applied a substantial number of log entries" — Raft log divergence during parallel StatefulSet startup | https://github.com/qdrant/qdrant/issues/5824 | 2025-01 | HIGH |
| 51 | Kubernetes HA issue (issue #6432): "if one of the nodes restarts (due to memory pressure), the Qdrant pods on that node go down" causing application downtime despite cluster configuration | https://github.com/qdrant/qdrant/issues/6432 | 2025-04 | HIGH |
| 52 | Rolling restart HA concern: "if my collection has shards distributed across all 10 pods, won't this rolling restart temporarily break the availability of some shards, causing downtime?" | https://github.com/qdrant/qdrant/issues/6432 | 2025-04 | MEDIUM |
| 53 | PVC migration causing Raft errors: one of three migrated pods recovered, two remained in CrashLoopBackOff — migration via file copy causes Raft identity conflicts | https://github.com/qdrant/qdrant/issues/6348 | 2025-04 | HIGH |
| 54 | Built-in Qdrant snapshotting during PVC migration "resulted in corrupted snapshots and 30% data loss in one collection" — data loss from using official backup mechanism during migration | https://github.com/qdrant/qdrant/issues/6348 | 2025-04 | CRITICAL |
| 55 | "PVC migrations are not supported in public helm chart (stateful sets). Once cluster is created, it can't migrate to other URLs by simply copying files" — fundamental operational limitation | https://github.com/qdrant/qdrant/issues/6348 | 2025-04 | HIGH |
| 56 | OOMKill during ingestion: 8-replica cluster "memory ramps until requested amounts and then triggers OOMKills. When the cluster goes down, it has problems booting from disk again" — crash recovery loop | https://github.com/orgs/qdrant/discussions/3501 | 2024 | CRITICAL |
| 57 | OOM during large dataset caching: "rapidly ingesting with quantization turned on, full vectors are cached causing the cluster to use significantly more memory than expected, leading to premature OOM kills" | https://github.com/qdrant/qdrant/issues/4378 | 2024 | HIGH |
| 58 | Qdrant startup takes "1 hour to complete collection metadata operations and consensus manager processes" when scaling from 1 to 6 nodes — startup time scales poorly with cluster size | https://github.com/qdrant/qdrant/issues/4980 | 2024 | HIGH |
| 59 | Startup too slow with ~100K vectors in one collection — users report unacceptable restart times even at modest data scales | https://github.com/qdrant/qdrant/issues/7190 | 2025-09 | MEDIUM |
| 60 | Optimization taking too long: even 10,000 points showed "ongoing indexing (yellow status) for over 3 hours" — indexing pipeline stalls under load | https://github.com/qdrant/qdrant/issues/5681 | 2024-12 | MEDIUM |
| 61 | Performance regression since v1.6: "response times have increased when using range filters during vector searches" — degradation introduced and persisting across patch versions | https://github.com/qdrant/qdrant/issues/4071 | 2024 | HIGH |
| 62 | Filter performance: querying all points using payload filters is "very slow" — ~100K points with filters causes timeouts that don't occur without filters | https://github.com/qdrant/qdrant/issues/3805 | 2024 | HIGH |
| 63 | Deadlock while dropping payload index (issue #6573) — appeared in CI testing, indicating production risk | https://github.com/qdrant/qdrant/issues/6573 | 2025-05 | HIGH |
| 64 | "Qdrant won't work with Network file systems such as NFS, or Object storage systems such as S3. Using Docker/WSL on Windows with mounts is known to have file system problems causing data loss" — documented operational constraint | https://qdrant.tech/documentation/guides/distributed_deployment/ | 2024 | HIGH |
| 65 | Resharding down panic "if no shard key is provided on a collection with custom sharding" — production panic during routine cluster scaling operation | https://github.com/qdrant/qdrant/releases | 2025 | HIGH |
| 66 | "When nodes are added to a cluster, resharding is needed to redistribute data. Without it, original nodes become overloaded while new nodes sit mostly idle" — out-of-memory risk on overloaded nodes | https://qdrant.tech/documentation/guides/distributed_deployment/ | 2024 | HIGH |
| 67 | Resharding is "only available in Qdrant Cloud and Hybrid/Private Clouds, and not when self-hosting" — self-hosted deployments cannot rebalance after scaling | https://qdrant.tech/articles/agentic-builders-guide/ | 2025 | HIGH |
| 68 | "Horizontal scaling features are still evolving compared to more mature systems, and operational tooling around large-scale clustering remains relatively limited" — acknowledged limitation from 2025 review | https://www.meilisearch.com/blog/elasticsearch-vs-qdrant | 2025 | HIGH |
| 69 | Helm chart limitation: "does not come with features for zero-downtime upgrades, up and down-scaling, monitoring, logging, and backup/disaster recovery" — self-hosted operators lack enterprise operational tooling | https://github.com/qdrant/qdrant-helm/blob/main/charts/qdrant/README.md | 2024 | HIGH |
| 70 | Security concerns: Qdrant "made misleading claims about PCI certification and lacked most markers of a company who has prioritized security" — self-hosted option had "insecure defaults" at last review | https://ironcorelabs.com/vectordbs/qdrant-security/ | 2024 | MEDIUM |
| 71 | Reddit evaluation (2025): "there was far more interaction between ingestion and query load on Qdrant than on Milvus" — ingestion degrades query performance significantly more than competing systems | https://milvus.io/blog/choosing-a-vector-database-for-ann-search-at-reddit.md | 2025 | HIGH |
| 72 | G2/Sourceforge user review: "instance became corrupt after a container restart and consuming a large amount of storage" — spontaneous corruption on restart | https://sourceforge.net/projects/qdrant.mirror/reviews/ | 2024-2025 | CRITICAL |
| 73 | G2 review: "initial ingestion hiccups raise concerns for large datasets" and "stability during data loading needs attention" | https://www.g2.com/products/qdrant/reviews | 2024-2025 | MEDIUM |
| 74 | Fix for breaking Raft by killing node at specific time during consensus snapshot (#7577) — race condition could cause crash loop on any node failure during snapshot | https://github.com/qdrant/qdrant/releases | 2026-02 | CRITICAL |
| 75 | Single-node deployments "will experience downtime during node restarts, and recovery is not possible unless you have backups or snapshots" — officially documented; full outage on any restart | https://qdrant.tech/documentation/guides/distributed_deployment/ | 2024 | HIGH |

---

## Thematic Summary

### Consensus / Raft Failures (data points: 6, 7, 8, 9, 10, 11, 12, 49, 50, 53, 74)
Qdrant's Raft-based consensus is the most critical failure surface. Node divergence can render a cluster completely unusable with no self-recovery path. Common triggers include resource exhaustion, Kubernetes network partitions, PVC migrations, and node kills during consensus snapshots. Some clusters remained stuck for multiple days.

### Data Inconsistency on Node Restart (data points: 1, 2, 17, 18, 19, 20, 21, 22)
Qdrant explicitly chose not to implement two-phase commit for performance reasons. This means node restarts during writes produce real data inconsistency. The maintainer documented this tradeoff. Data lost during a restart window is not recovered automatically when the node rejoins.

### Storage / Gridstore Corruption (data points: 33–42, 64, 72)
The Gridstore key-value store (introduced as RocksDB replacement) has exhibited multiple data corruption modes under production conditions: flush races, WAL truncation bugs, segment partial flushes, and filesystem incompatibilities. All were addressed in patches but indicate the system's storage layer is still maturing.

### Snapshot / Backup-Restore Failures (data points: 24, 43–48, 54)
Large collection restores (>30GB) fail consistently. Bulk restores of 200+ collections fail mid-way through. Azure File Share restore panics. PVC migration using built-in snapshots caused 30% data loss in one documented case.

### Startup Panics and Crashes (data points: 25–32, 57, 59)
Multiple panic scenarios on service restart — from malformed collection directories, version upgrade incompatibilities, to memory allocation failures. Some panics require manual storage cleanup before service starts.

### OOM / Memory Pressure (data points: 56, 57, 58)
OOMKills during ingestion cause cluster-wide problems because Qdrant has trouble booting from disk after crash. Quantization cache behavior causes unexpected memory spikes.

### High Availability Gaps in Self-Hosted (data points: 51, 52, 65, 66, 67, 68, 69, 75)
Resharding not available in self-hosted (only cloud). Helm chart lacks zero-downtime upgrades. Single-node deployments have no HA. Even with replication, planned rolling restarts cause shard downtime if replication factor = 1.

### Performance Degradation at Scale (data points: 61, 62, 63, 71)
Range filter regression introduced in v1.6 persisted across versions. Filter queries can stall. Query/ingest interaction degrades performance significantly more than competing systems (per Reddit's 2025 evaluation).
