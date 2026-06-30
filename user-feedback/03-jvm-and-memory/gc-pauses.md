# JVM Garbage Collection Pauses

## Severity: CRITICAL | Frequency: HIGH

---

## Core Complaints

### Stop-the-World Pauses
- Java GC periodically freezes ALL application threads
- Pause duration correlates with heap size -- larger heaps = longer pauses
- Documented pauses of 20-40 seconds (GitHub #6820, #11371, #13506)
- If pause exceeds 30 seconds, master node ejects the frozen node from cluster
- Ejection triggers cascading shard reallocation ("recovery storm")

### GC Thrashing
- Occurs when heap usage flatlines at 75-90% and collection cycles fail to reclaim enough memory
- Precursor to OOM crashes
- Young generation pool defaults can be severely undersized (2GB of 31GB heap, ratio 1:14 vs recommended 1:2 or 1:3)
- Requires manual JVM option tuning (`-XX:NewRatio`, `-XX:ParallelGCThreads`)

### Heap Size Dilemma
- Must set `-Xms` = `-Xmx` at 50% of available RAM
- Hard cap at ~31GB (compressed oops limit)
- Exceeding 31GB actually REDUCES addressable memory due to pointer decompression
- Undersized heap = OOM. Oversized heap = longer GC pauses. Must be "just right."
- Users often run out of heap before disk

### Version-Breaking GC Changes
- Switching ES versions changes internal memory allocation patterns
- Can break previously stable GC tuning
- One production cluster (1,300 indices, 100TB, 18 nodes) experienced 80-90% CPU spikes after upgrading 5.6 to 6.7 due to changed heap usage
- JDK 20+ memory regression documented (GitHub #99592)

### Circuit Breaker Estimation Failures
- Pre-flight memory estimates can be wrong
- Deep terms aggregation might pass check but expand exponentially during execution
- Exhausts heap and crashes the node
- Setting `size` to Integer.MAX_VALUE can crash a node instantly

### Off-Heap for Vector Search
- ANN graph structures must reside entirely in off-heap RAM
- Spilling to disk causes orders-of-magnitude performance degradation
- No graceful degradation path

---

## GitHub Issues

- #6820: Long GC pauses
- #11371: Groovy script GC pauses
- #13506: GC pause documentation
- #28062: Memory not reclaimed by GC (2.7TB index, 67M docs, 9 nodes)
- #99592: JDK 20+ memory pressure regression
- #10933: Node crashes cause data loss during GC

---

## XERJ.ai Response
- **No JVM. No GC. No pauses. Period.**
- Rust ownership model manages memory at compile time
- Arena allocators for query-scoped memory (freed in bulk, no GC)
- mmap for segment access -- OS page cache manages memory, not application
- No heap size to tune, no compressed oops limit, no `-Xms`/`-Xmx`
- Cold start <5s vs ES's 30-60s (no JVM class loading)
- <500MB RSS for 1M docs vs ES's 2-4GB heap

## Sources
- GitHub Issues: elastic/elasticsearch
- Elastic Blog: A Heap of Trouble
- e-mc2.net: Elasticsearch Garbage Collection Hell
- Elastic Blog: Six Ways to Crash Elasticsearch
- Elastic Search Labs: Heap Size and JVM GC
