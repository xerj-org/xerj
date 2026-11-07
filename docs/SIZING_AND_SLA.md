# Sizing and service levels

What hardware a XERJ node needs for a given amount of data, the latency,
throughput and recovery numbers we have measured, and what XERJ does **not**
promise yet.

- **Last updated:** 2026-11-05.
- **Measured against:** `v1.0.0-rc.101` (`main` at `dfbab650d`), x86_64 release build.
- **Which source wins:** [ROADMAP.md](../ROADMAP.md) is authoritative on what ships.
  If this page and the roadmap disagree, the roadmap wins and the disagreement is a bug.
- **Older numbers:** the *Capacity planning* block on
  [landing/docs/operations.html](../landing/docs/operations.html) predates these runs
  (it gives ~400 MB idle and memtables bounded by `flush_size_mb`). Where the two
  differ, use this page; restating that block is a roadmap item.

## No formal SLA

XERJ has **no formal SLA** today: no uptime percentage, no latency guarantee,
no support response time. The numbers below are measurements from single runs
on one machine, published so you can size a deployment. They are not
commitments.

Multi-node clusters are **not production-ready** (see
[Clusters](#clusters-what-works-and-what-does-not) below and issue
[#1170](https://github.com/xerj-org/xerj/issues/1170)). Run a single node, with
snapshots for backup, if you need predictable behaviour today.

## How these numbers were measured

- **Host:** one x86_64 machine, 32 cores, 119 GB RAM, local NVMe, Linux.
  Other builds were running on the same host, so **wall-clock times vary from
  run to run** (the same 1 GB load took 102 s on one run and 186 s on another).
  CPU-seconds and peak RSS are the stable numbers; plan with those.
- **Data:** English Wikipedia as JSONL, about 3.0 KB per document.
  1 GB raw = 284,158 docs; 5 GB raw = 1,649,948 docs.
  The 10 GB row comes from an earlier aarch64 run on a different machine.
- **Date:** 2026-11-05, except the 10 GB aarch64 run (earlier, same data).
- **Cluster runs:** 3 and 4 processes of the same binary on the same host
  (127.0.0.1), `number_of_replicas: 1`, 1,000,000 Wikipedia docs (text
  truncated to 2,000 characters, 1.36 GB JSONL). Loopback networking: real
  network latency is not in these numbers.
- The lab scripts are not in this repository yet. A CI job that runs a real
  multi-process cluster is a roadmap item; until it exists these numbers are
  not reproduced automatically.

## Single-node sizing table

| Raw text | Disk | RAM during bulk load | RAM to serve only | Cores |
|---|---|---|---|---|
| 1 GB | 2.5 GB | 8–10 GB | 2 GB | 4 |
| 5 GB | 12 GB | 16 GB | 8 GB | 8 |
| 10 GB | 25 GB | 24 GB | 16 GB | 8–16 |
| above 10 GB | not measured | not measured | about index size + 1 GB (arithmetic) | not measured |

What each column rests on:

- **Disk.** The index measured 1.02–1.14× the raw text (1 GB → 1.09 GB;
  5 GB → 5.39 GB; 1 GB force-merged → 1.02 GB). The table allows 2.5× so a
  merge can hold old and new segments at once, plus the 1 GB WAL limit.
- **RAM during bulk load** is the measured peak RSS plus headroom: 7.2 GB at
  1 GB, 14.4 GB at 5 GB, 18.1 GB at 10 GB. A force-merge of the 1 GB index
  peaked at 8.2 GB. Bulk load is the most memory-hungry thing a node does.
- **RAM to serve only** is the index in page cache plus about 1 GB of the
  node's own memory: on the 5 GB index after search load, own memory was
  0.87 GB and total RSS 4.4 GB.
- **Cores.** Search settles at about **110 queries/s per busy core** (measured
  with the server pinned to 8 and to 16 cores).

The memory cap does not hold peak RAM yet. With
`[limits] max_process_memory_mb = 2048`, peak RSS was 3.57 GB, and on rc.101
`xerj index` dropped 144,158 of 284,158 documents and still exited 0. PR
[#2991](https://github.com/xerj-org/xerj/pull/2991) (open) retries those
rejections and exits 1 when documents are lost. Size RAM from the table, not
from the cap. `flush_size_mb = 64` cut the 1 GB peak by 11% (7.17 → 6.39 GB).

## Measured numbers (single node)

### Search

5 GB index, 22 segments, `match` queries on the title, 20 s per run.

| Server cores | Clients | Throughput | p50 | p95 | p99 |
|---|---|---|---|---|---|
| 8 | 1 | 196 qps | 5.1 ms | 7.0 ms | 8.4 ms |
| 8 | 8 | 747 qps | 10.8 ms | 16.5 ms | 20.4 ms |
| 8 | 32 (saturated) | 810 qps | 27.4 ms | 108 ms | 222 ms |
| 16 | 8 | 956 qps | – | – | 13.9 ms |
| 16 | 32 | 1,260 qps | 18.6 ms | – | 130 ms |

First pass with unique queries (no cache help), 8 cores, 32 clients: 614 qps,
p99 241 ms. A 1 GB index force-merged to one segment, 8 cores, repeated queries:
1,089 qps at p99 4.2 ms with 1 client, 9,825 qps at p99 16.6 ms with 32.

### Ingest

| Path | Result |
|---|---|
| `xerj index` (offline loader), 5 GB, 16 workers | 2,503 docs/s wall, 1,270 docs per CPU-second (~3.85 MB raw per CPU-second), uses about 2 cores |
| `xerj index`, 1 GB, 8 workers | 101.9 s wall; the final flush took 96.9 s of it |
| `_bulk`, 200k docs, 4 clients × 1,000 docs | 8,415 docs/s, 2,458 docs per server CPU-second; bulk request p50 0.17 s, **p99 11.6 s** |

### Recovery, boot and idle

| What | Result |
|---|---|
| `kill -9` with 726 MB of WAL unflushed (200,000 docs) | healthy again in 24.0 s, 200,000 of 200,000 docs recovered |
| Boot to health after a clean shutdown | 0.27 s (1 GB index), 1.3 s (5 GB index) |
| Idle | 89 MB RSS and 0.17% of a core (1 GB node); 199 MB RSS (5 GB node) |

During WAL replay the whole node is unavailable, including indices that need
no replay. Replaying per index behind open listeners is a roadmap item.

## Clusters: what works and what does not

Measured on 2026-11-05 on 3 and 4 local processes of rc.101 (see *How these
numbers were measured*). Summary: data that was acknowledged was not lost in
these runs, failover elects a new leader in well under a second, but losing one
node blocks most writes, several read APIs give wrong answers, and the cluster
cannot grow.

**Works:**

| What | Result |
|---|---|
| Ingest, 3 nodes, 1 replica, 6 clients, bulk 1,000 | 1,000,000 docs acked, 0 failed, 4,218 docs/s; batch latency p50 1.14 s, p95 3.11 s |
| Acked writes after `kill -9` of a node and of the leader | 1,404,558 acked ids checked on primaries and replicas, **0 missing** |
| Leader election after the leader is killed | 170–405 ms; a metadata write succeeded 0.26 s after `kill -9` of the leader |
| Reads with one node dead | GET 300 of 300, search total 300 (falls back to replicas) |
| Restart of a killed node | ready in 9–23 s after `kill -9`, 0.4–8.9 s after a graceful stop |

**Does not work yet** (each is a roadmap item; the fixes in review are linked):

| Defect | Measured | Fix |
|---|---|---|
| Creating an index with a different shard count re-buckets older indices | 17–25% of 1M acked docs returned 404 on GET after a 4-shard index was created | PR [#2992](https://github.com/xerj-org/xerj/pull/2992), open |
| A replica that missed writes stays in sync and hides them when the primary dies | shard replica 27,646 docs short, never healed; search total dropped by that much with 0 failed shards | PR [#2993](https://github.com/xerj-org/xerj/pull/2993), open |
| A new leader never commits entries left from the previous term | new leader at commit=0 after 200 heartbeat rounds | PR [#2987](https://github.com/xerj-org/xerj/pull/2987), open |
| One dead node blocks writes to 2 of 3 shards | 40 of 60 writes got HTTP 503; 65% of bulk items failed while the node was down | planned: replica promotion |
| Election storms after a node is lost, leader churn with no faults | 3.5–9 s storms; 5–22 leader changes per 2–4 min of plain ingest | planned: PreVote, CheckQuorum, timeouts |
| `_count`, `_mget`, `_msearch`, `_cat/indices` read only the local shard | `_count` 93/104/103 vs 300; `_mget` found 2 of 6 | planned |
| `_cluster/health` reports configured nodes, not live ones | 3 nodes and 0 unassigned primaries with one node dead | planned |
| No membership change or rebalance | a 4th node never receives shards of existing indices | planned |
| A hung peer stalls requests that touch it | requests waited up to 10.01 s while one node was frozen | planned |
| Rolling restart is not transparent | 344 errors from the restarting node over 150 s (262 "shutting down") | planned |

A network partition was **not** tested (it needs root for iptables or network
namespaces); a node frozen with `SIGSTOP` for 15 s was tested instead.

## How to size a deployment today

1. **Use one node** unless you have measured your own cluster. Pick the row in
   the sizing table at or above your raw data size.
2. **Size RAM for the bulk load**, not for serving. After the first load, a
   node that only serves needs about index size + 1 GB.
3. **Size cores for search:** expected peak qps ÷ 110, plus 2 cores for ingest
   and merges.
4. **Disk:** 2.5× raw text. Vectors add about `dims × 4` bytes per vector on
   top (see the operations page).
5. **Backups:** take snapshots ([SNAPSHOT_AND_RESTORE.md](SNAPSHOT_AND_RESTORE.md));
   a single node with snapshots is the supported availability model today.
6. **If you run a cluster anyway:** treat it as a beta. Apply the single-node
   figures to each node's share of shards (arithmetic, not measured), plan for
   one node lost meaning most writes fail until it returns, and do not rely on
   `_count`, `_mget`, `_msearch` or `_cluster/health` for correctness.

Verify a node you sized:

```sh
curl -s localhost:9200/_cluster/health
# expect "status":"green" (single node) or "yellow" (a cluster with replicas)
curl -s 'localhost:9200/_cat/indices?v'
# expect your index with the docs.count you loaded
```

## What would change these numbers

A measured multi-node sizing and SLA run, on an isolated ring after the
cluster fixes above land, is on the roadmap. Until that run exists, nothing on
this page is a cluster SLA.
