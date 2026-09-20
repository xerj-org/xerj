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

<!--RESULTS-->

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

# 2. the public BEIR datasets
for d in scifact nfcorpus fiqa; do
  curl -LO https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets/$d.zip && unzip -q $d.zip
done

# 3. index, and take the first-stage candidate lists (BM25 and hybrid RRF top-100)
python3 load.py        ./scifact scifact
python3 first_stage.py ./scifact scifact test  runs/scifact.test.first_stage.json
python3 first_stage.py ./scifact scifact train runs/scifact.fit.first_stage.json 150
python3 make_pairs.py  ./scifact runs/scifact.test.first_stage.json runs/scifact.test

# 4. score the pairs (resumable; `--tier small|base|large`)
cargo build --release -p xerj-ai --features neural --example pair_score
RUNS=runs PAIR_SCORE=../../engine/target/release/examples/pair_score \
  ./score_parallel.sh 8 4 small scifact.fit.w30 scifact.test.w30 scifact.test.rest

# 5. the tables, the intervals and the calibration
python3 eval.py ./beir runs results/ndcg.json

# 6. the latency sweep — on an IDLE machine, one process
python3 latency.py runs ../../engine/target/release/examples/pair_score \
  scifact results/latency.json 25 small,base 10,20,30,50,100
```

`first_stage.py` is also how the `#940` repeats are taken: `ARMS=hybrid` re-runs only the
hybrid arm, and each repeat is a **fresh node process** over unchanged indices.

Raw output: [`results/`](./results).
