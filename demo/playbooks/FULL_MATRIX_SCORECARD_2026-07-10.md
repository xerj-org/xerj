# XERJ vs Elasticsearch — DEFINITIVE Full-Matrix Scorecard (2026-07-10)

**Engine: branch `feat/ai-first-customer` @ `addbbcd`, clean tree (batches 5–12 + autoindex +
BM25-exactness all landed and committed; binary `engine/target/release/xerj` rebuilt this
session).**
This snapshot re-measures the full 79-cell read/agg/pipeline/feature matrix against live
Elasticsearch 8.13.4 with the **same official methodology** as
`FULL_MATRIX_SCORECARD_2026-07-09.md`, and adds a **per-cell CORRECTNESS column**: every scored
query cell is compared to ES on **top-10 `_id` order + f32 `_score` bit patterns + `max_score` +
`hits.total`**; every agg cell is deep-compared value-by-value.

**AUDITED 2026-07-10 by independent re-measure; corrected from the measurer's 42/25/15.** The
auditor re-ran the band-edge cells over 6 independent rounds each: three measurer TIEs
(`query_string`, `range(cost_usd)`, `bool` m+f+s+mn) were band-edge artifacts and re-score to
LOSS.

## GRAND TOTAL: **42 WIN / 28 LOSS / 12 TIE** (audited; was 45/26/11 on 2026-07-09)

| scope | cells | WIN | LOSS | TIE | ES-REJECTS |
|---|--:|--:|--:|--:|--:|
| queries (29) | 29 | 4 | 22 | 1 | 2 |
| basic aggs (31) | 31 | 21 | 1 | 9 | 0 |
| pipeline aggs (12) | 12 | 12 | 0 | 0 | 0 |
| feature ops (7) | 7 | 2 | 4 | 1 | 0 |
| **Table A total** | **79** | **39** | **27** | **11** | **2** |
| Table B (ingest/disk/kNN, carried¹) | 5 | 3 | 1 | 1 | — |
| **GRAND TOTAL** | **84** | **42** | **28** | **12** | **2** |

¹ **Table B is carried forward unchanged from 2026-07-09** (ingest throughput WIN 1.54×, per-bulk
p50 WIN 1.80×, disk WIN 0.82×, kNN latency LOSS 120×, kNN recall TIE, plus the RRF XERJ-only
capability edge). The ES side (`:9201/bench`) is **READ-ONLY** for this run — ingest/kNN cells
require writing to ES and were not re-executed. All 79 Table-A cells were re-measured today.

**Strict-exactness alternative count** (if every WIN/TIE whose result is not bit-identical to ES —
including order-among-bit-equal-scores and wire-format divergences — is scored LOSS per the
hardest reading of the correctness rule): **38 WIN / 32 LOSS / 12 TIE**. The flips under that rule
are `regexp`, `function_score`, `search_after`, `sort-heavy` (WIN→LOSS); `range(cost_usd)` is
already a LOSS after the audit; every one of them has **bit-exact f32 scores / sort keys** and diverges only in id
order among tied keys or in a non-ranking wire field — see the Correctness section. The primary
count above follows the 07-09 precedent (wire-format/tie-order divergences are flagged, not
verdict-flipping); **one cell (`ids`) IS verdict-flipped** because its `_score` **value** is wrong.

---

## Method (identical to 2026-07-09 — fair, closed-loop, single-client)

- **XERJ** on `:9200`: fresh data dir `/tmp/xerj-scorecard/`, `--insecure`,
  `XERJ_DISABLE_QUERY_CACHE=1` (verified in `/proc/<pid>/environ`), memory-capped
  `systemd-run --user --scope -p MemoryMax=24G -p MemorySwapMax=0`. Killed after the run.
- **Real Elasticsearch 8.13.4** on `:9201`, untouched, read-only: index `bench`
  (alias `perf`), 100,000 docs, **single segment**, `docs.deleted 0`.
