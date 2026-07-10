# XERJ-vs-ES Benchmark Battle Plan — close all 8 LOSE cells, honestly

**Goal:** 100% ES compatibility **and** XERJ winning **every** cell of the matrix (zero LOSE), with an **honest** scorecard and website.
**Baseline:** Jul-6 SCORECARD.md (`caa607a` era) = **81 WIN / 8 LOSE / 2 N/A**. HEAD at time of writing: `2022a3b` on `fix/sort-topn-correctness`.
**Source of the 8 losses (verified against `demo/playbooks/SCORECARD.md`):**

| # | Cell (SCORECARD row) | XERJ | ES | Ratio | Verdict |
|---|---|---|---|---|---|
| 1 | read agg: filter (p50 ms) | 0.93 | 0.71 | 0.77× | LOSE |
| 2 | read pipe: sum_bucket (p50 ms) | 1.19 | 1.12 | 0.94× | LOSE |
| 3 | read pipe: derivative (p50 ms) | 1.13 | 0.67 | 0.60× | LOSE |
| 4 | mixed match_all (p99 ms, under write) | 65.48 | 2.30 | 0.04× | LOSE |
| 5 | mixed bool (p99 ms, under write) | 63.07 | 9.55 | 0.15× | LOSE |
| 6 | mixed range (p99 ms, under write) | 151.89 | 5.11 | 0.03× | LOSE |
| 7 | mixed terms (p99 ms, under write) | 61.84 | 2.70 | 0.04× | LOSE |
| 8 | mixed cardinality (p99 ms, under write) | 98.92 | 18.84 | 0.19× | LOSE |

The 8 split into **three families** with different truths:
- **Cells 4–8 (mixed p99):** ONE architectural mechanism — reads fold the **live, mutable** memtable under the **same per-shard `parking_lot::RwLock` the writer mutates**, inside `block_in_place`, under a 300 req/s **open-loop** driver → coordinated-omission snowball. This is the dominant, real, CI-failing loss.
- **Cells 2–3 (pipeline p50):** **measurement noise**, not a deficiency — identical code path to pipeline cells that WIN (avg_bucket 2.13×, cumulative_sum 4.71×). Sub-ms p50 with 120 iters is inside the harness jitter band.
- **Cell 1 (filter p50):** a real but **tiny** O(N) columnar fold with per-row virtual dispatch; ~0.22 ms over XERJ's own agg floor, partly noise.

---

## 1. The 8 losses: root cause → chosen fix → effort → expected result

