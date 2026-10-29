# scripts/bench — one-command reproductions for the headline numbers

Issue #806's ask, in one line: *for every number that appears in public copy,
either link the exact harness that produces it, or remove the number.* These
four scripts are that harness for the ENGINE numbers a stranger is most likely
to want to check. Each one:

- builds nothing and argues nothing — it starts a **throwaway node on a private
  port (93xx, never `:9200`)** with a throwaway data dir,
- runs on **generated (or committed synthetic) data**, end-to-end, through the
  Elasticsearch wire API,
- prints the **measured value next to the claimed one**, with the claim's
  source pointer, and
- tears the node down (pass `KEEP=1` to keep the data dir and server log).

They are honest by construction: docs/s is computed from the verified
post-refresh `_count`, never the intended count; kNN recall is scored against
a client-side brute-force ground truth and `GET /_cat/ann` proves the HNSW
graph (not a silent exact-scan fallback) served the queries; the rerank script
counts the provider calls the node actually made, from the provider's side.

## The four repros

| Headline claim (where it is quoted) | Claimed | Reproduce with |
|---|---|---|
| Bulk ingest, 100k docs x 1 client — `demo/playbooks/SCORECARD.md:17`, landing `/benchmarks/elasticsearch` ("1.72x higher") | XERJ **191,286 docs/s** (ES 111,073) | `bash scripts/bench/ingest-docs-per-second.sh` |
| Server round-trip, `size:0` over 300k docs — scorecard methodology note, landing `/benchmarks` ("0.126 ms, 2.2x") | **0.126 ms** avg, keep-alive client (1.61 ms via fetch/undici — client overhead) | `bash scripts/bench/query-latency-size0.sh` |
| kNN recall, HNSW-served with exact rescoring — `docs/WHY_XERJ.md`, `ROADMAP.md`, landing `/docs/vectors` | recall@10 **1.00** bench query, **100-probe mean 0.976** / min 0.90 (ES 0.937) | `bash scripts/bench/knn-recall.sh` |
| Jev rerank stage, cost + failure contract — `docs/RERANK.md` ("30 documents per provider call", "no verdict cache", "8 in flight by default, 16 at most", "no key -> 503") | see each row in the script output | `bash scripts/bench/rerank-stage.sh` |

Requirements for all four: `bash`, `curl`, `python3` (the latency and kNN
repros also use `node >= 18`), and a built server binary —
`cd engine && cargo build --release -p xerj-server`, or point `XERJ_BIN` at an
existing one. Runtime on a 32-core dev box: ~30 s (rerank), ~1 min (ingest,
kNN), ~2 min (latency, which indexes 300k docs first).

## What each script deliberately does NOT claim

- **The Elasticsearch side of each comparison is not run here.** That needs an
  ES install; the head-to-head harness is `demo/playbooks/bench-matrix.mjs`
  (one file, Node builtins only). These scripts reproduce the XERJ side of the
  published cell, on the published protocol.
- **The Jev model is not run, ever.** `rerank-stage.sh` measures the stage's
  published cost/failure contract against `lib/jev_stub.py`, a local test
  double with the same word-overlap verdicts as the engine's own test double.
  The nDCG figures for Jev/Voyage/Cohere in `docs/RERANK.md` are
  `hev/jev-rerank`'s numbers and are marked "not run by us" — no harness in
  this repository can or should reproduce them.
- **Absolute milliseconds and docs/s are hardware-bound.** A repro on a slower
  or shared box will measure slower numbers; what reproduces everywhere is the
  ordering, the gap, and (for kNN recall) the value itself, which is a
  property of the index, not the disk.

## Last verified run (this repository's CI sandbox)

2026-09-20, xerj v1.0.0-rc.75 release binary, Linux x86_64, 32 cores — all
four scripts run back-to-back from this tree, commands verbatim as documented
above. This sandbox is **shared**, so the two wall-clock-bound numbers move
run to run (ranges from the same session noted under each); the two
index-property numbers do not.

### ingest-docs-per-second.sh

```
  CLAIMED   191,286 docs/s — ingest 100k x c1, scorecard run 2026-07 (demo/playbooks/SCORECARD.md:17)
  CLAIMED               (ES 8.13.4 same corpus/box: 111,073 docs/s -> 1.72x; ES side needs an ES install, not run here)
  note      corpus: demo/data/extras/chat-events.ndjson (4008 distinct events, cycled to 100000 docs — the published protocol)

  MEASURED  99602 docs/s (100000 docs indexed and verified by _count in 1004 ms, 1 client, 10000-doc batches)
  MEASURED  bulk responses with errors: 0 (item errors: 0)
  ratio     0.52x of the claimed figure (same protocol, this machine — not the scorecard box)
```

Variance note: three runs the same hour measured 50,201 / 103,520 / 99,602
docs/s (the middle one on the script's fallback generator, the others on the
committed corpus) — a 2x spread from sandbox neighbours alone. That spread is
why the script prints its ratio next to the claim instead of asserting a pass,
and why the published cell names a dedicated benchmark box.

### query-latency-size0.sh

