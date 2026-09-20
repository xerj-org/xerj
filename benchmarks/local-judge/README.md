# The local judge on BEIR: does an in-process cross-encoder beat what XERJ already ships?

**Run:** 2026-09-20, `xerj v1.0.0-rc.74` (branch `feat/local-judge`). One node, one
shard, default settings, `--embed-mode neural` (built-in Candle BERT,
`sentence-transformers/all-MiniLM-L6-v2`, 384-dim, CPU) — so the hybrid arm is a real
hybrid and not the default lexical feature-hashing embedder. Metric: nDCG@10 on each
dataset's BEIR `test` split, computed by [`eval.py`](./eval.py) from live queries against
a running node. Hardware: one x86-64 host, 32 cores, 119 GB RAM, no GPU.

**The question this run had to answer.** XERJ's shipped best arm is hybrid RRF, at
nDCG@10 **0.699 SciFact / 0.345 NFCorpus** (`benchmarks/beir-hybrid`). The local
cross-encoder only deserves to be a *default* if it beats that by more than the
harness's own run-to-run spread. It does not. It ships opt-in.

## Results

nDCG@10, BEIR `test` split. Raw: [`results/ndcg.txt`](./results/ndcg.txt) ·
[`results/ndcg.json`](./results/ndcg.json).

| Arm | SciFact<br>300 q, 5,183 docs | NFCorpus<br>323 q, 3,633 docs | FiQA<br>648 q, 57,638 docs |
|---|---:|---:|---:|
| BM25 (`multi_match` on title, text) | 0.6572 | 0.3016 | 0.2382 |
| BM25 top-30 → local `small` | 0.6824 | 0.3370 | 0.3160 |
| BM25 top-100 → local `small` | 0.6792 | 0.3420 | 0.3337 |
| BM25 top-30 → local `base` | 0.7084 | 0.3141 | not run |
| **Hybrid RRF — what ships** | **0.7021** | **0.3445** | **0.3572** |
| Hybrid top-30 → local `small` | 0.6936 | **0.3597** | **0.3751** |
| Hybrid top-100 → local `small` | 0.6872 | 0.3575 | 0.3697 |
| Hybrid top-30 → local `base` | **0.7186** | 0.3328 | not run |

Difference from hybrid RRF on the same queries, 95% paired bootstrap, 2,000
resamples:

| Arm | SciFact | NFCorpus | FiQA |
|---|---|---|---|
| Hybrid top-30 → `small` | −0.0085 `[−0.0367, +0.0201]` | **+0.0152** `[+0.0047, +0.0268]` | **+0.0179** `[+0.0025, +0.0322]` |
| Hybrid top-100 → `small` | −0.0150 `[−0.0450, +0.0163]` | **+0.0129** `[+0.0019, +0.0243]` | +0.0125 `[−0.0031, +0.0276]` |
| Hybrid top-30 → `base` | +0.0165 `[−0.0093, +0.0425]` | −0.0117 `[−0.0246, +0.0013]` | not run |

### What it says

1. **No tier beats hybrid RRF on every dataset, and the tiers disagree about
   which one they help.** `small` gains on NFCorpus and FiQA (intervals
   excluding zero) and is a wash on SciFact. `base` is the opposite: the best
   SciFact arm measured, and *worse than the smaller model* on NFCorpus. A
   bigger cross-encoder is not uniformly a better one.
2. **Over a BM25-only first stage it is a large, unambiguous win** — +0.0253,
   +0.0354 and +0.0777 with `small`, +0.0512 on SciFact with `base`. On FiQA it
   closes two thirds of the BM25→hybrid gap with no vectors indexed at all.
   That is the case the provider ships for.
3. **So it ships opt-in, not as a default**, and the two non-quality reasons
   matter as much as the numbers: one 30-document call costs seconds of CPU
   (`results/latency.txt`), and no tier's score is calibrated.

### The noise floor