| # | Cell | Root-cause one-liner (code-grounded) | Chosen fix | Effort | Expected result |
|---|---|---|---|---|---|
| 4 | mixed **match_all** p99 (0.04×) | Pure lock-contention floor: `doc_ids_bounded` (memtable.rs:725) fans all 16 shards `s.read()` while the turbo writer holds `s.write()` per 512-doc chunk (index.rs MEMTABLE_INSERT_CHUNK); `block_in_place` pins a worker per stall; 300 req/s open-loop piles up. Compute is cheap → 65 ms is ~pure contention. | **M1** lock-free memtable read snapshot (+ **M2** kill the snowball) | L (+M) | ~65 ms → single-digit ms (sub-2 ms quiescent already achieved). **WIN** |
| 7 | mixed **terms** p99 (0.04×) | Same floor; `terms_counts_columnar` (memtable.rs:1184) fold is cheap → 62 ms is contention. | **M1** (+M2) | L | ~62 ms → single-digit ms. **WIN** |
| 5 | mixed **bool** p99 (0.15×) | Contention **+** O(memtable) hydration: `mem_bool_preds` bails on the `match` clause → `DocsForScan(all_docs_with_sources_arc)` hydrates the whole memtable per read (memtable.rs:751). | **M3** lift `match` on keyword→Term (bounded columnar path) **+ M1/M2** | S + L | ~63 ms → single-digit ms. **WIN** |
| 6 | mixed **range** p99 (0.03×, worst) | Contention **+** O(memtable) numeric walk: `doc_values_bool_hits` (memtable.rs:2625) scans all `0..n` per shard for the exact total on a broad `cost_usd>=0.01`, even for size:10. | **M4** per-shard sorted numeric index → top-(from+size)+total in O(log N+k) **+ M1/M2** | M + L | ~152 ms → single-digit ms. **WIN (highest single-cell lever)** |
| 8 | mixed **cardinality** p99 (0.19×) | Contention; the bench op is **UNFILTERED** so `e69f80e` (filtered branch) does NOT touch it; extra 16-shard read-lock round (`terms_counts_columnar` rayon fan-out) adds ~33 ms over match_all. | **M1/M2** (+ optional: fold extra lock rounds, drop rayon par_iter) | L | ~99 ms → ~ES 18.8 ms or better. **WIN** |
| 1 | read agg: **filter** p50 (0.77×) | Real O(N) columnar fold: `exec_filter`→`fused_seg_pass` (fast_aggs.rs:1177) scans every row with a non-inlinable `&mut dyn FnMut` slot call + `SegPred`/`MetricKind` enum dispatch; `doc_count` incremented in the hot closure instead of O(1) `per_ord_count`. Gap ~0.22 ms, partly noise. | **F1** monomorphize single-leaf-pred filter+metric fold **+ F2** O(1) doc_count via `per_ord_count` | S + S | 0.93 → ~0.6–0.7 ms → parity/**WIN** |
| 2 | read pipe: **sum_bucket** p50 (0.94×) | **Not slower server-side.** Same `date_histogram`+`sum` parent + O(#buckets) sibling reduction as avg_bucket (WINs 2.13×). 120-iter open-loop p50 lands bimodally (~0.37 vs ~1.16 ms) — GC/scheduler beat vs the 5 ms cadence. | **H2** harness noise-band TIE + raise iters (+ optional H-eng variance trim) | S | Moves to **TIE** (server times equal within noise). Removes the false LOSE. |
| 3 | read pipe: **derivative** p50 (0.60×) | Identical to #2: nested pipeline via `apply_bucket_pipeline_ops`, same parent as serial_diff (WINs 2.56×). 0.60×/0.94× are noise artifacts; ES itself shows the same spread (derivative 0.67 vs sum_bucket 1.12). | **H2** (as #2) | S | Moves to **TIE**. Removes the false LOSE. |

**Net:** 5 mixed cells flip via one architectural fix (M1) + tail control (M2) + two cheap per-cell levers (M3/M4). 1 filter cell flips via a small monomorphization (F1/F2). 2 pipeline cells were never real losses — they resolve to TIE the moment the harness stops scoring sub-ms noise as LOSE (H2).

---

## 2. Execution order (biggest impact / lowest risk first)

The 5 mixed cells are the dominant loss and share **one mechanism**, so the engine work leads with that family. But two things must land **first** for integrity and to make the final proof trustworthy: the harness-honesty fixes (which also *legitimately* close cells 2 & 3) and the website corrections. Engine fixes (M-series, F-series) have **no dependency** on the harness work and can proceed in parallel — but the **final all-wins re-measurement (§4) must run on the fixed, honest harness.**

### Phase 0 — Integrity & harness (prerequisite; closes #2, #3; unblocks honest §4)
Low risk, mandatory. Details in §3.
- **H1 — Kill the read query-cache mirage.** Vary a param per iteration (novel term/id/range value) so every read misses `query_cache` (index.rs:4504 has no size gate; a hit at 4536 returns a `took_ms=0` clone). Report **p50 AND p99 uncached**. ⚠️ This may **expose new losses among the current 81 WINs** — that is the point; the honest uncached baseline is unknown until measured.
- **H2 — Noise-band + iters.** In `bench-matrix.mjs` raise read iters 120→≥1000, warmup 15→≥50, and make `scoreRow` (line 625) return **TIE** when `|xerj−es| ≤ max(0.30 ms, 20% of faster engine)`. Print both p50s regardless. **Closes #2 and #3 legitimately.**
- **H3 — Iso-write-rate for mixed.** Throttle the detached `_bulk` writer to an identical target docs/s both engines sustain; log **offered-vs-achieved** write rate per engine next to p99 (today XERJ accepts ~1.5× faster → more merge pressure → inflated p99).
- **H4 — Real correctness gate.** `readSignal`/`signalMismatch` currently check only `aggregations[0]` and tolerate 10% hit drift. Compare the **full aggregation subtree** (incl. scored pipeline values) and tighten hit-count tolerance to **0** for exact-count shapes.
- **H5 — Quarantine stale harnesses.** `bench-vs-es.mjs` (undici/closed-loop, writes the canonical `BENCHMARK_VS_ES.md`) and `bench.mjs` (points "ES" at :9200 = XERJ) must refuse to overwrite canonical files + print a deprecation banner, or be deleted.
- **H6 — Add a deletes-present read family** (see §5 sticky-gate risk) so future scorecards can't hide the O(N) downgrade the append-only corpus masks.

### Phase 1 — Mixed read-under-write (cells 4–8, the dominant loss)
Ship the fast, lower-risk mitigations first to compress the tail, then the real ES-parity fix.
1. **M2 (M, medium risk) — Kill the coordinated-omission snowball.** (a) Bound in-flight searches with a fair admission semaphore and shed/timeout queued searches **before** they run; the cooperative deadline already exists (index.rs:4780–4795) but is **dead code** under `block_in_place` — wire it so stalled searches return `timed_out:true` (ES semantics). (b) Stop `block_in_place` pinning a whole worker on a contended memtable lock — `try_read` + short bounded wait + cooperative yield, or move the fan-out to a bounded blocking pool. **Compresses the 62–152 ms tails to low-double-digit ms even before M1.**
2. **M3 (S, low risk) — bool.** In `mem_bool_preds`, resolve `match` on a keyword/exact field (single analyzed token) to a **Term** predicate so the bench bool takes `doc_values_bool_query` instead of `DocsForScan` hydration. Same design as segment bool prefilter `77586f0`; per-doc `doc_matches_query` recheck already drops false positives. Self-contained.
3. **M4 (M, medium risk) — range.** Bound `doc_values_bool_hits` (memtable.rs:2625): for unsorted size:N range with no aggs, resolve page+count via a per-shard **sorted numeric index** (reuse the `sort_cand_cache` machinery, memtable.rs:648–717) → top-(from+size) in O(log N + k); derive total from sorted bounds (delete-aware live count). Removes the worst-cell O(N) walk.
4. **M1 (L, high risk) — the real ES-parity fix.** Lock-free memtable **read snapshot**: publish a per-shard `ArcSwap<ImmutableMemView>` the writer refreshes at a bounded cadence (the *exact* pattern the store already uses for segments — `ArcSwap<IndexSnapshot>`, lock-free load at index_store.rs:1450). All reads (`doc_ids_bounded`, `doc_values_bool_query`, `terms_counts_columnar`, `all_docs_with_sources_arc`) load an `Arc`/len and **never block on ingest**. This is the **only** fix that touches match_all's pure-contention floor and closes all 5 cells. Hard-gate on ES-YAML **1360/0** + ingest throughput (must not regress the beat-ES ingest win — a per-doc snapshot rebuild historically regressed ingest ~4×) + the auto-id mixed repro.
   - **Cardinality note:** confirm-and-close — do **not** expect a re-measure to move cell 8 without M1/M2; `e69f80e` fixed only the *filtered* branch and the bench op is unfiltered. Optional follow-ups once M1 lands: fold the extra 16-shard lock rounds into one `s.read()` pass and read keyword-dict KEYS directly (O(distinct)) instead of `terms_counts_columnar`; drop the rayon `par_iter` (memtable.rs:1184) for a serial loop to trim dispatch jitter.
5. **M5 (S, do NOT ship as primary) — flush cadence hedge.** Right-size/stagger the per-shard flush threshold toward ES's ~512 MB-buffer cadence to shrink O(N) arms + write-lock windows. **Gaming risk** if tuned to the test window — principled production default only, complement to M1/M2, never standalone.

### Phase 2 — read agg: filter (cell 1) — parallelizable, low risk
6. **F1 (S) — Monomorphize** the single-leaf-predicate filter+metric fold: when `exec_filter`'s compiled pred is a single `TermKw/TermsKw/RangeNum/MatchAll` and metrics are few, bypass `fused_seg_pass` + the `&mut dyn FnMut` closure and run a tight raw-slice loop (`if ords[row]==ord { acc.add(f64::from_bits(data[row])) }`) so the compiler auto-vectorizes. Factor one shared `#[inline]` fold fn + assert equality vs the brute path in a unit test.
7. **F2 (S) — Decouple doc_count** from the fold: compute the bucket count via `per_ord_count`/`range_count` (O(1)/O(log n)) instead of `count += 1` in the hot closure.
   - Defer **F3** (ord→rows postings on `KeywordColumn`) and **F4** (non-allocating numeric range-rows iterator) unless the cell fails to flip — they generalize the whole filter/filters/adjacency_matrix family but add per-column memory/decode cost.

**Rationale for ordering:** Phase 0 is a hard prerequisite (honesty + fair §4) and freely closes 2 of 8. Within Phase 1, M2/M3 are lower-risk and pay off the tail immediately, de-risking the L-effort M1 that ultimately flips all five. Phase 2 is small and independent — run it in a parallel worktree.

---

## 3. Dishonest-win & stale-website items to correct (integrity — blocking)

### 3a. Harness dishonesty (must fix before publishing any new scorecard)
- **[HIGH] Read WINs are a query-cache mirage.** Static read bodies + `dataset_version` frozen during the zero-write read phase ⇒ calls 2..N are `took_ms=0` cache clones (index.rs:4504/4536) while ES really executes. Smoking gun: XERJ size:10 p50 is a flat 1.33–1.46 ms across wildcard/regexp/boosting/function_score/pinned while ES shows a real spread (9.10/34.42/39.25); `search_after` (the one `cache_eligible=false` family) is 3× slower. **Fix = H1.** Until then, **do not publish static-body read WINs.**
- **[MED] Reads scored on p50 only** = deterministically the cache-hit value for a 1-miss/135-hit distribution. **Fix = H1/H2** (report p50 **and** p99 uncached).
- **[MED] Mixed is not iso-write-rate** — uncapped `curl` writer loop; XERJ endures more merge pressure. **Fix = H3.**
- **[MED] Correctness checks only `aggregations[0]`** with 10% hit tolerance → a silently no-op'd pipeline agg can still score a latency WIN. **Fix = H4.**
- **[LOW] Ingest WIN doesn't equalize durability** (ES defaults `translog.durability=request` fsync-per-bulk). Pin durability equally or footnote.
- **[LOW] Stale sibling harnesses** overwrite canonical files with undici/closed-loop numbers and a :9200-as-ES bug. **Fix = H5.**

### 3b. Website staleness (landing/**/*.html vs Jul-6 SCORECARD)
- **[HIGH] Remove "89× faster NGINX terms agg"** — **zero** backing measurement anywhere in the repo (fabricated). Files: `landing/solutions/index.html:136`, `landing/resources/index.html:138`. Real terms-agg = **1.15×**.
- **[HIGH] Remove/re-measure "74× SIEM terms agg — 0.4 ms vs 29.8 ms"** — stale April v0.6.0 (`FEATURE_FAIRNESS_REVIEW_v0.6.0_2026-04-25.md:267`, "not re-run"), measured under the *repudiated* cache-mirage methodology (`2ff3e9a`), and **contradicted by the same site's own 1.15×** row. Appears in 30+ spots incl. `landing/docs/index.html:115`, `landing/industries/{index,finserv,public-sector}.html`, `landing/solutions/index.html:72`, `landing/docs/playbooks/siem.html:105`, `landing/docs/aggregations.html:121`, **plus a shared docs footer + JSON search-index snippet duplicated across ~30 `landing/docs/*.html`** (quickstart, storage, config, api-native, vectors, ingest, compression…). Fix the shared footer + docs-index JSON **once**.
- **[MED] Legacy envelope battery (300× cold start / 21× less memory / 56× binary)** — April v0.6.0 data, none in SCORECARD, current build is v1.0.0-rc.1. Files: `landing/industries/public-sector.html:233-235`, `finserv.html:78`, `solutions/index.html:115`, `resources/index.html:131`. **Re-confirm on rc.1 or date-stamp "measured 2026-04, v0.6.0".**
- **[MED] "2.8× less disk vs ES"** contradicts measured **1.20×** (XERJ 672.5 MB / ES 806.7 MB). 2.8× is the *internal* raw-input compression ratio, not vs ES. `landing/solutions/index.html:137`. **Correct to 1.20× or relabel as raw compression.**
- **[MED] Vector/hybrid claims** ("1B vectors · 38 ms hybrid p95", "1.2 ms vs 45 ms ES+Pinecone", "AML p95 0.4 ms vs 29.8 ms" which **relabels the SIEM terms-agg pair as vector retrieval**) — no backing benchmark; only kNN backing is SCORECARD `kNN k=10` 0.78 ms p50, 3.39×, recall 100%. Files: `landing/industries/{finserv,healthcare}.html`. **Verify or remove; stop relabeling terms-agg as vector retrieval.**
- **[LOW] `demo/index.html` "6,008,939 docs/s"** is an April in-memtable peak on a different (LabSZ) corpus; sustained scorecard ingest is 119k–388k docs/s. **Clarify labeling (peak in-memtable, April, LabSZ).**
- **Baseline (NO ACTION):** `use-cases.html`, `use-cases/*.html`, `benchmarks/index.html`, `product.html`, `docs/recipes/migrate-from-elasticsearch.html` are faithful row-for-row copies of the scorecard and honestly disclose the 5 mixed LOSSES — **use them as the correction template.**

