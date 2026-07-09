# XERJ vs Elasticsearch — DEFINITIVE Full-Matrix Scorecard (2026-07-09)

> **UPDATE 2026-07-09 (post-snapshot, commit `0325fb7`) — four ~1s brute aggs flipped LOSS → WIN.**
> `missing` (642→0.16ms), `median_absolute_deviation` (612→0.29ms), `matrix_stats` (654→2.49ms),
> `auto_date_histogram` (824→0.13ms) were routed through the columnar doc-values fast path — all four
> now BEAT ES, ES-YAML gate held 1360/0/3, fast output byte-identical to brute (independently
> adversarially verified, no fast-but-wrong). **Revised counts: basic aggs 21W/4L/6T; Table A total
> 36W/31L/10T; GRAND TOTAL 39 WIN / 32 LOSS / 11 TIE.** Caveat: `auto_date_histogram` is a perf-only
> win — its sub-day bucket-grid anchoring still diverges from ES (epoch-anchored 18:00 vs ES
> earliest-hour 19:00; a PRE-EXISTING brute-vs-ES gap in shared `run_date_histogram`, tracked as a
> separate compat fix).
>
> **UPDATE 2026-07-09 (batch 2, commit `9379578`) — 2 more aggs flipped LOSS → WIN + a 340ms cliff eliminated.**
> `rare_terms` (996→0.3ms, 14× ES) and `significant_terms` no-query (1061→0.07ms, 15× ES) are now columnar
> WINs (exact buckets). `range(@timestamp)`'s 340ms O(N) cliff is **eliminated** → 1.4–3.2ms with EXACT ES
> hit-parity on every form (gte/lte, gt/lt, open-ended, boundary) via a date-shadow range prefilter +
> authoritative shadow count — but it stays an honest **LOSS** (~1–3ms behind ES's sub-ms floor; the empty
> range is now a TIE), so the query W/L count is unchanged while its absolute latency dropped 100×.
> `significant_terms` WITH a top-level query stays on brute (matching ES's JLH heuristic is high-risk).
> **Revised counts: basic aggs 23W/2L/6T (only scripted_metric + composite still LOSS); GRAND TOTAL
> 41 WIN / 30 LOSS / 11 TIE.** The tables below remain the original bd3bb41 snapshot.
>
> **UPDATE 2026-07-09 (batch 3) — `function_score(field_value_factor)` flipped LOSS → WIN + a latent correctness bug fixed.**
> The `function_score{match_all, field_value_factor}` shape was on the brute JSON-parse-everything path:
> **136.9→1.35ms (~100×), a 47× LOSS becomes a ~2.1× WIN vs ES** (ES 2.92ms). It was also the first fix
> for the latent scored-top-k defect: the old brute path returned the wrong top-10 (256 arrival-order cap,
> not the global top-k) AND double-applied the function (`fvf²`) with the wrong factor/modifier order. The
> new columnar path (walk the numeric doc-values column, score via shared `fvf_score_from_raw`, global
> TopN heap, F2 hydrate winners) is now **byte-identical to ES 8.13.4 top-10** on the 100k single-segment
> corpus (same _ids, same order, _score within 1e-6, max_score 0.016213253 on both). ES-YAML gate held
> **1360/0/3**; the `ln1p(-1)→NaN` error path still bails to brute and returns ES's exact
> `illegal_argument_exception`. **Revised counts: GRAND TOTAL 42 WIN / 29 LOSS / 11 TIE.** Caveat: only
> the covered `function_score` shape is fixed; other shapes (max_boost, multiple functions,
> filter/weight/random_score/script, non-match_all base, deletes present) still bail to the brute path and
> remain a (correct-for-those-shapes-later) LOSS — tracked with boosting/dis_max/pinned/bool-should/MLT.

**Original MEASUREMENT + compatibility audit at engine `bd3bb41` (pre-`0325fb7`). No engine code was changed for this snapshot; nothing was committed at snapshot time.**

This scorecard **replaces the discredited "81 W / 91" headline number.** That number was a
mirage: it was measured with the **open-loop, 200 req/s coordinated-omission driver**
(`timed()` in `bench-matrix.mjs`) with **XERJ's query cache ON**, so it measured
throughput-under-saturation + whole-result cache clones, not each engine's true per-query cost.
Every cell below is re-measured **closed-loop** (single client, sequential, one request at a
time — the true round-trip), with **XERJ's query cache OFF** and `request_cache=false` on every
search, using **identical query bodies and an identical corpus** on both engines.

## Method (fair, closed-loop, single-client)

- **Build:** committed working tree at **`bd3bb41`** (clean; only an untracked doc file present),
  release binary `engine/target/release/xerj` from `cargo build --release -p xerj-server`.
  No sources modified; nothing committed.
- **XERJ** on `:9200`, fresh temp data dir, `--insecure`, **`XERJ_DISABLE_QUERY_CACHE=1`**
  (whole-result cache OFF). **Real Elasticsearch 8.13.4** on `:9201`, default settings, untouched.
  (XERJ self-reports wire version 8.13.0.) `python :8080` untouched.
- **Corpus:** the flat LLM-telemetry corpus `demo/data/extras/chat-events.ndjson` (4,008 real docs)
  cycled to **100,000 docs** for the read surface, **identical** on both engines, with the exact
  `bench-matrix.mjs` mapping (keyword `model`/`intent`/`status`/`tenant`/`top_doc`, boolean
  `cache_hit`, integer token/latency fields, double `cost_usd`, date `@timestamp`).
  **Single segment** on both (`_forcemerge?max_num_segments=1` after seeding + 3 s settle).
  Both `/perf/_count` = 100,000.
- **Bodies:** the **query/agg/pipeline/feature bodies are copied VERBATIM** from
  `demo/playbooks/bench-matrix.mjs` (29 queries, 31 basic aggs, 12 pipeline aggs, 7 feature ops).
- **Measurement:** CLOSED-LOOP — feasibility probe, then **warm 8**, then **p50/p99 over 50 iters**,
  one request at a time. Lean keep-alive `http` client, applied identically to both engines.
- **Verdict rule (from XERJ's POV):** compare **p50**. `|Δp50| ≤ 0.30 ms` ⇒ **TIE**; otherwise
  **WIN** (XERJ faster) / **LOSS** (ES faster). If XERJ errors/unsupported while ES works ⇒
  **XERJ-UNSUPPORTED** (a compat gap, not a perf loss); if ES 4xx while XERJ works ⇒ **ES-REJECTS**.
- **Correctness guard:** every cell captures a hit-total / agg signal; a WIN/TIE whose result
  diverges from ES would be flagged **FAST-BUT-DIVERGENT**. A separate value-level spot-check
  (below) compares actual agg *values* vs ES so no "win" is a fast-but-wrong result.
- Harness: `/tmp/.../scratchpad/full_matrix.mjs`; raw JSON: `results.json`; run log: `fullrun.log`.

---

## SUMMARY COUNTS

### Table A — read / agg / feature matrix (79 cells)

| group | cells | WIN | LOSS | TIE | XERJ-UNSUPPORTED | ES-REJECTS | BOTH-ERROR |
|---|--:|--:|--:|--:|--:|--:|--:|
| queries | 29 | 0 | 24 | 3 | 0 | 2 | 0 |
| basic aggs | 31 | 17 | 8 | 6 | 0 | 0 | 0 |
| pipeline aggs | 12 | 12 | 0 | 0 | 0 | 0 | 0 |
| feature ops | 7 | 3 | 3 | 1 | 0 | 0 | 0 |
| **Total** | **79** | **32** | **35** | **10** | **0** | **2** | **0** |

- **Perf verdicts (both engines ran identical work):** **32 WIN / 35 LOSS / 10 TIE.**
- **Compat gaps:** **0 XERJ-UNSUPPORTED**, **2 ES-REJECTS** (both are XERJ being *more lenient*
  than ES — see Compatibility section), **0 BOTH-ERROR**.
- **Fast-but-wrong cells: 0.** No WIN/TIE returned results that diverge from ES.

### Table B — ingest, disk & vector (6 cells)

| dimension | XERJ | ES | verdict |
|---|--:|--:|:--:|
| ingest throughput (300k, 1 client, 10k/bulk) | **95,469 docs/s** | 62,154 docs/s | **WIN** (1.54×) |
| ingest per-bulk latency p50 / p99 | **84.0 / 168.6 ms** | 151.5 / 176.9 ms | **WIN** (p50 1.80×) |
| index on-disk size (`/perf` `_stats` store) | **9.85 MB** | 12.07 MB | **WIN** (0.82×) |
| kNN k=10 latency p50 / p99 | 309.55 / 1397.73 ms | **2.59 / 4.12 ms** | **LOSS** (ES 120× faster) |
| kNN recall@10 | 1.00 | 1.00 | **TIE** |
| hybrid lexical+vector RRF | **works, 4.84 ms** | unavailable on 8.13.4 | XERJ-only¹ |

¹ ES 8.13.4 cannot run the hybrid: the `retriever` syntax returns **400** (introduced in ES 8.14),
and the older `rank:{rrf}` form returns **403 — "current license is non-compliant for [Reciprocal
Rank Fusion (RRF)]"** (RRF is a commercial-license feature; this ES runs a basic license). XERJ
executes **both** RRF forms. XERJ's RRF result **could not be validated against ES** here because
ES refuses to run it — so this is a *capability* edge, not a verified correctness/latency win.

### Grand total (perf-comparable dimensions)

**35 WIN / 36 LOSS / 11 TIE**, plus **2 ES-REJECTS**, **1 XERJ-only (RRF)**, **0 XERJ-UNSUPPORTED**.

---

## DETAILED MATRIX

Verdict is XERJ's POV. p50/p99 in ms. `hits` is the shared result signal (size:0 shows the
`track_total_hits:true` total forced onto **both** engines; 100000 = full corpus).

