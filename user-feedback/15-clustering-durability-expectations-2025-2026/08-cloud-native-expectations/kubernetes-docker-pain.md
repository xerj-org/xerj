# Kubernetes & Docker Pain: Search Databases in Production
## Research Date: April 2026 | Data covering 2025–2026 reports

Collected from 8 targeted searches across community forums, GitHub issues, technical blogs, and official documentation. 60+ distinct data points catalogued below.

---

## 1. Elasticsearch on Kubernetes — General Deployment Pain

**DP-01** — Deploying a single Elasticsearch cluster took roughly 90 minutes. With four clusters in operation, operational burden grew significantly. *(Source: Daangn Tech Blog, Dec 2025)*

**DP-02** — Latency spikes during rolling restarts forced teams to avoid deployments during peak hours. A custom proxy ("search-coordinator") had to be built to control traffic routing so only warmed-up nodes received requests. *(Source: Daangn Tech Blog, Dec 2025)*

**DP-03** — Elasticsearch is unusually sensitive to CPU, memory, and disk throughput. Teams must start with conservative resource limits and tune based on actual workload — this iterative tuning is time-consuming and error-prone. *(Source: Sematext blog)*

**DP-04** — Running large Elasticsearch clusters on Kubernetes is harder to manage than bare-metal or VMs due to the added abstraction layer — specifically harder to decommission/restart several nodes while maintaining anti-affinity rules. *(Source: DZone)*

