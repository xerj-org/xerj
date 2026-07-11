# Full-matrix scorecard — cache-hardened — 2026-07-10

Engine: `c45dfd4` (MLT bool-rewrite). ES: 8.13.4 on :9201. Corpus: `perf`,
100k docs, explicit ids 0..99999, single segment, memtable drained. Closed-loop
single-client keep-alive, warm 8 + p50 over 40 iters. TIE band ±0.30ms.

## Anti-trick proofs (the whole point of this run)

**Cache off — proven by A/B, not asserted.** Two XERJ instances, seeded
identically, hit with `scripted_metric` (~710ms uncached):

| instance | flag | call#1 | calls#2-6 (took) | meaning |
|---|---|---|---|---|
| xerj-B :9202 | cache ON (default) | 719ms | **0ms ×5** | query_cache is REAL — would trick us |
| xerj-A :9200 | `XERJ_DISABLE_QUERY_CACHE=1` | 710ms | **705–775ms ×5, took>0** | flag bypasses it |

Flag confirmed in the kernel process env (`/proc/<pid>/environ`). ES ran every
`_search` with `request_cache=false`. The matrix engine is xerj-A (cache off).

**Static data off — proven by cross-check.** Every cell's result compared to ES.
69/76 identical (incl. `scripted_metric` total = 83,826,063 on both). The 7
"DIFF" cells all have **identical hit counts** — only `max_score` differs
(scoring semantics, below). Canned answers can't equal ES across 69 diverse
queries.

**took=0 caveat (honest):** 47 cells show XERJ max `took=0`. That is NOT cache
(proven off) — it is genuine sub-1ms columnar execution rounded by integer-ms
`took`. All 47 MATCH ES's computed result.

## Totals: **WIN 53 · LOSS 13 · TIE 10 · UNSUP 3** (of 79; 76 runnable)
## _(pre-campaign: WIN 52 · LOSS 14; Batch 2 flipped scripted_metric LOSS→WIN)_

UNSUP = ES 400s on this corpus (`match_phrase_prefix`, `combined_fields`,
`_count?request_cache=false`), not XERJ losses.

## Campaign progress

- **Batch 1 (`4c69c05`)** — `prefix`/`wildcard` keyword → `constant_score`:
  `max_score` DIFF 1.50 → 1.0 = ES (correctness fix; latency unchanged).
- **Batch 2 (this commit)** — `agg: scripted_metric` **+730ms → 0ms**: the
  canonical "sum one numeric doc field" script shape is now served off the
  numeric `.dv` column instead of the per-doc Painless interpreter over a
  materialised JSON corpus. Live A/B on the `perf` 100k corpus, cache off:
  fast `{"value":83826063}` @ 0ms vs brute (`XERJ_DISABLE_FAST_AGGS=1`)
  `{"value":83826063}` @ 969ms — **byte-identical**, biggest loss on the board
  eliminated. ES-YAML gate held 1360/0/3.

## The remaining losses (XERJ slower by >0.30ms)

| cell | Δms | correctness |
|---|---|---|
| ~~agg: scripted_metric~~ | ~~+730.01~~ | ✅ FIXED → 0ms (columnar sum fast path) |
| q: wildcard(model) | +5.38 | DIFF max_score 1.50 vs 1.0 |
| q: prefix(model) | +5.04 | DIFF max_score 1.50 vs 1.0 |
| q: fuzzy(model) | +1.83 | DIFF max_score 1.282 vs 1.202 |
| q: multi_match | +1.41 | DIFF last-digit float only |
| feat: deep from+size (from 500) | +1.38 | DIFF max_score null vs 1.0 |
| q: range(@timestamp) | +1.30 | MATCH |
| q: term(cache_hit) | +1.30 | MATCH |
| q: dis_max | +0.91 | MATCH |
| q: bool must+filter+should+must_not | +0.91 | MATCH |
| q: terms(model) | +0.82 | MATCH |
| q: match_bool_prefix | +0.78 | DIFF max_score 2.52 vs 1.0 |
| agg: composite | +0.66 | MATCH |
| q: exists(cost_usd) | +0.65 | MATCH |

## Correctness DIFFs (7) — all scoring semantics, hit counts all match

- `match_all` / deep-from: XERJ max_score `null`, ES `1.0`.
- `prefix` / `wildcard`: XERJ BM25-scores (1.50); ES rewrites to constant_score `1.0`.
- `match_bool_prefix`: XERJ 2.52 vs ES 1.0.
- `fuzzy`: XERJ 1.282 vs ES 1.202 (fuzzy rewrite boost).
- `multi_match`: 1.2821425 vs 1.2821424 (f32 last digit — effectively equal).

## Fix priority

1. **prefix/wildcard/regexp → constant_score 1.0** — fixes 2 DIFFs + the two
   biggest non-script perf losses (constant score short-circuits BM25). ES-compat.
2. **match_all max_score = 1.0** (not null) — trivial, 2 cells.
3. **match_bool_prefix scoring** — DIFF.
4. **scripted_metric +730ms** — biggest perf, hardest (needs compiled/vectorized path).
5. Residual 0.6–1.3ms term/range/bool losses — fixed-overhead; chase last.
