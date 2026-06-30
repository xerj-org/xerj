# Shard Management Pain Points

## Severity: HIGH | Frequency: HIGH

---

## Core Complaints

### Oversharding
- Each shard is a full Lucene index with own file handles, memory structures, CPU overhead
- Tens of thousands of small shards (<1GB) is a common anti-pattern
- Daily index rotation for logging exacerbates the problem
- Rule of thumb: 10-50GB per shard, ~20 non-frozen shards per GB of heap per node
- A test creating ~1 billion empty shards caused immediate OOM

### Undersharding
- Too few shards bottleneck query parallelism and slow recovery
- No universal formula; depends on data volume, query patterns, hardware
- Requires empirical testing -- guesswork

### Shard Count is Immutable
- Cannot change shard count post-creation without full index rebuild and reindexing
- A fundamentally irreversible decision made at design time
- Wrong initial choice requires reindexing potentially hundreds of TBs

### Shard Imbalance
- Elasticsearch balances by shard COUNT, not shard SIZE
- A cluster can look numerically balanced while deeply unbalanced in workload
- Some nodes handle gigabyte shards while others handle tiny ones
- Lacks visibility into per-shard resource utilization

### Unassigned Shards
- Common cause of Yellow/Red cluster status
- Triggered by: disk watermarks (85%), node failures, insufficient replicas, configuration errors
- Diagnosing requires arcane API calls and deep knowledge

### Disk Watermarks
- Low watermark (85%): no new shards allocated to node
- High watermark (90%): shards actively relocated away
- Flood stage (95%): ALL indices on node become read-only
- Recovery requires manual intervention (`index.blocks.read_only_allow_delete`)
- Abrupt disk exhaustion from large merge/snapshot temporary files

---

## XERJ.ai Response
- **No shards in M1.** Single-node architecture eliminates the entire concept.
- Segments are the unit of organization (like Lucene), but managed automatically
- No shard count decision at index creation time
- No shard rebalancing, no watermarks, no unassigned shard debugging
- Segment model designed with future sharding in mind (segment IDs, routing keys for M2)
- Background merge scheduler handles segment lifecycle automatically

## Sources
- Elastic Blog: How Many Shards Should I Have
- Datadog: 5 Performance and Scaling Problems
- Tiger Data: 10 Production Issues
- GitHub Issue #77466: Large shard count scalability
