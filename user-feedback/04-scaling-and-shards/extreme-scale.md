# What Breaks at Extreme Scale

## Source: Meltwater (3PB), Uber (800B docs), Netflix (3,500 hosts), Botify (120B docs), Slack

---

## Scale Reference Points

| Company | Scale | Key Problem |
|---------|-------|-------------|
| **Meltwater** | 3PB, 1,100 nodes, 90K shards | Rolling restarts took 2 MONTHS; cluster state >200MB |
| **Uber** | 800B docs, 1.5M writes/sec | Built custom search engine (Sia) to replace ES |
| **Netflix** | 3,500 hosts, 150 clusters | Built Raigad sidecar for management |
| **Botify** | 600TB, 69 nodes, 120B docs | 2-3 node failures/month pre-optimization |
| **Slack** | High-volume logging | Built KalDB to replace ES (no workload isolation) |

---

## What Breaks

### Cluster State Explosion
- Meltwater: cluster state exceeded 200MB
- Every change pushed to all 1,100 nodes
- Even with TCP compression (~3MB/node), single change = 1.3GB across 430 nodes
- Cross-AZ transfers compound the problem

### Master Node Bottleneck
- Many operations on single JVM thread, growing linearly with shard count
- At 90,000 shards: node join/leave detection took minutes per event
- Master at 100% CPU is commonly reported
- Pending task queue freezes entire cluster

### 2-Billion Document Hard Limit
- Lucene: max 2,147,483,519 docs per shard
- When hit, index turns RED, all read/write operations fail
- Integer overflow constraint that cannot be configured away

### O(N^2) Shard Scaling
- Query cache stats computation: O(N^2) relative to shard count
- Transport worker threads: O(shards) work synchronously during broadcasts
- Authorization for internal requests degrades significantly
- Node startup "can take minutes" with many indices
- Default limit: 1,000 non-frozen shards per node

### Shard Relocation Storms
- Meltwater: moving largest shards took hours per shard
- Cascading storms: shards bouncing back and forth for hours
- One report: shard reassignment took over 5 days
- Default recovery throttled to 40MB/s per node

### Rolling Restart at Scale
- Meltwater 1,100 nodes: up to 2 months for rolling restart
- After upgrading to ES 7 (600 nodes): restarts in 1 day
- Each node restart triggers reallocation + cluster state updates + recovery

### JVM Heap at Scale
- Static segment metadata consumed 40-50% of heap in ES 5.x
- Meltwater ran 64GB heaps despite 31GB recommendation
- At 75%+ pressure, GC consumes CPU unavailable for queries
- GC pauses must stay under 50ms (young) / 1s (old) or nodes disconnect

### Circuit Breaker Cascades
- At 95% JVM usage: parent breaker triggers, returns HTTP 429
- One bad query can trip breakers across multiple nodes simultaneously
- High-cardinality aggregations and large size values are common triggers

### Segment Merge I/O
- Force-merging requires 3x shard size in temporary disk space
- Segments >5GB ineligible for regular merges (soft-deleted docs accumulate)
- Heavy merging makes cluster unresponsive for both indexing and search

### Coordinating Node Fan-Out
- Scatter-gather: coordinator fans out to all shards, merges results
- With 98 data nodes and 3 coordinators: massive bottleneck
- Complex aggregations across thousands of shards: coordinator must hold partial results from every shard in memory

### No Workload Isolation
- Slack: indexing and querying on same nodes, no resource boundaries
- Single node failure delays ALL data ingestion
- Built KalDB specifically to solve isolation

### Snapshot Operations
- Meltwater: snapshot deletion so slow they alternated S3 buckets every 4 months
- Finalization is memory-intensive, blocks master node

### Version Upgrade at Scale
- Meltwater stuck on custom ES 1.7.6 (no rolling upgrade path)
- Migration required two parallel clusters: 1,100 old + 320 new
- Gradual traffic migration with no rollback procedure

---

## XERJ.ai Response (M1 Scope)

M1 is single-node, targeting the ICP (1TB/day, 20+ node ES clusters). At this scale:
- No cluster state to broadcast
- No master bottleneck
- No shard management
- No rolling restart complexity
- Segment model designed with future sharding in mind (segment IDs, routing keys)

The extreme-scale stories above validate that ES's architecture has a ceiling. XERJ.ai's clean-sheet design avoids these traps by not inheriting Lucene's segment model or JVM constraints.

## Sources
- Meltwater: 3PB ES upgrade blog
- Uber Engineering: Evolution of Search Platform
- Netflix: Open Source (Raigad)
- Botify Engineering: ES optimization
- Slack Engineering: KalDB
- GitHub #77466: Shard count scalability
- Elastic Blog: Six Ways to Crash
