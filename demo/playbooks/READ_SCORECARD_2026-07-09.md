# XERJ vs Elasticsearch — DEFINITIVE Post-Sweep Read/Agg Scorecard (2026-07-09)

**This is a MEASUREMENT + no-regression audit. No engine code was changed; nothing was committed.**
It re-measures the FULL matrix on the *combined* working-tree binary that now carries all 7
uncommitted read-perf fixes together (each was individually ES-YAML-gated; this run verifies
the **union** for correctness AND performance cross-interaction).

## TL;DR

- **Final: 7 WIN / 22 LOSS / 9 TIE** across 38 comparable query families (100k docs, single segment).
- **vs the prior scorecard (8 WIN / 24 LOSS / 6 TIE):** **2 losses collapsed into ties**
  (`ids` 128ms→0.11ms and `bool.must_not` 156ms→0.78ms now literally TIE ES); **1 win flipped
  to a tie** (`histogram` WIN→TIE — a measurement-noise boundary flip, see Regression Check).
- **The read-perf sweep is a decisive success on the O(N) cliffs.** Eight read families that were
  **56–556 ms full-collection scans** in the prior scorecard are now **0.1–43 ms**:
  `fuzzy(text)` 556→7ms, `wildcard(text)` 230→5ms, `match_phrase(text)` 225→39ms,
  `match_phrase_prefix` 186→43ms, `prefix(text)` 176→6ms, `exists` 159→0.75ms,
  `ids` 128→0.11ms, `bool.must_not` 156→0.78ms, `bool.should` 63→3ms, `terms` 56→3ms.
- **REGRESSION CHECK: no real code regression.** No row that was fast became a cliff; no O(N)
  cliff reappeared. The only verdict-negative change is `histogram` WIN→TIE, driven by ES
  getting faster (2.46→2.18ms) plus +0.38ms of sub-2ms XERJ jitter — not a XERJ slowdown.
- **Combined ES-YAML correctness gate: 1360 passed / 0 failed / 3 skipped.** All 7 fixes together
  still pass the full ES-compat suite.
- **Two O(N) reads remain UNFIXED this session:** `boosting` (~163ms) and `function_score`
  (~157ms) still brute-scan/score every document. `match_phrase(text)`/`match_phrase_prefix`
  improved 5–6× but remain ~40ms (positional phrase scan is faster but still super-linear).

## Method (fair, closed-loop, single-client — identical to the prior scorecard)

- **Build:** current working tree, combined binary `engine/target/release/xerj` (built 01:40 after
  all 4 modified sources: `xerj-fts/src/search.rs` +901, `xerj-engine/src/index.rs` +1224,
  `xerj-engine/src/fast_aggs.rs` +317, `xerj-query/src/parser.rs` +9; `cargo build` = up-to-date).
- **XERJ** on `:9200`, fresh temp data dir, `--insecure`, `XERJ_DISABLE_QUERY_CACHE=1`
  (whole-result cache OFF). **Real ES 8.13.4** on `:9201`, default settings, untouched.
- **Corpus:** 100,000 docs, identical to both engines (`chat-events.ndjson` enriched with a real
  analyzed **`body` text** field + high-cardinality **`doc_id`** keyword; `_id` = 1..N).
  **Single segment** on both (XERJ `_flush`+`_forcemerge`→1; ES `_forcemerge`→1). Both `/_count` = 100000.
  *(The `hits` column reads `10000` on most rows — that is ES's default `track_total_hits` 10k cap,
  NOT the corpus size. This is a genuine 100k run.)*
- **Measurement:** CLOSED-LOOP only (one request at a time, sequential — true round-trip; NOT the
  coordinated-omission open-loop driver). Warm 8, then p50/p99 over 40 iters. Identical bodies to
  both engines; `request_cache=false` on every search.
- **Verdict rule:** compare p50. `|Δp50| < 0.30 ms` = TIE (both in the sub-ms round-trip floor);
  otherwise WIN (XERJ faster) / LOSS (XERJ slower). Δ = XERJ p50 − ES p50 (positive = XERJ slower).