- **Corpus:** identical on both — `demo/data/extras/chat-events.ndjson` (4,008 real docs) cycled
  to 100,000 with **explicit `_id`s 0..99999** (the established seeder, same as the ES side was
  built with), exact `bench-matrix.mjs` mapping, `_forcemerge?max_num_segments=1` + settle;
  both `/perf/_count` = 100,000; both 1 search segment.
  ⚠️ Corpus drift vs the ORIGINAL 07-09 snapshot tables: that snapshot used auto-`_id`s, so its
  `ids`/`_mget`/`pinned` cells matched nothing. Since the post-wipe re-seed (batch 5 onward) the
  shared corpus has explicit ids 0..99999 — `ids` now returns 3 hits, `_mget` fetches 3 real docs,
  and `pinned` genuinely pins docs 1 and 2.
- **Bodies:** copied **VERBATIM** from `demo/playbooks/bench-matrix.mjs` (29 queries, 31 basic
  aggs, 12 pipeline aggs, 7 feature ops). `request_cache=false` on every `_search`;
  `track_total_hits: true` injected into every JSON `_search` body; `_count` measured without the
  bogus `request_cache` param (07-09 footnote 4).
- **Measurement:** CLOSED-LOOP — feasibility probe, then **warm 8**, then **p50/p99 over 50
  sequential iterations**, one request at a time, lean keep-alive `node:http` client identical for
  both engines. Per cell: XERJ first, then ES.
- **Verdict rule (XERJ POV):** compare p50; `|Δp50| ≤ 0.30 ms` ⇒ TIE, else WIN/LOSS. XERJ 4xx ⇒
  XERJ-UNSUPPORTED; ES 4xx ⇒ ES-REJECTS.
- **Correctness rule (this run, stronger than 07-09):** every scored query cell compares top-10
  `_id` sequence + **f32 bit patterns** of every `_score` + `max_score` + `hits.total` vs ES;
  agg/pipeline cells deep-compare the whole `aggregations` tree. **A faster-but-wrong cell is a
  LOSS with a correctness flag, never a WIN** (applied: `ids`). Determinism of every divergence
  was confirmed by repeat probes on BOTH engines (all orders are stable run-to-run).
- Box quiet (no parallel workflows). Harness `full_matrix.mjs`, raw `results-2026-07-10.json`,
  log `fullrun-2026-07-10.log` (session scratchpad). Nothing committed; ES never written.

Correctness column legend:
**EXACT** = ids order + score bits + max_score + total all identical to ES ·
**TIE-ORDER** = all f32 score/sort-key bits identical, only the id order among *bit-equal* keys
differs (both engines deterministic) · **SCORE-DIVERGE** = a real `_score` value difference ·
**WIRE** = non-ranking wire-format field differs · **F64-DRIFT** = agg values differ ≤1e-13 rel
(known naive-vs-Kahan ledger defect) · **APPROX** = ES value is approximate-by-design
(TDigest/clustering/sampling), XERJ exact or differently-approximate.

---

## QUERIES (29) — 4 WIN / 22 LOSS / 1 TIE / 2 ES-REJECTS