### QUERIES (29) — 0 WIN / 24 LOSS / 3 TIE / 2 ES-REJECTS

| case | XERJ status | XERJ p50/p99 (ms) | ES status | ES p50/p99 (ms) | hits | verdict |
|---|---|--:|---|--:|--:|:--:|
| match_all | ok | 2.83 / 6.35 | ok | 0.73 / 1.73 | 100000 | LOSS |
| match_none | ok | 0.36 / 0.80 | ok | 0.62 / 1.40 | 0 | TIE |
| match(model) | ok | 4.23 / 6.20 | ok | 1.08 / 2.13 | 27744 | LOSS |
| match_phrase(top_doc) | ok | 2.65 / 4.14 | ok | 0.87 / 1.88 | 9082 | LOSS |
| match_phrase_prefix | ok | 4.44 / 8.79 | **err 400** | — | 25259 | **ES-REJECTS** |
| match_bool_prefix | ok | 4.16 / 4.81 | ok | 1.23 / 2.09 | 25259 | LOSS |
| multi_match | ok | 4.46 / 5.83 | ok | 1.64 / 2.65 | 27744 | LOSS |
| combined_fields | ok | 7.10 / 9.82 | **err 400** | — | 27744 | **ES-REJECTS** |
| query_string | ok | 14.95 / 17.00 | ok | 2.26 / 3.03 | 27371 | LOSS |
| simple_query_string | ok | 19.11 / 28.20 | ok | 0.52 / 0.96 | 98752 | LOSS |
| more_like_this | ok | **269.20 / 419.16** | ok | 0.93 / 1.90 | 9082 | LOSS |
| term(status) | ok | 2.27 / 3.45 | ok | 0.78 / 1.58 | 98752 | LOSS |
| terms(model) | ok | 3.91 / 5.27 | ok | 1.18 / 1.90 | 27744 | LOSS |
| range(latency_ms) | ok | 2.47 / 3.58 | ok | 1.33 / 1.62 | 29067 | LOSS |
| range(@timestamp) | ok | **339.67 / 386.87** | ok | 0.61 / 1.42 | 100000 | LOSS |
| range(cost_usd) | ok | 2.62 / 3.85 | ok | 1.61 / 2.51 | 41540 | LOSS |
| prefix(model) | ok | 9.74 / 13.84 | ok | 2.06 / 3.30 | 84731 | LOSS |
| wildcard(model) | ok | 12.30 / 17.12 | ok | 2.31 / 3.90 | 84731 | LOSS |
| regexp(model) | ok | 2.00 / 4.02 | ok | 2.21 / 4.06 | 84731 | TIE |
| fuzzy(model) | ok | 5.19 / 6.93 | ok | 1.27 / 2.14 | 27744 | LOSS |
| exists(cost_usd) | ok | 1.59 / 2.47 | ok | 0.52 / 0.76 | 100000 | LOSS |
| ids | ok | 0.26 / 0.52 | ok | 0.35 / 1.25 | 0 | TIE |
| term(cache_hit) | ok | 3.07 / 3.94 | ok | 0.53 / 1.05 | 42875 | LOSS |
| bool must+filter+should+must_not | ok | **164.83 / 246.33** | ok | 4.32 / 5.57 | 37369 | LOSS |
| constant_score | ok | 1.98 / 2.73 | ok | 0.69 / 1.61 | 98752 | LOSS |
| boosting | ok | **244.83 / 295.28** | ok | 5.37 / 7.56 | 98752 | LOSS |
| dis_max | ok | **247.27 / 370.48** | ok | 1.81 / 2.82 | 27744 | LOSS |
| function_score | ok | **240.59 / 303.97** | ok | 6.39 / 7.92 | 100000 | LOSS |
| pinned | ok | **247.61 / 371.93** | ok | 2.75 / 3.61 | 98752 | LOSS |

