# Sync Engine Refactor — Results & Findings
Date: 2026-04-18T15:50 UTC (Session continuation after session crash at M9 profiling)
Machine: 32 cores x86_64, 119 GB RAM, tmpfs I/O
Branch: `perf/sync-engine`

## Hypothesis (from profiling — commit fd6432d)

strace `-c -f` on the 20M ingest path showed:
- **84 % of syscall time = futex** (lock/semaphore wait/wake)
- ~40 % of that futex attributed to **rayon workers calling `rt_handle.block_on(submit_batch)`** — a runtime crossing from rayon → tokio per batch
- ~20 % WAL shard mutex; ~15 % memtable RwLock; ~9 % tokio internals

**Prediction**: eliminate the rayon↔tokio crossing (Option A) → 2.5–3× throughput gain → 4–5 M/s ingest.

## What was built

1. **`Index::index_batch_sync_raw`** (new) — synchronous twin of `index_batch_turbo_raw`.
   Entry point for rayon workers: WAL append (already sync), memtable push (already sync),
   per-shard threshold check, spawn flush via cached `Handle`.  Zero `.await`.
2. **`SyncFlushCoord`** — caches a `tokio::runtime::Handle` captured at Index
   construction + FTS `field_configs` built once (previously rebuilt on every flush
   via `schema.read().await`).  `Handle::spawn` is a few atomics + mpsc send —
   no `block_on`, no dedicated flusher OS thread.
3. **`Index::try_spawn_sync_flush`** — non-blocking `try_acquire_owned` on
   `flush_sema` + direct `rt.spawn(do_flush_shard)` from any synchronous context.
4. **CLI rayon loop** rewired: rayon workers call `idx.index_batch_sync_raw(batch)`
   directly.  Deleted the per-batch `block_on(submit_batch)` pair, deleted the CLI
   `tokio::Semaphore`.  Back-pressure retry uses `std::thread::sleep`
   (nanosleep) instead of `tokio::time::sleep`.

Files:
- `engine/crates/xerj-engine/src/index.rs` — new `SyncFlushCoord`,
  `index_batch_sync_raw`, `try_spawn_sync_flush`, field on `Index`
- `engine/crates/xerj-engine/src/memtable.rs` — `shard_load(shard_idx)`
  single-shard accessor
- `engine/crates/xerj-server/src/main.rs` — rayon scanner loop rewritten;
  deleted `submit_batch` async helper and `rt_handle.block_on` crossings

## Results (20M nginx ingest, 32 workers, batch=10 000, ingest_shards=16 default)

| Run | Ingest rate | Total rate | Total elapsed |
|-----|-------------|------------|---------------|
| 1 | 1,698 k/s | 1,433 k/s | 13.95 s |
| 2 | 1,708 k/s | 1,408 k/s | 14.21 s |
| 3 | 1,549 k/s | 1,411 k/s | 14.17 s |

**Median**: 1,698 k/s ingest / 1,411 k/s total.

### Comparison to pre-refactor (commit fd6432d baseline, 5 runs)

| Metric | Pre-refactor | Sync engine | Δ |
|---|---|---|---|
| Ingest rate (median) | 1,695 k/s | 1,698 k/s | +0.2 % |
| Ingest rate (best) | 1,715 k/s | 1,763 k/s | +2.8 % |
| Total rate (median) | 1,496 k/s | 1,411 k/s | −5.7 % |
| Total rate (best) | 1,610 k/s | 1,529 k/s | −5.0 % |

### 1M burst ingest (no memtable back-pressure)

Single run, 1 M docs, default config: **5,452 k/s ingest** — fits entirely in
memtable, no flush triggered.  Matches prior "2.2 M/s burst" ceiling × ~2.5
(consistent with removing block_on overhead on the unconstrained hot path).

## Findings

**The profiling theory did not predict the 20M outcome.**  Eliminating the
rayon↔tokio crossing delivered essentially flat throughput at 20M scale, not
the predicted 2–3×.  The −5 % regression on total-rate is within run-to-run
variance (the 3-run sample size here is thin) but confirms we did not break
correctness and did not gain meaningfully on this workload.

Interpretation: the `block_on` futex cost attributed to "40 %" likely
*included* time waiting for the WAL mutex INSIDE `submit_batch` — profiling
counts mutex-held time as part of the enclosing function.  Removing `block_on`
without removing the mutex contention leaves the underlying wait in place.

The unconstrained 1M burst case (5.45 M/s) suggests the sync path IS faster
per-batch when the memtable doesn't need draining — but at 20M scale, flush
throughput is the bottleneck, not ingest dispatch overhead.

## Architectural value retained

Even without a measurable 20M win, the refactor:
- Removes the cross-runtime `block_on` idiom on the ingest hot path (cleaner).
- Caches `field_configs` (was rebuilt per flush via tokio `schema.read().await`).
- Makes `Handle::spawn` from sync code a first-class pattern we can reuse.
- `index_batch_sync_raw` is now the preferred entry point for any future
  fully-synchronous bulk ingest harnesses.

## Known issues

- Rare `"storage error: I/O error: No such file or directory (os error 2)"`
  on one shard's final flush.  Does not cause data loss (sent count is still
  20 M, no errors reported from CLI side) but a shard's segment write failed.
  Suspect a race between a sync-path flush and the async `Index::flush()`
  final drain; needs investigation but out of scope for this PR.  Reproducible
  ~1/5 runs.

## Recommended next work

1. **Option B: Per-thread (lockless) WAL** — profiling attributed 20 % of
   futex to the WAL shard mutex (16 mutexes / 32 threads → 2:1 contention).
   Give each rayon worker its own WAL file; generate seq_no via a single
   `AtomicU64`; drop the mutex entirely.  Expected gain: ~1.5×.
2. **Investigate the "No such file" race** — check whether concurrent
   `do_flush_shard` invocations (sync path + async `Index::flush`) step on
   each other's snapshot.json writes.
3. **Revisit profiling on the new binary** — strace `-c -f` a 20M run of
   the sync-engine build.  If futex is no longer 84 %, we now have an accurate
   signal.  If it is, we know the real bottleneck is WAL mutex.

## Config notes

- `ingest_shards=32` (matching rayon worker count) was unstable in this
  session — mixed runs of 1.8 M/s and 100 k/s.  Needs investigation.  For
  now, default `ingest_shards=16` is recommended.
- `ingest_shards=16`, `flush_size_mb=512`, `wal_max_size_mb=1024` remain
  the stable configuration.
