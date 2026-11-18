# Latency, like for like: where the seconds go

Head-to-head set: 160 questions over 5,000 records in 95 collections (the published XERJ Query-1 comparison). Measured 2026-11-06 on this laptop unless a row says otherwise. Every number comes from a file listed at the end; derived numbers are marked ARITHMETIC. Components are medians of each part, so a row's parts need not add up exactly to its end-to-end median.

## What the data says

1. **Retrieval is fast.** XERJ BM25 over the 5,000 records answers a query in 0.81 ms at the median on this laptop. The retrieval steps of the RAG arms, re-run here on prompts that came out byte-identical to the ones the LLM was given (160/160 for every arm), take 2.0 ms (BM25), 16–18 ms (bge-small, one query at a time) and 106 ms (GraphRAG local context), measured while other jobs loaded the machine; the 11-04 run on the same laptop measured 0.747 ms, 4.15 ms (queries encoded in one batch) and 96.0 ms.
2. **RAG end-to-end takes seconds because answer quality required an LLM to write the answer.** In the head-to-head every RAG and GraphRAG arm handed its retrieved records to Opus 5.5, which wrote the answer. Per question, at the median: Opus API time 1,532–1,648 ms, `claude -p` process start-up and preparation 1,045–1,310 ms, retrieval at most 106 ms; end to end 2.65–2.92 s. The whole-store Opus arm has no retrieval and took 3.31 s, of which 2,274 ms API. Those are the published 2.7 to 3.3 s. A one-word answer through the same path took 1,905 ms today (938 ms API + 972 ms CLI): 70% of the BM25-10 arm's 2,737 ms median is the fixed cost of one `claude -p` call (ARITHMETIC).
3. **Like for like, XERJ Query-1 on the same laptop: 219–226 ms p50, 361–412 ms p95** on the same 160 questions, released rc.101 x86_64 binary, shipped bundle, default config (4 model threads). Its answers are identical to the published our ARM build host run on 160 of 160 questions, and the frozen scorer on our ARM build host gives the same result (124 right, 1 wrong, 35 declined, Score 96.06). With other jobs loading the machine the median rose to 500 ms; the first question after a restart waits for the model load: 3,249–3,303 ms, 5,827 ms under heavy foreign load.
4. **The two published XERJ numbers are not this measurement.** "1,039 ms" is the same workload on four 2.8 GHz efficiency cores of a GB10 box that was training a model at the time (4,131 CPU-ms per question vs 892 here). "120 ms" is a different bundle (int8-guard, never shipped; the released bundle serves w8, and the node reports `int8_mode: w8`), a dev build, and a different workload: one namespace of 8,000 documents (40,000 facts, ARITHMETIC from bench_sys.py; the post says "up to 8,000 facts") and 100 copies of one templated lookup question, on which the w8 serving mode (the release default) ran at 163 ms with that bundle, against 219–226 ms for w8 on the head-to-head here. The published line "120 ms vs 2,700–3,300 ms" therefore set an unshipped mode on an easier workload against LLM wall times that include about a second of CLI start-up.
5. **The reviewer's 108.51 ms** (hybrid search p50 on their knowledge base) is a retrieval-only number. On this set the comparable rows are the retrieval column: 0.81 ms (XERJ BM25) to 106 ms (GraphRAG context). The 2.7–3.3 s were never search latency.

## One table

