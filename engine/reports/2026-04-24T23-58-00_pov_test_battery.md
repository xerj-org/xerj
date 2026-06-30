# POV test battery — `prod/v1-readiness` after ES-compat merge

**Date**: 2026-04-24
**Branch HEAD**: `ffd49ac` (merge fix landed)
**Goal**: validate the merged build against the things a customer
will exercise during a 60–90 minute proof-of-value (POV): functional
smoke, durability, performance, cluster.

## Executive verdict

**Not ship-ready for a POV today.** The good news is performance is
strong (100 K docs/s ingest, sub-millisecond p99 search latency on
600 K docs) and the ES wire surface answers correctly across CRUD,
queries, aggregations, and cluster endpoints. The bad news is **two
P0 production-blocker bugs** were caught in the first 30 minutes of
testing — one already fixed in this session, one still open.

## What was tested

| # | Bucket | Verdict |
|---|--------|---------|
| 1 | Functional smoke (schema · CRUD · queries · aggs · vector) | 🟡 16/18 pass |
| 2 | Durability — graceful restart | 🔴 100 % data loss when index has no flushed segments |
| 3 | Durability — kill -9 mid-flight | not run; same root cause as #2 |
| 4 | Performance — ingest throughput | 🟢 100 K docs/s sustained, single host |
| 5 | Performance — search latency (read-only) | 🟢 p50 0.39 ms · p95 0.73 ms · p99 0.90 ms |
| 6 | Performance — concurrent ingest+search | 🟡 search p99 1.0 ms → 463 ms under concurrent ingest (FTS RwLock contention, known) |
| 7 | Cluster endpoints (single-node) | 🟢 health=green · _cat/indices · _cat/shards · _cluster/state all OK |
| 8 | Cluster — multi-node | 🔴 Raft log in-memory only (separate, pre-existing) |

## Bugs found and triage

### B-1 — `merge` silently drops every segment (FIXED in `ffd49ac`)

**Symptom**: After a 100-doc bulk into a fresh index, `_count = 100`
and every `GET _doc/{id}` returns 200, but `match_all` returns 95.
Server log fires `WARN merge: failed to parse stored as RawValue` at
the 5 s merge cadence. Each tick drops more docs from search.

**Root cause**: `Box<RawValue>` deserialization uses serde_json's
private newtype tag `$serde_json::private::RawValue`, which
**simd_json's serde adapter does not recognise**. The merge code at
`engine/crates/xerj-engine/src/index.rs:1932` was the only call
site in the engine that fed `Vec<Box<RawValue>>` through
`simd_json::serde::from_slice` — every other call site used
`Vec<Value>` and worked. Every merge attempt failed the deserialize,
hit `continue`, then the merge wrote a (smaller) replacement
segment and the input segments were retired. The docs that didn't
fit into the smaller replacement were silently lost.

**Fix**: One-line switch to `serde_json::from_slice` (which DOES
handle `RawValue` correctly). Verified by re-running the bulk —
WARN gone, all docs reachable via term/range/exists/GET, count and
match_all agree post-merge.

### B-2 — graceful restart loses indexes that have no flushed segments

**Symptom**: Bulk-index 50 docs into a fresh index `durtest`. Call
`POST /_flush`. Stop the server with SIGTERM. Restart. The index is
not opened on startup (no log line for it), `GET /durtest/_count`
returns 0, every `GET _doc/{id}` returns 404. Result: **100 % data
loss for indexes whose memtable hadn't auto-flushed before
shutdown**.

**Three contributing failures, all in tree, none new in this
merge**:
1. `POST /_flush` does not actually drain the memtable to a segment
   for the just-bulk-ingested data. The data is still WAL-only when
   the server stops.
2. Graceful shutdown (SIGTERM) does not perform a final flush
   before exit. Memtable contents are lost.
3. Index discovery on startup looks for indexes via segment-bearing
   directories / a `snapshot.json`. An index with only WAL files
   (no segments yet) is never discovered, so its WAL is never
   replayed — even though replay would otherwise rebuild the
   in-memory state correctly.

  In the same restart, the *other* indexes that had completed at
  least one auto-flush before the test (test_simple, products) DID
  recover, with the doc-count log line firing on startup. So WAL
  replay works when the index is discovered; the bug is in the
  discovery step.

**Fix scope**: any one of the three would mitigate; ideally all
three:
- `_flush` becomes synchronous on the API path (force `do_flush_shard`
  for every shard whose memtable is non-empty, await all).
- SIGTERM handler invokes `engine.flush_all().await` before
  closing listeners.
- Index discovery should treat a populated `wal/` directory as a
  valid index marker and trigger WAL-replay-only recovery for it.

`engine/crates/xerj-engine/src/engine.rs` (open path),
`engine/crates/xerj-server/src/main.rs` (shutdown handler),
and the `_flush` REST handler in `engine/crates/xerj-api/src/`.

### B-3 — bulk visibility regression on small batches