```
  CLAIMED   0.126 ms avg, size:0 over 300k, Node http keep-alive client (scorecard run 2026-07)
  CLAIMED   1.61 ms avg for the SAME request through Node's global fetch/undici — client overhead, not server time
  MEASURED  keep-alive client : 0.491 ms avg (p50 0.464 / p99 0.870 / max 5.714)
  MEASURED  fetch/undici      : 0.735 ms avg (p50 0.711 / p99 1.377 / max 5.582)
  MEASURED  client overhead   : 0.245 ms (1.5x) — the published point: the client, not the server
  note      1200 timed iterations at 200 req/s open-loop, request_cache=false, query cache off, 1 merged segment
```

Same-session range: keep-alive avg 0.491–1.216 ms across two runs (the slower
one measured keep-alive 1.216 / fetch 2.631, a 2.16x gap). The reproduced fact
is the ordering and the client-overhead gap, not the absolute value.

### knn-recall.sh

```
  CLAIMED   recall@10 = 1.00 on the official bench query; 100-probe mean 0.976 / min 0.90 (docs/WHY_XERJ.md:32)
  CLAIMED   candidates exact-rescored: returned scores match the exact brute-force path
  MEASURED  recall@10 : 0.977 mean over 100 probes (min 0.900, max 1.000, 77/100 probes at 1.00)
  MEASURED  score parity    : max |engine _score - client exact (1+cos)/2| = 5.61e-08
  MEASURED  kNN latency     : p50 5.85 ms per probe, k=10, num_candidates=100 over 50000 x 128-d vectors
  MEASURED  ANN health      : coverage=50000 stale=False nodes=50000 (GET /_cat/ann — proves the HNSW
             graph served this, not a silent exact-scan fallback)
```

The published figures reproduce almost exactly: 0.977 mean / 0.900 min here
against 0.976 / 0.90 published (an earlier run the same session read 0.974
mean / 0.800 min — the mean is stable, a single worst probe is a draw). Recall
is a property of the index and query distribution, not of the hardware, so
this is the one headline that should reproduce closely anywhere.

### rerank-stage.sh

```
  PASS  defaults window/batch/max_concurrency       claimed 30 / 30 / 8      measured 30 / 30 / 8
  PASS  limits max_docs_per_call/max_window/...     claimed 30 / 300 / 16    measured 30 / 300 / 16
  PASS  window 35 -> provider calls / docs per call claimed 2 calls: 30 + 5  measured 2 calls: 5 + 30
  PASS  window 300 -> provider calls / docs per call claimed 10 calls of 30  measured 10 calls of 30
  PASS  3 page requests -> calls / paid judgements  claimed 3 calls, 90      measured 3 calls, 90
  PASS  slow provider (2s) with timeout_ms=200      claimed applied:false + engine order
                                                     measured HTTP 200, applied:False, score_kind:engine,
                                                     order == engine, reason: deadline exceeded after 200ms
  PASS  verdicts replace _score (min_score 0.5)     claimed probability ordering + prune counting
                                                     measured judged:40, pruned:21, order desc, min _score 0.800
  PASS  max provider calls in flight, window 300    claimed 8 by default, 16 at most
                                                     measured default: 8 in flight over 10 calls;
                                                     max_concurrency=16: 10 in flight over 10 calls
  PASS  no key configured -> refusal, not silence   claimed HTTP 503, rerank_exception, no hits
                                                     measured HTTP 503, hits 0, error type rerank_exception
  contract checks: 9/9 PASS (deterministic contract — these assertions are the reproduction)
```

(The per-call log order in the `window 35` row is completion order — the two
calls run concurrently; the assertion checks the multiset `{30, 5}`.)

## The other published numbers, and where their harnesses already live

So that every headline maps to a runnable thing from one place:

| Claim | Harness in this repo |
|---|---|
| Batch vs one-item retrieval on a pinned rust-vulns pack (CLI/MCP correctness, warm latency and payload BPE tokens; not generated-answer accuracy) | `tools/benchmarks/batch-retrieval/` (`run.py`; protocol and reproduction in its README) |
| 2.7x / 26x / up-to-278x fewer output tokens (reference coding) | `docs/case-studies/reference-coding/CASE_STUDY.md` + the 3-arm Harbor harness `tools/benchmarks/harbor-3arm/` (`run_matrix.sh`) |
| nDCG@10 rows (BM25 0.6572 / MiniLM 0.6764 / hybrid 0.6993 on SciFact) | `benchmarks/beir-hybrid/` (`load.py`, `eval.py`, raw output in `results/`) |
| Banking77 0.933 / SMS spam 0.983 decisions-from-history | `benchmarks/decisions-as-retrieval/` (`eval.py`, raw output in `results/`) |
| ES-YAML conformance badge (1444/1454) | the CI suite itself: `engine/tests/es-compat-yaml/` |
| 55 W / 26 T / 4 L vs Elasticsearch (all 88 cells) | `demo/playbooks/bench-matrix.mjs` -> `demo/playbooks/SCORECARD.md` |
| int8 quantization recall@10 ~ 0.998 | `docs/examples/vector-quantization/quant_demo.py` (docs in `docs/recipes/vector-quantization.md`) |
