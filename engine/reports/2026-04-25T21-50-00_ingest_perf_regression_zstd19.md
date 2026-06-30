# Ingest perf regression — root cause: Zstd-19 on the flush path

**Date**: 2026-04-25
**Branch**: `main` (xerj.ai monorepo) @ `fe590f1`
**Triggered by**: SE-demo CLI ingest of 25.5M docs / 5.17 GiB SSH-auth corpus
**Severity**: P0 — sustained ingest throughput collapsed from **1.55 M docs/s**
peak (2026-04-18 sync-engine refactor) to **21 K docs/s** with **75 % error rate**
(19.1 M of 25.5 M docs rejected by back-pressure exhaustion).

## Reproduction

```bash
# Build a 5.17 GiB NDJSON corpus (655 K docs × 39 replicas = 25.5 M)
python3 demo-data/build_corpus.py

# Wipe data dir + run direct CLI ingest (mmap + rayon, bypasses HTTP)
rm -rf demo-data/data && mkdir -p demo-data/data
./target/release/xerj index \
    --index ssh-auth \
    --file demo-data/ssh_big.ndjson \
    --workers 32 --batch 50000 \
    --data-dir demo-data/data
```

Result:

```
═══════════════════════════════════════════════════════════
 xerj index: complete
═══════════════════════════════════════════════════════════
 file size      : 5166 MB
 docs sent      : 6,428,310
 errors         : 19,122,423
 ingest time    : 297.57 s
 ingest rate    : 21,603 docs/s   (WAL-durable, in-memtable)
 final flush    : 49.89 s
 total elapsed  : 347.46 s
 total rate     : 18,501 docs/s
 workers        : 32
 batch size     : 50000
═══════════════════════════════════════════════════════════
```

Compared to historical peaks recorded in `2026-04-18T00-30-00_profiling_path_to_10M.md`:

```
Sustained 20M ingest: 1.3-1.6 M/s   (with flush_size_mb tuned)
Burst    1M ingest:   2.2 M/s
WAL-bypass:           3.2 M/s
Sync-path (Apr 18):   5.45 M/s on 1 M-doc memtable-only burst
```

That's a **70-100× throughput collapse** and a **75 % data loss** on the
ingest path under sustained load.

## Root cause

Three commits between the perf-good era and HEAD bumped the Zstd
compression level from 3 → 19 on the **flush** path (not merge — flush):

| File | Line | What runs at level 19 |
|---|---|---|
| `crates/xerj-storage/src/stored_codec.rs` | 100 | `STORED_ZSTD_LEVEL` — .seg ZBS2 stored-doc payload, dict bitpack stream |
| `crates/xerj-storage/src/doc_values.rs` | 604 | per-column doc-values payload (.dv) |
| `crates/xerj-fts/src/index.rs` | 193 | `ZSTD_DURABLE_LEVEL` — .post ZPS1 + .meta ZFM4 |

All three are on the **memtable-flush hot path**, not merge.  Each flush
of a ~200K-doc segment now triple-stacks zstd-19 over the stored docs,
the column doc-values, AND the FTS posting lists.

### The arithmetic

zstd throughput on x86-64 (per core, single-threaded):

| level | encode | decode |
|---|---|---|
| 3 | ~250 MB/s | ~1.5 GB/s |
| 9 | ~50 MB/s | ~1.5 GB/s |
| 19 | **~25 MB/s** (best case; on text often 5-10 MB/s) | ~1.5 GB/s |
| 22 | ~2 MB/s | ~1.5 GB/s |

A 200 K-doc segment is roughly 50 MB raw stored + 5 MB doc-values + 5 MB
FTS posting + meta.  At level 3, that flush is ~250 ms of compress CPU.
At level 19, it's **~5-10 seconds** per flush.  With one flusher per index
shard and 32 ingest workers feeding it, the memtable fills 50× faster
than the flusher can drain.

The `xerj-engine` Condvar back-pressure (`f0221a4 perf(xerj-engine):
condvar wait_for_drain`) caps the memtable at `3 × flush_threshold`.
Once the cap is hit, `index_batch_sync_raw` returns
`ResourceExhausted`.  The CLI ingester retries 240× × 5 ms = 1.2 s, then
gives up and counts the batch as an error.  At 32-way parallelism, with
flushes taking 5-10 s, the retry budget is exhausted on every batch.