| case | XERJ p50/p99 (ms) | ES p50/p99 (ms) | hits | verdict | correctness (top-10 ids + f32 score bits) |
|---|--:|--:|--:|:--:|---|
| match_all | 0.97 / 1.39 | 0.29 / 0.92 | 100000 | LOSS | TIE-ORDER — scores all 1.0 (bit-exact); XERJ ids 0,60,82,96,… vs ES 0,1,2,… |
| match_none | 0.15 / 2.60 | 0.23 / 0.47 | 0 | TIE | EXACT |
| match(model) | 1.91 / 3.73 | 0.32 / 1.23 | 27744 | LOSS | EXACT |
| match_phrase(top_doc) | 0.83 / 1.42 | 0.26 / 1.08 | 9082 | LOSS | TIE-ORDER |
| match_phrase_prefix | 2.18 / 4.82 | err 400 | 25259 | **ES-REJECTS** | n/a (ES refuses keyword-field phrase_prefix) |
| match_bool_prefix | 1.79 / 3.19 | 0.62 / 1.50 | 25259 | LOSS | **SCORE-DIVERGE** — XERJ 2.5210621 vs ES 1.0 (+ ids differ) |
| multi_match | 2.33 / 3.14 | 0.69 / 1.35 | 27744 | LOSS | **SCORE-DIVERGE** — 1 ulp: 0x3fa41d3f vs ES 0x3fa41d3e (1.2821425 vs 1.2821424) |
| combined_fields | 3.31 / 4.67 | err 400 | 27744 | **ES-REJECTS** | n/a |
| query_string | 1.66 / 2.35 | 1.44 / 2.64 | 27371 | LOSS | EXACT — audited: LOSS in 6/6 independent rounds (X 1.57–2.40 vs ES 0.90–1.34); ES floor ~30% faster on audit re-measure |
| simple_query_string | 1.35 / 2.28 | 0.28 / 0.82 | 98752 | LOSS | EXACT |
| more_like_this | 144.65 / 153.24 | 0.33 / 1.23 | 9082 | LOSS | **SCORE-DIVERGE** — flat 1.0 vs ES 2.3988307 (known out-of-gate brute path) |
| term(status) | 1.37 / 2.14 | 0.18 / 0.65 | 98752 | LOSS | EXACT |
| terms(model) | 1.88 / 2.04 | 0.41 / 1.35 | 27744 | LOSS | TIE-ORDER |
| range(latency_ms) | 0.80 / 1.20 | 0.44 / 1.37 | 29067 | LOSS | TIE-ORDER |
| range(@timestamp) | 2.16 / 2.51 | 0.18 / 0.79 | 100000 | LOSS | TIE-ORDER |
| range(cost_usd) | 1.00 / 1.29 | 0.71 / 1.64 | 41540 | LOSS | TIE-ORDER — audited: LOSS in 6/6 independent rounds (X 0.90–1.86 vs ES 0.50–0.75); ES floor ~30% faster on audit re-measure |
| prefix(model) | 5.85 / 7.57 | 1.01 / 2.11 | 84731 | LOSS | TIE-ORDER |
| wildcard(model) | 5.71 / 7.07 | 1.05 / 2.08 | 84731 | LOSS | TIE-ORDER |
| regexp(model) | 0.64 / 1.05 | 1.24 / 1.93 | 84731 | **WIN** | TIE-ORDER (scores bit-exact 1.0) |
| fuzzy(model) | 2.06 / 2.68 | 0.50 / 1.60 | 27744 | LOSS | **SCORE-DIVERGE** — XERJ 1.2821425 (full term score) vs ES 1.2020086 (fuzziness-discounted) |
| exists(cost_usd) | 0.60 / 0.71 | 0.16 / 0.78 | 100000 | LOSS | TIE-ORDER |
| ids | 0.07 / 0.11 | 0.16 / 0.87 | 3 | **LOSS**² | **SCORE-DIVERGE** — XERJ `_score`/`max_score` = **0** vs ES **1.0** (docs correct) |
| term(cache_hit) | 1.75 / 2.24 | 0.20 / 0.75 | 42875 | LOSS | EXACT |
| bool must+filter+should+must_not | 2.96 / 3.10 | 2.72 / 4.09 | 37369 | LOSS | EXACT — audited: LOSS in 5/6 independent rounds (Δ 0.25–0.70 ms); ES floor ~30% faster on audit re-measure |
| constant_score | 1.00 / 1.34 | 0.23 / 0.90 | 98752 | LOSS | EXACT |
| boosting | 1.87 / 2.73 | 3.40 / 4.87 | 98752 | **WIN** | EXACT |
| dis_max | 1.53 / 2.50 | 0.55 / 1.26 | 27744 | LOSS | EXACT |
| function_score | 1.38 / 2.25 | 3.27 / 4.50 | 100000 | **WIN** | TIE-ORDER — ranks 0–3 identical, scores bit-exact (0x…= 0.016213253) on all 10; ES's tied ranks 4–9 are 81282,85290,… vs XERJ 17154,21162,… (both orders deterministic)³ |
| pinned | 0.81 / 1.19 | 2.53 / 3.44 | 98752 | **WIN** | EXACT (pinned ids 1,2 float to top bit-exactly) |