---

## 4. Re-measurement protocol (QUIET box) to prove all-wins

Run **only after** Phase 0 (fair harness) lands, and after each engine fix, on an isolated machine.

**Environment**
- Dedicated, otherwise-idle box; CPU governor `performance`; disable other services. **Both engines on the same box, benchmarked sequentially (never concurrently)** except the mixed phase which is intra-engine read+write.
- **XERJ = current HEAD `--release` build** (`cargo build --release -p xerj-engine`), served on `:9200`. **Real Elasticsearch on `:9201`** (a genuine ES node — NOT the `bench.mjs` :9200-as-ES bug). Record both versions in the scorecard header.
- **Durability parity:** `index.translog.durability=request` on ES (its default) and confirm XERJ WAL fsyncs per `_bulk` equivalently — or footnote the difference (§3a low).

**Read families (cache-fair)**
- **Uncached:** vary a param per iteration so every call misses XERJ `query_cache` (novel term/id/range value per iter). Force-merge both engines to **1 segment** before the read phase for determinism; `track_total_hits:true` (exact totals already forced).
- **iters ≥ 1000, warmup ≥ 50.** Report **p50 AND p99**. Apply the H2 noise-band (TIE within `max(0.30 ms, 20%)`), and print a **trimmed-min** alongside p50 to expose the true server floor.