| Arm | Retrieval (p50) | Answer generation (p50) | Overhead (p50) | End-to-end p50 / p95 | Hardware, date |
|---|---|---|---|---|---|
| XERJ BM25 `_search` over the 5,000 records (returns records, writes no answer) | 0.81 ms | none | inside the wall time | 0.81 / 0.99 ms | laptop, rc.101 release, default config, 2026-11-06 |
| **XERJ Query-1**, released rc.101 binary + shipped bundle, default config, same 160 questions | link stage 0.4 µs (the facts are already in the namespace) | the model writes the answer program: 61% of server time on the facts path; executor 43.5 µs (split measured under load) | HTTP 0.7 ms; counting stage (runs first on every question) + projection + admission: the other 39% | **219–226 / 361–412 ms** (quiet passes) | laptop AMD Ryzen AI Max+ 395, 4 model threads, 2026-11-06 |
| XERJ Query-1 as published: "1,039 ms" (same 160 questions, same bundle) | not split | not split | not split | 1,039 / 1,784 ms | our ARM build host GB10 aarch64, 4 × Cortex-A725 2.8 GHz efficiency cores, nice 15, FP16 training running, 2026-11-04 |
| XERJ Query-1 as published: "120 ms" (int8-guard, unshipped bundle, other workload) | not split | not split | not split | 120 / 176 ms | laptop, dev build, one namespace of 8,000 docs = 40,000 facts, 100 templated lookups, 2026-11-03 |
| Classic RAG: BM25 top-10, Opus 5.5 writes the answer | 2.0 ms (0.747 on 11-04) | 1,648 ms (Opus API) | 1,080 ms (claude -p) | 2,737 / 4,770 ms | laptop + Anthropic API via `claude -p`, 160 calls, 2026-11-04/05 |
| Classic RAG: BM25 top-5 + Opus 5.5 | 2.1 ms (0.828 on 11-04) | 1,561 ms (Opus API) | 1,310 ms (claude -p) | 2,922 / 5,403 ms | laptop + Anthropic API via `claude -p`, 160 calls, 2026-11-04/05 |
| Classic RAG: bge-small top-10 + Opus 5.5 | 16.0 ms (4.15 on 11-04) | 1,564 ms (Opus API) | 1,045 ms (claude -p) | 2,696 / 5,104 ms | laptop + Anthropic API via `claude -p`, 160 calls, 2026-11-04/05 |
| Classic RAG: bge-small top-5 + Opus 5.5 | 18.0 ms (4.15 on 11-04) | 1,532 ms (Opus API) | 1,107 ms (claude -p) | 2,686 / 4,939 ms | laptop + Anthropic API via `claude -p`, 160 calls, 2026-11-04/05 |
| GraphRAG local search + Opus 5.5 | 105.7 ms (96 on 11-04) | 1,552 ms (Opus API) | 1,125 ms (claude -p) | 2,650 / 5,118 ms | laptop + Anthropic API via `claude -p`, 160 calls, 2026-11-04/05 |
| Opus 5.5 reads the whole store, 1 question per call (20 q) | none (whole store pasted) | 2,274 ms (Opus API) | 1,039 ms (claude -p) | 3,311 / 6,612 ms | laptop + Anthropic API via `claude -p`, 20 calls, 2026-11-04/05 |
| Opus 5.5 reads the whole store, 40 questions per call | none (whole store pasted) | 30,760 ms (Opus API) | 740 ms (claude -p) | 31,543 / 41,272 ms per call | laptop + Anthropic API via `claude -p`, 4 calls, 2026-11-04/05 |
| `claude -p` floor: trivial prompt, 4-token answer, same flags | none | 938 ms (Opus API) | 972 ms (claude -p) | 1,905 / 2,103 ms | laptop + Anthropic API, 10 sequential calls, 2026-11-06 |

Retrieval for the RAG arms was computed before each call, so it is not inside the `claude -p` wall time; adding it changes the end-to-end medians by at most 106 ms (ARITHMETIC). The whole-store 40-per-call arm costs 789 ms per question when divided by 40 (ARITHMETIC), but no question gets its answer before the whole call ends.

## Where the LLM seconds go

| Arm | Calls (parallel) | Wall p50 | API p50 | First streamed frame p50 | Process start-up p50 | CLI prep p50 | Input tokens p50 | Output tokens p50 (thinking p50) |
|---|---|---|---|---|---|---|---|---|
| Classic RAG: BM25 top-10, Opus 5.5 writes the answer | 160 (8) | 2,737 | 1,648 | 1,050 | 950 | 128 | 4,424 | 96 (57) |
| Classic RAG: BM25 top-5 + Opus 5.5 | 160 (8) | 2,922 | 1,561 | 1,052 | 1,185 | 138 | 2,770 | 88 (50) |
| Classic RAG: bge-small top-10 + Opus 5.5 | 160 (8) | 2,696 | 1,564 | 1,060 | 895 | 148 | 4,150 | 100 (62) |
| Classic RAG: bge-small top-5 + Opus 5.5 | 160 (8) | 2,686 | 1,532 | 1,024 | 974 | 134 | 2,647 | 88 (49) |
| GraphRAG local search + Opus 5.5 | 160 (8) | 2,650 | 1,552 | 1,039 | 990 | 118 | 15,400 | 98 (60) |
| Opus 5.5 reads the whole store, 1 question per call (20 q) | 20 (4) | 3,311 | 2,274 | 1,627 | 849 | 186 | 172,870 | 97 (57) |
| Opus 5.5 reads the whole store, 40 questions per call | 4 (4) | 31,543 | 30,760 | 1,653 | 535 | 204 | 175,726 | 3,644 (2,154) |
| `claude -p` floor, trivial prompt | 10 (1) | 1,905 | 938 | 974 | 889 | 75 | 594 | 4 (0) |

