# XERJ-vs-ES All-Wins Battle Plan — v2 (2026-07-10, rewritten on HEAD post-batch-12b)

**Goal:** every matrix cell an honest WIN vs live ES 8.13.4 — query cache OFF (`XERJ_DISABLE_QUERY_CACHE=1`, env-verified), `request_cache=false` both engines, closed-loop, warm-8 → p50/p99 × 50, correctness-checked per cell (top-10 ids + f32 score bits + max_score + totals vs ES). **A faster-but-wrong cell is a LOSS.** Methodology = `FULL_MATRIX_SCORECARD_2026-07-10.md`, verbatim bodies from `bench-matrix.mjs`.

**Baseline (this plan's ground truth):** HEAD re-measure 2026-07-10 (`results-2026-07-10-HEAD.json`, /tmp/xerj-allwins) = **Table A 46 W / 25 L / 6 T / 2 ES-REJECTS; grand total with Table B carried: 49 W / 26 L / 7 T.** Batch 12b verified live: `ids` LOSS→WIN (score 1.0 exact), `_mget` LOSS→WIN (1397 ms → 0.14 ms, source key order byte-matches ES), 5 agg band-edge TIE→WIN. Hit totals 79/79 identical to ES; no fast-but-wrong WIN survives. Box was noisier than the 07-10 audit (both engines' floors ~2-3× inflated symmetrically) — verdicts trustworthy, magnitudes inflated.

