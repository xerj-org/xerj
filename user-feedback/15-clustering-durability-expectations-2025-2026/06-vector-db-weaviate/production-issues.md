# Weaviate Production Issues — Research Summary 2025–2026

Collected via 8 targeted searches across GitHub Issues, community forums, CVE databases, and review platforms.
Data collected: 2026-04-10.

---

| # | Quote/Summary | Source | Date | Severity |
|---|---------------|--------|------|----------|
| 1 | Bulk Upsert via /v1/batch/objects Unexpectedly Clears Existing Data — updating a subset of objects removes all other existing objects in the collection (Windows) | [GitHub #8093](https://github.com/weaviate/weaviate/issues/8093) | May 2025 | Critical |
| 2 | "Class data deleted on restart on single node cluster" — all data wiped on restart on affected versions | [GitHub #5971](https://github.com/weaviate/weaviate/issues/5971) | 2025 | Critical |
| 3 | "Data loss in 1.26.4 version whenever pod restarted" — all data lost on Kubernetes pod restart; logs show empty write-ahead-log warnings | [GitHub #7162](https://github.com/weaviate/weaviate/issues/7162) | 2025 | Critical |
| 4 | CRITICAL BUG in v1.25.13: "might end up with cluster data deletion in certain setups" — affects single-node clusters on 1.25.12–1.25.19 and 1.26.2–1.26.5 where no Raft snapshot was ever taken | [Weaviate Forum Announcement](https://forum.weaviate.io/t/important-bug-fix-available/5290) | 2025 | Critical |
| 5 | EKS multi-replica Raft migration failure on upgrade to 1.25.0 — 7 of 19 classes became inaccessible with "shard not found" errors after Raft schema migration | [Weaviate Forum](https://forum.weaviate.io/t/eks-multi-replica-raft-migration-failure-in-weaviate-1-25-0/22033) | 2025 | Critical |
| 6 | Multi-node EKS cluster — "Raft consensus data corrupted" — after infrastructure changes, object queries return no data; schemas and tenants still visible | [Weaviate Forum](https://forum.weaviate.io/t/multi-node-weaviate-eks-cluster-raft-consensus-data-corrupted/22191) | 2025 | Critical |
| 7 | "Schema loss after scale down and scale up the RAFT cluster" — cluster fails to restart; Raft does not support scale-down/deletion in certain configurations | [Weaviate Forum](https://forum.weaviate.io/t/schema-loss-after-scale-down-and-scale-up-the-raft-cluster/22317) | 2025 | Critical |
| 8 | "Node Desync and Cluster Inconsistencies After OOM on Weaviate-0" — OOM kill on primary node causes cluster inconsistency across nodes | [Weaviate Forum](https://forum.weaviate.io/t/node-desync-and-cluster-inconsistencies-after-oom-on-weaviate-0/4466) | 2025 | Critical |
| 9 | "Panic after crash during importing data" (Issue #7408) — deploying with 7 replicas and async indexing, ungraceful shutdown causes panic on restart; vector index queue initialization fails | [GitHub #7408](https://github.com/weaviate/weaviate/issues/7408) | 2025 | Critical |
| 10 | "Panic when restarting Weaviate after unexpected termination on macOS (shard vector index restore failed)" — panic while restoring vector index from disk after force-kill during insert | [GitHub #8622](https://github.com/weaviate/weaviate/issues/8622) | 2025 | Critical |
| 11 | CVE-2025-67818 (CVSS 7.2 High) — Path traversal vulnerability in backup module; attacker with insert access can overwrite arbitrary files on restore | [GitHub Advisory](https://github.com/advisories/GHSA-7v39-2hx7-7c43) / [NVD](https://nvd.nist.gov/vuln/detail/CVE-2025-67818) | Nov 2025 | Critical |
| 12 | CVE-2025-67819 (CVSS 4.9 Medium) — Path traversal in shard movement module; attacker can read arbitrary files accessible to the service process | [Weaviate Security Blog](https://weaviate.io/blog/weaviate-security-release-november-2025) | Nov 2025 | High |
| 13 | "Founding node is unable to rejoin cluster on ECS" (Issue #8423) — after crash, founding node of 3-node cluster creates a new empty cluster instead of rejoining the existing one | [GitHub #8423](https://github.com/weaviate/weaviate/issues/8423) | Jun 2025 | High |
| 14 | "Asynchronous replication race condition after deleting collection while objects reparation is ongoing" — leaves cluster in unstable state, different nodes hold different data for same collection | [GitHub #6900](https://github.com/weaviate/weaviate/issues/6900) | 2025 | High |
| 15 | "Async Replication Issue After Version Migration from 1.24.25 to 1.26.10" — async replication does not work as expected after class was created on 1.24.x | [GitHub #6387](https://github.com/weaviate/weaviate/issues/6387) | 2025 | High |
| 16 | "Potential for half-written objects if update follows replication inconsistency" — when replicas come back before repair, partial updates cannot be corrected by further read-repairs | [GitHub #5277](https://github.com/weaviate/weaviate/issues/5277) | Ongoing | High |
| 17 | "Existing replication factor increase implementation does not work with Raft" — updating replication factor in a collection doesn't work as expected on 1.25 | [GitHub #4840](https://github.com/weaviate/weaviate/issues/4840) | 2025 | High |
| 18 | "Production weaviate 24.6 crashed" — unexpected production crash on Docker-compose setup | [Weaviate Forum](https://forum.weaviate.io/t/production-weaviate-24.6-crashed/11686) | Mar 2025 | High |
| 19 | "How to recover from Weaviate cluster crash due to memory limit?" — OOM-killed nodes leave cluster in unknown state with no clear recovery path | [Weaviate Forum](https://forum.weaviate.io/t/how-to-recover-from-weaviate-cluster-crash-due-to-memory-limit/1590) | 2025 | High |
| 20 | "Unclean shutdown of nodes in Kubernetes (panic: close database)" — Kubernetes pod restart triggers panic during LSM compaction on shutdown | [Weaviate Forum](https://forum.weaviate.io/t/unclean-shutdown-of-nodes-in-kubernetes-panic-close-database/21688) | 2025 | High |
| 21 | "LSM compaction broken after kill" (Issue #1697) — after OOM kill, LSM compaction processes are broken on restart | [GitHub #1697](https://github.com/semi-technologies/weaviate/issues/1697) | Ongoing | High |
| 22 | "Fault address / data race in lsmkv" (Issue #7560) — data race reported on 1.28.11 during repeated inserts and reads on single-node cluster | [GitHub #7560](https://github.com/weaviate/weaviate/issues/7560) | 2025 | High |
| 23 | "Objects may fail to be added when using batch import + vectorizer + async indexing" (Issue #7156) — some batches silently fail during high-throughput import with Cohere vectorizer | [GitHub #7156](https://github.com/weaviate/weaviate/issues/7156) | Feb 2025 | High |
| 24 | "BM25 search returns NaN scores on corrupted prop lengths" (Issue #6247) — data corruption in property length storage causes NaN scores in BM25 results | [GitHub #6247](https://github.com/weaviate/weaviate/issues/6247) | 2025 | High |
| 25 | "Pod restarts after S3 backup job started, backup job is cancelled" (Issue #7423) — S3 backup triggers pod restart, cancelling the in-progress backup | [GitHub #7423](https://github.com/weaviate/weaviate/issues/7423) | 2025 | High |
| 26 | "High Memory Usage After Upgrading Weaviate to Version 1.25" — 128 GB RAM instance showing 70%+ usage after upgrade; unexpected and unexplained | [Weaviate Forum](https://forum.weaviate.io/t/high-memory-usage-after-upgrading-weaviate-to-version-1-25/4028) | 2025 | High |
| 27 | "Weaviate docker container consumes 35 GB of memory with only 100k records" — drastically higher than expected memory footprint in production | [Weaviate Forum](https://forum.weaviate.io/t/weaviate-docker-container-consume-35gb-of-memory-with-only-100k-records/2246) | 2025 | High |
| 28 | "Memory Pressure in Single-Instance Weaviate Under Continuous Write/Deletion Load" — GOMEMLIMIT only controls Go heap, not total process memory; leads to unpredictable OOM behavior | [Weaviate Forum](https://forum.weaviate.io/t/memory-pressure-in-single-instance-weaviate-under-continuous-write-deletion-load/22104) | 2025 | High |
| 29 | "Very High Memory usage even after low vector_cache_max_objects" — memory does not drop even when cache size setting is lowered; setting has limited effect | [Weaviate Forum](https://forum.weaviate.io/t/very-high-memory-usage-even-after-low-vector-cache_max_objects/2732) | 2025 | Medium |
| 30 | "Space not being freed up on Weaviate's instances after deleting objects" (Issue #7360) — after deleting 99,999 of 100,000 objects across 3-node cluster, 600 MB of disk still consumed | [GitHub #7360](https://github.com/weaviate/weaviate/issues/7360) | Feb 2025 | Medium |
| 31 | "Storage Size Not Reducing After Deleting Content Chunks in Weaviate" — tombstones not cleaned up during compaction; community reports widespread occurrence | [Weaviate Forum](https://forum.weaviate.io/t/storage-size-not-reducing-after-deleting-content-chunks-in-weaviate-expected-behavior-or-issue/22193) | 2025 | Medium |
| 32 | "Volume and objects size going up instead of down after removing >50% of objects" — disk usage increases instead of decreasing after large bulk deletes | [Weaviate Forum](https://forum.weaviate.io/t/volume-and-objects-size-going-up-instead-of-down-after-removing-50-of-objects/10473) | 2025 | Medium |
| 33 | BM25 block compaction bug — using non-merged tombstones on compaction; fixed in PR #7447, but existed in production for extended period | [GitHub PR #7447](https://github.com/weaviate/weaviate/pull/7447) | 2025 | Medium |
| 34 | "Unable to restore a filesystem based backup on another machine" — file-based backups cannot be portably restored; node names must exactly match source | [Weaviate Forum](https://forum.weaviate.io/t/unable-to-restore-a-filesystem-based-backup-on-another-machine/3129) | 2025 | Medium |
| 35 | "Problems restoring from weaviate backup" — classes already existing on target node causes restore to fail silently or with opaque errors | [Weaviate Forum](https://forum.weaviate.io/t/problems-restoring-from-weaviate-backup/2489) | 2025 | Medium |
| 36 | "Migration from self-hosted to Weaviate Cloud using backup/restore" — restore path between on-prem and managed cloud unreliable without exact node name match | [Weaviate Forum](https://forum.weaviate.io/t/migration-from-self-hosted-to-weaviate-cloud-using-backup-restore/20760) | 2025 | Medium |
| 37 | Versions <= 1.23.12 must be upgraded before restore or risk data corruption — critical requirement missing from early user guides | [Weaviate Docs](https://docs.weaviate.io/deploy/configuration/backups) | 2025 | Medium |
| 38 | "Deleting an inactive tenant leaves the folder in the disk" (Issue #8889) — deleted tenants can be accidentally resurrected with old data if same tenant name is reused | [GitHub #8889](https://github.com/weaviate/weaviate/issues/8889) | Aug 2025 | Medium |
| 39 | "Weaviate on Railway does not persist to disk" (Issue #7516) — redeployment wipes state; persistence not working on Railway hosting environment | [GitHub #7516](https://github.com/weaviate/weaviate/issues/7516) | Mar 2025 | Medium |
| 40 | "Cross-reference from multi-tenant collection to single-tenant collection fails" (Issue #7281) — runtime error: "class has multi-tenancy disabled, but request was with tenant" | [GitHub #7281](https://github.com/weaviate/weaviate/issues/7281) | 2025 | Medium |
| 41 | "Multi tenancy doesn't help in our scenario when the number of collections reaches 1000" — performance degrades significantly beyond ~1000 collections | [Weaviate Forum](https://forum.weaviate.io/t/multi-tenancy-dosent-help-in-our-scenario-when-the-number-of-collection-reach-1000/21735) | 2025 | Medium |
| 42 | "Specifying properties with multi-tenancy causes bug" — schema declaration with properties alongside multi-tenancy config causes "Object was not updated" errors | [Weaviate Forum](https://forum.weaviate.io/t/specifying-properties-with-multi-tenancy-causes-bug/3946) | 2025 | Medium |
| 43 | "Description disappears after upgrade from 1.25 to 1.26" (Issue #7434) — metadata silently lost during minor version upgrade | [GitHub #7434](https://github.com/weaviate/weaviate/issues/7434) | 2025 | Medium |
| 44 | RBAC API breaking changes in v1.29 — multiple breaking changes to the RBAC API from the 1.28 preview version; no backward compatibility path | [Weaviate Blog](https://weaviate.io/blog/weaviate-1-29-release) | 2025 | Medium |
| 45 | Python client v3 API removed in December 2024 — weaviate-client v4 not backward compatible with any server below 1.27.0; forces coordinated dual upgrade | [Weaviate Blog](https://weaviate.io/blog/python-v3-client-deprecation) | 2025 | Medium |
| 46 | Python client v4 vectorizer config breaking change (v4.16.0+) — `.vectorizer_config` replaced by `.vector_config`; `Configure.NamedVectors` replaced by `Configure.Vectors`; widespread downstream breakage | [Python Client Changelog](https://weaviate-python-client.readthedocs.io/en/stable/changelog.html) | 2025 | Medium |
| 47 | Filter syntax completely reworked in v4 Python client — old `Filter(path=[...])` syntax broken; must migrate to `Filter.by_ref().by_property()` | [Dify Migration Guide](https://docs.dify.ai/en/self-host/troubleshooting/weaviate-v4-migration) | 2025 | Medium |
| 48 | Dspy OSS project — "Upgrade Weaviate client to v4" (Issue #699) — upstream client changes forced emergency updates across multiple dependent projects | [GitHub stanfordnlp/dspy #699](https://github.com/stanfordnlp/dspy/issues/699) | 2025 | Medium |
| 49 | "Slow query response times" after inactivity — HNSW vector cache becomes cold after hours of idleness; first queries take 30+ seconds to warm up | [Weaviate Forum](https://forum.weaviate.io/t/slow-query-response-times/21416) | 2025 | Medium |
| 50 | "Query response time is very slow after several hours of inactivity" — cache warm-up on large datasets; no automatic pre-warming mechanism | [Weaviate Forum](https://forum.weaviate.io/t/query-response-time-is-very-slow-after-several-hours-of-inactivity/2327) | 2025 | Medium |
| 51 | Hybrid search on 11M objects with filters averages 30 seconds, up to 2 minutes — reported by production user; makes real-time use cases impractical | [Weaviate Forum](https://forum.weaviate.io/t/high-query-latency-in-weaviate/3686) | 2025 | Medium |
| 52 | User with 18M objects, 256 GB RAM, 128 cores reports queries taking 5–15 seconds; exact repeats 3–4 seconds — performance unpredictable at scale | [Weaviate Forum](https://forum.weaviate.io/t/slow-query-response-times/21416) | 2025 | Medium |
| 53 | "Poor performance with scaling" (Issue #4572) — latency becomes unpredictable as load grows even with good hardware; cited in community as a persistent weak point | [GitHub #4572](https://github.com/weaviate/weaviate/issues/4572) | Ongoing | Medium |
| 54 | HNSW index loading scales linearly with number of operations — very large indexes can take dozens of minutes to load on node restart; production downtime risk | [Weaviate Docs](https://docs.weaviate.io/weaviate/concepts/vector-index) | Ongoing | Medium |
| 55 | HNSW cleanup (tombstone removal) requires more resources as index grows — for very large indexes, periodic cleanup can cause measurable performance degradation | [Weaviate Docs](https://docs.weaviate.io/weaviate/concepts/vector-index) | Ongoing | Medium |
| 56 | No vector index rebuild API — requested in Issue #3171; production users with corrupted or suboptimal indexes have no in-place rebuild path | [GitHub #3171](https://github.com/weaviate/weaviate/issues/3171) | Ongoing | Medium |
| 57 | G2 reviewer: "Data corrupted and Weaviate became entirely useless, retrieving different data at each request — Weaviate took a day to respond each time and consistently shifted blame to the user" | [G2 Reviews](https://www.g2.com/products/weaviate/reviews) | 2025 | High |
| 58 | G2/Gartner reviewer: "Performance when trying to scale things up" cited as main issue; described as a significant concern for larger datasets | [Gartner Peer Insights](https://www.gartner.com/reviews/product/weaviate) | 2025 | Medium |
| 59 | HN/G2 complaint: "Management console is incredibly barebones — no way to turn off instances or help yourself with issues" | [Hacker News](https://news.ycombinator.com/item?id=37391776) | 2025 | Low |
| 60 | HN complaint: "Must reindex all data when attaching a new module (vectorizer, generator) to a data class" — prohibitively expensive for non-prototype production workloads | [Hacker News](https://news.ycombinator.com/item?id=36116316) | Ongoing | Medium |
| 61 | Reviewer complaint: "Have to manage another vendor for data-store — doesn't function as general-purpose operational DB; additional DR/BCP burden" | [G2 Reviews](https://www.g2.com/products/weaviate/reviews) | 2025 | Low |
| 62 | Qdrant vs Weaviate reliability comparison: "Qdrant outperforms and offers ACID-compliant transactions for data consistency; Weaviate's eventual consistency model can be problematic" | [Cipher Projects Blog](https://cipherprojects.com/blog/posts/weaviate-vs-qdrant-vector-database-comparison-2025/) | 2025 | Low |
| 63 | Qdrant benchmarks suggest faster query speeds than Weaviate in head-to-head production comparison; Weaviate trades raw speed for broader query-type consistency | [Zilliz Comparison](https://zilliz.com/comparison/weaviate-vs-qdrant) | 2025 | Low |
| 64 | Raft bootstrap timeout causes cluster to take a long time to start — Issue #6284; no adaptive timeout; production clusters can appear hung on bootstrap | [GitHub #6284](https://github.com/weaviate/weaviate/issues/6284) | 2025 | Medium |
| 65 | "Deleting a collection after graceful restart while data is being imported ends up in inconsistent state" (Issue #5973) — incorrect replication factor recorded post-delete | [GitHub #5973](https://github.com/weaviate/weaviate/issues/5973) | 2025 | Medium |
| 66 | "Filtering on properties does not work" (Issue #8446) — filter logic regression affecting production query correctness | [GitHub #8446](https://github.com/weaviate/weaviate/issues/8446) | 2025 | High |
| 67 | "OOM issues when inserting data" — forum thread (2025): nodes OOM-killed during high-load batch insert operations; no graceful back-pressure mechanism | [Weaviate Forum](https://forum.weaviate.io/t/oom-issues-when-inserting-data/21903) | 2025 | High |
| 68 | "Weaviate Shutting Down Automatically" — production instance shuts down without clear error; discussed in forum with no definitive root cause identified | [Weaviate Forum](https://forum.weaviate.io/t/weaviate-shutting-down-automatically/4213) | 2025 | High |
| 69 | "No space left on device when both RAM and disk have >30% space" — Weaviate reports out-of-space error despite ample resources; mmap exhaustion suspected | [Weaviate Forum](https://forum.weaviate.io/t/no-space-left-on-device-when-both-ram-and-disk-have-30-space/1337) | Ongoing | Medium |
| 70 | Weaviate self-acknowledges that for 2025 the focus was "establishing production foundation" — implies prior versions were not production-grade; direction for 2026 only now deepening | [Weaviate Blog](https://weaviate.io/blog/weaviate-in-2025) | Jan 2025 | Low |

---

## Search Queries Used

1. `weaviate production issues scaling 2025 2026`
2. `site:github.com/weaviate/weaviate/issues data loss OR corruption 2025`
3. `weaviate cluster replication failure production 2025`
4. `weaviate memory usage production problem 2025`
5. `weaviate HNSW index rebuild time production`
6. `weaviate backup restore failure 2025`
7. `weaviate vs qdrant production reliability comparison 2025`
8. `weaviate upgrade breaking change 2025`
9. `weaviate forum production crash OOM killed 2025`
10. `weaviate github issues node crash data loss 2025`
11. `weaviate slow query performance degradation production`
12. `weaviate replication inconsistency async repair bug`
13. `weaviate reddit complaints production 2025`
14. `weaviate kubernetes pod restart data loss production kubernetes`
15. `weaviate tenant isolation failure multi-tenant production bug`
16. `weaviate CVE security vulnerability 2025`
17. `weaviate 1.25 data deletion bug cluster class deleted production`
18. `weaviate disk space not freed tombstones compaction issue`
19. `weaviate python client v4 migration breaking issues 2025`
20. `weaviate schema loss RAFT cluster production 2025`

---

## Sources

- [Weaviate GitHub Issues](https://github.com/weaviate/weaviate/issues)
- [Weaviate Community Forum](https://forum.weaviate.io)
- [Weaviate Blog — In 2025](https://weaviate.io/blog/weaviate-in-2025)
- [CVE-2025-67818 NVD](https://nvd.nist.gov/vuln/detail/CVE-2025-67818)
- [CVE-2025-67819 Wiz](https://www.wiz.io/vulnerability-database/cve/cve-2025-67819)
- [Weaviate Security Blog Nov 2025](https://weaviate.io/blog/weaviate-security-release-november-2025)
- [Weaviate 1.29 Release Notes](https://weaviate.io/blog/weaviate-1-29-release)
- [G2 Reviews 2026](https://www.g2.com/products/weaviate/reviews)
- [Gartner Peer Insights 2025](https://www.gartner.com/reviews/market/search-and-product-discovery/vendor/weaviate/product/weaviate)
- [Hacker News Weaviate Discussion](https://news.ycombinator.com/item?id=37391776)
- [Cipher Projects Weaviate vs Qdrant 2025](https://cipherprojects.com/blog/posts/weaviate-vs-qdrant-vector-database-comparison-2025/)
- [Zilliz Weaviate vs Qdrant Comparison](https://zilliz.com/comparison/weaviate-vs-qdrant)
- [Python Client Changelog](https://weaviate-python-client.readthedocs.io/en/stable/changelog.html)
- [Dify Weaviate v4 Migration Guide](https://docs.dify.ai/en/self-host/troubleshooting/weaviate-v4-migration)
- [Weaviate Docs — Resource Planning](https://weaviate.io/developers/weaviate/concepts/resources)
- [Weaviate Docs — Vector Index Concepts](https://docs.weaviate.io/weaviate/concepts/vector-index)
- [Weaviate Docs — Replication Architecture](https://docs.weaviate.io/weaviate/concepts/replication-architecture)
- [Weaviate Python v3 Deprecation Blog](https://weaviate.io/blog/python-v3-client-deprecation)