All fields come from each call's own `claude -p --output-format json` record: wall = `_wall_s` (bwrap + process, measured by the wrapper), API = `duration_api_ms`, CLI prep = `duration_ms` − `duration_api_ms` (equals `time_to_request_ms` within ±72 ms on every call), process start-up = wall − `duration_ms`, first frame = `ttft_stream_ms`. Calls ran 8 at a time in the RAG and GraphRAG arms and 4 at a time in the whole-store arms (max concurrency from file times, ARITHMETIC); the trivial calls ran one at a time, with other jobs loading the laptop (load average 24.86 at the start). Network from this laptop to api.anthropic.com, 10 fresh HTTPS connections on 2026-11-06: TCP connect 152 ms, TLS done 303 ms, first response byte 462 ms (median). A service that keeps one process and one connection open would not pay the per-call start-up or handshake; that path needs an API key and was not measured here.

## XERJ Query-1 on the laptop, every pass

| Run | Pass | Foreign CPU load (cores) | p50 ms | p95 ms | Mean ms | Server CPU ms per question (mean) |
|---|---|---|---|---|---|---|
| default config | fresh_after_index | 4.72 | 225.4 | 412.2 | 234.9 | 918 |
| default config | pass1_after_restart | 11.14 | 252.0 | 503.4 | 289.7 | 1,077 |
| default config | pass2_warm | 1.26 | 218.7 | 361.2 | 226.8 | 892 |
| pinned to cores 0-3, XERJ_FACTS_THREADS=4 | fresh_after_index | 0.44 | 226.4 | 377.5 | 233.6 | 912 |
| pinned to cores 0-3, XERJ_FACTS_THREADS=4 | pass1_after_restart | 8.76 | 293.7 | 592.0 | 348.5 | 1,120 |
| pinned to cores 0-3, XERJ_FACTS_THREADS=4 | pass2_warm | 8.3 | 257.4 | 505.5 | 278.6 | 1,021 |
| default config, re-run (stage timings) | fresh_after_index | 20.91 | 498.5 | 877.0 | 522.8 | 2,038 |
| default config, re-run (stage timings) | pass1_after_restart | 20.37 | 490.0 | 827.2 | 543.7 | 2,037 |
| default config, re-run (stage timings) | pass2_warm | 20.77 | 500.5 | 776.6 | 510.5 | 1,991 |

Workload: the 160 per-question namespaces of the head-to-head (9,496 facts incl. edits, copied from our ARM build host without gold), written with `/_facts/{ns}/_bulk`, asked with `/_facts/{ns}/_ask` and `verify: decline_v2`, one question at a time over a keep-alive HTTP connection. "fresh" asks right after writing; "pass1_after_restart" asks after a server restart (namespaces rebuilt from the index on first touch); "pass2_warm" repeats. Writing all 160 namespaces took 1.8–3.0 s (5.6 s under heavy load); boot to ready 0.21–0.58 s; resident memory after the asks 1,523 MiB. Foreign load = CPU time of all other processes during the pass, in cores (this laptop was shared with other review jobs). Status counts are identical in every pass (82 ANSWER, 45 UNKNOWN, 25 UNSUPPORTED, 8 CONFLICT).

Inside one `_ask` (113 of 160 responses take the facts path and carry `timings_us`; the other 47 take the counting path, 569 ms handler p50, and report no stage split), pass2_warm of the re-run, medians: link 0.4 µs, manifest 88.5 µs, model 245.2 ms, executor 43.5 µs, verify 0.14 ms, facts-engine total 245.9 ms; the handler around it adds 157.3 ms (the counting stage runs first on every question: 113 of 113 facts-path responses carry its `counting` block; with projection access and admission it is not broken down further); handler 434.1 ms. By mean time on the facts path: model 61.3%, link + manifest + executor + verify 0.07%, handler around the engine 38.6%. This re-run happened under heavy foreign load (20.77 cores), so its absolute times are about twice the quiet ones; it is here for the split. Query-1's time is model inference (the facts model read 1,474 input tokens per question at the median); finding the facts costs microseconds.

## The published numbers, labelled