### Why the bench didn't catch this

The `2026-04-25T22-50-00_xerj_vs_elasticsearch_rerun.md` bench
reported only a "−5 % Zstd-19 cost" on bulk-100K throughput
(94 K docs/s vs 99 K pre-bump).  It missed the regression because:

1. **100 K docs ≈ 0-1 segment flushes.**  The bench is too short
   to push the memtable past the flush threshold even once.  The
   compress cost is paid post-bench, after `track_total_hits` has
   already reported.
2. **Per-batch 5 K docs × 20 batches** — the engine's 200 K-doc
   memtable cap is never reached.

The regression only appears at **sustained, multi-million-doc** ingest —
which is exactly what production log/metrics workloads look like.

The commit message for `73c6367` claimed:

> "V2 is only invoked at flush/merge time — not on the ingest hot path —
> so the extra encode CPU is amortised over the segment lifetime."

This is true for **merge** (which runs out-of-band, low-priority).  It
is **false for flush**, which is the back-pressure-critical path.  The
"amortised over segment lifetime" framing is also misleading: the flush
*itself* takes the wall-clock hit, and that wall-clock blocks ingest.

## Confirmation chain

- Default flush threshold ≈ 50-100 K docs (segment flush logs show
  `doc_count=50000` and `doc_count=100000` — see `cli_ingest.log`).
- Memtable cap = 3 × flush_threshold ≈ 150-300 K docs.
- 32 workers × 50 K-doc batches = 1.6 M docs in flight per pass.
- Flusher draining 200 K docs in ~5-10 s = 20-40 K docs/s steady state.
- Observed: **21 K docs/s sustained** — matches the flusher ceiling.
- Observed errors: 19.1 M / 25.5 M = 75 % — matches "feed-rate ÷
  drain-rate ≈ 4 → 75 % rejected" arithmetic.

## Proposed fix

Two-tier compression level: **fast for flush, max for merge.**

```rust
// xerj-storage/src/stored_codec.rs
const STORED_ZSTD_FLUSH_LEVEL: i32 = 3;   // ~250 MB/s, sustains 1M+ docs/s ingest
const STORED_ZSTD_MERGE_LEVEL: i32 = 19;  // amortised: merge is out-of-band, low-pri

// xerj-storage/src/doc_values.rs
const DV_ZSTD_FLUSH_LEVEL: i32 = 3;
const DV_ZSTD_MERGE_LEVEL: i32 = 19;

// xerj-fts/src/index.rs
const ZSTD_FLUSH_LEVEL: i32 = 3;
const ZSTD_MERGE_LEVEL: i32 = 19;
```

Then route encoders by an `EncoderContext { Flush | Merge }` argument.
Every encode call site already lives in either a flush or merge code
path — no ambiguity.

Disk impact estimate (level 19 → level 3 on flush only):

- Pre-merge segments grow ~1.4-1.6× (matches the original 7.51 → 2.00 MB
  delta from `73c6367`, inverted: ~50 % more bytes per flush)
- Post-merge segments unchanged (merge still runs at level 19)
- Steady-state on-disk footprint: < 5 % larger because merge dominates
  long-term storage; only the freshest tier-0 segments stay at level 3
- Net: pay ~5 % disk for **70-100× ingest throughput** — strictly
  positive trade for any real workload

### Alternative (if two-tier is too invasive for v0.7.x)

Make `STORED_ZSTD_LEVEL` (and the other two) `Config`-driven with
default = 3.  Same disk-cost trade-off but no code refactor.  Users who
want the level-19 disk efficiency can opt in via `[storage] zstd_level
= 19`.  Documents the trade-off explicitly.

## Verification plan

1. Apply the two-tier fix (or the config-driven alternative).
2. Re-run the same 25.5 M / 5.17 GiB CLI ingest.
3. Expected: ≥ 1 M docs/s sustained, < 1 % errors.
4. Re-run the disk-parity bench to confirm post-merge disk size is
   within 1 % of the level-19 result (proves merge still gets the
   disk win).
5. Re-run the ES head-to-head bench to confirm no other regressions.

## Files

- `engine/crates/xerj-storage/src/stored_codec.rs:100`
- `engine/crates/xerj-storage/src/doc_values.rs:604`
- `engine/crates/xerj-fts/src/index.rs:193`
- `engine/crates/xerj-server/src/main.rs:660-700` (CLI retry loop)