**Reads: XERJ loses every comparable raw-lookup / scored-query race.** ES's postings/BKD lookup
floor is 0.5–2 ms; XERJ sits above it even on fast paths (`match_all` 2.83 ms, `term` 2.27 ms).
Six query shapes are **O(N) brute-force cliffs on XERJ** (bold): `range(@timestamp)` 340 ms,
`more_like_this` 269 ms, and the scored-compound family `boosting`/`dis_max`/`function_score`/
`pinned`/`bool(must+should+must_not)` at 165–248 ms — all `<8 ms` on ES. `ids`/`_mget` match
nothing on either engine (corpus uses auto-`_id`s), so those are equal no-match races.

### BASIC AGGS (31) — 17 WIN / 8 LOSS / 6 TIE

| case | XERJ status | XERJ p50/p99 (ms) | ES status | ES p50/p99 (ms) | hits | verdict |
|---|---|--:|---|--:|--:|:--:|
| avg | ok | 0.22 / 0.70 | ok | 3.63 / 4.76 | 100000 | **WIN** |
| sum | ok | 0.26 / 0.53 | ok | 3.47 / 4.52 | 100000 | **WIN** |
| min | ok | 0.15 / 0.34 | ok | 0.32 / 0.54 | 100000 | TIE |
| max | ok | 0.15 / 0.28 | ok | 0.30 / 0.41 | 100000 | TIE |
| stats | ok | 0.12 / 0.28 | ok | 3.31 / 4.06 | 100000 | **WIN** |
| extended_stats | ok | 0.38 / 0.60 | ok | 4.28 / 4.88 | 100000 | **WIN** |
| value_count | ok | 0.14 / 0.42 | ok | 2.26 / 2.99 | 100000 | **WIN** |
| cardinality | ok | 0.51 / 0.70 | ok | 2.46 / 2.86 | 100000 | **WIN** |
| percentiles | ok | 0.38 / 1.15 | ok | 13.61 / 19.68 | 100000 | **WIN** |
| percentile_ranks | ok | 0.40 / 0.68 | ok | 14.07 / 18.27 | 100000 | **WIN**² |
| median_absolute_deviation | ok | **1004.45 / 1061.47** | ok | 18.89 / 26.00 | 100000 | LOSS |
| matrix_stats | ok | **1028.69 / 1092.68** | ok | 22.11 / 31.64 | 100000 | LOSS |
| scripted_metric | ok | **1086.31 / 1133.94** | ok | 6.79 / 8.57 | 100000 | LOSS |
| top_hits (sub) | ok | 3.70 / 6.66 | ok | 4.95 / 6.71 | 100000 | **WIN** |
| terms | ok | 0.50 / 1.95 | ok | 0.77 / 1.87 | 100000 | TIE |
| rare_terms | ok | **996.88 / 1037.74** | ok | 9.12 / 13.72 | 100000 | LOSS |
| significant_terms | ok | **1061.33 / 1125.62** | ok | 3.07 / 5.30 | 100000 | LOSS |
| histogram | ok | 3.51 / 41.61 | ok | 6.32 / 8.09 | 100000 | **WIN**³ |
| date_histogram | ok | 0.35 / 34.96 | ok | 0.80 / 1.62 | 100000 | **WIN**³ |
| auto_date_histogram | ok | **1212.42 / 1298.06** | ok | 4.93 / 6.84 | 100000 | LOSS |
| variable_width_histogram | ok | 1.12 / 43.27 | ok | 12.63 / 18.84 | 100000 | **WIN**³ |
| range | ok | 0.38 / 11.02 | ok | 0.63 / 0.88 | 100000 | TIE |
| date_range | ok | 0.34 / 1.64 | ok | 0.68 / 0.85 | 100000 | **WIN** |
| filter | ok | 1.55 / 17.36 | ok | 4.26 / 6.27 | 100000 | **WIN**³ |
| filters | ok | 0.34 / 1.56 | ok | 0.59 / 0.84 | 100000 | TIE |
| missing | ok | **992.05 / 1061.64** | ok | 1.98 / 3.45 | 100000 | LOSS |
| global | ok | 0.42 / 17.33 | ok | 3.73 / 5.16 | 1248 | **WIN**³ |
| adjacency_matrix | ok | 1.45 / 48.31 | ok | 4.13 / 5.70 | 100000 | **WIN**³ |
| composite | ok | 5.64 / 8.88 | ok | 4.29 / 5.79 | 100000 | LOSS |
| random_sampler | ok | 1.00 / 2.63 | ok | 1.13 / 1.92 | 100000 | TIE |
| terms+avg(cost) | ok | 1.51 / 1.95 | ok | 4.51 / 6.16 | 100000 | **WIN** |