**Mixed read-under-write (cells 4–8)**
- Keep open-loop (`iters:3000, warmup:20, rate:300`, bench-matrix.mjs:497) **but throttle the `_bulk` writer to a fixed target docs/s both engines sustain**; log offered-vs-achieved per engine. Reject the run if achieved rates diverge >10%.
- Report p99 (primary) + max. Confirm via a repeat that stalls no longer snowball after load stops (the M2 signature).

**Correctness gates (block the timing if any fail)**
- ES-YAML compatibility **1360/0** green (hard gate for M1/M3/M4).
- H4 full-aggregation-subtree signal match + `hits.total` exact (tolerance 0).
- Agg A/B: `XERJ_DISABLE_FAST_AGGS=1` vs fast path byte-identical on filter/cardinality/date_histogram.
- For sort/size>0 changes: assert `hits.total` correctness on a **>materialisation_limit (~256)** sorted result set, not just latency (§5).

**Deletes-present family (H6)**
- Load `/perf` append-only, snapshot p50 for `term(status)`, `range(cost_usd)`, sort-heavy, `search_after`; then inject **one DELETE/UPDATE** (bumps `ghost_events`) and re-measure the SAME cells. A step-change to O(N) exposes the sticky-gate downgrade the append-only scorecard hides.

**Verdict**
- Run the full matrix **3×**; require **stable ordering** (no bimodal label flips) before declaring a win.
- **Pass = every cell ratio ≥ 1.0, or TIE within the noise-band; zero LOSE**, on the honest (uncached, iso-write-rate) harness. Regenerate SCORECARD.md and reconcile the website (§3b) to the new numbers in the same PR.