| Published | What it measured | Files | Host and config | Date |
|---|---|---|---|---|
| 120 ms p50 (176 p95), "on 4 CPU cores on a namespace of up to 8,000 facts" | int8-guard tau ×0.125, bundle `candidate_vA_i8g_t0.125` (not shipped), one namespace `benchmodel8000` = 8,000 synthetic airport docs × 5 facts = 40,000 facts (ARITHMETIC from the script), 100 templated region lookups, 518 CPU-ms per question; same run's w8 arm 163 ms | `xerj-r-dev/runs/gen/bench_sys_t0.125_N8000.json`, `xerj-r-dev/logs/e2e_t0.125_quiet2.log`, driver `runs/int8fix/e2e_i8g.sh`, bench `runs/gen/bench_sys.py`, server log `runs/int8fix/e2e_t0.125/server.log` | this laptop (server log: 119.2 GiB usable), dev build `xerj-r-dev/target/release/xerj` (reports v1.0.0-rc.100), taskset 0-11, 4 model threads, nice 15, quiet box | 2026-11-03 07:46–07:47 UTC |
| 1,039 ms p50 (1,784 p95) | the head-to-head itself: same 160 namespaces, bundle `candidate_a_s47_2026-11-04` (the shipped one, w8), 4,131 CPU-ms per question; restart pass 1,042, warm pass 1,038 | our ARM build host `~/blog_measure/xq/stats.json`, `latency.json` (byte-identical copies in `h2h-2026-11-05/raw/xq_stats.json`, `xq_latency.json`); scripts `xq_run.py`, `xq_lat.py` | our ARM build host GB10 aarch64, `taskset 10-13` = 4 × Cortex-A725 (CPU part 0xd87, 2.8 GHz), nice 15, rc.100 build of the rc.101 tree, FP16 training on the same box (loadavg 5.0–7.7) | 2026-11-04 23:55 – 11-05 00:06 CST |
| 2.7–3.3 s per question | `claude -p` wall time per call for the RAG/GraphRAG arms (p50 2.65–2.92 s) and whole-store Opus 1 q/call (3.31 s): Opus API time plus about 1 s of CLI start-up; retrieval excluded | `h2h-2026-11-05/raw/{rag,graphrag/query,longctx/single}/*.json`; extracted to `work/lat/llm_calls.json` | this laptop, bwrap-sandboxed `claude -p --model claude-opus-5-5`, 8 calls in parallel (4 for the whole-store arm), default effort | 2026-11-04 23:53 – 11-05 00:12 CST |

Live text checked: https://xerj.org/blog/xerj-query-ai-model, fetched 2026-11-06 08:42 UTC (HTTP 200): "120 ms vs 2,700–3,300 ms prompt to answer, XERJ Query-1 on 4 CPU cores on a namespace of up to 8,000 facts vs RAG / Opus 5.5" and "RAG and Opus pipelines took 2.7 to 3.3 s per question on the same set". The chart labels int8-guard "(served default)"; the released bundle's `bundle.json` says `"serve": "w8"`, ships no `int8_calib.json`, and the running node reports `int8_mode: w8`.

## Caveats

- The laptop was shared with other review jobs (embedding runs, other XERJ nodes) during these runs. Each Query-1 pass records the CPU the other processes used; the headline uses the passes with ≤5 foreign cores, and every pass is listed above. The RAG retrieval re-run happened under load average 10–29, so its milliseconds are higher than the 11-04 ones; both are shown.
- LLM latencies are the subscription CLI path, 8 calls in parallel (4 in the whole-store arms), from a laptop 152 ms (TCP connect) from the API. They are what the head-to-head measured, not the best a dedicated API client could do.
- Opus ran at default (medium) effort; its thinking tokens are inside the API time.
- XERJ Query-1 does not retrieve and then generate prose: it links the question to facts in the namespace and runs a program the model writes. Its "link" stage is the nearest thing to retrieval.
- The 160 namespaces hold 6–148 facts each (median 49); the 95 collections hold 6–146 records (median 45), 5,000 in all. Latency on one large namespace is the 120 ms workload's question, measured with a different bundle.

## Files

- Table data: `work-2026-11-06/measure/latency.json` (built by `scripts/lat_report.py`; this page by `scripts/lat_md.py`).
- LLM per-call extraction: `work/lat/llm_calls.json` (`scripts/lat_extract.py`) from `h2h-2026-11-05/raw/`.
- `claude -p` floor: `work/lat/trivial/t01..t10.json`, `run.log` (`scripts/lat_opus_trivial.sh`, `lat_trivial_run.sh`; 10 sandboxed calls, $0.0067 billed in total).
- Network: `work/lat/net_rtt.json`.
- XERJ Query-1: `work/lat/xq_default/result.json`, `work/lat/xq_pin4/result.json`, `work/lat/xq_default2/result.json` (`scripts/lat_xq.py`); answers `work/lat/xq_default/answers_pass1.json`, compared and scored on our ARM build host (`~/blog_measure/review_2026-11-06/`, `scripts/lat_host_parity.py`). Input `work/lat/worlds_nogold.jsonl` (sha256 8665fc20…, exported on our ARM build host by `scripts/lat_host_export.py`, no gold fields).
- Retrieval: `work/lat/retrieval.json` (`scripts/lat_retrieval.py`), `work/lat/xbm25/result.json` (`scripts/lat_xbm25.py`); 11-04 figures `h2h-2026-11-05/raw/rag_index_stats.json`, `gr_query_stats.json`.
- Binary and bundle: `release/xerj-1.0.0-rc.101-x86_64-unknown-linux-gnu.tar.gz` (sha256 999647a5…), `xerj-model-bundle-v1-v1.0.0-rc.101.tar.gz` (sha256 654b9b74…), both checked against their `.sha256`.