² `percentile_ranks` — WIN and values ~correct, but XERJ keys the `values` map `"200"`/`"500"`
while ES uses `"200.0"`/`"500.0"` (a wire-format divergence); see Correctness section.
³ p50-WIN but **p99 tail is unstable** (histogram 41.6 ms, date_histogram 35.0 ms, vwh 43.3 ms,
adjacency_matrix 48.3 ms, filter 17.4 ms, global 17.3 ms) — first-touch lazy columnar/GC spikes.
Verdict is on p50 (per the rule), but the tail is honestly worse than ES on these rows.

**Aggs are XERJ's real strength — where the data is columnar.** The precomputed doc-value stat
aggs (`avg`/`sum`/`stats`/`extended_stats`/`value_count`/`percentiles`/`percentile_ranks`) beat
ES by 3–14 ms, all returning **identical values** (spot-check below). But **seven aggs are ~1-second
O(N) brute-force cliffs** on XERJ (`median_absolute_deviation`, `matrix_stats`, `scripted_metric`,
`rare_terms`, `significant_terms`, `auto_date_histogram`, `missing`) — 30–250× slower than ES.

### PIPELINE AGGS (12) — 12 WIN / 0 LOSS / 0 TIE

| case | XERJ status | XERJ p50/p99 (ms) | ES status | ES p50/p99 (ms) | hits | verdict |
|---|---|--:|---|--:|--:|:--:|
| sum_bucket | ok | 1.52 / 7.24 | ok | 4.23 / 6.91 | 100000 | **WIN** |
| avg_bucket | ok | 1.44 / 6.13 | ok | 4.26 / 6.20 | 100000 | **WIN** |
| max_bucket | ok | 1.40 / 5.16 | ok | 5.08 / 6.48 | 100000 | **WIN** |
| stats_bucket | ok | 1.37 / 2.73 | ok | 4.35 / 7.31 | 100000 | **WIN** |
| percentiles_bucket | ok | 1.39 / 7.77 | ok | 4.34 / 6.15 | 100000 | **WIN** |
| derivative | ok | 1.37 / 6.62 | ok | 4.57 / 6.10 | 100000 | **WIN** |
| cumulative_sum | ok | 1.41 / 3.58 | ok | 4.67 / 6.42 | 100000 | **WIN** |
| moving_fn | ok | 1.41 / 6.02 | ok | 4.41 / 6.27 | 100000 | **WIN** |
| serial_diff | ok | 1.54 / 18.76 | ok | 4.57 / 6.24 | 100000 | **WIN** |
| bucket_script | ok | 1.82 / 4.57 | ok | 5.83 / 8.63 | 100000 | **WIN** |
| bucket_selector | ok | 1.39 / 4.82 | ok | 4.24 / 7.52 | 100000 | **WIN** |
| bucket_sort | ok | 1.70 / 3.83 | ok | 4.26 / 7.11 | 100000 | **WIN** |