**DP-05** — Elasticsearch startup on Kubernetes 1.30 (ECK 2.14.0) can be blocked for 1–2 minutes or longer at the `elasticsearch-keystore has-passwd --silent` phase, with very high CPU usage during that window. Root cause is entropy depletion in containerized environments. *(Source: GitHub elastic/cloud-on-k8s issue #8973)*

**DP-06** — Migrating to Kubernetes added significant overhead: teams had to manually relocate all shards away from data pods before terminating StatefulSets when scaling down. ECK operator automated this eventually but it wasn't the default experience. *(Source: adjoe engineer blog)*

**DP-07** — On very large Kubernetes clusters with many hundreds of ECK resources, the ECK operator itself can OOMKill during startup. *(Source: Elastic official docs — Common Problems)*

**DP-08** — Updating an existing Elasticsearch cluster configuration can fail because the ECK operator is unable to apply changes while replacing pods, leaving the cluster in an inconsistent state. *(Source: Elastic official docs — Common Problems)*

**DP-09** — Timeout errors when submitting ECK resource manifests are common, and private GKE clusters require adding custom firewall rules to allow port 9443 from the API server — a non-obvious requirement that blocks first-time deployments. *(Source: Elastic official docs — Common Problems)*

**DP-10** — When upgrading ECK via Operator Lifecycle Manager (OLM), upgrades may fail to complete on older OLM versions due to OLM bugs, not Elasticsearch bugs — operators must debug through two systems simultaneously. *(Source: Elastic official docs — Common Problems)*

---

## 2. Elasticsearch Helm Chart Issues

**DP-11** — Elastic officially handed off maintenance of the Elasticsearch Helm charts to the community in 2022 and scheduled the repo for archiving after 6 months, leaving production teams uncertain about long-term support. *(Source: ArtifactHub / elastic/helm-charts)*

**DP-12** — The Elastic Helm chart contains at least 7 documented misconfigurations out of the box, including missing `livenessProbe` properties and absent resource requests for memory and CPU. *(Source: Datree Elasticsearch Helm Chart analysis)*

**DP-13** — Bitnami's Elasticsearch Helm chart does not support Elasticsearch 8.19.0, blocking teams from upgrading to pick up security patches and performance improvements. *(Source: bitnami/charts GitHub issue #35342)*

**DP-14** — The Helm chart ecosystem for Elasticsearch is fragmented: official Elastic charts, community Bitnami charts, and OpenStack-specific forks all diverge. Teams must evaluate which is maintained before deploying. *(Source: Community observations across ArtifactHub / helm/charts)*

**DP-15** — Pod keeps restarting with Readiness exit code 1 — a documented issue in the Helm chart (GitHub elastic/helm-charts issue #361) that requires manual intervention into readiness probe configuration. *(Source: GitHub elastic/helm-charts issue #361)*

**DP-16** — The community remains conflicted between Helm charts and the ECK operator — both have similar adoption rates — because ECK is perceived as not yet transformative enough to justify the migration complexity. *(Source: Sematext blog, 2025)*

---

## 3. Elasticsearch Docker Memory Issues

**DP-17** — Elasticsearch reliably uses more memory than JVM heap settings indicate. The gap is caused by off-heap usage (Lucene segments, file system cache), causing containers to breach memory limits and crash with OOM. *(Source: Elastic discuss forum — multiple threads)*

**DP-18** — Container memory allocation must account for both JVM heap and off-heap. Total container memory should be significantly higher than heap size alone — but exactly how much higher is workload-dependent and hard to predict. *(Source: Elasticsearch Labs memory usage guide)*

**DP-19** — Users report OOM crashes even with 6 GiB, 12 GiB, and 40 GiB memory limits — the problem is not simply "add more memory" but rather the unpredictable interaction between heap settings and Lucene's off-heap demands. *(Source: Elastic discuss forum threads)*

**DP-20** — Setting `bootstrap.memory_lock=true` is required to prevent memory swapping in Docker, but this setting requires extra configuration (`--cap-add IPC_LOCK` or `ulimit -l`) that is not obvious from documentation and breaks out-of-box setups. *(Source: Multiple Docker/Elastic forum discussions)*

**DP-21** — JVM memory calculation in Docker is unclear. The interaction between `-Xms`, `-Xmx`, and Docker's cgroup memory limits leads to confusion. Elasticsearch may detect the wrong host memory when running inside Docker. *(Source: Elastic discuss — "Understanding JVM memory calculation with docker")*

**DP-22** — Elasticsearch requires at least 2 GB RAM to start, with production minimums of 4–8 GB per node. In resource-constrained Kubernetes clusters, this makes Elasticsearch prohibitively expensive to co-host with other workloads. *(Source: OneUptime blog, January 2026)*

**DP-23** — JVM heap must not exceed 31 GB (compressed oops limit). Teams hitting this limit cannot simply scale up a single node — they must add nodes, requiring cluster resharding and operational complexity. *(Source: Elasticsearch documentation)*

---

## 4. Elasticsearch OOM Kills on Kubernetes (Resource Limits)

**DP-24** — Elasticsearch nodes get killed by Kubernetes OOM killer frequently in production, even when operators believe they have set heap limits correctly. The OOM kill happens at the cgroup level before Elasticsearch's internal GC can respond. *(Source: Elastic discuss — "Elasticsearch nodes get killed by kubernetes due to OOM")*

**DP-25** — In OpenShift environments, proxy containers in Elasticsearch pods restart repeatedly due to OOM kills from cgroup limits — affecting logging pipelines at scale. *(Source: Red Hat Customer Portal solution #5541601)*

**DP-26** — New Elasticsearch installations (7.15.2) consume all available memory during startup itself, before any queries are served, and get killed by the OOM killer before the cluster is usable. *(Source: Elastic discuss — issue thread for ES 7.15.2)*

**DP-27** — ECK operator itself crashes with OOM errors when configured with small memory limits — meaning you need to budget memory for both the operator and the Elasticsearch pods. *(Source: GitHub elastic/cloud-on-k8s issue #1468)*

**DP-28** — Reducing heap from 9 GB to 7 GB on a 15 GB machine resolved OOM kills — the 50% heap rule-of-thumb is known but teams frequently violate it under pressure to fit ES within budget node sizes. *(Source: Elastic discuss community)*

**DP-29** — Elasticsearch can crash due to OOM even with `ES_JAVA_OPTS` correctly set if the JVM auto-detects container memory limits differently than expected, particularly in older JDK versions that don't respect cgroup v2. *(Source: GitHub elastic/elasticsearch-docker issue #43)*

---

## 5. Elasticsearch vm.max_map_count / sysctl Problems

**DP-30** — Elasticsearch requires `vm.max_map_count=262144` (or 1,048,576 for ES 8.16+) on the host kernel. In Kubernetes, this requires a privileged init container, which many security-hardened clusters explicitly disallow. *(Source: Elastic ECK documentation)*

**DP-31** — If the Kubernetes node is restarted after the init container ran, the `vm.max_map_count` setting is lost. The init container only runs at pod creation, not on node reboot — causing a latent failure mode that teams discover during incident recovery. *(Source: GitHub pires/kubernetes-elasticsearch-cluster issue #85)*

**DP-32** — Starting with Elasticsearch 8.0.0, the sysctl init container requires `runAsUser: 0` and fails with "permission denied on key 'vm.max_map_count'" in PSP/PSA-restricted namespaces. *(Source: GitHub elastic/cloud-on-k8s issue #5410)*

**DP-33** — Alternative workarounds (DaemonSet for sysctl, custom ComputeClass) add additional Kubernetes resources and complexity to manage, just to satisfy a single OS-level prerequisite. *(Source: Elastic ECK virtual memory documentation)*

---

## 6. Elasticsearch Rolling Restart / Upgrade Complexity

**DP-34** — Standard Kubernetes rolling restart (pod Ready = traffic sent) is unsafe for Elasticsearch because a Ready pod has empty caches and cannot handle production query load. Some shards had both primary and replica affected in succession, degrading availability. *(Source: Daangn Tech Blog, Dec 2025)*

**DP-35** — Rolling restart deadlocks occur when a node running a newer ES version receives a shard whose replica cannot be allocated on older-version nodes, blocking the entire restart process. *(Source: neteye-blog.com, Dec 2025)*

**DP-36** — When a Kubernetes node goes unhealthy during upgrade, Elasticsearch waits `index.unassigned.node_left.delayed_timeout` (default 1 minute) before rebalancing shards — causing unnecessary shard movement and I/O during what may be a transient failure. *(Source: Elastic documentation)*

**DP-37** — Teams must plan Elasticsearch version upgrades to account for ECK operator upgrade time plus rolling restart of all managed workloads — a process that can degrade large clusters for hours. *(Source: Medium — Raphael De Lio, ECK upgrade guide)*

**DP-38** — Rolling restarts between major Elasticsearch versions cannot skip versions — teams on 7.x must upgrade to 7.17 before going to 8.x, then to 8.x before 9.x. Multi-hop upgrades on Kubernetes are operationally intensive. *(Source: Elastic docs — Upgrade from 7.17 to 9.3.3)*

**DP-39** — ECK's pod update strategy documentation for rolling updates is sparse; teams must read multiple docs pages to understand max unavailable settings and their impact on shard availability. *(Source: Elastic ECK pod update strategy docs)*

---

## 7. Qdrant Kubernetes Deployment Experience

**DP-40** — The Qdrant Helm chart (open source) does not include the same level of features for zero-downtime upgrades, up/down-scaling, monitoring, logging, backup, and disaster recovery as Qdrant Cloud or the enterprise Qdrant Private Cloud Operator. *(Source: Qdrant documentation — Deployment Platforms)*

**DP-41** — Qdrant's full Kubernetes Operator (with enterprise features) requires onboarding into Qdrant Hybrid Cloud, creating a managed service dependency even for teams that want fully self-hosted operation. *(Source: Qdrant documentation — Hybrid Cloud)*

**DP-42** — Teams deploying Qdrant on GKE via Terraform must manage PersistentVolume provisioning, StorageClass selection, and anti-affinity rules manually — a non-trivial operational surface for ML/AI teams not fluent in Kubernetes. *(Source: ilert blog — Qdrant Terraform deployment)*

**DP-43** — Qdrant's distributed mode on Kubernetes lacks parity with the cloud offering for operational tooling, leading teams to either accept operational gaps or pay for the managed tier. *(Source: Qdrant documentation)*

---

## 8. Milvus Kubernetes Deployment Complexity

**DP-44** — Milvus's distributed deployment requires orchestrating multiple separate services: etcd (metadata), Pulsar (message queue), MinIO (object storage), plus the Milvus query/index/data nodes. Each is a separate stateful system with its own failure modes. *(Source: Milvus blog — Deploying Milvus on Kubernetes)*

**DP-45** — Manually configuring Milvus on Kubernetes involves dozens of YAML files and intricate dependency management before a single query can be served. *(Source: Milvus blog — Milvus Operator announcement)*

**DP-46** — Even with the Milvus Operator, production deployments on GCP Kubernetes require battle-tested tuning of resource limits, health probes, and storage provisioning to avoid instability — "guide" articles note that naive deployments fail under production load. *(Source: Carlos Martinez, Medium, Dec 2025)*

**DP-47** — Milvus Lite (single-node) and Milvus distributed have very different operational profiles. Teams starting with Lite and growing to distributed face a significant migration hurdle, not an incremental scale-up. *(Source: Milvus documentation — Overview of Deployment Options)*

**DP-48** — Milvus Operator is recommended for production but beginners are directed to Helm — this split creates a migration event teams must plan rather than having a single path. *(Source: Milvus documentation)*

---

## 9. Kubernetes StatefulSet Issues (All Databases)

**DP-49** — Kubernetes StatefulSets lack built-in logic for resizing underlying persistent volumes when datasets grow. Many operators also lack resize logic and can recreate StatefulSets against old specifications on deletion events, preventing resizing. *(Source: plural.sh — "Kubernetes StatefulSets are Broken")*

**DP-50** — When a StatefulSet pod update results in a pod that never becomes Ready (bad config, bad binary), the rollout halts indefinitely — the cluster neither rolls back nor forward automatically. *(Source: Kubernetes official StatefulSet documentation)*

**DP-51** — StatefulSets do not provide backup or disaster recovery — they only ensure the same volume is reattached to the same pod identity. Teams must layer separate backup solutions on top. *(Source: spacelift.io — Guide to Kubernetes StatefulSet)*

**DP-52** — StatefulSet failures can cause silent data loss, stuck volumes, or weekend on-call incidents. Debugging requires knowledge of EKS/GKE/on-prem storage internals, not just Kubernetes concepts. *(Source: Medium — kubectl cheatsheet for broken StatefulSets)*

**DP-53** — `OnDelete` update strategy for StatefulSets gives more control but introduces downtime and requires careful manual orchestration to avoid data loss — most teams default to `RollingUpdate` without fully understanding when to use each. *(Source: spacelift.io — StatefulSet vs Deployment)*

---

## 10. Kubernetes Database Operator Expectations (Industry-Wide)

**DP-54** — In 2025, multi-cluster and GitOps integration became baseline expectations for Kubernetes database operators, not advanced features — operators that lack these are viewed as production-unready. *(Source: outerbyte.com — Kubernetes Operators in 2025)*

**DP-55** — Security hardening, performance tuning, and observability are ongoing operational requirements in 2025, not one-time setup tasks — operators must surface these continuously, not just at install time. *(Source: outerbyte.com — Kubernetes Operators in 2025)*

**DP-56** — Percona's operators (PostgreSQL, MongoDB) in 2025 focused heavily on backup/restore reliability — evidence that this remains an unresolved pain point, not a solved problem. Retry logic for transient network failures during backup jobs was only added in 2025. *(Source: Percona blog — 2025 operator wrap-ups)*

**DP-57** — Teams expect operators to handle WAL file recovery configurably during restore — "just restore from backup" is not sufficient; teams need fine-grained control over recovery point objectives. *(Source: Percona blog — PostgreSQL operator 2025)*

**DP-58** — A review from 2025 perspective found that while many Kubernetes database operators have emerged, few have reached true production maturity — operators are evaluated on how well they handle failure, not just happy-path deployment. *(Source: Medium — earayu, "Looking back from 2025")*

---

## 11. Persistent Volume / Storage Class Pain

**DP-59** — PVCs get stuck in Pending state when no PV matches the request — this is a common first-run failure for teams deploying search databases on Kubernetes, especially when StorageClass is not pre-configured on the cluster. *(Source: oneuptime.com — Kubernetes Persistent Volumes, Feb 2026)*

**DP-60** — Resizing a PVC fails silently if the StorageClass does not have `allowVolumeExpansion: true`. Teams discover this only when they attempt to grow a full Elasticsearch data volume under time pressure. *(Source: Kubernetes persistent volumes documentation)*

**DP-61** — Running out of disk space on a PersistentVolume brings down search databases — and Kubernetes provides no native alerting for volume fill-rate. Teams must instrument Prometheus + Grafana separately to catch this before it causes an outage. *(Source: clutchevents.co — Persistent Volumes best practices)*

**DP-62** — Access mode mismatches (ReadWriteOnce vs ReadWriteMany) between what the database requires and what the StorageClass supports cause PVC binding failures that are difficult to diagnose without understanding both Kubernetes storage and the database's I/O model. *(Source: Kubernetes persistent volumes documentation)*

---

## 12. Self-Hosted Kubernetes TCO / Complexity Reality Check

**DP-63** — Organizations self-host databases on Kubernetes wanting cost savings and control, but the economics are non-obvious. A single physical server costs $7–12k over 5 years including power/cooling/labor. Hidden staffing costs for Kubernetes expertise frequently exceed managed service fees. *(Source: Medium — "Self-Hosting Databases in Kubernetes: Is It Worth the Cost?")*

**DP-64** — Self-hosted Kubernetes clusters require staff who understand kubeadm bootstrapping, etcd backups, and day-2 operational concerns — a significant hidden expense that many teams underestimate until a production incident occurs. *(Source: gcore.com — Kubernetes TCO comparison)*

**DP-65** — Self-managing a search database on Kubernetes requires ensuring resilience, automation, scaling, and disaster recovery — each requiring distinct tooling that must be evaluated, integrated, and maintained. *(Source: Multiple operator documentation sources)*

**DP-66** — Weaviate Cloud and Pinecone can cost as much or more than self-hosted alternatives at scale, but self-hosting adds Kubernetes operational burden significant enough that teams at 50–100M vectors or $500+/month cloud spend are re-evaluating trade-offs rather than clearly preferring either option. *(Source: mljourney.com — Vector DB production RAG comparison)*

**DP-67** — For regulated industries (healthcare, finance, government), Pinecone's fully managed architecture is a blocker because data cannot leave the organization's infrastructure — forcing self-hosted Kubernetes deployments on teams that may not have the operational maturity for them. *(Source: tensorblue.com — Vector DB comparison 2025)*

---

## 13. OpenSearch Kubernetes-Specific Pain

**DP-68** — OpenSearch on Kubernetes requires configuring encryption for all inter-node and client communication. Without it, OpenSearch falls back to demo TLS certificates that are not suitable for production — but the security setup is complex enough that teams skip it initially and then struggle to add it later. *(Source: last9.io — OpenSearch Operator)*

**DP-69** — Fluent-bit configuration for shipping logs from Kubernetes to OpenSearch is documented as difficult to get right, requiring iteration on tag matching, routing rules, and index patterns. *(Source: OpenSearch Kubernetes community discussions)*

**DP-70** — Oversharding in OpenSearch on Kubernetes degrades performance and causes unnecessary resource consumption — but teams often copy shard configurations from non-Kubernetes deployments without accounting for the different scale characteristics of Kubernetes pods. *(Source: last9.io — OpenSearch Operator)*

---

## Summary Themes

| Theme | Data Points | Severity |
|---|---|---|
| Memory/OOM issues (ES Docker/K8s) | DP-17 to DP-29 | Critical — causes production outages |
| Rolling restart / upgrade complexity | DP-34 to DP-39 | High — requires custom tooling |
| sysctl / privilege requirements | DP-30 to DP-33 | High — blocks hardened clusters |
| Helm chart quality and maintenance gaps | DP-11 to DP-16 | High — production misconfiguration risk |
| StatefulSet limitations | DP-49 to DP-53 | High — data loss risk |
| Multi-component deployment complexity (Milvus) | DP-44 to DP-48 | High — significant day-1 barrier |
| Operator maturity expectations vs reality | DP-54 to DP-58 | Medium-High — backup/restore gaps |
| Persistent volume / storage class issues | DP-59 to DP-62 | Medium-High — silent failure modes |
| Self-hosted TCO miscalculation | DP-63 to DP-67 | Medium — strategic/financial pain |
| ECK-specific operational problems | DP-07 to DP-10 | Medium — affects large clusters |
| Qdrant/OpenSearch cloud-native gaps | DP-40 to DP-43, DP-68 to DP-70 | Medium — feature parity gaps |

**Total data points collected: 70**

---

## Sources

- [Running Elasticsearch on Kubernetes the Easy Way, Part 2 — Data Node Warm-Up (Daangn, Dec 2025)](https://medium.com/daangn/running-elasticsearch-on-kubernetes-the-easy-way-part-2-data-node-warm-up-0d81d433c5c1)
- [Run & Deploy Elasticsearch on Kubernetes — Sematext](https://sematext.com/blog/kubernetes-elasticsearch/)
- [Running Elasticsearch on Kubernetes — DZone](https://dzone.com/articles/running-elasticsearch-on-kubernetes)
- [Elasticsearch startup slow on Kubernetes 1.30 — GitHub ECK issue #8973](https://github.com/elastic/cloud-on-k8s/issues/8973)
- [Lessons Learned Migrating Elasticsearch to Kubernetes — adjoe](https://adjoe.io/company/engineer-blog/lessons-learned-while-migrating-elasticsearch-to-kubernetes/)
- [Common Problems — Elastic Cloud on Kubernetes docs](https://www.elastic.co/guide/en/cloud-on-k8s/master/k8s-common-problems.html)
- [Elasticsearch Helm Chart — Datree](https://www.datree.io/helm-chart/elasticsearch-elastic-inc)
- [bitnami/charts GitHub issue #35342](https://github.com/bitnami/charts/issues/35342)
- [Pod keeps restarting — GitHub elastic/helm-charts issue #361](https://github.com/elastic/helm-charts/issues/361)
- [Elasticsearch uses more memory than JVM heap — Elastic discuss](https://discuss.elastic.co/t/elasticsearch-uses-more-memory-than-jvm-heap-settings-reaches-container-memory-limit-and-crash/218873)
- [Elasticsearch memory usage guide — Elasticsearch Labs](https://www.elastic.co/search-labs/blog/elasticsearch-memory-usage)
- [How to Run Elasticsearch in Docker — OneUptime, Jan 2026](https://oneuptime.com/blog/post/2026-01-16-docker-elasticsearch/view)
- [Understanding JVM memory calculation with Docker — Elastic discuss](https://discuss.elastic.co/t/understanding-jvm-memory-calculation-with-docker/276628)
- [Elasticsearch nodes killed by Kubernetes OOM — Elastic discuss](https://discuss.elastic.co/t/elasticsearch-nodes-get-killed-by-kubernetes-due-to-oom/186682)
- [Proxy container OOMKilled in OpenShift — Red Hat Customer Portal](https://access.redhat.com/solutions/5541601)
- [ECK Operator OOM — GitHub cloud-on-k8s issue #1468](https://github.com/elastic/cloud-on-k8s/issues/1468)
- [Virtual memory / vm.max_map_count — Elastic ECK docs](https://www.elastic.co/guide/en/cloud-on-k8s/current/k8s-virtual-memory.html)
- [sysctl initContainer runAsUser issue — GitHub ECK #5410](https://github.com/elastic/cloud-on-k8s/issues/5410)
- [Init container limitation — GitHub pires/kubernetes-elasticsearch-cluster #85](https://github.com/pires/kubernetes-elasticsearch-cluster/issues/85)
- [Optimizing Rolling Restarts in Elasticsearch — neteye-blog.com, Dec 2025](https://www.neteye-blog.com/2025/12/optimizing-elasticsearch-cluster-rolling-restart/)
- [How To Upgrade ES on Kubernetes with ECK — Raphael De Lio, Medium](https://raphaeldelio.medium.com/how-to-upgrade-elasticsearch-kibana-logstash-and-filebeat-on-kubernetes-with-eck-974f41e76114)
- [Qdrant Deployment Platforms documentation](https://qdrant.tech/documentation/hybrid-cloud/platform-deployment-options/)
- [How to Deploy Qdrant on Kubernetes using Terraform — ilert](https://www.ilert.com/blog/how-to-deploy-qdrant-database-to-kubernetes-using-terraform-a-step-by-outer-guide-with-examples)
- [Deploying Milvus on Kubernetes — Milvus blog](https://milvus.io/blog/deploying-milvus-on-kubernetes-just-got-easier-with-the-milvus-operator.md)
- [Running Milvus on GCP Kubernetes — Carlos Martinez, Medium, Dec 2025](https://medium.com/@CarlosMartes/running-milvus-on-gcp-kubernetes-a-battle-tested-deployment-guide-a3467afc77b6)
- [Kubernetes StatefulSets are Broken — plural.sh](https://www.plural.sh/blog/kubernetes-statefulsets-are-broken/)
- [Guide to Kubernetes StatefulSet — spacelift.io](https://spacelift.io/blog/kubernetes-statefulset)
- [kubectl cheatsheet for broken StatefulSets — Medium](https://medium.com/@ismailkovvuru/the-ultimate-kubectl-cheatsheet-for-debugging-broken-statefulsets-a-devops-engineers-real-world-a2dbbe121d2d)
- [Kubernetes Operators in 2025 — outerbyte.com](https://outerbyte.com/kubernetes-operators-2025-guide/)
- [What Are Kubernetes Operators in 2025 — syntasso.io](https://www.syntasso.io/post/what-are-kubernetes-operators-and-do-you-still-need-them-in-2025)
- [Percona Operator for PostgreSQL 2025 Wrap Up](https://www.percona.com/blog/percona-operator-for-postgresql-2025-wrap-up-and-what-we-are-focusing-on-next/)
- [Percona Operator for MongoDB in 2025](https://www.percona.com/blog/percona-operator-for-mongodb-in-2025-making-distributed-mongodb-more-predictable-on-kubernetes/)
- [Looking back from 2025 at Kubernetes operators — earayu, Medium](https://medium.com/@earayu/looking-back-from-the-perspective-of-2025-several-kubernetes-operators-have-emerged-that-7c4ad26a92dc)
- [Kubernetes Persistent Volumes — OneUptime, Feb 2026](https://oneuptime.com/blog/post/2026-02-20-kubernetes-persistent-volumes-claims/view)
- [Persistent Volumes in Kubernetes: Best Practices — clutchevents.co](https://www.clutchevents.co/resources/persistent-volumes-in-kubernetes-best-practices-for-managing-stateful-workloads)
- [Self-Hosting Databases in Kubernetes — Medium](https://medium.com/@PlanB./self-hosting-databases-in-kubernetes-is-it-worth-the-cost-and-complexity-8a927a6bcc1f)
- [Kubernetes Total Cost of Ownership — gcore.com](https://gcore.com/learning/kubernetes-tco-comparison)
- [Pinecone vs Weaviate vs Qdrant for Production RAG — mljourney.com](https://mljourney.com/pinecone-vs-weaviate-vs-qdrant-choosing-a-vector-database-for-production-rag/)
- [Vector Database Comparison 2025 — tensorblue.com](https://tensorblue.com/blog/vector-database-comparison-pinecone-weaviate-qdrant-milvus-2025)
- [OpenSearch Operator — last9.io](https://last9.io/blog/opensearch-operator/)