This version REPLACES the v1 plan (Jul-6 8-loss era). Status of v1 items:
- **Website false claims (v1 §3b): DONE** — commit `1401c4b` deployed; 89×/74×/2.8× removed sitewide. Residual debt: site read numbers must be refreshed from the honest cache-off scorecard after this campaign.
- **Harness honesty (v1 §3a): DONE** — cache-off + closed-loop + correctness columns are now the standing methodology (the 07-10 scorecard). Open-loop magnitudes repudiated (saturation artifacts, 2-77× inflated).
- **Mixed read-under-write p99 (v1 cells 4-8): SEPARATE TRACK** — not in the 79-cell matrix. Root cause stands (live-memtable reads under the writer's per-shard RwLock; see `MIXED_READ_UNDER_WRITE_FINDING_2026-07-08.md`, memory `mixed-p99-root-cause`). M1 (ArcSwap immutable mem-view) remains the fix; do not conflate with this matrix campaign.

---

## 1. The floor is not the problem — the engine walk is (2026-07-10 wire-level profile)

Profiled on the live box (tcpdump on lo, strace -T both servers, perf on a symbolized scratch build, XERJ phase log, the harness's exact node:http client). Full numbers in the floor-profile section of the campaign session; structure:

- **XERJ's HTTP floor BEATS ES 3-5×.** `_count` server dwell is **0.058 ms total** (recv + axum + auth/UUID/Trace/CORS middleware + route + serialize + writev) vs ES 0.304 ms. Both engines emit exactly ONE response packet, ONE read() client-side; XERJ's bodies are *smaller*. There is **no transport/response-path/client deficit** — the 2026-07-06 "~0.9 ms XERJ-specific client cost" is REFUTED (that was the undici tax, paid equally by both, plus engine-side agg cliffs since fixed).
- **Every microsecond of every loss above ~0.06 ms is engine execution.** On `term(status)` size:10 (4.4 vs 0.92 ms on the noisy box): perf shows 42% score-and-collect-every-match (ClauseEval::eval over all 100k rows, 98,752 candidate tuples pushed), 7.5% select_nth over 98,752, ~10% hydrating **256 stored docs for a 10-doc page** (`materialisation_limit=(from+size+100).max(256)`, index.rs:5187), ~6% final page sort whose comparator calls `lookup_seq_no` (dashmap string-hash) **per comparison**, 2.5% realloc growing an unreserved cands Vec.
- Behavioral isolation: same query size:0 = 0.39 ms vs size:10 = 5.9 ms. The count is free; the *hits page* costs everything.
- ES answers the same scored top-10 in ~0.6-0.9 ms because it never scores all matches (impact-based skipping) and fetches exactly 10 stored docs.

**Conclusion: the all-wins lever is bounded top-k collection + bounded hydration, not caching, not transport.** No query-result caching anywhere in this plan.

## 2. Current loss map (26), grouped by root cause

| class | cells | root cause |
|---|---|---|
| **A. Constant-score O(matches) walk** (12) | match_all, term(status), term(cache_hit), constant_score, exists, range(latency_ms), range(@timestamp), range(cost_usd), terms(model), deep from+size, prefix(model), wildcard(model) | Every per-row score is a per-segment constant, yet scored_columnar walks all 100k rows, pushes up to 98,752 candidates, select_nth's them, then hydrates 256 docs for a 10-doc page. All TIE-ORDER or EXACT correctness. Includes the two "floor" ranges and deep-paging. |
| **B. Ord-bucketed scored top-k** (8) | match(model), match_phrase(top_doc), match_bool_prefix, multi_match, query_string, simple_query_string, dis_max, bool m+f+s+mn | Scoring leaves are keyword terms → per-doc score depends only on *which terms match* = a small set of score classes (per-ord constants). Today: full scored walk O(matches). Two carry pre-existing scoring defects that must be fixed first (match_bool_prefix 2.52-vs-1.0, multi_match 1 ulp). |
| **C. Term-expansion scoring defect** (1) | fuzzy(model) | Same class-B walk + XERJ omits ES's fuzziness discount (SCORE-DIVERGE) — correctness first, then class-B machinery with per-ord (edit-distance-discounted) scores. |
| **D. O(N) brute cliff** (1) | more_like_this — 325 ms, flat-1.0 scores (wrong) | Never routed off the stored-doc brute scan; MLT term selection + real scoring missing. |
| **E. Script execution** (1) | scripted_metric — 1189 ms vs ES 6.6 | Both engines are O(N); ES JIT-compiles Painless, XERJ interprets per doc (~12 µs/doc vs ~65 ns/doc). JVM-vs-Rust is irrelevant while one side interprets. |
| **F. Response-path / de-route** (2) | highlight (26-ulp de-route off exact-BM25, ticket #3), _msearch (+0.67 ms envelope) | highlight bails the query off the scored_fast_plan path; _msearch pays per-sub-search floor overhead. |
| **G. kNN** (1, Table B) | kNN latency 120× (recall TIE 1.0) | Brute vector scan; `xerj-vector/src/hnsw.rs` exists (HnswIndex/HnswParams exported) but is UNWIRED into indexing/search. |

Open correctness tickets riding along: **#3** highlight de-route (class F), **#5** match_all-family page/tie order (ids 0,60,82 vs ES 0,1,2 — class A fixes it structurally: first-k-in-seq-order IS ES's order on this corpus), **#6** function_score tied-rank 4-9 order (WIN cell, flag only), fuzzy/mbp/mlt/multi_match scoring (classes B/C/D), Kahan F64-DRIFT (~1e-14, flagged not verdict-moving).

---

## 3. Ranked batches (cells-flipped per unit risk; every batch gated ES-YAML 1360/0 ×2 + bit-exact top-10 correctness vs live ES + no regression on current WINs)

### P1 — Constant-score short-circuit + hydration floor (class A) — **expected 10-12 flips** — size: 1-2 days — risk LOW-MED
The single highest-leverage batch on the board.
1. **Short-circuit in `scored_columnar` (index.rs:10052):** when every scoring leaf's per-row score is a per-segment constant (bare keyword/bool term, constant_score, filter-only bool, match_all, exists, numeric range, ord-SET constants for prefix/wildcard/terms), the kept set under (score desc, seq_no asc) is provably the FIRST (from+size) matching rows in seq order (the phase-3 total-order proof already in the code). Collect them with early exit; never push 98,752 candidates, never select_nth.
2. **Exact totals without the walk:** single term = the per-ord `df` phase 1 already reads for idf (ghost-corrected only when ghosts exist); match_all = shortcut_count (already wired); numeric range = binary search over the existing sort_shadow sorted column; prefix/wildcard/terms = sum of per_ord_count over matching ords.
3. **Drop the 256-doc hydration floor for proven-exact sets:** scored_columnar's kept set is exact → hydrate exactly from+size (keep the padding on the brute path where it guards dedup/ties). index.rs:5187.
4. **Micro:** precompute seq_no per hit before the final page sort (kill the per-COMPARISON `lookup_seq_no` dashmap get); `cands.reserve(df)` on paths that still walk.

Expected (scorecard-quiet magnitudes): term(status) 1.37→~0.2, term(cache_hit) 1.75→~0.2, constant_score 1.00→~0.15, match_all 0.97→~0.2, exists 0.60→~0.15, range ×3 → ~0.2, terms 1.88→~0.3, deep from+size 1.98→~0.5, prefix/wildcard 5.85/5.71→~0.5 — vs ES floors 0.16-1.05. **Also structurally closes ticket #5** (first-k-in-seq-order = insertion order = ES's 0,1,2,… page order on this corpus). Risk: narrow semantics gate, brute path stays the fallback, tie order (seq asc) unchanged; main hazard is the totals path when ghosts exist → gate on `ghost_events==0` per segment, fall back to walk otherwise. Gate additionally on a deletes-present A/B (inject 1 delete, assert totals + page still ES-exact via fallback).

### P2 — Ord-bucketed scored top-k (classes B + C) — **expected 6-8 flips** — size: 2-4 days — risk MED
When all scoring leaves are keyword-ord leaves (the entire bench FTS family — these fields are keywords), per-doc score = f(matched-term set) → enumerate the small set of score classes descending; within each class, rows in seq order until k filled; totals per class from per_ord_count / posting intersections. This is the honest analogue of Lucene's impact-based skipping — semantically general (any keyword-leaf scored query), not corpus-tuned.
- **Correctness FIRST, same batch (fast-but-wrong = LOSS):** fix `match_bool_prefix` (2.52 vs ES 1.0), `multi_match` 1-ulp, `fuzzy` fuzziness discount (per-ord score = term score × discount(edit distance) — the class machinery handles per-ord varying constants natively). Bit-exactness gate vs live ES on all 8 cells before timing.
- Targets: match(model) 4.05→sub-ms, match_phrase 2.68→sub-ms (keyword phrase ≡ term), simple_query_string 3.33, query_string 3.69, multi_match 5.74, dis_max 4.50 (2-term union = ≤3 score classes), bool m+f+s+mn 5.73 (the should-scoring is the classed part; filters prefilter), match_bool_prefix 4.88.
- Risk: dis_max/bool score composition across classes must reproduce batch-10/11's exact BM25 composition (reuse ScoredClause tree verbatim — only the *collection* changes, never the arithmetic). bool is the marginal one (ES 5.27-7.43 audit floor): if the class machinery lands, XERJ's floor advantage (0.06 vs 0.30 ms) should decide it; 3-round band-edge audit required.
- **Out of scope here:** true text-field BM25 top-k (varying tf/norms) = impact-ordered postings / block-max WAND — the multi-day infra item. Not needed for any current Table-A loss (all scoring leaves in the matrix are keyword/numeric), needed only if future matrix rows add text-field relevance races.

### P3 — Response path: raw _source splice + highlight re-route + _msearch (class F) — **expected 2 flips + cements P1** — size: 1 day — risk MED
1. **Raw `_source` splice** (ES's own approach): F2 `stored_slices_for` already yields each row's raw stored bytes — emit them verbatim into the response instead of serde_json → Value → clone → IndexMap → re-serialize per hit (scored_columnar phase 5 ~index.rs:10465 + API serialize). Kills ~10-30% CPU on hits pages, makes _source wire divergence structurally impossible (12b's preserve_order becomes moot). Fall back to the parsed path for _source filtering/highlight-needs-tree.
2. **highlight (ticket #3):** stop de-routing off scored_fast_plan when `highlight` is present — score on the exact path, then fragment ONLY the top-k page docs. Fixes the 26-ulp diverge AND the 3.46 ms delta together.
3. **_msearch:** the +0.67 ms is N× per-sub-search floor; after P1 each sub-search floor drops to ~0.1 ms → flips on its own; add a single-pass envelope parse if residual.

### P4 — more_like_this (class D) — **expected 1 flip + kills the worst query cliff** — size: 1-2 days — risk MED
The bench body is MLT with **like-TEXT** (`like: 'runbook/oncall.md'`, fields `[top_doc]`, min_term_freq/min_doc_freq 1 — verified in bench-matrix.mjs:324), no seed-doc fetch needed: analyze the like-text into terms, ES's MLT term selection (tf-idf ranking, max_query_terms=25 default) over the same per-ord df stats P1 reads, then execute as an OR of selected terms through **P2's class machinery**. Must fix the flat-1.0 scoring (SCORE-DIVERGE) to ES's MLT scoring in the same batch. 325 ms → low-single-digit ms vs ES 0.66; depends on P2. Exactness vs ES's MLT selection heuristics is the re-spin risk — budget one correctness iteration.

### P5 — scripted_metric: script→columnar compilation (class E) — **expected 1 flip** — size: 1-3 days — risk LOW-MED
Script shape VERIFIED (bench-matrix.mjs:358): `init: state.s=0; map: state.s+=doc.latency_ms.value; combine: return state.s; reduce: sum over states` — i.e. a columnar sum over `latency_ms`, the exact fold the already-winning `sum` agg does in 0.05 ms. Build a small Painless-subset compiler (state fields, `doc.<f>.value` access, `+ - * /`, accumulate, reduce-loop) that lowers map/combine/reduce to a closure over the existing numeric columns; interpreter stays as the general fallback and the A/B oracle (compiled result must be value-identical to interpreted AND to ES). 1189 ms → target <1 ms (ES 6.6). Honest bound: scripts outside the subset still take the interpreter — the CELL flips, the general script story doesn't.

### P6 — kNN: wire hnsw.rs (class G, Table B) — **expected 1 flip** — size: multi-day — risk MED
`xerj-vector` already exports `HnswIndex`/`HnswParams` — unwired. Wire: build per-segment HNSW at flush/merge (100k × dims, M=16 graph ≈ ~13 MB — trivial), search path with ef_search tuned so recall ≥ ES's (current recall TIE at 1.00 must NOT drop — recall is a correctness gate here; a faster-but-lower-recall kNN is a LOSS by campaign rules). 120× behind → ES-class single-digit ms. Requires an ES-writable bench window for Table B re-measure (current ES /bench is READ-ONLY — schedule a separate seeded ES index or a sanctioned re-run).

### P7 — Full honest re-measure + band-edge audit + publish
After each batch and finally: quiet box (no chrome/profilers — the 07-10 caveat), fresh XERJ boot with env-verified cache-off, identical seeded corpus, full 79-cell matrix + Table B, **3-round audit on every band-edge cell** (the bool/composite lesson: main-run TIE ≠ verdict; 3/3 rounds decide). Only then update SCORECARD/README/llms/site (site still carries pre-campaign read numbers).

**Ordering rationale:** P1 flips ~half the board alone at the lowest risk and unblocks nothing. P2 is the second half of the query board and P4's prerequisite. P3 is cheap and hardens P1's wins + closes 2 tickets. P5/P6 are independent silos (parallelizable in worktrees). Mixed-p99 (M1 ArcSwap mem-view) stays a separate track — it shares no code path with these batches.

---

## 4. All-wins feasibility — honest verdict per remaining loss

**Flippable with engineering (high confidence, existing data structures only):** the 12 class-A cells (P1 — the floor profile PROVES the entire deficit is walk+hydration and XERJ's HTTP floor already beats ES 3-5×; `_count` dwell 0.058 vs 0.304 ms is the existence proof), highlight, _msearch, deep-paging (P3), and — contingent on the correctness fixes landing bit-exact — the 8 class-B/C cells (P2). That is 22-23 of 26.

**Flippable with engineering, moderate uncertainty:** more_like_this (P4 — mechanism is clear; ES MLT heuristic exactness is the schedule risk). bool m+f+s+mn and query_string are the two marginal cells where ES's floor is only ~20-30% ahead — P1+P2 should decide them via XERJ's 5× floor advantage, but they are band-edge and could land TIE; call them 85%.

**Flippable only with new infra (sized):** scripted_metric — needs a script→columnar mini-compiler (1-3 days; P5); NOT structurally hard — ES is also O(N), it just JIT-compiles while XERJ interprets, and the bench script is verified to be a plain columnar sum (`state.s+=doc.latency_ms.value`) that the already-winning `sum` agg computes in 0.05 ms; a compiled Rust fold beats JIT'd Painless comfortably. kNN latency — needs the already-written `hnsw.rs` wired into flush/merge + search (multi-day; P6), an ES-writable re-measure window, and recall held at 1.00 (a recall drop is a LOSS by campaign rules).

**Structurally hard / not winnable as specified:** (a) the 7 TIEs at the mutual sub-ms floor (match_none, terms agg, filters→now WIN, composite, random_sampler, _count, kNN recall): where both engines sit inside the 0.30 ms band with XERJ already ≤ ES, a WIN verdict is arithmetically impossible without ES slowing down — changing the band to manufacture wins would be gaming. Honest campaign end-state = **0 LOSSES, every TIE with XERJ ≤ ES p50, every WIN correctness-clean**. (b) ES-REJECTS ×2 (ES refuses the body) — unscoreable. (c) Nothing else on the board is structurally ES-favored: no remaining loss depends on JVM magic, postings formats XERJ lacks are only needed for text-field relevance races not in the matrix, and the transport myth is dead.

**Net:** 24/26 losses have a concrete engineering path on existing infra; 2/26 need sized new infra (script compiler, HNSW wiring) that is real but bounded. Zero losses are "ES is unbeatable here."

---

## 5. Standing rules & risks

- **No query-result caching anywhere.** Cache stays OFF in every measurement; every fix must be per-request compute. The 2026-07-01 mirage does not get a sequel.
- **Correctness before speed, per cell:** top-10 ids + f32 bits + max_score + totals vs live ES before any timing counts; deletes-present A/B for every path that trusts flush-time stats (`ghost_events` gate); the 1360/0 ES-YAML gate ×2 per batch.
- **1360-gate risk:** P1/P2 touch scored_columnar/scored_fast_plan — the batch-5/10/11 exactness suites must be re-run bit-exact (the composition arithmetic is reused verbatim; only collection order changes). P3's raw splice must A/B byte-identical bodies vs the parsed path on the full matrix.
- **Regression tripwires that bit us before:** sticky `ghost_events` downgrades (one delete flips paths — every P1 shortcut needs the fallback proven), function_score off-gate brute (ticket: any body-shape drift re-routes — extend the gate tests), aliases don't survive restart (side-finding — file separately, breaks restart-based harnesses), `took` as_millis truncation (cosmetic; fix to micros opportunistically).
- **Measurement discipline:** quiet box or don't publish; timing.lock protocol between agents; 3-round audit for band-edge verdicts; magnitudes from noisy windows are structure-only.
- Git: nothing committed from measurement sessions; batches commit as xerj-org only, after gates.