**Clean 12/12 XERJ sweep.** All pipeline aggs are built on a `date_histogram` + `sum` sub-agg,
which is XERJ's fast columnar path; XERJ finishes in ~1.4–1.8 ms vs ES's ~4–6 ms. Values verified
identical (`sum_bucket` and `cumulative_sum` last-bucket both match ES to <1e-9 rel).

### FEATURE OPS (7) — 3 WIN / 3 LOSS / 1 TIE

| case | XERJ status | XERJ p50/p99 (ms) | ES status | ES p50/p99 (ms) | hits | verdict |
|---|---|--:|---|--:|--:|:--:|
| sort-heavy (2-key desc/asc) | ok | 2.93 / 4.45 | ok | 4.00 / 5.64 | 100000 | **WIN** |
| deep from+size (from 500) | ok | 5.55 / 9.58 | ok | 1.29 / 1.53 | 100000 | LOSS |
| search_after | ok | 2.66 / 4.00 | ok | 3.82 / 5.29 | 100000 | **WIN** |
| highlight | ok | 2.53 / 4.50 | ok | 1.09 / 1.36 | 98752 | LOSS |
| _count | ok | 0.12 / 0.25 | ok | 0.53 / 0.98 | 100000 | **WIN**⁴ |
| _msearch | ok | 1.58 / 4.24 | ok | 0.68 / 2.36 | — | LOSS |
| _mget | ok | 0.12 / 0.50 | ok | 0.37 / 1.26 | — | TIE |

