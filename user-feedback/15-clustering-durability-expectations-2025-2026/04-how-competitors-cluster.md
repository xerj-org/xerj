# How Competitors Implement Clustering — Architecture Comparison

## Sources: Source code, documentation, technical deep-dives

---

## Elasticsearch — ZenWithTerms (Custom Protocol)

**Consensus:** Custom quorum-based voting (NOT Raft)
- `Coordinator.java` (117KB) — main coordination
- `VoteCollection` — dual-quorum: lastCommitted AND lastAccepted configs
- `PersistedState` — term + state persisted to disk
- Master publishes state → followers ACK → master commits

**Routing:** Murmur3 hash, `hash(id) % num_shards` (IMMUTABLE shard count)

**Replication:** Custom protocol (`ReplicationOperation.java`)
- Primary → parallel fan-out to replicas → wait for quorum

**Search:** Scatter-gather
- Fan out to ALL shards → collect on coordinator → merge in memory

**Failure modes:** Split-brain, master bottleneck, shard explosion, O(N²) cache stats

---

## Qdrant — Raft Consensus (Rust)

**Consensus:** Raft (via `raft-rs` or custom)
- All nodes participate in Raft
- Leader handles writes, replicates to followers
- 3+ nodes required for fault tolerance

**Routing:** Collection-based sharding
- User-defined or automatic shard placement
- Shard transfer between nodes supported

**Replication:** Raft log replication
- Write committed when majority acknowledges

**Search:** Fan-out to nodes holding relevant shards

**Failure modes:** WAL corruption, shard transfer races, 3-node minimum

---

## Milvus — Disaggregated Architecture

**Consensus:** etcd (external) for metadata
**Message Queue:** Pulsar/Kafka for log stream
**Storage:** MinIO/S3 for segments

**Architecture:**
```
                    etcd (metadata)
                         │
     ┌───────────────────┼───────────────────┐
     │                   │                   │
  Proxy nodes      Query nodes        Data nodes
  (routing)        (search)           (ingest)
     │                   │                   │
     └───────────────────┼───────────────────┘
                         │
                    MinIO / S3
                    (segments)
```

**Routing:** Consistent hashing with virtual channels

**Failure modes:** 4 external dependencies, operational complexity

---

## Quickwit — S3-Native Search (Rust)

**Consensus:** None — stateless searchers, S3 is the source of truth
**Storage:** S3-native, segments are immutable files

**Architecture:**
```
Ingest → Split (in-memory) → Write to S3 → Index
Search → Read segment metadata → Range-read from S3 → Score
```

**Key insight:** No consensus needed if storage is the source of truth (S3 is already replicated/durable)

**Failure modes:** S3 latency, cold query performance

---

## TiKV — Region-Based Raft

**Consensus:** Raft per region (shard)
- Each region has its own Raft group (3 replicas)
- Regions auto-split when too large (default 96MB)
- Regions auto-merge when too small

**Placement Driver (PD):** Separate metadata service
- Tracks all regions, nodes, and their locations
- Schedules region splits, merges, transfers

**Key insight:** Raft per shard, not per cluster — scales horizontally

---

## CockroachDB — Multi-Raft

**Consensus:** One Raft group per range (shard)
- Ranges = contiguous key spans
- Auto-split at 512MB
- Each range is a Raft group with 3 replicas

**Key insight:** Same as TiKV — Raft at the shard level, not cluster level

---

## What the Best Modern Approach Is (2025-2026 Consensus)

### Pattern: S3-Native + Embedded Raft + Stateless Compute

```
Metadata:  Embedded Raft (3-node consensus) — tiny state (<10MB)
           NOT external etcd (Milvus), NOT custom protocol (ES)
           
Compute:   Stateless query/ingest nodes
           Scale up/down freely, no data migration
           Cache segments locally on NVMe for hot data
           
Storage:   S3/GCS/Azure Blob — immutable segments
           Already durable, already replicated, already global
           No need for application-level replication
           "Moving a shard" = updating a pointer (milliseconds)
           
Ingest:    WAL → memtable → flush to S3 segment
           WAL replicated via Raft for durability before S3 flush
           
Search:    Read segment metadata from Raft state
           Range-read segments from S3 (or local NVMe cache)
           Stream-merge results (bounded memory)
```

### Why This Wins:

| | ES | Milvus | Qdrant | S3-Native (XERJ v2) |
|---|---|---|---|---|
| External deps | Java, Lucene | etcd, Pulsar, MinIO | None | None |
| Rebalancing | Move data (hours) | Move data | Move data | Move pointer (ms) |
| Add node | Shard migration storm | Capacity planning | Shard transfer | Start serving (instant) |
| Storage cost | 3× (replication) | 1× (S3) | 3× (replication) | 1× (S3, already replicated) |
| Durability | Translog + Lucene | S3 | Raft WAL | S3 + Raft WAL |
| Consensus | Custom (complex) | External etcd | Raft | Embedded Raft |
