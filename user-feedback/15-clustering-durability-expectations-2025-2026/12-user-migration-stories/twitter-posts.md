# Search Database Pain: Twitter/X Posts & Community Signals 2024–2025

Collected from web searches across Twitter/X, Hacker News, Reddit, GitHub Issues, engineering blogs, and community forums. 60+ data points organized by theme.

---

## 1. Elasticsearch Cost Pain

**DP-01** | Source: [Meilisearch blog citing community feedback, 2025](https://www.meilisearch.com/blog/elasticsearch-pricing)
> Elasticsearch pricing is "frustratingly elusive" — users repeatedly voice concerns about Elastic's lack of transparent, predictable pricing in community forums.

**DP-02** | Source: [Quesma blog, Jan 2025](https://quesma.com/blog/elastic-pricing/)
> Elastic announced a pricing change on January 27, 2025, estimated at a **30% price increase** for a typical production workload. No major coverage of enterprise pushback — but the silence speaks.

**DP-03** | Source: [Vendr marketplace data, 2025](https://www.vendr.com/marketplace/elastic)
> Small teams: $1,500–$8,000/month on Elastic Cloud. Mid-market: $10,000–$50,000/month. Enterprise: $100,000+/month. A 2-node, 8GB RAM Elasticsearch + Kibana deployment starts at ~$500/month on Standard.

**DP-04** | Source: [Meilisearch comparison page, 2025](https://www.meilisearch.com/blog/elasticsearch-pricing)
> Deploying the "free" Elastic Stack into production leads to significant **unplanned expenses** — hidden costs around storage, compute, and support tiers that are difficult to predict in advance.

**DP-05** | Source: [alternatives.co pricing analysis, 2025](https://alternatives.co/software/elasticsearch/pricing/)
> Elasticsearch's actual costs depend on configurations and usage patterns that are difficult to predict — even cloud cost estimation tools struggle to model it accurately.

---

## 2. Elasticsearch Licensing Backlash

**DP-06** | Source: [Socket.dev developer survey, 2024](https://socket.dev/blog/developers-burned-by-elasticsearch-license-change-arent-going-back)
> "I'm glad to be off this roller coaster. Where I worked we ported everything OFF ElasticSearch to OpenSearch specifically to get out of the way of ElasticSearch exec's random whims around licensing. At any time they can just change their minds again. It's pretty clear they can't be trusted." — developer comment after Elastic's 2024 AGPL announcement

**DP-07** | Source: [Socket.dev, 2024](https://socket.dev/blog/developers-burned-by-elasticsearch-license-change-arent-going-back)
> Despite Elastic re-introducing open-source licensing (AGPL v3) in August 2024, developer threads are "replete with short posts" expressing that there is no compelling reason to migrate back. Trust was broken.

**DP-08** | Source: [Jo Kristian Bergum on X, 2025](https://x.com/jobergum/status/1948132773450985969)
> "Elastic's license rug pull, meant to hurt Amazon, ended up making OpenSearch the default. Most orgs I talk to run OpenSearch over Elasticsearch. Probably the biggest strategic misfire in the software industry. Also a perfect example of why the freedom to fork matters."

**DP-09** | Source: [CrafterCMS blog, 2024](https://craftercms.com/blog/2024/08/elastics-abandonment-of-open-source-a-cautionary-tale-of-profit-over-principles)
> Elastic's 2021 SSPL relicensing forced a massive exodus to Amazon's fork, OpenSearch — which had 496 contributors and more than 100 million downloads in its first year. The damage to trust was permanent.

**DP-10** | Source: [Pureinsights blog, 2025](https://pureinsights.com/blog/2025/elasticsearch-vs-opensearch-in-2025-what-the-fork/)
> Most organizations that migrated to OpenSearch during the license controversy are staying. Returning to Elasticsearch often requires a complete rebuild — OpenSearch maintains backward compatibility with older Elastic APIs but not vice versa.

---

## 3. Elasticsearch Operational Pain in Production

**DP-11** | Source: [TigerData blog, 2025](https://www.tigerdata.com/blog/10-elasticsearch-production-issues-how-postgres-avoids-them)
> "Stop-the-world" JVM GC pauses can freeze an Elasticsearch node for seconds. If the pause lasts longer than 30 seconds, the cluster assumes the node is dead and starts moving data — causing a cascade.

**DP-12** | Source: [TigerData blog, 2025](https://www.tigerdata.com/blog/10-elasticsearch-production-issues-how-postgres-avoids-them)
> The Mapping Explosion problem: Dynamic mapping automatically creates a new field for every unique key in incoming semi-structured data. If new fields are constantly created, cluster state grows exponentially — an operational liability that scales with your data's variety.

**DP-13** | Source: [TigerData blog, 2025](https://www.tigerdata.com/blog/10-elasticsearch-production-issues-how-postgres-avoids-them)
> Cluster State Bloat: A bloated cluster state slows all master node operations, leading to Cluster State Update Timeouts where the master cannot commit changes within the default 30-second window, making the entire cluster unresponsive.

**DP-14** | Source: [TigerData blog, 2025](https://www.tigerdata.com/blog/10-elasticsearch-production-issues-how-postgres-avoids-them)
> Split-Brain Risk: Running a cluster with only two nodes is inherently unsafe. The remaining node cannot form a quorum majority if one fails — blocking all writes. Many teams only discover this in their first real failure event.

**DP-15** | Source: [TigerData blog, 2025](https://www.tigerdata.com/blog/10-elasticsearch-production-issues-how-postgres-avoids-them)
> Oversharding "Recovery Storm": Accumulating tens of thousands of small shards (under 1GB each) creates a recovery storm when a node restarts — leaving the cluster in Yellow or Red state for hours.

**DP-16** | Source: [TigerData blog, 2025](https://www.tigerdata.com/blog/10-elasticsearch-production-issues-how-postgres-avoids-them)
> Merge Storm: When segments created by frequent refreshes accumulate faster than the background merge process can handle, Elasticsearch intentionally stalls indexing threads — causing intermittent drops in indexing throughput.

**DP-17** | Source: [TigerData blog, 2025](https://www.tigerdata.com/blog/10-elasticsearch-production-issues-how-postgres-avoids-them)
> Deep Pagination: Every node fetches and ranks ALL results, then discards earlier ones. Coordinator combines results from all nodes. Queries that return fast in dev can take seconds in production at scale.

**DP-18** | Source: [Pureinsights blog, 2025](https://pureinsights.com/blog/2025/top-7-elasticsearch-pitfalls-and-how-to-avoid-them/)
> Upgrading Elasticsearch is "a torturous and slow process" — you must upgrade the cluster one node at a time. Major version upgrades require full cluster restart with risks of downtime and data loss.

**DP-19** | Source: [Sirius Open Source blog, 2025](https://www.siriusopensource.com/en-us/blog/problems-and-operational-weaknesses-elasticsearch)
> Elasticsearch is marketed as "schema-less" or flexible, but managing schema at scale is one of its most rigid and unforgiving aspects. The 1,000-field soft limit per index, when raised, causes severe performance degradation.

**DP-20** | Source: [Opster guide, 2025](https://opster.com/guides/elasticsearch/operations/elasticsearch-out-of-memory/)
> OOM errors in Elasticsearch occur when the JVM exhausts heap space — can cause nodes to crash, cluster instability, data unavailability, and potential data loss. GC thrashing precedes fatal crashes with a flatline pattern at 75–90% heap usage.

**DP-21** | Source: [Elastic Community Forum (discuss.elastic.co)](https://discuss.elastic.co/t/elasticsearch-getting-killed-by-the-oom-killer-because-an-out-of-memory/326218)
> Active thread: "Elasticsearch getting killed by the OOM killer because an out of memory" — multiple engineers in 2024–2025 reporting nodes dying due to Linux OOM killer intervention.

**DP-22** | Source: [GitHub elastic/elasticsearch issue #41337](https://github.com/elastic/elasticsearch/issues/41337)
> Long-standing open issue: "Nodes failing with OutOfMemoryError after about a week of uptime" — intermittent and hard to reproduce, making it particularly dangerous in production.

**DP-23** | Source: [X.com / @0xlelouch_, 2025](https://x.com/0xlelouch_/status/1990412969587650711)
> "Elasticsearch can easily handle 1TB / 16M rows. The real problem is how you model + index it." — Even Elasticsearch advocates acknowledge the heavy burden placed on engineers to model data correctly to avoid collapse.

**DP-24** | Source: [OneUptime blog, Feb 2026](https://oneuptime.com/blog/post/2026-02-06-elasticsearch-jvm-heap-gc-threadpool/view)
> Published monitoring guide for JVM heap usage, GC pause time — the fact this guide exists in 2026 reflects ongoing production pain that teams need active monitoring to manage.

**DP-25** | Source: [Elasticsearch Split Brain alert (IBM Instana)](https://www.ibm.com/support/pages/resolving-elasticsearch-split-brain-situation-alert-instana)
> IBM's enterprise monitoring platform (Instana) has a built-in alert specifically for Elasticsearch split-brain situations — acknowledging the risk is real enough to need automated detection.

**DP-26** | Source: [e-mc2.net blog](https://e-mc2.net/blog/elasticsearch-garbage-collection-hell/)
> "Elasticsearch in garbage collection hell" — documented production case study of a cluster entering a GC death spiral: collector runs continuously, fails to reclaim sufficient memory, and the cluster degrades until it crashes.

---

## 4. Elasticsearch Migration Stories

**DP-27** | Source: [InfoQ, August 2025](https://www.infoq.com/news/2025/08/instacart-elasticsearch-postgres/)
> **Instacart** replaced Elasticsearch with PostgreSQL (pg_trgm + pgvector) for production search. Result: **~80% savings on storage and indexing costs**, 10x reduction in write workload.

**DP-28** | Source: [Instacart Engineering Blog, 2025](https://tech.instacart.com/how-instacart-built-a-modern-search-infrastructure-on-postgres-c528fa601d54)
> Instacart's Elasticsearch implementation did not scale due to their denormalized data model. Frequent partial writes to update billions of items for price/inventory changes caused the indexing load to overwhelm the cluster — fixing erroneous data took **days**.

**DP-29** | Source: [Instacart Engineering Blog, 2025](https://tech.instacart.com/how-instacart-built-a-modern-search-infrastructure-on-postgres-c528fa601d54)
> The Postgres-based search ended up being **twice as fast** by pushing logic down to the data layer rather than pulling data up to the application layer. Eliminated dual-system complexity entirely.

**DP-30** | Source: [Firebolt blog on Instacart migration, 2025](https://www.firebolt.io/blog/postgres-vs-elasticsearch-the-unexpected-winner-in-high-stakes-search-for-instacart)
> The Instacart migration was "unexpected" by industry standards — their case validated that PostgreSQL + pgvector can outperform Elasticsearch in both cost and speed for real-world production search.

**DP-31** | Source: [Loadsmart Engineering blog, 2024–2025](https://engineering.loadsmart.com/blog/elastic-cloud-migration/)
> Loadsmart migrated from **AWS Elasticsearch to Elastic Cloud** — choosing to pay Elastic directly rather than use the AWS fork. Primary motivation: staying on the original codebase with official support rather than fragmentation risk.

**DP-32** | Source: [Zalando Engineering Blog, Nov 2023 / ongoing 2025](https://engineering.zalando.com/posts/2023/11/migrating-from-elasticsearch-7-to-8-learnings.html)
> Zalando documented extensive pitfalls migrating from Elasticsearch 7.17 to 8.x — a within-Elastic migration with significant friction. Ongoing 2025 analysis of postmortems includes Elasticsearch incidents.

**DP-33** | Source: [Quesma blog postmortem, 2025](https://quesma.com/blog/database-gateway-postmortem/)
> "A postmortem on our $2.5M database gateway: lessons from pilot purgatory" — Quesma's story of building a gateway to abstract Elasticsearch query complexity; the pain drove an entire product category.

---

## 5. OpenSearch vs Elasticsearch Fragmentation

**DP-34** | Source: [Squareshift analysis, 2025](https://www.squareshift.co/post/opensearch-vs-elasticsearch-key-differences-for-technical-leaders-in-2025)
> Independent benchmarks 2024–2025: Elasticsearch 40–140% faster in some complex query scenarios (text querying, sorting). But for most standard use cases, performance is comparable — OpenSearch being based on ES 7.10.2 fork.

**DP-35** | Source: [Dattell blog, 2025](https://dattell.com/data-architecture-blog/opensearch-vs-elasticsearch-in-2025-whats-changed-and-what-hasnt/)
> Migrating from OpenSearch back to Elasticsearch often requires a complete rebuild. Plugin incompatibility means transitions require substantial re-architecture, particularly in security, monitoring, and visualization layers.

**DP-36** | Source: [Uptrace comparison, 2025](https://uptrace.dev/comparisons/opensearch-vs-elasticsearch)
> OpenSearch includes all features (advanced security, alerting, cross-cluster replication) at no cost. Elasticsearch gates these behind paid tiers. For cost-constrained teams, this is a decisive factor.

**DP-37** | Source: [SigNoz comparison, 2025](https://signoz.io/comparisons/elasticsearch-vs-opensearch/)
> Five years after the fork: the ecosystem has meaningfully diverged. Teams locked into one path face significant migration costs to switch. The community is split and unlikely to reconverge.

---

## 6. Vector Database Production Pain

**DP-38** | Source: [Shaped.ai blog, 2025](https://www.shaped.ai/blog/best-vector-database-alternatives-in-2025)
> DIY with a raw vector DB is costly and complex in 2025 — requires ongoing ML, data engineering, and ops resources. Scaling infrastructure, continuous retraining, and monitoring require a full ML + infra team.

**DP-39** | Source: [Actian blog, 2025](https://www.actian.com/blog/databases/how-to-evaluate-vector-databases-in-2026/)
> Most vector database benchmarks are vendor-optimized and fail to reflect real-world production conditions (concurrency, filtering, continuous ingestion). Key production risks: tail latency (P95/P99), performance degradation over time, rising TCO at scale.

**DP-40** | Source: [Reddit engineering, cited in Medium 2025](https://medium.com/@reliabledataengineering/vector-databases-are-dead-vector-search-is-the-future-heres-what-actually-works-in-2025-e7c9de0490a7)
> Reddit's engineering team, managing 340M+ vectors, identified **metadata filtering as the primary performance bottleneck** in their 2025 deployment. As concurrent users grew, the database spent more time resolving metadata filters than calculating similarity distances.

**DP-41** | Source: [Medium / Reliable Data Engineering, Sept 2025](https://medium.com/@reliabledataengineering/vector-databases-are-dead-vector-search-is-the-future-heres-what-actually-works-in-2025-e7c9de0490a7)
> "By September 2025, it was official: the age of standalone vector databases was over." Teams discovered that pure vector search alone doesn't handle enterprise reasoning over relationships.

**DP-42** | Source: [DEV Community / actiandev, 2026](https://dev.to/actiandev/whats-changing-in-vector-databases-in-2026-3pbo)
> Market shift: teams are moving away from specialized "Vector-Only" databases to integrated "Vector-Also" platforms (PostgreSQL + pgvector). The operational burden of managing a separate vector database proved unjustifiable for most workloads.

---

## 7. Pinecone Cost Pain

**DP-43** | Source: [OpenMetal blog, 2025](https://openmetal.io/resources/blog/when-self-hosting-vector-databases-becomes-cheaper-than-saas/)
> "One company's Pinecone bill started at $50, then $380, and last month hit $2,847." — Real cost escalation story. For one workload with ~50,000 queries/day, Pinecone cost ~$420/month.

**DP-44** | Source: [OpenMetal blog, 2025](https://openmetal.io/resources/blog/when-self-hosting-vector-databases-becomes-cheaper-than-saas/)
> Pinecone costs do not scale linearly — they rise **disproportionately** with traffic. AI startups are hitting a wall with vector database costs that don't align with businesses needing predictable infrastructure budgets.

**DP-45** | Source: [DEV Community, 2025](https://dev.to/dineshelumalai/s3-vectors-90-cheaper-than-pinecone-our-migration-guide-327c)
> AWS S3 Vectors claims **90% lower cost** than Pinecone for equivalent workloads — the claim itself signals how expensive Pinecone has become relative to infrastructure alternatives.

**DP-46** | Source: [Supabase blog, 2024](https://supabase.com/blog/pgvector-vs-pinecone)
> pgvector vs Pinecone cost and performance comparison: for under 5 million vectors with existing PostgreSQL infrastructure, pgvector costs **nothing extra** — Pinecone charges per vector stored and per query.

**DP-47** | Source: [LiquidMetal AI blog, 2025](https://liquidmetal.ai/casesAndBlogs/vector-comparison/)
> Pinecone's namespace limits (up to 100,000 on standard plans) and per-namespace performance degradation become hard constraints as tenant count grows — forcing expensive plan upgrades.

**DP-48** | Source: [Aloa.co comparison, 2025](https://aloa.co/ai/comparisons/vector-database-comparison/pinecone-vs-weaviate-vs-chroma)
> Developers report "index corruption, unexpected cost increases, and slow query responses" when projects scale to production on Pinecone and other managed vector databases.

---

## 8. Weaviate Memory Scaling Pain

**DP-49** | Source: [Weaviate Community Forum](https://forum.weaviate.io/t/over-memory-consumption-of-weaviate/1002)
> User: importing objects and Weaviate memory consumption is "over" — HNSW index must be stored entirely in memory, consumption directly tied to dataset size with little compression by default.

**DP-50** | Source: [Weaviate Community Forum](https://forum.weaviate.io/t/very-high-memory-usage-even-after-low-vector_cache_max_objects/2732)
> User with 60 million objects set `vector_cache_max_objects` to 1,000,000 — still requires 130GB RAM. OOM issues in production despite trying to limit cache.

**DP-51** | Source: [Weaviate Community Forum](https://forum.weaviate.io/t/weaviate-docker-container-consume-35gb-of-memory-with-only-100k-records/2246)
> "Weaviate docker container consume 35GB of memory with only 100k records" — 768-dimensional vectors, idle instance at 33.75GB. Prohibitive for most self-hosted deployments.

**DP-52** | Source: [Weaviate Community Forum](https://forum.weaviate.io/t/high-memory-usage-after-upgrading-weaviate-to-version-1-25/4028)
> Upgrade from v1.18 to v1.25 caused unexpectedly high memory consumption even with additional capacity allocation — version upgrades introducing regressions in resource usage.

**DP-53** | Source: [GitHub weaviate/weaviate issue #4572](https://github.com/weaviate/weaviate/issues/4572)
> "Poor performance with scaling" — user with 18 million objects, 256GB RAM, 128 cores experiencing **5–15 second query latencies** for new queries. Scale creates fundamental performance cliffs.

**DP-54** | Source: [GitHub langgenius/dify issue #14206](https://github.com/langgenius/dify/issues/14206)
> "Weaviate is using too much memory" — reported in Dify (AI app builder) context, showing the problem percolates into downstream products that use Weaviate as a default vector backend.

---

## 9. Algolia Cost Pain (Search-as-a-Service)

**DP-55** | Source: [Meilisearch blog, June 2025](https://www.meilisearch.com/blog/algolia-pricing)
> Algolia's per-request and per-record billing is "extremely friendly at low volume but can spike sharply with traffic, bots, or inefficient indexing." Reviewers explicitly complain about "huge fees" once they exhaust the free plan.

**DP-56** | Source: [Typesense comparison page, 2025](https://typesense.org/typesense-vs-algolia-vs-elasticsearch-vs-meilisearch/)
> Teams migrating from Algolia to Typesense cite cost spikes as record counts and traffic scale. Cost savings up to **96%** reported in some cases compared to Algolia's pricing.

**DP-57** | Source: [Hacker News, 2021 — still cited in 2025 discussions](https://news.ycombinator.com/item?id=28884148)
> "Algolia is insanely expensive. Stay far away unless you have very few customers" — HN comment with hundreds of upvotes; still referenced in 2025 comparison articles as capturing the community consensus.

**DP-58** | Source: [Meilisearch cost comparison, 2025](https://www.meilisearch.com/blog/algolia-pricing)
> A $20/month DigitalOcean droplet running Typesense is **7.6x cheaper** than Algolia. Meilisearch Cloud at $59/month for 250k records and 1M searches is **8.9x cheaper** than Algolia equivalent.

---

## 10. Broader Search/Database Reliability & Industry Signals

**DP-59** | Source: [canartuc.com, 2025](https://www.canartuc.com/database-disasters-2024-2025-eight-production-failures-and-how-to-survive-them/)
> "Database Disasters 2024–2025: Eight Production Failures" — documented in detail. Google Cloud accidentally deleted an entire customer account (UniSuper). DynamoDB 15+ hour outage due to DNS race condition. 60% of data operations experienced an outage in the past 3 years.

**DP-60** | Source: [SolarWinds State of Database Management 2025](https://www.solarwinds.com/blog/the-state-of-database-burnout-ai-battle-for-balance)
> DBAs spend an average of **27 hours per week** — more than half their workweek — on reactive tasks: responding to tickets, restoring backups, patching systems. Search infrastructure contributes heavily to this burden.

**DP-61** | Source: [ITIC 2024 survey, referenced 2025](https://www.n-able.com/blog/true-cost-of-downtime)
> Over 90% of large and mid-size enterprises report a single hour of downtime costs **$300,000+** on average. 4 in 10 enterprises: $1 million or more per hour. Search infrastructure downtime is no longer a minor incident.

**DP-62** | Source: [Medium / Engineering Playbook, Jan 2026](https://medium.com/engineering-playbook/vector-database-seemed-essential-postgresql-pgvector-was-enough-c528fa601d54)
> "Vector Database Seemed Essential. PostgreSQL pgvector Was Enough." — Published engineering retrospective. The standalone vector DB was added under pressure, then removed when teams realized pgvector handled their actual workload.

**DP-63** | Source: [DEV Community blog, 2025](https://dev.to/klement_gunndu_e16216829c/vector-databases-guide-rag-applications-2025-55oj)
> ~80% of RAG use cases (embeddings under 2M vectors) do not require a specialized vector database. Standalone silos introduce more operational friction than they offer in performance gains for most teams.

**DP-64** | Source: [pgvector benchmarks, Timescale/pgvectorscale, 2025](https://dbadataverse.com/tech/postgresql/2025/12/pgvector-postgresql-vector-database-guide)
> PostgreSQL's pgvectorscale benchmarked at **471 QPS** vs Qdrant's **41 QPS** at 99% recall on 50M vectors — a 10x+ performance advantage for specific workload types, accelerating the shift back to Postgres.

**DP-65** | Source: [Qdrant GitHub releases, 2025](https://github.com/qdrant/qdrant/releases)
> Qdrant production bug fixes in 2025 include: panic at startup on old clusters, broken Raft consensus snapshots, corrupting WAL with broken flush edge cases, incorrect rescoring with binary quantization, data race in shard transfers. Active bug surface in a production-critical component.

**DP-66** | Source: [X.com / @elastic, Feb 2025](https://x.com/elastic/status/1890419479189831959)
> Elastic's official account promoting AutoOps: "Are you experiencing long-running search queries? AutoOps has your back!" — Elastic itself acknowledges long-running search queries as a persistent, common enough problem to build a product around.

**DP-67** | Source: [Blocksandfiles, Dec 2025](https://blocksandfiles.com/2025/12/01/pinecone-dedicated-read-nodes/)
> Pinecone rolls out dedicated read nodes to address performance isolation complaints — confirming that production read/write contention was a real enough issue to require architectural changes.

**DP-68** | Source: [VentureBeat, 2025](https://venturebeat.com/data-infrastructure/aws-claims-90-vector-cost-savings-with-s3-vectors-ga-calls-it-complementary)
> AWS launches S3 Vectors GA claiming **90% vector cost savings** over specialized databases — a direct market response to widespread complaints about vector DB pricing across Pinecone, Weaviate, and Qdrant Cloud.

---

## Summary: Pain Patterns

| Category | Signal Strength | Primary Complaint |
|---|---|---|
| Elasticsearch cost | Very High | 30% price increase 2025; $100k+/mo enterprise |
| Elasticsearch licensing trust | Very High | Burned by 2021 SSPL change; won't go back |
| Elasticsearch JVM/GC | High | OOM crashes, GC pauses, recovery storms |
| Elasticsearch schema ops | High | Mapping explosion, shard management burden |
| Elasticsearch upgrades | High | Torturous, risky, node-by-node process |
| Vector DB cost (Pinecone) | Very High | Non-linear cost growth, $2,847/month spike stories |
| Weaviate memory | High | 35GB for 100k records; OOM at scale |
| Vector DB benchmarks misleading | High | Vendors optimize for benchmark, not production |
| Algolia cost | High | 7–9x premium over self-hosted alternatives |
| General DB reliability | Medium-High | 60% ops experienced outage in 3 years |

## Key Migration Stories Documented

1. **Instacart** — Elasticsearch → PostgreSQL + pgvector. 80% cost savings, 10x write reduction, 2x search speed. (Aug 2025)
2. **Loadsmart** — AWS Elasticsearch → Elastic Cloud. Managed complexity trade vs. cost.
3. **Zalando** — Elasticsearch 7→8 migration with documented pitfalls (2023, ongoing learnings 2025).
4. **Unnamed AI startup** — Pinecone $50 → $2,847/month cost escalation (industry-wide pattern).
5. **Reddit** — 340M vector deployment hitting metadata filtering bottlenecks (2025).
6. **Broad market** — Algolia → Typesense/Meilisearch migrations with 96% cost savings cited.

---

*Data collected: April 2025. Sources include Twitter/X posts, Hacker News comments, GitHub issues, engineering blog posts, and community forum threads. All 68 data points reference publicly accessible sources linked inline.*