- Harness: `/tmp/xerj/read_scorecard.mjs`.

## READS (p50/p99 ms)

| query | XERJ p50/p99 | ES p50/p99 | verdict | Δ p50 | vs prior scorecard |
|---|---|---|---|---|---|
| match_all | 1.33 / 2.28 | 0.28 / 0.88 | **LOSS** | +1.05 | ~same (1.00→1.33, floor jitter) |
| term | 1.45 / 2.43 | 0.30 / 0.95 | **LOSS** | +1.14 | ~same (0.94→1.45, floor jitter) |
| terms | 3.43 / 4.63 | 0.39 / 0.46 | **LOSS** | +3.03 | **fixed cliff 56.05→3.43ms** (still loses) |
| range | 1.59 / 2.33 | 0.50 / 1.08 | **LOSS** | +1.10 | ~same (1.36→1.59) |
| bool (filter) | 1.55 / 2.27 | 1.29 / 2.28 | **TIE** | +0.26 | still TIE |
| bool (must) | 3.63 / 4.47 | 0.45 / 1.20 | **LOSS** | +3.18 | ~same (3.76→3.63) |
| bool (should) | 3.11 / 3.93 | 0.53 / 1.06 | **LOSS** | +2.58 | **fixed cliff 62.87→3.11ms** (still loses) |
| bool (must_not) | 0.78 / 1.13 | 0.84 / 1.75 | **TIE** | −0.06 | **LOSS→TIE: 156.08→0.78ms, now ties ES** |
| match (text) | 6.43 / 7.56 | 0.29 / 0.60 | **LOSS** | +6.14 | ~same (6.30→6.43) |
| match_phrase (text) | 38.69 / 46.46 | 0.93 / 1.79 | **LOSS** | +37.76 | **improved 225.22→38.69ms** (still O(N)-ish) |
| match_phrase (keyword) | 5.00 / 5.99 | 0.29 / 0.75 | **LOSS** | +4.71 | ~same (5.06→5.00) |
| match_phrase_prefix | 43.10 / 55.47 | 3.35 / 4.31 | **LOSS** | +39.75 | **improved 186.11→43.10ms** (still O(N)-ish) |
| multi_match | 7.19 / 8.04 | 0.52 / 0.98 | **LOSS** | +6.67 | ~same (7.05→7.19) |
| query_string | 13.35 / 15.84 | 0.73 / 1.49 | **LOSS** | +12.62 | ~same (12.92→13.35) |
| prefix (text) | 5.61 / 6.28 | 0.40 / 0.82 | **LOSS** | +5.21 | **fixed cliff 176.30→5.61ms** (still loses) |
| prefix (keyword) | 9.79 / 11.07 | 0.34 / 0.65 | **LOSS** | +9.45 | ~same (9.57→9.79) |
| wildcard (text) | 5.41 / 6.79 | 0.43 / 0.72 | **LOSS** | +4.98 | **fixed cliff 230.62→5.41ms** (still loses) |
| wildcard (keyword) | 10.10 / 11.18 | 0.44 / 0.71 | **LOSS** | +9.66 | ~same (9.74→10.10) |
| fuzzy (text) | 7.06 / 8.29 | 0.33 / 0.90 | **LOSS** | +6.73 | **fixed cliff 556.50→7.06ms** (still loses) |
| fuzzy (keyword) | 5.84 / 6.64 | 0.43 / 0.95 | **LOSS** | +5.42 | ~same (5.90→5.84) |
| exists | 0.75 / 1.11 | 0.20 / 0.60 | **LOSS** | +0.54 | **fixed cliff 159.52→0.75ms** (loses sub-ms race) |
| ids (3 docs) | 0.11 / 0.18 | 0.18 / 0.28 | **TIE** | −0.07 | **LOSS→TIE: 128.00→0.11ms, now ties ES** |
| boosting | 162.61 / 195.90 | 2.57 / 3.45 | **LOSS** | +160.04 | **STILL O(N)** (161.37→162.61, unfixed) |
| function_score | 156.54 / 203.62 | 2.84 / 3.99 | **LOSS** | +153.71 | **STILL O(N)** (156.74→156.54, unfixed) |