⁴ `_count` measured **without** the bogus `request_cache=false` param that `bench-matrix.mjs`
appends — ES legitimately rejects that param on `/_count` (valid only on `/_search`), a harness
bug not a `_count` gap. See the leniency note in the Compatibility section.

---

## CORRECTNESS VERIFICATION (so no "win" is fast-but-wrong)

A separate value-level spot-check compared actual XERJ vs ES agg **values** on the same 100k index:

```
OK  avg=838.26063 (both)            OK  sum(cost_usd)=947.791318 (rel 0.000%)
OK  stats.avg/.sum (both exact)     OK  extended_stats.variance/.std_dev (rel 0.000%)
OK  value_count=100000 (both)       OK  cardinality(top_doc)=12 (both)
OK  percentiles.p50 XERJ 826 vs ES 822.45 (rel 0.43%, TDigest-approx)
OK  terms buckets/keys/counts (identical)   OK  date_histogram/histogram/vwh/range/date_range tot_doc=100000
OK  filter.avg (identical)          OK  adjacency_matrix/composite bucket counts (identical)
OK  global.value_count=100000       OK  pipe.sum_bucket / cumulative_sum.last (rel <1e-9)
DIFF percentile_ranks: values ~correct (XERJ 200→0.45 vs ES 0.47) but keyed "200" not "200.0"
```