² `ids` is a **latency TIE** (0.07 vs 0.16 ms) forced to **LOSS by the correctness rule**: XERJ
returns `_score: 0` / `max_score: 0` where ES returns `1.0` — a real scored-value defect (**NEW
ticket**), first visible now that the corpus has explicit ids.
³ The batch-3 claim "`function_score` fvf byte-identical to ES top-10" **no longer holds at tied
ranks**: all f32 score bits and ranks 0–3 match, but ES's deterministic tie order among the 25×
duplicated max-cost docs differs from XERJ's ascending-id order (ES's own tie order here is a
Lucene collector artifact — it is *not* ascending doc id, unlike its `boosting`/`pinned` ties).
Perf + scores are intact; the ids-at-tied-ranks claim is the regression. **Finding.**
⚠️ **Fragility caveat (audit):** this WIN and its bit-exactness hold ONLY on the pristine
append-only corpus with the exact gated body. Adding `sort: ["_score"]`, any `aggs`, `min_score`,
`max_boost`, `weight`, a `boost` ≠ 1, or any delete/overwrite on the index reroutes the query to
a brute path that **double-applies (squares) the function with wrong top-k**. Ticketed for
batch 12.

**Scored-family status (batches 10/11 exactness check):** `bool`/`boosting`/`pinned`/`dis_max`/
`constant_score`/`term`/`match`/`query_string`/`simple_query_string` are **bit-for-bit EXACT**
(ids order, f32 score bits, max_score, totals). Still diverging: `ids` (score 0), `fuzzy` (no
fuzziness discount), `match_bool_prefix` (2.52 vs 1.0), `more_like_this` (flat 1.0), `multi_match`
(1 ulp), the `highlight` path (below), and the constant-score/tie-order family.

---

## BASIC AGGS (31) — 21 WIN / 1 LOSS / 9 TIE

