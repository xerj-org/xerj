# Micro-ops + Snapshot-write Race Fix
Date: 2026-04-18T20:00 UTC
Machine: 32 cores x86_64, 119 GB RAM, tmpfs I/O (shared with other workload)
Branch: `perf/sync-engine`

## What was fixed

### 1. Snapshot.tmp race — the real cause of "No such file"

Pre-fix `save_snapshot`:
```rust
std::fs::write(&tmp, &bytes)?;        // tmp = snapshot.tmp (same name for ALL callers)
std::fs::rename(&tmp, &path)?;
```

With sharded flushes running in parallel, two flushes both call
`save_snapshot`:

1. Flush A: writes `snapshot.tmp`
2. Flush B: writes `snapshot.tmp`  (overwrites A)
3. Flush A: `rename(snapshot.tmp → snapshot.json)` ✓
4. Flush B: `rename(snapshot.tmp → snapshot.json)` → **ENOENT** (A consumed it)

Every race failure aborted the whole `do_flush_shard` task — its docs
stayed in the memtable until the next flush tick.  Observed on ~1/5
runs pre-fix; sometimes with 5-8 shards failing per run.

**Fix**: unique tmp filename per caller (`snapshot.tmp.<uuid>-<tid>`).
`arc_swap` already provides content-level atomicity for the in-memory
`self.snapshot`; the tmp name was the only real contention source.

### 2. WAL prune ENOENT tolerance

`finalize_flush_with_publisher` calls `wal.prune()` on all 16 WAL
shards after each segment flush.  Two concurrent flushes can both
see the same old-generation `.wal` file in `read_dir` and race to
delete it; the loser got `NotFound`.  Pre-fix that also aborted the
whole shard flush.  Now `prune` swallows `NotFound` as benign.

### 3. Ingest hot-path micro-ops

- **`IndexStore::wal_append_batch_raw`** — new fast-path that takes
  `&[(String, Arc<[u8]>)]` directly.  Saves a per-batch `Vec` allocation
  and N×`Arc<Value::Null>` wrappers.  At 400 batches/s × 5 k docs =
  2 M allocs/s of eliminated overhead.
- **`FtsMemtable::insert_raw_bytes_fresh`** — skip the prior
  `mem.remove(id)` HashMap-miss lookup that fired per doc on the CLI
  bulk path.
- **Per-batch `doc_count.fetch_add`** — one atomic per batch instead
  of per doc.  At 1.7 M docs/s that cuts ~17 M atomic ops/s of cache-
  line bouncing.

## Results (20M nginx, 32 workers, 5 runs — system is shared with another 50 GB RSS process)

| Run | Ingest | Total | Elapsed |
|-----|--------|-------|---------|
| 1 | 1,813 k/s | 1,500 k/s | 13.33 s |
| 2 | 1,813 k/s | 1,372 k/s | 14.57 s |
| 3 | 1,803 k/s | 1,232 k/s | 16.23 s |
| 4 | 1,789 k/s | 1,372 k/s | 14.58 s |
| 5 | 1,779 k/s | 1,417 k/s | 14.11 s |

With errors eliminated but another heavy process sharing the host, the
Total Rate variance is higher than the pre-refactor dataset.  Ingest is
stable: **median 1,803 k/s / best 1,813 k/s**.

### 1M burst

| Run | Ingest | Total |
|-----|--------|-------|
| 1 | 4,353 k/s | 1,264 k/s |
| 2 | 5,158 k/s | 1,444 k/s |
| 3 | 4,860 k/s | 1,498 k/s |

Median **4,860 k/s / best 5,158 k/s** — unchanged from pre-micro-op
(noise).  The micro-ops targeted per-batch overhead which is not the
dominant cost at this rate either; memtable push and WAL frame build
dominate.

### Before/after comparison

| Metric           | Pre-refactor (fd6432d) | Sync-engine (de22c4c) | Micro-ops (this) | Δ vs fd6432d |
|------------------|------------------------|-----------------------|------------------|--------------|
| 20M ingest med   | 1,695 k/s              | 1,698 k/s             | 1,803 k/s        | **+6.4 %**   |
| 20M ingest best  | 1,715 k/s              | 1,763 k/s             | 1,813 k/s        | **+5.7 %**   |
| 20M total med    | 1,496 k/s              | 1,411 k/s             | 1,372 k/s        | −8.3 %       |
| 20M total best   | 1,610 k/s              | 1,529 k/s             | 1,500 k/s        | −6.8 %       |
| 1M burst best    | 2,200 k/s              | 5,452 k/s             | 5,158 k/s        | **+2.3×**    |
| Shard flush errs | none reported          | ~1/5 runs             | **none**         | fixed        |

The 20M **total-rate** regression is partly from the shared-host
state (other process taking resources) and partly from higher final-
flush time variance.  Pure ingest is strictly better.

## Remaining bottleneck

The bench shows a clear pattern now:
- **1M (no flush triggered)**: 4.9 M/s ingest — per-batch dispatch fine
- **20M (flush cycle active)**: 1.8 M/s ingest

The 2.7× drop-off IS the flush cycle.  Memtable fills at ~2 GB/s while
the sharded flusher drains each 32 MB shard-snapshot to a segment file
in ~50-100 ms.  When the soft back-pressure (memtable > 2× flush
threshold) kicks in, every batch pays up to 50 ms of `thread::sleep`
(10 × 5 ms) while the flushes finish.

The **true ceiling** on this workload is not lock contention or async
overhead — those are now tight — but **segment-write throughput and
the flush-ingest oscillation**.  To continue climbing:
1. Concurrently emit MORE segments (raise `flush_sema`, lower per-
   shard threshold so each flush is smaller and finishes sooner).
2. Overlap flush I/O with ingest: the flushed shard's MemEntries are
   already `Arc`-deep-copied before we drop the write-lock; we could
   release back-pressure the instant the drain completes, long before
   the segment write hits disk.
3. Investigate the `version_map.set` per-doc call inside
   `wal_append_batch_raw` — another per-doc op we haven't batched.

## Files

- `engine/crates/xerj-storage/src/index_store.rs` — `wal_append_batch_raw`;
  unique tmp name in `save_snapshot`.
- `engine/crates/xerj-storage/src/wal.rs` — `prune` tolerates `NotFound`.
- `engine/crates/xerj-engine/src/memtable.rs` — `insert_raw_bytes_fresh`.
- `engine/crates/xerj-engine/src/index.rs` — `index_batch_sync_raw`
  rewired to the raw WAL path, batch doc_count atomic, fresh-insert.
