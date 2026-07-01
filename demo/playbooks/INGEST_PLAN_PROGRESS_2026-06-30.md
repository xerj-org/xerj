# Beat-ES Ingest/Tail Plan — Progress Report (2026-06-30)

Execution log for `INGEST_PLAN_BEAT_ES.md`. All numbers measured on this
box (AMD Ryzen AI MAX+ 395, 32 cores) with the reconstructed harness in
`scratchpad/` (gitignored). Guardrail held at **every** step: full ES-YAML
conformance **1326 passed / 0 failed**.

## Headline: the ingest gap is essentially closed; reads win outright

Real head-to-head vs **Elasticsearch 8.13.4** at 1M docs, single sequential
client, both single-node / security-off / same box
(`demo/playbooks/BENCHMARK_VS_ES.md`):

| Axis | Baseline (pre-work) | After P2.1+P2.3+P3.2 |
|---|---|---|
| **Bulk ingest** | ES 4.9× ahead (XERJ 0.20×) | **XERJ 0.85–0.94× — near parity** (XERJ ~95k vs ES ~103–111k docs/s) |
| **Read p50** | XERJ 1.5–1.85× | **XERJ wins every op 1.3–2.2×** |
| **Read p99 (steady state)** | ES tighter | **XERJ wins every op** (p99 1.5–2.2ms vs ES 2.5–3.7ms) |
| **Read p99 (during heavy merge/ingest)** | ES tighter | ES still tighter — remaining transient (see §Remaining) |

The 4.9× ingest deficit → ~parity is the transformation. XERJ now uses many
cores per bulk request (was 1.0 of 32).

## What landed (each gated 1326/0, committed as xerj-org)

- **P1.1** (`1a9f75b`, prior session) — route auto-id `_bulk` through the
  correctness-fixed turbo path. ~30k docs/s, still ~1 core.
- **P2.1** (`53da9b0`) — **intra-request shard fan-out** in
  `index_batch_turbo_raw`: rayon-parallel JSON parse → serial in-order
  schema evolve → bucket doc indices by their own shard → rayon-parallel
  insert holding each shard lock once. **30.7k→112k docs/s @400k (3.6×),
  22k→98.8k @1M (~4.5×); CPU 104%→347% avg/1690% peak.** Crosses ES at
  mid-scale; the 31k→22k scale decay is gone.
- **P2.3** (`8b16dbb`) — **debounced snapshot persistence**: stop the
  O(total-segments) `to_vec_pretty`+fsync-rename on every finalize;
  persist once per WAL-maintenance tick, before prune; `to_vec` compact.
  Durability invariant preserved (persist-before-prune; orphan recovery +
  WAL replay dedup already tolerate a stale snapshot). **Crash-recovery
  verified**: 800k docs / 32 segments, `kill -9`, restart → 800,000/800,000
  recovered, no loss, no double-count. Throughput-neutral at 1M (win is
  redundant-work/write-amp elimination that compounds at scale).
- **P3.2** (`4e5b7cc`) — **additive cache invalidation**: stop wiping the
  per-segment `dv_cache`/`stored_value_cache` on every flush (segments are
  immutable → a new segment can't invalidate existing ones; the blanket
  clear forced cold mmap+decode = the flush-coincident read-p99 spike).
  Keep the `dataset_version` bump (invalidates the query_cache by key);
  merges evict only their dropped segment ids. **Fixed the cache-cold
  spikes: date_histogram p99 123ms→2.04ms, stats 32ms→2.49ms.** Staleness
  test (warm cache → update+delete+forcemerge → re-read) confirms no stale
  reads.

## Evaluated and reverted (kept the tree honest)

- **P3.3 dedicated/bounded merge pool** — implemented (confine the merge
  FTS `add_documents_parallel` to a bounded rayon pool via `install()`),
  conformance-green, but A/B'd both post-ingest and under **sustained
  concurrent ingest+read**: bounded (8 threads) vs saturating (32) were
  **statistically identical** — the during-ingest read-p99/max spikes did
  **not** move. Root cause is therefore NOT merge CPU-theft. Reverted per
  the measure-before-commit discipline. (`scratchpad/during_merge_probe.sh`,
  `scratchpad/sustained_mixed_probe.sh`.)

## Remaining (the one axis where ES still wins): during-ingest tail

Under **sustained heavy ingest**, XERJ shows occasional multi-second query
maxes (server-side `took_ms` up to ~16–22s), worst on **aggregations**
(terms/cardinality do heavy parallel doc-values scans). Meanwhile p50/p95
stay excellent (1.3–2.8ms) and **steady-state p99 beats ES on every op**.
So this is a transient, not a serving-time regression.

Characterized, not yet fixed (needs larger/riskier work, best done focused):
- **Global rayon pool contention** — foreground search/agg parallel scans
  share the global pool with background flush FTS build
  (`do_flush_shard` → `add_documents_parallel`, index.rs:6782), merge FTS
  build, and P2.1 ingest parse/insert. A dedicated **search** pool (or
  bounding ALL background parallel work) is the clean fix; confining flush
  alone trades ingest throughput for tail and needs careful measurement.
- **Flush-drain lock contention (P3.1 freeze-and-swap)** — the plan's named
  fix: never flush the shard being actively written; hand a frozen buffer
  to the background task so reads/ingest never block behind the drain.
  Medium risk (NRT read-visibility of the frozen-but-unflushed buffer) →
  worktree-agent + hard conformance gate.
- **P2.2 no-reparse flush** — carry parsed Value+tokens into
  `do_flush_shard` (it currently re-parses each doc, index.rs:6685) to cut
  flush CPU/duration; could both push ingest past ES and shorten flush
  stalls. L-effort.

## Reproduce
- Harness (gitignored, `scratchpad/`): `conformance.sh` (boot + full
  es-yaml, expect `1326 passed · 0 failed`), `ingest_measure.sh <N>`
  (single-client docs/s + `top` CPU%), `recovery_test.sh` (crash-safety),
  `during_merge_probe.sh` / `sustained_mixed_probe.sh` (tail A/B).
- Real ES: 8.13.4 on :9201 (`ES_JAVA_OPTS=-Xms4g -Xmx4g bin/elasticsearch`),
  xerj on :9200, `node demo/playbooks/bench-vs-es.mjs 1000000 http://localhost:9200 http://localhost:9201`.
