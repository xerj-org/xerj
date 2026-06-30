# Profiling: Path from 1.5M to 10M docs/s
Date: 2026-04-18T00:30 UTC

## Current State
- **Sustained 20M ingest: ~1.3-1.6M/s** (depends on flush_size_mb)
- **Burst 1M ingest: 2.2M/s**
- **WAL bypass ingest: 3.2M/s** (ceiling without WAL)
- **Raw memchr scan: 5.2M lines/s** (Python single-thread!)
- vs ES 8.13: ~50-60× faster

## Profiling Results (strace -c -f)

| Syscall | % time | Meaning |
|---|---|---|
| **futex** | **84%** | Mutex/semaphore contention |
| write | 3% | WAL + segment disk I/O |
| mmap | 1% | Segment reader cache |
| other | 12% | CPU work (scan, hash, CRC, memcpy) |

**84% of time is lock contention, NOT I/O or CPU.**

## Futex Sources (in order of impact)

1. **rayon block_on(tokio)** — ~40% of futex
   - 32 rayon scanners call `rt_handle.block_on(submit_batch)` 
   - This crosses runtimes: rayon thread → tokio task → WAL mutex → back
   - Each crossing involves 2-4 futex wake/wait pairs
   
2. **WAL shard mutex** — ~20%
   - 16 Mutex<WalWriter> shards, but batches from 32 scanners
   - 2 scanners per shard on average → some contention
   
3. **Memtable shard RwLock** — ~15%
   - parking_lot RwLock is fast but still futex-based
   
4. **tokio internals** — ~9%
   - Semaphore acquire, task wake, timer wheel

## Attempted Solutions

| Approach | Result | Why |
|---|---|---|
| Channel pipeline (rayon→tokio) | 2M ingest, 17M errors | Can't retry without batch clone |
| Channel + retry clone | 1.35M total | Clone cost > futex savings |
| Sync index_batch_turbo_raw | 3.3M ingest, 17M errors | Can't trigger async flush from sync |
| Sync + merge-poll flush | Hang | 50ms poll too slow for 3M/s ingest |
| Sync + block_on(flush) only | Deadlock risk | rayon thread in tokio runtime |

## Architecture Required for 10M/s

The fundamental issue: **index_batch_turbo_raw is async but the only async
part is back-pressure sleep**. WAL write and memtable push are synchronous.

### Option A: Fully synchronous engine path (recommended)
1. Move flush scheduling to a dedicated OS thread (not tokio)
2. Use parking_lot::Condvar for back-pressure (not tokio::time::sleep)
3. rayon scanners call engine directly — no runtime crossing
4. Expected: eliminate 40% futex → 2.5-3× gain → 4-5M/s

### Option B: Lockless WAL (append-only file, no mutex)
1. Each rayon thread gets its own WAL file (N=32)
2. No mutex at all — just atomic seq_no + direct file write
3. BufWriter per thread, periodic flush
4. Expected: eliminate 20% futex → additional 1.5× on top of Option A

### Option C: Batch coalescing
1. Multiple scanner chunks feed into a ring buffer per shard
2. Single WAL writer per shard drains the ring buffer
3. Amortizes CRC + write across 100k+ docs per write_all
4. Expected: additional 1.3× → reaches 8-10M/s ceiling

### Hardware ceiling calculation
- memchr scan: 5.2M/s (Python) → ~20M/s (Rust rayon)
- At 453B/doc: 20M × 453 = 9 GB/s → within tmpfs bandwidth
- CRC32: ~10 GB/s → 22M docs/s
- Bottleneck at 10M/s would be memory bandwidth (~40 GB/s)