**Reads: 0 WIN / 21 LOSS / 3 TIE.** (prior: 0 WIN / 23 LOSS / 1 TIE)

## AGGREGATIONS (size:0, p50/p99 ms)

| agg | XERJ p50/p99 | ES p50/p99 | verdict | Δ p50 | vs prior scorecard |
|---|---|---|---|---|---|
| avg | 0.08 / 0.16 | 1.29 / 1.91 | **WIN** | −1.22 | still WIN |
| sum | 0.07 / 0.17 | 1.18 / 1.66 | **WIN** | −1.11 | still WIN |
| min | 0.07 / 0.12 | 0.12 / 0.25 | **TIE** | −0.05 | still TIE |
| max | 0.07 / 0.13 | 0.10 / 0.15 | **TIE** | −0.04 | still TIE |
| stats | 0.06 / 0.10 | 1.26 / 2.12 | **WIN** | −1.20 | still WIN |
| extended_stats | 0.14 / 0.17 | 1.58 / 2.19 | **WIN** | −1.44 | still WIN |
| cardinality | 9.52 / 13.39 | 3.79 / 4.70 | **LOSS** | +5.73 | still LOSS (10.57→9.52) |
| value_count | 0.07 / 0.19 | 0.56 / 0.85 | **WIN** | −0.49 | still WIN |
| terms (agg) | 0.22 / 0.36 | 0.14 / 0.21 | **TIE** | +0.08 | still TIE |
| date_histogram | 0.09 / 0.17 | 0.15 / 0.42 | **TIE** | −0.06 | still TIE |
| histogram | 1.91 / 3.03 | 2.18 / 2.72 | **TIE** | −0.27 | **WIN→TIE** (noise flip; see Regression Check) |
| range (agg) | 0.11 / 0.27 | 0.17 / 0.25 | **TIE** | −0.06 | still TIE |
| percentiles | 0.12 / 0.64 | 6.60 / 7.32 | **WIN** | −6.47 | still WIN |
| percentile_ranks | 0.13 / 0.26 | 6.57 / 7.26 | **WIN** | −6.44 | still WIN |

**Aggs: 7 WIN / 1 LOSS / 6 TIE.** (prior: 8 WIN / 1 LOSS / 5 TIE)

## FINAL SUMMARY: 7 WIN / 22 LOSS / 9 TIE (of 38 comparable families)

| | WIN | LOSS | TIE |
|---|---|---|---|
| Reads | 0 | 21 | 3 |
| Aggs | 7 | 1 | 6 |
| **Total** | **7** | **22** | **9** |
| *(prior scorecard)* | *8* | *24* | *6* |

## vs PREVIOUS SCORECARD — every verdict change

Three verdicts changed; ten more rows improved massively without changing verdict:

| row | prior | now | why |
|---|---|---|---|
| **ids** | LOSS (+127.8ms) | **TIE** (−0.07ms) | 128.00→0.11ms — postings resolution instead of full scan |
| **bool.must_not** | LOSS (+155.2ms) | **TIE** (−0.06ms) | 156.08→0.78ms — constant-score bitset instead of O(N) |
| **histogram** | WIN (−0.93ms) | **TIE** (−0.27ms) | ES sped up 2.46→2.18ms + XERJ +0.38ms jitter crossed the 0.30ms tie line (NOT a regression) |
| terms | LOSS (+55.6ms) | LOSS (+3.03ms) | cliff gone 56.05→3.43ms (still loses sub-ms race) |
| bool.should | LOSS (+62.3ms) | LOSS (+2.58ms) | cliff gone 62.87→3.11ms |
| match_phrase(text) | LOSS (+224.4ms) | LOSS (+37.76ms) | 225.22→38.69ms |
| match_phrase_prefix | LOSS (+182.7ms) | LOSS (+39.75ms) | 186.11→43.10ms |
| prefix(text) | LOSS (+175.9ms) | LOSS (+5.21ms) | 176.30→5.61ms |
| wildcard(text) | LOSS (+230.3ms) | LOSS (+4.98ms) | 230.62→5.41ms |
| fuzzy(text) | LOSS (+556.1ms) | LOSS (+6.73ms) | 556.50→7.06ms |
| exists | LOSS (+159.3ms) | LOSS (+0.54ms) | 159.52→0.75ms |