Issue [#940](https://github.com/xerj-org/xerj/issues/940) orders tied RRF scores
by a per-process seed, so the hybrid baseline moves between node restarts. Three
fresh node processes over unchanged indices:

| | run 1 | run 2 | run 3 | spread |
|---|---:|---:|---:|---:|
| SciFact hybrid RRF | 0.7021 | 0.7034 | 0.7019 | 0.0015 |
| NFCorpus hybrid RRF | 0.3445 | 0.3436 | 0.3438 | 0.0009 |

With the `benchmarks/neural-path-triage` triple (0.6993 / 0.7023 / 0.7044) the
SciFact band over all six recorded runs is **0.0051**; NFCorpus's is 0.0012.
Those are the floors used above.

Two facts from the same three runs make the reranked numbers comparable at all:

- the candidate **set** was identical on **100%** of queries — only tie order
  moved — while the top-10 order was identical on 44% (SciFact) and 50%
  (NFCorpus);
- so every reranked arm scored **identically across all three processes, to
  four decimal places** (spread 0.0000). `eval.py` reports this as
  `rerank_repeats`, so it is checked rather than argued.

### Calibration: one fit diverged, the other is not shipped

Fitted on a **different split** (SciFact `train`, NFCorpus `dev`, FiQA `dev`),
over the BM25 top-30 window, and reported on `test`.

| Tier | Fit | Pooled test ECE raw → Platt → temperature |
|---|---|---|
| `small` | **diverged** — maximum-likelihood slopes of 10^9–10^11 (near-separable classes in one feature) | 0.1444 → 0.1380 → 0.1667 |
| `base` | converged — Platt `scale 0.4331, bias −0.8912` | 0.0509 → **0.0094** → 0.0444 |

`small`'s "calibrated" map is not better than its raw sigmoid, and a slope of
10^11 is a step function that would destroy what resolution the raw score has.
`base`'s fit is a real 5.4× improvement — and still does not ship, because it
was fitted and evaluated on the **same two corpora** and there is no
out-of-domain check. Both tiers therefore carry `Calibration::NONE` and report
`score_kind: "relevance"`. The next step is concrete: score a third corpus with
`base`, fit on two and report ECE on the third.

### Latency

See [`results/latency.txt`](./results/latency.txt).

<!--LATENCY-->

### Against the hosted rerankers — read the caveat

**We did not run them.** The figures published in
[`hev/jev-rerank`](https://github.com/hev/jev-rerank)'s README rerank that
project's own first-stage shortlist, not ours: same datasets and metric, not a
controlled comparison.

| | SciFact | NFCorpus | Run by |
|---|---:|---:|---|
| Jev (TypeSafe AI) | 0.768 | 0.358 | their README |
| Voyage rerank-3 | 0.755 | 0.357 | their README |
| Cohere rerank-v3.5 | 0.745 | 0.340 | their README |
| **XERJ hybrid RRF + local `base`, no API** | **0.7186** | 0.3328 | us |
| **XERJ hybrid RRF + local `small`, no API** | 0.6936 | **0.3597** | us |
| XERJ hybrid RRF alone | 0.7021 | 0.3445 | us |

On NFCorpus a free, keyless, egress-free local judge is **above** every
published hosted figure in that table. On SciFact the best local arm is 0.049
below Jev's published number. Neither statement is a controlled comparison.


## Method

Four things this harness does on purpose, because each one is a way the same
measurement is usually got wrong.

**Every arm on a table is computed on the same queries.** BM25 and hybrid included. An
arm is reported only when *every* pair it needs was scored; otherwise it is `null`,
never computed on whatever happened to finish. That is what makes the paired bootstrap
intervals meaningful.

**Calibration is fitted on a different split from the one it is reported on** — SciFact
`train`, NFCorpus `dev`, FiQA `dev` — over the BM25 top-30 window, which is the
population a default `"rerank": {"provider": "local"}` actually judges.

**The scorer is the shipped code path.** [`pair_score`](../../engine/crates/xerj-ai/examples/pair_score.rs)
runs pairs through `xerj_ai::judge::JudgeRuntime` — the same tokenizer settings, the
same length-aware batching, the same thread pool the server scores with — and the
document side is built by the same `title \n text` concatenation, with the same clip,
that `xerj_rerank::local::document_text` uses. A number here is a number about the code
that ships.

**Latency is never taken from a throughput run.** [`latency.py`](./latency.py) runs one
process on an otherwise-idle machine and records the load average before and after each
sweep point. Bulk scoring is done by [`score_parallel.sh`](./score_parallel.sh), several
narrow processes over query-group shards, and its timings are not quoted anywhere.

## Reproduce

```sh
# 1. a throwaway node on private ports, neural embedder
xerj -c xerj.toml -d ./data --insecure --embed-mode neural &   # es_compat_port = 12600
export XERJ_URL=http://127.0.0.1:12600

# 2. the public BEIR datasets, all three under one root (`eval.py` reads <root>/<dataset>)
mkdir -p beir runs && cd beir
for d in scifact nfcorpus fiqa; do
  curl -LO https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets/$d.zip && unzip -q $d.zip
done
cd ..

# 3. index, and take the first-stage candidate lists (BM25 and hybrid RRF top-100)
python3 load.py        ./beir/scifact scifact 6
python3 first_stage.py ./beir/scifact scifact test  runs/scifact.test.first_stage.json
python3 first_stage.py ./beir/scifact scifact train runs/scifact.fit.first_stage.json 150
python3 make_pairs.py  ./beir/scifact runs/scifact.test.first_stage.json runs/scifact.test

# 4. score the pairs (resumable; `--tier small|base|large`)
cargo build --release -p xerj-ai --features neural --example pair_score
RUNS=runs PAIR_SCORE=../../engine/target/release/examples/pair_score \
  ./score_parallel.sh 8 4 small scifact.fit.w30 scifact.test.w30 scifact.test.rest

# 5. the tables, the intervals and the calibration
python3 eval.py ./beir runs results/ndcg.json | tee results/ndcg.txt

# 5b. the #940 repeats: re-run ONLY the hybrid arm from a FRESH node process, twice
ARMS=hybrid python3 first_stage.py ./beir/scifact scifact test runs/scifact.test.hybrid.rep1.json

# 5c. a slow tier on a seeded query subset, paired against every other arm on the same queries.
#     `hyb30` = the hybrid top-30 documents that are NOT already in the BM25 top-30, which is
#     exactly what `make_pairs.py` writes as `.rest` when you ask for the 30-document window
#     only. Scoring those two files covers both top-30 arms without paying for the top-100 ones.
python3 make_pairs.py ./beir/scifact runs/scifact.test.first_stage.json runs/tmp30 4000 30
mv runs/tmp30.rest.jsonl runs/scifact.test.hyb30.jsonl && rm runs/tmp30.w30.jsonl
python3 subset.py runs scifact 120 7 test w30,hyb30

# 6. the latency sweep — on an IDLE machine, one process
python3 latency.py runs ../../engine/target/release/examples/pair_score \
  scifact results/latency.json 25 small,base 10,20,30,50,100
```

`first_stage.py` is also how the `#940` repeats are taken: `ARMS=hybrid` re-runs only the
hybrid arm, and each repeat is a **fresh node process** over unchanged indices.

Raw output: [`results/`](./results).