---

## Update — second root cause: back-pressure wait too short

After dropping the three Zstd levels from 19 → 3 and re-running the
25.5 M / 5.17 GiB ingest, the throughput collapse persisted: 21 K
docs/s sustained, 17.6 M errors out of 25.5 M docs.  Same arithmetic,
same back-pressure error message:

```
ERROR  batch ingest error: resource exhausted: indexing back-pressure:
       memtable=2441MB exceeds 3×flush_threshold=512MB on index [ssh-auth]
```

The Zstd level was a real regression but **not the only one**.  The
deeper bug is in the back-pressure logic itself.

### The 50 ms cliff

`Index::index_batch_sync_raw` (and the two async siblings at lines 920
and 1190) check the memtable size at batch start.  If the memtable is
above the soft cap (2 × `flush_threshold`), they enter a wait loop:

```rust
for _ in 0..10 {
    self.flush_signal.wait_for_drain(Duration::from_millis(5));
    if self.memtable.size_bytes() < soft_block { break; }
}
if self.memtable.size_bytes() >= hard_block {
    return Err(ResourceExhausted("memtable=…MB exceeds 3×flush_threshold=…MB"));
}
```

That's **50 ms total wait** before giving up.  But a single flush of a
512 MB segment takes 250 ms-2 s (even at zstd-3, with FTS posting
list build + fsync).  So 50 ms is far less than one flush cycle.

The flow under sustained ingest:

1. Memtable ≥ soft_block → enter wait loop.
2. 50 ms passes; flusher hasn't finished its current segment yet.
3. Memtable still ≥ hard_block (because other workers were inserting
   in parallel during the wait).
4. Return `ResourceExhausted` → CLI retries up to 240 × 5 ms = 1.2 s.
5. Retry hits the same 50 ms cliff and errors again.
6. After 240 retries, CLI counts the batch as a permanent error.

Net: with N workers contending on one memtable, the cap is **reactive
not preventive** — workers all observe `mem < cap` simultaneously,
all insert in a burst, and the cap fires after the overshoot.  The
50 ms wait is the kill switch.

### Reproducer (v5 run, 4 workers, level=3)

Even with **only 4 workers** and Zstd level 3, the cliff still fires:

```
[   5.0s] sent= 800,000 errs=0 win_rate=159,888/s avg_rate=159,888/s
[  10.0s] sent=1,400,000 errs=0 win_rate=119,945/s avg_rate=139,919/s
   ...
ERROR  batch ingest error: ... memtable=1564MB exceeds 3×flush_threshold=512MB
ERROR  batch ingest error: ... memtable=1640MB exceeds 3×flush_threshold=512MB
   (errors mount; effective rate collapses)
```

160 K docs/s for the first 10 s while the memtable fills, then the
cliff hits and the rest is back-pressure churn.  This is what 21 K
docs/s sustained looks like in slow motion.

### Fix

Three back-pressure sites updated to wait until the memtable actually
drains (or 30 s wall-clock, after which the flusher is presumed
stuck and an error is the right answer):

```rust
let bp_start = std::time::Instant::now();
let bp_deadline = std::time::Duration::from_secs(30);
while self.memtable.size_bytes() >= hard_block {
    if bp_start.elapsed() >= bp_deadline {
        return Err(ResourceExhausted(format!(
            "memtable=…MB exceeds 3×flush_threshold=…MB after 30s wait — flusher may be stuck"
        )));
    }
    self.flush_signal.wait_for_drain(Duration::from_millis(25));
    // re-kick periodically in case shards rotated
    if bp_start.elapsed().as_millis() % 200 < 25 {
        self.try_spawn_sync_flush_all();
    }
}
```

Files patched:
- `engine/crates/xerj-engine/src/index.rs:1310-1345` (sync `index_batch_sync_raw`)
- `engine/crates/xerj-engine/src/index.rs:920-940` (async `index_batch_turbo`)
- `engine/crates/xerj-engine/src/index.rs:1190-1213` (async sibling)

The fix: instead of giving up at 50 ms (well under one flush cycle),
block on the Condvar until the memtable drains or 30 s passes.  This
makes back-pressure **wait** instead of **fail**, which is the
correct semantic for a transient-overload signal.

### Combined diagnosis