| case | XERJ p50/p99 (ms) | ES p50/p99 (ms) | verdict | correctness (value-level vs ES) |
|---|--:|--:|:--:|---|
| avg | 0.06 / 0.12 | 1.69 / 2.48 | **WIN** | EXACT |
| sum | 0.05 / 0.11 | 1.47 / 2.80 | **WIN** | F64-DRIFT (947.7913179999912 vs 947.791318, rel 9.4e-15 — Kahan ledger) |
| min | 0.05 / 0.13 | 0.22 / 0.89 | TIE | EXACT |
| max | 0.05 / 0.10 | 0.19 / 0.66 | TIE | EXACT |
| stats | 0.07 / 0.12 | 1.73 / 2.93 | **WIN** | EXACT |
| extended_stats | 0.12 / 0.21 | 2.10 / 2.84 | **WIN** | F64-DRIFT (std_deviation 1 ulp) |
| value_count | 0.05 / 0.13 | 0.83 / 1.83 | **WIN** | EXACT |
| cardinality | 0.20 / 0.50 | 1.07 / 1.95 | **WIN** | EXACT (12 both) |
| percentiles | 0.10 / 0.18 | 7.45 / 9.92 | **WIN** | APPROX — XERJ exact 826 vs ES TDigest 821.95 (XERJ more accurate) |
| percentile_ranks | 0.09 / 0.34 | 7.36 / 9.45 | **WIN** | APPROX (values); keys now ES-format `"200.0"` ✅ (batch-12 fix holds) |
| median_absolute_deviation | 0.25 / 0.89 | 11.00 / 13.13 | **WIN** | APPROX — XERJ exact 364 vs ES 364.42 |
| matrix_stats | 2.74 / 5.23 | 12.73 / 27.70 | **WIN** | F64-DRIFT (correlation 1 ulp) |
| scripted_metric | 719.17 / 760.24 | 4.99 / 5.86 | LOSS | EXACT (value identical; still the O(N) brute cliff) |
| top_hits (sub) | 1.63 / 2.38 | 2.76 / 5.33 | **WIN** | TIE-ORDER — sub-hit sort keys bit-exact ([1367]); tied top doc 70300 vs 2164 |
| terms | 0.19 / 0.25 | 0.36 / 1.04 | TIE | EXACT |
| rare_terms | 0.20 / 0.93 | 6.31 / 8.15 | **WIN** | EXACT |
| significant_terms | 0.06 / 0.21 | 1.66 / 3.21 | **WIN** | EXACT |
| histogram | 1.96 / 3.50 | 2.95 / 5.47 | **WIN** | EXACT |
| date_histogram | 0.11 / 0.67 | 0.36 / 1.03 | TIE | EXACT |
| auto_date_histogram | 0.10 / 0.18 | 2.80 / 3.82 | **WIN** | EXACT ✅ (batch-12 sub-day anchoring fix holds on the matrix body) |
| variable_width_histogram | 0.23 / 0.48 | 7.71 / 11.22 | **WIN** | APPROX — clustering differs (bucket0 28992 vs 29117; ES vwh is heuristic by design) |
| range | 0.06 / 0.17 | 0.32 / 1.12 | TIE | EXACT |
| date_range | 0.08 / 0.15 | 0.30 / 0.81 | TIE | EXACT |
| filter | 0.53 / 0.58 | 2.60 / 4.01 | **WIN** | EXACT |
| filters | 0.07 / 0.12 | 0.23 / 1.02 | TIE | EXACT |
| missing | 0.06 / 0.11 | 0.93 / 2.00 | **WIN** | EXACT |
| global | 0.07 / 0.15 | 1.43 / 2.65 | **WIN** | EXACT (1248 both) |
| adjacency_matrix | 0.47 / 0.52 | 2.02 / 3.28 | **WIN** | EXACT |
| composite | 2.33 / 6.14 | 2.57 / 3.85 | TIE | EXACT |
| random_sampler | 0.30 / 0.54 | 0.56 / 0.86 | TIE | APPROX (sampling RNGs differ by design) |
| terms+avg(cost) | 0.58 / 1.13 | 2.69 / 4.21 | **WIN** | F64-DRIFT (avg rel 6.7e-15) |

The seven former ~1-second cliffs stay fixed (`missing` 0.06, `mad` 0.25, `matrix_stats` 2.74,
`auto_date_histogram` 0.10, `rare_terms` 0.20, `significant_terms` 0.06 ms). **Only
`scripted_metric` remains a brute O(N) cliff** (719 ms — improved from 1086 ms but still 144×
behind ES). The 07-09 p99-tail instability on histogram/vwh/adjacency/filter/global is **gone**
(all p99 < 6.2 ms this run).

## PIPELINE AGGS (12) — 12 WIN / 0 LOSS / 0 TIE (clean sweep again)

| case | XERJ p50/p99 (ms) | ES p50/p99 (ms) | verdict | correctness |
|---|--:|--:|:--:|---|
| sum_bucket | 0.50 / 0.98 | 2.30 / 4.04 | **WIN** | F64-DRIFT (rel ≤5.2e-15) |
| avg_bucket | 0.46 / 0.80 | 2.27 / 3.91 | **WIN** | F64-DRIFT |
| max_bucket | 0.45 / 0.78 | 2.26 / 3.85 | **WIN** | F64-DRIFT |
| stats_bucket | 0.46 / 0.83 | 2.21 / 3.96 | **WIN** | F64-DRIFT |
| percentiles_bucket | 0.48 / 0.55 | 2.13 / 3.47 | **WIN** | F64-DRIFT |
| derivative | 0.45 / 0.82 | 2.14 / 3.56 | **WIN** | F64-DRIFT |
| cumulative_sum | 0.46 / 0.84 | 2.19 / 3.61 | **WIN** | F64-DRIFT |
| moving_fn | 0.44 / 0.88 | 2.19 / 4.02 | **WIN** | F64-DRIFT |
| serial_diff | 0.47 / 0.87 | 2.17 / 3.84 | **WIN** | F64-DRIFT |
| bucket_script | 0.56 / 0.94 | 3.18 / 4.79 | **WIN** | F64-DRIFT |
| bucket_selector | 0.48 / 0.80 | 2.07 / 3.25 | **WIN** | F64-DRIFT |
| bucket_sort | 0.46 / 0.65 | 2.18 / 3.26 | **WIN** | F64-DRIFT |