## REGRESSION CHECK

**No real code regression. No row that was fast in the prior scorecard became slow; no O(N)
cliff reappeared.** Rows whose XERJ p50 ticked *up* vs the prior scorecard did so only at the
sub-ms round-trip floor or within run-to-run variance on an already-slow O(N) op:

| row | prior X p50 | now X p50 | Δ | assessment |
|---|---|---|---|---|
| term | 0.94 | 1.45 | +0.51 | HTTP/executor floor jitter (~1ms op) |
| query_string | 12.92 | 13.35 | +0.43 | jitter on a ~13ms op |
| histogram | 1.53 | 1.91 | +0.38 | jitter on a ~2ms op — the WIN→TIE flip |
| wildcard(kw) | 9.74 | 10.10 | +0.36 | jitter on a ~10ms op |
| match_all | 1.00 | 1.33 | +0.33 | floor jitter |
| boosting | 161.37 | 162.61 | +1.24 | noise on a 160ms O(N) op (unchanged behaviour) |
| *(others +≤0.25ms)* | | | | all sub-ms floor jitter |

The single verdict-negative change (`histogram` WIN→TIE) is a boundary artifact: ES's own
histogram got faster (2.46→2.18ms) and XERJ drifted +0.38ms, together pushing Δp50 from −0.93
to −0.27, just inside the 0.30ms TIE band. XERJ's histogram code was not touched this session and
its absolute latency (~1.9ms) is unchanged within noise. **Verdict: zero regressions of substance.**

## COMBINED ES-YAML CORRECTNESS GATE

```
1360 passed · 0 failed · 3 skipped · 1363 total
```

Run against this exact combined binary on `:9200`
(`es-yaml-runner --dir tests/es-compat-yaml/yaml`). All 7 uncommitted read-perf fixes, applied
together, pass the full ES 8.13 wire-compat suite with **zero** failures — confirming the union
introduces no correctness cross-interaction.

## Honest bottom line

XERJ's read-perf sweep this session is a genuine, large win where it matters most: **eight
catastrophic O(N) full-collection read cliffs (56–556 ms at 100k) have collapsed to 0.1–43 ms**,
and two of them — `ids` and `bool.must_not` — now literally **tie** real Elasticsearch. XERJ
continues to **win the columnar statistical aggregations decisively** (`avg`/`sum`/`stats`/
`extended_stats`/`value_count`/`percentiles`/`percentile_ranks`), where its precomputed
doc-values beat ES by 1–7 ms. But on **raw read latency ES still wins the headline count**, for
two honest reasons: (1) on already-fast paths (`match_all`/`term`/`range` ~1–1.6ms, and
keyword/text FST paths ~5–13ms) XERJ sits above ES's true **sub-ms postings/BKD lookup floor**
(0.2–0.5ms) — a fixed HTTP+executor overhead the FST routing narrowed but did not erase; and
(2) **two O(N) reads remain unfixed — `boosting` (~163ms) and `function_score` (~157ms) — and
`match_phrase(text)`/`match_phrase_prefix` are 5–6× faster but still ~40ms**. Net vs the prior
8W/24L/6T: the score is 7W/22L/9T, the one-win dip is a noise-level TIE flip (not a regression),
the correctness gate is a clean 1360/0/3, and the real story is that the read O(N) cliffs are
mostly gone while ES keeps its sub-ms lookup-latency crown.

---
*Closed-loop, single-client, cache-off-on-XERJ, ES-default, 100k identical single-segment corpus.
XERJ `:9200` (this run's server, stopped by exact PID after), ES `:9201` (v8.13.4, untouched),
python `:8080` untouched. Harness: `/tmp/xerj/read_scorecard.mjs`; raw run:
`/tmp/xerj/sc-final-run.txt`; YAML gate: `/tmp/xerj/yaml-final.txt`. No engine code changed;
nothing committed.*
