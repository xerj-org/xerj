# CRITICAL FINDING (2026-07-01): XERJ read-perf wins were a query-cache mirage

## What we believed
Prior head-to-head benchmarks reported XERJ winning read latency vs
Elasticsearch by 1.3–2.2× on p50 and even winning steady-state p99. The
"beat ES on reads" story rested entirely on these numbers.

## What is actually true
Those benchmarks repeated the **same query** hundreds/thousands of times
against a **static** index. XERJ's result cache (`query_cache`, keyed by
`(query_hash, dataset_version)`) served every call after the first. We were
measuring **cache hits**, not query execution.

Measured directly (single client, NO write load, 500k docs, 16 flushed
segments, `scratchpad/` diagnostics):

| query | call #1 (uncached) | call #2/#3 (cached) |
|---|--:|--:|
| match_all size:10 | **2.28 s** | 0.00 s |
| term(status:ok) size:10 | **2.42 s** | 0.00 s |
| range size:10 (novel) | **2.2–2.4 s** | — |
| terms agg (size:0) | **8.48 s** | 0.01 s |

Every **distinct** query pays 2–8 s. ES does the same uncached in single-digit
ms. So XERJ's true uncached read latency is ~1000× ES; it only looked fast
because benchmarks hammered one cached query on a static dataset.

This is also the real explanation for "reads collapse under write": a write
bumps `dataset_version`, invalidating the query_cache, so every read reverts
to the 2–8 s uncached path — under sustained write, reads hit 5–137 s.

## Root cause (characterized, code-located)
Two distinct O(N) paths, both masked by caches:

1. **Hit materialization is O(total matches), not O(from+size).** Proof:
   `match_all size:0` = **0.01 s** (matching + counting are fast) but
   `match_all size:10` = **2.28 s**. Fetching 10 docs scans/decodes ALL
   matches' stored `_source` then truncates. The code even documents that
   `size:0` was special-cased away from the "full stored-doc scan" but
   `size>0` still does it (`crates/xerj-engine/src/index.rs` ~4002–4055,
   segment stored-scan; memtable side `all_docs_with_sources`/`all_doc_ids`
   ~3826–3894).
2. **Cold doc-values decode per segment for aggregations.** First agg over a
   segment's column decodes the whole column (8 s for 16 segments), cached
   after (`dv_cache`). Under write, new segments are cold → aggs slow again.

CPU is idle (2–11 of 32 cores) during the stalls → not CPU-bound; it's the
serial O(N) scan/decode + lock handoff, not parallel compute.

## Why the harness now catches this
`bench-matrix.mjs` (Phase 0) already added `track_total_hits=true` and a
mixed read-under-write mode; the mixed mode is what exposed the collapse.
To measure UNCACHED steady-state too, the harness should vary query params
per iteration (novel term/range values) so the query_cache can't serve
repeats — TODO before the next scorecard so read cells reflect reality.

## The fix (new top priority — supersedes tail/ingest/disk)
**F1. Bounded hit materialization** — collect only top-(from+size) doc refs
(heap by score/sort), then fetch `_source` for those alone. Target: term/
match_all/range size:10 from ~2.4 s → single-digit ms uncached. Biggest lever.

**F2. Faster / precomputable agg doc-values** — keep decoded columns warm
across flushes (additive, like P3.2) and/or decode lazily per-agg-field only;
consider persisting decoded/bit-packed columns so cold-agg isn't O(column).

**F3. Re-baseline honestly** — after F1/F2, re-run `bench-matrix.mjs` with
per-iteration novel query params; report UNCACHED p50/p99. Only then are the
read cells trustworthy.

Guardrail unchanged: ES-YAML conformance 1326/0 at every step; this is core
search-path surgery → worktree agent + hard gate + the `scratchpad/`
uncached diagnostics as the win metric.

## Honest status correction
The project is NOT "reads already beat ES." It is: ingest near-parity (100k
wins, 1M/8-client behind), disk 2× larger, and **reads are ~1000× slower
uncached** — the read engine needs fundamental work before any "beats ES on
reads" claim holds. Everything else is secondary to F1.

---

## UPDATE 2026-07-08 — F1 landed + selective-query prefilters; uncached reads now single-digit ms

The mirage is largely resolved. F1 (bounded hit materialisation) shipped
earlier (b79a8c2, a93354a). This session added the missing piece F1 didn't
cover — **selective queries** whose bounded hit collector never fills, so the
size>0 early-break never fired and they walked/parsed the whole section:

- **memtable numeric term/terms** used the doc-values fast path (was keyword-
  column-only → O(N) `_source` scan). 7219c78.
- **segment selective term/terms** got a doc-values position pre-filter
  (numeric = degenerate `[v,v]` range on the sorted index; keyword = the
  ordinal's positions), so the scan parses only matching positions. 8fc5928.
- **segment conjunction bool** (`filter:[term,range,…]`) gets a *superset*
  pre-filter from its most-selective conjunct — the scan already re-runs
  `doc_matches_query` per admitted doc, so a superset is sufficient. 77586f0.

Measured UNCACHED (novel params every call, 300k-doc flushed segment; median):

| query | before | now |
|---|--:|--:|
| match_all size:10 | 2.28 s (mirage-era 500k) | **2.4 ms** |
| term (numeric, selective) | ~2.4 s | **3.1 ms** |
| term (unique keyword) | ~100 ms (100k) | **2.7 ms** |
| range (novel bound) | ~2.4 s | **3.2 ms** |
| terms (5 novel values) | O(N) scan | **9.8 ms** |
| bool `uid AND range` (selective) | O(N) scan | **3.7 ms** |
| terms agg (low- and 3000-card) | 8.48 s (first touch) | **2.4 / 6.1 ms** |
| stats agg | — | **3.4 ms** |

All conformance-gated: full ES-compat YAML **1360 / 0** at every step.

### Known remaining read costs (honest, mechanism confirmed by instrumentation)
- **A selective term whose matches are SPREAD across all 16 shard-segments**
  (e.g. a ~3000-value field, ~100 matches / 300k, ~7 per shard): ~26–35 ms.
  Mechanism (measured, not guessed): the pre-filter *build* is ~8 µs/segment —
  negligible. The cost is that EVERY one of the 16 sharded stored-sections has
  a few matches, so all 16 get opened + decompressed + scanned (~2 ms each) on
  a cold/novel query. A term confined to ONE shard (e.g. a unique id) returns
  `Some(∅)` for the other 15 segments → they are skipped → ~3 ms. So the lever
  is **per-segment cold stored-section decompress** (retain/warm decoded
  sections across a varied query workload, or cheapen the decode), i.e. **F2**,
  NOT the term pre-filter. **Next lever.**
- **bool with `should` / `must_not`, or all-broad conjuncts**: no single-
  conjunct superset is valid (or none is selective) → full scan (still exact,
  just unoptimised).

Net: the common uncached read shapes are now **single-digit ms**, not seconds.
The remaining lever is F2 (per-segment decoded-section warmth) for selective
terms fanned across every shard — a decompress cost, not a scan or build cost.