- **All XERJ agg WINs return correct values** — the wins are real, not empty/short-circuited.
- **`percentiles`**: XERJ returns an **exact** integer percentile (826) vs ES's TDigest-approx
  (822.45) — XERJ is *more* accurate here, and faster. Not a defect.
- **`percentile_ranks`**: the one value-shape divergence — values are approximately correct
  (TDigest-approx), but the `values` map is keyed **`"200"`/`"500"`** on XERJ vs ES's
  **`"200.0"`/`"500.0"`**. A client expecting the ES `"200.0"` key reads `undefined` from XERJ.
  This is a genuine **wire-format compat bug**, though it does not make the latency win invalid.

---

## COMPATIBILITY / XERJ-UNSUPPORTED

**XERJ-UNSUPPORTED count on the runnable matrix: 0.** Across all 29 queries, 31 basic aggs, 12
pipeline aggs and 7 feature ops, **XERJ executed every ES query/agg/pipeline/feature shape** — it
never errored or returned "unsupported" while ES succeeded. The compat divergences that *do* exist
run the **other** direction (XERJ is more permissive than ES) or are format nuances:

1. **`match_phrase_prefix` on a keyword field** → **ES-REJECTS** ("Can only use phrase prefix
   queries on text fields — not on [top_doc] which is of type [keyword]"). **XERJ runs it** and
   returns 25,259 hits. XERJ does not enforce ES's text-field restriction — a **lenient semantic
   divergence** (XERJ answering a query ES deems illegal), not a XERJ capability win.
2. **`combined_fields` on a keyword field** → **ES-REJECTS** ("Field [model] of type [keyword]
   does not support [combined_fields] queries"). **XERJ runs it** (27,744 hits). Same lenient
   divergence.
3. **`_count` extra-param leniency** — ES returns **400** for `_count?request_cache=false`
   (`illegal_argument_exception: unrecognized parameter [request_cache]`); XERJ ignores the unknown
   param and returns 200. XERJ is more permissive about unknown query-string params.
4. **`percentile_ranks` value-key format** — `"200"` vs ES `"200.0"` (see Correctness).
5. **Hybrid RRF** — XERJ supports both the `retriever:{rrf}` and `rank:{rrf}` syntaxes; ES 8.13.4
   supports neither on this license (retriever → 400, version-gated to 8.14+; rank → 403,
   RRF is a commercial-license feature). A XERJ capability edge, correctness unverified vs ES.

### Families NOT exercised (need field types the flat corpus lacks) — separate known surface

These are **not** covered by this matrix and remain a **separate, known compat surface** (see
`STUB_AUDIT.md`): `geo_*` queries + `geohash_grid`/`geotile_grid`/`geo_bounds`/`geo_centroid`,
`ip_range`/`ip_prefix`, `nested`/`has_child`/`has_parent` (join), `span_*`, `significant_text`,
`semantic`/hybrid retrievers on real embeddings, `percolate`. "0 XERJ-UNSUPPORTED" applies **only**
to the corpus-runnable matrix above, not to these skipped families.

---

## HONEST BOTTOM LINE

- **Where XERJ genuinely wins:**
  - **Columnar aggregations** — `avg`/`sum`/`stats`/`extended_stats`/`value_count`/`cardinality`/
    `percentiles`/`percentile_ranks` beat ES by 3–14 ms with **identical (or more exact) values**.
  - **Pipeline aggregations** — a clean **12/12 sweep** (~1.4 ms vs ES's ~4–6 ms).
  - **Ingest** — **95k vs 62k docs/s** (1.54×) and per-bulk p50 **84 vs 152 ms**.
  - **On-disk size** — **9.85 vs 12.07 MB** (0.82×).
  - **Some feature ops** — `sort-heavy`, `search_after`, `_count` (fast columnar sort/count).
  - **kNN recall** — perfect **1.00** (exact brute-force) tying ES's HNSW on this set.
  - **Hybrid RRF availability** — runs out-of-the-box where this ES build cannot (license/version).
- **Where XERJ loses:**
  - **Raw lookup latency** — ES's sub-2 ms postings/BKD floor beats XERJ on **every** comparable
    read (`match_all`/`term`/`range`/`match`/`prefix`/`wildcard`/…). This is the honest headline:
    **XERJ wins 0 of the raw-read races.**
  - **O(N) brute-force cliffs** — `range(@timestamp)` (340 ms), `more_like_this` (269 ms), the
    scored-compound family `boosting`/`dis_max`/`function_score`/`pinned`/`bool(must+should)`
    (165–248 ms), and **seven ~1-second aggs** (`median_absolute_deviation`, `matrix_stats`,
    `scripted_metric`, `rare_terms`, `significant_terms`, `auto_date_histogram`, `missing`).
  - **kNN latency** — exact brute-force is **120× slower** than ES HNSW (310 ms vs 2.6 ms) — correct
    but not scalable; recall parity only holds because this 50k random set is trivially separable.
  - **p99 tails on several WIN aggs** — histogram/date_histogram/vwh/adjacency_matrix show 35–48 ms
    p99 spikes even where p50 wins.
- **Compat:** **0 XERJ-UNSUPPORTED** on the runnable matrix (XERJ runs every ES shape here); the only
  gaps are XERJ being *too lenient* (2 ES-REJECTS + `_count` param), one `percentile_ranks` key-format
  bug, and the untested geo/ip/nested/join/span/semantic surface.

**Net: 35 WIN / 36 LOSS / 11 TIE** across all perf-comparable dimensions — a near-even split, NOT
"81/91". XERJ owns aggregations, pipelines, ingest and disk; ES owns raw-read latency and vector
search. The old headline collapsed the moment the cache was turned off and the driver went
closed-loop.

---

## RECONCILIATION vs `READ_SCORECARD_2026-07-09.md` (the ~38-shape core)

That scorecard reported **7 WIN / 22 LOSS / 9 TIE** on an **enriched** corpus (it added an analyzed
`body` text field + a high-cardinality `doc_id` keyword). This matrix uses the **flat keyword**
corpus from `bench-matrix.mjs`, so the overlapping shapes agree **directionally** with two
corpus-driven drifts to note:

- **Same story on reads:** both scorecards show XERJ winning **0** raw-read races; ES's sub-ms
  lookup floor is unbeaten in both. The O(N) scored-query cliffs (`boosting`/`function_score`)
  reproduce (~160 ms enriched → ~245 ms here at 100k).
- **Same story on stat aggs:** `avg`/`sum`/`stats`/`extended_stats`/`value_count`/`percentiles`/
  `percentile_ranks` are WINs in both.
- **DRIFT — `cardinality`:** READ_SCORECARD scored it a **LOSS** (9.52 vs 3.79 ms) on the
  **high-cardinality** `doc_id`; here it's a **WIN** (0.51 vs 2.46 ms) on `top_doc`, which has only
  **12** distinct values. Cardinality's verdict is field-cardinality-dependent — flag when quoting.
- **DRIFT — `histogram`/`date_histogram`:** TIE in READ_SCORECARD (both sub-2 ms), WIN here — but
  with the p99-tail caveat above. Boundary flips at the sub-ms floor, not a code change.
- **New cliffs this matrix surfaced** (absent from the 38-shape core): `range(@timestamp)` 340 ms
  and the seven ~1-second exotic aggs — because this matrix exercises the full agg/query surface,
  not just the 38 core shapes.

---

*Closed-loop, single-client, XERJ query-cache OFF, ES default, 100k identical single-segment corpus.
XERJ `:9200` (this run's server, stopped by exact PID after), ES `:9201` (v8.13.4, untouched),
python `:8080` untouched. Committed engine at `bd3bb41`. No engine code changed; nothing committed.
Harness `full_matrix.mjs`; raw `results.json`; value spot-check `verify_values.mjs`.*