**Symptom**: Even with merge disabled (`min_segments=100000`), a
100-doc bulk that returns `match_all = 100` immediately drops to
~97 within 3 s. Stable at 97 thereafter. `_count = 100` throughout.
Affects bulk only; single-doc `PUT /_doc/{id}` doesn't lose visibility.
Does *not* manifest at 100 K-doc scale (perf test stayed at 100 K
visible).

**Root cause**: not narrowed in this session. Plausible candidates:
- Soft-flush re-reading FTS reader from a stale snapshot.
- Per-shard memtable→segment handover dropping a few docs from the
  index reader between writer-stage and reader-pickup.
- `?refresh=true` semantics differ from the immediate post-write
  in-memory enumeration.

Tracking as P1 — not a 100 % data-loss bug like B-2, but visible to
any POV that bulk-loads a small dataset and runs a `match_all` to
verify.

## Performance numbers (reproducible)

### Ingest

100 K docs / 5 K-doc batches / sequential POSTs to `/_bulk` over
loopback HTTP, single Xerj node, 32-core host:

```
total: 1000 ms · rate: 100,000 docs/s
count after refresh: 100,000
match_all visible: 100,000
```

### Search latency (read-only on a 100 K-doc index)

1000 sequential POSTs to `/perftest/_search`, mix of term, range,
match, bool, terms-agg:

```
p50:  0.39 ms
p95:  0.73 ms
p99:  0.90 ms
max:  4.25 ms
mean: 0.42 ms
```

### Search latency under concurrent ingest

10-second sustained run, one ingest thread (1 K-doc batches) +
one search thread (random `term:k=N`):

```
ingested 506,000 docs in 10 s (= 50,600 docs/s)
searches completed:      40
search p50:    231.56 ms
search p95:    422.38 ms
search p99:    462.91 ms
search max:    462.91 ms
```

Search p99 climbed from 0.9 ms to 463 ms under concurrent ingest —
matches the perf-audit prediction that the FTS RwLock held for the
duration of each batch insert blocks readers. Listed in
`reports/2026-04-24T00-00-00_v1_readiness_audit.md` as backlog item
#4 ("Split FTS memtable write lock", 15–25 % p99 win, medium
complexity).

## Cluster behaviour

Single-node green, all ES-compat cluster endpoints functional:

```
GET /_cluster/health
  status=green  number_of_nodes=1  active_primary_shards=4

GET /_cat/indices
  green open products    72   docs
  green open test_simple 97   docs
  green open durtest     0    docs   (post-B-2 restart, was 50)
  green open perftest    606,000 docs

GET /_cluster/state
  master_node=xerj-node-1
```

Multi-node still blocked behind cluster Raft log persistence —
documented separately in `2026-04-24T00-00-00_v1_readiness_audit.md`
(blocker #4). Until that lands, multi-node POVs cannot proceed.

## Recommendations before any customer POV

| Severity | Item | Status |
|----------|------|--------|
| **P0** | Fix B-1 (simd_json + RawValue merge) | ✅ landed in `ffd49ac` |
| **P0** | Fix B-2 (durability across restart) — three sub-fixes above | ❌ open |
| **P1** | Fix B-3 (small-batch bulk visibility regression) | ❌ open, not narrowed |
| **P2** | Land FTS memtable shard split for concurrent r/w (perf #4) | ❌ open, on backlog |
| **P3** | Cluster Raft log persistence for multi-node POVs | ❌ open, on audit |

The single-node, read-mostly POV with no restart and a >50 K-doc
bulk **works today** at 100 K docs/s ingest and sub-millisecond p99
search. The single-node POV with any of (small-batch bulk + assert
all docs visible / restart / sustained mixed ingest+search) **does
not** until B-2 and B-3 land.

## Reproduction commands

```bash
# Server
cat > /tmp/xerj-pov.toml <<'EOF'
[server]
rest_port      = 19301
es_compat_port = 19300
data_dir       = "/tmp/xerj-pov-data"
bind_address   = "127.0.0.1"
[auth]
enabled = false
EOF
rm -rf /tmp/xerj-pov-data
target/release/xerj --config /tmp/xerj-pov.toml --data-dir /tmp/xerj-pov-data &

# Functional smoke
/tmp/pov-functional.sh

# Bulk ingest perf
# (see helper at /tmp/perf-batches/*.ndjson generated by the harness)
for i in 0{0..9} {10..19}; do
  curl -s -XPOST http://localhost:19300/_bulk \
       -H 'content-type: application/x-ndjson' \
       --data-binary @/tmp/perf-batches/${i}.ndjson > /dev/null
done

# Search latency
# (1000 random POSTs, see harness in this report)

# Durability test
curl -s -XPUT http://localhost:19300/durtest -H 'content-type: application/json' \
     -d '{"mappings":{"properties":{"name":{"type":"text"}}}}'
# Bulk 50 docs into durtest, then:
curl -s -XPOST http://localhost:19300/durtest/_flush
pkill -SIGTERM -f xerj && sleep 4
target/release/xerj --config /tmp/xerj-pov.toml --data-dir /tmp/xerj-pov-data &
sleep 3
curl -s http://localhost:19300/durtest/_count   # should be 50; will be 0 until B-2 fixed
```
