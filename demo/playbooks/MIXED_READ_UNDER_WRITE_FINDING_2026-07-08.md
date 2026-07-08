# Mixed read-under-write is STILL the last real perf gap (2026-07-08)

## Why this doc exists
This session made **uncached SEGMENT reads** single-digit ms (doc-values
prefilters for term/terms/range/bool + F2 offset random-access + bool
must_not/should widening — commits 7219c78, 8fc5928, 77586f0, c3bd84b,
98b162c). That could be misread as "reads now beat ES everywhere." It does
NOT. Those fixes are all on the **committed-segment scan path**. The
**mixed read-under-write** path (reads issued while a high-rate writer runs)
is untouched by them and remains the one genuine perf weakness — consistent
with the long-standing 5 mixed-p99 LOSE rows (62–152 ms vs ES 2–19 ms) in
the scorecard.

## What was measured this session (honest, with caveats)
Harness: `scratchpad/mixed_p99_probe2.py` — 1M-doc preload + flush, then N
reads/shape (novel params) while 4 threads append. **Caveat: the writer was
over-aggressive** (throttle loose → ~240k/s actual, IDs eventually collide,
corpus grows fast), so the absolute numbers are WORSE than the clean
`bench-matrix.mjs` 120k/s benchmark and should NOT be quoted as the scorecard
number. What it does establish:

- With background **merges ON**, the server's OWN `took_ms` (server-side, not
  client) logged **~19.5 s** for `range(n)` window aggregations, recurring
  **~every 20 s** while most reads stayed sub-2 ms. So the slow reads are
  **periodic and merge-coincident**, not uniform.
- The selective-prefilter work does NOT help here: these are broad-ish
  `range(n)` window aggs (≈50k-doc window over a growing corpus), and the cost
  is in the **memtable / live-snapshot + segment-fan-out + merge-coincident**
  path, not the committed-segment scan the prefilters accelerate.

## NEW flag — possible reader-vs-segment-GC race (needs a CLEAN repro)
Under the same heavy sustained write, a `range` read returned
`store_exception: No such file or directory (os error 2)`. Hypothesis: a read
slow enough to span (a) a merge completing and (b) the **deferred** input-file
removal grace period expiring loses a segment file mid-read. The merge log
shows `removed_files=0 … deferred=true`, i.e. input files are GC'd later — safe
ONLY if no reader outlives the grace. A multi-second slow read (see above) can
outlive it. If confirmed, the slow-read weakness and this error share ONE root
cause: **reader snapshots don't pin segment files for the read's duration.**
NOT yet reproduced cleanly (only seen under the brutal harness) — do not
over-claim; confirm with a minimal repro before treating as a shipped bug.

## Root-cause status (measure-first — do NOT guess-patch the hot path)
CONFIRMED: merge-coincident multi-second range-agg reads under heavy write.
Prior art (memory [[mixed-p99-root-cause]]): a dedicated-merge-rayon-pool A/B
already **refuted CPU-theft** as the cause. 19.5 s ≈ a full 1M-doc merge, so a
**lock or file-lifecycle dependency held across the whole merge** is the prime
suspect, not CPU. Merge structure: `run_merge_once` → `merge_pass_locked`
(index.rs 2396/2481); readers take a cheap `store.snapshot()` (Arc clone) so
they should NOT block on the merge — which sharpens the puzzle: if reads don't
block on the merge lock, WHY are they 19.5 s? Candidates to instrument next:
1. Does the slow read actually block on a lock (`schema.read().await`, a shard
   memtable lock, or the store segment-list lock) that merge/flush holds?
2. Or is it O(num_segments) × per-segment open/prefilter over a churny segment
   set (merge transiently doubles the set: inputs + output both live)?
3. Or query_cache permanent cold-miss recompute (dataset_version bumped every
   flush) — but that was ~60-90 ms historically, not 19.5 s, so it's at most a
   contributor.

## Next-iteration plan (focused, gated)
1. Clean minimal repro: modest sustained writer (~120k/s, non-colliding, no
   early cap), merges ON, measure SUCCESSFUL range-agg p99 + capture a stack
   sample (or add temporary `XERJ_DBG_MIXED` timing spans around
   memtable-scan / segment-fan-out / lock-acquire in the read path) to
   attribute the 19.5 s to a specific span. Isolate one variable at a time.
2. Separately, a clean repro of the `store_exception` (slow reader + concurrent
   merge + deferred GC). If real, fix by pinning segment files for a
   snapshot's lifetime (epoch/refcount on the segment set the reader holds).
3. Only then implement — likely: (a) reader snapshot pins files against GC
   (closes the race and lets deferred GC stay), and/or (b) cut the read's
   per-segment fan-out cost so it never runs multi-second under a churny
   segment set. Hard gate: full ES-compat YAML 1360/0 + the clean mixed repro
   showing p99 back to low-double-digit ms + no store_exception.

Use a worktree agent for the implementation (delicate hot-path, per the
established pattern) with the clean repro as the win metric.