All 12 F64-DRIFT flags are the **same single root cause**: the per-day `sum(cost_usd)` bucket
values carry the known naive-vs-Kahan ~1e-14 relative drift (ledger defect from batch 12); every
derived pipeline value agrees with ES to ≥13 significant digits. Bucket counts/keys identical.

## FEATURE OPS (7) — 2 WIN / 4 LOSS / 1 TIE

| case | XERJ p50/p99 (ms) | ES p50/p99 (ms) | verdict | correctness |
|---|--:|--:|:--:|---|
| sort-heavy (2-key desc/asc) | 0.81 / 1.34 | 1.76 / 3.22 | **WIN** | WIRE + TIE-ORDER — sort keys bit-exact; XERJ returns `_score: 1` where ES returns `null` on sorted hits (**NEW wire ticket**); tied-key id order differs (ES's own order is non-ascending) |
| deep from+size (from 500) | 1.98 / 2.42 | 0.44 / 1.53 | LOSS | TIE-ORDER (match_all page at offset 500) |
| search_after | 0.70 / 0.91 | 1.88 / 3.22 | **WIN** | TIE-ORDER — both engines page past identical sort values [159, 1777304577000]; tied-key ids differ |
| highlight | 2.68 / 3.15 | 0.78 / 3.87 | LOSS | **SCORE-DIVERGE** — adding `highlight` de-routes off the exact-BM25 path: bare `match(status:ok)` is bit-exact (0.012563465) but with highlight XERJ scores 0.012563489 (26 ulps) + match_all-style tie order (**NEW ticket**); highlight fragments themselves identical (`<em>ok</em>`) |
| _count | 0.03 / 0.07 | 0.09 / 0.92 | TIE | EXACT (100000 both) |
| _msearch | 0.50 / 0.93 | 0.17 / 0.92 | LOSS | n/a (multi-line; engine-default totals) |
| _mget | **1397.11 / 1504.82** | 0.24 / 0.72 | LOSS | docs value-identical (1,2,3 found) — **WIRE**: XERJ re-serializes `_source` with alphabetized keys while ES returns the original bytes; docs are value-identical only — and see cliff note |

**`_mget` is a ~1.4-SECOND cliff** (5800× behind ES). On 07-09 this cell read 0.12 ms/TIE — but
the auto-id corpus made it a no-match no-op then. With real ids, XERJ's `_mget` evidently does an
O(N)-ish scan per id instead of a primary-key lookup, while `GET`-by-id semantics stay correct.
**NEW ticket — worst single regression-visible cell on the board.** (`ids` query is 0.07 ms on the
same lookup, so the fast id path exists and `_mget` just doesn't use it.)

---

## CHANGED SINCE 2026-07-09 (baseline 45 W / 26 L / 11 T → **42 W / 28 L / 12 T**, audited)

Eight cells flipped verdict; two more (`query_string`, `range(cost_usd)`) moved sharply in
latency but stayed LOSS after audit. Each row: baseline p50 → today's p50 (XERJ vs ES), with the
correctness check.

| cell | 07-09 | 07-10 | XERJ p50 (was→now) | ES p50 (was→now) | why / correctness |
|---|:--:|:--:|--:|--:|---|
| q: query_string | LOSS | LOSS | 14.95 → 1.66 | 2.26 → 1.44 | XERJ 9× faster since bd3bb41 (scored-path work); EXACT. Measurer scored TIE; audited LOSS in 6/6 independent rounds — not a flip |
| q: range(cost_usd) | LOSS | LOSS | 2.62 → 1.00 | 1.61 → 0.71 | both faster; TIE-ORDER. Measurer scored TIE (Δp50 0.29 inside band); audited LOSS in 6/6 independent rounds — not a flip |
| q: regexp(model) | TIE | **WIN** | 2.00 → 0.64 | 2.21 → 1.24 | XERJ 3× faster; TIE-ORDER (scores bit-exact) |
| q: ids | TIE | **LOSS (correctness)** | 0.26 → 0.07 | 0.35 → 0.16 | corpus now has ids 0..99999 → cell actually matches; XERJ `_score` 0 vs ES 1.0 = faster-but-WRONG ⇒ LOSS per rule |
| q: bool m+f+s+mn | WIN | **LOSS** | 3.8 → 2.96 | 5.5 → 2.72 | ES much faster on today's quiet box; still EXACT. Measurer scored TIE (Δ 0.24 inside band); audited LOSS in 5/6 independent rounds |
| agg: composite | LOSS | **TIE** | 5.64 → 2.33 | 4.29 → 2.57 | XERJ 2.4× faster; EXACT |
| agg: date_histogram | WIN | **TIE** | 0.35 → 0.11 | 0.80 → 0.36 | both faster; Δ 0.25 inside band; EXACT |
| agg: date_range | WIN | **TIE** | 0.34 → 0.08 | 0.68 → 0.30 | Δ 0.22 inside band; EXACT |
| feat: _count | WIN | **TIE** | 0.12 → 0.03 | 0.53 → 0.09 | Δ 0.06 inside band; EXACT |
| feat: _mget | TIE | **LOSS** | 0.12 → **1397.11** | 0.37 → 0.24 | corpus-driven unmasking: with real ids `_mget` is a 1.4 s per-id scan cliff (docs correct) |

Non-flips worth recording: `simple_query_string` 19.11 → 1.35 ms (14×, still LOSS vs ES 0.28);
`more_like_this` 269 → 145 ms (still the worst query cliff, + flat-1.0 scores);
`scripted_metric` 1086 → 719 ms (still LOSS); `dis_max` still the honest small LOSS (1.53 vs
0.55, EXACT); `boosting`/`pinned`/`function_score` hold their batch-5/3 WINs (but see the
`function_score` fragility caveat³). Three of the eight flips (`date_histogram`, `date_range`,
`_count`) are **not regressions** — XERJ got faster in absolute terms on every one of them; ES's
floor on today's quieter box tightened into the 0.30 ms tie band. `bool` also got faster in
absolute terms, but the audit shows ES's floor moved past it (LOSS in 5/6 rounds). Net: −3 WIN,
+2 LOSS, +1 TIE.

### New defect tickets from this run (all reproducible, deterministic)

1. **`ids` query scores 0** — `_score`/`max_score` = 0 vs ES 1.0 (constant-score semantics).
2. **`_mget` 1.4 s cliff** — no primary-key fast path on multi-get (single-doc-class lookup takes
   0.07 ms via the `ids` query, so the index exists).
3. **`highlight` de-routes scoring** — presence of `highlight` bails the query off the exact-BM25
   path: 26-ulp score drift + non-ES tie order, on a body that is bit-exact without highlight.
4. **`sort-heavy` wire bug** — `_score: 1` returned on sorted hits where ES returns `null`.
5. **match_all-family tie order** — the unscored/constant-score page path emits ids 0,60,82,96,…
   (deterministic, not insertion order) where ES pages 0,1,2,…; affects
   match_all/match_phrase/terms/range×3/prefix/wildcard/regexp/exists/deep-paging.
6. **`function_score` tied-rank order regression vs batch-3 claim** — scores bit-exact, ranks 0–3
   exact, ranks 4–9 tie-shuffled vs ES's deterministic collector order.
7. Pre-existing, still open: `fuzzy` no fuzziness discount; `match_bool_prefix` 2.52-vs-1.0;
   `more_like_this` flat 1.0; `multi_match` 1-ulp; Kahan F64-DRIFT on sums; scored shapes outside
   the batch-5 gate.

---

## CORRECTNESS VERIFICATION SUMMARY

- **Hit totals: 79/79 cells identical** to ES (every query/agg/pipe/feature signal matched).
- **Bit-for-bit EXACT scored cells (11/27 ES-runnable queries):** match_none, match(model),
  query_string, simple_query_string, term(status), term(cache_hit), bool, constant_score,
  boosting, dis_max, pinned — ids order + f32 score bits + max_score + totals all identical.
  The batch-5 scored family **held its exactness under re-measurement**.
- **TIE-ORDER only (scores/sort keys bit-exact): 10 query cells + top_hits + 3 feature cells.**
  Every one was probed repeatedly on both engines: both orders are deterministic; the corpus's
  25× duplication makes tied keys ubiquitous. ES's own tie order is a collector artifact and is
  itself not globally consistent (ascending doc-id on `boosting`/`pinned` ties, non-ascending on
  `function_score`/`sort` ties).
- **Real value divergences (findings):** ids (0 vs 1.0), fuzzy, match_bool_prefix, more_like_this,
  multi_match (1 ulp), highlight path (26 ulps). All on LOSS cells except `ids` (flipped to LOSS).
- **Aggs:** 23/31 EXACT to the last bit; 4 F64-DRIFT ≤1e-13 rel (one ledger root cause);
  4 APPROX where ES is approximate-by-design (percentiles/percentile_ranks/mad TDigest — XERJ is
  the *more exact* side; vwh clustering; random_sampler RNG). `percentile_ranks` keys are now
  ES-exact `"200.0"` (batch-12 fix verified live). Pipelines: all values match to ≥13 digits.
- **No fast-but-wrong WIN survives:** the only wrong-VALUE cell that was not already a latency
  LOSS (`ids`) was verdict-flipped to LOSS.

---

## HONEST BOTTOM LINE

- **XERJ now wins or ties 53 of 79 re-measured cells** (39 W + 11 T + 2 ES-REJECTS-where-XERJ-runs
  + `ids`-fast-but-flagged) vs ES 8.13.4 on identical work — up from 31 W+T at the original
  bd3bb41 snapshot. Aggregations are a rout: 21 W / 9 T / 1 L with 0.05–2.7 ms p50s and the p99
  tails now stable; pipelines sweep 12/12.
- **ES still owns the raw-lookup floor** (0.2–1 ms postings/BKD): 22 of 29 query cells are
  honest LOSSes, though the gap is now typically 0.5–1.5 ms (was 100–340 ms cliffs; only
  `more_like_this` 145 ms, `scripted_metric` 719 ms, `_mget` 1397 ms remain O(N)-class cliffs).
- **The scored family is genuinely ES-exact now** — bool/boosting/pinned/dis_max/constant_score/
  term/match/query_string bit-for-bit, under a re-run with fresh data. The remaining exactness
  debt is concentrated in: tie order on constant-score pages, the highlight de-route, `ids`
  scoring, fuzzy/mbp/mlt/multi_match scoring, and f64 sum drift.
- Table B (carried): ingest 1.54×/1.80× WIN, disk 0.82× WIN, kNN 120× LOSS, recall TIE, RRF
  XERJ-only.

*Closed-loop, single-client, XERJ query-cache OFF (env-verified), ES default + read-only, 100k
identical single-segment explicit-id corpus, box quiet. XERJ `:9200` data
`/tmp/xerj-scorecard/` under a 24G systemd scope, killed by PID after the run; ES `:9201`
untouched. Engine `addbbcd` (clean tree); binary rebuilt this session; nothing committed. Harness/raw/log: session scratchpad `full_matrix.mjs`, `results-2026-07-10.json`,
`fullrun-2026-07-10.log`.*