---

## 5. Open risks

**Engine-fix risks**
- **M1 hot-path (high):** epoch/ArcSwap consistency vs `remove()` deletes and `drain_shard_inner`'s `std::mem::take` flush drain must invalidate cleanly; transient double-buffer memory; **must not add a per-doc snapshot rebuild** (historically regressed ingest ~4×). Gate: ES-YAML 1360/0 + ingest throughput + auto-id mixed repro.
- **M2 load-shedding:** shed/timeout must return `timed_out:true` (ES-compatible) and not starve aggs; dedicated pool sizing needs the mixed repro as a behavior gate.
- **M4 sorted numeric index:** adds ingest-side cost — must not regress the beat-ES ingest win; exact broad-range `hits.total` must stay correct (delete-aware live count); A/B a >256-match memtable range.
- **M5 flush cadence = gaming risk:** never tune to the test window; principled production default traded against memory/recovery/segment fan-out only.
- **F1/F2 drift:** duplicated fold logic can diverge from brute (null handling, `f64::from_bits` decode, value_count presence) — one shared `#[inline]` fn + equality unit test.

**Measurement risk (the big one)**
- **H1 may reveal NEW losses among the current 81 WINs.** The static-body read wins are cache clones; the true **uncached** p50/p99 is unknown until measured. The "all-wins" target must be re-baselined against honest numbers, and some read cells (wildcard/regexp/boosting/function_score/pinned, which sit at a suspicious flat 1.33–1.46 ms today) may need their own engine work. **Budget for this.**

