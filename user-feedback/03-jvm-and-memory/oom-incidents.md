# Out of Memory Incidents (Forum + GitHub)

## Severity: CRITICAL | Frequency: HIGH
OOM is the single most common operational failure across all ES versions.

---

## Documented OOM Incidents

### 33GB RAM Consumed on Empty Install
> "I downloaded Elasticsearch to try it and it ate 33GB of memory from the start without anything in it yet. I decided not to use it because of this."
> -- Elastic Forum user "JorgeCarousel"

### OOM Killed Entire 3-Node Cluster
- ES 5.5.2 on AWS, 3-node cluster
- One node OOM right before planned go-live
- Single shard with zero replicas = all data on one node
- Entire cluster failed
- Forum #109207

### 9.6 Billion Docs: Constant OOM on 4 Data Nodes
- 7-node cluster, 64GB RAM, 30GB heap
- Heap fills to 75%, GC kicks in, then times out on 3rd/4th cycle
- Root cause: deeply nested aggregation (top 10K companies x top 500K destination IPs)
- All 4 data nodes going down "after few hours of restart constantly"
- Forum #173669

### Doubling RAM Doesn't Fix It
- ES 7.17.5: OOM errors on one cluster
- Even after doubling RAM from 56GB to 112GB and expanding 20 → 26 nodes
- Problem persisted on specific data nodes
- Forum #334985

### Memory Leak After 7.14.1 Upgrade
- Memory consumption steadily increasing after upgrading from 7.9.2
- Previous version worked well for months
- GitHub #80790, fixed in 7.15.2

### Circuit Breaker Never Releases Memory
- Large multiget request fails → allocated memory never released
- Circuit breaker counter grows indefinitely
- Node becomes unusable
- GitHub #27525

### Elasticsearch Requires 85x More RAM Than Alternatives
- ES baseline: ~4.5GB RAM
- Alternative (Meilisearch): ~53MB
- 85x difference documented in benchmarks

---

## The Heap Size Trap

```
Too small (< 16GB)  → OOM crashes
Too large (> 31GB)  → Compressed oops disabled, LESS effective memory
Sweet spot (30-31GB) → Works but GC pauses grow with heap size
                       Each shard adds fixed overhead regardless of data
```

> "Heaps beyond 32g can be beneficial but you'd have to go for 64g+, 32-48g is a dead zone."
> -- HN user "DmitryOlshansky"

> "Elasticsearch is very memory-intensive, and it's difficult to know exactly how much memory it will actually use."
> -- HN user "atombender"

> "Elasticsearch would randomly bloat up to occupy a huge amount of RAM. Restarting it would make it work for a few days."
> -- HN user "fareesh"

---

## XERJ.ai Response
- **No JVM. No heap. No OOM from GC.**
- Rust: memory is freed when ownership ends, deterministically
- mmap for segment access: OS manages page cache, no application heap
- Arena allocators for query-scoped allocations: freed in bulk
- No 31GB ceiling, no compressed oops, no heap tuning
- Target: <500MB RSS for 1M docs (vs ES's 2-4GB)

## Sources
- Elastic Forum: #310891, #109207, #173669, #334985
- GitHub: #80790, #27525
- HN: #22396918, #22685831
