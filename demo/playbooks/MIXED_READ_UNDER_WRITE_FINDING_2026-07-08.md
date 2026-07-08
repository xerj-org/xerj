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

## UPDATE — gdb root-cause session (2026-07-08, same day)
Built a symbolicated binary (`RUSTFLAGS=-C force-frame-pointers=yes
CARGO_PROFILE_RELEASE_STRIP=false CARGO_PROFILE_RELEASE_DEBUG=1`; the release
profile has `strip=true` so a plain `debug=1` is NOT enough) and attached gdb
during live stalls (`scratchpad/gdb_catch.sh` fires a range-agg read, snapshots
all threads if it's still running after 0.6 s). Caught 8.5 s / 20.9 s / 7.5 s
reads with full stacks. Findings:

**CONFIRMED (transferable):**
- The stall is **LOCK-BOUND, not CPU-bound.** During a 20.9 s read: CPU ~306 %
  (~3 of 32 cores), and **358–362 of 363 threads SLEEPING** (`/proc` state S,
  not D). This definitively refutes CPU-scheduling contention (matches the
  prior dedicated-merge-pool A/B refutation) — the read WAITS, it does not work.
- The contention is on **`FtsMemtable` per-shard `parking_lot::RwLock`.** Stacks
  show readers parked in `RwLock::read`→`lock_shared_slow` and a writer parked
  in `RwLock::write`→`wait_for_readers` — the classic parking_lot
  writer-preference cascade (new readers queue behind a waiting writer). Also
  ~294 `xerj-rt` runtime threads exist (worker+blocking pool), almost all
  parked.
- The slow range-agg READ itself has NO frames on any stack → it is
  **async-suspended at an `.await`**, waiting, not running.

**CRITICAL REPRO CORRECTION (honesty — my numbers were unrepresentative):**
`scratchpad/load_driver.py` wrote with **explicit `_id`s** →
`process_bulk_with_opts`→`index_document`→`ShardedFtsMemtable::remove`
(memtable.rs:619) takes a shard **write lock PER DOCUMENT** (remove-before-
insert). The real benchmark (`bench-matrix.mjs:424,470`) writes **auto-id
`{"index":{}}`** → the turbo/chunked path (one write lock per ~512-doc chunk).
So my ~20 s stalls are **amplified ~100×** by an unrepresentative write path;
the real magnitude is the documented **62–152 ms**. Additionally, the captured
readers were mostly `shard_loads()` (index.rs:2290) — the **flush SCHEDULER**,
i.e. write-side self-contention, NOT the read path. So I did NOT yet capture the
representative read-under-write contention. (Disciplined check: I verified the
benchmark's write mode BEFORE "fixing" the per-doc `remove()` — which would have
been chasing an artifact.)

## Corrected next-iteration plan (focused, gated)
1. **Representative repro:** rewrite the load driver to use **auto-id
   `{"index":{}}`** (turbo path) like the real benchmark, sustained ~120k/s,
   merges ON. Re-attach gdb during a stall and identify (a) which lock the
   range-agg READ future ultimately awaits/blocks on, and (b) which thread
   HOLDS it and for how long. The mechanism is almost certainly still
   `FtsMemtable` shard RwLock, but under the turbo writer the magnitude and the
   exact holder differ — capture it, don't assume.
2. Likely fix directions (design carefully, do NOT rush): make read-path shard
   access not starve under writers — e.g. snapshot the memtable via an Arc/
   epoch so reads are lock-free, or split the read fold so it never holds
   `s.read()` across expensive work, or bias the lock toward readers. Each is a
   delicate hot-path change.
3. The `store_exception` file-race flag still stands — clean repro separately;
   fix by pinning segment files per reader snapshot.
Hard gate for any fix: full ES-compat YAML 1360/0 + the representative auto-id
repro showing mixed p99 back to low-double-digit ms + no store_exception. Use a
worktree agent for the implementation.

Artifacts: `scratchpad/gdb_catch.sh`, `scratchpad/load_driver.py` (NOTE: fix to
auto-id), `scratchpad/gdb_dump_*.txt` (symbolicated stacks).

## UPDATE 2 — representative repro CONFIRMS the stall + fix target located
Re-ran with AUTO-ID turbo writes (`scratchpad/mixed_repr.py`) on the production
(stripped) binary. The stall REPRODUCES: server-side `took_ms=17030/17256` on
range(n) window aggs, 70 slow queries. So it is NOT just the explicit-id
amplifier — the mechanism is real for the representative write path. (My probe
over-drove the corpus so the absolute 17s is still inflated vs the documented
62-152ms, but the shape is confirmed.)

FIX TARGET LOCATED (index.rs `mem_snapshot`, ~5266-5327): for a size:0 agg WITH
a term/range/bool FILTER, the fused columnar memtable path is GATED OFF because
`request.aggs.is_some()` (line 5292). Correctness (the agg must fold ALL
matching docs, not just `materialisation_limit`) then forces the `DocsForScan`
arm → `mem.all_docs_with_sources_arc()` (line 5325) = materialize EVERY memtable
doc. Under a write flood the memtable holds ~all recent docs, so this is O(all
memtable docs) built under `s.read()` (the lock hold) then folded. The SEGMENT
side of exactly this was fixed in commit 5098645 ("filtered size:0 aggs →
columnar-filtered, 7.8s→~10ms"); the MEMTABLE side was not.

FIX DIRECTION: give the memtable filtered-agg a columnar range/term/bool
doc-values position fold — enumerate ONLY matching positions via the numeric/
keyword DV index (like `doc_values_bool_query` already does, but UNBOUNDED for
the agg case) and fold status/stats over just those, holding `s.read()` only
briefly. Cuts O(memtable)→O(matching) AND shrinks the lock hold (helps the
lock-bound contention gdb confirmed). CRITICAL correctness: fold ALL matches
(memtable matches can exceed materialisation_limit); verify with a dataset where
the filtered set > 256 in the memtable.