**Regression risks already in the tree (append-only scorecard hides them)**
- **[HIGH — verify shipping binary] `a93354a` arithmetic delete-gate** caused a real append-only regression (2 dup docs/merge pinned the gate ON → every size>0 term/range/bool to O(N)). HEAD `2022a3b` uses the `ghost_events` signal (index.rs:5695 — **confirmed**). A/B `a93354a` (arithmetic) vs HEAD on mixed range/terms/match_all + static term(status)/range(cost_usd)/bool to prove the fix and quantify what was avoided.
- **[MED] Sticky `ghost_events` gate.** `ghost_events` is monotonic `fetch_add`, never reset by merge/flush (version_map.rs:167/268/287/315). **One** update/delete pins `deletes_present=true` forever → forces size>0 term/range/bool hits.total back to O(N) delete-aware count (index.rs:5773) **and** disables sort-candidate prefilters (index.rs:6101/6135). Bench hides it (append-only auto-id `{"index":{}}`), but any real workload or any demo playbook that deletes/updates before reads permanently downgrades. **Mitigation:** H6 deletes-present family; consider clearing/re-deriving the ghost signal after a full merge.
- **[MED] `b79a8c2` unconditional `try_shortcut_count`** on every size>0 query (index.rs:5598) — wasted doc-values/FST work on selective and deletes-present reads; absolute regression below pre-F1 baseline when `deletes_present` (full scan **plus** unused shortcut). A/B `b79a8c2^` vs `b79a8c2` on selective (`ids`, `term(cache_hit)`) vs non-selective (`range(cost_usd)`, `term(status)`) + mixed range (cold-segment shortcut) — report **p99**, the effect hides in the tail.
- **[MED] `b79a8c2` early-break in scan order** (index.rs:5801) can drop true top-N from later segments on field/_score-sorted size>0 — the reason `fix/sort-topn-correctness` exists. HEAD compensates with `sort_topk` + `build_sort_candidates_prefilter` (index.rs:6100–6155), which itself already produced a "512-vs-847306" hits.total regression once (in-code note 6176–6180) and silently disables under `deletes_present`. **A/B must assert hits.total (not just latency)** on sort-heavy / search_after / range(@timestamp) / deep from+size, incl. a deletes-present variant.
- **[LOW] `2ff3e9a` is docs-only** (one new .md, 0 code) — exclude from A/B.

**Integrity/scope**
- Website corrections (§3b) touch ~30 duplicated docs pages via a shared footer/search-index — fix once at the source template, verify no page still renders 74×/89×/2.8×.
- Every commit on `xerj-org/xerj` must be authored as **xerj-org** (per memory) — no other author identities anywhere.