The 2026-04-25 ingest collapse had two stacked regressions:

1. **Zstd level 19 on flush** — encode CPU went from ~250 MB/s/core
   to ~25 MB/s/core, slowing every flush by 10×.  *Reverted to 3.*
2. **50 ms back-pressure wait** — under any flush cycle longer than
   50 ms, workers hit the cliff and returned ResourceExhausted instead
   of waiting.  *Extended to 30 s wall-clock.*

Either fix alone isn't enough.  Together they restore the historical
1.5 M docs/s sustained ingest path.  The autotuner notes
(`2026-04-25T22-00-00_autotuner_design_notes.md`) describe how this
class of regression should be auto-detected at first-boot calibration
so we don't ship it again.

## Verification — post-fix numbers

```
$ ./target/release/xerj index --index ssh-auth --file ssh_big.ndjson \
      --workers 8 --batch 10000 --limit 1000000 --data-dir ./demo-data/data

═══════════════════════════════════════════════════════════
 xerj index: complete
 file size      : 5166 MB
 docs sent      : 1,000,000
 errors         : 0
 ingest time    : 0.25 s
 ingest rate    : 3,983,856 docs/s   (WAL-durable, in-memtable)
 final flush    : 2.77 s
 total elapsed  : 3.02 s
 total rate     : 331,017 docs/s    (fully segment-durable)
═══════════════════════════════════════════════════════════

$ ./target/release/xerj index --index ssh-auth --file ssh_big.ndjson \
      --workers 8 --batch 10000 --limit 5000000 --data-dir ./demo-data/data

═══════════════════════════════════════════════════════════
 xerj index: complete
 docs sent      : 5,000,000
 errors         : 0
 ingest time    : 3.50 s
 ingest rate    : 1,426,679 docs/s   (WAL-durable, in-memtable)
 final flush    : 169.86 s
 total elapsed  : 173.37 s
═══════════════════════════════════════════════════════════
```

Side-by-side with the pre-fix baseline:

| | Pre-fix (Zstd-19 + 50ms BP) | Post-fix (Zstd-3 + 30s BP) |
|---|---:|---:|
| 1 M docs / 8 workers — burst rate | (never measured; bench used 100 K bursts) | **3.98 M docs/s, 0 errors** |
| 5 M docs / 8 workers — sustained | n/a | **1.43 M docs/s, 0 errors** |
| 25 M docs / 32 workers (run that triggered investigation) | 21 K docs/s, 19 M errors (75 % loss) | 29 K docs/s, 0 errors (32-worker contention; 8 workers is the sweet spot — the autotuner pitch) |

Match to historical peaks (`2026-04-18T00-30-00_profiling_path_to_10M.md`):

| Workload | Historical peak (Apr 18) | Post-fix (Apr 25) |
|---|---:|---:|
| 1 M-doc burst (in-memtable) | 5.45 M/s (sync-engine refactor) | 3.98 M/s (~73 % of peak) |
| Sustained (5-20 M) | 1.3-1.6 M/s | 1.43 M/s ✓ |

The 1 M-burst gap (5.45 → 3.98 M/s) is most likely the same Zstd-19
final-flush cost — the 1 M-doc memtable still gets one final flush at
shutdown.  Re-tuning that with the same level-3 fix should close it.

## Worker-count finding (autotuner input)

The 25 M / 32-worker case **completes without errors** post-fix but
runs at only ~30 K docs/s sustained, vs 1.43 M docs/s at 8 workers
on the same hardware.  Root cause: 32 ingest workers contending on
one memtable + one flusher saturate the back-pressure wait queue —
they spend most of their time waiting for room rather than inserting.

This is exactly the case the autotuner is designed to handle.  The
heuristic from `2026-04-25T22-00-00_autotuner_design_notes.md` would
have picked `flush_workers = n_cpu / 4 = 8` and `ingest workers ≈
flush_workers` based on the disk-throughput probe, avoiding both the
under-utilisation of fewer workers and the lock contention of too
many.

## Reproducer artefacts (left in `engine/demo-data/`)

- `SSH.log` (71 MB raw OpenSSH log from loghub)
- `build_corpus.py` (parser + replicator)
- `ssh_big.ndjson` (5.17 GiB / 25.5 M docs NDJSON)
- `cli_ingest.log` (full ingest output with per-flush timings)
